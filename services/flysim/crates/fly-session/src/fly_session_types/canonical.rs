//! Canonical JSON (RFC 8785) and the digests built on it.
//!
//! Two payloads mean the same domain operation when their canonical encodings are equal, so a
//! duplicate `Agent.Prepare` can be told from a changed one without depending on key order,
//! whitespace or float formatting. Object keys sort by UTF-16 code unit; numbers use the
//! ECMAScript `Number::toString` shortest form (`ryu_js`), so `1.0` and `1` are one value.

use serde_json::Value;

use super::scalars::Digest;

/// The canonical JSON encoding of `value`.
///
/// Panics on a non-finite number, which cannot appear in a validated payload and cannot be
/// represented in JSON at all.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

/// The SHA-256 of the canonical encoding.
pub fn canonical_digest(value: &Value) -> Digest {
    Digest::of(canonical_json(value).as_bytes())
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| utf16_cmp(a, b));
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, key);
                out.push(':');
                write_value(out, &map[*key]);
            }
            out.push('}');
        }
    }
}

fn write_number(out: &mut String, n: &serde_json::Number) {
    if let Some(u) = n.as_u64() {
        out.push_str(&u.to_string());
        return;
    }
    if let Some(i) = n.as_i64() {
        out.push_str(&i.to_string());
        return;
    }
    let f = n.as_f64().expect("a JSON number is representable");
    assert!(f.is_finite(), "canonical JSON cannot encode a non-finite number");
    let mut buf = ryu_js::Buffer::new();
    out.push_str(buf.format(f));
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
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Compares two strings by UTF-16 code unit, which is what RFC 8785 sorts object keys by.
///
/// Byte order and code-unit order disagree only above the BMP, so the surrogate expansion
/// matters for a key containing an astral character.
fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.encode_utf16();
    let mut bi = b.encode_utf16();
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x == y => continue,
            (Some(x), Some(y)) => return x.cmp(&y),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_sort_and_floats_take_their_shortest_form() {
        let v = json!({"b": 1.0, "a": [1, 2.5], "\u{00e9}": "x"});
        assert_eq!(canonical_json(&v), "{\"a\":[1,2.5],\"b\":1,\"\u{00e9}\":\"x\"}");
    }

    #[test]
    fn the_same_object_written_two_ways_has_one_digest() {
        let a: Value = serde_json::from_str("{\"x\":1,\"y\":{\"p\":2,\"q\":3}}").unwrap();
        let b: Value = serde_json::from_str("{\"y\":{\"q\":3,\"p\":2},\"x\":1}").unwrap();
        assert_eq!(canonical_digest(&a), canonical_digest(&b));
    }

    #[test]
    fn an_astral_key_sorts_by_code_unit_not_by_byte() {
        // U+10000 encodes as the surrogate pair D800 DC00, which is below U+FFFD's E000-range
        // code unit in UTF-16 but above it byte-wise.
        let v = json!({"\u{10000}": 1, "\u{fffd}": 2});
        assert_eq!(canonical_json(&v), "{\"\u{10000}\":1,\"\u{fffd}\":2}");
    }
}
