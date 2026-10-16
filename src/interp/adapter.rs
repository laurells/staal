// Action adapters (RUNTIME.md section 6). The runtime performs actions on the
// outside world only through this interface, and only after the capability
// broker has approved them.

use super::money::Money;
use super::value::Value;
use serde_json::Value as Json;

pub trait Adapter {
    /// The amount an action would spend, if it spends money. For a capability with a
    /// limit, an action whose amount cannot be determined is refused.
    fn amount(&self, capability: &str, method: &str, args: &[Value]) -> Option<Money>;
    /// Performs the action. Returns a receipt id on success, or an error message.
    fn perform(&mut self, capability: &str, method: &str, args: &[Json]) -> Result<String, String>;
}

/// An adapter that performs nothing and records what it would have done.
/// Its amount is the first Money value found in the arguments, searching depth first.
/// It fails an action when configured to: `{"fail": [{"cap": "email.send", "when_arg": "bad@x"}]}`
/// fails `email.send` whenever any argument (at any depth) is exactly that string.
pub struct SimulatedAdapter {
    fail_rules: Vec<(String, String)>,
    count: u64,
}

impl SimulatedAdapter {
    pub fn from_config(cfg: Option<&Json>) -> Result<SimulatedAdapter, String> {
        let mut fail_rules = Vec::new();
        if let Some(cfg) = cfg {
            let obj = cfg.as_object().ok_or("adapters: expected an object")?;
            for k in obj.keys() {
                if k != "simulated" {
                    return Err(format!("adapters: unknown key '{k}'; only \"simulated\" is available"));
                }
            }
            if let Some(sim) = obj.get("simulated") {
                let sim = sim.as_object().ok_or("adapters.simulated: expected an object")?;
                for k in sim.keys() {
                    if k != "fail" {
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
                        let when =
                            rule.get("when_arg").and_then(Json::as_str).ok_or(format!("{at}.when_arg: expected a string"))?;
                        fail_rules.push((cap.to_string(), when.to_string()));
                    }
                }
            }
        }
        Ok(SimulatedAdapter { fail_rules, count: 0 })
    }
}

fn first_money(v: &Value) -> Option<Money> {
    match v {
        Value::Money(m) => Some(*m),
        Value::Record { fields, .. } => fields.iter().find_map(|(_, f)| first_money(f)),
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

    fn perform(&mut self, capability: &str, method: &str, args: &[Json]) -> Result<String, String> {
        for (cap, when) in &self.fail_rules {
            if cap == capability && args.iter().any(|a| contains_text(a, when)) {
                return Err(format!("simulated failure of {capability}.{method}"));
            }
        }
        self.count += 1;
        Ok(format!("sim-{}", self.count))
    }
}
