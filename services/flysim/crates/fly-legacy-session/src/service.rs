//! The session runtime as the live service (SERVE-01): `flysim-session`.
//!
//! The legacy composition ([`crate::composition`]) runs here as a long-lived process behind the
//! very listeners `flysim` serves: [`flysim::serve`] binds the feed (`:7400`, or the feed bus for
//! `fly-edge`), the control API (`:7401`) and the metrics listener (`:9101`), handles the signals,
//! and hands this host the same three things it hands the legacy loop -- the [`Shared`] state, the
//! snapshot slot and the command queue. The feed and control contracts are therefore served by
//! the same code; what this module adds is the loop that fills them from a session instead of
//! from `flysim::simloop::Sim`, keeping `Sim`'s order, cadence and bookkeeping call site for call
//! site:
//!
//! | legacy `Sim` | here |
//! | --- | --- |
//! | `boot`: restore or warm up, versions, chat sidecar, journal boot header, startup durable save | [`SessionHost::boot`]: [`crate::composition::boot_unsaved`], the same, header `runtime: fly-session` |
//! | `drain_commands`: stimulate, reward, chat, checkpoint, pause, resume, shutdown | [`SessionHost::drain_commands`]; sugar by `RateLimiter` over the last commit's pulse, admitted at the next Prepare |
//! | `step_frame`: the frame, macro/reward events, `track_rank` + milestone archive, ratchet rollback + recovery | [`SessionHost::step_frame`]: `Coordinator::step`, the task's [`crate::task::FeedRecord`], the checkpointer's rank, `apply_pending_rollback` |
//! | `publish`: the header from the network, the adapter and the macro layer | [`SessionHost::publish`]: the agent's `Legacy.FeedStatus`, the task's parts, the committed view, the step's audio and spikes, through flysim's own `feed_*` builders |
//! | `checkpoint_if_due` (hot 5 s, durable 300 s, wall clock), pause, forced, shutdown | the same, through [`crate::composition::LegacySession::queue_save`] and STATE-02's `LegacyCheckpointer` |
//! | the sugar journal (`crate::journal` of flysim) | the same journal, `runtime: fly-session` |
//!
//! Declared differences from the legacy service (SERVE-01, `implementation.md`):
//!
//! - `POST /reward` is always refused (403): `control.allow_reward` is forced off, because the
//!   operator reward pulse has no session-framework counterpart (`legacy-gameboy-v1` section 15).
//! - A fresh start publishes no audio for its setup frame (the environment discards O[0]'s).
//! - `FLY_TRACE` and `FLY_PROFILE_SECONDS` are not implemented here; SHADOW-01 compares runs.
//! - Only the Pokémon Red composition exists on the session runtime.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use fly_session::fly_session_types::gameboy;
use fly_session::launcher::ExecutionMode;
use fly_session::legacy_agent::{LegacyProfileKind, METHOD_FEED_STATUS, SPIKES_ATTACHMENT};
use fly_session::legacy_checkpoint::{RankChange, SaveKind, SaveReport, SaveTicket};
use fly_session::types::*;
use flybrain_gb::adapter::GameAdapter;
use flysim::chat::{ChatLimiter, ChatRefusal, ChatRing, DenyList};
use flysim::config::Config;
use flysim::eventlog::{EventLog, NewEvent, now_wall_ms, utc_day};
use flysim::journal::{BootHeader, Entry, Input, SugarJournal};
use flysim::metrics::Metrics;
use flysim::pacing::{Pacer, RealtimeWindow};
use flysim::ratelimit::{RateLimiter, Refusal};
use flysim::simloop::{
    COMMAND_QUEUE, Command, DecoderChannelStatus, DecoderStatus, Shared, Versions, admit_chat,
    feed_game, feed_milestone, feed_rates, feed_sugar, sugar_label,
};
use flysim::snapshot::{
    AttachmentKind, DcBlocker, FeedEvent, FeedEventKind, FeedHeader, FeedLearning, FeedStatus,
    PROTOCOL, RewardKind, Snapshot, f32_bytes, finite,
};

use crate::composition::{Boot, LegacyConfig, LegacySession, StoreConfig, boot_unsaved};
use crate::task::GAME;

/// The runtime name the sugar journal's boot header records.
pub const RUNTIME: &str = "fly-session";

/// Where the session's own router store and worker sockets live (tmpfs on the containers).
pub const SESSION_DIR_ENV: &str = "FLY_SESSION_DIR";
pub const DEFAULT_SESSION_DIR: &str = "/run/fly/session";
/// `in-process` (the default), `thread` or `process` (`fly-session`'s launcher modes).
pub const SESSION_MODE_ENV: &str = "FLY_SESSION_MODE";
/// `production` (the default, `gameboy-legacy-fafb-v783-v1`) or `toy` (tests only).
pub const SESSION_PROFILE_ENV: &str = "FLY_SESSION_PROFILE";

/// `0` keeps every thread floating over the cpuset; anything else, or unset, places them
/// ([`place_threads`]).
pub const SESSION_PIN_ENV: &str = "FLY_SESSION_PIN";

/// How the service composes its session, beyond flysim's own [`Config`].
#[derive(Clone, Debug)]
pub struct ServiceOptions {
    pub mode: ExecutionMode,
    pub profile: LegacyProfileKind,
    pub session_dir: PathBuf,
}

impl ServiceOptions {
    /// From `FLY_SESSION_MODE`, `FLY_SESSION_PROFILE` and `FLY_SESSION_DIR`.
    pub fn from_env() -> Result<ServiceOptions> {
        let mode = match std::env::var(SESSION_MODE_ENV).ok().as_deref() {
            None | Some("") | Some("in-process") => ExecutionMode::InProcess,
            Some("thread") => ExecutionMode::Thread,
            Some("process") => ExecutionMode::Process,
            Some(other) => bail!("{SESSION_MODE_ENV}={other}: in-process, thread or process"),
        };
        let profile = match std::env::var(SESSION_PROFILE_ENV).ok().as_deref() {
            None | Some("") | Some("production") => LegacyProfileKind::Production,
            Some("toy") => LegacyProfileKind::Toy,
            Some(other) => bail!("{SESSION_PROFILE_ENV}={other}: production or toy"),
        };
        let session_dir = std::env::var_os(SESSION_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SESSION_DIR));
        Ok(ServiceOptions {
            mode,
            profile,
            session_dir,
        })
    }
}

/// The session's composition from the service configuration, refusing what the session runtime
/// cannot run rather than running something else.
pub fn legacy_config(config: &Config, options: &ServiceOptions) -> Result<LegacyConfig> {
    if config.loop_.game != GAME {
        bail!(
            "the session runtime runs {GAME} only; FLY_GAME is {:?} (switch back with \
             `fly-runtime legacy`)",
            config.loop_.game
        );
    }
    if config.loop_.warmup_ms != gameboy::WARMUP_MS {
        bail!(
            "loop.warmup_ms is {} but the legacy profile's warm-up is {} ms",
            config.loop_.warmup_ms,
            gameboy::WARMUP_MS
        );
    }
    let audio = fly_session::legacy_env::BackendConfig::legacy("0".repeat(64).as_str())
        .audio_sample_rate;
    if u64::from(config.feed.audio_hz) != audio {
        bail!(
            "feed.audio_hz is {} but the legacy environment runs its audio at {audio} Hz",
            config.feed.audio_hz
        );
    }
    Ok(LegacyConfig {
        mode: options.mode,
        rom_path: config.paths.rom.clone(),
        dataset_dir: config.paths.dataset.clone(),
        profile: options.profile,
        macro_mode: config.macros.mode,
        agent_id: id("fly"),
        agent_threads: config.loop_.threads.max(1),
        record: false,
    })
}

/// The legacy store the service writes: flysim's `[paths]` and `[loop]`.
pub fn store_config(config: &Config) -> StoreConfig {
    StoreConfig {
        hot_dir: config.paths.hot_dir.clone(),
        durable_dir: config.paths.save_dir.clone(),
        keep_generations: config.loop_.keep_generations,
        hot_seconds: config.loop_.hot_seconds,
        checkpoint_seconds: config.loop_.checkpoint_seconds,
        speed: config.loop_.speed,
    }
}

/// The compatibility string this build would write (`flysim-session --print-compatibility`):
/// the session's own, from the profile, which must equal `flysim --print-compatibility`.
pub fn compatibility_string(config: &Config, options: &ServiceOptions) -> Result<String> {
    Ok(crate::composition::compatibility_of(&legacy_config(
        config, options,
    )?))
}

/// The configuration the session runtime serves under: flysim's, with `control.allow_reward`
/// forced off (the operator pulse is refused on this runtime, so `POST /reward` answers 403 as
/// it does on every shipped legacy configuration).
pub fn service_config(mut config: Config) -> Config {
    if config.control.allow_reward {
        tracing::warn!(
            "control.allow_reward is on, but the session runtime has no operator reward pulse \
             (legacy-gameboy-v1 section 15): POST /reward answers 403"
        );
        config.control.allow_reward = false;
    }
    config
}

/// Run the service until a signal or a fatal error: [`flysim::serve`] around a [`SessionHost`].
pub fn run(config: Config, options: ServiceOptions) -> Result<()> {
    let config = service_config(config);
    // Refuse a configuration the session cannot run before any listener is bound.
    legacy_config(&config, &options)?;
    for (variable, what) in [
        (flysim::trace::ENV, "the per-frame trace"),
        ("FLY_PROFILE_SECONDS", "the per-phase profile"),
    ] {
        if std::env::var_os(variable).is_some() {
            tracing::warn!("{variable} is set, but {what} is a legacy-loop tool; ignored");
        }
    }
    place_threads(&config, &options);
    flysim::serve(config, move |shared, snapshots, commands, notifier| {
        run_host(shared, snapshots, commands, notifier, &options)
    })
}

/// PERF-02: one CPU of its own for each spawned sweep worker, the rest of the cpuset for every
/// other thread (the dispatching tokio worker, the coordinator, the listeners, the checkpoint
/// writer), before any of them is spawned. The legacy loop's four busy threads keep their CPUs by
/// themselves; the session's host threads sleep and wake around every transition, and without
/// this the sweep workers' wake-ups often land two on one CPU (`flybrain_core::pool::
/// place_workers`). In-process and thread modes only: a worker process would inherit the host's
/// mask without the plan. Placement changes no result, trace or checkpoint.
fn place_threads(config: &Config, options: &ServiceOptions) {
    if std::env::var(SESSION_PIN_ENV).ok().as_deref() == Some("0") {
        tracing::info!("{SESSION_PIN_ENV}=0: the threads float over the cpuset");
        return;
    }
    if !matches!(
        options.mode,
        ExecutionMode::InProcess | ExecutionMode::Thread
    ) {
        return;
    }
    let workers = config.loop_.threads;
    match flybrain_core::pool::place_workers(workers) {
        Some(placement) => tracing::info!(
            sweep_workers = ?placement.workers,
            host = ?placement.host,
            "sweep workers pinned one per cpu; every other thread on the host cpus"
        ),
        None => tracing::info!(
            workers,
            "threads not placed (fewer cpus than sweep threads, or no pool): they float"
        ),
    }
}

/// The runtime [`flysim::serve`] runs: boot a [`SessionHost`] on a session runtime of its own,
/// report ready, run it until a shutdown, stop every worker. Also what a test runs in place of
/// the listeners, as the legacy side runs `Sim::boot` and `Sim::run`.
pub fn run_host(
    shared: Arc<Shared>,
    snapshots: watch::Sender<Arc<Snapshot>>,
    commands: mpsc::Receiver<Command>,
    notifier: &flysim::sdnotify::Notifier,
    options: &ServiceOptions,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("fly-session")
        .enable_all()
        .build()
        .context("building the session runtime")?;
    let result = runtime.block_on(async {
        let mut host = SessionHost::boot(shared, snapshots, commands, options).await?;
        notifier.notify("READY=1\nSTATUS=session running\n");
        let result = host.run(notifier).await;
        notifier.notify("STOPPING=1\n");
        host.stop().await;
        result
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

/// A queued save and what waits on it.
struct PendingSave {
    ticket: SaveTicket,
    wall_ms: u64,
    reply: Option<oneshot::Sender<std::result::Result<u64, String>>>,
}

/// The agent's side of one published header ([`METHOD_FEED_STATUS`]).
#[derive(Clone, Debug)]
struct AgentFeed {
    brain_ms: f64,
    rates: Vec<(String, f64)>,
    population_rate: f64,
    reward_remaining_ms: f64,
    learning: FeedLearning,
    decoder: DecoderStatus,
}

impl Default for AgentFeed {
    fn default() -> AgentFeed {
        AgentFeed {
            brain_ms: 0.0,
            rates: Vec::new(),
            population_rate: 0.0,
            reward_remaining_ms: 0.0,
            learning: FeedLearning {
                enabled: false,
                updates: 0,
                changed: 0,
                synapses: 0,
                signal: 0.0,
            },
            decoder: DecoderStatus::default(),
        }
    }
}

impl AgentFeed {
    fn parse(value: &Value) -> Result<AgentFeed> {
        let number = |v: &Value, what: &str| {
            v.as_f64()
                .ok_or_else(|| anyhow!("Legacy.FeedStatus: {what} is not a number"))
        };
        let rates = value["rates"]
            .as_array()
            .ok_or_else(|| anyhow!("Legacy.FeedStatus: no rates"))?
            .iter()
            .map(|pair| {
                let name = pair[0]
                    .as_str()
                    .ok_or_else(|| anyhow!("Legacy.FeedStatus: a rate without a role"))?;
                Ok((name.to_owned(), number(&pair[1], "a rate")?))
            })
            .collect::<Result<Vec<_>>>()?;
        let learning = &value["learning"];
        let stats = flybrain_core::plasticity::LearningStats {
            version: String::new(),
            enabled: learning["enabled"].as_bool().unwrap_or(false),
            synapses: learning["synapses"].as_u64().unwrap_or(0) as usize,
            mushroom: 0,
            output: 0,
            updates: number(&learning["updates"], "learning.updates")?,
            changed: learning["changed"].as_u64().unwrap_or(0),
            mean_change: 0.0,
            max_change: 0.0,
            signal: number(&learning["signal"], "learning.signal")?,
        };
        let decoder = &value["decoder"];
        let channels = decoder["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|channel| {
                Ok(DecoderChannelStatus {
                    channel: channel["channel"].as_str().unwrap_or_default().to_owned(),
                    role: channel["role"].as_str().unwrap_or_default().to_owned(),
                    baseline: number(&channel["baseline"], "a baseline")?,
                    score: number(&channel["score"], "a score")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(AgentFeed {
            brain_ms: number(&value["brainMs"], "brainMs")?,
            rates,
            population_rate: number(&value["populationRate"], "populationRate")?,
            reward_remaining_ms: number(&value["rewardRemainingMs"], "rewardRemainingMs")?,
            learning: flysim::simloop::feed_learning(&stats),
            decoder: DecoderStatus {
                calibrated: decoder["calibrated"].as_bool().unwrap_or(false),
                pending: decoder["pending"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                channels,
            },
        })
    }
}

/// The service host: the session, and everything `Sim` keeps around its frame.
pub struct SessionHost {
    shared: Arc<Shared>,
    snapshots: watch::Sender<Arc<Snapshot>>,
    commands: mpsc::Receiver<Command>,
    session: LegacySession,
    agent_id: Id,
    log: EventLog,
    journal: SugarJournal,

    status: FeedStatus,
    pending_recovery: bool,
    seq: u64,
    started: Instant,
    realtime: RealtimeWindow,
    next_publish: Instant,

    pending_saves: Vec<PendingSave>,

    limiter: RateLimiter,
    sugar_last_by: Option<String>,
    sugar_today: u64,
    sugar_day: String,
    /// The pulse the network holds now, as the admission rule reads it: the last commit's
    /// `stimulusRemainingMs`, raised by every sugar admitted since (`network.stimulate` is a
    /// maximum), so two admissions in one drain see what the legacy loop's second one sees.
    remaining_ms: f64,
    sugar_serial: u64,

    chat_ring: ChatRing,
    chat_limits: ChatLimiter,
    deny_list: DenyList,

    /// The audio of the transitions since the last publish, DC-blocked (the legacy
    /// `pending_audio`).
    pending_audio: Vec<f32>,
    dc_blocker: DcBlocker,
    /// The OR of the commits' spike bitsets since the last publish: exactly the legacy
    /// `spike_bitset(last_spike_ms, last_publish_ms, ms)` (a neuron's last spike is at or after
    /// the window's start iff it is at or after the start of one of the commits inside it).
    spikes: Vec<u8>,
    /// The agent's feed status as last read, for a publish whose read failed.
    last_feed: AgentFeed,

    semantic_rewards: bool,
    next_profile: Instant,
}

/// How often the coordinator's timings are logged and cleared.
pub const PROFILE_PERIOD: Duration = Duration::from_secs(60);

/// The legacy reward event label, value and kind.
fn reward_event(event: &flybrain_gb::adapter::RewardEvent) -> NewEvent {
    let mut new = NewEvent::new(FeedEventKind::Reward, event.label.clone()).value(event.value);
    if let Some(kind) = RewardKind::from_adapter(event.kind) {
        new = new.reward_kind(kind);
    }
    new
}

fn f32_samples(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

impl SessionHost {
    /// `Sim::boot` on the session runtime.
    pub async fn boot(
        shared: Arc<Shared>,
        snapshots: watch::Sender<Arc<Snapshot>>,
        commands: mpsc::Receiver<Command>,
        options: &ServiceOptions,
    ) -> Result<SessionHost> {
        let config = shared.config.clone();
        let started = Instant::now();
        let legacy = legacy_config(&config, options)?;
        let store = store_config(&config);
        let compatibility = crate::composition::compatibility_of(&legacy);

        let rom = std::fs::read(&config.paths.rom)
            .with_context(|| format!("reading ROM {}", config.paths.rom.display()))?;
        let rom_sha256 = sha256_hex(&rom);
        if let Some(expected) = config
            .control
            .expect_rom_sha256
            .as_deref()
            .filter(|expected| *expected != rom_sha256)
        {
            tracing::warn!(
                expected,
                actual = %rom_sha256,
                "FLY_ROM_SHA256 does not match the cartridge on disk"
            );
        }
        let adapter = flybrain_gb::pokemon_red::PokemonRedReward::new();
        let semantic_rewards = adapter.rom_allowed(&rom_sha256);
        if !semantic_rewards {
            tracing::warn!(
                rom = %rom_sha256,
                "this cartridge is not the audited one: semantic rewards are off"
            );
        }
        let profile = legacy.profile.profile();
        let _ = shared.versions.set(Versions {
            kernel: gameboy::KERNEL_VERSION.to_owned(),
            plasticity: gameboy::PLASTICITY_VERSION.to_owned(),
            adapter: adapter.id().to_owned(),
            binjgb: flybrain_gb::compatibility::BINJGB_REVISION.to_owned(),
            dataset: profile.dataset_fingerprint.clone(),
            compatibility: compatibility.clone(),
        });

        // The session's router store and sockets: this process's alone, so whatever a crashed
        // predecessor left there is removed first (the directory itself may be tmpfiles').
        let root = options.session_dir.clone();
        std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        for entry in std::fs::read_dir(&root).with_context(|| format!("reading {}", root.display()))? {
            let path = entry?.path();
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            removed.with_context(|| format!("clearing {}", path.display()))?;
        }

        let log = EventLog::open(&config.paths.save_dir, shared.events.clone())?;
        let (session, boot) = boot_unsaved(&root, legacy.clone(), &store)
            .await
            .map_err(|e| anyhow!("{e}"))?;

        let now = Instant::now();
        let mut host = SessionHost {
            agent_id: legacy.agent_id.clone(),
            limiter: RateLimiter::new(config.control.sugar_per_minute),
            session,
            log,
            journal: SugarJournal::new(&config.paths.hot_dir),
            status: FeedStatus::Booting,
            pending_recovery: false,
            seq: 0,
            started,
            realtime: RealtimeWindow::new(Duration::from_secs(1)),
            next_publish: now,
            pending_saves: Vec::new(),
            sugar_last_by: None,
            sugar_today: 0,
            sugar_day: utc_day(now_wall_ms()),
            remaining_ms: 0.0,
            sugar_serial: 0,
            chat_ring: ChatRing::new(config.chat.ring),
            chat_limits: ChatLimiter::default(),
            deny_list: if config.chat.enabled {
                DenyList::load(config.chat.deny_list.as_deref(), now)
            } else {
                DenyList::empty(now)
            },
            pending_audio: Vec::new(),
            dc_blocker: DcBlocker::default(),
            spikes: Vec::new(),
            last_feed: AgentFeed::default(),
            semantic_rewards,
            next_profile: now + PROFILE_PERIOD,
            shared,
            snapshots,
            commands,
        };

        // The service reads every transition's commit (the spike bitset) and paces itself by
        // flysim's `Pacer`, as the legacy loop does.
        host.session.coordinator.disable_pacing();
        host.session
            .coordinator
            .request_commit_attachments(&[SPIKES_ATTACHMENT]);
        host.session.coordinator.digest_views(false);

        let origin = match &boot {
            Boot::Fresh => {
                tracing::info!("no checkpoint to restore: warmed up a fresh fly");
                host.emit(NewEvent::new(
                    FeedEventKind::System,
                    "Fresh start: the fly woke up",
                ));
                ("fresh start".to_owned(), None)
            }
            Boot::Restored {
                candidate,
                skipped,
                migrated_from,
                imported,
            } => {
                for (refused, reason) in skipped {
                    tracing::error!(
                        origin = %refused.origin,
                        error = %reason,
                        "checkpoint candidate refused"
                    );
                }
                if !skipped.is_empty() {
                    Metrics::set(&host.shared.metrics.restore_fallback, 1);
                    tracing::warn!(
                        origin = %candidate.origin,
                        skipped = skipped.len(),
                        "restored from a fallback candidate"
                    );
                }
                if let Some(from) = migrated_from {
                    tracing::warn!(
                        from = %from,
                        "restoring a checkpoint from an earlier adapter, by the migration \
                         FLY_ACCEPT_ADAPTERS opted this deploy into"
                    );
                }
                tracing::info!(
                    origin = %candidate.origin,
                    frame = host.frame_counter(),
                    boundary = imported.boundary,
                    brain_ms = host.session.task.brain_ms(),
                    rank = host.session.task.rank(),
                    "restored"
                );
                host.log.resume_from(host.session.last_event_id());
                host.emit(NewEvent::new(
                    FeedEventKind::System,
                    format!("Restored from {}", candidate.origin),
                ));
                (candidate.origin.clone(), candidate.generation)
            }
        };

        if config.chat.enabled {
            let restored = host
                .chat_ring
                .load_sidecar(&config.paths.hot_dir, now_wall_ms());
            if restored > 0 {
                tracing::info!(lines = restored, "restored the on-screen chat ring");
            }
        }
        // The pulse the first admission reads, before any commit of this process.
        let feed = host.read_feed().await?;
        host.remaining_ms = feed.reward_remaining_ms;
        host.last_feed = feed;
        host.journal.boot(&BootHeader {
            runtime: RUNTIME.to_owned(),
            start_frame: host.frame_counter(),
            brain_ms: host.session.task.brain_ms(),
            origin: origin.0,
            generation: origin.1,
            compatibility,
            wall_ms: now_wall_ms(),
        });
        host.status = FeedStatus::Running;
        // A durable commit immediately after startup or restore, then both intervals from it.
        host.checkpoint_blocking(SaveKind::Durable).await?;
        host.session
            .checkpointer_mut()
            .expect("attached by boot")
            .start_intervals(Instant::now());
        Ok(host)
    }

    /// The emulator frame counter at the committed boundary (the legacy `frame_counter`): the
    /// boundary's `engineFrame`.
    pub fn frame_counter(&self) -> u64 {
        self.session
            .coordinator
            .observation()
            .and_then(|o| o.engine_frame.as_deref())
            .and_then(|f| f.parse().ok())
            .unwrap_or(0)
    }

    /// The session, for tests and the shadow harness.
    pub fn session(&self) -> &LegacySession {
        &self.session
    }

    fn emit(&mut self, event: NewEvent) -> FeedEvent {
        Metrics::incr(&self.shared.metrics.events_total);
        let ms = self.session.task.brain_ms();
        self.log.append(now_wall_ms(), ms, event)
    }

    // -- the loop --------------------------------------------------------------------------

    /// `Sim::run`: until a shutdown command or a fatal error.
    pub async fn run(&mut self, notifier: &flysim::sdnotify::Notifier) -> Result<()> {
        let (running_period, idle_period) = self.shared.config.publish_periods();
        let mut pacer = Pacer::new(
            flybrain_core::agent::GAMEBOY_MS_PER_FRAME,
            self.shared.config.loop_.speed,
            Instant::now(),
        );
        let mut recovering = false;
        let watchdog_period = notifier.watchdog_interval();
        let mut next_watchdog = Instant::now();

        loop {
            self.shared.beat();
            if let Some(period) = watchdog_period {
                let now = Instant::now();
                if now >= next_watchdog {
                    next_watchdog = now + period;
                    notifier.watchdog();
                }
            }
            if !self.drain_commands().await? {
                self.shutdown().await;
                return Ok(());
            }
            self.reload_deny_list_if_due();
            self.poll_saves();
            self.drain_ended_admissions();

            if self.status == FeedStatus::Paused {
                let now = Instant::now();
                if now >= self.next_publish {
                    self.next_publish = now + idle_period;
                    self.publish(false).await;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }

            let started = Instant::now();
            self.step_frame().await?;
            self.session
                .coordinator
                .metrics
                .record("host.frame", started.elapsed());
            if std::mem::take(&mut self.pending_recovery) {
                recovering = true;
            }

            let now = Instant::now();
            self.realtime.record(now, self.session.task.brain_ms());
            if now >= self.next_publish {
                self.next_publish += running_period;
                if self.next_publish <= now {
                    self.next_publish = now + running_period;
                }
                if recovering {
                    self.status = FeedStatus::Recovering;
                }
                let started = Instant::now();
                self.publish(true).await;
                self.session
                    .coordinator
                    .metrics
                    .record("host.publish", started.elapsed());
                if recovering {
                    self.status = FeedStatus::Running;
                    recovering = false;
                }
            }

            let started = Instant::now();
            self.checkpoint_if_due(now).await?;
            self.session
                .coordinator
                .metrics
                .record("host.checkpoint", started.elapsed());
            self.profile_if_due(now);
            if let Err(error) = self.log.flush() {
                tracing::warn!(%error, "could not fsync the event log");
            }

            Metrics::set(
                &self.shared.metrics.lag_ms,
                (pacer.lag_seconds() * 1000.0) as u64,
            );
            if pacer.should_warn() {
                tracing::warn!(
                    lag_seconds = pacer.lag_seconds(),
                    realtime_factor = self.realtime.factor(),
                    "the simulation is behind real time; no frames are being skipped"
                );
            }
            let sleep = pacer.next_sleep(Instant::now());
            if !sleep.is_zero() {
                tokio::time::sleep(sleep).await;
            }
        }
    }

    /// The coordinator reports every admission applied or aborted; the legacy loop has no abort
    /// (its sugar is applied on the spot), so an aborted one is only logged. Drained every
    /// iteration so the reports do not accumulate.
    fn drain_ended_admissions(&mut self) {
        for (admission, end) in self.session.coordinator.admissions().take_ended() {
            if !matches!(end, fly_session::coordinator::AdmissionEnd::Applied { .. }) {
                tracing::warn!(
                    interaction = %admission.interaction_id(),
                    ?end,
                    "an admitted sugar was not applied"
                );
            }
        }
    }

    fn reload_deny_list_if_due(&mut self) {
        if !self.shared.config.chat.enabled {
            return;
        }
        let forced = self
            .shared
            .chat_reload
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        self.deny_list.maybe_reload(Instant::now(), forced);
    }

    /// One transition and the legacy loop's boundary work, in `Sim::step_frame`'s order.
    async fn step_frame(&mut self) -> Result<()> {
        self.session
            .coordinator
            .step()
            .await
            .map_err(|e| anyhow!("step ({} at {}): {}", e.detail, e.phase, e.error.message))?;
        Metrics::incr(&self.shared.metrics.sim_frames);
        if let Some(details) = self.session.coordinator.take_details() {
            if let Some((_, result)) = details.commits.iter().find(|(id, _)| *id == self.agent_id)
                && let Some(remaining) = result.telemetry.stimulus_remaining_ms
            {
                self.remaining_ms = remaining;
            }
            for (agent, name, bytes) in &details.commit_attachments {
                if *agent == self.agent_id && name == SPIKES_ATTACHMENT {
                    if self.spikes.len() < bytes.len() {
                        self.spikes.resize(bytes.len(), 0);
                    }
                    for (acc, byte) in self.spikes.iter_mut().zip(bytes) {
                        *acc |= byte;
                    }
                }
            }
        }
        // This transition's audio chunk, read before a rollback replaces the boundary's media.
        let audio_name =
            fly_session::media::audio_attachment(fly_session::legacy_env::AUDIO_STREAM_ID);
        if let Some(bytes) = self
            .session
            .coordinator
            .media_bytes(&audio_name)
            .await
            .map_err(|e| anyhow!(e))?
        {
            self.dc_blocker
                .process_f32_into(&f32_samples(&bytes), &mut self.pending_audio);
        }

        // The feed events, in the order the phases produced them: the executor's starts and
        // finishes, the rewards, then the observation's abandonment.
        let feed = self.session.task.take_feed();
        self.emit_macro_events(&feed.executed);
        for event in &feed.rewards {
            self.emit(reward_event(event));
        }
        self.emit_macro_events(&feed.abandoned);

        // `track_rank`: the milestone event, and the archive on a climb.
        let rank = self.session.task.rank();
        let ms = self.session.task.brain_ms();
        let change = self
            .session
            .checkpointer_mut()
            .expect("attached")
            .observe_rank(rank, ms);
        if let Some(change) = change {
            let label = self
                .session
                .task
                .inspect(|adapter, _, _| adapter.progress().rank_label.to_owned());
            let (climbed, rank) = match change {
                RankChange::Climbed(rank) => (true, rank),
                RankChange::FellBack(rank) => (false, rank),
            };
            self.emit(
                NewEvent::new(
                    FeedEventKind::Milestone,
                    format!(
                        "{} {label}",
                        if climbed { "Reached" } else { "Fell back to" }
                    ),
                )
                .value(f64::from(rank)),
            );
            if climbed && let Err(error) = self.checkpoint(SaveKind::Milestone(rank), None).await {
                tracing::error!(%error, "could not archive the milestone checkpoint");
            }
        }

        // `Ready(k+1)`: the ratchet's rollback, captured first (above), then applied.
        let rolled_back = self
            .session
            .coordinator
            .apply_pending_rollback()
            .await
            .map_err(|e| anyhow!("rollback ({} at {}): {}", e.detail, e.phase, e.error.message))?;
        if rolled_back {
            let feed = self.session.task.take_feed();
            let (game_over, events) = feed.rollback.unwrap_or_default();
            self.recovered(game_over, &events).await;
        }
        // The coordinator's trace, audit and timing samples grow with every transition; a
        // service keeps none of them past the boundary (the timings are logged, below).
        self.session.coordinator.trim_records();
        Ok(())
    }

    /// Every [`PROFILE_PERIOD`]: the coordinator's per-phase timings (p50/p95/max, ms) as one
    /// log line, then cleared -- the session runtime's counterpart of the legacy loop's
    /// `FLY_PROFILE_SECONDS` table, always on because it is also what bounds the samples.
    fn profile_if_due(&mut self, now: Instant) {
        if now < self.next_profile {
            return;
        }
        self.next_profile = now + PROFILE_PERIOD;
        let metrics = &mut self.session.coordinator.metrics;
        let mut line = String::new();
        for name in metrics.names() {
            if let Some(p) = metrics.percentiles(&name) {
                line.push_str(&format!(
                    " {name}={}x{:.1}/{:.1}/{:.1}",
                    p.count,
                    p.p50_us() / 1000.0,
                    p.p95_us() / 1000.0,
                    p.max_ns as f64 / 1e6
                ));
            }
        }
        metrics.clear();
        tracing::info!(
            realtime_factor = self.realtime.factor(),
            "session profile (count x p50/p95/max ms):{line}"
        );
        // The measurement spans (`fly_session::profile`, on only when the process started with
        // its switch set): mean per call, so a profile run can see where a frame goes.
        if fly_session::profile::enabled() {
            let mut spans = String::new();
            for (name, count, mean, max) in fly_session::profile::report_with_max() {
                spans.push_str(&format!(
                    " {name}={count}x{:.3}/{:.1}",
                    mean.as_secs_f64() * 1e3,
                    max.as_secs_f64() * 1e3
                ));
            }
            tracing::info!("session spans (count x mean/max ms):{spans}");
        }
    }

    /// `Sim::recovered`: the ticker, the metric, the `recovering` status and a durable save.
    async fn recovered(&mut self, game_over: bool, events: &[flysim::macros::MacroEvent]) {
        let reason = if game_over { "Game over" } else { "Stuck" };
        Metrics::incr(&self.shared.metrics.recoveries_total);
        let (attempts, label) = self.session.task.inspect(|adapter, ratchet, _| {
            (ratchet.state.attempts, adapter.progress().rank_label.to_owned())
        });
        let rank = self.session.checkpointer().expect("attached").rank();
        self.emit(
            NewEvent::new(
                FeedEventKind::Recovery,
                format!("{reason}: rolled back to {label} (attempt {attempts})"),
            )
            .value(f64::from(rank)),
        );
        self.emit_macro_events(events);
        self.pending_recovery = true;
        if let Err(error) = self.checkpoint(SaveKind::Durable, None).await {
            tracing::error!(%error, "could not checkpoint after a recovery");
        }
    }

    fn emit_macro_events(&mut self, events: &[flysim::macros::MacroEvent]) {
        for event in events {
            self.emit(
                NewEvent::new(FeedEventKind::Macro, event.label()).value(f64::from(event.slot)),
            );
        }
    }

    // -- commands --------------------------------------------------------------------------

    /// `Sim::drain_commands`. Returns false when a shutdown was requested.
    async fn drain_commands(&mut self) -> Result<bool> {
        for _ in 0..COMMAND_QUEUE {
            let command = match self.commands.try_recv() {
                Ok(command) => command,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => return Ok(false),
            };
            match command {
                Command::Stimulate {
                    duration_ms,
                    by,
                    source,
                    reply,
                } => {
                    let outcome = self.stimulate(duration_ms, &by, &source);
                    let _ = reply.send(outcome);
                }
                Command::Reward { reply, by, .. } => {
                    // Unreachable in service: the API answers 403 first, because
                    // `service_config` forces `control.allow_reward` off. Dropping the reply
                    // makes the API answer 503 rather than claim a pulse that never happened.
                    tracing::warn!(by, "{}", crate::admission::OPERATOR_REWARD_REFUSED);
                    drop(reply);
                }
                Command::Chat {
                    by,
                    text,
                    bot,
                    reply,
                } => {
                    let outcome = self.chat(&by, &text, bot);
                    let _ = reply.send(outcome);
                }
                Command::Checkpoint { reply } => {
                    match self.checkpoint(SaveKind::Durable, Some(reply)).await {
                        Ok(generation) => tracing::info!(generation, "checkpoint forced"),
                        Err(error) => tracing::error!(%error, "forced checkpoint failed"),
                    }
                }
                Command::Pause { reply } => {
                    if self.status != FeedStatus::Paused {
                        self.status = FeedStatus::Paused;
                        self.emit(NewEvent::new(FeedEventKind::System, "Paused by the operator"));
                        // A pause must never hold a begun save (TASK-01 review R2-1). This host
                        // saves through `queue_save`, which hands its capture over at once, so
                        // there is normally nothing to flush; a begun capture would reach the
                        // writer here rather than sit until the next step.
                        if let Err(error) = self.session.flush_captures().await {
                            tracing::error!(%error, "could not hand over the pending captures on pause");
                        }
                        if let Err(error) = self.checkpoint(SaveKind::Durable, None).await {
                            tracing::error!(%error, "could not checkpoint on pause");
                        }
                        self.publish(false).await;
                    }
                    let _ = reply.send(self.status);
                }
                Command::Resume { reply } => {
                    if self.status == FeedStatus::Paused {
                        self.status = FeedStatus::Running;
                        self.next_publish = Instant::now();
                        self.emit(NewEvent::new(FeedEventKind::System, "Resumed by the operator"));
                    }
                    let _ = reply.send(self.status);
                }
                Command::Shutdown { reply } => {
                    let _ = reply.send(());
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// `POST /stimulate`: flysim's rules over the pulse as it stands, admitted into the next
    /// Prepare (`preStepStimulations`), which is the next frame: where the legacy loop's
    /// `stimulate` lands too.
    fn stimulate(
        &mut self,
        duration_ms: Option<f64>,
        by: &str,
        source: &str,
    ) -> std::result::Result<u64, Refusal> {
        let now_ms = now_wall_ms();
        if let Err(refusal) = self.limiter.admit(now_ms, self.remaining_ms) {
            Metrics::incr(&self.shared.metrics.sugar_refused_total);
            tracing::info!(
                by,
                source,
                retry_after_ms = refusal.retry_after_ms(),
                "sugar refused"
            );
            return Err(refusal);
        }
        let control = &self.shared.config.control;
        let duration = duration_ms
            .unwrap_or(control.sugar_default_ms)
            .clamp(1.0, control.sugar_max_ms)
            .min(control.sugar_max_ms);
        self.session
            .coordinator
            .admissions()
            .admit(fly_session::coordinator::Admission::Stimulus {
                agent_id: self.agent_id.clone(),
                interaction_id: id(&format!("sugar-{}", self.sugar_serial + 1)),
                kind_id: id(gameboy::STIMULUS_REWARD_PULSE),
                duration_ms: duration,
            });
        self.sugar_serial += 1;
        // `network.stimulate` is a maximum.
        self.remaining_ms = self.remaining_ms.max(duration);

        let day = utc_day(now_ms);
        if day != self.sugar_day {
            self.sugar_day = day;
            self.sugar_today = 0;
        }
        self.sugar_today += 1;
        self.sugar_last_by = Some(by.to_string());
        Metrics::incr(&self.shared.metrics.sugar_accepted_total);
        let event = self.emit(
            NewEvent::new(FeedEventKind::Sugar, sugar_label(by))
                .by(by)
                .value(duration),
        );
        self.journal.record(&Entry {
            frame: self.frame_counter(),
            brain_ms: self.session.task.brain_ms(),
            input: Input::Sugar {
                duration_ms: duration,
            },
            by,
            source,
            event_id: event.id,
            wall_ms: event.wall_ms,
        });
        tracing::info!(by, source, duration_ms = duration, "sugar accepted");
        Ok(event.id)
    }

    fn chat(&mut self, by: &str, text: &str, bot: bool) -> std::result::Result<u64, ChatRefusal> {
        let ms = self.session.task.brain_ms();
        let Self {
            shared,
            chat_ring,
            chat_limits,
            deny_list,
            log,
            ..
        } = self;
        admit_chat(
            shared,
            chat_ring,
            chat_limits,
            deny_list,
            by,
            text,
            bot,
            |event| {
                Metrics::incr(&shared.metrics.events_total);
                log.append(now_wall_ms(), ms, event)
            },
        )
    }

    async fn shutdown(&mut self) {
        tracing::info!("shutting down: taking a final durable checkpoint");
        self.emit(NewEvent::new(FeedEventKind::System, "Shutting down"));
        if let Err(error) = self.checkpoint_blocking(SaveKind::Durable).await {
            tracing::error!(%error, "the final checkpoint failed");
        }
        if let Err(error) = self.log.flush() {
            tracing::error!(%error, "the final event log flush failed");
        }
        self.status = FeedStatus::Paused;
        self.publish(false).await;
        if let Some(checkpointer) = self.session.checkpointer_mut() {
            checkpointer.close();
        }
        self.poll_saves();
    }

    /// Stops every worker. After [`SessionHost::run`] returned, or on a fatal error.
    pub async fn stop(self) {
        self.session.stop().await;
    }

    // -- checkpoints -----------------------------------------------------------------------

    async fn checkpoint_if_due(&mut self, now: Instant) -> Result<()> {
        let due = self
            .session
            .checkpointer_mut()
            .expect("attached")
            .due(now);
        if let Some(kind) = due {
            self.checkpoint(kind, None).await.map_err(|e| anyhow!(e))?;
        }
        Ok(())
    }

    /// `Sim::checkpoint_with_reply`: capture the committed boundary now, write it on the
    /// checkpointer's thread, emit the durable save's event. Returns the generation.
    async fn checkpoint(
        &mut self,
        kind: SaveKind,
        reply: Option<oneshot::Sender<std::result::Result<u64, String>>>,
    ) -> std::result::Result<u64, String> {
        // `lastEventId` is the log's watermark before this save's own event.
        self.session
            .set_last_event_id(self.log.next_id().saturating_sub(1));
        let wall_ms = now_wall_ms();
        let ticket = match self.session.queue_save(kind).await {
            Ok(ticket) => ticket,
            Err(error) => {
                Metrics::incr(&self.shared.metrics.checkpoint_failures_total);
                if let Some(reply) = reply {
                    let _ = reply.send(Err(error.clone()));
                }
                return Err(error);
            }
        };
        let generation = ticket.generation;
        if ticket.durable {
            self.emit(
                NewEvent::new(
                    FeedEventKind::Checkpoint,
                    format!("Checkpoint {generation} saved"),
                )
                .value(generation as f64),
            );
        }
        self.pending_saves.push(PendingSave {
            ticket,
            wall_ms,
            reply,
        });
        Ok(generation)
    }

    /// A durable save that has really hit the disk before returning (startup, shutdown).
    async fn checkpoint_blocking(&mut self, kind: SaveKind) -> Result<()> {
        let (tx, mut rx) = oneshot::channel();
        self.checkpoint(kind, Some(tx))
            .await
            .map_err(|e| anyhow!(e))?;
        // The writer answers the ticket; `poll_saves` forwards the answer to `rx`.
        loop {
            self.poll_saves();
            match rx.try_recv() {
                Ok(Ok(_)) => return Ok(()),
                Ok(Err(error)) => bail!(error),
                Err(oneshot::error::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    bail!("the checkpoint writer dropped the reply")
                }
            }
        }
    }

    /// The writer's answers: metrics, `/status`'s checkpoint, and any reply waiting on one.
    fn poll_saves(&mut self) {
        let mut still = Vec::with_capacity(self.pending_saves.len());
        for mut pending in std::mem::take(&mut self.pending_saves) {
            let result: std::result::Result<SaveReport, String> =
                match pending.ticket.reply.try_recv() {
                    Ok(result) => result,
                    Err(oneshot::error::TryRecvError::Empty) => {
                        still.push(pending);
                        continue;
                    }
                    Err(oneshot::error::TryRecvError::Closed) => {
                        Err("the checkpoint writer stopped".to_owned())
                    }
                };
            let metrics = &self.shared.metrics;
            match &result {
                Ok(report) => {
                    Metrics::incr(&metrics.checkpoints_written_total);
                    if report.durable {
                        Metrics::set(&metrics.checkpoint_generation, report.generation);
                        Metrics::set(&metrics.checkpoint_wall_ms, pending.wall_ms);
                        if let Ok(mut status) = self.shared.checkpoint.lock() {
                            status.generation = report.generation;
                            status.latest_wall_ms = pending.wall_ms;
                        }
                    }
                    tracing::debug!(
                        generation = report.generation,
                        durable = report.durable,
                        bytes = report.bytes,
                        "checkpoint committed"
                    );
                }
                Err(error) => {
                    Metrics::incr(&metrics.checkpoint_failures_total);
                    tracing::error!(
                        %error,
                        generation = pending.ticket.generation,
                        durable = pending.ticket.durable,
                        "checkpoint commit failed"
                    );
                }
            }
            if let Some(reply) = pending.reply.take() {
                let _ = reply.send(
                    result
                        .map(|report| report.generation)
                        .map_err(|error| error.to_string()),
                );
            }
        }
        self.pending_saves = still;
    }

    // -- publishing ------------------------------------------------------------------------

    async fn read_feed(&mut self) -> Result<AgentFeed> {
        let agent_id = self.agent_id.clone();
        let value = self
            .session
            .coordinator
            .read_agent_extension(&agent_id, METHOD_FEED_STATUS)
            .await
            .map_err(|e| anyhow!(e))?;
        AgentFeed::parse(&value)
    }

    /// `Sim::publish`: one snapshot of the committed boundary. `with_attachments` is false while
    /// not running, which is the protocol's header-only idle cadence.
    async fn publish(&mut self, with_attachments: bool) {
        match self.read_feed().await {
            Ok(feed) => self.last_feed = feed,
            Err(error) => tracing::warn!(%error, "the agent's feed status: publishing the last one"),
        }
        if let Ok(mut status) = self.shared.decoder.lock() {
            *status = self.last_feed.decoder.clone();
        }
        let feed = self.last_feed.clone();
        let ms = feed.brain_ms;
        let cooldown = self.limiter.cooldown_ms(now_wall_ms());

        let (frame, audio, spikes, spike_count, attachments) = if with_attachments {
            let view = fly_session::media::view_attachment(gameboy::VIEW_ID);
            let frame = match self.session.coordinator.media_bytes(&view).await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => Vec::new(),
                Err(error) => {
                    tracing::warn!(%error, "the committed view");
                    Vec::new()
                }
            };
            let spikes = std::mem::take(&mut self.spikes);
            let count = spikes.iter().map(|b| u64::from(b.count_ones())).sum();
            let audio = f32_bytes(&self.pending_audio);
            self.pending_audio.clear();
            (
                Arc::new(frame),
                Arc::new(audio),
                Arc::new(spikes),
                count,
                AttachmentKind::ALL.to_vec(),
            )
        } else {
            // The spike window restarts at every publish, attachments or not (the legacy
            // `last_publish_ms`).
            self.spikes.clear();
            (
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                0,
                Vec::new(),
            )
        };

        let rank_since_ms = self
            .session
            .checkpointer()
            .map_or(0.0, |checkpointer| checkpointer.rank_since_ms());
        let macro_mode = self.shared.config.macros.mode;
        let semantic_rewards = self.semantic_rewards;
        let (game, milestone) = self.session.task.inspect(|adapter, ratchet, macros| {
            let progress = adapter.progress();
            (
                feed_game(adapter, &progress, semantic_rewards, macros, macro_mode, ms),
                feed_milestone(adapter, &progress, rank_since_ms, ms, ratchet.state.attempts),
            )
        });

        self.seq += 1;
        let header = FeedHeader {
            protocol: PROTOCOL,
            seq: self.seq,
            wall_ms: now_wall_ms(),
            status: self.status,
            realtime_factor: finite(self.realtime.factor()),
            uptime_seconds: self.started.elapsed().as_secs_f64(),
            run_seconds: finite(ms / 1000.0).max(0.0),
            brain_ms: finite(ms).max(0.0),
            frame: self.frame_counter(),
            buttons: self.session.task.mask() & 0xff,
            rates: feed_rates(feed.rates.iter().map(|(name, hz)| (name, *hz))),
            population_rate: finite(feed.population_rate).max(0.0),
            spike_count,
            learning: feed.learning,
            game,
            milestone,
            sugar: feed_sugar(
                feed.reward_remaining_ms,
                cooldown,
                self.sugar_last_by.clone(),
                self.sugar_today,
            ),
            events: self.log.take_pending(),
            chat: if self.shared.config.chat.enabled {
                Some(self.chat_ring.lines())
            } else {
                None
            },
            attachments,
        };
        let snapshot = Snapshot {
            header,
            frame,
            audio,
            spikes,
        };
        Metrics::incr(&self.shared.metrics.snapshots_published);
        let _ = self.snapshots.send(Arc::new(snapshot));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment's native audio through the f32 DC blocker is the legacy loop's u8 audio
    /// through its own, bit for bit, across chunk boundaries.
    #[test]
    fn the_session_audio_path_is_the_legacy_audio_path() {
        let mut raw = Vec::new();
        let mut x: u32 = 12345;
        for _ in 0..4096 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            raw.push((x >> 16) as u8);
        }
        let (mut legacy, mut session) = (DcBlocker::default(), DcBlocker::default());
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for chunk in raw.chunks(1600) {
            legacy.process_into(chunk, &mut a);
            let native = fly_session::legacy_env::audio_f32le(chunk);
            session.process_f32_into(&f32_samples(&native), &mut b);
        }
        assert_eq!(f32_bytes(&a), f32_bytes(&b));
    }

    /// `Legacy.FeedStatus` carries exactly what the legacy `publish` reads from the network.
    #[test]
    fn the_feed_status_is_the_networks_own_numbers() {
        let data = std::sync::Arc::new(
            flybrain_core::dataset::load_brain_dataset_from_dir(
                fly_session::legacy_parity::toy::dir(),
            )
            .expect("the toy connectome"),
        );
        let mut agent = flybrain_core::agent::NeuralAgent::new(
            data,
            flybrain_core::agent::AgentConfig::with_decoder(
                flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros(&[]),
            ),
        )
        .expect("an agent");
        agent.network.step(40);
        agent.network.stimulate(250.0);
        let feed =
            AgentFeed::parse(&fly_session::legacy_agent::feed_status_of(&agent)).expect("parses");
        let stats = agent.network.plasticity.statistics();
        let learning = flysim::simloop::feed_learning(&stats);
        assert_eq!(feed.learning, learning);
        assert_eq!(feed.brain_ms, agent.network.ms);
        assert_eq!(feed.reward_remaining_ms, 250.0);
        assert_eq!(feed.population_rate, agent.network.population_rate);
        let rates = feed_rates(feed.rates.iter().map(|(n, hz)| (n, *hz)));
        assert_eq!(rates, feed_rates(agent.network.rates.iter()));
        assert_eq!(
            feed.decoder.channels.len(),
            agent.decoder.channel_roles().len()
        );
    }
}
