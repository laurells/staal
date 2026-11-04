// The command line's own answers: help and the version, asked for, are printed on
// standard output with success; a command it does not know is an error with the usage.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");

fn staalc(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(BIN).args(args).output().unwrap();
    (o.status.code(), String::from_utf8(o.stdout).unwrap(), String::from_utf8(o.stderr).unwrap())
}

#[test]
fn help_asked_for_is_the_usage_on_standard_output() {
    for flag in ["--help", "-h", "help"] {
        let (code, out, err) = staalc(&[flag]);
        assert_eq!(code, Some(0), "{flag}: {err}");
        assert!(out.starts_with("usage:\n") && out.contains("staalc check") && out.contains("staalc mcp"), "{flag}: {out}");
        assert_eq!(err, "", "{flag}");
    }
}

#[test]
fn the_version_is_staalcs() {
    let (code, out, _) = staalc(&["--version"]);
    assert_eq!((code, out), (Some(0), format!("staalc {}\n", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn an_unknown_command_is_an_error_with_the_usage() {
    let (code, out, err) = staalc(&["frobnicate"]);
    assert_eq!(code, Some(2));
    assert_eq!(out, "");
    assert!(err.starts_with("staalc: unknown command frobnicate\nusage:\n"), "{err}");
    // Help with more after it is not a request for help.
    let (code, _, err) = staalc(&["--help", "check"]);
    assert_eq!(code, Some(2), "{err}");
}
