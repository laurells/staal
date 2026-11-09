// Access tokens for the HTTP transport (docs/MCP.md section 12.5): JWTs as RFC 9068
// profiles them, verified here under a strict profile, with signatures checked by ring.
//
// Accepted: a compact JWS (three base64url parts, no padding) whose header has `typ`
// `at+jwt` (or `application/at+jwt`), `alg` one of RS256, PS256, ES256 or EdDSA
// (Ed25519), no `crit`, and a `kid` naming a key of the authorization server's set
// whose type fits the algorithm. Nothing in the header is used to find a key but `kid`:
// `jwk`, `jku`, `x5u` and `x5c` are ignored. The claims must name the expected issuer
// and audience, be within their validity (with a leeway), and carry `sub`, `client_id`
// and `jti`, as RFC 9068 section 2.2 requires.
//
// Each refusal says which rule it broke, in words a log can hold; never the token.

use base64::Engine;
use serde_json::{Map, Value as Json};

/// The most keys a set may hold (docs/MCP.md section 12.5).
pub const MAX_KEYS: usize = 64;

/// A key from the authorization server's set that a token may name.
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub kid: String,
    /// The key's `alg`, if the set gives one: then only that algorithm may use it.
    alg: Option<Alg>,
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    /// Modulus and exponent, big-endian, without leading zeros.
    Rsa { n: Vec<u8>, e: Vec<u8> },
    /// The uncompressed point, 0x04 || x || y.
    P256(Vec<u8>),
    Ed25519(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Alg {
    Rs256,
    Ps256,
    Es256,
    EdDsa,
}

impl Alg {
    fn from(name: &str) -> Option<Alg> {
        match name {
            "RS256" => Some(Alg::Rs256),
            "PS256" => Some(Alg::Ps256),
            "ES256" => Some(Alg::Es256),
            "EdDSA" => Some(Alg::EdDsa),
            _ => None,
        }
    }

    fn fits(self, k: &Kind) -> bool {
        matches!((self, k), (Alg::Rs256 | Alg::Ps256, Kind::Rsa { .. }) | (Alg::Es256, Kind::P256(_)) | (Alg::EdDsa, Kind::Ed25519(_)))
    }
}

/// Why a token was refused: the rule (`format`, `typ`, `alg`, `crit`, `kid`, `key`,
/// `signature`, then the claim) and a sentence about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub rule: &'static str,
    pub detail: String,
}

fn refuse<T>(rule: &'static str, detail: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal { rule, detail: detail.into() })
}

/// What a token must say.
pub struct Expect<'a> {
    pub issuer: &'a str,
    pub audience: &'a str,
    /// Seconds since the Unix epoch.
    pub now: u64,
    /// The clock difference allowed on `exp`, `nbf` and `iat`, in seconds.
    pub leeway: u64,
}

/// A token that passed: its subject and the rest of what the server uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    pub iss: String,
    pub sub: String,
    pub client_id: String,
    /// The `scope` claim's scopes; empty when it has none.
    pub scopes: Vec<String>,
    /// Every claim, for app inputs taken from claims.
    pub claims: Map<String, Json>,
}

fn b64u(part: &str) -> Option<Vec<u8>> {
    // Strict: the URL-safe alphabet, no padding, no stray bits in the last character.
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(part).ok()
}

/// Reads a JWK set (RFC 7517) as docs/MCP.md section 12.5 says: an object whose `keys`
/// is an array of at most MAX_KEYS objects. A key this server does not use (another
/// `kty` or curve, a `use` other than `sig`, an `alg` it does not accept, no `kid`, or
/// malformed) is left out; a set with two usable keys of one `kid`, or none, is refused.
pub fn parse_jwks(j: &Json) -> Result<Vec<Key>, String> {
    let keys = j.as_object().and_then(|o| o.get("keys")).and_then(Json::as_array).ok_or("a key set must be an object whose \"keys\" is an array")?;
    if keys.len() > MAX_KEYS {
        return Err(format!("the key set holds {} keys; at most {MAX_KEYS} are read", keys.len()));
    }
    let mut out: Vec<Key> = Vec::new();
    for k in keys {
        let Some(k) = k.as_object() else { return Err("each key in a key set must be an object".into()) };
        let Some(key) = usable(k) else { continue };
        if out.iter().any(|o| o.kid == key.kid) {
            return Err(format!("the key set has two keys with kid '{}'; a kid must name one key", crate::clip::clip(&key.kid)));
        }
        out.push(key);
    }
    if out.is_empty() {
        return Err("the key set has no key this server can use (RSA, P-256 or Ed25519 signing keys with a kid)".into());
    }
    Ok(out)
}

fn usable(k: &Map<String, Json>) -> Option<Key> {
    let s = |name: &str| k.get(name).and_then(Json::as_str);
    let kid = s("kid").filter(|id| !id.is_empty())?.to_string();
    if k.get("use").is_some_and(|u| u.as_str() != Some("sig")) {
        return None;
    }
    let alg = match k.get("alg") {
        None => None,
        Some(a) => Some(Alg::from(a.as_str()?)?),
    };
    let kind = match s("kty")? {
        "RSA" => {
            let (n, e) = (b64u(s("n")?)?, b64u(s("e")?)?);
            // Minimal big-endian integers, as JWA requires.
            if n.first().is_none_or(|b| *b == 0) || e.first().is_none_or(|b| *b == 0) {
                return None;
            }
            Kind::Rsa { n, e }
        }
        "EC" if s("crv")? == "P-256" => {
            let (x, y) = (b64u(s("x")?)?, b64u(s("y")?)?);
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            let mut p = vec![0x04];
            p.extend_from_slice(&x);
            p.extend_from_slice(&y);
            Kind::P256(p)
        }
        "OKP" if s("crv")? == "Ed25519" => {
            let x = b64u(s("x")?)?;
            if x.len() != 32 {
                return None;
            }
            Kind::Ed25519(x)
        }
        _ => return None,
    };
    if alg.is_some_and(|a| !a.fits(&kind)) {
        return None;
    }
    Some(Key { kid, alg, kind })
}

/// Bits in a big-endian integer without leading zeros.
fn bits(n: &[u8]) -> usize {
    n.first().map_or(0, |b| (n.len() - 1) * 8 + (8 - b.leading_zeros() as usize))
}

/// Verifies `token` against `keys` and what `e` expects.
pub fn verify(token: &str, keys: &[Key], e: &Expect) -> Result<Verified, Refusal> {
    let parts: Vec<&str> = token.split('.').collect();
    let [h64, c64, s64] = parts[..] else { return refuse("format", "a token must be three base64url parts separated by dots") };
    let (Some(hb), Some(cb), Some(sig)) = (b64u(h64), b64u(c64), b64u(s64)) else {
        return refuse("format", "a part of the token is not base64url without padding");
    };
    let Ok(Json::Object(header)) = serde_json::from_slice::<Json>(&hb) else { return refuse("format", "the token's header is not a JSON object") };

    let typ = header.get("typ").and_then(Json::as_str).map(str::to_ascii_lowercase);
    if !matches!(typ.as_deref(), Some("at+jwt" | "application/at+jwt")) {
        return refuse("typ", "the token's typ is not at+jwt (RFC 9068): it is not an access token");
    }
    let Some(alg) = header.get("alg").and_then(Json::as_str).and_then(Alg::from) else {
        return refuse("alg", "the token's alg is not RS256, PS256, ES256 or EdDSA");
    };
    if header.contains_key("crit") {
        return refuse("crit", "the token's header has crit, naming extensions this server does not implement");
    }
    let Some(kid) = header.get("kid").and_then(Json::as_str) else { return refuse("kid", "the token's header names no kid") };
    let Some(key) = keys.iter().find(|k| k.kid == kid) else {
        return refuse("kid", format!("no key of the authorization server has kid '{}'", crate::clip::clip(kid)));
    };
    if !alg.fits(&key.kind) || key.alg.is_some_and(|a| a != alg) {
        return refuse("alg", format!("key '{}' is not for the token's alg", crate::clip::clip(kid)));
    }
    if let Kind::Rsa { n, .. } = &key.kind
        && bits(n) < 2048
    {
        return refuse("key", format!("key '{}' is an RSA key under 2048 bits", crate::clip::clip(kid)));
    }

    // The signature, over the two first parts exactly as sent.
    let signed = &token.as_bytes()[..h64.len() + 1 + c64.len()];
    use ring::signature as sg;
    let ok = match (&key.kind, alg) {
        (Kind::Rsa { n, e }, Alg::Rs256) => sg::RsaPublicKeyComponents { n, e }.verify(&sg::RSA_PKCS1_2048_8192_SHA256, signed, &sig),
        (Kind::Rsa { n, e }, Alg::Ps256) => sg::RsaPublicKeyComponents { n, e }.verify(&sg::RSA_PSS_2048_8192_SHA256, signed, &sig),
        (Kind::P256(p), Alg::Es256) => sg::UnparsedPublicKey::new(&sg::ECDSA_P256_SHA256_FIXED, p).verify(signed, &sig),
        (Kind::Ed25519(x), Alg::EdDsa) => sg::UnparsedPublicKey::new(&sg::ED25519, x).verify(signed, &sig),
        _ => return refuse("alg", "the key does not fit the algorithm"),
    };
    if ok.is_err() {
        return refuse("signature", "the token's signature does not verify");
    }

    let Ok(Json::Object(claims)) = serde_json::from_slice::<Json>(&cb) else { return refuse("format", "the token's claims are not a JSON object") };
    let text = |name: &'static str| -> Result<String, Refusal> {
        match claims.get(name) {
            Some(Json::String(s)) if !s.is_empty() => Ok(s.clone()),
            Some(_) => refuse(name, format!("the token's {name} is not a string")),
            None => refuse(name, format!("the token has no {name}")),
        }
    };
    let time = |name: &'static str| -> Result<Option<f64>, Refusal> {
        match claims.get(name) {
            None => Ok(None),
            Some(v) => v.as_f64().filter(|t| t.is_finite()).map(Some).ok_or(Refusal { rule: name, detail: format!("the token's {name} is not a number") }),
        }
    };
    let iss = text("iss")?;
    // Simple string comparison (RFC 3986 section 6.2.1): no normalisation.
    if iss != e.issuer {
        return refuse("iss", "the token was issued by another authorization server");
    }
    let aud_ok = match claims.get("aud") {
        Some(Json::String(a)) => a == e.audience,
        Some(Json::Array(a)) => a.iter().any(|x| x.as_str() == Some(e.audience)),
        Some(_) => return refuse("aud", "the token's aud is not a string or a list of strings"),
        None => return refuse("aud", "the token has no aud"),
    };
    if !aud_ok {
        return refuse("aud", "the token is not for this server (its aud does not name this server's resource)");
    }
    let (now, leeway) = (e.now as f64, e.leeway as f64);
    let Some(exp) = time("exp")? else { return refuse("exp", "the token has no exp") };
    if now > exp + leeway {
        return refuse("exp", "the token has expired");
    }
    if let Some(nbf) = time("nbf")?
        && now + leeway < nbf
    {
        return refuse("nbf", "the token is not valid yet");
    }
    let Some(iat) = time("iat")? else { return refuse("iat", "the token has no iat") };
    if iat > now + leeway {
        return refuse("iat", "the token was issued in the future");
    }
    let sub = text("sub")?;
    let client_id = text("client_id")?;
    text("jti")?;
    let scopes = match claims.get("scope") {
        None => Vec::new(),
        Some(Json::String(s)) => s.split(' ').filter(|x| !x.is_empty()).map(str::to_string).collect(),
        Some(_) => return refuse("scope", "the token's scope is not a string"),
    };
    Ok(Verified { iss, sub, client_id, scopes, claims })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(name: &str) -> Json {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jwt").join(name);
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    /// Every token PyJWT made (scripts/jwt_fixtures.py) is judged as the case says.
    #[test]
    fn tokens_made_by_an_independent_library_are_judged_as_expected() {
        let keys = parse_jwks(&fixture("jwks.json")).unwrap();
        let c = fixture("cases.json");
        let e = Expect { issuer: c["issuer"].as_str().unwrap(), audience: c["audience"].as_str().unwrap(), now: c["now"].as_u64().unwrap(), leeway: c["leeway_s"].as_u64().unwrap() };
        let cases = c["cases"].as_array().unwrap();
        assert!(cases.len() >= 38);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let got = verify(case["token"].as_str().unwrap(), &keys, &e);
            match (case["ok"].as_bool().unwrap(), &got) {
                (true, Ok(v)) => {
                    assert_eq!(v.sub, "user-1", "{name}");
                    assert_eq!(v.client_id, "client-1", "{name}");
                    assert_eq!(v.scopes, ["tools", "refunds:write"], "{name}");
                }
                (false, Err(r)) => assert_eq!(r.rule, case["why"].as_str().unwrap(), "{name}: {}", r.detail),
                _ => panic!("{name}: expected ok={}, got {got:?}", case["ok"]),
            }
        }
    }

    /// RFC 8037 appendix A.4: an Ed25519 JWS (its header has no typ, so this checks the
    /// signature path alone, with the typ rule bypassed by a header of our own).
    #[test]
    fn the_rfc_8037_ed25519_example_verifies() {
        use ring::signature::{UnparsedPublicKey, ED25519};
        let x = b64u("11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo").unwrap();
        let signing_input = b"eyJhbGciOiJFZERTQSJ9.RXhhbXBsZSBvZiBFZDI1NTE5IHNpZ25pbmc";
        let sig = b64u("hgyY0il_MGCjP0JzlnLWG1PPOt7-09PGcvMg3AIbQR6dWbhijcNR4ki4iylGjg5BhVsPt9g7sVvpAr_MuM0KAg").unwrap();
        assert!(UnparsedPublicKey::new(&ED25519, &x).verify(signing_input, &sig).is_ok());
        // The same key read from a JWK, used by verify's own path through Kind.
        let keys = parse_jwks(&json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "kid": "a4", "x": "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo" }] })).unwrap();
        assert_eq!(keys[0].kind, Kind::Ed25519(x));
    }

    /// RFC 7515 appendix A.3: the ES256 example verifies with ring's fixed-length ECDSA.
    #[test]
    fn the_rfc_7515_es256_example_verifies() {
        use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
        let keys = parse_jwks(&json!({ "keys": [{ "kty": "EC", "crv": "P-256", "kid": "a3",
            "x": "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU", "y": "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0" }] })).unwrap();
        let Kind::P256(p) = &keys[0].kind else { panic!() };
        let signing_input = b"eyJhbGciOiJFUzI1NiJ9.eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ";
        let sig = b64u("DtEhU3ljbEg8L38VWAfUAqOyKAM6-Xx-F4GawxaepmXFCgfTjDxw5djxLa8ISlSApmWQxfKTUJqPP3-Kg6NU1Q").unwrap();
        assert!(UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, p).verify(signing_input, &sig).is_ok());
    }

    #[test]
    fn key_sets_are_read_strictly() {
        let rsa = fixture("jwks.json")["keys"][0].clone();
        assert!(parse_jwks(&json!([])).is_err());
        assert!(parse_jwks(&json!({ "keys": {} })).is_err());
        assert!(parse_jwks(&json!({ "keys": [] })).unwrap_err().contains("no key this server can use"));
        assert!(parse_jwks(&json!({ "keys": [1] })).is_err());
        let many: Vec<Json> = (0..65).map(|i| { let mut k = rsa.clone(); k["kid"] = json!(i.to_string()); k }).collect();
        assert!(parse_jwks(&json!({ "keys": many })).unwrap_err().contains("at most 64"));
        assert!(parse_jwks(&json!({ "keys": [rsa.clone(), rsa.clone()] })).unwrap_err().contains("two keys with kid"));
        // Keys not used are left out, not refused: an encryption key, an unknown kty,
        // another curve, an alg not accepted, no kid.
        let mut enc = rsa.clone();
        enc["use"] = json!("enc");
        enc["kid"] = json!("enc");
        let mut hs = rsa.clone();
        hs["alg"] = json!("HS256");
        hs["kid"] = json!("hs");
        let mut nokid = rsa.clone();
        nokid.as_object_mut().unwrap().remove("kid");
        let set = json!({ "keys": [enc, hs, nokid, { "kty": "oct", "kid": "o", "k": "AAAA" }, { "kty": "EC", "crv": "P-384", "kid": "p384", "x": "AA", "y": "AA" }, rsa] });
        let keys = parse_jwks(&set).unwrap();
        assert_eq!(keys.iter().map(|k| k.kid.as_str()).collect::<Vec<_>>(), ["rsa"]);
    }

    #[test]
    fn bits_are_counted_from_the_top_byte() {
        assert_eq!(bits(&[]), 0);
        assert_eq!(bits(&[1]), 1);
        assert_eq!(bits(&[0x80, 0]), 16);
        assert_eq!(bits(&[0x7f; 256]), 2047);
    }
}
