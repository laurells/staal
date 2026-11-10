// The HTTP client behind `net.http` (RUNTIME.md section 12).
//
// The broker has already checked the target (`targets::Target`) against every `only`
// pattern. This module sends exactly that request, to the URI built from the same
// parsed target (never a string parsed again), and reports exactly what is known about
// it:
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
// proxy is used unless the run config asks for one; then the proxy is the one
// `proxy::of_setting` chose and the journal recorded, never one read again.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Settings {
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub max_response_bytes: u64,
    /// The proxy `net.proxy` chose, if any (`proxy`).
    pub proxy: Option<super::proxy::Choice>,
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
            proxy: None,
            extra_roots: Vec::new(),
        }
    }
}

pub struct Request<'a> {
    pub method: &'a str,
    pub target: &'a crate::targets::Target,
    pub headers: &'a [(String, String)],
    pub body: Option<&'a str>,
}

/// The most header fields a `Response` holds (RUNTIME.md section 12): the most the HTTP
/// client parses (ureq-proto's MAX_RESPONSE_HEADERS). A response with more cannot be
/// read at all, so it is no answer. tests/http_test.rs checks both sides of the limit,
/// so a client that changes it fails the tests.
pub const MAX_HEADER_FIELDS: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// `headers` are the response's header fields, each name lowercased (field names are
    /// case-insensitive, RFC 9110 section 5.1), sorted by name, each name's values in the
    /// order received. `complete` is false when part of the response could not be held:
    /// `body` is empty when the body was larger than the limit, was cut off, or was not
    /// UTF-8 text, and `headers` is empty when a header value was not UTF-8 text. The
    /// request still happened.
    Response { status: u16, headers: Vec<(String, String)>, body: String, complete: bool },
    NotSent(String),
    Unknown(String),
}

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    max: u64,
    /// The proxy requests go through, if the run config asked for one.
    proxy: Option<ureq::Proxy>,
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
            .proxy(s.proxy.as_ref().map(|c| c.proxy.clone()))
            .user_agent(concat!("staalc/", env!("CARGO_PKG_VERSION")))
            .build();
        Client { agent: ureq::Agent::new_with_config(config), max: s.max_response_bytes, proxy: s.proxy.as_ref().map(|c| c.proxy.clone()) }
    }

    pub fn send(&self, r: &Request) -> Outcome {
        let shown = r.target.as_str();
        // Resolve the host first: a name that cannot be looked up is certain not to have
        // received anything, and the client below reports that only as a general error.
        // An address literal needs no lookup, and nor does a host reached through a
        // proxy: the proxy looks it up, and this side may not be able to.
        let proxied = self.proxy.as_ref().is_some_and(|p| !p.is_no_proxy(r.target.uri()));
        if let (url::Host::Domain(d), false) = (r.target.host(), proxied) {
            use std::net::ToSocketAddrs;
            match (d.as_str(), r.target.port()).to_socket_addrs().map(|mut a| a.next().is_some()) {
                Ok(true) => {}
                Ok(false) => return Outcome::NotSent(format!("{shown} has no address")),
                Err(e) => return Outcome::NotSent(format!("cannot look up the host of {shown}: {e}")),
            }
        }
        use ureq::http::Method;
        let method = match r.method {
            "get" => Method::GET,
            "delete" => Method::DELETE,
            "post" => Method::POST,
            "put" => Method::PUT,
            "patch" => Method::PATCH,
            other => return Outcome::NotSent(format!("unknown HTTP method '{other}'")),
        };
        let mut builder = ureq::http::Request::builder().method(method).uri(r.target.uri().clone());
        for (k, v) in r.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let result = match r.method {
            "post" | "put" | "patch" => match builder.body(r.body.unwrap_or("")) {
                Ok(req) => self.agent.run(req),
                Err(e) => return Outcome::NotSent(format!("the request cannot be made: {e}")),
            },
            _ => match builder.body(()) {
                Ok(req) => self.agent.run(req),
                Err(e) => return Outcome::NotSent(format!("the request cannot be made: {e}")),
            },
        };
        let mut resp = match result {
            Ok(resp) => resp,
            Err(e) => return classify(e),
        };
        let status = resp.status().as_u16();
        let headers = header_fields(resp.headers());
        let body = match resp.body_mut().with_config().limit(self.max).read_to_vec().map(String::from_utf8) {
            Ok(Ok(text)) => Some(text),
            _ => None,
        };
        let complete = headers.is_some() && body.is_some();
        Outcome::Response { status, headers: headers.unwrap_or_default(), body: body.unwrap_or_default(), complete }
    }
}

/// A response's header fields as `Outcome::Response` holds them, or None when they
/// cannot all be held: a value that is not UTF-8, or more than MAX_HEADER_FIELDS (which
/// the client does not parse, so this only guards the limit's agreement).
/// `HeaderMap` iterates names in no guaranteed order, so they are sorted; `get_all`
/// gives one name's values in the order received.
fn header_fields(map: &ureq::http::HeaderMap) -> Option<Vec<(String, String)>> {
    if map.len() > MAX_HEADER_FIELDS {
        return None;
    }
    let mut names: Vec<&ureq::http::HeaderName> = map.keys().collect();
    names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let mut out = Vec::with_capacity(map.len());
    for name in names {
        for value in map.get_all(name) {
            // `HeaderName` is already lowercase; `to_str` refuses bytes outside visible
            // ASCII, so take the bytes and require UTF-8.
            let text = std::str::from_utf8(value.as_bytes()).ok()?;
            out.push((name.as_str().to_string(), text.to_string()));
        }
    }
    Some(out)
}

/// Mozilla's root certificates, then the operator's extra ones: added, never instead.
pub(crate) fn roots(extra: &[Vec<u8>]) -> Vec<ureq::tls::Certificate<'static>> {
    use ureq::tls::Certificate;
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|c| Certificate::from_der(c.as_ref()).to_owned())
        .chain(extra.iter().map(|d| Certificate::from_der(d).to_owned()))
        .collect()
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
        // The proxy refused the tunnel or did not answer: nothing reached the target.
        | E::ConnectProxyFailed(_)
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
        let t = crate::targets::target("https://no-such-host.invalid/x").unwrap();
        match c.send(&Request { method: "post", target: &t, headers: &[], body: Some("x") }) {
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
        let t = crate::targets::target(&url).unwrap();
        match c.send(&Request { method: "post", target: &t, headers: &[], body: Some("x") }) {
            Outcome::NotSent(_) => {}
            other => panic!("expected NotSent, got {other:?}"),
        }
    }

    /// One request a recording server received: which connection (numbered from 1 in
    /// the order accepted), its request line and its `Host`.
    #[derive(Debug, Clone, PartialEq)]
    struct Seen {
        conn: usize,
        line: String,
        host: String,
    }

    /// A server on `listener` that answers every request `200 ok` and keeps each
    /// connection open for the next, recording what it received.
    fn recorder(listener: std::net::TcpListener) -> std::sync::Arc<std::sync::Mutex<Vec<Seen>>> {
        use std::io::{BufRead, BufReader, Read, Write};
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let out = seen.clone();
        std::thread::spawn(move || {
            for (i, conn) in listener.incoming().enumerate() {
                let Ok(conn) = conn else { continue };
                let seen = seen.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(conn.try_clone().unwrap());
                    let mut conn = conn;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let (mut host, mut len) = (String::new(), 0usize);
                        loop {
                            let mut h = String::new();
                            if reader.read_line(&mut h).unwrap_or(0) == 0 {
                                return;
                            }
                            let h = h.trim_end();
                            if h.is_empty() {
                                break;
                            }
                            if let Some((k, v)) = h.split_once(':') {
                                match k.to_ascii_lowercase().as_str() {
                                    "host" => host = v.trim().to_string(),
                                    "content-length" => len = v.trim().parse().unwrap_or(0),
                                    _ => {}
                                }
                            }
                        }
                        let mut body = vec![0u8; len];
                        if reader.read_exact(&mut body).is_err() {
                            return;
                        }
                        seen.lock().unwrap().push(Seen { conn: i + 1, line: line.trim_end().to_string(), host });
                        if conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").is_err() {
                            return;
                        }
                    }
                });
            }
        });
        out
    }

    /// The `Host` a target sends: its authority, as checked.
    fn authority(t: &crate::targets::Target) -> String {
        t.uri().authority().unwrap().as_str().to_string()
    }

    /// AGENT_MANIFEST.md section 16, R6: whatever spelling a program gives, the request
    /// reaches the address checked, with the checked authority as its `Host` and the
    /// checked path and query as its request line.
    #[test]
    fn the_request_sent_is_the_target_checked() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = recorder(l);
        let c = Client::new(&Settings::default());
        let cases = [
            (format!("http://127.0.0.1:{port}/a/./b/%2e%2E/c?x=%7e#f"), format!("127.0.0.1:{port}"), "/a/c?x=~"),
            (format!("http://0x7f.1:{port}/x"), format!("127.0.0.1:{port}"), "/x"),
            (format!("http://2130706433:{port}/y"), format!("127.0.0.1:{port}"), "/y"),
            (format!("http://LOCALHOST:{port}/z"), format!("localhost:{port}"), "/z"),
            (format!("http://127.0.0.1:{port}\\@evil.example/w"), format!("127.0.0.1:{port}"), "/@evil.example/w"),
            (format!("http://127.0.0.1:0{port}/v"), format!("127.0.0.1:{port}"), "/v"),
        ];
        for (i, (given, host, path)) in cases.iter().enumerate() {
            let t = crate::targets::target(given).unwrap();
            assert_eq!(&authority(&t), host, "{given}");
            match c.send(&Request { method: "get", target: &t, headers: &[], body: None }) {
                Outcome::Response { status: 200, .. } => {}
                other => panic!("{given}: {other:?}"),
            }
            let got = seen.lock().unwrap()[i].clone();
            assert_eq!((got.line.as_str(), got.host.as_str()), (format!("GET {path} HTTP/1.1").as_str(), host.as_str()), "{given}");
        }
        // IPv6, where this machine has it.
        if let Ok(l6) = std::net::TcpListener::bind("[::1]:0") {
            let p6 = l6.local_addr().unwrap().port();
            let seen6 = recorder(l6);
            let t = crate::targets::target(&format!("http://[0:0:0:0:0:0:0:1]:{p6}/six")).unwrap();
            assert!(matches!(c.send(&Request { method: "get", target: &t, headers: &[], body: None }), Outcome::Response { status: 200, .. }));
            let got = seen6.lock().unwrap()[0].clone();
            assert_eq!((got.line.as_str(), got.host.as_str()), ("GET /six HTTP/1.1", format!("[::1]:{p6}").as_str()));
        }
    }

    /// R6: a kept-alive connection carries only requests for the authority it was
    /// opened for. Here two authorities reach the same listener: a request for the
    /// second never travels on the first's connection, though the first's is reused.
    #[test]
    fn a_kept_alive_connection_is_used_only_for_its_own_authority() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        use std::net::ToSocketAddrs;
        let localhost_v4 = ("localhost", port).to_socket_addrs().map(|mut a| a.any(|x| x.ip().is_ipv4())).unwrap_or(false);
        assert!(localhost_v4, "localhost must resolve to 127.0.0.1 for this test");
        let seen = recorder(l);
        let c = Client::new(&Settings::default());
        let ip = crate::targets::target(&format!("http://127.0.0.1:{port}/1")).unwrap();
        let name = crate::targets::target(&format!("http://localhost:{port}/2")).unwrap();
        for t in [&ip, &ip, &name, &ip] {
            assert!(matches!(c.send(&Request { method: "get", target: t, headers: &[], body: None }), Outcome::Response { status: 200, .. }));
        }
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].conn, got[1].conn, "the pool is in use, so the test can see misuse: {got:?}");
        assert_ne!(got[2].conn, got[0].conn, "a request for localhost travelled on 127.0.0.1's connection: {got:?}");
        assert_eq!(got[2].host, format!("localhost:{port}"));
        for g in [&got[0], &got[1], &got[3]] {
            assert_eq!(g.host, format!("127.0.0.1:{port}"));
            assert_ne!(g.conn, got[2].conn, "a request for 127.0.0.1 travelled on localhost's connection: {got:?}");
        }
    }

    /// R6: through a proxy, the CONNECT names the checked host and port (the host as
    /// written after checking, never an address looked up here), and its `Host` is the
    /// same.
    #[test]
    fn a_proxy_is_asked_for_the_checked_host_and_port() {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_port = l.local_addr().unwrap().port();
        let heads = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let h2 = heads.clone();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let mut host = String::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
                        break;
                    }
                    if let Some(v) = h.trim_end().strip_prefix("Host:") {
                        host = v.trim().to_string();
                    }
                }
                h2.lock().unwrap().push((line.trim_end().to_string(), host));
                // Refuse the tunnel: nothing reaches the target.
                let _ = conn.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let env = |k: &str| (k == "HTTPS_PROXY").then(|| format!("http://127.0.0.1:{proxy_port}"));
        let proxy = super::super::proxy::from_environment(env).unwrap();
        let c = Client::new(&Settings { proxy, ..Settings::default() });
        let cases = [
            ("https://LOCALHOST:9443/x", "localhost:9443"),
            ("https://BÜCHER.example/x", "xn--bcher-kva.example:443"),
            ("https://0x7f.1/x", "127.0.0.1:443"),
            ("https://[0:0::1]:8443/x", "[::1]:8443"),
            ("http://127.1:8080/x", "127.0.0.1:8080"),
        ];
        for (i, (given, dest)) in cases.iter().enumerate() {
            let t = crate::targets::target(given).unwrap();
            match c.send(&Request { method: "post", target: &t, headers: &[], body: Some("x") }) {
                Outcome::NotSent(_) => {}
                other => panic!("{given}: a refused tunnel is not sent: {other:?}"),
            }
            let got = heads.lock().unwrap()[i].clone();
            assert_eq!((got.0.as_str(), got.1.as_str()), (format!("CONNECT {dest} HTTP/1.1").as_str(), *dest), "{given}");
        }
    }
}
