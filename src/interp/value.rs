// Runtime values, and the strict boundary between them and JSON (RUNTIME.md
// sections 2 and 3). Everything that enters from outside, run inputs and model
// output alike, goes through `from_json`.

use super::money::{Currency, Money};
use crate::ast::Ty;
use serde_json::{json, Map, Value as Json};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct Receipt {
    pub id: String,
    pub capability: String,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Unit,
    Text(String),
    Int(i64),
    Bool(bool),
    Money(Money),
    Record { ty: String, fields: Vec<(String, Value)> },
    /// Raw model output, not yet checked against `ty`.
    Proposed { ty: Ty, raw: Json },
    Receipt(Receipt),
    /// `held` is the successful result a `try` produced (hidden from programs).
    Outcome { ok: bool, error: String, held: Option<Box<Value>> },
    Done(Vec<Value>),
    /// A handle into the capability broker.
    Cap(usize),
}

/// Declared record types: name -> fields in declaration order.
pub type Types = HashMap<String, Vec<(String, Ty)>>;

/// The deepest nesting accepted from outside, so hostile input cannot exhaust the stack.
const MAX_JSON_DEPTH: usize = 64;

/// Converts outside JSON into a value of type `ty`, strictly (RUNTIME.md section 3).
/// `path` names the location for error messages, e.g. "inputs.order.customer".
pub fn from_json(ty: &Ty, json: &Json, types: &Types, path: &str) -> Result<Value, String> {
    from_json_at(ty, json, types, path, 0)
}

fn from_json_at(ty: &Ty, json: &Json, types: &Types, path: &str, depth: usize) -> Result<Value, String> {
    if depth > MAX_JSON_DEPTH {
        return Err(format!("{path}: nested more than {MAX_JSON_DEPTH} levels deep"));
    }
    let expected = |what: &str| format!("{path}: expected {what}, found {}", describe(json));
    match ty {
        Ty::Proposed(inner) => Ok(Value::Proposed { ty: (**inner).clone(), raw: json.clone() }),
        Ty::Cap { .. } => Err(format!("{path}: a capability cannot be supplied as data")),
        Ty::Named(n) => match n.as_str() {
            "Text" => json.as_str().map(|s| Value::Text(s.to_string())).ok_or_else(|| expected("a string")),
            "Bool" => json.as_bool().map(Value::Bool).ok_or_else(|| expected("true or false")),
            "Int" => match json {
                Json::Number(num) => num.as_i64().map(Value::Int).ok_or_else(|| {
                    format!("{path}: expected a whole number between {} and {}, found {num}", i64::MIN, i64::MAX)
                }),
                _ => Err(expected("a whole number")),
            },
            "Money" => money_from_json(json, path).map(Value::Money),
            "Receipt" | "Outcome" | "Done" => {
                Err(format!("{path}: a {n} can only be created by the runtime, never supplied from outside"))
            }
            "Unit" => Err(format!("{path}: Unit cannot be supplied from outside")),
            _ => {
                let fields = types.get(n).ok_or_else(|| format!("{path}: unknown type {n}"))?;
                let obj = json.as_object().ok_or_else(|| expected(&format!("a {n} object")))?;
                for key in obj.keys() {
                    if !fields.iter().any(|(f, _)| f == key) {
                        return Err(format!("{path}: {n} has no field '{key}'"));
                    }
                }
                let mut out = Vec::with_capacity(fields.len());
                for (fname, fty) in fields {
                    let sub = format!("{path}.{fname}");
                    let fjson = obj.get(fname).ok_or_else(|| format!("{sub}: missing"))?;
                    out.push((fname.clone(), from_json_at(fty, fjson, types, &sub, depth + 1)?));
                }
                Ok(Value::Record { ty: n.clone(), fields: out })
            }
        },
    }
}

fn money_from_json(json: &Json, path: &str) -> Result<Money, String> {
    let obj = json
        .as_object()
        .ok_or_else(|| format!("{path}: expected money like {{\"amount\": \"12.50\", \"currency\": \"USD\"}}"))?;
    for key in obj.keys() {
        if key != "amount" && key != "currency" {
            return Err(format!("{path}: money has no field '{key}'"));
        }
    }
    let currency = obj
        .get("currency")
        .and_then(Json::as_str)
        .ok_or_else(|| format!("{path}.currency: expected a currency code string"))?;
    let currency = Currency::parse(currency).map_err(|e| format!("{path}.currency: {e}"))?;
    let amount = match obj.get("amount") {
        Some(Json::String(s)) => s,
        Some(Json::Number(_)) => {
            return Err(format!("{path}.amount: write money as a string like \"12.50\", not a number, to avoid rounding"))
        }
        _ => return Err(format!("{path}.amount: expected a decimal string like \"12.50\"")),
    };
    Money::parse(amount, currency).map_err(|e| format!("{path}.amount: {e}"))
}

fn describe(json: &Json) -> &'static str {
    match json {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "a list",
        Json::Object(_) => "an object",
    }
}

/// Encodes a value as JSON, for model data and for output. Capabilities are never
/// encoded as data: `for_model` makes that an error.
pub fn to_json(v: &Value, for_model: bool) -> Result<Json, String> {
    Ok(match v {
        Value::Unit => Json::Null,
        Value::Text(s) => Json::String(s.clone()),
        Value::Int(n) => json!(n),
        Value::Bool(b) => json!(b),
        Value::Money(m) => json!({ "amount": m.amount_string(), "currency": m.currency.code }),
        Value::Record { fields, .. } => {
            let mut obj = Map::new();
            for (k, fv) in fields {
                obj.insert(k.clone(), to_json(fv, for_model)?);
            }
            Json::Object(obj)
        }
        Value::Proposed { raw, .. } => raw.clone(),
        Value::Receipt(r) => json!({ "receipt": r.id, "capability": r.capability, "method": r.method }),
        Value::Outcome { ok, error, .. } => json!({ "ok": ok, "error": error }),
        Value::Done(items) => {
            json!({ "done": items.iter().map(|i| to_json(i, for_model)).collect::<Result<Vec<_>, _>>()? })
        }
        Value::Cap(_) if for_model => return Err("a capability cannot be sent to a model".into()),
        Value::Cap(h) => json!({ "capability_handle": h }),
    })
}

/// A JSON description of type `ty`, sent with each model request so the provider
/// can constrain its output. Recursive types are cut off with an unconstrained `{}`.
pub fn schema(ty: &Ty, types: &Types) -> Json {
    schema_at(ty, types, &mut HashSet::new())
}

fn schema_at(ty: &Ty, types: &Types, seen: &mut HashSet<String>) -> Json {
    match ty {
        Ty::Proposed(inner) => schema_at(inner, types, seen),
        Ty::Cap { .. } => json!({}),
        Ty::Named(n) => match n.as_str() {
            "Text" => json!({ "type": "string" }),
            "Int" => json!({ "type": "integer" }),
            "Bool" => json!({ "type": "boolean" }),
            "Money" => json!({
                "type": "object",
                "properties": {
                    "amount": { "type": "string", "description": "decimal amount, e.g. \"12.50\"" },
                    "currency": { "type": "string", "description": "ISO 4217 code, e.g. \"USD\"" }
                },
                "required": ["amount", "currency"],
                "additionalProperties": false
            }),
            _ => {
                let Some(fields) = types.get(n) else { return json!({}) };
                if !seen.insert(n.clone()) {
                    return json!({});
                }
                let mut props = Map::new();
                for (f, fty) in fields {
                    props.insert(f.clone(), schema_at(fty, types, seen));
                }
                seen.remove(n);
                let required: Vec<&String> = fields.iter().map(|(f, _)| f).collect();
                json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false })
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types() -> Types {
        let mut t = Types::new();
        t.insert("Customer".into(), vec![("name".into(), Ty::Named("Text".into()))]);
        t.insert(
            "Order".into(),
            vec![
                ("id".into(), Ty::Named("Text".into())),
                ("total".into(), Ty::Named("Money".into())),
                ("customer".into(), Ty::Named("Customer".into())),
            ],
        );
        t.insert("Node".into(), vec![("next".into(), Ty::Named("Node".into()))]);
        t
    }

    fn order_ty() -> Ty {
        Ty::Named("Order".into())
    }

    #[test]
    fn accepts_exact_records() {
        let j = json!({ "id": "A1", "total": { "amount": "9.99", "currency": "USD" }, "customer": { "name": "Ada" } });
        let v = from_json(&order_ty(), &j, &types(), "in").unwrap();
        assert_eq!(to_json(&v, true).unwrap(), j);
    }

    #[test]
    fn rejects_missing_extra_and_wrong_fields() {
        let t = types();
        let base = json!({ "id": "A1", "total": { "amount": "9.99", "currency": "USD" }, "customer": { "name": "Ada" } });
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove("id");
        assert!(from_json(&order_ty(), &missing, &t, "in").unwrap_err().contains("in.id: missing"));
        let mut extra = base.clone();
        extra.as_object_mut().unwrap().insert("admin".into(), json!(true));
        assert!(from_json(&order_ty(), &extra, &t, "in").unwrap_err().contains("no field 'admin'"));
        let mut wrong = base.clone();
        wrong["customer"]["name"] = json!(5);
        assert!(from_json(&order_ty(), &wrong, &t, "in").unwrap_err().contains("in.customer.name"));
        let mut float_money = base;
        float_money["total"]["amount"] = json!(9.99);
        assert!(from_json(&order_ty(), &float_money, &t, "in").unwrap_err().contains("not a number"));
    }

    #[test]
    fn never_accepts_runtime_only_values() {
        let t = types();
        for n in ["Receipt", "Outcome", "Done", "Unit"] {
            assert!(from_json(&Ty::Named(n.into()), &json!({ "receipt": "x" }), &t, "in").is_err());
        }
        assert!(from_json(&Ty::Cap { path: vec!["a".into()], max: None }, &json!({}), &t, "in").is_err());
    }

    #[test]
    fn ints_must_fit_and_be_whole() {
        let t = types();
        let int = Ty::Named("Int".into());
        assert_eq!(from_json(&int, &json!(-5), &t, "n").unwrap(), Value::Int(-5));
        assert!(from_json(&int, &json!(1.5), &t, "n").is_err());
        assert!(from_json(&int, &json!(u64::MAX), &t, "n").is_err());
        assert!(from_json(&int, &json!("5"), &t, "n").is_err());
    }

    #[test]
    fn deep_nesting_is_rejected_not_crashed() {
        let mut j = json!({ "next": null });
        for _ in 0..200 {
            j = json!({ "next": j });
        }
        let err = from_json(&Ty::Named("Node".into()), &j, &types(), "in").unwrap_err();
        assert!(err.contains("nested more than"), "{err}");
    }

    #[test]
    fn capabilities_never_go_to_models() {
        assert!(to_json(&Value::Cap(0), true).is_err());
        let rec = Value::Record { ty: "X".into(), fields: vec![("c".into(), Value::Cap(1))] };
        assert!(to_json(&rec, true).is_err());
    }

    #[test]
    fn schema_handles_recursive_types() {
        let s = schema(&Ty::Named("Node".into()), &types());
        assert_eq!(s["properties"]["next"], json!({}));
    }
}
