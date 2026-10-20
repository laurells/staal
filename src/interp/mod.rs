// The reference interpreter. It implements docs/RUNTIME.md directly and is the
// oracle any other backend is tested against.
//
// Rules for this module:
//  - It never panics. A state the compiler should have made impossible becomes a
//    failure of kind `internal`, not a crash.
//  - It re-checks at runtime what matters for safety (capabilities never sent to
//    models, runtime-only values never accepted from outside), so a bug in the
//    compiler is caught here rather than trusted.

pub mod adapter;
pub mod broker;
pub mod fsx;
pub mod http;
pub mod journal;
pub mod money;
pub mod anthropic;
pub mod provider;
pub mod value;

use crate::ast::*;
use adapter::{Adapter, SimulatedAdapter};
use broker::Broker;
use journal::{Journal, Step};
use money::{Currency, Money};
use provider::{ModelRequest, Provider, ProviderError};
use serde_json::{json, Map, Value as Json};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use value::{from_json, from_json_lenient, key_order, schema, to_json, HeaderValue, Receipt, Secret, Types, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    Action,
    Capability,
    Validation,
    /// A value error: arithmetic overflow, division by zero, money in different
    /// currencies or below zero, an index out of range, text too long.
    Value,
    Budget,
    Provider,
    StepLimit,
    DepthLimit,
    /// A state the compiler should have ruled out: a bug, reported instead of crashing.
    Internal,
    /// The journal could not be written, or replay diverged from it (RUNTIME.md section 11).
    Journal,
    /// An action that is not idempotent may or may not have happened before a crash.
    InDoubt,
}

impl FailKind {
    /// Whether `try` may turn this failure into an Outcome (RUNTIME.md section 8).
    ///
    /// Deployment limits are safeguards, not part of a program's meaning: if a program
    /// could catch them it could observe them, and a runaway loop or recursion would
    /// end in a "successful" run. An internal failure is a bug in staalc, which no
    /// program may hide. Journal and in-doubt failures mean the run cannot safely
    /// continue at all. All of these end the run.
    pub fn catchable(self) -> bool {
        match self {
            FailKind::Action
            | FailKind::Capability
            | FailKind::Validation
            | FailKind::Value
            | FailKind::Budget
            | FailKind::Provider => true,
            FailKind::StepLimit | FailKind::DepthLimit | FailKind::Internal | FailKind::Journal | FailKind::InDoubt => {
                false
            }
        }
    }
}

impl fmt::Display for FailKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FailKind::Action => "action",
            FailKind::Capability => "capability",
            FailKind::Validation => "validation",
            FailKind::Value => "value",
            FailKind::Budget => "budget",
            FailKind::Provider => "provider",
            FailKind::StepLimit => "step_limit",
            FailKind::DepthLimit => "depth_limit",
            FailKind::Internal => "internal",
            FailKind::Journal => "journal",
            FailKind::InDoubt => "in_doubt",
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub kind: FailKind,
    pub message: String,
}

fn fail<T>(kind: FailKind, message: impl Into<String>) -> Result<T, Failure> {
    Err(Failure { kind, message: message.into() })
}

fn internal<T>(message: impl Into<String>) -> Result<T, Failure> {
    fail(FailKind::Internal, message)
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_steps: u64,
    pub max_depth: usize,
    pub default_max_tokens: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_steps: 1_000_000, max_depth: 256, default_max_tokens: 1024 }
    }
}

/// The longest text a program can build. Without a ceiling, text doubled through
/// recursion could exhaust memory long before any step limit is reached.
pub const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

/// The hard ceiling on `max_depth`: deeper nesting could exhaust the interpreter's stack.
pub const MAX_DEPTH_CEILING: usize = 2_000;
/// Stack for the interpreter thread, sized so MAX_DEPTH_CEILING nested calls fit.
const INTERP_STACK_BYTES: usize = 512 * 1024 * 1024;

/// The outcome of a run: the entry function's result or a failure, and the log.
#[derive(Debug)]
pub struct RunResult {
    pub result: Result<Json, Failure>,
    pub log: Vec<Json>,
}

/// Which engine runs the program. Both give the same results, logs and journals: a
/// run may crash on one and resume on the other (RUNTIME.md section 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    /// Every function on the interpreter, the reference.
    #[default]
    Interpreter,
    /// Pure Int and Bool functions as native code, the rest on the interpreter.
    Native,
}

/// Whether a run keeps a journal (RUNTIME.md section 11).
pub enum Durability<'a> {
    /// No journal: the run cannot be resumed if it is killed.
    None,
    /// A new run with a new journal at this path, which must not exist.
    Create(&'a Path),
    /// Continue the run recorded in this journal.
    Resume(&'a Path),
}

/// Runs a checked program with the providers and adapters named in its config.
/// Returns Err for a bad config; nothing runs in that case.
pub fn run_config(prog: &Program, config: &Json) -> Result<RunResult, String> {
    run_config_durable(prog, "", config, Durability::None)
}

/// Like `run_config`, with a journal. `source` is the program's text: a journal is
/// bound to the exact program and config it was started with.
pub fn run_config_durable(prog: &Program, source: &str, config: &Json, durability: Durability) -> Result<RunResult, String> {
    run_config_engine(prog, source, config, durability, Engine::Interpreter)
}

/// Like `run_config_durable`, on the given engine.
pub fn run_config_engine(
    prog: &Program,
    source: &str,
    config: &Json,
    durability: Durability,
    engine: Engine,
) -> Result<RunResult, String> {
    let cfg = config.as_object().ok_or("run config: expected a JSON object")?;
    let model = cfg.get("model").ok_or("run config: missing \"model\"")?;
    let provider = provider::from_config(model)?;
    let adapter = SimulatedAdapter::from_config(cfg.get("adapters"))?;
    run_engine(prog, source, config, durability, provider, Box::new(adapter), engine)
}

/// Runs a checked program with the given provider and adapter, without a journal.
pub fn run_with(
    prog: &Program,
    config: &Json,
    provider: Box<dyn Provider + Send>,
    adapter: Box<dyn Adapter + Send>,
) -> Result<RunResult, String> {
    run_durable(prog, "", config, Durability::None, provider, adapter)
}

/// Runs a checked program with the given provider, adapter and durability.
pub fn run_durable(
    prog: &Program,
    source: &str,
    config: &Json,
    durability: Durability,
    provider: Box<dyn Provider + Send>,
    adapter: Box<dyn Adapter + Send>,
) -> Result<RunResult, String> {
    run_engine(prog, source, config, durability, provider, adapter, Engine::Interpreter)
}

/// Runs a checked program with the given provider, adapter, durability and engine.
pub fn run_engine(
    prog: &Program,
    source: &str,
    config: &Json,
    durability: Durability,
    provider: Box<dyn Provider + Send>,
    adapter: Box<dyn Adapter + Send>,
    engine: Engine,
) -> Result<RunResult, String> {
    let native = match engine {
        Engine::Interpreter => None,
        Engine::Native => Some(compile_native(prog)?),
    };
    run_impl(prog, source, config, durability, provider, adapter, native)
}

/// Runs a checked program with native code made before the run: the code linked into
/// an executable by `staalc build`.
#[cfg(feature = "native")]
pub fn run_config_with_native(
    prog: &Program,
    source: &str,
    config: &Json,
    durability: Durability,
    native: crate::native::Native,
) -> Result<RunResult, String> {
    let cfg = config.as_object().ok_or("run config: expected a JSON object")?;
    let model = cfg.get("model").ok_or("run config: missing \"model\"")?;
    let provider = provider::from_config(model)?;
    let adapter = SimulatedAdapter::from_config(cfg.get("adapters"))?;
    run_impl(prog, source, config, durability, provider, Box::new(adapter), Some(native))
}

fn run_impl(
    prog: &Program,
    source: &str,
    config: &Json,
    durability: Durability,
    provider: Box<dyn Provider + Send>,
    adapter: Box<dyn Adapter + Send>,
    native: Option<NativeCode>,
) -> Result<RunResult, String> {
    // Validate everything first: a bad config must not even create a journal.
    let setup = Setup::from_config(prog, config)?;
    let program_sha = journal::sha256_hex(source.as_bytes());
    let config_sha = journal::sha256_hex(config.to_string().as_bytes());
    let journal = match durability {
        Durability::None => Journal::memory()?,
        Durability::Create(path) => Journal::create(path, &program_sha, &config_sha)?,
        Durability::Resume(path) => Journal::resume(path, &program_sha, &config_sha)?,
    };
    // The interpreter recurses once per nested call, so it runs on a thread with a
    // stack large enough for the depth ceiling, whatever thread called us.
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(INTERP_STACK_BYTES)
            .spawn_scoped(s, move || {
                let mut it = Interp::new(prog, setup.limits, &setup.runtime, provider, adapter, journal);
                it.native = native;
                it.run(&setup)
            })
            .map_err(|e| format!("could not start the interpreter thread: {e}"))?
            .join()
            .map_err(|_| "the interpreter thread crashed; this is a bug in staalc".to_string())
    })
}

#[cfg(feature = "native")]
type NativeCode = crate::native::Native;
#[cfg(not(feature = "native"))]
type NativeCode = std::convert::Infallible;

#[cfg(feature = "native")]
fn compile_native(prog: &Program) -> Result<NativeCode, String> {
    crate::native::Native::compile(prog)
}

#[cfg(not(feature = "native"))]
fn compile_native(_: &Program) -> Result<NativeCode, String> {
    Err("this staalc was built without native code (the `native` feature)".into())
}

// ---------------------------------------------------------------------------
// Run config (RUNTIME.md section 1)

struct Setup {
    entry: String,
    /// Entry arguments in parameter order: data values, or grants for capabilities.
    args: Vec<Arg>,
    limits: Limits,
    runtime: RuntimeSettings,
}

/// Settings for the runtime's own capabilities (RUNTIME.md section 12).
struct RuntimeSettings {
    net: http::Settings,
    fs_max_read_bytes: usize,
    /// Every secret value of the run, removed from anything recorded or shown.
    secrets: Vec<String>,
}

enum Arg {
    Data(Value),
    Grant { path: String, limits: broker::Limits, idempotency_key: bool, needs: Vec<NeedArg> },
}

/// What a library capability's grant supplies for one of its `needs`.
enum NeedArg {
    /// A runtime capability, granted by the operator and then narrowed to the limits
    /// the library declared for it.
    Cap { path: String, limits: broker::Limits, idempotency_key: bool, declared: CapLimits },
    /// A secret or a text setting.
    Data(Value),
}

struct ParsedGrant {
    limits: broker::Limits,
    idempotency_key: bool,
    needs: Vec<NeedArg>,
}

/// How a net.http failure that certainly sent nothing begins, in results and journals.
const NOT_SENT: &str = "not sent: ";

/// Reads `{"env": "VARIABLE"}`: a secret from the environment (RUNTIME.md section 12).
fn load_secret(spec: &Json, at: &str) -> Result<Secret, String> {
    let spec = spec.as_object().ok_or(format!("{at}: expected {{\"env\": \"VARIABLE_NAME\"}}"))?;
    for k in spec.keys() {
        if k != "env" {
            return Err(format!("{at}: unknown key '{k}'; secrets come only from the environment"));
        }
    }
    let var = spec.get("env").and_then(Json::as_str).ok_or(format!("{at}.env: expected a variable name"))?;
    let value = std::env::var(var).map_err(|_| format!("{at}: environment variable {var} is not set"))?;
    if value.len() < MIN_SECRET_BYTES {
        return Err(format!("{at}: {var} is shorter than {MIN_SECRET_BYTES} characters, too short to keep out of logs reliably"));
    }
    // Printable ASCII without quotes, backslashes or spaces: then filling it into a JSON
    // string or a header can never change their structure.
    if !value.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'"' && b != b'\\') {
        return Err(format!(
            "{at}: {var} contains a space, quote, backslash or non-ASCII character; secrets must be printable ASCII without those"
        ));
    }
    Ok(Secret { name: var.to_string(), value, shown: "{secret}".to_string() })
}

/// Reads one grant: `{"cap": path, limits..., "idempotency_key"?, "needs"?}`, for a
/// capability slot of type `cap <want>(declared)`. A library capability's grant must
/// supply each of its `needs` (RUNTIME.md section 13); secret values are added to
/// `secrets` so they can be kept out of everything recorded.
fn parse_grant(
    g: &Json,
    at: &str,
    want: &str,
    declared: &CapLimits,
    types: &Types,
    prog: &Program,
    secrets: &mut Vec<String>,
) -> Result<ParsedGrant, String> {
    let g = g.as_object().ok_or(format!("{at}: expected an object"))?;
    for k in g.keys() {
        if !["cap", "max", "each", "uses", "only", "idempotency_key", "needs"].contains(&k.as_str()) {
            return Err(format!("{at}: unknown key '{k}'"));
        }
    }
    let gpath = g.get("cap").and_then(Json::as_str).ok_or(format!("{at}.cap: expected a string"))?;
    if gpath != want {
        return Err(format!("{at}.cap: is '{gpath}', but the slot needs '{want}'"));
    }
    let limits = grant_limits(g, at, types)?;
    let idempotency_key = match g.get("idempotency_key") {
        None => false,
        Some(Json::Bool(b)) => *b,
        Some(_) => return Err(format!("{at}.idempotency_key: expected true or false")),
    };
    if is_runtime_cap(want) {
        if limits.max.is_some() || limits.each.is_some() {
            return Err(format!("{at}: `{want}` spends no money the runtime can measure; max and each do not apply"));
        }
        let http = want.starts_with("net.http.");
        for (i, pat) in limits.only.iter().flatten().enumerate() {
            let checked = if http { crate::targets::check_url_pattern(pat) } else { crate::targets::check_path_pattern(pat) };
            checked.map_err(|e| format!("{at}.only[{i}]: {e}"))?;
        }
        if idempotency_key && (!http || want == "net.http.get") {
            return Err(format!("{at}.idempotency_key: only for net.http.post, put, patch and delete"));
        }
    } else if idempotency_key {
        return Err(format!("{at}.idempotency_key: only for net.http.post, put, patch and delete"));
    }
    // The slot's own limits narrow the grant when the run starts. Money limits in
    // different currencies could never both be met: refuse now.
    let d = runtime_limits(declared)?;
    for (name, granted, wanted) in [("max", limits.max, d.max), ("each", limits.each, d.each)] {
        if let (Some(gl), Some(dl)) = (granted, wanted)
            && gl.currency != dl.currency
        {
            return Err(format!("{at}.{name}: granted in {}, but the slot's limit is in {}", gl.currency.code, dl.currency.code));
        }
    }
    let lib = prog.caps.iter().find(|c| c.actions.iter().any(|a| c.action_path(&a.name) == want));
    let mut needs = Vec::new();
    match (lib, g.get("needs")) {
        (None, None) => {}
        (None, Some(_)) => return Err(format!("{at}.needs: `{want}` is not a library capability, so it needs nothing")),
        (Some(c), None) if c.needs.is_empty() => {}
        (Some(c), None) => {
            let names: Vec<&str> = c.needs.iter().map(|(n, _, _)| n.as_str()).collect();
            return Err(format!("{at}.needs: missing; `{want}` needs {}", names.join(", ")));
        }
        (Some(c), Some(spec)) => {
            let spec = spec.as_object().ok_or(format!("{at}.needs: expected an object"))?;
            for k in spec.keys() {
                if !c.needs.iter().any(|(n, _, _)| n == k) {
                    return Err(format!("{at}.needs: `{want}` needs no '{k}'"));
                }
            }
            for (n, t, _) in &c.needs {
                let nat = format!("{at}.needs.{n}");
                let v = spec.get(n).ok_or(format!("{nat}: missing"))?;
                match t {
                    Ty::Cap { path, limits: nd } => {
                        let np = path.join(".");
                        let inner = parse_grant(v, &nat, &np, nd, types, prog, secrets)?;
                        needs.push(NeedArg::Cap {
                            path: np,
                            limits: inner.limits,
                            idempotency_key: inner.idempotency_key,
                            declared: nd.clone(),
                        });
                    }
                    Ty::Named(x) if x == "Secret" => {
                        let secret = load_secret(v, &nat)?;
                        secrets.push(secret.value.clone());
                        needs.push(NeedArg::Data(Value::Secret(secret)));
                    }
                    Ty::Named(x) if x == "Text" => {
                        let text = v.as_str().ok_or(format!("{nat}: expected a string"))?;
                        needs.push(NeedArg::Data(Value::Text(text.to_string())));
                    }
                    other => return Err(format!("{nat}: a {other} cannot be supplied; the compiler should have refused it")),
                }
            }
        }
    }
    Ok(ParsedGrant { limits, idempotency_key, needs })
}

/// The shortest secret accepted: a shorter one would be found by accident in ordinary
/// text, and removing it from responses and logs would damage them.
const MIN_SECRET_BYTES: usize = 8;

/// Whether `path` is one of the runtime's own capabilities (`net.http.*`, `fs.*`).
fn is_runtime_cap(path: &str) -> bool {
    path.starts_with("net.") || path.starts_with("fs.")
}

/// Replaces every secret value in `text` with a marker.
fn scrub_text(secrets: &[String], text: &str) -> String {
    let mut out = text.to_string();
    for s in secrets {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), "[secret removed]");
        }
    }
    out
}

fn scrub_json(secrets: &[String], j: &mut Json) {
    match j {
        Json::String(t) => {
            if secrets.iter().any(|s| t.contains(s.as_str())) {
                *t = scrub_text(secrets, t);
            }
        }
        Json::Array(a) => a.iter_mut().for_each(|x| scrub_json(secrets, x)),
        Json::Object(o) => o.values_mut().for_each(|x| scrub_json(secrets, x)),
        _ => {}
    }
}

fn positive(v: &Json, at: &str) -> Result<u64, String> {
    v.as_u64().filter(|n| *n > 0).ok_or(format!("{at}: expected a positive whole number"))
}

/// `"net": {"timeout_ms", "connect_timeout_ms", "max_response_bytes", "proxy"}` and
/// `"fs": {"max_read_bytes"}`.
fn runtime_settings(cfg: &Map<String, Json>) -> Result<RuntimeSettings, String> {
    let mut net = http::Settings::default();
    if let Some(n) = cfg.get("net") {
        let n = n.as_object().ok_or("net: expected an object")?;
        for (k, v) in n {
            let at = format!("net.{k}");
            match k.as_str() {
                "timeout_ms" => net.timeout = std::time::Duration::from_millis(positive(v, &at)?),
                "connect_timeout_ms" => net.connect_timeout = std::time::Duration::from_millis(positive(v, &at)?),
                "max_response_bytes" => {
                    let b = positive(v, &at)?;
                    if b > MAX_TEXT_BYTES as u64 {
                        return Err(format!("{at}: at most {MAX_TEXT_BYTES}"));
                    }
                    net.max_response_bytes = b;
                }
                "extra_root_certificates" => {
                    let list = v.as_array().ok_or(format!("{at}: expected a list of PEM file paths"))?;
                    for (i, p) in list.iter().enumerate() {
                        let p = p.as_str().ok_or(format!("{at}[{i}]: expected a file path"))?;
                        net.extra_roots.extend(http::read_root_certificates(p).map_err(|e| format!("{at}[{i}]: {e}"))?);
                    }
                }
                "proxy" => {
                    net.proxy_from_env = match v.as_str() {
                        Some("none") => false,
                        Some("environment") => true,
                        _ => return Err(format!("{at}: expected \"none\" or \"environment\"")),
                    }
                }
                _ => return Err(format!("net: unknown key '{k}'")),
            }
        }
    }
    let mut fs_max_read_bytes = MAX_TEXT_BYTES;
    if let Some(f) = cfg.get("fs") {
        let f = f.as_object().ok_or("fs: expected an object")?;
        for (k, v) in f {
            match k.as_str() {
                "max_read_bytes" => {
                    let b = positive(v, "fs.max_read_bytes")?;
                    if b > MAX_TEXT_BYTES as u64 {
                        return Err(format!("fs.max_read_bytes: at most {MAX_TEXT_BYTES}"));
                    }
                    fs_max_read_bytes = b as usize;
                }
                _ => return Err(format!("fs: unknown key '{k}'")),
            }
        }
    }
    Ok(RuntimeSettings { net, fs_max_read_bytes, secrets: Vec::new() })
}

/// Converts a limit written in source (`max: 500 USD`) to money.
fn source_limit(name: &str, l: &Limit) -> Result<Money, String> {
    let unit = l.unit.as_deref().ok_or_else(|| {
        format!(
            "capability limit '{name}: {}' has no currency, so it cannot be enforced; write e.g. '{name}: {} USD'",
            l.amount, l.amount
        )
    })?;
    let currency = Currency::parse(unit)?;
    Money::from_major(l.amount, currency)
}

/// The broker's form of limits written in source.
fn runtime_limits(l: &CapLimits) -> Result<broker::Limits, String> {
    Ok(broker::Limits {
        max: l.max.as_ref().map(|m| source_limit("max", m)).transpose()?,
        each: l.each.as_ref().map(|e| source_limit("each", e)).transpose()?,
        uses: l.uses,
        only: l.only.clone(),
    })
}

/// Reads the limits of a grant in the run config: `{"cap": ..., "max": money,
/// "each": money, "uses": n, "only": ["pattern", ...]}`.
fn grant_limits(g: &Map<String, Json>, at: &str, types: &Types) -> Result<broker::Limits, String> {
    let money = |key: &str| -> Result<Option<Money>, String> {
        match g.get(key) {
            None => Ok(None),
            Some(m) => match from_json(&Ty::Named("Money".into()), m, types, &format!("{at}.{key}"))? {
                Value::Money(m) => Ok(Some(m)),
                _ => Err(format!("{at}.{key}: expected money")),
            },
        }
    };
    let uses = match g.get("uses") {
        None => None,
        Some(v) => Some(v.as_u64().ok_or(format!("{at}.uses: expected a whole number"))?),
    };
    let only = match g.get("only") {
        None => None,
        Some(v) => {
            let list = v.as_array().ok_or(format!("{at}.only: expected a list of patterns"))?;
            let mut out = Vec::with_capacity(list.len());
            for (i, p) in list.iter().enumerate() {
                let p = p.as_str().ok_or(format!("{at}.only[{i}]: expected a string"))?;
                crate::pattern::validate(p).map_err(|e| format!("{at}.only[{i}]: {e}"))?;
                out.push(p.to_string());
            }
            Some(out)
        }
    };
    let limits = broker::Limits { max: money("max")?, each: money("each")?, uses, only };
    if let (Some(m), Some(e)) = (limits.max, limits.each)
        && m.currency != e.currency
    {
        return Err(format!("{at}: max and each are in different currencies, so no action could satisfy both"));
    }
    Ok(limits)
}

impl Setup {
    fn from_config(prog: &Program, config: &Json) -> Result<Setup, String> {
        let cfg = config.as_object().ok_or("run config: expected a JSON object")?;
        for k in cfg.keys() {
            if !["entry", "inputs", "grants", "secrets", "model", "adapters", "limits", "net", "fs"].contains(&k.as_str()) {
                return Err(format!("run config: unknown key '{k}'"));
            }
        }
        // Every capability limit in the program must be enforceable before anything runs.
        check_all_limits(prog)?;

        let entry = cfg.get("entry").and_then(Json::as_str).ok_or("run config: \"entry\" must name a function")?;
        if entry.contains(' ') {
            return Err(format!("run config: '{entry}' is part of a capability, not a function a run can start"));
        }
        let f = prog.fns.iter().find(|f| f.name == entry).ok_or(format!("run config: no function named '{entry}'"))?;
        let types = types_of(prog);
        let empty = Map::new();
        let inputs = match cfg.get("inputs") {
            None => &empty,
            Some(v) => v.as_object().ok_or("inputs: expected an object")?,
        };
        let grants = match cfg.get("grants") {
            None => &empty,
            Some(v) => v.as_object().ok_or("grants: expected an object")?,
        };
        let secrets = match cfg.get("secrets") {
            None => &empty,
            Some(v) => v.as_object().ok_or("secrets: expected an object")?,
        };
        let is_secret = |p: &Param| p.ty == Ty::Named("Secret".into());
        for k in secrets.keys() {
            match f.params.iter().find(|p| &p.name == k) {
                Some(p) if is_secret(p) => {}
                Some(_) => return Err(format!("secrets.{k}: is not a Secret parameter")),
                None => return Err(format!("secrets.{k}: '{entry}' has no parameter named '{k}'")),
            }
        }
        let mut runtime = runtime_settings(cfg)?;
        for k in inputs.keys() {
            match f.params.iter().find(|p| &p.name == k) {
                None => return Err(format!("inputs.{k}: '{entry}' has no parameter named '{k}'")),
                Some(p) if matches!(p.ty, Ty::Cap { .. }) => {
                    return Err(format!("inputs.{k}: is a capability; capabilities are given in \"grants\""))
                }
                Some(p) if is_secret(p) => {
                    return Err(format!(
                        "inputs.{k}: is a Secret; secrets are read from the environment through \"secrets\", \
                         never written into a run config"
                    ))
                }
                _ => {}
            }
        }
        for k in grants.keys() {
            match f.params.iter().find(|p| &p.name == k) {
                None => return Err(format!("grants.{k}: '{entry}' has no parameter named '{k}'")),
                Some(p) if !matches!(p.ty, Ty::Cap { .. }) => {
                    return Err(format!("grants.{k}: is not a capability parameter; give it in \"inputs\""))
                }
                _ => {}
            }
        }

        let mut args = Vec::new();
        for p in &f.params {
            match &p.ty {
                Ty::Cap { path, limits: declared } => {
                    let at = format!("grants.{}", p.name);
                    let g = grants.get(&p.name).ok_or(format!("{at}: missing; '{entry}' needs this capability"))?;
                    let want = path.join(".");
                    let pg = parse_grant(g, &at, &want, declared, &types, prog, &mut runtime.secrets)?;
                    args.push(Arg::Grant { path: want, limits: pg.limits, idempotency_key: pg.idempotency_key, needs: pg.needs });
                }
                Ty::Proposed(_) => {
                    return Err(format!("inputs.{}: an entry parameter cannot be Proposed; only model calls produce those", p.name))
                }
                _ if is_secret(p) => {
                    let at = format!("secrets.{}", p.name);
                    let spec = secrets.get(&p.name).ok_or(format!("{at}: missing; '{entry}' needs this secret"))?;
                    let secret = load_secret(spec, &at)?;
                    runtime.secrets.push(secret.value.clone());
                    args.push(Arg::Data(Value::Secret(secret)));
                }
                ty => {
                    let at = format!("inputs.{}", p.name);
                    match (inputs.get(&p.name), &p.default) {
                        (Some(j), _) => args.push(Arg::Data(from_json(ty, j, &types, &at)?)),
                        // Left out, with a default: the default, as a call would pass it.
                        (None, Some(d)) => args.push(Arg::Data(eval_pure(d, &types).map_err(|f| format!("{at}: {}", f.message))?)),
                        (None, None) => return Err(format!("{at}: missing")),
                    }
                }
            }
        }

        let mut limits = Limits::default();
        if let Some(l) = cfg.get("limits") {
            let l = l.as_object().ok_or("limits: expected an object")?;
            for (k, v) in l {
                let n = v.as_u64().filter(|n| *n > 0).ok_or(format!("limits.{k}: expected a positive whole number"))?;
                match k.as_str() {
                    "max_steps" => limits.max_steps = n,
                    "default_max_tokens" => limits.default_max_tokens = n,
                    "max_depth" => {
                        if n > MAX_DEPTH_CEILING as u64 {
                            return Err(format!("limits.max_depth: at most {MAX_DEPTH_CEILING}"));
                        }
                        limits.max_depth = n as usize;
                    }
                    _ => return Err(format!("limits: unknown key '{k}'")),
                }
            }
        }
        Ok(Setup { entry: entry.to_string(), args, limits, runtime })
    }
}

fn types_of(prog: &Program) -> Types {
    let mut t = Types::new();
    t.insert("Outcome".into(), vec![("ok".into(), Ty::Named("Bool".into())), ("error".into(), Ty::Named("Text".into()))]);
    t.insert(
        "Response".into(),
        vec![
            ("status".into(), Ty::Named("Int".into())),
            ("body".into(), Ty::Named("Text".into())),
            ("complete".into(), Ty::Named("Bool".into())),
        ],
    );
    for d in &prog.types {
        t.insert(d.name.clone(), d.fields.clone());
    }
    for e in &prog.enums {
        t.insert_enum(e.name.clone(), e.variants.iter().map(|v| (v.name.clone(), v.fields.clone())).collect());
    }
    t
}

fn check_all_limits(prog: &Program) -> Result<(), String> {
    fn ty(t: &Ty, where_: &str) -> Result<(), String> {
        if let Ty::Cap { limits, .. } = t {
            runtime_limits(limits).map_err(|e| format!("{where_}: {e}"))?;
        }
        Ok(())
    }
    fn block(body: &[Stmt], fname: &str) -> Result<(), String> {
        for s in body {
            if let Stmt::Let { ty: Some(t), name, .. } = s {
                ty(t, &format!("in '{fname}', let {name}"))?;
            }
            for b in s.blocks() {
                block(b, fname)?;
            }
        }
        Ok(())
    }
    for f in &prog.fns {
        for p in &f.params {
            ty(&p.ty, &format!("in '{}', parameter {}", f.name, p.name))?;
        }
        if let Some(r) = &f.ret {
            ty(r, &format!("in '{}', return type", f.name))?;
        }
        block(&f.body, &f.name)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The interpreter

struct Meter {
    calls_left: Option<u64>,
    tokens_left: Option<u64>,
}

enum Flow {
    Next,
    Break,
    Continue,
    Return(Value),
}

/// Variables of one function invocation: a stack of block scopes.
struct Frame {
    scopes: Vec<HashMap<String, Value>>,
}

impl Frame {
    fn get(&self, name: &str) -> Result<Value, Failure> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(name) {
                return Ok(v.clone());
            }
        }
        internal(format!("undefined variable '{name}'"))
    }

    /// `set name = v`: replaces the value in the innermost scope that declares `name`.
    fn assign(&mut self, name: &str, v: Value) -> Result<(), Failure> {
        for s in self.scopes.iter_mut().rev() {
            if let Some(slot) = s.get_mut(name) {
                *slot = v;
                return Ok(());
            }
        }
        internal(format!("`set` of undeclared variable '{name}'"))
    }

    fn set(&mut self, name: &str, v: Value) -> Result<(), Failure> {
        match self.scopes.last_mut() {
            Some(s) => {
                s.insert(name.to_string(), v);
                Ok(())
            }
            None => internal("no scope to bind into"),
        }
    }
}

struct Interp<'p> {
    fns: HashMap<&'p str, &'p FnDecl>,
    types: Types,
    provider: Box<dyn Provider + Send>,
    adapter: Box<dyn Adapter + Send>,
    broker: Broker,
    meters: Vec<Meter>,
    journal: Journal,
    log: Vec<Json>,
    steps: u64,
    depth: usize,
    limits: Limits,
    http: http::Client,
    fs_max_read_bytes: usize,
    secrets: Vec<String>,
    /// Library capabilities' actions, by the path programs use (`termii.sms.send`).
    lib: HashMap<String, LibAction>,
    /// What each granted library capability was given for its needs.
    needs: HashMap<usize, Vec<Value>>,
    /// Actions that reached the world (performed, or answered), counted so a library
    /// action can be required to have made at least one request.
    performed: u64,
    /// Attempts at actions that may have changed something (all but net.http.get and
    /// fs.read, and not a request certainly never sent). When a library action fails
    /// after one, its amount may have been spent, and the budget counts it.
    effects: u64,
    /// Native code for the functions that have it, on the native engine.
    native: Option<NativeCode>,
}

#[derive(Clone)]
struct LibAction {
    fn_name: String,
    canonical: Option<String>,
    target: Option<usize>,
    amount: Option<usize>,
}

/// What `live` in `Interp::external` learned from the world.
enum Answer {
    /// A definite answer: success, or a failure that certainly left the world unchanged
    /// or is safe to report as failed.
    Done(Result<Json, String>),
    /// The operation may or may not have happened, and it is not safe to repeat.
    Unknown(String),
}

impl<'p> Interp<'p> {
    fn new(
        prog: &'p Program,
        limits: Limits,
        runtime: &RuntimeSettings,
        provider: Box<dyn Provider + Send>,
        adapter: Box<dyn Adapter + Send>,
        journal: Journal,
    ) -> Self {
        Interp {
            fns: prog.fns.iter().map(|f| (f.name.as_str(), f)).collect(),
            types: types_of(prog),
            provider,
            adapter,
            broker: Broker::default(),
            meters: Vec::new(),
            journal,
            log: Vec::new(),
            steps: 0,
            depth: 0,
            limits,
            http: http::Client::new(&runtime.net),
            fs_max_read_bytes: runtime.fs_max_read_bytes,
            secrets: runtime.secrets.clone(),
            lib: prog
                .caps
                .iter()
                .flat_map(|c| {
                    c.actions.iter().map(move |a| {
                        let la = LibAction {
                            fn_name: a.fn_name.clone(),
                            canonical: c.canonical.clone(),
                            target: a.target,
                            amount: a.amount,
                        };
                        (c.action_path(&a.name), la)
                    })
                })
                .collect(),
            needs: HashMap::new(),
            performed: 0,
            effects: 0,
            native: None,
        }
    }

    fn emit(&mut self, event: &str, mut fields: Json) {
        // Nothing logged may contain a secret, wherever it came from.
        scrub_json(&self.secrets, &mut fields);
        if let Some(obj) = fields.as_object_mut() {
            obj.insert("seq".into(), json!(self.log.len() + 1));
            obj.insert("event".into(), json!(event));
        }
        self.log.push(fields);
    }

    fn run(&mut self, setup: &Setup) -> RunResult {
        let resumed = self.journal.is_resuming();
        self.emit("run_start", json!({ "entry": setup.entry, "run_id": self.journal.run_id(), "resumed": resumed }));
        let mut args = Vec::new();
        for a in &setup.args {
            match a {
                Arg::Data(v) => args.push(v.clone()),
                Arg::Grant { path, limits, idempotency_key, needs } => {
                    let h = self.broker.grant(path, limits.clone());
                    if *idempotency_key {
                        self.broker.set_idempotency_key(h).unwrap_or_default();
                    }
                    let mut given = Vec::with_capacity(needs.len());
                    for n in needs {
                        match n {
                            NeedArg::Data(v) => given.push(v.clone()),
                            NeedArg::Cap { path: ip, limits: il, idempotency_key: ik, declared } => {
                                let ih = self.broker.grant(ip, il.clone());
                                if *ik {
                                    self.broker.set_idempotency_key(ih).unwrap_or_default();
                                }
                                self.emit(
                                    "grant",
                                    json!({ "capability": ip, "for": path, "uses": il.uses, "only": il.only }),
                                );
                                // The library's declared limits narrow what the operator granted.
                                match self.narrow(Value::Cap(ih), declared) {
                                    Ok(v) => given.push(v),
                                    Err(f) => return self.finish_early(f),
                                }
                            }
                        }
                    }
                    if !given.is_empty() {
                        self.needs.insert(h, given);
                    }
                    self.emit(
                        "grant",
                        json!({ "capability": path, "max": limits.max.map(|m| m.to_string()),
                                "each": limits.each.map(|m| m.to_string()), "uses": limits.uses, "only": limits.only }),
                    );
                    args.push(Value::Cap(h));
                }
            }
        }
        let mut result = self.call(&setup.entry, args).and_then(|v| {
            if let Value::Done(_) = &v {
                let mut ev = Vec::new();
                flatten_evidence(&v, &mut ev);
                self.emit("evidence", json!({ "items": ev }));
            }
            to_json(&v, false).or_else(internal)
        });
        // Record how the run ended, unless it must not count as ended: after a journal
        // failure nothing more may be written, and a run stopped in doubt must stay
        // resumable once an operator has resolved it.
        let ending = match &result {
            Ok(v) => Some(json!({ "ok": true, "result": v })),
            Err(f) if matches!(f.kind, FailKind::Journal | FailKind::InDoubt) => None,
            Err(f) => Some(json!({ "ok": false, "failure": f.kind.to_string(), "message": f.message })),
        };
        if let Some(rec) = ending
            && let Err(e) = self.journal.end(rec) {
                result = fail(FailKind::Journal, format!("the run finished, but its end could not be recorded: {e}"));
            }
        match &result {
            Ok(v) => self.emit("run_end", json!({ "ok": true, "result": v })),
            Err(f) => self.emit("run_end", json!({ "ok": false, "failure": f.kind.to_string(), "message": f.message })),
        }
        RunResult { result, log: std::mem::take(&mut self.log) }
    }

    /// Ends a run that failed before its entry function started.
    fn finish_early(&mut self, f: Failure) -> RunResult {
        self.emit("run_end", json!({ "ok": false, "failure": f.kind.to_string(), "message": f.message }));
        RunResult { result: Err(f), log: std::mem::take(&mut self.log) }
    }

    /// A capability flowing into a slot with limits gets a child with those limits.
    /// A slot without limits passes it through unchanged.
    fn narrow(&mut self, v: Value, limits: &CapLimits) -> Result<Value, Failure> {
        let Value::Cap(h) = v else { return internal("a non-capability flowed into a capability slot") };
        if limits.is_empty() {
            return Ok(Value::Cap(h));
        }
        let l = runtime_limits(limits).or_else(internal)?;
        let child = self.broker.narrow(h, l).or_else(internal)?;
        Ok(Value::Cap(child))
    }

    fn call(&mut self, name: &str, args: Vec<Value>) -> Result<Value, Failure> {
        let Some(&f) = self.fns.get(name) else { return internal(format!("undefined function '{name}'")) };
        if f.params.len() != args.len() {
            return internal(format!("'{name}' called with {} arguments, expects {}", args.len(), f.params.len()));
        }
        if self.depth >= self.limits.max_depth {
            return Err(depth_limit(self.limits.max_depth as u64));
        }
        #[cfg(feature = "native")]
        if let Some(n) = &self.native
            && n.has(name)
        {
            let mut ctx = crate::native::Ctx::new(
                self.steps,
                self.limits.max_steps,
                self.depth as u64 + 1,
                self.limits.max_depth as u64,
            );
            if let Some(r) = n.call(name, &mut ctx, &args) {
                self.steps = ctx.steps;
                return r;
            }
        }
        let mut frame = Frame { scopes: vec![HashMap::new()] };
        for (p, v) in f.params.iter().zip(args) {
            let v = match &p.ty {
                Ty::Cap { limits, .. } => self.narrow(v, limits)?,
                _ => v,
            };
            frame.set(&p.name, v)?;
        }
        let metered = f.budget.is_some();
        if let Some(b) = &f.budget {
            self.meters.push(Meter { calls_left: b.calls, tokens_left: b.tokens });
        }
        self.depth += 1;
        let flow = self.exec_block(&mut frame, &f.body);
        self.depth -= 1;
        if metered {
            self.meters.pop();
        }
        match flow? {
            // A capability leaving through a limited return type is narrowed to that
            // limit, exactly as it would be entering a limited parameter or typed let.
            Flow::Return(v) => match &f.ret {
                Some(Ty::Cap { limits, .. }) => self.narrow(v, limits),
                _ => Ok(v),
            },
            Flow::Next if f.ret.is_none() => Ok(Value::Unit),
            Flow::Next => internal(format!("'{name}' finished without returning a value")),
            Flow::Break => internal(format!("`break` escaped from '{name}'")),
            Flow::Continue => internal(format!("`continue` escaped from '{name}'")),
        }
    }

    fn exec_block(&mut self, frame: &mut Frame, body: &[Stmt]) -> Result<Flow, Failure> {
        frame.scopes.push(HashMap::new());
        let mut result = Ok(Flow::Next);
        for s in body {
            match self.exec_stmt(frame, s) {
                Ok(Flow::Next) => continue,
                other => {
                    result = other;
                    break;
                }
            }
        }
        frame.scopes.pop();
        result
    }

    /// Counts one step: every statement, and every loop iteration (so an empty
    /// `loop { }` still reaches the limit instead of spinning forever).
    fn tick(&mut self) -> Result<(), Failure> {
        self.steps += 1;
        if self.steps > self.limits.max_steps {
            return Err(step_limit(self.limits.max_steps));
        }
        Ok(())
    }

    fn exec_stmt(&mut self, frame: &mut Frame, s: &Stmt) -> Result<Flow, Failure> {
        self.tick()?;
        match s {
            Stmt::Let { name, ty, value, .. } => {
                let mut v = self.eval(frame, value)?;
                // `[]` takes its declared type, which may be a set.
                if matches!(ty, Some(Ty::Set(..))) && matches!(value, Expr::List(items, _) if items.is_empty()) {
                    v = Value::Set(Vec::new());
                }
                if let Some(Ty::Cap { limits, .. }) = ty {
                    v = self.narrow(v, limits)?;
                }
                frame.set(name, v)?;
                Ok(Flow::Next)
            }
            Stmt::Set { name, value, .. } => {
                let v = self.eval(frame, value)?;
                frame.assign(name, v)?;
                Ok(Flow::Next)
            }
            Stmt::Fail(msg, _) => match self.eval(frame, msg)? {
                Value::Text(m) => fail(FailKind::Action, m),
                _ => internal("`fail` with a message that is not Text"),
            },
            Stmt::Expr(e) => {
                self.eval(frame, e)?;
                Ok(Flow::Next)
            }
            Stmt::Return(None, _) => Ok(Flow::Return(Value::Unit)),
            Stmt::Return(Some(e), _) => Ok(Flow::Return(self.eval(frame, e)?)),
            Stmt::Repeat { count, body, .. } => {
                for _ in 0..*count {
                    self.tick()?;
                    match self.exec_block(frame, body)? {
                        Flow::Next | Flow::Continue => {}
                        Flow::Break => break,
                        r @ Flow::Return(_) => return Ok(r),
                    }
                }
                Ok(Flow::Next)
            }
            Stmt::Loop { body, .. } => loop {
                self.tick()?;
                match self.exec_block(frame, body)? {
                    Flow::Next | Flow::Continue => {}
                    Flow::Break => return Ok(Flow::Next),
                    r @ Flow::Return(_) => return Ok(r),
                }
            },
            Stmt::If { cond, then, els, .. } => match self.eval(frame, cond)? {
                Value::Bool(true) => self.exec_block(frame, then),
                Value::Bool(false) => self.exec_block(frame, els),
                _ => internal("`if` condition was not a Bool"),
            },
            Stmt::Break(_) => Ok(Flow::Break),
            Stmt::Continue(_) => Ok(Flow::Continue),
            Stmt::Match { value, arms, .. } => {
                // The first arm that accepts the value; the checker made the arms cover
                // every case. Then what the arm binds, in the order written.
                let (arm, binds) = match self.eval(frame, value)? {
                    Value::Enum { variant, fields, .. } => {
                        let arm = arms.iter().find(|a| match &a.pattern {
                            Pattern::Variant { name, .. } => *name == variant,
                            Pattern::Any(_) => true,
                            Pattern::Present { .. } | Pattern::Absent(_) => false,
                        });
                        let Some(arm) = arm else { return internal(format!("no `match` arm for variant {variant}")) };
                        let mut binds = Vec::new();
                        if let Pattern::Variant { binds: names, .. } = &arm.pattern {
                            for (field, to) in names {
                                let Some((_, v)) = fields.iter().find(|(f, _)| f == field) else {
                                    return internal(format!("variant {variant} has no field '{field}'"));
                                };
                                binds.push((to, v.clone()));
                            }
                        }
                        (arm, binds)
                    }
                    Value::Opt(held) => {
                        let arm = arms.iter().find(|a| match &a.pattern {
                            Pattern::Present { .. } => held.is_some(),
                            Pattern::Absent(_) => held.is_none(),
                            Pattern::Any(_) => true,
                            Pattern::Variant { .. } => false,
                        });
                        let Some(arm) = arm else { return internal("no `match` arm for this optional value") };
                        let mut binds = Vec::new();
                        if let (Pattern::Present { bind, .. }, Some(v)) = (&arm.pattern, held) {
                            binds.push((bind, *v));
                        }
                        (arm, binds)
                    }
                    _ => return internal("`match` on a value that is neither an enum nor optional"),
                };
                frame.scopes.push(HashMap::new());
                let mut bound = Ok(());
                for (to, v) in binds {
                    if let Err(e) = frame.set(to, v) {
                        bound = Err(e);
                        break;
                    }
                }
                let flow = bound.and_then(|()| self.exec_block(frame, &arm.body));
                frame.scopes.pop();
                flow
            }
            Stmt::For { var, list, body, .. } => {
                let Value::List(items) = self.eval(frame, list)? else {
                    return internal("`for` over a value that is not a list");
                };
                for item in items {
                    self.tick()?;
                    frame.scopes.push(HashMap::new());
                    let bound = frame.set(var, item);
                    let flow = bound.and_then(|()| self.exec_block(frame, body));
                    frame.scopes.pop();
                    match flow? {
                        Flow::Next | Flow::Continue => {}
                        Flow::Break => break,
                        r @ Flow::Return(_) => return Ok(r),
                    }
                }
                Ok(Flow::Next)
            }
        }
    }

    fn eval(&mut self, frame: &mut Frame, e: &Expr) -> Result<Value, Failure> {
        match e {
            Expr::Var(n, _) => frame.get(n),
            Expr::Str(s, _) => Ok(Value::Text(s.clone())),
            Expr::Bool(b, _) => Ok(Value::Bool(*b)),
            Expr::Money(m, _) => Ok(Value::Money(*m)),
            Expr::Int(n, _) => match i64::try_from(*n) {
                Ok(n) => Ok(Value::Int(n)),
                Err(_) => internal(format!("integer literal {n} is out of range")),
            },
            Expr::NoneLit(_) => Ok(Value::Opt(None)),
            Expr::SomeOf(inner, _) => Ok(Value::Opt(Some(Box::new(self.eval(frame, inner)?)))),
            Expr::Arith { op, left, right, .. } => {
                let l = self.eval(frame, left)?;
                let r = self.eval(frame, right)?;
                arith(*op, l, r)
            }
            Expr::Neg(inner, _) => match self.eval(frame, inner)? {
                Value::Int(n) => neg(n).map(Value::Int),
                _ => internal("`-` applied to a value that is not an Int"),
            },
            Expr::Logic { op, left, right, .. } => {
                let Value::Bool(l) = self.eval(frame, left)? else { return internal("`and`/`or` on a non-Bool") };
                // The right side runs only if it decides the answer.
                let short = match op {
                    LogicOp::And => !l,
                    LogicOp::Or => l,
                };
                if short {
                    return Ok(Value::Bool(l));
                }
                match self.eval(frame, right)? {
                    Value::Bool(r) => Ok(Value::Bool(r)),
                    _ => internal("`and`/`or` on a non-Bool"),
                }
            }
            Expr::Not(inner, _) => match self.eval(frame, inner)? {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                _ => internal("`not` on a non-Bool"),
            },
            Expr::Builtin { f, args, .. } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(frame, a)?);
                }
                builtin(*f, &vals)
            }
            Expr::Record { ty, fields, .. } => {
                let Some(decl) = self.types.get(ty).cloned() else { return internal(format!("unknown record type {ty}")) };
                // Evaluate in the order written (left to right), then store in declaration order.
                let mut given = Vec::with_capacity(fields.len());
                for (name, e) in fields {
                    given.push((name.clone(), self.eval(frame, e)?));
                }
                let mut out = Vec::with_capacity(decl.len());
                for (fname, _) in &decl {
                    match given.iter().position(|(n, _)| n == fname) {
                        Some(i) => out.push(given.swap_remove(i)),
                        None => return internal(format!("{ty} literal without field '{fname}'")),
                    }
                }
                Ok(Value::Record { ty: ty.clone(), fields: out })
            }
            Expr::List(items, _) => {
                let mut vals = Vec::with_capacity(items.len());
                for i in items {
                    vals.push(self.eval(frame, i)?);
                }
                Ok(Value::List(vals))
            }
            Expr::Variant { ty, variant, fields, .. } => {
                let decl = self.types.variants(ty).and_then(|vs| vs.iter().find(|(v, _)| v == variant)).map(|(_, f)| f.clone());
                let Some(decl) = decl else { return internal(format!("unknown variant {ty}.{variant}")) };
                // Evaluated in the order written, stored in declaration order, as records are.
                let mut given = Vec::with_capacity(fields.len());
                for (name, e) in fields {
                    given.push((name.clone(), self.eval(frame, e)?));
                }
                let mut out = Vec::with_capacity(decl.len());
                for (fname, _) in &decl {
                    match given.iter().position(|(n, _)| n == fname) {
                        Some(i) => out.push(given.swap_remove(i)),
                        None => return internal(format!("{ty}.{variant} without field '{fname}'")),
                    }
                }
                Ok(Value::Enum { ty: ty.clone(), variant: variant.clone(), fields: out })
            }
            Expr::Index { base, index, .. } => {
                let Value::List(items) = self.eval(frame, base)? else { return internal("indexing a non-list") };
                let Value::Int(i) = self.eval(frame, index)? else { return internal("a non-Int index") };
                list_index(&items, i)
            }
            Expr::Field { base, field, .. } => match self.eval(frame, base)? {
                Value::Record { fields, .. } => match fields.into_iter().find(|(f, _)| f == field) {
                    Some((_, v)) => Ok(v),
                    None => internal(format!("no field '{field}'")),
                },
                Value::Tuple(items) => tuple_item(items, field),
                Value::Outcome { ok, error, .. } => match field.as_str() {
                    "ok" => Ok(Value::Bool(ok)),
                    "error" => Ok(Value::Text(error)),
                    _ => internal(format!("Outcome has no field '{field}'")),
                },
                _ => internal(format!("cannot read '.{field}' of this value")),
            },
            Expr::Cmp { op, left, right, .. } => {
                let l = self.eval(frame, left)?;
                let r = self.eval(frame, right)?;
                compare(*op, &l, &r).map(Value::Bool)
            }
            Expr::Call { name, args, slots, .. } => {
                // Evaluated in the order written, passed in parameter order.
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(frame, a)?);
                }
                self.call(name, in_slots(vals, slots)?)
            }
            Expr::Named { .. } => internal("a named argument outside a call"),
            Expr::Tuple(items, _) => {
                let mut vals = Vec::with_capacity(items.len());
                for i in items {
                    vals.push(self.eval(frame, i)?);
                }
                Ok(Value::Tuple(vals))
            }
            Expr::MapLit(entries, _) => {
                let mut vals = Vec::with_capacity(entries.len() * 2);
                for (k, v) in entries {
                    vals.push(self.eval(frame, k)?);
                    vals.push(self.eval(frame, v)?);
                }
                map_literal(vals)
            }
            Expr::Action { receiver, method, args, .. } => {
                let Value::Cap(h) = frame.get(receiver)? else {
                    return internal(format!("'{receiver}' is not a capability"));
                };
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(frame, a)?);
                }
                self.action(h, method, vals)
            }
            Expr::Propose { ty, instruct, data, tokens, .. } => {
                let instruction = match instruct {
                    Some(i) => match self.eval(frame, i)? {
                        Value::Text(t) => t,
                        _ => return internal("`instruct` was not Text"),
                    },
                    None => "Respond with a value of the requested type.".to_string(),
                };
                let mut payload = Vec::with_capacity(data.len());
                for d in data {
                    let v = self.eval(frame, d)?;
                    // Re-checked here even though the compiler forbids it.
                    payload.push(to_json(&v, true).or_else(internal)?);
                }
                self.model_call(ty, instruction, payload, *tokens)
            }
            Expr::Parse { ty, text, .. } => match self.eval(frame, text)? {
                Value::Text(t) => match serde_json::from_str::<Json>(&t) {
                    Ok(raw) => Ok(Value::Proposed { ty: ty.clone(), raw, lenient: true }),
                    Err(e) => fail(FailKind::Validation, format!("`parse` was given text that is not JSON: {e}")),
                },
                _ => internal("`parse` was given a value that is not Text"),
            },
            Expr::Validate(inner, _) => match self.eval(frame, inner)? {
                Value::Proposed { ty, raw, lenient } => match if lenient {
                    from_json_lenient(&ty, &raw, &self.types, "parsed text")
                } else {
                    from_json(&ty, &raw, &self.types, "model output")
                } {
                    Ok(v) => {
                        self.emit("validated", json!({ "type": ty.to_string(), "ok": true }));
                        Ok(v)
                    }
                    Err(msg) => {
                        self.emit("validated", json!({ "type": ty.to_string(), "ok": false, "error": msg }));
                        fail(FailKind::Validation, msg)
                    }
                },
                _ => internal("`validate` applied to a value that is not Proposed"),
            },
            Expr::Try(inner, _) => match self.eval(frame, inner) {
                Ok(v) => Ok(Value::Outcome { ok: true, error: String::new(), held: Some(Box::new(v)) }),
                Err(f) if !f.kind.catchable() => Err(f),
                Err(f) => {
                    let error = format!("{}: {}", f.kind, f.message);
                    self.emit("caught", json!({ "failure": f.kind.to_string(), "message": f.message }));
                    Ok(Value::Outcome { ok: false, error, held: None })
                }
            },
            Expr::Done { evidence, .. } => {
                let mut items = Vec::with_capacity(evidence.len());
                for it in evidence {
                    let v = self.eval(frame, it)?;
                    // Re-checked here even though E008 guarantees it: a Done must never
                    // be made from something that leads to no receipt.
                    if receipt_count(&v) == 0 {
                        return internal("an evidence item holds no receipt; E008 should have rejected this program");
                    }
                    items.push(v);
                }
                if items.is_empty() {
                    return internal("`done` with no evidence; E008 should have rejected this program");
                }
                Ok(Value::Done(items))
            }
        }
    }

    // RUNTIME.md section 6
    fn action(&mut self, h: usize, method: &str, args: Vec<Value>) -> Result<Value, Failure> {
        let path = self.broker.path(h).or_else(internal)?.to_string();
        // The runtime's own capabilities never reach an adapter.
        if path.starts_with("net.http.") {
            return self.http_action(h, &path, method, args);
        }
        if path.starts_with("fs.") {
            return self.fs_action(h, &path, method, args);
        }
        if let Some(la) = self.lib.get(&path).cloned() {
            return self.library_action(h, &path, method, args, la);
        }
        if is_runtime_cap(&path) {
            return internal(format!("'{path}' is not a runtime capability; the compiler should have refused it"));
        }
        if args.iter().any(|a| matches!(a, Value::Secret(_) | Value::Header { .. })) {
            return internal("a secret was passed to an adapter's action; the compiler should have refused it");
        }
        let amount = self.adapter.amount(&path, method, &args);
        let target = self.adapter.target(&path, method, &args);
        let amount_text = amount.map(|m| m.to_string());
        self.emit(
            "action_requested",
            json!({ "capability": path, "method": method, "amount": amount_text, "target": target }),
        );
        if let Err(why) = self.broker.authorize(h, amount, target.as_deref()) {
            self.emit("action_refused", json!({ "capability": path, "method": method, "reason": why }));
            return fail(FailKind::Capability, why);
        }
        let mut args_json = Vec::with_capacity(args.len());
        for a in &args {
            match to_json(a, true) {
                Ok(j) => args_json.push(j),
                Err(_) => {
                    let why = "a capability cannot be passed to an action".to_string();
                    self.emit("action_refused", json!({ "capability": path, "method": method, "reason": why }));
                    return fail(FailKind::Capability, why);
                }
            }
        }
        // Approved: this attempt counts against every `uses` limit in the chain, whatever
        // the service then answers.
        self.broker.count_use(h).or_else(internal)?;
        let body =
            json!({ "capability": path, "method": method, "args": args_json, "amount": amount_text, "target": target });
        let idempotent = self.adapter.idempotent(&path, method);
        let what = format!("action {path}.{method}");
        let (answer, replayed) = self.external("action", body, idempotent, &what, |it, key| {
            Answer::Done(it.adapter.perform(key, &path, method, &args_json).map(|id| json!({ "receipt": id })))
        })?;
        match answer {
            Ok(v) => {
                let Some(id) = v["receipt"].as_str().map(str::to_string) else {
                    return fail(FailKind::Journal, format!("malformed action result in the journal: {v}"));
                };
                // The action has happened in the world: record it before any bookkeeping
                // that could fail, so the log never misses a performed action.
                self.performed += 1;
                self.effects += 1;
                self.emit(
                    "action_performed",
                    json!({ "capability": path, "method": method, "amount": amount_text, "args": args_json,
                            "receipt": id, "replayed": replayed }),
                );
                self.broker.charge(h, amount).or_else(internal)?;
                Ok(Value::Receipt(Receipt { id, capability: path, method: method.to_string() }))
            }
            Err(msg) => {
                // An adapter's failure does not say whether anything happened.
                self.effects += 1;
                self.emit("action_failed", json!({ "capability": path, "method": method, "error": msg, "replayed": replayed }));
                fail(FailKind::Action, msg)
            }
        }
    }

    // RUNTIME.md section 13
    fn library_action(&mut self, h: usize, path: &str, method: &str, args: Vec<Value>, la: LibAction) -> Result<Value, Failure> {
        // The target, in the one form the library defines, is what `only` is checked against.
        let target = match la.target {
            None => None,
            Some(i) => {
                let Some(Value::Text(raw)) = args.get(i) else { return internal("a @target that is not Text") };
                let Some(canon) = la.canonical.clone() else { return internal("a @target without `canonical`") };
                match self.call(&canon, vec![Value::Text(raw.clone())]) {
                    Ok(Value::Text(c)) => Some(c),
                    Ok(_) => return internal("`canonical` returned something that is not Text"),
                    Err(f) if f.kind.catchable() => {
                        let why = format!("the target cannot be put in canonical form: {}", f.message);
                        return self.refuse(path, method, FailKind::Capability, why);
                    }
                    Err(f) => return Err(f),
                }
            }
        };
        let amount = match la.amount {
            None => None,
            Some(i) => match args.get(i) {
                Some(Value::Money(m)) => Some(*m),
                _ => return internal("an @amount that is not Money"),
            },
        };
        let amount_text = amount.map(|m| m.to_string());
        self.emit(
            "action_requested",
            json!({ "capability": path, "method": method, "amount": amount_text, "target": target }),
        );
        if let Err(why) = self.broker.authorize(h, amount, target.as_deref()) {
            return self.refuse(path, method, FailKind::Capability, why);
        }
        self.broker.count_use(h).or_else(internal)?;
        let root = self.broker.root(h).or_else(internal)?;
        let mut call_args = args;
        call_args.extend(self.needs.get(&root).cloned().unwrap_or_default());
        let (before, effects_before) = (self.performed, self.effects);
        match self.call(&la.fn_name, call_args) {
            Ok(Value::Text(id)) => {
                // A receipt must prove something happened: the action made a request.
                if self.performed == before {
                    let why = format!("{path} returned without making any request, so there is nothing to give a receipt for");
                    self.emit("action_failed", json!({ "capability": path, "method": method, "error": why }));
                    return fail(FailKind::Action, why);
                }
                self.performed += 1;
                self.effects += 1;
                self.emit(
                    "action_performed",
                    json!({ "capability": path, "method": method, "amount": amount_text, "target": target, "receipt": id }),
                );
                self.broker.charge(h, amount).or_else(internal)?;
                Ok(Value::Receipt(Receipt { id, capability: path.to_string(), method: method.to_string() }))
            }
            Ok(_) => internal("a capability action returned something that is not Text"),
            Err(f) => {
                // A request that may have changed something was attempted before the failure,
                // so the money may have been spent: the budget counts it, and a failure can
                // never be used to spend past a limit.
                let may_have_spent = self.effects != effects_before;
                if may_have_spent {
                    self.effects += 1;
                }
                self.emit(
                    "action_failed",
                    json!({ "capability": path, "method": method, "error": f.message, "may_have_spent": may_have_spent }),
                );
                if may_have_spent {
                    self.broker.charge(h, amount).or_else(internal)?;
                }
                Err(f)
            }
        }
    }

    fn refuse<T>(&mut self, path: &str, method: &str, kind: FailKind, why: String) -> Result<T, Failure> {
        self.emit("action_refused", json!({ "capability": path, "method": method, "reason": why }));
        fail(kind, why)
    }

    // RUNTIME.md section 12
    fn http_action(&mut self, h: usize, path: &str, method: &str, args: Vec<Value>) -> Result<Value, Failure> {
        let has_body = matches!(method, "post" | "put" | "patch");
        let mut it = args.into_iter();
        let Some(Value::Text(raw_url)) = it.next() else { return internal("net.http without a URL") };
        // A body is Text, or a Secret (a template filled by `fill_secret`).
        let (body, body_shown): (Option<String>, Json) = if has_body {
            match it.next() {
                Some(Value::Text(b)) => (Some(b.clone()), json!(b)),
                Some(Value::Secret(s)) => (Some(s.value), json!({ "secret": s.name, "shown": s.shown })),
                _ => return internal("net.http without a body"),
            }
        } else {
            (None, Json::Null)
        };
        let mut headers: Vec<(String, HeaderValue)> = Vec::new();
        match it.next() {
            None => {}
            Some(Value::List(hs)) => {
                for h in hs {
                    match h {
                        Value::Header { name, value } => headers.push((name, value)),
                        _ => return internal("a header list holds something that is not a header"),
                    }
                }
            }
            Some(_) => return internal("net.http headers are not a list"),
        }
        let url = match crate::targets::canonical_url(&raw_url) {
            Ok(u) => u,
            Err(e) => return self.refuse(path, method, FailKind::Capability, e),
        };
        for (n, v) in &headers {
            let value = match v {
                HeaderValue::Text(t) => t.as_str(),
                HeaderValue::Secret(s) => s.value.as_str(),
            };
            if let Err(e) = http::check_header(n, value) {
                return self.refuse(path, method, FailKind::Value, e);
            }
        }
        self.emit("action_requested", json!({ "capability": path, "method": method, "target": url }));
        if let Err(why) = self.broker.authorize(h, None, Some(&url)) {
            return self.refuse(path, method, FailKind::Capability, why);
        }
        // GET is safe to repeat by definition; the others only when the operator has said
        // the service deduplicates by Idempotency-Key, which the runtime then sends.
        let keyed = method != "get" && self.broker.idempotency_key(h).or_else(internal)?;
        let retryable = method == "get" || keyed;
        self.broker.count_use(h).or_else(internal)?;
        let shown: Vec<Json> = headers
            .iter()
            .map(|(n, v)| match v {
                HeaderValue::Text(t) => json!([n, t]),
                HeaderValue::Secret(s) => json!([n, { "secret": s.name, "shown": s.shown }]),
            })
            .collect();
        // What the journal records: never a secret's value (see `Secret::shown`).
        let jbody = json!({ "capability": path, "method": method, "target": url, "headers": shown,
                            "body": body_shown, "idempotency_key": keyed });
        let what = format!("action {path} {url}");
        let mut send: Vec<(String, String)> = headers
            .into_iter()
            .map(|(n, v)| match v {
                HeaderValue::Text(t) => (n, t),
                HeaderValue::Secret(s) => (n, s.value),
            })
            .collect();
        let (answer, replayed) = self.external("action", jbody, retryable, &what, |it, key| {
            if keyed {
                send.push(("Idempotency-Key".into(), key.to_string()));
            }
            let req = http::Request { method, url: &url, headers: &send, body: body.as_deref() };
            match it.http.send(&req) {
                http::Outcome::Response { status, body, complete } => Answer::Done(Ok(
                    json!({ "status": status, "body": scrub_text(&it.secrets, &body), "complete": complete }),
                )),
                http::Outcome::NotSent(e) => Answer::Done(Err(format!("{NOT_SENT}{e}"))),
                // Safe to report as a failure: repeating it cannot do anything twice.
                http::Outcome::Unknown(e) if retryable => Answer::Done(Err(format!("no answer: {e}"))),
                http::Outcome::Unknown(e) => Answer::Unknown(e),
            }
        })?;
        match answer {
            Ok(v) => {
                let (Some(status), Some(text), Some(complete)) =
                    (v["status"].as_i64(), v["body"].as_str(), v["complete"].as_bool())
                else {
                    return fail(FailKind::Journal, format!("malformed HTTP result in the journal: {v}"));
                };
                self.performed += 1;
                if method != "get" {
                    self.effects += 1;
                }
                self.emit(
                    "action_performed",
                    json!({ "capability": path, "method": method, "target": url, "status": status, "replayed": replayed }),
                );
                Ok(Value::Record {
                    ty: "Response".into(),
                    fields: vec![
                        ("status".into(), Value::Int(status)),
                        ("body".into(), Value::Text(text.to_string())),
                        ("complete".into(), Value::Bool(complete)),
                    ],
                })
            }
            Err(msg) => {
                // Unless it was certainly never sent, a request may have been acted on.
                if method != "get" && !msg.starts_with(NOT_SENT) {
                    self.effects += 1;
                }
                self.emit("action_failed", json!({ "capability": path, "method": method, "error": msg, "replayed": replayed }));
                fail(FailKind::Action, msg)
            }
        }
    }

    // RUNTIME.md section 12
    fn fs_action(&mut self, h: usize, path: &str, method: &str, args: Vec<Value>) -> Result<Value, Failure> {
        let Some(Value::Text(raw)) = args.first() else { return internal("fs without a path") };
        let file = match crate::targets::canonical_path(raw) {
            Ok(f) => f,
            Err(e) => return self.refuse(path, method, FailKind::Capability, e),
        };
        self.emit("action_requested", json!({ "capability": path, "method": method, "target": file }));
        if let Err(why) = self.broker.authorize(h, None, Some(&file)) {
            return self.refuse(path, method, FailKind::Capability, why);
        }
        self.broker.count_use(h).or_else(internal)?;
        let what = format!("action {path} {file}");
        let (answer, replayed) = match method {
            // Reading changes nothing, and writing the same content again leaves the same
            // file, so both are safe to retry after a crash.
            "read" => {
                let jbody = json!({ "capability": path, "method": method, "target": file });
                self.external("action", jbody, true, &what, |it, _key| {
                    Answer::Done(fsx::read(&file, it.fs_max_read_bytes).map(|t| json!({ "text": scrub_text(&it.secrets, &t) })))
                })?
            }
            "write" => {
                let Some(Value::Text(content)) = args.get(1) else { return internal("fs.write without content") };
                let sha = journal::sha256_hex(content.as_bytes());
                // The journal records the content's hash, not the content: on resume the
                // program computes it again, and the hash detects any difference.
                let jbody = json!({ "capability": path, "method": method, "target": file,
                                    "bytes": content.len(), "sha256": sha });
                let id = format!("write:{}", &sha[..16]);
                self.external("action", jbody, true, &what, |_it, _key| {
                    Answer::Done(fsx::write(&file, content.as_bytes()).map(|()| json!({ "receipt": id })))
                })?
            }
            other => return internal(format!("fs has no method '{other}'")),
        };
        if method == "write" {
            // A failed write leaves the old file or the new one; count it either way.
            self.effects += 1;
        }
        match answer {
            Ok(v) => {
                self.performed += 1;
                self.emit("action_performed", json!({ "capability": path, "method": method, "target": file, "replayed": replayed }));
                match (method, v["text"].as_str(), v["receipt"].as_str()) {
                    ("read", Some(t), _) => Ok(Value::Text(t.to_string())),
                    ("write", _, Some(id)) => {
                        Ok(Value::Receipt(Receipt { id: id.to_string(), capability: path.to_string(), method: method.to_string() }))
                    }
                    _ => fail(FailKind::Journal, format!("malformed file result in the journal: {v}")),
                }
            }
            Err(msg) => {
                self.emit("action_failed", json!({ "capability": path, "method": method, "error": msg, "replayed": replayed }));
                fail(FailKind::Action, msg)
            }
        }
    }

    /// Performs one external operation through the journal (RUNTIME.md section 11).
    ///
    /// `live` contacts the world and returns its answer. It runs for a new operation,
    /// and for one left in doubt by a crash only when `retry_in_doubt` holds; an
    /// operation in doubt that cannot be retried stops the run (`in_doubt`), named by
    /// `what`. Returns the answer and whether it came from the journal.
    fn external(
        &mut self,
        op: &str,
        body: Json,
        retry_in_doubt: bool,
        what: &str,
        live: impl FnOnce(&mut Self, &str) -> Answer,
    ) -> Result<(Result<Json, String>, bool), Failure> {
        let step = match self.journal.begin(op, body) {
            Ok(step) => step,
            Err(e) => return fail(FailKind::Journal, e),
        };
        let (n, key) = match step {
            Step::Replay(rec) => {
                let answer = match (rec.get("ok"), rec.get("err").and_then(Json::as_str)) {
                    (Some(ok), None) => Ok(ok.clone()),
                    (None, Some(e)) => Err(e.to_string()),
                    _ => return fail(FailKind::Journal, format!("malformed result record in the journal: {rec}")),
                };
                return Ok((answer, true));
            }
            Step::Perform { n, key } => (n, key),
            Step::InDoubt { n, key } if retry_in_doubt => {
                self.emit("retrying_in_doubt", json!({ "operation": n, "what": what, "key": key }));
                (n, key)
            }
            Step::InDoubt { n, .. } => {
                self.emit("in_doubt", json!({ "operation": n, "what": what }));
                return fail(
                    FailKind::InDoubt,
                    format!(
                        "{what} (operation {n}) may or may not have happened before the run stopped, and it cannot \
                         be safely retried. Check whether it happened, record the answer with `staalc resolve` \
                         (--performed <receipt>, --failed <reason>, or --retry if it did not happen), then resume"
                    ),
                );
            }
        };
        let answer = match live(self, &key) {
            Answer::Done(a) => a,
            // Exactly as if the process had died here: the intent is recorded, the result
            // is not, and only an operator can say what happened.
            // An operation that is safe to repeat is made again when the run resumes:
            // nothing needs resolving first.
            Answer::Unknown(e) if retry_in_doubt => {
                self.emit("in_doubt", json!({ "operation": n, "what": what, "error": e, "retry_on_resume": true }));
                return fail(
                    FailKind::InDoubt,
                    format!("{what} (operation {n}) did not complete ({e}). Resume the run to make it again"),
                );
            }
            Answer::Unknown(e) => {
                self.emit("in_doubt", json!({ "operation": n, "what": what, "error": e }));
                return fail(
                    FailKind::InDoubt,
                    format!(
                        "{what} (operation {n}) may or may not have happened ({e}), and it cannot be safely \
                         retried. Check whether it happened, record the answer with `staalc resolve`, then resume"
                    ),
                );
            }
        };
        if let Err(e) = self.journal.finish(n, answer.clone()) {
            // The operation happened but is not durably recorded. Keep it in the log, then
            // stop: resume will find it in doubt.
            let shown = match &answer {
                Ok(v) => json!({ "ok": v }),
                Err(msg) => json!({ "err": msg }),
            };
            self.emit("journal_write_failed", json!({ "operation": n, "op": op, "answer": shown, "error": e }));
            return fail(FailKind::Journal, e);
        }
        Ok((answer, false))
    }

    // RUNTIME.md sections 5 and 7
    fn model_call(&mut self, ty: &Ty, instruction: String, data: Vec<Json>, tokens: Option<u64>) -> Result<Value, Failure> {
        // Refuse a call with no budget left before anything reaches the provider:
        // counting tokens means sending the request's contents to it.
        if self.meters.iter().any(|m| m.calls_left == Some(0)) {
            let why = "no model calls left in the budget".to_string();
            self.emit("model_refused", json!({ "reason": why }));
            return fail(FailKind::Budget, why);
        }
        let mut req =
            ModelRequest { instruction, data, output_schema: schema(ty, &self.types), max_output_tokens: 0 };
        // A real provider's count can change between runs (a tokenizer update), and it
        // decides the token limit, so it is journaled like any other answer from outside.
        let (counted, _) = self.external("count", req.to_json(), true, "token count", |it, _key| {
            match it.provider.count_input_tokens(&req) {
                Ok(n) => Answer::Done(Ok(json!(n))),
                Err(ProviderError::Failed(e)) => Answer::Done(Err(e)),
                Err(ProviderError::Unavailable(e)) => Answer::Unknown(e),
            }
        })?;
        let input = match counted {
            Ok(v) => match v.as_u64() {
                Some(n) => n,
                None => return fail(FailKind::Journal, format!("malformed token count in the journal: {v}")),
            },
            Err(e) => return fail(FailKind::Provider, format!("could not count input tokens: {e}")),
        };
        let max_output = match tokens {
            Some(n) if input >= n => {
                let why = format!("the request alone is {input} tokens, but this call may use at most {n}");
                self.emit("model_refused", json!({ "reason": why }));
                return fail(FailKind::Budget, why);
            }
            Some(n) => n - input,
            None => self.limits.default_max_tokens,
        };
        let Some(worst) = input.checked_add(max_output) else { return internal("token count overflow") };
        for m in &self.meters {
            let why = match (m.calls_left, m.tokens_left) {
                (Some(0), _) => Some("no model calls left in the budget".to_string()),
                (_, Some(t)) if t < worst => {
                    Some(format!("this call may use {worst} tokens, but the budget has {t} left"))
                }
                _ => None,
            };
            if let Some(why) = why {
                self.emit("model_refused", json!({ "reason": why }));
                return fail(FailKind::Budget, why);
            }
        }
        req.max_output_tokens = max_output;
        // The call counts against every open budget as soon as it is attempted.
        for m in &mut self.meters {
            if let Some(c) = &mut m.calls_left {
                *c -= 1; // checked above: every limited meter has at least one call left
            }
        }
        self.emit("model_request", json!({ "request": req.to_json(), "input_tokens_counted": input }));
        let (answer, replayed) = self.external("model", req.to_json(), true, "model call", |it, _key| {
            match it.provider.complete(&req) {
                // A reply without output (a refusal, output cut off) still used tokens:
                // it is recorded with them, so a resumed run charges them too.
                Ok(r) => Answer::Done(Ok(match r.output {
                    Ok(out) => json!({ "output": out, "input_tokens": r.input_tokens, "output_tokens": r.output_tokens }),
                    Err(e) => json!({ "error": e, "input_tokens": r.input_tokens, "output_tokens": r.output_tokens }),
                })),
                Err(ProviderError::Failed(e)) => Answer::Done(Err(e)),
                Err(ProviderError::Unavailable(e)) => Answer::Unknown(e),
            }
        })?;
        if replayed {
            self.provider.skip();
        }
        let reply = match answer {
            Ok(v) => match (v.get("output"), v.get("error").and_then(Json::as_str), v["input_tokens"].as_u64(), v["output_tokens"].as_u64()) {
                (Some(output), None, Some(input_tokens), Some(output_tokens)) => {
                    provider::ModelReply { output: Ok(output.clone()), input_tokens, output_tokens }
                }
                (None, Some(e), Some(input_tokens), Some(output_tokens)) => {
                    provider::ModelReply { output: Err(e.to_string()), input_tokens, output_tokens }
                }
                _ => return fail(FailKind::Journal, format!("malformed model reply in the journal: {v}")),
            },
            Err(e) => {
                self.emit("model_error", json!({ "error": e, "replayed": replayed }));
                return fail(FailKind::Provider, e);
            }
        };
        let actual = reply.input_tokens.saturating_add(reply.output_tokens);
        for m in &mut self.meters {
            if let Some(t) = &mut m.tokens_left {
                *t = t.saturating_sub(actual);
            }
        }
        let shown = match &reply.output {
            Ok(out) => json!({ "output": out }),
            Err(e) => json!({ "error": e }),
        };
        let mut event = json!({ "input_tokens": reply.input_tokens, "output_tokens": reply.output_tokens, "replayed": replayed });
        if let (Some(e), Some(s)) = (event.as_object_mut(), shown.as_object()) {
            e.extend(s.clone());
        }
        self.emit("model_reply", event);
        if reply.output_tokens > max_output || actual > worst {
            return fail(
                FailKind::Provider,
                format!(
                    "the provider used {actual} tokens ({} output), exceeding the limit of {worst} ({max_output} output)",
                    reply.output_tokens
                ),
            );
        }
        match reply.output {
            Ok(raw) => Ok(Value::Proposed { ty: ty.clone(), raw, lenient: false }),
            Err(e) => fail(FailKind::Provider, e),
        }
    }
}

// Failures that native code reports too: one definition, so both engines give the
// same message (src/native.rs).

pub(crate) fn list_index(items: &[Value], i: i64) -> Result<Value, Failure> {
    match usize::try_from(i).ok().and_then(|u| items.get(u)) {
        Some(v) => Ok(v.clone()),
        None => fail(FailKind::Value, format!("index {i} is out of range for a list of {}", items.len())),
    }
}

pub(crate) fn step_limit(max_steps: u64) -> Failure {
    Failure { kind: FailKind::StepLimit, message: format!("more than {max_steps} steps executed") }
}

pub(crate) fn depth_limit(max_depth: u64) -> Failure {
    Failure { kind: FailKind::DepthLimit, message: format!("calls nested more than {max_depth} deep") }
}

pub(crate) fn neg(n: i64) -> Result<i64, Failure> {
    n.checked_neg().ok_or_else(|| Failure { kind: FailKind::Value, message: format!("integer overflow: -({n})") })
}

/// The failure `arith` gives for Int operands that fail: native code calls this only
/// after its own check failed, so an Ok here means the two engines disagree.
#[cfg(feature = "native")]
pub(crate) fn arith_failure(op: ArithOp, a: i64, b: i64) -> Failure {
    match arith(op, Value::Int(a), Value::Int(b)) {
        Err(f) => f,
        Ok(_) => Failure { kind: FailKind::Internal, message: format!("native code reported {a} {op} {b} as failing; it does not") },
    }
}

/// Checked arithmetic (RUNTIME.md section 4). Every overflow, division by zero, mix of
/// currencies or result below zero is a `value` failure, never a wrapped or rounded answer.
pub(crate) fn arith(op: ArithOp, l: Value, r: Value) -> Result<Value, Failure> {
    let overflow = |a: &dyn fmt::Display, b: &dyn fmt::Display| fail(FailKind::Value, format!("overflow: {a} {op} {b}"));
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => {
            let v = match op {
                ArithOp::Add => a.checked_add(b),
                ArithOp::Sub => a.checked_sub(b),
                ArithOp::Mul => a.checked_mul(b),
                ArithOp::Div if b == 0 => return fail(FailKind::Value, format!("division by zero: {a} / 0")),
                // Rounds toward zero. checked_div fails only for i64::MIN / -1.
                ArithOp::Div => a.checked_div(b),
            };
            match v {
                Some(v) => Ok(Value::Int(v)),
                None => overflow(&a, &b),
            }
        }
        (Value::Money(a), Value::Money(b)) if matches!(op, ArithOp::Add | ArithOp::Sub) => {
            if a.currency != b.currency {
                return fail(FailKind::Value, format!("cannot combine {a} and {b}: different currencies"));
            }
            let v = if op == ArithOp::Add { a.minor.checked_add(b.minor) } else { a.minor.checked_sub(b.minor) };
            match v {
                Some(m) if m >= 0 => Ok(Value::Money(Money { minor: m, currency: a.currency })),
                Some(_) => fail(FailKind::Value, format!("money cannot be negative: {a} - {b}")),
                None => overflow(&a, &b),
            }
        }
        (Value::Money(m), Value::Int(k)) | (Value::Int(k), Value::Money(m)) if op == ArithOp::Mul => {
            match m.minor.checked_mul(k) {
                Some(v) if v >= 0 => Ok(Value::Money(Money { minor: v, currency: m.currency })),
                Some(_) => fail(FailKind::Value, format!("money cannot be negative: {m} * {k}")),
                None => overflow(&m, &k),
            }
        }
        (Value::Text(a), Value::Text(b)) if op == ArithOp::Add => {
            if a.len().saturating_add(b.len()) > MAX_TEXT_BYTES {
                return fail(FailKind::Value, format!("text would be longer than {MAX_TEXT_BYTES} bytes"));
            }
            Ok(Value::Text(a + &b))
        }
        _ => internal(format!("`{op}` applied to values the compiler should have rejected")),
    }
}

/// The built-in functions. `len` of Text counts Unicode characters, not bytes.
pub(crate) fn builtin(f: Builtin, args: &[Value]) -> Result<Value, Failure> {
    let count = |n: usize| i64::try_from(n).map(Value::Int).or_else(|_| internal("length does not fit in an Int"));
    match (f, args) {
        (Builtin::Len, [Value::Text(t)]) => count(t.chars().count()),
        (Builtin::Len, [Value::List(xs)]) => count(xs.len()),
        (Builtin::Len, [Value::Map(es)]) => count(es.len()),
        (Builtin::Len, [Value::Set(xs)]) => count(xs.len()),
        (Builtin::Contains, [Value::Text(a), Value::Text(b)]) => Ok(Value::Bool(a.contains(b.as_str()))),
        (Builtin::Contains, [Value::List(xs), v]) => Ok(Value::Bool(xs.contains(v))),
        (Builtin::Contains, [Value::Map(es), k]) => Ok(Value::Bool(es.iter().any(|(x, _)| x == k))),
        (Builtin::Contains, [Value::Set(xs), v]) => Ok(Value::Bool(xs.contains(v))),
        (Builtin::Get, [Value::Map(es), k]) => Ok(Value::Opt(es.iter().find(|(x, _)| x == k).map(|(_, v)| Box::new(v.clone())))),
        (Builtin::Put, [Value::Map(es), k, v, Value::Int(cap)]) => {
            let mut es = es.clone();
            match es.binary_search_by(|(x, _)| key_order(x, k)) {
                Ok(i) => es[i].1 = v.clone(),
                Err(i) => {
                    if es.len() as i64 >= *cap {
                        return fail(FailKind::Value, format!("the map is full: it holds at most {cap} entries, so {} cannot be added", shown(k)));
                    }
                    es.insert(i, (k.clone(), v.clone()));
                }
            }
            Ok(Value::Map(es))
        }
        (Builtin::Remove, [Value::Map(es), k]) => Ok(Value::Map(es.iter().filter(|(x, _)| x != k).cloned().collect())),
        (Builtin::Remove, [Value::Set(xs), v]) => Ok(Value::Set(xs.iter().filter(|x| *x != v).cloned().collect())),
        (Builtin::Keys, [Value::Map(es)]) => Ok(Value::List(es.iter().map(|(k, _)| k.clone()).collect())),
        (Builtin::Values, [Value::Map(es)]) => Ok(Value::List(es.iter().map(|(_, v)| v.clone()).collect())),
        (Builtin::Entries, [Value::Map(es)]) => {
            Ok(Value::List(es.iter().map(|(k, v)| Value::Tuple(vec![k.clone(), v.clone()])).collect()))
        }
        (Builtin::Add, [Value::Set(xs), v, Value::Int(cap)]) => {
            let mut xs = xs.clone();
            if let Err(i) = xs.binary_search_by(|x| key_order(x, v)) {
                if xs.len() as i64 >= *cap {
                    return fail(FailKind::Value, format!("the set is full: it holds at most {cap} items, so {} cannot be added", shown(v)));
                }
                xs.insert(i, v.clone());
            }
            Ok(Value::Set(xs))
        }
        (Builtin::Items, [Value::Set(xs)]) => Ok(Value::List(xs.clone())),
        (Builtin::ToSet, [Value::List(xs)]) => {
            let mut xs = xs.clone();
            xs.sort_by(key_order);
            xs.dedup();
            Ok(Value::Set(xs))
        }
        (Builtin::StartsWith, [Value::Text(a), Value::Text(b)]) => Ok(Value::Bool(a.starts_with(b.as_str()))),
        (Builtin::EndsWith, [Value::Text(a), Value::Text(b)]) => Ok(Value::Bool(a.ends_with(b.as_str()))),
        (Builtin::Header, [Value::Text(name), v]) => {
            let value = match v {
                Value::Text(t) => HeaderValue::Text(t.clone()),
                Value::Secret(s) => HeaderValue::Secret(s.clone()),
                _ => return internal("`header` value is neither Text nor a Secret"),
            };
            let check = match &value {
                HeaderValue::Text(t) => http::check_header(name, t),
                HeaderValue::Secret(s) => http::check_header(name, &s.value),
            };
            check.map_err(|e| Failure { kind: FailKind::Value, message: e })?;
            Ok(Value::Header { name: name.clone(), value })
        }
        (Builtin::FillSecret, [Value::Text(template), Value::Secret(key)]) => {
            // Exactly once. The program writes its own placeholder; a second one can only
            // have come in with data (a message text saying "{secret}"), which would copy
            // the key into the message and send it wherever that data goes.
            match template.matches("{secret}").count() {
                1 => {}
                0 => return fail(FailKind::Value, "`fill_secret` template has no {secret} placeholder"),
                n => {
                    return fail(
                        FailKind::Value,
                        format!(
                            "`fill_secret` template contains {{secret}} {n} times; it must appear exactly once, \
                             or text from outside could copy the key into a message"
                        ),
                    )
                }
            }
            if template.len().saturating_add(key.value.len()) > MAX_TEXT_BYTES {
                return fail(FailKind::Value, format!("text would be longer than {MAX_TEXT_BYTES} bytes"));
            }
            let placeholder = format!("{{secret {}}}", key.name);
            Ok(Value::Secret(Secret {
                name: key.name.clone(),
                value: template.replace("{secret}", &key.value),
                shown: template.replace("{secret}", &key.shown.replace("{secret}", &placeholder)),
            }))
        }
        (Builtin::Replace, [Value::Text(t), Value::Text(from), Value::Text(to)]) => {
            if from.is_empty() {
                return fail(FailKind::Value, "`replace` needs something to replace");
            }
            // The result can only grow when `to` is longer: bound it before building it.
            let worst = (t.len() / from.len()).saturating_mul(to.len().saturating_sub(from.len())).saturating_add(t.len());
            if worst > MAX_TEXT_BYTES {
                return fail(FailKind::Value, format!("text would be longer than {MAX_TEXT_BYTES} bytes"));
            }
            Ok(Value::Text(t.replace(from.as_str(), to)))
        }
        // Lowercasing can lengthen a text (some letters become two), so it is checked.
        (Builtin::Lower, [Value::Text(t)]) => {
            let lower = t.to_lowercase();
            if lower.len() > MAX_TEXT_BYTES {
                return fail(FailKind::Value, format!("lower would give a text longer than {MAX_TEXT_BYTES} bytes"));
            }
            Ok(Value::Text(lower))
        }
        (Builtin::UrlSegment, [Value::Text(t)]) => {
            if t.is_empty() || t == "." || t == ".." {
                return fail(FailKind::Value, format!("{t:?} cannot be a URL path segment"));
            }
            let seg = form_escape(t);
            if seg.len() > MAX_TEXT_BYTES {
                return fail(FailKind::Value, format!("url_segment would give a text longer than {MAX_TEXT_BYTES} bytes"));
            }
            Ok(Value::Text(seg))
        }
        (Builtin::Trim, [Value::Text(t)]) => Ok(Value::Text(t.trim().to_string())),
        (Builtin::Text, [Value::Text(t)]) => Ok(Value::Text(t.clone())),
        (Builtin::Text, [Value::Int(n)]) => Ok(Value::Text(n.to_string())),
        (Builtin::Text, [Value::Bool(b)]) => Ok(Value::Text(b.to_string())),
        (Builtin::Text, [Value::Money(m)]) => Ok(Value::Text(m.to_string())),
        (Builtin::Form, [Value::Record { fields, .. }]) => {
            let mut parts = Vec::with_capacity(fields.len());
            for (k, v) in fields {
                let text = match v {
                    Value::Text(t) => t.clone(),
                    Value::Int(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => return internal("`form` field that is not Text, Int or Bool"),
                };
                parts.push(format!("{}={}", form_escape(k), form_escape(&text)));
            }
            let out = parts.join("&");
            if out.len() > MAX_TEXT_BYTES {
                return fail(FailKind::Value, format!("form would be longer than {MAX_TEXT_BYTES} bytes"));
            }
            Ok(Value::Text(out))
        }
        (Builtin::MinorUnits, [Value::Money(m)]) => Ok(Value::Int(m.minor)),
        (Builtin::Currency, [Value::Money(m)]) => Ok(Value::Text(m.currency.code.to_string())),
        (Builtin::Json, [v]) => {
            let text = to_json(v, true).map_err(|e| Failure { kind: FailKind::Internal, message: e })?.to_string();
            if text.len() > MAX_TEXT_BYTES {
                return fail(FailKind::Value, format!("json would be longer than {MAX_TEXT_BYTES} bytes"));
            }
            Ok(Value::Text(text))
        }
        _ => internal(format!("`{}` applied to values the compiler should have rejected", f.name())),
    }
}

/// A map literal from its keys and values, alternating, in the order written. A key
/// given twice is a failure: which value was meant cannot be known.
pub(crate) fn map_literal(vals: Vec<Value>) -> Result<Value, Failure> {
    let mut es: Vec<(Value, Value)> = Vec::with_capacity(vals.len() / 2);
    let mut it = vals.into_iter();
    while let (Some(k), Some(v)) = (it.next(), it.next()) {
        match es.binary_search_by(|(x, _)| key_order(x, &k)) {
            Ok(_) => return fail(FailKind::Value, format!("key {} appears twice in a map literal", shown(&k))),
            Err(i) => es.insert(i, (k, v)),
        }
    }
    Ok(Value::Map(es))
}

/// A map key or set item as a message shows it: as JSON, so text is quoted.
fn shown(v: &Value) -> String {
    to_json(v, false).map(|j| j.to_string()).unwrap_or_else(|_| "this value".into())
}

/// Percent-encodes for application/x-www-form-urlencoded: only unreserved characters
/// (letters, digits, `-._~`) stay as they are; every other byte becomes `%XX`.
fn form_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Evaluates a constant's value when the program is checked (`crate::consts`). A
/// constant holds only the pure forms below, which mean exactly what they mean in
/// `Interp::eval`: the same helpers compute them, so a constant's value here is the
/// value the engines compute where it is used.
pub(crate) fn eval_const(e: &Expr, prog: &Program) -> Result<Value, Failure> {
    eval_pure(e, &types_of(prog))
}

fn eval_pure(e: &Expr, types: &Types) -> Result<Value, Failure> {
    let ev = |x: &Expr| eval_pure(x, types);
    match e {
        Expr::Str(s, _) => Ok(Value::Text(s.clone())),
        Expr::Bool(b, _) => Ok(Value::Bool(*b)),
        Expr::Money(m, _) => Ok(Value::Money(*m)),
        Expr::Int(n, _) => match i64::try_from(*n) {
            Ok(n) => Ok(Value::Int(n)),
            Err(_) => internal(format!("integer literal {n} is out of range")),
        },
        Expr::Arith { op, left, right, .. } => {
            let l = ev(left)?;
            arith(*op, l, ev(right)?)
        }
        Expr::Neg(inner, _) => match ev(inner)? {
            Value::Int(n) => neg(n).map(Value::Int),
            _ => internal("`-` applied to a value that is not an Int"),
        },
        Expr::Not(inner, _) => match ev(inner)? {
            Value::Bool(b) => Ok(Value::Bool(!b)),
            _ => internal("`not` on a non-Bool"),
        },
        Expr::Logic { op, left, right, .. } => {
            let Value::Bool(l) = ev(left)? else { return internal("`and`/`or` on a non-Bool") };
            if (*op == LogicOp::And && !l) || (*op == LogicOp::Or && l) {
                return Ok(Value::Bool(l));
            }
            match ev(right)? {
                Value::Bool(r) => Ok(Value::Bool(r)),
                _ => internal("`and`/`or` on a non-Bool"),
            }
        }
        Expr::Cmp { op, left, right, .. } => {
            let l = ev(left)?;
            compare(*op, &l, &ev(right)?).map(Value::Bool)
        }
        Expr::Builtin { f, args, .. } => {
            let vals = args.iter().map(ev).collect::<Result<Vec<_>, _>>()?;
            builtin(*f, &vals)
        }
        Expr::NoneLit(_) => Ok(Value::Opt(None)),
        Expr::SomeOf(inner, _) => Ok(Value::Opt(Some(Box::new(ev(inner)?)))),
        Expr::Record { ty, fields, .. } => {
            let Some(decl) = types.get(ty) else { return internal(format!("unknown record type {ty}")) };
            let mut given = Vec::with_capacity(fields.len());
            for (name, e) in fields {
                given.push((name.clone(), ev(e)?));
            }
            let mut out = Vec::with_capacity(decl.len());
            for (fname, _) in decl {
                match given.iter().position(|(n, _)| n == fname) {
                    Some(i) => out.push(given.swap_remove(i)),
                    None => return internal(format!("{ty} literal without field '{fname}'")),
                }
            }
            Ok(Value::Record { ty: ty.clone(), fields: out })
        }
        Expr::List(items, _) => Ok(Value::List(items.iter().map(ev).collect::<Result<Vec<_>, _>>()?)),
        Expr::Variant { ty, variant, fields, .. } => {
            let decl = types.variants(ty).and_then(|vs| vs.iter().find(|(v, _)| v == variant)).map(|(_, f)| f.clone());
            let Some(decl) = decl else { return internal(format!("unknown variant {ty}.{variant}")) };
            let mut given = Vec::with_capacity(fields.len());
            for (name, e) in fields {
                given.push((name.clone(), ev(e)?));
            }
            let mut out = Vec::with_capacity(decl.len());
            for (fname, _) in &decl {
                match given.iter().position(|(n, _)| n == fname) {
                    Some(i) => out.push(given.swap_remove(i)),
                    None => return internal(format!("{ty}.{variant} without field '{fname}'")),
                }
            }
            Ok(Value::Enum { ty: ty.clone(), variant: variant.clone(), fields: out })
        }
        Expr::Index { base, index, .. } => {
            let Value::List(items) = ev(base)? else { return internal("indexing a non-list") };
            let Value::Int(i) = ev(index)? else { return internal("a non-Int index") };
            list_index(&items, i)
        }
        Expr::Field { base, field, .. } => match ev(base)? {
            Value::Record { fields, .. } => match fields.into_iter().find(|(f, _)| f == field) {
                Some((_, v)) => Ok(v),
                None => internal(format!("no field '{field}'")),
            },
            Value::Tuple(items) => tuple_item(items, field),
            _ => internal(format!("cannot read '.{field}' of this value")),
        },
        Expr::Tuple(items, _) => Ok(Value::Tuple(items.iter().map(ev).collect::<Result<_, _>>()?)),
        Expr::MapLit(entries, _) => {
            let mut vals = Vec::with_capacity(entries.len() * 2);
            for (k, v) in entries {
                vals.push(ev(k)?);
                vals.push(ev(v)?);
            }
            map_literal(vals)
        }
        _ => internal("this is not something a constant can hold"),
    }
}

pub(crate) fn compare(op: CmpOp, l: &Value, r: &Value) -> Result<bool, Failure> {
    use std::cmp::Ordering;
    let ord: Option<Ordering> = match (l, r) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Money(a), Value::Money(b)) if a.currency == b.currency => Some(a.minor.cmp(&b.minor)),
        // Different currencies: never equal, never ordered (RUNTIME.md section 2).
        (Value::Money(_), Value::Money(_)) => {
            return Ok(op == CmpOp::Ne);
        }
        _ => None,
    };
    match (op, ord) {
        (CmpOp::Eq, _) => Ok(l == r),
        (CmpOp::Ne, _) => Ok(l != r),
        (CmpOp::Lt, Some(o)) => Ok(o == Ordering::Less),
        (CmpOp::Le, Some(o)) => Ok(o != Ordering::Greater),
        (CmpOp::Gt, Some(o)) => Ok(o == Ordering::Greater),
        (CmpOp::Ge, Some(o)) => Ok(o != Ordering::Less),
        _ => internal(format!("cannot order these values with `{op}`")),
    }
}

/// How many receipts a value leads to, by the same rules as `flatten_evidence`.
fn receipt_count(v: &Value) -> usize {
    let mut out = Vec::new();
    flatten_evidence(v, &mut out);
    out.len()
}

/// Receipts reachable from a Done: directly, through successful outcomes, and
/// through nested Done values.
/// Element `field` (its position, as text) of a tuple.
pub(crate) fn tuple_item(items: Vec<Value>, field: &str) -> Result<Value, Failure> {
    match field.parse::<usize>().ok().and_then(|i| items.into_iter().nth(i)) {
        Some(v) => Ok(v),
        None => internal(format!("a tuple has no element .{field}")),
    }
}

/// Values evaluated in the order written, put in parameter order: value `i` is for
/// parameter `slots[i]`. Empty `slots` means they are in order already.
fn in_slots(vals: Vec<Value>, slots: &[usize]) -> Result<Vec<Value>, Failure> {
    if slots.is_empty() {
        return Ok(vals);
    }
    let mut out: Vec<Option<Value>> = (0..vals.len()).map(|_| None).collect();
    for (v, &s) in vals.into_iter().zip(slots) {
        match out.get_mut(s) {
            Some(slot @ None) => *slot = Some(v),
            _ => return internal("a call's arguments do not fit its parameters"),
        }
    }
    out.into_iter().map(|v| v.ok_or_else(|| Failure { kind: FailKind::Internal, message: "a call's argument is missing".into() })).collect()
}

fn flatten_evidence(v: &Value, out: &mut Vec<Json>) {
    match v {
        Value::Receipt(r) => out.push(json!({ "receipt": r.id, "capability": r.capability, "method": r.method })),
        Value::Outcome { ok: true, held: Some(h), .. } => flatten_evidence(h, out),
        Value::Done(items) => items.iter().for_each(|i| flatten_evidence(i, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Records how many requests reach the provider, for counting and for completion.
    struct Recording {
        counted: Arc<AtomicUsize>,
        sent: Arc<AtomicUsize>,
    }

    impl Provider for Recording {
        fn count_input_tokens(&mut self, _req: &ModelRequest) -> Result<u64, ProviderError> {
            self.counted.fetch_add(1, Ordering::SeqCst);
            Ok(10)
        }
        fn complete(&mut self, _req: &ModelRequest) -> Result<provider::ModelReply, ProviderError> {
            self.sent.fetch_add(1, Ordering::SeqCst);
            Ok(provider::ModelReply { output: Ok(json!("hi")), input_tokens: 10, output_tokens: 5 })
        }
    }

    /// Parses without the static checks, to exercise the runtime's own defences
    /// against programs the compiler would reject.
    fn unchecked(src: &str) -> Program {
        let toks = crate::lexer::lex(src).expect("lex");
        crate::parser::Parser::new(toks).program().expect("parse")
    }

    #[test]
    fn a_call_with_no_budget_left_is_never_shown_to_the_provider() {
        // E004 rejects this program; the runtime must still refuse the second call,
        // and must not send its data to the provider even to count tokens.
        let prog = unchecked(
            "fn f() budget(calls: 1) {\n let a = propose(Text, tokens: 100)\n let b = propose(Text, tokens: 100)\n}",
        );
        let counted = Arc::new(AtomicUsize::new(0));
        let sent = Arc::new(AtomicUsize::new(0));
        let provider = Recording { counted: counted.clone(), sent: sent.clone() };
        let adapter = SimulatedAdapter::from_config(None).unwrap();
        let out = run_with(&prog, &json!({ "entry": "f" }), Box::new(provider), Box::new(adapter)).unwrap();
        let f = out.result.unwrap_err();
        assert_eq!(f.kind, FailKind::Budget, "{}", f.message);
        assert_eq!(sent.load(Ordering::SeqCst), 1, "only the first call may be sent");
        assert_eq!(counted.load(Ordering::SeqCst), 1, "the refused call must not be counted by the provider");
    }

    #[test]
    fn hollow_evidence_holds_no_receipt() {
        let receipt = Value::Receipt(Receipt { id: "r1".into(), capability: "email.send".into(), method: "send".into() });
        let ok = |held: Value| Value::Outcome { ok: true, error: String::new(), held: Some(Box::new(held)) };
        // Real proof: a receipt, directly or through an outcome or a nested Done.
        assert_eq!(receipt_count(&receipt), 1);
        assert_eq!(receipt_count(&ok(receipt.clone())), 1);
        assert_eq!(receipt_count(&Value::Done(vec![ok(receipt.clone()), receipt.clone()])), 2);
        // Not proof: an outcome of a pure call, a failed outcome, an empty Done.
        assert_eq!(receipt_count(&ok(Value::Int(1))), 0);
        assert_eq!(receipt_count(&ok(Value::Unit)), 0);
        assert_eq!(receipt_count(&Value::Outcome { ok: false, error: "x".into(), held: None }), 0);
        assert_eq!(receipt_count(&Value::Done(vec![])), 0);
    }

    #[test]
    fn only_program_level_failures_are_catchable() {
        for k in [FailKind::Action, FailKind::Capability, FailKind::Validation, FailKind::Budget, FailKind::Provider] {
            assert!(k.catchable(), "{k} should be catchable");
        }
        // Deployment limits and staalc bugs always end the run.
        for k in [FailKind::StepLimit, FailKind::DepthLimit, FailKind::Internal] {
            assert!(!k.catchable(), "{k} must not be catchable");
        }
    }
}
