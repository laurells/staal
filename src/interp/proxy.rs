// The proxy a network client uses when its config says `"proxy": "environment"`
// (RUNTIME.md sections 5 and 12): read from the environment once, checked, recorded in
// the run's journal, and used exactly as recorded, so the route a request takes is one
// decision that a resume can compare (RUNTIME.md section 11).
//
// The variables are those ureq's own `Proxy::try_from_env` reads, in its order: the
// first of `ALL_PROXY`, `all_proxy`, `HTTPS_PROXY`, `https_proxy`, `HTTP_PROXY`,
// `http_proxy` that is set to a non-empty value, with the first of `NO_PROXY`,
// `no_proxy` that is set. Where ureq silently skips a value it cannot use and tries the
// next variable, this refuses it, so a mistyped proxy never sends requests by another
// route. Also refused:
//  - a proxy URL with a user name or password, which would put a credential into the
//    journal;
//  - a SOCKS proxy, which this build cannot use (ureq's `socks-proxy` feature is off).
// `NO_PROXY` is split at commas, each entry trimmed and lowercased, empty entries left
// out; each entry is given to ureq's builder as one expression, as `try_from_env` does.

use serde_json::{json, Value as Json};

/// The variables a proxy URL is read from, in the order they are tried.
pub const PROXY_VARIABLES: [&str; 6] = ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];
/// The variables the hosts that bypass the proxy are read from, in order.
pub const NO_PROXY_VARIABLES: [&str; 2] = ["NO_PROXY", "no_proxy"];

/// A proxy chosen from the environment: what the client uses, and what the journal
/// records of it.
#[derive(Clone)]
pub struct Choice {
    pub proxy: ureq::Proxy,
    /// `{"from", "scheme", "host", "port", "no_proxy"}`: everything that decides the
    /// route, and no credential.
    pub record: Json,
}

impl std::fmt::Debug for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Choice({})", self.record)
    }
}

/// The proxy `"proxy": "environment"` means now, reading variables through `get`
/// (`std::env::var` in a run; a table in tests). None when no proxy variable is set.
pub fn from_environment(get: impl Fn(&str) -> Option<String>) -> Result<Option<Choice>, String> {
    let Some((from, value)) = PROXY_VARIABLES.iter().find_map(|v| get(v).filter(|x| !x.is_empty()).map(|x| (*v, x))) else {
        return Ok(None);
    };
    let parsed = ureq::Proxy::new(&value).map_err(|_| format!("the environment variable {from} is not a proxy URL"))?;
    if parsed.username().is_some() || parsed.password().is_some() {
        return Err(format!(
            "the environment variable {from} names a proxy with a user name or password, which would be written to \
             the run's journal; give a proxy without credentials"
        ));
    }
    let scheme = match parsed.protocol() {
        ureq::ProxyProtocol::Http => "http",
        ureq::ProxyProtocol::Https => "https",
        _ => return Err(format!("the environment variable {from} names a SOCKS proxy, which this staalc cannot use")),
    };
    let no_proxy: Vec<String> = NO_PROXY_VARIABLES
        .iter()
        .find_map(|v| get(v))
        .map(|list| list.split(',').map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()).collect())
        .unwrap_or_default();
    let mut builder = ureq::Proxy::builder(parsed.protocol()).host(parsed.host()).port(parsed.port());
    for e in &no_proxy {
        builder = builder.no_proxy(e);
    }
    let proxy = builder.build().map_err(|e| format!("the environment variable {from}: the proxy cannot be used: {e}"))?;
    let record = json!({ "from": from, "scheme": scheme, "host": proxy.host(), "port": proxy.port(), "no_proxy": no_proxy });
    Ok(Some(Choice { proxy, record }))
}

/// A `"proxy"` setting of a run config (`"none"` or `"environment"`), resolved against
/// the process's environment. `at` names the setting in messages.
pub fn of_setting(v: Option<&Json>, at: &str) -> Result<Option<Choice>, String> {
    match v.map(Json::as_str) {
        None | Some(Some("none")) => Ok(None),
        Some(Some("environment")) => from_environment(|k| std::env::var(k).ok()).map_err(|e| format!("{at}: {e}")),
        _ => Err(format!("{at}: expected \"none\" or \"environment\"")),
    }
}

/// What the journal records for a client: its choice's record, or null for no proxy.
pub fn record(c: &Option<Choice>) -> Json {
    c.as_ref().map_or(Json::Null, |c| c.record.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn no_variable_means_no_proxy() {
        assert!(from_environment(env(&[])).unwrap().is_none());
        // Set but empty is unset, and NO_PROXY alone chooses nothing.
        assert!(from_environment(env(&[("HTTPS_PROXY", ""), ("NO_PROXY", "x")])).unwrap().is_none());
    }

    #[test]
    fn the_first_variable_set_wins_in_ureqs_order() {
        let c = from_environment(env(&[("HTTPS_PROXY", "http://b.example:2"), ("ALL_PROXY", "http://a.example:1")])).unwrap().unwrap();
        assert_eq!(c.record["from"], "ALL_PROXY");
        assert_eq!((c.record["host"].as_str(), c.record["port"].as_u64()), (Some("a.example"), Some(1)));
        let c = from_environment(env(&[("http_proxy", "http://c.example:3"), ("HTTPS_PROXY", "https://b.example")])).unwrap().unwrap();
        assert_eq!(c.record["from"], "HTTPS_PROXY");
        assert_eq!((c.record["scheme"].as_str(), c.record["port"].as_u64()), (Some("https"), Some(443)));
        let c = from_environment(env(&[("http_proxy", "c.example:8080")])).unwrap().unwrap();
        assert_eq!(c.record, json!({ "from": "http_proxy", "scheme": "http", "host": "c.example", "port": 8080, "no_proxy": [] }));
    }

    #[test]
    fn an_unusable_value_is_refused_not_skipped() {
        // ureq would skip the bad ALL_PROXY and use HTTPS_PROXY; this refuses.
        let e = from_environment(env(&[("ALL_PROXY", "http://[bad"), ("HTTPS_PROXY", "http://ok.example")])).unwrap_err();
        assert!(e.contains("ALL_PROXY") && e.contains("not a proxy URL"), "{e}");
        for v in ["http://user:secret@p.example:1", "http://user@p.example:1"] {
            let e = from_environment(env(&[("HTTPS_PROXY", v)])).unwrap_err();
            assert!(e.contains("user name or password") && !e.contains("secret"), "{e}");
        }
        let e = from_environment(env(&[("ALL_PROXY", "socks5://p.example:1080")])).unwrap_err();
        assert!(e.contains("SOCKS"), "{e}");
    }

    #[test]
    fn no_proxy_entries_are_recorded_as_used() {
        let c = from_environment(env(&[("HTTP_PROXY", "http://p.example:3128"), ("no_proxy", "x"), ("NO_PROXY", " Localhost, .Internal.example ,,*.svc")]))
            .unwrap()
            .unwrap();
        assert_eq!(c.record["no_proxy"], json!(["localhost", ".internal.example", "*.svc"]));
        let direct = |u: &str| c.proxy.is_no_proxy(&u.parse().unwrap());
        assert!(direct("https://localhost/"));
        assert!(direct("https://a.internal.example/"));
        assert!(direct("https://b.svc/"));
        assert!(!direct("https://internal.example/"));
        assert!(!direct("https://api.example.com/"));
    }

    #[test]
    fn settings_are_none_or_environment() {
        assert!(of_setting(None, "net.proxy").unwrap().is_none());
        assert!(of_setting(Some(&json!("none")), "net.proxy").unwrap().is_none());
        assert!(of_setting(Some(&json!("system")), "net.proxy").unwrap_err().contains("net.proxy"));
    }
}
