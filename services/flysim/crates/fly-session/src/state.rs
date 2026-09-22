//! STATE-01: the coherent all-participant checkpoint store over the `FLYSESS1` envelope.
//!
//! [`fly_session_types::checkpoint`] owns the byte layout. This module owns everything
//! `checkpoint-envelope-v1` section 7 defers to this slice: generations, rotation, the store
//! manifest and its durable commit point, the bounded capture queue, the compatibility
//! comparison and the group fence.
//!
//! ```text
//! State.Capture  ──> every participant, at one committed boundary
//!                ──> one envelope: manifest + one payload per participant and per
//!                    coordinator-owned ledger
//! writer         ──> temp, fsync, rename, fsync dir, then the store manifest the same way
//!                    ^^^^ the store manifest rename is the durable commit point
//! State.StageRestore  ──> validated into replacement state, once-only token
//! State.ActivateRestore ──> installed under the new epoch, without a tick
//! ```
//!
//! Nothing here is best-effort. A saturated queue is a named `BUSY` refusal taken *before* a
//! capture is requested; a lost save reply is an explicit outcome that leaves durable
//! metadata where it was; an incompatible checkpoint names the field that differs; and an
//! unreferenced generation is never a restore candidate.
//!
//! `FLYSIM01` (`crates/flybrain-core/src/envelope.rs`, read by the legacy flysim store) is a
//! different format with a different magic and a different reader, and nothing here touches
//! it.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{Map, Value, json};

use fly_session_types::checkpoint::{self, Envelope};

// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the glob
// keeps the contract's own names in sight instead of restating them.
use crate::types::*;

/// The state-format identity this slice writes and reads. A checkpoint recorded under any
/// other one is refused by name rather than attempted.
pub const STATE_FORMAT_ID: &str = "flysess-1";

/// The worker capability `state-media-v1` section 5 makes the State methods conditional on.
pub const CHECKPOINT_CAPABILITY: &str = "checkpoint-v1";

/// The store manifest's own version. It is not the envelope version.
pub const STORE_MANIFEST_VERSION: u32 = 1;

/// The file the store manifest is committed to. Its rename is the durable commit point.
pub const STORE_MANIFEST_FILE: &str = "manifest.json";

/// The content type a checkpoint payload travels under as a bus artifact.
pub const PAYLOAD_CONTENT_TYPE: &str = "application/x-fly-checkpoint-payload";

/// The attachment name a checkpoint payload travels under, in both directions.
pub const PAYLOAD_ATTACHMENT: &str = "payload";

/// Seals one checkpoint payload as an immutable artifact against its content digest.
///
/// `state-media-v1` section 1 makes a digest mandatory on checkpoint payloads, so this seals
/// with one rather than computing it afterwards: a store that wrote the wrong bytes finds out
/// here and not at the next restore.
pub async fn seal_payload(
    client: &flybus::Client,
    bytes: &[u8],
    digest: &Digest,
) -> DomainResult<flybus::Artifact> {
    let mut writer = client
        .artifacts()
        .allocate(bytes.len() as u64, PAYLOAD_CONTENT_TYPE)
        .await
        .map_err(|e| store_error(format!("payload allocate: {}", e.message)))?;
    writer
        .write_all(bytes)
        .map_err(|e| store_error(format!("payload write: {e}")))?;
    let artifact = writer
        .seal_with_digest(Some(digest.clone()))
        .await
        .map_err(|e| store_error(format!("payload seal: {}", e.message)))?;
    match &artifact.reference().digest {
        Some(sealed) if sealed == digest => Ok(artifact),
        _ => Err(store_error("a sealed checkpoint payload has no matching content digest")),
    }
}

fn store_error(what: impl std::fmt::Display) -> DomainError {
    DomainError::new(ErrorCode::BackendFailure, what, MutationCertainty::Unknown)
}

fn incompatible(what: impl std::fmt::Display) -> DomainError {
    DomainError::before(ErrorCode::IncompatibleState, what)
}

// ----------------------------------------------------------------------------------------------
// Payload names

/// The payload name one agent's captured state is filed under.
pub fn agent_payload(agent_id: &str) -> String {
    format!("agent-{agent_id}")
}

/// The payload name one agent's action-executor state is filed under.
pub fn executor_payload(agent_id: &str) -> String {
    format!("executor-{agent_id}")
}

/// The environment's payload name.
pub const WORLD_PAYLOAD: &str = "world";
/// The task ledger's payload name.
pub const TASK_LEDGER_PAYLOAD: &str = "task-ledger";
/// The prior world inspection's payload name.
pub const PRIOR_INSPECTION_PAYLOAD: &str = "prior-inspection";
/// The coordinator's admission state and event watermarks.
pub const ADMISSION_PAYLOAD: &str = "coordinator-admission";

// ----------------------------------------------------------------------------------------------
// Compatibility

/// The compatibility identities `state-media-v1` section 4 requires a checkpoint to record.
///
/// Each one is a separate field on purpose: a restore that fails says which identity differs,
/// instead of reporting one opaque digest mismatch. The synthetic composition maps them onto
/// the identities it actually has:
///
/// | Field | Where it comes from |
/// | --- | --- |
/// | `backend_digest` | the environment descriptor's backend identity |
/// | `content_digest` | the environment descriptor's content identity |
/// | `patch_digest` | the environment descriptor's resolved configuration identity |
/// | `controller_digest` | every declared port's controller schema, in descriptor order |
/// | `parser_digest` | the inspection schema and the task schema that reads it |
/// | `state_format_id` | [`STATE_FORMAT_ID`] |
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compatibility {
    pub backend_digest: Digest,
    pub content_digest: Digest,
    pub patch_digest: Digest,
    pub controller_digest: Digest,
    pub parser_digest: Digest,
    pub state_format_id: Id,
}

impl Compatibility {
    /// The compatibility of one world, from the descriptor it advertises.
    ///
    /// Every identity comes from the descriptor, which is what lets the environment compute
    /// exactly this digest for its own capture while the coordinator computes it for the
    /// composition. The task's own schema is deliberately not in here: the captured task
    /// ledger carries its schema and refuses another one, so folding it in would put one
    /// identity in two places.
    pub fn of(descriptor: &EnvironmentDescriptor) -> Compatibility {
        let controllers = Value::Array(
            descriptor
                .ports
                .iter()
                .map(|port| {
                    json!({
                        "portId": port.port_id.as_str(),
                        "controls": port.controls.to_json(),
                    })
                })
                .collect(),
        );
        let parser = json!({
            "inspectionSchema": descriptor.inspection_schema.to_json(),
        });
        Compatibility {
            backend_digest: descriptor.backend_digest.clone(),
            content_digest: descriptor.content_digest.clone(),
            patch_digest: descriptor.configuration_digest.clone(),
            controller_digest: digest_of(&controllers)
                .expect("a validated controller schema canonicalizes"),
            parser_digest: digest_of(&parser).expect("a validated schema reference canonicalizes"),
            state_format_id: id(STATE_FORMAT_ID),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "backendDigest": self.backend_digest.as_str(),
            "contentDigest": self.content_digest.as_str(),
            "patchDigest": self.patch_digest.as_str(),
            "controllerDigest": self.controller_digest.as_str(),
            "parserDigest": self.parser_digest.as_str(),
            "stateFormatId": self.state_format_id.as_str(),
        })
    }

    pub fn from_json(value: &Value) -> Result<Compatibility, String> {
        let digest = |key: &str| -> Result<Digest, String> {
            let text = value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("compatibility: {key} is missing or not a string"))?;
            if !is_digest(text) {
                return Err(format!("compatibility: {key} is not a digest"));
            }
            Ok(text.to_owned())
        };
        let state_format_id = value
            .get("stateFormatId")
            .and_then(Value::as_str)
            .ok_or_else(|| "compatibility: stateFormatId is missing".to_owned())?;
        Ok(Compatibility {
            backend_digest: digest("backendDigest")?,
            content_digest: digest("contentDigest")?,
            patch_digest: digest("patchDigest")?,
            controller_digest: digest("controllerDigest")?,
            parser_digest: digest("parserDigest")?,
            state_format_id: parse_id(state_format_id)
                .map_err(|e| format!("compatibility: stateFormatId {e}"))?,
        })
    }

    /// The single digest a participant echoes in its capture and its stage request.
    pub fn digest(&self) -> Digest {
        digest_of(&self.to_json()).expect("a compatibility block canonicalizes")
    }

    /// Names the first identity that differs. There is no tolerance and no "close enough".
    pub fn compare(&self, live: &Compatibility) -> Result<(), String> {
        for (field, recorded, current) in [
            ("stateFormatId", &self.state_format_id, &live.state_format_id),
            ("backend", &self.backend_digest, &live.backend_digest),
            ("content", &self.content_digest, &live.content_digest),
            ("patch", &self.patch_digest, &live.patch_digest),
            ("controller", &self.controller_digest, &live.controller_digest),
            ("parser", &self.parser_digest, &live.parser_digest),
        ] {
            if recorded != current {
                return Err(format!(
                    "the checkpoint's {field} identity {recorded} is not this composition's {current}"
                ));
            }
        }
        Ok(())
    }
}

// ----------------------------------------------------------------------------------------------
// The store manifest

/// One committed generation, as the store manifest lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationRecord {
    pub checkpoint_id: Id,
    /// The generation's file name inside the store directory.
    pub file: String,
    pub session_id: Id,
    pub epoch: Id,
    pub episode_id: Id,
    pub boundary: u64,
    pub compatibility_digest: Digest,
    /// The SHA-256 of the whole envelope file, so a store can compare without opening it.
    pub envelope_digest: Digest,
    pub byte_length: u64,
}

impl GenerationRecord {
    fn to_json(&self) -> Value {
        json!({
            "checkpointId": self.checkpoint_id.as_str(),
            "file": self.file.as_str(),
            "sessionId": self.session_id.as_str(),
            "epoch": self.epoch.as_str(),
            "episodeId": self.episode_id.as_str(),
            "boundary": self.boundary.to_string(),
            "compatibilityDigest": self.compatibility_digest.as_str(),
            "envelopeDigest": self.envelope_digest.as_str(),
            "byteLength": self.byte_length.to_string(),
        })
    }

    fn from_json(value: &Value) -> Result<GenerationRecord, String> {
        let text = |key: &str| -> Result<String, String> {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("store manifest: a generation has no {key}"))
        };
        let number = |key: &str| -> Result<u64, String> {
            text(key)?
                .parse::<u64>()
                .map_err(|_| format!("store manifest: {key} is not a canonical U64"))
        };
        Ok(GenerationRecord {
            checkpoint_id: parse_id(&text("checkpointId")?)?,
            file: text("file")?,
            session_id: parse_id(&text("sessionId")?)?,
            epoch: parse_id(&text("epoch")?)?,
            episode_id: parse_id(&text("episodeId")?)?,
            boundary: number("boundary")?,
            compatibility_digest: text("compatibilityDigest")?,
            envelope_digest: text("envelopeDigest")?,
            byte_length: number("byteLength")?,
        })
    }
}

/// The store's durable metadata: which generations exist and how far durability has reached.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreManifest {
    pub generations: Vec<GenerationRecord>,
    /// The newest committed checkpoint, which is the high-water mark. `None` until the first
    /// durable commit; that is "nothing has been committed", not a default.
    pub high_water: Option<Id>,
}

impl StoreManifest {
    fn to_json(&self) -> Value {
        json!({
            "storeManifestVersion": STORE_MANIFEST_VERSION,
            "stateFormatId": STATE_FORMAT_ID,
            "highWater": self.high_water.as_ref().map_or(Value::Null, |h| h.as_str().into()),
            "generations": Value::Array(self.generations.iter().map(GenerationRecord::to_json).collect()),
        })
    }

    fn from_json(value: &Value) -> Result<StoreManifest, String> {
        let version = value
            .get("storeManifestVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| "store manifest: no storeManifestVersion".to_owned())?;
        if version != u64::from(STORE_MANIFEST_VERSION) {
            return Err(format!("store manifest: unsupported version {version}"));
        }
        match value.get("stateFormatId").and_then(Value::as_str) {
            Some(STATE_FORMAT_ID) => {}
            Some(other) => {
                return Err(format!("store manifest: state format {other} is not {STATE_FORMAT_ID}"));
            }
            None => return Err("store manifest: no stateFormatId".to_owned()),
        }
        let generations = value
            .get("generations")
            .and_then(Value::as_array)
            .ok_or_else(|| "store manifest: generations must be an array".to_owned())?
            .iter()
            .map(GenerationRecord::from_json)
            .collect::<Result<Vec<_>, _>>()?;
        let high_water = match value.get("highWater") {
            Some(Value::Null) | None => None,
            Some(Value::String(s)) => Some(parse_id(s)?),
            Some(_) => return Err("store manifest: highWater is neither null nor an Id".to_owned()),
        };
        if let Some(mark) = &high_water
            && !generations.iter().any(|g| g.checkpoint_id == *mark)
        {
            return Err("store manifest: the high-water mark names no listed generation".to_owned());
        }
        Ok(StoreManifest { generations, high_water })
    }
}

// ----------------------------------------------------------------------------------------------
// The store

/// How many committed generations the store keeps.
#[derive(Clone, Copy, Debug)]
pub struct StoreConfig {
    /// Generations retained after a commit. The oldest are dropped, and only once the
    /// manifest that no longer references them is itself committed.
    pub keep_generations: usize,
}

impl Default for StoreConfig {
    fn default() -> StoreConfig {
        StoreConfig { keep_generations: 3 }
    }
}

/// Deliberate durable-write faults, for the failure rows this slice has to demonstrate.
#[derive(Clone, Debug, Default)]
pub struct StoreFaults {
    /// Stop after the generation file has been renamed and before the store manifest is
    /// committed. The generation is then an unreferenced file, which is never a candidate.
    pub stop_before_manifest_commit: bool,
}

/// The durable checkpoint store: generation files, one store manifest and the commit order of
/// `checkpoint-envelope-v1` section 5.
pub struct CheckpointStore {
    root: PathBuf,
    config: StoreConfig,
    manifest: StoreManifest,
    faults: StoreFaults,
    commits: u64,
    /// Generations the committed manifest no longer references and whose files could not be
    /// removed.
    ///
    /// Rotation happens after the durable commit point, so a file that will not unlink is a
    /// leaked file and never a lost checkpoint. It is recorded rather than swallowed, because
    /// a store that keeps failing to rotate is filling a disk quietly.
    unrotated: Vec<String>,
}

impl CheckpointStore {
    /// Opens or creates a store at `root`, reading whatever it already committed.
    ///
    /// A directory with no manifest is an empty store: nothing has been committed there yet.
    /// A manifest that cannot be read is a failure, not an empty store.
    pub fn open(root: impl Into<PathBuf>, config: StoreConfig) -> DomainResult<CheckpointStore> {
        let root: PathBuf = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| store_error(format!("checkpoint store {}: {e}", root.display())))?;
        let path = root.join(STORE_MANIFEST_FILE);
        let manifest = match std::fs::read(&path) {
            Ok(bytes) => {
                let value: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| store_error(format!("store manifest: {e}")))?;
                StoreManifest::from_json(&value).map_err(store_error)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreManifest::default(),
            Err(e) => return Err(store_error(format!("store manifest: {e}"))),
        };
        Ok(CheckpointStore {
            root,
            config,
            manifest,
            faults: StoreFaults::default(),
            commits: 0,
            unrotated: Vec::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Re-reads the store manifest from disk.
    ///
    /// The store manifest is the durable metadata, and this session is not necessarily the
    /// only thing that has ever written it: a previous run, a repair or an operator may have
    /// committed or removed a generation. Reading it again is how a store finds that out,
    /// rather than trusting a copy it happens to be holding.
    pub fn reload(&mut self) -> DomainResult<()> {
        let reopened = CheckpointStore::open(self.root.clone(), self.config)?;
        self.manifest = reopened.manifest;
        Ok(())
    }

    pub fn manifest(&self) -> &StoreManifest {
        &self.manifest
    }

    pub fn faults_mut(&mut self) -> &mut StoreFaults {
        &mut self.faults
    }

    /// How many durable commits this store has completed.
    pub fn commits(&self) -> u64 {
        self.commits
    }

    /// Generations the committed manifest dropped whose files are still on disk.
    pub fn unrotated(&self) -> &[String] {
        &self.unrotated
    }

    /// The committed generation with this checkpoint id, if the manifest lists it.
    ///
    /// This is the query a coordinator uses to resolve a save whose reply it never saw: it
    /// asks the durable metadata about the *same* operation rather than saving again.
    pub fn lookup(&self, checkpoint_id: &Id) -> Option<&GenerationRecord> {
        self.manifest
            .generations
            .iter()
            .find(|g| g.checkpoint_id == *checkpoint_id)
    }

    /// The newest committed generation, or an explicit refusal when nothing is committed.
    pub fn high_water(&self) -> Option<&GenerationRecord> {
        let mark = self.manifest.high_water.as_ref()?;
        self.lookup(mark)
    }

    /// Selects a restore candidate: the named generation, or the high-water one.
    pub fn select(&self, checkpoint_id: Option<&Id>) -> DomainResult<GenerationRecord> {
        match checkpoint_id {
            Some(wanted) => self.lookup(wanted).cloned().ok_or_else(|| {
                incompatible(format!(
                    "the store has no committed generation {wanted}; an unreferenced \
temporary is never a restore candidate"
                ))
            }),
            None => self.high_water().cloned().ok_or_else(|| {
                incompatible("the store has committed no checkpoint to restore from")
            }),
        }
    }

    /// Reads one committed generation back and validates the whole envelope.
    pub fn read(&self, record: &GenerationRecord) -> DomainResult<Envelope> {
        let path = self.root.join(&record.file);
        let bytes = std::fs::read(&path)
            .map_err(|e| store_error(format!("generation {}: {e}", record.file)))?;
        if bytes.len() as u64 != record.byte_length {
            return Err(incompatible(format!(
                "generation {} is {} bytes; the store manifest records {}",
                record.file,
                bytes.len(),
                record.byte_length
            )));
        }
        if digest_of_bytes(&bytes) != record.envelope_digest {
            return Err(incompatible(format!(
                "generation {} does not match the digest the store manifest records",
                record.file
            )));
        }
        let envelope = checkpoint::decode(&bytes)
            .map_err(|e| incompatible(format!("generation {}: {}", record.file, e.0)))?;
        checkpoint::validate_manifest(&envelope)
            .map_err(|e| incompatible(format!("generation {}: {}", record.file, e.0)))?;
        Ok(envelope)
    }

    /// The durable commit sequence of `checkpoint-envelope-v1` section 5, in that order.
    ///
    /// Blocking by construction: it fsyncs. The writer runs it off the session's runtime.
    fn commit(&mut self, record: GenerationRecord, bytes: &[u8]) -> DomainResult<()> {
        let temporary = self.root.join(format!("tmp-{}.flysess", record.checkpoint_id));
        let final_path = self.root.join(&record.file);
        // 1. write the envelope to a temporary generation file, 2. fsync it
        {
            let mut file = std::fs::File::create(&temporary)
                .map_err(|e| store_error(format!("generation temporary: {e}")))?;
            file.write_all(bytes)
                .map_err(|e| store_error(format!("generation temporary: {e}")))?;
            file.sync_all()
                .map_err(|e| store_error(format!("generation fsync: {e}")))?;
        }
        // 3. rename it to its final generation name, 4. fsync the store directory
        std::fs::rename(&temporary, &final_path)
            .map_err(|e| store_error(format!("generation rename: {e}")))?;
        sync_dir(&self.root)?;
        if self.faults.stop_before_manifest_commit {
            // The generation file exists and nothing references it. It is not a restore
            // candidate and the high-water mark has not moved.
            return Err(store_error(
                "injected failure after the generation was renamed and before the store \
manifest was committed",
            ));
        }
        // 5. write the store manifest to its own temporary, fsync, rename, fsync the directory.
        let mut next = self.manifest.clone();
        next.generations.retain(|g| g.checkpoint_id != record.checkpoint_id);
        next.generations.push(record.clone());
        next.high_water = Some(record.checkpoint_id.clone());
        let dropped = if next.generations.len() > self.config.keep_generations {
            let excess = next.generations.len() - self.config.keep_generations;
            next.generations.drain(..excess).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let text = canonicalize(&next.to_json())
            .map_err(|e| store_error(format!("store manifest: {}", e.0)))?;
        let manifest_temporary = self.root.join("tmp-manifest.json");
        {
            let mut file = std::fs::File::create(&manifest_temporary)
                .map_err(|e| store_error(format!("store manifest temporary: {e}")))?;
            file.write_all(text.as_bytes())
                .map_err(|e| store_error(format!("store manifest temporary: {e}")))?;
            file.sync_all()
                .map_err(|e| store_error(format!("store manifest fsync: {e}")))?;
        }
        std::fs::rename(&manifest_temporary, self.root.join(STORE_MANIFEST_FILE))
            .map_err(|e| store_error(format!("store manifest rename: {e}")))?;
        sync_dir(&self.root)?;
        // Past the durable commit point. Rotation removes only files the committed manifest
        // no longer references.
        self.manifest = next;
        self.commits += 1;
        for old in dropped {
            if std::fs::remove_file(self.root.join(&old.file)).is_err() {
                self.unrotated.push(old.file);
            }
        }
        Ok(())
    }
}

fn sync_dir(path: &Path) -> DomainResult<()> {
    let dir = std::fs::File::open(path)
        .map_err(|e| store_error(format!("store directory {}: {e}", path.display())))?;
    dir.sync_all()
        .map_err(|e| store_error(format!("store directory fsync: {e}")))
}

// ----------------------------------------------------------------------------------------------
// The checkpoint manifest this slice writes and reads

/// One agent's row in a checkpoint manifest.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentEntry {
    pub agent_id: Id,
    pub profile_digest: Digest,
    pub dataset_digest: Digest,
    pub model_version: String,
    pub plasticity_version: String,
    pub seed: i32,
    pub brain_ticks: u64,
    pub remainder: RationalNs,
    pub payload: String,
}

impl AgentEntry {
    fn to_json(&self) -> Value {
        json!({
            "agentId": self.agent_id.as_str(),
            "profileDigest": self.profile_digest.as_str(),
            "datasetDigest": self.dataset_digest.as_str(),
            "modelVersion": self.model_version.as_str(),
            "plasticityVersion": self.plasticity_version.as_str(),
            "seed": self.seed,
            "brainTicks": self.brain_ticks.to_string(),
            "remainder": self.remainder.to_json(),
            "payload": self.payload.as_str(),
        })
    }

    fn from_json(value: &Value) -> Result<AgentEntry, String> {
        let text = |key: &str| -> Result<String, String> {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("checkpoint manifest: an agent row has no {key}"))
        };
        let seed = value
            .get("seed")
            .and_then(Value::as_i64)
            .ok_or_else(|| "checkpoint manifest: an agent row has no seed".to_owned())?;
        let seed = i32::try_from(seed)
            .map_err(|_| "checkpoint manifest: a seed is outside i32".to_owned())?;
        let remainder = RationalNs::from_json(
            value
                .get("remainder")
                .ok_or_else(|| "checkpoint manifest: an agent row has no remainder".to_owned())?,
        )
        .map_err(|e| format!("checkpoint manifest: remainder: {}", e.0))?;
        Ok(AgentEntry {
            agent_id: parse_id(&text("agentId")?)?,
            profile_digest: text("profileDigest")?,
            dataset_digest: text("datasetDigest")?,
            model_version: text("modelVersion")?,
            plasticity_version: text("plasticityVersion")?,
            seed,
            brain_ticks: text("brainTicks")?
                .parse()
                .map_err(|_| "checkpoint manifest: brainTicks is not a canonical U64".to_owned())?,
            remainder,
            payload: text("payload")?,
        })
    }
}

/// The coordinator-owned state a checkpoint records, each as a payload name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoordinatorEntry {
    pub task_ledger: String,
    pub prior_inspection: String,
    pub executor_state: Vec<(Id, String)>,
    pub admission_state: String,
    pub event_watermarks: EventWatermarks,
}

/// The event identity a resumed epoch continues from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventWatermarks {
    /// The highest source step any recorded event belongs to.
    pub last_source_step: u64,
    /// How many events the ledger has issued.
    pub issued: u64,
}

impl EventWatermarks {
    fn to_json(&self) -> Value {
        json!({
            "lastSourceStep": self.last_source_step.to_string(),
            "issued": self.issued.to_string(),
        })
    }

    fn from_json(value: &Value) -> Result<EventWatermarks, String> {
        let number = |key: &str| -> Result<u64, String> {
            value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("checkpoint manifest: eventWatermarks has no {key}"))?
                .parse()
                .map_err(|_| format!("checkpoint manifest: {key} is not a canonical U64"))
        };
        Ok(EventWatermarks {
            last_source_step: number("lastSourceStep")?,
            issued: number("issued")?,
        })
    }
}

impl CoordinatorEntry {
    fn to_json(&self) -> Value {
        json!({
            "taskLedger": self.task_ledger.as_str(),
            "priorInspection": self.prior_inspection.as_str(),
            "executorState": Value::Array(
                self.executor_state
                    .iter()
                    .map(|(agent_id, payload)| json!({
                        "agentId": agent_id.as_str(),
                        "payload": payload.as_str(),
                    }))
                    .collect(),
            ),
            "admissionState": self.admission_state.as_str(),
            "eventWatermarks": self.event_watermarks.to_json(),
        })
    }

    fn from_json(value: &Value) -> Result<CoordinatorEntry, String> {
        let text = |key: &str| -> Result<String, String> {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("checkpoint manifest: coordinator has no {key}"))
        };
        let executors = value
            .get("executorState")
            .and_then(Value::as_array)
            .ok_or_else(|| "checkpoint manifest: executorState must be an array".to_owned())?;
        let mut executor_state = Vec::with_capacity(executors.len());
        for entry in executors {
            let agent_id = entry
                .get("agentId")
                .and_then(Value::as_str)
                .ok_or_else(|| "checkpoint manifest: an executor row has no agentId".to_owned())?;
            let payload = entry
                .get("payload")
                .and_then(Value::as_str)
                .ok_or_else(|| "checkpoint manifest: an executor row has no payload".to_owned())?;
            executor_state.push((parse_id(agent_id)?, payload.to_owned()));
        }
        let watermarks = value
            .get("eventWatermarks")
            .ok_or_else(|| "checkpoint manifest: coordinator has no eventWatermarks".to_owned())?;
        Ok(CoordinatorEntry {
            task_ledger: text("taskLedger")?,
            prior_inspection: text("priorInspection")?,
            executor_state,
            admission_state: text("admissionState")?,
            event_watermarks: EventWatermarks::from_json(watermarks)?,
        })
    }
}

/// The environment's row: which worker the world belonged to and which payload holds it.
///
/// `checkpoint-envelope-v1` section 3 named a holder for every payload except the world's;
/// the 2026-09-22 amendment to that section adds this one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentEntry {
    pub worker_id: Id,
    pub payload: String,
}

impl EnvironmentEntry {
    fn to_json(&self) -> Value {
        json!({
            "workerId": self.worker_id.as_str(),
            "payload": self.payload.as_str(),
        })
    }

    fn from_json(value: &Value) -> Result<EnvironmentEntry, String> {
        let text = |key: &str| -> Result<String, String> {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("checkpoint manifest: environment has no {key}"))
        };
        Ok(EnvironmentEntry {
            worker_id: parse_id(&text("workerId")?)?,
            payload: text("payload")?,
        })
    }
}

/// A complete checkpoint manifest, in the field names `checkpoint-envelope-v1` section 3 sets.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckpointManifest {
    pub checkpoint_id: Id,
    pub source_scope: Scope,
    pub episode_id: Id,
    pub world_time: RationalNs,
    pub scheduler_id: String,
    pub composition_digest: Digest,
    pub port_map: Vec<(Id, Id)>,
    pub compatibility: Compatibility,
    pub agents: Vec<AgentEntry>,
    pub coordinator: CoordinatorEntry,
    pub environment: EnvironmentEntry,
    /// External-helper state required for exact resume, as payload names. The synthetic
    /// composition has no external helper, so it records an empty list rather than omitting
    /// the field: "no helper" is a statement, not a missing one.
    pub helper_state: Vec<String>,
    pub payloads: Vec<(String, u64, Digest)>,
}

impl CheckpointManifest {
    pub fn to_json(&self) -> Value {
        json!({
            "envelopeVersion": checkpoint::VERSION,
            "checkpointId": self.checkpoint_id.as_str(),
            "sourceScope": self.source_scope.to_json(),
            "episodeId": self.episode_id.as_str(),
            "worldTime": self.world_time.to_json(),
            "schedulerId": self.scheduler_id.as_str(),
            "compositionDigest": self.composition_digest.as_str(),
            "portMap": Value::Array(
                self.port_map
                    .iter()
                    .map(|(port_id, agent_id)| json!({
                        "portId": port_id.as_str(),
                        "agentId": agent_id.as_str(),
                    }))
                    .collect(),
            ),
            "compatibility": self.compatibility.to_json(),
            "agents": Value::Array(self.agents.iter().map(AgentEntry::to_json).collect()),
            "coordinator": self.coordinator.to_json(),
            "environment": self.environment.to_json(),
            "helperState": Value::Array(
                self.helper_state.iter().map(|n| Value::String(n.clone())).collect(),
            ),
            "payloads": Value::Array(
                self.payloads
                    .iter()
                    .map(|(name, length, digest)| json!({
                        "name": name.as_str(),
                        "byteLength": length.to_string(),
                        "digest": digest.as_str(),
                    }))
                    .collect(),
            ),
        })
    }

    /// Reads one back, refusing an incomplete manifest rather than filling anything in.
    pub fn from_json(value: &Value) -> Result<CheckpointManifest, String> {
        for field in checkpoint::REQUIRED_MANIFEST_FIELDS {
            if value.get(*field).is_none() {
                return Err(format!("checkpoint manifest: missing {field:?}"));
            }
        }
        let text = |key: &str| -> Result<String, String> {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("checkpoint manifest: {key} is missing or not a string"))
        };
        let source_scope = Scope::from_json(&value["sourceScope"])
            .map_err(|e| format!("checkpoint manifest: sourceScope: {}", e.0))?;
        let world_time = RationalNs::from_json(&value["worldTime"])
            .map_err(|e| format!("checkpoint manifest: worldTime: {}", e.0))?;
        let mut port_map = Vec::new();
        for entry in value["portMap"]
            .as_array()
            .ok_or_else(|| "checkpoint manifest: portMap must be an array".to_owned())?
        {
            let port_id = entry
                .get("portId")
                .and_then(Value::as_str)
                .ok_or_else(|| "checkpoint manifest: a port map row has no portId".to_owned())?;
            let agent_id = entry
                .get("agentId")
                .and_then(Value::as_str)
                .ok_or_else(|| "checkpoint manifest: a port map row has no agentId".to_owned())?;
            port_map.push((parse_id(port_id)?, parse_id(agent_id)?));
        }
        let agents = value["agents"]
            .as_array()
            .ok_or_else(|| "checkpoint manifest: agents must be an array".to_owned())?
            .iter()
            .map(AgentEntry::from_json)
            .collect::<Result<Vec<_>, _>>()?;
        let mut helper_state = Vec::new();
        for entry in value["helperState"]
            .as_array()
            .ok_or_else(|| "checkpoint manifest: helperState must be an array".to_owned())?
        {
            helper_state.push(
                entry
                    .as_str()
                    .ok_or_else(|| "checkpoint manifest: a helper state entry is not a payload name".to_owned())?
                    .to_owned(),
            );
        }
        let mut payloads = Vec::new();
        for entry in value["payloads"]
            .as_array()
            .ok_or_else(|| "checkpoint manifest: payloads must be an array".to_owned())?
        {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "checkpoint manifest: a payload row has no name".to_owned())?;
            let length: u64 = entry
                .get("byteLength")
                .and_then(Value::as_str)
                .ok_or_else(|| "checkpoint manifest: a payload row has no byteLength".to_owned())?
                .parse()
                .map_err(|_| "checkpoint manifest: byteLength is not a canonical U64".to_owned())?;
            let digest = entry
                .get("digest")
                .and_then(Value::as_str)
                .ok_or_else(|| "checkpoint manifest: a payload row has no digest".to_owned())?;
            payloads.push((name.to_owned(), length, digest.to_owned()));
        }
        Ok(CheckpointManifest {
            checkpoint_id: parse_id(&text("checkpointId")?)?,
            source_scope,
            episode_id: parse_id(&text("episodeId")?)?,
            world_time,
            scheduler_id: text("schedulerId")?,
            composition_digest: text("compositionDigest")?,
            port_map,
            compatibility: Compatibility::from_json(&value["compatibility"])?,
            agents,
            coordinator: CoordinatorEntry::from_json(&value["coordinator"])?,
            environment: EnvironmentEntry::from_json(
                value
                    .get("environment")
                    .ok_or_else(|| "checkpoint manifest: missing \"environment\"".to_owned())?,
            )?,
            helper_state,
            payloads,
        })
    }

    /// The payload name this manifest files one participant's state under.
    pub fn payload_of(&self, worker_id: &Id) -> Option<&str> {
        if self.environment.worker_id == *worker_id {
            return Some(self.environment.payload.as_str());
        }
        self.agents
            .iter()
            .find(|a| a.agent_id == *worker_id)
            .map(|a| a.payload.as_str())
    }
}

// ----------------------------------------------------------------------------------------------
// The bounded writer

/// One participant's captured payload, with the owned handle the writer keeps until the bytes
/// are committed or the job fails.
pub struct CapturedPayload {
    pub name: String,
    pub artifact: flybus::Artifact,
    pub byte_length: u64,
    pub digest: Digest,
}

/// What a coordinator hands the writer once every participant has captured.
pub struct CaptureSubmission {
    pub checkpoint_id: Id,
    pub boundary: u64,
    pub session_id: Id,
    pub epoch: Id,
    pub episode_id: Id,
    pub compatibility_digest: Digest,
    pub manifest: Value,
    pub payloads: Vec<CapturedPayload>,
    /// A hot checkpoint may replace a queued hot checkpoint, releasing its holds. A durable
    /// one never is: the retention table coalesces only queued replaceable captures.
    pub replaceable: bool,
}

impl CaptureSubmission {
    fn byte_length(&self) -> u64 {
        self.payloads.iter().map(|p| p.byte_length).sum()
    }
}

/// How one durable save ended. Every variant is a statement; none of them is a default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Past the store manifest rename. This is the only variant that is a saved
    /// acknowledgment and the only one that moves a high-water mark.
    Committed { checkpoint_id: Id, boundary: u64, file: String },
    /// The write failed and its owned captures were released under the retry policy. It
    /// never reports false durability.
    Failed { checkpoint_id: Id, reason: String },
    /// A later replaceable capture took this one's place in the queue before it was written.
    Superseded { checkpoint_id: Id, by: Id },
    /// The writer's reply never arrived. The operation's outcome is unknown from here, so
    /// durable metadata does not move; the caller resolves the *same* operation against the
    /// store manifest instead of saving again.
    ReplyLost { checkpoint_id: Id },
}

impl SaveOutcome {
    pub fn checkpoint_id(&self) -> &Id {
        match self {
            SaveOutcome::Committed { checkpoint_id, .. }
            | SaveOutcome::Failed { checkpoint_id, .. }
            | SaveOutcome::Superseded { checkpoint_id, .. }
            | SaveOutcome::ReplyLost { checkpoint_id } => checkpoint_id,
        }
    }

    /// The event name this outcome publishes under.
    pub fn event(&self) -> &'static str {
        match self {
            SaveOutcome::Committed { .. } => "committed",
            SaveOutcome::Failed { .. } => "failed",
            SaveOutcome::Superseded { .. } => "superseded",
            SaveOutcome::ReplyLost { .. } => "failed",
        }
    }
}

/// What a failed write does with the ephemeral captures it owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryPolicy {
    /// Release the owned captures and report the failure. The default: a capture is cheap to
    /// take again at the next committed boundary, and holding one is not free.
    ReleaseAndReport,
    /// Try the durable sequence again, up to `attempts` times in total, keeping the owned
    /// captures until they are committed or the attempts are spent.
    RetryThenRelease { attempts: u32 },
}

/// The writer's bounds. Both are finite and both refuse before a capture is requested.
#[derive(Clone, Copy, Debug)]
pub struct WriterConfig {
    /// Outstanding coherent captures. `state-media-v1` section 3's initial session default
    /// is two.
    pub queue_capacity: usize,
    /// The total payload bytes the queue may hold.
    pub max_queued_bytes: u64,
    pub retry: RetryPolicy,
}

impl Default for WriterConfig {
    fn default() -> WriterConfig {
        WriterConfig {
            queue_capacity: 2,
            max_queued_bytes: 64 * 1024 * 1024,
            retry: RetryPolicy::ReleaseAndReport,
        }
    }
}

/// Deliberate writer faults, for the rows this slice has to demonstrate.
#[derive(Clone, Default)]
pub struct WriterFaults {
    /// Hold every job until the gate is opened, so a test can fill the queue on purpose.
    pub gate: Option<Arc<tokio::sync::Semaphore>>,
    /// Drop this job's reply channel after the durable sequence ran, which is a lost save
    /// reply.
    pub drop_reply_for: Option<Id>,
}

impl std::fmt::Debug for WriterFaults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterFaults")
            .field("gate", &self.gate.is_some())
            .field("drop_reply_for", &self.drop_reply_for)
            .finish()
    }
}

/// The writer's own counters, so "bounded" is something a test reads rather than believes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriterStats {
    pub queued: u64,
    pub committed: u64,
    pub failed: u64,
    pub superseded: u64,
    /// Submissions refused because the queue or its byte budget was full.
    pub rejected: u64,
    /// Checkpoint events the bus would not take. The durable outcome the caller receives is
    /// the authority; a publication that failed is counted here rather than disappearing.
    pub events_dropped: u64,
    /// The deepest the queue ever got.
    pub peak_queue: usize,
    /// The most payload bytes the queue ever held.
    pub peak_bytes: u64,
}

struct Job {
    submission: CaptureSubmission,
    reply: tokio::sync::oneshot::Sender<SaveOutcome>,
    permit: tokio::sync::OwnedSemaphorePermit,
}

struct WriterShared {
    config: WriterConfig,
    faults: WriterFaults,
    permits: Arc<tokio::sync::Semaphore>,
    queue: std::sync::Mutex<VecDeque<Job>>,
    queued_bytes: AtomicU64,
    stats: std::sync::Mutex<WriterStats>,
    wake: tokio::sync::Notify,
    stop: AtomicBool,
    store: Arc<std::sync::Mutex<CheckpointStore>>,
    events: Option<(flybus::Client, String)>,
}

/// A queue slot, taken before a capture is requested so a saturated writer refuses early.
///
/// `state-media-v1` section 3's durable row says to reject or defer *before capture* when
/// saturated. Holding the slot from before `State.Capture` until the outcome is delivered is
/// what makes that true rather than aspirational.
pub struct Reservation {
    permit: tokio::sync::OwnedSemaphorePermit,
}

/// The bounded checkpoint writer.
pub struct CheckpointWriter {
    shared: Arc<WriterShared>,
    task: tokio::task::JoinHandle<()>,
}

impl CheckpointWriter {
    /// Starts the writer over `store`. `events` is the bus client and topic the distinct
    /// captured/queued/committed/failed/superseded publications go to.
    pub fn start(
        store: CheckpointStore,
        config: WriterConfig,
        faults: WriterFaults,
        events: Option<(flybus::Client, String)>,
    ) -> CheckpointWriter {
        let shared = Arc::new(WriterShared {
            config,
            faults,
            permits: Arc::new(tokio::sync::Semaphore::new(config.queue_capacity)),
            queue: std::sync::Mutex::new(VecDeque::new()),
            queued_bytes: AtomicU64::new(0),
            stats: std::sync::Mutex::new(WriterStats::default()),
            wake: tokio::sync::Notify::new(),
            stop: AtomicBool::new(false),
            store: Arc::new(std::sync::Mutex::new(store)),
            events,
        });
        let task = tokio::spawn(run_writer(shared.clone()));
        CheckpointWriter { shared, task }
    }

    pub fn stats(&self) -> WriterStats {
        *self.shared.stats.lock().expect("the writer stats are never poisoned")
    }

    pub fn config(&self) -> WriterConfig {
        self.shared.config
    }

    /// How many captures are outstanding: queued or being written.
    pub fn outstanding(&self) -> usize {
        self.shared.config.queue_capacity - self.shared.permits.available_permits()
    }

    /// Takes a queue slot, or refuses by name. Nothing is captured without one.
    pub fn reserve(&self) -> DomainResult<Reservation> {
        match self.shared.permits.clone().try_acquire_owned() {
            Ok(permit) => Ok(Reservation { permit }),
            Err(_) => {
                self.shared
                    .stats
                    .lock()
                    .expect("the writer stats are never poisoned")
                    .rejected += 1;
                Err(DomainError::before(
                    ErrorCode::Busy,
                    format!(
                        "the checkpoint queue already holds its {} outstanding captures; a \
capture is refused before it is requested rather than queued without bound",
                        self.shared.config.queue_capacity
                    ),
                ))
            }
        }
    }

    /// Hands one complete capture to the writer and returns the channel its outcome arrives
    /// on. The reservation becomes the job's slot.
    pub async fn submit(
        &self,
        reservation: Reservation,
        submission: CaptureSubmission,
    ) -> DomainResult<tokio::sync::oneshot::Receiver<SaveOutcome>> {
        let bytes = submission.byte_length();
        let budget = self.shared.config.max_queued_bytes;
        let held = self.shared.queued_bytes.load(Ordering::SeqCst);
        if held + bytes > budget {
            self.shared
                .stats
                .lock()
                .expect("the writer stats are never poisoned")
                .rejected += 1;
            return Err(DomainError::before(
                ErrorCode::Busy,
                format!(
                    "the checkpoint queue holds {held} of {budget} bytes and this capture adds \
{bytes}; the byte budget is finite and refuses before it is exceeded"
                ),
            ));
        }
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let checkpoint_id = submission.checkpoint_id.clone();
        let queued_event = submission.as_event("queued");
        let superseded = {
            let mut queue = self.shared.queue.lock().expect("the writer queue is never poisoned");
            let replaced = if submission.replaceable {
                queue
                    .iter()
                    .position(|job| job.submission.replaceable)
                    .map(|index| queue.remove(index).expect("just found"))
            } else {
                None
            };
            queue.push_back(Job { submission, reply, permit: reservation.permit });
            self.shared.queued_bytes.fetch_add(bytes, Ordering::SeqCst);
            let mut stats = self.shared.stats.lock().expect("the writer stats are never poisoned");
            stats.queued += 1;
            stats.peak_queue = stats.peak_queue.max(queue.len());
            stats.peak_bytes = stats
                .peak_bytes
                .max(self.shared.queued_bytes.load(Ordering::SeqCst));
            replaced
        };
        if let Some(old) = superseded {
            self.shared
                .queued_bytes
                .fetch_sub(old.submission.byte_length(), Ordering::SeqCst);
            self.shared
                .stats
                .lock()
                .expect("the writer stats are never poisoned")
                .superseded += 1;
            let outcome = SaveOutcome::Superseded {
                checkpoint_id: old.submission.checkpoint_id.clone(),
                by: checkpoint_id.clone(),
            };
            publish_event(
                &self.shared.events,
                &self.shared.stats,
                &outcome_event(&old.submission, &outcome),
            )
            .await;
            // A caller that dropped its ticket is not waiting for this; the store's own
            // metadata is the durable record either way.
            let _ = old.reply.send(outcome);
            // Dropping the job releases its owned captures and its queue slot.
            drop(old.submission);
            drop(old.permit);
        }
        publish_event(&self.shared.events, &self.shared.stats, &object(queued_event)).await;
        self.shared.wake.notify_one();
        Ok(receiver)
    }

    /// Waits for one save's outcome. A dropped reply channel is a lost save reply, which is
    /// an outcome and not a hang.
    pub async fn wait(
        receiver: tokio::sync::oneshot::Receiver<SaveOutcome>,
        checkpoint_id: &Id,
        budget: std::time::Duration,
    ) -> SaveOutcome {
        match tokio::time::timeout(budget, receiver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => SaveOutcome::ReplyLost { checkpoint_id: checkpoint_id.clone() },
        }
    }

    /// Runs `f` against the store, which is how a caller resolves a save whose reply it lost.
    ///
    /// The store's own work is blocking -- it fsyncs -- so it is reached on a blocking thread
    /// rather than from the session's runtime.
    pub async fn with_store<T, F>(&self, f: F) -> T
    where
        F: FnOnce(&mut CheckpointStore) -> T + Send + 'static,
        T: Send + 'static,
    {
        let store = self.shared.store.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = store.lock().expect("the checkpoint store is never poisoned");
            f(&mut store)
        })
        .await
        .expect("the checkpoint store task is never cancelled")
    }

    /// Stops the writer and waits for its task. A leftover writer task would hold artifact
    /// handles the session has finished with.
    pub async fn shutdown(self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.wake.notify_one();
        let _ = self.task.await;
        let mut queue = self.shared.queue.lock().expect("the writer queue is never poisoned");
        queue.clear();
    }
}

impl CaptureSubmission {
    fn as_event(&self, event: &'static str) -> Value {
        json!({
            "event": event,
            "checkpointId": self.checkpoint_id.as_str(),
            "sessionId": self.session_id.as_str(),
            "epoch": self.epoch.as_str(),
            "episodeId": self.episode_id.as_str(),
            "boundary": self.boundary.to_string(),
            "byteLength": self.byte_length().to_string(),
        })
    }
}

fn outcome_event(submission: &CaptureSubmission, outcome: &SaveOutcome) -> Map<String, Value> {
    let mut payload = object(submission.as_event(outcome.event()));
    match outcome {
        SaveOutcome::Committed { file, .. } => {
            payload.insert("generation".into(), file.as_str().into());
            payload.insert("durable".into(), true.into());
        }
        SaveOutcome::Failed { reason, .. } => {
            payload.insert("reason".into(), reason.as_str().into());
            payload.insert("durable".into(), false.into());
        }
        SaveOutcome::Superseded { by, .. } => {
            payload.insert("supersededBy".into(), by.as_str().into());
            payload.insert("durable".into(), false.into());
        }
        SaveOutcome::ReplyLost { .. } => {
            payload.insert("reason".into(), "the save reply was lost".into());
            payload.insert("durable".into(), false.into());
        }
    }
    payload
}

async fn publish_event(
    events: &Option<(flybus::Client, String)>,
    stats: &std::sync::Mutex<WriterStats>,
    payload: &Map<String, Value>,
) {
    let Some((client, topic)) = events else { return };
    // A checkpoint event is telemetry about the store, not the durable record. A publication
    // that cannot be delivered never changes what was committed, so the outcome the caller
    // receives stays the authority -- and the drop is counted rather than retried, because a
    // retry here would be exactly the implicit best-effort policy these contracts refuse.
    if client.publish(topic, payload.clone(), &[]).await.is_err() {
        stats
            .lock()
            .expect("the writer stats are never poisoned")
            .events_dropped += 1;
    }
}

async fn run_writer(shared: Arc<WriterShared>) {
    loop {
        let job = {
            let mut queue = shared.queue.lock().expect("the writer queue is never poisoned");
            queue.pop_front()
        };
        let Some(job) = job else {
            if shared.stop.load(Ordering::SeqCst) {
                return;
            }
            shared.wake.notified().await;
            continue;
        };
        if let Some(gate) = &shared.faults.gate {
            // A deliberately stalled writer. The queue in front of it stays bounded, which is
            // the point of the stall.
            if let Ok(permit) = gate.acquire().await {
                permit.forget();
            }
        }
        let Job { submission, reply, permit } = job;
        shared
            .queued_bytes
            .fetch_sub(submission.byte_length(), Ordering::SeqCst);
        let outcome = write_one(&shared, &submission).await;
        {
            let mut stats = shared.stats.lock().expect("the writer stats are never poisoned");
            match &outcome {
                SaveOutcome::Committed { .. } => stats.committed += 1,
                SaveOutcome::Failed { .. } | SaveOutcome::ReplyLost { .. } => stats.failed += 1,
                SaveOutcome::Superseded { .. } => stats.superseded += 1,
            }
        }
        publish_event(&shared.events, &shared.stats, &outcome_event(&submission, &outcome)).await;
        let lost = shared.faults.drop_reply_for.as_ref() == Some(&submission.checkpoint_id);
        // The writer owned these handles until the bytes were committed or the job failed.
        // The job is over, so they and its queue slot go before the outcome is delivered:
        // a caller that reads the queue depth the moment its outcome arrives must not see a
        // slot this job has finished with.
        drop(submission);
        drop(permit);
        if lost {
            // The bytes are committed and the acknowledgment is lost. Durable metadata is in
            // the store manifest, which is exactly where the caller must look.
            drop(reply);
        } else {
            // A caller that dropped its ticket is not waiting for this.
            let _ = reply.send(outcome);
        }
    }
}

async fn write_one(shared: &Arc<WriterShared>, submission: &CaptureSubmission) -> SaveOutcome {
    let mut payloads = Vec::with_capacity(submission.payloads.len());
    for payload in &submission.payloads {
        let bytes = match payload.artifact.read_all().await {
            Ok(bytes) => bytes,
            Err(e) => {
                return SaveOutcome::Failed {
                    checkpoint_id: submission.checkpoint_id.clone(),
                    reason: format!("payload {}: {}", payload.name, e.message),
                };
            }
        };
        if bytes.len() as u64 != payload.byte_length || digest_of_bytes(&bytes) != payload.digest {
            return SaveOutcome::Failed {
                checkpoint_id: submission.checkpoint_id.clone(),
                reason: format!(
                    "payload {} is not the content its capture declared",
                    payload.name
                ),
            };
        }
        payloads.push((payload.name.clone(), bytes));
    }
    let bytes = match checkpoint::encode(&submission.manifest, &payloads) {
        Ok(bytes) => bytes,
        Err(e) => {
            return SaveOutcome::Failed {
                checkpoint_id: submission.checkpoint_id.clone(),
                reason: format!("envelope: {}", e.0),
            };
        }
    };
    let record = GenerationRecord {
        checkpoint_id: submission.checkpoint_id.clone(),
        file: format!("{}.flysess", submission.checkpoint_id),
        session_id: submission.session_id.clone(),
        epoch: submission.epoch.clone(),
        episode_id: submission.episode_id.clone(),
        boundary: submission.boundary,
        compatibility_digest: submission.compatibility_digest.clone(),
        envelope_digest: digest_of_bytes(&bytes),
        byte_length: bytes.len() as u64,
    };
    let attempts = match shared.config.retry {
        RetryPolicy::ReleaseAndReport => 1,
        RetryPolicy::RetryThenRelease { attempts } => attempts.max(1),
    };
    let mut last = String::new();
    for _ in 0..attempts {
        let record = record.clone();
        let bytes = bytes.clone();
        let store = shared.store.clone();
        // The durable sequence fsyncs twice. It runs on a blocking thread so the session's
        // runtime is never the thing waiting on a disk.
        let result = tokio::task::spawn_blocking(move || {
            let mut store = store.lock().expect("the checkpoint store is never poisoned");
            store.commit(record, &bytes)
        })
        .await;
        match result {
            Ok(Ok(())) => {
                return SaveOutcome::Committed {
                    checkpoint_id: submission.checkpoint_id.clone(),
                    boundary: submission.boundary,
                    file: format!("{}.flysess", submission.checkpoint_id),
                };
            }
            Ok(Err(e)) => last = e.message,
            Err(e) => last = format!("the checkpoint writer stopped: {e}"),
        }
    }
    SaveOutcome::Failed {
        checkpoint_id: submission.checkpoint_id.clone(),
        reason: last,
    }
}
