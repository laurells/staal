// The native engine against the interpreter, the reference (phase 5).
//
// 1. Every runtime case (tests/run) runs on both engines with a journal: results,
//    failure kinds and messages, logs and journals must be identical, except for the
//    random run id.
// 2. Generated programs: thousands of random Int/Bool programs, compiled entirely to
//    native code, run on both engines under tight step and depth limits, with
//    literals near the edges of Int, so overflow, division by zero, step limits and
//    depth limits are all reached. Every outcome must match exactly.

#![cfg(feature = "native")]

use intentc::interp::{run_config_engine, Durability, Engine, RunResult};
use serde_json::{json, Value as Json};
use std::fs;
use std::path::{Path, PathBuf};

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("engines").join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A log or journal with the run's random id taken out.
fn normal(v: &Json) -> Json {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        if o.contains_key("run_id") {
            o.insert("run_id".into(), json!("-"));
        }
        for (_, x) in o.iter_mut() {
            *x = normal(x);
        }
    } else if let Some(a) = v.as_array_mut() {
        for x in a.iter_mut() {
            *x = normal(x);
        }
    }
    v
}

/// Text with every occurrence of the run id (it also appears inside operation keys)
/// replaced, then parsed.
fn without_id(text: &str, run_id: &str) -> Json {
    let t = if run_id.is_empty() { text.to_string() } else { text.replace(run_id, "RUN") };
    normal(&serde_json::from_str(&t).unwrap())
}

fn outcome(r: &Result<RunResult, String>) -> Json {
    match r {
        Err(e) => json!({ "config_error": e }),
        Ok(out) => {
            let result = match &out.result {
                Ok(v) => json!({ "ok": v }),
                Err(f) => json!({ "failed": f.kind.to_string(), "message": f.message }),
            };
            let id = out.log.first().and_then(|e| e["run_id"].as_str()).unwrap_or("").to_string();
            let log = without_id(&Json::Array(out.log.clone()).to_string(), &id);
            json!({ "result": result, "log": log })
        }
    }
}

fn journal(path: &Path) -> Vec<Json> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let id = text
        .lines()
        .next()
        .and_then(|l| serde_json::from_str::<Json>(l).ok())
        .and_then(|v| v["run_id"].as_str().map(str::to_string))
        .unwrap_or_default();
    text.lines().map(|l| without_id(l, &id)).collect()
}

#[test]
fn every_runtime_case_is_identical_on_both_engines() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run");
    let mut cases: Vec<PathBuf> =
        fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    cases.sort();
    let work = tmp("cases");
    let (mut compiled, mut ran) = (0usize, 0usize);
    let mut failures = Vec::new();
    for case in &cases {
        let stem = case.file_stem().unwrap().to_string_lossy().to_string();
        let base = stem.split("__").next().unwrap().to_string();
        let src = fs::read_to_string(dir.join(format!("{base}.int"))).unwrap();
        let prog = intentc::check_program(&src).unwrap();
        compiled += intentc::native::eligible(&prog).len();
        let mut cfg: Json = serde_json::from_str(&fs::read_to_string(case).unwrap()).unwrap();
        cfg.as_object_mut().unwrap().remove("expect");
        let mut outs = Vec::new();
        for (tag, engine) in [("i", Engine::Interpreter), ("n", Engine::Native)] {
            let j = work.join(format!("{stem}.{tag}.journal"));
            let r = run_config_engine(&prog, &src, &cfg, Durability::Create(&j), engine);
            outs.push((outcome(&r), journal(&j)));
        }
        ran += 1;
        if outs[0] != outs[1] {
            failures.push(format!("{stem}:\n  interpreter {}\n  native      {}", outs[0].0, outs[1].0));
        }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), ran, failures.join("\n"));
    println!("{ran} runtime cases identical on both engines; {compiled} functions compiled across them");
}

#[test]
fn the_crash_scenario_really_runs_native_code() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/crash/native.int")).unwrap();
    let prog = intentc::check_program(&src).unwrap();
    let mut got: Vec<String> = intentc::native::eligible(&prog).into_iter().collect();
    got.sort();
    assert_eq!(got, ["big", "score"]);
}

// ---------------------------------------------------------------------------------
// Generated programs

/// xorshift64*: deterministic, so a failing seed can be rerun.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

#[derive(Clone, Copy, PartialEq)]
enum K {
    Int,
    Bool,
}

struct FnSig {
    name: String,
    params: Vec<K>,
    ret: K,
}

struct Gen<'a> {
    rng: &'a mut Rng,
    sigs: &'a [FnSig],
    /// The first function this one may call: itself (recursion) and every later one.
    first: usize,
    vars: Vec<Vec<(String, K)>>,
    next_var: usize,
    depth: usize,
    loops: usize,
}

const EDGES: [i64; 12] = [0, 1, -1, 2, 3, 7, 100, 4096, i64::MAX, i64::MIN + 1, 3037000499, 3037000500];

impl Gen<'_> {
    fn int_literal(&mut self) -> String {
        let n = if self.rng.chance(40) { EDGES[self.rng.below(EDGES.len() as u64) as usize] } else { self.rng.below(50) as i64 - 10 };
        // Negative literals are written as `-(n)`, as the language has no negative literals.
        if n < 0 { format!("(-{})", n.unsigned_abs()) } else { n.to_string() }
    }

    fn var_of(&mut self, k: K) -> Option<String> {
        let all: Vec<String> = self.vars.iter().flatten().filter(|(_, vk)| *vk == k).map(|(n, _)| n.clone()).collect();
        if all.is_empty() { None } else { Some(all[self.rng.below(all.len() as u64) as usize].clone()) }
    }

    fn expr(&mut self, k: K, d: usize) -> String {
        let leaf = d >= 4 || self.rng.chance(30);
        if leaf {
            if self.rng.chance(60)
                && let Some(v) = self.var_of(k)
            {
                return v;
            }
            return match k {
                K::Int => self.int_literal(),
                K::Bool => if self.rng.chance(50) { "true".into() } else { "false".into() },
            };
        }
        // A call, sometimes.
        if self.rng.chance(15) {
            let options: Vec<usize> = (self.first..self.sigs.len()).filter(|&i| self.sigs[i].ret == k).collect();
            if !options.is_empty() {
                let i = options[self.rng.below(options.len() as u64) as usize];
                let params = self.sigs[i].params.clone();
                let args: Vec<String> = params.iter().map(|p| self.expr(*p, d + 1)).collect();
                return format!("{}({})", self.sigs[i].name, args.join(", "));
            }
        }
        match k {
            K::Int => match self.rng.below(5) {
                0 => format!("({} + {})", self.expr(K::Int, d + 1), self.expr(K::Int, d + 1)),
                1 => format!("({} - {})", self.expr(K::Int, d + 1), self.expr(K::Int, d + 1)),
                2 => format!("({} * {})", self.expr(K::Int, d + 1), self.expr(K::Int, d + 1)),
                3 => format!("({} / {})", self.expr(K::Int, d + 1), self.expr(K::Int, d + 1)),
                _ => format!("(-{})", self.expr(K::Int, d + 1)),
            },
            K::Bool => match self.rng.below(5) {
                0 => {
                    let ops = ["==", "!=", "<", "<=", ">", ">="];
                    let op = ops[self.rng.below(6) as usize];
                    format!("({} {op} {})", self.expr(K::Int, d + 1), self.expr(K::Int, d + 1))
                }
                1 => format!("({} and {})", self.expr(K::Bool, d + 1), self.expr(K::Bool, d + 1)),
                2 => format!("({} or {})", self.expr(K::Bool, d + 1), self.expr(K::Bool, d + 1)),
                3 => format!("(not {})", self.expr(K::Bool, d + 1)),
                _ => {
                    let op = if self.rng.chance(50) { "==" } else { "!=" };
                    format!("({} {op} {})", self.expr(K::Bool, d + 1), self.expr(K::Bool, d + 1))
                }
            },
        }
    }

    fn pick_kind(&mut self) -> K {
        if self.rng.chance(65) { K::Int } else { K::Bool }
    }

    fn block(&mut self, ind: usize, ret: K, n: usize) -> String {
        self.vars.push(Vec::new());
        let mut out = String::new();
        let pad = "    ".repeat(ind);
        for _ in 0..n {
            let choice = self.rng.below(100);
            if choice < 30 || self.vars.iter().all(|s| s.is_empty()) {
                let k = self.pick_kind();
                let e = self.expr(k, 0);
                let name = format!("v{}", self.next_var);
                self.next_var += 1;
                out.push_str(&format!("{pad}let {name} = {e}\n"));
                self.vars.last_mut().unwrap().push((name, k));
            } else if choice < 50 {
                // Parameters cannot be reassigned: only `let` variables.
                let k = self.pick_kind();
                let locals: Vec<String> =
                    self.vars.iter().flatten().filter(|(n, vk)| *vk == k && !n.starts_with('p')).map(|(n, _)| n.clone()).collect();
                if !locals.is_empty() {
                    let v = locals[self.rng.below(locals.len() as u64) as usize].clone();
                    let e = self.expr(k, 0);
                    out.push_str(&format!("{pad}set {v} = {e}\n"));
                }
            } else if choice < 62 && self.depth < 3 {
                self.depth += 1;
                let c = self.expr(K::Bool, 0);
                let n_ = 1 + self.rng.below(3) as usize;
                let t = self.block(ind + 1, ret, n_);
                let n_ = self.rng.below(3) as usize;
                let e = self.block(ind + 1, ret, n_);
                self.depth -= 1;
                out.push_str(&format!("{pad}if {c} {{\n{t}{pad}}} else {{\n{e}{pad}}}\n"));
            } else if choice < 72 && self.depth < 3 {
                self.depth += 1;
                self.loops += 1;
                let count = self.rng.below(6);
                let n_ = 1 + self.rng.below(3) as usize;
                let body = self.block(ind + 1, ret, n_);
                self.loops -= 1;
                self.depth -= 1;
                out.push_str(&format!("{pad}repeat {count} {{\n{body}{pad}}}\n"));
            } else if choice < 78 && self.depth < 3 {
                // A loop that a counter usually ends; the step limit ends the rest.
                self.depth += 1;
                self.loops += 1;
                let c = format!("c{}", self.next_var);
                self.next_var += 1;
                let limit = self.rng.below(8);
                out.push_str(&format!("{pad}let {c} = 0\n"));
                self.vars.last_mut().unwrap().push((c.clone(), K::Int));
                let n_ = 1 + self.rng.below(3) as usize;
                let body = self.block(ind + 1, ret, n_);
                let p2 = "    ".repeat(ind + 1);
                out.push_str(&format!(
                    "{pad}loop {{\n{p2}set {c} = {c} + 1\n{p2}if {c} > {limit} {{\n{p2}    break\n{p2}}}\n{body}{pad}}}\n"
                ));
                self.loops -= 1;
                self.depth -= 1;
            } else if choice < 82 && self.loops > 0 {
                out.push_str(&format!("{pad}break\n"));
                break;
            } else if choice < 88 {
                let e = self.expr(ret, 0);
                out.push_str(&format!("{pad}return {e}\n"));
                break;
            } else {
                let k = self.pick_kind();
                let e = self.expr(k, 0);
                let name = format!("v{}", self.next_var);
                self.next_var += 1;
                out.push_str(&format!("{pad}let {name} = {e}\n"));
                self.vars.last_mut().unwrap().push((name, k));
            }
        }
        self.vars.pop();
        out
    }
}

fn kind_name(k: K) -> &'static str {
    match k {
        K::Int => "Int",
        K::Bool => "Bool",
    }
}

/// A random program of 1 to 4 functions; `f0` is the entry.
fn program(seed: u64) -> (String, Vec<K>) {
    let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let n = 1 + rng.below(4) as usize;
    let mut sigs = Vec::new();
    for i in 0..n {
        let params: Vec<K> = (0..rng.below(3)).map(|_| if rng.chance(70) { K::Int } else { K::Bool }).collect();
        let ret = if rng.chance(70) { K::Int } else { K::Bool };
        sigs.push(FnSig { name: format!("f{i}"), params, ret });
    }
    let mut src = String::new();
    for i in (0..n).rev() {
        let sig = &sigs[i];
        let params: Vec<String> = sig.params.iter().enumerate().map(|(j, k)| format!("p{j}: {}", kind_name(*k))).collect();
        // Calls go to itself and later functions: no cycle except self-recursion.
        let mut g = Gen { rng: &mut rng, sigs: &sigs, first: i, vars: vec![], next_var: 0, depth: 0, loops: 0 };
        g.vars.push(sig.params.iter().enumerate().map(|(j, k)| (format!("p{j}"), *k)).collect());
        let n_ = 2 + g.rng.below(5) as usize;
        let body = g.block(1, sig.ret, n_);
        let last = g.expr(sig.ret, 0);
        src.push_str(&format!(
            "fn {}({}) -> {} {{\n{body}    return {last}\n}}\n\n",
            sig.name,
            params.join(", "),
            kind_name(sig.ret)
        ));
    }
    (src, sigs[0].params.clone())
}

#[test]
fn generated_programs_behave_identically_on_both_engines() {
    let mut checked = 0;
    let mut skipped = 0;
    let mut outcomes = std::collections::BTreeMap::<String, usize>::new();
    let mut failures = Vec::new();
    // INTENT_GENERATED sets how many (the mutation checks use fewer, to run faster).
    let count: u64 = std::env::var("INTENT_GENERATED").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
    for seed in 0..count {
        let (src, params) = program(seed);
        let prog = match intentc::check_program(&src) {
            Ok(p) => p,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let compiled = intentc::native::eligible(&prog);
        if compiled.len() != prog.fns.len() {
            failures.push(format!("seed {seed}: only {:?} of {} functions compile\n{src}", compiled, prog.fns.len()));
            continue;
        }
        let mut rng = Rng(seed ^ 0xABCDEF);
        let inputs: serde_json::Map<String, Json> = params
            .iter()
            .enumerate()
            .map(|(j, k)| {
                let v = match k {
                    K::Int => json!(EDGES[rng.below(EDGES.len() as u64) as usize]),
                    K::Bool => json!(rng.chance(50)),
                };
                (format!("p{j}"), v)
            })
            .collect();
        let steps = [40u64, 300, 100_000][rng.below(3) as usize];
        let depth = [2u64, 5, 256][rng.below(3) as usize];
        let cfg = json!({ "entry": "f0", "inputs": inputs, "limits": { "max_steps": steps, "max_depth": depth },
                          "model": { "provider": "script", "responses": [] } });
        let i = run_config_engine(&prog, &src, &cfg, Durability::None, Engine::Interpreter);
        let n = run_config_engine(&prog, &src, &cfg, Durability::None, Engine::Native);
        let (oi, on) = (outcome(&i), outcome(&n));
        if oi != on {
            failures.push(format!("seed {seed}:\n{src}\ncfg {cfg}\n  interpreter {}\n  native      {}", oi["result"], on["result"]));
        }
        let kind = match &oi["result"] {
            r if r.get("ok").is_some() => "ok".to_string(),
            r => r["failed"].as_str().unwrap_or("config").to_string(),
        };
        *outcomes.entry(kind).or_default() += 1;
        checked += 1;
        if failures.len() > 5 {
            break;
        }
    }
    assert!(failures.is_empty(), "{} programs differ:\n{}", failures.len(), failures.join("\n\n"));
    println!("{checked} generated programs identical on both engines ({skipped} did not pass the checker); outcomes {outcomes:?}");
    assert!(checked as u64 * 10 >= count * 9, "too few generated programs passed the checker: {checked} of {count}");
    for kind in ["ok", "value", "step_limit", "depth_limit"] {
        assert!(outcomes.get(kind).copied().unwrap_or(0) * 50 >= count as usize, "too few runs ended {kind}: {outcomes:?}");
    }
}


/// Interpreted code (it uses Text) calling native code and carrying on: the steps and
/// depth native code used must count against the same limits. Every step limit from 1
/// to past the end, and every depth limit, must stop both engines at the same point.
#[test]
fn mixed_programs_share_one_step_counter_and_one_depth() {
    let src = r#"fn work(n: Int) -> Int {
    let t = 0
    repeat 6 {
        set t = t + n
    }
    if n > 0 {
        return t + work(n - 1)
    }
    return t
}

fn go(label: Text) -> Text {
    let a = work(3)
    let b = label + ": " + json(a)
    let c = work(2)
    let d = b + ", " + json(c)
    return d
}
"#;
    let prog = intentc::check_program(src).unwrap();
    assert_eq!(intentc::native::eligible(&prog).into_iter().collect::<Vec<_>>(), ["work"]);
    let run = |steps: u64, depth: u64, engine: Engine| {
        let cfg = json!({ "entry": "go", "inputs": { "label": "total" },
                          "limits": { "max_steps": steps, "max_depth": depth },
                          "model": { "provider": "script", "responses": [] } });
        outcome(&run_config_engine(&prog, src, &cfg, Durability::None, engine))["result"].clone()
    };
    let full = run(1_000_000, 256, Engine::Interpreter);
    assert_eq!(full, json!({ "ok": "total: 36, 18" }));
    let mut limited = 0;
    for steps in 1..200 {
        let (i, n) = (run(steps, 256, Engine::Interpreter), run(steps, 256, Engine::Native));
        assert_eq!(i, n, "max_steps {steps}");
        if i["failed"] == "step_limit" {
            limited += 1;
        }
    }
    assert!(limited > 100, "the step limits tried did not reach the end of the program");
    for depth in 1..8 {
        assert_eq!(run(1_000_000, depth, Engine::Interpreter), run(1_000_000, depth, Engine::Native), "max_depth {depth}");
    }
}
