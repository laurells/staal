// Packages (roadmap phase 4): libraries from outside the program's folder.
//
// A project is the folder of the entry file. If it holds `staal.json`, its
// `dependencies` name packages by git URL and exact commit, by a version range of a
// repository's release tags, or by a version range from the project's registry:
//
//   { "registry": "https://github.com/x/registry",
//     "dependencies": { "sms": { "git": "https://github.com/x/sms", "commit": "<40 hex>" },
//                       "email": { "git": "https://github.com/x/email", "version": "^1.2" },
//                       "chat": { "version": "~0.3" } } }
//
// `staalc fetch` resolves each to one commit, the newest version fitting every range
// (or the locked one while it fits), and locks it.
//
// A package is a git repository with `staal.json` at its root:
//
//   { "package": "sms", "version": "0.1.0",
//     "authority": { "net.http.post": ["https://api.ng.termii.com/api/sms/send"] },
//     "dependencies": { ... } }
//
// `staalc fetch` reads each commit's files straight from git's objects (no checkout,
// so no filters or line-ending conversion can change them), writes them to
// `.staal/packages/<name>/`, and records the SHA-256 of every file in `staal.lock`.
// Checking or running a program first verifies every package folder against the lock:
// a file changed, added or removed is refused before anything is read as code.
//
// Imports: in the program, `import sms.termii` means module `termii` of package `sms`
// when `sms` is a dependency. Inside a package, an import whose first name is one of
// that package's dependencies means that package; any other import means the
// package's own module (`import util` in package `sms` is the module `sms.util`).
//
// Authority (E108): a package's code may only mention capabilities its manifest lists.
// For a listed path with patterns, every `cap` of that path in the package must carry
// an `only` inside those patterns, which the runtime enforces when a capability is
// passed in. A capability a package declares must be named after the package.

use crate::ast::{Program, Stmt, Ty};
use crate::checker::Diagnostic;
use crate::lexer::Pos;
use crate::modules::{Source, SourceFile};
use crate::semver::{Range, Version};
use serde_json::{json, Map, Value as Json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MANIFEST: &str = "staal.json";
pub const LOCKFILE: &str = "staal.lock";
/// Where fetched packages live, under the project folder.
pub const STORE: &str = ".staal/packages";

/// Bounds on what one package may hold, so a hostile repository cannot exhaust memory.
const MAX_FILES: usize = 10_000;
const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Authority {
    /// Any target: the operator's grant is the only limit.
    Any,
    /// Every `cap` of this path in the package must have an `only` within these.
    Only(Vec<String>),
}

/// Where a package's files are: a git location and one commit there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dep {
    pub git: String,
    pub commit: String,
}

/// What a manifest asks for: one commit, a range of a repository's release tags, or a
/// range of the versions the project's registry lists.
#[derive(Debug, Clone, PartialEq)]
pub enum Spec {
    Commit(Dep),
    Tags { git: String, range: Range },
    Registry { range: Range },
}

impl Spec {
    fn describe(&self) -> String {
        match self {
            Spec::Commit(d) => format!("commit {} of {}", &d.commit[..12], d.git),
            Spec::Tags { git, range } => format!("version {range} of {git}"),
            Spec::Registry { range } => format!("version {range} from the registry"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub package: Option<String>,
    pub version: Option<Version>,
    pub authority: BTreeMap<String, Authority>,
    pub dependencies: BTreeMap<String, Spec>,
    /// The project's registry, a git location (a project's manifest only).
    pub registry: Option<String>,
}

/// One verified package: its manifest and its `.staal` files, read once and kept, so the
/// text that is checked is exactly the text that was verified.
#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    pub dep: Dep,
    pub manifest: Manifest,
    modules: BTreeMap<String, String>,
}

/// A project with packages, verified against its lockfile.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub deps: BTreeMap<String, Spec>,
    pub packages: BTreeMap<String, Package>,
}

fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase()) && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn check_package_name(name: &str, at: &str) -> Result<(), String> {
    if !is_name(name) {
        return Err(format!("{at}: '{name}' is not a package name (lowercase letters, digits and _, starting with a letter)"));
    }
    if ["net", "fs", "db", "ffi"].contains(&name) {
        return Err(format!("{at}: '{name}' belongs to the runtime and cannot name a package"));
    }
    Ok(())
}

/// A git location: https, ssh, or a local path (for development and tests). Plain
/// http and git:// are refused: nothing would protect the code in transit.
fn check_git(url: &str, at: &str) -> Result<(), String> {
    if url.is_empty() || url.starts_with('-') || url.chars().any(|c| c.is_control()) {
        return Err(format!("{at}: '{url}' is not a git location"));
    }
    // `user@host:path`, the short form of ssh: a plain user and host before the colon.
    let scp_like = url.split_once(':').is_some_and(|(left, right)| {
        let host_ok = left.split_once('@').is_some_and(|(u, h)| {
            let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
            plain(u) && plain(h)
        });
        host_ok && !right.is_empty() && !right.starts_with('/') && !right.contains(char::is_whitespace)
    });
    let ok = url.starts_with("https://") || url.starts_with("ssh://") || url.starts_with("file://") || url.starts_with('/') || scp_like;
    if !ok {
        return Err(format!("{at}: '{url}' must be https://, ssh://, user@host:path, or a local path"));
    }
    Ok(())
}

fn check_commit(c: &str, at: &str) -> Result<(), String> {
    if c.len() != 40 || !c.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(format!("{at}: '{c}' must be a full commit id, 40 lowercase hex digits; branches and tags can move"));
    }
    Ok(())
}

fn check_pattern(path: &str, p: &str) -> Result<(), String> {
    crate::pattern::validate(p)?;
    if path.starts_with("net.http.") {
        crate::targets::check_url_pattern(p)
    } else if path.starts_with("fs.") {
        crate::targets::check_path_pattern(p)
    } else {
        Ok(())
    }
}

fn parse_deps(v: Option<&Json>, at: &str) -> Result<BTreeMap<String, Spec>, String> {
    let mut out = BTreeMap::new();
    let Some(v) = v else { return Ok(out) };
    let obj = v.as_object().ok_or(format!("{at}.dependencies: expected an object"))?;
    for (name, d) in obj {
        let dat = format!("{at}.dependencies.{name}");
        check_package_name(name, &dat)?;
        let d = d.as_object().ok_or(format!(
            "{dat}: expected {{\"git\": ..., \"commit\": ...}}, {{\"git\": ..., \"version\": ...}} or {{\"version\": ...}}"
        ))?;
        for k in d.keys() {
            if !["git", "commit", "version"].contains(&k.as_str()) {
                return Err(format!("{dat}: unknown key '{k}'"));
            }
        }
        let text = |k: &str| match d.get(k) {
            None => Ok(None),
            Some(Json::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(format!("{dat}.{k}: expected a string")),
        };
        let (git, commit, version) = (text("git")?, text("commit")?, text("version")?);
        if let Some(g) = git {
            check_git(g, &format!("{dat}.git"))?;
        }
        let range = |v: &str| Range::parse(v).map_err(|e| format!("{dat}.version: {e}"));
        let spec = match (git, commit, version) {
            (Some(g), Some(c), None) => {
                check_commit(c, &format!("{dat}.commit"))?;
                Spec::Commit(Dep { git: g.to_string(), commit: c.to_string() })
            }
            (Some(g), None, Some(v)) => Spec::Tags { git: g.to_string(), range: range(v)? },
            (None, None, Some(v)) => Spec::Registry { range: range(v)? },
            (_, Some(_), Some(_)) => return Err(format!("{dat}: give a commit or a version, not both")),
            (None, Some(_), None) => return Err(format!("{dat}: a commit needs the git location it is in")),
            (Some(_), None, None) => return Err(format!("{dat}: give the commit, or a version range of the repository's release tags")),
            (None, None, None) => return Err(format!("{dat}: give a version range, or a git location and a commit or version")),
        };
        out.insert(name.clone(), spec);
    }
    Ok(out)
}

impl Manifest {
    /// `package` is Some for a package's manifest (then `package` and `authority` are
    /// required), None for a project's.
    pub fn parse(text: &str, at: &str, as_package: bool) -> Result<Manifest, String> {
        let v: Json = serde_json::from_str(text).map_err(|e| format!("{at}: not valid JSON: {e}"))?;
        let obj = v.as_object().ok_or(format!("{at}: expected an object"))?;
        for k in obj.keys() {
            if !["package", "version", "authority", "dependencies", "registry"].contains(&k.as_str()) {
                return Err(format!("{at}: unknown key '{k}'"));
            }
        }
        let package = match obj.get("package") {
            None => None,
            Some(Json::String(n)) => {
                check_package_name(n, &format!("{at}.package"))?;
                Some(n.clone())
            }
            Some(_) => return Err(format!("{at}.package: expected a string")),
        };
        let version = match obj.get("version") {
            None => None,
            Some(Json::String(s)) => Some(Version::parse(s).map_err(|e| format!("{at}.version: {e}"))?),
            Some(_) => return Err(format!("{at}.version: expected a string")),
        };
        let registry = match obj.get("registry") {
            None => None,
            Some(_) if as_package => {
                return Err(format!("{at}.registry: the project chooses the registry; a package cannot"));
            }
            Some(Json::String(g)) => {
                check_git(g, &format!("{at}.registry"))?;
                Some(g.clone())
            }
            Some(_) => return Err(format!("{at}.registry: expected a git location")),
        };
        let mut authority = BTreeMap::new();
        if let Some(a) = obj.get("authority") {
            let a = a.as_object().ok_or(format!("{at}.authority: expected an object"))?;
            for (path, v) in a {
                let aat = format!("{at}.authority.{path}");
                if path.is_empty() || !path.split('.').all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
                    return Err(format!("{aat}: '{path}' is not a capability path"));
                }
                let auth = match v {
                    Json::String(s) if s == "any" => Authority::Any,
                    Json::Array(items) => {
                        if items.is_empty() {
                            return Err(format!("{aat}: an empty list allows nothing; leave the capability out instead"));
                        }
                        let mut pats = Vec::new();
                        for (i, p) in items.iter().enumerate() {
                            let p = p.as_str().ok_or(format!("{aat}[{i}]: expected a string"))?;
                            check_pattern(path, p).map_err(|e| format!("{aat}[{i}]: {e}"))?;
                            pats.push(p.to_string());
                        }
                        Authority::Only(pats)
                    }
                    _ => return Err(format!("{aat}: expected \"any\" or a list of `only` patterns")),
                };
                authority.insert(path.clone(), auth);
            }
        }
        if as_package {
            if package.is_none() {
                return Err(format!("{at}: a package's manifest must give its \"package\" name"));
            }
            if !obj.contains_key("authority") {
                return Err(format!("{at}: a package's manifest must list its \"authority\" ({{}} if it needs none)"));
            }
        }
        let dependencies = parse_deps(obj.get("dependencies"), at)?;
        if let Some(n) = &package
            && dependencies.contains_key(n)
        {
            return Err(format!("{at}: package '{n}' cannot depend on itself"));
        }
        Ok(Manifest { package, version, authority, dependencies, registry })
    }
}

/// `staal.lock`: for each package, where it came from and the SHA-256 of every file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lock {
    pub packages: BTreeMap<String, LockedPackage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LockedPackage {
    pub dep: Dep,
    /// The version its manifest gives, if any.
    pub version: Option<Version>,
    /// The registry it was chosen from, for a dependency on the registry.
    pub registry: Option<String>,
    pub files: BTreeMap<String, String>,
}

impl Lock {
    pub fn parse(text: &str, at: &str) -> Result<Lock, String> {
        let v: Json = serde_json::from_str(text).map_err(|e| format!("{at}: not valid JSON: {e}"))?;
        let obj = v.as_object().ok_or(format!("{at}: expected an object"))?;
        // Version 1 has no versions or registries; it reads as version 2 without them.
        if obj.get("staal_lock") != Some(&json!(1)) && obj.get("staal_lock") != Some(&json!(2)) {
            return Err(format!("{at}: not a staal.lock of version 1 or 2"));
        }
        for k in obj.keys() {
            if k != "staal_lock" && k != "packages" {
                return Err(format!("{at}: unknown key '{k}'"));
            }
        }
        let mut packages = BTreeMap::new();
        let ps = obj.get("packages").and_then(Json::as_object).ok_or(format!("{at}.packages: expected an object"))?;
        for (name, p) in ps {
            let pat = format!("{at}.packages.{name}");
            check_package_name(name, &pat)?;
            let p = p.as_object().ok_or(format!("{pat}: expected an object"))?;
            for k in p.keys() {
                if !["git", "commit", "version", "registry", "files"].contains(&k.as_str()) {
                    return Err(format!("{pat}: unknown key '{k}'"));
                }
            }
            let git = p.get("git").and_then(Json::as_str).ok_or(format!("{pat}.git: expected a string"))?;
            let commit = p.get("commit").and_then(Json::as_str).ok_or(format!("{pat}.commit: expected a string"))?;
            check_git(git, &format!("{pat}.git"))?;
            check_commit(commit, &format!("{pat}.commit"))?;
            let mut files = BTreeMap::new();
            let fs = p.get("files").and_then(Json::as_object).ok_or(format!("{pat}.files: expected an object"))?;
            for (f, h) in fs {
                check_rel_path(f).map_err(|e| format!("{pat}.files: {e}"))?;
                let h = h.as_str().filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()));
                let h = h.ok_or(format!("{pat}.files.{f}: expected a SHA-256 in hex"))?;
                files.insert(f.clone(), h.to_ascii_lowercase());
            }
            let version = match p.get("version") {
                None | Some(Json::Null) => None,
                Some(Json::String(v)) => Some(Version::parse(v).map_err(|e| format!("{pat}.version: {e}"))?),
                Some(_) => return Err(format!("{pat}.version: expected a string")),
            };
            let registry = match p.get("registry") {
                None | Some(Json::Null) => None,
                Some(Json::String(g)) => {
                    check_git(g, &format!("{pat}.registry"))?;
                    Some(g.clone())
                }
                Some(_) => return Err(format!("{pat}.registry: expected a git location")),
            };
            packages.insert(name.clone(), LockedPackage { dep: Dep { git: git.into(), commit: commit.into() }, version, registry, files });
        }
        Ok(Lock { packages })
    }

    pub fn to_text(&self) -> String {
        let mut ps = Map::new();
        for (name, p) in &self.packages {
            let files: Map<String, Json> = p.files.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            let mut e = json!({ "git": p.dep.git, "commit": p.dep.commit, "files": files });
            if let Some(v) = &p.version {
                e["version"] = json!(v.to_string());
            }
            if let Some(r) = &p.registry {
                e["registry"] = json!(r);
            }
            ps.insert(name.clone(), e);
        }
        let mut text = serde_json::to_string_pretty(&json!({ "staal_lock": 2, "packages": ps })).expect("serialisable");
        text.push('\n');
        text
    }
}

/// A file path inside a package, with `/` separators: no empty, `.` or `..` parts, no
/// `.git`, no backslash or control characters, nothing absolute.
fn check_rel_path(p: &str) -> Result<(), String> {
    if p.is_empty() || p.starts_with('/') {
        return Err(format!("'{p}' is not a relative path"));
    }
    for part in p.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git") {
            return Err(format!("'{p}' has a part that cannot be used: '{part}'"));
        }
        if part.chars().any(|c| c.is_control() || c == '\\' || c == ':') {
            return Err(format!("'{p}' has a character that cannot be used in a package path"));
        }
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every regular file under `dir`, by relative path, with its content. Links and other
/// special files are refused: a package is plain files.
fn read_tree(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut out = BTreeMap::new();
    let mut total = 0u64;
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, rel)) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|e| format!("cannot read {}: {e}", d.display()))?;
        for e in entries {
            let e = e.map_err(|e| format!("cannot read {}: {e}", d.display()))?;
            let name = e.file_name().into_string().map_err(|_| format!("a file name in {} is not UTF-8", d.display()))?;
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let meta = std::fs::symlink_metadata(e.path()).map_err(|err| format!("cannot read {r}: {err}"))?;
            if meta.file_type().is_dir() {
                stack.push((e.path(), r));
            } else if meta.file_type().is_file() {
                total += meta.len();
                if out.len() >= MAX_FILES || total > MAX_BYTES {
                    return Err(format!("{} holds more than {MAX_FILES} files or {MAX_BYTES} bytes", dir.display()));
                }
                let bytes = std::fs::read(e.path()).map_err(|err| format!("cannot read {r}: {err}"))?;
                out.insert(r, bytes);
            } else {
                return Err(format!("{r} is a link or a special file; a package holds only plain files"));
            }
        }
    }
    Ok(out)
}

impl Project {
    /// Opens the project in `root`: None when it has no `staal.json` (a program
    /// without packages). Every package is verified against `staal.lock` here.
    pub fn open(root: &Path) -> Result<Option<Project>, String> {
        let mpath = root.join(MANIFEST);
        let lpath = root.join(LOCKFILE);
        let mtext = match std::fs::read_to_string(&mpath) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if lpath.exists() {
                    return Err(format!("{} exists without {}", lpath.display(), mpath.display()));
                }
                return Ok(None);
            }
            Err(e) => return Err(format!("cannot read {}: {e}", mpath.display())),
        };
        let manifest = Manifest::parse(&mtext, MANIFEST, false)?;
        if manifest.dependencies.is_empty() && !lpath.exists() {
            return Ok(Some(Project { root: root.to_path_buf(), deps: BTreeMap::new(), packages: BTreeMap::new() }));
        }
        let ltext = std::fs::read_to_string(&lpath)
            .map_err(|e| format!("cannot read {}: {e}; run `staalc fetch` to fetch the dependencies", lpath.display()))?;
        let lock = Lock::parse(&ltext, LOCKFILE)?;
        let mut packages = BTreeMap::new();
        for (name, lp) in &lock.packages {
            let dir = root.join(STORE).join(name);
            let tree = read_tree(&dir).map_err(|e| format!("package {name}: {e}; run `staalc fetch`"))?;
            verify(name, &tree, &lp.files)?;
            let mtext = std::str::from_utf8(&tree[MANIFEST]).map_err(|_| format!("package {name}: {MANIFEST} is not UTF-8"))?;
            let m = Manifest::parse(mtext, &format!("package {name}: {MANIFEST}"), true)?;
            if m.package.as_deref() != Some(name) {
                return Err(format!("package {name}: its {MANIFEST} names it '{}'", m.package.unwrap_or_default()));
            }
            if m.version != lp.version {
                return Err(format!("package {name} does not match {LOCKFILE}: its {MANIFEST} gives another version; run `staalc fetch`"));
            }
            let mut modules = BTreeMap::new();
            for (f, bytes) in &tree {
                if let Some(stem) = f.strip_suffix(".staal") {
                    let text = String::from_utf8(bytes.clone()).map_err(|_| format!("package {name}: {f} is not UTF-8"))?;
                    modules.insert(stem.to_string(), text);
                }
            }
            packages.insert(name.clone(), Package { name: name.clone(), dep: lp.dep.clone(), manifest: m, modules });
        }
        // The lock must be exactly the dependency graph: every package locked at what
        // everything that depends on it asks for, and nothing else.
        let mut wanted: BTreeSet<String> = BTreeSet::new();
        let mut queue: Vec<(String, Spec, String)> =
            manifest.dependencies.iter().map(|(n, d)| (n.clone(), d.clone(), "the project".to_string())).collect();
        while let Some((n, spec, by)) = queue.pop() {
            let p = packages
                .get(&n)
                .ok_or(format!("{LOCKFILE} has no package {n} (wanted by {by}); run `staalc fetch`"))?;
            let locked = &lock.packages[&n];
            satisfies(&spec, locked, manifest.registry.as_deref()).map_err(|why| {
                format!("{LOCKFILE} has package {n} {}, but {by} wants {}: {why}; run `staalc fetch`", locked_text(locked), spec.describe())
            })?;
            if wanted.insert(n.clone()) {
                for (dn, dd) in &p.manifest.dependencies {
                    queue.push((dn.clone(), dd.clone(), format!("package {n}")));
                }
            }
        }
        for n in packages.keys() {
            if !wanted.contains(n) {
                return Err(format!("{LOCKFILE} has package {n}, which nothing depends on; run `staalc fetch`"));
            }
        }
        Ok(Some(Project { root: root.to_path_buf(), deps: manifest.dependencies, packages }))
    }

    /// The package a module belongs to, or None for the program's own modules.
    pub fn package_of(&self, module: &[String]) -> Option<&Package> {
        if module.len() >= 2 { self.packages.get(&module[0]) } else { None }
    }

    /// One line per package: what it is and the authority it may use.
    pub fn describe(&self) -> Vec<String> {
        self.packages.values().map(describe_package).collect()
    }
}

fn describe_authority(a: &BTreeMap<String, Authority>) -> String {
    if a.is_empty() {
        return "no capabilities".into();
    }
    a.iter()
        .map(|(p, au)| match au {
            Authority::Any => format!("{p} (any target)"),
            Authority::Only(v) => format!("{p} only {}", v.join(", ")),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn describe_package(p: &Package) -> String {
    let version = p.manifest.version.as_ref().map(|v| format!(" {v}")).unwrap_or_default();
    format!("{}{version} {} at {}: {}", p.name, p.dep.git, &p.dep.commit[..12], describe_authority(&p.manifest.authority))
}

fn verify(name: &str, tree: &BTreeMap<String, Vec<u8>>, files: &BTreeMap<String, String>) -> Result<(), String> {
    for (f, want) in files {
        let Some(bytes) = tree.get(f) else {
            return Err(format!("package {name} does not match {LOCKFILE}: {f} is missing"));
        };
        if &sha256_hex(bytes) != want {
            return Err(format!("package {name} does not match {LOCKFILE}: {f} has been changed"));
        }
    }
    for f in tree.keys() {
        if !files.contains_key(f) {
            return Err(format!("package {name} does not match {LOCKFILE}: {f} is not part of it"));
        }
    }
    if !files.contains_key(MANIFEST) {
        return Err(format!("package {name} has no {MANIFEST}"));
    }
    Ok(())
}

/// Whether a locked package is what `spec` asks for, or why not. `registry` is the
/// project's.
fn satisfies(spec: &Spec, l: &LockedPackage, registry: Option<&str>) -> Result<(), String> {
    let in_range = |range: &Range| match &l.version {
        Some(v) if range.matches(v) => Ok(()),
        Some(v) => Err(format!("{v} is not within {range}")),
        None => Err("its manifest gives no version".to_string()),
    };
    match spec {
        Spec::Commit(d) if l.registry.is_none() && l.dep == *d => Ok(()),
        Spec::Commit(_) => Err("another commit or location".into()),
        Spec::Tags { git, range } if l.registry.is_none() && l.dep.git == *git => in_range(range),
        Spec::Tags { .. } => Err("another location".into()),
        Spec::Registry { range } => match (registry, &l.registry) {
            (None, _) => Err(format!("the project's {MANIFEST} names no registry")),
            (Some(r), Some(lr)) if r == lr => in_range(range),
            _ => Err("it is not from the project's registry".into()),
        },
    }
}

fn locked_text(l: &LockedPackage) -> String {
    match &l.version {
        Some(v) => format!("{v} (commit {} of {})", &l.dep.commit[..12], l.dep.git),
        None => format!("at commit {} of {}", &l.dep.commit[..12], l.dep.git),
    }
}

/// Finds modules for a program in `root` that may use packages.
pub struct ProjectSource {
    pub root: PathBuf,
    pub project: Option<std::rc::Rc<Project>>,
    entry_real: Option<PathBuf>,
}

impl ProjectSource {
    pub fn new(root: PathBuf, project: Option<std::rc::Rc<Project>>, entry: &Path) -> ProjectSource {
        ProjectSource { root, project, entry_real: std::fs::canonicalize(entry).ok() }
    }

    fn local_exists(&self, base: &Path, first: &str) -> bool {
        base.join(first).exists() || base.join(format!("{first}.staal")).exists()
    }
}

impl Source for ProjectSource {
    fn locate(&mut self, from: &[String], written: &[String]) -> Result<Vec<String>, String> {
        let Some(project) = &self.project else { return Ok(written.to_vec()) };
        let first = &written[0];
        match project.package_of(from) {
            // Inside a package: its dependencies by name, otherwise its own modules.
            Some(pkg) => {
                if pkg.manifest.dependencies.contains_key(first) {
                    if written.len() < 2 {
                        return Err(format!("import a module of package {first}, such as {first}.<module>"));
                    }
                    if pkg.modules.keys().any(|m| m == first || m.starts_with(&format!("{first}/"))) {
                        return Err(format!("package {} has both a dependency and a module named '{first}'", pkg.name));
                    }
                    return Ok(written.to_vec());
                }
                let mut p = vec![pkg.name.clone()];
                p.extend(written.iter().cloned());
                Ok(p)
            }
            None => {
                if project.deps.contains_key(first) {
                    if written.len() < 2 {
                        return Err(format!("import a module of package {first}, such as {first}.<module>"));
                    }
                    if self.local_exists(&self.root, first) {
                        return Err(format!("'{first}' is both a dependency and a file or folder of this program; rename one"));
                    }
                    return Ok(written.to_vec());
                }
                if project.packages.contains_key(first) {
                    return Err(format!(
                        "'{first}' is a package this program does not depend on itself (another package does); add it to {MANIFEST} to use it"
                    ));
                }
                Ok(written.to_vec())
            }
        }
    }

    fn read(&mut self, module: &[String]) -> Result<SourceFile, String> {
        if let Some(pkg) = self.project.as_ref().and_then(|p| p.package_of(module)) {
            let rel = module[1..].join("/");
            let text = pkg.modules.get(&rel).ok_or(format!("package {} has no module {rel}.staal", pkg.name))?;
            return Ok(SourceFile { name: format!("{}:{rel}.staal", pkg.name), text: text.clone() });
        }
        let mut file = self.root.clone();
        for seg in module {
            file.push(seg);
        }
        file.set_extension("staal");
        if self.entry_real.is_some() && std::fs::canonicalize(&file).ok() == self.entry_real {
            return Err(format!("{} is the program itself", file.display()));
        }
        let text = std::fs::read_to_string(&file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        Ok(SourceFile { name: file.display().to_string(), text })
    }
}

// ---------------------------------------------------------------------------------
// Authority (E108)

fn e108(pos: Pos, msg: String) -> Diagnostic {
    Diagnostic { code: "E108", pos, msg }
}

/// Checks every package's code against the authority its manifest declares.
/// `module_of[file]` is the module path of each file of the program.
pub fn check_authority(prog: &Program, module_of: &[Vec<String>], project: &Project) -> Vec<Diagnostic> {
    let pkg = |pos: Pos| module_of.get(pos.file as usize).and_then(|m| project.package_of(m));
    let mut diags = Vec::new();
    for c in &prog.caps {
        let Some(p) = pkg(c.pos) else { continue };
        if c.path[0] != p.name {
            diags.push(e108(
                c.pos,
                format!("package {} declares capability {}; a package's capabilities must start with its name", p.name, c.path.join(".")),
            ));
        }
        for (_, t, pos) in &c.needs {
            check_ty(t, *pos, p, &mut diags);
        }
    }
    // An action's needs are also its last parameters; they are checked once, above.
    let declared: std::collections::HashMap<&str, usize> =
        prog.caps.iter().flat_map(|c| c.actions.iter().map(|a| (a.fn_name.as_str(), a.declared))).collect();
    for f in &prog.fns {
        let Some(p) = pkg(f.pos) else { continue };
        let own = declared.get(f.name.as_str()).copied().unwrap_or(f.params.len());
        for prm in &f.params[..own] {
            check_ty(&prm.ty, f.pos, p, &mut diags);
        }
        if let Some(t) = &f.ret {
            check_ty(t, f.pos, p, &mut diags);
        }
        if let Some(t) = &f.fails {
            check_ty(t, f.pos, p, &mut diags);
        }
        check_block(&f.body, p, &mut diags);
    }
    for t in &prog.types {
        let Some(p) = pkg(t.pos) else { continue };
        for (_, fty) in &t.fields {
            check_ty(fty, t.pos, p, &mut diags);
        }
    }
    diags
}

fn check_block(body: &[Stmt], p: &Package, diags: &mut Vec<Diagnostic>) {
    for s in body {
        if let Stmt::Let { ty: Some(t), pos, .. } = s {
            check_ty(t, *pos, p, diags);
        }
        for b in s.blocks() {
            check_block(b, p, diags);
        }
    }
}

fn check_ty(t: &Ty, pos: Pos, p: &Package, diags: &mut Vec<Diagnostic>) {
    match t {
        Ty::Cap { path, limits } => {
            let shown = path.join(".");
            match p.manifest.authority.get(&shown) {
                None => diags.push(e108(
                    pos,
                    format!("package {} uses `cap {shown}`, which its {MANIFEST} does not list under \"authority\"", p.name),
                )),
                Some(Authority::Any) => {}
                Some(Authority::Only(wide)) => match &limits.only {
                    Some(narrow) if crate::pattern::all_contained(narrow, wide) => {}
                    Some(narrow) => diags.push(e108(
                        pos,
                        format!(
                            "package {}: `cap {shown}(only: {narrow:?})` reaches beyond its {MANIFEST}, which allows only {wide:?}",
                            p.name
                        ),
                    )),
                    None => diags.push(e108(
                        pos,
                        format!("package {}: `cap {shown}` needs `only` within {wide:?}, as its {MANIFEST} declares", p.name),
                    )),
                },
            }
        }
        Ty::Proposed(inner) | Ty::List(inner, _) | Ty::Optional(inner) => check_ty(inner, pos, p, diags),
        Ty::Tuple(items) => items.iter().for_each(|i| check_ty(i, pos, p, diags)),
        Ty::Map(k, v, _) => {
            check_ty(k, pos, p, diags);
            check_ty(v, pos, p, diags);
        }
        Ty::Set(t, _) => check_ty(t, pos, p, diags),
        Ty::Func(ps, r) => {
            ps.iter().for_each(|t| check_ty(t, pos, p, diags));
            check_ty(r, pos, p, diags);
        }
        Ty::Named(_) => {}
    }
}

// ---------------------------------------------------------------------------------
// Fetching

fn git(dir: Option<&Path>, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    // Only these transports; never a helper program named by a URL; never a prompt.
    cmd.args(["-c", "protocol.allow=never", "-c", "protocol.https.allow=always", "-c", "protocol.ssh.allow=always"]);
    cmd.args(["-c", "protocol.file.allow=always"]);
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0").env("GIT_ASKPASS", "true").stdin(std::process::Stdio::null());
    let out = cmd.output().map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}: {}", args.first().unwrap_or(&""), err.trim()));
    }
    Ok(out.stdout)
}

type Files = BTreeMap<String, Vec<u8>>;

/// A scratch folder removed when dropped, whatever happened.
struct Scratch(PathBuf);

impl Scratch {
    fn new(dir: PathBuf) -> Result<Scratch, String> {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bare clone of `location`, as `<scratch>/<label>.git`.
fn clone_bare(label: &str, location: &str, scratch: &Path) -> Result<PathBuf, String> {
    let repo = scratch.join(format!("{label}.git"));
    let _ = std::fs::remove_dir_all(&repo);
    let url = location.strip_prefix("file://").unwrap_or(location);
    git(None, &["clone", "--bare", "--quiet", "--", url, repo.to_str().ok_or("scratch path is not UTF-8")?])
        .map_err(|e| format!("cannot fetch {location}: {e}"))?;
    Ok(repo)
}

/// The files of one commit, read from git's objects.
fn read_commit(name: &str, repo: &Path, dep: &Dep) -> Result<Files, String> {
    let commit_ok = git(Some(repo), &["cat-file", "-t", &dep.commit]).map(|t| t == b"commit\n").unwrap_or(false);
    if !commit_ok {
        return Err(format!("package {name}: {} has no commit {}", dep.git, dep.commit));
    }
    let listing = git(Some(repo), &["ls-tree", "-r", "-z", "--full-tree", &dep.commit])?;
    let mut files = BTreeMap::new();
    let mut lower = BTreeSet::new();
    let mut total = 0u64;
    for entry in listing.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = std::str::from_utf8(entry).map_err(|_| format!("package {name}: a file name is not UTF-8"))?;
        let (meta, path) = entry.split_once('\t').ok_or(format!("package {name}: unexpected git output"))?;
        let parts: Vec<&str> = meta.split(' ').collect();
        let [mode, kind, oid] = parts[..] else { return Err(format!("package {name}: unexpected git output")) };
        check_rel_path(path).map_err(|e| format!("package {name}: {e}"))?;
        if kind != "blob" || !(mode == "100644" || mode == "100755") {
            return Err(format!("package {name}: {path} is a link or submodule; a package holds only plain files"));
        }
        // Two names that differ only in case would be one file on some systems.
        if !lower.insert(path.to_lowercase()) {
            return Err(format!("package {name}: two files differ only in upper and lower case ({path})"));
        }
        let bytes = git(Some(repo), &["cat-file", "blob", oid])?;
        total += bytes.len() as u64;
        if files.len() >= MAX_FILES || total > MAX_BYTES {
            return Err(format!("package {name} holds more than {MAX_FILES} files or {MAX_BYTES} bytes"));
        }
        files.insert(path.to_string(), bytes);
    }
    Ok(files)
}

fn write_tree(dir: &Path, files: &Files) -> Result<(), String> {
    for (rel, bytes) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        std::io::Write::write_all(&mut f, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        f.sync_all().map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn hashes(files: &Files) -> BTreeMap<String, String> {
    files.iter().map(|(k, v)| (k.clone(), sha256_hex(v))).collect()
}

/// One SHA-256 for a whole package: of a line `<file hash>  <path>` per file, in path
/// order (paths cannot hold a newline). A registry records it for each version.
pub fn files_digest(hashes: &BTreeMap<String, String>) -> String {
    let mut text = String::new();
    for (path, h) in hashes {
        text.push_str(&format!("{h}  {path}\n"));
    }
    sha256_hex(text.as_bytes())
}

// ---------------------------------------------------------------------------------
// Registries
//
// A registry is a git repository: `staal-registry.json` (`{"staal_registry": 1}`) at
// its root, and `packages/<name>.json` for each package:
//
//   { "package": "sms",
//     "versions": [ { "version": "1.2.0", "git": "https://...", "commit": "<40 hex>",
//                     "files_sha256": "<64 hex>", "yanked": false } ] }
//
// Each version names one commit and the digest of its files, so a registry cannot be
// made to serve other files for a version by rewriting the package's repository.

pub const REGISTRY_MARKER: &str = "staal-registry.json";

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub version: Version,
    pub dep: Dep,
    pub files_sha256: String,
    pub yanked: bool,
}

fn parse_entries(text: &str, name: &str, at: &str) -> Result<Vec<Entry>, String> {
    let v: Json = serde_json::from_str(text).map_err(|e| format!("{at}: not valid JSON: {e}"))?;
    let obj = v.as_object().ok_or(format!("{at}: expected an object"))?;
    for k in obj.keys() {
        if k != "package" && k != "versions" {
            return Err(format!("{at}: unknown key '{k}'"));
        }
    }
    if obj.get("package").and_then(Json::as_str) != Some(name) {
        return Err(format!("{at}: \"package\" must be \"{name}\""));
    }
    let list = obj.get("versions").and_then(Json::as_array).ok_or(format!("{at}.versions: expected a list"))?;
    let mut out: Vec<Entry> = Vec::new();
    for (i, e) in list.iter().enumerate() {
        let eat = format!("{at}.versions[{i}]");
        let e = e.as_object().ok_or(format!("{eat}: expected an object"))?;
        for k in e.keys() {
            if !["version", "git", "commit", "files_sha256", "yanked"].contains(&k.as_str()) {
                return Err(format!("{eat}: unknown key '{k}'"));
            }
        }
        let s = |k: &str| e.get(k).and_then(Json::as_str).ok_or(format!("{eat}.{k}: expected a string"));
        let version = Version::parse(s("version")?).map_err(|err| format!("{eat}.version: {err}"))?;
        let (location, commit, digest) = (s("git")?, s("commit")?, s("files_sha256")?);
        check_git(location, &format!("{eat}.git"))?;
        check_commit(commit, &format!("{eat}.commit"))?;
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(format!("{eat}.files_sha256: expected a SHA-256 in lowercase hex"));
        }
        let yanked = match e.get("yanked") {
            None => false,
            Some(Json::Bool(b)) => *b,
            Some(_) => return Err(format!("{eat}.yanked: expected true or false")),
        };
        if out.iter().any(|o| o.version == version) {
            return Err(format!("{at}: version {version} is listed twice"));
        }
        out.push(Entry { version, dep: Dep { git: location.into(), commit: commit.into() }, files_sha256: digest.into(), yanked });
    }
    Ok(out)
}

fn entries_text(name: &str, entries: &[Entry]) -> String {
    let versions: Vec<Json> = entries
        .iter()
        .map(|e| json!({ "version": e.version.to_string(), "git": e.dep.git, "commit": e.dep.commit, "files_sha256": e.files_sha256, "yanked": e.yanked }))
        .collect();
    let mut text = serde_json::to_string_pretty(&json!({ "package": name, "versions": versions })).expect("serialisable");
    text.push('\n');
    text
}

fn check_marker(text: &[u8], at: &str) -> Result<(), String> {
    let v: Option<Json> = serde_json::from_slice(text).ok();
    if v != Some(json!({ "staal_registry": 1 })) {
        return Err(format!("{at}: {REGISTRY_MARKER} must be {{\"staal_registry\": 1}}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------
// Choosing versions

/// What one package was resolved to.
#[derive(Debug, Clone, PartialEq)]
struct Pick {
    dep: Dep,
    version: Option<Version>,
    registry: Option<String>,
}

impl Pick {
    fn as_locked(&self) -> LockedPackage {
        LockedPackage { dep: self.dep.clone(), version: self.version.clone(), registry: self.registry.clone(), files: BTreeMap::new() }
    }
}

/// Reads repositories, commits, tags and the registry, each once per fetch.
struct Fetcher {
    scratch: PathBuf,
    clones: BTreeMap<String, PathBuf>,
    trees: BTreeMap<Dep, Files>,
    registry: Option<String>,
    index: Option<PathBuf>,
}

impl Fetcher {
    fn repo(&mut self, location: &str) -> Result<PathBuf, String> {
        if let Some(r) = self.clones.get(location) {
            return Ok(r.clone());
        }
        let r = clone_bare(&format!("r{}", self.clones.len()), location, &self.scratch)?;
        self.clones.insert(location.to_string(), r.clone());
        Ok(r)
    }

    fn files(&mut self, name: &str, dep: &Dep) -> Result<&Files, String> {
        if !self.trees.contains_key(dep) {
            let repo = self.repo(&dep.git).map_err(|e| format!("package {name}: {e}"))?;
            let files = read_commit(name, &repo, dep)?;
            self.trees.insert(dep.clone(), files);
        }
        Ok(&self.trees[dep])
    }

    fn manifest(&mut self, name: &str, dep: &Dep) -> Result<Manifest, String> {
        let files = self.files(name, dep)?;
        let text = files.get(MANIFEST).ok_or(format!("package {name} has no {MANIFEST} at its root (commit {} of {})", &dep.commit[..12], dep.git))?;
        let text = std::str::from_utf8(text).map_err(|_| format!("package {name}: {MANIFEST} is not UTF-8"))?;
        let m = Manifest::parse(text, &format!("package {name}: {MANIFEST}"), true)?;
        if m.package.as_deref() != Some(name) {
            return Err(format!("package {name}: its {MANIFEST} (commit {} of {}) names it '{}'", &dep.commit[..12], dep.git, m.package.unwrap_or_default()));
        }
        Ok(m)
    }

    /// The release tags of a repository (`v1.2.0` or `1.2.0`), by version.
    fn tags(&mut self, name: &str, location: &str) -> Result<BTreeMap<Version, Dep>, String> {
        let repo = self.repo(location).map_err(|e| format!("package {name}: {e}"))?;
        let out = git(Some(&repo), &["for-each-ref", "--format=%(refname:strip=2)", "refs/tags"])?;
        let mut tags: BTreeMap<Version, (Dep, String)> = BTreeMap::new();
        for t in String::from_utf8_lossy(&out).lines() {
            let Ok(v) = Version::parse(t.strip_prefix('v').unwrap_or(t)) else { continue };
            // A tag of something other than a commit is not a release.
            let Ok(c) = git(Some(&repo), &["rev-parse", "--verify", "--quiet", &format!("refs/tags/{t}^{{commit}}")]) else { continue };
            let c = String::from_utf8_lossy(&c).trim().to_string();
            check_commit(&c, &format!("package {name}: tag {t}"))?;
            let dep = Dep { git: location.to_string(), commit: c };
            if let Some((other, ot)) = tags.get(&v)
                && *other != dep
            {
                return Err(format!("package {name}: tags {ot} and {t} of {location} are both version {v} but name different commits"));
            }
            tags.insert(v, (dep, t.to_string()));
        }
        Ok(tags.into_iter().map(|(v, (d, _))| (v, d)).collect())
    }

    /// Every version of a package the registry lists.
    fn entries(&mut self, name: &str, by: &str) -> Result<Vec<Entry>, String> {
        let Some(registry) = self.registry.clone() else {
            return Err(format!("{by} wants package {name} from the registry, but the project's {MANIFEST} names no \"registry\""));
        };
        if self.index.is_none() {
            let r = clone_bare("registry", &registry, &self.scratch).map_err(|e| format!("the registry: {e}"))?;
            let marker = git(Some(&r), &["cat-file", "blob", &format!("HEAD:{REGISTRY_MARKER}")])
                .map_err(|_| format!("{registry} is not a Staal registry: it has no {REGISTRY_MARKER}"))?;
            check_marker(&marker, &registry)?;
            self.index = Some(r);
        }
        let r = self.index.clone().expect("set above");
        let path = format!("HEAD:packages/{name}.json");
        if git(Some(&r), &["cat-file", "-e", &path]).is_err() {
            return Err(format!("the registry has no package {name} (wanted by {by})"));
        }
        let text = git(Some(&r), &["cat-file", "blob", &path])?;
        let text = String::from_utf8(text).map_err(|_| format!("the registry's packages/{name}.json is not UTF-8"))?;
        parse_entries(&text, name, &format!("the registry's packages/{name}.json"))
    }
}

/// Chooses what one package resolves to, given what everything that depends on it asks
/// for: the locked choice when it still fits (`locked`, None when updating), otherwise
/// the newest version that fits every range. Older versions of the packages that ask
/// are never tried: a conflict is reported, not searched around.
fn choose(f: &mut Fetcher, name: &str, cons: &[(Spec, String)], locked: Option<&LockedPackage>) -> Result<Pick, String> {
    let wants = || cons.iter().map(|(s, by)| format!("{by} wants {}", s.describe())).collect::<Vec<_>>().join("; ");
    let registry = cons.iter().any(|(s, _)| matches!(s, Spec::Registry { .. }));
    let locations: BTreeSet<&str> = cons
        .iter()
        .filter_map(|(s, _)| match s {
            Spec::Commit(d) => Some(d.git.as_str()),
            Spec::Tags { git, .. } => Some(git.as_str()),
            Spec::Registry { .. } => None,
        })
        .collect();
    if registry && !locations.is_empty() {
        return Err(format!("package {name}: {}; a package comes from the registry or from git, not both", wants()));
    }
    if locations.len() > 1 {
        return Err(format!("package {name}: {}; one location per package", wants()));
    }
    let commits: BTreeSet<&Dep> = cons.iter().filter_map(|(s, _)| if let Spec::Commit(d) = s { Some(d) } else { None }).collect();
    if commits.len() > 1 {
        return Err(format!("package {name}: {}; one version per package", wants()));
    }
    let ranges: Vec<&Range> = cons
        .iter()
        .filter_map(|(s, _)| match s {
            Spec::Tags { range, .. } | Spec::Registry { range } => Some(range),
            Spec::Commit(_) => None,
        })
        .collect();
    let fits = |v: &Version| ranges.iter().all(|r| r.matches(v));
    if let Some(d) = commits.first() {
        let m = f.manifest(name, d)?;
        if !ranges.is_empty() && !m.version.as_ref().is_some_and(fits) {
            let is = m.version.as_ref().map_or("no version".to_string(), |v| format!("version {v}"));
            return Err(format!("package {name}: {}; that commit is {is}", wants()));
        }
        return Ok(Pick { dep: (*d).clone(), version: m.version, registry: None });
    }
    // Every version on offer, by version: its commit, the digest the registry records,
    // and whether it was yanked.
    let first_by = &cons[0].1;
    let offered: BTreeMap<Version, (Dep, Option<String>, bool)> = if registry {
        f.entries(name, first_by)?.into_iter().map(|e| (e.version, (e.dep, Some(e.files_sha256), e.yanked))).collect()
    } else {
        let location = locations.first().expect("a location");
        f.tags(name, location)?.into_iter().map(|(v, d)| (v, (d, None, false))).collect()
    };
    let source = if registry { f.registry.clone() } else { None };
    // The locked version, if it came from this source: its tag or entry must still
    // name the locked commit.
    let mut chosen = None;
    if let Some(l) = locked
        && l.registry == source
        && (registry || locations.first() == Some(&l.dep.git.as_str()))
        && let Some(v) = &l.version
        && let Some((d, _, _)) = offered.get(v)
    {
        if *d != l.dep {
            let what = if registry { "the registry's entry for it" } else { "its release tag" };
            return Err(format!(
                "package {name} {v} is commit {} now, but {LOCKFILE} has commit {}: {what} was moved. Nothing was changed; check the package's history, then `staalc update` takes the new commit",
                &d.commit[..12],
                &l.dep.commit[..12]
            ));
        }
        if fits(v) {
            chosen = Some(v.clone());
        }
    }
    let chosen = match chosen.or_else(|| offered.iter().rev().find(|(v, (_, _, yanked))| fits(v) && !yanked).map(|(v, _)| v.clone())) {
        Some(v) => v,
        None => {
            let shown: Vec<String> = offered.iter().rev().take(12).map(|(v, (_, _, y))| if *y { format!("{v} (yanked)") } else { v.to_string() }).collect();
            let offer = if shown.is_empty() { "none".to_string() } else { shown.join(", ") };
            return Err(format!(
                "no version of package {name} fits: {}. Versions: {offer}. staalc takes the newest fitting version of each package and does not try older versions of the packages that ask; give one of them an exact version (\"=1.2.0\") to choose another",
                wants()
            ));
        }
    };
    let (dep, digest, _) = offered[&chosen].clone();
    // What the tag or entry says must be what the files say.
    let m = f.manifest(name, &dep)?;
    if m.version.as_ref() != Some(&chosen) {
        let is = m.version.as_ref().map_or("no version".to_string(), |v| format!("version {v}"));
        return Err(format!("package {name} {chosen} (commit {} of {}): its {MANIFEST} gives {is}", &dep.commit[..12], dep.git));
    }
    if let Some(want) = digest {
        let have = files_digest(&hashes(f.files(name, &dep)?));
        if have != want {
            return Err(format!(
                "package {name} {chosen}: the files of commit {} of {} are not the files the registry recorded for it. Nothing was changed",
                &dep.commit[..12],
                dep.git
            ));
        }
    }
    Ok(Pick { dep, version: Some(chosen), registry: source })
}

/// Which locked choices `staalc fetch` may change: none (fetch), all, or one package's
/// (update).
pub enum Update<'a> {
    Nothing,
    All,
    Package(&'a str),
}

/// `staalc fetch`: resolves every dependency (keeping each locked choice that still
/// fits), fetches it, writes the packages and `staal.lock`, and reports each package
/// and its authority. A package already in the lock at the same commit must have
/// exactly the files the lock records.
pub fn fetch(root: &Path) -> Result<Vec<String>, String> {
    fetch_with(root, Update::Nothing)
}

/// `staalc update`: as `fetch`, but `update` names the locked choices to forget, so
/// those packages move to the newest versions that fit.
pub fn fetch_with(root: &Path, update: Update) -> Result<Vec<String>, String> {
    let mpath = root.join(MANIFEST);
    let mtext = std::fs::read_to_string(&mpath).map_err(|e| format!("cannot read {}: {e}", mpath.display()))?;
    let manifest = Manifest::parse(&mtext, MANIFEST, false)?;
    let old = match std::fs::read_to_string(root.join(LOCKFILE)) {
        Ok(t) => Some(Lock::parse(&t, LOCKFILE)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {LOCKFILE}: {e}")),
    };
    if let Update::Package(n) = update
        && !old.as_ref().is_some_and(|l| l.packages.contains_key(n))
    {
        return Err(format!("{LOCKFILE} has no package {n} to update"));
    }
    let store = root.join(STORE);
    let scratch = Scratch::new(root.join(".staal/scratch"))?;
    let mut f = Fetcher { scratch: scratch.0.clone(), clones: BTreeMap::new(), trees: BTreeMap::new(), registry: manifest.registry.clone(), index: None };

    // Until nothing changes: walk the graph from the project through the current
    // choices, and choose again for every package whose choice no longer fits.
    let mut picks: BTreeMap<String, Pick> = BTreeMap::new();
    let mut settled = false;
    for _ in 0..64 {
        let mut cons: BTreeMap<String, Vec<(Spec, String)>> = BTreeMap::new();
        let mut queue: std::collections::VecDeque<(String, Spec, String)> =
            manifest.dependencies.iter().map(|(n, s)| (n.clone(), s.clone(), "the project".to_string())).collect();
        let mut expanded = BTreeSet::new();
        while let Some((n, s, by)) = queue.pop_front() {
            cons.entry(n.clone()).or_default().push((s, by));
            if let Some(p) = picks.get(&n).cloned()
                && expanded.insert(n.clone())
            {
                let m = f.manifest(&n, &p.dep)?;
                let by = match &m.version {
                    Some(v) => format!("package {n} {v}"),
                    None => format!("package {n}"),
                };
                for (dn, ds) in &m.dependencies {
                    queue.push_back((dn.clone(), ds.clone(), by.clone()));
                }
            }
        }
        let mut changed = false;
        for (n, cs) in &cons {
            if picks.get(n).is_some_and(|p| cs.iter().all(|(s, _)| satisfies(s, &p.as_locked(), manifest.registry.as_deref()).is_ok())) {
                continue;
            }
            let keep = match update {
                Update::Nothing => true,
                Update::All => false,
                Update::Package(u) => u != n,
            };
            let locked = old.as_ref().and_then(|l| l.packages.get(n)).filter(|_| keep);
            let p = choose(&mut f, n, cs, locked)?;
            picks.insert(n.clone(), p);
            changed = true;
        }
        picks.retain(|n, _| cons.contains_key(n));
        if !changed {
            settled = true;
            break;
        }
    }
    if !settled {
        return Err("the dependencies' versions did not settle after 64 rounds: their ranges keep moving each other; give some of them exact versions".into());
    }

    // Always read from git, and compare with the lock: what is on disk, and the lock
    // itself, may have been edited; the commit cannot be.
    let mut lock = Lock::default();
    let mut fetched: BTreeMap<String, (Files, Manifest)> = BTreeMap::new();
    for (name, p) in &picks {
        let files = f.files(name, &p.dep)?.clone();
        if let Some(l) = old.as_ref().and_then(|l| l.packages.get(name)).filter(|l| l.dep == p.dep)
            && hashes(&files) != l.files
        {
            return Err(format!(
                "package {name} at {} does not have the files {LOCKFILE} records: the lock was edited, or the repository's history rewritten. Nothing was changed",
                p.dep.commit
            ));
        }
        let m = f.manifest(name, &p.dep)?;
        lock.packages.insert(name.clone(), LockedPackage { files: hashes(&files), ..p.as_locked() });
        fetched.insert(name.clone(), (files, m));
    }

    // Everything fetched and checked: now replace the store and the lock.
    std::fs::create_dir_all(&store).map_err(|e| format!("cannot create {}: {e}", store.display()))?;
    for (name, (files, _)) in &fetched {
        let dir = store.join(name);
        if read_tree(&dir).ok().is_some_and(|t| verify(name, &t, &lock.packages[name].files).is_ok()) {
            continue;
        }
        let tmp = scratch.0.join(format!("{name}.new"));
        let _ = std::fs::remove_dir_all(&tmp);
        write_tree(&tmp, files)?;
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&tmp, &dir).map_err(|e| format!("cannot move package {name} into place: {e}"))?;
    }
    if let Ok(entries) = std::fs::read_dir(&store) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if !lock.packages.contains_key(&n) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let ltmp = scratch.0.join("staal.lock.new");
    std::fs::write(&ltmp, lock.to_text()).map_err(|e| format!("cannot write {LOCKFILE}: {e}"))?;
    std::fs::rename(&ltmp, root.join(LOCKFILE)).map_err(|e| format!("cannot write {LOCKFILE}: {e}"))?;

    let mut report = Vec::new();
    for (name, (_, m)) in &fetched {
        let l = &lock.packages[name];
        let before = old.as_ref().and_then(|o| o.packages.get(name));
        let note = match before {
            None => "new".to_string(),
            Some(b) if b.dep == l.dep => "unchanged".to_string(),
            Some(b) => match &b.version {
                Some(v) if b.version != l.version => format!("was {v}, commit {}", &b.dep.commit[..12]),
                _ => format!("was commit {}", &b.dep.commit[..12]),
            },
        };
        let version = l.version.as_ref().map(|v| format!(" {v}")).unwrap_or_default();
        let from = if l.registry.is_some() { " from the registry:" } else { "" };
        report.push(format!(
            "{name}{version}{from} {} at {} ({note})\n    authority: {}",
            l.dep.git,
            &l.dep.commit[..12],
            describe_authority(&m.authority)
        ));
    }
    if let Some(o) = &old {
        for n in o.packages.keys() {
            if !lock.packages.contains_key(n) {
                report.push(format!("{n} removed"));
            }
        }
    }
    drop(scratch);
    Ok(report)
}

// ---------------------------------------------------------------------------------
// Publishing to a registry

/// A clean scratch folder outside any project, for reading a package's commit.
fn temp_scratch(what: &str) -> Result<Scratch, String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| format!("no random source: {e}"))?;
    let tag: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Scratch::new(std::env::temp_dir().join(format!("staalc-{what}-{tag}")))
}

/// Checks `index` is a registry's working tree whose files for `name` have no
/// uncommitted changes, creating the marker in a new, empty registry.
fn open_index(index: &Path, name: &str) -> Result<PathBuf, String> {
    let top = git(Some(index), &["rev-parse", "--show-toplevel"]).map_err(|e| format!("{} is not a git working tree: {e}", index.display()))?;
    let top = PathBuf::from(String::from_utf8_lossy(&top).trim());
    let rel = format!("packages/{name}.json");
    let dirty = git(Some(&top), &["status", "--porcelain", "--", REGISTRY_MARKER, &rel])?;
    if !dirty.is_empty() {
        return Err(format!("the registry has uncommitted changes to {REGISTRY_MARKER} or {rel}; commit or discard them first"));
    }
    let marker = top.join(REGISTRY_MARKER);
    match std::fs::read(&marker) {
        Ok(t) => check_marker(&t, &top.display().to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if top.join("packages").exists() {
                return Err(format!("{} has a packages folder but no {REGISTRY_MARKER}", top.display()));
            }
            std::fs::write(&marker, "{\"staal_registry\": 1}\n").map_err(|e| format!("cannot write {}: {e}", marker.display()))?;
        }
        Err(e) => return Err(format!("cannot read {}: {e}", marker.display())),
    }
    Ok(top)
}

fn read_entries(top: &Path, name: &str) -> Result<Vec<Entry>, String> {
    let path = top.join("packages").join(format!("{name}.json"));
    match std::fs::read_to_string(&path) {
        Ok(t) => parse_entries(&t, name, &format!("packages/{name}.json")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn commit_entries(top: &Path, name: &str, entries: &mut [Entry], message: &str) -> Result<(), String> {
    entries.sort_by(|a, b| a.version.cmp(&b.version));
    let dir = top.join("packages");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let rel = format!("packages/{name}.json");
    std::fs::write(top.join(&rel), entries_text(name, entries)).map_err(|e| format!("cannot write {rel}: {e}"))?;
    git(Some(top), &["add", "--", REGISTRY_MARKER, &rel])?;
    git(Some(top), &["commit", "--quiet", "-m", message, "--", REGISTRY_MARKER, &rel])?;
    Ok(())
}

/// `staalc publish`: adds the package at `commit` of `location` to the registry whose
/// working tree is `index`, as a commit there. The version is the package's own, and
/// a version already published is never changed.
pub fn publish(index: &Path, location: &str, commit: &str) -> Result<String, String> {
    check_git(location, "--git")?;
    check_commit(commit, "--commit")?;
    let scratch = temp_scratch("publish")?;
    let repo = clone_bare("package", location, &scratch.0)?;
    let dep = Dep { git: location.to_string(), commit: commit.to_string() };
    let files = read_commit("to publish", &repo, &dep)?;
    let text = files.get(MANIFEST).ok_or(format!("commit {commit} of {location} has no {MANIFEST} at its root"))?;
    let text = std::str::from_utf8(text).map_err(|_| format!("{MANIFEST} is not UTF-8"))?;
    let m = Manifest::parse(text, MANIFEST, true)?;
    let name = m.package.clone().expect("a package's manifest has a name");
    let version = m.version.clone().ok_or(format!("package {name}: its {MANIFEST} gives no \"version\"; a published package must"))?;
    let top = open_index(index, &name)?;
    let mut entries = read_entries(&top, &name)?;
    if let Some(e) = entries.iter().find(|e| e.version == version) {
        return Err(format!("package {name} {version} is already published (commit {} of {}); a published version never changes: publish a new version", &e.dep.commit[..12], e.dep.git));
    }
    let digest = files_digest(&hashes(&files));
    entries.push(Entry { version: version.clone(), dep, files_sha256: digest, yanked: false });
    commit_entries(&top, &name, &mut entries, &format!("Publish {name} {version}"))?;
    Ok(format!(
        "published {name} {version} (commit {} of {location}) to the registry in {}\n    authority: {}\nPush the registry for others to see it.",
        &commit[..12],
        top.display(),
        describe_authority(&m.authority)
    ))
}

/// `staalc yank`: marks a published version yanked (or not, with `undo`). A yanked
/// version is not chosen again, but a project that locked it keeps it.
pub fn yank(index: &Path, name: &str, version: &str, undo: bool) -> Result<String, String> {
    check_package_name(name, "--package")?;
    let v = Version::parse(version).map_err(|e| format!("--version: {e}"))?;
    let top = open_index(index, name)?;
    let mut entries = read_entries(&top, name)?;
    let e = entries.iter_mut().find(|e| e.version == v).ok_or(format!("the registry has no {name} {v}"))?;
    if e.yanked != undo {
        return Err(format!("{name} {v} is {}yanked already", if undo { "not " } else { "" }));
    }
    e.yanked = !undo;
    let what = if undo { "Unyank" } else { "Yank" };
    commit_entries(&top, name, &mut entries, &format!("{what} {name} {v}"))?;
    Ok(format!("{}yanked {name} {v}; push the registry for others to see it", if undo { "un" } else { "" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_inside_a_package_are_plain() {
        for ok in ["a.staal", "lib/sms.staal", "README.md"] {
            assert!(check_rel_path(ok).is_ok(), "{ok}");
        }
        for bad in ["", "/etc/passwd", "../x.staal", "a/../b", "a//b", ".git/config", "a/.GIT/x", "a\\b", "c:x", "./a"] {
            assert!(check_rel_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn commits_and_locations_are_exact() {
        assert!(check_commit(&"a".repeat(40), "x").is_ok());
        for bad in ["main", "v1.0", &"a".repeat(39), &"A".repeat(40), &"g".repeat(40)] {
            assert!(check_commit(bad, "x").is_err(), "{bad}");
        }
        for ok in ["https://github.com/a/b", "ssh://git@h/a", "git@github.com:a/b.git", "/srv/repos/sms", "file:///srv/sms"] {
            assert!(check_git(ok, "x").is_ok(), "{ok}");
        }
        for bad in ["http://github.com/a/b", "git://h/a", "-uhack", "ext::sh -c x", "ext::sh@h:x", "relative/path", "", "a@b:/x y"] {
            assert!(check_git(bad, "x").is_err(), "{bad}");
        }
    }

    #[test]
    fn manifests_are_checked() {
        let ok = r#"{"package": "sms", "authority": {"net.http.post": ["https://api.ng.termii.com/api/sms/send"]}}"#;
        assert!(Manifest::parse(ok, "m", true).is_ok());
        for (bad, why) in [
            (r#"{"authority": {}}"#, "\"package\" name"),
            (r#"{"package": "sms"}"#, "\"authority\""),
            (r#"{"package": "Sms", "authority": {}}"#, "not a package name"),
            (r#"{"package": "net", "authority": {}}"#, "belongs to the runtime"),
            (r#"{"package": "sms", "authority": {"net.http.post": []}}"#, "empty list"),
            (r#"{"package": "sms", "authority": {"net.http.post": ["https://x.org/../a"]}}"#, "authority.net.http.post[0]"),
            (r#"{"package": "sms", "authority": {"net.http.post": "all"}}"#, "expected \"any\""),
            (r#"{"package": "sms", "authority": {}, "extra": 1}"#, "unknown key"),
            (r#"{"package": "sms", "authority": {}, "dependencies": {"sms": {"git": "/r", "commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}"#, "itself"),
        ] {
            let e = Manifest::parse(bad, "m", true).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
    }
}
