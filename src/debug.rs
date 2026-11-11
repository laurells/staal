// `staalc debug`: a run that stops before statements, to look at it; and the same
// session as data (`Session`), for a program embedding staalc (Staal Studio).
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
// starts. When the input ends, the run goes on to its end without stopping. Under the
// debugger, the tasks of `parallel` run one after another, in the list's order, and a
// stop in one names it.
//
// Both forms share where to stop (`Core`) and how a value is shown (`Shown`, made by
// the interpreter from the value itself): a `Secret` is shown by its variable's name
// only, a capability as one without its handle, and any secret value of the run in a
// text is removed. `Session` differs from the text form in one way: when it is dropped,
// the run stops, rather than going on unobserved.

use crate::ast::{Program, Stmt};
use crate::interp::{self, DebugHook, RunResult};
use crate::lexer::Pos;
use crate::SourceFile;
use serde_json::{json, Map, Value as Json};
use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// A value as the debugger shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    /// Nothing: a unit, or an `Optional` holding none (one holding a value is shown as
    /// that value).
    Null,
    Bool(bool),
    Int(i64),
    /// A text, with any secret value of the run removed.
    Text(String),
    Money { amount: String, currency: String },
    List(Vec<Shown>),
    Tuple(Vec<Shown>),
    Set(Vec<Shown>),
    Map(Vec<(Shown, Shown)>),
    Record { ty: String, fields: Vec<(String, Shown)> },
    Enum { ty: String, variant: String, fields: Vec<(String, Shown)> },
    /// A function value, by name.
    Function(String),
    /// Output from outside not yet validated against its type, as it came.
    Proposed(Json),
    Receipt { id: String, capability: String, method: String },
    Outcome { ok: bool, error: String },
    Done(Vec<Shown>),
    /// A capability: never its handle or what it reaches.
    Capability,
    /// A secret: its variable's name, never its value.
    Secret { name: String },
    /// An HTTP header; its value a `Text` or a `Secret`.
    Header { name: String, value: Box<Shown> },
}

impl Shown {
    /// As `staalc debug` prints it: the value's JSON, as a run writes it.
    pub fn to_json(&self) -> Json {
        let all = |items: &[Shown]| Json::Array(items.iter().map(Shown::to_json).collect());
        let obj = |fields: &[(String, Shown)]| fields.iter().map(|(k, v)| (k.clone(), v.to_json())).collect::<Map<_, _>>();
        match self {
            Shown::Null => Json::Null,
            Shown::Bool(b) => json!(b),
            Shown::Int(n) => json!(n),
            Shown::Text(t) => json!(t),
            Shown::Money { amount, currency } => json!({ "amount": amount, "currency": currency }),
            Shown::List(items) | Shown::Tuple(items) | Shown::Set(items) => all(items),
            Shown::Map(entries) => Json::Object(
                entries
                    .iter()
                    .map(|(k, v)| {
                        let key = match k {
                            Shown::Text(t) => t.clone(),
                            Shown::Int(n) => n.to_string(),
                            other => other.to_json().to_string(),
                        };
                        (key, v.to_json())
                    })
                    .collect(),
            ),
            Shown::Record { fields, .. } => Json::Object(obj(fields)),
            Shown::Enum { variant, fields, .. } => {
                let mut o = Map::new();
                o.insert("kind".into(), json!(variant));
                o.extend(obj(fields));
                Json::Object(o)
            }
            Shown::Function(name) => json!(format!("<function {name}>")),
            Shown::Proposed(raw) => raw.clone(),
            Shown::Receipt { id, capability, method } => json!({ "receipt": id, "capability": capability, "method": method }),
            Shown::Outcome { ok, error } => json!({ "ok": ok, "error": error }),
            Shown::Done(items) => json!({ "done": all(items) }),
            Shown::Capability => json!("<capability>"),
            Shown::Secret { name } => json!({ "secret": name }),
            Shown::Header { name, value } => json!({ "header": name, "value": value.to_json() }),
        }
    }

    /// The value with its kind spelled out at every level, so a reader can tell a text
    /// from a secret's name without guessing: `{"kind": "text", "value": "..."}`,
    /// `{"kind": "secret", "name": "API_KEY"}`, `{"kind": "capability"}`, and so on.
    pub fn describe(&self) -> Json {
        let all = |items: &[Shown]| items.iter().map(Shown::describe).collect::<Vec<_>>();
        let fields = |fs: &[(String, Shown)]| fs.iter().map(|(k, v)| json!({ "name": k, "value": v.describe() })).collect::<Vec<_>>();
        match self {
            Shown::Null => json!({ "kind": "null" }),
            Shown::Bool(b) => json!({ "kind": "bool", "value": b }),
            Shown::Int(n) => json!({ "kind": "int", "value": n }),
            Shown::Text(t) => json!({ "kind": "text", "value": t }),
            Shown::Money { amount, currency } => json!({ "kind": "money", "amount": amount, "currency": currency }),
            Shown::List(items) => json!({ "kind": "list", "items": all(items) }),
            Shown::Tuple(items) => json!({ "kind": "tuple", "items": all(items) }),
            Shown::Set(items) => json!({ "kind": "set", "items": all(items) }),
            Shown::Map(entries) => json!({ "kind": "map", "entries": entries.iter().map(|(k, v)| json!({ "key": k.describe(), "value": v.describe() })).collect::<Vec<_>>() }),
            Shown::Record { ty, fields: fs } => json!({ "kind": "record", "type": ty, "fields": fields(fs) }),
            Shown::Enum { ty, variant, fields: fs } => json!({ "kind": "enum", "type": ty, "variant": variant, "fields": fields(fs) }),
            Shown::Function(name) => json!({ "kind": "function", "name": name }),
            Shown::Proposed(raw) => json!({ "kind": "proposed", "raw": raw }),
            Shown::Receipt { id, capability, method } => json!({ "kind": "receipt", "id": id, "capability": capability, "method": method }),
            Shown::Outcome { ok, error } => json!({ "kind": "outcome", "ok": ok, "error": error }),
            Shown::Done(items) => json!({ "kind": "done", "items": all(items) }),
            Shown::Capability => json!({ "kind": "capability" }),
            Shown::Secret { name } => json!({ "kind": "secret", "name": name }),
            Shown::Header { name, value } => json!({ "kind": "header", "name": name, "value": value.describe() }),
        }
    }
}

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

/// Where a session stops: the program's statement lines, the breakpoints, and the mode
/// the last command left. The same for the text form and `Session`.
struct Core {
    files: Vec<SourceFile>,
    /// Each (file, line) where a statement starts.
    lines: BTreeSet<(u32, usize)>,
    breaks: BTreeSet<(u32, usize)>,
    mode: Mode,
    /// Where the last statement was, and at what depth: a breakpoint stops once on
    /// entering its line, not again for each statement on it.
    last: Option<(u32, usize, usize)>,
    /// The concurrent task now running ("" for the program itself).
    task: String,
}

impl Core {
    fn new(prog: &Program, files: Vec<SourceFile>) -> Core {
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
        Core { files, lines, breaks: BTreeSet::new(), mode: Mode::Step, last: None, task: String::new() }
    }

    fn file_name(&self, file: u32) -> &str {
        self.files.get(file as usize).map_or("?", |f| f.name.as_str())
    }

    fn line_text(&self, file: u32, line: usize) -> &str {
        self.files.get(file as usize).and_then(|f| f.text.lines().nth(line.saturating_sub(1))).unwrap_or("")
    }

    /// A file as the program names it (`./lib.staal`), or the end of that path; None for
    /// the entry file.
    fn file(&self, name: Option<&str>) -> Result<u32, String> {
        let Some(f) = name else { return Ok(0) };
        let named: Vec<usize> = (0..self.files.len())
            .filter(|&i| {
                let n = &self.files[i].name;
                n == f || std::path::Path::new(n).ends_with(f)
            })
            .collect();
        match named.as_slice() {
            [i] => Ok(*i as u32),
            [] => Err(format!("no file named {f} in this program")),
            _ => Err(format!("{f} names more than one file: give more of its path")),
        }
    }

    /// `LINE` or `FILE:LINE` as (file, line).
    fn place(&self, arg: &str) -> Result<(u32, usize), String> {
        let (file, line) = match arg.rsplit_once(':') {
            Some((f, l)) => (self.file(Some(f))?, l),
            None => (0, arg),
        };
        let line: usize = line.parse().map_err(|_| format!("'{line}' is not a line number"))?;
        Ok((file, line))
    }

    fn set_break(&mut self, p: (u32, usize)) -> Result<String, String> {
        if !self.lines.contains(&p) {
            return Err(format!("no statement starts on {}:{}", self.file_name(p.0), p.1));
        }
        self.breaks.insert(p);
        Ok(format!("breakpoint at {}:{}", self.file_name(p.0), p.1))
    }

    fn delete_break(&mut self, p: (u32, usize)) -> Result<String, String> {
        if self.breaks.remove(&p) {
            Ok(format!("deleted the breakpoint at {}:{}", self.file_name(p.0), p.1))
        } else {
            Err(format!("no breakpoint at {}:{}", self.file_name(p.0), p.1))
        }
    }

    fn enter_task(&mut self, task: &str) {
        self.task = task.to_string();
        // A new task enters its lines afresh.
        self.last = None;
    }

    /// Whether to stop before the statement at `at`, `depth` calls deep.
    fn stops(&mut self, at: Pos, depth: usize) -> bool {
        let entering = self.last != Some((at.file, at.line, depth));
        self.last = Some((at.file, at.line, depth));
        (match self.mode {
            Mode::Step => true,
            Mode::Next(d) => depth <= d,
            Mode::Finish(d) => depth < d,
            Mode::Run => false,
        }) || (entering && self.breaks.contains(&(at.file, at.line)))
    }

    /// The calls that led to `at`, innermost first.
    fn frames(&self, at: Pos, stack: &[(String, Pos)]) -> Vec<Frame> {
        stack
            .iter()
            .enumerate()
            .rev()
            .map(|(i, (n, p))| {
                let p = if i + 1 == stack.len() { at } else { *p };
                Frame { function: n.clone(), file: self.file_name(p.file).to_string(), line: p.line }
            })
            .collect()
    }
}

/// `staalc debug`'s session: commands from `input`, answers to `output`.
pub struct Debugger {
    core: Core,
    input: Box<dyn BufRead + Send>,
    output: Box<dyn Write + Send>,
    /// Whether the input has ended: from then on the run goes on without stopping.
    detached: bool,
}

impl Debugger {
    pub fn new(prog: &Program, files: Vec<SourceFile>, input: Box<dyn BufRead + Send>, output: Box<dyn Write + Send>) -> Debugger {
        Debugger { core: Core::new(prog, files), input, output, detached: false }
    }

    fn say(&mut self, text: &str) {
        let _ = writeln!(self.output, "{text}");
        let _ = self.output.flush();
    }

    /// Reads commands until one goes on; false to stop the run.
    fn prompt(&mut self, at: Pos, stack: &[(String, Pos)], vars: &dyn Fn() -> Vec<(String, Shown)>) -> bool {
        let func = stack.last().map_or("?", |(n, _)| n.as_str()).to_string();
        let mut here = format!("{}:{} in {func}", self.core.file_name(at.file), at.line);
        if !self.core.task.is_empty() {
            here.push_str(&format!(" (task {})", self.core.task));
        }
        let text = self.core.line_text(at.file, at.line).trim().to_string();
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
                    self.core.mode = Mode::Run;
                    return true;
                }
                ("step" | "s", None) => {
                    self.core.mode = Mode::Step;
                    return true;
                }
                ("next" | "n", None) => {
                    self.core.mode = Mode::Next(depth);
                    return true;
                }
                ("finish", None) => {
                    self.core.mode = Mode::Finish(depth);
                    return true;
                }
                ("quit" | "q", None) => return false,
                ("break" | "b", Some(a)) => {
                    let said = self.core.place(a).and_then(|p| self.core.set_break(p));
                    self.say(&said.unwrap_or_else(|e| e));
                }
                ("delete", Some(a)) => {
                    let said = self.core.place(a).and_then(|p| self.core.delete_break(p));
                    self.say(&said.unwrap_or_else(|e| e));
                }
                ("breaks", None) => {
                    if self.core.breaks.is_empty() {
                        self.say("no breakpoints");
                    }
                    let all: Vec<String> = self.core.breaks.iter().map(|p| format!("{}:{}", self.core.file_name(p.0), p.1)).collect();
                    for b in all {
                        self.say(&b);
                    }
                }
                ("print" | "p", Some(name)) => match vars().into_iter().find(|(n, _)| n == name) {
                    Some((_, v)) => self.say(&v.to_json().to_string()),
                    None => self.say(&format!("no variable named {name} here")),
                },
                ("locals", None) => {
                    let vs = vars();
                    if vs.is_empty() {
                        self.say("no variables");
                    }
                    for (n, v) in vs {
                        self.say(&format!("{n} = {}", v.to_json()));
                    }
                }
                ("where" | "bt", None) => {
                    for f in self.core.frames(at, stack) {
                        self.say(&format!("{} at {}:{}", f.function, f.file, f.line));
                    }
                }
                ("list", None) => {
                    let total = self.core.files.get(at.file as usize).map_or(0, |f| f.text.lines().count());
                    for l in at.line.saturating_sub(3).max(1)..=(at.line + 3).min(total) {
                        let mark = if l == at.line { ">" } else { " " };
                        let text = self.core.line_text(at.file, l).to_string();
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
    fn task(&mut self, task: &str) {
        self.core.enter_task(task);
    }

    fn before(&mut self, at: Pos, stack: &[(String, Pos)], vars: &dyn Fn() -> Vec<(String, Shown)>) -> bool {
        if self.detached || !self.core.stops(at, stack.len()) {
            return true;
        }
        self.prompt(at, stack, vars)
    }
}

// -----------------------------------------------------------------------------------
// The session as data

/// One call that led to a stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub function: String,
    /// The file as the program names it.
    pub file: String,
    pub line: usize,
}

/// Where a run stopped, and what was in scope.
#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    /// The calls that led here, innermost first: the first is where the run stopped.
    pub frames: Vec<Frame>,
    /// The line's text, trimmed.
    pub source: String,
    /// The concurrent task the stop is in; "" for the program itself.
    pub task: String,
    /// Every variable in scope, by name, in name order.
    pub locals: Vec<(String, Shown)>,
}

/// What a session reports.
#[derive(Debug)]
pub enum Event {
    /// The run stopped before a statement and waits for a command.
    Stopped(Stop),
    /// The run ended: its result, or why it could not run. A run stopped by `Quit` ends
    /// with an `internal` failure saying so (`interp::STOPPED_IN_DEBUGGER`).
    Ended(Result<RunResult, String>),
}

/// What a session can be told while the run is stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run to the next breakpoint.
    Continue,
    /// Run to the next statement, into calls.
    Step,
    /// Run to the next statement in this function or its callers.
    Next,
    /// Run to the next statement shallower than this one.
    Finish,
    /// Stop the run.
    Quit,
    /// Stop before the statement on `line` of `file` (None for the entry file).
    Break { file: Option<String>, line: usize },
    /// Remove that breakpoint.
    Delete { file: Option<String>, line: usize },
}

/// From the run to the session.
enum Message {
    Stopped(Stop),
    /// The answer to a `Break` or `Delete`.
    Answer(Result<String, String>),
    Ended(Result<RunResult, String>),
}

struct Remote {
    core: Core,
    commands: Receiver<Command>,
    messages: Sender<Message>,
}

impl DebugHook for Remote {
    fn task(&mut self, task: &str) {
        self.core.enter_task(task);
    }

    fn before(&mut self, at: Pos, stack: &[(String, Pos)], vars: &dyn Fn() -> Vec<(String, Shown)>) -> bool {
        if !self.core.stops(at, stack.len()) {
            return true;
        }
        let stop = Stop {
            frames: self.core.frames(at, stack),
            source: self.core.line_text(at.file, at.line).trim().to_string(),
            task: self.core.task.clone(),
            locals: vars(),
        };
        if self.messages.send(Message::Stopped(stop)).is_err() {
            return false;
        }
        let depth = stack.len();
        loop {
            // The session is gone: stop the run rather than let it go on unobserved.
            let Ok(c) = self.commands.recv() else { return false };
            self.core.mode = match c {
                Command::Continue => Mode::Run,
                Command::Step => Mode::Step,
                Command::Next => Mode::Next(depth),
                Command::Finish => Mode::Finish(depth),
                Command::Quit => return false,
                Command::Break { file, line } | Command::Delete { file, line } => {
                    let place = self.core.file(file.as_deref()).map(|f| (f, line));
                    let said = if matches!(c, Command::Break { .. }) {
                        place.and_then(|p| self.core.set_break(p))
                    } else {
                        place.and_then(|p| self.core.delete_break(p))
                    };
                    if self.messages.send(Message::Answer(said)).is_err() {
                        return false;
                    }
                    continue;
                }
            };
            return true;
        }
    }
}

/// A debugging session as data: the run goes on a thread of its own, stops before its
/// first statement, and is driven by commands. Dropping the session stops the run.
pub struct Session {
    commands: Sender<Command>,
    messages: Receiver<Message>,
    /// Whether the run is stopped, waiting for a command: only then is one sent, so an
    /// answer is never confused with a stop.
    stopped: bool,
    ended: bool,
}

impl Session {
    /// Starts `prog` under the debugger with run config `config`; `files` are the
    /// program's files, as `check_path` gives them.
    pub fn start(prog: Arc<Program>, files: Vec<SourceFile>, config: Json) -> Session {
        let (commands, rx) = channel();
        let (tx, messages) = channel();
        let remote = Remote { core: Core::new(&prog, files), commands: rx, messages: tx.clone() };
        std::thread::spawn(move || {
            let r = interp::run_config_debugged(&prog, &config, Box::new(remote));
            let _ = tx.send(Message::Ended(r));
        });
        Session { commands, messages, stopped: false, ended: false }
    }

    /// The next stop, or the end of the run; waits for it. While the run is stopped,
    /// it is that stop's place to answer commands, and this is refused.
    pub fn next(&mut self) -> Result<Event, String> {
        if self.ended {
            return Err("the run has ended".into());
        }
        if self.stopped {
            return Err("the run is stopped: give it a command that lets it go on".into());
        }
        match self.messages.recv() {
            Ok(Message::Stopped(s)) => {
                self.stopped = true;
                Ok(Event::Stopped(s))
            }
            Ok(Message::Ended(r)) => {
                self.ended = true;
                Ok(Event::Ended(r))
            }
            // Answers come only to a command, which is given only while stopped.
            Ok(Message::Answer(_)) => Err("an answer came with no command".into()),
            Err(_) => {
                self.ended = true;
                Ok(Event::Ended(Err("the run's thread ended without saying how".into())))
            }
        }
    }

    /// Gives the stopped run a command. `Break` and `Delete` are answered, and the run
    /// stays stopped; any other command lets it go on, and `next` gives what follows.
    /// Refused unless the run is stopped.
    pub fn command(&mut self, c: Command) -> Result<String, String> {
        if self.ended {
            return Err("the run has ended".into());
        }
        if !self.stopped {
            return Err("the run is not stopped: wait for its next stop".into());
        }
        let answered = matches!(c, Command::Break { .. } | Command::Delete { .. });
        self.commands.send(c).map_err(|_| "the run has ended".to_string())?;
        if !answered {
            self.stopped = false;
            return Ok(String::new());
        }
        // While stopped, the run sends nothing but this answer.
        match self.messages.recv() {
            Ok(Message::Answer(a)) => a,
            Ok(Message::Stopped(_) | Message::Ended(_)) | Err(_) => {
                self.ended = true;
                Err("the run did not answer".into())
            }
        }
    }
}
