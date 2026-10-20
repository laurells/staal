// Packages (roadmap phase 4): libraries from outside the program's folder.
//
// A project is the folder of the entry file. If it holds `staal.json`, its
// `dependencies` name packages by git URL and exact commit:
//
//   { "dependencies": { "sms": { "git": "https://github.com/x/sms", "commit": "<40 hex>" } } }
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dep {
    pub git: String,
    pub commit: String,
}

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub package: Option<String>,
    pub version: Option<String>,
    pub authority: BTreeMap<String, Authority>,
    pub dependencies: BTreeMap<String, Dep>,
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
    pub deps: BTreeMap<String, Dep>,
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
    if name == "net" || name == "fs" {
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

fn parse_deps(v: Option<&Json>, at: &str) -> Result<BTreeMap<String, Dep>, String> {
    let mut out = BTreeMap::new();
    let Some(v) = v else { return Ok(out) };
    let obj = v.as_object().ok_or(format!("{at}.dependencies: expected an object"))?;
    for (name, d) in obj {
        let dat = format!("{at}.dependencies.{name}");
        check_package_name(name, &dat)?;
        let d = d.as_object().ok_or(format!("{dat}: expected {{\"git\": ..., \"commit\": ...}}"))?;
        for k in d.keys() {
            if k != "git" && k != "commit" {
                return Err(format!("{dat}: unknown key '{k}'"));
            }
        }
        let git = d.get("git").and_then(Json::as_str).ok_or(format!("{dat}.git: expected a string"))?;
        let commit = d.get("commit").and_then(Json::as_str).ok_or(format!("{dat}.commit: expected a string"))?;
        check_git(git, &format!("{dat}.git"))?;
        check_commit(commit, &format!("{dat}.commit"))?;
        out.insert(name.clone(), Dep { git: git.to_string(), commit: commit.to_string() });
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
            if !["package", "version", "authority", "dependencies"].contains(&k.as_str()) {
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
            Some(Json::String(s)) => Some(s.clone()),
            Some(_) => return Err(format!("{at}.version: expected a string")),
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
        Ok(Manifest { package, version, authority, dependencies })
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
    pub files: BTreeMap<String, String>,
}

impl Lock {
    pub fn parse(text: &str, at: &str) -> Result<Lock, String> {
        let v: Json = serde_json::from_str(text).map_err(|e| format!("{at}: not valid JSON: {e}"))?;
        let obj = v.as_object().ok_or(format!("{at}: expected an object"))?;
        if obj.get("staal_lock") != Some(&json!(1)) {
            return Err(format!("{at}: not a staal.lock of version 1"));
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
                if !["git", "commit", "files"].contains(&k.as_str()) {
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
            packages.insert(name.clone(), LockedPackage { dep: Dep { git: git.into(), commit: commit.into() }, files });
        }
        Ok(Lock { packages })
    }

    pub fn to_text(&self) -> String {
        let mut ps = Map::new();
        for (name, p) in &self.packages {
            let files: Map<String, Json> = p.files.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            ps.insert(name.clone(), json!({ "git": p.dep.git, "commit": p.dep.commit, "files": files }));
        }
        let mut text = serde_json::to_string_pretty(&json!({ "staal_lock": 1, "packages": ps })).expect("serialisable");
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
            let mut modules = BTreeMap::new();
            for (f, bytes) in &tree {
                if let Some(stem) = f.strip_suffix(".staal") {
                    let text = String::from_utf8(bytes.clone()).map_err(|_| format!("package {name}: {f} is not UTF-8"))?;
                    modules.insert(stem.to_string(), text);
                }
            }
            packages.insert(name.clone(), Package { name: name.clone(), dep: lp.dep.clone(), manifest: m, modules });
        }
        // The lock must be exactly the dependency graph: every package wanted at the
        // commit it is wanted at, and nothing else.
        let mut wanted: BTreeMap<String, (Dep, String)> = BTreeMap::new();
        let mut queue: Vec<(String, Dep, String)> =
            manifest.dependencies.iter().map(|(n, d)| (n.clone(), d.clone(), "the project".to_string())).collect();
        while let Some((n, d, by)) = queue.pop() {
            if let Some((have, first)) = wanted.get(&n) {
                if *have != d {
                    return Err(format!("{first} and {by} want different versions of package {n}; one version per package"));
                }
                continue;
            }
            let p = packages
                .get(&n)
                .ok_or(format!("{LOCKFILE} has no package {n} (wanted by {by}); run `staalc fetch`"))?;
            if p.dep != d {
                return Err(format!("{LOCKFILE} has package {n} at {}, but {by} wants {}; run `staalc fetch`", p.dep.commit, d.commit));
            }
            for (dn, dd) in &p.manifest.dependencies {
                queue.push((dn.clone(), dd.clone(), format!("package {n}")));
            }
            wanted.insert(n, (d, by));
        }
        for n in packages.keys() {
            if !wanted.contains_key(n) {
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
    format!("{} {} at {}: {}", p.name, p.dep.git, &p.dep.commit[..12], describe_authority(&p.manifest.authority))
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

/// The files of one commit, read from git's objects.
fn fetch_commit(name: &str, dep: &Dep, scratch: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let repo = scratch.join(format!("{name}.git"));
    let _ = std::fs::remove_dir_all(&repo);
    let url = dep.git.strip_prefix("file://").unwrap_or(&dep.git);
    git(None, &["clone", "--bare", "--quiet", "--", url, repo.to_str().ok_or("scratch path is not UTF-8")?])
        .map_err(|e| format!("package {name}: cannot fetch {}: {e}", dep.git))?;
    let commit_ok = git(Some(&repo), &["cat-file", "-t", &dep.commit]).map(|t| t == b"commit\n").unwrap_or(false);
    if !commit_ok {
        return Err(format!("package {name}: {} has no commit {}", dep.git, dep.commit));
    }
    let listing = git(Some(&repo), &["ls-tree", "-r", "-z", "--full-tree", &dep.commit])?;
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
        let bytes = git(Some(&repo), &["cat-file", "blob", oid])?;
        total += bytes.len() as u64;
        if files.len() >= MAX_FILES || total > MAX_BYTES {
            return Err(format!("package {name} holds more than {MAX_FILES} files or {MAX_BYTES} bytes"));
        }
        files.insert(path.to_string(), bytes);
    }
    let _ = std::fs::remove_dir_all(&repo);
    Ok(files)
}

fn write_tree(dir: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<(), String> {
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

fn hashes(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, String> {
    files.iter().map(|(k, v)| (k.clone(), sha256_hex(v))).collect()
}

type Fetched = (BTreeMap<String, Vec<u8>>, Manifest, String);

/// `staalc fetch`: fetches every dependency at its commit, writes the packages and
/// `staal.lock`, and reports each package and its authority. A package already in the
/// lock at the same commit must have exactly the files the lock records.
pub fn fetch(root: &Path) -> Result<Vec<String>, String> {
    let mpath = root.join(MANIFEST);
    let mtext = std::fs::read_to_string(&mpath).map_err(|e| format!("cannot read {}: {e}", mpath.display()))?;
    let manifest = Manifest::parse(&mtext, MANIFEST, false)?;
    let old = match std::fs::read_to_string(root.join(LOCKFILE)) {
        Ok(t) => Some(Lock::parse(&t, LOCKFILE)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {LOCKFILE}: {e}")),
    };
    let store = root.join(STORE);
    let scratch = root.join(".staal/scratch");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;

    let mut lock = Lock::default();
    // Per package: its files, its manifest, and who first asked for it.
    let mut fetched: BTreeMap<String, Fetched> = BTreeMap::new();
    let mut queue: Vec<(String, Dep, String)> =
        manifest.dependencies.iter().rev().map(|(n, d)| (n.clone(), d.clone(), "the project".to_string())).collect();
    while let Some((name, dep, by)) = queue.pop() {
        if let Some(have) = lock.packages.get(&name) {
            if have.dep != dep {
                let first = &fetched[&name].2;
                return Err(format!("{first} and {by} want different versions of package {name}; one version per package"));
            }
            continue;
        }
        // Always read from git, and compare with the lock: what is on disk, and the lock
        // itself, may have been edited; the commit cannot be.
        let locked = old.as_ref().and_then(|l| l.packages.get(&name)).filter(|l| l.dep == dep);
        let files = fetch_commit(&name, &dep, &scratch)?;
        if let Some(l) = locked
            && hashes(&files) != l.files
        {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(format!(
                "package {name} at {} does not have the files {LOCKFILE} records: the lock was edited, or the repository's history rewritten. Nothing was changed",
                dep.commit
            ));
        }
        let mtext = files.get(MANIFEST).ok_or(format!("package {name} has no {MANIFEST} at its root"))?;
        let mtext = std::str::from_utf8(mtext).map_err(|_| format!("package {name}: {MANIFEST} is not UTF-8"))?;
        let m = Manifest::parse(mtext, &format!("package {name}: {MANIFEST}"), true)?;
        if m.package.as_deref() != Some(name.as_str()) {
            return Err(format!("{by} calls it {name}, but its {MANIFEST} names it '{}'", m.package.clone().unwrap_or_default()));
        }
        for (dn, dd) in m.dependencies.iter().rev() {
            queue.push((dn.clone(), dd.clone(), format!("package {name}")));
        }
        lock.packages.insert(name.clone(), LockedPackage { dep, files: hashes(&files) });
        fetched.insert(name, (files, m, by));
    }

    // Everything fetched and checked: now replace the store and the lock.
    std::fs::create_dir_all(&store).map_err(|e| format!("cannot create {}: {e}", store.display()))?;
    for (name, (files, _, _)) in &fetched {
        let dir = store.join(name);
        if read_tree(&dir).ok().is_some_and(|t| verify(name, &t, &lock.packages[name].files).is_ok()) {
            continue;
        }
        let tmp = scratch.join(format!("{name}.new"));
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
    let ltmp = scratch.join("staal.lock.new");
    std::fs::write(&ltmp, lock.to_text()).map_err(|e| format!("cannot write {LOCKFILE}: {e}"))?;
    std::fs::rename(&ltmp, root.join(LOCKFILE)).map_err(|e| format!("cannot write {LOCKFILE}: {e}"))?;
    let _ = std::fs::remove_dir_all(&scratch);

    let mut report = Vec::new();
    for (name, (_, m, _)) in &fetched {
        let dep = &lock.packages[name].dep;
        let before = old.as_ref().and_then(|l| l.packages.get(name));
        let note = match before {
            None => "new".to_string(),
            Some(b) if b.dep == dep.clone() => "unchanged".to_string(),
            Some(b) => format!("was {}", &b.dep.commit[..12]),
        };
        report.push(format!(
            "{name} {} at {} ({note})\n    authority: {}",
            dep.git,
            &dep.commit[..12],
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
    Ok(report)
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
