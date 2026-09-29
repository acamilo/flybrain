//! The legacy Game Boy composition on the session framework: one legacy agent (AGENT-01), the
//! legacy environment (ENV-01), and the `pokered-macros-v1` task and executor (this crate) in the
//! coordinator, over one router, in any of the launcher's execution modes.
//!
//! This is the whole new runtime of the live fly short of its edge (EDGE-01) and its FLYSIM01
//! export (STATE-02): what CUT-01's shadow runs beside the legacy service.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use flybus::{Grants, Pattern, Policy, RouterConfig};

use fly_session::coordinator::{AgentSlot, Coordinator, StepDetails, StepReport};
use fly_session::fly_session_types::extensions::ROLLBACK_POLICY;
use fly_session::fly_session_types::gameboy;
use fly_session::launcher::{
    ExecutionMode, Launcher, LegacyAgentLaunch, LegacyEnvironmentLaunch, SUPERVISOR_CLIENT,
    ThreadBudget, Via,
};
use fly_session::legacy_agent::{LegacyProfileKind, SPIKES_ATTACHMENT};
use fly_session::legacy_env::BackendConfig;
use fly_session::task::ActionExecutor;
use fly_session::types::*;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flysim::snapshot::MacroMode;

use crate::task::{PokeredConfig, PokeredTask, TaskRecord};
use fly_session::legacy_checkpoint::{
    AgentImport, CheckpointerConfig, LegacyCapture, LegacyCheckpointer, RankChange, SaveKind,
    SaveReport, SaveTicket, now_wall_ms,
};
use flysim::store::Checkpoint;
use std::time::Instant;

fn grants(f: impl FnOnce(&mut Grants)) -> Grants {
    let mut g = Grants::default();
    f(&mut g);
    g
}

/// The transitions a service session keeps in memory for diagnosis (about a minute at 60 Hz).
const HISTORY: usize = 4096;

/// Every other boundary's snapshot: the legacy feed's `snapshot_hz` (30 Hz) at 60 Hz.
const SNAPSHOT_EVERY: u64 = 2;

const COORDINATOR_CLIENT: &str = "coordinator";
const ENV_CLIENT: &str = "legacy-world";
const ENV_SERVICE: &str = "env.world";
const ENV_WORKER: &str = "world";
const AGENT_CLIENT: &str = "legacy-fly";

/// How one legacy session is composed.
#[derive(Clone, Debug)]
pub struct LegacyConfig {
    pub mode: ExecutionMode,
    pub rom_path: PathBuf,
    pub dataset_dir: PathBuf,
    pub profile: LegacyProfileKind,
    pub macro_mode: MacroMode,
    pub agent_id: Id,
    pub agent_threads: usize,
    /// Record the step details and the task records (a parity run).
    pub record: bool,
}

/// The macro channels and the macro group's hold for a mode: the decoder preset flysim builds.
pub fn channels_and_hold(mode: MacroMode) -> (Vec<String>, f64) {
    let channels: Vec<&str> = if mode.dealt() {
        flybrain_gb::macro_channels(crate::task::GAME)
    } else {
        Vec::new()
    };
    let preset = gameboy_decoder_config_with_macros(&channels);
    let hold_ms = preset
        .macros
        .as_ref()
        .or(preset.exclusive.as_ref())
        .map_or(0.0, |group| group.hold_ms);
    (channels.iter().map(|c| (*c).to_owned()).collect(), hold_ms)
}

/// A running legacy session: its launcher, its coordinator and the task object.
pub struct LegacySession {
    pub coordinator: Coordinator,
    pub task: PokeredTask,
    pub launcher: Launcher,
    pub rom: Vec<u8>,
    pub config: LegacyConfig,
    session_id: Id,
    backend: BackendConfig,
    /// Which generation of the world is running: 1 is the one the session started.
    world_generation: u32,
    /// The legacy store policy, once attached ([`LegacySession::attach_store`]).
    checkpointer: Option<LegacyCheckpointer>,
    saves: u64,
    /// Saves on the writer thread whose outcome has not been collected.
    pending_saves: Vec<SaveTicket>,
    /// The feed event log's watermark the saves record: the restored file's, carried over (the
    /// session runtime has no feed event log of its own before EDGE-01).
    last_event_id: u64,
}

/// Where the legacy store lives and how often it is written (the service's `[paths]` and
/// `[loop]`).
#[derive(Clone, Debug)]
pub struct StoreConfig {
    pub hot_dir: PathBuf,
    pub durable_dir: PathBuf,
    pub keep_generations: usize,
    pub hot_seconds: f64,
    pub checkpoint_seconds: f64,
    pub speed: f64,
}

/// This build's legacy compatibility string (`flysim --print-compatibility`): the kernel, the
/// adapter, the profile's dataset fingerprint, the plasticity rule and the pokered symbols. It is
/// the restore gate and what every save records (`legacy-gameboy-v1` section 12).
pub fn compatibility_of(config: &LegacyConfig) -> String {
    let adapter = flybrain_gb::pokemon_red::PokemonRedReward::new();
    let profile = config.profile.profile();
    flybrain_gb::Compatibility {
        neural_kernel_version: gameboy::KERNEL_VERSION,
        adapter: flybrain_gb::GameAdapter::id(&adapter),
        dataset_fingerprint: &profile.dataset_fingerprint,
        plasticity_version: gameboy::PLASTICITY_VERSION,
        pokered_commit: &flybrain_gb::GameAdapter::symbol_provenance(&adapter),
    }
    .string()
}

/// How a session came up.
#[derive(Debug)]
pub enum Boot {
    /// No checkpoint in either store: a fresh fly, warmed up.
    Fresh,
    /// A candidate restored, after `skipped` were refused.
    Restored {
        candidate: flysim::store::Candidate,
        skipped: Vec<(flysim::store::Candidate, String)>,
        migrated_from: Option<String>,
        imported: crate::import::Imported,
    },
}

/// `Sim::boot` on the session runtime: restore from the live FLYSIM01 stores in the legacy order
/// (hot latest, hot previous, durable latest, durable previous, milestone archives by rank), each
/// candidate held to the legacy gate (`RestoreGate`: the cartridge and the compatibility decision
/// with `FLY_ACCEPT_ADAPTERS`), falling to the next on *any* refusal -- unreadable, gated, or
/// refused by a participant's `State.StageRestore` or the task. A refused install fences its
/// session, so each attempt runs on a session of its own (a directory under `root` each). No
/// candidate at all is a fresh start; candidates that all fail are an error, as the legacy loop
/// refuses to start a fresh fly over them. Either way the store is attached and the startup
/// durable save is written, and the intervals run from then.
pub async fn boot(
    root: &Path,
    config: LegacyConfig,
    store: &StoreConfig,
) -> Result<(LegacySession, Boot), String> {
    let hot = flysim::store::Store::new(&store.hot_dir, store.keep_generations);
    let durable = flysim::store::Store::new(&store.durable_dir, store.keep_generations);
    let candidates = flysim::store::restore_order(&hot, &durable);
    let rom = std::fs::read(&config.rom_path)
        .map_err(|e| format!("the cartridge {}: {e}", config.rom_path.display()))?;
    let adapter = flybrain_gb::pokemon_red::PokemonRedReward::new();
    let gate = fly_session::legacy_checkpoint::RestoreGate::from_env(
        &sha256_hex(&rom),
        &compatibility_of(&config),
        flybrain_gb::GameAdapter::migrates_from(&adapter),
    );
    let mut skipped: Vec<(flysim::store::Candidate, String)> = Vec::new();
    let mut outcome = None;
    for (attempt, candidate) in candidates.iter().enumerate() {
        let checkpoint = match flysim::store::load(&candidate.path) {
            Ok(checkpoint) => checkpoint,
            Err(e) => {
                skipped.push((candidate.clone(), format!("{e:#}")));
                continue;
            }
        };
        let migrated_from = match gate.check(&checkpoint) {
            Ok(from) => from,
            Err(reason) => {
                skipped.push((candidate.clone(), reason));
                continue;
            }
        };
        let mut session =
            LegacySession::start(&root.join(format!("attempt-{attempt}")), config.clone()).await?;
        let installed = match session.bootstrap().await {
            Ok(()) => session.import_checkpoint(&checkpoint, &id("e2")).await,
            Err(e) => Err(e),
        };
        match installed {
            Ok(imported) => {
                let rank = session.task.rank();
                session.attach_store(store)?;
                session
                    .checkpointer_mut()
                    .expect("attached")
                    .restored(rank, imported.rank_since_ms);
                outcome = Some((
                    session,
                    Boot::Restored {
                        candidate: candidate.clone(),
                        skipped: std::mem::take(&mut skipped),
                        migrated_from,
                        imported,
                    },
                ));
                break;
            }
            Err(reason) => {
                session.stop().await;
                skipped.push((candidate.clone(), reason));
            }
        }
    }
    let (mut session, boot) = match outcome {
        Some(pair) => pair,
        None if candidates.is_empty() => {
            let mut session = LegacySession::start(&root.join("fresh"), config).await?;
            session.bootstrap().await?;
            session.attach_store(store)?;
            (session, Boot::Fresh)
        }
        None => {
            let reasons: Vec<String> = skipped
                .iter()
                .map(|(candidate, reason)| format!("{}: {reason}", candidate.origin))
                .collect();
            return Err(format!(
                "every one of the {} checkpoint candidates failed to load; refusing to start a \
                 fresh run over them:\n{}",
                candidates.len(),
                reasons.join("\n")
            ));
        }
    };
    // `Sim::boot`: a durable save of the state the process starts from, then the intervals.
    session.save(SaveKind::Durable).await?;
    session
        .checkpointer_mut()
        .expect("attached")
        .start_intervals(Instant::now());
    Ok((session, boot))
}

/// A save [`LegacySession::advance`] queued at the boundary it reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveQueued {
    pub generation: u64,
    pub durable: bool,
    pub archive_rank: Option<u32>,
}

/// One transition as a parity run sees it.
pub struct Step {
    pub report: StepReport,
    pub details: Option<StepDetails>,
    pub records: Vec<TaskRecord>,
}

impl LegacySession {
    /// Builds the router, launches the world and the fly, and builds the coordinator with the task
    /// object's two faces. Nothing has been initialized yet.
    pub async fn start(root: &Path, config: LegacyConfig) -> Result<LegacySession, String> {
        let rom = std::fs::read(&config.rom_path)
            .map_err(|e| format!("the cartridge {}: {e}", config.rom_path.display()))?;
        let rom_digest = sha256_hex(&rom);
        let session_id = id("legacy");
        let epoch = id("e1");
        let store_root = root.join("store");
        let sockets = root.join("sockets");
        std::fs::create_dir_all(&sockets).map_err(|e| format!("sockets: {e}"))?;
        let agent_service = format!("agent.{}", config.agent_id);
        let policy = Policy::closed()
            .client(
                COORDINATOR_CLIENT,
                grants(|g| {
                    g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")];
                    g.publish = vec![Pattern::prefix("session.")];
                    g.manage_topics = vec![Pattern::prefix("session.")];
                    g.register = vec![Pattern::prefix("session.")];
                }),
            )
            // The launcher's supervisor: health checks and shutdown, never a registration.
            .client(
                SUPERVISOR_CLIENT,
                grants(|g| g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")]),
            )
            .client(
                ENV_CLIENT,
                grants(|g| g.register = vec![Pattern::exact(ENV_SERVICE)]),
            )
            .client(
                &format!("{ENV_CLIENT}-r2"),
                grants(|g| g.register = vec![Pattern::exact(ENV_SERVICE)]),
            )
            .client(
                AGENT_CLIENT,
                grants(|g| g.register = vec![Pattern::exact(&agent_service)]),
            );
        let mut router_config = RouterConfig::new(&store_root);
        router_config.policy = policy;
        let router = flybus::Router::new(router_config).map_err(|e| format!("router: {e}"))?;
        let budget = ThreadBudget::new(2 + config.agent_threads, 1).map_err(|e| e.message)?;
        let mut launcher = Launcher::start(
            router,
            config.mode,
            // In-process participants reach the router in memory: no socket between two tasks
            // of one process (a separate process always uses the socket).
            Via::Memory,
            &store_root,
            &sockets,
            budget,
        )
        .await
        .map_err(|e| format!("launcher: {}", e.message))?;

        let mut backend = BackendConfig::legacy(&rom_digest);
        backend.slots = vec![id(crate::task::SLOT)];
        launcher
            .launch_legacy_environment(LegacyEnvironmentLaunch {
                session_id: session_id.clone(),
                worker_id: id(ENV_WORKER),
                incarnation_id: id("world-inc-1"),
                worker_threads: 1,
                rom_path: config.rom_path.clone(),
                backend: backend.clone(),
                client_id: ENV_CLIENT.to_owned(),
                service: ENV_SERVICE.to_owned(),
            })
            .await
            .map_err(|e| format!("the world: {}", e.message))?;
        let environment = launcher
            .worker(&id(ENV_WORKER))
            .expect("launched")
            .worker_ref();

        let (macro_channels, hold_ms) = channels_and_hold(config.macro_mode);
        let identity = launcher
            .launch_legacy_agent(LegacyAgentLaunch {
                session_id: session_id.clone(),
                agent_id: config.agent_id.clone(),
                port_id: id(fly_session::legacy_env::DEFAULT_PORT),
                incarnation_id: parse_id(&format!("{}-inc-1", config.agent_id))?,
                worker_threads: config.agent_threads,
                dataset_dir: config.dataset_dir.clone(),
                profile: config.profile,
                macro_channels: macro_channels.clone(),
                client_id: AGENT_CLIENT.to_owned(),
                service: agent_service,
            })
            .await
            .map_err(|e| format!("the fly: {}", e.message))?;
        let agent_ref = launcher
            .worker(&config.agent_id)
            .expect("launched")
            .worker_ref();
        let mut slot = AgentSlot::new(
            agent_ref,
            config.agent_id.clone(),
            id(fly_session::legacy_env::DEFAULT_PORT),
            config.profile.profile().asset,
            fly_session::legacy_parity::LEGACY_SEED,
        );
        slot.worker_threads = identity.worker_threads as u64;
        slot.model_version = gameboy::KERNEL_VERSION.to_owned();
        slot.plasticity_version = gameboy::PLASTICITY_VERSION.to_owned();

        let task = PokeredTask::new(
            PokeredConfig {
                agent_id: config.agent_id.clone(),
                port_id: id(fly_session::legacy_env::DEFAULT_PORT),
                mode: config.macro_mode,
                macro_channels,
                hold_ms,
                record: config.record,
            },
            &rom,
            &rom_digest,
            &epoch,
        )?;
        let client = launcher
            .connect(COORDINATOR_CLIENT)
            .await
            .map_err(|e| format!("connect: {}", e.message))?;
        let executors: BTreeMap<Id, Box<dyn ActionExecutor>> =
            BTreeMap::from([(config.agent_id.clone(), task.executor())]);
        let mut coordinator = Coordinator::new(
            client,
            session_id.clone(),
            epoch,
            id("ep1"),
            environment,
            vec![slot],
            task.task(),
            executors,
        );
        coordinator.set_environment_config(
            backend.asset_ref(),
            fly_session::environment::synthetic_asset("pokered-task", gameboy::EXECUTOR_ID),
        );
        coordinator.set_media(
            &[gameboy::VIEW_ID],
            &[fly_session::legacy_env::AUDIO_STREAM_ID],
        );
        coordinator.set_state_format(fly_session::legacy_env::STATE_FORMAT_ID);
        coordinator.set_decision_schema(gameboy::CHANNELS.schema_ref());
        coordinator
            .declare_rollback_policy(ROLLBACK_POLICY)
            .map_err(|e| e.message)?;
        // The legacy loop runs as fast as its brain allows when measured, and the service paces
        // itself; a parity run is a measurement.
        if config.record {
            coordinator.request_commit_attachments(&[SPIKES_ATTACHMENT]);
        } else {
            // A service session runs indefinitely: its in-memory history stays bounded.
            coordinator.bound_history(HISTORY);
            // The legacy feed publishes at `snapshot_hz` (30 Hz) from a 60 Hz loop.
            coordinator.snapshot_every(SNAPSHOT_EVERY);
        }
        Ok(LegacySession {
            coordinator,
            task,
            launcher,
            rom,
            config,
            session_id,
            backend,
            world_generation: 1,
            checkpointer: None,
            saves: 0,
            pending_saves: Vec::new(),
            last_event_id: 0,
        })
    }

    /// Hello, `Environment.Initialize` (the one-frame scaffold), the task's bootstrap on O[0],
    /// `Agent.Initialize` (the warm-up): `Ready(0)`.
    pub async fn bootstrap(&mut self) -> Result<(), String> {
        self.coordinator
            .bootstrap()
            .await
            .map_err(|e| format!("bootstrap ({}): {}", e.detail, e.error.message))?;
        if self.config.record {
            self.coordinator.disable_pacing();
        }
        Ok(())
    }

    /// The agent's identity as an import stamps it.
    pub fn agent_import(&self) -> AgentImport {
        let (macro_channels, _) = channels_and_hold(self.config.macro_mode);
        AgentImport {
            agent_id: self.config.agent_id.clone(),
            profile: self.config.profile.profile().asset,
            seed: fly_session::legacy_parity::LEGACY_SEED,
            macro_channels,
        }
    }

    /// This build's legacy compatibility string ([`compatibility_of`]).
    pub fn compatibility(&self) -> String {
        compatibility_of(&self.config)
    }

    /// Installs a FLYSIM01 checkpoint as this session's start (after [`LegacySession::bootstrap`],
    /// before any transition): the world is replaced by a fresh one (the environment stages a
    /// restore only on a replacement, as every group restore does), then [`crate::import`] runs
    /// the group install and the session resumes at the checkpoint's boundary.
    pub async fn import_checkpoint(
        &mut self,
        checkpoint: &Checkpoint,
        epoch: &Id,
    ) -> Result<crate::import::Imported, String> {
        if self.world_generation != 1 {
            return Err("this session has already replaced its world".to_owned());
        }
        self.launcher.reap(&id(ENV_WORKER), &id("import")).await;
        self.world_generation = 2;
        self.launcher
            .launch_legacy_environment(LegacyEnvironmentLaunch {
                session_id: self.session_id.clone(),
                worker_id: id(ENV_WORKER),
                incarnation_id: id("world-inc-2"),
                worker_threads: 1,
                rom_path: self.config.rom_path.clone(),
                backend: self.backend.clone(),
                client_id: format!("{ENV_CLIENT}-r2"),
                service: ENV_SERVICE.to_owned(),
            })
            .await
            .map_err(|e| format!("the replacement world: {}", e.message))?;
        let world = self
            .launcher
            .worker(&id(ENV_WORKER))
            .expect("launched")
            .worker_ref();
        self.coordinator
            .replace_participant(&id(ENV_WORKER), world)
            .map_err(|e| format!("replacing the world: {}", e.error.message))?;
        let rom = self.rom.clone();
        let agent = self.agent_import();
        let imported = crate::import::import_flysim01(
            &mut self.coordinator,
            &self.task,
            checkpoint,
            &rom,
            &agent,
            self.backend.audio_sample_rate,
            epoch,
        )
        .await?;
        if self.config.record {
            self.coordinator.disable_pacing();
        }
        self.last_event_id = checkpoint.runtime.last_event_id;
        Ok(imported)
    }

    /// Decodes FLYSIM01 bytes and installs them ([`LegacySession::import_checkpoint`]).
    pub async fn import_flysim01(
        &mut self,
        bytes: &[u8],
        epoch: &Id,
    ) -> Result<crate::import::Imported, String> {
        let checkpoint = flysim::store::decode(bytes).map_err(|e| format!("FLYSIM01: {e:#}"))?;
        self.import_checkpoint(&checkpoint, epoch).await
    }

    // -- the store (STATE-02) ------------------------------------------------------------------

    /// Attaches the legacy store policy (`LegacyCheckpointer`): FLYSIM01 generations in the hot
    /// and durable stores, milestone archives, the legacy intervals. Rollbacks are deferred from
    /// here on, so a milestone capture lands between the boundary's slot save and its rollback.
    pub fn attach_store(&mut self, store: &StoreConfig) -> Result<(), String> {
        let checkpointer = LegacyCheckpointer::open(CheckpointerConfig {
            hot_dir: store.hot_dir.clone(),
            durable_dir: store.durable_dir.clone(),
            keep_generations: store.keep_generations,
            hot_seconds: store.hot_seconds,
            checkpoint_seconds: store.checkpoint_seconds,
            compatibility: self.compatibility(),
            speed: store.speed,
            slot_id: id(crate::task::SLOT),
        })?;
        self.checkpointer = Some(checkpointer);
        self.coordinator.defer_rollbacks(true);
        Ok(())
    }

    pub fn checkpointer(&self) -> Option<&LegacyCheckpointer> {
        self.checkpointer.as_ref()
    }

    pub fn checkpointer_mut(&mut self) -> Option<&mut LegacyCheckpointer> {
        self.checkpointer.as_mut()
    }

    /// One save of the committed boundary, written by the checkpointer's writer thread; waits for
    /// the commit, so a caller that goes on to read the store finds it. For the startup and
    /// shutdown saves, which the legacy loop takes blocking too.
    pub async fn save(&mut self, kind: SaveKind) -> Result<SaveReport, String> {
        let ticket = self.queue_save(kind).await?;
        ticket
            .reply
            .await
            .map_err(|_| "the checkpoint writer stopped".to_owned())?
    }

    /// One save of the committed boundary, queued: the participants capture now (the boundary's
    /// state) and the encoding and fsync run on the checkpointer's writer thread while the loop
    /// goes on, as the legacy loop hands its saves to its writer thread (TASK-01 review N4). The
    /// outcome is collected by [`LegacySession::completed_saves`].
    pub async fn queue_save(&mut self, kind: SaveKind) -> Result<SaveTicket, String> {
        self.saves += 1;
        let (boundary, world, agents) = self
            .coordinator
            .capture_payloads(&parse_id(&format!("legacy-save-{}", self.saves))?)
            .await
            .map_err(|e| format!("capture ({}): {}", e.detail, e.error.message))?;
        let [(_, agent)] = <[(Id, Vec<u8>); 1]>::try_from(agents)
            .map_err(|_| "the legacy composition has exactly one agent".to_owned())?;
        let (task, _slot_filled) = self.task.task_half();
        let capture = LegacyCapture {
            boundary,
            agent_payload: agent,
            world_payload: world,
            task,
        };
        let last_event_id = self.last_event_id;
        let checkpointer = self.checkpointer.as_mut().ok_or("no store is attached")?;
        checkpointer.save(kind, capture, last_event_id, now_wall_ms())
    }

    /// The saves queued so far that have committed (or failed), without waiting; with `wait`,
    /// every queued save's outcome. A failed save is an error: the caller decides.
    pub async fn completed_saves(&mut self, wait: bool) -> Result<Vec<SaveReport>, String> {
        let mut done = Vec::new();
        let mut still = Vec::new();
        for mut ticket in std::mem::take(&mut self.pending_saves) {
            let outcome = if wait {
                Some((&mut ticket.reply).await.map_err(|_| ()))
            } else {
                match ticket.reply.try_recv() {
                    Ok(result) => Some(Ok(result)),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(Err(())),
                }
            };
            match outcome {
                None => still.push(ticket),
                Some(Ok(result)) => done.push(result?),
                Some(Err(())) => return Err("the checkpoint writer stopped".to_owned()),
            }
        }
        self.pending_saves = still;
        Ok(done)
    }

    /// One transition and what the legacy loop does at the boundary it reaches, in the declared
    /// order (`legacy-gameboy-v1` sections 4 and 16): the slot save (inside the step), the
    /// milestone archive when the rank climbed (`Sim::track_rank`), the rollback, a durable save
    /// after it, then the interval saves (`checkpoint_if_due`).
    pub async fn advance(&mut self) -> Result<(Step, Vec<SaveQueued>), String> {
        let report = self
            .coordinator
            .step()
            .await
            .map_err(|e| format!("step ({} at {}): {}", e.detail, e.phase, e.error.message))?;
        let mut saves = Vec::new();
        if self.checkpointer.is_some() {
            let rank = self.task.rank();
            let ms = self.task.brain_ms();
            let change = self
                .checkpointer
                .as_mut()
                .expect("attached")
                .observe_rank(rank, ms);
            if let Some(RankChange::Climbed(rank)) = change {
                let ticket = self.queue_save(SaveKind::Milestone(rank)).await?;
                saves.push(SaveQueued {
                    generation: ticket.generation,
                    durable: ticket.durable,
                    archive_rank: ticket.archive_rank,
                });
                self.pending_saves.push(ticket);
            }
        }
        let rolled_back = self
            .coordinator
            .apply_pending_rollback()
            .await
            .map_err(|e| {
                format!(
                    "rollback ({} at {}): {}",
                    e.detail, e.phase, e.error.message
                )
            })?;
        if self.checkpointer.is_some() {
            if rolled_back {
                let ticket = self.queue_save(SaveKind::Durable).await?;
                saves.push(SaveQueued {
                    generation: ticket.generation,
                    durable: ticket.durable,
                    archive_rank: ticket.archive_rank,
                });
                self.pending_saves.push(ticket);
            }
            let due = self
                .checkpointer
                .as_mut()
                .expect("attached")
                .due(Instant::now());
            if let Some(kind) = due {
                let ticket = self.queue_save(kind).await?;
                saves.push(SaveQueued {
                    generation: ticket.generation,
                    durable: ticket.durable,
                    archive_rank: ticket.archive_rank,
                });
                self.pending_saves.push(ticket);
            }
        }
        Ok((
            Step {
                report,
                details: self.coordinator.take_details(),
                records: self.task.take_records(),
            },
            saves,
        ))
    }

    /// One transition, with its details when recorded ([`LegacySession::advance`] without the
    /// saves it reports).
    pub async fn step(&mut self) -> Result<Step, String> {
        self.advance().await.map(|(step, _)| step)
    }

    /// A durable save, as the legacy loop takes on shutdown, then the store's writer is closed.
    pub async fn shutdown_save(&mut self) -> Result<Option<SaveReport>, String> {
        if self.checkpointer.is_none() {
            return Ok(None);
        }
        let report = self.save(SaveKind::Durable).await?;
        self.completed_saves(true).await?;
        if let Some(checkpointer) = self.checkpointer.as_mut() {
            checkpointer.close();
        }
        Ok(Some(report))
    }

    pub async fn stop(mut self) {
        self.launcher.reap_all(&id("done")).await;
    }
}
