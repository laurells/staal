// Access tokens and key sets from any text (src/mcp/jwt.rs): refused with a rule or
// verified, never a panic; and nothing but a token the test issuer actually signed and
// that must be accepted (tests/fixtures/jwt/cases.json, "ok": true) verifies against
// the test keys. The fuzzer has no private key, so the signed tokens it can start from
// are those in its corpus (scripts/fuzz_seed.sh seeds them); any change to one, in any
// byte, must make it fail.
#![no_main]
use libfuzzer_sys::fuzz_target;
use serde_json::Value as Json;
use staalc::mcp::jwt::{parse_jwks, verify, Expect, Key};
use std::collections::HashSet;
use std::sync::OnceLock;

const JWKS: &str = include_str!("../../tests/fixtures/jwt/jwks.json");
const CASES: &str = include_str!("../../tests/fixtures/jwt/cases.json");

struct Fixed {
    keys: Vec<Key>,
    issuer: String,
    audience: String,
    now: u64,
    leeway: u64,
    accepted: HashSet<String>,
}

fn fixed() -> &'static Fixed {
    static F: OnceLock<Fixed> = OnceLock::new();
    F.get_or_init(|| {
        let c: Json = serde_json::from_str(CASES).unwrap();
        let text = |k: &str| c[k].as_str().unwrap().to_string();
        let number = |k: &str| c[k].as_u64().unwrap();
        let accepted = c["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["ok"] == Json::Bool(true))
            .map(|x| x["token"].as_str().unwrap().to_string())
            .collect();
        Fixed {
            keys: parse_jwks(&serde_json::from_str(JWKS).unwrap()).unwrap(),
            issuer: text("issuer"),
            audience: text("audience"),
            now: number("now"),
            leeway: number("leeway_s"),
            accepted,
        }
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let f = fixed();
    let e = Expect { issuer: &f.issuer, audience: &f.audience, now: f.now, leeway: f.leeway };
    if verify(s, &f.keys, &e).is_ok() {
        assert!(f.accepted.contains(s), "a token the test issuer did not sign as valid verified");
    }
    if let Ok(j) = serde_json::from_str(s) {
        let _ = parse_jwks(&j);
    }
});
