// The formatter (`staalc fmt`, src/fmt.rs). It may change only whitespace, so the
// tests prove exactly that on every Staal file in the repository: the formatted file
// has the same syntax tree (positions aside), the same comments and the same line
// breaks between tokens, and formatting it again changes nothing. A fixed input shows
// the layout itself, and the command line's exit codes are checked.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn staal_files() -> Vec<PathBuf> {
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
    let mut out = Vec::new();
    for d in ["tests", "examples", "packages", "bench"] {
        walk(&Path::new(ROOT).join(d), &mut out);
    }
    out.sort();
    out
}

/// The syntax tree with every position taken out, as text.
fn tree(src: &str) -> Option<String> {
    let toks = staalc::lexer::lex(src).ok()?;
    let prog = staalc::parser::Parser::new(toks).program();
    let shown = format!("{prog:?}");
    // `Pos { file: 0, line: 3, col: 5 }` -> `Pos`
    let mut out = String::with_capacity(shown.len());
    let mut rest = shown.as_str();
    while let Some(i) = rest.find("Pos { file: ") {
        out.push_str(&rest[..i + 3]);
        let end = rest[i..].find('}').unwrap();
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

#[test]
fn formatting_any_file_changes_only_whitespace_and_is_stable() {
    let files = staal_files();
    assert!(files.len() > 500, "found only {} files", files.len());
    let (mut formatted, mut refused) = (0, 0);
    for f in &files {
        let src = fs::read_to_string(f).unwrap();
        let name = f.display();
        match staalc::fmt::format(&src) {
            Ok(out) => {
                formatted += 1;
                // The same program, whatever it means: the parser gives the same tree
                // (or the same syntax error).
                assert_eq!(tree(&out), tree(&src), "{name}: the syntax tree changed");
                let again = staalc::fmt::format(&out).unwrap_or_else(|e| panic!("{name}: the formatted file does not format: {e}"));
                assert_eq!(again, out, "{name}: formatting is not stable");
                assert!(out.ends_with('\n') && !out.ends_with("\n\n"), "{name}: does not end with one line break");
                assert!(!out.contains("\n\n\n"), "{name}: two blank lines in a row");
            }
            Err(_) => {
                // Only a file that does not lex is refused, and it is left as it is.
                refused += 1;
                assert!(staalc::lexer::lex(&src).is_err(), "{name}: lexes but the formatter refused it");
            }
        }
    }
    assert!(formatted > 500 && refused <= 5, "formatted {formatted}, refused {refused}");
}

#[test]
fn the_layout_is_this() {
    let messy = "// A messy file.\ntype Line{sku:Text,qty:Int ,price:Money}\n\n\n\nfn total( lines:List<Line,100>,fee:Money=0.00 NGN )->Money budget( calls:0 ){\nlet sum=fee   // start   \n  for l in lines{\n\n        set sum=sum+l.price*l.qty\n\n  }\n    if not(sum==fee) and sum>fee{\nreturn sum}\nlet x = f(a,\nb,\n   -c)\nlet long = sum +\nfee\n    return sum - - fee\n}\nfn first<T:Priced+Named>(xs:List<T,10>)->Optional<T>{\n    if a.b<c and x>y { return none }\n    return fn(x:Int)->Int{return x}(3)\n}\nfn handle(a: Text,\n          b: cap x.y(uses: 1)) -> Text {\n    return \"\\(a) has  two  spaces\"\n}\nfn Line.cost<U>(self)->Int{return 1}\n\n\n";
    let want = "// A messy file.\ntype Line { sku: Text, qty: Int, price: Money }\n\nfn total(lines: List<Line, 100>, fee: Money = 0.00 NGN) -> Money budget(calls: 0) {\n    let sum = fee // start\n    for l in lines {\n        set sum = sum + l.price * l.qty\n    }\n    if not (sum == fee) and sum > fee {\n        return sum }\n    let x = f(a,\n        b,\n        -c)\n    let long = sum +\n        fee\n    return sum - -fee\n}\nfn first<T: Priced + Named>(xs: List<T, 10>) -> Optional<T> {\n    if a.b < c and x > y { return none }\n    return fn(x: Int) -> Int { return x }(3)\n}\nfn handle(a: Text,\n    b: cap x.y(uses: 1)) -> Text {\n    return \"\\(a) has  two  spaces\"\n}\nfn Line.cost<U>(self) -> Int { return 1 }\n";
    assert_eq!(staalc::fmt::format(messy).unwrap(), want);
}

#[test]
fn multi_line_text_is_kept_exactly() {
    let src = "fn f() -> Text {\n        let t = \"\"\"\n            first   \n              indented\n            \"\"\" + \"x\"\n        return t\n}\n";
    let out = staalc::fmt::format(src).unwrap();
    // The text's own lines, trailing spaces included, are untouched.
    assert!(out.contains("\"\"\"\n            first   \n              indented\n            \"\"\" + \"x\"\n"), "{out}");
    assert!(out.starts_with("fn f() -> Text {\n    let t = \"\"\""), "{out}");
}

fn staalc(args: &[&str]) -> (Option<i32>, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_staalc")).args(args).output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stdout).into_owned())
}

#[test]
fn the_command_formats_in_place_checks_and_refuses_what_it_cannot_lex() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fmt");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (messy, tidy, broken) = (dir.join("messy.staal"), dir.join("tidy.staal"), dir.join("broken.staal"));
    fs::write(&messy, "fn f()->Int{return 1}\n").unwrap();
    fs::write(&tidy, "fn f() -> Int { return 1 }\n").unwrap();
    fs::write(&broken, "fn f() { let t = \"\\q\" }\n").unwrap();
    let p = |f: &PathBuf| f.to_string_lossy().into_owned();
    // --check lists what is not formatted, and changes nothing.
    let (code, out) = staalc(&["fmt", "--check", &p(&messy), &p(&tidy)]);
    assert_eq!(code, Some(1));
    assert_eq!(out.trim(), p(&messy));
    assert_eq!(fs::read_to_string(&messy).unwrap(), "fn f()->Int{return 1}\n");
    // Without it, formats in place.
    assert_eq!(staalc(&["fmt", &p(&messy)]).0, Some(0));
    assert_eq!(fs::read_to_string(&messy).unwrap(), "fn f() -> Int { return 1 }\n");
    assert_eq!(staalc(&["fmt", "--check", &p(&messy), &p(&tidy)]).0, Some(0));
    // A file that does not lex is left as it was.
    assert_eq!(staalc(&["fmt", &p(&broken)]).0, Some(2));
    assert_eq!(fs::read_to_string(&broken).unwrap(), "fn f() { let t = \"\\q\" }\n");
}
