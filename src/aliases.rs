// Type aliases: `type Name = T` at the top of a file.
//
// After modules are resolved (an alias is named, exported and imported like any type),
// every use of an alias is replaced by the type it names, with the aliases in that type
// replaced in turn. The checker and both engines never see an alias: `Amount` where
// `type Amount = Money` is exactly `Money`, so no rule can treat the two differently.
// A cycle (`type A = List<A, 3>`) is an error, since it would name no type at all.

use crate::ast::*;
use crate::checker::Diagnostic;
use crate::lexer::Pos;
use std::collections::HashMap;

fn err(pos: Pos, msg: String) -> Diagnostic {
    Diagnostic { code: "E101", pos, msg }
}

/// Replaces every alias in the program by its type. Afterwards each alias in
/// `prog.aliases` holds its own type with no alias in it, for the checker to check.
pub fn expand(prog: &mut Program) -> Vec<Diagnostic> {
    if prog.aliases.is_empty() {
        return Vec::new();
    }
    let index: HashMap<String, usize> = prog.aliases.iter().enumerate().map(|(i, a)| (a.name.clone(), i)).collect();
    let mut done: Vec<Option<Ty>> = vec![None; prog.aliases.len()];
    for i in 0..prog.aliases.len() {
        if let Err(d) = expanded(i, &prog.aliases, &index, &mut done, &mut Vec::new()) {
            return vec![d];
        }
    }
    for (a, t) in prog.aliases.iter_mut().zip(done) {
        a.ty = t.expect("every alias is expanded");
    }
    let to: HashMap<String, Ty> = prog.aliases.iter().map(|a| (a.name.clone(), a.ty.clone())).collect();
    let mut diags = Vec::new();
    let mut x = Expander { to: &to, diags: &mut diags };
    for t in &mut prog.types {
        for (_, ft) in &mut t.fields {
            x.ty(ft);
        }
    }
    for e in &mut prog.enums {
        for v in &mut e.variants {
            for (_, ft) in &mut v.fields {
                x.ty(ft);
            }
        }
    }
    for c in &mut prog.caps {
        for (_, t, _) in &mut c.needs {
            x.ty(t);
        }
    }
    for c in &mut prog.consts {
        if let Some(t) = &mut c.ty {
            x.ty(t);
        }
        x.expr(&mut c.value);
    }
    for f in &mut prog.fns {
        for p in &mut f.params {
            x.ty(&mut p.ty);
            if let Some(d) = &mut p.default {
                x.expr(d);
            }
        }
        if let Some(t) = &mut f.ret {
            x.ty(t);
        }
        x.block(&mut f.body);
    }
    diags
}

/// The type alias `i` names, with every alias in it replaced.
fn expanded(
    i: usize,
    aliases: &[AliasDecl],
    index: &HashMap<String, usize>,
    done: &mut Vec<Option<Ty>>,
    path: &mut Vec<usize>,
) -> Result<Ty, Diagnostic> {
    if let Some(t) = &done[i] {
        return Ok(t.clone());
    }
    if let Some(at) = path.iter().position(|p| *p == i) {
        let names: Vec<&str> = path[at..].iter().chain(std::iter::once(&i)).map(|p| aliases[*p].name.as_str()).collect();
        return Err(err(aliases[i].pos, format!("type aliases name each other in a cycle: {}", names.join(" -> "))));
    }
    path.push(i);
    let mut t = aliases[i].ty.clone();
    let r = replace_in(&mut t, &mut |n| match index.get(n) {
        Some(&j) => expanded(j, aliases, index, done, path).map(Some),
        None => Ok(None),
    });
    path.pop();
    r?;
    done[i] = Some(t.clone());
    Ok(t)
}

/// Replaces each named type `with` maps to something.
fn replace_in(t: &mut Ty, with: &mut dyn FnMut(&str) -> Result<Option<Ty>, Diagnostic>) -> Result<(), Diagnostic> {
    match t {
        Ty::Named(n) => {
            if let Some(r) = with(n)? {
                *t = r;
            }
            Ok(())
        }
        Ty::Proposed(inner) | Ty::List(inner, _) | Ty::Optional(inner) => replace_in(inner, with),
        Ty::Tuple(items) => items.iter_mut().try_for_each(|i| replace_in(i, with)),
        Ty::Cap { .. } => Ok(()),
    }
}

struct Expander<'a, 'd> {
    to: &'a HashMap<String, Ty>,
    diags: &'d mut Vec<Diagnostic>,
}

impl Expander<'_, '_> {
    fn ty(&mut self, t: &mut Ty) {
        let to = self.to;
        let _ = replace_in(t, &mut |n| Ok(to.get(n).cloned()));
    }

    fn block(&mut self, body: &mut [Stmt]) {
        for s in body {
            match s {
                Stmt::Let { ty, value, .. } => {
                    if let Some(t) = ty {
                        self.ty(t);
                    }
                    self.expr(value);
                }
                Stmt::Set { value, .. } | Stmt::Expr(value) | Stmt::Return(Some(value), _) | Stmt::Fail(value, _) => {
                    self.expr(value)
                }
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
        match e {
            Expr::Propose { ty, .. } | Expr::Parse { ty, .. } => self.ty(ty),
            // `Alias { ... }` builds the record the alias names.
            Expr::Record { ty, pos, .. } => match self.to.get(ty.as_str()) {
                Some(Ty::Named(n)) => *ty = n.clone(),
                Some(other) => self.diags.push(err(
                    *pos,
                    format!("'{ty}' is another name for {other}; only a record type can be built with {{ ... }}"),
                )),
                None => {}
            },
            _ => {}
        }
        for c in e.children_mut() {
            self.expr(c);
        }
    }
}
