// Lexer: turns Staal source text into tokens with line/column positions.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Str(String),
    Num(u64),
    /// Digits with a decimal point, kept as written: `12.50`. Only money uses them.
    Decimal(String),
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
    Try,
    Done,
    And,
    Or,
    Not,
    For,
    In,
    Import,
    Pub,
    Set,
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
    Plus,
    Minus,
    Star,
    Slash,
    Eof,
}

/// A place in the source: which file (an index into the program's file list, 0 for
/// the entry file), and the line and column there. Positions from different files
/// never compare equal, so analyses can key on them across a whole program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pos {
    pub file: u32,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub pos: Pos,
}

/// Lexes a single-file program (file 0).
impl Tok {
    /// The text of a keyword, which may also be used as a record field name: JSON from
    /// real services has fields such as "type" and "in".
    pub fn keyword_text(&self) -> Option<&'static str> {
        Some(match self {
            Tok::Fn => "fn",
            Tok::Type => "type",
            Tok::Let => "let",
            Tok::Return => "return",
            Tok::Cap => "cap",
            Tok::Propose => "propose",
            Tok::Validate => "validate",
            Tok::Budget => "budget",
            Tok::Repeat => "repeat",
            Tok::Loop => "loop",
            Tok::If => "if",
            Tok::Else => "else",
            Tok::Break => "break",
            Tok::True => "true",
            Tok::False => "false",
            Tok::Try => "try",
            Tok::Done => "done",
            Tok::And => "and",
            Tok::Or => "or",
            Tok::Not => "not",
            Tok::For => "for",
            Tok::In => "in",
            Tok::Import => "import",
            Tok::Pub => "pub",
            Tok::Set => "set",
            _ => return None,
        })
    }
}

pub fn lex(src: &str) -> Result<Vec<Token>, (Pos, String)> {
    lex_file(src, 0)
}

/// Lexes file number `file` of a program.
pub fn lex_file(src: &str, file: u32) -> Result<Vec<Token>, (Pos, String)> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let (mut line, mut col) = (1, 1);
    let mut out = Vec::new();

    while i < chars.len() {
        let c = chars[i];
        let pos = Pos { file, line, col };

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
            // `12.50`: a decimal point between digits. Kept as text, never a float.
            if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                col += i - start;
                out.push(Token { tok: Tok::Decimal(text), pos });
                continue;
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
                "try" => Tok::Try,
                "done" => Tok::Done,
                "and" => Tok::And,
                "or" => Tok::Or,
                "not" => Tok::Not,
                "for" => Tok::For,
                "in" => Tok::In,
                "import" => Tok::Import,
                "pub" => Tok::Pub,
                "set" => Tok::Set,
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
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            other => return Err((pos, format!("unexpected character '{other}'"))),
        };
        out.push(Token { tok, pos });
        i += 1;
        col += 1;
    }
    out.push(Token { tok: Tok::Eof, pos: Pos { file, line, col } });
    Ok(out)
}
