// Module tests. Each folder in tests/modules/ is a program: main.staal plus the modules
// it imports. main.staal starts with `// expect: ok` or `// expect: CODE [CODE ...]`,
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
        let main = case.join("main.staal");
        let text = fs::read_to_string(&main).unwrap();
        let header = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim);
        let want = header("// expect:").unwrap_or_else(|| panic!("{} lacks `// expect:`", main.display()));
        let checked = staalc::check_path(&main).unwrap();
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
                Err(d) => d.iter().map(|d| staalc::format_diag_in(&checked.files, d)).collect(),
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
    use staalc::interp::{run_config_durable, Durability};
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("module-resume");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("lib")).unwrap();
    let main = dir.join("main.staal");
    fs::write(&main, "import lib.notify\nfn go(sms: cap sms.send) -> Receipt {\n    return notify.send(\"+2348000000000\", sms)\n}\n").unwrap();
    let lib = dir.join("lib/notify.staal");
    let lib_text = "pub fn send(to: Text, sms: cap sms.send) -> Receipt {\n    return sms.send(to)\n}\n";
    fs::write(&lib, lib_text).unwrap();
    let cfg = serde_json::json!({
        "entry": "go", "grants": { "sms": { "cap": "sms.send" } },
        "model": { "provider": "script", "responses": [] }
    });
    let journal = dir.join("run.journal");
    let checked = staalc::check_path(&main).unwrap();
    assert_eq!(checked.files.len(), 2);
    let prog = checked.result.unwrap();
    let out = run_config_durable(&prog, &checked.fingerprint, &cfg, Durability::Create(&journal)).unwrap();
    assert!(out.result.is_ok(), "{:?}", out.result.err());
    // Unchanged files: resuming the finished run replays it.
    let again = staalc::check_path(&main).unwrap();
    let prog = again.result.unwrap();
    assert!(run_config_durable(&prog, &again.fingerprint, &cfg, Durability::Resume(&journal)).is_ok());
    // One character changed in the module, not in the program file.
    fs::write(&lib, lib_text.replace("sms.send(to)", "sms.send(to) ")).unwrap();
    let changed = staalc::check_path(&main).unwrap();
    let prog = changed.result.unwrap();
    let Err(err) = run_config_durable(&prog, &changed.fingerprint, &cfg, Durability::Resume(&journal)) else {
        panic!("resume must refuse a changed module");
    };
    assert!(err.contains("different program"), "{err}");
}

/// The fingerprint is the program, not where its files are: a moved folder, or a path
/// written differently, is the same program. Which module a text is matters.
#[test]
fn the_fingerprint_follows_modules_not_file_locations() {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("module-fingerprint");
    let _ = fs::remove_dir_all(&base);
    let main = "import lib.a\nimport lib.b\nfn go() -> Int {\n    return a.one() + b.two()\n}\n";
    let a = "pub fn one() -> Int {\n    return 1\n}\n";
    let b = "pub fn two() -> Int {\n    return 2\n}\n";
    let write = |dir: &Path, a: &str, b: &str| {
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("main.staal"), main).unwrap();
        fs::write(dir.join("lib/a.staal"), a).unwrap();
        fs::write(dir.join("lib/b.staal"), b).unwrap();
    };
    write(&base.join("one"), a, b);
    write(&base.join("two/deeper"), a, b);
    let fp = |p: std::path::PathBuf| staalc::check_path(&p).unwrap().fingerprint;
    let first = fp(base.join("one/main.staal"));
    assert_eq!(first, fp(base.join("two/deeper/main.staal")), "moved");
    assert_eq!(first, fp(base.join("one/../one/main.staal")), "the same path written differently");
    // Swapping the two modules' texts changes the program.
    write(&base.join("swapped"), &a.replace("one", "two").replace("1", "2"), &b.replace("two", "one").replace("2", "1"));
    assert_ne!(first, fp(base.join("swapped/main.staal")), "swapped modules");
}
