// C libraries behind `cap ffi.<library>.<function>` (phase 12), against a library
// built here from tests/ffi/staaltest.c: values cross the boundary as declared, on
// both engines; only the pinned file is loaded; every run that loads one is marked
// unchecked; and calls are journaled, replayed, and never made twice after a crash
// unless the grant says the function may be.

#![cfg(all(unix, any(target_arch = "x86_64", target_arch = "aarch64")))]

use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const PROGRAM: &str = "tests/ffi/calls.staal";

/// The test library, built once: its path and SHA-256.
fn library() -> (String, String) {
    static BUILT: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ffi-lib");
            std::fs::create_dir_all(&dir).unwrap();
            let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
            let out = dir.join(format!("libstaaltest.{ext}"));
            let o = Command::new("cc")
                .args(["-shared", "-fPIC", "-O1", "-o"])
                .arg(&out)
                .arg(Path::new(ROOT).join("tests/ffi/staaltest.c"))
                .output()
                .expect("a C compiler (cc) builds the test library");
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            let sha = staalc::packages::sha256_hex(&std::fs::read(&out).unwrap());
            (out.to_string_lossy().into_owned(), sha)
        })
        .clone()
}

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ffi").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A grant of C function `f` of the test library.
fn grant(f: &str) -> Json {
    let (lib, sha) = library();
    json!({ "cap": format!("ffi.staaltest.{f}"), "library": lib, "sha256": sha })
}

fn config(dir: &Path, entry: &str, grants: Json, inputs: Json) -> String {
    let cfg = json!({ "entry": entry, "inputs": inputs, "grants": grants, "model": { "provider": "script", "responses": [] } });
    let p = dir.join("cfg.json");
    std::fs::write(&p, cfg.to_string()).unwrap();
    p.to_string_lossy().into_owned()
}

fn staalc(args: &[&str], failpoint: Option<&str>) -> (Option<i32>, String, String) {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(ROOT);
    match failpoint {
        Some(fp) => c.env("STAALC_FAILPOINT", fp),
        None => c.env_remove("STAALC_FAILPOINT"),
    };
    let o = c.output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
}

fn engines() -> Vec<&'static str> {
    if cfg!(feature = "native") { vec!["interpreter", "native"] } else { vec!["interpreter"] }
}

fn numbers_grants() -> Json {
    json!({
        "add": grant("add64"), "neg": grant("neg32"), "i8": grant("take_i8"), "u16": grant("take_u16"),
        "low": grant("low_byte"), "wrap": grant("as_i8"), "even": grant("is_even"), "len": grant("text_len"),
        "hello": grant("greeting"), "six": grant("sum6"),
    })
}

#[test]
fn values_cross_the_boundary_as_declared() {
    let dir = scratch("numbers");
    let cfg = config(&dir, "numbers", numbers_grants(), json!({}));
    for engine in engines() {
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine], None);
        assert_eq!(code, Some(0), "{engine}: {err}");
        let v: Json = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v,
            json!({ "sum": 9007199254740990i64, "neg": 2147483647, "i8": -128, "u16": 65535, "low": 255, "wrapped": -56,
                    "even": true, "odd": false, "len": 6, "hello": "hello from C", "six": 654321 }),
            "{engine}"
        );
    }
}

#[test]
fn a_run_that_loads_c_is_marked_unchecked() {
    let dir = scratch("unchecked");
    let cfg = config(&dir, "length", json!({ "len": grant("text_len") }), json!({ "s": "abc" }));
    let log = dir.join("run.jsonl");
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--log", log.to_str().unwrap()], None);
    assert_eq!((code, out.as_str()), (Some(0), "3"), "{err}");
    assert!(err.contains("this run is unchecked") && err.contains("ffi.staaltest.text_len"), "{err}");
    let events: Vec<Json> = std::fs::read_to_string(&log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let u = events.iter().find(|e| e["event"] == "unchecked").expect("an unchecked event");
    assert_eq!((u["capability"].as_str(), u["sha256"].as_str()), (Some("ffi.staaltest.text_len"), Some(library().1.as_str())));
    // It comes before anything the program does.
    let first_action = events.iter().position(|e| e["event"] == "action_requested").unwrap();
    assert!(events.iter().position(|e| e["event"] == "unchecked").unwrap() < first_action);
    // The library API says so too.
    let checked = staalc::check_path(&Path::new(ROOT).join(PROGRAM)).unwrap();
    let prog = checked.result.unwrap();
    let cfg_json: Json = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    let r = staalc::interp::run_config(&prog, &cfg_json).unwrap();
    assert_eq!(r.unchecked(), vec!["ffi.staaltest.text_len".to_string()]);
    // And the C interface.
    let prog_c = std::ffi::CString::new(Path::new(ROOT).join(PROGRAM).to_str().unwrap()).unwrap();
    let cfg_c = std::ffi::CString::new(cfg_json.to_string()).unwrap();
    // SAFETY: NUL-terminated strings that outlive the call; the result is freed once.
    let out = unsafe { staalc::capi::staal_run(prog_c.as_ptr(), cfg_c.as_ptr(), std::ptr::null(), std::ptr::null()) };
    let text = unsafe { std::ffi::CStr::from_ptr(out) }.to_str().unwrap().to_string();
    unsafe { staalc::capi::staal_free(out) };
    let v: Json = serde_json::from_str(&text).unwrap();
    assert_eq!((&v["status"], &v["unchecked"]), (&json!("done"), &json!(["ffi.staaltest.text_len"])), "{v}");
    // A run of the same program granted no C function is not.
    let cfg = config(&dir, "plain", json!({}), json!({}));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert_eq!(code, Some(0));
    assert!(!err.contains("unchecked"), "{err}");
}

#[test]
fn values_that_do_not_fit_are_refused() {
    let dir = scratch("fit");
    for (n, ok) in [(-128, true), (127, true), (128, false), (-129, false)] {
        let cfg = config(&dir, "narrow", json!({ "i8": grant("take_i8") }), json!({ "n": n }));
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
        if ok {
            assert_eq!((code, out), (Some(0), n.to_string()), "{err}");
        } else {
            assert_eq!(code, Some(1), "{n}");
            assert!(err.contains(&format!("{n} does not fit in a C i8")), "{err}");
        }
    }
    // Refused before the call: nothing was recorded as attempted.
    let j = dir.join("refused.journal");
    let cfg = config(&dir, "narrow", json!({ "i8": grant("take_i8") }), json!({ "n": 1000 }));
    staalc(&["run", PROGRAM, "--config", &cfg, "--journal", j.to_str().unwrap()], None);
    assert!(!std::fs::read_to_string(&j).unwrap().contains("\"intent\""));
    for (entry, slot, f, why) in [
        ("too_big", "big", "too_big", "does not fit in Int"),
        ("no_text", "t", "no_text", "returned no text (a null pointer)"),
        ("not_utf8", "t", "not_utf8", "not UTF-8"),
    ] {
        let cfg = config(&dir, entry, json!({ slot: grant(f) }), json!({}));
        let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
        assert_eq!(code, Some(1), "{entry}");
        assert!(err.contains(why), "{entry}: {err}");
    }
    let cfg = config(&dir, "length", json!({ "len": grant("text_len") }), json!({ "s": "a\u{0}b" }));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert_eq!(code, Some(1));
    assert!(err.contains("cannot contain a NUL"), "{err}");
}

#[test]
fn only_the_pinned_library_is_loaded() {
    let dir = scratch("pinned");
    let (lib, sha) = library();
    let run = |g: Json| {
        let cfg = config(&dir, "length", json!({ "len": g }), json!({ "s": "x" }));
        staalc(&["run", PROGRAM, "--config", &cfg], None)
    };
    let wrong = "0".repeat(64);
    for (g, why) in [
        (json!({ "cap": "ffi.staaltest.text_len", "library": lib, "sha256": wrong }), "not the 0000"),
        (json!({ "cap": "ffi.staaltest.text_len", "library": lib }), "sha256: the SHA-256"),
        (json!({ "cap": "ffi.staaltest.text_len", "sha256": sha }), "library: the C library's file"),
        (json!({ "cap": "ffi.staaltest.text_len", "library": "libstaaltest.so", "sha256": sha }), "is not an absolute path"),
        (json!({ "cap": "ffi.staaltest.text_len", "library": lib, "sha256": sha.to_uppercase() }), "lowercase hex"),
        (json!({ "cap": "ffi.staaltest.text_len", "library": lib, "sha256": sha, "only": ["x"] }), "no target for `only`"),
        (json!({ "cap": "ffi.staaltest.text_len", "library": lib, "sha256": sha, "max": { "amount": "1.00", "currency": "USD" } }), "max and each do not apply"),
        (json!({ "cap": "ffi.staaltest.text_len", "library": lib, "sha256": sha, "repeatable": "yes" }), "expected true or false"),
        (json!({ "cap": "ffi.staaltest.text_len", "library": lib, "sha256": sha, "idempotency_key": true }), "idempotency_key"),
        (json!({ "cap": "ffi.staaltest.text_len", "library": "/nonexistent/lib.so", "sha256": sha }), "cannot open"),
    ] {
        let (code, _, err) = run(g.clone());
        assert_eq!(code, Some(2), "{g}");
        assert!(err.contains(why), "{g}: {err}");
    }
    // A function the library does not have.
    let cfg = config(&dir, "missing", json!({ "m": grant("missing") }), json!({}));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert_eq!(code, Some(2));
    assert!(err.contains("has no function missing"), "{err}");
    // A library anyone could change.
    let open = dir.join("libopen.so");
    std::fs::copy(&lib, &open).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o666)).unwrap();
    let (code, _, err) = run(json!({ "cap": "ffi.staaltest.text_len", "library": open.to_str().unwrap(), "sha256": sha }));
    assert_eq!(code, Some(2));
    assert!(err.contains("can be changed by any user"), "{err}");
}

#[test]
fn calls_are_journaled_and_replayed_without_calling_again() {
    let dir = scratch("journal");
    let out_file = dir.join("lines.txt");
    let grants = json!({ "append": grant("append_line"), "next": grant("next") });
    let cfg = config(&dir, "effects", grants, json!({ "path": out_file.to_str().unwrap() }));
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], None);
    assert_eq!((code, out.as_str()), (Some(0), "1"), "{err}");
    for engine in engines() {
        let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j, "--engine", engine], None);
        assert_eq!((code, out.as_str()), (Some(0), "1"), "{engine}: {err}");
        assert!(err.contains("this run is unchecked"), "{err}");
    }
    assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "one\ntwo\n", "a replay called C again");
    // The journal holds a hash of the arguments, not the arguments.
    let text = std::fs::read_to_string(&j).unwrap();
    assert!(text.contains("args_sha256") && !text.contains("lines.txt"), "{text}");
}

/// Needs the crash injection of `--features failpoints`.
#[cfg(feature = "failpoints")]
#[test]
fn a_call_cut_short_is_in_doubt_unless_repeatable() {
    let dir = scratch("doubt");
    let out_file = dir.join("lines.txt");
    let inputs = json!({ "path": out_file.to_str().unwrap() });
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let cfg = config(&dir, "effects", json!({ "append": grant("append_line"), "next": grant("next") }), inputs.clone());
    // Killed after the first line was appended, before its result was recorded.
    let (code, _, _) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], Some("before_result:1"));
    assert_eq!(code, None);
    assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "one\n");
    let (code, _, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("may or may not have happened"), "{err}");
    let (code, _, err) = staalc(&["resolve", "--journal", &j, "--performed", "x"], None);
    assert_eq!(code, Some(2));
    assert!(err.contains("is a call of a C function: record --failed <reason>, or --retry"), "{err}");
    let (code, _, err) = staalc(&["resolve", "--journal", &j, "--failed", "checked: the line is there"], None);
    assert_eq!(code, Some(0), "{err}");
    let (code, _, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
    assert_eq!(code, Some(1));
    assert!(err.contains("checked: the line is there"), "{err}");
    assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "one\n", "nothing was called again");

    // A repeatable function is called again on resume by itself.
    std::fs::remove_file(&out_file).unwrap();
    let mut append = grant("append_line");
    append["repeatable"] = json!(true);
    let cfg = config(&dir, "effects", json!({ "append": append, "next": grant("next") }), inputs);
    let j2 = dir.join("run2.journal").to_string_lossy().into_owned();
    let (code, _, _) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j2], Some("before_result:1"));
    assert_eq!(code, None);
    let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j2], None);
    assert_eq!((code, out.as_str()), (Some(0), "1"), "{err}");
    assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "one\none\ntwo\n", "the repeatable call was made again, once");
}

#[test]
fn a_c_function_that_crashes_leaves_its_call_in_doubt() {
    let dir = scratch("crash");
    let cfg = config(&dir, "crash", json!({ "c": grant("crash") }), json!({}));
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let (code, _, _) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], None);
    assert_eq!(code, None, "abort() ends the process with a signal");
    // Resume does not call it again.
    let (code, _, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("ffi.staaltest.crash"), "{err}");
}

#[test]
fn concurrent_calls_are_made_one_at_a_time() {
    let dir = scratch("parallel");
    let cfg = config(&dir, "counts", json!({ "next": grant("next") }), json!({}));
    for engine in engines() {
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine], None);
        // Eight calls of a counter, each seeing the last one finished: 1 + ... + 8.
        assert_eq!((code, out.as_str()), (Some(0), "36"), "{engine}: {err}");
    }
}

#[test]
fn uses_limits_the_calls() {
    let dir = scratch("uses");
    let mut g = grant("next");
    g["uses"] = json!(1);
    let cfg = config(&dir, "twice", json!({ "next": g }), json!({}));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert_eq!(code, Some(1));
    assert!(err.contains("capability"), "{err}");
    let cfg = config(&dir, "twice", json!({ "next": grant("next") }), json!({}));
    assert_eq!(staalc(&["run", PROGRAM, "--config", &cfg], None).1, "2");
}

#[test]
fn the_documentation_marks_c_functions_unchecked() {
    let (code, out, err) = staalc(&["doc", PROGRAM], None);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("## C libraries (unchecked)") && out.contains("foreign ffi.staaltest {"), "{out}");
}
