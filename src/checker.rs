// Type checker. Enforces:
//   E001  a model's Proposed<T> value is used where a plain T is needed, without `validate`
//   E002  an action is performed without a matching capability in scope
//   E003  a capability is widened (given more authority than it holds)
//   E006  an untrusted value is used as a model instruction, or flows into an @trusted slot
//   E007  a model call's input, or the decision to make it, can reveal a path the call
//         is blind to (via `blind_to` or an @blind parameter); or blinded data is passed
//         to a function that does not promise to keep it blinded
// plus ordinary errors:
//   E100  undefined name or function
//   E101  type mismatch or wrong number of arguments
//   E102  unknown type
//   E103  `break` outside a loop
//   E104  unknown field, or field access on a value that has no fields
//
// Every place a value flows into a slot (call argument, typed `let`, `return`)
// goes through `assignable`, so the authority rules are decided in one place.
//
// Trust: string, number and boolean literals are trusted. Parameters are
// untrusted unless marked `@trusted`, and callers must pass trusted values to
// those. Model output (validated or not), action results and function results
// are untrusted unless the function's return is marked `@trusted`. A comparison
// is trusted only if both sides are. Nothing else can make a value trusted.

use crate::ast::*;
use crate::lexer::Pos;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: &'static str,
    pub pos: Pos,
    pub msg: String,
}

struct Sig {
    /// (type, @trusted, @blind paths) per parameter
    params: Vec<(Ty, bool, Vec<Vec<String>>)>,
    ret: Ty,
    ret_trusted: bool,
}

struct Ctx<'a> {
    known: HashSet<String>,
    /// Declared types and their fields, in declaration order.
    fields: HashMap<String, Vec<(String, Ty)>>,
    sigs: HashMap<&'a str, Sig>,
    diags: Vec<Diagnostic>,
    /// What reaching the current statement reveals (see `check_block`).
    pc: Vec<Src>,
    /// Absolute paths blinded by the current function's @blind parameters, with display text.
    fn_blind: Vec<(Vec<String>, String)>,
    /// Functions that can make a model call, directly or transitively.
    model_fns: HashSet<String>,
}

impl Ctx<'_> {
    fn err(&mut self, code: &'static str, pos: Pos, msg: String) {
        self.diags.push(Diagnostic { code, pos, msg });
    }
}

/// A variable's type, whether its value is trusted, and what data it can reveal.
#[derive(Clone)]
struct Binding {
    ty: Ty,
    trusted: bool,
    reveals: Vec<Src>,
}

/// A source a value can reveal. `path` is rooted at a parameter, e.g. ["order", "customer"].
/// `exact` means the value IS that path (an alias, so `.field` narrows it);
/// otherwise the value is merely derived from it and may reveal anything beneath it.
#[derive(Clone, Debug)]
struct Src {
    path: Vec<String>,
    exact: bool,
}

/// Two paths overlap when one contains the other: ["order"] and ["order","customer","email"].
fn overlaps(a: &[String], b: &[String]) -> bool {
    let n = a.len().min(b.len());
    a[..n] == b[..n]
}

/// What data an expression's value can reveal. Literals reveal nothing. Anything
/// computed (calls, actions, comparisons, model output) can reveal all of its inputs.
fn reveals(e: &Expr, env: &Env) -> Vec<Src> {
    let inexact = |v: Vec<Src>| v.into_iter().map(|s| Src { path: s.path, exact: false }).collect::<Vec<_>>();
    match e {
        Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) => Vec::new(),
        Expr::Var(n, _) => env.get(n).map(|b| b.reveals.clone()).unwrap_or_default(),
        Expr::Field { base, field, .. } => reveals(base, env)
            .into_iter()
            .map(|s| {
                if s.exact {
                    let mut path = s.path;
                    path.push(field.clone());
                    Src { path, exact: true }
                } else {
                    s
                }
            })
            .collect(),
        Expr::Validate(inner, _) => inexact(reveals(inner, env)),
        Expr::Cmp { left, right, .. } => {
            let mut v = reveals(left, env);
            v.extend(reveals(right, env));
            inexact(v)
        }
        Expr::Call { args, .. } | Expr::Action { args, .. } => {
            inexact(args.iter().flat_map(|a| reveals(a, env)).collect())
        }
        Expr::Propose { instruct, data, .. } => {
            let mut v: Vec<Src> = data.iter().flat_map(|d| reveals(d, env)).collect();
            if let Some(i) = instruct {
                v.extend(reveals(i, env));
            }
            inexact(v)
        }
    }
}

/// The text of a name/field expression, for messages: `order.customer.email`.
fn expr_text(e: &Expr) -> String {
    match e {
        Expr::Var(n, _) => n.clone(),
        Expr::Field { base, field, .. } => format!("{}.{field}", expr_text(base)),
        Expr::Call { name, .. } => format!("{name}(...)"),
        Expr::Action { receiver, method, .. } => format!("{receiver}.{method}(...)"),
        Expr::Validate(inner, _) => format!("validate {}", expr_text(inner)),
        Expr::Propose { .. } => "propose(...)".into(),
        Expr::Cmp { left, op, right, .. } => format!("{} {op} {}", expr_text(left), expr_text(right)),
        Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) => "literal".into(),
    }
}

fn is_path(e: &Expr) -> bool {
    match e {
        Expr::Var(..) => true,
        Expr::Field { base, .. } => is_path(base),
        _ => false,
    }
}

type Env = HashMap<String, Binding>;

fn named(n: &str) -> Ty {
    Ty::Named(n.into())
}

pub fn check(prog: &Program) -> Vec<Diagnostic> {
    let mut ctx = Ctx {
        known: ["Text", "Int", "Bool", "Money", "Unit", "Receipt"].iter().map(|s| s.to_string()).collect(),
        fields: HashMap::new(),
        sigs: HashMap::new(),
        diags: Vec::new(),
        pc: Vec::new(),
        fn_blind: Vec::new(),
        model_fns: model_calling_fns(prog),
    };
    for t in &prog.types {
        if ctx.known.contains(&t.name) {
            ctx.err("E101", t.pos, format!("type '{}' is already defined", t.name));
            continue;
        }
        ctx.known.insert(t.name.clone());
        let mut seen = HashSet::new();
        for (fname, fty) in &t.fields {
            if !seen.insert(fname.as_str()) {
                ctx.err("E101", t.pos, format!("type '{}' declares field '{fname}' twice", t.name));
            }
            if let Ty::Cap { .. } = fty {
                ctx.err(
                    "E101",
                    t.pos,
                    format!(
                        "field '{}.{fname}' cannot be a capability; pass capabilities as parameters so they \
                         cannot travel inside data",
                        t.name
                    ),
                );
            }
        }
        ctx.fields.insert(t.name.clone(), t.fields.clone());
    }
    for f in &prog.fns {
        if ctx.sigs.contains_key(f.name.as_str()) {
            ctx.err("E101", f.pos, format!("function '{}' is defined twice", f.name));
            continue;
        }
        ctx.sigs.insert(
            &f.name,
            Sig {
                params: f.params.iter().map(|p| (p.ty.clone(), p.trusted, p.blind.clone())).collect(),
                ret: f.ret.clone().unwrap_or_else(|| named("Unit")),
                ret_trusted: f.ret_trusted,
            },
        );
    }
    for t in &prog.types {
        for (_, fty) in &t.fields {
            check_ty_known(&mut ctx, fty, t.pos);
        }
    }
    for f in &prog.fns {
        check_fn(&mut ctx, f);
    }
    ctx.diags
}

/// Names of functions that can make a model call, directly or through calls.
fn model_calling_fns(prog: &Program) -> HashSet<String> {
    fn scan_expr(e: &Expr, direct: &mut bool, calls: &mut HashSet<String>) {
        match e {
            Expr::Propose { instruct, data, .. } => {
                *direct = true;
                for x in instruct.iter().map(|b| b.as_ref()).chain(data.iter()) {
                    scan_expr(x, direct, calls);
                }
            }
            Expr::Call { name, args, .. } => {
                calls.insert(name.clone());
                args.iter().for_each(|a| scan_expr(a, direct, calls));
            }
            Expr::Action { args, .. } => args.iter().for_each(|a| scan_expr(a, direct, calls)),
            Expr::Validate(inner, _) | Expr::Field { base: inner, .. } => scan_expr(inner, direct, calls),
            Expr::Cmp { left, right, .. } => {
                scan_expr(left, direct, calls);
                scan_expr(right, direct, calls);
            }
            Expr::Var(..) | Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) => {}
        }
    }
    fn scan_block(body: &[Stmt], direct: &mut bool, calls: &mut HashSet<String>) {
        for s in body {
            match s {
                Stmt::Let { value: e, .. } | Stmt::Expr(e) | Stmt::Return(Some(e), _) => scan_expr(e, direct, calls),
                Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => scan_block(body, direct, calls),
                Stmt::If { cond, then, els, .. } => {
                    scan_expr(cond, direct, calls);
                    scan_block(then, direct, calls);
                    scan_block(els, direct, calls);
                }
                Stmt::Return(None, _) | Stmt::Break(_) => {}
            }
        }
    }
    let mut info = Vec::new();
    for f in &prog.fns {
        let (mut direct, mut calls) = (false, HashSet::new());
        scan_block(&f.body, &mut direct, &mut calls);
        info.push((f.name.clone(), direct, calls));
    }
    let mut out: HashSet<String> = info.iter().filter(|(_, d, _)| *d).map(|(n, _, _)| n.clone()).collect();
    loop {
        let before = out.len();
        for (name, _, calls) in &info {
            if calls.iter().any(|c| out.contains(c)) {
                out.insert(name.clone());
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// Does this block contain a `return` or `break` anywhere?
fn may_exit(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Return(..) | Stmt::Break(_) => true,
        Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => may_exit(body),
        Stmt::If { then, els, .. } => may_exit(then) || may_exit(els),
        _ => false,
    })
}

/// Adds sources to a set, as "derived" (inexact), without duplicates.
fn add_srcs(into: &mut Vec<Src>, from: Vec<Src>) {
    for s in from {
        if !into.iter().any(|x| x.path == s.path && !x.exact) {
            into.push(Src { path: s.path, exact: false });
        }
    }
}

fn check_ty_known(ctx: &mut Ctx, ty: &Ty, pos: Pos) {
    match ty {
        Ty::Named(n) if !ctx.known.contains(n) => ctx.err("E102", pos, format!("unknown type '{n}'")),
        Ty::Proposed(inner) => check_ty_known(ctx, inner, pos),
        _ => {}
    }
}

/// Can a value of type `from` be placed in a slot of type `to`?
/// Returns the error code and message if not.
fn assignable(from: &Ty, to: &Ty) -> Result<(), (&'static str, String)> {
    match (from, to) {
        (Ty::Proposed(_), t) if !matches!(t, Ty::Proposed(_)) => {
            Err(("E001", format!("unvalidated {from} used where {to} is required; use `validate` first")))
        }
        (Ty::Cap { path: fp, max: fmax }, Ty::Cap { path: tp, max: tmax }) => {
            if fp != tp {
                return Err(("E101", format!("expected {to}, found {from}")));
            }
            match (fmax, tmax) {
                (Some(f), None) => Err((
                    "E003",
                    format!("capability limited to {f} cannot be widened to unlimited `cap {}`", tp.join(".")),
                )),
                (None, _) => Ok(()),
                (Some(f), Some(t)) => {
                    if f.unit != t.unit {
                        Err(("E003", format!("capability limit {f} cannot be converted to {t}; units differ")))
                    } else if f.amount < t.amount {
                        Err(("E003", format!("capability limited to {f} cannot be widened to {t}")))
                    } else {
                        Ok(())
                    }
                }
            }
        }
        _ if from == to => Ok(()),
        _ => Err(("E101", format!("expected {to}, found {from}"))),
    }
}

fn check_fn(ctx: &mut Ctx, f: &FnDecl) {
    let mut env = Env::new();
    ctx.fn_blind.clear();
    ctx.pc.clear();
    for p in &f.params {
        check_ty_known(ctx, &p.ty, f.pos);
        for rel in &p.blind {
            if !rel.is_empty() && !field_path_exists(ctx, &p.ty, rel) {
                ctx.err(
                    "E104",
                    f.pos,
                    format!("@blind on '{}': {} has no field path '{}'", p.name, p.ty, rel.join(".")),
                );
                continue;
            }
            let mut path = vec![p.name.clone()];
            path.extend(rel.iter().cloned());
            let text = path.join(".");
            ctx.fn_blind.push((path, format!("{text} (blinded by parameter '{}')", p.name)));
        }
        let reveals = vec![Src { path: vec![p.name.clone()], exact: true }];
        env.insert(p.name.clone(), Binding { ty: p.ty.clone(), trusted: p.trusted, reveals });
    }
    let ret = f.ret.clone().unwrap_or_else(|| named("Unit"));
    check_ty_known(ctx, &ret, f.pos);
    check_block(ctx, f, &ret, &f.body, env, false);
}

fn field_path_exists(ctx: &Ctx, ty: &Ty, rel: &[String]) -> bool {
    let mut cur = ty.clone();
    for name in rel {
        let Ty::Named(t) = &cur else { return false };
        let Some(fields) = ctx.fields.get(t) else { return false };
        let Some((_, fty)) = fields.iter().find(|(n, _)| n == name) else { return false };
        cur = fty.clone();
    }
    true
}

/// Checks a block. `env` is a copy, so names bound inside a nested block do not leak out.
///
/// `ctx.pc` holds what reaching the current point reveals: inside `if c { ... }` it
/// includes what `c` reveals, and after an `if` whose branch can `return` or `break`
/// it includes that condition for the rest of the block. Values bound here, and the
/// decision to call a model here, carry `ctx.pc`. Returns what this block's early
/// exits reveal, so enclosing blocks can taint the code that runs after them.
fn check_block(ctx: &mut Ctx, f: &FnDecl, ret: &Ty, body: &[Stmt], mut env: Env, in_loop: bool) -> Vec<Src> {
    let saved_pc = ctx.pc.clone();
    let mut exit_taint: Vec<Src> = Vec::new();
    for stmt in body {
        match stmt {
            Stmt::Let { name, ty, value, pos } => {
                let Some((vty, trusted)) = type_of(ctx, value, &env) else { continue };
                let bound = match ty {
                    Some(decl) => {
                        check_ty_known(ctx, decl, *pos);
                        if let Err((code, msg)) = assignable(&vty, decl) {
                            ctx.err(code, *pos, format!("in `let {name}`: {msg}"));
                        }
                        decl.clone()
                    }
                    None => vty,
                };
                let mut r = reveals(value, &env);
                add_srcs(&mut r, ctx.pc.clone());
                env.insert(name.clone(), Binding { ty: bound, trusted, reveals: r });
            }
            Stmt::Expr(e) => {
                type_of(ctx, e, &env);
            }
            Stmt::Return(None, pos) => {
                if *ret != named("Unit") {
                    ctx.err("E101", *pos, format!("'{}' must return a {ret}; bare `return` gives nothing", f.name));
                }
            }
            Stmt::Return(Some(e), pos) => {
                if let Some((vty, trusted)) = type_of(ctx, e, &env) {
                    if let Err((code, msg)) = assignable(&vty, ret) {
                        ctx.err(code, *pos, format!("in return from '{}': {msg}", f.name));
                    } else if f.ret_trusted && !trusted {
                        ctx.err(
                            "E006",
                            *pos,
                            format!("'{}' promises an @trusted result but returns an untrusted value", f.name),
                        );
                    }
                }
            }
            Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => {
                // A `break` taken in one iteration decides whether later iterations run,
                // so the body is checked under its own exit taint, repeated until stable.
                // Diagnostics from the trial passes are discarded.
                let outer_pc = ctx.pc.clone();
                let mut taint: Vec<Src> = Vec::new();
                for _ in 0..8 {
                    let mark = ctx.diags.len();
                    ctx.pc = outer_pc.clone();
                    add_srcs(&mut ctx.pc, taint.clone());
                    let t = check_block(ctx, f, ret, body, env.clone(), true);
                    ctx.diags.truncate(mark);
                    let before = taint.len();
                    add_srcs(&mut taint, t);
                    if taint.len() == before {
                        break;
                    }
                }
                ctx.pc = outer_pc;
                add_srcs(&mut ctx.pc, taint.clone());
                let t = check_block(ctx, f, ret, body, env.clone(), true);
                add_srcs(&mut taint, t);
                ctx.pc = saved_pc.clone();
                add_srcs(&mut ctx.pc, exit_taint.clone());
                add_srcs(&mut exit_taint, taint.clone());
                add_srcs(&mut ctx.pc, taint);
            }
            Stmt::If { cond, then, els, .. } => {
                if let Some((cty, _)) = type_of(ctx, cond, &env) {
                    match cty {
                        Ty::Proposed(_) => ctx.err(
                            "E001",
                            cond.pos(),
                            format!("unvalidated {cty} used as a condition; use `validate` first"),
                        ),
                        t if t != named("Bool") => {
                            ctx.err("E101", cond.pos(), format!("`if` condition must be Bool, found {t}"))
                        }
                        _ => {}
                    }
                }
                let cond_srcs = reveals(cond, &env);
                let here = ctx.pc.clone();
                add_srcs(&mut ctx.pc, cond_srcs.clone());
                let t1 = check_block(ctx, f, ret, then, env.clone(), in_loop);
                let t2 = check_block(ctx, f, ret, els, env.clone(), in_loop);
                ctx.pc = here;
                if may_exit(then) || may_exit(els) {
                    let mut t = Vec::new();
                    add_srcs(&mut t, cond_srcs);
                    add_srcs(&mut t, t1);
                    add_srcs(&mut t, t2);
                    add_srcs(&mut exit_taint, t.clone());
                    add_srcs(&mut ctx.pc, t);
                }
            }
            Stmt::Break(pos) => {
                if !in_loop {
                    ctx.err("E103", *pos, "`break` is only allowed inside `repeat` or `loop`".into());
                }
            }
        }
    }
    ctx.pc = saved_pc;
    exit_taint
}

/// E007: no input of a model call may reveal a path the call is blind to, either
/// through `blind_to` on the call or through an @blind parameter of this function.
/// Whether the call happens at all counts as an input (`ctx.pc`).
fn check_blind(ctx: &mut Ctx, instruct: Option<&Expr>, data: &[Expr], blind_to: &[Expr], env: &Env, pos: Pos) {
    let mut blind: Vec<(Vec<String>, String)> = Vec::new();
    for b in blind_to {
        if !is_path(b) {
            ctx.err("E101", b.pos(), "`blind_to` entries must be names or fields, e.g. order.customer.email".into());
            continue;
        }
        if type_of(ctx, b, env).is_none() {
            continue; // undefined name or unknown field, already reported
        }
        let srcs = reveals(b, env);
        if srcs.is_empty() || srcs.iter().any(|s| !s.exact) {
            ctx.err(
                "E101",
                b.pos(),
                format!(
                    "`blind_to` must name data directly; '{}' is a computed value, so name the data it came from",
                    expr_text(b)
                ),
            );
            continue;
        }
        for s in srcs {
            blind.push((s.path, expr_text(b)));
        }
    }
    blind.extend(ctx.fn_blind.iter().cloned());

    let inputs = instruct.into_iter().chain(data.iter());
    for input in inputs {
        let mut reported = HashSet::new();
        for src in reveals(input, env) {
            for (bpath, btext) in &blind {
                if overlaps(&src.path, bpath) && reported.insert(btext.clone()) {
                    ctx.err(
                        "E007",
                        input.pos(),
                        format!("model input '{}' can reveal '{}', which this call is blind to", expr_text(input), btext),
                    );
                }
            }
        }
    }
    let mut reported = HashSet::new();
    for src in ctx.pc.clone() {
        for (bpath, btext) in &blind {
            if overlaps(&src.path, bpath) && reported.insert(btext.clone()) {
                ctx.err(
                    "E007",
                    pos,
                    format!("whether this model call happens depends on '{btext}', which it is blind to"),
                );
            }
        }
    }
}

/// E007 at a call: blinded data passed to another function must stay blinded there.
fn check_call_blind(ctx: &mut Ctx, name: &str, args: &[Expr], params: &[(Ty, bool, Vec<Vec<String>>)], env: &Env, pos: Pos) {
    let fn_blind = ctx.fn_blind.clone();
    // A function that cannot reach a model (directly or through calls) cannot leak to one.
    // Its result still reveals its arguments, which the caller tracks.
    let callee_uses_models = ctx.model_fns.contains(name);
    for (arg, (_, _, pblind)) in args.iter().zip(params).filter(|_| callee_uses_models) {
        let mut reported = HashSet::new();
        for src in reveals(arg, env) {
            for (bpath, btext) in &fn_blind {
                if !overlaps(&src.path, bpath) {
                    continue;
                }
                // If the argument is exactly a parent of the blinded path, the callee must
                // blind the remaining sub-path (or a parent of it). Otherwise the argument
                // lies inside the blinded data or is computed from it: the whole parameter
                // must be blind.
                let covered = if src.exact && bpath.len() > src.path.len() {
                    let rest = &bpath[src.path.len()..];
                    pblind.iter().any(|q| q.len() <= rest.len() && q[..] == rest[..q.len()])
                } else {
                    pblind.iter().any(|q| q.is_empty())
                };
                if !covered && reported.insert(btext.clone()) {
                    let need = if src.exact && bpath.len() > src.path.len() {
                        format!("@blind({})", bpath[src.path.len()..].join("."))
                    } else {
                        "@blind".to_string()
                    };
                    ctx.err(
                        "E007",
                        arg.pos(),
                        format!(
                            "in call to '{name}': argument '{}' can reveal '{btext}', but '{name}' does not \
                             promise to keep it from models; mark that parameter {need}",
                            expr_text(arg)
                        ),
                    );
                }
            }
        }
    }
    if ctx.model_fns.contains(name) {
        let mut reported = HashSet::new();
        for src in ctx.pc.clone() {
            for (bpath, btext) in &fn_blind {
                if overlaps(&src.path, bpath) && reported.insert(btext.clone()) {
                    ctx.err(
                        "E007",
                        pos,
                        format!("'{name}' makes model calls, and whether it runs here depends on '{btext}'"),
                    );
                }
            }
        }
    }
}

/// Returns the expression's type and whether its value is trusted.
fn type_of(ctx: &mut Ctx, e: &Expr, env: &Env) -> Option<(Ty, bool)> {
    match e {
        Expr::Str(..) => Some((named("Text"), true)),
        Expr::Int(..) => Some((named("Int"), true)),
        Expr::Bool(..) => Some((named("Bool"), true)),
        Expr::Var(n, pos) => match env.get(n) {
            Some(b) => Some((b.ty.clone(), b.trusted)),
            None => {
                ctx.err("E100", *pos, format!("undefined name '{n}'"));
                None
            }
        },
        Expr::Field { base, field, pos } => {
            let (bty, trusted) = type_of(ctx, base, env)?;
            match &bty {
                Ty::Proposed(_) => {
                    ctx.err(
                        "E001",
                        *pos,
                        format!("cannot read '.{field}' of unvalidated {bty}; use `validate` first"),
                    );
                    None
                }
                Ty::Named(tname) => match ctx.fields.get(tname).map(|fs| fs.iter().find(|(n, _)| n == field)) {
                    Some(Some((_, fty))) => Some((fty.clone(), trusted)),
                    Some(None) => {
                        ctx.err("E104", *pos, format!("type '{tname}' has no field '{field}'"));
                        None
                    }
                    None => {
                        ctx.err("E104", *pos, format!("{tname} has no fields; cannot read '.{field}'"));
                        None
                    }
                },
                Ty::Cap { .. } => {
                    ctx.err("E104", *pos, format!("a capability has no fields; cannot read '.{field}'"));
                    None
                }
            }
        }
        Expr::Cmp { op, left, right, pos } => {
            let l = type_of(ctx, left, env);
            let r = type_of(ctx, right, env);
            let ((lt, ltr), (rt, rtr)) = (l?, r?);
            for t in [&lt, &rt] {
                if let Ty::Proposed(_) = t {
                    ctx.err("E001", *pos, format!("unvalidated {t} compared with `{op}`; use `validate` first"));
                    return Some((named("Bool"), false));
                }
            }
            let ordered = matches!(op, CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge);
            if matches!(lt, Ty::Cap { .. }) || matches!(rt, Ty::Cap { .. }) {
                ctx.err("E101", *pos, "capabilities cannot be compared".into());
            } else if lt != rt {
                ctx.err("E101", *pos, format!("cannot compare {lt} with {rt}"));
            } else if ordered && lt != named("Int") && lt != named("Money") {
                ctx.err("E101", *pos, format!("`{op}` needs Int or Money, found {lt}"));
            }
            Some((named("Bool"), ltr && rtr))
        }
        Expr::Propose { ty, instruct, data, blind_to, pos, .. } => {
            check_ty_known(ctx, ty, *pos);
            check_blind(ctx, instruct.as_deref(), data, blind_to, env, *pos);
            if let Some(ins) = instruct {
                if let Some((ity, trusted)) = type_of(ctx, ins, env) {
                    if ity != named("Text") {
                        ctx.err("E101", ins.pos(), format!("`instruct` must be Text, found {ity}"));
                    } else if !trusted {
                        ctx.err(
                            "E006",
                            ins.pos(),
                            "untrusted value used as a model instruction; pass it in `data: [...]` instead, \
                             or mark its source @trusted"
                                .into(),
                        );
                    }
                }
            }
            for d in data {
                if let Some((Ty::Cap { .. }, _)) = type_of(ctx, d, env) {
                    ctx.err("E101", d.pos(), "a capability cannot be sent to a model as data".into());
                }
            }
            Some((Ty::Proposed(Box::new(ty.clone())), false))
        }
        Expr::Validate(inner, pos) => match type_of(ctx, inner, env)? {
            // Validation checks the shape of model output, not its intent: it stays untrusted.
            (Ty::Proposed(t), _) => Some((*t, false)),
            (other, _) => {
                ctx.err("E101", *pos, format!("`validate` expects a Proposed value, found {other}"));
                None
            }
        },
        Expr::Call { name, args, pos } => {
            let arg_tys: Vec<Option<(Ty, bool)>> = args.iter().map(|a| type_of(ctx, a, env)).collect();
            let Some(sig) = ctx.sigs.get(name.as_str()) else {
                ctx.err("E100", *pos, format!("undefined function '{name}'"));
                return None;
            };
            let (params, ret, ret_trusted) = (sig.params.clone(), sig.ret.clone(), sig.ret_trusted);
            if params.len() == args.len() {
                check_call_blind(ctx, name, args, &params, env, *pos);
            }
            if params.len() != args.len() {
                ctx.err(
                    "E101",
                    *pos,
                    format!("'{name}' takes {} argument(s) but {} were given", params.len(), args.len()),
                );
                return Some((ret, ret_trusted));
            }
            for ((arg, aty), (pty, ptrusted, _)) in args.iter().zip(arg_tys).zip(&params) {
                let Some((aty, atrusted)) = aty else { continue };
                if let Err((code, msg)) = assignable(&aty, pty) {
                    ctx.err(code, arg.pos(), format!("in call to '{name}': {msg}"));
                } else if *ptrusted && !atrusted {
                    ctx.err(
                        "E006",
                        arg.pos(),
                        format!("in call to '{name}': untrusted value passed to an @trusted parameter"),
                    );
                }
            }
            Some((ret, ret_trusted))
        }
        Expr::Action { receiver, method, args, pos } => {
            match env.get(receiver).map(|b| &b.ty) {
                Some(Ty::Cap { path, .. }) if path.last() == Some(method) => {}
                Some(Ty::Cap { path, .. }) => {
                    let p = path.join(".");
                    ctx.err("E002", *pos, format!("capability '{receiver}' is `cap {p}` and does not permit '{method}'"))
                }
                _ => ctx.err(
                    "E002",
                    *pos,
                    format!("'{receiver}.{method}' acts on the world but '{receiver}' is not a capability in scope"),
                ),
            }
            for a in args {
                if let Some((Ty::Proposed(inner), _)) = type_of(ctx, a, env) {
                    ctx.err(
                        "E001",
                        a.pos(),
                        format!("unvalidated Proposed<{inner}> passed to action '{receiver}.{method}'; use `validate` first"),
                    );
                }
            }
            Some((named("Receipt"), false))
        }
    }
}
