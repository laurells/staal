// `staalc repl`: Staal one input at a time.
//
// A session is the declarations and statements kept so far. Each input is checked
// and run as a whole program made of them, by the same checker and runtime as any
// program, so the REPL accepts exactly what a file would. An input is:
//
// - a declaration (`fn`, `type`, `enum`, `interface`, `const`, `import`, with or
//   without `pub`): kept once the session with it passes every check;
// - a statement (`let`, `set`, `if`, `for`, `repeat`, `loop`, `match`): run after
//   the kept ones, and kept if it succeeds;
// - anything else, an expression: run after the kept statements, and its value shown
//   as JSON (or, for one with no value, run as a statement); never kept.
//
// Since each input runs the kept statements again, a kept input must do the same
// every time: one that reads the clock, draws a random number or calls a model is run
// and shown but not kept. What the kept statements print is printed once, when they
// are kept. Runs have no journal, no capabilities and a model with no replies, so
// nothing reaches the world. Every run is made on each engine, which must agree.

use crate::interp::{self, Engine, FailKind, RunResult};
use crate::lexer::{lex, Tok};
use std::path::{Path, PathBuf};

/// The names the REPL's own code uses.
const ENTRY: &str = "_repl";
const VALUE: &str = "_repl_value";

#[derive(Debug, PartialEq)]
enum Kind {
    Import,
    Declaration,
    Statement,
    Expression,
}

pub struct Session {
    /// Where modules and the package manifest are looked for: a file in this folder,
    /// which is never written.
    path: PathBuf,
    imports: Vec<String>,
    decls: Vec<String>,
    stmts: Vec<String>,
    /// What the kept statements print, which every run prints first.
    printed: String,
    engines: Vec<Engine>,
}

/// One run of a session, on every engine.
struct Run {
    result: Result<serde_json::Value, (FailKind, String)>,
    printed: String,
    /// Whether it read the clock, drew a random number or called a model.
    varies: bool,
}

impl Session {
    pub fn new(dir: &Path, engines: Vec<Engine>) -> Session {
        Session { path: dir.join("repl.staal"), imports: Vec::new(), decls: Vec::new(), stmts: Vec::new(), printed: String::new(), engines }
    }

    /// Whether `text` is unfinished: an open bracket or text, so more lines follow.
    pub fn incomplete(text: &str) -> bool {
        // A `"""` that ends the input waits for its next line, as in a file.
        match lex(&format!("{text}\n")) {
            Ok(toks) => {
                let mut depth = 0i64;
                for t in &toks {
                    match t.tok {
                        Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
                        Tok::RParen | Tok::RBracket | Tok::RBrace => depth -= 1,
                        _ => {}
                    }
                }
                depth > 0
            }
            // A one-line text cannot go on to the next line; a `"""` text does.
            Err((_, m)) => m.starts_with("unterminated \"\"\""),
        }
    }

    /// Takes one input and gives what to show: its output, then its value, its
    /// errors or its failure.
    pub fn eval(&mut self, input: &str) -> String {
        let input = input.trim_end();
        if input.trim().is_empty() {
            return String::new();
        }
        let n = input.lines().count();
        let kind = match lex(input) {
            Ok(toks) => kind_of(&toks.iter().map(|t| &t.tok).collect::<Vec<_>>()),
            Err((p, m)) => return format!("error[E900]: {m} ({}:{})\n", p.line, p.col),
        };
        match kind {
            Kind::Import | Kind::Declaration => {
                let (mut imports, mut decls) = (self.imports.clone(), self.decls.clone());
                if kind == Kind::Import { &mut imports } else { &mut decls }.push(input.to_string());
                let (text, at) = program(&imports, &decls, &self.stmts, None, if kind == Kind::Import { Slot::Import } else { Slot::Decl });
                match self.check(&text, at, n) {
                    Ok(_) => {
                        self.imports = imports;
                        self.decls = decls;
                        String::new()
                    }
                    Err(e) => e,
                }
            }
            Kind::Statement => {
                let mut stmts = self.stmts.clone();
                stmts.push(input.to_string());
                let (text, at) = program(&self.imports, &self.decls, &stmts, None, Slot::LastStmt);
                let prog = match self.check(&text, at, n) {
                    Ok(p) => p,
                    Err(e) => return e,
                };
                match self.run(&prog) {
                    Err(e) => e,
                    Ok(r) => {
                        let mut out = self.new_output(&r);
                        match (&r.result, r.varies) {
                            (Err((k, m)), _) => out.push_str(&format!("failed: {k}: {m}\n")),
                            (Ok(_), true) => out.push_str(
                                "not kept: it reads the clock, draws a random number or calls a model, which give a different answer each time it runs\n",
                            ),
                            (Ok(_), false) => {
                                self.stmts = stmts;
                                self.printed = r.printed;
                            }
                        }
                        out
                    }
                }
            }
            Kind::Expression => {
                // One with no value (a call of a function that gives nothing) is run for
                // what it does; the checker says which it is.
                let (unit, at) = program(&self.imports, &self.decls, &self.stmts, Some((input, true)), Slot::Value);
                let (with_value, at_value) = program(&self.imports, &self.decls, &self.stmts, Some((input, false)), Slot::Value);
                let checked = match self.check(&unit, at, n) {
                    Ok(p) => Ok((p, false)),
                    Err(_) => self.check(&with_value, at_value, n).map(|p| (p, true)),
                };
                let (prog, value) = match checked {
                    Ok(p) => p,
                    Err(first) => {
                        // An expression with no value, or one JSON cannot show: as a statement.
                        let mut stmts = self.stmts.clone();
                        stmts.push(input.to_string());
                        let (text, at) = program(&self.imports, &self.decls, &stmts, None, Slot::LastStmt);
                        match self.check(&text, at, n) {
                            Ok(p) => (p, false),
                            Err(e) if e.is_empty() => return first,
                            Err(e) => return e,
                        }
                    }
                };
                match self.run(&prog) {
                    Err(e) => e,
                    Ok(r) => {
                        let mut out = self.new_output(&r);
                        match &r.result {
                            Err((k, m)) => out.push_str(&format!("failed: {k}: {m}\n")),
                            Ok(serde_json::Value::String(json)) if value => {
                                out.push_str(json);
                                out.push('\n');
                            }
                            Ok(_) => {}
                        }
                        out
                    }
                }
            }
        }
    }

    /// What a run printed beyond what the kept statements print.
    fn new_output(&self, r: &Run) -> String {
        r.printed.strip_prefix(self.printed.as_str()).unwrap_or(&r.printed).to_string()
    }

    /// Checks a session's program; the errors are shown at their place in the input
    /// (`at` is the input's first line in the program).
    /// `n` is how many lines the input has.
    fn check(&self, text: &str, at: usize, n: usize) -> Result<crate::ast::Program, String> {
        let checked = crate::check_text_at(&self.path, text.to_string()).map_err(|e| format!("error: {e}\n"))?;
        match checked.result {
            Ok(p) => Ok(p),
            Err(diags) => {
                let mut out = String::new();
                for d in &diags {
                    if d.pos.file != 0 {
                        // In a module: its own file and place.
                        out.push_str(&format!("{}\n", crate::format_diag_in(&checked.files, d)));
                    } else if d.pos.line >= at && d.pos.line < at + n {
                        out.push_str(&format!("error[{}]: {} ({}:{})\n", d.code, d.msg, d.pos.line - at + 1, d.pos.col));
                    } else {
                        out.push_str(&format!("error[{}]: {} (in an earlier input)\n", d.code, d.msg));
                    }
                }
                Err(out)
            }
        }
    }

    fn run(&self, prog: &crate::ast::Program) -> Result<Run, String> {
        let config = serde_json::json!({ "entry": ENTRY, "model": { "provider": "script", "responses": [] } });
        let mut runs: Vec<(RunResult, String)> = Vec::new();
        for e in &self.engines {
            runs.push(interp::run_config_capturing(prog, &config, *e)?);
        }
        let varies = |r: &RunResult| r.log.iter().any(|e| matches!(e["event"].as_str(), Some("clock" | "random" | "model_request")));
        let Some(((first, printed), rest)) = runs.split_first() else { return Err("no engine to run on".into()) };
        let result = match &first.result {
            Ok(v) => Ok(v.clone()),
            Err(f) => Err((f.kind, f.message.clone())),
        };
        let varies = runs.iter().any(|(r, _)| varies(r));
        // A run that reads the clock or draws a number may rightly differ on each
        // engine; every other must not, in what it gives, logs or prints.
        if !varies && rest.iter().any(|(r, p)| !interp::same_run(r, first) || p != printed) {
            return Err("the engines disagree on this input; this is a bug in staalc\n".into());
        }
        let printed = printed.clone();
        Ok(Run { result, printed, varies })
    }
}

fn kind_of(t: &[&Tok]) -> Kind {
    let first = t.first().copied();
    let second = t.get(1).copied();
    let word = |x: Option<&Tok>, w: &str| matches!(x, Some(Tok::Ident(s)) if s == w);
    match first {
        Some(Tok::Import) => Kind::Import,
        Some(Tok::Pub | Tok::Type) => Kind::Declaration,
        Some(Tok::Fn) if matches!(second, Some(Tok::Ident(_))) => Kind::Declaration,
        Some(Tok::Ident(w)) if ["enum", "interface", "const", "capability"].contains(&w.as_str()) && matches!(second, Some(Tok::Ident(_))) => {
            Kind::Declaration
        }
        Some(Tok::Let | Tok::Set | Tok::If | Tok::For | Tok::Repeat | Tok::Loop) => Kind::Statement,
        _ if word(first, "match") && !matches!(second, Some(Tok::LParen | Tok::Dot | Tok::Eof) | None) => Kind::Statement,
        _ => Kind::Expression,
    }
}

/// Which part of the program is the new input, whose first line `program` gives.
#[derive(Clone, Copy)]
enum Slot {
    Import,
    Decl,
    LastStmt,
    Value,
}

/// A session's program: imports, declarations, then the entry function holding the
/// statements and, for a value, the expression given back as JSON. Also the line
/// where the new input starts.
/// `value` is an expression and whether it has no value (`Unit`), which is then
/// only evaluated.
fn program(imports: &[String], decls: &[String], stmts: &[String], value: Option<(&str, bool)>, slot: Slot) -> (String, usize) {
    let mut text = String::new();
    let mut at = 1;
    let mut push = |text: &mut String, part: &str, mark: bool| {
        if mark {
            at = text.lines().count() + 1;
        }
        text.push_str(part);
        text.push('\n');
    };
    for (i, s) in imports.iter().enumerate() {
        push(&mut text, s, matches!(slot, Slot::Import) && i + 1 == imports.len());
    }
    for (i, s) in decls.iter().enumerate() {
        push(&mut text, s, matches!(slot, Slot::Decl) && i + 1 == decls.len());
    }
    push(&mut text, &format!("fn {ENTRY}(){} {{", if matches!(value, Some((_, false))) { " -> Text" } else { "" }), false);
    for (i, s) in stmts.iter().enumerate() {
        push(&mut text, s, matches!(slot, Slot::LastStmt) && i + 1 == stmts.len());
    }
    match value {
        Some((v, true)) => {
            push(&mut text, &format!("let {VALUE}: Unit = ("), false);
            push(&mut text, v, true);
            push(&mut text, ")", false);
        }
        Some((v, false)) => {
            push(&mut text, &format!("let {VALUE} = json("), false);
            push(&mut text, v, true);
            push(&mut text, &format!(")\nreturn {VALUE}"), false);
        }
        None => {}
    }
    push(&mut text, "}", false);
    (text, at)
}

/// Runs a session on `input`, writing to `output`: a prompt before each input when
/// `prompt` is set (`> `, and `. ` for a line that continues one).
pub fn serve(input: impl std::io::BufRead, mut output: impl std::io::Write, prompt: bool, engines: Vec<Engine>) -> u8 {
    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut session = Session::new(&dir, engines);
    let mut pending = String::new();
    let mut lines = input.lines();
    loop {
        if prompt {
            let _ = write!(output, "{}", if pending.is_empty() { "> " } else { ". " });
            let _ = output.flush();
        }
        let line = match lines.next() {
            Some(Ok(l)) => l,
            Some(Err(e)) => {
                let _ = writeln!(output, "staalc: cannot read the input: {e}");
                return 2;
            }
            None => {
                // Only input still open is left over: each complete input ran.
                if !pending.is_empty() {
                    let _ = writeln!(output, "error[E901]: the input ended inside a bracket or a text it opened");
                }
                return 0;
            }
        };
        if !pending.is_empty() {
            pending.push('\n');
        }
        pending.push_str(&line);
        if Session::incomplete(&pending) {
            continue;
        }
        let shown = session.eval(&pending);
        pending.clear();
        if write!(output, "{shown}").and_then(|_| output.flush()).is_err() {
            return 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::serve;
    use crate::interp::Engine;

    #[test]
    fn prompts_show_where_an_input_goes_on() {
        let mut out = Vec::new();
        let code = serve("fn f() -> Int {\n    return 4\n}\nf()\n".as_bytes(), &mut out, true, vec![Engine::Interpreter]);
        assert_eq!(code, 0);
        assert_eq!(String::from_utf8(out).unwrap(), "> . . > 4\n> ");
    }
}
