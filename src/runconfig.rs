// Run configs from the command line (RUNTIME.md section 1).
//
// `staalc run prog.staal` without `--config` runs a program that needs nothing an
// operator must grant: its entry function (`main`, or `--entry`) has no capability or
// Secret parameter. Its inputs come from `--input name=value`: a Text parameter takes
// the value as written, any other the value as JSON. The config this makes is an
// ordinary one, so the run, its journal and `resume` behave exactly as with a file.
//
// `staalc config prog.staal` writes a config to start from: the entry, every input
// without a default (a placeholder of its type), a grant for every capability with
// the limits the code declares (and what a library capability needs), every Secret as
// an environment variable, and a model only when the program can ask one.

use crate::ast::{CapLimits, Expr, FnDecl, Program, Stmt, Ty};
use serde_json::{json, Map, Value as Json};

/// The entry function `entry` names, or `main`.
fn entry_fn<'a>(prog: &'a Program, entry: Option<&str>) -> Result<&'a FnDecl, String> {
    let name = entry.unwrap_or("main");
    prog.fns.iter().find(|f| f.name == name && f.method_of.is_none() && f.generics.is_empty()).ok_or_else(|| match entry {
        Some(n) => format!("the program has no function '{n}' to start"),
        None => "the program has no function `main`: name the function to start with --entry".to_string(),
    })
}

/// The config of a run given on the command line: `entry` (or `main`) and inputs as
/// `name=value`.
pub fn from_command_line(prog: &Program, entry: Option<&str>, inputs: &[String]) -> Result<Json, String> {
    let f = entry_fn(prog, entry)?;
    // What only an operator may give cannot come from the command line.
    let granted: Vec<String> = f
        .params
        .iter()
        .filter(|p| matches!(p.ty, Ty::Cap { .. }) || p.ty == Ty::Named("Secret".into()))
        .map(|p| format!("{} ({})", p.name, p.ty))
        .collect();
    if !granted.is_empty() {
        return Err(format!(
            "'{}' needs {}, which only a run config can grant; `staalc config` writes one to fill in",
            f.name,
            granted.join(", ")
        ));
    }
    let mut given = Map::new();
    for i in inputs {
        let (name, value) = i.split_once('=').ok_or_else(|| format!("--input {i}: expected name=value"))?;
        let p = f.params.iter().find(|p| p.name == name).ok_or_else(|| format!("--input {i}: '{}' has no parameter '{name}'", f.name))?;
        let v = if p.ty == Ty::Named("Text".into()) {
            Json::String(value.to_string())
        } else {
            serde_json::from_str(value).map_err(|_| {
                format!(
                    "--input {name}: '{value}' is not JSON; a {} is given as JSON (only Text is given as written), e.g. {}",
                    p.ty,
                    placeholder(prog, &p.ty, 0)
                )
            })?
        };
        if given.insert(name.to_string(), v).is_some() {
            return Err(format!("--input {name} is given twice"));
        }
    }
    Ok(json!({ "entry": f.name, "inputs": given }))
}

/// A value of type `t` to fill in: empty text, zero, false, the first variant, one
/// item of a list.
fn placeholder(prog: &Program, t: &Ty, depth: usize) -> Json {
    if depth > 8 {
        return Json::Null;
    }
    match t {
        Ty::Named(n) => match n.as_str() {
            "Text" => json!(""),
            "Int" => json!(0),
            "Bool" => json!(false),
            "Money" => json!({ "amount": "0.00", "currency": "USD" }),
            _ => {
                if let Some(r) = prog.types.iter().find(|r| r.name == *n) {
                    let mut o = Map::new();
                    for (f, ft) in &r.fields {
                        o.insert(f.clone(), placeholder(prog, ft, depth + 1));
                    }
                    Json::Object(o)
                } else if let Some(v) = prog.enums.iter().find(|e| e.name == *n).and_then(|e| e.variants.first()) {
                    let mut o = Map::new();
                    o.insert("kind".into(), json!(v.name));
                    for (f, ft) in &v.fields {
                        o.insert(f.clone(), placeholder(prog, ft, depth + 1));
                    }
                    Json::Object(o)
                } else {
                    Json::Null
                }
            }
        },
        Ty::List(inner, _) => json!([placeholder(prog, inner, depth + 1)]),
        Ty::Set(_, _) => json!([]),
        Ty::Map(..) => json!({}),
        Ty::Optional(_) => Json::Null,
        Ty::Tuple(items) => Json::Array(items.iter().map(|i| placeholder(prog, i, depth + 1)).collect()),
        Ty::Proposed(_) | Ty::Cap { .. } | Ty::Func(..) => Json::Null,
    }
}

/// A Secret's environment variable: its name in capitals.
fn env_name(name: &str) -> String {
    name.to_ascii_uppercase()
}

/// The grant of `cap path(limits)`, with the limits the code declares, and what a
/// library capability or a C function needs besides.
fn grant(prog: &Program, path: &[String], limits: &CapLimits) -> Json {
    let p = path.join(".");
    let mut g = Map::new();
    g.insert("cap".into(), json!(p));
    if let Some(u) = limits.uses {
        g.insert("uses".into(), json!(u));
    }
    for (key, l) in [("max", &limits.max), ("each", &limits.each)] {
        if let Some(l) = l
            && let Some(cur) = &l.unit
        {
            g.insert(key.into(), json!({ "amount": l.amount.to_string(), "currency": cur }));
        }
    }
    if let Some(only) = &limits.only {
        g.insert("only".into(), json!(only));
    }
    if p.starts_with("ffi.") {
        g.insert("library".into(), json!(""));
        g.insert("sha256".into(), json!(""));
    }
    // A database to fill in, under the name the program gives it.
    if p.starts_with("db.postgres.") {
        g.insert(
            "databases".into(),
            json!({ "database": { "host": "", "dbname": "", "user": "", "password": { "env": "DATABASE_PASSWORD" } } }),
        );
    }
    if let Some(c) = prog.caps.iter().find(|c| c.actions.iter().any(|a| c.action_path(&a.name) == p))
        && !c.needs.is_empty()
    {
        let mut needs = Map::new();
        for (n, t, _) in &c.needs {
            let v = match t {
                Ty::Cap { path, limits } => grant(prog, path, limits),
                // Named after the capability, so two libraries' keys never share a name.
                Ty::Named(x) if x == "Secret" => json!({ "env": env_name(&format!("{}_{n}", c.path.join("_"))) }),
                _ => json!(""),
            };
            needs.insert(n.clone(), v);
        }
        g.insert("needs".into(), Json::Object(needs));
    }
    Json::Object(g)
}

/// What one expression does that matters here: whether it asks a model, and the
/// functions it names (called, or taken as a value, which may be called later).
fn scan(e: &Expr, asks: &mut bool, names: &mut Vec<String>) {
    match e {
        Expr::Propose { .. } => *asks = true,
        Expr::Call { name, .. } | Expr::Closure { name, .. } | Expr::Var(name, _) => names.push(name.clone()),
        _ => {}
    }
    for c in e.children() {
        scan(c, asks, names);
    }
}

fn scan_block(body: &[Stmt], asks: &mut bool, names: &mut Vec<String>) {
    for s in body {
        for e in s.exprs() {
            scan(e, asks, names);
        }
        for b in s.blocks() {
            scan_block(b, asks, names);
        }
    }
}

/// Whether running `f` can ask a model: through it, the functions it reaches, or the
/// library capabilities it is given.
fn reaches_a_model(prog: &Program, f: &FnDecl) -> bool {
    let mut todo = vec![f.name.clone()];
    for p in &f.params {
        if let Ty::Cap { path, .. } = &p.ty {
            let p = path.join(".");
            for c in &prog.caps {
                for a in c.actions.iter().filter(|a| c.action_path(&a.name) == p) {
                    todo.push(a.fn_name.clone());
                    todo.extend(c.canonical.clone());
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    while let Some(n) = todo.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        let Some(g) = prog.fns.iter().find(|g| g.name == n) else { continue };
        let mut asks = false;
        scan_block(&g.body, &mut asks, &mut todo);
        if asks {
            return true;
        }
    }
    false
}

/// `staalc config`: a run config to fill in for `entry` (or `main`).
pub fn skeleton(prog: &Program, entry: Option<&str>) -> Result<Json, String> {
    let f = entry_fn(prog, entry)?;
    let (mut inputs, mut grants, mut secrets) = (Map::new(), Map::new(), Map::new());
    for p in &f.params {
        match &p.ty {
            Ty::Cap { path, limits } => {
                grants.insert(p.name.clone(), grant(prog, path, limits));
            }
            Ty::Named(x) if x == "Secret" => {
                secrets.insert(p.name.clone(), json!({ "env": env_name(&p.name) }));
            }
            // A parameter with a default may be left out.
            _ if p.default.is_some() => {}
            t => {
                inputs.insert(p.name.clone(), placeholder(prog, t, 0));
            }
        }
    }
    let mut cfg = Map::new();
    cfg.insert("entry".into(), json!(f.name));
    cfg.insert("inputs".into(), Json::Object(inputs));
    if !grants.is_empty() {
        cfg.insert("grants".into(), Json::Object(grants));
    }
    if !secrets.is_empty() {
        cfg.insert("secrets".into(), Json::Object(secrets));
    }
    if reaches_a_model(prog, f) {
        cfg.insert(
            "model".into(),
            json!({ "provider": "anthropic", "model": "claude-sonnet-5-5", "api_key": { "env": "ANTHROPIC_API_KEY" } }),
        );
    }
    Ok(Json::Object(cfg))
}

/// What a caller outside the program sees of entry function `entry` (or `main`): a JSON
/// Schema (2020-12) for the inputs a caller gives, one for the value it returns, and the
/// grants and secrets only an operator may give, which are never inputs. Used to describe
/// a program as a tool (an MCP server, an agent framework). The schemas describe what
/// `inputs` accepts; the runtime's own check of inputs (RUNTIME.md section 3) is still
/// the one that decides, and it is stricter in one place a schema cannot express: a
/// money amount too large to hold exactly is refused.
pub fn interface(prog: &Program, entry: Option<&str>) -> Result<Json, String> {
    let f = entry_fn(prog, entry)?;
    let mut defs = Map::new();
    let (mut props, mut required, mut grants, mut secrets) = (Map::new(), Vec::new(), Vec::new(), Vec::new());
    for p in &f.params {
        match &p.ty {
            Ty::Cap { path, .. } => grants.push(json!({ "name": p.name, "cap": path.join(".") })),
            Ty::Named(x) if x == "Secret" => secrets.push(json!(p.name)),
            t => {
                let s = input_schema(prog, t, &mut defs).map_err(|why| format!("parameter '{}': {why}", p.name))?;
                props.insert(p.name.clone(), s);
                if p.default.is_none() {
                    required.push(json!(p.name));
                }
            }
        }
    }
    let mut input = Map::new();
    input.insert("$schema".into(), json!("https://json-schema.org/draft/2020-12/schema"));
    input.insert("type".into(), json!("object"));
    input.insert("properties".into(), Json::Object(props));
    input.insert("required".into(), Json::Array(required));
    input.insert("additionalProperties".into(), json!(false));
    let mut out_defs = Map::new();
    let output = match &f.ret {
        None => json!({ "type": "null" }),
        Some(t) => output_schema(prog, t, &mut out_defs)?,
    };
    if !defs.is_empty() {
        input.insert("$defs".into(), Json::Object(defs));
    }
    let output = match output {
        Json::Object(mut o) => {
            o.insert("$schema".into(), json!("https://json-schema.org/draft/2020-12/schema"));
            if !out_defs.is_empty() {
                o.insert("$defs".into(), Json::Object(out_defs));
            }
            Json::Object(o)
        }
        other => other,
    };
    Ok(json!({ "entry": f.name, "input": Json::Object(input), "output": output, "grants": grants, "secrets": secrets }))
}

/// A `$ref` to a declared record or enum, its definition added to `defs` once. A
/// definition refers to others by `$ref` too, so recursive types are described exactly.
fn named_ref(prog: &Program, name: &str, defs: &mut Map<String, Json>, out: bool) -> Result<Json, String> {
    // JSON Pointer escaping (RFC 6901) for the definition's name inside the reference.
    let pointer = name.replace('~', "~0").replace('/', "~1");
    let r = json!({ "$ref": format!("#/$defs/{pointer}") });
    if defs.contains_key(name) {
        return Ok(r);
    }
    let fields_schema = |fields: &[(String, Ty)], defs: &mut Map<String, Json>, kind: Option<&str>| -> Result<Json, String> {
        let (mut props, mut req) = (Map::new(), Vec::new());
        if let Some(k) = kind {
            props.insert("kind".into(), json!({ "const": k }));
            req.push(json!("kind"));
        }
        for (f, t) in fields {
            let s = if out { output_schema(prog, t, defs)? } else { input_schema(prog, t, defs)? };
            props.insert(f.clone(), s);
            req.push(json!(f));
        }
        Ok(json!({ "type": "object", "properties": props, "required": req, "additionalProperties": false }))
    };
    if let Some(rec) = prog.types.iter().find(|r| r.name == name) {
        // Placed before the fields are described, so a field that refers back to this
        // type finds it and stops.
        defs.insert(name.to_string(), json!({}));
        let s = fields_schema(&rec.fields, defs, None)?;
        defs.insert(name.to_string(), s);
        return Ok(r);
    }
    if let Some(en) = prog.enums.iter().find(|e| e.name == name) {
        defs.insert(name.to_string(), json!({}));
        let mut options = Vec::new();
        for v in &en.variants {
            options.push(fields_schema(&v.fields, defs, Some(&v.name))?);
        }
        defs.insert(name.to_string(), json!({ "anyOf": options }));
        return Ok(r);
    }
    Err(format!("unknown type {name}"))
}

/// Money as inputs take it: an amount with no more decimal places than its currency
/// has, so one alternative per number of places.
fn money_schema() -> Json {
    let mut places: Vec<u32> = crate::interp::money::Currency::all().iter().map(|(_, e)| *e).collect();
    places.sort_unstable();
    places.dedup();
    let options: Vec<Json> = places
        .iter()
        .map(|&e| {
            let codes: Vec<&str> = crate::interp::money::Currency::all().iter().filter(|(_, x)| *x == e).map(|(c, _)| *c).collect();
            let pattern = if e == 0 { "^[0-9]+$".to_string() } else { format!("^[0-9]+(\\.[0-9]{{1,{e}}})?$") };
            json!({
                "type": "object",
                "properties": {
                    "amount": { "type": "string", "pattern": pattern },
                    "currency": { "enum": codes }
                },
                "required": ["amount", "currency"],
                "additionalProperties": false
            })
        })
        .collect();
    json!({ "anyOf": options, "description": "money: a decimal amount as a string, e.g. {\"amount\": \"12.50\", \"currency\": \"USD\"}" })
}

/// The JSON Schema of a value a caller gives as type `t`, as `inputs` accepts it.
fn input_schema(prog: &Program, t: &Ty, defs: &mut Map<String, Json>) -> Result<Json, String> {
    Ok(match t {
        Ty::Named(n) => match n.as_str() {
            "Text" => json!({ "type": "string" }),
            "Int" => json!({ "type": "integer", "minimum": i64::MIN, "maximum": i64::MAX }),
            "Bool" => json!({ "type": "boolean" }),
            "Money" => money_schema(),
            "Secret" | "Header" => return Err(format!("a {n} cannot be given as data")),
            "Receipt" | "Outcome" | "Done" => {
                return Err(format!("a {n} can only be made by the runtime, never given from outside"))
            }
            _ => named_ref(prog, n, defs, false)?,
        },
        Ty::List(inner, max) => json!({ "type": "array", "items": input_schema(prog, inner, defs)?, "maxItems": max }),
        Ty::Set(inner, max) => {
            json!({ "type": "array", "items": input_schema(prog, inner, defs)?, "maxItems": max, "uniqueItems": true })
        }
        Ty::Map(k, v, max) => {
            let mut m = Map::new();
            m.insert("type".into(), json!("object"));
            m.insert("additionalProperties".into(), input_schema(prog, v, defs)?);
            m.insert("maxProperties".into(), json!(max));
            if **k == Ty::Named("Int".into()) {
                m.insert("propertyNames".into(), json!({ "pattern": "^-?(0|[1-9][0-9]*)$" }));
            }
            Json::Object(m)
        }
        Ty::Optional(inner) => json!({ "anyOf": [input_schema(prog, inner, defs)?, { "type": "null" }] }),
        Ty::Tuple(items) => {
            let mut s = Vec::new();
            for i in items {
                s.push(input_schema(prog, i, defs)?);
            }
            json!({ "type": "array", "prefixItems": s, "items": false, "minItems": items.len(), "maxItems": items.len() })
        }
        Ty::Proposed(_) => return Err("a Proposed value can only come from a model call".into()),
        Ty::Func(..) => return Err("a function value cannot be given as data".into()),
        Ty::Cap { .. } => return Err("a capability can only be granted, never given as data".into()),
    })
}

/// The JSON Schema of a value the entry function returns, as a run's result writes it.
fn output_schema(prog: &Program, t: &Ty, defs: &mut Map<String, Json>) -> Result<Json, String> {
    Ok(match t {
        Ty::Named(n) => match n.as_str() {
            "Text" => json!({ "type": "string" }),
            "Int" => json!({ "type": "integer", "minimum": i64::MIN, "maximum": i64::MAX }),
            "Bool" => json!({ "type": "boolean" }),
            "Money" => money_schema(),
            "Receipt" => json!({
                "type": "object",
                "properties": { "receipt": { "type": "string" }, "capability": { "type": "string" }, "method": { "type": "string" } },
                "required": ["receipt", "capability", "method"],
                "additionalProperties": false
            }),
            "Outcome" => json!({
                "type": "object",
                "properties": { "ok": { "type": "boolean" }, "error": { "type": "string" } },
                "required": ["ok", "error"],
                "additionalProperties": false
            }),
            // Evidence: receipts, outcomes and other Done values, as the result writes them.
            "Done" => json!({
                "type": "object",
                "properties": { "done": { "type": "array" } },
                "required": ["done"],
                "additionalProperties": false
            }),
            "Secret" | "Header" => return Err(format!("a {n} is never written as a result")),
            _ => named_ref(prog, n, defs, true)?,
        },
        Ty::List(inner, max) => json!({ "type": "array", "items": output_schema(prog, inner, defs)?, "maxItems": max }),
        Ty::Set(inner, max) => {
            json!({ "type": "array", "items": output_schema(prog, inner, defs)?, "maxItems": max, "uniqueItems": true })
        }
        Ty::Map(k, v, max) => {
            let mut m = Map::new();
            m.insert("type".into(), json!("object"));
            m.insert("additionalProperties".into(), output_schema(prog, v, defs)?);
            m.insert("maxProperties".into(), json!(max));
            if **k == Ty::Named("Int".into()) {
                m.insert("propertyNames".into(), json!({ "pattern": "^-?(0|[1-9][0-9]*)$" }));
            }
            Json::Object(m)
        }
        Ty::Optional(inner) => json!({ "anyOf": [output_schema(prog, inner, defs)?, { "type": "null" }] }),
        Ty::Tuple(items) => {
            let mut s = Vec::new();
            for i in items {
                s.push(output_schema(prog, i, defs)?);
            }
            json!({ "type": "array", "prefixItems": s, "items": false, "minItems": items.len(), "maxItems": items.len() })
        }
        Ty::Proposed(_) => json!({}),
        Ty::Func(..) => return Err("a function value is never written as a result".into()),
        Ty::Cap { .. } => return Err("a capability is never written as a result".into()),
    })
}
