//! The scalar encodings of `ipc-v1` section 2: `Id`, `U64`, `Digest`, `Scope`, `RationalNs`,
//! `SchemaRef` and `TypedValue`.
//!
//! Every type parses strictly. A `U64` is a canonical decimal string, never a JSON number; a
//! `RationalNs` is reduced with a positive denominator; an `Id` matches
//! `^[a-z0-9][a-z0-9._-]{0,63}$`. Nothing here knows about a method, a phase or a bus.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `^[a-z0-9][a-z0-9._-]{0,63}$`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(String);

impl Id {
    /// Parses an `Id`, or reports why the string is not one.
    pub fn parse(s: &str) -> Result<Id, String> {
        if s.is_empty() || s.len() > 64 {
            return Err(format!("id must be 1..=64 characters, got {}", s.len()));
        }
        let first = s.as_bytes()[0];
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err("id must start with a lowercase letter or a digit".to_owned());
        }
        for b in s.bytes() {
            let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_'
                || b == b'-';
            if !ok {
                return Err("id may only contain [a-z0-9._-]".to_owned());
            }
        }
        Ok(Id(s.to_owned()))
    }

    /// Parses an `Id` from a trusted literal; panics on a malformed one.
    pub fn lit(s: &str) -> Id {
        Id::parse(s).expect("malformed Id literal")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Id, D::Error> {
        let s = String::deserialize(d)?;
        Id::parse(&s).map_err(D::Error::custom)
    }
}

/// `"0"` or `[1-9][0-9]*`, at most `u64::MAX`, carried as a decimal string on the wire.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct U64(pub u64);

impl U64 {
    pub fn get(self) -> u64 {
        self.0
    }

    /// Parses the canonical decimal encoding. Leading zeros and signs are refused.
    pub fn parse(s: &str) -> Result<U64, String> {
        if s.is_empty() {
            return Err("u64 must not be empty".to_owned());
        }
        if s != "0" && s.starts_with('0') {
            return Err(format!("u64 {s:?} has a leading zero"));
        }
        if !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("u64 {s:?} is not decimal digits"));
        }
        s.parse::<u64>().map(U64).map_err(|_| format!("u64 {s:?} overflows"))
    }
}

impl From<u64> for U64 {
    fn from(v: u64) -> U64 {
        U64(v)
    }
}

impl fmt::Debug for U64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for U64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for U64 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for U64 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<U64, D::Error> {
        let s = String::deserialize(d)?;
        U64::parse(&s).map_err(D::Error::custom)
    }
}

/// 64 lowercase hexadecimal digits: a SHA-256.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(String);

impl Digest {
    pub fn parse(s: &str) -> Result<Digest, String> {
        if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("digest must be 64 lowercase hexadecimal digits".to_owned());
        }
        Ok(Digest(s.to_owned()))
    }

    /// The SHA-256 of `bytes`, hex-encoded.
    pub fn of(bytes: &[u8]) -> Digest {
        use sha2::{Digest as _, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        let out = h.finalize();
        let mut s = String::with_capacity(64);
        for b in out {
            s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
            s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
        }
        Digest(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Digest, D::Error> {
        let s = String::deserialize(d)?;
        Digest::parse(&s).map_err(D::Error::custom)
    }
}

/// The simulation timeline coordinate: which session, which epoch, which step.
///
/// Epoch protects simulation order. It is not a bus route incarnation or a store id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Scope {
    pub session_id: Id,
    pub epoch: Id,
    pub step: U64,
}

impl Scope {
    pub fn new(session_id: &Id, epoch: &Id, step: u64) -> Scope {
        Scope { session_id: session_id.clone(), epoch: epoch.clone(), step: U64(step) }
    }

    /// The same scope at `step + 1`.
    pub fn next(&self) -> Scope {
        Scope { step: U64(self.step.0 + 1), ..self.clone() }
    }
}

/// A reduced, positive-denominator duration in nanoseconds. Zero is `0/1`.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RationalNs {
    pub numerator: U64,
    pub denominator: U64,
}

impl RationalNs {
    /// Reduces `numerator / denominator`; the denominator must be positive.
    pub fn new(numerator: u64, denominator: u64) -> Result<RationalNs, String> {
        if denominator == 0 {
            return Err("rational denominator must be positive".to_owned());
        }
        let g = gcd(numerator, denominator);
        let g = if g == 0 { 1 } else { g };
        Ok(RationalNs { numerator: U64(numerator / g), denominator: U64(denominator / g) })
    }

    pub fn zero() -> RationalNs {
        RationalNs { numerator: U64(0), denominator: U64(1) }
    }

    /// `hz` steps per second as an exact nanosecond duration.
    pub fn from_hz(hz: u64) -> Result<RationalNs, String> {
        RationalNs::new(1_000_000_000, hz)
    }

    pub fn from_millis(ms: u64) -> Result<RationalNs, String> {
        ms.checked_mul(1_000_000)
            .ok_or_else(|| "millisecond duration overflows".to_owned())
            .and_then(|ns| RationalNs::new(ns, 1))
    }

    pub fn is_zero(&self) -> bool {
        self.numerator.0 == 0
    }

    pub fn is_positive(&self) -> bool {
        self.numerator.0 > 0
    }

    /// Rejects an unreduced, zero-denominator or non-canonical value that parsed as JSON.
    pub fn validate(&self) -> Result<(), String> {
        if self.denominator.0 == 0 {
            return Err("rational denominator must be positive".to_owned());
        }
        if self.numerator.0 == 0 {
            if self.denominator.0 != 1 {
                return Err("rational zero must be encoded 0/1".to_owned());
            }
            return Ok(());
        }
        if gcd(self.numerator.0, self.denominator.0) != 1 {
            return Err("rational must be reduced".to_owned());
        }
        Ok(())
    }

    /// Checked addition, reduced.
    pub fn checked_add(&self, other: &RationalNs) -> Result<RationalNs, String> {
        let l = self.numerator.0.checked_mul(other.denominator.0);
        let r = other.numerator.0.checked_mul(self.denominator.0);
        let d = self.denominator.0.checked_mul(other.denominator.0);
        match (l, r, d) {
            (Some(l), Some(r), Some(d)) => {
                let n = l.checked_add(r).ok_or_else(|| "rational add overflows".to_owned())?;
                RationalNs::new(n, d)
            }
            _ => Err("rational add overflows".to_owned()),
        }
    }

    /// Checked subtraction; refuses a negative result.
    pub fn checked_sub(&self, other: &RationalNs) -> Result<RationalNs, String> {
        let l = self
            .numerator
            .0
            .checked_mul(other.denominator.0)
            .ok_or_else(|| "rational sub overflows".to_owned())?;
        let r = other
            .numerator
            .0
            .checked_mul(self.denominator.0)
            .ok_or_else(|| "rational sub overflows".to_owned())?;
        let d = self
            .denominator
            .0
            .checked_mul(other.denominator.0)
            .ok_or_else(|| "rational sub overflows".to_owned())?;
        let n = l.checked_sub(r).ok_or_else(|| "rational sub would be negative".to_owned())?;
        RationalNs::new(n, d)
    }

    /// Checked multiplication by a whole count.
    pub fn checked_mul_u64(&self, k: u64) -> Result<RationalNs, String> {
        let n = self
            .numerator
            .0
            .checked_mul(k)
            .ok_or_else(|| "rational scale overflows".to_owned())?;
        RationalNs::new(n, self.denominator.0)
    }

    /// `floor(self / other)`; `other` must be positive.
    pub fn checked_div_floor(&self, other: &RationalNs) -> Result<u64, String> {
        if other.numerator.0 == 0 {
            return Err("cannot divide by a zero duration".to_owned());
        }
        let l = self
            .numerator
            .0
            .checked_mul(other.denominator.0)
            .ok_or_else(|| "rational divide overflows".to_owned())?;
        let r = other
            .numerator
            .0
            .checked_mul(self.denominator.0)
            .ok_or_else(|| "rational divide overflows".to_owned())?;
        Ok(l / r)
    }

    pub fn cmp_value(&self, other: &RationalNs) -> std::cmp::Ordering {
        let l = u128::from(self.numerator.0) * u128::from(other.denominator.0);
        let r = u128::from(other.numerator.0) * u128::from(self.denominator.0);
        l.cmp(&r)
    }
}

impl fmt::Debug for RationalNs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}ns", self.numerator.0, self.denominator.0)
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Identity of a registered payload schema: what shape, which revision, which definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SchemaRef {
    pub id: Id,
    pub version: u16,
    pub digest: Digest,
}

impl SchemaRef {
    /// A schema reference whose digest is derived from its own name and version, so the
    /// synthetic composition has stable identities without a schema registry file.
    pub fn synthetic(id: &str, version: u16) -> SchemaRef {
        let id = Id::lit(id);
        let digest = Digest::of(format!("fly-session-schema-v1\n{id}\n{version}\n").as_bytes());
        SchemaRef { id, version, digest }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version == 0 {
            return Err("schema version must be 1..=65535".to_owned());
        }
        Ok(())
    }
}

/// A schema-tagged JSON object. The canonical encoding of `value` is capped at 32 KiB.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedValue {
    pub schema: SchemaRef,
    pub value: serde_json::Map<String, serde_json::Value>,
}

/// `ipc-v1` section 2: a `TypedValue`'s canonical JSON is at most 32 KiB.
pub const MAX_TYPED_VALUE_BYTES: usize = 32 * 1024;

impl TypedValue {
    pub fn new(schema: SchemaRef, value: serde_json::Map<String, serde_json::Value>) -> TypedValue {
        TypedValue { schema, value }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.schema.validate()?;
        let canonical = crate::fly_session_types::canonical::canonical_json(
            &serde_json::Value::Object(self.value.clone()),
        );
        if canonical.len() > MAX_TYPED_VALUE_BYTES {
            return Err(format!(
                "typed value is {} canonical bytes, over the 32 KiB limit",
                canonical.len()
            ));
        }
        Ok(())
    }

    /// The canonical digest of the whole typed value, schema identity included.
    pub fn digest(&self) -> Digest {
        crate::fly_session_types::canonical::canonical_digest(
            &serde_json::to_value(self).expect("TypedValue serializes"),
        )
    }

    pub fn number(&self, key: &str) -> Result<f64, String> {
        self.value
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!("typed value has no finite number {key:?}"))
    }

    pub fn integer(&self, key: &str) -> Result<i64, String> {
        self.value
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| format!("typed value has no integer {key:?}"))
    }

    pub fn boolean(&self, key: &str) -> Result<bool, String> {
        self.value
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| format!("typed value has no boolean {key:?}"))
    }
}
