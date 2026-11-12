// What the journal view says of an action a crash left in doubt is what resume then
// does with it (RUNTIME.md section 11). Each case runs a real program, crashes it after
// its first action happened and before its result was recorded (`before_result:1`),
// reads the view, and resumes: an action the view says an operator settles stops the
// resumed run, in doubt; one it says resume settles, resume makes again, and the run
// finishes. The view reads the rule from the journal itself: each action's intent
// records the rule it was made under. A journal written before intents recorded it is
// still resumed, and the view says the rule is not recorded rather than guess it.
//
// Run with: cargo test --features failpoints --test view_test

#![cfg(feature = "failpoints")]

mod common;

use common::server::Server;
use serde_json::{json, Value as Json};
use staalc::interp::journal::{self, OpState, Settle};
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");

struct Case {
    dir: PathBuf,
    program: PathBuf,
    config: PathBuf,
    journal: PathBuf,
}

impl Case {
    fn new(name: &str, program: &str, config: impl FnOnce(&Path) -> Json) -> Case {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("view").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let program_file = dir.join("p.staal");
        std::fs::write(&program_file, program.replace("{dir}", dir.to_str().unwrap())).unwrap();
        let cfg = config(&dir);
        std::fs::write(dir.join("cfg.json"), cfg.to_string()).unwrap();
        Case { config: dir.join("cfg.json"), journal: dir.join("journal"), program: program_file, dir }
    }

    fn staalc(&self, command: &str, failpoint: Option<&str>) -> (Option<i32>, String) {
        let mut cmd = Command::new(BIN);
        cmd.arg(command).arg(&self.program).arg("--config").arg(&self.config).arg("--journal").arg(&self.journal);
        match failpoint {
            Some(fp) => cmd.env("STAALC_FAILPOINT", fp),
            None => cmd.env_remove("STAALC_FAILPOINT"),
        };
        let o = cmd.output().unwrap();
        (o.status.code(), String::from_utf8_lossy(&o.stderr).into_owned())
    }

    /// Runs it to the crash, and gives what the view says of the action in doubt and the
    /// rule its intent recorded.
    fn crash(&self) -> (Settle, Option<String>) {
        let (code, err) = self.staalc("run", Some("before_result:1"));
        assert_eq!(code, None, "{}: the run was not killed: {err}", self.dir.display());
        let v = journal::view(&self.journal).unwrap();
        let op = &v.operations[0];
        assert_eq!(op.op, "action");
        let OpState::InDoubt { settle, .. } = &op.state else { panic!("{}: not in doubt: {:?}", self.dir.display(), op.state) };
        let records = journal::read_records(&self.journal).unwrap();
        let intent = records.iter().find(|r| r["j"] == "intent").unwrap();
        (settle.clone(), intent.get("recovery").and_then(Json::as_str).map(str::to_string))
    }

    /// Resumes, and says whether the run finished (true) or stopped in doubt (false).
    fn resumed(&self) -> bool {
        let (code, err) = self.staalc("resume", None);
        match code {
            Some(0) => true,
            Some(_) if err.contains("may or may not have happened") => false,
            other => panic!("{}: resume ended {other:?}: {err}", self.dir.display()),
        }
    }
}

/// The view's answer and resume's agree, and both are the recorded rule.
fn agree(c: &Case, rule: &str) {
    let (settle, recorded) = c.crash();
    assert_eq!(recorded.as_deref(), Some(rule), "{}", c.dir.display());
    let operator = matches!(settle, Settle::Operator);
    assert_eq!(operator, rule == "operator", "{}: the view says {settle:?}", c.dir.display());
    assert!(matches!(settle, Settle::Operator | Settle::Resume(_)), "{settle:?}");
    assert_eq!(c.resumed(), !operator, "{}: resume did not do what the view said ({settle:?})", c.dir.display());
}

const SEND: &str = "fn main(mail: cap email.send) -> Int {\n    let r = mail.send(\"a@example.com\")\n    return 1\n}\n";

#[test]
fn an_adapters_action_is_settled_as_its_adapter_deduplicates() {
    for (idempotent, rule) in [(true, "resend_with_key"), (false, "operator")] {
        let c = Case::new(&format!("adapter-{idempotent}"), SEND, |d| {
            json!({ "entry": "main", "grants": { "mail": { "cap": "email.send" } },
                    "adapters": { "simulated": { "idempotent": idempotent, "world": d.join("world.jsonl").to_str().unwrap() } } })
        });
        agree(&c, rule);
    }
}

#[test]
fn a_file_write_is_made_again() {
    let c = Case::new("fs-write", "fn main(w: cap fs.write) -> Int {\n    let r = w.write(\"{dir}/out.txt\", \"x\")\n    return 1\n}\n", |d| {
        json!({ "entry": "main", "grants": { "w": { "cap": "fs.write", "only": [format!("{}/*", d.display())] } } })
    });
    agree(&c, "repeat");
}

/// A GET is made again; a POST is sent again with its key when its grant has one, and is
/// an operator's otherwise. The GET was the case the view got wrong before intents
/// recorded the rule: it said an operator settles it, and resume made it again.
#[test]
fn http_requests_are_settled_by_method_and_key() {
    let s = Server::start();
    let url = s.url("/fixed");
    let cases = [
        ("http-get", "get", "net.http.get", None, "repeat"),
        ("http-post-keyed", "post", "net.http.post", Some(true), "resend_with_key"),
        ("http-post", "post", "net.http.post", Some(false), "operator"),
    ];
    for (name, method, cap, keyed, rule) in cases {
        let call = if method == "get" { format!("h.get(\"{url}\")") } else { format!("h.post(\"{url}\", \"x\")") };
        let program = format!("fn main(h: cap {cap}) -> Int {{\n    let r = {call}\n    return r.status\n}}\n");
        let c = Case::new(name, &program, |_| {
            let mut g = json!({ "cap": cap, "private_addresses": ["127.0.0.1", "::1"] });
            if let Some(k) = keyed {
                g["idempotency_key"] = json!(k);
            }
            json!({ "entry": "main", "grants": { "h": g } })
        });
        agree(&c, rule);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_database_statement_is_settled_by_its_key() {
    for (keyed, rule) in [(true, "resend_with_key"), (false, "operator")] {
        let c = Case::new(
            &format!("sqlite-{keyed}"),
            "fn main(db: cap db.sqlite.execute) -> Int {\n    let n = db.execute(\"{dir}/x.db\", \"CREATE TABLE IF NOT EXISTS t (a TEXT)\")\n    return 1\n}\n",
            |d| json!({ "entry": "main", "grants": { "db": { "cap": "db.sqlite.execute", "only": [format!("{}/*.db", d.display())], "idempotency_key": keyed } } }),
        );
        agree(&c, rule);
    }
}

/// A journal whose intents do not record the rule (as staalc 0.28.0 and before wrote
/// them) is still resumed, and as before; the view says the rule is not recorded.
#[test]
fn a_journal_from_before_the_rule_was_recorded_is_resumed_and_not_guessed() {
    let c = Case::new("unrecorded", SEND, |d| {
        json!({ "entry": "main", "grants": { "mail": { "cap": "email.send" } },
                "adapters": { "simulated": { "idempotent": true, "world": d.join("world.jsonl").to_str().unwrap() } } })
    });
    let (_, recorded) = c.crash();
    assert_eq!(recorded.as_deref(), Some("resend_with_key"));
    // The journal as an older staalc would have written it.
    let text = std::fs::read_to_string(&c.journal).unwrap();
    let older: Vec<String> = text
        .lines()
        .map(|l| {
            let mut r: Json = serde_json::from_str(l).unwrap();
            if let Some(o) = r.as_object_mut() {
                o.remove("recovery");
            }
            r.to_string()
        })
        .collect();
    std::fs::write(&c.journal, older.join("\n") + "\n").unwrap();
    let v = journal::view(&c.journal).unwrap();
    assert!(matches!(&v.operations[0].state, OpState::InDoubt { settle: Settle::NotRecorded, .. }), "{:?}", v.operations[0].state);
    assert!(c.resumed(), "an older journal was not resumed");
    // A rule that is not one of the three, or on an operation that is not an action, is
    // corruption.
    let crashed = Case::new("bad-rule", SEND, |_| json!({ "entry": "main", "grants": { "mail": { "cap": "email.send" } } }));
    crashed.crash();
    let text = std::fs::read_to_string(&crashed.journal).unwrap().replace("\"recovery\":\"resend_with_key\"", "\"recovery\":\"maybe\"");
    std::fs::write(&crashed.journal, text).unwrap();
    let e = journal::view(&crashed.journal).unwrap_err();
    assert!(e.contains("is corrupt") && e.contains("only repeat, resend_with_key or operator"), "{e}");
}
