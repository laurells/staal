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
    for f in ["refund.staal", "staal.json"] {
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
        let o = Command::new(BIN).args(["agent", "check", d.to_str().unwrap()]).output().unwrap();
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        match want["staalc"].as_str() {
            None if o.status.code() == Some(0) => {}
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
