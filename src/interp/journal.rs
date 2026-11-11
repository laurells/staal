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

/// How an operator resolves an action left in doubt (`staalc resolve`).
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
        Journal { file, _held: held, poisoned: false, streams, recorded_end, run_id, failpoint: Failpoint::from_env(), writes: 0 }
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
        let (file, held, records) = open_existing(path)?;
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

    /// Records an operator's answer for an action left in doubt.
    /// Records an operator's answer for an action in doubt. When tasks leave several in
    /// doubt, `operation` names the one (`op_id`).
    pub fn resolve(path: &Path, resolution: Resolution, operation: Option<&str>) -> Result<String, String> {
        let (file, held, records) = open_existing(path)?;
        let start = records.first().ok_or_else(|| format!("journal {} is empty", path.display()))?;
        if start["j"] != "start" {
            return Err(format!("journal {}: first record is not a start record", path.display()));
        }
        check_version(start, path)?;
        let run_id = start["run_id"].as_str().unwrap_or_default();
        let (pending, recorded_end) = check_body(&records[1..], run_id, path)?;
        if recorded_end.is_some() {
            return Err("this run has already finished; nothing is in doubt".into());
        }
        // Each stream whose last record leaves an operation open.
        let mut open: Vec<(String, &Json)> = Vec::new();
        for (t, recs) in &pending {
            match recs.back().and_then(|r| r["j"].as_str()) {
                Some("intent" | "attempt") => {
                    let intent = recs.iter().rev().find(|r| r["j"] == "intent").ok_or("no operation is in doubt in this journal")?;
                    open.push((t.clone(), intent));
                }
                Some("retry") => {
                    let n = recs.back().and_then(|r| r["n"].as_u64()).unwrap_or_default();
                    if operation.is_none_or(|o| o == op_id(t, n)) {
                        return Err(format!("a retry is already authorized for operation {}; resume the run", op_id(t, n)));
                    }
                }
                _ => {}
            }
        }
        let ids: Vec<String> = open.iter().map(|(t, r)| op_id(t, r["n"].as_u64().unwrap_or_default())).collect();
        let (task, intent) = match (operation, open.len()) {
            (_, 0) => return Err("no operation is in doubt in this journal".into()),
            (None, 1) => open.swap_remove(0),
            (None, _) => {
                return Err(format!("operations {} are in doubt; name one with --operation", ids.join(", ")));
            }
            (Some(o), _) => match ids.iter().position(|i| i == o) {
                Some(k) => open.swap_remove(k),
                None => return Err(format!("operation {o} is not in doubt; in doubt: {}", ids.join(", "))),
            },
        };
        if intent["op"] != "action" {
            return Err(format!(
                "operation {} in doubt is a {}, which resume retries by itself; nothing to resolve",
                op_id(&task, intent["n"].as_u64().unwrap_or_default()),
                intent["op"]
            ));
        }
        let n = intent["n"].as_u64().unwrap_or_default(); // checked by check_body
        // Actions always safe to make again: resume makes them again, and a result
        // recorded here could not take the form the program reads.
        if let Some(c @ ("fs.read" | "fs.write" | "db.sqlite.query" | "db.postgres.query" | "net.tcp.accept" | "net.tcp.reply" | "net.tls.accept"
            | "net.tls.reply")) =
            intent["body"]["capability"].as_str()
        {
            return Err(format!("operation {} in doubt is {c}, which resume makes again by itself; nothing to resolve", op_id(&task, n)));
        }
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
        let shown = op_id(&task, n);
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
                j.write(&tagged(json!({ "j": "retry", "n": n }), &task))?;
                return Ok(format!("authorized one more attempt at {what} (operation {shown})"));
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
        j.finish(&task, n, result)?;
        Ok(summary)
    }

    /// Begins external operation `op` of `task` ("" for the program's own) with the
    /// given intent `body`.
    pub fn begin(&mut self, task: &str, op: &str, body: Json) -> Result<Step, String> {
        let stream = self.streams.entry(task.to_string()).or_default();
        let n = stream.next_n;
        stream.next_n += 1;
        let key = if task.is_empty() { format!("{}-{n}", self.run_id) } else { format!("{}-{task}-{n}", self.run_id) };
        let intent = tagged(json!({ "j": "intent", "n": n, "op": op, "key": key, "body": body }), task);
        if let Some(recorded) = stream.pending.pop_front() {
            if recorded != intent {
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

/// The identity of an open file: its device and inode, so two paths to one file, or a
/// path re-pointed at another, are never confused.
#[cfg(unix)]
type FileId = (u64, u64);
#[cfg(not(unix))]
type FileId = std::path::PathBuf;

#[cfg(unix)]
fn file_id(file: &File, _path: &Path) -> Result<FileId, String> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|e| e.to_string())?;
    Ok((m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_file: &File, path: &Path) -> Result<FileId, String> {
    std::fs::canonicalize(path).map_err(|e| e.to_string())
}

/// The journals this process holds.
static HELD: std::sync::Mutex<Vec<FileId>> = std::sync::Mutex::new(Vec::new());

/// This process's hold on one journal; dropping it lets the journal go.
#[derive(Debug)]
struct Held(FileId);

impl Drop for Held {
    fn drop(&mut self) {
        HELD.lock().unwrap_or_else(|p| p.into_inner()).retain(|id| *id != self.0);
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
    {
        let mut held = HELD.lock().unwrap_or_else(|p| p.into_inner());
        if held.contains(&id) {
            return Err(format!("journal {shown} is in use by another run in this process"));
        }
        #[cfg_attr(unix, allow(clippy::clone_on_copy, reason = "a FileId is a path, not Copy, off Unix"))]
        held.push(id.clone());
    }
    let hold = Held(id);
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(hold),
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
fn open_existing(path: &Path) -> Result<(File, Held, Vec<Json>), String> {
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
    Ok((file, held, records))
}

/// A journal's complete records, read without opening it for writing or locking it:
/// for a reader that must change nothing (a status query). A torn last line is left
/// out, as resuming would cut it off; any other damage is an error. A file with no
/// complete record gives an empty list.
pub fn read_records(path: &Path) -> Result<Vec<Json>, String> {
    let shown = path.display();
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read journal {shown}: {e}"))?;
    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let text = std::str::from_utf8(&bytes[..complete]).map_err(|_| format!("journal {shown} is corrupt: not UTF-8"))?;
    let mut records = Vec::new();
    for (i, line) in text.split_terminator('\n').enumerate() {
        let rec: Json = serde_json::from_str(line).map_err(|e| format!("journal {shown} is corrupt at line {}: {e}", i + 1))?;
        if !rec.is_object() {
            return Err(format!("journal {shown} is corrupt at line {}: not a record", i + 1));
        }
        records.push(rec);
    }
    Ok(records)
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
    for (i, rec) in records.iter().enumerate() {
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
        let Step::Perform { n, key } = j.begin("", "model", json!({ "q": 1 })).unwrap() else { panic!() };
        assert_eq!(n, 1);
        assert!(key.ends_with("-1"));
        j.finish("", n, Ok(json!({ "a": 2 }))).unwrap();
        let Step::Perform { n, .. } = j.begin("", "action", json!({ "x": "y" })).unwrap() else { panic!() };
        j.finish("", n, Err("declined".into())).unwrap();
        j.end(json!({ "ok": true, "result": 7 })).unwrap();
        let run_id = j.run_id().to_string();
        drop(j);

        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert_eq!(r.run_id(), run_id);
        assert!(r.is_resuming());
        assert_eq!(r.begin("", "model", json!({ "q": 1 })).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": { "a": 2 } })));
        assert_eq!(r.begin("", "action", json!({ "x": "y" })).unwrap(), Step::Replay(json!({ "j": "result", "n": 2, "err": "declined" })));
        r.end(json!({ "ok": true, "result": 7 })).unwrap();
        // Resuming again is idempotent.
        drop(r);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        r.begin("", "model", json!({ "q": 1 })).unwrap();
        r.begin("", "action", json!({ "x": "y" })).unwrap();
        assert!(r.end(json!({ "ok": false })).is_err(), "a different ending is divergence");
    }

    #[test]
    fn tasks_are_streams_of_their_own_and_replay_whatever_the_interleaving() {
        let p = tmp("tasks");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, key } = j.begin("", "count", json!(0)).unwrap() else { panic!() };
        j.finish("", n, Ok(json!(1))).unwrap();
        // Two tasks, interleaved: each numbered from 1 by itself, with keys of its own.
        let Step::Perform { n: a1, key: ka } = j.begin("2.0", "model", json!("a")).unwrap() else { panic!() };
        let Step::Perform { n: b1, key: kb } = j.begin("2.1", "model", json!("b")).unwrap() else { panic!() };
        assert_eq!((a1, b1), (1, 1));
        assert!(ka.ends_with("-2.0-1") && kb.ends_with("-2.1-1") && key.ends_with("-1") && ka != kb);
        j.finish("2.1", b1, Ok(json!("B"))).unwrap();
        j.finish("2.0", a1, Ok(json!("A"))).unwrap();
        let Step::Perform { n: a2, .. } = j.begin("2.0", "action", json!("a2")).unwrap() else { panic!() };
        assert_eq!(a2, 2);
        j.finish("2.0", a2, Ok(json!({ "receipt": "r" }))).unwrap();
        let Step::Perform { n, .. } = j.begin("", "count", json!(3)).unwrap() else { panic!() };
        assert_eq!(n, 2, "the program's own numbering is not moved by its tasks'");
        j.finish("", n, Ok(json!(1))).unwrap();
        j.end(json!({ "ok": true })).unwrap();
        drop(j);
        // Replayed in a different order across tasks: each task still gets its own.
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "count", json!(0)).unwrap(), Step::Replay(_)));
        assert_eq!(r.begin("2.0", "model", json!("a")).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": "A", "t": "2.0" })));
        assert_eq!(r.begin("2.0", "action", json!("a2")).unwrap(), Step::Replay(json!({ "j": "result", "n": 2, "ok": { "receipt": "r" }, "t": "2.0" })));
        assert_eq!(r.begin("2.1", "model", json!("b")).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": "B", "t": "2.1" })));
        // A task that asks for something else than it recorded diverges.
        drop(r);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        r.begin("", "count", json!(0)).unwrap();
        assert!(r.begin("2.1", "model", json!("a")).unwrap_err().contains("operation 2.1:1"));
    }

    #[test]
    fn each_task_in_doubt_is_resolved_by_name() {
        let p = tmp("tasks-doubt");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        j.begin("1.0", "action", json!({ "capability": "email.send", "method": "send" })).unwrap();
        j.begin("1.1", "action", json!({ "capability": "email.send", "method": "send" })).unwrap();
        drop(j); // both in flight when the process died
        let all = Journal::resolve(&p, Resolution::Retry, None).unwrap_err();
        assert!(all.contains("1.0:1, 1.1:1") && all.contains("--operation"), "{all}");
        assert!(Journal::resolve(&p, Resolution::Retry, Some("1.2:1")).unwrap_err().contains("not in doubt"));
        Journal::resolve(&p, Resolution::Performed { receipt: "R".into() }, Some("1.1:1")).unwrap();
        // One left: it is the one resolved without a name.
        let msg = Journal::resolve(&p, Resolution::Retry, None).unwrap();
        assert!(msg.contains("operation 1.0:1"), "{msg}");
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("1.1", "action", json!({ "capability": "email.send", "method": "send" })).unwrap(), Step::Replay(_)));
        assert!(matches!(r.begin("1.0", "action", json!({ "capability": "email.send", "method": "send" })).unwrap(), Step::Perform { .. }));
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
        let Step::Perform { key, .. } = j.begin("", "action", json!(1)).unwrap() else { panic!() };
        drop(j); // crash before finish
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert_eq!(r.begin("", "action", json!(1)).unwrap(), Step::InDoubt { n: 1, key: key.clone() });
        r.finish("", 1, Ok(json!({ "receipt": "R" }))).unwrap();
        let Step::Perform { n, .. } = r.begin("", "count", json!(2)).unwrap() else { panic!() };
        assert_eq!(n, 2);
    }

    #[test]
    fn divergence_is_detected() {
        let p = tmp("diverge");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "action", json!({ "capability": "payments.refund", "method": "refund" })).unwrap() else { panic!() };
        j.finish("", n, Ok(json!({}))).unwrap();
        drop(j);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        let e = r.begin("", "action", json!({ "capability": "email.send", "method": "send" })).unwrap_err();
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
        let Step::Perform { n, .. } = j.begin("", "count", json!(1)).unwrap() else { panic!() };
        j.finish("", n, Ok(json!(5))).unwrap();
        drop(j);
        // A crash in the middle of writing the next intent.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"j\":\"intent\",\"n\":2,\"op\":\"mod").unwrap();
        drop(f);
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "count", json!(1)).unwrap(), Step::Replay(_)));
        assert!(matches!(r.begin("", "model", json!(2)).unwrap(), Step::Perform { n: 2, .. }), "op 2 never started");
        drop(r);
        // The repaired file is well formed.
        Journal::resume(&p, &bind("P", "C")).unwrap();
    }

    #[test]
    fn damage_before_the_last_line_is_refused() {
        let p = tmp("corrupt");
        let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
        let Step::Perform { n, .. } = j.begin("", "count", json!(1)).unwrap() else { panic!() };
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
        j.begin("", "action", body.clone()).unwrap();
        drop(j); // crash: the action is in doubt
        assert!(matches!(Journal::resume(&p, &bind("P", "C")).unwrap().begin("", "action", body.clone()).unwrap(), Step::InDoubt { .. }));
        Journal::resolve(&p, Resolution::Retry, None).unwrap();
        assert!(Journal::resolve(&p, Resolution::Retry, None).unwrap_err().contains("already authorized"));
        // Resume performs it, recording the attempt first.
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "action", body.clone()).unwrap(), Step::Perform { n: 1, .. }));
        drop(r); // crash during the authorized attempt
        // That attempt may have happened: in doubt again, never silently retried twice.
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert!(matches!(r.begin("", "action", body.clone()).unwrap(), Step::InDoubt { n: 1, .. }));
        drop(r);
        Journal::resolve(&p, Resolution::Performed { receipt: "R9".into() }, None).unwrap();
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        assert_eq!(r.begin("", "action", body).unwrap(), Step::Replay(json!({ "j": "result", "n": 1, "ok": { "receipt": "R9" } })));
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
        j.begin("", "action", json!({ "capability": "payments.refund", "method": "refund" })).unwrap();
        drop(j);
        let msg = Journal::resolve(&p, Resolution::Performed { receipt: "re_123".into() }, None).unwrap();
        assert!(msg.contains("payments.refund.refund"), "{msg}");
        assert!(Journal::resolve(&p, Resolution::Failed { message: "x".into() }, None).unwrap_err().contains("no operation is in doubt"));
        let mut r = Journal::resume(&p, &bind("P", "C")).unwrap();
        let step = r.begin("", "action", json!({ "capability": "payments.refund", "method": "refund" })).unwrap();
        assert_eq!(step, Step::Replay(json!({ "j": "result", "n": 1, "ok": { "receipt": "re_123" } })));
    }

    #[test]
    fn an_action_resume_makes_again_is_never_resolved() {
        for cap in ["fs.read", "fs.write", "db.sqlite.query", "net.tcp.accept", "net.tcp.reply", "net.tls.accept", "net.tls.reply"] {
            let p = tmp(&format!("again-{cap}"));
            let mut j = Journal::create(&p, &bind("P", "C")).unwrap();
            j.begin("", "action", json!({ "capability": cap, "method": "m" })).unwrap();
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
        let Step::Perform { n, .. } = j.begin("", "action", json!({ "x": 1 })).unwrap() else { panic!() };
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
        assert!(matches!(r.begin("", "action", json!({ "x": 1 })).unwrap(), Step::InDoubt { n: 1, .. }));
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
}
