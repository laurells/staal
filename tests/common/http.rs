// Reading one HTTP/1.1 request and writing its answer, for the test servers
// (server.rs, fake.rs). Every connection carries one request and is closed after.

use std::collections::HashMap;
use std::io::{Read, Write};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// The path as sent, query included.
    pub path: String,
    /// Header names in lower case.
    pub headers: HashMap<String, String>,
    pub body: String,
}

/// Reads one request: its head, then as many body bytes as Content-Length says. None
/// if the connection closed before a whole head arrived.
pub fn read_request<S: Read>(conn: &mut S) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        match conn.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let path = first.next().unwrap_or("").to_string();
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while buf.len() < head_end + len {
        match conn.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body = String::from_utf8_lossy(&buf[head_end..(head_end + len).min(buf.len())]).to_string();
    Some(Request { method, path, headers, body })
}

/// Writes a whole answer and closes the exchange.
pub fn write_response<S: Write>(conn: &mut S, status: u16, headers: &[(String, String)], body: &[u8]) {
    let mut resp = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        resp.push_str(&format!("{k}: {v}\r\n"));
    }
    resp.push_str("\r\n");
    let _ = conn.write_all(resp.as_bytes());
    let _ = conn.write_all(body);
    let _ = conn.flush();
}
