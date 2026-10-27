// net.tls.exchange over real TLS: a local server (rustls) with certificates made here.
// A certificate the client must not trust is refused during the handshake, before any
// data is sent, and a reply the peer did not end properly is never taken as whole.

mod common;

use common::tls::{self_signed, Ca};
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A one-line TLS server: answers `ECHO w` with `+w\r\n`, then ends TLS properly when
/// `close_notify`, or just closes the connection. Counts the lines it received.
fn server(cfg: Arc<rustls::ServerConfig>, close_notify: bool) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let lines = Arc::new(AtomicUsize::new(0));
    let seen = lines.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(c) = c else { continue };
            let Ok(conn) = rustls::ServerConnection::new(cfg.clone()) else { continue };
            let mut tls = rustls::StreamOwned::new(conn, c);
            let mut line = String::new();
            if BufReader::new(&mut tls).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            seen.fetch_add(1, Ordering::SeqCst);
            let w = line.trim().strip_prefix("ECHO ").unwrap_or("?").to_string();
            let _ = tls.write_all(format!("+{w}\r\n").as_bytes());
            if close_notify {
                tls.conn.send_close_notify();
            }
            let _ = tls.flush();
            // End the connection with a plain FIN, never a reset: closing a socket with
            // unread data (the client's own close_notify, sent after its request)
            // makes the kernel send RST, which the client would rightly report as a
            // broken connection, and which can discard the reply it has not read yet.
            let _ = tls.sock.shutdown(std::net::Shutdown::Write);
            let _ = tls.sock.set_read_timeout(Some(std::time::Duration::from_secs(3)));
            let mut sink = [0u8; 4096];
            while matches!(std::io::Read::read(&mut tls.sock, &mut sink), Ok(n) if n > 0) {}
        }
    });
    (format!("localhost:{port}"), lines)
}

fn exchange(address: &str, roots: Option<&str>, until: bool) -> staalc::interp::RunResult {
    let call = if until { r#"conn.exchange(address, "ECHO hi\r\n", "\r\n")"# } else { r#"conn.exchange(address, "ECHO hi\r\n")"# };
    let src = format!("fn f(conn: cap net.tls.exchange, address: Text) -> Text {{\n    let r = try {call}\n    if r.ok {{\n        return \"ok\"\n    }}\n    return r.error\n}}\n");
    let prog = staalc::check_program(&src).unwrap();
    let mut cfg = json!({ "entry": "f", "inputs": { "address": address },
                          "grants": { "conn": { "cap": "net.tls.exchange", "only": ["localhost:*"] } },
                          "model": { "provider": "script", "responses": [] }, "net": { "timeout_ms": 3000 } });
    if let Some(r) = roots {
        cfg["net"]["extra_root_certificates"] = json!([r]);
    }
    staalc::interp::run_config(&prog, &cfg).unwrap()
}

fn reply(address: &str, roots: Option<&str>, until: bool) -> Result<String, String> {
    // The program gives back "ok" for a reply, or the failure.
    match exchange(address, roots, until).result {
        Ok(v) if v == json!("ok") => Ok("ok".into()),
        Ok(v) => Err(v.as_str().unwrap_or_default().to_string()),
        Err(f) => Err(format!("{}: {}", f.kind, f.message)),
    }
}

fn ca_file(ca: &Ca, name: &str) -> String {
    let p = std::env::temp_dir().join(format!("staalc-tls-{name}-{}.pem", std::process::id()));
    std::fs::write(&p, ca.pem()).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn a_certificate_from_a_trusted_authority_works_with_or_without_a_delimiter() {
    let ca = Ca::new("Staal Test CA");
    let roots = ca_file(&ca, "trusted");
    let (addr, lines) = server(ca.server(&["localhost"], None), true);
    assert_eq!(reply(&addr, Some(&roots), true), Ok("ok".into()));
    assert_eq!(reply(&addr, Some(&roots), false), Ok("ok".into()));
    assert_eq!(lines.load(Ordering::SeqCst), 2);
}

#[test]
fn an_untrusted_certificate_is_refused_before_anything_is_sent() {
    let ca = Ca::new("Staal Test CA");
    let roots = ca_file(&ca, "untrusted");
    for (cfg, given, label) in [
        (Ca::new("Some Other CA").server(&["localhost"], None), Some(roots.as_str()), "unknown authority"),
        (self_signed(&["localhost"]), None, "self-signed"),
        (ca.server(&["example.com"], None), Some(roots.as_str()), "another name"),
        (ca.server(&["localhost"], Some((2000, 2001))), Some(roots.as_str()), "expired"),
    ] {
        let (addr, lines) = server(cfg, true);
        let got = reply(&addr, given, true).unwrap_err();
        assert!(got.starts_with("action: not sent: the TLS handshake"), "{label}: {got}");
        assert_eq!(lines.load(Ordering::SeqCst), 0, "{label}: data reached the server");
    }
}

#[test]
fn a_reply_that_does_not_end_tls_is_never_taken_as_whole() {
    let ca = Ca::new("Staal Test CA");
    let roots = ca_file(&ca, "cut");
    let (addr, _) = server(ca.server(&["localhost"], None), false);
    let got = reply(&addr, Some(&roots), false).unwrap_err();
    assert!(got.contains("without ending TLS"), "{got}");
    // With a delimiter, the reply ends at it, before the close.
    assert_eq!(reply(&addr, Some(&roots), true), Ok("ok".into()));
}
