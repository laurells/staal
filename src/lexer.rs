// Lexer: turns Intent source text into tokens with line/column positions.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Str(String),
    Num(u64),
    // keywords
    Fn,
    Type,
    Let,
    Return,
    Cap,
    Propose,
    Validate,
    Budget,
    Repeat,
    Loop,
    If,
    Else,
    Break,
    True,
    False,
    // symbols
    LParen,
    RParen,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Dot,
    Arrow,
    Lt,
    Gt,
    Eq,
    EqEq,
    NotEq,
    LtEq,
    GtEq,
    At,
    LBracket,
    RBracket,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub pos: Pos,
}

pub fn lex(src: &str) -> Result<Vec<Token>, (Pos, String)> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let (mut line, mut col) = (1, 1);
    let mut out = Vec::new();

    while i < chars.len() {
        let c = chars[i];
        let pos = Pos { line, col };

        if c == '\n' {
            line += 1;
            col = 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            col += 1;
            continue;
        }
        // line comment
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '"' {
            let mut s = String::new();
            i += 1;
            col += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\n' {
                    return Err((pos, "unterminated string".into()));
                }
                s.push(chars[i]);
                i += 1;
                col += 1;
            }
            if i >= chars.len() {
                return Err((pos, "unterminated string".into()));
            }
            i += 1;
            col += 1;
            out.push(Token { tok: Tok::Str(s), pos });
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            col += i - start;
            let n = text
                .parse::<u64>()
                .map_err(|_| (pos, format!("number '{text}' is too large")))?;
            out.push(Token { tok: Tok::Num(n), pos });
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            col += i - start;
            let tok = match word.as_str() {
                "fn" => Tok::Fn,
                "type" => Tok::Type,
                "let" => Tok::Let,
                "return" => Tok::Return,
                "cap" => Tok::Cap,
                "propose" => Tok::Propose,
                "validate" => Tok::Validate,
                "budget" => Tok::Budget,
                "repeat" => Tok::Repeat,
                "loop" => Tok::Loop,
                "if" => Tok::If,
                "else" => Tok::Else,
                "break" => Tok::Break,
                "true" => Tok::True,
                "false" => Tok::False,
                _ => Tok::Ident(word),
            };
            out.push(Token { tok, pos });
            continue;
        }
        let two = match (c, chars.get(i + 1)) {
            ('-', Some('>')) => Some(Tok::Arrow),
            ('=', Some('=')) => Some(Tok::EqEq),
            ('!', Some('=')) => Some(Tok::NotEq),
            ('<', Some('=')) => Some(Tok::LtEq),
            ('>', Some('=')) => Some(Tok::GtEq),
            _ => None,
        };
        if let Some(tok) = two {
            out.push(Token { tok, pos });
            i += 2;
            col += 2;
            continue;
        }
        let tok = match c {
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            ':' => Tok::Colon,
            ',' => Tok::Comma,
            '.' => Tok::Dot,
            '<' => Tok::Lt,
            '>' => Tok::Gt,
            '=' => Tok::Eq,
            '@' => Tok::At,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            other => return Err((pos, format!("unexpected character '{other}'"))),
        };
        out.push(Token { tok, pos });
        i += 1;
        col += 1;
    }
    out.push(Token { tok: Tok::Eof, pos: Pos { line, col } });
    Ok(out)
}
