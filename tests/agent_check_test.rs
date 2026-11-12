// `staalc agent check` and docs/schemas/agent.schema.json against the fixtures of
// tests/fixtures/agent/manifests/ (AGENT_MANIFEST.md section 16, R8), written by
// scripts/agent_fixtures.py: the valid agent of tests/fixtures/agent/valid/refunds/
// with one change each, and expected.json, which says whether the schema accepts each
// and whether staalc does (and if not, part of its message).
//
// The schema is checked by an independent validator, Python's `jsonschema`, which this
// test runs only when STAAL_SCHEMA_PYTHON names a Python that has it; CI installs it
// (`pip install -r tests/schemas/requirements.txt`). staalc is checked always.

use serde_json::Value as Json;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");
const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn fixtures() -> Vec<(String, Json)> {
    let e: Json = serde_json::from_str(&std::fs::read_to_string(Path::new(ROOT).join("tests/fixtures/agent/manifests/expected.json")).unwrap()).unwrap();
    let cases = e["cases"].as_object().unwrap();
    assert!(cases.len() >= 80, "too few cases: {}", cases.len());
    cases.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// A copy of the valid agent's project with `manifest` as its agent.json.
fn agent_dir(name: &str, manifest: &Path) -> PathBuf {
    let d = std::fs::canonicalize(env!("CARGO_TARGET_TMPDIR")).unwrap().join("agent-check").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let valid = Path::new(ROOT).join("tests/fixtures/agent/valid/refunds");
    for f in ["refund.staal", "staal.json", "agent.described.json"] {
        std::fs::copy(valid.join(f), d.join(f)).unwrap();
    }
    std::fs::copy(manifest, d.join("agent.json")).unwrap();
    d
}

#[test]
fn staalc_accepts_and_refuses_each_fixture_as_marked() {
    let mut failures = Vec::new();
    for (name, want) in fixtures() {
        let file = Path::new(ROOT).join(format!("tests/fixtures/agent/manifests/{name}.json"));
        let d = agent_dir(&name, &file);
        // `agent describe` runs every check of `agent check` but the comparison with the
        // committed description, which it writes; `agent check` must then accept it.
        let o = Command::new(BIN).args(["agent", "describe", d.to_str().unwrap()]).output().unwrap();
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        match want["staalc"].as_str() {
            None if o.status.code() == Some(0) => {
                let c = Command::new(BIN).args(["agent", "check", d.to_str().unwrap()]).output().unwrap();
                if !c.status.success() {
                    failures.push(format!("{name}: described, then refused by check: {}", String::from_utf8_lossy(&c.stderr)));
                }
            }
            None => failures.push(format!("{name}: refused, but should be accepted: {err}")),
            Some(part) if o.status.code() == Some(1) && err.contains(part) => {}
            Some(part) => failures.push(format!("{name}: exit {:?}, expected a refusal containing {part:?}: {err}", o.status.code())),
        }
        // Nothing the schema refuses may staalc accept.
        if want["schema"] == false && want["staalc"].is_null() {
            failures.push(format!("{name}: marked refused by the schema and accepted by staalc"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_schema_accepts_and_refuses_each_fixture_as_marked() {
    let Ok(python) = std::env::var("STAAL_SCHEMA_PYTHON") else {
        eprintln!("skipped: set STAAL_SCHEMA_PYTHON to a Python with tests/schemas/requirements.txt installed");
        return;
    };
    let script = r#"
import json, sys, jsonschema
schema = json.load(open(sys.argv[1]))
jsonschema.Draft202012Validator.check_schema(schema)
v = jsonschema.Draft202012Validator(schema)
out = {}
for path in sys.argv[2:]:
    out[path] = not any(True for _ in v.iter_errors(json.load(open(path))))
print(json.dumps(out))
"#;
    let cases = fixtures();
    let files: Vec<String> = cases.iter().map(|(n, _)| Path::new(ROOT).join(format!("tests/fixtures/agent/manifests/{n}.json")).to_string_lossy().into_owned()).collect();
    let o = Command::new(&python)
        .arg("-I")
        .arg("-c")
        .arg(script)
        .arg(Path::new(ROOT).join("docs/schemas/agent.schema.json"))
        .args(&files)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let got: Json = serde_json::from_slice(&o.stdout).unwrap();
    let mut failures = Vec::new();
    for ((name, want), file) in cases.iter().zip(&files) {
        if got[file] != want["schema"] {
            failures.push(format!("{name}: the schema {} it", if got[file] == true { "accepts" } else { "refuses" }));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The schema's currencies and their decimal places are the runtime's: the schema
/// cannot fall behind the table money is read with.
#[test]
fn the_schemas_money_is_the_runtimes() {
    let s: Json = serde_json::from_str(&std::fs::read_to_string(Path::new(ROOT).join("docs/schemas/agent.schema.json")).unwrap()).unwrap();
    let m = &s["$defs"]["money"];
    let mut listed: Vec<String> = m["properties"]["currency"]["enum"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    let zero: Vec<String> = m["if"]["properties"]["currency"]["enum"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    let mut runtime: Vec<String> = staalc::interp::money::Currency::all().iter().map(|(c, _)| c.to_string()).collect();
    listed.sort();
    runtime.sort();
    assert_eq!(listed, runtime);
    for (c, e) in staalc::interp::money::Currency::all() {
        assert!(*e == 0 || *e == 2, "{c} has {e} decimal places, which the schema does not describe");
        assert_eq!(zero.contains(&c.to_string()), *e == 0, "{c}");
    }
    assert_eq!(m["then"]["properties"]["amount"]["pattern"], "^[0-9]+$");
    assert_eq!(m["else"]["properties"]["amount"]["pattern"], "^[0-9]+(\\.[0-9]{1,2})?$");
    // The categories are staalc's.
    let cats: Vec<&str> = s["properties"]["categories"]["items"]["enum"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    assert_eq!(cats, staalc::agent::CATEGORIES.to_vec());
}

fn described_fixtures() -> Vec<PathBuf> {
    let dir = Path::new(ROOT).join("tests/fixtures/agent/described");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    out.sort();
    assert!(out.len() >= 15, "too few described fixtures");
    out
}

/// The committed description of the valid agent is what `staalc agent describe`
/// writes, byte for byte, and `staalc agent check` refuses any other: each fixture of
/// tests/fixtures/agent/described/ is the description with one change.
#[test]
fn a_description_must_be_exactly_the_fresh_one() {
    let valid = Path::new(ROOT).join("tests/fixtures/agent/valid/refunds");
    let d = agent_dir("described-fresh", &valid.join("agent.json"));
    let o = Command::new(BIN).args(["agent", "check", d.to_str().unwrap()]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = Command::new(BIN).args(["agent", "describe", d.to_str().unwrap()]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read(d.join("agent.described.json")).unwrap(), std::fs::read(valid.join("agent.described.json")).unwrap());
    for f in described_fixtures() {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let d = agent_dir(&format!("described-{name}"), &valid.join("agent.json"));
        std::fs::copy(&f, d.join("agent.described.json")).unwrap();
        let o = Command::new(BIN).args(["agent", "check", d.to_str().unwrap()]).output().unwrap();
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.code() == Some(1) && err.contains("differs from a fresh description"), "{name}: {err}");
    }
}

/// docs/schemas/agent.described.schema.json accepts the description staalc writes and
/// refuses each fixture of tests/fixtures/agent/described/.
#[test]
fn the_described_schema_accepts_staalcs_description_and_refuses_each_fixture() {
    let Ok(python) = std::env::var("STAAL_SCHEMA_PYTHON") else {
        eprintln!("skipped: set STAAL_SCHEMA_PYTHON to a Python with tests/schemas/requirements.txt installed");
        return;
    };
    let script = r#"
import json, sys, jsonschema
schema = json.load(open(sys.argv[1]))
jsonschema.Draft202012Validator.check_schema(schema)
v = jsonschema.Draft202012Validator(schema)
print(json.dumps({p: not any(True for _ in v.iter_errors(json.load(open(p)))) for p in sys.argv[2:]}))
"#;
    let valid = Path::new(ROOT).join("tests/fixtures/agent/valid/refunds/agent.described.json");
    let mut files = vec![valid.to_string_lossy().into_owned()];
    files.extend(described_fixtures().iter().map(|p| p.to_string_lossy().into_owned()));
    let o = Command::new(&python)
        .arg("-I")
        .arg("-c")
        .arg(script)
        .arg(Path::new(ROOT).join("docs/schemas/agent.described.schema.json"))
        .args(&files)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let got: Json = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(got[&files[0]], true, "the schema refuses staalc's own description");
    for f in &files[1..] {
        assert_eq!(got[f], false, "{f}: the schema accepts it");
    }
}

fn staalc_agent(cmd: &str, d: &Path) -> (Option<i32>, String) {
    let o = Command::new(BIN).args(["agent", cmd, d.to_str().unwrap()]).output().unwrap();
    (o.status.code(), String::from_utf8_lossy(&o.stderr).into_owned())
}

/// Files that are not JSON as an agent's files must be: a name twice in one object (at
/// any depth, however escaped), a byte order mark, and text that is not one JSON value.
#[test]
fn malformed_json_is_refused() {
    let dir = Path::new(ROOT).join("tests/fixtures/agent/malformed");
    let e: Json = serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
    let cases = e["cases"].as_object().unwrap();
    assert!(cases.len() >= 8);
    for (name, part) in cases {
        let d = agent_dir(&format!("malformed-{name}"), &dir.join(format!("{name}.json")));
        let (code, err) = staalc_agent("check", &d);
        assert!(code == Some(1) && err.contains(part.as_str().unwrap()), "{name}: {err}");
    }
}

/// The order of an object's members does not matter: the same agent.json with every
/// object's members reversed gives the same description, byte for byte.
#[test]
fn member_order_does_not_change_the_description() {
    /// The JSON text of `v` with every object's members in reverse order (serde_json
    /// keeps members sorted, so the text is written here).
    fn reversed(v: &Json) -> String {
        match v {
            Json::Object(o) => {
                let members: Vec<String> = o.iter().rev().map(|(k, x)| format!("{}: {}", Json::String(k.clone()), reversed(x))).collect();
                format!("{{{}}}", members.join(", "))
            }
            Json::Array(a) => format!("[{}]", a.iter().map(reversed).collect::<Vec<_>>().join(", ")),
            other => other.to_string(),
        }
    }
    let valid = Path::new(ROOT).join("tests/fixtures/agent/valid/refunds");
    let original = std::fs::read_to_string(valid.join("agent.json")).unwrap();
    let v: Json = serde_json::from_str(&original).unwrap();
    let text = reversed(&v);
    // serde_json writes members in name order; this text has them the other way.
    assert!(text.starts_with("{\"version\"") && serde_json::to_string(&v).unwrap().starts_with("{\"agent\""), "{text}");
    assert_eq!(serde_json::from_str::<Json>(&text).unwrap(), v);
    let d = agent_dir("reordered", &valid.join("agent.json"));
    std::fs::write(d.join("agent.json"), text).unwrap();
    let (code, err) = staalc_agent("check", &d);
    assert_eq!(code, Some(0), "{err}");
}

/// A request a crash can leave in doubt with nothing to tell whether it happened: without
/// `idempotency_key`, the refund's POST is an operator's to resolve, and the description
/// says its outcome can be unknown, as the rule every action decides with gives it.
#[test]
fn the_description_says_when_an_outcome_can_be_unknown() {
    let valid = Path::new(ROOT).join("tests/fixtures/agent/valid/refunds");
    let mut m: Json = serde_json::from_str(&std::fs::read_to_string(valid.join("agent.json")).unwrap()).unwrap();
    let mut found = 0;
    for t in m["tools"].as_array_mut().unwrap() {
        for g in t["grants"].as_object_mut().unwrap().values_mut() {
            if let Some(post) = g.pointer_mut("/needs/post").and_then(Json::as_object_mut) {
                found += usize::from(post.remove("idempotency_key").is_some());
            }
        }
    }
    assert_eq!(found, 1, "the fixture's keyed POST was not where this test expects it");
    let file = Path::new(env!("CARGO_TARGET_TMPDIR")).join("agent-no-key.json");
    std::fs::write(&file, m.to_string()).unwrap();
    let d = agent_dir("no-key", &file);
    let (code, err) = staalc_agent("describe", &d);
    assert_eq!(code, Some(0), "{err}");
    let described: Json = serde_json::from_str(&std::fs::read_to_string(d.join("agent.described.json")).unwrap()).unwrap();
    assert_eq!(
        described["tools"][0]["failure"],
        serde_json::json!([{ "cap": "net.http.post", "grant": "pay.post", "outcome_can_be_unknown": true, "recovery": "operator", "relies_on_service": false }])
    );
}

/// A tool's program must be a file in the agent's folder after every link is resolved:
/// a link to a file outside is refused; a link to a file inside is that file.
#[cfg(unix)]
#[test]
fn a_program_outside_the_folder_is_refused_through_a_link() {
    let valid = Path::new(ROOT).join("tests/fixtures/agent/valid/refunds");
    let d = agent_dir("link-outside", &valid.join("agent.json"));
    let outside = d.parent().unwrap().join("link-outside-elsewhere");
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::rename(d.join("refund.staal"), outside.join("refund.staal")).unwrap();
    std::os::unix::fs::symlink(outside.join("refund.staal"), d.join("refund.staal")).unwrap();
    let (code, err) = staalc_agent("check", &d);
    assert!(code == Some(1) && err.contains("is not a file in the agent's folder"), "{err}");
    // A link inside the folder to a file inside it.
    let d = agent_dir("link-inside", &valid.join("agent.json"));
    std::fs::rename(d.join("refund.staal"), d.join("real.staal")).unwrap();
    std::os::unix::fs::symlink(d.join("real.staal"), d.join("refund.staal")).unwrap();
    let (code, err) = staalc_agent("check", &d);
    assert_eq!(code, Some(0), "{err}");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit(dir: &Path, files: &[(&str, &str)]) -> String {
    std::fs::create_dir_all(dir).unwrap();
    if !dir.join(".git").exists() {
        git(dir, &["init", "-q"]);
    }
    for (f, t) in files {
        std::fs::write(dir.join(f), t).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c", "--allow-empty"]);
    git(dir, &["rev-parse", "HEAD"])
}

/// An agent with a package. Its description holds the package, what the tool hands it
/// and the network reach; each of these changes, made alone after the description was
/// written, makes the committed description stale, and `agent check` refuses it: the
/// package's contents (a new commit), the lockfile's text, the program's capability
/// graph with agent.json unchanged, and a reach in agent.json.
#[test]
fn a_description_goes_stale_with_every_change_that_matters() {
    let b = std::fs::canonicalize(env!("CARGO_TARGET_TMPDIR")).unwrap().join("agent-check").join("stale");
    let _ = std::fs::remove_dir_all(&b);
    std::fs::create_dir_all(&b).unwrap();
    let repo = b.join("repos/ledger");
    let any = "https://ledger.example.com/*";
    let ledger_manifest = format!(r#"{{"package": "ledger", "authority": {{"net.http.post": ["{any}"]}}}}"#);
    let ledger_code = format!("pub fn record(h: cap net.http.post(only: [\"{any}\"]), x: Text) -> Int {{\n    let r = h.post(\"https://ledger.example.com/entries\", x)\n    return r.status\n}}\n");
    let c1 = commit(&repo, &[("staal.json", &ledger_manifest), ("n.staal", &ledger_code)]);
    let d = b.join("agent");
    std::fs::create_dir_all(&d).unwrap();
    let staal_json = |c: &str| format!(r#"{{"dependencies": {{"ledger": {{"git": "{}", "commit": "{c}"}}}}}}"#, repo.display());
    std::fs::write(d.join("staal.json"), staal_json(&c1)).unwrap();
    let program = |extra: &str| {
        format!(
            "import ledger.n\n\nfn main(note: Text, post: cap net.http.post(only: [\"{any}\"])) -> Int {{\n    let r = post.post(\"https://ledger.example.com/notes\", note)\n{extra}    return r.status\n}}\n"
        )
    };
    std::fs::write(d.join("note.staal"), program("")).unwrap();
    let manifest = serde_json::json!({
        "manifest": "0.1", "agent": "ledger-notes", "version": "1.0.0", "requires_staalc": ">=0.27.0",
        "title": "Ledger notes", "summary": "Writes a note to the ledger.",
        "publisher": { "id": "github:example-org", "name": "Example", "security_contact": "https://example.com/security" },
        "license": "MIT", "categories": ["finance"],
        "tools": [{ "name": "note", "description": "Write a note.", "program": "note.staal",
                    "grants": { "post": { "reach": ["https://ledger.example.com/*"] } } }],
        "data": { "reads": [], "writes": ["a note"], "sends": ["the note to the ledger"], "categories": [], "processors": [],
                  "retention": "none", "model_training": "no model is called", "regions": "the ledger's" }
    });
    std::fs::write(d.join("agent.json"), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    staalc::packages::fetch(&d).unwrap();
    let describe = || {
        let (code, err) = staalc_agent("describe", &d);
        assert_eq!(code, Some(0), "{err}");
        std::fs::read_to_string(d.join("agent.described.json")).unwrap()
    };
    let stale = |what: &str| {
        let (code, err) = staalc_agent("check", &d);
        assert!(code == Some(1) && err.contains("differs from a fresh description"), "{what}: {err}");
    };
    let first = describe();
    let v: Json = serde_json::from_str(&first).unwrap();
    assert_eq!(v["dependencies"][0]["name"], "ledger");
    assert_eq!(v["tools"][0]["network"][0]["reach"], serde_json::json!(["https://ledger.example.com/*"]));
    assert_eq!(v["tools"][0]["package_boundaries"], serde_json::json!([{ "package": "ledger", "declared_authority": { "net.http.post": [any] }, "receives": [] }]));
    assert_eq!(staalc_agent("check", &d).0, Some(0));

    // The capability graph: the program now hands its grant to the package.
    std::fs::write(d.join("note.staal"), program("    let k = n.record(post, note)\n")).unwrap();
    stale("the capability graph");
    let v: Json = serde_json::from_str(&describe()).unwrap();
    assert_eq!(v["tools"][0]["package_boundaries"][0]["receives"][0]["function"], "ledger.n.record", "{v}");

    // The package's contents.
    let c2 = commit(&repo, &[("n.staal", &ledger_code.replace("entries", "lines"))]);
    std::fs::write(d.join("staal.json"), staal_json(&c2)).unwrap();
    staalc::packages::fetch(&d).unwrap();
    stale("the package's contents");
    describe();

    // The lockfile's text alone.
    let lock: Json = serde_json::from_str(&std::fs::read_to_string(d.join("staal.lock")).unwrap()).unwrap();
    std::fs::write(d.join("staal.lock"), lock.to_string()).unwrap();
    stale("the lockfile's text");
    describe();

    // A reach in agent.json.
    let mut m2 = manifest.clone();
    m2["tools"][0]["grants"]["post"]["reach"] = serde_json::json!(["https://ledger.example.com/notes"]);
    std::fs::write(d.join("agent.json"), serde_json::to_string_pretty(&m2).unwrap()).unwrap();
    stale("a reach");
    describe();
    assert_eq!(staalc_agent("check", &d).0, Some(0));
}

/// Read through a `Root`, the agent check gives what it gives by path, word for word, on
/// every fixture: the same refusals, the same description, the same answer about the
/// committed one, fresh or stale.
#[cfg(unix)]
#[test]
fn the_check_through_a_root_is_the_check_by_path() {
    use staalc::agent;
    let mut differ = Vec::new();
    for (name, _) in fixtures() {
        let file = Path::new(ROOT).join(format!("tests/fixtures/agent/manifests/{name}.json"));
        let d = agent_dir(&format!("in-root-{name}"), &file);
        let root = staalc::interp::fsx::Root::open(&d).unwrap();
        let msg = |r: Result<agent::Checked, String>| r.map(|c| c.programs.iter().map(|p| p.fingerprint.clone()).collect::<Vec<_>>());
        let (by_path, in_root) = (msg(agent::check_manifest(&d)), msg(agent::check_manifest_in(&root, &d)));
        if by_path != in_root {
            differ.push(format!("{name}: manifest by path {by_path:?}, through a root {in_root:?}"));
            continue;
        }
        if let Ok(c) = agent::check_manifest(&d) {
            let (a, b) = (agent::describe::bytes(&d, &c), agent::describe::bytes_in(&root, &d, &c));
            if a != b {
                differ.push(format!("{name}: description by path {a:?}, through a root {b:?}"));
            }
        }
        // The committed description, as copied (fresh for the valid agent, stale or
        // refused for most), then written fresh.
        for round in ["as copied", "described"] {
            if round == "described" && agent::write_description(&d).is_err() {
                continue;
            }
            let (a, b) = (msg(agent::check(&d)), msg(agent::check_in(&root, &d)));
            if a != b {
                differ.push(format!("{name}, {round}: check by path {a:?}, through a root {b:?}"));
            }
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
    // A tool's program reached through a link: by path it is resolved and must be in the
    // folder; through a root it is never followed.
    let d = agent_dir("in-root-link", &Path::new(ROOT).join("tests/fixtures/agent/valid/refunds/agent.json"));
    std::fs::rename(d.join("refund.staal"), d.join("real.staal")).unwrap();
    std::os::unix::fs::symlink(d.join("real.staal"), d.join("refund.staal")).unwrap();
    let root = staalc::interp::fsx::Root::open(&d).unwrap();
    let e = agent::check_manifest_in(&root, &d).err().expect("a program through a link was read");
    assert!(e.contains("is not a file in the agent's folder"), "{e}");
    // The project's staal.json through a link: refused by the check and by the
    // description, each reading it through the root.
    let d = agent_dir("in-root-manifest-link", &Path::new(ROOT).join("tests/fixtures/agent/valid/refunds/agent.json"));
    let root = staalc::interp::fsx::Root::open(&d).unwrap();
    let c = agent::check_manifest_in(&root, &d).unwrap();
    std::fs::rename(d.join("staal.json"), d.join("real.json")).unwrap();
    std::os::unix::fs::symlink(d.join("real.json"), d.join("staal.json")).unwrap();
    assert!(agent::describe::describe(&d, &c).is_ok(), "by path, the link is followed");
    let e = agent::describe::describe_in(&root, &d, &c).unwrap_err();
    assert!(e.contains("symbolic link"), "{e}");
    let e = agent::check_manifest_in(&root, &d).err().expect("staal.json through a link was read");
    assert!(e.contains("symbolic link"), "{e}");
}
