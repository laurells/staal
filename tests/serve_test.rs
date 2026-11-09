// `net.tcp.accept` and `net.tcp.reply` end to end through `staalc run` and `resume`,
// with clients in this test: a program listens only where its grant allows, answers
// each request once, and a resumed run neither answers again nor pretends to.

use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const PROGRAM: &str = "tests/serve/serve.staal";

/// The tests in this file take turns. Each finds a free port, releases it for its
/// program, and connects to it; a test running beside another could be handed the
/// same port and reach the other's program. A test that fails releases the lock.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A free local address (bound, then released for the program to bind).
fn free_address() -> String {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string()
}

/// A client sending `line` once the program listens, and reading the answer to EOF.
fn client(address: String, line: &'static str) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let start = Instant::now();
        let mut c = loop {
            match TcpStream::connect(&address) {
                Ok(c) => break c,
                Err(_) if start.elapsed() < Duration::from_secs(20) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("never listening on {address}: {e}"),
            }
        };
        c.write_all(line.as_bytes()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut got = String::new();
        let _ = c.read_to_string(&mut got);
        got
    })
}

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("serve").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(dir: &Path, entry: &str, address: &str, only: &str) -> String {
    let mut grants = json!({ "listen": { "cap": "net.tcp.accept", "only": [only] } });
    if entry != "peek" {
        grants["answer"] = json!({ "cap": "net.tcp.reply" });
    }
    let cfg = json!({
        "entry": entry,
        "inputs": { "address": address },
        "grants": grants,
        "model": { "provider": "script", "responses": [] },
    });
    let p = dir.join("cfg.json");
    std::fs::write(&p, cfg.to_string()).unwrap();
    p.to_string_lossy().into_owned()
}

/// `config` with more keys on the `listen` grant.
fn config_with(dir: &Path, entry: &str, address: &str, listen: serde_json::Value) -> String {
    let p = config(dir, entry, address, "127.0.0.1:*");
    let mut cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    for (k, v) in listen.as_object().unwrap() {
        cfg["grants"]["listen"][k] = v.clone();
    }
    std::fs::write(&p, cfg.to_string()).unwrap();
    p
}

/// A client that sends raw bytes and reads whatever comes back; for clients the run
/// must drop. Connects once the program listens.
fn raw_client(address: String, bytes: Vec<u8>, close: bool) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let start = Instant::now();
        let mut c = loop {
            match TcpStream::connect(&address) {
                Ok(c) => break c,
                Err(_) if start.elapsed() < Duration::from_secs(20) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("never listening on {address}: {e}"),
            }
        };
        let _ = c.write_all(&bytes);
        if close {
            let _ = c.shutdown(std::net::Shutdown::Write);
        }
        c.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut got = Vec::new();
        let _ = c.read_to_end(&mut got);
        got
    })
}

fn staalc(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(BIN).args(args).current_dir(ROOT).output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
}

fn engines() -> Vec<&'static str> {
    if cfg!(feature = "native") { vec!["interpreter", "native"] } else { vec!["interpreter"] }
}

#[test]
fn requests_are_answered_one_after_another() {
    let _turn = one_at_a_time();
    for engine in engines() {
        let addr = free_address();
        let dir = scratch(&format!("serial-{engine}"));
        let cfg = config(&dir, "serve", &addr, "127.0.0.1:*");
        let clients: Vec<_> = ["a\n", "b\n", "c\n"]
            .into_iter()
            .map(|l| {
                let h = client(addr.clone(), l);
                std::thread::sleep(Duration::from_millis(50));
                h
            })
            .collect();
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine]);
        assert_eq!((code, out.as_str()), (Some(0), "3"), "{engine}: {err}");
        let mut got: Vec<String> = clients.into_iter().map(|h| h.join().unwrap()).collect();
        got.sort();
        assert_eq!(got, ["+a\n", "+b\n", "+c\n"], "{engine}");
    }
}

#[test]
fn concurrent_tasks_answer_their_own_requests() {
    let _turn = one_at_a_time();
    for engine in engines() {
        let addr = free_address();
        let dir = scratch(&format!("parallel-{engine}"));
        let cfg = config(&dir, "serve_parallel", &addr, "127.0.0.1:*");
        let clients: Vec<_> = ["x\n", "y\n", "z\n"].into_iter().map(|l| client(addr.clone(), l)).collect();
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine]);
        assert_eq!((code, out.as_str()), (Some(0), "3"), "{engine}: {err}");
        let mut got: Vec<String> = clients.into_iter().map(|h| h.join().unwrap()).collect();
        got.sort();
        assert_eq!(got, ["+x\n", "+y\n", "+z\n"], "{engine}: every client gets its own answer");
    }
}

#[test]
fn a_program_listens_only_where_its_grant_allows() {
    let _turn = one_at_a_time();
    let addr = free_address();
    let dir = scratch("only");
    let cfg = config(&dir, "serve", &addr, "127.0.0.2:*");
    // Clients try for a while, so a run that listened anyway would end, not hang.
    let tries = {
        let addr = addr.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut reached = 0;
            while start.elapsed() < Duration::from_secs(5) && reached < 3 {
                if let Ok(mut c) = TcpStream::connect(&addr) {
                    // Connecting to a port nobody listens on can connect the socket to
                    // itself (both ends the same address and port): that is no run.
                    if c.local_addr().ok() == c.peer_addr().ok() {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    reached += 1;
                    let _ = c.write_all(b"a\n");
                    let _ = c.read_to_string(&mut String::new());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            reached
        })
    };
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg]);
    assert_eq!(tries.join().unwrap(), 0, "a client reached a run that should not listen");
    assert_ne!(code, Some(0));
    assert!(err.contains(&addr) && err.contains("127.0.0.2:*"), "{err}");
    // Nothing was bound: the address is still free.
    TcpListener::bind(&addr).expect("the refused run listened anyway");
}

#[test]
fn a_reply_takes_no_address_limit() {
    let _turn = one_at_a_time();
    let dir = scratch("reply-only");
    let cfg = json!({
        "entry": "serve",
        "inputs": { "address": "127.0.0.1:1" },
        "grants": { "listen": { "cap": "net.tcp.accept" }, "answer": { "cap": "net.tcp.reply", "only": ["127.0.0.1:*"] } },
        "model": { "provider": "script", "responses": [] },
    });
    let p = dir.join("cfg.json");
    std::fs::write(&p, cfg.to_string()).unwrap();
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", p.to_str().unwrap()]);
    assert_ne!(code, Some(0));
    assert!(err.contains("a reply goes to a connection accepted there"), "{err}");
}

#[test]
fn a_request_without_a_delimiter_is_read_to_its_end() {
    let _turn = one_at_a_time();
    let addr = free_address();
    let dir = scratch("peek");
    let cfg = config(&dir, "peek", &addr, "127.0.0.1:*");
    // Without a delimiter the request ends when the client stops sending.
    let h = std::thread::spawn({
        let addr = addr.clone();
        move || {
            let start = Instant::now();
            let mut c = loop {
                match TcpStream::connect(&addr) {
                    Ok(c) => break c,
                    Err(_) if start.elapsed() < Duration::from_secs(20) => std::thread::sleep(Duration::from_millis(20)),
                    Err(e) => panic!("{e}"),
                }
            };
            c.write_all(b"two\nlines").unwrap();
            c.shutdown(std::net::Shutdown::Write).unwrap();
        }
    });
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg]);
    h.join().unwrap();
    assert_eq!((code, out.as_str()), (Some(0), "\"two\\nlines\""), "{err}");
}

#[test]
fn a_finished_run_replays_without_listening() {
    let _turn = one_at_a_time();
    let addr = free_address();
    let dir = scratch("journal");
    let cfg = config(&dir, "serve", &addr, "127.0.0.1:*");
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let clients: Vec<_> = ["a\n", "b\n", "c\n"].into_iter().map(|l| client(addr.clone(), l)).collect();
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j]);
    assert_eq!((code, out.as_str()), (Some(0), "3"), "{err}");
    for h in clients {
        h.join().unwrap();
    }
    // The address is taken by something else now; the replay never needs it.
    let _held = TcpListener::bind(&addr).unwrap();
    let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j]);
    assert_eq!((code, out.as_str()), (Some(0), "3"), "{err}");
    // The journal holds a hash of each answer, not the answer.
    let text = std::fs::read_to_string(&j).unwrap();
    assert!(text.contains("net.tcp.reply") && text.contains("data_sha256") && !text.contains("+a"), "{text}");
}

#[test]
fn a_connection_is_answered_once() {
    let _turn = one_at_a_time();
    for engine in engines() {
        let addr = free_address();
        let dir = scratch(&format!("twice-{engine}"));
        let cfg = config(&dir, "twice", &addr, "127.0.0.1:*");
        let c = client(addr.clone(), "q\n");
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine]);
        assert_eq!(code, Some(0), "{engine}: {err}");
        assert!(out.contains("is not open"), "{engine}: {out}");
        assert_eq!(c.join().unwrap(), "one\n", "{engine}");
    }
}

#[test]
fn a_wait_limit_ends_an_accept_with_no_client_and_is_replayed() {
    let _turn = one_at_a_time();
    for engine in engines() {
        let addr = free_address();
        let dir = scratch(&format!("wait-{engine}"));
        let cfg = config_with(&dir, "waited", &addr, json!({ "wait_ms": 300 }));
        let j = dir.join("run.journal").to_string_lossy().into_owned();
        let start = Instant::now();
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j, "--engine", engine]);
        let took = start.elapsed();
        let want = format!("\"action: no client connected to {addr} within 300 ms\"");
        assert_eq!((code, out.as_str()), (Some(0), want.as_str()), "{engine}: {err}");
        assert!(took >= Duration::from_millis(300), "{engine}: ended after {took:?}, before the limit");
        // Replayed from the journal: the same failure, without waiting again.
        let start = Instant::now();
        let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j, "--engine", engine]);
        assert_eq!((code, out.as_str()), (Some(0), want.as_str()), "{engine}: {err}");
        assert!(start.elapsed() < Duration::from_millis(300), "{engine}: the replay waited");
        // A client in time is served as without a limit.
        let addr = free_address();
        let cfg = config_with(&dir, "waited", &addr, json!({ "wait_ms": 20000 }));
        let c = client(addr.clone(), "x\n");
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine]);
        assert_eq!((code, out.as_str()), (Some(0), "\"served\""), "{engine}: {err}");
        assert_eq!(c.join().unwrap(), "+x\n", "{engine}");
    }
}

#[test]
fn a_run_killed_while_waiting_takes_the_next_client_on_resume() {
    let _turn = one_at_a_time();
    for engine in engines() {
        let addr = free_address();
        let dir = scratch(&format!("killed-waiting-{engine}"));
        let cfg = config_with(&dir, "waited", &addr, json!({ "wait_ms": 60000 }));
        let j = dir.join("run.journal");
        let mut run = Command::new(BIN)
            .args(["run", PROGRAM, "--config", &cfg, "--journal", j.to_str().unwrap(), "--engine", engine])
            .current_dir(ROOT)
            .spawn()
            .unwrap();
        // Waiting: the accept's intent is in the journal, and no client has come.
        let start = Instant::now();
        while !std::fs::read_to_string(&j).unwrap_or_default().contains("net.tcp.accept") {
            assert!(start.elapsed() < Duration::from_secs(20), "{engine}: the run never began to wait");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200));
        run.kill().unwrap(); // SIGKILL
        run.wait().unwrap();
        // Resumed, the accept is in doubt and safe to make again: it takes the next client.
        let c = client(addr.clone(), "after\n");
        let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", j.to_str().unwrap(), "--engine", engine]);
        assert_eq!((code, out.as_str()), (Some(0), "\"served\""), "{engine}: {err}");
        assert_eq!(c.join().unwrap(), "+after\n", "{engine}");
    }
}

#[test]
fn bad_clients_are_dropped_and_logged_and_good_ones_served() {
    let _turn = one_at_a_time();
    for engine in engines() {
        let addr = free_address();
        let dir = scratch(&format!("bad-clients-{engine}"));
        // net.max_response_bytes bounds a request: 64 bytes here.
        let p = config(&dir, "serve", &addr, "127.0.0.1:*");
        let mut cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        cfg["net"] = json!({ "max_response_bytes": 64, "timeout_ms": 2000 });
        std::fs::write(&p, cfg.to_string()).unwrap();
        let log = dir.join("run.jsonl");
        // Clients one at a time, in this order: each is accepted before the next comes.
        let order: Vec<(&str, Vec<u8>)> = vec![
            ("good", b"one\n".to_vec()),
            ("too long", vec![b'x'; 100]),
            ("good", b"two\n".to_vec()),
            ("not UTF-8", vec![0xff, 0xfe, b'\n']),
            ("good", b"three\n".to_vec()),
        ];
        let feeder = std::thread::spawn({
            let addr = addr.clone();
            move || {
                let mut got = Vec::new();
                for (what, bytes) in order {
                    let close = what != "good";
                    got.push((what, raw_client(addr.clone(), bytes, close).join().unwrap()));
                }
                got
            }
        });
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &p, "--log", log.to_str().unwrap(), "--engine", engine]);
        assert_eq!((code, out.as_str()), (Some(0), "3"), "{engine}: {err}");
        let got = feeder.join().unwrap();
        let answers: Vec<String> = got.iter().map(|(_, b)| String::from_utf8_lossy(b).into_owned()).collect();
        assert_eq!(answers, ["+one\n", "", "+two\n", "", "+three\n"], "{engine}: every good client answered, no bad one");
        let events: Vec<serde_json::Value> =
            std::fs::read_to_string(&log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let dropped: Vec<&str> = events.iter().filter(|e| e["event"] == "client_dropped").map(|e| e["reason"].as_str().unwrap()).collect();
        assert_eq!(dropped.len(), 2, "{engine}: {dropped:?}");
        assert!(dropped[0].contains("longer than 64 bytes") && dropped[1].contains("not UTF-8"), "{engine}: {dropped:?}");
        // The reason never holds what the client sent.
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(!text.contains("xxxxxxxxxx"), "{engine}: a dropped request reached the log");
    }
}

#[test]
fn a_bad_client_fails_the_accept_when_the_grant_says_fail() {
    let _turn = one_at_a_time();
    for engine in engines() {
        let addr = free_address();
        let dir = scratch(&format!("fail-bad-{engine}"));
        let cfg = config_with(&dir, "waited", &addr, json!({ "on_bad_client": "fail" }));
        let bad = raw_client(addr.clone(), vec![0xff, b'\n'], true);
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine]);
        assert_eq!(code, Some(0), "{engine}: {err}");
        assert!(out.contains("is not UTF-8 text"), "{engine}: {out}");
        assert_eq!(bad.join().unwrap(), b"", "{engine}");
    }
}

#[test]
fn the_listening_settings_are_checked_before_anything_runs() {
    let _turn = one_at_a_time();
    let dir = scratch("listen-settings");
    for (extra, want) in [
        (json!({ "wait_ms": 0 }), "wait_ms: expected a whole number of milliseconds from 1 to 2147483647"),
        (json!({ "wait_ms": 2147483648u64 }), "wait_ms: expected a whole number"),
        (json!({ "wait_ms": 1.5 }), "wait_ms: expected a whole number"),
        (json!({ "wait_ms": "100" }), "wait_ms: expected a whole number"),
        (json!({ "on_bad_client": "maybe" }), "on_bad_client: expected \"skip\" or \"fail\""),
        (json!({ "on_bad_client": true }), "on_bad_client: expected \"skip\" or \"fail\""),
    ] {
        let cfg = config_with(&dir, "waited", "127.0.0.1:1", extra.clone());
        let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg]);
        assert_eq!(code, Some(2), "{extra}: {err}");
        assert!(err.contains(want), "{extra}: {err}");
    }
    // Only a net.tcp.accept grant takes them.
    let p = config(&dir, "serve", "127.0.0.1:1", "127.0.0.1:*");
    let mut cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    cfg["grants"]["answer"]["wait_ms"] = json!(100);
    std::fs::write(&p, cfg.to_string()).unwrap();
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &p]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("grants.answer: unknown key 'wait_ms'"), "{err}");
}
