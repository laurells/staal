// The host of tool calls: what `staalc mcp` and the C interface's tools do with a call,
// from the arguments to the answer, and what a host does when it opens with whatever an
// earlier process left (docs/MCP.md sections 5 to 7). One host serves one journal
// directory; the MCP server and an app's tools use the same code, so they cannot drift.
//
// A call goes through steps, each made durable before the next begins:
//   1. reserve       `reserve` in the ledger (with the call's `once` key)
//   2. run file      `<run>.run.json`: the program, engine and run config, so the run
//                    can be resumed after a crash without the tool's definition
//   3. journal       created by the runtime with the run id from step 1
//   4. run           the runtime's journal records
//   5. waiting       `waiting/<run>.json`, for a run that stopped for an operator
//   6. settle        `settle` in the ledger
//   7. answer
// and `recover` has one rule for a crash after each of them.

pub mod ledger;
pub mod tool;

use ledger::{Ledger, OnceRef, Run};
use tool::{Settings, Tool};
use crate::interp::money::Money;
use crate::interp::{self, journal, Durability, Engine, FailKind, RunResult};
use serde_json::{json, Map, Value as Json};
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The longest result sent to a client, as JSON.
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;
/// How long the server waits for another server to let go of the journal directory.
const LOCK_WAIT: Duration = Duration::from_secs(5);
/// How long the notify command may run.
const NOTIFY_LIMIT: Duration = Duration::from_secs(30);
/// How often settled runs' files are checked against `server.keep` while running.
pub const SWEEP_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// How a run ended, as a client is told it.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Done(Json),
    Failed { kind: String, message: String },
    /// Stopped: an action may have happened, or the journal could not be written.
    /// `retry`: only because a request safe to send again (with its idempotency key,
    /// or one always safe) got no answer, so resuming sends it again and needs no
    /// person's answer first.
    Stopped { kind: String, message: String, retry: bool },
}

/// A tool result: what MCP's `tools/call` answers, before a protocol version adds its
/// fields, and what the C interface's `staal_tool_call` returns.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// `done`, `failed`, `stopped` (waiting for an operator), `in_progress` (a repeated
    /// call of an operation still running), or `not_run` (refused before any run).
    pub status: &'static str,
    /// The run the answer is about, when there is one.
    pub run: Option<String>,
    pub text: String,
    pub structured: Option<Json>,
    pub is_error: bool,
}

impl Answer {
    /// A refusal before any run started.
    pub fn error(text: impl Into<String>) -> Answer {
        Answer { status: "not_run", run: None, text: text.into(), structured: None, is_error: true }
    }

    pub fn to_json(&self) -> Json {
        let mut r = json!({ "content": [{ "type": "text", "text": self.text }], "isError": self.is_error });
        if let Some(s) = &self.structured {
            r["structuredContent"] = s.clone();
        }
        r
    }
}

/// One call in progress, as the protocol sees it.
#[derive(Default)]
pub struct Ctl {
    pub cancelled: AtomicBool,
}

struct State {
    ledger: Ledger,
    /// Runs in progress in this process, with their tool.
    running: HashMap<String, String>,
}

pub struct Host {
    pub settings: Settings,
    /// How a caller follows a run, for messages: "with staal_run_status" for MCP.
    follow: String,
    state: Mutex<State>,
    /// Held for the server's life: one server per journal directory.
    _lock: File,
    notifies: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// The runs this process has started the notify command for.
    notified: Mutex<std::collections::HashSet<String>>,
}

static LOG_PREFIX: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Names the process in the log, once: "staalc mcp" for the MCP server. Hosts opened
/// through the C interface log as "staal".
pub fn set_log_prefix(prefix: &'static str) {
    let _ = LOG_PREFIX.set(prefix);
}

pub fn log(msg: &str) {
    // Standard error is the host's log; a failed write there has nowhere to go.
    let _ = writeln!(std::io::stderr(), "{}: {msg}", LOG_PREFIX.get().copied().unwrap_or("staal"));
}

fn rfc3339(ms: u64) -> String {
    interp::time::format(i64::try_from(ms).unwrap_or(i64::MAX)).unwrap_or_else(|_| ms.to_string())
}

/// Writes `bytes` to `path` so that afterwards the file is there, whole, or not there:
/// through a temporary file, flushed, renamed over `path`, and the folder flushed.
pub fn write_durably(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let res = opts.open(&tmp).and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()));
    res.and_then(|()| std::fs::rename(&tmp, path)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    sync_dir(path)
}

fn sync_dir(path: &Path) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    File::open(dir).and_then(|d| d.sync_all()).map_err(|e| format!("cannot flush {}: {e}", dir.display()))
}

/// Removes a file that may already be gone.
/// A removal is flushed into the folder before this returns: the ledger may next forget
/// the run, and a file that came back after a power loss would then be one no
/// reservation explains, which stops the next start.
fn remove(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => sync_dir(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

/// The money a run's broker charged, one total per currency, from its `usage`.
fn charged(usage: &Json) -> Result<Vec<Money>, String> {
    let mut out: Vec<Money> = Vec::new();
    for g in usage.as_object().map(|o| o.values().collect::<Vec<_>>()).unwrap_or_default() {
        for m in g["charged"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let code = m["currency"].as_str().ok_or("usage: money without a currency")?;
            let c = interp::money::Currency::parse(code)?;
            let m = Money::parse(m["amount"].as_str().ok_or("usage: money without an amount")?, c)?;
            match out.iter_mut().find(|x| x.currency == m.currency) {
                Some(x) => x.minor = x.minor.checked_add(m.minor).ok_or("usage: the amount charged is too large")?,
                None => out.push(m),
            }
        }
    }
    Ok(out)
}

fn engine_name(e: Engine) -> &'static str {
    match e {
        Engine::Interpreter => "interpreter",
        Engine::Native => "native",
    }
}

fn sha256(s: &str) -> String {
    journal::sha256_hex(s.as_bytes())
}

/// What a run's outcome tells the client. Used for a run that just ended and for an
/// earlier run a repeated call finds, so both are answered the same. `follow` says how
/// the caller can follow a run ("with staal_run_status").
pub fn answer(run: &str, o: &Outcome, follow: &str) -> Answer {
    let made = |status: &'static str, text: String, structured: Option<Json>, is_error: bool| Answer {
        status,
        run: Some(run.to_string()),
        text,
        structured,
        is_error,
    };
    match o {
        Outcome::Done(v) => {
            let text = v.to_string();
            if text.len() > MAX_RESULT_BYTES {
                return made(
                    "done",
                    format!(
                        "run {run} finished, but its result is larger than {MAX_RESULT_BYTES} bytes, too large to send; \
                         it is kept in the run's journal (the operator can read it)"
                    ),
                    None,
                    true,
                );
            }
            made("done", text, Some(json!({ "result": v })), false)
        }
        Outcome::Failed { kind, .. } if kind == "internal" => made(
            "failed",
            format!("an internal error happened in run {run}; the operator has the details in the log"),
            None,
            true,
        ),
        Outcome::Failed { kind, message } if kind == "capability" => made("failed", message.clone(), None, true),
        Outcome::Failed { kind, message } => made("failed", format!("{kind}: {message}"), None, true),
        Outcome::Stopped { retry: true, .. } => made(
            "stopped",
            format!(
                "run {run} stopped before it finished: an action it started got no answer. The action is safe to \
                 send again (with the same idempotency key, never as a new request), and it is sent again when \
                 the run is resumed at the server's next start. Do not call this tool again for it: a new call \
                 would act again. Follow it {follow}."
            ),
            Some(json!({ "status": "waiting_for_operator", "run": run })),
            true,
        ),
        Outcome::Stopped { .. } => made(
            "stopped",
            format!(
                "run {run} stopped before it finished and is waiting for an operator: an action it started may \
                 already have happened, and a person must check before it goes on. Do not call this tool again \
                 for it: a new call would act again. Follow it {follow}."
            ),
            Some(json!({ "status": "waiting_for_operator", "run": run })),
            true,
        ),
    }
}

fn outcome_of(r: &RunResult) -> Outcome {
    match &r.result {
        Ok(v) => Outcome::Done(v.clone()),
        Err(f) if matches!(f.kind, FailKind::InDoubt | FailKind::Journal) => {
            Outcome::Stopped { kind: f.kind.to_string(), message: f.message.clone(), retry: retry_safe(r) }
        }
        Err(f) => Outcome::Failed { kind: f.kind.to_string(), message: f.message.clone() },
    }
}

/// Whether a run stopped only because actions safe to perform again got no answer, so
/// that resuming it performs them again (an HTTP request with its idempotency key, or
/// an action always safe to repeat) and needs no person's answer first. Every
/// `in_doubt` event of the run's last attempt (after its last `run_start`; a run of
/// parallel tasks can have several) must say so; a model call, a print or anything
/// else in doubt is left to the next start.
fn retry_safe(r: &RunResult) -> bool {
    if !matches!(&r.result, Err(f) if f.kind == FailKind::InDoubt) {
        return false;
    }
    let start = r.log.iter().rposition(|e| e["event"] == "run_start").unwrap_or(0);
    let mut doubts = r.log[start..].iter().filter(|e| e["event"] == "in_doubt").peekable();
    doubts.peek().is_some() && doubts.all(|e| e["retry_on_resume"] == true && e["op"] == "action")
}

/// How long to wait before each resume of a run that stopped on a request safe to
/// send again, within the call that made it. Bounded: the call holds its concurrency
/// slot meanwhile, and a service that does not answer three times is left to the next
/// start and the operator.
const RETRY_WAITS: [Duration; 3] = [Duration::from_millis(250), Duration::from_secs(1), Duration::from_secs(2)];

/// How a finished run ended, from its journal's end record.
fn recorded_outcome(path: &Path) -> Result<Outcome, String> {
    let recs = journal::read_records(path)?;
    let end = recs.last().filter(|r| r["j"] == "end").ok_or("its journal records no end")?;
    if end["ok"] == true {
        return Ok(Outcome::Done(end["result"].clone()));
    }
    let kind = end["failure"].as_str().ok_or("its end record names no failure")?.to_string();
    let message = end["message"].as_str().unwrap_or_default().to_string();
    Ok(Outcome::Failed { kind, message })
}

impl Host {
    /// Takes the journal directory, opens the ledger and recovers whatever an earlier
    /// process left, before anything is answered. `follow` says how a caller follows a
    /// run, for messages ("with staal_run_status"); `warnings` are logged.
    pub fn open(settings: Settings, follow: &str, warnings: &[String]) -> Result<Arc<Host>, String> {
        let dir = &settings.journals;
        let lock_path = dir.join("server.lock");
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let lock = opts.open(&lock_path).map_err(|e| format!("cannot open {}: {e}", lock_path.display()))?;
        // A client may start the server again right after an earlier one (an SDK's
        // probe, a restart): that one is given a moment to finish and let go.
        let wait_until = Instant::now() + LOCK_WAIT;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < wait_until => std::thread::sleep(Duration::from_millis(50)),
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(format!(
                        "another process is using {} (a staalc mcp server, or an app's tools); only one process may \
                         use a journal directory at a time",
                        dir.display()
                    ));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(format!("cannot lock {}: {e}", lock_path.display())),
            }
        }
        tool::private_dir(&dir.join("waiting"))?;
        File::open(dir).and_then(|f| f.sync_all()).map_err(|e| format!("cannot flush {}: {e}", dir.display()))?;
        let ledger = Ledger::open(&dir.join("server.ledger"))?;
        let last = ledger.last_time();
        let now = ledger::clock_ms();
        if last > now {
            log(&format!("warning: the clock reads {}, behind the ledger's last event at {}", rfc3339(now), rfc3339(last)));
        } else if last > 0 && now - last > 24 * 60 * 60 * 1000 {
            log(&format!(
                "warning: the clock reads {}, more than a day after the ledger's last event at {}; \
                 if the clock jumped forward, quotas may free up early",
                rfc3339(now),
                rfc3339(last)
            ));
        }
        for w in warnings {
            log(&format!("warning: {w}"));
        }
        let s = Arc::new(Host {
            settings,
            follow: follow.to_string(),
            state: Mutex::new(State { ledger, running: HashMap::new() }),
            _lock: lock,
            notifies: Mutex::new(Vec::new()),
            notified: Mutex::new(std::collections::HashSet::new()),
        });
        s.recover()?;
        s.sweep()?;
        Ok(s)
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, String> {
        self.state.lock().map_err(|_| "the host's state was lost after a crash in another call; restart the process".to_string())
    }

    pub fn run_dir(&self, tool: &str, run: &str) -> PathBuf {
        self.settings.journals.join(tool).join(&run[..2])
    }
    pub fn journal_path(&self, tool: &str, run: &str) -> PathBuf {
        self.run_dir(tool, run).join(format!("{run}.journal"))
    }
    fn run_file(&self, tool: &str, run: &str) -> PathBuf {
        self.run_dir(tool, run).join(format!("{run}.run.json"))
    }
    fn log_path(&self, tool: &str, run: &str) -> PathBuf {
        self.run_dir(tool, run).join(format!("{run}.log.jsonl"))
    }
    pub fn waiting_path(&self, run: &str) -> PathBuf {
        self.settings.journals.join("waiting").join(format!("{run}.json"))
    }
    fn notified_path(&self, run: &str) -> PathBuf {
        self.settings.journals.join("waiting").join(format!("{run}.notify"))
    }

    // -------------------------------------------------------------------------------
    // A call

    /// Runs one call of `tool`, with `grants` when the tool takes them per call. None:
    /// the call was cancelled before its run started, so nothing is answered.
    ///
    /// `scope` is the trusted scope an app gives the call (a tenant, a user), never taken
    /// from the arguments: a `once` key is held within its scope, so two scopes calling
    /// with the same arguments get two runs, and a run's status is given only to a
    /// question in its scope. The MCP server serves one client and gives none.
    #[allow(clippy::too_many_arguments)]
    pub fn call(
        &self,
        tool: &Tool,
        args: &Map<String, Json>,
        app_inputs: Option<&Map<String, Json>>,
        grants: Option<&Map<String, Json>>,
        scope: Option<&str>,
        ctl: &Ctl,
        started: &dyn Fn(&str),
    ) -> Option<Answer> {
        if let Some(s) = scope
            && !ledger::valid_scope(s)
        {
            return Some(Answer::error("scope: expected 1 to 256 characters, with no control characters"));
        }
        // 1. The arguments, the app's inputs and the grants, checked by the runtime as
        // the run would check them.
        let checked = tool.config(args, app_inputs, grants).and_then(|cfg| {
            let plan = interp::plan(&tool.prog, &cfg)?;
            let reserve = tool.check_call(&plan, &cfg)?;
            Ok((cfg, plan, reserve))
        });
        let (cfg, plan, reserve) = match checked {
            Ok(c) => c,
            Err(e) => return Some(Answer::error(e)),
        };
        let once = tool.once.as_ref().map(|o| {
            let named: Map<String, Json> = o.by.iter().filter_map(|k| plan.inputs.get(k).map(|v| (k.clone(), v.clone()))).collect();
            // Without a scope, the key is the one the MCP server has always written.
            let key = match scope {
                None => json!([tool.name, named]),
                Some(s) => json!([tool.name, named, { "scope": s }]),
            };
            OnceRef {
                key: sha256(&key.to_string()),
                args: sha256(&Json::Object(plan.inputs.clone()).to_string()),
                keep: o.keep_ms,
            }
        });
        let run = {
            let mut st = match self.state() {
                Ok(s) => s,
                Err(e) => return Some(Answer::error(e)),
            };
            if ctl.cancelled.load(Ordering::SeqCst) {
                return None;
            }
            if st.ledger.poisoned() {
                return Some(Answer::error("the server cannot record calls after a failed write to its ledger; the operator must restart it"));
            }
            let now = st.ledger.now();
            // 2. An earlier call of the same operation.
            if let (Some(o), Some(spec)) = (&once, &tool.once)
                && let Some((id, r)) = st.ledger.find_once(&o.key)
                && !once_expired(r, now)
            {
                let (id, r) = (id.clone(), r.clone());
                let in_progress = st.running.contains_key(&id);
                drop(st);
                if r.once.as_ref().is_some_and(|x| x.args != o.args) {
                    return Some(Answer::error(format!(
                        "run {id} was already started for this operation with different arguments; \
                         a call with the same {} must have the same arguments",
                        spec.by.join(", ")
                    )));
                }
                return Some(self.earlier(&tool.name, &id, &r, in_progress));
            }
            // 3. A concurrency slot.
            let total = st.running.len();
            let mine = st.running.values().filter(|t| **t == tool.name).count();
            if total >= self.settings.max_concurrent_runs || tool.max_concurrent_runs.is_some_and(|n| mine >= n) {
                return Some(Answer::error("the server is busy with other calls; nothing was done; try again later"));
            }
            // 4. The quota.
            if let Err(e) = quota_check(&st.ledger, tool, reserve, now, scope) {
                return Some(Answer::error(e));
            }
            // 5. Reserve.
            let run = match journal::new_run_id() {
                Ok(r) => r,
                Err(e) => return Some(Answer::error(e)),
            };
            let window = tool.quota.as_ref().map(|q| q.window_ms);
            if let Err(e) = st.ledger.reserve(&run, &tool.name, reserve, window, once, scope) {
                log(&format!("tool {}: {e}", tool.name));
                return Some(Answer::error("the server could not record the call; nothing was done; the operator must restart it"));
            }
            st.running.insert(run.clone(), tool.name.clone());
            run
        };
        started(&run);
        let a = self.execute(tool, &run, &cfg);
        if let Ok(mut st) = self.state() {
            st.running.remove(&run);
        }
        if ctl.cancelled.load(Ordering::SeqCst) {
            // The run ended as it would have; its outcome is in its journal and in
            // staal_run_status. The client asked to hear nothing more.
            return None;
        }
        Some(a)
    }

    /// The answer for a repeated call of an operation an earlier run holds.
    fn earlier(&self, tool: &str, id: &str, r: &Run, in_progress: bool) -> Answer {
        if in_progress {
            return Answer {
                status: "in_progress",
                run: Some(id.to_string()),
                text: format!("this operation is already in progress as run {id}; follow it {}", self.follow),
                structured: None,
                is_error: true,
            };
        }
        if r.settled().is_none() {
            return answer(id, &Outcome::Stopped { kind: String::new(), message: String::new(), retry: false }, &self.follow);
        }
        match recorded_outcome(&self.journal_path(tool, id)) {
            Ok(o) => answer(id, &o, &self.follow),
            Err(e) => Answer {
                run: Some(id.to_string()),
                ..Answer::error(format!(
                    "run {id} already performed this operation, but its record cannot be read ({e}); it was not run again"
                ))
            },
        }
    }

    /// The folder of a run's files, `<journals>/<tool>/<shard>`. A folder made here has
    /// the folder holding it flushed, up to the journal directory, so a power loss
    /// cannot take away a folder whose files were flushed.
    fn make_run_dir(&self, tool: &str, run: &str) -> Result<(), String> {
        let dir = self.run_dir(tool, run);
        if dir.is_dir() {
            return Ok(());
        }
        tool::private_dir(&dir)?;
        let tool_dir = dir.parent().unwrap_or(&self.settings.journals);
        for d in [tool_dir, self.settings.journals.as_path()] {
            File::open(d).and_then(|f| f.sync_all()).map_err(|e| format!("cannot flush {}: {e}", d.display()))?;
        }
        Ok(())
    }

    /// Steps 2 to 7 for a reserved run.
    fn execute(&self, t: &Tool, run: &str, cfg: &Json) -> Answer {
        let tool = t.name.as_str();
        let journal = self.journal_path(tool, run);
        let file = json!({ "program": t.program, "engine": engine_name(t.engine), "config": cfg });
        let prepared = self.make_run_dir(tool, run).and_then(|_| write_durably(&self.run_file(tool, run), file.to_string().as_bytes()));
        if let Err(e) = prepared {
            log(&format!("tool {tool} run {run}: {e}"));
            return self.release(tool, run, "the host could not prepare the run; nothing was done");
        }
        let res = interp::run_config_engine(&t.prog, &t.fingerprint, cfg, Durability::CreateWithId(&journal, run), t.engine);
        match res {
            Ok(r) => {
                let r = self.retry(t, run, cfg, &journal, r);
                self.finish(tool, run, &r)
            }
            Err(e) => self.not_run(tool, run, &e),
        }
    }

    /// A run that stopped only because a request safe to send again got no answer
    /// (a lost answer, a timeout, a dropped connection) is resumed here, after each of
    /// RETRY_WAITS: the runtime sends the request again with the same idempotency key,
    /// never as a new request. Any other stop is the operator's. The log keeps every
    /// attempt's events.
    fn retry(&self, t: &Tool, run: &str, cfg: &Json, journal: &Path, mut r: RunResult) -> RunResult {
        for wait in RETRY_WAITS {
            if !retry_safe(&r) {
                break;
            }
            std::thread::sleep(wait);
            log(&format!("tool {} run {run}: an action got no answer; performing it again, as safe to repeat", t.name));
            match interp::run_config_engine(&t.prog, &t.fingerprint, cfg, Durability::Resume(journal), t.engine) {
                Ok(mut next) => {
                    let mut all = std::mem::take(&mut r.log);
                    all.append(&mut next.log);
                    next.log = all;
                    r = next;
                }
                Err(e) => {
                    // Left as it stopped: resumed at the next start, as any stopped run is.
                    log(&format!("tool {} run {run}: could not resume it ({e}); it is resumed at the next start", t.name));
                    break;
                }
            }
        }
        r
    }

    /// The runtime refused or crashed. If its journal holds nothing past its start, the
    /// run did nothing and its reservation is released; otherwise it stays, to be resumed.
    fn not_run(&self, tool: &str, run: &str, why: &str) -> Answer {
        let journal = self.journal_path(tool, run);
        let records = if journal.exists() { journal::read_records(&journal) } else { Ok(Vec::new()) };
        match records {
            Ok(recs) if recs.len() <= 1 => {
                if let Err(e) = remove(&journal) {
                    log(&format!("tool {} run {run}: {e}", tool));
                }
                self.release(tool, run, &format!("the run could not start: {why}"))
            }
            _ => {
                log(&format!("tool {} run {run}: the runtime failed part-way ({why}); the run will be resumed at the next start", tool));
                let o = Outcome::Stopped { kind: "internal".into(), message: why.to_string(), retry: false };
                self.stopped(tool, run, &o);
                answer(run, &o, &self.follow)
            }
        }
    }

    fn release(&self, tool: &str, run: &str, told: &str) -> Answer {
        if let Err(e) = remove(&self.run_file(tool, run)) {
            log(&format!("tool {} run {run}: {e}", tool));
        }
        let released = self.state().and_then(|mut st| st.ledger.release(run));
        if let Err(e) = released {
            log(&format!("tool {} run {run}: {e}", tool));
        }
        Answer::error(told)
    }

    /// Steps 5 to 7 for a run the runtime ended (or stopped).
    fn finish(&self, tool: &str, run: &str, r: &RunResult) -> Answer {
        let lines: Vec<String> = r.log.iter().map(Json::to_string).collect();
        if let Err(e) = write_durably(&self.log_path(tool, run), (lines.join("\n") + "\n").as_bytes()) {
            log(&format!("tool {} run {run}: {e}", tool));
        }
        if !r.unchecked().is_empty() {
            log(&format!("tool {} run {run}: unchecked: it loaded C functions {:?}", tool, r.unchecked()));
        }
        let o = outcome_of(r);
        if let Outcome::Failed { kind, message } = &o
            && kind == "internal"
        {
            log(&format!("tool {} run {run}: internal error: {message}", tool));
        }
        if let Outcome::Stopped { .. } = &o {
            self.stopped(tool, run, &o);
            return answer(run, &o, &self.follow);
        }
        let charged = match charged(&r.usage) {
            Ok(c) => c,
            Err(e) => {
                log(&format!("tool {} run {run}: {e}", tool));
                return Answer::error(format!("run {run} ended, but the server could not read what it spent; the operator must check it"));
            }
        };
        let kind = if matches!(o, Outcome::Done(_)) { "done" } else { "failed" };
        let settled = self.state().and_then(|mut st| st.ledger.settle(run, &charged, kind));
        let spent: Vec<String> = charged.iter().map(Money::to_string).collect();
        log(&format!("tool {} run {run}: {kind}; charged [{}]", tool, spent.join(", ")));
        if let Err(e) = settled {
            log(&format!("tool {} run {run}: {e}", tool));
            return Answer::error(format!(
                "run {run} ended, but the server could not record it; it will be recorded when the server restarts"
            ));
        }
        // The arguments are no longer needed, and a waiting record no longer applies.
        for p in [self.run_file(tool, run), self.waiting_path(run), self.notified_path(run)] {
            if let Err(e) = remove(&p) {
                log(&format!("tool {} run {run}: {e}", tool));
            }
        }
        answer(run, &o, &self.follow)
    }

    /// A run stopped for an operator: its waiting record, durable before the caller is
    /// answered, then the log and the notify command.
    fn stopped(&self, tool: &str, run: &str, o: &Outcome) {
        let Outcome::Stopped { kind, message, .. } = o else { return };
        let path = self.waiting_path(run);
        // A run that stops again in the same way (each start resumes it until an
        // operator resolves it) keeps its first record, whose message says what
        // happened when it stopped, and its operator is not told twice.
        let same = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Json>(&t).ok())
            .is_some_and(|w| w["run"] == json!(run) && w["kind"] == json!(kind));
        if same {
            log(&format!("tool {} run {run}: still waiting for an operator ({kind}): {message}", tool));
            return;
        }
        let now = self.state().map(|st| st.ledger.now()).unwrap_or_else(|_| ledger::clock_ms());
        let rec = json!({ "tool": tool, "run": run, "kind": kind, "in_doubt": message, "at": rfc3339(now) });
        match write_durably(&path, rec.to_string().as_bytes()) {
            Ok(()) => {
                failpoint("after_waiting");
                log(&format!("tool {} run {run}: waiting for an operator ({kind}): {message}; record {}", tool, path.display()));
                let _ = remove(&self.notified_path(run));
                self.notify(run);
            }
            Err(e) => log(&format!(
                "tool {} run {run}: waiting for an operator ({kind}): {message}; the waiting record could not be written ({e}), \
                 it is written again at the next start",
                tool
            )),
        }
    }

    /// Runs the notify command for a waiting record, in the background, and records
    /// how it ended beside the record.
    fn notify(&self, run: &str) {
        let Some(cmd) = self.settings.notify.clone() else { return };
        let first = self.notified.lock().map(|mut n| n.insert(run.to_string())).unwrap_or(false);
        if !first {
            return;
        }
        let record = self.waiting_path(run);
        let status = self.notified_path(run);
        let mut env: Vec<(String, String)> = Vec::new();
        for k in ["PATH", "HOME", "LANG", "TZ"].iter().map(|s| s.to_string()).chain(self.settings.notify_env.iter().cloned()) {
            if let Ok(v) = std::env::var(&k) {
                env.push((k, v));
            }
        }
        let run = run.to_string();
        let h = std::thread::spawn(move || {
            let res = run_notify(&cmd, &record, &env);
            let line = match &res {
                Ok(()) => "ok\n".to_string(),
                Err(e) => format!("failed: {e}\n"),
            };
            if let Err(e) = &res {
                log(&format!("run {run}: the notify command failed ({e}); it is run again at the next start"));
            }
            if let Err(e) = write_durably(&status, line.as_bytes()) {
                log(&format!("run {run}: {e}"));
            }
        });
        if let Ok(mut n) = self.notifies.lock() {
            n.push(h);
        }
    }

    /// Waits for every notify command started so far.
    pub fn wait_for_notifies(&self) {
        let hs: Vec<_> = self.notifies.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default();
        for h in hs {
            let _ = h.join();
        }
    }

    // -------------------------------------------------------------------------------
    // Status

    /// `staal_run_status`: a snapshot of one run, read without changing anything. A run
    /// made in a scope is described only to a question in that scope; to any other it
    /// is `unknown`, as a run never made is, so its existence is not revealed either.
    pub fn status(&self, run: &str, scope: Option<&str>) -> Json {
        let (known, running, now) = match self.state() {
            Ok(st) => (st.ledger.run(run).cloned(), st.running.contains_key(run), st.ledger.now()),
            Err(e) => return json!({ "run": run, "status": "unknown", "reason": e, "observed_at": rfc3339(ledger::clock_ms()) }),
        };
        let mut s = json!({ "run": run, "observed_at": rfc3339(now) });
        let Some(r) = known.filter(|r| r.scope.as_deref() == scope) else {
            s["status"] = json!("unknown");
            return s;
        };
        if running {
            s["status"] = json!("running");
            return s;
        }
        if r.released() {
            s["status"] = json!("not_started");
            return s;
        }
        if r.settled().is_none() {
            s["status"] = json!("waiting_for_operator");
            if let Ok(text) = std::fs::read_to_string(self.waiting_path(run))
                && let Ok(w) = serde_json::from_str::<Json>(&text)
            {
                s["failure"] = json!({ "kind": w["kind"], "message": w["in_doubt"] });
            }
            return s;
        }
        let path = self.journal_path(&r.tool, run);
        if !path.exists() {
            s["status"] = json!("expired");
            return s;
        }
        match recorded_outcome(&path) {
            Ok(Outcome::Done(v)) => {
                s["status"] = json!("done");
                // The same limit as a call's answer, so a result too large to send is
                // not sent this way either; the operator reads it from the journal.
                let bytes = v.to_string().len();
                if bytes > MAX_RESULT_BYTES {
                    s["result_bytes"] = json!(bytes);
                    s["reason"] = json!(format!(
                        "the result is larger than {MAX_RESULT_BYTES} bytes, too large to send; it is kept in the run's journal (the operator can read it)"
                    ));
                } else {
                    s["result"] = v;
                }
            }
            Ok(Outcome::Failed { kind, message }) => {
                s["status"] = json!("failed");
                let message = if kind == "internal" { "an internal error happened; the operator has the details".into() } else { message };
                s["failure"] = json!({ "kind": kind, "message": message });
            }
            Ok(Outcome::Stopped { .. }) | Err(_) => {
                s["status"] = json!("unknown");
                s["reason"] = json!("the run's journal cannot be read");
            }
        }
        s
    }

    // -------------------------------------------------------------------------------
    // At start

    /// Settles what an earlier process left (docs/MCP.md section 7). Every check is
    /// made before any run is resumed; any problem stops the server.
    fn recover(&self) -> Result<(), String> {
        let mut problems = Vec::new();
        let (ledger_runs, unfinished): (HashMap<String, Run>, Vec<(String, Run)>) = {
            let st = self.state()?;
            let all = st.ledger.runs().map(|(i, r)| (i.clone(), r.clone())).collect::<HashMap<_, _>>();
            let open = st.ledger.runs().filter(|(_, r)| r.end.is_none()).map(|(i, r)| (i.clone(), r.clone())).collect();
            (all, open)
        };
        // Files no reservation explains.
        for (tool, id, path) in self.run_files()? {
            match ledger_runs.get(&id) {
                None => problems.push(format!(
                    "{} has no reservation in the ledger, so the host cannot tell whether it ran; an operator must look at it",
                    path.display()
                )),
                Some(r) if r.tool != tool => problems.push(format!("{} belongs to tool {}, not {tool}", path.display(), r.tool)),
                Some(_) => {}
            }
        }
        // What each unfinished run needs to be resumed: its run file (the program, the
        // engine and the config it started with), and a journal started with exactly
        // that program and config. Programs are checked again from their files.
        struct Resume {
            id: String,
            tool: String,
            /// None: the run never started.
            run: Option<(crate::Checked, Engine, Json)>,
        }
        let mut plan: Vec<Resume> = Vec::new();
        for (id, r) in &unfinished {
            let tool = r.tool.clone();
            let journal = self.journal_path(&tool, id);
            let records = if journal.exists() { journal::read_records(&journal) } else { Ok(Vec::new()) };
            let records = match records {
                Ok(r) => r,
                Err(e) => {
                    problems.push(format!("run {id} of tool '{tool}': {e}"));
                    continue;
                }
            };
            if records.is_empty() {
                plan.push(Resume { id: id.clone(), tool, run: None });
                continue;
            }
            match self.resumable(&tool, id, &records[0], &journal) {
                Ok(run) => plan.push(Resume { id: id.clone(), tool, run: Some(run) }),
                Err(e) => problems.push(format!(
                    "run {id} of tool '{tool}' is unfinished and cannot be resumed: {e}. Let every run finish before \
                     changing a program or upgrading staalc (docs/MCP.md section 7); to go on, restore the old program and staalc"
                )),
            }
        }
        if !problems.is_empty() {
            return Err(format!("the journal directory {} cannot be opened:\n  {}", self.settings.journals.display(), problems.join("\n  ")));
        }
        for Resume { id, tool, run } in plan {
            let journal = self.journal_path(&tool, &id);
            let Some((checked, engine, cfg)) = run else {
                // Reserved, but the run never started: nothing happened.
                remove(&journal)?;
                remove(&self.run_file(&tool, &id))?;
                self.state()?.ledger.release(&id)?;
                log(&format!("tool {tool} run {id}: released; it had not started"));
                continue;
            };
            log(&format!("tool {tool} run {id}: resuming"));
            let prog = checked.result.as_ref().map_err(|_| format!("run {id}: its program has check errors"))?;
            let res = interp::run_config_engine(prog, &checked.fingerprint, &cfg, Durability::Resume(&journal), engine)
                .map_err(|e| format!("run {id} of tool '{tool}' could not be resumed: {e}"))?;
            self.finish(&tool, &id, &res);
        }
        // Leftovers of settled runs, and notify commands that did not succeed.
        let st = self.state()?;
        let settled: Vec<(String, String)> =
            st.ledger.runs().filter(|(_, r)| r.settled().is_some()).map(|(i, r)| (i.clone(), r.tool.clone())).collect();
        let waiting: Vec<String> = st.ledger.runs().filter(|(_, r)| r.end.is_none()).map(|(i, _)| i.clone()).collect();
        let released: Vec<(String, String)> =
            st.ledger.runs().filter(|(_, r)| r.released()).map(|(i, r)| (i.clone(), r.tool.clone())).collect();
        drop(st);
        for (id, tool) in &settled {
            remove(&self.run_file(tool, id))?;
            remove(&self.waiting_path(id))?;
            remove(&self.notified_path(id))?;
        }
        // A released run never started; whatever it left can go, so that once the
        // ledger forgets it no file is left that no reservation explains.
        for (id, tool) in &released {
            let journal = self.journal_path(tool, id);
            if journal.exists() {
                let recs = journal::read_records(&journal)?;
                if recs.len() > 1 {
                    return Err(format!("{} holds records of a run the ledger says never started; an operator must look at it", journal.display()));
                }
                remove(&journal)?;
            }
            remove(&self.run_file(tool, id))?;
        }
        for id in waiting {
            let ok = std::fs::read_to_string(self.notified_path(&id)).is_ok_and(|s| s == "ok\n");
            if !ok && self.waiting_path(&id).exists() {
                self.notify(&id);
            }
        }
        Ok(())
    }

    /// What a run needs to be resumed, from its run file, checked against its journal's
    /// start record before anything runs.
    fn resumable(&self, tool: &str, id: &str, start: &Json, journal: &Path) -> Result<(crate::Checked, Engine, Json), String> {
        let path = self.run_file(tool, id);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("its run file {} cannot be read: {e}", path.display()))?;
        let file: Json = serde_json::from_str(&text).map_err(|e| format!("its run file {} is damaged: {e}", path.display()))?;
        let program = file["program"].as_str().ok_or(format!("its run file {} names no program", path.display()))?;
        let engine = match file["engine"].as_str() {
            Some("interpreter") => Engine::Interpreter,
            Some("native") => Engine::Native,
            _ => return Err(format!("its run file {} names no engine", path.display())),
        };
        let cfg = file.get("config").filter(|c| c.is_object()).cloned().ok_or(format!("its run file {} holds no config", path.display()))?;
        let checked = crate::check_path(Path::new(program))?;
        if checked.result.is_err() {
            return Err(format!("its program {program} now has check errors"));
        }
        journal::check_resumable(start, &checked.fingerprint, &cfg, journal)?;
        Ok((checked, engine, cfg))
    }

    /// Every journal and run file under the journal directory: (tool, run id, path).
    fn run_files(&self) -> Result<Vec<(String, String, PathBuf)>, String> {
        let root = &self.settings.journals;
        let read = |d: &Path| std::fs::read_dir(d).map_err(|e| format!("cannot read {}: {e}", d.display()));
        let mut out = Vec::new();
        for t in read(root)? {
            let t = t.map_err(|e| e.to_string())?;
            let name = t.file_name().to_string_lossy().into_owned();
            if name == "waiting" || !t.file_type().is_ok_and(|f| f.is_dir()) {
                continue;
            }
            for shard in read(&t.path())? {
                let shard = shard.map_err(|e| e.to_string())?;
                if !shard.file_type().is_ok_and(|f| f.is_dir()) {
                    continue;
                }
                for f in read(&shard.path())? {
                    let f = f.map_err(|e| e.to_string())?;
                    let file = f.file_name().to_string_lossy().into_owned();
                    let id = file.strip_suffix(".journal").or_else(|| file.strip_suffix(".run.json"));
                    if let Some(id) = id {
                        out.push((name.clone(), id.to_string(), f.path()));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Deletes the files of runs settled longer ago than `server.keep`, then drops from
    /// the ledger the runs nothing needs any more (docs/MCP.md, "Keeping files").
    pub fn sweep(&self) -> Result<(), String> {
        let Some(keep) = self.settings.keep_ms else { return Ok(()) };
        let (old, now) = {
            let st = self.state()?;
            let now = st.ledger.now();
            let old: Vec<(String, String)> = st
                .ledger
                .runs()
                .filter(|(_, r)| r.settled().is_some_and(|(t, _, _)| t.saturating_add(keep) < now))
                .map(|(i, r)| (i.clone(), r.tool.clone()))
                .collect();
            (old, now)
        };
        for (id, tool) in &old {
            // The log first, so a failure part-way never leaves a log without its journal.
            for p in [self.log_path(tool, id), self.journal_path(tool, id), self.run_file(tool, id)] {
                if let Err(e) = remove(&p) {
                    log(&format!("{e}; it is tried again at the next sweep"));
                }
            }
        }
        let mut st = self.state()?;
        let dropped = st.ledger.compact(|id, r| {
            let Some((end, _, _)) = r.settled() else {
                // Unfinished runs stay; released ones never started and hold nothing.
                return !r.released();
            };
            let files_kept = end.saturating_add(keep) >= now || self.journal_path(&r.tool, id).exists();
            let in_window = r.window.is_some_and(|w| r.t.saturating_add(w) > now);
            let once_held = r.once.is_some() && !once_expired(r, now);
            files_kept || in_window || once_held
        })?;
        if dropped > 0 {
            log(&format!("dropped {dropped} finished run(s) from the ledger"));
        }
        Ok(())
    }

}

impl Host {
    /// Records the fingerprint of the key the call scopes are made with (docs/MCP.md
    /// section 12.6), in `principal-key` beside the ledger. A different key gives every
    /// caller a different scope, so the runs made under the old one could no longer be
    /// found: while any run with a scope is unsettled or holds a `once` key not yet
    /// expired, a different fingerprint (or none recorded) is refused, naming how many.
    pub fn check_principal_key(&self, fingerprint: &str) -> Result<(), String> {
        let path = self.settings.journals.join("principal-key");
        let recorded = match std::fs::read_to_string(&path) {
            Ok(t) => Some(t.trim().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        if recorded.as_deref() == Some(fingerprint) {
            return Ok(());
        }
        let st = self.state()?;
        let now = st.ledger.now();
        let pending = st.ledger.runs().filter(|(_, r)| r.scope.is_some() && ((r.end.is_none()) || (r.once.is_some() && !once_expired(r, now)))).count();
        drop(st);
        if pending > 0 {
            let what = if recorded.is_some() { "is not the key recorded" } else { "is not recorded" };
            return Err(format!(
                "server.http.auth.principal_key {what} in {}, and {pending} run(s) made for a caller are unfinished or                  hold a `once` key not yet expired; with another key their callers could not find them. Use the key                  they were made with, or change it once they have settled and their keys expired",
                path.display()
            ));
        }
        write_durably(&path, format!("{fingerprint}\n").as_bytes())
    }
}

/// Whether a run's `once` key is no longer held: it never started, or ended longer ago
/// than the key's `keep`.
fn once_expired(r: &Run, now: u64) -> bool {
    match (r.settled(), r.once.as_ref().and_then(|o| o.keep)) {
        (None, _) => r.released(),
        (Some(_), None) => false,
        (Some((end, _, _)), Some(k)) => end.saturating_add(k) < now,
    }
}

/// What a tool's runs count against its quota at `now`: the calls started in the window,
/// and the money held (worst case for unsettled runs, actual for settled ones in the window).
struct Use {
    calls: Vec<u64>,
    /// (time, minor units) of settled runs in the window, oldest first.
    settled: Vec<(u64, i128)>,
    held: i128,
}

/// What `tool`'s runs use of its quota at `now`; with `scope`, only that scope's runs.
fn usage_of(l: &Ledger, tool: &Tool, now: u64, scope: Option<&str>) -> Option<Use> {
    let q = tool.quota.as_ref()?;
    let from = now.saturating_sub(q.window_ms);
    let mut u = Use { calls: Vec::new(), settled: Vec::new(), held: 0 };
    let ours = |r: &Run| scope.is_none_or(|s| r.scope.as_deref() == Some(s));
    for (_, r) in l.runs().filter(|(_, r)| r.tool == tool.name && !r.released() && ours(r)) {
        if r.t > from {
            u.calls.push(r.t);
        }
        let Some(limit) = q.money else { continue };
        match r.settled() {
            None => u.held += i128::from(r.money.map_or(0, |m| m.minor)),
            Some((_, charged, _)) if r.t > from => {
                let m: i128 = charged.iter().filter(|m| m.currency == limit.currency).map(|m| i128::from(m.minor)).sum();
                u.held += m;
                u.settled.push((r.t, m));
            }
            Some(_) => {}
        }
    }
    u.calls.sort_unstable();
    u.settled.sort_unstable();
    Some(u)
}

/// Whether one more call of `tool` fits its quota at `now`, and, for a call in `scope`,
/// that scope's share of it; or why not and when it frees up.
fn quota_check(l: &Ledger, tool: &Tool, reserve: Option<Money>, now: u64, scope: Option<&str>) -> Result<(), String> {
    let Some(q) = tool.quota.as_ref() else { return Ok(()) };
    if let Some(u) = usage_of(l, tool, now, None) {
        fits(&u, q.window_ms, q.calls, q.money, reserve, "the quota")?;
    }
    if let (Some(share), Some(s)) = (&q.per_scope, scope)
        && let Some(u) = usage_of(l, tool, now, Some(s))
    {
        fits(&u, q.window_ms, share.calls, share.money, reserve, "this caller's quota")?;
    }
    Ok(())
}

/// Whether one more call fits `calls` and `money` per `window_ms` given use `u`.
fn fits(u: &Use, window_ms: u64, calls: Option<u64>, money: Option<Money>, reserve: Option<Money>, what: &str) -> Result<(), String> {
    let window = window_text(window_ms);
    if let Some(limit) = calls
        && u.calls.len() as u64 >= limit
    {
        // The call frees up when enough of the calls in the window leave it.
        let over = u.calls.len() - (limit as usize - 1) - 1;
        let at = u.calls[over] + window_ms;
        return Err(format!("{what} of {limit} calls per {window} is used up; nothing was done; the next call can start at {}", rfc3339(at)));
    }
    if let (Some(limit), Some(reserve)) = (money, reserve) {
        let need = i128::from(reserve.minor);
        if u.held + need > i128::from(limit.minor) {
            let mut held = u.held;
            let mut at = None;
            for (t, m) in &u.settled {
                held -= m;
                if held + need <= i128::from(limit.minor) {
                    at = Some(t + window_ms);
                    break;
                }
            }
            let when = match at {
                Some(t) => format!("enough frees up at {}", rfc3339(t)),
                None => "it frees up only when runs waiting for an operator are resolved".into(),
            };
            return Err(format!(
                "{what} of {limit} per {window} cannot hold this call, which can spend up to {reserve}; nothing was done; {when}"
            ));
        }
    }
    Ok(())
}

pub fn describe_quota(l: &Ledger, tool: &Tool, now: u64) -> String {
    let (Some(q), Some(u)) = (tool.quota.as_ref(), usage_of(l, tool, now, None)) else { return "no quota".into() };
    let mut parts = Vec::new();
    if let Some(c) = q.calls {
        parts.push(format!("{} of {c} calls", u.calls.len()));
    }
    if let Some(m) = q.money {
        let held = Money { minor: i64::try_from(u.held).unwrap_or(i64::MAX), currency: m.currency };
        parts.push(format!("{held} of {m}"));
    }
    format!("{} in the last {}", parts.join(", "), window_text(q.window_ms))
}

fn window_text(ms: u64) -> String {
    const MIN: u64 = 60_000;
    if ms.is_multiple_of(24 * 60 * MIN) {
        format!("{}d", ms / (24 * 60 * MIN))
    } else if ms.is_multiple_of(60 * MIN) {
        format!("{}h", ms / (60 * MIN))
    } else {
        format!("{}m", ms / MIN)
    }
}

/// Test-only crash injection (feature `failpoints`): `STAALC_MCP_FAILPOINT=<name>`
/// aborts the process at the point of that name.
#[cfg(feature = "failpoints")]
fn failpoint(name: &str) {
    if std::env::var("STAALC_MCP_FAILPOINT").is_ok_and(|v| v == name) {
        std::process::abort();
    }
}

#[cfg(not(feature = "failpoints"))]
fn failpoint(_: &str) {}

/// Runs the notify command with a cleared environment, no input and at most
/// NOTIFY_LIMIT; its output goes to the server's standard error.
fn run_notify(cmd: &[String], record: &Path, env: &[(String, String)]) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]).arg(record).env_clear().stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    for (k, v) in env {
        c.env(k, v);
    }
    let mut child = c.spawn().map_err(|e| format!("cannot start {}: {e}", cmd[0]))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{} exited with {status}", cmd[0])),
            Ok(None) if start.elapsed() > NOTIFY_LIMIT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} ran longer than {} s and was stopped", cmd[0], NOTIFY_LIMIT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("cannot wait for {}: {e}", cmd[0])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_too_large_is_not_sent() {
        let big = Json::String("x".repeat(MAX_RESULT_BYTES));
        let a = answer("r", &Outcome::Done(big), "x");
        assert!(a.is_error && a.text.contains("too large") && a.structured.is_none());
        let a = answer("r", &Outcome::Done(json!({ "ok": 1 })), "x");
        assert_eq!(a.structured, Some(json!({ "result": { "ok": 1 } })));
        assert!(!a.is_error);
    }

    #[test]
    fn only_a_run_whose_every_doubt_is_safe_to_repeat_is_retried_in_the_call() {
        let stopped = |log: Vec<Json>| RunResult {
            result: Err(interp::Failure { kind: FailKind::InDoubt, message: "m".into(), raised: None }),
            log,
            usage: json!({}),
        };
        let start = json!({ "event": "run_start" });
        let keyed = json!({ "event": "in_doubt", "op": "action", "retry_on_resume": true });
        let unkeyed = json!({ "event": "in_doubt" });
        let model = json!({ "event": "in_doubt", "op": "model", "retry_on_resume": true });
        assert!(retry_safe(&stopped(vec![start.clone(), keyed.clone()])));
        // Parallel tasks: one in doubt with no key is the operator's, whichever comes last.
        assert!(!retry_safe(&stopped(vec![start.clone(), unkeyed.clone(), keyed.clone()])));
        assert!(!retry_safe(&stopped(vec![start.clone(), keyed.clone(), unkeyed.clone()])));
        // A model call, a print or a clock read is never repeated within the call.
        assert!(!retry_safe(&stopped(vec![start.clone(), model])));
        // Only the last attempt counts: an earlier one's doubt was resolved by resuming.
        assert!(retry_safe(&stopped(vec![start.clone(), unkeyed.clone(), start.clone(), keyed.clone()])));
        assert!(!retry_safe(&stopped(vec![start.clone(), keyed.clone(), start.clone(), unkeyed])));
        assert!(!retry_safe(&stopped(vec![start.clone()])));
        let done = RunResult { result: Ok(json!(1)), log: vec![start, keyed], usage: json!({}) };
        assert!(!retry_safe(&done));
    }

    #[test]
    fn an_internal_failure_hides_its_details() {
        let a = answer("abc", &Outcome::Failed { kind: "internal".into(), message: "secret detail".into() }, "x");
        assert!(a.is_error && a.text.contains("abc") && !a.text.contains("secret detail"));
        let a = answer("abc", &Outcome::Failed { kind: "capability".into(), message: "allows at most 50.00 USD".into() }, "x");
        assert_eq!(a.text, "allows at most 50.00 USD");
    }

    #[test]
    fn charges_are_summed_per_currency() {
        let usage = json!({
            "a": { "charged": [{ "amount": "1.50", "currency": "USD" }, { "amount": "2", "currency": "JPY" }] },
            "b": { "charged": [{ "amount": "0.50", "currency": "USD" }] },
            "c": { "charged": [] }
        });
        let c = charged(&usage).unwrap();
        assert_eq!(c.iter().map(Money::to_string).collect::<Vec<_>>(), ["2.00 USD", "2 JPY"]);
    }
}
