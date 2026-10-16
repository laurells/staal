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
    /// `name: T @trusted` — callers may only pass trusted values here.
    pub trusted: bool,
}

#[derive(Debug)]
pub struct FnDecl {
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
    /// `break` — leave the innermost `repeat` or `loop`
    Break(Pos),
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
    /// `base.field`, e.g. `order.total` or `ticket.customer.email`
    Field { base: Box<Expr>, field: String, pos: Pos },
    /// `left op right`, e.g. `count <= 3`
    Cmp { op: CmpOp, left: Box<Expr>, right: Box<Expr>, pos: Pos },
    /// `name(args)` — a call to a function defined in the program
    Call { name: String, args: Vec<Expr>, pos: Pos },
    /// `propose(T, instruct: E, data: [E, ...], tokens: N)` — ask a model for a T.
    /// All options are optional. `instruct` must be trusted (E006); `data` may be anything.
    /// `tokens` is the most this one call may use; None means no declared limit.
    Propose { ty: Ty, instruct: Option<Box<Expr>>, data: Vec<Expr>, tokens: Option<u64>, pos: Pos },
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
            | Expr::Bool(_, p)
            | Expr::Validate(_, p) => *p,
            Expr::Cmp { pos, .. } | Expr::Field { pos, .. } => *pos,
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
