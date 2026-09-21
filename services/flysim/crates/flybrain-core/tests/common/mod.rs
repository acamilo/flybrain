//! Shared support for the golden tests.
//!
//! Golden files are checkpoint envelopes with the magic `FLYGOLD1`, written by
//! `packages/brain/tools/golden.ts`: a JSON manifest holding the scenario definition and every
//! scalar, plus named chunks holding the arrays verbatim. Reading them through the crate's own
//! envelope port means every golden test also exercises that port.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use flybrain_core::dataset::{BrainDataset, BrainMetadata, Roles, VisualMeta};
use flybrain_core::envelope::{decode_envelope, EnvelopeParts};
use flybrain_core::json::JsonValue;

/// `<repo>/services/flysim`.
pub fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `<repo>`.
pub fn repo_dir() -> PathBuf {
    workspace_dir().join("../..")
}

/// `<repo>/data/fafb-v783`, or `None` on a branch where the dataset has not been merged.
pub fn real_dataset_dir() -> Option<PathBuf> {
    let dir = repo_dir().join("data/fafb-v783");
    dir.join("meta.json").exists().then_some(dir)
}

/// One golden scenario.
pub struct Golden {
    pub name: String,
    pub parts: EnvelopeParts,
}

/// Load `services/flysim/golden/<name>.flygold`.
pub fn golden(name: &str) -> Golden {
    let path = workspace_dir()
        .join("golden")
        .join(format!("{name}.flygold"));
    let bytes = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "unable to read {}: {error}\nregenerate with: npx tsx packages/brain/tools/golden.ts",
            path.display()
        )
    });
    let parts = decode_envelope(&bytes, "FLYGOLD1")
        .unwrap_or_else(|error| panic!("{} is not a FLYGOLD1 envelope: {error}", path.display()));
    Golden {
        name: name.to_string(),
        parts,
    }
}

impl Golden {
    pub fn manifest(&self) -> &JsonValue {
        &self.parts.manifest
    }

    /// `manifest.a.b.c`, panicking with the path when a member is missing.
    pub fn at(&self, path: &str) -> &JsonValue {
        let mut value = self.manifest();
        for key in path.split('.') {
            value = value.get(key).unwrap_or_else(|| {
                panic!("golden {}: manifest has no {path} (at {key})", self.name)
            });
        }
        value
    }

    pub fn number(&self, path: &str) -> f64 {
        self.at(path)
            .as_f64()
            .unwrap_or_else(|| panic!("golden {}: {path} is not a number", self.name))
    }

    pub fn text(&self, path: &str) -> String {
        self.at(path)
            .as_str()
            .unwrap_or_else(|| panic!("golden {}: {path} is not a string", self.name))
            .to_string()
    }

    pub fn flag(&self, path: &str) -> bool {
        self.at(path)
            .as_bool()
            .unwrap_or_else(|| panic!("golden {}: {path} is not a boolean", self.name))
    }

    pub fn numbers(&self, path: &str) -> Vec<f64> {
        numbers_of(self.at(path))
    }

    pub fn strings(&self, path: &str) -> Vec<String> {
        self.at(path)
            .as_array()
            .unwrap_or_else(|| panic!("golden {}: {path} is not an array", self.name))
            .iter()
            .map(|entry| entry.as_str().unwrap_or_default().to_string())
            .collect()
    }

    pub fn chunk(&self, name: &str) -> &[u8] {
        self.parts
            .chunk(name)
            .unwrap_or_else(|| panic!("golden {}: no chunk {name}", self.name))
    }

    pub fn f32_chunk(&self, name: &str) -> Vec<f32> {
        self.chunk(name)
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }

    pub fn f64_chunk(&self, name: &str) -> Vec<f64> {
        self.chunk(name)
            .chunks_exact(8)
            .map(|chunk| {
                f64::from_le_bytes([
                    chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
                ])
            })
            .collect()
    }

    pub fn u32_chunk(&self, name: &str) -> Vec<u32> {
        self.chunk(name)
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }

    pub fn i16_chunk(&self, name: &str) -> Vec<i16> {
        self.chunk(name)
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect()
    }
}

pub fn numbers_of(value: &JsonValue) -> Vec<f64> {
    value
        .as_array()
        .expect("expected a JSON array")
        .iter()
        .map(|entry| entry.as_f64().expect("expected a numeric array member"))
        .collect()
}

/// The `xorshift` helper from `tests/fixtures/toy-dataset.ts`.
pub fn xorshift(seed: i32) -> impl FnMut() -> f64 {
    let mut state = if seed == 0 { 1 } else { seed };
    move || {
        state ^= state << 13;
        state ^= ((state as u32) >> 17) as i32;
        state ^= state << 5;
        f64::from(state as u32) / 4_294_967_296.0
    }
}

/// The `framePool` helper from `tests/agent.test.ts`: deterministic RGBA noise frames.
pub fn frame_pool(count: usize, seed: i32, width: usize, height: usize) -> Vec<Vec<u8>> {
    let mut random = xorshift(seed);
    (0..count)
        .map(|_| {
            (0..width * height * 4)
                .map(|_| (random() * 256.0).floor() as u8)
                .collect()
        })
        .collect()
}

/// SHA-256 over a whole frame pool, matching `poolDigest` in the generator.
pub fn pool_digest(frames: &[Vec<u8>]) -> String {
    let joined: Vec<u8> = frames.iter().flatten().copied().collect();
    flybrain_core::dataset::sha256_hex(&joined)
}

/// The four-neuron fixture from `tests/fixtures/toy-dataset.ts`.
///
/// Neuron 0 is a Kenyon cell, 1 and 2 are MBONs, 3 is a motor and `command_0` neuron. Edges, in
/// CSR order: 0: 0->1 (10), 1: 0->2 (-5), 2: 0->3 (20), 3: 1->3 (10), 4: 2->1 (5). Structured so
/// the plastic-edge selection rule has both a qualifying edge and disqualifying ones (a negative
/// weight, a non-KC source).
pub fn toy_dataset() -> BrainDataset {
    let mut roles = Roles::new();
    roles.insert("kenyon".to_string(), vec![0]);
    roles.insert("mbon".to_string(), vec![1, 2]);
    roles.insert("motor".to_string(), vec![3]);
    roles.insert("command_0".to_string(), vec![3]);
    BrainDataset {
        fingerprint: None,
        meta: BrainMetadata {
            schema_version: 1,
            dataset: "test".to_string(),
            neurons: 4,
            edges: 5,
            roles,
            visual: VisualMeta {
                population: "test".to_string(),
                count: 0,
            },
        },
        indptr: vec![0, 3, 4, 5, 5],
        targets: vec![1, 2, 3, 3, 1],
        weights: vec![10, -5, 20, 10, 5],
        visual_indices: Vec::new(),
        visual_hemisphere: Vec::new(),
        visual_xy: Vec::new(),
    }
}

/// The toy fixture with a single retina column on neuron 0, as the original warm-up test used.
pub fn visual_toy_dataset() -> BrainDataset {
    let mut data = toy_dataset();
    data.meta.visual.count = 1;
    data.visual_indices = vec![0];
    data.visual_hemisphere = vec![0];
    data.visual_xy = vec![0.0, 0.0];
    data
}

/// The 4,096-neuron synthetic connectome, read out of the `agent` golden file.
///
/// Its generator consumes two random draws for a forced Kenyon edge and three for a free one, so
/// the arrays travel in the envelope rather than being re-derived here.
pub fn synthetic_dataset() -> (Arc<BrainDataset>, Golden) {
    let golden = golden("agent");
    let meta = golden.at("meta");
    let mut roles = Roles::new();
    for (name, list) in meta.get("roles").and_then(JsonValue::as_object).unwrap() {
        roles.insert(
            name.clone(),
            numbers_of(list)
                .into_iter()
                .map(|value| value as u32)
                .collect(),
        );
    }
    let visual = meta.get("visual").unwrap();
    let data = BrainDataset {
        fingerprint: None,
        meta: BrainMetadata {
            schema_version: meta.get("schemaVersion").unwrap().as_f64().unwrap() as i64,
            dataset: meta.get("dataset").unwrap().as_str().unwrap().to_string(),
            neurons: meta.get("neurons").unwrap().as_usize().unwrap(),
            edges: meta.get("edges").unwrap().as_usize().unwrap(),
            roles,
            visual: VisualMeta {
                population: visual
                    .get("population")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string(),
                count: visual.get("count").unwrap().as_usize().unwrap(),
            },
        },
        indptr: golden.u32_chunk("indptr"),
        targets: golden.u32_chunk("targets"),
        weights: golden.i16_chunk("weights"),
        visual_indices: golden.u32_chunk("visualIndices"),
        visual_hemisphere: golden.chunk("visualHemisphere").to_vec(),
        visual_xy: golden.f32_chunk("visualXY"),
    };
    flybrain_core::dataset::validate_dataset(&data).expect("the fixture must validate");
    (Arc::new(data), golden)
}

/// Rebuild the toy connectome from the manifest JSON the generator embedded.
pub fn dataset_from_json(value: &JsonValue) -> Arc<BrainDataset> {
    let meta = value.get("meta").expect("dataset.meta");
    let mut roles = Roles::new();
    for (name, list) in meta
        .get("roles")
        .and_then(JsonValue::as_object)
        .expect("dataset.meta.roles")
    {
        roles.insert(
            name.clone(),
            numbers_of(list)
                .into_iter()
                .map(|value| value as u32)
                .collect(),
        );
    }
    let visual = meta.get("visual").expect("dataset.meta.visual");
    Arc::new(BrainDataset {
        fingerprint: None,
        meta: BrainMetadata {
            schema_version: meta.get("schemaVersion").unwrap().as_f64().unwrap() as i64,
            dataset: meta.get("dataset").unwrap().as_str().unwrap().to_string(),
            neurons: meta.get("neurons").unwrap().as_usize().unwrap(),
            edges: meta.get("edges").unwrap().as_usize().unwrap(),
            roles,
            visual: VisualMeta {
                population: visual
                    .get("population")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string(),
                count: visual.get("count").unwrap().as_usize().unwrap(),
            },
        },
        indptr: numbers_of(value.get("indptr").unwrap())
            .into_iter()
            .map(|value| value as u32)
            .collect(),
        targets: numbers_of(value.get("targets").unwrap())
            .into_iter()
            .map(|value| value as u32)
            .collect(),
        weights: numbers_of(value.get("weights").unwrap())
            .into_iter()
            .map(|value| value as i16)
            .collect(),
        visual_indices: numbers_of(value.get("visualIndices").unwrap())
            .into_iter()
            .map(|value| value as u32)
            .collect(),
        visual_hemisphere: numbers_of(value.get("visualHemisphere").unwrap())
            .into_iter()
            .map(|value| value as u8)
            .collect(),
        visual_xy: numbers_of(value.get("visualXY").unwrap())
            .into_iter()
            .map(|value| value as f32)
            .collect(),
    })
}

// --- Exact comparison, with a diagnosable failure ------------------------------------------------

/// Bit-exact equality for an f32 array, reporting the first mismatch and its ulp distance.
///
/// The tolerance is zero. The ulp distance is printed only so that a failure says immediately
/// whether it is a rounding difference (1-2 ulp, look at a transcendental) or a real divergence.
pub fn assert_f32_eq(label: &str, got: &[f32], want: &[f32]) {
    assert_eq!(got.len(), want.len(), "{label}: length differs");
    let differing = got
        .iter()
        .zip(want)
        .filter(|(got, want)| got.to_bits() != want.to_bits())
        .count();
    if differing == 0 {
        return;
    }
    let total = want.len();
    let (index, (mine, theirs)) = got
        .iter()
        .zip(want)
        .enumerate()
        .find(|(_, (mine, theirs))| mine.to_bits() != theirs.to_bits())
        .expect("a differing entry");
    let ulp = (mine.to_bits() as i64 - theirs.to_bits() as i64).abs();
    panic!(
        "{label}: {differing} of {total} entries differ; first at [{index}]: \
         rust {mine:?} ({:#010x}) != oracle {theirs:?} ({:#010x}), {ulp} ulp",
        mine.to_bits(),
        theirs.to_bits(),
    );
}

/// Bit-exact equality for an f64 array.
pub fn assert_f64_eq(label: &str, got: &[f64], want: &[f64]) {
    assert_eq!(got.len(), want.len(), "{label}: length differs");
    for (index, (got, want)) in got.iter().zip(want).enumerate() {
        if got.to_bits() != want.to_bits() {
            let ulp = (got.to_bits() as i64 - want.to_bits() as i64).abs();
            panic!(
                "{label}[{index}]: rust {got:?} ({:#018x}) != oracle {want:?} ({:#018x}), {ulp} ulp",
                got.to_bits(),
                want.to_bits(),
            );
        }
    }
}

pub fn assert_u8_eq(label: &str, got: &[u8], want: &[u8]) {
    assert_eq!(got.len(), want.len(), "{label}: length differs");
    for (index, (got, want)) in got.iter().zip(want).enumerate() {
        assert_eq!(got, want, "{label}[{index}]");
    }
}

pub fn assert_f64_exact(label: &str, got: f64, want: f64) {
    assert!(
        got.to_bits() == want.to_bits(),
        "{label}: rust {got:?} ({:#018x}) != oracle {want:?} ({:#018x})",
        got.to_bits(),
        want.to_bits()
    );
}
