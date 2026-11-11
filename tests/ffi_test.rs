// C libraries behind `cap ffi.<library>.<function>` (phase 12, RUNTIME.md section
// 19), against a library built here from tests/ffi/staaltest.c: values cross the
// boundary as declared, on both engines and however the C code is run; only the pinned
// file is loaded; calls are journaled, replayed, and never made twice after a crash
// unless that is safe. And isolation: a contained C function (the default, on Linux)
// cannot reach anything outside its worker, by any of the ways tried here, its
// library's initialiser included; a run is unchecked only when its C code is not
// contained.

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

/// A grant of C function `f`, run as `isolation` says.
fn grant_as(f: &str, isolation: &str) -> Json {
    let mut g = grant(f);
    g["isolation"] = json!(isolation);
    g
}

/// Every isolation this machine can run C code with.
fn isolations() -> Vec<&'static str> {
    if staalc::interp::ffi::can_contain() { vec!["contained", "separate", "none"] } else { vec!["separate", "none"] }
}

/// Runs `body` only where C code can be contained.
fn where_contained() -> bool {
    staalc::interp::ffi::can_contain()
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

fn numbers_grants(isolation: &str) -> Json {
    let g = |f: &str| grant_as(f, isolation);
    json!({
        "add": g("add64"), "neg": g("neg32"), "i8": g("take_i8"), "u16": g("take_u16"),
        "low": g("low_byte"), "wrap": g("as_i8"), "even": g("is_even"), "len": g("text_len"),
        "hello": g("greeting"), "six": g("sum6"),
    })
}

#[test]
fn values_cross_the_boundary_as_declared() {
    let dir = scratch("numbers");
    for isolation in isolations() {
        let cfg = config(&dir, "numbers", numbers_grants(isolation), json!({}));
        for engine in engines() {
            let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine], None);
            assert_eq!(code, Some(0), "{isolation} {engine}: {err}");
            let v: Json = serde_json::from_str(&out).unwrap();
            assert_eq!(
                v,
                json!({ "sum": 9007199254740990i64, "neg": 2147483647, "i8": -128, "u16": 65535, "low": 255, "wrapped": -56,
                        "even": true, "odd": false, "len": 6, "hello": "hello from C", "six": 654321 }),
                "{isolation} {engine}"
            );
        }
    }
}

/// The run's log events, from a `--log` file.
fn events(log: &Path) -> Vec<Json> {
    std::fs::read_to_string(log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[test]
fn a_run_is_unchecked_unless_its_c_is_contained() {
    let dir = scratch("unchecked");
    for isolation in isolations() {
        let cfg = config(&dir, "length", json!({ "len": grant_as("text_len", isolation) }), json!({ "s": "abc" }));
        let log = dir.join(format!("{isolation}.jsonl"));
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--log", log.to_str().unwrap()], None);
        assert_eq!((code, out.as_str()), (Some(0), "3"), "{isolation}: {err}");
        let ev = events(&log);
        let first_action = ev.iter().position(|e| e["event"] == "action_requested").unwrap();
        let cfg_json: Json = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        let checked = staalc::check_path(&Path::new(ROOT).join(PROGRAM)).unwrap();
        let prog = checked.result.unwrap();
        // The library API and the C interface, in this process: their worker is the
        // staalc these tests run.
        staalc::interp::ffi_worker::use_executable(PathBuf::from(BIN));
        let r = staalc::interp::run_config(&prog, &cfg_json).unwrap();
        let prog_c = std::ffi::CString::new(Path::new(ROOT).join(PROGRAM).to_str().unwrap()).unwrap();
        let cfg_c = std::ffi::CString::new(cfg_json.to_string()).unwrap();
        // SAFETY: NUL-terminated strings that outlive the call; the result is freed once.
        let c_out = unsafe { staalc::capi::staal_run(prog_c.as_ptr(), cfg_c.as_ptr(), std::ptr::null(), std::ptr::null()) };
        let text = unsafe { std::ffi::CStr::from_ptr(c_out) }.to_str().unwrap().to_string();
        unsafe { staalc::capi::staal_free(c_out) };
        let v: Json = serde_json::from_str(&text).unwrap();
        assert_eq!(v["status"], json!("done"), "{isolation}: {v}");
        if isolation == "contained" {
            assert!(!err.contains("unchecked"), "{err}");
            assert!(!ev.iter().any(|e| e["event"] == "unchecked"), "{ev:?}");
            let c = ev.iter().find(|e| e["event"] == "contained").expect("a contained event");
            assert_eq!((c["capability"].as_str(), c["sha256"].as_str()), (Some("ffi.staaltest.text_len"), Some(library().1.as_str())));
            assert!(ev.iter().position(|e| e["event"] == "contained").unwrap() < first_action);
            assert!(r.unchecked().is_empty());
            // The C interface names unchecked capabilities only when there are some.
            assert!(v.get("unchecked").is_none(), "{v}");
        } else {
            assert!(err.contains("this run is unchecked") && err.contains("ffi.staaltest.text_len"), "{isolation}: {err}");
            let u = ev.iter().find(|e| e["event"] == "unchecked").expect("an unchecked event");
            assert_eq!(
                (u["capability"].as_str(), u["sha256"].as_str(), u["isolation"].as_str()),
                (Some("ffi.staaltest.text_len"), Some(library().1.as_str()), Some(isolation))
            );
            // It comes before anything the program does.
            assert!(ev.iter().position(|e| e["event"] == "unchecked").unwrap() < first_action);
            assert_eq!(r.unchecked(), vec!["ffi.staaltest.text_len".to_string()]);
            assert_eq!(v["unchecked"], json!(["ffi.staaltest.text_len"]), "{v}");
        }
    }
    // A run of the same program granted no C function is not unchecked.
    let cfg = config(&dir, "plain", json!({}), json!({}));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert_eq!(code, Some(0));
    assert!(!err.contains("unchecked"), "{err}");
}

#[test]
fn c_is_contained_by_default_and_only_where_it_can_be() {
    let dir = scratch("default");
    let cfg = config(&dir, "length", json!({ "len": grant("text_len") }), json!({ "s": "abc" }));
    let log = dir.join("run.jsonl");
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--log", log.to_str().unwrap()], None);
    if where_contained() {
        assert_eq!((code, out.as_str()), (Some(0), "3"), "{err}");
        assert!(events(&log).iter().any(|e| e["event"] == "contained"));
    } else {
        // Elsewhere a grant must say how its C code is run instead.
        assert_eq!(code, Some(2), "{err}");
        assert!(err.contains("can be contained only on Linux") && err.contains("\"separate\" or \"none\""), "{err}");
    }
    for (bad, why) in [(json!("sideways"), "expected \"contained\", \"separate\" or \"none\""), (json!(true), "expected")] {
        let mut g = grant("text_len");
        g["isolation"] = bad.clone();
        let cfg = config(&dir, "length", json!({ "len": g }), json!({ "s": "abc" }));
        let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
        assert_eq!(code, Some(2), "{bad}");
        assert!(err.contains("isolation") && err.contains(why), "{bad}: {err}");
    }
    // One library is run one way in a run.
    let cfg = config(&dir, "numbers", json!({ "add": grant_as("add64", "separate"), "neg": grant_as("neg32", "none"),
        "i8": grant_as("take_i8", "none"), "u16": grant_as("take_u16", "none"), "low": grant_as("low_byte", "none"),
        "wrap": grant_as("as_i8", "none"), "even": grant_as("is_even", "none"), "len": grant_as("text_len", "none"),
        "hello": grant_as("greeting", "none"), "six": grant_as("sum6", "none") }), json!({}));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("one library runs one way"), "{err}");
}

#[test]
fn values_that_do_not_fit_are_refused() {
    let dir = scratch("fit");
    for isolation in isolations() {
        let g = |f: &str| grant_as(f, isolation);
        for (n, ok) in [(-128, true), (127, true), (128, false), (-129, false)] {
            let cfg = config(&dir, "narrow", json!({ "i8": g("take_i8") }), json!({ "n": n }));
            let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
            if ok {
                assert_eq!((code, out), (Some(0), n.to_string()), "{isolation}: {err}");
            } else {
                assert_eq!(code, Some(1), "{isolation} {n}");
                assert!(err.contains(&format!("{n} does not fit in a C i8")), "{isolation}: {err}");
            }
        }
        // Refused before the call: nothing was recorded as attempted.
        let j = dir.join(format!("refused-{isolation}.journal"));
        let cfg = config(&dir, "narrow", json!({ "i8": g("take_i8") }), json!({ "n": 1000 }));
        staalc(&["run", PROGRAM, "--config", &cfg, "--journal", j.to_str().unwrap()], None);
        assert!(!std::fs::read_to_string(&j).unwrap().contains("\"intent\""));
        for (entry, slot, f, why) in [
            ("too_big", "big", "too_big", "does not fit in Int"),
            ("no_text", "t", "no_text", "returned no text (a null pointer)"),
            ("not_utf8", "t", "not_utf8", "not UTF-8"),
        ] {
            let cfg = config(&dir, entry, json!({ slot: g(f) }), json!({}));
            let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
            assert_eq!(code, Some(1), "{isolation} {entry}");
            assert!(err.contains(why), "{isolation} {entry}: {err}");
        }
        let cfg = config(&dir, "length", json!({ "len": g("text_len") }), json!({ "s": "a\u{0}b" }));
        let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
        assert_eq!(code, Some(1));
        assert!(err.contains("cannot contain a NUL"), "{isolation}: {err}");
    }
}

#[test]
fn only_the_pinned_library_is_loaded() {
    let dir = scratch("pinned");
    let (lib, sha) = library();
    for isolation in isolations() {
        let run = |mut g: Json| {
            g["isolation"] = json!(isolation);
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
            assert_eq!(code, Some(2), "{isolation} {g}");
            assert!(err.contains(why), "{isolation} {g}: {err}");
        }
        // A function the library does not have.
        let cfg = config(&dir, "missing", json!({ "m": grant_as("missing", isolation) }), json!({}));
        let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
        assert_eq!(code, Some(2));
        assert!(err.contains("has no function missing"), "{isolation}: {err}");
        // A library anyone could change.
        let open = dir.join("libopen.so");
        std::fs::copy(&lib, &open).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o666)).unwrap();
        let (code, _, err) = run(json!({ "cap": "ffi.staaltest.text_len", "library": open.to_str().unwrap(), "sha256": sha }));
        assert_eq!(code, Some(2));
        assert!(err.contains("can be changed by any user"), "{isolation}: {err}");
    }
}

#[test]
fn calls_are_journaled_and_replayed_without_calling_again() {
    // C code that is not contained can append to a file, an effect replay must not repeat.
    for isolation in ["separate", "none"] {
        let dir = scratch(&format!("journal-{isolation}"));
        let out_file = dir.join("lines.txt");
        let grants = json!({ "append": grant_as("append_line", isolation), "next": grant_as("next", isolation) });
        let cfg = config(&dir, "effects", grants, json!({ "path": out_file.to_str().unwrap() }));
        let j = dir.join("run.journal").to_string_lossy().into_owned();
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], None);
        assert_eq!((code, out.as_str()), (Some(0), "1"), "{isolation}: {err}");
        for engine in engines() {
            let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j, "--engine", engine], None);
            assert_eq!((code, out.as_str()), (Some(0), "1"), "{isolation} {engine}: {err}");
            assert!(err.contains("this run is unchecked"), "{err}");
        }
        assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "one\ntwo\n", "{isolation}: a replay called C again");
        // The journal holds a hash of the arguments, not the arguments.
        let text = std::fs::read_to_string(&j).unwrap();
        assert!(text.contains("args_sha256") && !text.contains("lines.txt"), "{text}");
    }
}

/// Needs the crash injection of `--features failpoints`.
#[cfg(feature = "failpoints")]
#[test]
fn a_call_cut_short_is_in_doubt_unless_repeatable_or_contained() {
    for isolation in ["separate", "none"] {
        let dir = scratch(&format!("doubt-{isolation}"));
        let out_file = dir.join("lines.txt");
        let inputs = json!({ "path": out_file.to_str().unwrap() });
        let j = dir.join("run.journal").to_string_lossy().into_owned();
        let g = |f: &str| grant_as(f, isolation);
        let cfg = config(&dir, "effects", json!({ "append": g("append_line"), "next": g("next") }), inputs.clone());
        // Killed after the first line was appended, before its result was recorded.
        let (code, _, _) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], Some("before_result:1"));
        assert_eq!(code, None);
        assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "one\n");
        let (code, _, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
        assert_eq!(code, Some(3), "{isolation}: {err}");
        assert!(err.contains("may or may not have happened"), "{err}");
        let (code, _, err) = staalc(&["resolve", "--journal", &j, "--performed", "x"], None);
        assert_eq!(code, Some(2));
        assert!(err.contains("is a call of a C function: record --failed <reason>, or --retry"), "{err}");
        let (code, _, err) = staalc(&["resolve", "--journal", &j, "--failed", "checked: the line is there"], None);
        assert_eq!(code, Some(0), "{err}");
        let (code, _, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
        assert_eq!(code, Some(1));
        assert!(err.contains("checked: the line is there"), "{err}");
        assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "one\n", "{isolation}: nothing was called again");

        // A repeatable function is called again on resume by itself.
        std::fs::remove_file(&out_file).unwrap();
        let mut append = g("append_line");
        append["repeatable"] = json!(true);
        let cfg = config(&dir, "effects", json!({ "append": append, "next": g("next") }), inputs);
        let j2 = dir.join("run2.journal").to_string_lossy().into_owned();
        let (code, _, _) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j2], Some("before_result:1"));
        assert_eq!(code, None);
        let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j2], None);
        assert_eq!((code, out.as_str()), (Some(0), "1"), "{isolation}: {err}");
        assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "one\none\ntwo\n", "{isolation}: the repeatable call was made again, once");
    }
    // A contained call cut short is made again on resume without being declared
    // repeatable: nothing it did can have outlived its worker.
    if where_contained() {
        let dir = scratch("doubt-contained");
        let cfg = config(&dir, "twice", json!({ "next": grant("next") }), json!({}));
        let j = dir.join("run.journal").to_string_lossy().into_owned();
        let (code, _, _) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], Some("before_result:1"));
        assert_eq!(code, None);
        let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
        // A fresh worker: the counter counts from the start again.
        assert_eq!((code, out.as_str()), (Some(0), "2"), "{err}");
        assert!(!err.contains("may or may not"), "{err}");
    }
}

#[test]
fn a_c_function_that_crashes() {
    // In this process: the process ends, and the call is in doubt.
    let dir = scratch("crash-none");
    let cfg = config(&dir, "crash", json!({ "c": grant_as("crash", "none") }), json!({}));
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let (code, _, _) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], None);
    assert_eq!(code, None, "abort() ends the process with a signal");
    // Resume does not call it again.
    let (code, _, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("ffi.staaltest.crash"), "{err}");
    // In a worker: the worker ends, and the call fails, saying why.
    for isolation in isolations().into_iter().filter(|i| *i != "none") {
        let dir = scratch(&format!("crash-{isolation}"));
        let cfg = config(&dir, "crash", json!({ "c": grant_as("crash", isolation) }), json!({}));
        let j = dir.join("run.journal").to_string_lossy().into_owned();
        let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], None);
        assert_eq!(code, Some(1), "{isolation}: {err}");
        assert!(err.contains(&format!("the C code crashed (signal {})", libc::SIGABRT)), "{isolation}: {err}");
    }
}

#[test]
fn concurrent_calls_are_made_one_at_a_time() {
    let dir = scratch("parallel");
    for isolation in isolations() {
        let cfg = config(&dir, "counts", json!({ "next": grant_as("next", isolation) }), json!({}));
        for engine in engines() {
            let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine], None);
            // Eight calls of a counter, each seeing the last one finished: 1 + ... + 8.
            assert_eq!((code, out.as_str()), (Some(0), "36"), "{isolation} {engine}: {err}");
        }
    }
}

#[test]
fn uses_limits_the_calls() {
    let dir = scratch("uses");
    let iso = isolations()[0];
    let mut g = grant_as("next", iso);
    g["uses"] = json!(1);
    let cfg = config(&dir, "twice", json!({ "next": g }), json!({}));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert_eq!(code, Some(1));
    assert!(err.contains("capability"), "{err}");
    let cfg = config(&dir, "twice", json!({ "next": grant_as("next", iso) }), json!({}));
    assert_eq!(staalc(&["run", PROGRAM, "--config", &cfg], None).1, "2");
}

#[test]
fn the_documentation_says_how_c_functions_are_run() {
    let (code, out, err) = staalc(&["doc", PROGRAM], None);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("## C libraries") && out.contains("contained") && out.contains("foreign ffi.staaltest {"), "{out}");
}

/// Runs entry `entry` with one grant of `f` and `inputs`: exit code, output, errors.
fn run_one(dir: &Path, entry: &str, slot: &str, g: Json, inputs: Json) -> (Option<i32>, String, String) {
    let cfg = config(dir, entry, json!({ slot: g }), inputs);
    staalc(&["run", PROGRAM, "--config", &cfg], None)
}

const STOPPED: &str = "the C code made a system call that contained C code may not make, and was stopped";

#[test]
fn contained_c_cannot_reach_outside_its_worker() {
    if !where_contained() {
        return;
    }
    let dir = scratch("escape");
    let c = |f: &str| grant_as(f, "contained");
    // A socket, a program, a file written, a file read: each stops the worker.
    let marker = dir.join("ran.txt");
    let secret = dir.join("secret.txt");
    std::fs::write(&secret, "a secret").unwrap();
    for (entry, slot, f, inputs) in [
        ("socket", "s", "open_socket", json!({})),
        ("program", "r", "run_program", json!({ "cmd": format!("touch {}", marker.display()) })),
        ("effects", "append", "append_line", json!({ "path": dir.join("lines.txt").to_str().unwrap() })),
        ("size", "f", "file_size", json!({ "path": secret.to_str().unwrap() })),
    ] {
        let mut grants = json!({ slot: c(f) });
        if entry == "effects" {
            grants["next"] = c("next");
        }
        let cfg = config(&dir, entry, grants, inputs);
        let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
        assert_eq!(code, Some(1), "{f}: {err}");
        assert!(err.contains(STOPPED), "{f}: {err}");
    }
    assert!(!marker.exists(), "a contained function started a program");
    assert!(!dir.join("lines.txt").exists(), "a contained function wrote a file");
    // The same calls not contained do reach out: the test can tell.
    let (code, out, err) = run_one(&dir, "size", "f", grant_as("file_size", "none"), json!({ "path": secret.to_str().unwrap() }));
    assert_eq!((code, out.as_str()), (Some(0), "8"), "{err}");
}

#[test]
fn c_in_a_worker_sees_none_of_the_runs_environment() {
    let dir = scratch("env");
    let cfg = config(&dir, "env", json!({ "e": grant_as("read_env", "none") }), json!({ "name": "STAAL_TEST_SECRET" }));
    let mut cmd = Command::new(BIN);
    cmd.args(["run", PROGRAM, "--config", &cfg]).current_dir(ROOT).env("STAAL_TEST_SECRET", "s3cret");
    let o = cmd.output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "\"s3cret\"", "in this process, C sees the environment");
    for isolation in isolations().into_iter().filter(|i| *i != "none") {
        let cfg = config(&dir, "env", json!({ "e": grant_as("read_env", isolation) }), json!({ "name": "STAAL_TEST_SECRET" }));
        let o = Command::new(BIN).args(["run", PROGRAM, "--config", &cfg]).current_dir(ROOT).env("STAAL_TEST_SECRET", "s3cret").output().unwrap();
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{isolation}: {err}");
        assert!(err.contains("returned no text"), "{isolation}: the variable reached C: {err}");
    }
}

#[test]
fn a_worker_inherits_no_descriptor() {
    let dir = scratch("inherited");
    let target = dir.join("inherited.txt");
    let path = std::ffi::CString::new(target.to_str().unwrap()).unwrap();
    // A descriptor without close-on-exec, as a careless host program might hold: staalc
    // inherits it from this test.
    // SAFETY: a NUL-terminated path; the descriptor is closed at the end.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o600) };
    assert!(fd > 2);
    for isolation in isolations() {
        let (code, out, err) = run_one(&dir, "to_fd", "w", grant_as("write_fd", isolation), json!({ "fd": fd, "text": isolation }));
        assert_eq!(code, Some(0), "{isolation}: {err}");
        if isolation == "none" {
            assert_eq!(out, isolation.len().to_string(), "in this process, C can write to it");
        } else {
            assert_eq!(out, "-1", "{isolation}: the worker could write to an inherited descriptor");
        }
    }
    // SAFETY: the descriptor opened above.
    unsafe { libc::close(fd) };
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "none");
}

#[test]
fn a_worker_that_breaks_its_channel_is_ended() {
    let dir = scratch("channel");
    for isolation in isolations().into_iter().filter(|i| *i != "none") {
        // C code writes into its worker's own channel: the parent takes it for no reply,
        // ends the worker, and every later call to the library fails too.
        let cfg = config(&dir, "after_break", json!({ "w": grant_as("write_fd", isolation), "l": grant_as("text_len", isolation) }), json!({}));
        let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
        assert_eq!(code, Some(1), "{isolation}: {err}");
        assert!(err.contains("text_len: the C worker sent a malformed reply"), "{isolation}: {err}");
    }
}

#[test]
fn a_contained_librarys_initialiser_cannot_write() {
    if !where_contained() {
        return;
    }
    let dir = scratch("initialiser");
    let mark = dir.join("initialiser.txt");
    let lib = dir.join("libhostile.so");
    let o = Command::new("cc")
        .args(["-shared", "-fPIC", "-O1", &format!("-DMARK=\"{}\"", mark.display()), "-o"])
        .arg(&lib)
        .arg(Path::new(ROOT).join("tests/ffi/hostile_init.c"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let sha = staalc::packages::sha256_hex(&std::fs::read(&lib).unwrap());
    let src = dir.join("init.staal");
    std::fs::write(&src, "foreign ffi.hostile {\n    fn initialiser_wrote() -> i64\n}\n\nfn main(h: cap ffi.hostile.initialiser_wrote) -> Int {\n    return h.initialiser_wrote()\n}\n").unwrap();
    let run = |isolation: &str| {
        let g = json!({ "cap": "ffi.hostile.initialiser_wrote", "library": lib.to_str().unwrap(), "sha256": sha, "isolation": isolation });
        let cfg = config(&dir, "main", json!({ "h": g }), json!({}));
        staalc(&["run", src.to_str().unwrap(), "--config", &cfg], None)
    };
    let (code, _, err) = run("contained");
    assert_eq!(code, Some(2), "loading fails: {err}");
    assert!(err.contains(STOPPED), "{err}");
    assert!(!mark.exists(), "a contained library's initialiser wrote a file");
    // Loaded in this process, the initialiser has the process's authority.
    let (code, out, err) = run("none");
    assert_eq!((code, out.as_str()), (Some(0), "1"), "{err}");
    assert!(mark.exists());
}
