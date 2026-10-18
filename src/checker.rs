// Type checker. Enforces:
//   E001  a model's Proposed<T> value is used where a plain T is needed, without `validate`
//   E002  an action is performed without a matching capability in scope
//   E003  a capability is widened (given more authority than it holds)
//   E006  an untrusted value is used as a model instruction, or flows into an @trusted slot
//   E007  a model call's input, or the decision to make it, can reveal a path the call
//         is blind to (via `blind_to` or an @blind parameter); or blinded data is passed
//         to a function that does not promise to keep it blinded. "The decision to make
//         it" covers branches, early exits, and operations that might not finish
//         (actions can fail; `loop` and recursion might not end).
// plus ordinary errors:
//   E100  undefined name or function
//   E101  type mismatch or wrong number of arguments
//   E102  unknown type
//   E103  `break` outside a loop
//   E104  unknown field, or field access on a value that has no fields
//   E105  a function with a return type can finish without returning
//
// Lists are bounded (`List<T, N>`), so every `for` loop is bounded: its maximum
// iteration count is recorded for the cost analysis (see `check_with_bounds`).
//   E008  `done(evidence: [...])` cites something that is not proof of this function's work:
//         not a Receipt / checked Outcome / Done, an unchecked Outcome, a parameter,
//         or no evidence at all
//
// `try action` / `try call(...)` turns a failure into an `Outcome { ok, error }` value
// instead of stopping the function, so code after it runs either way.
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
    /// Parameter names of the function being checked.
    params: HashSet<String>,
    /// The maximum iteration count of each `for` loop, by position, for the cost analysis.
    for_bounds: HashMap<Pos, u64>,
    /// Functions that may fail: an action, or a call to a failing function, not under `try`.
    fail_fns: HashSet<String>,
    /// Functions that may never finish: `loop` or recursion, directly or through any call.
    diverge_fns: HashSet<String>,
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
    /// For an `Outcome`: the code has checked `.ok` is true on this path.
    proven_ok: bool,
    /// For an `Outcome`: it was made by `try` in this function around an action, or
    /// around a call that returns a Receipt or a Done, so on success it holds proof.
    holds_evidence: bool,
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
        // An element of a list is part of the list: it reveals what the list does, exactly,
        // so `xs[i].email` narrows to the email path. Which element was picked reveals `i`.
        Expr::Index { base, index, .. } => {
            let mut v = reveals(base, env);
            v.extend(inexact(reveals(index, env)));
            v
        }
        Expr::Validate(inner, _) | Expr::Try(inner, _) | Expr::Neg(inner, _) | Expr::Not(inner, _) => {
            inexact(reveals(inner, env))
        }
        Expr::Cmp { left, right, .. } | Expr::Arith { left, right, .. } | Expr::Logic { left, right, .. } => {
            let mut v = reveals(left, env);
            v.extend(reveals(right, env));
            inexact(v)
        }
        Expr::Call { args, .. }
        | Expr::Action { args, .. }
        | Expr::Done { evidence: args, .. }
        | Expr::Builtin { args, .. }
        | Expr::List(args, _) => inexact(args.iter().flat_map(|a| reveals(a, env)).collect()),
        // Conservative: every field of a record literal counts as revealing all of them.
        Expr::Record { fields, .. } => inexact(fields.iter().flat_map(|(_, v)| reveals(v, env)).collect()),
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
        Expr::Try(inner, _) => format!("try {}", expr_text(inner)),
        Expr::Propose { .. } => "propose(...)".into(),
        Expr::Done { .. } => "done(...)".into(),
        Expr::Cmp { left, op, right, .. } => format!("{} {op} {}", expr_text(left), expr_text(right)),
        Expr::Arith { left, op, right, .. } => format!("{} {op} {}", expr_text(left), expr_text(right)),
        Expr::Logic { left, op, right, .. } => {
            let word = if *op == LogicOp::And { "and" } else { "or" };
            format!("{} {word} {}", expr_text(left), expr_text(right))
        }
        Expr::Neg(inner, _) => format!("-{}", expr_text(inner)),
        Expr::Not(inner, _) => format!("not {}", expr_text(inner)),
        Expr::Builtin { f, .. } => format!("{}(...)", f.name()),
        Expr::Record { ty, .. } => format!("{ty} {{...}}"),
        Expr::List(..) => "[...]".into(),
        Expr::Index { base, .. } => format!("{}[...]", expr_text(base)),
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
    check_with_bounds(prog).0
}

/// Checks a program. Also returns the maximum iteration count of every `for` loop,
/// keyed by the loop's position, which the cost analysis needs.
pub fn check_with_bounds(prog: &Program) -> (Vec<Diagnostic>, HashMap<Pos, u64>) {
    let mut ctx = Ctx {
        known: ["Text", "Int", "Bool", "Money", "Unit", "Receipt"].iter().map(|s| s.to_string()).collect(),
        fields: HashMap::new(),
        sigs: HashMap::new(),
        diags: Vec::new(),
        pc: Vec::new(),
        fn_blind: Vec::new(),
        model_fns: model_calling_fns(prog),
        fail_fns: HashSet::new(),
        diverge_fns: HashSet::new(),
        params: HashSet::new(),
        for_bounds: HashMap::new(),
    };
    (ctx.fail_fns, ctx.diverge_fns) = partial_fns(prog);
    // Built-in result of `try`.
    ctx.known.insert("Outcome".into());
    ctx.known.insert("Done".into()); // built-in; made only by `done(evidence: [...])`
    ctx.fields.insert("Outcome".into(), vec![("ok".into(), named("Bool")), ("error".into(), named("Text"))]);
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
            if let Some(bad) = proof_or_authority(fty) {
                let why = if bad == "capability" {
                    "pass capabilities as parameters so they cannot travel inside data"
                } else {
                    "receipts, outcomes and Done values are proof made by the runtime; they flow directly \
                     and never travel inside data, where they could be copied or forged"
                };
                ctx.err("E101", t.pos, format!("field '{}.{fname}' cannot hold a {bad}: {why}", t.name));
            }
        }
        ctx.fields.insert(t.name.clone(), t.fields.clone());
    }
    for f in &prog.fns {
        if Builtin::from_name(&f.name).is_some() {
            ctx.err("E101", f.pos, format!("'{}' is a built-in function and cannot be redefined", f.name));
            continue;
        }
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
    (ctx.diags, ctx.for_bounds)
}

/// Calls `f` on every expression in `body`, nested blocks included, outermost first.
fn walk_block<'a>(body: &'a [Stmt], f: &mut impl FnMut(&'a Expr)) {
    for s in body {
        for e in s.exprs() {
            walk_expr(e, f);
        }
        for b in s.blocks() {
            walk_block(b, f);
        }
    }
}

fn walk_expr<'a>(e: &'a Expr, f: &mut impl FnMut(&'a Expr)) {
    f(e);
    for c in e.children() {
        walk_expr(c, f);
    }
}

/// Names of functions that can make a model call, directly or through calls.
fn model_calling_fns(prog: &Program) -> HashSet<String> {
    let mut info = Vec::new();
    for f in &prog.fns {
        let (mut direct, mut calls) = (false, HashSet::new());
        walk_block(&f.body, &mut |e| match e {
            Expr::Propose { .. } => direct = true,
            Expr::Call { name, .. } => {
                calls.insert(name.clone());
            }
            _ => {}
        });
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

/// Whether evaluating `e` itself can fail, apart from what its children do:
/// arithmetic (overflow, division by zero, money in different currencies or below
/// zero) and indexing (out of range). Actions and calls are judged separately.
fn expr_can_fail_itself(e: &Expr) -> bool {
    matches!(e, Expr::Arith { .. } | Expr::Neg(..) | Expr::Index { .. })
}

/// Per function: whether it may fail (an action or a failing call not directly under
/// `try`, or arithmetic or indexing anywhere) and whether it may never finish (`loop`,
/// recursion, or a call to such a function; `try` cannot catch that). A `for` always
/// finishes: its list is bounded. Returns (may_fail, may_diverge).
fn partial_fns(prog: &Program) -> (HashSet<String>, HashSet<String>) {
    #[derive(Default)]
    struct Info {
        fails: bool,
        diverges: bool,
        calls_untried: HashSet<String>,
        calls_tried: HashSet<String>,
    }
    /// `tried`: `e` sits directly under `try`.
    fn scan_expr(e: &Expr, tried: bool, i: &mut Info) {
        match e {
            Expr::Action { .. } if !tried => i.fails = true,
            Expr::Call { name, .. } => {
                if tried {
                    i.calls_tried.insert(name.clone());
                } else {
                    i.calls_untried.insert(name.clone());
                }
            }
            _ if expr_can_fail_itself(e) => i.fails = true,
            _ => {}
        }
        let under_try = matches!(e, Expr::Try(..));
        for c in e.children() {
            scan_expr(c, under_try, i);
        }
    }
    fn scan_block(body: &[Stmt], i: &mut Info) {
        for s in body {
            if let Stmt::Loop { .. } = s {
                i.diverges = true; // `loop` may never end
            }
            for e in s.exprs() {
                scan_expr(e, false, i);
            }
            for b in s.blocks() {
                scan_block(b, i);
            }
        }
    }
    let mut graph: HashMap<String, Info> = HashMap::new();
    for f in &prog.fns {
        let mut i = Info::default();
        scan_block(&f.body, &mut i);
        graph.entry(f.name.clone()).or_insert(i);
    }
    // Recursion: a function that can reach itself through any call may never end.
    let reaches_self = |start: &str| -> bool {
        let next = |n: &str| -> Vec<String> {
            graph.get(n).map(|i| i.calls_untried.iter().chain(&i.calls_tried).cloned().collect()).unwrap_or_default()
        };
        let mut seen = HashSet::new();
        let mut stack = next(start);
        while let Some(n) = stack.pop() {
            if n == start {
                return true;
            }
            if seen.insert(n.clone()) {
                stack.extend(next(&n));
            }
        }
        false
    };
    let mut fails: HashSet<String> = graph.iter().filter(|(_, i)| i.fails).map(|(n, _)| n.clone()).collect();
    let mut diverges: HashSet<String> =
        graph.iter().filter(|(n, i)| i.diverges || reaches_self(n)).map(|(n, _)| n.clone()).collect();
    loop {
        let before = (fails.len(), diverges.len());
        for (name, i) in &graph {
            if i.calls_untried.iter().any(|c| fails.contains(c)) {
                fails.insert(name.clone());
            }
            if i.calls_untried.iter().chain(&i.calls_tried).any(|c| diverges.contains(c)) {
                diverges.insert(name.clone());
            }
        }
        if (fails.len(), diverges.len()) == before {
            return (fails, diverges);
        }
    }
}

/// Is this operation partial, i.e. can execution fail to get past it? `tried` means it
/// sits directly under `try`, which catches failure but not a computation that never ends.
fn op_is_partial(ctx: &Ctx, e: &Expr, tried: bool) -> bool {
    match e {
        Expr::Action { .. } => !tried,
        Expr::Call { name, .. } => ctx.diverge_fns.contains(name) || (!tried && ctx.fail_fns.contains(name)),
        _ => expr_can_fail_itself(e),
    }
}

/// Whether anything inside `e` (itself included) is partial.
fn expr_has_partial(ctx: &Ctx, e: &Expr, tried: bool) -> bool {
    if op_is_partial(ctx, e, tried) {
        return true;
    }
    let under_try = matches!(e, Expr::Try(..));
    e.children().into_iter().any(|c| expr_has_partial(ctx, c, under_try))
}

/// Everything the partial operations in `e` depend on. Whether execution continues
/// past them can depend on those values.
fn partial_taint(ctx: &Ctx, e: &Expr, env: &Env) -> Vec<Src> {
    let mut out = Vec::new();
    collect_partial(ctx, e, env, false, &mut out);
    out
}

fn collect_partial(ctx: &Ctx, e: &Expr, env: &Env, tried: bool, out: &mut Vec<Src>) {
    if op_is_partial(ctx, e, tried) {
        // Whether it fails can depend on every value it is given.
        for c in e.children() {
            add_srcs(out, reveals(c, env));
        }
    }
    if let Expr::Logic { left, right, .. } = e {
        // The right side runs only for some values of the left side: if it can stop
        // the run, then whether the run continues depends on the left side too.
        if expr_has_partial(ctx, right, false) {
            add_srcs(out, reveals(left, env));
        }
    }
    let under_try = matches!(e, Expr::Try(..));
    for c in e.children() {
        collect_partial(ctx, c, env, under_try, out);
    }
}

/// Does this block contain anything that can stop execution early: `return`, `break`,
/// `loop`, or a partial operation?
fn may_stop(ctx: &Ctx, body: &[Stmt]) -> bool {
    body.iter().any(|s| {
        matches!(s, Stmt::Return(..) | Stmt::Break(_) | Stmt::Loop { .. })
            || s.exprs().into_iter().any(|e| expr_has_partial(ctx, e, false))
            || s.blocks().into_iter().any(|b| may_stop(ctx, b))
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
        Ty::Named(_) | Ty::Cap { .. } => {}
        Ty::Proposed(inner) => check_ty_known(ctx, inner, pos),
        Ty::List(inner, _) => {
            if let Some(bad) = proof_or_authority(inner) {
                ctx.err("E101", pos, format!("a list cannot hold a {bad}"));
            }
            check_ty_known(ctx, inner, pos);
        }
    }
}

/// A capability or runtime-made proof (Receipt, Outcome, Done) anywhere in `ty`,
/// through lists and Proposed. Such values are never stored inside data.
fn proof_or_authority(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::Cap { .. } => Some("capability"),
        Ty::Named(n) if n == "Receipt" => Some("Receipt"),
        Ty::Named(n) if n == "Outcome" => Some("Outcome"),
        Ty::Named(n) if n == "Done" => Some("Done"),
        Ty::Named(_) => None,
        Ty::Proposed(inner) | Ty::List(inner, _) => proof_or_authority(inner),
    }
}

/// Can a value of type `from` be placed in a slot of type `to`?
/// Returns the error code and message if not.
fn assignable(from: &Ty, to: &Ty) -> Result<(), (&'static str, String)> {
    match (from, to) {
        (Ty::List(fe, fmax), Ty::List(te, tmax)) => {
            assignable(fe, te)?;
            if fe != te {
                return Err(("E101", format!("expected {to}, found {from}")));
            }
            if fmax > tmax {
                return Err((
                    "E101",
                    format!("a list of up to {fmax} cannot go where at most {tmax} are allowed ({from} into {to})"),
                ));
            }
            Ok(())
        }
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
    ctx.params = f.params.iter().map(|p| p.name.clone()).collect();
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
        // A parameter's Outcome came from the caller: it is never this function's proof.
        let b = Binding { ty: p.ty.clone(), trusted: p.trusted, reveals, proven_ok: false, holds_evidence: false };
        env.insert(p.name.clone(), b);
    }
    let ret = f.ret.clone().unwrap_or_else(|| named("Unit"));
    check_ty_known(ctx, &ret, f.pos);
    check_block(ctx, f, &ret, &f.body, env, false);
    if ret != named("Unit") && !always_returns(&f.body) {
        ctx.err("E105", f.pos, format!("'{}' can finish without returning a value of type {ret}", f.name));
    }
}

fn f_params_contains(ctx: &Ctx, name: &str) -> bool {
    ctx.params.contains(name)
}

/// Whether binding `value` yields an Outcome that holds proof on success: it is
/// `try` around an action, or around a call to a function returning Receipt or Done,
/// or an alias of such an Outcome. A Receipt or Done that a callee returns has itself
/// passed E008, so it is real proof of work done by that call.
fn outcome_holds_evidence(ctx: &Ctx, value: &Expr, env: &Env) -> bool {
    match value {
        Expr::Try(inner, _) => match inner.as_ref() {
            Expr::Action { .. } => true,
            Expr::Call { name, .. } => ctx.sigs.get(name.as_str()).is_some_and(|s| is_proof_type(&s.ret)),
            _ => false,
        },
        Expr::Var(v, _) => env.get(v).is_some_and(|b| b.holds_evidence),
        _ => false,
    }
}

fn is_proof_type(t: &Ty) -> bool {
    matches!(t, Ty::Named(n) if n == "Receipt" || n == "Done")
}

/// Whether `e` is (an alias of) one of this function's parameters.
fn is_param_value(ctx: &Ctx, e: &Expr, env: &Env) -> bool {
    reveals(e, env).iter().any(|s| s.exact && f_params_contains(ctx, &s.path[0]))
}

/// Does every path through this block end in `return` (or in a `loop` that is never left)?
fn always_returns(body: &[Stmt]) -> bool {
    fn has_break(body: &[Stmt]) -> bool {
        body.iter().any(|s| match s {
            Stmt::Break(_) => true,
            Stmt::If { then, els, .. } => has_break(then) || has_break(els),
            Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => has_break(body),
            _ => false,
        })
    }
    body.iter().any(|s| match s {
        Stmt::Return(..) => true,
        Stmt::If { then, els, .. } => always_returns(then) && always_returns(els),
        Stmt::Loop { body, .. } => !has_break(body),
        _ => false,
    })
}

/// Recognises `x.ok`, `x.ok == true`, `x.ok != false` (positive) and `x.ok == false`,
/// `x.ok != true` (negative), where x is a variable.
fn ok_test(cond: &Expr) -> Option<(String, bool)> {
    fn ok_var(e: &Expr) -> Option<String> {
        match e {
            Expr::Field { base, field, .. } if field == "ok" => match base.as_ref() {
                Expr::Var(v, _) => Some(v.clone()),
                _ => None,
            },
            _ => None,
        }
    }
    if let Some(v) = ok_var(cond) {
        return Some((v, true));
    }
    if let Expr::Cmp { op, left, right, .. } = cond {
        let (v, lit) = match (ok_var(left), ok_var(right), left.as_ref(), right.as_ref()) {
            (Some(v), _, _, Expr::Bool(b, _)) => (v, *b),
            (_, Some(v), Expr::Bool(b, _), _) => (v, *b),
            _ => return None,
        };
        return match op {
            CmpOp::Eq => Some((v, lit)),
            CmpOp::Ne => Some((v, !lit)),
            _ => None,
        };
    }
    None
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
        // If this statement contains an action or a call that might not finish, whether
        // execution gets past it can depend on that operation's arguments. That holds for
        // model calls later in this statement and everything after it.
        let stmt_expr = match stmt {
            Stmt::Let { value: e, .. } | Stmt::Expr(e) | Stmt::Return(Some(e), _) => Some(e),
            Stmt::If { cond, .. } => Some(cond),
            _ => None,
        };
        if let Some(e) = stmt_expr {
            let pt = partial_taint(ctx, e, &env);
            if !pt.is_empty() {
                add_srcs(&mut ctx.pc, pt.clone());
                add_srcs(&mut exit_taint, pt);
            }
        }
        match stmt {
            Stmt::Let { name, ty, value, pos } => {
                // `[]` has no element type of its own: it takes the declared list type.
                if let (Expr::List(items, _), Some(decl @ Ty::List(..))) = (value, ty)
                    && items.is_empty()
                {
                    check_ty_known(ctx, decl, *pos);
                    let b = Binding { ty: decl.clone(), trusted: true, reveals: Vec::new(), proven_ok: false, holds_evidence: false };
                    env.insert(name.clone(), b);
                    continue;
                }
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
                // `let y = x` keeps what is known about x's outcome.
                let proven_ok = matches!(value, Expr::Var(v, _) if env.get(v).is_some_and(|b| b.proven_ok));
                let holds_evidence = outcome_holds_evidence(ctx, value, &env);
                env.insert(name.clone(), Binding { ty: bound, trusted, reveals: r, proven_ok, holds_evidence });
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
                    } else if is_proof_type(ret) && is_param_value(ctx, e, &env) {
                        // A Receipt or Done returned from here is proof for the caller; it must
                        // come from this function's own work, never be passed straight through.
                        ctx.err(
                            "E008",
                            *pos,
                            format!(
                                "'{}' returns a {ret} it was given by its caller; a {ret} returned from \
                                 a function must prove that function's own work",
                                f.name
                            ),
                        );
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
                let taint = check_loop(ctx, f, ret, body, env.clone(), &[]);
                ctx.pc = saved_pc.clone();
                add_srcs(&mut ctx.pc, exit_taint.clone());
                add_srcs(&mut exit_taint, taint.clone());
                add_srcs(&mut ctx.pc, taint);
            }
            Stmt::For { var, list, body, pos } => {
                let Some((lty, ltrusted)) = type_of(ctx, list, &env) else { continue };
                let Ty::List(el, max) = &lty else {
                    let code = if matches!(lty, Ty::Proposed(_)) { "E001" } else { "E101" };
                    ctx.err(code, list.pos(), format!("`for` needs a list, found {lty}"));
                    continue;
                };
                ctx.for_bounds.insert(*pos, *max);
                // Each element is part of the list: it reveals what the list does, exactly.
                let list_srcs = reveals(list, &env);
                let mut body_env = env.clone();
                let b = Binding {
                    ty: (**el).clone(),
                    trusted: ltrusted,
                    reveals: list_srcs.clone(),
                    proven_ok: false,
                    holds_evidence: false,
                };
                body_env.insert(var.clone(), b);
                // How many times the body runs reveals the list's length, so everything the
                // body does depends on the list, like the body of an `if` on its condition.
                let mut count = Vec::new();
                add_srcs(&mut count, list_srcs);
                let mut taint = check_loop(ctx, f, ret, body, body_env, &count);
                if may_stop(ctx, body) {
                    add_srcs(&mut taint, count);
                }
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
                // `if x.ok { ... }` proves x succeeded inside the branch; and after
                // `if x.ok == false { return }` it is proven for the rest of the block.
                let mut then_env = env.clone();
                let mut else_env = env.clone();
                let mut after_if_proven: Option<String> = None;
                if let Some((var, positive)) = ok_test(cond) {
                    if positive {
                        if let Some(b) = then_env.get_mut(&var) {
                            b.proven_ok = true;
                        }
                        if always_returns(els) && !els.is_empty() {
                            after_if_proven = Some(var);
                        }
                    } else {
                        if let Some(b) = else_env.get_mut(&var) {
                            b.proven_ok = true;
                        }
                        if always_returns(then) {
                            after_if_proven = Some(var);
                        }
                    }
                }
                let t1 = check_block(ctx, f, ret, then, then_env, in_loop);
                let t2 = check_block(ctx, f, ret, els, else_env, in_loop);
                if let Some(var) = after_if_proven
                    && let Some(b) = env.get_mut(&var) {
                        b.proven_ok = true;
                    }
                ctx.pc = here;
                // Code after this `if` only runs through a branch that can complete
                // normally. A branch that always returns cannot influence it, so its
                // inner taint is dropped; if both always return, nothing after runs at
                // all, so the condition cannot influence it either.
                let (then_ret, else_ret) = (always_returns(then), always_returns(els));
                let mut t = Vec::new();
                if !then_ret {
                    add_srcs(&mut t, t1);
                }
                if !else_ret {
                    add_srcs(&mut t, t2);
                }
                if !(then_ret && else_ret) && (may_stop(ctx, then) || may_stop(ctx, els)) {
                    add_srcs(&mut t, cond_srcs);
                }
                if !t.is_empty() {
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

/// Checks a loop body (`repeat`, `loop`, `for`) under `extra` in addition to the
/// current pc. A `break` taken in one iteration decides whether later iterations run,
/// so the body is checked again under its own exit taint until that is stable; the
/// diagnostics of the trial passes are discarded. Exit taint does not depend on the pc
/// (it is the inputs of partial operations and the conditions of stopping branches),
/// so this settles after one extra pass; the bound of 8 is a safeguard. Returns the
/// body's exit taint. Leaves `ctx.pc` for the caller to set.
fn check_loop(ctx: &mut Ctx, f: &FnDecl, ret: &Ty, body: &[Stmt], env: Env, extra: &[Src]) -> Vec<Src> {
    let mut base = ctx.pc.clone();
    add_srcs(&mut base, extra.to_vec());
    let mut taint: Vec<Src> = Vec::new();
    for _ in 0..8 {
        let mark = ctx.diags.len();
        ctx.pc = base.clone();
        add_srcs(&mut ctx.pc, taint.clone());
        let t = check_block(ctx, f, ret, body, env.clone(), true);
        ctx.diags.truncate(mark);
        let before = taint.len();
        add_srcs(&mut taint, t);
        if taint.len() == before {
            break;
        }
    }
    ctx.pc = base;
    add_srcs(&mut ctx.pc, taint.clone());
    let t = check_block(ctx, f, ret, body, env, true);
    add_srcs(&mut taint, t);
    taint
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

/// The first runtime-only type (Receipt, Outcome, Done, a capability) found anywhere
/// inside `ty`, following record fields. `seen` guards against recursive types.
fn runtime_only_inside(ctx: &Ctx, ty: &Ty, seen: &mut HashSet<String>) -> Option<String> {
    match ty {
        Ty::Cap { .. } => Some("capability".into()),
        Ty::Proposed(inner) | Ty::List(inner, _) => runtime_only_inside(ctx, inner, seen),
        Ty::Named(n) if n == "Receipt" || n == "Outcome" || n == "Done" => Some(n.clone()),
        Ty::Named(n) => {
            if !seen.insert(n.clone()) {
                return None;
            }
            let fields = ctx.fields.get(n)?.clone();
            fields.iter().find_map(|(_, fty)| runtime_only_inside(ctx, fty, seen))
        }
    }
}

/// Returns the expression's type and whether its value is trusted.
fn type_of(ctx: &mut Ctx, e: &Expr, env: &Env) -> Option<(Ty, bool)> {
    match e {
        Expr::Str(..) => Some((named("Text"), true)),
        Expr::Int(n, pos) => {
            if *n > i64::MAX as u64 {
                ctx.err("E101", *pos, format!("integer {n} is too large; the largest Int is {}", i64::MAX));
            }
            Some((named("Int"), true))
        }
        Expr::Bool(..) => Some((named("Bool"), true)),
        Expr::Var(n, pos) => match env.get(n) {
            Some(b) => Some((b.ty.clone(), b.trusted)),
            None => {
                ctx.err("E100", *pos, format!("undefined name '{n}'"));
                None
            }
        },
        Expr::Done { evidence, pos } => {
            if evidence.is_empty() {
                ctx.err("E008", *pos, "`done` needs at least one piece of evidence".into());
            }
            for item in evidence {
                let Some((ity, _)) = type_of(ctx, item, env) else { continue };
                let text = expr_text(item);
                match &ity {
                    Ty::Named(n) if n == "Receipt" || n == "Done" => {}
                    Ty::Named(n) if n == "Outcome" => {
                        let binding = match item {
                            Expr::Var(v, _) => env.get(v),
                            _ => None,
                        };
                        if !binding.is_some_and(|b| b.holds_evidence) {
                            ctx.err(
                                "E008",
                                item.pos(),
                                format!(
                                    "'{text}' does not hold proof of work: an Outcome is evidence only when it \
                                     comes from `try` in this function around an action, or around a call that \
                                     returns a Receipt or a Done"
                                ),
                            );
                            continue;
                        }
                        if !binding.is_some_and(|b| b.proven_ok) {
                            ctx.err(
                                "E008",
                                item.pos(),
                                format!("'{text}' may be a failure; check `{text}.ok` before citing it as evidence"),
                            );
                            continue;
                        }
                    }
                    other => {
                        ctx.err(
                            "E008",
                            item.pos(),
                            format!(
                                "'{text}' is a {other}, not evidence; evidence is an action's Receipt, a checked \
                                 Outcome, or another function's Done"
                            ),
                        );
                        continue;
                    }
                }
                // Evidence must come from this function's own work, not from a caller.
                if is_param_value(ctx, item, env) {
                    ctx.err(
                        "E008",
                        item.pos(),
                        format!("'{text}' was passed in by the caller, so it proves nothing about this function's work"),
                    );
                }
            }
            Some((named("Done"), false))
        }
        Expr::Try(inner, pos) => {
            if !matches!(inner.as_ref(), Expr::Action { .. } | Expr::Call { .. }) {
                ctx.err("E101", *pos, "`try` applies to an action or a function call".into());
                return None;
            }
            // All the usual checks (capabilities, validation, trust, blinding) still apply.
            type_of(ctx, inner, env)?;
            // The outcome comes from the outside world, so it is untrusted.
            Some((named("Outcome"), false))
        }
        Expr::Arith { op, left, right, pos } => {
            let l = type_of(ctx, left, env);
            let r = type_of(ctx, right, env);
            let ((lt, ltr), (rt, rtr)) = (l?, r?);
            for t in [&lt, &rt] {
                if let Ty::Proposed(_) = t {
                    ctx.err("E001", *pos, format!("unvalidated {t} used with `{op}`; use `validate` first"));
                    return None;
                }
            }
            let (int, money, text) = (named("Int"), named("Money"), named("Text"));
            let result = if lt == int && rt == int {
                int
            } else if (lt == money && rt == money && matches!(op, ArithOp::Add | ArithOp::Sub))
                || (*op == ArithOp::Mul && ((lt == money && rt == int) || (lt == int && rt == money)))
            {
                money
            } else if *op == ArithOp::Div && lt == money && rt == int {
                ctx.err(
                    "E101",
                    *pos,
                    "money cannot be divided: dividing needs a rounding rule, and choosing one silently loses \
                     or invents cents; compute in minor units with Int instead"
                        .into(),
                );
                return None;
            } else if *op == ArithOp::Add && lt == text && rt == text {
                text
            } else {
                ctx.err("E101", *pos, format!("cannot apply `{op}` to {lt} and {rt}"));
                return None;
            };
            Some((result, ltr && rtr))
        }
        Expr::Neg(inner, pos) => {
            let (t, tr) = type_of(ctx, inner, env)?;
            match &t {
                Ty::Named(n) if n == "Int" => Some((t, tr)),
                Ty::Proposed(_) => {
                    ctx.err("E001", *pos, format!("unvalidated {t} negated; use `validate` first"));
                    None
                }
                Ty::Named(n) if n == "Money" => {
                    ctx.err("E101", *pos, "money cannot be negative".into());
                    None
                }
                _ => {
                    ctx.err("E101", *pos, format!("`-` needs an Int, found {t}"));
                    None
                }
            }
        }
        Expr::Logic { op, left, right, pos } => {
            let l = type_of(ctx, left, env);
            // The right side runs only for some values of the left side, so everything it
            // does is decided by the left side, as the body of an `if` is by its condition.
            let here = ctx.pc.clone();
            add_srcs(&mut ctx.pc, reveals(left, env));
            let r = type_of(ctx, right, env);
            ctx.pc = here;
            let ((lt, ltr), (rt, rtr)) = (l?, r?);
            let word = if *op == LogicOp::And { "and" } else { "or" };
            for t in [&lt, &rt] {
                if let Ty::Proposed(_) = t {
                    ctx.err("E001", *pos, format!("unvalidated {t} used with `{word}`; use `validate` first"));
                    return None;
                }
                if *t != named("Bool") {
                    ctx.err("E101", *pos, format!("`{word}` needs Bool on both sides, found {t}"));
                    return None;
                }
            }
            Some((named("Bool"), ltr && rtr))
        }
        Expr::Not(inner, pos) => {
            let (t, tr) = type_of(ctx, inner, env)?;
            if let Ty::Proposed(_) = t {
                ctx.err("E001", *pos, format!("unvalidated {t} used with `not`; use `validate` first"));
                return None;
            }
            if t != named("Bool") {
                ctx.err("E101", *pos, format!("`not` needs a Bool, found {t}"));
                return None;
            }
            Some((t, tr))
        }
        Expr::Builtin { f, args, pos } => {
            let tys: Vec<Option<(Ty, bool)>> = args.iter().map(|a| type_of(ctx, a, env)).collect();
            let arity = if *f == Builtin::Len { 1 } else { 2 };
            if args.len() != arity {
                ctx.err("E101", *pos, format!("`{}` takes {arity} argument(s) but {} were given", f.name(), args.len()));
                return None;
            }
            let tys: Vec<(Ty, bool)> = tys.into_iter().collect::<Option<Vec<_>>>()?;
            for (t, _) in &tys {
                if let Ty::Proposed(_) = t {
                    ctx.err("E001", *pos, format!("unvalidated {t} given to `{}`; use `validate` first", f.name()));
                    return None;
                }
            }
            let trusted = tys.iter().all(|(_, tr)| *tr);
            let text = named("Text");
            let ok = match f {
                Builtin::Len => matches!(&tys[0].0, Ty::List(..)) || tys[0].0 == text,
                Builtin::Contains => match (&tys[0].0, &tys[1].0) {
                    (a, b) if *a == text && *b == text => true,
                    (Ty::List(el, _), b) => **el == *b,
                    _ => false,
                },
                Builtin::StartsWith | Builtin::EndsWith => tys[0].0 == text && tys[1].0 == text,
            };
            if !ok {
                let found: Vec<String> = tys.iter().map(|(t, _)| t.to_string()).collect();
                let want = match f {
                    Builtin::Len => "Text or a list",
                    Builtin::Contains => "(Text, Text) or (a list, a value of its element type)",
                    Builtin::StartsWith | Builtin::EndsWith => "(Text, Text)",
                };
                ctx.err("E101", *pos, format!("`{}` needs {want}, found ({})", f.name(), found.join(", ")));
                return None;
            }
            let result = if *f == Builtin::Len { named("Int") } else { named("Bool") };
            Some((result, trusted))
        }
        Expr::Record { ty, fields, pos } => {
            // Outcome is a built-in record made only by `try`; other built-ins have no fields.
            let decl = match ctx.fields.get(ty) {
                Some(d) if ty != "Outcome" => d.clone(),
                _ => {
                    if ctx.known.contains(ty) {
                        ctx.err("E101", *pos, format!("a {ty} cannot be built in code; only the runtime makes one"));
                    } else {
                        ctx.err("E102", *pos, format!("unknown type '{ty}'"));
                    }
                    return None;
                }
            };
            let mut trusted = true;
            let mut seen = HashSet::new();
            let mut ok = true;
            for (fname, value) in fields {
                let vt = type_of(ctx, value, env);
                if !seen.insert(fname.as_str()) {
                    ctx.err("E101", value.pos(), format!("field '{fname}' given twice in this {ty}"));
                    ok = false;
                    continue;
                }
                let Some((_, fty)) = decl.iter().find(|(n, _)| n == fname) else {
                    ctx.err("E104", value.pos(), format!("type '{ty}' has no field '{fname}'"));
                    ok = false;
                    continue;
                };
                let Some((vty, vtr)) = vt else {
                    ok = false;
                    continue;
                };
                trusted &= vtr;
                if let Err((code, msg)) = assignable(&vty, fty) {
                    ctx.err(code, value.pos(), format!("in field '{fname}' of {ty}: {msg}"));
                    ok = false;
                }
            }
            for (fname, _) in &decl {
                if !fields.iter().any(|(n, _)| n == fname) {
                    ctx.err("E101", *pos, format!("this {ty} is missing field '{fname}'"));
                    ok = false;
                }
            }
            if !ok {
                return None;
            }
            Some((named(ty), trusted))
        }
        Expr::List(items, pos) => {
            if items.is_empty() {
                ctx.err(
                    "E101",
                    *pos,
                    "an empty list has no element type; give it one in a typed let: let xs: List<Text, 10> = []".into(),
                );
                return None;
            }
            let tys: Vec<Option<(Ty, bool)>> = items.iter().map(|i| type_of(ctx, i, env)).collect();
            let tys: Vec<(Ty, bool)> = tys.into_iter().collect::<Option<Vec<_>>>()?;
            let first = tys[0].0.clone();
            if let Some(bad) = proof_or_authority(&first) {
                ctx.err("E101", *pos, format!("a list cannot hold a {bad}"));
                return None;
            }
            for (item, (t, _)) in items.iter().zip(&tys).skip(1) {
                if *t != first {
                    ctx.err("E101", item.pos(), format!("list elements must all have one type: {first} and {t}"));
                    return None;
                }
            }
            let trusted = tys.iter().all(|(_, tr)| *tr);
            Some((Ty::List(Box::new(first), items.len() as u64), trusted))
        }
        Expr::Index { base, index, pos } => {
            let b = type_of(ctx, base, env);
            let i = type_of(ctx, index, env);
            let ((bt, btr), (it, _)) = (b?, i?);
            let Ty::List(el, _) = &bt else {
                let code = if matches!(bt, Ty::Proposed(_)) { "E001" } else { "E101" };
                ctx.err(code, *pos, format!("only a list can be indexed, found {bt}"));
                return None;
            };
            if it != named("Int") {
                ctx.err("E101", index.pos(), format!("a list index must be an Int, found {it}"));
                return None;
            }
            // Choosing among a list's values by any index yields one of those values, so
            // the element is exactly as trusted as the list.
            Some(((**el).clone(), btr))
        }
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
                Ty::List(..) => {
                    ctx.err("E104", *pos, format!("a list has no fields; cannot read '.{field}' (use len(xs) or xs[i])"));
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
            if let Some(bad) = runtime_only_inside(ctx, ty, &mut HashSet::new()) {
                ctx.err(
                    "E101",
                    *pos,
                    format!(
                        "a model cannot produce a {bad}: only the runtime creates receipts, outcomes and \
                         Done values, so model output of type {ty} could forge one"
                    ),
                );
            }
            check_blind(ctx, instruct.as_deref(), data, blind_to, env, *pos);
            if let Some(ins) = instruct
                && let Some((ity, trusted)) = type_of(ctx, ins, env) {
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
