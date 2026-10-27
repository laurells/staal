//! The Node.js addon behind the `staal` npm package.
//!
//! Each function calls the matching function of Staal's C interface
//! (`staalc::capi`, documented in include/staal.h) and hands its JSON text back to
//! JavaScript, so Node gets exactly what C and Python callers get. Runs, resumes,
//! resolutions and checks work on libuv's thread pool and return Promises: a run
//! that waits on a model or a service never blocks the event loop.

use napi::bindgen_prelude::AsyncTask;
use napi::{Env, Error, Result, Task};
use napi_derive::napi;
use staalc::capi;
use std::ffi::{CStr, CString, c_char};

/// A JavaScript string as a C string; JavaScript strings may hold NUL, C strings cannot.
fn c(s: &str, what: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::from_reason(format!("{what} contains a NUL character")))
}

fn c_opt(s: Option<String>, what: &str) -> Result<Option<CString>> {
    s.map(|s| c(&s, what)).transpose()
}

fn ptr(s: &Option<CString>) -> *const c_char {
    s.as_ref().map_or(std::ptr::null(), |s| s.as_ptr())
}

/// Takes a string the C interface returned, copies it and frees it.
fn take(p: *mut c_char) -> Result<String> {
    if p.is_null() {
        return Err(Error::from_reason("staalc returned no result; this is a bug in staalc"));
    }
    // SAFETY: the C interface returns a NUL-terminated string it allocated; it is read
    // here and freed exactly once below.
    let text = unsafe { CStr::from_ptr(p) }.to_str().map(str::to_string);
    // SAFETY: `p` came from a staal_* function and is not used after this.
    unsafe { capi::staal_free(p) };
    text.map_err(|_| Error::from_reason("staalc returned text that is not UTF-8; this is a bug in staalc"))
}

pub enum Call {
    Check { program: CString },
    Run { program: CString, config: CString, journal: Option<CString>, engine: Option<CString>, resume: bool },
    Resolve { journal: CString, resolution: CString },
}

pub struct Job(Call);

impl Task for Job {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<String> {
        // SAFETY: every pointer is NULL or points into a CString this Job owns, which
        // outlives the call; the C interface may be called from any thread.
        let p = unsafe {
            match &self.0 {
                Call::Check { program } => capi::staal_check(program.as_ptr()),
                Call::Run { program, config, journal, engine, resume: false } => {
                    capi::staal_run(program.as_ptr(), config.as_ptr(), ptr(journal), ptr(engine))
                }
                Call::Run { program, config, journal, engine, resume: true } => {
                    capi::staal_resume(program.as_ptr(), config.as_ptr(), ptr(journal), ptr(engine))
                }
                Call::Resolve { journal, resolution } => capi::staal_resolve(journal.as_ptr(), resolution.as_ptr()),
            }
        };
        take(p)
    }

    fn resolve(&mut self, _env: Env, output: String) -> Result<String> {
        Ok(output)
    }
}

/// staalc's version.
#[napi]
pub fn version() -> Result<String> {
    // SAFETY: staal_version returns a static NUL-terminated string.
    let v = unsafe { CStr::from_ptr(capi::staal_version()) };
    v.to_str().map(str::to_string).map_err(|_| Error::from_reason("staalc's version is not UTF-8"))
}

/// Checks a program; resolves to the JSON text of the result.
#[napi]
pub fn check(program: String) -> Result<AsyncTask<Job>> {
    Ok(AsyncTask::new(Job(Call::Check { program: c(&program, "program")? })))
}

/// Runs a program with a run config (JSON text).
#[napi]
pub fn run(program: String, config: String, journal: Option<String>, engine: Option<String>) -> Result<AsyncTask<Job>> {
    Ok(AsyncTask::new(Job(Call::Run {
        program: c(&program, "program")?,
        config: c(&config, "config")?,
        journal: c_opt(journal, "journal")?,
        engine: c_opt(engine, "engine")?,
        resume: false,
    })))
}

/// Resumes a run from its journal.
#[napi]
pub fn resume(program: String, config: String, journal: String, engine: Option<String>) -> Result<AsyncTask<Job>> {
    Ok(AsyncTask::new(Job(Call::Run {
        program: c(&program, "program")?,
        config: c(&config, "config")?,
        journal: Some(c(&journal, "journal")?),
        engine: c_opt(engine, "engine")?,
        resume: true,
    })))
}

/// Resolves the action in doubt in a stopped run's journal (resolution: JSON text).
#[napi]
pub fn resolve(journal: String, resolution: String) -> Result<AsyncTask<Job>> {
    Ok(AsyncTask::new(Job(Call::Resolve { journal: c(&journal, "journal")?, resolution: c(&resolution, "resolution")? })))
}
