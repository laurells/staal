// Named arguments and default values.
//
// `fn send(to: Text, retries: Int = 3)` may be called `send(x)`, `send(x, 5)` or
// `send(to: x, retries: 5)`. After modules are resolved, every call is put in its
// final form: each argument tagged with the parameter it is for (`Expr::Call::slots`)
// and every parameter left out given its default. Arguments are still evaluated in the
// order they are written, and a default is a constant, so filling it in can neither
// fail nor reveal anything. The checker and both engines then see only complete calls.

use crate::ast::*;
use crate::checker::Diagnostic;
use crate::lexer::Pos;
use std::collections::HashMap;

fn err(pos: Pos, msg: String) -> Diagnostic {
    Diagnostic { code: "E101", pos, msg }
}

/// A function's parameters: names and defaults.
type Params = Vec<(String, Option<Expr>)>;

/// Checks every default and puts every call in its final form. Method calls are
/// calls only once the checker has resolved them by type, so until `last` their
/// named arguments are left as they are; then a named argument anywhere but a call to
/// a function is an error.
pub fn complete(prog: &mut Program, last: bool) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let consts: HashMap<String, usize> = prog.consts.iter().enumerate().map(|(i, c)| (c.name.clone(), i)).collect();
    for f in &prog.fns {
        let action = f.name.contains(' ');
        let mut seen_default = false;
        for p in &f.params {
            match &p.default {
                Some(d) if action => {
                    diags.push(err(d.pos(), format!("parameter '{}' of a capability's action cannot have a default", p.name)))
                }
                Some(d) => {
                    seen_default = true;
                    // The rules of a constant's value, in a default's words.
                    let mut found = Vec::new();
                    crate::consts::form(d, &consts, &mut found);
                    for mut x in found {
                        x.msg = x.msg.replace("a constant cannot", "a default cannot").replace("; a constant is", "; a default is");
                        diags.push(x);
                    }
                }
                None if seen_default && !action => diags.push(err(
                    f.pos,
                    format!("parameter '{}' of '{}' has no default but follows one that has; put parameters with defaults last", p.name, f.name),
                )),
                None => {}
            }
        }
    }
    let sigs: HashMap<String, Params> = prog
        .fns
        .iter()
        .filter(|f| !f.name.contains(' '))
        .map(|f| (f.name.clone(), f.params.iter().map(|p| (p.name.clone(), p.default.clone())).collect()))
        .collect();
    let mut c = Completer { sigs: &sigs, diags: &mut diags, last };
    for f in &mut prog.fns {
        c.block(&mut f.body);
        for p in &mut f.params {
            if let Some(d) = &mut p.default {
                c.expr(d);
            }
        }
    }
    for k in &mut prog.consts {
        c.expr(&mut k.value);
    }
    diags
}

struct Completer<'a, 'd> {
    sigs: &'a HashMap<String, Params>,
    diags: &'d mut Vec<Diagnostic>,
    last: bool,
}

impl Completer<'_, '_> {
    fn block(&mut self, body: &mut [Stmt]) {
        for s in body {
            match s {
                Stmt::Let { value, .. }
                | Stmt::Set { value, .. }
                | Stmt::Expr(value)
                | Stmt::Return(Some(value), _)
                | Stmt::Fail(value, _) => self.expr(value),
                Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => {}
                Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => self.block(body),
                Stmt::For { list, body, .. } => {
                    self.expr(list);
                    self.block(body);
                }
                Stmt::If { cond, then, els, .. } => {
                    self.expr(cond);
                    self.block(then);
                    self.block(els);
                }
                Stmt::Match { value, arms, .. } => {
                    self.expr(value);
                    for a in arms {
                        self.block(&mut a.body);
                    }
                }
            }
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        if let Expr::Call { name, args, slots, pos } = e {
            self.call(name, args, slots, *pos);
        }
        match e {
            // `x.m(a: 1)` may be a method call: decided once its type is known.
            Expr::Action { .. } | Expr::Method { .. } if !self.last => {
                for c in e.children_mut() {
                    match c {
                        Expr::Named { value, .. } => self.expr(value),
                        c => self.expr(c),
                    }
                }
            }
            // A named argument outside a call to a function has nothing to name.
            Expr::Named { name, value, pos } => {
                self.diags.push(err(
                    *pos,
                    format!("`{name}: ...` names a parameter, which only calls to functions have"),
                ));
                let v = std::mem::replace(value.as_mut(), Expr::Bool(false, *pos));
                *e = v;
                self.expr(e);
            }
            _ => {
                for c in e.children_mut() {
                    self.expr(c);
                }
            }
        }
    }

    fn call(&mut self, name: &str, args: &mut Vec<Expr>, slots: &mut Vec<usize>, pos: Pos) {
        let named = args.iter().any(|a| matches!(a, Expr::Named { .. }));
        let Some(params) = self.sigs.get(name) else {
            // An unknown function is the checker's to report; its arguments are plain.
            for a in args.iter_mut() {
                unwrap_named(a);
            }
            return;
        };
        if !named && args.len() == params.len() {
            return;
        }
        let mut written: Vec<(Expr, Option<usize>)> = Vec::with_capacity(args.len());
        let mut filled = vec![false; params.len()];
        let mut ok = true;
        let mut after_named = false;
        for (i, a) in std::mem::take(args).into_iter().enumerate() {
            match a {
                Expr::Named { name: pname, value, pos: apos } => {
                    after_named = true;
                    match params.iter().position(|(n, _)| *n == pname) {
                        None => {
                            self.diags.push(err(apos, format!("'{name}' has no parameter named '{pname}'")));
                            ok = false;
                        }
                        Some(k) if filled[k] => {
                            self.diags.push(err(apos, format!("argument '{pname}' of '{name}' is given twice")));
                            ok = false;
                        }
                        Some(k) => filled[k] = true,
                    }
                    let slot = params.iter().position(|(n, _)| *n == pname);
                    written.push((*value, slot));
                }
                a => {
                    if after_named {
                        self.diags.push(err(a.pos(), "an argument without a name cannot follow a named one".into()));
                        ok = false;
                    }
                    if i < params.len() && !filled[i] {
                        filled[i] = true;
                        written.push((a, Some(i)));
                    } else {
                        written.push((a, None));
                        if i >= params.len() {
                            // Too many: the checker reports the count.
                            ok = false;
                        }
                    }
                }
            }
        }
        if ok {
            for (k, (pname, default)) in params.iter().enumerate() {
                if filled[k] {
                    continue;
                }
                match default {
                    Some(d) => written.push((d.clone(), Some(k))),
                    None => {
                        self.diags.push(err(pos, format!("the call to '{name}' is missing argument '{pname}'")));
                        ok = false;
                    }
                }
            }
        }
        let identity = written.len() == params.len() && written.iter().enumerate().all(|(i, (_, s))| *s == Some(i));
        *slots = if ok && !identity { written.iter().map(|(_, s)| s.expect("every argument has a slot")).collect() } else { Vec::new() };
        *args = written.into_iter().map(|(a, _)| a).collect();
    }
}

fn unwrap_named(a: &mut Expr) {
    if let Expr::Named { value, pos, .. } = a {
        let pos = *pos;
        *a = std::mem::replace(value.as_mut(), Expr::Bool(false, pos));
    }
}
