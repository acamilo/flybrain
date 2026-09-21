//! Golden scenario `real`: the committed `data/fafb-v783` artifacts for 200 ms.
//!
//! 139,255 neurons and 2,700,513 edges are too large to commit as state, so this scenario compares
//! digests, the per-millisecond spike counts, a few hundred sampled values for localizing a
//! failure, and the plastic-edge selection. It also covers the loader end to end: the seven-part
//! SHA-256 fingerprint is an assertion, which makes it the only test that can catch a difference
//! in how the `.binz` artifacts decode.
//!
//! Skips with an explicit message when `data/fafb-v783/meta.json` is absent, which happens on a
//! branch where the dataset has not been merged.

mod common;

use std::sync::Arc;

use common::{assert_f64_exact, frame_pool, golden, pool_digest, real_dataset_dir};
use flybrain_core::dataset::{load_brain_dataset_from_dir, sha256_hex};
use flybrain_core::envelope::{f32_bytes, f64_bytes};
use flybrain_core::json::JsonValue;
use flybrain_core::lif::{LifConfig, LifNetwork, SweepPlan};
use flybrain_core::plasticity::{PlasticityConfig, RewardModulatedStdp};

fn u32_bytes(values: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

#[test]
fn the_real_dataset_run_is_bit_exact() {
    let Some(dir) = real_dataset_dir() else {
        eprintln!("skipped: data/fafb-v783/meta.json is absent in this worktree");
        return;
    };
    let golden = golden("real");

    // --- The loader: seven digests joined with ':', over the decoded arrays.
    let data = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset must load"));
    assert_eq!(
        data.fingerprint.as_deref(),
        Some(golden.text("fingerprint").as_str()),
        "the dataset fingerprint differs from the oracle's"
    );
    assert_eq!(data.meta.neurons, golden.number("meta.neurons") as usize);
    assert_eq!(data.meta.edges, golden.number("meta.edges") as usize);
    assert_eq!(data.meta.dataset, golden.text("meta.dataset"));
    assert_eq!(
        data.meta.visual.count,
        golden.number("meta.visual.count") as usize
    );
    // Role *order* is what the merge preserves and what the tracked-role list depends on.
    assert_eq!(
        data.meta.roles.keys().cloned().collect::<Vec<_>>(),
        golden.strings("meta.roleOrder"),
        "circuit-role merge changed the role order"
    );
    for (role, size) in golden
        .at("meta.roleSizes")
        .as_object()
        .expect("meta.roleSizes")
    {
        assert_eq!(
            data.role(role).len(),
            size.as_usize().unwrap(),
            "role {role} size"
        );
    }

    // --- The run: a frame, 100 ms, a 120 ms pulse, 100 ms, one reinforcement.
    let mut network = LifNetwork::new(
        Arc::clone(&data),
        LifConfig::default(),
        PlasticityConfig::default(),
    )
    .expect("a default network");
    network.set_sweep_plan(SweepPlan::with_threads(4).expect("a pool"));

    assert_eq!(network.version, golden.text("kernelVersion"));
    assert_eq!(network.plasticity.version, golden.text("plasticityVersion"));
    assert_eq!(network.role_names, golden.strings("roleNames"));
    assert_eq!(
        sha256_hex(&f32_bytes(&network.baseline)),
        golden.text("baselineDigest"),
        "the seeded per-neuron baseline draws differ"
    );
    assert_eq!(
        network.plasticity.edges.len(),
        golden.number("plasticEdgeCount") as usize
    );
    assert_eq!(
        sha256_hex(&u32_bytes(&network.plasticity.edges)),
        golden.text("plasticEdgesDigest"),
        "the 16384 selected plastic edge indices differ"
    );
    assert_eq!(
        f64::from(network.plasticity.topology()),
        golden.number("topology"),
        "the FNV-1a-32 topology hash differs"
    );

    let width = golden.number("frame.width") as u32;
    let height = golden.number("frame.height") as u32;
    let frames = frame_pool(
        1,
        golden.number("frame.seed") as i32,
        width as usize,
        height as usize,
    );
    assert_eq!(pool_digest(&frames), golden.text("frame.digest"));
    network.set_visual_frame(&frames[0], width, height);

    let phases: Vec<u64> = golden
        .numbers("script.stepMs")
        .iter()
        .map(|value| *value as u64)
        .collect();
    let mut spikes = Vec::new();
    for (index, phase) in phases.iter().enumerate() {
        if index > 0 {
            network.stimulate(golden.number("script.stimulationMs"));
        }
        for _ in 0..*phase {
            spikes.push(network.step(1));
        }
    }
    let ms = network.ms;
    network
        .plasticity
        .reinforce(golden.number("script.reinforce"), ms);

    let want_spikes: Vec<u64> = golden
        .numbers("spikesPerMs")
        .iter()
        .map(|value| *value as u64)
        .collect();
    assert_eq!(spikes.len(), want_spikes.len());
    for (ms, (got, want)) in spikes.iter().zip(&want_spikes).enumerate() {
        assert_eq!(got, want, "spike count differs at ms {ms}");
    }
    assert_eq!(
        spikes.iter().sum::<u64>(),
        golden.number("totalSpikes") as u64
    );

    // --- The state: digests, then the sampled values that localize a digest failure.
    let state = network.export_state();
    for (field, got) in [
        ("membrane", sha256_hex(&f32_bytes(&state.membrane))),
        ("refractory", sha256_hex(&state.refractory)),
        ("lastSpikeMs", sha256_hex(&f64_bytes(&state.last_spike_ms))),
        ("visualDrive", sha256_hex(&f32_bytes(&state.visual_drive))),
        ("gains", sha256_hex(&f32_bytes(&state.plasticity.gains))),
        ("traces", sha256_hex(&f32_bytes(&state.plasticity.traces))),
        ("touched", sha256_hex(&f64_bytes(&state.plasticity.touched))),
    ] {
        let want = golden.text(&format!("digests.{field}"));
        if got != want {
            // Report the first sampled value that differs, so a 139k-entry digest mismatch names
            // a neuron rather than only a hash.
            report_sampled(&golden, &state);
            panic!("{field} digest differs: rust {got} != oracle {want}");
        }
    }

    let indices = golden.numbers("sampled.membraneIndices");
    let membrane = golden.numbers("sampled.membrane");
    let last_spike = golden.numbers("sampled.lastSpikeMs");
    let refractory = golden.numbers("sampled.refractory");
    for (position, index) in indices.iter().enumerate() {
        let index = *index as usize;
        assert_f64_exact(
            &format!("membrane[{index}]"),
            f64::from(state.membrane[index]),
            membrane[position],
        );
        assert_f64_exact(
            &format!("lastSpikeMs[{index}]"),
            state.last_spike_ms[index],
            last_spike[position],
        );
        assert_eq!(
            f64::from(state.refractory[index]),
            refractory[position],
            "refractory[{index}]"
        );
    }

    let slots = golden.numbers("sampledSlots.slots");
    let gains = golden.numbers("sampledSlots.gains");
    let traces = golden.numbers("sampledSlots.traces");
    let touched = golden.numbers("sampledSlots.touched");
    for (position, slot) in slots.iter().enumerate() {
        let slot = *slot as usize;
        assert_f64_exact(
            &format!("gains[{slot}]"),
            f64::from(state.plasticity.gains[slot]),
            gains[position],
        );
        assert_f64_exact(
            &format!("traces[{slot}]"),
            f64::from(state.plasticity.traces[slot]),
            traces[position],
        );
        assert_f64_exact(
            &format!("touched[{slot}]"),
            state.plasticity.touched[slot],
            touched[position],
        );
    }

    // --- The scalars.
    let network_json = golden.at("network");
    let number = |field: &str| network_json.get(field).and_then(JsonValue::as_f64).unwrap();
    assert_eq!(state.rng, number("rng") as i32, "rng");
    assert_f64_exact(
        "rewardRemaining",
        state.reward_remaining,
        number("rewardRemaining"),
    );
    assert_f64_exact("ms", state.ms, number("ms"));
    assert_f64_exact(
        "populationRate",
        state.population_rate,
        number("populationRate"),
    );
    for (role, value) in network_json.get("rates").unwrap().as_object().unwrap() {
        assert_f64_exact(
            &format!("rates.{role}"),
            state.rates.get(role).unwrap(),
            value.as_f64().unwrap(),
        );
    }

    let learning = network.plasticity.statistics();
    let want_learning = golden.at("learning");
    let learned = |field: &str| {
        want_learning
            .get(field)
            .and_then(JsonValue::as_f64)
            .unwrap()
    };
    assert_eq!(learning.synapses, learned("synapses") as usize);
    assert_eq!(learning.mushroom, learned("mushroom") as u64);
    assert_eq!(learning.output, 0);
    assert_eq!(learning.updates, learned("updates"));
    assert_eq!(learning.changed, learned("changed") as u64);
    assert_f64_exact("meanChange", learning.mean_change, learned("meanChange"));
    assert_f64_exact("maxChange", learning.max_change, learned("maxChange"));
    assert_f64_exact("signal", learning.signal, learned("signal"));

    // Guards against a vacuous pass.
    assert!(
        state.population_rate > 0.0,
        "the network must actually spike"
    );
    assert!(
        learning.changed > 0,
        "reinforcement must actually move gains"
    );
    assert!(
        network.plasticity.traces.iter().any(|trace| *trace != 0.0),
        "observation must actually write eligibility"
    );
}

/// Print the first sampled divergence, for a digest failure on arrays too large to diff.
fn report_sampled(golden: &common::Golden, state: &flybrain_core::lif::LifState) {
    let indices = golden.numbers("sampled.membraneIndices");
    let membrane = golden.numbers("sampled.membrane");
    for (position, index) in indices.iter().enumerate() {
        let index = *index as usize;
        let got = f64::from(state.membrane[index]);
        if got.to_bits() != membrane[position].to_bits() {
            eprintln!(
                "first sampled membrane divergence at neuron {index}: rust {got:?} != oracle {:?}",
                membrane[position]
            );
            return;
        }
    }
    eprintln!("no sampled value diverges; the difference is outside the sample stride");
}

#[test]
fn the_real_dataset_run_is_identical_for_one_two_and_seven_threads() {
    let Some(dir) = real_dataset_dir() else {
        eprintln!("skipped: data/fafb-v783/meta.json is absent in this worktree");
        return;
    };
    let data = Arc::new(load_brain_dataset_from_dir(&dir).expect("the dataset must load"));
    let frames = frame_pool(1, 99, 160, 144);

    let run = |sweep: SweepPlan| {
        let mut network = LifNetwork::new(
            Arc::clone(&data),
            LifConfig::default(),
            PlasticityConfig::default(),
        )
        .expect("a default network");
        network.set_sweep_plan(sweep);
        network.set_visual_frame(&frames[0], 160, 144);
        let mut spikes = Vec::new();
        for _ in 0..40 {
            spikes.push(network.step(1));
        }
        network.stimulate(120.0);
        for _ in 0..40 {
            spikes.push(network.step(1));
        }
        let ms = network.ms;
        network.plasticity.reinforce(1.0, ms);
        (spikes, network.export_state())
    };

    let reference = run(SweepPlan::sequential());
    assert!(reference.0.iter().sum::<u64>() > 0);
    // All three parallel phases partition by worker count -- the sweep by neuron index,
    // propagation by target range (bounds from the in-degree distribution) and `observe` by slot
    // range -- and the compared state includes the plastic traces and touch stamps, so the list
    // covers an even split, two odd ones, and more workers than the machine has cores.
    for threads in [1usize, 2, 3, 7, 16] {
        let got = run(SweepPlan::with_threads(threads).expect("a pool"));
        assert_eq!(
            got.0, reference.0,
            "spike counts differ with {threads} threads"
        );
        assert_eq!(got.1, reference.1, "state differs with {threads} threads");
    }
}

/// The ranked bitset replaced a dense `Vec<i32>` of "slot, or -1" with one entry per base edge.
/// Rebuild that array from the selected-edge list and compare all 2,700,513 answers, so the
/// substitution is proven on the real connectome rather than inferred from the digests.
#[test]
fn the_plastic_edge_bitset_reproduces_the_dense_slot_array() {
    let Some(dir) = real_dataset_dir() else {
        eprintln!("skipped: data/fafb-v783/meta.json is absent in this worktree");
        return;
    };
    let data = load_brain_dataset_from_dir(&dir).expect("the dataset must load");
    let mut plasticity = RewardModulatedStdp::new(&data, PlasticityConfig::default());

    let mut dense = vec![-1i32; data.meta.edges];
    for (slot, edge) in plasticity.edges.iter().copied().enumerate() {
        dense[edge as usize] = slot as i32;
    }
    // Move every gain off 1.0 first, so an implementation that always answered 1.0 cannot pass.
    for (slot, gain) in plasticity.gains.iter_mut().enumerate() {
        *gain = 0.9 + (slot % 101) as f32 / 1000.0;
    }

    let words = plasticity.slot_words();
    for (edge, want) in dense.iter().copied().enumerate() {
        let got = plasticity.slot_of(edge).map_or(-1i32, |slot| slot as i32);
        assert_eq!(got, want, "slot_of({edge})");
        let want_gain = if want < 0 {
            1.0
        } else {
            f64::from(plasticity.gains[want as usize])
        };
        assert_eq!(plasticity.gain(edge), want_gain, "gain({edge})");
        assert_eq!(
            plasticity.gain_in_word(edge, words[edge >> 6]),
            want_gain,
            "gain_in_word({edge})"
        );
    }
    assert!(
        plasticity.slot_index_bytes() * 20 < data.meta.edges * 4,
        "the index must be far smaller than the array it replaced: {} bytes",
        plasticity.slot_index_bytes()
    );
}
