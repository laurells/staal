// The crash states of a run, derived from the journal and world of one uninterrupted
// run (AGENT_MANIFEST.md section 16, R5).
//
// A journal is append-only and every record is flushed before the run goes on
// (RUNTIME.md section 11), so a process killed at any moment leaves a prefix of the
// journal it would have written. The world is what the simulated adapter performed:
// one line per action, written and flushed when the action happens. So the state a
// crash leaves is fixed by where it happens:
//
//   after_write:K    the first K journal records; in the world, the actions whose result
//                    is among them. (The record after an action's intent is its result,
//                    so an action whose intent is the K-th record has not happened.)
//   before_result:N  the records up to and including operation N's intent; in the world,
//                    the actions whose result is among them, and action N itself, which
//                    happened but whose result was never written.
//
// These are exactly the crash points the failpoints of `--features failpoints` inject,
// and tests/crash_test.rs (`derived_crash_states_are_the_states_a_crash_leaves`) checks,
// for the current build, that each derived state equals the one a real crash at that
// point leaves. That is what lets a released binary, which has no failpoints, supply
// the states: its uninterrupted run is enough (tests/journal_compat_test.rs).
//
// Only a run of one stream is accepted (no concurrent tasks, no operator records, no
// resumes): the derivation is exact only there.
#![allow(dead_code)]

use serde_json::Value as Json;
use std::collections::BTreeMap;

/// One state a crash can leave: its journal and its world, as file contents.
#[derive(Debug, Clone, PartialEq)]
pub struct CrashState {
    /// `after_write:K` or `before_result:N`.
    pub name: String,
    pub journal: String,
    pub world: String,
}

/// Lines of a file, each with its newline, exactly as written.
fn lines_of(text: &str) -> Result<Vec<&str>, String> {
    if !text.is_empty() && !text.ends_with('\n') {
        return Err("the file does not end with a newline: its last record is torn".into());
    }
    Ok(text.split_inclusive('\n').collect())
}

/// Every state a crash of the run that wrote `journal` and `world` can leave.
pub fn crash_states(journal: &str, world: &str) -> Result<Vec<CrashState>, String> {
    let jlines = lines_of(journal)?;
    let wlines = lines_of(world)?;
    let records: Vec<Json> =
        jlines.iter().enumerate().map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("journal line {}: {e}", i + 1))).collect::<Result<_, _>>()?;
    let (Some(first), Some(last)) = (records.first(), records.last()) else { return Err("the journal is empty".into()) };
    if first["j"] != "start" || last["j"] != "end" {
        return Err("the journal is not of an uninterrupted run: it must start with a start record and end with an end record".into());
    }
    // Each operation: the line of its intent, and of its result.
    let mut intents: BTreeMap<u64, usize> = BTreeMap::new();
    let mut results: BTreeMap<u64, usize> = BTreeMap::new();
    let mut keys: BTreeMap<String, u64> = BTreeMap::new();
    for (i, r) in records.iter().enumerate().skip(1).take(records.len() - 2) {
        if r.get("t").is_some() {
            return Err(format!("journal line {}: a concurrent task's record; only one stream is accepted", i + 1));
        }
        let n = r["n"].as_u64().ok_or(format!("journal line {}: no operation number", i + 1))?;
        match r["j"].as_str() {
            Some("intent") => {
                if n != intents.len() as u64 + 1 || intents.len() != results.len() {
                    return Err(format!("journal line {}: intent {n} out of order", i + 1));
                }
                intents.insert(n, i);
                if r["op"] == "action" {
                    let key = r["key"].as_str().ok_or(format!("journal line {}: an action without a key", i + 1))?;
                    keys.insert(key.to_string(), n);
                }
            }
            Some("result") => {
                if intents.get(&n) != Some(&(i - 1)) {
                    return Err(format!("journal line {}: result {n} does not follow its intent", i + 1));
                }
                results.insert(n, i);
            }
            other => return Err(format!("journal line {}: record {other:?}; only intents and results are accepted", i + 1)),
        }
    }
    // Each world line: the operation that performed it.
    let mut performed: Vec<(u64, &str)> = Vec::new();
    for (i, l) in wlines.iter().enumerate() {
        let e: Json = serde_json::from_str(l).map_err(|e| format!("world line {}: {e}", i + 1))?;
        let key = e["key"].as_str().ok_or(format!("world line {}: no key", i + 1))?;
        let n = *keys.get(key).ok_or(format!("world line {}: key {key} is no action of the journal", i + 1))?;
        if performed.iter().any(|(m, _)| *m == n) {
            return Err(format!("world line {}: action {n} was performed twice", i + 1));
        }
        performed.push((n, l));
    }
    let world_with = |done: &dyn Fn(u64) -> bool| performed.iter().filter(|(n, _)| done(*n)).map(|(_, l)| *l).collect::<String>();
    let mut out = Vec::new();
    for k in 1..=jlines.len() {
        // The results among the first k lines.
        let done = |n: u64| results.get(&n).is_some_and(|&i| i < k);
        out.push(CrashState { name: format!("after_write:{k}"), journal: jlines[..k].concat(), world: world_with(&done) });
    }
    for (&n, &i) in &intents {
        let done = |m: u64| m == n || results.get(&m).is_some_and(|&r| r <= i);
        out.push(CrashState { name: format!("before_result:{n}"), journal: jlines[..=i].concat(), world: world_with(&done) });
    }
    Ok(out)
}
