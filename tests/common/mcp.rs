// A client for `staalc mcp` in tests: starts the server with a manifest, writes
// messages to its standard input, and collects every line it writes to standard
// output (each must be a JSON-RPC message) and to standard error.

use serde_json::{json, Value as Json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_staalc");

/// The `_meta` every 2026-07-28 request carries.
pub fn meta() -> Json {
    json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} })
}

/// A fresh folder for one test, with every link resolved.
pub fn case_dir(name: &str) -> PathBuf {
    std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let base = std::fs::canonicalize(env!("CARGO_TARGET_TMPDIR")).unwrap().join("mcp").join(name);
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

pub struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Json>,
    /// Messages received but not yet asked for.
    held: Vec<Json>,
    /// Every line written to standard output, as written.
    pub raw: Arc<Mutex<Vec<String>>>,
    pub stderr: Arc<Mutex<String>>,
}

impl Mcp {
    pub fn start(manifest: &Path, env: &[(&str, &str)]) -> Mcp {
        let mut cmd = Command::new(BIN);
        cmd.arg("mcp").arg(manifest).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("staalc must start");
        let stdout = child.stdout.take().unwrap();
        let mut stderr_pipe = child.stderr.take().unwrap();
        let (tx, rx) = channel();
        let raw = Arc::new(Mutex::new(Vec::new()));
        let r2 = raw.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                r2.lock().unwrap().push(line.clone());
                let v: Json = serde_json::from_str(&line).unwrap_or_else(|e| json!({ "NOT_JSON": line, "why": e.to_string() }));
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let e2 = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stderr_pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                e2.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        Mcp { stdin: child.stdin.take(), child, rx, held: Vec::new(), raw, stderr }
    }

    pub fn send_raw(&mut self, text: &str) {
        let stdin = self.stdin.as_mut().expect("standard input is open");
        stdin.write_all(text.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    pub fn send(&mut self, msg: &Json) {
        self.send_raw(&msg.to_string());
    }

    /// Sends a 2026-07-28 request (with `_meta`) and does not wait.
    pub fn ask(&mut self, id: Json, method: &str, mut params: Json) {
        if params.get("_meta").is_none() {
            params["_meta"] = meta();
        }
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    /// The next message for which `pick` holds, waiting at most `secs`.
    pub fn wait_for(&mut self, secs: u64, pick: impl Fn(&Json) -> bool) -> Option<Json> {
        if let Some(i) = self.held.iter().position(&pick) {
            return Some(self.held.remove(i));
        }
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(m) if pick(&m) => return Some(m),
                Ok(m) => self.held.push(m),
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// The response to request `id`.
    pub fn response(&mut self, id: &Json) -> Json {
        let id2 = id.clone();
        self.wait_for(60, move |m| m["id"] == id2 && (m.get("result").is_some() || m.get("error").is_some()))
            .unwrap_or_else(|| panic!("no response to {id}; stderr:\n{}", self.stderr.lock().unwrap()))
    }

    pub fn request(&mut self, id: Json, method: &str, params: Json) -> Json {
        self.ask(id.clone(), method, params);
        self.response(&id)
    }

    /// Calls a tool (2026-07-28) and gives its result.
    pub fn call(&mut self, id: u64, name: &str, args: Json) -> Json {
        let r = self.request(json!(id), "tools/call", json!({ "name": name, "arguments": args }));
        r.get("result").cloned().unwrap_or_else(|| panic!("tools/call {name} failed: {r}"))
    }

    /// Closes standard input and waits for the server to exit.
    pub fn close(mut self) -> (Option<i32>, String, Vec<String>) {
        drop(self.stdin.take());
        let end = Instant::now() + Duration::from_secs(60);
        let code = loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                break s.code();
            }
            if Instant::now() > end {
                let _ = self.child.kill();
                panic!("the server did not exit after its input closed; stderr:\n{}", self.stderr.lock().unwrap());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Let the reader threads take the last bytes.
        std::thread::sleep(Duration::from_millis(100));
        let err = self.stderr.lock().unwrap().clone();
        let raw = self.raw.lock().unwrap().clone();
        (code, err, raw)
    }

    /// Waits for a server that crashes by itself (a failpoint) to exit.
    pub fn wait_exit(mut self) -> (Option<i32>, String) {
        let end = Instant::now() + Duration::from_secs(60);
        let code = loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                break s.code();
            }
            if Instant::now() > end {
                let _ = self.child.kill();
                panic!("the server did not exit; stderr:\n{}", self.stderr.lock().unwrap());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        std::thread::sleep(Duration::from_millis(100));
        (code, self.stderr.lock().unwrap().clone())
    }

    pub fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs `staalc mcp <manifest>` with empty input: the server checks the manifest,
/// recovers, and exits. Gives the exit code and standard error.
pub fn start_and_stop(manifest: &Path, env: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut cmd = Command::new(BIN);
    cmd.arg("mcp").arg(manifest).stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(out.stdout.is_empty(), "nothing but MCP messages on standard output: {:?}", String::from_utf8_lossy(&out.stdout));
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Writes `manifest` (with `server.journals` set to `<dir>/journals` unless given)
/// into `dir` and returns its path.
pub fn write_manifest(dir: &Path, mut manifest: Json) -> PathBuf {
    if manifest["server"].get("journals").is_none() {
        manifest["server"]["journals"] = json!("journals");
    }
    let p = dir.join("manifest.json");
    std::fs::write(&p, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    p
}

/// Every file under `dir`, recursively.
pub fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}
