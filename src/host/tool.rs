// A tool: a program's entry function, the run config every call starts from, and the
// bounds across calls (quota, `once`, concurrency, deadline). Defined in an MCP manifest
// (docs/MCP.md section 2) or through the C interface (include/staal.h), with the same
// keys and the same checks. Every program is checked when the tool is defined, and
// whatever can be checked then is: with grants fixed in the definition, the runtime
// checks the whole config with placeholder inputs; with grants given at each call (the
// C interface's tools, whose app computes them), the grants are checked at each call.

use crate::ast::Program;
use crate::interp::money::{Currency, Money};
use crate::interp::{self, Engine};
use serde_json::{json, Map, Value as Json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The name of the tool the MCP server adds to every manifest.
pub const STATUS_TOOL: &str = "staal_run_status";

pub(crate) const MS_PER_MINUTE: u64 = 60_000;
pub(crate) const MS_PER_DAY: u64 = 24 * 60 * MS_PER_MINUTE;

/// Where and how a host keeps its runs: `server` in an MCP manifest, or the settings
/// given to `staal_host_open`.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// What the settings are called in messages: `server` in a manifest.
    pub at: String,
    /// Absolute, with every link resolved.
    pub journals: PathBuf,
    /// How long a settled run's files are kept, in milliseconds; None keeps them for ever.
    pub keep_ms: Option<u64>,
    pub max_concurrent_runs: usize,
    pub notify: Option<Vec<String>>,
    pub notify_env: Vec<String>,
}

/// Who defines a tool, which decides what its definition may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Definer {
    /// An MCP manifest: grants written in it, and no app to give inputs.
    Manifest,
    /// An app, through the C interface: its grants are fixed in the definition or
    /// given with each call, and it may give app inputs with each call.
    App(Grants),
}

/// Where a tool's grants come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grants {
    /// Written in the tool's definition (an MCP manifest): the same for every call.
    Fixed,
    /// Given with each call by the app, which computes them from what it knows of the
    /// request (the C interface's tools).
    PerCall,
}

#[derive(Debug)]
pub struct Tool {
    pub name: String,
    pub grants: Grants,
    pub title: Option<String>,
    pub description: String,
    pub program: PathBuf,
    pub prog: Arc<Program>,
    /// Identifies the exact program text; every journal of the tool is bound to it.
    pub fingerprint: String,
    /// The run config every call starts from: everything but `inputs`.
    pub base: Map<String, Json>,
    pub engine: Engine,
    /// Inputs the app gives with each call, never a caller: they are not in
    /// `input_schema`, and a call whose arguments name one is refused.
    pub app_inputs: Vec<String>,
    /// MCP's `inputSchema` and `outputSchema`.
    pub input_schema: Json,
    pub output_schema: Json,
    pub annotations: Json,
    pub max_concurrent_runs: Option<usize>,
    pub once: Option<Once>,
    pub quota: Option<Quota>,
    /// What one run reserves against `quota.money`: the most it can be charged. For
    /// grants given per call, worked out at each call instead.
    pub reserve: Option<Money>,
    /// The environment variables the notify command may see, which no grant may use.
    pub notify_env: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Once {
    pub by: Vec<String>,
    /// How long after a run ends its key is remembered; None: as long as its files.
    pub keep_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Quota {
    pub window_ms: u64,
    pub calls: Option<u64>,
    pub money: Option<Money>,
}

impl Tool {
    /// The run config of one call: the caller's arguments and the app's inputs, each
    /// exactly as the tool declares them. Grants are given exactly when the tool takes
    /// them per call.
    pub fn config(
        &self,
        arguments: &Map<String, Json>,
        app_inputs: Option<&Map<String, Json>>,
        grants: Option<&Map<String, Json>>,
    ) -> Result<Json, String> {
        // A caller (a model, often) cannot supply what only the app may.
        if let Some(k) = arguments.keys().find(|k| self.app_inputs.contains(k)) {
            return Err(format!("inputs.{k}: given by the app, never by the caller"));
        }
        let mut inputs = arguments.clone();
        match app_inputs {
            None if self.app_inputs.is_empty() => {}
            None => {
                return Err(format!(
                    "tool '{}' takes app inputs ({}) with each call, and this call gave none",
                    self.name,
                    self.app_inputs.join(", ")
                ))
            }
            Some(_) if self.app_inputs.is_empty() => {
                return Err(format!("tool '{}' takes no app inputs; its definition names none in \"app_inputs\"", self.name))
            }
            Some(given) => {
                if let Some(k) = self.app_inputs.iter().find(|k| !given.contains_key(*k)) {
                    return Err(format!("app inputs: '{k}' missing"));
                }
                if let Some(k) = given.keys().find(|k| !self.app_inputs.contains(k)) {
                    return Err(format!("app inputs: '{}' is not one of this tool's app inputs", crate::clip::clip(k)));
                }
                inputs.extend(given.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
        let mut c = self.base.clone();
        match (self.grants, grants) {
            (Grants::Fixed, None) => {}
            (Grants::Fixed, Some(_)) => return Err(format!("tool '{}' has its grants in its definition; a call cannot give any", self.name)),
            (Grants::PerCall, Some(g)) => {
                c.insert("grants".into(), Json::Object(g.clone()));
            }
            (Grants::PerCall, None) => return Err(format!("tool '{}' takes its grants with each call, and this call gave none", self.name)),
        }
        c.insert("inputs".into(), Json::Object(inputs));
        Ok(Json::Object(c))
    }

    /// For grants given per call, the checks the definition could not make, on the
    /// runtime's plan of one call; gives what the run reserves against the money quota.
    pub fn check_call(&self, plan: &interp::Plan, config: &Json) -> Result<Option<Money>, String> {
        if self.grants == Grants::Fixed {
            return Ok(self.reserve);
        }
        let mut used = Vec::new();
        env_names(&config["grants"], &mut used);
        if let Some(v) = self.notify_env.iter().find(|v| used.contains(v)) {
            return Err(format!("grants: '{v}' is one of the notify command's variables, which never hold a secret"));
        }
        let base = config.as_object().cloned().unwrap_or_default();
        check_grants(plan, &base, "grants")?;
        reserve(plan, self.quota.as_ref(), "grants")
    }
}

/// What grants may not do in a tool: wait for a connection with no time limit, or
/// reach the network without timeouts given. Every wait in a tool has a bound.
fn check_grants(plan: &interp::Plan, base: &Map<String, Json>, at: &str) -> Result<(), String> {
    let all_paths: Vec<&str> =
        plan.grants.iter().flat_map(|g| std::iter::once(g.path.as_str()).chain(g.needs.iter().map(String::as_str))).collect();
    if let Some(g) = plan.grants.iter().find(|g| g.waits_unbounded) {
        return Err(format!(
            "{at}.{}: net.tcp.accept in a tool needs \"wait_ms\": without it an accept waits for a connection with no time limit",
            g.name
        ));
    }
    // Every wait has a bound: in a tool the timeouts are given, not defaulted.
    if all_paths.iter().any(|p| reaches_network(p)) {
        let net = base.get("net").and_then(Json::as_object);
        for k in ["connect_timeout_ms", "timeout_ms"] {
            if !net.is_some_and(|n| n.contains_key(k)) {
                let at = at.strip_suffix(".grants").or_else(|| at.strip_suffix("grants")).unwrap_or(at);
                let dot = if at.is_empty() { "" } else { "." };
                return Err(format!("{at}{dot}net.{k}: required in a tool that reaches the network"));
            }
        }
    }
    Ok(())
}

/// What one run reserves against the money quota: the most its grants let it be
/// charged, which every grant that can spend money must bound.
fn reserve(plan: &interp::Plan, quota: Option<&Quota>, at: &str) -> Result<Option<Money>, String> {
    let Some(limit) = quota.and_then(|q| q.money) else { return Ok(None) };
    let mut total = Money { minor: 0, currency: limit.currency };
    for g in plan.grants.iter().filter(|g| g.spends) {
        let w = g.worst.ok_or(format!(
            "{at}.{}: can spend money, so with a money quota it needs \"max\", or \"each\" and \"uses\"",
            g.name
        ))?;
        if w.currency != limit.currency {
            return Err(format!("{at}.{}: is limited in {}, but the money quota is in {}", g.name, w.currency.code, limit.currency.code));
        }
        total.minor = total.minor.checked_add(w.minor).ok_or(format!("{at}: the grants' limits add up to too much"))?;
    }
    if total.minor > limit.minor {
        return Err(format!("{at}: one run can spend up to {total}, more than the whole money quota of {limit}, so no call could start"));
    }
    Ok(Some(total))
}

pub(crate) fn keys_only(o: &Map<String, Json>, at: &str, allowed: &[&str]) -> Result<(), String> {
    for k in o.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(format!("{at}: unknown key '{k}'"));
        }
    }
    Ok(())
}

pub(crate) fn text(o: &Map<String, Json>, key: &str, at: &str) -> Result<Option<String>, String> {
    match o.get(key) {
        None => Ok(None),
        Some(Json::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(format!("{at}.{key}: expected a string that is not empty")),
    }
}

pub(crate) fn whole(o: &Map<String, Json>, key: &str, at: &str, lo: u64, hi: u64) -> Result<Option<u64>, String> {
    match o.get(key) {
        None => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if (lo..=hi).contains(&n) => Ok(Some(n)),
            _ => Err(format!("{at}.{key}: expected a whole number from {lo} to {hi}")),
        },
    }
}

/// `"90s"`, `"15m"`, `"24h"`, `"30d"`: a positive whole number and a unit from `units`.
fn duration(v: &Json, at: &str, units: &[&str]) -> Result<u64, String> {
    let bad = || format!("{at}: expected a whole number and a unit ({}), e.g. \"{}\"", units.join(", "), example(units));
    let s = v.as_str().ok_or_else(bad)?;
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let (n, unit) = s.split_at(digits);
    if n.is_empty() || n.starts_with('0') || !units.contains(&unit) {
        return Err(bad());
    }
    let n: u64 = n.parse().map_err(|_| bad())?;
    let per = match unit {
        "s" => 1_000,
        "m" => MS_PER_MINUTE,
        "h" => 60 * MS_PER_MINUTE,
        "d" => MS_PER_DAY,
        _ => return Err(bad()),
    };
    n.checked_mul(per).ok_or_else(|| format!("{at}: too long"))
}

fn example(units: &[&str]) -> &'static str {
    if units.contains(&"s") { "120s" } else { "24h" }
}

fn money(v: &Json, at: &str) -> Result<Money, String> {
    let o = v.as_object().ok_or(format!("{at}: expected {{\"amount\": \"...\", \"currency\": \"...\"}}"))?;
    keys_only(o, at, &["amount", "currency"])?;
    let code = o.get("currency").and_then(Json::as_str).ok_or(format!("{at}.currency: expected a currency code"))?;
    let amount = o.get("amount").and_then(Json::as_str).ok_or(format!("{at}.amount: expected a decimal string"))?;
    Money::parse(amount, Currency::parse(code).map_err(|e| format!("{at}.currency: {e}"))?).map_err(|e| format!("{at}.amount: {e}"))
}

/// Every environment variable a config names (`{"env": "NAME"}`, at any depth).
pub(crate) fn env_names(v: &Json, out: &mut Vec<String>) {
    match v {
        Json::Object(o) => {
            if let Some(Json::String(n)) = o.get("env") {
                out.push(n.clone());
            }
            o.values().for_each(|x| env_names(x, out));
        }
        Json::Array(a) => a.iter().for_each(|x| env_names(x, out)),
        _ => {}
    }
}

fn valid_tool_name(n: &str) -> bool {
    (1..=128).contains(&n.len())
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        && n.bytes().any(|b| b != b'.')
}

/// Makes `dir` if missing (mode 0700, as each missing parent), then checks it is a
/// directory owned by this user that no one else can write, and returns it with every
/// link resolved. A journal decides what a resumed run replays, so whoever could write
/// one could make a run remember things that never happened.
pub fn private_dir(dir: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let real = std::fs::canonicalize(dir).map_err(|e| format!("cannot resolve {}: {e}", dir.display()))?;
    let md = std::fs::metadata(&real).map_err(|e| format!("cannot read {}: {e}", real.display()))?;
    if !md.is_dir() {
        return Err(format!("{} is not a directory", real.display()));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if md.uid() != me {
        return Err(format!("{} belongs to user {}, not to the user the server runs as ({me})", real.display(), md.uid()));
    }
    if md.mode() & 0o022 != 0 {
        return Err(format!(
            "{} can be written by its group or by others (mode {:o}); make it 0700 (chmod 700)",
            real.display(),
            md.mode() & 0o777
        ));
    }
    Ok(real)
}

/// Reads where and how a host keeps its runs. `at` names the object in messages
/// (`server` in a manifest); paths are relative to `dir`.
pub fn parse_settings(o: &Map<String, Json>, at: &str, dir: &Path, extra: &[&str]) -> Result<Settings, String> {
    let mut allowed = vec!["journals", "keep", "max_concurrent_runs", "notify", "notify_env"];
    allowed.extend_from_slice(extra);
    keys_only(o, at, &allowed)?;
    let journals = text(o, "journals", at)?.ok_or(format!("{at}.journals: missing"))?;
    let journals = private_dir(&dir.join(journals)).map_err(|e| format!("{at}.journals: {e}"))?;
    let keep_ms = match o.get("keep") {
        None => Some(90 * MS_PER_DAY),
        Some(Json::String(s)) if s == "forever" => None,
        Some(v) => match v.as_u64() {
            Some(d) if (1..=36_500).contains(&d) => Some(d * MS_PER_DAY),
            _ => return Err(format!("{at}.keep: expected a whole number of days from 1 to 36500, or \"forever\"")),
        },
    };
    let max_concurrent_runs = whole(o, "max_concurrent_runs", at, 1, 256)?.unwrap_or(8) as usize;
    let notify = match o.get("notify") {
        None => None,
        Some(v) => {
            let a = v.as_array().filter(|a| !a.is_empty()).ok_or(format!("{at}.notify: expected a list: the program, then its arguments"))?;
            let mut cmd = Vec::new();
            for (i, x) in a.iter().enumerate() {
                let s = x.as_str().filter(|s| !s.is_empty() && !s.contains('\0'));
                cmd.push(s.ok_or(format!("{at}.notify[{i}]: expected a string that is not empty"))?.to_string());
            }
            // Resolved now, like every path.
            let prog = Path::new(&cmd[0]);
            let prog = if prog.is_absolute() { prog.to_path_buf() } else { dir.join(prog) };
            let real = std::fs::canonicalize(&prog).map_err(|e| format!("{at}.notify[0]: {}: {e}", prog.display()))?;
            cmd[0] = real.to_string_lossy().into_owned();
            Some(cmd)
        }
    };
    let mut notify_env = Vec::new();
    if let Some(v) = o.get("notify_env") {
        if notify.is_none() {
            return Err(format!("{at}.notify_env: there is no {at}.notify command for it"));
        }
        let a = v.as_array().ok_or(format!("{at}.notify_env: expected a list of variable names"))?;
        for (i, x) in a.iter().enumerate() {
            let n = x.as_str().filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
            let n = n.ok_or(format!("{at}.notify_env[{i}]: expected a variable name (letters, digits, _)"))?;
            if ["PATH", "HOME", "LANG", "TZ"].contains(&n) {
                return Err(format!("{at}.notify_env[{i}]: '{n}' is always given; leave it out"));
            }
            notify_env.push(n.to_string());
        }
    }
    Ok(Settings { at: at.to_string(), journals, keep_ms, max_concurrent_runs, notify, notify_env })
}

/// The runtime capabilities that only read.
const READS: [&str; 4] = ["net.http.get", "fs.read", "db.sqlite.query", "db.postgres.query"];

fn reaches_network(path: &str) -> bool {
    path.starts_with("net.") || path.starts_with("db.postgres.")
}

/// Reads and checks one tool's definition. `at` names it in messages; paths are
/// relative to `dir`; `model` is the model every tool uses unless it names its own.
pub fn parse_tool(
    v: &Json,
    at: &str,
    dir: &Path,
    model: Option<&Json>,
    settings: &Settings,
    definer: Definer,
    warnings: &mut Vec<String>,
) -> Result<Tool, String> {
    let (grants, app_inputs_allowed) = match definer {
        Definer::Manifest => (Grants::Fixed, false),
        Definer::App(g) => (g, true),
    };
    let o = v.as_object().ok_or(format!("{at}: expected an object"))?;
    const RUN_KEYS: [&str; 7] = ["grants", "secrets", "model", "net", "fs", "adapters", "limits"];
    for k in o.keys() {
        if k == "inputs" {
            return Err(format!("{at}.inputs: not allowed; a tool's inputs come from each call"));
        }
        if k == "grants" && grants == Grants::PerCall {
            return Err(format!("{at}.grants: not allowed; this tool's grants come with each call"));
        }
        if k == "app_inputs" && !app_inputs_allowed {
            return Err(format!(
                "{at}.app_inputs: not allowed in a manifest; app inputs are given with each call by an app \
                 (the C interface, Python and Node.js), and a manifest's tools have no app"
            ));
        }
        let own = [
            "name",
            "title",
            "description",
            "program",
            "entry",
            "engine",
            "deadline",
            "max_concurrent_runs",
            "once",
            "quota",
            "app_inputs",
        ];
        if !own.contains(&k.as_str()) && !RUN_KEYS.contains(&k.as_str()) {
            return Err(format!("{at}: unknown key '{k}'"));
        }
    }
    let name = o.get("name").and_then(Json::as_str).ok_or(format!("{at}.name: missing"))?.to_string();
    if !valid_tool_name(&name) {
        return Err(format!("{at}.name: '{name}' must be 1 to 128 characters of A-Z a-z 0-9 _ - ., not only dots"));
    }
    if name == STATUS_TOOL {
        return Err(format!("{at}.name: '{STATUS_TOOL}' is the server's own tool"));
    }
    let at = format!("{at} ('{name}')");
    let at = at.as_str();
    let title = text(o, "title", at)?;
    let description = text(o, "description", at)?.ok_or(format!("{at}.description: missing; a model chooses tools by it"))?;
    let program = text(o, "program", at)?.ok_or(format!("{at}.program: missing"))?;
    let program = std::fs::canonicalize(dir.join(&program)).map_err(|e| format!("{at}.program: {program}: {e}"))?;
    let entry = text(o, "entry", at)?.unwrap_or_else(|| "main".into());
    let engine = match o.get("engine").map(|e| e.as_str()) {
        None | Some(Some("interpreter")) => Engine::Interpreter,
        Some(Some("native")) => {
            if !cfg!(feature = "native") {
                return Err(format!("{at}.engine: this staalc was built without native code"));
            }
            Engine::Native
        }
        Some(_) => return Err(format!("{at}.engine: expected \"interpreter\" or \"native\"")),
    };

    // The program, checked and held: a file changed while the server runs is never used.
    let checked = crate::check_path(&program).map_err(|e| format!("{at}.program: {e}"))?;
    let prog = match checked.result {
        Ok(p) => p,
        Err(diags) => {
            let shown: Vec<String> = diags.iter().take(10).map(|d| crate::format_diag_in(&checked.files, d)).collect();
            return Err(format!("{at}.program: {} has check errors:\n{}", program.display(), shown.join("\n")));
        }
    };
    let interface = crate::runconfig::interface(&prog, Some(&entry)).map_err(|e| format!("{at}.entry: {e}"))?;
    // A function that takes no capability needs no grants, from anywhere.
    let grants = if interface["grants"].as_array().is_some_and(|g| g.is_empty()) { Grants::Fixed } else { grants };
    if interface["reads_console"] == true {
        return Err(format!(
            "{at}: '{entry}' can reach read_line(); a tool is called by a program, not a person, and cannot read the console"
        ));
    }

    // The run config every call starts from.
    let mut base = Map::new();
    base.insert("entry".into(), json!(entry));
    for k in RUN_KEYS {
        if let Some(v) = o.get(k) {
            base.insert(k.into(), v.clone());
        }
    }
    if !base.contains_key("model")
        && let Some(m) = model
    {
        base.insert("model".into(), m.clone());
    }
    if let Some(l) = base.get("limits").and_then(Json::as_object)
        && l.contains_key("deadline_ms")
    {
        return Err(format!("{at}.limits.deadline_ms: give the tool's \"deadline\" instead"));
    }
    if let Some(d) = o.get("deadline") {
        let ms = duration(d, &format!("{at}.deadline"), &["s", "m", "h"])?;
        let limits = base.entry("limits").or_insert_with(|| json!({}));
        let Some(l) = limits.as_object_mut() else { return Err(format!("{at}.limits: expected an object")) };
        l.insert("deadline_ms".into(), json!(ms));
    }

    // With fixed grants, the runtime checks the whole config, with placeholder inputs,
    // as a run would. With grants per call, that waits for each call.
    let plan = match grants {
        Grants::Fixed => {
            let skeleton = crate::runconfig::skeleton(&prog, Some(&entry)).map_err(|e| format!("{at}: {e}"))?;
            let placeholders = skeleton["inputs"].as_object().cloned().unwrap_or_default();
            let mut probe = base.clone();
            probe.insert("inputs".into(), Json::Object(placeholders));
            let plan = interp::plan(&prog, &Json::Object(probe)).map_err(|e| format!("{at}: {e}"))?;
            check_grants(&plan, &base, &format!("{at}.grants"))?;
            Some(plan)
        }
        Grants::PerCall => None,
    };
    let all_paths: Vec<&str> = plan
        .iter()
        .flat_map(|p| p.grants.iter())
        .flat_map(|g| std::iter::once(g.path.as_str()).chain(g.needs.iter().map(String::as_str)))
        .collect();
    let uses_model = crate::runconfig::reaches_model(&prog, Some(&entry))?;
    if uses_model {
        let m = base.get("model").and_then(Json::as_object).ok_or(format!(
            "{at}: '{entry}' can call a model; give \"model\" in the tool or at the top of the manifest"
        ))?;
        if m.get("provider").and_then(Json::as_str) != Some("script") {
            for k in ["timeout_ms", "retries"] {
                if !m.contains_key(k) {
                    return Err(format!("{at}.model.{k}: required in a tool that can call a model"));
                }
            }
        }
    }

    let max_concurrent_runs = whole(o, "max_concurrent_runs", at, 1, 256)?.map(|n| n as usize);
    if let Some(n) = max_concurrent_runs
        && n > settings.max_concurrent_runs
    {
        return Err(format!("{at}.max_concurrent_runs: {n} is more than {}.max_concurrent_runs ({})", settings.at, settings.max_concurrent_runs));
    }

    let props = interface["input"]["properties"].as_object().cloned().unwrap_or_default();
    let mut app_inputs: Vec<String> = Vec::new();
    if let Some(v) = o.get("app_inputs") {
        let aa = format!("{at}.app_inputs");
        let list = v.as_array().filter(|a| !a.is_empty()).ok_or(format!("{aa}: expected a list of at least one input name"))?;
        for (i, x) in list.iter().enumerate() {
            let n = x.as_str().ok_or(format!("{aa}[{i}]: expected an input name"))?;
            if !props.contains_key(n) {
                return Err(format!("{aa}[{i}]: '{entry}' has no input '{n}' (capabilities and secrets are never inputs)"));
            }
            if app_inputs.iter().any(|a| a == n) {
                return Err(format!("{aa}[{i}]: '{n}' is named twice"));
            }
            app_inputs.push(n.to_string());
        }
    }
    let once = match o.get("once") {
        None => None,
        Some(v) => {
            let oa = format!("{at}.once");
            let oo = v.as_object().ok_or(format!("{oa}: expected an object"))?;
            keys_only(oo, &oa, &["by", "keep"])?;
            let list = oo.get("by").and_then(Json::as_array).filter(|a| !a.is_empty());
            let list = list.ok_or(format!("{oa}.by: expected a list of at least one argument name"))?;
            let mut by = Vec::new();
            for (i, x) in list.iter().enumerate() {
                let n = x.as_str().ok_or(format!("{oa}.by[{i}]: expected an argument name"))?;
                if !props.contains_key(n) {
                    return Err(format!("{oa}.by[{i}]: '{entry}' has no input '{n}'"));
                }
                if by.iter().any(|b| b == n) {
                    return Err(format!("{oa}.by[{i}]: '{n}' is named twice"));
                }
                by.push(n.to_string());
            }
            let keep_ms = match oo.get("keep") {
                None => settings.keep_ms,
                Some(k) => {
                    let ms = duration(k, &format!("{oa}.keep"), &["m", "h", "d"])?;
                    if settings.keep_ms.is_some_and(|s| ms > s) {
                        return Err(format!("{oa}.keep: longer than {}.keep; a key cannot outlive its run's files", settings.at));
                    }
                    Some(ms)
                }
            };
            Some(Once { by, keep_ms })
        }
    };

    let quota = match o.get("quota") {
        None => None,
        Some(v) => {
            let qa = format!("{at}.quota");
            let qo = v.as_object().ok_or(format!("{qa}: expected an object"))?;
            keys_only(qo, &qa, &["window", "calls", "money"])?;
            let w = qo.get("window").ok_or(format!("{qa}.window: missing"))?;
            let window_ms = duration(w, &format!("{qa}.window"), &["m", "h", "d"])?;
            if window_ms > 30 * MS_PER_DAY {
                return Err(format!("{qa}.window: at most 30d"));
            }
            let calls = whole(qo, "calls", &qa, 1, u32::MAX as u64)?;
            let money = qo.get("money").map(|m| money(m, &format!("{qa}.money"))).transpose()?;
            if calls.is_none() && money.is_none() {
                return Err(format!("{qa}: give \"calls\", \"money\" or both"));
            }
            Some(Quota { window_ms, calls, money })
        }
    };

    // What one run may be charged at most, for the money quota.
    let spending: Vec<&interp::GrantPlan> = plan.iter().flat_map(|p| p.grants.iter()).filter(|g| g.spends).collect();
    let reserve = match &plan {
        Some(p) => reserve(p, quota.as_ref(), &format!("{at}.grants"))?,
        None => None,
    };
    if all_paths.iter().any(|p| p.starts_with("ffi.")) {
        warnings.push(format!(
            "tool '{name}' calls C code, which no timeout or deadline can stop: a call that never returns holds its \
             concurrency slot and its reservation until the process is restarted"
        ));
    }
    if !spending.is_empty() && once.is_none() {
        warnings.push(format!(
            "tool '{name}' can spend money and has no \"once\": a call sent again is a new run, which can spend again"
        ));
    }

    // Annotations, derived from the grants and never claiming more than the broker
    // enforces. Grants given per call are unknown here, so nothing is claimed for them.
    let read_only = plan.as_ref().is_some_and(|p| p.grants.iter().all(|g| !g.library && READS.contains(&g.path.as_str())));
    let mut annotations = Map::new();
    if let Some(t) = &title {
        annotations.insert("title".into(), json!(t));
    }
    annotations.insert("readOnlyHint".into(), json!(read_only));
    annotations.insert("destructiveHint".into(), json!(!read_only));
    // The same arguments give the same run only through `once`, and only while its key
    // is kept: a key kept for a time lets the same call run again after it, so only a
    // key kept for ever makes the tool idempotent. A run's idempotency keys are its
    // own, so they do not make a second call harmless.
    let once_for_ever = once.as_ref().is_some_and(|o| o.keep_ms.is_none());
    annotations.insert("idempotentHint".into(), json!(read_only || once_for_ever));
    let open = plan.is_none()
        || uses_model
        || all_paths.iter().any(|p| reaches_network(p) || p.starts_with("ffi."))
        || plan.iter().flat_map(|p| p.grants.iter()).any(|g| !g.library && !p_is_runtime(&g.path));
    annotations.insert("openWorldHint".into(), json!(open));

    let (mut input_schema, output_schema) = schemas(&interface);
    // What a caller sees: the inputs it may give, without the app's.
    if let Some(s) = input_schema.as_object_mut() {
        if let Some(p) = s.get_mut("properties").and_then(Json::as_object_mut) {
            p.retain(|k, _| !app_inputs.contains(k));
        }
        if let Some(r) = s.get_mut("required").and_then(Json::as_array_mut) {
            r.retain(|k| !k.as_str().is_some_and(|k| app_inputs.iter().any(|a| a == k)));
        }
    }
    let mut used = Vec::new();
    env_names(&Json::Object(base.clone()), &mut used);
    if let Some(m) = model {
        env_names(m, &mut used);
    }
    if let Some(v) = settings.notify_env.iter().find(|v| used.contains(v)) {
        return Err(format!("{at}: '{v}' is one of the notify command's variables, and holds a secret this tool uses"));
    }
    Ok(Tool {
        name,
        grants,
        title,
        description,
        program,
        prog: Arc::new(prog),
        fingerprint: checked.fingerprint,
        base,
        engine,
        app_inputs,
        input_schema,
        output_schema,
        annotations: Json::Object(annotations),
        max_concurrent_runs,
        once,
        quota,
        reserve,
        notify_env: settings.notify_env.clone(),
    })
}

fn p_is_runtime(path: &str) -> bool {
    path.starts_with("net.") || path.starts_with("fs.") || path.starts_with("db.") || path.starts_with("ffi.")
}

/// MCP's schemas from `staalc schema`'s. MCP requires an object for both, so the
/// result is given as `{"result": <value>}`, with the definitions at the top.
fn schemas(interface: &Json) -> (Json, Json) {
    let input = interface["input"].clone();
    let mut out = interface["output"].clone();
    let mut wrapper = Map::new();
    if let Some(o) = out.as_object_mut() {
        if let Some(s) = o.remove("$schema") {
            wrapper.insert("$schema".into(), s);
        }
        if let Some(d) = o.remove("$defs") {
            wrapper.insert("$defs".into(), d);
        }
    }
    wrapper.insert("type".into(), json!("object"));
    wrapper.insert("properties".into(), json!({ "result": out }));
    wrapper.insert("required".into(), json!(["result"]));
    wrapper.insert("additionalProperties".into(), json!(false));
    (input, Json::Object(wrapper))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_a_whole_number_and_a_unit() {
        let d = |s: &str, u: &[&str]| duration(&json!(s), "x", u);
        assert_eq!(d("120s", &["s", "m"]), Ok(120_000));
        assert_eq!(d("24h", &["m", "h", "d"]), Ok(86_400_000));
        assert_eq!(d("1m", &["m"]), Ok(60_000));
        for bad in ["", "0s", "01s", "5", "s", "5 s", "5S", "-5s", "1.5h", "5ms", "5d"] {
            assert!(d(bad, &["s", "m", "h"]).is_err(), "{bad}");
        }
        assert!(duration(&json!(5), "x", &["s"]).is_err());
        assert!(d("99999999999999999999d", &["d"]).is_err());
    }

    #[test]
    fn tool_names_cannot_be_dots_or_escape_a_folder() {
        for ok in ["refund_order", "a.b", "x-1", "A"] {
            assert!(valid_tool_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "a b", "ü", &"x".repeat(129)] {
            assert!(!valid_tool_name(bad), "{bad}");
        }
    }

    #[test]
    fn every_named_variable_is_found() {
        let mut out = Vec::new();
        env_names(&json!({ "a": { "env": "X" }, "b": [{ "c": { "env": "Y" } }], "d": "env" }), &mut out);
        out.sort();
        assert_eq!(out, ["X", "Y"]);
    }
}
