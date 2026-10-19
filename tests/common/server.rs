// A small HTTP/1.1 server for tests, on 127.0.0.1. It records every request it
// receives and answers by path:
//   /ok            200, a JSON body {"message_id": "m-<n>"}
//   /send          200 {"message_id": "sent"}, deduplicating by Idempotency-Key the way
//                  payment APIs do: a key seen before gets the same answer, with no new effect
//   /fixed         200, body "fixed" (an answer that never changes)
//   /echo-auth     200, the request's Authorization header as the body
//   /redirect      302 to /ok (a client must not follow it)
//   /big           200, a body of 4096 bytes
//   /error         500, body "boom"
//   /binary        200, a body that is not UTF-8
//   /hang          reads the request, then never answers (for timeouts)
//   anything else  404
// Every connection is closed after one answer.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
    /// False for a request that repeated a known Idempotency-Key: it changed nothing.
    pub effect: bool,
}

pub struct Server {
    pub port: u16,
    pub log: Arc<Mutex<Vec<Req>>>,
}

impl Server {
    pub fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Arc<Mutex<Vec<Req>>> = Arc::new(Mutex::new(Vec::new()));
        let seen: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let l2 = log.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let (log, seen) = (l2.clone(), seen.clone());
                std::thread::spawn(move || handle(conn, log, seen));
            }
        });
        Server { port, log }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }

    /// Requests to `path` that had an effect.
    pub fn effects(&self, path: &str) -> usize {
        self.requests().iter().filter(|r| r.path == path && r.effect).count()
    }
}

fn handle(mut conn: TcpStream, log: Arc<Mutex<Vec<Req>>>, seen: Arc<Mutex<HashMap<String, String>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        match conn.read(&mut chunk) {
            Ok(0) | Err(_) => return,
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
    let mut effect = true;
    let n = log.lock().unwrap().len() + 1;
    let (status, reply): (u16, Vec<u8>) = match path.as_str() {
        "/ok" => (200, format!("{{\"message_id\": \"m-{n}\"}}").into_bytes()),
        "/send" => {
            let key = headers.get("idempotency-key").cloned();
            let mut seen = seen.lock().unwrap();
            match key.as_ref().and_then(|k| seen.get(k).cloned()) {
                Some(first) => {
                    effect = false;
                    (200, first.into_bytes())
                }
                None => {
                    let r = "{\"message_id\": \"sent\"}".to_string();
                    if let Some(k) = key {
                        seen.insert(k, r.clone());
                    }
                    (200, r.into_bytes())
                }
            }
        }
        "/fixed" => (200, b"fixed".to_vec()),
        "/echo-auth" => (200, headers.get("authorization").cloned().unwrap_or_default().into_bytes()),
        "/redirect" => (302, Vec::new()),
        "/big" => (200, vec![b'x'; 4096]),
        "/error" => (500, b"boom".to_vec()),
        "/binary" => (200, vec![0xff, 0xfe, 0x00]),
        "/hang" => {
            log.lock().unwrap().push(Req { method, path, headers, body, effect });
            std::thread::sleep(std::time::Duration::from_secs(30));
            return;
        }
        _ => (404, Vec::new()),
    };
    log.lock().unwrap().push(Req { method, path: path.clone(), headers, body, effect });
    let mut resp = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", reply.len());
    if path == "/redirect" {
        resp.push_str("Location: /ok\r\n");
    }
    resp.push_str("\r\n");
    let _ = conn.write_all(resp.as_bytes());
    let _ = conn.write_all(&reply);
}
