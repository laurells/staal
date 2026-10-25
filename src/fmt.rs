// The formatter (`staalc fmt`): one layout for every Staal file.
//
// It works on tokens, never on the syntax tree, and changes only the whitespace
// between them: every token is copied exactly as written (text literals with their
// escapes and interpolations, multi-line text with its lines), every comment is
// kept, and a line break between two tokens stays a line break. So it cannot change
// what a program means. What it does:
//   - indents four spaces for each bracket ( [ { left open by an earlier line, and
//     one level more for a line that continues an expression (it starts or the line
//     before ends with an operator);
//   - puts one space, or none, between the tokens of a line, by the rules in `space`;
//   - keeps at most one blank line in a row, none after an opening `{` or before a
//     closing `}`, none at the start, and ends the file with one line break;
//   - removes trailing spaces (never inside a text literal).
// Before giving its result it lexes it again and refuses unless the tokens, the
// comments and the line breaks between tokens are exactly those of the input.

use crate::lexer::{lex_with_spans, Tok, Token};

const INDENT: &str = "    ";

/// The text of `src` formatted, or why it cannot be: it does not lex (with the
/// lexer's position and message), or formatting would not be safe.
pub fn format(src: &str) -> Result<String, String> {
    let (toks, spans) = lex_with_spans(src).map_err(|(p, m)| format!("{}:{}: {m}", p.line, p.col))?;
    let chars: Vec<char> = src.chars().collect();
    let out = layout(&chars, &toks, &spans.tokens, &spans.comments);
    verify(src, &out)?;
    Ok(out)
}

/// One token or comment, in the order they appear.
struct Item<'a> {
    start: usize,
    end: usize,
    /// None for a comment.
    tok: Option<&'a Tok>,
}

fn layout(chars: &[char], toks: &[Token], tspans: &[(usize, usize)], cspans: &[(usize, usize)]) -> String {
    // The line of each character, counted from 1.
    let mut line_of = Vec::with_capacity(chars.len() + 1);
    let mut line = 1;
    for c in chars {
        line_of.push(line);
        if *c == '\n' {
            line += 1;
        }
    }
    line_of.push(line);
    let mut items: Vec<Item> = tspans.iter().zip(toks).map(|(&(s, e), t)| Item { start: s, end: e, tok: Some(&t.tok) }).collect();
    items.extend(cspans.iter().map(|&(s, e)| Item { start: s, end: e, tok: None }));
    items.sort_by_key(|i| i.start);

    let mut out = String::new();
    // For each bracket still open: the indentation level of the line it is on.
    let mut open: Vec<usize> = Vec::new();
    // The level brackets opened on this line count from: the line's own, or lower when
    // the line first closes a bracket an earlier line opened (`texts: T) -> R {`, whose
    // brace counts from the line that opened the parameters).
    let mut base = 0;
    // The last token written, the token before it, and whether that `-` was unary.
    let mut last: Option<&Tok> = None;
    let mut before_last: Option<&Tok> = None;
    // The last four tokens written, and how many type brackets `<` are open.
    let mut history: Vec<&Tok> = Vec::new();
    let mut angles = 0usize;
    let mut last_angle = false;
    let mut prev_end_line = 0;
    let mut first = true;
    for item in &items {
        let start_line = line_of[item.start];
        let text: String = chars[item.start..item.end].iter().collect();
        if first || start_line != prev_end_line {
            if !first {
                // A blank line, if there was one, except just inside a block's braces.
                let gap = start_line - prev_end_line;
                let after_open = matches!(last, Some(Tok::LBrace | Tok::LParen | Tok::LBracket)) && !last_was_comment(&out);
                let before_close = matches!(item.tok, Some(Tok::RBrace | Tok::RParen | Tok::RBracket));
                out.push('\n');
                if gap > 1 && !after_open && !before_close {
                    out.push('\n');
                }
            }
            let level = match item.tok {
                Some(Tok::RBrace | Tok::RParen | Tok::RBracket) => open.last().copied().unwrap_or(0),
                _ => {
                    let base = open.last().map_or(0, |l| l + 1);
                    let continues = !first && (ends_expression(last) || item.tok.is_some_and(starts_continuation));
                    base + usize::from(continues)
                }
            };
            for _ in 0..level {
                out.push_str(INDENT);
            }
            base = level;
        } else if let Some(prev) = last {
            let unary_before = prev == &Tok::Minus && !before_last.is_some_and(is_operand);
            match item.tok {
                None => out.push(' '),
                Some(t) => {
                    let this_angle = type_angle(t, &history, angles);
                    if space(prev, t, unary_before, (last_angle, this_angle)) {
                        out.push(' ');
                    }
                }
            }
        } else {
            out.push(' ');
        }
        // A comment keeps its text, without trailing spaces.
        if item.tok.is_none() {
            out.push_str(text.trim_end());
        } else {
            out.push_str(&text);
        }
        if let Some(t) = item.tok {
            last_angle = type_angle(t, &history, angles);
            match t {
                Tok::Lt if last_angle => angles += 1,
                Tok::Gt if last_angle => angles -= 1,
                _ => {}
            }
            history.push(t);
            if history.len() > 4 {
                history.remove(0);
            }
            match t {
                Tok::LBrace | Tok::LParen | Tok::LBracket => open.push(base),
                Tok::RBrace | Tok::RParen | Tok::RBracket => {
                    if let Some(l) = open.pop() {
                        base = base.min(l);
                    }
                }
                _ => {}
            }
            before_last = last;
            last = Some(t);
        }
        prev_end_line = line_of[item.end];
        first = false;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Whether the text written so far ends in a comment (so the last token is not last
/// on its line).
fn last_was_comment(out: &str) -> bool {
    out.lines().last().is_some_and(|l| l.trim_start().starts_with("//"))
}

/// A token that ends an operand: a `-` after it is subtraction.
fn is_operand(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Ident(_) | Tok::Num(_) | Tok::Decimal(_) | Tok::Str(_) | Tok::Interp(_) | Tok::True | Tok::False | Tok::RParen | Tok::RBracket | Tok::RBrace
    )
}

fn binary(t: &Tok) -> bool {
    matches!(
        t,
        Tok::Plus | Tok::Star | Tok::Slash | Tok::EqEq | Tok::NotEq | Tok::LtEq | Tok::GtEq | Tok::Eq | Tok::Arrow | Tok::FatArrow | Tok::And | Tok::Or
    )
}

/// A line whose last token is one of these goes on into the next line.
fn ends_expression(last: Option<&Tok>) -> bool {
    last.is_some_and(|t| (binary(t) && *t != Tok::FatArrow) || matches!(t, Tok::Minus | Tok::Dot))
}

/// A line that starts with one of these goes on from the line before.
fn starts_continuation(t: &Tok) -> bool {
    (binary(t) && !matches!(t, Tok::Eq | Tok::FatArrow)) || matches!(t, Tok::Dot)
}

/// Whether `t` is a type bracket, not a comparison: a `<` after `List`, `Optional`,
/// `Map`, `Set` or `Proposed`, or after a function's name where it is declared
/// (`fn f<T>`, `fn Line.f<T>`); and a `>` while such a `<` is open. Only spacing
/// depends on this: a wrong answer cannot change a token.
fn type_angle(t: &Tok, history: &[&Tok], open: usize) -> bool {
    match t {
        Tok::Gt => open > 0,
        Tok::Lt => {
            let n = history.len();
            let at = |k: usize| (k <= n).then(|| history[n - k]);
            let type_name = matches!(at(1), Some(Tok::Ident(w)) if ["List", "Optional", "Map", "Set", "Proposed"].contains(&w.as_str()));
            let fn_name = matches!((at(2), at(1)), (Some(Tok::Fn), Some(Tok::Ident(_))))
                || matches!((at(4), at(3), at(2), at(1)), (Some(Tok::Fn), Some(Tok::Ident(_)), Some(Tok::Dot), Some(Tok::Ident(_))));
            type_name || fn_name
        }
        _ => false,
    }
}

/// Whether one space goes between `a` and `b` on a line. `unary` says `a` is a unary
/// minus; `angles` whether `a` and `b` are type brackets.
fn space(a: &Tok, b: &Tok, unary: bool, angles: (bool, bool)) -> bool {
    use Tok::*;
    // Type brackets hold their contents close: `List<Text, 100>`, `fn f<T>(x: T)`.
    match angles {
        (_, true) => return false,
        (true, _) if *a == Lt => return false,
        (true, _) => return !matches!(b, LParen | Comma | RParen | RBracket | Gt | Dot | Colon),
        _ => {}
    }
    if unary {
        return false;
    }
    match (a, b) {
        (_, Comma | RParen | RBracket | Dot | Colon) => false,
        (LParen | LBracket | Dot | At, _) => false,
        (LBrace, RBrace) => false,
        // A call: of a function, of what a call gave, of `fn(...) { ... }` itself.
        (_, LParen) => !matches!(a, Ident(_) | RParen | RBracket | RBrace | Fn | Propose | Done | Budget),
        (_, LBracket) => !matches!(a, Ident(_) | RParen | RBracket | Str(_) | Interp(_)),
        _ => true,
    }
}

/// A token without positions: the tokens inside an interpolation carry theirs, which
/// move when a line is indented.
#[derive(Debug, PartialEq)]
enum Shape {
    Plain(String),
    Interp(Vec<(Option<String>, Vec<Shape>)>),
}

fn shape_of(t: &Tok) -> Shape {
    match t {
        Tok::Interp(parts) => Shape::Interp(
            parts
                .iter()
                .map(|p| match p {
                    crate::lexer::Part::Text(s) => (Some(s.clone()), Vec::new()),
                    crate::lexer::Part::Code(ts) => (None, ts.iter().map(|t| shape_of(&t.tok)).collect()),
                })
                .collect(),
        ),
        other => Shape::Plain(format!("{other:?}")),
    }
}

/// What the check compares: the tokens, whether a line break comes between each two,
/// and the comments.
type Layout = (Vec<Shape>, Vec<bool>, Vec<String>);

/// Refuses `out` unless it has exactly the tokens and comments of `src`, with a line
/// break between two tokens exactly where `src` has one.
fn verify(src: &str, out: &str) -> Result<(), String> {
    let shape = |text: &str| -> Result<Layout, String> {
        let (toks, spans) = lex_with_spans(text).map_err(|(p, m)| format!("{}:{}: {m}", p.line, p.col))?;
        let chars: Vec<char> = text.chars().collect();
        let breaks = spans.tokens.windows(2).map(|w| chars[w[0].1..w[1].0].contains(&'\n')).collect();
        let comments = spans.comments.iter().map(|&(s, e)| chars[s..e].iter().collect::<String>().trim_end().to_string()).collect();
        Ok((toks.iter().map(|t| shape_of(&t.tok)).collect(), breaks, comments))
    };
    let (a, b) = (shape(src)?, shape(out).map_err(|e| format!("internal: the formatted text does not lex ({e})"))?);
    if a.0 != b.0 {
        return Err("internal: formatting would change the tokens; the file is left as it is".into());
    }
    if a.1 != b.1 {
        return Err("internal: formatting would move a line break between tokens; the file is left as it is".into());
    }
    if a.2 != b.2 {
        return Err("internal: formatting would change a comment; the file is left as it is".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::verify;

    /// The last check refuses any output that is not the input with other whitespace.
    #[test]
    fn the_check_refuses_anything_but_a_change_of_whitespace() {
        let src = "fn f() -> Int { // why\n    return 1 +\n        2\n}\n";
        assert!(verify(src, "fn f() -> Int {   // why\n return 1 +\n2\n}\n").is_ok());
        // A token changed, added or lost.
        assert!(verify(src, "fn f() -> Int { // why\n    return 1 -\n        2\n}\n").is_err());
        assert!(verify(src, "fn f() -> Int { // why\n    return 1 +\n        2 + 0\n}\n").is_err());
        // A line break between two tokens moved, added or removed.
        assert!(verify(src, "fn f() -> Int { // why\n    return 1 + 2\n}\n").is_err());
        assert!(verify(src, "fn f() -> Int { // why\n    return\n 1 +\n        2\n}\n").is_err());
        // A comment changed or lost.
        assert!(verify(src, "fn f() -> Int { // who\n    return 1 +\n        2\n}\n").is_err());
        assert!(verify(src, "fn f() -> Int {\n    return 1 +\n        2\n}\n").is_err());
        // Text inside a literal is a token: changing its spaces is refused.
        assert!(verify("let t = \"a  b\"\n", "let t = \"a b\"\n").is_err());
    }
}
