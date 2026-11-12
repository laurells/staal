// C libraries for `cap ffi.<library>.<function>` (RUNTIME.md section 19).
//
// A program declares the functions it calls and their C types:
//
//   foreign ffi.zlib {
//       fn crc32(crc: u64, data: text, length: u32) -> u64
//   }
//
// and the operator grants each one with the library file and its SHA-256:
//
//   "crc": { "cap": "ffi.zlib.crc32", "library": "/usr/lib/libz.so.1.3", "sha256": "<64 hex>" }
//
// What is loaded is the file that was hashed: it is opened once, hashed through that
// descriptor and, on Linux, loaded through the same descriptor (/proc/self/fd), so
// replacing the file between the two changes nothing. Elsewhere it is loaded by path
// after hashing, and a file replaced in that moment would not be noticed.
//
// Where the C code runs is the grant's `isolation` (RUNTIME.md section 19,
// "Isolation"). `contained`, the default, runs it in a worker process of its own
// (src/interp/ffi_worker.rs) that the kernel stops from doing anything but compute:
// it cannot open a file to write, open a socket, start a program or signal another
// process, so nothing a contained C function does can reach outside its worker. Only
// Linux on x86_64 and aarch64 can contain C code; elsewhere a grant must say how it
// is run instead. `separate` runs it in a worker with no such limits, and `none` in
// this process, as before: C code run either way can do anything the process can, so
// the broker cannot check it, and every run granted one is marked unchecked. Only
// what the declaration states is enforced at the boundary: argument values fit their
// C types, text has no NUL, a returned integer fits Int and its type, returned text is
// UTF-8 and at most 1 MiB. Types are integers, `bool` and
// `text` (a NUL-terminated string the call may read only while it runs; returned text
// is copied and never freed), at most six arguments, no floating point, structs or
// variadic functions: with those, every argument is passed in an integer register,
// extended to 64 bits as its type requires, which is what the C calling conventions
// of x86_64 and aarch64 on Linux and macOS expect. Other machines are refused.

#[cfg(unix)]
use super::ffi_worker::Worker;
use crate::ast::CType;
use serde_json::Value as Json;
use std::sync::{Arc, Mutex};

/// The most arguments a C function may take here.
pub const MAX_ARGS: usize = 6;
/// The longest text a C function may return.
pub(crate) const MAX_TEXT: usize = 1 << 20;

/// Where a library's C code runs: the grant's `isolation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// In a worker process the kernel lets do nothing but compute (Linux only).
    Contained,
    /// In a worker process of its own, with no limits on what it does.
    Separate,
    /// In this process.
    None,
}

impl Isolation {
    pub const NAMES: [&'static str; 3] = ["contained", "separate", "none"];

    /// A grant's `isolation`: absent means `contained`.
    pub fn from_grant(v: Option<&Json>) -> Result<Isolation, String> {
        match v {
            None => Ok(Isolation::Contained),
            Some(Json::String(s)) => match s.as_str() {
                "contained" => Ok(Isolation::Contained),
                "separate" => Ok(Isolation::Separate),
                "none" => Ok(Isolation::None),
                _ => Err(format!("expected \"contained\", \"separate\" or \"none\", not {s:?}")),
            },
            Some(_) => Err("expected \"contained\", \"separate\" or \"none\"".into()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Isolation::Contained => "contained",
            Isolation::Separate => "separate",
            Isolation::None => "none",
        }
    }

    /// Whether C code run this way can act outside its own process, so that a run
    /// granted it is unchecked.
    pub fn unchecked(self) -> bool {
        self != Isolation::Contained
    }
}

/// Whether this build, on this machine, can contain C code.
pub fn can_contain() -> bool {
    cfg!(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))
}

/// A loaded C library. Calls into it are made one at a time: whether its functions
/// are safe to call from several threads is not known.
pub struct Library {
    exec: Exec,
    pub path: String,
    pub sha256: String,
    pub isolation: Isolation,
    calls: Mutex<()>,
}

/// Where a library was loaded.
enum Exec {
    /// In this process: the handle dlopen returned.
    Here { handle: usize },
    /// In a worker process.
    Worker(Worker),
}

/// One function of a library, with the signature the program declared for it.
pub struct Function {
    pub lib: Arc<Library>,
    pub name: String,
    /// Its address, when the library is loaded in this process.
    addr: Option<usize>,
    pub params: Vec<CType>,
    pub ret: CType,
    /// The grant says the function changes nothing, so a call cut short by a crash may
    /// be made again.
    pub repeatable: bool,
}

/// A value passed to or returned by a C function.
#[derive(Debug, Clone, PartialEq)]
pub enum CValue {
    Int(i64),
    Bool(bool),
    Text(String),
    Unit,
}

fn supported() -> Result<(), String> {
    if cfg!(all(any(target_arch = "x86_64", target_arch = "aarch64"), unix)) {
        Ok(())
    } else {
        Err("C libraries can be called only on x86_64 and aarch64, on Linux and macOS".into())
    }
}

/// Loads `path`, run as `isolation` says, if its SHA-256 is `sha256`. One library
/// loaded twice in a run is loaded once (`loaded`, by path and hash), and only ever
/// one way.
pub fn load(path: &str, sha256: &str, isolation: Isolation, loaded: &mut Vec<Arc<Library>>) -> Result<Arc<Library>, String> {
    supported()?;
    if let Some(l) = loaded.iter().find(|l| l.path == path && l.sha256 == sha256) {
        if l.isolation != isolation {
            return Err(format!("{path} is granted with isolation \"{}\" and \"{}\": one library runs one way", l.isolation.name(), isolation.name()));
        }
        return Ok(l.clone());
    }
    let exec = match isolation {
        Isolation::None => Exec::Here { handle: load_here(path, sha256)? },
        Isolation::Contained if !can_contain() => {
            return Err("C code can be contained only on Linux (x86_64 and aarch64); grant it with \"isolation\": \"separate\" or \"none\", which mark the run unchecked".into())
        }
        Isolation::Contained | Isolation::Separate => Exec::Worker(Worker::start(path, sha256, isolation == Isolation::Contained)?),
    };
    let lib = Arc::new(Library { exec, path: path.to_string(), sha256: sha256.to_string(), isolation, calls: Mutex::new(()) });
    loaded.push(lib.clone());
    Ok(lib)
}

/// What `load` checks before it loads anything, without loading it, so no C code runs: for
/// a plan (`interp::plan`), shown before a run is confirmed. That the library has the
/// function the program declares can be checked only by loading it, so a run checks it
/// when it starts. `checked` holds what was checked before, as `loaded` does for `load`.
pub fn check(path: &str, sha256: &str, isolation: Isolation, checked: &mut Vec<(String, String, Isolation)>) -> Result<(), String> {
    supported()?;
    if let Some((_, _, was)) = checked.iter().find(|(p, s, _)| p == path && s == sha256) {
        if *was != isolation {
            return Err(format!("{path} is granted with isolation \"{}\" and \"{}\": one library runs one way", was.name(), isolation.name()));
        }
        return Ok(());
    }
    if isolation == Isolation::Contained && !can_contain() {
        return Err("C code can be contained only on Linux (x86_64 and aarch64); grant it with \"isolation\": \"separate\" or \"none\", which mark the run unchecked".into());
    }
    imp::checked_file(path, sha256)?;
    checked.push((path.to_string(), sha256.to_string(), isolation));
    Ok(())
}

/// The function `name` of `lib`, with its declared signature.
pub fn function(lib: &Arc<Library>, name: &str, params: &[CType], ret: CType, repeatable: bool) -> Result<Function, String> {
    if params.len() > MAX_ARGS {
        return Err(format!("{name} takes more than {MAX_ARGS} arguments"));
    }
    let addr = match &lib.exec {
        Exec::Here { handle } => Some(imp::symbol(*handle, name).ok_or(format!("{} has no function {name}", lib.path))?),
        Exec::Worker(w) => {
            w.lookup(name).map_err(|e| format!("{}: {e}", lib.path))?;
            None
        }
    };
    Ok(Function { lib: lib.clone(), name: name.to_string(), addr, params: params.to_vec(), ret, repeatable })
}

/// Loads a library into this process, if its SHA-256 is `sha256`: its dlopen handle.
/// Used for `isolation: none`, and by the worker for the other two.
pub(crate) fn load_here(path: &str, sha256: &str) -> Result<usize, String> {
    supported()?;
    imp::open(path, sha256)
}

/// The address of function `name` in a library loaded in this process.
pub(crate) fn symbol_here(handle: usize, name: &str) -> Option<usize> {
    imp::symbol(handle, name)
}

/// Calls the C function at `addr` in this process, with `args` checked against
/// `params` first; the value it returns read as `ret`.
///
/// # Safety
/// `addr` must be a function of a library loaded here. Whether it really takes
/// `params` and returns `ret` nothing can check: that is the declaration's promise.
pub(crate) unsafe fn call_here(addr: usize, params: &[CType], ret: CType, args: &[CValue]) -> Result<CValue, String> {
    if args.len() != params.len() {
        return Err(format!("takes {} arguments, given {}", params.len(), args.len()));
    }
    let mut texts = Vec::new();
    let mut regs = Vec::with_capacity(args.len());
    for (t, v) in params.iter().zip(args) {
        regs.push(register(*t, v, &mut texts)?);
    }
    // SAFETY: the caller's contract. Each argument is in the register its declared
    // type needs, and `texts` outlives the call.
    let raw = unsafe { imp::call(addr, &regs) };
    drop(texts);
    value(ret, raw)
}

/// Whether `v`, returned by a C function in a worker, is a value of type `t`: the
/// worker is not trusted to have read the register as declared.
fn returned(t: CType, v: CValue) -> Result<CValue, String> {
    let fits = match (t, &v) {
        (CType::I8, CValue::Int(n)) => i8::try_from(*n).is_ok(),
        (CType::I16, CValue::Int(n)) => i16::try_from(*n).is_ok(),
        (CType::I32, CValue::Int(n)) => i32::try_from(*n).is_ok(),
        (CType::I64, CValue::Int(_)) => true,
        (CType::U8, CValue::Int(n)) => u8::try_from(*n).is_ok(),
        (CType::U16, CValue::Int(n)) => u16::try_from(*n).is_ok(),
        (CType::U32, CValue::Int(n)) => u32::try_from(*n).is_ok(),
        (CType::U64, CValue::Int(n)) => *n >= 0,
        (CType::Bool, CValue::Bool(_)) | (CType::Void, CValue::Unit) => true,
        (CType::Text, CValue::Text(s)) => s.len() <= MAX_TEXT,
        _ => false,
    };
    if fits {
        Ok(v)
    } else {
        Err(format!("its worker returned {v:?}, which is not a C {}", t.name()))
    }
}

/// Converts one argument to the 64-bit register value its C type expects: signed
/// types sign-extended, unsigned ones zero-extended.
fn register(t: CType, v: &CValue, texts: &mut Vec<std::ffi::CString>) -> Result<u64, String> {
    let range = |n: i64, lo: i64, hi: i64| {
        if n < lo || n > hi { Err(format!("{n} does not fit in a C {}", t.name())) } else { Ok(n) }
    };
    Ok(match (t, v) {
        (CType::I8, CValue::Int(n)) => range(*n, i8::MIN.into(), i8::MAX.into())? as u64,
        (CType::I16, CValue::Int(n)) => range(*n, i16::MIN.into(), i16::MAX.into())? as u64,
        (CType::I32, CValue::Int(n)) => range(*n, i32::MIN.into(), i32::MAX.into())? as u64,
        (CType::I64, CValue::Int(n)) => *n as u64,
        (CType::U8, CValue::Int(n)) => range(*n, 0, u8::MAX.into())? as u64,
        (CType::U16, CValue::Int(n)) => range(*n, 0, u16::MAX.into())? as u64,
        (CType::U32, CValue::Int(n)) => range(*n, 0, u32::MAX.into())? as u64,
        (CType::U64, CValue::Int(n)) => range(*n, 0, i64::MAX)? as u64,
        (CType::Bool, CValue::Bool(b)) => u64::from(*b),
        (CType::Text, CValue::Text(s)) => {
            let c = std::ffi::CString::new(s.as_str()).map_err(|_| "text passed to C cannot contain a NUL character".to_string())?;
            let p = c.as_ptr() as u64;
            texts.push(c);
            p
        }
        _ => return Err(format!("a {v:?} cannot be passed as a C {}", t.name())),
    })
}

/// The value a C function returned in its integer register, by its declared type.
/// Only the bits of the type are read: the rest of the register is unspecified.
fn value(t: CType, raw: u64) -> Result<CValue, String> {
    Ok(match t {
        CType::I8 => CValue::Int((raw as u8 as i8).into()),
        CType::I16 => CValue::Int((raw as u16 as i16).into()),
        CType::I32 => CValue::Int((raw as u32 as i32).into()),
        CType::I64 => CValue::Int(raw as i64),
        CType::U8 => CValue::Int((raw as u8).into()),
        CType::U16 => CValue::Int((raw as u16).into()),
        CType::U32 => CValue::Int((raw as u32).into()),
        CType::U64 => CValue::Int(i64::try_from(raw).map_err(|_| format!("the C function returned {raw}, which does not fit in Int"))?),
        CType::Bool => CValue::Bool(raw as u8 != 0),
        CType::Void => CValue::Unit,
        CType::Text => {
            let p = raw as usize as *const libc::c_char;
            if p.is_null() {
                return Err("the C function returned no text (a null pointer)".into());
            }
            // SAFETY: the declaration says the function returns a NUL-terminated string;
            // at most MAX_TEXT + 1 bytes are read looking for its end.
            let n = unsafe { libc::strnlen(p, MAX_TEXT + 1) };
            if n > MAX_TEXT {
                return Err(format!("the C function returned text longer than {MAX_TEXT} bytes"));
            }
            // SAFETY: `n` bytes from `p` were just found to precede a NUL.
            let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), n) };
            CValue::Text(String::from_utf8(bytes.to_vec()).map_err(|_| "the C function returned text that is not UTF-8".to_string())?)
        }
    })
}

impl Function {
    /// Whether `args` fit the declared parameters, without calling anything.
    pub fn check(&self, args: &[CValue]) -> Result<(), String> {
        if args.len() != self.params.len() {
            return Err(format!("{} takes {} arguments, given {}", self.name, self.params.len(), args.len()));
        }
        let mut texts = Vec::new();
        for (t, v) in self.params.iter().zip(args) {
            register(*t, v, &mut texts).map_err(|e| format!("{}: {e}", self.name))?;
        }
        Ok(())
    }

    /// Whether a call cut short by a crash may be made again: the grant says it changes
    /// nothing, or it is contained, so nothing it did can have reached outside its
    /// worker, which did not outlive the crash.
    pub fn may_repeat(&self) -> bool {
        self.repeatable || self.lib.isolation == Isolation::Contained
    }

    /// Calls the function. Arguments are checked against the declaration first; a
    /// value that does not fit is refused before anything is called.
    pub fn call(&self, args: &[CValue]) -> Result<CValue, String> {
        self.check(args)?;
        let _one_at_a_time = self.lib.calls.lock().unwrap_or_else(|p| p.into_inner());
        let got = match (&self.lib.exec, self.addr) {
            // SAFETY: none can be given. The function is called with the signature the
            // program declared, which nothing checks against the library; this is why
            // a run that calls C in this process is marked unchecked.
            (Exec::Here { .. }, Some(addr)) => unsafe { call_here(addr, &self.params, self.ret, args) },
            (Exec::Worker(w), _) => w.call(&self.name, &self.params, self.ret, args).and_then(|v| returned(self.ret, v)),
            (Exec::Here { .. }, None) => Err("was looked up in no library".into()),
        };
        got.map_err(|e| format!("{}: {e}", self.name))
    }
}

#[cfg(unix)]
mod imp {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;

    /// The library file at `path`, checked: a regular file no other user can change,
    /// whose SHA-256 is `sha256`. Opened and read, never loaded.
    pub fn checked_file(path: &str, sha256: &str) -> Result<std::fs::File, String> {
        let mut file = std::fs::File::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
        let meta = file.metadata().map_err(|e| format!("{path}: {e}"))?;
        if !meta.is_file() {
            return Err(format!("{path} is not a regular file"));
        }
        if meta.mode() & 0o002 != 0 {
            return Err(format!("{path} can be changed by any user; a library run from here cannot be"));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| format!("cannot read {path}: {e}"))?;
        let have: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if have != sha256 {
            return Err(format!("{path} has SHA-256 {have}, not the {sha256} the grant names"));
        }
        Ok(file)
    }

    /// Every library this process has loaded, by the SHA-256 of the file it was loaded
    /// from, with its handle and, on Linux, the descriptor it was loaded through. Nothing
    /// is ever unloaded, so nothing leaves this list.
    ///
    /// The dynamic loader returns a library it already has when asked for one by a name it
    /// has loaded before. Loading through /proc/self/fd/N, a descriptor closed afterwards
    /// lets the next library's descriptor take the same number N, and the loader would give
    /// back the first library for the second: so each descriptor stays open as long as its
    /// library is loaded, and no name is ever reused. On macOS, which loads by path, a file
    /// replaced at a path already loaded gives back the old library: so a handle the loader
    /// returns that is already this list's, for another SHA-256, is refused.
    struct Loaded {
        sha256: String,
        handle: usize,
        _through: Option<std::fs::File>,
    }

    static LOADED: std::sync::Mutex<Vec<Loaded>> = std::sync::Mutex::new(Vec::new());

    pub fn open(path: &str, sha256: &str) -> Result<usize, String> {
        let file = checked_file(path, sha256)?;
        let mut loaded = LOADED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // The same bytes loaded before: the same library, already initialised.
        if let Some(l) = loaded.iter().find(|l| l.sha256 == sha256) {
            return Ok(l.handle);
        }
        // Load the file that was hashed: on Linux through this descriptor.
        let target = if cfg!(target_os = "linux") {
            use std::os::fd::AsRawFd;
            format!("/proc/self/fd/{}", file.as_raw_fd())
        } else {
            path.to_string()
        };
        let c = std::ffi::CString::new(target).map_err(|_| "a library path cannot contain a NUL character".to_string())?;
        // SAFETY: loading a library runs its initialisers, which nothing can check;
        // the run is marked unchecked for it.
        let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            // SAFETY: dlerror returns a NUL-terminated message, or null.
            let why = unsafe {
                let e = libc::dlerror();
                if e.is_null() { "unknown error".to_string() } else { std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned() }
            };
            return Err(format!("cannot load {path}: {why}"));
        }
        let handle = handle as usize;
        if let Some(other) = loaded.iter().find(|l| l.handle == handle) {
            return Err(format!(
                "{path}: the dynamic loader gave back a library this process already loaded from another file (SHA-256 \
                 {}), not the one with SHA-256 {sha256}; a library cannot be replaced while the process runs, so start \
                 it again to load this one",
                other.sha256
            ));
        }
        // Never unloaded: unloading C code can leave the process pointing into it. The
        // descriptor stays open as long, so its /proc/self/fd name is never reused.
        loaded.push(Loaded { sha256: sha256.to_string(), handle, _through: cfg!(target_os = "linux").then_some(file) });
        Ok(handle)
    }

    pub fn symbol(handle: usize, name: &str) -> Option<usize> {
        let c = std::ffi::CString::new(name).ok()?;
        // SAFETY: `handle` came from dlopen and is never closed.
        let p = unsafe { libc::dlsym(handle as *mut libc::c_void, c.as_ptr()) };
        (!p.is_null()).then_some(p as usize)
    }

    /// # Safety
    /// `addr` must be a C function taking `regs.len()` integer-class arguments.
    pub unsafe fn call(addr: usize, regs: &[u64]) -> u64 {
        type F0 = unsafe extern "C" fn() -> u64;
        type F1 = unsafe extern "C" fn(u64) -> u64;
        type F2 = unsafe extern "C" fn(u64, u64) -> u64;
        type F3 = unsafe extern "C" fn(u64, u64, u64) -> u64;
        type F4 = unsafe extern "C" fn(u64, u64, u64, u64) -> u64;
        type F5 = unsafe extern "C" fn(u64, u64, u64, u64, u64) -> u64;
        type F6 = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64;
        // SAFETY: the caller's contract; each transmute turns a code address into the
        // function pointer type of its arity.
        unsafe {
            match *regs {
                [] => std::mem::transmute::<usize, F0>(addr)(),
                [a] => std::mem::transmute::<usize, F1>(addr)(a),
                [a, b] => std::mem::transmute::<usize, F2>(addr)(a, b),
                [a, b, c] => std::mem::transmute::<usize, F3>(addr)(a, b, c),
                [a, b, c, d] => std::mem::transmute::<usize, F4>(addr)(a, b, c, d),
                [a, b, c, d, e] => std::mem::transmute::<usize, F5>(addr)(a, b, c, d, e),
                [a, b, c, d, e, f] => std::mem::transmute::<usize, F6>(addr)(a, b, c, d, e, f),
                _ => unreachable!("at most MAX_ARGS arguments"),
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    pub fn checked_file(_: &str, _: &str) -> Result<std::fs::File, String> {
        Err("C libraries can be called only on x86_64 and aarch64, on Linux and macOS".into())
    }
    pub fn open(_: &str, _: &str) -> Result<usize, String> {
        Err("C libraries can be called only on x86_64 and aarch64, on Linux and macOS".into())
    }
    pub fn symbol(_: usize, _: &str) -> Option<usize> {
        None
    }
    /// # Safety
    /// Never reached: no library can be opened here, so there is no address to call.
    pub unsafe fn call(_: usize, _: &[u64]) -> u64 {
        unreachable!("no library can be loaded")
    }
}

/// No worker can be started where C cannot be called at all.
#[cfg(not(unix))]
struct Worker;

#[cfg(not(unix))]
impl Worker {
    fn start(_: &str, _: &str, _: bool) -> Result<Worker, String> {
        Err("C libraries can be called only on x86_64 and aarch64, on Linux and macOS".into())
    }
    fn lookup(&self, _: &str) -> Result<(), String> {
        Err("no worker".into())
    }
    fn call(&self, _: &str, _: &[CType], _: CType, _: &[CValue]) -> Result<CValue, String> {
        Err("no worker".into())
    }
}
