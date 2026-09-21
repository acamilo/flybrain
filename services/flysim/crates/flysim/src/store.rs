//! Checkpoint persistence: the `FLYSIM01` envelope, the atomic commit, the generations and the
//! milestone archives.
//!
//! `docs/design/flysim.md` section 8, as amended by the contract override at the top of that
//! document:
//!
//! - the envelope is `flybrain-core`'s frozen `encodeEnvelope` layout with the magic `FLYSIM01`,
//!   carrying the seven agent chunks plus `emulator`, `framebuffer`, `ratchetGame` and
//!   `ratchetFrame`;
//! - a commit writes `<gen>.checkpoint.tmp`, fsyncs it, renames it, fsyncs the directory, and
//!   then does the same for `manifest.json`. **The manifest rename is the commit point**: a
//!   crash before it leaves an unreferenced file that is never loaded;
//! - durable commits go to `save_dir` every `checkpoint_seconds` (default 300) and on every
//!   event that matters; a hot copy goes to `hot_dir` (tmpfs) every `hot_seconds` (default 5);
//! - the first commit at a new best rank also writes `milestone-<rank>.checkpoint`, which later
//!   rotations never unlink.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use flybrain_core::envelope::{EnvelopeParts, decode_envelope, encode_envelope};
use flybrain_core::json::JsonValue;
use serde::{Deserialize, Serialize};

/// Envelope magic. Chunk names stay `[a-zA-Z]+` so the TypeScript `decodeEnvelope` can read a
/// flysim checkpoint for recap and inspection tooling.
pub const MAGIC: &str = "FLYSIM01";

/// Chunk names flysim adds to `flybrain-core`'s `AGENT_CHUNK_NAMES`.
pub const EMULATOR_CHUNK: &str = "emulator";
pub const FRAMEBUFFER_CHUNK: &str = "framebuffer";
pub const RATCHET_GAME_CHUNK: &str = "ratchetGame";
pub const RATCHET_FRAME_CHUNK: &str = "ratchetFrame";

/// `manifest.json`: which generations are current, and which are archived forever.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreManifest {
    /// Highest generation ever allocated in this store.
    pub generation: u64,
    /// The generation to restore first.
    pub latest: Option<u64>,
    /// The generation to restore if `latest` will not load.
    pub previous: Option<u64>,
    /// Best-rank archives, rank to generation. Their files are never rotated away.
    pub archives: BTreeMap<u32, u64>,
}

impl StoreManifest {
    fn record(&mut self, generation: u64, archive_rank: Option<u32>) {
        self.generation = self.generation.max(generation);
        if self.latest != Some(generation) {
            self.previous = self.latest;
            self.latest = Some(generation);
        }
        if let Some(rank) = archive_rank {
            self.archives.insert(rank, generation);
        }
    }

    /// Generations that must survive rotation.
    fn referenced(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self.latest.into_iter().chain(self.previous).collect();
        out.extend(self.archives.values().copied());
        out
    }
}

/// One restore candidate, in the order it should be tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    /// Human description for the log and for `/status`'s fallback reporting.
    pub origin: String,
    pub generation: Option<u64>,
}

/// A checkpoint directory: `<gen>.checkpoint`, `milestone-<rank>.checkpoint`, `manifest.json`.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
    keep_generations: usize,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>, keep_generations: usize) -> Self {
        Self { dir: dir.into(), keep_generations: keep_generations.max(1) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn create(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating checkpoint directory {}", self.dir.display()))
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    pub fn generation_path(&self, generation: u64) -> PathBuf {
        self.dir.join(format!("{generation}.checkpoint"))
    }

    pub fn archive_path(&self, rank: u32) -> PathBuf {
        self.dir.join(format!("milestone-{rank}.checkpoint"))
    }

    /// Read `manifest.json`, or `None` when it is absent or unreadable.
    pub fn manifest(&self) -> Option<StoreManifest> {
        let text = std::fs::read_to_string(self.manifest_path()).ok()?;
        match serde_json::from_str(&text) {
            Ok(manifest) => Some(manifest),
            Err(error) => {
                tracing::warn!(%error, path = %self.manifest_path().display(), "unreadable store manifest");
                None
            }
        }
    }

    /// Highest generation the store has ever allocated, from the manifest and from the files on
    /// disk (an unreferenced file from a crashed commit still counts, so a generation number is
    /// never reused).
    pub fn highest_generation(&self) -> u64 {
        let mut highest = self.manifest().map_or(0, |manifest| manifest.generation);
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                if let Some(generation) = parse_generation(&entry.file_name().to_string_lossy()) {
                    highest = highest.max(generation);
                }
            }
        }
        highest
    }

    /// Restore candidates from this store, in order: latest, previous, then the archives by
    /// descending rank.
    pub fn candidates(&self, label: &str) -> Vec<Candidate> {
        let Some(manifest) = self.manifest() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (kind, generation) in [("latest", manifest.latest), ("previous", manifest.previous)] {
            if let Some(generation) = generation {
                out.push(Candidate {
                    path: self.generation_path(generation),
                    origin: format!("{label} {kind} generation {generation}"),
                    generation: Some(generation),
                });
            }
        }
        for (rank, generation) in manifest.archives.iter().rev() {
            out.push(Candidate {
                path: self.generation_path(*generation),
                origin: format!("{label} archived generation {generation} (rank {rank})"),
                generation: Some(*generation),
            });
            out.push(Candidate {
                path: self.archive_path(*rank),
                origin: format!("{label} milestone archive rank {rank}"),
                generation: Some(*generation),
            });
        }
        out.retain(|candidate| candidate.path.exists());
        out
    }

    /// Write `bytes` as `generation`, commit it, archive it if `archive_rank` is set, then rotate.
    ///
    /// The sequence is exactly section 8's: payload tmp, fsync, rename, dir fsync, then manifest
    /// tmp, fsync, rename, dir fsync.
    pub fn commit(
        &self,
        generation: u64,
        bytes: &[u8],
        archive_rank: Option<u32>,
    ) -> Result<StoreManifest> {
        self.create()?;
        write_atomic(&self.generation_path(generation), bytes)?;
        if let Some(rank) = archive_rank {
            write_atomic(&self.archive_path(rank), bytes)?;
        }
        let mut manifest = self.manifest().unwrap_or_default();
        manifest.record(generation, archive_rank);
        let text = serde_json::to_vec_pretty(&manifest)?;
        write_atomic(&self.manifest_path(), &text)?;
        self.rotate(&manifest)?;
        Ok(manifest)
    }

    /// Stage a payload without committing it. Only the crash-simulation test uses this: it is
    /// the state a `kill -9` between the tmp write and the rename leaves behind.
    pub fn stage_for_test(&self, generation: u64, bytes: &[u8]) -> Result<PathBuf> {
        self.create()?;
        let path = tmp_path(&self.generation_path(generation));
        write_and_sync(&path, bytes)?;
        Ok(path)
    }

    /// Remove generation files that the manifest no longer references, keeping the newest
    /// `keep_generations` and never touching an archive.
    fn rotate(&self, manifest: &StoreManifest) -> Result<()> {
        let referenced = manifest.referenced();
        let mut generations: Vec<u64> = Vec::new();
        for entry in std::fs::read_dir(&self.dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(generation) = parse_generation(&name) {
                generations.push(generation);
            } else if name.ends_with(".checkpoint.tmp") {
                // A tmp file from an interrupted commit is never loadable; clean it up.
                let _ = std::fs::remove_file(entry.path());
            }
        }
        generations.sort_unstable_by(|a, b| b.cmp(a));
        for (index, generation) in generations.iter().enumerate() {
            if referenced.contains(generation) || index < self.keep_generations {
                continue;
            }
            let path = self.generation_path(*generation);
            if let Err(error) = std::fs::remove_file(&path) {
                tracing::warn!(%error, path = %path.display(), "could not rotate a checkpoint");
            }
        }
        Ok(())
    }
}

/// `milestone-<rank>.checkpoint` is an independent file, so it is not a generation.
fn parse_generation(name: &str) -> Option<u64> {
    name.strip_suffix(".checkpoint")?.parse().ok()
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

/// tmp write, fsync, rename, directory fsync.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = tmp_path(path);
    write_and_sync(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all().with_context(|| format!("fsync {}", path.display()))?;
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<()> {
    // Opening a directory read-only and fsyncing it is what makes the rename durable.
    File::open(dir)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("fsync directory {}", dir.display()))
}

// ---------------------------------------------------------------------------------------------
// The envelope
// ---------------------------------------------------------------------------------------------

/// Everything a checkpoint carries besides the agent's own chunks.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeState {
    pub generation: u64,
    pub wall_ms: u64,
    pub rom_sha256: String,
    pub emulator_frame: u64,
    pub compatibility: String,
    pub speed: f64,
    pub buttons: u32,
    /// Simulated milliseconds at which the current milestone rank was reached.
    pub rank_since_ms: f64,
    pub last_event_id: u64,
    /// The adapter's own lifetime reward state.
    pub reward: serde_json::Value,
    pub ratchet: flybrain_gb::RatchetState,
    pub emulator: Vec<u8>,
    pub framebuffer: Vec<u8>,
    /// The ratchet's best safe snapshot, if it has one.
    pub ratchet_game: Vec<u8>,
    pub ratchet_frame: Vec<u8>,
}

/// A decoded checkpoint: the agent half plus the runtime half.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub agent: flybrain_core::agent::AgentState,
    pub runtime: RuntimeState,
}

/// Encode an agent state plus the runtime state into one `FLYSIM01` envelope.
pub fn encode(
    agent: &flybrain_core::agent::AgentState,
    runtime: &RuntimeState,
) -> Result<Vec<u8>> {
    let parts = flybrain_core::envelope::agent_to_chunks(agent);
    let mut manifest = parts.manifest;
    manifest.set("generation", (runtime.generation as f64).into());
    manifest.set("wallMs", (runtime.wall_ms as f64).into());
    manifest.set("romHash", runtime.rom_sha256.as_str().into());
    manifest.set("emulatorFrame", (runtime.emulator_frame as f64).into());
    manifest.set("compatibility", runtime.compatibility.as_str().into());
    manifest.set("speed", runtime.speed.into());
    manifest.set("buttons", f64::from(runtime.buttons).into());
    manifest.set("rankSinceMs", runtime.rank_since_ms.into());
    manifest.set("lastEventId", (runtime.last_event_id as f64).into());
    manifest.set("reward", to_json_value(&runtime.reward)?);
    manifest.set("ratchet", to_json_value(&serde_json::to_value(runtime.ratchet)?)?);

    let mut chunks = parts.chunks;
    chunks.push((EMULATOR_CHUNK.to_string(), runtime.emulator.clone()));
    chunks.push((FRAMEBUFFER_CHUNK.to_string(), runtime.framebuffer.clone()));
    chunks.push((RATCHET_GAME_CHUNK.to_string(), runtime.ratchet_game.clone()));
    chunks.push((RATCHET_FRAME_CHUNK.to_string(), runtime.ratchet_frame.clone()));

    encode_envelope(MAGIC, &manifest, &chunks).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Decode and structurally validate a `FLYSIM01` envelope.
///
/// Value-level validation belongs to the importers: `NeuralAgent::import_state` is
/// self-validating and a no-op on failure, and `Ratchet::import` rejects an impossible state.
pub fn decode(bytes: &[u8]) -> Result<Checkpoint> {
    let parts: EnvelopeParts =
        decode_envelope(bytes, MAGIC).map_err(|error| anyhow::anyhow!("{error}"))?;
    let manifest = parts.manifest.clone();
    let agent = flybrain_core::envelope::agent_from_chunks(&manifest, &parts)
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    let string = |key: &str| -> Result<String> {
        manifest
            .get(key)
            .and_then(JsonValue::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("checkpoint manifest is missing {key}"))
    };
    let number = |key: &str| -> Result<f64> {
        manifest
            .get(key)
            .and_then(JsonValue::as_f64)
            .ok_or_else(|| anyhow::anyhow!("checkpoint manifest is missing {key}"))
    };
    let chunk = |name: &str| -> Result<Vec<u8>> {
        parts
            .chunk(name)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| anyhow::anyhow!("checkpoint is missing the {name} chunk"))
    };

    let ratchet_json = manifest
        .get("ratchet")
        .ok_or_else(|| anyhow::anyhow!("checkpoint manifest is missing ratchet"))?;
    let ratchet: flybrain_gb::RatchetState = serde_json::from_str(&ratchet_json.stringify())
        .context("checkpoint ratchet state is not readable")?;
    let reward = manifest
        .get("reward")
        .map(|value| serde_json::from_str(&value.stringify()))
        .transpose()
        .context("checkpoint reward state is not readable")?
        .unwrap_or(serde_json::Value::Null);

    Ok(Checkpoint {
        agent,
        runtime: RuntimeState {
            generation: number("generation")? as u64,
            wall_ms: number("wallMs")? as u64,
            rom_sha256: string("romHash")?,
            emulator_frame: number("emulatorFrame")? as u64,
            compatibility: string("compatibility")?,
            speed: number("speed")?,
            buttons: number("buttons")? as u32,
            rank_since_ms: number("rankSinceMs").unwrap_or(0.0),
            last_event_id: number("lastEventId").unwrap_or(0.0) as u64,
            reward,
            ratchet,
            emulator: chunk(EMULATOR_CHUNK)?,
            framebuffer: chunk(FRAMEBUFFER_CHUNK)?,
            ratchet_game: chunk(RATCHET_GAME_CHUNK)?,
            ratchet_frame: chunk(RATCHET_FRAME_CHUNK)?,
        },
    })
}

/// Read and decode a candidate, so a caller can try the next one on any failure.
pub fn load(path: &Path) -> Result<Checkpoint> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading checkpoint {}", path.display()))?;
    decode(&bytes).with_context(|| format!("decoding checkpoint {}", path.display()))
}

fn to_json_value(value: &serde_json::Value) -> Result<JsonValue> {
    JsonValue::parse(&serde_json::to_string(value)?).map_err(|error| anyhow::anyhow!("{error}"))
}

/// The restore order: the tmpfs hot copy first, then the durable store.
///
/// The hot copy is written every few seconds and the durable one every few minutes, so the hot
/// copy is normally the newest state that exists; it is a first-class candidate because it uses
/// the identical envelope and the identical atomic sequence, and not a partial dump.
/// The compatibility string the newest decodable checkpoint in `dir` carries, if any.
///
/// The store's own `manifest.json` is only an index (generation, latest, previous, archives) —
/// the compatibility string lives in each checkpoint's own `FLYSIM01` envelope manifest, so
/// answering "what will this build refuse?" means decoding a checkpoint. Candidates are tried
/// newest first and a corrupt one is skipped, which is the same order and the same tolerance
/// `Sim::boot` restores with.
///
/// `flysim --print-state-compatibility DIR` is this, and `infra/05-deploy.sh` compares its
/// answer with `--print-compatibility` before flipping the `current` symlink.
pub fn state_compatibility(dir: &Path) -> Option<String> {
    let store = Store::new(dir, 8);
    for candidate in store.candidates("durable") {
        if let Ok(checkpoint) = load(&candidate.path) {
            return Some(checkpoint.runtime.compatibility);
        }
    }
    None
}

pub fn restore_order(hot: &Store, durable: &Store) -> Vec<Candidate> {
    let mut out = hot.candidates("hot");
    out.extend(durable.candidates("durable"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(generation: u64) -> RuntimeState {
        RuntimeState {
            generation,
            wall_ms: 1_700_000_000_000,
            rom_sha256: "ab".repeat(32),
            emulator_frame: 12_345,
            compatibility: "kernel/adapter/fingerprint".to_string(),
            speed: 1.0,
            buttons: 0b0001_0000,
            rank_since_ms: 4_242.0,
            last_event_id: 77,
            reward: serde_json::json!({ "version": 3, "total": 1.25 }),
            ratchet: flybrain_gb::RatchetState { best: 3, ..Default::default() },
            emulator: vec![7; 64],
            framebuffer: vec![9; 32],
            ratchet_game: vec![1, 2, 3],
            ratchet_frame: vec![4, 5, 6],
        }
    }

    fn agent_state() -> flybrain_core::agent::AgentState {
        use flybrain_core::decoder::DecoderState;
        use flybrain_core::lif::LifState;
        use flybrain_core::ordered::NumberMap;
        use flybrain_core::plasticity::PlasticityState;
        flybrain_core::agent::AgentState {
            version: 1,
            remainder: 0.75,
            warmed_up: true,
            network: LifState {
                membrane: vec![0.5, -0.25],
                refractory: vec![0, 3],
                last_spike_ms: vec![-1_000_000.0, 12.0],
                visual_drive: vec![0.1],
                rng: -12_345,
                reward_remaining: 40.0,
                ms: 9_000.0,
                population_rate: 1.5,
                rates: NumberMap::from_pairs([("forward", 2.0), ("reward_pam", 0.5)]),
                plasticity: PlasticityState {
                    version: "fly-kc-mbon-rstdp-v2".to_string(),
                    topology: 42,
                    enabled: true,
                    updates: 3.0,
                    signal: 0.25,
                    gains: vec![1.0, 0.9],
                    traces: vec![0.0, 0.1],
                    touched: vec![0.0, 8_000.0],
                },
            },
            decoder: DecoderState {
                version: 4,
                calibrated: true,
                baseline: NumberMap::from_pairs([("forward", 1.0)]),
                held_until: NumberMap::new(),
                next_allowed: NumberMap::new(),
                next_decision: 100.0,
                current: Some("a".to_string()),
                fatigue: NumberMap::new(),
                macro_next_decision: 0.0,
                macro_current: None,
                macro_fatigue: NumberMap::new(),
            },
        }
    }

    #[test]
    fn the_envelope_round_trips_every_field_and_chunk() {
        let agent = agent_state();
        let runtime = runtime(4);
        let bytes = encode(&agent, &runtime).unwrap();
        assert!(bytes.starts_with(MAGIC.as_bytes()));

        let back = decode(&bytes).unwrap();
        assert_eq!(back.agent, agent);
        assert_eq!(back.runtime, runtime);

        // Re-encoding the decoded state is byte-identical, so a restore-and-save cycle is stable.
        assert_eq!(encode(&back.agent, &back.runtime).unwrap(), bytes);
    }

    #[test]
    fn a_corrupted_or_truncated_envelope_is_rejected() {
        let bytes = encode(&agent_state(), &runtime(1)).unwrap();
        assert!(decode(&bytes[..bytes.len() - 1]).is_err(), "truncated");

        let mut flipped = bytes.clone();
        let middle = flipped.len() / 2;
        flipped[middle] ^= 0xff;
        assert!(decode(&flipped).is_err(), "checksum");

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_err(), "trailing bytes");

        let mut wrong_magic = bytes;
        wrong_magic[0] = b'X';
        assert!(decode(&wrong_magic).is_err(), "magic");
    }

    #[test]
    fn a_commit_is_atomic_and_a_crash_before_the_rename_leaves_a_loadable_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path(), 2);
        let first = encode(&agent_state(), &runtime(1)).unwrap();
        store.commit(1, &first, None).unwrap();

        // kill -9 between the tmp write and the rename.
        let mut broken = encode(&agent_state(), &runtime(2)).unwrap();
        broken.truncate(broken.len() / 2);
        let staged = store.stage_for_test(2, &broken).unwrap();
        assert!(staged.exists());

        let manifest = store.manifest().unwrap();
        assert_eq!(manifest.latest, Some(1));
        let candidates = store.candidates("durable");
        assert_eq!(candidates.len(), 1, "the staged tmp file is not a candidate: {candidates:?}");
        assert_eq!(load(&candidates[0].path).unwrap().runtime.generation, 1);

        // The next successful commit cleans the orphan up.
        let second = encode(&agent_state(), &runtime(3)).unwrap();
        store.commit(3, &second, None).unwrap();
        assert!(!staged.exists());
        assert_eq!(store.manifest().unwrap().latest, Some(3));
        assert_eq!(store.manifest().unwrap().previous, Some(1));
    }

    #[test]
    fn a_payload_renamed_without_its_manifest_is_never_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path(), 2);
        store.commit(1, &encode(&agent_state(), &runtime(1)).unwrap(), None).unwrap();

        // The payload rename succeeded, the manifest rename did not: the commit point is the
        // manifest, so generation 2 is invisible.
        write_atomic(&store.generation_path(2), b"not even an envelope").unwrap();
        let candidates = store.candidates("durable");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].generation, Some(1));
        assert_eq!(store.highest_generation(), 2, "but the number is not reused");
    }

    #[test]
    fn the_restore_order_is_hot_then_durable_then_archives_by_descending_rank() {
        let dir = tempfile::tempdir().unwrap();
        let durable = Store::new(dir.path().join("durable"), 2);
        let hot = Store::new(dir.path().join("hot"), 2);
        let bytes = encode(&agent_state(), &runtime(1)).unwrap();

        durable.commit(1, &bytes, Some(1)).unwrap();
        durable.commit(2, &bytes, Some(4)).unwrap();
        durable.commit(3, &bytes, None).unwrap();
        hot.commit(7, &bytes, None).unwrap();

        let order: Vec<String> =
            restore_order(&hot, &durable).into_iter().map(|c| c.origin).collect();
        assert_eq!(
            order,
            vec![
                "hot latest generation 7",
                "durable latest generation 3",
                "durable previous generation 2",
                "durable archived generation 2 (rank 4)",
                "durable milestone archive rank 4",
                "durable archived generation 1 (rank 1)",
                "durable milestone archive rank 1",
            ]
        );
    }

    #[test]
    fn rotation_keeps_the_referenced_generations_and_never_unlinks_an_archive() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path(), 2);
        let bytes = encode(&agent_state(), &runtime(1)).unwrap();
        store.commit(1, &bytes, Some(2)).unwrap();
        for generation in 2..=6 {
            store.commit(generation, &bytes, None).unwrap();
        }
        assert!(store.generation_path(6).exists(), "latest");
        assert!(store.generation_path(5).exists(), "previous");
        assert!(store.generation_path(1).exists(), "archived generation");
        assert!(store.archive_path(2).exists(), "milestone archive");
        assert!(!store.generation_path(3).exists(), "rotated away");
        assert!(!store.generation_path(4).exists(), "rotated away");
    }

    #[test]
    fn an_empty_or_manifestless_directory_offers_no_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("missing"), 2);
        assert!(store.candidates("durable").is_empty());
        assert_eq!(store.highest_generation(), 0);

        let store = Store::new(dir.path(), 2);
        store.create().unwrap();
        std::fs::write(store.manifest_path(), "{ not json").unwrap();
        assert!(store.candidates("durable").is_empty(), "an unreadable manifest is not fatal");
    }
}
