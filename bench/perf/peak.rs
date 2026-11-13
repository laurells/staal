// Runs a command and reports, on the file named first, its wall time in nanoseconds and
// the peak resident set its process reached, in KiB, from the kernel's accounting at
// exit (wait4's ru_maxrss). Linux keeps a process's peak across exec, so the command is
// started from this launcher, whose own memory is small: that is the floor any command
// measured here can show (RESULTS.md gives it). The command's output passes through.
//
// Usage: peak <report-file> <command> [args...]

use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::time::Instant;

#[repr(C)]
struct Timeval {
    sec: i64,
    usec: i64,
}

#[repr(C)]
struct Rusage {
    utime: Timeval,
    stime: Timeval,
    maxrss: i64,
    rest: [i64; 13],
}

unsafe extern "C" {
    fn wait4(pid: i32, status: *mut i32, options: i32, usage: *mut Rusage) -> i32;
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (report, cmd) = args.split_first().expect("usage: peak <report-file> <command> [args...]");
    let start = Instant::now();
    let child = Command::new(&cmd[0]).args(&cmd[1..]).spawn().expect("cannot start the command");
    let mut status = 0;
    let mut usage = Rusage { utime: Timeval { sec: 0, usec: 0 }, stime: Timeval { sec: 0, usec: 0 }, maxrss: 0, rest: [0; 13] };
    // SAFETY: wait4 on our own child, with pointers to live values.
    let pid = unsafe { wait4(child.id() as i32, &mut status, 0, &mut usage) };
    let elapsed = start.elapsed();
    assert_eq!(pid, child.id() as i32, "wait4 failed");
    std::mem::forget(child);
    let code = std::process::ExitStatus::from_raw(status).code().unwrap_or(-1);
    std::fs::write(report, format!("{} {} {}\n", code, elapsed.as_nanos(), usage.maxrss)).expect("cannot write the report");
    std::process::exit(code);
}
