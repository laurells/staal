// Type checker. Enforces the first two Intent guarantees:
//   E001  a model's Proposed<T> value is used in an action without `validate`
//   E002  an action is performed without a matching capability in scope
// plus ordinary errors (E100 undefined name, E101 type mismatch, E102 unknown type).

use crate::ast::*;
use crate::lexer::Pos;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: &'static str,
    pub pos: Pos,
    pub msg: String,
}

pub fn check(prog: &Program) -> Vec<Diagnostic> {
    let mut known: HashSet<String> = ["Text", "Int", "Bool", "Money"].iter().map(|s| s.to_string()).collect();
    for t in &prog.types {
        known.insert(t.name.clone());
    }
    let mut diags = Vec::new();
    for t in &prog.types {
        for (_, fty) in &t.fields {
            check_ty_known(fty, &known, t.pos, &mut diags);
        }
    }
    for f in &prog.fns {
        check_fn(f, &known, &mut diags);
    }
    diags
}

fn check_ty_known(ty: &Ty, known: &HashSet<String>, pos: Pos, diags: &mut Vec<Diagnostic>) {
    match ty {
        Ty::Named(n) if !known.contains(n) => diags.push(Diagnostic {
            code: "E102",
            pos,
            msg: format!("unknown type '{n}'"),
        }),
        Ty::Proposed(inner) => check_ty_known(inner, known, pos, diags),
        _ => {}
    }
}

fn check_fn(f: &FnDecl, known: &HashSet<String>, diags: &mut Vec<Diagnostic>) {
    let mut env: HashMap<String, Ty> = HashMap::new();
    for p in &f.params {
        check_ty_known(&p.ty, known, f.pos, diags);
        env.insert(p.name.clone(), p.ty.clone());
    }
    if let Some(r) = &f.ret {
        check_ty_known(r, known, f.pos, diags);
    }

    for stmt in &f.body {
        match stmt {
            Stmt::Let { name, ty, value, pos } => {
                let Some(vty) = type_of(value, &env, known, diags) else { continue };
                if let Some(decl) = ty {
                    check_ty_known(decl, known, *pos, diags);
                    if *decl != vty {
                        diags.push(Diagnostic {
                            code: "E101",
                            pos: *pos,
                            msg: format!("'{name}' is declared as {decl} but the value is {vty}"),
                        });
                    }
                }
                env.insert(name.clone(), ty.clone().unwrap_or(vty));
            }
            Stmt::Expr(e) => {
                type_of(e, &env, known, diags);
            }
            Stmt::Return(e, pos) => {
                if let (Some(vty), Some(ret)) = (type_of(e, &env, known, diags), &f.ret) {
                    if let Ty::Proposed(_) = vty {
                        if !matches!(ret, Ty::Proposed(_)) {
                            diags.push(Diagnostic {
                                code: "E001",
                                pos: *pos,
                                msg: format!("returning unvalidated {vty} where {ret} is expected; use `validate` first"),
                            });
                            continue;
                        }
                    }
                    if vty != *ret {
                        diags.push(Diagnostic {
                            code: "E101",
                            pos: *pos,
                            msg: format!("function '{}' returns {ret} but this value is {vty}", f.name),
                        });
                    }
                }
            }
        }
    }
}

fn type_of(
    e: &Expr,
    env: &HashMap<String, Ty>,
    known: &HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) -> Option<Ty> {
    match e {
        Expr::Str(_, _) => Some(Ty::Named("Text".into())),
        Expr::Var(n, pos) => match env.get(n) {
            Some(t) => Some(t.clone()),
            None => {
                diags.push(Diagnostic { code: "E100", pos: *pos, msg: format!("undefined name '{n}'") });
                None
            }
        },
        Expr::Propose(t, pos) => {
            check_ty_known(t, known, *pos, diags);
            Some(Ty::Proposed(Box::new(t.clone())))
        }
        Expr::Validate(inner, pos) => match type_of(inner, env, known, diags)? {
            Ty::Proposed(t) => Some(*t),
            other => {
                diags.push(Diagnostic {
                    code: "E101",
                    pos: *pos,
                    msg: format!("`validate` expects a Proposed value, found {other}"),
                });
                None
            }
        },
        Expr::Action { receiver, method, args, pos } => {
            // E002: the receiver must be a capability whose last segment is the method.
            match env.get(receiver) {
                Some(Ty::Cap(path)) if path.last() == Some(method) => {}
                Some(Ty::Cap(path)) => diags.push(Diagnostic {
                    code: "E002",
                    pos: *pos,
                    msg: format!(
                        "capability '{receiver}' is `cap {}` and does not permit '{method}'",
                        path.join(".")
                    ),
                }),
                _ => diags.push(Diagnostic {
                    code: "E002",
                    pos: *pos,
                    msg: format!("'{receiver}.{method}' acts on the world but '{receiver}' is not a capability in scope"),
                }),
            }
            // E001: no unvalidated model output may reach an action.
            for a in args {
                if let Some(Ty::Proposed(inner)) = type_of(a, env, known, diags) {
                    diags.push(Diagnostic {
                        code: "E001",
                        pos: a.pos(),
                        msg: format!(
                            "unvalidated Proposed<{inner}> passed to action '{receiver}.{method}'; use `validate` first"
                        ),
                    });
                }
            }
            Some(Ty::Named("Receipt".into()))
        }
    }
}
