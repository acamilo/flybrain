//! STATE-02: the legacy Game Boy composition's checkpoints, in `FLYSIM01`, the format of record.
//!
//! `legacy-gameboy-v1` section 16 and the operator's decision of 2026-09-23: `FLYSIM01` stays
//! the format of record until RETIRE-01, and the session runtime exports a `FLYSIM01` envelope on
//! every durable save, which the current `flysim` reads under its unchanged compatibility string.
//! The deploy gate, `fly-reset-to-milestone` and `fly-loop-reset` keep working on those files. This
//! module is that export, the import back, the store policy around them and the restore selection:
//!
//! | `FLYSIM01` | Owner in the session runtime | Here |
//! | --- | --- | --- |
//! | the seven agent chunks and `remainder` | the agent (`FLYAGT01` capture payload) | [`agent_state`], [`agent_payload`] |
//! | `emulator`, `framebuffer`, `emulatorFrame`, `buttons`, `romHash`, `ratchetGame`, `ratchetFrame` | the environment (`FLYENV01`; the ratchet's snapshot is slot `best`) | [`world_state`], [`world_payload`] |
//! | `reward`, `ratchet` | the task's ledger | [`TaskHalf`] |
//! | `generation`, `wallMs`, `compatibility`, `speed`, `rankSinceMs`, `lastEventId` | the host (store, feed, rank tracking) | [`HostHalf`], [`LegacyCheckpointer`] |
//!
//! **Byte compatibility is by construction.** The envelope is written by
//! `flysim_store::store::encode`, the one function the legacy loop writes with, from the same
//! values: [`export`] builds a `RuntimeState` field for field as `Sim::snapshot_state` does. The
//! store is `flysim_store::store::Store`, so generations, `manifest.json`, `milestone-<N>`
//! archives, `keep_generations` rotation and the restore order are the legacy store's own code.
//! The tests prove the rest on real checkpoints: legacy -> session -> legacy and session ->
//! legacy -> session round trips, and restore-then-continue against the legacy loop's
//! `FLY_TRACE`.
//!
//! **Restore is `legacy-transient-reset`** (`legacy-gameboy-v1` section 14): what `FLYSIM01`
//! carries is restored; the executor's ledgers, the running macro, the readout transient and the
//! adapter's transient observations are not in it and start cleared.
//!
//! **Declared difference, archive order** (`legacy-gameboy-v1` sections 4 and 16, operator
//! 2026-09-23): a milestone archive written by the session runtime is captured *after* that
//! boundary's slot save, so it holds the post-capture ratchet and slot (`best = r`, rung `r`'s
//! snapshot), where the legacy loop's holds `best = r-1` and rung `r-1`'s. Both are ordinary
//! `FLYSIM01` files and each runtime restores the other's.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fly_session_types::gameboy;
use flybrain_core::agent::AgentState;
use flybrain_gb::RatchetState;
use flysim_store::store::{self, Candidate, Checkpoint, RuntimeState, Store};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::clock::TickAccumulator;
use crate::legacy_agent::{self, PayloadIdentity, legacy_remainder_to_rational};
use crate::legacy_env::{Flysim01World, PayloadHeader, WorldState};
use crate::types::*;

pub use flysim_store::journal;
pub use flysim_store::store as flysim01;

/// The source epoch of a payload made from a `FLYSIM01` file: the legacy file has no session
/// identity, so the imported payloads say where they came from rather than invent one.
pub const LEGACY_SOURCE_EPOCH: &str = "legacy";

// -------------------------------------------------------------------------------------------
// The halves

/// The task's half of a `FLYSIM01` checkpoint: the manifest members `reward` and `ratchet`.
///
/// This is the `pokered-macros-v1` task ledger (`legacy-gameboy-v1` section 10: "the adapter
/// state (FLYSIM01's `reward` chunk) and the ratchet state"). The executor's ledgers are not in
/// it (`legacy-transient-reset`).
#[derive(Clone, Debug, PartialEq)]
pub struct TaskHalf {
    /// `GameAdapter::export_state`, exactly as `FLYSIM01` records it; `null` for an adapter that
    /// has none, which a restore then leaves alone.
    pub reward: Value,
    pub ratchet: RatchetState,
}

impl TaskHalf {
    /// `{reward, ratchet, slotFilled}`: the ledger value the task captures and installs.
    /// `slotFilled` says whether the ratchet has a snapshot, which lives in the environment's
    /// slot `best`; it is derived, never stored in `FLYSIM01`.
    pub fn to_ledger(&self, slot_filled: bool) -> Value {
        serde_json::json!({
            "reward": self.reward,
            "ratchet": serde_json::to_value(self.ratchet).expect("a ratchet state serializes"),
            "slotFilled": slot_filled,
        })
    }

    pub fn from_ledger(value: &Value) -> Result<TaskHalf, String> {
        let ratchet = value.get("ratchet").ok_or("the task ledger has no ratchet")?;
        Ok(TaskHalf {
            reward: value.get("reward").cloned().unwrap_or(Value::Null),
            ratchet: serde_json::from_value(ratchet.clone())
                .map_err(|e| format!("the task ledger's ratchet: {e}"))?,
        })
    }
}

/// The host's half: what neither the agent, the world nor the task owns.
#[derive(Clone, Debug, PartialEq)]
pub struct HostHalf {
    pub generation: u64,
    pub wall_ms: u64,
    /// The legacy compatibility string (`flysim --print-compatibility`), recorded, not
    /// reinterpreted; it is still the restore gate (`legacy-gameboy-v1` section 12).
    pub compatibility: String,
    pub speed: f64,
    /// Brain milliseconds at which the current ladder rank was reached.
    pub rank_since_ms: f64,
    /// The feed event log's watermark: the last event id issued.
    pub last_event_id: u64,
}

/// A `FLYSIM01` checkpoint read into its owners' halves.
#[derive(Clone, Debug)]
pub struct Halves {
    pub agent: AgentState,
    pub world: WorldState,
    pub task: TaskHalf,
    pub host: HostHalf,
}

// -------------------------------------------------------------------------------------------
// Export

/// The agent state a `FLYAGT01` capture payload carries (its remainder is the accumulator's).
pub fn agent_state(agent_payload: &[u8]) -> Result<AgentState, String> {
    legacy_agent::decode_payload_state(agent_payload)
}

/// The world a `FLYENV01` capture payload carries.
pub fn world_state(world_payload: &[u8]) -> Result<WorldState, String> {
    WorldState::decode(world_payload).map(|(_, state)| state)
}

/// The `FLYSIM01` runtime fields of one boundary, exactly as `Sim::snapshot_state` fills them.
pub fn runtime_state(
    world: &WorldState,
    slot_id: &Id,
    task: &TaskHalf,
    host: &HostHalf,
) -> RuntimeState {
    let Flysim01World {
        rom_hash,
        emulator_frame,
        buttons,
        emulator,
        framebuffer,
        ratchet_game,
        ratchet_frame,
    } = world.flysim01_chunks(slot_id);
    RuntimeState {
        generation: host.generation,
        wall_ms: host.wall_ms,
        rom_sha256: rom_hash,
        emulator_frame,
        compatibility: host.compatibility.clone(),
        speed: host.speed,
        buttons,
        rank_since_ms: host.rank_since_ms,
        last_event_id: host.last_event_id,
        reward: task.reward.clone(),
        ratchet: task.ratchet,
        emulator,
        framebuffer,
        ratchet_game,
        ratchet_frame,
    }
}

/// The `FLYSIM01` envelope of one boundary, from the agent's state, the world, the task's half
/// and the host's.
pub fn export_states(
    agent: &AgentState,
    world: &WorldState,
    slot_id: &Id,
    task: &TaskHalf,
    host: &HostHalf,
) -> Result<Vec<u8>, String> {
    store::encode(agent, &runtime_state(world, slot_id, task, host)).map_err(|e| format!("{e:#}"))
}

/// The `FLYSIM01` envelope of one boundary, from the two participants' capture payloads.
pub fn export(
    agent_payload: &[u8],
    world_payload: &[u8],
    slot_id: &Id,
    task: &TaskHalf,
    host: &HostHalf,
) -> Result<Vec<u8>, String> {
    let agent = agent_state(agent_payload)?;
    let world = world_state(world_payload)?;
    export_states(&agent, &world, slot_id, task, host)
}

// -------------------------------------------------------------------------------------------
// Import

/// Reads a decoded `FLYSIM01` checkpoint into its owners' halves. The world is at boundary
/// `emulatorFrame - setupFrames` with the ratchet's snapshot as slot `slot_id`.
pub fn halves(
    checkpoint: &Checkpoint,
    episode_id: &Id,
    slot_id: &Id,
    audio_sample_rate: u64,
) -> Result<Halves, String> {
    let runtime = &checkpoint.runtime;
    let world = Flysim01World {
        rom_hash: runtime.rom_sha256.clone(),
        emulator_frame: runtime.emulator_frame,
        buttons: runtime.buttons,
        emulator: runtime.emulator.clone(),
        framebuffer: runtime.framebuffer.clone(),
        ratchet_game: runtime.ratchet_game.clone(),
        ratchet_frame: runtime.ratchet_frame.clone(),
    }
    .into_state(episode_id, slot_id, audio_sample_rate)?;
    Ok(Halves {
        agent: checkpoint.agent.clone(),
        world,
        task: TaskHalf {
            reward: runtime.reward.clone(),
            ratchet: runtime.ratchet,
        },
        host: HostHalf {
            generation: runtime.generation,
            wall_ms: runtime.wall_ms,
            compatibility: runtime.compatibility.clone(),
            speed: runtime.speed,
            rank_since_ms: runtime.rank_since_ms,
            last_event_id: runtime.last_event_id,
        },
    })
}

/// Decodes `FLYSIM01` bytes into halves.
pub fn split(
    bytes: &[u8],
    episode_id: &Id,
    slot_id: &Id,
    audio_sample_rate: u64,
) -> Result<Halves, String> {
    let checkpoint = store::decode(bytes).map_err(|e| format!("{e:#}"))?;
    halves(&checkpoint, episode_id, slot_id, audio_sample_rate)
}

/// The reinforcement calls to start a legacy agent state's `learning.updates` at.
///
/// `FLYSIM01` records `plasticity.updates`, the reinforcements that moved a gain, but not every
/// call, and the contract requires `learning.changed <= learning.updates`. An import that starts
/// the call count at zero makes the first telemetry unreadable (found by AGENT-01 on the row-58
/// stream checkpoint: `changed` is in the thousands), so it starts at the recorded count, a
/// lower bound on the calls actually made (`legacy-gameboy-v1` section 13, amendment of
/// 2026-09-29).
pub fn legacy_reinforcements(state: &AgentState) -> u64 {
    state.network.plasticity.updates as u64
}

/// The session accumulator of a legacy agent state: its exact remainder, every tick it has run
/// (`network.ms`, warm-up included) and the profile's warm-up as the offset.
pub fn legacy_accumulator(state: &AgentState) -> Result<TickAccumulator, String> {
    let ms = state.network.ms;
    if !ms.is_finite() || ms < 0.0 || ms.fract() != 0.0 {
        return Err(format!("the agent's brain clock {ms} ms is not a whole tick count"));
    }
    let ticks = ms as u64;
    let warmup = if state.warmed_up { gameboy::WARMUP_MS.min(ticks) } else { 0 };
    TickAccumulator::restored(
        gameboy::tick_duration(),
        legacy_remainder_to_rational(state.remainder)?,
        ticks,
        warmup,
    )
}

/// What the agent's imported payload is stamped with besides its state.
#[derive(Clone, Debug)]
pub struct AgentImport {
    pub agent_id: Id,
    pub profile: AssetRef,
    pub seed: i32,
    pub macro_channels: Vec<String>,
}

/// A `FLYAGT01` payload for `State.StageRestore` from a legacy agent state, the shipped form of
/// AGENT-01's `seed_from_legacy_state` prototype. `context` is the decision context the task
/// gives the restored agent for its next Prepare (`{boot, bound, location}` after the executor
/// observed the restored world). `frame_on_screen` is the file's `framebuffer`: the restore
/// installs it as the next input, as `LegacyFrame::restore` does, so a file whose saved visual
/// drive is not its framebuffer's projection (a survey checkpoint, say) restores to the legacy
/// outcome and not to its own bytes (`legacy-gameboy-v1` section 16 amendment of 2026-09-29).
pub fn agent_payload(
    state: &AgentState,
    frame_on_screen: &[u8],
    import: &AgentImport,
    checkpoint_id: &Id,
    source_scope: &Scope,
    context: &TypedValue,
) -> Result<Vec<u8>, String> {
    let accumulator = legacy_accumulator(state)?;
    legacy_agent::encode_payload(
        state,
        &PayloadIdentity {
            agent_id: &import.agent_id,
            checkpoint_id,
            source_scope,
            committed_step: source_scope.step,
            profile: &import.profile,
            seed: import.seed,
            reinforcements: legacy_reinforcements(state),
            macro_channels: &import.macro_channels,
        },
        &accumulator,
        context,
        Some(frame_on_screen),
    )
}

/// A `FLYENV01` payload for `State.StageRestore` from a world read out of `FLYSIM01`.
pub fn world_payload(
    world: &WorldState,
    worker_id: &Id,
    checkpoint_id: &Id,
    source_scope: &Scope,
    configuration_digest: &Digest,
) -> Vec<u8> {
    world.encode(&PayloadHeader {
        worker_id: worker_id.clone(),
        checkpoint_id: checkpoint_id.clone(),
        source_scope: source_scope.clone(),
        configuration_digest: configuration_digest.clone(),
    })
}

/// The scope a payload made from a `FLYSIM01` file records as its source: the session, the
/// [`LEGACY_SOURCE_EPOCH`] and the world's boundary.
pub fn legacy_source_scope(session_id: &Id, boundary: u64) -> Scope {
    scope_at(session_id, LEGACY_SOURCE_EPOCH, boundary)
}

// -------------------------------------------------------------------------------------------
// Restore selection

/// What a candidate must be to be restored: the legacy loop's `try_restore` gate.
#[derive(Clone, Debug)]
pub struct RestoreGate {
    pub rom_sha256: String,
    /// This build's compatibility string.
    pub compatibility: String,
    /// The adapter ids this build's adapter migrates from (`GameAdapter::migrates_from`).
    pub migrates_from: Vec<String>,
    /// `FLY_ACCEPT_ADAPTERS`, parsed (`flybrain_gb::compatibility::accepted_adapters`).
    pub accepted: Vec<String>,
}

impl RestoreGate {
    /// The gate with `FLY_ACCEPT_ADAPTERS` read from the process environment, as `try_restore`
    /// reads it: a property of a deploy, not of a run.
    pub fn from_env(rom_sha256: &str, compatibility: &str, migrates_from: &[&str]) -> RestoreGate {
        RestoreGate {
            rom_sha256: rom_sha256.to_owned(),
            compatibility: compatibility.to_owned(),
            migrates_from: migrates_from.iter().map(|s| (*s).to_owned()).collect(),
            accepted: flybrain_gb::compatibility::accepted_adapters(
                std::env::var(flybrain_gb::compatibility::ACCEPT_ADAPTERS_ENV).ok().as_deref(),
            ),
        }
    }

    /// `Ok` for a restorable checkpoint (with the adapter it migrates from, if any), or why not.
    pub fn check(&self, checkpoint: &Checkpoint) -> Result<Option<String>, String> {
        let runtime = &checkpoint.runtime;
        if runtime.rom_sha256 != self.rom_sha256 {
            return Err(format!("checkpoint is for another cartridge ({})", runtime.rom_sha256));
        }
        let migrates: Vec<&str> = self.migrates_from.iter().map(String::as_str).collect();
        match flybrain_gb::compatibility::decide(
            &runtime.compatibility,
            &self.compatibility,
            &migrates,
            &self.accepted,
        ) {
            flybrain_gb::compatibility::RestoreDecision::Exact => Ok(None),
            flybrain_gb::compatibility::RestoreDecision::MigrateAdapter { from } => Ok(Some(from)),
            flybrain_gb::compatibility::RestoreDecision::Refuse(reason) => Err(format!(
                "compatibility mismatch: {reason}\n  checkpoint: {}\n  this build: {}",
                runtime.compatibility, self.compatibility
            )),
        }
    }
}

/// One restored candidate.
#[derive(Clone, Debug)]
pub struct Restored<T> {
    pub candidate: Candidate,
    /// Candidates refused before this one, with why: the legacy `restore_fallback` metric.
    pub skipped: Vec<(Candidate, String)>,
    pub migrated_from: Option<String>,
    pub checkpoint: Checkpoint,
    pub installed: T,
}

/// The outcome of a restore over the live store.
#[derive(Debug)]
pub enum Selection<T> {
    /// No candidate exists: warm up a fresh fly.
    Fresh,
    Restored(Box<Restored<T>>),
    /// Candidates exist and every one failed. The legacy loop exits non-zero here rather than
    /// silently start a fresh fly over them, and so must the session runtime.
    Refused(Vec<(Candidate, String)>),
}

/// Restores from the live `FLYSIM01` stores in the legacy order -- hot latest, hot previous,
/// durable latest, durable previous, then the milestone archives by descending rank
/// (`store::restore_order`) -- trying the next candidate on *any* failure: unreadable,
/// another cartridge, refused compatibility, or `install` refusing it (a participant's
/// `State.StageRestore` or the task's validation). This is `Sim::restore_or_warm_up`.
pub async fn restore_from_store<T, F, Fut>(
    hot: &Store,
    durable: &Store,
    gate: &RestoreGate,
    mut install: F,
) -> Selection<T>
where
    F: FnMut(Checkpoint) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let candidates = store::restore_order(hot, durable);
    if candidates.is_empty() {
        return Selection::Fresh;
    }
    let mut skipped: Vec<(Candidate, String)> = Vec::new();
    for candidate in candidates {
        let checkpoint = match store::load(&candidate.path) {
            Ok(checkpoint) => checkpoint,
            Err(e) => {
                skipped.push((candidate, format!("{e:#}")));
                continue;
            }
        };
        let migrated_from = match gate.check(&checkpoint) {
            Ok(from) => from,
            Err(reason) => {
                skipped.push((candidate, reason));
                continue;
            }
        };
        match install(checkpoint.clone()).await {
            Ok(installed) => {
                return Selection::Restored(Box::new(Restored {
                    candidate,
                    skipped,
                    migrated_from,
                    checkpoint,
                    installed,
                }));
            }
            Err(reason) => skipped.push((candidate, reason)),
        }
    }
    Selection::Refused(skipped)
}

// -------------------------------------------------------------------------------------------
// The store policy

/// Why a save is taken. Each maps onto the legacy loop's call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveKind {
    /// `checkpoint_if_due`, the hot interval: `hot_dir`, every `hot_seconds`.
    Hot,
    /// A durable save: the interval (`checkpoint_seconds`), startup/restore, after a rollback,
    /// a forced checkpoint, a pause and shutdown.
    Durable,
    /// `track_rank`: a climb to a new rank. Durable, and archived as `milestone-<rank>` when the
    /// rank is above every rank this process archived.
    Milestone(u32),
}

/// A rank change the task reported, as `Sim::track_rank` sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RankChange {
    Climbed(u32),
    FellBack(u32),
}

/// The configuration the legacy loop reads from `[paths]` and `[loop]`.
#[derive(Clone, Debug)]
pub struct CheckpointerConfig {
    pub hot_dir: std::path::PathBuf,
    pub durable_dir: std::path::PathBuf,
    pub keep_generations: usize,
    pub hot_seconds: f64,
    pub checkpoint_seconds: f64,
    pub compatibility: String,
    pub speed: f64,
    /// The environment's slot that holds the ratchet's snapshot (`best`).
    pub slot_id: Id,
}

/// One participant-coherent capture of a boundary, ready to be written.
#[derive(Clone, Debug)]
pub struct LegacyCapture {
    pub boundary: u64,
    pub agent_payload: Vec<u8>,
    pub world_payload: Vec<u8>,
    pub task: TaskHalf,
}

/// What a committed save wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveReport {
    pub generation: u64,
    pub durable: bool,
    pub archive_rank: Option<u32>,
    pub bytes: usize,
}

/// A save that was queued; its reply resolves when the writer committed it (or failed).
pub struct SaveTicket {
    pub generation: u64,
    pub durable: bool,
    pub archive_rank: Option<u32>,
    pub reply: oneshot::Receiver<Result<SaveReport, String>>,
}

struct WriteJob {
    durable: bool,
    archive_rank: Option<u32>,
    capture: LegacyCapture,
    host: HostHalf,
    slot_id: Id,
    reply: oneshot::Sender<Result<SaveReport, String>>,
}

/// The session runtime's `FLYSIM01` store policy: the legacy loop's, call site for call site.
///
/// - generations come from one counter over both stores, starting above the highest either
///   has ever allocated (`Sim::boot`);
/// - a hot copy every `hot_seconds`, a durable one every `checkpoint_seconds`, the durable
///   interval resetting the hot one (`checkpoint_if_due`);
/// - the first durable commit at a new best rank of *this process* is also written as
///   `milestone-<rank>.checkpoint` (`checkpoint_with_reply`'s `best_archived_rank`);
/// - the encoding and the fsyncs run on a writer thread, off the session's runtime.
pub struct LegacyCheckpointer {
    config: CheckpointerConfig,
    hot: Store,
    durable: Store,
    next_generation: u64,
    best_archived_rank: Option<u32>,
    rank: u32,
    rank_since_ms: f64,
    next_hot: Instant,
    next_durable: Instant,
    writer: Option<mpsc::Sender<WriteJob>>,
    thread: Option<JoinHandle<()>>,
}

impl LegacyCheckpointer {
    /// Creates both store directories and starts the writer.
    pub fn open(config: CheckpointerConfig) -> Result<LegacyCheckpointer, String> {
        let durable = Store::new(&config.durable_dir, config.keep_generations);
        let hot = Store::new(&config.hot_dir, config.keep_generations);
        durable.create().map_err(|e| format!("{e:#}"))?;
        hot.create().map_err(|e| format!("{e:#}"))?;
        let next_generation = durable.highest_generation().max(hot.highest_generation()) + 1;
        let (tx, rx) = mpsc::channel::<WriteJob>();
        let (writer_hot, writer_durable) = (hot.clone(), durable.clone());
        let thread = std::thread::Builder::new()
            .name("legacy-checkpoint".to_owned())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let store = if job.durable { &writer_durable } else { &writer_hot };
                    let generation = job.host.generation;
                    let result = export(
                        &job.capture.agent_payload,
                        &job.capture.world_payload,
                        &job.slot_id,
                        &job.capture.task,
                        &job.host,
                    )
                    .and_then(|bytes| {
                        store
                            .commit(generation, &bytes, job.archive_rank)
                            .map(|_| bytes.len())
                            .map_err(|e| format!("{e:#}"))
                    })
                    .map(|bytes| SaveReport {
                        generation,
                        durable: job.durable,
                        archive_rank: job.archive_rank,
                        bytes,
                    });
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|e| format!("spawning the checkpoint writer: {e}"))?;
        let now = Instant::now();
        Ok(LegacyCheckpointer {
            config,
            hot,
            durable,
            next_generation,
            best_archived_rank: None,
            rank: 0,
            rank_since_ms: 0.0,
            next_hot: now,
            next_durable: now,
            writer: Some(tx),
            thread: Some(thread),
        })
    }

    pub fn hot(&self) -> &Store {
        &self.hot
    }

    pub fn durable(&self) -> &Store {
        &self.durable
    }

    pub fn config(&self) -> &CheckpointerConfig {
        &self.config
    }

    /// The generation the next save will take.
    pub fn next_generation(&self) -> u64 {
        self.next_generation
    }

    pub fn rank(&self) -> u32 {
        self.rank
    }

    pub fn rank_since_ms(&self) -> f64 {
        self.rank_since_ms
    }

    /// After a restore: the ladder rank the restored task reports and the checkpoint's
    /// `rankSinceMs` (`Sim::try_restore`). A fresh start keeps rank 0.
    pub fn restored(&mut self, rank: u32, rank_since_ms: f64) {
        self.rank = rank;
        self.rank_since_ms = rank_since_ms;
    }

    /// Both intervals run from now: called right after the startup durable commit, as
    /// `Sim::boot` does, so the loop's first boundary does not write a second one.
    pub fn start_intervals(&mut self, now: Instant) {
        self.next_hot = now + Duration::from_secs_f64(self.config.hot_seconds);
        self.next_durable = now + Duration::from_secs_f64(self.config.checkpoint_seconds);
    }

    /// `Sim::track_rank`: the rank after a transition. A climb is a milestone save.
    pub fn observe_rank(&mut self, rank: u32, brain_ms: f64) -> Option<RankChange> {
        if rank == self.rank {
            return None;
        }
        let climbed = rank > self.rank;
        self.rank = rank;
        self.rank_since_ms = brain_ms;
        Some(if climbed { RankChange::Climbed(rank) } else { RankChange::FellBack(rank) })
    }

    /// `Sim::checkpoint_if_due`: the interval save due now, if any.
    pub fn due(&mut self, now: Instant) -> Option<SaveKind> {
        let hot_period = Duration::from_secs_f64(self.config.hot_seconds);
        let durable_period = Duration::from_secs_f64(self.config.checkpoint_seconds);
        if now >= self.next_durable {
            self.next_durable = now + durable_period;
            self.next_hot = now + hot_period;
            Some(SaveKind::Durable)
        } else if now >= self.next_hot {
            self.next_hot = now + hot_period;
            Some(SaveKind::Hot)
        } else {
            None
        }
    }

    /// Queues one save of `capture`. The generation is allocated here, on the caller's thread,
    /// in call order, and a milestone archive is decided here too, exactly as
    /// `Sim::checkpoint_with_reply` decides both before handing the write to its thread.
    pub fn save(
        &mut self,
        kind: SaveKind,
        capture: LegacyCapture,
        last_event_id: u64,
        wall_ms: u64,
    ) -> Result<SaveTicket, String> {
        let durable = kind != SaveKind::Hot;
        let archive_rank = match kind {
            SaveKind::Milestone(rank) => Some(rank),
            _ => None,
        }
        .filter(|rank| durable && self.best_archived_rank.is_none_or(|best| *rank > best));
        let generation = self.next_generation;
        self.next_generation += 1;
        if let Some(rank) = archive_rank {
            self.best_archived_rank = Some(rank);
        }
        let host = HostHalf {
            generation,
            wall_ms,
            compatibility: self.config.compatibility.clone(),
            speed: self.config.speed,
            rank_since_ms: self.rank_since_ms,
            last_event_id,
        };
        let (tx, rx) = oneshot::channel();
        let job = WriteJob {
            durable,
            archive_rank,
            capture,
            host,
            slot_id: self.config.slot_id.clone(),
            reply: tx,
        };
        self.writer
            .as_ref()
            .ok_or("the checkpoint writer is gone")?
            .send(job)
            .map_err(|_| "the checkpoint writer stopped".to_owned())?;
        Ok(SaveTicket {
            generation,
            durable,
            archive_rank,
            reply: rx,
        })
    }

    /// Restore candidates in the legacy order.
    pub fn candidates(&self) -> Vec<Candidate> {
        store::restore_order(&self.hot, &self.durable)
    }

    /// Stops the writer after every queued save committed.
    pub fn close(&mut self) {
        self.writer = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for LegacyCheckpointer {
    fn drop(&mut self) {
        self.close();
    }
}

/// The current wall clock in milliseconds, for `wallMs`.
pub fn now_wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy_env::{SlotState, sample_at, world_time_at};
    use flybrain_gb::emulator::FRAMEBUFFER_LEN;

    fn agent(ms: f64, remainder: f64, updates: f64) -> AgentState {
        use flybrain_core::decoder::DecoderState;
        use flybrain_core::lif::LifState;
        use flybrain_core::ordered::NumberMap;
        use flybrain_core::plasticity::PlasticityState;
        AgentState {
            version: 1,
            remainder,
            warmed_up: true,
            network: LifState {
                membrane: vec![0.5, -0.25],
                refractory: vec![0, 3],
                last_spike_ms: vec![-1_000_000.0, 12.0],
                visual_drive: vec![0.1],
                rng: -12_345,
                reward_remaining: 40.0,
                ms,
                population_rate: 1.5,
                rates: NumberMap::from_pairs([("forward", 2.0), ("reward_pam", 0.5)]),
                plasticity: PlasticityState {
                    version: "fly-kc-mbon-rstdp-v2".to_string(),
                    topology: 42,
                    enabled: true,
                    updates,
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

    fn world_at(boundary: u64, slot: bool) -> WorldState {
        let world_time = world_time_at(boundary).unwrap();
        WorldState {
            rom_digest: "ab".repeat(32),
            episode_id: id("ep1"),
            boundary,
            audio_next_sample: sample_at(&world_time, 48_000),
            world_time,
            engine_frame: boundary + 1,
            buttons: 0b0001_0000,
            emulator: vec![7; 64],
            framebuffer: vec![9; FRAMEBUFFER_LEN],
            slots: if slot {
                vec![(
                    id("best"),
                    SlotState { game: vec![1, 2, 3], frame: vec![4; FRAMEBUFFER_LEN], boundary },
                )]
            } else {
                Vec::new()
            },
        }
    }

    fn task() -> TaskHalf {
        TaskHalf {
            reward: serde_json::json!({ "version": 3, "total": 1.25 }),
            ratchet: RatchetState { best: 3, attempts: 1, ..Default::default() },
        }
    }

    fn host(generation: u64) -> HostHalf {
        HostHalf {
            generation,
            wall_ms: 1_700_000_000_000,
            compatibility: "kernel/adapter/fingerprint".to_owned(),
            speed: 1.0,
            rank_since_ms: 4_242.0,
            last_event_id: 77,
        }
    }

    #[test]
    fn the_export_is_the_legacy_encoder_on_the_same_values_and_splits_back_into_its_halves() {
        let agent = agent(9_000.0, 0.75, 3.0);
        let world = world_at(12_344, true);
        let bytes = export_states(&agent, &world, &id("best"), &task(), &host(4)).unwrap();
        // Field for field what `Sim::snapshot_state` builds, through the one encoder.
        let legacy = store::encode(
            &agent,
            &RuntimeState {
                generation: 4,
                wall_ms: 1_700_000_000_000,
                rom_sha256: "ab".repeat(32),
                emulator_frame: 12_345,
                compatibility: "kernel/adapter/fingerprint".to_owned(),
                speed: 1.0,
                buttons: 0b0001_0000,
                rank_since_ms: 4_242.0,
                last_event_id: 77,
                reward: task().reward,
                ratchet: task().ratchet,
                emulator: vec![7; 64],
                framebuffer: vec![9; FRAMEBUFFER_LEN],
                ratchet_game: vec![1, 2, 3],
                ratchet_frame: vec![4; FRAMEBUFFER_LEN],
            },
        )
        .unwrap();
        assert_eq!(bytes, legacy);
        let back = split(&bytes, &id("ep1"), &id("best"), 48_000).unwrap();
        assert_eq!(back.agent, agent);
        assert_eq!(back.world, world);
        assert_eq!(back.task, task());
        assert_eq!(back.host, host(4));
        // And no slot is an empty ratchet snapshot, both ways.
        let bytes = export_states(&agent, &world_at(5, false), &id("best"), &task(), &host(1)).unwrap();
        let back = split(&bytes, &id("ep1"), &id("best"), 48_000).unwrap();
        assert!(back.world.slots.is_empty());
    }

    #[test]
    fn the_export_from_capture_payloads_equals_the_export_from_states() {
        let agent = agent(9_000.0, 0.75, 3.0);
        let world = world_at(700, true);
        let import = AgentImport {
            agent_id: id("fly"),
            profile: gameboy::profile_asset_ref(),
            seed: 22_222,
            macro_channels: vec!["macro_talk".to_owned()],
        };
        let scope = legacy_source_scope(&id("s1"), world.boundary);
        let context = gameboy::ReadoutContext { boot: false, bound: vec![], location: None }.to_typed();
        let agent_bytes = agent_payload(&agent, &world.framebuffer, &import, &id("c1"), &scope, &context).unwrap();
        let world_bytes = world_payload(&world, &id("world"), &id("c1"), &scope, &"cd".repeat(32));
        assert_eq!(agent_state(&agent_bytes).unwrap(), agent);
        assert_eq!(world_state(&world_bytes).unwrap(), world);
        assert_eq!(
            export(&agent_bytes, &world_bytes, &id("best"), &task(), &host(9)).unwrap(),
            export_states(&agent, &world, &id("best"), &task(), &host(9)).unwrap()
        );
        assert!(export(&world_bytes, &agent_bytes, &id("best"), &task(), &host(9)).is_err());
    }

    #[test]
    fn the_import_starts_the_accumulator_and_the_reinforcement_count_from_the_legacy_state() {
        let state = agent(108_246_189.0, 0.75, 4_321.0);
        let accumulator = legacy_accumulator(&state).unwrap();
        assert_eq!(accumulator.brain_ticks(), 108_246_189);
        assert_eq!(accumulator.warmup_offset(), gameboy::WARMUP_MS);
        assert_eq!(
            crate::legacy_agent::rational_remainder_to_legacy(&accumulator.remainder()).unwrap(),
            0.75
        );
        assert_eq!(legacy_reinforcements(&state), 4_321);
        // A clock that is not whole ticks, or a remainder that is not k/32768 ms, is refused.
        assert!(legacy_accumulator(&agent(10.5, 0.0, 0.0)).is_err());
        assert!(legacy_accumulator(&agent(10.0, 0.1, 0.0)).is_err());
        let session = {
            let import = AgentImport {
                agent_id: id("fly"),
                profile: gameboy::profile_asset_ref(),
                seed: 22_222,
                macro_channels: Vec::new(),
            };
            let context =
                gameboy::ReadoutContext { boot: true, bound: vec![], location: None }.to_typed();
            let bytes = agent_payload(
                &state,
                &[0; FRAMEBUFFER_LEN],
                &import,
                &id("c1"),
                &legacy_source_scope(&id("s1"), 40),
                &context,
            )
            .unwrap();
            let parts = flybrain_core::envelope::decode_envelope(
                &bytes,
                crate::legacy_agent::PAYLOAD_MAGIC,
            )
            .unwrap();
            let session: Value =
                serde_json::from_str(&parts.manifest.get("session").unwrap().stringify()).unwrap();
            session
        };
        assert_eq!(session["reinforcements"], "4321");
        assert_eq!(session["committedStep"], "40");
        assert_eq!(session["sourceScope"]["epoch"], LEGACY_SOURCE_EPOCH);
        assert_eq!(session["accumulator"]["executedTicks"], "108246189");
        assert_eq!(session["accumulator"]["warmupOffset"], "2500");
    }

    #[test]
    fn the_task_ledger_carries_exactly_the_reward_and_the_ratchet() {
        let ledger = task().to_ledger(true);
        assert_eq!(ledger["slotFilled"], true);
        assert_eq!(TaskHalf::from_ledger(&ledger).unwrap(), task());
        assert_eq!(
            ledger["ratchet"],
            serde_json::to_value(task().ratchet).unwrap(),
            "the FLYSIM01 manifest's own spelling"
        );
        assert!(TaskHalf::from_ledger(&serde_json::json!({ "reward": null })).is_err());
    }

    fn capture(boundary: u64) -> LegacyCapture {
        let agent = agent(9_000.0 + boundary as f64, 0.75, 3.0);
        let world = world_at(boundary, boundary > 1);
        let scope = legacy_source_scope(&id("s1"), boundary);
        let import = AgentImport {
            agent_id: id("fly"),
            profile: gameboy::profile_asset_ref(),
            seed: 22_222,
            macro_channels: Vec::new(),
        };
        let context = gameboy::ReadoutContext { boot: false, bound: vec![], location: None }.to_typed();
        LegacyCapture {
            boundary,
            agent_payload: agent_payload(&agent, &world.framebuffer, &import, &id("c"), &scope, &context)
                .unwrap(),
            world_payload: world_payload(&world, &id("world"), &id("c"), &scope, &"cd".repeat(32)),
            task: task(),
        }
    }

    fn config(root: &std::path::Path) -> CheckpointerConfig {
        CheckpointerConfig {
            hot_dir: root.join("hot"),
            durable_dir: root.join("state"),
            keep_generations: 2,
            hot_seconds: 5.0,
            checkpoint_seconds: 300.0,
            compatibility: "kernel/adapter/fingerprint".to_owned(),
            speed: 1.0,
            slot_id: id("best"),
        }
    }

    fn commit(checkpointer: &mut LegacyCheckpointer, kind: SaveKind, boundary: u64) -> SaveReport {
        let ticket = checkpointer.save(kind, capture(boundary), boundary, 1).unwrap();
        ticket.reply.blocking_recv().unwrap().unwrap()
    }

    #[test]
    fn the_checkpointer_keeps_the_legacy_generations_archives_and_rotation() {
        let dir = tempfile::tempdir().unwrap();
        // A store the legacy loop already wrote to: generation numbers are never reused.
        let durable = Store::new(dir.path().join("state"), 2);
        durable.create().unwrap();
        durable.stage_for_test(41, b"a crashed commit").unwrap();
        std::fs::rename(
            dir.path().join("state/41.checkpoint.tmp"),
            dir.path().join("state/41.checkpoint"),
        )
        .unwrap();
        let mut checkpointer = LegacyCheckpointer::open(config(dir.path())).unwrap();
        assert_eq!(checkpointer.next_generation(), 42);

        let first = commit(&mut checkpointer, SaveKind::Durable, 10);
        assert_eq!((first.generation, first.durable, first.archive_rank), (42, true, None));
        let hot = commit(&mut checkpointer, SaveKind::Hot, 11);
        assert_eq!((hot.generation, hot.durable), (43, false));
        assert_eq!(checkpointer.hot().manifest().unwrap().latest, Some(43));

        // A climb: rank and rankSinceMs as track_rank keeps them, and the first archive.
        assert_eq!(checkpointer.observe_rank(0, 1.0), None);
        assert_eq!(checkpointer.observe_rank(4, 500.0), Some(RankChange::Climbed(4)));
        let archived = commit(&mut checkpointer, SaveKind::Milestone(4), 12);
        assert_eq!(archived.archive_rank, Some(4));
        assert!(checkpointer.durable().archive_path(4).is_file());
        let decoded = store::load(&checkpointer.durable().archive_path(4)).unwrap();
        assert_eq!(decoded.runtime.generation, 44);
        assert_eq!(decoded.runtime.rank_since_ms, 500.0);
        assert_eq!(decoded.runtime.last_event_id, 12);
        // Falling back and climbing to the same rank again is not a new best for this process.
        assert_eq!(checkpointer.observe_rank(3, 600.0), Some(RankChange::FellBack(3)));
        assert_eq!(checkpointer.observe_rank(4, 700.0), Some(RankChange::Climbed(4)));
        let again = commit(&mut checkpointer, SaveKind::Milestone(4), 13);
        assert_eq!((again.durable, again.archive_rank), (true, None));
        assert_eq!(store::load(&checkpointer.durable().archive_path(4)).unwrap().runtime.generation, 44);

        for boundary in 14..20 {
            commit(&mut checkpointer, SaveKind::Durable, boundary);
        }
        let manifest = checkpointer.durable().manifest().unwrap();
        assert_eq!(manifest.archives.get(&4), Some(&44));
        assert!(checkpointer.durable().generation_path(44).is_file(), "an archived generation stays");
        assert!(!checkpointer.durable().generation_path(46).exists(), "rotated away");
        assert_eq!(manifest.previous.map(|p| p + 1), manifest.latest);
        checkpointer.close();

        // The next process allocates above everything either store holds.
        let checkpointer = LegacyCheckpointer::open(config(dir.path())).unwrap();
        assert_eq!(checkpointer.next_generation(), manifest.latest.unwrap() + 1);
    }

    #[test]
    fn the_intervals_are_the_legacy_loops() {
        let dir = tempfile::tempdir().unwrap();
        let mut checkpointer = LegacyCheckpointer::open(config(dir.path())).unwrap();
        let start = Instant::now();
        checkpointer.start_intervals(start);
        assert_eq!(checkpointer.due(start + Duration::from_secs(1)), None);
        assert_eq!(checkpointer.due(start + Duration::from_secs(5)), Some(SaveKind::Hot));
        assert_eq!(checkpointer.due(start + Duration::from_secs(6)), None);
        assert_eq!(checkpointer.due(start + Duration::from_secs(10)), Some(SaveKind::Hot));
        // The durable interval wins and restarts the hot one.
        let at = start + Duration::from_secs(300);
        assert_eq!(checkpointer.due(at), Some(SaveKind::Durable));
        assert_eq!(checkpointer.due(at + Duration::from_secs(4)), None);
        assert_eq!(checkpointer.due(at + Duration::from_secs(5)), Some(SaveKind::Hot));
    }

    fn gate() -> RestoreGate {
        RestoreGate {
            rom_sha256: "ab".repeat(32),
            compatibility: "kernel/adapter/fingerprint".to_owned(),
            migrates_from: Vec::new(),
            accepted: Vec::new(),
        }
    }

    fn select(
        checkpointer: &LegacyCheckpointer,
        gate: &RestoreGate,
        refuse: &[u64],
    ) -> Selection<u64> {
        let refuse = refuse.to_vec();
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(restore_from_store(checkpointer.hot(), checkpointer.durable(), gate, |c| {
            let refuse = refuse.clone();
            async move {
                if refuse.contains(&c.runtime.generation) {
                    Err(format!("participant refused generation {}", c.runtime.generation))
                } else {
                    Ok(c.runtime.generation)
                }
            }
        }))
    }

    #[test]
    fn restore_walks_the_legacy_order_and_takes_the_next_candidate_on_any_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let mut checkpointer = LegacyCheckpointer::open(config(dir.path())).unwrap();
        assert!(matches!(select(&checkpointer, &gate(), &[]), Selection::Fresh));
        commit(&mut checkpointer, SaveKind::Milestone(3), 10); // 1, archive 3
        commit(&mut checkpointer, SaveKind::Durable, 11); // 2
        commit(&mut checkpointer, SaveKind::Durable, 12); // 3
        commit(&mut checkpointer, SaveKind::Hot, 13); // 4
        commit(&mut checkpointer, SaveKind::Hot, 14); // 5

        let Selection::Restored(first) = select(&checkpointer, &gate(), &[]) else { panic!() };
        assert_eq!((first.installed, first.candidate.origin.as_str()), (5, "hot latest generation 5"));
        assert!(first.skipped.is_empty());

        // A torn hot latest, then a participant that refuses hot previous: durable latest.
        std::fs::write(checkpointer.hot().generation_path(5), b"torn").unwrap();
        let Selection::Restored(next) = select(&checkpointer, &gate(), &[4]) else { panic!() };
        assert_eq!(next.installed, 3);
        assert_eq!(next.skipped.len(), 2);
        // Everything above the archive refused: the archive's generation, then its own file.
        let Selection::Restored(deep) = select(&checkpointer, &gate(), &[2, 3, 4]) else { panic!() };
        assert_eq!(deep.installed, 1);
        assert_eq!(deep.candidate.origin, "durable archived generation 1 (rank 3)");

        // Another cartridge or another brain: refused by name, and nothing restores.
        let other = RestoreGate { rom_sha256: "cd".repeat(32), ..gate() };
        let Selection::Refused(reasons) = select(&checkpointer, &other, &[]) else { panic!() };
        assert!(reasons.iter().all(|(_, why)| why.contains("another cartridge") || why.contains("decoding")));
        let newer = RestoreGate { compatibility: "kernel/adapter-v2/fingerprint".to_owned(), ..gate() };
        assert!(matches!(select(&checkpointer, &newer, &[]), Selection::Refused(_)));
        // The operator's adapter migration is the legacy decision, unchanged.
        let migrating = RestoreGate {
            compatibility: "kernel/adapter-v2/fingerprint".to_owned(),
            migrates_from: vec!["adapter".to_owned()],
            accepted: vec!["adapter".to_owned()],
            ..gate()
        };
        let Selection::Restored(migrated) = select(&checkpointer, &migrating, &[]) else { panic!() };
        assert_eq!(migrated.migrated_from.as_deref(), Some("adapter"));
    }
}
