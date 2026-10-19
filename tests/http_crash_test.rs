// Crash injection for net.http and fs (the phase 2 gate). Like tests/crash_test.rs,
// but the outside world is a real HTTP server in this test process and real files.
//
// The program: a GET (safe to repeat), a POST to a service that deduplicates by
// Idempotency-Key and carries a secret, a POST to one that does not, and a file write.
// It is killed at every journal write and in every in-doubt window, resumed, and an
// operation left in doubt is resolved the way a correct operator would: by asking the
// server what it received. Then:
//   - the result and the written file equal those of an uninterrupted run;
//   - the deduplicated POST took effect exactly once (repeats carry the same key);
//   - the other POST reached the server exactly once;
//   - the journal never contains the secret.
//
// Run with: cargo test --features failpoints --test http_crash_test

#![cfg(feature = "failpoints")]

mod common;

use common::server::Server;
use serde_json::{json, Value as Json};
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const KEY: &str = "sk_test_4f9a8b7c6d5e3f21";

struct Case {
    dir: PathBuf,
    server: Server,
}

impl Case {
    fn new(name: &str) -> Case {
        let dir = std::fs::canonicalize(env!("CARGO_TARGET_TMPDIR")).unwrap().join("http-crash").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("out")).unwrap();
        let server = Server::start();
        let s = |p: &str| server.url(p);
        let out = dir.join("out");
        let out = out.to_str().unwrap();
        let program = format!(
            r#"type Payment {{ order: Text, cents: Int }}

fn go(key: Secret,
      quote: cap net.http.get(only: ["{fixed}"]),
      pay: cap net.http.post(only: ["{send}"]),
      notify: cap net.http.post(only: ["{fixed}"]),
      files: cap fs.write(only: ["{out}/*"])) -> Done {{
    let q = quote.get("{fixed}")
    let auth = header("Authorization", fill_secret("Bearer {{secret}}", key))
    let p = pay.post("{send}", json(Payment {{ order: "A1", cents: 500 }}), [auth])
    let n = notify.post("{fixed}", "paid A1")
    let w = files.write("{out}/report.txt", q.body + " " + p.body + " " + n.body)
    return done(evidence: [w])
}}
"#,
            fixed = s("/fixed"),
            send = s("/send"),
        );
        std::fs::write(dir.join("prog.staal"), program).unwrap();
        let cfg = json!({
            "entry": "go",
            "secrets": { "key": { "env": "STAAL_CRASH_KEY" } },
            "grants": {
                "quote": { "cap": "net.http.get" },
                "pay": { "cap": "net.http.post", "idempotency_key": true },
                "notify": { "cap": "net.http.post" },
                "files": { "cap": "fs.write" }
            },
            "model": { "provider": "script", "responses": [] },
            "net": { "timeout_ms": 5000 }
        });
        std::fs::write(dir.join("cfg.json"), cfg.to_string()).unwrap();
        Case { dir, server }
    }

    fn staalc(&self, args: &[&str], failpoint: Option<&str>) -> (Option<i32>, String, String) {
        let mut cmd = Command::new(BIN);
        cmd.args(args).env("STAAL_CRASH_KEY", KEY);
        match failpoint {
            Some(fp) => cmd.env("STAALC_FAILPOINT", fp),
            None => cmd.env_remove("STAALC_FAILPOINT"),
        };
        let o = cmd.output().unwrap();
        (o.status.code(), String::from_utf8_lossy(&o.stdout).trim().into(), String::from_utf8_lossy(&o.stderr).trim().into())
    }

    fn p(&self, f: &str) -> String {
        self.dir.join(f).to_str().unwrap().to_string()
    }

    fn run(&self, cmd: &str, failpoint: Option<&str>) -> (Option<i32>, String, String) {
        let (prog, cfg, j) = (self.p("prog.staal"), self.p("cfg.json"), self.p("journal"));
        self.staalc(&[cmd, &prog, "--config", &cfg, "--journal", &j], failpoint)
    }

    fn journal(&self) -> Vec<Json> {
        std::fs::read_to_string(self.p("journal"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn report(&self) -> String {
        std::fs::read_to_string(self.dir.join("out/report.txt")).unwrap_or_default()
    }

    /// Resumes until done, resolving in doubt from what the server really received.
    fn finish(&self, label: &str) -> String {
        for _ in 0..10 {
            let (code, out, err) = self.run("resume", None);
            match code {
                Some(0) => return out,
                Some(3) => {
                    assert!(err.contains("in_doubt"), "{label}: {err}");
                    let intent = self.journal().into_iter().rev().find(|r| r["j"] == "intent").unwrap();
                    assert_eq!(intent["body"]["capability"], "net.http.post", "{label}: only the plain POST is ever in doubt");
                    assert_eq!(intent["body"]["idempotency_key"], false, "{label}");
                    let j = self.p("journal");
                    let received = self.server.requests().iter().any(|r| r.method == "POST" && r.path == "/fixed");
                    let (c, _, e) = if received {
                        self.staalc(&["resolve", "--journal", &j, "--status", "200", "--body", "fixed"], None)
                    } else {
                        self.staalc(&["resolve", "--journal", &j, "--retry"], None)
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
fn http_and_files_recover_from_a_crash_at_every_point() {
    let base = Case::new("baseline");
    let (code, result, err) = base.run("run", None);
    assert_eq!(code, Some(0), "baseline: {err}");
    let journal = base.journal();
    let ops = journal.iter().filter(|r| r["j"] == "intent").count();
    assert_eq!(ops, 4, "GET, two POSTs and a write");
    assert_eq!(journal.len(), 2 * ops + 2);
    let report = base.report();
    assert_eq!(report, "fixed {\"message_id\": \"sent\"} fixed");

    let mut points: Vec<String> = (1..=journal.len()).map(|k| format!("after_write:{k}")).collect();
    points.extend((1..=ops).map(|n| format!("before_result:{n}")));
    for fp in &points {
        let c = Case::new(&fp.replace(':', "-"));
        let (code, _, err) = c.run("run", Some(fp));
        assert_eq!(code, None, "{fp}: the crash did not happen: {err}");
        let out = c.finish(fp);
        assert_eq!(out, result, "{fp}: result differs from the uninterrupted run");
        assert_eq!(c.report(), report, "{fp}: the file differs");
        assert_eq!(c.server.effects("/send"), 1, "{fp}: the deduplicated payment took effect {} times", c.server.effects("/send"));
        let sends: Vec<_> = c.server.requests().into_iter().filter(|r| r.path == "/send").collect();
        let keys: std::collections::HashSet<_> = sends.iter().map(|r| r.headers["idempotency-key"].clone()).collect();
        assert_eq!(keys.len(), 1, "{fp}: every attempt must carry the same key");
        assert!(sends.iter().all(|r| r.headers["authorization"] == format!("Bearer {KEY}")), "{fp}");
        let notifies = c.server.requests().into_iter().filter(|r| r.method == "POST" && r.path == "/fixed").count();
        assert_eq!(notifies, 1, "{fp}: the plain POST reached the server {notifies} times");
        let text = std::fs::read_to_string(c.p("journal")).unwrap();
        assert!(!text.contains(KEY), "{fp}: the journal contains the secret");
        // Resuming the finished run changes nothing anywhere.
        let before = c.server.requests().len();
        assert_eq!(c.run("resume", None).1, result, "{fp}");
        assert_eq!(c.server.requests().len(), before, "{fp}: replay contacted the server");
    }
    println!("{} crash points recovered", points.len());
}
