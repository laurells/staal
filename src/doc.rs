// `staalc doc`: a program's documentation, in Markdown, from its own text.
//
// Only a program that passes every check is documented, so nothing here describes
// code that cannot run. The entry file's declarations are all shown; of the modules
// it imports, what they make `pub` and their capabilities (named by path, never
// `pub`), which is all another module can use. A
// declaration's documentation is the block of `//` lines directly above it, with no
// blank line between; a block at the top of a file that is not directly above a
// declaration is the file's. What is shown of each declaration is its own text, laid
// out by the formatter: a function's signature without its body, a type, enum,
// interface or constant whole, and a capability's name with its functions'
// signatures.

use crate::lexer::{lex_with_spans, Tok, Token};

/// One documented declaration.
#[derive(Debug, PartialEq)]
pub struct Item {
    /// "type", "enum", "interface", "fn", "const", "capability" or "foreign".
    pub kind: &'static str,
    /// As written after its keyword: `Line`, `Line.cost`, `sms.termii`.
    pub name: String,
    pub public: bool,
    /// The declaration as shown.
    pub code: String,
    /// The `//` lines above it, without the slashes.
    pub doc: String,
}

/// The documentation of one file: its own, and its declarations', in order.
#[derive(Debug, PartialEq)]
pub struct FileDoc {
    pub doc: String,
    pub items: Vec<Item>,
}

/// Reads the declarations of a file that lexes.
pub fn file_doc(src: &str) -> Result<FileDoc, String> {
    let (toks, spans) = lex_with_spans(src).map_err(|(p, m)| format!("{}:{}: {m}", p.line, p.col))?;
    let toks = &toks[..spans.tokens.len()];
    let chars: Vec<char> = src.chars().collect();
    let line_of = |i: usize| chars[..i].iter().filter(|c| **c == '\n').count() + 1;
    let text = |a: usize, b: usize| chars[spans.tokens[a].0..spans.tokens[b].1].iter().collect::<String>();

    // Where each top-level declaration starts (its `pub`, if any).
    let mut starts = Vec::new();
    let mut depth = 0i64;
    for (i, t) in toks.iter().enumerate() {
        match t.tok {
            Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
            Tok::RParen | Tok::RBracket | Tok::RBrace => depth -= 1,
            _ => {}
        }
        if depth == 0 && starts_decl(toks, i) && !(i > 0 && toks[i - 1].tok == Tok::Pub) {
            starts.push(i);
        }
    }

    // Blocks of comments each alone on its line, on lines one after another:
    // (first line, last line, text).
    let mut blocks: Vec<(usize, usize, String)> = Vec::new();
    for &(a, b) in &spans.comments {
        let line_start = chars[..a].iter().rposition(|c| *c == '\n').map_or(0, |p| p + 1);
        if chars[line_start..a].iter().any(|c| !c.is_whitespace()) {
            continue; // after code on its line: a remark, not documentation
        }
        let body: String = chars[a + 2..b].iter().collect();
        let body = body.strip_prefix(' ').unwrap_or(&body).trim_end().to_string();
        let line = line_of(a);
        match blocks.last_mut() {
            Some((_, last, text)) if *last + 1 == line => {
                *last = line;
                text.push('\n');
                text.push_str(&body);
            }
            _ => blocks.push((line, line, body)),
        }
    }

    let mut items = Vec::new();
    let mut prev_end_line = 0;
    let mut first_decl_line = None;
    for (n, &s) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(toks.len(), |&e| e) - 1;
        let public = toks[s].tok == Tok::Pub;
        let k = if public { s + 1 } else { s };
        let decl_line = line_of(spans.tokens[s].0);
        let doc = blocks.iter().find(|b| b.1 + 1 == decl_line && b.0 > prev_end_line).map_or(String::new(), |b| b.2.clone());
        prev_end_line = line_of(spans.tokens[end].0);
        let Some((kind, name)) = kind_and_name(toks, k) else { continue };
        if kind == "import" {
            continue;
        }
        first_decl_line.get_or_insert(decl_line);
        let code = match kind {
            "fn" => text(s, before_body(toks, k, end)),
            "capability" => capability_text(toks, k, end, &text),
            _ => text(s, end),
        };
        let code = crate::fmt::format(&code).map(|c| c.trim_end().to_string()).unwrap_or(code);
        items.push(Item { kind, name, public, code, doc });
    }
    // The file's own: the first block, above all code, unless it documents the first
    // declaration.
    let first_token_line = toks.first().map_or(usize::MAX, |t| t.pos.line);
    let doc = match blocks.first() {
        Some((_, last, text)) if *last < first_token_line && Some(*last + 1) != first_decl_line => text.clone(),
        _ => String::new(),
    };
    Ok(FileDoc { doc, items })
}

fn starts_decl(toks: &[Token], i: usize) -> bool {
    match &toks[i].tok {
        Tok::Pub | Tok::Fn | Tok::Type | Tok::Import => true,
        Tok::Ident(w) => {
            matches!(w.as_str(), "enum" | "interface" | "const" | "capability" | "foreign")
                && matches!(toks.get(i + 1).map(|t| &t.tok), Some(Tok::Ident(_)))
                && (i == 0 || matches!(toks[i - 1].tok, Tok::RBrace | Tok::Pub) || toks[i - 1].pos.line < toks[i].pos.line)
        }
        _ => false,
    }
}

fn kind_and_name(toks: &[Token], k: usize) -> Option<(&'static str, String)> {
    let ident = |i: usize| match toks.get(i).map(|t| &t.tok) {
        Some(Tok::Ident(n)) => Some(n.clone()),
        _ => None,
    };
    // A dotted path from `i`: `sms.termii`, `Line.cost`.
    let path = |mut i: usize| {
        let mut name = ident(i)?;
        while toks.get(i + 1).map(|t| &t.tok) == Some(&Tok::Dot) {
            i += 2;
            name = format!("{name}.{}", ident(i)?);
        }
        Some(name)
    };
    match &toks[k].tok {
        Tok::Fn => Some(("fn", path(k + 1)?)),
        Tok::Type => Some(("type", ident(k + 1)?)),
        Tok::Import => Some(("import", String::new())),
        Tok::Ident(w) => {
            let kind = match w.as_str() {
                "enum" => "enum",
                "interface" => "interface",
                "const" => "const",
                "capability" => "capability",
                "foreign" => "foreign",
                _ => return None,
            };
            Some((kind, if kind == "capability" || kind == "foreign" { path(k + 1)? } else { ident(k + 1)? }))
        }
        _ => None,
    }
}

/// The last token of a function's signature: the one before its body's `{`.
fn before_body(toks: &[Token], from: usize, end: usize) -> usize {
    let mut depth = 0i64;
    for (i, t) in toks.iter().enumerate().take(end + 1).skip(from) {
        match t.tok {
            Tok::LParen | Tok::LBracket => depth += 1,
            Tok::RParen | Tok::RBracket => depth -= 1,
            Tok::LBrace if depth == 0 => return i - 1,
            _ => {}
        }
    }
    end
}

/// `capability path {`, each of its `needs` (what the operator must grant it), and the
/// signatures of its `canonical` and its actions.
fn capability_text(toks: &[Token], k: usize, end: usize, text: &dyn Fn(usize, usize) -> String) -> String {
    let Some(open) = (k..=end).find(|&i| toks[i].tok == Tok::LBrace) else { return text(k, end) };
    // Each member starts with its word, first on its line, in the capability's braces.
    let mut members = Vec::new();
    let mut depth = 0i64;
    for i in open..=end {
        match &toks[i].tok {
            Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
            Tok::RParen | Tok::RBracket | Tok::RBrace => depth -= 1,
            Tok::Ident(w) if depth == 1 && matches!(w.as_str(), "needs" | "canonical" | "action") && toks[i - 1].pos.line < toks[i].pos.line => {
                members.push((i, w.as_str() == "needs"))
            }
            _ => {}
        }
    }
    // The capability's closing brace.
    let close = (open..=end).rev().find(|&i| toks[i].tok == Tok::RBrace).unwrap_or(end);
    let mut out = format!("{} {{\n", text(k, open - 1));
    for (n, &(m, needs)) in members.iter().enumerate() {
        let next = members.get(n + 1).map_or(close, |&(e, _)| e) - 1;
        let last = if needs { next } else { before_body(toks, m, next) };
        out.push_str(&text(m, last));
        out.push('\n');
    }
    out.push('}');
    out
}

/// The Markdown documentation of a checked program: its entry file, then each module
/// it imports.
pub fn document(files: &[crate::SourceFile]) -> Result<String, String> {
    let mut out = String::new();
    for (i, f) in files.iter().enumerate() {
        let d = file_doc(&f.text).map_err(|e| format!("{}:{e}", f.name))?;
        // A capability is never `pub`: it is named by its path, from any module.
        let items: Vec<&Item> = d.items.iter().filter(|it| i == 0 || it.public || it.kind == "capability" || it.kind == "foreign").collect();
        if i == 0 {
            out.push_str(&format!("# {}\n", f.name));
        } else {
            out.push_str(&format!("\n# Module {}\n", f.name));
        }
        if !d.doc.is_empty() {
            out.push_str(&format!("\n{}\n", d.doc));
        }
        for (title, kinds) in [
            ("Types", &["type", "enum", "interface"][..]),
            ("Constants", &["const"][..]),
            ("Functions", &["fn"][..]),
            ("Capabilities", &["capability"][..]),
            ("C libraries (unchecked)", &["foreign"][..]),
        ] {
            let these: Vec<&&Item> = items.iter().filter(|it| kinds.contains(&it.kind)).collect();
            if these.is_empty() {
                continue;
            }
            out.push_str(&format!("\n## {title}\n"));
            if kinds == ["foreign"] {
                out.push_str(
                    "\nC code can do anything the process can, so the runtime cannot check these functions. \
                     A run granted any of them is marked unchecked.\n",
                );
            }
            for it in these {
                out.push_str(&format!("\n### `{}`\n\n```staal\n{}\n```\n", it.name, it.code));
                if !it.doc.is_empty() {
                    out.push_str(&format!("\n{}\n", it.doc));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::file_doc;

    #[test]
    fn documentation_is_the_block_directly_above_and_nothing_else() {
        // A block at the top that runs into the first declaration is that
        // declaration's, not the file's.
        let d = file_doc("// Doubles.\n// Exactly.\nfn f(n: Int) -> Int {\n    return n * 2\n}\n").unwrap();
        assert_eq!((d.doc.as_str(), d.items[0].doc.as_str()), ("", "Doubles.\nExactly."));
        // With a blank line between, it is the file's.
        let d = file_doc("// The file.\n\nfn f() {\n}\n").unwrap();
        assert_eq!((d.doc.as_str(), d.items[0].doc.as_str()), ("The file.", ""));
        // A remark after code is no one's documentation, even on the line above.
        let d = file_doc("const A = 1 // one\nconst B = 2\n").unwrap();
        assert_eq!(d.items.iter().map(|i| (i.name.as_str(), i.doc.as_str())).collect::<Vec<_>>(), [("A", ""), ("B", "")]);
        // An indented block above is still the declaration's; `//` alone is a line.
        let d = file_doc("fn a() {\n}\n\n  // Second.\n  //\n  // More.\nfn b() {\n}\n").unwrap();
        assert_eq!(d.items[1].doc, "Second.\n\nMore.");
        // Comments only: the file's documentation, no items.
        let d = file_doc("// Just this.\n").unwrap();
        assert_eq!((d.doc.as_str(), d.items.len()), ("Just this.", 0));
    }
}
