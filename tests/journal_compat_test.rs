// Journals written by released `staalc` binaries, resumed by this one (AGENT_MANIFEST.md
// section 16, R5): a release gate. `release.yml` runs every test before it publishes,
// so a `staalc` that cannot resume a journal it claims to read, or that does anything
// at all with a journal it does not, is never released.
//
// tests/fixtures/journals/staalc-<version>/<scenario>-<mode>/ holds what one released
// binary wrote in one uninterrupted run: the program and config it ran, its `journal`,
// its `world.jsonl` (what the simulated adapter performed) and its `stdout`. Every
// state a crash of that run can leave is derived from them (tests/common/
// journal_fixtures.rs; tests/crash_test.rs checks the derivation against real crashes).
// For each state, on each engine:
//
//  - a journal of the version this `staalc` reads is resumed until the run ends, an
//    action left in doubt resolved from what the world shows, as an operator would.
//    The run must end with the release's output and with exactly the release's world:
//    every action performed once, none repeated, none lost.
//  - a journal of any other version is refused before anything runs: the exit code is
//    2, the message names the version, and neither the journal nor the world changes.
//
// The fixtures are made with `generate_fixtures` below, from a binary downloaded from
// a GitHub release, after checking its SHA-256 (and its build provenance, where the
// release has one). A release that writes a new journal version has no fixtures of it
// until it is published; `UNRELEASED` names that version, and
// `every_journal_version_read_has_fixtures_from_a_release` fails as soon as the
// version is released and its fixtures are added without clearing `UNRELEASED`, or
// when a version this build reads has none.
//
// Run with: cargo test --test journal_compat_test

mod common;

use common::journal_fixtures::crash_states;
use serde_json::Value as Json;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// The journal version this build writes and no release has written yet, if any.
/// Cleared in the commit that adds the first release's fixtures of it.
const UNRELEASED: Option<u64> = Some(3);

/// The scenarios a fixture set holds: deterministic programs, so that a resumed run's
/// output and world can be compared with the release's exactly. Each is a program and a
/// run config, relative to the crate root.
const SCENARIOS: [(&str, &str, &str); 3] = [
    ("workflow", "tests/crash/workflow.staal", "tests/crash/workflow.json"),
    ("access", "tests/run/domain_access.staal", "tests/run/domain_access__mixed_batch.json"),
    ("native", "tests/crash/native.staal", "tests/crash/native.json"),
];

fn fixtures_root() -> PathBuf {
    Path::new(ROOT).join("tests/fixtures/journals")
}

/// Every fixture: (its directory, the journal version and `staalc` its start record
/// names). The `staalc` field is absent before journal version 3.
fn fixtures() -> Vec<(PathBuf, u64, Option<String>)> {
    let mut out = Vec::new();
    let Ok(sets) = std::fs::read_dir(fixtures_root()) else { return out };
    let mut sets: Vec<PathBuf> = sets.map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    sets.sort();
    for set in sets {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&set).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        for d in dirs {
            let journal = std::fs::read_to_string(d.join("journal")).unwrap();
            let start: Json = serde_json::from_str(journal.lines().next().unwrap()).unwrap();
            let version = start["version"].as_u64().unwrap_or_else(|| panic!("{}: no journal version", d.display()));
            out.push((d, version, start["staalc"].as_str().map(str::to_string)));
        }
    }
    out
}

struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn staalc(dir: &Path, args: &[&str]) -> Out {
    let o = Command::new(BIN).current_dir(dir).args(args).env_remove("STAALC_FAILPOINT").output().unwrap();
    Out { code: o.status.code(), stdout: String::from_utf8_lossy(&o.stdout).trim().into(), stderr: String::from_utf8_lossy(&o.stderr).trim().into() }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// A fresh directory holding the fixture's program and config and the crash state.
fn workdir(name: &str, fixture: &Path, journal: &str, world: &str) -> PathBuf {
    let d = std::fs::canonicalize(env!("CARGO_TARGET_TMPDIR")).unwrap().join("journal-compat").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for f in ["program.staal", "config.json"] {
        std::fs::copy(fixture.join(f), d.join(f)).unwrap();
    }
    std::fs::write(d.join("journal"), journal).unwrap();
    std::fs::write(d.join("world.jsonl"), world).unwrap();
    d
}

fn resume_args(engine: &str) -> Vec<&str> {
    vec!["resume", "program.staal", "--config", "config.json", "--journal", "journal", "--engine", engine]
}

/// Resumes until the run ends, resolving an action in doubt from the world: performed
/// if the world holds its key, otherwise to be retried. Returns the last resume.
fn finish(d: &Path, engine: &str, label: &str) -> Out {
    for _ in 0..10 {
        let out = staalc(d, &resume_args(engine));
        match out.code {
            Some(0) => return out,
            Some(3) => {
                assert!(out.stderr.contains("in_doubt"), "{label}: exit 3 without in_doubt: {}", out.stderr);
                let journal = read(&d.join("journal"));
                let intent: Json = journal
                    .lines()
                    .rev()
                    .map(|l| serde_json::from_str::<Json>(l).unwrap())
                    .find(|r| r["j"] == "intent")
                    .unwrap_or_else(|| panic!("{label}: nothing in doubt"));
                let key = intent["key"].as_str().unwrap();
                let entry = read(&d.join("world.jsonl")).lines().map(|l| serde_json::from_str::<Json>(l).unwrap()).find(|e| e["key"] == key);
                let how: Vec<&str> = match &entry {
                    Some(e) => vec!["--performed", e["receipt"].as_str().unwrap()],
                    None => vec!["--retry"],
                };
                let mut args = vec!["resolve", "--journal", "journal"];
                args.extend(how);
                let r = staalc(d, &args);
                assert_eq!(r.code, Some(0), "{label}: resolve: {}", r.stderr);
            }
            other => panic!("{label}: resume exited {other:?}: {}", out.stderr),
        }
    }
    panic!("{label}: the run did not end after 10 resumes");
}

fn engines() -> Vec<&'static str> {
    if cfg!(feature = "native") {
        vec!["interpreter", "native"]
    } else {
        vec!["interpreter"]
    }
}

#[test]
fn every_release_journal_resumes_exactly_or_is_refused_untouched() {
    let current = staalc::interp::journal::VERSION;
    let mut resumed = 0;
    let mut refused = 0;
    for (fixture, version, _) in fixtures() {
        let name = format!(
            "{}-{}",
            fixture.parent().unwrap().file_name().unwrap().to_string_lossy(),
            fixture.file_name().unwrap().to_string_lossy()
        );
        let (journal, world, stdout) = (read(&fixture.join("journal")), read(&fixture.join("world.jsonl")), read(&fixture.join("stdout")));
        let states = crash_states(&journal, &world).unwrap_or_else(|e| panic!("{name}: {e}"));
        for engine in engines() {
            for s in &states {
                let label = format!("{name} {} on {engine}", s.name);
                let d = workdir(&format!("{name}-{}-{engine}", s.name.replace(':', "-")), &fixture, &s.journal, &s.world);
                if version == current {
                    let out = finish(&d, engine, &label);
                    assert_eq!(out.stdout, stdout.trim(), "{label}: the output differs from the release's");
                    assert_eq!(read(&d.join("world.jsonl")), world, "{label}: the world differs from the release's");
                    resumed += 1;
                } else {
                    let out = staalc(&d, &resume_args(engine));
                    assert_eq!(out.code, Some(2), "{label}: {}", out.stderr);
                    assert!(out.stderr.contains(&format!("has version {version}")), "{label}: {}", out.stderr);
                    assert_eq!(read(&d.join("journal")), s.journal, "{label}: the journal changed");
                    assert_eq!(read(&d.join("world.jsonl")), s.world, "{label}: the world changed");
                    refused += 1;
                }
            }
        }
    }
    println!("{resumed} crash states resumed, {refused} refused untouched");
}

#[test]
fn every_journal_version_read_has_fixtures_from_a_release() {
    let current = staalc::interp::journal::VERSION;
    let have = fixtures().iter().any(|(_, v, _)| *v == current);
    match UNRELEASED {
        Some(v) if v == current => {
            assert!(!have, "journal version {current} has fixtures from a release now: set UNRELEASED to None");
        }
        Some(v) => panic!("UNRELEASED is {v}, but this build writes journal version {current}"),
        None => assert!(have, "this build reads journal version {current}, and no release's journal of it is in tests/fixtures/journals"),
    }
    // Each set is named for the release that wrote it, and a journal that names its
    // `staalc` names that release.
    for (d, _, staalc) in fixtures() {
        let set = d.parent().unwrap().file_name().unwrap().to_string_lossy().to_string();
        let release = set.strip_prefix("staalc-").unwrap_or_else(|| panic!("{set}: a set is named staalc-<version>"));
        if let Some(s) = staalc {
            assert_eq!(s, release, "{}: written by staalc {s}, kept as {set}", d.display());
        }
    }
}

/// Writes the fixtures of the binary at `STAALC_FIXTURE_BIN`, which must be a binary
/// from a GitHub release whose SHA-256 (and build provenance, if it has one) was
/// checked, into tests/fixtures/journals/staalc-<its version>/:
///
///     STAALC_FIXTURE_BIN=/path/to/staalc cargo test --test journal_compat_test -- --ignored generate_fixtures
#[test]
#[ignore]
fn generate_fixtures() {
    let bin = std::env::var("STAALC_FIXTURE_BIN").expect("STAALC_FIXTURE_BIN: the released staalc to record");
    let v = Command::new(&bin).arg("--version").output().unwrap();
    let shown = String::from_utf8(v.stdout).unwrap();
    let version = shown.trim().strip_prefix("staalc ").unwrap_or_else(|| panic!("not a staalc: {shown}")).to_string();
    let set = fixtures_root().join(format!("staalc-{version}"));
    assert!(!set.exists(), "{} exists; remove it to record again", set.display());
    for (scenario, program, config) in SCENARIOS {
        for idempotent in [true, false] {
            let d = set.join(format!("{scenario}-{}", if idempotent { "idempotent" } else { "plain" }));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::copy(Path::new(ROOT).join(program), d.join("program.staal")).unwrap();
            let mut cfg: Json = serde_json::from_str(&read(&Path::new(ROOT).join(config))).unwrap();
            cfg.as_object_mut().unwrap().remove("expect");
            // The world is named relative to the run's directory, so the config (and its
            // hash in the journal) is the same wherever the fixture is resumed.
            cfg["adapters"]["simulated"]["world"] = Json::from("world.jsonl");
            cfg["adapters"]["simulated"]["idempotent"] = Json::from(idempotent);
            std::fs::write(d.join("config.json"), serde_json::to_string_pretty(&cfg).unwrap() + "\n").unwrap();
            let o = Command::new(&bin)
                .current_dir(&d)
                .args(["run", "program.staal", "--config", "config.json", "--journal", "journal"])
                .env_remove("STAALC_FAILPOINT")
                .output()
                .unwrap();
            assert!(o.status.success(), "{scenario}: {}", String::from_utf8_lossy(&o.stderr));
            std::fs::write(d.join("stdout"), &o.stdout).unwrap();
            if !d.join("world.jsonl").exists() {
                std::fs::write(d.join("world.jsonl"), "").unwrap();
            }
            crash_states(&read(&d.join("journal")), &read(&d.join("world.jsonl"))).unwrap_or_else(|e| panic!("{scenario}: {e}"));
        }
    }
}
