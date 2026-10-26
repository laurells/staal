// Running without a config file (`--entry`, `--input`), runs without a model, and
// `staalc config`, which writes a config to fill in.

use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const PROGRAM: &str = "tests/runconfig/prog.staal";

fn staalc(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(BIN).args(args).current_dir(ROOT).output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
}

fn engines() -> Vec<&'static str> {
    if cfg!(feature = "native") { vec!["interpreter", "native"] } else { vec!["interpreter"] }
}

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("runconfig").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_program_runs_from_the_command_line() {
    for engine in engines() {
        let e = ["--engine", engine];
        assert_eq!(staalc(&[&["run", PROGRAM][..], &e].concat()).1, "42", "{engine}: main is the default");
        let (code, out, err) = staalc(&[&["run", PROGRAM, "--entry", "describe", "--input", "name=two words", "--input", "count=3", "--input", "urgent=true"][..], &e].concat());
        assert_eq!((code, out.as_str()), (Some(0), "\"two words x3 urgent=true note=none\""), "{engine}: {err}");
        // A Text is taken as written, even when it looks like JSON.
        let (_, out, _) = staalc(&[&["run", PROGRAM, "--entry", "describe", "--input", "name=5", "--input", "count=1", "--input", "urgent=false", "--input", "note=\"q\""][..], &e].concat());
        assert_eq!(out, r#""5 x1 urgent=false note=\"q\"""#, "{engine}");
        let items = r#"items=[{"name": "a", "price": {"amount": "1.00", "currency": "NGN"}}]"#;
        let (code, out, err) = staalc(&[
            &["run", PROGRAM, "--entry", "shapes", "--input", items, "--input", r#"size={"kind": "Large", "extra": 2}"#, "--input", "maybe=null"][..],
            &["--input", r#"pair=[1, "x"]"#, "--input", r#"tags=["a"]"#, "--input", r#"stock={"a": 1}"#],
            &e,
        ]
        .concat());
        assert_eq!((code, out.as_str()), (Some(0), "1"), "{engine}: {err}");
    }
}

#[test]
fn what_the_command_line_cannot_give_is_refused() {
    for (args, why) in [
        (vec!["--entry", "nope"], "has no function 'nope'"),
        (vec!["--entry", "send", "--input", "to=a@b.c"], "'send' needs mail (cap email.send(uses: 3)), key (Secret), which only a run config can grant; `staalc config` writes one"),
        (vec!["--entry", "describe", "--input", "count=three"], "--input count: 'three' is not JSON; a Int is given as JSON (only Text is given as written), e.g. 0"),
        (vec!["--entry", "describe", "--input", "colour=red"], "'describe' has no parameter 'colour'"),
        (vec!["--entry", "describe", "--input", "name"], "--input name: expected name=value"),
        (vec!["--entry", "describe", "--input", "name=a", "--input", "name=b"], "--input name is given twice"),
        (vec!["--entry", "describe", "--input", "name=a"], "inputs.count: missing"),
        (vec!["--entry", "describe", "--input", "name=a", "--input", "count=1", "--input", "urgent=1"], "inputs.urgent: expected true or false"),
        (vec!["--config", "tests/run/basics__div_truncates.json", "--input", "a=1"], "give --entry and --input without --config, or put them in it"),
    ] {
        let (code, _, err) = staalc(&[&["run", PROGRAM][..], &args].concat());
        assert_eq!(code, Some(2), "{args:?}: {err}");
        assert!(err.contains(why), "{args:?}: {err}");
    }
    // The checks come first for a program without `main` too.
    let (code, _, err) = staalc(&["run", "tests/ffi/calls.staal"]);
    assert_eq!(code, Some(2));
    assert!(err.contains("no function `main`: name the function to start with --entry"), "{err}");
}

#[test]
fn a_run_without_a_model_says_so_and_records_nothing() {
    let dir = scratch("nomodel");
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let (code, _, err) = staalc(&["run", PROGRAM, "--entry", "ask", "--input", "q=hi", "--journal", &j]);
    assert_eq!(code, Some(1));
    assert!(err.contains("provider: this run names no model"), "{err}");
    let text = std::fs::read_to_string(&j).unwrap();
    assert!(!text.contains("\"count\"") && !text.contains("\"model\""), "something was recorded: {text}");
}

#[test]
fn a_run_from_the_command_line_resumes_only_with_the_same_inputs() {
    let dir = scratch("resume");
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let args = ["--entry", "describe", "--input", "name=a", "--input", "count=1", "--input", "urgent=true"];
    let (code, out, err) = staalc(&[&["run", PROGRAM][..], &args, &["--journal", &j]].concat());
    assert_eq!((code, out.as_str()), (Some(0), "\"a x1 urgent=true note=none\""), "{err}");
    let (code, out, err) = staalc(&[&["resume", PROGRAM][..], &args, &["--journal", &j]].concat());
    assert_eq!((code, out.as_str()), (Some(0), "\"a x1 urgent=true note=none\""), "{err}");
    let (code, _, err) = staalc(&["resume", PROGRAM, "--entry", "describe", "--input", "name=b", "--input", "count=1", "--input", "urgent=true", "--journal", &j]);
    assert_eq!(code, Some(2));
    assert!(err.contains("config"), "{err}");
}

#[test]
fn staalc_config_writes_a_config_to_fill_in() {
    let skeleton = |entry: &str| {
        let (code, out, err) = staalc(&["config", PROGRAM, "--entry", entry]);
        assert_eq!(code, Some(0), "{err}");
        serde_json::from_str::<Json>(&out).unwrap()
    };
    // An input with a default is left out; the rest get a placeholder of their type.
    assert_eq!(skeleton("describe"), json!({ "entry": "describe", "inputs": { "name": "", "count": 0, "urgent": false } }));
    assert_eq!(
        skeleton("shapes")["inputs"],
        json!({ "items": [{ "name": "", "price": { "amount": "0.00", "currency": "USD" } }], "size": { "kind": "Small" },
                "maybe": null, "pair": [0, ""], "tags": [], "stock": {} })
    );
    // Capabilities with the limits the code declares; secrets from the environment.
    assert_eq!(
        skeleton("send"),
        json!({ "entry": "send", "inputs": { "to": "" }, "grants": { "mail": { "cap": "email.send", "uses": 3 } },
                "secrets": { "key": { "env": "KEY" } } })
    );
    // A model only where the program asks one.
    assert_eq!(skeleton("ask")["model"]["provider"], "anthropic");
    // A library capability's grant holds what it needs, its secrets named after it.
    let (code, out, err) = staalc(&["config", "tests/fixtures/pass/library_capability.staal", "--entry", "notify"]);
    assert_eq!(code, Some(0), "{err}");
    let lib: Json = serde_json::from_str(&out).unwrap();
    assert_eq!(
        lib["grants"]["sms"],
        json!({ "cap": "clinic.sms.send", "uses": 1,
                "needs": { "http": { "cap": "net.http.post", "uses": 100 }, "key": { "env": "CLINIC_SMS_KEY" }, "base": "" } }),
        "{lib}"
    );
    let (_, out, _) = staalc(&["config", "tests/fixtures/pass/library_capability.staal", "--entry", "give_back"]);
    let lib: Json = serde_json::from_str(&out).unwrap();
    assert_eq!(lib["grants"]["r"]["max"], json!({ "amount": "50", "currency": "USD" }), "{lib}");
    // What it writes is a config the runtime takes: the placeholders are values of
    // their types.
    let dir = scratch("skeleton");
    for (entry, want) in [("describe", "\" x0 urgent=false note=none\""), ("shapes", "1"), ("main", "42")] {
        let p = dir.join(format!("{entry}.json"));
        std::fs::write(&p, skeleton(entry).to_string()).unwrap();
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", p.to_str().unwrap()]);
        assert_eq!((code, out.as_str()), (Some(0), want), "{entry}: {err}");
    }
    let p = dir.join("send.json");
    std::fs::write(&p, skeleton("send").to_string()).unwrap();
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", p.to_str().unwrap()]);
    assert_eq!(code, Some(2));
    assert!(err.contains("environment variable KEY is not set"), "only the secret is left to fill in: {err}");
}

#[test]
fn profile_and_debug_run_without_a_config_too() {
    let (code, out, err) = staalc(&["profile", PROGRAM, "--entry", "describe", "--input", "name=a", "--input", "count=1", "--input", "urgent=true"]);
    assert_eq!((code, out.as_str()), (Some(0), "\"a x1 urgent=true note=none\""), "{err}");
}
