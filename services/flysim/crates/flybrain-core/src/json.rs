//! `JSON.parse` / `JSON.stringify` with JavaScript semantics.
//!
//! Two places need byte-exact agreement with V8 rather than merely valid JSON:
//!
//! - the dataset fingerprint hashes `JSON.stringify(meta)`, so key order, number formatting and
//!   string escaping all have to match;
//! - a checkpoint envelope's manifest is hashed by the envelope's own CRC32, so the same applies.
//!
//! `serde_json` would print `1.0` where `JSON.stringify` prints `1`, so values are built and
//! written through [`JsonValue`] here. Parsing goes through `serde_json` with `preserve_order` and
//! is then converted, which reproduces `JSON.parse` (every number becomes an `f64`, object key
//! order is insertion order).

use std::fmt::Write as _;

use crate::error::{Error, Result};
use crate::jsmath::number_to_string;

/// A JavaScript JSON value. Objects keep insertion order.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    pub fn object() -> Self {
        JsonValue::Object(Vec::new())
    }

    /// `object[key] = value`, with JavaScript's rule that an existing key keeps its position.
    pub fn set(&mut self, key: &str, value: JsonValue) {
        if let JsonValue::Object(entries) = self {
            if let Some(slot) = entries.iter_mut().find(|(name, _)| name == key) {
                slot.1 = value;
            } else {
                entries.push((key.to_string(), value));
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            JsonValue::Number(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, JsonValue)]> {
        match self {
            JsonValue::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, JsonValue::Null)
    }

    /// A non-negative integer field, as `Number.isSafeInteger` would accept it.
    pub fn as_usize(&self) -> Option<usize> {
        let value = self.as_f64()?;
        if value.fract() != 0.0 || !(0.0..=9_007_199_254_740_991.0).contains(&value) {
            return None;
        }
        Some(value as usize)
    }

    /// `JSON.stringify(value)`.
    pub fn stringify(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            JsonValue::Null => out.push_str("null"),
            JsonValue::Bool(true) => out.push_str("true"),
            JsonValue::Bool(false) => out.push_str("false"),
            // JSON.stringify writes a non-finite number as null.
            JsonValue::Number(value) if !value.is_finite() => out.push_str("null"),
            JsonValue::Number(value) => out.push_str(&number_to_string(*value)),
            JsonValue::String(value) => write_quoted(value, out),
            JsonValue::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            JsonValue::Object(entries) => {
                out.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_quoted(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    /// `JSON.parse(text)`.
    pub fn parse(text: &str) -> Result<JsonValue> {
        let parsed: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| Error::new(format!("Unexpected token in JSON: {error}")))?;
        Ok(JsonValue::from_serde(&parsed))
    }

    fn from_serde(value: &serde_json::Value) -> JsonValue {
        match value {
            serde_json::Value::Null => JsonValue::Null,
            serde_json::Value::Bool(flag) => JsonValue::Bool(*flag),
            // JSON.parse yields a double for every numeric literal.
            serde_json::Value::Number(number) => {
                JsonValue::Number(number.as_f64().unwrap_or(f64::NAN))
            }
            serde_json::Value::String(text) => JsonValue::String(text.clone()),
            serde_json::Value::Array(items) => {
                JsonValue::Array(items.iter().map(JsonValue::from_serde).collect())
            }
            serde_json::Value::Object(entries) => JsonValue::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), JsonValue::from_serde(value)))
                    .collect(),
            ),
        }
    }
}

impl From<f64> for JsonValue {
    fn from(value: f64) -> Self {
        JsonValue::Number(value)
    }
}

impl From<bool> for JsonValue {
    fn from(value: bool) -> Self {
        JsonValue::Bool(value)
    }
}

impl From<&str> for JsonValue {
    fn from(value: &str) -> Self {
        JsonValue::String(value.to_string())
    }
}

impl From<String> for JsonValue {
    fn from(value: String) -> Self {
        JsonValue::String(value)
    }
}

impl From<usize> for JsonValue {
    fn from(value: usize) -> Self {
        JsonValue::Number(value as f64)
    }
}

impl From<u32> for JsonValue {
    fn from(value: u32) -> Self {
        JsonValue::Number(f64::from(value))
    }
}

impl From<i32> for JsonValue {
    fn from(value: i32) -> Self {
        JsonValue::Number(f64::from(value))
    }
}

/// `QuoteJSONString`: escape `"`, `\` and the C0 controls, and pass everything else through.
///
/// A Rust `str` is well-formed UTF-8 and so holds no lone surrogates, which is the only case where
/// `JSON.stringify` emits a `\uXXXX` escape for a non-control character. Non-BMP scalars are
/// emitted verbatim, exactly as V8 does.
fn write_quoted(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            character if (character as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stringify_prints_integers_without_a_fraction() {
        let mut object = JsonValue::object();
        object.set("schemaVersion", 1.0.into());
        object.set("dataset", "test".into());
        object.set("gain", 0.2.into());
        object.set(
            "roles",
            JsonValue::Array(vec![0.0.into(), 1.0.into(), 2.0.into()]),
        );
        assert_eq!(
            object.stringify(),
            r#"{"schemaVersion":1,"dataset":"test","gain":0.2,"roles":[0,1,2]}"#
        );
    }

    #[test]
    fn set_keeps_the_position_of_an_existing_key() {
        let mut object = JsonValue::object();
        object.set("a", 1.0.into());
        object.set("b", 2.0.into());
        object.set("a", 3.0.into());
        assert_eq!(object.stringify(), r#"{"a":3,"b":2}"#);
    }

    #[test]
    fn round_trips_through_parse() {
        let text = r#"{"a":[1,2.5,-3],"b":{"c":"x\ny"},"d":null,"e":true}"#;
        assert_eq!(JsonValue::parse(text).unwrap().stringify(), text);
    }

    #[test]
    fn escapes_the_same_characters_as_json_stringify() {
        assert_eq!(
            JsonValue::String("a\"b\\c\nd\u{1}".to_string()).stringify(),
            "\"a\\\"b\\\\c\\nd\\u0001\""
        );
        assert_eq!(
            JsonValue::String("héllo".to_string()).stringify(),
            "\"héllo\""
        );
    }
}
