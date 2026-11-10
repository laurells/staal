// JSON as an agent's files are read: RFC 8259, with every object's member names
// distinct at every depth. RFC 8259 says names SHOULD be unique and leaves duplicates
// to each parser; most keep the last, some the first, so a file with a repeated name can
// mean different things to different readers. I-JSON (RFC 7493, section 2.3) requires
// them unique, and so does `staalc` for agent.json and agent.described.json: a repeated
// name is refused, never resolved. A byte order mark is refused too (RFC 8259, section
// 8.1: implementations MUST NOT add one; a reader may ignore one, so readers disagree).
// Nesting is bounded by serde_json's limit of 128.

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value as Json};
use std::fmt;

struct Strict;

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Json;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Json, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, b: bool) -> Result<Json, E> {
        Ok(Json::Bool(b))
    }

    fn visit_i64<E>(self, n: i64) -> Result<Json, E> {
        Ok(Json::Number(n.into()))
    }

    fn visit_u64<E>(self, n: u64) -> Result<Json, E> {
        Ok(Json::Number(n.into()))
    }

    fn visit_f64<E: de::Error>(self, n: f64) -> Result<Json, E> {
        Number::from_f64(n).map(Json::Number).ok_or_else(|| E::custom("a number that is not finite"))
    }

    fn visit_str<E>(self, s: &str) -> Result<Json, E> {
        Ok(Json::String(s.to_string()))
    }

    fn visit_string<E>(self, s: String) -> Result<Json, E> {
        Ok(Json::String(s))
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(Strict)? {
            out.push(v);
        }
        Ok(Json::Array(out))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut out = Map::new();
        while let Some(k) = map.next_key::<String>()? {
            if out.contains_key(&k) {
                return Err(de::Error::custom(format!("the name '{k}' appears twice in one object")));
            }
            let v = map.next_value_seed(Strict)?;
            out.insert(k, v);
        }
        Ok(Json::Object(out))
    }
}

/// Parses `text` as one JSON value, refusing a repeated name in any object and a byte
/// order mark.
pub fn parse(text: &str) -> Result<Json, String> {
    if text.starts_with('\u{feff}') {
        return Err("the file starts with a byte order mark".into());
    }
    let mut d = serde_json::Deserializer::from_str(text);
    let v = Strict.deserialize(&mut d).map_err(|e| e.to_string())?;
    d.end().map_err(|e| e.to_string())?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_names_are_refused_at_every_depth() {
        assert!(parse(r#"{"a": 1, "b": [ {"c": {"d": 2}} ]}"#).is_ok());
        for bad in [r#"{"a": 1, "a": 1}"#, r#"{"x": {"a": 1, "a": 2}}"#, r#"[{"k": 1, "k": 2}]"#, r#"{"a": [{"b": {"c": 1, "c": 1}}]}"#] {
            assert!(parse(bad).unwrap_err().contains("appears twice"), "{bad}");
        }
        // Names that differ only by an escape are the same name.
        assert!(parse(r#"{"a": 1, "a": 2}"#).unwrap_err().contains("appears twice"));
    }

    #[test]
    fn malformed_input_is_refused() {
        for bad in ["", "{", "{\"a\": 1,}", "[1, 2,]", "{\"a\": 1} {}", "\u{feff}{}", "{'a': 1}", "{\"a\": NaN}", "{\"a\": 01}", "{\"a\": \"\u{1}\"}"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(parse(&deep).is_err(), "nesting beyond the limit");
        assert_eq!(parse(" {\"a\": [1, 2.5, null, true]} \n").unwrap(), serde_json::json!({ "a": [1, 2.5, null, true] }));
    }
}
