// Crash injection through library capabilities (the phase 3 gate). The program refunds
// through the Stripe library (a GET, then a POST to a service that deduplicates by
// Idempotency-Key) and texts through the Termii library (a POST to a service that does
// not). It is killed at every journal write and in every in-doubt window, resumed, and
// an SMS left in doubt is resolved from what the server really received. Then:
//   - the result equals that of an uninterrupted run;
//   - the refund took effect exactly once, and every attempt carried the same key;
//   - the SMS reached the server exactly once;
//   - the journal never contains the key.
//
// Run with: cargo test --features failpoints --test library_crash_test

#![cfg(feature = "failpoints")]

mod common;

use common::server::Server;
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Command;

static RESOLVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

const BIN: &str = env!("CARGO_BIN_EXE_intentc");
const KEY: &str = "sk_test_crash_lib_5e4d3c2b1a";

const PROGRAM: &str = r#"import stripe
import termii

fn go(payment: Text, amount: Money, phone: Text,
      refunds: cap stripe.refunds.create(max: 100 USD),
      texts: cap termii.sms.send(uses: 1)) -> Done {
    let r = refunds.create(payment, amount)
    let t = texts.send(phone, "Your refund is on its way.")
    return done(evidence: [r, t])
}
"#;

struct Case {
    dir: PathBuf,
    server: Server,
}

impl Case {
    fn new(name: &str) -> Case {
        let dir = std::fs::canonicalize(env!("CARGO_TARGET_TMPDIR")).unwrap().join("library-crash").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/capabilities");
        for lib in ["termii.int", "stripe.int"] {
            std::fs::copy(examples.join(lib), dir.join(lib)).unwrap();
        }
        std::fs::write(dir.join("prog.int"), PROGRAM).unwrap();
        let server = Server::start();
        let s = |p: &str| server.url(p);
        let cfg = json!({
            "entry": "go",
            "inputs": { "payment": "pi_usd", "amount": { "amount": "25.00", "currency": "USD" }, "phone": "+234 801 234 5678" },
            "grants": {
                "refunds": { "cap": "stripe.refunds.create", "only": ["pi_usd"], "needs": {
                    "get": { "cap": "net.http.get", "only": [s("/v1/payment_intents/*")] },
                    "post": { "cap": "net.http.post", "only": [s("/v1/refunds")], "idempotency_key": true },
                    "key": { "env": "INTENT_CRASH_LIB_KEY" },
                    "base": s("")
                } },
                "texts": { "cap": "termii.sms.send", "only": ["2348012345678"], "needs": {
                    "http": { "cap": "net.http.post", "only": [s("/api/sms/send")] },
                    "key": { "env": "INTENT_CRASH_LIB_KEY" },
                    "base": s(""),
                    "sender": "Shop"
                } }
            },
            "model": { "provider": "script", "responses": [] },
            "net": { "timeout_ms": 5000 }
        });
        std::fs::write(dir.join("cfg.json"), cfg.to_string()).unwrap();
        Case { dir, server }
    }

    fn intentc(&self, args: &[&str], failpoint: Option<&str>) -> (Option<i32>, String, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args).env("INTENT_CRASH_LIB_KEY", KEY);
        match failpoint {
            Some(fp) => cmd.env("INTENTC_FAILPOINT", fp),
            None => cmd.env_remove("INTENTC_FAILPOINT"),
        };
        let o = cmd.output().unwrap();
        (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().into(), String::from_utf8_lossy(&o.stderr).trim().into())
    }

    fn p(&self, f: &str) -> String {
        self.dir.join(f).to_str().unwrap().to_string()
    }

    fn run(&self, cmd: &str, failpoint: Option<&str>) -> (Option<i32>, String, String) {
        let (prog, cfg, j) = (self.p("prog.int"), self.p("cfg.json"), self.p("journal"));
        self.intentc(&[cmd, &prog, "--config", &cfg, "--journal", &j], failpoint)
    }

    fn journal(&self) -> Vec<Json> {
        std::fs::read_to_string(self.p("journal"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn sms_count(&self) -> usize {
        self.server.requests().iter().filter(|r| r.path == "/api/sms/send").count()
    }

    /// Resumes until done, resolving in doubt from what the server really received.
    fn finish(&self, label: &str) -> String {
        for _ in 0..10 {
            let (code, out, err) = self.run("resume", None);
            match code {
                Some(0) => return out,
                Some(3) => {
                    assert!(err.contains("in_doubt"), "{label}: {err}");
                    RESOLVED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let intent = self.journal().into_iter().rev().find(|r| r["j"] == "intent").unwrap();
                    assert_eq!(intent["body"]["target"], self.server.url("/api/sms/send"), "{label}: only the SMS is ever in doubt");
                    let j = self.p("journal");
                    let (c, _, e) = if self.sms_count() > 0 {
                        let body = "{\"message_id\": \"t-resolved\", \"code\": \"ok\"}";
                        self.intentc(&["resolve", "--journal", &j, "--status", "200", "--body", body], None)
                    } else {
                        self.intentc(&["resolve", "--journal", &j, "--retry"], None)
                    };
                    assert_eq!(c, Some(0), "{label}: resolve failed: {e}");
                }
                other => panic!("{label}: resume exited {other:?}: {err}"),
            }
        }
        panic!("{label}: did not finish");
    }
}

#[test]
fn library_actions_recover_from_a_crash_at_every_point() {
    let base = Case::new("baseline");
    let (code, result, err) = base.run("run", None);
    assert_eq!(code, Some(0), "baseline: {err}");
    let journal = base.journal();
    let ops = journal.iter().filter(|r| r["j"] == "intent").count();
    assert_eq!(ops, 3, "the GET and POST inside the refund, and the SMS");
    assert_eq!(journal.len(), 2 * ops + 2);

    let mut points: Vec<String> = (1..=journal.len()).map(|k| format!("after_write:{k}")).collect();
    points.extend((1..=ops).map(|n| format!("before_result:{n}")));
    for fp in &points {
        let c = Case::new(&fp.replace(':', "-"));
        let (code, _, err) = c.run("run", Some(fp));
        assert_eq!(code, None, "{fp}: the crash did not happen: {err}");
        let out = c.finish(fp);
        assert_eq!(c.server.effects("/v1/refunds"), 1, "{fp}: the refund took effect {} times", c.server.effects("/v1/refunds"));
        let refunds: Vec<_> = c.server.requests().into_iter().filter(|r| r.path == "/v1/refunds").collect();
        let keys: std::collections::HashSet<_> = refunds.iter().map(|r| r.headers["idempotency-key"].clone()).collect();
        assert_eq!(keys.len(), 1, "{fp}: every attempt must carry the same key");
        assert!(refunds.iter().all(|r| r.body == "payment_intent=pi_usd&amount=2500"), "{fp}");
        assert_eq!(c.sms_count(), 1, "{fp}: the SMS reached the server {} times", c.sms_count());
        // Receipt ids come from the server's answers, whose numbering depends on how
        // many requests came before; compare everything else.
        let norm = |s: &str| -> Json {
            let mut v: Json = serde_json::from_str(s).unwrap();
            for e in v["done"].as_array_mut().unwrap() {
                e["receipt"] = json!("?");
            }
            v
        };
        assert_eq!(norm(&out), norm(&result), "{fp}: result differs from the uninterrupted run");
        let text = std::fs::read_to_string(c.p("journal")).unwrap();
        assert!(!text.contains(KEY), "{fp}: the journal contains the key");
        let before = c.server.requests().len();
        assert_eq!(c.run("resume", None).1, out, "{fp}");
        assert_eq!(c.server.requests().len(), before, "{fp}: replay contacted the server");
    }
    let resolved = RESOLVED.load(std::sync::atomic::Ordering::SeqCst);
    assert!(resolved >= 1, "no crash left the SMS in doubt: the in-doubt path was not tested");
    println!("{} crash points recovered, {resolved} in doubt and resolved", points.len());
}
