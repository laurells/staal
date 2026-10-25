// `staalc debug`: a run that stops before statements, to look at it.
//
// The run is a real one, as `staalc run` makes it without a journal: its model calls
// and actions happen as they would there. It runs on the interpreter, where every
// statement is seen; native code runs whole functions at once, and the engines give
// the same results (RUNTIME.md section 15). It stops before the first statement, so
// breakpoints can be set, then wherever a command says. Commands:
//
//   break LINE | break FILE:LINE   stop before the statement on that line
//   delete LINE | delete FILE:LINE
//   breaks                         list the breakpoints
//   continue (c)                   run to the next breakpoint
//   step (s)                       run to the next statement, into calls
//   next (n)                       run to the next statement in this function or its callers
//   finish                         run to the next statement shallower than this one
//   print NAME (p)                 a variable's value, as JSON
//   locals                         every variable in scope
//   where (bt)                     the calls that led here
//   list                           the lines around this one
//   quit (q)                       stop the run
//
// A line names the entry file's line unless FILE is given: a module's file as the
// program names it, or the end of that path (`lib.staal` for `./lib.staal`). A breakpoint must be on a line where a statement
// starts. When the input ends, the run goes on to its end without stopping.

use crate::ast::{Program, Stmt};
use crate::interp::DebugHook;
use crate::lexer::Pos;
use crate::SourceFile;
use serde_json::Value as Json;
use std::collections::BTreeSet;
use std::io::{BufRead, Write};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Stop only at breakpoints.
    Run,
    /// Stop at the next statement.
    Step,
    /// Stop at the next statement no deeper than this.
    Next(usize),
    /// Stop at the next statement shallower than this.
    Finish(usize),
}

pub struct Debugger {
    input: Box<dyn BufRead + Send>,
    output: Box<dyn Write + Send>,
    files: Vec<SourceFile>,
    /// Each (file, line) where a statement starts.
    lines: BTreeSet<(u32, usize)>,
    breaks: BTreeSet<(u32, usize)>,
    mode: Mode,
    /// Where the last statement was, and at what depth: a breakpoint stops once on
    /// entering its line, not again for each statement on it.
    last: Option<(u32, usize, usize)>,
    /// Whether the input has ended: from then on the run goes on without stopping.
    detached: bool,
}

impl Debugger {
    pub fn new(prog: &Program, files: Vec<SourceFile>, input: Box<dyn BufRead + Send>, output: Box<dyn Write + Send>) -> Debugger {
        let mut lines = BTreeSet::new();
        fn walk(body: &[Stmt], lines: &mut BTreeSet<(u32, usize)>) {
            for s in body {
                let p = s.pos();
                lines.insert((p.file, p.line));
                for b in s.blocks() {
                    walk(b, lines);
                }
            }
        }
        for f in &prog.fns {
            walk(&f.body, &mut lines);
        }
        Debugger {
            input,
            output,
            files,
            lines,
            breaks: BTreeSet::new(),
            mode: Mode::Step,
            last: None,
            detached: false,
        }
    }

    fn say(&mut self, text: &str) {
        let _ = writeln!(self.output, "{text}");
        let _ = self.output.flush();
    }

    fn file_name(&self, file: u32) -> &str {
        self.files.get(file as usize).map_or("?", |f| f.name.as_str())
    }

    fn line_text(&self, file: u32, line: usize) -> &str {
        self.files.get(file as usize).and_then(|f| f.text.lines().nth(line.saturating_sub(1))).unwrap_or("")
    }

    /// `LINE` or `FILE:LINE` as (file, line).
    fn place(&self, arg: &str) -> Result<(u32, usize), String> {
        let (file, line) = match arg.rsplit_once(':') {
            Some((f, l)) => {
                // A file as the program names it (`./lib.staal`), or the end of that path.
                let named: Vec<usize> = (0..self.files.len())
                    .filter(|&i| {
                        let n = &self.files[i].name;
                        n == f || std::path::Path::new(n).ends_with(f)
                    })
                    .collect();
                match named.as_slice() {
                    [i] => (*i as u32, l),
                    [] => return Err(format!("no file named {f} in this program")),
                    _ => return Err(format!("{f} names more than one file: give more of its path")),
                }
            }
            None => (0, arg),
        };
        let line: usize = line.parse().map_err(|_| format!("'{line}' is not a line number"))?;
        Ok((file, line))
    }

    /// Reads commands until one goes on; false to stop the run.
    fn prompt(&mut self, at: Pos, stack: &[(String, Pos)], vars: &dyn Fn() -> Vec<(String, Json)>) -> bool {
        let func = stack.last().map_or("?", |(n, _)| n.as_str()).to_string();
        let here = format!("{}:{} in {func}", self.file_name(at.file), at.line);
        let text = self.line_text(at.file, at.line).trim().to_string();
        self.say(&format!("stopped at {here}\n  {} | {text}", at.line));
        loop {
            let _ = write!(self.output, "(debug) ");
            let _ = self.output.flush();
            let mut line = String::new();
            match self.input.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    self.say("");
                    self.detached = true;
                    return true;
                }
                Ok(_) => {}
            }
            let mut words = line.split_whitespace();
            let cmd = words.next().unwrap_or("");
            let arg = words.next();
            if words.next().is_some() {
                self.say("one argument at most");
                continue;
            }
            let depth = stack.len();
            match (cmd, arg) {
                ("", None) => {}
                ("continue" | "c", None) => {
                    self.mode = Mode::Run;
                    return true;
                }
                ("step" | "s", None) => {
                    self.mode = Mode::Step;
                    return true;
                }
                ("next" | "n", None) => {
                    self.mode = Mode::Next(depth);
                    return true;
                }
                ("finish", None) => {
                    self.mode = Mode::Finish(depth);
                    return true;
                }
                ("quit" | "q", None) => return false,
                ("break" | "b", Some(a)) => match self.place(a) {
                    Ok(p) if self.lines.contains(&p) => {
                        self.breaks.insert(p);
                        self.say(&format!("breakpoint at {}:{}", self.file_name(p.0), p.1));
                    }
                    Ok(p) => self.say(&format!("no statement starts on {}:{}", self.file_name(p.0), p.1)),
                    Err(e) => self.say(&e),
                },
                ("delete", Some(a)) => match self.place(a) {
                    Ok(p) if self.breaks.remove(&p) => self.say(&format!("deleted the breakpoint at {}:{}", self.file_name(p.0), p.1)),
                    Ok(p) => self.say(&format!("no breakpoint at {}:{}", self.file_name(p.0), p.1)),
                    Err(e) => self.say(&e),
                },
                ("breaks", None) => {
                    if self.breaks.is_empty() {
                        self.say("no breakpoints");
                    }
                    let all: Vec<String> = self.breaks.iter().map(|p| format!("{}:{}", self.file_name(p.0), p.1)).collect();
                    for b in all {
                        self.say(&b);
                    }
                }
                ("print" | "p", Some(name)) => match vars().into_iter().find(|(n, _)| n == name) {
                    Some((_, v)) => self.say(&v.to_string()),
                    None => self.say(&format!("no variable named {name} here")),
                },
                ("locals", None) => {
                    let vs = vars();
                    if vs.is_empty() {
                        self.say("no variables");
                    }
                    for (n, v) in vs {
                        self.say(&format!("{n} = {v}"));
                    }
                }
                ("where" | "bt", None) => {
                    for (i, (n, p)) in stack.iter().enumerate().rev() {
                        let p = if i + 1 == stack.len() { at } else { *p };
                        self.say(&format!("{n} at {}:{}", self.file_name(p.file), p.line));
                    }
                }
                ("list", None) => {
                    let total = self.files.get(at.file as usize).map_or(0, |f| f.text.lines().count());
                    for l in at.line.saturating_sub(3).max(1)..=(at.line + 3).min(total) {
                        let mark = if l == at.line { ">" } else { " " };
                        let text = self.line_text(at.file, l).to_string();
                        self.say(&format!("{mark}{l:>4} | {text}"));
                    }
                }
                ("help" | "h", None) => self.say(
                    "break LINE, delete LINE, breaks, continue (c), step (s), next (n), finish, print NAME (p), locals, where (bt), list, quit (q)",
                ),
                _ => self.say(&format!("unknown command '{}': help lists them", line.trim())),
            }
        }
    }
}

impl DebugHook for Debugger {
    fn before(&mut self, at: Pos, stack: &[(String, Pos)], vars: &dyn Fn() -> Vec<(String, Json)>) -> bool {
        if self.detached {
            return true;
        }
        let depth = stack.len();
        let entering = self.last != Some((at.file, at.line, depth));
        self.last = Some((at.file, at.line, depth));
        let stop = match self.mode {
            Mode::Step => true,
            Mode::Next(d) => depth <= d,
            Mode::Finish(d) => depth < d,
            Mode::Run => false,
        } || (entering && self.breaks.contains(&(at.file, at.line)));
        if !stop {
            return true;
        }
        self.prompt(at, stack, vars)
    }
}
