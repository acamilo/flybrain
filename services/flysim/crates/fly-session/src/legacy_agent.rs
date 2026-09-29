//! AGENT-01: the legacy agent worker, `gameboy-legacy-fafb-v783-v1` over the existing neural core.
//!
//! [`LegacyAgentWorker`] is a [`WorkerEndpoint`] with the agent role. It is a small adapter over
//! `flybrain-core`'s [`NeuralAgent`] primitives -- the LIF network, its plasticity, the retina and
//! the fixed Game Boy readout -- and it does **not** go through `NeuralAgent::tick`, because that
//! wrapper runs a whole legacy frame in one call and the session splits it in two:
//!
//! | Legacy `Sim::step_frame` | Here |
//! | --- | --- |
//! | drain commands: `stimulate(sugar)` | `Agent.Prepare`: `preStepStimulations`, in order |
//! | `network.step(ticks)`, 548625/32768 ms per frame | `Agent.Prepare`: the rational accumulator of `step-v1` section 5, whose ticks and remainders are the legacy f64 ones exactly (`legacy-gameboy-v1` section 3) |
//! | blocked rule, `decode_bound(rates, ms, boot, blocked, bound)` | `Agent.Prepare`, with `gameboy-readout-context-v1` |
//! | `set_visual_frame(frame)` | `Agent.Commit` step 1 |
//! | `stimulate(event.stimulation_ms)` per event | `Agent.Commit` step 2, `taskStimulations` in order |
//! | `reinforce(sum, ms)` if plasticity is enabled | `Agent.Commit` step 3, once |
//! | `location != last` restarts the blocked window | `Agent.Commit`, from `nextDecisionContext.location` |
//! | `recover_game`'s neural half | `Agent.Rollback` (`legacy-ratchet-rollback-v1`) |
//! | FLYSIM01 load, then a fresh process's transients | `State.StageRestore` + `ActivateRestore` (`legacy-transient-reset`) |
//!
//! The readout transient -- the held channel, the start of the blocked window and the last
//! location -- is private agent state (`legacy-gameboy-v1` section 5) and is never captured.
//! A fresh start and a restore both begin it exactly as a fresh legacy process does: no held
//! channel, no location, the window starting at brain time **0 ms**. So the first decode after a
//! restore passes the restored winner as `blocked` (section 14 and the port-contracts review, N1).
//!
//! Rate roles are published under `legacy-rate-role-id-v1` ([`gameboy::rate_role_id`]): the
//! identity on the `Id` grammar, which every legacy role name already satisfies.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use fly_session_types::extensions::{
    AgentRollbackParams, AgentRollbackResult, METHOD_AGENT_ROLLBACK, ROLLBACK_CAPABILITY,
};
use fly_session_types::gameboy::{self, ChannelsDecision, Location, ReadoutContext};
use flybrain_core::agent::{AgentConfig as CoreAgentConfig, AgentState, NeuralAgent};
use flybrain_core::dataset::{BrainDataset, MACRO_ROLE_PREFIX, load_brain_dataset_from_dir};
use flybrain_core::decoder::gameboy::{gameboy_decoder_config_with_macros, to_button_mask};
use flybrain_core::decoder::{DecoderConfig, ExclusiveGroup};
use flybrain_core::envelope::{
    agent_from_chunks, agent_to_chunks, decode_envelope, encode_envelope,
};
use flybrain_core::json::JsonValue;
use flybrain_core::lif::{LifConfig, SweepPlan, kernel_version};
use serde_json::{Value, json};

use crate::clock::TickAccumulator;
// `crate::types` is this crate's facade over the shared `fly-session-types` crate.
use crate::types::*;
use crate::worker::{BoxFuture, HandlerCtx, HandlerReply, StatusCell, WorkerEndpoint};

// -------------------------------------------------------------------------------------------
// Profiles

/// The toy profile's id. A fixture profile for the committed toy connectome: every field is the
/// legacy profile's except the id, the dataset id and the fingerprint, and a profile never
/// reuses the legacy profile's id (`legacy-gameboy-v1` section 17). It is test identity, not a
/// production profile.
pub const TOY_PROFILE_ID: &str = "gameboy-legacy-toy-v1";
/// The toy connectome's dataset id.
pub const TOY_DATASET_ID: &str = "legacy-toy";
/// The schema-1 fingerprint of `fixtures/legacy-toy`, which the loader recomputes and the toy
/// golden test pins.
pub const TOY_FINGERPRINT: &str = "4014d84930520778281dba0000f97e07e36f200c045f9464f1fc860cb0d8efe6:\
6a95a07f1598277333397b551c9b3a3d88b0c75a507123486810b83bc5deb88e:\
6af524099ade9a45f6426eb2dc297306e8ecb2aa19fde7c5764fe8dda722d979:\
f767d48ff556fceae996e2ac73c0d0871df71101db0af414a0ffcd0bfb678489:\
525b5fe3910fd219ad7c989f977489fa98fe815a3eef95301626391a9ab126e6:\
319fbbb4acc79b1711ce1dc9431df2b3c0be49ca04fdd703f1afa204bd3f6e5d:\
49c2699978666f863cee2baf914167f00fa9a215ef4756536f8d6e64449ceb38";

/// The magic of the agent's capture payload: a `flybrain-core` checkpoint envelope whose
/// manifest and chunks are exactly `agent_to_chunks`, plus one `session` manifest member.
pub const PAYLOAD_MAGIC: &str = "FLYAGT01";
/// An optional payload chunk: the frame to install as the next input when the payload is
/// staged. Only the `FLYSIM01` import writes it (STATE-02), because the legacy restore
/// re-projects the saved framebuffer rather than trusting the saved visual drive.
pub const INPUT_FRAME_CHUNK: &str = "inputFrame";
/// The layout version of the `session` member.
pub const PAYLOAD_VERSION: u64 = 1;
/// The attachment a Commit reply carries the transition's spike bitset under.
pub const SPIKES_ATTACHMENT: &str = "telemetry.spikes";
/// Bit `i` is neuron `i` in dataset order, little-endian within a byte, `ceil(neurons/8)` bytes:
/// the legacy feed's `spike_bitset` layout.
pub const SPIKES_CONTENT_TYPE: &str = "application/x-fly-spike-bitset";
/// The legacy agent's read-only feed status (SERVE-01), and the capability that advertises it.
///
/// The legacy feed header and `GET /status` carry numbers from inside the network that no
/// session-framework message carries: the plasticity rule's whole statistics (`learning.changed`
/// counts synapses whose gain moved, `learning.synapses` the rule's edges), the decoder's
/// per-channel baseline and last score, and the rates and pulse as they stand at the committed
/// boundary. The session runtime's service host reads them here, once per published snapshot, at a
/// committed `Ready(k)`, which is where the legacy loop reads them for its own publish. It is a
/// declared extension of this one worker, not a `workers-v1` method: it changes nothing, is
/// answered in any phase once initialized, and is not in any trace.
pub const METHOD_FEED_STATUS: &str = "Legacy.FeedStatus";
pub const FEED_STATUS_CAPABILITY: &str = "legacy-feed-status-v1";

/// The one stimulus kind the profile supports.
pub const REWARD_PULSE: &str = gameboy::STIMULUS_REWARD_PULSE;

/// Which profile document a worker is configured for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegacyProfileKind {
    /// `gameboy-legacy-fafb-v783-v1`, over `data/fafb-v783`.
    Production,
    /// [`TOY_PROFILE_ID`], over the committed toy connectome.
    Toy,
}

impl LegacyProfileKind {
    pub fn label(&self) -> &'static str {
        match self {
            LegacyProfileKind::Production => "production",
            LegacyProfileKind::Toy => "toy",
        }
    }

    pub fn parse(text: &str) -> Result<LegacyProfileKind, String> {
        match text {
            "production" => Ok(LegacyProfileKind::Production),
            "toy" => Ok(LegacyProfileKind::Toy),
            other => Err(format!(
                "unknown legacy profile {other:?}; production or toy"
            )),
        }
    }

    pub fn profile(&self) -> LegacyAgentProfile {
        match self {
            LegacyProfileKind::Production => LegacyAgentProfile::production(),
            LegacyProfileKind::Toy => LegacyAgentProfile::toy(),
        }
    }
}

/// A profile document, its `AssetRef` and the dataset fingerprint it embeds.
#[derive(Clone, Debug)]
pub struct LegacyAgentProfile {
    pub kind: LegacyProfileKind,
    pub document: Value,
    pub asset: AssetRef,
    pub dataset_fingerprint: String,
}

impl LegacyAgentProfile {
    /// The one legacy profile of `legacy-gameboy-v1` section 2.
    pub fn production() -> LegacyAgentProfile {
        let document = DomainType::to_json(&gameboy::legacy_profile());
        LegacyAgentProfile {
            kind: LegacyProfileKind::Production,
            document,
            asset: gameboy::profile_asset_ref(),
            dataset_fingerprint: gameboy::FAFB_V783_FINGERPRINT.to_owned(),
        }
    }

    /// The toy fixture profile: the legacy document with the toy id, dataset and fingerprint.
    pub fn toy() -> LegacyAgentProfile {
        let mut document = DomainType::to_json(&gameboy::legacy_profile());
        document["profileId"] = Value::from(TOY_PROFILE_ID);
        document["datasetId"] = Value::from(TOY_DATASET_ID);
        document["datasetFingerprint"] = Value::from(TOY_FINGERPRINT);
        let canonical = canonicalize(&document).expect("a profile document canonicalizes");
        LegacyAgentProfile {
            kind: LegacyProfileKind::Toy,
            asset: AssetRef {
                id: TOY_PROFILE_ID.to_owned(),
                digest: digest_of_bytes(canonical.as_bytes()),
                byte_length: canonical.len() as u64,
                format: gameboy::PROFILE_FORMAT.to_owned(),
            },
            document,
            dataset_fingerprint: TOY_FINGERPRINT.to_owned(),
        }
    }
}

// -------------------------------------------------------------------------------------------
// The decoder configuration digest

fn group_form(group: Option<&ExclusiveGroup>) -> Value {
    match group {
        None => Value::Null,
        Some(g) => json!({
            "channels": g.channels.iter()
                .map(|(channel, role)| json!({"channel": channel, "role": role}))
                .collect::<Vec<_>>(),
            "decisionMs": g.decision_ms,
            "holdMs": g.hold_ms,
            "hysteresis": g.hysteresis,
            "fatigueGain": g.fatigue_gain,
            "fatigueDecay": g.fatigue_decay,
            "blockedFatigue": g.blocked_fatigue,
            "blockedMs": g.blocked_ms,
        }),
    }
}

/// The canonical form `gameboy-decoder-config-v1` of `legacy-gameboy-v1` section 12.
///
/// `flysim`'s `legacy_profile_identity` test builds the same form and writes the shared vectors
/// in `fly-session-types/fixtures/gameboy-decoder-config.json`; this crate's tests hold this
/// function to those vectors, so the agent reports the digest its own decoder has.
pub fn decoder_config_form(config: &DecoderConfig) -> Value {
    json!({
        "form": "gameboy-decoder-config-v1",
        "exclusive": group_form(config.exclusive.as_ref()),
        "macros": group_form(config.macros.as_ref()),
        "pulses": config.pulses.iter().map(|p| json!({
            "channel": p.channel,
            "role": p.role,
            "holdMs": p.hold_ms,
            "cooldownMs": p.cooldown_ms,
            "threshold": p.threshold,
            "boot": p.boot.map_or(Value::Null, |b| json!({"cooldownMs": b.cooldown_ms, "threshold": b.threshold})),
            "throttleGroup": p.throttle_group.clone().map_or(Value::Null, Value::String),
        })).collect::<Vec<_>>(),
        "clearLockoutMs": config.clear_lockout_ms,
    })
}

/// `decoderConfigDigest`: the SHA-256 of the canonical form.
pub fn decoder_config_digest(config: &DecoderConfig) -> Digest {
    digest_of(&decoder_config_form(config)).expect("a decoder form canonicalizes")
}

// -------------------------------------------------------------------------------------------
// Clock conversions

/// The legacy f64 frame remainder (ms) as the exact `RationalNs` remainder.
///
/// Every legacy remainder is a multiple of 2^-15 ms below one tick (`legacy-gameboy-v1`
/// section 3), so the conversion is exact; anything else is refused rather than rounded.
pub fn legacy_remainder_to_rational(ms: f64) -> Result<RationalNs, String> {
    let scaled = ms * 32_768.0;
    if !scaled.is_finite() || scaled < 0.0 || scaled.fract() != 0.0 || scaled >= 32_768.0 {
        return Err(format!(
            "legacy remainder {ms} is not k/32768 ms below one tick"
        ));
    }
    RationalNs::reduced(scaled as u128 * 1_000_000, 32_768).map_err(|e| e.0)
}

/// The exact `RationalNs` remainder as the legacy f64 millisecond remainder.
pub fn rational_remainder_to_legacy(remainder: &RationalNs) -> Result<f64, String> {
    let numerator = u128::from(remainder.numerator) * 32_768;
    let denominator = u128::from(remainder.denominator) * 1_000_000;
    if !numerator.is_multiple_of(denominator) {
        return Err(format!(
            "remainder {}/{} ns is not a multiple of 2^-15 ms",
            remainder.numerator, remainder.denominator
        ));
    }
    let k = numerator / denominator;
    if k >= 32_768 {
        return Err("a remainder must stay below one model tick".to_owned());
    }
    Ok(k as f64 / 32_768.0)
}

// -------------------------------------------------------------------------------------------
// The capture payload

/// Everything in a `FLYAGT01` capture payload's `session` member except the accumulator and the
/// context: whose state it is, where it was taken and under which identities.
#[derive(Clone, Copy, Debug)]
pub struct PayloadIdentity<'a> {
    pub agent_id: &'a Id,
    pub checkpoint_id: &'a Id,
    pub source_scope: &'a Scope,
    pub committed_step: u64,
    pub profile: &'a AssetRef,
    pub seed: i32,
    /// Reinforcement calls applied since the fly's fresh start (`learning.updates`).
    pub reinforcements: u64,
    /// The composition's `executor.macroChannels`, in composition order; empty in raw mode.
    pub macro_channels: &'a [String],
}

/// Encodes a `FLYAGT01` capture payload: `agent_to_chunks(state)` in `flybrain-core`'s envelope,
/// plus the `session` manifest member `State.StageRestore` reads back. `state.remainder` must
/// already be the accumulator's, as a capture sets it.
///
/// The worker's own `State.Capture` writes exactly this, and so does the `FLYSIM01` import
/// (`crate::legacy_checkpoint`), which is why it is one function.
pub fn encode_payload(
    state: &AgentState,
    identity: &PayloadIdentity<'_>,
    accumulator: &TickAccumulator,
    context: &TypedValue,
    input_frame: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let mut chunks = agent_to_chunks(state);
    if let Some(frame) = input_frame {
        chunks.chunks.push((INPUT_FRAME_CHUNK.to_owned(), frame.to_vec()));
    }
    let channels: Vec<&str> = identity.macro_channels.iter().map(String::as_str).collect();
    let session = json!({
        "payloadVersion": PAYLOAD_VERSION,
        "kind": "legacy-agent",
        "agentId": identity.agent_id,
        "checkpointId": identity.checkpoint_id,
        "sourceScope": identity.source_scope.to_json(),
        "committedStep": identity.committed_step.to_string(),
        "profile": identity.profile.to_json(),
        "seed": identity.seed,
        "reinforcements": identity.reinforcements.to_string(),
        "kernelVersion": gameboy::KERNEL_VERSION,
        "plasticityVersion": gameboy::PLASTICITY_VERSION,
        "macroChannels": identity.macro_channels,
        "decoderConfigDigest": decoder_config_digest(&gameboy_decoder_config_with_macros(&channels)),
        "accumulator": {
            "tickDuration": accumulator.tick_duration().to_json(),
            "remainder": accumulator.remainder().to_json(),
            "executedTicks": accumulator.executed_ticks().to_string(),
            "warmupOffset": accumulator.warmup_offset().to_string(),
        },
        "context": context.to_json(),
    });
    let mut manifest = chunks.manifest;
    let session = JsonValue::parse(&session.to_string()).map_err(|e| e.to_string())?;
    manifest.set("session", session);
    encode_envelope(PAYLOAD_MAGIC, &manifest, &chunks.chunks).map_err(|e| e.to_string())
}

/// The reinforcement calls a `FLYAGT01` capture payload records (`learning.updates`).
pub fn decode_payload_reinforcements(bytes: &[u8]) -> Result<u64, String> {
    let parts = decode_envelope(bytes, PAYLOAD_MAGIC)
        .map_err(|e| format!("not a {PAYLOAD_MAGIC} envelope: {e}"))?;
    let session = parts.manifest.get("session").ok_or("the agent payload has no session member")?;
    let session: Value = serde_json::from_str(&session.stringify()).map_err(|e| e.to_string())?;
    session
        .get("reinforcements")
        .and_then(Value::as_str)
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| "the agent payload has no reinforcement count".to_owned())
}

/// The agent state a `FLYAGT01` capture payload carries, with the accumulator's remainder in it.
/// The `session` member is not validated here; `State.StageRestore` does that.
pub fn decode_payload_state(bytes: &[u8]) -> Result<AgentState, String> {
    let parts = decode_envelope(bytes, PAYLOAD_MAGIC)
        .map_err(|e| format!("not a {PAYLOAD_MAGIC} envelope: {e}"))?;
    agent_from_chunks(&parts.manifest, &parts).map_err(|e| e.to_string())
}

// -------------------------------------------------------------------------------------------
// The readout transient

/// The readout's private transient: the channel the direction group holds, the brain time at
/// which the blocked window last restarted, and the last location the task reported.
///
/// `legacy-gameboy-v1` sections 5 and 14: never captured, and reset on Initialize and on every
/// restore to exactly what a fresh legacy process holds.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadoutTransient {
    pub held_channel: Option<String>,
    pub blocked_since_ms: f64,
    pub location: Option<Location>,
}

impl ReadoutTransient {
    /// A fresh legacy process: no held channel, no location, the window starting at 0 ms.
    pub fn legacy_reset() -> ReadoutTransient {
        ReadoutTransient {
            held_channel: None,
            blocked_since_ms: 0.0,
            location: None,
        }
    }
}

/// The spike bitset of the legacy feed: neuron `i` fired at or after `since_ms`.
pub fn spike_bitset(last_spike_ms: &[f64], since_ms: f64, now_ms: f64) -> Vec<u8> {
    let mut bytes = vec![0u8; last_spike_ms.len().div_ceil(8)];
    if now_ms <= since_ms {
        return bytes;
    }
    for (index, last) in last_spike_ms.iter().enumerate() {
        if *last >= since_ms {
            bytes[index >> 3] |= 1 << (index & 7);
        }
    }
    bytes
}

// -------------------------------------------------------------------------------------------
// Configuration

/// One legacy agent worker's configuration.
#[derive(Clone, Debug)]
pub struct LegacyAgentConfig {
    pub session_id: Id,
    pub agent_id: Id,
    pub incarnation_id: Id,
    /// The launcher's thread allocation. `workerThreads > 1` parallelises the neuron sweep,
    /// which `flybrain-core` guarantees does not change a result.
    pub worker_threads: usize,
    /// The dataset directory: `data/fafb-v783` for the production profile.
    pub dataset_dir: PathBuf,
    pub profile: LegacyProfileKind,
    /// The composition's `executor.macroChannels`, in composition order; empty in raw mode.
    pub macro_channels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentPhase {
    Uninitialized,
    Ready(u64),
    Prepared(u64),
    Failed,
}

/// The decision context this worker holds for its next Prepare.
#[derive(Clone, Debug)]
struct HeldContext {
    typed: TypedValue,
    readout: ReadoutContext,
    digest: Digest,
}

/// A validated replacement state, invisible to the live session until it is activated.
struct StagedAgent {
    token: Id,
    checkpoint_id: Id,
    scope: Scope,
    agent: NeuralAgent,
    dataset: Arc<BrainDataset>,
    graph: AgentGraph,
    accumulator: TickAccumulator,
    context: HeldContext,
    seed: i32,
    reinforcements: u64,
    committed_step: u64,
}

/// The legacy agent worker endpoint.
pub struct LegacyAgentWorker {
    config: LegacyAgentConfig,
    profile: LegacyAgentProfile,
    status: StatusCell,
    phase: AgentPhase,
    epoch: Option<Id>,
    dataset: Option<Arc<BrainDataset>>,
    graph: Option<AgentGraph>,
    seed: i32,
    agent: Option<NeuralAgent>,
    accumulator: Option<TickAccumulator>,
    transient: ReadoutTransient,
    context: Option<HeldContext>,
    prepared: Option<(DomainRequestId, PreparedDecision)>,
    /// The brain time before the in-flight transition's ticks: the spike window's start.
    transition_start_ms: f64,
    staged: Option<StagedAgent>,
    activated: BTreeSet<Id>,
    /// Every mutation this worker applied, reported as its progress counter.
    mutations: u64,
    /// Reinforcement calls applied since the fly's fresh start; captured.
    reinforcements: u64,
}

fn incompatible(message: impl std::fmt::Display) -> DomainError {
    DomainError::before(ErrorCode::IncompatibleState, message)
}

fn applied(code: ErrorCode, message: impl std::fmt::Display) -> DomainError {
    DomainError::new(code, message, MutationCertainty::Applied)
}

fn check_supported(stimulus: &Stimulus) -> DomainResult<()> {
    stimulus.validate().map_err(DomainError::invalid)?;
    if stimulus.kind_id != REWARD_PULSE {
        return Err(DomainError::before(
            ErrorCode::Unsupported,
            format!(
                "stimulus kind {} is not one this profile resolves; it supports {REWARD_PULSE}",
                stimulus.kind_id
            ),
        ));
    }
    Ok(())
}

/// The dataset digest an `AgentGraph` attests: SHA-256 of the schema-1 fingerprint string.
pub fn dataset_digest(fingerprint: &str) -> Digest {
    digest_of_bytes(fingerprint.as_bytes())
}

/// The index digest: the fingerprint (which covers the neuron order and every anatomical role)
/// plus the `macro_*` populations it deliberately does not cover, the profile's declared
/// `macro-roles-outside-fingerprint` exception. Two datasets that relabel the macro
/// populations differently have the same fingerprint and different index digests.
pub fn index_digest(dataset: &BrainDataset, fingerprint: &str) -> Digest {
    let mut text = format!(
        "fly-session/legacy-agent-index-v1\nfingerprint={fingerprint}\nneurons={}\n",
        dataset.meta.neurons
    );
    for (name, neurons) in &dataset.meta.roles {
        if name.starts_with(MACRO_ROLE_PREFIX) {
            let list: Vec<String> = neurons.iter().map(u32::to_string).collect();
            text.push_str(&format!("role={name}:{}\n", list.join(",")));
        }
    }
    digest_of_bytes(text.as_bytes())
}

/// The capture compatibility digest (`workers-v1` section 2) of a legacy agent.
pub fn compatibility_digest(
    agent_id: &Id,
    profile: &AssetRef,
    graph: &AgentGraph,
    seed: i32,
) -> Digest {
    crate::agent::agent_compatibility_digest(
        agent_id,
        &profile.digest,
        &graph.dataset_digest,
        gameboy::KERNEL_VERSION,
        gameboy::PLASTICITY_VERSION,
        seed,
        &graph.index_digest,
    )
}

/// The telemetry of an agent at its current state.
///
/// `learning` maps the legacy counters onto the contract's, which requires
/// `changed <= updates`: `updates` is the reinforcement calls this agent applied (one per
/// commit while the rule is enabled, zero sums included), and `changed` is the legacy
/// `plasticity.updates`, the reinforcements that moved at least one gain. The legacy
/// per-synapse count (`LearningStats.changed`, gains off 1.0) is not a counter of updates and
/// is not carried; it stays readable from the captured state.
pub fn telemetry_of(agent: &NeuralAgent, brain_ticks: u64, reinforcements: u64) -> AgentTelemetry {
    let network = &agent.network;
    let plasticity = &network.plasticity;
    AgentTelemetry {
        brain_ticks,
        population_rate_hz: network.population_rate,
        rates: network
            .rates
            .iter()
            .map(|(name, hz)| RateSample {
                role_id: name.clone(),
                hz,
            })
            .collect(),
        learning: LearningTelemetry {
            enabled: plasticity.enabled,
            updates: reinforcements,
            changed: plasticity.updates() as u64,
            signal: plasticity.signal(),
        },
        stimulus_remaining_ms: Some(network.reward_remaining()),
    }
}

/// A finite number for JSON: the legacy header's `finite`, applied before the value is sent.
fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

/// The [`METHOD_FEED_STATUS`] result: exactly what the legacy loop's `publish` and
/// `publish_decoder_status` read from the network, in the network's own order.
///
/// ```text
/// { brainMs, rates: [[role, hz], ...], populationRate, rewardRemainingMs,
///   learning: { enabled, updates, changed, synapses, signal },
///   decoder: { calibrated, pending: [...], channels: [{channel, role, baseline, score}] } }
/// ```
///
/// Rates are pairs rather than an object so their order survives any JSON reader. Every
/// number goes through the legacy `finite` first: JSON has no NaN, and the header would have
/// written 0 for one anyway.
pub fn feed_status_of(agent: &NeuralAgent) -> Value {
    let network = &agent.network;
    let stats = network.plasticity.statistics();
    let decoder = &agent.decoder;
    let scores = decoder.last_scores();
    let baselines = decoder.baselines();
    let rates: Vec<Value> = network
        .rates
        .iter()
        .map(|(name, hz)| json!([name, finite_or_zero(hz)]))
        .collect();
    let channels: Vec<Value> = decoder
        .channel_roles()
        .into_iter()
        .map(|(channel, role)| {
            json!({
                "channel": channel,
                "role": role,
                "baseline": finite_or_zero(baselines.get_or_zero(role)),
                "score": finite_or_zero(scores.get_or_zero(channel)),
            })
        })
        .collect();
    json!({
        "brainMs": finite_or_zero(network.ms),
        "rates": rates,
        "populationRate": finite_or_zero(network.population_rate),
        "rewardRemainingMs": finite_or_zero(network.reward_remaining()),
        "learning": {
            "enabled": stats.enabled,
            "updates": finite_or_zero(stats.updates),
            "changed": stats.changed,
            "synapses": stats.synapses,
            "signal": finite_or_zero(stats.signal),
        },
        "decoder": {
            "calibrated": decoder.calibrated(),
            "pending": decoder.pending_baseline_roles(),
            "channels": channels,
        },
    })
}

/// `gameboy-channels-v1` from a decode's active set: the button mask in `GAMEBOY_BUTTONS`
/// order, and the first bound channel the decode holds, which is exactly the channel the legacy
/// macro layer's `asked` would start.
pub fn channels_decision(active: &[String], bound: &[String]) -> ChannelsDecision {
    let mask = to_button_mask(active);
    let mut buttons = [false; 8];
    for (bit, down) in buttons.iter_mut().enumerate() {
        *down = mask & (1 << bit) != 0;
    }
    let macro_channel = bound
        .iter()
        .find(|channel| active.contains(channel))
        .cloned();
    ChannelsDecision {
        buttons,
        macro_channel,
    }
}

impl LegacyAgentWorker {
    pub fn new(config: LegacyAgentConfig) -> LegacyAgentWorker {
        LegacyAgentWorker {
            profile: config.profile.profile(),
            config,
            status: StatusCell::new(),
            phase: AgentPhase::Uninitialized,
            epoch: None,
            dataset: None,
            graph: None,
            seed: 0,
            agent: None,
            accumulator: None,
            transient: ReadoutTransient::legacy_reset(),
            context: None,
            prepared: None,
            transition_start_ms: 0.0,
            staged: None,
            activated: BTreeSet::new(),
            mutations: 0,
            reinforcements: 0,
        }
    }

    pub fn status(&self) -> StatusCell {
        self.status.clone()
    }

    /// The readout transient, for an in-process test.
    pub fn transient(&self) -> &ReadoutTransient {
        &self.transient
    }

    /// The neural agent, for an in-process test.
    pub fn agent(&self) -> Option<&NeuralAgent> {
        self.agent.as_ref()
    }

    fn bump(&mut self) {
        self.mutations += 1;
        self.status.advance_to(self.mutations);
    }

    fn check_epoch(&self, scope: &Scope) -> DomainResult<()> {
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this worker belongs to another session",
            ));
        }
        match &self.epoch {
            Some(epoch) if *epoch == scope.epoch => Ok(()),
            Some(_) => Err(DomainError::before(
                ErrorCode::StaleEpoch,
                "this scope names an epoch this worker has left",
            )),
            None => Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "this worker is uninitialized",
            )),
        }
    }

    /// Reads and validates a decision context against the composition's macro channels.
    fn read_context(&self, typed: &TypedValue) -> DomainResult<HeldContext> {
        typed.validate().map_err(DomainError::invalid)?;
        let readout = ReadoutContext::from_typed(typed).map_err(|e| {
            DomainError::before(
                ErrorCode::IdentityMismatch,
                format!(
                    "the decision context is not gameboy-readout-context-v1: {}",
                    e.0
                ),
            )
        })?;
        readout
            .validate_against(&self.config.macro_channels)
            .map_err(DomainError::invalid)?;
        Ok(HeldContext {
            typed: typed.clone(),
            readout,
            digest: typed.digest(),
        })
    }

    /// Reads the one `lcd` view of a sensory input: 160 x 144 RGBA8, produced at the observed
    /// boundary, from a live owned attachment. A missing view is an error, never zero input.
    async fn read_lcd(&self, ctx: &HandlerCtx<'_>, input: &SensoryInput) -> DomainResult<Vec<u8>> {
        input
            .validate_for_profile(false)
            .map_err(DomainError::invalid)?;
        let [view] = input.views.as_slice() else {
            return Err(DomainError::invalid(format!(
                "this profile consumes exactly one view, {}; the input has {}",
                gameboy::VIEW_ID,
                input.views.len()
            )));
        };
        if view.view_id != gameboy::VIEW_ID {
            return Err(DomainError::invalid(format!(
                "this profile consumes the view {}, not {}",
                gameboy::VIEW_ID,
                view.view_id
            )));
        }
        if view.produced_step != input.boundary {
            return Err(DomainError::invalid(
                "the lcd view has no render delay: it must be produced at the observed boundary",
            ));
        }
        let expected = gameboy::VIEW_WIDTH * gameboy::VIEW_HEIGHT * 4;
        let name = crate::media::view_attachment(&view.view_id);
        let artifact = ctx.artifact(&name)?;
        if artifact.reference() != &view.pixels {
            return Err(DomainError::before(
                ErrorCode::BufferInvalid,
                format!("attachment {name} is not the artifact the payload names"),
            ));
        }
        let bytes = artifact.read_all().await.map_err(|e| {
            DomainError::before(
                ErrorCode::BufferInvalid,
                format!("view {} could not be read: {}", view.view_id, e.message),
            )
        })?;
        if bytes.len() as u64 != expected || view.pixels.byte_length != expected {
            return Err(DomainError::before(
                ErrorCode::BufferInvalid,
                format!(
                    "the lcd view must be {expected} RGBA bytes, found {}",
                    bytes.len()
                ),
            ));
        }
        Ok(bytes)
    }

    /// Loads the dataset once and checks it is the one the profile embeds.
    fn dataset(&mut self) -> DomainResult<Arc<BrainDataset>> {
        if let Some(dataset) = &self.dataset {
            return Ok(dataset.clone());
        }
        let dir = &self.config.dataset_dir;
        let dataset = load_brain_dataset_from_dir(dir).map_err(|e| {
            DomainError::before(
                ErrorCode::IncompatibleState,
                format!("the dataset at {} does not load: {e}", dir.display()),
            )
        })?;
        match dataset.fingerprint.as_deref() {
            Some(fingerprint) if fingerprint == self.profile.dataset_fingerprint => {}
            found => {
                return Err(DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!(
                        "the dataset at {} is not the one {} embeds: fingerprint {} (expected {})",
                        dir.display(),
                        self.profile.asset.id,
                        found.unwrap_or("none"),
                        self.profile.dataset_fingerprint
                    ),
                ));
            }
        }
        let dataset = Arc::new(dataset);
        self.dataset = Some(dataset.clone());
        Ok(dataset)
    }

    /// Builds a network and readout over `dataset`, before any state is installed, and checks
    /// every identity the profile pins: kernel and plasticity versions, frame size, rate roles.
    fn build_agent(
        &self,
        dataset: Arc<BrainDataset>,
        seed: i32,
    ) -> DomainResult<(NeuralAgent, AgentGraph)> {
        let lif = LifConfig {
            seed,
            ..LifConfig::default()
        };
        let kernel = kernel_version(&lif);
        if kernel != gameboy::KERNEL_VERSION {
            return Err(incompatible(format!(
                "seed {seed} is not this profile's: its kernel version would be {kernel}, and the \
                 profile pins {} (the default seed, 22222)",
                gameboy::KERNEL_VERSION
            )));
        }
        let channels: Vec<&str> = self
            .config
            .macro_channels
            .iter()
            .map(String::as_str)
            .collect();
        let mut config =
            CoreAgentConfig::with_decoder(gameboy_decoder_config_with_macros(&channels));
        config.lif = lif;
        config.warmup_ms = gameboy::WARMUP_MS;
        let mut agent = NeuralAgent::new(dataset.clone(), config)
            .map_err(|e| incompatible(format!("the agent does not build: {e}")))?;
        if agent.network.version != gameboy::KERNEL_VERSION
            || agent.network.plasticity.version != gameboy::PLASTICITY_VERSION
        {
            return Err(incompatible(format!(
                "the built network is {} / {}, not the profile's {} / {}",
                agent.network.version,
                agent.network.plasticity.version,
                gameboy::KERNEL_VERSION,
                gameboy::PLASTICITY_VERSION
            )));
        }
        if (u64::from(agent.frame.width), u64::from(agent.frame.height))
            != (gameboy::VIEW_WIDTH, gameboy::VIEW_HEIGHT)
        {
            return Err(incompatible(
                "the retina is not the profile's 160 x 144 lcd",
            ));
        }
        let names: Vec<&str> = agent.network.rates.keys().map(String::as_str).collect();
        let rate_roles = gameboy::rate_role_ids(names.iter().copied()).map_err(|e| {
            incompatible(format!(
                "the dataset's rate roles cannot be published: {}",
                e.0
            ))
        })?;
        for channel in &self.config.macro_channels {
            if !names.contains(&channel.as_str()) {
                return Err(incompatible(format!(
                    "macro channel {channel} reads a rate role this dataset does not track"
                )));
            }
        }
        if self.config.worker_threads > 1 {
            let plan = SweepPlan::with_threads(self.config.worker_threads)
                .map_err(|e| DomainError::invalid(format!("sweep plan: {e}")))?;
            agent.set_sweep_plan(plan);
        }
        let fingerprint = self.profile.dataset_fingerprint.clone();
        let graph = AgentGraph {
            dataset_digest: dataset_digest(&fingerprint),
            index_digest: index_digest(&dataset, &fingerprint),
            neuron_count: dataset.meta.neurons as u64,
            rate_roles,
            supported_stimuli: vec![REWARD_PULSE.to_owned()],
        };
        Ok((agent, graph))
    }

    fn telemetry(&self) -> AgentTelemetry {
        let agent = self.agent.as_ref().expect("initialized");
        let accumulator = self.accumulator.as_ref().expect("initialized");
        telemetry_of(agent, accumulator.brain_ticks(), self.reinforcements)
    }

    // ---------------------------------------------------------------------------------------
    // Legacy.FeedStatus (SERVE-01)

    /// The network as the legacy loop's `publish` reads it, at the committed boundary.
    fn feed_status(&self) -> DomainResult<HandlerReply> {
        let Some(agent) = self.agent.as_ref() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "Legacy.FeedStatus needs an initialized agent",
            ));
        };
        if matches!(self.phase, AgentPhase::Prepared(_) | AgentPhase::Failed) {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "Legacy.FeedStatus reads a committed boundary",
            ));
        }
        let mut reply = HandlerReply::new(object(feed_status_of(agent)));
        reply.mutated = false;
        Ok(reply)
    }

    // ---------------------------------------------------------------------------------------
    // Agent.Initialize

    async fn initialize(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        if self.phase != AgentPhase::Uninitialized {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "Agent.Initialize is only allowed on an uninitialized agent; restore uses the \
                 state interface",
            ));
        }
        if scope.step != 0 {
            return Err(DomainError::before(
                ErrorCode::FutureStep,
                "Agent.Initialize uses the new epoch at step 0",
            ));
        }
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this worker belongs to another session",
            ));
        }
        let params: AgentInitializeParams = ctx.params()?;
        if params.agent_id != self.config.agent_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Initialize names another agent",
            ));
        }
        if params.worker_threads == 0 {
            return Err(DomainError::invalid("workerThreads must be >= 1"));
        }
        if params.worker_threads > self.config.worker_threads as u64 {
            return Err(DomainError::before(
                ErrorCode::Busy,
                format!(
                    "Agent.Initialize asks for {} worker threads; the launcher allocated {}",
                    params.worker_threads, self.config.worker_threads
                ),
            ));
        }
        // The profile identity: the document this worker is configured for, byte for byte.
        if params.profile != self.profile.asset {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!(
                    "Agent.Initialize names profile {} ({}); this worker serves {} ({})",
                    params.profile.id,
                    params.profile.digest,
                    self.profile.asset.id,
                    self.profile.asset.digest
                ),
            ));
        }
        if params.initial_input.boundary != 0 {
            return Err(DomainError::invalid(
                "the initial input must observe boundary 0",
            ));
        }
        let context = self.read_context(&params.initial_decision_context)?;
        let frame = self.read_lcd(ctx, &params.initial_input).await?;
        // Everything above is validated; now the dataset and the model. The dataset is checked
        // against the fingerprint the profile embeds before a network is built over it.
        let dataset = self.dataset()?;
        let (mut agent, graph) = self.build_agent(dataset, params.seed)?;

        // Warm-up with learning disabled, calibration on the settled rates, then the first
        // frame -- `NeuralAgent::warmup`, which is what the legacy fresh start calls.
        let mut accumulator = TickAccumulator::new(gameboy::tick_duration())
            .map_err(|e| applied(ErrorCode::Internal, e))?;
        agent
            .warmup(Some(&frame))
            .map_err(|e| applied(ErrorCode::BackendFailure, format!("warm-up: {e}")))?;
        accumulator
            .warm_up(gameboy::WARMUP_MS)
            .map_err(|e| applied(ErrorCode::Internal, e))?;
        if agent.network.ms != accumulator.brain_ticks() as f64 {
            return Err(applied(
                ErrorCode::Internal,
                "the brain clock after warm-up is not the accumulator's tick count",
            ));
        }

        self.seed = params.seed;
        self.agent = Some(agent);
        self.graph = Some(graph.clone());
        self.accumulator = Some(accumulator);
        // A fresh legacy process: the context's location is not adopted as the last location.
        // The legacy loop only takes a location at the end of a frame, so frame one's commit
        // is where the window first restarts on it.
        self.transient = ReadoutTransient::legacy_reset();
        self.transition_start_ms = gameboy::WARMUP_MS as f64;
        let digest = context.digest.clone();
        self.context = Some(context);
        self.epoch = Some(scope.epoch.clone());
        self.phase = AgentPhase::Ready(0);
        self.bump();
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope.clone()));

        let result = AgentInitializeResult {
            agent_id: self.config.agent_id.clone(),
            profile_digest: self.profile.asset.digest.clone(),
            tick_duration: gameboy::tick_duration(),
            warmup_ticks: gameboy::WARMUP_MS,
            committed_step: 0,
            decision_context_digest: digest,
            telemetry: self.telemetry(),
            graph,
        };
        result
            .validate()
            .map_err(|e| applied(ErrorCode::Internal, e.0))?;
        Ok(HandlerReply::from(&result))
    }

    // ---------------------------------------------------------------------------------------
    // Agent.Prepare

    async fn prepare(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        self.check_epoch(&scope)?;
        let AgentPhase::Ready(k) = self.phase.clone() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                format!(
                    "Agent.Prepare needs Ready(k); this worker is {:?}",
                    self.phase
                ),
            ));
        };
        if scope.step != k {
            return Err(DomainError::before(
                if scope.step < k {
                    ErrorCode::StaleStep
                } else {
                    ErrorCode::FutureStep
                },
                "Agent.Prepare names a step other than this worker's committed boundary",
            ));
        }
        let params: PrepareParams = ctx.params()?;
        if params.agent_id != self.config.agent_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Prepare names another agent",
            ));
        }
        if params.profile_digest != self.profile.asset.digest {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Prepare names another profile",
            ));
        }
        let context = self.context.clone().expect("initialized");
        if params.decision_context_digest != context.digest {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the cached decision context digest does not match",
            ));
        }
        if params.interval != gameboy::step_duration() {
            return Err(DomainError::invalid(
                "this profile's interval is one Game Boy frame, 8572265625/512 ns",
            ));
        }
        if params.pre_step_stimulations.len() > MAX_STIMULI {
            return Err(DomainError::invalid("at most 64 pre-step stimulations"));
        }
        for stimulus in &params.pre_step_stimulations {
            check_supported(stimulus)?;
        }

        self.status.set_state(WorkerState::Preparing);
        // 1. Admitted pre-step stimulation (sugar), in command order: the legacy drain.
        let agent = self.agent.as_mut().expect("initialized");
        for stimulus in &params.pre_step_stimulations {
            agent.network.stimulate(stimulus.duration_ms);
        }
        // 2. The frame's whole ticks.
        let accumulator = self.accumulator.as_mut().expect("initialized");
        let ticks = accumulator
            .advance(&params.interval)
            .map_err(|e| applied(ErrorCode::Internal, e))?;
        let brain_ticks = accumulator.brain_ticks();
        let remainder = accumulator.remainder();
        self.transition_start_ms = agent.network.ms;
        agent.network.step(ticks);
        if agent.network.ms != brain_ticks as f64 {
            return Err(applied(
                ErrorCode::Internal,
                "the brain clock is not the accumulator's tick count",
            ));
        }
        // 3. The fixed readout with the declared context, and the blocked-direction rule on
        // the agent's own held channel, clock and location history (`docs/readout.md`).
        let ms = agent.network.ms;
        let rates = agent.network.rates.clone();
        let blocked_ms = agent.decoder.blocked_ms();
        let blocked = (blocked_ms > 0.0 && ms - self.transient.blocked_since_ms >= blocked_ms)
            .then(|| agent.decoder.current())
            .flatten()
            .map(str::to_string);
        let bound = context.readout.bound.clone();
        // Raw mode has no macro group, and the legacy loop passes `None` there.
        let mask_bound = (!self.config.macro_channels.is_empty()).then_some(bound.as_slice());
        let active = agent.decoder.decode_bound(
            &rates,
            ms,
            context.readout.boot,
            blocked.as_deref(),
            mask_bound,
        );
        // A new winner starts its own window.
        let held = agent.decoder.current().map(str::to_string);
        if held != self.transient.held_channel {
            self.transient.held_channel = held;
            self.transient.blocked_since_ms = ms;
        }
        let decision = channels_decision(&active, &bound);
        decision
            .validate_against(&context.readout)
            .map_err(|e| applied(ErrorCode::Internal, e.0))?;
        let prepared = PreparedDecision {
            agent_id: self.config.agent_id.clone(),
            ticks_advanced: ticks,
            brain_ticks,
            remainder,
            decision: decision.to_typed(),
        };
        self.prepared = Some((ctx.request.request_id.clone(), prepared.clone()));
        self.phase = AgentPhase::Prepared(k);
        self.bump();
        self.status.set_state(WorkerState::Prepared);
        Ok(HandlerReply::from(&prepared))
    }

    // ---------------------------------------------------------------------------------------
    // Agent.Commit

    async fn commit(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        self.check_epoch(&scope)?;
        let AgentPhase::Prepared(k) = self.phase.clone() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                format!(
                    "Agent.Commit needs Prepared(k); this worker is {:?}",
                    self.phase
                ),
            ));
        };
        if scope.step != k {
            return Err(DomainError::before(
                if scope.step < k {
                    ErrorCode::StaleStep
                } else {
                    ErrorCode::FutureStep
                },
                "Agent.Commit must carry the step of its transition, not the new boundary",
            ));
        }
        let params: CommitParams = ctx.params()?;
        params
            .validate_against_scope(&scope)
            .map_err(DomainError::invalid)?;
        if params.agent_id != self.config.agent_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Commit names another agent",
            ));
        }
        let (prepared_request, _) = self.prepared.as_ref().expect("prepared");
        if params.prepared_request_id != *prepared_request {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Commit does not match this worker's Prepare request",
            ));
        }
        if params.rewards.len() > MAX_REWARDS || params.task_stimulations.len() > MAX_STIMULI {
            return Err(DomainError::invalid(
                "at most 64 rewards and 64 stimulations",
            ));
        }
        for reward in &params.rewards {
            reward.validate().map_err(DomainError::invalid)?;
        }
        for stimulus in &params.task_stimulations {
            check_supported(stimulus)?;
        }
        let context = self.read_context(&params.next_decision_context)?;
        // The complete request and its owned artifact are validated before anything applies.
        let frame = self.read_lcd(ctx, &params.next_input).await?;

        self.status.set_state(WorkerState::Committing);
        let agent = self.agent.as_mut().expect("initialized");
        // 1. Install the next input: the frame the environment just produced.
        let (width, height) = (agent.frame.width, agent.frame.height);
        agent.network.set_visual_frame(&frame, width, height);
        // 2. Task stimulation, in event order.
        for stimulus in &params.task_stimulations {
            agent.network.stimulate(stimulus.duration_ms);
        }
        // 3. One reinforcement with the sum, in event order, at the current brain time. The
        // legacy loop gates it on the rule being enabled and never skips a zero sum.
        let ms = agent.network.ms;
        let mut total = 0.0f64;
        for reward in &params.rewards {
            total += reward.value;
        }
        if agent.network.plasticity.enabled {
            agent.network.plasticity.reinforce(total, ms);
            self.reinforcements += 1;
        }
        // The blocked window's other reset: the player moved. `None` is no information.
        if let Some(location) = context.readout.location
            && Some(location) != self.transient.location
        {
            self.transient.location = Some(location);
            self.transient.blocked_since_ms = ms;
        }
        let spikes = spike_bitset(&agent.network.last_spike_ms, self.transition_start_ms, ms);
        // 4. Retain the next context and acknowledge k+1.
        let digest = context.digest.clone();
        self.context = Some(context);
        self.prepared = None;
        self.phase = AgentPhase::Ready(k + 1);
        self.bump();
        let result = AgentCommitResult {
            agent_id: self.config.agent_id.clone(),
            committed_step: k + 1,
            decision_context_digest: digest,
            telemetry: self.telemetry(),
        };
        let artifact = crate::media::seal_copy(ctx.client, SPIKES_CONTENT_TYPE.to_owned(), &spikes)
            .await
            .map_err(|e| applied(e.code, e.message))?;
        self.status.set_state(WorkerState::Ready);
        self.status
            .set_scope(Some(scope_at(&scope.session_id, &scope.epoch, k + 1)));
        Ok(HandlerReply::with_artifacts(
            object(result.to_json()),
            vec![(SPIKES_ATTACHMENT.to_owned(), artifact)],
        ))
    }

    // ---------------------------------------------------------------------------------------
    // Agent.Rollback: legacy-ratchet-rollback-v1

    async fn rollback(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this worker belongs to another session",
            ));
        }
        let params: AgentRollbackParams = ctx.params()?;
        params
            .validate_against_scope(&scope)
            .map_err(DomainError::invalid)?;
        if params.agent_id != self.config.agent_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Rollback names another agent",
            ));
        }
        match &self.epoch {
            None => {
                return Err(DomainError::before(
                    ErrorCode::InvalidPhase,
                    "this worker is uninitialized",
                ));
            }
            Some(epoch) if *epoch == scope.epoch => {
                return Err(DomainError::before(
                    ErrorCode::StaleEpoch,
                    "Agent.Rollback moves to a new epoch; this worker is already in it",
                ));
            }
            Some(epoch) if *epoch != params.prior_epoch => {
                return Err(DomainError::before(
                    ErrorCode::StaleEpoch,
                    format!(
                        "Agent.Rollback names prior epoch {}; this worker is in {epoch}",
                        params.prior_epoch
                    ),
                ));
            }
            Some(_) => {}
        }
        let AgentPhase::Ready(k) = self.phase.clone() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                format!(
                    "Agent.Rollback needs Ready(k); this worker is {:?}",
                    self.phase
                ),
            ));
        };
        if scope.step != k {
            return Err(DomainError::before(
                if scope.step < k {
                    ErrorCode::StaleStep
                } else {
                    ErrorCode::FutureStep
                },
                "Agent.Rollback is applied at this worker's committed boundary",
            ));
        }
        let context = self.read_context(&params.decision_context)?;
        let frame = self.read_lcd(ctx, &params.input).await?;

        self.status.set_state(WorkerState::Restoring);
        let agent = self.agent.as_mut().expect("initialized");
        let ms = agent.network.ms;
        // The neural half of `recover_game`, in its order: decoder holds, eligibility, frame.
        agent.decoder.clear_holds(ms);
        agent.network.plasticity.clear_eligibility(ms);
        let (width, height) = (agent.frame.width, agent.frame.height);
        agent.network.set_visual_frame(&frame, width, height);
        // `Sim::recover`: the location from the restored world, no held channel, the window
        // restarting now.
        self.transient = ReadoutTransient {
            held_channel: None,
            blocked_since_ms: ms,
            location: context.readout.location,
        };
        self.transition_start_ms = ms;
        let digest = context.digest.clone();
        self.context = Some(context);
        self.epoch = Some(scope.epoch.clone());
        self.prepared = None;
        self.phase = AgentPhase::Ready(k);
        self.bump();
        let result = AgentRollbackResult {
            agent_id: self.config.agent_id.clone(),
            committed_step: k,
            decision_context_digest: digest,
            telemetry: self.telemetry(),
        };
        result
            .validate_against_scope(&scope)
            .map_err(|e| applied(ErrorCode::Internal, e.0))?;
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope.clone()));
        Ok(HandlerReply::new(object(result.to_json())))
    }

    // ---------------------------------------------------------------------------------------
    // State.Capture / StageRestore / ActivateRestore

    /// The capture payload: `agent_to_chunks` in a checkpoint envelope, plus a `session`
    /// manifest member with the accumulator, the context and the identities. The readout
    /// transient is not in it (`legacy-transient-reset`).
    fn payload(&self, checkpoint_id: &Id, scope: &Scope, k: u64) -> DomainResult<Vec<u8>> {
        let agent = self.agent.as_ref().expect("initialized");
        let accumulator = self.accumulator.as_ref().expect("initialized");
        let context = self.context.as_ref().expect("initialized");
        let mut state = agent.export_state();
        // The session owns the frame remainder, as the legacy loop does (`Sim::checkpoint`).
        state.remainder = rational_remainder_to_legacy(&accumulator.remainder())
            .map_err(|e| DomainError::new(ErrorCode::Internal, e, MutationCertainty::None))?;
        encode_payload(
            &state,
            &PayloadIdentity {
                agent_id: &self.config.agent_id,
                checkpoint_id,
                source_scope: scope,
                committed_step: k,
                profile: &self.profile.asset,
                seed: self.seed,
                reinforcements: self.reinforcements,
                macro_channels: &self.config.macro_channels,
            },
            accumulator,
            &context.typed,
            None,
        )
        .map_err(|e| DomainError::new(ErrorCode::Internal, e, MutationCertainty::None))
    }

    async fn state_capture(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        self.check_epoch(&scope)?;
        let AgentPhase::Ready(k) = self.phase.clone() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                format!(
                    "State.Capture needs a quiescent Ready(k); this worker is {:?}",
                    self.phase
                ),
            ));
        };
        if scope.step != k {
            return Err(DomainError::before(
                if scope.step < k {
                    ErrorCode::StaleStep
                } else {
                    ErrorCode::FutureStep
                },
                "State.Capture names a boundary this worker is not at",
            ));
        }
        let params: CaptureParams = ctx.params()?;
        let previous = self.status.state();
        self.status.set_state(WorkerState::Capturing);
        let bytes = self.payload(&params.checkpoint_id, &scope, k);
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status.set_state(previous);
                return Err(e);
            }
        };
        let digest = digest_of_bytes(&bytes);
        let artifact = crate::state::seal_payload(ctx.client, &bytes, &digest).await;
        self.status.set_state(previous);
        let artifact = artifact?;
        let graph = self.graph.as_ref().expect("initialized");
        let result = CaptureResult {
            checkpoint_id: params.checkpoint_id,
            boundary: k,
            compatibility_digest: compatibility_digest(
                &self.config.agent_id,
                &self.profile.asset,
                graph,
                self.seed,
            ),
            payload: artifact.reference().clone(),
        };
        Ok(HandlerReply::with_artifacts(
            object(result.to_json()),
            vec![(crate::state::PAYLOAD_ATTACHMENT.to_owned(), artifact)],
        ))
    }

    async fn state_stage_restore(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this worker belongs to another session",
            ));
        }
        match &self.phase {
            AgentPhase::Uninitialized | AgentPhase::Ready(_) => {}
            other => {
                return Err(DomainError::before(
                    ErrorCode::InvalidPhase,
                    format!(
                        "State.StageRestore needs an uninitialized replacement or a quiescent \
                         worker; this worker is {other:?}"
                    ),
                ));
            }
        }
        if let Some(epoch) = &self.epoch
            && *epoch == scope.epoch
        {
            return Err(DomainError::before(
                ErrorCode::StaleEpoch,
                "State.StageRestore proposes the epoch this worker is already running",
            ));
        }
        let params: StageRestoreParams = ctx.params()?;
        if params.source_scope.step != scope.step {
            return Err(DomainError::invalid(
                "State.StageRestore's scope step must be the source boundary",
            ));
        }
        let artifact = ctx.artifact(crate::state::PAYLOAD_ATTACHMENT)?;
        if artifact.reference() != &params.payload {
            return Err(DomainError::before(
                ErrorCode::BufferInvalid,
                "the staged payload attachment is not the artifact the request names",
            ));
        }
        let bytes = artifact.read_all().await.map_err(|e| {
            DomainError::before(
                ErrorCode::BufferInvalid,
                format!("the staged payload could not be read: {}", e.message),
            )
        })?;
        let declared = params
            .payload
            .digest
            .clone()
            .ok_or_else(|| incompatible("a checkpoint payload must carry a content digest"))?;
        let actual = digest_of_bytes(&bytes);
        if actual != declared || bytes.len() as u64 != params.payload.byte_length {
            return Err(incompatible(
                "the staged payload is not the content the request declares",
            ));
        }
        let parts = decode_envelope(&bytes, PAYLOAD_MAGIC).map_err(|e| {
            incompatible(format!(
                "the staged payload is not a {PAYLOAD_MAGIC} envelope: {e}"
            ))
        })?;
        let session = parts
            .manifest
            .get("session")
            .ok_or_else(|| incompatible("the agent payload has no session member"))?;
        let session: Value = serde_json::from_str(&session.stringify())
            .map_err(|e| incompatible(format!("the session member is not JSON: {e}")))?;
        let text = |key: &str| -> DomainResult<String> {
            session
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| incompatible(format!("the agent payload has no {key}")))
        };
        if session.get("payloadVersion").and_then(Value::as_u64) != Some(PAYLOAD_VERSION) {
            return Err(incompatible("the agent payload is another payload version"));
        }
        if text("kind")? != "legacy-agent" {
            return Err(incompatible("this payload is not a legacy agent's state"));
        }
        if text("agentId")? != self.config.agent_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the staged payload belongs to another agent",
            ));
        }
        if text("checkpointId")? != params.checkpoint_id {
            return Err(incompatible(
                "the staged payload belongs to another checkpoint",
            ));
        }
        if text("kernelVersion")? != gameboy::KERNEL_VERSION
            || text("plasticityVersion")? != gameboy::PLASTICITY_VERSION
        {
            return Err(incompatible(
                "the staged payload was captured under another numerical model",
            ));
        }
        let source_scope = Scope::from_json(
            session
                .get("sourceScope")
                .ok_or_else(|| incompatible("the agent payload has no sourceScope"))?,
        )
        .map_err(|e| incompatible(format!("the agent payload's sourceScope: {}", e.0)))?;
        if source_scope != params.source_scope {
            return Err(incompatible(
                "the staged payload was captured at another source scope",
            ));
        }
        let committed_step: u64 = text("committedStep")?
            .parse()
            .map_err(|_| incompatible("the agent payload's committedStep is not a U64"))?;
        if committed_step != params.source_scope.step {
            return Err(incompatible(
                "the staged payload's committed step is not the source boundary",
            ));
        }
        let profile = AssetRef::from_json(
            session
                .get("profile")
                .ok_or_else(|| incompatible("the agent payload has no profile"))?,
        )
        .map_err(|e| incompatible(format!("the agent payload's profile: {}", e.0)))?;
        if profile != self.profile.asset {
            return Err(incompatible(format!(
                "the staged payload is profile {}; this worker serves {}",
                profile.id, self.profile.asset.id
            )));
        }
        let seed = session
            .get("seed")
            .and_then(Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| incompatible("the agent payload has no seed"))?;
        let reinforcements: u64 = text("reinforcements")?
            .parse()
            .map_err(|_| incompatible("the agent payload's reinforcements is not a U64"))?;
        let accumulator_value = session
            .get("accumulator")
            .ok_or_else(|| incompatible("the agent payload has no accumulator"))?;
        let rational = |key: &str| -> DomainResult<RationalNs> {
            RationalNs::from_json(
                accumulator_value
                    .get(key)
                    .ok_or_else(|| incompatible(format!("the accumulator has no {key}")))?,
            )
            .map_err(|e| incompatible(format!("the accumulator's {key}: {}", e.0)))
        };
        let counter = |key: &str| -> DomainResult<u64> {
            accumulator_value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| incompatible(format!("the accumulator has no {key}")))?
                .parse::<u64>()
                .map_err(|_| incompatible(format!("the accumulator's {key} is not a U64")))
        };
        let tick_duration = rational("tickDuration")?;
        if tick_duration != gameboy::tick_duration() {
            return Err(incompatible(
                "the staged state was captured at another model tick",
            ));
        }
        let accumulator = TickAccumulator::restored(
            tick_duration,
            rational("remainder")?,
            counter("executedTicks")?,
            counter("warmupOffset")?,
        )
        .map_err(incompatible)?;
        let context = self.read_context(
            &TypedValue::from_json(
                session
                    .get("context")
                    .ok_or_else(|| incompatible("the agent payload has no context"))?,
            )
            .map_err(|e| incompatible(format!("the agent payload's context: {}", e.0)))?,
        )?;
        let state: AgentState = agent_from_chunks(&parts.manifest, &parts)
            .map_err(|e| incompatible(format!("the agent state does not read: {e}")))?;
        // The accumulator and the agent state describe one clock.
        let legacy_remainder =
            rational_remainder_to_legacy(&accumulator.remainder()).map_err(incompatible)?;
        if state.remainder != legacy_remainder
            || state.network.ms != accumulator.brain_ticks() as f64
        {
            return Err(incompatible(
                "the agent state's clock and the session accumulator disagree",
            ));
        }
        if !state.warmed_up {
            return Err(incompatible("the staged agent state was never warmed up"));
        }
        // The conflicts first: a worker already holding a staged restore, or a token already
        // activated, is refused before a replacement network is built for nothing.
        if let Some(staged) = &self.staged {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                format!(
                    "this worker already holds the staged restore {} for checkpoint {}",
                    staged.token, staged.checkpoint_id
                ),
            ));
        }
        let token = crate::agent::restore_token(
            &params.checkpoint_id,
            &scope,
            &actual,
            &self.config.incarnation_id,
        );
        if self.activated.contains(&token) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                "this exact restore was already activated on this worker",
            ));
        }
        // A replacement fly: the dataset is loaded and checked here, not trusted.
        let dataset = self.dataset()?;
        let (mut agent, graph) = self.build_agent(dataset.clone(), seed)?;
        let computed =
            compatibility_digest(&self.config.agent_id, &self.profile.asset, &graph, seed);
        if computed != params.compatibility_digest {
            return Err(incompatible(format!(
                "the staged state's compatibility {} is not the {computed} this worker is",
                params.compatibility_digest
            )));
        }
        agent
            .import_state(&state)
            .map_err(|e| incompatible(format!("the agent state does not import: {e}")))?;
        // A payload imported from `FLYSIM01` carries the frame on screen as its next input, and
        // the restore installs it, as `LegacyFrame::restore` re-projects the saved framebuffer
        // (`legacy_checkpoint::agent_payload`). A worker's own capture carries none.
        if let Some(frame) = parts.chunk(INPUT_FRAME_CHUNK) {
            let (width, height) = (agent.frame.width, agent.frame.height);
            if frame.len() != width as usize * height as usize * 4 {
                return Err(incompatible("the staged input frame is not the profile's view"));
            }
            agent.network.set_visual_frame(frame, width, height);
        }
        self.staged = Some(StagedAgent {
            token: token.clone(),
            checkpoint_id: params.checkpoint_id.clone(),
            scope: scope.clone(),
            agent,
            dataset,
            graph,
            accumulator,
            context,
            seed,
            reinforcements,
            committed_step,
        });
        self.status.set_state(WorkerState::StagedRestore);
        Ok(HandlerReply::from(&StageRestoreResult {
            checkpoint_id: params.checkpoint_id,
            restore_token: token,
        }))
    }

    async fn state_activate_restore(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let params: ActivateRestoreParams = ctx.params()?;
        if self.activated.contains(&params.restore_token) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                "this restore token has already been activated",
            ));
        }
        let Some(staged) = self.staged.take() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "this worker holds no staged restore",
            ));
        };
        if staged.token != params.restore_token {
            let token = staged.token.clone();
            self.staged = Some(staged);
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!(
                    "this worker's staged restore is {token}, not {}",
                    params.restore_token
                ),
            ));
        }
        self.status.set_state(WorkerState::Restoring);
        let StagedAgent {
            token,
            checkpoint_id,
            scope,
            agent,
            dataset,
            graph,
            accumulator,
            context,
            seed,
            reinforcements,
            committed_step,
        } = staged;
        self.transition_start_ms = agent.network.ms;
        self.agent = Some(agent);
        self.dataset = Some(dataset);
        self.graph = Some(graph);
        self.accumulator = Some(accumulator);
        self.context = Some(context);
        self.seed = seed;
        self.reinforcements = reinforcements;
        // legacy-transient-reset: the readout transient starts as a fresh legacy process has
        // it, while the decoder state (holds, winners, fatigue) was restored with the agent.
        self.transient = ReadoutTransient::legacy_reset();
        self.epoch = Some(scope.epoch.clone());
        self.prepared = None;
        self.phase = AgentPhase::Ready(committed_step);
        self.activated.insert(token);
        self.bump();
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope_at(
            &scope.session_id,
            &scope.epoch,
            committed_step,
        )));
        let result = ActivateRestoreResult {
            committed_step,
            checkpoint_id,
            observation: None,
        };
        result
            .validate_for_role(Role::Agent)
            .map_err(|e| DomainError::invalid(e.0))?;
        Ok(HandlerReply::from(&result))
    }

    fn fail_on_applied(
        &mut self,
        outcome: DomainResult<HandlerReply>,
    ) -> DomainResult<HandlerReply> {
        if let Err(e) = &outcome
            && e.mutation != MutationCertainty::None
        {
            self.phase = AgentPhase::Failed;
        }
        outcome
    }
}

impl WorkerEndpoint for LegacyAgentWorker {
    fn worker_id(&self) -> Id {
        self.config.agent_id.clone()
    }

    fn incarnation_id(&self) -> Id {
        self.config.incarnation_id.clone()
    }

    fn session_id(&self) -> Id {
        self.config.session_id.clone()
    }

    fn role(&self) -> Role {
        Role::Agent
    }

    fn capabilities(&self) -> Vec<Id> {
        vec![
            id("agent-step-v1"),
            id("pixel-observation-v1"),
            id(crate::state::CHECKPOINT_CAPABILITY),
            id(ROLLBACK_CAPABILITY),
            id(FEED_STATUS_CAPABILITY),
        ]
    }

    fn status_cell(&self) -> StatusCell {
        self.status.clone()
    }

    fn worker_threads(&self) -> u64 {
        self.config.worker_threads as u64
    }

    fn methods(&self) -> Vec<&'static str> {
        vec![
            "Agent.Initialize",
            "Agent.Prepare",
            "Agent.Commit",
            METHOD_AGENT_ROLLBACK,
            "State.Capture",
            "State.StageRestore",
            "State.ActivateRestore",
            METHOD_FEED_STATUS,
        ]
    }

    fn handle<'a>(&'a mut self, ctx: HandlerCtx<'a>) -> BoxFuture<'a, DomainResult<HandlerReply>> {
        Box::pin(async move {
            let outcome = match ctx.method {
                "Agent.Initialize" => self.initialize(&ctx).await,
                "Agent.Prepare" => self.prepare(&ctx).await,
                "Agent.Commit" => self.commit(&ctx).await,
                METHOD_AGENT_ROLLBACK => self.rollback(&ctx).await,
                "State.Capture" => self.state_capture(&ctx).await,
                "State.StageRestore" => self.state_stage_restore(&ctx).await,
                "State.ActivateRestore" => self.state_activate_restore(&ctx).await,
                METHOD_FEED_STATUS => self.feed_status(),
                other => Err(DomainError::before(
                    ErrorCode::Unsupported,
                    format!("{other} is not an agent method"),
                )),
            };
            self.fail_on_applied(outcome)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_legacy_remainder_converts_exactly_both_ways() {
        let mut legacy = 0.0f64;
        for _ in 0..100_000 {
            legacy += flybrain_core::agent::GAMEBOY_MS_PER_FRAME;
            legacy -= legacy.floor();
            let rational = legacy_remainder_to_rational(legacy).expect("exact");
            assert_eq!(
                rational_remainder_to_legacy(&rational).expect("exact"),
                legacy
            );
        }
        assert!(
            legacy_remainder_to_rational(0.1).is_err(),
            "0.1 ms is not k/32768"
        );
        assert!(
            legacy_remainder_to_rational(1.0).is_err(),
            "a remainder is below one tick"
        );
    }

    #[test]
    fn the_spike_bitset_is_the_legacy_feed_layout() {
        let last = [5.0, 9.0, 10.0, -1.0, 12.0, 0.0, 0.0, 0.0, 11.0];
        assert_eq!(
            spike_bitset(&last, 10.0, 12.0),
            vec![0b0001_0100, 0b0000_0001]
        );
        assert_eq!(
            spike_bitset(&last, 12.0, 12.0),
            vec![0, 0],
            "an empty window is empty"
        );
    }

    #[test]
    fn the_macro_in_a_decision_is_the_first_bound_channel_held() {
        let active = vec![
            "up".to_owned(),
            "macro_talk".to_owned(),
            "macro_go_out".to_owned(),
        ];
        let bound = vec!["macro_go_out".to_owned(), "macro_talk".to_owned()];
        let decision = channels_decision(&active, &bound);
        assert_eq!(decision.macro_channel.as_deref(), Some("macro_go_out"));
        assert_eq!(decision.mask(), 1);
        assert_eq!(channels_decision(&active, &[]).macro_channel, None);
    }

    #[test]
    fn the_decoder_digest_reproduces_the_shared_vectors() {
        let file =
            fly_session_types::fixtures::load("gameboy-decoder-config.json").expect("vectors");
        for case in file["cases"].as_array().expect("cases") {
            let channels: Vec<&str> = case["macroChannels"]
                .as_array()
                .expect("channels")
                .iter()
                .map(|c| c.as_str().expect("a channel"))
                .collect();
            let config = gameboy_decoder_config_with_macros(&channels);
            assert_eq!(
                decoder_config_form(&config),
                case["form"],
                "{}",
                case["name"]
            );
            assert_eq!(
                decoder_config_digest(&config),
                case["digest"].as_str().expect("digest"),
                "{}",
                case["name"]
            );
        }
    }

    #[test]
    fn the_production_profile_is_the_contract_constant_and_the_toy_one_is_not() {
        let production = LegacyAgentProfile::production();
        assert_eq!(production.asset, gameboy::profile_asset_ref());
        let toy = LegacyAgentProfile::toy();
        assert_eq!(toy.asset.id, TOY_PROFILE_ID);
        assert_ne!(toy.asset.digest, production.asset.digest);
        assert_eq!(
            digest_of(&toy.document).expect("digest"),
            toy.asset.digest,
            "the toy AssetRef is the digest of its own document"
        );
        // Every pinned field other than the three it names is the legacy profile's.
        for key in [
            "kernelVersion",
            "plasticityVersion",
            "tickDuration",
            "warmupMs",
            "view",
            "supportedStimuli",
            "readoutContextSchema",
            "decisionSchema",
            "legacyExceptions",
        ] {
            assert_eq!(toy.document[key], production.document[key], "{key}");
        }
    }
}
