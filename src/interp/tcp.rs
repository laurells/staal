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
    let timeout = |st: &S, read: bool| -> Result<(), String> {
        let d = left().ok_or_else(|| format!("no reply from {address} within {} ms", s.timeout.as_millis()))?;
        let so = sock(st).map_err(|e| format!("{address}: {e}"))?;
        if read { so.set_read_timeout(Some(d)) } else { so.set_write_timeout(Some(d)) }.map_err(|e| format!("{address}: {e}"))
    };
    // From here on the data may reach the peer.
    if let Err(e) = timeout(st, false) {
        return Outcome::Unknown(e);
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
        if let Err(e) = timeout(st, true) {
            return Outcome::Unknown(e);
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
}
