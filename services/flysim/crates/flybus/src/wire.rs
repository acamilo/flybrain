//! The wire: scalar encodings, strict JSON, the envelope and its framing (bus-v1 section 4).
//!
//! A frame is a `u32` little-endian byte count followed by that many bytes of UTF-8 JSON. The
//! JSON is parsed strictly: duplicate keys at any depth, invalid UTF-8, non-finite numbers,
//! trailing bytes and unknown envelope fields are all refused. Both ends use this module, so
//! the router and the client agree byte for byte on what is valid.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::BusError;

pub const PROTOCOL: &str = "flybus";
pub const MAJOR: u64 = 1;
pub const MINOR: u64 = 0;
/// Largest JSON envelope, in bytes, not counting the four-byte length prefix.
pub const MAX_ENVELOPE_BYTES: usize = 65_536;
pub const MAX_ATTACHMENTS: usize = 32;
/// Release and consume batches carry 1..=64 ids.
pub const MAX_BATCH: usize = 64;
/// Queue and credit requests are integers in 1..=65535.
pub const MAX_CREDIT: u64 = 65_535;
pub const MAX_NAME_LEN: usize = 192;
pub const MAX_METHOD_LEN: usize = 128;
pub const MAX_CONTENT_TYPE_LEN: usize = 127;
/// The one artifact generation v1 issues; ids and inodes are never reused.
pub const GENERATION: u64 = 1;

/// The operations, delivery and notice shapes this implementation speaks. `contractDigest` is
/// the SHA-256 of this text, so any change to it changes the digest a client sees in hello.
pub const CONTRACT: &str = "flybus 1.0
frame: u32le length, 1..=65536 bytes of strict UTF-8 JSON
envelope: protocol major minor id replyTo kind op body attachments
attachment: name ref ownerId
ref: storeId artifactId generation byteLength contentType digest
reply: ok value | ok error{code message dispatch}
bus.hello: clientId clientIncarnation supportedMajors -> routerId connectionId selectedMajor selectedMinor contractDigest limits
service.register: name maxQueued maxInFlight -> serviceIncarnation
service.unregister: name serviceIncarnation -> removed
rpc.call: callId target expectedIncarnation method payload -> accepted serviceIncarnation
rpc.reply: callId requestDeliveryId outcome -> routed
rpc.responder.release: callId requestDeliveryId -> released; final attached release emits call.failed dispatched (CALL_GONE, or NO_SERVICE after route loss)
rpc.cancel: callId -> state(cancelled-before-dispatch|execution-unknown|completed|call-gone)
topic.declare: name retained(none|latest) -> declared topicIncarnation
topic.clear: name -> cleared
topic.delete: name -> deleted
subscribe: topic mode(latest|bounded) maxQueued maxInFlight replayLatest -> subscriptionId topicIncarnation
unsubscribe: subscriptionId -> removed
publish: topic payload -> topicSequence subscribers replaced
delivery.consumed: deliveryIds -> released
artifact.allocate: byteLength contentType -> artifactId generation ownerId writeLocation
artifact.seal: artifactId generation ownerId digest -> ref ownerId
artifact.open: ref ownerId -> readLocation
artifact.retain: ref ownerId -> ownerId
artifact.release: ownerIds -> released
delivery rpc.request: deliveryId callId caller target serviceIncarnation method payload
delivery rpc.result: deliveryId callId responder serviceIncarnation outcome
delivery topic.message: deliveryId subscriptionId topic topicIncarnation topicSequence replaced payload
notice call.failed: callId code message dispatch
notice route.removed: name serviceIncarnation reason
notice subscription.closed: subscriptionId topic topicIncarnation reason
notice connection.closing: code message
";

/// SHA-256 of [`CONTRACT`], lowercase hex.
pub fn contract_digest() -> String {
    hex(&Sha256::digest(CONTRACT.as_bytes()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A wire-level validation failure. Always maps to `INVALID_ENVELOPE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireError(pub String);

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WireError {}

impl From<WireError> for BusError {
    fn from(e: WireError) -> BusError {
        BusError::invalid(e.0)
    }
}

fn err<T>(message: impl Into<String>) -> Result<T, WireError> {
    Err(WireError(message.into()))
}

// ---------------------------------------------------------------------------------------------
// Scalars

/// `Id`: `^[a-z0-9][a-z0-9._-]{0,63}$`.
pub fn is_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|&c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

/// `U64`: `"0"` or `[1-9][0-9]*`, at most `u64::MAX`.
pub fn parse_u64(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 20 || (b.len() > 1 && b[0] == b'0') {
        return None;
    }
    let mut n: u64 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(u64::from(c - b'0'))?;
    }
    Some(n)
}

/// `Digest`: 64 lowercase hexadecimal digits.
pub fn is_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Service and topic names: 1..=192 of `[a-z0-9._-]`, no empty dot-separated segment.
pub fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NAME_LEN
        && s.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
        && s.split('.').all(|seg| !seg.is_empty())
}

/// RPC method: 1..=128 printable ASCII characters.
pub fn is_method(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_METHOD_LEN && s.bytes().all(|c| (0x20..=0x7e).contains(&c))
}

/// Content type: 1..=127 printable ASCII characters.
pub fn is_content_type(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_CONTENT_TYPE_LEN
        && s.bytes().all(|c| (0x20..=0x7e).contains(&c))
}

/// Operation names: 1..=64 of `[a-z.]`.
pub fn is_op(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_lowercase() || c == b'.')
}

/// `<prefix>-<U64>`, the canonical form of every serial-numbered id on the bus.
pub fn serial_id(prefix: &str, n: u64) -> String {
    format!("{prefix}-{n}")
}

/// Parses `<prefix>-<U64>`; `None` unless canonical.
pub fn parse_serial_id(prefix: &str, s: &str) -> Option<u64> {
    s.strip_prefix(prefix)?
        .strip_prefix('-')
        .and_then(parse_u64)
}

// ---------------------------------------------------------------------------------------------
// Strict JSON

/// Parses one JSON value, refusing duplicate object keys at any depth, invalid UTF-8,
/// non-finite numbers and trailing data. Nesting depth is bounded by serde_json's recursion
/// limit (128).
pub fn parse_json_strict(bytes: &[u8]) -> Result<Value, WireError> {
    if std::str::from_utf8(bytes).is_err() {
        return err("invalid UTF-8");
    }
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = StrictSeed
        .deserialize(&mut de)
        .map_err(|e| WireError(format!("invalid JSON: {e}")))?;
    de.end()
        .map_err(|e| WireError(format!("invalid JSON: {e}")))?;
    Ok(value)
}

struct StrictSeed;

impl<'de> DeserializeSeed<'de> for StrictSeed {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(StrictSeed)? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key {key:?}")));
            }
            let v = map.next_value_seed(StrictSeed)?;
            out.insert(key, v);
        }
        Ok(Value::Object(out))
    }
}

// ---------------------------------------------------------------------------------------------
// Exact-field object reader

/// Reads the fields of one JSON object, then refuses any it did not read.
pub struct Fields<'a> {
    map: &'a Map<String, Value>,
    seen: HashSet<&'static str>,
    what: &'static str,
}

impl<'a> Fields<'a> {
    pub fn new(v: &'a Value, what: &'static str) -> Result<Fields<'a>, WireError> {
        match v {
            Value::Object(map) => Ok(Fields::of(map, what)),
            _ => err(format!("{what} must be an object")),
        }
    }

    pub fn of(map: &'a Map<String, Value>, what: &'static str) -> Fields<'a> {
        Fields {
            map,
            seen: HashSet::new(),
            what,
        }
    }

    pub fn value(&mut self, key: &'static str) -> Result<&'a Value, WireError> {
        self.seen.insert(key);
        match self.map.get(key) {
            Some(v) => Ok(v),
            None => err(format!("{}: missing field {key:?}", self.what)),
        }
    }

    pub fn string(&mut self, key: &'static str) -> Result<&'a str, WireError> {
        let what = self.what;
        self.value(key)?
            .as_str()
            .ok_or_else(|| WireError(format!("{what}: {key} must be a string")))
    }

    fn checked(
        &mut self,
        key: &'static str,
        ok: fn(&str) -> bool,
        kind: &str,
    ) -> Result<String, WireError> {
        let s = self.string(key)?;
        if ok(s) {
            Ok(s.to_owned())
        } else {
            err(format!("{}: {key} is not a valid {kind}", self.what))
        }
    }

    pub fn id(&mut self, key: &'static str) -> Result<String, WireError> {
        self.checked(key, is_id, "id")
    }

    pub fn nullable_id(&mut self, key: &'static str) -> Result<Option<String>, WireError> {
        match self.value(key)? {
            Value::Null => Ok(None),
            _ => self.id(key).map(Some),
        }
    }

    pub fn name(&mut self, key: &'static str) -> Result<String, WireError> {
        self.checked(key, is_name, "name")
    }

    pub fn method(&mut self, key: &'static str) -> Result<String, WireError> {
        self.checked(key, is_method, "method")
    }

    pub fn u64_string(&mut self, key: &'static str) -> Result<u64, WireError> {
        let s = self.string(key)?;
        parse_u64(s).ok_or_else(|| {
            WireError(format!(
                "{}: {key} is not a canonical U64 string",
                self.what
            ))
        })
    }

    /// A JSON integer in `lo..=hi`.
    pub fn int(&mut self, key: &'static str, lo: u64, hi: u64) -> Result<u64, WireError> {
        let what = self.what;
        match self.value(key)?.as_u64() {
            Some(n) if (lo..=hi).contains(&n) => Ok(n),
            _ => err(format!("{what}: {key} must be an integer in {lo}..={hi}")),
        }
    }

    pub fn boolean(&mut self, key: &'static str) -> Result<bool, WireError> {
        let what = self.what;
        self.value(key)?
            .as_bool()
            .ok_or_else(|| WireError(format!("{what}: {key} must be a boolean")))
    }

    pub fn object(&mut self, key: &'static str) -> Result<&'a Map<String, Value>, WireError> {
        let what = self.what;
        self.value(key)?
            .as_object()
            .ok_or_else(|| WireError(format!("{what}: {key} must be an object")))
    }

    pub fn array(
        &mut self,
        key: &'static str,
        lo: usize,
        hi: usize,
    ) -> Result<&'a Vec<Value>, WireError> {
        let what = self.what;
        match self.value(key)?.as_array() {
            Some(a) if (lo..=hi).contains(&a.len()) => Ok(a),
            _ => err(format!(
                "{what}: {key} must be an array of {lo}..={hi} items"
            )),
        }
    }

    /// Refuses fields that were not read.
    pub fn finish(self) -> Result<(), WireError> {
        if let Some(extra) = self.map.keys().find(|k| !self.seen.contains(k.as_str())) {
            return err(format!("{}: unknown field {extra:?}", self.what));
        }
        Ok(())
    }
}

/// An array of ids, each `<prefix>-<U64>`, 1..=64 of them.
pub fn id_batch(v: &[Value], what: &str) -> Result<Vec<String>, WireError> {
    let mut out = Vec::with_capacity(v.len());
    for item in v {
        match item.as_str() {
            Some(s) if is_id(s) => out.push(s.to_owned()),
            _ => return err(format!("{what}: every entry must be an id")),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Shared structures

/// An immutable artifact's identity. Not an address and not authority to read.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArtifactRef {
    pub store_id: String,
    pub artifact_id: String,
    pub generation: u64,
    pub byte_length: u64,
    pub content_type: String,
    pub digest: Option<String>,
}

impl ArtifactRef {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("storeId".into(), self.store_id.clone().into());
        m.insert("artifactId".into(), self.artifact_id.clone().into());
        m.insert("generation".into(), self.generation.to_string().into());
        m.insert("byteLength".into(), self.byte_length.to_string().into());
        m.insert("contentType".into(), self.content_type.clone().into());
        m.insert(
            "digest".into(),
            self.digest.clone().map_or(Value::Null, Value::String),
        );
        Value::Object(m)
    }

    pub fn from_json(v: &Value) -> Result<ArtifactRef, WireError> {
        let mut f = Fields::new(v, "ref")?;
        let store_id = f.id("storeId")?;
        let artifact_id = f.id("artifactId")?;
        let generation = f.u64_string("generation")?;
        let byte_length = f.u64_string("byteLength")?;
        let content_type = f.string("contentType")?;
        if !is_content_type(content_type) {
            return err("ref: contentType must be 1..=127 printable ASCII characters");
        }
        let digest = match f.value("digest")? {
            Value::Null => None,
            Value::String(s) if is_digest(s) => Some(s.clone()),
            _ => return err("ref: digest must be null or 64 lowercase hex digits"),
        };
        f.finish()?;
        Ok(ArtifactRef {
            store_id,
            artifact_id,
            generation,
            byte_length,
            content_type: content_type.to_owned(),
            digest,
        })
    }
}

/// One attachment entry: an application name, the reference and the sender's owner token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub reference: ArtifactRef,
    pub owner_id: String,
}

impl Attachment {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), self.name.clone().into());
        m.insert("ref".into(), self.reference.to_json());
        m.insert("ownerId".into(), self.owner_id.clone().into());
        Value::Object(m)
    }

    pub fn from_json(v: &Value) -> Result<Attachment, WireError> {
        let mut f = Fields::new(v, "attachment")?;
        let name = f.id("name")?;
        let reference = ArtifactRef::from_json(f.value("ref")?)?;
        let owner_id = f.id("ownerId")?;
        f.finish()?;
        Ok(Attachment {
            name,
            reference,
            owner_id,
        })
    }
}

/// A store location grant: a path relative to `<store root>/<storeId>`. SDK-private.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub store_id: String,
    pub relative_path: String,
}

impl Location {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("storeId".into(), self.store_id.clone().into());
        m.insert("relativePath".into(), self.relative_path.clone().into());
        Value::Object(m)
    }

    pub fn from_json(v: &Value) -> Result<Location, WireError> {
        let mut f = Fields::new(v, "location")?;
        let store_id = f.id("storeId")?;
        let relative_path = f.string("relativePath")?.to_owned();
        f.finish()?;
        Ok(Location {
            store_id,
            relative_path,
        })
    }
}

/// A participant identity as the router reports it on deliveries. It is authenticated only
/// when the connection was accepted through a launcher-bound transport entry point.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    pub client_id: String,
    pub client_incarnation: String,
}

impl Identity {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("clientId".into(), self.client_id.clone().into());
        m.insert(
            "clientIncarnation".into(),
            self.client_incarnation.clone().into(),
        );
        Value::Object(m)
    }

    pub fn from_json(v: &Value) -> Result<Identity, WireError> {
        let mut f = Fields::new(v, "identity")?;
        let client_id = f.id("clientId")?;
        let client_incarnation = f.id("clientIncarnation")?;
        f.finish()?;
        Ok(Identity {
            client_id,
            client_incarnation,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Envelope

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Command,
    Reply,
    Delivery,
    Notice,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Reply => "reply",
            Kind::Delivery => "delivery",
            Kind::Notice => "notice",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        match s {
            "command" => Some(Kind::Command),
            "reply" => Some(Kind::Reply),
            "delivery" => Some(Kind::Delivery),
            "notice" => Some(Kind::Notice),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub major: u64,
    pub minor: u64,
    pub id: String,
    pub reply_to: Option<String>,
    pub kind: Kind,
    pub op: String,
    pub body: Map<String, Value>,
    pub attachments: Vec<Attachment>,
}

impl Envelope {
    pub fn new(id: String, kind: Kind, op: &str, body: Map<String, Value>) -> Envelope {
        Envelope {
            major: MAJOR,
            minor: MINOR,
            id,
            reply_to: None,
            kind,
            op: op.to_owned(),
            body,
            attachments: Vec::new(),
        }
    }

    /// Decodes and validates one frame's bytes.
    pub fn decode(bytes: &[u8]) -> Result<Envelope, WireError> {
        if bytes.is_empty() {
            return err("empty envelope");
        }
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return err(format!(
                "envelope of {} bytes exceeds {MAX_ENVELOPE_BYTES}",
                bytes.len()
            ));
        }
        let v = parse_json_strict(bytes)?;
        let mut f = Fields::new(&v, "envelope")?;
        if f.string("protocol")? != PROTOCOL {
            return err("envelope: protocol must be \"flybus\"");
        }
        let major = f.int("major", 0, MAX_CREDIT)?;
        let minor = f.int("minor", 0, MAX_CREDIT)?;
        let id = f.id("id")?;
        let reply_to = f.nullable_id("replyTo")?;
        let kind = Kind::parse(f.string("kind")?)
            .ok_or_else(|| WireError("envelope: unknown kind".into()))?;
        let op = f.string("op")?;
        if !is_op(op) {
            return err("envelope: op must be 1..=64 of [a-z.]");
        }
        let op = op.to_owned();
        let body = f.object("body")?.clone();
        let raw = f.array("attachments", 0, MAX_ATTACHMENTS)?;
        let mut attachments = Vec::with_capacity(raw.len());
        let mut names = HashSet::new();
        for a in raw {
            let a = Attachment::from_json(a)?;
            if !names.insert(a.name.clone()) {
                return err(format!("envelope: duplicate attachment name {:?}", a.name));
            }
            attachments.push(a);
        }
        f.finish()?;
        Ok(Envelope {
            major,
            minor,
            id,
            reply_to,
            kind,
            op,
            body,
            attachments,
        })
    }

    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("protocol".into(), PROTOCOL.into());
        m.insert("major".into(), self.major.into());
        m.insert("minor".into(), self.minor.into());
        m.insert("id".into(), self.id.clone().into());
        m.insert(
            "replyTo".into(),
            self.reply_to.clone().map_or(Value::Null, Value::String),
        );
        m.insert("kind".into(), self.kind.as_str().into());
        m.insert("op".into(), self.op.clone().into());
        m.insert("body".into(), Value::Object(self.body.clone()));
        m.insert(
            "attachments".into(),
            Value::Array(self.attachments.iter().map(Attachment::to_json).collect()),
        );
        Value::Object(m)
    }

    /// Serializes, refusing anything over [`MAX_ENVELOPE_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        let bytes = serde_json::to_vec(&self.to_value()).map_err(|e| WireError(e.to_string()))?;
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return err(format!(
                "envelope of {} bytes exceeds {MAX_ENVELOPE_BYTES}",
                bytes.len()
            ));
        }
        Ok(bytes)
    }
}

// ---------------------------------------------------------------------------------------------
// Framing

#[derive(Debug)]
pub enum FrameError {
    Io(std::io::Error),
    /// A zero length prefix.
    Empty,
    /// The length prefix exceeds the limit; nothing was allocated for it.
    TooLarge(u32),
    /// End of stream inside a frame.
    Truncated,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "I/O error: {e}"),
            FrameError::Empty => f.write_str("zero-length frame"),
            FrameError::TooLarge(n) => write!(f, "frame of {n} bytes exceeds {MAX_ENVELOPE_BYTES}"),
            FrameError::Truncated => f.write_str("stream ended inside a frame"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Reads one frame. `Ok(None)` is a clean end of stream at a frame boundary. The length is
/// checked against [`MAX_ENVELOPE_BYTES`] before any buffer is allocated.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut len[got..]).await.map_err(FrameError::Io)?;
        if n == 0 {
            return if got == 0 {
                Ok(None)
            } else {
                Err(FrameError::Truncated)
            };
        }
        got += n;
    }
    let len = u32::from_le_bytes(len);
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len as usize > MAX_ENVELOPE_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => FrameError::Truncated,
        _ => FrameError::Io(e),
    })?;
    Ok(Some(buf))
}

/// Writes one frame: the length prefix and the bytes, as one buffer.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    debug_assert!(!bytes.is_empty() && bytes.len() <= MAX_ENVELOPE_BYTES);
    let mut buf = Vec::with_capacity(bytes.len() + 4);
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
    w.write_all(&buf).await?;
    w.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars() {
        assert!(is_id("a") && is_id("msg-7") && is_id("0.a_b-c"));
        assert!(!is_id("") && !is_id("-a") && !is_id("A") && !is_id(&"a".repeat(65)));
        assert!(is_id(&"a".repeat(64)));
        assert_eq!(parse_u64("0"), Some(0));
        assert_eq!(parse_u64("18446744073709551615"), Some(u64::MAX));
        for bad in ["", "01", "18446744073709551616", "-1", "1.0", "+1", " 1"] {
            assert_eq!(parse_u64(bad), None, "{bad:?}");
        }
        assert!(is_name("session.demo.snapshots") && is_name(&"a".repeat(192)));
        for bad in ["", ".a", "a.", "a..b", "A", "a/b", &"a".repeat(193)] {
            assert!(!is_name(bad), "{bad:?}");
        }
        assert!(is_digest(&"0f".repeat(32)) && !is_digest(&"0F".repeat(32)) && !is_digest("00"));
        assert_eq!(parse_serial_id("msg", "msg-12"), Some(12));
        assert_eq!(parse_serial_id("msg", "msg-012"), None);
        assert_eq!(parse_serial_id("msg", "msgx-1"), None);
        assert!(is_method("Counter.Increment") && !is_method("a\u{7f}") && !is_method(""));
    }

    #[test]
    fn strict_json() {
        assert!(parse_json_strict(br#"{"a":{"b":[{"c":1,"d":2}]}}"#).is_ok());
        for bad in [
            &br#"{"a":1,"a":2}"#[..],
            br#"{"a":{"b":[{"c":1,"c":2}]}}"#,
            br#"[{"x":1},{"y":{"z":1,"z":1}}]"#,
            br#"{"a":NaN}"#,
            br#"{"a":Infinity}"#,
            br#"{"a":1e400}"#,
            br#"{"a":"\ud800"}"#,
            br#"{"a":1} x"#,
            b"{\"a\":\"\xff\"}",
        ] {
            assert!(
                parse_json_strict(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert!(parse_json_strict(deep.as_bytes()).is_err());
    }
}
