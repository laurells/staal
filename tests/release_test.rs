// One version everywhere a release states it: staalc itself, the Python package, the
// Node package and its addon, the VS Code extension, and the changelog's latest release.

use std::fs;
use std::path::Path;
use std::process::Command;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn read(p: &str) -> String {
    fs::read_to_string(Path::new(ROOT).join(p)).unwrap()
}

/// Every release archive and wheel carries the licences of the code it contains: the
/// script that writes them covers each crate staalc links, and both packaging scripts
/// run it.
#[test]
fn a_release_carries_the_licences_of_what_it_contains() {
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("LICENSE-THIRD-PARTY.txt");
    let o = Command::new("python3").arg(Path::new(ROOT).join("scripts/third_party_licenses.py")).arg(&out).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = fs::read_to_string(&out).unwrap();
    let count: usize = text.lines().find_map(|l| l.strip_suffix(" crates.")?.parse().ok()).unwrap();
    let listed = text.lines().filter(|l| l.contains(": ") && !l.starts_with("---") && l.split(' ').nth(1).is_some_and(|v| v.ends_with(':') && v.chars().next().is_some_and(|c| c.is_ascii_digit()))).count();
    assert_eq!(listed, count, "every crate is listed once");
    for name in ["rustls ", "ring ", "rusqlite ", "libsqlite3-sys ", "cranelift-codegen ", "postgres ", "ureq ", "serde_json ", "sha2 "] {
        assert!(text.lines().any(|l| l.starts_with(name)), "{name}is missing");
    }
    assert!(text.contains("Permission is hereby granted") && text.contains("Apache License"), "the licence texts themselves");
    for script in ["scripts/package.sh", "scripts/wheel.sh", "scripts/node_package.sh"] {
        assert!(read(script).contains("third_party_licenses.py"), "{script} does not include the licences");
    }
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
    let node: serde_json::Value = serde_json::from_str(&read("node/package.json")).unwrap();
    assert_eq!(node["version"], VERSION, "node/package.json");
    let addon = read("node/Cargo.toml");
    let addon_version = addon.lines().find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"')).unwrap();
    assert_eq!(addon_version, VERSION, "node/Cargo.toml");
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
