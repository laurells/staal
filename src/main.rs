// intentc command line.
//
//   intentc check <file.int>
//   intentc run <file.int> --config <run.json> [--log <run.jsonl>]
//
// Exit codes: 0 success; 1 the program has check errors or its run failed;
// 2 a usage, file or run-config problem (nothing was run).

use std::process::ExitCode;

const USAGE: &str = "usage:\n  intentc check <file.int>\n  intentc run <file.int> --config <run.json> [--log <run.jsonl>]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") if args.len() == 2 => check(&args[1]),
        Some("run") => run(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn read(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("intentc: cannot read {path}: {e}");
        ExitCode::from(2)
    })
}

fn report(path: &str, diags: &[intentc::checker::Diagnostic]) {
    for d in diags {
        eprintln!("{}", intentc::format_diag(path, d));
    }
    eprintln!("{} error(s)", diags.len());
}

fn check(path: &str) -> ExitCode {
    let src = match read(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    match intentc::check_program(&src) {
        Ok(_) => {
            println!("{path}: ok");
            ExitCode::SUCCESS
        }
        Err(diags) => {
            report(path, &diags);
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> ExitCode {
    let (mut file, mut config, mut log) = (None, None, None);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "--log" if i + 1 >= args.len() => {
                eprintln!("intentc: {} needs a file path\n{USAGE}", args[i]);
                return ExitCode::from(2);
            }
            "--config" => {
                config = Some(args[i + 1].clone());
                i += 2;
            }
            "--log" => {
                log = Some(args[i + 1].clone());
                i += 2;
            }
            a if a.starts_with("--") => {
                eprintln!("intentc: unknown option {a}\n{USAGE}");
                return ExitCode::from(2);
            }
            a if file.is_none() => {
                file = Some(a.to_string());
                i += 1;
            }
            a => {
                eprintln!("intentc: unexpected argument {a}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(file), Some(config)) = (file, config) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let src = match read(&file) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let prog = match intentc::check_program(&src) {
        Ok(p) => p,
        Err(diags) => {
            report(&file, &diags);
            eprintln!("intentc: not running {file}: it has check errors");
            return ExitCode::FAILURE;
        }
    };
    let cfg_text = match read(&config) {
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
    let out = match intentc::interp::run_config(&prog, &cfg) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("intentc: {config}: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(log) = log {
        let mut text = String::new();
        for event in &out.log {
            text.push_str(&event.to_string());
            text.push('\n');
        }
        if let Err(e) = std::fs::write(&log, text) {
            eprintln!("intentc: cannot write log {log}: {e}");
            return ExitCode::from(2);
        }
    }
    match out.result {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(f) => {
            eprintln!("intentc: run failed: {}: {}", f.kind, f.message);
            ExitCode::FAILURE
        }
    }
}
