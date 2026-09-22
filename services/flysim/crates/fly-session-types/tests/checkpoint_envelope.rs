//! The `FLYSESS1` envelope layout: its fixture, its offsets and the corruptions it refuses.

use fly_session_types::{canonical, checkpoint, fixtures};
use serde_json::Value;

fn envelope_bytes(file: &Value) -> Vec<u8> {
    fixtures::decode_base64(
        file.get("envelope")
            .and_then(|e| e.get("base64"))
            .and_then(Value::as_str)
            .expect("base64"),
    )
    .expect("base64")
}

#[test]
fn the_fixture_envelope_decodes_to_its_recorded_layout() {
    let file = fixtures::load("checkpoint-envelope.json").expect("checkpoint-envelope.json");
    let bytes = envelope_bytes(&file);
    let envelope = checkpoint::decode(&bytes).expect("a valid envelope");
    checkpoint::validate_manifest(&envelope).expect("a complete manifest");

    let layout = file["envelope"]["layout"].clone();
    assert_eq!(&bytes[0..8], checkpoint::MAGIC);
    assert_eq!(
        bytes.len().to_string(),
        layout["totalBytes"].as_str().expect("totalBytes")
    );
    assert_eq!(
        envelope.layout.table_offset.to_string(),
        layout["tableOffset"].as_str().expect("tableOffset")
    );
    assert_eq!(
        envelope.layout.manifest_bytes,
        layout["manifestBytes"].as_u64().expect("manifestBytes") as u32
    );
    let entries = layout["entries"].as_array().expect("entries");
    assert_eq!(envelope.layout.entries.len(), entries.len());
    for (entry, recorded) in envelope.layout.entries.iter().zip(entries) {
        assert_eq!(entry.name, recorded["name"].as_str().expect("name"));
        assert_eq!(
            entry.offset.to_string(),
            recorded["offset"].as_str().expect("offset")
        );
        assert_eq!(
            entry.byte_length.to_string(),
            recorded["byteLength"].as_str().expect("byteLength")
        );
        assert_eq!(
            checkpoint::hex(&entry.digest),
            recorded["digest"].as_str().expect("digest")
        );
        assert_eq!(entry.offset % 8, 0, "payloads start on an eight-byte boundary");
    }

    for payload in file["payloads"].as_array().expect("payloads") {
        let name = payload["name"].as_str().expect("name");
        let expected = fixtures::decode_base64(payload["base64"].as_str().expect("base64"))
            .expect("base64");
        assert_eq!(
            envelope.payload(name).expect("a payload"),
            expected.as_slice(),
            "payload {name} must come back byte for byte"
        );
    }
    assert_eq!(
        envelope.manifest,
        file["manifest"],
        "the manifest round trips as canonical JSON"
    );
}

#[test]
fn every_recorded_corruption_is_refused() {
    let file = fixtures::load("checkpoint-envelope.json").expect("checkpoint-envelope.json");
    let bytes = envelope_bytes(&file);
    for case in file["corruption"].as_array().expect("corruption") {
        let name = case["name"].as_str().expect("name");
        let offset = case["offset"].as_u64().expect("offset") as usize;
        let mut corrupted = bytes.clone();
        corrupted[offset] ^= 0x01;
        assert!(
            checkpoint::decode(&corrupted).is_err(),
            "{name} must be refused: {}",
            case["reason"].as_str().unwrap_or("")
        );
    }
    let truncated = &bytes[..bytes.len() - 1];
    assert!(
        checkpoint::decode(truncated).is_err(),
        "a truncated envelope must be refused"
    );
    assert!(
        checkpoint::decode(&bytes[..8]).is_err(),
        "a header alone is not an envelope"
    );
}

#[test]
fn a_flysim01_envelope_is_not_read_as_a_session_checkpoint() {
    // The historical envelope: magic, u32 manifest length, manifest, chunks, CRC32.
    let mut legacy = Vec::new();
    legacy.extend_from_slice(b"FLYSIM01");
    let manifest = br#"{"schemaVersion":2,"chunks":[]}"#;
    legacy.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
    legacy.extend_from_slice(manifest);
    legacy.extend_from_slice(&0u32.to_le_bytes());
    assert!(
        checkpoint::decode(&legacy).is_err(),
        "FLYSESS1 is a new format; the old reader stays separate"
    );
}

#[test]
fn the_layout_is_deterministic_and_the_manifest_is_canonical() {
    let manifest = canonical::parse_strict(br#"{"b":2,"a":1}"#).expect("parses");
    let payloads = vec![
        ("one".to_owned(), b"first".to_vec()),
        ("two".to_owned(), vec![0u8; 9]),
    ];
    let bytes = checkpoint::encode(&manifest, &payloads).expect("encode");
    let again = checkpoint::encode(&manifest, &payloads).expect("encode");
    assert_eq!(bytes, again, "the same inputs produce the same bytes");
    let envelope = checkpoint::decode(&bytes).expect("decode");
    let start = checkpoint::HEADER_BYTES;
    let end = start + envelope.layout.manifest_bytes as usize;
    assert_eq!(
        std::str::from_utf8(&bytes[start..end]).expect("utf-8"),
        "{\"a\":1,\"b\":2}",
        "the manifest is stored as canonical JSON"
    );
    assert_eq!(envelope.layout.entries[1].offset % 8, 0);
    assert!(
        checkpoint::encode(
            &manifest,
            &[("one".to_owned(), vec![]), ("one".to_owned(), vec![])]
        )
        .is_err(),
        "payload names are unique"
    );
    assert!(
        checkpoint::encode(&manifest, &[("One".to_owned(), vec![])]).is_err(),
        "payload names are Ids, and the widening from letters-only is deliberate, not arbitrary"
    );
    assert!(
        checkpoint::encode(&manifest, &[("a".to_owned(), Vec::new())]).is_ok(),
        "an empty payload is still a payload"
    );
}

#[test]
fn a_manifest_missing_a_required_field_is_not_a_complete_checkpoint() {
    let file = fixtures::load("checkpoint-envelope.json").expect("checkpoint-envelope.json");
    let full = file["manifest"].clone();
    for field in checkpoint::REQUIRED_MANIFEST_FIELDS {
        let mut manifest = full.clone();
        manifest.as_object_mut().expect("object").remove(*field);
        let bytes = checkpoint::encode(&manifest, &[]).expect("encode");
        let envelope = checkpoint::decode(&bytes).expect("decode");
        assert!(
            checkpoint::validate_manifest(&envelope).is_err(),
            "a manifest without {field:?} must be refused"
        );
    }
}
