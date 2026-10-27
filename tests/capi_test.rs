// The C interface (phase 5): called directly here, and from Python through the
// shared library, as other languages will call it.

use staalc::capi::{staal_check, staal_free, staal_run, staal_version};
use serde_json::Value as Json;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

/// Takes a result string from the library, parses it and frees it.
fn take(p: *mut std::ffi::c_char) -> Json {
    assert!(!p.is_null());
    // SAFETY: the library returned a NUL-terminated string, freed once below.
    let text = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
    unsafe { staal_free(p) };
    serde_json::from_str(&text).unwrap()
}

#[test]
fn arguments_that_are_not_strings_are_errors_not_crashes() {
    let bad = [0xffu8, 0xfe, 0x00];
    // SAFETY: NUL-terminated buffers that live for the calls.
    let r = take(unsafe { staal_check(bad.as_ptr().cast()) });
    assert_eq!(r["status"], "error");
    assert!(r["error"].as_str().unwrap().contains("not UTF-8"), "{r}");
    let r = take(unsafe { staal_check(std::ptr::null()) });
    assert_eq!(r["status"], "error");
    let prog = CString::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/basics.staal").to_str().unwrap()).unwrap();
    let cfg = CString::new(r#"{"entry": "add", "inputs": {"a": 1, "b": 2}, "model": {"provider": "script", "responses": []}}"#).unwrap();
    let r = take(unsafe { staal_run(prog.as_ptr(), cfg.as_ptr(), std::ptr::null(), std::ptr::null()) });
    assert!(r["status"] == "done" || r["status"] == "error", "{r}");
    // Freeing NULL is allowed.
    unsafe { staal_free(std::ptr::null_mut()) };
    // SAFETY: a static NUL-terminated string.
    let v = unsafe { CStr::from_ptr(staal_version()) }.to_str().unwrap();
    assert_eq!(v, env!("CARGO_PKG_VERSION"));
}

/// The shared library cargo built for this test run: next to the test binary.
fn shared_library() -> PathBuf {
    let dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    for name in ["libstaalc.so", "libstaalc.dylib"] {
        if dir.join(name).exists() {
            return dir.join(name);
        }
    }
    panic!("no libstaalc shared library in {}", dir.display());
}

#[test]
fn python_runs_the_clinic_program_through_the_c_interface() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("python");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root.join("python/tests/test_staal.py"))
        .arg(root)
        .arg(&scratch)
        .env("STAAL_LIBRARY", shared_library())
        .env("STAAL_HAS_NATIVE", if cfg!(feature = "native") { "1" } else { "0" })
        .output()
        .expect("python3 must be installed to run this test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "python failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains("checks passed"), "{stdout}");
    println!("{}", stdout.trim());
}

#[test]
fn python_app_decides_the_limit_and_staal_enforces_it() {
    // examples/policy: the limit each run is granted comes from the app (tier, order
    // cost), and neither the model nor a bug in the app gets a refund past it.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("python-policy");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root.join("python/tests/test_policy.py"))
        .arg(root)
        .arg(&scratch)
        .env("STAAL_LIBRARY", shared_library())
        .env("STAAL_HAS_NATIVE", if cfg!(feature = "native") { "1" } else { "0" })
        .output()
        .expect("python3 must be installed to run this test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "python failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains("checks passed"), "{stdout}");
    println!("{}", stdout.trim());
}

#[test]
fn the_python_quickstart_prints_what_the_readme_says() {
    // examples/quickstart/app.py, as README.md shows it: one refund made, one refused.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = std::process::Command::new("python3")
        .arg(root.join("examples/quickstart/app.py"))
        .env("STAAL_LIBRARY", shared_library())
        .env("PYTHONPATH", root.join("python"))
        .output()
        .expect("python3 must be installed to run this test");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Refunded 35.00 USD for order 4417\n'payments.refund' allows at most 50.00 USD per action; this action needs 300.00 USD\n"
    );
}
