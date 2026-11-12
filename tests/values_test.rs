// A run's values (RUNTIME.md section 21): given to a run, they are the only ones it reads,
// never the process's environment, for its secrets, its model API key and its proxy
// variables; two runs at once with different values for one name each see only their own;
// and they are dropped when the run ends, however it ends. Alone in its binary: it sets
// environment variables, which no other test may be reading meanwhile.

use serde_json::{Value as Json, json};
use staalc::interp::adapter::{Adapter, SimulatedAdapter};
use staalc::interp::money::Money;
use staalc::interp::value::Value;
use staalc::interp::values::{Given, Values};
use staalc::interp::{Attached, Durability, Engine, run_attached, run_config_attached};
use std::sync::Arc;
use zeroize::Zeroizing;

const PROCESS_KEY: &str = "sk-process-environment-91d2";

fn given(pairs: &[(&str, &str)]) -> Arc<dyn Values> {
    Arc::new(Given::new(pairs.iter().map(|(k, v)| (k.to_string(), Zeroizing::new(v.to_string())))))
}

fn engines() -> Vec<Engine> {
    if cfg!(feature = "native") { vec![Engine::Interpreter, Engine::Native] } else { vec![Engine::Interpreter] }
}

/// A secret, and a model that echoes what it is told to: the log shows which values the
/// run held, because it removes exactly those.
const ECHO: &str = "type Note { text: Text }\n\nfn main(key: Secret) -> Text {\n    let n = validate propose(Note, tokens: 100)\n    return n.text\n}\n";

fn echo_config(text: &str) -> Json {
    json!({
        "entry": "main",
        "secrets": { "key": { "env": "STAAL_VALUES_TEST_KEY" } },
        "model": { "provider": "script", "responses": [{ "output": { "text": text }, "output_tokens": 3 }] }
    })
}

#[test]
fn a_run_reads_only_the_values_it_is_given() {
    // SAFETY: the only test in this binary, before it starts any thread.
    unsafe {
        std::env::set_var("STAAL_VALUES_TEST_KEY", PROCESS_KEY);
        std::env::set_var("STAAL_VALUES_TEST_API_KEY", PROCESS_KEY);
        std::env::set_var("HTTPS_PROXY", "http://process.example:3128");
    }
    let prog = staalc::check_program(ECHO).unwrap();
    let (a, b) = ("sk-given-to-run-a-3f81", "sk-given-to-run-b-6c07");
    let text = format!("{a} {b} {PROCESS_KEY}");

    // Not given: refused, though the process's environment has it.
    let e = run_config_attached(&prog, ECHO, &echo_config(&text), Durability::None, Engine::Interpreter, Attached { values: Some(given(&[])), ..Attached::default() })
        .unwrap_err();
    assert!(e.contains("secrets.key: no value given to this run STAAL_VALUES_TEST_KEY is set"), "{e}");

    // Two runs at once, one name, two values: each removes its own, and only its own.
    for engine in engines() {
        let runs: Vec<_> = [a, b]
            .into_iter()
            .map(|v| {
                let (prog, text) = (prog.clone(), text.clone());
                std::thread::spawn(move || {
                    let values = given(&[("STAAL_VALUES_TEST_KEY", v)]);
                    run_config_attached(&prog, ECHO, &echo_config(&text), Durability::None, engine, Attached { values: Some(values), ..Attached::default() }).unwrap()
                })
            })
            .collect();
        let results: Vec<_> = runs.into_iter().map(|h| h.join().unwrap()).collect();
        let shown = |r: &staalc::interp::RunResult| json!(r.log).to_string();
        let (ra, rb) = (shown(&results[0]), shown(&results[1]));
        assert!(!ra.contains(a) && ra.contains(b) && ra.contains(PROCESS_KEY), "{engine:?}: run a: {ra}");
        assert!(!rb.contains(b) && rb.contains(a) && rb.contains(PROCESS_KEY), "{engine:?}: run b: {rb}");
    }

    // The model's API key: given, or refused; never the process's.
    let model = |key_name: &str| {
        json!({ "entry": "main", "secrets": { "key": { "env": "STAAL_VALUES_TEST_KEY" } },
                "model": { "provider": "anthropic", "model": "claude-sonnet-5-5", "api_key": { "env": key_name } } })
    };
    let src_no_model = "fn main(key: Secret) -> Int {\n    return 1\n}\n";
    let p2 = staalc::check_program(src_no_model).unwrap();
    let only_secret = given(&[("STAAL_VALUES_TEST_KEY", a)]);
    let e = run_config_attached(&p2, src_no_model, &model("STAAL_VALUES_TEST_API_KEY"), Durability::None, Engine::Interpreter, Attached { values: Some(only_secret), ..Attached::default() })
        .unwrap_err();
    assert_eq!(e, "model.api_key: no value given to this run STAAL_VALUES_TEST_API_KEY is set");
    let with_key = given(&[("STAAL_VALUES_TEST_KEY", a), ("STAAL_VALUES_TEST_API_KEY", "sk-ant-given-key")]);
    let r = run_config_attached(&p2, src_no_model, &model("STAAL_VALUES_TEST_API_KEY"), Durability::None, Engine::Interpreter, Attached { values: Some(with_key), ..Attached::default() })
        .unwrap();
    assert_eq!(r.result.unwrap(), json!(1));

    // The proxy variables: the given ones, or none; never the process's.
    let proxied = |values: Arc<dyn Values>, name: &str| {
        let mut cfg = echo_config("x");
        cfg["net"] = json!({ "proxy": "environment" });
        let j = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("values-proxy-{name}.journal"));
        let _ = std::fs::remove_file(&j);
        run_config_attached(&prog, ECHO, &cfg, Durability::Create(&j), Engine::Interpreter, Attached { values: Some(values), ..Attached::default() }).unwrap();
        let start: Json = serde_json::from_str(std::fs::read_to_string(&j).unwrap().lines().next().unwrap()).unwrap();
        start["proxies"]["net"].clone()
    };
    assert_eq!(proxied(given(&[("STAAL_VALUES_TEST_KEY", a)]), "none"), Json::Null);
    let net = proxied(given(&[("STAAL_VALUES_TEST_KEY", a), ("HTTPS_PROXY", "http://given.example:8080")]), "given");
    assert_eq!((net["host"].as_str(), net["port"].as_u64(), net["from"].as_str()), (Some("given.example"), Some(8080), Some("HTTPS_PROXY")));
    let mut cfg = echo_config("x");
    cfg["net"] = json!({ "proxy": "environment" });
    let e = run_config_attached(&prog, ECHO, &cfg, Durability::None, Engine::Interpreter, Attached { values: Some(given(&[("STAAL_VALUES_TEST_KEY", a), ("HTTPS_PROXY", "not a url")])), ..Attached::default() })
        .unwrap_err();
    assert!(e.contains("net.proxy: the value given to this run HTTPS_PROXY is not a proxy URL"), "{e}");
    // Without values given, the process's environment, as before.
    let r = run_config_attached(&prog, ECHO, &echo_config(&text), Durability::None, Engine::Interpreter, Attached::default()).unwrap();
    assert!(!json!(r.log).to_string().contains(PROCESS_KEY));

    values_are_dropped_when_the_run_ends(&prog);
}

/// An adapter whose every action panics: a bug in it ends the run's thread.
struct Panics;

impl Adapter for Panics {
    fn amount(&self, _c: &str, _m: &str, _a: &[Value]) -> Option<Money> {
        None
    }
    fn target(&self, _c: &str, _m: &str, _a: &[Value]) -> Option<String> {
        None
    }
    fn idempotent(&self, _c: &str, _m: &str) -> bool {
        true
    }
    fn perform(&mut self, _k: &str, _c: &str, _m: &str, _a: &[Json]) -> Result<String, String> {
        panic!("the adapter broke")
    }
    fn fork(&self) -> Box<dyn Adapter + Send> {
        Box::new(Panics)
    }
}

/// Whatever ends the run, its values are dropped by the time it returns: the caller's
/// is the only other hold, and once that is gone, nothing holds them.
fn values_are_dropped_when_the_run_ends(prog: &staalc::ast::Program) {
    let send = "fn main(key: Secret, mail: cap email.send) -> Int {\n    let r = mail.send(\"+2348000000001\")\n    return 1\n}\n";
    let ps = staalc::check_program(send).unwrap();
    let fails = "fn main(key: Secret) -> Int {\n    return 1 / 0\n}\n";
    let pf = staalc::check_program(fails).unwrap();
    let cfg = json!({ "entry": "main", "secrets": { "key": { "env": "STAAL_VALUES_TEST_KEY" } }, "grants": { "mail": { "cap": "email.send" } } });
    let cfg_f = json!({ "entry": "main", "secrets": { "key": { "env": "STAAL_VALUES_TEST_KEY" } } });
    let cases: Vec<(&str, Box<dyn Fn(Arc<dyn Values>) -> Result<(), String>>)> = vec![
        ("a result", Box::new(|v| {
            let r = run_config_attached(&ps, send, &cfg, Durability::None, Engine::Interpreter, Attached { values: Some(v), ..Attached::default() })?;
            assert!(r.result.is_ok());
            Ok(())
        })),
        ("a failure", Box::new(|v| {
            let r = run_config_attached(&pf, fails, &cfg_f, Durability::None, Engine::Interpreter, Attached { values: Some(v), ..Attached::default() })?;
            assert!(r.result.is_err());
            Ok(())
        })),
        ("a refused config", Box::new(|v| {
            let mut bad = cfg.clone();
            bad["grants"]["mail"]["cap"] = json!("email.nothing");
            assert!(run_config_attached(&ps, send, &bad, Durability::None, Engine::Interpreter, Attached { values: Some(v), ..Attached::default() }).is_err());
            Ok(())
        })),
        ("a panic", Box::new(|v| {
            let provider = staalc::interp::provider::of_run(None, &staalc::interp::values::ProcessEnvironment)?;
            let e = run_attached(&ps, send, &cfg, Durability::None, provider, Box::new(Panics), Engine::Interpreter, Attached { values: Some(v), ..Attached::default() })
                .unwrap_err();
            assert!(e.contains("crashed"), "{e}");
            Ok(())
        })),
    ];
    let _ = (prog, SimulatedAdapter::from_config(None));
    for (how, run) in cases {
        let values = given(&[("STAAL_VALUES_TEST_KEY", "sk-dropped-at-the-end-55a0")]);
        let weak = Arc::downgrade(&values);
        run(values).unwrap();
        assert!(weak.upgrade().is_none(), "the run's values outlived a run ended by {how}");
    }
}
