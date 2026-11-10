// The JSON Canonicalization Scheme (RFC 8785): one exact byte form for a JSON value,
// so that a hash of it identifies the value however the JSON was written. A journal
// binds a run's config by the SHA-256 of this form (RUNTIME.md section 11).
//
//  - Objects: members sorted by their names' UTF-16 code units (RFC 8785 section
//    3.2.3), no whitespace anywhere.
//  - Strings: the shortest JSON escape of each character (section 3.2.2.2): `\"`,
//    `\\`, `\b`, `\f`, `\n`, `\r`, `\t`, any other character below U+0020 as `\u00xx`
//    in lowercase hexadecimal, and every other character as itself in UTF-8.
//  - Numbers: as an IEEE 754 double, written as ECMAScript's Number::toString writes
//    it (ECMA-262, Number::toString; RFC 8785 section 3.2.2.3). A number that a double
//    cannot hold exactly (an integer beyond 2^53 that is not a double, for instance)
//    is refused rather than rounded: two different configs must never hash alike.
//  - Literals: `true`, `false`, `null`.

use serde_json::{Number, Value as Json};
use std::fmt::Write;

/// The canonical form of `v`, or why a number in it cannot be represented exactly.
pub fn canonical(v: &Json) -> Result<String, String> {
    let mut out = String::new();
    write_value(&mut out, v)?;
    Ok(out)
}

fn write_value(out: &mut String, v: &Json) -> Result<(), String> {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Number(n) => out.push_str(&number(n)?),
        Json::String(s) => write_string(out, s),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item)?;
            }
            out.push(']');
        }
        Json::Object(members) => {
            let mut names: Vec<&String> = members.keys().collect();
            names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, name) in names.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, name);
                out.push(':');
                write_value(out, &members[name])?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // Writing to a String cannot fail.
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The number as a double, refusing any that a double does not hold exactly.
fn number(n: &Number) -> Result<String, String> {
    let exact = |x: f64| x.is_finite();
    let x = if let Some(i) = n.as_i64() {
        let x = i as f64;
        // `as` rounds to the nearest double; it is exact only if it converts back. The
        // comparison is made in i128 so that 2^63, which no i64 holds, compares honestly.
        if x as i128 != i as i128 {
            return Err(format!("the number {i} cannot be represented exactly as a JSON number of RFC 8785 (an IEEE double)"));
        }
        x
    } else if let Some(u) = n.as_u64() {
        let x = u as f64;
        if x as u128 != u as u128 {
            return Err(format!("the number {u} cannot be represented exactly as a JSON number of RFC 8785 (an IEEE double)"));
        }
        x
    } else {
        match n.as_f64() {
            Some(x) if exact(x) => x,
            _ => return Err(format!("the number {n} is not a finite IEEE double")),
        }
    };
    Ok(es_number(x))
}

/// ECMAScript's Number::toString(x) for a finite double (ECMA-262, Number::toString,
/// radix 10). Its digits are the fewest that read back as `x`; when several digit
/// strings of that length do, the one closest to `x`, and of two equally close, the one
/// whose last digit is even. Rust's `{:e}` gives the fewest digits that read back, but
/// not always the closest of them (it rounds a tie up), so the length is taken from it
/// and the digits from Rust's exact formatting at that length (correctly rounded, ties
/// to even), unless those do not read back as `x`; then the closest digits that do are
/// on the other side of `x`, and are the ones `{:e}` gave. This function then places the
/// point and exponent as ECMAScript does.
pub fn es_number(x: f64) -> String {
    debug_assert!(x.is_finite());
    if x == 0.0 {
        // Both +0 and -0 are "0".
        return "0".into();
    }
    if x < 0.0 {
        return format!("-{}", es_number(-x));
    }
    let (shortest, n_shortest) = sci_digits(&format!("{x:e}"));
    let rounded_text = format!("{:.*e}", shortest.len() - 1, x);
    let (digits, n) = if rounded_text.parse::<f64>() == Ok(x) { sci_digits(&rounded_text) } else { (shortest, n_shortest) };
    let k = digits.len() as i64;
    if k <= n && n <= 21 {
        // An integer: the digits, then n - k zeros.
        let mut s = digits;
        s.extend(std::iter::repeat_n('0', (n - k) as usize));
        s
    } else if 0 < n && n <= 21 {
        // The point inside the digits.
        let (int, frac) = digits.split_at(n as usize);
        format!("{int}.{frac}")
    } else if -6 < n && n <= 0 {
        // A small number: "0.", -n zeros, the digits.
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        // Exponential: the first digit, the rest after a point if any, then e±(n-1).
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        let (first, rest) = digits.split_at(1);
        if rest.is_empty() {
            format!("{first}e{sign}{}", e.abs())
        } else {
            format!("{first}.{rest}e{sign}{}", e.abs())
        }
    }
}

/// The digits (without the point, and without trailing zeros) and ECMAScript's n (the
/// exponent of the first digit plus one) of Rust's scientific form "d.ddde±x" or "de±x".
fn sci_digits(sci: &str) -> (String, i64) {
    let (mantissa, exp) = sci.split_once('e').expect("Rust's scientific form of a finite double has an exponent");
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    (digits, exp.parse::<i64>().expect("Rust writes a decimal exponent") + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every double in `src/jcs_vectors.json` written as ECMAScript writes it, by
    /// Node.js 22 (`String(x)`), from `scripts/jcs_vectors.mjs`: an independent
    /// implementation of the same algorithm. Each is given by its IEEE bits, so the
    /// test feeds exactly the double that Node.js formatted.
    #[test]
    fn numbers_are_written_as_ecmascript_writes_them() {
        let vectors: Json = serde_json::from_str(include_str!("jcs_vectors.json")).unwrap();
        let cases = vectors["numbers"].as_array().unwrap();
        assert!(cases.len() > 1000, "too few vectors: {}", cases.len());
        for case in cases {
            let bits = u64::from_str_radix(case[0].as_str().unwrap(), 16).unwrap();
            let x = f64::from_bits(bits);
            assert_eq!(es_number(x), case[1].as_str().unwrap(), "bits {}", case[0]);
        }
    }

    /// Boundary values of each branch of Number::toString, by IEEE bits; every expected
    /// text was checked against Node.js 22 (`String(x)`).
    #[test]
    fn boundary_numbers() {
        for (bits, want) in [
            (0x0000000000000000u64, "0"),
            (0x8000000000000000, "0"),
            (0x0000000000000001, "5e-324"),
            (0x8000000000000001, "-5e-324"),
            (0x7fefffffffffffff, "1.7976931348623157e+308"),
            (0xffefffffffffffff, "-1.7976931348623157e+308"),
            (0x4340000000000000, "9007199254740992"),
            (0xc340000000000000, "-9007199254740992"),
            (0x4430000000000000, "295147905179352830000"),
            (0x44b52d02c7e14af5, "9.999999999999997e+22"),
            (0x44b52d02c7e14af6, "1e+23"),
            (0x44b52d02c7e14af7, "1.0000000000000001e+23"),
            (0x444b1ae4d6e2ef4e, "999999999999999700000"),
            (0x444b1ae4d6e2ef4f, "999999999999999900000"),
            (0x444b1ae4d6e2ef50, "1e+21"),
            (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
            (0x3eb0c6f7a0b5ed8d, "0.000001"),
            (0x41b3de4355555553, "333333333.3333332"),
            (0x41b3de4355555554, "333333333.33333325"),
            (0x41b3de4355555555, "333333333.3333333"),
            (0x41b3de4355555556, "333333333.3333334"),
            (0x41b3de4355555557, "333333333.33333343"),
            (0xbecbf647612f3696, "-0.0000033333333333333333"),
            (0x43143ff3c1cb0959, "1424953923781206.2"),
        ] {
            assert_eq!(es_number(f64::from_bits(bits)), want, "bits {bits:016x}");
        }
    }

    #[test]
    fn members_are_sorted_by_utf16_code_units() {
        // By UTF-16 code units, U+1F600 (a surrogate pair starting 0xD83D) sorts before
        // U+FB33, though by UTF-8 or by code point it sorts after. The expected order is
        // JavaScript's default sort of these names, which compares UTF-16 code units,
        // checked with Node.js 22.
        let v = json!({ "\u{20ac}": "Euro Sign", "\r": "Carriage Return", "\u{fb33}": "Hebrew Letter Dalet With Dagesh",
            "1": "One", "\u{1f600}": "Emoji: Grinning Face", "\u{0080}": "Control", "\u{00f6}": "Latin Small Letter O With Diaeresis" });
        let want = "{\"\\r\":\"Carriage Return\",\"1\":\"One\",\"\u{0080}\":\"Control\",\"\u{00f6}\":\"Latin Small Letter O With Diaeresis\",\
                    \"\u{20ac}\":\"Euro Sign\",\"\u{1f600}\":\"Emoji: Grinning Face\",\"\u{fb33}\":\"Hebrew Letter Dalet With Dagesh\"}";
        assert_eq!(canonical(&v).unwrap(), want);
    }

    #[test]
    fn strings_take_their_shortest_escapes() {
        let v = json!(["\u{0}\u{1f}\u{7f}", "\"\\/\u{8}\u{c}\n\r\t", "\u{2028}é😀"]);
        assert_eq!(canonical(&v).unwrap(), "[\"\\u0000\\u001f\u{7f}\",\"\\\"\\\\/\\b\\f\\n\\r\\t\",\"\u{2028}é😀\"]");
    }

    #[test]
    fn nesting_literals_and_whitespace() {
        let v: Json = serde_json::from_str("{ \"b\" : [ 1 , 2.50 , { \"z\":null, \"a\":true } ], \"a\" : false }").unwrap();
        assert_eq!(canonical(&v).unwrap(), "{\"a\":false,\"b\":[1,2.5,{\"a\":true,\"z\":null}]}");
    }

    #[test]
    fn integers_a_double_cannot_hold_are_refused() {
        // 2^53 + 1 is the first integer a double cannot hold; 2^53 and 2^60 it can.
        assert_eq!(canonical(&json!(9007199254740992u64)).unwrap(), "9007199254740992");
        // 2^60 is held exactly, and written with ECMAScript's shortest digits.
        assert_eq!(canonical(&json!(1152921504606846976u64)).unwrap(), "1152921504606847000");
        assert!(canonical(&json!(9007199254740993u64)).is_err());
        assert!(canonical(&json!(-9007199254740993i64)).is_err());
        assert!(canonical(&json!(u64::MAX)).is_err());
        assert_eq!(canonical(&json!(i64::MIN)).unwrap(), "-9223372036854776000");
    }
}
