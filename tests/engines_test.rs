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

use staalc::interp::{run_config_engine, Durability, Engine, RunResult};
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
        let src = fs::read_to_string(dir.join(format!("{base}.staal"))).unwrap();
        let prog = staalc::check_program(&src).unwrap();
        compiled += staalc::native::eligible(&prog).len();
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
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/crash/native.staal")).unwrap();
    let prog = staalc::check_program(&src).unwrap();
    let mut got: Vec<String> = staalc::native::eligible(&prog).into_iter().collect();
    got.sort();
    assert_eq!(got, ["big", "score"]);
}

#[test]
fn the_enum_cases_really_run_native_code() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/enums.staal")).unwrap();
    let prog = staalc::check_program(&src).unwrap();
    let mut got: Vec<String> = staalc::native::eligible(&prog).into_iter().collect();
    got.sort();
    // Everything but `triage`, which asks a model.
    assert_eq!(got, ["describe", "double", "first_pending", "make", "same", "settled", "total"]);
}

#[test]
fn the_optional_cases_really_run_native_code() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/optional.staal")).unwrap();
    let prog = staalc::check_program(&src).unwrap();
    let mut got: Vec<String> = staalc::native::eligible(&prog).into_iter().collect();
    got.sort();
    // Everything but `ask`, which asks a model.
    let want = ["double", "find", "greet", "latest", "missing", "pair", "reach", "same", "total", "track", "wrap"];
    assert_eq!(got, want);
}

#[test]
fn the_alias_cases_really_run_native_code() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/aliases.staal")).unwrap();
    let prog = staalc::check_program(&src).unwrap();
    let mut got: Vec<String> = staalc::native::eligible(&prog).into_iter().collect();
    got.sort();
    assert_eq!(got, ["capped", "closed", "contact", "first", "mail", "open_count", "owed"]);
}

#[test]
fn the_argument_cases_really_run_native_code() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/arguments.staal")).unwrap();
    let prog = staalc::check_program(&src).unwrap();
    let mut got: Vec<String> = staalc::native::eligible(&prog).into_iter().collect();
    got.sort();
    let want =
        ["all_named", "boom", "consts", "defaults", "entry_default", "note", "optional_default", "order", "pair", "retries", "some_named"];
    assert_eq!(got, want);
}

#[test]
fn the_tuple_cases_really_run_native_code() {
    let src = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/tuples.staal")).unwrap();
    let prog = staalc::check_program(&src).unwrap();
    let mut got: Vec<String> = staalc::native::eligible(&prog).into_iter().collect();
    got.sort();
    let want = ["first", "make", "maybe", "min_max", "nested", "overflow", "pairs", "same", "split", "spread", "width"];
    assert_eq!(got, want);
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
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

/// The kinds of value generated programs use. `R` is `type R { a: Int, t: Text, f: Bool }`;
/// lists are `List<Int, 3>`; money is in USD.
#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    Int,
    Bool,
    Text,
    Rec,
    List,
    Money,
}

const KINDS: [K; 6] = [K::Int, K::Bool, K::Text, K::Rec, K::List, K::Money];

fn kind_name(k: K) -> &'static str {
    match k {
        K::Int => "Int",
        K::Bool => "Bool",
        K::Text => "Text",
        K::Rec => "R",
        K::List => "List<Int, 3>",
        K::Money => "Money",
    }
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
const TEXTS: [&str; 7] = ["", "a", "ab", "Abc ", " x y ", "\u{e9}t\u{e9}", "a/b?c"];
const MONEY: [&str; 6] = ["0.00 USD", "0.01 USD", "1.50 USD", "12.00 USD", "99.99 USD", "90000000000000000.00 USD"];

impl Gen<'_> {
    fn int_literal(&mut self) -> String {
        let n = if self.rng.chance(40) { *self.rng.pick(&EDGES) } else { self.rng.below(50) as i64 - 10 };
        // The language has no negative literals: -n is written as negation.
        if n < 0 { format!("(-{})", n.unsigned_abs()) } else { n.to_string() }
    }

    fn literal(&mut self, k: K, d: usize) -> String {
        match k {
            K::Int => self.int_literal(),
            K::Bool => (if self.rng.chance(50) { "true" } else { "false" }).into(),
            K::Text => format!("{:?}", self.rng.pick(&TEXTS)),
            K::Money => self.rng.pick(&MONEY).to_string(),
            K::Rec => {
                let mut fields = [
                    format!("a: {}", self.expr(K::Int, d + 1)),
                    format!("t: {}", self.expr(K::Text, d + 1)),
                    format!("f: {}", self.expr(K::Bool, d + 1)),
                ];
                // Fields in any order: the record keeps declaration order.
                let i = self.rng.below(3) as usize;
                fields.swap(0, i);
                // In parentheses: `if R { ... }.f {` would read as the start of a block.
                format!("(R {{ {} }})", fields.join(", "))
            }
            K::List => {
                let items: Vec<String> = (0..3).map(|_| self.expr(K::Int, d + 1)).collect();
                format!("[{}]", items.join(", "))
            }
        }
    }

    fn var_of(&mut self, k: K) -> Option<String> {
        let all: Vec<String> = self.vars.iter().flatten().filter(|(_, vk)| *vk == k).map(|(n, _)| n.clone()).collect();
        if all.is_empty() { None } else { Some(self.rng.pick(&all).clone()) }
    }

    fn expr(&mut self, k: K, d: usize) -> String {
        let leaf = d >= 3 || self.rng.chance(30);
        if leaf {
            if self.rng.chance(60)
                && let Some(v) = self.var_of(k)
            {
                return v;
            }
            if matches!(k, K::Rec | K::List) && d >= 3 {
                // A literal of these nests further; keep it shallow.
                return match k {
                    K::Rec => "(R { a: 1, t: \"z\", f: true })".into(),
                    _ => "[1, 2, 3]".into(),
                };
            }
            return self.literal(k, d);
        }
        if self.rng.chance(12) {
            let options: Vec<usize> = (self.first..self.sigs.len()).filter(|&i| self.sigs[i].ret == k).collect();
            if !options.is_empty() {
                let i = *self.rng.pick(&options);
                let params = self.sigs[i].params.clone();
                let args: Vec<String> = params.iter().map(|p| self.expr(*p, d + 1)).collect();
                return format!("{}({})", self.sigs[i].name, args.join(", "));
            }
        }
        let e = |g: &mut Gen, k: K| g.expr(k, d + 1);
        match k {
            K::Int => match self.rng.below(10) {
                0 => format!("({} + {})", e(self, K::Int), e(self, K::Int)),
                1 => format!("({} - {})", e(self, K::Int), e(self, K::Int)),
                2 => format!("({} * {})", e(self, K::Int), e(self, K::Int)),
                3 => format!("({} / {})", e(self, K::Int), e(self, K::Int)),
                4 => format!("(-{})", e(self, K::Int)),
                5 => format!("len({})", e(self, K::Text)),
                6 => format!("{}.a", e(self, K::Rec)),
                7 => format!("{}[{}]", e(self, K::List), e(self, K::Int)),
                8 => format!("minor_units({})", e(self, K::Money)),
                _ => format!("len({})", e(self, K::List)),
            },
            K::Bool => match self.rng.below(10) {
                0 => {
                    let op = *self.rng.pick(&["==", "!=", "<", "<=", ">", ">="]);
                    format!("({} {op} {})", e(self, K::Int), e(self, K::Int))
                }
                1 => format!("({} and {})", e(self, K::Bool), e(self, K::Bool)),
                2 => format!("({} or {})", e(self, K::Bool), e(self, K::Bool)),
                3 => format!("(not {})", e(self, K::Bool)),
                4 => format!("contains({}, {})", e(self, K::Text), e(self, K::Text)),
                5 => format!("starts_with({}, {})", e(self, K::Text), e(self, K::Text)),
                6 => format!("({} == {})", e(self, K::Text), e(self, K::Text)),
                7 => format!("{}.f", e(self, K::Rec)),
                8 => {
                    let op = *self.rng.pick(&["==", "!=", "<", ">="]);
                    format!("({} {op} {})", e(self, K::Money), e(self, K::Money))
                }
                _ => format!("contains({}, {})", e(self, K::List), e(self, K::Int)),
            },
            K::Text => match self.rng.below(9) {
                0 | 1 => format!("({} + {})", e(self, K::Text), e(self, K::Text)),
                2 => format!("lower({})", e(self, K::Text)),
                3 => format!("trim({})", e(self, K::Text)),
                4 => {
                    let from = *self.rng.pick(&["a", "b", " ", ""]);
                    format!("replace({}, {from:?}, {})", e(self, K::Text), e(self, K::Text))
                }
                5 => {
                    let k = *self.rng.pick(&KINDS);
                    format!("json({})", e(self, k))
                }
                6 => format!("{}.t", e(self, K::Rec)),
                7 => format!("url_segment({})", e(self, K::Text)),
                _ => format!("currency({})", e(self, K::Money)),
            },
            K::Rec => self.literal(K::Rec, d),
            K::List => self.literal(K::List, d),
            K::Money => match self.rng.below(4) {
                0 => format!("({} + {})", e(self, K::Money), e(self, K::Money)),
                1 => format!("({} - {})", e(self, K::Money), e(self, K::Money)),
                2 => format!("({} * {})", e(self, K::Money), e(self, K::Int)),
                _ => format!("({} * {})", e(self, K::Int), e(self, K::Money)),
            },
        }
    }

    fn pick_kind(&mut self) -> K {
        if self.rng.chance(40) { K::Int } else { *self.rng.pick(&KINDS) }
    }

    fn block(&mut self, ind: usize, ret: K, n: usize) -> String {
        self.vars.push(Vec::new());
        let mut out = String::new();
        let pad = "    ".repeat(ind);
        for _ in 0..n {
            let choice = self.rng.below(100);
            if choice < 28 || self.vars.iter().all(|s| s.is_empty()) {
                let k = self.pick_kind();
                let e = self.expr(k, 0);
                let name = format!("v{}", self.next_var);
                self.next_var += 1;
                out.push_str(&format!("{pad}let {name} = {e}\n"));
                self.vars.last_mut().unwrap().push((name, k));
            } else if choice < 46 {
                // Parameters cannot be reassigned: only `let` variables.
                let k = self.pick_kind();
                let locals: Vec<String> =
                    self.vars.iter().flatten().filter(|(n, vk)| *vk == k && n.starts_with('v')).map(|(n, _)| n.clone()).collect();
                if !locals.is_empty() {
                    let v = self.rng.pick(&locals).clone();
                    let e = self.expr(k, 0);
                    out.push_str(&format!("{pad}set {v} = {e}\n"));
                }
            } else if choice < 57 && self.depth < 3 {
                self.depth += 1;
                let c = self.expr(K::Bool, 0);
                let n1 = 1 + self.rng.below(3) as usize;
                let t = self.block(ind + 1, ret, n1);
                let n2 = self.rng.below(3) as usize;
                let e = self.block(ind + 1, ret, n2);
                self.depth -= 1;
                out.push_str(&format!("{pad}if {c} {{\n{t}{pad}}} else {{\n{e}{pad}}}\n"));
            } else if choice < 65 && self.depth < 3 {
                self.depth += 1;
                self.loops += 1;
                let count = self.rng.below(6);
                let n1 = 1 + self.rng.below(3) as usize;
                let body = self.block(ind + 1, ret, n1);
                self.loops -= 1;
                self.depth -= 1;
                out.push_str(&format!("{pad}repeat {count} {{\n{body}{pad}}}\n"));
            } else if choice < 71 && self.depth < 3 {
                // A `for` over a list, its item bound in the body.
                self.depth += 1;
                self.loops += 1;
                let list = self.expr(K::List, 1);
                let item = format!("i{}", self.next_var);
                self.next_var += 1;
                self.vars.push(vec![(item.clone(), K::Int)]);
                let n1 = 1 + self.rng.below(3) as usize;
                let body = self.block(ind + 1, ret, n1);
                self.vars.pop();
                self.loops -= 1;
                self.depth -= 1;
                out.push_str(&format!("{pad}for {item} in {list} {{\n{body}{pad}}}\n"));
            } else if choice < 76 && self.depth < 3 {
                // A loop that a counter usually ends; the step limit ends the rest.
                self.depth += 1;
                self.loops += 1;
                let c = format!("c{}", self.next_var);
                self.next_var += 1;
                let limit = self.rng.below(8);
                out.push_str(&format!("{pad}let {c} = 0\n"));
                self.vars.last_mut().unwrap().push((c.clone(), K::Int));
                let n1 = 1 + self.rng.below(3) as usize;
                let body = self.block(ind + 1, ret, n1);
                let p2 = "    ".repeat(ind + 1);
                out.push_str(&format!(
                    "{pad}loop {{\n{p2}set {c} = {c} + 1\n{p2}if {c} > {limit} {{\n{p2}    break\n{p2}}}\n{body}{pad}}}\n"
                ));
                self.loops -= 1;
                self.depth -= 1;
            } else if choice < 80 && self.loops > 0 {
                out.push_str(&format!("{pad}break\n"));
                break;
            } else if choice < 83 {
                let c = self.expr(K::Bool, 1);
                let m = self.expr(K::Text, 1);
                out.push_str(&format!("{pad}if {c} {{\n{pad}    fail({m})\n{pad}}}\n"));
            } else if choice < 90 {
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

/// A random program of 1 to 4 functions; `f0` is the entry.
fn program(seed: u64) -> (String, Vec<K>) {
    let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let n = 1 + rng.below(4) as usize;
    let mut sigs = Vec::new();
    for i in 0..n {
        let params: Vec<K> = (0..rng.below(4)).map(|_| if rng.chance(40) { K::Int } else { *rng.pick(&KINDS) }).collect();
        let ret = if rng.chance(40) { K::Int } else { *rng.pick(&KINDS) };
        sigs.push(FnSig { name: format!("f{i}"), params, ret });
    }
    let mut src = String::from("type R { a: Int, t: Text, f: Bool }\n\n");
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

fn input(rng: &mut Rng, k: K) -> Json {
    match k {
        K::Int => json!(*rng.pick(&EDGES)),
        K::Bool => json!(rng.chance(50)),
        K::Text => json!(rng.pick(&TEXTS)),
        K::Money => {
            let m = rng.pick(&MONEY);
            json!({ "amount": m.trim_end_matches(" USD"), "currency": "USD" })
        }
        K::Rec => json!({ "a": *rng.pick(&EDGES), "t": rng.pick(&TEXTS), "f": rng.chance(50) }),
        K::List => json!([rng.below(5) as i64 - 1, *rng.pick(&EDGES), 2]),
    }
}

#[test]
fn generated_programs_behave_identically_on_both_engines() {
    let mut checked = 0;
    let mut skipped = 0;
    let mut outcomes = std::collections::BTreeMap::<String, usize>::new();
    let mut failures = Vec::new();
    // STAAL_GENERATED sets how many (the mutation checks use fewer, to run faster).
    let count: u64 = std::env::var("STAAL_GENERATED").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
    let mut why_skipped = std::collections::BTreeMap::<String, usize>::new();
    for seed in 0..count {
        let (src, params) = program(seed);
        let prog = match staalc::check_program(&src) {
            Ok(p) => p,
            Err(d) => {
                skipped += 1;
                *why_skipped.entry(format!("{} {}", d[0].code, d[0].msg.chars().take(40).collect::<String>())).or_default() += 1;
                continue;
            }
        };
        let compiled = staalc::native::eligible(&prog);
        if compiled.len() != prog.fns.len() {
            failures.push(format!("seed {seed}: only {:?} of {} functions compile\n{src}", compiled, prog.fns.len()));
            continue;
        }
        let mut rng = Rng(seed ^ 0xABCDEF);
        let inputs: serde_json::Map<String, Json> =
            params.iter().enumerate().map(|(j, k)| (format!("p{j}"), input(&mut rng, *k))).collect();
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
        if failures.len() > 3 {
            break;
        }
    }
    assert!(failures.is_empty(), "{} programs differ:\n{}", failures.len(), failures.join("\n\n"));
    println!("{checked} generated programs identical on both engines ({skipped} did not pass the checker: {why_skipped:?}); outcomes {outcomes:?}");
    assert!(checked as u64 * 10 >= count * 9, "too few generated programs passed the checker: {checked} of {count}: {why_skipped:?}");
    for kind in ["ok", "value", "step_limit", "depth_limit", "action"] {
        assert!(outcomes.get(kind).copied().unwrap_or(0) * 100 >= count as usize, "too few runs ended {kind}: {outcomes:?}");
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
    for x in [1, 2, 3] {
        set t = t + x - x
    }
    let k = 0
    loop {
        set k = k + 1
        if k > 2 {
            break
        }
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
    // `try` keeps this function on the interpreter.
    let t = try work(1)
    let d = b + ", " + json(c)
    if t.ok {
        return d
    }
    return "?"
}
"#;
    let prog = staalc::check_program(src).unwrap();
    assert_eq!(staalc::native::eligible(&prog).into_iter().collect::<Vec<_>>(), ["work"]);
    let run = |steps: u64, depth: u64, engine: Engine| {
        let cfg = json!({ "entry": "go", "inputs": { "label": "total" },
                          "limits": { "max_steps": steps, "max_depth": depth },
                          "model": { "provider": "script", "responses": [] } });
        outcome(&run_config_engine(&prog, src, &cfg, Durability::None, engine))["result"].clone()
    };
    let full = run(1_000_000, 256, Engine::Interpreter);
    assert_eq!(full, json!({ "ok": "total: 36, 18" }));
    let mut limited = 0;
    for steps in 1..400 {
        let (i, n) = (run(steps, 256, Engine::Interpreter), run(steps, 256, Engine::Native));
        assert_eq!(i, n, "max_steps {steps}");
        if i["failed"] == "step_limit" {
            limited += 1;
        }
    }
    assert!(limited > 200, "the step limits tried did not reach the end of the program");
    for depth in 1..8 {
        assert_eq!(run(1_000_000, depth, Engine::Interpreter), run(1_000_000, depth, Engine::Native), "max_depth {depth}");
    }
}

/// Every failure native code reports, each at its edge, on both engines: the same
/// kind and the same message. Deterministic, so none depends on the generator's luck.
#[test]
fn every_failure_at_its_edge_is_identical() {
    let min = "(-9223372036854775807 - 1)";
    let cases: Vec<(&str, String)> = vec![
        ("add overflow", "9223372036854775807 + 1".into()),
        ("sub overflow", format!("{min} - 1")),
        ("mul overflow", "3037000500 * 3037000500".into()),
        ("div by zero", "7 / (3 - 3)".into()),
        ("MIN / -1", format!("{min} / (-1)")),
        ("negate MIN", format!("-({min})")),
        ("index past the end", "[1, 2, 3][3]".into()),
        ("negative index", "[1, 2, 3][-(1)]".into()),
        ("url_segment of ..", "len(url_segment(\"..\"))".into()),
        ("money below zero", "minor_units(1.00 USD - 2.00 USD)".into()),
        ("money overflow", "minor_units(90000000000000000.00 USD * 2)".into()),
        ("mixed currencies", "minor_units(1.00 USD + 1.00 EUR)".into()),
        ("fine", "len(\"abc\" + json([1, 2])) + minor_units(1.50 USD * 3)".into()),
    ];
    for (label, e) in cases {
        let src = format!("fn f(x: Int) -> Int {{\n    let v = {e}\n    return v + x\n}}\n");
        let prog = staalc::check_program(&src).unwrap_or_else(|d| panic!("{label}: {d:?}"));
        assert_eq!(staalc::native::eligible(&prog).len(), 1, "{label}: not compiled");
        let cfg = json!({ "entry": "f", "inputs": { "x": 0 }, "model": { "provider": "script", "responses": [] } });
        let i = outcome(&run_config_engine(&prog, &src, &cfg, Durability::None, Engine::Interpreter));
        let n = outcome(&run_config_engine(&prog, &src, &cfg, Durability::None, Engine::Native));
        assert_eq!(i["result"], n["result"], "{label}");
        if label == "fine" {
            assert!(i["result"].get("ok").is_some(), "{label}: {}", i["result"]);
        } else {
            assert_eq!(i["result"]["failed"], "value", "{label}: {}", i["result"]);
        }
    }
    // `fail` inside native code, and a step limit inside each kind of loop.
    let src = "fn f(x: Int) -> Int {\n    if x > 0 {\n        fail(\"too big: \" + json(x))\n    }\n    loop {\n        set x = x\n    }\n    return x\n}\n";
    let src = src.replace("set x = x", "let y = x");
    let prog = staalc::check_program(&src).unwrap();
    for (x, steps) in [(1, 1000), (0, 50), (0, 51)] {
        let cfg = json!({ "entry": "f", "inputs": { "x": x }, "limits": { "max_steps": steps },
                          "model": { "provider": "script", "responses": [] } });
        let i = outcome(&run_config_engine(&prog, &src, &cfg, Durability::None, Engine::Interpreter));
        let n = outcome(&run_config_engine(&prog, &src, &cfg, Durability::None, Engine::Native));
        assert_eq!(i["result"], n["result"], "x={x} steps={steps}");
    }
}
