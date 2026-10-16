use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 || args[1] != "check" {
        eprintln!("usage: intentc check <file.int>");
        return ExitCode::from(2);
    }
    let path = &args[2];
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("intentc: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let diags = intentc::compile_check(&src);
    if diags.is_empty() {
        println!("{path}: ok");
        ExitCode::SUCCESS
    } else {
        for d in &diags {
            eprintln!("{}", intentc::format_diag(path, d));
        }
        eprintln!("{} error(s)", diags.len());
        ExitCode::FAILURE
    }
}
