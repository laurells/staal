// Hand-written recursive-descent parser.
//
// Grammar (v0.1 subset):
//   program  := (typedecl | fndecl)*
//   typedecl := "type" IDENT "{" (IDENT ":" ty ","?)* "}"
//   fndecl   := "fn" IDENT "(" (param ("," param)*)? ")" ("->" ty)? block
//   param    := IDENT ":" ty
//   ty       := "cap" IDENT ("." IDENT)* | IDENT ("<" ty ">")?
//   block    := "{" stmt* "}"
//   stmt     := "let" IDENT (":" ty)? "=" expr | "return" expr | expr
//   expr     := "propose" "(" ty ")" | "validate" expr
//             | IDENT "." IDENT "(" (expr ("," expr)*)? ")" | IDENT | STRING

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
                params.push(Param { name: pname, ty });
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
        let ret = if *self.peek() == Tok::Arrow {
            self.bump();
            Some(self.ty()?)
        } else {
            None
        };
        let body = self.block()?;
        Ok(FnDecl { name, params, ret, body, pos })
    }

    fn ty(&mut self) -> PResult<Ty> {
        if *self.peek() == Tok::Cap {
            self.bump();
            let mut path = vec![self.ident()?];
            while *self.peek() == Tok::Dot {
                self.bump();
                path.push(self.ident()?);
            }
            return Ok(Ty::Cap(path));
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
                Ok(Stmt::Return(self.expr()?, pos))
            }
            _ => Ok(Stmt::Expr(self.expr()?)),
        }
    }

    fn expr(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        match self.peek().clone() {
            Tok::Propose => {
                self.bump();
                self.expect(Tok::LParen, "'('")?;
                let t = self.ty()?;
                self.expect(Tok::RParen, "')'")?;
                Ok(Expr::Propose(t, pos))
            }
            Tok::Validate => {
                self.bump();
                Ok(Expr::Validate(Box::new(self.expr()?), pos))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Expr::Str(s, pos))
            }
            Tok::Ident(name) => {
                self.bump();
                if *self.peek() == Tok::Dot {
                    self.bump();
                    let method = self.ident()?;
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
                    Ok(Expr::Action { receiver: name, method, args, pos })
                } else {
                    Ok(Expr::Var(name, pos))
                }
            }
            other => Err((pos, format!("expected expression, found {other:?}"))),
        }
    }
}
