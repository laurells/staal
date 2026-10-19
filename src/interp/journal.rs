// The durable journal (RUNTIME.md section 11).
//
// Every external operation (token count, model call, action) is recorded twice:
// an intent before it, a result after it, each flushed to disk before the run
// continues. Resuming re-executes the program and serves recorded results instead
// of contacting the world, so a killed run finishes without repeating or losing
// anything.
//
// Rules for this module:
//  - A record is acknowledged only once `sync_data` has returned.
//  - After any failed write the journal is poisoned and refuses further writes:
//    appending after a half-written line would corrupt the middle of the file.
//  - A journal is validated in full before anything runs. Only the last line may
//    be torn (a crash during its write); any other damage is refused.
//  - One process at a time: an OS lock, freed automatically if the process dies.

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub const VERSION: u64 = 1;

/// The SHA-256 of `bytes`, as 64 lowercase hex digits.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    s
}

/// A fresh run id: 128 bits from the operating system's random source.
pub fn new_run_id() -> Result<String, String> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).map_err(|e| format!("could not read the operating system's random source: {e}"))?;
    Ok(hex(&buf))
}

/// What the runtime must do for one external operation.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// The journal holds this operation's result record: use it; do not contact the world.
    Replay(Json),
    /// Perform the operation now, then call `finish`.
    Perform { n: u64, key: String },
    /// An earlier attempt recorded the intent but not the result: the operation may or
    /// may not have happened. The caller decides what is safe, then calls `finish`.
    InDoubt { n: u64, key: String },
}

/// How an operator resolves an action left in doubt (`intentc resolve`).
pub enum Resolution {
    /// It happened; this is the service's receipt.
    Performed { receipt: String },
    /// Treat it as failed: the program sees an action failure.
    Failed { message: String },
    /// It did not happen: perform it on resume. The authorization covers one attempt;
    /// if that attempt is itself interrupted, the action is in doubt again.
    Retry,
}

#[derive(Debug)]
pub struct Journal {
    /// None: an in-memory journal, for runs without durability.
    file: Option<File>,
    poisoned: bool,
    /// Recorded intents and results not yet replayed.
    pending: VecDeque<Json>,
    /// The `end` record of a run that had already finished.
    recorded_end: Option<Json>,
    run_id: String,
    next_n: u64,
    failpoint: Failpoint,
    writes: u64,
}

impl Journal {
    /// A journal that keeps nothing: the run is not durable.
    pub fn memory() -> Result<Journal, String> {
        Ok(Journal::with(None, new_run_id()?, VecDeque::new(), None))
    }

    fn with(file: Option<File>, run_id: String, pending: VecDeque<Json>, recorded_end: Option<Json>) -> Journal {
        Journal { file, poisoned: false, pending, recorded_end, run_id, next_n: 1, failpoint: Failpoint::from_env(), writes: 0 }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Whether this journal had already recorded operations when opened.
    pub fn is_resuming(&self) -> bool {
        !self.pending.is_empty() || self.recorded_end.is_some()
    }

    /// Creates a new journal for a new run. Fails if the file already exists.
    pub fn create(path: &Path, program_sha256: &str, config_sha256: &str) -> Result<Journal, String> {
        let shown = path.display();
        let file = OpenOptions::new().append(true).create_new(true).open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("journal {shown} already exists; use `intentc resume` to continue that run")
            } else {
                format!("cannot create journal {shown}: {e}")
            }
        })?;
        lock(&file, path)?;
        let run_id = new_run_id()?;
        let mut j = Journal::with(Some(file), run_id.clone(), VecDeque::new(), None);
        j.write(&json!({
            "j": "start", "version": VERSION, "run_id": run_id,
            "program_sha256": program_sha256, "config_sha256": config_sha256,
        }))?;
        // Make the new file's directory entry durable too.
        sync_parent_dir(path)?;
        Ok(j)
    }

    /// Opens an existing journal to resume its run. The program and config must be
    /// exactly those it was started with.
    pub fn resume(path: &Path, program_sha256: &str, config_sha256: &str) -> Result<Journal, String> {
        let (file, records) = open_existing(path)?;
        let start = records.first().ok_or_else(|| {
            format!(
                "journal {} is empty: the run never started, so nothing was done; start a new run",
                path.display()
            )
        })?;
        check_start(start, program_sha256, config_sha256, path)?;
        let run_id = start["run_id"].as_str().unwrap_or_default().to_string(); // checked by check_start
        let (pending, recorded_end) = check_body(&records[1..], path)?;
        Ok(Journal::with(Some(file), run_id, pending, recorded_end))
    }

    /// Records an operator's answer for an action left in doubt.
    pub fn resolve(path: &Path, resolution: Resolution) -> Result<String, String> {
        let (file, records) = open_existing(path)?;
        let start = records.first().ok_or_else(|| format!("journal {} is empty", path.display()))?;
        if start["j"] != "start" {
            return Err(format!("journal {}: first record is not a start record", path.display()));
        }
        let (pending, recorded_end) = check_body(&records[1..], path)?;
        if recorded_end.is_some() {
            return Err("this run has already finished; nothing is in doubt".into());
        }
        match pending.back().and_then(|r| r["j"].as_str()) {
            Some("intent" | "attempt") => {}
            Some("retry") => return Err("a retry is already authorized for the action in doubt; resume the run".into()),
            _ => return Err("no operation is in doubt in this journal".into()),
        }
        let intent = pending.iter().rev().find(|r| r["j"] == "intent").ok_or("no operation is in doubt in this journal")?;
        if intent["op"] != "action" {
            return Err(format!(
                "operation {} in doubt is a {}, which resume retries by itself; nothing to resolve",
                intent["n"], intent["op"]
            ));
        }
        let n = intent["n"].as_u64().unwrap_or_default(); // checked by check_body
        let what = format!(
            "{}.{}",
            intent["body"]["capability"].as_str().unwrap_or("?"),
            intent["body"]["method"].as_str().unwrap_or("?")
        );
        let mut j = Journal::with(Some(file), String::new(), VecDeque::new(), None);
        let (result, summary) = match resolution {
            Resolution::Retry => {
                j.write(&json!({ "j": "retry", "n": n }))?;
                return Ok(format!("authorized one more attempt at {what} (operation {n})"));
            }
            Resolution::Performed { receipt } => {
                if receipt.is_empty() {
                    return Err("--performed needs the receipt id the service gave for this action".into());
                }
                (Ok(json!({ "receipt": receipt })), format!("recorded {what} (operation {n}) as performed"))
            }
            Resolution::Failed { message } => (
                Err(format!("resolved by operator: {message}")),
                format!("recorded {what} (operation {n}) as failed"),
            ),
        };
        j.finish(n, result)?;
        Ok(summary)
    }

    /// Begins external operation `op` with the given intent `body`.
    pub fn begin(&mut self, op: &str, body: Json) -> Result<Step, String> {
        let n = self.next_n;
        self.next_n += 1;
        let key = format!("{}-{n}", self.run_id);
        let intent = json!({ "j": "intent", "n": n, "op": op, "key": key, "body": body });
        if let Some(recorded) = self.pending.pop_front() {
            if recorded != intent {
                return Err(divergence(n, &recorded, &intent));
            }
            // check_body guarantees what may follow an intent: its result, or operator
            // `retry` authorizations each followed by an `attempt`, possibly cut short
            // by the end of the journal.
            let mut authorized = false;
            loop {
                match self.pending.pop_front() {
                    Some(r) if r["j"] == "result" => return Ok(Step::Replay(r)),
                    Some(r) if r["j"] == "retry" => authorized = true,
                    Some(_) => authorized = false, // an attempt: it used up the authorization
                    None if authorized => {
                        // Record the attempt before making it, so a crash during it leaves
                        // the action in doubt again rather than silently retried.
                        self.write(&json!({ "j": "attempt", "n": n }))?;
                        return Ok(Step::Perform { n, key });
                    }
                    None => return Ok(Step::InDoubt { n, key }),
                }
            }
        }
        if self.recorded_end.is_some() {
            return Err(format!(
                "operation {n}: the journal records that this run had finished, but replay reached a further \
                 {op}; the run is not reproducible"
            ));
        }
        self.write(&intent)?;
        Ok(Step::Perform { n, key })
    }

    /// Records the result of operation `n`.
    pub fn finish(&mut self, n: u64, result: Result<Json, String>) -> Result<(), String> {
        self.failpoint.before_result(n);
        let rec = match result {
            Ok(v) => json!({ "j": "result", "n": n, "ok": v }),
            Err(e) => json!({ "j": "result", "n": n, "err": e }),
        };
        self.write(&rec)
    }

    /// Records how the run ended. On resume of a finished run, checks it matches.
    pub fn end(&mut self, rec: Json) -> Result<(), String> {
        if !self.pending.is_empty() {
            return Err(format!(
                "the run finished, but the journal holds {} more recorded record(s); the run is not reproducible",
                self.pending.len()
            ));
        }
        let mut full = json!({ "j": "end" });
        if let (Some(dst), Some(src)) = (full.as_object_mut(), rec.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        match &self.recorded_end {
            Some(recorded) if *recorded == full => Ok(()),
            Some(recorded) => Err(format!("the run ended differently from its journal: recorded {recorded}, now {full}")),
            None => self.write(&full),
        }
    }

    fn write(&mut self, rec: &Json) -> Result<(), String> {
        let Some(file) = &mut self.file else { return Ok(()) };
        if self.poisoned {
            return Err("the journal is unusable after an earlier write failure".into());
        }
        let mut line = rec.to_string();
        line.push('\n');
        let res = file.write_all(line.as_bytes()).and_then(|()| file.sync_data());
        if let Err(e) = res {
            self.poisoned = true;
            return Err(format!("could not write the journal: {e}"));
        }
        self.writes += 1;
        self.failpoint.after_write(self.writes);
        Ok(())
    }
}

fn lock(file: &File, path: &Path) -> Result<(), String> {
    file.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => {
            format!("journal {} is in use by another intentc process", path.display())
        }
        std::fs::TryLockError::Error(e) => format!("cannot lock journal {}: {e}", path.display()),
    })
}

#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> Result<(), String> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    File::open(dir).and_then(|d| d.sync_all()).map_err(|e| format!("cannot sync directory {}: {e}", dir.display()))
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Opens, locks and reads an existing journal. A torn last line is cut off: its write
/// was never acknowledged. Any other malformed line is refused as corruption.
fn open_existing(path: &Path) -> Result<(File, Vec<Json>), String> {
    let shown = path.display();
    let mut file = OpenOptions::new().read(true).append(true).open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("journal {shown} does not exist")
        } else {
            format!("cannot open journal {shown}: {e}")
        }
    })?;
    lock(&file, path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| format!("cannot read journal {shown}: {e}"))?;
    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    if complete < bytes.len() {
        file.set_len(complete as u64)
            .and_then(|()| file.sync_data())
            .map_err(|e| format!("cannot repair torn last line of journal {shown}: {e}"))?;
    }
    let text = std::str::from_utf8(&bytes[..complete]).map_err(|_| format!("journal {shown} is corrupt: not UTF-8"))?;
    let mut records = Vec::new();
    for (i, line) in text.split_terminator('\n').enumerate() {
        let rec: Json =
            serde_json::from_str(line).map_err(|e| format!("journal {shown} is corrupt at line {}: {e}", i + 1))?;
        if !rec.is_object() {
            return Err(format!("journal {shown} is corrupt at line {}: not a record", i + 1));
        }
        records.push(rec);
    }
    Ok((file, records))
}

fn check_start(start: &Json, program_sha256: &str, config_sha256: &str, path: &Path) -> Result<(), String> {
    let shown = path.display();
    if start["j"] != "start" {
        return Err(format!("journal {shown}: first record is not a start record"));
    }
    if start["version"] != VERSION {
        return Err(format!("journal {shown} has version {}, but this intentc reads version {VERSION}", start["version"]));
    }
    let id_ok = start["run_id"].as_str().is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    if !id_ok {
        return Err(format!("journal {shown}: start record has no valid run id"));
    }
    if start["program_sha256"] != program_sha256 {
        return Err(format!(
            "journal {shown} was started with a different program; resume needs the identical program, every module included"
        ));
    }
    if start["config_sha256"] != config_sha256 {
        return Err(format!(
            "journal {shown} was started with a different run config; resume needs the identical config"
        ));
    }
    Ok(())
}

/// Validates the records after `start`: intent n, result n, intent n+1, ... from
/// n = 1, where only the last intent may lack a result, and `end` may only come last
/// with nothing in doubt. Returns the intents and results, and the end record.
fn check_body(records: &[Json], path: &Path) -> Result<(VecDeque<Json>, Option<Json>), String> {
    let bad = |i: usize, why: String| format!("journal {} is corrupt at line {}: {why}", path.display(), i + 2);
    let mut pending = VecDeque::new();
    let mut end = None;
    let mut expect_n: u64 = 1;
    // Within an open operation: the last record was its intent or an attempt (the
    // operation is in doubt), or a retry authorization (an attempt must come next).
    let mut open_intent = false;
    let mut last_was_retry = false;
    let mut open_op = "";
    for (i, rec) in records.iter().enumerate() {
        if end.is_some() {
            return Err(bad(i, "a record follows the end record".into()));
        }
        let n = rec["n"].as_u64();
        match rec["j"].as_str() {
            Some("retry") | Some("attempt") => {
                let is_retry = rec["j"] == "retry";
                if !open_intent || n != Some(expect_n) {
                    return Err(bad(i, format!("{} for operation {} that is not in doubt", rec["j"], rec["n"])));
                }
                if open_op != "action" {
                    return Err(bad(i, format!("{} recorded for a {open_op}, which is never resolved by hand", rec["j"])));
                }
                if is_retry == last_was_retry {
                    let what = if is_retry { "two retries without an attempt" } else { "an attempt without a retry" };
                    return Err(bad(i, what.into()));
                }
                last_was_retry = is_retry;
            }
            Some("intent") => {
                if open_intent {
                    return Err(bad(i, format!("operation {} has no result but is followed by another", expect_n)));
                }
                if n != Some(expect_n) {
                    return Err(bad(i, format!("expected operation {expect_n}, found {}", rec["n"])));
                }
                let op_ok = matches!(rec["op"].as_str(), Some("count" | "model" | "action"));
                if !op_ok || !rec["key"].is_string() || rec.get("body").is_none() {
                    return Err(bad(i, "malformed intent record".into()));
                }
                open_intent = true;
                last_was_retry = false;
                open_op = rec["op"].as_str().unwrap_or_default();
            }
            Some("result") => {
                if !open_intent || n != Some(expect_n) {
                    return Err(bad(i, format!("result for operation {} does not follow its intent", rec["n"])));
                }
                if last_was_retry {
                    return Err(bad(i, "a result follows a retry with no attempt".into()));
                }
                if rec.get("ok").is_some() == rec.get("err").is_some() {
                    return Err(bad(i, "a result must have exactly one of \"ok\" or \"err\"".into()));
                }
                open_intent = false;
                last_was_retry = false;
                expect_n += 1;
            }
            Some("end") => {
                if open_intent {
                    return Err(bad(i, "the run ended with an operation in doubt".into()));
                }
                end = Some(rec.clone());
                continue;
            }
            _ => return Err(bad(i, format!("unknown record type {}", rec["j"]))),
        }
        pending.push_back(rec.clone());
    }
    Ok((pending, end))
}

fn divergence(n: u64, recorded: &Json, now: &Json) -> String {
    let summary = |r: &Json| -> String {
        match r["op"].as_str() {
            Some("action") => format!(
                "action {}.{}",
                r["body"]["capability"].as_str().unwrap_or("?"),
                r["body"]["method"].as_str().unwrap_or("?")
            ),
            Some(op) => op.to_string(),
            None => r.to_string(),
        }
    };
    format!(
        "operation {n}: the journal recorded a {}, but replay now makes a different {}; the program, its \
         config or intentc has changed since the run started, so it cannot be resumed safely",
        summary(recorded),
        summary(now)
    )
}

// ---------------------------------------------------------------------------
// Failpoints: test-only crash injection. Compiled out unless the `failpoints`
// feature is enabled, so a production build cannot crash this way.
//
//   INTENTC_FAILPOINT=after_write:N     abort right after the Nth journal record is durable
//   INTENTC_FAILPOINT=before_result:N   abort after operation N happened, before its result is written

#[derive(Debug, Default, Clone, Copy)]
struct Failpoint {
    #[cfg(feature = "failpoints")]
    after_write: Option<u64>,
    #[cfg(feature = "failpoints")]
    before_result: Option<u64>,
}

impl Failpoint {
    #[cfg(feature = "failpoints")]
    fn from_env() -> Failpoint {
        let mut fp = Failpoint::default();
        if let Ok(spec) = std::env::var("INTENTC_FAILPOINT") {
            match spec.split_once(':').and_then(|(k, v)| v.parse::<u64>().ok().map(|n| (k, n))) {
                Some(("after_write", n)) => fp.after_write = Some(n),
                Some(("before_result", n)) => fp.before_result = Some(n),
                _ => eprintln!("intentc: ignoring malformed INTENTC_FAILPOINT={spec}"),
            }
        }
        fp
    }

    #[cfg(not(feature = "failpoints"))]
    fn from_env() -> Failpoint {
        Failpoint::default()
    }

    #[cfg_attr(not(feature = "failpoints"), allow(unused_variables))]
    fn after_write(&self, writes: u64) {
        #[cfg(feature = "failpoints")]
        if self.after_write == Some(writes) {
            std::process::abort();
        }
    }

    #[cfg_attr(not(feature = "failpoints"), allow(unused_variables))]
    fn before_result(&self, n: u64) {
        #[cfg(feature = "failpoints")]
        if self.before_result == Some(n) {
            std::process::abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("intentc-journal-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn sha256_matches_the_published_test_vectors() {
        // FIPS 180-2 / NIST examples.
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn run_ids_are_distinct_and_well_formed() {
        let a = new_run_id().unwrap();
        let b = new_run_id().unwrap();
        assert_ne!(a, b);
        assert!(a.len() == 32 && a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_new_run_replays_exactly_after_reopening() {
        let p = tmp("roundtrip");
        let mut j = Journal::create(&p, "P", "C").unwrap();
        let Step::Perform { n, key } = j.begin("model", json!({ "q": 1 })).unwrap() else { panic!() };
        assert_eq!(n, 1);
        assert!(key.ends_with("-1"));
        j.finish(n, Ok(json!({ "a": 2 }))).unwrap();
        let Step::Perform { n, .. } = j.begin("action", json!({ "x": "y" })).unwrap() else { panic!() };
        j.finish(n, Err("declined".into())).unwrap();
        j.end(json!({ "ok": true, "result": 7 })).unwrap();
        let run_id = j.run_id().to_string();
        drop(j);

        let mut r = Journal::resume(&p, "P", "C").unwrap();
        assert_eq!(r.run_id(), run_id);
        assert!(r.is_resuming());
        assert_eq!(r.begin("model", json!({ "q": 1 })).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": { "a": 2 } })));
        assert_eq!(r.begin("action", json!({ "x": "y" })).unwrap(), Step::Replay(json!({ "j": "result", "n": 2, "err": "declined" })));
        r.end(json!({ "ok": true, "result": 7 })).unwrap();
        // Resuming again is idempotent.
        drop(r);
        let mut r = Journal::resume(&p, "P", "C").unwrap();
        r.begin("model", json!({ "q": 1 })).unwrap();
        r.begin("action", json!({ "x": "y" })).unwrap();
        assert!(r.end(json!({ "ok": false })).is_err(), "a different ending is divergence");
    }

    #[test]
    fn an_intent_without_result_is_in_doubt() {
        let p = tmp("doubt");
        let mut j = Journal::create(&p, "P", "C").unwrap();
        let Step::Perform { key, .. } = j.begin("action", json!(1)).unwrap() else { panic!() };
        drop(j); // crash before finish
        let mut r = Journal::resume(&p, "P", "C").unwrap();
        assert_eq!(r.begin("action", json!(1)).unwrap(), Step::InDoubt { n: 1, key: key.clone() });
        r.finish(1, Ok(json!({ "receipt": "R" }))).unwrap();
        let Step::Perform { n, .. } = r.begin("count", json!(2)).unwrap() else { panic!() };
        assert_eq!(n, 2);
    }

    #[test]
    fn divergence_is_detected() {
        let p = tmp("diverge");
        let mut j = Journal::create(&p, "P", "C").unwrap();
        let Step::Perform { n, .. } = j.begin("action", json!({ "capability": "payments.refund", "method": "refund" })).unwrap() else { panic!() };
        j.finish(n, Ok(json!({}))).unwrap();
        drop(j);
        let mut r = Journal::resume(&p, "P", "C").unwrap();
        let e = r.begin("action", json!({ "capability": "email.send", "method": "send" })).unwrap_err();
        assert!(e.contains("payments.refund") && e.contains("email.send"), "{e}");
    }

    #[test]
    fn program_and_config_must_be_identical() {
        let p = tmp("hashes");
        drop(Journal::create(&p, "P", "C").unwrap());
        assert!(Journal::resume(&p, "P2", "C").unwrap_err().contains("different program"));
        assert!(Journal::resume(&p, "P", "C2").unwrap_err().contains("different run config"));
        assert!(Journal::create(&p, "P", "C").unwrap_err().contains("already exists"));
    }

    #[test]
    fn a_torn_last_line_is_discarded() {
        let p = tmp("torn");
        let mut j = Journal::create(&p, "P", "C").unwrap();
        let Step::Perform { n, .. } = j.begin("count", json!(1)).unwrap() else { panic!() };
        j.finish(n, Ok(json!(5))).unwrap();
        drop(j);
        // A crash in the middle of writing the next intent.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"j\":\"intent\",\"n\":2,\"op\":\"mod").unwrap();
        drop(f);
        let mut r = Journal::resume(&p, "P", "C").unwrap();
        assert!(matches!(r.begin("count", json!(1)).unwrap(), Step::Replay(_)));
        assert!(matches!(r.begin("model", json!(2)).unwrap(), Step::Perform { n: 2, .. }), "op 2 never started");
        drop(r);
        // The repaired file is well formed.
        Journal::resume(&p, "P", "C").unwrap();
    }

    #[test]
    fn damage_before_the_last_line_is_refused() {
        let p = tmp("corrupt");
        let mut j = Journal::create(&p, "P", "C").unwrap();
        let Step::Perform { n, .. } = j.begin("count", json!(1)).unwrap() else { panic!() };
        j.finish(n, Ok(json!(5))).unwrap();
        drop(j);
        let text = std::fs::read_to_string(&p).unwrap().replace("\"result\"", "\"resul");
        std::fs::write(&p, text).unwrap();
        assert!(Journal::resume(&p, "P", "C").unwrap_err().contains("corrupt"));
    }

    #[test]
    fn structure_is_validated_before_anything_runs() {
        let p = tmp("structure");
        drop(Journal::create(&p, "P", "C").unwrap());
        let base = std::fs::read_to_string(&p).unwrap();
        let cases = [
            ("{\"j\":\"result\",\"n\":1,\"ok\":1}\n", "does not follow its intent"),
            ("{\"j\":\"intent\",\"n\":2,\"op\":\"count\",\"key\":\"k\",\"body\":1}\n", "expected operation 1"),
            (
                "{\"j\":\"intent\",\"n\":1,\"op\":\"count\",\"key\":\"k\",\"body\":1}\n{\"j\":\"intent\",\"n\":2,\"op\":\"count\",\"key\":\"k\",\"body\":1}\n",
                "has no result",
            ),
            (
                "{\"j\":\"intent\",\"n\":1,\"op\":\"count\",\"key\":\"k\",\"body\":1}\n{\"j\":\"result\",\"n\":1,\"ok\":1,\"err\":\"x\"}\n",
                "exactly one",
            ),
            ("{\"j\":\"end\",\"ok\":true}\n{\"j\":\"end\",\"ok\":true}\n", "follows the end"),
            ("{\"j\":\"intent\",\"n\":1,\"op\":\"teleport\",\"key\":\"k\",\"body\":1}\n", "malformed intent"),
        ];
        for (extra, want) in cases {
            std::fs::write(&p, format!("{base}{extra}")).unwrap();
            let e = Journal::resume(&p, "P", "C").unwrap_err();
            assert!(e.contains(want), "{extra:?}: {e}");
        }
    }

    #[test]
    fn only_one_process_may_hold_a_journal() {
        let p = tmp("lock");
        let held = Journal::create(&p, "P", "C").unwrap();
        // The lock is per open file description, so a second open in this process is refused too.
        assert!(Journal::resume(&p, "P", "C").unwrap_err().contains("in use"));
        drop(held);
        Journal::resume(&p, "P", "C").unwrap();
    }

    #[test]
    fn an_authorized_retry_is_one_attempt() {
        let p = tmp("retry");
        let body = json!({ "capability": "email.send", "method": "send" });
        let mut j = Journal::create(&p, "P", "C").unwrap();
        j.begin("action", body.clone()).unwrap();
        drop(j); // crash: the action is in doubt
        assert!(matches!(Journal::resume(&p, "P", "C").unwrap().begin("action", body.clone()).unwrap(), Step::InDoubt { .. }));
        Journal::resolve(&p, Resolution::Retry).unwrap();
        assert!(Journal::resolve(&p, Resolution::Retry).unwrap_err().contains("already authorized"));
        // Resume performs it, recording the attempt first.
        let mut r = Journal::resume(&p, "P", "C").unwrap();
        assert!(matches!(r.begin("action", body.clone()).unwrap(), Step::Perform { n: 1, .. }));
        drop(r); // crash during the authorized attempt
        // That attempt may have happened: in doubt again, never silently retried twice.
        let mut r = Journal::resume(&p, "P", "C").unwrap();
        assert!(matches!(r.begin("action", body.clone()).unwrap(), Step::InDoubt { n: 1, .. }));
        drop(r);
        Journal::resolve(&p, Resolution::Performed { receipt: "R9".into() }).unwrap();
        let mut r = Journal::resume(&p, "P", "C").unwrap();
        assert_eq!(r.begin("action", body).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": { "receipt": "R9" } })));
    }

    #[test]
    fn retries_are_only_for_actions_in_doubt() {
        let p = tmp("retry-structure");
        drop(Journal::create(&p, "P", "C").unwrap());
        let base = std::fs::read_to_string(&p).unwrap();
        let count = "{\"j\":\"intent\",\"n\":1,\"op\":\"count\",\"key\":\"k\",\"body\":1}\n";
        let action = "{\"j\":\"intent\",\"n\":1,\"op\":\"action\",\"key\":\"k\",\"body\":1}\n";
        let retry = "{\"j\":\"retry\",\"n\":1}\n";
        let attempt = "{\"j\":\"attempt\",\"n\":1}\n";
        let result = "{\"j\":\"result\",\"n\":1,\"ok\":1}\n";
        let cases = [
            (format!("{count}{retry}"), "never resolved by hand"),
            (format!("{action}{retry}{retry}"), "two retries"),
            (format!("{action}{attempt}"), "attempt without a retry"),
            (format!("{action}{retry}{result}"), "retry with no attempt"),
            (format!("{action}{result}{retry}"), "not in doubt"),
        ];
        for (extra, want) in cases {
            std::fs::write(&p, format!("{base}{extra}")).unwrap();
            let e = Journal::resume(&p, "P", "C").unwrap_err();
            assert!(e.contains(want), "{extra:?}: {e}");
        }
        // The valid shape: intent, retry, attempt, result.
        std::fs::write(&p, format!("{base}{action}{retry}{attempt}{result}")).unwrap();
        Journal::resume(&p, "P", "C").unwrap();
    }

    #[test]
    fn resolve_answers_only_an_action_in_doubt() {
        let p = tmp("resolve");
        let mut j = Journal::create(&p, "P", "C").unwrap();
        j.begin("action", json!({ "capability": "payments.refund", "method": "refund" })).unwrap();
        drop(j);
        let msg = Journal::resolve(&p, Resolution::Performed { receipt: "re_123".into() }).unwrap();
        assert!(msg.contains("payments.refund.refund"), "{msg}");
        assert!(Journal::resolve(&p, Resolution::Failed { message: "x".into() }).unwrap_err().contains("no operation is in doubt"));
        let mut r = Journal::resume(&p, "P", "C").unwrap();
        let step = r.begin("action", json!({ "capability": "payments.refund", "method": "refund" })).unwrap();
        assert_eq!(step, Step::Replay(json!({ "j": "result", "n": 1, "ok": { "receipt": "re_123" } })));
    }
}
