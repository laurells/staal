// The language server (`staalc lsp`, src/lsp.rs) against the compiler.
//
// The phase 9 gate: the editor and the compiler report exactly the same errors. One
// server opens every Staal program in the repository (each fixture, each module
// case's main file, each runtime program, the examples and packages), and for each
// one its diagnostics, in every file they are in, must be exactly what `staalc
// check` prints for the same file: the same files, lines, columns, codes and
// messages, in any order. The protocol itself is checked too.

use serde_json::{json, Value as Json};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

struct Lsp {
    child: Arc<Mutex<Child>>,
    to: ChildStdin,
    from: BufReader<ChildStdout>,
    next_id: i64,
    /// When the read now waiting for the server began.
    waiting: Arc<Mutex<Option<Instant>>>,
}

impl Lsp {
    fn start() -> Lsp {
        let mut child = Command::new(BIN).arg("lsp").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let to = child.stdin.take().unwrap();
        let from = BufReader::new(child.stdout.take().unwrap());
        // A server that stops answering fails the test instead of hanging it: one
        // reply awaited for a minute and it is killed, so the read finds its output
        // closed.
        let child = Arc::new(Mutex::new(child));
        let waiting: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let (watched, since) = (Arc::clone(&child), Arc::clone(&waiting));
        std::thread::spawn(move || loop {
            if !matches!(watched.lock().unwrap().try_wait(), Ok(None)) {
                return;
            }
            if since.lock().unwrap().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
                let _ = watched.lock().unwrap().kill();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        });
        Lsp { child, to, from, next_id: 1, waiting }
    }

    fn send_raw(&mut self, body: &str) {
        write!(self.to, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.to.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Json) {
        self.send_raw(&json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string());
    }

    /// Sends a request and returns its response, and every message that came first.
    fn request(&mut self, method: &str, params: Json) -> (Json, Vec<Json>) {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string());
        let mut before = Vec::new();
        loop {
            let m = self.read();
            if m["id"] == json!(id) && m.get("method").is_none() {
                return (m, before);
            }
            before.push(m);
        }
    }

    fn read(&mut self) -> Json {
        *self.waiting.lock().unwrap() = Some(Instant::now());
        let m = self.read_one();
        *self.waiting.lock().unwrap() = None;
        m
    }

    fn read_one(&mut self) -> Json {
        let mut len = 0;
        loop {
            let mut line = String::new();
            assert!(self.from.read_line(&mut line).unwrap() > 0, "the server closed its output");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                len = v.parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        self.from.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Closes the input and returns the exit code.
    fn finish(self) -> Option<i32> {
        drop(self.to);
        loop {
            if let Some(status) = self.child.lock().unwrap().try_wait().unwrap() {
                return status.code();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn uri(p: &Path) -> String {
    url::Url::from_file_path(p).unwrap().to_string()
}

/// (path, line, column in characters, code, message): one error, as the compiler
/// prints it or as the editor is told it.
type Diag = (PathBuf, usize, usize, String, String);

/// What `staalc check` prints for `file`, as a set of errors. A problem that is not
/// a diagnostic (exit 2) is at line 1, column 1 of `file`, with no code.
fn compiler(file: &Path) -> BTreeSet<Diag> {
    let o = Command::new(BIN).arg("check").arg(file).output().unwrap();
    let (out, err) = (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned());
    let mut set = BTreeSet::new();
    match o.status.code() {
        Some(0) => assert!(out.trim().ends_with(": ok"), "{}: {out}", file.display()),
        Some(1) => {
            for line in out.lines().chain(err.lines()) {
                // `path:line:col: error[CODE]: message`
                let Some(at) = line.find(": error[") else { continue };
                let (place, rest) = line.split_at(at);
                let mut p = place.rsplitn(3, ':');
                let (col, ln, path) = (p.next().unwrap(), p.next().unwrap(), p.next().unwrap());
                let rest = &rest[": error[".len()..];
                let close = rest.find("]: ").unwrap();
                let path = fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
                set.insert((path, ln.parse().unwrap(), col.parse().unwrap(), rest[..close].to_string(), rest[close + 3..].to_string()));
            }
            assert!(!set.is_empty(), "{}: exit 1 but no errors printed:\n{out}{err}", file.display());
        }
        _ => {
            let msg = err.trim().strip_prefix("staalc: ").unwrap_or(err.trim()).to_string();
            set.insert((fs::canonicalize(file).unwrap(), 1, 1, String::new(), msg));
        }
    }
    set
}

/// The errors in a batch of `publishDiagnostics`, positions turned back into lines
/// and character columns.
fn editor(published: &[Json]) -> BTreeSet<Diag> {
    let mut set = BTreeSet::new();
    for m in published.iter().filter(|m| m["method"] == "textDocument/publishDiagnostics") {
        let path = url::Url::parse(m["params"]["uri"].as_str().unwrap()).unwrap().to_file_path().unwrap();
        let text = fs::read_to_string(&path).unwrap_or_default();
        for d in m["params"]["diagnostics"].as_array().unwrap() {
            let line = d["range"]["start"]["line"].as_u64().unwrap() as usize;
            let utf16 = d["range"]["start"]["character"].as_u64().unwrap() as usize;
            // UTF-16 units back to characters.
            let mut units = 0;
            let mut col = 0;
            for c in text.lines().nth(line).unwrap_or("").chars() {
                if units >= utf16 {
                    break;
                }
                units += c.len_utf16();
                col += 1;
            }
            let path = fs::canonicalize(&path).unwrap_or(path.clone());
            let code = d["code"].as_str().unwrap_or("").to_string();
            set.insert((path, line + 1, col + 1, code, d["message"].as_str().unwrap().to_string()));
        }
    }
    set
}

fn programs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["tests/fixtures/pass", "tests/fixtures/fail", "tests/run", "tests/crash", "tests/staal", "tests/doc", "tests/debug", "tests/profile", "tests/db"] {
        for e in fs::read_dir(Path::new(ROOT).join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "staal") {
                out.push(p);
            }
        }
    }
    for e in fs::read_dir(Path::new(ROOT).join("tests/modules")).unwrap() {
        let main = e.unwrap().path().join("main.staal");
        if main.exists() {
            out.push(main);
        }
    }
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "staal") {
                out.push(p);
            }
        }
    }
    walk(&Path::new(ROOT).join("examples"), &mut out);
    walk(&Path::new(ROOT).join("packages"), &mut out);
    out.sort();
    out.into_iter().map(|p| fs::canonicalize(p).unwrap()).collect()
}

#[test]
fn the_editor_and_the_compiler_report_exactly_the_same_errors() {
    let files = programs();
    assert!(files.len() > 700, "found only {} programs", files.len());
    let mut lsp = Lsp::start();
    lsp.request("initialize", json!({ "capabilities": {} }));
    lsp.notify("initialized", json!({}));
    let (mut with_errors, mut differ) = (0, Vec::new());
    for f in &files {
        let text = fs::read_to_string(f).unwrap();
        lsp.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri(f), "languageId": "staal", "version": 1, "text": text } }));
        // A request after it: everything the open published comes before its answer.
        let (_, published) = lsp.request("textDocument/completion", json!({ "textDocument": { "uri": uri(f) }, "position": { "line": 0, "character": 0 } }));
        let (want, got) = (compiler(f), editor(&published));
        if want.iter().any(|d| !d.3.is_empty()) {
            with_errors += 1;
        }
        if want != got {
            differ.push(format!("{}:\n  compiler {want:?}\n  editor   {got:?}", f.display()));
        }
        lsp.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri(f) } }));
    }
    lsp.request("shutdown", Json::Null);
    lsp.notify("exit", Json::Null);
    assert_eq!(lsp.finish(), Some(0));
    assert!(differ.is_empty(), "{} of {} programs differ:\n{}", differ.len(), files.len(), differ.join("\n"));
    assert!(with_errors > 500, "only {with_errors} programs had errors to compare");
}

#[test]
fn an_edit_is_checked_again_and_closing_clears_its_errors() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("lsp-edit");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let f = dir.join("a.staal");
    fs::write(&f, "fn f() -> Int { return 1 }\n").unwrap();
    let u = uri(&f);
    let mut lsp = Lsp::start();
    lsp.request("initialize", json!({ "capabilities": {} }));
    let diags = |published: &[Json]| -> Vec<Json> {
        published.iter().filter(|m| m["params"]["uri"] == json!(u)).flat_map(|m| m["params"]["diagnostics"].as_array().unwrap().clone()).collect()
    };
    let sync = |lsp: &mut Lsp| lsp.request("textDocument/completion", json!({ "textDocument": { "uri": u }, "position": { "line": 0, "character": 0 } })).1;
    lsp.notify("textDocument/didOpen", json!({ "textDocument": { "uri": u, "languageId": "staal", "version": 1, "text": "fn f() -> Int { return 1 }\n" } }));
    assert!(diags(&sync(&mut lsp)).is_empty());
    // The unsaved text is what is checked: an error, with code, place and message.
    lsp.notify("textDocument/didChange", json!({ "textDocument": { "uri": u, "version": 2 }, "contentChanges": [{ "text": "fn f() -> Int {\n    return \"é😀\" + x\n}\n" }] }));
    let d = diags(&sync(&mut lsp));
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0]["code"], "E100");
    assert_eq!(d[0]["message"], "undefined name 'x'");
    // `x` is the 19th character of its line; "é" is one UTF-16 unit and "😀" two,
    // so it starts at unit 19 from 0.
    assert_eq!(d[0]["range"], json!({ "start": { "line": 1, "character": 19 }, "end": { "line": 1, "character": 20 } }));
    // Closing clears it.
    lsp.notify("textDocument/didClose", json!({ "textDocument": { "uri": u } }));
    let cleared = sync(&mut lsp);
    assert!(cleared.iter().any(|m| m["params"]["uri"] == json!(u) && m["params"]["diagnostics"] == json!([])), "{cleared:?}");
    lsp.request("shutdown", Json::Null);
    lsp.notify("exit", Json::Null);
    assert_eq!(lsp.finish(), Some(0));
}

#[test]
fn completion_and_formatting() {
    let mut lsp = Lsp::start();
    let (init, _) = lsp.request("initialize", json!({ "capabilities": {} }));
    let caps = &init["result"]["capabilities"];
    assert_eq!(caps["textDocumentSync"]["change"], 1);
    assert_eq!(caps["documentFormattingProvider"], true);
    let u = "untitled:Untitled-1";
    let text = "type Line{qty:Int}\nfn total(l:Line)->Int{\nlet n=l.qty\nreturn n}\n";
    lsp.notify("textDocument/didOpen", json!({ "textDocument": { "uri": u, "languageId": "staal", "version": 1, "text": text } }));
    let (c, published) = lsp.request("textDocument/completion", json!({ "textDocument": { "uri": u }, "position": { "line": 0, "character": 0 } }));
    // A document that is not a file is still checked, as a program on its own.
    assert!(published.iter().any(|m| m["params"]["uri"] == json!(u) && m["params"]["diagnostics"] == json!([])), "{published:?}");
    let labels: BTreeSet<String> = c["result"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect();
    for want in ["fn", "validate", "map", "random_int", "total", "Line", "n"] {
        assert!(labels.contains(want), "no completion {want}");
    }
    // Every reserved word, every built-in.
    let lexer = fs::read_to_string(Path::new(ROOT).join("src/lexer.rs")).unwrap();
    let keywords: BTreeSet<String> = lexer.lines().filter_map(|l| l.trim().strip_prefix('"')?.split_once("\" => Tok::").map(|(w, _)| w.to_string())).collect();
    let reserved: BTreeSet<String> = staalc::lsp::RESERVED.iter().map(|s| s.to_string()).collect();
    assert_eq!(reserved, keywords);
    for b in staalc::ast::Builtin::ALL {
        assert!(labels.contains(b.name()), "no completion for {}", b.name());
    }
    // Formatting gives the whole document as `staalc fmt` formats it.
    let (fmt, _) = lsp.request("textDocument/formatting", json!({ "textDocument": { "uri": u }, "options": { "tabSize": 4, "insertSpaces": true } }));
    let edits = fmt["result"].as_array().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["newText"].as_str().unwrap(), staalc::fmt::format(text).unwrap());
    assert_eq!(edits[0]["range"], json!({ "start": { "line": 0, "character": 0 }, "end": { "line": 4, "character": 0 } }));
    lsp.request("shutdown", Json::Null);
    lsp.notify("exit", Json::Null);
    assert_eq!(lsp.finish(), Some(0));
}

#[test]
fn the_protocol_is_kept_when_things_go_wrong() {
    let mut lsp = Lsp::start();
    // Before `initialize`, a request is refused.
    let (r, _) = lsp.request("textDocument/completion", json!({}));
    assert_eq!(r["error"]["code"], -32002);
    lsp.request("initialize", json!({ "capabilities": {} }));
    // A body that is not JSON is answered with a parse error, and the server goes on.
    lsp.send_raw("{not json");
    assert_eq!(lsp.read()["error"]["code"], -32700);
    // An unknown request is refused by name; an unknown notification is ignored.
    lsp.notify("$/unknown", json!({}));
    let (r, before) = lsp.request("workspace/unknown", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    assert!(before.is_empty());
    // Formatting text that does not lex fails, and changes nothing.
    lsp.notify("textDocument/didOpen", json!({ "textDocument": { "uri": "untitled:x", "languageId": "staal", "version": 1, "text": "fn f() { let t = \"\\q\" }" } }));
    let (r, _) = lsp.request("textDocument/formatting", json!({ "textDocument": { "uri": "untitled:x" }, "options": {} }));
    assert_eq!(r["error"]["code"], -32803);
    // `exit` without `shutdown` is exit code 1.
    lsp.notify("exit", Json::Null);
    assert_eq!(lsp.finish(), Some(1));
}

#[test]
fn errors_in_modules_and_in_the_manifest_are_the_compilers_too() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("lsp-modules");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let dir = fs::canonicalize(&dir).unwrap();
    let (main, lib) = (dir.join("main.staal"), dir.join("lib.staal"));
    let uses = "import lib\nfn f() -> Int { return lib.g() }\n";
    fs::write(&main, uses).unwrap();
    // The module's error is after a character of two UTF-16 units.
    fs::write(&lib, "pub fn g() -> Int {\n    let t = \"😀\" + y\n    return 1\n}\n").unwrap();
    let mut lsp = Lsp::start();
    lsp.request("initialize", json!({ "capabilities": {} }));
    let sync = |lsp: &mut Lsp| lsp.request("textDocument/completion", json!({ "textDocument": { "uri": uri(&main) }, "position": { "line": 0, "character": 0 } })).1;
    lsp.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri(&main), "languageId": "staal", "version": 1, "text": uses } }));
    let published = sync(&mut lsp);
    let want = compiler(&main);
    assert!(want.iter().any(|d| d.0 == lib), "{want:?}");
    assert_eq!(editor(&published), want);
    // When the document no longer imports the module, the module's errors are cleared.
    lsp.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri(&main), "version": 2 }, "contentChanges": [{ "text": "fn f() -> Int { return 1 }\n" }] }));
    let published = sync(&mut lsp);
    assert!(published.iter().any(|m| m["params"]["uri"] == json!(uri(&lib)) && m["params"]["diagnostics"] == json!([])), "{published:?}");
    assert!(editor(&published).is_empty());
    // A manifest that cannot be read is not a diagnostic of the program; it is shown
    // at the start of the document, exactly as `staalc check` reports it.
    fs::write(dir.join("staal.json"), "{").unwrap();
    lsp.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri(&main), "version": 3 }, "contentChanges": [{ "text": uses }] }));
    let published = sync(&mut lsp);
    let want = compiler(&main);
    assert!(want.iter().all(|d| d.3.is_empty() && (d.1, d.2) == (1, 1)), "{want:?}");
    assert_eq!(editor(&published), want);
    // A document that is not saved is checked too, and cannot import.
    let u = "untitled:Untitled-2";
    lsp.notify("textDocument/didOpen", json!({ "textDocument": { "uri": u, "languageId": "staal", "version": 1, "text": "fn f() -> Int { return z }\n" } }));
    let published = sync(&mut lsp);
    let d: Vec<&Json> = published.iter().filter(|m| m["params"]["uri"] == json!(u)).flat_map(|m| m["params"]["diagnostics"].as_array().unwrap()).collect();
    assert_eq!(d.len(), 1, "{published:?}");
    assert_eq!((d[0]["code"].as_str(), d[0]["message"].as_str()), (Some("E100"), Some("undefined name 'z'")));
    lsp.request("shutdown", Json::Null);
    lsp.notify("exit", Json::Null);
    assert_eq!(lsp.finish(), Some(0));
}
