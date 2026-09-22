//! SESSION-02 acceptance: one agent process per fly and one environment process under the
//! coordinator, compared with the in-process and dedicated-thread variants.
//!
//! Every acceptance bullet is one named test here, generated once per execution mode, so a
//! rule that holds in one process holds across a process boundary too. The two process-mode
//! failure rows of section 4 that SESSION-01 could not reach in one process -- a router
//! restart during a world advance, and an old worker's reply after a restart -- are at the
//! end and run in the separate-process mode.

mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use common::{at, count, fly_a, fly_b, mode_fixture, within};
use fly_session::agent::AgentFaults;
use fly_session::coordinator::{DispatchOrder, Injections};
use fly_session::environment::EnvironmentFaults;
use fly_session::harness::{ExecutionMode, HarnessConfig, Via};
use fly_session::launcher::{ReapOutcome, ThreadBudget};
use fly_session::phase::Phase;
use fly_session::types::*;

all_modes!(
    a_delayed_one_agent_result_holds_the_world,
    a_worker_death_has_a_bounded_diagnosed_outcome,
    a_helper_death_has_a_bounded_diagnosed_outcome,
    an_uncertain_advance_never_creates_a_second_batch,
    a_partial_commit_never_permits_next_step_play,
    every_participant_answers_its_supervisor,
    worker_threads_lie_within_the_launcher_allocation,
);

const STEPS: u64 = 4;

fn two_agents(mode: ExecutionMode) -> HarnessConfig {
    HarnessConfig { mode, ..HarnessConfig::default() }
}

// -------------------------------------------------------------------------------------------
// Acceptance: sequential, reversed and parallel completion produce equivalent traces

/// `step-v1` section 8, across the process boundary: sequential, concurrent and reversed
/// dispatch, in all three execution modes, produce one behaviour trace.
///
/// This reuses the wave-1 comparator -- the behaviour half of the section 8 trace, with
/// request ids, bus correlation and wall time excluded -- so "a process behaves like a task"
/// is the same assertion that "a reordered dispatch behaves like an ordered one" was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_reversed_and_parallel_completion_agree() {
    let mut behaviours: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for mode in ExecutionMode::all() {
        for order in [
            DispatchOrder::Sequential,
            DispatchOrder::Concurrent,
            DispatchOrder::Reversed,
        ] {
            let mut config = two_agents(mode);
            // Deliberately unequal completion times, so a concurrent run really does finish
            // out of dispatch order whichever side of a process boundary the agents are on.
            config.agents[0].faults =
                AgentFaults { prepare_delay_ms: 12, ..AgentFaults::default() };
            config.agents[1].faults = AgentFaults { commit_delay_ms: 9, ..AgentFaults::default() };
            let mut f = mode_fixture(mode, config).await;
            f.harness.coordinator.dispatch = order;
            within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
            within("run", f.harness.coordinator.run(STEPS)).await.unwrap();
            let behaviour = f.harness.coordinator.trace.behavior();
            assert_eq!(behaviour.len() as u64, STEPS);
            behaviours.insert(format!("{}/{order:?}", mode.label()), behaviour);
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

// -------------------------------------------------------------------------------------------
// Acceptance: a delayed one-agent result holds the world

/// One agent takes far longer than the other to prepare. No `Environment.Advance` is sent
/// until every agent is Prepared, and the world is still at its old boundary while the
/// coordinator waits.
async fn a_delayed_one_agent_result_holds_the_world(mode: ExecutionMode) {
    let mut config = two_agents(mode);
    config.agents[1].faults = AgentFaults { prepare_delay_ms: 400, ..AgentFaults::default() };
    let mut f = mode_fixture(mode, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let environment = f.harness.environment_id();
    let before = within("progress", f.harness.progress_of(&environment)).await.unwrap();

    let (coordinator, launcher) = f.harness.parts();
    // The supervisor watches the world while the transition is in flight. That is what a
    // supervisor is for, and `Worker.Status` answers without waiting for a mutation.
    let (stepped, held) = tokio::join!(
        async { within("step", coordinator.step()).await },
        async {
            tokio::time::sleep(Duration::from_millis(120)).await;
            within("status", launcher.health_check(&environment)).await
        }
    );
    let report = stepped.expect("the transition completes once the slow agent answers");
    assert_eq!(report.boundary, 1);
    let held = held.expect("the environment answers its supervisor during the wait");
    assert_eq!(
        held.progress_counter, before,
        "the world may not advance while one agent is still preparing"
    );
    assert_eq!(
        held.state,
        WorkerState::Ready,
        "the environment is at a committed boundary, not advancing"
    );

    // And the ordering the audit records says the same thing from the coordinator's side.
    let audit = f.harness.coordinator.audit.clone();
    let advance = at(&audit, "advance:0");
    for agent in [fly_a(), fly_b()] {
        assert!(
            at(&audit, &format!("prepared:{agent}@0")) < advance,
            "{agent} must be Prepared before the world advances: {audit:?}"
        );
    }
    assert_eq!(f.harness.coordinator.stats().advances, 1);
    f.shutdown().await;
}

// -------------------------------------------------------------------------------------------
// Acceptance: worker or helper death has a bounded diagnosed outcome

/// One agent dies in the middle of its Prepare. The epoch fails with a typed cause naming
/// that agent, within the caller's own budget, and nothing continues on the remainder.
async fn a_worker_death_has_a_bounded_diagnosed_outcome(mode: ExecutionMode) {
    let mut config = two_agents(mode);
    config.agents[1].faults = AgentFaults { prepare_delay_ms: 5_000, ..AgentFaults::default() };
    let mut f = mode_fixture(mode, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let started = Instant::now();

    let (coordinator, launcher) = f.harness.parts();
    let (stepped, reaped) = tokio::join!(
        async { within("step", coordinator.step()).await },
        async {
            tokio::time::sleep(Duration::from_millis(80)).await;
            launcher.kill(&fly_b()).await
        }
    );
    assert_eq!(reaped, ReapOutcome::Terminated);
    let failure = stepped.expect_err("a dead participant is a failed epoch, not a slow one");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the outcome must be bounded, not a hang"
    );
    assert_eq!(
        failure.participant.as_deref(),
        Some(fly_b().as_str()),
        "the failure names the participant: {failure}"
    );
    assert_ne!(
        failure.error.mutation,
        MutationCertainty::None,
        "a participant that died mid-call leaves an uncertain mutation, never a clean none"
    );
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert!(f.harness.coordinator.is_fenced());
    // No partial continuation: no world step, no publication, and no next transition.
    assert_eq!(f.harness.coordinator.stats().advances, 0);
    assert_eq!(count(&f.harness.coordinator.audit, "publish:1"), 0);
    let again = f.harness.coordinator.step().await.expect_err("a fenced epoch takes no step");
    assert_eq!(again.error.code, ErrorCode::InvalidPhase);
    f.shutdown().await;
}

/// The environment helper dies in the middle of the world advance. Same rule: a typed cause
/// naming it, bounded, and no half-transition afterwards.
async fn a_helper_death_has_a_bounded_diagnosed_outcome(mode: ExecutionMode) {
    let config = HarnessConfig {
        environment_faults: EnvironmentFaults {
            advance_delay_ms: 5_000,
            ..EnvironmentFaults::default()
        },
        ..two_agents(mode)
    };
    let mut f = mode_fixture(mode, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let environment = f.harness.environment_id();
    let started = Instant::now();

    let (coordinator, launcher) = f.harness.parts();
    let (stepped, reaped) = tokio::join!(
        async { within("step", coordinator.step()).await },
        async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            launcher.kill(&environment).await
        }
    );
    assert_eq!(reaped, ReapOutcome::Terminated);
    let failure = stepped.expect_err("a dead world is a failed epoch");
    assert!(started.elapsed() < Duration::from_secs(20), "bounded, not a hang");
    assert_eq!(
        failure.participant.as_deref(),
        Some(environment.as_str()),
        "the failure names the participant: {failure}"
    );
    assert_ne!(failure.error.mutation, MutationCertainty::None);
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert!(f.harness.coordinator.is_fenced());
    assert_eq!(f.harness.coordinator.stats().advances, 0);
    // The agents prepared and are not asked to prepare again or to commit anything.
    assert_eq!(f.harness.coordinator.stats().commits, 0);
    assert_eq!(count(&f.harness.coordinator.audit, "publish:1"), 0);
    f.shutdown().await;
}

// -------------------------------------------------------------------------------------------
// Acceptance: an uncertain Advance never creates a second batch

/// The Advance result is lost after the world already stepped. The coordinator resolves the
/// same operation against its original domain request id; the world advances once per
/// transition and the batch is never re-sent as a new one.
async fn an_uncertain_advance_never_creates_a_second_batch(mode: ExecutionMode) {
    let clean = {
        let mut f = mode_fixture(mode, two_agents(mode)).await;
        within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
        within("run", f.harness.coordinator.run(STEPS)).await.unwrap();
        let environment = f.harness.environment_id();
        let world = within("progress", f.harness.progress_of(&environment)).await.unwrap();
        let out = (f.harness.coordinator.trace.behavior(), world);
        f.shutdown().await;
        out
    };

    let mut f = mode_fixture(mode, two_agents(mode)).await;
    f.harness.coordinator.injections = Injections {
        at_step: 2,
        lose_advance_result: true,
        ..Injections::default()
    };
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(STEPS)).await.unwrap();
    let environment = f.harness.environment_id();
    let world = within("progress", f.harness.progress_of(&environment)).await.unwrap();

    assert_eq!(f.harness.coordinator.stats().advances, STEPS, "one advance per transition");
    assert_eq!(
        world, clean.1,
        "the world moved exactly as often as it did without the loss"
    );
    assert_eq!(
        f.harness.coordinator.trace.behavior(),
        clean.0,
        "an uncertain Advance changes no behaviour, so it created no second batch"
    );
    // Every transition has exactly one batch, and every batch id is its own.
    let batches: Vec<Id> = f
        .harness
        .coordinator
        .trace
        .transitions
        .iter()
        .map(|t| t.behaviour.batch_id.clone())
        .collect();
    let unique: std::collections::BTreeSet<Id> = batches.iter().cloned().collect();
    assert_eq!(unique.len(), batches.len(), "one batch id per transition: {batches:?}");
    let injections = f.harness.coordinator.injection_log.clone();
    assert!(
        injections.iter().any(|o| o.what == "lost-advance-result" && o.identical),
        "the loss must happen after dispatch, so the outcome really is uncertain: {injections:?}"
    );
    f.shutdown().await;
}

// -------------------------------------------------------------------------------------------
// Acceptance: a partial Commit never permits next-step play

/// One agent's Commit fails after the other's succeeded. The epoch fails naming that agent,
/// the boundary does not move, nothing is published and there is no next transition.
async fn a_partial_commit_never_permits_next_step_play(mode: ExecutionMode) {
    let mut config = two_agents(mode);
    config.agents[1].faults =
        AgentFaults { fail_commit_at_step: Some(1), ..AgentFaults::default() };
    let mut f = mode_fixture(mode, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("step", f.harness.coordinator.step()).await.unwrap();
    let environment = f.harness.environment_id();

    let failure = within("step", f.harness.coordinator.step())
        .await
        .expect_err("one failed Commit fails the epoch");
    assert_eq!(
        failure.participant.as_deref(),
        Some(fly_b().as_str()),
        "the failure names the agent whose Commit failed: {failure}"
    );
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert!(f.harness.coordinator.is_fenced());

    // The world moved once inside the failing transition -- the Advance is what the Commit
    // follows -- and it moves no further. There is no next-step play on a partial commit.
    let world_before = within("progress", f.harness.progress_of(&environment)).await.unwrap();
    let again = f.harness.coordinator.step().await.expect_err("no play after a partial commit");
    assert_eq!(again.error.code, ErrorCode::InvalidPhase);
    let world_after = within("progress", f.harness.progress_of(&environment)).await.unwrap();
    assert_eq!(world_after, world_before, "no next world step follows a partial commit");
    let status = within("status", f.harness.launcher.health_check(&environment)).await.unwrap();
    assert_eq!(
        status.current_scope.unwrap().step,
        2,
        "the world stays at the boundary the failed transition reached"
    );
    let audit = f.harness.coordinator.audit.clone();
    assert_eq!(count(&audit, "publish:2"), 0);
    assert_eq!(f.harness.coordinator.committed_boundary(), None);
    f.shutdown().await;
}

// -------------------------------------------------------------------------------------------
// Supervision: identity, health and reaping

/// Every participant answers the supervisor with the identity the launcher configured, and
/// stops when it is asked to.
async fn every_participant_answers_its_supervisor(mode: ExecutionMode) {
    let mut f = mode_fixture(mode, two_agents(mode)).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(2)).await.unwrap();

    let environment = f.harness.environment_id();
    for who in [fly_a(), fly_b(), environment.clone()] {
        let worker = f.harness.launcher.worker(&who).expect("a launched participant");
        assert_eq!(worker.identity.worker_id, who);
        assert_eq!(worker.domain_incarnation, worker.identity.incarnation_id);
        assert!(!worker.service_incarnation.is_empty());
        let status = within("health", f.harness.launcher.health_check(&who)).await.unwrap();
        assert_eq!(status.state, WorkerState::Ready, "{who} is healthy at a boundary");
    }
    // The agents carry their configured port identities; the environment owns the ports.
    assert_eq!(
        f.harness.launcher.worker(&fly_a()).unwrap().identity.port_id.as_deref(),
        Some("p1")
    );
    assert_eq!(
        f.harness.launcher.worker(&fly_b()).unwrap().identity.port_id.as_deref(),
        Some("p2")
    );
    assert!(f.harness.launcher.worker(&environment).unwrap().identity.port_id.is_none());

    // A worker that is not the one the caller expects refuses to negotiate at all.
    let worker = f.harness.coordinator.agent_ref(&fly_a()).cloned().unwrap();
    let wrong = serde_json::json!({
        "sessionId": "demo",
        "expectedWorkerId": "fly-z",
        "role": "agent",
        "supportedMajors": [1],
    });
    let err = within(
        "hello",
        f.harness.coordinator.probe_raw(&worker, "Worker.Hello", None, wrong),
    )
    .await
    .expect_err("a worker is not whoever a caller says it is");
    assert_eq!(err.code, ErrorCode::IdentityMismatch);

    // Asking a participant to stop stops it, and the supervisor says which kind of stop it was.
    let outcome = f.harness.launcher.reap(&fly_a(), "test").await;
    assert_eq!(outcome, ReapOutcome::Stopped, "a live participant answers Worker.Shutdown");
    assert_eq!(f.harness.launcher.reap(&fly_a(), "test").await, ReapOutcome::AlreadyGone);
    f.shutdown().await;
}

/// `workers-v1`: `Agent.Initialize`'s `workerThreads` lies within the launcher allocation.
///
/// The budget refuses an allocation it cannot cover before anything is started, and an agent
/// refuses an `Agent.Initialize` asking for more threads than its launcher gave it.
async fn worker_threads_lie_within_the_launcher_allocation(mode: ExecutionMode) {
    // The budget itself: a total, a coordinator reservation, and a refusal that names both.
    let mut budget = ThreadBudget::new(4, 1).unwrap();
    assert_eq!(budget.remaining(), 3);
    assert_eq!(budget.allocate(&id("arena"), 1).unwrap(), 1);
    assert_eq!(budget.allocate(&id("fly-a"), 2).unwrap(), 2);
    let refused = budget.allocate(&id("fly-b"), 1).expect_err("the budget is spent");
    assert_eq!(refused.code, ErrorCode::Busy);
    budget.release(&id("fly-a"));
    assert_eq!(budget.allocate(&id("fly-b"), 1).unwrap(), 1);
    assert_eq!(budget.allocate(&id("fly-b"), 1).expect_err("already held").code, ErrorCode::Conflict);

    // A composition the configured budget cannot cover never starts.
    let config = HarnessConfig {
        mode,
        thread_budget: Some(2),
        ..HarnessConfig::default()
    };
    let dir = tempfile::tempdir().expect("a temporary directory");
    let refused = fly_session::harness::SessionHarness::start(Via::Unix, dir.path(), config).await;
    let refused = refused.err().expect("two threads cannot hold a coordinator, a world and two flies");
    assert_eq!(refused.code, flybus::ErrorCode::QuotaExceeded, "{}", refused.message);
    drop(dir);

    // And the worker's own check: it was launched with one thread, so an Initialize asking
    // for eight is refused before the model is constructed.
    let mut f = mode_fixture(mode, two_agents(mode)).await;
    let worker = f.harness.coordinator.agent_ref(&fly_a()).cloned().unwrap();
    let profile = fly_session::agent::synthetic_profile(
        &fly_a(),
        &millis(1).unwrap(),
        f.harness.config.warmup_ticks,
    );
    let params = serde_json::json!({
        "agentId": "fly-a",
        "profile": profile.to_json(),
        "seed": 7,
        "initialInput": {"boundary": "0", "views": [], "structured": null},
        "initialDecisionContext": {
            "schema": fly_session::task::context_schema().to_json(),
            "value": {},
        },
        "workerThreads": 8,
    });
    let err = within(
        "initialize",
        f.harness.coordinator.probe_raw(
            &worker,
            "Agent.Initialize",
            Some(scope_at("demo", "e1", 0)),
            params,
        ),
    )
    .await
    .expect_err("eight threads are not within a one-thread allocation");
    assert_eq!(err.code, ErrorCode::Busy);
    assert_eq!(err.mutation, MutationCertainty::None, "nothing was constructed");
    // The allocation the coordinator actually sends is the one the launcher handed out.
    assert_eq!(f.harness.launcher.worker(&fly_a()).unwrap().identity.worker_threads, 1);
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    f.shutdown().await;
}

// -------------------------------------------------------------------------------------------
// Section 4 rows SESSION-01 could not reach in one process

/// Row: "Router restarts during a world advance | Old handles/routes invalid; epoch fails and
/// restores coherently."
///
/// The restore half is STATE-01's. What SESSION-02 establishes is the half before it: the
/// epoch fails with a typed cause naming the participant the coordinator was talking to, the
/// session is fenced, every artifact handle of that store incarnation is gone, and no
/// boundary, publication or further transition follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_router_restart_during_a_world_advance_fences_the_epoch() {
    let mode = ExecutionMode::Process;
    let config = HarnessConfig {
        environment_faults: EnvironmentFaults {
            advance_delay_ms: 3_000,
            ..EnvironmentFaults::default()
        },
        ..two_agents(mode)
    };
    let mut f = mode_fixture(mode, config).await;
    // The router is gone in a moment, so the supervisor must not spend its full budget
    // asking a participant that can no longer be reached.
    f.harness.launcher.set_health_policy(fly_session::launcher::HealthPolicy {
        probe: Duration::from_millis(200),
        fail: Duration::from_millis(500),
        boot: Duration::from_secs(30),
    });
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let boundary_before = f.harness.coordinator.observation().unwrap().boundary;
    let router = f.harness.router().clone();
    let started = Instant::now();

    let (coordinator, _launcher) = f.harness.parts();
    let (stepped, ()) = tokio::join!(
        async { within("step", coordinator.step()).await },
        async {
            // Mid-advance: the world has been asked to move and has not answered yet.
            tokio::time::sleep(Duration::from_millis(250)).await;
            router.shutdown();
        }
    );
    let failure = stepped.expect_err("a lost router fails the epoch");
    assert!(started.elapsed() < Duration::from_secs(20), "bounded, not a hang");
    assert_eq!(
        failure.participant.as_deref(),
        Some(f.harness.environment_id().as_str()),
        "the failure names the participant the coordinator was waiting for: {failure}"
    );
    assert_ne!(
        failure.error.mutation,
        MutationCertainty::None,
        "the world may have stepped; a lost router is never proof that it did not"
    );
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert!(
        f.harness.coordinator.is_fenced(),
        "old handles and routes are invalid from here on"
    );
    assert_eq!(f.harness.coordinator.stats().advances, 0, "no boundary was committed");
    assert_eq!(count(&f.harness.coordinator.audit, "publish:1"), 0);
    assert_eq!(
        f.harness.coordinator.observation().unwrap().boundary,
        boundary_before,
        "the committed observation is still the one from before the advance"
    );
    // Nothing reconnects into the active epoch: a new call on the old route is refused.
    let again = f.harness.coordinator.step().await.expect_err("a fenced epoch takes no step");
    assert_eq!(again.error.code, ErrorCode::InvalidPhase);
    f.shutdown().await;
}

/// Row: "Old worker replies after restore | Stale epoch/incarnation rejected", with real
/// processes.
///
/// A restarted agent is a new process, a new registration and a new domain incarnation. The
/// coordinator pinned the old registration, so its next call fails rather than reaching the
/// replacement; and the replacement, followed deliberately, refuses an operation from the
/// epoch the old process belonged to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_old_worker_reply_after_a_restart_is_rejected_on_stale_epoch_or_incarnation() {
    let mode = ExecutionMode::Process;
    let mut f = mode_fixture(mode, two_agents(mode)).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("step", f.harness.coordinator.step()).await.unwrap();

    let old = f.harness.coordinator.agent_ref(&fly_b()).cloned().unwrap();
    let old_pid = f.harness.launcher.worker(&fly_b()).unwrap().pid;
    assert!(old_pid.is_some(), "a separate-process agent has a process of its own");
    let restarted = f.harness.restart_agent(&fly_b()).await.unwrap();
    let new_pid = f.harness.launcher.worker(&fly_b()).unwrap().pid;
    assert_ne!(old_pid, new_pid, "a restart is a new process");
    assert_ne!(
        restarted.service_incarnation, old.bus_incarnation,
        "a replacement registration is a new incarnation"
    );

    // Following the new registration while still pinning the old worker's negotiated
    // incarnation is rejected: this is the shape an old worker's reply would arrive in.
    let stale = fly_session::rpc::WorkerRef {
        service: restarted.service.clone(),
        bus_incarnation: restarted.service_incarnation.clone(),
        worker_id: fly_b(),
        domain_incarnation: old.domain_incarnation.clone(),
    };
    assert_ne!(old.domain_incarnation, Some(restarted.incarnation_id.clone()));
    let err = within("status", f.harness.coordinator.status(&stale))
        .await
        .expect_err("the replacement is not the incarnation this epoch negotiated");
    assert_eq!(err.error.code, ErrorCode::IdentityMismatch);
    assert_eq!(err.participant.as_deref(), Some(fly_b().as_str()));
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert!(f.harness.coordinator.is_fenced());
    assert_eq!(f.harness.coordinator.stats().advances, 1, "no world step under a lost pin");
    f.shutdown().await;
}

/// The other half of the same row: the replacement process is live and refuses an operation
/// naming the epoch the old process belonged to, rather than applying it to a fresh brain.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_worker_refuses_an_operation_from_the_old_epoch() {
    let mode = ExecutionMode::Process;
    let mut f = mode_fixture(mode, two_agents(mode)).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("step", f.harness.coordinator.step()).await.unwrap();
    let restarted = f.harness.restart_agent(&fly_b()).await.unwrap();

    let replacement = fly_session::rpc::WorkerRef::new(
        &restarted.service,
        &restarted.service_incarnation,
        &fly_b(),
    );
    let params = serde_json::json!({
        "agentId": "fly-b",
        "profileDigest": digest_of_bytes(b"whatever"),
        "interval": {"numerator": "16666667", "denominator": "1"},
        "decisionContextDigest": digest_of_bytes(b"whatever"),
        "preStepStimulations": [],
    });
    let err = within(
        "stale epoch",
        f.harness.coordinator.probe_raw(
            &replacement,
            "Agent.Prepare",
            Some(scope_at("demo", "e1", 1)),
            params,
        ),
    )
    .await
    .expect_err("an uninitialized replacement has no epoch to prepare in");
    assert!(
        matches!(err.code, ErrorCode::StaleEpoch | ErrorCode::InvalidPhase),
        "a replacement refuses the old epoch's work: {err}"
    );
    assert_eq!(err.mutation, MutationCertainty::None, "nothing was applied to a fresh brain");
    assert_eq!(f.harness.coordinator.stats().advances, 1);
    f.shutdown().await;
}
