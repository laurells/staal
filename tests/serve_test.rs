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

fn staalc(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(BIN).args(args).current_dir(ROOT).output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
}

fn engines() -> Vec<&'static str> {
    if cfg!(feature = "native") { vec!["interpreter", "native"] } else { vec!["interpreter"] }
}

#[test]
fn requests_are_answered_one_after_another() {
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
