//! Golden scenario `agent`: the 4,096-neuron synthetic connectome through `NeuralAgent` and the
//! Game Boy readout for 600 frames.
//!
//! This is the end-to-end oracle. It covers the fractional frame remainder, the warm-up and its
//! calibration, the readout's exclusive group and pulse channels, reward-driven stimulation and
//! reinforcement, and the `learn` gate — asserting the button mask on every single frame.
//!
//! The connectome travels in the envelope as chunks rather than being re-derived here: its
//! generator consumes two random draws for a forced Kenyon edge and three for a free one, and a
//! reimplementation getting that short-circuit wrong would be a confusing failure rather than an
//! obvious one.

mod common;

use std::sync::Arc;

use common::{
    assert_f32_eq, assert_f64_eq, assert_f64_exact, assert_u8_eq, frame_pool, golden, pool_digest,
    synthetic_dataset, Golden,
};
use flybrain_core::agent::{
    AgentConfig, AgentState, FrameSize, NeuralAgent, RewardEvent, TickOptions,
};
use flybrain_core::dataset::sha256_hex;
use flybrain_core::decoder::gameboy::{gameboy_decoder_config, to_button_mask};
use flybrain_core::json::JsonValue;
use flybrain_core::lif::{LifState, SweepPlan};

/// The generator's per-frame schedule, rule for rule.
fn rewards_for(frame: usize) -> Vec<RewardEvent> {
    if frame.is_multiple_of(40) {
        vec![RewardEvent::new(1.0)]
    } else {
        Vec::new()
    }
}
fn boot_for(frame: usize) -> bool {
    (frame / 120).is_multiple_of(2)
}
fn learn_for(frame: usize) -> bool {
    !frame.is_multiple_of(90)
}

struct Replay {
    warmup: AgentState,
    masks: Vec<u32>,
    steps: Vec<u64>,
    spikes: Vec<u64>,
    dumps: Vec<(usize, AgentState)>,
    final_state: AgentState,
    learning: flybrain_core::plasticity::LearningStats,
    plastic_edges: Vec<u32>,
    compatibility: String,
}

fn replay(golden: &Golden, sweep: SweepPlan) -> Replay {
    let (data, _) = synthetic_dataset();
    let width = golden.number("frame.width") as u32;
    let height = golden.number("frame.height") as u32;
    let frames = frame_pool(
        golden.number("frame.count") as usize,
        golden.number("frame.seed") as i32,
        width as usize,
        height as usize,
    );
    assert_eq!(
        pool_digest(&frames),
        golden.text("frame.digest"),
        "the seeded frame generator differs from the oracle's"
    );

    let mut config = AgentConfig::with_decoder(gameboy_decoder_config());
    config.frame = Some(FrameSize { width, height });
    let mut agent = NeuralAgent::new(Arc::clone(&data), config).expect("an agent");
    agent.set_sweep_plan(sweep);
    assert_eq!(agent.warmup_ms, golden.number("script.warmupMs") as u64);
    assert_f64_exact(
        "msPerFrame",
        agent.ms_per_frame,
        golden.number("script.msPerFrame"),
    );

    assert!(!agent.ready());
    agent.warmup(Some(&frames[0])).expect("warmup");
    assert!(agent.ready());
    let warmup = agent.export_state();

    let total = golden.number("script.frames") as usize;
    let dump_at: Vec<usize> = golden
        .numbers("script.dumpAtFrames")
        .iter()
        .map(|value| *value as usize)
        .collect();
    let mut masks = Vec::with_capacity(total);
    let mut steps = Vec::with_capacity(total);
    let mut spikes = Vec::with_capacity(total);
    let mut dumps = Vec::new();
    for frame in 1..=total {
        let rewards = rewards_for(frame);
        let result = agent
            .tick(
                &frames[frame % frames.len()],
                &TickOptions {
                    rewards: &rewards,
                    boot: boot_for(frame),
                    learn: learn_for(frame),
                },
            )
            .expect("tick");
        masks.push(to_button_mask(&result.active));
        steps.push(result.steps);
        spikes.push(result.spikes);
        if dump_at.contains(&frame) {
            dumps.push((frame, agent.export_state()));
        }
    }
    Replay {
        warmup,
        masks,
        steps,
        spikes,
        final_state: agent.export_state(),
        learning: agent.plasticity().statistics(),
        plastic_edges: agent.plasticity().edges.clone(),
        compatibility: agent.compatibility(),
        dumps,
    }
}

/// Every array of a dump, against the manifest digests.
fn assert_digests(dump: &JsonValue, state: &LifState, label: &str) {
    let digests = dump.get("digests").expect("dump.digests");
    let want = |field: &str| {
        digests
            .get(field)
            .and_then(JsonValue::as_str)
            .unwrap()
            .to_string()
    };
    use flybrain_core::envelope::{f32_bytes, f64_bytes};
    for (field, got) in [
        ("membrane", sha256_hex(&f32_bytes(&state.membrane))),
        ("refractory", sha256_hex(&state.refractory)),
        ("lastSpikeMs", sha256_hex(&f64_bytes(&state.last_spike_ms))),
        ("visualDrive", sha256_hex(&f32_bytes(&state.visual_drive))),
        ("gains", sha256_hex(&f32_bytes(&state.plasticity.gains))),
        ("traces", sha256_hex(&f32_bytes(&state.plasticity.traces))),
        ("touched", sha256_hex(&f64_bytes(&state.plasticity.touched))),
    ] {
        assert_eq!(got, want(field), "{label}: {field} digest");
    }
}

/// The scalar half of a network dump.
fn assert_scalars(dump: &JsonValue, state: &LifState, label: &str) {
    let network = dump.get("network").expect("dump.network");
    let number = |field: &str| network.get(field).and_then(JsonValue::as_f64).unwrap();
    assert_eq!(state.rng, number("rng") as i32, "{label}: rng");
    assert_f64_exact(
        &format!("{label}: rewardRemaining"),
        state.reward_remaining,
        number("rewardRemaining"),
    );
    assert_f64_exact(&format!("{label}: ms"), state.ms, number("ms"));
    assert_f64_exact(
        &format!("{label}: populationRate"),
        state.population_rate,
        number("populationRate"),
    );
    for (role, value) in network.get("rates").unwrap().as_object().unwrap() {
        assert_f64_exact(
            &format!("{label}: rates.{role}"),
            state
                .rates
                .get(role)
                .unwrap_or_else(|| panic!("no rate {role}")),
            value.as_f64().unwrap(),
        );
    }
    let plasticity = network.get("plasticity").unwrap();
    assert_eq!(
        f64::from(state.plasticity.topology),
        plasticity
            .get("topology")
            .and_then(JsonValue::as_f64)
            .unwrap(),
        "{label}: topology"
    );
    assert_f64_exact(
        &format!("{label}: updates"),
        state.plasticity.updates,
        plasticity
            .get("updates")
            .and_then(JsonValue::as_f64)
            .unwrap(),
    );
    assert_f64_exact(
        &format!("{label}: signal"),
        state.plasticity.signal,
        plasticity
            .get("signal")
            .and_then(JsonValue::as_f64)
            .unwrap(),
    );
}

/// The readout half of a dump.
fn assert_decoder(dump: &JsonValue, state: &flybrain_core::decoder::DecoderState, label: &str) {
    let want = dump.get("decoder").expect("dump.decoder");
    assert_eq!(
        f64::from(state.version),
        want.get("version").and_then(JsonValue::as_f64).unwrap()
    );
    assert_eq!(
        state.calibrated,
        want.get("calibrated").and_then(JsonValue::as_bool).unwrap()
    );
    assert_f64_exact(
        &format!("{label}: nextDecision"),
        state.next_decision,
        want.get("nextDecision")
            .and_then(JsonValue::as_f64)
            .unwrap(),
    );
    match want.get("current") {
        Some(JsonValue::String(channel)) => {
            assert_eq!(
                state.current.as_deref(),
                Some(channel.as_str()),
                "{label}: current"
            )
        }
        _ => assert_eq!(state.current, None, "{label}: current"),
    }
    for (field, map) in [
        ("baseline", &state.baseline),
        ("heldUntil", &state.held_until),
        ("nextAllowed", &state.next_allowed),
        ("fatigue", &state.fatigue),
    ] {
        let expected = want.get(field).unwrap().as_object().unwrap();
        assert_eq!(
            map.keys().cloned().collect::<Vec<_>>(),
            expected
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>(),
            "{label}: {field} key order"
        );
        for (key, value) in expected {
            assert_f64_exact(
                &format!("{label}: {field}.{key}"),
                map.get(key).unwrap(),
                value.as_f64().unwrap(),
            );
        }
    }
}

#[test]
fn six_hundred_frames_are_bit_exact_frame_by_frame() {
    let golden = golden("agent");
    let replayed = replay(&golden, SweepPlan::sequential());

    assert_eq!(replayed.compatibility, golden.text("compatibility"));
    assert_eq!(
        replayed.plastic_edges.len(),
        golden.number("plasticEdgeCount") as usize
    );
    assert_eq!(
        sha256_hex(&u32_bytes(&replayed.plastic_edges)),
        golden.text("plasticEdgesDigest"),
        "plastic edge selection differs"
    );

    // The warm-up must land on identical state before a single frame is ticked.
    let warmup = golden.at("warmup");
    assert_scalars(warmup, &replayed.warmup.network, "warmup");
    assert_digests(warmup, &replayed.warmup.network, "warmup");
    assert_decoder(warmup, &replayed.warmup.decoder, "warmup");

    let want_masks: Vec<u32> = golden.numbers("masks").iter().map(|v| *v as u32).collect();
    let want_steps: Vec<u64> = golden.numbers("steps").iter().map(|v| *v as u64).collect();
    let want_spikes: Vec<u64> = golden.numbers("spikes").iter().map(|v| *v as u64).collect();
    assert_eq!(replayed.masks.len(), want_masks.len());
    for (index, (got, want)) in replayed.masks.iter().zip(&want_masks).enumerate() {
        assert_eq!(got, want, "button mask differs at frame {}", index + 1);
    }
    for (index, (got, want)) in replayed.steps.iter().zip(&want_steps).enumerate() {
        assert_eq!(got, want, "step count differs at frame {}", index + 1);
    }
    for (index, (got, want)) in replayed.spikes.iter().zip(&want_spikes).enumerate() {
        assert_eq!(got, want, "spike count differs at frame {}", index + 1);
    }

    // A run that never fires a pulse or never holds a direction would pass vacuously.
    let directions = 0b1111u32;
    let pulses = 0b1111_0000u32;
    assert!(
        want_masks.iter().any(|mask| mask & pulses != 0),
        "no pulse channel ever fired"
    );
    assert!(
        want_masks.iter().any(|mask| mask & directions != 0),
        "no direction was ever held"
    );
    // The fractional remainder must be carried, not lost: 16 and 17 both have to occur.
    assert!(replayed.steps.contains(&16) && replayed.steps.contains(&17));

    let dumps = golden.at("dumps").as_array().expect("dumps").to_vec();
    assert_eq!(dumps.len(), replayed.dumps.len());
    for (dump, (frame, state)) in dumps.iter().zip(&replayed.dumps) {
        let label = format!("frame {frame}");
        assert_eq!(
            dump.get("atFrame").and_then(JsonValue::as_f64).unwrap() as usize,
            *frame
        );
        assert_f64_exact(
            &format!("{label}: remainder"),
            state.remainder,
            dump.get("remainder").and_then(JsonValue::as_f64).unwrap(),
        );
        assert_scalars(dump, &state.network, &label);
        assert_digests(dump, &state.network, &label);
        assert_decoder(dump, &state.decoder, &label);
    }

    // The last dump also travels as full arrays, so the final state is compared entry by entry.
    let final_state = &replayed.final_state.network;
    assert_f32_eq(
        "final membrane",
        &final_state.membrane,
        &golden.f32_chunk("membraneZ"),
    );
    assert_u8_eq(
        "final refractory",
        &final_state.refractory,
        golden.chunk("refractoryZ"),
    );
    assert_f64_eq(
        "final lastSpikeMs",
        &final_state.last_spike_ms,
        &golden.f64_chunk("lastSpikeMsZ"),
    );
    assert_f32_eq(
        "final visualDrive",
        &final_state.visual_drive,
        &golden.f32_chunk("visualDriveZ"),
    );
    assert_f32_eq(
        "final gains",
        &final_state.plasticity.gains,
        &golden.f32_chunk("gainsZ"),
    );
    assert_f32_eq(
        "final traces",
        &final_state.plasticity.traces,
        &golden.f32_chunk("tracesZ"),
    );
    assert_f64_eq(
        "final touched",
        &final_state.plasticity.touched,
        &golden.f64_chunk("touchedZ"),
    );

    // Learning must have happened, or the plasticity half of the loop is untested.
    let learning = golden.at("learning");
    assert_eq!(
        replayed.learning.updates,
        learning.get("updates").and_then(JsonValue::as_f64).unwrap()
    );
    assert_eq!(
        replayed.learning.changed,
        learning.get("changed").and_then(JsonValue::as_f64).unwrap() as u64
    );
    assert_f64_exact(
        "meanChange",
        replayed.learning.mean_change,
        learning
            .get("meanChange")
            .and_then(JsonValue::as_f64)
            .unwrap(),
    );
    assert_f64_exact(
        "maxChange",
        replayed.learning.max_change,
        learning
            .get("maxChange")
            .and_then(JsonValue::as_f64)
            .unwrap(),
    );
    assert!(replayed.learning.updates > 0.0, "plasticity never updated");
    assert!(replayed.learning.changed > 0, "no gain ever moved");
}

#[test]
fn six_hundred_frames_are_identical_for_one_two_and_seven_threads() {
    let golden = golden("agent");
    let reference = replay(&golden, SweepPlan::sequential());
    for threads in [1usize, 2, 7] {
        let replayed = replay(&golden, SweepPlan::with_threads(threads).expect("a pool"));
        assert_eq!(
            replayed.masks, reference.masks,
            "button masks differ with {threads} threads"
        );
        assert_eq!(
            replayed.spikes, reference.spikes,
            "spike counts differ with {threads} threads"
        );
        assert_eq!(
            replayed.final_state, reference.final_state,
            "final state differs with {threads} threads"
        );
    }
}

fn u32_bytes(values: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}
