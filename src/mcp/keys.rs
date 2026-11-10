// The authorization server's signing keys, for the HTTP transport (docs/MCP.md section
// 12.5): found from its metadata at start, fetched again when a token names an unknown
// key (at most once a minute) and every hour, and used for at most 24 hours after the
// fetch that got them.
//
// Every fetch is HTTPS (the server's certificate verified against Mozilla's roots and
// the operator's extra ones, and against the host), follows no redirect, must connect
// within 5 s and finish within 10 s, and reads at most 256 KiB of JSON nested at most 16
// levels; what it reads is checked field by field. A failed fetch keeps the keys held.

use super::jwt::{self, Key};
use serde_json::Value as Json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const CONNECT_TIME: Duration = Duration::from_secs(5);
pub const FETCH_TIME: Duration = Duration::from_secs(10);
pub const MAX_DOCUMENT_BYTES: u64 = 256 * 1024;
pub const MAX_DEPTH: usize = 16;
/// A token naming an unknown key fetches the set again at most this often.
pub const UNKNOWN_KID_EVERY: Duration = Duration::from_secs(60);
/// The set is fetched again this often regardless.
pub const REFRESH_EVERY: Duration = Duration::from_secs(60 * 60);
/// Keys are used for at most this long after the fetch that got them.
pub const MAX_KEY_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Fetches JSON documents as section 12.5 requires.
#[derive(Clone)]
pub struct Fetcher {
    agent: ureq::Agent,
}

impl Fetcher {
    /// `extra_roots`: DER certificates trusted in addition to Mozilla's roots.
    pub fn new(extra_roots: &[Vec<u8>], connect: Duration, total: Duration) -> Fetcher {
        use ureq::tls::{RootCerts, TlsConfig};
        let config = ureq::Agent::config_builder()
            .tls_config(TlsConfig::builder().root_certs(RootCerts::from(crate::interp::http::roots(extra_roots))).build())
            .https_only(true)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_global(Some(total))
            .timeout_connect(Some(connect))
            .proxy(None)
            .user_agent(concat!("staalc/", env!("CARGO_PKG_VERSION")))
            .build();
        Fetcher { agent: ureq::Agent::new_with_config(config) }
    }

    /// GETs `url`: Ok(Some(document)) for a 200 with a JSON object, Ok(None) for a 404
    /// (another discovery location may answer), Err for anything else.
    pub fn get(&self, url: &str) -> Result<Option<serde_json::Map<String, Json>>, String> {
        // Parsed once, as `net.http` parses a target; the request is sent to the URI
        // built from that parse, never to the text again. A `#` always starts a
        // fragment in the URL Standard.
        let target = crate::targets::target(url).map_err(|e| format!("{url}: {e}"))?;
        if !target.is_https() || url.contains('#') {
            return Err(format!("{url}: only https URLs without a fragment are fetched"));
        }
        let request = ureq::http::Request::get(target.uri().clone())
            .header("accept", "application/json")
            .body(())
            .map_err(|e| format!("fetching {url}: {e}"))?;
        let mut resp = self.agent.run(request).map_err(|e| format!("fetching {url}: {e}"))?;
        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if (300..400).contains(&status) {
            return Err(format!("{url} answered with a redirect ({status}), which is not followed"));
        }
        if status != 200 {
            return Err(format!("{url} answered {status}"));
        }
        let body = resp
            .body_mut()
            .with_config()
            .limit(MAX_DOCUMENT_BYTES)
            .read_to_vec()
            .map_err(|e| format!("{url}: the document could not be read whole within {MAX_DOCUMENT_BYTES} bytes: {e}"))?;
        if super::proto::nesting(&body, MAX_DEPTH) > MAX_DEPTH {
            return Err(format!("{url}: the document is nested deeper than {MAX_DEPTH} levels"));
        }
        match serde_json::from_slice::<Json>(&body) {
            Ok(Json::Object(o)) => Ok(Some(o)),
            Ok(_) => Err(format!("{url}: the document is not a JSON object")),
            Err(e) => Err(format!("{url}: not JSON: {e}")),
        }
    }
}

/// Where the metadata of the authorization server `issuer` may be, in the order the
/// specification gives (RFC 8414 section 3.1, then OpenID Connect Discovery with path
/// insertion, then path appending).
pub fn metadata_urls(issuer: &url::Url) -> Vec<String> {
    let origin = issuer.origin().ascii_serialization();
    let path = issuer.path().trim_end_matches('/');
    if path.is_empty() {
        vec![format!("{origin}/.well-known/oauth-authorization-server"), format!("{origin}/.well-known/openid-configuration")]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{origin}{path}/.well-known/openid-configuration"),
        ]
    }
}

/// Finds the authorization server's `jwks_uri` from its metadata. The first location
/// that answers 200 decides: its `issuer` must equal `issuer` exactly (RFC 8414 section
/// 3.3), and its `jwks_uri` must be an https URL without a fragment.
pub fn discover(f: &Fetcher, issuer: &str) -> Result<String, String> {
    let u = url::Url::parse(issuer).map_err(|e| format!("{issuer}: {e}"))?;
    for at in metadata_urls(&u) {
        let Some(doc) = f.get(&at)? else { continue };
        let got = doc.get("issuer").and_then(Json::as_str).ok_or(format!("{at}: the metadata has no issuer string"))?;
        if got != issuer {
            return Err(format!("{at}: the metadata's issuer is not {issuer} exactly, so it is not used"));
        }
        let jwks = doc.get("jwks_uri").and_then(Json::as_str).ok_or(format!("{at}: the metadata has no jwks_uri string"))?;
        let ju = url::Url::parse(jwks).map_err(|e| format!("{at}: jwks_uri: {e}"))?;
        if ju.scheme() != "https" || ju.fragment().is_some() {
            return Err(format!("{at}: jwks_uri must be an https URL without a fragment"));
        }
        return Ok(jwks.to_string());
    }
    Err(format!("no authorization server metadata was found for {issuer}"))
}

/// Fetches and reads the key set at `jwks_uri`.
pub fn fetch_keys(f: &Fetcher, jwks_uri: &str) -> Result<Vec<Key>, String> {
    let doc = f.get(jwks_uri)?.ok_or(format!("{jwks_uri} answered 404"))?;
    jwt::parse_jwks(&Json::Object(doc)).map_err(|e| format!("{jwks_uri}: {e}"))
}

/// How the keys are got: from the network in the server, from a function in tests.
pub type Source = Arc<dyn Fn() -> Result<Vec<Key>, String> + Send + Sync>;

struct Held {
    keys: Vec<Key>,
    /// When the fetch that got `keys` finished.
    fetched: Instant,
    /// When a token's unknown kid last made a fetch.
    unknown_fetch: Option<Instant>,
}

/// The keys in use, with the rules for fetching them again.
pub struct Cache {
    source: Source,
    held: Mutex<Held>,
}

/// What the cache gives a token to be verified against.
pub enum Keys {
    Held(Vec<Key>),
    /// Every fetch has failed for longer than MAX_KEY_AGE: no token is accepted.
    TooOld,
}

impl Cache {
    /// Fetches the keys once; an error stops the server from starting.
    pub fn start(source: Source, now: Instant) -> Result<Cache, String> {
        let keys = source()?;
        Ok(Cache { source, held: Mutex::new(Held { keys, fetched: now, unknown_fetch: None }) })
    }

    /// The keys to verify a token naming `kid` against, at `now`. A `kid` not held makes
    /// one fetch, unless one was made for an unknown kid within UNKNOWN_KID_EVERY. Blocks
    /// while it fetches.
    pub fn for_kid(&self, kid: Option<&str>, now: Instant) -> Keys {
        let refetch = {
            let Ok(mut h) = self.held.lock() else { return Keys::TooOld };
            let unknown = kid.is_some_and(|k| !h.keys.iter().any(|x| x.kid == k));
            let due = h.unknown_fetch.is_none_or(|t| now.saturating_duration_since(t) >= UNKNOWN_KID_EVERY);
            if unknown && due {
                // Marked before fetching, so tokens arriving meanwhile do not fetch too.
                h.unknown_fetch = Some(now);
                true
            } else {
                false
            }
        };
        if refetch && let Err(e) = self.fetch(now) {
            // The keys held stay in use (within MAX_KEY_AGE); the operator is told.
            crate::host::log(&format!("fetching the authorization server's keys for an unknown kid: {e}"));
        }
        self.current(now)
    }

    /// Fetches the set again (the hourly refresh, or for an unknown kid). A failure keeps
    /// the keys held and is returned for the log.
    pub fn fetch(&self, now: Instant) -> Result<(), String> {
        let got = (self.source)();
        let Ok(mut h) = self.held.lock() else { return Err("the key cache was lost".into()) };
        match got {
            Ok(keys) => {
                h.keys = keys;
                h.fetched = now;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn current(&self, now: Instant) -> Keys {
        let Ok(h) = self.held.lock() else { return Keys::TooOld };
        if now.saturating_duration_since(h.fetched) > MAX_KEY_AGE {
            return Keys::TooOld;
        }
        Keys::Held(h.keys.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn set(kids: &[&str]) -> Vec<Key> {
        let keys: Vec<Json> = kids.iter().map(|k| json!({ "kty": "OKP", "crv": "Ed25519", "kid": k, "x": "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo" })).collect();
        jwt::parse_jwks(&json!({ "keys": keys })).unwrap()
    }

    fn kids(k: &Keys) -> Vec<String> {
        match k {
            Keys::Held(v) => v.iter().map(|k| k.kid.clone()).collect(),
            Keys::TooOld => vec!["TOO OLD".into()],
        }
    }

    #[test]
    fn keys_are_fetched_again_only_as_the_rules_say() {
        // The server publishes "a" at first, then rotates to "b".
        let fetches = Arc::new(AtomicUsize::new(0));
        let published = Arc::new(Mutex::new(Ok(vec!["a"])));
        let source: Source = {
            let (fetches, published) = (fetches.clone(), published.clone());
            Arc::new(move || {
                fetches.fetch_add(1, Ordering::SeqCst);
                published.lock().unwrap().clone().map(|k| set(&k))
            })
        };
        let t0 = Instant::now();
        let c = Cache::start(source, t0).unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert_eq!(kids(&c.for_kid(Some("a"), t0)), ["a"]);
        assert_eq!(fetches.load(Ordering::SeqCst), 1, "a known kid fetches nothing");

        // A rotated key: the first token naming it fetches, and finds it.
        *published.lock().unwrap() = Ok(vec!["b"]);
        assert_eq!(kids(&c.for_kid(Some("b"), t0 + Duration::from_secs(1))), ["b"]);
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        // The old key is gone with the set that dropped it.
        assert_eq!(kids(&c.for_kid(Some("b"), t0 + Duration::from_secs(2))), ["b"]);

        // Made-up kids: at most one fetch a minute.
        for s in 3..60 {
            c.for_kid(Some(&format!("made-up-{s}")), t0 + Duration::from_secs(s));
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 2, "within a minute of the last such fetch");
        c.for_kid(Some("made-up"), t0 + Duration::from_secs(61));
        assert_eq!(fetches.load(Ordering::SeqCst), 3);

        // A failed fetch keeps the keys held, for up to 24 hours after the last good one.
        *published.lock().unwrap() = Err("down".to_string());
        let last_good = t0 + Duration::from_secs(61);
        assert!(c.fetch(last_good + Duration::from_secs(3600)).is_err());
        assert_eq!(kids(&c.for_kid(Some("b"), last_good + MAX_KEY_AGE)), ["b"]);
        assert_eq!(kids(&c.for_kid(Some("b"), last_good + MAX_KEY_AGE + Duration::from_secs(1))), ["TOO OLD"]);
        // A fetch that succeeds again restores them.
        *published.lock().unwrap() = Ok(vec!["c"]);
        let later = last_good + MAX_KEY_AGE + Duration::from_secs(5);
        c.fetch(later).unwrap();
        assert_eq!(kids(&c.for_kid(Some("c"), later)), ["c"]);
    }

    #[test]
    fn a_failed_first_fetch_stops_the_start() {
        let source: Source = Arc::new(|| Err("unreachable".into()));
        assert!(Cache::start(source, Instant::now()).is_err());
    }

    #[test]
    fn metadata_is_looked_for_where_the_specification_says() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert_eq!(
            metadata_urls(&u("https://auth.example.com")),
            ["https://auth.example.com/.well-known/oauth-authorization-server", "https://auth.example.com/.well-known/openid-configuration"]
        );
        assert_eq!(
            metadata_urls(&u("https://auth.example.com/tenant1")),
            [
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.com/.well-known/openid-configuration/tenant1",
                "https://auth.example.com/tenant1/.well-known/openid-configuration",
            ]
        );
    }
}
