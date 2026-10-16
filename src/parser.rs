// Hand-written recursive-descent parser.
//
// Grammar (v0.1 subset):
//   program  := (typedecl | fndecl)*
//   typedecl := "type" IDENT "{" (IDENT ":" ty ","?)* "}"
//   fndecl   := "fn" IDENT "(" (param ("," param)*)? ")" ("->" ty ("@" "trusted")?)? budget? block
//   budget   := "budget" "(" limit ("," limit)* ")"      limit := ("calls" | "tokens") ":" NUM
//   param    := IDENT ":" ty mark*
//   mark     := "@" "trusted" | "@" "blind" ("(" field_path ("," field_path)* ")")?
//   field_path := IDENT ("." IDENT)*
//   ty       := "cap" IDENT ("." IDENT)* ("(" "max" ":" NUM IDENT? ")")?
//             | IDENT ("<" ty ">")?
//   block    := "{" stmt* "}"
//   stmt     := "let" IDENT (":" ty)? "=" expr | "return" expr?
//             | "repeat" NUM block | "loop" block | "break"
//             | "if" expr block ("else" (block | if-stmt))? | expr
//   expr     := primary (("==" | "!=" | "<" | "<=" | ">" | ">=") primary)?
//   primary  := atom ("." IDENT)*         (field access, e.g. order.customer.email)
//   atom     := "propose" "(" ty ("," popt)* ")" | "validate" primary
//             | "try" primary             (an action or call; failure becomes an Outcome)
//             | IDENT "." IDENT args      (action through a capability variable)
//             | IDENT args                (call to a program function)
//             | IDENT | STRING | NUM | "true" | "false"
//   popt     := "instruct" ":" expr | "data" ":" list | "blind_to" ":" list | "tokens" ":" NUM
//   list     := "[" (expr ("," expr)*)? "]"
//   args     := "(" (expr ("," expr)*)? ")"

use crate::ast::*;
use crate::lexer::{Pos, Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    i: usize,
}

type PResult<T> = Result<T, (Pos, String)>;

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser { toks, i: 0 }
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.i].tok
    }
    fn pos(&self) -> Pos {
        self.toks[self.i].pos
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.i].clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }
    fn expect(&mut self, want: Tok, what: &str) -> PResult<()> {
        if *self.peek() == want {
            self.bump();
            Ok(())
        } else {
            Err((self.pos(), format!("expected {what}, found {:?}", self.peek())))
        }
    }
    fn ident(&mut self) -> PResult<String> {
        match self.peek().clone() {
            Tok::Ident(s) => {
                self.bump();
                Ok(s)
            }
            other => Err((self.pos(), format!("expected identifier, found {other:?}"))),
        }
    }

    fn num(&mut self) -> PResult<u64> {
        match self.peek().clone() {
            Tok::Num(n) => {
                self.bump();
                Ok(n)
            }
            other => Err((self.pos(), format!("expected a number, found {other:?}"))),
        }
    }

    /// Optional `@trusted` after a return type.
    fn trust_mark(&mut self) -> PResult<bool> {
        if *self.peek() != Tok::At {
            return Ok(false);
        }
        self.bump();
        let pos = self.pos();
        let label = self.ident()?;
        match label.as_str() {
            "trusted" => Ok(true),
            "blind" => Err((pos, "'@blind' applies to parameters, not return types".into())),
            _ => Err((pos, format!("unknown label '@{label}'; use '@trusted' or '@blind'"))),
        }
    }

    /// Zero or more `@trusted` / `@blind(...)` marks after a parameter type.
    fn param_marks(&mut self) -> PResult<(bool, Vec<Vec<String>>)> {
        let (mut trusted, mut blind) = (false, Vec::new());
        let mut seen_blind = false;
        while *self.peek() == Tok::At {
            self.bump();
            let pos = self.pos();
            let label = self.ident()?;
            match label.as_str() {
                "trusted" if !trusted => trusted = true,
                "blind" if !seen_blind => {
                    seen_blind = true;
                    if *self.peek() == Tok::LParen {
                        self.bump();
                        loop {
                            let mut path = vec![self.ident()?];
                            while *self.peek() == Tok::Dot {
                                self.bump();
                                path.push(self.ident()?);
                            }
                            blind.push(path);
                            if *self.peek() == Tok::Comma {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                        self.expect(Tok::RParen, "')'")?;
                    } else {
                        blind.push(Vec::new()); // the whole value
                    }
                }
                "trusted" | "blind" => return Err((pos, format!("'@{label}' given twice"))),
                _ => return Err((pos, format!("unknown label '@{label}'; use '@trusted' or '@blind'"))),
            }
        }
        Ok((trusted, blind))
    }

    fn budget(&mut self) -> PResult<Budget> {
        let pos = self.pos();
        self.expect(Tok::Budget, "'budget'")?;
        self.expect(Tok::LParen, "'('")?;
        let mut b = Budget { calls: None, tokens: None, pos };
        loop {
            let kpos = self.pos();
            let key = self.ident()?;
            self.expect(Tok::Colon, "':'")?;
            let n = self.num()?;
            let slot = match key.as_str() {
                "calls" => &mut b.calls,
                "tokens" => &mut b.tokens,
                _ => return Err((kpos, format!("unknown budget limit '{key}'; use 'calls' or 'tokens'"))),
            };
            if slot.is_some() {
                return Err((kpos, format!("budget limit '{key}' given twice")));
            }
            *slot = Some(n);
            if *self.peek() == Tok::Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(Tok::RParen, "')'")?;
        Ok(b)
    }

    pub fn program(&mut self) -> PResult<Program> {
        let mut p = Program::default();
        loop {
            match self.peek() {
                Tok::Type => p.types.push(self.typedecl()?),
                Tok::Fn => p.fns.push(self.fndecl()?),
                Tok::Eof => return Ok(p),
                other => {
                    return Err((self.pos(), format!("expected 'type' or 'fn', found {other:?}")))
                }
            }
        }
    }

    fn typedecl(&mut self) -> PResult<TypeDecl> {
        let pos = self.pos();
        self.expect(Tok::Type, "'type'")?;
        let name = self.ident()?;
        self.expect(Tok::LBrace, "'{'")?;
        let mut fields = Vec::new();
        while *self.peek() != Tok::RBrace {
            let f = self.ident()?;
            self.expect(Tok::Colon, "':'")?;
            let t = self.ty()?;
            fields.push((f, t));
            if *self.peek() == Tok::Comma {
                self.bump();
            }
        }
        self.expect(Tok::RBrace, "'}'")?;
        Ok(TypeDecl { name, fields, pos })
    }

    fn fndecl(&mut self) -> PResult<FnDecl> {
        let pos = self.pos();
        self.expect(Tok::Fn, "'fn'")?;
        let name = self.ident()?;
        self.expect(Tok::LParen, "'('")?;
        let mut params = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                let pname = self.ident()?;
                self.expect(Tok::Colon, "':'")?;
                let ty = self.ty()?;
                let (trusted, blind) = self.param_marks()?;
                params.push(Param { name: pname, ty, trusted, blind });
                if *self.peek() == Tok::Comma {
                    self.bump();
                    if *self.peek() == Tok::RParen {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        self.expect(Tok::RParen, "')'")?;
        let (ret, ret_trusted) = if *self.peek() == Tok::Arrow {
            self.bump();
            let t = self.ty()?;
            (Some(t), self.trust_mark()?)
        } else {
            (None, false)
        };
        let budget = if *self.peek() == Tok::Budget { Some(self.budget()?) } else { None };
        let body = self.block()?;
        Ok(FnDecl { name, params, ret, ret_trusted, budget, body, pos })
    }

    fn ty(&mut self) -> PResult<Ty> {
        if *self.peek() == Tok::Cap {
            self.bump();
            let mut path = vec![self.ident()?];
            while *self.peek() == Tok::Dot {
                self.bump();
                path.push(self.ident()?);
            }
            let mut max = None;
            if *self.peek() == Tok::LParen {
                self.bump();
                let kpos = self.pos();
                let key = self.ident()?;
                if key != "max" {
                    return Err((kpos, format!("unknown capability limit '{key}'; only 'max' is supported")));
                }
                self.expect(Tok::Colon, "':'")?;
                let amount = self.num()?;
                let unit = match self.peek().clone() {
                    Tok::Ident(u) => {
                        self.bump();
                        Some(u)
                    }
                    _ => None,
                };
                self.expect(Tok::RParen, "')'")?;
                max = Some(Limit { amount, unit });
            }
            return Ok(Ty::Cap { path, max });
        }
        let name = self.ident()?;
        if *self.peek() == Tok::Lt {
            self.bump();
            let inner = self.ty()?;
            self.expect(Tok::Gt, "'>'")?;
            if name == "Proposed" {
                return Ok(Ty::Proposed(Box::new(inner)));
            }
            return Err((self.pos(), format!("unknown generic type '{name}'")));
        }
        Ok(Ty::Named(name))
    }

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect(Tok::LBrace, "'{'")?;
        let mut stmts = Vec::new();
        while *self.peek() != Tok::RBrace {
            if *self.peek() == Tok::Eof {
                return Err((self.pos(), "unexpected end of file, missing '}'".into()));
            }
            stmts.push(self.stmt()?);
        }
        self.expect(Tok::RBrace, "'}'")?;
        Ok(stmts)
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let pos = self.pos();
        match self.peek() {
            Tok::Let => {
                self.bump();
                let name = self.ident()?;
                let ty = if *self.peek() == Tok::Colon {
                    self.bump();
                    Some(self.ty()?)
                } else {
                    None
                };
                self.expect(Tok::Eq, "'='")?;
                let value = self.expr()?;
                Ok(Stmt::Let { name, ty, value, pos })
            }
            Tok::Return => {
                self.bump();
                // A bare `return` ends at the closing brace of its block.
                if *self.peek() == Tok::RBrace {
                    return Ok(Stmt::Return(None, pos));
                }
                Ok(Stmt::Return(Some(self.expr()?), pos))
            }
            Tok::Repeat => {
                self.bump();
                let count = self.num()?;
                let body = self.block()?;
                Ok(Stmt::Repeat { count, body, pos })
            }
            Tok::Loop => {
                self.bump();
                let body = self.block()?;
                Ok(Stmt::Loop { body, pos })
            }
            Tok::Break => {
                self.bump();
                Ok(Stmt::Break(pos))
            }
            Tok::If => self.if_stmt(),
            _ => Ok(Stmt::Expr(self.expr()?)),
        }
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let pos = self.pos();
        self.expect(Tok::If, "'if'")?;
        let cond = self.expr()?;
        let then = self.block()?;
        let els = if *self.peek() == Tok::Else {
            self.bump();
            if *self.peek() == Tok::If {
                vec![self.if_stmt()?]
            } else {
                self.block()?
            }
        } else {
            Vec::new()
        };
        Ok(Stmt::If { cond, then, els, pos })
    }

    fn expr(&mut self) -> PResult<Expr> {
        let left = self.primary()?;
        let op = match self.peek() {
            Tok::EqEq => CmpOp::Eq,
            Tok::NotEq => CmpOp::Ne,
            Tok::Lt => CmpOp::Lt,
            Tok::LtEq => CmpOp::Le,
            Tok::Gt => CmpOp::Gt,
            Tok::GtEq => CmpOp::Ge,
            _ => return Ok(left),
        };
        let pos = self.pos();
        self.bump();
        let right = self.primary()?;
        if matches!(
            self.peek(),
            Tok::EqEq | Tok::NotEq | Tok::Lt | Tok::LtEq | Tok::Gt | Tok::GtEq
        ) {
            return Err((self.pos(), "comparisons cannot be chained; compare two values at a time".into()));
        }
        Ok(Expr::Cmp { op, left: Box::new(left), right: Box::new(right), pos })
    }

    fn propose(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        self.expect(Tok::Propose, "'propose'")?;
        self.expect(Tok::LParen, "'('")?;
        let ty = self.ty()?;
        let (mut instruct, mut data, mut blind_to, mut tokens) = (None, None, None, None);
        while *self.peek() == Tok::Comma {
            self.bump();
            let kpos = self.pos();
            let key = self.ident()?;
            self.expect(Tok::Colon, "':'")?;
            let dup = match key.as_str() {
                "instruct" => instruct.replace(Box::new(self.expr()?)).is_some(),
                "tokens" => tokens.replace(self.num()?).is_some(),
                "data" => data.replace(self.list()?).is_some(),
                "blind_to" => blind_to.replace(self.list()?).is_some(),
                _ => {
                    return Err((
                        kpos,
                        format!("unknown propose option '{key}'; use 'instruct', 'data', 'blind_to' or 'tokens'"),
                    ))
                }
            };
            if dup {
                return Err((kpos, format!("propose option '{key}' given twice")));
            }
        }
        self.expect(Tok::RParen, "')'")?;
        Ok(Expr::Propose {
            ty,
            instruct,
            data: data.unwrap_or_default(),
            blind_to: blind_to.unwrap_or_default(),
            tokens,
            pos,
        })
    }

    /// "[" (expr ("," expr)*)? "]"
    fn list(&mut self) -> PResult<Vec<Expr>> {
        self.expect(Tok::LBracket, "'['")?;
        let mut items = Vec::new();
        if *self.peek() != Tok::RBracket {
            loop {
                items.push(self.expr()?);
                if *self.peek() == Tok::Comma {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(Tok::RBracket, "']'")?;
        Ok(items)
    }

    /// An atom followed by any number of `.field` accesses.
    fn primary(&mut self) -> PResult<Expr> {
        let mut e = self.atom()?;
        while *self.peek() == Tok::Dot {
            self.bump();
            let pos = self.pos();
            let field = self.ident()?;
            if *self.peek() == Tok::LParen {
                return Err((
                    pos,
                    format!("'{field}(...)' is not allowed here: actions must be called directly on a capability variable"),
                ));
            }
            e = Expr::Field { base: Box::new(e), field, pos };
        }
        Ok(e)
    }

    fn atom(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        match self.peek().clone() {
            Tok::Propose => self.propose(),
            Tok::Validate => {
                self.bump();
                Ok(Expr::Validate(Box::new(self.primary()?), pos))
            }
            Tok::Try => {
                self.bump();
                Ok(Expr::Try(Box::new(self.atom()?), pos))
            }
            Tok::True => {
                self.bump();
                Ok(Expr::Bool(true, pos))
            }
            Tok::False => {
                self.bump();
                Ok(Expr::Bool(false, pos))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Expr::Str(s, pos))
            }
            Tok::Num(n) => {
                self.bump();
                Ok(Expr::Int(n, pos))
            }
            Tok::Ident(name) => {
                self.bump();
                if *self.peek() == Tok::LParen {
                    let args = self.args()?;
                    return Ok(Expr::Call { name, args, pos });
                }
                // `name.method(` is an action; `name.field` is left to `primary`.
                let is_action = *self.peek() == Tok::Dot
                    && matches!(self.toks.get(self.i + 1).map(|t| &t.tok), Some(Tok::Ident(_)))
                    && matches!(self.toks.get(self.i + 2).map(|t| &t.tok), Some(Tok::LParen));
                if is_action {
                    self.bump();
                    let method = self.ident()?;
                    let args = self.args()?;
                    Ok(Expr::Action { receiver: name, method, args, pos })
                } else {
                    Ok(Expr::Var(name, pos))
                }
            }
            other => Err((pos, format!("expected expression, found {other:?}"))),
        }
    }

    /// "(" (expr ("," expr)*)? ")"
    fn args(&mut self) -> PResult<Vec<Expr>> {
        self.expect(Tok::LParen, "'('")?;
        let mut args = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                args.push(self.expr()?);
                if *self.peek() == Tok::Comma {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(Tok::RParen, "')'")?;
        Ok(args)
    }
}
