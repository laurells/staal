// Cost analysis. Enforces:
//   E004  a function's worst-case model use can exceed its declared `budget(...)`
//   E005  a model call (direct or through a called function) sits inside an unbounded `loop`
//
// Cost is counted in two dimensions: model calls and tokens. Every quantity is
// either a finite worst case or Unbounded. The analysis is modular: a function
// that declares a budget is costed by its declaration (its own body is checked
// against that declaration separately); a function without one is costed by
// walking its body. Recursion that involves any model cost is Unbounded.

use crate::ast::*;
use crate::checker::Diagnostic;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bound {
    Fin(u64),
    Unbounded,
}

impl Bound {
    fn add(self, o: Bound) -> Bound {
        match (self, o) {
            (Bound::Fin(a), Bound::Fin(b)) => a.checked_add(b).map_or(Bound::Unbounded, Bound::Fin),
            _ => Bound::Unbounded,
        }
    }
    fn times(self, n: u64) -> Bound {
        match self {
            Bound::Fin(a) => a.checked_mul(n).map_or(Bound::Unbounded, Bound::Fin),
            Bound::Unbounded if n == 0 => Bound::Fin(0),
            Bound::Unbounded => Bound::Unbounded,
        }
    }
    /// Repeat without limit: zero stays zero, anything else becomes Unbounded.
    fn forever(self) -> Bound {
        if self == Bound::Fin(0) {
            Bound::Fin(0)
        } else {
            Bound::Unbounded
        }
    }
    fn is_zero(self) -> bool {
        self == Bound::Fin(0)
    }
    fn max(self, o: Bound) -> Bound {
        match (self, o) {
            (Bound::Fin(a), Bound::Fin(b)) => Bound::Fin(a.max(b)),
            _ => Bound::Unbounded,
        }
    }
}

impl std::fmt::Display for Bound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Bound::Fin(n) => write!(f, "{n}"),
            Bound::Unbounded => write!(f, "unbounded"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost {
    pub calls: Bound,
    pub tokens: Bound,
}

impl Cost {
    const ZERO: Cost = Cost { calls: Bound::Fin(0), tokens: Bound::Fin(0) };
    fn add(self, o: Cost) -> Cost {
        Cost { calls: self.calls.add(o.calls), tokens: self.tokens.add(o.tokens) }
    }
    fn times(self, n: u64) -> Cost {
        Cost { calls: self.calls.times(n), tokens: self.tokens.times(n) }
    }
    fn forever(self) -> Cost {
        Cost { calls: self.calls.forever(), tokens: self.tokens.forever() }
    }
    fn is_zero(self) -> bool {
        self.calls.is_zero() && self.tokens.is_zero()
    }
    /// Worst case of two alternatives (the two branches of an `if`).
    fn max(self, o: Cost) -> Cost {
        Cost { calls: self.calls.max(o.calls), tokens: self.tokens.max(o.tokens) }
    }
}

struct Analyzer<'a> {
    fns: HashMap<&'a str, &'a FnDecl>,
    /// Inferred costs of functions without a budget, once fully resolved.
    memo: HashMap<&'a str, Cost>,
}

pub fn check(prog: &Program) -> Vec<Diagnostic> {
    let mut a = Analyzer { fns: HashMap::new(), memo: HashMap::new() };
    for f in &prog.fns {
        a.fns.entry(f.name.as_str()).or_insert(f);
    }
    let mut diags = Vec::new();
    for f in &prog.fns {
        let mut stack = vec![f.name.as_str()];
        let mut cycles = HashSet::new();
        let cost = a.block(&f.body, &mut stack, &mut cycles, Some(&mut diags));
        // A function whose own body re-enters itself through recursion has no worst case.
        let cost = if cycles.is_empty() || cost.is_zero() { cost } else { cost.forever() };
        if let Some(b) = &f.budget {
            check_dim(&mut diags, f, b, "calls", b.calls, cost.calls);
            check_dim(&mut diags, f, b, "tokens", b.tokens, cost.tokens);
        }
    }
    diags
}

fn check_dim(diags: &mut Vec<Diagnostic>, f: &FnDecl, b: &Budget, dim: &str, declared: Option<u64>, used: Bound) {
    let Some(limit) = declared else { return };
    let over = match used {
        Bound::Fin(n) => n > limit,
        Bound::Unbounded => true,
    };
    if !over {
        return;
    }
    let msg = match used {
        Bound::Unbounded => format!(
            "function '{}' declares budget {dim}: {limit} but its {dim} use is unbounded \
             (a model call without a `tokens:` limit, an unbounded `loop`, or recursion)",
            f.name
        ),
        Bound::Fin(n) => {
            format!("function '{}' declares budget {dim}: {limit} but can use up to {n} {dim}", f.name)
        }
    };
    diags.push(Diagnostic { code: "E004", pos: b.pos, msg });
}

impl<'a> Analyzer<'a> {
    /// Worst-case cost of one call to `name`.
    fn fn_cost(&mut self, name: &str, stack: &mut Vec<&'a str>, cycles: &mut HashSet<&'a str>) -> Cost {
        let Some(&f) = self.fns.get(name) else { return Cost::ZERO }; // undefined: reported by the checker
        if let Some(b) = &f.budget {
            // Trust the declaration; the function's own body is checked against it.
            return Cost {
                calls: b.calls.map_or(Bound::Unbounded, Bound::Fin),
                tokens: b.tokens.map_or(Bound::Unbounded, Bound::Fin),
            };
        }
        if let Some(c) = self.memo.get(f.name.as_str()) {
            return *c;
        }
        if stack.contains(&f.name.as_str()) {
            cycles.insert(f.name.as_str());
            return Cost::ZERO; // provisional; the cycle is resolved where it started
        }
        stack.push(f.name.as_str());
        let mut inner = HashSet::new();
        let mut cost = self.block(&f.body, stack, &mut inner, None);
        stack.pop();
        if !inner.is_empty() && !cost.is_zero() {
            cost = cost.forever();
        }
        inner.remove(f.name.as_str());
        if inner.is_empty() {
            self.memo.insert(f.name.as_str(), cost);
        } else {
            cycles.extend(inner); // still inside an enclosing cycle
        }
        cost
    }

    /// `diags` is Some only for the function currently being checked, so E005 is
    /// reported once, at the loop's own location, not again for every caller.
    fn block(
        &mut self,
        body: &[Stmt],
        stack: &mut Vec<&'a str>,
        cycles: &mut HashSet<&'a str>,
        mut diags: Option<&mut Vec<Diagnostic>>,
    ) -> Cost {
        let mut total = Cost::ZERO;
        for stmt in body {
            let c = match stmt {
                Stmt::Let { value, .. } => self.expr(value, stack, cycles),
                Stmt::Expr(e) | Stmt::Return(Some(e), _) => self.expr(e, stack, cycles),
                Stmt::Return(None, _) => Cost::ZERO,
                Stmt::Repeat { count, body, .. } => {
                    self.block(body, stack, cycles, diags.as_deref_mut()).times(*count)
                }
                Stmt::Break(_) => Cost::ZERO,
                Stmt::If { cond, then, els, .. } => {
                    let c = self.expr(cond, stack, cycles);
                    let t = self.block(then, stack, cycles, diags.as_deref_mut());
                    let e = self.block(els, stack, cycles, diags.as_deref_mut());
                    c.add(t.max(e))
                }
                Stmt::Loop { body, pos } => {
                    let c = self.block(body, stack, cycles, diags.as_deref_mut());
                    if !c.calls.is_zero() {
                        if let Some(d) = diags.as_deref_mut() {
                            d.push(Diagnostic {
                                code: "E005",
                                pos: *pos,
                                msg: "model call inside an unbounded `loop`; use `repeat N { ... }` to bound it"
                                    .into(),
                            });
                        }
                    }
                    c.forever()
                }
            };
            total = total.add(c);
        }
        total
    }

    fn expr(&mut self, e: &Expr, stack: &mut Vec<&'a str>, cycles: &mut HashSet<&'a str>) -> Cost {
        match e {
            Expr::Var(..) | Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) => Cost::ZERO,
            Expr::Done { evidence, .. } => {
                evidence.iter().fold(Cost::ZERO, |acc, e| acc.add(self.expr(e, stack, cycles)))
            }
            Expr::Cmp { left, right, .. } => self.expr(left, stack, cycles).add(self.expr(right, stack, cycles)),
            Expr::Propose { tokens, instruct, data, .. } => {
                let mut c = Cost { calls: Bound::Fin(1), tokens: tokens.map_or(Bound::Unbounded, Bound::Fin) };
                if let Some(i) = instruct {
                    c = c.add(self.expr(i, stack, cycles));
                }
                for d in data {
                    c = c.add(self.expr(d, stack, cycles));
                }
                c
            }
            Expr::Validate(inner, _) | Expr::Field { base: inner, .. } | Expr::Try(inner, _) => {
                self.expr(inner, stack, cycles)
            }
            Expr::Action { args, .. } => {
                args.iter().fold(Cost::ZERO, |acc, a| acc.add(self.expr(a, stack, cycles)))
            }
            Expr::Call { name, args, .. } => {
                let args_cost = args.iter().fold(Cost::ZERO, |acc, a| acc.add(self.expr(a, stack, cycles)));
                args_cost.add(self.fn_cost(name, stack, cycles))
            }
        }
    }
}
