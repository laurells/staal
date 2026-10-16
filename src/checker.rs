// Type checker. Enforces:
//   E001  a model's Proposed<T> value is used where a plain T is needed, without `validate`
//   E002  an action is performed without a matching capability in scope
//   E003  a capability is widened (given more authority than it holds)
// plus ordinary errors:
//   E100  undefined name or function
//   E101  type mismatch or wrong number of arguments
//   E102  unknown type
//
// Every place a value flows into a slot (call argument, typed `let`, `return`)
// goes through `assignable`, so the authority rules are decided in one place.

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
    params: Vec<Ty>,
    ret: Ty,
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

fn unit() -> Ty {
    Ty::Named("Unit".into())
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
            Sig { params: f.params.iter().map(|p| p.ty.clone()).collect(), ret: f.ret.clone().unwrap_or_else(unit) },
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
                // Slot has no limit: only an unlimited capability may fill it.
                (Some(f), None) => Err((
                    "E003",
                    format!("capability limited to {f} cannot be widened to unlimited `cap {}`", tp.join(".")),
                )),
                // Holder is unlimited, or slot is unlimited and holder is too: any narrowing is fine.
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
    let mut env: HashMap<String, Ty> = HashMap::new();
    for p in &f.params {
        check_ty_known(ctx, &p.ty, f.pos);
        env.insert(p.name.clone(), p.ty.clone());
    }
    let ret = f.ret.clone().unwrap_or_else(unit);
    check_ty_known(ctx, &ret, f.pos);
    check_block(ctx, f, &ret, &f.body, env);
}

/// Checks a block. `env` is a copy, so names bound inside a loop body do not leak out.
fn check_block(ctx: &mut Ctx, f: &FnDecl, ret: &Ty, body: &[Stmt], mut env: HashMap<String, Ty>) {
    for stmt in body {
        match stmt {
            Stmt::Let { name, ty, value, pos } => {
                let Some(vty) = type_of(ctx, value, &env) else { continue };
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
                env.insert(name.clone(), bound);
            }
            Stmt::Expr(e) => {
                type_of(ctx, e, &env);
            }
            Stmt::Return(e, pos) => {
                if let Some(vty) = type_of(ctx, e, &env) {
                    if let Err((code, msg)) = assignable(&vty, ret) {
                        ctx.err(code, *pos, format!("in return from '{}': {msg}", f.name));
                    }
                }
            }
            Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => {
                check_block(ctx, f, ret, body, env.clone());
            }
        }
    }
}

fn type_of(ctx: &mut Ctx, e: &Expr, env: &HashMap<String, Ty>) -> Option<Ty> {
    match e {
        Expr::Str(_, _) => Some(Ty::Named("Text".into())),
        Expr::Int(_, _) => Some(Ty::Named("Int".into())),
        Expr::Var(n, pos) => match env.get(n) {
            Some(t) => Some(t.clone()),
            None => {
                ctx.err("E100", *pos, format!("undefined name '{n}'"));
                None
            }
        },
        Expr::Propose { ty, pos, .. } => {
            check_ty_known(ctx, ty, *pos);
            Some(Ty::Proposed(Box::new(ty.clone())))
        }
        Expr::Validate(inner, pos) => match type_of(ctx, inner, env)? {
            Ty::Proposed(t) => Some(*t),
            other => {
                ctx.err("E101", *pos, format!("`validate` expects a Proposed value, found {other}"));
                None
            }
        },
        Expr::Call { name, args, pos } => {
            let arg_tys: Vec<Option<Ty>> = args.iter().map(|a| type_of(ctx, a, env)).collect();
            let Some(sig) = ctx.sigs.get(name.as_str()) else {
                ctx.err("E100", *pos, format!("undefined function '{name}'"));
                return None;
            };
            let (params, ret) = (sig.params.clone(), sig.ret.clone());
            if params.len() != args.len() {
                ctx.err(
                    "E101",
                    *pos,
                    format!("'{name}' takes {} argument(s) but {} were given", params.len(), args.len()),
                );
                return Some(ret);
            }
            for ((arg, aty), pty) in args.iter().zip(arg_tys).zip(&params) {
                if let Some(aty) = aty {
                    if let Err((code, msg)) = assignable(&aty, pty) {
                        ctx.err(code, arg.pos(), format!("in call to '{name}': {msg}"));
                    }
                }
            }
            Some(ret)
        }
        Expr::Action { receiver, method, args, pos } => {
            // E002: the receiver must be a capability whose last segment is the method.
            match env.get(receiver) {
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
            // E001: no unvalidated model output may reach an action.
            for a in args {
                if let Some(Ty::Proposed(inner)) = type_of(ctx, a, env) {
                    ctx.err(
                        "E001",
                        a.pos(),
                        format!("unvalidated Proposed<{inner}> passed to action '{receiver}.{method}'; use `validate` first"),
                    );
                }
            }
            Some(Ty::Named("Receipt".into()))
        }
    }
}
