// staalc command line: see src/cli.rs for the commands and exit codes.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::ExitCode::from(staalc::cli::staalc_main(args))
}
