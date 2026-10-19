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
                if chars[i] == '\\' {
                    let at = Pos { file, line, col };
                    let (c, used) = escape(&chars[i + 1..]).map_err(|e| (at, e))?;
                    s.push(c);
                    i += 1 + used;
                    col += 1 + used;
                    continue;
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

/// The character a backslash escape stands for, and how many characters after the
/// backslash it used. `\(` is reserved for text interpolation.
fn escape(rest: &[char]) -> Result<(char, usize), String> {
    match rest.first() {
        Some('"') => Ok(('"', 1)),
        Some('\\') => Ok(('\\', 1)),
        Some('n') => Ok(('\n', 1)),
        Some('t') => Ok(('\t', 1)),
        Some('r') => Ok(('\r', 1)),
        Some('u') => {
            if rest.get(1) != Some(&'{') {
                return Err("a \\u escape is written \\u{...} with 1 to 6 hex digits, e.g. \\u{e9}".into());
            }
            let digits: String = rest[2..].iter().take_while(|c| **c != '}' && **c != '"' && **c != '\n').collect();
            if rest.get(2 + digits.chars().count()) != Some(&'}') {
                return Err("unterminated \\u{...} escape".into());
            }
            if digits.is_empty() || digits.len() > 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("\\u{{{digits}}} needs 1 to 6 hex digits"));
            }
            let n = u32::from_str_radix(&digits, 16).map_err(|e| e.to_string())?;
            let c = char::from_u32(n).ok_or(format!("\\u{{{digits}}} is not a Unicode character"))?;
            Ok((c, 3 + digits.chars().count()))
        }
        Some('(') => Err("\\( is reserved for text interpolation".into()),
        Some(c) => Err(format!("unknown escape \\{c}: the escapes are \\\" \\\\ \\n \\t \\r and \\u{{...}}")),
        None => Err("unterminated string".into()),
    }
}

#[cfg(test)]
mod escape_tests {
    use super::*;

    fn text(src: &str) -> Result<String, String> {
        match lex(src).map_err(|(_, e)| e)?.first().map(|t| t.tok.clone()) {
            Some(Tok::Str(s)) => Ok(s),
            other => Err(format!("not a string: {other:?}")),
        }
    }

    #[test]
    fn escapes_give_their_characters() {
        assert_eq!(text(r#""say \"hi\"""#).unwrap(), "say \"hi\"");
        assert_eq!(text(r#""a\\b""#).unwrap(), "a\\b");
        assert_eq!(text(r#""l1\nl2\tx\r""#).unwrap(), "l1\nl2\tx\r");
        assert_eq!(text(r#""caf\u{e9} \u{1F600}""#).unwrap(), "café 😀");
        assert_eq!(text(r#""{secret}""#).unwrap(), "{secret}", "braces stay ordinary characters");
    }

    #[test]
    fn bad_escapes_are_errors_with_positions() {
        for (src, want) in [
            (r#""\q""#, "unknown escape \\q"),
            (r#""\u{}""#, "1 to 6 hex digits"),
            (r#""\u{1234567}""#, "1 to 6 hex digits"),
            (r#""\u{d800}""#, "not a Unicode character"),
            (r#""\u{110000}""#, "not a Unicode character"),
            (r#""\u{zz}""#, "1 to 6 hex digits"),
            (r#""\u41""#, "written \\u{...}"),
            (r#""\u{41""#, "unterminated"),
            (r#""\(x)""#, "reserved for text interpolation"),
            ("\"ends\\", "unterminated string"),
        ] {
            let err = text(src).unwrap_err();
            assert!(err.contains(want), "{src}: {err}");
        }
        let (pos, _) = lex("let x = \"ab\\q\"").unwrap_err();
        assert_eq!((pos.line, pos.col), (1, 12), "the error points at the backslash");
    }

    #[test]
    fn positions_after_an_escape_stay_right() {
        let toks = lex("\"a\\u{e9}b\" x").unwrap();
        assert_eq!(toks[1].pos.col, 12);
    }
}
