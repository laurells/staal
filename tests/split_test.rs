// The phase 1 gate: every rule must hold across module boundaries.
//
// Each test program (every pass and fail fixture, and every runtime program) is split
// mechanically into two files: a module `split_lib` holding every type and every
// function that some function calls (with everything those call, so the module never
// needs the program), all marked `pub`, and the program, which imports them all by
// name. Calls, types, records, budgets, blinding promises and evidence then cross a
// module boundary wherever they did inside one file. Results must not change:
//   - a fixture that compiled still compiles; a rejected one gets the same codes;
//   - every runtime case gives the same result, actions, model calls and evidence.
// Fixtures that do not parse (lexer and syntax errors) cannot be split; the test
// checks that those are exactly the ones expecting E900 or E901.

mod common;

use staalc::ast::{Expr, FnDecl, Program, Stmt};
use staalc::SourceFile;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const LIB: &str = "split_lib";

struct Split {
    main: String,
    lib: String,
    /// Functions moved into the module.
    moved: HashSet<String>,
}

fn calls_in(f: &FnDecl) -> Vec<String> {
    fn expr(e: &Expr, out: &mut Vec<String>) {
        if let Expr::Call { name, .. } = e {
            out.push(name.clone());
        }
        for c in e.children() {
            expr(c, out);
        }
    }
    fn block(body: &[Stmt], out: &mut Vec<String>) {
        for s in body {
            for e in s.exprs() {
                expr(e, out);
            }
            for b in s.blocks() {
                block(b, out);
            }
        }
    }
    let mut out = Vec::new();
    block(&f.body, &mut out);
    out
}

/// Splits `src`, or returns None when it does not parse or has nothing to move.
fn split(src: &str) -> Option<Split> {
    let toks = staalc::lexer::lex(src).ok()?;
    let prog: Program = staalc::parser::Parser::new(toks).program().ok()?;
    assert!(prog.imports.is_empty(), "test programs are single files");
    // Every function called by some function, and everything those call.
    let mut moved: HashSet<String> = prog.fns.iter().flat_map(calls_in).collect();
    loop {
        let more: Vec<String> =
            prog.fns.iter().filter(|f| moved.contains(&f.name)).flat_map(calls_in).filter(|c| !moved.contains(c)).collect();
        if more.is_empty() {
            break;
        }
        moved.extend(more);
    }
    // A capability's action and canonical functions (named with a space) are moved with
    // the capability, never on their own.
    moved.retain(|n| !n.contains(' ') && prog.fns.iter().any(|f| &f.name == n));
    if moved.is_empty() && prog.types.is_empty() && prog.caps.is_empty() {
        return None;
    }
    // Declarations by starting line; each runs to the line before the next one.
    // Capabilities always move to the module (declared there, used by the program), and
    // take no `pub`: a capability is named by its path, not imported.
    let mut decls: Vec<(usize, usize, bool, String)> = prog
        .types
        .iter()
        .map(|t| (t.pos.line, t.pos.col, true, t.name.clone()))
        .chain(prog.fns.iter().filter(|f| !f.name.contains(' ')).map(|f| (f.pos.line, f.pos.col, moved.contains(&f.name), f.name.clone())))
        .chain(prog.caps.iter().map(|c| (c.pos.line, c.pos.col, true, format!("capability {}", c.path.join(".")))))
        .collect();
    decls.sort();
    let lines: Vec<&str> = src.lines().collect();
    if decls.windows(2).any(|w| w[0].0 == w[1].0) {
        return None; // two declarations on one line: not split by lines
    }
    let first = decls[0].0 - 1;
    let mut main = lines[..first].join("\n");
    let names: BTreeSet<String> = decls.iter().filter(|d| d.2 && !d.3.contains(' ')).map(|d| d.3.clone()).collect();
    if names.is_empty() {
        main.push_str(&format!("\nimport {LIB}\n"));
    } else {
        main.push_str(&format!("\nimport {LIB} {{ {} }}\n", names.into_iter().collect::<Vec<_>>().join(", ")));
    }
    let mut lib = String::new();
    for (i, (line, col, to_lib, _)) in decls.iter().enumerate() {
        let end = decls.get(i + 1).map_or(lines.len(), |d| d.0 - 1);
        let mut seg: Vec<String> = lines[line - 1..end].iter().map(|s| s.to_string()).collect();
        if *to_lib {
            if !decls[i].3.starts_with("capability ") {
                let (a, b) = seg[0].split_at(col - 1);
                seg[0] = format!("{a}pub {b}");
            }
            lib.push_str(&seg.join("\n"));
            lib.push('\n');
        } else {
            main.push_str(&seg.join("\n"));
            main.push('\n');
        }
    }
    Some(Split { main, lib, moved })
}

fn check_split(s: &Split) -> staalc::Checked {
    let lib = s.lib.clone();
    staalc::check_files(SourceFile { name: "main.staal".into(), text: s.main.clone() }, move |m| {
        if m == [LIB.to_string()] {
            Ok(SourceFile { name: format!("{LIB}.staal"), text: lib.clone() })
        } else {
            Err("not part of the split".into())
        }
    })
}

fn sorted_codes(r: &Result<Program, Vec<staalc::checker::Diagnostic>>) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = match r {
        Ok(_) => Vec::new(),
        Err(d) => d.iter().map(|d| d.code).collect(),
    };
    v.sort();
    v
}

fn files_in(dir: &str, ext: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut v: Vec<PathBuf> =
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == ext)).collect();
    v.sort();
    v
}

#[test]
fn fixtures_keep_their_results_across_modules() {
    let mut failures = Vec::new();
    let (mut split_n, mut crossing) = (0, 0);
    for f in files_in("tests/fixtures/pass", "staal").into_iter().chain(files_in("tests/fixtures/fail", "staal")) {
        let src = fs::read_to_string(&f).unwrap();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        // The fixture's written expectation, not a recomputed one: a compiler bug that
        // affected one file and two files alike must still be caught.
        let mut want: Vec<&str> = match src.lines().next().and_then(|l| l.strip_prefix("// expect:")) {
            Some(codes) => codes.split_whitespace().collect(),
            None => Vec::new(),
        };
        want.sort();
        let Some(s) = split(&src) else {
            let unparsable = staalc::lexer::lex(&src)
                .ok()
                .and_then(|t| staalc::parser::Parser::new(t).program().ok())
                .is_none();
            if unparsable && !want.iter().all(|c| *c == "E900" || *c == "E901") {
                failures.push(format!("{name}: does not parse but expects {want:?}"));
            }
            continue;
        };
        split_n += 1;
        if !s.moved.is_empty() {
            crossing += 1;
        }
        let got = sorted_codes(&check_split(&s).result);
        if got != want {
            failures.push(format!(
                "{name}: one file gives {want:?}, split gives {got:?}\n--- main.staal\n{}\n--- {LIB}.staal\n{}",
                s.main, s.lib
            ));
        }
    }
    assert!(failures.is_empty(), "{} fixtures changed when split:\n{}", failures.len(), failures.join("\n"));
    assert!(split_n >= 120 && crossing >= 55, "too few fixtures split ({split_n}, {crossing} with calls across)");
    println!("{split_n} fixtures split, {crossing} with calls crossing the module boundary");
}

#[test]
fn runtime_cases_behave_the_same_across_modules() {
    let mut failures = Vec::new();
    let mut ran = 0;
    for prog_path in files_in("tests/run", "staal") {
        let src = fs::read_to_string(&prog_path).unwrap();
        let base = prog_path.file_stem().unwrap().to_string_lossy().to_string();
        let Some(s) = split(&src) else { continue };
        let prog = match check_split(&s).result {
            Ok(p) => p,
            Err(d) => {
                failures.push(format!("{base}.staal: split program has errors: {d:?}"));
                continue;
            }
        };
        for case in files_in("tests/run", "json") {
            let stem = case.file_stem().unwrap().to_string_lossy().to_string();
            if stem.split("__").next() != Some(base.as_str()) {
                continue;
            }
            // The entry function may have moved into the module.
            let cfg: serde_json::Value = serde_json::from_str(&fs::read_to_string(&case).unwrap()).unwrap();
            let entry = cfg["entry"].as_str().unwrap_or_default();
            let prefix = if s.moved.contains(entry) { format!("{LIB}.") } else { String::new() };
            ran += 1;
            if let Err(e) = common::check_case(&prog, &case, &prefix) {
                failures.push(format!("{stem}: {e}"));
            }
        }
    }
    assert!(failures.is_empty(), "{} runtime cases changed when split:\n{}", failures.len(), failures.join("\n"));
    assert!(ran > 100, "only {ran} runtime cases ran split");
    println!("{ran} runtime cases ran split");
}
