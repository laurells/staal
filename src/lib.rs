pub mod ast;
pub mod budget;
pub mod checker;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod pattern;

use checker::Diagnostic;
use lexer::Pos;

/// Lex, parse and check a source string. Returns the program only if every check
/// passes, so nothing that fails a check can ever be run.
pub fn check_program(src: &str) -> Result<ast::Program, Vec<Diagnostic>> {
    let toks = lexer::lex(src).map_err(|(pos, msg)| vec![Diagnostic { code: "E900", pos, msg }])?;
    let prog = parser::Parser::new(toks).program().map_err(|(pos, msg)| vec![Diagnostic { code: "E901", pos, msg }])?;
    let (mut diags, for_bounds) = checker::check_with_bounds(&prog);
    diags.extend(budget::check(&prog, &for_bounds));
    if diags.is_empty() {
        Ok(prog)
    } else {
        Err(diags)
    }
}

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
    let (mut diags, for_bounds) = checker::check_with_bounds(&prog);
    diags.extend(budget::check(&prog, &for_bounds));
    diags
}

pub fn format_diag(file: &str, d: &Diagnostic) -> String {
    let Pos { line, col } = d.pos;
    format!("{file}:{line}:{col}: error[{}]: {}", d.code, d.msg)
}
