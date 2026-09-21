//! Unit tests for the retina projection and the dataset loader, ported from
//! `packages/brain/tests/model.test.ts` and `tests/dataset.test.ts`.
//!
//! The golden scenarios compare the projection's *output* for two fixed column geometries; these
//! pin the rules that geometry cannot reach: the bounding-box normalization, the hemisphere mirror,
//! the nearest-neighbour rounding and its clamp, and the degenerate-axis guard.

mod common;

use common::{real_dataset_dir, toy_dataset};
use flybrain_core::dataset::{load_brain_dataset_from_dir, validate_dataset, BrainDataset};
use flybrain_core::jsmath::fround;
use flybrain_core::retina::{project_frame, RetinaColumns, DEFAULT_RETINA_CONFIG};

/// Gray frame of 4x4 blocks, so nearest-neighbour rounding differences stay inside a block.
fn block_frame(width: usize, height: usize) -> Vec<u8> {
    let mut rgba = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let value = (((x * 4 / width) * 4 + (y * 4 / height)) * 15 + 3) as u8;
            let offset = (y * width + x) * 4;
            rgba[offset] = value;
            rgba[offset + 1] = value;
            rgba[offset + 2] = value;
            rgba[offset + 3] = 255;
        }
    }
    rgba
}

#[test]
fn the_projection_is_resolution_independent() {
    let xy: Vec<f32> = vec![
        0.0, 0.0, 0.1, 0.35, 0.6, 0.85, 1.0, 1.0, 0.35, 0.6, 0.85, 0.1,
    ];
    let hemisphere = vec![1u8, 1, 0, 1, 0, 1];
    let columns = RetinaColumns {
        xy: &xy,
        hemisphere: &hemisphere,
        count: 6,
    };

    let mut small = vec![0.0f32; 6];
    let mut large = vec![0.0f32; 6];
    project_frame(
        &block_frame(160, 144),
        160,
        144,
        columns,
        DEFAULT_RETINA_CONFIG.gain,
        &mut small,
    );
    project_frame(
        &block_frame(320, 288),
        320,
        288,
        columns,
        DEFAULT_RETINA_CONFIG.gain,
        &mut large,
    );
    assert_eq!(large, small, "320x288 must map to the same relative pixels");
    let distinct: std::collections::HashSet<u32> = small.iter().map(|v| v.to_bits()).collect();
    assert_eq!(
        distinct.len(),
        6,
        "columns must land in six distinct blocks"
    );

    // Gain scales the drive linearly, through a Float32 store.
    let mut doubled = vec![0.0f32; 6];
    project_frame(
        &block_frame(320, 288),
        320,
        288,
        columns,
        DEFAULT_RETINA_CONFIG.gain * 2.0,
        &mut doubled,
    );
    for index in 0..6 {
        assert_eq!(
            f64::from(doubled[index]),
            fround(f64::from(small[index]) * 2.0),
            "column {index}"
        );
    }
}

#[test]
fn hemisphere_zero_is_mirrored_on_x_and_hemisphere_one_is_not() {
    // A frame that is black on the left half and white on the right.
    let (width, height) = (160usize, 144usize);
    let mut rgba = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in width / 2..width {
            let offset = (y * width + x) * 4;
            rgba[offset] = 255;
            rgba[offset + 1] = 255;
            rgba[offset + 2] = 255;
        }
    }
    // Two columns at the same y: one at the left edge of the bounding box, one at the right.
    let xy = vec![0.0f32, 0.0, 10.0, 0.0];
    let mut mirrored = vec![0.0f32; 2];
    project_frame(
        &rgba,
        width as u32,
        height as u32,
        RetinaColumns {
            xy: &xy,
            hemisphere: &[0, 0],
            count: 2,
        },
        1.0,
        &mut mirrored,
    );
    let mut direct = vec![0.0f32; 2];
    project_frame(
        &rgba,
        width as u32,
        height as u32,
        RetinaColumns {
            xy: &xy,
            hemisphere: &[1, 1],
            count: 2,
        },
        1.0,
        &mut direct,
    );
    // Unmirrored: the first column reads the black left edge, the second the white right edge.
    assert_eq!(direct[0], 0.0);
    assert!(direct[1] > 0.99);
    // Mirrored: exactly the other way round.
    assert!(mirrored[0] > 0.99);
    assert_eq!(mirrored[1], 0.0);
}

#[test]
fn a_degenerate_axis_maps_to_zero_instead_of_nan() {
    // Every column at the same coordinate: `maxX - minX` is 0, and the `|| 1` guard applies.
    let xy = vec![7.0f32, 7.0, 7.0, 7.0];
    let mut out = vec![0.0f32; 2];
    project_frame(
        &block_frame(160, 144),
        160,
        144,
        RetinaColumns {
            xy: &xy,
            hemisphere: &[0, 1],
            count: 2,
        },
        DEFAULT_RETINA_CONFIG.gain,
        &mut out,
    );
    assert!(out.iter().all(|value| value.is_finite()), "{out:?}");
    // Hemisphere 0 mirrors 0 to 1, so the two columns read opposite corners of the frame.
    assert_ne!(out[0], out[1]);
}

#[test]
fn luminance_uses_rec_709_weights_and_ignores_alpha() {
    let (width, height) = (2usize, 1usize);
    let mut rgba = vec![0u8; width * height * 4];
    // Pixel (0, 0): pure red at full alpha. Pixel (1, 0): pure green at zero alpha.
    rgba[0] = 255;
    rgba[3] = 255;
    rgba[5] = 255;
    rgba[7] = 0;
    let xy = vec![0.0f32, 0.0, 1.0, 0.0];
    let mut out = vec![0.0f32; 2];
    project_frame(
        &rgba,
        width as u32,
        height as u32,
        RetinaColumns {
            xy: &xy,
            hemisphere: &[1, 1],
            count: 2,
        },
        1.0,
        &mut out,
    );
    assert_eq!(f64::from(out[0]), fround(255.0 * 0.2126 / 255.0));
    assert_eq!(
        f64::from(out[1]),
        fround(255.0 * 0.7152 / 255.0),
        "alpha must not scale the drive"
    );
}

#[test]
fn validate_dataset_rejects_each_length_mismatch_individually() {
    validate_dataset(&toy_dataset()).expect("the fixture must validate");

    /// A label, the length mismatch to introduce, and the message it must produce.
    type Case = (&'static str, Box<dyn Fn(&mut BrainDataset)>, &'static str);
    let broken: Vec<Case> = vec![
        (
            "indptr not neurons + 1",
            Box::new(|data: &mut BrainDataset| data.indptr.pop().map(|_| ()).unwrap_or(())),
            "FlyWire artifact lengths do not match metadata",
        ),
        (
            "targets against meta.edges",
            Box::new(|data: &mut BrainDataset| data.targets.push(0)),
            "FlyWire artifact lengths do not match metadata",
        ),
        (
            "weights against meta.edges",
            Box::new(|data: &mut BrainDataset| data.weights.push(0)),
            "FlyWire artifact lengths do not match metadata",
        ),
        (
            "a neuron count that disagrees with the arrays",
            Box::new(|data: &mut BrainDataset| data.meta.neurons += 1),
            "FlyWire artifact lengths do not match metadata",
        ),
        (
            "visualIndices against visual.count",
            Box::new(|data: &mut BrainDataset| data.visual_indices.push(0)),
            "FlyWire visual artifact lengths do not match metadata",
        ),
        (
            "visualHemisphere against visual.count",
            Box::new(|data: &mut BrainDataset| data.visual_hemisphere.push(0)),
            "FlyWire visual artifact lengths do not match metadata",
        ),
        (
            "visualXY against visual.count * 2",
            Box::new(|data: &mut BrainDataset| data.visual_xy.push(0.0)),
            "FlyWire visual artifact lengths do not match metadata",
        ),
    ];
    for (label, patch, message) in broken {
        let mut data = toy_dataset();
        patch(&mut data);
        assert_eq!(
            validate_dataset(&data).unwrap_err().message(),
            message,
            "{label}"
        );
    }
}

#[test]
fn the_loader_reports_a_missing_dataset_rather_than_panicking() {
    let error = load_brain_dataset_from_dir("does/not/exist").unwrap_err();
    assert!(
        error.message().starts_with("Unable to load brain metadata"),
        "{}",
        error.message()
    );
}

#[test]
fn the_real_dataset_decodes_to_the_connectome_the_model_was_tuned_against() {
    let Some(dir) = real_dataset_dir() else {
        eprintln!("skipped: data/fafb-v783/meta.json is absent in this worktree");
        return;
    };
    let data = load_brain_dataset_from_dir(&dir).expect("the dataset must load");
    assert_eq!(data.meta.schema_version, 1);
    assert_eq!(data.meta.dataset, "FlyWire FAFB Codex v783");
    assert_eq!(data.meta.neurons, 139_255);
    assert_eq!(data.meta.edges, 2_700_513);
    assert_eq!(data.meta.visual.population, "L1");
    assert_eq!(data.meta.visual.count, 1_572);

    // Seven hex digests joined by ':'.
    let parts: Vec<&str> = data
        .fingerprint
        .as_deref()
        .expect("a fingerprint")
        .split(':')
        .collect();
    assert_eq!(parts.len(), 7);
    assert!(parts.iter().all(|part| part.len() == 64
        && part
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())));

    // The CSR graph is internally consistent.
    assert_eq!(data.indptr.len(), data.meta.neurons + 1);
    assert_eq!(data.indptr[0], 0);
    assert_eq!(*data.indptr.last().unwrap() as usize, data.meta.edges);
    assert!(data.indptr.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(data
        .targets
        .iter()
        .all(|target| (*target as usize) < data.meta.neurons));

    // The circuit-role merge landed, with the documented population sizes.
    for (role, size) in [
        ("kenyon", 5_177),
        ("mbon", 96),
        ("sensory", 17_550),
        ("reward_pam", 307),
        ("visual_l1", 1_572),
        ("descending", 1_305),
        ("motor", 110),
    ] {
        assert_eq!(data.role(role).len(), size, "role {role}");
    }
    assert!(
        data.role("kenyon").windows(2).all(|pair| pair[0] < pair[1]),
        "roles are sorted"
    );
}
