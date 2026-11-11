//! Local ports for tests that hand an address to code that binds it, or that need an
//! address nothing listens on.
//!
//! A port found by binding port 0 and closing the socket goes back to the kernel's
//! ephemeral pool, and a test running at the same time that binds port 0 may be handed
//! it: the code under test then cannot listen, or reaches a listener it should not.
//! Ports here come from 20000..30000, below the ephemeral ranges of Linux
//! (32768..60999) and macOS (49152..65535), so the kernel never hands them out for
//! port 0. Within a process each port is given once. Across processes, a lock on a
//! file per port, held until the process ends, keeps two test processes running at
//! the same time from taking the same port.
//!
//! Used by the library's unit tests and, through `#[path]`, by the integration tests,
//! so it refers to nothing in the crate.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::Path;
use std::sync::Mutex;

const FIRST: u16 = 20000;
const END: u16 = 30000;

struct Taken {
    next: u16,
    /// The lock files of the ports given, held until the process ends.
    held: Vec<File>,
}

static TAKEN: Mutex<Taken> = Mutex::new(Taken { next: FIRST, held: Vec::new() });

/// A port on 127.0.0.1 that no other test takes, nothing bound to it when taken.
pub fn port() -> u16 {
    let dir = std::env::temp_dir().join("staalc-test-ports");
    let mut taken = TAKEN.lock().unwrap_or_else(|e| e.into_inner());
    take(&dir, &mut taken)
}

/// The next port from `taken.next` whose lock file in `dir` this process could lock
/// and that nothing is bound to.
fn take(dir: &Path, taken: &mut Taken) -> u16 {
    std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    while taken.next < END {
        let p = taken.next;
        taken.next += 1;
        let path = dir.join(p.to_string());
        let file = match OpenOptions::new().create(true).truncate(false).write(true).open(&path) {
            Ok(f) => f,
            // Another user's lock file: that user's tests may hold this port.
            Err(e) if e.kind() == ErrorKind::PermissionDenied => continue,
            Err(e) => panic!("{}: {e}", path.display()),
        };
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Error(e)) => panic!("{}: {e}", path.display()),
        }
        if unbound(p) {
            taken.held.push(file);
            return p;
        }
    }
    panic!("no test port free in {FIRST}..{END}");
}

/// `127.0.0.1:` and a port from [`port`].
pub fn address() -> String {
    format!("127.0.0.1:{}", port())
}

/// Whether nothing is bound to `p` on the loopback addresses: 127.0.0.1, and ::1 where
/// this machine has IPv6 (some servers under test bind both).
fn unbound(p: u16) -> bool {
    if TcpListener::bind(("127.0.0.1", p)).is_err() {
        return false;
    }
    match TcpListener::bind(("::1", p)) {
        Ok(_) => true,
        Err(e) => e.kind() != ErrorKind::AddrInUse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    /// A lock directory of the test's own, and a state starting at `next`.
    fn own(name: &str, next: u16) -> (std::path::PathBuf, Taken) {
        let dir = std::env::temp_dir().join(format!("staalc-test-ports-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (dir, Taken { next, held: Vec::new() })
    }

    /// The first port from `from` nothing is bound to, held bound.
    fn bound_from(from: u16) -> (u16, TcpListener) {
        (from..END).find_map(|p| TcpListener::bind(("127.0.0.1", p)).ok().map(|l| (p, l))).unwrap()
    }

    #[test]
    fn ports_are_outside_the_ephemeral_ranges_and_never_given_twice() {
        let ports: Vec<u16> = (0..50).map(|_| port()).collect();
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ports.len(), "{ports:?}");
        assert!(ports.iter().all(|p| (FIRST..END).contains(p)), "{ports:?}");
    }

    #[test]
    fn a_port_something_is_bound_to_is_skipped() {
        let (busy, _held) = bound_from(29000);
        let (dir, mut taken) = own("bound", busy);
        let got = take(&dir, &mut taken);
        assert!(got > busy, "{got} after {busy}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_port_locked_by_another_process_is_skipped() {
        let (free, listener) = bound_from(29500);
        drop(listener);
        let (dir, mut taken) = own("locked", free);
        std::fs::create_dir_all(&dir).unwrap();
        // Another process holds the lock on that port's file while `take` runs.
        let mut child = Command::new("python3")
            .args(["-c", "import fcntl,sys,time\nf=open(sys.argv[1],'a')\nfcntl.flock(f,fcntl.LOCK_EX)\nprint('locked',flush=True)\ntime.sleep(60)"])
            .arg(dir.join(free.to_string()))
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "locked");
        let got = take(&dir, &mut taken);
        let _ = child.kill();
        let _ = child.wait();
        assert!(got > free, "{got} after {free}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_port_given_is_not_given_again_by_another_state_in_this_process() {
        let (free, listener) = bound_from(28500);
        drop(listener);
        let (dir, mut first) = own("twice", free);
        assert_eq!(take(&dir, &mut first), free);
        let mut second = Taken { next: free, held: Vec::new() };
        assert!(take(&dir, &mut second) > free);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
