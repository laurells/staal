//! Files the tests write and then run.
//!
//! A process another test starts has a copy of every file open in the test process
//! until it runs its own program, and a file open for writing anywhere cannot be run
//! ("Text file busy" on Linux). So a file to be run is written by a shell started for
//! that alone, never opened for writing here.
//!
//! Used by the integration tests through `#[path]`, so it refers to nothing in the crate.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes `bytes` to `path`, replacing any file there, and makes it runnable (0755).
pub fn write_executable(path: &Path, bytes: &[u8]) {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("/bin/sh");
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "writing {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
}
