//! Unit tests ported from `packages/brain/tests/plasticity.test.ts`.
//!
//! The golden scenarios pin the numbers; these pin the *rules*: what makes an edge plastic, which
//! pairings count, where the clamps sit, and that a rejected checkpoint mutates nothing.

mod common;

use std::sync::Arc;

use common::{toy_dataset, visual_toy_dataset};
use flybrain_core::lif::{LifConfig, LifNetwork};
use flybrain_core::plasticity::{
    plasticity_version, PlasticityConfig, PlasticityState, RewardModulatedStdp, PLASTICITY_VERSION,
};
use flybrain_core::pool::WorkerPool;

/// One causal pre(0)->post(1) pairing 10 ms apart, as in the original unit tests.
fn causal(plasticity: &mut RewardModulatedStdp, ms: f64) {
    plasticity.observe(&[1], 1, &[ms - 10.0, -1e6, -1e6, -1e6], ms);
}

fn rule(config: PlasticityConfig) -> RewardModulatedStdp {
    RewardModulatedStdp::new(&toy_dataset(), config)
}

fn network(config: LifConfig, plasticity: PlasticityConfig) -> LifNetwork {
    LifNetwork::new(Arc::new(visual_toy_dataset()), config, plasticity).expect("a network")
}

#[test]
fn causal_eligibility_is_necessary_and_learning_stays_in_anatomical_scope() {
    let mut plasticity = rule(PlasticityConfig::default());
    assert_eq!(plasticity.edges, vec![0]);

    // No eligibility yet: reward alone changes nothing.
    plasticity.reinforce(1.0, 100.0);
    assert_eq!(plasticity.gain(0), 1.0);

    // Eligibility but zero reward: `reinforce` returns before the restoring term too.
    causal(&mut plasticity, 110.0);
    plasticity.reinforce(0.0, 120.0);
    assert_eq!(plasticity.gain(0), 1.0);

    plasticity.reinforce(1.0, 130.0);
    assert!(plasticity.gain(0) > 1.0);

    // Only the selected edge moved; the rest of the connectome is immutable.
    for edge in 1..5 {
        assert_eq!(plasticity.gain(edge), 1.0, "edge {edge}");
    }

    let before = plasticity.export_state();
    plasticity.reinforce(0.0, 140.0);
    assert_eq!(plasticity.export_state(), before, "zero reward is a no-op");
}

#[test]
fn eligibility_decays_over_five_seconds_and_anti_causal_pairing_depresses() {
    let mut immediate = rule(PlasticityConfig::default());
    let mut delayed = rule(PlasticityConfig::default());
    causal(&mut immediate, 100.0);
    causal(&mut delayed, 100.0);
    immediate.reinforce(1.0, 100.0);
    delayed.reinforce(1.0, 5100.0);
    assert!(
        delayed.gain(0) > 1.0 && delayed.gain(0) < immediate.gain(0),
        "a 5 s old trace must still potentiate, but less"
    );

    // Post before pre: the anti-causal branch depresses.
    let mut plasticity = rule(PlasticityConfig::default());
    plasticity.observe(&[0], 1, &[-1e6, 90.0, -1e6, -1e6], 100.0);
    plasticity.reinforce(1.0, 100.0);
    assert!(plasticity.gain(0) < 1.0);

    // 20,000 alternating reinforcements never leave the clamp, so excitation never changes sign.
    for index in 0..20_000 {
        let ms = 200.0 + f64::from(index);
        causal(&mut plasticity, ms);
        plasticity.reinforce(if index < 10_000 { 10.0 } else { -10.0 }, ms);
        assert!(
            plasticity.gain(0) >= 0.899999 && plasticity.gain(0) <= 1.100001,
            "gain left the clamp at index {index}: {}",
            plasticity.gain(0)
        );
    }
}

#[test]
fn simultaneous_spikes_never_pair_in_either_direction() {
    // The window is 0 < dt <= 100 ms, so dt = 0 is excluded, and dt = 101 is out of range.
    let mut plasticity = rule(PlasticityConfig::default());
    plasticity.observe(&[1], 1, &[100.0, -1e6, -1e6, -1e6], 100.0);
    plasticity.reinforce(1.0, 100.0);
    assert_eq!(plasticity.gain(0), 1.0, "dt = 0 must not pair");

    let mut plasticity = rule(PlasticityConfig::default());
    plasticity.observe(&[1], 1, &[100.0, -1e6, -1e6, -1e6], 201.0);
    plasticity.reinforce(1.0, 201.0);
    assert_eq!(plasticity.gain(0), 1.0, "dt = 101 is outside the window");

    let mut plasticity = rule(PlasticityConfig::default());
    plasticity.observe(&[1], 1, &[100.0, -1e6, -1e6, -1e6], 200.0);
    plasticity.reinforce(1.0, 200.0);
    assert!(plasticity.gain(0) > 1.0, "dt = 100 is inside the window");
}

#[test]
fn traces_are_clipped_after_every_update_not_at_reinforcement_time() {
    let mut plasticity = rule(PlasticityConfig::default());
    // 0.1 per pair at dt -> 0 would exceed 1 after eleven pairs without the per-update clip.
    for index in 0..40 {
        let ms = 100.0 + f64::from(index);
        plasticity.observe(&[1], 1, &[ms - 1.0, -1e6, -1e6, -1e6], ms);
    }
    assert!(plasticity.traces.iter().all(|trace| trace.abs() <= 1.0));
    assert!(
        plasticity.traces[0] > 0.9,
        "the trace should be near saturation"
    );
}

#[test]
fn pre_post_roles_and_budget_choose_which_edges_are_plastic() {
    // Toy connectome edges: 0: 0->1 (10), 1: 0->2 (-5), 2: 0->3 (20), 3: 1->3 (10), 4: 2->1 (5).
    let with = |pre: &str, post: &str, budget: usize| {
        rule(PlasticityConfig {
            pre_role: pre.to_string(),
            post_role: post.to_string(),
            budget,
            ..PlasticityConfig::default()
        })
        .edges
        .clone()
    };
    assert_eq!(with("kenyon", "mbon", 16_384), vec![0]);
    assert_eq!(with("kenyon", "motor", 16_384), vec![2]);
    assert_eq!(with("mbon", "motor", 16_384), vec![3]);
    assert_eq!(with("mbon", "mbon", 16_384), vec![4]);
    assert_eq!(with("kenyon", "missing", 16_384), Vec::<u32>::new());

    // Budget keeps the strongest candidates, edge index breaking ties, re-sorted by edge.
    let mut wide = toy_dataset();
    wide.meta.roles.insert("kenyon".to_string(), vec![0, 1, 2]);
    wide.meta.roles.insert("mbon".to_string(), vec![1, 2, 3]);
    assert_eq!(
        RewardModulatedStdp::new(&wide, PlasticityConfig::default()).edges,
        vec![0, 2, 3, 4]
    );
    let budget_two = PlasticityConfig {
        budget: 2,
        ..PlasticityConfig::default()
    };
    assert_eq!(
        RewardModulatedStdp::new(&wide, budget_two).edges,
        vec![0, 2],
        "the two strongest are edges 2 (20) and 0 (10), re-sorted by edge"
    );

    // The topology hash separates all three selections even where the version string does not.
    let motor = PlasticityConfig {
        post_role: "motor".to_string(),
        ..PlasticityConfig::default()
    };
    let topologies: std::collections::HashSet<u32> = [
        rule(PlasticityConfig::default()).topology(),
        rule(motor).topology(),
        RewardModulatedStdp::new(&wide, PlasticityConfig::default()).topology(),
    ]
    .into_iter()
    .collect();
    assert_eq!(topologies.len(), 3);
}

#[test]
fn statistics_keep_the_historical_site_field_names() {
    let mut plasticity = rule(PlasticityConfig::default());
    let before = plasticity.statistics();
    assert_eq!(before.version, PLASTICITY_VERSION);
    assert!(before.enabled);
    assert_eq!(before.synapses, 1);
    assert_eq!(before.mushroom, 1);
    assert_eq!(before.output, 0);
    assert_eq!(before.updates, 0.0);
    assert_eq!(before.changed, 0);
    assert_eq!(before.mean_change, 0.0);
    assert_eq!(before.max_change, 0.0);
    assert_eq!(before.signal, 0.0);

    causal(&mut plasticity, 100.0);
    plasticity.reinforce(1.0, 100.0);
    let after = plasticity.statistics();
    assert_eq!(after.mushroom, plasticity.edges.len() as u64);
    assert_eq!(after.output, 0);
    assert_eq!(after.updates, 1.0);
    assert_eq!(after.changed, 1);
    assert!(after.max_change > 0.0);
    assert_eq!(after.signal, flybrain_core::jsmath::tanh(1.0));
}

#[test]
fn clamps_follow_the_configured_gain_bounds() {
    let wide = || PlasticityConfig {
        min_gain: 0.5,
        max_gain: 3.0,
        learning_rate: 1.0,
        ..PlasticityConfig::default()
    };
    let mut plasticity = rule(wide());
    for index in 0..50 {
        let ms = 100.0 + f64::from(index);
        causal(&mut plasticity, ms);
        plasticity.reinforce(10.0, ms);
    }
    assert_eq!(plasticity.gain(0), 3.0);

    let state = plasticity.export_state();
    rule(wide())
        .import_state(Some(&state))
        .expect("the same bounds accept their own state");

    // The default radius (0.100001) rejects a gain of 3.
    let default_version = PlasticityState {
        version: PLASTICITY_VERSION.to_string(),
        ..state
    };
    assert_eq!(
        rule(PlasticityConfig::default())
            .import_state(Some(&default_version))
            .unwrap_err()
            .message(),
        "Invalid plasticity values"
    );
}

#[test]
fn disabled_plasticity_ignores_observation_and_reinforcement() {
    let mut plasticity = rule(PlasticityConfig::default());
    plasticity.enabled = false;
    causal(&mut plasticity, 100.0);
    plasticity.reinforce(1.0, 100.0);
    assert_eq!(plasticity.gain(0), 1.0);
    assert_eq!(plasticity.statistics().updates, 0.0);

    plasticity.enabled = true;
    causal(&mut plasticity, 200.0);
    plasticity.reinforce(1.0, 200.0);
    assert!(plasticity.gain(0) > 1.0);

    plasticity.clear_eligibility(200.0);
    assert!(plasticity.traces.iter().all(|trace| *trace == 0.0));
    assert!(plasticity.touched.iter().all(|touched| *touched == 200.0));
    assert_eq!(plasticity.statistics().signal, 0.0);
    // clearEligibility does not touch gains.
    assert!(plasticity.gain(0) > 1.0);
}

#[test]
fn a_default_version_state_is_incompatible_with_a_non_default_configuration() {
    let state = rule(PlasticityConfig::default()).export_state();

    let faster = PlasticityConfig {
        learning_rate: 0.004,
        ..PlasticityConfig::default()
    };
    assert_eq!(
        rule(faster)
            .import_state(Some(&state))
            .unwrap_err()
            .message(),
        "Incompatible plasticity topology/version"
    );

    // Same rule constants, different site: the topology hash rejects it even though versions match.
    let site = || PlasticityConfig {
        post_role: "motor".to_string(),
        ..PlasticityConfig::default()
    };
    assert_eq!(rule(site()).version, PLASTICITY_VERSION);
    assert_eq!(
        rule(site())
            .import_state(Some(&state))
            .unwrap_err()
            .message(),
        "Incompatible plasticity topology/version"
    );

    // And through the network, which delegates the check.
    let slower = PlasticityConfig {
        trace_ms: 4000.0,
        ..PlasticityConfig::default()
    };
    let mut brain = network(LifConfig::default(), slower);
    let default_state = network(LifConfig::default(), PlasticityConfig::default()).export_state();
    assert_eq!(
        brain.import_state(&default_state).unwrap_err().message(),
        "Incompatible plasticity topology/version"
    );
}

#[test]
fn invalid_imports_reject_before_any_neural_or_plastic_mutation() {
    let mut brain = network(LifConfig::default(), PlasticityConfig::default());
    let original = brain.export_state();

    let mut bad_ms = original.clone();
    bad_ms.ms = f64::NAN;
    assert_eq!(
        brain.import_state(&bad_ms).unwrap_err().message(),
        "Invalid neural checkpoint values"
    );
    assert_eq!(brain.export_state(), original);

    /// An expected message paired with the corruption that should produce it.
    type Case = (&'static str, Box<dyn Fn(&mut PlasticityState)>);
    let patches: Vec<Case> = vec![
        (
            "Incompatible plasticity topology/version",
            Box::new(|state: &mut PlasticityState| state.version = "wrong".to_string()),
        ),
        (
            "Incompatible plasticity topology/version",
            Box::new(|state: &mut PlasticityState| state.topology = 0),
        ),
        (
            "Invalid plasticity values",
            Box::new(|state: &mut PlasticityState| state.gains = vec![f32::NAN]),
        ),
        (
            "Invalid plasticity values",
            Box::new(|state: &mut PlasticityState| state.traces = vec![2.0]),
        ),
        (
            "Invalid plasticity values",
            Box::new(|state: &mut PlasticityState| state.touched = vec![-1.0]),
        ),
        (
            "Invalid plasticity metadata",
            Box::new(|state: &mut PlasticityState| state.updates = -1.0),
        ),
        (
            "Invalid plasticity metadata",
            Box::new(|state: &mut PlasticityState| state.signal = 2.0),
        ),
    ];
    for (message, patch) in patches {
        let mut state = original.plasticity.clone();
        patch(&mut state);
        assert_eq!(
            brain
                .plasticity
                .import_state(Some(&state))
                .unwrap_err()
                .message(),
            message
        );
        assert_eq!(brain.export_state(), original, "mutated on {message}");
    }
}

#[test]
fn exact_neural_continuation_survives_a_long_run_and_a_checkpoint() {
    // 2^26 ms exercises the Float64 pairing timestamps: 1-ms differences must still resolve.
    let mut source = network(LifConfig::default(), PlasticityConfig::default());
    source.ms = f64::from(1u32 << 26);
    source.step(15);
    let ms = source.ms;
    causal(&mut source.plasticity, ms);
    source.plasticity.reinforce(1.0, ms);

    let mut restored = network(LifConfig::default(), PlasticityConfig::default());
    restored
        .import_state(&source.export_state())
        .expect("import");
    assert_eq!(restored.export_state(), source.export_state());

    source.step(25);
    restored.step(25);
    let source_ms = source.ms;
    let restored_ms = restored.ms;
    source.plasticity.reinforce(0.5, source_ms);
    restored.plasticity.reinforce(0.5, restored_ms);
    assert_eq!(restored.export_state(), source.export_state());
}

#[test]
fn a_warm_up_without_a_framebuffer_keeps_visual_neurons_finite() {
    // The regression test for the prototype's uninitialized warm-up bug.
    let mut brain = network(LifConfig::default(), PlasticityConfig::default());
    brain.step(100);
    assert!(brain.membrane.iter().all(|value| value.is_finite()));
    assert_eq!(brain.visual_drive(), &[0.0]);
    network(LifConfig::default(), PlasticityConfig::default())
        .import_state(&brain.export_state())
        .expect("a zero-drive checkpoint must load");
}

#[test]
fn the_default_configuration_keeps_the_historical_version() {
    assert_eq!(
        plasticity_version(&PlasticityConfig::default()),
        PLASTICITY_VERSION
    );
    assert_eq!(
        rule(PlasticityConfig::default()).export_state().version,
        PLASTICITY_VERSION
    );
}

/// `observe` sharded by slot range is the sequential walk, including where a shard is empty.
///
/// `golden_real` proves this on the real connectome's 16,384 slots as part of its whole-state
/// thread sweep. This one is the degenerate end: the toy fixture has a single plastic slot, so
/// every worker but one gets an empty range, and a pool with more workers than slots has to be a
/// no-op for the rest rather than a panic or a double write.
#[test]
fn sharded_observe_matches_the_sequential_walk_even_with_empty_shards() {
    let spikes = [1u32, 0, 1];
    let last_spike = [90.0, 95.0, -1e6, -1e6];

    let mut reference = rule(PlasticityConfig::default());
    reference.observe(&spikes, spikes.len(), &last_spike, 100.0);
    reference.observe(&spikes, spikes.len(), &last_spike, 101.0);
    assert!(
        reference.traces.iter().any(|trace| *trace != 0.0),
        "the fixture has to actually pair, or this test proves nothing"
    );

    for workers in [1usize, 2, 4, 16] {
        let pool = WorkerPool::new(workers, "test").expect("a pool");
        let mut sharded = rule(PlasticityConfig::default());
        let bounds = sharded.slot_shards(workers);
        assert_eq!(bounds.len(), workers + 1);
        assert_eq!(bounds[0], 0);
        assert_eq!(
            *bounds.last().expect("a last bound") as usize,
            sharded.gains.len(),
            "the shards have to cover every slot"
        );
        for ms in [100.0, 101.0] {
            sharded.observe_sharded(
                &spikes,
                spikes.len(),
                &last_spike,
                ms,
                Some((&pool, &bounds)),
            );
        }
        assert_eq!(sharded.traces, reference.traces, "traces, {workers} workers");
        assert_eq!(
            sharded.touched, reference.touched,
            "touched, {workers} workers"
        );
    }
}
