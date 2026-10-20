// Constants: `const NAME: T = value` at the top of a file.
//
// A constant's value is computed only from literals, other constants, and pure
// operations on them: arithmetic, comparison, logic, records, lists, field reads,
// indexing and the pure built-ins. No variable, call, action, model call, `parse`,
// `validate`, `try` or `done`. So a constant is known when the program is checked.
//
// After modules are resolved, every use of a constant is replaced by its value's
// expression, with the constants it uses replaced in turn. The checker and both
// engines then see literals and nothing else: a constant is trusted, reveals nothing,
// and costs nothing, exactly like the literal it stands for, with no rule of its own
// to get wrong. The checker still checks each constant's type, and evaluates it once
// (`check`), so a constant that overflows or divides by zero is a compile error.

use crate::ast::*;
use crate::checker::Diagnostic;
use std::collections::HashMap;

fn err(pos: crate::lexer::Pos, msg: String) -> Diagnostic {
    Diagnostic { code: "E101", pos, msg }
}

/// Checks the form of every constant, rejects cycles, and replaces every use.
pub fn expand(prog: &mut Program) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let index: HashMap<String, usize> = prog.consts.iter().enumerate().map(|(i, c)| (c.name.clone(), i)).collect();
    for c in &prog.consts {
        form(&c.value, &index, &mut diags);
    }
    if !diags.is_empty() {
        return diags;
    }
    // Expand in dependency order; a constant met again while it is being expanded is
    // part of a cycle.
    let mut done: Vec<Option<Expr>> = vec![None; prog.consts.len()];
    for i in 0..prog.consts.len() {
        let mut path = Vec::new();
        if let Err(d) = expanded(i, &prog.consts, &index, &mut done, &mut path) {
            diags.push(d);
            return diags;
        }
    }
    for (c, v) in prog.consts.iter_mut().zip(done) {
        c.value = v.expect("every constant is expanded");
    }
    let values: HashMap<String, Expr> = prog.consts.iter().map(|c| (c.name.clone(), c.value.clone())).collect();
    for f in &mut prog.fns {
        for s in &mut f.body {
            stmt(s, &values);
        }
        for p in &mut f.params {
            if let Some(d) = &mut p.default {
                replace(d, &mut |n| values.get(n).cloned());
            }
        }
    }
    diags
}

/// The value of constant `i` with every constant in it replaced.
fn expanded(
    i: usize,
    consts: &[ConstDecl],
    index: &HashMap<String, usize>,
    done: &mut Vec<Option<Expr>>,
    path: &mut Vec<usize>,
) -> Result<Expr, Diagnostic> {
    if let Some(v) = &done[i] {
        return Ok(v.clone());
    }
    if let Some(at) = path.iter().position(|p| *p == i) {
        let names: Vec<&str> = path[at..].iter().chain(std::iter::once(&i)).map(|p| consts[*p].name.as_str()).collect();
        return Err(err(consts[i].pos, format!("constants depend on each other in a cycle: {}", names.join(" -> "))));
    }
    path.push(i);
    let mut v = consts[i].value.clone();
    let mut failure = None;
    replace(&mut v, &mut |name| match index.get(name) {
        Some(&j) => match expanded(j, consts, index, done, path) {
            Ok(e) => Some(e),
            Err(d) => {
                failure.get_or_insert(d);
                None
            }
        },
        None => None,
    });
    path.pop();
    if let Some(d) = failure {
        return Err(d);
    }
    done[i] = Some(v.clone());
    Ok(v)
}

/// Replaces every variable `with` knows inside `e`. What replaces it keeps its own
/// positions, so a message about a constant's value points at its definition.
fn replace(e: &mut Expr, with: &mut dyn FnMut(&str) -> Option<Expr>) {
    if let Expr::Var(name, _) = e
        && let Some(v) = with(name)
    {
        *e = v;
        return;
    }
    if let Expr::Lambda { body, .. } = e {
        for s in body.iter_mut() {
            stmt_with(s, with);
        }
        return;
    }
    for c in e.children_mut() {
        replace(c, with);
    }
}

fn stmt(s: &mut Stmt, values: &HashMap<String, Expr>) {
    let mut with = |n: &str| values.get(n).cloned();
    stmt_with(s, &mut with);
}

fn stmt_with(s: &mut Stmt, with: &mut dyn FnMut(&str) -> Option<Expr>) {
    match s {
        Stmt::Let { value, .. } | Stmt::Set { value, .. } | Stmt::Expr(value) | Stmt::Return(Some(value), _) | Stmt::Fail(value, _) => {
            replace(value, with)
        }
        Stmt::If { cond, then, els, .. } => {
            replace(cond, with);
            for s in then.iter_mut().chain(els.iter_mut()) {
                stmt_with(s, with);
            }
        }
        Stmt::For { list, body, .. } => {
            replace(list, with);
            for s in body {
                stmt_with(s, with);
            }
        }
        Stmt::Match { value, arms, .. } => {
            replace(value, with);
            for s in arms.iter_mut().flat_map(|a| a.body.iter_mut()) {
                stmt_with(s, with);
            }
        }
        Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => {
            for s in body {
                stmt_with(s, with);
            }
        }
        Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => {}
    }
}

/// Reports anything in a constant's value that is not known when the program is checked.
pub(crate) fn form(e: &Expr, index: &HashMap<String, usize>, diags: &mut Vec<Diagnostic>) {
    let bad = match e {
        Expr::Var(name, _) if !index.contains_key(name) => {
            Some(format!("'{name}' is not a constant; a constant is computed only from literals and other constants"))
        }
        Expr::Builtin { f: Builtin::Header | Builtin::FillSecret, .. } => Some("a constant cannot hold a header or a secret".to_string()),
        Expr::Call { .. } => Some("a constant cannot call a function".to_string()),
        Expr::Action { .. } => Some("a constant cannot perform an action".to_string()),
        Expr::Propose { .. } => Some("a constant cannot ask a model".to_string()),
        Expr::Validate(..) | Expr::Parse { .. } => Some("a constant cannot hold or check outside data".to_string()),
        Expr::Try(..) => Some("`try` has nothing to catch in a constant".to_string()),
        Expr::Done { .. } => Some("a constant cannot be evidence of work".to_string()),
        Expr::Lambda { .. } | Expr::Closure { .. } | Expr::CallValue { .. } => {
            Some("a constant cannot be or call a function value".to_string())
        }
        // They need a capacity only known from a type; write the literal instead.
        Expr::Builtin { f: Builtin::Put | Builtin::Add, .. } => Some("a constant cannot grow a map or set; write it as a literal".to_string()),
        Expr::Builtin { f: Builtin::Append, .. } => Some("a constant cannot grow a list; write it as a literal".to_string()),
        _ => None,
    };
    if let Some(msg) = bad {
        diags.push(err(e.pos(), msg));
        return;
    }
    for c in e.children() {
        form(c, index, diags);
    }
}
