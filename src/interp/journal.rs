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
//  - One holder at a time: in this process, a registry of the journals it holds, by
//    the file's identity; between processes, an OS lock (`flock`), freed
//    automatically if the process dies. See `lock`.

use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

/// The version a new journal is written in, and the only one this `staalc` resumes.
/// Version 3 binds the source program fingerprint `staal-program-v3` (`identity`) and the
/// run config's RFC 8785 form (`jcs`), records the `staalc` version, the engine and the
/// proxies in the start record, and adds a `resume` record per resume. A journal of
/// version 1 or 2 bound other hashes, which this `staalc` does not compute; such a run
/// is finished with the `staalc` that started it (RUNTIME.md section 11).
pub const VERSION: u64 = 3;

/// What a journal binds a run to, and what it records about what executes it. The
/// start record holds it; each resume must present the same program, config and
/// proxies, and records its own `staalc` version and engine (RUNTIME.md section 11).
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    /// SHA-256 of the source program fingerprint (`identity`).
    pub program_sha256: String,
    /// SHA-256 of the run config in RFC 8785 form (`jcs`).
    pub config_sha256: String,
    /// `"interpreter"` or `"native"`: may differ between the start and a resume.
    pub engine: &'static str,
    /// The proxies the run's network clients use, as `interp::proxy` resolved them:
    /// `{"net": ..., "model": ...}`, each `null` for none.
    pub proxies: Json,
}

impl Binding {
    /// The SHA-256 of a fingerprint and of a config's RFC 8785 form. Fails for a config
    /// holding a number RFC 8785 cannot represent exactly (`jcs`).
    pub fn of(fingerprint: &str, config: &Json, engine: &'static str, proxies: Json) -> Result<Binding, String> {
        let canonical = crate::jcs::canonical(config).map_err(|e| format!("run config: {e}"))?;
        Ok(Binding { program_sha256: sha256_hex(fingerprint.as_bytes()), config_sha256: sha256_hex(canonical.as_bytes()), engine, proxies })
    }
}

/// The `staalc` version that writes a record.
const STAALC: &str = env!("CARGO_PKG_VERSION");

/// An operation's name in messages: its number, after its task's id for a task's.
pub fn op_id(task: &str, n: u64) -> String {
    if task.is_empty() { n.to_string() } else { format!("{task}:{n}") }
}

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

/// What `Journal::resolve` recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// What was recorded, for people: `staalc resolve` prints it.
    pub summary: String,
    /// The file that was locked, checked and written: the path named it when it was
    /// locked, and nothing was opened by path after that.
    #[cfg(unix)]
    pub identity: FileIdentity,
    /// The decision as the journal records it, for `resolve_decision`: `None` for a
    /// resolution without one.
    pub decision: Option<Json>,
    /// The decision was in the journal already, with everything the same: nothing was
    /// written, and `decision` is what was recorded then.
    pub already_recorded: bool,
}

/// A file's identity: its device and inode, which a path does not change and two paths
/// to one file share.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

#[cfg(unix)]
impl FileIdentity {
    /// The identity of the file `path` names now, following symbolic links as opening it
    /// does.
    pub fn of_path(path: &Path) -> std::io::Result<FileIdentity> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path)?;
        Ok(FileIdentity { device: m.dev(), inode: m.ino() })
    }
}

/// How an operator resolves an action left in doubt (`staalc resolve`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// It happened; this is the service's receipt.
    Performed { receipt: String },
    /// An HTTP request happened and was answered with this status and body (only for
    /// `net.http` actions, whose result is a response rather than a receipt).
    Responded { status: u16, body: String },
    /// A TCP exchange happened and the peer answered this (only for `net.tcp`, whose
    /// result is the reply).
    Replied { text: String },
    /// Treat it as failed: the program sees an action failure.
    Failed { message: String },
    /// It did not happen: perform it on resume. The authorization covers one attempt;
    /// if that attempt is itself interrupted, the action is in doubt again.
    Retry,
}

impl Resolution {
    /// The resolution as a decision records it: `{"performed": {"receipt": ...}}`,
    /// `{"responded": {"status": ..., "body": ...}}`, `{"replied": {"text": ...}}`,
    /// `{"failed": {"message": ...}}` or `{"retry": {}}`, exactly as given.
    pub fn to_json(&self) -> Json {
        match self {
            Resolution::Performed { receipt } => json!({ "performed": { "receipt": receipt } }),
            Resolution::Responded { status, body } => json!({ "responded": { "status": status, "body": body } }),
            Resolution::Replied { text } => json!({ "replied": { "text": text } }),
            Resolution::Failed { message } => json!({ "failed": { "message": message } }),
            Resolution::Retry => json!({ "retry": {} }),
        }
    }
}

/// An operator's decision about an operation in doubt, recorded with the resolution it
/// makes, so a journal says who decided what, on what evidence (Staal Studio's design,
/// section 7). Applying the same decision again finds it recorded and changes nothing;
/// the same id with anything else different is refused (`Journal::resolve_decision`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// 128 random bits, as 32 lowercase hexadecimal digits: unique to this decision.
    pub id: String,
    /// The run it is for: the journal's run id must be this.
    pub run_id: String,
    /// What the operator checked, and where.
    pub evidence: String,
}

impl Decision {
    fn check(&self) -> Result<(), String> {
        if !is_hex(&self.id, 32) {
            return Err(format!("decision id '{}' is not 32 lowercase hexadecimal characters", self.id));
        }
        if !is_hex(&self.run_id, 32) {
            return Err(format!("run id '{}' is not 32 lowercase hexadecimal characters", self.run_id));
        }
        if self.evidence.trim().is_empty() {
            return Err("a decision needs its evidence: what was checked, and where".into());
        }
        Ok(())
    }

    /// The decision as the journal records it, applying `resolution` to `operation`.
    fn record(&self, operation: &str, resolution: &Resolution) -> Json {
        json!({
            "id": self.id, "run_id": self.run_id, "operation": operation,
            "resolution": resolution.to_json(), "evidence": self.evidence, "staalc": STAALC,
        })
    }
}

/// Whether `s` is `len` lowercase hexadecimal digits.
fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug)]
pub struct Journal {
    /// None: an in-memory journal, for runs without durability.
    file: Option<File>,
    /// This process's hold on the file, kept only to be released, after `file` is
    /// closed (fields are dropped in order).
    _held: Option<Held>,
    poisoned: bool,
    /// Each stream of operations: the program's own (task ""), and each concurrent
    /// task's, numbered from 1 by itself.
    streams: BTreeMap<String, Stream>,
    /// The `end` record of a run that had already finished.
    recorded_end: Option<Json>,
    run_id: String,
    failpoint: Failpoint,
    writes: u64,
    /// The bytes of the file written and synced: every record within them is durable.
    durable: u64,
}

#[derive(Debug)]
struct Stream {
    next_n: u64,
    /// Recorded intents and results not yet replayed.
    pending: VecDeque<Json>,
}

impl Default for Stream {
    fn default() -> Stream {
        Stream { next_n: 1, pending: VecDeque::new() }
    }
}

impl Journal {
    /// A journal that keeps nothing: the run is not durable.
    pub fn memory() -> Result<Journal, String> {
        Ok(Journal::with(None, None, new_run_id()?, BTreeMap::new(), None))
    }

    fn with(file: Option<File>, held: Option<Held>, run_id: String, pending: BTreeMap<String, VecDeque<Json>>, recorded_end: Option<Json>) -> Journal {
        let streams = pending.into_iter().map(|(t, p)| (t, Stream { next_n: 1, pending: p })).collect();
        Journal { file, _held: held, poisoned: false, streams, recorded_end, run_id, failpoint: Failpoint::from_env(), writes: 0, durable: 0 }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Whether this journal had already recorded operations when opened.
    pub fn is_resuming(&self) -> bool {
        self.streams.values().any(|s| !s.pending.is_empty()) || self.recorded_end.is_some()
    }

    /// Creates a new journal for a new run. Fails if the file already exists.
    pub fn create(path: &Path, binding: &Binding) -> Result<Journal, String> {
        Journal::create_with_id(path, &new_run_id()?, binding)
    }

    /// As `create`, for a run whose id the caller drew with `new_run_id`.
    pub fn create_with_id(path: &Path, run_id: &str, binding: &Binding) -> Result<Journal, String> {
        if run_id.len() != 32 || !run_id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(format!("run id '{run_id}' is not 32 lowercase hexadecimal characters"));
        }
        let shown = path.display();
        let mut opts = OpenOptions::new();
        opts.append(true).create_new(true);
        // A journal holds what the run sent and received: readable by its owner only.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let file = opts.open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("journal {shown} already exists; use `staalc resume` to continue that run")
            } else {
                format!("cannot create journal {shown}: {e}")
            }
        })?;
        let held = lock(&file, path)?;
        let run_id = run_id.to_string();
        let mut j = Journal::with(Some(file), Some(held), run_id.clone(), BTreeMap::new(), None);
        j.write(&json!({
            "j": "start", "version": VERSION, "run_id": run_id,
            "program_sha256": binding.program_sha256, "config_sha256": binding.config_sha256,
            "staalc": STAALC, "engine": binding.engine, "proxies": binding.proxies,
        }))?;
        // Make the new file's directory entry durable too.
        sync_parent_dir(path)?;
        Ok(j)
    }

    /// Opens an existing journal to resume its run. The program, config and proxies must
    /// be exactly those it was started with. Unless the run has already ended, a
    /// `resume` record is appended, and flushed, before anything else happens: which
    /// `staalc` and which engine carry the run on. A run that has ended is only replayed,
    /// which changes nothing, and gets no record.
    pub fn resume(path: &Path, binding: &Binding) -> Result<Journal, String> {
        let (file, held, records, durable) = open_existing(path)?;
        let start = records.first().ok_or_else(|| {
            format!(
                "journal {} is empty: the run never started, so nothing was done; start a new run",
                path.display()
            )
        })?;
        check_start(start, binding, path)?;
        let run_id = start["run_id"].as_str().unwrap_or_default().to_string(); // checked by check_start
        let (pending, recorded_end) = check_body(&records[1..], &run_id, path)?;
        let ended = recorded_end.is_some();
        let mut j = Journal::with(Some(file), Some(held), run_id, pending, recorded_end);
        j.durable = durable;
        if !ended {
            let at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "the system clock is before 1970".to_string())?
                .as_millis() as u64;
            j.write(&json!({
                "j": "resume", "run_id": j.run_id, "version": VERSION, "staalc": STAALC, "engine": binding.engine,
                "program_sha256": binding.program_sha256, "config_sha256": binding.config_sha256,
                "proxies": binding.proxies, "at_ms": at_ms,
            }))?;
        }
        Ok(j)
    }

    /// Records an operator's answer for an action in doubt. When tasks leave several in
    /// doubt, `operation` names the one (`op_id`). Says what was recorded, and which file
    /// was locked and written (`Resolved`).
    pub fn resolve(path: &Path, resolution: Resolution, operation: Option<&str>) -> Result<Resolved, String> {
        Journal::resolve_with(path, resolution, operation, None)
    }

    /// As `resolve`, recording `decision` with the resolution: its id, the run, the
    /// operation, the resolution, the evidence and this `staalc`'s version. Matched on
    /// all of it: if the journal already records a decision with this id for this run
    /// and operation, with the same resolution and evidence, that record is returned and
    /// nothing is written (`already_recorded`), whether or not the run has gone on since;
    /// the same id with anything different, or recorded for another operation, is
    /// refused as a conflict. A journal of another run is refused.
    pub fn resolve_decision(path: &Path, decision: &Decision, resolution: Resolution, operation: &str) -> Result<Resolved, String> {
        decision.check()?;
        Journal::resolve_with(path, resolution, Some(operation), Some(decision))
    }

    fn resolve_with(path: &Path, resolution: Resolution, operation: Option<&str>, decision: Option<&Decision>) -> Result<Resolved, String> {
        let (file, held, records, durable) = open_existing(path)?;
        #[cfg(unix)]
        let identity = held.identity();
        let start = records.first().ok_or_else(|| format!("journal {} is empty", path.display()))?;
        if start["j"] != "start" {
            return Err(format!("journal {}: first record is not a start record", path.display()));
        }
        check_version(start, path)?;
        let run_id = start["run_id"].as_str().unwrap_or_default();
        let (pending, recorded_end) = check_body(&records[1..], run_id, path)?;
        if let Some(d) = decision {
            if d.run_id != run_id {
                return Err(format!(
                    "decision {} is for run {}, but journal {} is of run {run_id}; nothing was recorded",
                    d.id,
                    d.run_id,
                    path.display()
                ));
            }
            // check_body has validated every recorded decision, and that no id is
            // recorded twice.
            if let Some(recorded) = records.iter().filter_map(|r| r.get("decision")).find(|r| r["id"] == d.id.as_str()) {
                let given = d.record(operation.unwrap_or_default(), &resolution);
                let differs: Vec<&str> =
                    ["run_id", "operation", "resolution", "evidence"].into_iter().filter(|k| recorded[*k] != given[*k]).collect();
                if !differs.is_empty() {
                    return Err(format!(
                        "decision {} is already recorded in this journal with a different {}: recorded for operation {} as {}; \
                         nothing was recorded",
                        d.id,
                        differs.join(", "),
                        recorded["operation"].as_str().unwrap_or("?"),
                        recorded["resolution"]
                    ));
                }
                return Ok(Resolved {
                    summary: format!(
                        "decision {} was already recorded for operation {}; nothing was recorded again",
                        d.id,
                        recorded["operation"].as_str().unwrap_or("?")
                    ),
                    #[cfg(unix)]
                    identity,
                    decision: Some(recorded.clone()),
                    already_recorded: true,
                });
            }
        }
        if recorded_end.is_some() {
            return Err(format!("this run has already finished; nothing is in doubt{}", decided(&records, operation)));
        }
        let (mut open, authorized) = open_operations(&pending);
        if let Some(id) = authorized.iter().find(|id| operation.is_none_or(|o| o == id.as_str())) {
            return Err(format!("a retry is already authorized for operation {id}; resume the run{}", decided(&records, Some(id))));
        }
        let ids: Vec<String> = open.iter().map(|(t, r)| op_id(t, r["n"].as_u64().unwrap_or_default())).collect();
        let (task, intent) = match (operation, open.len()) {
            (None, 0) => return Err("no operation is in doubt in this journal".into()),
            (Some(o), 0) => return Err(format!("operation {o} is not in doubt; no operation is{}", decided(&records, operation))),
            (None, 1) => open.swap_remove(0),
            (None, _) => {
                return Err(format!("operations {} are in doubt; name one with --operation", ids.join(", ")));
            }
            (Some(o), _) => match ids.iter().position(|i| i == o) {
                Some(k) => open.swap_remove(k),
                None => return Err(format!("operation {o} is not in doubt; in doubt: {}{}", ids.join(", "), decided(&records, operation))),
            },
        };
        operator_settles(&task, intent)?;
        let n = intent["n"].as_u64().unwrap_or_default(); // checked by check_body
        let what = format!(
            "{}.{}",
            intent["body"]["capability"].as_str().unwrap_or("?"),
            intent["body"]["method"].as_str().unwrap_or("?")
        );
        // A C call's result is a value nobody outside the process saw: an operator can
        // only say it failed, or have it made again.
        if intent["body"]["capability"].as_str().is_some_and(|c| c.starts_with("ffi."))
            && !matches!(resolution, Resolution::Failed { .. } | Resolution::Retry)
        {
            return Err(format!("{what} is a call of a C function: record --failed <reason>, or --retry to call it again"));
        }
        let http = intent["body"]["capability"].as_str().is_some_and(|c| c.starts_with("net.http."));
        let tcp = matches!(intent["body"]["capability"].as_str(), Some("net.tcp.exchange" | "net.tls.exchange"));
        let mut j = Journal::with(Some(file), Some(held), String::new(), BTreeMap::new(), None);
        j.durable = durable;
        let shown = op_id(&task, n);
        let recorded = decision.map(|d| d.record(&shown, &resolution));
        let (result, summary) = match resolution {
            Resolution::Performed { .. } if http => {
                return Err(format!(
                    "{what} is an HTTP request: record what the server answered with --status <code> [--body <text>]"
                ))
            }
            Resolution::Performed { .. } | Resolution::Responded { .. } if tcp => {
                return Err(format!("{what} is a TCP exchange: record what the peer answered with --reply <text>"))
            }
            Resolution::Responded { .. } if !http => {
                return Err(format!("{what} is not an HTTP request: record it with --performed <receipt>"))
            }
            Resolution::Replied { .. } if !tcp => {
                return Err(format!("{what} is not a TCP exchange: record it with {}", if http { "--status <code> [--body <text>]" } else { "--performed <receipt>" }))
            }
            Resolution::Replied { text } => (
                Ok(json!({ "text": text })),
                format!("recorded {what} (operation {shown}) as answered"),
            ),
            Resolution::Responded { status, body } => {
                if !(100..=599).contains(&status) {
                    return Err(format!("--status {status} is not an HTTP status code"));
                }
                (
                    Ok(json!({ "status": status, "body": body, "complete": true })),
                    format!("recorded {what} (operation {shown}) as answered with status {status}"),
                )
            }
            Resolution::Retry => {
                let mut rec = json!({ "j": "retry", "n": n });
                if let Some(d) = &recorded {
                    rec["decision"] = d.clone();
                }
                j.write(&tagged(rec, &task))?;
                return Ok(Resolved {
                    summary: format!("authorized one more attempt at {what} (operation {shown})"),
                    #[cfg(unix)]
                    identity,
                    decision: recorded,
                    already_recorded: false,
                });
            }
            Resolution::Performed { receipt } => {
                if receipt.is_empty() {
                    return Err("--performed needs the receipt id the service gave for this action".into());
                }
                (Ok(json!({ "receipt": receipt })), format!("recorded {what} (operation {shown}) as performed"))
            }
            Resolution::Failed { message } => (
                Err(format!("resolved by operator: {message}")),
                format!("recorded {what} (operation {shown}) as failed"),
            ),
        };
        let mut rec = match result {
            Ok(v) => json!({ "j": "result", "n": n, "ok": v }),
            Err(e) => json!({ "j": "result", "n": n, "err": e }),
        };
        if let Some(d) = &recorded {
            rec["decision"] = d.clone();
        }
        j.failpoint.before_result(&shown);
        j.write(&tagged(rec, &task))?;
        Ok(Resolved {
            summary,
            #[cfg(unix)]
            identity,
            decision: recorded,
            already_recorded: false,
        })
    }

    /// The identity of the journal's file, as it was when locked: `None` for a journal
    /// kept in memory.
    #[cfg(unix)]
    pub fn identity(&self) -> Option<FileIdentity> {
        self._held.as_ref().map(Held::identity)
    }

    /// Begins external operation `op` of `task` ("" for the program's own) with the
    /// given intent `body`. An action's intent records `recovery`, the rule it is made
    /// under (`interp::Recovery::name`): what a resumed run does with it if a crash
    /// leaves it in doubt, so a reader of the journal knows it without the run config.
    pub fn begin(&mut self, task: &str, op: &str, body: Json, recovery: Option<&str>) -> Result<Step, String> {
        let stream = self.streams.entry(task.to_string()).or_default();
        let n = stream.next_n;
        stream.next_n += 1;
        let key = if task.is_empty() { format!("{}-{n}", self.run_id) } else { format!("{}-{task}-{n}", self.run_id) };
        let mut intent = json!({ "j": "intent", "n": n, "op": op, "key": key, "body": body });
        if let Some(r) = recovery {
            intent["recovery"] = json!(r);
        }
        let intent = tagged(intent, task);
        if let Some(recorded) = stream.pending.pop_front() {
            // An intent recorded by a staalc that did not record `recovery` (0.28.0 and
            // before) is compared without it.
            let same = if recorded.get("recovery").is_none() {
                let mut without = intent.clone();
                if let Some(o) = without.as_object_mut() {
                    o.remove("recovery");
                }
                recorded == without
            } else {
                recorded == intent
            };
            if !same {
                return Err(divergence(&op_id(task, n), &recorded, &intent));
            }
            // check_body guarantees what may follow an intent: its result, or operator
            // `retry` authorizations each followed by an `attempt`, possibly cut short
            // by the end of the journal.
            let mut authorized = false;
            loop {
                match stream.pending.pop_front() {
                    Some(r) if r["j"] == "result" => return Ok(Step::Replay(r)),
                    Some(r) if r["j"] == "retry" => authorized = true,
                    Some(_) => authorized = false, // an attempt: it used up the authorization
                    None if authorized => {
                        // Record the attempt before making it, so a crash during it leaves
                        // the action in doubt again rather than silently retried.
                        self.write(&tagged(json!({ "j": "attempt", "n": n }), task))?;
                        return Ok(Step::Perform { n, key });
                    }
                    None => return Ok(Step::InDoubt { n, key }),
                }
            }
        }
        if self.recorded_end.is_some() {
            return Err(format!(
                "operation {}: the journal records that this run had finished, but replay reached a further \
                 {op}; the run is not reproducible",
                op_id(task, n)
            ));
        }
        self.write(&intent)?;
        Ok(Step::Perform { n, key })
    }

    /// Whether the next operation of `task` is already in the journal (it is replayed,
    /// or retried after being in doubt), rather than about to be made for the first time.
    pub fn replays_next(&self, task: &str) -> bool {
        self.streams.get(task).is_some_and(|s| !s.pending.is_empty())
    }

    /// The failure kind the journal records the run as having ended with, if it ended
    /// with one.
    pub fn recorded_failure(&self) -> Option<&str> {
        self.recorded_end.as_ref().filter(|e| e["ok"] == false).and_then(|e| e["failure"].as_str())
    }

    /// Records the result of operation `n` of `task`.
    pub fn finish(&mut self, task: &str, n: u64, result: Result<Json, String>) -> Result<(), String> {
        self.failpoint.before_result(&op_id(task, n));
        let rec = match result {
            Ok(v) => json!({ "j": "result", "n": n, "ok": v }),
            Err(e) => json!({ "j": "result", "n": n, "err": e }),
        };
        self.write(&tagged(rec, task))
    }

    /// Records how the run ended. On resume of a finished run, checks it matches.
    pub fn end(&mut self, rec: Json) -> Result<(), String> {
        let left: usize = self.streams.values().map(|s| s.pending.len()).sum();
        if left > 0 {
            return Err(format!(
                "the run finished, but the journal holds {left} more recorded record(s); the run is not reproducible"
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
        let res = file.write_all(line.as_bytes()).and_then(|()| {
            #[cfg(test)]
            if let Some(h) = &self._held {
                tests::before_sync(&h.0);
            }
            file.sync_data()
        });
        if let Err(e) = res {
            self.poisoned = true;
            return Err(format!("could not write the journal: {e}"));
        }
        self.durable += line.len() as u64;
        if let Some(h) = &self._held {
            h.set_durable(self.durable);
        }
        self.writes += 1;
        self.failpoint.after_write(self.writes);
        Ok(())
    }
}

/// The identity of an open file: its device and inode, so two paths to one file, or a
/// path re-pointed at another, are never confused.
#[cfg(unix)]
type FileId = FileIdentity;
#[cfg(not(unix))]
type FileId = std::path::PathBuf;

#[cfg(unix)]
fn file_id(file: &File, _path: &Path) -> Result<FileId, String> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|e| e.to_string())?;
    Ok(FileIdentity { device: m.dev(), inode: m.ino() })
}

#[cfg(not(unix))]
fn file_id(_file: &File, path: &Path) -> Result<FileId, String> {
    std::fs::canonicalize(path).map_err(|e| e.to_string())
}

/// The journals this process holds, each with its durable length: the bytes its writer
/// has written and synced (`snapshot`).
static HELD: std::sync::Mutex<Vec<(FileId, u64)>> = std::sync::Mutex::new(Vec::new());

/// The durable length of the journal `id`, if this process holds it.
fn durable_length(id: &FileId) -> Option<u64> {
    HELD.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(h, _)| h == id).map(|(_, d)| *d)
}

/// This process's hold on one journal; dropping it lets the journal go.
#[derive(Debug)]
struct Held(FileId);

impl Held {
    #[cfg(unix)]
    fn identity(&self) -> FileIdentity {
        self.0
    }

    /// Records that the first `bytes` of the journal are durable.
    fn set_durable(&self, bytes: u64) {
        if let Some(e) = HELD.lock().unwrap_or_else(|p| p.into_inner()).iter_mut().find(|(id, _)| *id == self.0) {
            e.1 = bytes;
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        HELD.lock().unwrap_or_else(|p| p.into_inner()).retain(|(id, _)| *id != self.0);
    }
}

/// How long `lock` waits for an OS lock that a child process briefly keeps (below).
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Takes the journal for this run alone, or refuses.
///
/// In this process, a registry of the journals it holds decides, at once: a journal
/// another run here holds is refused. Between processes, the OS lock (`flock`) decides.
/// That lock belongs to the open file, and a process another thread starts (a C worker,
/// `git`, any child of a program that embeds the runtime) briefly has a copy of every
/// open file, from its start until it runs its program and its copies close. A journal
/// whose run has just let it go can therefore still be locked for that moment, though
/// nothing holds it. So a lock this process does not hold is waited for, up to
/// `LOCK_WAIT`, and only then refused: a child's copy goes within that time; a journal
/// another process really holds is refused, as before, only later.
fn lock(file: &File, path: &Path) -> Result<Held, String> {
    let shown = path.display();
    let id = file_id(file, path).map_err(|e| format!("cannot lock journal {shown}: {e}"))?;
    #[cfg(test)]
    tests::before_lock(path);
    {
        let mut held = HELD.lock().unwrap_or_else(|p| p.into_inner());
        if held.iter().any(|(h, _)| *h == id) {
            return Err(format!("journal {shown} is in use by another run in this process"));
        }
        // Nothing is known durable until the holder has synced (open_existing, write).
        #[cfg_attr(unix, allow(clippy::clone_on_copy, reason = "a FileId is a path, not Copy, off Unix"))]
        held.push((id.clone(), 0));
    }
    let hold = Held(id);
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => {
                still_named(path, &hold.0)?;
                return Ok(hold);
            }
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(format!("journal {shown} is in use by another staalc process"));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(format!("cannot lock journal {shown}: {e}")),
        }
    }
}

/// After locking: the path must still name the file that was opened, or what was locked
/// is not the journal the caller named (a file swapped in between opening and locking).
/// Everything after this goes through the open file; the path is not opened again, so a
/// later rename or replacement is not seen (RUNTIME.md section 11).
#[cfg(unix)]
fn still_named(path: &Path, id: &FileId) -> Result<(), String> {
    let shown = path.display();
    match FileIdentity::of_path(path) {
        Ok(now) if now == *id => Ok(()),
        Ok(_) => Err(format!(
            "journal {shown} was replaced while it was being opened: the path no longer names the file that was \
             opened and locked, so nothing was done"
        )),
        Err(e) => Err(format!("journal {shown} was removed or moved while it was being opened ({e}), so nothing was done")),
    }
}

#[cfg(not(unix))]
fn still_named(_path: &Path, _id: &FileId) -> Result<(), String> {
    Ok(())
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
/// was never acknowledged. Any other malformed line is refused as corruption. The file
/// is then synced, so every record read is durable even if its writer stopped between
/// writing it and syncing it; returns the records' length, all of it durable.
fn open_existing(path: &Path) -> Result<(File, Held, Vec<Json>, u64), String> {
    let shown = path.display();
    let mut file = OpenOptions::new().read(true).append(true).open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("journal {shown} does not exist")
        } else {
            format!("cannot open journal {shown}: {e}")
        }
    })?;
    let held = lock(&file, path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| format!("cannot read journal {shown}: {e}"))?;
    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    if complete < bytes.len() {
        file.set_len(complete as u64).map_err(|e| format!("cannot repair torn last line of journal {shown}: {e}"))?;
    }
    file.sync_data().map_err(|e| format!("cannot sync journal {shown}: {e}"))?;
    held.set_durable(complete as u64);
    let (records, _) = parse_complete(&bytes[..complete], &shown.to_string())?;
    Ok((file, held, records, complete as u64))
}

/// The records in `bytes`, which end with a complete line or are empty, and where each
/// ends (its byte offset after its line end).
fn parse_complete(bytes: &[u8], shown: &str) -> Result<(Vec<Json>, Vec<u64>), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| format!("journal {shown} is corrupt: not UTF-8"))?;
    let (mut records, mut ends) = (Vec::new(), Vec::new());
    let mut at = 0u64;
    for (i, line) in text.split_terminator('\n').enumerate() {
        let rec: Json = serde_json::from_str(line).map_err(|e| format!("journal {shown} is corrupt at line {}: {e}", i + 1))?;
        if !rec.is_object() {
            return Err(format!("journal {shown} is corrupt at line {}: not a record", i + 1));
        }
        at += line.len() as u64 + 1;
        records.push(rec);
        ends.push(at);
    }
    Ok((records, ends))
}

/// Whether what a snapshot read is known to be durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durable {
    /// This process holds the journal, so its writer says: the first this many records
    /// read are durable (written and synced); any after them were written and not yet
    /// synced, and a crash could still lose them.
    Records(usize),
    /// This process does not hold the journal: another process may be writing it, or
    /// none. What was read had been written; whether it is durable is not established.
    NotEstablished,
}

/// A journal read at one moment through one open file, changing nothing and taking no
/// lock: its complete records, what the read covered, the identity of the file read,
/// and how much of it is known durable (`Durable`).
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub records: Vec<Json>,
    pub read: Extent,
    /// The file read: the one the path named when it was opened.
    #[cfg(unix)]
    pub identity: FileIdentity,
    pub durable: Durable,
}

/// Reads the journal at `path` (`Snapshot`). A torn or unfinished last line is left out
/// and counted in `read.unfinished_bytes`; any other damage is an error.
pub fn snapshot(path: &Path) -> Result<Snapshot, String> {
    let shown = path.display().to_string();
    let mut file = File::open(path).map_err(|e| format!("cannot read journal {shown}: {e}"))?;
    let id = file_id(&file, path).map_err(|e| format!("cannot read journal {shown}: {e}"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| format!("cannot read journal {shown}: {e}"))?;
    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let (records, ends) = parse_complete(&bytes[..complete], &shown)?;
    // Asked after reading: a length the writer reached since covers what was read.
    let durable = match durable_length(&id) {
        Some(d) => Durable::Records(ends.iter().take_while(|e| **e <= d).count()),
        None => Durable::NotEstablished,
    };
    Ok(Snapshot {
        read: Extent { records: records.len(), bytes: complete as u64, unfinished_bytes: (bytes.len() - complete) as u64 },
        records,
        #[cfg(unix)]
        identity: id,
        durable,
    })
}

/// A journal's complete records, read without opening it for writing or locking it:
/// for a reader that must change nothing (a status query). A torn last line is left
/// out, as resuming would cut it off; any other damage is an error. A file with no
/// complete record gives an empty list.
pub fn read_records(path: &Path) -> Result<Vec<Json>, String> {
    snapshot(path).map(|s| s.records)
}

/// Each operation left open in `pending` (check_body's streams), in task order, as
/// its task and its intent: a stream whose last record is an intent or an attempt.
/// And each operation a retry is authorized for (its last record a `retry`), by id.
fn open_operations(pending: &Streams) -> (Vec<(String, &Json)>, Vec<String>) {
    let (mut open, mut authorized) = (Vec::new(), Vec::new());
    for (t, recs) in pending {
        match recs.back().and_then(|r| r["j"].as_str()) {
            // check_body guarantees an open operation's intent is in its stream.
            Some("intent" | "attempt") => {
                if let Some(intent) = recs.iter().rev().find(|r| r["j"] == "intent") {
                    open.push((t.clone(), intent));
                }
            }
            Some("retry") => authorized.push(op_id(t, recs.back().and_then(|r| r["n"].as_u64()).unwrap_or_default())),
            _ => {}
        }
    }
    (open, authorized)
}

/// Whether `resolve` accepts a resolution for the open operation `intent` of `task`, or
/// refuses it, and then why: only an action can be resolved, and not one always made
/// again, whose result could not take the form the program reads. (Who settles an
/// operation left alone is `settle_of`: an action that is resolved is settled by what
/// the operator recorded, whatever its rule.)
fn operator_settles(task: &str, intent: &Json) -> Result<(), String> {
    let n = intent["n"].as_u64().unwrap_or_default(); // checked by check_body
    if intent["op"] != "action" {
        return Err(format!("operation {} in doubt is a {}, which resume retries by itself; nothing to resolve", op_id(task, n), intent["op"]));
    }
    // Actions always safe to make again: resume makes them again, and a result
    // recorded here could not take the form the program reads.
    if let Some(c @ ("fs.read" | "fs.write" | "db.sqlite.query" | "db.postgres.query" | "net.tcp.accept" | "net.tcp.reply" | "net.tls.accept"
        | "net.tls.reply")) = intent["body"]["capability"].as_str()
    {
        return Err(format!("operation {} in doubt is {c}, which resume makes again by itself; nothing to resolve", op_id(task, n)));
    }
    Ok(())
}


/// A journal read without changing it: what `resume` and `resolve` would make of it,
/// from the same validation (`check_body`) and the same rules for what is open and who
/// settles it. Built on a `Snapshot`: the file is read without a lock, and `durable`
/// says how much of what was read is known durable.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub run_id: String,
    /// The start record, as written.
    pub start: Json,
    /// Who carried the run on: the start, then each resume, in order.
    pub sessions: Vec<Session>,
    /// Every operation, each task's in its order; the program's own (task "") first,
    /// then each task's by name.
    pub operations: Vec<Operation>,
    /// The end record, once the run has ended.
    pub end: Option<Json>,
    /// What was read: complete records, their bytes, and the bytes after them.
    pub read: Extent,
    /// The file read.
    #[cfg(unix)]
    pub identity: FileIdentity,
    /// How many of the records read are known durable.
    pub durable: Durable,
}

/// One session of a run: its start, or a resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub staalc: String,
    pub engine: String,
    /// When a resume began (milliseconds since 1970); None for the start, which records
    /// no time.
    pub at_ms: Option<u64>,
}

/// What a read covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    /// Complete records read, the start record included.
    pub records: usize,
    /// The bytes those records take: the length of the journal this view describes.
    pub bytes: u64,
    /// Bytes after the last complete record: a record being written, or one torn by a
    /// crash, which `resume` would cut off. Not part of this view.
    pub unfinished_bytes: u64,
}

/// One operation of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    /// Its id in messages and in `resolve --operation`: `n`, or `task:n`.
    pub id: String,
    /// "" for the program's own.
    pub task: String,
    pub n: u64,
    /// "action", "model", "count", "print", "clock", "random" or "input".
    pub op: String,
    /// What its intent recorded.
    pub body: Json,
    /// The session (index into `View::sessions`) that recorded its intent.
    pub session: usize,
    /// Whether a later session replayed it: every session after the one that recorded
    /// its result served that result instead of making the operation again.
    pub replayed: bool,
    pub state: OpState,
    /// The decisions recorded for it (`Journal::resolve_decision`), in order: a retry's,
    /// then perhaps a result's.
    pub decisions: Vec<Json>,
}

/// Where an operation stands.
#[derive(Debug, Clone, PartialEq)]
pub enum OpState {
    /// It has a result.
    Done(Json),
    /// It has a failure as its result.
    Failed(String),
    /// Its intent has no result: it may or may not have happened. `attempts` counts the
    /// attempts operators authorized; `settle` says who settles it.
    InDoubt { attempts: u64, settle: Settle },
    /// An operator authorized one more attempt, which the next resume makes.
    RetryAuthorized { attempts: u64 },
}

/// Who settles an operation in doubt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settle {
    /// An operator, with `staalc resolve`: resume stops at it until one has.
    Operator,
    /// `resume`, by making it again (an action safe to repeat, or one sent again with the
    /// same key, or any operation not an action); why.
    Resume(String),
    /// Not recorded: an action whose intent was written by a staalc that did not record
    /// the rule it was made under (0.28.0 and before). Resume decides it from the run
    /// config, which the journal holds only as a hash.
    NotRecorded,
}

/// Who settles the open operation `intent`: the rule an action recorded when it was
/// made (`interp::runtime_recovery`, `interp::adapter_recovery`), which resume, bound to
/// the same program and config, applies again; any other operation, resume.
fn settle_of(task: &str, intent: &Json) -> Settle {
    let n = intent["n"].as_u64().unwrap_or_default();
    if intent["op"] != "action" {
        return Settle::Resume(format!("operation {} in doubt is a {}, which resume makes again by itself", op_id(task, n), intent["op"]));
    }
    match intent.get("recovery").and_then(Json::as_str) {
        Some("operator") => Settle::Operator,
        Some("repeat") => Settle::Resume(format!("operation {} in doubt is safe to repeat; resume makes it again by itself", op_id(task, n))),
        Some("resend_with_key") => Settle::Resume(format!(
            "operation {} in doubt is sent again with the same idempotency key, which tells the service, or the database, whether it already happened",
            op_id(task, n)
        )),
        _ => Settle::NotRecorded,
    }
}

/// Reads the journal at `path` as `resume` and `resolve` interpret it, changing
/// nothing. A journal with no complete record, or one `resume` would refuse as corrupt
/// or of another version, is an error with the message they would give.
pub fn view(path: &Path) -> Result<View, String> {
    view_of(snapshot(path)?, path)
}

/// The view of a snapshot already taken of the journal at `path`.
pub fn view_of(snap: Snapshot, path: &Path) -> Result<View, String> {
    let Snapshot { records, read, durable, .. } = snap;
    #[cfg(unix)]
    let identity = snap.identity;
    let start = records.first().ok_or_else(|| format!("journal {} is empty: the run never started", path.display()))?;
    if start["j"] != "start" {
        return Err(format!("journal {}: first record is not a start record", path.display()));
    }
    check_version(start, path)?;
    let run_id = start["run_id"].as_str().filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())).ok_or_else(|| format!("journal {}: start record has no valid run id", path.display()))?;
    let (pending, end) = check_body(&records[1..], run_id, path)?;
    let text = |r: &Json, k: &str| r[k].as_str().unwrap_or_default().to_string();
    // Which session wrote each record (validated above): 0 for the start's, k for the
    // k-th resume's.
    let mut sessions = vec![Session { staalc: text(start, "staalc"), engine: text(start, "engine"), at_ms: None }];
    let mut written: BTreeMap<(String, u64, &str), usize> = BTreeMap::new();
    for rec in &records[1..] {
        match rec["j"].as_str() {
            Some("resume") => sessions.push(Session { staalc: text(rec, "staalc"), engine: text(rec, "engine"), at_ms: rec["at_ms"].as_u64() }),
            Some(j @ ("intent" | "result")) => {
                written.insert((text(rec, "t"), rec["n"].as_u64().unwrap_or_default(), j), sessions.len() - 1);
            }
            _ => {}
        }
    }
    let last_session = sessions.len() - 1;
    let (open, authorized) = open_operations(&pending);
    let mut operations = Vec::new();
    for (task, recs) in &pending {
        for rec in recs {
            let n = rec["n"].as_u64().unwrap_or_default();
            match rec["j"].as_str() {
                Some("intent") => {
                    let id = op_id(task, n);
                    let state = if authorized.contains(&id) {
                        OpState::RetryAuthorized { attempts: 0 }
                    } else if let Some((_, intent)) = open.iter().find(|(t, i)| t == task && i["n"] == n) {
                        OpState::InDoubt { attempts: 0, settle: settle_of(task, intent) }
                    } else {
                        OpState::Failed(String::new()) // replaced by its result below
                    };
                    operations.push(Operation {
                        id,
                        task: task.clone(),
                        n,
                        op: text(rec, "op"),
                        body: rec["body"].clone(),
                        session: written.get(&(task.clone(), n, "intent")).copied().unwrap_or_default(),
                        replayed: false,
                        state,
                        decisions: Vec::new(),
                    });
                }
                Some("attempt") => {
                    if let Some(Operation { state: OpState::InDoubt { attempts, .. } | OpState::RetryAuthorized { attempts }, .. }) = operations.last_mut() {
                        *attempts += 1;
                    }
                }
                Some("result") => {
                    if let Some(o) = operations.last_mut() {
                        o.decisions.extend(rec.get("decision").cloned());
                        o.state = match (rec.get("ok"), rec.get("err")) {
                            (Some(v), _) => OpState::Done(v.clone()),
                            (_, e) => OpState::Failed(e.and_then(Json::as_str).unwrap_or_default().to_string()),
                        };
                        o.replayed = written.get(&(task.clone(), n, "result")).is_some_and(|s| *s < last_session);
                    }
                }
                // A retry: its operation's state is decided above.
                _ => {
                    if let Some(o) = operations.last_mut() {
                        o.decisions.extend(rec.get("decision").cloned());
                    }
                }
            }
        }
    }
    Ok(View {
        run_id: run_id.to_string(),
        start: start.clone(),
        sessions,
        operations,
        end,
        read,
        #[cfg(unix)]
        identity,
        durable,
    })
}

/// Whether a run could be resumed from the journal whose first record is `start`:
/// this journal version, and the identical program, config and proxies. A host checks
/// this for every unfinished run before resuming any (RUNTIME.md section 11).
pub fn check_resumable(start: &Json, binding: &Binding, path: &Path) -> Result<(), String> {
    check_start(start, binding, path)
}

/// A journal is read only in this version: an older one bound other hashes.
fn check_version(start: &Json, path: &Path) -> Result<(), String> {
    if start["version"] != VERSION {
        let wrote = start.get("staalc").and_then(Json::as_str).map(|v| format!("staalc {v}")).unwrap_or("an older staalc".into());
        return Err(format!(
            "journal {} has version {}, written by {wrote}; this staalc {STAALC} reads version {VERSION} only. \
             Finish or resolve the run with the staalc that started it (RUNTIME.md section 11)",
            path.display(),
            start["version"]
        ));
    }
    Ok(())
}

fn check_start(start: &Json, binding: &Binding, path: &Path) -> Result<(), String> {
    let shown = path.display();
    if start["j"] != "start" {
        return Err(format!("journal {shown}: first record is not a start record"));
    }
    check_version(start, path)?;
    let id_ok = start["run_id"].as_str().is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    if !id_ok {
        return Err(format!("journal {shown}: start record has no valid run id"));
    }
    if start["program_sha256"] != binding.program_sha256.as_str() {
        return Err(format!(
            "journal {shown} was started with a different program; resume needs the identical program, every module included"
        ));
    }
    if start["config_sha256"] != binding.config_sha256.as_str() {
        return Err(format!(
            "journal {shown} was started with a different run config; resume needs the identical config"
        ));
    }
    if start["proxies"] != binding.proxies {
        return Err(format!(
            "journal {shown} was started with the proxies {}, but they are now {}; resume needs the same proxy \
             configuration (the proxy variables of the environment), so requests take the route they took",
            start["proxies"], binding.proxies
        ));
    }
    Ok(())
}

/// Validates the records after `start`. Each stream (the program's own, and each
/// task's, named by `"t"`) must be intent n, result n, intent n+1, ... from n = 1, where
/// only its last intent may lack a result; streams may interleave in any order. `end`
/// may only come last, with nothing in doubt. Returns each stream's intents and
/// results, and the end record.
/// Each stream's records, by task ("" for the program's own).
type Streams = BTreeMap<String, VecDeque<Json>>;

fn check_body(records: &[Json], run_id: &str, path: &Path) -> Result<(Streams, Option<Json>), String> {
    let bad = |i: usize, why: String| format!("journal {} is corrupt at line {}: {why}", path.display(), i + 2);
    /// Within an open operation: the last record was its intent or an attempt (the
    /// operation is in doubt), or a retry authorization (an attempt must come next).
    #[derive(Default)]
    struct State {
        expect_n: u64,
        open_intent: bool,
        last_was_retry: bool,
        open_op: String,
    }
    let mut pending: BTreeMap<String, VecDeque<Json>> = BTreeMap::new();
    let mut states: BTreeMap<String, State> = BTreeMap::new();
    let mut end = None;
    let mut decisions: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, rec) in records.iter().enumerate() {
        // A decision rides on the result or retry its resolution wrote: well formed, for
        // this run and this record's operation, and recorded once.
        if let Some(d) = rec.get("decision") {
            let task = rec.get("t").and_then(Json::as_str).unwrap_or_default();
            let n = rec["n"].as_u64().unwrap_or_default();
            let ok = matches!(rec["j"].as_str(), Some("result" | "retry"))
                && d.as_object().is_some_and(|o| o.len() == 6)
                && d["id"].as_str().is_some_and(|s| is_hex(s, 32))
                && !run_id.is_empty()
                && d["run_id"] == run_id
                && d["operation"] == op_id(task, n).as_str()
                && d["resolution"].as_object().is_some_and(|o| o.len() == 1)
                && d["evidence"].is_string()
                && d["staalc"].is_string();
            if !ok {
                return Err(bad(i, "malformed decision".into()));
            }
            if !decisions.insert(d["id"].as_str().unwrap_or_default().to_string()) {
                return Err(bad(i, format!("decision {} is recorded twice", d["id"].as_str().unwrap_or_default())));
            }
        }
        if end.is_some() {
            return Err(bad(i, "a record follows the end record".into()));
        }
        let task = match rec.get("t") {
            None => String::new(),
            Some(Json::String(t)) if !t.is_empty() && rec["j"] != "end" => t.clone(),
            Some(_) => return Err(bad(i, "malformed task id".into())),
        };
        let st = states.entry(task.clone()).or_insert_with(|| State { expect_n: 1, ..State::default() });
        let n = rec["n"].as_u64();
        let id = op_id(&task, st.expect_n);
        match rec["j"].as_str() {
            Some("retry") | Some("attempt") => {
                let is_retry = rec["j"] == "retry";
                if !st.open_intent || n != Some(st.expect_n) {
                    return Err(bad(i, format!("{} for operation {} that is not in doubt", rec["j"], op_id(&task, n.unwrap_or_default()))));
                }
                if st.open_op != "action" {
                    return Err(bad(i, format!("{} recorded for a {}, which is never resolved by hand", rec["j"], st.open_op)));
                }
                if is_retry == st.last_was_retry {
                    let what = if is_retry { "two retries without an attempt" } else { "an attempt without a retry" };
                    return Err(bad(i, what.into()));
                }
                st.last_was_retry = is_retry;
            }
            Some("intent") => {
                if st.open_intent {
                    return Err(bad(i, format!("operation {id} has no result but is followed by another")));
                }
                if n != Some(st.expect_n) {
                    return Err(bad(i, format!("expected operation {id}, found {}", rec["n"])));
                }
                let op_ok = matches!(rec["op"].as_str(), Some("count" | "model" | "action" | "print" | "clock" | "random" | "input"));
                if !op_ok || !rec["key"].is_string() || rec.get("body").is_none() {
                    return Err(bad(i, "malformed intent record".into()));
                }
                // The rule an action was made under, when recorded: one of the three, and
                // on an action only.
                if let Some(r) = rec.get("recovery") {
                    let known = matches!(r.as_str(), Some("repeat" | "resend_with_key" | "operator"));
                    if !known || rec["op"] != "action" {
                        return Err(bad(i, format!("an intent records the rule {r}, which only an action may, and only repeat, resend_with_key or operator")));
                    }
                }
                st.open_intent = true;
                st.last_was_retry = false;
                st.open_op = rec["op"].as_str().unwrap_or_default().to_string();
            }
            Some("result") => {
                if !st.open_intent || n != Some(st.expect_n) {
                    return Err(bad(i, format!("result for operation {} does not follow its intent", op_id(&task, n.unwrap_or_default()))));
                }
                if st.last_was_retry {
                    return Err(bad(i, "a result follows a retry with no attempt".into()));
                }
                if rec.get("ok").is_some() == rec.get("err").is_some() {
                    return Err(bad(i, "a result must have exactly one of \"ok\" or \"err\"".into()));
                }
                st.open_intent = false;
                st.last_was_retry = false;
                st.expect_n += 1;
            }
            Some("resume") => {
                // Which staalc and engine carried the run on after a crash. Not an
                // operation: no task, no number; it changes no stream.
                let hex64 = |v: &Json| v.as_str().is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
                let ok = rec.get("t").is_none()
                    && rec.get("n").is_none()
                    && !run_id.is_empty()
                    && rec["run_id"] == run_id
                    && rec["version"] == VERSION
                    && rec["staalc"].is_string()
                    && matches!(rec["engine"].as_str(), Some("interpreter" | "native"))
                    && hex64(&rec["program_sha256"])
                    && hex64(&rec["config_sha256"])
                    && rec["proxies"].is_object()
                    && rec["at_ms"].is_u64();
                if !ok {
                    return Err(bad(i, "malformed resume record".into()));
                }
                continue;
            }
            Some("end") => {
                if let Some((t, s)) = states.iter().find(|(_, s)| s.open_intent) {
                    return Err(bad(i, format!("the run ended with operation {} in doubt", op_id(t, s.expect_n))));
                }
                end = Some(rec.clone());
                continue;
            }
            _ => return Err(bad(i, format!("unknown record type {}", rec["j"]))),
        }
        pending.entry(task).or_default().push_back(rec.clone());
    }
    Ok((pending, end))
}

/// For a message: the decisions recorded for `operation` (all of them, for `None`),
/// as "; decided by decision <id> (...)", or nothing.
fn decided(records: &[Json], operation: Option<&str>) -> String {
    let found: Vec<String> = records
        .iter()
        .filter_map(|r| r.get("decision"))
        .filter(|d| operation.is_none_or(|o| d["operation"] == o))
        .map(|d| format!("decision {} ({} for operation {})", d["id"].as_str().unwrap_or("?"), d["resolution"], d["operation"].as_str().unwrap_or("?")))
        .collect();
    if found.is_empty() { String::new() } else { format!("; recorded: {}", found.join(", ")) }
}

/// A record of `task`: tagged with it, unless it is the program's own.
fn tagged(mut rec: Json, task: &str) -> Json {
    if !task.is_empty()
        && let Some(o) = rec.as_object_mut()
    {
        o.insert("t".into(), json!(task));
    }
    rec
}

fn divergence(n: &str, recorded: &Json, now: &Json) -> String {
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
         config or staalc has changed since the run started, so it cannot be resumed safely",
        summary(recorded),
        summary(now)
    )
}

// ---------------------------------------------------------------------------
// Failpoints: test-only crash injection. Compiled out unless the `failpoints`
// feature is enabled, so a production build cannot crash this way.
//
//   STAALC_FAILPOINT=after_write:N     abort right after the Nth journal record is durable
//   STAALC_FAILPOINT=before_result:ID  abort after operation ID happened, before its result is
//                                      written: N for the program's own, TASK:N for a task's

#[derive(Debug, Default, Clone)]
struct Failpoint {
    #[cfg(feature = "failpoints")]
    after_write: Option<u64>,
    #[cfg(feature = "failpoints")]
    before_result: Option<String>,
}

impl Failpoint {
    #[cfg(feature = "failpoints")]
    fn from_env() -> Failpoint {
        let mut fp = Failpoint::default();
        if let Ok(spec) = std::env::var("STAALC_FAILPOINT") {
            match spec.split_once(':') {
                Some(("after_write", v)) if v.parse::<u64>().is_ok() => fp.after_write = v.parse().ok(),
                Some(("before_result", id)) if !id.is_empty() => fp.before_result = Some(id.to_string()),
                _ => eprintln!("staalc: ignoring malformed STAALC_FAILPOINT={spec}"),
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
    fn before_result(&self, id: &str) {
        #[cfg(feature = "failpoints")]
        if self.before_result.as_deref() == Some(id) {
            std::process::abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Hook = Box<dyn FnOnce() + Send>;

    /// What to do to a path between opening it and locking it, once (`on_lock`).
    static BEFORE_LOCK: std::sync::Mutex<Vec<(std::path::PathBuf, Hook)>> = std::sync::Mutex::new(Vec::new());

    pub(super) fn before_lock(path: &Path) {
        let hook = {
            let mut hooks = BEFORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            hooks.iter().position(|(p, _)| p == path).map(|i| hooks.swap_remove(i).1)
        };
        if let Some(f) = hook {
            f();
        }
    }

    type SyncHook = Box<dyn FnOnce() + Send>;

    /// What to do once to a journal, by identity, after a record is written and before it
    /// is synced (`on_sync`).
    static BEFORE_SYNC: std::sync::Mutex<Vec<(FileId, SyncHook)>> = std::sync::Mutex::new(Vec::new());

    pub(super) fn before_sync(id: &FileId) {
        let hook = {
            let mut hooks = BEFORE_SYNC.lock().unwrap_or_else(|p| p.into_inner());
            hooks.iter().position(|(h, _)| h == id).map(|i| hooks.swap_remove(i).1)
        };
        if let Some(f) = hook {
            f();
        }
    }

    fn on_sync(id: FileId, f: impl FnOnce() + Send + 'static) {
        BEFORE_SYNC.lock().unwrap().push((id, Box::new(f)));
    }

    #[test]
    fn a_snapshot_of_a_journal_this_process_writes_says_which_records_are_durable() {
        let p = tmp("snapshot-writer");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let id = j.identity().unwrap();
        let s = snapshot(&p).unwrap();
        assert_eq!((s.records.len(), s.durable, s.identity), (1, Durable::Records(1), id));
        // Between writing an intent and syncing it, a snapshot sees it, not yet durable.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (path, out) = (p.clone(), seen.clone());
        on_sync(id, move || *out.lock().unwrap() = Some(snapshot(&path).unwrap()));
        j.begin("", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        let during = seen.lock().unwrap().take().expect("the hook ran");
        assert_eq!((during.records.len(), during.durable), (2, Durable::Records(1)));
        assert_eq!(during.records[1]["j"], "intent");
        // Once the write returns, it is durable.
        assert_eq!(snapshot(&p).unwrap().durable, Durable::Records(2));
        // The view says the same.
        let v = view(&p).unwrap();
        assert_eq!((v.durable, v.identity, v.read.records), (Durable::Records(2), id, 2));
        // Let go, nothing is known about it any more.
        drop(j);
        let s = snapshot(&p).unwrap();
        assert_eq!((s.records.len(), s.durable), (2, Durable::NotEstablished));
    }

    #[test]
    fn a_journal_opened_again_is_synced_so_all_it_read_is_durable() {
        let p = in_doubt("snapshot-reopen");
        // A torn last line, as a writer stopped mid-write leaves it.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"j\":\"res").unwrap();
        drop(f);
        let s = snapshot(&p).unwrap();
        assert_eq!((s.records.len(), s.read.unfinished_bytes, s.durable), (2, 9, Durable::NotEstablished));
        let j = Journal::resume(&p, &bind("P", "C")).unwrap();
        // Read, cut, synced, and the resume record written and synced after it.
        let s = snapshot(&p).unwrap();
        assert_eq!((s.records.len(), s.read.unfinished_bytes, s.durable), (3, 0, Durable::Records(3)));
        assert_eq!(s.records[2]["j"], "resume");
        drop(j);
        // resolve, too, holds what it read as durable while it writes.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (path, out) = (p.clone(), seen.clone());
        on_sync(FileIdentity::of_path(&p).unwrap(), move || *out.lock().unwrap() = Some(snapshot(&path).unwrap()));
        Journal::resolve(&p, Resolution::Retry, None).unwrap();
        let during = seen.lock().unwrap().take().expect("the hook ran");
        assert_eq!((during.records.len(), during.durable), (4, Durable::Records(3)));
    }

    #[test]
    fn a_snapshot_reads_one_file_and_names_it() {
        let p = in_doubt("snapshot-link");
        let link = p.with_file_name("snapshot-link-symlink");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&p, &link).unwrap();
        let s = snapshot(&link).unwrap();
        assert_eq!(s.identity, FileIdentity::of_path(&p).unwrap());
        assert_eq!(s.records, read_records(&p).unwrap());
        assert_eq!(s.read.bytes, std::fs::metadata(&p).unwrap().len());
        assert!(snapshot(&p.with_file_name("snapshot-none")).unwrap_err().contains("cannot read journal"));
    }

    /// Runs `f` the next time `path` is opened, after it is opened and before it is locked.
    fn on_lock(path: &Path, f: impl FnOnce() + Send + 'static) {
        BEFORE_LOCK.lock().unwrap().push((path.to_path_buf(), Box::new(f)));
    }

    /// A journal at `name` whose one action is in doubt, as a crash leaves it.
    fn in_doubt(name: &str) -> std::path::PathBuf {
        let p = tmp(name);
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        j.begin("", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        p
    }

    #[test]
    fn a_journal_replaced_between_opening_and_locking_is_refused_and_left_alone() {
        let p = in_doubt("swap");
        // The file opened, still reachable through a second name, and another put at the path.
        let opened = p.with_file_name("swap-opened");
        let _ = std::fs::remove_file(&opened);
        std::fs::hard_link(&p, &opened).unwrap();
        let before = std::fs::read(&opened).unwrap();
        let other = in_doubt("swap-other");
        let other_bytes = std::fs::read(&other).unwrap();
        for what in ["resume", "resolve"] {
            let (from, to) = (other.clone(), p.clone());
            // Put the other journal at the path after opening; put the original back after.
            std::fs::copy(&other, other.with_file_name("swap-spare")).unwrap();
            on_lock(&p, move || std::fs::rename(&from, &to).unwrap());
            let e = match what {
                "resume" => Journal::resume(&p, &bind("P", "C")).map(|_| ()).unwrap_err(),
                _ => Journal::resolve(&p, Resolution::Retry, None).map(|_| ()).unwrap_err(),
            };
            assert!(e.contains("was replaced while it was being opened") && e.contains("nothing was done"), "{what}: {e}");
            assert_eq!(std::fs::read(&opened).unwrap(), before, "{what}: the file opened was written to");
            assert_eq!(std::fs::read(&p).unwrap(), other_bytes, "{what}: the file swapped in was written to");
            // Back as it was for the next case.
            std::fs::rename(other.with_file_name("swap-spare"), &other).unwrap();
            std::fs::remove_file(&p).unwrap();
            std::fs::hard_link(&opened, &p).unwrap();
        }
        // A path removed between opening and locking.
        let gone = p.clone();
        on_lock(&p, move || std::fs::remove_file(&gone).unwrap());
        let e = Journal::resume(&p, &bind("P", "C")).map(|_| ()).unwrap_err();
        assert!(e.contains("was removed or moved while it was being opened"), "{e}");
        assert_eq!(std::fs::read(&opened).unwrap(), before);
        // A new journal, replaced as soon as it is made, is refused before its start record.
        let fresh = tmp("swap-new");
        let (from, to) = (other.clone(), fresh.clone());
        on_lock(&fresh, move || std::fs::rename(&from, &to).unwrap());
        let e = Journal::create(&fresh, &bind("P", "C")).map(|_| ()).unwrap_err();
        assert!(e.contains("was replaced while it was being opened"), "{e}");
        assert_eq!(std::fs::read(&fresh).unwrap(), other_bytes);
    }

    #[test]
    fn the_identity_of_the_file_locked_is_reported() {
        let p = in_doubt("identity");
        let id = FileIdentity::of_path(&p).unwrap();
        let link = p.with_file_name("identity-link");
        let _ = std::fs::remove_file(&link);
        std::fs::hard_link(&p, &link).unwrap();
        assert_eq!(FileIdentity::of_path(&link).unwrap(), id);
        let r = Journal::resolve(&link, Resolution::Retry, None).unwrap();
        assert_eq!(r.identity, id);
        let j = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert_eq!(j.identity(), Some(id));
        drop(j);
        let other = in_doubt("identity-other");
        assert_ne!(Journal::resume(&other, &bind("P", "C")).unwrap().identity(), Some(id));
        assert_eq!(Journal::memory().unwrap().identity(), None);
    }

    /// A decision with a fixed id, for the run of the journal at `p`.
    fn decision(p: &Path, id: char, evidence: &str) -> Decision {
        let run_id = read_records(p).unwrap()[0]["run_id"].as_str().unwrap().to_string();
        Decision { id: id.to_string().repeat(32), run_id, evidence: evidence.into() }
    }

    #[test]
    fn a_decision_is_recorded_with_its_resolution() {
        let p = in_doubt("decision");
        let d = decision(&p, 'a', "the payment shows in the dashboard as ch_1");
        let r = Journal::resolve_decision(&p, &d, Resolution::Performed { receipt: "ch_1".into() }, "1").unwrap();
        assert!(!r.already_recorded);
        let want = json!({
            "id": d.id, "run_id": d.run_id, "operation": "1", "resolution": { "performed": { "receipt": "ch_1" } },
            "evidence": "the payment shows in the dashboard as ch_1", "staalc": STAALC,
        });
        assert_eq!(r.decision, Some(want.clone()));
        let last = read_records(&p).unwrap().pop().unwrap();
        assert_eq!(last, json!({ "j": "result", "n": 1, "ok": { "receipt": "ch_1" }, "decision": want }));
        let v = view(&p).unwrap();
        assert_eq!(v.operations[0].decisions, vec![want]);
        // The run replays it as it would a resolution without a decision.
        let mut j = Journal::resume(&p, &bind("P", "C")).unwrap();
        let step = j.begin("", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        assert!(matches!(step, Step::Replay(rec) if rec["ok"] == json!({ "receipt": "ch_1" })));
    }

    #[test]
    fn the_same_decision_again_changes_nothing_even_after_the_run_went_on() {
        let p = in_doubt("decision-again");
        let d = decision(&p, 'b', "checked the outbox");
        let first = Journal::resolve_decision(&p, &d, Resolution::Failed { message: "never sent".into() }, "1").unwrap();
        let bytes = std::fs::read(&p).unwrap();
        let again = Journal::resolve_decision(&p, &d, Resolution::Failed { message: "never sent".into() }, "1").unwrap();
        assert!(again.already_recorded);
        assert_eq!(again.decision, first.decision);
        assert!(again.summary.contains("already recorded for operation 1"), "{}", again.summary);
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        // The run resumes and ends; the decision is still found, and nothing is written.
        let mut j = Journal::resume(&p, &bind("P", "C")).unwrap();
        j.begin("", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        j.end(json!({ "ok": true })).unwrap();
        drop(j);
        let bytes = std::fs::read(&p).unwrap();
        assert!(Journal::resolve_decision(&p, &d, Resolution::Failed { message: "never sent".into() }, "1").unwrap().already_recorded);
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        // Another decision for it is refused, naming the one recorded.
        let other = decision(&p, 'c', "checked again");
        let e = Journal::resolve_decision(&p, &other, Resolution::Retry, "1").unwrap_err();
        assert!(e.contains("already finished") && e.contains(&format!("decision {}", d.id)), "{e}");
    }

    #[test]
    fn the_same_decision_id_with_anything_different_is_refused() {
        let p = tmp("decision-conflict");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        j.begin("1.0", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        j.begin("1.1", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        drop(j);
        let d = decision(&p, 'd', "the provider's log shows message m1");
        Journal::resolve_decision(&p, &d, Resolution::Performed { receipt: "m1".into() }, "1.0:1").unwrap();
        let bytes = std::fs::read(&p).unwrap();
        let cases: Vec<(&str, Decision, Resolution, &str)> = vec![
            ("resolution", d.clone(), Resolution::Performed { receipt: "m2".into() }, "1.0:1"),
            ("resolution", d.clone(), Resolution::Retry, "1.0:1"),
            ("evidence", Decision { evidence: "something else".into(), ..d.clone() }, Resolution::Performed { receipt: "m1".into() }, "1.0:1"),
            ("operation", d.clone(), Resolution::Performed { receipt: "m1".into() }, "1.1:1"),
        ];
        for (what, given, resolution, op) in cases {
            let e = Journal::resolve_decision(&p, &given, resolution, op).unwrap_err();
            assert!(e.contains(&format!("already recorded in this journal with a different {what}")) && e.contains("nothing was recorded"), "{what}: {e}");
            assert_eq!(std::fs::read(&p).unwrap(), bytes, "{what}");
        }
        // A decision for another run is refused before anything else.
        let foreign = Decision { run_id: "f".repeat(32), ..d.clone() };
        let e = Journal::resolve_decision(&p, &foreign, Resolution::Performed { receipt: "m1".into() }, "1.0:1").unwrap_err();
        assert!(e.contains(&format!("is for run {}", "f".repeat(32))), "{e}");
        // The other operation is still in doubt, and takes a decision of its own.
        let e2 = decision(&p, 'e', "nothing in the provider's log");
        Journal::resolve_decision(&p, &e2, Resolution::Retry, "1.1:1").unwrap();
        // A second decision for an operation already decided is refused, naming the first.
        let e3 = decision(&p, '9', "checked twice");
        let e = Journal::resolve_decision(&p, &e3, Resolution::Retry, "1.0:1").unwrap_err();
        assert!(e.contains("not in doubt") && e.contains(&format!("decision {}", d.id)), "{e}");
        let e = Journal::resolve_decision(&p, &e3, Resolution::Retry, "1.1:1").unwrap_err();
        assert!(e.contains("already authorized") && e.contains(&format!("decision {}", e2.id)), "{e}");
    }

    #[test]
    fn a_retry_decision_is_found_after_the_attempt_it_authorized() {
        let p = in_doubt("decision-retry");
        let d = decision(&p, 'f', "the action never reached the service");
        let r = Journal::resolve_decision(&p, &d, Resolution::Retry, "1").unwrap();
        let last = read_records(&p).unwrap().pop().unwrap();
        assert_eq!(last["j"], "retry");
        assert_eq!(Some(last["decision"].clone()), r.decision);
        assert!(matches!(view(&p).unwrap().operations[0].state, OpState::RetryAuthorized { .. }));
        let mut j = Journal::resume(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap() else { panic!() };
        j.finish("", n, Ok(json!({ "receipt": "x" }))).unwrap();
        drop(j);
        assert!(Journal::resolve_decision(&p, &d, Resolution::Retry, "1").unwrap().already_recorded);
        assert_eq!(view(&p).unwrap().operations[0].decisions, vec![r.decision.unwrap()]);
    }

    #[test]
    fn a_decision_needs_its_id_run_and_evidence_and_a_recorded_one_must_be_well_formed() {
        let p = in_doubt("decision-checks");
        let good = decision(&p, '1', "checked");
        for (bad, want) in [
            (Decision { id: "A".repeat(32), ..good.clone() }, "decision id"),
            (Decision { id: "1".repeat(31), ..good.clone() }, "decision id"),
            (Decision { run_id: "x".into(), ..good.clone() }, "run id"),
            (Decision { evidence: "  ".into(), ..good.clone() }, "needs its evidence"),
        ] {
            let e = Journal::resolve_decision(&p, &bad, Resolution::Retry, "1").unwrap_err();
            assert!(e.contains(want), "{e}");
        }
        Journal::resolve_decision(&p, &good, Resolution::Performed { receipt: "r".into() }, "1").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let line = text.lines().last().unwrap().to_string();
        let mut rec: Json = serde_json::from_str(&line).unwrap();
        type Damage = Box<dyn Fn(&mut Json)>;
        let damaged: Vec<(&str, Damage)> = vec![
            ("another operation", Box::new(|r: &mut Json| r["decision"]["operation"] = json!("2"))),
            ("another run", Box::new(|r: &mut Json| r["decision"]["run_id"] = json!("0".repeat(32)))),
            ("a key more", Box::new(|r: &mut Json| r["decision"]["extra"] = json!(1))),
            ("no evidence", Box::new(|r: &mut Json| { r["decision"].as_object_mut().unwrap().remove("evidence"); })),
        ];
        for (what, f) in damaged {
            let mut r = rec.clone();
            f(&mut r);
            std::fs::write(&p, text.replace(&line, &r.to_string())).unwrap();
            let e = Journal::resume(&p, &bind("P", "C")).map(|_| ()).unwrap_err();
            assert!(e.contains("malformed decision"), "{what}: {e}");
        }
        // One id on two records.
        rec["n"] = json!(1);
        std::fs::write(&p, &text).unwrap();
        let mut j = Journal::resume(&p, &bind("P", "C")).unwrap();
        j.begin("", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        j.begin("", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        drop(j);
        let mut twice = rec.clone();
        twice["n"] = json!(2);
        twice["decision"]["operation"] = json!("2");
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{twice}").unwrap();
        drop(f);
        let e = Journal::resume(&p, &bind("P", "C")).map(|_| ()).unwrap_err();
        assert!(e.contains(&format!("decision {} is recorded twice", good.id)), "{e}");
    }

    /// Two resolutions of one operation at once: one is recorded, the other refused.
    #[test]
    fn two_decisions_at_once_record_one() {
        for round in 0..20 {
            let p = in_doubt(&format!("decision-race-{round}"));
            let (a, b) = (decision(&p, 'a', "first"), decision(&p, 'b', "second"));
            let go = std::sync::Arc::new(std::sync::Barrier::new(2));
            let run = |d: Decision| {
                let (p, go) = (p.clone(), go.clone());
                std::thread::spawn(move || {
                    go.wait();
                    Journal::resolve_decision(&p, &d, Resolution::Retry, "1")
                })
            };
            let (ra, rb) = (run(a), run(b));
            let results = [ra.join().unwrap(), rb.join().unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "round {round}: {results:?}");
            let refused = results.iter().find_map(|r| r.as_ref().err()).unwrap();
            assert!(refused.contains("in use by another run in this process") || refused.contains("already authorized"), "{refused}");
            assert_eq!(view(&p).unwrap().operations[0].decisions.len(), 1);
        }
    }

    /// The OS lock alone, without this process's registry, refuses a second open file
    /// in the same process (`flock` locks an open file, not a process): checked here on
    /// each platform CI runs. Taking a lock waits up to `LOCK_WAIT`, as `lock` does: a
    /// process another test starts meanwhile briefly holds a copy of every open file,
    /// and with it the lock (seen on macOS, 2026-11-12). The refusal itself is at once.
    #[test]
    fn the_os_lock_refuses_a_second_open_file_in_the_same_process() {
        fn take(f: &File) {
            let deadline = std::time::Instant::now() + LOCK_WAIT;
            loop {
                match f.try_lock() {
                    Ok(()) => return,
                    Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(5))
                    }
                    Err(e) => panic!("{e:?}"),
                }
            }
        }
        let p = in_doubt("flock");
        let a = File::open(&p).unwrap();
        let b = File::open(&p).unwrap();
        take(&a);
        assert!(matches!(b.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        drop(a);
        take(&b);
    }

    /// A binding whose hashes are the SHA-256 of `program` and `config`, on the
    /// interpreter, with no proxies.
    fn bind(program: &str, config: &str) -> Binding {
        Binding {
            program_sha256: sha256_hex(program.as_bytes()),
            config_sha256: sha256_hex(config.as_bytes()),
            engine: "interpreter",
            proxies: json!({ "net": null, "model": null }),
        }
    }

    /// The start record `bind(program, config)` writes, as a line, with a fixed run id.
    fn start_line(program: &str, config: &str) -> String {
        let b = bind(program, config);
        json!({ "j": "start", "version": VERSION, "run_id": "00000000000000000000000000000000",
                "program_sha256": b.program_sha256, "config_sha256": b.config_sha256,
                "staalc": STAALC, "engine": b.engine, "proxies": b.proxies })
        .to_string()
            + "\n"
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("staalc-journal-test-{}", std::process::id()));
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
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, key } = j.begin("", "model", json!({ "q": 1 }), None).unwrap() else { panic!() };
        assert_eq!(n, 1);
        assert!(key.ends_with("-1"));
        j.finish("", n, Ok(json!({ "a": 2 }))).unwrap();
        let Step::Perform { n, .. } = j.begin("", "action", json!({ "x": "y" }), None).unwrap() else { panic!() };
        j.finish("", n, Err("declined".into())).unwrap();
        j.end(json!({ "ok": true, "result": 7 })).unwrap();
        let run_id = j.run_id().to_string();
        drop(j);

        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert_eq!(r.run_id(), run_id);
        assert!(r.is_resuming());
        assert_eq!(r.begin("", "model", json!({ "q": 1 }), None).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": { "a": 2 } })));
        assert_eq!(r.begin("", "action", json!({ "x": "y" }), None).unwrap(), Step::Replay(json!({ "j": "result", "n": 2, "err": "declined" })));
        r.end(json!({ "ok": true, "result": 7 })).unwrap();
        // Resuming again is idempotent.
        drop(r);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        r.begin("", "model", json!({ "q": 1 }), None).unwrap();
        r.begin("", "action", json!({ "x": "y" }), None).unwrap();
        assert!(r.end(json!({ "ok": false })).is_err(), "a different ending is divergence");
    }

    #[test]
    fn tasks_are_streams_of_their_own_and_replay_whatever_the_interleaving() {
        let p = tmp("tasks");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, key } = j.begin("", "count", json!(0), None).unwrap() else { panic!() };
        j.finish("", n, Ok(json!(1))).unwrap();
        // Two tasks, interleaved: each numbered from 1 by itself, with keys of its own.
        let Step::Perform { n: a1, key: ka } = j.begin("2.0", "model", json!("a"), None).unwrap() else { panic!() };
        let Step::Perform { n: b1, key: kb } = j.begin("2.1", "model", json!("b"), None).unwrap() else { panic!() };
        assert_eq!((a1, b1), (1, 1));
        assert!(ka.ends_with("-2.0-1") && kb.ends_with("-2.1-1") && key.ends_with("-1") && ka != kb);
        j.finish("2.1", b1, Ok(json!("B"))).unwrap();
        j.finish("2.0", a1, Ok(json!("A"))).unwrap();
        let Step::Perform { n: a2, .. } = j.begin("2.0", "action", json!("a2"), None).unwrap() else { panic!() };
        assert_eq!(a2, 2);
        j.finish("2.0", a2, Ok(json!({ "receipt": "r" }))).unwrap();
        let Step::Perform { n, .. } = j.begin("", "count", json!(3), None).unwrap() else { panic!() };
        assert_eq!(n, 2, "the program's own numbering is not moved by its tasks'");
        j.finish("", n, Ok(json!(1))).unwrap();
        j.end(json!({ "ok": true })).unwrap();
        drop(j);
        // Replayed in a different order across tasks: each task still gets its own.
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "count", json!(0), None).unwrap(), Step::Replay(_)));
        assert_eq!(r.begin("2.0", "model", json!("a"), None).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": "A", "t": "2.0" })));
        assert_eq!(r.begin("2.0", "action", json!("a2"), None).unwrap(), Step::Replay(json!({ "j": "result", "n": 2, "ok": { "receipt": "r" }, "t": "2.0" })));
        assert_eq!(r.begin("2.1", "model", json!("b"), None).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": "B", "t": "2.1" })));
        // A task that asks for something else than it recorded diverges.
        drop(r);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        r.begin("", "count", json!(0), None).unwrap();
        assert!(r.begin("2.1", "model", json!("a"), None).unwrap_err().contains("operation 2.1:1"));
    }

    #[test]
    fn each_task_in_doubt_is_resolved_by_name() {
        let p = tmp("tasks-doubt");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        j.begin("1.0", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        j.begin("1.1", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap();
        drop(j); // both in flight when the process died
        let all = Journal::resolve(&p, Resolution::Retry, None).unwrap_err();
        assert!(all.contains("1.0:1, 1.1:1") && all.contains("--operation"), "{all}");
        assert!(Journal::resolve(&p, Resolution::Retry, Some("1.2:1")).unwrap_err().contains("not in doubt"));
        Journal::resolve(&p, Resolution::Performed { receipt: "R".into() }, Some("1.1:1")).unwrap();
        // One left: it is the one resolved without a name.
        let msg = Journal::resolve(&p, Resolution::Retry, None).unwrap().summary;
        assert!(msg.contains("operation 1.0:1"), "{msg}");
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("1.1", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap(), Step::Replay(_)));
        assert!(matches!(r.begin("1.0", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap(), Step::Perform { .. }));
    }

    #[test]
    fn task_records_are_validated_stream_by_stream() {
        let start = start_line("P", "C");
        let intent = |t: &str, n: u64| format!("{{\"j\":\"intent\",\"n\":{n},\"op\":\"count\",\"key\":\"k\",\"body\":1,\"t\":\"{t}\"}}\n");
        let result = |t: &str, n: u64| format!("{{\"j\":\"result\",\"n\":{n},\"ok\":1,\"t\":\"{t}\"}}\n");
        for (body, why) in [
            (format!("{}{}", intent("1.0", 2), ""), "expected operation 1.0:1"),
            (format!("{}{}", intent("1.0", 1), result("1.1", 1)), "does not follow its intent"),
            (format!("{}{}", intent("1.0", 1), "{\"j\":\"end\",\"ok\":true}\n"), "operation 1.0:1 in doubt"),
            ("{\"j\":\"intent\",\"n\":1,\"op\":\"count\",\"key\":\"k\",\"body\":1,\"t\":7}\n".to_string(), "malformed task id"),
        ] {
            let p = tmp("tasks-bad");
            std::fs::write(&p, format!("{start}{body}")).unwrap();
            let e = Journal::resume(&p, &bind("P", "C")).unwrap_err();
            assert!(e.contains(why), "{e}");
        }
        // Interleaved streams, each complete: valid.
        let p = tmp("tasks-good");
        std::fs::write(&p, format!("{start}{}{}{}{}", intent("1.0", 1), intent("1.1", 1), result("1.1", 1), result("1.0", 1))).unwrap();
        assert!(Journal::resume(&p, &bind("P", "C")).is_ok());
    }

    #[test]
    fn an_intent_without_result_is_in_doubt() {
        let p = tmp("doubt");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { key, .. } = j.begin("", "action", json!(1), None).unwrap() else { panic!() };
        drop(j); // crash before finish
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert_eq!(r.begin("", "action", json!(1), None).unwrap(), Step::InDoubt { n: 1, key: key.clone() });
        r.finish("", 1, Ok(json!({ "receipt": "R" }))).unwrap();
        let Step::Perform { n, .. } = r.begin("", "count", json!(2), None).unwrap() else { panic!() };
        assert_eq!(n, 2);
    }

    #[test]
    fn divergence_is_detected() {
        let p = tmp("diverge");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "action", json!({ "capability": "payments.refund", "method": "refund" }), None).unwrap() else { panic!() };
        j.finish("", n, Ok(json!({}))).unwrap();
        drop(j);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        let e = r.begin("", "action", json!({ "capability": "email.send", "method": "send" }), None).unwrap_err();
        assert!(e.contains("payments.refund") && e.contains("email.send"), "{e}");
    }

    #[test]
    fn program_and_config_must_be_identical() {
        let p = tmp("hashes");
        drop(Journal::create(&p, &bind("P", "C")).unwrap());
        assert!(Journal::resume(&p, &bind("P2", "C")).unwrap_err().contains("different program"));
        assert!(Journal::resume(&p, &bind("P", "C2")).unwrap_err().contains("different run config"));
        assert!(Journal::create(&p, &bind("P", "C")).unwrap_err().contains("already exists"));
    }

    #[test]
    fn a_torn_last_line_is_discarded() {
        let p = tmp("torn");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "count", json!(1), None).unwrap() else { panic!() };
        j.finish("", n, Ok(json!(5))).unwrap();
        drop(j);
        // A crash in the middle of writing the next intent.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"j\":\"intent\",\"n\":2,\"op\":\"mod").unwrap();
        drop(f);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "count", json!(1), None).unwrap(), Step::Replay(_)));
        assert!(matches!(r.begin("", "model", json!(2), None).unwrap(), Step::Perform { n: 2, .. }), "op 2 never started");
        drop(r);
        // The repaired file is well formed.
        Journal::resume(&p, &bind("P", "C")).unwrap();
    }

    #[test]
    fn damage_before_the_last_line_is_refused() {
        let p = tmp("corrupt");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "count", json!(1), None).unwrap() else { panic!() };
        j.finish("", n, Ok(json!(5))).unwrap();
        drop(j);
        let text = std::fs::read_to_string(&p).unwrap().replace("\"result\"", "\"resul");
        std::fs::write(&p, text).unwrap();
        assert!(Journal::resume(&p, &bind("P", "C")).unwrap_err().contains("corrupt"));
    }

    #[test]
    fn structure_is_validated_before_anything_runs() {
        let p = tmp("structure");
        drop(Journal::create(&p, &bind("P", "C")).unwrap());
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
            let e = Journal::resume(&p, &bind("P", "C")).unwrap_err();
            assert!(e.contains(want), "{extra:?}: {e}");
        }
    }

    #[test]
    fn only_one_run_may_hold_a_journal() {
        let p = tmp("lock");
        let held = Journal::create(&p, &bind("P", "C")).unwrap();
        // A second run in this process is refused at once, not after waiting.
        let started = std::time::Instant::now();
        assert!(Journal::resume(&p, &bind("P", "C")).unwrap_err().contains("in use by another run in this process"));
        assert!(started.elapsed() < LOCK_WAIT);
        // So is resolving, and through another path to the same file.
        let other = p.with_file_name("lock-link");
        let _ = std::fs::remove_file(&other);
        std::fs::hard_link(&p, &other).unwrap();
        assert!(Journal::resume(&other, &bind("P", "C")).unwrap_err().contains("in this process"));
        assert!(Journal::resolve(&p, Resolution::Retry, None).unwrap_err().contains("in use by another run in this process"));
        assert!(Journal::resolve(&other, Resolution::Retry, None).unwrap_err().contains("in this process"));
        drop(held);
        Journal::resume(&p, &bind("P", "C")).unwrap();
    }

    /// A process another thread starts briefly holds a copy of every open file, and so of
    /// a journal's OS lock; a journal just let go must still be taken without a refusal.
    #[test]
    fn a_journal_let_go_while_another_thread_starts_programs_is_taken_again() {
        let p = tmp("lock-spawn");
        drop(Journal::create(&p, &bind("P", "C")).unwrap());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = stop.clone();
        let spawner = std::thread::spawn(move || {
            let mut n = 0u32;
            while !s.load(std::sync::atomic::Ordering::Relaxed) {
                std::process::Command::new("true").status().unwrap();
                n += 1;
            }
            n
        });
        let mut refused = Vec::new();
        for i in 0..500 {
            if let Err(e) = Journal::resume(&p, &bind("P", "C")) {
                refused.push(format!("{i}: {e}"));
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let started = spawner.join().unwrap();
        assert!(started > 0, "no program was started meanwhile");
        assert!(refused.is_empty(), "{} of 500 refused while {started} programs started: {:?}", refused.len(), refused.first());
    }

    #[test]
    fn an_authorized_retry_is_one_attempt() {
        let p = tmp("retry");
        let body = json!({ "capability": "email.send", "method": "send" });
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        j.begin("", "action", body.clone(), None).unwrap();
        drop(j); // crash: the action is in doubt
        assert!(matches!(Journal::resume(&p, &bind("P", "C")).unwrap().begin("", "action", body.clone(), None).unwrap(), Step::InDoubt { .. }));
        Journal::resolve(&p, Resolution::Retry, None).unwrap();
        assert!(Journal::resolve(&p, Resolution::Retry, None).unwrap_err().contains("already authorized"));
        // Resume performs it, recording the attempt first.
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "action", body.clone(), None).unwrap(), Step::Perform { n: 1, .. }));
        drop(r); // crash during the authorized attempt
        // That attempt may have happened: in doubt again, never silently retried twice.
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "action", body.clone(), None).unwrap(), Step::InDoubt { n: 1, .. }));
        drop(r);
        Journal::resolve(&p, Resolution::Performed { receipt: "R9".into() }, None).unwrap();
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert_eq!(r.begin("", "action", body, None).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": { "receipt": "R9" } })));
    }

    #[test]
    fn retries_are_only_for_actions_in_doubt() {
        let p = tmp("retry-structure");
        drop(Journal::create(&p, &bind("P", "C")).unwrap());
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
            let e = Journal::resume(&p, &bind("P", "C")).unwrap_err();
            assert!(e.contains(want), "{extra:?}: {e}");
        }
        // The valid shape: intent, retry, attempt, result.
        std::fs::write(&p, format!("{base}{action}{retry}{attempt}{result}")).unwrap();
        Journal::resume(&p, &bind("P", "C")).unwrap();
    }

    #[test]
    fn resolve_answers_only_an_action_in_doubt() {
        let p = tmp("resolve");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        j.begin("", "action", json!({ "capability": "payments.refund", "method": "refund" }), None).unwrap();
        drop(j);
        let msg = Journal::resolve(&p, Resolution::Performed { receipt: "re_123".into() }, None).unwrap().summary;
        assert!(msg.contains("payments.refund.refund"), "{msg}");
        assert!(Journal::resolve(&p, Resolution::Failed { message: "x".into() }, None).unwrap_err().contains("no operation is in doubt"));
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        let step = r.begin("", "action", json!({ "capability": "payments.refund", "method": "refund" }), None).unwrap();
        assert_eq!(step, Step::Replay(json!({ "j": "result", "n": 1, "ok": { "receipt": "re_123" } })));
    }

    #[test]
    fn an_action_resume_makes_again_is_never_resolved() {
        for cap in ["fs.read", "fs.write", "db.sqlite.query", "net.tcp.accept", "net.tcp.reply", "net.tls.accept", "net.tls.reply"] {
            let p = tmp(&format!("again-{cap}"));
            let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
            j.begin("", "action", json!({ "capability": cap, "method": "m" }), None).unwrap();
            drop(j);
            let before = std::fs::read_to_string(&p).unwrap();
            for r in [Resolution::Performed { receipt: "x".into() }, Resolution::Retry] {
                let e = Journal::resolve(&p, r, None).unwrap_err();
                assert!(e.contains("resume makes again by itself") && e.contains(cap), "{e}");
            }
            assert_eq!(std::fs::read_to_string(&p).unwrap(), before, "{cap}: the journal changed");
        }
    }

    fn records(p: &Path) -> Vec<Json> {
        read_records(p).unwrap()
    }

    #[test]
    fn the_start_record_says_what_started_the_run() {
        let p = tmp("start-record");
        let mut b = bind("P", "C");
        b.engine = "native";
        b.proxies = json!({ "net": { "from": "HTTPS_PROXY", "scheme": "http", "host": "proxy.example", "port": 3128, "no_proxy": [] }, "model": null });
        drop(Journal::create(&p, &b).unwrap());
        let start = &records(&p)[0];
        assert_eq!(start["version"], VERSION);
        assert_eq!(start["staalc"], STAALC);
        assert_eq!(start["engine"], "native");
        assert_eq!(start["proxies"], b.proxies);
        assert_eq!(start["program_sha256"], sha256_hex(b"P"));
        assert_eq!(start["config_sha256"], sha256_hex(b"C"));
    }

    #[test]
    fn each_resume_of_an_unfinished_run_is_recorded_and_the_start_never_changes() {
        let p = tmp("resume-records");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "action", json!({ "x": 1 }), None).unwrap() else { panic!() };
        drop(j); // crashed with the action in doubt
        let start = records(&p)[0].clone();

        // Resumed on the other engine, then again on the first.
        let mut native = bind("P", "C");
        native.engine = "native";
        drop(Journal::resume(&p, &native).unwrap());
        drop(Journal::resume(&p, &bind("P", "C")).unwrap());
        let recs = records(&p);
        assert_eq!(recs[0], start, "the start record changed");
        let resumes: Vec<&Json> = recs.iter().filter(|r| r["j"] == "resume").collect();
        assert_eq!(resumes.len(), 2);
        assert_eq!((resumes[0]["engine"].as_str(), resumes[1]["engine"].as_str()), (Some("native"), Some("interpreter")));
        for r in &resumes {
            assert_eq!(r["run_id"], start["run_id"]);
            assert_eq!(r["staalc"], STAALC);
            assert_eq!(r["version"], VERSION);
            assert_eq!(r["program_sha256"], sha256_hex(b"P"));
            assert_eq!(r["config_sha256"], sha256_hex(b"C"));
            assert!(r["at_ms"].as_u64().unwrap() > 1_700_000_000_000);
        }
        // The resume records change no stream: the action is still in doubt, and once
        // resolved the run finishes as it would have.
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "action", json!({ "x": 1 }), None).unwrap(), Step::InDoubt { n: 1, .. }));
        r.finish("", n, Ok(json!({ "receipt": "R" }))).unwrap();
        r.end(json!({ "ok": true })).unwrap();
        drop(r);

        // A run that has ended is only replayed: nothing is added.
        let before = std::fs::read_to_string(&p).unwrap();
        drop(Journal::resume(&p, &bind("P", "C")).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    }

    #[test]
    fn a_malformed_resume_record_is_corruption() {
        let good = json!({ "j": "resume", "run_id": "00000000000000000000000000000000", "version": VERSION, "staalc": STAALC, "engine": "interpreter",
            "program_sha256": sha256_hex(b"P"), "config_sha256": sha256_hex(b"C"),
            "proxies": { "net": null, "model": null }, "at_ms": 1_800_000_000_000u64 });
        let mut bad: Vec<Json> = Vec::new();
        for (k, v) in [("run_id", json!("11111111111111111111111111111111")), ("engine", json!("jit")), ("version", json!(2)), ("program_sha256", json!("P")), ("at_ms", json!(-1)),
            ("proxies", json!(null)), ("t", json!("1.0")), ("n", json!(1))]
        {
            let mut r = good.clone();
            r[k] = v;
            bad.push(r);
        }
        let mut missing = good.clone();
        missing.as_object_mut().unwrap().remove("staalc");
        bad.push(missing);
        for r in bad {
            let p = tmp("resume-bad");
            std::fs::write(&p, format!("{}{r}\n", start_line("P", "C"))).unwrap();
            let e = Journal::resume(&p, &bind("P", "C")).unwrap_err();
            assert!(e.contains("malformed resume record"), "{r}: {e}");
        }
        let p = tmp("resume-good");
        std::fs::write(&p, format!("{}{good}\n", start_line("P", "C"))).unwrap();
        Journal::resume(&p, &bind("P", "C")).unwrap();
    }

    #[test]
    fn a_resume_needs_the_proxies_the_run_started_with() {
        let p = tmp("proxies");
        drop(Journal::create(&p, &bind("P", "C")).unwrap());
        let mut other = bind("P", "C");
        other.proxies = json!({ "net": null, "model": { "from": "ALL_PROXY", "scheme": "http", "host": "p", "port": 80, "no_proxy": [] } });
        let e = Journal::resume(&p, &other).unwrap_err();
        assert!(e.contains("proxies"), "{e}");
        assert!(check_resumable(&records(&p)[0], &other, &p).is_err());
        assert!(check_resumable(&records(&p)[0], &bind("P", "C"), &p).is_ok());
    }

    #[test]
    fn an_older_journal_version_is_refused_by_name() {
        for (version, staalc) in [(1, None), (2, Some("0.27.0"))] {
            let p = tmp("old-version");
            let mut start = json!({ "j": "start", "version": version, "run_id": "00000000000000000000000000000000",
                "program_sha256": "P", "config_sha256": "C" });
            if let Some(v) = staalc {
                start["staalc"] = json!(v);
            }
            std::fs::write(&p, format!("{start}\n")).unwrap();
            let e = Journal::resume(&p, &bind("P", "C")).unwrap_err();
            assert!(e.contains(&format!("has version {version}")) && e.contains("with the staalc that started it"), "{e}");
            let e = Journal::resolve(&p, Resolution::Retry, None).unwrap_err();
            assert!(e.contains(&format!("has version {version}")), "{e}");
        }
    }

    #[test]
    fn the_config_is_bound_by_its_canonical_form() {
        // The same config, written with its keys in another order and other spacing,
        // binds the same; any change of value does not.
        let a: Json = serde_json::from_str(r#"{"entry":"main","inputs":{"b":1,"a":2.50}}"#).unwrap();
        let b: Json = serde_json::from_str(r#"{ "inputs" : { "a" : 2.5 , "b" : 1 } , "entry" : "main" }"#).unwrap();
        let c: Json = serde_json::from_str(r#"{"entry":"main","inputs":{"b":1,"a":2.51}}"#).unwrap();
        let p = json!({ "net": null, "model": null });
        let ba = Binding::of("F", &a, "interpreter", p.clone()).unwrap();
        assert_eq!(ba, Binding::of("F", &b, "interpreter", p.clone()).unwrap());
        assert_ne!(ba.config_sha256, Binding::of("F", &c, "interpreter", p.clone()).unwrap().config_sha256);
        assert_eq!(ba.config_sha256, sha256_hex(br#"{"entry":"main","inputs":{"a":2.5,"b":1}}"#));
        assert!(Binding::of("F", &json!({ "n": 9007199254740993u64 }), "interpreter", p).is_err());
    }

    #[test]
    fn a_view_shows_each_operation_who_carried_the_run_and_what_was_read() {
        let p = tmp("view-basic");
        let action = json!({ "capability": "payments.refund", "method": "refund", "amount": "10.00 USD", "target": "o1" });
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "model", json!({ "q": 1 }), None).unwrap() else { panic!() };
        j.finish("", n, Ok(json!({ "a": 1 }))).unwrap();
        let Step::Perform { n, .. } = j.begin("", "action", action.clone(), Some("operator")).unwrap() else { panic!() };
        j.finish("", n, Err("declined".into())).unwrap();
        j.begin("", "action", action.clone(), Some("operator")).unwrap();
        drop(j); // crash: the second action is in doubt
        // A resume replays the first two operations and finds the third in doubt.
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "model", json!({ "q": 1 }), None).unwrap(), Step::Replay(_)));
        drop(r);
        let v = view(&p).unwrap();
        assert_eq!(v.sessions.len(), 2);
        assert_eq!((v.sessions[0].at_ms, v.sessions[1].at_ms.is_some()), (None, true));
        let states: Vec<(&str, &OpState, usize, bool)> = v.operations.iter().map(|o| (o.id.as_str(), &o.state, o.session, o.replayed)).collect();
        assert_eq!(
            states,
            vec![
                ("1", &OpState::Done(json!({ "a": 1 })), 0, true),
                ("2", &OpState::Failed("declined".into()), 0, true),
                ("3", &OpState::InDoubt { attempts: 0, settle: Settle::Operator }, 0, false),
            ]
        );
        assert_eq!(v.operations[2].body, action);
        assert_eq!(v.end, None);
        let len = std::fs::metadata(&p).unwrap().len();
        // start, three intents, two results, the resume
        assert_eq!(v.read, Extent { records: 7, bytes: len, unfinished_bytes: 0 });
        // Settled and ended: nothing in doubt, nothing replayed after.
        Journal::resolve(&p, Resolution::Performed { receipt: "R1".into() }, None).unwrap();
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        for (op, body) in [("model", json!({ "q": 1 })), ("action", action.clone()), ("action", action)] {
            let rule = (op == "action").then_some("operator");
            assert!(matches!(r.begin("", op, body, rule).unwrap(), Step::Replay(_)));
        }
        r.end(json!({ "ok": true })).unwrap();
        drop(r);
        let v = view(&p).unwrap();
        assert_eq!(v.sessions.len(), 3);
        assert_eq!(v.operations[2].state, OpState::Done(json!({ "receipt": "R1" })));
        // The operator's answer was recorded between sessions; the last session replayed it.
        assert!(v.operations.iter().all(|o| o.replayed), "{:?}", v.operations);
        assert_eq!(v.end, Some(json!({ "j": "end", "ok": true })));
    }

    /// For each operation of a journal with several tasks: who settles it, from the rule
    /// its intent recorded (an operator, resume, or not recorded), and, apart from that,
    /// whether `resolve` accepts a resolution for it, with `resolve`'s own reason when not.
    #[test]
    fn a_view_says_who_settles_each_operation_and_resolve_says_what_it_accepts() {
        let p = tmp("view-resolve");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        j.begin("a", "action", json!({ "capability": "email.send", "method": "send", "target": "x" }), Some("operator")).unwrap();
        j.begin("b", "action", json!({ "capability": "fs.write", "method": "write", "target": "/f" }), Some("repeat")).unwrap();
        j.begin("c", "model", json!({ "q": 2 }), None).unwrap();
        j.begin("d", "action", json!({ "capability": "payments.refund", "method": "refund" }), Some("operator")).unwrap();
        let Step::Perform { n, .. } = j.begin("e", "action", json!({ "capability": "payments.refund", "method": "refund" }), Some("operator")).unwrap() else { panic!() };
        j.finish("e", n, Ok(json!({ "receipt": "R" }))).unwrap();
        j.begin("f", "action", json!({ "capability": "net.http.get", "method": "get", "target": "https://example.com/" }), Some("repeat")).unwrap();
        j.begin("g", "action", json!({ "capability": "payments.refund", "method": "refund" }), Some("resend_with_key")).unwrap();
        j.begin("h", "action", json!({ "capability": "payments.refund", "method": "refund" }), None).unwrap();
        drop(j);
        Journal::resolve(&p, Resolution::Retry, Some("d:1")).unwrap();
        let v = view(&p).unwrap();
        assert_eq!(v.operations.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["a:1", "b:1", "c:1", "d:1", "e:1", "f:1", "g:1", "h:1"]);
        let settle = |id: &str| match &v.operations.iter().find(|o| o.id == id).unwrap().state {
            OpState::InDoubt { settle, .. } => Some(match settle {
                Settle::Operator => "operator",
                Settle::Resume(_) => "resume",
                Settle::NotRecorded => "not recorded",
            }),
            _ => None,
        };
        let got: Vec<_> = ["a:1", "b:1", "c:1", "d:1", "e:1", "f:1", "g:1", "h:1"].map(settle).to_vec();
        assert_eq!(got, [Some("operator"), Some("resume"), Some("resume"), None, None, Some("resume"), Some("resume"), Some("not recorded")]);
        // What resolve accepts: an action not always made again, in doubt and not yet
        // decided. A GET is accepted though resume would make it again: an operator may
        // give the response the server gave.
        for o in &v.operations {
            let copy = tmp(&format!("view-resolve-{}", o.task));
            std::fs::copy(&p, &copy).unwrap();
            let got = Journal::resolve(&copy, Resolution::Failed { message: "m".into() }, Some(&o.id));
            match (o.task.as_str(), &o.state) {
                ("a" | "f" | "g" | "h", OpState::InDoubt { .. }) => assert!(got.is_ok(), "{}: {got:?}", o.id),
                ("b", _) => assert!(got.unwrap_err().contains("which resume makes again by itself; nothing to resolve"), "{}", o.id),
                ("c", _) => assert!(got.unwrap_err().contains("which resume retries by itself; nothing to resolve"), "{}", o.id),
                (_, OpState::RetryAuthorized { .. }) => assert!(got.unwrap_err().contains("a retry is already authorized"), "{}", o.id),
                (_, OpState::Done(_) | OpState::Failed(_)) => assert!(got.unwrap_err().contains("is not in doubt"), "{}", o.id),
                other => panic!("{}: {other:?}", o.id),
            }
        }
        // Done in the only session: nothing has replayed it.
        assert_eq!(v.operations.iter().find(|o| o.id == "e:1").map(|o| (&o.state, o.replayed)), Some((&OpState::Done(json!({ "receipt": "R" })), false)));
    }

    /// An action replayed under another rule than its intent recorded is a divergence;
    /// one whose intent recorded none (an older journal) is compared without it.
    #[test]
    fn an_action_replays_only_under_the_rule_it_recorded() {
        let action = json!({ "capability": "payments.refund", "method": "refund" });
        let p = tmp("rule-replay");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "action", action.clone(), Some("operator")).unwrap() else { panic!() };
        j.finish("", n, Ok(json!({ "receipt": "R" }))).unwrap();
        drop(j);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        let e = r.begin("", "action", action.clone(), Some("resend_with_key")).unwrap_err();
        assert!(e.contains("replay now makes a different action"), "{e}");
        drop(r);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "action", action.clone(), Some("operator")).unwrap(), Step::Replay(_)));
        // Recorded without a rule: replayed under any.
        let p = tmp("rule-replay-older");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "action", action.clone(), None).unwrap() else { panic!() };
        j.finish("", n, Ok(json!({ "receipt": "R" }))).unwrap();
        drop(j);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "action", action, Some("operator")).unwrap(), Step::Replay(_)));
    }

    #[test]
    fn a_view_changes_nothing_reads_a_journal_another_holds_and_refuses_what_resume_refuses() {
        let p = tmp("view-held");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        j.begin("", "model", json!({ "q": 1 }), None).unwrap();
        // Held by a run: still read, without a lock.
        let before = std::fs::read(&p).unwrap();
        let v = view(&p).unwrap();
        assert!(matches!(v.operations[0].state, OpState::InDoubt { settle: Settle::Resume(_), .. }));
        // A line being written is not part of the view, and is left as it is.
        use std::io::Write as _;
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{\"j\":\"res").unwrap();
        let v2 = view(&p).unwrap();
        assert_eq!(v2.operations, v.operations);
        let part = br#"{"j":"res"#.len();
        assert_eq!((v2.read.bytes, v2.read.unfinished_bytes), (before.len() as u64, part as u64));
        assert_eq!(std::fs::read(&p).unwrap().len(), before.len() + part);
        drop(j);
        // What resume refuses, the view refuses with the same message.
        for (name, text) in [
            ("view-corrupt", start_line("P", "C") + "{\"j\":\"result\",\"n\":1,\"ok\":1}\n"),
            ("view-version", start_line("P", "C").replace(&format!("\"version\":{VERSION}"), "\"version\":2")),
            ("view-not-start", "{\"j\":\"intent\"}\n".to_string()),
        ] {
            let q = tmp(name);
            std::fs::write(&q, &text).unwrap();
            let resumed = Journal::resume(&q, &bind("P", "C")).unwrap_err();
            std::fs::write(&q, &text).unwrap();
            assert_eq!(view(&q).unwrap_err(), resumed, "{name}");
        }
        let empty = tmp("view-empty");
        std::fs::write(&empty, "").unwrap();
        assert!(view(&empty).unwrap_err().contains("is empty"));
    }

}
