// What a plan says is enforced on each grant: for every runtime capability a grant
// gives, itself or a library's need, the `only` the grant gives and the one the program
// writes, the addresses it may connect to, how its connections are verified, and what a
// resumed run does with a request through it left in doubt by a crash (RUNTIME.md section
// 11); for an adapter's capability, the adapter's rule. The rule is the one every action
// decides with (`interp::runtime_recovery`, `interp::adapter_recovery`), and the crash
// tests show each action follows it.

use serde_json::json;
use staalc::interp::{plan, AddressPlan, FailKind, Recovery, RuntimeGrantPlan};
use std::collections::HashMap;

const SRC: &str = r#"capability bank.pay {
    needs get: cap net.http.get(only: ["https://bank.example.com/*"])
    needs post: cap net.http.post

    canonical(id: Text) -> Text {
        return id
    }

    action fetch(id: Text @target) -> Text {
        let r = get.get("https://bank.example.com/" + url_segment(id))
        return "status " + json(r.status)
    }
}

fn main(
    get: cap net.http.get(only: ["https://api.example.com/*"]),
    post: cap net.http.post,
    tcp: cap net.tcp.exchange,
    read: cap fs.read,
    mail: cap email.send,
    pay: cap bank.pay.fetch,
) -> Int {
    return 1
}
"#;

fn config(idempotent: Option<bool>) -> serde_json::Value {
    let mut cfg = json!({ "entry": "main", "grants": {
        "get": { "cap": "net.http.get", "only": ["https://api.example.com/v1/*"], "private_addresses": ["10.0.0.0/8"], "cloud_metadata": true },
        "post": { "cap": "net.http.post", "idempotency_key": true },
        "tcp": { "cap": "net.tcp.exchange", "only": ["mail.example.com:25"] },
        "read": { "cap": "fs.read", "only": ["/srv/data/*"] },
        "mail": { "cap": "email.send" },
        "pay": { "cap": "bank.pay.fetch", "needs": {
            "get": { "cap": "net.http.get", "only": ["https://bank.example.com/accounts/*"] },
            "post": { "cap": "net.http.post" } } } } });
    if let Some(i) = idempotent {
        cfg["adapters"] = json!({ "simulated": { "idempotent": i } });
    }
    cfg
}

fn strs(v: &[&str]) -> Option<Vec<String>> {
    Some(v.iter().map(|s| s.to_string()).collect())
}

fn http(grant: &str, path: &str, granted: Option<Vec<String>>, declared: Option<Vec<String>>, recovery: Recovery) -> RuntimeGrantPlan {
    RuntimeGrantPlan {
        grant: grant.into(),
        path: path.into(),
        granted_only: granted,
        declared_only: declared,
        addresses: Some(AddressPlan { private: vec![], cloud_metadata: false }),
        tls: Some("https_targets"),
        recovery,
    }
}

#[test]
fn a_plan_states_what_is_enforced_on_each_runtime_grant() {
    let prog = staalc::check_program(SRC).unwrap();
    let p = plan(&prog, &config(None)).unwrap();
    let by: HashMap<&str, &staalc::interp::GrantPlan> = p.grants.iter().map(|g| (g.name.as_str(), g)).collect();
    let only = |name: &str| -> RuntimeGrantPlan {
        let g = by[name];
        assert_eq!(g.recovery, None, "{name}: a runtime capability's rule is its runtime grant's");
        assert_eq!(g.runtime_grants.len(), 1, "{name}");
        g.runtime_grants[0].clone()
    };
    assert_eq!(
        only("get"),
        RuntimeGrantPlan {
            addresses: Some(AddressPlan { private: vec!["10.0.0.0/8".into()], cloud_metadata: true }),
            ..http("get", "net.http.get", strs(&["https://api.example.com/v1/*"]), strs(&["https://api.example.com/*"]), Recovery::Repeat)
        }
    );
    assert_eq!(only("post"), http("post", "net.http.post", None, None, Recovery::ResendWithKey));
    assert_eq!(
        only("tcp"),
        RuntimeGrantPlan {
            grant: "tcp".into(),
            path: "net.tcp.exchange".into(),
            granted_only: strs(&["mail.example.com:25"]),
            declared_only: None,
            addresses: Some(AddressPlan { private: vec![], cloud_metadata: false }),
            tls: Some("never"),
            recovery: Recovery::Operator,
        }
    );
    assert_eq!(
        only("read"),
        RuntimeGrantPlan {
            grant: "read".into(),
            path: "fs.read".into(),
            granted_only: strs(&["/srv/data/*"]),
            declared_only: None,
            addresses: None,
            tls: None,
            recovery: Recovery::Repeat,
        }
    );
    // An adapter's capability: the run config's simulated adapter, idempotent unless it says not.
    assert!(by["mail"].runtime_grants.is_empty());
    assert_eq!(by["mail"].recovery, Some(Recovery::ResendWithKey));
    let not = plan(&prog, &config(Some(false))).unwrap();
    assert_eq!(not.grants.iter().find(|g| g.name == "mail").unwrap().recovery, Some(Recovery::Operator));
    // A library capability: no rule of its own; one per need, each with what the library declares.
    let pay = by["pay"];
    assert_eq!(pay.recovery, None);
    assert_eq!(
        pay.runtime_grants,
        [
            http("pay.get", "net.http.get", strs(&["https://bank.example.com/accounts/*"]), strs(&["https://bank.example.com/*"]), Recovery::Repeat),
            http("pay.post", "net.http.post", None, None, Recovery::Operator),
        ]
    );
}

/// What the plan says of `only`: a target must match a pattern of each list there is.
/// Each refusal comes before anything is sent.
#[test]
fn a_target_must_match_both_the_grant_and_the_program() {
    let src = "fn main(get: cap net.http.get(only: [\"https://a.example.com/*\", \"https://b.example.com/*\"]), url: Text) -> Int {\n    let r = get.get(url)\n    return r.status\n}\n";
    let prog = staalc::check_program(src).unwrap();
    let cfg = |url: &str| {
        json!({ "entry": "main", "inputs": { "url": url },
                "grants": { "get": { "cap": "net.http.get", "only": ["https://b.example.com/*", "https://c.example.com/*"] } } })
    };
    for url in ["https://a.example.com/x", "https://c.example.com/x"] {
        let f = staalc::interp::run_config(&prog, &cfg(url)).unwrap().result.unwrap_err();
        assert_eq!(f.kind, FailKind::Capability, "{url}: {}", f.message);
    }
    let g = plan(&prog, &cfg("")).unwrap().grants[0].runtime_grants[0].clone();
    assert_eq!(g.granted_only, strs(&["https://b.example.com/*", "https://c.example.com/*"]));
    assert_eq!(g.declared_only, strs(&["https://a.example.com/*", "https://b.example.com/*"]));
}

/// How each family's connections are verified, as the plan states it.
#[test]
fn a_plan_states_how_connections_are_verified() {
    let src = "fn main(a: cap net.http.post, b: cap net.tcp.exchange, c: cap net.tls.exchange, d: cap fs.write, e: cap db.sqlite.query) -> Int {\n    return 1\n}\n";
    let prog = staalc::check_program(src).unwrap();
    let cfg = json!({ "entry": "main", "grants": {
        "a": { "cap": "net.http.post" }, "b": { "cap": "net.tcp.exchange" }, "c": { "cap": "net.tls.exchange" },
        "d": { "cap": "fs.write" }, "e": { "cap": "db.sqlite.query" } } });
    let p = plan(&prog, &cfg).unwrap();
    let tls: Vec<(&str, Option<&str>)> = p.grants.iter().map(|g| (g.name.as_str(), g.runtime_grants[0].tls)).collect();
    assert_eq!(tls, [("a", Some("https_targets")), ("b", Some("never")), ("c", Some("always")), ("d", None), ("e", None)]);
    assert_eq!(staalc::interp::tls_of("db.postgres.query"), Some("per_grant"));
    assert_eq!(staalc::interp::tls_of("net.tcp.accept"), Some("never"));
    assert_eq!(staalc::interp::tls_of("net.tls.accept"), Some("always"));
}

/// The rule itself, for every runtime capability: the cases RUNTIME.md section 11 states.
#[test]
fn the_rule_for_a_request_left_in_doubt() {
    use staalc::interp::{adapter_recovery, runtime_recovery};
    for cap in ["net.http.get", "fs.read", "fs.write", "db.sqlite.query", "db.postgres.query", "net.tcp.accept", "net.tcp.reply", "net.tls.accept", "net.tls.reply"] {
        assert_eq!(runtime_recovery(cap, false, false), Recovery::Repeat, "{cap}");
    }
    for cap in ["net.http.post", "net.http.put", "net.http.patch", "net.http.delete", "db.sqlite.execute", "db.postgres.execute"] {
        assert_eq!(runtime_recovery(cap, false, false), Recovery::Operator, "{cap}");
        assert_eq!(runtime_recovery(cap, true, false), Recovery::ResendWithKey, "{cap} with a key");
    }
    for cap in ["net.tcp.exchange", "net.tls.exchange"] {
        assert_eq!(runtime_recovery(cap, false, false), Recovery::Operator, "{cap}");
    }
    assert_eq!(runtime_recovery("ffi.m.f", false, false), Recovery::Operator);
    assert_eq!(runtime_recovery("ffi.m.f", false, true), Recovery::Repeat);
    assert_eq!((adapter_recovery(true), adapter_recovery(false)), (Recovery::ResendWithKey, Recovery::Operator));
    assert_eq!(
        [Recovery::Repeat, Recovery::ResendWithKey, Recovery::Operator].map(|r| (r.name(), r.retries(), r.outcome_can_be_unknown())),
        [("repeat", true, false), ("resend_with_key", true, false), ("operator", false, true)]
    );
}
