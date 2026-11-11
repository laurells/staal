//! Local ports for tests that hand an address to code that binds it, or that need an
//! address nothing listens on.
//!
//! A port found by binding port 0 and closing the socket goes back to the kernel's
//! ephemeral pool, and a test running at the same time that binds port 0 may be handed
//! it: the code under test then cannot listen, or reaches a listener it should not.
//! Ports here come from 20000 up, skipping the ports this machine's kernel hands out
//! for port 0 and for outgoing connections, read from the machine itself
//! (`/proc/sys/net/ipv4/ip_local_port_range` on Linux, the `net.inet.ip.portrange`
//! sysctls on macOS) rather than assumed. Within a process each port is given once. Across processes, a lock on a
//! file per port, held until the process ends, keeps two test processes running at
//! the same time from taking the same port.
//!
//! Used by the library's unit tests and, through `#[path]`, by the integration tests,
//! so it refers to nothing in the crate.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const FIRST: u16 = 20000;

/// An inclusive range of ports.
type Range = (u16, u16);

struct Taken {
    /// The next port to try; `None` past 65535.
    next: Option<u16>,
    /// The lock files of the ports given, held until the process ends.
    held: Vec<File>,
    /// Each port passed over because something was bound to it, and what: kept so a
    /// test that fails can say why a port was not given.
    skipped: Vec<(u16, String)>,
}

static TAKEN: Mutex<Taken> = Mutex::new(Taken { next: Some(FIRST), held: Vec::new(), skipped: Vec::new() });

/// A port on 127.0.0.1 that no other test takes, nothing bound to it when taken.
pub fn port() -> u16 {
    static EPHEMERAL: OnceLock<Vec<Range>> = OnceLock::new();
    let skip = EPHEMERAL.get_or_init(ephemeral);
    let dir = std::env::temp_dir().join("staalc-test-ports");
    let mut taken = TAKEN.lock().unwrap_or_else(|e| e.into_inner());
    take(&dir, &mut taken, skip)
}

/// The ranges this machine's kernel hands out ports from without being asked for one.
#[cfg(target_os = "linux")]
fn ephemeral() -> Vec<Range> {
    let path = "/proc/sys/net/ipv4/ip_local_port_range";
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let n: Vec<u16> = text.split_whitespace().map(|w| w.parse().unwrap_or_else(|e| panic!("{path}: {text:?}: {e}"))).collect();
    assert_eq!(n.len(), 2, "{path}: {text:?}");
    vec![(n[0], n[1])]
}

/// The ranges this machine's kernel hands out ports from without being asked for one:
/// the default range, and the high range a socket may ask for.
#[cfg(target_os = "macos")]
fn ephemeral() -> Vec<Range> {
    let names = ["net.inet.ip.portrange.first", "net.inet.ip.portrange.last", "net.inet.ip.portrange.hifirst", "net.inet.ip.portrange.hilast"];
    let out = std::process::Command::new("/usr/sbin/sysctl").arg("-n").args(names).output().expect("/usr/sbin/sysctl");
    assert!(out.status.success(), "sysctl: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let n: Vec<u16> = text.split_whitespace().map(|w| w.parse().unwrap_or_else(|e| panic!("sysctl: {text:?}: {e}"))).collect();
    assert_eq!(n.len(), 4, "sysctl: {text:?}");
    vec![(n[0], n[1]), (n[2], n[3])]
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn ephemeral() -> Vec<Range> {
    panic!("test ports: this system's ephemeral port range is not known")
}

/// The next port from `taken.next`, outside every range in `skip`, whose lock file in
/// `dir` this process could lock and that nothing is bound to.
fn take(dir: &Path, taken: &mut Taken, skip: &[Range]) -> u16 {
    std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    while let Some(p) = taken.next {
        taken.next = p.checked_add(1);
        if skip.iter().any(|&(lo, hi)| (lo..=hi).contains(&p)) {
            continue;
        }
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
        match bound(p) {
            None => {
                taken.held.push(file);
                return p;
            }
            Some(why) => taken.skipped.push((p, why)),
        }
    }
    panic!("no test port free from {FIRST}, outside {skip:?}; bound: {:?}", taken.skipped);
}

/// `127.0.0.1:` and a port from [`port`].
pub fn address() -> String {
    format!("127.0.0.1:{}", port())
}

/// What is bound to `p`, if anything: checked on 127.0.0.1, on every IPv4 address,
/// and on ::1 and every IPv6 address where this machine has IPv6 (some servers under
/// test bind both loopback addresses). Each check's socket is closed before the next.
fn bound(p: u16) -> Option<String> {
    for host in ["127.0.0.1", "0.0.0.0", "::1", "::"] {
        let ipv6 = host.contains(':');
        match TcpListener::bind((host, p)) {
            Ok(listener) => drop(listener),
            // No IPv6 on this machine: nothing can be bound there.
            Err(e) if ipv6 && e.kind() != ErrorKind::AddrInUse => {}
            Err(e) => return Some(format!("{host}: {e}")),
        }
    }
    None
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
        (dir, Taken { next: Some(next), held: Vec::new(), skipped: Vec::new() })
    }

    /// The first port from `from` nothing is bound to, by the same checks `take` makes.
    fn free_from(from: u16) -> u16 {
        (from..=u16::MAX).find(|p| bound(*p).is_none()).unwrap()
    }

    #[test]
    fn ports_are_outside_the_ephemeral_ranges_and_never_given_twice() {
        let ports: Vec<u16> = (0..50).map(|_| port()).collect();
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ports.len(), "{ports:?}");
        let skip = ephemeral();
        assert!(ports.iter().all(|p| *p >= FIRST && !skip.iter().any(|&(lo, hi)| (lo..=hi).contains(p))), "{ports:?} {skip:?}");
    }

    #[test]
    fn a_port_something_is_bound_to_is_skipped() {
        let busy = free_from(29000);
        let _held = TcpListener::bind(("127.0.0.1", busy)).unwrap();
        let (dir, mut taken) = own("bound", busy);
        let got = take(&dir, &mut taken, &[]);
        assert!(got > busy, "{got} after {busy}");
        assert_eq!(taken.skipped.first().map(|s| s.0), Some(busy), "{:?}", taken.skipped);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_port_locked_by_another_process_is_skipped() {
        let free = free_from(29500);
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
        let got = take(&dir, &mut taken, &[]);
        let _ = child.kill();
        let _ = child.wait();
        assert!(got > free, "{got} after {free}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_port_given_is_not_given_again_by_another_state_in_this_process() {
        let free = free_from(28500);
        let (dir, mut first) = own("twice", free);
        assert_eq!(take(&dir, &mut first, &[]), free, "passed over: {:?}", first.skipped);
        let mut second = Taken { next: Some(free), held: Vec::new(), skipped: Vec::new() };
        assert!(take(&dir, &mut second, &[]) > free);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ports_in_a_range_to_skip_are_never_given() {
        let free = free_from(28000);
        let (dir, mut taken) = own("skip", free);
        let got = take(&dir, &mut taken, &[(free, free.saturating_add(99))]);
        assert!(got > free.saturating_add(99), "{got} within {free}..={}", free.saturating_add(99));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_machines_ephemeral_ranges_are_read_and_include_a_port_the_kernel_hands_out() {
        let skip = ephemeral();
        let given = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(skip.iter().any(|&(lo, hi)| lo <= hi && (lo..=hi).contains(&given)), "{given} outside {skip:?}");
    }
}
