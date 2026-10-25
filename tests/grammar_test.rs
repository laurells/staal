// docs/GRAMMAR.md against the compiler. The document is the grammar people read and
// tools are built from, so it must not drift from what staalc parses:
//   - its reserved words are exactly the lexer's keywords;
//   - every contextual word the parser tests for is named in it;
//   - its built-in functions are exactly the compiler's;
//   - every example in it parses.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn read(rel: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

/// The indented block after the paragraph starting with `lead` in the document.
fn block_after<'a>(doc: &'a str, lead: &str) -> Vec<&'a str> {
    let start = doc.find(lead).unwrap_or_else(|| panic!("GRAMMAR.md has no {lead:?}"));
    let rest = &doc[start..];
    let lines: Vec<&str> = rest.lines().skip_while(|l| !l.starts_with("    ")).take_while(|l| l.starts_with("    ")).collect();
    lines.iter().flat_map(|l| l.split_whitespace()).collect()
}

/// The strings between `"` that follow `prefix` in `src`, one per occurrence.
fn quoted_after(src: &str, prefix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(i) = rest.find(prefix) {
        rest = &rest[i + prefix.len()..];
        if let Some(end) = rest.find('"') {
            out.insert(rest[..end].to_string());
        }
    }
    out
}

#[test]
fn the_reserved_words_are_the_lexers_keywords() {
    let lexer = read("src/lexer.rs");
    // The keyword table: lines of the form `"word" => Tok::Name,`.
    let keywords: BTreeSet<String> = lexer
        .lines()
        .filter_map(|l| l.trim().strip_prefix('"'))
        .filter_map(|l| l.split_once("\" => Tok::"))
        .map(|(w, _)| w.to_string())
        .collect();
    assert!(keywords.len() >= 20, "keyword table not found: {keywords:?}");
    let doc = read("docs/GRAMMAR.md");
    let listed: BTreeSet<String> = block_after(&doc, "**Reserved words**").into_iter().map(String::from).collect();
    assert_eq!(listed, keywords);
    // And each really is a keyword: none lexes as an identifier.
    for w in &keywords {
        let toks = staalc::lexer::lex(w).unwrap();
        assert!(!matches!(toks[0].tok, staalc::lexer::Tok::Ident(_)), "{w} lexes as an identifier");
    }
}

#[test]
fn every_contextual_word_is_named() {
    let parser = read("src/parser.rs");
    let mut words = BTreeSet::new();
    // `w == "enum"`, `name == "some"`, `!= "self"`, and `"calls" =>` style match arms.
    for prefix in ["== \"", "!= \"", "Tok::Ident(\""] {
        words.extend(quoted_after(&parser, prefix));
    }
    for line in parser.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix('"')
            && let Some((w, after)) = rest.split_once('"')
            && (after.trim_start().starts_with("=>") || after.trim_start().starts_with("if ") || after.trim_start().starts_with('|'))
        {
            words.insert(w.to_string());
        }
    }
    words.retain(|w| !w.is_empty() && w.chars().all(|c| c.is_alphanumeric() || c == '_'));
    assert!(words.contains("enum") && words.contains("calls") && words.contains("instruct"), "contextual words not found: {words:?}");
    // The document's list of them is exactly these: the words in backquotes in the
    // paragraph that introduces them.
    let doc = read("docs/GRAMMAR.md");
    let start = doc.find("**Contextual words**").unwrap();
    let para = &doc[start..start + doc[start..].find("\n\n").unwrap()];
    let listed: BTreeSet<String> = para.split('`').skip(1).step_by(2).map(String::from).collect();
    assert_eq!(listed, words, "GRAMMAR.md's contextual words differ from those the parser tests for");
}

#[test]
fn the_built_in_functions_are_the_compilers() {
    let doc = read("docs/GRAMMAR.md");
    let listed: BTreeSet<String> = block_after(&doc, "These names are built-in functions.").into_iter().map(String::from).collect();
    let builtins: BTreeSet<String> = staalc::ast::Builtin::ALL.iter().map(|b| b.name().to_string()).collect();
    assert_eq!(listed, builtins);
}

#[test]
fn every_example_parses() {
    let doc = read("docs/GRAMMAR.md");
    let mut n = 0;
    let mut lines = doc.lines();
    while let Some(l) = lines.next() {
        let Some(tag) = l.strip_prefix("```staal") else { continue };
        let body: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
        let sources: Vec<String> = match tag {
            "" => vec![body.join("\n")],
            "-statements" => vec![format!("fn f() {{\n{}\n}}", body.join("\n"))],
            "-types" => body.iter().map(|t| format!("type X = {t}")).collect(),
            other => panic!("unknown example kind staal{other}"),
        };
        for src in sources {
            let toks = staalc::lexer::lex(&src).unwrap_or_else(|e| panic!("example does not lex: {e:?}\n{src}"));
            staalc::parser::Parser::new(toks).program().unwrap_or_else(|e| panic!("example does not parse: {e:?}\n{src}"));
            n += 1;
        }
    }
    assert!(n >= 10, "only {n} examples found");
}
