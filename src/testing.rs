//! Tests written in Staal: what `staalc test` runs and reports, as a library, so a
//! program embedding staalc (Staal Studio) gets each case's result as data, from the
//! same code the command line uses.
//!
//! A test holds no authority. It is a function taking no parameters, so it is given no
//! grant and no secret; a test config may not give `entry`, `inputs`, `grants` or
//! `secrets`; its model is scripted, never a provider reached with a key from the
//! environment; and it may not set `net` or `fs`, which a run with no grant has no use
//! for (and whose `extra_root_certificates` would read files by path). So a test
//! reaches nothing a run with no grants could not, whatever its file imports: a
//! package's code and a library capability need a grant to act, and a test has none.

use crate::interp::{self, Durability, Engine, RunResult};
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};

/// The keys a test config may hold: everything else a run config holds either gives
/// authority or has no use without it.
const ALLOWED: [&str; 3] = ["model", "adapters", "limits"];

/// How every test runs: the base of each test's run config, and the engines.
#[derive(Debug, Clone)]
pub struct Settings {
    base: Json,
    engines: Vec<Engine>,
}

impl Settings {
    /// From a test config (`--config`'s JSON), if any, and the engines to run each
    /// test on, in order. Without a config, the model is scripted with no replies, so
    /// a test that asks a model fails rather than reaching one.
    pub fn new(config: Option<&Json>, engines: Vec<Engine>) -> Result<Settings, String> {
        if engines.is_empty() {
            return Err("no engine to run the tests on".into());
        }
        let mut base = json!({ "model": { "provider": "script", "responses": [] } });
        if let Some(c) = config {
            let m = c.as_object().ok_or("expected a JSON object")?;
            for k in m.keys() {
                if ["entry", "inputs", "grants", "secrets"].contains(&k.as_str()) {
                    return Err(format!("a test config cannot give \"{k}\": each test is its own entry and takes no parameters"));
                }
                if !ALLOWED.contains(&k.as_str()) {
                    return Err(format!(
                        "a test config cannot set \"{k}\": a test is given no grant, so it has no use for it; a test config holds only \"model\", \"adapters\" and \"limits\""
                    ));
                }
            }
            if let Some(model) = m.get("model")
                && model.get("provider").and_then(Json::as_str) != Some("script")
            {
                return Err("a test's model is scripted (\"provider\": \"script\"): a test reaches no model, and holds no key".into());
            }
            base = c.clone();
        }
        Ok(Settings { base, engines })
    }
}

/// What became of one case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// Why it failed, as `staalc test` says it.
    Failed(String),
}

/// One result: a test of a file, or the file itself when it could not be tested (check
/// errors, or no tests in it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    pub file: PathBuf,
    /// The test function's name; None for a result about the whole file.
    pub test: Option<String>,
    pub outcome: Outcome,
    /// For a file with check errors, each error as `staalc check` prints it.
    pub diagnostics: Vec<String>,
}

impl Case {
    /// How `staalc test` names it: `<file>` or `<file>::<test>`.
    pub fn id(&self) -> String {
        match &self.test {
            Some(t) => format!("{}::{t}", self.file.display()),
            None => self.file.display().to_string(),
        }
    }
}

/// How many cases passed and failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    pub passed: usize,
    pub failed: usize,
}

/// Runs every test in `files`, in order, giving each case to `each` as it ends. An
/// error is something that stops all testing (a file that cannot be read, a test that
/// cannot be started), as `staalc test` exits 2 for; a test that fails is a case.
pub fn run(files: &[PathBuf], settings: &Settings, mut each: impl FnMut(&Case)) -> Result<Summary, String> {
    let mut summary = Summary::default();
    let mut give = |case: Case, summary: &mut Summary| {
        match case.outcome {
            Outcome::Passed => summary.passed += 1,
            Outcome::Failed(_) => summary.failed += 1,
        }
        each(&case);
    };
    for file in files {
        let checked = crate::check_path(file)?;
        let failed = |why: &str| Case { file: file.clone(), test: None, outcome: Outcome::Failed(why.into()), diagnostics: Vec::new() };
        let prog = match &checked.result {
            Ok(p) => p,
            Err(diags) => {
                let mut case = failed("it has check errors");
                case.diagnostics = diags.iter().map(|d| crate::format_diag_in(&checked.files, d)).collect();
                give(case, &mut summary);
                continue;
            }
        };
        let tests: Vec<&crate::ast::FnDecl> =
            prog.fns.iter().filter(|f| f.pos.file == 0 && f.method_of.is_none() && f.name.starts_with("test_") && !f.name.contains(['.', ':', ' ', '<'])).collect();
        if tests.is_empty() {
            give(failed("no tests (functions named test_...)"), &mut summary);
            continue;
        }
        for t in tests {
            let case = |outcome| Case { file: file.clone(), test: Some(t.name.clone()), outcome, diagnostics: Vec::new() };
            if !t.params.is_empty() || t.ret.is_some() || !t.generics.is_empty() {
                give(case(Outcome::Failed("a test takes no parameters and gives no result".into())), &mut summary);
                continue;
            }
            let mut cfg = settings.base.clone();
            cfg["entry"] = json!(t.name);
            let mut runs = Vec::new();
            for e in &settings.engines {
                let r = interp::run_config_engine(prog, &checked.fingerprint, &cfg, Durability::None, *e)
                    .map_err(|err| format!("{}: {err}", case(Outcome::Passed).id()))?;
                runs.push((label(*e), r));
            }
            let outcome = match verdict(&runs) {
                Ok(()) => Outcome::Passed,
                Err(why) => Outcome::Failed(why),
            };
            give(case(outcome), &mut summary);
        }
    }
    Ok(summary)
}

fn label(e: Engine) -> &'static str {
    match e {
        Engine::Interpreter => "interpreter",
        Engine::Native => "native",
    }
}

/// The `*_test.staal` files under `dir`, in order, outside hidden folders, `target`
/// and `node_modules`.
pub fn files_under(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .map(|e| e.map(|e| e.path()).map_err(|e| format!("cannot read {}: {e}", dir.display())))
        .collect::<Result<_, _>>()?;
    entries.sort();
    for p in entries {
        let file_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !file_name.starts_with('.') && file_name != "target" && file_name != "node_modules" {
                out.append(&mut files_under(&p)?);
            }
        } else if file_name.ends_with("_test.staal") {
            out.push(p);
        }
    }
    Ok(out)
}

/// Whether a test passed, given its run on each engine: every engine must give the
/// same outcome and the same log, and the outcome must be a return.
fn verdict(runs: &[(&str, RunResult)]) -> Result<(), String> {
    let outcome = |r: &RunResult| match &r.result {
        Ok(_) => Ok(()),
        Err(f) => Err(format!("{}: {}", f.kind, f.message)),
    };
    let Some(((first_label, first), rest)) = runs.split_first() else {
        return Err("no engine ran it".into());
    };
    if let Some((label, r)) = rest.iter().find(|(_, r)| !interp::same_run(r, first)) {
        let show = |o: Result<(), String>| o.err().unwrap_or_else(|| "passed".into());
        let mut why = format!("the engines disagree: {first_label} {}, {label} {}", show(outcome(first)), show(outcome(r)));
        if outcome(r) == outcome(first) {
            why.push_str(" (the same outcome, but different logs)");
        }
        return Err(why);
    }
    outcome(first)
}

#[cfg(test)]
mod tests {
    use super::verdict;
    use crate::interp::{FailKind, Failure, RunResult};
    use serde_json::json;

    fn run(id: &str, result: Result<i64, &str>, extra: &str) -> RunResult {
        RunResult {
            result: result.map(|n| json!(n)).map_err(|m| Failure { kind: FailKind::Action, message: m.into(), raised: None }),
            log: vec![json!({ "event": "run_start", "run_id": id }), json!({ "event": "x", "key": format!("{id}-1{extra}") })],
            usage: json!({}),
        }
    }

    #[test]
    fn engines_must_agree_on_the_outcome_and_on_the_log_apart_from_the_run_id() {
        let both = |a: RunResult, b: RunResult| verdict(&[("interpreter", a), ("native", b)]);
        // Different run ids, in the log and in keys made from them: the same.
        assert_eq!(both(run("aa", Ok(1), ""), run("bb", Ok(1), "")), Ok(()));
        assert_eq!(both(run("aa", Err("boom"), ""), run("bb", Err("boom"), "")), Err("action: boom".into()));
        assert_eq!(
            both(run("aa", Ok(1), ""), run("bb", Err("boom"), "")),
            Err("the engines disagree: interpreter passed, native action: boom".into())
        );
        assert_eq!(
            both(run("aa", Err("one"), ""), run("bb", Err("two"), "")),
            Err("the engines disagree: interpreter action: one, native action: two".into())
        );
        assert_eq!(
            both(run("aa", Ok(1), ""), run("bb", Ok(1), "x")),
            Err("the engines disagree: interpreter passed, native passed (the same outcome, but different logs)".into())
        );
        // One engine alone decides by its outcome; none is never a pass.
        assert_eq!(verdict(&[("interpreter", run("aa", Ok(1), ""))]), Ok(()));
        assert!(verdict(&[]).is_err());
    }
}
