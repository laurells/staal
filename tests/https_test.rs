// net.http over real TLS: a local HTTPS server (rustls) with certificates made here.
// A certificate the client must not trust is refused during the handshake, before any
// request is sent: the server records nothing, and the failure is an ordinary one
// (never "in doubt"), so even a POST is safe to report and retry.

mod common;

use common::server::Server;
use common::tls::{self_signed, Ca};
use intentc::interp::{run_config, FailKind, RunResult};
use serde_json::{json, Value as Json};
use std::path::PathBuf;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("intentc-https-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_ca(d: &std::path::Path, ca: &Ca) -> String {
    let p = d.join("ca.pem");
    std::fs::write(&p, ca.pem()).unwrap();
    p.to_str().unwrap().to_string()
}

/// POSTs to the server under `try` and reports whether it succeeded.
fn post(server: &Server, roots: Option<&str>) -> RunResult {
    let src = format!(
        r#"fn f(http: cap net.http.post(only: ["{0}"])) -> Bool {{
    let r = try http.post("{0}", "hello")
    return r.ok
}}"#,
        server.url("/ok")
    );
    let prog = intentc::check_program(&src).unwrap();
    let mut cfg = json!({ "entry": "f", "grants": { "http": { "cap": "net.http.post" } },
                          "model": { "provider": "script", "responses": [] } });
    if let Some(r) = roots {
        cfg["net"] = json!({ "extra_root_certificates": [r] });
    }
    run_config(&prog, &cfg).unwrap()
}

#[test]
fn a_certificate_from_a_trusted_authority_works() {
    let d = dir("trusted");
    let ca = Ca::new("Intent Test CA");
    let s = Server::start_tls(ca.server(&["localhost"], None));
    let out = post(&s, Some(&write_ca(&d, &ca)));
    assert_eq!(out.result.unwrap(), json!(true));
    let reqs = s.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].body, "hello");
}

/// Each case: the server's certificate, and the authority the client is told to trust.
fn refused(server: Server, roots: Option<&str>, label: &str) {
    let out = post(&server, roots);
    // Caught by `try`: an ordinary failure, not in doubt, because nothing was sent.
    assert_eq!(out.result.as_ref().unwrap(), &json!(false), "{label}: {:?}", out.result);
    let failed = out.log.iter().find(|e| e["event"] == "action_failed").expect("an action_failed event");
    assert!(failed["error"].as_str().unwrap().starts_with("not sent"), "{label}: {}", failed["error"]);
    assert!(server.requests().is_empty(), "{label}: a request reached the server");
}

#[test]
fn an_unknown_authority_is_refused_before_anything_is_sent() {
    let ca = Ca::new("Some Other CA");
    refused(Server::start_tls(ca.server(&["localhost"], None)), None, "unknown authority");
}

#[test]
fn a_self_signed_certificate_is_refused() {
    refused(Server::start_tls(self_signed(&["localhost"])), None, "self-signed");
}

#[test]
fn a_certificate_for_another_name_is_refused() {
    let d = dir("name");
    let ca = Ca::new("Intent Test CA");
    let roots = write_ca(&d, &ca);
    refused(Server::start_tls(ca.server(&["api.example.com"], None)), Some(&roots), "wrong name");
}

#[test]
fn an_expired_certificate_is_refused() {
    let d = dir("expired");
    let ca = Ca::new("Intent Test CA");
    let roots = write_ca(&d, &ca);
    refused(Server::start_tls(ca.server(&["localhost"], Some((2020, 2021)))), Some(&roots), "expired");
}

#[test]
fn a_certificate_not_yet_valid_is_refused() {
    let d = dir("future");
    let ca = Ca::new("Intent Test CA");
    let roots = write_ca(&d, &ca);
    refused(Server::start_tls(ca.server(&["localhost"], Some((2090, 2091)))), Some(&roots), "not yet valid");
}

#[test]
fn trusting_one_authority_does_not_trust_another() {
    let d = dir("other");
    let trusted = Ca::new("Trusted CA");
    let other = Ca::new("Other CA");
    let roots = write_ca(&d, &trusted);
    refused(Server::start_tls(other.server(&["localhost"], None)), Some(&roots), "other authority");
}

#[test]
fn root_certificate_files_are_checked() {
    let d = dir("files");
    let src = "fn f(http: cap net.http.get) -> Int {\n    return 1\n}";
    let prog = intentc::check_program(src).unwrap();
    let err = |roots: Json| -> String {
        let cfg = json!({ "entry": "f", "grants": { "http": { "cap": "net.http.get" } },
                          "model": { "provider": "script", "responses": [] },
                          "net": { "extra_root_certificates": roots } });
        match run_config(&prog, &cfg) {
            Ok(_) => panic!("expected a config error"),
            Err(e) => e,
        }
    };
    assert!(err(json!([d.join("missing.pem").to_str().unwrap()])).contains("cannot read"));
    let empty = d.join("empty.pem");
    std::fs::write(&empty, "not a certificate").unwrap();
    assert!(err(json!([empty.to_str().unwrap()])).contains("no certificate"));
    let with_key = d.join("key.pem");
    let kp = rcgen::KeyPair::generate().unwrap();
    std::fs::write(&with_key, format!("{}{}", Ca::new("x").pem(), kp.serialize_pem())).unwrap();
    assert!(err(json!([with_key.to_str().unwrap()])).contains("private key"));
    assert!(err(json!("one.pem")).contains("list"));
}

#[test]
fn a_failed_handshake_is_never_in_doubt_even_without_try() {
    // Without `try` the failure ends the run, and its kind must be `action`, not
    // `in_doubt`: an operator is never asked about a request that could not be sent.
    let s = Server::start_tls(self_signed(&["localhost"]));
    let src = format!(
        r#"fn f(http: cap net.http.post(only: ["{0}"])) -> Int {{
    let r = http.post("{0}", "x")
    return r.status
}}"#,
        s.url("/ok")
    );
    let prog = intentc::check_program(&src).unwrap();
    let cfg = json!({ "entry": "f", "grants": { "http": { "cap": "net.http.post" } },
                      "model": { "provider": "script", "responses": [] } });
    let f = run_config(&prog, &cfg).unwrap().result.unwrap_err();
    assert_eq!(f.kind, FailKind::Action, "{}", f.message);
}

#[test]
fn after_the_handshake_a_lost_answer_is_still_in_doubt() {
    // The server's certificate is good and the request is sent; then no answer comes.
    // That is "may have happened", over TLS as over plain HTTP.
    let d = dir("hang");
    let ca = Ca::new("Intent Test CA");
    let roots = write_ca(&d, &ca);
    let s = Server::start_tls(ca.server(&["localhost"], None));
    let src = format!(
        r#"fn f(http: cap net.http.post(only: ["{0}"])) -> Bool {{
    let r = try http.post("{0}", "pay")
    return r.ok
}}"#,
        s.url("/hang")
    );
    let prog = intentc::check_program(&src).unwrap();
    let cfg = json!({ "entry": "f", "grants": { "http": { "cap": "net.http.post" } },
                      "model": { "provider": "script", "responses": [] },
                      "net": { "extra_root_certificates": [roots], "timeout_ms": 500 } });
    let f = run_config(&prog, &cfg).unwrap().result.unwrap_err();
    assert_eq!(f.kind, FailKind::InDoubt, "{}", f.message);
    assert_eq!(s.requests().len(), 1, "the request reached the server");
}

#[test]
fn a_tls_error_after_the_request_was_sent_is_in_doubt() {
    // Only handshake errors mean "not sent". Here the handshake succeeds, the request
    // goes out, and then the server sends bytes that are not TLS: a TLS error that
    // comes after sending, so the request may have been acted on.
    let d = dir("garbage");
    let ca = Ca::new("Intent Test CA");
    let roots = write_ca(&d, &ca);
    let s = Server::start_tls(ca.server(&["localhost"], None));
    let src = format!(
        r#"fn f(http: cap net.http.post(only: ["{0}"])) -> Bool {{
    let r = try http.post("{0}", "pay")
    return r.ok
}}"#,
        s.url("/garbage")
    );
    let prog = intentc::check_program(&src).unwrap();
    let cfg = json!({ "entry": "f", "grants": { "http": { "cap": "net.http.post" } },
                      "model": { "provider": "script", "responses": [] },
                      "net": { "extra_root_certificates": [roots], "timeout_ms": 2000 } });
    let f = run_config(&prog, &cfg).unwrap().result.unwrap_err();
    assert_eq!(f.kind, FailKind::InDoubt, "{}", f.message);
    assert_eq!(s.requests().len(), 1, "the request reached the server");
}
