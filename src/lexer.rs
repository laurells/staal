// Lexer: turns Staal source text into tokens with line/column positions.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Str(String),
    /// Text with at least one `\(expr)` in it: literal pieces and the tokens of each
    /// expression, in order.
    Interp(Vec<Part>),
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

/// A piece of interpolated text.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    /// The tokens of one `\(expr)`, ending with Eof, at their place in the source.
    Code(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq)]
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
    lex_chars(&chars, file, 1, 1)
}

/// Lexes `chars`, the first of which is at `line`, `col` of `file`.
fn lex_chars(chars: &[char], file: u32, line: usize, col: usize) -> Result<Vec<Token>, (Pos, String)> {
    let mut i = 0;
    let (mut line, mut col) = (line, col);
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
            let tok = text(chars, &mut i, &mut line, &mut col, file)?;
            out.push(Token { tok, pos });
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

/// Lexes a text literal starting at `chars[*i]`, a `"`: one line in `"..."`, or
/// several in `"""`. Leaves `*i` after the closing quote.
fn text(chars: &[char], i: &mut usize, line: &mut usize, col: &mut usize, file: u32) -> Result<Tok, (Pos, String)> {
    let start = Pos { file, line: *line, col: *col };
    let mut parts = Vec::new();
    let mut cur = String::new();
    if chars.get(*i + 1) == Some(&'"') && chars.get(*i + 2) == Some(&'"') {
        multiline(chars, i, line, col, file, start, &mut parts, &mut cur)?;
    } else {
        *i += 1;
        *col += 1;
        loop {
            match chars.get(*i) {
                None | Some('\n') => return Err((start, "unterminated string".into())),
                Some('"') => break,
                _ => piece(chars, i, *line, col, file, &mut parts, &mut cur)?,
            }
        }
        *i += 1;
        *col += 1;
    }
    if parts.is_empty() {
        return Ok(Tok::Str(cur));
    }
    if !cur.is_empty() {
        parts.push(Part::Text(cur));
    }
    Ok(Tok::Interp(parts))
}

/// Text in `"""` quotes. The opening quotes end their line; the closing quotes
/// stand on a line of their own, and their indentation is removed from every line
/// (a line indented less is an error). The line breaks between lines are kept; the
/// one before the closing quotes is not. Code may follow the closing quotes.
#[allow(clippy::too_many_arguments)]
fn multiline(
    chars: &[char],
    i: &mut usize,
    line: &mut usize,
    col: &mut usize,
    file: u32,
    start: Pos,
    parts: &mut Vec<Part>,
    cur: &mut String,
) -> Result<(), (Pos, String)> {
    *i += 3;
    *col += 3;
    while matches!(chars.get(*i), Some(' ' | '\t')) {
        *i += 1;
        *col += 1;
    }
    if chars.get(*i) == Some(&'\r') && chars.get(*i + 1) == Some(&'\n') {
        *i += 1;
    }
    if chars.get(*i) != Some(&'\n') {
        return Err((Pos { file, line: *line, col: *col }, "text after \"\"\" begins on the next line".into()));
    }
    // The lines up to the closing quotes, each as (index of its first character, length).
    let mut lines = Vec::new();
    let mut at = *i + 1;
    let indent = loop {
        if at >= chars.len() {
            return Err((start, "unterminated \"\"\" text".into()));
        }
        let end = chars[at..].iter().position(|c| *c == '\n').map_or(chars.len(), |n| at + n);
        let ws = chars[at..end].iter().take_while(|c| **c == ' ' || **c == '\t').count();
        if chars[at + ws..end].starts_with(&['"', '"', '"']) {
            break (at, ws, end);
        }
        // A line ending of \r\n ends the line just as \n does.
        let len = if end > at && chars[end - 1] == '\r' { end - at - 1 } else { end - at };
        lines.push((at, len));
        at = end + 1;
    };
    let (close_at, width, _) = indent;
    let margin = &chars[close_at..close_at + width];
    for (n, (first, len)) in lines.iter().enumerate() {
        *line += 1;
        *col = 1;
        if n > 0 {
            cur.push('\n');
        }
        let body = &chars[*first..*first + len];
        if body.iter().all(|c| *c == ' ' || *c == '\t') {
            continue;
        }
        if !body.starts_with(margin) {
            return Err((Pos { file, line: *line, col: 1 }, "this line is indented less than the closing \"\"\"".into()));
        }
        let mut j = *first + width;
        *col += width;
        while j < *first + len {
            piece(chars, &mut j, *line, col, file, parts, cur)?;
        }
    }
    *line += 1;
    *col = width + 4;
    // What follows the closing quotes on their line is ordinary code: `\"\"\")`.
    *i = close_at + width + 3;
    Ok(())
}

/// One character, escape or interpolation of a text literal at `chars[*i]`.
fn piece(
    chars: &[char],
    i: &mut usize,
    line: usize,
    col: &mut usize,
    file: u32,
    parts: &mut Vec<Part>,
    cur: &mut String,
) -> Result<(), (Pos, String)> {
    let at = Pos { file, line, col: *col };
    if chars[*i] != '\\' {
        cur.push(chars[*i]);
        *i += 1;
        *col += 1;
        return Ok(());
    }
    if chars.get(*i + 1) == Some(&'(') {
        let open = *i + 2;
        let close = interpolation_end(chars, open).ok_or((at, "unterminated \\( in text: close it with ) on the same line".to_string()))?;
        let tokens = lex_chars(&chars[open..close], file, line, *col + 2)?;
        if tokens.len() == 1 {
            return Err((at, "empty \\() in text".into()));
        }
        if !cur.is_empty() {
            parts.push(Part::Text(std::mem::take(cur)));
        }
        parts.push(Part::Code(tokens));
        *col += close + 1 - *i;
        *i = close + 1;
        return Ok(());
    }
    let (c, used) = escape(&chars[*i + 1..]).map_err(|e| (at, e))?;
    cur.push(c);
    *i += 1 + used;
    *col += 1 + used;
    Ok(())
}

/// The index of the `)` that closes an interpolation whose contents start at `open`,
/// skipping parentheses and text literals inside it; None if the line ends first.
fn interpolation_end(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = open;
    while let Some(&c) = chars.get(j) {
        match c {
            '\n' => return None,
            '(' => depth += 1,
            ')' if depth == 0 => return Some(j),
            ')' => depth -= 1,
            '"' => {
                j += 1;
                while let Some(&d) = chars.get(j) {
                    match d {
                        '\n' => return None,
                        '"' => break,
                        '\\' => j += 1,
                        _ => {}
                    }
                    j += 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// The character a backslash escape stands for, and how many characters after the
/// backslash it used. `\(` starts an interpolation and is handled before this.
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
            (r#""\()""#, "empty \\()"),
            (r#""\(x""#, "unterminated \\("),
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

#[cfg(test)]
mod text_tests {
    use super::*;

    fn one(src: &str) -> Result<Tok, String> {
        lex(src).map_err(|(p, e)| format!("{}:{}: {e}", p.line, p.col)).map(|t| t[0].tok.clone())
    }

    fn code_cols(t: &Tok) -> Vec<(usize, usize)> {
        let Tok::Interp(parts) = t else { panic!("not interpolated: {t:?}") };
        parts
            .iter()
            .filter_map(|p| match p {
                Part::Code(toks) => Some((toks[0].pos.line, toks[0].pos.col)),
                Part::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn multiline_text_drops_the_closing_indent() {
        let src = "let x = \"\"\"\n    Dear \\\"customer\\\",\n\n      thanks.\n    \"\"\"\nlet y = 1";
        assert_eq!(one(src.split("let x = ").nth(1).unwrap()).unwrap(), Tok::Str("Dear \"customer\",\n\n  thanks.".into()));
        let toks = lex(src).unwrap();
        let y = toks.iter().position(|t| t.tok == Tok::Ident("y".into())).unwrap();
        assert_eq!((toks[y].pos.line, toks[y].pos.col), (6, 5), "lines after the text are counted right");
    }

    #[test]
    fn multiline_rules_are_enforced() {
        for (src, want) in [
            ("\"\"\"text\n\"\"\"", "begins on the next line"),
            ("\"\"\"\n  a\n b\n  \"\"\"", "3:1: this line is indented less"),
            ("\"\"\"\n  a\n", "unterminated \"\"\""),
        ] {
            let err = one(src).unwrap_err();
            assert!(err.contains(want), "{src:?}: {err}");
        }
        assert_eq!(one("\"\"\"\r\n  a\r\n  b\r\n  \"\"\"").unwrap(), Tok::Str("a\nb".into()), "CRLF lines");
        assert_eq!(one("\"\"\"\n\"\"\"").unwrap(), Tok::Str(String::new()));
        let toks = lex("f(\"\"\"\n  a\n  \"\"\")").unwrap();
        assert_eq!(toks[3].tok, Tok::RParen, "code may follow the closing quotes");
        assert_eq!((toks[3].pos.line, toks[3].pos.col), (3, 6));
    }

    #[test]
    fn interpolation_is_lexed_in_place() {
        let t = one(r#""Hi \(name), you owe \(total(a, b))!""#).unwrap();
        let Tok::Interp(parts) = &t else { panic!() };
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], Part::Text("Hi ".into()));
        assert_eq!(parts[4], Part::Text("!".into()));
        assert_eq!(code_cols(&t), vec![(1, 7), (1, 24)], "tokens keep their real columns");
        // Parentheses and text inside an interpolation do not end it.
        let t = one(r#""a \(f("x)", (1))) b""#).unwrap();
        let Tok::Interp(parts) = &t else { panic!() };
        assert_eq!(parts.len(), 3);
        // A parenthesis inside quoted text within \( ) does not count.
        let t = one(r#""a \(f(")")) b""#).unwrap();
        let Tok::Interp(parts) = &t else { panic!() };
        let Part::Code(code) = &parts[1] else { panic!() };
        let kinds: Vec<Tok> = code.iter().map(|t| t.tok.clone()).collect();
        assert_eq!(kinds, vec![Tok::Ident("f".into()), Tok::LParen, Tok::Str(")".into()), Tok::RParen, Tok::Eof]);
        assert_eq!(parts[2], Part::Text(" b".into()));
        // Interpolation in multi-line text, on its line and column.
        assert_eq!(code_cols(&one("\"\"\"\n    one\n    is \\(n)\n    \"\"\"").unwrap()), vec![(3, 10)]);
        // Errors inside point into the text.
        assert!(one(r#""x \(a $ b)""#).unwrap_err().starts_with("1:8:"), "{}", one(r#""x \(a $ b)""#).unwrap_err());
        assert!(one("\"a \\(b\nc)\"").unwrap_err().contains("unterminated \\("));
    }
}
