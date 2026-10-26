// The `db.postgres` capabilities against a real PostgreSQL server (tests/common/
// postgres.rs): a program changes and reads a database on both engines; queries cannot
// change anything; values come back exactly; only the databases a grant names are
// reached, with TLS verified unless the server is on this machine; the password never
// reaches a log or journal; and a journaled run never applies a statement twice.

#![cfg(unix)]

mod common;

use common::postgres::{skip, Server, PASSWORD, USER};
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const PROGRAM: &str = "tests/postgres/shop.staal";
const PW_VAR: &str = "STAAL_TEST_PG_PASSWORD";

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("postgres").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A server with the `shop` database: an orders table and an enum type.
fn shop_server(dir: &Path, tls: Option<(&str, &str)>) -> Server {
    let s = Server::start(&dir.join("server"), tls);
    s.psql("postgres", "CREATE DATABASE shop").unwrap();
    s.psql("shop", "CREATE TABLE orders (id serial PRIMARY KEY, item text NOT NULL, qty int NOT NULL); CREATE TYPE size AS ENUM ('small', 'medium')").unwrap();
    s
}

fn database(s: &Server, extra: Json) -> Json {
    let mut d = json!({ "host": "127.0.0.1", "port": s.port, "dbname": "shop", "user": USER, "password": { "env": PW_VAR }, "tls": "disable" });
    for (k, v) in extra.as_object().unwrap() {
        d[k] = v.clone();
    }
    d
}

fn grant(cap: &str, db: &Json) -> Json {
    json!({ "cap": cap, "databases": { "shop": db } })
}

fn write_config(dir: &Path, name: &str, cfg: Json) -> String {
    let p = dir.join(format!("{name}.json"));
    std::fs::write(&p, cfg.to_string()).unwrap();
    p.to_string_lossy().into_owned()
}

fn record_config(dir: &Path, s: &Server, item: &str, exec_extra: Json) -> String {
    let db = database(s, json!({}));
    let mut exec = grant("db.postgres.execute", &db);
    for (k, v) in exec_extra.as_object().unwrap() {
        exec[k] = v.clone();
    }
    let cfg = json!({
        "entry": "record",
        "inputs": { "item": item, "qty": 2 },
        "grants": { "db": exec, "read": grant("db.postgres.query", &db) },
    });
    write_config(dir, item, cfg)
}

fn staalc(args: &[&str], failpoint: Option<&str>) -> (Option<i32>, String, String) {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(ROOT).env(PW_VAR, PASSWORD);
    match failpoint {
        Some(f) => c.env("STAALC_FAILPOINT", f),
        None => c.env_remove("STAALC_FAILPOINT"),
    };
    let o = c.output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
}

fn engines() -> Vec<&'static str> {
    if cfg!(feature = "native") { vec!["interpreter", "native"] } else { vec!["interpreter"] }
}

fn count(s: &Server, item: &str) -> usize {
    s.psql("shop", &format!("SELECT count(*) FROM orders WHERE item = '{item}'")).unwrap().parse().unwrap()
}

#[test]
fn a_program_changes_reads_and_is_refused_on_a_real_server() {
    if skip() {
        return;
    }
    let dir = scratch("basic");
    let s = shop_server(&dir, None);

    // Changed and read, on both engines; each statement once.
    for (i, engine) in engines().into_iter().enumerate() {
        let cfg = record_config(&dir, &s, &format!("item{i}"), json!({}));
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine], None);
        assert_eq!((code, out.as_str()), (Some(0), (i + 1).to_string().as_str()), "{engine}: {err}");
        assert_eq!(count(&s, &format!("item{i}")), 1);
    }

    // Every type a query reads, exactly.
    let read = grant("db.postgres.query", &database(&s, json!({})));
    let cfg = write_config(&dir, "kinds", json!({ "entry": "kinds", "grants": { "read": read } }));
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert_eq!(code, Some(0), "{err}");
    let rows: Json = serde_json::from_str(&serde_json::from_str::<String>(&out).unwrap()).unwrap();
    assert_eq!(
        rows,
        json!([{ "i2": 7, "i4": -8, "i8": 9007199254740993i64, "yes": true, "t": "naira ₦", "v": "x", "price": "12.50",
                 "small": "-0.000123", "f8": "0.1", "f4": "0.1", "d": "2026-10-26", "ts": "2026-10-26T13:05:09.25",
                 "tz": "2026-10-26T12:05:09Z", "id": "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11", "j": "{\"a\": 1.10}",
                 "jb": "{\"b\": [1, 2]}", "nothing": null, "sz": "medium" }])
    );

    // A parameter is read as the type the server infers: here an int.
    let cfg = write_config(&dir, "typed", json!({ "entry": "typed", "inputs": { "qty": "2" }, "grants": { "read": read } }));
    let (_, out, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert!(out.contains(r#"\"n\":"#), "{out} {err}");

    // What a query refuses.
    for (which, why) in [
        ("insert", "cannot execute INSERT in a read-only transaction"),
        ("two", "cannot insert multiple commands into a prepared statement"),
        ("params", "the SQL has 1 parameter(s), but 2 were given"),
        ("interval", "column 'gap': its type interval is not read here; cast it to text in the SQL (column::text)"),
        ("bytes", "column 'raw': it is bytes, not text; select it as encode(column, 'hex')"),
        ("names", "two columns are both named 'a'"),
        ("nowhere", "the grant reaches no database named 'elsewhere' (it reaches shop)"),
        ("missing", "ERROR: column \\\"nope\\\" does not exist"),
    ] {
        let cfg = write_config(&dir, which, json!({ "entry": "refused", "inputs": { "which": which }, "grants": { "read": read } }));
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
        assert_eq!(code, Some(0), "{which}: {err}");
        assert!(out.contains(why), "{which}: {out}");
    }
    assert_eq!(count(&s, "sneaky"), 0, "a query changed the database");

    // TLS unless the grant says otherwise: this server has none, so it is refused.
    let mut no_tls = database(&s, json!({}));
    no_tls.as_object_mut().unwrap().remove("tls");
    let cfg = write_config(&dir, "notls", json!({ "entry": "kinds", "grants": { "read": grant("db.postgres.query", &no_tls) } }));
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
    assert_eq!(code, Some(1));
    assert!(err.contains("cannot connect to database shop"), "{err}");

    // A wrong password: refused by the server, and never shown.
    let bad = grant("db.postgres.query", &database(&s, json!({ "password": { "env": "STAAL_TEST_PG_WRONG" } })));
    let cfg = write_config(&dir, "wrong", json!({ "entry": "kinds", "grants": { "read": bad } }));
    let o = Command::new(BIN).args(["run", PROGRAM, "--config", &cfg]).current_dir(ROOT).env("STAAL_TEST_PG_WRONG", "not-the-password-0000").output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(err.contains("cannot connect to database shop") && err.contains("password authentication failed"), "{err}");
    assert!(!err.contains("not-the-password-0000"), "{err}");
}

#[test]
fn grants_name_their_databases_and_plain_tcp_only_stays_on_this_machine() {
    if skip() {
        return;
    }
    let dir = scratch("grants");
    let base = json!({ "host": "127.0.0.1", "dbname": "shop", "user": USER, "password": { "env": PW_VAR }, "tls": "disable" });
    for (g, why) in [
        (json!({ "cap": "db.postgres.query" }), "databases: the databases it reaches"),
        (json!({ "cap": "db.postgres.query", "databases": {} }), "names no database"),
        (json!({ "cap": "db.postgres.query", "databases": { "shop": { "host": "db.example.com", "dbname": "shop", "user": "u", "tls": "disable" } } }), "\"disable\" only for a server on this machine"),
        (json!({ "cap": "db.postgres.query", "databases": { "shop": { "host": "127.0.0.1", "dbname": "shop", "user": "u", "tls": "none" } } }), "\"verify\" (the default) or \"disable\""),
        (json!({ "cap": "db.postgres.query", "databases": { "shop": { "host": "127.0.0.1", "dbname": "shop", "user": "u", "port": 70000 } } }), "port: a port, 1 to 65535"),
        (json!({ "cap": "db.postgres.query", "databases": { "shop": { "host": "127.0.0.1", "dbname": "shop" } } }), "user: expected a string"),
        (json!({ "cap": "db.postgres.query", "databases": { "shop": { "host": "127.0.0.1", "dbname": "shop", "user": "u", "sslmode": "x" } } }), "unknown key 'sslmode'"),
        (json!({ "cap": "db.postgres.query", "databases": { "shop": base }, "only": ["shop"] }), "`only` does not apply"),
        (json!({ "cap": "db.postgres.query", "databases": { "shop": base }, "idempotency_key": true }), "idempotency_key: only for"),
        (json!({ "cap": "db.postgres.query", "databases": { "shop": { "host": "127.0.0.1", "dbname": "shop", "user": "u", "password": { "env": "STAAL_TEST_PG_UNSET" } } } }), "STAAL_TEST_PG_UNSET is not set"),
    ] {
        let cfg = write_config(&dir, "g", json!({ "entry": "kinds", "grants": { "read": g } }));
        let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg], None);
        assert_eq!(code, Some(2), "{g}: {err}");
        assert!(err.contains(why), "{g}: {err}");
    }
}

#[test]
fn tls_is_verified_against_the_operator_s_roots_and_the_host_name() {
    if skip() {
        return;
    }
    let dir = scratch("tls");
    let ca = common::tls::Ca::new("Staal test CA");
    let (cert, key) = ca.server_pem(&["localhost"]);
    let s = shop_server(&dir, Some((&cert, &key)));
    let ca_file = dir.join("ca.pem");
    std::fs::write(&ca_file, ca.pem()).unwrap();
    let run = |host: &str, roots: bool| {
        let db = database(&s, json!({ "host": host, "tls": "verify" }));
        let mut cfg = json!({ "entry": "kinds", "grants": { "read": grant("db.postgres.query", &db) } });
        if roots {
            cfg["net"] = json!({ "extra_root_certificates": [ca_file.to_str().unwrap()] });
        }
        let p = write_config(&dir, "tls", cfg);
        staalc(&["run", PROGRAM, "--config", &p], None)
    };
    let (code, out, err) = run("localhost", true);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("naira"), "{out}");
    // Not trusted without the operator's root.
    let (code, _, err) = run("localhost", false);
    assert_eq!(code, Some(1));
    assert!(err.contains("cannot connect to database shop") && err.contains("certificate"), "{err}");
    // Trusted, but for another name.
    let (code, _, err) = run("127.0.0.1", true);
    assert_eq!(code, Some(1));
    assert!(err.contains("certificate"), "{err}");
}

#[test]
fn a_journaled_run_replays_its_queries_and_keeps_the_password_out() {
    if skip() {
        return;
    }
    let dir = scratch("journal");
    let s = shop_server(&dir, None);
    let cfg = record_config(&dir, &s, "once", json!({}));
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let log = dir.join("run.jsonl").to_string_lossy().into_owned();
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j, "--log", &log], None);
    assert_eq!((code, out.as_str()), (Some(0), "1"), "{err}");
    for engine in engines() {
        let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j, "--engine", engine], None);
        assert_eq!((code, out.as_str()), (Some(0), "1"), "{engine}: {err}");
    }
    assert_eq!(count(&s, "once"), 1, "a replay ran the statement again");
    for f in [&j, &log] {
        let text = std::fs::read_to_string(f).unwrap();
        assert!(!text.contains(PASSWORD), "{f} holds the password");
        assert!(!text.contains("INSERT INTO"), "{f} holds the SQL, not its hash");
    }
}

/// Killed at every journal write and between each statement and its result, then
/// resumed: with idempotency keys the statement is applied exactly once, by itself;
/// without, a statement in doubt waits for an operator, who records what happened.
#[cfg(feature = "failpoints")]
#[test]
fn every_crash_point_applies_each_statement_once() {
    if skip() {
        return;
    }
    let dir = scratch("crash");
    let s = shop_server(&dir, None);
    let baseline = record_config(&dir, &s, "baseline", json!({}));
    let j = dir.join("baseline.journal").to_string_lossy().into_owned();
    assert_eq!(staalc(&["run", PROGRAM, "--config", &baseline, "--journal", &j], None).0, Some(0));
    let writes = std::fs::read_to_string(&j).unwrap().lines().count();
    let mut points: Vec<String> = (1..=writes).map(|k| format!("after_write:{k}")).collect();
    points.extend((1..=2).map(|n| format!("before_result:{n}")));
    for keyed in [true, false] {
        let mut asked = 0;
        for (i, fp) in points.iter().enumerate() {
            let item = format!("{}{i}", if keyed { "k" } else { "u" });
            let cfg = record_config(&dir, &s, &item, if keyed { json!({ "idempotency_key": true }) } else { json!({}) });
            let j = dir.join(format!("{item}.journal")).to_string_lossy().into_owned();
            let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], Some(fp));
            assert_eq!(code, None, "{item} {fp}: {err}");
            let mut out = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
            if out.0 == Some(3) {
                assert!(!keyed, "{fp}: a keyed statement was left for an operator: {}", out.2);
                asked += 1;
                // The operator looks, and records what the database shows.
                let n = count(&s, &item);
                let r = if n == 1 {
                    staalc(&["resolve", "--journal", &j, "--performed", "postgres:1"], None)
                } else {
                    staalc(&["resolve", "--journal", &j, "--retry"], None)
                };
                assert_eq!(r.0, Some(0), "{fp}: {}", r.2);
                out = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
            }
            assert_eq!(out.0, Some(0), "{item} {fp}: {}", out.2);
            assert_eq!(count(&s, &item), 1, "{item} {fp}: applied {} times", count(&s, &item));
        }
        // Without keys, a crash between a statement and its result is left for an
        // operator; with them, never.
        assert_eq!(asked > 0, !keyed, "keyed {keyed}: {asked} left for an operator");
    }
}

/// Forwards one client's connection to `server`, and when the client sends COMMIT,
/// passes it on and then breaks the connection before the answer comes back: the
/// commit happens, and the client cannot know it did.
fn commit_breaker(server: u16) -> u16 {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for client in l.incoming() {
            let Ok(mut client) = client else { continue };
            let Ok(mut up) = std::net::TcpStream::connect(("127.0.0.1", server)) else { continue };
            let (mut c2, mut u2) = (client.try_clone().unwrap(), up.try_clone().unwrap());
            // Server to client, as it comes.
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut u2, &mut c2);
            });
            // Client to server, message by message: the startup message has no type byte.
            let mut first = true;
            loop {
                let mut head = vec![0u8; if first { 4 } else { 5 }];
                if client.read_exact(&mut head).is_err() {
                    break;
                }
                let n = u32::from_be_bytes(head[head.len() - 4..].try_into().unwrap()) as usize;
                let mut body = vec![0u8; n - 4];
                if client.read_exact(&mut body).is_err() {
                    break;
                }
                let commit = !first && head[0] == b'Q' && body.starts_with(b"COMMIT");
                first = false;
                if commit {
                    // Cut the client off first, so no answer can reach it; then let the
                    // server have the COMMIT, and the time to carry it out.
                    let _ = client.shutdown(std::net::Shutdown::Both);
                    let _ = up.write_all(&head);
                    let _ = up.write_all(&body);
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let _ = up.shutdown(std::net::Shutdown::Both);
                    break;
                }
                let _ = up.write_all(&head);
                let _ = up.write_all(&body);
            }
        }
    });
    port
}

#[test]
fn a_connection_lost_during_commit_leaves_the_statement_in_doubt() {
    if skip() {
        return;
    }
    let dir = scratch("commit");
    let s = shop_server(&dir, None);
    let proxy = commit_breaker(s.port);
    let mut db = database(&s, json!({}));
    db["port"] = json!(proxy);
    let read = grant("db.postgres.query", &database(&s, json!({})));
    let cfg = write_config(&dir, "c", json!({ "entry": "record", "inputs": { "item": "doubt", "qty": 2 }, "grants": { "db": grant("db.postgres.execute", &db), "read": read } }));
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j], None);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("failed while committing"), "{err}");
    // It did commit (the proxy hands the server the COMMIT just after cutting the
    // client off); nobody guessed otherwise.
    let start = std::time::Instant::now();
    while count(&s, "doubt") == 0 && start.elapsed().as_secs() < 5 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(count(&s, "doubt"), 1);
    let (code, _, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j], None);
    assert_eq!(code, Some(3), "resume does not guess either: {err}");
    assert_eq!(count(&s, "doubt"), 1);
}
