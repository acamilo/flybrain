//! The simulation thread: one thread owning the agent, the emulator, the adapter and the ratchet.
//!
//! Nothing else touches them (`docs/design/flysim.md` section 4). Control commands arrive on a
//! bounded channel and are drained at exactly one point per frame, immediately before the brain
//! step, so command application is deterministic and can be recorded against a frame index.
//! Snapshots leave through a `tokio::sync::watch`, which is drop-oldest by construction: a slow
//! client can never stall the loop.
//!
//! The per-frame order is the prototype worker's (`fly-plays-pokemon/src/simulation.worker.ts`,
//! `tick()`), not `NeuralAgent::tick`'s argument order, because the prototype samples reward
//! inside the same frame it produced:
//!
//! 1. drain commands
//! 2. step the brain 16 or 17 ms (the fractional remainder carries and is checkpointed)
//! 3. decode
//! 4. apply the buttons
//! 5. run one emulator frame
//! 6. set the visual frame from it
//! 7. sample rewards from it, and in macros mode detect the scene and deal its palette
//! 8. stimulate once per reward event
//! 9. reinforce with the summed value
//! 10. ratchet observe, and recover if it says so
//! 11. publish

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use flybrain_core::agent::{AgentConfig, NeuralAgent};
use flybrain_core::dataset::load_brain_dataset_from_dir;
use flybrain_core::decoder::DecoderConfig;
use flybrain_core::decoder::gameboy::{gameboy_decoder_config_with_macros, to_button_mask};
use flybrain_core::decoder::platformer::platformer_decoder_config;
use flybrain_core::lif::SweepPlan;
use flybrain_gb::adapter::{DecoderPresetId, GameAdapter, ProgressSnapshot};
use flybrain_gb::macros::AdapterLedger;
use flybrain_gb::emulator::{DEFAULT_AUDIO_FRAMES, Emulator, FRAMEBUFFER_LEN};
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::recovery::{NeuralRecovery, recover_game};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot, watch};

use crate::chat::{ChatLimiter, ChatRefusal, ChatRing, DenyList, RejectReason};
use crate::config::Config;
use crate::eventlog::{EventLog, EventRing, NewEvent, now_wall_ms, utc_day};
use crate::macros::{MacroEvent, MacroLayer, macro_layer};
use crate::metrics::Metrics;
use crate::pacing::{Pacer, RealtimeWindow};
use crate::profile::{Phase, Profiler};
use crate::ratelimit::{RateLimiter, Refusal};
use crate::snapshot::{
    AttachmentKind, ChatLine, DcBlocker, FeedEvent, FeedEventKind, FeedGame, FeedHeader,
    FeedLearning, FeedMilestone, FeedScene, FeedStatus, FeedSugar, GameMode, MacroMode, PROTOCOL,
    RewardCounts, RewardKind, Snapshot, f32_bytes, finite, spike_bitset,
};
use crate::store::{self, RuntimeState, Store};

/// Commands the control API queues for the sim thread.
#[derive(Debug)]
pub enum Command {
    Stimulate {
        duration_ms: Option<f64>,
        by: String,
        source: String,
        reply: oneshot::Sender<Result<u64, Refusal>>,
    },
    Reward {
        value: f64,
        by: String,
        source: String,
        reply: oneshot::Sender<u64>,
    },
    /// `POST /chat`. Nothing about this command reaches the network, the emulator or the reward
    /// path: it appends one line to the chat ring and one `viewer` event to the log.
    Chat {
        by: String,
        text: String,
        bot: bool,
        reply: oneshot::Sender<Result<u64, ChatRefusal>>,
    },
    Checkpoint {
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Pause {
        reply: oneshot::Sender<FeedStatus>,
    },
    Resume {
        reply: oneshot::Sender<FeedStatus>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

/// Commands queued before the loop starts refusing them.
pub const COMMAND_QUEUE: usize = 64;

/// Version strings `GET /status` reports.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Versions {
    pub kernel: String,
    pub plasticity: String,
    pub adapter: String,
    pub binjgb: String,
    /// Dataset fingerprint.
    pub dataset: String,
    /// The full checkpoint compatibility string, which is not in the TypeScript `FeedVersions`
    /// but is the one string an operator needs when a restore is refused.
    pub compatibility: String,
}

/// `GET /status`'s `checkpoint` object.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointStatus {
    pub latest_wall_ms: u64,
    pub generation: u64,
}

/// What `GET /status` reports under `decoder`: the numbers behind the readout's last decision.
///
/// One entry per channel in the decoder's own order, each with the role it reads, the resting rate
/// that role is normalized against (`baseline`), and the score the last decode computed
/// (`(rate + 1) / (baseline + 1)`, before the group's fatigue division). `pending` names the roles
/// a restored checkpoint carried no baseline for and the next decode will calibrate — empty in
/// steady state, and non-empty for exactly one decode after a restore that added channels.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecoderStatus {
    pub calibrated: bool,
    pub pending: Vec<String>,
    pub channels: Vec<DecoderChannelStatus>,
}

/// One channel's row of [`DecoderStatus`].
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecoderChannelStatus {
    pub channel: String,
    pub role: String,
    pub baseline: f64,
    pub score: f64,
}

/// State the listeners share with the sim thread.
#[derive(Debug)]
pub struct Shared {
    pub config: Config,
    pub metrics: Metrics,
    pub events: EventRing,
    /// `Date.now()` at the top of the most recent loop iteration. `GET /healthz` is 200 while
    /// this is less than two seconds old, which is true while paused as well: a paused loop is
    /// still iterating, and an operator pause must not make the watchdog restart the unit.
    pub heartbeat_ms: AtomicU64,
    pub versions: OnceLock<Versions>,
    pub checkpoint: Mutex<CheckpointStatus>,
    /// The readout's per-channel baseline and score, for `GET /status`.
    ///
    /// Additive and diagnostic: nothing reads it back and no decision depends on it. It exists
    /// because the 2026-09-17 stall was invisible from outside — the macro channels had no baseline
    /// after a restore and competed on raw rate, and `/status` showed a winner without showing the
    /// numbers that produced it (`docs/readout.md`). Published once per snapshot by the loop.
    pub decoder: Mutex<DecoderStatus>,
    /// Set by the SIGHUP task, cleared by the loop: "re-read the chat deny list now". An atomic
    /// rather than a command so a wedged command queue cannot swallow an operator's signal.
    pub chat_reload: std::sync::atomic::AtomicBool,
}

impl Shared {
    pub fn new(config: Config, events: EventRing) -> Self {
        Self {
            config,
            metrics: Metrics::default(),
            events,
            heartbeat_ms: AtomicU64::new(0),
            versions: OnceLock::new(),
            checkpoint: Mutex::new(CheckpointStatus::default()),
            decoder: Mutex::new(DecoderStatus::default()),
            chat_reload: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Ask the loop to re-read the chat deny list at its next iteration (SIGHUP).
    pub fn request_chat_reload(&self) {
        self.chat_reload.store(true, Ordering::Relaxed);
    }

    pub fn beat(&self) {
        self.heartbeat_ms.store(now_wall_ms(), Ordering::Relaxed);
    }

    /// `docs/control-api.md`: "200 when the loop advanced in the last 2 seconds, else 503".
    pub fn healthy(&self, now_ms: u64) -> bool {
        let beat = self.heartbeat_ms.load(Ordering::Relaxed);
        beat != 0 && now_ms.saturating_sub(beat) < 2_000
    }
}

/// A header-only snapshot for the boot window, before the dataset is even loaded.
///
/// `mode` is the configured macro mode. It is a parameter rather than a default because the boot
/// window is tens of seconds long on the release box, and a palette-mode service that published
/// `raw` for all of it would put a wrong MODE chip on screen for the whole of every restart.
pub fn booting_snapshot(seq: u64, wall_ms: u64, mode: MacroMode) -> Snapshot {
    Snapshot {
        header: FeedHeader {
            protocol: PROTOCOL,
            seq,
            wall_ms,
            status: FeedStatus::Booting,
            realtime_factor: 0.0,
            uptime_seconds: 0.0,
            run_seconds: 0.0,
            brain_ms: 0.0,
            frame: 0,
            buttons: 0,
            rates: Map::new(),
            population_rate: 0.0,
            spike_count: 0,
            learning: FeedLearning {
                enabled: true,
                updates: 0,
                changed: 0,
                synapses: 0,
                signal: 0.0,
            },
            game: FeedGame {
                mode: GameMode::Boot,
                semantic_rewards: false,
                map: None,
                badges: 0,
                unique_locations: 0,
                reward_total: 0.0,
                reward_counts: RewardCounts::default(),
                // No frame has been observed yet, so no scene is claimed. The mode is known:
                // it is configuration, not observation.
                scene: FeedScene::Unknown,
                macro_mode: mode,
                palette: Vec::new(),
                running_macro: None,
                macro_outcome: None,
                pad_empty_ms: 0.0,
            },
            milestone: FeedMilestone {
                rank: 0,
                label: "BOOT".to_string(),
                next: String::new(),
                since_seconds: 0.0,
                attempts: 0,
                // The booting header predates the adapter, so it cannot ask one how
                // long its ladder is. One rung is the honest answer: rank 0 of 1.
                total: 1,
            },
            sugar: FeedSugar {
                active: false,
                remaining_ms: 0.0,
                cooldown_ms: 0.0,
                last_by: None,
                today_count: 0,
            },
            events: Vec::new(),
            // The boot window predates the config being read by anything that could publish a
            // ring, so it says nothing about chat either way.
            chat: None,
            attachments: Vec::new(),
        },
        frame: Arc::new(Vec::new()),
        audio: Arc::new(Vec::new()),
        spikes: Arc::new(Vec::new()),
    }
}

/// The neural half of a ratchet recovery, wired to `flybrain-core`.
struct AgentRecovery<'a> {
    agent: &'a mut NeuralAgent,
}

impl NeuralRecovery for AgentRecovery<'_> {
    fn clear_decoder_holds(&mut self) {
        let ms = self.agent.network.ms;
        self.agent.decoder.clear_holds(ms);
    }

    fn clear_eligibility(&mut self) {
        let ms = self.agent.network.ms;
        self.agent.network.plasticity.clear_eligibility(ms);
    }

    fn set_visual_frame(&mut self, frame: &[u8]) {
        let (width, height) = (self.agent.frame.width, self.agent.frame.height);
        self.agent.network.set_visual_frame(frame, width, height);
    }
}

/// A checkpoint write, handed to the writer thread.
///
/// The job carries the *state*, not the encoded envelope: `store::encode` turns about 2.4 MB of
/// arrays into chunk buffers and then concatenates and CRCs them, which measured 8 to 10 ms on the
/// sim thread every `hot_seconds` — a whole dropped frame twice a stream-minute. Cloning the state
/// is the part that has to happen on the sim thread, because it is what makes the snapshot
/// consistent; encoding it does not.
struct WriteJob {
    durable: bool,
    generation: u64,
    agent: flybrain_core::agent::AgentState,
    runtime: RuntimeState,
    archive_rank: Option<u32>,
    wall_ms: u64,
    reply: Option<oneshot::Sender<Result<u64, String>>>,
}

/// Which stimulation pathway an accepted pulse came from, for the event label.
fn sugar_label(by: &str) -> String {
    format!("{by} fed the fly sugar")
}

/// Everything the loop owns.
pub struct Sim {
    shared: Arc<Shared>,
    snapshots: watch::Sender<Arc<Snapshot>>,
    commands: mpsc::Receiver<Command>,

    agent: NeuralAgent,
    emulator: Emulator,
    adapter: Box<dyn GameAdapter>,
    ratchet: Ratchet,
    log: EventLog,

    durable: Store,
    hot: Store,
    writer: Option<std::sync::mpsc::Sender<WriteJob>>,
    writer_thread: Option<std::thread::JoinHandle<()>>,
    next_generation: u64,
    best_archived_rank: Option<u32>,

    /// Fractional millisecond carried into the next frame, exactly as `NeuralAgent` keeps it.
    remainder: f64,
    frame_counter: u64,
    frame_buffer: Vec<u8>,
    pending_audio: Vec<f32>,
    dc_blocker: DcBlocker,
    buttons: u32,
    /// The readout's blocked-direction cooldown (`docs/readout.md`), as the loop computes it: the
    /// player's area and tile as of the last frame the adapter reported one, the channel the group is
    /// holding, and the brain clock at which *either* of those last changed. A direction is only
    /// blamed once it has been held for a whole `blocked_ms` with no movement, so a direction that
    /// has just won is never blamed for a wall the previous one hit.
    ///
    /// All three are transient and deliberately not checkpointed: one hold of a wall after a
    /// restart is cheaper than a stale position surviving a restore.
    location: Option<(u32, u32, u32)>,
    held_channel: Option<String>,
    blocked_since_ms: f64,

    /// Palette mode (`docs/design/macros.md`), or `None` in raw mode — which is the default and
    /// which is byte for byte the behaviour that predates it: every call site below is inside an
    /// `if let Some`, so raw mode runs the same decode, the same mask and the same `set_buttons`.
    ///
    /// Transient by design and not checkpointed (section 8): a restore starts with no macro
    /// running, the scene is detected on the restored frame, and the fly is consulted again.
    macros: Option<MacroLayer>,

    status: FeedStatus,
    /// Set when a recovery happened inside the current frame, so the next snapshot reports
    /// `status: "recovering"` exactly once.
    pending_recovery: bool,
    seq: u64,
    started: Instant,
    rank: u32,
    rank_since_ms: f64,
    realtime: RealtimeWindow,
    /// Absolute deadline for the next snapshot, for the same reason the pacer uses one: a
    /// snapshot can only be published at a frame boundary, and measuring the 33.33 ms period
    /// from the last *actual* publish lets a fraction of a millisecond of jitter push the next
    /// one out by a whole extra frame, which is the difference between 29.9 Hz and 26.9 Hz.
    next_publish: Instant,
    last_publish_ms: f64,
    next_hot: Instant,
    next_durable: Instant,

    limiter: RateLimiter,
    sugar_last_by: Option<String>,
    sugar_today: u64,
    sugar_day: String,

    /// The chat path. None of it is wired to the agent, the emulator or plasticity.
    chat_ring: ChatRing,
    chat_limits: ChatLimiter,
    deny_list: DenyList,

    rom_sha256: String,
    compatibility: String,
    semantic_rewards: bool,
    restored: bool,
    /// Per-phase timing, off unless `FLY_PROFILE_SECONDS` is set (`crate::profile`).
    profiler: Profiler,
}

/// The checkpoint compatibility string, from the pieces that make it up.
///
/// One function, called by `Sim::boot` and by `compatibility_string` below, so the string a
/// deploy-time check prints can never drift from the string the running service writes into a
/// checkpoint. Per-game: the pokered commit for Pokémon (unchanged to the byte, so its
/// checkpoints keep loading), the disassembly plus the pinned ROM hash for the platformer.
fn compatibility_of(agent: &NeuralAgent, adapter: &dyn GameAdapter, fingerprint: &str) -> String {
    flybrain_gb::Compatibility {
        neural_kernel_version: &agent.network.version,
        adapter: adapter.id(),
        dataset_fingerprint: fingerprint,
        plasticity_version: &agent.network.plasticity.version,
        pokered_commit: &adapter.symbol_provenance(),
    }
    .string()
}

/// The decoder configuration for a preset, with the macro group the mode asks for.
///
/// The one place a macro channel enters the readout. In raw mode, and for any game without a
/// palette, `macro_channels` is empty and the preset is the one that has always shipped: no
/// second group, no extra channels in a checkpoint, nothing to mask. In macros mode the group
/// carries one channel per macro type for the whole run (`docs/design/macros.md` section 11), and
/// which of them may win a decision travels per frame as the scene's bound set.
///
/// None of this touches the compatibility string: the readout is not part of the neural
/// checkpoint, and the `macro_<type>` rate roles are outside the dataset fingerprint by
/// construction (`flybrain_core::dataset::merge_macro_roles`).
fn decoder_for(config: &Config, preset: DecoderPresetId) -> DecoderConfig {
    match preset {
        DecoderPresetId::GameBoy => {
            let channels = if config.macros.mode.dealt() {
                flybrain_gb::macro_channels(&config.loop_.game)
            } else {
                Vec::new()
            };
            gameboy_decoder_config_with_macros(&channels)
        }
        DecoderPresetId::Platformer => platformer_decoder_config(),
    }
}

/// The compatibility string this build would write, computed from the config alone.
///
/// `flysim --print-compatibility` is this, and `infra/05-deploy.sh` runs it against the release
/// it is about to install, comparing the answer with the `compatibility` field of the durable
/// state's `manifest.json` BEFORE flipping the `current` symlink. A build whose string differs
/// refuses every checkpoint in that directory and then refuses to start at all rather than run
/// fresh over them, which is correct behaviour and a black stream if it is discovered by
/// deploying it: it cost the 2026-09-16 GPU run a five-minute outage on the live demo, because
/// the release bumped the Pokémon ladder from `pokered-unique8-v3` to `-v4`.
///
/// It loads the dataset and builds the network, because the fingerprint and the kernel and
/// plasticity version strings come from them; that is a second or two. It does NOT read the ROM:
/// the ROM's own hash is not in the string (the platformer's pin comes from config, not from the
/// file), so a deploy check does not need the cartridge to be present.
pub fn compatibility_string(config: &Config) -> Result<String> {
    let dataset = load_brain_dataset_from_dir(&config.paths.dataset)
        .map_err(|error| anyhow!("{error}"))
        .with_context(|| format!("loading dataset {}", config.paths.dataset.display()))?;
    let fingerprint = dataset
        .fingerprint
        .clone()
        .unwrap_or_else(|| "unfingerprinted".to_string());

    let adapter = flybrain_gb::adapter_for_with_rom_pin(&config.loop_.game, config.rom_pin())
        .ok_or_else(|| anyhow!("unknown game {:?}", config.loop_.game))?;
    let decoder = decoder_for(config, adapter.decoder_preset());
    let mut agent_config = AgentConfig::with_decoder(decoder);
    agent_config.warmup_ms = config.loop_.warmup_ms;
    let agent = NeuralAgent::new(Arc::new(dataset), agent_config).map_err(|e| anyhow!("{e}"))?;

    Ok(compatibility_of(&agent, adapter.as_ref(), &fingerprint))
}

impl Sim {
    /// Load everything, restore or warm up, and return a loop ready to run.
    pub fn boot(
        shared: Arc<Shared>,
        snapshots: watch::Sender<Arc<Snapshot>>,
        commands: mpsc::Receiver<Command>,
    ) -> Result<Self> {
        let config = shared.config.clone();
        let started = Instant::now();

        let rom = std::fs::read(&config.paths.rom)
            .with_context(|| format!("reading ROM {}", config.paths.rom.display()))?;
        let dataset = load_brain_dataset_from_dir(&config.paths.dataset)
            .map_err(|error| anyhow!("{error}"))
            .with_context(|| format!("loading dataset {}", config.paths.dataset.display()))?;
        let fingerprint = dataset
            .fingerprint
            .clone()
            .unwrap_or_else(|| "unfingerprinted".to_string());

        let adapter =
            flybrain_gb::adapter_for_with_rom_pin(&config.loop_.game, config.rom_pin())
                .ok_or_else(|| anyhow!("unknown game {:?}", config.loop_.game))?;
        let recovery_policy = adapter.recovery_policy();
        // The adapter picks its readout; the sim loop never names a game.
        let decoder = decoder_for(&config, adapter.decoder_preset());
        // Macros mode takes one decision per the macro group's own hold
        // (`docs/design/macros.md` sections 4 and 12), which is the direction group's hold to the
        // digit; falling back to the direction group covers a preset with no macro group at all.
        let hold_ms = decoder
            .macros
            .as_ref()
            .or(decoder.exclusive.as_ref())
            .map_or(0.0, |group| group.hold_ms);
        let mut agent_config = AgentConfig::with_decoder(decoder);
        agent_config.warmup_ms = config.loop_.warmup_ms;
        let mut agent = NeuralAgent::new(Arc::new(dataset), agent_config)
            .map_err(|error| anyhow!("{error}"))?;
        if config.loop_.threads > 1 {
            agent.set_sweep_plan(
                SweepPlan::with_threads(config.loop_.threads).map_err(|error| anyhow!("{error}"))?,
            );
        }

        let emulator = Emulator::new(&rom, config.feed.audio_hz, DEFAULT_AUDIO_FRAMES)
            .map_err(|error| anyhow!("booting the Game Boy: {error}"))?;
        let rom_sha256 = emulator.rom_sha256();
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
        let semantic_rewards = adapter.rom_allowed(&rom_sha256);
        if !semantic_rewards {
            tracing::warn!(
                rom = %rom_sha256,
                "this cartridge is not the audited one: semantic rewards are off"
            );
        }
        let compatibility = compatibility_of(&agent, adapter.as_ref(), &fingerprint);

        let _ = shared.versions.set(Versions {
            kernel: agent.network.version.clone(),
            plasticity: agent.network.plasticity.version.clone(),
            adapter: adapter.id().to_string(),
            binjgb: flybrain_gb::compatibility::BINJGB_REVISION.to_string(),
            dataset: fingerprint,
            compatibility: compatibility.clone(),
        });

        let durable = Store::new(&config.paths.save_dir, config.loop_.keep_generations);
        let hot = Store::new(&config.paths.hot_dir, config.loop_.keep_generations);
        durable.create()?;
        hot.create()?;
        let log = EventLog::open(&config.paths.save_dir, shared.events.clone())?;

        let now = Instant::now();
        let mut sim = Self {
            limiter: RateLimiter::new(config.control.sugar_per_minute),
            durable,
            hot,
            writer: None,
            writer_thread: None,
            next_generation: 1,
            best_archived_rank: None,
            remainder: 0.0,
            frame_counter: 0,
            frame_buffer: vec![0u8; FRAMEBUFFER_LEN],
            pending_audio: Vec::new(),
            dc_blocker: DcBlocker::default(),
            buttons: 0,
            status: FeedStatus::Booting,
            location: None,
            held_channel: None,
            blocked_since_ms: 0.0,
            pending_recovery: false,
            seq: 0,
            started,
            rank: 0,
            rank_since_ms: 0.0,
            realtime: RealtimeWindow::new(Duration::from_secs(1)),
            next_publish: now,
            last_publish_ms: 0.0,
            next_hot: now,
            next_durable: now,
            sugar_last_by: None,
            sugar_today: 0,
            sugar_day: utc_day(now_wall_ms()),
            chat_ring: ChatRing::new(config.chat.ring),
            chat_limits: ChatLimiter::default(),
            deny_list: if config.chat.enabled {
                DenyList::load(config.chat.deny_list.as_deref(), now)
            } else {
                DenyList::empty(now)
            },
            // Built below, after the restore, because its seed comes from the restored network.
            macros: None,
            rom_sha256,
            compatibility,
            semantic_rewards,
            restored: false,
            profiler: Profiler::from_env(now),
            agent,
            emulator,
            adapter,
            // Recovery limits and triggers are the adapter's, not the loop's.
            ratchet: Ratchet::with_policy(recovery_policy),
            log,
            shared,
            snapshots,
            commands,
        };

        // The kernel's five phases are read off its own lap timer, so it has to be told.
        sim.agent.network.profile = sim.profiler.enabled();
        sim.next_generation = sim.durable.highest_generation().max(sim.hot.highest_generation()) + 1;
        sim.start_writer();
        sim.restore_or_warm_up()?;
        // The on-screen chat ring, from its sidecar beside the hot checkpoints, before the first
        // publish (`docs/control-api.md`, `[chat]`). It is session state and not part of the
        // checkpoint envelope, so it is restored whatever the checkpoints did — including on a
        // fresh start, where the brain is new but the panel's last dozen lines are not stale.
        if config.chat.enabled {
            let restored = sim.chat_ring.load_sidecar(&config.paths.hot_dir, now_wall_ms());
            if restored > 0 {
                tracing::info!(lines = restored, "restored the on-screen chat ring");
            }
        }
        // A dealt mode, seeded from the network as it now stands. No macro has a random
        // component since `GO FRONTIER` replaced `WANDER` (`docs/design/macros.md` section 9), so
        // the seed changes nothing about a run today; taking it here rather than before the
        // restore is what kept a restored run from replaying the last process's flails, and it
        // stays that way for whatever needs a seed next.
        sim.macros = macro_layer(&config, hold_ms, sim.agent.network.rng_state() as u32);
        // One observation before the first frame, so frame one is decided on a real palette
        // rather than on an empty one. After a restore this is the restored frame's scene, which
        // is the whole of what macros mode carries across a restart.
        if let Some(layer) = sim.macros.as_mut() {
            let ledger = AdapterLedger(sim.adapter.as_ref());
            // Nothing can be running before the first frame, so this observation has no events.
            let _ = layer.observe(&mut sim.emulator, &ledger, sim.agent.network.ms);
        }
        // The spike bitset is "who fired since the previous snapshot", and there has not been
        // one. Anchoring the window on the clock as it stands stops the first snapshot reporting
        // the whole warm-up — or, after a restore, every neuron that has ever fired, since the
        // checkpoint's `lastSpikeMs` values are absolute brain milliseconds.
        sim.last_publish_ms = sim.agent.network.ms;
        sim.status = FeedStatus::Running;
        // A durable commit immediately after startup or restore (section 8), so the 300-second
        // interval is a ceiling on routine loss only, and so a broken write path is found now
        // rather than five minutes in.
        sim.checkpoint_blocking(true, None)?;
        // Both intervals run from that commit, not from construction, or the loop's first
        // iteration would immediately write a second one.
        let now = Instant::now();
        sim.next_hot = now + Duration::from_secs_f64(config.loop_.hot_seconds);
        sim.next_durable = now + Duration::from_secs_f64(config.loop_.checkpoint_seconds);
        Ok(sim)
    }

    /// Startup restore: hot latest, hot previous, durable latest, durable previous, then the
    /// milestone archives by descending rank. If candidates exist and every one fails, exit
    /// non-zero rather than silently starting a fresh fly.
    fn restore_or_warm_up(&mut self) -> Result<()> {
        let candidates = store::restore_order(&self.hot, &self.durable);
        if candidates.is_empty() {
            tracing::info!("no checkpoint to restore: warming up a fresh fly");
            self.fresh_start()?;
            return Ok(());
        }

        let total = candidates.len();
        for (index, candidate) in candidates.into_iter().enumerate() {
            match self.try_restore(&candidate) {
                Ok(()) => {
                    if index > 0 {
                        Metrics::set(&self.shared.metrics.restore_fallback, 1);
                        tracing::warn!(
                            origin = %candidate.origin,
                            skipped = index,
                            "restored from a fallback candidate"
                        );
                    }
                    tracing::info!(
                        origin = %candidate.origin,
                        frame = self.frame_counter,
                        brain_ms = self.agent.network.ms,
                        rank = self.rank,
                        "restored"
                    );
                    self.restored = true;
                    self.emit(NewEvent::new(
                        FeedEventKind::System,
                        format!("Restored from {}", candidate.origin),
                    ));
                    return Ok(());
                }
                Err(error) => tracing::error!(
                    origin = %candidate.origin,
                    %error,
                    "checkpoint candidate refused"
                ),
            }
        }
        bail!(
            "every one of the {total} checkpoint candidates failed to load; refusing to start a \
             fresh run over them. Move {} aside by hand to reset deliberately.",
            self.durable.dir().display()
        )
    }

    fn try_restore(&mut self, candidate: &store::Candidate) -> Result<()> {
        let checkpoint = store::load(&candidate.path)?;
        let runtime = &checkpoint.runtime;
        if runtime.rom_sha256 != self.rom_sha256 {
            bail!("checkpoint is for another cartridge ({})", runtime.rom_sha256);
        }
        if runtime.compatibility != self.compatibility {
            bail!(
                "compatibility mismatch\n  checkpoint: {}\n  this build: {}",
                runtime.compatibility,
                self.compatibility
            );
        }
        if runtime.framebuffer.len() != FRAMEBUFFER_LEN {
            bail!("checkpoint framebuffer is {} bytes", runtime.framebuffer.len());
        }
        // `import_state` is self-validating and a no-op on failure, so a refused checkpoint
        // leaves the agent exactly as it was and the next candidate starts clean.
        self.agent
            .import_state(&checkpoint.agent)
            .map_err(|error| anyhow!("{error}"))?;
        self.emulator
            .import_state(&runtime.emulator)
            .map_err(|error| anyhow!("{error}"))?;
        if !runtime.reward.is_null() {
            self.adapter
                .import_state(&runtime.reward)
                .map_err(|error| anyhow!("{error}"))?;
        }
        let snapshot = if runtime.ratchet_game.is_empty() {
            None
        } else {
            Some(flybrain_gb::ratchet::Snapshot {
                game: runtime.ratchet_game.clone(),
                frame: runtime.ratchet_frame.clone(),
            })
        };
        self.ratchet
            .import(Some(runtime.ratchet), snapshot, self.adapter.rank_ladder().len())
            .map_err(|error| anyhow!("{error}"))?;

        self.remainder = checkpoint.agent.remainder;
        self.frame_counter = runtime.emulator_frame;
        self.buttons = runtime.buttons;
        self.rank_since_ms = runtime.rank_since_ms;
        self.frame_buffer.copy_from_slice(&runtime.framebuffer);
        let (width, height) = (self.agent.frame.width, self.agent.frame.height);
        self.agent
            .network
            .set_visual_frame(&self.frame_buffer, width, height);
        self.rank = self.adapter.progress().rank;
        self.log.resume_from(runtime.last_event_id);
        self.emulator.set_buttons(self.buttons as u8);
        Ok(())
    }

    /// Warm up on a fresh start only: 2,500 ms with plasticity disabled, then calibrate the
    /// readout on the settled rates. Never after a restore, which already carries a settled
    /// network and a calibrated decoder.
    fn fresh_start(&mut self) -> Result<()> {
        self.emulator
            .run_frame()
            .map_err(|error| anyhow!("running the first frame: {error}"))?;
        self.frame_buffer.copy_from_slice(self.emulator.framebuffer());
        self.frame_counter = 1;
        let raw = self.emulator.take_audio_u8();
        self.dc_blocker.process_into(&raw, &mut self.pending_audio);
        self.agent
            .warmup(Some(&self.frame_buffer))
            .map_err(|error| anyhow!("{error}"))?;
        self.emit(NewEvent::new(FeedEventKind::System, "Fresh start: the fly woke up"));
        Ok(())
    }

    // -- the loop ------------------------------------------------------------------------

    /// Run until a shutdown command or a fatal error.
    ///
    /// `WATCHDOG=1` is sent from here rather than from a timer task on purpose: a ping sent by a
    /// separate task would keep the unit alive while this loop was wedged, which is the one
    /// failure `WatchdogSec=30` exists to catch.
    pub fn run(&mut self, notifier: &crate::sdnotify::Notifier) -> Result<()> {
        let (running_period, idle_period) = self.shared.config.publish_periods();
        let mut pacer = Pacer::new(
            self.agent.ms_per_frame,
            self.shared.config.loop_.speed,
            Instant::now(),
        );
        let mut recovering = false;
        let watchdog_period = notifier.watchdog_interval();
        let mut next_watchdog = Instant::now();

        loop {
            let iteration = Instant::now();
            self.shared.beat();
            if let Some(period) = watchdog_period {
                let now = Instant::now();
                if now >= next_watchdog {
                    next_watchdog = now + period;
                    notifier.watchdog();
                }
            }
            self.profiler.start();
            if !self.drain_commands()? {
                self.shutdown();
                return Ok(());
            }
            self.reload_deny_list_if_due();
            self.profiler.lap(Phase::Commands);

            if self.status == FeedStatus::Paused {
                // Header-only at 2 Hz while paused, which is also what keeps `/status` fresh and
                // the heartbeat honest: a paused loop is still iterating.
                let now = Instant::now();
                if now >= self.next_publish {
                    self.next_publish = now + idle_period;
                    self.publish(false);
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }

            self.step_frame()?;
            if std::mem::take(&mut self.pending_recovery) {
                recovering = true;
            }

            let now = Instant::now();
            self.realtime.record(now, self.agent.network.ms);
            if now >= self.next_publish {
                self.next_publish += running_period;
                if self.next_publish <= now {
                    // More than one period late (a long frame, or unthrottled): re-anchor
                    // rather than publishing a burst to catch up.
                    self.next_publish = now + running_period;
                }
                if recovering {
                    self.status = FeedStatus::Recovering;
                }
                self.publish(true);
                if recovering {
                    self.status = FeedStatus::Running;
                    recovering = false;
                }
            }
            // Lapped outside the branch, so the frames that do not publish charge their share
            // of nothing rather than charging the gap to the next phase.
            self.profiler.lap(Phase::Publish);

            self.checkpoint_if_due(now)?;
            self.profiler.lap(Phase::Checkpoint);
            if let Err(error) = self.log.flush() {
                tracing::warn!(%error, "could not fsync the event log");
            }
            self.profiler.lap(Phase::Log);

            Metrics::set(&self.shared.metrics.lag_ms, (pacer.lag_seconds() * 1000.0) as u64);
            if pacer.should_warn() {
                tracing::warn!(
                    lag_seconds = pacer.lag_seconds(),
                    realtime_factor = self.realtime.factor(),
                    "the simulation is behind real time; no frames are being skipped"
                );
            }
            let sleep = pacer.next_sleep(Instant::now());
            if !sleep.is_zero() {
                std::thread::sleep(sleep);
            }
            self.profiler.iteration(iteration.elapsed());
            if self.profiler.due(Instant::now()) {
                let table = self.profiler.report(Instant::now(), self.realtime.factor());
                tracing::info!("per-phase profile\n{table}");
            }
        }
    }

    /// Re-read the chat deny list when SIGHUP asked or the interval elapsed.
    ///
    /// Called once per loop iteration: the file read only actually happens on a SIGHUP or once a
    /// minute (`DenyList::RELOAD_INTERVAL`), so this is an `Instant` comparison per frame.
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

    /// One frame, in the prototype's order.
    fn step_frame(&mut self) -> Result<()> {
        // 2. Step the brain: 16 or 17 integer ticks, the remainder carried and checkpointed.
        self.remainder += self.agent.ms_per_frame;
        let steps = self.remainder.floor();
        self.remainder -= steps;
        self.agent.network.step(steps as u64);
        self.profiler.lap(Phase::Step);
        if self.profiler.enabled() {
            self.profiler.absorb_brain(self.agent.network.timings());
            self.agent.network.reset_timings();
        }

        // 3. Decode, 4. apply the buttons.
        // Not the mode string: the adapter decides what counts as boot, because a platformer needs
        // the permissive Start variant in four of its five modes (`GameAdapter::boot`).
        let boot = self.adapter.boot();
        let ms = self.agent.network.ms;
        let rates = self.agent.network.rates.clone();
        // The readout's blocked-direction cooldown (`docs/readout.md`): the direction the group is
        // holding, once the adapter's position has stood still for a whole `blocked_ms`. The loop
        // owns the clock and the position; the decoder only learns *which* channel did nothing.
        // `blocked_ms == 0` -- the platformer preset, and the Game Boy preset before v0.1.1 --
        // switches the rule off here, before the decoder is asked.
        let blocked_ms = self.agent.decoder.blocked_ms();
        let blocked = (blocked_ms > 0.0 && ms - self.blocked_since_ms >= blocked_ms)
            .then(|| self.agent.decoder.current())
            .flatten()
            .map(str::to_string);
        // The scene's own macro buttons, for the macro group's per-decision mask
        // (`docs/design/macros.md` section 12: "unbound channels are masked from the decision").
        // They are the bindings the previous frame's `observe` dealt, which is the palette the
        // page is showing, so the fly is choosing among exactly the buttons the audience can see.
        // `None` in raw mode, where the group has no channels to mask anyway.
        let bound = self.macros.as_ref().map(MacroLayer::bound_channels);
        let active = self.agent.decoder.decode_bound(
            &rates,
            ms,
            boot,
            blocked.as_deref(),
            bound.as_deref(),
        );
        // A new winner starts its own window: it has not had a hold to move in yet.
        let held = self.agent.decoder.current().map(str::to_string);
        if held != self.held_channel {
            self.held_channel = held;
            self.blocked_since_ms = ms;
        }
        self.buttons = to_button_mask(&active);
        // Macros mode (`docs/design/macros.md` sections 4 and 12): the same decode, plus the
        // macro group whose winner is in `active` alongside the buttons. The mask that reaches
        // the emulator is the running macro's, or nothing, or -- on the title screen alone -- the
        // raw mask above. In raw mode `self.macros` is `None` and not one line of this runs.
        let started_or_finished = match self.macros.as_mut() {
            // Two disjoint fields of the same struct, so the layer can read the emulator while
            // the loop still owns both. Taking the layer out and putting it back would leave
            // raw mode running silently if anything in between ever panicked.
            Some(layer) => {
                // Three disjoint fields: the layer decides, the emulator is read, and the
                // adapter's exploration ledger answers the ways out' "unvisited" — read-only, by
                // `&dyn`, and the only thing the palette is told about the reward side.
                let ledger = AdapterLedger(self.adapter.as_ref());
                let decision = layer.decide(
                    &active,
                    self.buttons,
                    ms,
                    &mut self.emulator,
                    &ledger,
                );
                self.buttons = decision.mask;
                decision.events
            }
            None => Vec::new(),
        };
        self.emit_macro_events(&started_or_finished);
        self.emulator.set_buttons(self.buttons as u8);
        self.profiler.lap(Phase::Decode);

        // 5. Run one emulator frame, 6. set the visual frame from it.
        self.emulator
            .run_frame()
            .map_err(|error| anyhow!("frame {}: {error}", self.frame_counter + 1))?;
        self.frame_counter += 1;
        Metrics::incr(&self.shared.metrics.sim_frames);
        self.profiler.lap(Phase::Emulate);
        self.frame_buffer.copy_from_slice(self.emulator.framebuffer());
        let (width, height) = (self.agent.frame.width, self.agent.frame.height);
        self.agent
            .network
            .set_visual_frame(&self.frame_buffer, width, height);
        let raw = self.emulator.take_audio_u8();
        self.dc_blocker.process_into(&raw, &mut self.pending_audio);
        self.profiler.lap(Phase::Retina);

        // 7. Sample rewards from the frame just produced.
        let ms = self.agent.network.ms;
        let events = {
            let (adapter, emulator) = (&mut self.adapter, &mut self.emulator);
            adapter.sample(emulator, ms)
        };

        // 8. Stimulate once per event, 9. reinforce with the sum.
        let mut total = 0.0;
        for event in &events {
            self.agent.network.stimulate(f64::from(event.stimulation_ms));
            total += event.value;
        }
        if self.agent.network.plasticity.enabled {
            self.agent.network.plasticity.reinforce(total, ms);
        }
        for event in &events {
            let kind = RewardKind::from_adapter(event.kind);
            let mut new = NewEvent::new(FeedEventKind::Reward, event.label.clone())
                .value(event.value);
            if let Some(kind) = kind {
                new = new.reward_kind(kind);
            }
            self.emit(new);
        }

        self.profiler.lap(Phase::Rewards);

        // `docs/design/macros.md` section 2: the scene is sampled once per game frame, after the
        // frame. So the palette the fly is offered on the next frame is the one for the frame it
        // can actually see, and the feed's `game.scene` is never a frame ahead of the screen.
        let abandoned = match self.macros.as_mut() {
            Some(layer) => {
                let ledger = AdapterLedger(self.adapter.as_ref());
                layer.observe(&mut self.emulator, &ledger, ms)
            }
            None => Vec::new(),
        };
        // At most one: a macro that has run into a scene with no palette, abandoned before the
        // header this frame publishes can show it beside that scene.
        self.emit_macro_events(&abandoned);

        // The cooldown's other reset: the player actually moved. `None` -- a battle, a script, a
        // map transition -- is no information rather than "still", so the rule cannot fire while
        // the fly has no control anyway.
        let location = self.adapter.location();
        if location.is_some() && location != self.location {
            self.location = location;
            self.blocked_since_ms = ms;
        }

        // 10. Ratchet: observe, and recover if it says so.
        let progress = self.adapter.progress();
        self.track_rank(&progress, ms);
        let safe = self.adapter.safe_for_snapshot();
        let capture_due = safe && u64::from(progress.rank) > self.ratchet.state.best;
        let captured = if capture_due {
            Some(flybrain_gb::ratchet::Snapshot {
                game: self
                    .emulator
                    .export_state()
                    .map_err(|error| anyhow!("capturing a ratchet snapshot: {error}"))?,
                frame: self.frame_buffer.clone(),
            })
        } else {
            None
        };
        // The stall window's second progress signal (`docs/design/ladder.md`, the 2026-09-17
        // rule as amended 2026-09-22): coverage is ground never stood on, and a fly crossing a
        // town it has already covered to reach the rung's own door earns none of it while it is
        // plainly getting somewhere. The macro layer answers with the map graph it already walks
        // (`docs/design/macros.md` section 12.15); in raw mode there is no layer and no
        // objective, and the answer is false.
        let nearer = self.macros.as_ref().is_some_and(MacroLayer::nearer_the_objective);
        let recover = self.ratchet.observe_with_progress(
            safe,
            u64::from(progress.rank),
            progress.unique_locations as u64,
            ms as u64,
            self.adapter.game_over(),
            nearer,
            || captured.expect("the ratchet only captures when a snapshot was prepared"),
        );
        if recover {
            // Two triggers, two stories on the ticker: a game over ended the run, a stall did not.
            let reason = if self.adapter.game_over() { "Game over" } else { "Stuck" };
            self.recover(reason)?;
        }
        self.profiler.lap(Phase::Ratchet);
        Ok(())
    }

    fn track_rank(&mut self, progress: &ProgressSnapshot, ms: f64) {
        if progress.rank == self.rank {
            return;
        }
        let climbed = progress.rank > self.rank;
        self.rank = progress.rank;
        self.rank_since_ms = ms;
        self.emit(
            NewEvent::new(
                FeedEventKind::Milestone,
                format!(
                    "{} {}",
                    if climbed { "Reached" } else { "Fell back to" },
                    progress.rank_label
                ),
            )
            .value(f64::from(progress.rank)),
        );
        if climbed {
            // A new best rank earns a permanent archive, taken now rather than at the next
            // interval: the point of the archive is that this exact moment is recoverable.
            if let Err(error) = self.checkpoint(true, Some(progress.rank)) {
                tracing::error!(%error, "could not archive the milestone checkpoint");
            }
        }
    }

    fn recover(&mut self, reason: &str) -> Result<()> {
        let snapshot = flybrain_gb::ratchet::Snapshot {
            game: self
                .ratchet
                .game()
                .ok_or_else(|| anyhow!("the ratchet asked to recover with no snapshot"))?
                .to_vec(),
            frame: self
                .ratchet
                .frame()
                .ok_or_else(|| anyhow!("the ratchet snapshot has no framebuffer"))?
                .to_vec(),
        };
        let frame = {
            let mut neural = AgentRecovery { agent: &mut self.agent };
            recover_game(
                &mut self.emulator,
                self.adapter.as_mut(),
                &mut neural,
                &snapshot,
            )
            .map_err(|error| anyhow!("recovering the game: {error}"))?
        };
        self.frame_buffer.copy_from_slice(&frame);
        self.buttons = 0;
        self.emulator.set_buttons(0);
        Metrics::incr(&self.shared.metrics.recoveries_total);
        let attempts = self.ratchet.state.attempts;
        self.emit(
            NewEvent::new(
                FeedEventKind::Recovery,
                format!(
                    "{reason}: rolled back to {} (attempt {attempts})",
                    self.adapter.progress().rank_label
                ),
            )
            .value(f64::from(self.rank)),
        );
        self.location = self.adapter.location();
        self.held_channel = None;
        self.blocked_since_ms = self.agent.network.ms;
        // A rollback restores a game the running macro's plan was never made for, so the macro is
        // abandoned here rather than carried over a map change it cannot see.
        let abandoned = match self.macros.as_mut() {
            Some(layer) => {
                let events = layer.cancel(self.agent.network.ms);
                // The frame's `observe` ran before the ratchet decided to roll back, so the scene
                // and the palette describe the run that was just thrown away. The restored game
                // is in WRAM now, so re-detect here rather than let the next frame's decision be
                // made against a map the fly is no longer standing on.
                let ledger = AdapterLedger(self.adapter.as_ref());
                let mut events = events;
                events.extend(layer.observe(&mut self.emulator, &ledger, self.agent.network.ms));
                events
            }
            None => Vec::new(),
        };
        self.emit_macro_events(&abandoned);
        self.pending_recovery = true;
        if let Err(error) = self.checkpoint(true, None) {
            tracing::error!(%error, "could not checkpoint after a recovery");
        }
        Ok(())
    }

    // -- commands ------------------------------------------------------------------------

    /// Drain the command queue. Returns false when a shutdown was requested.
    fn drain_commands(&mut self) -> Result<bool> {
        for _ in 0..COMMAND_QUEUE {
            let command = match self.commands.try_recv() {
                Ok(command) => command,
                Err(mpsc::error::TryRecvError::Empty) => break,
                // Every sender is gone: the listeners have shut down, so should the loop.
                Err(mpsc::error::TryRecvError::Disconnected) => return Ok(false),
            };
            match command {
                Command::Stimulate { duration_ms, by, source, reply } => {
                    let outcome = self.stimulate(duration_ms, &by, &source);
                    let _ = reply.send(outcome);
                }
                Command::Reward { value, by, source, reply } => {
                    let id = self.reward(value, &by, &source);
                    let _ = reply.send(id);
                }
                Command::Chat { by, text, bot, reply } => {
                    let outcome = self.chat(&by, &text, bot);
                    let _ = reply.send(outcome);
                }
                Command::Checkpoint { reply } => {
                    let generation = self.next_generation;
                    match self.checkpoint_with_reply(true, None, Some(reply)) {
                        Ok(()) => tracing::info!(generation, "checkpoint forced"),
                        Err(error) => tracing::error!(%error, "forced checkpoint failed"),
                    }
                }
                Command::Pause { reply } => {
                    if self.status != FeedStatus::Paused {
                        self.status = FeedStatus::Paused;
                        self.emit(NewEvent::new(FeedEventKind::System, "Paused by the operator"));
                        // A pause takes an implicit durable checkpoint (section 7).
                        if let Err(error) = self.checkpoint(true, None) {
                            tracing::error!(%error, "could not checkpoint on pause");
                        }
                        self.publish(false);
                    }
                    let _ = reply.send(self.status);
                }
                Command::Resume { reply } => {
                    if self.status == FeedStatus::Paused {
                        self.status = FeedStatus::Running;
                        // The idle deadline is up to half a second out; a resumed stream should
                        // not wait for it.
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

    /// `POST /stimulate`: the sugar path. Admission is decided here, on the sim thread, because
    /// the "no overlap with an active pulse" rule reads the network's own remaining pulse.
    fn stimulate(
        &mut self,
        duration_ms: Option<f64>,
        by: &str,
        source: &str,
    ) -> Result<u64, Refusal> {
        let now_ms = now_wall_ms();
        let remaining = self.agent.network.reward_remaining();
        if let Err(refusal) = self.limiter.admit(now_ms, remaining) {
            Metrics::incr(&self.shared.metrics.sugar_refused_total);
            tracing::info!(
                by,
                source,
                retry_after_ms = refusal.retry_after_ms(),
                "sugar refused"
            );
            return Err(refusal);
        }
        let requested = duration_ms.unwrap_or(self.shared.config.control.sugar_default_ms);
        let duration = requested
            .clamp(1.0, self.shared.config.control.sugar_max_ms)
            .min(self.shared.config.control.sugar_max_ms);
        self.agent.network.stimulate(duration);

        let day = utc_day(now_ms);
        if day != self.sugar_day {
            self.sugar_day = day;
            self.sugar_today = 0;
        }
        self.sugar_today += 1;
        self.sugar_last_by = Some(by.to_string());
        Metrics::incr(&self.shared.metrics.sugar_accepted_total);
        let event = self.emit(NewEvent::new(FeedEventKind::Sugar, sugar_label(by))
            .by(by)
            .value(duration));
        tracing::info!(by, source, duration_ms = duration, "sugar accepted");
        Ok(event.id)
    }

    /// `POST /reward`: a direct reinforcement pulse. The API refuses this with 403 unless
    /// `control.allow_reward` is on, so reaching here means an operator turned it on.
    fn reward(&mut self, value: f64, by: &str, source: &str) -> u64 {
        let ms = self.agent.network.ms;
        self.agent.network.plasticity.reinforce(value, ms);
        tracing::info!(by, source, value, "reward pulse applied");
        self.emit(
            NewEvent::new(FeedEventKind::Reward, format!("{by} sent a reward pulse ({value})"))
                .by(by)
                .value(value),
        )
        .id
    }

    /// `POST /chat`: the on-screen chat path, enforced here rather than trusted from the bridge.
    ///
    /// Name, sanitizer, deny list, then the admission limits, in that order — a line that a rule
    /// refuses must not spend anyone's rate budget. On acceptance the line joins the ring and one
    /// `viewer` event labelled `chat` joins the event log; the event carries the name only, so no
    /// chat text is ever written to `events.jsonl`.
    ///
    /// Nothing here touches the network, the emulator, the decoder or plasticity. That is the
    /// whole point: chat is a caption track, not an input.
    fn chat(&mut self, by: &str, text: &str, bot: bool) -> Result<u64, ChatRefusal> {
        let refuse = |sim: &Self, refusal: ChatRefusal| {
            sim.shared.metrics.chat_rejected(refusal.reason());
            refusal
        };

        if !crate::chat::is_valid_display_name(by) {
            return Err(refuse(self, ChatRefusal::Rejected(RejectReason::Name)));
        }
        let text = match crate::chat::sanitize_chat_text(text) {
            Ok(text) => text,
            Err(reason) => return Err(refuse(self, ChatRefusal::Rejected(reason))),
        };
        if self.deny_list.blocks(by, &text) {
            tracing::info!(by, "chat line refused by the deny list");
            return Err(refuse(self, ChatRefusal::Rejected(RejectReason::DenyList)));
        }

        let now_ms = now_wall_ms();
        if let Err(refusal) = self.chat_limits.admit(by, now_ms) {
            return Err(refuse(self, refusal));
        }

        let event = self.emit(NewEvent::new(FeedEventKind::Viewer, "chat").by(by));
        self.chat_ring.push(ChatLine {
            id: event.id,
            wall_ms: event.wall_ms,
            by: by.to_string(),
            text,
            bot: if bot { Some(true) } else { None },
        });
        // The ring survives a restart because it is written here, not because it is in a
        // checkpoint: one atomic rename onto tmpfs per accepted line, and a failure is a warning
        // rather than a refusal — the line is already on screen.
        if let Err(error) = self.chat_ring.save_sidecar(&self.shared.config.paths.hot_dir) {
            tracing::warn!(%error, "could not persist the chat ring; it will not survive a restart");
        }
        Metrics::incr(&self.shared.metrics.chat_accepted_total);
        Ok(event.id)
    }

    /// One `macro` feed event per start and per finish (`docs/design/macros.md` section 5).
    ///
    /// `value` carries the slot, the way a `milestone` event carries its rank: the ticker wants
    /// the label and the stage wants to know which cell lit up.
    fn emit_macro_events(&mut self, events: &[MacroEvent]) {
        for event in events {
            self.emit(
                NewEvent::new(FeedEventKind::Macro, event.label()).value(f64::from(event.slot)),
            );
        }
    }

    fn shutdown(&mut self) {
        tracing::info!("shutting down: taking a final durable checkpoint");
        self.emit(NewEvent::new(FeedEventKind::System, "Shutting down"));
        if let Err(error) = self.checkpoint_blocking(true, None) {
            tracing::error!(%error, "the final checkpoint failed");
        }
        if let Err(error) = self.log.flush() {
            tracing::error!(%error, "the final event log flush failed");
        }
        self.status = FeedStatus::Paused;
        self.publish(false);
        self.stop_writer();
    }

    // -- checkpoints ---------------------------------------------------------------------

    fn start_writer(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel::<WriteJob>();
        let shared = Arc::clone(&self.shared);
        let durable = self.durable.clone();
        let hot = self.hot.clone();
        let handle = std::thread::Builder::new()
            .name("flysim-store".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let store = if job.durable { &durable } else { &hot };
                    let result = store::encode(&job.agent, &job.runtime)
                        .and_then(|bytes| {
                            store
                                .commit(job.generation, &bytes, job.archive_rank)
                                .map(|manifest| (manifest, bytes.len()))
                        });
                    match &result {
                        Ok((_, bytes)) => {
                            Metrics::incr(&shared.metrics.checkpoints_written_total);
                            if job.durable {
                                Metrics::set(
                                    &shared.metrics.checkpoint_generation,
                                    job.generation,
                                );
                                Metrics::set(&shared.metrics.checkpoint_wall_ms, job.wall_ms);
                                if let Ok(mut status) = shared.checkpoint.lock() {
                                    status.generation = job.generation;
                                    status.latest_wall_ms = job.wall_ms;
                                }
                            }
                            tracing::debug!(
                                generation = job.generation,
                                durable = job.durable,
                                bytes,
                                "checkpoint committed"
                            );
                        }
                        Err(error) => {
                            Metrics::incr(&shared.metrics.checkpoint_failures_total);
                            tracing::error!(
                                %error,
                                generation = job.generation,
                                durable = job.durable,
                                "checkpoint commit failed"
                            );
                        }
                    }
                    if let Some(reply) = job.reply {
                        let _ = reply.send(
                            result
                                .map(|_| job.generation)
                                .map_err(|error| format!("{error:#}")),
                        );
                    }
                }
            })
            .expect("spawning the checkpoint writer");
        self.writer = Some(tx);
        self.writer_thread = Some(handle);
    }

    fn stop_writer(&mut self) {
        self.writer = None;
        if let Some(handle) = self.writer_thread.take() {
            let _ = handle.join();
        }
    }

    fn checkpoint_if_due(&mut self, now: Instant) -> Result<()> {
        let config = &self.shared.config.loop_;
        let hot_period = Duration::from_secs_f64(config.hot_seconds);
        let durable_period = Duration::from_secs_f64(config.checkpoint_seconds);
        if now >= self.next_durable {
            self.next_durable = now + durable_period;
            self.next_hot = now + hot_period;
            self.checkpoint(true, None)?;
        } else if now >= self.next_hot {
            self.next_hot = now + hot_period;
            self.checkpoint(false, None)?;
        }
        Ok(())
    }

    fn checkpoint(&mut self, durable: bool, archive_rank: Option<u32>) -> Result<()> {
        self.checkpoint_with_reply(durable, archive_rank, None)
    }

    /// Serialize on the sim thread (so the snapshot is consistent) and let the writer thread do
    /// the write and the fsyncs.
    fn checkpoint_with_reply(
        &mut self,
        durable: bool,
        archive_rank: Option<u32>,
        reply: Option<oneshot::Sender<Result<u64, String>>>,
    ) -> Result<()> {
        let archive_rank = archive_rank.filter(|rank| {
            durable && self.best_archived_rank.is_none_or(|best| *rank > best)
        });
        let (generation, agent, runtime) = self.snapshot_state()?;
        if let Some(rank) = archive_rank {
            self.best_archived_rank = Some(rank);
        }
        let job = WriteJob {
            durable,
            generation,
            agent,
            runtime,
            archive_rank,
            wall_ms: now_wall_ms(),
            reply,
        };
        let Some(writer) = &self.writer else {
            bail!("the checkpoint writer is gone");
        };
        writer.send(job).map_err(|_| anyhow!("the checkpoint writer stopped"))?;
        if durable {
            self.emit(
                NewEvent::new(
                    FeedEventKind::Checkpoint,
                    format!("Checkpoint {generation} saved"),
                )
                .value(generation as f64),
            );
        }
        Ok(())
    }

    /// A durable checkpoint that has really hit the disk before returning, for shutdown.
    fn checkpoint_blocking(&mut self, durable: bool, archive_rank: Option<u32>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.checkpoint_with_reply(durable, archive_rank, Some(tx))?;
        match rx.blocking_recv() {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => bail!(error),
            Err(_) => bail!("the checkpoint writer dropped the reply"),
        }
    }

    /// The consistent copy of everything a checkpoint holds, for the writer thread to encode.
    fn snapshot_state(
        &mut self,
    ) -> Result<(u64, flybrain_core::agent::AgentState, RuntimeState)> {
        let generation = self.next_generation;
        self.next_generation += 1;
        let mut agent_state = self.agent.export_state();
        // The sim loop owns the frame remainder, not `NeuralAgent::tick`, so the exported state
        // carries the loop's value.
        agent_state.remainder = self.remainder;
        let runtime = RuntimeState {
            generation,
            wall_ms: now_wall_ms(),
            rom_sha256: self.rom_sha256.clone(),
            emulator_frame: self.frame_counter,
            compatibility: self.compatibility.clone(),
            speed: self.shared.config.loop_.speed,
            buttons: self.buttons,
            rank_since_ms: self.rank_since_ms,
            last_event_id: self.log.next_id().saturating_sub(1),
            reward: self.adapter.export_state(),
            ratchet: self.ratchet.state,
            emulator: self
                .emulator
                .export_state()
                .map_err(|error| anyhow!("exporting the Game Boy: {error}"))?,
            framebuffer: self.frame_buffer.clone(),
            ratchet_game: self.ratchet.game().map(<[u8]>::to_vec).unwrap_or_default(),
            ratchet_frame: self.ratchet.frame().map(<[u8]>::to_vec).unwrap_or_default(),
        };
        Ok((generation, agent_state, runtime))
    }

    // -- publishing ----------------------------------------------------------------------

    fn emit(&mut self, event: NewEvent) -> FeedEvent {
        Metrics::incr(&self.shared.metrics.events_total);
        let ms = self.agent.network.ms;
        self.log.append(now_wall_ms(), ms, event)
    }

    /// Publish the readout's per-channel baseline and score for `GET /status`.
    ///
    /// Diagnostic only: [`Shared::decoder`] is written here and read nowhere else in the loop, so
    /// no decision can depend on it. It is here because the 2026-09-17 stall was invisible from
    /// outside — after a restore the macro channels had no baseline and competed on raw rate, and
    /// nothing the service published said so (`docs/readout.md`).
    fn publish_decoder_status(&mut self) {
        let Ok(mut status) = self.shared.decoder.lock() else { return };
        let decoder = &self.agent.decoder;
        let scores = decoder.last_scores();
        let baselines = decoder.baselines();
        status.calibrated = decoder.calibrated();
        status.pending = decoder.pending_baseline_roles().to_vec();
        status.channels = decoder
            .channel_roles()
            .into_iter()
            .map(|(channel, role)| DecoderChannelStatus {
                channel: channel.to_string(),
                role: role.to_string(),
                baseline: finite(baselines.get_or_zero(role)),
                score: finite(scores.get_or_zero(channel)),
            })
            .collect();
    }

    /// Build and publish one snapshot. `with_attachments` is false while not running, which is
    /// the protocol's header-only idle cadence.
    fn publish(&mut self, with_attachments: bool) {
        self.publish_decoder_status();
        let ms = self.agent.network.ms;
        let stats = self.agent.network.plasticity.statistics();
        let progress = self.adapter.progress();
        let ladder = self.adapter.rank_ladder();
        // Which rung the fly is going for, when the adapter knows: see `next_label`.
        let next_rung = self.adapter.next_rung();
        let rank = progress.rank.min(progress.rank_max);

        let mut rates = Map::new();
        for (name, value) in self.agent.network.rates.iter() {
            rates.insert(name.clone(), json_number(value));
        }

        let mut reward_counts = RewardCounts::default();
        for (kind, count) in &progress.counts {
            if let Some(kind) = RewardKind::from_adapter(kind) {
                reward_counts.add(kind, *count);
            }
        }

        let remaining = self.agent.network.reward_remaining();
        let cooldown = self.limiter.cooldown_ms(now_wall_ms());

        let (frame, audio, spikes, spike_count, attachments) = if with_attachments {
            let (bitset, count) =
                spike_bitset(&self.agent.network.last_spike_ms, self.last_publish_ms, ms);
            let audio = f32_bytes(&self.pending_audio);
            self.pending_audio.clear();
            (
                Arc::new(self.frame_buffer.clone()),
                Arc::new(audio),
                Arc::new(bitset),
                count,
                AttachmentKind::ALL.to_vec(),
            )
        } else {
            (
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                0,
                Vec::new(),
            )
        };

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
            frame: self.frame_counter,
            buttons: self.buttons & 0xff,
            rates,
            population_rate: finite(self.agent.network.population_rate).max(0.0),
            spike_count,
            learning: FeedLearning {
                enabled: stats.enabled,
                updates: finite(stats.updates).max(0.0) as u64,
                changed: stats.changed,
                synapses: stats.synapses as u64,
                signal: finite(stats.signal),
            },
            game: FeedGame {
                mode: GameMode::from_adapter(self.adapter.mode()),
                semantic_rewards: self.semantic_rewards,
                map: self.adapter.map_id(),
                badges: progress.counter.min(8),
                unique_locations: progress.unique_locations as u64,
                reward_total: finite(progress.reward_total),
                reward_counts,
                // Raw mode deals no palette, so it claims no scene either: an empty palette and
                // `unknown` are the honest answers, and the page's MODE chip says which it is.
                scene: self.macros.as_ref().map_or(FeedScene::Unknown, MacroLayer::scene),
                macro_mode: self.shared.config.macros.mode,
                palette: self
                    .macros
                    .as_ref()
                    .map(MacroLayer::feed_palette)
                    .unwrap_or_default(),
                running_macro: self.macros.as_ref().and_then(|layer| layer.feed_macro(ms)),
                macro_outcome: self.macros.as_ref().and_then(MacroLayer::feed_outcome),
                // Report-only (`docs/design/macros.md` section 13.1): how long the pad has had
                // nothing on it in a playable scene. Nothing in the loop reads it back.
                pad_empty_ms: finite(
                    self.macros.as_ref().map_or(0.0, |layer| layer.pad_empty_ms(ms)),
                ),
            },
            // `total` is the running adapter's ladder length, never a number spelled out here:
            // 38 rungs for Pokémon (`docs/design/ladder.md`), 16 for the platformer
            // (`docs/design/platformer.md` §3). `rank_ladder()` is also what the page draws.
            milestone: FeedMilestone {
                rank,
                label: progress.rank_label.to_string(),
                next: next_label(ladder, rank, progress.rank_label, next_rung).to_string(),
                since_seconds: finite((ms - self.rank_since_ms) / 1000.0).max(0.0),
                attempts: self.ratchet.state.attempts,
                total: ladder.len() as u32,
            },
            sugar: FeedSugar {
                active: remaining > 0.0,
                remaining_ms: finite(remaining).max(0.0),
                cooldown_ms: cooldown as f64,
                last_by: self.sugar_last_by.clone(),
                today_count: self.sugar_today,
            },
            events: self.log.take_pending(),
            // The kill switch omits the field rather than sending an empty array.
            chat: if self.shared.config.chat.enabled {
                Some(self.chat_ring.lines())
            } else {
                None
            },
            attachments,
        };

        let snapshot = Snapshot { header, frame, audio, spikes };
        self.last_publish_ms = ms;
        Metrics::incr(&self.shared.metrics.snapshots_published);
        let _ = self.snapshots.send(Arc::new(snapshot));
    }
}

/// The rung the fly is trying for: the label of the adapter's own next rung, or of `rank + 1`
/// where the adapter has no answer, or the current label once there is nothing above it.
///
/// Taken from the ladder rather than from a page-side table, so the whole ladder is the
/// adapter's to change. `current` is the fallback rather than an empty string because
/// the stage renders `next` as "→ <label>" and a blank there reads as a bug.
///
/// **`next_rung` rather than `rank + 1` since 2026-09-17** (`docs/feed-protocol.md`). The rank is
/// the maximum over satisfied rungs, so a rung earned out of order carries it past every rung
/// skipped on the way: the live save at the Viridian gate read rank 8 with rungs 6 and 7 unearned,
/// and the header said "→ VIRIDIAN FOREST" while `GO OBJECTIVE` walked two maps south to deliver a
/// parcel. The screen now names the rung the fly is actually going for. Adapters that answer `None`
/// keep the old arithmetic exactly.
fn next_label<'a>(
    ladder: &[&'a str],
    rank: u32,
    current: &'a str,
    next_rung: Option<u32>,
) -> &'a str {
    let index = next_rung.unwrap_or(rank + 1) as usize;
    ladder.get(index).copied().unwrap_or(current)
}

/// Serde writes `null` for a non-finite f64, which would break the header schema; every rate is
/// clamped to a finite number first.
fn json_number(value: f64) -> Value {
    serde_json::Number::from_f64(finite(value)).map_or(Value::from(0), Value::Number)
}

impl Drop for Sim {
    fn drop(&mut self) {
        self.stop_writer();
    }
}

#[cfg(test)]
mod tests {
    use flybrain_gb::adapter::{GAME_IDS, adapter_for};

    use super::*;

    #[test]
    fn the_next_rung_label_comes_from_the_adapter_ladder_and_holds_at_the_top() {
        let adapter = adapter_for("pokemon-red").unwrap();
        let ladder = adapter.rank_ladder();
        assert_eq!(ladder.len(), 38, "the Pokemon ladder is 38 rungs");

        assert_eq!(next_label(ladder, 0, ladder[0], None), ladder[1]);
        assert_eq!(next_label(ladder, 1, ladder[1], None), "DOWNSTAIRS");
        assert_eq!(next_label(ladder, 36, ladder[36], None), "CHAMPION");
        // Nothing above the top rung, so `next` repeats the current label rather than
        // going blank.
        assert_eq!(next_label(ladder, 37, ladder[37], None), "CHAMPION");
        assert_eq!(next_label(ladder, 99, "WHATEVER", None), "WHATEVER");
        // The adapter's own next rung wins over `rank + 1`, which is the whole point of it: a save
        // with a rung earned out of order reads a rank above rungs it never earned.
        assert_eq!(next_label(ladder, 8, ladder[8], Some(6)), ladder[6]);
        assert_eq!(next_label(ladder, 8, ladder[8], None), ladder[9]);

        // Every game, whatever its ladder is: at the top rung there is nothing above,
        // for the platformer's 16 as much as for Pokémon's 38.
        for id in GAME_IDS {
            let adapter = adapter_for(id).unwrap();
            let ladder = adapter.rank_ladder();
            let top = ladder.len() as u32 - 1;
            assert_eq!(
                next_label(ladder, top, ladder[top as usize], None),
                ladder[top as usize],
                "{id}"
            );
        }
    }

    #[test]
    fn the_booting_header_reports_a_ladder_it_can_honestly_claim() {
        let header = booting_snapshot(1, 0, MacroMode::Raw).header;
        assert_eq!(header.milestone.rank, 0);
        assert!(header.milestone.total >= 1, "a ladder has at least one rung");
        assert!(header.milestone.rank < header.milestone.total, "rank is 0..total-1");
    }
}
