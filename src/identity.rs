// The source program fingerprint, `staal-program-v3` (RUNTIME.md section 11): the
// text a journal binds by its SHA-256, which changes whenever anything changes that
// can change what a program executes or what authority it can exercise.
//
// It is built from netstrings: `ns(b)` is the length of `b` in bytes in decimal ASCII
// (no sign, no leading zeros, "0" for empty), then `:`, then `b`, then `,`. Every field
// is a netstring, so the whole is unambiguous: no two different inputs give the same
// text. Every input is UTF-8 (Staal source, JSON files, paths, names, hex digests), so
// the fingerprint is UTF-8 text too. The grammar, with groups in this order:
//
//   fingerprint = ns("staal-program-v3") modules project
//   modules     = ns("modules") ns(count) { ns(module-path) ns(text) }
//   project     = ns("project") ( ns("none") | ns("some") ns(staal.json) lock packages )
//   lock        = ns("lock") ( ns("none") | ns("some") ns(staal.lock) )
//   packages    = ns("packages") ns(count) { ns(name) ns(git) ns(commit) ns(count) { ns(path) ns(sha256) } }
//
// `count` is a decimal number of the items that follow. Modules are sorted by module
// path, packages by name and a package's files by path, each comparing the bytes of
// their UTF-8, never by locale. A module path is its segments joined by `.` (segments
// are identifiers, so no segment holds a `.`); the entry file's is empty. A package's
// files and their SHA-256 (64 lowercase hexadecimal digits) are those `staal.lock`
// records and loading verified, so content is bound to the path it is at. Files are
// read as bytes and must be UTF-8; nothing is normalized (no Unicode normalization, no
// line-ending change).
//
// A change to the inputs, the order, the path rules or the encoding takes a new label
// and a new journal version (RUNTIME.md section 11). `scripts/fingerprint.py` is a
// second implementation, written from this grammar alone; the test vectors in
// `tests/fixtures/fingerprint/` are its output, and the tests below check this one
// gives the same.

/// The label that starts every fingerprint of this construction.
pub const LABEL: &str = "staal-program-v3";

/// One package of a project, as `staal.lock` records it and loading verified it.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageId {
    pub name: String,
    pub git: String,
    pub commit: String,
    /// Every file of the package: its path in the package and its SHA-256 as 64
    /// lowercase hexadecimal digits.
    pub files: Vec<(String, String)>,
}

/// What a project contributes: its manifest, its lockfile if it has one, and its
/// packages.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectId {
    pub manifest: String,
    pub lock: Option<String>,
    pub packages: Vec<PackageId>,
}

fn ns(out: &mut String, field: &str) {
    out.push_str(&field.len().to_string());
    out.push(':');
    out.push_str(field);
    out.push(',');
}

/// The `project` group: what follows the modules. A program checked without a project
/// (a single file with no `staal.json` beside it) has `None`.
pub fn project_part(project: Option<&ProjectId>) -> String {
    let mut out = String::new();
    ns(&mut out, "project");
    let Some(p) = project else {
        ns(&mut out, "none");
        return out;
    };
    ns(&mut out, "some");
    ns(&mut out, &p.manifest);
    ns(&mut out, "lock");
    match &p.lock {
        None => ns(&mut out, "none"),
        Some(l) => {
            ns(&mut out, "some");
            ns(&mut out, l);
        }
    }
    ns(&mut out, "packages");
    let mut packages: Vec<&PackageId> = p.packages.iter().collect();
    packages.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    ns(&mut out, &packages.len().to_string());
    for pkg in packages {
        ns(&mut out, &pkg.name);
        ns(&mut out, &pkg.git);
        ns(&mut out, &pkg.commit);
        let mut files: Vec<&(String, String)> = pkg.files.iter().collect();
        files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        ns(&mut out, &files.len().to_string());
        for (path, sha) in files {
            ns(&mut out, path);
            ns(&mut out, sha);
        }
    }
    out
}

/// The whole fingerprint: the label, the modules (path and text, in any order: they are
/// sorted here) and the project part from `project_part`.
pub fn fingerprint(modules: &[(String, &str)], project_part: &str) -> String {
    let mut sorted: Vec<&(String, &str)> = modules.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut out = String::new();
    ns(&mut out, LABEL);
    ns(&mut out, "modules");
    ns(&mut out, &sorted.len().to_string());
    for (path, text) in sorted {
        ns(&mut out, path);
        ns(&mut out, text);
    }
    out.push_str(project_part);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as Json;

    fn project_of(j: &Json) -> Option<ProjectId> {
        if j.is_null() {
            return None;
        }
        let packages = j["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| PackageId {
                name: p["name"].as_str().unwrap().into(),
                git: p["git"].as_str().unwrap().into(),
                commit: p["commit"].as_str().unwrap().into(),
                files: p["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| (f[0].as_str().unwrap().into(), f[1].as_str().unwrap().into()))
                    .collect(),
            })
            .collect();
        Some(ProjectId {
            manifest: j["manifest"].as_str().unwrap().into(),
            lock: j["lock"].as_str().map(str::to_string),
            packages,
        })
    }

    /// Each vector's inputs give the fingerprint, and its SHA-256, that the second
    /// implementation (`scripts/fingerprint.py`) computed from the grammar.
    #[test]
    fn the_test_vectors_of_the_second_implementation() {
        let v: Json = serde_json::from_str(include_str!("../tests/fixtures/fingerprint/vectors.json")).unwrap();
        let cases = v["vectors"].as_array().unwrap();
        assert!(cases.len() >= 10, "too few vectors");
        let mut digests = std::collections::BTreeSet::new();
        for c in cases {
            let modules: Vec<(String, &str)> = c["modules"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| (m[0].as_str().unwrap().to_string(), m[1].as_str().unwrap()))
                .collect();
            let text = fingerprint(&modules, &project_part(project_of(&c["project"]).as_ref()));
            let name = c["name"].as_str().unwrap();
            assert_eq!(text, c["fingerprint"].as_str().unwrap(), "{name}");
            assert_eq!(crate::interp::journal::sha256_hex(text.as_bytes()), c["sha256"].as_str().unwrap(), "{name}");
            assert!(digests.insert(c["sha256"].as_str().unwrap().to_string()), "{name} gives the same digest as another vector");
        }
    }

    #[test]
    fn module_order_does_not_matter_and_every_byte_does() {
        let a = fingerprint(&[("".into(), "fn main() {}"), ("m".into(), "fn f() {}")], &project_part(None));
        let b = fingerprint(&[("m".into(), "fn f() {}"), ("".into(), "fn main() {}")], &project_part(None));
        assert_eq!(a, b);
        let c = fingerprint(&[("".into(), "fn main() {} "), ("m".into(), "fn f() {}")], &project_part(None));
        assert_ne!(a, c);
        // A path and the text beside it cannot trade characters.
        let d = fingerprint(&[("ab".into(), "c")], &project_part(None));
        let e = fingerprint(&[("a".into(), "bc")], &project_part(None));
        assert_ne!(d, e);
    }
}
