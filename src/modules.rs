// Modules: loading the files of a program and resolving names across them.
//
// A file is a module. `import sms.termii` loads `sms/termii.int` from the program's
// root folder (the folder of the entry file) and makes it known as `termii`;
// `import sms.termii as t` names it `t`; `import sms.termii { Message, send }` also
// brings those names in unqualified. Declarations are private unless marked `pub`.
//
// Resolution turns all modules into ONE program in which every name is qualified by
// its module: `send` in sms/termii.int becomes `sms.termii.send`. Names in the entry
// file keep their plain form. Identifiers cannot contain `.`, so qualified names never
// collide with each other or with entry names. The checker then sees the whole program
// exactly as it sees a single file, so every rule (blinding, budgets, trust, evidence,
// partiality) holds across module boundaries with no separate cross-module logic.
//
// Errors:
//   E106  an import problem: a module that cannot be found, an import cycle, a name
//         that clashes with an import, a qualifier that is not an import
//   E107  a declaration used from another module without being `pub`
// Undefined names keep the qualification of the module they were written in, so an
// unknown `f` in a library is reported as `sms.termii.f` (E100) and can never be
// mistaken for a function of the same name elsewhere.

use crate::ast::*;
use crate::checker::Diagnostic;
use crate::lexer::{self, Pos};
use crate::parser::Parser;
use std::collections::{HashMap, HashSet};

/// The types every module can name without importing them. A module may not declare
/// a type with one of these names.
pub const BUILTIN_TYPES: &[&str] =
    &["Text", "Int", "Bool", "Money", "Unit", "Receipt", "Outcome", "Done", "Secret", "Header", "Response"];

/// One source file of a program. `name` is how it is shown in messages.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
}

/// The result of loading a program's files.
pub struct Loaded {
    /// Every file, indexed by `Pos::file` (0 is the entry file).
    pub files: Vec<SourceFile>,
    /// The module path of each file (empty for the entry file).
    pub paths: Vec<Vec<String>>,
    /// The resolved program, or the errors that prevent resolving it.
    pub program: Result<Program, Vec<Diagnostic>>,
}

impl Loaded {
    /// Text that identifies the program exactly: a journal records its hash, so a run
    /// cannot be resumed after any of its files changed. For a single-file program it
    /// is that file's text.
    pub fn fingerprint(&self) -> String {
        if self.files.len() == 1 {
            return self.files[0].text.clone();
        }
        // Module paths, not file names: the same program resumed from another folder,
        // or after the folder was moved, is still the same program.
        let mut s = String::from("intent-modules-v2\n");
        for (f, path) in self.files.iter().zip(&self.paths) {
            let name = path.join(".");
            s.push_str(&format!("{}\n{name}\n{}\n{}\n", name.len(), f.text.len(), f.text));
        }
        s
    }
}

struct Module {
    /// Empty for the entry file.
    path: Vec<String>,
    prog: Program,
}

impl Module {
    /// What names declared here are qualified with: "" for the entry, "a.b." otherwise.
    fn prefix(&self) -> String {
        if self.path.is_empty() {
            String::new()
        } else {
            format!("{}.", self.path.join("."))
        }
    }
}

fn err(code: &'static str, pos: Pos, msg: String) -> Diagnostic {
    Diagnostic { code, pos, msg }
}

fn parse(text: &str, file: u32) -> Result<Program, Diagnostic> {
    let toks = lexer::lex_file(text, file).map_err(|(pos, msg)| err("E900", pos, msg))?;
    Parser::new(toks).program().map_err(|(pos, msg)| err("E901", pos, msg))
}

/// Loads the entry file and every module it imports, directly or indirectly.
/// `fetch` returns the file for a module path, or why it cannot.
pub fn load(entry: SourceFile, mut fetch: impl FnMut(&[String]) -> Result<SourceFile, String>) -> Loaded {
    let mut files = vec![entry];
    let mut paths: Vec<Vec<String>> = vec![Vec::new()];
    let prog = match parse(&files[0].text, 0) {
        Ok(p) => p,
        Err(d) => return Loaded { files, paths, program: Err(vec![d]) },
    };
    let mut modules = vec![Module { path: Vec::new(), prog }];
    let mut index: HashMap<Vec<String>, usize> = HashMap::new();
    index.insert(Vec::new(), 0);
    let mut diags = Vec::new();

    // Depth first, so that a cycle is seen as an import of a module still being loaded.
    // Each stack entry is a module and how many of its imports have been followed.
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    while let Some(&(m, next)) = stack.last() {
        let Some(imp) = modules[m].prog.imports.get(next).cloned() else {
            stack.pop();
            continue;
        };
        stack.last_mut().expect("non-empty").1 += 1;
        if let Some(&i) = index.get(&imp.path) {
            if let Some(at) = stack.iter().position(|&(s, _)| s == i) {
                let mut cycle: Vec<String> = stack[at..].iter().map(|&(s, _)| display(&modules[s].path)).collect();
                cycle.push(display(&imp.path));
                diags.push(err("E106", imp.pos, format!("import cycle: {}", cycle.join(" imports "))));
            }
            continue;
        }
        let file = match fetch(&imp.path) {
            Ok(f) => f,
            Err(why) => {
                diags.push(err("E106", imp.pos, format!("cannot import '{}': {why}", imp.path.join("."))));
                continue;
            }
        };
        let id = files.len() as u32;
        files.push(file);
        paths.push(imp.path.clone());
        match parse(&files[id as usize].text, id) {
            Ok(prog) => {
                index.insert(imp.path.clone(), modules.len());
                modules.push(Module { path: imp.path.clone(), prog });
                stack.push((modules.len() - 1, 0));
            }
            // A file that does not parse cannot be resolved; nothing more is checked.
            Err(d) => return Loaded { files, paths, program: Err(vec![d]) },
        }
    }
    if !diags.is_empty() {
        return Loaded { files, paths, program: Err(diags) };
    }
    let program = resolve(modules, &index);
    Loaded { files, paths, program }
}

/// How a module path is shown: the entry file is "the program".
fn display(path: &[String]) -> String {
    if path.is_empty() {
        "the program".into()
    } else {
        format!("'{}'", path.join("."))
    }
}

/// What a module declares: name -> (qualified name, pub).
#[derive(Default)]
struct Exports {
    types: HashMap<String, (String, bool)>,
    fns: HashMap<String, (String, bool)>,
}

/// The names visible inside one module.
struct Scope<'a> {
    prefix: String,
    /// Unqualified type and function names: local declarations and `{ ... }` imports.
    types: HashMap<String, String>,
    fns: HashMap<String, String>,
    /// Import alias -> what that module exports.
    aliases: HashMap<String, &'a Exports>,
    alias_prefix: HashMap<String, String>,
}

fn resolve(mut modules: Vec<Module>, index: &HashMap<Vec<String>, usize>) -> Result<Program, Vec<Diagnostic>> {
    let mut diags = Vec::new();
    let exports: Vec<Exports> = modules
        .iter()
        .map(|m| {
            let p = m.prefix();
            let mut e = Exports::default();
            for t in &m.prog.types {
                e.types.entry(t.name.clone()).or_insert((format!("{p}{}", t.name), t.public));
            }
            for f in &m.prog.fns {
                e.fns.entry(f.name.clone()).or_insert((format!("{p}{}", f.name), f.public));
            }
            e
        })
        .collect();

    let mut out = Program::default();
    for (mi, m) in modules.iter_mut().enumerate() {
        let mut scope = Scope {
            prefix: m.prefix(),
            types: HashMap::new(),
            fns: HashMap::new(),
            aliases: HashMap::new(),
            alias_prefix: HashMap::new(),
        };
        for (n, (q, _)) in &exports[mi].types {
            scope.types.insert(n.clone(), q.clone());
        }
        for (n, (q, _)) in &exports[mi].fns {
            scope.fns.insert(n.clone(), q.clone());
        }
        if !m.path.is_empty() {
            for t in &m.prog.types {
                if BUILTIN_TYPES.contains(&t.name.as_str()) {
                    diags.push(err("E101", t.pos, format!("type '{}' is built in and cannot be declared", t.name)));
                }
            }
        }
        let mut seen_paths = HashSet::new();
        let mut brought_in: HashSet<String> = HashSet::new();
        for imp in &m.prog.imports {
            if !seen_paths.insert(imp.path.clone()) {
                diags.push(err("E106", imp.pos, format!("'{}' is imported twice", imp.path.join("."))));
                continue;
            }
            let target = index[&imp.path];
            let tex = &exports[target];
            if scope.aliases.contains_key(&imp.alias) {
                diags.push(err(
                    "E106",
                    imp.pos,
                    format!("two imports are both named '{}'; rename one with `as`", imp.alias),
                ));
                continue;
            }
            if exports[mi].types.contains_key(&imp.alias) || exports[mi].fns.contains_key(&imp.alias) {
                diags.push(err(
                    "E106",
                    imp.pos,
                    format!("import '{}' has the same name as a declaration in this file; rename it with `as`", imp.alias),
                ));
                continue;
            }
            scope.aliases.insert(imp.alias.clone(), tex);
            scope.alias_prefix.insert(imp.alias.clone(), modules_prefix(&imp.path));
            for (n, npos) in &imp.names {
                let ty = tex.types.get(n);
                let f = tex.fns.get(n);
                if ty.is_none() && f.is_none() {
                    diags.push(err("E100", *npos, format!("module '{}' has no '{n}'", imp.path.join("."))));
                    continue;
                }
                if ty.is_some_and(|t| !t.1) || f.is_some_and(|f| !f.1) {
                    diags.push(err(
                        "E107",
                        *npos,
                        format!("'{n}' in module '{}' is not `pub`, so it cannot be imported", imp.path.join(".")),
                    ));
                    continue;
                }
                // A name may come from one place only: this file, or one import.
                let local_clash = (ty.is_some() && exports[mi].types.contains_key(n))
                    || (f.is_some() && exports[mi].fns.contains_key(n));
                if local_clash || !brought_in.insert(n.clone()) {
                    diags.push(err("E106", *npos, format!("'{n}' is already defined in this file")));
                    continue;
                }
                if let Some((q, _)) = ty {
                    scope.types.insert(n.clone(), q.clone());
                }
                if let Some((q, _)) = f {
                    scope.fns.insert(n.clone(), q.clone());
                }
            }
        }
        let mut r = Resolver { scope: &scope, diags: &mut diags };
        for t in &mut m.prog.types {
            t.name = format!("{}{}", scope.prefix, t.name);
            for (_, fty) in &mut t.fields {
                r.ty(fty, t.pos);
            }
        }
        for f in &mut m.prog.fns {
            // Capability actions keep their names (`action termii.sms.send`): capability
            // paths are global, and the names cannot be written in a program anyway.
            if !f.name.contains(' ') {
                f.name = format!("{}{}", scope.prefix, f.name);
            }
            for p in &mut f.params {
                r.binder(&p.name, f.pos);
                r.ty(&mut p.ty, f.pos);
            }
            if let Some(t) = &mut f.ret {
                r.ty(t, f.pos);
            }
            r.block(&mut f.body);
        }
        for c in &mut m.prog.caps {
            for (_, t, pos) in &mut c.needs {
                r.ty(t, *pos);
            }
        }
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    for m in modules {
        out.types.extend(m.prog.types);
        out.fns.extend(m.prog.fns);
        out.caps.extend(m.prog.caps);
    }
    Ok(out)
}

fn modules_prefix(path: &[String]) -> String {
    format!("{}.", path.join("."))
}

struct Resolver<'s, 'a, 'd> {
    scope: &'s Scope<'a>,
    diags: &'d mut Vec<Diagnostic>,
}

impl Resolver<'_, '_, '_> {
    /// A local variable, parameter or loop variable may not reuse an import's name:
    /// `termii.send(...)` must always mean one thing.
    fn binder(&mut self, name: &str, pos: Pos) {
        if self.scope.aliases.contains_key(name) {
            self.diags.push(err(
                "E106",
                pos,
                format!("'{name}' is the name of an imported module in this file; choose another name"),
            ));
        }
    }

    /// The qualified name of a type written as `name` here.
    fn type_name(&mut self, name: &str, pos: Pos) -> String {
        if BUILTIN_TYPES.contains(&name) {
            return name.to_string();
        }
        if let Some((alias, t)) = name.split_once('.') {
            let (Some(ex), Some(p)) = (self.scope.aliases.get(alias), self.scope.alias_prefix.get(alias)) else {
                self.diags.push(err("E106", pos, format!("'{alias}' in '{name}' is not an imported module")));
                return name.to_string();
            };
            if let Some((q, public)) = ex.types.get(t) {
                if !public {
                    self.diags.push(err("E107", pos, format!("type '{name}' is not `pub` in its module")));
                }
                return q.clone();
            }
            return format!("{p}{t}");
        }
        match self.scope.types.get(name) {
            Some(q) => q.clone(),
            None => format!("{}{name}", self.scope.prefix),
        }
    }

    fn ty(&mut self, ty: &mut Ty, pos: Pos) {
        match ty {
            Ty::Named(n) => *n = self.type_name(n, pos),
            Ty::Proposed(inner) | Ty::List(inner, _) => self.ty(inner, pos),
            Ty::Cap { .. } => {}
        }
    }

    fn block(&mut self, body: &mut [Stmt]) {
        for s in body {
            match s {
                Stmt::Let { name, ty, value, pos } => {
                    self.binder(name, *pos);
                    if let Some(t) = ty {
                        self.ty(t, *pos);
                    }
                    self.expr(value);
                }
                Stmt::Set { value, .. } | Stmt::Expr(value) | Stmt::Return(Some(value), _) | Stmt::Fail(value, _) => {
                    self.expr(value)
                }
                Stmt::Return(None, _) | Stmt::Break(_) => {}
                Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } => self.block(body),
                Stmt::For { var, list, body, pos } => {
                    self.binder(var, *pos);
                    self.expr(list);
                    self.block(body);
                }
                Stmt::If { cond, then, els, .. } => {
                    self.expr(cond);
                    self.block(then);
                    self.block(els);
                }
            }
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        // `alias.f(args)` parses like an action; through an import it is a call.
        if let Expr::Action { receiver, method, args, pos } = e
            && let (Some(ex), Some(p)) = (self.scope.aliases.get(receiver.as_str()), self.scope.alias_prefix.get(receiver.as_str()))
        {
            let name = match ex.fns.get(method.as_str()) {
                Some((q, public)) => {
                    if !public {
                        self.diags.push(err(
                            "E107",
                            *pos,
                            format!("function '{receiver}.{method}' is not `pub` in its module"),
                        ));
                    }
                    q.clone()
                }
                None => format!("{p}{method}"),
            };
            let args = std::mem::take(args);
            *e = Expr::Call { name, args, pos: *pos };
        }
        match e {
            Expr::Call { name, .. } if !name.contains('.') => {
                *name = match self.scope.fns.get(name.as_str()) {
                    Some(q) => q.clone(),
                    None => format!("{}{name}", self.scope.prefix),
                };
            }
            Expr::Record { ty, pos, .. } => *ty = self.type_name(ty, *pos),
            Expr::Propose { ty, pos, .. } | Expr::Parse { ty, pos, .. } => self.ty(ty, *pos),
            _ => {}
        }
        match e {
            Expr::Var(..) | Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) | Expr::Money(..) => {}
            Expr::Field { base, .. } => self.expr(base),
            Expr::Validate(inner, _) | Expr::Try(inner, _) | Expr::Neg(inner, _) | Expr::Not(inner, _) => self.expr(inner),
            Expr::Parse { text, .. } => self.expr(text),
            Expr::Cmp { left, right, .. } | Expr::Arith { left, right, .. } | Expr::Logic { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Index { base, index, .. } => {
                self.expr(base);
                self.expr(index);
            }
            Expr::Call { args, .. }
            | Expr::Action { args, .. }
            | Expr::Builtin { args, .. }
            | Expr::List(args, _)
            | Expr::Done { evidence: args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Record { fields, .. } => {
                for (_, v) in fields {
                    self.expr(v);
                }
            }
            Expr::Propose { instruct, data, blind_to, .. } => {
                if let Some(i) = instruct {
                    self.expr(i);
                }
                for d in data.iter_mut().chain(blind_to.iter_mut()) {
                    self.expr(d);
                }
            }
        }
    }
}
