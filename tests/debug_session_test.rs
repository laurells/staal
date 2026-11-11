// A debugging session as data (staalc::debug::Session) never carries a secret's value
// or a capability: a `Secret` is shown by its variable's name, a capability as one,
// and a secret value in any text is removed. Alone in its binary: it sets an
// environment variable, which no other test may be reading meanwhile.

use serde_json::json;
use staalc::debug::{Command, Event, Session, Shown};

const KEY: &str = "sk-debug-session-test-4f9a";

#[test]
fn a_session_shows_no_secret_value_and_no_capability() {
    // SAFETY: the only test in this binary, before it starts any thread.
    unsafe { std::env::set_var("STAAL_DEBUG_SESSION_TEST_KEY", KEY) };
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("debug-session-secrets");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.staal");
    std::fs::write(
        &file,
        "type Name { name: Text }\n\nfn main(key: Secret, http: cap net.http.get) -> Int {\n    let n = validate propose(Name, tokens: 100)\n    return 1\n}\n",
    )
    .unwrap();
    let checked = staalc::check_path(&file).unwrap();
    let files = checked.files.clone();
    let prog = std::sync::Arc::new(checked.result.unwrap_or_else(|e| panic!("{e:?}")));
    let config = json!({
        "entry": "main",
        "grants": { "http": { "cap": "net.http.get", "only": ["https://example.com/*"] } },
        "secrets": { "key": { "env": "STAAL_DEBUG_SESSION_TEST_KEY" } },
        // The model echoes the key: a text in the program now holds it.
        "model": { "provider": "script", "responses": [{ "output": { "name": format!("leak {KEY} here") }, "output_tokens": 3 }] }
    });
    let mut s = Session::start(prog, files, config);
    assert!(matches!(s.wait().unwrap(), Event::Stopped(_)));
    s.command(Command::Break { file: None, line: 5 }).unwrap();
    s.command(Command::Continue).unwrap();
    let Event::Stopped(stop) = s.wait().unwrap() else { panic!("no stop at line 5") };
    assert_eq!((stop.frames[0].line, stop.source.as_str()), (5, "return 1"));
    let locals: std::collections::BTreeMap<&str, &Shown> = stop.locals.iter().map(|(n, v)| (n.as_str(), v)).collect();
    assert_eq!(locals["key"], &Shown::Secret { name: "STAAL_DEBUG_SESSION_TEST_KEY".into() });
    assert_eq!(locals["http"], &Shown::Capability);
    assert_eq!(
        locals["n"],
        &Shown::Record { ty: "Name".into(), fields: vec![("name".into(), Shown::Text("leak [secret removed] here".into()))] }
    );
    assert_eq!(locals["key"].describe(), json!({ "kind": "secret", "name": "STAAL_DEBUG_SESSION_TEST_KEY" }));
    assert_eq!(locals["http"].describe(), json!({ "kind": "capability" }));
    // Nowhere in the stop, in either form.
    let everything = format!("{stop:?} {}", stop.locals.iter().map(|(_, v)| format!("{} {}", v.to_json(), v.describe())).collect::<String>());
    assert!(!everything.contains(KEY), "{everything}");
    s.command(Command::Continue).unwrap();
    let Event::Ended(Ok(r)) = s.wait().unwrap() else { panic!("the run did not end") };
    assert_eq!(r.result.unwrap(), json!(1));
}
