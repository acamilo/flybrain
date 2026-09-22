//! Canonical JSON (RFC 8785) and the digest rules of ipc-v1 section 5.
//!
//! One serialization, two languages: keys sorted by UTF-16 code unit, numbers printed by the
//! ECMAScript `Number::toString` algorithm (so a JavaScript `JSON.stringify` over the same
//! sorted tree produces the same bytes), strings escaped the way `JSON.stringify` escapes
//! them, no insignificant whitespace. A digest is the SHA-256 of those bytes, lowercase hex.
//!
//! Numbers outside the exactly representable double range are refused rather than rounded:
//! every counter and clock in these contracts is a `U64` decimal string, so a JSON number
//! larger than 2^53-1 is a schema error, not something to canonicalize approximately.

use serde_json::{Number, Value};
use sha2::{Digest as _, Sha256};

use crate::scalar::{Result, Scope, err, wire_err};

/// The largest integer a double represents exactly.
pub const MAX_EXACT_INTEGER: i64 = 9_007_199_254_740_991;

/// The bus envelope ceiling every domain message must also fit (bus-v1 section 4).
pub const MAX_ENVELOPE_BYTES: usize = flybus::wire::MAX_ENVELOPE_BYTES;

/// The `f64` a JSON number denotes, or `None` if it is not a finite exactly representable one.
pub fn finite_double(n: &Number) -> Option<f64> {
    if let Some(u) = n.as_u64() {
        return (u <= MAX_EXACT_INTEGER as u64).then_some(u as f64);
    }
    if let Some(i) = n.as_i64() {
        return (i >= -MAX_EXACT_INTEGER).then_some(i as f64);
    }
    n.as_f64().filter(|v| v.is_finite())
}

/// `String(number)` for a finite double, the ECMAScript algorithm RFC 8785 requires.
fn number_to_string(value: f64) -> String {
    if value == 0.0 {
        // Covers -0.0, which `JSON.stringify` prints as "0".
        return "0".to_owned();
    }
    let mut buffer = ryu_js::Buffer::new();
    buffer.format(value).to_owned()
}

/// Escapes one string the way `JSON.stringify` does.
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0a}' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0d}' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Sorts object keys by UTF-16 code unit, as RFC 8785 section 3.2.3 specifies.
fn utf16_key(key: &str) -> Vec<u16> {
    key.encode_utf16().collect()
}

/// The canonical JSON text of `value`.
pub fn canonicalize(value: &Value) -> Result<String> {
    let mut out = String::new();
    write_value(&mut out, value)?;
    Ok(out)
}

/// The canonical JSON bytes of `value`.
pub fn canonical_bytes(value: &Value) -> Result<Vec<u8>> {
    canonicalize(value).map(String::into_bytes)
}

fn write_value(out: &mut String, value: &Value) -> Result<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            let d = finite_double(n).ok_or_else(|| {
                wire_err(format!(
                    "canonical JSON: {n} is not a finite number in the exact double range"
                ))
            })?;
            out.push_str(&number_to_string(d));
        }
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by_cached_key(|k| utf16_key(k));
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, key);
                out.push(':');
                write_value(out, &map[key.as_str()])?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The canonical digest of a JSON value: SHA-256 over its canonical JSON bytes.
pub fn digest_of(value: &Value) -> Result<String> {
    canonical_bytes(value).map(|bytes| sha256_hex(&bytes))
}

/// Parses JSON strictly: duplicate keys at any depth, invalid UTF-8, non-finite numbers and
/// trailing bytes are refused. The bus reader, reused so both layers agree byte for byte.
pub fn parse_strict(bytes: &[u8]) -> Result<Value> {
    flybus::wire::parse_json_strict(bytes).map_err(|e| wire_err(e.0))
}

/// Refuses a domain payload that does not fit the bus envelope ceiling.
///
/// The check is on canonical bytes, and the caller passes the overhead the surrounding
/// envelope adds, so a payload that only fits without its envelope still fails.
pub fn require_envelope_fit(value: &Value, envelope_overhead: usize) -> Result<usize> {
    let len = canonicalize(value)?.len();
    let total = len + envelope_overhead;
    if total > MAX_ENVELOPE_BYTES {
        return err(format!(
            "envelope: {total} bytes exceeds the {MAX_ENVELOPE_BYTES}-byte maximum"
        ));
    }
    Ok(total)
}

// ---------------------------------------------------------------------------------------------
// Operation keys and canonical bodies

/// The keys that belong to the bus, never to a domain body (ipc-v1 section 5: the canonical
/// body "excludes changing bus callIds, deliveryIds and owner tokens").
pub const BUS_ONLY_KEYS: &[&str] = &[
    "callId",
    "deliveryId",
    "ownerId",
    "ownerIds",
    "deliveryIds",
    "requestDeliveryId",
    "expectedIncarnation",
    "serviceIncarnation",
    "connectionId",
    "topicSequence",
    "subscriptionId",
];

/// Fails if any bus-only key appears anywhere in `value`.
pub fn reject_bus_identities(value: &Value) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                if BUS_ONLY_KEYS.contains(&key.as_str()) {
                    return err(format!(
                        "canonical body: {key:?} is a bus identity and never part of a domain body"
                    ));
                }
                reject_bus_identities(inner)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_bus_identities(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// `(sessionId, epoch, step, method, workerId)`: the operation key of a step mutation.
///
/// There is at most one Prepare, Commit or Advance for one key (ipc-v1 section 5). The key
/// deliberately does not contain the requestId: a changed id for an existing key is CONFLICT,
/// which can only be detected if the key is the same.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OperationKey {
    pub scope: Scope,
    pub method: String,
    pub worker_id: String,
}

impl OperationKey {
    pub fn new(scope: Scope, method: &str, worker_id: &str) -> Result<OperationKey> {
        let key = OperationKey {
            scope,
            method: method.to_owned(),
            worker_id: worker_id.to_owned(),
        };
        key.validate()?;
        Ok(key)
    }

    pub fn validate(&self) -> Result<()> {
        use crate::scalar::DomainType;
        self.scope.validate()?;
        if !flybus::wire::is_method(&self.method) {
            return err("OperationKey: method must be 1..=128 printable ASCII characters");
        }
        if !crate::scalar::is_id(&self.worker_id) {
            return err("OperationKey: workerId is not a valid id");
        }
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        use crate::scalar::DomainType;
        crate::scalar::obj(vec![
            ("scope", self.scope.to_json()),
            ("method", self.method.clone().into()),
            ("workerId", self.worker_id.clone().into()),
        ])
    }

    /// The canonical digest of the key, for a deduplication table that stores digests.
    pub fn digest(&self) -> Result<String> {
        digest_of(&self.to_json())
    }
}

/// The canonical body of a domain operation: method, scope and validated params.
///
/// Two calls of the same operation key whose body digests differ are CONFLICT; two calls with
/// the same digest are the same operation, whatever bus callId carried them.
pub fn canonical_body(method: &str, scope: Option<&Scope>, params: &Value) -> Result<Value> {
    if !flybus::wire::is_method(method) {
        return err("canonical body: method must be 1..=128 printable ASCII characters");
    }
    if !params.is_object() {
        return err("canonical body: params must be an object");
    }
    reject_bus_identities(params)?;
    Ok(crate::scalar::obj(vec![
        ("method", method.into()),
        ("scope", Scope::nullable_to_json(scope)),
        ("params", params.clone()),
    ]))
}

/// The canonical body digest of a domain operation.
pub fn body_digest(method: &str, scope: Option<&Scope>, params: &Value) -> Result<String> {
    digest_of(&canonical_body(method, scope, params)?)
}
