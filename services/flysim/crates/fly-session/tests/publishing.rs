//! PUBLISH-01 acceptance: committed snapshots, observer isolation and the repair path.
//!
//! Every test here is one of the slice's acceptance bullets, or one sentence of
//! `publishing-v1` the session now enforces. The boundary itself is `fly_session::publish`:
//! the same bus, named delivery policies, named outcomes, and a fake multi-agent consumer that
//! is an ordinary subscriber. No public v2 wire schema is exercised, because none is approved.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{Fixture, default_fixture, fixture, fly_a, fly_b, mode_fixture, within};
use fly_session::coordinator::{DESCRIPTOR_REVISION, Injections};
use fly_session::harness::{AgentSpec, ExecutionMode, HarnessConfig, Via};
use fly_session::publish::{
    ApplicationChannel, ConsumerOutcome, Delivery, EVENT_BATCH_DEPTH, EventOutbox, GET_SNAPSHOT,
    PresentationConsumer, PublicationOutcome, TopicPolicy, check_publication, query_service,
};
use fly_session::types::*;
use serde_json::json;

both_transports!(
    a_consumer_that_disconnects_neither_advances_nor_stalls_the_world,
    a_consumer_that_stops_consuming_costs_only_its_own_credits,
    a_bounded_observer_refusal_is_named_and_never_fails_the_epoch,
    every_published_snapshot_carries_its_own_boundarys_media,
    a_published_frame_from_another_boundary_is_refused,
    a_published_handle_that_is_not_the_referenced_artifact_is_refused,
    an_unheld_descriptor_revision_is_repaired_rather_than_inferred,
    a_revision_that_was_never_published_is_a_named_answer,
    a_changed_index_digest_is_named_rather_than_remapped,
    boundary_zero_publishes_no_decision_and_no_controls,
    a_committed_action_is_the_transition_that_just_ended,
    one_snapshot_carries_every_agent_in_the_composition,
    application_state_and_cues_are_the_applications_own,
    a_refused_event_batch_is_held_and_counted_not_lost,
    the_query_service_answers_reads_and_nothing_else,
    a_refused_snapshot_is_still_what_the_repair_path_answers,
    the_published_descriptor_is_what_the_workers_attested_to,
    a_stimulus_kind_the_descriptor_does_not_declare_is_refused,
    a_restored_boundary_publishes_a_new_revision_and_no_transition,
);

all_modes!(
    the_publication_boundary_holds_in_every_execution_mode,
    a_stalled_observer_never_moves_the_world_in_any_execution_mode,
);

const STEPS: u64 = 4;

/// Polls until `ok` holds, so a test never asserts something that has not happened yet.
async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: never happened"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A started session at Ready(0), with boundary 0 already published.
async fn started(via: Via) -> Fixture {
    let mut f = default_fixture(via).await;
    f.harness
        .coordinator
        .bootstrap()
        .await
        .expect("the session bootstraps");
    f
}

async fn started_in_mode(mode: ExecutionMode) -> Fixture {
    let mut f = mode_fixture(mode, HarnessConfig::default()).await;
    f.harness
        .coordinator
        .bootstrap()
        .await
        .expect("the session bootstraps");
    f
}

/// Reads snapshots until the consumer reports the boundary it is waiting for.
async fn read_until(consumer: &mut PresentationConsumer, boundary: u64) -> u64 {
    loop {
        match within("a snapshot", consumer.take_snapshot()).await {
            Some(ConsumerOutcome::Read { boundary: at, .. }) if at >= boundary => return at,
            Some(ConsumerOutcome::Read { .. }) => {}
            other => panic!("expected a readable snapshot, got {other:?}"),
        }
    }
}

// ------------------------------------------------------------------------------------------
// "A browser consumer disconnecting, or applying backpressure, never advances the world and
// never stalls it."

/// A consumer that goes away mid-run. The world takes exactly the steps it was asked for.
async fn a_consumer_that_disconnects_neither_advances_nor_stalls_the_world(via: Via) {
    let mut f = started(via).await;
    let mut consumer = f.harness.consumer().await.expect("a consumer attaches");
    assert!(matches!(
        within("the descriptor", consumer.take_descriptor()).await,
        Some(ConsumerOutcome::Composition { revision, .. }) if revision == DESCRIPTOR_REVISION
    ));
    f.harness.coordinator.run(1).await.expect("one transition");
    read_until(&mut consumer, 1).await;

    // The browser goes away. Nothing in the session is told, and nothing waits for it.
    drop(consumer);

    let started_at = std::time::Instant::now();
    let before = f.harness.coordinator.stats();
    f.harness
        .coordinator
        .run(STEPS)
        .await
        .expect("the world carries on");
    let after = f.harness.coordinator.stats();
    assert_eq!(
        after.advances - before.advances,
        STEPS,
        "a departed observer neither added a world step nor removed one"
    );
    assert_eq!(after.publications - before.publications, STEPS);
    assert!(
        !f.harness.coordinator.is_fenced(),
        "a departed observer never fences an epoch"
    );
    assert!(
        started_at.elapsed() < Duration::from_secs(10),
        "a departed observer never stalls the world"
    );
    assert_eq!(f.harness.coordinator.ledger().refusals(), 0);
    f.shutdown().await;
}

/// A viewer that holds its deliveries and stops rendering. Its own credits run out; the
/// world's boundaries do not.
async fn a_consumer_that_stops_consuming_costs_only_its_own_credits(via: Via) {
    let mut f = started(via).await;
    let mut slow = f
        .harness
        .consumer()
        .await
        .expect("a slow consumer attaches");
    let mut healthy = f
        .harness
        .consumer()
        .await
        .expect("a healthy consumer attaches");

    // The slow one takes its deliveries and never releases them: a viewer holding output.
    until("the slow viewer has a delivery", || {
        slow.try_hold_snapshot()
    })
    .await;
    while slow.try_hold_snapshot() && slow.held() < 4 {}
    let held = slow.held();
    assert!(held > 0);

    f.harness
        .coordinator
        .run(STEPS)
        .await
        .expect("the world carries on");
    assert_eq!(f.harness.coordinator.stats().advances, STEPS);
    assert_eq!(
        f.harness.coordinator.ledger().refusals(),
        0,
        "a latest subscriber cannot refuse"
    );
    assert_eq!(
        slow.held(),
        held,
        "the slow viewer took nothing more once its credits were gone"
    );

    // The healthy one is unaffected and reads the newest boundary.
    assert!(matches!(
        within("the descriptor", healthy.take_descriptor()).await,
        Some(ConsumerOutcome::Composition { .. })
    ));
    let at = read_until(&mut healthy, STEPS).await;
    assert_eq!(at, STEPS);
    assert_eq!(
        healthy.last().expect("a snapshot").boundary,
        STEPS,
        "the healthy consumer reads the newest boundary while the slow one holds"
    );
    f.shutdown().await;
}

/// A bounded subscriber whose queue is full refuses a publication -- `bus-v1` section 6 says
/// it may. The refusal is a named outcome on a named topic, the world takes every step it was
/// asked for, the epoch is not fenced, and when the offender goes away the next boundary
/// reaches the well-behaved consumer.
async fn a_bounded_observer_refusal_is_named_and_never_fails_the_epoch(via: Via) {
    let mut f = started(via).await;
    let healthy_first = f
        .harness
        .consumer()
        .await
        .expect("a healthy consumer attaches");
    let offender_client = f.harness.observer().await.expect("an observer client");
    let snapshots = f.harness.coordinator.topics().snapshots.clone();
    let mut offender = offender_client
        .subscribe(
            &snapshots,
            flybus::SubscriptionConfig::bounded().queued(1).in_flight(1),
        )
        .await
        .expect("a bounded subscription");

    let before = f.harness.coordinator.stats().advances;
    f.harness
        .coordinator
        .run(STEPS)
        .await
        .expect("an observer never fails the epoch");
    assert_eq!(
        f.harness.coordinator.stats().advances - before,
        STEPS,
        "a refused publication never costs the world a step"
    );
    assert!(!f.harness.coordinator.is_fenced());
    let counters = f.harness.coordinator.ledger().counters(&snapshots);
    assert!(
        counters.refused > 0,
        "the refusal is counted on its own topic: {counters:?}"
    );
    assert!(
        matches!(
            f.harness.coordinator.ledger().last(),
            Some(PublicationOutcome::RefusedByObserver { .. })
        ),
        "the last outcome names the refusal rather than defaulting to success"
    );
    assert_eq!(
        counters.faulted, 0,
        "an observer's refusal is never the session's fault"
    );

    // The offender leaves. The next boundary is published and the healthy consumer reads it.
    drop(offender.try_next());
    drop(offender);
    offender_client.close().await;
    drop(healthy_first);
    let mut healthy = f
        .harness
        .consumer()
        .await
        .expect("a healthy consumer attaches");
    f.harness
        .coordinator
        .run(2)
        .await
        .expect("the world carries on");
    assert!(matches!(
        within("the descriptor", healthy.take_descriptor()).await,
        Some(ConsumerOutcome::Composition { .. })
    ));
    let at = read_until(&mut healthy, STEPS + 1).await;
    assert!(
        at > STEPS,
        "a boundary published after the offender left reaches a consumer"
    );
    f.shutdown().await;
}

// ------------------------------------------------------------------------------------------
// "Future agent state is never mixed with old media."

/// Every snapshot's media belongs to the boundary its agent state belongs to: the frame is the
/// one this boundary's declared delay requires, the handle is the artifact the payload names,
/// the audio continues where the last chunk ended, and the agents are committed at it.
async fn every_published_snapshot_carries_its_own_boundarys_media(via: Via) {
    let mut f = started(via).await;
    let mut consumer = f.harness.consumer().await.expect("a consumer attaches");
    assert!(matches!(
        within("the descriptor", consumer.take_descriptor()).await,
        Some(ConsumerOutcome::Composition { .. })
    ));
    let mut seen: Vec<(u64, String)> = Vec::new();
    let mut ticks: BTreeMap<Id, u64> = BTreeMap::new();
    for step in 1..=STEPS {
        f.harness.coordinator.run(1).await.expect("a transition");
        let at = read_until(&mut consumer, step).await;
        let view = consumer.last().expect("a snapshot");
        assert_eq!(view.boundary, at);
        for reference in &view.views {
            // The counter arena declares no render delay, so the frame of boundary k is
            // produced at k. The consumer checked this itself before it got here.
            assert_eq!(reference.produced_step, at, "the frame is this boundary's");
            let handle = view
                .artifacts
                .get(&fly_session::media::view_attachment(&reference.view_id))
                .expect("the referenced frame has its handle");
            assert_eq!(
                handle.reference(),
                &reference.pixels,
                "the handle is that artifact"
            );
            seen.push((at, reference.pixels.artifact_id.clone()));
        }
        for agent in &view.agents {
            let previous = ticks.insert(agent.agent_id.clone(), agent.brain_ticks);
            match previous {
                None => assert!(agent.brain_ticks > 0, "the agent has run by boundary {at}"),
                // Telemetry is the state of the transition that ended here. Republishing the
                // previous boundary's numbers -- or `Agent.Initialize`'s warm-up numbers --
                // would be old agent state labelled as this boundary, which is the same
                // mislabelling as old media.
                Some(before) => assert!(
                    agent.brain_ticks > before,
                    "the telemetry of {} advanced into boundary {at}: {before} -> {}",
                    agent.agent_id,
                    agent.brain_ticks
                ),
            }
        }
    }
    let mut ids: Vec<&String> = seen.iter().map(|(_, id)| id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        seen.len(),
        "no boundary republished another boundary's frame: {seen:?}"
    );
    f.shutdown().await;
}

/// The injection of new agent state with the previous boundary's frame. The publication is
/// refused before it reaches a subscriber, and the consumer never sees a mixed boundary.
async fn a_published_frame_from_another_boundary_is_refused(via: Via) {
    let mut f = started(via).await;
    let mut consumer = f.harness.consumer().await.expect("a consumer attaches");
    f.harness
        .coordinator
        .run(1)
        .await
        .expect("one clean transition");
    f.harness.coordinator.injections = Injections {
        at_step: 1,
        stale_published_view: true,
        ..Injections::default()
    };
    let failure = f
        .harness
        .coordinator
        .run(1)
        .await
        .expect_err("a snapshot that mixes boundaries is refused");
    assert_eq!(failure.error.code, ErrorCode::BufferInvalid, "{failure:?}");
    assert!(
        failure.error.message.contains("produced at"),
        "the refusal names the boundary the frame came from: {}",
        failure.error.message
    );
    assert_eq!(
        failure.error.mutation,
        MutationCertainty::None,
        "nothing was published, so nothing downstream saw it"
    );

    // Whatever the consumer read, none of it is the mixed boundary.
    assert!(matches!(
        within("the descriptor", consumer.take_descriptor()).await,
        Some(ConsumerOutcome::Composition { .. })
    ));
    until("a snapshot of the clean boundary", || {
        matches!(
            consumer.try_take_snapshot(),
            Some(ConsumerOutcome::Read { boundary: 1, .. })
        )
    })
    .await;
    assert_eq!(consumer.last().expect("a snapshot").boundary, 1);
    while let Some(outcome) = consumer.try_take_snapshot() {
        match outcome {
            ConsumerOutcome::Read { boundary, .. } => {
                assert!(boundary <= 1, "the mixed boundary was never published");
            }
            other => panic!("the consumer saw {other:?}"),
        }
    }
    f.shutdown().await;
}

/// The same attachment names and the same bytes, another object. Only the artifact identity
/// sees it, and the publication is refused.
async fn a_published_handle_that_is_not_the_referenced_artifact_is_refused(via: Via) {
    let mut f = started(via).await;
    f.harness.coordinator.injections = Injections {
        at_step: 0,
        substituted_published_handle: true,
        ..Injections::default()
    };
    let failure = f
        .harness
        .coordinator
        .run(1)
        .await
        .expect_err("a handle that is not the referenced artifact is refused");
    assert_eq!(failure.error.code, ErrorCode::BufferInvalid, "{failure:?}");
    assert!(
        failure.error.message.contains("generation"),
        "the refusal names the object it got: {}",
        failure.error.message
    );
    f.shutdown().await;
}

// ------------------------------------------------------------------------------------------
// "A descriptor or index mismatch is visible rather than silently tolerated."

/// A consumer that meets a revision it does not hold repairs through the query service. It
/// infers nothing from the snapshot in the meantime.
async fn an_unheld_descriptor_revision_is_repaired_rather_than_inferred(via: Via) {
    let mut f = started(via).await;
    let mut consumer = f.harness.consumer().await.expect("a consumer attaches");
    f.harness.coordinator.run(1).await.expect("a transition");

    // The snapshot arrives first: cross-topic ordering is not guaranteed, and this consumer
    // has deliberately not read its descriptor topic.
    let outcome = within("a snapshot", consumer.take_snapshot()).await;
    assert_eq!(
        outcome,
        Some(ConsumerOutcome::UnknownRevision {
            revision: DESCRIPTOR_REVISION
        }),
        "an unknown revision is reported, not guessed"
    );
    assert!(
        consumer.last().is_none(),
        "nothing was read out of a snapshot it cannot shape"
    );

    let repaired = consumer
        .repair(DESCRIPTOR_REVISION)
        .await
        .expect("the repair path answers");
    assert_eq!(repaired, DESCRIPTOR_REVISION);
    assert_eq!(consumer.repairs(), 1);
    f.harness.coordinator.run(1).await.expect("a transition");
    let at = read_until(&mut consumer, 1).await;
    assert!(at >= 1, "with the descriptor in hand the same stream reads");
    f.shutdown().await;
}

/// A revision this session never published is an answer, not an empty result.
async fn a_revision_that_was_never_published_is_a_named_answer(via: Via) {
    let f = started(via).await;
    let mut consumer = f.harness.consumer().await.expect("a consumer attaches");
    let error = consumer
        .repair(99)
        .await
        .expect_err("revision 99 was never published");
    assert_eq!(error.code, ErrorCode::IdentityMismatch, "{error:?}");
    assert!(error.message.contains("99"), "{}", error.message);
    assert_eq!(
        error.mutation,
        MutationCertainty::None,
        "a read never mutates"
    );
    f.shutdown().await;
}

/// The same neuron count, another index. A consumer that has mapped geometry is told, and the
/// new composition is not quietly cached over the one it mapped.
///
/// The two descriptors here come from two real sessions rather than from a restore. Driving it
/// through a restore was tried and does not work yet, for a reason outside this slice: the
/// coordinator's `AgentSlot.graph` is written only by `Agent.Initialize`, and a group restore
/// installs state through `State.ActivateRestore`, so a replacement fly that built another
/// index is published under its predecessor's `indexDigest` -- and it is not refused on the way
/// in either, because `agent_compatibility` digests `agent::dataset_digest()` rather than the
/// index the worker attested to. Both halves belong to the restore contract, so this test uses
/// the compositions it can build honestly and the gap is reported rather than papered over.
async fn a_changed_index_digest_is_named_rather_than_remapped(via: Via) {
    // Two real compositions that differ only in the graph one fly built.
    let first = started(via).await;
    let mut second = fixture(
        via,
        HarnessConfig {
            agents: vec![
                AgentSpec {
                    graph_variant: 1,
                    ..AgentSpec::new("fly-a", "p1", 7)
                },
                AgentSpec {
                    graph_variant: 0,
                    ..AgentSpec::new("fly-b", "p2", 11)
                },
            ],
            ..HarnessConfig::default()
        },
    )
    .await;
    second
        .harness
        .coordinator
        .bootstrap()
        .await
        .expect("the second session bootstraps");
    let revised = {
        let mut revised = second
            .harness
            .coordinator
            .session_descriptor()
            .expect("a published descriptor")
            .clone();
        revised.revision = DESCRIPTOR_REVISION + 1;
        revised
    };
    let original = first
        .harness
        .coordinator
        .session_descriptor()
        .expect("a published descriptor")
        .clone();
    let moved = original
        .agents
        .iter()
        .find(|a| a.agent_id == fly_a())
        .expect("fly-a");
    let arrived = revised
        .agents
        .iter()
        .find(|a| a.agent_id == fly_a())
        .expect("fly-a");
    assert_eq!(
        moved.neuron_count, arrived.neuron_count,
        "the same number of neurons"
    );
    assert_ne!(moved.index_digest, arrived.index_digest, "and another index");

    let mut consumer = first.harness.consumer().await.expect("a consumer attaches");
    let held = match within("the descriptor", consumer.take_descriptor()).await {
        Some(ConsumerOutcome::Composition { revision, .. }) => revision,
        other => panic!("expected a composition, got {other:?}"),
    };
    consumer.map_geometry(held).expect("this consumer maps geometry");
    assert_eq!(consumer.mapped_index(&fly_a()), Some(&moved.index_digest));

    // The composition changes under it.
    let publisher_client = first
        .harness
        .publisher()
        .await
        .expect("a publishing client");
    let mut publisher = fly_session::publish::Publisher::new(
        publisher_client,
        &first.harness.config.session_id,
        &first.harness.config.epoch,
        first.harness.coordinator.topics(),
    );
    publisher
        .publish_descriptor(&revised)
        .await
        .expect("the revision publishes");
    let outcome = within("the revised descriptor", consumer.take_descriptor()).await;
    assert_eq!(
        outcome,
        Some(ConsumerOutcome::IndexChanged {
            agent_id: fly_a(),
            from: moved.index_digest.clone(),
            to: arrived.index_digest.clone(),
        }),
        "the change is named"
    );
    assert_eq!(
        consumer.mapped_index(&fly_a()),
        Some(&moved.index_digest),
        "and nothing was remapped behind the consumer's back"
    );
    assert!(
        consumer.descriptor(DESCRIPTOR_REVISION + 1).is_none(),
        "the revision it refused is not in its cache either"
    );
    second.shutdown().await;
    first.shutdown().await;
}

/// The descriptor says what the workers said, not what the composition asked for.
async fn the_published_descriptor_is_what_the_workers_attested_to(via: Via) {
    let f = started(via).await;
    let descriptor = f
        .harness
        .coordinator
        .session_descriptor()
        .expect("a descriptor")
        .clone();
    assert_eq!(descriptor.revision, DESCRIPTOR_REVISION);
    assert_eq!(descriptor.agents.len(), 2);
    for agent in &descriptor.agents {
        let expected = fly_session::agent::synthetic_graph(&agent.agent_id, 0);
        assert_eq!(
            agent.index_digest, expected.index_digest,
            "the fly's own index"
        );
        assert_eq!(agent.dataset_digest, expected.dataset_digest);
        assert_eq!(agent.neuron_count, expected.neuron_count);
        assert_eq!(
            agent.rate_roles, expected.rate_roles,
            "the profile's rate order"
        );
        assert_eq!(agent.supported_stimuli, expected.supported_stimuli);
        assert!(
            descriptor.environment.port(&agent.port_id).is_some(),
            "every agent's port is declared by the environment"
        );
    }
    let mut asset_ids: Vec<&Id> = descriptor.assets.iter().map(|a| &a.id).collect();
    asset_ids.sort();
    asset_ids.dedup();
    assert_eq!(
        asset_ids.len(),
        descriptor.assets.len(),
        "assets are unique by id"
    );
    f.shutdown().await;
}

/// `supportedStimuli` is enforced, not advertised: a kind outside the list the descriptor
/// publishes is refused before the model is touched, so the declaration is worth reading.
async fn a_stimulus_kind_the_descriptor_does_not_declare_is_refused(via: Via) {
    let mut f = started(via).await;
    let declared = f
        .harness
        .coordinator
        .session_descriptor()
        .expect("a descriptor")
        .agents
        .iter()
        .find(|a| a.agent_id == fly_a())
        .expect("fly-a")
        .supported_stimuli
        .clone();
    assert!(!declared.contains(&id("arena.undeclared")), "{declared:?}");

    // A clean transition first, so the refusal is the injection and not the composition.
    f.harness.coordinator.run(1).await.expect("one clean transition");
    let before = f.harness.coordinator.stats().advances;
    f.harness.coordinator.injections = Injections {
        at_step: 1,
        undeclared_stimulus: true,
        ..Injections::default()
    };
    let failure = f
        .harness
        .coordinator
        .run(1)
        .await
        .expect_err("an undeclared stimulus kind is refused");
    assert_eq!(failure.error.code, ErrorCode::Unsupported, "{failure:?}");
    assert_eq!(
        failure.error.mutation,
        MutationCertainty::None,
        "refused before the model is touched"
    );
    assert!(
        failure.error.message.contains("arena.undeclared"),
        "the refusal names the kind: {}",
        failure.error.message
    );
    assert!(failure.participant.is_some(), "and the participant it came from");
    assert_eq!(
        f.harness.coordinator.stats().advances,
        before,
        "the refused transition never completed, so no boundary was added"
    );
    assert!(
        f.harness.coordinator.is_fenced(),
        "a commit that refused after the world moved fences the epoch"
    );
    f.shutdown().await;
}

// ------------------------------------------------------------------------------------------
// "A committed action is labelled as the transition that just ended, not the one about to
// start."

async fn boundary_zero_publishes_no_decision_and_no_controls(via: Via) {
    let f = started(via).await;
    let state = f.harness.coordinator.published_state();
    let snapshot = {
        let state = state.lock().expect("not poisoned");
        state
            .latest_snapshot()
            .expect("boundary 0 was published")
            .clone()
    };
    assert_eq!(snapshot.scope.step, 0);
    for agent in &snapshot.agents {
        assert!(
            agent.selected_decision.is_none(),
            "boundary 0 ended no transition"
        );
        assert!(agent.applied_controls.is_none());
    }
    f.shutdown().await;
}

/// The decision published at boundary k+1 is the one the agent prepared for the transition
/// k -> k+1, never the one it is about to prepare for k+1 -> k+2.
async fn a_committed_action_is_the_transition_that_just_ended(via: Via) {
    let mut f = started(via).await;
    let mut consumer = f.harness.consumer().await.expect("a consumer attaches");
    assert!(matches!(
        within("the descriptor", consumer.take_descriptor()).await,
        Some(ConsumerOutcome::Composition { .. })
    ));
    let mut published: Vec<(u64, Digest)> = Vec::new();
    for step in 1..=STEPS {
        f.harness.coordinator.run(1).await.expect("a transition");
        read_until(&mut consumer, step).await;
        let view = consumer.last().expect("a snapshot");
        let agent = view.agent(&fly_a()).expect("fly-a is in every snapshot");
        let decision = agent
            .decision
            .as_ref()
            .expect("past boundary 0 there is a decision");
        published.push((view.boundary, typed_digest(decision)));
        assert!(
            agent.controls.is_some(),
            "and the controls that were applied"
        );
    }
    // The trace records, per transition, the decision digest the coordinator actually sent to
    // the world. The snapshot at boundary k+1 must carry the transition ending there.
    let trace = &f.harness.coordinator.trace.transitions;
    assert_eq!(trace.len() as u64, STEPS);
    for (index, (boundary, digest)) in published.iter().enumerate() {
        let transition = &trace[index];
        assert_eq!(
            transition.behaviour.published_boundary, *boundary,
            "the snapshot at {boundary} is the transition that ended there"
        );
        let traced = transition
            .behaviour
            .agents
            .iter()
            .find(|a| a.agent_id == fly_a())
            .expect("fly-a in the trace");
        assert_eq!(
            traced.decision_digest, *digest,
            "the decision published at boundary {boundary} is the one that produced it"
        );
        assert_eq!(traced.committed_step, *boundary);
    }
    // And it is not the next transition's, which the following trace row holds.
    if STEPS > 1 {
        let first = &published[0];
        let next = trace[1]
            .behaviour
            .agents
            .iter()
            .find(|a| a.agent_id == fly_a())
            .expect("fly-a in the trace");
        assert_ne!(
            first.1, next.decision_digest,
            "boundary 1 did not publish the decision of the transition about to start"
        );
    }
    f.shutdown().await;
}

/// One snapshot is the whole composition: a presentation consumer is a multi-agent consumer
/// and has no per-fly stream to join.
async fn one_snapshot_carries_every_agent_in_the_composition(via: Via) {
    let mut f = started(via).await;
    let mut consumer = f.harness.consumer().await.expect("a consumer attaches");
    assert!(matches!(
        within("the descriptor", consumer.take_descriptor()).await,
        Some(ConsumerOutcome::Composition { .. })
    ));
    f.harness.coordinator.run(1).await.expect("a transition");
    let outcome = match within("a snapshot", consumer.take_snapshot()).await {
        Some(outcome) => outcome,
        None => panic!("the snapshot stream ended"),
    };
    let agents = match outcome {
        ConsumerOutcome::Read { agents, .. } => agents,
        other => panic!("expected a readable snapshot, got {other:?}"),
    };
    assert_eq!(agents, vec![fly_a(), fly_b()], "every agent, in one value");
    let view = consumer.last().expect("a snapshot");
    for agent_id in [fly_a(), fly_b()] {
        let agent = view.agent(&agent_id).expect("in the snapshot");
        assert_eq!(
            agent.rates.len(),
            2,
            "telemetry in the descriptor's rate-role order"
        );
        assert_eq!(agent.rates[0].0, id("kc"));
        assert_eq!(agent.rates[1].0, id("mbon"));
        assert!(!agent.index_digest.is_empty());
    }
    f.shutdown().await;
}

/// A group restore re-establishes a committed boundary this epoch did not run a transition
/// into. Two things follow, and both are published rather than inferred: the composition is a
/// new one, because a fresh epoch is a new `compositionDigest`, so the descriptor takes the
/// next revision; and the restored boundary carries no decision and no controls for any agent,
/// because the abandoned epoch's actions are not this session's to republish.
async fn a_restored_boundary_publishes_a_new_revision_and_no_transition(via: Via) {
    let mut f = started(via).await;
    let checkpoint = id("ck-1");
    f.harness.coordinator.run(1).await.expect("one transition");
    let outcome = within("checkpoint", f.harness.coordinator.checkpoint(&checkpoint))
        .await
        .expect("a committed checkpoint");
    assert!(matches!(outcome, fly_session::state::SaveOutcome::Committed { .. }), "{outcome:?}");
    let first = f.harness.coordinator.descriptor_revision();

    // The snapshot of a boundary this epoch produced does carry the transition.
    let produced = {
        let state = f.harness.coordinator.published_state();
        let state = state.lock().expect("not poisoned");
        state.latest_snapshot().expect("boundary 1").clone()
    };
    assert_eq!(produced.scope.step, 1);
    assert!(produced.agents.iter().all(|a| a.selected_decision.is_some()));

    // Fail the epoch and restore into a fresh one.
    f.harness.kill(&fly_a()).await;
    f.harness.coordinator.step().await.expect_err("a dead participant fails the epoch");
    within("replace", f.harness.replace_all_participants())
        .await
        .expect("replacements");
    within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint), &id("e2")),
    )
    .await
    .expect("a coherent group restore");

    assert_eq!(
        f.harness.coordinator.descriptor_revision(),
        first + 1,
        "a fresh epoch is a new composition, so the revision advanced"
    );
    let restored = {
        let state = f.harness.coordinator.published_state();
        let state = state.lock().expect("not poisoned");
        state.latest_snapshot().expect("the restored boundary").clone()
    };
    assert_eq!(restored.scope.step, 1, "the same committed boundary");
    assert_eq!(restored.descriptor_revision, first + 1);
    assert!(
        restored.agents.iter().all(|a| a.selected_decision.is_none() && a.applied_controls.is_none()),
        "an installed boundary carries no transition for any agent"
    );
    f.shutdown().await;
}

// ------------------------------------------------------------------------------------------
// Application-owned state and cues, bounded events, and the read-only repair service.

/// The application publishes its own state and cues, on its own addresses, under its own
/// schema. Nothing about them is framework-shaped.
async fn application_state_and_cues_are_the_applications_own(via: Via) {
    let mut f = started(via).await;
    let client = f
        .harness
        .application()
        .await
        .expect("an application client");
    let mut channel = ApplicationChannel::new(client, "app.counter", 16);
    channel
        .declare()
        .await
        .expect("the application declares its own topics");
    assert_eq!(channel.state_topic(), "app.counter.state");
    assert_eq!(channel.cue_topic(), "app.counter.cues");

    let watcher = f.harness.observer().await.expect("an observer client");
    let mut states = watcher
        .subscribe(channel.state_topic(), Delivery::LatestValue.subscription())
        .await
        .expect("a latest subscription");
    let mut cues = watcher
        .subscribe(
            channel.cue_topic(),
            Delivery::BoundedBatch { depth: 16 }.subscription(),
        )
        .await
        .expect("a bounded subscription");

    f.harness.coordinator.run(1).await.expect("a transition");
    // The application's schema: its own namespace, not a framework field.
    let schema = SchemaRef {
        id: id("counter.show.v1"),
        version: 1,
        digest: digest_of_bytes(b"counter.show.v1"),
    };
    let state = typed(schema.clone(), json!({ "featured": "fly-a", "streak": 3 }))
        .expect("an application value");
    let outcome = channel.publish_state(1, &state).await;
    assert!(outcome.is_accepted(), "{outcome:?}");
    let cue = typed(schema, json!({ "line": "fly-a takes the lead" })).expect("a cue");
    assert!(
        channel
            .publish_cue(1, &id("counter.headline"), &cue)
            .await
            .is_accepted()
    );

    let message = within("the application state", states.next())
        .await
        .expect("a state message");
    assert_eq!(
        message.payload().get("boundary").and_then(|v| v.as_str()),
        Some("1")
    );
    let published = TypedValue::from_json(message.payload().get("state").expect("state"))
        .expect("an application typed value");
    assert_eq!(published.schema.id, id("counter.show.v1"));
    drop(message);
    let message = within("the cue", cues.next()).await.expect("a cue message");
    assert_eq!(
        message.payload().get("kind").and_then(|v| v.as_str()),
        Some("counter.headline")
    );
    drop(message);

    // Nothing the application published is on a session topic, and nothing on a session topic
    // knows the application's schema.
    let descriptor = f
        .harness
        .coordinator
        .session_descriptor()
        .expect("a descriptor");
    assert_ne!(descriptor.task_schema.id, id("counter.show.v1"));
    f.shutdown().await;
}

/// A refused event batch is held, counted and delivered later. Nothing is dropped quietly,
/// and what the bounded depth does push out travels as a count.
async fn a_refused_event_batch_is_held_and_counted_not_lost(via: Via) {
    let mut f = started(via).await;
    let events_topic = f.harness.coordinator.topics().events.clone();
    let offender_client = f.harness.observer().await.expect("an observer client");
    let mut offender = offender_client
        .subscribe(
            &events_topic,
            flybus::SubscriptionConfig::bounded().queued(1).in_flight(1),
        )
        .await
        .expect("a bounded subscription");

    f.harness
        .coordinator
        .run(STEPS)
        .await
        .expect("an event observer never fails the epoch");
    assert_eq!(f.harness.coordinator.stats().advances, STEPS);
    let counters = f.harness.coordinator.ledger().counters(&events_topic);
    assert!(counters.refused > 0, "the refusal is counted: {counters:?}");
    assert!(f.harness.coordinator.ledger().events_held > 0);
    assert!(
        f.harness.coordinator.pending_events() > 0,
        "the refused events are held in a bounded batch, not dropped"
    );

    // The offender starts consuming; the held batch goes out at the next boundary.
    drop(within("an event delivery", offender.next()).await);
    drop(offender.try_next());
    let held = f.harness.coordinator.pending_events();
    f.harness.coordinator.run(1).await.expect("a transition");
    until("the held batch is published", || {
        f.harness.coordinator.pending_events() < held
    })
    .await;
    offender_client.close().await;
    f.shutdown().await;
}

/// A publication an observer refused is exactly the value the repair path exists to hand back.
///
/// The `state-media-v1` amendment promises the exact value stays recoverable through the query
/// path, so recording it cannot depend on whether the router admitted the delivery -- that is
/// the case the promise is about.
async fn a_refused_snapshot_is_still_what_the_repair_path_answers(via: Via) {
    let mut f = started(via).await;
    let snapshots = f.harness.coordinator.topics().snapshots.clone();
    let offender_client = f.harness.observer().await.expect("an observer client");
    let _offender = offender_client
        .subscribe(
            &snapshots,
            flybus::SubscriptionConfig::bounded().queued(1).in_flight(1),
        )
        .await
        .expect("a bounded subscription");

    f.harness.coordinator.run(STEPS).await.expect("the world carries on");
    let counters = f.harness.coordinator.ledger().counters(&snapshots);
    assert!(counters.refused > 0, "a refusal is what this test is about: {counters:?}");

    let latest = {
        let state = f.harness.coordinator.published_state();
        let state = state.lock().expect("not poisoned");
        state.latest_snapshot().expect("a snapshot").clone()
    };
    assert_eq!(
        latest.scope.step, STEPS,
        "the newest committed boundary is recoverable even though its delivery was refused"
    );
    assert_eq!(
        latest.sequence + 1,
        f.harness.coordinator.published_sequence(),
        "the sequence advanced with the value, not with the delivery"
    );

    // And the query service answers it over the bus, not just the state behind it.
    let mut consumer = f.harness.consumer().await.expect("a consumer attaches");
    let answered = within("the repair path", consumer.repair(DESCRIPTOR_REVISION))
        .await
        .expect("the repair path answers");
    assert_eq!(answered, DESCRIPTOR_REVISION);
    offender_client.close().await;
    f.shutdown().await;
}

/// Two reads and nothing else. There is no method on this service that could move anything.
async fn the_query_service_answers_reads_and_nothing_else(via: Via) {
    let mut f = started(via).await;
    f.harness.coordinator.run(1).await.expect("a transition");
    let client = f.harness.observer().await.expect("an observer client");
    let service = query_service(&f.harness.config.session_id);

    let request = SessionRpcRequest {
        request_id: DomainRequestId::from_serial(1),
        scope: None,
        params: json!({}),
    };
    let mut pending = client
        .call(&service, None, GET_SNAPSHOT, object(request.to_json()), &[])
        .await
        .expect("the service answers");
    let result = pending.result().await.expect("a reply");
    let outcome =
        SessionRpcOutcome::from_json(&serde_json::Value::Object(result.outcome().clone()))
            .expect("a domain outcome");
    drop(result);
    let snapshot = CommittedSnapshot::from_json(outcome_result(&outcome).expect("a result"))
        .expect("a committed snapshot");
    assert_eq!(snapshot.scope.step, 1);
    assert_eq!(
        snapshot.descriptor_revision,
        f.harness.coordinator.descriptor_revision(),
        "a read answers the composition the session is publishing under"
    );
    assert!(
        snapshot.agents.iter().all(|a| a.selected_decision.is_some()),
        "and the values of the transition that ended at it"
    );

    // A method it does not implement is a named refusal, not a default.
    let request = SessionRpcRequest {
        request_id: DomainRequestId::from_serial(2),
        scope: None,
        params: json!({}),
    };
    let mut pending = client
        .call(
            &service,
            None,
            "Session.Advance",
            object(request.to_json()),
            &[],
        )
        .await
        .expect("the service answers");
    let result = pending.result().await.expect("a reply");
    let outcome =
        SessionRpcOutcome::from_json(&serde_json::Value::Object(result.outcome().clone()))
            .expect("a domain outcome");
    drop(result);
    let error = outcome_result(&outcome).expect_err("no such method");
    assert_eq!(error.code, ErrorCode::Unsupported, "{error:?}");
    assert_eq!(
        f.harness.coordinator.stats().advances,
        1,
        "and nothing advanced"
    );
    client.close().await;
    f.shutdown().await;
}

// ------------------------------------------------------------------------------------------
// Every execution mode

/// The publication boundary is inside the coordinator, so it crosses no process boundary: it
/// must therefore behave identically in all three modes, and this asserts that rather than
/// assuming it.
async fn the_publication_boundary_holds_in_every_execution_mode(mode: ExecutionMode) {
    let mut f = started_in_mode(mode).await;
    let mut consumer = f.harness.consumer().await.expect("a consumer attaches");
    let revision = match within("the descriptor", consumer.take_descriptor()).await {
        Some(ConsumerOutcome::Composition { revision, agents }) => {
            assert_eq!(agents, vec![fly_a(), fly_b()]);
            revision
        }
        other => panic!("expected a composition, got {other:?}"),
    };
    assert_eq!(revision, DESCRIPTOR_REVISION);
    let descriptor = consumer.descriptor(revision).expect("held").clone();
    for agent in &descriptor.agents {
        // The graph identity is attested over the bus, so it crosses a process boundary like
        // every other reply and is observable in every mode.
        assert_eq!(
            agent.index_digest,
            fly_session::agent::synthetic_graph(&agent.agent_id, 0).index_digest
        );
    }
    f.harness
        .coordinator
        .run(STEPS)
        .await
        .expect("the world runs");
    let at = read_until(&mut consumer, STEPS).await;
    assert_eq!(at, STEPS);
    let view = consumer.last().expect("a snapshot");
    assert_eq!(view.agents.len(), 2);
    assert!(view.agent(&fly_a()).expect("fly-a").decision.is_some());
    assert_eq!(
        f.harness
            .coordinator
            .ledger()
            .counters(&f.harness.coordinator.topics().snapshots)
            .accepted,
        STEPS + 1
    );
    f.shutdown().await;
}

/// A stalled observer costs only itself, in every mode.
async fn a_stalled_observer_never_moves_the_world_in_any_execution_mode(mode: ExecutionMode) {
    let mut f = started_in_mode(mode).await;
    let mut slow = f
        .harness
        .consumer()
        .await
        .expect("a slow consumer attaches");
    until("the slow viewer has a delivery", || {
        slow.try_hold_snapshot()
    })
    .await;
    while slow.try_hold_snapshot() && slow.held() < 4 {}
    let before = f.harness.coordinator.stats();
    let started_at = std::time::Instant::now();
    f.harness
        .coordinator
        .run(STEPS)
        .await
        .expect("the world carries on");
    assert_eq!(
        f.harness.coordinator.stats().advances - before.advances,
        STEPS
    );
    assert!(!f.harness.coordinator.is_fenced());
    assert_eq!(f.harness.coordinator.ledger().refusals(), 0);
    assert!(started_at.elapsed() < Duration::from_secs(10));
    f.shutdown().await;
}

// ------------------------------------------------------------------------------------------
// The policy and the batch, without a session

#[test]
fn a_topic_policy_declares_its_delivery_once() {
    let latest = TopicPolicy::latest("session.demo.snapshots");
    assert_eq!(latest.delivery, Delivery::LatestValue);
    assert_eq!(latest.delivery.retained(), flybus::Retained::Latest);
    assert_eq!(latest.delivery.subscription().mode, flybus::Mode::Latest);
    let bounded = TopicPolicy::bounded("session.demo.events", 8);
    assert_eq!(bounded.delivery, Delivery::BoundedBatch { depth: 8 });
    // A bounded stream is never retained: it is "not a durable log", and a retained tail
    // would be the beginning of one.
    assert_eq!(bounded.delivery.retained(), flybus::Retained::None);
    assert_eq!(bounded.delivery.subscription().max_queued, 8);
}

#[test]
fn a_full_event_batch_drops_the_oldest_by_an_explicit_count() {
    let mut outbox = EventOutbox::new(EVENT_BATCH_DEPTH);
    let event = |n: u64| TaskEvent {
        id: id(&format!("ev-{n}")),
        kind_id: id("arena.counter-delta"),
        source_step: 1,
        agent_id: None,
        payload: typed(
            SchemaRef {
                id: id("t.v1"),
                version: 1,
                digest: digest_of_bytes(b"t.v1"),
            },
            json!({ "n": n }),
        )
        .expect("a typed value"),
    };
    for n in 0..EVENT_BATCH_DEPTH as u64 {
        assert_eq!(
            outbox.offer(1, &[event(n)]),
            0,
            "nothing is dropped inside the depth"
        );
    }
    assert_eq!(outbox.len(), EVENT_BATCH_DEPTH);
    assert_eq!(
        outbox.offer(2, &[event(999)]),
        1,
        "the oldest is dropped, and counted"
    );
    assert_eq!(outbox.dropped_since_accepted(), 1);
    assert_eq!(
        outbox.len(),
        EVENT_BATCH_DEPTH,
        "and the batch stays bounded"
    );
}

#[tokio::test]
async fn a_snapshot_that_disagrees_with_its_descriptor_is_refused() {
    let f = started(Via::Memory).await;
    let descriptor = f
        .harness
        .coordinator
        .session_descriptor()
        .expect("a descriptor")
        .clone();
    let snapshot = {
        let state = f.harness.coordinator.published_state();
        let state = state.lock().expect("not poisoned");
        state.latest_snapshot().expect("boundary 0").clone()
    };
    let positions: BTreeMap<String, u64> = snapshot
        .audio
        .iter()
        .map(|a| (a.stream_id.clone(), a.first_sample + a.sample_frames))
        .collect();
    let attachments: Vec<(String, ArtifactRef)> = snapshot
        .views
        .iter()
        .map(|v| {
            (
                fly_session::media::view_attachment(&v.view_id),
                v.pixels.clone(),
            )
        })
        .chain(snapshot.audio.iter().map(|a| {
            (
                fly_session::media::audio_attachment(&a.stream_id),
                a.samples.clone(),
            )
        }))
        .collect();

    let mut wrong = snapshot.clone();
    wrong.descriptor_revision = DESCRIPTOR_REVISION + 1;
    let error = check_publication(&descriptor, &wrong, &attachments, &positions)
        .expect_err("another revision is a disagreement");
    assert_eq!(error.code, ErrorCode::BufferInvalid, "{error:?}");

    let mut renamed = snapshot.clone();
    renamed.agents[0].agent_id = id("fly-z");
    let error = check_publication(&descriptor, &renamed, &attachments, &positions)
        .expect_err("an agent the descriptor does not declare");
    assert!(error.message.contains("fly-z"), "{}", error.message);

    // And a snapshot the session actually published passes the same check.
    check_publication(&descriptor, &snapshot, &attachments, &positions)
        .expect("what the session published agrees with the descriptor it named");
    f.shutdown().await;
}
