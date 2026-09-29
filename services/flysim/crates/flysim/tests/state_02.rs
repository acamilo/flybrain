//! STATE-02: the session runtime's checkpoints are the legacy loop's `FLYSIM01`, on real
//! checkpoints and against the running legacy frame.
//!
//! Three proofs, all explicit ROM jobs (`. bin/rom-env.sh`, never a download):
//!
//! - `every_real_checkpoint_round_trips_legacy_session_legacy` (`FLY_ROM` and the rom-env
//!   checkpoints): each checkpoint is read into its owners' halves by the shipped import, the
//!   world is restored into the real environment worker and captured again, the agent state goes
//!   through the `FLYAGT01` payload the import builds, the task half through the Pokémon adapter
//!   and the ratchet, and the `FLYSIM01` export is compared with the original file byte for byte.
//! - `toy_restore_then_continue` (`FLY_ROM`): the legacy frame warms a toy fly up from power-on,
//!   the legacy loop writes a checkpoint into a store, and from that store the legacy loop and
//!   the session runtime each restore and run on. The session runtime's world and agent are
//!   held to the legacy loop's `FLY_TRACE` transition by transition, its export at the end is
//!   compared byte for byte with the legacy loop's own checkpoint of the same boundary, and that
//!   export then goes session -> legacy -> session unchanged.
//! - `fafb_restore_then_continue` (also `FLY_STATE02_FAFB=1` and `data/fafb-v783`): the same
//!   with the live composition, `gameboy-legacy-fafb-v783-v1` in macros mode, from a stream
//!   checkpoint (`FLY_STATE02_CHECKPOINT`, else `FLY_ROW67_CHECKPOINT`), with sugar and a ratchet
//!   rollback.
//!
//! The executor and the task's per-frame evaluation are TASK-01's; here the joypad masks come
//! from the trace and the task's end-of-run half from the legacy loop, which is what makes the
//! world and the agent comparable on their own. `FLY_STATE02_FRAMES` sets the run length
//! (default 600 toy, 300 FAFB).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fly_session::ExecutionMode;
use fly_session::fly_session_types::gameboy::{Location, ReadoutContext};
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_checkpoint::{self as lc, HostHalf, Selection, TaskHalf};
use fly_session::legacy_env::{self, BackendConfig};
use fly_session::legacy_env_parity::{EnvRecord, EnvRig, FlyTrace};
use fly_session::legacy_parity::{
    self, LegacyRig, LegacyScript, RewardEvent, RigAgent, ScriptStep, TraceTransition, toy,
};
use fly_session::types::{digest_of_bytes, id};
use flybrain_core::agent::{AgentConfig, NeuralAgent};
use flybrain_core::dataset::{BrainDataset, load_brain_dataset_from_dir};
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_gb::adapter::GameAdapter;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::ratchet::{Ratchet, Snapshot};
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};
use flysim::config::Config;
use flysim::frame::{LegacyFrame, Parts};
use flysim::macros::{MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;
use flysim::store::{self, Checkpoint, RuntimeState, Store};
use flysim::trace::FrameTrace;

const EPISODE: &str = "ep1";

fn rom_path() -> Option<PathBuf> {
    let Some(path) = std::env::var_os("FLY_ROM") else {
        eprintln!("skipping: FLY_ROM is not set (source bin/rom-env.sh)");
        return None;
    };
    Some(PathBuf::from(path))
}

fn frames(default: usize) -> usize {
    std::env::var("FLY_STATE02_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn slot() -> fly_session::types::Id {
    id(legacy_env::DEFAULT_SLOT)
}

/// Every stream checkpoint rom-env names, once each, by variable name.
fn rom_env_checkpoints() -> Vec<(String, PathBuf)> {
    let mut seen: HashMap<PathBuf, String> = HashMap::new();
    for (key, value) in std::env::vars() {
        if key.starts_with("FLY_") && key.ends_with("_CHECKPOINT") {
            let path = PathBuf::from(value);
            if path.is_file() {
                seen.entry(path).or_insert(key);
            }
        }
    }
    let mut out: Vec<(String, PathBuf)> = seen.into_iter().map(|(p, k)| (k, p)).collect();
    out.sort();
    out
}

fn context(adapter: &dyn GameAdapter, macros: Option<&MacroLayer>) -> ReadoutContext {
    ReadoutContext {
        boot: adapter.boot(),
        bound: macros.map(MacroLayer::bound_channels).unwrap_or_default(),
        location: adapter.location().map(|(area, x, y)| Location { area, x, y }),
    }
}

/// The host fields the two runtimes are given alike, so their bytes are comparable.
fn host_of(runtime: &RuntimeState, generation: u64) -> HostHalf {
    HostHalf {
        generation,
        wall_ms: runtime.wall_ms,
        compatibility: runtime.compatibility.clone(),
        speed: runtime.speed,
        rank_since_ms: runtime.rank_since_ms,
        last_event_id: runtime.last_event_id,
    }
}

/// The field that first differs between two `FLYSIM01` files, for a readable failure.
fn first_difference(left: &[u8], right: &[u8]) -> String {
    let (Ok(a), Ok(b)) = (store::decode(left), store::decode(right)) else {
        return "one side does not decode".to_owned();
    };
    let checks: Vec<(&str, bool)> = vec![
        ("agent", a.agent == b.agent),
        ("generation", a.runtime.generation == b.runtime.generation),
        ("wallMs", a.runtime.wall_ms == b.runtime.wall_ms),
        ("romHash", a.runtime.rom_sha256 == b.runtime.rom_sha256),
        ("emulatorFrame", a.runtime.emulator_frame == b.runtime.emulator_frame),
        ("compatibility", a.runtime.compatibility == b.runtime.compatibility),
        ("speed", a.runtime.speed == b.runtime.speed),
        ("buttons", a.runtime.buttons == b.runtime.buttons),
        ("rankSinceMs", a.runtime.rank_since_ms == b.runtime.rank_since_ms),
        ("lastEventId", a.runtime.last_event_id == b.runtime.last_event_id),
        ("reward", a.runtime.reward == b.runtime.reward),
        ("ratchet", a.runtime.ratchet == b.runtime.ratchet),
        ("emulator", a.runtime.emulator == b.runtime.emulator),
        ("framebuffer", a.runtime.framebuffer == b.runtime.framebuffer),
        ("ratchetGame", a.runtime.ratchet_game == b.runtime.ratchet_game),
        ("ratchetFrame", a.runtime.ratchet_frame == b.runtime.ratchet_frame),
    ];
    let differing: Vec<&str> = checks.iter().filter(|(_, same)| !same).map(|(n, _)| *n).collect();
    if differing.is_empty() {
        "every field decodes equal; the bytes differ in layout".to_owned()
    } else {
        format!("differing fields: {}", differing.join(", "))
    }
}

/// Which world fields differ, and where the first differing emulator byte is.
fn world_difference(a: &fly_session::legacy_env::WorldState, b: &fly_session::legacy_env::WorldState) -> String {
    let mut out = Vec::new();
    if a.boundary != b.boundary || a.engine_frame != b.engine_frame {
        out.push(format!("boundary/frame {}/{} vs {}/{}", a.boundary, a.engine_frame, b.boundary, b.engine_frame));
    }
    if a.world_time != b.world_time || a.audio_next_sample != b.audio_next_sample {
        out.push("clock".to_owned());
    }
    if a.buttons != b.buttons {
        out.push(format!("buttons {} vs {}", a.buttons, b.buttons));
    }
    if a.framebuffer != b.framebuffer {
        out.push("framebuffer".to_owned());
    }
    if a.slots != b.slots {
        out.push("slots".to_owned());
    }
    if a.emulator != b.emulator {
        let first = a.emulator.iter().zip(&b.emulator).position(|(x, y)| x != y);
        let count = a.emulator.iter().zip(&b.emulator).filter(|(x, y)| x != y).count();
        out.push(format!(
            "emulator ({} vs {} bytes, first difference at {first:?}, {count} bytes differ)",
            a.emulator.len(),
            b.emulator.len()
        ));
    }
    if a.rom_digest != b.rom_digest || a.episode_id != b.episode_id {
        out.push("identity".to_owned());
    }
    out.join("; ")
}

// -------------------------------------------------------------------------------------------
// The session runtime's world, through the environment worker

/// The world half of `halves` restored into the environment worker through the shipped payload,
/// the worker driven by `ops`, then captured: the records and the captured payload.
async fn world_on_worker(
    rig: &mut EnvRig,
    halves: &lc::Halves,
    ops: &[fly_session::legacy_env_parity::EnvOp],
) -> (Vec<EnvRecord>, Vec<u8>) {
    use fly_session::legacy_env_parity::EnvOp;
    let mut driver = rig.driver();
    let source = lc::legacy_source_scope(&rig.session_id, halves.world.boundary);
    let captured = driver.payload_of(&halves.world, source.clone()).await.expect("sealed");
    // The harness's sealing is the shipped encoder with the same header.
    let descriptor = legacy_env::LegacyGameboyEnvironment::descriptor_for(&BackendConfig::legacy(
        &halves.world.rom_digest,
    ));
    assert_eq!(
        captured.bytes,
        lc::world_payload(
            &halves.world,
            &rig.worker_id,
            &captured.result.checkpoint_id,
            &source,
            &descriptor.configuration_digest,
        ),
        "the world payload is the shipped import's"
    );
    let target = driver.target().clone();
    let mut records = vec![driver.restore_into(target, &captured).await.expect("the world restores")];
    for op in ops {
        records.push(match op {
            EnvOp::Advance(mask) => driver.advance(*mask).await.expect("advance"),
            EnvOp::SaveSlot(slot) => driver.save_slot(slot).await.expect("save slot"),
            EnvOp::Rollback(slot) => driver.rollback(slot).await.expect("rollback"),
            EnvOp::CaptureRestore => unreachable!("not in these scripts"),
        });
    }
    let bytes = driver.capture().await.expect("the world captures").bytes;
    (records, bytes)
}

async fn env_rig(root: &Path, rom: &Path, rom_digest: &str) -> EnvRig {
    EnvRig::start(root, ExecutionMode::InProcess, rom, BackendConfig::legacy(rom_digest))
        .await
        .expect("the environment rig")
}

/// The task half through the objects TASK-01's task holds: the adapter and the ratchet.
fn task_through_adapter(task: &TaskHalf, snapshot: Option<Snapshot>) -> TaskHalf {
    let mut adapter = PokemonRedReward::new();
    if !task.reward.is_null() {
        adapter.import_state(&task.reward).expect("the adapter imports its state");
    }
    let mut ratchet = Ratchet::with_policy(adapter.recovery_policy());
    ratchet
        .import(Some(task.ratchet), snapshot, adapter.rank_ladder().len())
        .expect("the ratchet imports its state");
    TaskHalf {
        reward: adapter.export_state(),
        ratchet: ratchet.state,
    }
}

// -------------------------------------------------------------------------------------------
// 1. legacy -> session -> legacy on every real checkpoint

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_real_checkpoint_round_trips_legacy_session_legacy() {
    let Some(rom) = rom_path() else { return };
    let checkpoints = rom_env_checkpoints();
    if checkpoints.is_empty() {
        eprintln!("skipping: no FLY_*_CHECKPOINT is set (source bin/rom-env.sh)");
        return;
    }
    let root = tempfile::tempdir().expect("tmp");
    let mut identical = 0;
    let mut semantic = Vec::new();
    for (name, path) in &checkpoints {
        let original = std::fs::read(path).expect("the checkpoint reads");
        let checkpoint = store::decode(&original).expect("a FLYSIM01 checkpoint");
        let halves = lc::halves(&checkpoint, &id(EPISODE), &slot(), legacy_env::DEFAULT_AUDIO_RATE)
            .expect("the halves");

        // The world: into the real environment worker and out again.
        let rig_root = root.path().join(name.to_lowercase());
        let mut rig = env_rig(&rig_root, &rom, &halves.world.rom_digest).await;
        let (_, world_bytes) = world_on_worker(&mut rig, &halves, &[]).await;
        rig.stop().await;
        let world = lc::world_state(&world_bytes).expect("the captured world");

        // Everything but the emulator's bytes is the file's; those are the legacy restore's
        // (import, then the recorded joypad applied), compared below through the export.
        let expected = fly_session::legacy_env::WorldState {
            emulator: world.emulator.clone(),
            ..halves.world.clone()
        };
        assert!(
            world == expected,
            "{name}: the world restores and captures exactly: {}",
            world_difference(&expected, &world)
        );

        // The agent: the FLYAGT01 payload the import builds carries exactly the legacy state.
        let import = lc::AgentImport {
            agent_id: id("fly"),
            profile: fly_session::fly_session_types::gameboy::profile_asset_ref(),
            seed: legacy_parity::LEGACY_SEED,
            macro_channels: flybrain_gb::macro_channels("pokemon-red")
                .iter()
                .map(|c| (*c).to_owned())
                .collect(),
        };
        let restored_context = ReadoutContext { boot: false, bound: vec![], location: None };
        let agent_bytes = lc::agent_payload(
            &halves.agent,
            &halves.world.framebuffer,
            &import,
            &id("c1"),
            &lc::legacy_source_scope(&id("legacy"), halves.world.boundary),
            &restored_context.to_typed(),
        )
        .expect("the agent payload");

        // The task: through the adapter and the ratchet, as the task object holds them.
        let snapshot = (!checkpoint.runtime.ratchet_game.is_empty()).then(|| Snapshot {
            game: checkpoint.runtime.ratchet_game.clone(),
            frame: checkpoint.runtime.ratchet_frame.clone(),
        });
        let task = task_through_adapter(&halves.task, snapshot);

        let exported = lc::export(&agent_bytes, &world_bytes, &slot(), &task, &halves.host)
            .expect("the export");
        // And legacy again: the legacy store reads it and writes it back unchanged.
        let back = store::decode(&exported).expect("the legacy loop reads the export");
        assert_eq!(store::encode(&back.agent, &back.runtime).unwrap(), exported);

        // The legacy loop's own restore outcome of the same file, checkpointed at once: what
        // `LegacyFrame::restore` leaves in the emulator (import, then `set_buttons(buttons)`),
        // the adapter (import, export) and the ratchet (import). The agent's import is exact
        // (AGENT-01; the FAFB proof below restores it into both runtimes).
        let rom_bytes = std::fs::read(&rom).expect("the ROM reads");
        let mut emulator = Emulator::new(&rom_bytes, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
            .expect("binjgb should accept the cartridge");
        emulator.import_state(&checkpoint.runtime.emulator).expect("the emulator imports");
        emulator.set_buttons(checkpoint.runtime.buttons as u8);
        let snapshot = (!checkpoint.runtime.ratchet_game.is_empty()).then(|| Snapshot {
            game: checkpoint.runtime.ratchet_game.clone(),
            frame: checkpoint.runtime.ratchet_frame.clone(),
        });
        let legacy_task = task_through_adapter(&halves.task, snapshot);
        let legacy = store::encode(
            &checkpoint.agent,
            &RuntimeState {
                emulator: emulator.export_state().expect("the emulator exports"),
                reward: legacy_task.reward,
                ratchet: legacy_task.ratchet,
                ..checkpoint.runtime.clone()
            },
        )
        .expect("the legacy encoder");
        assert!(
            exported == legacy,
            "{name}: the session export is not the legacy restore outcome: {}",
            first_difference(&legacy, &exported)
        );
        if exported == original {
            identical += 1;
            eprintln!("{name}: {} bytes, byte-identical to the file", original.len());
        } else {
            // Both runtimes restore the same: what moved is the file's, and the reason is named.
            let mut why = first_difference(&original, &exported);
            if why.contains("reward") {
                // Only an older adapter's state moves: the adapter's own import migration.
                let adapter = checkpoint.runtime.compatibility.split('/').nth(1).unwrap_or("");
                assert_ne!(adapter, PokemonRedReward::new().id(), "{name}: {why}");
                why.push_str(&format!(" (written by adapter {adapter}, migrated on import)"));
            }
            eprintln!("{name}: {} bytes, the legacy restore outcome; vs the file: {why}", original.len());
            semantic.push(format!("{name} ({why})"));
        }
    }
    eprintln!(
        "legacy -> session -> legacy: {} checkpoints, every export equal to the legacy restore \
         outcome; {identical} byte-identical to the file, {} not: {semantic:?}",
        checkpoints.len(),
        semantic.len()
    );
}

// -------------------------------------------------------------------------------------------
// 2 and 3. restore then continue, against the legacy loop's trace

/// A legacy brain and everything the legacy frame drives.
struct LegacyWorld {
    agent: NeuralAgent,
    emulator: Emulator,
    adapter: PokemonRedReward,
    ratchet: Ratchet,
    frame: LegacyFrame,
    macros: Option<MacroLayer>,
}

fn legacy_world(
    rom: &[u8],
    data: &Arc<BrainDataset>,
    mode: MacroMode,
    trace: Option<&Path>,
) -> (LegacyWorld, Vec<String>) {
    let emulator = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the cartridge");
    let adapter = PokemonRedReward::new();
    let ratchet = Ratchet::with_policy(adapter.recovery_policy());
    let channels = if mode.dealt() {
        flybrain_gb::macro_channels("pokemon-red")
    } else {
        Vec::new()
    };
    let preset = gameboy_decoder_config_with_macros(&channels);
    let mut agent_config = AgentConfig::with_decoder(preset);
    agent_config.warmup_ms = Config::default().loop_.warmup_ms;
    let agent = NeuralAgent::new(Arc::clone(data), agent_config).expect("a valid agent");
    let frame = LegacyFrame::new().with_trace(
        trace.map(|path| FrameTrace::create(path).expect("a trace file")),
    );
    (
        LegacyWorld { agent, emulator, adapter, ratchet, frame, macros: None },
        channels.iter().map(|c| (*c).to_owned()).collect(),
    )
}

impl LegacyWorld {
    fn parts(&mut self) -> Parts<'_> {
        Parts {
            agent: &mut self.agent,
            emulator: &mut self.emulator,
            adapter: &mut self.adapter,
            ratchet: &mut self.ratchet,
            macros: self.macros.as_mut(),
        }
    }

    /// `Sim::boot` after its restore or warm-up: the macro layer, seeded from the network, and
    /// one observation before the first frame.
    fn deal(&mut self, mode: MacroMode) {
        let mut config = Config::default();
        config.loop_.game = "pokemon-red".to_string();
        config.macros.mode = mode;
        let channels = if mode.dealt() { flybrain_gb::macro_channels("pokemon-red") } else { Vec::new() };
        let preset = gameboy_decoder_config_with_macros(&channels);
        let hold_ms = preset
            .macros
            .as_ref()
            .or(preset.exclusive.as_ref())
            .expect("the preset has a group")
            .hold_ms;
        self.macros = macro_layer(&config, hold_ms, self.agent.network.rng_state() as u32);
        if let Some(layer) = self.macros.as_mut() {
            let ledger = AdapterLedger(&self.adapter);
            let _ = layer.observe(&mut self.emulator, &ledger, self.agent.network.ms);
        }
    }

    /// `Sim::snapshot_state` with the host fields given.
    fn checkpoint(&mut self, host: &HostHalf) -> Vec<u8> {
        let mut agent = self.agent.export_state();
        agent.remainder = self.frame.remainder;
        let runtime = RuntimeState {
            generation: host.generation,
            wall_ms: host.wall_ms,
            rom_sha256: self.emulator.rom_sha256(),
            emulator_frame: self.frame.frame_counter,
            compatibility: host.compatibility.clone(),
            speed: host.speed,
            buttons: self.frame.buttons,
            rank_since_ms: host.rank_since_ms,
            last_event_id: host.last_event_id,
            reward: self.adapter.export_state(),
            ratchet: self.ratchet.state,
            emulator: self.emulator.export_state().expect("the emulator exports"),
            framebuffer: self.frame.frame_buffer.clone(),
            ratchet_game: self.ratchet.game().map(<[u8]>::to_vec).unwrap_or_default(),
            ratchet_frame: self.ratchet.frame().map(<[u8]>::to_vec).unwrap_or_default(),
        };
        store::encode(&agent, &runtime).expect("the legacy encoder")
    }

    fn task(&self) -> TaskHalf {
        TaskHalf { reward: self.adapter.export_state(), ratchet: self.ratchet.state }
    }
}

/// Frames by content, so a still screen is stored once.
#[derive(Default)]
struct Pool {
    frames: Vec<Vec<u8>>,
    index: HashMap<String, usize>,
}

impl Pool {
    fn add(&mut self, frame: &[u8]) -> usize {
        let digest = digest_of_bytes(frame);
        *self.index.entry(digest).or_insert_with(|| {
            self.frames.push(frame.to_vec());
            self.frames.len() - 1
        })
    }
}

/// The legacy loop restored from `checkpoint` and run `n` frames with `FLY_TRACE` on: the trace,
/// the agent's script, and the legacy world at the end.
#[allow(clippy::too_many_arguments)]
fn legacy_run(
    rom: &[u8],
    data: &Arc<BrainDataset>,
    checkpoint: &Checkpoint,
    mode: MacroMode,
    n: usize,
    sugar: &[(usize, f64)],
    rollback_at: Option<usize>,
    trace_path: &Path,
) -> (LegacyWorld, LegacyScript, Vec<TraceTransition>) {
    let (mut world, channels) = legacy_world(rom, data, mode, Some(trace_path));
    {
        let mut frame = std::mem::replace(&mut world.frame, LegacyFrame::new());
        frame.restore(&mut world.parts(), checkpoint).expect("the checkpoint restores");
        world.frame = frame;
    }
    world.deal(mode);
    let mut pool = Pool::default();
    let initial_frame = pool.add(&world.frame.frame_buffer);
    let initial_context = context(&world.adapter, world.macros.as_ref());
    let mut steps = Vec::new();
    for k in 1..=n {
        let mut admitted = Vec::new();
        for (_, duration) in sugar.iter().filter(|(at, _)| *at == k) {
            world.agent.network.stimulate(*duration);
            if let Some(trace) = world.frame.trace_mut() {
                trace.sugar(*duration);
            }
            admitted.push(*duration);
        }
        let mut frame = std::mem::replace(&mut world.frame, LegacyFrame::new());
        let mut parts = world.parts();
        let transition = frame.transition(&mut parts, &mut ()).expect("a frame");
        // The frame this transition produced, before a rollback puts the slot's on screen.
        let produced = pool.add(&frame.frame_buffer);
        let rewards: Vec<RewardEvent> = transition
            .evaluated
            .rewards
            .iter()
            .map(|event| RewardEvent { value: event.value, stimulation_ms: f64::from(event.stimulation_ms) })
            .collect();
        let next_context = context(&*parts.adapter, parts.macros.as_deref());
        let boundary = frame
            .boundary(&mut parts, &transition.evaluated.progress, transition.ms)
            .expect("the boundary");
        let mut rolled_back = boundary.rollback.is_some();
        if !rolled_back && rollback_at == Some(k) && parts.ratchet.game().is_some() {
            frame.rollback(&mut parts).expect("the forced rollback");
            rolled_back = true;
        }
        let rollback_context = rolled_back.then(|| context(&*parts.adapter, parts.macros.as_deref()));
        world.frame = frame;
        steps.push(ScriptStep::Frame { sugar: admitted, frame: produced, rewards, next_context });
        if let Some(context) = rollback_context {
            steps.push(ScriptStep::Rollback { frame: pool.add(&world.frame.frame_buffer), context });
        }
    }
    world.frame.finish_trace();
    let text = std::fs::read_to_string(trace_path).expect("the trace reads back");
    let trace = legacy_parity::read_frame_trace(&text).expect("a flysim-legacy-frame-trace-v1");
    let script = LegacyScript {
        name: format!("state-02-{}", mode.as_str()),
        macro_channels: channels,
        frames: Arc::new(pool.frames),
        initial_frame,
        initial_context,
        steps,
        checkpoints: Default::default(),
    };
    (world, script, trace)
}

async fn agent_rig(root: &Path, dataset_dir: PathBuf, profile: LegacyProfileKind, channels: &[String]) -> LegacyRig {
    let agent = RigAgent {
        agent_id: id("fly"),
        port_id: id("p1"),
        dataset_dir,
        profile,
        macro_channels: channels.to_vec(),
        worker_threads: 2,
    };
    LegacyRig::start(root, ExecutionMode::InProcess, &[agent]).await.expect("the agent rig")
}

/// What one restore-then-continue proves, for the report line.
struct Proof {
    transitions: usize,
    world_values: usize,
    rollbacks: usize,
    sugar: usize,
    bytes: usize,
}

/// The whole proof from one `FLYSIM01` start:
///
/// 1. the start is committed into a durable store, under a torn hot copy, and the session
///    runtime selects it the legacy way;
/// 2. the legacy loop restores the same bytes and runs `n` frames with the trace on;
/// 3. the session runtime's world and agent, restored through the shipped import, run the
///    same transitions and are held to that trace;
/// 4. at the end the session runtime's `FLYSIM01` export equals the legacy loop's checkpoint of
///    the same boundary, byte for byte;
/// 5. that export restores into a fresh legacy loop, whose checkpoint is the same bytes, and
///    back into a fresh session runtime, whose export is the same bytes again.
#[allow(clippy::too_many_arguments)]
async fn restore_then_continue(
    rom_path: &Path,
    data: &Arc<BrainDataset>,
    dataset_dir: PathBuf,
    profile: LegacyProfileKind,
    start: &[u8],
    mode: MacroMode,
    n: usize,
    sugar: &[(usize, f64)],
    rollback_at: Option<usize>,
) -> Proof {
    let rom = std::fs::read(rom_path).expect("the ROM reads");
    let root = tempfile::tempdir().expect("tmp");

    // 1. The live store: a durable generation under a torn hot one.
    let start_checkpoint = store::decode(start).expect("the start decodes");
    let durable = Store::new(root.path().join("state"), 8);
    let hot = Store::new(root.path().join("hot"), 8);
    durable.commit(start_checkpoint.runtime.generation, start, None).expect("commit");
    hot.commit(start_checkpoint.runtime.generation + 1, b"torn by a crash", None).expect("commit");
    let gate = lc::RestoreGate {
        rom_sha256: start_checkpoint.runtime.rom_sha256.clone(),
        compatibility: start_checkpoint.runtime.compatibility.clone(),
        migrates_from: Vec::new(),
        accepted: Vec::new(),
    };
    let selection = lc::restore_from_store(&hot, &durable, &gate, |checkpoint| async move {
        lc::halves(&checkpoint, &id(EPISODE), &slot(), legacy_env::DEFAULT_AUDIO_RATE)
    })
    .await;
    let Selection::Restored(restored) = selection else {
        panic!("the durable generation should restore: {selection:?}")
    };
    assert_eq!(restored.skipped.len(), 1, "the torn hot copy is skipped");
    let halves = restored.installed;
    let generation = start_checkpoint.runtime.generation + 10;
    let host = host_of(&start_checkpoint.runtime, generation);

    // 2. The legacy loop from the same bytes.
    let trace_path = root.path().join("trace.jsonl");
    let (mut legacy, script, trace) =
        legacy_run(&rom, data, &restored.checkpoint, mode, n, sugar, rollback_at, &trace_path);
    assert_eq!(trace.len(), n);
    let legacy_bytes = legacy.checkpoint(&host);

    // 3a. The world, through the environment worker, driven by the trace's masks and boundary
    // actions and held to its frame, WRAM and slot digests.
    let fly_trace = FlyTrace::read(&trace_path, None).expect("the trace as the world reads it");
    let env_script = fly_trace.script("state-02", Arc::new(start.to_vec()));
    let mut env = env_rig(&root.path().join("env"), rom_path, &halves.world.rom_digest).await;
    let (records, world_bytes) = world_on_worker(&mut env, &halves, &env_script.ops).await;
    let world_values = fly_trace.check(&records).expect("the world against FLY_TRACE");

    // 3b. The agent, seeded through the shipped import and held to the trace.
    let mut agents = agent_rig(&root.path().join("agent"), dataset_dir.clone(), profile, &script.macro_channels).await;
    let (agent_records, agent_bytes) = legacy_parity::run_on_worker_from_then_capture(
        &mut agents,
        &id("fly"),
        &script,
        Some(&halves.agent),
    )
    .await
    .expect("the agent runs");
    let check = legacy_parity::check_against_trace(&script, &agent_records, &trace)
        .expect("the agent against FLY_TRACE");

    // 4. The session runtime's export at the last boundary is the legacy loop's checkpoint.
    let session_bytes =
        lc::export(&agent_bytes, &world_bytes, &slot(), &legacy.task(), &host).expect("the export");
    assert!(
        session_bytes == legacy_bytes,
        "session export vs legacy checkpoint after {n} frames: {}",
        first_difference(&legacy_bytes, &session_bytes)
    );

    // 5a. session -> legacy: a fresh legacy loop restores the export and checkpoints it again.
    let exported = store::decode(&session_bytes).expect("the legacy loop reads the export");
    let (mut fresh, _) = legacy_world(&rom, data, mode, None);
    {
        let mut frame = std::mem::replace(&mut fresh.frame, LegacyFrame::new());
        frame.restore(&mut fresh.parts(), &exported).expect("the legacy loop restores the export");
        fresh.frame = frame;
    }
    let host_after = host_of(&exported.runtime, exported.runtime.generation);
    assert_eq!(fresh.checkpoint(&host_after), session_bytes, "session -> legacy is exact");

    // 5b. -> session: a fresh environment and a fresh agent import it and export it again.
    let again = lc::split(&session_bytes, &id(EPISODE), &slot(), legacy_env::DEFAULT_AUDIO_RATE)
        .expect("the halves again");
    env.replace().await.expect("a fresh environment worker");
    let (_, world_again) = world_on_worker(&mut env, &again, &[]).await;
    agents.replace(&id("fly")).await.expect("a fresh agent worker");
    // The import installs the frame on screen: the export's own.
    let one_step = LegacyScript {
        frames: Arc::new(vec![again.world.framebuffer.clone()]),
        initial_frame: 0,
        ..script.clone_shallow()
    };
    let (_, agent_again) = legacy_parity::run_on_worker_from_then_capture(
        &mut agents,
        &id("fly"),
        &one_step,
        Some(&again.agent),
    )
    .await
    .expect("the agent imports the export");
    let round = lc::export(&agent_again, &world_again, &slot(), &again.task, &again.host)
        .expect("the export again");
    assert!(
        round == session_bytes,
        "session -> legacy -> session: {}",
        first_difference(&session_bytes, &round)
    );
    env.stop().await;
    agents.stop().await;

    Proof {
        transitions: check.transitions,
        world_values,
        rollbacks: check.rollbacks,
        sugar: check.sugar,
        bytes: session_bytes.len(),
    }
}

trait CloneShallow {
    fn clone_shallow(&self) -> LegacyScript;
}

impl CloneShallow for LegacyScript {
    fn clone_shallow(&self) -> LegacyScript {
        LegacyScript {
            name: self.name.clone(),
            macro_channels: self.macro_channels.clone(),
            frames: Arc::clone(&self.frames),
            initial_frame: self.initial_frame,
            initial_context: self.initial_context.clone(),
            steps: Vec::new(),
            checkpoints: Default::default(),
        }
    }
}

fn report(what: &str, n: usize, proof: &Proof) {
    eprintln!(
        "{what}: {n} frames restored from the store and run on; world {} values and agent {} \
         transitions identical to FLY_TRACE ({} sugar, {} rollbacks); the {}-byte FLYSIM01 export \
         equals the legacy checkpoint of the same boundary and survives session -> legacy -> \
         session unchanged",
        proof.world_values, proof.transitions, proof.sugar, proof.rollbacks, proof.bytes
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toy_restore_then_continue() {
    let Some(rom_path) = rom_path() else { return };
    let rom = std::fs::read(&rom_path).expect("the ROM reads");
    let data = Arc::new(load_brain_dataset_from_dir(toy::dir()).expect("the toy dataset"));
    // A toy fly warmed up from power-on and run a while by the legacy frame: the checkpoint the
    // legacy loop would write of it.
    let (mut warm, _) = legacy_world(&rom, &data, MacroMode::Raw, None);
    warm.frame.initialize(&mut warm.emulator, &mut warm.agent).expect("the fresh start");
    for _ in 0..120 {
        let mut frame = std::mem::replace(&mut warm.frame, LegacyFrame::new());
        let mut parts = warm.parts();
        let transition = frame.transition(&mut parts, &mut ()).expect("a frame");
        frame.boundary(&mut parts, &transition.evaluated.progress, transition.ms).expect("a boundary");
        warm.frame = frame;
    }
    let start = warm.checkpoint(&HostHalf {
        generation: 7,
        wall_ms: 1_790_000_000_000,
        compatibility: "toy/compat".to_owned(),
        speed: 1.0,
        rank_since_ms: 0.0,
        last_event_id: 3,
    });
    let n = frames(600);
    let proof = restore_then_continue(
        &rom_path,
        &data,
        toy::dir(),
        LegacyProfileKind::Toy,
        &start,
        MacroMode::Raw,
        n,
        &[(20, 300.0), (n / 2, 500.0)],
        None,
    )
    .await;
    report("toy raw", n, &proof);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fafb_restore_then_continue() {
    let Some(rom_path) = rom_path() else { return };
    if std::env::var_os("FLY_STATE02_FAFB").is_none() {
        eprintln!("skipping: FLY_STATE02_FAFB is not set");
        return;
    }
    let Some(dataset_dir) = legacy_parity::fafb_dir() else {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        return;
    };
    let Some(path) = std::env::var_os("FLY_STATE02_CHECKPOINT")
        .or_else(|| std::env::var_os("FLY_ROW67_CHECKPOINT"))
    else {
        eprintln!("skipping: neither FLY_STATE02_CHECKPOINT nor FLY_ROW67_CHECKPOINT is set");
        return;
    };
    let start = std::fs::read(&path).expect("the checkpoint reads");
    let data = Arc::new(load_brain_dataset_from_dir(&dataset_dir).expect("the dataset"));
    let n = frames(300);
    let started = std::time::Instant::now();
    let proof = restore_then_continue(
        &rom_path,
        &data,
        dataset_dir,
        LegacyProfileKind::Production,
        &start,
        MacroMode::Macros,
        n,
        &[(10, 400.0), (n / 2, 250.0)],
        Some(n / 3),
    )
    .await;
    report(
        &format!("FAFB macros from {} ({:?})", Path::new(&path).display(), started.elapsed()),
        n,
        &proof,
    );
}
