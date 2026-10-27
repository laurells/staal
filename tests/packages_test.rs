// Packages (phase 4): fetching from git at an exact commit, verifying every file
// against staal.lock before use, resolving imports inside packages, and holding a
// package's code to the authority its manifest declares (E108).
//
// Repositories are made here with git, in the test's own folder, and fetched by path:
// the same code path as a remote, without the network.

mod common;

use common::server::Server;
use staalc::interp::{run_config_durable, Durability};
use staalc::packages;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

const KEY: &str = "sk_test_pkg_1a2b3c4d5e6f";

fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: set once, before any test reads the environment (every test calls init first).
    ONCE.call_once(|| unsafe {
        std::env::set_var("STAAL_PKG_KEY", KEY);
    });
}

fn base(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("packages").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(d).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A git repository at `dir` holding `files`; returns the commit id.
fn repo(dir: &Path, files: &[(&str, &str)]) -> String {
    std::fs::create_dir_all(dir).unwrap();
    if !dir.join(".git").exists() {
        git(dir, &["init", "-q"]);
    }
    for (f, text) in files {
        let p = dir.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c", "--allow-empty"]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A repository holding a copy of one of the reference packages in packages/.
fn reference_repo(at: &Path, name: &str) -> String {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("packages").join(name);
    let mut files = Vec::new();
    for e in std::fs::read_dir(&src).unwrap() {
        let e = e.unwrap();
        files.push((e.file_name().to_string_lossy().to_string(), std::fs::read_to_string(e.path()).unwrap()));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    repo(&at.join(name), &refs)
}

fn manifest(deps: &[(&str, &Path, &str)]) -> String {
    let mut d = serde_json::Map::new();
    for (n, path, commit) in deps {
        d.insert(n.to_string(), json!({ "git": path.to_str().unwrap(), "commit": commit }));
    }
    json!({ "dependencies": d }).to_string()
}

fn project(dir: &Path, deps: &[(&str, &Path, &str)], main: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("staal.json"), manifest(deps)).unwrap();
    std::fs::write(dir.join("main.staal"), main).unwrap();
    dir.join("main.staal")
}

fn codes(main: &Path) -> Result<Vec<&'static str>, String> {
    let c = staalc::check_path(main)?;
    Ok(match c.result {
        Ok(_) => vec![],
        Err(d) => d.iter().map(|d| d.code).collect(),
    })
}

fn messages(main: &Path) -> String {
    let c = staalc::check_path(main).unwrap();
    match c.result {
        Ok(_) => String::new(),
        Err(d) => d.iter().map(|d| staalc::format_diag_in(&c.files, d)).collect::<Vec<_>>().join("\n"),
    }
}

const USES_ALL: &str = r#"import sms.termii
import payments.stripe
import email.resend
import chat.slack

fn go(texts: cap sms.termii.send(uses: 1), refunds: cap payments.stripe.refund(max: 10 USD),
      mail: cap email.resend.send(only: ["*@clinic.org"]), channel: cap chat.slack.post) -> Int {
    let a = try texts.send("+234 801 234 5678", "hello")
    let b = try refunds.refund("pi_1", 5.00 USD)
    let c = try mail.send("ada@clinic.org", "Results", "Ready.")
    let d = try channel.post("C0123456789", "done")
    let n = 0
    if a.ok and b.ok and c.ok and d.ok {
        set n = 4
    }
    return n
}
"#;

#[test]
fn the_reference_packages_fetch_check_and_report_their_authority() {
    let b = base("reference");
    let repos = b.join("repos");
    let deps: Vec<(String, PathBuf, String)> = ["sms", "payments", "email", "chat"]
        .iter()
        .map(|n| (n.to_string(), repos.join(n), reference_repo(&repos, n)))
        .collect();
    let d: Vec<(&str, &Path, &str)> = deps.iter().map(|(n, p, c)| (n.as_str(), p.as_path(), c.as_str())).collect();
    let main = project(&b.join("app"), &d, USES_ALL);
    let report = packages::fetch(&b.join("app")).unwrap().join("\n");
    assert!(report.contains("sms "), "{report}");
    assert!(report.contains("authority: net.http.post only https://api.ng.termii.com/api/sms/send"), "{report}");
    assert!(report.contains("net.http.get only https://api.stripe.com/v1/payment_intents/*"), "{report}");
    assert!(report.contains("(new)"), "{report}");
    assert_eq!(codes(&main).unwrap(), Vec::<&str>::new(), "{}", messages(&main));
    // A second fetch changes nothing and says so.
    let again = packages::fetch(&b.join("app")).unwrap().join("\n");
    assert_eq!(again.matches("(unchanged)").count(), 4, "{again}");
    let lock = std::fs::read_to_string(b.join("app/staal.lock")).unwrap();
    assert!(lock.contains("\"termii.staal\""), "{lock}");
}

/// A project depending on package `sms` made from the reference package.
fn fetched_sms(name: &str) -> (PathBuf, PathBuf) {
    let b = base(name);
    let commit = reference_repo(&b.join("repos"), "sms");
    let main = project(
        &b.join("app"),
        &[("sms", &b.join("repos/sms"), &commit)],
        "import sms.termii\nfn go(t: cap sms.termii.send) -> Receipt {\n    return t.send(\"2348012345678\", \"hi\")\n}\n",
    );
    packages::fetch(&b.join("app")).unwrap();
    assert_eq!(codes(&main).unwrap(), Vec::<&str>::new(), "{}", messages(&main));
    (b.join("app"), main)
}

#[test]
fn a_tampered_package_is_refused_before_it_is_read() {
    let (app, main) = fetched_sms("tamper");
    let pkg = app.join(".staal/packages/sms");
    let original = std::fs::read_to_string(pkg.join("termii.staal")).unwrap();

    // One character changed.
    std::fs::write(pkg.join("termii.staal"), original.replace("api.ng.termii.com", "api.ng.termii.co")).unwrap();
    let e = codes(&main).unwrap_err();
    assert!(e.contains("termii.staal has been changed"), "{e}");
    std::fs::write(pkg.join("termii.staal"), &original).unwrap();
    assert!(codes(&main).is_ok());

    // A file added: even one that is not code.
    std::fs::write(pkg.join("extra.staal"), "fn x() -> Int {\n    return 1\n}\n").unwrap();
    let e = codes(&main).unwrap_err();
    assert!(e.contains("extra.staal is not part of it"), "{e}");
    std::fs::remove_file(pkg.join("extra.staal")).unwrap();

    // A file removed.
    std::fs::remove_file(pkg.join("termii.staal")).unwrap();
    let e = codes(&main).unwrap_err();
    assert!(e.contains("termii.staal is missing"), "{e}");

    // A link in place of a file, even one pointing at identical content.
    let copy = app.join("copy.staal");
    std::fs::write(&copy, &original).unwrap();
    std::os::unix::fs::symlink(&copy, pkg.join("termii.staal")).unwrap();
    let e = codes(&main).unwrap_err();
    assert!(e.contains("link or a special file"), "{e}");
    std::fs::remove_file(pkg.join("termii.staal")).unwrap();

    // `fetch` restores the package from git, and the program checks again.
    let report = packages::fetch(&app).unwrap().join("\n");
    assert!(report.contains("(unchanged)"), "{report}");
    std::fs::remove_file(&copy).unwrap();
    assert_eq!(std::fs::read_to_string(pkg.join("termii.staal")).unwrap(), original);
    assert_eq!(codes(&main).unwrap(), Vec::<&str>::new());
}

#[test]
fn the_lockfile_itself_is_checked() {
    let (app, main) = fetched_sms("lock");
    let lock_path = app.join("staal.lock");
    let lock = std::fs::read_to_string(&lock_path).unwrap();
    // A hash edited to match a tampered file is still a mismatch with the commit: the
    // next fetch re-reads git and refuses rather than trusting either.
    let tampered = app.join(".staal/packages/sms/termii.staal");
    let evil = std::fs::read_to_string(&tampered).unwrap().replace("api.ng.termii.com", "evil.example.com");
    std::fs::write(&tampered, &evil).unwrap();
    let parsed = packages::Lock::parse(&lock, "lock").unwrap();
    let old_hash = parsed.packages["sms"].files["termii.staal"].clone();
    std::fs::write(&lock_path, lock.replace(&old_hash, &packages::sha256_hex(evil.as_bytes()))).unwrap();
    // The check alone cannot tell (the lock is the record); fetch compares with git.
    assert!(codes(&main).is_ok());
    let e = packages::fetch(&app).unwrap_err();
    assert!(e.contains("does not have the files staal.lock records"), "{e}");
    assert!(e.contains("Nothing was changed"), "{e}");
    assert_eq!(std::fs::read_to_string(&tampered).unwrap(), evil, "a refused fetch changed the store");

    // No lock at all: nothing runs until `fetch`.
    std::fs::remove_file(&lock_path).unwrap();
    assert!(codes(&main).unwrap_err().contains("run `staalc fetch`"));
}

#[test]
fn dependencies_are_exact() {
    let b = base("exact");
    let commit = reference_repo(&b.join("repos"), "sms");
    let app = b.join("app");
    let main = project(&app, &[("sms", &b.join("repos/sms"), "main")], "fn go() -> Int {\n    return 1\n}\n");
    assert!(codes(&main).unwrap_err().contains("full commit id"));
    // A commit the repository does not have.
    let missing = "0123456789abcdef0123456789abcdef01234567";
    project(&app, &[("sms", &b.join("repos/sms"), missing)], "fn go() -> Int {\n    return 1\n}\n");
    assert!(packages::fetch(&app).unwrap_err().contains("has no commit"));
    // The package must call itself what the project calls it.
    project(&app, &[("texts", &b.join("repos/sms"), &commit)], "fn go() -> Int {\n    return 1\n}\n");
    assert!(packages::fetch(&app).unwrap_err().contains("names it 'sms'"));
    // Plain http is refused.
    std::fs::write(app.join("staal.json"), json!({"dependencies": {"sms": {"git": "http://example.com/sms", "commit": commit}}}).to_string())
        .unwrap();
    assert!(packages::fetch(&app).unwrap_err().contains("must be https://"));
    // A manifest changed after fetching: the lock no longer matches it.
    let other = repo(&b.join("repos/sms"), &[("NOTES.md", "second commit")]);
    project(&app, &[("sms", &b.join("repos/sms"), &commit)], "fn go() -> Int {\n    return 1\n}\n");
    packages::fetch(&app).unwrap();
    assert!(codes(&main).is_ok());
    project(&app, &[("sms", &b.join("repos/sms"), &other)], "fn go() -> Int {\n    return 1\n}\n");
    assert!(codes(&main).unwrap_err().contains("run `staalc fetch`"));
    let report = packages::fetch(&app).unwrap().join("\n");
    assert!(report.contains(&format!("(was commit {})", &commit[..12])), "{report}");
    assert!(codes(&main).is_ok());
}

#[test]
fn a_repository_with_a_link_or_unsafe_path_is_refused() {
    let b = base("links");
    let r = b.join("repos/bad");
    let commit = repo(&r, &[("staal.json", r#"{"package": "bad", "authority": {}}"#), ("a.staal", "fn f() -> Int {\n    return 1\n}\n")]);
    std::os::unix::fs::symlink("/etc/passwd", r.join("b.staal")).unwrap();
    let linked = repo(&r, &[]);
    let app = b.join("app");
    project(&app, &[("bad", &r, &linked)], "fn go() -> Int {\n    return 1\n}\n");
    let e = packages::fetch(&app).unwrap_err();
    assert!(e.contains("b.staal is a link or submodule"), "{e}");
    assert!(!app.join(".staal/packages/bad").exists());
    // The commit before the link is fine.
    project(&app, &[("bad", &r, &commit)], "fn go() -> Int {\n    return 1\n}\n");
    packages::fetch(&app).unwrap();
}

/// Packages that import their own modules and each other.
#[test]
fn imports_resolve_inside_packages() {
    let b = base("imports");
    let repos = b.join("repos");
    let fmt = repo(
        &repos.join("fmt"),
        &[
            ("staal.json", r#"{"package": "fmt", "authority": {}}"#),
            ("text.staal", "pub fn shout(t: Text) -> Text {\n    return t + \"!\"\n}\n"),
        ],
    );
    let greet_manifest = json!({"package": "greet", "authority": {},
        "dependencies": {"fmt": {"git": repos.join("fmt").to_str().unwrap(), "commit": fmt}}})
    .to_string();
    let greet = repo(
        &repos.join("greet"),
        &[
            ("staal.json", greet_manifest.as_str()),
            // `util` is greet's own module; `fmt.text` is the dependency.
            ("hello.staal", "import util\nimport fmt.text\npub fn hello(n: Text) -> Text {\n    return text.shout(util.prefix() + n)\n}\n"),
            ("util.staal", "pub fn prefix() -> Text {\n    return \"hello \"\n}\n"),
        ],
    );
    let app = b.join("app");
    // The program has its own `util` too: a different module from greet's.
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("util.staal"), "pub fn prefix() -> Text {\n    return \"WRONG \"\n}\n").unwrap();
    let main = project(
        &app,
        &[("greet", &repos.join("greet"), &greet)],
        "import greet.hello\nimport util\nfn go() -> Text {\n    return hello.hello(\"ada\") + \" \" + util.prefix()\n}\n",
    );
    let report = packages::fetch(&app).unwrap().join("\n");
    assert!(report.contains("fmt ") && report.contains("greet "), "{report}");
    let checked = staalc::check_path(&main).unwrap();
    let prog = checked.result.unwrap_or_else(|d| panic!("{d:?}"));
    let cfg = json!({ "entry": "go", "model": { "provider": "script", "responses": [] } });
    let out = staalc::interp::run_config(&prog, &cfg).unwrap();
    assert_eq!(out.result.unwrap(), json!("hello ada! WRONG "));

    // The program cannot reach a package it does not depend on itself.
    std::fs::write(&main, "import fmt.text\nfn go() -> Text {\n    return text.shout(\"x\")\n}\n").unwrap();
    assert!(messages(&main).contains("does not depend on itself"), "{}", messages(&main));
    // A dependency and a folder of the program with the same name are refused.
    std::fs::create_dir_all(app.join("greet")).unwrap();
    std::fs::write(&main, "import greet.hello\nfn go() -> Text {\n    return hello.hello(\"x\")\n}\n").unwrap();
    assert!(messages(&main).contains("both a dependency and a file or folder"), "{}", messages(&main));
}

#[test]
fn one_version_per_package() {
    let b = base("versions");
    let repos = b.join("repos");
    let v1 = repo(&repos.join("fmt"), &[("staal.json", r#"{"package": "fmt", "authority": {}}"#), ("text.staal", "pub fn f() -> Int {\n    return 1\n}\n")]);
    let v2 = repo(&repos.join("fmt"), &[("text.staal", "pub fn f() -> Int {\n    return 2\n}\n")]);
    let user = |name: &str, commit: &str| {
        let m = json!({"package": name, "authority": {},
            "dependencies": {"fmt": {"git": repos.join("fmt").to_str().unwrap(), "commit": commit}}})
        .to_string();
        repo(&repos.join(name), &[("staal.json", m.as_str()), ("m.staal", "pub fn g() -> Int {\n    return 3\n}\n")])
    };
    let a = user("a", &v1);
    let bb = user("b", &v2);
    let app = b.join("app");
    project(&app, &[("a", &repos.join("a"), &a), ("b", &repos.join("b"), &bb)], "fn go() -> Int {\n    return 1\n}\n");
    let e = packages::fetch(&app).unwrap_err();
    assert!(e.starts_with("package fmt: package a wants commit") && e.contains("package b wants commit") && e.ends_with("one version per package"), "{e}");
}

/// A package whose authority and code are given, in a project that uses it.
fn authority_case(name: &str, authority: serde_json::Value, code: &str) -> PathBuf {
    let b = base(name);
    let m = json!({"package": "lib", "authority": authority}).to_string();
    let commit = repo(&b.join("repos/lib"), &[("staal.json", m.as_str()), ("m.staal", code)]);
    let main = project(&b.join("app"), &[("lib", &b.join("repos/lib"), &commit)], "import lib.m\nfn go() -> Int {\n    return 1\n}\n");
    packages::fetch(&b.join("app")).unwrap();
    main
}

#[test]
fn a_package_is_held_to_its_declared_authority() {
    let post = "https://api.example.com/v1/*";
    let cases: Vec<(&str, serde_json::Value, &str, Vec<&str>, &str)> = vec![
        ("auth-ok", json!({"net.http.post": [post]}),
         "pub fn f(h: cap net.http.post(only: [\"https://api.example.com/v1/send\"])) -> Int {\n    let r = h.post(\"https://api.example.com/v1/send\", \"x\")\n    return r.status\n}\n",
         vec![], ""),
        ("auth-unlisted", json!({}),
         "pub fn f(h: cap net.http.post(only: [\"https://api.example.com/v1/send\"])) -> Int {\n    return 1\n}\n",
         vec!["E108"], "does not list"),
        ("auth-wider", json!({"net.http.post": [post]}),
         "pub fn f(h: cap net.http.post(only: [\"https://api.example.com/*\"])) -> Int {\n    return 1\n}\n",
         vec!["E108"], "reaches beyond"),
        ("auth-no-only", json!({"net.http.post": [post]}),
         "pub fn f(h: cap net.http.post) -> Int {\n    return 1\n}\n",
         vec!["E108"], "needs `only`"),
        ("auth-any", json!({"net.http.post": "any"}),
         "pub fn f(h: cap net.http.post) -> Int {\n    return 1\n}\n",
         vec![], ""),
        ("auth-in-let", json!({"net.http.post": [post]}),
         "pub fn f(h: cap net.http.post(only: [\"https://api.example.com/v1/a\"])) -> Int {\n    let g: cap net.http.get = h\n    return 1\n}\n",
         vec!["E108", "E101"], "net.http.get"),
        ("auth-return", json!({"net.http.post": [post]}),
         "pub fn f(h: cap net.http.post(only: [\"https://api.example.com/v1/a\"])) -> cap net.http.delete {\n    return h\n}\n",
         vec!["E101", "E108"], "net.http.delete"),
        ("auth-need", json!({}),
         "capability lib.x {\n    needs http: cap net.http.post\n    action go(t: Text) -> Text {\n        let r = http.post(\"https://api.example.com/v1/a\", t)\n        return r.body\n    }\n}\n",
         vec!["E108"], "does not list"),
        ("auth-cap-name", json!({"net.http.post": "any"}),
         "capability stripe.refunds {\n    needs http: cap net.http.post\n    action go(t: Text) -> Text {\n        let r = http.post(\"https://api.example.com/v1/a\", t)\n        return r.body\n    }\n}\n",
         vec!["E108"], "must start with its name"),
        ("auth-foreign-ok", json!({"ffi.lib.f": "any"}),
         "foreign ffi.lib {\n    fn f() -> i64\n}\n\npub fn g(c: cap ffi.lib.f) -> Int {\n    return c.f()\n}\n",
         vec![], ""),
        ("auth-foreign-name", json!({"ffi.zlib.f": "any"}),
         "foreign ffi.zlib {\n    fn f() -> i64\n}\n\npub fn g(c: cap ffi.zlib.f) -> Int {\n    return c.f()\n}\n",
         vec!["E108"], "a package's C library must be ffi.lib"),
        ("auth-foreign-unlisted", json!({}),
         "foreign ffi.lib {\n    fn f() -> i64\n}\n\npub fn g(c: cap ffi.lib.f) -> Int {\n    return c.f()\n}\n",
         vec!["E108"], "does not list"),
        ("auth-library-cap", json!({"net.http.post": "any"}),
         "pub fn f(s: cap sms.termii.send) -> Receipt {\n    return s.send(\"1\", \"x\")\n}\n",
         vec!["E108"], "sms.termii.send"),
    ];
    for (name, authority, code, want, text) in cases {
        let main = authority_case(name, authority, code);
        let mut got = codes(&main).unwrap();
        got.sort();
        let mut want = want;
        want.sort();
        assert_eq!(got, want, "{name}: {}", messages(&main));
        assert!(messages(&main).contains(text), "{name}: {}", messages(&main));
    }
}

#[test]
fn the_program_itself_is_not_limited_by_a_manifest() {
    let b = base("root");
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(b.join("staal.json"), r#"{"package": "app", "authority": {}}"#).unwrap();
    std::fs::write(b.join("main.staal"), "fn go(h: cap net.http.post) -> Int {\n    return 1\n}\n").unwrap();
    assert_eq!(codes(&b.join("main.staal")).unwrap(), Vec::<&str>::new());
}

/// The phase 4 gate: a project adds a package from git, builds, refuses a tampered
/// copy, and runs a program through it, against a local server standing in for the
/// service. A journal cannot resume on different package code.
#[test]
fn a_program_runs_through_a_fetched_package() {
    init();
    let s = Server::start();
    let b = base("gate");
    let send = s.url("/api/sms/send");
    let m = json!({"package": "sms", "version": "0.1.0", "authority": {"net.http.post": [send]}}).to_string();
    let reference = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("packages/sms/termii.staal")).unwrap();
    // The reference package, pointed at the local server.
    let code = reference.replace("https://api.ng.termii.com/api/sms/send", &send);
    let commit = repo(&b.join("repos/sms"), &[("staal.json", m.as_str()), ("termii.staal", code.as_str())]);
    let app = b.join("app");
    let main = project(
        &app,
        &[("sms", &b.join("repos/sms"), &commit)],
        "import sms.termii\nfn remind(phone: Text, texts: cap sms.termii.send(only: [\"2348012345678\"], uses: 1)) -> Receipt {\n    return texts.send(phone, \"See you tomorrow.\")\n}\n",
    );
    packages::fetch(&app).unwrap();
    let checked = staalc::check_path(&main).unwrap();
    let prog = checked.result.unwrap_or_else(|d| panic!("{d:?}"));
    let cfg = json!({
        "entry": "remind",
        "inputs": { "phone": "+234 801 234 5678" },
        "grants": { "texts": { "cap": "sms.termii.send", "needs": {
            "http": { "cap": "net.http.post" },
            "key": { "env": "STAAL_PKG_KEY" },
            "sender": "Clinic" } } },
        "model": { "provider": "script", "responses": [] }
    });
    let journal = b.join("run.journal");
    let out = run_config_durable(&prog, &checked.fingerprint, &cfg, Durability::Create(&journal)).unwrap();
    assert_eq!(out.result.as_ref().unwrap()["receipt"], "t-1", "{:?}", out.log);
    let reqs = s.requests();
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
    assert_eq!(body["to"], "2348012345678");
    assert_eq!(body["api_key"], KEY);

    // Same package code: the finished run replays.
    let again = staalc::check_path(&main).unwrap();
    assert_eq!(again.fingerprint, checked.fingerprint);
    let replay = run_config_durable(&again.result.unwrap(), &again.fingerprint, &cfg, Durability::Resume(&journal)).unwrap();
    assert_eq!(replay.result.unwrap(), out.result.unwrap());
    assert_eq!(s.requests().len(), 1, "replay contacted the server");

    // A new commit of the package: resume refuses.
    let changed = code.replace("channel: \"generic\"", "channel: \"dnd\"");
    let commit2 = repo(&b.join("repos/sms"), &[("termii.staal", changed.as_str())]);
    std::fs::write(app.join("staal.json"), manifest(&[("sms", &b.join("repos/sms"), &commit2)])).unwrap();
    packages::fetch(&app).unwrap();
    let newer = staalc::check_path(&main).unwrap();
    assert_ne!(newer.fingerprint, checked.fingerprint);
    let Err(e) = run_config_durable(&newer.result.unwrap(), &newer.fingerprint, &cfg, Durability::Resume(&journal)) else {
        panic!("resume must refuse different package code");
    };
    assert!(e.contains("different program"), "{e}");
}

#[test]
fn files_are_taken_from_git_objects_not_a_checkout() {
    // A repository that asks git to convert line endings on checkout: the package must
    // hold the committed bytes, so its hashes are the same on every machine.
    let b = base("eol");
    let r = b.join("repos/eol");
    let commit = repo(
        &r,
        &[
            (".gitattributes", "*.staal text eol=crlf\n"),
            ("staal.json", r#"{"package": "eol", "authority": {}}"#),
            ("m.staal", "pub fn f() -> Int {\n    return 1\n}\n"),
        ],
    );
    let app = b.join("app");
    project(&app, &[("eol", &r, &commit)], "import eol.m\nfn go() -> Int {\n    return m.f()\n}\n");
    packages::fetch(&app).unwrap();
    let text = std::fs::read(app.join(".staal/packages/eol/m.staal")).unwrap();
    assert!(!text.contains(&b'\r'), "line endings were converted");
    let blob = git(&r, &["rev-parse", &format!("{commit}:m.staal")]);
    let committed = Command::new("git").arg("-C").arg(&r).args(["cat-file", "blob", &blob]).output().unwrap().stdout;
    assert_eq!(text, committed);
}

/// A reference package with its service's base URL replaced by the local server's,
/// in its code and its manifest alike.
fn local_reference(at: &Path, name: &str, real: &str, local: &str) -> String {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("packages").join(name);
    let mut files = Vec::new();
    for e in std::fs::read_dir(&src).unwrap() {
        let e = e.unwrap();
        let text = std::fs::read_to_string(e.path()).unwrap().replace(real, local);
        files.push((e.file_name().to_string_lossy().to_string(), text));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    repo(&at.join(name), &refs)
}

#[test]
fn every_reference_package_runs_against_a_local_service() {
    init();
    let s = Server::start();
    let b = base("all-reference");
    let repos = b.join("repos");
    let local = s.url("");
    let deps = [
        ("sms", "https://api.ng.termii.com", local_reference(&repos, "sms", "https://api.ng.termii.com", &local)),
        ("payments", "https://api.stripe.com", local_reference(&repos, "payments", "https://api.stripe.com", &local)),
        ("email", "https://api.resend.com", local_reference(&repos, "email", "https://api.resend.com", &local)),
        ("chat", "https://slack.com", local_reference(&repos, "chat", "https://slack.com", &local)),
    ];
    let d: Vec<(&str, PathBuf, &str)> = deps.iter().map(|(n, _, c)| (*n, repos.join(n), c.as_str())).collect();
    let d: Vec<(&str, &Path, &str)> = d.iter().map(|(n, p, c)| (*n, p.as_path(), *c)).collect();
    let main = project(
        &b.join("app"),
        &d,
        r#"import sms.termii
import payments.stripe
import email.resend
import chat.slack

fn go(texts: cap sms.termii.send(uses: 1), refunds: cap payments.stripe.refund(max: 10 USD),
      mail: cap email.resend.send(only: ["*@clinic.org"]), channel: cap chat.slack.post(only: ["C1", "C_BAD"])) -> Text {
    let a = texts.send("+234 801 234 5678", "hello")
    let r = refunds.refund("pi_usd", 5.00 USD)
    let c = mail.send("ada@clinic.org", "Results", "Ready.")
    let upper = try mail.send("Ada@clinic.org", "Results", "Ready.")
    let d = channel.post("C1", "done")
    let bad = try channel.post("C_BAD", "done")
    if upper.ok or bad.ok {
        return "a refusal was missed"
    }
    return bad.error
}
"#,
    );
    packages::fetch(&b.join("app")).unwrap();
    let checked = staalc::check_path(&main).unwrap();
    let prog = checked.result.unwrap_or_else(|d| panic!("{d:?}"));
    let http = |p: &str| json!({ "cap": "net.http.post", "only": [s.url(p)] });
    let cfg = json!({
        "entry": "go",
        "grants": {
            "texts": { "cap": "sms.termii.send", "needs": { "http": http("/api/sms/send"), "key": { "env": "STAAL_PKG_KEY" }, "sender": "Clinic" } },
            "refunds": { "cap": "payments.stripe.refund", "needs": {
                "get": { "cap": "net.http.get", "only": [s.url("/v1/payment_intents/*")] },
                "post": { "cap": "net.http.post", "only": [s.url("/v1/refunds")], "idempotency_key": true },
                "key": { "env": "STAAL_PKG_KEY" } } },
            "mail": { "cap": "email.resend.send", "needs": { "http": http("/emails"), "key": { "env": "STAAL_PKG_KEY" }, "sender": "clinic@clinic.org" } },
            "channel": { "cap": "chat.slack.post", "needs": { "http": http("/api/chat.postMessage"), "token": { "env": "STAAL_PKG_KEY" } } }
        },
        "model": { "provider": "script", "responses": [] }
    });
    let out = staalc::interp::run_config(&prog, &cfg).unwrap();
    assert_eq!(out.result.as_ref().unwrap(), &json!("action: Slack refused the message: channel_not_found"), "{:?}", out.log);
    let reqs = s.requests();
    let paths: Vec<&str> = reqs.iter().map(|r| r.path.as_str()).collect();
    // The capital-letter address was refused by `canonical` before any request.
    assert_eq!(paths, ["/api/sms/send", "/v1/payment_intents/pi_usd", "/v1/refunds", "/emails", "/api/chat.postMessage", "/api/chat.postMessage"]);
    let email: serde_json::Value = serde_json::from_str(&reqs[3].body).unwrap();
    assert_eq!(email, json!({ "from": "clinic@clinic.org", "to": ["ada@clinic.org"], "subject": "Results", "text": "Ready." }));
    assert_eq!(reqs[3].headers["authorization"], format!("Bearer {KEY}"));
    assert_eq!(reqs[4].headers["authorization"], format!("Bearer {KEY}"));
    assert_eq!(reqs[2].body, "payment_intent=pi_usd&amount=500");
    let log = serde_json::to_string(&out.log).unwrap();
    assert!(!log.contains(KEY), "the log contains the key");
}

/// The payments package's Paystack and Flutterwave refunds, against a local server
/// answering as their documentation shows: each refund checked against its
/// transaction first, amounts in each service's unit, and every refusal made before
/// any refund is sent.
#[test]
fn paystack_and_flutterwave_refunds_run_against_local_services() {
    init();
    let s = Server::start();
    let b = base("african-payments");
    let repos = b.join("repos");
    let local = s.url("");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("packages/payments");
    let mut files = Vec::new();
    for e in std::fs::read_dir(&src).unwrap() {
        let e = e.unwrap();
        let text = std::fs::read_to_string(e.path())
            .unwrap()
            .replace("https://api.paystack.co", &local)
            .replace("https://api.flutterwave.com", &local);
        files.push((e.file_name().to_string_lossy().to_string(), text));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let commit = repo(&repos.join("payments"), &refs);
    let main = project(
        &b.join("app"),
        &[("payments", &repos.join("payments"), &commit)],
        r#"import payments.paystack
import payments.flutterwave

fn go(ps: cap payments.paystack.refund(max: 50000 NGN), fw: cap payments.flutterwave.refund(max: 50000 NGN)) -> Text {
    let a = ps.refund(" T_ok ", 2500.00 NGN)
    let failed = try ps.refund("T_failed", 100.00 NGN)
    let ghs = try ps.refund("T_ghs", 100.00 NGN)
    let c = fw.refund("908790", 1500.00 NGN)
    let fw_failed = try fw.refund("908791", 1500.00 NGN)
    let kobo = try fw.refund("908790", 150.50 NGN)
    let not_id = try fw.refund("order-1", 100.00 NGN)
    if failed.ok or ghs.ok or fw_failed.ok or kobo.ok or not_id.ok {
        return "a refusal was missed"
    }
    return failed.error + " | " + ghs.error + " | " + fw_failed.error + " | " + kobo.error + " | " + not_id.error
}
"#,
    );
    packages::fetch(&b.join("app")).unwrap();
    let checked = staalc::check_path(&main).unwrap();
    let prog = checked.result.unwrap_or_else(|d| panic!("{d:?}"));
    let cfg = json!({
        "entry": "go",
        "grants": {
            "ps": { "cap": "payments.paystack.refund", "needs": {
                "get": { "cap": "net.http.get", "only": [s.url("/transaction/verify/*")] },
                "post": { "cap": "net.http.post", "only": [s.url("/refund")] },
                "key": { "env": "STAAL_PKG_KEY" } } },
            "fw": { "cap": "payments.flutterwave.refund", "needs": {
                "get": { "cap": "net.http.get", "only": [s.url("/v3/transactions/*/verify")] },
                "post": { "cap": "net.http.post", "only": [s.url("/v3/transactions/*/refund")] },
                "key": { "env": "STAAL_PKG_KEY" } } }
        },
        "model": { "provider": "script", "responses": [] }
    });
    let out = staalc::interp::run_config(&prog, &cfg).unwrap();
    let result = out.result.as_ref().unwrap_or_else(|f| panic!("{f:?}")).as_str().unwrap().to_string();
    let parts: Vec<&str> = result.split(" | ").collect();
    assert!(parts[0].contains("transaction T_failed is 'failed', not a successful payment"), "{result}");
    assert!(parts[1].contains("the payment is in GHS, the refund in NGN"), "{result}");
    assert!(parts[2].contains("transaction 908791 is 'failed', not a successful payment"), "{result}");
    assert!(parts[3].contains("whole units of a two-decimal currency, not 150.50 NGN"), "{result}");
    assert!(parts[4].contains("'order-1' is not a Flutterwave transaction id"), "{result}");
    // The receipts are the services' own refund ids.
    let receipts: Vec<String> = out
        .log
        .iter()
        .filter(|e| e["event"] == "action_performed" && e["capability"].as_str().is_some_and(|c| c.starts_with("payments.")))
        .map(|e| e["receipt"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(receipts, ["paystack-refund-3018285", "flutterwave-refund-75928"], "{:?}", out.log);
    let reqs = s.requests();
    let sent: Vec<(&str, &str)> = reqs.iter().map(|r| (r.method.as_str(), r.path.as_str())).collect();
    // Only the two good refunds were sent; every refusal came before a refund request.
    assert_eq!(
        sent,
        [
            ("GET", "/transaction/verify/T_ok"),
            ("POST", "/refund"),
            ("GET", "/transaction/verify/T_failed"),
            ("GET", "/transaction/verify/T_ghs"),
            ("GET", "/v3/transactions/908790/verify"),
            ("POST", "/v3/transactions/908790/refund"),
            ("GET", "/v3/transactions/908791/verify"),
        ]
    );
    let paystack: serde_json::Value = serde_json::from_str(&reqs[1].body).unwrap();
    assert_eq!(paystack, json!({ "transaction": "T_ok", "amount": "250000" }), "kobo, as a string, as Paystack's example");
    let flutterwave: serde_json::Value = serde_json::from_str(&reqs[5].body).unwrap();
    assert_eq!(flutterwave, json!({ "amount": 1500, "comments": "Refund made by a Staal program" }), "naira, not kobo");
    for r in &reqs {
        assert_eq!(r.headers["authorization"], format!("Bearer {KEY}"), "{}", r.path);
    }
    let log = serde_json::to_string(&out.log).unwrap();
    assert!(!log.contains(KEY), "the log contains the key");
}
