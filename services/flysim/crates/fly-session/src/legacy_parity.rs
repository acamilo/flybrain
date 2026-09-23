//! AGENT-01's parity harness: recorded inputs, a reference that drives [`NeuralAgent`] directly
//! in the legacy frame order, the same inputs driven through a launched [`LegacyAgentWorker`]
//! over the bus, and a record-by-record comparison.
//!
//! ```text
//! LegacyScript ──> DirectReference (NeuralAgent in Sim::step_frame order) ──> Vec<ParityRecord>
//!              ──> AgentDriver (Agent.* RPCs to a launched worker)        ──> Vec<ParityRecord>
//!                                                                          └─ compare() ─┘
//! ```
//!
//! A [`ParityRecord`] carries only what both sides can observe of the agent: the ticks, the
//! brain clock and its exact remainder, the decision, the whole telemetry, the digest of the
//! transition's spike bitset and, at declared checkpoints, the digest of the full agent state
//! (`agent_to_chunks`). A state digest is exact equality of every membrane, trace, gain, rate,
//! RNG and decoder field.
//!
//! [`ReferenceSource`] is the seam for FND-01: a reader of `LegacyFrame`'s `FLY_TRACE` is a
//! second reference beside [`DirectReference`], producing the same records from the running
//! loop instead of from a transcription of it. Nothing here depends on it.
//!
//! [`toy`] is the committed toy connectome the goldens run on; the real dataset is optional.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fly_session_types::extensions::{
    AgentRollbackParams, AgentRollbackResult, METHOD_AGENT_ROLLBACK, ROLLBACK_POLICY,
};
use fly_session_types::gameboy::{self, ChannelsDecision, Location, ReadoutContext};
use flybrain_core::agent::{AgentConfig as CoreAgentConfig, NeuralAgent};
use flybrain_core::dataset::BrainDataset;
use flybrain_core::decoder::gameboy::{gameboy_decoder_config_with_macros, to_button_mask};
use flybrain_core::envelope::{agent_from_chunks, agent_to_chunks, decode_envelope};
use flybrain_core::rng::Xorshift32;
use flybus::{Client, Grants, Pattern, Policy, Router, RouterConfig};
use serde_json::{Value, json};

use crate::launcher::{
    ExecutionMode, Launcher, LegacyAgentLaunch, SUPERVISOR_CLIENT, ThreadBudget, Via,
};
use crate::legacy_agent::{
    LegacyAgentProfile, LegacyProfileKind, PAYLOAD_MAGIC, SPIKES_ATTACHMENT, channels_decision,
    legacy_remainder_to_rational, spike_bitset, telemetry_of,
};
use crate::rpc::{Serials, WorkerRef, call};
use crate::types::*;

/// The frame size: 160 x 144 RGBA8.
pub const FRAME_BYTES: usize = (gameboy::VIEW_WIDTH * gameboy::VIEW_HEIGHT * 4) as usize;
/// The legacy default seed. The profile's kernel version pins it (`lif-1ms-f64-v2` hashes the
/// seed into any other value), so it is the only seed this profile accepts.
pub const LEGACY_SEED: i32 = 22_222;

// -------------------------------------------------------------------------------------------
// The toy connectome

pub mod toy {
    //! The committed toy connectome, `fixtures/legacy-toy`: 4,096 neurons, sized as the
    //! oracle's synthetic connectome is (`packages/brain/tools/golden.ts`) so that 300 noise
    //! kicks leave the network sub-threshold and the connectome drives it, with the roles the
    //! legacy readout and plasticity read -- `command_0..7`, `kenyon`, `mbon`, `reward_pam` --
    //! and four `macro_*` populations merged after the fingerprint, as FAFB's are.

    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use flybrain_core::rng::Xorshift32;
    use serde_json::{Map, Value, json};

    /// The macro channels of the toy composition, in composition order.
    pub const MACRO_CHANNELS: [&str; 4] =
        ["macro_go_out", "macro_talk", "macro_menu", "macro_next"];
    pub const NEURONS: u32 = 4_096;
    const EDGES_PER_NEURON: u32 = 10;
    const SEED: i32 = 20_260_923;

    /// `services/flysim/crates/fly-session/fixtures/legacy-toy`.
    pub fn dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/legacy-toy")
    }

    fn range(from: u32, to: u32) -> Vec<u32> {
        (from..to).collect()
    }

    fn gz(bytes: &[u8]) -> Vec<u8> {
        // flate2's default header has no name and a zero mtime, so the bytes are reproducible.
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(bytes).expect("in memory");
        encoder.finish().expect("in memory")
    }

    /// Every file of the toy dataset, by name, as it is committed.
    pub fn files() -> Vec<(String, Vec<u8>)> {
        let mut random = Xorshift32::new(SEED);
        let kenyon = (0u32, 1_024u32);
        let mbon = (1_024u32, 1_280u32);
        let visual = range(1_600, 2_400);

        let mut roles = Map::new();
        for command in 0..8u32 {
            roles.insert(
                format!("command_{command}"),
                json!(range(1_344 + command * 32, 1_376 + command * 32)),
            );
        }
        roles.insert("reward_pam".to_owned(), json!(range(1_280, 1_344)));
        roles.insert("steer_left".to_owned(), json!(range(2_400, 2_432)));
        roles.insert("steer_right".to_owned(), json!(range(2_432, 2_464)));
        roles.insert("visual_l1".to_owned(), json!(visual));

        let mut indptr = Vec::with_capacity(NEURONS as usize + 1);
        let mut targets: Vec<u32> = Vec::new();
        let mut weights: Vec<i16> = Vec::new();
        for source in 0..NEURONS {
            indptr.push(targets.len() as u32);
            let is_kenyon = source >= kenyon.0 && source < kenyon.1;
            for edge in 0..EDGES_PER_NEURON {
                let forced = is_kenyon && edge < 4;
                let target = if forced {
                    mbon.0 + (random.next_f64() * f64::from(mbon.1 - mbon.0)).floor() as u32
                } else {
                    (random.next_f64() * f64::from(NEURONS)).floor() as u32
                };
                let magnitude = 1 + (random.next_f64() * 40.0).floor() as i16;
                targets.push(target);
                weights.push(if !forced && random.next_f64() < 0.2 {
                    -magnitude
                } else {
                    magnitude
                });
            }
        }
        indptr.push(targets.len() as u32);

        let mut visual_xy = Vec::with_capacity(visual.len() * 2);
        for column in 0..visual.len() {
            visual_xy.push((column % 40) as f32 * 7.5);
            visual_xy.push((column / 40) as f32 * 5.25);
        }
        let hemisphere: Vec<u8> = (0..visual.len()).map(|c| (c % 2) as u8).collect();

        let meta = json!({
            "schemaVersion": 1,
            "dataset": "legacy-toy",
            "neurons": NEURONS,
            "edges": targets.len(),
            "roles": Value::Object(roles),
            "visual": {"population": "toy-retina", "count": visual.len()},
        });
        let circuits = json!({
            "neurons": NEURONS,
            "roles": {
                "kenyon": range(kenyon.0, kenyon.1),
                "mbon": range(mbon.0, mbon.1),
                // Relabelled MBONs, outside the fingerprint, as the FAFB macro populations are.
                "macro_go_out": range(1_100, 1_116),
                "macro_talk": range(1_116, 1_132),
                "macro_menu": range(1_132, 1_148),
                "macro_next": range(1_148, 1_164),
            },
        });
        let le32 = |values: &[u32]| {
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<u8>>()
        };
        let text = |value: &Value| {
            let mut s = serde_json::to_string(value).expect("json");
            s.push('\n');
            s.into_bytes()
        };
        vec![
            ("meta.json".to_owned(), text(&meta)),
            ("circuit-roles.json".to_owned(), text(&circuits)),
            ("indptr.binz".to_owned(), gz(&le32(&indptr))),
            ("targets.binz".to_owned(), gz(&le32(&targets))),
            (
                "weights.binz".to_owned(),
                gz(&weights
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<u8>>()),
            ),
            ("visual-indices.binz".to_owned(), gz(&le32(&visual))),
            ("visual-hemisphere.binz".to_owned(), gz(&hemisphere)),
            (
                "visual-xy.binz".to_owned(),
                gz(&visual_xy
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<u8>>()),
            ),
        ]
    }

    /// Writes the dataset into `dir`.
    pub fn write(dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        for (name, bytes) in files() {
            std::fs::write(dir.join(name), bytes)?;
        }
        Ok(())
    }
}

/// `<repo>/data/fafb-v783`, or `None` where the dataset is not checked out.
pub fn fafb_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../data/fafb-v783");
    dir.join("meta.json").is_file().then_some(dir)
}

// -------------------------------------------------------------------------------------------
// Recorded inputs

/// One reward event of the legacy adapter: a value for `reinforce` and a pulse for
/// `stimulate`. The task turns it into one `Reward` and one `reward-pulse` `Stimulus`.
#[derive(Clone, Debug, PartialEq)]
pub struct RewardEvent {
    pub value: f64,
    pub stimulation_ms: f64,
}

/// One step of a recorded run.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptStep {
    /// One transition: sugar admitted before it, the frame the environment produced, the
    /// adapter's reward events, and the next decision context.
    Frame {
        sugar: Vec<f64>,
        frame: usize,
        rewards: Vec<RewardEvent>,
        next_context: ReadoutContext,
    },
    /// `legacy-ratchet-rollback-v1` at the boundary just reached: the slot's archived frame and
    /// the context the executor's observation of the restored world produced.
    Rollback {
        frame: usize,
        context: ReadoutContext,
    },
    /// A group restore at the boundary just reached into a fresh agent
    /// (`legacy-transient-reset`): a FLYSIM01-style load into a new process.
    Restore,
}

/// A recorded run: the macro channels, the frame pool, the first frame and context, and the
/// steps. The frames are the environment's output, so they are inputs here.
#[derive(Clone, Debug)]
pub struct LegacyScript {
    pub name: String,
    pub macro_channels: Vec<String>,
    pub frames: Arc<Vec<Vec<u8>>>,
    pub initial_frame: usize,
    pub initial_context: ReadoutContext,
    pub steps: Vec<ScriptStep>,
    /// Step indices after which the full agent state is compared.
    pub checkpoints: BTreeSet<usize>,
}

/// A deterministic pool of frames with distinct overall brightness, so the retina drive differs
/// from frame to frame and the connectome has something to respond to.
pub fn frame_pool(count: usize, seed: i32) -> Vec<Vec<u8>> {
    let mut random = Xorshift32::new(seed);
    (0..count)
        .map(|index| {
            let scale = (index + 1) as f64 / count as f64;
            (0..FRAME_BYTES)
                .map(|byte| {
                    if byte % 4 == 3 {
                        255
                    } else {
                        (random.next_f64() * 256.0 * scale).floor() as u8
                    }
                })
                .collect()
        })
        .collect()
}

fn context(boot: bool, bound: &[&str], location: Option<(u32, u32, u32)>) -> ReadoutContext {
    ReadoutContext {
        boot,
        bound: bound.iter().map(|b| (*b).to_owned()).collect(),
        location: location.map(|(area, x, y)| Location { area, x, y }),
    }
}

/// The macros-mode toy scenario: boot and play, a still location long enough for the blocked
/// rule, a battle (no location), sugar overlapping a pulse, reward events on a schedule with a
/// zero-value one, a rollback, a restore into a fresh worker and play after both.
pub fn toy_macros_script(frames: usize) -> LegacyScript {
    let channels: Vec<String> = toy::MACRO_CHANNELS
        .iter()
        .map(|c| (*c).to_owned())
        .collect();
    let bound_at = |k: usize| -> Vec<&'static str> {
        match k {
            0..60 => vec![],
            60..130 => vec!["macro_go_out", "macro_talk"],
            130..200 => vec!["macro_talk", "macro_menu", "macro_next"],
            _ => vec!["macro_go_out", "macro_menu"],
        }
    };
    let location_at = |k: usize| -> Option<(u32, u32, u32)> {
        match k {
            0..20 => None,
            20..90 => Some((1, 5, 5)),
            90..96 => None,
            _ => Some((1, 5 + (k / 30) as u32, 5)),
        }
    };
    let mut steps = Vec::new();
    for k in 1..=frames {
        let sugar = match k {
            25 => vec![300.0],
            26 => vec![100.0],
            170 => vec![500.0, 40.0],
            _ => vec![],
        };
        let mut rewards = Vec::new();
        if k % 17 == 0 {
            rewards.push(RewardEvent {
                value: 0.4,
                stimulation_ms: 120.0,
            });
        }
        if k == 50 {
            rewards.push(RewardEvent {
                value: 0.25,
                stimulation_ms: 80.0,
            });
            rewards.push(RewardEvent {
                value: 0.5,
                stimulation_ms: 200.0,
            });
        }
        if k == 77 {
            rewards.push(RewardEvent {
                value: 0.0,
                stimulation_ms: 60.0,
            });
        }
        steps.push(ScriptStep::Frame {
            sugar,
            frame: (k * 3) % 8,
            rewards,
            next_context: context(k < 40, &bound_at(k), location_at(k)),
        });
        if k == 120 {
            steps.push(ScriptStep::Rollback {
                frame: 5,
                context: context(false, &bound_at(k), Some((2, 3, 4))),
            });
        }
        if k == 190 {
            steps.push(ScriptStep::Restore);
        }
    }
    let checkpoints: BTreeSet<usize> = steps
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 40 == 39)
        .map(|(i, _)| i)
        .chain([steps.len() - 1])
        .collect();
    LegacyScript {
        name: "toy-macros".to_owned(),
        macro_channels: channels,
        frames: Arc::new(frame_pool(8, 4_242)),
        initial_frame: 0,
        initial_context: context(true, &[], None),
        steps,
        checkpoints,
    }
}

/// The raw-mode toy scenario: no macro group, a still location, rewards, a rollback.
pub fn toy_raw_script(frames: usize) -> LegacyScript {
    let mut steps = Vec::new();
    for k in 1..=frames {
        steps.push(ScriptStep::Frame {
            sugar: if k == 10 { vec![250.0] } else { vec![] },
            frame: (k * 5) % 8,
            rewards: if k % 13 == 0 {
                vec![RewardEvent {
                    value: 0.3,
                    stimulation_ms: 120.0,
                }]
            } else {
                vec![]
            },
            next_context: context(k < 30, &[], if k > 5 { Some((7, 1, 1)) } else { None }),
        });
        if k == 60 {
            steps.push(ScriptStep::Rollback {
                frame: 2,
                context: context(false, &[], None),
            });
        }
    }
    let last = steps.len() - 1;
    LegacyScript {
        name: "toy-raw".to_owned(),
        macro_channels: Vec::new(),
        frames: Arc::new(frame_pool(8, 99)),
        initial_frame: 1,
        initial_context: context(true, &[], None),
        steps,
        checkpoints: [29, last].into_iter().collect(),
    }
}

// -------------------------------------------------------------------------------------------
// Records

/// What both sides can observe of one step.
#[derive(Clone, Debug, PartialEq)]
pub struct ParityRecord {
    /// 0 is Initialize; step `i` of the script is record `i + 1`.
    pub index: usize,
    pub kind: &'static str,
    /// The committed boundary after the step.
    pub boundary: u64,
    pub ticks: u64,
    pub brain_ticks: u64,
    pub remainder: RationalNs,
    pub decision: Option<ChannelsDecision>,
    pub telemetry: Option<AgentTelemetry>,
    /// SHA-256 of the transition's spike bitset.
    pub spikes: Option<Digest>,
    /// SHA-256 of the full agent state, at a checkpoint.
    pub state: Option<Digest>,
}

impl ParityRecord {
    pub fn to_json(&self) -> Value {
        json!({
            "index": self.index,
            "kind": self.kind,
            "boundary": self.boundary.to_string(),
            "ticks": self.ticks.to_string(),
            "brainTicks": self.brain_ticks.to_string(),
            "remainder": self.remainder.to_json(),
            "decision": self.decision.as_ref().map_or(Value::Null, DomainType::to_json),
            "telemetry": self.telemetry.as_ref().map_or(Value::Null, |t| t.to_json()),
            "spikes": self.spikes,
            "state": self.state,
        })
    }

    pub fn digest(&self) -> Digest {
        digest_of(&self.to_json()).expect("a record canonicalizes")
    }

    /// One readable line for a golden file: the fields a reviewer reads, then the digest of
    /// the whole record.
    pub fn row(&self) -> String {
        let (mask, macro_channel) = match &self.decision {
            Some(d) => (
                format!("{:02x}", d.mask()),
                d.macro_channel.clone().unwrap_or_else(|| "-".to_owned()),
            ),
            None => ("--".to_owned(), "-".to_owned()),
        };
        let pulse = self
            .telemetry
            .as_ref()
            .and_then(|t| t.stimulus_remaining_ms)
            .map_or("-".to_owned(), |ms| format!("{ms}"));
        format!(
            "{} {} b{} t{} bt{} m{} {} p{} {}{}",
            self.index,
            self.kind,
            self.boundary,
            self.ticks,
            self.brain_ticks,
            mask,
            macro_channel,
            pulse,
            &self.digest()[..16],
            if self.state.is_some() { " state" } else { "" }
        )
    }
}

/// The digest of a full agent state: every chunk and the manifest `agent_to_chunks` writes.
pub fn state_digest(state: &flybrain_core::agent::AgentState) -> Digest {
    let chunks = agent_to_chunks(state);
    let mut bytes = chunks.manifest.stringify().into_bytes();
    for (name, chunk) in &chunks.chunks {
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
        bytes.extend_from_slice(chunk);
    }
    digest_of_bytes(&bytes)
}

/// The state digest of a worker's capture payload.
pub fn payload_state_digest(payload: &[u8]) -> Result<Digest, String> {
    let parts = decode_envelope(payload, PAYLOAD_MAGIC).map_err(|e| e.to_string())?;
    let state = agent_from_chunks(&parts.manifest, &parts).map_err(|e| e.to_string())?;
    Ok(state_digest(&state))
}

/// The first record at which two runs differ, described.
pub fn compare(left: &[ParityRecord], right: &[ParityRecord]) -> Result<(), String> {
    for (a, b) in left.iter().zip(right) {
        if a != b {
            let mut fields = Vec::new();
            let (ja, jb) = (a.to_json(), b.to_json());
            for key in [
                "kind",
                "boundary",
                "ticks",
                "brainTicks",
                "remainder",
                "decision",
                "telemetry",
                "spikes",
                "state",
            ] {
                if ja[key] != jb[key] {
                    fields.push(format!("{key}: {} != {}", ja[key], jb[key]));
                }
            }
            return Err(format!(
                "record {} ({}) differs: {}",
                a.index,
                a.kind,
                fields.join("; ")
            ));
        }
    }
    if left.len() != right.len() {
        return Err(format!("{} records against {}", left.len(), right.len()));
    }
    Ok(())
}

/// A source of reference records for a script.
///
/// [`DirectReference`] is one. FND-01's `LegacyFrame` trace (`FLY_TRACE`) is meant to be the
/// other: it would produce the same records from the running legacy loop, and at that point the
/// transcription in [`DirectReference`] is itself checked against the service.
pub trait ReferenceSource {
    fn records(&mut self, script: &LegacyScript) -> Result<Vec<ParityRecord>, String>;
}

// -------------------------------------------------------------------------------------------
// The direct reference: NeuralAgent in `Sim::step_frame` order

/// `NeuralAgent` driven the way `flysim::simloop::Sim` drives it, transcribed line by line from
/// `fresh_start`, `step_frame`, `recover` and `try_restore`, with the emulator, adapter and
/// macro layer replaced by the script's recorded outputs.
pub struct DirectReference {
    dataset: Arc<BrainDataset>,
    agent: NeuralAgent,
    macro_channels: Vec<String>,
    // `Sim`'s own fields.
    remainder: f64,
    location: Option<Location>,
    held_channel: Option<String>,
    blocked_since_ms: f64,
    frame_buffer: Vec<u8>,
    /// What the adapter and the macro layer would report after the last frame.
    boot: bool,
    bound: Vec<String>,
    /// Reinforcement calls, for `learning.updates`.
    reinforcements: u64,
}

impl DirectReference {
    fn build(dataset: Arc<BrainDataset>, macro_channels: &[String]) -> Result<NeuralAgent, String> {
        let channels: Vec<&str> = macro_channels.iter().map(String::as_str).collect();
        let mut config =
            CoreAgentConfig::with_decoder(gameboy_decoder_config_with_macros(&channels));
        config.warmup_ms = gameboy::WARMUP_MS;
        NeuralAgent::new(dataset, config).map_err(|e| e.to_string())
    }

    /// `Sim::boot` with no checkpoint: `fresh_start` warms up on the setup frame.
    pub fn fresh(
        dataset: Arc<BrainDataset>,
        script: &LegacyScript,
    ) -> Result<(DirectReference, ParityRecord), String> {
        let mut agent = DirectReference::build(dataset.clone(), &script.macro_channels)?;
        let frame = script.frames[script.initial_frame].clone();
        agent.warmup(Some(&frame)).map_err(|e| e.to_string())?;
        let reference = DirectReference {
            dataset,
            agent,
            macro_channels: script.macro_channels.clone(),
            remainder: 0.0,
            location: None,
            held_channel: None,
            blocked_since_ms: 0.0,
            frame_buffer: frame,
            boot: script.initial_context.boot,
            bound: script.initial_context.bound.clone(),
            reinforcements: 0,
        };
        let record = reference.record(0, "initialize", 0, gameboy::WARMUP_MS, None, None);
        Ok((reference, record))
    }

    fn record(
        &self,
        index: usize,
        kind: &'static str,
        boundary: u64,
        ticks: u64,
        decision: Option<ChannelsDecision>,
        spikes: Option<Digest>,
    ) -> ParityRecord {
        ParityRecord {
            index,
            kind,
            boundary,
            ticks,
            brain_ticks: self.agent.network.ms as u64,
            remainder: legacy_remainder_to_rational(self.remainder).expect("a legacy remainder"),
            decision,
            telemetry: Some(telemetry_of(
                &self.agent,
                self.agent.network.ms as u64,
                self.reinforcements,
            )),
            spikes,
            state: None,
        }
    }

    /// The full state as `Sim::checkpoint` exports it: the loop owns the remainder.
    pub fn state_digest(&self) -> Digest {
        let mut state = self.agent.export_state();
        state.remainder = self.remainder;
        state_digest(&state)
    }

    /// `drain_commands` (sugar) and then `Sim::step_frame`'s agent half.
    fn frame(
        &mut self,
        sugar: &[f64],
        frame: &[u8],
        rewards: &[RewardEvent],
        next: &ReadoutContext,
    ) -> (u64, ChannelsDecision, Digest) {
        // Commands: `self.agent.network.stimulate(duration)`.
        for duration in sugar {
            self.agent.network.stimulate(*duration);
        }
        // 2. Step the brain.
        self.remainder += self.agent.ms_per_frame;
        let steps = self.remainder.floor();
        self.remainder -= steps;
        let since = self.agent.network.ms;
        self.agent.network.step(steps as u64);
        // 3. Decode.
        let boot = self.boot;
        let ms = self.agent.network.ms;
        let rates = self.agent.network.rates.clone();
        let blocked_ms = self.agent.decoder.blocked_ms();
        let blocked = (blocked_ms > 0.0 && ms - self.blocked_since_ms >= blocked_ms)
            .then(|| self.agent.decoder.current())
            .flatten()
            .map(str::to_string);
        let bound = (!self.macro_channels.is_empty()).then(|| self.bound.clone());
        let active =
            self.agent
                .decoder
                .decode_bound(&rates, ms, boot, blocked.as_deref(), bound.as_deref());
        let held = self.agent.decoder.current().map(str::to_string);
        if held != self.held_channel {
            self.held_channel = held;
            self.blocked_since_ms = ms;
        }
        let mask = to_button_mask(&active);
        let decision = channels_decision(&active, &self.bound);
        debug_assert_eq!(u32::from(decision.mask()), mask);
        // 5. Run a frame (recorded), 6. set the visual frame from it.
        self.frame_buffer.copy_from_slice(frame);
        let (width, height) = (self.agent.frame.width, self.agent.frame.height);
        self.agent
            .network
            .set_visual_frame(&self.frame_buffer, width, height);
        // 7. Sample rewards (recorded), 8. stimulate once per event, 9. reinforce with the sum.
        let ms = self.agent.network.ms;
        let mut total = 0.0;
        for event in rewards {
            self.agent.network.stimulate(event.stimulation_ms);
            total += event.value;
        }
        if self.agent.network.plasticity.enabled {
            self.agent.network.plasticity.reinforce(total, ms);
            self.reinforcements += 1;
        }
        // `observe` (recorded as the next context's bound) and the cooldown's other reset.
        self.boot = next.boot;
        self.bound = next.bound.clone();
        let location = next.location;
        if location.is_some() && location != self.location {
            self.location = location;
            self.blocked_since_ms = ms;
        }
        let spikes = digest_of_bytes(&spike_bitset(&self.agent.network.last_spike_ms, since, ms));
        (steps as u64, decision, spikes)
    }

    /// `Sim::recover`: `recover_game`'s neural half, then the loop's own transients.
    fn rollback(&mut self, frame: &[u8], context: &ReadoutContext) {
        let ms = self.agent.network.ms;
        self.agent.decoder.clear_holds(ms);
        let ms = self.agent.network.ms;
        self.agent.network.plasticity.clear_eligibility(ms);
        let (width, height) = (self.agent.frame.width, self.agent.frame.height);
        self.agent.network.set_visual_frame(frame, width, height);
        self.frame_buffer.copy_from_slice(frame);
        self.location = context.location;
        self.held_channel = None;
        self.blocked_since_ms = self.agent.network.ms;
        self.boot = context.boot;
        self.bound = context.bound.clone();
    }

    /// A service restart onto the last checkpoint: `Sim::boot`'s fresh fields, then
    /// `try_restore`'s agent half. The executor observes the restored world before the first
    /// frame, which is the context this reference already holds.
    fn restore(&mut self) -> Result<(), String> {
        let mut state = self.agent.export_state();
        state.remainder = self.remainder;
        let mut agent = DirectReference::build(self.dataset.clone(), &self.macro_channels)?;
        agent.import_state(&state).map_err(|e| e.to_string())?;
        self.agent = agent;
        self.remainder = state.remainder;
        let (width, height) = (self.agent.frame.width, self.agent.frame.height);
        self.agent
            .network
            .set_visual_frame(&self.frame_buffer, width, height);
        self.location = None;
        self.held_channel = None;
        self.blocked_since_ms = 0.0;
        Ok(())
    }
}

/// [`ReferenceSource`] over a dataset directory.
pub struct DirectSource {
    pub dataset: Arc<BrainDataset>,
}

impl ReferenceSource for DirectSource {
    fn records(&mut self, script: &LegacyScript) -> Result<Vec<ParityRecord>, String> {
        let (mut reference, first) = DirectReference::fresh(self.dataset.clone(), script)?;
        let mut records = vec![first];
        let mut boundary = 0u64;
        for (index, step) in script.steps.iter().enumerate() {
            let mut record = match step {
                ScriptStep::Frame {
                    sugar,
                    frame,
                    rewards,
                    next_context,
                } => {
                    let (ticks, decision, spikes) =
                        reference.frame(sugar, &script.frames[*frame], rewards, next_context);
                    boundary += 1;
                    reference.record(
                        index + 1,
                        "frame",
                        boundary,
                        ticks,
                        Some(decision),
                        Some(spikes),
                    )
                }
                ScriptStep::Rollback { frame, context } => {
                    reference.rollback(&script.frames[*frame], context);
                    reference.record(index + 1, "rollback", boundary, 0, None, None)
                }
                ScriptStep::Restore => {
                    reference.restore()?;
                    let mut record =
                        reference.record(index + 1, "restore", boundary, 0, None, None);
                    // An ActivateRestore reply carries no telemetry; the record says so.
                    record.telemetry = None;
                    record.state = Some(reference.state_digest());
                    record
                }
            };
            if script.checkpoints.contains(&index) {
                record.state = Some(reference.state_digest());
            }
            records.push(record);
        }
        Ok(records)
    }
}

// -------------------------------------------------------------------------------------------
// The worker side: a launched LegacyAgentWorker driven over the bus

const DRIVER_CLIENT: &str = "coordinator";
const MAX_GENERATIONS: u32 = 8;

fn agent_service(agent_id: &Id) -> String {
    format!("agent.{agent_id}")
}

fn agent_client(agent_id: &Id, generation: u32) -> String {
    if generation <= 1 {
        format!("worker-{agent_id}")
    } else {
        format!("worker-{agent_id}-r{generation}")
    }
}

fn grants(f: impl FnOnce(&mut Grants)) -> Grants {
    let mut g = Grants::default();
    f(&mut g);
    g
}

/// One legacy agent the rig launches.
#[derive(Clone, Debug)]
pub struct RigAgent {
    pub agent_id: Id,
    pub port_id: Id,
    pub dataset_dir: PathBuf,
    pub profile: LegacyProfileKind,
    pub macro_channels: Vec<String>,
    pub worker_threads: usize,
}

/// A router, a launcher in one execution mode and the legacy agents it started: the smallest
/// composition that runs `LegacyAgentWorker` under `fly-session`'s own supervisor. The
/// coordinator's part is played by [`AgentDriver`]s on one bus client.
pub struct LegacyRig {
    pub launcher: Launcher,
    pub client: Client,
    pub session_id: Id,
    pub mode: ExecutionMode,
    agents: BTreeMap<Id, (RigAgent, u32)>,
}

impl LegacyRig {
    pub async fn start(
        root: &Path,
        mode: ExecutionMode,
        agents: &[RigAgent],
    ) -> Result<LegacyRig, String> {
        let session_id = id("legacy");
        let store_root = root.join("store");
        let sockets = root.join("sockets");
        std::fs::create_dir_all(&sockets).map_err(|e| format!("sockets: {e}"))?;
        let mut policy = Policy::closed()
            .client(
                DRIVER_CLIENT,
                grants(|g| g.call = vec![Pattern::prefix("agent.")]),
            )
            .client(
                SUPERVISOR_CLIENT,
                grants(|g| g.call = vec![Pattern::prefix("agent.")]),
            );
        for agent in agents {
            let service = agent_service(&agent.agent_id);
            for generation in 1..=MAX_GENERATIONS {
                policy = policy.client(
                    &agent_client(&agent.agent_id, generation),
                    grants(|g| g.register = vec![Pattern::exact(&service)]),
                );
            }
        }
        let mut config = RouterConfig::new(&store_root);
        config.policy = policy;
        let router = Router::new(config).map_err(|e| format!("router: {e}"))?;
        let total = 1 + agents.iter().map(|a| a.worker_threads).sum::<usize>();
        let budget = ThreadBudget::new(total, 1).map_err(|e| e.message)?;
        let launcher = Launcher::start(router, mode, Via::Unix, &store_root, &sockets, budget)
            .await
            .map_err(|e| format!("launcher: {}", e.message))?;
        let client = launcher
            .connect(DRIVER_CLIENT)
            .await
            .map_err(|e| format!("connect: {}", e.message))?;
        let mut rig = LegacyRig {
            launcher,
            client,
            session_id,
            mode,
            agents: BTreeMap::new(),
        };
        for agent in agents {
            rig.agents
                .insert(agent.agent_id.clone(), (agent.clone(), 0));
            rig.launch(&agent.agent_id).await?;
        }
        Ok(rig)
    }

    async fn launch(&mut self, agent_id: &Id) -> Result<WorkerRef, String> {
        let (agent, generation) = self.agents.get_mut(agent_id).ok_or("unknown agent")?;
        *generation += 1;
        let spec = LegacyAgentLaunch {
            session_id: self.session_id.clone(),
            agent_id: agent.agent_id.clone(),
            port_id: agent.port_id.clone(),
            incarnation_id: parse_id(&format!("{}-inc-{generation}", agent.agent_id))?,
            worker_threads: agent.worker_threads,
            dataset_dir: agent.dataset_dir.clone(),
            profile: agent.profile,
            macro_channels: agent.macro_channels.clone(),
            client_id: agent_client(&agent.agent_id, *generation),
            service: agent_service(&agent.agent_id),
        };
        self.launcher
            .launch_legacy_agent(spec)
            .await
            .map_err(|e| e.message)?;
        Ok(self.worker_ref(agent_id))
    }

    pub fn worker_ref(&self, agent_id: &Id) -> WorkerRef {
        self.launcher
            .worker(agent_id)
            .expect("launched")
            .worker_ref()
    }

    /// Stops an agent's worker and starts its replacement: a new process in process mode, with
    /// a new incarnation and a new client identity, uninitialized.
    pub async fn replace(&mut self, agent_id: &Id) -> Result<WorkerRef, String> {
        self.launcher.reap(agent_id, &id("replaced")).await;
        self.launch(agent_id).await
    }

    pub async fn stop(mut self) {
        self.launcher.reap_all(&id("done")).await;
    }

    /// A driver for one agent, playing the coordinator's part.
    pub fn driver(&self, agent_id: &Id, script: &LegacyScript) -> AgentDriver {
        let (agent, _) = &self.agents[agent_id];
        AgentDriver {
            client: self.client.clone(),
            target: self.worker_ref(agent_id),
            session_id: self.session_id.clone(),
            agent_id: agent_id.clone(),
            profile: agent.profile.profile(),
            epoch_serial: 1,
            boundary: 0,
            serials: Serials::default(),
            context: script.initial_context.to_typed(),
            frames: script.frames.clone(),
            captures: 0,
            remainder: RationalNs::ZERO,
        }
    }
}

fn domain(e: DomainError) -> String {
    format!("{:?}: {}", e.code, e.message)
}

/// The coordinator's side of one agent: scopes, request serials, frame artifacts and the
/// context it last sent.
pub struct AgentDriver {
    client: Client,
    target: WorkerRef,
    session_id: Id,
    agent_id: Id,
    profile: LegacyAgentProfile,
    epoch_serial: u32,
    boundary: u64,
    serials: Serials,
    context: TypedValue,
    frames: Arc<Vec<Vec<u8>>>,
    captures: u64,
    /// The remainder the last Prepare reported: a rollback keeps it.
    remainder: RationalNs,
}

/// A captured payload, held for a restore.
pub struct Captured {
    pub result: CaptureResult,
    pub artifact: flybus::Artifact,
    pub bytes: Vec<u8>,
    pub scope: Scope,
}

impl AgentDriver {
    fn epoch(&self) -> Id {
        id(&format!("e{}", self.epoch_serial))
    }

    fn scope(&self) -> Scope {
        scope_at(&self.session_id, &self.epoch(), self.boundary)
    }

    async fn frame_input(
        &self,
        frame: usize,
        boundary: u64,
    ) -> Result<(SensoryInput, flybus::Artifact), String> {
        let artifact = crate::media::seal_copy(
            &self.client,
            crate::media::FRAME_CONTENT_TYPE.to_owned(),
            &self.frames[frame],
        )
        .await
        .map_err(domain)?;
        let input = SensoryInput {
            boundary,
            views: vec![ViewRef {
                view_id: gameboy::VIEW_ID.to_owned(),
                produced_step: boundary,
                pixels: artifact.reference().clone(),
            }],
            structured: None,
        };
        Ok((input, artifact))
    }

    async fn call(
        &mut self,
        method: &str,
        scope: Option<Scope>,
        params: Value,
        attachments: &[(&str, &flybus::Artifact)],
        want: &[&str],
    ) -> Result<crate::rpc::DomainReply, String> {
        let request_id = self.serials.next(&self.target.service);
        let want: Vec<String> = want.iter().map(|w| (*w).to_owned()).collect();
        let reply = call(
            &self.client,
            &self.target,
            method,
            scope,
            object(params),
            attachments,
            request_id,
            &want,
        )
        .await
        .map_err(|e| format!("{method}: {}", domain(e)))?;
        reply
            .result()
            .map_err(|e| format!("{method}: {}", domain(e)))?;
        Ok(reply)
    }

    /// One call with any params, its domain outcome returned as it stands: for tests that
    /// need the worker to refuse something.
    pub async fn call_raw(
        &mut self,
        method: &str,
        scope: Option<Scope>,
        params: Value,
        attachments: &[(&str, &flybus::Artifact)],
    ) -> Result<crate::rpc::DomainReply, DomainError> {
        let request_id = self.serials.next(&self.target.service);
        call(
            &self.client,
            &self.target,
            method,
            scope,
            object(params),
            attachments,
            request_id,
            &[],
        )
        .await
    }

    /// The same request again, same id and body: a domain retry.
    pub async fn retry_raw(
        &mut self,
        request_id: DomainRequestId,
        method: &str,
        scope: Option<Scope>,
        params: Value,
    ) -> Result<crate::rpc::DomainReply, DomainError> {
        call(
            &self.client,
            &self.target,
            method,
            scope,
            object(params),
            &[],
            request_id,
            &[],
        )
        .await
    }

    /// A sealed frame from the script's pool, as a sensory input observing `boundary`.
    pub async fn input(
        &self,
        frame: usize,
        boundary: u64,
    ) -> Result<(SensoryInput, flybus::Artifact), String> {
        self.frame_input(frame, boundary).await
    }

    /// The scope of the committed boundary this driver is at.
    pub fn current_scope(&self) -> Scope {
        self.scope()
    }

    /// Moves the driver to the next epoch without telling the worker, to address it wrongly.
    pub fn bump_epoch(&mut self) {
        self.epoch_serial += 1;
    }

    pub fn context(&self) -> &TypedValue {
        &self.context
    }

    pub fn profile(&self) -> &AssetRef {
        &self.profile.asset
    }

    async fn acknowledge(&mut self, request_id: DomainRequestId) -> Result<(), String> {
        let params = AcknowledgeParams {
            request_ids: vec![request_id],
        };
        self.call("Worker.Acknowledge", None, params.to_json(), &[], &[])
            .await
            .map(|_| ())
    }

    pub async fn initialize(&mut self, initial_frame: usize) -> Result<ParityRecord, String> {
        let (input, artifact) = self.frame_input(initial_frame, 0).await?;
        let params = AgentInitializeParams {
            agent_id: self.agent_id.clone(),
            profile: self.profile.asset.clone(),
            seed: LEGACY_SEED,
            initial_input: input,
            initial_decision_context: self.context.clone(),
            worker_threads: 1,
        };
        let reply = self
            .call(
                "Agent.Initialize",
                Some(self.scope()),
                params.to_json(),
                &[("view.lcd", &artifact)],
                &[],
            )
            .await?;
        let result: AgentInitializeResult = reply.parse().map_err(domain)?;
        if result
            .telemetry
            .rates
            .iter()
            .map(|r| r.role_id.as_str())
            .ne(result.graph.rate_roles.iter().map(String::as_str))
        {
            return Err("telemetry rates are not in AgentGraph.rateRoles order".to_owned());
        }
        self.acknowledge(reply.request_id).await?;
        Ok(ParityRecord {
            index: 0,
            kind: "initialize",
            boundary: 0,
            ticks: result.warmup_ticks,
            brain_ticks: result.telemetry.brain_ticks,
            remainder: RationalNs::ZERO,
            decision: None,
            telemetry: Some(result.telemetry),
            spikes: None,
            state: None,
        })
    }

    /// Prepare then Commit: one transition.
    pub async fn frame(
        &mut self,
        index: usize,
        sugar: &[f64],
        frame: usize,
        rewards: &[RewardEvent],
        next_context: &ReadoutContext,
    ) -> Result<ParityRecord, String> {
        let k = self.boundary;
        let prepare = PrepareParams {
            agent_id: self.agent_id.clone(),
            profile_digest: self.profile.asset.digest.clone(),
            interval: gameboy::step_duration(),
            decision_context_digest: self.context.digest(),
            pre_step_stimulations: sugar
                .iter()
                .enumerate()
                .map(|(n, ms)| Stimulus {
                    id: id(&format!("sugar-{k}-{n}")),
                    kind_id: gameboy::STIMULUS_REWARD_PULSE.to_owned(),
                    duration_ms: *ms,
                })
                .collect(),
        };
        let reply = self
            .call(
                "Agent.Prepare",
                Some(self.scope()),
                prepare.to_json(),
                &[],
                &[],
            )
            .await?;
        let prepared: PreparedDecision = reply.parse().map_err(domain)?;
        let decision = ChannelsDecision::from_typed(&prepared.decision).map_err(|e| e.0)?;

        let (input, artifact) = self.frame_input(frame, k + 1).await?;
        let next = next_context.to_typed();
        let commit = CommitParams {
            agent_id: self.agent_id.clone(),
            prepared_request_id: reply.request_id.clone(),
            next_input: input,
            next_decision_context: next.clone(),
            rewards: rewards
                .iter()
                .enumerate()
                .map(|(n, e)| Reward {
                    event_id: id(&format!("ev-{k}-{n}")),
                    rule_id: id("toy"),
                    value: e.value,
                })
                .collect(),
            task_stimulations: rewards
                .iter()
                .enumerate()
                .map(|(n, e)| Stimulus {
                    id: id(&format!("ev-{k}-{n}")),
                    kind_id: gameboy::STIMULUS_REWARD_PULSE.to_owned(),
                    duration_ms: e.stimulation_ms,
                })
                .collect(),
        };
        let reply = self
            .call(
                "Agent.Commit",
                Some(self.scope()),
                commit.to_json(),
                &[("view.lcd", &artifact)],
                &[SPIKES_ATTACHMENT],
            )
            .await?;
        let result: AgentCommitResult = reply.parse().map_err(domain)?;
        if result.committed_step != k + 1 || result.decision_context_digest != next.digest() {
            return Err(format!(
                "Agent.Commit acknowledged {} with another context",
                result.committed_step
            ));
        }
        let spikes = match reply.artifacts.get(SPIKES_ATTACHMENT) {
            Some(artifact) => digest_of_bytes(&artifact.read_all().await.map_err(|e| e.message)?),
            None => return Err("Agent.Commit carried no spike bitset".to_owned()),
        };
        self.context = next;
        self.boundary = k + 1;
        self.remainder = prepared.remainder;
        Ok(ParityRecord {
            index,
            kind: "frame",
            boundary: k + 1,
            ticks: prepared.ticks_advanced,
            brain_ticks: prepared.brain_ticks,
            remainder: prepared.remainder,
            decision: Some(decision),
            telemetry: Some(result.telemetry),
            spikes: Some(spikes),
            state: None,
        })
    }

    /// `legacy-ratchet-rollback-v1`'s agent half, under a new epoch at the same boundary.
    pub async fn rollback(
        &mut self,
        index: usize,
        frame: usize,
        context: &ReadoutContext,
    ) -> Result<ParityRecord, String> {
        let prior = self.epoch();
        self.epoch_serial += 1;
        let (input, artifact) = self.frame_input(frame, self.boundary).await?;
        let typed = context.to_typed();
        let params = AgentRollbackParams {
            agent_id: self.agent_id.clone(),
            prior_epoch: prior,
            policy: ROLLBACK_POLICY.to_owned(),
            input,
            decision_context: typed.clone(),
        };
        let reply = self
            .call(
                METHOD_AGENT_ROLLBACK,
                Some(self.scope()),
                params.to_json(),
                &[("view.lcd", &artifact)],
                &[],
            )
            .await?;
        let result: AgentRollbackResult = reply.parse().map_err(domain)?;
        result
            .validate_against_scope(&self.scope())
            .map_err(|e| e.0)?;
        self.context = typed;
        Ok(ParityRecord {
            index,
            kind: "rollback",
            boundary: self.boundary,
            ticks: 0,
            brain_ticks: result.telemetry.brain_ticks,
            remainder: self.remainder,
            decision: None,
            telemetry: Some(result.telemetry),
            spikes: None,
            state: None,
        })
    }

    /// `State.Capture` at the committed boundary.
    pub async fn capture(&mut self) -> Result<Captured, String> {
        self.captures += 1;
        let scope = self.scope();
        let params = CaptureParams {
            checkpoint_id: id(&format!("cp-{}", self.captures)),
        };
        let reply = self
            .call(
                "State.Capture",
                Some(scope.clone()),
                params.to_json(),
                &[],
                &[crate::state::PAYLOAD_ATTACHMENT],
            )
            .await?;
        let result: CaptureResult = reply.parse().map_err(domain)?;
        let artifact = reply
            .artifacts
            .get(crate::state::PAYLOAD_ATTACHMENT)
            .cloned()
            .ok_or("State.Capture carried no payload")?;
        let bytes = artifact.read_all().await.map_err(|e| e.message)?;
        self.acknowledge(reply.request_id).await?;
        Ok(Captured {
            result,
            artifact,
            bytes,
            scope,
        })
    }

    /// A group restore into `target`, a fresh replacement worker, under a new epoch.
    pub async fn restore_into(
        &mut self,
        target: WorkerRef,
        captured: &Captured,
    ) -> Result<(), String> {
        self.target = target;
        self.epoch_serial += 1;
        let params = StageRestoreParams {
            checkpoint_id: captured.result.checkpoint_id.clone(),
            source_scope: captured.scope.clone(),
            compatibility_digest: captured.result.compatibility_digest.clone(),
            payload: captured.artifact.reference().clone(),
        };
        let reply = self
            .call(
                "State.StageRestore",
                Some(self.scope()),
                params.to_json(),
                &[(crate::state::PAYLOAD_ATTACHMENT, &captured.artifact)],
                &[],
            )
            .await?;
        let staged: StageRestoreResult = reply.parse().map_err(domain)?;
        self.acknowledge(reply.request_id).await?;
        let params = ActivateRestoreParams {
            restore_token: staged.restore_token,
        };
        let reply = self
            .call("State.ActivateRestore", None, params.to_json(), &[], &[])
            .await?;
        let activated: ActivateRestoreResult = reply.parse().map_err(domain)?;
        if activated.committed_step != self.boundary {
            return Err("State.ActivateRestore landed on another boundary".to_owned());
        }
        self.acknowledge(reply.request_id).await
    }
}

/// Runs a script against the rig's agent, recording what the coordinator observes.
pub async fn run_on_worker(
    rig: &mut LegacyRig,
    agent_id: &Id,
    script: &LegacyScript,
) -> Result<Vec<ParityRecord>, String> {
    let mut driver = rig.driver(agent_id, script);
    let mut records = vec![driver.initialize(script.initial_frame).await?];
    for (index, step) in script.steps.iter().enumerate() {
        let mut record = match step {
            ScriptStep::Frame {
                sugar,
                frame,
                rewards,
                next_context,
            } => {
                driver
                    .frame(index + 1, sugar, *frame, rewards, next_context)
                    .await?
            }
            ScriptStep::Rollback { frame, context } => {
                driver.rollback(index + 1, *frame, context).await?
            }
            ScriptStep::Restore => {
                let captured = driver.capture().await?;
                let target = rig.replace(agent_id).await?;
                driver.restore_into(target, &captured).await?;
                // The replacement's own capture: a restore is exact for everything captured.
                let after = driver.capture().await?;
                ParityRecord {
                    index: index + 1,
                    kind: "restore",
                    boundary: driver.boundary,
                    ticks: 0,
                    brain_ticks: captured_brain_ticks(&after.bytes)?,
                    remainder: captured_remainder(&after.bytes)?,
                    decision: None,
                    telemetry: None,
                    spikes: None,
                    state: Some(payload_state_digest(&after.bytes)?),
                }
            }
        };
        if script.checkpoints.contains(&index) {
            let captured = driver.capture().await?;
            record.state = Some(payload_state_digest(&captured.bytes)?);
        }
        records.push(record);
    }
    Ok(records)
}

fn captured_session(payload: &[u8]) -> Result<Value, String> {
    let parts = decode_envelope(payload, PAYLOAD_MAGIC).map_err(|e| e.to_string())?;
    let session = parts.manifest.get("session").ok_or("no session member")?;
    serde_json::from_str(&session.stringify()).map_err(|e| e.to_string())
}

fn captured_brain_ticks(payload: &[u8]) -> Result<u64, String> {
    captured_session(payload)?["accumulator"]["executedTicks"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "no executedTicks".to_owned())
}

fn captured_remainder(payload: &[u8]) -> Result<RationalNs, String> {
    RationalNs::from_json(&captured_session(payload)?["accumulator"]["remainder"]).map_err(|e| e.0)
}

/// The rows and the overall digest of a record sequence, as a golden file holds them.
pub fn golden_json(script: &LegacyScript, records: &[ParityRecord]) -> Value {
    let all: Vec<Value> = records.iter().map(ParityRecord::to_json).collect();
    json!({
        "scenario": script.name,
        "macroChannels": script.macro_channels,
        "records": records.len(),
        "digest": digest_of(&Value::Array(all)).expect("records canonicalize"),
        "rows": records.iter().map(ParityRecord::row).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_pool_and_scripts_are_deterministic() {
        assert_eq!(frame_pool(3, 7), frame_pool(3, 7));
        assert_ne!(frame_pool(3, 7)[0], frame_pool(3, 7)[1]);
        let script = toy_macros_script(260);
        assert_eq!(
            script
                .steps
                .iter()
                .filter(|s| matches!(s, ScriptStep::Rollback { .. }))
                .count(),
            1
        );
        assert_eq!(
            script
                .steps
                .iter()
                .filter(|s| matches!(s, ScriptStep::Restore))
                .count(),
            1
        );
        for step in &script.steps {
            if let ScriptStep::Frame { next_context, .. } = step {
                next_context
                    .validate_against(&script.macro_channels)
                    .expect("a valid context");
            }
        }
    }
}
