// The C interface (phase 5): called directly here, and from Python through the
// shared library, as other languages will call it.

use intentc::capi::{intent_check, intent_free, intent_run, intent_version};
use serde_json::Value as Json;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

/// Takes a result string from the library, parses it and frees it.
fn take(p: *mut std::ffi::c_char) -> Json {
    assert!(!p.is_null());
    // SAFETY: the library returned a NUL-terminated string, freed once below.
    let text = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
    unsafe { intent_free(p) };
    serde_json::from_str(&text).unwrap()
}

#[test]
fn arguments_that_are_not_strings_are_errors_not_crashes() {
    let bad = [0xffu8, 0xfe, 0x00];
    // SAFETY: NUL-terminated buffers that live for the calls.
    let r = take(unsafe { intent_check(bad.as_ptr() as *const _) });
    assert_eq!(r["status"], "error");
    assert!(r["error"].as_str().unwrap().contains("not UTF-8"), "{r}");
    let r = take(unsafe { intent_check(std::ptr::null()) });
    assert_eq!(r["status"], "error");
    let prog = CString::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/basics.int").to_str().unwrap()).unwrap();
    let cfg = CString::new(r#"{"entry": "add", "inputs": {"a": 1, "b": 2}, "model": {"provider": "script", "responses": []}}"#).unwrap();
    let r = take(unsafe { intent_run(prog.as_ptr(), cfg.as_ptr(), std::ptr::null(), std::ptr::null()) });
    assert!(r["status"] == "done" || r["status"] == "error", "{r}");
    // Freeing NULL is allowed.
    unsafe { intent_free(std::ptr::null_mut()) };
    // SAFETY: a static NUL-terminated string.
    let v = unsafe { CStr::from_ptr(intent_version()) }.to_str().unwrap();
    assert_eq!(v, env!("CARGO_PKG_VERSION"));
}

/// The shared library cargo built for this test run: next to the test binary.
fn shared_library() -> PathBuf {
    let dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    for name in ["libintentc.so", "libintentc.dylib"] {
        if dir.join(name).exists() {
            return dir.join(name);
        }
    }
    panic!("no libintentc shared library in {}", dir.display());
}

#[test]
fn python_runs_the_clinic_program_through_the_c_interface() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("python");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root.join("python/tests/test_intent.py"))
        .arg(root)
        .arg(&scratch)
        .env("INTENT_LIBRARY", shared_library())
        .env("INTENT_HAS_NATIVE", if cfg!(feature = "native") { "1" } else { "0" })
        .output()
        .expect("python3 must be installed to run this test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "python failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains("checks passed"), "{stdout}");
    println!("{}", stdout.trim());
}
