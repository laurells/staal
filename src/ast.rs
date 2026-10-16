// Abstract syntax tree for the Intent v0.1 subset.

use crate::lexer::Pos;

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Named(String),
    Proposed(Box<Ty>),
    /// A capability, e.g. `cap payments.refund(max: 500 USD)`.
    /// `max: None` means the capability has no upper limit.
    Cap { path: Vec<String>, max: Option<Limit> },
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
            Ty::Cap { path, max: None } => write!(f, "cap {}", path.join(".")),
            Ty::Cap { path, max: Some(m) } => write!(f, "cap {}(max: {m})", path.join(".")),
        }
    }
}

#[derive(Debug)]
pub struct TypeDecl {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
    pub pos: Pos,
}

#[derive(Debug)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<Ty>,
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
    Return(Expr, Pos),
    /// `repeat N { ... }` — runs the body at most N times
    Repeat { count: u64, body: Vec<Stmt>, pos: Pos },
    /// `loop { ... }` — no bound on iterations
    Loop { body: Vec<Stmt>, pos: Pos },
}

#[derive(Debug)]
pub enum Expr {
    Var(String, Pos),
    Str(String, Pos),
    Int(u64, Pos),
    /// `name(args)` — a call to a function defined in the program
    Call { name: String, args: Vec<Expr>, pos: Pos },
    /// `propose(T)` or `propose(T, tokens: N)` — ask a model for a value of type T.
    /// `tokens` is the most this one call may use; None means no declared limit.
    Propose { ty: Ty, tokens: Option<u64>, pos: Pos },
    /// `validate x` — turn Proposed<T> into T
    Validate(Box<Expr>, Pos),
    /// `receiver.method(args)` — an action performed through a capability
    Action { receiver: String, method: String, args: Vec<Expr>, pos: Pos },
}

impl Expr {
    pub fn pos(&self) -> Pos {
        match self {
            Expr::Var(_, p)
            | Expr::Str(_, p)
            | Expr::Int(_, p)
            | Expr::Validate(_, p) => *p,
            Expr::Propose { pos, .. } => *pos,
            Expr::Action { pos, .. } | Expr::Call { pos, .. } => *pos,
        }
    }
}

#[derive(Debug, Default)]
pub struct Program {
    pub types: Vec<TypeDecl>,
    pub fns: Vec<FnDecl>,
}
