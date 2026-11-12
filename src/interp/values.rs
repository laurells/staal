// Where a run's values come from (RUNTIME.md section 21): the secrets a config names with
// `{"env": "NAME"}`, the model provider's API key (`model.api_key`), and the proxy
// variables `"proxy": "environment"` reads. The command line reads them from the process's
// environment (`ProcessEnvironment`). A program that starts runs for others (Staal Studio)
// gives each run its own (`Given`): that run reads them and nothing else, never the
// process's environment, and no other run sees them.
//
// A value is held in a buffer that is overwritten when it is dropped (`Zeroizing`), and a
// run's values are dropped when the run ends, however it ends. What this does not reach:
// copies a library makes inside itself (an HTTP request's header buffers, TLS records, a
// database client's connection settings) and copies the allocator left when a buffer
// grew or moved are outside staalc's control. Clearing narrows how long a value stays in
// memory; it does not prove it has gone.

use std::collections::HashMap;
use zeroize::Zeroizing;

/// A value read for a run, overwritten when dropped.
pub type Value = Zeroizing<String>;

/// Where a run's values come from.
pub trait Values: Send + Sync {
    /// The value called `name`, or None.
    fn get(&self, name: &str) -> Option<Value>;
    /// What a value is called here, for messages: "environment variable", or "value given
    /// to this run".
    fn noun(&self) -> &'static str;
}

/// The process's environment: what the command line reads.
pub struct ProcessEnvironment;

impl Values for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<Value> {
        std::env::var(name).ok().map(Zeroizing::new)
    }
    fn noun(&self) -> &'static str {
        "environment variable"
    }
}

/// Values given to one run: the run reads these and nothing else.
pub struct Given {
    values: HashMap<String, Value>,
}

impl Given {
    /// The values, by name. Each is moved in, so the caller keeps no copy of it here.
    pub fn new(values: impl IntoIterator<Item = (String, Value)>) -> Given {
        Given { values: values.into_iter().collect() }
    }
}

impl Values for Given {
    fn get(&self, name: &str) -> Option<Value> {
        self.values.get(name).cloned()
    }
    fn noun(&self) -> &'static str {
        "value given to this run"
    }
}

impl std::fmt::Debug for Given {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<&String> = self.values.keys().collect();
        names.sort();
        write!(f, "Given({names:?})")
    }
}
