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
// C code can do anything the process can, so the broker cannot check it: every run
// granted one of these is marked unchecked. Only what the declaration states is
// enforced at the boundary: argument values fit their C types, text has no NUL, a
// returned integer fits Int, returned text is UTF-8. Types are integers, `bool` and
// `text` (a NUL-terminated string the call may read only while it runs; returned text
// is copied and never freed), at most six arguments, no floating point, structs or
// variadic functions: with those, every argument is passed in an integer register,
// extended to 64 bits as its type requires, which is what the C calling conventions
// of x86_64 and aarch64 on Linux and macOS expect. Other machines are refused.

use crate::ast::CType;
use std::sync::{Arc, Mutex};

/// The most arguments a C function may take here.
pub const MAX_ARGS: usize = 6;
/// The longest text a C function may return.
const MAX_TEXT: usize = 1 << 20;

/// A loaded C library. Calls into it are made one at a time: whether its functions
/// are safe to call from several threads is not known.
pub struct Library {
    handle: usize,
    pub path: String,
    pub sha256: String,
    calls: Mutex<()>,
}

/// One function of a library, with the signature the program declared for it.
pub struct Function {
    pub lib: Arc<Library>,
    pub name: String,
    addr: usize,
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

/// Loads `path` if its SHA-256 is `sha256`. One library loaded twice in a run is
/// loaded once (`loaded`, by path and hash).
pub fn load(path: &str, sha256: &str, loaded: &mut Vec<Arc<Library>>) -> Result<Arc<Library>, String> {
    supported()?;
    if let Some(l) = loaded.iter().find(|l| l.path == path && l.sha256 == sha256) {
        return Ok(l.clone());
    }
    let lib = imp::open(path, sha256)?;
    let lib = Arc::new(lib);
    loaded.push(lib.clone());
    Ok(lib)
}

/// The function `name` of `lib`, with its declared signature.
pub fn function(lib: &Arc<Library>, name: &str, params: &[CType], ret: CType, repeatable: bool) -> Result<Function, String> {
    if params.len() > MAX_ARGS {
        return Err(format!("{name} takes more than {MAX_ARGS} arguments"));
    }
    let addr = imp::symbol(lib.handle, name).ok_or(format!("{} has no function {name}", lib.path))?;
    Ok(Function { lib: lib.clone(), name: name.to_string(), addr, params: params.to_vec(), ret, repeatable })
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
            let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, n) };
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

    /// Calls the function. Arguments are checked against the declaration first; a
    /// value that does not fit is refused before anything is called.
    pub fn call(&self, args: &[CValue]) -> Result<CValue, String> {
        if args.len() != self.params.len() {
            return Err(format!("{} takes {} arguments, given {}", self.name, self.params.len(), args.len()));
        }
        let mut texts = Vec::new();
        let mut regs = Vec::with_capacity(args.len());
        for (t, v) in self.params.iter().zip(args) {
            regs.push(register(*t, v, &mut texts).map_err(|e| format!("{}: {e}", self.name))?);
        }
        let raw = {
            let _one_at_a_time = self.lib.calls.lock().unwrap_or_else(|p| p.into_inner());
            // SAFETY: none can be given. The function is called with the signature the
            // program declared, which nothing checks against the library; this is why
            // a run that can call C is marked unchecked. Each argument is in the
            // register its declared type needs, and `texts` outlives the call.
            unsafe { imp::call(self.addr, &regs) }
        };
        drop(texts);
        value(self.ret, raw).map_err(|e| format!("{}: {e}", self.name))
    }
}

#[cfg(unix)]
mod imp {
    use super::Library;
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;

    pub fn open(path: &str, sha256: &str) -> Result<Library, String> {
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
        drop(file);
        if handle.is_null() {
            // SAFETY: dlerror returns a NUL-terminated message, or null.
            let why = unsafe {
                let e = libc::dlerror();
                if e.is_null() { "unknown error".to_string() } else { std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned() }
            };
            return Err(format!("cannot load {path}: {why}"));
        }
        // Never unloaded: unloading C code can leave the process pointing into it.
        Ok(Library { handle: handle as usize, path: path.to_string(), sha256: sha256.to_string(), calls: std::sync::Mutex::new(()) })
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
    use super::Library;
    pub fn open(_: &str, _: &str) -> Result<Library, String> {
        Err("C libraries can be called only on x86_64 and aarch64, on Linux and macOS".into())
    }
    pub fn symbol(_: usize, _: &str) -> Option<usize> {
        None
    }
    pub unsafe fn call(_: usize, _: &[u64]) -> u64 {
        unreachable!("no library can be loaded")
    }
}
