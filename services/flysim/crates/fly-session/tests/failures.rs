//! The failure-injection rows of the implementation guide's section 4 that apply to
//! SESSION-01, plus the `step-v1` section 7 rules they enforce.
//!
//! Most rows are proved by comparing an injected run with a clean run of the same
//! composition: same seeds, same cadence, same number of steps. If the injected run's
//! behaviour trace, model mutation counts and world counter are identical, then the injected
//! message added no tick, no RNG draw, no stimulation, no reward and no world step.

mod common;

use common::{Fixture, at, fly_a, fly_b, within};
use fly_session::agent::AgentFaults;
use fly_session::coordinator::Injections;
use fly_session::environment::EnvironmentFaults;
use fly_session::harness::{HarnessConfig, Via};
use fly_session::phase::Phase;
use fly_session::types::*;

both_transports!(
    a_duplicate_prepare_after_a_lost_reply_repeats_nothing,
    a_duplicate_commit_replays_without_a_second_reinforcement,
    the_same_batch_with_altered_controls_conflicts,
    a_lost_advance_result_resolves_the_same_operation,
    a_cached_artifact_consumed_by_its_first_caller_survives_a_retry,
    one_commit_failing_after_another_succeeds_fails_the_epoch,
    a_replaced_registration_is_not_silently_reached,
    a_reply_from_another_incarnation_is_rejected,
    a_world_that_advanced_without_sensory_data_fails_the_transition,
    an_exact_duplicate_of_a_running_operation_is_in_progress,
    an_old_epoch_operation_is_refused_with_stale_epoch,
);

const STEPS: u64 = 4;
const INJECT_AT: u64 = 2;

/// The mutation counter of a participant running in this process. These suites are all
/// in-process compositions, so it is always there; SESSION-02's are not, and read it over the
/// bus instead.
fn local_mutations(f: &Fixture, agent_id: &Id) -> u64 {
    f.harness
        .agent_mutations(agent_id)
        .expect("an in-process participant keeps its counter in this process")
}

/// What a run of the standard composition produced.
struct Run {
    behaviour: Vec<String>,
    mutations: Vec<(Id, u64)>,
    counter: i64,
    advances: u64,
    injections: Vec<fly_session::coordinator::InjectionOutcome>,
    in_progress: u64,
}

async fn run_with(via: Via, injections: Injections) -> Run {
    let mut f = clean_fixture(via).await;
    f.harness.coordinator.injections = injections;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(STEPS)).await.unwrap();
    let run = Run {
        behaviour: f.harness.coordinator.trace.behavior(),
        mutations: vec![
            (fly_a(), local_mutations(&f, &fly_a())),
            (fly_b(), local_mutations(&f, &fly_b())),
        ],
        counter: f
            .harness
            .coordinator
            .task_progress()
            .integer("counter")
            .unwrap(),
        advances: f.harness.coordinator.stats().advances,
        injections: f.harness.coordinator.injection_log.clone(),
        in_progress: f.harness.coordinator.in_progress_replies,
    };
    f.shutdown().await;
    run
}

async fn clean_fixture(via: Via) -> Fixture {
    common::fixture(via, HarnessConfig::default()).await
}

fn assert_same(injected: &Run, clean: &Run, what: &str) {
    assert_eq!(injected.behaviour, clean.behaviour, "{what}: behaviour trace");
    assert_eq!(injected.mutations, clean.mutations, "{what}: model mutations");
    assert_eq!(injected.counter, clean.counter, "{what}: world counter");
    assert_eq!(injected.advances, clean.advances, "{what}: world advances");
    assert_eq!(injected.advances, STEPS, "{what}: one advance per batch");
    assert_eq!(clean.in_progress, 0, "{what}: a clean run never meets a duplicate");
}

/// Row: duplicate Prepare after a lost reply. No extra ticks, RNG draws, stimulation or
/// decode, and the cached decision comes back unchanged.
async fn a_duplicate_prepare_after_a_lost_reply_repeats_nothing(via: Via) {
    let clean = run_with(via, Injections::default()).await;
    let injected = run_with(
        via,
        Injections {
            at_step: INJECT_AT,
            duplicate_prepare: Some(fly_a()),
            ..Injections::default()
        },
    )
    .await;
    let probe = injected
        .injections
        .iter()
        .find(|o| o.what == "duplicate-prepare")
        .expect("the duplicate was sent");
    assert_eq!(probe.code, None, "a safe replay is a success, not an error");
    assert!(probe.identical, "the replay returned the same decision, ticks and remainder");
    assert_same(&injected, &clean, "duplicate prepare");
}

/// Row: the same Commit again. It replays its cached reply and reinforces nothing twice.
async fn a_duplicate_commit_replays_without_a_second_reinforcement(via: Via) {
    let clean = run_with(via, Injections::default()).await;
    let injected = run_with(
        via,
        Injections {
            at_step: INJECT_AT,
            duplicate_commit: Some(fly_b()),
            ..Injections::default()
        },
    )
    .await;
    let probe = injected
        .injections
        .iter()
        .find(|o| o.what == "duplicate-commit")
        .expect("the duplicate was sent");
    assert_eq!(probe.code, None);
    assert!(probe.identical, "the replay returned the same committed step and telemetry");
    assert_same(&injected, &clean, "duplicate commit");
}

/// Row: the same batch with altered controls. A conflict, never a second world mutation.
async fn the_same_batch_with_altered_controls_conflicts(via: Via) {
    let clean = run_with(via, Injections::default()).await;
    let injected = run_with(
        via,
        Injections {
            at_step: INJECT_AT,
            altered_advance_controls: true,
            ..Injections::default()
        },
    )
    .await;
    let probe = injected
        .injections
        .iter()
        .find(|o| o.what == "altered-advance-controls")
        .expect("the altered batch was sent");
    assert_eq!(probe.code, Some(ErrorCode::Conflict));
    assert_same(&injected, &clean, "altered controls");
}

/// Row: the Advance result is lost after the world stepped. The same operation is resolved
/// against its original request id; no new batch is ever sent.
async fn a_lost_advance_result_resolves_the_same_operation(via: Via) {
    let clean = run_with(via, Injections::default()).await;
    let injected = run_with(
        via,
        Injections { at_step: INJECT_AT, lose_advance_result: true, ..Injections::default() },
    )
    .await;
    assert!(
        injected
            .injections
            .iter()
            .any(|o| o.what == "lost-advance-result" && o.identical),
        "the call was abandoned after dispatch, with an uncertain outcome"
    );
    assert!(
        injected.injections.iter().any(|o| o.what == "status-after-loss" && o.identical),
        "the uncertain call was probed with Status before being resolved"
    );
    assert_same(&injected, &clean, "lost advance result");
}

/// Row: the cached RPC artifact is consumed by its first caller. The endpoint's domain cache
/// still owns it, so the retry gets valid bytes.
async fn a_cached_artifact_consumed_by_its_first_caller_survives_a_retry(via: Via) {
    let clean = run_with(via, Injections::default()).await;
    let injected = run_with(
        via,
        Injections {
            at_step: INJECT_AT,
            consume_advance_artifact_then_retry: true,
            ..Injections::default()
        },
    )
    .await;
    let probe = injected
        .injections
        .iter()
        .find(|o| o.what == "cached-artifact-after-consumption")
        .expect("the frame was read, released and replayed");
    assert!(probe.identical, "the replayed frame has the same bytes as the consumed one");
    assert_same(&injected, &clean, "cached artifact");
}

/// Row: one Commit fails after another succeeds. No next world step, and the epoch fails
/// rather than continuing with a partial match.
async fn one_commit_failing_after_another_succeeds_fails_the_epoch(via: Via) {
    let mut config = HarnessConfig::default();
    // fly-a commits quickly and succeeds; fly-b fails after its next input was installed.
    config.agents[1].faults =
        AgentFaults { fail_commit_at_step: Some(1), commit_delay_ms: 15, ..AgentFaults::default() };
    let mut f = common::fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("step", f.harness.coordinator.step()).await.unwrap();
    let err = within("failing step", f.harness.coordinator.step())
        .await
        .expect_err("the epoch fails when one commit fails");
    assert_eq!(err.error.code, ErrorCode::BackendFailure);
    assert_eq!(err.error.mutation, MutationCertainty::Applied);
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);

    // The world took the step whose commits failed, and it takes no further step.
    let advances = f.harness.coordinator.stats().advances;
    assert_eq!(advances, 1, "the failed transition never reached a committed boundary");
    let env = f.harness.coordinator.environment_ref().clone();
    let status = within("status", f.harness.coordinator.status(&env)).await.unwrap();
    assert_eq!(status.current_scope.as_ref().unwrap().step, 2);
    let again = f.harness.coordinator.step().await.expect_err("no step from Failed");
    assert_eq!(again.error.code, ErrorCode::InvalidPhase);
    let status = within("status", f.harness.coordinator.status(&env)).await.unwrap();
    assert_eq!(
        status.current_scope.unwrap().step,
        2,
        "no world step follows a partial commit"
    );

    // The agent that succeeded is at the new boundary; the one that failed reports Failed.
    let a = f.harness.coordinator.agent_ref(&fly_a()).cloned().unwrap();
    let status = within("status", f.harness.coordinator.status(&a)).await.unwrap();
    assert_eq!(status.current_scope.unwrap().step, 2);
    assert_eq!(
        f.harness.agent_status(&fly_b()).unwrap().state(),
        WorkerState::Failed
    );
    // Nothing was published for the boundary that failed to commit.
    let audit = f.harness.coordinator.audit.clone();
    assert!(!audit.iter().any(|entry| entry == "publish:2"));
    assert!(at(&audit, "publish:1") < at(&audit, "advance:1"));
    f.shutdown().await;
}

/// Row: an old worker is replaced. The coordinator pinned the old registration, so its next
/// call fails rather than silently reaching another brain.
async fn a_replaced_registration_is_not_silently_reached(via: Via) {
    let mut f = clean_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("step", f.harness.coordinator.step()).await.unwrap();
    let restarted = f.harness.restart_agent(&fly_b()).await.unwrap();
    let pinned = f.harness.coordinator.agent_ref(&fly_b()).cloned().unwrap();
    assert_ne!(
        restarted.service_incarnation, pinned.bus_incarnation,
        "a replacement registration is a new incarnation"
    );
    let err = within("step after restart", f.harness.coordinator.step())
        .await
        .expect_err("the pinned incarnation is gone");
    assert_eq!(err.error.code, ErrorCode::IdentityMismatch);
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert_eq!(f.harness.coordinator.stats().advances, 1, "no world step under a lost pin");
    f.shutdown().await;
}

/// Row: a reply carrying another domain incarnation is rejected, even when the bus route is
/// live and answering.
async fn a_reply_from_another_incarnation_is_rejected(via: Via) {
    let mut f = clean_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let old = f.harness.coordinator.agent_ref(&fly_b()).cloned().unwrap();
    let restarted = f.harness.restart_agent(&fly_b()).await.unwrap();
    // Follow the new registration, but keep pinning the incarnation the old worker negotiated.
    let stale = fly_session::rpc::WorkerRef {
        service: restarted.service.clone(),
        bus_incarnation: restarted.service_incarnation.clone(),
        worker_id: fly_b(),
        domain_incarnation: old.domain_incarnation.clone(),
    };
    assert_ne!(old.domain_incarnation, Some(restarted.incarnation_id.clone()));
    let err = within("status", f.harness.coordinator.status(&stale))
        .await
        .expect_err("the replacement is not the negotiated incarnation");
    assert_eq!(err.error.code, ErrorCode::IdentityMismatch);
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    f.shutdown().await;
}

/// `step-v1` section 7: the world advanced but the required sensory data is unavailable. The
/// transition fails; nothing is rewarded or continued on guessed input.
async fn a_world_that_advanced_without_sensory_data_fails_the_transition(via: Via) {
    let config = HarnessConfig {
        environment_faults: EnvironmentFaults {
            omit_view_at_boundary: Some(1),
            ..EnvironmentFaults::default()
        },
        ..HarnessConfig::default()
    };
    let mut f = common::fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let err = within("step", f.harness.coordinator.step())
        .await
        .expect_err("a missing required view is not silently replaced");
    assert_eq!(err.error.code, ErrorCode::BufferInvalid);
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    // The task never interpreted the transition, so nothing was rewarded.
    assert_eq!(f.harness.coordinator.evaluations(), 0);
    assert_eq!(
        f.harness.coordinator.task_progress().number("totalReward").unwrap(),
        0.0
    );
    let audit = f.harness.coordinator.audit.clone();
    assert!(!audit.iter().any(|entry| entry.starts_with("committed:")));
    assert!(!audit.iter().any(|entry| entry == "publish:1"));
    f.shutdown().await;
}

/// `ipc-v1` section 5: an exact duplicate arriving while the original is still executing gets
/// IN_PROGRESS for that bus call, and the original completes normally.
async fn an_exact_duplicate_of_a_running_operation_is_in_progress(via: Via) {
    let config = HarnessConfig {
        environment_faults: EnvironmentFaults {
            // The world moves, then the reply is held, so the resolution attempt lands while
            // the original operation is still active.
            advance_delay_ms: 120,
            ..EnvironmentFaults::default()
        },
        ..HarnessConfig::default()
    };
    let mut f = common::fixture(via, config).await;
    f.harness.coordinator.injections =
        Injections { at_step: 0, lose_advance_result: true, ..Injections::default() };
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("step", f.harness.coordinator.step()).await.unwrap();
    assert!(
        f.harness.coordinator.in_progress_replies > 0,
        "the duplicate met the original still running"
    );
    // And the original still completed: exactly one world step, at one boundary.
    assert_eq!(f.harness.coordinator.stats().advances, 1);
    assert_eq!(f.harness.coordinator.observation().unwrap().boundary, 1);
    f.shutdown().await;
}

/// A live worker under this epoch refuses an operation naming another one.
async fn an_old_epoch_operation_is_refused_with_stale_epoch(via: Via) {
    let mut f = clean_fixture(via).await;
    let harness = &mut f.harness;
    within("bootstrap", harness.coordinator.bootstrap()).await.unwrap();
    within("step", harness.coordinator.step()).await.unwrap();

    // The agent is live and initialized under epoch e1. An operation naming another epoch is
    // refused as stale rather than applied to this brain.
    let worker = harness.coordinator.agent_ref(&fly_a()).cloned().unwrap();
    let scope = scope_at("demo", "e0", 1);
    let params = serde_json::json!({
        "agentId": "fly-a",
        "profileDigest": digest_of_bytes(b"whatever").to_string(),
        "interval": {"numerator": "16666667", "denominator": "1"},
        "decisionContextDigest": digest_of_bytes(b"whatever").to_string(),
        "preStepStimulations": [],
    });
    let bus = harness.client("coordinator2").await;
    // The launcher grants no second coordinator, so the old-epoch probe goes through the
    // session's own client instead.
    assert!(bus.is_err(), "an unconfigured client id is refused before it can route");
    let err = within(
        "stale epoch",
        harness.coordinator.probe_raw(&worker, "Agent.Prepare", Some(scope), params),
    )
    .await
    .expect_err("an old epoch cannot mutate this worker");
    assert_eq!(err.code, ErrorCode::StaleEpoch);
    assert_eq!(harness.coordinator.stats().advances, 1);
    f.shutdown().await;
}
