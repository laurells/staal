// The command line, shared by `staalc` and by executables `staalc build` makes.
//
//   staalc check   <file.staal>
//   staalc fetch   [<project folder>]
//   staalc build   <file.staal> -o <executable> [--runtime <libstaalc.a>]
//   staalc run     <file.staal> --config <run.json> [--journal <run.journal>] [--log <run.jsonl>] [--engine native]
//   staalc resume  <file.staal> --config <run.json> --journal <run.journal> [--log <run.jsonl>] [--engine native]
//   staalc resolve --journal <run.journal> (--performed <receipt> | --status <code> [--body <text>]
//                                            | --failed <reason> | --retry)
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
  staalc fetch   [<project folder>]
  staalc build   <file.staal> -o <executable> [--runtime <libstaalc.a>]
  staalc run     <file.staal> --config <run.json> [--journal <run.journal>] [--log <run.jsonl>] [--engine native]
  staalc resume  <file.staal> --config <run.json> --journal <run.journal> [--log <run.jsonl>] [--engine native]
  staalc resolve --journal <run.journal> (--performed <receipt> | --status <code> [--body <text>]
                                           | --failed <reason> | --retry)";

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
    let opts = match parse(rest, &["--journal", "--performed", "--status", "--body", "--failed", "--retry"], &["--journal"]) {
        Ok((None, opts)) => opts,
        Ok((Some(extra), _)) => return u(&format!("unexpected argument {extra}")),
        Err(e) => return u(&e),
    };
    let given = ["--performed", "--status", "--failed", "--retry"].iter().filter(|k| opts.contains_key(**k)).count();
    if given != 1 {
        return u("resolve needs exactly one of --performed <receipt>, --status <code>, --failed <reason> or --retry");
    }
    if opts.contains_key("--body") && !opts.contains_key("--status") {
        return u("--body goes with --status");
    }
    let resolution = if let Some(receipt) = opts.get("--performed") {
        journal::Resolution::Performed { receipt: receipt.clone() }
    } else if let Some(code) = opts.get("--status") {
        let Ok(status) = code.parse::<u16>() else { return u("--status needs an HTTP status code such as 200") };
        journal::Resolution::Responded { status, body: opts.get("--body").cloned().unwrap_or_default() }
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
