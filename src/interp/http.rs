// The HTTP client behind `net.http` (RUNTIME.md section 12).
//
// The broker has already checked the canonical URL against every `only` pattern.
// This module sends exactly that request and reports exactly what is known about it:
//
//   Response  the server answered (any status); the request reached it.
//   NotSent   it failed before anything was sent (no such host, connection refused,
//             TLS handshake, connect timeout): it certainly did not happen.
//   Unknown   it failed after the request may have reached the server (a timeout or
//             a broken connection while sending or waiting): it may have happened.
//
// The distinction matters for actions that are not safe to repeat: the runtime
// treats Unknown like a crash in the middle of an action (in doubt), never like a
// failure the program might retry.
//
// Redirects are never followed (a redirect could lead outside `only`; the program
// gets the 3xx response), HTTP error statuses are responses, not errors, and no
// proxy is used unless the run config asks for one.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Settings {
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub max_response_bytes: u64,
    pub proxy_from_env: bool,
    /// Root certificates (DER) trusted in addition to Mozilla's, for services signed by
    /// an organisation's own authority. Never instead of them; verification is never off.
    pub extra_roots: Vec<Vec<u8>>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            max_response_bytes: 4 * 1024 * 1024,
            proxy_from_env: false,
            extra_roots: Vec::new(),
        }
    }
}

pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub body: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// `complete` is false when the body was larger than the limit, was cut off, or was
    /// not UTF-8 text; `body` is then empty. The request still happened.
    Response { status: u16, body: String, complete: bool },
    NotSent(String),
    Unknown(String),
}

pub struct Client {
    agent: ureq::Agent,
    max: u64,
}

impl Client {
    pub fn new(s: &Settings) -> Client {
        let mut builder = ureq::Agent::config_builder();
        if !s.extra_roots.is_empty() {
            use ureq::tls::{RootCerts, TlsConfig};
            builder = builder.tls_config(TlsConfig::builder().root_certs(RootCerts::from(roots(&s.extra_roots))).build());
        }
        let config = builder
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_global(Some(s.timeout))
            .timeout_connect(Some(s.connect_timeout))
            .proxy(if s.proxy_from_env { ureq::Proxy::try_from_env() } else { None })
            .user_agent(concat!("staalc/", env!("CARGO_PKG_VERSION")))
            .build();
        Client { agent: ureq::Agent::new_with_config(config), max: s.max_response_bytes }
    }

    pub fn send(&self, r: &Request) -> Outcome {
        // Resolve the host first: a name that cannot be looked up is certain not to have
        // received anything, and the client below reports that only as a general error.
        match url::Url::parse(r.url).map(|u| u.socket_addrs(|| None)) {
            Ok(Ok(addrs)) if !addrs.is_empty() => {}
            Ok(Ok(_)) => return Outcome::NotSent(format!("{} has no address", r.url)),
            Ok(Err(e)) => return Outcome::NotSent(format!("cannot look up the host of {}: {e}", r.url)),
            Err(e) => return Outcome::NotSent(format!("{} is not a URL: {e}", r.url)),
        }
        let result = match r.method {
            "get" => with_headers(self.agent.get(r.url), r.headers).call(),
            "delete" => with_headers(self.agent.delete(r.url), r.headers).call(),
            "post" => with_headers(self.agent.post(r.url), r.headers).send(r.body.unwrap_or("")),
            "put" => with_headers(self.agent.put(r.url), r.headers).send(r.body.unwrap_or("")),
            "patch" => with_headers(self.agent.patch(r.url), r.headers).send(r.body.unwrap_or("")),
            other => return Outcome::NotSent(format!("unknown HTTP method '{other}'")),
        };
        let mut resp = match result {
            Ok(resp) => resp,
            Err(e) => return classify(e),
        };
        let status = resp.status().as_u16();
        let body = resp.body_mut().with_config().limit(self.max).read_to_vec();
        match body.map(String::from_utf8) {
            Ok(Ok(text)) => Outcome::Response { status, body: text, complete: true },
            _ => Outcome::Response { status, body: String::new(), complete: false },
        }
    }
}

/// Mozilla's root certificates, then the operator's extra ones: added, never instead.
fn roots(extra: &[Vec<u8>]) -> Vec<ureq::tls::Certificate<'static>> {
    use ureq::tls::Certificate;
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|c| Certificate::from_der(c.as_ref()).to_owned())
        .chain(extra.iter().map(|d| Certificate::from_der(d).to_owned()))
        .collect()
}

fn with_headers<B>(mut rb: ureq::RequestBuilder<B>, headers: &[(String, String)]) -> ureq::RequestBuilder<B> {
    for (k, v) in headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    rb
}

/// Reads the certificates in a PEM file, for `extra_roots`. A file that holds a
/// private key is refused: a key has no place in a run's configuration.
pub fn read_root_certificates(path: &str) -> Result<Vec<Vec<u8>>, String> {
    let pem = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut out = Vec::new();
    for item in ureq::tls::parse_pem(&pem) {
        match item.map_err(|e| format!("{path}: {e}"))? {
            ureq::tls::PemItem::Certificate(c) => out.push(c.der().to_vec()),
            ureq::tls::PemItem::PrivateKey(_) => {
                return Err(format!("{path} contains a private key; give only the authority's certificate"))
            }
            _ => {}
        }
    }
    if out.is_empty() {
        return Err(format!("{path} contains no certificate"));
    }
    Ok(out)
}

/// Errors that are certain to have happened before any byte of the request left are
/// NotSent; everything else might have reached the server.
pub(super) fn classify(e: ureq::Error) -> Outcome {
    use ureq::Error as E;
    use ureq::Timeout as T;
    let text = e.to_string();
    match e {
        E::HostNotFound
        | E::ConnectionFailed
        | E::BadUri(_)
        | E::Http(_)
        | E::InvalidProxyUrl
        | E::Tls(_)
        | E::Timeout(T::Resolve | T::Connect) => Outcome::NotSent(text),
        // TLS checks the server's certificate, and agrees on a protocol, before the
        // client sends any request data (no early data is ever sent), so these errors
        // mean nothing was sent. Other TLS errors, such as an alert, can come later.
        E::Io(io)
            if io.get_ref().and_then(|e| e.downcast_ref::<rustls::Error>()).is_some_and(|e| {
                matches!(
                    e,
                    rustls::Error::InvalidCertificate(_)
                        | rustls::Error::NoCertificatesPresented
                        | rustls::Error::PeerIncompatible(_)
                )
            }) =>
        {
            Outcome::NotSent(text)
        }
        // Only a connection attempt can be refused or find no route; a reset, a broken
        // pipe or a timeout can come after the request was sent.
        E::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::AddrNotAvailable
                    | std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::HostUnreachable
            ) =>
        {
            Outcome::NotSent(text)
        }
        _ => Outcome::Unknown(text),
    }
}

/// Header names a program may not set: the runtime and the HTTP layer own them.
pub fn reserved_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "host" | "content-length" | "transfer-encoding" | "connection" | "upgrade" | "te" | "trailer" | "keep-alive"
            | "idempotency-key"
    ) || n.starts_with("proxy-")
}

/// A header name is an HTTP token; a value has no control characters (no CR or LF, so
/// no header can be smuggled into another).
pub fn check_header(name: &str, value: &str) -> Result<(), String> {
    let token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if name.is_empty() || !name.chars().all(token) {
        return Err(format!("'{name}' is not a valid header name"));
    }
    if value.chars().any(|c| c.is_control() && c != '\t') {
        return Err(format!("the value of header '{name}' contains a control character"));
    }
    if reserved_header(name) {
        return Err(format!("header '{name}' is set by the runtime and cannot be set by a program"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_roots_are_added_to_mozillas_never_instead() {
        let mozilla = webpki_root_certs::TLS_SERVER_ROOT_CERTS.len();
        assert!(mozilla > 100, "{mozilla} Mozilla roots");
        let extra = vec![vec![1u8, 2, 3]];
        let all = roots(&extra);
        assert_eq!(all.len(), mozilla + 1);
        assert_eq!(all.last().unwrap().der(), &[1, 2, 3]);
        for (a, b) in all.iter().zip(webpki_root_certs::TLS_SERVER_ROOT_CERTS) {
            assert_eq!(a.der(), b.as_ref());
        }
    }

    #[test]
    fn headers_cannot_smuggle_or_override() {
        assert!(check_header("Authorization", "Bearer abc").is_ok());
        assert!(check_header("X-Api-Key", "k\tv").is_ok());
        assert!(check_header("X-A", "a\r\nX-Evil: 1").is_err());
        assert!(check_header("X A", "v").is_err());
        assert!(check_header("", "v").is_err());
        for n in ["Host", "content-length", "Transfer-Encoding", "Proxy-Authorization", "Idempotency-Key"] {
            assert!(check_header(n, "v").is_err(), "{n}");
        }
    }

    #[test]
    fn a_host_that_does_not_exist_means_not_sent() {
        // `.invalid` never resolves (RFC 6761).
        let c = Client::new(&Settings::default());
        match c.send(&Request { method: "post", url: "https://no-such-host.invalid/x", headers: &[], body: Some("x") }) {
            Outcome::NotSent(_) => {}
            other => panic!("expected NotSent, got {other:?}"),
        }
    }

    #[test]
    fn nothing_listening_means_not_sent() {
        // Bind and close a port so nothing listens there.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = Client::new(&Settings::default());
        let url = format!("http://127.0.0.1:{port}/x");
        match c.send(&Request { method: "post", url: &url, headers: &[], body: Some("x") }) {
            Outcome::NotSent(_) => {}
            other => panic!("expected NotSent, got {other:?}"),
        }
    }
}
