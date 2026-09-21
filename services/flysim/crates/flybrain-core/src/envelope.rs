//! Checkpoint envelope: a JSON manifest plus named binary chunks, checksummed.
//!
//! Ports `agent/envelope.ts`. The layout is frozen (files written by earlier versions of this
//! format must keep loading):
//!
//! ```text
//!   magic                       ASCII, caller-chosen, identifies the file kind
//!   manifestLength              u32 little-endian
//!   manifest                    UTF-8 JSON, includes schemaVersion and the chunk name list
//!   [ length, bytes ] * n       u32 little-endian length then the chunk, in manifest order
//!   checksum                    u32 little-endian CRC32 over every preceding byte
//! ```

use crate::agent::AgentState;
use crate::decoder::DecoderState;
use crate::error::{bail, Error, Result};
use crate::json::JsonValue;
use crate::lif::LifState;
use crate::ordered::NumberMap;
use crate::plasticity::PlasticityState;

/// Envelope schema; bumped only for a layout change, not for manifest fields.
pub const ENVELOPE_SCHEMA_VERSION: f64 = 2.0;

/// CRC32 (IEEE 802.3, reflected polynomial 0xedb88320) over `bytes`.
///
/// Detects accidental corruption of the manifest and every payload byte. Not a signature.
pub fn checksum(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    crc ^ 0xffff_ffff
}

/// A decoded envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvelopeParts {
    pub manifest: JsonValue,
    /// Chunks in manifest order.
    pub chunks: Vec<(String, Vec<u8>)>,
}

impl EnvelopeParts {
    pub fn chunk(&self, name: &str) -> Option<&[u8]> {
        self.chunks
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, bytes)| bytes.as_slice())
    }
}

/// Chunk names are restricted to letters, so a manifest can never name a prototype key.
fn is_chunk_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic())
}

/// Encode `manifest` and `chunks` into one checksummed buffer.
///
/// `magic` must be ASCII; the chunk name list and `schemaVersion` are written into the manifest,
/// overriding any same-named fields.
pub fn encode_envelope(
    magic: &str,
    manifest: &JsonValue,
    chunks: &[(String, Vec<u8>)],
) -> Result<Vec<u8>> {
    if magic.is_empty() || !magic.is_ascii() {
        bail!("Envelope magic must be a non-empty ASCII string");
    }
    let names: Vec<String> = chunks.iter().map(|(name, _)| name.clone()).collect();
    if names.iter().any(|name| !is_chunk_name(name)) {
        bail!("Invalid envelope chunk name");
    }
    let mut manifest = manifest.clone();
    if !matches!(manifest, JsonValue::Object(_)) {
        manifest = JsonValue::object();
    }
    manifest.set("schemaVersion", ENVELOPE_SCHEMA_VERSION.into());
    manifest.set(
        "chunks",
        JsonValue::Array(names.iter().map(|name| name.as_str().into()).collect()),
    );
    let manifest_bytes = manifest.stringify().into_bytes();

    let mut out = Vec::new();
    out.extend_from_slice(magic.as_bytes());
    out.extend_from_slice(&(manifest_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&manifest_bytes);
    for (_, bytes) in chunks {
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    let crc = checksum(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

/// Decode a buffer written by [`encode_envelope`], rejecting anything that is not structurally
/// this format with `magic`.
pub fn decode_envelope(bytes: &[u8], magic: &str) -> Result<EnvelopeParts> {
    let magic_bytes = magic.as_bytes();
    if bytes.len() < magic_bytes.len() + 4 || !bytes.starts_with(magic_bytes) {
        bail!("Not a {magic} envelope");
    }
    let mut offset = magic_bytes.len();
    let manifest_length = read_u32(bytes, offset) as usize;
    offset += 4;
    if offset + manifest_length > bytes.len() {
        bail!("Envelope manifest is truncated");
    }
    let manifest_text = std::str::from_utf8(&bytes[offset..offset + manifest_length])
        .map_err(|_| Error::new("Envelope manifest is not valid UTF-8"))?;
    let manifest = JsonValue::parse(manifest_text)?;
    offset += manifest_length;
    let schema = manifest.get("schemaVersion").and_then(JsonValue::as_f64);
    if schema != Some(ENVELOPE_SCHEMA_VERSION) {
        bail!(
            "Unsupported envelope schema: {}",
            schema
                .map(crate::jsmath::number_to_string)
                .unwrap_or_else(|| "undefined".to_string())
        );
    }
    if bytes.len() < 4 || checksum(&bytes[..bytes.len() - 4]) != read_u32(bytes, bytes.len() - 4) {
        bail!("Envelope checksum mismatch");
    }
    let names = manifest
        .get("chunks")
        .and_then(JsonValue::as_array)
        .map(<[JsonValue]>::to_vec);
    let Some(names) = names else {
        bail!("Invalid envelope chunks");
    };
    let mut chunk_names = Vec::with_capacity(names.len());
    for name in &names {
        match name.as_str() {
            Some(name) if is_chunk_name(name) => chunk_names.push(name.to_string()),
            _ => bail!("Invalid envelope chunks"),
        }
    }
    let unique: std::collections::HashSet<&String> = chunk_names.iter().collect();
    if unique.len() != chunk_names.len() {
        bail!("Invalid envelope chunks");
    }

    let mut chunks = Vec::with_capacity(chunk_names.len());
    for name in chunk_names {
        if offset + 4 > bytes.len() {
            bail!("Envelope chunk header is truncated");
        }
        let length = read_u32(bytes, offset) as usize;
        offset += 4;
        if offset + length > bytes.len() {
            bail!("Envelope chunk {name} is truncated");
        }
        chunks.push((name, bytes[offset..offset + length].to_vec()));
        offset += length;
    }
    if offset != bytes.len() - 4 {
        bail!("Envelope contains trailing data");
    }
    Ok(EnvelopeParts { manifest, chunks })
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// Chunk names of the typed arrays in an agent checkpoint, in write order.
pub const AGENT_CHUNK_NAMES: [&str; 7] = [
    "membrane",
    "refractory",
    "lastSpikeMs",
    "visualDrive",
    "plasticGains",
    "plasticTraces",
    "plasticTouched",
];

/// The scalar half of an [`AgentState`] plus its chunks, ready for [`encode_envelope`].
pub struct AgentChunks {
    pub manifest: JsonValue,
    pub chunks: Vec<(String, Vec<u8>)>,
}

/// Split an agent state into the manifest and chunks [`encode_envelope`] takes.
///
/// Chunk names and manifest key order are the original checkpoint's, so a host that already
/// writes those names keeps its file format.
pub fn agent_to_chunks(state: &AgentState) -> AgentChunks {
    let network = &state.network;
    let mut manifest = JsonValue::object();
    manifest.set("agentVersion", 1.0.into());
    manifest.set("remainder", state.remainder.into());
    manifest.set("warmedUp", state.warmed_up.into());

    let mut network_json = JsonValue::object();
    network_json.set("rng", network.rng.into());
    network_json.set("rewardRemaining", network.reward_remaining.into());
    network_json.set("ms", network.ms.into());
    network_json.set("populationRate", network.population_rate.into());
    network_json.set("rates", network.rates.to_json());
    manifest.set("network", network_json);

    manifest.set("decoder", decoder_state_to_json(&state.decoder));

    let mut plasticity = JsonValue::object();
    plasticity.set("version", network.plasticity.version.as_str().into());
    plasticity.set("topology", f64::from(network.plasticity.topology).into());
    plasticity.set("enabled", network.plasticity.enabled.into());
    plasticity.set("updates", network.plasticity.updates.into());
    plasticity.set("signal", network.plasticity.signal.into());
    manifest.set("plasticity", plasticity);

    let chunks = vec![
        ("membrane".to_string(), f32_bytes(&network.membrane)),
        ("refractory".to_string(), network.refractory.clone()),
        ("lastSpikeMs".to_string(), f64_bytes(&network.last_spike_ms)),
        ("visualDrive".to_string(), f32_bytes(&network.visual_drive)),
        (
            "plasticGains".to_string(),
            f32_bytes(&network.plasticity.gains),
        ),
        (
            "plasticTraces".to_string(),
            f32_bytes(&network.plasticity.traces),
        ),
        (
            "plasticTouched".to_string(),
            f64_bytes(&network.plasticity.touched),
        ),
    ];
    AgentChunks { manifest, chunks }
}

/// Rebuild an agent state from a decoded envelope.
///
/// Only presence, alignment and the manifest shape are checked here; the values themselves are
/// validated by [`crate::agent::NeuralAgent::import_state`], which is also what makes a rejected
/// checkpoint a no-op.
pub fn agent_from_chunks(manifest: &JsonValue, parts: &EnvelopeParts) -> Result<AgentState> {
    if manifest.get("agentVersion").and_then(JsonValue::as_f64) != Some(1.0) {
        bail!("Unsupported agent checkpoint version");
    }
    let network = manifest.get("network");
    let decoder = manifest.get("decoder");
    let plasticity = manifest.get("plasticity");
    let (Some(network), Some(decoder), Some(plasticity)) = (network, decoder, plasticity) else {
        bail!("Agent checkpoint manifest is incomplete");
    };
    if network.is_null() || decoder.is_null() || plasticity.is_null() {
        bail!("Agent checkpoint manifest is incomplete");
    }
    for name in AGENT_CHUNK_NAMES {
        if parts.chunk(name).is_none() {
            bail!("Checkpoint is missing {name}");
        }
    }
    let number = |value: &JsonValue, field: &str| -> Result<f64> {
        value
            .get(field)
            .and_then(JsonValue::as_f64)
            .ok_or_else(|| Error::new(format!("Agent checkpoint manifest is incomplete: {field}")))
    };

    let plasticity_state = PlasticityState {
        version: plasticity
            .get("version")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        topology: number(plasticity, "topology")? as u32,
        enabled: plasticity
            .get("enabled")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        updates: number(plasticity, "updates")?,
        signal: number(plasticity, "signal")?,
        gains: read_f32(parts, "plasticGains")?,
        traces: read_f32(parts, "plasticTraces")?,
        touched: read_f64(parts, "plasticTouched")?,
    };

    Ok(AgentState {
        version: 1,
        remainder: number(manifest, "remainder")?,
        warmed_up: manifest
            .get("warmedUp")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        network: LifState {
            membrane: read_f32(parts, "membrane")?,
            refractory: parts.chunk("refractory").unwrap_or_default().to_vec(),
            last_spike_ms: read_f64(parts, "lastSpikeMs")?,
            visual_drive: read_f32(parts, "visualDrive")?,
            rng: crate::rng::Xorshift32::to_int32(number(network, "rng")?),
            reward_remaining: number(network, "rewardRemaining")?,
            ms: number(network, "ms")?,
            population_rate: number(network, "populationRate")?,
            rates: network
                .get("rates")
                .and_then(NumberMap::from_json)
                .unwrap_or_default(),
            plasticity: plasticity_state,
        },
        decoder: decoder_state_from_json(decoder)?,
    })
}

/// A [`DecoderState`] as the manifest writes it.
pub fn decoder_state_to_json(state: &DecoderState) -> JsonValue {
    let mut json = JsonValue::object();
    json.set("version", f64::from(state.version).into());
    json.set("calibrated", state.calibrated.into());
    json.set("baseline", state.baseline.to_json());
    json.set("heldUntil", state.held_until.to_json());
    json.set("nextAllowed", state.next_allowed.to_json());
    json.set("nextDecision", state.next_decision.into());
    json.set(
        "current",
        match &state.current {
            Some(channel) => channel.as_str().into(),
            None => JsonValue::Null,
        },
    );
    json.set("fatigue", state.fatigue.to_json());
    // The macro group (`docs/design/macros.md` section 11). Always written, never required on the
    // way back in: a manifest from before the group carries none of these three and the group
    // then starts rested, which is what keeps the live checkpoint loadable across the change.
    json.set("macroNextDecision", state.macro_next_decision.into());
    json.set(
        "macroCurrent",
        match &state.macro_current {
            Some(channel) => channel.as_str().into(),
            None => JsonValue::Null,
        },
    );
    json.set("macroFatigue", state.macro_fatigue.to_json());
    json
}

/// Read a [`DecoderState`] back out of a manifest.
pub fn decoder_state_from_json(value: &JsonValue) -> Result<DecoderState> {
    let map = |field: &str| -> Result<NumberMap> {
        value
            .get(field)
            .and_then(NumberMap::from_json)
            .ok_or_else(|| Error::new("Invalid decoder checkpoint"))
    };
    Ok(DecoderState {
        version: value
            .get("version")
            .and_then(JsonValue::as_f64)
            .unwrap_or(f64::NAN) as u32,
        calibrated: value
            .get("calibrated")
            .and_then(JsonValue::as_bool)
            .ok_or_else(|| Error::new("Invalid decoder checkpoint"))?,
        baseline: map("baseline")?,
        held_until: map("heldUntil")?,
        next_allowed: map("nextAllowed")?,
        next_decision: value
            .get("nextDecision")
            .and_then(JsonValue::as_f64)
            .ok_or_else(|| Error::new("Invalid decoder checkpoint"))?,
        current: match value.get("current") {
            Some(JsonValue::String(channel)) => Some(channel.clone()),
            _ => None,
        },
        fatigue: map("fatigue")?,
        macro_next_decision: value
            .get("macroNextDecision")
            .and_then(JsonValue::as_f64)
            .unwrap_or(0.0),
        macro_current: match value.get("macroCurrent") {
            Some(JsonValue::String(channel)) => Some(channel.clone()),
            _ => None,
        },
        macro_fatigue: value
            .get("macroFatigue")
            .and_then(NumberMap::from_json)
            .unwrap_or_default(),
    })
}

/// `Zero-offset copy, so reading back an f32 array is aligned and exactly the right length.`
fn read_f32(parts: &EnvelopeParts, name: &str) -> Result<Vec<f32>> {
    let bytes = parts
        .chunk(name)
        .ok_or_else(|| Error::new(format!("Checkpoint is missing {name}")))?;
    if bytes.len() % 4 != 0 {
        bail!("Checkpoint chunk {name} has a partial element");
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn read_f64(parts: &EnvelopeParts, name: &str) -> Result<Vec<f64>> {
    let bytes = parts
        .chunk(name)
        .ok_or_else(|| Error::new(format!("Checkpoint is missing {name}")))?;
    if bytes.len() % 8 != 0 {
        bail!("Checkpoint chunk {name} has a partial element");
    }
    Ok(bytes
        .chunks_exact(8)
        .map(|chunk| {
            f64::from_le_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ])
        })
        .collect())
}

/// Raw little-endian bytes of an f32 array, as `Float32Array` holds them.
pub fn f32_bytes(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Raw little-endian bytes of an f64 array, as `Float64Array` holds them.
pub fn f64_bytes(values: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 8);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_reference() {
        assert_eq!(checksum(b""), 0);
        assert_eq!(checksum(b"a"), 0xe8b7_be43);
        assert_eq!(checksum(b"123456789"), 0xcbf4_3926);
    }
}
