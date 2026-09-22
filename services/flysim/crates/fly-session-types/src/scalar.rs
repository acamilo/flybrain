//! The domain scalars of ipc-v1 section 2, and the four identities that must never be confused.
//!
//! `Id`, `U64` and `Digest` are the bus encodings: this module calls straight into
//! [`flybus::wire`] instead of restating the regular expressions, and
//! `tests/encodings.rs` pins that the two agree. Everything else here is domain-only:
//! `Scope`, `RationalNs` (reduced, positive denominator, zero as `0/1`, checked arithmetic),
//! `SchemaRef` and `TypedValue` with its 32-KiB canonical-JSON cap.

use std::cmp::Ordering;

use flybus::wire::{self, Fields, WireError};
use serde_json::{Map, Value};

use crate::canonical;

pub type Result<T> = std::result::Result<T, WireError>;

/// Every parsed domain type re-validates itself, so a value built in Rust and a value read
/// from JSON are held to the same rules.
pub trait DomainType: Sized {
    /// The name this type has in the canonical schema set.
    const TYPE_NAME: &'static str;

    /// Reads and validates one JSON value. Unknown fields are refused.
    fn from_json(value: &Value) -> Result<Self>;

    /// The canonical JSON shape of this value.
    fn to_json(&self) -> Value;

    /// The rules that are not expressible as one field read: ranges that depend on another
    /// field, uniqueness, ordering and size caps.
    fn validate(&self) -> Result<()>;
}

pub fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(WireError(message.into()))
}

pub fn wire_err(message: impl Into<String>) -> WireError {
    WireError(message.into())
}

pub(crate) fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.to_owned(), value);
    }
    Value::Object(map)
}

/// A `U64` field: the decimal string encoding, never a JSON number.
pub fn u64_json(n: u64) -> Value {
    Value::String(n.to_string())
}

/// `Id`: `^[a-z0-9][a-z0-9._-]{0,63}$`, exactly the bus encoding.
pub fn is_id(s: &str) -> bool {
    wire::is_id(s)
}

/// `Digest`: 64 lowercase hexadecimal digits, exactly the bus encoding.
pub fn is_digest(s: &str) -> bool {
    wire::is_digest(s)
}

/// `U64`: `"0"` or `[1-9][0-9]*` up to `u64::MAX`, exactly the bus encoding.
pub fn parse_u64(s: &str) -> Option<u64> {
    wire::parse_u64(s)
}

// ---------------------------------------------------------------------------------------------
// Field readers the bus reader does not have

/// A finite JSON number. NaN and infinities never survive strict parsing; this also refuses
/// integers outside the exactly representable double range, which canonical JSON cannot encode.
pub fn finite(f: &mut Fields<'_>, key: &'static str) -> Result<f64> {
    let value = f.value(key)?;
    match value {
        Value::Number(n) => canonical::finite_double(n)
            .ok_or_else(|| wire_err(format!("{key} must be a finite JSON number"))),
        _ => err(format!("{key} must be a finite JSON number")),
    }
}

/// A finite JSON number inside `lo..=hi`, refused rather than clamped.
pub fn finite_in(f: &mut Fields<'_>, key: &'static str, lo: f64, hi: f64) -> Result<f64> {
    let n = finite(f, key)?;
    if n < lo || n > hi {
        return err(format!("{key} must be in [{lo}, {hi}]"));
    }
    Ok(n)
}

/// A JSON integer in `i32` range, the seed encoding Agent.Initialize uses.
pub fn i32_field(f: &mut Fields<'_>, key: &'static str) -> Result<i32> {
    let value = f.value(key)?;
    match value.as_i64() {
        Some(n) if i64::from(i32::MIN) <= n && n <= i64::from(i32::MAX) => Ok(n as i32),
        _ => err(format!("{key} must be a signed 32-bit integer")),
    }
}

/// One member of a closed string enum.
pub fn enumeration(f: &mut Fields<'_>, key: &'static str, allowed: &[&str]) -> Result<String> {
    let s = f.string(key)?;
    if allowed.contains(&s) {
        Ok(s.to_owned())
    } else {
        err(format!("{key} must be one of {}", allowed.join(", ")))
    }
}

/// A string constant: a field whose only legal value is `expected`.
pub fn constant(f: &mut Fields<'_>, key: &'static str, expected: &str) -> Result<()> {
    let s = f.string(key)?;
    if s == expected {
        Ok(())
    } else {
        err(format!("{key} must be {expected:?}"))
    }
}

/// A `true` constant.
pub fn constant_true(f: &mut Fields<'_>, key: &'static str) -> Result<()> {
    if f.boolean(key)? {
        Ok(())
    } else {
        err(format!("{key} must be true"))
    }
}

/// A string of at most `max` Unicode code points.
pub fn bounded_string(f: &mut Fields<'_>, key: &'static str, max: usize) -> Result<String> {
    let s = f.string(key)?;
    if s.chars().count() > max {
        return err(format!("{key} must be at most {max} code points"));
    }
    Ok(s.to_owned())
}

/// `null`, or a string of at most `max` code points.
pub fn nullable_bounded_string(
    f: &mut Fields<'_>,
    key: &'static str,
    max: usize,
) -> Result<Option<String>> {
    match f.value(key)? {
        Value::Null => Ok(None),
        _ => bounded_string(f, key, max).map(Some),
    }
}

/// Reads an array of `lo..=hi` items through `read`, keeping the supplied order.
pub fn list<T>(
    f: &mut Fields<'_>,
    key: &'static str,
    lo: usize,
    hi: usize,
    read: impl Fn(&Value) -> Result<T>,
) -> Result<Vec<T>> {
    let items = f.array(key, lo, hi)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(read(item).map_err(|e| wire_err(format!("{key}: {e}")))?);
    }
    Ok(out)
}

/// An array of `lo..=hi` `Id`s.
pub fn id_list(f: &mut Fields<'_>, key: &'static str, lo: usize, hi: usize) -> Result<Vec<String>> {
    list(f, key, lo, hi, |v| match v.as_str() {
        Some(s) if is_id(s) => Ok(s.to_owned()),
        _ => err("every entry must be an id"),
    })
}

/// Fails on the first repeated key, naming it.
pub fn require_unique<'a>(keys: impl IntoIterator<Item = &'a str>, what: &str) -> Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    for key in keys {
        if seen.contains(&key) {
            return err(format!("{what}: duplicate {key:?}"));
        }
        seen.push(key);
    }
    Ok(())
}

/// Fails unless `actual` is exactly `expected`, in that order: descriptor order is part of
/// the contract, not a set membership test.
pub fn require_same_order<'a>(
    actual: impl IntoIterator<Item = &'a str>,
    expected: impl IntoIterator<Item = &'a str>,
    what: &str,
) -> Result<()> {
    let actual: Vec<&str> = actual.into_iter().collect();
    let expected: Vec<&str> = expected.into_iter().collect();
    if actual != expected {
        return err(format!(
            "{what}: must list [{}] in that order, found [{}]",
            expected.join(", "),
            actual.join(", ")
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Scope

/// `Scope`: the simulation timeline identity. Never the bus route or store incarnation.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Scope {
    pub session_id: String,
    pub epoch: String,
    pub step: u64,
}

impl Scope {
    pub fn new(session_id: &str, epoch: &str, step: u64) -> Result<Scope> {
        let scope = Scope {
            session_id: session_id.to_owned(),
            epoch: epoch.to_owned(),
            step,
        };
        scope.validate()?;
        Ok(scope)
    }

    /// `null`, or a scope.
    pub fn nullable_from_json(value: &Value) -> Result<Option<Scope>> {
        match value {
            Value::Null => Ok(None),
            _ => Scope::from_json(value).map(Some),
        }
    }

    pub fn nullable_to_json(scope: Option<&Scope>) -> Value {
        scope.map_or(Value::Null, Scope::to_json)
    }
}

impl DomainType for Scope {
    const TYPE_NAME: &'static str = "Scope";

    fn from_json(value: &Value) -> Result<Scope> {
        let mut f = Fields::new(value, "Scope")?;
        let session_id = f.id("sessionId")?;
        let epoch = f.id("epoch")?;
        let step = f.u64_string("step")?;
        f.finish()?;
        let scope = Scope {
            session_id,
            epoch,
            step,
        };
        scope.validate()?;
        Ok(scope)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("sessionId", self.session_id.clone().into()),
            ("epoch", self.epoch.clone().into()),
            ("step", u64_json(self.step)),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.session_id) {
            return err("Scope: sessionId is not a valid id");
        }
        if !is_id(&self.epoch) {
            return err("Scope: epoch is not a valid id");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// RationalNs

/// A nanosecond rational: reduced, positive denominator, zero encoded `0/1`.
///
/// ipc-v1 section 2: "Fractions are reduced, denominators positive, durations positive; zero
/// is encoded 0/1. Arithmetic is checked." Durations are checked with
/// [`RationalNs::require_positive`] by the fields that are durations; `worldTime` and a tick
/// remainder are legitimately zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RationalNs {
    pub numerator: u64,
    pub denominator: u64,
}

fn gcd(a: u64, b: u64) -> u64 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// One checked `u64` x `u64` product. The product itself always fits `u128`; the function
/// exists so every multiplication in the arithmetic below goes through one checked path.
fn mul(a: u64, b: u64) -> Result<u128> {
    u128::from(a)
        .checked_mul(u128::from(b))
        .ok_or_else(|| wire_err("RationalNs: multiplication overflowed"))
}

fn gcd128(a: u128, b: u128) -> u128 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

impl RationalNs {
    pub const ZERO: RationalNs = RationalNs {
        numerator: 0,
        denominator: 1,
    };

    /// Exactly the supplied pair, which must already be in canonical form.
    pub fn new(numerator: u64, denominator: u64) -> Result<RationalNs> {
        let r = RationalNs {
            numerator,
            denominator,
        };
        r.validate()?;
        Ok(r)
    }

    /// Reduces first, then validates: the constructor for arithmetic results.
    pub fn reduced(numerator: u128, denominator: u128) -> Result<RationalNs> {
        if denominator == 0 {
            return err("RationalNs: denominator must be positive");
        }
        let (n, d) = if numerator == 0 {
            (0u128, 1u128)
        } else {
            let g = gcd128(numerator, denominator);
            (numerator / g, denominator / g)
        };
        if n > u128::from(u64::MAX) || d > u128::from(u64::MAX) {
            return err("RationalNs: reduced value does not fit U64");
        }
        RationalNs::new(n as u64, d as u64)
    }

    pub fn is_zero(&self) -> bool {
        self.numerator == 0
    }

    /// Durations must be positive (ipc-v1 section 2).
    pub fn require_positive(&self, what: &str) -> Result<()> {
        if self.is_zero() {
            return err(format!("{what}: duration must be positive"));
        }
        Ok(())
    }

    /// Cross-multiplication of two `U64` pairs fits `u128`, but their *sum* does not: two
    /// reduced fractions near the `U64` maximum add to about 2^129. Every step is checked, as
    /// ipc-v1 section 2 requires; nothing here may wrap in release and panic in debug.
    pub fn checked_add(&self, other: &RationalNs) -> Result<RationalNs> {
        let left = mul(self.numerator, other.denominator)?;
        let right = mul(other.numerator, self.denominator)?;
        let n = left
            .checked_add(right)
            .ok_or_else(|| wire_err("RationalNs: addition overflowed"))?;
        let d = mul(self.denominator, other.denominator)?;
        RationalNs::reduced(n, d)
    }

    pub fn checked_sub(&self, other: &RationalNs) -> Result<RationalNs> {
        let left = mul(self.numerator, other.denominator)?;
        let right = mul(other.numerator, self.denominator)?;
        if right > left {
            return err("RationalNs: subtraction would be negative");
        }
        let d = mul(self.denominator, other.denominator)?;
        RationalNs::reduced(left - right, d)
    }

    pub fn checked_mul_u64(&self, k: u64) -> Result<RationalNs> {
        let n = u128::from(self.numerator)
            .checked_mul(u128::from(k))
            .ok_or_else(|| wire_err("RationalNs: multiplication overflowed"))?;
        RationalNs::reduced(n, u128::from(self.denominator))
    }

    /// The step-v1 section 5 accumulator: `ticks = floor(self / tick)` and the remainder
    /// `self - ticks * tick`, which is always `>= 0` and `< tick`.
    pub fn divide_floor(&self, tick: &RationalNs) -> Result<(u64, RationalNs)> {
        tick.require_positive("RationalNs::divide_floor tick")?;
        let n = mul(self.numerator, tick.denominator)?;
        let d = mul(self.denominator, tick.numerator)?;
        let ticks = n / d;
        if ticks > u128::from(u64::MAX) {
            return err("RationalNs: tick count does not fit U64");
        }
        let ticks = ticks as u64;
        let remainder = self.checked_sub(&tick.checked_mul_u64(ticks)?)?;
        Ok((ticks, remainder))
    }
}

impl PartialOrd for RationalNs {
    fn partial_cmp(&self, other: &RationalNs) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RationalNs {
    fn cmp(&self, other: &RationalNs) -> Ordering {
        let left = u128::from(self.numerator) * u128::from(other.denominator);
        let right = u128::from(other.numerator) * u128::from(self.denominator);
        left.cmp(&right)
    }
}

impl DomainType for RationalNs {
    const TYPE_NAME: &'static str = "RationalNs";

    fn from_json(value: &Value) -> Result<RationalNs> {
        let mut f = Fields::new(value, "RationalNs")?;
        let numerator = f.u64_string("numerator")?;
        let denominator = f.u64_string("denominator")?;
        f.finish()?;
        RationalNs::new(numerator, denominator)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("numerator", u64_json(self.numerator)),
            ("denominator", u64_json(self.denominator)),
        ])
    }

    fn validate(&self) -> Result<()> {
        if self.denominator == 0 {
            return err("RationalNs: denominator must be positive");
        }
        if self.numerator == 0 && self.denominator != 1 {
            return err("RationalNs: zero is encoded 0/1");
        }
        if self.numerator != 0 && gcd(self.numerator, self.denominator) != 1 {
            return err("RationalNs: fraction must be reduced");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// SchemaRef and TypedValue

/// `SchemaRef`: the identity of a registered typed payload schema. Version is 1..=65535.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SchemaRef {
    pub id: String,
    pub version: u16,
    pub digest: String,
}

impl SchemaRef {
    pub fn new(id: &str, version: u16, digest: &str) -> Result<SchemaRef> {
        let r = SchemaRef {
            id: id.to_owned(),
            version,
            digest: digest.to_owned(),
        };
        r.validate()?;
        Ok(r)
    }
}

impl DomainType for SchemaRef {
    const TYPE_NAME: &'static str = "SchemaRef";

    fn from_json(value: &Value) -> Result<SchemaRef> {
        let mut f = Fields::new(value, "SchemaRef")?;
        let id = f.id("id")?;
        let version = f.int("version", 1, 65_535)? as u16;
        let digest = f.string("digest")?.to_owned();
        f.finish()?;
        let r = SchemaRef {
            id,
            version,
            digest,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("id", self.id.clone().into()),
            ("version", Value::from(u64::from(self.version))),
            ("digest", self.digest.clone().into()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.id) {
            return err("SchemaRef: id is not a valid id");
        }
        if self.version == 0 {
            return err("SchemaRef: version must be 1..=65535");
        }
        if !is_digest(&self.digest) {
            return err("SchemaRef: digest must be 64 lowercase hex digits");
        }
        Ok(())
    }
}

/// The canonical-JSON size limit of one `TypedValue` (ipc-v1 section 2, workers-v1 section 1).
pub const MAX_TYPED_VALUE_BYTES: usize = 32 * 1024;

/// `TypedValue`: a schema identity plus an object, capped at 32 KiB of canonical JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedValue {
    pub schema: SchemaRef,
    pub value: Value,
}

impl TypedValue {
    pub fn new(schema: SchemaRef, value: Value) -> Result<TypedValue> {
        let t = TypedValue { schema, value };
        t.validate()?;
        Ok(t)
    }

    pub fn nullable_from_json(value: &Value) -> Result<Option<TypedValue>> {
        match value {
            Value::Null => Ok(None),
            _ => TypedValue::from_json(value).map(Some),
        }
    }

    pub fn nullable_to_json(value: Option<&TypedValue>) -> Value {
        value.map_or(Value::Null, TypedValue::to_json)
    }

    /// The canonical JSON byte length of the whole typed value.
    pub fn canonical_len(&self) -> Result<usize> {
        canonical::canonicalize(&self.to_json()).map(|s| s.len())
    }
}

impl DomainType for TypedValue {
    const TYPE_NAME: &'static str = "TypedValue";

    fn from_json(value: &Value) -> Result<TypedValue> {
        let mut f = Fields::new(value, "TypedValue")?;
        let schema = SchemaRef::from_json(f.value("schema")?)?;
        let inner = f.value("value")?.clone();
        f.finish()?;
        let t = TypedValue {
            schema,
            value: inner,
        };
        t.validate()?;
        Ok(t)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("schema", self.schema.to_json()),
            ("value", self.value.clone()),
        ])
    }

    fn validate(&self) -> Result<()> {
        self.schema.validate()?;
        if !self.value.is_object() {
            return err("TypedValue: value must be an object");
        }
        let len = self.canonical_len()?;
        if len > MAX_TYPED_VALUE_BYTES {
            return err(format!(
                "TypedValue: {len} bytes of canonical JSON exceeds the {MAX_TYPED_VALUE_BYTES}-byte limit"
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The four identities

/// A bus RPC correlation id, `call-<U64>` (bus-v1 section 6). It is not a domain operation id:
/// a safe domain retry keeps its [`DomainRequestId`] and gets a new `BusCallId`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BusCallId(String);

/// A domain operation id, `req-` plus a canonical `U64` serial (ipc-v1 section 5).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DomainRequestId(String);

/// The identity of an immutable artifact: store incarnation, artifact id and generation.
/// Not an address, not authority to read, and not an [`crate::workers::AssetRef`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArtifactIdentity {
    pub store_id: String,
    pub artifact_id: String,
    pub generation: u64,
}

/// Which kind of ownership root a token names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OwnerKind {
    /// One recipient's delivery, `dlv-<U64>`.
    Delivery,
    /// An explicit artifact hold, `own-<U64>`.
    Hold,
}

/// A delivery or explicit-hold owner token. Connection-private: it never appears in a domain
/// payload or a canonical body digest.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OwnerToken {
    token: String,
    kind: OwnerKind,
}

macro_rules! serial_identity {
    ($type:ty, $prefix:literal, $what:literal) => {
        impl $type {
            /// Parses the canonical `prefix-<U64>` form; any other prefix is refused, which is
            /// what keeps the four identities from being swapped for one another.
            pub fn parse(s: &str) -> Result<Self> {
                match wire::parse_serial_id($prefix, s) {
                    Some(_) => Ok(Self(s.to_owned())),
                    None => err(concat!($what, " must be canonical ", $prefix, "-<U64>")),
                }
            }

            pub fn from_serial(serial: u64) -> Self {
                Self(wire::serial_id($prefix, serial))
            }

            pub fn serial(&self) -> u64 {
                wire::parse_serial_id($prefix, &self.0).expect("validated on construction")
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn read(f: &mut Fields<'_>, key: &'static str) -> Result<Self> {
                let s = f.string(key)?;
                Self::parse(s).map_err(|e| wire_err(format!("{key}: {e}")))
            }

            pub fn read_nullable(f: &mut Fields<'_>, key: &'static str) -> Result<Option<Self>> {
                match f.value(key)? {
                    Value::Null => Ok(None),
                    _ => Self::read(f, key).map(Some),
                }
            }

            pub fn to_json(&self) -> Value {
                Value::String(self.0.clone())
            }
        }
    };
}

serial_identity!(BusCallId, "call", "a bus callId");
serial_identity!(DomainRequestId, "req", "a domain requestId");

impl OwnerToken {
    pub fn parse(s: &str) -> Result<OwnerToken> {
        if wire::parse_serial_id("dlv", s).is_some() {
            return Ok(OwnerToken {
                token: s.to_owned(),
                kind: OwnerKind::Delivery,
            });
        }
        if wire::parse_serial_id("own", s).is_some() {
            return Ok(OwnerToken {
                token: s.to_owned(),
                kind: OwnerKind::Hold,
            });
        }
        err("an owner token must be canonical dlv-<U64> or own-<U64>")
    }

    pub fn delivery(serial: u64) -> OwnerToken {
        OwnerToken {
            token: wire::serial_id("dlv", serial),
            kind: OwnerKind::Delivery,
        }
    }

    pub fn hold(serial: u64) -> OwnerToken {
        OwnerToken {
            token: wire::serial_id("own", serial),
            kind: OwnerKind::Hold,
        }
    }

    pub fn kind(&self) -> OwnerKind {
        self.kind
    }

    pub fn as_str(&self) -> &str {
        &self.token
    }
}

impl ArtifactIdentity {
    /// The identity half of a bus `ArtifactRef`: the parts that name the bytes, without the
    /// byte length, content type or optional digest.
    pub fn of(reference: &flybus::wire::ArtifactRef) -> ArtifactIdentity {
        ArtifactIdentity {
            store_id: reference.store_id.clone(),
            artifact_id: reference.artifact_id.clone(),
            generation: reference.generation,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if !is_id(&self.store_id) {
            return err("ArtifactIdentity: storeId is not a valid id");
        }
        if !is_id(&self.artifact_id) {
            return err("ArtifactIdentity: artifactId is not a valid id");
        }
        Ok(())
    }
}
