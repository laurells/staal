#![cfg(unix)]
// The C-function worker (RUNTIME.md section 19, "Isolation").
//
// A library granted with `isolation: contained` or `separate` is loaded in a process of
// its own, started fresh (never forked from this one, which may hold other threads'
// locks), that serves calls over one socket: a frame is a 4-byte little-endian length
// and that many bytes of JSON. The parent trusts nothing the worker says: every reply
// is size-capped, must be exactly one of `{"ok": ...}` or `{"err": "..."}`, and every
// returned value is checked against the declared type (ffi.rs, `returned`). A worker
// that breaks the protocol, crashes or is stopped is killed and reaped, and every later
// call to its library fails with the reason: its C state is gone.
//
// The worker is the `staalc` a program embedding the runtime names (`use_executable`),
// or else the one `STAALC_FFI_WORKER` names; otherwise
// this executable restarted with `WORKER_ARG` when this process is `staalc` or a
// Staal-built executable (`hosted_here`); otherwise, for libstaalc loaded by another
// program, the `staalc` installed beside libstaalc. Its version must match. Its environment is emptied but for the library search path, so no secret a
// program's environment holds reaches C code, and every descriptor but the socket and
// a standard error that goes nowhere is closed before the library is loaded.
//
// Contained (Linux, x86_64 and aarch64): two seccomp filters (src: `lockdown`). The
// first, before the library is loaded, lets the loader and the library's initialisers
// read files but not create, write or delete one, open a socket, start or signal a
// process, or change who the process is. The second, after loading, lets it only
// compute: use memory, wait, read and write the socket and exit. Anything else kills
// the worker (SIGSYS), and the call fails. Filters can only be added, never removed.

use super::ffi::{self, CValue};
use crate::ast::CType;
use serde_json::{json, Map, Value as Json};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// The argument that starts this executable as a worker.
pub const WORKER_ARG: &str = "--staal-ffi-worker";
/// The largest frame either side sends or accepts: room for six arguments of text and
/// a returned text of `ffi::MAX_TEXT`, each escaped as JSON at worst.
const MAX_FRAME: usize = 64 << 20;
const VERSION: &str = env!("CARGO_PKG_VERSION");

static HOSTED_HERE: AtomicBool = AtomicBool::new(false);
static CHOSEN: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Names the `staalc` to run as the worker, for a program that embeds the runtime: it
/// takes the place of `STAALC_FFI_WORKER` and of looking for one.
pub fn use_executable(path: PathBuf) {
    *CHOSEN.lock().unwrap_or_else(|p| p.into_inner()) = Some(path);
}

/// Says that this executable serves as its own worker: `staalc` and built executables
/// call this first, and answer `WORKER_ARG` with `serve`.
pub fn hosted_here() {
    HOSTED_HERE.store(true, Ordering::SeqCst);
}

/// A worker serving one library, seen from the process that started it.
pub struct Worker {
    conn: Mutex<Conn>,
}

struct Conn {
    child: Child,
    sock: UnixStream,
    /// Why the worker can serve no more calls, once it cannot.
    dead: Option<String>,
}

impl Worker {
    /// Starts a worker and has it load `path`, contained or not.
    pub fn start(path: &str, sha256: &str, contained: bool) -> Result<Worker, String> {
        let exe = executable()?;
        let (ours, theirs) = UnixStream::pair().map_err(|e| format!("cannot make the C worker's socket: {e}"))?;
        no_sigpipe(&ours)?;
        let theirs_out = theirs.try_clone().map_err(|e| format!("cannot make the C worker's socket: {e}"))?;
        let mut cmd = Command::new(&exe);
        cmd.arg(WORKER_ARG).env_clear().current_dir("/");
        // The library search path, so a library finds what it depends on as it would here.
        for v in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"] {
            if let Some(x) = std::env::var_os(v) {
                cmd.env(v, x);
            }
        }
        cmd.stdin(Stdio::from(OwnedFd::from(theirs))).stdout(Stdio::from(OwnedFd::from(theirs_out))).stderr(Stdio::null());
        let child = cmd.spawn().map_err(|e| format!("cannot start the C worker {}: {e}", exe.display()))?;
        // The command holds this process's copies of the worker's ends of the socket;
        // closing them is what lets a worker that dies be seen to hang up.
        drop(cmd);
        let w = Worker { conn: Mutex::new(Conn { child, sock: ours, dead: None }) };
        let ready = w.request(&json!({ "op": "load", "staalc": VERSION, "library": path, "sha256": sha256, "contained": contained }))?;
        if ready != json!({ "staalc": VERSION, "contained": contained }) {
            let why = format!("the C worker {} answered {ready} to loading the library", exe.display());
            w.conn.lock().unwrap_or_else(|p| p.into_inner()).stop(&why);
            return Err(why);
        }
        Ok(w)
    }

    /// Whether the library has function `name`.
    pub fn lookup(&self, name: &str) -> Result<(), String> {
        match self.request(&json!({ "op": "lookup", "name": name }))? {
            Json::Null => Ok(()),
            other => Err(self.broken(&format!("answered {other} to a lookup"))),
        }
    }

    /// Calls function `name` with `args`, as declared: the value the worker says it
    /// returned, of the variant `ret` has (its range is checked by the caller).
    pub fn call(&self, name: &str, params: &[CType], ret: CType, args: &[CValue]) -> Result<CValue, String> {
        let a: Vec<Json> = args.iter().map(to_json).collect();
        let p: Vec<&str> = params.iter().map(|t| t.name()).collect();
        let v = self.request(&json!({ "op": "call", "name": name, "params": p, "ret": ret.name(), "args": a }))?;
        from_json(ret, &v).ok_or_else(|| self.broken(&format!("returned {v} for a C {}", ret.name())))
    }

    /// One request and its reply. An `{"err": ...}` reply is the call's failure; a
    /// broken channel or a malformed reply ends the worker.
    fn request(&self, msg: &Json) -> Result<Json, String> {
        let mut c = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(why) = &c.dead {
            return Err(why.clone());
        }
        let reply = match write_frame(&c.sock, msg.to_string().as_bytes(), true).and_then(|()| read_frame(&mut &c.sock)) {
            Ok(Some(r)) => r,
            // A length past the limit: whatever wrote it was not the worker's own reply.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => return Err(c.stop("the C worker sent a malformed reply")),
            // It hung up: why is in how it ended.
            Ok(None) | Err(_) => return Err(c.reap()),
        };
        match serde_json::from_slice::<Json>(&reply) {
            Ok(Json::Object(m)) if m.len() == 1 => match m.into_iter().next() {
                Some((k, v)) if k == "ok" => Ok(v),
                Some((k, Json::String(e))) if k == "err" => Err(e),
                _ => Err(c.stop("the C worker sent a malformed reply")),
            },
            _ => Err(c.stop("the C worker sent a malformed reply")),
        }
    }

    fn broken(&self, what: &str) -> String {
        self.conn.lock().unwrap_or_else(|p| p.into_inner()).stop(&format!("the C worker {what}"))
    }
}

impl Conn {
    /// Ends the worker for `why`, which is then the answer to every later call.
    fn stop(&mut self, why: &str) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.dead = Some(why.to_string());
        why.to_string()
    }

    /// The worker hung up: why, from how it ended.
    fn reap(&mut self) -> String {
        let _ = self.child.kill();
        let why = match self.child.wait() {
            Ok(s) => ended(s),
            Err(e) => format!("the C worker could not be waited for: {e}"),
        };
        self.dead = Some(why.clone());
        why
    }
}

#[cfg(unix)]
fn ended(s: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (s.code(), s.signal()) {
        (_, Some(libc::SIGSYS)) => "the C code made a system call that contained C code may not make, and was stopped".into(),
        (_, Some(n)) => format!("the C code crashed (signal {n})"),
        (Some(n), _) => format!("the C worker exited with code {n}"),
        _ => "the C worker ended".into(),
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let c = self.conn.get_mut().unwrap_or_else(|p| p.into_inner());
        if c.dead.is_none() {
            let _ = c.child.kill();
            let _ = c.child.wait();
        }
    }
}

/// The variable an operator sets to the `staalc` to run as the worker, where none is
/// found: an absolute path. Its version must match, as any worker's must.
pub const WORKER_VAR: &str = "STAALC_FFI_WORKER";

/// The executable to run as a worker (see the top of this file).
fn executable() -> Result<PathBuf, String> {
    if let Some(p) = CHOSEN.lock().unwrap_or_else(|p| p.into_inner()).clone() {
        if !p.is_absolute() {
            return Err(format!("the C worker must be an absolute path to staalc, not {}", p.display()));
        }
        return checked_file(p);
    }
    if let Some(p) = std::env::var_os(WORKER_VAR) {
        let p = PathBuf::from(p);
        if !p.is_absolute() {
            return Err(format!("{WORKER_VAR} must be an absolute path to staalc, not {}", p.display()));
        }
        return checked_file(p);
    }
    if HOSTED_HERE.load(Ordering::SeqCst) {
        // On Linux, the very file this process runs, even if its path was replaced since.
        return if cfg!(target_os = "linux") {
            Ok(PathBuf::from("/proc/self/exe"))
        } else {
            std::env::current_exe().map_err(|e| format!("cannot find this executable to run the C worker: {e}"))
        };
    }
    // libstaalc in another program: the staalc installed beside it.
    // SAFETY: an all-zero Dl_info is a valid value to be filled in.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: dladdr reads the address of a function of this library and fills `info`.
    let found = unsafe { libc::dladdr(executable as *const libc::c_void, &mut info) };
    if found == 0 || info.dli_fname.is_null() {
        return Err("cannot find where libstaalc is installed, to run the C worker beside it".into());
    }
    // SAFETY: dladdr filled `dli_fname` with a NUL-terminated path.
    let me = PathBuf::from(unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy().into_owned());
    checked_file(me.with_file_name("staalc")).map_err(|e| format!("{e} (the staalc installed beside libstaalc; set {WORKER_VAR} to use another)"))
}

/// `exe`, if it is a regular file that only its owner can change.
fn checked_file(exe: PathBuf) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(&exe).map_err(|e| format!("cannot use {} as the C worker: {e}", exe.display()))?;
    if !meta.is_file() || meta.permissions().mode() & 0o002 != 0 {
        return Err(format!("{} is not a file only its owner can change, so it cannot be the C worker", exe.display()));
    }
    Ok(exe)
}

#[cfg(target_os = "macos")]
fn no_sigpipe(s: &UnixStream) -> Result<(), String> {
    let one: libc::c_int = 1;
    // SAFETY: a valid socket and option of the size given.
    let r = unsafe {
        libc::setsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_NOSIGPIPE, (&one as *const libc::c_int).cast(), std::mem::size_of::<libc::c_int>() as libc::socklen_t)
    };
    if r == 0 { Ok(()) } else { Err(format!("cannot set up the C worker's socket: {}", io::Error::last_os_error())) }
}

#[cfg(not(target_os = "macos"))]
fn no_sigpipe(_: &UnixStream) -> Result<(), String> {
    Ok(())
}

/// Writes one frame. The parent sends (`by_send`), so that a worker gone away is an
/// error, never a SIGPIPE; the worker writes, the one way out its filter leaves it,
/// and if the parent is gone a SIGPIPE ends it, which is what it should do.
fn write_frame(s: &impl AsRawFd, body: &[u8], by_send: bool) -> io::Result<()> {
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    let mut rest: &[u8] = &out;
    #[cfg(target_os = "linux")]
    let flags = libc::MSG_NOSIGNAL;
    #[cfg(not(target_os = "linux"))]
    let flags = 0;
    while !rest.is_empty() {
        // SAFETY: `rest` is valid for its length; the descriptor is a socket.
        let n = unsafe {
            if by_send {
                libc::send(s.as_raw_fd(), rest.as_ptr().cast(), rest.len(), flags)
            } else {
                libc::write(s.as_raw_fd(), rest.as_ptr().cast(), rest.len())
            }
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        rest = &rest[n as usize..];
    }
    Ok(())
}

/// Reads one frame: None at a clean end of the stream before a frame starts.
fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

fn to_json(v: &CValue) -> Json {
    match v {
        CValue::Int(n) => json!(n),
        CValue::Bool(b) => json!(b),
        CValue::Text(t) => json!(t),
        CValue::Unit => Json::Null,
    }
}

/// A JSON value as the C value of type `t` it must be, or None.
fn from_json(t: CType, v: &Json) -> Option<CValue> {
    match (t, v) {
        (CType::Bool, Json::Bool(b)) => Some(CValue::Bool(*b)),
        (CType::Text, Json::String(s)) => Some(CValue::Text(s.clone())),
        (CType::Void, Json::Null) => Some(CValue::Unit),
        (CType::Bool | CType::Text | CType::Void, _) => None,
        (_, j) => j.as_i64().map(CValue::Int),
    }
}

// --- The worker's side ---

/// A raw descriptor as a reader, without owning it.
struct Fd(i32);

impl Read for Fd {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `buf` is valid for its length.
        let n = unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
    }
}

impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> i32 {
        self.0
    }
}

/// Runs this process as a worker, on the socket that is its standard input and
/// output. Returns the exit code.
pub fn serve() -> u8 {
    let mut chan = Fd(0);
    let reply_to = Fd(1);
    let reply = |r: Result<Json, String>| {
        let m = match r {
            Ok(v) => json!({ "ok": v }),
            Err(e) => json!({ "err": e }),
        };
        write_frame(&reply_to, m.to_string().as_bytes(), false).is_ok()
    };
    let Ok(Some(first)) = read_frame(&mut chan) else { return 2 };
    let load = match serde_json::from_slice::<Json>(&first) {
        Ok(Json::Object(m)) => m,
        _ => return 2,
    };
    let (handle, contained) = match start(&load) {
        Ok(x) => x,
        Err(e) => {
            reply(Err(e));
            return 2;
        }
    };
    if !reply(Ok(json!({ "staalc": VERSION, "contained": contained }))) {
        return 2;
    }
    let mut found: Vec<(String, usize)> = Vec::new();
    loop {
        let frame = match read_frame(&mut chan) {
            Ok(Some(f)) => f,
            Ok(None) => return 0,
            Err(_) => return 2,
        };
        let r = match serde_json::from_slice::<Json>(&frame) {
            Ok(Json::Object(m)) => handle_request(handle, &m, &mut found),
            _ => Err("a malformed request".into()),
        };
        if !reply(r) {
            return 2;
        }
    }
}

/// Loads the library the first request names, locked down as it asks.
fn start(load: &Map<String, Json>) -> Result<(usize, bool), String> {
    if load.get("op") != Some(&json!("load")) {
        return Err("the first request must load a library".into());
    }
    match load.get("staalc").and_then(Json::as_str) {
        Some(v) if v == VERSION => {}
        other => return Err(format!("this worker is staalc {VERSION}, asked by {}", other.unwrap_or("an unknown version"))),
    }
    let path = load.get("library").and_then(Json::as_str).ok_or("no library named")?;
    let sha = load.get("sha256").and_then(Json::as_str).ok_or("no SHA-256 given")?;
    let contained = load.get("contained").and_then(Json::as_bool).ok_or("no isolation given")?;
    close_from(3)?;
    if contained {
        lockdown::loading()?;
    }
    let handle = ffi::load_here(path, sha)?;
    // Whatever the library's initialisers opened goes before the last filter.
    close_from(3)?;
    if contained {
        lockdown::computing()?;
    }
    Ok((handle, contained))
}

fn handle_request(handle: usize, m: &Map<String, Json>, found: &mut Vec<(String, usize)>) -> Result<Json, String> {
    let name = m.get("name").and_then(Json::as_str).ok_or("no function named")?;
    let addr = match found.iter().find(|(n, _)| n == name) {
        Some((_, a)) => *a,
        None => {
            let a = ffi::symbol_here(handle, name).ok_or(format!("has no function {name}"))?;
            found.push((name.to_string(), a));
            a
        }
    };
    match m.get("op").and_then(Json::as_str) {
        Some("lookup") => Ok(Json::Null),
        Some("call") => {
            let types = |k: &str| -> Result<Vec<CType>, String> {
                m.get(k)
                    .and_then(Json::as_array)
                    .ok_or(format!("no {k}"))?
                    .iter()
                    .map(|t| t.as_str().and_then(CType::parse).ok_or(format!("a bad type in {k}")))
                    .collect()
            };
            let params = types("params")?;
            let ret = m.get("ret").and_then(Json::as_str).and_then(CType::parse).ok_or("a bad return type")?;
            let raw = m.get("args").and_then(Json::as_array).ok_or("no args")?;
            if raw.len() != params.len() || params.len() > ffi::MAX_ARGS {
                return Err("arguments that do not match the parameters".into());
            }
            let mut args = Vec::with_capacity(raw.len());
            for (t, a) in params.iter().zip(raw) {
                args.push(from_json(*t, a).ok_or("an argument of the wrong type")?);
            }
            // SAFETY: `addr` is a function of the library this worker loaded; that it takes
            // `params` and returns `ret` is the declaration's promise, which is why this
            // runs here and not in the run's own process.
            unsafe { ffi::call_here(addr, &params, ret, &args) }.map(|v| to_json(&v))
        }
        _ => Err("an unknown request".into()),
    }
}

/// Closes every descriptor from `low` up.
fn close_from(low: u32) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: close_range takes plain integers; closing descriptors this worker does
        // not use is the point.
        let r = unsafe { libc::syscall(libc::SYS_close_range, low, u32::MAX, 0u32) };
        if r == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ENOSYS) {
            return Err(format!("cannot close the worker's descriptors: {e}"));
        }
    }
    // Without close_range (Linux before 5.9, and macOS): every descriptor up to the
    // process's limit.
    // SAFETY: getrlimit fills the struct given.
    let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return Err(format!("cannot read the worker's descriptor limit: {}", io::Error::last_os_error()));
    }
    let high = if lim.rlim_cur == libc::RLIM_INFINITY { i32::MAX as u64 } else { lim.rlim_cur as u64 };
    let high = high.min(1 << 20) as i32;
    for fd in low as i32..high {
        // SAFETY: closing a descriptor number, open or not.
        unsafe { libc::close(fd) };
    }
    Ok(())
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
mod lockdown {
    use seccompiler::{BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule};
    use std::collections::BTreeMap;

    /// What computing needs: reading and writing the socket (and standard error, which
    /// goes nowhere); memory, for allocating; futexes, for locks; time, sleeping and
    /// yielding; and ending, by exiting or by a signal to itself (abort).
    const COMPUTE: &[libc::c_long] = &[
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_mprotect,
        libc::SYS_madvise,
        libc::SYS_brk,
        libc::SYS_futex,
        libc::SYS_sched_yield,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];

    /// What loading a library adds: opening files to read (only to read: see
    /// `install`), reading their metadata (each of the calls glibc versions use for
    /// it), reading and mapping them, closing descriptors, and installing the second
    /// filter.
    const LOAD: &[libc::c_long] = &[
        libc::SYS_close,
        libc::SYS_close_range,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_faccessat,
        libc::SYS_lseek,
        libc::SYS_pread64,
        libc::SYS_prctl,
        libc::SYS_seccomp,
    ];

    fn install(names: &[&[libc::c_long]], read_only_open: bool) -> Result<(), String> {
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for list in names {
            for s in *list {
                rules.insert(*s, Vec::new());
            }
        }
        if read_only_open {
            // openat(dirfd, path, flags, mode): only for reading, and never creating or
            // truncating (O_TMPFILE needs write access, so it is refused with these).
            let mask = (libc::O_ACCMODE | libc::O_CREAT | libc::O_TRUNC) as u64;
            let cond = SeccompCondition::new(2, SeccompCmpArgLen::Dword, SeccompCmpOp::MaskedEq(mask), libc::O_RDONLY as u64)
                .map_err(|e| format!("cannot build the worker's filter: {e}"))?;
            rules.insert(libc::SYS_openat, vec![SeccompRule::new(vec![cond]).map_err(|e| format!("cannot build the worker's filter: {e}"))?]);
            // fcntl only to ask about a descriptor (Rust's debug builds check one is still
            // open when a file is dropped), never to change or duplicate one.
            let mut asks = Vec::new();
            for cmd in [libc::F_GETFD, libc::F_GETFL] {
                let c = SeccompCondition::new(1, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, cmd as u64)
                    .map_err(|e| format!("cannot build the worker's filter: {e}"))?;
                asks.push(SeccompRule::new(vec![c]).map_err(|e| format!("cannot build the worker's filter: {e}"))?);
            }
            rules.insert(libc::SYS_fcntl, asks);
        }
        // A signal only to this process itself (abort raises SIGABRT this way), so a C
        // function that aborts is reported as crashed, and no other process is reached.
        // SAFETY: getpid cannot fail.
        let me = unsafe { libc::getpid() } as u64;
        let own = SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, me).map_err(|e| format!("cannot build the worker's filter: {e}"))?;
        rules.insert(libc::SYS_tgkill, vec![SeccompRule::new(vec![own]).map_err(|e| format!("cannot build the worker's filter: {e}"))?]);
        let arch = std::env::consts::ARCH.try_into().map_err(|e| format!("cannot build the worker's filter: {e:?}"))?;
        let filter = SeccompFilter::new(rules, SeccompAction::KillProcess, SeccompAction::Allow, arch)
            .map_err(|e| format!("cannot build the worker's filter: {e}"))?;
        let prog: BpfProgram = filter.try_into().map_err(|e| format!("cannot build the worker's filter: {e}"))?;
        seccompiler::apply_filter_all_threads(&prog).map_err(|e| format!("cannot install the worker's filter: {e}"))
    }

    /// The first filter: loading a library, reading but changing nothing.
    pub fn loading() -> Result<(), String> {
        install(&[COMPUTE, LOAD], true)
    }

    /// The second filter: computing only.
    pub fn computing() -> Result<(), String> {
        install(&[COMPUTE], false)
    }
}

#[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
mod lockdown {
    pub fn loading() -> Result<(), String> {
        Err("C code can be contained only on Linux (x86_64 and aarch64)".into())
    }
    pub fn computing() -> Result<(), String> {
        Err("C code can be contained only on Linux (x86_64 and aarch64)".into())
    }
}
