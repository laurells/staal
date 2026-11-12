// An observer is given events after secrets are removed: a secret's value that reached a
// text the program acts on is in no event it receives. Alone in its binary: it sets an
// environment variable, which no other test may be reading meanwhile.

use serde_json::json;
use staalc::interp::observe::{End, Observation, Observed, Observer};
use staalc::interp::{Durability, Engine, run_config_observed};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

const KEY: &str = "sk-observer-test-7c21e9";

struct Collect(Arc<Mutex<Vec<Observed>>>, Sender<End>);

impl Observer for Collect {
    fn event(&mut self, e: Observed) -> Result<(), String> {
        self.0.lock().unwrap().push(e);
        Ok(())
    }
    fn end(&mut self, end: End) {
        let _ = self.1.send(end);
    }
}

#[test]
fn an_observer_never_receives_a_secret_value() {
    // SAFETY: the only test in this binary, before it starts any thread.
    unsafe { std::env::set_var("STAAL_OBSERVER_TEST_KEY", KEY) };
    let src = "type Note { text: Text }\n\nfn main(key: Secret, mail: cap email.send) -> Int {\n    let n = validate propose(Note, tokens: 100)\n    let r = mail.send(n.text)\n    return 1\n}\n";
    let prog = staalc::check_program(src).unwrap();
    let config = json!({
        "entry": "main",
        "grants": { "mail": { "cap": "email.send" } },
        "secrets": { "key": { "env": "STAAL_OBSERVER_TEST_KEY" } },
        // The model echoes the key, and the program sends what it said.
        "model": { "provider": "script", "responses": [{ "output": { "text": format!("leak {KEY} here") }, "output_tokens": 3 }] }
    });
    let engines = if cfg!(feature = "native") { vec![Engine::Interpreter, Engine::Native] } else { vec![Engine::Interpreter] };
    for engine in engines {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let o = Observation { observer: Box::new(Collect(seen.clone(), tx)), capacity: 1024 };
        let r = run_config_observed(&prog, src, &config, Durability::None, engine, o).unwrap();
        assert_eq!(r.result.unwrap(), json!(1));
        rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap();
        let seen = seen.lock().unwrap();
        let all = format!("{seen:?}");
        assert!(!all.contains(KEY), "{engine:?}: {all}");
        // The text was there, with the secret removed: the scrubbed events were observed.
        assert!(seen.iter().any(|e| e.event.to_string().contains("leak [secret removed] here")), "{engine:?}: {all}");
    }
}
