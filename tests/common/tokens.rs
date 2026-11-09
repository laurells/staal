// A test token issuer: an ES256 key made with ring, its public JWK, and access tokens
// signed with it as an authorization server would sign them (RFC 9068).

use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde_json::{json, Value as Json};

pub struct Issuer {
    key: EcdsaKeyPair,
    pub kid: String,
}

fn b64u(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

impl Issuer {
    pub fn new(kid: &str) -> Issuer {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        Issuer { key: EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap(), kid: kid.to_string() }
    }

    /// The public key as a JWK.
    pub fn jwk(&self) -> Json {
        let p = self.key.public_key().as_ref(); // 0x04 || x || y
        json!({ "kty": "EC", "crv": "P-256", "kid": self.kid, "use": "sig", "x": b64u(&p[1..33]), "y": b64u(&p[33..65]) })
    }

    /// A token with `claims`, its header `typ: at+jwt`, `alg: ES256` and this key's kid,
    /// with `header` keys added or (null) removed.
    pub fn token(&self, claims: &Json, header: &Json) -> String {
        let mut h = json!({ "typ": "at+jwt", "alg": "ES256", "kid": self.kid });
        for (k, v) in header.as_object().unwrap() {
            if v.is_null() {
                h.as_object_mut().unwrap().remove(k);
            } else {
                h[k] = v.clone();
            }
        }
        let input = format!("{}.{}", b64u(h.to_string().as_bytes()), b64u(claims.to_string().as_bytes()));
        let sig = self.key.sign(&SystemRandom::new(), input.as_bytes()).unwrap();
        format!("{input}.{}", b64u(sig.as_ref()))
    }
}

/// Claims valid now for `iss` and `aud`, for subject `sub` with `scope`; `over` keys
/// added or (null) removed.
pub fn claims(iss: &str, aud: &str, sub: &str, scope: &str, over: &Json) -> Json {
    let t = now();
    let mut c = json!({ "iss": iss, "aud": aud, "sub": sub, "client_id": "test-client", "jti": format!("j-{t}-{sub}"),
                        "iat": t - 10, "exp": t + 600, "scope": scope });
    for (k, v) in over.as_object().unwrap() {
        if v.is_null() {
            c.as_object_mut().unwrap().remove(k);
        } else {
            c[k] = v.clone();
        }
    }
    c
}
