// The server's ledger (docs/MCP.md sections 6 and 7): which runs exist and what they
// hold. One JSON object per line, each written and flushed with fsync before anything
// that depends on it happens:
//
//   {"e":"reserve","run":ID,"tool":NAME,"t":MS,"money":MONEY?,"window":MS?,
//    "once":{"key":HEX,"args":HEX,"keep":MS|null}?}
//   {"e":"settle","run":ID,"t":MS,"charged":[MONEY...],"outcome":"done"|"failed"}
//   {"e":"release","run":ID,"t":MS}
//
// where MONEY is {"amount": "12.50", "currency": "USD"} and MS is UTC milliseconds since
// 1970 (or, for `window` and `keep`, a length of time). `window` is the tool's quota
// window and `once.keep` how long its key is held after the run ends (null: as long as
// its files), so the ledger can be compacted without the tools' definitions. A run exists from its `reserve`; it has at most one `settle` or `release`.
// Rules:
//  - An event is applied to memory only once it is durable; a failed write poisons the
//    ledger, which then refuses every write.
//  - Only the last line may be torn (a crash during its write). It is cut off when the
//    ledger is opened; any other damage is refused.
//  - Time never goes backwards: an event is stamped with the later of the clock and
//    the last event's time.

use crate::interp::money::{Currency, Money};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct OnceRef {
    /// SHA-256 of the tool's name and the arguments that identify the operation.
    pub key: String,
    /// SHA-256 of all of the call's arguments.
    pub args: String,
    /// How long after the run ends the key is held; None: as long as the run's files.
    pub keep: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum End {
    /// The run ended (done or failed) and was charged this.
    Settled { t: u64, charged: Vec<Money>, outcome: String },
    /// The run never started.
    Released { t: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub tool: String,
    /// When it was reserved.
    pub t: u64,
    /// What it holds against the money quota until settled.
    pub money: Option<Money>,
    /// Its tool's quota window when it was reserved, if the tool had a quota.
    pub window: Option<u64>,
    pub once: Option<OnceRef>,
    pub end: Option<End>,
}

impl Run {
    pub fn settled(&self) -> Option<(u64, &[Money], &str)> {
        match &self.end {
            Some(End::Settled { t, charged, outcome }) => Some((*t, charged, outcome)),
            _ => None,
        }
    }
    pub fn released(&self) -> bool {
        matches!(self.end, Some(End::Released { .. }))
    }
}

#[derive(Debug)]
pub struct Ledger {
    path: PathBuf,
    /// None for a ledger opened only to read, which refuses every write.
    file: Option<File>,
    poisoned: bool,
    last_t: u64,
    runs: HashMap<String, Run>,
    /// Run ids in the order they were reserved.
    order: Vec<String>,
    /// Whether the clock was found behind the ledger at the last stamp.
    pub clock_behind: bool,
    writes: u64,
    failpoint: Option<u64>,
}

/// The wall clock, in milliseconds since 1970 (UTC).
pub fn clock_ms() -> u64 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

pub fn money_json(m: &Money) -> Json {
    json!({ "amount": m.amount_string(), "currency": m.currency.code })
}

fn money_from(v: &Json) -> Result<Money, String> {
    let code = v["currency"].as_str().ok_or("money without a currency")?;
    let amount = v["amount"].as_str().ok_or("money without an amount")?;
    Money::parse(amount, Currency::parse(code)?)
}

fn is_run_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Ledger {
    /// Opens (or creates) the ledger at `path`. The caller holds the server's lock.
    pub fn open(path: &Path) -> Result<Ledger, String> {
        let shown = path.display();
        let mut opts = OpenOptions::new();
        opts.read(true).append(true).create(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut file = opts.open(path).map_err(|e| format!("cannot open the ledger {shown}: {e}"))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| format!("cannot read the ledger {shown}: {e}"))?;
        // A torn last line is a write the crash interrupted: nothing depended on it.
        let good = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        if good < bytes.len() {
            file.set_len(good as u64).and_then(|()| file.sync_all()).map_err(|e| format!("cannot repair the ledger {shown}: {e}"))?;
            bytes.truncate(good);
        }
        let mut l = Ledger {
            path: path.to_path_buf(),
            file: Some(file),
            poisoned: false,
            last_t: 0,
            runs: HashMap::new(),
            order: Vec::new(),
            clock_behind: false,
            writes: 0,
            failpoint: failpoint(),
        };
        l.load(&bytes)?;
        if !bytes.is_empty() {
            sync_dir(path)?;
        }
        Ok(l)
    }

    /// Reads the ledger at `path` without writing to it, locking it or repairing it
    /// (a torn last line is left out): for `staalc mcp status`, beside a running server.
    pub fn read_only(path: &Path) -> Result<Ledger, String> {
        let mut bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("cannot read the ledger {}: {e}", path.display())),
        };
        bytes.truncate(bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1));
        let mut l = Ledger {
            path: path.to_path_buf(),
            file: None,
            poisoned: true,
            last_t: 0,
            runs: HashMap::new(),
            order: Vec::new(),
            clock_behind: false,
            writes: 0,
            failpoint: None,
        };
        l.load(&bytes)?;
        Ok(l)
    }

    fn load(&mut self, bytes: &[u8]) -> Result<(), String> {
        let shown = self.path.display().to_string();
        let text = std::str::from_utf8(bytes).map_err(|_| format!("the ledger {shown} is not UTF-8"))?;
        for (i, line) in text.lines().enumerate() {
            let ev: Json = serde_json::from_str(line).map_err(|e| format!("the ledger {shown} is damaged at line {}: {e}", i + 1))?;
            self.check(&ev).and_then(|()| self.apply(&ev)).map_err(|e| format!("the ledger {shown} is damaged at line {}: {e}", i + 1))?;
        }
        Ok(())
    }

    pub fn run(&self, id: &str) -> Option<&Run> {
        self.runs.get(id)
    }

    /// Every run, in the order reserved.
    pub fn runs(&self) -> impl Iterator<Item = (&String, &Run)> {
        self.order.iter().filter_map(|id| self.runs.get(id).map(|r| (id, r)))
    }

    /// The time of the last event, 0 for an empty ledger.
    pub fn last_time(&self) -> u64 {
        self.last_t
    }

    /// The ledger's notion of now: the clock, but never before the last event.
    pub fn now(&self) -> u64 {
        clock_ms().max(self.last_t)
    }

    pub fn poisoned(&self) -> bool {
        self.poisoned
    }

    /// The run holding `key`, if any, among runs not released.
    pub fn find_once(&self, key: &str) -> Option<(&String, &Run)> {
        self.runs().filter(|(_, r)| !r.released() && r.once.as_ref().is_some_and(|o| o.key == key)).last()
    }

    pub fn reserve(&mut self, id: &str, tool: &str, money: Option<Money>, window: Option<u64>, once: Option<OnceRef>) -> Result<(), String> {
        let t = self.stamp();
        let r = Run { tool: tool.to_string(), t, money, window, once, end: None };
        self.commit(reserve_event(id, &r))
    }

    pub fn settle(&mut self, id: &str, charged: &[Money], outcome: &str) -> Result<(), String> {
        let t = self.stamp();
        let charged: Vec<Json> = charged.iter().map(money_json).collect();
        self.commit(json!({ "e": "settle", "run": id, "t": t, "charged": charged, "outcome": outcome }))
    }

    pub fn release(&mut self, id: &str) -> Result<(), String> {
        let t = self.stamp();
        self.commit(json!({ "e": "release", "run": id, "t": t }))
    }

    fn stamp(&mut self) -> u64 {
        let now = clock_ms();
        self.clock_behind = now < self.last_t;
        now.max(self.last_t)
    }

    fn commit(&mut self, ev: Json) -> Result<(), String> {
        if self.poisoned {
            return Err("the ledger is unusable after an earlier write failure; restart the server".into());
        }
        // A second end for a run, or an end for no run, is a bug: refuse to record it.
        self.check(&ev)?;
        let mut line = ev.to_string();
        line.push('\n');
        let Some(file) = self.file.as_mut() else {
            return Err("the ledger was opened only to read".into());
        };
        if let Err(e) = file.write_all(line.as_bytes()).and_then(|()| file.sync_data()) {
            self.poisoned = true;
            return Err(format!("could not write the ledger {}: {e}", self.path.display()));
        }
        self.apply(&ev)?;
        self.writes += 1;
        if self.failpoint == Some(self.writes) {
            // Test-only crash injection (feature `failpoints`): die right after the write.
            std::process::abort();
        }
        Ok(())
    }

    /// Whether `ev` is a well-formed event that may follow the ledger as it stands.
    fn check(&self, ev: &Json) -> Result<(), String> {
        let id = ev["run"].as_str().filter(|s| is_run_id(s)).ok_or("an event without a valid run id")?;
        let t = ev["t"].as_u64().ok_or("an event without a time")?;
        if t < self.last_t {
            return Err(format!("an event at {t} follows one at {}: time went backwards", self.last_t));
        }
        let fields: Vec<&str> = ev.as_object().ok_or("an event that is not an object")?.keys().map(String::as_str).collect();
        let allow = |allowed: &[&str]| -> Result<(), String> {
            match fields.iter().find(|f| !allowed.contains(f)) {
                Some(f) => Err(format!("an event with an unknown field '{f}'")),
                None => Ok(()),
            }
        };
        match ev["e"].as_str() {
            Some("reserve") => {
                allow(&["e", "run", "tool", "t", "money", "window", "once"])?;
                if self.runs.contains_key(id) {
                    return Err(format!("run {id} is reserved twice"));
                }
                ev["tool"].as_str().filter(|s| !s.is_empty()).ok_or("a reserve without a tool")?;
                if let Some(m) = ev.get("money") {
                    money_from(m)?;
                }
                if ev.get("window").is_some_and(|w| !w.is_u64()) {
                    return Err("a reserve with a malformed window".into());
                }
                if let Some(o) = ev.get("once") {
                    let ok = o["key"].as_str().is_some_and(is_sha)
                        && o["args"].as_str().is_some_and(is_sha)
                        && (o["keep"].is_null() || o["keep"].is_u64())
                        && o.as_object().is_some_and(|m| m.keys().all(|k| ["key", "args", "keep"].contains(&k.as_str())));
                    if !ok {
                        return Err("a reserve with a malformed once key".into());
                    }
                }
            }
            Some(kind @ ("settle" | "release")) => {
                if kind == "settle" {
                    allow(&["e", "run", "t", "charged", "outcome"])?;
                    let list = ev["charged"].as_array().ok_or("a settle without \"charged\"")?;
                    for m in list {
                        money_from(m)?;
                    }
                    if !matches!(ev["outcome"].as_str(), Some("done" | "failed")) {
                        return Err("a settle whose outcome is not done or failed".into());
                    }
                } else {
                    allow(&["e", "run", "t"])?;
                }
                match self.runs.get(id) {
                    None => return Err(format!("a {kind} for run {id}, which was never reserved")),
                    Some(r) if r.end.is_some() => return Err(format!("a second end for run {id}")),
                    Some(_) => {}
                }
            }
            _ => return Err(format!("an event of unknown kind {}", ev["e"])),
        }
        Ok(())
    }

    /// Applies a checked event.
    fn apply(&mut self, ev: &Json) -> Result<(), String> {
        let id = ev["run"].as_str().unwrap_or_default().to_string();
        let t = ev["t"].as_u64().unwrap_or_default();
        match ev["e"].as_str() {
            Some("reserve") => {
                let money = ev.get("money").map(money_from).transpose()?;
                let once = ev.get("once").map(|o| OnceRef {
                    key: o["key"].as_str().unwrap_or_default().to_string(),
                    args: o["args"].as_str().unwrap_or_default().to_string(),
                    keep: o["keep"].as_u64(),
                });
                let tool = ev["tool"].as_str().unwrap_or_default().to_string();
                let window = ev["window"].as_u64();
                self.runs.insert(id.clone(), Run { tool, t, money, window, once, end: None });
                self.order.push(id);
            }
            Some("settle") => {
                let charged = ev["charged"].as_array().map(|a| a.iter().map(money_from).collect::<Result<Vec<_>, _>>()).transpose()?;
                let outcome = ev["outcome"].as_str().unwrap_or_default().to_string();
                if let Some(r) = self.runs.get_mut(&id) {
                    r.end = Some(End::Settled { t, charged: charged.unwrap_or_default(), outcome });
                }
            }
            Some("release") => {
                if let Some(r) = self.runs.get_mut(&id) {
                    r.end = Some(End::Released { t });
                }
            }
            _ => {}
        }
        self.last_t = self.last_t.max(t);
        Ok(())
    }

    /// Rewrites the ledger with only the runs `keep` keeps, through a temporary file
    /// renamed over it, so a crash leaves either the old ledger or the new one.
    pub fn compact(&mut self, keep: impl Fn(&str, &Run) -> bool) -> Result<usize, String> {
        if self.poisoned || self.file.is_none() {
            return Err("the ledger is unusable after an earlier write failure".into());
        }
        let kept: Vec<String> = self.order.iter().filter(|id| self.runs.get(*id).is_some_and(|r| keep(id, r))).cloned().collect();
        let dropped = self.order.len() - kept.len();
        if dropped == 0 {
            return Ok(0);
        }
        // Events in time order: each run's reserve, then its end, merged by time.
        let mut events: Vec<(u64, usize, Json)> = Vec::new();
        for (i, id) in kept.iter().enumerate() {
            let r = &self.runs[id];
            events.push((r.t, 2 * i, reserve_event(id, r)));
            match &r.end {
                Some(End::Settled { t, charged, outcome }) => {
                    let charged: Vec<Json> = charged.iter().map(money_json).collect();
                    events.push((*t, 2 * i + 1, json!({ "e": "settle", "run": id, "t": t, "charged": charged, "outcome": outcome })));
                }
                Some(End::Released { t }) => events.push((*t, 2 * i + 1, json!({ "e": "release", "run": id, "t": t }))),
                None => {}
            }
        }
        events.sort_by_key(|(t, i, _)| (*t, *i));
        // The last event's time is kept, so time still never goes backwards.
        let mut text = String::new();
        for (_, _, ev) in &events {
            text.push_str(&ev.to_string());
            text.push('\n');
        }
        let tmp = self.path.with_extension("ledger.tmp");
        let shown = tmp.display();
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp).map_err(|e| format!("cannot write {shown}: {e}"))?;
        f.write_all(text.as_bytes()).and_then(|()| f.sync_all()).map_err(|e| format!("cannot write {shown}: {e}"))?;
        drop(f);
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("cannot replace the ledger with {shown}: {e}"))?;
        sync_dir(&self.path)?;
        let mut reopened = Ledger::open(&self.path)?;
        // The compacted ledger may have lost the latest time; never let it go back.
        reopened.last_t = reopened.last_t.max(self.last_t);
        reopened.writes = self.writes;
        *self = reopened;
        Ok(dropped)
    }
}

fn reserve_event(id: &str, r: &Run) -> Json {
    let mut ev = json!({ "e": "reserve", "run": id, "tool": r.tool, "t": r.t });
    if let Some(m) = &r.money {
        ev["money"] = money_json(m);
    }
    if let Some(w) = r.window {
        ev["window"] = json!(w);
    }
    if let Some(o) = &r.once {
        ev["once"] = json!({ "key": o.key, "args": o.args, "keep": o.keep });
    }
    ev
}

fn sync_dir(path: &Path) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    File::open(dir).and_then(|d| d.sync_all()).map_err(|e| format!("cannot sync {}: {e}", dir.display()))
}

#[cfg(feature = "failpoints")]
fn failpoint() -> Option<u64> {
    std::env::var("STAALC_MCP_FAILPOINT").ok()?.strip_prefix("ledger_write:")?.parse().ok()
}

#[cfg(not(feature = "failpoints"))]
fn failpoint() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("staal-ledger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const A: &str = "0123456789abcdef0123456789abcdef";
    const B: &str = "fedcba9876543210fedcba9876543210";

    fn usd(a: &str) -> Money {
        Money::parse(a, Currency::parse("USD").unwrap()).unwrap()
    }

    #[test]
    fn events_survive_reopening_and_a_torn_line_is_cut() {
        let d = dir("reopen");
        let p = d.join("server.ledger");
        let mut l = Ledger::open(&p).unwrap();
        let once = OnceRef { key: "a".repeat(64), args: "b".repeat(64), keep: Some(60_000) };
        l.reserve(A, "refund", Some(usd("50.00")), Some(3_600_000), Some(once.clone())).unwrap();
        l.settle(A, &[usd("12.50")], "done").unwrap();
        l.reserve(B, "refund", None, None, None).unwrap();
        drop(l);
        // A crash in the middle of a write leaves part of a line.
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(br#"{"e":"release","run":"fedcba98"#).unwrap();
        drop(f);
        let l = Ledger::open(&p).unwrap();
        let a = l.run(A).unwrap();
        assert_eq!(a.once, Some(once));
        assert_eq!(a.window, Some(3_600_000));
        assert_eq!(a.settled().map(|(_, c, o)| (c.to_vec(), o.to_string())), Some((vec![usd("12.50")], "done".into())));
        assert_eq!(l.run(B).unwrap().end, None);
        assert!(std::fs::read_to_string(&p).unwrap().ends_with('\n'));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn damage_before_the_last_line_is_refused() {
        let d = dir("damage");
        let p = d.join("server.ledger");
        std::fs::write(&p, format!("not json\n{{\"e\":\"release\",\"run\":\"{A}\",\"t\":1}}\n")).unwrap();
        assert!(Ledger::open(&p).unwrap_err().contains("line 1"));
        std::fs::write(&p, format!("{{\"e\":\"release\",\"run\":\"{A}\",\"t\":1}}\n")).unwrap();
        assert!(Ledger::open(&p).unwrap_err().contains("never reserved"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_run_ends_once() {
        let d = dir("once");
        let mut l = Ledger::open(&d.join("server.ledger")).unwrap();
        l.reserve(A, "t", None, None, None).unwrap();
        assert!(l.reserve(A, "t", None, None, None).unwrap_err().contains("twice"));
        l.release(A).unwrap();
        assert!(l.settle(A, &[], "done").unwrap_err().contains("second end"));
        assert!(l.release(B).unwrap_err().contains("never reserved"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn time_never_goes_backwards() {
        let d = dir("time");
        let p = d.join("server.ledger");
        // A ledger written by a clock far ahead of this one.
        let future = clock_ms() + 10 * 86_400_000;
        std::fs::write(&p, format!("{{\"e\":\"reserve\",\"run\":\"{A}\",\"tool\":\"t\",\"t\":{future}}}\n")).unwrap();
        let mut l = Ledger::open(&p).unwrap();
        assert_eq!(l.now(), future);
        l.release(A).unwrap();
        assert!(l.clock_behind);
        assert_eq!(l.run(A).unwrap().end, Some(End::Released { t: future }));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn compaction_keeps_what_it_is_told_and_the_time() {
        let d = dir("compact");
        let p = d.join("server.ledger");
        let mut l = Ledger::open(&p).unwrap();
        l.reserve(A, "t", None, None, None).unwrap();
        l.release(A).unwrap();
        l.reserve(B, "t", Some(usd("5.00")), Some(60_000), None).unwrap();
        let last = l.last_time();
        assert_eq!(l.compact(|id, _| id == B).unwrap(), 1);
        assert!(l.run(A).is_none());
        assert_eq!(l.run(B).unwrap().money, Some(usd("5.00")));
        assert!(l.now() >= last);
        let reopened = Ledger::open(&p).unwrap();
        assert!(reopened.run(A).is_none() && reopened.run(B).is_some());
        assert!(!d.join("server.ledger.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
