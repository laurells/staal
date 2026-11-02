// Every package's tests (packages/<name>/tests/*.json) against its fake service, as
// packages/README.md ("Testing a package") describes, and the guide
// docs/PACKAGES.md followed as written: its files saved into an empty package folder,
// whose tests must pass. With `--features failpoints`, each test is also killed at
// every crash point and must end as the uninterrupted run did.
//
//   cargo test --test package_cases_test                      every package
//   STAAL_PACKAGE=messaging cargo test --test package_cases_test   one package

mod common;

use common::package_case::run_package;
use std::path::{Path, PathBuf};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn work(name: &str) -> PathBuf {
    std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let d = std::fs::canonicalize(env!("CARGO_TARGET_TMPDIR")).unwrap().join("package-cases").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn every_package_passes_its_tests() {
    let only = std::env::var("STAAL_PACKAGE").ok();
    let mut ran = Vec::new();
    let mut failures = Vec::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(Path::new(ROOT).join("packages")).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for pkg in dirs {
        let name = pkg.file_name().unwrap().to_string_lossy().into_owned();
        if only.as_ref().is_some_and(|o| *o != name) || !pkg.join("tests").is_dir() {
            continue;
        }
        match run_package(&pkg, &work(&name)) {
            Ok(n) => ran.push(format!("{name}: {n} test(s)")),
            Err(f) => failures.extend(f),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
    assert!(!ran.is_empty(), "no package has tests");
    println!("{}", ran.join("\n"));
}

#[test]
fn packages_written_for_the_ecosystem_have_tests() {
    // The packages written to the checklist in packages/README.md, and the template.
    for name in ["chat", "code", "email", "messaging", "template"] {
        assert!(Path::new(ROOT).join("packages").join(name).join("tests").is_dir(), "packages/{name} has no tests");
    }
}

/// The guide followed as written: every block marked `file=` is saved, relative to a
/// new package folder, and the package's tests must pass.
#[test]
fn the_package_guide_works_as_written() {
    let text = std::fs::read_to_string(Path::new(ROOT).join("docs/PACKAGES.md")).unwrap();
    let pkg = work("guide-package");
    let mut lines = text.lines();
    let mut saved = Vec::new();
    while let Some(line) = lines.next() {
        let Some(info) = line.strip_prefix("```") else { continue };
        let mut block = Vec::new();
        for l in lines.by_ref() {
            if l == "```" {
                break;
            }
            block.push(l);
        }
        if let Some(name) = info.split_whitespace().find_map(|w| w.strip_prefix("file=")) {
            assert!(!name.contains("..") && !name.starts_with('/'), "{name}");
            let p = pkg.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, block.join("\n") + "\n").unwrap();
            saved.push(name.to_string());
        }
    }
    assert!(saved.iter().any(|s| s == "staal.json") && saved.iter().any(|s| s.starts_with("tests/")), "{saved:?}");
    match run_package(&pkg, &work("guide-run")) {
        Ok(n) => assert!(n >= 2, "the guide has {n} test(s)"),
        Err(f) => panic!("\n{}", f.join("\n\n")),
    }
}
