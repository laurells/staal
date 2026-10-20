// Action adapters (RUNTIME.md sections 6 and 11). The runtime performs actions on
// the outside world only through this interface, and only after the capability
// broker has approved them.

use super::money::Money;
use super::value::Value;
use serde_json::{json, Value as Json};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;

pub trait Adapter {
    /// The amount an action would spend, if it spends money. Must be a pure function
    /// of the arguments: a resumed run computes it again and must get the same answer.
    /// For a capability with a limit, an action whose amount cannot be determined is refused.
    fn amount(&self, capability: &str, method: &str, args: &[Value]) -> Option<Money>;
    /// What an action acts on (a phone number, a door, a file path), checked against
    /// a capability's `only` limit. Like `amount`, a pure function of the arguments.
    /// For a capability with `only`, an action whose target cannot be determined is refused.
    fn target(&self, capability: &str, method: &str, args: &[Value]) -> Option<String>;
    /// Whether the service deduplicates this action by idempotency key. If true, the
    /// adapter promises that performing it again with a key it has already seen does
    /// not perform it again and returns the original receipt. Only then may a resumed
    /// run retry an action that was left in doubt by a crash.
    fn idempotent(&self, capability: &str, method: &str) -> bool;
    /// Performs the action. `key` is unique to this run and this operation. Returns a
    /// receipt id on success, or an error message.
    fn perform(&mut self, key: &str, capability: &str, method: &str, args: &[Json]) -> Result<String, String>;
}

/// An adapter that performs nothing real. It records each action it "performs" in a
/// world: in memory, or in a file that survives a crash, so tests can check exactly
/// what happened outside the run.
///
/// Config: `{"simulated": {"fail": [...], "world": "path", "idempotent": true}}`
///  - `fail`: `[{"cap": "email.send", "when_arg": "bad@x"}]` fails `email.send` whenever
///    any argument (at any depth) is exactly that string.
///  - `world`: a JSON-lines file of performed actions, kept across runs.
///  - `idempotent` (default true): whether a repeated key returns the original receipt.
///    With false, the adapter behaves like a service without deduplication: a repeated
///    key performs the action again, which a correct runtime must never cause.
///
/// The amount of an action is the first Money value found in its arguments, depth first;
/// its target is the first Text value found the same way.
pub struct SimulatedAdapter {
    fail_rules: Vec<(String, String)>,
    idempotent: bool,
    world: Vec<Json>,
    world_file: Option<File>,
}

impl SimulatedAdapter {
    pub fn from_config(cfg: Option<&Json>) -> Result<SimulatedAdapter, String> {
        let mut adapter = SimulatedAdapter { fail_rules: Vec::new(), idempotent: true, world: Vec::new(), world_file: None };
        let Some(cfg) = cfg else { return Ok(adapter) };
        let obj = cfg.as_object().ok_or("adapters: expected an object")?;
        for k in obj.keys() {
            if k != "simulated" {
                return Err(format!("adapters: unknown key '{k}'; only \"simulated\" is available"));
            }
        }
        let Some(sim) = obj.get("simulated") else { return Ok(adapter) };
        let sim = sim.as_object().ok_or("adapters.simulated: expected an object")?;
        for k in sim.keys() {
            if !["fail", "world", "idempotent"].contains(&k.as_str()) {
                return Err(format!("adapters.simulated: unknown key '{k}'"));
            }
        }
        if let Some(list) = sim.get("fail") {
            let list = list.as_array().ok_or("adapters.simulated.fail: expected a list")?;
            for (i, rule) in list.iter().enumerate() {
                let at = format!("adapters.simulated.fail[{i}]");
                let rule = rule.as_object().ok_or(format!("{at}: expected an object"))?;
                for k in rule.keys() {
                    if k != "cap" && k != "when_arg" {
                        return Err(format!("{at}: unknown key '{k}'"));
                    }
                }
                let cap = rule.get("cap").and_then(Json::as_str).ok_or(format!("{at}.cap: expected a string"))?;
                let when = rule.get("when_arg").and_then(Json::as_str).ok_or(format!("{at}.when_arg: expected a string"))?;
                adapter.fail_rules.push((cap.to_string(), when.to_string()));
            }
        }
        if let Some(v) = sim.get("idempotent") {
            adapter.idempotent = v.as_bool().ok_or("adapters.simulated.idempotent: expected true or false")?;
        }
        if let Some(path) = sim.get("world") {
            let path = PathBuf::from(path.as_str().ok_or("adapters.simulated.world: expected a file path")?);
            let mut file = OpenOptions::new()
                .read(true)
                .append(true)
                .create(true)
                .open(&path)
                .map_err(|e| format!("adapters.simulated.world: cannot open {}: {e}", path.display()))?;
            let mut text = String::new();
            file.read_to_string(&mut text).map_err(|e| format!("adapters.simulated.world: cannot read {}: {e}", path.display()))?;
            // Every line is written whole and flushed before an action reports success,
            // so a torn line belongs to an action that never reported success: ignore it.
            for line in text.lines() {
                if let Ok(entry) = serde_json::from_str::<Json>(line) {
                    adapter.world.push(entry);
                }
            }
            adapter.world_file = Some(file);
        }
        Ok(adapter)
    }

    /// Everything performed in the world so far (for tests).
    pub fn world(&self) -> &[Json] {
        &self.world
    }
}

fn first_money(v: &Value) -> Option<Money> {
    match v {
        Value::Money(m) => Some(*m),
        Value::Record { fields, .. } | Value::Enum { fields, .. } => fields.iter().find_map(|(_, f)| first_money(f)),
        Value::List(items) => items.iter().find_map(first_money),
        // Inside `some(...)` as anywhere else, so an optional amount is never skipped
        // for a later, smaller one.
        Value::Opt(Some(inner)) => first_money(inner),
        _ => None,
    }
}

fn first_text(v: &Value) -> Option<String> {
    match v {
        Value::Text(t) => Some(t.clone()),
        Value::Record { fields, .. } | Value::Enum { fields, .. } => fields.iter().find_map(|(_, f)| first_text(f)),
        Value::List(items) => items.iter().find_map(first_text),
        Value::Opt(Some(inner)) => first_text(inner),
        _ => None,
    }
}

fn contains_text(j: &Json, needle: &str) -> bool {
    match j {
        Json::String(s) => s == needle,
        Json::Array(a) => a.iter().any(|x| contains_text(x, needle)),
        Json::Object(o) => o.values().any(|x| contains_text(x, needle)),
        _ => false,
    }
}

impl Adapter for SimulatedAdapter {
    fn amount(&self, _capability: &str, _method: &str, args: &[Value]) -> Option<Money> {
        args.iter().find_map(first_money)
    }

    fn target(&self, _capability: &str, _method: &str, args: &[Value]) -> Option<String> {
        args.iter().find_map(first_text)
    }

    fn idempotent(&self, _capability: &str, _method: &str) -> bool {
        self.idempotent
    }

    fn perform(&mut self, key: &str, capability: &str, method: &str, args: &[Json]) -> Result<String, String> {
        for (cap, when) in &self.fail_rules {
            if cap == capability && args.iter().any(|a| contains_text(a, when)) {
                return Err(format!("simulated failure of {capability}.{method}"));
            }
        }
        if self.idempotent
            && let Some(seen) = self.world.iter().find(|e| e["key"] == key) {
                return seen["receipt"].as_str().map(str::to_string).ok_or_else(|| "world entry has no receipt".into());
            }
        let receipt = format!("sim-{}", self.world.len() + 1);
        let entry = json!({ "key": key, "capability": capability, "method": method, "args": args, "receipt": receipt });
        if let Some(f) = &mut self.world_file {
            let mut line = entry.to_string();
            line.push('\n');
            f.write_all(line.as_bytes()).and_then(|()| f.sync_data()).map_err(|e| format!("simulated world: {e}"))?;
        }
        self.world.push(entry);
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idempotent_service_returns_the_original_receipt_for_a_repeated_key() {
        let mut a = SimulatedAdapter::from_config(None).unwrap();
        let r1 = a.perform("run-1", "email.send", "send", &[json!("x")]).unwrap();
        let r2 = a.perform("run-1", "email.send", "send", &[json!("x")]).unwrap();
        assert_eq!(r1, r2);
        assert_eq!(a.world().len(), 1, "performed once");
        a.perform("run-2", "email.send", "send", &[json!("x")]).unwrap();
        assert_eq!(a.world().len(), 2);
    }

    #[test]
    fn a_service_without_deduplication_performs_again() {
        let cfg = json!({ "simulated": { "idempotent": false } });
        let mut a = SimulatedAdapter::from_config(Some(&cfg)).unwrap();
        assert!(!a.idempotent("email.send", "send"));
        a.perform("run-1", "email.send", "send", &[]).unwrap();
        a.perform("run-1", "email.send", "send", &[]).unwrap();
        assert_eq!(a.world().len(), 2, "a duplicate: the runtime must never cause this");
    }

    #[test]
    fn the_world_file_survives_reopening() {
        let p = std::env::temp_dir().join(format!("staalc-world-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let cfg = json!({ "simulated": { "world": p.to_str().unwrap() } });
        let mut a = SimulatedAdapter::from_config(Some(&cfg)).unwrap();
        let r1 = a.perform("run-1", "payments.refund", "refund", &[]).unwrap();
        drop(a);
        let mut b = SimulatedAdapter::from_config(Some(&cfg)).unwrap();
        assert_eq!(b.world().len(), 1);
        assert_eq!(b.perform("run-1", "payments.refund", "refund", &[]).unwrap(), r1);
        assert_eq!(b.perform("run-2", "payments.refund", "refund", &[]).unwrap(), "sim-2");
        let _ = std::fs::remove_file(&p);
    }
}
