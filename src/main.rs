// intentc command line.
//
//   intentc check   <file.int>
//   intentc run     <file.int> --config <run.json> [--journal <run.journal>] [--log <run.jsonl>]
//   intentc resume  <file.int> --config <run.json> --journal <run.journal> [--log <run.jsonl>]
//   intentc resolve --journal <run.journal> (--performed <receipt> | --failed <reason> | --retry)
//
// Exit codes:
//   0  success
//   1  the program has check errors, or its run failed
//   2  a usage, file, config or journal problem: nothing was run
//   3  the run stopped before finishing and can be continued: an action is in doubt
//      (resolve it, then resume) or the journal could not be written (resume)

use intentc::interp::{self, journal, Durability, FailKind};
use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage:
  intentc check   <file.int>
  intentc run     <file.int> --config <run.json> [--journal <run.journal>] [--log <run.jsonl>]
  intentc resume  <file.int> --config <run.json> --journal <run.journal> [--log <run.jsonl>]
  intentc resolve --journal <run.journal> (--performed <receipt> | --failed <reason> | --retry)";

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("intentc: {msg}\n{USAGE}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { return usage_error("no command given") };
    let rest = &args[1..];
    match cmd.as_str() {
        "check" => match parse(rest, &[], &[]) {
            Ok((Some(file), _)) => check(&file),
            Ok((None, _)) => usage_error("check needs a file"),
            Err(e) => usage_error(&e),
        },
        "run" => match parse(rest, &["--config", "--journal", "--log"], &["--config"]) {
            Ok((Some(file), opts)) => {
                let durability = match opts.get("--journal") {
                    Some(j) => Durability::Create(Path::new(j)),
                    None => Durability::None,
                };
                run(&file, &opts["--config"], durability, opts.get("--log"))
            }
            Ok((None, _)) => usage_error("run needs a program file"),
            Err(e) => usage_error(&e),
        },
        "resume" => match parse(rest, &["--config", "--journal", "--log"], &["--config", "--journal"]) {
            Ok((Some(file), opts)) => {
                run(&file, &opts["--config"], Durability::Resume(Path::new(&opts["--journal"])), opts.get("--log"))
            }
            Ok((None, _)) => usage_error("resume needs a program file"),
            Err(e) => usage_error(&e),
        },
        "resolve" => match parse(rest, &["--journal", "--performed", "--failed", "--retry"], &["--journal"]) {
            Ok((None, opts)) => resolve(&opts),
            Ok((Some(extra), _)) => usage_error(&format!("unexpected argument {extra}")),
            Err(e) => usage_error(&e),
        },
        other => usage_error(&format!("unknown command {other}")),
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
        if a.starts_with("--") {
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

fn read(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("intentc: cannot read {path}: {e}");
        ExitCode::from(2)
    })
}

fn report(files: &[intentc::SourceFile], diags: &[intentc::checker::Diagnostic]) {
    for d in diags {
        eprintln!("{}", intentc::format_diag_in(files, d));
    }
    eprintln!("{} error(s)", diags.len());
}

/// Loads and checks the program at `path` with all its modules.
fn load(path: &str) -> Result<intentc::Checked, ExitCode> {
    intentc::check_path(Path::new(path)).map_err(|e| {
        eprintln!("intentc: {e}");
        ExitCode::from(2)
    })
}

fn check(path: &str) -> ExitCode {
    let checked = match load(path) {
        Ok(c) => c,
        Err(code) => return code,
    };
    match checked.result {
        Ok(_) => {
            println!("{path}: ok");
            ExitCode::SUCCESS
        }
        Err(diags) => {
            report(&checked.files, &diags);
            ExitCode::FAILURE
        }
    }
}

fn run(file: &str, config: &str, durability: Durability, log: Option<&String>) -> ExitCode {
    let checked = match load(file) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let prog = match checked.result {
        Ok(p) => p,
        Err(diags) => {
            report(&checked.files, &diags);
            eprintln!("intentc: not running {file}: it has check errors");
            return ExitCode::FAILURE;
        }
    };
    let cfg_text = match read(config) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let cfg: serde_json::Value = match serde_json::from_str(&cfg_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("intentc: {config} is not valid JSON: {e}");
            return ExitCode::from(2);
        }
    };
    let out = match interp::run_config_durable(&prog, &checked.fingerprint, &cfg, durability) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("intentc: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(log) = log {
        let mut text = String::new();
        for event in &out.log {
            text.push_str(&event.to_string());
            text.push('\n');
        }
        if let Err(e) = std::fs::write(log, text) {
            eprintln!("intentc: cannot write log {log}: {e}");
            return ExitCode::from(2);
        }
    }
    match out.result {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(f) if matches!(f.kind, FailKind::InDoubt | FailKind::Journal) => {
            eprintln!("intentc: run stopped: {}: {}", f.kind, f.message);
            ExitCode::from(3)
        }
        Err(f) => {
            eprintln!("intentc: run failed: {}: {}", f.kind, f.message);
            ExitCode::FAILURE
        }
    }
}

fn resolve(opts: &HashMap<String, String>) -> ExitCode {
    let resolution = match (opts.get("--performed"), opts.get("--failed"), opts.contains_key("--retry")) {
        (Some(receipt), None, false) => journal::Resolution::Performed { receipt: receipt.clone() },
        (None, Some(reason), false) => journal::Resolution::Failed { message: reason.clone() },
        (None, None, true) => journal::Resolution::Retry,
        _ => return usage_error("resolve needs exactly one of --performed <receipt>, --failed <reason> or --retry"),
    };
    match journal::Journal::resolve(Path::new(&opts["--journal"]), resolution) {
        Ok(summary) => {
            println!("{summary}; now resume the run");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("intentc: {e}");
            ExitCode::from(2)
        }
    }
}
