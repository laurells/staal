// The command line, shared by `staalc` and by executables `staalc build` makes.
//
//   staalc check   <file.staal>
//   staalc fmt     [--check] <file.staal>...
//   staalc doc     <file.staal> [-o <out.md>]
//   staalc profile <file.staal> --config <run.json> [--engine native] [-o <profile.json>]
//   staalc debug   <file.staal> --config <run.json>
//   staalc lsp
//   staalc --version
//   staalc repl    [--engine interpreter|native|both]
//   staalc test    [--config <test.json>] [--engine interpreter|native|both] <file.staal or folder>...
//   staalc fetch   [<project folder>]
//   staalc build   <file.staal> -o <executable> [--runtime <libstaalc.a>]
//   staalc run     <file.staal> --config <run.json> [--journal <run.journal>] [--log <run.jsonl>] [--engine native]
//   staalc resume  <file.staal> --config <run.json> --journal <run.journal> [--log <run.jsonl>] [--engine native]
//   staalc resolve --journal <run.journal> (--performed <receipt> | --status <code> [--body <text>]
//                                            | --reply <text> | --failed <reason> | --retry)
//
// A built executable takes the same commands without the program file:
//
//   app run     --config <run.json> [--journal <run.journal>] [--log <run.jsonl>]
//   app resume  --config <run.json> --journal <run.journal> [--log <run.jsonl>]
//   app resolve --journal <run.journal> (...as above)
//
// Exit codes:
//   0  success
//   1  the program has check errors, or its run failed
//   2  a usage, file, config or journal problem: nothing was run
//   3  the run stopped before finishing and can be continued: an action is in doubt
//      (resolve it, then resume) or the journal could not be written (resume)

use crate::interp::{self, journal, Durability, Engine, FailKind, RunResult};
use std::collections::HashMap;
use std::path::Path;

const USAGE: &str = "usage:
  staalc check   <file.staal>
  staalc fmt     [--check] <file.staal>...
  staalc doc     <file.staal> [-o <out.md>]
  staalc profile <file.staal> --config <run.json> [--engine native] [-o <profile.json>]
  staalc debug   <file.staal> --config <run.json>   (commands: help)
  staalc lsp     (a language server for editors, on standard input and output)
  staalc --version
  staalc repl    [--engine interpreter|native|both]
  staalc test    [--config <test.json>] [--engine interpreter|native|both] <file.staal or folder>...
  staalc fetch   [<project folder>]
  staalc build   <file.staal> -o <executable> [--runtime <libstaalc.a>]
  staalc run     <file.staal> --config <run.json> [--journal <run.journal>] [--log <run.jsonl>] [--engine native]
  staalc resume  <file.staal> --config <run.json> --journal <run.journal> [--log <run.jsonl>] [--engine native]
  staalc resolve --journal <run.journal> (--performed <receipt> | --status <code> [--body <text>]
                                           | --reply <text> | --failed <reason> | --retry)";

#[cfg(feature = "native")]
const APP_USAGE: &str = "usage:
  run     --config <run.json> [--journal <run.journal>] [--log <run.jsonl>]
  resume  --config <run.json> --journal <run.journal> [--log <run.jsonl>]
  resolve --journal <run.journal> (--performed <receipt> | --status <code> [--body <text>]
                                   | --failed <reason> | --retry)";

fn usage_error(usage: &str, msg: &str) -> u8 {
    eprintln!("staalc: {msg}\n{usage}");
    2
}

/// `staalc` with its arguments (without the program name). Returns the exit code.
pub fn staalc_main(args: Vec<String>) -> u8 {
    let u = |m: &str| usage_error(USAGE, m);
    let Some(cmd) = args.first() else { return u("no command given") };
    let rest = &args[1..];
    match cmd.as_str() {
        "check" => match parse(rest, &[], &[]) {
            Ok((Some(file), _)) => check(&file),
            Ok((None, _)) => u("check needs a file"),
            Err(e) => u(&e),
        },
        "--version" | "version" if rest.is_empty() => {
            println!("staalc {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "fmt" => fmt_cmd(rest),
        "test" => test_cmd(rest),
        "debug" => match parse(rest, &["--config"], &["--config"]) {
            Ok((Some(file), opts)) => debug_cmd(&file, &opts),
            Ok((None, _)) => u("debug needs a program file"),
            Err(e) => u(&e),
        },
        "profile" => match parse(rest, &["--config", "--engine", "-o"], &["--config"]) {
            Ok((Some(file), opts)) => {
                let engine = match opts.get("--engine").map(String::as_str) {
                    None | Some("interpreter") => Engine::Interpreter,
                    Some("native") => Engine::Native,
                    Some(other) => return u(&format!("--engine is native or interpreter, not {other}")),
                };
                profile_cmd(&file, &opts, engine)
            }
            Ok((None, _)) => u("profile needs a program file"),
            Err(e) => u(&e),
        },
        "repl" => match parse(rest, &["--engine"], &[]) {
            Ok((None, opts)) => {
                let engines = match opts.get("--engine").map(String::as_str) {
                    None if cfg!(feature = "native") => vec![Engine::Interpreter, Engine::Native],
                    None | Some("interpreter") => vec![Engine::Interpreter],
                    Some("native") => vec![Engine::Native],
                    Some("both") => vec![Engine::Interpreter, Engine::Native],
                    Some(o) => return u(&format!("--engine is interpreter, native or both, not {o}")),
                };
                use std::io::IsTerminal;
                let stdin = std::io::stdin();
                let prompt = stdin.is_terminal();
                crate::repl::serve(stdin.lock(), std::io::stdout().lock(), prompt, engines)
            }
            Ok((Some(f), _)) => u(&format!("repl takes no file (found {f}): it reads its input")),
            Err(e) => u(&e),
        },
        "doc" => match parse(rest, &["-o"], &[]) {
            Ok((Some(file), opts)) => doc_cmd(&file, opts.get("-o").map(String::as_str)),
            Ok((None, _)) => u("doc needs a program file"),
            Err(e) => u(&e),
        },
        "lsp" if rest.is_empty() => crate::lsp::serve(std::io::stdin().lock(), std::io::stdout().lock()),
        "lsp" => u("lsp takes no arguments: an editor starts it and talks to it on standard input and output"),
        "fetch" => match parse(rest, &[], &[]) {
            Ok((dir, _)) => fetch(dir.as_deref().unwrap_or(".")),
            Err(e) => u(&e),
        },
        "build" => match parse(rest, &["-o", "--runtime"], &["-o"]) {
            Ok((Some(file), opts)) => build(&file, &opts["-o"], opts.get("--runtime").map(String::as_str)),
            Ok((None, _)) => u("build needs a program file"),
            Err(e) => u(&e),
        },
        "run" | "resume" => {
            let resume = cmd == "resume";
            let required: &[&str] = if resume { &["--config", "--journal"] } else { &["--config"] };
            match parse(rest, &["--config", "--journal", "--log", "--engine"], required) {
                Ok((Some(file), opts)) => {
                    let engine = match opts.get("--engine").map(String::as_str) {
                        None | Some("interpreter") => Engine::Interpreter,
                        Some("native") => Engine::Native,
                        Some(other) => return u(&format!("--engine is native or interpreter, not {other}")),
                    };
                    let checked = match crate::check_path(Path::new(&file)) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("staalc: {e}");
                            return 2;
                        }
                    };
                    run(&checked, &file, &opts, resume, |prog, fp, cfg, d| interp::run_config_engine(prog, fp, cfg, d, engine))
                }
                Ok((None, _)) => u(&format!("{cmd} needs a program file")),
                Err(e) => u(&e),
            }
        }
        "resolve" => resolve_cmd(rest, USAGE),
        other => u(&format!("unknown command {other}")),
    }
}

/// Options that take no value.
const FLAGS: &[&str] = &["--retry"];

/// Parses `[file] (--option value | --flag)*`. Each option may appear once;
/// `required` options must be present.
fn parse(args: &[String], allowed: &[&str], required: &[&str]) -> Result<(Option<String>, HashMap<String, String>), String> {
    let mut file = None;
    let mut opts = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with('-') {
            if !allowed.contains(&a.as_str()) {
                return Err(format!("unknown option {a}"));
            }
            let (value, used) = if FLAGS.contains(&a.as_str()) {
                (String::new(), 1)
            } else {
                match args.get(i + 1) {
                    Some(v) => (v.clone(), 2),
                    None => return Err(format!("{a} needs a value")),
                }
            };
            if opts.insert(a.clone(), value).is_some() {
                return Err(format!("{a} given twice"));
            }
            i += used;
        } else if file.is_none() {
            file = Some(a.clone());
            i += 1;
        } else {
            return Err(format!("unexpected argument {a}"));
        }
    }
    for r in required {
        if !opts.contains_key(*r) {
            return Err(format!("{r} is required"));
        }
    }
    Ok((file, opts))
}

fn report(files: &[crate::SourceFile], diags: &[crate::checker::Diagnostic]) {
    for d in diags {
        eprintln!("{}", crate::format_diag_in(files, d));
    }
    eprintln!("{} error(s)", diags.len());
}

fn check(path: &str) -> u8 {
    let checked = match crate::check_path(Path::new(path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("staalc: {e}");
            return 2;
        }
    };
    match checked.result {
        Ok(_) => {
            println!("{path}: ok");
            0
        }
        Err(diags) => {
            report(&checked.files, &diags);
            1
        }
    }
}

/// Runs or resumes a checked program with `go`, writes the log, prints the result.
fn run(
    checked: &crate::Checked,
    name: &str,
    opts: &HashMap<String, String>,
    resume: bool,
    go: impl FnOnce(&crate::ast::Program, &str, &serde_json::Value, Durability) -> Result<RunResult, String>,
) -> u8 {
    let prog = match &checked.result {
        Ok(p) => p,
        Err(diags) => {
            report(&checked.files, diags);
            eprintln!("staalc: not running {name}: it has check errors");
            return 1;
        }
    };
    let config = &opts["--config"];
    let cfg_text = match std::fs::read_to_string(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("staalc: cannot read {config}: {e}");
            return 2;
        }
    };
    let cfg: serde_json::Value = match serde_json::from_str(&cfg_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("staalc: {config} is not valid JSON: {e}");
            return 2;
        }
    };
    let durability = match (opts.get("--journal"), resume) {
        (Some(j), false) => Durability::Create(Path::new(j)),
        (Some(j), true) => Durability::Resume(Path::new(j)),
        (None, _) => Durability::None,
    };
    let out = match go(prog, &checked.fingerprint, &cfg, durability) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("staalc: {e}");
            return 2;
        }
    };
    if let Some(log) = opts.get("--log") {
        let mut text = String::new();
        for event in &out.log {
            text.push_str(&event.to_string());
            text.push('\n');
        }
        if let Err(e) = std::fs::write(log, text) {
            eprintln!("staalc: cannot write log {log}: {e}");
            return 2;
        }
    }
    match out.result {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(f) if matches!(f.kind, FailKind::InDoubt | FailKind::Journal) => {
            eprintln!("staalc: run stopped: {}: {}", f.kind, f.message);
            3
        }
        Err(f) if f.kind == FailKind::Internal && f.message == interp::STOPPED_IN_DEBUGGER => {
            eprintln!("staalc: {}", f.message);
            1
        }
        Err(f) => {
            eprintln!("staalc: run failed: {}: {}", f.kind, f.message);
            1
        }
    }
}

/// Fetches the project's packages, writes staal.lock, and shows each package's
/// authority, so it can be reviewed before any of its code is used.
fn fetch(dir: &str) -> u8 {
    match crate::packages::fetch(Path::new(dir)) {
        Ok(lines) => {
            if lines.is_empty() {
                println!("no dependencies");
            }
            for l in lines {
                println!("{l}");
            }
            0
        }
        Err(e) => {
            eprintln!("staalc: {e}");
            2
        }
    }
}

fn resolve_cmd(rest: &[String], usage: &str) -> u8 {
    let u = |m: &str| usage_error(usage, m);
    let opts = match parse(rest, &["--journal", "--performed", "--status", "--body", "--reply", "--failed", "--retry"], &["--journal"]) {
        Ok((None, opts)) => opts,
        Ok((Some(extra), _)) => return u(&format!("unexpected argument {extra}")),
        Err(e) => return u(&e),
    };
    let given = ["--performed", "--status", "--reply", "--failed", "--retry"].iter().filter(|k| opts.contains_key(**k)).count();
    if given != 1 {
        return u("resolve needs exactly one of --performed <receipt>, --status <code>, --reply <text>, --failed <reason> or --retry");
    }
    if opts.contains_key("--body") && !opts.contains_key("--status") {
        return u("--body goes with --status");
    }
    let resolution = if let Some(receipt) = opts.get("--performed") {
        journal::Resolution::Performed { receipt: receipt.clone() }
    } else if let Some(code) = opts.get("--status") {
        let Ok(status) = code.parse::<u16>() else { return u("--status needs an HTTP status code such as 200") };
        journal::Resolution::Responded { status, body: opts.get("--body").cloned().unwrap_or_default() }
    } else if let Some(text) = opts.get("--reply") {
        journal::Resolution::Replied { text: text.clone() }
    } else if let Some(reason) = opts.get("--failed") {
        journal::Resolution::Failed { message: reason.clone() }
    } else {
        journal::Resolution::Retry
    };
    match journal::Journal::resolve(Path::new(&opts["--journal"]), resolution) {
        Ok(summary) => {
            println!("{summary}; now resume the run");
            0
        }
        Err(e) => {
            eprintln!("staalc: {e}");
            2
        }
    }
}

// ---------------------------------------------------------------------------------
// Executables (`staalc build`)

/// What an executable carries besides its machine code, as JSON.
#[cfg(feature = "native")]
const BUNDLE_FORMAT: u64 = 1;

#[cfg(feature = "native")]
fn build(file: &str, out: &str, runtime: Option<&str>) -> u8 {
    match build_executable(Path::new(file), Path::new(out), runtime.map(Path::new)) {
        Ok(n) => {
            println!("{out}: built, {n} function(s) as native code");
            0
        }
        Err((code, e)) => {
            eprintln!("staalc: {e}");
            code
        }
    }
}

#[cfg(not(feature = "native"))]
fn build(_: &str, _: &str, _: Option<&str>) -> u8 {
    eprintln!("staalc: this staalc was built without native code (the `native` feature), so it cannot build executables");
    2
}

/// The system libraries the Rust runtime needs when linked statically (what
/// `rustc --print native-static-libs` reports for this target).
#[cfg(all(feature = "native", target_os = "linux"))]
const SYSTEM_LIBS: &[&str] = &["-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl", "-lc"];
/// Elsewhere `build_executable` refuses before linking, so no list is needed yet.
#[cfg(all(feature = "native", not(target_os = "linux")))]
const SYSTEM_LIBS: &[&str] = &[];

/// Checks the program, compiles its native code into an object file with everything
/// the executable needs, and links it with the runtime library. Returns how many
/// functions were compiled.
#[cfg(feature = "native")]
pub fn build_executable(file: &Path, out: &Path, runtime: Option<&Path>) -> Result<usize, (u8, String)> {
    if !cfg!(target_os = "linux") {
        return Err((2, "building executables is supported on Linux only so far".into()));
    }
    let (checked, recorded) = crate::check_path_recorded(file).map_err(|e| (2, e))?;
    let prog = match &checked.result {
        Ok(p) => p,
        Err(diags) => {
            report(&checked.files, diags);
            return Err((1, format!("not building {}: it has check errors", file.display())));
        }
    };
    // The runtime: the static library built alongside this staalc.
    let lib = match runtime {
        Some(p) => p.to_path_buf(),
        None => {
            let exe = std::env::current_exe().map_err(|e| (2, format!("cannot find staalc itself: {e}")))?;
            exe.parent().map(|d| d.join("libstaalc.a")).unwrap_or_default()
        }
    };
    if !lib.is_file() {
        return Err((2, format!("the runtime library {} is missing; give its path with --runtime", lib.display())));
    }
    let fingerprint_sha = journal::sha256_hex(checked.fingerprint.as_bytes());
    let mut compiled = Vec::new();
    let object = crate::native::compile_object(prog, |tables, names| {
        compiled = names.to_vec();
        let bundle = serde_json::json!({
            "format": BUNDLE_FORMAT,
            "staalc": env!("CARGO_PKG_VERSION"),
            "program": recorded.to_json(),
            "fingerprint_sha256": fingerprint_sha,
            "compiled": names,
            "tables": tables,
        });
        bundle.to_string().into_bytes()
    })
    .map_err(|e| (2, e))?;
    let dir = std::env::temp_dir().join(format!("staalc-build-{}-{}", std::process::id(), journal::sha256_hex(out.to_string_lossy().as_bytes()).get(..12).unwrap_or("x")));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| (2, format!("cannot create {}: {e}", dir.display())))?;
    let obj = dir.join("program.o");
    std::fs::write(&obj, &object).map_err(|e| (2, format!("cannot write {}: {e}", obj.display())))?;
    let linked = std::process::Command::new("cc")
        .arg("-o")
        .arg(out)
        .arg(&obj)
        .arg(&lib)
        .args(SYSTEM_LIBS)
        .output()
        .map_err(|e| (2, format!("cannot run the C compiler `cc` to link: {e}")))?;
    let _ = std::fs::remove_dir_all(&dir);
    if !linked.status.success() {
        return Err((2, format!("linking failed:\n{}", String::from_utf8_lossy(&linked.stderr))));
    }
    Ok(compiled.len())
}

/// Where a built executable starts: `main` in its object file calls this with the
/// program it carries and the addresses of its native code.
///
/// # Safety
/// `argv` holds `argc` NUL-terminated strings; `bundle` points to `len` bytes; `table`
/// to `n` function addresses. The generated `main` passes exactly these.
#[cfg(feature = "native")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn staal_aot_main(
    argc: i32,
    argv: *const *const std::ffi::c_char,
    bundle: *const u8,
    len: usize,
    table: *const usize,
    n: usize,
) -> i32 {
    let args: Vec<String> = (1..argc.max(0) as usize)
        .map(|i| {
            // SAFETY: see above.
            let p = unsafe { *argv.add(i) };
            // SAFETY: see above.
            unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
        })
        .collect();
    // SAFETY: see above.
    let bundle = unsafe { std::slice::from_raw_parts(bundle, len) };
    // SAFETY: see above.
    let fns = unsafe { std::slice::from_raw_parts(table, n) }.to_vec();
    match std::panic::catch_unwind(|| app_main(args, bundle, &fns)) {
        Ok(code) => code as i32,
        Err(_) => {
            eprintln!("staalc: the program crashed; this is a bug in staalc");
            70
        }
    }
}

#[cfg(feature = "native")]
fn app_main(args: Vec<String>, bundle: &[u8], fns: &[usize]) -> u8 {
    let u = |m: &str| usage_error(APP_USAGE, m);
    let Some(cmd) = args.first() else { return u("no command given") };
    let rest = &args[1..];
    if cmd == "resolve" {
        return resolve_cmd(rest, APP_USAGE);
    }
    if cmd != "run" && cmd != "resume" {
        return u(&format!("unknown command {cmd}"));
    }
    let resume = cmd == "resume";
    let required: &[&str] = if resume { &["--config", "--journal"] } else { &["--config"] };
    let opts = match parse(rest, &["--config", "--journal", "--log"], required) {
        Ok((None, opts)) => opts,
        Ok((Some(extra), _)) => return u(&format!("unexpected argument {extra}")),
        Err(e) => return u(&e),
    };
    // The program, loaded again from the sources it carries; it must be the very
    // program its native code was made from.
    let b: serde_json::Value = match serde_json::from_slice(bundle) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("staalc: this executable's program data is damaged");
            return 2;
        }
    };
    if b["format"] != serde_json::json!(BUNDLE_FORMAT) {
        eprintln!("staalc: this executable was built by a different staalc");
        return 2;
    }
    let recorded = match crate::Recorded::from_json(&b["program"]) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("staalc: {e}");
            return 2;
        }
    };
    let checked = crate::check_recorded(&recorded);
    if Some(journal::sha256_hex(checked.fingerprint.as_bytes()).as_str()) != b["fingerprint_sha256"].as_str() {
        eprintln!("staalc: this executable's program does not match the one it was built from");
        return 2;
    }
    let compiled: Vec<String> = match serde_json::from_value(b["compiled"].clone()) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("staalc: this executable's program data is damaged");
            return 2;
        }
    };
    let name = recorded.entry.0.clone();
    run(&checked, &name, &opts, resume, |prog, fp, cfg, d| {
        let native = crate::native::Native::from_linked(prog, &compiled, fns, &b["tables"])?;
        interp::run_config_with_native(prog, fp, cfg, d, native)
    })
}

/// `staalc fmt [--check] <file.staal>...`: formats each file in place, or with
/// `--check` changes nothing and lists those that are not formatted. Exit 0 when
/// every file is (or now is) formatted, 1 when `--check` found one that is not, 2 for
/// a file that cannot be read, written or formatted (it is left as it was).
fn fmt_cmd(rest: &[String]) -> u8 {
    let check = rest.iter().any(|a| a == "--check");
    let files: Vec<&String> = rest.iter().filter(|a| *a != "--check").collect();
    if let Some(bad) = files.iter().find(|f| f.starts_with('-')) {
        return usage_error(USAGE, &format!("unknown option {bad}"));
    }
    if files.is_empty() {
        return usage_error(USAGE, "fmt needs at least one file");
    }
    let mut code = 0;
    for f in files {
        let src = match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("staalc: {f}: {e}");
                code = 2;
                continue;
            }
        };
        let out = match crate::fmt::format(&src) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("staalc: {f}: {e}");
                code = 2;
                continue;
            }
        };
        if out == src {
            continue;
        }
        if check {
            println!("{f}");
            code = code.max(1);
        } else if let Err(e) = std::fs::write(f, &out) {
            eprintln!("staalc: {f}: {e}");
            code = 2;
        }
    }
    code
}

/// `staalc test`: runs each test in the files given, and in the `*_test.staal` files
/// under the folders given. A test is a function of the file itself whose name starts
/// with `test_`, with no parameters and no result; it passes if it returns, and fails
/// if it fails (`fail("...")` is its assertion). Each runs alone, as the entry of its
/// own run, without a journal, with the model and adapters of `--config` (by default a
/// scripted model with no replies) and in both engines unless `--engine` says one;
/// both must give the same result and the same log. Exit 0 when every test passes, 1
/// when one fails or a file has check errors or no tests, 2 for a usage, file or
/// config problem.
fn test_cmd(rest: &[String]) -> u8 {
    const U: &str = "usage: staalc test [--config <test.json>] [--engine interpreter|native|both] <file.staal or folder>...";
    let (mut paths, mut config, mut engine) = (Vec::new(), None, None);
    let mut args = rest.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" | "--engine" => {
                let Some(v) = args.next() else { return usage_error(U, &format!("{a} needs a value")) };
                let slot = if a == "--config" { &mut config } else { &mut engine };
                if slot.replace(v.clone()).is_some() {
                    return usage_error(U, &format!("{a} is given twice"));
                }
            }
            o if o.starts_with("--") => return usage_error(U, &format!("unknown option {o}")),
            p => paths.push(p.to_string()),
        }
    }
    if paths.is_empty() {
        return usage_error(U, "test needs a file or a folder");
    }
    let engines: &[(Engine, &str)] = match engine.as_deref() {
        None if cfg!(feature = "native") => &[(Engine::Interpreter, "interpreter"), (Engine::Native, "native")],
        None | Some("interpreter") => &[(Engine::Interpreter, "interpreter")],
        Some("native") => &[(Engine::Native, "native")],
        Some("both") => &[(Engine::Interpreter, "interpreter"), (Engine::Native, "native")],
        Some(o) => return usage_error(U, &format!("--engine is interpreter, native or both, not {o}")),
    };
    // The base of every test's run config: what --config gives, or a scripted model
    // with no replies, so a test that asks a model fails instead of reaching one.
    let mut base = serde_json::json!({ "model": { "provider": "script", "responses": [] } });
    if let Some(c) = &config {
        let parsed = std::fs::read_to_string(c).map_err(|e| format!("cannot read {c}: {e}")).and_then(|t| {
            serde_json::from_str::<serde_json::Value>(&t).map_err(|e| format!("{c} is not valid JSON: {e}"))
        });
        match parsed {
            Ok(serde_json::Value::Object(m)) => {
                if let Some(k) = ["entry", "inputs", "grants", "secrets"].iter().find(|k| m.contains_key(**k)) {
                    eprintln!("staalc: {c}: a test config cannot give \"{k}\": each test is its own entry and takes no parameters");
                    return 2;
                }
                base = serde_json::Value::Object(m);
            }
            Ok(_) => {
                eprintln!("staalc: {c}: expected a JSON object");
                return 2;
            }
            Err(e) => {
                eprintln!("staalc: {e}");
                return 2;
            }
        }
    }
    let mut files = Vec::new();
    for p in &paths {
        let path = Path::new(p);
        if path.is_dir() {
            match test_files(path) {
                Ok(mut found) if !found.is_empty() => files.append(&mut found),
                Ok(_) => {
                    eprintln!("staalc: no *_test.staal files under {p}");
                    return 1;
                }
                Err(e) => {
                    eprintln!("staalc: {e}");
                    return 2;
                }
            }
        } else {
            files.push(path.to_path_buf());
        }
    }
    let (mut passed, mut failed) = (0usize, 0usize);
    for file in &files {
        let name = file.display().to_string();
        let checked = match crate::check_path(file) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("staalc: {e}");
                return 2;
            }
        };
        let prog = match &checked.result {
            Ok(p) => p,
            Err(diags) => {
                report(&checked.files, diags);
                println!("FAIL {name}: it has check errors");
                failed += 1;
                continue;
            }
        };
        let tests: Vec<&crate::ast::FnDecl> =
            prog.fns.iter().filter(|f| f.pos.file == 0 && f.method_of.is_none() && f.name.starts_with("test_") && !f.name.contains(['.', ':', ' ', '<'])).collect();
        if tests.is_empty() {
            println!("FAIL {name}: no tests (functions named test_...)");
            failed += 1;
            continue;
        }
        for t in tests {
            let id = format!("{name}::{}", t.name);
            if !t.params.is_empty() || t.ret.is_some() || !t.generics.is_empty() {
                println!("FAIL {id}: a test takes no parameters and gives no result");
                failed += 1;
                continue;
            }
            let mut cfg = base.clone();
            cfg["entry"] = serde_json::json!(t.name);
            let mut runs = Vec::new();
            for (e, label) in engines {
                match interp::run_config_engine(prog, &checked.fingerprint, &cfg, Durability::None, *e) {
                    Ok(r) => runs.push((*label, r)),
                    Err(e) => {
                        eprintln!("staalc: {id}: {e}");
                        return 2;
                    }
                }
            }
            match verdict(&runs) {
                Ok(()) => {
                    println!("ok   {id}");
                    passed += 1;
                }
                Err(why) => {
                    println!("FAIL {id}: {why}");
                    failed += 1;
                }
            }
        }
    }
    println!("{passed} passed, {failed} failed");
    u8::from(failed > 0)
}

/// The `*_test.staal` files under `dir`, in order, outside hidden folders and `target`.
fn test_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .map(|e| e.map(|e| e.path()).map_err(|e| format!("cannot read {}: {e}", dir.display())))
        .collect::<Result<_, _>>()?;
    entries.sort();
    for p in entries {
        let file_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !file_name.starts_with('.') && file_name != "target" && file_name != "node_modules" {
                out.append(&mut test_files(&p)?);
            }
        } else if file_name.ends_with("_test.staal") {
            out.push(p);
        }
    }
    Ok(out)
}

/// Whether a test passed, given its run on each engine: every engine must give the
/// same outcome and the same log, and the outcome must be a return.
fn verdict(runs: &[(&str, RunResult)]) -> Result<(), String> {
    let outcome = |r: &RunResult| match &r.result {
        Ok(_) => Ok(()),
        Err(f) => Err(format!("{}: {}", f.kind, f.message)),
    };
    let Some(((first_label, first), rest)) = runs.split_first() else {
        return Err("no engine ran it".into());
    };
    if let Some((label, r)) = rest.iter().find(|(_, r)| !interp::same_run(r, first)) {
        let show = |o: Result<(), String>| o.err().unwrap_or_else(|| "passed".into());
        let mut why = format!("the engines disagree: {first_label} {}, {label} {}", show(outcome(first)), show(outcome(r)));
        if outcome(r) == outcome(first) {
            why.push_str(" (the same outcome, but different logs)");
        }
        return Err(why);
    }
    outcome(first)
}


/// `staalc doc`: the program's documentation in Markdown, on standard output or in
/// `-o`'s file. Only a program that passes every check is documented (exit 1
/// otherwise, nothing written); exit 2 for a file that cannot be read or written.
fn doc_cmd(file: &str, out: Option<&str>) -> u8 {
    let checked = match crate::check_path(Path::new(file)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("staalc: {e}");
            return 2;
        }
    };
    if let Err(diags) = &checked.result {
        report(&checked.files, diags);
        eprintln!("staalc: not documenting {file}: it has check errors");
        return 1;
    }
    let text = match crate::doc::document(&checked.files) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("staalc: {e}");
            return 2;
        }
    };
    match out {
        None => {
            print!("{text}");
            0
        }
        Some(o) => match std::fs::write(o, &text) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("staalc: cannot write {o}: {e}");
                2
            }
        },
    }
}

/// `staalc profile`: runs a program as `staalc run` does (without a journal) and shows
/// what each function spent: calls, steps, time, model calls and tokens, actions. The
/// run's output and result are as `run` gives them; the table is on standard error,
/// and `-o` writes it as JSON. Exit codes are `run`'s.
fn profile_cmd(file: &str, opts: &HashMap<String, String>, engine: Engine) -> u8 {
    let checked = match crate::check_path(Path::new(file)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("staalc: {e}");
            return 2;
        }
    };
    let mut fns = std::collections::BTreeMap::new();
    let code = run(&checked, file, opts, false, |prog, _, cfg, _| {
        let (r, f) = interp::run_config_profiled(prog, cfg, engine)?;
        fns = f;
        Ok(r)
    });
    if checked.result.is_err() || code == 2 {
        return code;
    }
    let mut rows: Vec<(&String, &interp::FnProfile)> = fns.iter().collect();
    rows.sort_by(|a, b| b.1.steps.cmp(&a.1.steps).then(a.0.cmp(b.0)));
    let ms = |n: u128| format!("{:.3}", n as f64 / 1e6);
    let width = rows.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0).max("function".len());
    let mut table = format!(
        "{:<width$}  {:>8}  {:>10}  {:>10}  {:>10}  {:>10}  {:>6}  {:>8}  {:>7}\n",
        "function", "calls", "steps", "own steps", "ms", "own ms", "models", "tokens", "actions"
    );
    for (name, p) in &rows {
        table.push_str(&format!(
            "{:<width$}  {:>8}  {:>10}  {:>10}  {:>10}  {:>10}  {:>6}  {:>8}  {:>7}\n",
            name,
            p.calls,
            p.steps,
            p.self_steps,
            ms(p.nanos),
            ms(p.self_nanos),
            p.model_calls,
            p.tokens,
            p.actions
        ));
    }
    eprint!("{table}");
    if let Some(out) = opts.get("-o") {
        let json: serde_json::Map<String, serde_json::Value> = fns
            .iter()
            .map(|(n, p)| {
                let v = serde_json::json!({
                    "calls": p.calls, "steps": p.steps, "own_steps": p.self_steps,
                    "nanos": p.nanos as u64, "own_nanos": p.self_nanos as u64,
                    "model_calls": p.model_calls, "tokens": p.tokens, "actions": p.actions,
                });
                (n.clone(), v)
            })
            .collect();
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(json)).unwrap_or_default() + "\n";
        if let Err(e) = std::fs::write(out, text) {
            eprintln!("staalc: cannot write {out}: {e}");
            return 2;
        }
    }
    code
}

/// `staalc debug`: runs a program as `staalc run` does (without a journal), on the
/// interpreter, stopping for the commands of `crate::debug` on standard input. Exit
/// codes are `run`'s; a run stopped with `quit` is exit 1.
fn debug_cmd(file: &str, opts: &HashMap<String, String>) -> u8 {
    let checked = match crate::check_path(Path::new(file)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("staalc: {e}");
            return 2;
        }
    };
    let files = checked.files.clone();
    run(&checked, file, opts, false, move |prog, _, cfg, _| {
        let input = Box::new(std::io::BufReader::new(std::io::stdin()));
        let d = crate::debug::Debugger::new(prog, files, input, Box::new(std::io::stdout()));
        interp::run_config_debugged(prog, cfg, Box::new(d))
    })
}

#[cfg(test)]
mod test_cmd_tests {
    use super::verdict;
    use crate::interp::{FailKind, Failure, RunResult};
    use serde_json::json;

    fn run(id: &str, result: Result<i64, &str>, extra: &str) -> RunResult {
        RunResult {
            result: result.map(|n| json!(n)).map_err(|m| Failure { kind: FailKind::Action, message: m.into(), raised: None }),
            log: vec![json!({ "event": "run_start", "run_id": id }), json!({ "event": "x", "key": format!("{id}-1{extra}") })],
        }
    }

    #[test]
    fn engines_must_agree_on_the_outcome_and_on_the_log_apart_from_the_run_id() {
        let both = |a: RunResult, b: RunResult| verdict(&[("interpreter", a), ("native", b)]);
        // Different run ids, in the log and in keys made from them: the same.
        assert_eq!(both(run("aa", Ok(1), ""), run("bb", Ok(1), "")), Ok(()));
        assert_eq!(both(run("aa", Err("boom"), ""), run("bb", Err("boom"), "")), Err("action: boom".into()));
        assert_eq!(
            both(run("aa", Ok(1), ""), run("bb", Err("boom"), "")),
            Err("the engines disagree: interpreter passed, native action: boom".into())
        );
        assert_eq!(
            both(run("aa", Err("one"), ""), run("bb", Err("two"), "")),
            Err("the engines disagree: interpreter action: one, native action: two".into())
        );
        assert_eq!(
            both(run("aa", Ok(1), ""), run("bb", Ok(1), "x")),
            Err("the engines disagree: interpreter passed, native passed (the same outcome, but different logs)".into())
        );
        // One engine alone decides by its outcome; none is never a pass.
        assert_eq!(verdict(&[("interpreter", run("aa", Ok(1), ""))]), Ok(()));
        assert!(verdict(&[]).is_err());
    }
}
