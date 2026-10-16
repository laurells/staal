// Every file in tests/fixtures/pass must compile with no errors.
// Every file in tests/fixtures/fail must start with `// expect: CODE`
// and the compiler must report exactly those codes, in order (e.g. `// expect: E007 E007`).

use std::fs;
use std::path::Path;

fn files(dir: &str) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(dir))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map_or(false, |e| e == "int"))
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no fixtures in {dir}");
    v
}

#[test]
fn valid_programs_compile() {
    for f in files("tests/fixtures/pass") {
        let src = fs::read_to_string(&f).unwrap();
        let diags = intentc::compile_check(&src);
        assert!(diags.is_empty(), "{} should compile, got {:?}", f.display(), diags);
    }
}

#[test]
fn invalid_programs_are_rejected_with_the_right_code() {
    for f in files("tests/fixtures/fail") {
        let src = fs::read_to_string(&f).unwrap();
        let expected: Vec<&str> = src
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("// expect: "))
            .unwrap_or_else(|| panic!("{} lacks `// expect: CODE`", f.display()))
            .split_whitespace()
            .collect();
        let codes: Vec<&str> = intentc::compile_check(&src).iter().map(|d| d.code).collect();
        assert_eq!(codes, expected, "{}", f.display());
    }
}
