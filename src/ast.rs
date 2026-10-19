// Abstract syntax tree for the Intent v0.1 subset.

use crate::lexer::Pos;

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Named(String),
    Proposed(Box<Ty>),
    /// `List<T, N>`: a list of at most N values of type T.
    List(Box<Ty>, u64),
    /// A capability, e.g. `cap payments.refund(max: 500 USD)`, with its limits.
    Cap { path: Vec<String>, limits: CapLimits },
}

/// The limits on a capability. Every limit is optional; a capability with none is
/// unlimited. All of them hold together, and through every narrowed copy.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CapLimits {
    /// `max: 500 USD`: the most money that may be spent through it in total.
    pub max: Option<Limit>,
    /// `each: 50 USD`: the most money one action may spend.
    pub each: Option<Limit>,
    /// `uses: 10`: how many actions may be attempted through it in total.
    pub uses: Option<u64>,
    /// `only: ["a", "*@b.org"]`: the targets it may act on (see `pattern`).
    pub only: Option<Vec<String>>,
}

impl CapLimits {
    pub fn is_empty(&self) -> bool {
        *self == CapLimits::default()
    }
}

impl std::fmt::Display for CapLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if let Some(m) = &self.max {
            parts.push(format!("max: {m}"));
        }
        if let Some(e) = &self.each {
            parts.push(format!("each: {e}"));
        }
        if let Some(u) = self.uses {
            parts.push(format!("uses: {u}"));
        }
        if let Some(o) = &self.only {
            let items: Vec<String> = o.iter().map(|p| format!("{p:?}")).collect();
            parts.push(format!("only: [{}]", items.join(", ")));
        }
        if parts.is_empty() { Ok(()) } else { write!(f, "({})", parts.join(", ")) }
    }
}

/// An upper bound on a capability, e.g. `500 USD`. `unit` is None for a bare number.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub amount: u64,
    pub unit: Option<String>,
}

impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.unit {
            Some(u) => write!(f, "{} {u}", self.amount),
            None => write!(f, "{}", self.amount),
        }
    }
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::Named(n) => write!(f, "{n}"),
            Ty::Proposed(t) => write!(f, "Proposed<{t}>"),
            Ty::List(t, n) => write!(f, "List<{t}, {n}>"),
            Ty::Cap { path, limits } => write!(f, "cap {}{limits}", path.join(".")),
        }
    }
}

#[derive(Debug)]
pub struct TypeDecl {
    /// `pub type ...`: other modules may use it.
    pub public: bool,
    pub name: String,
    pub fields: Vec<(String, Ty)>,
    pub pos: Pos,
}

#[derive(Debug)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
    /// `name: T @trusted` — callers may only pass trusted values here.
    pub trusted: bool,
    /// `@blind` (whole value, stored as one empty path) or `@blind(customer.email, ...)`:
    /// paths inside this parameter that must never reach a model, here or in anything
    /// this function passes them to.
    pub blind: Vec<Vec<String>>,
}

#[derive(Debug)]
pub struct FnDecl {
    /// `pub fn ...`: other modules may call it.
    pub public: bool,
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Ty>,
    /// `-> T @trusted` — every returned value must be trusted.
    pub ret_trusted: bool,
    pub budget: Option<Budget>,
    pub body: Vec<Stmt>,
    pub pos: Pos,
}

/// `budget(calls: N, tokens: N)` — either part may be omitted.
#[derive(Debug, Clone)]
pub struct Budget {
    pub calls: Option<u64>,
    pub tokens: Option<u64>,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum Stmt {
    Let { name: String, ty: Option<Ty>, value: Expr, pos: Pos },
    Expr(Expr),
    /// `return value`, or a bare `return` in a function that returns nothing
    Return(Option<Expr>, Pos),
    /// `repeat N { ... }` — runs the body at most N times
    Repeat { count: u64, body: Vec<Stmt>, pos: Pos },
    /// `loop { ... }` — no bound on iterations
    Loop { body: Vec<Stmt>, pos: Pos },
    /// `if cond { ... } else { ... }`; `else if` is stored as an else block holding one If.
    If { cond: Expr, then: Vec<Stmt>, els: Vec<Stmt>, pos: Pos },
    /// `break` — leave the innermost `repeat`, `loop` or `for`
    Break(Pos),
    /// `for x in xs { ... }` — once per element of a bounded list
    For { var: String, list: Expr, body: Vec<Stmt>, pos: Pos },
    /// `set name = value` — give a variable declared with `let` a new value
    Set { name: String, value: Expr, pos: Pos },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl std::fmt::Display for ArithOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ArithOp::Add => "+",
            ArithOp::Sub => "-",
            ArithOp::Mul => "*",
            ArithOp::Div => "/",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogicOp {
    And,
    Or,
}

/// Functions built into the language. Their names are reserved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Builtin {
    Len,
    Contains,
    StartsWith,
    EndsWith,
}

impl Builtin {
    pub fn from_name(n: &str) -> Option<Builtin> {
        match n {
            "len" => Some(Builtin::Len),
            "contains" => Some(Builtin::Contains),
            "starts_with" => Some(Builtin::StartsWith),
            "ends_with" => Some(Builtin::EndsWith),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Len => "len",
            Builtin::Contains => "contains",
            Builtin::StartsWith => "starts_with",
            Builtin::EndsWith => "ends_with",
        }
    }
}

impl Stmt {
    /// The expressions this statement evaluates itself (not those in nested blocks).
    pub fn exprs(&self) -> Vec<&Expr> {
        match self {
            Stmt::Let { value, .. } | Stmt::Set { value, .. } => vec![value],
            Stmt::Expr(e) | Stmt::Return(Some(e), _) => vec![e],
            Stmt::If { cond, .. } => vec![cond],
            Stmt::For { list, .. } => vec![list],
            Stmt::Return(None, _) | Stmt::Repeat { .. } | Stmt::Loop { .. } | Stmt::Break(_) => Vec::new(),
        }
    }

    /// The blocks nested directly in this statement.
    pub fn blocks(&self) -> Vec<&[Stmt]> {
        match self {
            Stmt::Repeat { body, .. } | Stmt::Loop { body, .. } | Stmt::For { body, .. } => vec![body],
            Stmt::If { then, els, .. } => vec![then, els],
            Stmt::Let { .. } | Stmt::Set { .. } | Stmt::Expr(_) | Stmt::Return(..) | Stmt::Break(_) => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl std::fmt::Display for CmpOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug)]
pub enum Expr {
    Var(String, Pos),
    Str(String, Pos),
    Int(u64, Pos),
    Bool(bool, Pos),
    /// `12.50 USD`: an exact amount, checked against its currency when parsed.
    Money(crate::interp::money::Money, Pos),
    /// `base.field`, e.g. `order.total` or `ticket.customer.email`
    Field { base: Box<Expr>, field: String, pos: Pos },
    /// `left op right`, e.g. `count <= 3`
    Cmp { op: CmpOp, left: Box<Expr>, right: Box<Expr>, pos: Pos },
    /// `left + right` and the other arithmetic operators
    Arith { op: ArithOp, left: Box<Expr>, right: Box<Expr>, pos: Pos },
    /// `-x`
    Neg(Box<Expr>, Pos),
    /// `left and right`, `left or right`; the right side runs only if needed
    Logic { op: LogicOp, left: Box<Expr>, right: Box<Expr>, pos: Pos },
    /// `not x`
    Not(Box<Expr>, Pos),
    /// `len(x)` and the other built-in functions
    Builtin { f: Builtin, args: Vec<Expr>, pos: Pos },
    /// `Type { field: value, ... }`
    Record { ty: String, fields: Vec<(String, Expr)>, pos: Pos },
    /// `[a, b, c]`
    List(Vec<Expr>, Pos),
    /// `xs[i]`
    Index { base: Box<Expr>, index: Box<Expr>, pos: Pos },
    /// `name(args)` — a call to a function defined in the program
    Call { name: String, args: Vec<Expr>, pos: Pos },
    /// `propose(T, instruct: E, data: [E, ...], blind_to: [path, ...], tokens: N)` — ask a model for a T.
    /// All options are optional. `instruct` must be trusted (E006); `data` may be anything
    /// except what `blind_to` names (E007). `tokens` is the most this one call may use.
    Propose {
        ty: Ty,
        instruct: Option<Box<Expr>>,
        data: Vec<Expr>,
        blind_to: Vec<Expr>,
        tokens: Option<u64>,
        pos: Pos,
    },
    /// `validate x` — turn Proposed<T> into T
    Validate(Box<Expr>, Pos),
    /// `done(evidence: [e, ...])` — the only way to make a `Done`: proof of completed work.
    Done { evidence: Vec<Expr>, pos: Pos },
    /// `try action` or `try call(...)`: a failure becomes an `Outcome` value
    /// instead of stopping the function.
    Try(Box<Expr>, Pos),
    /// `receiver.method(args)` — an action performed through a capability
    Action { receiver: String, method: String, args: Vec<Expr>, pos: Pos },
}

impl Expr {
    /// The expressions directly inside this one, in evaluation order. Exhaustive on
    /// purpose: a new kind of expression must say here what it contains, so analyses
    /// that walk expressions through this function can never skip part of one.
    pub fn children(&self) -> Vec<&Expr> {
        match self {
            Expr::Var(..) | Expr::Str(..) | Expr::Int(..) | Expr::Bool(..) | Expr::Money(..) => Vec::new(),
            Expr::Field { base, .. } => vec![base],
            Expr::Validate(inner, _) | Expr::Try(inner, _) | Expr::Neg(inner, _) | Expr::Not(inner, _) => vec![inner],
            Expr::Cmp { left, right, .. } | Expr::Arith { left, right, .. } | Expr::Logic { left, right, .. } => {
                vec![left, right]
            }
            Expr::Index { base, index, .. } => vec![base, index],
            Expr::Call { args, .. }
            | Expr::Action { args, .. }
            | Expr::Builtin { args, .. }
            | Expr::List(args, _)
            | Expr::Done { evidence: args, .. } => args.iter().collect(),
            Expr::Record { fields, .. } => fields.iter().map(|(_, v)| v).collect(),
            // Only what the model is given is evaluated; `blind_to` names data, it is not evaluated.
            Expr::Propose { instruct, data, .. } => instruct.iter().map(|b| b.as_ref()).chain(data.iter()).collect(),
        }
    }

    pub fn pos(&self) -> Pos {
        match self {
            Expr::Var(_, p)
            | Expr::Str(_, p)
            | Expr::Int(_, p)
            | Expr::Bool(_, p)
            | Expr::Money(_, p)
            | Expr::Validate(_, p)
            | Expr::Try(_, p)
            | Expr::Neg(_, p)
            | Expr::Not(_, p)
            | Expr::List(_, p) => *p,
            Expr::Arith { pos, .. }
            | Expr::Logic { pos, .. }
            | Expr::Builtin { pos, .. }
            | Expr::Record { pos, .. }
            | Expr::Index { pos, .. } => *pos,
            Expr::Cmp { pos, .. } | Expr::Field { pos, .. } | Expr::Done { pos, .. } => *pos,
            Expr::Propose { pos, .. } => *pos,
            Expr::Action { pos, .. } | Expr::Call { pos, .. } => *pos,
        }
    }
}

/// `import a.b`, `import a.b as c`, `import a.b { T, f }`: makes module `a/b.int`
/// available as `b` (or `c`), and optionally names from it without qualification.
#[derive(Debug, Clone)]
pub struct Import {
    /// The module path, e.g. ["sms", "termii"].
    pub path: Vec<String>,
    /// The name the module is known by in this file.
    pub alias: String,
    /// Names brought in unqualified, with where each was written.
    pub names: Vec<(String, Pos)>,
    pub pos: Pos,
}

#[derive(Debug, Default)]
pub struct Program {
    /// The imports of a single parsed file. Empty after modules are resolved into one
    /// program (see `crate::modules`), where every name is already qualified.
    pub imports: Vec<Import>,
    pub types: Vec<TypeDecl>,
    pub fns: Vec<FnDecl>,
}
