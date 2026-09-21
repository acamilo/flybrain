//! Framing round-trip against a decoder written the way `packages/feed/src/codec.ts` reads it.
//!
//! `decode_snapshot` below is a deliberate transcription of the TypeScript decoder, including
//! its error cases, so a layout change on the Rust side fails here instead of in the browser:
//!
//! ```text
//! u32 LE headerLength | header JSON (UTF-8) | (u32 LE byteLength | bytes) * n
//! ```
//!
//! and attachments come in `header.attachments` order, each skippable by its length prefix.

mod common;

use std::collections::HashMap;

use flysim::snapshot::{
    AttachmentKind, FRAME_BYTES, FRAME_HEIGHT, FRAME_WIDTH, FeedHeader, Snapshot, Wants,
};

#[derive(Debug)]
struct Decoded {
    header: FeedHeader,
    attachments: HashMap<AttachmentKind, Vec<u8>>,
}

/// `decodeSnapshot` from `packages/feed/src/codec.ts`, transcribed.
fn decode_snapshot(bytes: &[u8]) -> Result<Decoded, String> {
    let mut offset = 0usize;
    let read_u32 = |bytes: &[u8], offset: usize, what: &str| -> Result<u32, String> {
        if offset + 4 > bytes.len() {
            return Err(format!("truncated message: not enough bytes to read {what}"));
        }
        Ok(u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()))
    };

    let header_length = read_u32(bytes, offset, "header length")? as usize;
    offset += 4;
    if offset + header_length > bytes.len() {
        return Err(format!(
            "truncated message: header claims {header_length} bytes but only {} remain",
            bytes.len() - offset
        ));
    }
    let header: FeedHeader = serde_json::from_slice(&bytes[offset..offset + header_length])
        .map_err(|error| format!("header is not valid JSON: {error}"))?;
    offset += header_length;

    if header.protocol != 1 {
        return Err(format!("unsupported protocol {}, expected 1", header.protocol));
    }
    let mut seen = Vec::new();
    for kind in &header.attachments {
        if seen.contains(kind) {
            return Err(format!("header.attachments lists {kind:?} more than once"));
        }
        seen.push(*kind);
    }

    let mut attachments = HashMap::new();
    for kind in &header.attachments {
        let length = read_u32(bytes, offset, "attachment length")? as usize;
        offset += 4;
        if offset + length > bytes.len() {
            return Err(format!(
                "truncated message: {kind:?} attachment claims {length} bytes but only {} remain",
                bytes.len() - offset
            ));
        }
        attachments.insert(*kind, bytes[offset..offset + length].to_vec());
        offset += length;
    }
    if offset != bytes.len() {
        return Err(format!(
            "trailing bytes after last attachment: {} unexpected bytes",
            bytes.len() - offset
        ));
    }
    Ok(Decoded { header, attachments })
}

fn sample() -> Snapshot {
    common::populated_snapshot(139_255)
}

#[test]
fn a_produced_snapshot_decodes_with_the_layout_the_page_expects() {
    let snapshot = sample();
    let bytes = snapshot.encode(Wants::all());
    let decoded = decode_snapshot(&bytes).expect("decodes");

    assert_eq!(decoded.header, snapshot.header);
    assert_eq!(
        decoded.header.attachments,
        vec![AttachmentKind::Frame, AttachmentKind::Audio, AttachmentKind::Spikes],
        "attachments are framed in header order"
    );

    // The contract's sizes: 160x144 RGBA, interleaved stereo f32, ceil(n / 8) bytes of bitset.
    let frame = &decoded.attachments[&AttachmentKind::Frame];
    assert_eq!(frame.len(), FRAME_BYTES);
    assert_eq!(frame.len(), FRAME_WIDTH * FRAME_HEIGHT * 4);
    assert_eq!(frame, snapshot.frame.as_ref());

    let audio = &decoded.attachments[&AttachmentKind::Audio];
    assert_eq!(audio.len() % 8, 0, "a whole number of stereo f32 frames");
    assert_eq!(audio, snapshot.audio.as_ref());
    // Read back as `Float32Array` would, little-endian.
    let samples: Vec<f32> = audio
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect();
    assert_eq!(samples.len(), 3_200);
    assert!(samples.iter().all(|sample| (-1.0..=1.0).contains(sample)));

    let spikes = &decoded.attachments[&AttachmentKind::Spikes];
    assert_eq!(spikes.len(), 139_255usize.div_ceil(8));
    assert_eq!(spikes.len(), 17_407, "the contract's own number");
    let set_bits: u64 = spikes.iter().map(|byte| u64::from(byte.count_ones())).sum();
    assert_eq!(set_bits, decoded.header.spike_count, "spikeCount is the bitset's popcount");
}

#[test]
fn the_total_length_is_exactly_the_sum_of_the_framed_parts() {
    let snapshot = sample();
    let bytes = snapshot.encode(Wants::all());
    let header_length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let expected = 4
        + header_length
        + 4
        + snapshot.frame.len()
        + 4
        + snapshot.audio.len()
        + 4
        + snapshot.spikes.len();
    assert_eq!(bytes.len(), expected);
    assert!(header_length < 4_096, "the header stays under 4 KB: {header_length}");
}

#[test]
fn every_wants_subset_frames_only_what_was_asked_for() {
    let snapshot = sample();
    for (wants, expected) in [
        (Wants::none(), vec![]),
        (
            Wants { frame: true, audio: false, spikes: false },
            vec![AttachmentKind::Frame],
        ),
        (
            Wants { frame: false, audio: true, spikes: false },
            vec![AttachmentKind::Audio],
        ),
        (
            Wants { frame: false, audio: false, spikes: true },
            vec![AttachmentKind::Spikes],
        ),
        (
            Wants { frame: true, audio: false, spikes: true },
            vec![AttachmentKind::Frame, AttachmentKind::Spikes],
        ),
        (Wants::all(), AttachmentKind::ALL.to_vec()),
    ] {
        let decoded = decode_snapshot(&snapshot.encode(wants)).expect("decodes");
        assert_eq!(decoded.header.attachments, expected, "{wants:?}");
        assert_eq!(decoded.attachments.len(), expected.len(), "{wants:?}");
        for kind in &expected {
            assert!(!decoded.attachments[kind].is_empty(), "{kind:?} for {wants:?}");
        }
        // Withholding the bitset zeroes the count, as the protocol says.
        if expected.contains(&AttachmentKind::Spikes) {
            assert_eq!(decoded.header.spike_count, snapshot.header.spike_count);
        } else {
            assert_eq!(decoded.header.spike_count, 0, "{wants:?}");
        }
        // Everything else in the header is unchanged by the subset.
        assert_eq!(decoded.header.seq, snapshot.header.seq);
        assert_eq!(decoded.header.frame, snapshot.header.frame);
    }
}

#[test]
fn a_header_only_snapshot_is_four_bytes_plus_its_json() {
    let snapshot = flysim::simloop::booting_snapshot(7, 1_757_000_000_000, flysim::snapshot::MacroMode::Raw);
    let bytes = snapshot.encode(Wants::all());
    let decoded = decode_snapshot(&bytes).expect("decodes");
    assert!(decoded.attachments.is_empty());
    let header_length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    assert_eq!(bytes.len(), 4 + header_length);
}

#[test]
fn the_transcribed_decoder_rejects_what_the_typescript_one_rejects() {
    let bytes = sample().encode(Wants::all());

    let error = decode_snapshot(&bytes[..2]).unwrap_err();
    assert!(error.contains("not enough bytes to read header length"), "{error}");

    let error = decode_snapshot(&bytes[..8]).unwrap_err();
    assert!(error.contains("header claims"), "{error}");

    let mut truncated = bytes.clone();
    truncated.truncate(truncated.len() - 1);
    let error = decode_snapshot(&truncated).unwrap_err();
    assert!(error.contains("attachment claims"), "{error}");

    let mut trailing = bytes;
    trailing.push(0);
    let error = decode_snapshot(&trailing).unwrap_err();
    assert!(error.contains("trailing bytes"), "{error}");
}
