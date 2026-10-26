// The `db.sqlite` capabilities, end to end through `staalc run` and `resume`: a
// program changes and reads a real SQLite file, only where its grant allows, and a
// journaled run replays its queries and never repeats a statement.

#![cfg(target_os = "linux")]

use serde_json::{json, Value as Json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("db").join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    fs::canonicalize(&d).unwrap()
}

/// A run config for tests/db/shop.staal's `record`, on `db` (its folder
/// granted), with `only` for the execute grant, and `extra` merged into that grant.
fn config(dir: &Path, db: &str, item: &str, extra: Json) -> PathBuf {
    let mut exec = json!({ "cap": "db.sqlite.execute", "only": [format!("{}/*.db", dir.display())] });
    for (k, v) in extra.as_object().unwrap() {
        exec[k] = v.clone();
    }
    let cfg = json!({
        "entry": "record",
        "inputs": { "path": db, "item": item, "qty": 2 },
        "grants": { "db": exec, "read": { "cap": "db.sqlite.query", "only": [format!("{}/*.db", dir.display())] } },
        "model": { "provider": "script", "responses": [] },
    });
    let p = dir.join(format!("{item}.json"));
    fs::write(&p, cfg.to_string()).unwrap();
    p
}

fn staalc(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(BIN).args(args).current_dir(ROOT).output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
}

fn rows(db: &str) -> i64 {
    let c = rusqlite::Connection::open(db).unwrap();
    c.query_row("SELECT count(*) FROM orders", [], |r| r.get(0)).unwrap()
}

const PROGRAM: &str = "tests/db/shop.staal";

#[test]
fn a_program_changes_and_reads_a_database_on_both_engines() {
    let dir = scratch("basic");
    let db = format!("{}/shop.db", dir.display());
    let engines: &[&str] = if cfg!(feature = "native") { &["interpreter", "native", "interpreter"] } else { &["interpreter", "interpreter"] };
    for (i, engine) in engines.iter().enumerate() {
        let cfg = config(&dir, &db, &format!("item{i}"), json!({}));
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", cfg.to_str().unwrap(), "--engine", engine]);
        assert_eq!(code, Some(0), "{engine}: {err}");
        // Each run adds one order and counts them all.
        assert_eq!(out, (i + 1).to_string(), "{engine}");
    }
    // The values went in as parameters, exactly.
    let c = rusqlite::Connection::open(&db).unwrap();
    let item: String = c.query_row("SELECT item FROM orders WHERE id = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(item, "item0");
}

#[test]
fn a_database_outside_the_grant_is_refused_before_it_is_opened() {
    let dir = scratch("outside");
    let elsewhere = scratch("outside-other");
    let db = format!("{}/shop.db", elsewhere.display());
    let cfg = config(&dir, &db, "x", json!({}));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", cfg.to_str().unwrap()]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("capability"), "{err}");
    assert!(!Path::new(&db).exists(), "the database outside the grant was created");
    // A path through a link is refused too, even into the granted folder.
    let link = elsewhere.join("into");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let cfg = config(&elsewhere, &format!("{}/shop.db", link.display()), "y", json!({}));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", cfg.to_str().unwrap()]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("symbolic link"), "{err}");
}

#[test]
fn a_journaled_run_replays_its_queries_and_never_repeats_a_statement() {
    let dir = scratch("journal");
    let db = format!("{}/shop.db", dir.display());
    let cfg = config(&dir, &db, "once", json!({}));
    let journal = dir.join("run.journal");
    let j = journal.to_str().unwrap();
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", cfg.to_str().unwrap(), "--journal", j]);
    assert_eq!((code, out.as_str()), (Some(0), "1"), "{err}");
    // Resuming the finished run gives its result again from the journal: the database
    // is not touched, so a row added meanwhile changes nothing.
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute("INSERT INTO orders (item, qty) VALUES ('meanwhile', 1)", []).unwrap();
    let (code, out, err) = staalc(&["resume", PROGRAM, "--config", cfg.to_str().unwrap(), "--journal", j]);
    assert_eq!((code, out.as_str()), (Some(0), "1"), "{err}");
    assert_eq!(rows(&db), 2);
    // The journal holds hashes of the SQL and its parameters, never the values.
    let text = fs::read_to_string(&journal).unwrap();
    assert!(text.contains("sql_sha256") && !text.contains("INSERT INTO") && !text.contains("\"once\""), "{text}");
}

#[test]
fn idempotency_keys_are_only_for_execute() {
    let dir = scratch("keys");
    let db = format!("{}/shop.db", dir.display());
    let cfg = config(&dir, &db, "k", json!({ "idempotency_key": true }));
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", cfg.to_str().unwrap()]);
    assert_eq!((code, out.as_str()), (Some(0), "1"), "{err}");
    let c = rusqlite::Connection::open(&db).unwrap();
    let keys: i64 = c.query_row("SELECT count(*) FROM _staal_keys", [], |r| r.get(0)).unwrap();
    assert_eq!(keys, 2, "one key per statement");
    let mut bad: Json = serde_json::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
    bad["grants"]["read"]["idempotency_key"] = json!(true);
    fs::write(&cfg, bad.to_string()).unwrap();
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", cfg.to_str().unwrap()]);
    assert_eq!(code, Some(2));
    assert!(err.contains("idempotency_key: only for"), "{err}");
}
