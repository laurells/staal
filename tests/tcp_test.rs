// `net.tcp.exchange` end to end through `staalc run`, `resume` and `resolve`, against
// a server in this test: only the addresses a grant allows, every exchange recorded,
// and one left in doubt never repeated or guessed.

use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// A server answering each line `ECHO w` with `+w` (or never answering, when
/// `answer` is false), counting its connections.
fn server(answer: bool) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let seen = Arc::new(AtomicUsize::new(0));
    let count = seen.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            count.fetch_add(1, Ordering::SeqCst);
            let mut line = String::new();
            if BufReader::new(&c).read_line(&mut line).is_ok() && answer {
                let w = line.trim().strip_prefix("ECHO ").unwrap_or("?");
                let _ = c.write_all(format!("+{w}\r\n").as_bytes());
            } else {
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }
    });
    (addr, seen)
}

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tcp").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(dir: &Path, entry: &str, address: &str, only: &str) -> String {
    let mut inputs = json!({ "address": address });
    if entry == "ask" {
        inputs["word"] = json!("hello");
    }
    let cfg = json!({
        "entry": entry,
        "inputs": inputs,
        "grants": { "conn": { "cap": "net.tcp.exchange", "only": [only] } },
        "model": { "provider": "script", "responses": [] },
        "net": { "timeout_ms": 300 },
    });
    let p = dir.join("cfg.json");
    std::fs::write(&p, cfg.to_string()).unwrap();
    p.to_string_lossy().into_owned()
}

fn staalc(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(BIN).args(args).current_dir(ROOT).output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string())
}

const PROGRAM: &str = "tests/tcp/echo.staal";

#[test]
fn an_exchange_reaches_only_what_the_grant_allows() {
    let (addr, seen) = server(true);
    let dir = scratch("basic");
    for engine in ["interpreter", "native"] {
        if engine == "native" && !cfg!(feature = "native") {
            continue;
        }
        let cfg = config(&dir, "ask", &addr, "127.0.0.1:*");
        let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--engine", engine]);
        assert_eq!((code, out.as_str()), (Some(0), "\"+hello\""), "{engine}: {err}");
    }
    let before = seen.load(Ordering::SeqCst);
    // Another host than the grant names, though it is the same machine: refused, and
    // nothing connects.
    let port = addr.rsplit_once(':').unwrap().1;
    let cfg = config(&dir, "ask", &format!("localhost:{port}"), "127.0.0.1:*");
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg]);
    assert_eq!(code, Some(1));
    assert!(err.contains("capability") && err.contains("localhost"), "{err}");
    assert_eq!(seen.load(Ordering::SeqCst), before, "a refused exchange connected");
    // A pattern that is not canonical is a config error.
    let cfg = config(&dir, "ask", &addr, "*.0.0.1:*");
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg]);
    assert_eq!(code, Some(2), "{err}");
}

#[test]
fn nothing_listening_is_a_failure_the_program_can_catch() {
    let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let dir = scratch("refused");
    let cfg = config(&dir, "try_ask", &free, "127.0.0.1:*");
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.starts_with("\"action: not sent: cannot connect"), "{out}");
}

#[test]
fn a_finished_run_replays_without_connecting() {
    let (addr, seen) = server(true);
    let dir = scratch("journal");
    let cfg = config(&dir, "ask", &addr, "127.0.0.1:*");
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let (code, out, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j]);
    assert_eq!((code, out.as_str()), (Some(0), "\"+hello\""), "{err}");
    let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j]);
    assert_eq!((code, out.as_str()), (Some(0), "\"+hello\""), "{err}");
    assert_eq!(seen.load(Ordering::SeqCst), 1, "the resumed run connected again");
    // The journal holds a hash of the data sent, not the data.
    let text = std::fs::read_to_string(&j).unwrap();
    assert!(text.contains("data_sha256") && !text.contains("ECHO hello"), "{text}");
}

#[test]
fn an_exchange_without_an_answer_is_in_doubt_until_an_operator_says() {
    let (addr, seen) = server(false);
    let dir = scratch("doubt");
    let cfg = config(&dir, "ask", &addr, "127.0.0.1:*");
    let j = dir.join("run.journal").to_string_lossy().into_owned();
    let (code, _, err) = staalc(&["run", PROGRAM, "--config", &cfg, "--journal", &j]);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("may or may not have happened"), "{err}");
    // Resuming does not guess: it stops again without connecting.
    let (code, _, _) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j]);
    assert_eq!(code, Some(3));
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    // A receipt or an HTTP status is not what a TCP exchange gives.
    for wrong in [&["--performed", "r1"][..], &["--status", "200"]] {
        let mut args = vec!["resolve", "--journal", j.as_str()];
        args.extend_from_slice(wrong);
        let (code, _, err) = staalc(&args);
        assert_eq!(code, Some(2), "{wrong:?}");
        assert!(err.contains("--reply <text>"), "{err}");
    }
    let (code, _, err) = staalc(&["resolve", "--journal", &j, "--reply", "+recorded\r\n"]);
    assert_eq!(code, Some(0), "{err}");
    let (code, out, err) = staalc(&["resume", PROGRAM, "--config", &cfg, "--journal", &j]);
    assert_eq!((code, out.as_str()), (Some(0), "\"+recorded\""), "{err}");
    assert_eq!(seen.load(Ordering::SeqCst), 1, "the exchange was made twice");
}
