// `agent.json` (AGENT_MANIFEST.md section 4) and `staalc agent check` (section 6).
//
// `read` holds the file to its form: every key known, at every depth, and every value
// of its type, exactly as docs/schemas/agent.schema.json states it, plus the value rules
// a schema cannot state (an SPDX expression, a version range, money a currency can
// hold, a duration that fits). `check` then holds it to the project: each tool's program
// checks, its grants are exactly its entry's capability parameters and their needs, each
// suggestion fits the program's ceiling, each reach pattern is one a grant could carry,
// and the tool host would accept the tool. Money, durations, version ranges, tool names
// and patterns are read by the same code the runtime and the tool host use, so the two
// can never disagree on what one is.

pub mod describe;
pub mod spdx;
pub mod strict_json;

use crate::ast::{CapLimits, Program, Ty};
use crate::interp::broker;
use crate::interp::money::Money;
use serde_json::{json, Map, Value as Json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The version of this specification this `staalc` implements.
pub const MANIFEST_VERSION: &str = "0.1";

/// The categories an agent may name. A fixed list, so that every surface can group
/// agents the same way; a new category is a new version of this list.
pub const CATEGORIES: [&str; 16] = [
    "commerce",
    "communications",
    "customer-operations",
    "data",
    "education",
    "finance",
    "healthcare",
    "human-resources",
    "it-operations",
    "legal",
    "logistics",
    "marketing",
    "productivity",
    "sales",
    "security",
    "software-development",
];

/// The most tools one agent holds.
pub const MAX_TOOLS: usize = 64;

/// A placeholder in a reach pattern is replaced by this host to check the pattern; the
/// business supplies the real one at install.
const PLACEHOLDER_HOST: &str = "placeholder.invalid";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Suggested {
    pub max: Option<Money>,
    pub each: Option<Money>,
    pub uses: Option<u64>,
    pub only: Option<Vec<String>>,
}

/// What `agent.json` says of one grant (`grants.pay`) or one of its needs
/// (`grants.pay.needs.post`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GrantNode {
    pub suggested: Suggested,
    pub reach: Option<Vec<String>>,
    pub idempotency_key: Option<bool>,
    /// The private addresses (and cloud metadata services) a capability that connects
    /// may reach, if agent.json declares any (RUNTIME.md section 12, "Addresses").
    pub addresses: Option<crate::interp::addresses::Policy>,
    pub needs: BTreeMap<String, GrantNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub program: String,
    pub entry: String,
    pub app_inputs: Vec<String>,
    pub app_input_requirements: BTreeMap<String, String>,
    pub grants: BTreeMap<String, GrantNode>,
    pub once: Option<Json>,
    pub quota_suggested: Option<Json>,
    pub deadline: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub agent: String,
    pub version: String,
    pub requires_staalc: String,
    pub title: String,
    pub summary: String,
    pub publisher: (String, String, String),
    pub license: String,
    pub categories: Vec<String>,
    pub tools: Vec<ToolDef>,
    pub data: Json,
}

type Obj = Map<String, Json>;

fn object<'a>(v: &'a Json, at: &str) -> Result<&'a Obj, String> {
    v.as_object().ok_or_else(|| format!("{at}: expected an object"))
}

/// Refuses a key not in `allowed`, and a missing key in `required`.
fn keys(o: &Obj, at: &str, required: &[&str], optional: &[&str]) -> Result<(), String> {
    for k in o.keys() {
        if !required.contains(&k.as_str()) && !optional.contains(&k.as_str()) {
            return Err(format!("{at}: unknown key '{k}'"));
        }
    }
    for k in required {
        if !o.contains_key(*k) {
            return Err(format!("{at}.{k}: missing"));
        }
    }
    Ok(())
}

/// Text of 1 to `max` characters with no control character.
fn text(v: &Json, at: &str, max: usize) -> Result<String, String> {
    let s = v.as_str().ok_or_else(|| format!("{at}: expected text"))?;
    let n = s.chars().count();
    if n == 0 || n > max {
        return Err(format!("{at}: expected 1 to {max} characters, got {n}"));
    }
    if s.chars().any(char::is_control) {
        return Err(format!("{at}: contains a control character"));
    }
    Ok(s.to_string())
}

/// A list of distinct texts, each of 1 to `max` characters.
fn texts(v: &Json, at: &str, max: usize, min_items: usize, max_items: usize) -> Result<Vec<String>, String> {
    let a = v.as_array().ok_or_else(|| format!("{at}: expected a list"))?;
    if a.len() < min_items || a.len() > max_items {
        return Err(format!("{at}: expected {min_items} to {max_items} items, got {}", a.len()));
    }
    let mut out: Vec<String> = Vec::with_capacity(a.len());
    for (i, x) in a.iter().enumerate() {
        let t = text(x, &format!("{at}[{i}]"), max)?;
        if out.contains(&t) {
            return Err(format!("{at}[{i}]: '{t}' is listed twice"));
        }
        out.push(t);
    }
    Ok(out)
}

/// `[a-z0-9]+` parts joined by single `-`, at most `max` characters.
fn slug(s: &str, max: usize) -> bool {
    s.len() <= max && !s.is_empty() && s.split('-').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

/// An identifier as Staal writes one: a letter or `_`, then letters, digits or `_`; at
/// most 128 characters here.
fn ident(s: &str) -> bool {
    let mut b = s.bytes();
    if s.len() > 128 {
        return false;
    }
    b.next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_') && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn money(v: &Json, at: &str) -> Result<Money, String> {
    crate::host::tool::money(v, at)
}

fn suggested(v: &Json, at: &str) -> Result<Suggested, String> {
    let o = object(v, at)?;
    keys(o, at, &[], &["max", "each", "uses", "only"])?;
    let uses = match o.get("uses") {
        None => None,
        Some(u) => Some(u.as_u64().filter(|n| *n >= 1).ok_or_else(|| format!("{at}.uses: expected a whole number of at least 1"))?),
    };
    let only = match o.get("only") {
        None => None,
        Some(x) => Some(texts(x, &format!("{at}.only"), 2048, 1, 256)?),
    };
    Ok(Suggested {
        max: o.get("max").map(|m| money(m, &format!("{at}.max"))).transpose()?,
        each: o.get("each").map(|m| money(m, &format!("{at}.each"))).transpose()?,
        uses,
        only,
    })
}

fn grant_node(v: &Json, at: &str) -> Result<GrantNode, String> {
    let o = object(v, at)?;
    keys(o, at, &[], &["suggested", "reach", "idempotency_key", "private_addresses", "cloud_metadata", "needs"])?;
    let mut needs = BTreeMap::new();
    if let Some(n) = o.get("needs") {
        for (k, x) in object(n, &format!("{at}.needs"))? {
            if !ident(k) {
                return Err(format!("{at}.needs: '{k}' is not a need's name"));
            }
            needs.insert(k.clone(), grant_node(x, &format!("{at}.needs.{k}"))?);
        }
    }
    Ok(GrantNode {
        suggested: o.get("suggested").map(|s| suggested(s, &format!("{at}.suggested"))).transpose()?.unwrap_or_default(),
        reach: o.get("reach").map(|r| texts(r, &format!("{at}.reach"), 2048, 1, 256)).transpose()?,
        idempotency_key: match o.get("idempotency_key") {
            None => None,
            Some(Json::Bool(b)) => Some(*b),
            Some(_) => return Err(format!("{at}.idempotency_key: expected true or false")),
        },
        addresses: if o.contains_key("private_addresses") || o.contains_key("cloud_metadata") {
            Some(crate::interp::addresses::Policy::from_grant(o.get("private_addresses"), o.get("cloud_metadata"), at)?)
        } else {
            None
        },
        needs,
    })
}

/// A duration as the tool host reads it, in the units this key allows.
fn duration(v: &Json, at: &str, units: &[&str]) -> Result<String, String> {
    crate::host::tool::duration(v, at, units)?;
    Ok(v.as_str().unwrap_or_default().to_string())
}

fn tool(v: &Json, at: &str) -> Result<ToolDef, String> {
    let o = object(v, at)?;
    keys(o, at, &["name", "description", "program", "grants"], &["entry", "app_inputs", "app_input_requirements", "once", "quota", "deadline"])?;
    let name = o["name"].as_str().filter(|n| crate::host::tool::valid_tool_name(n)).ok_or_else(|| {
        format!("{at}.name: expected 1 to 128 characters of A-Z a-z 0-9 _ - ., not only dots")
    })?;
    let description = text(&o["description"], &format!("{at}.description"), 1024)?;
    let program = o["program"].as_str().ok_or_else(|| format!("{at}.program: expected a file name"))?;
    if !program.ends_with(".staal") || program.len() > 255 || !program.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)) || program.starts_with('.') {
        return Err(format!(
            "{at}.program: '{program}' must be the name of a .staal file in the agent's own folder (letters, digits, _, - and ., not starting with .)"
        ));
    }
    let entry = match o.get("entry") {
        None => "main".to_string(),
        Some(e) => e.as_str().filter(|e| ident(e)).ok_or_else(|| format!("{at}.entry: expected a function name"))?.to_string(),
    };
    let app_inputs = match o.get("app_inputs") {
        None => Vec::new(),
        Some(a) => {
            let list = texts(a, &format!("{at}.app_inputs"), 128, 1, 64)?;
            if let Some(bad) = list.iter().find(|n| !ident(n)) {
                return Err(format!("{at}.app_inputs: '{bad}' is not a parameter name"));
            }
            list
        }
    };
    let mut app_input_requirements = BTreeMap::new();
    match o.get("app_input_requirements") {
        None if app_inputs.is_empty() => {}
        None => return Err(format!("{at}.app_input_requirements: missing; every app input needs one")),
        Some(r) => {
            let r = object(r, &format!("{at}.app_input_requirements"))?;
            for (k, x) in r {
                if !app_inputs.contains(k) {
                    return Err(format!("{at}.app_input_requirements: '{k}' is not one of app_inputs"));
                }
                app_input_requirements.insert(k.clone(), text(x, &format!("{at}.app_input_requirements.{k}"), 500)?);
            }
            if let Some(missing) = app_inputs.iter().find(|n| !r.contains_key(*n)) {
                return Err(format!("{at}.app_input_requirements.{missing}: missing; every app input needs one"));
            }
        }
    }
    let mut grants = BTreeMap::new();
    for (k, x) in object(&o["grants"], &format!("{at}.grants"))? {
        if !ident(k) {
            return Err(format!("{at}.grants: '{k}' is not a parameter name"));
        }
        grants.insert(k.clone(), grant_node(x, &format!("{at}.grants.{k}"))?);
    }
    let once = match o.get("once") {
        None => None,
        Some(x) => {
            let oa = format!("{at}.once");
            let oo = object(x, &oa)?;
            keys(oo, &oa, &["by"], &["keep"])?;
            let by = texts(&oo["by"], &format!("{oa}.by"), 128, 1, 64)?;
            if let Some(bad) = by.iter().find(|n| !ident(n)) {
                return Err(format!("{oa}.by: '{bad}' is not a parameter name"));
            }
            if let Some(k) = oo.get("keep") {
                duration(k, &format!("{oa}.keep"), &["m", "h", "d"])?;
            }
            Some(x.clone())
        }
    };
    let quota_suggested = match o.get("quota") {
        None => None,
        Some(q) => {
            let qa = format!("{at}.quota");
            let qo = object(q, &qa)?;
            keys(qo, &qa, &["suggested"], &[])?;
            let sa = format!("{qa}.suggested");
            let s = object(&qo["suggested"], &sa)?;
            keys(s, &sa, &["window"], &["calls", "money"])?;
            if crate::host::tool::duration(&s["window"], &format!("{sa}.window"), &["m", "h", "d"])? > crate::host::tool::MAX_QUOTA_WINDOW_MS {
                return Err(format!("{sa}.window: at most 30d"));
            }
            if let Some(c) = s.get("calls") {
                c.as_u64().filter(|n| (1..=u32::MAX as u64).contains(n)).ok_or_else(|| format!("{sa}.calls: expected a whole number from 1 to {}", u32::MAX))?;
            }
            if let Some(m) = s.get("money") {
                money(m, &format!("{sa}.money"))?;
            }
            if !s.contains_key("calls") && !s.contains_key("money") {
                return Err(format!("{sa}: give \"calls\", \"money\" or both"));
            }
            Some(qo["suggested"].clone())
        }
    };
    let deadline = o.get("deadline").map(|d| duration(d, &format!("{at}.deadline"), &["s", "m", "h"])).transpose()?;
    Ok(ToolDef { name: name.to_string(), description, program: program.to_string(), entry, app_inputs, app_input_requirements, grants, once, quota_suggested, deadline })
}

/// An `https://` URL with a host and no user name or password.
fn https_url(v: &Json, at: &str) -> Result<String, String> {
    let s = v.as_str().ok_or_else(|| format!("{at}: expected an https URL"))?;
    if s.len() > 2048 {
        return Err(format!("{at}: longer than 2048 bytes"));
    }
    let u = url::Url::parse(s).map_err(|e| format!("{at}: not a URL: {e}"))?;
    if u.scheme() != "https" || u.host_str().is_none_or(str::is_empty) || !u.username().is_empty() || u.password().is_some() || s.chars().any(char::is_whitespace) {
        return Err(format!("{at}: expected an https URL with a host and no user name or password"));
    }
    Ok(s.to_string())
}

/// Reads `agent.json` and holds it to its form (docs/schemas/agent.schema.json and the
/// value rules beside it).
pub fn read(text_of_file: &str) -> Result<Manifest, String> {
    let v: Json = strict_json::parse(text_of_file).map_err(|e| format!("agent.json: not JSON as an agent's files must be: {e}"))?;
    let at = "agent.json";
    let o = object(&v, at)?;
    keys(o, at, &["manifest", "agent", "version", "requires_staalc", "title", "summary", "publisher", "license", "categories", "tools", "data"], &[])?;
    match o["manifest"].as_str() {
        Some(MANIFEST_VERSION) => {}
        Some(other) => return Err(format!("{at}.manifest: version '{other}' is not one this staalc knows (it knows {MANIFEST_VERSION})")),
        None => return Err(format!("{at}.manifest: expected \"{MANIFEST_VERSION}\"")),
    }
    let agent = o["agent"].as_str().filter(|a| slug(a, 64)).ok_or_else(|| {
        format!("{at}.agent: expected lowercase letters and digits in parts joined by single -, at most 64 characters")
    })?;
    let version = o["version"].as_str().ok_or_else(|| format!("{at}.version: expected a version"))?;
    crate::semver::Version::parse(version).map_err(|e| format!("{at}.version: {e}"))?;
    let requires = o["requires_staalc"].as_str().ok_or_else(|| format!("{at}.requires_staalc: expected a version range"))?;
    crate::semver::Range::parse(requires).map_err(|e| format!("{at}.requires_staalc: {e}"))?;
    let title = text(&o["title"], &format!("{at}.title"), 80)?;
    let summary = text(&o["summary"], &format!("{at}.summary"), 300)?;
    let pa = format!("{at}.publisher");
    let p = object(&o["publisher"], &pa)?;
    keys(p, &pa, &["id", "name", "security_contact"], &[])?;
    let id = p["id"].as_str().ok_or_else(|| format!("{pa}.id: expected github:<account>"))?;
    let login = id.strip_prefix("github:").unwrap_or("");
    // A GitHub account name: 1 to 39 letters, digits and single hyphens, not at either end.
    if login.len() > 39 || !(!login.is_empty() && login.split('-').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric()))) {
        return Err(format!("{pa}.id: expected github:<account>, an account name of 1 to 39 letters, digits and single hyphens"));
    }
    let pname = text(&p["name"], &format!("{pa}.name"), 100)?;
    let contact = https_url(&p["security_contact"], &format!("{pa}.security_contact"))?;
    let license = o["license"].as_str().ok_or_else(|| format!("{at}.license: expected an SPDX license expression"))?;
    if license.len() > 256 {
        return Err(format!("{at}.license: longer than 256 bytes"));
    }
    spdx::check(license).map_err(|e| format!("{at}.license: '{license}': {e}"))?;
    let categories = texts(&o["categories"], &format!("{at}.categories"), 64, 1, 3)?;
    if let Some(bad) = categories.iter().find(|c| !CATEGORIES.contains(&c.as_str())) {
        return Err(format!("{at}.categories: '{bad}' is not one of {}", CATEGORIES.join(", ")));
    }
    let ta = format!("{at}.tools");
    let list = o["tools"].as_array().ok_or_else(|| format!("{ta}: expected a list"))?;
    if list.is_empty() || list.len() > MAX_TOOLS {
        return Err(format!("{ta}: expected 1 to {MAX_TOOLS} tools, got {}", list.len()));
    }
    let mut tools: Vec<ToolDef> = Vec::with_capacity(list.len());
    for (i, t) in list.iter().enumerate() {
        let t = tool(t, &format!("{ta}[{i}]"))?;
        if tools.iter().any(|x| x.name == t.name) {
            return Err(format!("{ta}[{i}].name: '{}' names another tool too", t.name));
        }
        tools.push(t);
    }
    let da = format!("{at}.data");
    let d = object(&o["data"], &da)?;
    keys(d, &da, &["reads", "writes", "sends", "categories", "processors", "retention", "model_training", "regions"], &[])?;
    for k in ["reads", "writes", "sends", "categories", "processors"] {
        texts(&d[k], &format!("{da}.{k}"), 500, 0, 64)?;
    }
    for k in ["retention", "model_training", "regions"] {
        text(&d[k], &format!("{da}.{k}"), 500)?;
    }
    Ok(Manifest {
        agent: agent.to_string(),
        version: version.to_string(),
        requires_staalc: requires.to_string(),
        title,
        summary,
        publisher: (id.to_string(), pname, contact),
        license: license.to_string(),
        categories,
        tools,
        data: o["data"].clone(),
    })
}

// -------------------------------------------------------------------------------------
// The checks against the project (AGENT_MANIFEST.md section 6, "At publish")

/// A reach pattern with each `{{NAME}}` replaced by a host, after checking that each
/// stands for a whole host: right after `scheme://` in a URL pattern, or before `:` in
/// an address pattern, with nothing else in the host.
fn substitute(pattern: &str, url: bool, at: &str) -> Result<String, String> {
    if !pattern.contains("{{") && !pattern.contains("}}") {
        return Ok(pattern.to_string());
    }
    let host_start = if url {
        pattern.find("://").map(|i| i + 3).ok_or_else(|| format!("{at}: '{pattern}' is not a URL pattern"))?
    } else {
        0
    };
    let rest = &pattern[host_start..];
    let inner = rest.strip_prefix("{{").ok_or_else(|| format!("{at}: '{pattern}': a placeholder must be the whole host"))?;
    let close = inner.find("}}").ok_or_else(|| format!("{at}: '{pattern}': a placeholder is not closed"))?;
    let name = &inner[..close];
    let mut nb = name.bytes();
    let valid = name.len() <= 32
        && nb.next().is_some_and(|c| c.is_ascii_uppercase())
        && nb.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_');
    if !valid {
        return Err(format!("{at}: '{pattern}': a placeholder's name is A-Z, then A-Z, 0-9 or _, at most 32 characters"));
    }
    let after = &inner[close + 2..];
    let ends_host = if url { after.is_empty() || after.starts_with('/') || after.starts_with(':') } else { after.starts_with(':') };
    if !ends_host || after.contains("{{") || after.contains("}}") {
        return Err(format!("{at}: '{pattern}': a placeholder must be the whole host, and there is one at most"));
    }
    Ok(format!("{}{PLACEHOLDER_HOST}{after}", &pattern[..host_start]))
}

/// The runtime capabilities a reach is declared for: those whose grants take `only`
/// patterns of URLs, addresses or paths.
fn takes_reach(cap: &str) -> bool {
    crate::interp::is_runtime_cap(cap)
        && !cap.starts_with("ffi.")
        && !cap.starts_with("db.postgres.")
        && cap != "net.tcp.reply"
        && cap != "net.tls.reply"
}

fn money_fits(name: &str, s: Option<Money>, ceiling: &Option<crate::ast::Limit>, at: &str) -> Result<(), String> {
    let (Some(s), Some(c)) = (s, ceiling) else { return Ok(()) };
    let unit = c.unit.as_deref().ok_or_else(|| format!("{at}.{name}: the program's limit has no currency"))?;
    let c = Money::from_major(c.amount, crate::interp::money::Currency::parse(unit)?)?;
    if s.currency != c.currency {
        return Err(format!("{at}.{name}: suggested in {}, but the program's limit is in {}", s.currency.code, c.currency.code));
    }
    if s.minor > c.minor {
        return Err(format!("{at}.{name}: suggests {s}, more than the program's ceiling of {c}"));
    }
    Ok(())
}

/// Holds one grant node to the capability type `cap(limits)` of its slot: its keys
/// are those the capability takes, its suggestion fits the ceiling, its reach is one a
/// grant could carry, and its needs are exactly the capability's.
fn check_node(prog: &Program, node: &GrantNode, cap: &str, limits: &CapLimits, at: &str, depth: usize) -> Result<(), String> {
    if depth > 16 {
        return Err(format!("{at}: needs nested more than 16 deep"));
    }
    if cap == "net.tcp.accept" || cap == "net.tls.accept" {
        return Err(format!(
            "{at}: `{cap}` needs \"wait_ms\" for the tool host to accept it (MCP.md section 2), which agent.json cannot give"
        ));
    }
    let runtime = crate::interp::is_runtime_cap(cap);
    let s = &node.suggested;
    // The suggestion fits the ceiling in every dimension the ceiling limits.
    money_fits("max", s.max, &limits.max, &format!("{at}.suggested"))?;
    money_fits("each", s.each, &limits.each, &format!("{at}.suggested"))?;
    if let (Some(u), Some(c)) = (s.uses, limits.uses)
        && u > c
    {
        return Err(format!("{at}.suggested.uses: suggests {u}, more than the program's ceiling of {c}"));
    }
    if runtime {
        if s.only.is_some() {
            return Err(format!("{at}.suggested.only: the targets of `{cap}` are its \"reach\""));
        }
        if takes_reach(cap) && node.reach.is_none() {
            return Err(format!("{at}.reach: missing; every grant of `{cap}` declares the targets it needs"));
        }
        if !takes_reach(cap) && node.reach.is_some() {
            return Err(format!("{at}.reach: `{cap}` takes no target patterns"));
        }
        if !crate::interp::connects(cap) && node.addresses.is_some() {
            return Err(format!("{at}: `{cap}` connects to no address a program chooses, so it takes no private_addresses or cloud_metadata"));
        }
        if !node.needs.is_empty() {
            return Err(format!("{at}.needs: `{cap}` has no needs"));
        }
        let url = cap.starts_with("net.http.");
        let address = ["net.tcp.exchange", "net.tls.exchange"].contains(&cap);
        let mut only = Vec::new();
        for (i, p) in node.reach.iter().flatten().enumerate() {
            let pa = format!("{at}.reach[{i}]");
            only.push(if url || address { substitute(p, url, &pa)? } else if p.contains("{{") { return Err(format!("{pa}: only a host can be a placeholder")) } else { p.clone() });
        }
        let l = broker::Limits { max: s.max, each: s.each, uses: s.uses, only: node.reach.as_ref().map(|_| only.clone()) };
        crate::interp::check_runtime_grant(cap, &l, node.idempotency_key == Some(true), at, "reach")?;
        if let Some(ceiling) = &limits.only
            && !crate::pattern::all_contained(&only, ceiling)
        {
            return Err(format!("{at}.reach: {:?} reaches beyond the program's `only` {ceiling:?}", node.reach.as_ref().unwrap_or(&Vec::new())));
        }
        return Ok(());
    }
    // A library capability.
    if node.addresses.is_some() {
        return Err(format!("{at}: a library capability's addresses are its needs' (private_addresses and cloud_metadata go on each need)"));
    }
    if node.reach.is_some() {
        return Err(format!("{at}.reach: a library capability's targets are suggested in \"suggested.only\""));
    }
    if node.idempotency_key.is_some() {
        crate::interp::check_runtime_grant(cap, &broker::Limits::default(), true, at, "reach")?;
    }
    if let Some(only) = &s.only {
        for (i, p) in only.iter().enumerate() {
            crate::pattern::validate(p).map_err(|e| format!("{at}.suggested.only[{i}]: {e}"))?;
        }
        if let Some(ceiling) = &limits.only
            && !crate::pattern::all_contained(only, ceiling)
        {
            return Err(format!("{at}.suggested.only: {only:?} reaches beyond the program's `only` {ceiling:?}"));
        }
    }
    let l = broker::Limits { max: s.max, each: s.each, uses: s.uses, only: s.only.clone() };
    crate::interp::check_currencies(&l, &format!("{at}.suggested"))?;
    let (decl, _) = prog
        .caps
        .iter()
        .find_map(|c| c.actions.iter().find(|a| c.action_path(&a.name) == cap).map(|a| (c, a)))
        .ok_or_else(|| format!("{at}: `{cap}` is not a capability of the program"))?;
    let cap_needs: Vec<(&String, &Vec<String>, &CapLimits)> = decl
        .needs
        .iter()
        .filter_map(|(n, t, _)| match t {
            Ty::Cap { path, limits } => Some((n, path, limits)),
            _ => None,
        })
        .collect();
    for k in node.needs.keys() {
        if !cap_needs.iter().any(|(n, _, _)| *n == k) {
            return Err(format!("{at}.needs.{k}: `{}` has no capability need '{k}'", decl.path.join(".")));
        }
    }
    for (n, path, nlimits) in cap_needs {
        let child = node.needs.get(n).ok_or_else(|| format!("{at}.needs.{n}: missing; every capability need of `{}` is described", decl.path.join(".")))?;
        check_node(prog, child, &path.join("."), nlimits, &format!("{at}.needs.{n}"), depth + 1)?;
    }
    Ok(())
}

/// The agent's folder after `check`: what it found.
pub struct Checked {
    pub manifest: Manifest,
    /// Each tool's checked program and its fingerprint, in the order of `tools`.
    pub programs: Vec<crate::Checked>,
}

/// `staalc agent check` without the description: `agent.json` read and held to the
/// project in `root` (AGENT_MANIFEST.md section 6, checks 1 to 7 but the description).
pub fn check_manifest(root: &Path) -> Result<Checked, String> {
    let path = root.join("agent.json");
    let text_of_file = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let manifest = read(&text_of_file)?;
    let mine = crate::semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    let range = crate::semver::Range::parse(&manifest.requires_staalc)?;
    if !range.matches(&mine) {
        return Err(format!("agent.json.requires_staalc: '{}' does not include this staalc, {mine}", manifest.requires_staalc));
    }
    let root = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut programs = Vec::new();
    for (i, t) in manifest.tools.iter().enumerate() {
        let at = format!("agent.json.tools[{i}] ('{}')", t.name);
        let file: PathBuf = root.join(&t.program);
        let real = std::fs::canonicalize(&file).map_err(|e| format!("{at}.program: {}: {e}", t.program))?;
        if real.parent() != Some(root.as_path()) || !real.is_file() {
            return Err(format!("{at}.program: {} is not a file in the agent's folder", t.program));
        }
        let checked = crate::check_path(&real).map_err(|e| format!("{at}.program: {e}"))?;
        let prog = match &checked.result {
            Ok(p) => p,
            Err(d) => {
                let shown: Vec<String> = d.iter().take(10).map(|x| crate::format_diag_in(&checked.files, x)).collect();
                return Err(format!("{at}.program: {} has check errors:\n{}", t.program, shown.join("\n")));
            }
        };
        let f = prog.fns.iter().find(|f| f.name == t.entry).ok_or_else(|| format!("{at}.entry: '{}' has no function '{}'", t.program, t.entry))?;
        // Every capability parameter is in grants, and nothing else is.
        for p in &f.params {
            if let Ty::Cap { path, limits } = &p.ty {
                let node = t.grants.get(&p.name).ok_or_else(|| format!("{at}.grants.{}: missing; every capability parameter of '{}' is described", p.name, t.entry))?;
                check_node(prog, node, &path.join("."), limits, &format!("{at}.grants.{}", p.name), 0)?;
            }
        }
        for k in t.grants.keys() {
            if !f.params.iter().any(|p| &p.name == k && matches!(p.ty, Ty::Cap { .. })) {
                return Err(format!("{at}.grants.{k}: '{}' has no capability parameter '{k}'", t.entry));
            }
        }
        // The tool host would accept the tool: given as an app's tool with grants per
        // call, so the host checks the program, entry, console use, app inputs, `once`,
        // quota and deadline as it would, and the grants (which a business sets at
        // install) are the checks above. A model is assumed for a tool that can call one;
        // the business configures it at install.
        let mut def = json!({ "name": t.name, "description": t.description, "program": t.program, "entry": t.entry });
        if !t.app_inputs.is_empty() {
            def["app_inputs"] = json!(t.app_inputs);
        }
        if let Some(o) = &t.once {
            def["once"] = o.clone();
        }
        if let Some(q) = &t.quota_suggested {
            def["quota"] = q.clone();
        }
        if let Some(d) = &t.deadline {
            def["deadline"] = json!(d);
        }
        let settings = crate::host::tool::Settings {
            at: "agent".into(),
            journals: root.clone(),
            keep_ms: None,
            max_concurrent_runs: 256,
            notify: None,
            notify_env: Vec::new(),
        };
        let model = json!({ "provider": "script", "responses": [] });
        let mut warnings = Vec::new();
        crate::host::tool::parse_tool(
            &def,
            &format!("agent.json.tools[{i}]"),
            &root,
            Some(&model),
            &settings,
            crate::host::tool::Definer::App(crate::host::tool::Grants::PerCall),
            &mut warnings,
        )?;
        programs.push(checked);
    }
    Ok(Checked { manifest, programs })
}

/// `staalc agent check`: everything in `check_manifest`, and `agent.described.json`
/// equal, byte for byte, to a fresh description (AGENT_MANIFEST.md section 6, check 2).
pub fn check(root: &Path) -> Result<Checked, String> {
    let checked = check_manifest(root)?;
    let fresh = describe::bytes(root, &checked)?;
    let path = root.join("agent.described.json");
    let committed = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}; run `staalc agent describe`", path.display()))?;
    if committed != fresh.as_bytes() {
        return Err(format!(
            "{} differs from a fresh description of the agent; run `staalc agent describe` and commit what it writes",
            path.display()
        ));
    }
    Ok(checked)
}

/// `staalc agent describe`: checks `agent.json` against the project and writes
/// `agent.described.json`.
pub fn write_description(root: &Path) -> Result<Checked, String> {
    let checked = check_manifest(root)?;
    let fresh = describe::bytes(root, &checked)?;
    let path = root.join("agent.described.json");
    std::fs::write(&path, fresh).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(checked)
}
