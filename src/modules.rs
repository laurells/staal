// Modules: loading the files of a program and resolving names across them.
//
// A file is a module. `import sms.termii` loads `sms/termii.staal` from the program's
// root folder (the folder of the entry file) and makes it known as `termii`;
// `import sms.termii as t` names it `t`; `import sms.termii { Message, send }` also
// brings those names in unqualified. Declarations are private unless marked `pub`.
//
// Resolution turns all modules into ONE program in which every name is qualified by
// its module: `send` in sms/termii.staal becomes `sms.termii.send`. Names in the entry
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
        let mut s = String::from("staal-modules-v2\n");
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

/// Where modules come from. `locate` says which module an `import` written in the
/// module at `from` means (the entry file is `[]`); `read` returns that module's file.
/// The default `locate` takes the written path as it is: a program without packages.
pub trait Source {
    fn locate(&mut self, from: &[String], written: &[String]) -> Result<Vec<String>, String> {
        let _ = from;
        Ok(written.to_vec())
    }
    fn read(&mut self, module: &[String]) -> Result<SourceFile, String>;
}

impl<F: FnMut(&[String]) -> Result<SourceFile, String>> Source for F {
    fn read(&mut self, module: &[String]) -> Result<SourceFile, String> {
        self(module)
    }
}

/// Loads the entry file and every module it imports, directly or indirectly.
/// `fetch` returns the file for a module path, or why it cannot.
pub fn load(entry: SourceFile, fetch: impl FnMut(&[String]) -> Result<SourceFile, String>) -> Loaded {
    load_from(entry, fetch)
}

/// Like `load`, with a `Source` that can map an import to a different module path
/// (packages: `import util` inside package `sms` means the module `sms.util`).
pub fn load_from(entry: SourceFile, mut source: impl Source) -> Loaded {
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
        let Some(mut imp) = modules[m].prog.imports.get(next).cloned() else {
            stack.pop();
            continue;
        };
        stack.last_mut().expect("non-empty").1 += 1;
        match source.locate(&modules[m].path, &imp.path) {
            Ok(canon) => {
                // Resolution and every message after this use the module's real path.
                imp.path = canon;
                modules[m].prog.imports[next].path = imp.path.clone();
            }
            Err(why) => {
                diags.push(err("E106", imp.pos, format!("cannot import '{}': {why}", imp.path.join("."))));
                continue;
            }
        }
        if let Some(&i) = index.get(&imp.path) {
            if let Some(at) = stack.iter().position(|&(s, _)| s == i) {
                let mut cycle: Vec<String> = stack[at..].iter().map(|&(s, _)| display(&modules[s].path)).collect();
                cycle.push(display(&imp.path));
                diags.push(err("E106", imp.pos, format!("import cycle: {}", cycle.join(" imports "))));
            }
            continue;
        }
        let file = match source.read(&imp.path) {
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
    consts: HashMap<String, (String, bool)>,
}

/// The names visible inside one module.
struct Scope<'a> {
    prefix: String,
    /// Unqualified type and function names: local declarations and `{ ... }` imports.
    types: HashMap<String, String>,
    fns: HashMap<String, String>,
    /// Unqualified constant names, like `fns`.
    consts: HashMap<String, String>,
    /// Import alias -> what that module exports.
    aliases: HashMap<String, &'a Exports>,
    alias_prefix: HashMap<String, String>,
    /// Every enum, and every alias of one, by qualified name: the enum it names.
    enums: &'a HashMap<String, String>,
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
            // An enum is a type: it is named, exported and imported like one. So is an
            // alias.
            for en in &m.prog.enums {
                e.types.entry(en.name.clone()).or_insert((format!("{p}{}", en.name), en.public));
            }
            for a in &m.prog.aliases {
                e.types.entry(a.name.clone()).or_insert((format!("{p}{}", a.name), a.public));
            }
            for f in &m.prog.fns {
                e.fns.entry(f.name.clone()).or_insert((format!("{p}{}", f.name), f.public));
            }
            for c in &m.prog.consts {
                e.consts.entry(c.name.clone()).or_insert((format!("{p}{}", c.name), c.public));
            }
            e
        })
        .collect();

    // Every enum by qualified name, to itself; aliases of enums are added below.
    let enums: HashMap<String, String> = modules
        .iter()
        .flat_map(|m| m.prog.enums.iter().map(move |e| format!("{}{}", m.prefix(), e.name)))
        .map(|q| (q.clone(), q))
        .collect();
    let mut out = Program::default();
    // Aliases first: what each names, in its own module, so that an alias of an enum
    // can name its variants (`State.Open` where `type State = Status`).
    let mut alias_targets: Vec<(String, Ty)> = Vec::new();
    for (mi, m) in modules.iter().enumerate() {
        let mut scratch = Vec::new();
        let scope = scope_of(mi, m, &exports, index, &enums, &mut scratch);
        let mut r = Resolver { scope: &scope, diags: &mut scratch, generics: Vec::new() };
        for a in &m.prog.aliases {
            let mut t = a.ty.clone();
            r.ty(&mut t, a.pos);
            alias_targets.push((format!("{}{}", m.prefix(), a.name), t));
        }
    }
    let mut enums = enums;
    loop {
        let more: Vec<(String, String)> = alias_targets
            .iter()
            .filter(|(q, _)| !enums.contains_key(q))
            .filter_map(|(q, t)| match t {
                Ty::Named(n) => enums.get(n).map(|e| (q.clone(), e.clone())),
                _ => None,
            })
            .collect();
        if more.is_empty() {
            break;
        }
        enums.extend(more);
    }
    for (mi, m) in modules.iter_mut().enumerate() {
        let scope = scope_of(mi, m, &exports, index, &enums, &mut diags);
        let mut r = Resolver { scope: &scope, diags: &mut diags, generics: Vec::new() };
        for t in &mut m.prog.types {
            t.name = format!("{}{}", scope.prefix, t.name);
            for (_, fty) in &mut t.fields {
                r.ty(fty, t.pos);
            }
        }
        for f in &m.prog.fns {
            // Types are already qualified here (enums are not yet).
            if let Some(t) = &f.method_of
                && !m.prog.types.iter().any(|d| d.name == format!("{}{t}", scope.prefix))
                && !m.prog.enums.iter().any(|d| &d.name == t)
            {
                r.diags.push(err(
                    "E101",
                    f.pos,
                    format!("a method is declared beside its type: '{t}' is not a record or enum declared in this file"),
                ));
            }
        }
        for f in &mut m.prog.fns {
            // Capability actions keep their names (`action termii.sms.send`): capability
            // paths are global, and the names cannot be written in a program anyway.
            if !f.name.contains(' ') {
                f.name = format!("{}{}", scope.prefix, f.name);
            }
            // A type parameter is a name of its own; it cannot also name a type here.
            for g in &f.generics {
                if BUILTIN_TYPES.contains(&g.as_str()) || r.scope.types.contains_key(g) {
                    r.diags.push(err("E101", f.pos, format!("type parameter '{g}' has the name of a type; choose another name")));
                }
            }
            r.generics = f.generics.clone();
            for p in &mut f.params {
                r.binder(&p.name, f.pos);
                r.ty(&mut p.ty, f.pos);
                if let Some(d) = &mut p.default {
                    r.expr(d);
                }
            }
            if let Some(t) = &mut f.ret {
                r.ty(t, f.pos);
            }
            if let Some(t) = &mut f.fails {
                r.ty(t, f.pos);
            }
            r.block(&mut f.body);
            r.generics.clear();
        }
        for c in &mut m.prog.caps {
            for (name, t, pos) in &mut c.needs {
                // A need is a variable inside the capability's actions.
                r.binder(name, *pos);
                r.ty(t, *pos);
            }
        }
        for en in &mut m.prog.enums {
            en.name = format!("{}{}", scope.prefix, en.name);
            for v in &mut en.variants {
                for (_, fty) in &mut v.fields {
                    r.ty(fty, v.pos);
                }
            }
        }
        for a in &mut m.prog.aliases {
            a.name = format!("{}{}", scope.prefix, a.name);
            r.ty(&mut a.ty, a.pos);
        }
        for c in &mut m.prog.consts {
            c.name = format!("{}{}", scope.prefix, c.name);
            if let Some(t) = &mut c.ty {
                r.ty(t, c.pos);
            }
            r.expr(&mut c.value);
        }
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    for m in modules {
        out.types.extend(m.prog.types);
        out.fns.extend(m.prog.fns);
        out.caps.extend(m.prog.caps);
        out.consts.extend(m.prog.consts);
        out.enums.extend(m.prog.enums);
        out.aliases.extend(m.prog.aliases);
    }
    let diags = crate::aliases::expand(&mut out);
    if !diags.is_empty() {
        return Err(diags);
    }
    let diags = crate::calls::complete(&mut out, false);
    if !diags.is_empty() {
        return Err(diags);
    }
    let diags = crate::consts::expand(&mut out);
    if !diags.is_empty() {
        return Err(diags);
    }
    Ok(out)
}

/// The names visible in module `mi`, reporting problems with its imports.
fn scope_of<'a>(
    mi: usize,
    m: &Module,
    exports: &'a [Exports],
    index: &HashMap<Vec<String>, usize>,
    enums: &'a HashMap<String, String>,
    diags: &mut Vec<Diagnostic>,
) -> Scope<'a> {
    let mut scope = Scope {
        prefix: m.prefix(),
        types: HashMap::new(),
        fns: HashMap::new(),
        consts: HashMap::new(),
        aliases: HashMap::new(),
        alias_prefix: HashMap::new(),
        enums,
    };
    for (n, (q, _)) in &exports[mi].types {
        scope.types.insert(n.clone(), q.clone());
    }
    for (n, (q, _)) in &exports[mi].fns {
        scope.fns.insert(n.clone(), q.clone());
    }
    for (n, (q, _)) in &exports[mi].consts {
        scope.consts.insert(n.clone(), q.clone());
    }
    let mut seen_consts = HashSet::new();
    for c in &m.prog.consts {
        if !seen_consts.insert(c.name.clone()) {
            diags.push(err("E101", c.pos, format!("constant '{}' is declared twice", c.name)));
        }
    }
    if !m.path.is_empty() {
        for t in &m.prog.types {
            if BUILTIN_TYPES.contains(&t.name.as_str()) {
                diags.push(err("E101", t.pos, format!("type '{}' is built in and cannot be declared", t.name)));
            }
        }
    }
    for en in &m.prog.enums {
        if BUILTIN_TYPES.contains(&en.name.as_str()) {
            diags.push(err("E101", en.pos, format!("type '{}' is built in and cannot be declared", en.name)));
        }
    }
    // An alias shares the type names of its file with records and enums.
    let mut declared: HashSet<&str> =
        m.prog.types.iter().map(|t| t.name.as_str()).chain(m.prog.enums.iter().map(|e| e.name.as_str())).collect();
    for a in &m.prog.aliases {
        if BUILTIN_TYPES.contains(&a.name.as_str()) {
            diags.push(err("E101", a.pos, format!("type '{}' is built in and cannot be declared", a.name)));
        } else if !declared.insert(a.name.as_str()) {
            diags.push(err("E101", a.pos, format!("type '{}' is already defined", a.name)));
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
        if exports[mi].types.contains_key(&imp.alias)
            || exports[mi].fns.contains_key(&imp.alias)
            || exports[mi].consts.contains_key(&imp.alias)
        {
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
            let c = tex.consts.get(n);
            if ty.is_none() && f.is_none() && c.is_none() {
                diags.push(err("E100", *npos, format!("module '{}' has no '{n}'", imp.path.join("."))));
                continue;
            }
            if ty.is_some_and(|t| !t.1) || f.is_some_and(|f| !f.1) || c.is_some_and(|c| !c.1) {
                diags.push(err(
                    "E107",
                    *npos,
                    format!("'{n}' in module '{}' is not `pub`, so it cannot be imported", imp.path.join(".")),
                ));
                continue;
            }
            // A name may come from one place only: this file, or one import.
            let local_clash = (ty.is_some() && exports[mi].types.contains_key(n))
                || (f.is_some() && exports[mi].fns.contains_key(n))
                || (c.is_some() && exports[mi].consts.contains_key(n));
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
            if let Some((q, _)) = c {
                scope.consts.insert(n.clone(), q.clone());
            }
        }
    }
    scope
}

fn modules_prefix(path: &[String]) -> String {
    format!("{}.", path.join("."))
}

struct Resolver<'s, 'a, 'd> {
    scope: &'s Scope<'a>,
    diags: &'d mut Vec<Diagnostic>,
    /// The type parameters of the function being resolved: names, not types to qualify.
    generics: Vec<String>,
}

impl Resolver<'_, '_, '_> {
    /// A local variable, parameter or loop variable may not reuse an import's name:
    /// `termii.send(...)` must always mean one thing.
    fn binder(&mut self, name: &str, pos: Pos) {
        if name == "some" || name == "none" {
            self.diags.push(err("E101", pos, format!("'{name}' is reserved for optional values; choose another name")));
        }
        // `Status.Pending` must always mean the enum's variant, never a field of a variable.
        if self.scope.types.get(name).is_some_and(|q| self.scope.enums.contains_key(q)) {
            self.diags.push(err("E101", pos, format!("'{name}' is an enum here; choose another name")));
        }
        if self.scope.consts.contains_key(name) {
            self.diags.push(err("E101", pos, format!("'{name}' is a constant here; choose another name")));
        }
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
            Ty::Named(n) if self.generics.contains(n) => {}
            Ty::Named(n) => *n = self.type_name(n, pos),
            Ty::Proposed(inner) | Ty::List(inner, _) | Ty::Optional(inner) => self.ty(inner, pos),
            Ty::Tuple(items) => {
                for i in items {
                    self.ty(i, pos);
                }
            }
            Ty::Map(k, v, _) => {
                self.ty(k, pos);
                self.ty(v, pos);
            }
            Ty::Set(t, _) => self.ty(t, pos),
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
                Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => {}
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
                Stmt::Match { value, arms, .. } => {
                    self.expr(value);
                    for a in arms {
                        match &a.pattern {
                            Pattern::Variant { binds, pos, .. } => {
                                for (_, to) in binds {
                                    self.binder(to, *pos);
                                }
                            }
                            Pattern::Present { bind, pos } => self.binder(bind, *pos),
                            Pattern::Absent(_) | Pattern::Any(_) => {}
                        }
                        self.block(&mut a.body);
                    }
                }
            }
        }
    }

    /// The qualified enum a name written here refers to: `Status` (this file or an
    /// import by name) or `m.Status` (through an import).
    fn enum_named(&mut self, written: &[&str], pos: Pos) -> Option<String> {
        let q = match written {
            [name] => self.scope.types.get(*name)?.clone(),
            [alias, name] => {
                let ex = self.scope.aliases.get(*alias)?;
                let (q, public) = ex.types.get(*name)?;
                if self.scope.enums.contains_key(q) && !public {
                    self.diags.push(err("E107", pos, format!("enum '{alias}.{name}' is not `pub` in its module")));
                }
                q.clone()
            }
            _ => return None,
        };
        self.scope.enums.get(&q).cloned()
    }

    fn expr(&mut self, e: &mut Expr) {
        // A record literal or field read whose type part names an enum is a variant:
        // `Status.Paid { ... }`, `m.Status.Paid { ... }`, `Status.Pending`.
        let variant = match e {
            Expr::Record { ty, fields, pos } => {
                let parts: Vec<&str> = ty.split('.').collect();
                match parts.split_last() {
                    Some((v, head)) if !head.is_empty() => {
                        let (v, pos) = (v.to_string(), *pos);
                        self.enum_named(head, pos).map(|q| (q, v, std::mem::take(fields), pos))
                    }
                    _ => None,
                }
            }
            Expr::Field { base, field, pos } => {
                let head: Option<Vec<String>> = match base.as_ref() {
                    Expr::Var(n, _) => Some(vec![n.clone()]),
                    Expr::Field { base: b2, field: f2, .. } => match b2.as_ref() {
                        Expr::Var(n, _) => Some(vec![n.clone(), f2.clone()]),
                        _ => None,
                    },
                    _ => None,
                };
                let (field, pos) = (field.clone(), *pos);
                head.and_then(|h| {
                    let h: Vec<&str> = h.iter().map(String::as_str).collect();
                    self.enum_named(&h, pos)
                })
                .map(|q| (q, field, Vec::new(), pos))
            }
            _ => None,
        };
        if let Some((ty, variant, fields, pos)) = variant {
            *e = Expr::Variant { ty, variant, fields, pos };
        }
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
            *e = Expr::Call { name, args, slots: Vec::new(), pos: *pos };
        }
        // `alias.NAME` is a constant of an imported module.
        if let Expr::Field { base, field, pos } = e
            && let Expr::Var(alias, _) = base.as_ref()
            && let Some(ex) = self.scope.aliases.get(alias.as_str())
            && let Some((q, public)) = ex.consts.get(field.as_str())
        {
            if !public {
                self.diags.push(err("E107", *pos, format!("constant '{alias}.{field}' is not `pub` in its module")));
            }
            *e = Expr::Var(q.clone(), *pos);
            return;
        }
        match e {
            Expr::Var(name, _) if self.scope.consts.contains_key(name.as_str()) => {
                *name = self.scope.consts[name.as_str()].clone();
            }
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
            Expr::Var(..) | Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) | Expr::Money(..) | Expr::NoneLit(..) => {}
            Expr::Field { base, .. } => self.expr(base),
            Expr::Validate(inner, _)
            | Expr::Try(inner, _)
            | Expr::Neg(inner, _)
            | Expr::Not(inner, _)
            | Expr::SomeOf(inner, _) => self.expr(inner),
            Expr::Named { value, .. } => self.expr(value),
            Expr::Method { base, args, .. } => {
                self.expr(base);
                for a in args {
                    self.expr(a);
                }
            }
            Expr::MapLit(entries, _) => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
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
            | Expr::Tuple(args, _)
            | Expr::Done { evidence: args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Record { fields, .. } | Expr::Variant { fields, .. } => {
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
