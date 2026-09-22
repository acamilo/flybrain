//! MEDIA-01 shape rules: every sentence of `state-media-v1` section 2 that a descriptor, a
//! reference or a chunk sequence can be checked against on its own.
//!
//! These are the contract-level halves of the slice's acceptance bullets -- bad strides, bad
//! lengths, bad producing times and the audio rules -- and the type-level distinction between
//! a persistent `AssetRef` and a transient `ArtifactRef`.

use fly_session_types::ArtifactRef;
use fly_session_types::media::{
    AudioDescriptor, AudioRef, AudioTimeline, ViewDescriptor, ViewRef, check_imported_asset,
    require_finite_samples,
};
use fly_session_types::scalar::DomainType;
use fly_session_types::workers::AssetRef;

fn artifact(byte_length: u64, content_type: &str) -> ArtifactRef {
    ArtifactRef {
        store_id: "store-a".into(),
        artifact_id: "art-1".into(),
        generation: 1,
        byte_length,
        content_type: content_type.into(),
        digest: None,
    }
}

fn view_descriptor(width: u64, height: u64, delay: u64) -> ViewDescriptor {
    ViewDescriptor {
        view_id: "arena".into(),
        width,
        height,
        row_stride: width * 4,
        pixel_aspect_numerator: 1,
        pixel_aspect_denominator: 1,
        observation_delay_steps: delay,
    }
}

fn view_ref(descriptor: &ViewDescriptor, produced_step: u64, bytes: u64) -> ViewRef {
    ViewRef {
        view_id: descriptor.view_id.clone(),
        produced_step,
        pixels: artifact(bytes, "image/x-rgba8"),
    }
}

fn audio_descriptor(sample_rate: u64, channels: u64) -> AudioDescriptor {
    AudioDescriptor {
        stream_id: "arena".into(),
        sample_rate,
        channels,
    }
}

fn audio_ref(
    descriptor: &AudioDescriptor,
    first_sample: u64,
    frames: u64,
    discontinuity: bool,
) -> AudioRef {
    AudioRef {
        stream_id: descriptor.stream_id.clone(),
        first_sample,
        sample_frames: frames,
        samples: artifact(frames * descriptor.channels * 4, "audio/x-f32le"),
        discontinuity,
    }
}

// -----------------------------------------------------------------------------------------
// Bad strides

/// `rowStride` is exactly `4 x width`; v1 has no padded rows.
#[test]
fn a_padded_row_stride_is_refused() {
    let good = view_descriptor(32, 24, 0);
    good.validate().expect("4 x width is the only stride");

    let mut padded = good.clone();
    padded.row_stride = 32 * 4 + 16;
    padded.validate().expect_err("a padded row is not readable in v1");

    let mut narrow = good.clone();
    narrow.row_stride = 32 * 3;
    narrow.validate().expect_err("a stride under 4 x width is refused");

    // The same rule through the wire form, where a hand-written descriptor arrives.
    let mut json = good.to_json();
    json["rowStride"] = serde_json::json!(32 * 4 + 4);
    ViewDescriptor::from_json(&json).expect_err("a padded stride is refused when read");
}

/// Dimensions are integers 1..=4096, and `pixelAspect` parts positive integers <=65535.
#[test]
fn dimensions_pixel_aspect_and_delay_have_stated_bounds() {
    for (width, height) in [(0, 24), (32, 0), (4097, 24), (32, 4097)] {
        let mut d = view_descriptor(32, 24, 0);
        d.width = width;
        d.height = height;
        d.row_stride = width.max(1) * 4;
        d.validate().expect_err("dimensions are 1..=4096");
    }
    view_descriptor(1, 1, 0).validate().expect("1x1 is inside the bounds");
    view_descriptor(4096, 4096, 0)
        .validate()
        .expect("4096x4096 is inside the bounds");

    for (numerator, denominator) in [(0, 1), (1, 0), (65_536, 1), (1, 65_536)] {
        let mut d = view_descriptor(32, 24, 0);
        d.pixel_aspect_numerator = numerator;
        d.pixel_aspect_denominator = denominator;
        d.validate().expect_err("pixelAspect parts are 1..=65535");
    }

    let mut d = view_descriptor(32, 24, 9);
    d.validate().expect_err("observationDelaySteps is 0..=8");
    d.observation_delay_steps = 8;
    d.validate().expect("eight steps of delay are allowed");
}

/// Only top-left RGBA8 exists in v1; another format is a media-schema change.
#[test]
fn only_rgba8_is_a_readable_view_format() {
    let descriptor = view_descriptor(32, 24, 0);
    let mut json = descriptor.to_json();
    json["format"] = serde_json::json!("rgb8");
    ViewDescriptor::from_json(&json).expect_err("rgb8 is not a v1 format");
    json["format"] = serde_json::json!("rgba8");
    ViewDescriptor::from_json(&json).expect("rgba8 is the v1 format");
}

// -----------------------------------------------------------------------------------------
// Bad lengths

/// A frame's artifact is exactly `rowStride x height` bytes.
#[test]
fn a_frame_whose_length_is_not_stride_times_height_is_refused() {
    let descriptor = view_descriptor(32, 24, 0);
    let exact = descriptor.frame_bytes();
    assert_eq!(exact, 32 * 4 * 24);

    view_ref(&descriptor, 7, exact)
        .validate_against(&descriptor, None)
        .expect("the exact frame length is accepted");
    for wrong in [exact - 1, exact + 1, exact - 32 * 4, exact * 2] {
        view_ref(&descriptor, 7, wrong)
            .validate_against(&descriptor, None)
            .expect_err("only rowStride x height is the frame length");
    }
}

// -----------------------------------------------------------------------------------------
// Bad producing times

/// A required sensory view is produced at exactly `max(0, boundary - observationDelaySteps)`.
#[test]
fn a_view_produced_at_the_wrong_boundary_is_refused() {
    let descriptor = view_descriptor(32, 24, 2);
    let bytes = descriptor.frame_bytes();
    assert_eq!(descriptor.required_produced_step(10), 8);

    view_ref(&descriptor, 8, bytes)
        .validate_against(&descriptor, Some(10))
        .expect("the declared delay is exactly two steps");
    // One frame later than the delay allows, and one frame older: both are step failures,
    // not an arbitrary latest frame.
    view_ref(&descriptor, 9, bytes)
        .validate_against(&descriptor, Some(10))
        .expect_err("an under-delayed frame is refused");
    view_ref(&descriptor, 7, bytes)
        .validate_against(&descriptor, Some(10))
        .expect_err("an extra-delayed frame is refused");
}

/// Bootstrap may repeat `O[0]` until the declared pipeline delay fills, and only until then.
#[test]
fn bootstrap_repeats_the_first_frame_until_the_pipeline_delay_fills() {
    let descriptor = view_descriptor(32, 24, 3);
    let bytes = descriptor.frame_bytes();
    // Boundaries 0..=3 all require the frame produced at 0.
    for boundary in 0..=3 {
        assert_eq!(descriptor.required_produced_step(boundary), 0);
        view_ref(&descriptor, 0, bytes)
            .validate_against(&descriptor, Some(boundary))
            .expect("O[0] repeats while the pipeline fills");
    }
    // From boundary 4 the pipeline is full and O[0] is a stale frame.
    assert_eq!(descriptor.required_produced_step(4), 1);
    view_ref(&descriptor, 0, bytes)
        .validate_against(&descriptor, Some(4))
        .expect_err("the repetition ends when the delay is filled");
    view_ref(&descriptor, 1, bytes)
        .validate_against(&descriptor, Some(4))
        .expect("boundary 4 requires the frame produced at 1");
}

// -----------------------------------------------------------------------------------------
// Audio shapes

/// sampleRate is 8000..=192000, channels 1..=8 and sampleFrames 0..=192000.
#[test]
fn audio_rate_channels_and_frames_have_stated_bounds() {
    audio_descriptor(8_000, 1).validate().expect("8 kHz mono is the floor");
    audio_descriptor(192_000, 8).validate().expect("192 kHz 8ch is the ceiling");
    audio_descriptor(7_999, 2).validate().expect_err("under 8 kHz is refused");
    audio_descriptor(192_001, 2).validate().expect_err("over 192 kHz is refused");
    audio_descriptor(48_000, 0).validate().expect_err("zero channels are refused");
    audio_descriptor(48_000, 9).validate().expect_err("nine channels are refused");

    let descriptor = audio_descriptor(48_000, 2);
    let mut chunk = audio_ref(&descriptor, 0, 192_000, false);
    chunk.validate().expect("192000 frames is the per-chunk ceiling");
    chunk.sample_frames = 192_001;
    chunk.validate().expect_err("over 192000 frames in one chunk is refused");
}

/// A chunk's artifact is exactly `sampleFrames x channels x 4` bytes.
#[test]
fn an_audio_chunk_length_is_frames_times_channels_times_four() {
    let descriptor = audio_descriptor(48_000, 2);
    let chunk = audio_ref(&descriptor, 0, 800, false);
    assert_eq!(chunk.samples.byte_length, 800 * 2 * 4);
    chunk
        .validate_against(&descriptor)
        .expect("the exact chunk length is accepted");

    let mut wrong = chunk.clone();
    wrong.samples = artifact(800 * 2 * 4 - 4, "audio/x-f32le");
    wrong
        .validate_against(&descriptor)
        .expect_err("a short chunk is refused");

    // The same frames at another channel count are a different number of bytes.
    let mono = audio_descriptor(48_000, 1);
    let mut wrong_channels = chunk.clone();
    wrong_channels.stream_id = mono.stream_id.clone();
    wrong_channels
        .validate_against(&mono)
        .expect_err("stereo bytes are not a mono chunk");
}

/// Samples are finite f32.
#[test]
fn a_non_finite_sample_is_refused() {
    let mut bytes = Vec::new();
    for value in [0.0f32, -0.5, 0.75, 1.0] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    require_finite_samples(&bytes).expect("finite samples are accepted");

    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut broken = bytes.clone();
        broken.extend_from_slice(&bad.to_le_bytes());
        require_finite_samples(&broken).expect_err("a non-finite sample is refused");
    }
    require_finite_samples(&bytes[..5]).expect_err("a partial sample is refused");
}

/// Within an epoch, chunks cannot overlap or go backwards.
#[test]
fn chunks_cannot_overlap_or_go_backwards_within_an_epoch() {
    let descriptor = audio_descriptor(48_000, 2);
    let mut timeline = AudioTimeline::fresh(&descriptor, 0);
    timeline
        .accept(&audio_ref(&descriptor, 0, 800, false), &descriptor)
        .expect("the first chunk starts at the origin");
    assert_eq!(timeline.next_sample(), 800);
    timeline
        .accept(&audio_ref(&descriptor, 800, 800, false), &descriptor)
        .expect("the second chunk continues the first");
    assert_eq!(timeline.next_sample(), 1_600);

    let mut overlapping = timeline.clone();
    overlapping
        .accept(&audio_ref(&descriptor, 1_599, 800, false), &descriptor)
        .expect_err("a chunk that starts inside the previous one is refused");
    let mut backwards = timeline.clone();
    backwards
        .accept(&audio_ref(&descriptor, 0, 800, false), &descriptor)
        .expect_err("a chunk that goes backwards is refused");
    // A rejected chunk leaves the timeline where it was.
    assert_eq!(overlapping.next_sample(), 1_600);
    assert_eq!(overlapping.accepted(), 2);

    // A gap is forward, so it is allowed; it is the one place a later chunk may mark a
    // discontinuity.
    timeline
        .accept(&audio_ref(&descriptor, 2_000, 800, true), &descriptor)
        .expect("a forward gap is not an overlap");
    timeline
        .accept(&audio_ref(&descriptor, 2_800, 800, true), &descriptor)
        .expect_err("a chunk that continues the previous one is not a discontinuity");
}

/// Crash restore preserves the sample position under a new epoch, and its first chunk marks
/// the discontinuity.
#[test]
fn the_first_chunk_after_a_restore_marks_discontinuity() {
    let descriptor = audio_descriptor(48_000, 2);
    let mut fresh = AudioTimeline::fresh(&descriptor, 0);
    fresh
        .accept(&audio_ref(&descriptor, 0, 800, true), &descriptor)
        .expect_err("the episode's first chunk is not a discontinuity");
    fresh
        .accept(&audio_ref(&descriptor, 0, 800, false), &descriptor)
        .expect("the episode's first chunk continues nothing");

    // The restored epoch resumes at the preserved position.
    let mut restored = AudioTimeline::restored_at(&descriptor, 800);
    restored
        .accept(&audio_ref(&descriptor, 800, 800, false), &descriptor)
        .expect_err("the first chunk after a restore marks discontinuity");
    restored
        .accept(&audio_ref(&descriptor, 0, 800, true), &descriptor)
        .expect_err("the restored position is preserved, not reset");
    restored
        .accept(&audio_ref(&descriptor, 800, 800, true), &descriptor)
        .expect("the restored epoch resumes at its preserved sample position");
    assert_eq!(restored.next_sample(), 1_600);
    restored
        .accept(&audio_ref(&descriptor, 1_600, 800, false), &descriptor)
        .expect("the chunks after it are ordinary");
}

// -----------------------------------------------------------------------------------------
// Persistent assets against transient artifacts

/// An `AssetRef` and an `ArtifactRef` are different identities with different fields, and
/// neither is readable as the other.
#[test]
fn an_asset_ref_is_not_a_transient_artifact_ref() {
    let asset = AssetRef {
        id: "counter-arena-backend".into(),
        digest: "a".repeat(64),
        byte_length: 24,
        format: "fly-config-v1".into(),
    };
    let imported = ArtifactRef {
        store_id: "store-a".into(),
        artifact_id: "art-9".into(),
        generation: 1,
        byte_length: 24,
        content_type: "application/octet-stream".into(),
        digest: Some("a".repeat(64)),
    };

    // Identity fields do not overlap: the asset has no store and the artifact has no format.
    let asset_keys: Vec<String> = asset
        .to_json()
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    let artifact_keys: Vec<String> = imported
        .to_json()
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    assert_eq!(asset_keys, vec!["id", "digest", "byteLength", "format"]);
    assert!(artifact_keys.contains(&"storeId".to_owned()));
    assert!(artifact_keys.contains(&"artifactId".to_owned()));
    assert!(!artifact_keys.contains(&"format".to_owned()));
    assert!(!asset_keys.contains(&"storeId".to_owned()));

    // Neither reads as the other: a view's pixels are an artifact, never an asset.
    ArtifactRef::from_json(&asset.to_json()).expect_err("an asset is not an artifact reference");
    AssetRef::from_json(&imported.to_json()).expect_err("an artifact is not an asset reference");
    let pixels_as_asset = serde_json::json!({
        "viewId": "arena",
        "producedStep": "0",
        "pixels": asset.to_json(),
    });
    ViewRef::from_json(&pixels_as_asset).expect_err("a view's pixels cannot be an asset");

    // Importing an asset is a content check, not an identity conversion.
    check_imported_asset(&asset, &imported).expect("the import carries the asset's content");
    let mut no_digest = imported.clone();
    no_digest.digest = None;
    check_imported_asset(&asset, &no_digest)
        .expect_err("a persistent asset import must carry a content digest");
    let mut other_content = imported.clone();
    other_content.digest = Some("b".repeat(64));
    check_imported_asset(&asset, &other_content).expect_err("another digest is another content");
    let mut short = imported.clone();
    short.byte_length = 23;
    check_imported_asset(&asset, &short).expect_err("the import must be the asset's length");
}
