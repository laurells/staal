// `staalc debug` (src/debug.rs): exact sessions on tests/debug, a program with a
// module, and how a run ends: at its end, by `quit`, or when the input ends.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_staalc");

fn debug(input: &str) -> (Option<i32>, String, String) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/debug");
    let mut child = Command::new(BIN)
        .args(["debug", "main.staal", "--config", "run.json"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    (o.status.code(), String::from_utf8(o.stdout).unwrap(), String::from_utf8(o.stderr).unwrap())
}

#[test]
fn breakpoints_stepping_and_looking_at_values() {
    let (code, out, err) = debug(
        "break 4\nbreak 16\nbreak lib.staal:2\nbreak nowhere.staal:1\nbreak x\nlist\ncontinue\nlocals\nstep\nwhere\nnext\nprint t\nprint lines\nstep\nstep\nprint n\nwhere\nfinish\ndelete lib.staal:2\ndelete 99\nfrobnicate\nprint a b\nprint nobody\ncontinue\n",
    );
    assert_eq!((code, err.as_str()), (Some(0), ""));
    assert_eq!(
        out,
        r#"stopped at main.staal:14 in main
  14 | let lines = [Line { sku: "a", qty: 1 }, Line { sku: "b", qty: 2 }]
(debug) no statement starts on main.staal:4
(debug) breakpoint at main.staal:16
(debug) breakpoint at ./lib.staal:2
(debug) no file named nowhere.staal in this program
(debug) 'x' is not a line number
(debug)    11 | }
   12 | 
   13 | fn main() -> Int {
>  14 |     let lines = [Line { sku: "a", qty: 1 }, Line { sku: "b", qty: 2 }]
   15 |     let pick = fn(l: Line) -> Int { return l.qty }
   16 |     let n = total(lines)
   17 |     print("total \(n)")
(debug) stopped at main.staal:16 in main
  16 | let n = total(lines)
(debug) lines = [{"qty":1,"sku":"a"},{"qty":2,"sku":"b"}]
pick = "<function main::fn@15:16>"
(debug) stopped at main.staal:6 in total
  6 | let t = 0
(debug) total at main.staal:6
main at main.staal:16
(debug) stopped at main.staal:7 in total
  7 | for l in lines {
(debug) 0
(debug) [{"qty":1,"sku":"a"},{"qty":2,"sku":"b"}]
(debug) stopped at main.staal:8 in total
  8 | set t = t + lib.double(l.qty)
(debug) stopped at ./lib.staal:2 in lib.double
  2 | let d = n * 2
(debug) 1
(debug) lib.double at ./lib.staal:2
total at main.staal:8
main at main.staal:16
(debug) stopped at main.staal:8 in total
  8 | set t = t + lib.double(l.qty)
(debug) deleted the breakpoint at ./lib.staal:2
(debug) no breakpoint at main.staal:99
(debug) unknown command 'frobnicate': help lists them
(debug) one argument at most
(debug) no variable named nobody here
(debug) total 6
positive
7
"#
    );
}

#[test]
fn next_runs_over_calls_and_the_end_of_the_input_lets_the_run_finish() {
    let (code, out, _) = debug("break 16\nnext\nnext\nbreaks\n\nnext\nnext\n");
    assert_eq!(code, Some(0));
    assert_eq!(
        out,
        r#"stopped at main.staal:14 in main
  14 | let lines = [Line { sku: "a", qty: 1 }, Line { sku: "b", qty: 2 }]
(debug) breakpoint at main.staal:16
(debug) stopped at main.staal:15 in main
  15 | let pick = fn(l: Line) -> Int { return l.qty }
(debug) stopped at main.staal:16 in main
  16 | let n = total(lines)
(debug) main.staal:16
(debug) (debug) stopped at main.staal:17 in main
  17 | print("total \(n)")
(debug) total 6
stopped at main.staal:18 in main
  18 | if n > 0 { print("positive") }
(debug) 
positive
7
"#
    );
    // A breakpoint in a function called twice stops at each call.
    let (_, out, _) = debug("break lib.staal:2\ncontinue\ncontinue\ncontinue\n");
    assert_eq!(out.matches("stopped at ./lib.staal:2 in lib.double").count(), 2, "{out}");
    // A breakpoint stops once on entering its line, not again for the `print` on it.
    let (_, out, _) = debug("break 18\ncontinue\ncontinue\n");
    assert!(out.ends_with("(debug) total 6\nstopped at main.staal:18 in main\n  18 | if n > 0 { print(\"positive\") }\n(debug) positive\n7\n"), "{out}");
}

#[test]
fn quit_stops_the_run_and_a_program_with_errors_never_starts() {
    let (code, out, err) = debug("quit\n");
    assert_eq!(code, Some(1));
    assert!(out.ends_with("(debug) "), "{out}");
    assert_eq!(err, "staalc: the run was stopped in the debugger\n");
    // No input at all: the run goes to its end.
    let (code, out, _) = debug("");
    assert_eq!(code, Some(0));
    assert!(out.ends_with("total 6\npositive\n7\n"), "{out}");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("debug-errors");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.staal"), "fn main() -> Int {\n    return y\n}\n").unwrap();
    std::fs::write(dir.join("run.json"), r#"{"entry": "main", "model": {"provider": "script", "responses": []}}"#).unwrap();
    let o = Command::new(BIN).args(["debug", "bad.staal", "--config", "run.json"]).current_dir(&dir).stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    // Without a config the program is still checked first, and does not start.
    let o = Command::new(BIN).args(["debug", "bad.staal"]).current_dir(&dir).stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
}

#[test]
fn a_breakpoint_in_concurrent_tasks_stops_in_each_one_in_turn() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/debug");
    let mut child = Command::new(BIN)
        .args(["debug", "tasks.staal", "--config", "run.json"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"break 2\ncontinue\ncontinue\nprint n\ncontinue\n").unwrap();
    let o = child.wait_with_output().unwrap();
    let out = String::from_utf8(o.stdout).unwrap();
    for task in ["1.0", "1.1", "1.2"] {
        assert!(out.contains(&format!("stopped at tasks.staal:2 in square (task {task})")), "{out}");
    }
    assert!(out.contains("(debug) 2\n"), "{out}");
    assert!(out.ends_with("14\n"), "{out}");
}

/// Runs `program` in tests/debug under `staalc debug` and as a `Session`, with the same
/// breakpoints and the same commands, asking for every variable at every stop, and
/// checks both stop at the same places, in the same functions and tasks, with the same
/// variables, and answer each breakpoint alike.
fn same_session(program: &str, breaks: &[&str], go: &[&str]) {
    use staalc::debug::{Command as C, Event, Session};
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/debug");
    // The text form.
    let mut input = String::new();
    for b in breaks {
        input.push_str(&format!("break {b}\n"));
    }
    for g in go {
        input.push_str(&format!("locals\n{g}\n"));
    }
    // Both given the program by the same path, so both name its files alike.
    let mut child = Command::new(BIN)
        .arg("debug")
        .arg(dir.join(program))
        .arg("--config")
        .arg(dir.join("run.json"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    let segments: Vec<&str> = out.split("(debug) ").collect();
    let header = |seg: &str| seg.lines().find(|l| l.starts_with("stopped at ")).map(String::from);
    let mut text_stops = vec![header(segments[0]).unwrap()];
    let text_breaks: Vec<String> = segments[1..=breaks.len()].iter().map(|s| s.trim_end().to_string()).collect();
    let mut text_locals = Vec::new();
    for pair in segments[breaks.len() + 1..].chunks(2) {
        text_locals.push(pair[0].trim_end().to_string());
        match pair.get(1).and_then(|s| header(s)) {
            Some(h) => text_stops.push(h),
            None => break,
        }
    }
    // The session.
    let checked = staalc::check_path(&dir.join(program)).unwrap();
    let files = checked.files.clone();
    let prog = std::sync::Arc::new(checked.result.unwrap());
    let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("run.json")).unwrap()).unwrap();
    let mut s = Session::start(prog, files, config);
    let (mut stops, mut locals, mut answers) = (Vec::new(), Vec::new(), Vec::new());
    let mut commands = go.iter();
    let mut first = true;
    while let Event::Stopped(stop) = s.wait().unwrap() {
        let f = &stop.frames[0];
        let task = if stop.task.is_empty() { String::new() } else { format!(" (task {})", stop.task) };
        stops.push(format!("stopped at {}:{} in {}{task}", f.file, f.line, f.function));
        if first {
            first = false;
            for b in breaks {
                let (file, line) = match b.rsplit_once(':') {
                    Some((f, l)) => (Some(f.to_string()), l.parse().unwrap()),
                    None => (None, b.parse().unwrap()),
                };
                answers.push(s.command(C::Break { file, line }).unwrap_or_else(|e| e));
            }
        }
        // The text form's input ends here: it shows nothing more.
        let next = match commands.next().copied() {
            Some("continue") => C::Continue,
            Some("step") => C::Step,
            Some("next") => C::Next,
            Some("finish") => C::Finish,
            Some(other) => panic!("no command {other}"),
            None => break,
        };
        let shown: Vec<String> = stop.locals.iter().map(|(n, v)| format!("{n} = {}", v.to_json())).collect();
        locals.push(if shown.is_empty() { "no variables".to_string() } else { shown.join("\n") });
        s.command(next).unwrap();
    }
    assert_eq!(answers, text_breaks, "{program}: the breakpoints");
    assert_eq!(stops, text_stops, "{program}: where each stopped\n{out}");
    assert_eq!(locals, text_locals, "{program}: the variables at each stop\n{out}");
    assert!(stops.len() > 1, "{program}: the session stopped only once");
}

#[test]
fn a_structured_session_gives_the_same_states_as_the_text_one() {
    let c = ["continue"; 6];
    same_session("main.staal", &["16", "lib.staal:2", "4", "nowhere.staal:1"], &["continue", "step", "next", "step", "step", "finish", "continue", "continue"]);
    same_session("main.staal", &["16"], &["next"; 8]);
    same_session("main.staal", &["lib.staal:2"], &c);
    same_session("main.staal", &["18"], &c);
    same_session("main.staal", &[], &["step"; 40]);
    same_session("main.staal", &["lib.staal:2"], &["continue", "finish", "finish", "step", "continue", "continue"]);
    same_session("tasks.staal", &["2"], &c);
}

#[test]
fn a_session_takes_commands_only_while_stopped_and_quit_ends_the_run() {
    use staalc::debug::{Command as C, Event, Session};
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/debug");
    let checked = staalc::check_path(&dir.join("main.staal")).unwrap();
    let files = checked.files.clone();
    let prog = std::sync::Arc::new(checked.result.unwrap());
    let config = serde_json::json!({ "entry": "main", "model": { "provider": "script", "responses": [] } });
    let mut s = Session::start(prog, files, config);
    assert!(s.command(C::Step).unwrap_err().contains("not stopped"), "before the first stop");
    assert!(matches!(s.wait().unwrap(), Event::Stopped(_)));
    assert!(s.wait().unwrap_err().contains("is stopped"));
    assert_eq!(s.command(C::Break { file: Some("nowhere.staal".into()), line: 1 }), Err("no file named nowhere.staal in this program".into()));
    assert_eq!(s.command(C::Delete { file: None, line: 16 }), Err(format!("no breakpoint at {}:16", dir.join("main.staal").display())));
    s.command(C::Quit).unwrap();
    let Event::Ended(Ok(r)) = s.wait().unwrap() else { panic!("the run did not end") };
    assert_eq!(r.result.unwrap_err().message, staalc::interp::STOPPED_IN_DEBUGGER);
    assert!(s.wait().unwrap_err().contains("has ended"));
    assert!(s.command(C::Continue).unwrap_err().contains("has ended"));
}
