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
use fly_session::harness::{AgentSpec, ExecutionMode, HarnessConfig, Via};
use fly_session::launcher::{ReapOutcome, ThreadBudget};
use fly_session::ResolutionEnd;
use fly_session::phase::Phase;
use fly_session::types::*;

all_modes!(
    an_acknowledge_that_releases_nothing_is_not_a_failure,
    bootstrap_survives_the_second_acknowledge_its_resolution_makes,
    a_slow_participant_is_resolved_rather_than_failed,
    a_resolution_says_which_of_its_two_bounds_ended_it,
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
// ipc-v1 section 5: an Acknowledge that releases nothing is success

/// `ipc-v1` section 5: "Already released/unknown IDs are ignored."
///
/// A second `Worker.Acknowledge` of ids the worker has already released answers with an empty
/// list. That is the contract working, not a worker misbehaving, and the coordinator must
/// accept it and carry on. The session's own bootstrap releases every lifecycle reply, so
/// asking again for the same ids is exactly that case -- driven directly here rather than by
/// making something slow, because it is a rule about the reply and not about timing.
///
/// The rule has teeth because of section 6: any Acknowledge whose reply outruns the probe is
/// resolved, and the resolution *is* a second Acknowledge of the same ids. A coordinator that
/// demands the whole list back therefore fences a healthy session the first time a worker is
/// slow to answer. It did, on this branch's parent; this test fails if that check returns.
async fn an_acknowledge_that_releases_nothing_is_not_a_failure(mode: ExecutionMode) {
    let mut f = mode_fixture(mode, two_agents(mode)).await;
    // Bootstrap acknowledges every lifecycle reply, so afterwards the worker holds none.
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let worker = f.harness.coordinator.agent_ref(&fly_a()).cloned().unwrap();

    // The ids bootstrap already released. The worker ignores them and releases nothing.
    let already: Vec<DomainRequestId> =
        (1..=3).map(DomainRequestId::from_serial).collect();
    let released = within(
        "acknowledge",
        f.harness.coordinator.acknowledge_replies(&worker, &already),
    )
    .await
    .expect("a second Acknowledge of released ids is success, not a failed epoch");
    assert!(
        released.is_empty(),
        "already released ids are ignored, so this call released nothing: {released:?}"
    );

    // The session is untouched by it: not fenced, still at its boundary, and still plays.
    assert!(!f.harness.coordinator.is_fenced(), "an empty acknowledgment is not a fault");
    assert_eq!(f.harness.coordinator.phase(), Phase::Ready(0));
    let report = within("step", f.harness.coordinator.step())
        .await
        .expect("the session continues after an Acknowledge that released nothing");
    assert_eq!(report.boundary, 1);
    assert_eq!(f.harness.coordinator.stats().advances, 1);
    f.shutdown().await;
}

/// The same rule, on the path `bootstrap` actually uses.
///
/// The test above calls `acknowledge_replies` directly, which guards the check where it lives
/// now but not where it lived before: a length check reintroduced into `acknowledge_lifecycle`
/// after that call would leave it green. This one drives bootstrap itself, with the
/// `duplicate_lifecycle_acknowledge` injection doing exactly what the section 6 resolution
/// does -- the same ids again, to a worker that has already released them -- so the second,
/// empty answer has to be accepted by every check on bootstrap's path.
async fn bootstrap_survives_the_second_acknowledge_its_resolution_makes(mode: ExecutionMode) {
    let mut f = mode_fixture(mode, two_agents(mode)).await;
    f.harness.coordinator.injections = Injections {
        duplicate_lifecycle_acknowledge: true,
        ..Injections::default()
    };
    within("bootstrap", f.harness.coordinator.bootstrap())
        .await
        .expect("bootstrap accepts the second, empty acknowledgment of its own lifecycle ids");
    assert!(!f.harness.coordinator.is_fenced());
    assert_eq!(f.harness.coordinator.phase(), Phase::Ready(0));
    let report = within("step", f.harness.coordinator.step()).await.expect("and still plays");
    assert_eq!(report.boundary, 1);
    f.shutdown().await;
}

/// The other half of the rule: a short list is accepted, an id outside the request is not.
///
/// A worker reports what *it* released, so fewer ids than asked for is success -- but it is
/// only entitled to report about the ids it was asked about. An id from outside the request is
/// a worker talking about another caller's cache, and
/// `AcknowledgeResult::validate_against` is what refuses it. Without this, dropping the length
/// check left nothing checking the reply against the request at all.
///
/// Not generated per mode, deliberately. The check is the *caller's*, so the mode of the
/// worker that misbehaves is irrelevant to it, and the alternative -- carrying the
/// misbehaviour to a separate process over argv -- would put a flag in the shipped binary
/// whose only purpose is to make a worker lie about its acknowledgments.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_acknowledged_id_outside_the_request_is_refused() {
    let mode = ExecutionMode::InProcess;
    let mut config = two_agents(mode);
    // This worker adds an id nobody asked about to every acknowledgment.
    config.agents[0].faults = AgentFaults {
        acknowledge_extra_id: Some(id("req-9999")),
        ..AgentFaults::default()
    };
    let mut f = mode_fixture(mode, config).await;
    let failure = within("bootstrap", f.harness.coordinator.bootstrap())
        .await
        .expect_err("a worker may not acknowledge an id this session never asked about");
    assert_eq!(failure.error.code, ErrorCode::IdentityMismatch);
    assert!(
        failure.error.message.contains("never asked about"),
        "the refusal says what was wrong: {failure}"
    );
    assert_eq!(failure.detail, "acknowledge");
    assert_eq!(failure.error.mutation, MutationCertainty::None, "refused before any mutation");
    f.shutdown().await;
}

// -------------------------------------------------------------------------------------------
// ipc-v1 section 6: an uncertain call is resolved, not failed

/// A participant that is merely slow -- slower than the caller's probe, faster than the
/// resolution's budget -- finishes its step. The epoch is not lost, and the resolution adds no
/// second operation.
///
/// This is the `ipc-v1` section 6 procedure on the path that actually reaches it: the probe
/// expires, the coordinator queries the same request id against the same incarnation, the
/// worker answers `IN_PROGRESS` while its original is still running and then replays its
/// cached reply. `step-v1` section 7's Advance row is the same rule, so the world is slow here
/// too and its batch is never re-sent as a new one.
async fn a_slow_participant_is_resolved_rather_than_failed(mode: ExecutionMode) {
    // A clean run of the same composition, to compare against.
    let clean = {
        let mut f = mode_fixture(mode, two_agents(mode)).await;
        within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
        within("run", f.harness.coordinator.run(2)).await.unwrap();
        let environment = f.harness.environment_id();
        let world = within("progress", f.harness.progress_of(&environment)).await.unwrap();
        let out = (f.harness.coordinator.trace.behavior(), world);
        f.shutdown().await;
        out
    };

    let mut config = two_agents(mode);
    config.agents[1].faults = AgentFaults { prepare_delay_ms: 500, ..AgentFaults::default() };
    config.environment_faults =
        EnvironmentFaults { advance_delay_ms: 500, ..EnvironmentFaults::default() };
    let mut f = mode_fixture(mode, config).await;
    // Bootstrap first, at ordinary deadlines: its lifecycle calls are not what this test is
    // about, and squeezing them through the probe below only tests the machine's luck.
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    // A probe well inside both delays, and a resolution budget well outside them: the point is
    // a call that expires and an operation that is nevertheless fine. The guard is out of
    // reach so the budget is the only bound in play, and the budget is far above what the
    // delays need, so neither ends this resolution -- the answer does.
    f.harness.coordinator.deadlines = fly_session::Deadlines {
        probe: Duration::from_millis(120),
        resolve: Duration::from_secs(15),
        resolve_attempts: u32::MAX,
        boot: Duration::from_secs(30),
        capture: Duration::from_secs(30),
        durable: Duration::from_secs(60),
    };
    let reports = within("run", f.harness.coordinator.run(2))
        .await
        .expect("a slow participant is resolved, not failed");

    assert_eq!(reports.len(), 2);
    assert_eq!(f.harness.coordinator.phase(), Phase::Ready(2));
    assert!(!f.harness.coordinator.is_fenced(), "a slow answer is not a lost epoch");
    assert!(
        f.harness.coordinator.resolutions >= 2,
        "both the slow Prepare and the slow Advance must have run the resolution, not {}",
        f.harness.coordinator.resolutions
    );
    assert!(
        f.harness.coordinator.in_progress_replies > 0,
        "the resolution must have met the original still running"
    );
    assert_eq!(
        f.harness.coordinator.last_resolution,
        Some(ResolutionEnd::Answered),
        "the resolution ended by being answered, not by running out of anything"
    );

    // No second operation anywhere: one advance per transition, one batch id per transition,
    // and the same behaviour as the run that never timed out.
    assert_eq!(f.harness.coordinator.stats().advances, 2);
    let environment = f.harness.environment_id();
    let world = within("progress", f.harness.progress_of(&environment)).await.unwrap();
    assert_eq!(world, clean.1, "the world moved exactly as often as in the clean run");
    assert_eq!(
        f.harness.coordinator.trace.behavior(),
        clean.0,
        "resolving an uncertain call changes no behaviour"
    );
    let batches: std::collections::BTreeSet<Id> = f
        .harness
        .coordinator
        .trace
        .transitions
        .iter()
        .map(|t| t.behaviour.batch_id.clone())
        .collect();
    assert_eq!(batches.len(), 2, "one batch id per transition, never a second batch");
    // And the agents took exactly the ticks the clean run took: a resolution is a query.
    for transition in &f.harness.coordinator.trace.transitions {
        for agent in &transition.behaviour.agents {
            assert!(agent.ticks_advanced == 16 || agent.ticks_advanced == 17);
        }
    }
    f.shutdown().await;
}

/// One agent, one port, and a participant that will not answer this side of the test's own
/// timeout. The composition for the bound tests: one participant means one possible name in
/// the failure, so which agent is blamed is not a race.
fn one_silent_agent(mode: ExecutionMode) -> HarnessConfig {
    HarnessConfig {
        agents: vec![AgentSpec {
            // Ten minutes. The suite's own `within` gives up at twenty seconds, so if the step
            // returns at all, a bound ended it and not the participant. That is a claim about
            // the code rather than about how fast this machine happens to be.
            faults: AgentFaults { prepare_delay_ms: 600_000, ..AgentFaults::default() },
            ..AgentSpec::new("fly-a", "p1", 7)
        }],
        mode,
        ..HarnessConfig::default()
    }
}

/// The resolution has two bounds, and which one ended it is never left to be guessed.
///
/// Both halves are arranged so the bound under test is the only one that *can* fire: the
/// other is set orders of magnitude out of reach, so no amount of scheduling delay flips them.
/// The claim is the contract's -- a resolution ends by budget or by guard, records which, and
/// names it in the failure -- and nothing here is timed.
///
/// The deadlines are installed after `bootstrap`, deliberately. Bootstrap makes lifecycle
/// calls of its own, and squeezing them through a fifty-millisecond probe tests the harness's
/// luck rather than the resolution.
async fn a_resolution_says_which_of_its_two_bounds_ended_it(mode: ExecutionMode) {
    // Half one: the budget fires, because the guard cannot. `u32::MAX` attempts at the two
    // millisecond pause is over ninety days; the budget is a fifth of a second.
    let mut f = mode_fixture(mode, one_silent_agent(mode)).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    f.harness.coordinator.deadlines = fly_session::Deadlines {
        probe: Duration::from_millis(50),
        resolve: Duration::from_millis(200),
        resolve_attempts: u32::MAX,
        boot: Duration::from_secs(30),
        capture: Duration::from_secs(30),
        durable: Duration::from_secs(60),
    };
    let failure = within("step", f.harness.coordinator.step())
        .await
        .expect_err("a participant that never answers exhausts the resolution");
    assert_eq!(f.harness.coordinator.last_resolution, Some(ResolutionEnd::BudgetExpired));
    assert!(
        failure.error.message.contains("resolution budget"),
        "the message names the bound that fired: {failure}"
    );
    // The budget ended it with attempts still in hand, which is what makes it the budget. A
    // 200 ms budget at a 50 ms probe cannot spend more than a handful, and `u32::MAX` was
    // never in reach; asserting against the guard's own size would be vacuous.
    let spent = f.harness.coordinator.last_resolution_attempts;
    assert!(spent >= 1, "the resolution made at least one attempt");
    assert!(spent < 100, "and nowhere near its guard: {spent}");
    assert_eq!(failure.participant.as_deref(), Some(fly_a().as_str()));
    assert_eq!(failure.error.mutation, MutationCertainty::Unknown);
    assert!(f.harness.coordinator.is_fenced());
    f.shutdown().await;

    // Half two: the guard fires, because the budget cannot. Three attempts against an hour.
    let mut f = mode_fixture(mode, one_silent_agent(mode)).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    f.harness.coordinator.deadlines = fly_session::Deadlines {
        probe: Duration::from_millis(50),
        resolve: Duration::from_secs(3_600),
        resolve_attempts: 3,
        boot: Duration::from_secs(30),
        capture: Duration::from_secs(30),
        durable: Duration::from_secs(60),
    };
    let failure = within("step", f.harness.coordinator.step())
        .await
        .expect_err("three attempts are not enough to resolve a silent participant");
    assert_eq!(f.harness.coordinator.last_resolution, Some(ResolutionEnd::AttemptsExhausted));
    assert!(
        failure.error.message.contains("attempt guard")
            && failure.error.message.contains("3 attempts"),
        "the message names the bound that fired and its size: {failure}"
    );
    // Counted, not timed: the guard was spent exactly, and the hour never came near.
    assert_eq!(f.harness.coordinator.last_resolution_attempts, 3);
    assert_eq!(failure.participant.as_deref(), Some(fly_a().as_str()));
    assert_eq!(failure.error.mutation, MutationCertainty::Unknown);
    assert!(f.harness.coordinator.is_fenced(), "an exhausted guard fences the epoch too");
    f.shutdown().await;
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

/// Waits until `worker` is provably inside the operation, then kills it.
///
/// Sleeping a fixed time before the kill asserts a race: under load the kill can land before
/// the call is even dispatched, and then `MutationCertainty::None` is the *correct* answer
/// because the participant never received anything. The certainty the death rows are about --
/// `unknown`, because the participant died with work in its hands -- only holds if the work
/// reached it, so the test waits for the worker's own status to say so rather than guessing
/// from the clock.
async fn kill_once_it_is_working(
    launcher: &mut fly_session::Launcher,
    worker: &Id,
    inside: impl Fn(&StatusResult) -> bool,
) -> ReapOutcome {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(status) = launcher.health_check(worker).await
            && inside(&status)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{worker} never reported itself inside the operation"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    launcher.kill(worker).await
}

/// One agent dies in the middle of its Prepare. The epoch fails with a typed cause naming
/// that agent, within the caller's own budget, and nothing continues on the remainder.
async fn a_worker_death_has_a_bounded_diagnosed_outcome(mode: ExecutionMode) {
    let mut config = two_agents(mode);
    config.agents[1].faults = AgentFaults { prepare_delay_ms: 5_000, ..AgentFaults::default() };
    let mut f = mode_fixture(mode, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();

    let victim = fly_b();
    let (coordinator, launcher) = f.harness.parts();
    let (stepped, reaped) = tokio::join!(
        async { within("step", coordinator.step()).await },
        // Killed once it has the Prepare in its hands, not after a fixed sleep: the row is
        // about a participant that dies *with work*, so the work has to have reached it.
        kill_once_it_is_working(launcher, &victim, |status| {
            status.state == WorkerState::Preparing && status.active_request_id.is_some()
        })
    );
    assert_eq!(reaped, ReapOutcome::Terminated);
    // Boundedness is the suite's own `within` above: the participant is five seconds slow and
    // `within` gives up at twenty, so returning at all is the claim.
    let failure = stepped.expect_err("a dead participant is a failed epoch, not a slow one");
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

    let (coordinator, launcher) = f.harness.parts();
    let (stepped, reaped) = tokio::join!(
        async { within("step", coordinator.step()).await },
        // Killed once the world has recorded the batch, which the arena does before its
        // injected delay. So the Advance provably reached it and the certainty is `unknown`
        // rather than `none`; a fixed sleep could land before dispatch under load, and then
        // `none` would be right and this row would be asserting a race.
        kill_once_it_is_working(launcher, &environment, |status| {
            status.last_batch_id.is_some()
        })
    );
    assert_eq!(reaped, ReapOutcome::Terminated);
    let failure = stepped.expect_err("a dead world is a failed epoch");
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
    // Every participant reports the allocation its launcher gave it, which is the wire the
    // 2026-09-22 `workers-v1` amendment added. The launcher refused anything else at start,
    // so a caller reads it here rather than being told it out of band.
    for who in [fly_a(), fly_b(), environment.clone()] {
        let worker = f.harness.coordinator.agent_ref(&who).cloned().unwrap_or_else(|| {
            f.harness.coordinator.environment_ref().clone()
        });
        let params = serde_json::json!({
            "sessionId": "demo",
            "expectedWorkerId": who.as_str(),
            "role": if who == environment { "environment" } else { "agent" },
            "supportedMajors": [1],
        });
        let result = within(
            "hello",
            f.harness.coordinator.probe_raw(&worker, "Worker.Hello", None, params),
        )
        .await
        .expect("a worker answers its own identity");
        let reported = result["limits"]["workerThreads"].as_u64();
        assert_eq!(
            reported,
            Some(f.harness.launcher.worker(&who).unwrap().identity.worker_threads as u64),
            "{who} must report the allocation its launcher gave it"
        );
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
    let outcome = f.harness.launcher.reap(&fly_a(), &id("test")).await;
    assert_eq!(outcome, ReapOutcome::Stopped, "a live participant answers Worker.Shutdown");
    assert_eq!(
        f.harness.launcher.reap(&fly_a(), &id("test")).await,
        ReapOutcome::AlreadyGone
    );
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
    assert!(
        f.harness.coordinator.live_view_handles() > 0,
        "boundary 0's view is owned before the router goes away"
    );
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
    assert_eq!(
        f.harness.coordinator.live_view_handles(),
        0,
        "the fence drops every artifact handle of the old store incarnation"
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

/// The other half of the same row, in two parts, because the two refusals are different
/// refusals and each deserves its own exact code.
///
/// A restarted worker is a *fresh* process: it has no epoch at all, so the old epoch's work is
/// refused on phase, not on timeline. The stale-epoch half of the row needs a worker that has
/// an epoch and has left it, which in process mode is the agent that did not restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_worker_refuses_an_operation_from_the_old_epoch() {
    let mode = ExecutionMode::Process;
    let mut f = mode_fixture(mode, two_agents(mode)).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("step", f.harness.coordinator.step()).await.unwrap();

    // Part one: a live agent process, initialized under epoch e1, meets an operation from
    // another epoch. This is the row's stale-epoch half, with a real child process.
    let live = f.harness.coordinator.agent_ref(&fly_a()).cloned().unwrap();
    let err = within(
        "stale epoch",
        f.harness.coordinator.probe_raw(
            &live,
            "Agent.Prepare",
            Some(scope_at("demo", "e0", 1)),
            prepare_params("fly-a"),
        ),
    )
    .await
    .expect_err("an old epoch cannot mutate a worker that belongs to this one");
    assert_eq!(err.code, ErrorCode::StaleEpoch);
    assert_eq!(err.mutation, MutationCertainty::None, "refused before any mutation");

    // Part two: the replacement process. It is a fresh worker with no epoch at all, so the
    // same request is refused on phase rather than on timeline -- and, either way, nothing
    // from the old epoch is applied to a fresh brain.
    let restarted = f.harness.restart_agent(&fly_b()).await.unwrap();
    let replacement = fly_session::rpc::WorkerRef::new(
        &restarted.service,
        &restarted.service_incarnation,
        &fly_b(),
    );
    let err = within(
        "uninitialized replacement",
        f.harness.coordinator.probe_raw(
            &replacement,
            "Agent.Prepare",
            Some(scope_at("demo", "e1", 1)),
            prepare_params("fly-b"),
        ),
    )
    .await
    .expect_err("an uninitialized replacement has no epoch to prepare in");
    assert_eq!(
        err.code,
        ErrorCode::InvalidPhase,
        "a fresh process has no epoch to be stale about: {err}"
    );
    assert_eq!(err.mutation, MutationCertainty::None, "nothing was applied to a fresh brain");
    assert_eq!(f.harness.coordinator.stats().advances, 1);
    f.shutdown().await;
}

/// A well-formed `Agent.Prepare` body, for a probe whose subject is the scope rather than the
/// payload.
fn prepare_params(agent_id: &str) -> serde_json::Value {
    serde_json::json!({
        "agentId": agent_id,
        "profileDigest": digest_of_bytes(b"whatever"),
        "interval": {"numerator": "16666667", "denominator": "1"},
        "decisionContextDigest": digest_of_bytes(b"whatever"),
        "preStepStimulations": [],
    })
}
