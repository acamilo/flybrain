//! One run on a real brain: a checkpoint restored, the seed's idle frames, then the measured
//! brain minutes, on either runtime.
//!
//! Both runtimes drive the unchanged engine. The legacy runner is the stream's own frame
//! (`flysim::frame::LegacyFrame`, as the trap hunt runs it); the session runner is the session
//! composition the release cut over to (`fly_legacy_session::composition::LegacySession`, the
//! agent, the world and the task under one coordinator, in-process, unpaced). Each hands the same
//! [`Frame`] to the same [`Tally`].
//!
//! Neither runner presses anything or changes what the brain chooses. The seed's idle frames
//! ignore the brain's readout for a while before the measurement starts ([`idle_frames`]); from
//! the first measured frame the readout is the brain's own.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use flybrain_core::agent::{AgentConfig, NeuralAgent};
use flybrain_core::dataset::{BrainDataset, load_brain_dataset_from_dir};
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::lif::SweepPlan;
use flybrain_gb::adapter::GameAdapter;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::Ratchet;
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};
use flysim::config::Config;
use flysim::frame::{FrameObserver, LegacyFrame, Parts, RollbackTrigger};
use flysim::macros::{MacroEvent, MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;

use crate::obs::{Frame, MacroEv, Rolled, facts_of, idle_frames};
use crate::tally::{RunReport, Tally, TallyConfig};
use crate::watchdog::Rules;

/// Which runtime ran it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    Session,
    Legacy,
}

impl Runtime {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "session" => Ok(Self::Session),
            "legacy" => Ok(Self::Legacy),
            other => Err(anyhow!("runtime {other:?} is not session or legacy")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Legacy => "legacy",
        }
    }
}

/// Everything one run needs.
#[derive(Clone, Debug)]
pub struct RunSpec {
    pub id: String,
    pub checkpoint: PathBuf,
    pub rom: PathBuf,
    pub dataset: PathBuf,
    pub seed: u32,
    pub minutes: f64,
    pub threads: usize,
    pub mode: MacroMode,
    pub tally: TallyConfig,
}

impl RunSpec {
    /// A tally configuration with the probe cadence and window the caller chose.
    pub fn tally_config(probe_s: f64, window_s: f64) -> TallyConfig {
        TallyConfig {
            rules: Rules { window_ms: window_s * 1000.0, ..Rules::default() },
            probe_s,
            ..TallyConfig::default()
        }
    }

    fn identity(&self, runtime: Runtime, idle: u32, wall: f64) -> RunReport {
        RunReport {
            checkpoint: self.id.clone(),
            seed: self.seed,
            runtime: runtime.as_str().to_owned(),
            mode: self.mode.as_str().to_owned(),
            idle_frames: idle,
            wall_seconds: wall,
            ..RunReport::default()
        }
    }
}

fn conv(event: &MacroEvent) -> MacroEv {
    MacroEv { name: event.name, outcome: event.outcome.map(|outcome| outcome.as_str()) }
}

/// The measurement's start: it begins on the first frame after the idle frames, with the rung and
/// the exploration count the run stood on just before it.
struct Measure {
    spec: RunSpec,
    idle_left: u32,
    prev: (u32, u32),
    tally: Option<Tally>,
}

impl Measure {
    fn new(spec: &RunSpec, rank: u32, places: u32) -> Self {
        Self { spec: spec.clone(), idle_left: idle_frames(spec.seed), prev: (rank, places), tally: None }
    }

    /// Takes one frame; true when the measured minutes are up.
    fn take(&mut self, frame: Frame) -> bool {
        let (rank, places) = (frame.rank, frame.places);
        if self.idle_left > 0 {
            self.idle_left -= 1;
            self.prev = (rank, places);
            return false;
        }
        let tally = self.tally.get_or_insert_with(|| {
            Tally::new(self.spec.tally.clone(), frame.ms, self.prev.0, self.prev.1)
        });
        tally.push(&frame);
        tally.minutes() >= self.spec.minutes
    }

    fn finish(self, runtime: Runtime, wall: f64) -> Result<RunReport> {
        let idle = idle_frames(self.spec.seed);
        let tally = self.tally.ok_or_else(|| anyhow!("no frame was measured"))?;
        Ok(tally.finish(self.spec.identity(runtime, idle, wall)))
    }
}

// -- the legacy runtime -----------------------------------------------------------------------

/// The seed's idle frames, and the pad's state read just before the executor decides.
struct Idle {
    left: u32,
    pad_empty: bool,
}

impl FrameObserver for Idle {
    fn readout(&mut self, _ms: f64, _bound: Option<&[String]>, active: &mut Vec<String>) {
        if self.left > 0 {
            self.left -= 1;
            active.clear();
        }
    }

    fn before_execute(&mut self, _frame: &LegacyFrame, parts: &mut Parts<'_>, _active: &[String]) {
        self.pad_empty = parts
            .macros
            .as_deref()
            .is_some_and(|layer| layer.running().is_none() && layer.feed_palette().is_empty());
    }
}

fn load_dataset(spec: &RunSpec) -> Result<Arc<BrainDataset>> {
    let data = load_brain_dataset_from_dir(&spec.dataset)
        .map_err(|e| anyhow!("loading the connectome at {}: {e}", spec.dataset.display()))?;
    Ok(Arc::new(data))
}

/// The stream's own frame, restored from the checkpoint as the stream restores it.
pub fn run_legacy(spec: &RunSpec) -> Result<RunReport> {
    let began_wall = Instant::now();
    let rom = std::fs::read(&spec.rom).with_context(|| format!("reading {}", spec.rom.display()))?;
    let checkpoint = flysim::store::load(&spec.checkpoint)
        .with_context(|| format!("the checkpoint {}", spec.checkpoint.display()))?;
    let data = load_dataset(spec)?;

    let mut emulator = Emulator::new(&rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .map_err(|e| anyhow!("the cartridge: {e}"))?;
    let mut adapter = PokemonRedReward::new();
    let channels =
        if spec.mode.dealt() { flybrain_gb::macro_channels("pokemon-red") } else { Vec::new() };
    let preset = gameboy_decoder_config_with_macros(&channels);
    let hold_ms = preset
        .macros
        .as_ref()
        .or(preset.exclusive.as_ref())
        .map(|group| group.hold_ms)
        .ok_or_else(|| anyhow!("the preset has no group"))?;
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
    let mut agent = NeuralAgent::new(Arc::clone(&data), AgentConfig::with_decoder(preset))
        .map_err(|e| anyhow!("{e}"))?;
    if spec.threads > 1 {
        agent.set_sweep_plan(SweepPlan::with_threads(spec.threads).map_err(|e| anyhow!("{e}"))?);
    }
    let mut frame = LegacyFrame::new();
    frame
        .restore(
            &mut Parts {
                agent: &mut agent,
                emulator: &mut emulator,
                adapter: &mut adapter,
                ratchet: &mut ratchet,
                macros: None,
            },
            &checkpoint,
        )
        .context("the checkpoint should restore")?;
    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_string();
    config.macros.mode = spec.mode;
    // As the service seeds it after a restore: the restored brain's own generator.
    let mut macros: Option<MacroLayer> =
        macro_layer(&config, hold_ms, agent.network.rng_state() as u32);
    if let Some(layer) = macros.as_mut() {
        let ledger = AdapterLedger(&adapter);
        let _ = layer.observe(&mut emulator, &ledger, agent.network.ms);
    }

    let start = adapter.progress();
    let mut measure = Measure::new(spec, start.rank, start.unique_locations as u32);
    let mut observer = Idle { left: measure.idle_left, pad_empty: false };
    loop {
        let mut parts = Parts {
            agent: &mut agent,
            emulator: &mut emulator,
            adapter: &mut adapter,
            ratchet: &mut ratchet,
            macros: macros.as_mut(),
        };
        let transition = frame.transition(&mut parts, &mut observer).context("a frame")?;
        let boundary = frame
            .boundary(&mut parts, &transition.evaluated.progress, transition.ms)
            .context("the boundary")?;
        let mut events: Vec<MacroEv> = transition.executed.events.iter().map(conv).collect();
        events.extend(transition.evaluated.abandoned.iter().map(conv));
        let rolled = boundary.rollback.as_ref().map(|rollback| {
            events.extend(rollback.events.iter().map(conv));
            Rolled { game_over: rollback.trigger == RollbackTrigger::GameOver }
        });
        let progress = &transition.evaluated.progress;
        let obs = Frame {
            ms: transition.ms,
            events,
            rewards: transition.evaluated.rewards.iter().map(|r| (r.kind, r.value)).collect(),
            rank: progress.rank,
            places: progress.unique_locations as u32,
            location: parts.adapter.location(),
            macros_on: parts.macros.is_some(),
            pad_empty: observer.pad_empty,
            scene: parts.macros.as_deref().map_or("raw", MacroLayer::scene_name),
            rollback: rolled,
            facts: facts_of(parts.emulator),
        };
        if measure.take(obs) {
            break;
        }
    }
    measure.finish(Runtime::Legacy, began_wall.elapsed().as_secs_f64())
}

// -- the session runtime ----------------------------------------------------------------------

/// The seed's idle frames on the session's executor: the same readout replacement the legacy
/// frame's observer makes.
struct IdleDriver {
    left: u32,
}

impl fly_legacy_session::driver::DecisionDriver for IdleDriver {
    fn readout(&mut self, _ms: f64, _bound: &[String], active: &mut Vec<String>) {
        if self.left > 0 {
            self.left -= 1;
            active.clear();
        }
    }
}

/// The session composition the release runs, in-process and unpaced, from the checkpoint.
pub async fn run_session(spec: &RunSpec) -> Result<RunReport> {
    use fly_legacy_session::composition::{LegacyConfig, LegacySession};
    use fly_session::ExecutionMode;
    use fly_session::legacy_agent::LegacyProfileKind;
    use fly_session::types::id;

    let began_wall = Instant::now();
    let checkpoint = flysim::store::load(&spec.checkpoint)
        .with_context(|| format!("the checkpoint {}", spec.checkpoint.display()))?;
    let root = tempfile::tempdir().context("a scratch directory")?;
    let mut session = LegacySession::start(
        root.path(),
        LegacyConfig {
            mode: ExecutionMode::InProcess,
            rom_path: spec.rom.clone(),
            dataset_dir: spec.dataset.clone(),
            profile: LegacyProfileKind::Production,
            macro_mode: spec.mode,
            agent_id: id("fly"),
            agent_threads: spec.threads.max(1),
            record: false,
        },
    )
    .await
    .map_err(|e| anyhow!("{e}"))?;
    session.bootstrap().await.map_err(|e| anyhow!("{e}"))?;
    session
        .import_checkpoint(&checkpoint, &id("e2"))
        .await
        .map_err(|e| anyhow!("importing the checkpoint: {e}"))?;
    // A measurement runs as fast as the brain allows.
    session.coordinator.disable_pacing();
    let task = session.task.clone();
    let (rank, places) = task.inspect(|adapter, _, _| {
        let progress = adapter.progress();
        (progress.rank, progress.unique_locations as u32)
    });
    let mut measure = Measure::new(spec, rank, places);
    task.set_driver(Box::new(IdleDriver { left: measure.idle_left }));

    let outcome = async {
        loop {
            session.step().await.map_err(|e| anyhow!("{e}"))?;
            let feed = task.take_feed();
            let ms = task.brain_ms();
            let mut events: Vec<MacroEv> = feed.executed.iter().map(conv).collect();
            events.extend(feed.abandoned.iter().map(conv));
            let rolled = feed.rollback.as_ref().map(|(game_over, rollback_events)| {
                events.extend(rollback_events.iter().map(conv));
                Rolled { game_over: *game_over }
            });
            let (rank, places, location, macros_on, pad_empty, scene) =
                task.inspect(|adapter, _, macros| {
                    let progress = adapter.progress();
                    (
                        progress.rank,
                        progress.unique_locations as u32,
                        adapter.location(),
                        macros.is_some(),
                        macros.is_some_and(|l| l.running().is_none() && l.feed_palette().is_empty()),
                        macros.map_or("raw", MacroLayer::scene_name),
                    )
                });
            let facts = task
                .with_reader(|memory| facts_of(memory))
                .ok_or_else(|| anyhow!("the task has no boundary image"))?;
            let obs = Frame {
                ms,
                events,
                rewards: feed.rewards.iter().map(|r| (r.kind, r.value)).collect(),
                rank,
                places,
                location,
                macros_on,
                pad_empty,
                scene,
                rollback: rolled,
                facts,
            };
            if measure.take(obs) {
                break;
            }
        }
        anyhow::Ok(())
    }
    .await;
    session.stop().await;
    outcome?;
    measure.finish(Runtime::Session, began_wall.elapsed().as_secs_f64())
}

/// The adapter and the compatibility digest of this build, for the report.
pub fn build_info(release: &str) -> crate::suite::BuildInfo {
    use fly_legacy_session::composition::{LegacyConfig, compatibility_of};
    use fly_session::ExecutionMode;
    use fly_session::legacy_agent::LegacyProfileKind;
    use fly_session::types::id;
    let adapter = GameAdapter::id(&PokemonRedReward::new()).to_owned();
    let compat = compatibility_of(&LegacyConfig {
        mode: ExecutionMode::InProcess,
        rom_path: PathBuf::new(),
        dataset_dir: PathBuf::new(),
        profile: LegacyProfileKind::Production,
        macro_mode: MacroMode::Macros,
        agent_id: id("fly"),
        agent_threads: 1,
        record: false,
    });
    let digest = flybrain_core::dataset::sha256_hex(compat.as_bytes());
    crate::suite::BuildInfo {
        release: release.to_owned(),
        adapter,
        compatibility_sha: digest.chars().take(12).collect(),
    }
}
