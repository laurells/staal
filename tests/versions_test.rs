// Version ranges and registries (phase 12): a dependency on a range of a repository's
// release tags or of a registry's versions resolves to the newest version that fits
// everything that asks for it, is locked, and stays locked until `staalc update`; a
// moved tag or registry entry, a conflict, or files that are not what the registry
// recorded stop the fetch with nothing changed.
//
// Repositories and registries are made here with git and fetched by path.

use serde_json::{json, Value as Json};
use staalc::packages::{self, Update};
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");

fn base(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("versions").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(d).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Commits `files` to the repository at `dir` (made if needed); returns the commit.
fn commit(dir: &Path, files: &[(&str, &str)]) -> String {
    std::fs::create_dir_all(dir).unwrap();
    if !dir.join(".git").exists() {
        git(dir, &["init", "-q"]);
    }
    for (f, text) in files {
        std::fs::write(dir.join(f), text).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c", "--allow-empty"]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A release of package `name`: its manifest at `version`, a module whose function
/// returns `n`, committed and tagged `v<version>` (annotated when `annotated`).
fn release(dir: &Path, name: &str, version: &str, n: i64, deps: Json, annotated: bool) -> String {
    let m = json!({ "package": name, "version": version, "authority": {}, "dependencies": deps }).to_string();
    let c = commit(dir, &[("staal.json", &m), ("lib.staal", &format!("pub fn n() -> Int {{\n    return {n}\n}}\n"))]);
    if annotated {
        git(dir, &["tag", "-a", &format!("v{version}"), "-m", "release"]);
    } else {
        git(dir, &["tag", &format!("v{version}")]);
    }
    c
}

fn app(dir: &Path, manifest: Json, uses: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("staal.json"), manifest.to_string()).unwrap();
    std::fs::write(dir.join("main.staal"), format!("import {uses}.lib\nfn go() -> Int {{\n    return lib.n()\n}}\n")).unwrap();
    dir.join("main.staal")
}

fn locked(app: &Path) -> Json {
    serde_json::from_str(&std::fs::read_to_string(app.join("staal.lock")).unwrap()).unwrap()
}

fn run(main: &Path, engine: &str) -> (Option<i32>, String, String) {
    let cfg = main.with_file_name("cfg.json");
    std::fs::write(&cfg, json!({ "entry": "go", "model": { "provider": "script", "responses": [] } }).to_string()).unwrap();
    let o = Command::new(BIN).args(["run", main.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--engine", engine]).output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
}

fn engines() -> Vec<&'static str> {
    if cfg!(feature = "native") { vec!["interpreter", "native"] } else { vec!["interpreter"] }
}

fn staalc(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(BIN)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
}

#[test]
fn a_range_takes_the_newest_release_that_fits_and_keeps_it() {
    let b = base("tags");
    let fmt = b.join("repos/fmt");
    release(&fmt, "fmt", "1.0.0", 10, json!({}), false);
    let v11 = release(&fmt, "fmt", "1.1.0", 11, json!({}), true);
    release(&fmt, "fmt", "1.2.0-beta.1", 12, json!({}), false);
    release(&fmt, "fmt", "2.0.0", 20, json!({}), false);
    // Not a release: ignored.
    git(&fmt, &["tag", "latest"]);
    let a = b.join("app");
    let main = app(&a, json!({ "dependencies": { "fmt": { "git": fmt.to_str().unwrap(), "version": "^1.0" } } }), "fmt");
    let report = packages::fetch(&a).unwrap().join("\n");
    assert!(report.contains("fmt 1.1.0 ") && report.contains("(new)"), "{report}");
    let l = locked(&a);
    assert_eq!(l["staal_lock"], 2);
    assert_eq!((l["packages"]["fmt"]["version"].as_str(), l["packages"]["fmt"]["commit"].as_str()), (Some("1.1.0"), Some(v11.as_str())));
    for engine in engines() {
        assert_eq!(run(&main, engine), (Some(0), "11".to_string(), String::new()), "{engine}");
    }
    // A newer release that fits: fetch keeps the locked one, update takes the new one.
    release(&fmt, "fmt", "1.4.0", 14, json!({}), false);
    let report = packages::fetch(&a).unwrap().join("\n");
    assert!(report.contains("fmt 1.1.0 ") && report.contains("(unchanged)"), "{report}");
    let report = packages::fetch_with(&a, Update::All).unwrap().join("\n");
    assert!(report.contains("fmt 1.4.0 ") && report.contains("(was 1.1.0, commit"), "{report}");
    for engine in engines() {
        assert_eq!(run(&main, engine).1, "14", "{engine}");
    }
    // A range that excludes the locked version moves it, even on fetch.
    std::fs::write(a.join("staal.json"), json!({ "dependencies": { "fmt": { "git": fmt.to_str().unwrap(), "version": "~1.1" } } }).to_string()).unwrap();
    let e = staalc::check_path(&main).err().unwrap_or_default();
    assert!(e.contains("1.4.0 is not within ~1.1") && e.contains("run `staalc fetch`"), "{e}");
    assert!(packages::fetch(&a).unwrap().join("\n").contains("fmt 1.1.0 "));
    // Updating one package names it.
    assert!(packages::fetch_with(&a, Update::Package("nope")).unwrap_err().contains("no package nope"));
    assert!(packages::fetch_with(&a, Update::Package("fmt")).unwrap().join("\n").contains("fmt 1.1.0 "));
    // The lock's version must be the package's own.
    let lock = std::fs::read_to_string(a.join("staal.lock")).unwrap();
    std::fs::write(a.join("staal.lock"), lock.replace("\"1.1.0\"", "\"1.1.5\"")).unwrap();
    let e = staalc::check_path(&main).err().unwrap_or_default();
    assert!(e.contains("gives another version"), "{e}");
}

#[test]
fn a_moved_tag_or_a_tag_that_disagrees_is_refused() {
    let b = base("moved");
    let fmt = b.join("repos/fmt");
    release(&fmt, "fmt", "1.0.0", 1, json!({}), false);
    let a = b.join("app");
    app(&a, json!({ "dependencies": { "fmt": { "git": fmt.to_str().unwrap(), "version": "1" } } }), "fmt");
    packages::fetch(&a).unwrap();
    let before = std::fs::read_to_string(a.join("staal.lock")).unwrap();
    // The tag is moved to another commit with the same version.
    commit(&fmt, &[("lib.staal", "pub fn n() -> Int {\n    return 666\n}\n")]);
    git(&fmt, &["tag", "-f", "v1.0.0"]);
    let e = packages::fetch(&a).unwrap_err();
    assert!(e.contains("package fmt 1.0.0 is commit") && e.contains("its release tag was moved") && e.contains("Nothing was changed"), "{e}");
    assert_eq!(std::fs::read_to_string(a.join("staal.lock")).unwrap(), before);
    // Update takes the moved tag, deliberately.
    packages::fetch_with(&a, Update::All).unwrap();
    // A tag whose manifest gives another version.
    let m = json!({ "package": "fmt", "version": "1.4.0", "authority": {} }).to_string();
    commit(&fmt, &[("staal.json", &m)]);
    git(&fmt, &["tag", "v1.5.0"]);
    let e = packages::fetch_with(&a, Update::All).unwrap_err();
    assert!(e.contains("package fmt 1.5.0") && e.contains("gives version 1.4.0"), "{e}");
    // Two tags of one version naming different commits.
    git(&fmt, &["tag", "1.5.0", "HEAD~1"]);
    let e = packages::fetch_with(&a, Update::All).unwrap_err();
    assert!(e.contains("are both version 1.5.0"), "{e}");
}

#[test]
fn every_range_must_fit_one_version() {
    let b = base("conflict");
    let repos = b.join("repos");
    let fmt = repos.join("fmt");
    for (v, n) in [("1.0.0", 1), ("1.3.0", 13), ("2.0.0", 2)] {
        release(&fmt, "fmt", v, n, json!({}), false);
    }
    let wants = |range: &str| json!({ "fmt": { "git": fmt.to_str().unwrap(), "version": range } });
    release(&repos.join("one"), "one", "1.0.0", 0, wants("^1"), false);
    release(&repos.join("two"), "two", "1.0.0", 0, wants(">=1.2"), false);
    release(&repos.join("three"), "three", "1.0.0", 0, wants("^2"), false);
    let dep = |n: &str| json!({ "git": repos.join(n).to_str().unwrap(), "version": "1" });
    // Two ranges that share a version: the newest in both.
    let a = b.join("shared");
    app(&a, json!({ "dependencies": { "one": dep("one"), "two": dep("two") } }), "one");
    packages::fetch(&a).unwrap();
    assert_eq!(locked(&a)["packages"]["fmt"]["version"], "1.3.0");
    // Two that share none.
    let c = b.join("conflict");
    app(&c, json!({ "dependencies": { "one": dep("one"), "three": dep("three") } }), "one");
    let e = packages::fetch(&c).unwrap_err();
    assert!(e.contains("no version of package fmt fits") && e.contains("package one 1.0.0 wants version ^1 of") && e.contains("package three 1.0.0 wants version ^2 of"), "{e}");
    assert!(e.contains("Versions: 2.0.0, 1.3.0, 1.0.0"), "{e}");
    assert!(!c.join("staal.lock").exists());
    // An exact commit and a range: the commit's own version must fit.
    let first = git(&fmt, &["rev-parse", "v1.0.0^{commit}"]);
    let d = b.join("commit");
    let exact = json!({ "git": fmt.to_str().unwrap(), "commit": first });
    app(&d, json!({ "dependencies": { "fmt": exact, "one": dep("one") } }), "fmt");
    packages::fetch(&d).unwrap();
    assert_eq!(locked(&d)["packages"]["fmt"]["version"], "1.0.0");
    app(&d, json!({ "dependencies": { "fmt": exact, "two": dep("two") } }), "fmt");
    let e = packages::fetch(&d).unwrap_err();
    assert!(e.contains("that commit is version 1.0.0"), "{e}");
}

#[test]
fn a_pre_release_is_taken_only_when_asked_for() {
    let b = base("pre");
    let fmt = b.join("repos/fmt");
    release(&fmt, "fmt", "1.0.0", 1, json!({}), false);
    release(&fmt, "fmt", "1.1.0-rc.1", 2, json!({}), false);
    let a = b.join("app");
    let manifest = |r: &str| json!({ "dependencies": { "fmt": { "git": fmt.to_str().unwrap(), "version": r } } });
    app(&a, manifest("^1"), "fmt");
    packages::fetch(&a).unwrap();
    assert_eq!(locked(&a)["packages"]["fmt"]["version"], "1.0.0");
    app(&a, manifest("^1.1.0-rc.1"), "fmt");
    packages::fetch(&a).unwrap();
    assert_eq!(locked(&a)["packages"]["fmt"]["version"], "1.1.0-rc.1");
}

#[test]
fn manifests_say_what_they_want_one_way() {
    let b = base("manifests");
    let a = b.join("app");
    for (deps, why) in [
        (json!({ "fmt": { "git": "/r", "commit": "a".repeat(40), "version": "1" } }), "a commit or a version, not both"),
        (json!({ "fmt": { "commit": "a".repeat(40) } }), "needs the git location"),
        (json!({ "fmt": { "git": "/r" } }), "give the commit, or a version range"),
        (json!({ "fmt": {} }), "give a version range"),
        (json!({ "fmt": { "version": "1.2.3 1.4" } }), "separate comparators with commas"),
        (json!({ "fmt": { "version": "^1", "registry": "/r" } }), "unknown key 'registry'"),
        (json!({ "db": { "version": "1" } }), "belongs to the runtime"),
        (json!({ "ffi": { "version": "1" } }), "belongs to the runtime"),
    ] {
        app(&a, json!({ "dependencies": deps }), "fmt");
        let e = packages::fetch(&a).unwrap_err();
        assert!(e.contains(why), "{deps}: {e}");
    }
    app(&a, json!({ "registry": "http://example.com/r", "dependencies": {} }), "fmt");
    assert!(packages::fetch(&a).unwrap_err().contains("registry"));
    // A package cannot choose the registry, and its version must be a version.
    let r = b.join("repos/p");
    let c = commit(&r, &[("staal.json", r#"{"package": "p", "authority": {}, "registry": "/x"}"#)]);
    app(&a, json!({ "dependencies": { "p": { "git": r.to_str().unwrap(), "commit": c } } }), "p");
    assert!(packages::fetch(&a).unwrap_err().contains("the project chooses the registry"));
    let c = commit(&r, &[("staal.json", r#"{"package": "p", "version": "1.0", "authority": {}}"#)]);
    app(&a, json!({ "dependencies": { "p": { "git": r.to_str().unwrap(), "commit": c } } }), "p");
    assert!(packages::fetch(&a).unwrap_err().contains("is not a version"));
}

/// A registry with fmt 1.0.0 and 1.1.0 published by `staalc publish`.
fn registry(b: &Path) -> (PathBuf, PathBuf, String, String) {
    let reg = b.join("registry");
    std::fs::create_dir_all(&reg).unwrap();
    git(&reg, &["init", "-q"]);
    let fmt = b.join("repos/fmt");
    let c1 = release(&fmt, "fmt", "1.0.0", 10, json!({}), false);
    let c2 = release(&fmt, "fmt", "1.1.0", 11, json!({}), false);
    for c in [&c1, &c2] {
        let (code, out, err) = staalc(&["publish", "--registry", reg.to_str().unwrap(), "--git", fmt.to_str().unwrap(), "--commit", c]);
        assert_eq!(code, Some(0), "{err}");
        assert!(out.contains("published fmt") && out.contains("Push the registry"), "{out}");
    }
    (reg, fmt, c1, c2)
}

#[test]
fn a_registry_dependency_is_fetched_verified_and_locked() {
    let b = base("registry");
    let (reg, fmt, c1, c2) = registry(&b);
    let index: Json = serde_json::from_str(&std::fs::read_to_string(reg.join("packages/fmt.json")).unwrap()).unwrap();
    assert_eq!(index["versions"].as_array().unwrap().len(), 2);
    assert_eq!(git(&reg, &["status", "--porcelain"]), "", "publish commits to the registry");
    let a = b.join("app");
    let main = app(&a, json!({ "registry": reg.to_str().unwrap(), "dependencies": { "fmt": { "version": "^1" } } }), "fmt");
    let report = packages::fetch(&a).unwrap().join("\n");
    assert!(report.contains("fmt 1.1.0 from the registry:"), "{report}");
    let l = locked(&a);
    assert_eq!((l["packages"]["fmt"]["commit"].as_str(), l["packages"]["fmt"]["registry"].as_str()), (Some(c2.as_str()), reg.to_str()));
    for engine in engines() {
        assert_eq!(run(&main, engine).1, "11", "{engine}");
    }
    // A version is published once.
    let (code, _, err) = staalc(&["publish", "--registry", reg.to_str().unwrap(), "--git", fmt.to_str().unwrap(), "--commit", &c2]);
    assert_eq!(code, Some(2));
    assert!(err.contains("already published") && err.contains("never changes"), "{err}");
    // Yanked: not chosen again, but a project that locked it keeps it.
    let (code, out, err) = staalc(&["yank", "--registry", reg.to_str().unwrap(), "--package", "fmt", "--version", "1.1.0"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("yanked fmt 1.1.0"));
    assert!(packages::fetch(&a).unwrap().join("\n").contains("fmt 1.1.0 "));
    let fresh = b.join("fresh");
    app(&fresh, json!({ "registry": reg.to_str().unwrap(), "dependencies": { "fmt": { "version": "^1" } } }), "fmt");
    packages::fetch(&fresh).unwrap();
    assert_eq!(locked(&fresh)["packages"]["fmt"]["commit"], c1);
    assert!(staalc(&["yank", "--registry", reg.to_str().unwrap(), "--package", "fmt", "--version", "1.1.0"]).2.contains("yanked already"));
    assert_eq!(staalc(&["yank", "--registry", reg.to_str().unwrap(), "--package", "fmt", "--version", "1.1.0", "--undo"]).0, Some(0));
    packages::fetch_with(&fresh, Update::All).unwrap();
    assert_eq!(locked(&fresh)["packages"]["fmt"]["commit"], c2);
    // A registry dependency needs a registry, and a git and a registry source do not mix.
    app(&fresh, json!({ "dependencies": { "fmt": { "version": "^1" } } }), "fmt");
    assert!(packages::fetch(&fresh).unwrap_err().contains("names no \"registry\""));
    let one = b.join("repos/one");
    release(&one, "one", "1.0.0", 0, json!({ "fmt": { "git": fmt.to_str().unwrap(), "version": "1" } }), false);
    app(&fresh, json!({ "registry": reg.to_str().unwrap(), "dependencies": { "fmt": { "version": "^1" }, "one": { "git": one.to_str().unwrap(), "version": "1" } } }), "fmt");
    assert!(packages::fetch(&fresh).unwrap_err().contains("from the registry or from git, not both"));
}

#[test]
fn a_registry_cannot_change_what_a_version_is() {
    let b = base("registry-tampered");
    let (reg, _fmt, c1, _) = registry(&b);
    let a = b.join("app");
    app(&a, json!({ "registry": reg.to_str().unwrap(), "dependencies": { "fmt": { "version": "=1.0.0" } } }), "fmt");
    packages::fetch(&a).unwrap();
    let before = std::fs::read_to_string(a.join("staal.lock")).unwrap();
    let edit = |f: &dyn Fn(&mut Json)| {
        let p = reg.join("packages/fmt.json");
        let mut v: Json = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        f(&mut v);
        std::fs::write(&p, v.to_string()).unwrap();
        git(&reg, &["commit", "-q", "-am", "edit"]);
    };
    // The entry for 1.0.0 now names another commit.
    let other = git(&b.join("repos/fmt"), &["rev-parse", "v1.1.0^{commit}"]);
    edit(&|v| v["versions"][0]["commit"] = json!(other));
    let e = packages::fetch(&a).unwrap_err();
    assert!(e.contains("the registry's entry for it was moved"), "{e}");
    assert_eq!(std::fs::read_to_string(a.join("staal.lock")).unwrap(), before);
    // Its digest no longer matches the files.
    edit(&|v| {
        v["versions"][0]["commit"] = json!(c1);
        v["versions"][0]["files_sha256"] = json!("0".repeat(64));
    });
    std::fs::remove_file(a.join("staal.lock")).unwrap();
    let e = packages::fetch(&a).unwrap_err();
    assert!(e.contains("not the files the registry recorded"), "{e}");
    assert!(!a.join("staal.lock").exists());
    // A repository that is not a registry.
    let not = b.join("not-a-registry");
    commit(&not, &[("README", "x")]);
    app(&a, json!({ "registry": not.to_str().unwrap(), "dependencies": { "fmt": { "version": "1" } } }), "fmt");
    assert!(packages::fetch(&a).unwrap_err().contains("is not a Staal registry"));
    // A package the registry does not have.
    app(&a, json!({ "registry": reg.to_str().unwrap(), "dependencies": { "zzz": { "version": "1" } } }), "zzz");
    assert!(packages::fetch(&a).unwrap_err().contains("the registry has no package zzz"));
}

#[test]
fn publishing_needs_a_version_and_a_clean_registry() {
    let b = base("publish");
    let reg = b.join("registry");
    std::fs::create_dir_all(&reg).unwrap();
    git(&reg, &["init", "-q"]);
    let p = b.join("repos/p");
    let c = commit(&p, &[("staal.json", r#"{"package": "p", "authority": {}}"#)]);
    let publish = |c: &str| staalc(&["publish", "--registry", reg.to_str().unwrap(), "--git", p.to_str().unwrap(), "--commit", c]);
    assert!(publish(&c).2.contains("gives no \"version\""));
    let c = commit(&p, &[("staal.json", r#"{"package": "p", "version": "0.1.0", "authority": {}}"#)]);
    assert_eq!(publish(&c).0, Some(0));
    std::fs::write(reg.join("packages/p.json"), "{}").unwrap();
    let c2 = commit(&p, &[("staal.json", r#"{"package": "p", "version": "0.2.0", "authority": {}}"#)]);
    assert!(publish(&c2).2.contains("uncommitted changes"));
    git(&reg, &["checkout", "--", "packages/p.json"]);
    assert_eq!(publish(&c2).0, Some(0));
    assert!(publish("main").2.contains("full commit id"));
}

#[test]
fn update_moves_only_the_package_it_names() {
    let b = base("update-one");
    let (x, y) = (b.join("repos/x"), b.join("repos/y"));
    release(&x, "x", "1.0.0", 1, json!({}), false);
    release(&y, "y", "1.0.0", 1, json!({}), false);
    let a = b.join("app");
    let dep = |r: &Path| json!({ "git": r.to_str().unwrap(), "version": "1" });
    app(&a, json!({ "dependencies": { "x": dep(&x), "y": dep(&y) } }), "x");
    assert_eq!(staalc(&["fetch", a.to_str().unwrap()]).0, Some(0));
    release(&x, "x", "1.1.0", 2, json!({}), false);
    release(&y, "y", "1.1.0", 2, json!({}), false);
    let (code, out, err) = staalc(&["update", a.to_str().unwrap(), "--package", "x"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("x 1.1.0 ") && out.contains("y 1.0.0 "), "{out}");
    let (code, out, _) = staalc(&["update", a.to_str().unwrap()]);
    assert_eq!(code, Some(0));
    assert!(out.contains("y 1.1.0 "), "{out}");
}
