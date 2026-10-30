// `staalc schema` and runconfig::interface: the JSON Schemas of an entry function's
// inputs and result, for describing a program as a tool. Every kind of type an entry
// can take or return is checked, as are the parameters a caller can never give.

use serde_json::{json, Value as Json};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_staalc");

fn interface(src: &str, entry: Option<&str>) -> Result<Json, String> {
    let prog = staalc::check_program(src).unwrap_or_else(|e| panic!("{e:?}"));
    staalc::runconfig::interface(&prog, entry)
}

const MONEY_CODES: [&str; 10] = ["USD", "EUR", "GBP", "NGN", "GHS", "KES", "ZAR", "CAD", "AUD", "JPY"];

#[test]
fn every_input_type_is_described_exactly() {
    let src = r#"
type Customer { name: Text, email: Optional<Text> }
enum Ask { Refund { amount: Money }, Close }
fn main(name: Text, count: Int, urgent: Bool, total: Money, who: Customer, ask: Ask,
        tags: List<Text, 5>, ids: Set<Int, 3>, scores: Map<Int, Bool, 4>, notes: Map<Text, Text, 2>,
        pair: (Text, Int), maybe: Optional<Int>,
        pay: cap payments.refund(each: 50 USD), key: Secret, retries: Int = 2) -> Text {
    return name
}
"#;
    let i = interface(src, None).unwrap();
    assert_eq!(i["entry"], "main");
    assert_eq!(i["grants"], json!([{ "name": "pay", "cap": "payments.refund" }]));
    assert_eq!(i["secrets"], json!(["key"]));
    let input = &i["input"];
    assert_eq!(input["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(input["type"], "object");
    assert_eq!(input["additionalProperties"], false);
    // A parameter with a default may be left out; grants and secrets are not inputs.
    assert_eq!(
        input["required"],
        json!(["name", "count", "urgent", "total", "who", "ask", "tags", "ids", "scores", "notes", "pair", "maybe"])
    );
    let p = &input["properties"];
    assert!(p.get("pay").is_none() && p.get("key").is_none());
    assert_eq!(p["name"], json!({ "type": "string" }));
    assert_eq!(p["count"], json!({ "type": "integer", "minimum": i64::MIN, "maximum": i64::MAX }));
    assert_eq!(p["retries"], p["count"]);
    assert_eq!(p["urgent"], json!({ "type": "boolean" }));
    // One alternative per number of decimal places: JPY has none, the rest two.
    let money = p["total"]["anyOf"].as_array().unwrap();
    assert_eq!(money.len(), 2);
    assert_eq!(money[0]["properties"]["amount"]["pattern"], "^[0-9]+$");
    assert_eq!(money[0]["properties"]["currency"]["enum"], json!(["JPY"]));
    assert_eq!(money[1]["properties"]["amount"]["pattern"], "^[0-9]+(\\.[0-9]{1,2})?$");
    let mut two: Vec<&str> = MONEY_CODES.iter().copied().filter(|c| *c != "JPY").collect();
    two.sort_unstable();
    let mut got: Vec<&str> = money[1]["properties"]["currency"]["enum"].as_array().unwrap().iter().map(|c| c.as_str().unwrap()).collect();
    got.sort_unstable();
    assert_eq!(got, two);
    for m in money {
        assert_eq!(m["required"], json!(["amount", "currency"]));
        assert_eq!(m["additionalProperties"], false);
    }
    assert_eq!(p["who"], json!({ "$ref": "#/$defs/Customer" }));
    assert_eq!(p["ask"], json!({ "$ref": "#/$defs/Ask" }));
    assert_eq!(p["tags"], json!({ "type": "array", "items": { "type": "string" }, "maxItems": 5 }));
    assert_eq!(p["ids"]["uniqueItems"], true);
    assert_eq!(p["ids"]["maxItems"], 3);
    assert_eq!(p["scores"]["maxProperties"], 4);
    assert_eq!(p["scores"]["additionalProperties"], json!({ "type": "boolean" }));
    // Int keys are written as whole numbers in decimal; Text keys are anything.
    assert_eq!(p["scores"]["propertyNames"], json!({ "pattern": "^-?(0|[1-9][0-9]*)$" }));
    assert!(p["notes"].get("propertyNames").is_none());
    assert_eq!(p["pair"]["prefixItems"], json!([{ "type": "string" }, p["count"]]));
    assert_eq!(p["pair"]["items"], false);
    assert_eq!(p["pair"]["minItems"], 2);
    assert_eq!(p["pair"]["maxItems"], 2);
    assert_eq!(p["maybe"], json!({ "anyOf": [p["count"], { "type": "null" }] }));
    let defs = &input["$defs"];
    assert_eq!(
        defs["Customer"],
        json!({ "type": "object", "properties": { "name": { "type": "string" }, "email": { "anyOf": [{ "type": "string" }, { "type": "null" }] } },
                "required": ["name", "email"], "additionalProperties": false })
    );
    let options = defs["Ask"]["anyOf"].as_array().unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options[0]["properties"]["kind"], json!({ "const": "Refund" }));
    assert_eq!(options[0]["required"], json!(["kind", "amount"]));
    assert_eq!(options[0]["properties"]["amount"], p["total"]);
    assert_eq!(options[1], json!({ "type": "object", "properties": { "kind": { "const": "Close" } }, "required": ["kind"], "additionalProperties": false }));
}

#[test]
fn a_type_that_refers_to_itself_is_described_by_reference() {
    let src = r#"
type Task { name: Text, subtasks: List<Task, 4> }
fn main(task: Task) -> Int { return 1 }
"#;
    let i = interface(src, None).unwrap();
    let task = &i["input"]["$defs"]["Task"];
    assert_eq!(task["properties"]["subtasks"], json!({ "type": "array", "items": { "$ref": "#/$defs/Task" }, "maxItems": 4 }));
}

#[test]
fn results_are_described_as_runs_write_them() {
    let text = interface("fn main() -> Text { return \"x\" }", None).unwrap();
    assert_eq!(text["output"], json!({ "type": "string", "$schema": "https://json-schema.org/draft/2020-12/schema" }));
    assert_eq!(interface("fn main() { return }", None).unwrap()["output"]["type"], "null");
    let receipt = interface(
        "fn main(lock: cap building.lock.open(uses: 1)) -> Receipt { return lock.open(\"north-door\") }",
        None,
    )
    .unwrap();
    assert_eq!(receipt["output"]["required"], json!(["receipt", "capability", "method"]));
    let done = interface(
        "fn main(lock: cap building.lock.open(uses: 1)) -> Done {\n    let r = lock.open(\"north-door\")\n    return done(evidence: [r])\n}",
        None,
    )
    .unwrap();
    assert_eq!(done["output"]["required"], json!(["done"]));
    let record = interface("type Total { sum: Money }\nfn main() -> Total { return Total { sum: 1.00 USD } }", None).unwrap();
    assert_eq!(record["output"]["$ref"], "#/$defs/Total");
    assert_eq!(record["output"]["$defs"]["Total"]["required"], json!(["sum"]));
}

#[test]
fn the_entry_is_named_or_main() {
    let src = "fn main() -> Int { return 1 }\nfn other(n: Int) -> Int { return n }";
    assert_eq!(interface(src, Some("other")).unwrap()["input"]["required"], json!(["n"]));
    assert!(interface(src, Some("nope")).unwrap_err().contains("no function 'nope'"));
}

#[test]
fn the_command_prints_the_interface_and_refuses_bad_programs() {
    let dir = std::env::temp_dir().join(format!("staal-schema-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good.staal");
    std::fs::write(&good, "fn greet(name: Text) -> Text { return \"Hello \" + name }\n").unwrap();
    let out = Command::new(BIN).args(["schema", good.to_str().unwrap(), "--entry", "greet"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let i: Json = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(i["input"]["properties"]["name"], json!({ "type": "string" }));
    let bad = dir.join("bad.staal");
    std::fs::write(&bad, "fn main() -> Int { return \"x\" }\n").unwrap();
    let out = Command::new(BIN).args(["schema", bad.to_str().unwrap()]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}
