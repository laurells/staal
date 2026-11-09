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

/// One exchange, over TLS when `tls`.
pub fn exchange(address: &str, data: &[u8], until: Option<&str>, s: &Settings, tls: bool) -> Outcome {
    let deadline = Instant::now() + s.timeout;
    let addrs: Vec<_> = match address.to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => return Outcome::NotSent(format!("cannot resolve {address}: {e}")),
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
    listeners: std::collections::HashMap<String, std::net::TcpListener>,
    open: std::collections::HashMap<String, TcpStream>,
}

/// What a `net.tcp.accept` grant says about waiting (RUNTIME.md section 12, Listening).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Listen {
    /// The most an `accept` waits for a client whose request it can use; None: no limit.
    pub wait: Option<Duration>,
    /// What a client that cannot be served (too slow, too long, not UTF-8) does to the
    /// `accept`: dropped, and the next one waited for (the default), or the accept fails.
    pub on_bad_client: OnBadClient,
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
    let listener = match listener(server, address) {
        Ok(l) => l,
        Err(e) => return Accepted { request: Err(e), dropped },
    };
    let deadline = listen.wait.map(|w| Instant::now() + w);
    loop {
        let (mut st, peer) = match wait_for_client(&listener, address, deadline, listen.wait) {
            Ok(c) => c,
            Err(e) => return Accepted { request: Err(e), dropped },
        };
        match read_request(&mut st, &peer.to_string(), until, s) {
            Ok(text) => {
                let kept = server.lock().map_err(|_| "the open connections were lost".to_string());
                return Accepted { request: kept.map(|mut srv| { srv.open.insert(id.to_string(), st); text }), dropped };
            }
            Err(why) if listen.on_bad_client == OnBadClient::Fail => return Accepted { request: Err(why), dropped },
            // Dropped: the connection closes as `st` goes, and the next client is waited for.
            Err(why) => dropped.push(why),
        }
    }
}

/// The listener for `address`, made at its first accept. It is non-blocking, so that a
/// wait can have a limit (`wait_for_client`); every clone shares that.
fn listener(server: &std::sync::Mutex<Server>, address: &str) -> Result<std::net::TcpListener, String> {
    let mut srv = server.lock().map_err(|_| "the listening sockets were lost".to_string())?;
    if !srv.listeners.contains_key(address) {
        let l = std::net::TcpListener::bind(address).map_err(|e| format!("cannot listen on {address}: {e}"))?;
        l.set_nonblocking(true).map_err(|e| format!("{address}: {e}"))?;
        srv.listeners.insert(address.to_string(), l);
    }
    srv.listeners[address].try_clone().map_err(|e| format!("{address}: {e}"))
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

/// Reads a client's request: until it ends with `until`, or until the client closes its
/// sending side; within `net.timeout_ms` and `net.max_response_bytes`, as UTF-8 text.
/// The reason it cannot be used names the client, never what it sent.
fn read_request(st: &mut TcpStream, peer: &str, until: Option<&str>, s: &Settings) -> Result<String, String> {
    let deadline = Instant::now() + s.timeout;
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
        match st.set_read_timeout(Some(d)) {
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
            Err(e) => return Err(format!("reading the request from {peer}: {e}")),
        }
    }
    String::from_utf8(request).map_err(|_| format!("the request from {peer} is not UTF-8 text"))
}

/// Sends `data` on connection `id` and closes it.
pub fn reply(server: &std::sync::Mutex<Server>, id: &str, data: &[u8], s: &Settings) -> Result<(), String> {
    let st = server.lock().map_err(|_| "the open connections were lost".to_string())?.open.remove(id);
    let Some(mut st) = st else {
        return Err(format!(
            "connection {id} is not open: it was answered already, or the run stopped since it was accepted (a reply \
             made just before the run stopped may have reached the client)"
        ));
    };
    st.set_write_timeout(Some(s.timeout)).map_err(|e| format!("connection {id}: {e}"))?;
    st.write_all(data).and_then(|_| st.flush()).map_err(|e| format!("replying on connection {id}: {e}"))?;
    let _ = st.shutdown(Shutdown::Both);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(exchange(&addr, b"ping", None, &settings(2000), false), Outcome::Reply("pong\n".into()));
        assert_eq!(h.join().unwrap(), b"ping");
    }

    #[test]
    fn a_delimiter_ends_the_reply_on_a_connection_left_open() {
        let (addr, h) = server(Some(6), b"+OK\r\n", false);
        let start = Instant::now();
        assert_eq!(exchange(&addr, b"PING\r\n", Some("\r\n"), &settings(2000), false), Outcome::Reply("+OK\r\n".into()));
        assert!(start.elapsed() < Duration::from_millis(300), "it waited for the close");
        h.join().unwrap();
    }

    #[test]
    fn what_is_known_is_reported_exactly() {
        // Nobody listening: certainly not sent.
        let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
        assert!(matches!(exchange(&free, b"x", None, &settings(2000), false), Outcome::NotSent(_)));
        assert!(matches!(exchange("no-such-host.invalid:80", b"x", None, &settings(2000), false), Outcome::NotSent(_)));
        // A peer that never answers: it may have acted on the data.
        let (addr, h) = server(Some(1), b"", false);
        assert!(matches!(exchange(&addr, b"x", Some("\n"), &settings(150), false), Outcome::Unknown(m) if m.contains("no reply")));
        h.join().unwrap();
        // It answered, but the answer cannot be used.
        let (addr, _) = server(None, &[0xff, 0xfe], true);
        assert!(matches!(exchange(&addr, b"x", None, &settings(2000), false), Outcome::Unusable(m) if m.contains("UTF-8")));
        let (addr, _) = server(None, &[b'a'; 65], true);
        assert!(matches!(exchange(&addr, b"x", None, &settings(2000), false), Outcome::Unusable(m) if m.contains("longer than 64")));
    }

    #[test]
    fn an_accept_ends_at_its_wait_limit_while_bad_clients_keep_arriving() {
        // Ten clients already queued, each sending nothing, so each is dropped only when
        // its read passes net.timeout_ms (50 ms). Without the limit checked before every
        // attempt, the accept would drop all ten (500 ms or more) before it ended.
        let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
        let server = std::sync::Mutex::new(Server::default());
        listener(&server, &free).unwrap();
        let clients: Vec<TcpStream> = (0..10).map(|_| TcpStream::connect(&free).unwrap()).collect();
        let listen = Listen { wait: Some(Duration::from_millis(100)), on_bad_client: OnBadClient::Skip };
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
}
