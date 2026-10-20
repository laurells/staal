// Runtime values, and the strict boundary between them and JSON (RUNTIME.md
// sections 2 and 3). Everything that enters from outside, run inputs and model
// output alike, goes through `from_json`.

use super::money::{Currency, Money};
use crate::ast::{Ty, Variants};
use serde_json::{json, Map, Value as Json};
use std::collections::{HashMap, HashSet};

/// A secret from the run config (an API key), or a template filled with one by
/// `fill_secret`. Its value is only ever placed in an HTTP request (a header or the
/// body); it is never printed, logged, journaled or sent to a model. `name` is the
/// environment variable it came from, and `shown` is what may be recorded: `{secret}`
/// for the key itself, or the template with a placeholder where the key goes.
#[derive(Clone, PartialEq)]
pub struct Secret {
    pub name: String,
    pub value: String,
    pub shown: String,
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({})", self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HeaderValue {
    Text(String),
    Secret(Secret),
}

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
    /// A value of enum `ty`: one variant and its fields, in declaration order.
    Enum { ty: String, variant: String, fields: Vec<(String, Value)> },
    /// A list; its length never exceeds the maximum in its type (checked where lists
    /// enter from outside, and fixed for literals).
    List(Vec<Value>),
    /// A value of `Optional<T>`: a T (`some`), or nothing (`none`).
    Opt(Option<Box<Value>>),
    /// A tuple: its elements, in order.
    Tuple(Vec<Value>),
    /// Raw output from outside (a model, or text given to `parse`), not yet checked
    /// against `ty`. `lenient`: fields the type does not declare are dropped when it is
    /// validated, as services add fields over time. Model output is strict.
    Proposed { ty: Ty, raw: Json, lenient: bool },
    Receipt(Receipt),
    /// `held` is the successful result a `try` produced (hidden from programs).
    Outcome { ok: bool, error: String, held: Option<Box<Value>> },
    Done(Vec<Value>),
    /// A handle into the capability broker.
    Cap(usize),
    Secret(Secret),
    /// An HTTP header, made by `header(name, value)`.
    Header { name: String, value: HeaderValue },
}

/// Declared types: records (name -> fields in declaration order) and enums (name ->
/// variants, each with its fields in declaration order).
#[derive(Debug, Clone, Default)]
pub struct Types {
    records: HashMap<String, Vec<(String, Ty)>>,
    enums: HashMap<String, Variants>,
}

impl Types {
    pub fn new() -> Types {
        Types::default()
    }

    /// A record type's fields.
    pub fn get(&self, name: &str) -> Option<&Vec<(String, Ty)>> {
        self.records.get(name)
    }

    pub fn insert(&mut self, name: String, fields: Vec<(String, Ty)>) {
        self.records.insert(name, fields);
    }

    /// An enum's variants.
    pub fn variants(&self, name: &str) -> Option<&Variants> {
        self.enums.get(name)
    }

    pub fn insert_enum(&mut self, name: String, variants: Vec<(String, Vec<(String, Ty)>)>) {
        self.enums.insert(name, variants);
    }
}

/// The deepest nesting accepted from outside, so hostile input cannot exhaust the stack.
const MAX_JSON_DEPTH: usize = 64;

/// Converts outside JSON into a value of type `ty`, strictly (RUNTIME.md section 3).
/// `path` names the location for error messages, e.g. "inputs.order.customer".
pub fn from_json(ty: &Ty, json: &Json, types: &Types, path: &str) -> Result<Value, String> {
    from_json_at(ty, json, types, path, 0, false)
}

/// Like `from_json`, but a record may carry fields its type does not declare; they
/// are dropped and never reach the program. Every declared field is still required
/// and checked exactly. For responses from services, which add fields over time.
pub fn from_json_lenient(ty: &Ty, json: &Json, types: &Types, path: &str) -> Result<Value, String> {
    from_json_at(ty, json, types, path, 0, true)
}

fn from_json_at(ty: &Ty, json: &Json, types: &Types, path: &str, depth: usize, lenient: bool) -> Result<Value, String> {
    if depth > MAX_JSON_DEPTH {
        return Err(format!("{path}: nested more than {MAX_JSON_DEPTH} levels deep"));
    }
    let expected = |what: &str| format!("{path}: expected {what}, found {}", describe(json));
    match ty {
        Ty::Proposed(inner) => Ok(Value::Proposed { ty: (**inner).clone(), raw: json.clone(), lenient: false }),
        Ty::Cap { .. } => Err(format!("{path}: a capability cannot be supplied as data")),
        // `null` is none; anything else must be a valid T. (The checker refuses
        // Optional<Optional<T>>, whose `some(none)` would also be `null`.)
        Ty::Optional(inner) => match json {
            Json::Null => Ok(Value::Opt(None)),
            _ => Ok(Value::Opt(Some(Box::new(from_json_at(inner, json, types, path, depth + 1, lenient)?)))),
        },
        // A tuple is a JSON array of exactly its length, each item valid as its type.
        Ty::Tuple(tys) => {
            let items = json.as_array().ok_or_else(|| expected(&format!("a list of {} items", tys.len())))?;
            if items.len() != tys.len() {
                return Err(format!("{path}: expected exactly {} items, found {}", tys.len(), items.len()));
            }
            let mut out = Vec::with_capacity(items.len());
            for (i, (item, t)) in items.iter().zip(tys).enumerate() {
                out.push(from_json_at(t, item, types, &format!("{path}[{i}]"), depth + 1, lenient)?);
            }
            Ok(Value::Tuple(out))
        }
        Ty::List(inner, max) => {
            let items = json.as_array().ok_or_else(|| expected("a list"))?;
            // Checked before looking at any element: a program's budget is computed
            // from this maximum, so a longer list must never get in.
            if items.len() as u64 > *max {
                return Err(format!("{path}: at most {max} items allowed, found {}", items.len()));
            }
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                out.push(from_json_at(inner, item, types, &format!("{path}[{i}]"), depth + 1, lenient)?);
            }
            Ok(Value::List(out))
        }
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
            "Receipt" | "Outcome" | "Done" | "Response" => {
                Err(format!("{path}: a {n} can only be created by the runtime, never supplied from outside"))
            }
            "Secret" | "Header" => Err(format!(
                "{path}: a {n} cannot be supplied as data; secrets come from the environment through \"secrets\""
            )),
            "Unit" => Err(format!("{path}: Unit cannot be supplied from outside")),
            _ if types.variants(n).is_some() => {
                let variants = types.variants(n).expect("checked");
                let names: Vec<&str> = variants.iter().map(|(v, _)| v.as_str()).collect();
                let obj = json.as_object().ok_or_else(|| expected(&format!("a {n} object with a \"kind\"")))?;
                let kind = obj.get("kind").and_then(Json::as_str).ok_or_else(|| {
                    format!("{path}.kind: expected the name of a {n} variant: {}", names.join(", "))
                })?;
                let (variant, fields) = variants.iter().find(|(v, _)| v == kind).ok_or_else(|| {
                    format!("{path}.kind: '{kind}' is not a {n} variant; expected one of {}", names.join(", "))
                })?;
                if !lenient {
                    for key in obj.keys() {
                        if key != "kind" && !fields.iter().any(|(f, _)| f == key) {
                            return Err(format!("{path}: {n}.{variant} has no field '{key}'"));
                        }
                    }
                }
                let mut out = Vec::with_capacity(fields.len());
                for (fname, fty) in fields {
                    let sub = format!("{path}.{fname}");
                    let fjson = obj.get(fname).ok_or_else(|| format!("{sub}: missing"))?;
                    out.push((fname.clone(), from_json_at(fty, fjson, types, &sub, depth + 1, lenient)?));
                }
                Ok(Value::Enum { ty: n.clone(), variant: variant.clone(), fields: out })
            }
            _ => {
                let fields = types.get(n).ok_or_else(|| format!("{path}: unknown type {n}"))?;
                let obj = json.as_object().ok_or_else(|| expected(&format!("a {n} object")))?;
                if !lenient {
                    for key in obj.keys() {
                        if !fields.iter().any(|(f, _)| f == key) {
                            return Err(format!("{path}: {n} has no field '{key}'"));
                        }
                    }
                }
                let mut out = Vec::with_capacity(fields.len());
                for (fname, fty) in fields {
                    let sub = format!("{path}.{fname}");
                    let fjson = obj.get(fname).ok_or_else(|| format!("{sub}: missing"))?;
                    out.push((fname.clone(), from_json_at(fty, fjson, types, &sub, depth + 1, lenient)?));
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
        Value::Enum { variant, fields, .. } => {
            let mut obj = Map::new();
            obj.insert("kind".into(), json!(variant));
            for (k, fv) in fields {
                obj.insert(k.clone(), to_json(fv, for_model)?);
            }
            Json::Object(obj)
        }
        Value::List(items) => Json::Array(items.iter().map(|i| to_json(i, for_model)).collect::<Result<Vec<_>, _>>()?),
        Value::Tuple(items) => Json::Array(items.iter().map(|i| to_json(i, for_model)).collect::<Result<Vec<_>, _>>()?),
        Value::Opt(None) => Json::Null,
        Value::Opt(Some(inner)) => to_json(inner, for_model)?,
        Value::Proposed { raw, .. } => raw.clone(),
        Value::Receipt(r) => json!({ "receipt": r.id, "capability": r.capability, "method": r.method }),
        Value::Outcome { ok, error, .. } => json!({ "ok": ok, "error": error }),
        Value::Done(items) => {
            json!({ "done": items.iter().map(|i| to_json(i, for_model)).collect::<Result<Vec<_>, _>>()? })
        }
        Value::Cap(_) if for_model => return Err("a capability cannot be sent to a model".into()),
        Value::Cap(h) => json!({ "capability_handle": h }),
        // A secret's value is never encoded anywhere; for the log, only its name.
        Value::Secret(_) | Value::Header { .. } if for_model => return Err("a secret cannot be sent to a model".into()),
        Value::Secret(s) => json!({ "secret": s.name, "shown": s.shown }),
        Value::Header { name, value } => match value {
            HeaderValue::Text(t) => json!({ "header": name, "value": t }),
            HeaderValue::Secret(s) => json!({ "header": name, "value": { "secret": s.name, "shown": s.shown } }),
        },
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
        Ty::List(inner, max) => json!({ "type": "array", "items": schema_at(inner, types, seen), "maxItems": max }),
        Ty::Optional(inner) => json!({ "anyOf": [schema_at(inner, types, seen), { "type": "null" }] }),
        // The compiler never asks a model for a tuple (a schema could not fix each
        // position for every provider); this is the exact JSON Schema all the same.
        Ty::Tuple(tys) => json!({
            "type": "array",
            "prefixItems": tys.iter().map(|t| schema_at(t, types, seen)).collect::<Vec<_>>(),
            "items": false,
            "minItems": tys.len(),
            "maxItems": tys.len(),
        }),
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
            _ if types.variants(n).is_some() => {
                if !seen.insert(n.clone()) {
                    return json!({});
                }
                let mut options = Vec::new();
                for (v, fields) in types.variants(n).expect("checked") {
                    let mut props = Map::new();
                    props.insert("kind".into(), json!({ "const": v }));
                    let mut required = vec![json!("kind")];
                    for (f, fty) in fields {
                        props.insert(f.clone(), schema_at(fty, types, seen));
                        required.push(json!(f));
                    }
                    options.push(json!({ "type": "object", "properties": props, "required": required, "additionalProperties": false }));
                }
                seen.remove(n);
                json!({ "anyOf": options })
            }
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
    fn optional_values_are_null_or_the_value() {
        let t = Ty::Optional(Box::new(Ty::Named("Customer".into())));
        assert_eq!(from_json(&t, &json!(null), &types(), "in").unwrap(), Value::Opt(None));
        let v = from_json(&t, &json!({ "name": "Ada" }), &types(), "in").unwrap();
        assert!(matches!(&v, Value::Opt(Some(c)) if matches!(**c, Value::Record { .. })));
        assert_eq!(to_json(&v, true).unwrap(), json!({ "name": "Ada" }));
        assert_eq!(to_json(&Value::Opt(None), true).unwrap(), json!(null));
        // What is inside is checked exactly as it would be on its own.
        let e = from_json(&t, &json!({ "name": 1 }), &types(), "in").unwrap_err();
        assert_eq!(e, "in.name: expected a string, found a number");
        let customer = json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"], "additionalProperties": false });
        assert_eq!(schema(&t, &types()), json!({ "anyOf": [customer, { "type": "null" }] }));
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
        assert!(from_json(&Ty::Cap { path: vec!["a".into()], limits: Default::default() }, &json!({}), &t, "in").is_err());
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
    fn lists_are_bounded_at_the_boundary() {
        let t = types();
        let ty = Ty::List(Box::new(Ty::Named("Int".into())), 3);
        assert_eq!(from_json(&ty, &json!([1, 2, 3]), &t, "xs").unwrap(), Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(from_json(&ty, &json!([]), &t, "xs").unwrap(), Value::List(vec![]));
        assert!(from_json(&ty, &json!([1, 2, 3, 4]), &t, "xs").unwrap_err().contains("at most 3"));
        assert!(from_json(&ty, &json!([1, "two"]), &t, "xs").unwrap_err().contains("xs[1]"));
        assert!(from_json(&ty, &json!({ "0": 1 }), &t, "xs").is_err());
        assert_eq!(schema(&ty, &t)["maxItems"], json!(3));
    }

    #[test]
    fn schema_handles_recursive_types() {
        let s = schema(&Ty::Named("Node".into()), &types());
        assert_eq!(s["properties"]["next"], json!({}));
    }
}
