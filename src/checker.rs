// Type checker. Enforces:
//   E001  a model's Proposed<T> value is used where a plain T is needed, without `validate`
//   E002  an action is performed without a matching capability in scope
//   E003  a capability is widened (given more authority than it holds)
//   E006  an untrusted value is used as a model instruction, or flows into an @trusted slot
// plus ordinary errors:
//   E100  undefined name or function
//   E101  type mismatch or wrong number of arguments
//   E102  unknown type
//   E103  `break` outside a loop
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
    params: Vec<(Ty, bool)>,
    ret: Ty,
    ret_trusted: bool,
}

struct Ctx<'a> {
    known: HashSet<String>,
    sigs: HashMap<&'a str, Sig>,
    diags: Vec<Diagnostic>,
}

impl Ctx<'_> {
    fn err(&mut self, code: &'static str, pos: Pos, msg: String) {
        self.diags.push(Diagnostic { code, pos, msg });
    }
}

/// A variable's type and whether its value is trusted.
#[derive(Clone)]
struct Binding {
    ty: Ty,
    trusted: bool,
}

type Env = HashMap<String, Binding>;

fn named(n: &str) -> Ty {
    Ty::Named(n.into())
}

pub fn check(prog: &Program) -> Vec<Diagnostic> {
    let mut ctx = Ctx {
        known: ["Text", "Int", "Bool", "Money", "Unit", "Receipt"].iter().map(|s| s.to_string()).collect(),
        sigs: HashMap::new(),
        diags: Vec::new(),
    };
    for t in &prog.types {
        ctx.known.insert(t.name.clone());
    }
    for f in &prog.fns {
        if ctx.sigs.contains_key(f.name.as_str()) {
            ctx.err("E101", f.pos, format!("function '{}' is defined twice", f.name));
            continue;
        }
        ctx.sigs.insert(
            &f.name,
            Sig {
                params: f.params.iter().map(|p| (p.ty.clone(), p.trusted)).collect(),
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
    for p in &f.params {
        check_ty_known(ctx, &p.ty, f.pos);
        env.insert(p.name.clone(), Binding { ty: p.ty.clone(), trusted: p.trusted });
    }
    let ret = f.ret.clone().unwrap_or_else(|| named("Unit"));
    check_ty_known(ctx, &ret, f.pos);
    check_block(ctx, f, &ret, &f.body, env, false);
}

/// Checks a block. `env` is a copy, so names bound inside a nested block do not leak out.
fn check_block(ctx: &mut Ctx, f: &FnDecl, ret: &Ty, body: &[Stmt], mut env: Env, in_loop: bool) {
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
                env.insert(name.clone(), Binding { ty: bound, trusted });
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
                check_block(ctx, f, ret, body, env.clone(), true);
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
                check_block(ctx, f, ret, then, env.clone(), in_loop);
                check_block(ctx, f, ret, els, env.clone(), in_loop);
            }
            Stmt::Break(pos) => {
                if !in_loop {
                    ctx.err("E103", *pos, "`break` is only allowed inside `repeat` or `loop`".into());
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
        Expr::Propose { ty, instruct, data, pos, .. } => {
            check_ty_known(ctx, ty, *pos);
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
            if params.len() != args.len() {
                ctx.err(
                    "E101",
                    *pos,
                    format!("'{name}' takes {} argument(s) but {} were given", params.len(), args.len()),
                );
                return Some((ret, ret_trusted));
            }
            for ((arg, aty), (pty, ptrusted)) in args.iter().zip(arg_tys).zip(&params) {
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
