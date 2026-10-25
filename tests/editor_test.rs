// The VS Code extension (editors/vscode) kept in step with the compiler: the grammar
// highlights exactly the lexer's reserved words and the built-in functions, and the
// extension's version is the compiler's. Its own tests (`npm test` there) run the
// grammar with VS Code's engine and the server with VS Code's protocol library.

use serde_json::Value as Json;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn read(p: &str) -> Json {
    serde_json::from_str(&fs::read_to_string(Path::new(ROOT).join(p)).unwrap()).unwrap()
}

/// The words of a `\b(?:a|b|c)\b...` pattern.
fn words(pattern: &str) -> BTreeSet<String> {
    let inner = pattern.strip_prefix("\\b(?:").and_then(|p| p.split_once(")\\b")).map(|(w, _)| w).unwrap_or_else(|| panic!("not a word list: {pattern}"));
    inner.split('|').map(str::to_string).collect()
}

fn named<'a>(patterns: &'a Json, scope: &str) -> &'a str {
    patterns.as_array().unwrap().iter().find(|p| p["name"] == scope).unwrap_or_else(|| panic!("no {scope}"))["match"].as_str().unwrap()
}

#[test]
fn the_grammar_knows_exactly_the_reserved_words_and_the_built_ins() {
    let g = read("editors/vscode/syntaxes/staal.tmLanguage.json");
    let lexer = fs::read_to_string(Path::new(ROOT).join("src/lexer.rs")).unwrap();
    let reserved: BTreeSet<String> = lexer.lines().filter_map(|l| l.trim().strip_prefix('"')?.split_once("\" => Tok::").map(|(w, _)| w.to_string())).collect();
    assert_eq!(reserved.len(), 26);

    // Every reserved word is highlighted as exactly one kind of word.
    let k = &g["repository"]["keyword"]["patterns"];
    let mut seen = BTreeSet::new();
    for scope in ["keyword.control.staal", "keyword.other.staal", "storage.modifier.staal", "keyword.operator.word.staal", "constant.language.staal"] {
        let m = named(k, scope);
        let ws = if m == "\\bpub\\b" { BTreeSet::from(["pub".to_string()]) } else { words(m) };
        for w in ws {
            assert!(seen.insert(w.clone()), "{w} is highlighted twice");
        }
    }
    assert_eq!(seen, reserved);
    // ... and is a field name where it is one.
    let field = &g["repository"]["field"]["patterns"][1]["match"];
    assert_eq!(words(field.as_str().unwrap()), reserved);

    let builtins: BTreeSet<String> = staalc::ast::Builtin::ALL.iter().map(|b| b.name().to_string()).collect();
    assert_eq!(words(named(&g["repository"]["call"]["patterns"], "support.function.builtin.staal")), builtins);
    // The language server completes the same reserved words.
    assert_eq!(staalc::lsp::RESERVED.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(), reserved);
}

#[test]
fn the_extension_is_the_compilers_version_and_runs_its_language_server() {
    let p = read("editors/vscode/package.json");
    assert_eq!(p["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(p["private"], true);
    assert_eq!(p["contributes"]["languages"][0]["extensions"][0], ".staal");
    assert_eq!(p["contributes"]["configuration"]["properties"]["staal.server.path"]["default"], "staalc");
    let ext = fs::read_to_string(Path::new(ROOT).join("editors/vscode/extension.js")).unwrap();
    assert!(ext.contains("args: ['lsp']"));
    let c = read("editors/vscode/language-configuration.json");
    assert_eq!(c["comments"]["lineComment"], "//");
}
