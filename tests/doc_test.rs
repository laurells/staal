// `staalc doc` (src/doc.rs): a golden page for a program with a module, every
// program in the repository documented, and its refusals.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

#[test]
fn a_program_and_its_module_are_documented_exactly() {
    // tests/doc/main.staal has a file doc, a const, an alias and functions, a remark
    // after code and a block cut off by a blank line (neither is documentation); its
    // module has a private function (not shown), pub types and a capability.
    let dir = Path::new(ROOT).join("tests/doc");
    let o = Command::new(BIN).args(["doc", "main.staal"]).current_dir(&dir).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let want = fs::read_to_string(dir.join("expected.md")).unwrap();
    assert_eq!(String::from_utf8(o.stdout).unwrap(), want);
    for absent in ["private_helper", "a remark", "This block is separated", "The service's address", "return"] {
        assert!(!want.contains(absent), "{absent} is shown");
    }
    // -o writes the same page.
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("doc-out.md");
    let _ = fs::remove_file(&out);
    let o = Command::new(BIN).args(["doc", "main.staal", "-o"]).arg(&out).current_dir(&dir).output().unwrap();
    assert_eq!((o.status.code(), o.stdout.is_empty()), (Some(0), true));
    assert_eq!(fs::read_to_string(&out).unwrap(), want);
}

fn staal_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            staal_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "staal") {
            out.push(p);
        }
    }
}

#[test]
fn every_program_that_checks_is_documented_and_every_declaration_is_shown_as_formatted() {
    let mut files = Vec::new();
    for d in ["examples", "packages", "tests/staal", "tests/run", "tests/fixtures/pass", "tests/doc"] {
        staal_files(&Path::new(ROOT).join(d), &mut files);
    }
    let mut documented = 0;
    for f in &files {
        let src = fs::read_to_string(f).unwrap();
        let checked = staalc::check_path(f).unwrap();
        if checked.result.is_err() {
            continue;
        }
        let page = staalc::doc::document(&checked.files).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        documented += 1;
        // Every declaration of the entry file is shown, under its name.
        let d = staalc::doc::file_doc(&src).unwrap();
        for it in &d.items {
            assert!(page.contains(&format!("### `{}`\n\n```staal\n{}\n```\n", it.name, it.code)), "{}: {} missing", f.display(), it.name);
            // What is shown is laid out as `staalc fmt` lays it out, and is the
            // declaration's own text.
            assert_eq!(staalc::fmt::format(&it.code).unwrap().trim_end(), it.code, "{}: {}", f.display(), it.name);
            let first = it.code.lines().next().unwrap();
            assert!(src.contains(first.trim()) || it.kind == "capability", "{}: {first}", f.display());
        }
        // Each `pub` declaration of each module is shown.
        for m in &checked.files[1..] {
            for it in staalc::doc::file_doc(&m.text).unwrap().items.iter().filter(|it| it.public) {
                assert!(page.contains(&format!("### `{}`", it.name)), "{}: module {} {} missing", f.display(), m.name, it.name);
            }
        }
    }
    assert!(documented >= 100, "only {documented} programs documented");
}

#[test]
fn a_program_with_check_errors_is_not_documented() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("doc-refused");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("bad.staal"), "fn f() -> Int { return y }\n").unwrap();
    let o = Command::new(BIN).args(["doc", "bad.staal", "-o", "out.md"]).current_dir(&dir).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("bad.staal:1:24: error[E100]: undefined name 'y'"));
    assert!(!dir.join("out.md").exists());
    let o = Command::new(BIN).args(["doc", "missing.staal"]).current_dir(&dir).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    fs::write(dir.join("ok.staal"), "fn f() -> Int {\n    return 1\n}\n").unwrap();
    let o = Command::new(BIN).args(["doc", "ok.staal", "-o", "no/such/dir/out.md"]).current_dir(&dir).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = Command::new(BIN).args(["doc"]).current_dir(&dir).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}
