// The Streamable HTTP transport of `staalc mcp` (docs/MCP.md section 12), protocol
// version 2026-07-28: one endpoint, POST only, each request one JSON-RPC message,
// answered with one JSON object or, for a call that asked for progress, an SSE stream
// that ends with the response. Requests are checked in the order of section 12.3, and
// the first rule a request breaks decides its answer; one that passes is handed to the
// same `Session` stdio uses, so the two transports share every rule about calls.
//
// HTTP/1.1 framing is hyper's (chunked bodies, a request with both Content-Length and
// Transfer-Encoding, keep-alive), on tokio. Runs stay on their own threads, as over
// stdio: the async side only reads requests and writes answers.
//
// Stage 1 of section 12.7: no authentication yet. A test build (`--features
// failpoints`) accepts `"auth": "none"` on a loopback address; a release build
// refuses `server.http` altogether until OAuth is built.

use super::proto::{self, Sink};
use super::{Cancel, Session, Sweeper, MODERN, VERSION_KEY};
use crate::host::tool::{keys_only, text, whole as whole_number};
use crate::host::{self, log, Ctl, Host};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::{Frame, Incoming};
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use serde_json::{json, Map, Value as Json};
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

/// The request line and headers must arrive within this, from when the server is ready
/// to read a request: when the connection opens (after the TLS handshake), and after
/// each answer on a kept-alive connection. So it is also how long an idle connection is
/// kept.
const HEADER_TIME: Duration = Duration::from_secs(10);
/// The TLS handshake must finish within this.
const HANDSHAKE_TIME: Duration = Duration::from_secs(10);
/// The body must arrive within this, once the headers have.
const BODY_TIME: Duration = Duration::from_secs(30);
/// The time limits in force: the constants above, except that a test build lets
/// `STAALC_TEST_HTTP_TIMES_MS="<header>,<body>"` shorten them, so tests of a slow client
/// take milliseconds. A release build reads no such variable.
fn times() -> (Duration, Duration) {
    #[cfg(feature = "failpoints")]
    if let Ok(v) = std::env::var("STAALC_TEST_HTTP_TIMES_MS")
        && let Some((h, b)) = v.split_once(',')
        && let (Ok(h), Ok(b)) = (h.parse::<u64>(), b.parse::<u64>())
    {
        return (Duration::from_millis(h), Duration::from_millis(b));
    }
    (HEADER_TIME, BODY_TIME)
}

/// The most a request's line and headers may take.
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// The most a body may hold: the longest message stdio reads.
const MAX_BODY_BYTES: usize = proto::MAX_LINE_BYTES;

/// What `server.http` says.
#[derive(Debug)]
pub struct Settings {
    pub listen: SocketAddr,
    pub path: String,
    /// The server's canonical URI (RFC 8707 section 2).
    pub resource: String,
    pub tls: Option<Arc<rustls::ServerConfig>>,
    /// Accepted `Host` header values, lowercase.
    pub allowed_hosts: Vec<String>,
    /// Serialized origins (RFC 6454) a browser may call from.
    pub allowed_origins: Vec<String>,
    pub max_connections: usize,
}

/// Reads `server.http`. `at` names it in messages.
pub fn parse(v: &Json, at: &str) -> Result<Settings, String> {
    if !cfg!(feature = "failpoints") {
        return Err(format!(
            "{at}: the HTTP transport is not finished (docs/MCP.md section 12): it has no authentication yet; serve over stdio"
        ));
    }
    let o = v.as_object().ok_or(format!("{at}: expected an object"))?;
    keys_only(o, at, &["listen", "path", "resource", "tls", "allowed_hosts", "allowed_origins", "auth", "max_connections"])?;
    let listen = text(o, "listen", at)?.ok_or(format!("{at}.listen: missing"))?;
    let listen: SocketAddr = listen
        .parse()
        .ok()
        .filter(|a: &SocketAddr| a.port() != 0)
        .ok_or(format!("{at}.listen: expected an IP address and a port from 1 to 65535, such as 127.0.0.1:8443"))?;
    let loopback = listen.ip().is_loopback();

    let path = text(o, "path", at)?.unwrap_or_else(|| "/mcp".to_string());
    let path_ok = path.starts_with('/')
        && path.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'?' | b'#' | b'%' | b'\\'))
        && !path.split('/').any(|seg| seg == "." || seg == "..");
    if !path_ok {
        return Err(format!("{at}.path: expected an absolute path such as /mcp: visible ASCII, without ?, #, %, \\, . or .. segments"));
    }

    let resource = text(o, "resource", at)?.ok_or(format!("{at}.resource: missing (the server's canonical https URI)"))?;
    let url = url::Url::parse(&resource).map_err(|e| format!("{at}.resource: {e}"))?;
    let canonical = url.as_str().strip_suffix('/').filter(|_| url.path() == "/").unwrap_or(url.as_str());
    let resource_ok = url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && canonical == resource;
    if !resource_ok {
        return Err(format!(
            "{at}.resource: expected an https URI in canonical form (lowercase scheme and host, no user, query, fragment or trailing slash), such as https://mcp.example.com/mcp"
        ));
    }
    if !url.path().ends_with(path.as_str()) {
        return Err(format!("{at}.resource: its path must end with {at}.path ({path})"));
    }

    let tls = match o.get("tls") {
        None if loopback => None,
        None => return Err(format!("{at}.tls: required unless {at}.listen is a loopback address (a TLS-terminating proxy on this machine)")),
        Some(t) => {
            let t = t.as_object().ok_or(format!("{at}.tls: expected an object"))?;
            let tat = format!("{at}.tls");
            keys_only(t, &tat, &["certificate_chain", "private_key"])?;
            let chain = text(t, "certificate_chain", &tat)?.ok_or(format!("{tat}.certificate_chain: missing"))?;
            let key = text(t, "private_key", &tat)?.ok_or(format!("{tat}.private_key: missing"))?;
            Some(crate::interp::tcp::server_config(&chain, &key, std::time::SystemTime::now()).map_err(|e| format!("{tat}: {e}"))?)
        }
    };

    let allowed_hosts = match o.get("allowed_hosts") {
        Some(v) => {
            let a = v.as_array().filter(|a| !a.is_empty()).ok_or(format!("{at}.allowed_hosts: expected a list of host or host:port"))?;
            let mut hosts = Vec::new();
            for (i, h) in a.iter().enumerate() {
                let h = h.as_str().filter(|h| valid_host(h)).ok_or(format!(
                    "{at}.allowed_hosts[{i}]: expected a lowercase host or host:port, as a Host header carries it"
                ))?;
                hosts.push(h.to_string());
            }
            hosts
        }
        None => {
            let mut hosts = vec![url.host_str().unwrap_or_default().to_string()];
            if let Some(p) = url.port() {
                hosts[0] = format!("{}:{p}", hosts[0]);
            }
            if loopback {
                hosts.push(listen.to_string());
                hosts.push(format!("localhost:{}", listen.port()));
            }
            hosts
        }
    };

    let mut allowed_origins = Vec::new();
    if let Some(v) = o.get("allowed_origins") {
        let a = v.as_array().ok_or(format!("{at}.allowed_origins: expected a list of origins"))?;
        for (i, x) in a.iter().enumerate() {
            let s = x.as_str().unwrap_or_default();
            let ok = url::Url::parse(s).ok().filter(|u| {
                matches!(u.scheme(), "https" | "http") && u.origin().ascii_serialization() == s
            });
            if ok.is_none() {
                return Err(format!("{at}.allowed_origins[{i}]: expected an origin, scheme://host[:port] in lowercase, such as https://app.example.com"));
            }
            allowed_origins.push(s.to_string());
        }
    }

    match o.get("auth") {
        None => return Err(format!("{at}.auth: missing")),
        Some(Json::String(s)) if s == "none" => {
            if !loopback {
                return Err(format!("{at}.auth: \"none\" is accepted only on a loopback address, and only by a test build"));
            }
        }
        Some(Json::Object(_)) => {
            return Err(format!("{at}.auth: OAuth is not built yet (docs/MCP.md section 12, stage 2)"));
        }
        Some(_) => return Err(format!("{at}.auth: expected an object")),
    }

    let max_connections = whole_number(o, "max_connections", at, 1, 10_000)?.unwrap_or(64) as usize;
    Ok(Settings { listen, path, resource, tls, allowed_hosts, allowed_origins, max_connections })
}

/// A Host header value: a lowercase host name, IPv4 address or bracketed IPv6 address,
/// with an optional port.
fn valid_host(h: &str) -> bool {
    let (host, port) = match h.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(':') && (host.starts_with('[') == host.ends_with(']')) && !port.contains(']') => (host, Some(port)),
        _ => (h, None),
    };
    let host_ok = if host.starts_with('[') {
        host.ends_with(']') && host[1..host.len() - 1].parse::<std::net::Ipv6Addr>().is_ok()
    } else {
        !host.is_empty() && host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    };
    host_ok && port.is_none_or(|p| p.parse::<u16>().is_ok_and(|p| p != 0) && !p.starts_with('0'))
}

/// `staalc mcp <manifest>` over HTTP. Returns the exit code.
pub fn serve(path: &Path) -> u8 {
    host::set_log_prefix("staalc mcp");
    // SIGINT and SIGTERM end the server: blocked here, before any thread starts, so every
    // thread inherits the mask and one thread receives them with sigwait. (Child
    // processes start with an empty mask: the standard library resets it.)
    let signals = match block_stop_signals() {
        Ok(s) => s,
        Err(e) => {
            log(&e);
            return 2;
        }
    };
    let opened = super::manifest::load(path).and_then(|m| {
        let follow = format!("with {}", host::tool::STATUS_TOOL);
        let h = Host::open(m.settings.clone(), &follow, &m.warnings)?;
        Ok((m, h))
    });
    let (mut m, server) = match opened {
        Ok(s) => s,
        Err(e) => {
            log(&e);
            return 2;
        }
    };
    let Some(settings) = m.http.take() else {
        log("internal error: the HTTP transport started without server.http");
        return 2;
    };
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_io().enable_time().build() {
        Ok(rt) => rt,
        Err(e) => {
            log(&format!("cannot start the HTTP runtime: {e}"));
            return 2;
        }
    };
    let listener = match rt.block_on(tokio::net::TcpListener::bind(settings.listen)) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("cannot listen on {}: {e}", settings.listen));
            return 2;
        }
    };
    let tools = m.tools.len();
    let session = Session::new(m, server.clone());
    let sweeper = Sweeper::start(server.clone());
    let scheme = if settings.tls.is_some() { "https" } else { "http" };
    log(&format!("serving {tools} tool(s) from {} on {scheme}://{}{}", path.display(), settings.listen, settings.path));

    let state = Arc::new(State { settings, session: session.clone(), next: AtomicU64::new(0) });
    let accepting = rt.spawn(accept_loop(listener, state));
    // Until SIGINT or SIGTERM.
    let got = wait_for_stop_signal(&signals);
    log(&format!("{got}: no new connection is accepted; finishing the calls in progress"));
    accepting.abort();
    // Calls in progress finish and are answered while the runtime still runs; then
    // connections still open (idle, or waiting for a call that is done) are dropped
    // with the runtime.
    let finishing = session.clone();
    let finished = rt.spawn_blocking(move || finishing.finish());
    let _ = rt.block_on(finished);
    rt.shutdown_timeout(Duration::from_secs(1));
    sweeper.stop();
    0
}

struct State {
    settings: Settings,
    session: Arc<Session>,
    /// Numbers the calls made over HTTP, for the session's table of calls in progress.
    next: AtomicU64,
}

#[cfg(unix)]
fn block_stop_signals() -> Result<libc::sigset_t, String> {
    // SAFETY: a sigset_t is plain data, initialised by sigemptyset before use; the calls
    // only read and write it and the calling thread's mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        let r = libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        if r != 0 {
            return Err(format!("cannot block SIGINT and SIGTERM: {}", std::io::Error::from_raw_os_error(r)));
        }
        Ok(set)
    }
}

#[cfg(unix)]
fn wait_for_stop_signal(set: &libc::sigset_t) -> &'static str {
    loop {
        let mut sig: libc::c_int = 0;
        // SAFETY: `set` is a valid, initialised signal set; `sig` is written by sigwait.
        let r = unsafe { libc::sigwait(set, &mut sig) };
        if r == 0 {
            return if sig == libc::SIGTERM { "SIGTERM" } else { "SIGINT" };
        }
        // EINTR cannot happen for sigwait on Linux or macOS; anything else, try again.
    }
}

async fn accept_loop(listener: tokio::net::TcpListener, state: Arc<State>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(state.settings.max_connections));
    let tls = state.settings.tls.clone().map(tokio_rustls::TlsAcceptor::from);
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                // Running out of descriptors, say: wait a little rather than spin.
                log(&format!("accepting a connection: {e}"));
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        // One over max_connections is closed at once.
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let (state, tls) = (state.clone(), tls.clone());
        tokio::spawn(async move {
            let _slot = slot;
            match tls {
                None => connection(hyper_util::rt::TokioIo::new(stream), state).await,
                Some(acceptor) => {
                    // A failed or slow handshake ends the connection; nothing was asked.
                    if let Ok(Ok(s)) = tokio::time::timeout(HANDSHAKE_TIME, acceptor.accept(stream)).await {
                        connection(hyper_util::rt::TokioIo::new(s), state).await;
                    }
                }
            }
        });
    }
}

async fn connection<I>(io: I, state: Arc<State>)
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |req| {
        let state = state.clone();
        async move { Ok::<_, std::convert::Infallible>(handle(&state, req).await) }
    });
    let mut b = hyper::server::conn::http1::Builder::new();
    b.timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(times().0).max_buf_size(MAX_HEAD_BYTES);
    // Errors here are the client's (a malformed request, a closed connection, a slow
    // head): hyper has answered or closed already, and there is nothing to log.
    let _ = b.serve_connection(io, service).await;
}

/// An answer's body: a whole one, or an SSE stream fed by a call.
pub enum Body {
    Whole(Option<Bytes>),
    Stream(StreamBody),
}

/// An SSE stream: one event per message from the call, ended after the response. It
/// holds the call's cancel guard, so the call is cancelled if the stream is dropped
/// before it ends (the client closed it).
pub struct StreamBody {
    rx: tokio::sync::mpsc::UnboundedReceiver<Json>,
    done: bool,
    guard: CancelGuard,
}

impl hyper::body::Body for Body {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Body::Whole(b) => Poll::Ready(b.take().map(|b| Ok(Frame::data(b)))),
            Body::Stream(s) => {
                if s.done {
                    return Poll::Ready(None);
                }
                match s.rx.poll_recv(cx) {
                    Poll::Ready(Some(msg)) => {
                        if msg.get("id").is_some() {
                            // The response: the last event. The call is answered, so
                            // dropping the stream from here on cancels nothing.
                            s.done = true;
                            s.guard.disarm();
                        }
                        // serde_json writes no raw newline, so one data line holds it.
                        Poll::Ready(Some(Ok(Frame::data(Bytes::from(format!("event: message\ndata: {msg}\n\n"))))))
                    }
                    // The call ended without a response (its thread is gone): end the stream.
                    Poll::Ready(None) => {
                        s.done = true;
                        Poll::Ready(None)
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Body::Whole(b) => b.is_none(),
            Body::Stream(s) => s.done,
        }
    }
}

/// Sets the call's cancelled flag when dropped, unless disarmed: the handler or the
/// stream holding it is dropped when the client closes the connection. Whatever the
/// flag decides is decided by the host (docs/MCP.md section 3, "Cancellation"): before
/// the reservation, the call does nothing; after it, nothing changes.
pub struct CancelGuard {
    ctl: Arc<Ctl>,
    armed: bool,
}

impl CancelGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if self.armed {
            self.ctl.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

/// One HTTP request's messages, sent to the handler waiting for them.
struct ChannelSink(tokio::sync::mpsc::UnboundedSender<Json>);

impl Sink for ChannelSink {
    fn send(&self, msg: &Json) {
        // The receiver is gone only if the client is: nothing more is sent.
        let _ = self.0.send(msg.clone());
    }
}

fn whole(status: StatusCode, content_type: Option<&'static str>, body: Option<Bytes>) -> Response<Body> {
    let mut r = Response::new(Body::Whole(body));
    *r.status_mut() = status;
    if let Some(ct) = content_type {
        r.headers_mut().insert(hyper::header::CONTENT_TYPE, HeaderValue::from_static(ct));
    }
    r
}

/// A JSON-RPC error with no id, as the transport's refusals carry.
fn refusal(status: StatusCode, code: i64, message: &str) -> Response<Body> {
    rpc(status, &proto::error_message(&Json::Null, code, message, None))
}

fn rpc(status: StatusCode, msg: &Json) -> Response<Body> {
    whole(status, Some("application/json"), Some(Bytes::from(msg.to_string())))
}

/// The status a JSON-RPC response travels with: the transport's own for the errors it
/// names, 200 for everything else (an error in a tool's run is a result, section 5).
fn status_of(msg: &Json) -> StatusCode {
    match msg["error"]["code"].as_i64() {
        Some(proto::METHOD_NOT_FOUND) => StatusCode::NOT_FOUND,
        Some(proto::UNSUPPORTED_VERSION | proto::HEADER_MISMATCH | proto::PARSE_ERROR | proto::INVALID_REQUEST) => StatusCode::BAD_REQUEST,
        _ => StatusCode::OK,
    }
}

/// The one value of header `name`: None when absent, Err when repeated or not visible
/// ASCII (with spaces and tabs).
fn one_header<'a>(h: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, String> {
    let mut all = h.get_all(name).iter();
    let Some(v) = all.next() else { return Ok(None) };
    if all.next().is_some() {
        return Err(format!("the {name} header is given more than once"));
    }
    v.to_str().map(Some).map_err(|_| format!("the {name} header holds characters a header value may not"))
}

/// Whether a media type (before any parameters) is `want`, case-insensitively.
fn media_type(v: &str) -> (String, Vec<(String, String)>) {
    let mut parts = v.split(';');
    let t = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().trim_matches('"').to_ascii_lowercase()))
        .collect();
    (t, params)
}

/// Whether an Accept header value accepts `want` (RFC 9110 section 12.5.1): a range
/// that matches it (`*/*`, `type/*` or the type itself) with a weight above 0.
fn accepts(accept: &str, want: &str) -> bool {
    let (wt, _) = want.split_once('/').unwrap_or((want, ""));
    accept.split(',').any(|range| {
        let (t, params) = media_type(range);
        let q = params.iter().find(|(k, _)| k == "q").map(|(_, v)| v.parse::<f32>().unwrap_or(0.0)).unwrap_or(1.0);
        q > 0.0 && (t == "*/*" || t == want || t == format!("{wt}/*"))
    })
}

/// The value of an `Mcp-Name` header: decoded from `=?base64?...?=` (standard base64 of
/// UTF-8) when it has that form.
fn decode_name(v: &str) -> Result<String, String> {
    use base64::Engine;
    match v.strip_prefix("=?base64?").and_then(|r| r.strip_suffix("?=")) {
        None => Ok(v.to_string()),
        Some(b) => base64::engine::general_purpose::STANDARD
            .decode(b)
            .ok()
            .and_then(|d| String::from_utf8(d).ok())
            .ok_or_else(|| "the Mcp-Name header's base64 value does not decode to UTF-8 text".to_string()),
    }
}

/// The methods the server serves over HTTP (2026-07-28).
const SERVED: [&str; 3] = ["server/discover", "tools/list", "tools/call"];

async fn handle(state: &Arc<State>, req: Request<Incoming>) -> Response<Body> {
    let s = &state.settings;
    let headers = req.headers();

    // Host: one, and allowed. An absolute-form target carries its own authority; it is
    // refused rather than judged.
    if req.uri().authority().is_some() {
        return refusal(StatusCode::BAD_REQUEST, proto::INVALID_REQUEST, "the request target must be a path");
    }
    match one_header(headers, "host") {
        Ok(Some(h)) if s.allowed_hosts.iter().any(|a| a.eq_ignore_ascii_case(h)) => {}
        Ok(Some(_)) => return refusal(StatusCode::MISDIRECTED_REQUEST, proto::INVALID_REQUEST, "this server does not answer for that host"),
        Ok(None) | Err(_) => return refusal(StatusCode::BAD_REQUEST, proto::INVALID_REQUEST, "a request needs exactly one Host header"),
    }
    // Origin, when a browser sends one.
    match one_header(headers, "origin") {
        Ok(None) => {}
        Ok(Some(o)) if s.allowed_origins.iter().any(|a| a == o) => {}
        _ => return refusal(StatusCode::FORBIDDEN, proto::INVALID_REQUEST, "requests from this origin are not allowed"),
    }
    if req.uri().path() != s.path {
        return refusal(StatusCode::NOT_FOUND, proto::INVALID_REQUEST, "no such endpoint");
    }
    if req.method() != Method::POST {
        let mut r = refusal(StatusCode::METHOD_NOT_ALLOWED, proto::INVALID_REQUEST, "the MCP endpoint takes POST only");
        r.headers_mut().insert(hyper::header::ALLOW, HeaderValue::from_static("POST"));
        return r;
    }
    match one_header(headers, "content-type") {
        Ok(Some(ct)) => {
            let (t, params) = media_type(ct);
            let charset_ok = params.iter().all(|(k, v)| k != "charset" || v == "utf-8");
            if t != "application/json" || !charset_ok {
                return refusal(StatusCode::UNSUPPORTED_MEDIA_TYPE, proto::INVALID_REQUEST, "the body must be application/json (UTF-8)");
            }
        }
        _ => return refusal(StatusCode::UNSUPPORTED_MEDIA_TYPE, proto::INVALID_REQUEST, "the body must be application/json (UTF-8)"),
    }
    let accept = headers.get_all(hyper::header::ACCEPT).iter().filter_map(|v| v.to_str().ok()).collect::<Vec<_>>().join(",");
    if !accepts(&accept, "application/json") || !accepts(&accept, "text/event-stream") {
        return refusal(StatusCode::NOT_ACCEPTABLE, proto::INVALID_REQUEST, "Accept must list application/json and text/event-stream");
    }
    let version = match one_header(headers, "mcp-protocol-version") {
        Ok(Some(v)) => v.to_string(),
        Ok(None) => return refusal(StatusCode::BAD_REQUEST, proto::HEADER_MISMATCH, "the MCP-Protocol-Version header is required"),
        Err(e) => return refusal(StatusCode::BAD_REQUEST, proto::HEADER_MISMATCH, &e),
    };
    let method_header = one_header(headers, "mcp-method").map(|v| v.map(str::to_string));
    let name_header = one_header(headers, "mcp-name").map(|v| v.map(str::to_string));

    // The body: at most MAX_BODY_BYTES, within BODY_TIME.
    let too_large = || {
        let mut r = refusal(StatusCode::PAYLOAD_TOO_LARGE, proto::INVALID_REQUEST, &format!("a body longer than {MAX_BODY_BYTES} bytes was refused"));
        r.headers_mut().insert(hyper::header::CONNECTION, HeaderValue::from_static("close"));
        r
    };
    if one_header(headers, "content-length").ok().flatten().and_then(|l| l.parse::<u64>().ok()).is_some_and(|l| l > MAX_BODY_BYTES as u64) {
        return too_large();
    }
    let body = http_body_util::Limited::new(req.into_body(), MAX_BODY_BYTES);
    let body_time = times().1;
    let bytes = match tokio::time::timeout(body_time, body.collect()).await {
        Ok(Ok(c)) => c.to_bytes(),
        Ok(Err(e)) if e.downcast_ref::<http_body_util::LengthLimitError>().is_some() => return too_large(),
        Ok(Err(_)) => return refusal(StatusCode::BAD_REQUEST, proto::INVALID_REQUEST, "the body could not be read"),
        Err(_) => {
            let mut r = refusal(StatusCode::REQUEST_TIMEOUT, proto::INVALID_REQUEST, &format!("the body did not arrive within {} ms", body_time.as_millis()));
            r.headers_mut().insert(hyper::header::CONNECTION, HeaderValue::from_static("close"));
            return r;
        }
    };

    // One JSON-RPC request or notification.
    if proto::nesting(&bytes, proto::MAX_DEPTH) > proto::MAX_DEPTH {
        return refusal(StatusCode::BAD_REQUEST, proto::INVALID_REQUEST, &format!("a message nested deeper than {} levels was refused", proto::MAX_DEPTH));
    }
    let msg: Json = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(e) => return refusal(StatusCode::BAD_REQUEST, proto::PARSE_ERROR, &format!("not JSON: {e}")),
    };
    let Some(o) = msg.as_object() else {
        let why = if msg.is_array() { "batches are not part of MCP; send one message per request" } else { "a message must be an object" };
        return refusal(StatusCode::BAD_REQUEST, proto::INVALID_REQUEST, why);
    };
    if o.get("jsonrpc").and_then(Json::as_str) != Some("2.0") {
        return refusal(StatusCode::BAD_REQUEST, proto::INVALID_REQUEST, "\"jsonrpc\" must be \"2.0\"");
    }
    let id = o.get("id");
    if id.is_some_and(|i| !super::valid_id(i)) {
        return refusal(StatusCode::BAD_REQUEST, proto::INVALID_REQUEST, "a request id must be a string or an integer");
    }
    let Some(method) = o.get("method").and_then(Json::as_str) else {
        return refusal(StatusCode::BAD_REQUEST, proto::INVALID_REQUEST, "a message must be a request or a notification; a client sends no responses");
    };
    let empty = Map::new();
    let params = match o.get("params") {
        None => &empty,
        Some(Json::Object(p)) => p,
        Some(_) => return refusal(StatusCode::BAD_REQUEST, proto::INVALID_REQUEST, "\"params\" must be an object"),
    };
    let id_or_null = id.cloned().unwrap_or(Json::Null);
    let mismatch = |why: String| rpc(StatusCode::BAD_REQUEST, &proto::error_message(&id_or_null, proto::HEADER_MISMATCH, &why, None));

    if version != MODERN {
        let data = json!({ "supported": [MODERN], "requested": version });
        return rpc(StatusCode::BAD_REQUEST, &proto::error_message(&id_or_null, proto::UNSUPPORTED_VERSION, "Unsupported protocol version", Some(data)));
    }
    let Some(id) = id else {
        // A notification: accepted, and none is used (docs/MCP.md section 12.3).
        return whole(StatusCode::ACCEPTED, None, None);
    };
    // The headers must say what the body says.
    let body_version = params.get("_meta").and_then(|m| m.get(VERSION_KEY)).and_then(Json::as_str);
    if body_version != Some(version.as_str()) {
        return mismatch(format!("Header mismatch: MCP-Protocol-Version is '{version}' but _meta[\"{VERSION_KEY}\"] is not the same"));
    }
    match method_header {
        Ok(Some(m)) if m == method => {}
        Ok(Some(_)) => return mismatch("Header mismatch: Mcp-Method does not match the body's method".into()),
        Ok(None) => return mismatch("the Mcp-Method header is required".into()),
        Err(e) => return mismatch(e),
    }
    if method == "tools/call" {
        let body_name = params.get("name").and_then(Json::as_str);
        match name_header {
            Ok(Some(n)) => match decode_name(&n) {
                Ok(n) if Some(n.as_str()) == body_name => {}
                Ok(_) => return mismatch("Header mismatch: Mcp-Name does not match the body's params.name".into()),
                Err(e) => return mismatch(e),
            },
            Ok(None) => return mismatch("the Mcp-Name header is required for tools/call".into()),
            Err(e) => return mismatch(e),
        }
    }
    if !SERVED.contains(&method) {
        return rpc(StatusCode::NOT_FOUND, &proto::error_message(id, proto::METHOD_NOT_FOUND, &format!("unknown method '{}'", crate::clip::clip(method)), None));
    }

    // Served as over stdio. A call is cancelled if this handler (or, for a stream, the
    // stream) is dropped before the response: the client closed the connection.
    let progress = method == "tools/call" && params.get("_meta").and_then(|m| m.get("progressToken")).is_some();
    let ctl = Arc::new(Ctl::default());
    let mut guard = CancelGuard { ctl: ctl.clone(), armed: true };
    let key = format!("http:{}", state.next.fetch_add(1, Ordering::SeqCst));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let out: Arc<dyn Sink> = Arc::new(ChannelSink(tx));
    state.session.request(id, method, params, &out, Cancel::Given { ctl, key });
    drop(out);

    if progress {
        let mut r = Response::new(Body::Stream(StreamBody { rx, done: false, guard }));
        let h = r.headers_mut();
        h.insert(hyper::header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        h.insert(hyper::header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        h.insert(HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no"));
        return r;
    }
    // One JSON object: the response; a progress message cannot come without a token.
    loop {
        match rx.recv().await {
            Some(m) if m.get("id").is_some() => {
                guard.disarm();
                return rpc(status_of(&m), &m);
            }
            Some(_) => continue,
            // The call ended without answering: it was cancelled, and no one is waiting.
            None => {
                guard.disarm();
                return whole(StatusCode::NO_CONTENT, None, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_headers_are_read_as_rfc_9110_says() {
        assert!(accepts("application/json, text/event-stream", "application/json"));
        assert!(accepts("application/json, text/event-stream", "text/event-stream"));
        assert!(accepts("*/*", "text/event-stream"));
        assert!(accepts("text/*;q=0.5", "text/event-stream"));
        assert!(accepts("Application/JSON", "application/json"));
        assert!(!accepts("application/json;q=0", "application/json"));
        assert!(!accepts("application/json", "text/event-stream"));
        assert!(!accepts("", "application/json"));
        assert!(!accepts("application/jsonx", "application/json"));
    }

    #[test]
    fn names_are_decoded_from_the_base64_sentinel() {
        assert_eq!(decode_name("get_weather").unwrap(), "get_weather");
        assert_eq!(decode_name("=?base64?SGVsbG8sIOS4lueVjA==?=").unwrap(), "Hello, 世界");
        assert_eq!(decode_name("=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?=").unwrap(), "=?base64?literal?=");
        assert!(decode_name("=?base64?not base64?=").is_err());
        assert!(decode_name("=?base64?/w==?=").is_err()); // 0xff is not UTF-8
    }

    #[test]
    fn host_values_are_checked() {
        for ok in ["mcp.example.com", "mcp.example.com:8443", "127.0.0.1:8443", "[::1]:8443", "localhost"] {
            assert!(valid_host(ok), "{ok}");
        }
        for bad in ["MCP.example.com", "mcp.example.com:0", "mcp.example.com:08443", "h:99999", "[::1", "a b", "", "x:"] {
            assert!(!valid_host(bad), "{bad}");
        }
    }
}
