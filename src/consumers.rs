// The consumers of a program's grants (AGENT_MANIFEST.md sections 1 and 5, R3): for
// every capability a run is granted, each part of the program that can come to hold it,
// and the boundary that part is held to.
//
// A grant is an entry parameter of capability type, or a need of a library capability
// granted to one (`p.post`), or a need of that need, and so on. Its consumers are:
//
//   agent         the program's own code, holding the entry parameter: bounded by the
//                 entry parameter's type;
//   package       a parameter of a package's function that the grant can be passed to
//                 from outside that package (from the program, or from another
//                 package): bounded by that parameter's type;
//   library_need  the body of the library capability whose need it is: bounded by the
//                 need's type.
//
// A slot's type is the whole boundary of what arrives in it. The checker allows a
// capability into a slot only if the slot's type keeps every limit the capability's own
// type has, at least as strict (E003), and the runtime narrows what arrives to the
// slot's limits, as a child counted against every ancestor (RUNTIME.md section 6). So
// each dimension (`each`, `max`, `uses`, `only`) the slot's type leaves without a limit
// had none in any type the capability passed through; only the operator's grant bounds
// it there. `max` and `uses` are counted from each arrival in a slot, and also against
// every limit the capability passed through.
//
// How the analysis follows a capability. In Staal a capability can be held only by a
// parameter, a `let`, a return value or a library capability's need: no list, record,
// tuple, optional, map or function value can hold one, and a variable that holds one is
// never reassigned (E101, E003). So it follows exactly these: each call passes its
// capability arguments to the callee's parameters, each `let` binds what its value
// holds, and each call's result holds what the callee can return. A function is
// analysed once per assignment of grants to its capability parameters (context
// sensitive), and returns are solved to a fixed point, so recursion is followed too.
// Branches are not evaluated: a consumer is listed when some path through the program
// takes the grant there, whether or not any input makes the program take that path. A
// listed consumer may be unreachable for every input; an unlisted one is never reachable.
//
// Anything the analysis does not follow, it reports as unresolved instead of guessing:
// a capability reaching a slot through an expression that is neither a variable nor a
// call, a capability passed to an action, a library capability whose needs form a
// cycle, or more contexts than `MAX_CONTEXTS`. With anything unresolved the analysis is
// incomplete, and says where and why.

use crate::ast::{CapLimits, Expr, FnDecl, Program, Stmt, Ty};
use crate::lexer::Pos;
use serde_json::{json, Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The most function contexts analysed before the analysis gives up as incomplete.
pub const MAX_CONTEXTS: usize = 100_000;

/// Who holds a grant. Ordered as reported: the agent, then a library capability's
/// body, then the package slots each hands it to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Holder {
    Agent,
    LibraryNeed { capability: String, need: String, package: Option<String> },
    Package { package: String, function: String, parameter: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Consumer {
    /// The grant: an entry parameter's name, or `<grant>.<need>`.
    pub grant: String,
    /// The capability path, as written in the type.
    pub cap: String,
    pub holder: Holder,
    pub limits: Limits,
}

/// A type's limits in a form that can be ordered and compared.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Limits {
    pub max: Option<(u64, Option<String>)>,
    pub each: Option<(u64, Option<String>)>,
    pub uses: Option<u64>,
    pub only: Option<Vec<String>>,
}

impl Limits {
    pub fn of(l: &CapLimits) -> Limits {
        Limits {
            max: l.max.as_ref().map(|m| (m.amount, m.unit.clone())),
            each: l.each.as_ref().map(|m| (m.amount, m.unit.clone())),
            uses: l.uses,
            only: l.only.clone(),
        }
    }

    /// `{"max": money, "each": money, "uses": n, "only": [...]}`, each only if set. A
    /// money limit is written as money is everywhere (`{"amount": "500.00", "currency":
    /// "USD"}`), with its currency's decimal places.
    pub fn to_json(&self) -> Result<Json, String> {
        let money = |name: &str, (amount, unit): &(u64, Option<String>)| -> Result<Json, String> {
            let unit = unit.as_deref().ok_or_else(|| format!("the limit `{name}: {amount}` has no currency"))?;
            let currency = crate::interp::money::Currency::parse(unit)?;
            let m = crate::interp::money::Money::from_major(*amount, currency)?;
            Ok(json!({ "amount": m.amount_string(), "currency": m.currency.code }))
        };
        let mut o = Map::new();
        if let Some(m) = &self.max {
            o.insert("max".into(), money("max", m)?);
        }
        if let Some(e) = &self.each {
            o.insert("each".into(), money("each", e)?);
        }
        if let Some(u) = self.uses {
            o.insert("uses".into(), json!(u));
        }
        if let Some(only) = &self.only {
            o.insert("only".into(), json!(only));
        }
        Ok(Json::Object(o))
    }
}

/// Something the analysis could not follow.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Unresolved {
    pub grant: Option<String>,
    pub pos: Option<Pos>,
    pub why: String,
}

#[derive(Debug, Default)]
pub struct Analysis {
    pub consumers: BTreeSet<Consumer>,
    pub unresolved: BTreeSet<Unresolved>,
}

/// A function analysed with each capability parameter holding one grant (None for a
/// parameter that is not a capability, or whose grant is unknown).
type Context = (String, Vec<Option<String>>);

struct Analyzer<'a> {
    fns: HashMap<&'a str, &'a FnDecl>,
    /// The package each file belongs to (None for the program's own files).
    package_of_file: &'a [Option<String>],
    /// The grants each context can return, as far as solved.
    returns: HashMap<Context, BTreeSet<String>>,
    in_progress: HashSet<Context>,
    changed: bool,
    out: Analysis,
}

/// What each variable holds, in nested scopes: the grants of a capability, or None for
/// a variable that is not one (so that it hides an outer one of the same name).
struct Env {
    scopes: Vec<HashMap<String, Option<BTreeSet<String>>>>,
}

impl Env {
    fn get(&self, name: &str) -> Option<&BTreeSet<String>> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).and_then(Option::as_ref)
    }

    fn bind(&mut self, name: &str, grants: Option<BTreeSet<String>>) {
        self.scopes.last_mut().expect("a scope").insert(name.to_string(), grants);
    }
}

impl<'a> Analyzer<'a> {
    fn package_of(&self, pos: Pos) -> Option<String> {
        self.package_of_file.get(pos.file as usize).cloned().flatten()
    }

    fn unresolved(&mut self, grant: Option<&str>, pos: Option<Pos>, why: String) {
        self.out.unresolved.insert(Unresolved { grant: grant.map(str::to_string), pos, why });
    }

    /// The grants a capability-typed expression holds, or None if it is not one the
    /// analysis follows. A call's flows are recorded as it is analysed.
    fn grants_of(&mut self, e: &Expr, env: &Env, caller: &FnDecl) -> Option<BTreeSet<String>> {
        match e {
            Expr::Var(n, _) => env.get(n).cloned(),
            Expr::Call { .. } => self.call(e, env, caller),
            _ => None,
        }
    }

    /// Follows a call: its capability arguments into the callee, and back what it
    /// returns. Returns the grants of its result if the callee returns a capability.
    fn call(&mut self, e: &Expr, env: &Env, caller: &FnDecl) -> Option<BTreeSet<String>> {
        let Expr::Call { name, args, slots, pos } = e else { return None };
        // The arguments' own calls come first, as they are evaluated.
        let mut arg_grants: Vec<Option<BTreeSet<String>>> = Vec::with_capacity(args.len());
        for a in args {
            arg_grants.push(match a {
                Expr::Var(..) | Expr::Call { .. } => self.grants_of(a, env, caller),
                other => {
                    self.walk_expr(other, env, caller);
                    None
                }
            });
        }
        let Some(callee) = self.fns.get(name.as_str()).copied() else {
            // A built-in or a name the checker resolved elsewhere: no capability
            // parameter to follow. A capability argument to it is not followed.
            for (a, g) in args.iter().zip(&arg_grants) {
                if let Some(g) = g {
                    for grant in g {
                        self.unresolved(Some(grant), Some(a.pos()), format!("a capability is passed to '{name}', which is not a function of the program"));
                    }
                }
            }
            return None;
        };
        let caller_package = self.package_of(caller.pos);
        let callee_package = self.package_of(callee.pos);
        // Each capability parameter's possible grants.
        let mut per_param: Vec<Option<BTreeSet<String>>> = vec![None; callee.params.len()];
        for (i, a) in args.iter().enumerate() {
            let p = if slots.is_empty() { i } else { slots[i] };
            let Some(param) = callee.params.get(p) else { continue };
            let Ty::Cap { path, limits } = &param.ty else {
                if let Some(g) = &arg_grants[i] {
                    for grant in g {
                        self.unresolved(Some(grant), Some(a.pos()), format!("a capability is passed to parameter '{}' of '{name}', which is not a capability", param.name));
                    }
                }
                continue;
            };
            match &arg_grants[i] {
                None => self.unresolved(
                    None,
                    Some(a.pos()),
                    format!("a capability reaches parameter '{}' of '{name}' through an expression the analysis does not follow", param.name),
                ),
                Some(g) => {
                    if let Some(pkg) = &callee_package
                        && caller_package.as_ref() != Some(pkg)
                    {
                        for grant in g {
                            self.out.consumers.insert(Consumer {
                                grant: grant.clone(),
                                cap: path.join("."),
                                holder: Holder::Package { package: pkg.clone(), function: name.clone(), parameter: param.name.clone() },
                                limits: Limits::of(limits),
                            });
                        }
                    }
                    per_param[p] = Some(g.clone());
                }
            }
        }
        // A default can never be a capability (constants cannot hold one), so every
        // capability parameter is now known or reported.
        let mut contexts: Vec<Vec<Option<String>>> = vec![Vec::new()];
        for g in &per_param {
            let mut next = Vec::new();
            for c in &contexts {
                match g {
                    None => {
                        let mut c = c.clone();
                        c.push(None);
                        next.push(c);
                    }
                    Some(set) => {
                        for grant in set {
                            let mut c = c.clone();
                            c.push(Some(grant.clone()));
                            next.push(c);
                        }
                    }
                }
            }
            contexts = next;
        }
        let mut result = BTreeSet::new();
        for binds in contexts {
            result.extend(self.analyse((name.clone(), binds), *pos));
        }
        matches!(callee.ret, Some(Ty::Cap { .. })).then_some(result)
    }

    /// The grants `ctx` can return, analysing its body if it is not being analysed.
    fn analyse(&mut self, ctx: Context, at: Pos) -> BTreeSet<String> {
        if self.in_progress.contains(&ctx) {
            return self.returns.get(&ctx).cloned().unwrap_or_default();
        }
        if !self.returns.contains_key(&ctx) && self.returns.len() >= MAX_CONTEXTS {
            self.unresolved(None, Some(at), format!("more than {MAX_CONTEXTS} function contexts; the analysis stopped"));
            return BTreeSet::new();
        }
        let Some(f) = self.fns.get(ctx.0.as_str()).copied() else { return BTreeSet::new() };
        self.in_progress.insert(ctx.clone());
        let mut env = Env { scopes: vec![HashMap::new()] };
        for (p, g) in f.params.iter().zip(&ctx.1) {
            env.bind(&p.name, g.as_ref().map(|g| BTreeSet::from([g.clone()])));
        }
        let mut returned = BTreeSet::new();
        self.walk_block(&f.body, &mut env, f, &mut returned);
        self.in_progress.remove(&ctx);
        let entry = self.returns.entry(ctx).or_default();
        let before = entry.len();
        entry.extend(returned);
        if entry.len() != before {
            self.changed = true;
        }
        entry.clone()
    }

    fn walk_block(&mut self, body: &[Stmt], env: &mut Env, f: &FnDecl, returned: &mut BTreeSet<String>) {
        env.scopes.push(HashMap::new());
        for s in body {
            match s {
                Stmt::Let { name, ty, value, pos } => {
                    let g = match value {
                        Expr::Var(..) | Expr::Call { .. } => self.grants_of(value, env, f),
                        other => {
                            self.walk_expr(other, env, f);
                            None
                        }
                    };
                    if g.is_none() && matches!(ty, Some(Ty::Cap { .. })) {
                        self.unresolved(None, Some(*pos), format!("`let {name}` holds a capability the analysis does not follow"));
                    }
                    env.bind(name, g);
                }
                Stmt::Return(Some(e), pos) => {
                    if matches!(f.ret, Some(Ty::Cap { .. })) {
                        match e {
                            Expr::Var(..) | Expr::Call { .. } => match self.grants_of(e, env, f) {
                                Some(g) => returned.extend(g),
                                None => self.unresolved(None, Some(*pos), format!("'{}' returns a capability the analysis does not follow", f.name)),
                            },
                            other => {
                                self.walk_expr(other, env, f);
                                self.unresolved(None, Some(*pos), format!("'{}' returns a capability the analysis does not follow", f.name));
                            }
                        }
                    } else {
                        self.walk_expr(e, env, f);
                    }
                }
                // The names a loop or a match binds hold data, never a capability (no
                // list or enum can hold one), and hide any outer name.
                Stmt::For { var, list, body, .. } => {
                    self.walk_expr(list, env, f);
                    env.scopes.push(HashMap::from([(var.clone(), None)]));
                    self.walk_block(body, env, f, returned);
                    env.scopes.pop();
                }
                Stmt::Match { value, arms, .. } => {
                    self.walk_expr(value, env, f);
                    for arm in arms {
                        let names: Vec<String> = match &arm.pattern {
                            crate::ast::Pattern::Variant { binds, .. } => binds.iter().map(|(_, n)| n.clone()).collect(),
                            crate::ast::Pattern::Present { bind, .. } => vec![bind.clone()],
                            crate::ast::Pattern::Absent(_) | crate::ast::Pattern::Any(_) => Vec::new(),
                        };
                        env.scopes.push(names.into_iter().map(|n| (n, None)).collect());
                        self.walk_block(&arm.body, env, f, returned);
                        env.scopes.pop();
                    }
                }
                other => {
                    for e in other.exprs() {
                        self.walk_expr(e, env, f);
                    }
                    for b in other.blocks() {
                        self.walk_block(b, env, f, returned);
                    }
                }
            }
        }
        env.scopes.pop();
    }

    /// Walks an expression that does not itself give a capability, following every call
    /// inside it.
    fn walk_expr(&mut self, e: &Expr, env: &Env, f: &FnDecl) {
        match e {
            Expr::Call { .. } => {
                self.call(e, env, f);
            }
            Expr::Action { receiver, args, pos, .. } => {
                for a in args {
                    if let Some(g) = self.grants_of(a, env, f) {
                        for grant in g {
                            self.unresolved(Some(&grant), Some(*pos), format!("a capability is passed to an action through '{receiver}'"));
                        }
                    } else {
                        self.walk_expr(a, env, f);
                    }
                }
            }
            other => {
                for c in other.children() {
                    self.walk_expr(c, env, f);
                }
            }
        }
    }
}

/// A library capability's action, by its path (`bank.pay.send`): the capability's
/// path, its declaration, and the action's function.
fn library_action<'a>(prog: &'a Program, path: &str) -> Option<(&'a crate::ast::CapDecl, &'a crate::ast::ActionDecl)> {
    prog.caps.iter().find_map(|c| c.actions.iter().find(|a| c.action_path(&a.name) == path).map(|a| (c, a)))
}

/// The consumers of every grant of `entry`. `package_of_file` gives, for each file of
/// the program (by `Pos::file`), the package it belongs to.
pub fn analyse(prog: &Program, entry: &str, package_of_file: &[Option<String>]) -> Result<Analysis, String> {
    let f = prog.fns.iter().find(|f| f.name == entry).ok_or_else(|| format!("no function '{entry}'"))?;
    let mut a = Analyzer {
        fns: prog.fns.iter().map(|f| (f.name.as_str(), f)).collect(),
        package_of_file,
        returns: HashMap::new(),
        in_progress: HashSet::new(),
        changed: false,
        out: Analysis::default(),
    };
    // The roots: the entry function with its capability parameters, and the action of
    // each library capability granted, with its needs, recursively.
    let mut roots: Vec<(Context, Pos)> = vec![(
        (f.name.clone(), f.params.iter().map(|p| matches!(p.ty, Ty::Cap { .. }).then(|| p.name.clone())).collect()),
        f.pos,
    )];
    let mut queue: Vec<(String, String, Vec<String>)> = Vec::new();
    for p in &f.params {
        if let Ty::Cap { path, limits } = &p.ty {
            a.out.consumers.insert(Consumer { grant: p.name.clone(), cap: path.join("."), holder: Holder::Agent, limits: Limits::of(limits) });
            queue.push((p.name.clone(), path.join("."), vec![path.join(".")]));
        }
    }
    while let Some((grant, path, chain)) = queue.pop() {
        let Some((cap, action)) = library_action(prog, &path) else { continue };
        let Some(afn) = prog.fns.iter().find(|x| x.name == action.fn_name) else {
            return Err(format!("the action {path} has no function"));
        };
        let package = a.package_of(cap.pos);
        let mut binds: Vec<Option<String>> = vec![None; afn.params.len()];
        for (need, ty, _) in &cap.needs {
            let Ty::Cap { path: npath, limits } = ty else { continue };
            let ngrant = format!("{grant}.{need}");
            let shown = npath.join(".");
            a.out.consumers.insert(Consumer {
                grant: ngrant.clone(),
                cap: shown.clone(),
                holder: Holder::LibraryNeed { capability: cap.path.join("."), need: need.clone(), package: package.clone() },
                limits: Limits::of(limits),
            });
            if let Some(i) = afn.params.iter().skip(action.declared).position(|p| &p.name == need) {
                binds[action.declared + i] = Some(ngrant.clone());
            }
            if chain.contains(&shown) {
                a.unresolved(Some(&ngrant), Some(cap.pos), format!("the needs of {} lead back to {shown}", cap.path.join(".")));
                continue;
            }
            let mut next = chain.clone();
            next.push(shown.clone());
            queue.push((ngrant, shown, next));
        }
        roots.push(((afn.name.clone(), binds), afn.pos));
    }
    // Returns are solved to a fixed point: each pass can only add grants to a context's
    // returns, and those are bounded by the grants, so the passes end.
    loop {
        a.changed = false;
        for (ctx, pos) in &roots {
            a.analyse(ctx.clone(), *pos);
        }
        if !a.changed {
            break;
        }
    }
    Ok(a.out)
}

/// The analysis as `staalc schema` reports it: `consumers`, one record per grant and
/// holder, and `consumer_analysis`, whether it is complete and, if not, what is
/// unresolved. `files` names each file for positions.
pub fn describe(analysis: &Analysis, files: &[crate::SourceFile]) -> Result<Json, String> {
    let mut consumers = Vec::new();
    // Grouped by grant, in the order of the grants' names.
    let mut by_grant: BTreeMap<&str, Vec<&Consumer>> = BTreeMap::new();
    for c in &analysis.consumers {
        by_grant.entry(c.grant.as_str()).or_default().push(c);
    }
    for list in by_grant.values() {
        for c in list {
            let (holder, source) = match &c.holder {
                Holder::Agent => (json!({ "kind": "agent" }), format!("the type of entry parameter '{}'", c.grant)),
                Holder::Package { package, function, parameter } => (
                    json!({ "kind": "package", "package": package, "function": function, "parameter": parameter }),
                    format!("the type of parameter '{parameter}' of {function}"),
                ),
                Holder::LibraryNeed { capability, need, package } => (
                    json!({ "kind": "library_need", "capability": capability, "need": need, "package": package }),
                    format!("the type of need '{need}' of capability {capability}"),
                ),
            };
            consumers.push(json!({ "grant": c.grant, "cap": c.cap, "holder": holder, "limits": c.limits.to_json()?, "source": source }));
        }
    }
    let unresolved: Vec<Json> = analysis
        .unresolved
        .iter()
        .map(|u| {
            let at = u.pos.map(|p| {
                let name = files.get(p.file as usize).map(|f| f.name.as_str()).unwrap_or("?");
                format!("{name}:{}:{}", p.line, p.col)
            });
            json!({ "grant": u.grant, "at": at, "why": u.why })
        })
        .collect();
    Ok(json!({
        "consumers": consumers,
        "consumer_analysis": { "complete": unresolved.is_empty(), "unresolved": unresolved },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program the checker accepts never moves a capability in a way the analysis does
    /// not follow (tests/consumers_test.rs). Here one is made by hand, by changing a
    /// checked program: a capability argument given through an expression the analysis
    /// does not follow. It is reported, the analysis is incomplete, and the callee is
    /// still analysed, with that parameter's grant unknown.
    #[test]
    fn what_is_not_followed_is_reported_never_guessed() {
        let mut prog = crate::check_program(
            "fn helper(h: cap net.http.post) -> Int {\n    return 1\n}\n\nfn go(h: cap net.http.post) -> Int {\n    return helper(h)\n}\n",
        )
        .unwrap();
        let go = prog.fns.iter_mut().find(|f| f.name == "go").unwrap();
        let Stmt::Return(Some(Expr::Call { args, .. }), _) = &mut go.body[0] else { panic!("{:?}", go.body) };
        let pos = args[0].pos();
        args[0] = Expr::Try(Box::new(Expr::Var("h".into(), pos)), pos);
        let a = analyse(&prog, "go", &[None]).unwrap();
        assert_eq!(a.unresolved.len(), 1, "{:?}", a.unresolved);
        assert!(a.unresolved.iter().next().unwrap().why.contains("does not follow"));
        let d = describe(&a, &[crate::SourceFile { name: "p.staal".into(), text: String::new() }]).unwrap();
        assert_eq!(d["consumer_analysis"]["complete"], false);
        assert_eq!(d["consumer_analysis"]["unresolved"][0]["at"], "p.staal:6:19");
    }
}
