// Module tests. Each folder in tests/modules/ is a program: main.int plus the modules
// it imports. main.int starts with `// expect: ok` or `// expect: CODE [CODE ...]`,
// and optionally `// expect-in: FILE`, meaning every diagnostic is reported in a file
// whose path ends with FILE.

use std::fs;
use std::path::Path;

#[test]
fn module_programs() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/modules");
    let mut cases: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    cases.sort();
    assert!(cases.len() >= 29, "expected the module cases, found {}", cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let main = case.join("main.int");
        let text = fs::read_to_string(&main).unwrap();
        let header = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim);
        let want = header("// expect:").unwrap_or_else(|| panic!("{} lacks `// expect:`", main.display()));
        let checked = intentc::check_path(&main).unwrap();
        let (codes, places): (Vec<&str>, Vec<String>) = match &checked.result {
            Ok(_) => (vec!["ok"], Vec::new()),
            Err(d) => (
                d.iter().map(|d| d.code).collect(),
                d.iter().map(|d| checked.files[d.pos.file as usize].name.clone()).collect(),
            ),
        };
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        if codes.join(" ") != want {
            let msgs: Vec<String> = match &checked.result {
                Ok(_) => Vec::new(),
                Err(d) => d.iter().map(|d| intentc::format_diag_in(&checked.files, d)).collect(),
            };
            failures.push(format!("{name}: expected {want}, got {}\n    {}", codes.join(" "), msgs.join("\n    ")));
            continue;
        }
        if let Some(file) = header("// expect-in:")
            && let Some(bad) = places.iter().find(|p| !p.ends_with(file))
        {
            failures.push(format!("{name}: a diagnostic was reported in {bad}, expected {file}"));
        }
    }
    assert!(failures.is_empty(), "{} of {} module cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

/// A journal records the hash of every file of the program: a run cannot be resumed
/// after any imported module changed, even by one character.
#[test]
fn resume_refuses_a_changed_module() {
    use intentc::interp::{run_config_durable, Durability};
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("module-resume");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("lib")).unwrap();
    let main = dir.join("main.int");
    fs::write(&main, "import lib.notify\nfn go(sms: cap sms.send) -> Receipt {\n    return notify.send(\"+2348000000000\", sms)\n}\n").unwrap();
    let lib = dir.join("lib/notify.int");
    let lib_text = "pub fn send(to: Text, sms: cap sms.send) -> Receipt {\n    return sms.send(to)\n}\n";
    fs::write(&lib, lib_text).unwrap();
    let cfg = serde_json::json!({
        "entry": "go", "grants": { "sms": { "cap": "sms.send" } },
        "model": { "provider": "script", "responses": [] }
    });
    let journal = dir.join("run.journal");
    let checked = intentc::check_path(&main).unwrap();
    assert_eq!(checked.files.len(), 2);
    let prog = checked.result.unwrap();
    let out = run_config_durable(&prog, &checked.fingerprint, &cfg, Durability::Create(&journal)).unwrap();
    assert!(out.result.is_ok(), "{:?}", out.result.err());
    // Unchanged files: resuming the finished run replays it.
    let again = intentc::check_path(&main).unwrap();
    let prog = again.result.unwrap();
    assert!(run_config_durable(&prog, &again.fingerprint, &cfg, Durability::Resume(&journal)).is_ok());
    // One character changed in the module, not in the program file.
    fs::write(&lib, lib_text.replace("sms.send(to)", "sms.send(to) ")).unwrap();
    let changed = intentc::check_path(&main).unwrap();
    let prog = changed.result.unwrap();
    let Err(err) = run_config_durable(&prog, &changed.fingerprint, &cfg, Durability::Resume(&journal)) else {
        panic!("resume must refuse a changed module");
    };
    assert!(err.contains("different program"), "{err}");
}
