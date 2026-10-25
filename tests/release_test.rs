// One version everywhere a release states it: staalc itself, the Python package, the
// VS Code extension, and the changelog's latest release.

use std::fs;
use std::path::Path;
use std::process::Command;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn read(p: &str) -> String {
    fs::read_to_string(Path::new(ROOT).join(p)).unwrap()
}

#[test]
fn every_part_of_a_release_has_staalcs_version() {
    let o = Command::new(env!("CARGO_BIN_EXE_staalc")).arg("--version").output().unwrap();
    assert_eq!((o.status.code(), String::from_utf8(o.stdout).unwrap()), (Some(0), format!("staalc {VERSION}\n")));
    let py = read("python/pyproject.toml");
    let py_version = py.lines().find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"')).unwrap();
    assert_eq!(py_version, VERSION, "python/pyproject.toml");
    let ext: serde_json::Value = serde_json::from_str(&read("editors/vscode/package.json")).unwrap();
    assert_eq!(ext["version"], VERSION, "editors/vscode/package.json");
    // The changelog: what is not released yet first, then the latest release, which
    // is this version, with its date.
    let log = read("CHANGELOG.md");
    let mut heads = log.lines().filter(|l| l.starts_with("## "));
    let first = heads.next().unwrap();
    let latest = if first == "## Unreleased" { heads.next().unwrap() } else { first };
    let (v, date) = latest.strip_prefix("## ").unwrap().split_once(" — ").expect("## VERSION — DATE");
    assert_eq!(v, VERSION, "the changelog's latest release");
    assert!(date.len() == 10 && date.chars().filter(|c| *c == '-').count() == 2, "{date}");
}
