// Hand-written recursive-descent parser.
//
// Grammar:
//   program  := import* ("pub"? (typedecl | fndecl | capdecl))*
//   capdecl  := "capability" IDENT ("." IDENT)* "{" need* (canonical | action)* "}"
//   need     := "needs" IDENT ":" ty
//   canonical:= "canonical" "(" param ")" "->" ty block
//   action   := "action" IDENT "(" (param ("," param)*)? ")" "->" ty block
//               (`needs`, `canonical` and `action` are words only inside a capability)
//   import   := "import" IDENT ("." IDENT)* ("as" IDENT)? ("{" IDENT ("," IDENT)* ","? "}")?
//   typedecl := "type" IDENT "{" (IDENT ":" ty ","?)* "}"
//   fndecl   := "fn" IDENT "(" (param ("," param)*)? ")" ("->" ty ("@" "trusted")?)? budget? block
//   budget   := "budget" "(" limit ("," limit)* ")"      limit := ("calls" | "tokens") ":" NUM
//   param    := IDENT ":" ty mark*
//   mark     := "@" "trusted" | "@" "blind" ("(" field_path ("," field_path)* ")")?
//             | "@" "target" | "@" "amount"     (capability actions only)
//   field_path := IDENT ("." IDENT)*
//   ty       := "cap" IDENT ("." IDENT)* ("(" climit ("," climit)* ")")?
//   climit   := ("max" | "each") ":" NUM IDENT? | "uses" ":" NUM | "only" ":" "[" (STRING ","?)* "]"
//             | "List" "<" ty "," NUM ">" | "Proposed" "<" ty ">" | IDENT ("." IDENT)?
//               (`m.T` is type T of the module imported as m)
//   block    := "{" stmt* "}"
//   stmt     := "let" IDENT (":" ty)? "=" expr | "set" IDENT "=" expr | "return" expr?
//             | "fail" "(" expr ")"
//             | "repeat" NUM block | "loop" block | "break"
//             | "for" IDENT "in" expr* block
//             | "if" expr* block ("else" (block | if-stmt))? | expr
//   expr*    := an expr in which a record literal needs parentheses, so that in
//               `if ready { ... }` the brace opens the block (as in Rust)
//   expr     := and_expr ("or" and_expr)*
//   and_expr := not_expr ("and" not_expr)*
//   not_expr := "not" not_expr | cmp_expr
//   cmp_expr := add_expr (("==" | "!=" | "<" | "<=" | ">" | ">=") add_expr)?   (not chained)
//   add_expr := mul_expr (("+" | "-") mul_expr)*
//   mul_expr := unary (("*" | "/") unary)*
//   unary    := "-" unary | primary
//   primary  := atom ("." IDENT | "[" expr "]")*     (field access, indexing)
//   atom     := "(" expr ")" | "[" (expr ("," expr)*)? "]"
//             | "propose" "(" ty ("," popt)* ")" | "validate" primary
//             | "parse" "(" ty "," expr ")"   (JSON text as an unchecked Proposed<ty>)
//             | "try" primary             (an action or call; failure becomes an Outcome)
//             | "done" "(" "evidence" ":" list ")"
//             | IDENT ("." IDENT)? "{" (IDENT ":" expr ","?)* "}"   (record literal)
//             | BUILTIN args              (len, contains, starts_with, ends_with)
//             | IDENT "." IDENT args      (an action through a capability variable, or
//                                          a call to function IDENT of an imported module)
//             | IDENT args                (call to a program function)
//             | IDENT | STRING | NUM | money | "true" | "false"
//   money    := (NUM | DECIMAL) CURRENCY      (on one line: `0 USD`, `12.50 NGN`)
//   popt     := "instruct" ":" expr | "data" ":" list | "blind_to" ":" list | "tokens" ":" NUM
//   list     := "[" (expr ("," expr)*)? "]"
//   args     := "(" (expr ("," expr)*)? ")"

use crate::ast::*;
use crate::lexer::{Pos, Tok, Token};

struct Marks {
    trusted: bool,
    blind: Vec<Vec<String>>,
    target: bool,
    amount: bool,
}

pub struct Parser {
    toks: Vec<Token>,
    i: usize,
    /// Set while parsing an `if` or `for` head, where `{` must open the block.
    no_record: bool,
    /// How many tuples `let (a, b) = ...` has unpacked, to name each one's holder.
    unpacked: usize,
}

type PResult<T> = Result<T, (Pos, String)>;

impl Parser {
    pub fn new(toks: Vec<Token>) -> Self {
        Parser { toks, i: 0, no_record: false, unpacked: 0 }
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

    /// A record field name: an identifier, or a keyword (fields are never ambiguous).
    fn field_name(&mut self) -> PResult<String> {
        if let Some(k) = self.peek().keyword_text() {
            self.bump();
            return Ok(k.to_string());
        }
        self.ident()
    }

    /// One step of a path inside a value: a field name, or a tuple position. `0.1`
    /// arrives as one number and is two positions.
    fn path_parts(&mut self) -> PResult<Vec<String>> {
        match self.peek().clone() {
            Tok::Num(n) => {
                self.bump();
                Ok(vec![n.to_string()])
            }
            Tok::Decimal(d) if d.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) => {
                let pos = self.pos();
                self.bump();
                d.split('.').map(|p| p.parse::<u64>().map(|n| n.to_string()).map_err(|_| (pos, format!("'{p}' is not a tuple position")))).collect()
            }
            _ => Ok(vec![self.field_name()?]),
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

    /// Zero or more `@trusted` / `@blind(...)` / `@target` / `@amount` marks after a
    /// parameter type.
    fn param_marks(&mut self) -> PResult<Marks> {
        let (mut trusted, mut blind) = (false, Vec::new());
        let (mut target, mut amount) = (false, false);
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
                            let mut path = self.path_parts()?;
                            while *self.peek() == Tok::Dot {
                                self.bump();
                                path.extend(self.path_parts()?);
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
                "target" if !target => target = true,
                "amount" if !amount => amount = true,
                "trusted" | "blind" | "target" | "amount" => return Err((pos, format!("'@{label}' given twice"))),
                _ => {
                    return Err((pos, format!("unknown label '@{label}'; use '@trusted', '@blind', '@target' or '@amount'")))
                }
            }
        }
        Ok(Marks { trusted, blind, target, amount })
    }

    /// `500 USD` in a capability limit: a whole number and an optional currency.
    fn money_limit(&mut self) -> PResult<Limit> {
        let amount = self.num()?;
        let unit = match self.peek().clone() {
            Tok::Ident(u) => {
                self.bump();
                Some(u)
            }
            _ => None,
        };
        Ok(Limit { amount, unit })
    }

    /// `["a", "*@b.org"]`: allow-list patterns for `only`.
    fn patterns(&mut self) -> PResult<Vec<String>> {
        self.expect(Tok::LBracket, "'['")?;
        let mut out = Vec::new();
        while *self.peek() != Tok::RBracket {
            let pos = self.pos();
            let Tok::Str(p) = self.peek().clone() else {
                return Err((pos, format!("expected a quoted pattern, found {:?}", self.peek())));
            };
            self.bump();
            crate::pattern::validate(&p).map_err(|e| (pos, e))?;
            out.push(p);
            if *self.peek() == Tok::Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(Tok::RBracket, "']'")?;
        Ok(out)
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
        while *self.peek() == Tok::Import {
            p.imports.push(self.import()?);
        }
        loop {
            let public = *self.peek() == Tok::Pub;
            if public {
                self.bump();
            }
            match self.peek() {
                // `type Name = T` is an alias; `type Name { ... }` a record.
                Tok::Type if self.toks.get(self.i + 2).map(|t| &t.tok) == Some(&Tok::Eq) => {
                    let pos = self.pos();
                    self.bump();
                    let name = self.ident()?;
                    self.bump(); // `=`
                    let ty = self.ty()?;
                    p.aliases.push(AliasDecl { public, name, ty, pos });
                }
                Tok::Type => p.types.push(self.typedecl(public)?),
                Tok::Ident(w) if w == "enum" => p.enums.push(self.enumdecl(public)?),
                Tok::Fn => p.fns.push(self.fndecl(public)?),
                Tok::Ident(w) if w == "capability" => {
                    let (cap, fns) = self.capdecl()?;
                    p.caps.push(cap);
                    p.fns.extend(fns);
                }
                // `const` is a word only here, so it stays usable as a name elsewhere.
                Tok::Ident(w) if w == "const" => p.consts.push(self.constdecl(public)?),
                Tok::Eof if !public => return Ok(p),
                Tok::Import => return Err((self.pos(), "imports must come before every declaration".into())),
                other => {
                    let want = if public { "'type', 'enum', 'fn' or 'const' after 'pub'" } else { "'type', 'enum', 'fn' or 'const'" };
                    return Err((self.pos(), format!("expected {want}, found {other:?}")));
                }
            }
        }
    }

    fn import(&mut self) -> PResult<Import> {
        let pos = self.pos();
        self.expect(Tok::Import, "'import'")?;
        let mut path = vec![self.ident()?];
        while *self.peek() == Tok::Dot {
            self.bump();
            path.push(self.ident()?);
        }
        let mut alias = path.last().cloned().unwrap_or_default();
        if *self.peek() == Tok::Ident("as".into()) {
            self.bump();
            alias = self.ident()?;
        }
        let mut names = Vec::new();
        if *self.peek() == Tok::LBrace {
            self.bump();
            while *self.peek() != Tok::RBrace {
                let npos = self.pos();
                names.push((self.ident()?, npos));
                if *self.peek() == Tok::Comma {
                    self.bump();
                } else {
                    break;
                }
            }
            self.expect(Tok::RBrace, "'}'")?;
            if names.is_empty() {
                return Err((pos, "an import list `{ }` must name at least one thing".into()));
            }
        }
        Ok(Import { path, alias, names, pos })
    }

    fn constdecl(&mut self, public: bool) -> PResult<ConstDecl> {
        let pos = self.pos();
        self.bump(); // `const`
        let name = self.ident()?;
        let ty = if *self.peek() == Tok::Colon {
            self.bump();
            Some(self.ty()?)
        } else {
            None
        };
        self.expect(Tok::Eq, "'='")?;
        let value = self.expr()?;
        Ok(ConstDecl { public, name, ty, value, pos })
    }

    /// "enum" IDENT "{" (IDENT ("{" (IDENT ":" ty ","?)* "}")? ","?)* "}"
    fn enumdecl(&mut self, public: bool) -> PResult<EnumDecl> {
        let pos = self.pos();
        self.bump(); // `enum`
        let name = self.ident()?;
        self.expect(Tok::LBrace, "'{'")?;
        let mut variants = Vec::new();
        while *self.peek() != Tok::RBrace {
            let vpos = self.pos();
            let vname = self.ident()?;
            let mut fields = Vec::new();
            if *self.peek() == Tok::LBrace {
                self.bump();
                while *self.peek() != Tok::RBrace {
                    let f = self.field_name()?;
                    self.expect(Tok::Colon, "':'")?;
                    fields.push((f, self.ty()?));
                    if *self.peek() == Tok::Comma {
                        self.bump();
                    }
                }
                self.expect(Tok::RBrace, "'}'")?;
            }
            variants.push(Variant { name: vname, fields, pos: vpos });
            if *self.peek() == Tok::Comma {
                self.bump();
            }
        }
        self.expect(Tok::RBrace, "'}'")?;
        if variants.is_empty() {
            return Err((pos, format!("enum '{name}' needs at least one variant")));
        }
        Ok(EnumDecl { public, name, variants, pos })
    }

    /// "match" expr* "{" (pattern "=>" block)+ "}"
    fn match_stmt(&mut self) -> PResult<Stmt> {
        let pos = self.pos();
        self.bump(); // `match`
        let value = self.head_expr()?;
        self.expect(Tok::LBrace, "'{'")?;
        let mut arms = Vec::new();
        while *self.peek() != Tok::RBrace {
            let ppos = self.pos();
            let name = self.ident()?;
            let pattern = if name == "_" {
                Pattern::Any(ppos)
            } else if name == "none" {
                Pattern::Absent(ppos)
            } else if name == "some" && *self.peek() == Tok::LParen {
                self.bump();
                let bind = self.ident()?;
                self.expect(Tok::RParen, "')'")?;
                Pattern::Present { bind, pos: ppos }
            } else {
                let mut binds = Vec::new();
                let mut rest = false;
                if *self.peek() == Tok::LBrace {
                    self.bump();
                    while *self.peek() != Tok::RBrace {
                        if *self.peek() == Tok::DotDot {
                            self.bump();
                            rest = true;
                            break;
                        }
                        let field = self.field_name()?;
                        let to = if *self.peek() == Tok::Colon {
                            self.bump();
                            self.ident()?
                        } else {
                            field.clone()
                        };
                        binds.push((field, to));
                        if *self.peek() == Tok::Comma {
                            self.bump();
                        }
                    }
                    self.expect(Tok::RBrace, "'}'")?;
                }
                Pattern::Variant { name, binds, rest, pos: ppos }
            };
            self.expect(Tok::FatArrow, "'=>'")?;
            let body = self.block()?;
            arms.push(Arm { pattern, body });
        }
        self.expect(Tok::RBrace, "'}'")?;
        if arms.is_empty() {
            return Err((pos, "a `match` needs at least one arm".into()));
        }
        Ok(Stmt::Match { value, arms, pos })
    }

    fn typedecl(&mut self, public: bool) -> PResult<TypeDecl> {
        let pos = self.pos();
        self.expect(Tok::Type, "'type'")?;
        let name = self.ident()?;
        self.expect(Tok::LBrace, "'{'")?;
        let mut fields = Vec::new();
        while *self.peek() != Tok::RBrace {
            let f = self.field_name()?;
            self.expect(Tok::Colon, "':'")?;
            let t = self.ty()?;
            fields.push((f, t));
            if *self.peek() == Tok::Comma {
                self.bump();
            }
        }
        self.expect(Tok::RBrace, "'}'")?;
        Ok(TypeDecl { public, name, fields, pos })
    }

    fn fndecl(&mut self, public: bool) -> PResult<FnDecl> {
        let pos = self.pos();
        self.expect(Tok::Fn, "'fn'")?;
        let name = self.ident()?;
        self.fn_rest(public, name, pos)
    }

    /// `capability a.b { needs ...  canonical(...) -> T { }  action m(...) -> T { } }`.
    /// Returns the declaration and the functions holding its bodies.
    fn capdecl(&mut self) -> PResult<(CapDecl, Vec<FnDecl>)> {
        let pos = self.pos();
        self.bump(); // `capability`
        let mut path = vec![self.ident()?];
        while *self.peek() == Tok::Dot {
            self.bump();
            path.push(self.ident()?);
        }
        let shown = path.join(".");
        self.expect(Tok::LBrace, "'{'")?;
        let mut needs = Vec::new();
        let mut actions = Vec::new();
        let mut fns: Vec<FnDecl> = Vec::new();
        let mut canonical: Option<String> = None;
        loop {
            let wpos = self.pos();
            match self.peek().clone() {
                Tok::RBrace => {
                    self.bump();
                    break;
                }
                Tok::Ident(w) if w == "needs" => {
                    if !actions.is_empty() || canonical.is_some() {
                        return Err((wpos, "`needs` must come before `canonical` and every `action`".into()));
                    }
                    self.bump();
                    let npos = self.pos();
                    let name = self.ident()?;
                    self.expect(Tok::Colon, "':'")?;
                    let ty = self.ty()?;
                    needs.push((name, ty, npos));
                }
                Tok::Ident(w) if w == "canonical" => {
                    if canonical.is_some() {
                        return Err((wpos, format!("capability {shown} has two `canonical` functions")));
                    }
                    self.bump();
                    let name = format!("canonical {shown}");
                    let f = self.fn_rest(false, name.clone(), wpos)?;
                    canonical = Some(name);
                    fns.push(f);
                }
                Tok::Ident(w) if w == "action" => {
                    self.bump();
                    let aname = self.ident()?;
                    let fn_name = format!("action {shown}.{aname}");
                    let mut f = self.fn_rest(false, fn_name.clone(), wpos)?;
                    let declared = f.params.len();
                    let target = f.params.iter().position(|p| p.target);
                    let amount = f.params.iter().position(|p| p.amount);
                    // The needs follow the declared parameters: the runtime supplies them.
                    for (n, t, _) in &needs {
                        f.params.push(Param {
                            name: n.clone(),
                            ty: t.clone(),
                            trusted: false,
                            target: false,
                            amount: false,
                            blind: Vec::new(),
                            default: None,
                        });
                    }
                    actions.push(ActionDecl { name: aname, fn_name, declared, target, amount, pos: wpos });
                    fns.push(f);
                }
                other => {
                    return Err((wpos, format!("expected `needs`, `canonical`, `action` or '}}' in a capability, found {other:?}")))
                }
            }
        }
        Ok((CapDecl { path, needs, actions, canonical, pos }, fns))
    }

    /// The rest of a function after its name: parameters, return type, budget, body.
    fn fn_rest(&mut self, public: bool, name: String, pos: Pos) -> PResult<FnDecl> {
        self.expect(Tok::LParen, "'('")?;
        let mut params = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                let pname = self.ident()?;
                self.expect(Tok::Colon, "':'")?;
                let ty = self.ty()?;
                let m = self.param_marks()?;
                let default = if *self.peek() == Tok::Eq {
                    self.bump();
                    Some(self.with_records(true, Self::expr)?)
                } else {
                    None
                };
                params.push(Param {
                    name: pname,
                    ty,
                    trusted: m.trusted,
                    target: m.target,
                    amount: m.amount,
                    blind: m.blind,
                    default,
                });
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
        // `fails` is a word only here, so it stays usable as a name elsewhere.
        let fails = if matches!(self.peek(), Tok::Ident(w) if w == "fails") {
            self.bump();
            Some(self.ty()?)
        } else {
            None
        };
        let budget = if *self.peek() == Tok::Budget { Some(self.budget()?) } else { None };
        let body = self.block()?;
        Ok(FnDecl { public, name, params, ret, ret_trusted, budget, fails, body, pos })
    }

    fn ty(&mut self) -> PResult<Ty> {
        if *self.peek() == Tok::Cap {
            self.bump();
            let mut path = vec![self.ident()?];
            while *self.peek() == Tok::Dot {
                self.bump();
                path.push(self.ident()?);
            }
            let mut limits = CapLimits::default();
            if *self.peek() == Tok::LParen {
                self.bump();
                loop {
                    let kpos = self.pos();
                    let key = self.ident()?;
                    self.expect(Tok::Colon, "':'")?;
                    let dup = match key.as_str() {
                        "max" => limits.max.replace(self.money_limit()?).is_some(),
                        "each" => limits.each.replace(self.money_limit()?).is_some(),
                        "uses" => limits.uses.replace(self.num()?).is_some(),
                        "only" => limits.only.replace(self.patterns()?).is_some(),
                        _ => {
                            return Err((
                                kpos,
                                format!("unknown capability limit '{key}'; use 'max', 'each', 'uses' or 'only'"),
                            ))
                        }
                    };
                    if dup {
                        return Err((kpos, format!("capability limit '{key}' given twice")));
                    }
                    if *self.peek() == Tok::Comma {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.expect(Tok::RParen, "')'")?;
            }
            return Ok(Ty::Cap { path, limits });
        }
        // `(A, B, ...)`: a tuple type.
        if *self.peek() == Tok::LParen {
            let tpos = self.pos();
            self.bump();
            let mut items = vec![self.ty()?];
            while *self.peek() == Tok::Comma {
                self.bump();
                if *self.peek() == Tok::RParen {
                    break;
                }
                items.push(self.ty()?);
            }
            self.expect(Tok::RParen, "')'")?;
            if items.len() < 2 {
                return Err((tpos, "a tuple type has two or more elements, e.g. (Text, Int)".into()));
            }
            return Ok(Ty::Tuple(items));
        }
        let npos = self.pos();
        let name = self.ident()?;
        if *self.peek() == Tok::Lt {
            self.bump();
            let inner = self.ty()?;
            return match name.as_str() {
                "Proposed" => {
                    self.expect(Tok::Gt, "'>'")?;
                    Ok(Ty::Proposed(Box::new(inner)))
                }
                "List" => {
                    if *self.peek() != Tok::Comma {
                        return Err((
                            self.pos(),
                            "a list type needs a maximum length, e.g. List<Text, 100>: every list in Staal is bounded".into(),
                        ));
                    }
                    self.bump();
                    let max = self.num()?;
                    self.expect(Tok::Gt, "'>'")?;
                    Ok(Ty::List(Box::new(inner), max))
                }
                "Optional" => {
                    self.expect(Tok::Gt, "'>'")?;
                    Ok(Ty::Optional(Box::new(inner)))
                }
                "Map" => {
                    self.expect(Tok::Comma, "',' and the value type: Map<Text, Int, 100>")?;
                    let value = self.ty()?;
                    if *self.peek() != Tok::Comma {
                        return Err((self.pos(), "a map type needs a maximum size, e.g. Map<Text, Int, 100>: every map in Staal is bounded".into()));
                    }
                    self.bump();
                    let max = self.num()?;
                    self.expect(Tok::Gt, "'>'")?;
                    Ok(Ty::Map(Box::new(inner), Box::new(value), max))
                }
                "Set" => {
                    if *self.peek() != Tok::Comma {
                        return Err((self.pos(), "a set type needs a maximum size, e.g. Set<Text, 100>: every set in Staal is bounded".into()));
                    }
                    self.bump();
                    let max = self.num()?;
                    self.expect(Tok::Gt, "'>'")?;
                    Ok(Ty::Set(Box::new(inner), max))
                }
                _ => Err((npos, format!("unknown generic type '{name}'"))),
            };
        }
        if name == "List" {
            return Err((npos, "a list type needs an element type and a maximum length, e.g. List<Text, 100>".into()));
        }
        if name == "Optional" {
            return Err((npos, "an optional type needs the type it may hold, e.g. Optional<Text>".into()));
        }
        // `m.T`: a type from the module imported as `m`.
        if *self.peek() == Tok::Dot {
            self.bump();
            let t = self.ident()?;
            return Ok(Ty::Named(format!("{name}.{t}")));
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
            if *self.peek() == Tok::Let && self.toks.get(self.i + 1).map(|t| &t.tok) == Some(&Tok::LParen) {
                stmts.extend(self.let_unpack()?);
                continue;
            }
            stmts.push(self.stmt()?);
        }
        self.expect(Tok::RBrace, "'}'")?;
        Ok(stmts)
    }

    /// `let (a, b) = value`: the tuple in a variable of its own, which no program can
    /// name, then `let a = it.0`, `let b = it.1`. The names are ordinary variables.
    fn let_unpack(&mut self) -> PResult<Vec<Stmt>> {
        let pos = self.pos();
        self.bump(); // `let`
        self.expect(Tok::LParen, "'('")?;
        let mut names = Vec::new();
        loop {
            names.push((self.pos(), self.ident()?));
            if *self.peek() == Tok::Comma {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(Tok::RParen, "')'")?;
        if names.len() < 2 {
            return Err((pos, "`let (...)` unpacks a tuple of two or more values; for one, write `let x = ...`".into()));
        }
        let ty = if *self.peek() == Tok::Colon {
            self.bump();
            Some(self.ty()?)
        } else {
            None
        };
        self.expect(Tok::Eq, "'='")?;
        let value = self.expr()?;
        self.unpacked += 1;
        let holder = format!("(tuple {})", self.unpacked);
        let mut out = vec![Stmt::Let { name: holder.clone(), ty, value, pos }];
        for (i, (npos, name)) in names.into_iter().enumerate() {
            let base = Box::new(Expr::Var(holder.clone(), npos));
            out.push(Stmt::Let { name, ty: None, value: Expr::Field { base, field: i.to_string(), pos: npos }, pos: npos });
        }
        Ok(out)
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
            Tok::Continue => {
                self.bump();
                Ok(Stmt::Continue(pos))
            }
            Tok::If => self.if_stmt(),
            // `match` is a word only at the start of a statement, followed by a value:
            // `match.send(x)` (an action through a capability called `match`) is not one.
            Tok::Ident(w)
                if w == "match"
                    && !matches!(self.toks.get(self.i + 1).map(|t| &t.tok), Some(Tok::Dot | Tok::LParen | Tok::Eq)) =>
            {
                self.match_stmt()
            }
            Tok::Ident(w) if w == "fail" && self.toks.get(self.i + 1).map(|t| &t.tok) == Some(&Tok::LParen) => {
                self.bump();
                let mut args = self.args()?;
                if args.len() != 1 {
                    return Err((pos, "`fail` takes one message".into()));
                }
                Ok(Stmt::Fail(args.remove(0), pos))
            }
            Tok::Set => {
                self.bump();
                let name = self.ident()?;
                self.expect(Tok::Eq, "'='")?;
                let value = self.expr()?;
                Ok(Stmt::Set { name, value, pos })
            }
            Tok::For => {
                self.bump();
                // `for (k, v) in xs`: each item, a tuple, unpacked as `let (k, v)` would.
                if *self.peek() == Tok::LParen {
                    self.bump();
                    let mut names = Vec::new();
                    loop {
                        names.push((self.pos(), self.ident()?));
                        if *self.peek() == Tok::Comma {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    self.expect(Tok::RParen, "')'")?;
                    if names.len() < 2 {
                        return Err((pos, "`for (...)` unpacks tuples of two or more values; for one, write `for x in ...`".into()));
                    }
                    self.expect(Tok::In, "'in'")?;
                    let list = self.head_expr()?;
                    let mut body = self.block()?;
                    self.unpacked += 1;
                    let holder = format!("(tuple {})", self.unpacked);
                    let lets = names.into_iter().enumerate().map(|(i, (npos, name))| {
                        let base = Box::new(Expr::Var(holder.clone(), npos));
                        Stmt::Let { name, ty: None, value: Expr::Field { base, field: i.to_string(), pos: npos }, pos: npos }
                    });
                    body.splice(0..0, lets);
                    return Ok(Stmt::For { var: holder, list, body, pos });
                }
                let var = self.ident()?;
                self.expect(Tok::In, "'in'")?;
                let list = self.head_expr()?;
                let body = self.block()?;
                Ok(Stmt::For { var, list, body, pos })
            }
            _ => Ok(Stmt::Expr(self.expr()?)),
        }
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let pos = self.pos();
        self.expect(Tok::If, "'if'")?;
        let cond = self.head_expr()?;
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

    /// An expression in an `if` or `for` head: a record literal there needs parentheses.
    fn head_expr(&mut self) -> PResult<Expr> {
        self.with_records(false, Self::expr)
    }

    /// Runs `f` with record literals allowed or not. Inside any brackets, parentheses
    /// or braces they are allowed again: there the next `{` cannot open a block.
    fn with_records<T>(&mut self, allowed: bool, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let saved = std::mem::replace(&mut self.no_record, !allowed);
        let r = f(self);
        self.no_record = saved;
        r
    }

    fn expr(&mut self) -> PResult<Expr> {
        let mut left = self.and_expr()?;
        while *self.peek() == Tok::Or {
            let pos = self.pos();
            self.bump();
            let right = self.and_expr()?;
            left = Expr::Logic { op: LogicOp::Or, left: Box::new(left), right: Box::new(right), pos };
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut left = self.not_expr()?;
        while *self.peek() == Tok::And {
            let pos = self.pos();
            self.bump();
            let right = self.not_expr()?;
            left = Expr::Logic { op: LogicOp::And, left: Box::new(left), right: Box::new(right), pos };
        }
        Ok(left)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        if *self.peek() == Tok::Not {
            let pos = self.pos();
            self.bump();
            return Ok(Expr::Not(Box::new(self.not_expr()?), pos));
        }
        self.cmp_expr()
    }

    fn add_expr(&mut self) -> PResult<Expr> {
        let mut left = self.mul_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => ArithOp::Add,
                Tok::Minus => ArithOp::Sub,
                _ => return Ok(left),
            };
            let pos = self.pos();
            self.bump();
            let right = self.mul_expr()?;
            left = Expr::Arith { op, left: Box::new(left), right: Box::new(right), pos };
        }
    }

    fn mul_expr(&mut self) -> PResult<Expr> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => ArithOp::Mul,
                Tok::Slash => ArithOp::Div,
                _ => return Ok(left),
            };
            let pos = self.pos();
            self.bump();
            let right = self.unary()?;
            left = Expr::Arith { op, left: Box::new(left), right: Box::new(right), pos };
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        if *self.peek() == Tok::Minus {
            let pos = self.pos();
            self.bump();
            return Ok(Expr::Neg(Box::new(self.unary()?), pos));
        }
        self.primary()
    }

    fn cmp_expr(&mut self) -> PResult<Expr> {
        let left = self.add_expr()?;
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
        let right = self.add_expr()?;
        if matches!(
            self.peek(),
            Tok::EqEq | Tok::NotEq | Tok::Lt | Tok::LtEq | Tok::Gt | Tok::GtEq
        ) {
            return Err((self.pos(), "comparisons cannot be chained; compare two values at a time".into()));
        }
        Ok(Expr::Cmp { op, left: Box::new(left), right: Box::new(right), pos })
    }

    fn propose(&mut self) -> PResult<Expr> {
        self.with_records(true, Self::propose_inner)
    }

    fn propose_inner(&mut self) -> PResult<Expr> {
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
        self.with_records(true, Self::list_inner)
    }

    fn list_inner(&mut self) -> PResult<Vec<Expr>> {
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

    /// `[a, b]` a list, `[k: v, ...]` a map, `[:]` an empty map.
    fn list_or_map(&mut self, pos: Pos) -> PResult<Expr> {
        self.expect(Tok::LBracket, "'['")?;
        if *self.peek() == Tok::Colon {
            self.bump();
            self.expect(Tok::RBracket, "']'")?;
            return Ok(Expr::MapLit(Vec::new(), pos));
        }
        if *self.peek() == Tok::RBracket {
            self.bump();
            return Ok(Expr::List(Vec::new(), pos));
        }
        let first = self.expr()?;
        if *self.peek() == Tok::Colon {
            self.bump();
            let mut entries = vec![(first, self.expr()?)];
            while *self.peek() == Tok::Comma {
                self.bump();
                if *self.peek() == Tok::RBracket {
                    break;
                }
                let k = self.expr()?;
                self.expect(Tok::Colon, "':' between a key and its value")?;
                entries.push((k, self.expr()?));
            }
            self.expect(Tok::RBracket, "']'")?;
            return Ok(Expr::MapLit(entries, pos));
        }
        let mut items = vec![first];
        while *self.peek() == Tok::Comma {
            self.bump();
            if *self.peek() == Tok::RBracket {
                break;
            }
            items.push(self.expr()?);
        }
        self.expect(Tok::RBracket, "']'")?;
        Ok(Expr::List(items, pos))
    }

    /// An atom followed by any number of `.field` accesses and `[index]`es.
    fn primary(&mut self) -> PResult<Expr> {
        let mut e = self.atom()?;
        loop {
            match self.peek() {
                Tok::Dot => {
                    self.bump();
                    let pos = self.pos();
                    // `t.0`: an element of a tuple. `t.0.1` arrives as the number 0.1.
                    match self.peek().clone() {
                        Tok::Num(n) => {
                            self.bump();
                            e = Expr::Field { base: Box::new(e), field: n.to_string(), pos };
                            continue;
                        }
                        Tok::Decimal(d) if d.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) => {
                            self.bump();
                            for part in d.split('.') {
                                let n: u64 = part.parse().map_err(|_| (pos, format!("'{part}' is not a tuple position")))?;
                                e = Expr::Field { base: Box::new(e), field: n.to_string(), pos };
                            }
                            continue;
                        }
                        _ => {}
                    }
                    let field = self.field_name()?;
                    if *self.peek() == Tok::LParen {
                        return Err((
                            pos,
                            format!(
                                "'{field}(...)' is not allowed here: actions must be called directly on a capability variable"
                            ),
                        ));
                    }
                    e = Expr::Field { base: Box::new(e), field, pos };
                }
                Tok::LBracket => {
                    let pos = self.pos();
                    self.bump();
                    let index = self.with_records(true, Self::expr)?;
                    self.expect(Tok::RBracket, "']'")?;
                    e = Expr::Index { base: Box::new(e), index: Box::new(index), pos };
                }
                _ => return Ok(e),
            }
        }
    }

    /// `Type { field: value, ... }`, after the type name has been read.
    fn record_literal(&mut self, ty: String, pos: Pos) -> PResult<Expr> {
        self.expect(Tok::LBrace, "'{'")?;
        let fields = self.with_records(true, |p| {
            let mut fields = Vec::new();
            while *p.peek() != Tok::RBrace {
                let name = p.field_name()?;
                p.expect(Tok::Colon, "':'")?;
                let value = p.expr()?;
                fields.push((name, value));
                if *p.peek() == Tok::Comma {
                    p.bump();
                } else {
                    break;
                }
            }
            Ok(fields)
        })?;
        self.expect(Tok::RBrace, "'}'")?;
        Ok(Expr::Record { ty, fields, pos })
    }

    fn atom(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        match self.peek().clone() {
            Tok::LParen => {
                self.bump();
                let e = self.with_records(true, Self::expr)?;
                if *self.peek() != Tok::Comma {
                    self.expect(Tok::RParen, "')'")?;
                    return Ok(e);
                }
                // `(a, b, ...)`: a tuple.
                let mut items = vec![e];
                while *self.peek() == Tok::Comma {
                    self.bump();
                    if *self.peek() == Tok::RParen {
                        break; // trailing comma
                    }
                    items.push(self.with_records(true, Self::expr)?);
                }
                self.expect(Tok::RParen, "')'")?;
                Ok(Expr::Tuple(items, pos))
            }
            Tok::LBracket => self.with_records(true, |p| p.list_or_map(pos)),
            Tok::Propose => self.propose(),
            Tok::Validate => {
                self.bump();
                Ok(Expr::Validate(Box::new(self.primary()?), pos))
            }
            Tok::Try => {
                self.bump();
                Ok(Expr::Try(Box::new(self.atom()?), pos))
            }
            Tok::Done => {
                self.bump();
                self.expect(Tok::LParen, "'('")?;
                let kpos = self.pos();
                let key = self.ident()?;
                if key != "evidence" {
                    return Err((kpos, format!("expected 'evidence', found '{key}'")));
                }
                self.expect(Tok::Colon, "':'")?;
                let evidence = self.list()?;
                self.expect(Tok::RParen, "')'")?;
                Ok(Expr::Done { evidence, pos })
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
            Tok::Interp(parts) => {
                self.bump();
                interpolation(parts, pos)
            }
            Tok::Num(_) | Tok::Decimal(_) => self.number(pos),
            Tok::Ident(name) if name == "parse" && self.toks.get(self.i + 1).map(|t| &t.tok) == Some(&Tok::LParen) => {
                self.bump();
                self.with_records(true, |p| {
                    p.expect(Tok::LParen, "'('")?;
                    let ty = p.ty()?;
                    p.expect(Tok::Comma, "','")?;
                    let text = p.expr()?;
                    p.expect(Tok::RParen, "')'")?;
                    Ok(Expr::Parse { ty, text: Box::new(text), pos })
                })
            }
            Tok::Ident(name) if name == "none" => {
                self.bump();
                Ok(Expr::NoneLit(pos))
            }
            Tok::Ident(name) if name == "some" && self.toks.get(self.i + 1).map(|t| &t.tok) == Some(&Tok::LParen) => {
                self.bump();
                let mut args = self.args()?;
                if args.len() != 1 {
                    return Err((pos, "`some` takes exactly one value: some(x)".into()));
                }
                Ok(Expr::SomeOf(Box::new(args.remove(0)), pos))
            }
            Tok::Ident(name) => {
                self.bump();
                if *self.peek() == Tok::LParen {
                    let args = self.args()?;
                    if let Some(f) = Builtin::from_name(&name) {
                        return Ok(Expr::Builtin { f, args, pos });
                    }
                    return Ok(Expr::Call { name, args, slots: Vec::new(), pos });
                }
                if *self.peek() == Tok::LBrace && !self.no_record {
                    return self.record_literal(name, pos);
                }
                // `m.E.V { ... }`: variant V of enum E from the module imported as `m`.
                let tok_at = |p: &Self, k: usize| p.toks.get(p.i + k).map(|t| t.tok.clone());
                let twice_qualified = !self.no_record
                    && tok_at(self, 0) == Some(Tok::Dot)
                    && matches!(tok_at(self, 1), Some(Tok::Ident(_)))
                    && tok_at(self, 2) == Some(Tok::Dot)
                    && matches!(tok_at(self, 3), Some(Tok::Ident(_)))
                    && tok_at(self, 4) == Some(Tok::LBrace);
                if twice_qualified {
                    self.bump();
                    let a = self.ident()?;
                    self.bump();
                    let b = self.ident()?;
                    return self.record_literal(format!("{name}.{a}.{b}"), pos);
                }
                // `m.T { ... }`: a record of type T from the module imported as `m` (or
                // variant T of the enum `m`).
                let qualified_record = !self.no_record
                    && *self.peek() == Tok::Dot
                    && matches!(self.toks.get(self.i + 1).map(|t| &t.tok), Some(Tok::Ident(_)))
                    && matches!(self.toks.get(self.i + 2).map(|t| &t.tok), Some(Tok::LBrace));
                if qualified_record {
                    self.bump();
                    let t = self.ident()?;
                    return self.record_literal(format!("{name}.{t}"), pos);
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

    /// A whole number, or money: a number followed on the same line by a currency
    /// code. Money is checked exactly as money arriving from outside is (RUNTIME.md
    /// section 2): a supported currency, no more decimals than it has, no overflow.
    fn number(&mut self, pos: Pos) -> PResult<Expr> {
        use crate::interp::money::{Currency, Money};
        let tok = self.bump().tok;
        let next = &self.toks[self.i];
        let code = match &next.tok {
            Tok::Ident(c) if next.pos.line == pos.line && c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()) => {
                Some(c.clone())
            }
            _ => None,
        };
        let (text, code) = match (tok, code) {
            (Tok::Num(n), None) => return Ok(Expr::Int(n, pos)),
            (Tok::Num(n), Some(c)) => (n.to_string(), c),
            (Tok::Decimal(d), Some(c)) => (d, c),
            (Tok::Decimal(d), None) => {
                return Err((
                    pos,
                    format!("'{d}' needs a currency, e.g. `{d} USD`: decimal numbers are only for money, and Staal has no floating point"),
                ))
            }
            (other, _) => return Err((pos, format!("expected a number, found {other:?}"))),
        };
        let cpos = self.pos();
        self.bump();
        let currency = Currency::parse(&code).map_err(|e| (cpos, e))?;
        let m = Money::parse(&text, currency).map_err(|e| (pos, e))?;
        Ok(Expr::Money(m, pos))
    }

    /// "(" (expr ("," expr)*)? ")"
    fn args(&mut self) -> PResult<Vec<Expr>> {
        self.with_records(true, Self::args_inner)
    }

    fn args_inner(&mut self) -> PResult<Vec<Expr>> {
        self.expect(Tok::LParen, "'('")?;
        let mut args = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                // `name: value` names the parameter it is for.
                let named = matches!(self.peek(), Tok::Ident(_)) && self.toks.get(self.i + 1).map(|t| &t.tok) == Some(&Tok::Colon);
                if named {
                    let pos = self.pos();
                    let name = self.ident()?;
                    self.bump(); // `:`
                    let value = self.expr()?;
                    args.push(Expr::Named { name, value: Box::new(value), pos });
                } else {
                    args.push(self.expr()?);
                }
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

/// `"a \(x) b"` as `"a " + to_text(x) + " b"`: interpolation is joining text, so every
/// rule that applies to `+` on text (trust, blinding) applies to it unchanged.
fn interpolation(parts: Vec<crate::lexer::Part>, pos: Pos) -> PResult<Expr> {
    use crate::lexer::Part;
    let mut acc: Option<Expr> = None;
    for part in parts {
        let next = match part {
            Part::Text(t) => Expr::Str(t, pos),
            Part::Code(tokens) => {
                let mut p = Parser::new(tokens);
                let e = p.expr()?;
                if *p.peek() != Tok::Eof {
                    return Err((p.pos(), format!("unexpected {:?} in \\( )", p.peek())));
                }
                let at = e.pos();
                Expr::Builtin { f: Builtin::Text, args: vec![e], pos: at }
            }
        };
        acc = Some(match acc {
            // The first piece is text, so the whole is text even for "\(n)".
            None if matches!(next, Expr::Str(..)) => next,
            None => Expr::Arith { op: ArithOp::Add, left: Box::new(Expr::Str(String::new(), pos)), right: Box::new(next), pos },
            Some(left) => Expr::Arith { op: ArithOp::Add, left: Box::new(left), right: Box::new(next), pos },
        });
    }
    Ok(acc.unwrap_or(Expr::Str(String::new(), pos)))
}
