// Access tokens and key sets from any text (src/mcp/jwt.rs): refused with a rule or
// verified, never a panic; and nothing made up verifies against the test keys.
#![no_main]
use libfuzzer_sys::fuzz_target;
use staalc::mcp::jwt::{parse_jwks, verify, Expect};

const JWKS: &str = include_str!("../../tests/fixtures/jwt/jwks.json");

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let keys = parse_jwks(&serde_json::from_str(JWKS).unwrap()).unwrap();
    let e = Expect { issuer: "https://auth.test.example", audience: "https://mcp.test.example/mcp", now: 1893456000, leeway: 60 };
    // Without the test keys' private halves, no input can carry a valid signature.
    assert!(verify(s, &keys, &e).is_err(), "a made-up token verified");
    if let Ok(j) = serde_json::from_str(s) {
        let _ = parse_jwks(&j);
    }
});
