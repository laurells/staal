// The stdio transport of `staalc mcp` (docs/MCP.md section 3): newline-delimited
// JSON-RPC 2.0 on descriptors the server takes over before anything else runs, with
// limits on what it reads and one lock around every message it writes.

use serde_json::{json, Value as Json};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// The longest line read; a longer one is refused without being parsed.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// The deepest nesting of arrays and objects accepted in a message.
pub const MAX_DEPTH: usize = 64;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const UNSUPPORTED_VERSION: i64 = -32022;
/// The HTTP headers do not match the body, or a required one is missing or malformed
/// (Streamable HTTP, 2026-07-28).
pub const HEADER_MISMATCH: i64 = -32020;

/// Where a request's messages go: the stdio writer, or one HTTP request's answer.
pub trait Sink: Send + Sync {
    /// Sends one message.
    fn send(&self, msg: &Json);

    fn result(&self, id: &Json, result: Json) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn error(&self, id: &Json, code: i64, message: &str, data: Option<Json>) {
        self.send(&error_message(id, code, message, data));
    }
}

/// A JSON-RPC error response.
pub fn error_message(id: &Json, code: i64, message: &str, data: Option<Json>) -> Json {
    let mut e = json!({ "code": code, "message": message });
    if let Some(d) = data {
        e["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": e })
}

/// Takes the process's standard input and output for the protocol: duplicates
/// descriptors 0 and 1 to new ones only the protocol uses, then points 0 at /dev/null
/// and 1 at standard error. Whatever else writes to standard output (a panic message,
/// a C library) lands on standard error, and nothing else can read the client's
/// messages.
pub fn take_stdio() -> Result<(File, File), String> {
    use std::os::fd::FromRawFd;
    let err = |what: &str| format!("cannot take standard input and output for the protocol ({what}): {}", std::io::Error::last_os_error());
    // SAFETY: plain descriptor calls on descriptors this process owns; each result is
    // checked before use, and each new descriptor is owned by exactly one File.
    unsafe {
        let input = libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 3);
        if input < 0 {
            return Err(err("duplicating standard input"));
        }
        let output = libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3);
        if output < 0 {
            libc::close(input);
            return Err(err("duplicating standard output"));
        }
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
        if null < 0 {
            libc::close(input);
            libc::close(output);
            return Err(err("opening /dev/null"));
        }
        let ok = libc::dup2(null, 0) >= 0 && libc::dup2(2, 1) >= 0;
        libc::close(null);
        if !ok {
            libc::close(input);
            libc::close(output);
            return Err(err("redirecting descriptors 0 and 1"));
        }
        Ok((File::from_raw_fd(input), File::from_raw_fd(output)))
    }
}

/// One line from the client.
#[derive(Debug, PartialEq)]
pub enum Line {
    Text(Vec<u8>),
    /// Longer than MAX_LINE_BYTES: read to its end and dropped.
    TooLong,
}

pub struct Reader {
    inner: BufReader<File>,
}

impl Reader {
    pub fn new(f: File) -> Reader {
        Reader { inner: BufReader::with_capacity(64 * 1024, f) }
    }

    /// The next line, without its newline; None at end of input. A last line without
    /// a newline is still a line.
    pub fn next_line(&mut self) -> std::io::Result<Option<Line>> {
        read_line(&mut self.inner, MAX_LINE_BYTES)
    }
}

pub fn read_line(r: &mut impl BufRead, max: usize) -> std::io::Result<Option<Line>> {
    let mut line = Vec::new();
    let mut too_long = false;
    let mut any = false;
    loop {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            return Ok(if !any {
                None
            } else if too_long {
                Some(Line::TooLong)
            } else {
                Some(Line::Text(line))
            });
        }
        any = true;
        let (part, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (&buf[..i], Some(i + 1)),
            None => (buf, None),
        };
        if !too_long {
            if line.len() + part.len() > max {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(part);
            }
        }
        let used = done.unwrap_or(buf.len());
        r.consume(used);
        if done.is_some() {
            return Ok(Some(if too_long { Line::TooLong } else { Line::Text(line) }));
        }
    }
}

/// How deeply arrays and objects nest in `bytes`, counted without parsing (strings,
/// and escapes inside them, are skipped), up to `limit + 1`.
pub fn nesting(bytes: &[u8], limit: usize) -> usize {
    let (mut depth, mut deepest, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for &b in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                deepest = deepest.max(depth);
                if deepest > limit {
                    return deepest;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// The protocol's only writer. Every message is one line, written whole under the
/// lock; after a write fails for good the client is gone and nothing more is written.
pub struct Writer {
    out: Mutex<File>,
    broken: AtomicBool,
}

impl Writer {
    pub fn new(f: File) -> Writer {
        Writer { out: Mutex::new(f), broken: AtomicBool::new(false) }
    }

    pub fn broken(&self) -> bool {
        self.broken.load(Ordering::SeqCst)
    }
}

impl Sink for Writer {
    /// Writes one message. `write_all` retries short writes and interrupted calls.
    fn send(&self, msg: &Json) {
        if self.broken() {
            return;
        }
        // serde_json writes no raw newline: one inside a string is written as \n.
        let mut line = msg.to_string();
        line.push('\n');
        let Ok(mut out) = self.out.lock() else {
            self.broken.store(true, Ordering::SeqCst);
            return;
        };
        if out.write_all(line.as_bytes()).and_then(|()| out.flush()).is_err() {
            self.broken.store(true, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &[u8], max: usize) -> Vec<Line> {
        let mut r = std::io::Cursor::new(input.to_vec());
        let mut out = Vec::new();
        while let Some(l) = read_line(&mut r, max).unwrap() {
            out.push(l);
        }
        out
    }

    #[test]
    fn lines_are_split_and_long_ones_dropped_whole() {
        assert_eq!(lines(b"ab\ncd\n", 10), vec![Line::Text(b"ab".to_vec()), Line::Text(b"cd".to_vec())]);
        assert_eq!(lines(b"ab\ncd", 10), vec![Line::Text(b"ab".to_vec()), Line::Text(b"cd".to_vec())]);
        assert_eq!(lines(b"0123456789xyz\nok\n", 10), vec![Line::TooLong, Line::Text(b"ok".to_vec())]);
        assert_eq!(lines(b"0123456789", 10), vec![Line::Text(b"0123456789".to_vec())]);
        assert_eq!(lines(b"", 10), vec![]);
        assert_eq!(lines(b"\n", 10), vec![Line::Text(Vec::new())]);
    }

    #[test]
    fn nesting_is_counted_outside_strings() {
        assert_eq!(nesting(br#"{"a":[1,{"b":2}]}"#, 64), 3);
        assert_eq!(nesting(br#"{"a":"[[[[\"[[["}"#, 64), 1);
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert_eq!(nesting(deep.as_bytes(), 64), 65);
    }
}
