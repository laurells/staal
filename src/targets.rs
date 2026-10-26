// Canonical targets for the runtime's own capabilities (RUNTIME.md section 12).
//
// An `only` limit compares an action's target with patterns as text. That is only
// safe if every target has exactly one spelling: otherwise "/contracts/../payroll"
// matches "/contracts/*". So before any check, the runtime puts URLs and file paths
// into one canonical form, and refuses anything it cannot. The compiler and the run
// config require `only` patterns for these capabilities to be canonical too, so a
// pattern can never silently fail to match its own targets.

/// The canonical form of a URL for `net.http`, or why it is refused.
///
/// Parsed by the WHATWG URL Standard (the `url` crate, as browsers do), which lowercases
/// the scheme and host, converts international hosts to ASCII, drops the default port
/// and resolves `.` and `..` segments, including percent-encoded ones. Then:
///  - the scheme must be `https`, or `http` for a loopback host (local testing only);
///  - user names and passwords in the URL are refused (credentials go in headers);
///  - an encoded `/` or `\` in the path is refused: servers disagree on what it means;
///  - percent-encoded unreserved characters are decoded and other escapes uppercased,
///    so `%7e` and `~` are the same target;
///  - the fragment is dropped, since it is never sent.
pub fn canonical_url(s: &str) -> Result<String, String> {
    let u = url::Url::parse(s).map_err(|e| format!("'{s}' is not a valid URL: {e}"))?;
    let host = u.host_str().filter(|h| !h.is_empty()).ok_or_else(|| format!("'{s}' has no host"))?;
    match u.scheme() {
        "https" => {}
        "http" if is_loopback(&u) => {}
        "http" => return Err(format!("'{s}' uses http://; only https:// is allowed (http:// only for localhost)")),
        other => return Err(format!("'{s}' uses {other}://; only https:// is allowed")),
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(format!("'{s}' contains a user name or password; send credentials in a header"));
    }
    let path = u.path();
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") {
        return Err(format!("'{s}' has an encoded '/' or '\\' in its path, which servers interpret differently"));
    }
    let mut out = format!("{}://{host}", u.scheme());
    if let Some(port) = u.port() {
        out.push_str(&format!(":{port}"));
    }
    out.push_str(&normalize_escapes(path));
    if let Some(q) = u.query() {
        out.push('?');
        out.push_str(&normalize_escapes(q));
    }
    Ok(out)
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
        return Err(format!("'{s}' is not an absolute path"));
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
                    return Err(format!("'{s}' goes above /"));
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Err(format!("'{s}' names / itself, not a file"));
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
    let (host, port) = s.rsplit_once(':').ok_or_else(|| format!("'{s}' is not host:port"))?;
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("'{s}': the port must be a number"));
    }
    let port: u16 = port.parse().ok().filter(|p| *p > 0).ok_or_else(|| format!("'{s}': the port must be from 1 to 65535"))?;
    if host.is_empty() {
        return Err(format!("'{s}' names no host"));
    }
    let h = url::Host::parse(host).map_err(|e| format!("'{s}': {e}"))?;
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
