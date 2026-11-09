// A fake OAuth authorization server for tests: HTTPS on 127.0.0.1 with a certificate
// for localhost from the test authority, answering each path with what the test set
// (status, headers, body, an optional delay), and counting requests per path. Paths not
// set answer 404. Every connection is closed after one answer.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct Route {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub delay: Duration,
}

impl Route {
    pub fn json(body: &serde_json::Value) -> Route {
        Route { status: 200, headers: vec![("Content-Type".into(), "application/json".into())], body: body.to_string().into_bytes(), delay: Duration::ZERO }
    }
    pub fn status(status: u16) -> Route {
        Route { status, headers: vec![], body: vec![], delay: Duration::ZERO }
    }
    pub fn raw(body: Vec<u8>) -> Route {
        Route { status: 200, headers: vec![("Content-Type".into(), "application/json".into())], body, delay: Duration::ZERO }
    }
}

pub struct AuthServer {
    /// `https://localhost:<port>`, the issuer when it serves at its root.
    pub base: String,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl AuthServer {
    pub fn start(tls: Arc<rustls::ServerConfig>) -> AuthServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
        let hits: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let (r2, h2) = (routes.clone(), hits.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let (routes, hits, tls) = (r2.clone(), h2.clone(), tls.clone());
                std::thread::spawn(move || {
                    let Ok(sc) = rustls::ServerConnection::new(tls) else { return };
                    let mut s = rustls::StreamOwned::new(sc, conn);
                    let _ = s.sock.set_read_timeout(Some(Duration::from_secs(10)));
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match s.read(&mut b) {
                            Ok(1) => head.push(b[0]),
                            _ => return,
                        }
                    }
                    let line = String::from_utf8_lossy(&head).lines().next().unwrap_or_default().to_string();
                    let path = line.split(' ').nth(1).unwrap_or_default().to_string();
                    *hits.lock().unwrap().entry(path.clone()).or_default() += 1;
                    let route = routes.lock().unwrap().get(&path).cloned().unwrap_or_else(|| Route::status(404));
                    std::thread::sleep(route.delay);
                    let mut out = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", route.status, route.body.len());
                    for (k, v) in &route.headers {
                        out.push_str(&format!("{k}: {v}\r\n"));
                    }
                    out.push_str("\r\n");
                    let _ = s.write_all(out.as_bytes()).and_then(|_| s.write_all(&route.body)).and_then(|_| s.flush());
                    s.conn.send_close_notify();
                    let _ = s.flush();
                });
            }
        });
        AuthServer { base: format!("https://localhost:{port}"), routes, hits }
    }

    pub fn set(&self, path: &str, r: Route) {
        self.routes.lock().unwrap().insert(path.to_string(), r);
    }

    pub fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}
