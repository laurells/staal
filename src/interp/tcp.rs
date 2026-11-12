// The TCP client behind `net.tcp.exchange` (RUNTIME.md section 12).
//
// The broker has already checked the canonical address against every `only` pattern.
// One exchange connects, sends the program's data, and reads the reply: until the
// peer closes the connection, or, when the program names one, until the reply ends
// with a delimiter (`"\r\n"`, say), for protocols that keep the connection open. With
// no delimiter, the sending side is closed after the data, which tells the peer the
// request is complete. Plain TCP: nothing is encrypted.
//
// What is known about an exchange, as for HTTP (http.rs):
//
//   Reply     the peer answered (or closed without a word): it happened.
//   NotSent   it failed before anything was sent (no such host, connection refused,
//             connect timeout): it certainly did not happen.
//   Unknown   it failed after the data may have reached the peer (a timeout or a
//             broken connection while sending or waiting): it may have happened.
//   Unusable  it happened, but the reply cannot be used (too long, not UTF-8 text).

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Settings {
    pub connect_timeout: Duration,
    /// For the whole exchange: connecting, the TLS handshake, sending and every read.
    pub timeout: Duration,
    pub max_reply_bytes: u64,
    /// For `net.tls`: Mozilla's root certificates and the operator's extra ones.
    pub tls: Arc<rustls::ClientConfig>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings").field("timeout", &self.timeout).finish_non_exhaustive()
    }
}

/// The TLS settings `net.tls` uses: certificates verified against Mozilla's roots and
/// `extra` (DER), added, never instead; the safe default protocol versions.
pub fn tls_config(extra: &[Vec<u8>]) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    for c in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        roots.add(c.clone()).map_err(|e| format!("a root certificate: {e}"))?;
    }
    for d in extra {
        roots.add(rustls::pki_types::CertificateDer::from(d.clone())).map_err(|e| format!("net.extra_root_certificates: {e}"))?;
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Reply(String),
    NotSent(String),
    Unknown(String),
    Unusable(String),
}

/// One exchange, over TLS when `tls`, with an address `reach` permits (RUNTIME.md
/// section 12, "Addresses"): the name is looked up once, and only the addresses that
/// pass are connected to.
pub fn exchange(address: &str, data: &[u8], until: Option<&str>, s: &Settings, tls: bool, reach: &super::addresses::Policy) -> Outcome {
    let deadline = Instant::now() + s.timeout;
    let found: Vec<_> = match address.to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => return Outcome::NotSent(format!("cannot resolve {address}: {e}")),
    };
    if found.is_empty() {
        return Outcome::NotSent(format!("{address} resolves to no address"));
    }
    let addrs = match reach.check(found) {
        Ok(a) => a,
        Err(why) => return Outcome::NotSent(format!("{address}: {why}")),
    };
    let mut stream = None;
    let mut last = format!("{address} resolves to no address");
    for a in &addrs {
        match TcpStream::connect_timeout(a, s.connect_timeout.min(s.timeout)) {
            Ok(st) => {
                stream = Some(st);
                break;
            }
            Err(e) => last = format!("cannot connect to {address} ({a}): {e}"),
        }
    }
    let Some(st) = stream else { return Outcome::NotSent(last) };
    let _ = st.set_nodelay(true);
    let left = move || deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero());
    if !tls {
        let mut st = st;
        return talk(&mut st, address, data, until, s, &left, |st: &TcpStream| st.try_clone(), |st: &mut TcpStream| st.shutdown(Shutdown::Write));
    }
    // The handshake comes before any of the data: if it fails, nothing was sent.
    let host = address.rsplit_once(':').map_or(address, |(h, _)| h).trim_start_matches('[').trim_end_matches(']');
    let name = match rustls::pki_types::ServerName::try_from(host.to_string()) {
        Ok(n) => n,
        Err(e) => return Outcome::NotSent(format!("{address}: {e}")),
    };
    let conn = match rustls::ClientConnection::new(s.tls.clone(), name) {
        Ok(c) => c,
        Err(e) => return Outcome::NotSent(format!("{address}: {e}")),
    };
    let mut tls = rustls::StreamOwned::new(conn, st);
    while tls.conn.is_handshaking() {
        let Some(d) = left() else { return Outcome::NotSent(format!("{address}: no TLS handshake within {} ms", s.timeout.as_millis())) };
        if let Err(e) = tls.sock.set_read_timeout(Some(d)).and_then(|_| tls.sock.set_write_timeout(Some(d))) {
            return Outcome::NotSent(format!("{address}: {e}"));
        }
        if let Err(e) = tls.conn.complete_io(&mut tls.sock) {
            return Outcome::NotSent(format!("the TLS handshake with {address} failed: {e}"));
        }
    }
    talk(
        &mut tls,
        address,
        data,
        until,
        s,
        &left,
        |t: &rustls::StreamOwned<rustls::ClientConnection, TcpStream>| t.sock.try_clone(),
        |t: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>| {
            t.conn.send_close_notify();
            t.flush()
        },
    )
}

/// Sends the data and reads the reply, on a stream whose socket `sock` gives (to set
/// timeouts on); `close` ends the sending side when there is no delimiter.
#[allow(clippy::too_many_arguments)]
fn talk<S: Read + Write>(
    st: &mut S,
    address: &str,
    data: &[u8],
    until: Option<&str>,
    s: &Settings,
    left: &dyn Fn() -> Option<Duration>,
    sock: impl Fn(&S) -> std::io::Result<TcpStream>,
    close: impl FnOnce(&mut S) -> std::io::Result<()>,
) -> Outcome {
    // Each read and write is bounded by what is left of the exchange's time. The
    // deadline passing is the exchange's failure; the socket refusing a new timeout
    // is not: macOS refuses one (EINVAL) once the peer has closed a connection whose
    // sending side is already shut, while the reply still waits to be read. The read
    // timeout set before sending then still bounds the read.
    enum Bound {
        Passed(String),
        Refused(String),
    }
    let timeout = |st: &S, read: bool| -> Result<(), Bound> {
        let d = left().ok_or_else(|| Bound::Passed(format!("no reply from {address} within {} ms", s.timeout.as_millis())))?;
        let so = sock(st).map_err(|e| Bound::Refused(format!("{address}: {e}")))?;
        if read { so.set_read_timeout(Some(d)) } else { so.set_write_timeout(Some(d)) }.map_err(|e| Bound::Refused(format!("{address}: {e}")))
    };
    // From here on the data may reach the peer.
    for read in [false, true] {
        if let Err(Bound::Passed(e) | Bound::Refused(e)) = timeout(st, read) {
            return Outcome::Unknown(e);
        }
    }
    if let Err(e) = st.write_all(data).and_then(|_| st.flush()) {
        return Outcome::Unknown(format!("sending to {address}: {e}"));
    }
    if until.is_none()
        && let Err(e) = close(st)
    {
        return Outcome::Unknown(format!("closing the request to {address}: {e}"));
    }
    let mut reply: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if let Some(u) = until
            && reply.ends_with(u.as_bytes())
        {
            break;
        }
        match timeout(st, true) {
            Err(Bound::Passed(e)) => return Outcome::Unknown(e),
            Err(Bound::Refused(_)) | Ok(()) => {}
        }
        match st.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                reply.extend_from_slice(&chunk[..n]);
                if reply.len() as u64 > s.max_reply_bytes {
                    return Outcome::Unusable(format!("the reply from {address} is longer than {} bytes", s.max_reply_bytes));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Outcome::Unknown(format!("no reply from {address} within {} ms", s.timeout.as_millis()));
            }
            // TLS: the connection closed without the peer ending TLS, so the reply may
            // have been cut short by someone between: it happened, but cannot be trusted
            // to be whole.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Outcome::Unusable(format!("{address} closed the connection without ending TLS: the reply may be cut short"));
            }
            Err(e) => return Outcome::Unknown(format!("reading from {address}: {e}")),
        }
    }
    match String::from_utf8(reply) {
        Ok(t) => Outcome::Reply(t),
        Err(_) => Outcome::Unusable(format!("the reply from {address} is not UTF-8 text")),
    }
}

/// The listening side of `net.tcp.accept` and `net.tcp.reply`: each address listened
/// on, and each accepted connection not yet answered, by its id. A run's tasks share
/// one; nothing in it survives the process, so a connection open when a run stops is
/// lost.
#[derive(Default)]
pub struct Server {
    /// Each address listened on, with the TLS settings its clients get (None: plain TCP).
    listeners: std::collections::HashMap<String, (std::net::TcpListener, Option<Arc<rustls::ServerConfig>>)>,
    open: std::collections::HashMap<String, Conn>,
}

/// An accepted connection not yet answered.
enum Conn {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

/// What a `net.tcp.accept` or `net.tls.accept` grant says about listening (RUNTIME.md
/// section 12, Listening).
#[derive(Debug, Clone, Default)]
pub struct Listen {
    /// The most an `accept` waits for a client whose request it can use; None: no limit.
    pub wait: Option<Duration>,
    /// What a client that cannot be served (too slow, too long, not UTF-8, or, over TLS,
    /// a failed handshake) does to the `accept`: dropped, and the next one waited for (the
    /// default), or the accept fails.
    pub on_bad_client: OnBadClient,
    /// For `net.tls.accept`: the certificate and key clients are served with.
    pub tls: Option<Arc<rustls::ServerConfig>>,
}

/// The TLS settings for `net.tls.accept`: the certificate chain in the PEM file
/// `chain_path` (the server's certificate first) and the private key in the PEM file
/// `key_path`. Refused unless every certificate parses and is valid now, the key is one
/// rustls can sign with, and it is the key of the first certificate. The key file must
/// be a regular file of this user's that no one else can read or write. The safe default
/// protocol versions (TLS 1.2 and 1.3); no client certificates.
pub fn server_config(chain_path: &str, key_path: &str, now: std::time::SystemTime) -> Result<Arc<rustls::ServerConfig>, String> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use x509_cert::der::Decode;

    let pem = std::fs::read(chain_path).map_err(|e| format!("cannot read {chain_path}: {e}"))?;
    let mut chain = Vec::new();
    for item in ureq::tls::parse_pem(&pem) {
        match item.map_err(|e| format!("{chain_path}: {e}"))? {
            ureq::tls::PemItem::Certificate(c) => chain.push(CertificateDer::from(c.der().to_vec())),
            // A chain file is often readable by others; a key in it is a key disclosed.
            ureq::tls::PemItem::PrivateKey(_) => {
                return Err(format!("{chain_path} contains a private key; give the key in its own file (private_key)"))
            }
            _ => {}
        }
    }
    if chain.is_empty() {
        return Err(format!("{chain_path} contains no certificate"));
    }
    for (i, der) in chain.iter().enumerate() {
        let which = if i == 0 { format!("the certificate in {chain_path}") } else { format!("certificate {} in {chain_path}", i + 1) };
        let cert = x509_cert::Certificate::from_der(der).map_err(|e| format!("{which} is not a valid X.509 certificate: {e}"))?;
        let v = &cert.tbs_certificate.validity;
        if now < v.not_before.to_system_time() {
            return Err(format!("{which} is not valid until {}", v.not_before));
        }
        if now > v.not_after.to_system_time() {
            return Err(format!("{which} expired on {}", v.not_after));
        }
    }

    check_key_file(key_path)?;
    let key = PrivateKeyDer::from_pem_file(key_path).map_err(|e| format!("{key_path}: no private key read: {e}"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let signer = provider.key_provider.load_private_key(key.clone_key()).map_err(|e| format!("{key_path}: {e}"))?;
    let certified = rustls::sign::CertifiedKey::new(chain.clone(), signer);
    // An unknown answer is a refusal: a key that cannot be shown to match is not used.
    certified.keys_match().map_err(|e| format!("the key in {key_path} is not the key of the certificate in {chain_path}: {e}"))?;
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| format!("{chain_path}, {key_path}: {e}"))?;
    Ok(Arc::new(cfg))
}

/// Checks that the first certificate in the PEM file `chain_path` is valid for `host`
/// (a DNS name or an IP address), as a TLS client checks it: by its subject alternative
/// names (RFC 6125), with rustls's own check.
pub fn check_certificate_names(chain_path: &str, host: &str) -> Result<(), String> {
    use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName};
    let leaf = CertificateDer::pem_file_iter(chain_path)
        .map_err(|e| format!("cannot read {chain_path}: {e}"))?
        .next()
        .ok_or(format!("{chain_path} contains no certificate"))?
        .map_err(|e| format!("{chain_path}: {e}"))?;
    let name = ServerName::try_from(host.trim_start_matches('[').trim_end_matches(']').to_string()).map_err(|e| format!("{host}: {e}"))?;
    let parsed = rustls::server::ParsedCertificate::try_from(&leaf).map_err(|e| format!("the certificate in {chain_path}: {e}"))?;
    rustls::client::verify_server_name(&parsed, &name).map_err(|_| format!("the certificate in {chain_path} is not for {host}"))
}

/// A private key file: a regular file (not a link) owned by this user, which no one
/// else can read or write. Where file permissions cannot be checked, keys are refused.
#[cfg(unix)]
fn check_key_file(path: &str) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    if !m.file_type().is_file() {
        return Err(format!("{path} is not a regular file (a link to the key is not followed)"));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    if m.uid() != unsafe { libc::geteuid() } {
        return Err(format!("{path} does not belong to the user running staalc"));
    }
    if m.mode() & 0o077 != 0 {
        return Err(format!("{path} can be read or written by other users (mode {:o}); a private key must be 600 or 400", m.mode() & 0o777));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_key_file(path: &str) -> Result<(), String> {
    Err(format!("{path}: net.tls.accept is available on Linux and macOS only, where a key file's permissions can be checked"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnBadClient {
    #[default]
    Skip,
    Fail,
}

/// One `accept`: the request it took, or why none was taken; and why each client it
/// dropped was dropped (never what the client sent).
#[derive(Debug)]
pub struct Accepted {
    pub request: Result<String, String>,
    pub dropped: Vec<String>,
}

/// Waits for a client on `address` (listening on it from the first accept on) whose
/// request can be used, and reads it: until it ends with `until`, or until the client
/// closes its sending side. Keeps the connection, as `id`, for `reply`.
pub fn accept(server: &std::sync::Mutex<Server>, address: &str, until: Option<&str>, listen: &Listen, s: &Settings, id: &str) -> Accepted {
    let mut dropped = Vec::new();
    let listener = match listener(server, address, &listen.tls) {
        Ok(l) => l,
        Err(e) => return Accepted { request: Err(e), dropped },
    };
    let deadline = listen.wait.map(|w| Instant::now() + w);
    loop {
        let (st, peer) = match wait_for_client(&listener, address, deadline, listen.wait) {
            Ok(c) => c,
            Err(e) => return Accepted { request: Err(e), dropped },
        };
        match serve_client(st, &peer.to_string(), until, listen.tls.as_ref(), s) {
            Ok((conn, text)) => {
                let kept = server.lock().map_err(|_| "the open connections were lost".to_string());
                return Accepted { request: kept.map(|mut srv| { srv.open.insert(id.to_string(), conn); text }), dropped };
            }
            Err(why) if listen.on_bad_client == OnBadClient::Fail => return Accepted { request: Err(why), dropped },
            // Dropped: the connection closes as `st` goes, and the next client is waited for.
            Err(why) => dropped.push(why),
        }
    }
}

/// The listener for `address`, made at its first accept. It is non-blocking, so that a
/// wait can have a limit (`wait_for_client`); every clone shares that. Every accept on
/// one address must serve its clients the same way: all plain TCP, or all TLS with the
/// same grant's certificate, since they take clients from one queue.
fn listener(
    server: &std::sync::Mutex<Server>,
    address: &str,
    tls: &Option<Arc<rustls::ServerConfig>>,
) -> Result<std::net::TcpListener, String> {
    let mut srv = server.lock().map_err(|_| "the listening sockets were lost".to_string())?;
    if !srv.listeners.contains_key(address) {
        let l = std::net::TcpListener::bind(address).map_err(|e| format!("cannot listen on {address}: {e}"))?;
        l.set_nonblocking(true).map_err(|e| format!("{address}: {e}"))?;
        srv.listeners.insert(address.to_string(), (l, tls.clone()));
    }
    let (l, served) = &srv.listeners[address];
    match (served, tls) {
        (None, None) => {}
        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => {}
        (None, Some(_)) => return Err(format!("{address} is already listened on with plain TCP (net.tcp.accept) in this run")),
        (Some(_), None) => return Err(format!("{address} is already listened on with TLS (net.tls.accept) in this run")),
        (Some(_), Some(_)) => return Err(format!("{address} is already listened on with another grant's certificate in this run")),
    }
    l.try_clone().map_err(|e| format!("{address}: {e}"))
}

/// The next connection on `listener`, taken before `deadline` if there is one. Tasks
/// share the listener, so a client another task took first means waiting again.
///
/// The deadline is checked before every attempt, so once it passes no further client is
/// taken, even one already queued: an accept that keeps dropping bad clients still ends.
/// An accept therefore lasts at most the wait plus one request's read (`net.timeout_ms`).
fn wait_for_client(
    listener: &std::net::TcpListener,
    address: &str,
    deadline: Option<Instant>,
    wait: Option<Duration>,
) -> Result<(TcpStream, std::net::SocketAddr), String> {
    let passed = || format!("no client connected to {address} within {} ms", wait.map_or(0, |w| w.as_millis()));
    let left = || match deadline {
        None => Ok(None),
        Some(d) => d.checked_duration_since(Instant::now()).filter(|l| !l.is_zero()).map(Some).ok_or_else(passed),
    };
    loop {
        left()?;
        match listener.accept() {
            Ok((st, peer)) => {
                // An accepted socket inherits non-blocking mode on some systems (macOS);
                // its request is read with timeouts, which need blocking mode.
                st.set_nonblocking(false).map_err(|e| format!("{peer}: {e}"))?;
                return Ok((st, peer));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("accepting on {address}: {e}")),
        }
        readable(listener, left()?).map_err(|e| format!("waiting on {address}: {e}"))?;
    }
}

/// Waits until `listener` has a connection to accept, or `left` passes (None: no limit).
/// Returning does not promise a connection: another task may take it first.
#[cfg(unix)]
fn readable(listener: &std::net::TcpListener, left: Option<Duration>) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // poll(2) counts milliseconds in an int: round up, so a wait never ends early, and
    // cap at its largest value; a longer wait polls again.
    let ms = left.map_or(-1, |l| i32::try_from(l.as_nanos().div_ceil(1_000_000)).unwrap_or(i32::MAX));
    let mut fd = libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd, for a descriptor that stays open while `listener` lives.
    let r = unsafe { libc::poll(&mut fd, 1, ms) };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        // Interrupted: the caller tries to accept, then waits again.
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn readable(_listener: &std::net::TcpListener, left: Option<Duration>) -> std::io::Result<()> {
    std::thread::sleep(left.map_or(Duration::from_millis(10), |l| l.min(Duration::from_millis(10))));
    Ok(())
}

/// Serves one client: over TLS, the handshake, then its request (`read_request`), all
/// within one `net.timeout_ms`. The connection is returned to be answered.
fn serve_client(
    st: TcpStream,
    peer: &str,
    until: Option<&str>,
    tls: Option<&Arc<rustls::ServerConfig>>,
    s: &Settings,
) -> Result<(Conn, String), String> {
    let deadline = Instant::now() + s.timeout;
    let Some(cfg) = tls else {
        let mut st = st;
        let text = read_request(&mut st, |c| c, peer, until, s, deadline)?;
        return Ok((Conn::Plain(st), text));
    };
    let conn = rustls::ServerConnection::new(cfg.clone()).map_err(|e| format!("TLS with {peer}: {e}"))?;
    let mut tls = rustls::StreamOwned::new(conn, st);
    while tls.conn.is_handshaking() {
        let d = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| format!("no TLS handshake with {peer} within {} ms", s.timeout.as_millis()))?;
        tls.sock.set_read_timeout(Some(d)).and_then(|_| tls.sock.set_write_timeout(Some(d))).map_err(|e| format!("{peer}: {e}"))?;
        tls.conn.complete_io(&mut tls.sock).map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                format!("no TLS handshake with {peer} within {} ms", s.timeout.as_millis())
            }
            _ => format!("the TLS handshake with {peer} failed: {e}"),
        })?;
    }
    let text = read_request(&mut tls, |t| &t.sock, peer, until, s, deadline)?;
    Ok((Conn::Tls(Box::new(tls)), text))
}

/// Reads a client's request: until it ends with `until`, or until the client closes its
/// sending side (over TLS, by ending TLS); by `deadline`, within `net.max_response_bytes`,
/// as UTF-8 text. `sock` gives the socket, to set timeouts on. The reason a request
/// cannot be used names the client, never what it sent.
fn read_request<S: Read>(
    st: &mut S,
    sock: impl Fn(&S) -> &TcpStream,
    peer: &str,
    until: Option<&str>,
    s: &Settings,
    deadline: Instant,
) -> Result<String, String> {
    let mut bounded = false;
    let mut request: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if let Some(u) = until
            && request.ends_with(u.as_bytes())
        {
            break;
        }
        let d = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| format!("the request from {peer} did not arrive within {} ms", s.timeout.as_millis()))?;
        // The first timeout must be set; a later one the socket refuses (macOS, once
        // the client has closed) leaves the earlier, longer one bounding the read.
        match sock(st).set_read_timeout(Some(d)) {
            Ok(()) => bounded = true,
            Err(e) if !bounded => return Err(format!("{peer}: {e}")),
            Err(_) => {}
        }
        match st.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                request.extend_from_slice(&chunk[..n]);
                if request.len() as u64 > s.max_reply_bytes {
                    return Err(format!("the request from {peer} is longer than {} bytes", s.max_reply_bytes));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err(format!("the request from {peer} did not arrive within {} ms", s.timeout.as_millis()));
            }
            // TLS: the connection closed without the client ending TLS, so the request may
            // have been cut short by someone between.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(format!("{peer} closed the connection without ending TLS: its request may be cut short"));
            }
            Err(e) => return Err(format!("reading the request from {peer}: {e}")),
        }
    }
    String::from_utf8(request).map_err(|_| format!("the request from {peer} is not UTF-8 text"))
}

/// Sends `data` on connection `id` and closes it: over TLS when `tls` (`net.tls.reply`),
/// ending TLS (close_notify) before closing, and refused for a connection accepted the
/// other way, which is left open.
pub fn reply(server: &std::sync::Mutex<Server>, id: &str, data: &[u8], s: &Settings, tls: bool) -> Result<(), String> {
    let st = {
        let mut srv = server.lock().map_err(|_| "the open connections were lost".to_string())?;
        match srv.open.get(id) {
            Some(Conn::Plain(_)) if tls => {
                return Err(format!("connection {id} was accepted with plain TCP (net.tcp.accept); answer it with net.tcp.reply"))
            }
            Some(Conn::Tls(_)) if !tls => {
                return Err(format!("connection {id} was accepted over TLS (net.tls.accept); answer it with net.tls.reply"))
            }
            _ => srv.open.remove(id),
        }
    };
    let Some(conn) = st else {
        return Err(format!(
            "connection {id} is not open: it was answered already, or the run stopped since it was accepted (a reply \
             made just before the run stopped may have reached the client)"
        ));
    };
    match conn {
        Conn::Plain(mut st) => {
            st.set_write_timeout(Some(s.timeout)).map_err(|e| format!("connection {id}: {e}"))?;
            st.write_all(data).and_then(|_| st.flush()).map_err(|e| format!("replying on connection {id}: {e}"))?;
            let _ = st.shutdown(Shutdown::Both);
        }
        Conn::Tls(mut t) => {
            t.sock.set_write_timeout(Some(s.timeout)).map_err(|e| format!("connection {id}: {e}"))?;
            t.write_all(data).and_then(|_| t.flush()).map_err(|e| format!("replying on connection {id}: {e}"))?;
            t.conn.send_close_notify();
            t.flush().map_err(|e| format!("ending TLS on connection {id}: {e}"))?;
            let _ = t.sock.shutdown(Shutdown::Both);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the tests' local servers need: the loopback addresses, and no others.
    fn local() -> super::super::addresses::Policy {
        super::super::addresses::Policy::from_grant(Some(&serde_json::json!(["127.0.0.1", "::1"])), None, "test").unwrap()
    }

    #[test]
    fn an_address_the_grant_may_not_reach_is_not_connected_to() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        match exchange(&addr, b"x", None, &settings(2000), false, &Default::default()) {
            Outcome::NotSent(m) => assert!(m.contains("not a public address"), "{m}"),
            other => panic!("{other:?}"),
        }
        l.set_nonblocking(true).unwrap();
        assert!(l.accept().is_err(), "nothing connected");
    }
    use std::net::TcpListener;

    fn settings(ms: u64) -> Settings {
        Settings { connect_timeout: Duration::from_millis(ms), timeout: Duration::from_millis(ms), max_reply_bytes: 64, tls: tls_config(&[]).unwrap() }
    }

    /// A one-connection server on a free port: `serve` gets the bytes it read (to EOF,
    /// or `read` bytes) and writes its answer.
    fn server(read: Option<usize>, answer: &'static [u8], close: bool) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let h = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = Vec::new();
            match read {
                None => {
                    c.read_to_end(&mut got).unwrap();
                }
                Some(n) => {
                    got.resize(n, 0);
                    c.read_exact(&mut got).unwrap();
                }
            }
            c.write_all(answer).unwrap();
            if !close {
                std::thread::sleep(Duration::from_millis(400));
            }
            got
        });
        (addr, h)
    }

    #[test]
    fn a_request_is_sent_whole_and_the_reply_read_to_the_end() {
        let (addr, h) = server(None, b"pong\n", true);
        assert_eq!(exchange(&addr, b"ping", None, &settings(2000), false, &local()), Outcome::Reply("pong\n".into()));
        assert_eq!(h.join().unwrap(), b"ping");
    }

    #[test]
    fn a_delimiter_ends_the_reply_on_a_connection_left_open() {
        let (addr, h) = server(Some(6), b"+OK\r\n", false);
        let start = Instant::now();
        assert_eq!(exchange(&addr, b"PING\r\n", Some("\r\n"), &settings(2000), false, &local()), Outcome::Reply("+OK\r\n".into()));
        assert!(start.elapsed() < Duration::from_millis(300), "it waited for the close");
        h.join().unwrap();
    }

    #[test]
    fn what_is_known_is_reported_exactly() {
        // Nobody listening: certainly not sent.
        let free = crate::test_ports::address();
        assert!(matches!(exchange(&free, b"x", None, &settings(2000), false, &local()), Outcome::NotSent(_)));
        assert!(matches!(exchange("no-such-host.invalid:80", b"x", None, &settings(2000), false, &local()), Outcome::NotSent(_)));
        // A peer that never answers: it may have acted on the data.
        let (addr, h) = server(Some(1), b"", false);
        assert!(matches!(exchange(&addr, b"x", Some("\n"), &settings(150), false, &local()), Outcome::Unknown(m) if m.contains("no reply")));
        h.join().unwrap();
        // It answered, but the answer cannot be used.
        let (addr, _) = server(None, &[0xff, 0xfe], true);
        assert!(matches!(exchange(&addr, b"x", None, &settings(2000), false, &local()), Outcome::Unusable(m) if m.contains("UTF-8")));
        let (addr, _) = server(None, &[b'a'; 65], true);
        assert!(matches!(exchange(&addr, b"x", None, &settings(2000), false, &local()), Outcome::Unusable(m) if m.contains("longer than 64")));
    }

    #[test]
    fn an_accept_ends_at_its_wait_limit_while_bad_clients_keep_arriving() {
        // Ten clients already queued, each sending nothing, so each is dropped only when
        // its read passes net.timeout_ms (50 ms). Without the limit checked before every
        // attempt, the accept would drop all ten (500 ms or more) before it ended.
        let free = crate::test_ports::address();
        let server = std::sync::Mutex::new(Server::default());
        listener(&server, &free, &None).unwrap();
        let clients: Vec<TcpStream> = (0..10).map(|_| TcpStream::connect(&free).unwrap()).collect();
        let listen = Listen { wait: Some(Duration::from_millis(100)), on_bad_client: OnBadClient::Skip, tls: None };
        let start = Instant::now();
        let got = accept(&server, &free, None, &listen, &settings(50), "c1");
        let took = start.elapsed();
        assert!(matches!(&got.request, Err(m) if m.contains("within 100 ms")), "{:?}", got.request);
        assert!(!got.dropped.is_empty() && got.dropped.len() < 10, "dropped {}", got.dropped.len());
        assert!(got.dropped.iter().all(|d| d.contains("did not arrive within 50 ms")), "{:?}", got.dropped);
        // At most the wait plus one read, with room for a loaded machine.
        assert!(took < Duration::from_millis(400), "took {took:?}");
        drop(clients);
    }

    /// net.tls.accept: key files are checked with Unix permissions.
    #[cfg(unix)]
    mod tls_listening {
        use super::*;

        // ---- net.tls.accept ----

        use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};

        /// A test authority, and a folder for the files a grant names.
        struct Pki {
            ca: CertifiedIssuer<'static, KeyPair>,
            dir: std::path::PathBuf,
        }

        impl Pki {
            fn new(name: &str) -> Pki {
                let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
                p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
                let dir = std::env::temp_dir().join(format!("staalc-tls-{}-{name}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                Pki { ca: CertifiedIssuer::self_signed(p, KeyPair::generate().unwrap()).unwrap(), dir }
            }

            /// A certificate for localhost (valid in the years given, or rcgen's wide default)
            /// and its key; returns (chain PEM, key PEM).
            fn leaf(&self, years: Option<(i32, i32)>) -> (String, String) {
                let mut p = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
                if let Some((from, to)) = years {
                    p.not_before = rcgen::date_time_ymd(from, 1, 1);
                    p.not_after = rcgen::date_time_ymd(to, 1, 1);
                }
                let key = KeyPair::generate().unwrap();
                (p.signed_by(&key, &self.ca).unwrap().pem(), key.serialize_pem())
            }

            /// Writes `chain` and `key` (the key with `mode`); returns their paths.
            fn files(&self, chain: &str, key: &str, mode: u32) -> (String, String) {
                use std::os::unix::fs::PermissionsExt;
                let c = self.dir.join("chain.pem");
                let k = self.dir.join("key.pem");
                let _ = std::fs::remove_file(&k);
                std::fs::write(&c, chain).unwrap();
                std::fs::write(&k, key).unwrap();
                std::fs::set_permissions(&k, std::fs::Permissions::from_mode(mode)).unwrap();
                (c.to_string_lossy().into_owned(), k.to_string_lossy().into_owned())
            }

            fn client(&self) -> Arc<rustls::ClientConfig> {
                let mut roots = rustls::RootCertStore::empty();
                roots.add(self.ca.der().clone()).unwrap();
                Arc::new(
                    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                        .with_safe_default_protocol_versions()
                        .unwrap()
                        .with_root_certificates(roots)
                        .with_no_client_auth(),
                )
            }
        }

        fn refused(r: Result<Arc<rustls::ServerConfig>, String>) -> String {
            r.expect_err("refused")
        }

        #[test]
        fn a_certificate_and_its_key_are_checked_before_listening() {
            let now = std::time::SystemTime::now();
            let pki = Pki::new("checks");
            let (chain, key) = pki.leaf(None);
            let (c, k) = pki.files(&chain, &key, 0o600);
            assert!(server_config(&c, &k, now).is_ok());
            let (c, k) = pki.files(&chain, &key, 0o400);
            assert!(server_config(&c, &k, now).is_ok());

            // Expired, and not yet valid.
            let (old, old_key) = pki.leaf(Some((2000, 2001)));
            let (c, k) = pki.files(&old, &old_key, 0o600);
            assert!(refused(server_config(&c, &k, now)).contains("expired on 2001-01-01"), "{}", refused(server_config(&c, &k, now)));
            let (soon, soon_key) = pki.leaf(Some((2999, 3000)));
            let (c, k) = pki.files(&soon, &soon_key, 0o600);
            assert!(refused(server_config(&c, &k, now)).contains("is not valid until 2999-01-01"));
            // A certificate later in the chain counts too: an expired intermediate.
            let (c, k) = pki.files(&format!("{chain}{old}"), &key, 0o600);
            assert!(refused(server_config(&c, &k, now)).contains("certificate 2 in"), "{}", refused(server_config(&c, &k, now)));

            // Another certificate's key.
            let (_, other_key) = pki.leaf(None);
            let (c, k) = pki.files(&chain, &other_key, 0o600);
            assert!(refused(server_config(&c, &k, now)).contains("is not the key of the certificate"));

            // A key others can read or write, a link to the key, and a key in the chain file.
            for mode in [0o640, 0o604, 0o660, 0o644] {
                let (c, k) = pki.files(&chain, &key, mode);
                assert!(refused(server_config(&c, &k, now)).contains("can be read or written by other users"), "{mode:o}");
            }
            let (c, k) = pki.files(&chain, &key, 0o600);
            let link = pki.dir.join("link.pem");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&k, &link).unwrap();
            assert!(refused(server_config(&c, link.to_str().unwrap(), now)).contains("not a regular file"));
            let (c, k) = pki.files(&format!("{chain}{key}"), &key, 0o600);
            assert!(refused(server_config(&c, &k, now)).contains("contains a private key"));

            // Files without what they should hold.
            let (c, k) = pki.files("not a certificate\n", &key, 0o600);
            assert!(refused(server_config(&c, &k, now)).contains("contains no certificate"));
            let (c, k) = pki.files(&chain, "not a key\n", 0o600);
            assert!(refused(server_config(&c, &k, now)).contains("no private key read"));
            assert!(refused(server_config("/no/such/chain.pem", &k, now)).contains("cannot read /no/such/chain.pem"));
            let _ = std::fs::remove_dir_all(&pki.dir);
        }

        /// A TLS client: connects, verifies the certificate for localhost, sends `data`,
        /// ends TLS when `end`, and returns what it read (or the error that ended it).
        fn tls_client(addr: String, cfg: Arc<rustls::ClientConfig>, data: &'static [u8], end: bool) -> std::thread::JoinHandle<Result<Vec<u8>, String>> {
            std::thread::spawn(move || {
                let st = TcpStream::connect(&addr).map_err(|e| e.to_string())?;
                st.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
                let mut t = rustls::StreamOwned::new(rustls::ClientConnection::new(cfg, name).unwrap(), st);
                t.write_all(data).map_err(|e| e.to_string())?;
                t.flush().map_err(|e| e.to_string())?;
                if end {
                    t.conn.send_close_notify();
                    t.flush().map_err(|e| e.to_string())?;
                }
                let mut got = Vec::new();
                t.read_to_end(&mut got).map_err(|e| e.to_string())?;
                Ok(got)
            })
        }

        fn tls_listen(pki: &Pki) -> (String, Listen, std::sync::Mutex<Server>) {
            let (chain, key) = pki.leaf(None);
            let (c, k) = pki.files(&chain, &key, 0o600);
            let cfg = server_config(&c, &k, std::time::SystemTime::now()).unwrap();
            let addr = crate::test_ports::address();
            let server = std::sync::Mutex::new(Server::default());
            listener(&server, &addr, &Some(cfg.clone())).unwrap();
            (addr, Listen { wait: Some(Duration::from_secs(10)), on_bad_client: OnBadClient::Skip, tls: Some(cfg) }, server)
        }

        #[test]
        fn a_tls_client_is_served_and_answered_over_tls() {
            let pki = Pki::new("served");
            let (addr, listen, server) = tls_listen(&pki);
            // Read to its end: the client ends TLS after its request.
            let c = tls_client(addr.clone(), pki.client(), b"hello", true);
            let got = accept(&server, &addr, None, &listen, &settings(5000), "c1");
            assert_eq!(got.request, Ok("hello".to_string()));
            assert!(got.dropped.is_empty());
            // net.tcp.reply refuses it and leaves it open; net.tls.reply answers it.
            let wrong = reply(&server, "c1", b"no", &settings(5000), false).unwrap_err();
            assert!(wrong.contains("answer it with net.tls.reply"), "{wrong}");
            reply(&server, "c1", b"world", &settings(5000), true).unwrap();
            // The client read the answer to a proper end of TLS (close_notify).
            assert_eq!(c.join().unwrap(), Ok(b"world".to_vec()));

            // To a delimiter, on a connection the client keeps open.
            let c = tls_client(addr.clone(), pki.client(), b"line\n", false);
            let got = accept(&server, &addr, Some("\n"), &listen, &settings(5000), "c2");
            assert_eq!(got.request, Ok("line\n".to_string()));
            reply(&server, "c2", b"+line\n", &settings(5000), true).unwrap();
            assert_eq!(c.join().unwrap(), Ok(b"+line\n".to_vec()));
            let _ = std::fs::remove_dir_all(&pki.dir);
        }

        #[test]
        fn a_client_that_cannot_be_served_over_tls_is_dropped() {
            let pki = Pki::new("dropped");
            let (addr, listen, server) = tls_listen(&pki);
            let strict = Listen { on_bad_client: OnBadClient::Fail, ..listen.clone() };
            let fails = |what: &str| {
                let got = accept(&server, &addr, None, &strict, &settings(1000), "c");
                got.request.expect_err(what)
            };

            // Plain text where a TLS handshake should be.
            let plain = raw(addr.clone(), b"GET / HTTP/1.0\r\n\r\n");
            let e = fails("plain text");
            assert!(e.contains("the TLS handshake with 127.0.0.1:") && e.contains("failed"), "{e}");
            plain.join().unwrap();

            // A client that does not trust the certificate ends the handshake.
            let stranger = Pki::new("stranger");
            let c = tls_client(addr.clone(), stranger.client(), b"x", true);
            let e = fails("untrusted");
            assert!(e.contains("the TLS handshake with"), "{e}");
            assert!(c.join().unwrap().is_err());
            let _ = std::fs::remove_dir_all(&stranger.dir);

            // A client that connects and never begins the handshake.
            let silent = std::thread::spawn({
                let addr = addr.clone();
                move || {
                    let c = TcpStream::connect(&addr).unwrap();
                    std::thread::sleep(Duration::from_millis(1500));
                    drop(c);
                }
            });
            let e = fails("silent");
            assert!(e.contains("no TLS handshake with") && e.contains("within 1000 ms"), "{e}");
            silent.join().unwrap();

            // A client that closes its connection without ending TLS: its request may be cut
            // short, so it is not used, though it reads to the end.
            let cut = std::thread::spawn({
                let (addr, cfg) = (addr.clone(), pki.client());
                move || {
                    let st = TcpStream::connect(&addr).unwrap();
                    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
                    let mut t = rustls::StreamOwned::new(rustls::ClientConnection::new(cfg, name).unwrap(), st);
                    t.write_all(b"partial").unwrap();
                    t.flush().unwrap();
                    t.sock.shutdown(Shutdown::Write).unwrap();
                    let mut sink = Vec::new();
                    let _ = t.sock.read_to_end(&mut sink);
                }
            });
            let e = fails("cut");
            assert!(e.contains("closed the connection without ending TLS"), "{e}");
            cut.join().unwrap();

            // By default, a bad client is dropped with its reason and the next one served.
            let plain = raw(addr.clone(), b"hello\n");
            let good = std::thread::spawn({
                let (addr, cfg) = (addr.clone(), pki.client());
                move || {
                    std::thread::sleep(Duration::from_millis(200));
                    tls_client(addr, cfg, b"fine", true).join().unwrap()
                }
            });
            let got = accept(&server, &addr, None, &listen, &settings(1000), "ok");
            assert_eq!(got.request, Ok("fine".to_string()));
            assert_eq!(got.dropped.len(), 1, "{:?}", got.dropped);
            assert!(got.dropped[0].contains("the TLS handshake with"), "{:?}", got.dropped);
            reply(&server, "ok", b"done", &settings(1000), true).unwrap();
            assert_eq!(good.join().unwrap(), Ok(b"done".to_vec()));
            plain.join().unwrap();
            let _ = std::fs::remove_dir_all(&pki.dir);
        }

        /// A plain TCP client sending `data` and reading until the server closes.
        fn raw(addr: String, data: &'static [u8]) -> std::thread::JoinHandle<()> {
            std::thread::spawn(move || {
                let mut c = TcpStream::connect(&addr).unwrap();
                // Set before writing: once the server has dropped this client, macOS
                // refuses socket options (EINVAL).
                c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                c.write_all(data).unwrap();
                let mut got = Vec::new();
                let _ = c.read_to_end(&mut got);
            })
        }

        #[test]
        fn one_address_serves_its_clients_one_way() {
            let pki = Pki::new("one-way");
            let (addr, listen, server) = tls_listen(&pki);
            let e = listener(&server, &addr, &None).unwrap_err();
            assert!(e.contains("already listened on with TLS"), "{e}");
            let (chain, key) = pki.leaf(None);
            let (c, k) = pki.files(&chain, &key, 0o600);
            let other = server_config(&c, &k, std::time::SystemTime::now()).unwrap();
            let e = listener(&server, &addr, &Some(other)).unwrap_err();
            assert!(e.contains("another grant's certificate"), "{e}");
            assert!(listener(&server, &addr, &listen.tls).is_ok());
            let plain = crate::test_ports::address();
            listener(&server, &plain, &None).unwrap();
            let e = listener(&server, &plain, &listen.tls).unwrap_err();
            assert!(e.contains("already listened on with plain TCP"), "{e}");
            let _ = std::fs::remove_dir_all(&pki.dir);
        }
    }
}
