//! Connectome artifact format (schema 1) and the directory loader.
//!
//! Ports `dataset/format.ts` and `dataset/load-node.ts`, including the exact fingerprint string:
//! seven lowercase SHA-256 digests joined with `:`, over `JSON.stringify(meta)` (circuit roles
//! already merged) and then the six simulation arrays in their frozen order.

use std::fs;
use std::io::Read as _;
use std::path::Path;

use indexmap::IndexMap;
use sha2::{Digest, Sha256};

use crate::error::{bail, Error, Result};
use crate::json::JsonValue;

/// `meta.visual`: the retina population.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualMeta {
    pub population: String,
    pub count: usize,
}

/// Anatomical role name -> sorted neuron indices. Labels, not inferred task functions.
pub type Roles = IndexMap<String, Vec<u32>>;

/// The typed view of `meta.json` the kernel needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainMetadata {
    pub schema_version: i64,
    pub dataset: String,
    pub neurons: usize,
    pub edges: usize,
    /// Role order is observable: it decides the network's tracked-role order.
    pub roles: Roles,
    pub visual: VisualMeta,
}

/// A directed, signed, aggregated connectivity graph in source-indexed CSR form, plus roles and
/// the retina columns.
#[derive(Debug, Clone)]
pub struct BrainDataset {
    /// SHA-256 digests of metadata and every array, joined with ':'; used for checkpoint
    /// compatibility. `None` for a dataset assembled in memory, as in TypeScript.
    pub fingerprint: Option<String>,
    pub meta: BrainMetadata,
    /// CSR row pointers, length neurons + 1.
    pub indptr: Vec<u32>,
    /// CSR column indices (post-synaptic neuron per edge).
    pub targets: Vec<u32>,
    /// Signed aggregated synapse counts per edge (inhibitory transmitters negative).
    pub weights: Vec<i16>,
    /// Neuron index of each retina column.
    pub visual_indices: Vec<u32>,
    /// 0 = left (mirrored on X when projecting), 1 = right.
    pub visual_hemisphere: Vec<u8>,
    /// Interleaved x,y column coordinates in dataset units.
    pub visual_xy: Vec<f32>,
}

impl BrainDataset {
    /// Neuron indices of a role, or an empty slice when the dataset does not declare it.
    pub fn role(&self, name: &str) -> &[u32] {
        self.meta
            .roles
            .get(name)
            .map(|neurons| neurons.as_slice())
            .unwrap_or(&[])
    }
}

/// Throw if the arrays do not describe the connectome the metadata claims.
pub fn validate_dataset(dataset: &BrainDataset) -> Result<()> {
    let meta = &dataset.meta;
    if dataset.indptr.len() != meta.neurons + 1
        || dataset.targets.len() != meta.edges
        || dataset.weights.len() != meta.edges
    {
        bail!("FlyWire artifact lengths do not match metadata");
    }
    if dataset.visual_indices.len() != meta.visual.count
        || dataset.visual_hemisphere.len() != meta.visual.count
        || dataset.visual_xy.len() != meta.visual.count * 2
    {
        bail!("FlyWire visual artifact lengths do not match metadata");
    }
    Ok(())
}

/// Prefix of the macro-type populations (`docs/design/macros.md` section 11).
///
/// The one name rule that splits `circuit-roles.json` in two at load time: everything else is an
/// anatomical role that the dataset fingerprint covers, and a `macro_*` role is a relabelling of
/// neurons the dataset already carries, merged *after* the fingerprint is taken. See
/// [`merge_macro_roles`].
pub const MACRO_ROLE_PREFIX: &str = "macro_";

/// Merge the sidecar's anatomical circuit roles into loaded metadata, in place.
///
/// Both the fingerprint and the role order depend on this being an `Object.assign` over the
/// existing `roles` object rather than a rebuild: an existing key (`descending`, `motor`) keeps
/// its position and a new one (`sensory`, `kenyon`, `mbon`) is appended.
///
/// `macro_*` roles are deliberately *not* merged here: [`merge_macro_roles`] adds them after the
/// fingerprint has been taken.
pub fn merge_circuit_roles(meta: &mut JsonValue, circuits: &JsonValue) -> Result<()> {
    let declared = meta
        .get("neurons")
        .and_then(JsonValue::as_usize)
        .ok_or_else(|| Error::new("Unable to load brain metadata: neurons is missing"))?;
    let sidecar = circuits.get("neurons").and_then(JsonValue::as_usize);
    if sidecar != Some(declared) {
        bail!("Circuit roles do not match connectome");
    }
    let incoming = circuits
        .get("roles")
        .and_then(JsonValue::as_object)
        .map(<[(String, JsonValue)]>::to_vec)
        .unwrap_or_default();
    let Some(JsonValue::Object(_)) = meta.get("roles") else {
        bail!("Unable to load brain metadata: roles is missing");
    };
    // Object.assign(meta.roles, circuits.roles), minus the macro populations
    let mut roles = meta.get("roles").cloned().unwrap_or_else(JsonValue::object);
    for (name, value) in incoming {
        if name.starts_with(MACRO_ROLE_PREFIX) {
            continue;
        }
        roles.set(&name, value);
    }
    meta.set("roles", roles);
    Ok(())
}

/// Merge the sidecar's `macro_<type>` populations into loaded metadata, in place.
///
/// Split from [`merge_circuit_roles`] for one reason, and it is a contract rather than a
/// convenience: the dataset fingerprint hashes `JSON.stringify(meta)`, checkpoints record that
/// string, and the macro populations name no new neuron, edge or weight — they are the mushroom
/// body output neurons and the brain motor neurons the artifact already listed, relabelled
/// twenty-two ways (`docs/design/macros.md` section 11: "the neuron ids, edges and kernel are
/// untouched, so the compatibility string must not move"). So a loader takes the fingerprint over
/// the anatomical roles alone and calls this afterwards, which is what keeps
/// `flysim --print-compatibility` byte-identical across this change and what lets a checkpoint
/// written before the roles existed load and start them at zero.
///
/// The cost is stated rather than hidden: re-cutting the macro populations differently would not
/// invalidate a checkpoint, and the rates restored by name would then belong to different
/// neurons. `tools/build_flywire.py` owns that partition and
/// `flybrain-core/tests/macro_roles.rs` pins it against the committed artifact.
pub fn merge_macro_roles(meta: &mut JsonValue, circuits: &JsonValue) -> Result<()> {
    let incoming = circuits
        .get("roles")
        .and_then(JsonValue::as_object)
        .map(<[(String, JsonValue)]>::to_vec)
        .unwrap_or_default();
    let mut roles = meta.get("roles").cloned().unwrap_or_else(JsonValue::object);
    for (name, value) in incoming {
        if name.starts_with(MACRO_ROLE_PREFIX) {
            roles.set(&name, value);
        }
    }
    meta.set("roles", roles);
    Ok(())
}

/// Read the typed metadata out of a merged `meta.json` value.
pub fn metadata_from_json(meta: &JsonValue) -> Result<BrainMetadata> {
    let field = |name: &str| -> Result<&JsonValue> {
        meta.get(name)
            .ok_or_else(|| Error::new(format!("Unable to load brain metadata: {name} is missing")))
    };
    let schema_version = field("schemaVersion")?
        .as_f64()
        .ok_or_else(|| Error::new("Unable to load brain metadata: schemaVersion is not a number"))?
        as i64;
    let dataset = field("dataset")?
        .as_str()
        .ok_or_else(|| Error::new("Unable to load brain metadata: dataset is not a string"))?
        .to_string();
    let neurons = field("neurons")?
        .as_usize()
        .ok_or_else(|| Error::new("Unable to load brain metadata: neurons is not an integer"))?;
    let edges = field("edges")?
        .as_usize()
        .ok_or_else(|| Error::new("Unable to load brain metadata: edges is not an integer"))?;
    let mut roles = Roles::new();
    for (name, value) in field("roles")?
        .as_object()
        .ok_or_else(|| Error::new("Unable to load brain metadata: roles is not an object"))?
    {
        let mut neurons = Vec::new();
        for entry in value
            .as_array()
            .ok_or_else(|| Error::new(format!("Role {name} is not an array")))?
        {
            neurons.push(
                entry
                    .as_usize()
                    .ok_or_else(|| Error::new(format!("Role {name} holds a non-index")))?
                    as u32,
            );
        }
        roles.insert(name.clone(), neurons);
    }
    let visual = field("visual")?;
    let visual = VisualMeta {
        population: visual
            .get("population")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        count: visual
            .get("count")
            .and_then(JsonValue::as_usize)
            .ok_or_else(|| Error::new("Unable to load brain metadata: visual.count is missing"))?,
    };
    Ok(BrainMetadata {
        schema_version,
        dataset,
        neurons,
        edges,
        roles,
        visual,
    })
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The metadata the fingerprint is taken over: everything, minus the `macro_*` populations.
///
/// Key order is what a digest is made of, so this rebuilds `roles` in its own place and in its own
/// order — the macro roles are appended last by the artifact, so dropping them leaves exactly the
/// object the fingerprint was defined over before they existed. The TypeScript oracle does the
/// same in `fingerprintedMetadata` (`dataset/format.ts`). See [`merge_macro_roles`].
pub fn fingerprinted_metadata(meta: &JsonValue) -> String {
    let Some(JsonValue::Object(entries)) = meta.get("roles") else {
        return meta.stringify();
    };
    if !entries
        .iter()
        .any(|(name, _)| name.starts_with(MACRO_ROLE_PREFIX))
    {
        return meta.stringify();
    }
    let mut roles = JsonValue::object();
    for (name, value) in entries.clone() {
        if !name.starts_with(MACRO_ROLE_PREFIX) {
            roles.set(&name, value);
        }
    }
    let mut trimmed = meta.clone();
    trimmed.set("roles", roles);
    trimmed.stringify()
}

/// Digest of the metadata JSON and every array, joined with ':'.
///
/// The order of the seven parts and the hashed byte ranges are frozen, because checkpoints record
/// this string: metadata as UTF-8 `JSON.stringify(meta)` (circuit roles already merged), then
/// indptr, targets, weights, visualIndices, visualHemisphere, visualXY, each as its raw
/// little-endian typed-array bytes.
pub fn fingerprint_dataset(meta_json: &str, dataset: &BrainDataset) -> String {
    [
        sha256_hex(meta_json.as_bytes()),
        sha256_hex(&to_le_bytes_u32(&dataset.indptr)),
        sha256_hex(&to_le_bytes_u32(&dataset.targets)),
        sha256_hex(&to_le_bytes_i16(&dataset.weights)),
        sha256_hex(&to_le_bytes_u32(&dataset.visual_indices)),
        sha256_hex(&dataset.visual_hemisphere),
        sha256_hex(&to_le_bytes_f32(&dataset.visual_xy)),
    ]
    .join(":")
}

fn to_le_bytes_u32(values: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn to_le_bytes_i16(values: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 2);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn to_le_bytes_f32(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Load a dataset from a local `data/<dataset>` directory, exactly as `load-node.ts` does.
pub fn load_brain_dataset_from_dir(dir: impl AsRef<Path>) -> Result<BrainDataset> {
    let dir = dir.as_ref();
    let meta_text = fs::read_to_string(dir.join("meta.json"))
        .map_err(|error| Error::new(format!("Unable to load brain metadata: {error}")))?;
    let mut meta_json = JsonValue::parse(&meta_text)
        .map_err(|error| Error::new(format!("Unable to load brain metadata: {error}")))?;
    let circuits_text = fs::read_to_string(dir.join("circuit-roles.json"))
        .map_err(|_| Error::new("Unable to load anatomical circuit roles"))?;
    let circuits = JsonValue::parse(&circuits_text)
        .map_err(|_| Error::new("Unable to load anatomical circuit roles"))?;
    merge_circuit_roles(&mut meta_json, &circuits)?;
    merge_macro_roles(&mut meta_json, &circuits)?;
    let meta = metadata_from_json(&meta_json)?;

    let dataset = BrainDataset {
        fingerprint: None,
        meta,
        indptr: load_u32(dir, "indptr.binz")?,
        targets: load_u32(dir, "targets.binz")?,
        weights: load_i16(dir, "weights.binz")?,
        visual_indices: load_u32(dir, "visual-indices.binz")?,
        visual_hemisphere: load_bytes(dir, "visual-hemisphere.binz")?,
        visual_xy: load_f32(dir, "visual-xy.binz")?,
    };
    validate_dataset(&dataset)?;
    // The macro populations are deliberately outside the digest (`merge_macro_roles`).
    let fingerprint = fingerprint_dataset(&fingerprinted_metadata(&meta_json), &dataset);
    Ok(BrainDataset {
        fingerprint: Some(fingerprint),
        ..dataset
    })
}

/// Gunzip one `.binz` artifact.
fn load_bytes(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let path = dir.join(name);
    let compressed = fs::read(&path).map_err(|error| {
        Error::new(format!(
            "Unable to load {}: {error}",
            path.to_string_lossy()
        ))
    })?;
    let mut decoder = flate2::read::GzDecoder::new(compressed.as_slice());
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).map_err(|error| {
        Error::new(format!(
            "Unable to load {}: {error}",
            path.to_string_lossy()
        ))
    })?;
    Ok(out)
}

fn load_u32(dir: &Path, name: &str) -> Result<Vec<u32>> {
    let bytes = load_bytes(dir, name)?;
    if bytes.len() % 4 != 0 {
        bail!("Artifact {name} has a partial element");
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn load_i16(dir: &Path, name: &str) -> Result<Vec<i16>> {
    let bytes = load_bytes(dir, name)?;
    if bytes.len() % 2 != 0 {
        bail!("Artifact {name} has a partial element");
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect())
}

fn load_f32(dir: &Path, name: &str) -> Result<Vec<f32>> {
    let bytes = load_bytes(dir, name)?;
    if bytes.len() % 4 != 0 {
        bail!("Artifact {name} has a partial element");
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_reference() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn merge_appends_new_roles_and_keeps_existing_positions() {
        let mut meta = JsonValue::parse(
            r#"{"neurons":4,"roles":{"motor":[3],"command_0":[3]},"visual":{"count":0}}"#,
        )
        .unwrap();
        let circuits =
            JsonValue::parse(r#"{"neurons":4,"roles":{"motor":[2],"kenyon":[0]}}"#).unwrap();
        merge_circuit_roles(&mut meta, &circuits).unwrap();
        assert_eq!(
            meta.get("roles").unwrap().stringify(),
            r#"{"motor":[2],"command_0":[3],"kenyon":[0]}"#
        );
    }

    #[test]
    fn merge_rejects_a_neuron_count_mismatch() {
        let mut meta =
            JsonValue::parse(r#"{"neurons":4,"roles":{},"visual":{"count":0}}"#).unwrap();
        let circuits = JsonValue::parse(r#"{"neurons":5,"roles":{}}"#).unwrap();
        assert_eq!(
            merge_circuit_roles(&mut meta, &circuits)
                .unwrap_err()
                .message(),
            "Circuit roles do not match connectome"
        );
    }
}
