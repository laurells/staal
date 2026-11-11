// The test API (staalc::testing): each case's result as data, the same as `staalc test`
// prints, and a test that holds no authority by any route: no grant, no secret, no
// capability a package or library declares, no model reached with a key from the
// environment, no setting that reads a file by path.

use serde_json::json;
use staalc::interp::Engine;
use staalc::testing::{Case, Outcome, Settings};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn engines() -> Vec<Engine> {
    if cfg!(feature = "native") { vec![Engine::Interpreter, Engine::Native] } else { vec![Engine::Interpreter] }
}

fn cases(files: &[PathBuf], settings: &Settings) -> Vec<Case> {
    let mut got = Vec::new();
    staalc::testing::run(files, settings, |c| got.push(c.clone())).unwrap();
    got
}

#[test]
fn a_test_config_gives_no_authority() {
    let script = json!({ "provider": "script", "responses": [] });
    // A key in the environment is never reached: each setting naming one is refused.
    for (config, says) in [
        (json!({ "grants": {} }), "cannot give \"grants\""),
        (json!({ "secrets": { "k": { "env": "STAAL_TESTING_TEST_KEY" } } }), "cannot give \"secrets\""),
        (json!({ "entry": "test_a" }), "cannot give \"entry\""),
        (json!({ "inputs": {} }), "cannot give \"inputs\""),
        (json!({ "model": { "provider": "anthropic", "model": "claude-haiku-5-5", "api_key": { "env": "STAAL_TESTING_TEST_KEY" } } }), "model is scripted"),
        (json!({ "model": { "provider": "openai_compatible", "base_url": "http://127.0.0.1:1", "model": "m" } }), "model is scripted"),
        (json!({ "model": {} }), "model is scripted"),
        (json!({ "model": "script" }), "model is scripted"),
        (json!({ "net": { "extra_root_certificates": ["/etc/passwd"] } }), "cannot set \"net\""),
        (json!({ "net": { "proxy": "environment" } }), "cannot set \"net\""),
        (json!({ "fs": { "max_read_bytes": 10 } }), "cannot set \"fs\""),
        (json!({ "anything": 1 }), "cannot set \"anything\""),
        (json!([1]), "expected a JSON object"),
    ] {
        let e = Settings::new(Some(&config), engines()).unwrap_err();
        assert!(e.contains(says), "{config}: {e}");
    }
    for config in [json!({}), json!({ "model": script }), json!({ "adapters": { "simulated": { "fail": [] } } }), json!({ "limits": {} })] {
        assert!(Settings::new(Some(&config), engines()).is_ok(), "{config}");
    }
    assert!(Settings::new(None, Vec::new()).unwrap_err().contains("no engine"));
}

#[test]
fn a_test_takes_no_capability_or_secret_whatever_it_imports() {
    let d = scratch("testing-authority");
    // A library capability, in a module a test file imports.
    fs::create_dir_all(d.join("vendor")).unwrap();
    fs::copy(Path::new(ROOT).join("tests/modules/ok_library_capability/vendor/sms.staal"), d.join("vendor/sms.staal")).unwrap();
    fs::write(
        d.join("reach_test.staal"),
        r#"import vendor.sms as vs

fn send(texts: cap vendor.sms.send(uses: 1)) -> Receipt {
    return texts.send("+1555", "hello")
}

fn test_with_a_library_capability(texts: cap vendor.sms.send(uses: 1)) {
    let r = send(texts)
}

fn test_with_the_runtimes_capability(http: cap net.http.post) {
    let r = http.post("http://127.0.0.1:1/x", "x")
}

fn test_with_a_secret(key: Secret) {
}

fn test_plain() {
}
"#,
    )
    .unwrap();
    // A test that would call code needing a capability has none to pass: the file
    // does not check.
    fs::write(d.join("borrow_test.staal"), "import vendor.sms as vs\n\nfn test_send() {\n    let r = vs.send(\"+1\", \"x\")\n}\n").unwrap();
    let settings = Settings::new(None, engines()).unwrap();
    let got = cases(&[d.join("reach_test.staal"), d.join("borrow_test.staal")], &settings);
    let by = |t: &str| got.iter().find(|c| c.test.as_deref() == Some(t)).unwrap_or_else(|| panic!("{t}: {got:?}")).outcome.clone();
    for t in ["test_with_a_library_capability", "test_with_the_runtimes_capability", "test_with_a_secret"] {
        assert_eq!(by(t), Outcome::Failed("a test takes no parameters and gives no result".into()), "{t}");
    }
    assert_eq!(by("test_plain"), Outcome::Passed);
    let borrow = got.iter().find(|c| c.file.ends_with("borrow_test.staal")).unwrap();
    assert_eq!((borrow.test.as_deref(), &borrow.outcome), (None, &Outcome::Failed("it has check errors".into())));
    assert!(!borrow.diagnostics.is_empty() && borrow.diagnostics.iter().all(|m| m.contains("borrow_test.staal")), "{:?}", borrow.diagnostics);
}

#[test]
fn a_test_asking_a_model_gets_only_the_scripted_replies() {
    let d = scratch("testing-model");
    fs::write(
        d.join("m_test.staal"),
        "type Name { name: Text }\n\nfn test_asks() {\n    let n = validate propose(Name, tokens: 200)\n    if n.name != \"Ada\" {\n        fail(\"got \\(n.name)\")\n    }\n}\n",
    )
    .unwrap();
    let files = [d.join("m_test.staal")];
    let none = cases(&files, &Settings::new(None, engines()).unwrap());
    assert!(matches!(&none[0].outcome, Outcome::Failed(why) if why.starts_with("provider: ")), "{none:?}");
    let one = json!({ "model": { "provider": "script", "responses": [{ "output": { "name": "Ada" }, "output_tokens": 3 }] } });
    assert_eq!(cases(&files, &Settings::new(Some(&one), engines()).unwrap())[0].outcome, Outcome::Passed);
}

#[test]
fn the_command_line_prints_what_the_library_returns() {
    let d = scratch("testing-same");
    fs::write(d.join("a_test.staal"), "fn test_passes() {\n}\n\nfn test_fails() {\n    fail(\"no\")\n}\n\nfn test_takes(n: Int) {\n}\n").unwrap();
    fs::write(d.join("b_test.staal"), "fn test_bad() -> Int {\n    return \"x\"\n}\n").unwrap();
    fs::write(d.join("c_test.staal"), "fn helper() {\n}\n").unwrap();
    let files: Vec<PathBuf> = ["a_test.staal", "b_test.staal", "c_test.staal"].iter().map(|f| d.join(f)).collect();
    let mut got = Vec::new();
    let sum = staalc::testing::run(&files, &Settings::new(None, engines()).unwrap(), |c| got.push(c.clone())).unwrap();
    let mut expected: Vec<String> = got
        .iter()
        .map(|c| match &c.outcome {
            Outcome::Passed => format!("ok   {}", c.id()),
            Outcome::Failed(why) => format!("FAIL {}: {why}", c.id()),
        })
        .collect();
    expected.push(format!("{} passed, {} failed", sum.passed, sum.failed));
    assert_eq!((sum.passed, sum.failed), (1, 4), "{got:?}");
    let o = Command::new(BIN).arg("test").args(&files).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let printed: Vec<String> = String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect();
    assert_eq!(printed, expected);
    let diagnostics = &got.iter().find(|c| c.file.ends_with("b_test.staal")).unwrap().diagnostics;
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(!diagnostics.is_empty() && diagnostics.iter().all(|m| stderr.contains(m.as_str())), "{stderr}");
}
