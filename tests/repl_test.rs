// `staalc repl` (src/repl.rs), driven through its standard input: what it shows for
// each kind of input, what it keeps, and that what is kept runs again without being
// seen again.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_staalc");

fn repl(dir: &Path, args: &[&str], input: &str) -> (Option<i32>, String, String) {
    let mut child = Command::new(BIN)
        .arg("repl")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    // A REPL that stops answering fails the test instead of hanging it.
    let id = child.id();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watched = done.clone();
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(60) {
            if watched.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = Command::new("kill").arg("-9").arg(id.to_string()).status();
    });
    let o = child.wait_with_output().unwrap();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(o.status.code().is_some(), "the REPL did not finish within a minute");
    (o.status.code(), String::from_utf8(o.stdout).unwrap(), String::from_utf8(o.stderr).unwrap())
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_session_shows_values_keeps_what_succeeds_and_prints_once() {
    let d = scratch("repl-session");
    let input = r#"1 + 2
let x = 5
x * 2
set x = x + 1
x
type P { name: Text, age: Int }
fn older(p: P) -> P {
    return P { name: p.name, age: p.age + 1 }
}
older(P { name: "Ada", age: 36 })
let greeting = "hé"
print("printed when given: \(greeting)")
let shown = print("printed once, when kept")
let kept = len(greeting)
print("not kept, shown")
kept
10.50 NGN + 1.00 NGN
[some(1), none]
let q = 1 / 0
q
let t = now()
t
now() > 0
undefined_thing + 1
fn older(p: P) -> P {
    return p
}
let s = """
    two
    lines
    """
s
fn nothing() {
}
nothing()
"#;
    let engines: &[&[&str]] = if cfg!(feature = "native") { &[&[], &["--engine", "native"], &["--engine", "interpreter"]] } else { &[&[]] };
    for args in engines {
        let (code, out, err) = repl(&d, args, input);
        assert_eq!(code, Some(0), "{args:?}: {err}");
        assert_eq!(
            out,
            r#"3
10
6
{"age":37,"name":"Ada"}
printed when given: hé
printed once, when kept
not kept, shown
2
{"amount":"11.50","currency":"NGN"}
[1,null]
failed: value: division by zero: 1 / 0
error[E100]: undefined name 'q' (1:1)
not kept: it reads the clock, draws a random number or calls a model, which give a different answer each time it runs
error[E100]: undefined name 't' (1:1)
true
error[E100]: undefined name 'undefined_thing' (1:1)
error[E101]: function 'older' is defined twice (1:1)
"two\nlines"
"#,
            "{args:?}"
        );
    }
}

#[test]
fn a_model_is_never_reached_and_modules_come_from_the_folder() {
    let d = scratch("repl-modules");
    fs::write(d.join("util.staal"), "pub fn twice(n: Int) -> Int {\n    return n * 2\n}\n\npub fn broken() -> Int {\n    return 1\n}\n").unwrap();
    let (code, out, _) = repl(
        &d,
        &[],
        "import util\nutil.twice(21)\ntype N { n: Int }\nlet m = validate propose(N, tokens: 300)\nimport nowhere\nutil.twice(1)\n",
    );
    assert_eq!(code, Some(0));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "42");
    // The model has no replies: the call fails, and nothing is kept.
    assert!(lines[1].starts_with("failed: provider: "), "{out}");
    // A module that is not there is refused, and the session goes on without it.
    assert!(lines[2].contains("nowhere"), "{out}");
    assert_eq!(lines[3], "2");
    assert_eq!(lines.len(), 4, "{out}");
}

#[test]
fn input_left_open_at_the_end_is_still_checked_and_options_are_checked() {
    let d = scratch("repl-end");
    let (code, out, _) = repl(&d, &[], "fn f() -> Int {\n    return 1\n");
    assert_eq!(code, Some(0));
    assert_eq!(out, "error[E901]: the input ended inside a bracket or a text it opened\n");
    // A text open across lines waits for its end; a one-line text does not.
    let (_, out, _) = repl(&d, &[], "\"open\n1\n");
    assert_eq!(out.lines().nth(1), Some("1"), "{out}");
    for args in [&["--engine", "fast"][..], &["file.staal"], &["--engine"]] {
        let (code, _, err) = repl(&d, args, "");
        assert_eq!(code, Some(2), "{args:?}");
        assert!(err.contains("usage"), "{args:?}: {err}");
    }
}
