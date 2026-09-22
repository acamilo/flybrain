//! SESSION-01 acceptance: the synthetic sequential transaction, over both transports.
//!
//! Every test here is one of the acceptance bullets of the implementation guide's SESSION-01
//! slice, or one of the initialization, pause and episode rules of `step-v1` section 6.

mod common;

use std::collections::BTreeMap;

use common::{Fixture, at, count, default_fixture, fixture, fly_a, fly_b, within};
use fly_session::coordinator::DispatchOrder;
use fly_session::harness::{AgentSpec, HarnessConfig, Via};
use fly_session::phase::Phase;
use fly_session::types::*;
use fly_session::agent::AgentFaults;

both_transports!(
    one_world_advance_per_complete_batch,
    every_agent_is_prepared_before_the_world_advances,
    the_task_evaluates_each_transition_once,
    every_agent_commits_before_the_next_prepare_or_publication,
    a_60_hz_world_with_a_1_ms_tick_runs_16_17_17,
    a_pause_mid_step_completes_the_step_and_pauses_at_the_boundary,
    bootstrap_cannot_advance_the_world_or_produce_a_reward,
    the_committed_snapshot_names_the_boundary_that_just_ended,
    a_terminal_episode_pauses_at_its_own_boundary,
    status_answers_with_the_committed_boundary,
    a_worker_refuses_a_second_initialize,
    a_single_agent_composition_runs_the_same_transaction,
);

const STEPS: u64 = 3;

/// One `Environment.Advance` per complete batch, and one boundary per advance.
async fn one_world_advance_per_complete_batch(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let before = f.harness.environment_mutations();
    let reports = within("run", f.harness.coordinator.run(STEPS)).await.unwrap();
    assert_eq!(reports.len() as u64, STEPS);
    assert_eq!(f.harness.coordinator.stats().advances, STEPS);
    assert_eq!(f.harness.coordinator.phase(), Phase::Ready(STEPS));
    assert_eq!(
        f.harness.coordinator.observation().unwrap().boundary,
        STEPS,
        "the world is at exactly one boundary per batch"
    );
    // The environment's progress counter moves once per advance and not otherwise.
    assert_eq!(f.harness.environment_mutations() - before, STEPS);
    assert_eq!(count(&f.harness.coordinator.audit, "advance:0"), 1);
    assert_eq!(f.harness.coordinator.trace.transitions.len() as u64, STEPS);
    f.shutdown().await;
}

/// Every agent reaches Prepared before the batch is built and the world advances.
async fn every_agent_is_prepared_before_the_world_advances(via: Via) {
    // Different completion delays, so "all prepared" cannot be an accident of timing.
    let mut config = HarnessConfig::default();
    config.agents[0].faults = AgentFaults { prepare_delay_ms: 15, ..AgentFaults::default() };
    config.agents[1].faults = AgentFaults { prepare_delay_ms: 1, ..AgentFaults::default() };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(STEPS)).await.unwrap();
    let audit = f.harness.coordinator.audit.clone();
    for k in 0..STEPS {
        let advance = at(&audit, &format!("advance:{k}"));
        for agent in [fly_a(), fly_b()] {
            let prepared = at(&audit, &format!("prepared:{agent}@{k}"));
            assert!(
                prepared < advance,
                "{agent} must be Prepared({k}) before the world advances: {audit:?}"
            );
        }
    }
    f.shutdown().await;
}

/// The task's transition evaluation runs exactly once per acknowledged world step.
async fn the_task_evaluates_each_transition_once(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    assert_eq!(f.harness.coordinator.evaluations(), 0, "bootstrap evaluates no transition");
    within("run", f.harness.coordinator.run(STEPS)).await.unwrap();
    assert_eq!(f.harness.coordinator.evaluations(), STEPS);
    let audit = f.harness.coordinator.audit.clone();
    for k in 0..STEPS {
        assert_eq!(count(&audit, &format!("evaluate:{k}")), 1);
    }
    f.shutdown().await;
}

/// No agent starts the next Prepare, and nothing is published as committed, until every agent
/// has committed this transition.
async fn every_agent_commits_before_the_next_prepare_or_publication(via: Via) {
    let mut config = HarnessConfig::default();
    config.agents[0].faults = AgentFaults { commit_delay_ms: 12, ..AgentFaults::default() };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(STEPS)).await.unwrap();
    let audit = f.harness.coordinator.audit.clone();
    for k in 0..STEPS {
        let publish = at(&audit, &format!("publish:{}", k + 1));
        for agent in [fly_a(), fly_b()] {
            let committed = at(&audit, &format!("committed:{agent}@{k}"));
            assert!(
                committed < publish,
                "{agent} must commit before boundary {} is published: {audit:?}",
                k + 1
            );
            if k + 1 < STEPS {
                let next = at(&audit, &format!("prepared:{agent}@{}", k + 1));
                for other in [fly_a(), fly_b()] {
                    let other_commit = at(&audit, &format!("committed:{other}@{k}"));
                    assert!(
                        other_commit < next,
                        "{other} must commit step {k} before {agent} prepares {}: {audit:?}",
                        k + 1
                    );
                }
            }
        }
    }
    assert_eq!(f.harness.coordinator.stats().publications, STEPS + 1, "one per boundary, plus 0");
    f.shutdown().await;
}

/// `step-v1` section 5: a 60 Hz world with a 1 ms model tick runs 16, 17, 17 ticks over three
/// steps, totalling 50, with a remainder of exactly zero.
async fn a_60_hz_world_with_a_1_ms_tick_runs_16_17_17(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(3)).await.unwrap();
    let transitions = &f.harness.coordinator.trace.transitions;
    assert_eq!(transitions.len(), 3);
    for agent in [fly_a(), fly_b()] {
        let ticks: Vec<u64> = transitions
            .iter()
            .map(|t| {
                t.behaviour
                    .agents
                    .iter()
                    .find(|a| a.agent_id == agent)
                    .expect("the agent is in every transition")
                    .ticks_advanced
            })
            .collect();
        assert_eq!(ticks, vec![16, 17, 17], "{agent} tick profile");
        assert_eq!(ticks.iter().sum::<u64>(), 50);
        let last = transitions
            .last()
            .unwrap()
            .behaviour
            .agents
            .iter()
            .find(|a| a.agent_id == agent)
            .unwrap();
        assert!(last.remainder.is_zero(), "{agent} remainder after three steps");
        // Warm-up ticks are counted too, so brainTicks is warm-up plus the 50 gameplay ticks.
        assert_eq!(last.brain_ticks, 50 + f.harness.config.warmup_ticks);
    }
    f.shutdown().await;
}

/// A pause arriving mid-step means "finish this transition, then pause", and it pauses at the
/// committed boundary rather than truncating anything.
async fn a_pause_mid_step_completes_the_step_and_pauses_at_the_boundary(via: Via) {
    let mut config = HarnessConfig::default();
    // Both agents hold their Commit open, so the pause request lands inside the transition.
    config.agents[0].faults = AgentFaults { commit_delay_ms: 40, ..AgentFaults::default() };
    config.agents[1].faults = AgentFaults { commit_delay_ms: 60, ..AgentFaults::default() };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("step", f.harness.coordinator.step()).await.unwrap();

    // A supervisor asks for a pause while the second transition is still running.
    let handle = f.harness.coordinator.pause_handle();
    let asked = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        handle.request();
    });
    let report = within("paused step", f.harness.coordinator.step()).await.unwrap();
    asked.await.unwrap();

    // The transition completed and the session paused at its committed boundary.
    assert_eq!(report.boundary, 2);
    assert!(report.paused);
    assert_eq!(f.harness.coordinator.phase(), Phase::Paused(2));
    assert!(f.harness.coordinator.phase().is_committed_boundary());
    assert_eq!(f.harness.coordinator.stats().advances, 2, "the pause truncated no transition");
    let audit = f.harness.coordinator.audit.clone();
    let pause = at(&audit, "pause:2");
    for agent in [fly_a(), fly_b()] {
        assert!(at(&audit, &format!("committed:{agent}@1")) < pause);
    }
    assert!(at(&audit, "publish:2") < pause);

    // A paused worker retains its state and answers Status; the world does not advance.
    let env = f.harness.coordinator.environment_ref().clone();
    let status = within("status", f.harness.coordinator.status(&env)).await.unwrap();
    assert_eq!(status.current_scope.unwrap().step, 2);
    assert_eq!(f.harness.coordinator.stats().advances, 2);

    f.harness.coordinator.resume().unwrap();
    assert_eq!(f.harness.coordinator.phase(), Phase::Ready(2));
    let report = within("resumed step", f.harness.coordinator.step()).await.unwrap();
    assert_eq!(report.boundary, 3);
    assert!(!report.paused, "the pause request was consumed by the pause it caused");
    f.shutdown().await;
}

/// Bootstrap and warm-up mutate the fake brains but cannot advance the environment or produce
/// a gameplay reward.
async fn bootstrap_cannot_advance_the_world_or_produce_a_reward(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    assert_eq!(f.harness.coordinator.phase(), Phase::Ready(0));
    let observation = f.harness.coordinator.observation().unwrap();
    assert_eq!(observation.boundary, 0);
    assert!(observation.world_time.is_zero());
    assert_eq!(f.harness.coordinator.stats().advances, 0);
    assert_eq!(f.harness.coordinator.evaluations(), 0);
    // The environment advanced nothing, so its status is still at boundary 0 with no batch.
    let env = f.harness.coordinator.environment_ref().clone();
    let status = within("status", f.harness.coordinator.status(&env)).await.unwrap();
    assert_eq!(status.current_scope.as_ref().unwrap().step, 0);
    assert!(status.last_batch_id.is_none(), "no batch was ever applied");
    // Warm-up did run, with learning disabled, so the models did mutate.
    for agent in [fly_a(), fly_b()] {
        assert!(
            f.harness.agent_mutations(&agent) >= f.harness.config.warmup_ticks,
            "warm-up ticks are real mutations"
        );
    }
    // And the task ledger has no reward yet.
    let progress = f.harness.coordinator.task_progress();
    assert_eq!(progress.number("totalReward").unwrap(), 0.0);
    assert_eq!(progress.integer("transitions").unwrap(), 0);
    f.shutdown().await;
}

/// The published snapshot represents the committed boundary and labels the transition that
/// just ended.
async fn the_committed_snapshot_names_the_boundary_that_just_ended(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    // The snapshots topic retains its latest value, so a subscriber joining after boundary 0
    // still replays it before the boundaries that follow.
    let observer = f.harness.observer().await.unwrap();
    let topic = f.harness.coordinator.topics().snapshots.clone();
    let mut subscription = observer
        .subscribe(
            &topic,
            flybus::SubscriptionConfig::bounded().in_flight(8).replay(true),
        )
        .await
        .unwrap();
    within("run", f.harness.coordinator.run(2)).await.unwrap();

    let mut boundaries = Vec::new();
    for _ in 0..3 {
        let message = within("snapshot", subscription.next()).await.expect("a snapshot");
        let payload = message.payload().clone();
        let step: u64 = payload["scope"]["step"].as_str().unwrap().parse().unwrap();
        let decisions_present = payload["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| !a["selectedDecision"].is_null());
        boundaries.push((step, decisions_present));
        // The frame the snapshot names travels as an owned attachment.
        if step > 0 {
            let frame = message.artifact("view.arena").expect("the published frame");
            assert_eq!(
                frame.reference().byte_length,
                fly_session::environment::VIEW_WIDTH * fly_session::environment::VIEW_HEIGHT * 4,
                "the published frame is the environment native frame"
            );
        }
    }
    assert_eq!(boundaries[0], (0, false), "boundary 0 has no decision or control");
    assert_eq!(boundaries[1], (1, true));
    assert_eq!(boundaries[2], (2, true));
    f.shutdown().await;
}

/// A terminal task event is evaluated, its rewards committed once, and the session pauses at
/// that boundary before any further gameplay transition.
async fn a_terminal_episode_pauses_at_its_own_boundary(via: Via) {
    let config = HarnessConfig {
        terminal: fly_session::task::Terminal::AfterTransitions(3),
        ..HarnessConfig::default()
    };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let reports = within("run", f.harness.coordinator.run(5)).await.unwrap();
    let last = reports.last().unwrap();
    assert!(last.terminal, "the counter task asked for a terminal transition");
    assert!(last.paused);
    assert_eq!(f.harness.coordinator.phase(), Phase::Paused(last.boundary));
    assert!(f.harness.coordinator.episode_request().is_some());
    // No worker resets itself, and no further gameplay transition is allowed.
    let err = f.harness.coordinator.step().await.expect_err("no transition after terminal");
    assert_eq!(err.error.code, ErrorCode::InvalidPhase);
    f.shutdown().await;
}

/// `Worker.Status` answers with the worker's own committed boundary and progress.
async fn status_answers_with_the_committed_boundary(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(2)).await.unwrap();
    for agent in [fly_a(), fly_b()] {
        let worker = f.harness.coordinator.agent_ref(&agent).cloned().unwrap();
        let status = within("status", f.harness.coordinator.status(&worker)).await.unwrap();
        assert_eq!(status.state, WorkerState::Ready);
        assert_eq!(status.current_scope.unwrap().step, 2);
        let before = status.progress_counter;
        // A status query is not progress.
        let again = within("status", f.harness.coordinator.status(&worker)).await.unwrap();
        assert_eq!(again.progress_counter, before);
    }
    f.shutdown().await;
}

/// `Agent.Initialize` is allowed only on an uninitialized agent.
async fn a_worker_refuses_a_second_initialize(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let err = within("second bootstrap", f.harness.coordinator.bootstrap())
        .await
        .expect_err("the environment is already initialized");
    assert_eq!(err.error.code, ErrorCode::InvalidPhase);
    f.shutdown().await;
}

// -------------------------------------------------------------------------------------------
// Dispatch order equivalence: this one builds its own fixtures per order.

/// `step-v1` section 8: sequential, concurrent and reversed dispatch and completion orders all
/// produce the same behaviour trace, excluding request ids and other operational metadata.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_concurrent_and_reversed_orders_agree() {
    let mut behaviours: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for via in [Via::Memory, Via::Unix] {
        for order in [
            DispatchOrder::Sequential,
            DispatchOrder::Concurrent,
            DispatchOrder::Reversed,
        ] {
            let mut config = HarnessConfig::default();
            // Deliberately unequal completion times, so a concurrent run really does finish
            // out of dispatch order.
            config.agents[0].faults =
                AgentFaults { prepare_delay_ms: 12, commit_delay_ms: 0, ..AgentFaults::default() };
            config.agents[1].faults =
                AgentFaults { prepare_delay_ms: 0, commit_delay_ms: 9, ..AgentFaults::default() };
            let mut f = fixture(via, config).await;
            f.harness.coordinator.dispatch = order;
            within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
            within("run", f.harness.coordinator.run(4)).await.unwrap();
            let behaviour = f.harness.coordinator.trace.behavior();
            assert_eq!(behaviour.len(), 4);
            behaviours.insert(format!("{via:?}/{order:?}"), behaviour);
            // The operational metadata is recorded but is not part of the comparison.
            // The operational metadata is recorded beside the behaviour, not inside it.
            let operational = &f.harness.coordinator.trace.transitions[0].operational;
            assert_eq!(operational.prepare_request_ids.len(), 2);
            assert_eq!(operational.commit_request_ids.len(), 2);
            f.shutdown().await;
        }
    }
    let mut iter = behaviours.iter();
    let (first_name, first) = iter.next().expect("at least one run");
    for (name, behaviour) in iter {
        assert_eq!(
            behaviour, first,
            "{name} produced a different behaviour trace from {first_name}"
        );
    }
}

/// A one-agent composition still runs the same transaction, so the barrier is not two-agent
/// specific.
async fn a_single_agent_composition_runs_the_same_transaction(via: Via) {
    let config = HarnessConfig {
        agents: vec![AgentSpec {
            agent_id: id("fly-a"),
            port_id: id("p1"),
            seed: 7,
            faults: AgentFaults::default(),
        }],
        ..HarnessConfig::default()
    };
    let mut f: Fixture = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(3)).await.unwrap();
    assert_eq!(f.harness.coordinator.stats().advances, 3);
    let ticks: Vec<u64> = f
        .harness
        .coordinator
        .trace
        .transitions
        .iter()
        .map(|t| t.behaviour.agents[0].ticks_advanced)
        .collect();
    assert_eq!(ticks, vec![16, 17, 17]);
    f.shutdown().await;
}
