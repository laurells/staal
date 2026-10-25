// The language server (`staalc lsp`): the Language Server Protocol over standard
// input and output, for editors.
//
// It offers three things:
//   - diagnostics: each time a document is opened or changed, its text is checked by
//     `check_text_at`, the very function `staalc check` uses, so the editor and the
//     compiler report exactly the same errors, at the same places, in the same words
//     (tests/lsp_test.rs compares them on every test program);
//   - completion: the reserved words, the built-in functions and the names the
//     document declares;
//   - formatting: `staalc fmt`'s own formatter.
// Documents are synchronised whole (each change sends the full text). Imported
// modules are read from disk, as `staalc check` reads them.

use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Serves one editor session until `exit`. Returns the process exit code: 0 when
/// `exit` followed `shutdown`, 1 otherwise (as the protocol says).
pub fn serve(mut input: impl BufRead, mut output: impl Write) -> u8 {
    let mut s = Server::default();
    loop {
        let msg = match read_message(&mut input) {
            Ok(Some(m)) => m,
            // The editor closed the stream without `exit`.
            Ok(None) => return 1,
            Err(e) => {
                // Not JSON, or a broken header: answer as the protocol says and go on.
                if send(&mut output, &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e } })).is_err() {
                    return 1;
                }
                continue;
            }
        };
        let out = s.handle(&msg);
        for m in &out {
            if send(&mut output, m).is_err() {
                return 1;
            }
        }
        if let Some(code) = s.exit {
            return code;
        }
    }
}

#[derive(Default)]
struct Server {
    initialized: bool,
    shut_down: bool,
    exit: Option<u8>,
    /// The text of each open document, by URI.
    docs: BTreeMap<String, String>,
    /// For each open document: the URIs its last check published diagnostics to.
    published: BTreeMap<String, BTreeSet<String>>,
}

impl Server {
    /// The messages to send in answer to `msg`.
    fn handle(&mut self, msg: &Json) -> Vec<Json> {
        let method = msg["method"].as_str().unwrap_or_default();
        let id = msg.get("id").cloned();
        let params = &msg["params"];
        let reply = |result: Json| json!({ "jsonrpc": "2.0", "id": id.clone(), "result": result });
        let error = |code: i64, message: &str| json!({ "jsonrpc": "2.0", "id": id.clone(), "error": { "code": code, "message": message } });
        if method == "exit" {
            self.exit = Some(if self.shut_down { 0 } else { 1 });
            return Vec::new();
        }
        if !self.initialized && method != "initialize" {
            // Before `initialize` a request is refused and a notification dropped.
            return if id.is_some() { vec![error(-32002, "the server is not initialized")] } else { Vec::new() };
        }
        if self.shut_down {
            return if id.is_some() { vec![error(-32600, "the server is shut down")] } else { Vec::new() };
        }
        match method {
            "initialize" => {
                self.initialized = true;
                vec![reply(json!({
                    "capabilities": {
                        "textDocumentSync": { "openClose": true, "change": 1 },
                        "completionProvider": {},
                        "documentFormattingProvider": true
                    },
                    "serverInfo": { "name": "staalc", "version": env!("CARGO_PKG_VERSION") }
                }))]
            }
            "shutdown" => {
                self.shut_down = true;
                vec![reply(Json::Null)]
            }
            "textDocument/didOpen" => {
                let (uri, text) = (params["textDocument"]["uri"].as_str(), params["textDocument"]["text"].as_str());
                match (uri, text) {
                    (Some(u), Some(t)) => {
                        self.docs.insert(u.to_string(), t.to_string());
                        self.check(u)
                    }
                    _ => Vec::new(),
                }
            }
            "textDocument/didChange" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default().to_string();
                // Whole-document sync: the last change holds the full text.
                let text = params["contentChanges"].as_array().and_then(|c| c.last()).and_then(|c| c["text"].as_str());
                match text {
                    Some(t) if self.docs.contains_key(&uri) => {
                        self.docs.insert(uri.clone(), t.to_string());
                        self.check(&uri)
                    }
                    _ => Vec::new(),
                }
            }
            "textDocument/didClose" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default().to_string();
                self.docs.remove(&uri);
                // Clear what this document's checks published.
                self.published.remove(&uri).unwrap_or_default().into_iter().map(|u| publish(&u, Vec::new())).collect()
            }
            "textDocument/completion" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                vec![reply(Json::Array(completions(self.docs.get(uri).map(String::as_str).unwrap_or(""))))]
            }
            "textDocument/formatting" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
                let Some(text) = self.docs.get(uri) else { return vec![error(-32602, "the document is not open")] };
                match crate::fmt::format(text) {
                    Ok(out) if out == *text => vec![reply(json!([]))],
                    Ok(out) => vec![reply(json!([{ "range": { "start": { "line": 0, "character": 0 }, "end": end_of(text) }, "newText": out }]))],
                    // -32803: the request failed.
                    Err(e) => vec![error(-32803, &e)],
                }
            }
            _ if id.is_some() => vec![error(-32601, &format!("{method} is not supported"))],
            // Other notifications (`initialized`, `$/...`, `didSave`) need nothing.
            _ => Vec::new(),
        }
    }

    /// Checks open document `uri` and publishes its diagnostics, and clears those an
    /// earlier check published to files that now have none.
    fn check(&mut self, uri: &str) -> Vec<Json> {
        let text = self.docs[uri].clone();
        let mut by_uri: BTreeMap<String, Vec<Json>> = BTreeMap::new();
        by_uri.insert(uri.to_string(), Vec::new());
        match uri_path(uri) {
            Some(path) => match crate::check_text_at(&path, text.clone()) {
                Ok(checked) => {
                    for d in checked.result.err().unwrap_or_default() {
                        let i = d.pos.file as usize;
                        // File 0 is this document; the others are modules read from disk.
                        let (target, src) = if i == 0 {
                            (uri.to_string(), text.as_str())
                        } else {
                            match checked.files.get(i) {
                                Some(f) => (file_uri(&f.name).unwrap_or_else(|| uri.to_string()), f.text.as_str()),
                                None => (uri.to_string(), text.as_str()),
                            }
                        };
                        by_uri.entry(target).or_default().push(diagnostic(src, d.pos.line, d.pos.col, Some(d.code), &d.msg));
                    }
                }
                // Not a diagnostic of the program (an unreadable manifest, say): shown at
                // the start of the document.
                Err(e) => by_uri.entry(uri.to_string()).or_default().push(diagnostic(&text, 1, 1, None, &e)),
            },
            // A document that is not a file (`untitled:`) has no modules beside it.
            None => {
                let checked = crate::check_files(crate::SourceFile { name: uri.to_string(), text: text.clone() }, |m| {
                    Err(format!("module '{}' can only be found when the program is a saved file", m.join(".")))
                });
                for d in checked.result.err().unwrap_or_default() {
                    by_uri.entry(uri.to_string()).or_default().push(diagnostic(&text, d.pos.line, d.pos.col, Some(d.code), &d.msg));
                }
            }
        }
        let now: BTreeSet<String> = by_uri.keys().cloned().collect();
        let before = self.published.insert(uri.to_string(), now.clone()).unwrap_or_default();
        let mut out: Vec<Json> = by_uri.into_iter().map(|(u, ds)| publish(&u, ds)).collect();
        out.extend(before.difference(&now).map(|u| publish(u, Vec::new())));
        out
    }
}

fn publish(uri: &str, diagnostics: Vec<Json>) -> Json {
    json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": { "uri": uri, "diagnostics": diagnostics } })
}

/// A diagnostic at 1-based `line` and `col` (in characters) of `src`, as the
/// protocol has it: 0-based, columns in UTF-16 code units, covering the word there.
fn diagnostic(src: &str, line: usize, col: usize, code: Option<&str>, message: &str) -> Json {
    let text = src.lines().nth(line.saturating_sub(1)).unwrap_or("");
    let chars: Vec<char> = text.chars().collect();
    let start = col.saturating_sub(1).min(chars.len());
    let word = chars[start..].iter().take_while(|c| c.is_alphanumeric() || **c == '_').count().max(1);
    let end = (start + word).min(chars.len()).max(start);
    let utf16 = |n: usize| chars[..n].iter().map(|c| c.len_utf16()).sum::<usize>();
    let at = |n: usize| json!({ "line": line.saturating_sub(1), "character": utf16(n) });
    let mut d = json!({ "range": { "start": at(start), "end": at(end) }, "severity": 1, "source": "staalc", "message": message });
    if let Some(c) = code {
        d["code"] = json!(c);
    }
    d
}

/// The position just after the last character of `text`.
fn end_of(text: &str) -> Json {
    let line = text.matches('\n').count();
    let last = text.rsplit('\n').next().unwrap_or("");
    json!({ "line": line, "character": last.chars().map(char::len_utf16).sum::<usize>() })
}

/// The path of a `file:` URI.
fn uri_path(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok().filter(|u| u.scheme() == "file").and_then(|u| u.to_file_path().ok())
}

/// The `file:` URI of a file the checker read, named as it names it.
fn file_uri(name: &str) -> Option<String> {
    let p = Path::new(name);
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().ok()?.join(p) };
    url::Url::from_file_path(abs).ok().map(String::from)
}

/// Completion items: reserved words, built-in functions, and the names `text`
/// declares (functions, types, enums, interfaces, constants, variables).
fn completions(text: &str) -> Vec<Json> {
    use crate::lexer::Tok;
    let mut items = Vec::new();
    // Kinds, as the protocol numbers them: 14 keyword, 3 function, 6 variable, 7 class.
    for w in RESERVED {
        items.push(json!({ "label": w, "kind": 14 }));
    }
    for b in crate::ast::Builtin::ALL {
        items.push(json!({ "label": b.name(), "kind": 3, "detail": "built-in" }));
    }
    let mut declared = BTreeSet::new();
    if let Ok(toks) = crate::lexer::lex(text) {
        for w in toks.windows(2) {
            let kind = match (&w[0].tok, &w[1].tok) {
                (Tok::Fn, Tok::Ident(n)) => Some((n, 3)),
                (Tok::Type, Tok::Ident(n)) => Some((n, 7)),
                (Tok::Let | Tok::For | Tok::Set, Tok::Ident(n)) => Some((n, 6)),
                (Tok::Ident(k), Tok::Ident(n)) if k == "enum" || k == "interface" => Some((n, 7)),
                (Tok::Ident(k), Tok::Ident(n)) if k == "const" => Some((n, 6)),
                _ => None,
            };
            if let Some((n, k)) = kind {
                declared.insert((n.clone(), k));
            }
        }
    }
    for (n, k) in declared {
        items.push(json!({ "label": n, "kind": k }));
    }
    items
}

/// The reserved words (docs/GRAMMAR.md; tests/lsp_test.rs checks they are the lexer's).
pub const RESERVED: [&str; 26] = [
    "and", "break", "budget", "cap", "continue", "done", "else", "false", "fn", "for", "if", "import", "in", "let", "loop", "not", "or",
    "propose", "pub", "repeat", "return", "set", "true", "try", "type", "validate",
];

/// One message: `Content-Length` headers, a blank line, then that many bytes of JSON.
/// None at the end of the input.
fn read_message(input: &mut impl BufRead) -> Result<Option<Json>, String> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            length = Some(v.trim().parse::<usize>().map_err(|_| format!("bad Content-Length: {v}"))?);
        }
    }
    let n = length.ok_or("a message without Content-Length")?;
    let mut body = vec![0u8; n];
    std::io::Read::read_exact(input, &mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map(Some).map_err(|e| format!("not JSON: {e}"))
}

fn send(output: &mut impl Write, msg: &Json) -> std::io::Result<()> {
    let body = msg.to_string();
    write!(output, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    output.flush()
}
