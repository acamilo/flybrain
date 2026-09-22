//! Loading the crate's `fixtures/` directory.
//!
//! The same files are read by the Rust tests and by `packages/session-types`, so a case only
//! has to be written once to hold both languages to it.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::canonical;
use crate::scalar::{Result, err, wire_err};

/// The crate's `fixtures/` directory.
pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Reads one fixture file, parsed strictly.
pub fn load(name: &str) -> Result<Value> {
    let path = dir().join(name);
    let bytes =
        std::fs::read(&path).map_err(|e| wire_err(format!("fixture {}: {e}", path.display())))?;
    canonical::parse_strict(&bytes)
}

/// Reads one fixture file as raw bytes, for the cases that are deliberately not valid JSON.
pub fn load_bytes(name: &str) -> Result<Vec<u8>> {
    let path = dir().join(name);
    std::fs::read(&path).map_err(|e| wire_err(format!("fixture {}: {e}", path.display())))
}

/// The `cases` array of a fixture file.
pub fn cases(file: &Value) -> Result<&Vec<Value>> {
    match file.get("cases").and_then(Value::as_array) {
        Some(cases) if !cases.is_empty() => Ok(cases),
        _ => err("fixture: cases must be a nonempty array"),
    }
}

/// A string field of one case.
pub fn field<'a>(case: &'a Value, key: &str) -> Result<&'a str> {
    case.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| wire_err(format!("fixture case: missing string field {key:?}")))
}

/// Decodes the `base64` field of a case that carries raw bytes.
pub fn base64(case: &Value, key: &str) -> Result<Vec<u8>> {
    decode_base64(field(case, key)?)
}

/// Standard base64 with padding. Small and local: the crate has no base64 dependency and the
/// fixtures only carry a few hundred bytes.
pub fn decode_base64(text: &str) -> Result<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return err("base64: length must be a multiple of 4");
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for quad in bytes.chunks_exact(4) {
        let mut buffer = 0u32;
        let mut keep = 3;
        for (index, byte) in quad.iter().enumerate() {
            let value = if *byte == b'=' {
                if index < 2 {
                    return err("base64: misplaced padding");
                }
                keep -= 1;
                0
            } else {
                ALPHABET
                    .iter()
                    .position(|c| c == byte)
                    .ok_or_else(|| wire_err("base64: invalid character"))? as u32
            };
            buffer = (buffer << 6) | value;
        }
        let triple = buffer.to_be_bytes();
        out.extend_from_slice(&triple[1..1 + keep]);
    }
    Ok(out)
}

/// Standard base64 with padding, for generating fixtures.
pub fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
        let indexes = [
            (value >> 18) & 0x3f,
            (value >> 12) & 0x3f,
            (value >> 6) & 0x3f,
            value & 0x3f,
        ];
        for (position, index) in indexes.iter().enumerate() {
            if position <= chunk.len() {
                out.push(ALPHABET[*index as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
