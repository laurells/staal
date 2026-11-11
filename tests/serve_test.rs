// `net.tcp.accept` and `net.tcp.reply` end to end through `staalc run` and `resume`,
// with clients in this test: a program listens only where its grant allows, answers
// each request once, and a resumed run neither answers again nor pretends to.

mod common;

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

/// A local address no other test binds, for the program to bind.
fn free_address() -> String {
    common::ports::address()
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

// ---- net.tls.accept and net.tls.reply ----

const TLS_PROGRAM: &str = "tests/serve/serve_tls.staal";

/// A run config for serve_tls.staal: `listen` a net.tls.accept grant with the
/// certificate files, `answer` a reply grant of `reply` (net.tls.reply, or net.tcp.reply
/// to show it is refused).
fn tls_config(dir: &Path, entry: &str, address: &str, files: &(String, String), reply: &str, extra: serde_json::Value) -> String {
    let mut listen = json!({ "cap": "net.tls.accept", "only": ["127.0.0.1:*"],
                             "certificate_chain": files.0, "private_key": files.1 });
    for (k, v) in extra.as_object().unwrap() {
        listen[k] = v.clone();
    }
    let cfg = json!({
        "entry": entry,
        "inputs": { "address": address },
        "grants": { "listen": listen, "answer": { "cap": reply } },
        "model": { "provider": "script", "responses": [] },
    });
    let p = dir.join("cfg.json");
    std::fs::write(&p, cfg.to_string()).unwrap();
    p.to_string_lossy().into_owned()
}

/// A TLS client verifying the certificate for localhost against `cfg`'s authority:
/// sends `line` once the program listens, and reads the answer to a proper end of TLS.
fn tls_client(address: String, cfg: std::sync::Arc<rustls::ClientConfig>, line: &'static str) -> std::thread::JoinHandle<Result<String, String>> {
    std::thread::spawn(move || {
        let start = Instant::now();
        let st = loop {
            match TcpStream::connect(&address) {
                Ok(c) => break c,
                Err(_) if start.elapsed() < Duration::from_secs(20) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("never listening on {address}: {e}"),
            }
        };
        st.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut t = rustls::StreamOwned::new(rustls::ClientConnection::new(cfg, name).unwrap(), st);
        t.write_all(line.as_bytes()).and_then(|_| t.flush()).map_err(|e| e.to_string())?;
        let mut got = String::new();
        t.read_to_string(&mut got).map_err(|e| e.to_string())?;
        Ok(got)
    })
}

#[test]
fn requests_over_tls_are_answered_over_tls() {
    let _turn = one_at_a_time();
    let ca = common::tls::Ca::new("Serve Test CA");
    for engine in engines() {
        let dir = scratch(&format!("tls-{engine}"));
        let files = ca.listen_files(&dir, &["localhost"], None);
        // One after another, and in parallel tasks.
        for (entry, lines) in [("serve", ["a\n", "b\n", "c\n"]), ("serve_parallel", ["x\n", "y\n", "z\n"])] {
            let addr = free_address();
            let cfg = tls_config(&dir, entry, &addr, &files, "net.tls.reply", json!({}));
            let j = dir.join(format!("{entry}.journal"));
            let clients: Vec<_> = if entry == "serve" {
                // Each starts once the one before it is answered.
                let (addr, client_cfg) = (addr.clone(), ca.client());
                vec![std::thread::spawn(move || {
                    lines.iter().map(|l| tls_client(addr.clone(), client_cfg.clone(), l).join().unwrap()).collect::<Vec<_>>()
                })]
            } else {
                lines
                    .iter()
                    .map(|l| {
                        let h = tls_client(addr.clone(), ca.client(), l);
                        std::thread::spawn(move || vec![h.join().unwrap()])
                    })
                    .collect()
            };
            let (code, out, err) = staalc(&["run", TLS_PROGRAM, "--config", &cfg, "--journal", j.to_str().unwrap(), "--engine", engine]);
            assert_eq!((code, out.as_str()), (Some(0), "3"), "{engine} {entry}: {err}");
            let mut got: Vec<String> = clients.into_iter().flat_map(|c| c.join().unwrap()).map(|r| r.unwrap()).collect();
            got.sort();
            let mut want: Vec<String> = lines.iter().map(|l| format!("+{l}")).collect();
            want.sort();
            assert_eq!(got, want, "{engine} {entry}");
            // The journal holds each request, a hash of each reply, and no key.
            let journal = std::fs::read_to_string(&j).unwrap();
            assert!(journal.contains("net.tls.accept") && journal.contains("data_sha256"), "{engine}");
            assert!(!journal.contains("PRIVATE KEY") && !journal.contains(&format!("+{}", lines[0].trim())), "{engine}: {journal}");
            // A finished run replays without listening.
            let (code, out, err) = staalc(&["resume", TLS_PROGRAM, "--config", &cfg, "--journal", j.to_str().unwrap(), "--engine", engine]);
            assert_eq!((code, out.as_str()), (Some(0), "3"), "{engine} {entry} replay: {err}");
        }
    }
}

#[test]
fn a_connection_accepted_over_tls_is_answered_only_over_tls() {
    let _turn = one_at_a_time();
    let ca = common::tls::Ca::new("Serve Test CA");
    for engine in engines() {
        let dir = scratch(&format!("tls-wrong-reply-{engine}"));
        let files = ca.listen_files(&dir, &["localhost"], None);
        let addr = free_address();
        let cfg = tls_config(&dir, "wrong_reply", &addr, &files, "net.tcp.reply", json!({}));
        let c = tls_client(addr.clone(), ca.client(), "q\n");
        let (code, out, err) = staalc(&["run", TLS_PROGRAM, "--config", &cfg, "--engine", engine]);
        assert_eq!(code, Some(0), "{engine}: {err}");
        assert!(out.contains("was accepted over TLS (net.tls.accept); answer it with net.tls.reply"), "{engine}: {out}");
        // Nothing was sent in plain text: the client saw its connection end, not "no".
        assert_ne!(c.join().unwrap(), Ok("no\n".to_string()), "{engine}");
    }
}

#[test]
fn a_bad_tls_client_is_dropped_and_logged_and_a_good_one_served() {
    let _turn = one_at_a_time();
    let ca = common::tls::Ca::new("Serve Test CA");
    for engine in engines() {
        let dir = scratch(&format!("tls-bad-{engine}"));
        let files = ca.listen_files(&dir, &["localhost"], None);
        let addr = free_address();
        let cfg = tls_config(&dir, "waited", &addr, &files, "net.tls.reply", json!({ "wait_ms": 20000 }));
        let log = dir.join("run.jsonl");
        // A plain-text client first, then one that verifies the certificate.
        let feeder = std::thread::spawn({
            let (addr, client_cfg) = (addr.clone(), ca.client());
            move || {
                let plain = raw_client(addr.clone(), b"hello\n".to_vec(), true).join().unwrap();
                (plain, tls_client(addr, client_cfg, "fine\n").join().unwrap())
            }
        });
        let (code, out, err) = staalc(&["run", TLS_PROGRAM, "--config", &cfg, "--log", log.to_str().unwrap(), "--engine", engine]);
        assert_eq!((code, out.as_str()), (Some(0), "\"served\""), "{engine}: {err}");
        let (plain, good) = feeder.join().unwrap();
        assert!(!String::from_utf8_lossy(&plain).contains("+hello"), "{engine}: answered in plain text");
        assert_eq!(good, Ok("+fine\n".to_string()), "{engine}");
        let text = std::fs::read_to_string(&log).unwrap();
        let dropped: Vec<serde_json::Value> =
            text.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).filter(|e| e["event"] == "client_dropped").collect();
        assert_eq!(dropped.len(), 1, "{engine}: {dropped:?}");
        assert!(dropped[0]["reason"].as_str().unwrap().contains("the TLS handshake with"), "{engine}: {dropped:?}");
        assert_eq!(dropped[0]["capability"], "net.tls.accept", "{engine}");
    }
}

#[test]
fn a_certificate_or_key_that_cannot_be_used_stops_the_run_before_it_starts() {
    use std::os::unix::fs::PermissionsExt;
    let _turn = one_at_a_time();
    let ca = common::tls::Ca::new("Serve Test CA");
    let dir = scratch("tls-config");
    let refused = |cfg: &str, want: &str| {
        let (code, out, err) = staalc(&["run", TLS_PROGRAM, "--config", cfg]);
        assert_eq!(code, Some(2), "{want}: {out} {err}");
        assert!(err.contains(want), "{want}: {err}");
    };
    // Expired, and not yet valid.
    let files = ca.listen_files(&dir, &["localhost"], Some((2000, 2001)));
    refused(&tls_config(&dir, "serve", "127.0.0.1:1", &files, "net.tls.reply", json!({})), "expired on 2001-01-01");
    let files = ca.listen_files(&dir, &["localhost"], Some((2999, 3000)));
    refused(&tls_config(&dir, "serve", "127.0.0.1:1", &files, "net.tls.reply", json!({})), "is not valid until 2999-01-01");
    // Another certificate's key.
    let good = ca.listen_files(&dir, &["localhost"], None);
    let other = scratch("tls-config-other");
    let theirs = ca.listen_files(&other, &["localhost"], None);
    refused(&tls_config(&dir, "serve", "127.0.0.1:1", &(good.0.clone(), theirs.1.clone()), "net.tls.reply", json!({})), "is not the key of the certificate");
    // A key others can read.
    std::fs::set_permissions(&good.1, std::fs::Permissions::from_mode(0o644)).unwrap();
    refused(&tls_config(&dir, "serve", "127.0.0.1:1", &good, "net.tls.reply", json!({})), "can be read or written by other users");
    std::fs::set_permissions(&good.1, std::fs::Permissions::from_mode(0o600)).unwrap();
    // Missing files in the grant, and the keys only net.tls.accept takes.
    let p = tls_config(&dir, "serve", "127.0.0.1:1", &good, "net.tls.reply", json!({}));
    let mut cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    cfg["grants"]["listen"].as_object_mut().unwrap().remove("private_key");
    std::fs::write(&p, cfg.to_string()).unwrap();
    refused(&p, "grants.listen.private_key: expected a file path");
    let plain = config(&dir, "serve", "127.0.0.1:1", "127.0.0.1:*");
    let mut cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&plain).unwrap()).unwrap();
    cfg["grants"]["listen"]["certificate_chain"] = json!(good.0);
    std::fs::write(&plain, cfg.to_string()).unwrap();
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &plain]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("grants.listen: unknown key 'certificate_chain'"), "{err}");
    // A reply grant takes no `only`.
    let p = tls_config(&dir, "serve", "127.0.0.1:1", &good, "net.tls.reply", json!({}));
    let mut cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    cfg["grants"]["answer"]["only"] = json!(["127.0.0.1:*"]);
    std::fs::write(&p, cfg.to_string()).unwrap();
    refused(&p, "a reply goes to a connection accepted there");
}
