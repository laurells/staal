pub mod aliases;
pub mod ast;
pub mod budget;
pub mod calls;
pub mod capi;
pub mod cli;
pub mod checker;
pub mod consts;
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
    let result = loaded.program.and_then(|mut prog| {
        let (mut diags, for_bounds, capacities) = checker::check_with_bounds(&prog);
        diags.extend(budget::check(&prog, &for_bounds));
        if !diags.is_empty() {
            return Err(diags);
        }
        checker::supply_capacities(&mut prog, &capacities);
        Ok(prog)
    });
    Checked { files: loaded.files, fingerprint, result }
}

/// Checks the program in the file at `path`. Modules are found under the folder that
/// holds it: `import sms.termii` reads `<folder>/sms/termii.staal`, unless `sms` is a
/// package the folder's `staal.json` depends on (see `packages`). Packages are
/// verified against `staal.lock` before anything is read, and their code is checked
/// against the authority their manifests declare (E108).
pub fn check_path(path: &Path) -> Result<Checked, String> {
    check_path_recorded(path).map(|(c, _)| c)
}

/// Everything a program was loaded from, so it can be loaded again without its files:
/// what `staalc build` puts in an executable.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub entry: (String, String),
    /// (importing module, path written, module it means)
    pub locate: Vec<(Vec<String>, Vec<String>, Vec<String>)>,
    /// (module, file name, text)
    pub files: Vec<(Vec<String>, String, String)>,
    /// The part of the fingerprint that names the packages and their commits.
    pub packages: String,
}

impl Recorded {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "entry": [self.entry.0, self.entry.1],
            "locate": self.locate,
            "files": self.files.iter().map(|(m, n, t)| serde_json::json!([m, n, t])).collect::<Vec<_>>(),
            "packages": self.packages,
        })
    }

    pub fn from_json(j: &serde_json::Value) -> Result<Recorded, String> {
        let bad = || "the recorded program is damaged".to_string();
        let strs = |v: &serde_json::Value| -> Result<Vec<String>, String> {
            v.as_array().ok_or_else(bad)?.iter().map(|x| x.as_str().map(str::to_string).ok_or_else(bad)).collect()
        };
        let s = |v: &serde_json::Value| v.as_str().map(str::to_string).ok_or_else(bad);
        let entry = j["entry"].as_array().filter(|a| a.len() == 2).ok_or_else(bad)?;
        let mut locate = Vec::new();
        for l in j["locate"].as_array().ok_or_else(bad)? {
            let l = l.as_array().filter(|a| a.len() == 3).ok_or_else(bad)?;
            locate.push((strs(&l[0])?, strs(&l[1])?, strs(&l[2])?));
        }
        let mut files = Vec::new();
        for f in j["files"].as_array().ok_or_else(bad)? {
            let f = f.as_array().filter(|a| a.len() == 3).ok_or_else(bad)?;
            files.push((strs(&f[0])?, s(&f[1])?, s(&f[2])?));
        }
        Ok(Recorded { entry: (s(&entry[0])?, s(&entry[1])?), locate, files, packages: s(&j["packages"])? })
    }
}

struct Recording<S: modules::Source> {
    inner: S,
    rec: std::rc::Rc<std::cell::RefCell<Recorded>>,
}

impl<S: modules::Source> modules::Source for Recording<S> {
    fn locate(&mut self, from: &[String], written: &[String]) -> Result<Vec<String>, String> {
        let to = self.inner.locate(from, written)?;
        self.rec.borrow_mut().locate.push((from.to_vec(), written.to_vec(), to.clone()));
        Ok(to)
    }
    fn read(&mut self, module: &[String]) -> Result<SourceFile, String> {
        let f = self.inner.read(module)?;
        self.rec.borrow_mut().files.push((module.to_vec(), f.name.clone(), f.text.clone()));
        Ok(f)
    }
}

/// Loads a program again from what was recorded: the same files, the same imports.
struct Replay<'a>(&'a Recorded);

impl modules::Source for Replay<'_> {
    fn locate(&mut self, from: &[String], written: &[String]) -> Result<Vec<String>, String> {
        let hit = self.0.locate.iter().find(|(f, w, _)| f == from && w == written);
        hit.map(|(_, _, t)| t.clone()).ok_or_else(|| format!("'{}' was not part of the program when it was built", written.join(".")))
    }
    fn read(&mut self, module: &[String]) -> Result<SourceFile, String> {
        let hit = self.0.files.iter().find(|(m, _, _)| m == module);
        hit.map(|(_, name, text)| SourceFile { name: name.clone(), text: text.clone() })
            .ok_or_else(|| format!("module '{}' was not part of the program when it was built", module.join(".")))
    }
}

/// `check_path`, also returning what the program was loaded from.
pub fn check_path_recorded(path: &Path) -> Result<(Checked, Recorded), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let root = if root.as_os_str().is_empty() { Path::new(".").to_path_buf() } else { root };
    let project = packages::Project::open(&root)?.map(std::rc::Rc::new);
    let entry = SourceFile { name: path.display().to_string(), text };
    let rec = std::rc::Rc::new(std::cell::RefCell::new(Recorded {
        entry: (entry.name.clone(), entry.text.clone()),
        locate: Vec::new(),
        files: Vec::new(),
        packages: String::new(),
    }));
    let source = Recording { inner: packages::ProjectSource::new(root, project.clone(), path), rec: rec.clone() };
    let loaded = modules::load_from(entry, source);
    let mut packages_part = String::new();
    if let Some(p) = &project
        && !p.packages.is_empty()
    {
        packages_part.push_str("\npackages\n");
        for (n, pkg) in &p.packages {
            packages_part.push_str(&format!("{n} {} {}\n", pkg.dep.git, pkg.dep.commit));
        }
    }
    let fingerprint = loaded.fingerprint() + &packages_part;
    rec.borrow_mut().packages = packages_part;
    let result = loaded.program.and_then(|mut prog| {
        let (mut diags, for_bounds, capacities) = checker::check_with_bounds(&prog);
        diags.extend(budget::check(&prog, &for_bounds));
        if let Some(p) = &project {
            diags.extend(packages::check_authority(&prog, &loaded.paths, p));
        }
        if !diags.is_empty() {
            return Err(diags);
        }
        checker::supply_capacities(&mut prog, &capacities);
        Ok(prog)
    });
    let recorded = rec.borrow().clone();
    Ok((Checked { files: loaded.files, fingerprint, result }, recorded))
}

/// Loads and checks a recorded program again, without its files. Package authority
/// (E108) was checked when it was recorded and needs the packages' manifests, so it is
/// not checked again; every other rule is.
pub fn check_recorded(rec: &Recorded) -> Checked {
    let entry = SourceFile { name: rec.entry.0.clone(), text: rec.entry.1.clone() };
    let loaded = modules::load_from(entry, Replay(rec));
    let fingerprint = loaded.fingerprint() + &rec.packages;
    let result = loaded.program.and_then(|mut prog| {
        let (mut diags, for_bounds, capacities) = checker::check_with_bounds(&prog);
        diags.extend(budget::check(&prog, &for_bounds));
        if !diags.is_empty() {
            return Err(diags);
        }
        checker::supply_capacities(&mut prog, &capacities);
        Ok(prog)
    });
    Checked { files: loaded.files, fingerprint, result }
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
