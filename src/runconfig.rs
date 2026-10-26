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
