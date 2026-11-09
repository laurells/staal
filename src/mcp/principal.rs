// The principal of an HTTP call (docs/MCP.md section 12.6): the pair (iss, sub) of its
// validated token, turned into the call's scope with HMAC-SHA-256 (RFC 2104) keyed by
// the server's principal key, over
//
//     "staal-principal-v1" 0x00  len(iss) iss  len(sub) sub
//
// with each length the identifier's UTF-8 byte count as a 4-byte big-endian unsigned
// integer, in lowercase hexadecimal. Without the key, a guessed subject identifier
// cannot be matched to its scope in the ledger.

/// The server's principal key. Its Debug never shows it.
#[derive(Clone)]
pub struct PrincipalKey(ring::hmac::Key);

/// The fewest bytes a principal key may have.
pub const MIN_KEY_BYTES: usize = 32;

impl std::fmt::Debug for PrincipalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrincipalKey(..)")
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl PrincipalKey {
    pub fn new(secret: &[u8]) -> Result<PrincipalKey, String> {
        if secret.len() < MIN_KEY_BYTES {
            return Err(format!("the principal key must be at least {MIN_KEY_BYTES} bytes"));
        }
        Ok(PrincipalKey(ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret)))
    }

    /// The scope of the principal (`iss`, `sub`).
    pub fn scope(&self, iss: &str, sub: &str) -> String {
        let mut m = b"staal-principal-v1\0".to_vec();
        for part in [iss, sub] {
            // A JWT is far shorter than 4 GiB; a longer identifier cannot arrive.
            m.extend_from_slice(&u32::try_from(part.len()).unwrap_or(u32::MAX).to_be_bytes());
            m.extend_from_slice(part.as_bytes());
        }
        hex(ring::hmac::sign(&self.0, &m).as_ref())
    }

    /// A fingerprint of the key, recorded beside the ledger so a changed key is noticed;
    /// it reveals nothing of the key.
    pub fn fingerprint(&self) -> String {
        hex(ring::hmac::sign(&self.0, b"staal-principal-key-fingerprint").as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values computed independently with Python's hmac and struct modules.
    #[test]
    fn scopes_and_fingerprints_match_an_independent_computation() {
        let k = PrincipalKey::new(&[b'k'; 32]).unwrap();
        assert_eq!(k.scope("https://auth.test.example", "user-1"), "edea352a3b7f634cdeb3461b334d45facc9eeee89f7096b979aeb81d658308a9");
        // The length prefixes keep a different split of the same characters apart.
        assert_eq!(k.scope("https://auth.test.example/u", "ser-1"), "7b91e569f4357672ceec528e5861bfedcf5618ee56f94073bd2f7bb25bb44b44");
        assert_eq!(k.fingerprint(), "1871b401d6f58bba56f2d8316d676c36382c7041a3f3b32175e1a37d57adf50d");
        assert!(PrincipalKey::new(&[b'k'; 31]).is_err());
        assert_eq!(format!("{k:?}"), "PrincipalKey(..)");
    }
}
