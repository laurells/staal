// Canonical targets for the runtime's own capabilities (RUNTIME.md section 12).
//
// An `only` limit compares an action's target with patterns as text. That is only
// safe if every target has exactly one spelling: otherwise "/contracts/../payroll"
// matches "/contracts/*". So before any check, the runtime puts URLs and file paths
// into one canonical form, and refuses anything it cannot. The compiler and the run
// config require `only` patterns for these capabilities to be canonical too, so a
// pattern can never silently fail to match its own targets.

/// A request target for `net.http`, parsed and made canonical once (AGENT_MANIFEST.md
/// section 16, R6). It cannot be changed after it is made, and everything that acts on
/// a request uses it: the `only` check compares `as_str`, and the HTTP client is given
/// `uri`, built from the same parsed parts, never the text the program gave. The
/// HTTP client takes the connection's host and port, the `Host` header, the TLS name
/// (and SNI), a proxy's `CONNECT` destination and the request line from that `uri`, so
/// each is the host and port that were checked.
///
/// Parsed by the WHATWG URL Standard (the `url` crate, as browsers do), which lowercases
/// the scheme and host, converts international hosts to ASCII, reads every IPv4 form
/// (`0x7f.1`, `2130706433`) as one address, writes IPv6 addresses compressed, drops the
/// default port and resolves `.` and `..` segments, including percent-encoded ones.
/// Then:
///  - the scheme must be `https`, or `http` for a loopback host (local testing only);
///  - user names and passwords in the URL are refused (credentials go in headers);
///  - an encoded `/` or `\` in the path is refused: servers disagree on what it means;
///  - percent-encoded unreserved characters are decoded and other escapes uppercased,
///    so `%7e` and `~` are the same target;
///  - the fragment is dropped, since it is never sent;
///  - the result must be a URI the HTTP layer reads back as exactly these parts, or it
///    is refused: a target is never sent in a form other than the one checked.
#[derive(Debug, Clone)]
pub struct Target {
    canonical: String,
    https: bool,
    host: url::Host<String>,
    port: u16,
    path: String,
    query: Option<String>,
    uri: ureq::http::Uri,
}

impl Target {
    /// The canonical text: what `only` patterns are matched against and the journal
    /// records.
    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    /// The URI the request is sent to: the same parts as `as_str`.
    pub fn uri(&self) -> &ureq::http::Uri {
        &self.uri
    }

    pub fn host(&self) -> &url::Host<String> {
        &self.host
    }

    /// The port connected to: the one in the URL, or the scheme's default.
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn is_https(&self) -> bool {
        self.https
    }

    pub fn has_query(&self) -> bool {
        self.query.is_some()
    }

    /// This target with `suffix` appended to its path, for a client that sends to
    /// fixed paths under a configured base (the model provider). The base must have no
    /// query; a trailing `/` of its path is dropped first. `suffix` is a path of
    /// unreserved characters and `/`, starting with `/`, so it needs no parsing.
    pub fn joined(&self, suffix: &str) -> Result<Target, String> {
        if self.query.is_some() {
            return Err(format!("'{}' has a query, so no path can be added to it", self.canonical));
        }
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '/');
        if !suffix.starts_with('/') || !suffix.chars().all(plain) || suffix.split('/').any(|seg| seg == "." || seg == "..") {
            return Err(format!("'{suffix}' is not a plain path"));
        }
        let path = format!("{}{suffix}", self.path.trim_end_matches('/'));
        Target::from_parts(self.https, self.host.clone(), self.port, path, None, &self.canonical)
    }

    /// Builds the target from parsed parts, and checks that the HTTP layer reads the URI
    /// back as exactly those parts. `given` names the URL in messages.
    fn from_parts(https: bool, host: url::Host<String>, port: u16, path: String, query: Option<String>, given: &str) -> Result<Target, String> {
        let scheme = if https { "https" } else { "http" };
        // As the URL Standard writes a host: a domain as it is, IPv4 in dotted decimal,
        // IPv6 compressed in brackets (`::ffff:7f00:1`, where Rust's own form would be
        // `::ffff:127.0.0.1`).
        let host_text = host.to_string();
        let default = if https { 443 } else { 80 };
        let explicit = (port != default).then_some(port);
        let authority = match explicit {
            Some(p) => format!("{host_text}:{p}"),
            None => host_text.clone(),
        };
        let path_and_query = match &query {
            Some(q) => format!("{path}?{q}"),
            None => path.clone(),
        };
        let canonical = format!("{scheme}://{authority}{path_and_query}");
        let refuse = |why: String| format!("'{}' cannot be sent exactly as checked: {why}", crate::clip::clip(given));
        let uri = ureq::http::Uri::builder()
            .scheme(scheme)
            .authority(authority.as_str())
            .path_and_query(path_and_query.as_str())
            .build()
            .map_err(|e| refuse(e.to_string()))?;
        let same = uri.scheme_str() == Some(scheme)
            && uri.authority().map(|a| a.as_str()) == Some(authority.as_str())
            && uri.host() == Some(host_text.as_str())
            && uri.port_u16() == explicit
            && uri.path_and_query().map(|p| p.as_str()) == Some(path_and_query.as_str())
            && uri.to_string() == canonical;
        if !same {
            return Err(refuse(format!("the HTTP layer reads it as {uri}")));
        }
        Ok(Target { canonical, https, host, port, path, query, uri })
    }
}

/// The target a URL names, under `net.http`'s rules (see `Target`), or why it is
/// refused.
pub fn target(s: &str) -> Result<Target, String> {
    let u = url::Url::parse(s).map_err(|e| format!("'{}' is not a valid URL: {e}", crate::clip::clip(s)))?;
    let host = match u.host() {
        Some(url::Host::Domain("")) | None => return Err(format!("'{}' has no host", crate::clip::clip(s))),
        Some(h) => h.to_owned(),
    };
    let https = match u.scheme() {
        "https" => true,
        "http" if is_loopback(&u) => false,
        "http" => return Err(format!("'{}' uses http://; only https:// is allowed (http:// only for localhost)", crate::clip::clip(s))),
        other => return Err(format!("'{}' uses {}://; only https:// is allowed", crate::clip::clip(s), crate::clip::clip(other))),
    };
    if !u.username().is_empty() || u.password().is_some() {
        return Err(format!("'{}' contains a user name or password; send credentials in a header", crate::clip::clip(s)));
    }
    let lower = u.path().to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") {
        return Err(format!("'{}' has an encoded '/' or '\\' in its path, which servers interpret differently", crate::clip::clip(s)));
    }
    // http and https always have a default port.
    let port = u.port_or_known_default().ok_or_else(|| format!("'{}' has no port", crate::clip::clip(s)))?;
    Target::from_parts(https, host, port, normalize_escapes(u.path()), u.query().map(normalize_escapes), s)
}

/// The canonical form of a URL for `net.http` (`Target::as_str`), or why it is refused.
pub fn canonical_url(s: &str) -> Result<String, String> {
    target(s).map(|t| t.canonical)
}

fn is_loopback(u: &url::Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Decodes `%XX` escapes of unreserved characters (RFC 3986: letters, digits, `-._~`)
/// and uppercases the hex digits of every other escape.
fn normalize_escapes(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            // Two hex digits: always a valid byte.
            let v = (hex(b[i + 1]) << 4) | hex(b[i + 2]);
            if v.is_ascii_alphanumeric() || matches!(v, b'-' | b'.' | b'_' | b'~') {
                out.push(v as char);
            } else {
                out.push('%');
                out.push_str(&s[i + 1..i + 3].to_ascii_uppercase());
            }
            i += 3;
        } else {
            // WHATWG serialisation leaves only ASCII here; copy whole characters anyway.
            match s[i..].chars().next() {
                Some(ch) => {
                    out.push(ch);
                    i += ch.len_utf8();
                }
                None => break,
            }
        }
    }
    out
}

fn hex(d: u8) -> u8 {
    match d {
        b'0'..=b'9' => d - b'0',
        b'a'..=b'f' => d - b'a' + 10,
        _ => d - b'A' + 10,
    }
}

/// The canonical form of a file path for `fs`, or why it is refused: absolute, no
/// empty, `.` or `..` segments (`..` is resolved as text, never above `/`), no NUL,
/// and naming something below `/`. Symbolic links are not resolved here: the runtime
/// refuses to follow any when it opens the file, so the canonical text is the file.
pub fn canonical_path(s: &str) -> Result<String, String> {
    if !s.starts_with('/') {
        return Err(format!("'{}' is not an absolute path", crate::clip::clip(s)));
    }
    if s.contains('\0') {
        return Err("a file path cannot contain a NUL character".into());
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!("'{}' goes above /", crate::clip::clip(s)));
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Err(format!("'{}' names / itself, not a file", crate::clip::clip(s)));
    }
    Ok(format!("/{}", parts.join("/")))
}

/// Checks that an `only` pattern for `net.http` is canonical, and that its `*` cannot
/// reach another host: the `*` must come after `scheme://host/`.
pub fn check_url_pattern(p: &str) -> Result<(), String> {
    match p.split_once('*') {
        None => {
            let c = canonical_url(p)?;
            if c != p {
                return Err(format!("URL pattern \"{p}\" is not in canonical form; write \"{c}\""));
            }
        }
        Some((pre, _)) => {
            let Some(at) = pre.find("://") else {
                return Err(format!("URL pattern \"{p}\" must start with https:// and a host"));
            };
            if !pre[at + 3..].contains('/') {
                return Err(format!(
                    "in URL pattern \"{p}\" the `*` is in the host, where it could match other servers; \
                     put it after the host and a `/`, as in \"https://api.example.com/*\""
                ));
            }
            let probe = format!("{pre}x");
            let c = canonical_url(&probe)?;
            if c != probe {
                let fixed = c.strip_suffix('x').unwrap_or(&c);
                return Err(format!("URL pattern \"{p}\" is not in canonical form; write \"{fixed}*\""));
            }
        }
    }
    Ok(())
}

/// Checks that an `only` pattern for `fs` is a canonical absolute path (with at most
/// one `*`, which the general pattern rules already require).
pub fn check_path_pattern(p: &str) -> Result<(), String> {
    if p == "*" {
        return Ok(());
    }
    let (probe, star) = match p.split_once('*') {
        None => (p.to_string(), false),
        Some((pre, post)) => (format!("{pre}x{post}"), true),
    };
    let c = canonical_path(&probe)?;
    if c != probe {
        let shown = if star { "a canonical absolute path around the `*`".to_string() } else { format!("\"{c}\"") };
        return Err(format!("path pattern \"{p}\" is not canonical; write {shown}"));
    }
    Ok(())
}

/// A TCP address, `host:port`, in one form: the host as the URL Standard writes it (a
/// domain lowercased and in ASCII, an IPv4 address in dotted decimal, an IPv6 address
/// in brackets, compressed), the port as a decimal number from 1 to 65535.
pub fn canonical_address(s: &str) -> Result<String, String> {
    let (host, port) = s.rsplit_once(':').ok_or_else(|| format!("'{}' is not host:port", crate::clip::clip(s)))?;
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("'{}': the port must be a number", crate::clip::clip(s)));
    }
    let port: u16 = port.parse().ok().filter(|p| *p > 0).ok_or_else(|| format!("'{}': the port must be from 1 to 65535", crate::clip::clip(s)))?;
    if host.is_empty() {
        return Err(format!("'{}' names no host", crate::clip::clip(s)));
    }
    let h = url::Host::parse(host).map_err(|e| format!("'{}': {e}", crate::clip::clip(s)))?;
    let host = match h {
        url::Host::Domain(d) => d,
        url::Host::Ipv4(a) => a.to_string(),
        url::Host::Ipv6(a) => format!("[{a}]"),
    };
    Ok(format!("{host}:{port}"))
}

/// Checks that an `only` pattern for `net.tcp` is a canonical address, `host:*` (any
/// port of one host), or `*`: a `*` can never stand for part of a host.
pub fn check_address_pattern(p: &str) -> Result<(), String> {
    if p == "*" {
        return Ok(());
    }
    let probe = match p.strip_suffix(":*") {
        Some(host) if !host.contains('*') => format!("{host}:1"),
        _ if p.contains('*') => return Err(format!("address pattern \"{p}\": a `*` may only be the whole port (host:*) or the whole pattern")),
        _ => p.to_string(),
    };
    let c = canonical_address(&probe)?;
    if c != probe {
        let want = if probe != p { format!("\"{}:*\"", c.rsplit_once(':').map_or(c.as_str(), |(h, _)| h)) } else { format!("\"{c}\"") };
        return Err(format!("address pattern \"{p}\" is not canonical; write {want}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_have_one_spelling() {
        for (given, want) in [
            ("Example.COM:25", "example.com:25"),
            ("127.0.0.1:080", "127.0.0.1:80"),
            ("[::1]:6379", "[::1]:6379"),
            ("[0:0:0:0:0:0:0:1]:6379", "[::1]:6379"),
            ("bücher.example:70", "xn--bcher-kva.example:70"),
        ] {
            assert_eq!(canonical_address(given).as_deref(), Ok(want), "{given}");
        }
        for bad in ["example.com", "example.com:", "example.com:0", "example.com:65536", "example.com:x", ":25", "exa mple.com:25", "::1:25"] {
            assert!(canonical_address(bad).is_err(), "{bad}");
        }
        assert_eq!(check_address_pattern("example.com:25"), Ok(()));
        assert_eq!(check_address_pattern("example.com:*"), Ok(()));
        assert_eq!(check_address_pattern("*"), Ok(()));
        for bad in ["*.example.com:25", "example.com:2*", "ex*:25", "Example.com:25", "Example.com:*"] {
            assert!(check_address_pattern(bad).is_err(), "{bad}");
        }
        assert!(check_address_pattern("Example.com:*").unwrap_err().contains("\"example.com:*\""));
    }

    /// R6: for each spelling, the authority checked (`as_str`), the authority the HTTP
    /// layer is given (`uri`: its host, port and the `Host` it sends) and the port
    /// connected to are one and the same.
    #[test]
    fn the_uri_sent_is_the_target_checked() {
        for (given, canonical, host, port) in [
            // International names, as ASCII; full-width dots are dots.
            ("https://BÜCHER.example/x", "https://xn--bcher-kva.example/x", "xn--bcher-kva.example", 443),
            ("https://a\u{ff0e}example/x", "https://a.example/x", "a.example", 443),
            // Every IPv4 form is one address.
            ("http://0x7f.1:8080/", "http://127.0.0.1:8080/", "127.0.0.1", 8080),
            ("http://2130706433/", "http://127.0.0.1/", "127.0.0.1", 80),
            ("http://127.1/", "http://127.0.0.1/", "127.0.0.1", 80),
            ("https://0300.0250.0.1/", "https://192.168.0.1/", "192.168.0.1", 443),
            // IPv6, compressed, in brackets.
            ("https://[0:0:0:0:0:0:0:1]:8443/", "https://[::1]:8443/", "[::1]", 8443),
            ("https://[2001:DB8::0:1]/", "https://[2001:db8::1]/", "[2001:db8::1]", 443),
            ("https://[::ffff:127.0.0.1]/", "https://[::ffff:7f00:1]/", "[::ffff:7f00:1]", 443),
            // Default and explicit ports.
            ("https://a.example:443/", "https://a.example/", "a.example", 443),
            ("https://a.example:0443/", "https://a.example/", "a.example", 443),
            ("https://a.example:80/", "https://a.example:80/", "a.example", 80),
            ("http://localhost:80/", "http://localhost/", "localhost", 80),
            ("https://a.example:8443", "https://a.example:8443/", "a.example", 8443),
            // Case and a trailing dot: the dot is kept, so it is another host to `only`.
            ("https://API.Example.COM./x", "https://api.example.com./x", "api.example.com.", 443),
            // Dot segments, plain and percent-encoded, are resolved before the check.
            ("https://a.example/x/%2e%2E/y/./z", "https://a.example/y/z", "a.example", 443),
            // A backslash is a slash: the host is a.example, the rest is path.
            ("https://a.example\\@b.example/", "https://a.example/@b.example/", "a.example", 443),
            // Tabs and newlines are removed, as browsers do.
            ("https://a.exa\tmple/x\n", "https://a.example/x", "a.example", 443),
            ("https://a.example/a?b=%7e#c", "https://a.example/a?b=~", "a.example", 443),
        ] {
            let t = target(given).unwrap_or_else(|e| panic!("{given}: {e}"));
            assert_eq!(t.as_str(), canonical, "{given}");
            assert_eq!(t.uri().to_string(), canonical, "{given}");
            assert_eq!(t.uri().host(), Some(host), "{given}");
            assert_eq!(t.uri().port_u16().unwrap_or(if t.is_https() { 443 } else { 80 }), port, "{given}");
            assert_eq!(t.port(), port, "{given}");
            assert_eq!(t.host().to_string(), host, "{given}");
        }
        for refused in [
            "https://a.example@b.example/",
            "https://a.example%2Fb.example/",
            "https://a.example/x%2Fy",
            "https://a.example/x%5cy",
            "https://a.example:99999/",
            "http://a.example/",
            "http://127.0.0.1.example/",
            "https://[::1/",
            "https://exa mple.com/",
            "https:///",
        ] {
            assert!(target(refused).is_err(), "{refused} was accepted as {:?}", target(refused).map(|t| t.as_str().to_string()));
        }
    }

    #[test]
    fn a_fixed_path_joins_a_base_without_parsing_again() {
        let base = target("https://API.example.com/proxy/").unwrap();
        let t = base.joined("/v1/messages").unwrap();
        assert_eq!((t.as_str(), t.uri().to_string().as_str()), ("https://api.example.com/proxy/v1/messages", "https://api.example.com/proxy/v1/messages"));
        assert_eq!(target("http://127.0.0.1:9/").unwrap().joined("/v1/x").unwrap().as_str(), "http://127.0.0.1:9/v1/x");
        for bad in ["v1", "/v1/../x", "/v1?q", "/v1#f", "/v1/%2e"] {
            assert!(base.joined(bad).is_err(), "{bad}");
        }
        assert!(target("https://a.example/?q=1").unwrap().joined("/x").is_err());
    }

    #[test]
    fn urls_have_one_spelling() {
        let same = [
            "https://API.Example.COM/v1/send",
            "https://api.example.com:443/v1/send",
            "https://api.example.com/v1/./send",
            "https://api.example.com/v1/x/../send",
            "https://api.example.com/v1/x/%2e%2e/send",
            "https://api.example.com/v1/x/%2E./send",
            "https://api.example.com/v%31/send",
            "https://api.example.com/v1/send#frag",
        ];
        for s in same {
            assert_eq!(canonical_url(s).unwrap(), "https://api.example.com/v1/send", "{s}");
        }
        assert_eq!(canonical_url("https://x.org/a%7eb").unwrap(), "https://x.org/a~b");
        assert_eq!(canonical_url("https://x.org/a%3cb").unwrap(), "https://x.org/a%3Cb");
        assert_eq!(canonical_url("https://x.org").unwrap(), "https://x.org/");
        assert_eq!(canonical_url("https://x.org/a?q=%7e&b=1").unwrap(), "https://x.org/a?q=~&b=1");
        assert_eq!(canonical_url("https://bücher.de/").unwrap(), "https://xn--bcher-kva.de/");
        assert_eq!(canonical_url("https://x.org:8443/").unwrap(), "https://x.org:8443/");
    }

    #[test]
    fn traversal_cannot_hide_inside_a_prefix() {
        // Each of these reaches /payroll, so none may match "/contracts/*".
        for s in [
            "https://x.org/contracts/../payroll",
            "https://x.org/contracts/%2e%2e/payroll",
            "https://x.org/contracts/.%2E/payroll",
        ] {
            let c = canonical_url(s).unwrap();
            assert!(!crate::pattern::matches("https://x.org/contracts/*", &c), "{s} -> {c}");
        }
        assert!(canonical_url("https://x.org/contracts/..%2Fpayroll").is_err());
        assert!(canonical_url("https://x.org/contracts/..%5cpayroll").is_err());
    }

    #[test]
    fn refused_urls() {
        for s in [
            "http://api.example.com/",
            "ftp://x.org/",
            "file:///etc/passwd",
            "https://user:pw@x.org/",
            "https://user@x.org/",
            "not a url",
            "https://",
        ] {
            assert!(canonical_url(s).is_err(), "{s}");
        }
        // Browsers read extra slashes as part of the scheme; the canonical form shows it.
        assert_eq!(canonical_url("https:///nohost").unwrap(), "https://nohost/");
        // A user name cannot pose as the host.
        let c = canonical_url("https://api.example.com.evil.net/").unwrap();
        assert!(!crate::pattern::matches("https://api.example.com/*", &c));
    }

    #[test]
    fn http_only_for_loopback() {
        assert_eq!(canonical_url("http://localhost:8080/x").unwrap(), "http://localhost:8080/x");
        assert_eq!(canonical_url("http://127.0.0.1:9/x").unwrap(), "http://127.0.0.1:9/x");
        assert_eq!(canonical_url("http://[::1]:9/x").unwrap(), "http://[::1]:9/x");
        assert!(canonical_url("http://10.0.0.1/").is_err());
        assert!(canonical_url("http://localhost.evil.net/").is_err());
    }

    #[test]
    fn canonical_urls_are_fixed_points() {
        for s in ["https://API.example.com/A/%7e/../b?x=%3c", "http://localhost:1/a/./b", "https://x.org/%E2%82%AC"] {
            let c = canonical_url(s).unwrap();
            assert_eq!(canonical_url(&c).unwrap(), c, "{s}");
        }
    }

    #[test]
    fn paths_have_one_spelling() {
        assert_eq!(canonical_path("/data//reports/./q3.csv").unwrap(), "/data/reports/q3.csv");
        assert_eq!(canonical_path("/data/reports/../secret").unwrap(), "/data/secret");
        assert_eq!(canonical_path("/a/b/").unwrap(), "/a/b");
        assert!(canonical_path("/..").is_err());
        assert!(canonical_path("/a/../../b").is_err());
        assert!(canonical_path("relative/x").is_err());
        assert!(canonical_path("/").is_err());
        assert!(canonical_path("/a\0b").is_err());
        let c = canonical_path("/contracts/../payroll").unwrap();
        assert!(!crate::pattern::matches("/contracts/*", &c));
    }

    #[test]
    fn url_patterns() {
        assert!(check_url_pattern("https://api.ng.termii.com/*").is_ok());
        assert!(check_url_pattern("https://api.ng.termii.com/api/sms/send").is_ok());
        assert!(check_url_pattern("http://localhost:8080/*").is_ok());
        // The `*` in the host would match other servers.
        assert!(check_url_pattern("https://api.ng.termii.com*").is_err());
        assert!(check_url_pattern("https://*.termii.com/*").is_err());
        assert!(check_url_pattern("*").is_err());
        // Not canonical: would never match a canonical target.
        assert!(check_url_pattern("https://API.termii.com/*").unwrap_err().contains("https://api.termii.com/*"));
        assert!(check_url_pattern("https://api.termii.com:443/x").is_err());
        assert!(check_url_pattern("http://api.termii.com/*").is_err());
    }

    #[test]
    fn path_patterns() {
        assert!(check_path_pattern("/data/reports/*").is_ok());
        assert!(check_path_pattern("/data/reports/q3.csv").is_ok());
        assert!(check_path_pattern("/data/*.csv").is_ok());
        assert!(check_path_pattern("*").is_ok());
        assert!(check_path_pattern("/data/../etc/*").is_err());
        assert!(check_path_pattern("data/*").is_err());
        assert!(check_path_pattern("/data//x").is_err());
    }
}
