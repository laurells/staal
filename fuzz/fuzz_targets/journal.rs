// A journal whose start record is genuine and whose every other byte is the fuzzer's:
// resuming the crash-test workflow on it, or recording an operator's answer in it,
// must fail with a message or succeed, never panic, and never treat a malformed
// record as a result.
#![no_main]
use libfuzzer_sys::fuzz_target;
use serde_json::Value as Json;
use staalc::interp::{journal, run_config_durable, Durability};
use std::path::PathBuf;
use std::sync::OnceLock;

struct Setup {
    prog: staalc::ast::Program,
    fingerprint: String,
    config: Json,
    start: String,
    path: PathBuf,
}

fn setup() -> &'static Setup {
    static S: OnceLock<Setup> = OnceLock::new();
    S.get_or_init(|| {
        let text = include_str!("../../tests/crash/workflow.staal");
        let checked = staalc::check_files(staalc::SourceFile { name: "workflow.staal".into(), text: text.into() }, |m| Err(format!("no module {m:?}")));
        let fingerprint = checked.fingerprint.clone();
        let prog = checked.result.expect("the workflow checks");
        let config: Json = serde_json::from_str(include_str!("../../tests/crash/workflow.json")).unwrap();
        let dir = std::env::temp_dir().join(format!("staalc-fuzz-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.journal");
        // A genuine run, for its start record.
        run_config_durable(&prog, &fingerprint, &config, Durability::Create(&path)).unwrap();
        let start = std::fs::read_to_string(&path).unwrap().lines().next().unwrap().to_string();
        Setup { prog, fingerprint, config, start, path }
    })
}

fuzz_target!(|data: &[u8]| {
    let s = setup();
    let mut bytes = s.start.clone().into_bytes();
    bytes.push(b'\n');
    bytes.extend_from_slice(data);
    std::fs::write(&s.path, &bytes).unwrap();
    let _ = run_config_durable(&s.prog, &s.fingerprint, &s.config, Durability::Resume(&s.path));
    std::fs::write(&s.path, &bytes).unwrap();
    let _ = journal::Journal::resolve(&s.path, journal::Resolution::Retry, None);
});
