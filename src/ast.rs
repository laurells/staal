// Abstract syntax tree for the Intent v0.1 subset.

use crate::lexer::Pos;

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Named(String),
    Proposed(Box<Ty>),
    /// A capability, e.g. `cap email.send` -> Cap(["email", "send"])
    Cap(Vec<String>),
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::Named(n) => write!(f, "{n}"),
            Ty::Proposed(t) => write!(f, "Proposed<{t}>"),
            Ty::Cap(p) => write!(f, "cap {}", p.join(".")),
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
    pub body: Vec<Stmt>,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum Stmt {
    Let { name: String, ty: Option<Ty>, value: Expr, pos: Pos },
    Expr(Expr),
    Return(Expr, Pos),
}

#[derive(Debug)]
pub enum Expr {
    Var(String, Pos),
    Str(String, Pos),
    /// `propose(T)` — ask a model for a value of type T
    Propose(Ty, Pos),
    /// `validate x` — turn Proposed<T> into T
    Validate(Box<Expr>, Pos),
    /// `receiver.method(args)` — an action performed through a capability
    Action { receiver: String, method: String, args: Vec<Expr>, pos: Pos },
}

impl Expr {
    pub fn pos(&self) -> Pos {
        match self {
            Expr::Var(_, p) | Expr::Str(_, p) | Expr::Propose(_, p) | Expr::Validate(_, p) => *p,
            Expr::Action { pos, .. } => *pos,
        }
    }
}

#[derive(Debug, Default)]
pub struct Program {
    pub types: Vec<TypeDecl>,
    pub fns: Vec<FnDecl>,
}
