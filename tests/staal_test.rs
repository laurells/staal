// `staalc test` and the tests written in Staal (tests/staal): the suite passes in
// each engine, and the command reports passes, failures, refusals and problems
// exactly, with the exit codes it documents.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn staalc(args: &[&str], dir: &Path) -> Output {
    Command::new(BIN).args(args).current_dir(dir).output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn the_tests_written_in_staal_pass_in_each_engine() {
    let engines: &[&str] = if cfg!(feature = "native") { &["both", "interpreter", "native"] } else { &["interpreter"] };
    for e in engines {
        let o = staalc(&["test", "--engine", e, "tests/staal"], Path::new(ROOT));
        let text = out(&o);
        assert_eq!(o.status.code(), Some(0), "--engine {e}:\n{text}{}", err(&o));
        let ok = text.lines().filter(|l| l.starts_with("ok   tests/staal/")).count();
        assert!(ok >= 30, "only {ok} tests");
        assert_eq!(text.lines().last(), Some(format!("{ok} passed, 0 failed").as_str()));
        // check.staal, which has no tests, is not a test file and is not run.
        assert!(!text.contains("check.staal"));
    }
}

#[test]
fn passes_failures_and_refusals_are_reported_exactly() {
    let d = scratch("staal-test-report");
    fs::write(
        d.join("a_test.staal"),
        r#"import lib

fn helper() -> Int {
    return lib.two()
}

fn test_passes() {
    if helper() != 2 {
        fail("helper changed")
    }
}

fn test_fails() {
    let n = helper() + 1
    if n != 4 {
        fail("expected 4, got \(n)")
    }
}

fn test_takes_a_parameter(n: Int) {
}

fn test_gives_a_result() -> Int {
    return 1
}

fn test_fails_as_a_value_does() {
    let n = helper() - 2
    let x = 1 / n
}
"#,
    )
    .unwrap();
    // A module's functions are not the test file's tests, whatever their names.
    fs::write(d.join("lib.staal"), "pub fn two() -> Int {\n    return 2\n}\n\npub fn test_in_a_module() {\n    fail(\"run\")\n}\n").unwrap();
    fs::create_dir_all(d.join("sub/.hidden")).unwrap();
    fs::write(d.join("sub/.hidden/x_test.staal"), "fn test_hidden() { fail(\"run\") }\n").unwrap();
    fs::write(d.join("sub/b_test.staal"), "fn test_broken() { return y }\n").unwrap();
    fs::write(d.join("sub/c_test.staal"), "fn not_a_test() {\n}\n").unwrap();
    fs::write(d.join("sub/notes.staal"), "fn test_not_in_a_test_file() { fail(\"run\") }\n").unwrap();
    let o = staalc(&["test", "."], &d);
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    assert_eq!(
        out(&o),
        "ok   ./a_test.staal::test_passes
FAIL ./a_test.staal::test_fails: action: expected 4, got 3
FAIL ./a_test.staal::test_takes_a_parameter: a test takes no parameters and gives no result
FAIL ./a_test.staal::test_gives_a_result: a test takes no parameters and gives no result
FAIL ./a_test.staal::test_fails_as_a_value_does: value: division by zero: 1 / 0
FAIL ./sub/b_test.staal: it has check errors
FAIL ./sub/c_test.staal: no tests (functions named test_...)
1 passed, 6 failed
"
    );
    // The check errors are printed as `staalc check` prints them.
    assert!(err(&o).contains("b_test.staal:1:27: error[E100]: undefined name 'y'"), "{}", err(&o));

    // A file given by name is run whatever its name; one passing test is exit 0.
    fs::write(d.join("one.staal"), "fn test_one() {\n}\n").unwrap();
    let o = staalc(&["test", "one.staal"], &d);
    assert_eq!((o.status.code(), out(&o).as_str()), (Some(0), "ok   one.staal::test_one\n1 passed, 0 failed\n"));
}

#[test]
fn a_test_gets_the_model_of_its_config_and_each_run_is_fresh() {
    let d = scratch("staal-test-model");
    fs::write(
        d.join("m_test.staal"),
        r#"type Name { name: Text }

fn test_asks_a_model() {
    let n = validate propose(Name, tokens: 200)
    if n.name != "Ada" {
        fail("got \(n.name)")
    }
}

fn test_prints() {
    print("hello from a test")
}
"#,
    )
    .unwrap();
    // Without a config the model has no replies, so asking it fails.
    let o = staalc(&["test", "m_test.staal"], &d);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("FAIL m_test.staal::test_asks_a_model: provider: "), "{}", out(&o));
    // With one reply, every test in every engine gets that reply: each run is fresh.
    fs::write(d.join("t.json"), r#"{"model": {"provider": "script", "responses": [{"output": {"name": "Ada"}, "output_tokens": 3}]}}"#).unwrap();
    let o = staalc(&["test", "--config", "t.json", "m_test.staal"], &d);
    assert_eq!(o.status.code(), Some(0), "{}{}", out(&o), err(&o));
    // Each engine runs the test, and each prints.
    let engines = if cfg!(feature = "native") { 2 } else { 1 };
    assert_eq!(out(&o).matches("hello from a test\n").count(), engines);
}

#[test]
fn problems_that_are_not_test_failures_are_exit_2() {
    let d = scratch("staal-test-usage");
    fs::write(d.join("a_test.staal"), "fn test_a() {\n}\n").unwrap();
    fs::write(d.join("entry.json"), r#"{"entry": "test_a", "model": {"provider": "script", "responses": []}}"#).unwrap();
    fs::write(d.join("bad.json"), "{").unwrap();
    fs::write(d.join("model.json"), r#"{"model": {"provider": "nobody"}}"#).unwrap();
    for (args, says) in [
        (&["test"][..], "test needs a file or a folder"),
        (&["test", "--engine", "fast", "a_test.staal"], "--engine is interpreter, native or both, not fast"),
        (&["test", "--engine"], "--engine needs a value"),
        (&["test", "--config", "bad.json", "--config", "bad.json", "a_test.staal"], "--config is given twice"),
        (&["test", "--quick", "a_test.staal"], "unknown option --quick"),
        (&["test", "--config", "entry.json", "a_test.staal"], "a test config cannot give \"entry\""),
        (&["test", "--config", "bad.json", "a_test.staal"], "bad.json is not valid JSON"),
        (&["test", "--config", "missing.json", "a_test.staal"], "cannot read missing.json"),
        (&["test", "--config", "model.json", "a_test.staal"], "model.provider"),
        (&["test", "missing_test.staal"], "missing_test.staal"),
    ] {
        let o = staalc(args, &d);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}{}", out(&o), err(&o));
        assert!(err(&o).contains(says), "{args:?}: {}", err(&o));
    }
    // A folder with no test files is a failure to test anything: exit 1.
    fs::create_dir_all(d.join("empty")).unwrap();
    let o = staalc(&["test", "empty"], &d);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no *_test.staal files under empty"));
}
