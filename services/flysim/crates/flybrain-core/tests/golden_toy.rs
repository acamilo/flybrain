//! Golden scenario `toy`: the four-neuron fixture over 3,000 ms, compared array for array.
//!
//! Small enough to carry every array verbatim, so a divergence here is localized to a neuron and
//! a millisecond rather than to a digest.

mod common;

use std::sync::Arc;

use common::{
    assert_f32_eq, assert_f64_eq, assert_f64_exact, assert_u8_eq, dataset_from_json, frame_pool,
    golden, pool_digest, Golden,
};
use flybrain_core::dataset::sha256_hex;
use flybrain_core::envelope::f32_bytes;
use flybrain_core::json::JsonValue;
use flybrain_core::lif::{LifNetwork, LifState, SweepPlan};
use flybrain_core::plasticity::PlasticityConfig;

/// What one replay of the scenario produced.
struct Replay {
    dumps: Vec<LifState>,
    spikes: Vec<u64>,
    role_names: Vec<String>,
    kernel_version: String,
    plasticity_version: String,
    plastic_edges: Vec<u32>,
    baseline: Vec<f32>,
}

/// Replay the scenario the generator scripted, collecting the three state dumps.
fn replay(sweep: SweepPlan) -> Replay {
    let golden = golden("toy");
    let data = dataset_from_json(golden.at("dataset"));
    let mut network = LifNetwork::new(
        Arc::clone(&data),
        flybrain_core::lif::LifConfig::default(),
        PlasticityConfig::default(),
    )
    .expect("a default network");
    network.set_sweep_plan(sweep);

    let width = golden.number("frame.width") as u32;
    let height = golden.number("frame.height") as u32;
    let frames = frame_pool(
        1,
        golden.number("frame.seed") as i32,
        width as usize,
        height as usize,
    );
    assert_eq!(
        pool_digest(&frames),
        golden.text("frame.digest"),
        "the seeded frame generator differs from the oracle's"
    );
    // The frame also travels in the envelope, so a generator mismatch cannot hide behind it.
    assert_u8_eq("frame", &frames[0], golden.chunk("frame"));

    let frame_at = golden.number("script.frameAtMs");
    let stimulate_at = golden.number("script.stimulateAtMs");
    let stimulation_ms = golden.number("script.stimulationMs");
    let reinforcements: Vec<(f64, f64)> = golden
        .at("script.reinforce")
        .as_array()
        .expect("script.reinforce")
        .iter()
        .map(|entry| {
            (
                entry.get("atMs").and_then(JsonValue::as_f64).unwrap(),
                entry.get("reward").and_then(JsonValue::as_f64).unwrap(),
            )
        })
        .collect();
    let dump_at = golden.numbers("script.dumpAtMs");
    let total_ms = golden.number("script.totalMs") as u64;

    let mut spikes = Vec::with_capacity(total_ms as usize);
    let mut dumps = Vec::new();
    for ms in 0..total_ms {
        let ms = ms as f64;
        if ms == frame_at {
            network.set_visual_frame(&frames[0], width, height);
        }
        if ms == stimulate_at {
            network.stimulate(stimulation_ms);
        }
        spikes.push(network.step(1));
        for (at, reward) in &reinforcements {
            if network.ms == *at {
                let now = network.ms;
                network.plasticity.reinforce(*reward, now);
            }
        }
        if dump_at.contains(&network.ms) {
            dumps.push(network.export_state());
        }
    }
    Replay {
        dumps,
        spikes,
        role_names: network.role_names.clone(),
        kernel_version: network.version.clone(),
        plasticity_version: network.plasticity.version.clone(),
        plastic_edges: network.plasticity.edges.clone(),
        baseline: network.baseline.clone(),
    }
}

/// Compare one dump against the manifest scalars and the suffixed chunks.
fn assert_dump(golden: &Golden, dump: &JsonValue, state: &LifState) {
    let at_ms = dump.get("atMs").and_then(JsonValue::as_f64).unwrap();
    let suffix = dump.get("suffix").and_then(JsonValue::as_str).unwrap();
    let label = |field: &str| format!("ms {at_ms} {field}");

    assert_f32_eq(
        &label("membrane"),
        &state.membrane,
        &golden.f32_chunk(&format!("membrane{suffix}")),
    );
    assert_u8_eq(
        &label("refractory"),
        &state.refractory,
        golden.chunk(&format!("refractory{suffix}")),
    );
    assert_f64_eq(
        &label("lastSpikeMs"),
        &state.last_spike_ms,
        &golden.f64_chunk(&format!("lastSpikeMs{suffix}")),
    );
    assert_f32_eq(
        &label("visualDrive"),
        &state.visual_drive,
        &golden.f32_chunk(&format!("visualDrive{suffix}")),
    );
    assert_f32_eq(
        &label("gains"),
        &state.plasticity.gains,
        &golden.f32_chunk(&format!("gains{suffix}")),
    );
    assert_f32_eq(
        &label("traces"),
        &state.plasticity.traces,
        &golden.f32_chunk(&format!("traces{suffix}")),
    );
    assert_f64_eq(
        &label("touched"),
        &state.plasticity.touched,
        &golden.f64_chunk(&format!("touched{suffix}")),
    );

    let network = dump.get("network").expect("dump.network");
    let number = |field: &str| network.get(field).and_then(JsonValue::as_f64).unwrap();
    assert_eq!(state.rng, number("rng") as i32, "{}", label("rng"));
    assert_f64_exact(
        &label("rewardRemaining"),
        state.reward_remaining,
        number("rewardRemaining"),
    );
    assert_f64_exact(&label("ms"), state.ms, number("ms"));
    assert_f64_exact(
        &label("populationRate"),
        state.population_rate,
        number("populationRate"),
    );

    let rates = network.get("rates").expect("dump.network.rates");
    let expected = rates.as_object().unwrap();
    assert_eq!(
        state.rates.keys().cloned().collect::<Vec<_>>(),
        expected
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>(),
        "{}: rate role order",
        label("rates")
    );
    for (role, value) in expected {
        assert_f64_exact(
            &label(&format!("rates.{role}")),
            state.rates.get(role).unwrap(),
            value.as_f64().unwrap(),
        );
    }

    let plasticity = dump
        .get("plasticity")
        .or_else(|| network.get("plasticity"))
        .unwrap();
    assert_eq!(
        state.plasticity.version,
        plasticity
            .get("version")
            .and_then(JsonValue::as_str)
            .unwrap()
    );
    assert_eq!(
        f64::from(state.plasticity.topology),
        plasticity
            .get("topology")
            .and_then(JsonValue::as_f64)
            .unwrap(),
        "{}",
        label("topology")
    );
    assert_eq!(
        state.plasticity.enabled,
        plasticity
            .get("enabled")
            .and_then(JsonValue::as_bool)
            .unwrap()
    );
    assert_f64_exact(
        &label("updates"),
        state.plasticity.updates,
        plasticity
            .get("updates")
            .and_then(JsonValue::as_f64)
            .unwrap(),
    );
    assert_f64_exact(
        &label("signal"),
        state.plasticity.signal,
        plasticity
            .get("signal")
            .and_then(JsonValue::as_f64)
            .unwrap(),
    );
}

#[test]
fn the_toy_run_is_bit_exact_at_every_dump() {
    let golden = golden("toy");
    let Replay {
        dumps,
        spikes,
        role_names,
        kernel_version,
        plasticity_version,
        plastic_edges,
        baseline,
    } = replay(SweepPlan::sequential());

    assert_eq!(kernel_version, golden.text("kernelVersion"));
    assert_eq!(plasticity_version, golden.text("plasticityVersion"));
    assert_eq!(role_names, golden.strings("roleNames"));
    assert_eq!(
        sha256_hex(&f32_bytes(&baseline)),
        golden.text("baselineDigest"),
        "the seeded per-neuron baseline draws differ"
    );
    assert_eq!(
        plastic_edges
            .iter()
            .map(|edge| f64::from(*edge))
            .collect::<Vec<_>>(),
        golden.numbers("plasticEdges"),
        "plastic edge selection differs"
    );

    let want_spikes: Vec<u64> = golden
        .numbers("spikesPerMs")
        .iter()
        .map(|v| *v as u64)
        .collect();
    assert_eq!(spikes.len(), want_spikes.len());
    for (ms, (got, want)) in spikes.iter().zip(&want_spikes).enumerate() {
        assert_eq!(got, want, "spike count differs at ms {ms}");
    }
    assert_eq!(
        spikes.iter().sum::<u64>(),
        golden.number("totalSpikes") as u64
    );
    // A run that never spiked would pass every comparison vacuously.
    assert!(
        spikes.iter().sum::<u64>() > 0,
        "the network must actually spike"
    );

    let expected_dumps = golden.at("dumps").as_array().expect("dumps").to_vec();
    assert_eq!(dumps.len(), expected_dumps.len());
    for (dump, state) in expected_dumps.iter().zip(&dumps) {
        assert_dump(&golden, dump, state);
    }

    // Reinforcement must actually have moved a gain, or the plasticity path is untested.
    let last = dumps.last().expect("a final dump");
    assert!(
        last.plasticity.gains.iter().any(|gain| *gain != 1.0),
        "no gain ever moved"
    );
    assert!(last.plasticity.updates > 0.0);
}

#[test]
fn the_toy_run_is_identical_for_one_two_and_seven_threads() {
    let reference = replay(SweepPlan::sequential()).dumps;
    // The fixture has four neurons, so anything above four leaves workers with an empty shard —
    // which is the degenerate partition the real dataset never produces.
    for threads in [1usize, 2, 7, 16] {
        let plan = SweepPlan::with_threads(threads).expect("a pool");
        assert_eq!(plan.threads(), threads);
        let dumps = replay(plan).dumps;
        assert_eq!(dumps.len(), reference.len());
        for (index, (got, want)) in dumps.iter().zip(&reference).enumerate() {
            assert_eq!(got, want, "dump {index} differs with {threads} threads");
        }
    }
}
