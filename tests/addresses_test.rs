// The addresses a network capability may reach (RUNTIME.md section 12, "Addresses"):
// public ones, and the private addresses and networks its grant lists; a cloud
// metadata service only when the grant also says so. Checked on the address actually
// connected to, after looking the name up, so a name that leads to a private address
// is refused like the address itself. A listener counts the connections it gets, so
// each refusal is shown to have reached nothing.

use serde_json::{json, Value as Json};
use staalc::interp::{run_config, RunResult};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn run(src: &str, entry: &str, grants: Json, inputs: Json) -> RunResult {
    let p = staalc::check_program(src).unwrap_or_else(|d| panic!("check errors: {d:?}\n{src}"));
    let cfg = json!({ "entry": entry, "inputs": inputs, "grants": grants, "model": { "provider": "script", "responses": [] } });
    run_config(&p, &cfg).unwrap_or_else(|e| panic!("config error: {e}"))
}

fn config_error(src: &str, entry: &str, grants: Json) -> String {
    let p = staalc::check_program(src).unwrap();
    let cfg = json!({ "entry": entry, "inputs": {}, "grants": grants, "model": { "provider": "script", "responses": [] } });
    match run_config(&p, &cfg) {
        Err(e) => e,
        Ok(r) => panic!("the config was accepted: {:?}", r.result),
    }
}

/// A listener on the loopback address that answers every connection with an HTTP 200
/// and counts them: its port and the count.
fn listener() -> (u16, Arc<AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            n2.fetch_add(1, Ordering::SeqCst);
            use std::io::{Read, Write};
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
        }
    });
    (port, n)
}

const GET: &str = r#"fn fetch(http: cap net.http.get, url: Text) -> Int {
    return http.get(url).status
}"#;

fn failure(r: &RunResult) -> String {
    r.result.as_ref().map(|v| panic!("succeeded: {v}")).unwrap_err().message.clone()
}

#[test]
fn a_grant_reaches_no_private_address_unless_it_lists_it() {
    let (port, seen) = listener();
    // By address, and by a name that resolves to it.
    for host in ["127.0.0.1", "localhost"] {
        let url = format!("http://{host}:{port}/");
        let r = run(GET, "fetch", json!({ "http": { "cap": "net.http.get" } }), json!({ "url": url }));
        let m = failure(&r);
        assert!(m.contains("not sent") && m.contains("not a public address") && m.contains(host), "{host}: {m}");
        // Listing another private network does not reach this one.
        let r = run(GET, "fetch", json!({ "http": { "cap": "net.http.get", "private_addresses": ["10.0.0.0/8", "::1"] } }), json!({ "url": url }));
        assert!(failure(&r).contains("not in this grant's private_addresses"), "{host}");
    }
    assert_eq!(seen.load(Ordering::SeqCst), 0, "a refused request reached the listener");
    // Listing the address reaches it.
    let r = run(GET, "fetch", json!({ "http": { "cap": "net.http.get", "private_addresses": ["127.0.0.1"] } }), json!({ "url": format!("http://127.0.0.1:{port}/") }));
    assert_eq!(r.result.unwrap(), json!(200));
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[test]
fn a_cloud_metadata_service_is_refused_even_on_a_listed_network() {
    // Refused before any connection is tried: nothing is listening there in a test.
    // (Plain http:// to them is refused anyway: it is only for loopback.)
    for (url, private) in [
        ("https://169.254.169.254/latest/meta-data/", json!(["169.254.0.0/16"])),
        ("https://[fd00:ec2::254]/latest/meta-data/", json!(["fd00::/8"])),
        ("https://[::ffff:a9fe:a9fe]/", json!(["169.254.0.0/16"])),
    ] {
        let r = run(GET, "fetch", json!({ "http": { "cap": "net.http.get", "private_addresses": private } }), json!({ "url": url }));
        let m = failure(&r);
        assert!(m.contains("not sent") && m.contains("cloud metadata"), "{url}: {m}");
    }
}

#[test]
fn tcp_reaches_no_private_address_unless_its_grant_lists_it() {
    let (port, seen) = listener();
    let src = r#"fn ask(tcp: cap net.tcp.exchange, address: Text) -> Text {
    return tcp.exchange(address, "GET / HTTP/1.0\r\n\r\n")
}"#;
    for address in [format!("127.0.0.1:{port}"), format!("localhost:{port}")] {
        let r = run(src, "ask", json!({ "tcp": { "cap": "net.tcp.exchange" } }), json!({ "address": address }));
        let m = failure(&r);
        assert!(m.contains("not a public address"), "{address}: {m}");
    }
    assert_eq!(seen.load(Ordering::SeqCst), 0, "a refused exchange reached the listener");
    let r = run(src, "ask", json!({ "tcp": { "cap": "net.tcp.exchange", "private_addresses": ["127.0.0.0/8"] } }), json!({ "address": format!("127.0.0.1:{port}") }));
    assert!(r.result.unwrap().as_str().unwrap().starts_with("HTTP/1.1 200"));
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[test]
fn address_settings_are_checked_when_the_run_starts() {
    for (g, why) in [
        (json!({ "cap": "net.http.get", "private_addresses": "127.0.0.1" }), "expected a list"),
        (json!({ "cap": "net.http.get", "private_addresses": ["10.0.0.1/8"] }), "the network is 10.0.0.0/8"),
        (json!({ "cap": "net.http.get", "private_addresses": ["localhost"] }), "not an IP address"),
        (json!({ "cap": "net.http.get", "private_addresses": ["::ffff:127.0.0.1"] }), "write an IPv4 network as IPv4"),
        (json!({ "cap": "net.http.get", "private_addresses": ["127.0.0.1", "127.0.0.1/32"] }), "listed twice"),
        (json!({ "cap": "net.http.get", "cloud_metadata": "yes" }), "expected true or false"),
    ] {
        let e = config_error(GET, "fetch", json!({ "http": g }));
        assert!(e.contains("grants.http") && e.contains(why), "{g}: {e}");
    }
    // Only a capability that connects to an address a program chooses takes them.
    let src = r#"fn read(f: cap fs.read, path: Text) -> Text {
    return f.read(path)
}"#;
    let e = config_error(src, "read", json!({ "f": { "cap": "fs.read", "private_addresses": ["127.0.0.1"] } }));
    assert!(e.contains("unknown key 'private_addresses'"), "{e}");
}
