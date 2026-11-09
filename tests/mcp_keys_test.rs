// Finding and fetching the authorization server's keys (src/mcp/keys.rs, docs/MCP.md
// section 12.5), against a fake authorization server over HTTPS: discovery in the
// specification's order, the issuer check, and every hardening rule of a fetch.

mod common;

use common::authserver::{AuthServer, Route};
use common::tls::Ca;
use serde_json::json;
use staalc::mcp::keys::{discover, fetch_keys, Fetcher, CONNECT_TIME, FETCH_TIME, MAX_DOCUMENT_BYTES};
use std::time::Duration;

fn jwks() -> serde_json::Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jwt/jwks.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// A trusted authorization server, and a fetcher trusting its authority.
fn setup() -> (Ca, AuthServer, Fetcher) {
    let ca = Ca::new("Auth Test CA");
    let s = AuthServer::start(ca.server(&["localhost"], None));
    let roots = vec![ca_der(&ca)];
    (ca, s, Fetcher::new(&roots, CONNECT_TIME, FETCH_TIME))
}

fn ca_der(ca: &Ca) -> Vec<u8> {
    use rustls::pki_types::pem::PemObject;
    rustls::pki_types::CertificateDer::from_pem_slice(ca.pem().as_bytes()).unwrap().to_vec()
}

#[test]
fn keys_are_found_from_the_metadata_and_fetched() {
    let (_ca, s, f) = setup();
    let jwks_uri = format!("{}/jwks", s.base);
    s.set("/.well-known/oauth-authorization-server", Route::json(&json!({ "issuer": s.base, "jwks_uri": jwks_uri, "other": [1, 2] })));
    s.set("/jwks", Route::json(&jwks()));
    assert_eq!(discover(&f, &s.base).unwrap(), jwks_uri);
    assert_eq!(fetch_keys(&f, &jwks_uri).unwrap().len(), 5);
}

#[test]
fn discovery_tries_each_location_in_order() {
    let (_ca, s, f) = setup();
    let issuer = format!("{}/tenant1", s.base);
    let jwks_uri = format!("{}/keys", s.base);
    // Only the last location answers: OpenID Connect with the path appended.
    s.set("/tenant1/.well-known/openid-configuration", Route::json(&json!({ "issuer": issuer, "jwks_uri": jwks_uri })));
    assert_eq!(discover(&f, &issuer).unwrap(), jwks_uri);
    assert_eq!(s.hits("/.well-known/oauth-authorization-server/tenant1"), 1);
    assert_eq!(s.hits("/.well-known/openid-configuration/tenant1"), 1);
    // An earlier location answering decides: one naming another issuer is refused, and
    // the later one is not tried.
    s.set("/.well-known/openid-configuration/tenant1", Route::json(&json!({ "issuer": s.base, "jwks_uri": jwks_uri })));
    let e = discover(&f, &issuer).unwrap_err();
    assert!(e.contains("is not") && e.contains("exactly"), "{e}");
    assert_eq!(s.hits("/tenant1/.well-known/openid-configuration"), 1);
}

#[test]
fn metadata_that_cannot_be_trusted_is_refused() {
    let (_ca, s, f) = setup();
    let at = "/.well-known/oauth-authorization-server";
    let refused = |r: Route, want: &str| {
        s.set(at, r);
        let e = discover(&f, &s.base).unwrap_err();
        assert!(e.contains(want), "expected {want:?} in {e}");
    };
    refused(Route::json(&json!({ "issuer": format!("{}/", s.base), "jwks_uri": "https://x.example/k" })), "exactly");
    refused(Route::json(&json!({ "jwks_uri": "https://x.example/k" })), "no issuer");
    refused(Route::json(&json!({ "issuer": s.base })), "no jwks_uri");
    refused(Route::json(&json!({ "issuer": s.base, "jwks_uri": "http://x.example/k" })), "https URL without a fragment");
    refused(Route::json(&json!({ "issuer": s.base, "jwks_uri": "https://x.example/k#f" })), "https URL without a fragment");
    refused(Route::json(&json!({ "issuer": s.base, "jwks_uri": 7 })), "no jwks_uri");
    refused(Route::json(&json!([1])), "not a JSON object");
    let mut redirect = Route::status(302);
    redirect.headers.push(("Location".into(), "https://evil.example/".into()));
    refused(redirect, "redirect");
    refused(Route::status(500), "answered 500");
    refused(Route::raw(b"{not json".to_vec()), "not JSON");
    refused(Route::raw(format!("{}{}", "[".repeat(17), "]".repeat(17)).into_bytes()), "nested deeper than 16");
    let mut big = b"{\"issuer\": \"".to_vec();
    big.resize(MAX_DOCUMENT_BYTES as usize + 10, b'a');
    refused(Route::raw(big), "within 262144 bytes");
    // Nothing found anywhere.
    let (_c2, s2, f2) = setup();
    assert!(discover(&f2, &s2.base).unwrap_err().contains("no authorization server metadata"));
}

#[test]
fn key_sets_that_cannot_be_used_are_refused() {
    let (_ca, s, f) = setup();
    let uri = format!("{}/jwks", s.base);
    let mut k = jwks();
    let first = k["keys"][0].clone();
    k["keys"].as_array_mut().unwrap().push(first);
    s.set("/jwks", Route::json(&k));
    assert!(fetch_keys(&f, &uri).unwrap_err().contains("two keys with kid"));
    s.set("/jwks", Route::json(&json!({ "keys": [{ "kty": "oct", "kid": "o", "k": "AA" }] })));
    assert!(fetch_keys(&f, &uri).unwrap_err().contains("no key this server can use"));
    s.set("/jwks", Route::status(404));
    assert!(fetch_keys(&f, &uri).unwrap_err().contains("404"));
}

#[test]
fn a_fetch_is_https_to_a_trusted_server_within_its_time() {
    let (ca, s, _f) = setup();
    s.set("/jwks", Route::json(&jwks()));
    // Not trusted: a fetcher with only Mozilla's roots.
    let plain = Fetcher::new(&[], CONNECT_TIME, FETCH_TIME);
    assert!(fetch_keys(&plain, &format!("{}/jwks", s.base)).is_err());
    // Another host's certificate.
    let other = AuthServer::start(ca.server(&["auth.other.example"], None));
    other.set("/jwks", Route::json(&jwks()));
    let f = Fetcher::new(&[ca_der(&ca)], CONNECT_TIME, FETCH_TIME);
    assert!(fetch_keys(&f, &format!("{}/jwks", other.base)).is_err());
    // http is never fetched.
    assert!(f.get("http://localhost:1/jwks").unwrap_err().contains("only https"));
    // Too slow: the whole fetch must finish in its time (one second here).
    let mut slow = Route::json(&jwks());
    slow.delay = Duration::from_millis(1500);
    s.set("/slow", slow);
    let quick = Fetcher::new(&[ca_der(&ca)], CONNECT_TIME, Duration::from_secs(1));
    let start = std::time::Instant::now();
    assert!(fetch_keys(&quick, &format!("{}/slow", s.base)).is_err());
    assert!(start.elapsed() < Duration::from_millis(1400), "{:?}", start.elapsed());
}
