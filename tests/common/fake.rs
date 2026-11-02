// A fake service for package tests: a local HTTP server that answers as a service's
// documentation shows, described as data instead of written as a server. A package
// test (packages/<name>/tests/*.json, run by tests/package_cases_test.rs) lists the
// routes it needs:
//
//   { "method": "POST", "path": "/v1/messages",            exact, or with one `*`
//     "status": 201,
//     "body": { "id": "msg_{n}" },                          JSON (or a string, sent as is)
//     "headers": { "Content-Type": "application/json" },    optional
//     "dedupe": "Idempotency-Key" }                         optional
//
// `{n}` in a body is the request's number (1 for the first request to the service),
// so two requests that took effect are told apart. With `dedupe`, a request repeating
// a value of that header seen before gets the first answer again and has no effect, as
// a service that deduplicates does. A request no route matches gets 404. The fake
// records every request, the answer it gave, and whether it had an effect (changed
// something at the service: not a read, not a deduplicated repeat, not a 404).

use super::http::{read_request, write_response};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Route {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
    pub dedupe: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
    /// False for a read (GET, HEAD), a repeat the service deduplicated, or a request
    /// no route matched: none of them changes anything at the service.
    pub effect: bool,
    pub status: u16,
    pub answer: String,
}

/// The first answer to each deduplication key, by route.
type Answers = HashMap<(usize, String), (u16, String)>;

pub struct Fake {
    base: String,
    log: Arc<Mutex<Vec<Seen>>>,
}

/// Whether `s` matches `p`: exact, or with one `*` standing for any run of characters.
fn matches(p: &str, s: &str) -> bool {
    match p.split_once('*') {
        None => p == s,
        Some((pre, suf)) => s.len() >= pre.len() + suf.len() && s.starts_with(pre) && s.ends_with(suf),
    }
}

impl Route {
    /// Reads routes from a package test, refusing anything it does not know, so a
    /// typo is an error rather than a route that never matches.
    pub fn list(j: &Json) -> Result<Vec<Route>, String> {
        let a = j.as_array().ok_or("routes: expected a list")?;
        let mut out = Vec::new();
        for (i, r) in a.iter().enumerate() {
            let at = format!("routes[{i}]");
            let o = r.as_object().ok_or(format!("{at}: expected an object"))?;
            for k in o.keys() {
                if !["method", "path", "status", "body", "headers", "dedupe"].contains(&k.as_str()) {
                    return Err(format!("{at}: unknown key '{k}'"));
                }
            }
            let text = |k: &str| o.get(k).and_then(Json::as_str).map(str::to_string).ok_or(format!("{at}.{k}: expected a string"));
            let path = text("path")?;
            if !path.starts_with('/') || path.matches('*').count() > 1 {
                return Err(format!("{at}.path: expected a path starting with / and at most one *"));
            }
            let status = o.get("status").and_then(Json::as_u64).filter(|s| (100..600).contains(s));
            let body = match o.get("body") {
                None => String::new(),
                Some(Json::String(s)) => s.clone(),
                Some(v) => v.to_string(),
            };
            let mut headers = Vec::new();
            if let Some(h) = o.get("headers") {
                for (k, v) in h.as_object().ok_or(format!("{at}.headers: expected an object"))? {
                    headers.push((k.clone(), v.as_str().ok_or(format!("{at}.headers.{k}: expected a string"))?.to_string()));
                }
            }
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) && o.get("body").is_some_and(|b| !b.is_string()) {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
            out.push(Route {
                method: text("method")?.to_ascii_uppercase(),
                path,
                status: status.ok_or(format!("{at}.status: expected an HTTP status"))? as u16,
                body,
                headers,
                dedupe: o.get("dedupe").map(|_| text("dedupe")).transpose()?.map(|h| h.to_ascii_lowercase()),
            });
        }
        Ok(out)
    }
}

impl Fake {
    pub fn start(routes: Vec<Route>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let log: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let seen: Arc<Mutex<Answers>> = Arc::new(Mutex::new(HashMap::new()));
        let routes = Arc::new(routes);
        let l2 = log.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let (log, seen, routes) = (l2.clone(), seen.clone(), routes.clone());
                std::thread::spawn(move || {
                    let Some(req) = read_request(&mut conn) else { return };
                    let bare = req.path.split('?').next().unwrap_or("").to_string();
                    let route = routes.iter().position(|r| r.method == req.method && matches(&r.path, &bare));
                    // One lock across the decision and the record, so the numbering
                    // and the deduplication are exact under concurrent requests.
                    let mut log = log.lock().unwrap();
                    let n = log.len() + 1;
                    let (status, answer, effect, headers) = match route {
                        None => (404, json!({ "message": format!("no route in the fake service for {} {bare}", req.method) }).to_string(), false, Vec::new()),
                        Some(i) => {
                            let r = &routes[i];
                            let key = r.dedupe.as_ref().and_then(|h| req.headers.get(h)).cloned();
                            let mut seen = seen.lock().unwrap();
                            match key.as_ref().and_then(|k| seen.get(&(i, k.clone())).cloned()) {
                                Some((s, a)) => (s, a, false, r.headers.clone()),
                                None => {
                                    let a = r.body.replace("{n}", &n.to_string());
                                    if let Some(k) = key {
                                        seen.insert((i, k), (r.status, a.clone()));
                                    }
                                    // A read changes nothing at the service.
                                    let effect = !matches!(req.method.as_str(), "GET" | "HEAD");
                                    (r.status, a, effect, r.headers.clone())
                                }
                            }
                        }
                    };
                    log.push(Seen {
                        method: req.method,
                        path: req.path,
                        headers: req.headers,
                        body: req.body,
                        effect,
                        status,
                        answer: answer.clone(),
                    });
                    drop(log);
                    write_response(&mut conn, status, &headers, answer.as_bytes());
                });
            }
        });
        Fake { base, log }
    }

    /// The service's base URL, `http://127.0.0.1:<port>`.
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.log.lock().unwrap().clone()
    }

    /// How many requests to `method path` (the path without its query) took effect.
    pub fn effects(&self) -> HashMap<(String, String), usize> {
        let mut out = HashMap::new();
        for r in self.requests().iter().filter(|r| r.effect) {
            *out.entry((r.method.clone(), r.path.split('?').next().unwrap_or("").to_string())).or_insert(0) += 1;
        }
        out
    }
}
