pub mod ast;
pub mod budget;
pub mod capi;
pub mod checker;
pub mod interp;
pub mod lexer;
pub mod modules;
#[cfg(feature = "native")]
pub mod native;
pub mod packages;
pub mod parser;
pub mod pattern;
pub mod targets;

use checker::Diagnostic;
pub use modules::SourceFile;
use std::path::Path;

/// A checked program and the files it came from.
pub struct Checked {
    /// Every file of the program, indexed by `Pos::file`; 0 is the entry file.
    pub files: Vec<SourceFile>,
    /// Identifies the exact program text across all files; journals record its hash.
    pub fingerprint: String,
    /// The program, only if every file loads and every check passes, so nothing that
    /// fails a check can ever be run.
    pub result: Result<ast::Program, Vec<Diagnostic>>,
}

/// Loads, resolves and checks a program: the entry file and every module it imports.
/// `fetch` returns the file for a module path such as ["sms", "termii"].
pub fn check_files(entry: SourceFile, fetch: impl FnMut(&[String]) -> Result<SourceFile, String>) -> Checked {
    let loaded = modules::load(entry, fetch);
    let fingerprint = loaded.fingerprint();
    let result = loaded.program.and_then(|prog| {
        let (mut diags, for_bounds) = checker::check_with_bounds(&prog);
        diags.extend(budget::check(&prog, &for_bounds));
        if diags.is_empty() { Ok(prog) } else { Err(diags) }
    });
    Checked { files: loaded.files, fingerprint, result }
}

/// Checks the program in the file at `path`. Modules are found under the folder that
/// holds it: `import sms.termii` reads `<folder>/sms/termii.int`, unless `sms` is a
/// package the folder's `intent.json` depends on (see `packages`). Packages are
/// verified against `intent.lock` before anything is read, and their code is checked
/// against the authority their manifests declare (E108).
pub fn check_path(path: &Path) -> Result<Checked, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let root = if root.as_os_str().is_empty() { Path::new(".").to_path_buf() } else { root };
    let project = packages::Project::open(&root)?.map(std::rc::Rc::new);
    let entry = SourceFile { name: path.display().to_string(), text };
    let loaded = modules::load_from(entry, packages::ProjectSource::new(root, project.clone(), path));
    let mut fingerprint = loaded.fingerprint();
    if let Some(p) = &project
        && !p.packages.is_empty()
    {
        fingerprint.push_str("\npackages\n");
        for (n, pkg) in &p.packages {
            fingerprint.push_str(&format!("{n} {} {}\n", pkg.dep.git, pkg.dep.commit));
        }
    }
    let result = loaded.program.and_then(|prog| {
        let (mut diags, for_bounds) = checker::check_with_bounds(&prog);
        diags.extend(budget::check(&prog, &for_bounds));
        if let Some(p) = &project {
            diags.extend(packages::check_authority(&prog, &loaded.paths, p));
        }
        if diags.is_empty() { Ok(prog) } else { Err(diags) }
    });
    Ok(Checked { files: loaded.files, fingerprint, result })
}

fn no_modules(module: &[String]) -> Result<SourceFile, String> {
    Err(format!(
        "module '{}' can only be found when the program is checked from a file",
        module.join(".")
    ))
}

/// Checks a single-file program given as text. Returns the program only if every
/// check passes.
pub fn check_program(src: &str) -> Result<ast::Program, Vec<Diagnostic>> {
    check_files(SourceFile { name: "<source>".into(), text: src.into() }, no_modules).result
}

/// Checks a single-file program given as text. Returns all diagnostics (empty = OK).
pub fn compile_check(src: &str) -> Vec<Diagnostic> {
    check_program(src).err().unwrap_or_default()
}

/// A diagnostic as `file:line:col: error[CODE]: message`, naming the file it is in.
pub fn format_diag_in(files: &[SourceFile], d: &Diagnostic) -> String {
    let file = files.get(d.pos.file as usize).map_or("<unknown file>", |f| f.name.as_str());
    format_diag(file, d)
}

pub fn format_diag(file: &str, d: &Diagnostic) -> String {
    format!("{file}:{}:{}: error[{}]: {}", d.pos.line, d.pos.col, d.code, d.msg)
}
