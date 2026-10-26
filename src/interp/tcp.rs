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
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Settings {
    pub connect_timeout: Duration,
    /// For the whole exchange: connecting, sending and every read.
    pub timeout: Duration,
    pub max_reply_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Reply(String),
    NotSent(String),
    Unknown(String),
    Unusable(String),
}

pub fn exchange(address: &str, data: &[u8], until: Option<&str>, s: &Settings) -> Outcome {
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
    let Some(mut st) = stream else { return Outcome::NotSent(last) };
    // From here on the data may reach the peer.
    let left = |what: &str| deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(|| format!("no {what} within {} ms", s.timeout.as_millis()));
    let _ = st.set_nodelay(true);
    match left("time to send").and_then(|d| st.set_write_timeout(Some(d)).map_err(|e| e.to_string())) {
        Ok(()) => {}
        Err(e) => return Outcome::Unknown(e),
    }
    if let Err(e) = st.write_all(data).and_then(|_| st.flush()) {
        return Outcome::Unknown(format!("sending to {address}: {e}"));
    }
    if until.is_none()
        && let Err(e) = st.shutdown(Shutdown::Write)
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
        let d = match left("reply") {
            Ok(d) => d,
            Err(e) => return Outcome::Unknown(format!("{address}: {e}")),
        };
        if let Err(e) = st.set_read_timeout(Some(d)) {
            return Outcome::Unknown(format!("{address}: {e}"));
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
        Settings { connect_timeout: Duration::from_millis(ms), timeout: Duration::from_millis(ms), max_reply_bytes: 64 }
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
        assert_eq!(exchange(&addr, b"ping", None, &settings(2000)), Outcome::Reply("pong\n".into()));
        assert_eq!(h.join().unwrap(), b"ping");
    }

    #[test]
    fn a_delimiter_ends_the_reply_on_a_connection_left_open() {
        let (addr, h) = server(Some(6), b"+OK\r\n", false);
        let start = Instant::now();
        assert_eq!(exchange(&addr, b"PING\r\n", Some("\r\n"), &settings(2000)), Outcome::Reply("+OK\r\n".into()));
        assert!(start.elapsed() < Duration::from_millis(300), "it waited for the close");
        h.join().unwrap();
    }

    #[test]
    fn what_is_known_is_reported_exactly() {
        // Nobody listening: certainly not sent.
        let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
        assert!(matches!(exchange(&free, b"x", None, &settings(2000)), Outcome::NotSent(_)));
        assert!(matches!(exchange("no-such-host.invalid:80", b"x", None, &settings(2000)), Outcome::NotSent(_)));
        // A peer that never answers: it may have acted on the data.
        let (addr, h) = server(Some(1), b"", false);
        assert!(matches!(exchange(&addr, b"x", Some("\n"), &settings(150)), Outcome::Unknown(m) if m.contains("no reply")));
        h.join().unwrap();
        // It answered, but the answer cannot be used.
        let (addr, _) = server(None, &[0xff, 0xfe], true);
        assert!(matches!(exchange(&addr, b"x", None, &settings(2000)), Outcome::Unusable(m) if m.contains("UTF-8")));
        let (addr, _) = server(None, &[b'a'; 65], true);
        assert!(matches!(exchange(&addr, b"x", None, &settings(2000)), Outcome::Unusable(m) if m.contains("longer than 64")));
    }
}
