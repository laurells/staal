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
//   (E106 and E107, import errors, come from `crate::modules`; E108, a package using
//   authority its manifest does not declare, from `crate::packages`.)
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
// Reassignment: `set x = e` gives a `let` variable a new value. The analysis follows
// the code in order. After `set x = e`, x reveals what e reveals plus what reaching
// that `set` reveals (its branch conditions, loop counts, partial operations before
// it), and is trusted only if e is. Where paths meet (after an `if`, at a loop's head,
// after a loop), a variable reveals what it reveals on any path that arrives there,
// and is trusted only if it is trusted on all of them. A loop is checked again until
// what its variables reveal at its head stops growing. Reading a variable before a
// tainted `set` therefore sees only what it held then; in a loop, the next iteration
// sees the later value.
//
// Trust: string, number and boolean literals are trusted. Parameters are
// untrusted unless marked `@trusted`, and callers must pass trusted values to
// those. Model output (validated or not), action results and function results
// are untrusted unless the function's return is marked `@trusted`. A comparison
// is trusted only if both sides are. Nothing else can make a value trusted.

use crate::ast::*;
use crate::lexer::Pos;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
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
    /// `fails E`: what its `fail(...)` raises.
    fails: Option<Ty>,
    /// Whether other modules may call it, and the file it is in: a method is called by
    /// its type, not by a name an import could check.
    public: bool,
    file: u32,
}

/// While checking a capability action's body: the `@target` and `@amount` parameters
/// must reach a request made through one of its needed capabilities, or the program's
/// limits would be checked against a value that is never used.
struct ActionFlow {
    needs: HashSet<String>,
    target: Option<String>,
    amount: Option<String>,
    request: bool,
    target_used: bool,
    amount_used: bool,
}

struct Ctx<'a> {
    known: HashSet<String>,
    /// Declared types and their fields, in declaration order.
    fields: HashMap<String, Vec<(String, Ty)>>,
    /// Declared enums: their variants, each with its fields in declaration order.
    enums: HashMap<String, Variants>,
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
    /// The capacity of the map or set each `put` and `add` grows, by its position.
    capacities: HashMap<Pos, u64>,
    /// Each method call found, by its position: the method it calls.
    methods: HashMap<Pos, String>,
    /// Problems with method calls found while resolving them, which the plain calls
    /// they become no longer show (a method that is not `pub`).
    method_diags: Vec<Diagnostic>,
    /// Actions of library capabilities, by the path programs use (`termii.sms.send`):
    /// the types of the parameters a program passes.
    lib_actions: HashMap<String, Vec<Ty>>,
    /// The body of a capability action being checked: what must reach a request.
    flow: Option<ActionFlow>,
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
    /// The position of the `let` that declared it; None for parameters and loop variables,
    /// which cannot be reassigned.
    decl: Option<Pos>,
    /// For an `Outcome` from `try` around a call to a function declaring `fails E`: E,
    /// so `.failure` is an `Optional<E>`.
    raises: Option<Ty>,
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
        Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) | Expr::Money(..) => Vec::new(),
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
        Expr::Parse { text, .. } => inexact(reveals(text, env)),
        Expr::Named { value, .. } => reveals(value, env),
        // `some(x)` is x, wrapped: what is inside it is exactly x.
        Expr::SomeOf(inner, _) => reveals(inner, env),
        Expr::NoneLit(_) => Vec::new(),
        Expr::Cmp { left, right, .. } | Expr::Arith { left, right, .. } | Expr::Logic { left, right, .. } => {
            let mut v = reveals(left, env);
            v.extend(reveals(right, env));
            inexact(v)
        }
        Expr::Method { base, args, .. } => {
            inexact(std::iter::once(base.as_ref()).chain(args.iter()).flat_map(|a| reveals(a, env)).collect())
        }
        Expr::Call { args, .. }
        | Expr::Action { args, .. }
        | Expr::Done { evidence: args, .. }
        | Expr::Builtin { args, .. }
        | Expr::List(args, _) => inexact(args.iter().flat_map(|a| reveals(a, env)).collect()),
        // Conservative: every element of a tuple literal counts as revealing all of them.
        Expr::Tuple(items, _) => inexact(items.iter().flat_map(|a| reveals(a, env)).collect()),
        Expr::MapLit(entries, _) => inexact(entries.iter().flat_map(|(k, v)| reveals(k, env).into_iter().chain(reveals(v, env))).collect()),
        // Conservative: every field of a record literal counts as revealing all of them.
        Expr::Record { fields, .. } | Expr::Variant { fields, .. } => {
            inexact(fields.iter().flat_map(|(_, v)| reveals(v, env)).collect())
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
        Expr::Method { base, method, .. } => format!("{}.{method}(...)", expr_text(base)),
        Expr::Action { receiver, method, .. } => format!("{receiver}.{method}(...)"),
        Expr::Validate(inner, _) => format!("validate {}", expr_text(inner)),
        Expr::Try(inner, _) => format!("try {}", expr_text(inner)),
        Expr::Propose { .. } => "propose(...)".into(),
        Expr::Parse { .. } => "parse(...)".into(),
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
        Expr::Variant { ty, variant, .. } => format!("{ty}.{variant}"),
        Expr::List(..) => "[...]".into(),
        Expr::Tuple(..) => "(...)".into(),
        Expr::MapLit(..) => "[...: ...]".into(),
        Expr::Index { base, .. } => format!("{}[...]", expr_text(base)),
        Expr::SomeOf(inner, _) => format!("some({})", expr_text(inner)),
        Expr::NoneLit(_) => "none".into(),
        Expr::Named { name, value, .. } => format!("{name}: {}", expr_text(value)),
        Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) | Expr::Money(..) => "literal".into(),
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

/// What checking found beyond diagnostics: the bound of each `for` (for the budget
/// check) and the capacity each `put` and `add` needs (`supply_capacities`).
pub type Found = (Vec<Diagnostic>, HashMap<Pos, u64>, HashMap<Pos, u64>);

/// Checks a program. Also returns the maximum iteration count of every `for` loop,
/// keyed by the loop's position, which the cost analysis needs.
pub fn check_with_bounds(prog: &Program) -> Found {
    check_once(prog).0
}

/// Checks a program whose method calls may not be resolved yet. Resolving one needs
/// the type of what it is called on, which only checking finds: so the program is
/// checked, each method call found is rewritten into a plain call of that method
/// (`c.total()` into `Charge::total(c)`), and the result is checked again, until
/// nothing is left to resolve. The last check sees only plain calls, so every rule
/// applies to a method exactly as to any function.
pub fn check_and_resolve(prog: &mut Program) -> Found {
    let mut resolving: Vec<Diagnostic> = Vec::new();
    let finish = |mut found: Found, resolving: Vec<Diagnostic>| {
        for d in resolving {
            if !found.0.contains(&d) {
                found.0.push(d);
            }
        }
        found
    };
    for _ in 0..8 {
        let (found, methods) = check_once(prog);
        if methods.is_empty() {
            return finish(found, resolving);
        }
        resolving.extend(found.0.into_iter().filter(|d| d.code == "E107" && d.msg.starts_with("method '")));
        resolve_methods(prog, &methods);
        // Arguments of method calls are completed (named, defaults) now they are calls.
        let diags = crate::calls::complete(prog, true);
        if !diags.is_empty() {
            return (diags, HashMap::new(), HashMap::new());
        }
    }
    let (mut found, methods) = check_once(prog);
    for pos in methods.keys() {
        found.0.push(Diagnostic { code: "E101", pos: *pos, msg: "this method call could not be resolved".into() });
    }
    finish(found, resolving)
}

/// Rewrites each method call `methods` names into a call of that method, with what
/// it is called on as the first argument.
fn resolve_methods(prog: &mut Program, methods: &HashMap<Pos, String>) {
    fn expr(e: &mut Expr, m: &HashMap<Pos, String>) {
        for c in e.children_mut() {
            expr(c, m);
        }
        let call = match e {
            Expr::Action { receiver, args, pos, .. } => m.get(pos).map(|name| {
                let mut all = vec![Expr::Var(receiver.clone(), *pos)];
                all.append(args);
                Expr::Call { name: name.clone(), args: all, slots: Vec::new(), pos: *pos }
            }),
            Expr::Method { base, args, pos, .. } => m.get(pos).map(|name| {
                let pos = *pos;
                let mut all = vec![std::mem::replace(base.as_mut(), Expr::Bool(false, pos))];
                all.append(args);
                Expr::Call { name: name.clone(), args: all, slots: Vec::new(), pos }
            }),
            _ => None,
        };
        if let Some(c) = call {
            *e = c;
        }
    }
    fn block(body: &mut [Stmt], m: &HashMap<Pos, String>) {
        for s in body {
            match s {
                Stmt::Let { value, .. } | Stmt::Set { value, .. } | Stmt::Expr(value) | Stmt::Return(Some(value), _) | Stmt::Fail(value, _) => {
                    expr(value, m)
                }
                Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => {}
                Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => block(body, m),
                Stmt::For { list, body, .. } => {
                    expr(list, m);
                    block(body, m);
                }
                Stmt::If { cond, then, els, .. } => {
                    expr(cond, m);
                    block(then, m);
                    block(els, m);
                }
                Stmt::Match { value, arms, .. } => {
                    expr(value, m);
                    for a in arms {
                        block(&mut a.body, m);
                    }
                }
            }
        }
    }
    for f in &mut prog.fns {
        block(&mut f.body, methods);
    }
}

fn check_once(prog: &Program) -> (Found, HashMap<Pos, String>) {
    let mut ctx = Ctx {
        known: ["Text", "Int", "Bool", "Money", "Unit", "Receipt"].iter().map(|s| s.to_string()).collect(),
        fields: HashMap::new(),
        enums: HashMap::new(),
        sigs: HashMap::new(),
        diags: Vec::new(),
        pc: Vec::new(),
        fn_blind: Vec::new(),
        model_fns: model_calling_fns(prog),
        fail_fns: HashSet::new(),
        diverge_fns: HashSet::new(),
        params: HashSet::new(),
        for_bounds: HashMap::new(),
        capacities: HashMap::new(),
        methods: HashMap::new(),
        method_diags: Vec::new(),
        lib_actions: HashMap::new(),
        flow: None,
    };
    (ctx.fail_fns, ctx.diverge_fns) = partial_fns(prog);
    // Built-in result of `try`.
    ctx.known.insert("Outcome".into());
    ctx.known.insert("Done".into()); // built-in; made only by `done(evidence: [...])`
    ctx.fields.insert("Outcome".into(), vec![("ok".into(), named("Bool")), ("error".into(), named("Text"))]);
    // Built-in for net.http: a secret from the run config, a header, and a response.
    for t in ["Secret", "Header", "Response"] {
        ctx.known.insert(t.into());
    }
    ctx.fields.insert(
        "Response".into(),
        vec![("status".into(), named("Int")), ("body".into(), named("Text")), ("complete".into(), named("Bool"))],
    );
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
            if let Some(bad) = secret_inside(&ctx, fty, &mut HashSet::new()) {
                ctx.err(
                    "E101",
                    t.pos,
                    format!(
                        "field '{}.{fname}' cannot hold a {bad}: secrets never travel inside data, where they could \
                         be logged, encoded or sent to a model",
                        t.name
                    ),
                );
            }
        }
        ctx.fields.insert(t.name.clone(), t.fields.clone());
    }
    for e in &prog.enums {
        if ctx.known.contains(&e.name) {
            ctx.err("E101", e.pos, format!("type '{}' is already defined", e.name));
            continue;
        }
        ctx.known.insert(e.name.clone());
        let mut names = HashSet::new();
        for v in &e.variants {
            if !names.insert(v.name.as_str()) {
                ctx.err("E101", v.pos, format!("enum '{}' declares variant '{}' twice", e.name, v.name));
            }
            let mut seen = HashSet::new();
            for (fname, fty) in &v.fields {
                let at = format!("{}.{}.{fname}", e.name, v.name);
                if !seen.insert(fname.as_str()) {
                    ctx.err("E101", v.pos, format!("variant '{}.{}' declares field '{fname}' twice", e.name, v.name));
                }
                // In JSON a variant is {"kind": "...", fields...}.
                if fname == "kind" {
                    ctx.err("E101", v.pos, format!("field '{at}' cannot be called `kind`: JSON uses it to name the variant"));
                }
                if let Some(bad) = proof_or_authority(fty) {
                    ctx.err("E101", v.pos, format!("field '{at}' cannot hold a {bad}: it would travel inside data"));
                }
                if let Some(bad) = secret_inside(&ctx, fty, &mut HashSet::new()) {
                    ctx.err("E101", v.pos, format!("field '{at}' cannot hold a {bad}: secrets never travel inside data"));
                }
            }
        }
        ctx.enums.insert(e.name.clone(), e.variants.iter().map(|v| (v.name.clone(), v.fields.clone())).collect());
    }
    for f in &prog.fns {
        // A library's `len` is `lib.len`: the name a call is written with is the last part.
        let base = f.name.rsplit('.').next().unwrap_or(&f.name);
        if Builtin::from_name(base).is_some() || base == "parse" {
            ctx.err("E101", f.pos, format!("'{}' is a built-in function and cannot be redefined", f.name));
            continue;
        }
        if base == "some" || base == "none" {
            ctx.err("E101", f.pos, format!("'{base}' is reserved for optional values; choose another name"));
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
                fails: f.fails.clone(),
                public: f.public,
                file: f.pos.file,
            },
        );
    }
    for t in &prog.types {
        for (_, fty) in &t.fields {
            check_ty_known(&mut ctx, fty, t.pos);
        }
    }
    for e in &prog.enums {
        for v in &e.variants {
            for (_, fty) in &v.fields {
                check_ty_known(&mut ctx, fty, v.pos);
            }
        }
    }
    // Every use of an alias is already its type; the type is checked here once, so an
    // alias that names nothing is an error even where it is never used.
    for a in &prog.aliases {
        check_ty_known(&mut ctx, &a.ty, a.pos);
    }
    // Constants (`crate::consts`): every use is already replaced by the value, so here
    // each is checked once: its type, and that computing it succeeds.
    for c in &prog.consts {
        if let Some(t) = &c.ty {
            let before = ctx.diags.len();
            check_ty_known(&mut ctx, t, c.pos);
            if ctx.diags.len() > before {
                continue;
            }
        }
        let Some((vty, _)) = type_of(&mut ctx, &c.value, &Env::new()) else { continue };
        if let Some(decl) = &c.ty
            && let Err((code, msg)) = assignable(&vty, decl)
        {
            ctx.err(code, c.pos, format!("in `const {}`: {msg}", c.name));
            continue;
        }
        if let Err(f) = crate::interp::eval_const(&c.value, prog) {
            ctx.err("E101", c.pos, format!("constant '{}' cannot be computed: {}", c.name, f.message));
        }
    }
    check_caps(&mut ctx, prog);
    // A default is a constant: data, of its parameter's type, known when checked.
    for f in &prog.fns {
        for p in &f.params {
            let Some(d) = &p.default else { continue };
            if matches!(p.ty, Ty::Cap { .. })
                || proof_or_authority(&p.ty).is_some()
                || secret_inside(&ctx, &p.ty, &mut HashSet::new()).is_some()
            {
                ctx.err("E101", d.pos(), format!("parameter '{}' of type {} cannot have a default: a default is data", p.name, p.ty));
                continue;
            }
            if let Some((t, _)) = type_of(&mut ctx, d, &Env::new())
                && let Err((code, msg)) = assignable(&t, &p.ty)
            {
                ctx.err(code, d.pos(), format!("default of parameter '{}': {msg}", p.name));
                continue;
            }
            if let Err(fl) = crate::interp::eval_const(d, prog) {
                ctx.err("E101", d.pos(), format!("default of parameter '{}' cannot be computed: {}", p.name, fl.message));
            }
        }
    }
    for f in &prog.fns {
        check_fn(&mut ctx, f, prog);
    }
    // A constant's value appears wherever it is used, so a problem inside it would be
    // reported once per use: report each exact diagnostic once, where first found.
    let mut seen = HashSet::new();
    ctx.diags.retain(|d| seen.insert((d.code, d.pos, d.msg.clone())));
    let mut diags = ctx.diags;
    diags.extend(ctx.method_diags);
    ((diags, ctx.for_bounds, ctx.capacities), ctx.methods)
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
    // `parse` fails on text that is not JSON; `json` on a result too long; `header` on
    // a name or value that is not valid in HTTP.
    matches!(
        e,
        Expr::Arith { .. }
            | Expr::Neg(..)
            | Expr::Index { .. }
            | Expr::Parse { .. }
            | Expr::Builtin {
                f: Builtin::Json | Builtin::Header | Builtin::FillSecret | Builtin::Replace | Builtin::Form | Builtin::Lower | Builtin::UrlSegment,
                ..
            }
    )
}

/// The capabilities the runtime implements itself (RUNTIME.md section 12). Every other
/// capability path is performed by an adapter and returns a Receipt.
#[derive(Clone, Copy, PartialEq)]
enum RuntimeCap {
    /// `net.http.<method>`; `body` for post, put and patch.
    Http { body: bool },
    FsRead,
    FsWrite,
}

const HTTP_METHODS: &[&str] = &["get", "post", "put", "patch", "delete"];

fn runtime_cap(path: &[String]) -> Option<RuntimeCap> {
    match path.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["net", "http", m] if HTTP_METHODS.contains(m) => Some(RuntimeCap::Http { body: matches!(*m, "post" | "put" | "patch") }),
        ["fs", "read"] => Some(RuntimeCap::FsRead),
        ["fs", "write"] => Some(RuntimeCap::FsWrite),
        _ => None,
    }
}

/// A Secret, or a Header (which may hold one), anywhere inside `ty`. Such values only
/// ever leave the program as an HTTP header: never as text, data, model input or JSON.
fn secret_inside(ctx: &Ctx, ty: &Ty, seen: &mut HashSet<String>) -> Option<&'static str> {
    match ty {
        Ty::Named(n) if n == "Secret" => Some("Secret"),
        Ty::Named(n) if n == "Header" => Some("Header"),
        Ty::Named(n) => {
            if !seen.insert(n.clone()) {
                return None;
            }
            inner_types(ctx, n)?.iter().find_map(|fty| secret_inside(ctx, fty, seen))
        }
        Ty::Proposed(inner) | Ty::List(inner, _) | Ty::Optional(inner) => secret_inside(ctx, inner, seen),
        Ty::Tuple(items) => items.iter().find_map(|t| secret_inside(ctx, t, seen)),
        Ty::Map(k, v, _) => secret_inside(ctx, k, seen).or_else(|| secret_inside(ctx, v, seen)),
        Ty::Set(t, _) => secret_inside(ctx, t, seen),
        Ty::Cap { .. } => None,
    }
}

/// The types directly inside named type `n`: a record's fields, or the fields of every
/// variant of an enum. None for a built-in type.
fn inner_types(ctx: &Ctx, n: &str) -> Option<Vec<Ty>> {
    if let Some(fs) = ctx.fields.get(n) {
        return Some(fs.iter().map(|(_, t)| t.clone()).collect());
    }
    let vs = ctx.enums.get(n)?;
    Some(vs.iter().flat_map(|(_, fs)| fs.iter().map(|(_, t)| t.clone())).collect())
}

/// A tuple, map or set anywhere inside `ty`, following records and enums: shapes a
/// model's output schema cannot state exactly for every provider.
fn no_schema_inside(ctx: &Ctx, ty: &Ty, seen: &mut HashSet<String>) -> Option<&'static str> {
    match ty {
        Ty::Tuple(_) => Some("a tuple, whose positions"),
        Ty::Map(..) => Some("a map, whose keys"),
        Ty::Set(..) => Some("a set, whose items"),
        Ty::Proposed(inner) | Ty::List(inner, _) | Ty::Optional(inner) => no_schema_inside(ctx, inner, seen),
        Ty::Named(n) => {
            if !seen.insert(n.clone()) {
                return None;
            }
            inner_types(ctx, n)?.iter().find_map(|t| no_schema_inside(ctx, t, seen))
        }
        Ty::Cap { .. } => None,
    }
}

/// Whether a capability appears anywhere inside `ty`.
fn cap_inside(ctx: &Ctx, ty: &Ty, seen: &mut HashSet<String>) -> bool {
    match ty {
        Ty::Cap { .. } => true,
        Ty::Proposed(inner) | Ty::List(inner, _) | Ty::Optional(inner) => cap_inside(ctx, inner, seen),
        Ty::Tuple(items) => items.iter().any(|t| cap_inside(ctx, t, seen)),
        Ty::Map(k, v, _) => cap_inside(ctx, k, seen) || cap_inside(ctx, v, seen),
        Ty::Set(t, _) => cap_inside(ctx, t, seen),
        Ty::Named(n) => {
            seen.insert(n.clone())
                && inner_types(ctx, n).is_some_and(|ts| ts.iter().any(|t| cap_inside(ctx, t, seen)))
        }
    }
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
            if let Stmt::Fail(..) = s {
                i.fails = true;
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
/// `continue` (the rest of the iteration), `loop`, or a partial operation?
fn may_stop(ctx: &Ctx, body: &[Stmt]) -> bool {
    body.iter().any(|s| {
        matches!(s, Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::Loop { .. } | Stmt::Fail(..))
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
        Ty::Named(_) => {}
        Ty::Cap { path, limits } => {
            if let (Some(m), Some(e)) = (&limits.max, &limits.each)
                && m.unit != e.unit
            {
                ctx.err(
                    "E101",
                    pos,
                    format!("`max: {m}` and `each: {e}` use different units, so no action could satisfy both"),
                );
            }
            // `net` and `fs` belong to the runtime (RUNTIME.md section 12).
            let p = path.join(".");
            let family = path.first().map(String::as_str);
            if matches!(family, Some("net") | Some("fs")) {
                match runtime_cap(path) {
                    None => ctx.err(
                        "E101",
                        pos,
                        format!(
                            "`cap {p}` is not a runtime capability; there are net.http.get, net.http.post, \
                             net.http.put, net.http.patch, net.http.delete, fs.read and fs.write"
                        ),
                    ),
                    Some(kind) => {
                        if limits.max.is_some() || limits.each.is_some() {
                            ctx.err(
                                "E101",
                                pos,
                                format!("`cap {p}` spends no money the runtime can measure, so `max` or `each` would refuse every action"),
                            );
                        }
                        for pat in limits.only.iter().flatten() {
                            let checked = match kind {
                                RuntimeCap::Http { .. } => crate::targets::check_url_pattern(pat),
                                RuntimeCap::FsRead | RuntimeCap::FsWrite => crate::targets::check_path_pattern(pat),
                            };
                            if let Err(e) = checked {
                                ctx.err("E101", pos, format!("in `cap {p}`: {e}"));
                            }
                        }
                    }
                }
            }
        }
        Ty::Proposed(inner) => check_ty_known(ctx, inner, pos),
        Ty::List(inner, _) => {
            if let Some(bad) = proof_or_authority(inner) {
                ctx.err("E101", pos, format!("a list cannot hold a {bad}"));
            }
            check_ty_known(ctx, inner, pos);
        }
        Ty::Optional(inner) => {
            if let Some(bad) = proof_or_authority(inner) {
                ctx.err("E101", pos, format!("an optional value cannot hold a {bad}"));
            } else if matches!(&**inner, Ty::Named(n) if n == "Secret" || n == "Header") {
                ctx.err("E101", pos, format!("an optional value cannot hold a {inner}: secrets never travel inside data"));
            } else if matches!(&**inner, Ty::Optional(_)) {
                // In JSON both `some(none)` and `none` would be `null`.
                ctx.err("E101", pos, format!("{ty} cannot be told apart from `none`; use one Optional, or an enum"));
            }
            check_ty_known(ctx, inner, pos);
        }
        Ty::Map(k, v, _) => {
            check_key(ctx, k, "a map's keys", pos);
            if let Some(bad) = proof_or_authority(v) {
                ctx.err("E101", pos, format!("a map cannot hold a {bad}"));
            } else if matches!(&**v, Ty::Named(n) if n == "Secret" || n == "Header") {
                ctx.err("E101", pos, format!("a map cannot hold a {v}: secrets never travel inside data"));
            }
            check_ty_known(ctx, v, pos);
        }
        Ty::Set(t, _) => check_key(ctx, t, "a set's items", pos),
        Ty::Tuple(items) => {
            for t in items {
                if let Some(bad) = proof_or_authority(t) {
                    ctx.err("E101", pos, format!("a tuple cannot hold a {bad}"));
                } else if matches!(t, Ty::Named(n) if n == "Secret" || n == "Header") {
                    ctx.err("E101", pos, format!("a tuple cannot hold a {t}: secrets never travel inside data"));
                }
                check_ty_known(ctx, t, pos);
            }
        }
    }
}

/// Whether two expressions are the same Text or Int literal.
fn literal_eq(a: &Expr, b: &Expr) -> bool {
    match (a, b) {
        (Expr::Str(x, _), Expr::Str(y, _)) => x == y,
        (Expr::Int(x, _), Expr::Int(y, _)) => x == y,
        _ => false,
    }
}

fn expr_text_literal(e: &Expr) -> String {
    match e {
        Expr::Str(s, _) => format!("{s:?}"),
        Expr::Int(n, _) => n.to_string(),
        _ => expr_text(e),
    }
}

/// The type of a map or set operation. `put` and `add` record the capacity of the
/// collection they grow, which the compiler then passes to them (`supply_capacities`).
fn collection_builtin(ctx: &mut Ctx, f: Builtin, tys: &[(Ty, bool)], trusted: bool, pos: Pos) -> Option<(Ty, bool)> {
    let shown: Vec<String> = tys.iter().map(|(t, _)| t.to_string()).collect();
    let mut wrong = |want: &str| {
        ctx.err("E101", pos, format!("`{}` needs {want}, found ({})", f.name(), shown.join(", ")));
        None
    };
    let arg = |i: usize| &tys[i].0;
    match (f, arg(0)) {
        (Builtin::Get, Ty::Map(k, v, _)) if arg(1) == &**k => Some((Ty::Optional(v.clone()), trusted)),
        (Builtin::Get, _) => wrong("(a map, a key)"),
        (Builtin::Put, m @ Ty::Map(k, v, n)) if arg(1) == &**k && arg(2).fits(v) => {
            let (m, n) = (m.clone(), *n);
            ctx.capacities.insert(pos, n);
            Some((m, trusted))
        }
        (Builtin::Put, _) => wrong("(a map, a key, a value)"),
        (Builtin::Remove, m @ Ty::Map(k, _, _)) if arg(1) == &**k => Some((m.clone(), trusted)),
        (Builtin::Remove, s @ Ty::Set(t, _)) if arg(1) == &**t => Some((s.clone(), trusted)),
        (Builtin::Remove, _) => wrong("(a map, a key) or (a set, an item)"),
        (Builtin::Keys, Ty::Map(k, _, n)) => Some((Ty::List(k.clone(), *n), trusted)),
        (Builtin::Values, Ty::Map(_, v, n)) => Some((Ty::List(v.clone(), *n), trusted)),
        (Builtin::Entries, Ty::Map(k, v, n)) => Some((Ty::List(Box::new(Ty::Tuple(vec![(**k).clone(), (**v).clone()])), *n), trusted)),
        (Builtin::Keys | Builtin::Values | Builtin::Entries, _) => wrong("a map"),
        (Builtin::Add, s @ Ty::Set(t, n)) if arg(1) == &**t => {
            let (s, n) = (s.clone(), *n);
            ctx.capacities.insert(pos, n);
            Some((s, trusted))
        }
        (Builtin::Add, _) => wrong("(a set, an item)"),
        (Builtin::Items, Ty::Set(t, n)) => Some((Ty::List(t.clone(), *n), trusted)),
        (Builtin::Items, _) => wrong("a set"),
        (Builtin::ToSet, Ty::List(t, n)) => {
            let s = Ty::Set(t.clone(), *n);
            check_ty_known(ctx, &s, pos);
            Some((s, trusted))
        }
        (Builtin::ToSet, _) => wrong("a list of Text or Int"),
        _ => wrong("a map or a set"),
    }
}

/// Passes `put` and `add` the capacity of the map or set they grow, as a last
/// argument: the capacity is the type's, which only the compiler knows.
pub fn supply_capacities(prog: &mut Program, capacities: &HashMap<Pos, u64>) {
    fn expr(e: &mut Expr, caps: &HashMap<Pos, u64>) {
        for c in e.children_mut() {
            expr(c, caps);
        }
        // Once: a call already given its capacity has one argument more than it takes.
        if let Expr::Builtin { f: f @ (Builtin::Put | Builtin::Add), args, pos } = e
            && let Some(&n) = caps.get(pos)
            && args.len() == f.arity()
        {
            args.push(Expr::Int(n, *pos));
        }
    }
    fn block(body: &mut [Stmt], caps: &HashMap<Pos, u64>) {
        for s in body {
            match s {
                Stmt::Let { value, .. } | Stmt::Set { value, .. } | Stmt::Expr(value) | Stmt::Return(Some(value), _) | Stmt::Fail(value, _) => {
                    expr(value, caps)
                }
                Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => {}
                Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => block(body, caps),
                Stmt::For { list, body, .. } => {
                    expr(list, caps);
                    block(body, caps);
                }
                Stmt::If { cond, then, els, .. } => {
                    expr(cond, caps);
                    block(then, caps);
                    block(els, caps);
                }
                Stmt::Match { value, arms, .. } => {
                    expr(value, caps);
                    for a in arms {
                        block(&mut a.body, caps);
                    }
                }
            }
        }
    }
    for f in &mut prog.fns {
        block(&mut f.body, capacities);
    }
}

/// Map keys and set items are Text or Int: kept in an order every engine agrees on,
/// and written as JSON keys exactly.
fn check_key(ctx: &mut Ctx, t: &Ty, what: &str, pos: Pos) {
    if *t != named("Text") && *t != named("Int") {
        ctx.err("E101", pos, format!("{what} are Text or Int, not {t}"));
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
        Ty::Proposed(inner) | Ty::List(inner, _) | Ty::Optional(inner) => proof_or_authority(inner),
        Ty::Tuple(items) => items.iter().find_map(proof_or_authority),
        Ty::Map(k, v, _) => proof_or_authority(k).or_else(|| proof_or_authority(v)),
        Ty::Set(t, _) => proof_or_authority(t),
    }
}

/// Can a value of type `from` be placed in a slot of type `to`?
/// Returns the error code and message if not.
fn assignable(from: &Ty, to: &Ty) -> Result<(), (&'static str, String)> {
    match (from, to) {
        (Ty::List(fe, fmax), Ty::List(te, tmax)) => {
            assignable(fe, te)?;
            if !fe.fits(te) {
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
        (Ty::Map(fk, fv, fmax), Ty::Map(tk, tv, tmax)) if fk == tk && fv.fits(tv) => {
            if fmax > tmax {
                return Err(("E101", format!("a map of up to {fmax} entries cannot go where at most {tmax} are allowed ({from} into {to})")));
            }
            Ok(())
        }
        (Ty::Set(ft, fmax), Ty::Set(tt, tmax)) if ft == tt => {
            if fmax > tmax {
                return Err(("E101", format!("a set of up to {fmax} items cannot go where at most {tmax} are allowed ({from} into {to})")));
            }
            Ok(())
        }
        (Ty::Proposed(_), t) if !matches!(t, Ty::Proposed(_)) => {
            Err(("E001", format!("unvalidated {from} used where {to} is required; use `validate` first")))
        }
        (Ty::Cap { path: fp, limits: fl }, Ty::Cap { path: tp, limits: tl }) => {
            if fp != tp {
                return Err(("E101", format!("expected {to}, found {from}")));
            }
            // Narrowing may only tighten: every limit the source has, the slot must have
            // too, at least as strict. A limit the source lacks may be added freely.
            narrow_money("max", &fl.max, &tl.max, tp)?;
            narrow_money("each", &fl.each, &tl.each, tp)?;
            match (fl.uses, tl.uses) {
                (Some(f), None) => {
                    return Err(("E003", format!("capability limited to {f} uses cannot be widened to unlimited uses")))
                }
                (Some(f), Some(t)) if t > f => {
                    return Err(("E003", format!("capability limited to {f} uses cannot be widened to {t} uses")))
                }
                _ => {}
            }
            match (&fl.only, &tl.only) {
                (Some(f), None) => {
                    return Err((
                        "E003",
                        format!("capability limited to only {f:?} cannot be widened to act on any target"),
                    ))
                }
                (Some(f), Some(t)) => {
                    if let Some(bad) = t.iter().find(|p| !crate::pattern::all_contained(std::slice::from_ref(*p), f)) {
                        return Err((
                            "E003",
                            format!(
                                "capability limited to only {f:?} cannot be widened to {t:?}: \"{bad}\" allows \
                                 targets the original does not"
                            ),
                        ));
                    }
                }
                _ => {}
            }
            Ok(())
        }
        _ if from.fits(to) => Ok(()),
        (t, Ty::Optional(inner)) if t.fits(inner) => {
            Err(("E101", format!("expected {to}, found {from}; write some(...) to make an optional value")))
        }
        _ => Err(("E101", format!("expected {to}, found {from}"))),
    }
}

/// One money limit (`max` or `each`) under narrowing: it may only shrink.
fn narrow_money(name: &str, from: &Option<Limit>, to: &Option<Limit>, path: &[String]) -> Result<(), (&'static str, String)> {
    match (from, to) {
        (Some(f), None) => Err((
            "E003",
            format!("capability `{name}: {f}` cannot be widened to an unlimited `{name}` on `cap {}`", path.join(".")),
        )),
        (Some(f), Some(t)) if f.unit != t.unit => {
            Err(("E003", format!("capability limit `{name}: {f}` cannot be converted to {t}; units differ")))
        }
        (Some(f), Some(t)) if f.amount < t.amount => {
            Err(("E003", format!("capability limited to `{name}: {f}` cannot be widened to {t}")))
        }
        _ => Ok(()),
    }
}

/// The rules for capabilities a library declares (RUNTIME.md section 13).
fn check_caps(ctx: &mut Ctx, prog: &Program) {
    let mut seen_paths = HashSet::new();
    for cap in &prog.caps {
        let p = cap.path.join(".");
        if matches!(cap.path.first().map(String::as_str), Some("net") | Some("fs")) {
            ctx.err("E101", cap.pos, format!("capability {p}: `net` and `fs` belong to the runtime"));
        }
        if !seen_paths.insert(p.clone()) {
            ctx.err("E101", cap.pos, format!("capability {p} is declared twice"));
        }
        for (n, t, pos) in &cap.needs {
            check_ty_known(ctx, t, *pos);
            let ok = match t {
                Ty::Cap { path, .. } => runtime_cap(path).is_some(),
                Ty::Named(x) => x == "Secret" || x == "Text",
                _ => false,
            };
            if !ok {
                ctx.err(
                    "E101",
                    *pos,
                    format!("`needs {n}`: a capability needs runtime capabilities (net.http.*, fs.*), Secrets or Text settings"),
                );
            }
        }
        let fn_of = |name: &str| prog.fns.iter().find(|f| f.name == name);
        if cap.actions.iter().any(|a| a.target.is_some()) && cap.canonical.is_none() {
            ctx.err(
                "E101",
                cap.pos,
                format!("capability {p} has a @target, so it must give `canonical(t: Text) -> Text` to put targets in one form"),
            );
        }
        if let Some(f) = cap.canonical.as_deref().and_then(fn_of) {
            let one_text = f.params.len() == 1 && f.params[0].ty == named("Text") && f.ret == Some(named("Text"));
            if !one_text {
                ctx.err("E101", f.pos, format!("capability {p}: `canonical` takes one Text and returns Text"));
            }
            if ctx.model_fns.contains(&f.name) || ctx.diverge_fns.contains(&f.name) {
                ctx.err("E101", f.pos, format!("capability {p}: `canonical` must be pure: no model calls, `loop` or recursion"));
            }
        }
        let mut names = HashSet::new();
        for a in &cap.actions {
            if !names.insert(a.name.as_str()) {
                ctx.err("E101", a.pos, format!("capability {p} declares action '{}' twice", a.name));
            }
            let Some(f) = fn_of(&a.fn_name) else { continue };
            if f.ret != Some(named("Text")) {
                ctx.err(
                    "E101",
                    a.pos,
                    format!("action {}: an action returns the service's id for its receipt, as `-> Text`", cap.action_path(&a.name)),
                );
            }
            for (i, prm) in f.params[..a.declared].iter().enumerate() {
                let bad = matches!(prm.ty, Ty::Cap { .. }) || secret_inside(ctx, &prm.ty, &mut HashSet::new()).is_some();
                if bad {
                    ctx.err(
                        "E101",
                        a.pos,
                        format!(
                            "action {}: parameter '{}' must be data; capabilities and secrets come through `needs`",
                            cap.action_path(&a.name),
                            prm.name
                        ),
                    );
                }
                if a.target == Some(i) && prm.ty != named("Text") {
                    ctx.err("E101", a.pos, format!("@target '{}' must be Text", prm.name));
                }
                if a.amount == Some(i) && prm.ty != named("Money") {
                    ctx.err("E101", a.pos, format!("@amount '{}' must be Money", prm.name));
                }
            }
            let counts = [
                ("@target", f.params.iter().filter(|q| q.target).count()),
                ("@amount", f.params.iter().filter(|q| q.amount).count()),
            ];
            for (mark, count) in counts {
                if count > 1 {
                    ctx.err("E101", a.pos, format!("action {} marks {count} parameters {mark}; at most one", cap.action_path(&a.name)));
                }
            }
            if ctx.model_fns.contains(&f.name) {
                ctx.err(
                    "E101",
                    a.pos,
                    format!("action {} makes model calls; a capability action only talks to its service", cap.action_path(&a.name)),
                );
            }
            if ctx.diverge_fns.contains(&f.name) {
                ctx.err("E101", a.pos, format!("action {} may never finish (`loop` or recursion)", cap.action_path(&a.name)));
            }
            let declared: Vec<Ty> = f.params[..a.declared].iter().map(|q| q.ty.clone()).collect();
            ctx.lib_actions.insert(cap.action_path(&a.name), declared);
        }
    }
}

fn check_fn(ctx: &mut Ctx, f: &FnDecl, prog: &Program) {
    // `fails E`: E is data a caller reads back, so a declared record or enum.
    if let Some(t) = &f.fails {
        check_ty_known(ctx, t, f.pos);
        let declared = matches!(t, Ty::Named(n) if n != "Outcome" && n != "Response" && (ctx.fields.contains_key(n) || ctx.enums.contains_key(n)));
        let unknown = matches!(t, Ty::Named(n) if !ctx.known.contains(n));
        if !declared && !unknown {
            ctx.err("E101", f.pos, format!("'{}' declares `fails {t}`; a function fails with a declared record or enum type", f.name));
        } else if let Some(bad) = secret_inside(ctx, t, &mut HashSet::new()).or_else(|| proof_or_authority(t)) {
            ctx.err("E101", f.pos, format!("'{}' cannot fail with a {bad}: what a function fails with is data", f.name));
        }
        if f.name.contains(' ') {
            ctx.err("E101", f.pos, "a capability's action cannot declare `fails`".into());
        }
    }
    escaping_failures(ctx, f);
    let action = prog.caps.iter().find_map(|c| c.actions.iter().find(|a| a.fn_name == f.name).map(|a| (c, a)));
    if action.is_none() && f.params.iter().any(|p| p.target || p.amount) {
        ctx.err("E101", f.pos, format!("'{}': @target and @amount mark parameters of capability actions only", f.name));
    }
    ctx.flow = action.map(|(c, a)| ActionFlow {
        needs: c.needs.iter().filter(|(_, t, _)| matches!(t, Ty::Cap { .. })).map(|(n, _, _)| n.clone()).collect(),
        target: a.target.map(|i| f.params[i].name.clone()),
        amount: a.amount.map(|i| f.params[i].name.clone()),
        request: false,
        target_used: false,
        amount_used: false,
    });
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
        let b = Binding { ty: p.ty.clone(), trusted: p.trusted, reveals, proven_ok: false, holds_evidence: false, decl: None, raises: None };
        env.insert(p.name.clone(), b);
    }
    let ret = f.ret.clone().unwrap_or_else(|| named("Unit"));
    check_ty_known(ctx, &ret, f.pos);
    check_block(ctx, f, &ret, &f.body, env, false);
    if ret != named("Unit") && !always_returns(&f.body) {
        ctx.err("E105", f.pos, format!("'{}' can finish without returning a value of type {ret}", f.name));
    }
    if let Some(fl) = ctx.flow.take() {
        if !fl.request {
            ctx.err(
                "E101",
                f.pos,
                format!("{} makes no request through what it needs, so it could not prove anything happened", f.name),
            );
        }
        if let Some(t) = fl.target.filter(|_| !fl.target_used) {
            ctx.err(
                "E101",
                f.pos,
                format!("in {}: @target '{t}' never reaches a request, so the program's `only` would check a value that is not used", f.name),
            );
        }
        if let Some(t) = fl.amount.filter(|_| !fl.amount_used) {
            ctx.err(
                "E101",
                f.pos,
                format!(
                    "in {}: @amount '{t}' never reaches a request, so the program's money limits would check a value that is not used",
                    f.name
                ),
            );
        }
    }
}

fn f_params_contains(ctx: &Ctx, name: &str) -> bool {
    ctx.params.contains(name)
}

/// For an Outcome: the type the call it came from declares it `fails` with, if any.
/// `try g(...)` where `g` declares `fails E`, or a variable holding such an Outcome.
fn outcome_raises(ctx: &Ctx, value: &Expr, env: &Env) -> Option<Ty> {
    match value {
        Expr::Try(inner, _) => match inner.as_ref() {
            Expr::Call { name, .. } => ctx.sigs.get(name.as_str())?.fails.clone(),
            _ => None,
        },
        Expr::Var(v, _) => env.get(v)?.raises.clone(),
        _ => None,
    }
}

/// The calls in `body` whose declared failures would escape this function: calls to a
/// function declaring `fails E` that are not directly under `try`. Each such failure
/// leaves this function as it is, so this function must declare the same type.
fn escaping_failures(ctx: &mut Ctx, f: &FnDecl) {
    fn expr(ctx: &mut Ctx, f: &FnDecl, e: &Expr) {
        if let Expr::Try(inner, _) = e
            && let Expr::Call { args, .. } = inner.as_ref()
        {
            // Caught here; its arguments are not.
            for a in args {
                expr(ctx, f, a);
            }
            return;
        }
        if let Expr::Call { name, pos, .. } = e
            && let Some(theirs) = ctx.sigs.get(name.as_str()).and_then(|s| s.fails.clone())
            && f.fails.as_ref() != Some(&theirs)
        {
            let ours = match &f.fails {
                Some(t) => format!("declares `fails {t}`"),
                None => "declares no `fails` type".to_string(),
            };
            ctx.err(
                "E101",
                *pos,
                format!("'{name}' fails with {theirs}, which would leave '{}', which {ours}; catch it with `try`, or declare `fails {theirs}`", f.name),
            );
        }
        for c in e.children() {
            expr(ctx, f, c);
        }
    }
    let mut stack: Vec<&[Stmt]> = vec![&f.body];
    while let Some(b) = stack.pop() {
        for s in b {
            for e in s.exprs() {
                expr(ctx, f, e);
            }
            stack.extend(s.blocks());
        }
    }
}

/// Whether binding `value` yields an Outcome that holds proof on success: it is
/// `try` around an action, or around a call to a function returning Receipt or Done,
/// or an alias of such an Outcome. A Receipt or Done that a callee returns has itself
/// passed E008, so it is real proof of work done by that call.
fn outcome_holds_evidence(ctx: &Ctx, value: &Expr, env: &Env) -> bool {
    match value {
        Expr::Try(inner, _) => match inner.as_ref() {
            // An action holds a receipt unless it is one of the runtime's that returns data.
            Expr::Action { receiver, .. } => match env.get(receiver).map(|b| &b.ty) {
                Some(Ty::Cap { path, .. }) => {
                    !matches!(runtime_cap(path), Some(RuntimeCap::Http { .. } | RuntimeCap::FsRead))
                }
                _ => false,
            },
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

/// Whether a `break` appears in this block, or in an `if`, `repeat` or `loop` inside it.
/// (Conservative where it matters: a `break` of a nested loop counts too.)
fn has_break(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Break(_) => true,
        Stmt::If { then, els, .. } => has_break(then) || has_break(els),
        Stmt::Match { arms, .. } => arms.iter().any(|a| has_break(&a.body)),
        Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => has_break(body),
        _ => false,
    })
}

/// Whether control can never reach the end of this block: every path returns or breaks
/// first. When unsure, says it can: assuming more paths only adds taint.
fn never_falls_through(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::Fail(..) => true,
        Stmt::If { then, els, .. } => !els.is_empty() && never_falls_through(then) && never_falls_through(els),
        // The arms cover every variant (E109), so one of them runs.
        Stmt::Match { arms, .. } => arms.iter().all(|a| never_falls_through(&a.body)),
        Stmt::Loop { body, .. } => !has_break(body),
        _ => false,
    })
}

/// What checking a block found: the taint its early exits carry (see `check_block`), the
/// variables as they stand where it ends normally (`falls`: whether it can), and the
/// variables as they stand at each `break` in it that leaves the enclosing loop.
struct BlockOut {
    taint: Vec<Src>,
    end: Env,
    falls: bool,
    breaks: Vec<Env>,
    /// The variables at each `continue` in it that goes on to the enclosing loop's next
    /// iteration.
    continues: Vec<Env>,
}

/// A loop's exit taint, its variables at the head once settled, and at every `break`.
struct LoopOut {
    taint: Vec<Src>,
    head: Env,
    breaks: Vec<Env>,
}

/// Adds `from` to `into` as a set. An exact alias of a path stays exact unless the same
/// path is also present as merely derived, which already covers everything beneath it.
fn union_srcs(into: &mut Vec<Src>, from: &[Src]) {
    for s in from {
        if s.exact {
            if !into.iter().any(|x| x.path == s.path) {
                into.push(s.clone());
            }
        } else {
            into.retain(|x| !(x.path == s.path && x.exact));
            if !into.iter().any(|x| x.path == s.path) {
                into.push(s.clone());
            }
        }
    }
}

/// The binding a `let` at `decl` made, wherever it is in `env` (it may be shadowed).
fn by_decl(env: &Env, decl: Pos) -> Option<&Binding> {
    env.values().find(|b| b.decl == Some(decl))
}

/// A `let` that reuses a name hides the earlier variable for the rest of its block, but
/// that variable still exists and an earlier `set` may have changed it: keep it under a
/// key no name can match, so joins still find it by its `let`.
fn hide_shadowed(env: &mut Env, name: &str) {
    if let Some(old) = env.remove(name)
        && let Some(d) = old.decl
    {
        env.insert(format!("\0{name}@{}:{}:{}", d.file, d.line, d.col), old);
    }
}

/// `base`'s variables where control arrives from any of `paths`: each `let` variable
/// reveals what it reveals on any of them and is trusted only if trusted on all of them.
/// With no paths (nothing arrives), `base` is kept.
fn join(base: &Env, paths: &[&Env]) -> Env {
    let mut out = base.clone();
    if paths.is_empty() {
        return out;
    }
    for b in out.values_mut() {
        let Some(d) = b.decl else { continue };
        let (mut reveals, mut trusted, mut found) = (Vec::new(), true, false);
        for p in paths {
            if let Some(pb) = by_decl(p, d) {
                union_srcs(&mut reveals, &pb.reveals);
                trusted &= pb.trusted;
                found = true;
            }
        }
        if found {
            b.reveals = reveals;
            b.trusted = trusted;
        }
    }
    out
}

/// Whether every `let` variable of `a` reveals the same and has the same trust in `b`.
fn same_vars(a: &Env, b: &Env) -> bool {
    let key = |v: &[Src]| {
        let mut k: Vec<(Vec<String>, bool)> = v.iter().map(|s| (s.path.clone(), s.exact)).collect();
        k.sort();
        k.dedup();
        k
    };
    a.values().all(|x| {
        let Some(d) = x.decl else { return true };
        by_decl(b, d).is_some_and(|y| y.trusted == x.trusted && key(&x.reveals) == key(&y.reveals))
    })
}

/// Does every path through this block end in `return` (or in a `loop` that is never left)?
fn always_returns(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        // `fail` never falls off the end either: it stops the function.
        Stmt::Return(..) | Stmt::Fail(..) => true,
        Stmt::If { then, els, .. } => always_returns(then) && always_returns(els),
        Stmt::Match { arms, .. } => arms.iter().all(|a| always_returns(&a.body)),
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

/// Whether `rel` names a field path inside `ty`. Through an enum, the next name may be
/// a field of any of its variants.
fn field_path_exists(ctx: &Ctx, ty: &Ty, rel: &[String]) -> bool {
    let Some((name, rest)) = rel.split_first() else { return true };
    // Through an optional value, to the fields of what it may hold.
    if let Ty::Optional(inner) = ty {
        return field_path_exists(ctx, inner, rel);
    }
    // In a tuple, the names are positions: `pair.0`.
    if let Ty::Tuple(items) = ty {
        return name.parse::<usize>().ok().and_then(|i| items.get(i)).is_some_and(|t| field_path_exists(ctx, t, rest));
    }
    let Ty::Named(t) = ty else { return false };
    if let Some(fields) = ctx.fields.get(t) {
        return fields.iter().any(|(n, fty)| n == name && field_path_exists(ctx, fty, rest));
    }
    ctx.enums
        .get(t)
        .is_some_and(|vs| vs.iter().flat_map(|(_, fs)| fs).any(|(n, fty)| n == name && field_path_exists(ctx, fty, rest)))
}

/// Checks a block. `env` is a copy, so names bound inside a nested block do not leak out.
///
/// `ctx.pc` holds what reaching the current point reveals: inside `if c { ... }` it
/// includes what `c` reveals, and after an `if` whose branch can `return` or `break`
/// it includes that condition for the rest of the block. Values bound here, and the
/// decision to call a model here, carry `ctx.pc`. Returns what this block's early
/// exits reveal, so enclosing blocks can taint the code that runs after them.
fn check_block(ctx: &mut Ctx, f: &FnDecl, ret: &Ty, body: &[Stmt], mut env: Env, in_loop: bool) -> BlockOut {
    let saved_pc = ctx.pc.clone();
    let mut exit_taint: Vec<Src> = Vec::new();
    let mut breaks: Vec<Env> = Vec::new();
    let mut continues: Vec<Env> = Vec::new();
    for stmt in body {
        // If this statement contains an action or a call that might not finish, whether
        // execution gets past it can depend on that operation's arguments. That holds for
        // model calls later in this statement and everything after it.
        let stmt_expr = match stmt {
            Stmt::Let { value: e, .. }
            | Stmt::Set { value: e, .. }
            | Stmt::Expr(e)
            | Stmt::Return(Some(e), _)
            | Stmt::Fail(e, _) => Some(e),
            Stmt::If { cond, .. } => Some(cond),
            Stmt::Match { value, .. } => Some(value),
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
                // `[]` and `[:]` have no element type of their own: they take the declared
                // list, set or map type.
                let empty = match (value, ty) {
                    (Expr::List(items, _), Some(Ty::List(..) | Ty::Set(..))) => items.is_empty(),
                    (Expr::MapLit(entries, _), Some(Ty::Map(..))) => entries.is_empty(),
                    _ => false,
                };
                if let Some(decl) = ty
                    && empty
                {
                    check_ty_known(ctx, decl, *pos);
                    let mut reveals = Vec::new();
                    add_srcs(&mut reveals, ctx.pc.clone());
                    let b = Binding { ty: decl.clone(), trusted: true, reveals, proven_ok: false, holds_evidence: false, decl: Some(*pos), raises: None };
                    hide_shadowed(&mut env, name);
                    env.insert(name.clone(), b);
                    continue;
                }
                // `let (a, b) = x`: the parser holds x in a variable named `(tuple N)`
                // and reads its elements. When x is not a tuple that was reported once,
                // at the holder; its elements are not reported again.
                if let Expr::Field { base, .. } = value
                    && let Expr::Var(h, _) = base.as_ref()
                    && h.starts_with("(tuple ")
                    && !env.contains_key(h)
                {
                    continue;
                }
                let Some((vty, trusted)) = type_of(ctx, value, &env) else { continue };
                if name.starts_with("(tuple ") && !matches!(vty, Ty::Tuple(_)) {
                    ctx.err("E101", value.pos(), format!("`let (...)` unpacks a tuple, found {vty}"));
                    continue;
                }
                let bound = match ty {
                    Some(decl) => {
                        check_ty_known(ctx, decl, *pos);
                        if let Err((code, msg)) = assignable(&vty, decl) {
                            let shown = if name.starts_with("(tuple ") { "(...)" } else { name.as_str() };
                            ctx.err(code, *pos, format!("in `let {shown}`: {msg}"));
                        }
                        decl.clone()
                    }
                    None if vty.has_unknown() => {
                        ctx.err(
                            "E101",
                            *pos,
                            format!("`let {name}` needs a type: `none` alone does not say what it may hold, e.g. let {name}: Optional<Text> = none"),
                        );
                        continue;
                    }
                    None => vty,
                };
                let mut r = reveals(value, &env);
                add_srcs(&mut r, ctx.pc.clone());
                // `let y = x` keeps what is known about x's outcome.
                let proven_ok = matches!(value, Expr::Var(v, _) if env.get(v).is_some_and(|b| b.proven_ok));
                let holds_evidence = outcome_holds_evidence(ctx, value, &env);
                hide_shadowed(&mut env, name);
                let raises = outcome_raises(ctx, value, &env);
                env.insert(name.clone(), Binding { ty: bound, trusted, reveals: r, proven_ok, holds_evidence, decl: Some(*pos), raises });
            }
            Stmt::Set { name, value, pos } => {
                let Some(b) = env.get(name).cloned() else {
                    type_of(ctx, value, &env);
                    ctx.err("E100", *pos, format!("cannot `set {name}`: no variable named '{name}' is declared here"));
                    continue;
                };
                let Some(decl) = b.decl else {
                    ctx.err(
                        "E101",
                        *pos,
                        format!("cannot `set {name}`: parameters and loop variables cannot be reassigned; copy it into a `let` first"),
                    );
                    continue;
                };
                if let Some(bad) = proof_or_authority(&b.ty) {
                    ctx.err(
                        "E101",
                        *pos,
                        format!("cannot `set {name}`: it holds a {bad}, which is never reassigned, so what it proves or allows cannot change under it"),
                    );
                    continue;
                }
                let Some((vty, trusted)) = type_of(ctx, value, &env) else { continue };
                if let Err((code, msg)) = assignable(&vty, &b.ty) {
                    ctx.err(code, *pos, format!("in `set {name}`: {msg}"));
                    continue;
                }
                // From here on the variable holds the new value, and reveals that this
                // `set` was reached. What it held before no longer matters on this path.
                let mut r = reveals(value, &env);
                add_srcs(&mut r, ctx.pc.clone());
                if let Some(cur) = env.get_mut(name)
                    && cur.decl == Some(decl)
                {
                    cur.reveals = r;
                    cur.trusted = trusted;
                }
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
                let lo = check_loop(ctx, f, ret, body, env.clone(), &[]);
                // After the loop: from its head (it may end after any iteration) or a `break`.
                let mut paths = vec![&lo.head];
                paths.extend(lo.breaks.iter());
                env = join(&env, &paths);
                let taint = lo.taint;
                ctx.pc = saved_pc.clone();
                add_srcs(&mut ctx.pc, exit_taint.clone());
                add_srcs(&mut exit_taint, taint.clone());
                add_srcs(&mut ctx.pc, taint);
            }
            Stmt::For { var, list, body, pos } => {
                let Some((lty, ltrusted)) = type_of(ctx, list, &env) else { continue };
                let Ty::List(el, max) = &lty else {
                    let code = if matches!(lty, Ty::Proposed(_)) { "E001" } else { "E101" };
                    let hint = match lty {
                        Ty::Map(..) => "; use keys(m), values(m) or entries(m)",
                        Ty::Set(..) => "; use items(s)",
                        _ => "",
                    };
                    ctx.err(code, list.pos(), format!("`for` needs a list, found {lty}{hint}"));
                    continue;
                };
                ctx.for_bounds.insert(*pos, *max);
                // `for (a, b) in xs`: the parser names each item `(tuple N)` and unpacks it.
                let unpacks = var.starts_with("(tuple ");
                if unpacks && !matches!(&**el, Ty::Tuple(_)) {
                    ctx.err("E101", list.pos(), format!("`for (...)` unpacks tuples, but the items of {lty} are {el}"));
                    continue;
                }
                // Each element is part of the list: it reveals what the list does, exactly.
                let list_srcs = reveals(list, &env);
                let mut body_env = env.clone();
                let b = Binding {
                    ty: (**el).clone(),
                    trusted: ltrusted,
                    reveals: list_srcs.clone(),
                    proven_ok: false,
                    holds_evidence: false,
                    decl: None,
                    raises: None,
                };
                body_env.insert(var.clone(), b);
                // How many times the body runs reveals the list's length, so everything the
                // body does depends on the list, like the body of an `if` on its condition.
                let mut count = Vec::new();
                add_srcs(&mut count, list_srcs);
                let lo = check_loop(ctx, f, ret, body, body_env, &count);
                let mut paths = vec![&lo.head];
                paths.extend(lo.breaks.iter());
                env = join(&env, &paths);
                let mut taint = lo.taint;
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
                let o1 = check_block(ctx, f, ret, then, then_env, in_loop);
                let o2 = check_block(ctx, f, ret, els, else_env, in_loop);
                // After the `if`: from whichever branches can reach their end.
                let mut reach = Vec::new();
                if o1.falls {
                    reach.push(&o1.end);
                }
                if o2.falls {
                    reach.push(&o2.end);
                }
                env = join(&env, &reach);
                let (t1, t2) = (o1.taint, o2.taint);
                breaks.extend(o1.breaks);
                breaks.extend(o2.breaks);
                continues.extend(o1.continues);
                continues.extend(o2.continues);
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
            Stmt::Fail(msg, pos) => {
                if let Some((t, _)) = type_of(ctx, msg, &env) {
                    // An action's `fails` is refused where declared; its `fail` stays Text.
                    match f.fails.as_ref().filter(|_| !f.name.contains(' ')) {
                        Some(e) => {
                            if let Err((code, m)) = assignable(&t, e) {
                                ctx.err(code, *pos, format!("'{}' declares `fails {e}`, so `fail` needs a value of type {e}: {m}", f.name));
                            }
                        }
                        None if t != named("Text") => {
                            let hint = match &t {
                                Ty::Named(n) if ctx.fields.contains_key(n) || ctx.enums.contains_key(n) => {
                                    format!("; to fail with a value of type {t}, declare `fails {t}`")
                                }
                                _ => String::new(),
                            };
                            ctx.err("E101", *pos, format!("`fail` needs a Text message, found {t}{hint}"));
                        }
                        None => {}
                    }
                }
            }
            Stmt::Break(pos) => {
                breaks.push(env.clone());
                if !in_loop {
                    ctx.err("E103", *pos, "`break` is only allowed inside `repeat`, `loop` or `for`".into());
                }
            }
            Stmt::Match { value, arms, pos } => {
                let Some((vty, vtrusted)) = type_of(ctx, value, &env) else { continue };
                // An enum's variants, or what an optional value may hold.
                let variants = match &vty {
                    Ty::Named(n) => ctx.enums.get(n).cloned(),
                    _ => None,
                };
                let held = match &vty {
                    Ty::Optional(inner) => Some((**inner).clone()),
                    _ => None,
                };
                if let Some(v) = &variants {
                    match_patterns(ctx, &vty.to_string(), v, arms, *pos);
                } else if held.is_some() {
                    if vty.has_unknown() {
                        ctx.err("E101", value.pos(), "there is nothing to match: this value is always `none`".into());
                        continue;
                    }
                    optional_patterns(ctx, &vty, arms, *pos);
                } else {
                    if matches!(vty, Ty::Proposed(_)) {
                        ctx.err("E001", value.pos(), format!("cannot match unvalidated {vty}; use `validate` first"));
                    } else {
                        ctx.err("E101", value.pos(), format!("`match` needs an enum or optional value, found {vty}"));
                    }
                    continue;
                }
                // Which arm runs depends on the value's variant, or on whether it holds
                // anything, like an `if` on its condition: for a path, that is the
                // path's `kind`, or `?` (no field is called either), so a blinded field
                // inside does not decide the arm. A bound field is exactly that field of
                // the value; what `some(x)` binds is exactly the value's content.
                let srcs = reveals(value, &env);
                let marker = if variants.is_some() { "kind" } else { "?" };
                let decides: Vec<Src> = srcs
                    .iter()
                    .map(|s| {
                        let mut s = s.clone();
                        if s.exact {
                            s.path.push(marker.into());
                        }
                        s
                    })
                    .collect();
                let here = ctx.pc.clone();
                add_srcs(&mut ctx.pc, decides.clone());
                let mut outs = Vec::new();
                for arm in arms.iter() {
                    let mut arm_env = env.clone();
                    // Each name the arm binds: its type, and the field of the value it is.
                    let mut bound: Vec<(&String, Ty, Option<&String>)> = Vec::new();
                    match (&arm.pattern, &variants, &held) {
                        (Pattern::Variant { name, binds, .. }, Some(vs), _) => {
                            if let Some((_, fields)) = vs.iter().find(|(v, _)| v == name) {
                                for (field, to) in binds {
                                    if let Some((_, fty)) = fields.iter().find(|(f, _)| f == field) {
                                        bound.push((to, fty.clone(), Some(field)));
                                    }
                                }
                            }
                        }
                        (Pattern::Present { bind, .. }, _, Some(inner)) => bound.push((bind, inner.clone(), None)),
                        _ => {}
                    }
                    for (to, fty, field) in bound {
                        let mut r: Vec<Src> = srcs
                            .iter()
                            .map(|s| {
                                let mut s = s.clone();
                                if s.exact
                                    && let Some(field) = field
                                {
                                    s.path.push(field.clone());
                                }
                                s
                            })
                            .collect();
                        add_srcs(&mut r, ctx.pc.clone());
                        hide_shadowed(&mut arm_env, to);
                        let b = Binding { ty: fty, trusted: vtrusted, reveals: r, proven_ok: false, holds_evidence: false, decl: None, raises: None };
                        arm_env.insert(to.clone(), b);
                    }
                    outs.push(check_block(ctx, f, ret, &arm.body, arm_env, in_loop));
                }
                let reach: Vec<&Env> = outs.iter().filter(|o| o.falls).map(|o| &o.end).collect();
                env = join(&env, &reach);
                ctx.pc = here;
                // As for `if`: an arm that always returns cannot influence what follows;
                // if some arm can stop early, whether code after runs depends on the value.
                let mut t = Vec::new();
                let mut all_return = true;
                let mut any_stops = false;
                for (arm, o) in arms.iter().zip(outs) {
                    breaks.extend(o.breaks);
                    continues.extend(o.continues);
                    if always_returns(&arm.body) {
                        any_stops = true;
                    } else {
                        all_return = false;
                        add_srcs(&mut t, o.taint);
                        any_stops |= may_stop(ctx, &arm.body);
                    }
                }
                if !all_return && any_stops {
                    add_srcs(&mut t, decides);
                }
                if !t.is_empty() {
                    add_srcs(&mut exit_taint, t.clone());
                    add_srcs(&mut ctx.pc, t);
                }
            }
            Stmt::Continue(pos) => {
                continues.push(env.clone());
                if !in_loop {
                    ctx.err("E103", *pos, "`continue` is only allowed inside `repeat`, `loop` or `for`".into());
                }
            }
        }
    }
    ctx.pc = saved_pc;
    BlockOut { taint: exit_taint, falls: !never_falls_through(body), end: env, breaks, continues }
}

/// The patterns of a `match` on enum `ename`: each names a variant once, binds only
/// its fields (each once, to distinct names), binds them all unless it ends in `..`,
/// and together they cover every variant (E109). `_` accepts what is left, so it must
/// come last and must have something left to accept.
fn match_patterns(ctx: &mut Ctx, ename: &str, variants: &Variants, arms: &[Arm], pos: Pos) {
    let mut covered: HashSet<&str> = HashSet::new();
    let mut any = false;
    for arm in arms {
        match &arm.pattern {
            Pattern::Any(p) => {
                if any || covered.len() == variants.len() {
                    ctx.err("E101", *p, "this `_` arm can never run: every variant is already matched".into());
                }
                any = true;
            }
            Pattern::Present { pos: p, .. } | Pattern::Absent(p) => {
                ctx.err("E101", *p, format!("`some(...)` and `none` match optional values; {ename} is an enum, so name its variants"));
            }
            Pattern::Variant { name, binds, rest, pos: p } => {
                if any {
                    ctx.err("E101", *p, format!("the arm for {name} can never run: `_` above already matches it"));
                }
                let Some((vname, fields)) = variants.iter().find(|(v, _)| v == name) else {
                    let all: Vec<&str> = variants.iter().map(|(v, _)| v.as_str()).collect();
                    ctx.err("E104", *p, format!("enum '{ename}' has no variant '{name}'; it has {}", all.join(", ")));
                    continue;
                };
                if !covered.insert(vname.as_str()) {
                    ctx.err("E101", *p, format!("variant {name} is matched twice"));
                }
                let (mut fseen, mut tseen) = (HashSet::new(), HashSet::new());
                for (field, to) in binds {
                    if !fields.iter().any(|(f, _)| f == field) {
                        ctx.err("E104", *p, format!("variant '{ename}.{name}' has no field '{field}'"));
                    }
                    if !fseen.insert(field.as_str()) {
                        ctx.err("E101", *p, format!("field '{field}' is bound twice"));
                    }
                    if !tseen.insert(to.as_str()) {
                        ctx.err("E101", *p, format!("'{to}' is bound twice in this pattern"));
                    }
                }
                let missing: Vec<&str> = fields.iter().map(|(f, _)| f.as_str()).filter(|f| !fseen.contains(f)).collect();
                if !rest && !missing.is_empty() {
                    ctx.err(
                        "E101",
                        *p,
                        format!("the pattern for {name} does not bind {}; bind every field, or end with `..`", missing.join(", ")),
                    );
                }
            }
        }
    }
    if !any {
        let left: Vec<&str> = variants.iter().map(|(v, _)| v.as_str()).filter(|v| !covered.contains(v)).collect();
        if !left.is_empty() {
            ctx.err("E109", pos, format!("this `match` does not handle {}; add an arm for each, or `_ =>`", left.join(", ")));
        }
    }
}

/// The patterns of a `match` on optional type `ty`: `some(x)` and `none`, each once,
/// or `_` for what is left, last and with something left to accept (E109 without).
fn optional_patterns(ctx: &mut Ctx, ty: &Ty, arms: &[Arm], pos: Pos) {
    let (mut some, mut none, mut any) = (false, false, false);
    for arm in arms {
        let (seen, what, p) = match &arm.pattern {
            Pattern::Any(p) => {
                if any || (some && none) {
                    ctx.err("E101", *p, "this `_` arm can never run: `some` and `none` are already matched".into());
                }
                any = true;
                continue;
            }
            Pattern::Variant { name, pos: p, .. } => {
                ctx.err("E101", *p, format!("a `match` on {ty} takes `some(x)`, `none` or `_`, not {name}"));
                continue;
            }
            Pattern::Present { pos, .. } => (&mut some, "some", *pos),
            Pattern::Absent(p) => (&mut none, "none", *p),
        };
        if any {
            ctx.err("E101", p, format!("the `{what}` arm can never run: `_` above already matches it"));
        } else if *seen {
            ctx.err("E101", p, format!("`{what}` is matched twice"));
        }
        *seen = true;
    }
    if !any && !(some && none) {
        let left = if some { "none" } else if none { "some(...)" } else { "some(...) or none" };
        ctx.err("E109", pos, format!("this `match` does not handle {left}; add an arm for it, or `_ =>`"));
    }
}

/// Checks a loop body (`repeat`, `loop`, `for`) under `extra` in addition to the
/// current pc. Two things are settled first, with the diagnostics of the trial passes
/// discarded: a `break` taken in one iteration decides whether later iterations run, so
/// the body is checked again under its own exit taint; and a variable set in one
/// iteration is read by the next, so the variables at the loop head are joined with
/// those at the end of the body. Both only grow and there are finitely many sources,
/// so this settles; the bound is a safeguard that fails closed. Leaves `ctx.pc` for
/// the caller to set.
fn check_loop(ctx: &mut Ctx, f: &FnDecl, ret: &Ty, body: &[Stmt], env: Env, extra: &[Src]) -> LoopOut {
    let mut base = ctx.pc.clone();
    add_srcs(&mut base, extra.to_vec());
    let mut taint: Vec<Src> = Vec::new();
    let mut head = env;
    let mut settled = false;
    for _ in 0..64 {
        let mark = ctx.diags.len();
        ctx.pc = base.clone();
        add_srcs(&mut ctx.pc, taint.clone());
        let out = check_block(ctx, f, ret, body, head.clone(), true);
        ctx.diags.truncate(mark);
        let before = taint.len();
        add_srcs(&mut taint, out.taint);
        // The next iteration starts from the end of this one, or from a `continue`.
        let mut paths = vec![&head];
        if out.falls {
            paths.push(&out.end);
        }
        paths.extend(out.continues.iter());
        let next = join(&head, &paths);
        if taint.len() == before && same_vars(&head, &next) {
            settled = true;
            break;
        }
        head = next;
    }
    if !settled {
        ctx.err("E101", f.pos, format!("a loop in '{}' is too complex to analyse; simplify it", f.name));
    }
    ctx.pc = base;
    add_srcs(&mut ctx.pc, taint.clone());
    let out = check_block(ctx, f, ret, body, head.clone(), true);
    add_srcs(&mut taint, out.taint);
    LoopOut { taint, head, breaks: out.breaks }
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
        Ty::Proposed(inner) | Ty::List(inner, _) | Ty::Optional(inner) => runtime_only_inside(ctx, inner, seen),
        Ty::Tuple(items) => items.iter().find_map(|t| runtime_only_inside(ctx, t, seen)),
        Ty::Map(k, v, _) => runtime_only_inside(ctx, k, seen).or_else(|| runtime_only_inside(ctx, v, seen)),
        Ty::Set(t, _) => runtime_only_inside(ctx, t, seen),
        Ty::Named(n) if n == "Receipt" || n == "Outcome" || n == "Done" => Some(n.clone()),
        Ty::Named(n) => {
            if !seen.insert(n.clone()) {
                return None;
            }
            inner_types(ctx, n)?.iter().find_map(|fty| runtime_only_inside(ctx, fty, seen))
        }
    }
}

/// Checks the fields given to a record or variant literal named `ty` against `decl`:
/// each given once, each declared, each assignable, none missing. Returns whether every
/// value is trusted, or None after reporting a problem.
fn record_fields(ctx: &mut Ctx, env: &Env, ty: &str, decl: &[(String, Ty)], fields: &[(String, Expr)], pos: Pos) -> Option<bool> {
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
    for (fname, _) in decl {
        if !fields.iter().any(|(n, _)| n == fname) {
            ctx.err("E101", pos, format!("this {ty} is missing field '{fname}'"));
            ok = false;
        }
    }
    ok.then_some(trusted)
}

/// An expression's type and whether its value is trusted.
type Typed = (Ty, bool);

/// `base.method(args)`: a call of the method of `base`'s type, `T::method(base, args)`.
/// It is recorded, so the call can be rewritten into exactly that (see
/// `check_and_resolve`), and typed as that call.
/// `on_variable`: written `x.method(...)`, which is an action when `x` is a capability;
/// on anything else with no such method it is still an attempt to act (E002).
fn method_call(ctx: &mut Ctx, base: &Expr, method: &str, args: &[Expr], pos: Pos, env: &Env, on_variable: bool) -> Option<Typed> {
    let (bt, _) = type_of(ctx, base, env)?;
    let found = match &bt {
        Ty::Named(n) => {
            let name = format!("{n}::{method}");
            ctx.sigs.get(name.as_str()).map(|s| (name, s.public, s.file))
        }
        _ => None,
    };
    let Some((name, public, file)) = found else {
        let what = match &bt {
            Ty::Proposed(_) => {
                ctx.err("E001", pos, format!("cannot call '.{method}()' on unvalidated {bt}; use `validate` first"));
                return None;
            }
            Ty::Named(_) => format!("type {bt} has no method '{method}'"),
            _ => format!("{bt} has no methods; cannot call '.{method}()'"),
        };
        if on_variable {
            let r = expr_text(base);
            ctx.err("E002", pos, format!("'{r}.{method}' acts on the world but '{r}' is not a capability in scope, and {what}"));
        } else {
            ctx.err("E104", pos, what);
        }
        return None;
    };
    if !public && file != pos.file {
        ctx.method_diags.push(Diagnostic { code: "E107", pos, msg: format!("method '{name}' is not `pub` in its module") });
    }
    ctx.methods.insert(pos, name.clone());
    let mut all = vec![base.clone()];
    all.extend(args.iter().cloned());
    type_of(ctx, &Expr::Call { name, args: all, slots: Vec::new(), pos }, env)
}

/// Returns the expression's type and whether its value is trusted.
fn type_of(ctx: &mut Ctx, e: &Expr, env: &Env) -> Option<(Ty, bool)> {
    match e {
        // `crate::calls` replaces every one; kept total.
        Expr::Named { value, .. } => type_of(ctx, value, env),
        Expr::Str(..) => Some((named("Text"), true)),
        Expr::Money(..) => Some((named("Money"), true)),
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
            let arity = f.arity();
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
            match f {
                Builtin::Header => {
                    if tys[0].0 != text || (tys[1].0 != text && tys[1].0 != named("Secret")) {
                        ctx.err(
                            "E101",
                            *pos,
                            format!("`header` needs (name: Text, value: Text or Secret), found ({}, {})", tys[0].0, tys[1].0),
                        );
                        return None;
                    }
                    return Some((named("Header"), trusted));
                }
                Builtin::Replace | Builtin::Lower | Builtin::Trim | Builtin::UrlSegment => {
                    if tys.iter().any(|(t, _)| *t != text) {
                        let found: Vec<String> = tys.iter().map(|(t, _)| t.to_string()).collect();
                        ctx.err("E101", *pos, format!("`{}` needs Text, found ({})", f.name(), found.join(", ")));
                        return None;
                    }
                    return Some((text, trusted));
                }
                Builtin::Text => {
                    let t = &tys[0].0;
                    if ![text.clone(), named("Int"), named("Bool"), named("Money")].contains(t) {
                        let hint = if *t == named("Secret") || *t == named("Header") {
                            ": secrets never become text"
                        } else {
                            "; use `json(x)` for records and lists"
                        };
                        ctx.err("E101", *pos, format!("interpolation and `to_text` need Text, Int, Bool or Money, found {t}{hint}"));
                        return None;
                    }
                    return Some((text, trusted));
                }
                Builtin::MinorUnits | Builtin::Currency => {
                    if tys[0].0 != named("Money") {
                        ctx.err("E101", *pos, format!("`{}` needs Money, found {}", f.name(), tys[0].0));
                        return None;
                    }
                    let result = if *f == Builtin::MinorUnits { named("Int") } else { text };
                    return Some((result, trusted));
                }
                Builtin::Form => {
                    let fields = match &tys[0].0 {
                        Ty::Named(n) if n != "Outcome" && n != "Response" => ctx.fields.get(n).cloned(),
                        _ => None,
                    };
                    let simple = |t: &Ty| *t == text || *t == named("Int") || *t == named("Bool");
                    match fields {
                        Some(fs) if fs.iter().all(|(_, t)| simple(t)) => return Some((text, trusted)),
                        _ => {
                            ctx.err("E101", *pos, format!("`form` needs a record of Text, Int and Bool fields, found {}", tys[0].0));
                            return None;
                        }
                    }
                }
                Builtin::FillSecret => {
                    if tys[0].0 != text || tys[1].0 != named("Secret") {
                        ctx.err(
                            "E101",
                            *pos,
                            format!("`fill_secret` needs (template: Text, key: Secret), found ({}, {})", tys[0].0, tys[1].0),
                        );
                        return None;
                    }
                    return Some((named("Secret"), true));
                }
                Builtin::Json => {
                    if let Some(bad) = secret_inside(ctx, &tys[0].0, &mut HashSet::new()) {
                        ctx.err("E101", *pos, format!("`json` cannot encode a {bad}: secrets never become text"));
                        return None;
                    }
                    if cap_inside(ctx, &tys[0].0, &mut HashSet::new()) {
                        ctx.err("E101", *pos, "`json` cannot encode a capability".into());
                        return None;
                    }
                    return Some((text, trusted));
                }
                Builtin::Get
                | Builtin::Put
                | Builtin::Remove
                | Builtin::Keys
                | Builtin::Values
                | Builtin::Entries
                | Builtin::Add
                | Builtin::Items
                | Builtin::ToSet => return collection_builtin(ctx, *f, &tys, trusted, *pos),
                _ => {}
            }
            if *f == Builtin::Contains && secret_inside(ctx, &tys[0].0, &mut HashSet::new()).is_some() {
                ctx.err("E101", *pos, "`contains` cannot search secrets or headers: comparing them would reveal them".into());
                return None;
            }
            let ok = match f {
                Builtin::Len => matches!(&tys[0].0, Ty::List(..) | Ty::Map(..) | Ty::Set(..)) || tys[0].0 == text,
                Builtin::Contains => match (&tys[0].0, &tys[1].0) {
                    (a, b) if *a == text && *b == text => true,
                    (Ty::List(el, _), b) => b.fits(el),
                    // A map: whether it has the key. A set: whether it has the item.
                    (Ty::Map(k, _, _), b) | (Ty::Set(k, _), b) => b == &**k,
                    _ => false,
                },
                Builtin::StartsWith | Builtin::EndsWith => tys[0].0 == text && tys[1].0 == text,
                Builtin::Header
                | Builtin::Json
                | Builtin::FillSecret
                | Builtin::Replace
                | Builtin::Lower
                | Builtin::Trim
                | Builtin::Form
                | Builtin::MinorUnits
                | Builtin::Currency
                | Builtin::UrlSegment
                | Builtin::Text
                | Builtin::Get
                | Builtin::Put
                | Builtin::Remove
                | Builtin::Keys
                | Builtin::Values
                | Builtin::Entries
                | Builtin::Add
                | Builtin::Items
                | Builtin::ToSet => true,
            };
            if !ok {
                let found: Vec<String> = tys.iter().map(|(t, _)| t.to_string()).collect();
                let want = match f {
                    Builtin::Len => "Text, a list, a map or a set",
                    Builtin::Contains => "(Text, Text), (a list, a value of its element type), (a map, a key) or (a set, an item)",
                    Builtin::StartsWith | Builtin::EndsWith => "(Text, Text)",
                    _ => "",
                };
                ctx.err("E101", *pos, format!("`{}` needs {want}, found ({})", f.name(), found.join(", ")));
                return None;
            }
            let result = if *f == Builtin::Len { named("Int") } else { named("Bool") };
            Some((result, trusted))
        }
        Expr::Parse { ty, text, pos } => {
            check_ty_known(ctx, ty, *pos);
            if let Some(bad) = runtime_only_inside(ctx, ty, &mut HashSet::new()) {
                ctx.err("E101", *pos, format!("text from outside cannot be parsed into a {bad}: only the runtime creates one"));
            }
            if let Some(bad) = secret_inside(ctx, ty, &mut HashSet::new()) {
                ctx.err("E101", *pos, format!("text cannot be parsed into a {bad}"));
            }
            let (tt, _) = type_of(ctx, text, env)?;
            if tt != named("Text") {
                let code = if matches!(tt, Ty::Proposed(_)) { "E001" } else { "E101" };
                ctx.err(code, text.pos(), format!("`parse` needs Text, found {tt}"));
                return None;
            }
            // Like model output: unchecked until validated, and never trusted.
            Some((Ty::Proposed(Box::new(ty.clone())), false))
        }
        Expr::Record { ty, fields, pos } => {
            // Outcome is a built-in record made only by `try`; other built-ins have no fields.
            let decl = match ctx.fields.get(ty) {
                Some(d) if ty != "Outcome" && ty != "Response" => d.clone(),
                _ => {
                    if ctx.enums.contains_key(ty) {
                        ctx.err("E101", *pos, format!("{ty} is an enum; build one of its variants, as {ty}.Variant {{ ... }}"));
                    } else if ctx.known.contains(ty) {
                        ctx.err("E101", *pos, format!("a {ty} cannot be built in code; only the runtime makes one"));
                    } else {
                        ctx.err("E102", *pos, format!("unknown type '{ty}'"));
                    }
                    return None;
                }
            };
            let trusted = record_fields(ctx, env, ty, &decl, fields, *pos)?;
            Some((named(ty), trusted))
        }
        Expr::NoneLit(_) => Some((Ty::none(), true)),
        Expr::MapLit(entries, pos) => {
            if entries.is_empty() {
                ctx.err("E101", *pos, "an empty map has no key or value type; give it one in a typed let: let m: Map<Text, Int, 10> = [:]".into());
                return None;
            }
            let mut trusted = true;
            let (mut kt, mut vt): (Option<Ty>, Option<Ty>) = (None, None);
            for (k, v) in entries {
                let (Some((k_ty, ktr)), Some((v_ty, vtr))) = (type_of(ctx, k, env), type_of(ctx, v, env)) else { return None };
                trusted &= ktr && vtr;
                match &kt {
                    None => kt = Some(k_ty),
                    Some(t) if *t == k_ty => {}
                    Some(t) => {
                        ctx.err("E101", k.pos(), format!("map keys must all have one type: {t} and {k_ty}"));
                        return None;
                    }
                }
                vt = match vt {
                    None => Some(v_ty),
                    Some(t) => match t.unify(&v_ty) {
                        Some(u) => Some(u),
                        None => {
                            ctx.err("E101", v.pos(), format!("map values must all have one type: {t} and {v_ty}"));
                            return None;
                        }
                    },
                };
            }
            let (kt, vt) = (kt.expect("an entry"), vt.expect("an entry"));
            // The same literal key twice is a mistake the compiler can see.
            for (i, (k, _)) in entries.iter().enumerate() {
                if entries[..i].iter().any(|(e, _)| literal_eq(e, k)) {
                    ctx.err("E101", k.pos(), format!("key {} appears twice in this map", expr_text_literal(k)));
                    return None;
                }
            }
            let t = Ty::Map(Box::new(kt), Box::new(vt), entries.len() as u64);
            check_ty_known(ctx, &t, *pos);
            Some((t, trusted))
        }
        Expr::Tuple(items, pos) => {
            let tys: Vec<Option<(Ty, bool)>> = items.iter().map(|i| type_of(ctx, i, env)).collect();
            let tys: Vec<(Ty, bool)> = tys.into_iter().collect::<Option<Vec<_>>>()?;
            for (t, _) in &tys {
                if let Some(bad) = proof_or_authority(t) {
                    ctx.err("E101", *pos, format!("a tuple cannot hold a {bad}"));
                    return None;
                }
                if let Some(bad) = secret_inside(ctx, t, &mut HashSet::new()) {
                    ctx.err("E101", *pos, format!("a tuple cannot hold a {bad}: secrets never travel inside data"));
                    return None;
                }
            }
            let trusted = tys.iter().all(|(_, tr)| *tr);
            Some((Ty::Tuple(tys.into_iter().map(|(t, _)| t).collect()), trusted))
        }
        Expr::SomeOf(inner, pos) => {
            let (t, trusted) = type_of(ctx, inner, env)?;
            let bad = match &t {
                Ty::Optional(_) => Some(format!("{t} is already optional; some(...) of it could not be told apart from `none`")),
                Ty::Proposed(_) => Some(format!("cannot wrap unvalidated {t}; use `validate` first")),
                _ => proof_or_authority(&t)
                    .map(|b| format!("an optional value cannot hold a {b}"))
                    .or_else(|| secret_inside(ctx, &t, &mut HashSet::new()).map(|b| format!("an optional value cannot hold a {b}: secrets never travel inside data"))),
            };
            if let Some(msg) = bad {
                let code = if matches!(t, Ty::Proposed(_)) { "E001" } else { "E101" };
                ctx.err(code, *pos, msg);
                return None;
            }
            Some((Ty::Optional(Box::new(t)), trusted))
        }
        Expr::Variant { ty, variant, fields, pos } => {
            let Some(variants) = ctx.enums.get(ty) else {
                ctx.err("E102", *pos, format!("unknown type '{ty}'"));
                return None;
            };
            let Some((_, decl)) = variants.iter().find(|(v, _)| v == variant).cloned() else {
                let all: Vec<String> = variants.iter().map(|(v, _)| v.clone()).collect();
                ctx.err("E104", *pos, format!("enum '{ty}' has no variant '{variant}'; it has {}", all.join(", ")));
                return None;
            };
            let trusted = record_fields(ctx, env, &format!("{ty}.{variant}"), &decl, fields, *pos)?;
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
            let mut first = tys[0].0.clone();
            if let Some(bad) = proof_or_authority(&first) {
                ctx.err("E101", *pos, format!("a list cannot hold a {bad}"));
                return None;
            }
            // `[some(1), none]` is a List<Optional<Int>, 2>.
            for (item, (t, _)) in items.iter().zip(&tys).skip(1) {
                match first.unify(t) {
                    Some(u) => first = u,
                    None => {
                        ctx.err("E101", item.pos(), format!("list elements must all have one type: {first} and {t}"));
                        return None;
                    }
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
                // What the call `try` caught raised, when the callee declares `fails E`.
                Ty::Named(tname) if tname == "Outcome" && field == "failure" => match outcome_raises(ctx, base, env) {
                    Some(e) => Some((Ty::Optional(Box::new(e)), trusted)),
                    None => {
                        ctx.err(
                            "E104",
                            *pos,
                            "'.failure' is only on the Outcome of `try` around a call to a function that declares `fails`".into(),
                        );
                        None
                    }
                },
                Ty::Named(tname) if ctx.enums.contains_key(tname) => {
                    ctx.err(
                        "E104",
                        *pos,
                        format!("cannot read '.{field}' of enum {tname}: which fields exist depends on the variant; use `match`"),
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
                Ty::Optional(_) => {
                    ctx.err(
                        "E104",
                        *pos,
                        format!("cannot read '.{field}' of {bty}: it may hold nothing; use `match` with some(x) and none"),
                    );
                    None
                }
                Ty::Map(..) | Ty::Set(..) => {
                    ctx.err("E104", *pos, format!("{bty} has no fields; cannot read '.{field}' (use get(m, key), contains, keys or items)"));
                    None
                }
                Ty::Tuple(items) => match field.parse::<usize>().ok().and_then(|i| items.get(i)) {
                    Some(t) => Some((t.clone(), trusted)),
                    None => {
                        let last = items.len() - 1;
                        ctx.err("E104", *pos, format!("{bty} has no element .{field}; its elements are .0 to .{last}"));
                        None
                    }
                },
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
            } else if secret_inside(ctx, &lt, &mut HashSet::new()).is_some()
                || secret_inside(ctx, &rt, &mut HashSet::new()).is_some()
            {
                ctx.err("E101", *pos, "secrets and headers cannot be compared: a comparison would reveal them".into());
            } else if lt.unify(&rt).is_none() {
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
            if let Some(what) = no_schema_inside(ctx, ty, &mut HashSet::new()) {
                ctx.err(
                    "E101",
                    *pos,
                    format!("a model cannot be asked for {ty}: it holds {what} a model's output schema cannot fix; use records and lists"),
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
            if let Some(bad) = secret_inside(ctx, ty, &mut HashSet::new()) {
                ctx.err("E101", *pos, format!("a model cannot produce a {bad}"));
            }
            for d in data {
                match type_of(ctx, d, env) {
                    Some((Ty::Cap { .. }, _)) => {
                        ctx.err("E101", d.pos(), "a capability cannot be sent to a model as data".into())
                    }
                    Some((t, _)) => {
                        if let Some(bad) = secret_inside(ctx, &t, &mut HashSet::new()) {
                            ctx.err("E101", d.pos(), format!("a {bad} cannot be sent to a model"));
                        }
                    }
                    None => {}
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
        Expr::Call { name, args, slots, pos } => {
            // Typed in the order written; paired with parameters through `slots`.
            let written: Vec<Option<Typed>> = args.iter().map(|a| type_of(ctx, a, env)).collect();
            let (args, arg_tys): (Vec<Expr>, Vec<Option<Typed>>) = if slots.is_empty() {
                (args.clone(), written)
            } else {
                let mut pairs: Vec<(usize, Expr, Option<Typed>)> =
                    slots.iter().zip(args.iter().cloned()).zip(written).map(|((s, a), t)| (*s, a, t)).collect();
                pairs.sort_by_key(|p| p.0);
                pairs.into_iter().map(|(_, a, t)| (a, t)).unzip()
            };
            let args = &args;
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
        // `x.total()` on a record or enum `x`: a call of its type's method.
        Expr::Action { receiver, method, args, pos }
            if env.get(receiver).is_some_and(|b| !matches!(b.ty, Ty::Cap { .. })) =>
        {
            let base = Expr::Var(receiver.clone(), *pos);
            method_call(ctx, &base, method, args, *pos, env, true)
        }
        Expr::Method { base, method, args, pos } => method_call(ctx, base, method, args, *pos, env, false),
        Expr::Action { receiver, method, args, pos } => {
            // Inside a capability action: note requests, and whether the @target and
            // @amount parameters reach one.
            if let Some(fl) = ctx.flow.as_mut()
                && fl.needs.contains(receiver)
            {
                fl.request = true;
                for a in args {
                    let r = reveals(a, env);
                    let uses = |name: &Option<String>| name.as_ref().is_some_and(|n| r.iter().any(|s| s.path.first() == Some(n)));
                    fl.target_used |= uses(&fl.target);
                    fl.amount_used |= uses(&fl.amount);
                }
            }
            let cap_path = match env.get(receiver).map(|b| &b.ty) {
                Some(Ty::Cap { path, .. }) => Some(path.clone()),
                _ => None,
            };
            match &cap_path {
                Some(path) if path.last() == Some(method) => {}
                Some(path) => {
                    let p = path.join(".");
                    ctx.err("E002", *pos, format!("capability '{receiver}' is `cap {p}` and does not permit '{method}'"))
                }
                None => ctx.err(
                    "E002",
                    *pos,
                    format!("'{receiver}.{method}' acts on the world but '{receiver}' is not a capability in scope"),
                ),
            }
            let arg_tys: Vec<Option<(Ty, bool)>> = args.iter().map(|a| type_of(ctx, a, env)).collect();
            for (a, t) in args.iter().zip(&arg_tys) {
                if let Some((Ty::Proposed(inner), _)) = t {
                    ctx.err(
                        "E001",
                        a.pos(),
                        format!("unvalidated Proposed<{inner}> passed to action '{receiver}.{method}'; use `validate` first"),
                    );
                }
            }
            // An action of a library's capability: typed by its declaration.
            if let Some(params) = cap_path.as_ref().and_then(|p| ctx.lib_actions.get(&p.join("."))).cloned() {
                let p = cap_path.as_ref().map(|p| p.join(".")).unwrap_or_default();
                if params.len() != args.len() {
                    ctx.err("E101", *pos, format!("'{p}' takes {} argument(s) but {} were given", params.len(), args.len()));
                } else {
                    for ((a, t), want) in args.iter().zip(&arg_tys).zip(&params) {
                        if let Some((t, _)) = t
                            && let Err((code, msg)) = assignable(t, want)
                        {
                            ctx.err(code, a.pos(), format!("in action '{p}': {msg}"));
                        }
                    }
                }
                return Some((named("Receipt"), false));
            }
            let kind = cap_path.as_deref().and_then(runtime_cap);
            let Some(kind) = kind else {
                // An adapter's action: anything but secrets may be passed.
                for (a, t) in args.iter().zip(&arg_tys) {
                    if let Some((t, _)) = t
                        && let Some(bad) = secret_inside(ctx, t, &mut HashSet::new())
                    {
                        ctx.err("E101", a.pos(), format!("a {bad} can only be sent as an HTTP header, through net.http"));
                    }
                }
                return Some((named("Receipt"), false));
            };
            let text = named("Text");
            let is_headers = |t: &Ty| matches!(t, Ty::List(el, _) if **el == named("Header"));
            let tys: Vec<Ty> = arg_tys.iter().map(|t| t.as_ref().map_or(named("?"), |(t, _)| t.clone())).collect();
            let (ok, want, result) = match kind {
                RuntimeCap::Http { body } => {
                    let base = if body { 2 } else { 1 };
                    // A body may be a Secret: a template filled by `fill_secret`.
                    let ok = (tys.len() == base || tys.len() == base + 1)
                        && tys[0] == text
                        && (!body || tys[1] == text || tys[1] == named("Secret"))
                        && tys.get(base).is_none_or(is_headers);
                    let want = if body {
                        format!("{method}(url: Text, body: Text or Secret) or {method}(url, body, headers: List<Header, N>)")
                    } else {
                        format!("{method}(url: Text) or {method}(url, headers: List<Header, N>)")
                    };
                    (ok, want, named("Response"))
                }
                RuntimeCap::FsRead => (tys.len() == 1 && tys[0] == text, "read(path: Text)".to_string(), text.clone()),
                RuntimeCap::FsWrite => (
                    tys.len() == 2 && tys.iter().all(|t| *t == text),
                    "write(path: Text, content: Text)".to_string(),
                    named("Receipt"),
                ),
            };
            if !ok && arg_tys.iter().all(Option::is_some) {
                let found: Vec<String> = tys.iter().map(|t| t.to_string()).collect();
                ctx.err("E101", *pos, format!("'{receiver}.{method}' takes {want}, found ({})", found.join(", ")));
            }
            // What comes back from the outside world is untrusted.
            Some((result, false))
        }
    }
}
