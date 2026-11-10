// The consumers of a program's grants, as `staalc schema` reports them (src/consumers.rs;
// AGENT_MANIFEST.md section 16, R3). Each test asserts one of two outcomes: the
// consumer graph complete and exactly as expected, or a program the language refuses,
// for the ways of moving a capability it does not allow.
//
// Packages are made with git in the test's own folder and fetched by path.

use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");

fn base(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("consumers").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(d).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A git repository at `dir` holding `files`; returns the commit id.
fn repo(dir: &Path, files: &[(&str, String)]) -> String {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    for (f, text) in files {
        std::fs::write(dir.join(f), text).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c"]);
    git(dir, &["rev-parse", "HEAD"])
}

fn schema(main: &Path, entry: &str) -> Json {
    let o = Command::new(BIN).args(["schema", main.to_str().unwrap(), "--entry", entry]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn check_errors(src: &str) -> String {
    match staalc::check_program(src) {
        Ok(_) => String::new(),
        Err(d) => d.iter().map(|d| format!("{}: {}", d.code, d.msg)).collect::<Vec<_>>().join("\n"),
    }
}

const V1: &str = "https://bank.example.com/v1/*";
const ALL: &str = "https://bank.example.com/*";

fn agent(grant: &str, cap: &str, limits: Json) -> Json {
    json!({ "grant": grant, "cap": cap, "holder": { "kind": "agent" }, "limits": limits,
            "source": format!("the type of entry parameter '{grant}'") })
}

fn package(grant: &str, cap: &str, pkg: &str, function: &str, parameter: &str, limits: Json) -> Json {
    json!({ "grant": grant, "cap": cap, "holder": { "kind": "package", "package": pkg, "function": function, "parameter": parameter },
            "limits": limits, "source": format!("the type of parameter '{parameter}' of {function}") })
}

fn need(grant: &str, cap: &str, capability: &str, need: &str, pkg: Option<&str>, limits: Json) -> Json {
    json!({ "grant": grant, "cap": cap, "holder": { "kind": "library_need", "capability": capability, "need": need, "package": pkg },
            "limits": limits, "source": format!("the type of need '{need}' of capability {capability}") })
}

/// A project using two packages: `ledger`, and `bank`, which depends on `ledger`. The
/// agent holds four grants and moves them every way the language allows: through
/// several of its own functions, returned from its own function, returned from a
/// package's, into a package that passes it on to another package (and to a function
/// of its own on the way, which is not a consumer), by a name imported from a package,
/// and as the need of a library capability, both its own and a package's, one of which
/// hands its need on to a package. One grant reaches several consumers with different
/// limits. The reported consumers are exactly these, and the analysis is complete.
#[test]
fn every_consumer_of_every_grant_with_its_boundary() {
    let b = base("graph");
    let ledger = repo(
        &b.join("repos/ledger"),
        &[
            ("staal.json", json!({ "package": "ledger", "authority": { "net.http.post": [V1] } }).to_string()),
            (
                "n.staal",
                "pub fn record(h: cap net.http.post(only: [\"https://bank.example.com/v1/ledger\"]), x: Text) -> Int {\n    \
                 let r = h.post(\"https://bank.example.com/v1/ledger\", x)\n    return r.status\n}\n"
                    .to_string(),
            ),
        ],
    );
    let bank = repo(
        &b.join("repos/bank"),
        &[
            (
                "staal.json",
                json!({ "package": "bank", "authority": { "net.http.post": [V1], "net.http.get": [V1] },
                        "dependencies": { "ledger": { "git": b.join("repos/ledger").to_str().unwrap(), "commit": ledger } } })
                .to_string(),
            ),
            (
                "m.staal",
                format!(
                    r#"import ledger.n

pub fn refund(h: cap net.http.post(only: ["https://bank.example.com/v1/refunds"], uses: 2)) -> Int {{
    let r = h.post("https://bank.example.com/v1/refunds", "a")
    return r.status
}}

pub fn audit(h: cap net.http.post(only: ["{V1}"])) -> Int {{
    return pass_on(h)
}}

fn pass_on(h: cap net.http.post(only: ["{V1}"])) -> Int {{
    return n.record(h, "x")
}}

pub fn give_back(h: cap net.http.post(only: ["{V1}"])) -> cap net.http.post(only: ["{V1}"]) {{
    return h
}}

capability bank.api {{
    needs get: cap net.http.get(only: ["{V1}"])

    canonical(id: Text) -> Text {{
        return id
    }}

    action fetch(id: Text @target) -> Text {{
        let r = get.get("https://bank.example.com/v1/" + url_segment(id))
        return "status " + json(r.status)
    }}
}}
"#
                ),
            ),
        ],
    );
    let app = b.join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("staal.json"),
        json!({ "dependencies": { "bank": { "git": b.join("repos/bank").to_str().unwrap(), "commit": bank } } }).to_string(),
    )
    .unwrap();
    let main = app.join("main.staal");
    std::fs::write(
        &main,
        format!(
            r#"import bank.m {{ refund }}

capability shop.pay {{
    needs post: cap net.http.post(only: ["{ALL}"])

    canonical(id: Text) -> Text {{
        return id
    }}

    action send(id: Text @target) -> Text {{
        let r = post.post("https://bank.example.com/v1/" + url_segment(id), "a")
        let s = m.refund(post)
        return "sent " + json(r.status + s)
    }}
}}

fn hop1(h: cap net.http.post(only: ["{ALL}"])) -> Int {{
    return hop2(h)
}}

fn hop2(h: cap net.http.post(only: ["{ALL}"])) -> Int {{
    return refund(h)
}}

fn keep(h: cap net.http.post(only: ["{ALL}"])) -> cap net.http.post(only: ["{ALL}"]) {{
    return h
}}

fn go(h: cap net.http.post(only: ["{ALL}"]), g: cap net.http.get, p: cap shop.pay.send, q: cap bank.api.fetch) -> Int {{
    let a = hop1(h)
    let k = keep(h)
    let c = m.audit(k)
    let back = m.give_back(h)
    let d = m.refund(back)
    let r = g.get("https://example.org/")
    let s = p.send("x")
    let t = q.fetch("y")
    return a + c + d + r.status
}}
"#
        ),
    )
    .unwrap();
    staalc::packages::fetch(&app).unwrap();
    let s = schema(&main, "go");
    let refunds = json!({ "only": ["https://bank.example.com/v1/refunds"], "uses": 2 });
    assert_eq!(
        s["grants"],
        json!([
            { "name": "h", "cap": "net.http.post", "limits": { "only": [ALL] } },
            { "name": "g", "cap": "net.http.get", "limits": {} },
            { "name": "p", "cap": "shop.pay.send", "limits": {} },
            { "name": "q", "cap": "bank.api.fetch", "limits": {} },
        ])
    );
    let want = json!([
        agent("g", "net.http.get", json!({})),
        agent("h", "net.http.post", json!({ "only": [ALL] })),
        package("h", "net.http.post", "bank", "bank.m.audit", "h", json!({ "only": [V1] })),
        package("h", "net.http.post", "bank", "bank.m.give_back", "h", json!({ "only": [V1] })),
        package("h", "net.http.post", "bank", "bank.m.refund", "h", refunds.clone()),
        package("h", "net.http.post", "ledger", "ledger.n.record", "h", json!({ "only": ["https://bank.example.com/v1/ledger"] })),
        agent("p", "shop.pay.send", json!({})),
        need("p.post", "net.http.post", "shop.pay", "post", None, json!({ "only": [ALL] })),
        package("p.post", "net.http.post", "bank", "bank.m.refund", "h", refunds),
        agent("q", "bank.api.fetch", json!({})),
        need("q.get", "net.http.get", "bank.api", "get", Some("bank"), json!({ "only": [V1] })),
    ]);
    assert_eq!(s["consumers"], want, "{}", serde_json::to_string_pretty(&s["consumers"]).unwrap());
    assert_eq!(s["consumer_analysis"], json!({ "complete": true, "unresolved": [] }));
}

/// A program without packages: the agent is the only consumer of each grant, with the
/// limits its entry type writes, money with its currency's decimal places.
#[test]
fn entry_limits_are_reported_with_the_grant() {
    let b = base("entry");
    let main = b.join("main.staal");
    std::fs::write(
        &main,
        "fn go(pay: cap payments.refund(max: 500 USD, each: 50 USD, uses: 3, only: [\"A*\"]), mail: cap email.send) -> Int {\n    \
         return 1\n}\n",
    )
    .unwrap();
    let s = schema(&main, "go");
    let limits = json!({ "max": { "amount": "500.00", "currency": "USD" }, "each": { "amount": "50.00", "currency": "USD" },
                         "uses": 3, "only": ["A*"] });
    assert_eq!(s["grants"][0], json!({ "name": "pay", "cap": "payments.refund", "limits": limits }));
    assert_eq!(s["consumers"], json!([agent("mail", "email.send", json!({})), agent("pay", "payments.refund", limits)]));
    assert_eq!(s["consumer_analysis"]["complete"], true);
}

/// A capability returned through several functions, and through a function that can
/// return either of two grants, reaches a package slot: each grant that can arrive is
/// listed there.
#[test]
fn returns_are_followed_through_every_path() {
    let b = base("returns");
    let lib = repo(
        &b.join("repos/lib"),
        &[
            ("staal.json", json!({ "package": "lib", "authority": { "net.http.post": "any" } }).to_string()),
            ("m.staal", "pub fn use(h: cap net.http.post) -> Int {\n    return 1\n}\n".to_string()),
        ],
    );
    let app = b.join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("staal.json"),
        json!({ "dependencies": { "lib": { "git": b.join("repos/lib").to_str().unwrap(), "commit": lib } } }).to_string(),
    )
    .unwrap();
    let main = app.join("main.staal");
    std::fs::write(
        &main,
        r#"import lib.m

fn pick(a: cap net.http.post, b: cap net.http.post, first: Bool) -> cap net.http.post {
    if first {
        return a
    }
    return b
}

fn again(a: cap net.http.post, b: cap net.http.post, first: Bool) -> cap net.http.post {
    let x = pick(a, b, first)
    return x
}

fn go(a: cap net.http.post, b: cap net.http.post, c: cap net.http.post, first: Bool) -> Int {
    let x = again(a, b, first)
    return m.use(x)
}
"#,
    )
    .unwrap();
    staalc::packages::fetch(&app).unwrap();
    let s = schema(&main, "go");
    assert_eq!(
        s["consumers"],
        json!([
            agent("a", "net.http.post", json!({})),
            package("a", "net.http.post", "lib", "lib.m.use", "h", json!({})),
            agent("b", "net.http.post", json!({})),
            package("b", "net.http.post", "lib", "lib.m.use", "h", json!({})),
            agent("c", "net.http.post", json!({})),
        ])
    );
    assert_eq!(s["consumer_analysis"]["complete"], true);
}

/// Recursion: a function that passes its capability to itself, and on to a package
/// from inside the recursion.
#[test]
fn recursion_is_followed_to_a_fixed_point() {
    let b = base("recursion");
    let lib = repo(
        &b.join("repos/lib"),
        &[
            ("staal.json", json!({ "package": "lib", "authority": { "net.http.post": "any" } }).to_string()),
            ("m.staal", "pub fn use(h: cap net.http.post) -> Int {\n    return 1\n}\n".to_string()),
        ],
    );
    let app = b.join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("staal.json"),
        json!({ "dependencies": { "lib": { "git": b.join("repos/lib").to_str().unwrap(), "commit": lib } } }).to_string(),
    )
    .unwrap();
    let main = app.join("main.staal");
    std::fs::write(
        &main,
        r#"import lib.m

fn down(h: cap net.http.post, n: Int) -> cap net.http.post {
    if n <= 0 {
        return h
    }
    return down(h, n - 1)
}

fn go(h: cap net.http.post) -> Int {
    let x = down(h, 3)
    return m.use(x)
}
"#,
    )
    .unwrap();
    staalc::packages::fetch(&app).unwrap();
    let s = schema(&main, "go");
    assert_eq!(
        s["consumers"],
        json!([agent("h", "net.http.post", json!({})), package("h", "net.http.post", "lib", "lib.m.use", "h", json!({}))])
    );
    assert_eq!(s["consumer_analysis"]["complete"], true);
}

/// The ways of moving a capability the analysis would not follow are not part of the
/// language: a capability cannot be stored in a value, carried by a function value, or
/// reassigned, so no program does it.
#[test]
fn a_capability_cannot_be_stored_or_carried_by_a_function_value() {
    for (src, want) in [
        ("fn go(h: cap net.http.post) -> Int {\n    let xs = [h]\n    return 1\n}\n", "a list cannot hold a capability"),
        ("type Box { h: cap net.http.post }\nfn go() -> Int {\n    return 1\n}\n", "cannot hold a capability"),
        ("fn go(h: cap net.http.post) -> Int {\n    let t = (h, 1)\n    return 1\n}\n", "a tuple cannot hold a capability"),
        ("fn go(h: cap net.http.post) -> Int {\n    let o = some(h)\n    return 1\n}\n", "an optional value cannot hold a capability"),
        (
            "fn go(h: cap net.http.post) -> Int {\n    let f = fn(x: Int) -> Int {\n        let r = h.post(\"https://a.example/\", \"x\")\n        return x\n    }\n    return 1\n}\n",
            "a function value can never carry authority",
        ),
        (
            "fn k(f: fn(cap net.http.post) -> Int) -> Int {\n    return 1\n}\nfn go(h: cap net.http.post) -> Int {\n    return 1\n}\n",
            "a function value can never carry authority",
        ),
        (
            "fn go(h: cap net.http.post, g: cap net.http.post) -> Int {\n    let x = h\n    set x = g\n    return 1\n}\n",
            "never reassigned",
        ),
    ] {
        let e = check_errors(src);
        assert!(e.contains(want), "{src}\n{e}");
    }
}
