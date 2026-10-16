pub mod ast;
pub mod checker;
pub mod lexer;
pub mod parser;

use checker::Diagnostic;
use lexer::Pos;

/// Lex, parse and type-check a source string. Returns all diagnostics (empty = OK).
pub fn compile_check(src: &str) -> Vec<Diagnostic> {
    let toks = match lexer::lex(src) {
        Ok(t) => t,
        Err((pos, msg)) => return vec![Diagnostic { code: "E900", pos, msg }],
    };
    let prog = match parser::Parser::new(toks).program() {
        Ok(p) => p,
        Err((pos, msg)) => return vec![Diagnostic { code: "E901", pos, msg }],
    };
    checker::check(&prog)
}

pub fn format_diag(file: &str, d: &Diagnostic) -> String {
    let Pos { line, col } = d.pos;
    format!("{file}:{line}:{col}: error[{}]: {}", d.code, d.msg)
}
