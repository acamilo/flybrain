//! `FLYSESS1`: the envelope layout of `docs/design/session-framework/checkpoint-envelope-v1.md`.
//!
//! This is the layout half of the specification, not the store: it lays out a header, a
//! canonical-JSON manifest, a payload table and the payload bytes, and it reads one back.
//! Writing generations, fsyncing and committing a manifest belong to the STATE-01 store slice.
//! `FLYSIM01` is a different format with a different magic and is not touched by any of this.

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::canonical;
use crate::scalar::{Result, err, is_id};

/// Envelope magic. Eight ASCII bytes, distinct from `FLYSIM01`.
pub const MAGIC: &[u8; 8] = b"FLYSESS1";
/// Footer magic, so a truncated file cannot look complete.
pub const FOOTER_MAGIC: &[u8; 8] = b"FLYSESSF";
/// Envelope version, in the header and in the manifest.
pub const VERSION: u32 = 1;
/// Fixed header size in bytes.
pub const HEADER_BYTES: usize = 32;
/// One payload table entry: a 64-byte name field, offset, length and a 32-byte digest.
pub const TABLE_ENTRY_BYTES: usize = 112;
/// Payload name field width.
pub const NAME_BYTES: usize = 64;
/// Footer size in bytes: total length, whole-prefix digest and the footer magic.
pub const FOOTER_BYTES: usize = 48;
/// Payloads start on an eight-byte boundary.
pub const ALIGNMENT: u64 = 8;
/// Payloads per envelope.
pub const MAX_PAYLOADS: usize = 64;

/// One payload's table entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadEntry {
    /// An `Id`: the new envelope widens the historical letters-only chunk name deliberately,
    /// which is why it is a new version and not an extension of `FLYSIM01`.
    pub name: String,
    pub offset: u64,
    pub byte_length: u64,
    /// SHA-256 of exactly `byte_length` bytes at `offset`.
    pub digest: [u8; 32],
}

/// A laid-out envelope: where everything is, before any bytes are written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub manifest_offset: u64,
    pub manifest_bytes: u32,
    pub table_offset: u64,
    pub entries: Vec<PayloadEntry>,
    pub footer_offset: u64,
    pub total_bytes: u64,
}

fn align_up(value: u64) -> u64 {
    value.div_ceil(ALIGNMENT) * ALIGNMENT
}

/// Lays out the envelope for one manifest and a list of `(name, bytes)` payloads.
pub fn layout(manifest: &Value, payloads: &[(String, Vec<u8>)]) -> Result<Layout> {
    if payloads.len() > MAX_PAYLOADS {
        return err("checkpoint envelope: at most 64 payloads");
    }
    crate::scalar::require_unique(
        payloads.iter().map(|(name, _)| name.as_str()),
        "checkpoint envelope: payload names",
    )?;
    for (name, _) in payloads {
        if !is_id(name) {
            return err(format!(
                "checkpoint envelope: payload name {name:?} is not an Id"
            ));
        }
    }
    let manifest_text = canonical::canonicalize(manifest)?;
    let manifest_bytes = u32::try_from(manifest_text.len())
        .map_err(|_| crate::scalar::wire_err("checkpoint envelope: manifest is too large"))?;
    let manifest_offset = HEADER_BYTES as u64;
    let table_offset = align_up(manifest_offset + u64::from(manifest_bytes));
    let mut offset = align_up(table_offset + (payloads.len() * TABLE_ENTRY_BYTES) as u64);
    let mut entries = Vec::with_capacity(payloads.len());
    for (name, bytes) in payloads {
        entries.push(PayloadEntry {
            name: name.clone(),
            offset,
            byte_length: bytes.len() as u64,
            digest: Sha256::digest(bytes).into(),
        });
        offset = align_up(offset + bytes.len() as u64);
    }
    Ok(Layout {
        manifest_offset,
        manifest_bytes,
        table_offset,
        entries,
        footer_offset: offset,
        total_bytes: offset + FOOTER_BYTES as u64,
    })
}

/// Writes one envelope: header, manifest, payload table, payloads, footer.
pub fn encode(manifest: &Value, payloads: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let layout = layout(manifest, payloads)?;
    let manifest_text = canonical::canonicalize(manifest)?;
    let mut out = vec![0u8; layout.footer_offset as usize];
    out[0..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12..16].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
    out[16..20].copy_from_slice(&layout.manifest_bytes.to_le_bytes());
    out[20..24].copy_from_slice(&(payloads.len() as u32).to_le_bytes());
    out[24..28].copy_from_slice(&(layout.table_offset as u32).to_le_bytes());
    out[28..32].copy_from_slice(&0u32.to_le_bytes());
    let manifest_start = layout.manifest_offset as usize;
    out[manifest_start..manifest_start + manifest_text.len()]
        .copy_from_slice(manifest_text.as_bytes());
    for (index, entry) in layout.entries.iter().enumerate() {
        let base = layout.table_offset as usize + index * TABLE_ENTRY_BYTES;
        out[base..base + entry.name.len()].copy_from_slice(entry.name.as_bytes());
        let numbers = base + NAME_BYTES;
        out[numbers..numbers + 8].copy_from_slice(&entry.offset.to_le_bytes());
        out[numbers + 8..numbers + 16].copy_from_slice(&entry.byte_length.to_le_bytes());
        out[numbers + 16..numbers + 48].copy_from_slice(&entry.digest);
    }
    for (entry, (_, bytes)) in layout.entries.iter().zip(payloads) {
        let start = entry.offset as usize;
        out[start..start + bytes.len()].copy_from_slice(bytes);
    }
    let digest: [u8; 32] = Sha256::digest(&out).into();
    out.extend_from_slice(&layout.total_bytes.to_le_bytes());
    out.extend_from_slice(&digest);
    out.extend_from_slice(FOOTER_MAGIC);
    Ok(out)
}

/// A decoded envelope.
#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub manifest: Value,
    pub payloads: Vec<(String, Vec<u8>)>,
    pub layout: Layout,
}

impl Envelope {
    pub fn payload(&self, name: &str) -> Option<&[u8]> {
        self.payloads
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, bytes)| bytes.as_slice())
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(buf)
}

/// Reads and fully validates one envelope: magic, version, footer digest, table ordering,
/// alignment, bounds and every payload digest.
pub fn decode(bytes: &[u8]) -> Result<Envelope> {
    if bytes.len() < HEADER_BYTES + FOOTER_BYTES {
        return err("checkpoint envelope: shorter than a header plus a footer");
    }
    if &bytes[0..8] != MAGIC {
        return err("checkpoint envelope: wrong magic (FLYSIM01 is a different format)");
    }
    if u32_at(bytes, 8) != VERSION {
        return err("checkpoint envelope: unsupported version");
    }
    if u32_at(bytes, 12) as usize != HEADER_BYTES {
        return err("checkpoint envelope: headerBytes must be 32");
    }
    if u32_at(bytes, 28) != 0 {
        return err("checkpoint envelope: reserved header word must be zero");
    }
    let manifest_bytes = u32_at(bytes, 16) as usize;
    let payload_count = u32_at(bytes, 20) as usize;
    let table_offset = u32_at(bytes, 24) as u64;
    if payload_count > MAX_PAYLOADS {
        return err("checkpoint envelope: at most 64 payloads");
    }
    let footer_offset = bytes.len() - FOOTER_BYTES;
    if &bytes[footer_offset + 40..] != FOOTER_MAGIC {
        return err("checkpoint envelope: missing footer magic");
    }
    if u64_at(bytes, footer_offset) != bytes.len() as u64 {
        return err("checkpoint envelope: footer length does not match the file");
    }
    let recorded = &bytes[footer_offset + 8..footer_offset + 40];
    let computed: [u8; 32] = Sha256::digest(&bytes[..footer_offset]).into();
    if recorded != computed {
        return err("checkpoint envelope: footer digest does not match the contents");
    }
    let manifest_start = HEADER_BYTES;
    let manifest_end = manifest_start + manifest_bytes;
    if manifest_end > footer_offset {
        return err("checkpoint envelope: manifest runs past the payload area");
    }
    let manifest = canonical::parse_strict(&bytes[manifest_start..manifest_end])?;
    let canonical_manifest = canonical::canonicalize(&manifest)?;
    if canonical_manifest.as_bytes() != &bytes[manifest_start..manifest_end] {
        return err("checkpoint envelope: the manifest is not canonical JSON");
    }
    if table_offset != align_up(manifest_end as u64) {
        return err("checkpoint envelope: the payload table is not at its laid-out offset");
    }
    let table_end = table_offset as usize + payload_count * TABLE_ENTRY_BYTES;
    if table_end > footer_offset {
        return err("checkpoint envelope: the payload table runs past the payload area");
    }
    let mut entries = Vec::with_capacity(payload_count);
    let mut payloads = Vec::with_capacity(payload_count);
    let mut previous_end = align_up(table_end as u64);
    for index in 0..payload_count {
        let base = table_offset as usize + index * TABLE_ENTRY_BYTES;
        let name_field = &bytes[base..base + NAME_BYTES];
        let length = name_field
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(NAME_BYTES);
        if name_field[length..].iter().any(|b| *b != 0) {
            return err("checkpoint envelope: a payload name has bytes after its terminator");
        }
        let name = std::str::from_utf8(&name_field[..length])
            .map_err(|_| crate::scalar::wire_err("checkpoint envelope: payload name is not UTF-8"))?
            .to_owned();
        if !is_id(&name) {
            return err(format!(
                "checkpoint envelope: payload name {name:?} is not an Id"
            ));
        }
        let numbers = base + NAME_BYTES;
        let offset = u64_at(bytes, numbers);
        let byte_length = u64_at(bytes, numbers + 8);
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&bytes[numbers + 16..numbers + 48]);
        if offset != previous_end {
            return err(format!(
                "checkpoint envelope: payload {name:?} starts at {offset}, not at its aligned {previous_end}"
            ));
        }
        let end = offset
            .checked_add(byte_length)
            .ok_or_else(|| crate::scalar::wire_err("checkpoint envelope: payload overflows"))?;
        if end > footer_offset as u64 {
            return err(format!(
                "checkpoint envelope: payload {name:?} runs past the payload area"
            ));
        }
        let payload = bytes[offset as usize..end as usize].to_vec();
        let computed: [u8; 32] = Sha256::digest(&payload).into();
        if computed != digest {
            return err(format!(
                "checkpoint envelope: payload {name:?} fails its digest"
            ));
        }
        previous_end = align_up(end);
        entries.push(PayloadEntry {
            name: name.clone(),
            offset,
            byte_length,
            digest,
        });
        payloads.push((name, payload));
    }
    crate::scalar::require_unique(
        entries.iter().map(|e| e.name.as_str()),
        "checkpoint envelope: payload names",
    )?;
    if previous_end != footer_offset as u64 {
        return err("checkpoint envelope: padding between the last payload and the footer");
    }
    Ok(Envelope {
        manifest,
        layout: Layout {
            manifest_offset: manifest_start as u64,
            manifest_bytes: manifest_bytes as u32,
            table_offset,
            entries,
            footer_offset: footer_offset as u64,
            total_bytes: bytes.len() as u64,
        },
        payloads,
    })
}

/// The manifest fields state-media-v1 section 4 requires, checked as a set: a manifest that
/// omits one of them is not a complete checkpoint.
pub const REQUIRED_MANIFEST_FIELDS: &[&str] = &[
    "envelopeVersion",
    "checkpointId",
    "sourceScope",
    "episodeId",
    "worldTime",
    "schedulerId",
    "compositionDigest",
    "portMap",
    "compatibility",
    "agents",
    "coordinator",
    "payloads",
];

/// Checks the manifest's required field set and that its payload table mirrors the envelope's.
pub fn validate_manifest(envelope: &Envelope) -> Result<()> {
    let map = envelope
        .manifest
        .as_object()
        .ok_or_else(|| crate::scalar::wire_err("checkpoint manifest: must be an object"))?;
    for field in REQUIRED_MANIFEST_FIELDS {
        if !map.contains_key(*field) {
            return err(format!("checkpoint manifest: missing {field:?}"));
        }
    }
    if map.get("envelopeVersion").and_then(Value::as_u64) != Some(u64::from(VERSION)) {
        return err("checkpoint manifest: envelopeVersion must be 1");
    }
    let listed = map
        .get("payloads")
        .and_then(Value::as_array)
        .ok_or_else(|| crate::scalar::wire_err("checkpoint manifest: payloads must be an array"))?;
    if listed.len() != envelope.layout.entries.len() {
        return err("checkpoint manifest: payloads does not match the payload table");
    }
    for (declared, entry) in listed.iter().zip(&envelope.layout.entries) {
        let name = declared.get("name").and_then(Value::as_str);
        let length = declared
            .get("byteLength")
            .and_then(Value::as_str)
            .and_then(crate::scalar::parse_u64);
        let digest = declared.get("digest").and_then(Value::as_str);
        if name != Some(entry.name.as_str()) {
            return err("checkpoint manifest: payload name does not match the table");
        }
        if length != Some(entry.byte_length) {
            return err(format!(
                "checkpoint manifest: payload {:?} byteLength does not match the table",
                entry.name
            ));
        }
        if digest != Some(hex(&entry.digest).as_str()) {
            return err(format!(
                "checkpoint manifest: payload {:?} digest does not match the table",
                entry.name
            ));
        }
    }
    Ok(())
}

/// Lowercase hex of a raw digest, the form the manifest records.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
