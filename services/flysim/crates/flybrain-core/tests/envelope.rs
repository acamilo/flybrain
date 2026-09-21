//! Unit tests ported from the envelope half of `packages/brain/tests/agent.test.ts`.
//!
//! The envelope validates only its own structure: magic, truncation, chunk names, trailing bytes
//! and the checksum. Every one of those rejections is exercised here, because a checkpoint from
//! disk is the one input the service cannot assume is well-formed.

mod common;

use std::sync::Arc;

use common::{frame_pool, synthetic_dataset};
use flybrain_core::agent::{AgentConfig, FrameSize, NeuralAgent, RewardEvent, TickOptions};
use flybrain_core::decoder::gameboy::gameboy_decoder_config;
use flybrain_core::envelope::{
    agent_from_chunks, agent_to_chunks, checksum, decode_envelope, encode_envelope,
    AGENT_CHUNK_NAMES,
};
use flybrain_core::json::JsonValue;

const MAGIC: &str = "FLYBRAIN";

fn chunk(name: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    (name.to_string(), bytes.to_vec())
}

fn manifest(fields: &[(&str, JsonValue)]) -> JsonValue {
    let mut json = JsonValue::object();
    for (key, value) in fields {
        json.set(key, value.clone());
    }
    json
}

/// Encodes a manifest verbatim, so decode's own structural checks can be exercised.
fn encode_raw(magic: &str, manifest: &JsonValue, chunks: &[Vec<u8>]) -> Vec<u8> {
    let manifest_bytes = manifest.stringify().into_bytes();
    let mut out = Vec::new();
    out.extend_from_slice(magic.as_bytes());
    out.extend_from_slice(&(manifest_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&manifest_bytes);
    for bytes in chunks {
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    let crc = checksum(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// An independent CRC32, so the test does not trust the implementation it is checking.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xffff_ffff
}

#[test]
fn an_envelope_round_trips_its_manifest_and_chunks() {
    let chunks = vec![
        chunk("alpha", &[1, 2, 3]),
        chunk("beta", &[]),
        chunk(
            "gamma",
            &(0..300u32)
                .map(|index| (index & 0xff) as u8)
                .collect::<Vec<_>>(),
        ),
    ];
    let bytes = encode_envelope(
        MAGIC,
        &manifest(&[("note", "hello".into()), ("count", 7.0.into())]),
        &chunks,
    )
    .expect("encode");
    let decoded = decode_envelope(&bytes, MAGIC).expect("decode");

    assert_eq!(
        decoded
            .manifest
            .get("schemaVersion")
            .and_then(JsonValue::as_f64),
        Some(2.0)
    );
    assert_eq!(
        decoded.manifest.get("note").and_then(JsonValue::as_str),
        Some("hello")
    );
    assert_eq!(
        decoded.manifest.get("count").and_then(JsonValue::as_f64),
        Some(7.0)
    );
    assert_eq!(
        decoded
            .manifest
            .get("chunks")
            .and_then(JsonValue::as_array)
            .map(|names| names
                .iter()
                .filter_map(JsonValue::as_str)
                .collect::<Vec<_>>()),
        Some(vec!["alpha", "beta", "gamma"])
    );
    assert_eq!(decoded.chunks, chunks);

    // The footer is a CRC32 over everything before it.
    let footer = u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap());
    assert_eq!(footer, crc32(&bytes[..bytes.len() - 4]));
    assert_eq!(&bytes[..8], MAGIC.as_bytes());
}

#[test]
fn an_envelope_rejects_a_foreign_magic_a_bad_schema_and_a_bad_chunk_name() {
    let bytes =
        encode_envelope(MAGIC, &JsonValue::object(), &[chunk("alpha", &[9])]).expect("encode");
    assert_eq!(
        decode_envelope(&bytes, "OTHERMAG").unwrap_err().message(),
        "Not a OTHERMAG envelope"
    );
    assert_eq!(
        decode_envelope(&[0, 0, 0], MAGIC).unwrap_err().message(),
        "Not a FLYBRAIN envelope"
    );
    assert_eq!(
        encode_envelope("", &JsonValue::object(), &[])
            .unwrap_err()
            .message(),
        "Envelope magic must be a non-empty ASCII string"
    );
    assert_eq!(
        encode_envelope(MAGIC, &JsonValue::object(), &[chunk("bad-name", &[1])])
            .unwrap_err()
            .message(),
        "Invalid envelope chunk name"
    );

    let schema_one = encode_raw(
        MAGIC,
        &manifest(&[
            ("schemaVersion", 1.0.into()),
            ("chunks", JsonValue::Array(vec![])),
        ]),
        &[],
    );
    assert_eq!(
        decode_envelope(&schema_one, MAGIC).unwrap_err().message(),
        "Unsupported envelope schema: 1"
    );

    for (label, chunks_field, payloads) in [
        (
            "a non-letter name",
            JsonValue::Array(vec!["bad-name".into()]),
            vec![vec![1u8]],
        ),
        (
            "a duplicate name",
            JsonValue::Array(vec!["a".into(), "a".into()]),
            vec![vec![1u8], vec![2u8]],
        ),
        (
            "a non-string name",
            JsonValue::Array(vec![7.0.into()]),
            vec![vec![1u8]],
        ),
        ("a non-array chunk list", "alpha".into(), vec![]),
    ] {
        let bytes = encode_raw(
            MAGIC,
            &manifest(&[("schemaVersion", 2.0.into()), ("chunks", chunks_field)]),
            &payloads,
        );
        assert_eq!(
            decode_envelope(&bytes, MAGIC).unwrap_err().message(),
            "Invalid envelope chunks",
            "{label}"
        );
    }
}

#[test]
fn an_envelope_rejects_corruption_truncation_and_trailing_bytes() {
    let chunks = vec![
        chunk("alpha", &(0..64u8).collect::<Vec<_>>()),
        chunk("beta", &[5, 6]),
    ];
    let original =
        encode_envelope(MAGIC, &manifest(&[("note", "x".into())]), &chunks).expect("encode");

    let mut flipped = original.clone();
    let at = flipped.len() - 10;
    flipped[at] ^= 0x01;
    assert_eq!(
        decode_envelope(&flipped, MAGIC).unwrap_err().message(),
        "Envelope checksum mismatch"
    );

    let mut flipped_manifest = original.clone();
    flipped_manifest[20] ^= 0x20;
    let message = decode_envelope(&flipped_manifest, MAGIC)
        .unwrap_err()
        .message()
        .to_string();
    assert!(
        message.contains("checksum mismatch")
            || message.contains("JSON")
            || message.contains("Unsupported")
            || message.contains("Invalid"),
        "unexpected message: {message}"
    );

    let mut trailing = original.clone();
    trailing.extend_from_slice(&[0, 0, 0]);
    assert_eq!(
        decode_envelope(&trailing, MAGIC).unwrap_err().message(),
        "Envelope checksum mismatch"
    );

    // Trailing data with a valid footer: only the length bookkeeping can catch it. Four extra
    // payload bytes, then a footer recomputed over all of them.
    let mut padded = original[..original.len() - 4].to_vec();
    padded.extend_from_slice(&[0; 8]);
    assert_eq!(padded.len(), original.len() + 4);
    let crc = crc32(&padded[..padded.len() - 4]);
    let at = padded.len() - 4;
    padded[at..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(
        decode_envelope(&padded, MAGIC).unwrap_err().message(),
        "Envelope contains trailing data"
    );

    let short = &original[..original.len() - 8];
    let message = decode_envelope(short, MAGIC)
        .unwrap_err()
        .message()
        .to_string();
    assert!(
        message.contains("truncated") || message.contains("checksum mismatch"),
        "unexpected message: {message}"
    );

    // A manifest length that runs past the end of the file.
    let manifest_bytes = manifest(&[
        ("schemaVersion", 2.0.into()),
        ("chunks", JsonValue::Array(vec!["alpha".into()])),
    ])
    .stringify()
    .into_bytes();
    let mut truncated = vec![0u8; MAGIC.len() + 4 + manifest_bytes.len() - 5];
    truncated[..MAGIC.len()].copy_from_slice(MAGIC.as_bytes());
    truncated[MAGIC.len()..MAGIC.len() + 4]
        .copy_from_slice(&(manifest_bytes.len() as u32).to_le_bytes());
    assert_eq!(
        decode_envelope(&truncated, MAGIC).unwrap_err().message(),
        "Envelope manifest is truncated"
    );

    // A chunk header that claims more bytes than the file holds.
    let overlong = encode_raw(
        MAGIC,
        &manifest(&[
            ("schemaVersion", 2.0.into()),
            (
                "chunks",
                JsonValue::Array(vec!["alpha".into(), "beta".into()]),
            ),
        ]),
        &[vec![1, 2, 3]],
    );
    assert!(decode_envelope(&overlong, MAGIC)
        .unwrap_err()
        .message()
        .contains("truncated"),);
}

// --- The agent checkpoint through a real envelope ------------------------------------------------

#[test]
fn agent_chunks_survive_a_full_envelope_round_trip() {
    let (data, _) = synthetic_dataset();
    let frames = frame_pool(8, 5150, 160, 144);
    let mut config = AgentConfig::with_decoder(gameboy_decoder_config());
    config.frame = Some(FrameSize {
        width: 160,
        height: 144,
    });
    let mut source = NeuralAgent::new(Arc::clone(&data), config.clone()).expect("an agent");
    source.warmup(Some(&frames[0])).expect("warmup");
    for frame in 1..=150usize {
        let rewards = if frame.is_multiple_of(50) {
            vec![RewardEvent::new(0.6)]
        } else {
            Vec::new()
        };
        source
            .tick(
                &frames[frame % frames.len()],
                &TickOptions {
                    rewards: &rewards,
                    boot: (frame / 120).is_multiple_of(2),
                    learn: !frame.is_multiple_of(90),
                },
            )
            .expect("tick");
    }

    let parts = agent_to_chunks(&source.export_state());
    assert_eq!(
        parts
            .chunks
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        AGENT_CHUNK_NAMES
    );
    let chunk = |name: &str| {
        parts
            .chunks
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, bytes)| bytes.len())
            .expect("a chunk")
    };
    assert_eq!(chunk("membrane"), data.meta.neurons * 4);
    assert_eq!(chunk("lastSpikeMs"), data.meta.neurons * 8);
    assert_eq!(chunk("visualDrive"), data.visual_indices.len() * 4);

    let mut with_compatibility = parts.manifest.clone();
    with_compatibility.set("compatibility", source.compatibility().as_str().into());
    let bytes = encode_envelope(MAGIC, &with_compatibility, &parts.chunks).expect("encode");
    let decoded = decode_envelope(&bytes, MAGIC).expect("decode");
    assert_eq!(
        decoded
            .manifest
            .get("compatibility")
            .and_then(JsonValue::as_str),
        Some(source.compatibility().as_str())
    );

    let state = agent_from_chunks(&decoded.manifest, &decoded).expect("rebuild");
    let mut restored = NeuralAgent::new(Arc::clone(&data), config).expect("an agent");
    restored.import_state(&state).expect("import");
    assert_eq!(restored.export_state(), source.export_state());

    // And the two run on identically from there.
    for frame in 151..=200usize {
        let options = TickOptions {
            rewards: &[],
            boot: (frame / 120).is_multiple_of(2),
            learn: true,
        };
        let image = &frames[frame % frames.len()];
        assert_eq!(
            restored.tick(image, &options).expect("tick"),
            source.tick(image, &options).expect("tick"),
            "diverged at frame {frame}"
        );
    }
    assert_eq!(restored.export_state(), source.export_state());
}

#[test]
fn agent_from_chunks_rejects_an_incomplete_or_misaligned_checkpoint() {
    let (data, _) = synthetic_dataset();
    let mut config = AgentConfig::with_decoder(gameboy_decoder_config());
    config.frame = Some(FrameSize {
        width: 160,
        height: 144,
    });
    let mut agent = NeuralAgent::new(Arc::clone(&data), config).expect("an agent");
    agent.warmup(None).expect("warmup");
    let parts = agent_to_chunks(&agent.export_state());
    let envelope = flybrain_core::envelope::EnvelopeParts {
        manifest: parts.manifest.clone(),
        chunks: parts.chunks.clone(),
    };

    let mut wrong_version = parts.manifest.clone();
    wrong_version.set("agentVersion", 2.0.into());
    assert_eq!(
        agent_from_chunks(&wrong_version, &envelope)
            .unwrap_err()
            .message(),
        "Unsupported agent checkpoint version"
    );

    let mut no_decoder = parts.manifest.clone();
    no_decoder.set("decoder", JsonValue::Null);
    assert_eq!(
        agent_from_chunks(&no_decoder, &envelope)
            .unwrap_err()
            .message(),
        "Agent checkpoint manifest is incomplete"
    );

    for name in AGENT_CHUNK_NAMES {
        let missing = flybrain_core::envelope::EnvelopeParts {
            manifest: parts.manifest.clone(),
            chunks: parts
                .chunks
                .iter()
                .filter(|(key, _)| key != name)
                .cloned()
                .collect(),
        };
        assert_eq!(
            agent_from_chunks(&parts.manifest, &missing)
                .unwrap_err()
                .message(),
            format!("Checkpoint is missing {name}")
        );
    }

    let partial = flybrain_core::envelope::EnvelopeParts {
        manifest: parts.manifest.clone(),
        chunks: parts
            .chunks
            .iter()
            .map(|(key, bytes)| {
                if key == "membrane" {
                    (key.clone(), bytes[..bytes.len() - 1].to_vec())
                } else {
                    (key.clone(), bytes.clone())
                }
            })
            .collect(),
    };
    assert_eq!(
        agent_from_chunks(&parts.manifest, &partial)
            .unwrap_err()
            .message(),
        "Checkpoint chunk membrane has a partial element"
    );
}
