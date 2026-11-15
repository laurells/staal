// The developer documentation (docs/get-started, docs/guides and docs/reference), kept
// true to the implementation. Each page is followed in an empty folder of its own, in
// order: a block whose info string holds `file=NAME` is saved there as NAME; each
// command of a `console` block (a line starting `$ `, optionally after NAME=VALUE
// environment settings, then `staalc` and its arguments) is run there and must print
// exactly the lines that follow it (standard output, then standard error); every other
// `staal` block must be a program `staalc check` accepts; every other `json` block must
// parse. Every relative link must name a file that exists.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const SECTIONS: [&str; 3] = ["docs/get-started", "docs/guides", "docs/reference"];

fn pages() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for s in SECTIONS {
        let Ok(rd) = std::fs::read_dir(Path::new(ROOT).join(s)) else { continue };
        for e in rd {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn rel(p: &Path) -> String {
    p.strip_prefix(ROOT).unwrap().display().to_string()
}

/// The fenced blocks of a page: (info string, lines).
fn blocks(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(info) = line.strip_prefix("```") else { continue };
        let mut block = Vec::new();
        for l in lines.by_ref() {
            if l == "```" {
                break;
            }
            block.push(l.to_string());
        }
        out.push((info.trim().to_string(), block));
    }
    out
}

fn check_program(dir: &Path, n: usize, src: &str) -> Result<(), String> {
    let f = dir.join(format!("block{n}.staal"));
    std::fs::write(&f, src).unwrap();
    let o = Command::new(BIN).arg("check").arg(&f).current_dir(dir).output().unwrap();
    if o.status.success() {
        Ok(())
    } else {
        Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }
}

fn follow(page: &Path) -> Vec<String> {
    let name = rel(page);
    let text = std::fs::read_to_string(page).unwrap();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("docs").join(name.replace('/', "_"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut errors = Vec::new();
    for (n, (info, block)) in blocks(&text).into_iter().enumerate() {
        let words: Vec<&str> = info.split_whitespace().collect();
        let lang = words.first().copied().unwrap_or("");
        if let Some(file) = words.iter().find_map(|w| w.strip_prefix("file=")) {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, block.join("\n") + "\n").unwrap();
        } else if lang == "console" {
            let mut i = 0;
            while i < block.len() {
                let Some(cmd) = block[i].strip_prefix("$ ") else {
                    errors.push(format!("{name}: a console block line that is not a command: {}", block[i]));
                    break;
                };
                let mut want = Vec::new();
                i += 1;
                while i < block.len() && !block[i].starts_with("$ ") {
                    want.push(block[i].as_str());
                    i += 1;
                }
                let mut parts = cmd.split_whitespace().peekable();
                let mut env = Vec::new();
                while let Some(w) = parts.peek() {
                    match w.split_once('=') {
                        Some((k, v)) if !k.is_empty() && k.chars().all(|c| c.is_ascii_uppercase() || c == '_') => {
                            env.push((k.to_string(), v.to_string()));
                            parts.next();
                        }
                        _ => break,
                    }
                }
                let args: Vec<&str> = parts.collect();
                if args.first() != Some(&"staalc") {
                    errors.push(format!("{name}: a console command that is not staalc: {cmd}"));
                    continue;
                }
                let o = Command::new(BIN).args(&args[1..]).envs(env).current_dir(&dir).output().unwrap();
                let got = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                if got.trim_end() != want.join("\n") {
                    errors.push(format!("{name}: `{cmd}` printed:\n{}\nthe page shows:\n{}", got.trim_end(), want.join("\n")));
                }
            }
        } else if lang == "staal" {
            if let Err(e) = check_program(&dir, n, &(block.join("\n") + "\n")) {
                errors.push(format!("{name}: block {n} does not check:\n{e}"));
            }
        } else if lang == "json"
            && let Err(e) = serde_json::from_str::<serde_json::Value>(&block.join("\n"))
        {
            errors.push(format!("{name}: block {n} is not JSON: {e}"));
        }
    }
    errors
}

/// Relative links of a page that name no file.
fn broken_links(page: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(page).unwrap();
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        let Some(end) = rest.find(')') else { break };
        let target = &rest[..end];
        rest = &rest[end..];
        if target.contains("://") || target.starts_with('#') || target.starts_with("mailto:") {
            continue;
        }
        let file = target.split('#').next().unwrap();
        if !page.parent().unwrap().join(file).exists() {
            out.push(format!("{}: a link to {target}, which does not exist", rel(page)));
        }
    }
    out
}

#[test]
fn every_page_works_as_written() {
    let pages = pages();
    assert!(!pages.is_empty(), "no pages");
    let errors: Vec<String> = pages.iter().flat_map(|p| follow(p)).collect();
    assert!(errors.is_empty(), "{} problem(s):\n\n{}", errors.len(), errors.join("\n\n"));
}

#[test]
fn every_relative_link_names_a_file() {
    let errors: Vec<String> = pages().iter().flat_map(|p| broken_links(p)).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
