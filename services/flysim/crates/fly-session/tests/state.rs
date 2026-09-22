//! STATE-01 acceptance: one coherent all-participant checkpoint, and the recovery that
//! installs it into a fresh epoch.
//!
//! Every test here is one of the slice's acceptance bullets or one of the failure-injection
//! rows the implementation guide's section 4 assigns to it. Each of them runs over both
//! transports and in all three execution modes: the recovery path crosses a process boundary
//! in exactly the places the media path does, so a row that holds in one mode has to hold in
//! all of them.
//!
//! The byte layout itself is proved against the contract crate in
//! `fly-session-types/tests/checkpoint_envelope.rs`; these prove the session's use of it.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use common::{Fixture, count, fixture, fly_a, fly_b, within};
use fly_session::agent::AgentFaults;
use fly_session::harness::{ExecutionMode, HarnessConfig, Via};
use fly_session::phase::Phase;
use fly_session::state::{SaveOutcome, WriterFaults};
use fly_session::types::*;
use fly_session_types::checkpoint;

both_transports!(
    an_uninterrupted_run_and_a_resumed_run_produce_matching_traces,
    corrupting_any_single_participants_payload_fails_the_install_as_a_group,
    a_lost_save_reply_does_not_advance_durable_metadata,
    a_failure_during_activation_cannot_resume_half_a_world,
    the_checkpoint_queue_under_stress_stays_bounded,
    old_media_cannot_cross_a_recovery,
    a_checkpoint_taken_by_another_parser_is_refused_by_name,
    a_group_where_one_participant_will_not_stage_resumes_nothing,
    a_restore_token_activates_only_once,
    the_fence_lifts_only_through_a_coherent_restore,
    a_queued_replaceable_capture_is_superseded_rather_than_duplicated,
    a_capture_is_refused_anywhere_but_a_committed_boundary,
);

all_modes!(
    matching_traces_in_every_mode,
    a_group_install_fails_as_a_group_in_every_mode,
    a_lost_save_reply_holds_durable_metadata_in_every_mode,
    half_an_activation_resumes_nothing_in_every_mode,
    the_queue_stays_bounded_in_every_mode,
    old_media_cannot_cross_in_every_mode,
    another_parser_is_refused_in_every_mode,
    a_refused_stage_resumes_nothing_in_every_mode,
    a_token_activates_once_in_every_mode,
    the_fence_lifts_only_by_restore_in_every_mode,
);

const BEFORE: u64 = 2;
const AFTER: u64 = 2;

fn ckpt(n: u32) -> Id {
    id(&format!("ckpt-{n}"))
}

/// A fixture in one transport and one execution mode.
///
/// `Via` is the composition's; a dedicated thread or a separate process reaches the router
/// over a socket whatever it says, which the launcher decides and this does not second-guess.
async fn fx(via: Via, mode: ExecutionMode, config: HarnessConfig) -> Fixture {
    fixture(via, HarnessConfig { mode, ..config }).await
}

/// Runs to a committed boundary and takes one durable checkpoint there.
async fn run_and_checkpoint(f: &mut Fixture, steps: u64, checkpoint_id: &Id) {
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(steps)).await.unwrap();
    let outcome = within("checkpoint", f.harness.coordinator.checkpoint(checkpoint_id))
        .await
        .unwrap();
    match outcome {
        SaveOutcome::Committed { boundary, .. } => assert_eq!(boundary, steps),
        other => panic!("the checkpoint was not committed: {other:?}"),
    }
    assert_eq!(
        f.harness.coordinator.durable(),
        Some((checkpoint_id.clone(), steps)),
        "a committed save is the only thing that moves the durable mark"
    );
}

/// Fails the epoch the way a participant death does, and checks the fence closed.
async fn fail_the_epoch(f: &mut Fixture) {
    f.harness.kill(&fly_a()).await;
    let failure = within("step", f.harness.coordinator.step())
        .await
        .expect_err("a dead participant fails the epoch");
    assert!(
        failure.participant.is_some(),
        "a diagnosed failure names its participant: {failure}"
    );
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert!(f.harness.coordinator.is_fenced());
    assert_eq!(
        f.harness.coordinator.live_view_handles(),
        0,
        "the fence drops every handle the old epoch held"
    );
}

/// The committed generation's bytes, as they are on disk.
fn generation_bytes(root: &Path, checkpoint_id: &Id) -> Vec<u8> {
    std::fs::read(root.join(format!("{checkpoint_id}.flysess"))).expect("a committed generation")
}

/// Writes a generation file and makes the store manifest describe it, as a repair tool or a
/// previous run would have left it.
fn write_generation(root: &Path, checkpoint_id: &Id, bytes: &[u8]) {
    std::fs::write(root.join(format!("{checkpoint_id}.flysess")), bytes).expect("writable");
    let path = root.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("a store manifest")).expect("json");
    let generations = manifest["generations"].as_array_mut().expect("an array");
    for generation in generations.iter_mut() {
        if generation["checkpointId"] == json!(checkpoint_id.as_str()) {
            generation["byteLength"] = json!(bytes.len().to_string());
            generation["envelopeDigest"] = json!(digest_of_bytes(bytes));
            let envelope = checkpoint::decode(bytes).expect("a well formed envelope");
            let compatibility = fly_session::state::Compatibility::from_json(
                &envelope.manifest["compatibility"],
            )
            .expect("a compatibility block");
            generation["compatibilityDigest"] = json!(compatibility.digest());
        }
    }
    std::fs::write(&path, serde_json::to_vec(&manifest).expect("json")).expect("writable");
}

/// Re-encodes one committed generation after `edit` has changed its manifest or its payloads.
fn rewrite_generation(
    root: &Path,
    checkpoint_id: &Id,
    edit: impl FnOnce(&mut Value, &mut Vec<(String, Vec<u8>)>),
) {
    let bytes = generation_bytes(root, checkpoint_id);
    let envelope = checkpoint::decode(&bytes).expect("a committed generation decodes");
    let mut manifest = envelope.manifest.clone();
    let mut payloads = envelope.payloads.clone();
    edit(&mut manifest, &mut payloads);
    // The manifest's payload table mirrors the envelope's, so it is rebuilt from the bytes
    // that are actually being written rather than left to disagree.
    manifest["payloads"] = Value::Array(
        payloads
            .iter()
            .map(|(name, bytes)| {
                json!({
                    "name": name,
                    "byteLength": bytes.len().to_string(),
                    "digest": digest_of_bytes(bytes),
                })
            })
            .collect(),
    );
    let rewritten = checkpoint::encode(&manifest, &payloads).expect("a valid envelope");
    write_generation(root, checkpoint_id, &rewritten);
}

/// Makes the store read its durable metadata again, after a test has edited it.
async fn reload_store(f: &mut Fixture) {
    f.harness
        .coordinator
        .writer()
        .expect("a store is attached")
        .with_store(|store| store.reload())
        .await
        .expect("the store manifest is still readable");
}

/// The names of every payload the checkpoint holds, participants first.
fn payload_names(root: &Path, checkpoint_id: &Id) -> Vec<String> {
    let bytes = generation_bytes(root, checkpoint_id);
    checkpoint::decode(&bytes)
        .expect("decodes")
        .payloads
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Every artifact identity this session's committed boundary is holding.
fn live_artifact_ids(f: &Fixture) -> BTreeSet<String> {
    f.harness
        .coordinator
        .media_handles()
        .into_iter()
        .map(|(_, reference)| reference.artifact_id)
        .collect()
}

/// Asserts that no participant is running the proposed epoch: nothing was installed.
async fn nothing_is_installed(f: &mut Fixture, epoch: &Id) {
    let ids: Vec<Id> = std::iter::once(f.harness.environment_id())
        .chain(f.harness.config.agents.iter().map(|a| a.agent_id.clone()))
        .collect();
    for worker_id in ids {
        let (_, launcher) = f.harness.parts();
        let Ok(status) = launcher.health_check(&worker_id).await else {
            // A participant that is gone is certainly not running the new epoch.
            continue;
        };
        if let Some(scope) = status.current_scope {
            assert_ne!(
                scope.epoch, *epoch,
                "{worker_id} is running the epoch the abandoned install proposed"
            );
        }
    }
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert!(f.harness.coordinator.is_fenced());
    let refused = within("step", f.harness.coordinator.step())
        .await
        .expect_err("a fenced session takes no step");
    assert_eq!(refused.error.code, ErrorCode::InvalidPhase);
}

// ===============================================================================================
// Acceptance: an uninterrupted run and a resumed run produce matching traces

async fn an_uninterrupted_run_and_a_resumed_run_produce_matching_traces(via: Via) {
    matching_traces(via, ExecutionMode::InProcess).await;
}

async fn matching_traces_in_every_mode(mode: ExecutionMode) {
    matching_traces(Via::Unix, mode).await;
}

/// The reference run and the resumed run commit the same behaviour, once the epoch metadata
/// the restore necessarily changed is accounted for.
///
/// `step-v1` section 8's split is the whole of the comparison: the behaviour is what must
/// match, the request ids and wall time are excluded because they are operational, and the
/// epoch is neither -- it is behaviour metadata, so it is rewritten explicitly and everything
/// else is compared byte for byte.
async fn matching_traces(via: Via, mode: ExecutionMode) {
    let mut reference = fx(via, mode, HarnessConfig::default()).await;
    within("bootstrap", reference.harness.coordinator.bootstrap()).await.unwrap();
    within("run", reference.harness.coordinator.run(BEFORE + AFTER)).await.unwrap();
    let expected = reference.harness.coordinator.trace.behavior();
    assert_eq!(expected.len() as u64, BEFORE + AFTER);
    reference.shutdown().await;

    let mut f = fx(via, mode, HarnessConfig::default()).await;
    let checkpoint_id = ckpt(1);
    run_and_checkpoint(&mut f, BEFORE, &checkpoint_id).await;
    fail_the_epoch(&mut f).await;
    within("replace", f.harness.replace_all_participants()).await.unwrap();
    let report = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e2")),
    )
    .await
    .unwrap();
    assert_eq!(report.boundary, BEFORE);
    assert_eq!(report.epoch, id("e2"));
    assert_eq!(report.activated.len(), report.staged.len());
    assert!(!f.harness.coordinator.is_fenced(), "a coherent restore lifts the fence");
    assert_eq!(f.harness.coordinator.phase(), Phase::Paused(BEFORE));

    f.harness.coordinator.resume().unwrap();
    within("resume", f.harness.coordinator.run(AFTER)).await.unwrap();
    assert_eq!(f.harness.coordinator.phase(), Phase::Ready(BEFORE + AFTER));

    // Without accounting for the epoch the two traces disagree, which is what makes the
    // rebase a statement rather than a formality.
    let raw = f.harness.coordinator.trace.behavior();
    assert_eq!(raw.len() as u64, BEFORE + AFTER);
    assert_ne!(raw, expected, "the resumed run runs in a different epoch");

    let rebase = f.harness.coordinator.rebase(&id("e1")).unwrap();
    let resumed = f.harness.coordinator.trace.behavior_rebased(&rebase).unwrap();
    assert_eq!(
        resumed, expected,
        "a resumed run commits the behaviour the uninterrupted run committed"
    );
    f.shutdown().await;
}

// ===============================================================================================
// Acceptance: corrupt any participant and installation fails as a group

async fn corrupting_any_single_participants_payload_fails_the_install_as_a_group(via: Via) {
    group_install_is_all_or_nothing(via, ExecutionMode::InProcess).await;
}

async fn a_group_install_fails_as_a_group_in_every_mode(mode: ExecutionMode) {
    group_install_is_all_or_nothing(Via::Unix, mode).await;
}

/// One corrupted payload -- any participant's, and the coordinator's own ledger too -- fails
/// the whole install, and the group afterwards is exactly as it was.
async fn group_install_is_all_or_nothing(via: Via, mode: ExecutionMode) {
    let mut f = fx(via, mode, HarnessConfig::default()).await;
    let checkpoint_id = ckpt(1);
    run_and_checkpoint(&mut f, BEFORE, &checkpoint_id).await;
    let root = f.harness.checkpoint_root().to_path_buf();
    let good = generation_bytes(&root, &checkpoint_id);
    let names = payload_names(&root, &checkpoint_id);
    // Every participant's payload, plus one of the coordinator's own ledgers.
    let mut corrupt: Vec<String> = names
        .iter()
        .filter(|name| name.starts_with("agent-") || *name == "world")
        .cloned()
        .collect();
    corrupt.push("task-ledger".to_owned());
    assert!(corrupt.len() >= 3, "the composition has several payloads: {names:?}");
    fail_the_epoch(&mut f).await;

    let mut epoch = 1u32;
    for target in &corrupt {
        epoch += 1;
        let proposed = id(&format!("e{epoch}"));
        // Each round starts from the intact bytes, so exactly one payload is corrupt.
        write_generation(&root, &checkpoint_id, &good);
        // A well formed envelope whose digests all agree, so what fails is the participant
        // reading its own bytes and not the envelope reader in front of it.
        let name = target.clone();
        rewrite_generation(&root, &checkpoint_id, |_manifest, payloads| {
            for (payload, bytes) in payloads.iter_mut() {
                if *payload == name {
                    let last = bytes.len() - 1;
                    bytes[last] ^= 0xff;
                }
            }
        });
        reload_store(&mut f).await;
        within("replace", f.harness.replace_all_participants()).await.unwrap();
        let failure = within(
            "restore",
            f.harness.coordinator.restore(Some(&checkpoint_id), &proposed),
        )
        .await
        .expect_err(&format!("a corrupt {target} must fail the install"));
        assert!(
            matches!(
                failure.error.code,
                ErrorCode::IncompatibleState | ErrorCode::InvalidArgument
            ),
            "a corrupt {target} is an explicit refusal, not {failure}"
        );
        nothing_is_installed(&mut f, &proposed).await;
        let tainted = f.harness.coordinator.tainted();
        if target == "world" {
            assert!(
                tainted.is_empty(),
                "the first participant asked refused, so nothing staged: {tainted:?}"
            );
        } else {
            assert!(
                !tainted.is_empty(),
                "a participant that staged into an abandoned install must be replaced"
            );
        }
    }

    // The same group, the same store, the intact bytes: the failures above installed nothing
    // that stops this from working.
    epoch += 1;
    write_generation(&root, &checkpoint_id, &good);
    reload_store(&mut f).await;
    within("replace", f.harness.replace_all_participants()).await.unwrap();
    let report = within(
        "restore",
        f.harness
            .coordinator
            .restore(Some(&checkpoint_id), &id(&format!("e{epoch}"))),
    )
    .await
    .unwrap();
    assert_eq!(report.boundary, BEFORE);
    f.harness.coordinator.resume().unwrap();
    within("resume", f.harness.coordinator.run(1)).await.unwrap();
    f.shutdown().await;
}

// ===============================================================================================
// Acceptance: a lost save reply does not advance durable metadata

async fn a_lost_save_reply_does_not_advance_durable_metadata(via: Via) {
    a_lost_save_reply(via, ExecutionMode::InProcess).await;
}

async fn a_lost_save_reply_holds_durable_metadata_in_every_mode(mode: ExecutionMode) {
    a_lost_save_reply(Via::Unix, mode).await;
}

/// Three ways a save can end without a saved acknowledgment. None moves the mark, and each
/// is reported as itself rather than as the others.
async fn a_lost_save_reply(via: Via, mode: ExecutionMode) {
    let lost = ckpt(1);
    let config = HarnessConfig {
        writer_faults: WriterFaults {
            drop_reply_for: Some(lost.clone()),
            ..WriterFaults::default()
        },
        ..HarnessConfig::default()
    };
    let mut f = fx(via, mode, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(1)).await.unwrap();

    let ticket = within("capture", f.harness.coordinator.capture(&lost, false))
        .await
        .unwrap();
    let outcome = within("durable", f.harness.coordinator.await_durable(ticket))
        .await
        .unwrap();
    assert_eq!(outcome, SaveOutcome::ReplyLost { checkpoint_id: lost.clone() });
    assert_eq!(
        f.harness.coordinator.durable(),
        None,
        "a lost save reply never moves the durable mark"
    );
    assert_eq!(count(&f.harness.coordinator.audit, &format!("durable:{lost}@1")), 0);

    // The resolution asks the durable metadata about the *same* operation. It never saves
    // again, and here the bytes did reach the store manifest.
    let resolved = within("resolve", f.harness.coordinator.resolve_durable(&lost))
        .await
        .unwrap();
    assert_eq!(resolved, Some(1));
    assert_eq!(f.harness.coordinator.durable(), Some((lost.clone(), 1)));
    let commits = f
        .harness
        .coordinator
        .writer()
        .expect("a store")
        .with_store(|store| store.commits())
        .await;
    assert_eq!(commits, 1, "the resolution queried the store; it did not save again");
    f.shutdown().await;

    // The other half: the generation file is renamed and the store manifest is never
    // committed. That generation is unreferenced, so it is not a candidate and the mark is
    // still where it was.
    let orphan = ckpt(2);
    let mut config = HarnessConfig::default();
    config.store_faults.stop_before_manifest_commit = true;
    let mut g = fx(via, mode, config).await;
    within("bootstrap", g.harness.coordinator.bootstrap()).await.unwrap();
    within("run", g.harness.coordinator.run(1)).await.unwrap();
    let outcome = within("checkpoint", g.harness.coordinator.checkpoint(&orphan))
        .await
        .unwrap();
    match outcome {
        SaveOutcome::Failed { checkpoint_id, .. } => assert_eq!(checkpoint_id, orphan),
        other => panic!("an uncommitted manifest is not a saved checkpoint: {other:?}"),
    }
    assert_eq!(g.harness.coordinator.durable(), None);
    let root = g.harness.checkpoint_root().to_path_buf();
    assert!(
        root.join(format!("{orphan}.flysess")).is_file(),
        "the generation file was written and renamed"
    );
    let resolved = within("resolve", g.harness.coordinator.resolve_durable(&orphan))
        .await
        .unwrap();
    assert_eq!(resolved, None, "an unreferenced generation is never a restore candidate");
    assert_eq!(g.harness.coordinator.durable(), None);
    g.shutdown().await;

    // The third: the caller's own budget runs out while the save is still going. That is a
    // different fact from a lost reply -- this save has not stopped -- and it is reported as
    // itself, because diagnosing one as the other is the implicit reading these rules refuse.
    let slow = ckpt(3);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut config = HarnessConfig::default();
    config.writer_faults.gate = Some(gate.clone());
    let mut h = fx(via, mode, config).await;
    within("bootstrap", h.harness.coordinator.bootstrap()).await.unwrap();
    within("run", h.harness.coordinator.run(1)).await.unwrap();
    // The durable budget is the caller's own and is not the call budget: this shortens the
    // wait for an acknowledgment without shortening a single call to a participant.
    h.harness.coordinator.deadlines.durable = Duration::from_millis(100);
    let ticket = within("capture", h.harness.coordinator.capture(&slow, false))
        .await
        .unwrap();
    let outcome = within("durable", h.harness.coordinator.await_durable(ticket))
        .await
        .unwrap();
    assert_eq!(
        outcome,
        SaveOutcome::DeadlineExpired { checkpoint_id: slow.clone() },
        "an expired caller budget is not a lost reply"
    );
    assert_ne!(outcome, SaveOutcome::ReplyLost { checkpoint_id: slow.clone() });
    assert_eq!(h.harness.coordinator.durable(), None);
    assert_eq!(
        count(
            &h.harness.coordinator.audit,
            &format!("not-durable:failed:{slow}@1")
        ),
        1,
        "the session records that this capture is not durable: {:?}",
        h.harness.coordinator.audit
    );

    // It really was still going: once the writer is let past its gate the same operation
    // commits, and resolving it is what moves the mark.
    gate.add_permits(16);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let resolved = loop {
        let found = within("resolve", h.harness.coordinator.resolve_durable(&slow))
            .await
            .unwrap();
        if found.is_some() || std::time::Instant::now() >= deadline {
            break found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(resolved, Some(1), "the save the caller stopped waiting for still committed");
    assert_eq!(h.harness.coordinator.durable(), Some((slow, 1)));
    h.shutdown().await;
}

// ===============================================================================================
// Acceptance: a failure during activation cannot resume half a world

async fn a_failure_during_activation_cannot_resume_half_a_world(via: Via) {
    half_an_activation(via, ExecutionMode::InProcess).await;
}

async fn half_an_activation_resumes_nothing_in_every_mode(mode: ExecutionMode) {
    half_an_activation(Via::Unix, mode).await;
}

/// The world and the first agent activate; the second refuses. Nothing plays, the fence
/// stays closed, and the group cannot be resumed until every participant is replaced.
async fn half_an_activation(via: Via, mode: ExecutionMode) {
    let mut f = fx(via, mode, HarnessConfig::default()).await;
    let checkpoint_id = ckpt(1);
    run_and_checkpoint(&mut f, BEFORE, &checkpoint_id).await;
    fail_the_epoch(&mut f).await;

    // The replacement for fly-b refuses to activate after it has staged.
    f.harness.set_agent_faults(
        &fly_b(),
        AgentFaults { fail_activate_restore: true, ..AgentFaults::default() },
    );
    within("replace", f.harness.replace_all_participants()).await.unwrap();
    let failure = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e2")),
    )
    .await
    .expect_err("a refused activation fails the install");
    assert_eq!(failure.participant.as_deref(), Some(fly_b().as_str()));
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert!(f.harness.coordinator.is_fenced(), "the group stays fenced");
    let refused = within("step", f.harness.coordinator.step())
        .await
        .expect_err("half a world never runs");
    assert_eq!(refused.error.code, ErrorCode::InvalidPhase);

    // The participants that got as far as installing hold state no group resumed. Another
    // restore over them is refused by name rather than attempted.
    let tainted = f.harness.coordinator.tainted();
    assert!(tainted.contains(&f.harness.environment_id()), "{tainted:?}");
    assert!(tainted.contains(&fly_a()), "{tainted:?}");
    let refused = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e3")),
    )
    .await
    .expect_err("the half-installed group is not restored over");
    assert_eq!(refused.error.code, ErrorCode::InvalidPhase);
    assert!(refused.error.message.contains(&fly_a()), "{refused}");

    // A fresh group, without the injected refusal, resumes the same boundary.
    f.harness.set_agent_faults(&fly_b(), AgentFaults::default());
    within("replace", f.harness.replace_all_participants()).await.unwrap();
    assert!(f.harness.coordinator.tainted().is_empty());
    let report = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e4")),
    )
    .await
    .unwrap();
    assert_eq!(report.boundary, BEFORE);
    assert!(!f.harness.coordinator.is_fenced());
    f.harness.coordinator.resume().unwrap();
    within("resume", f.harness.coordinator.run(1)).await.unwrap();
    f.shutdown().await;
}

// ===============================================================================================
// Acceptance: the checkpoint queue under stress stays bounded

async fn the_checkpoint_queue_under_stress_stays_bounded(via: Via) {
    queue_stays_bounded(via, ExecutionMode::InProcess).await;
}

async fn the_queue_stays_bounded_in_every_mode(mode: ExecutionMode) {
    queue_stays_bounded(Via::Unix, mode).await;
}

/// With the writer stalled, the queue fills to its configured bound and the next capture is
/// refused *before* a single participant is asked for one.
async fn queue_stays_bounded(via: Via, mode: ExecutionMode) {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut config = HarnessConfig::default();
    config.writer_faults.gate = Some(gate.clone());
    config.writer.queue_capacity = 2;
    let mut f = fx(via, mode, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(1)).await.unwrap();

    let first = within("capture", f.harness.coordinator.capture(&ckpt(1), false))
        .await
        .unwrap();
    let second = within("capture", f.harness.coordinator.capture(&ckpt(2), false))
        .await
        .unwrap();
    assert_eq!(f.harness.coordinator.writer().expect("a store").outstanding(), 2);

    let environment = f.harness.environment_id();
    let before = within("status", f.harness.progress_of(&environment)).await.unwrap();
    let refused = within("capture", f.harness.coordinator.capture(&ckpt(3), false))
        .await
        .expect_err("a full queue refuses");
    assert_eq!(refused.error.code, ErrorCode::Busy);
    assert_eq!(refused.error.mutation, MutationCertainty::None);
    let after = within("status", f.harness.progress_of(&environment)).await.unwrap();
    assert_eq!(
        before, after,
        "a refused capture asks no participant for one: rejection happens before capture"
    );
    assert_eq!(count(&f.harness.coordinator.audit, "capture-refused:ckpt-3"), 1);

    // A refused capture is not a failed session: the world keeps stepping.
    assert!(!f.harness.coordinator.is_fenced());
    within("run", f.harness.coordinator.run(1)).await.unwrap();
    assert_eq!(f.harness.coordinator.phase(), Phase::Ready(2));

    let stats = f.harness.coordinator.writer().expect("a store").stats();
    assert_eq!(stats.rejected, 1);
    assert!(stats.peak_queue <= 2, "the queue never exceeded its bound: {stats:?}");
    assert!(stats.peak_bytes <= 64 * 1024 * 1024);

    gate.add_permits(16);
    for (ticket, expected) in [(first, ckpt(1)), (second, ckpt(2))] {
        let outcome = within("durable", f.harness.coordinator.await_durable(ticket))
            .await
            .unwrap();
        match outcome {
            SaveOutcome::Committed { checkpoint_id, .. } => assert_eq!(checkpoint_id, expected),
            other => panic!("an unstalled writer commits: {other:?}"),
        }
    }
    let stats = f.harness.coordinator.writer().expect("a store").stats();
    assert_eq!(stats.committed, 2);
    assert_eq!(f.harness.coordinator.writer().expect("a store").outstanding(), 0);
    f.shutdown().await;
}

/// A queued replaceable capture is replaced by the next one, releasing its holds, rather than
/// both being written.
async fn a_queued_replaceable_capture_is_superseded_rather_than_duplicated(via: Via) {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut config = HarnessConfig::default();
    config.writer_faults.gate = Some(gate.clone());
    config.writer.queue_capacity = 3;
    let mut f = fx(via, ExecutionMode::InProcess, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(1)).await.unwrap();

    // The first job is taken by the writer and stalls at the gate; the next two are queued.
    let durable = within("capture", f.harness.coordinator.capture(&ckpt(1), false))
        .await
        .unwrap();
    let hot = within("capture", f.harness.coordinator.capture(&ckpt(2), true))
        .await
        .unwrap();
    let newer = within("capture", f.harness.coordinator.capture(&ckpt(3), true))
        .await
        .unwrap();

    let superseded = within("durable", f.harness.coordinator.await_durable(hot))
        .await
        .unwrap();
    assert_eq!(
        superseded,
        SaveOutcome::Superseded { checkpoint_id: ckpt(2), by: ckpt(3) },
        "only a queued replaceable capture is coalesced"
    );
    gate.add_permits(16);
    for ticket in [durable, newer] {
        let outcome = within("durable", f.harness.coordinator.await_durable(ticket))
            .await
            .unwrap();
        assert!(matches!(outcome, SaveOutcome::Committed { .. }), "{outcome:?}");
    }
    let stats = f.harness.coordinator.writer().expect("a store").stats();
    assert_eq!(stats.superseded, 1);
    assert_eq!(stats.committed, 2, "the superseded capture was never written");
    f.shutdown().await;
}

// ===============================================================================================
// Acceptance: old media and old parser data cannot cross a recovery

async fn old_media_cannot_cross_a_recovery(via: Via) {
    old_media_cannot_cross(via, ExecutionMode::InProcess).await;
}

async fn old_media_cannot_cross_in_every_mode(mode: ExecutionMode) {
    old_media_cannot_cross(Via::Unix, mode).await;
}

/// Nothing the fence dropped comes back: the payloads are re-imported as fresh artifacts, the
/// restored view is a new object, and the new epoch's first audio chunk resumes the preserved
/// sample position and marks the discontinuity.
async fn old_media_cannot_cross(via: Via, mode: ExecutionMode) {
    let mut f = fx(via, mode, HarnessConfig::default()).await;
    let checkpoint_id = ckpt(1);
    run_and_checkpoint(&mut f, BEFORE, &checkpoint_id).await;
    let old_ids = live_artifact_ids(&f);
    assert!(!old_ids.is_empty(), "the committed boundary holds media handles");
    let positions = f.harness.coordinator.audio_positions();
    assert!(positions.values().any(|sample| *sample > 0), "audio has played");

    fail_the_epoch(&mut f).await;
    within("replace", f.harness.replace_all_participants()).await.unwrap();
    let report = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e2")),
    )
    .await
    .unwrap();
    for imported in &report.imported {
        assert!(
            !old_ids.contains(imported),
            "a checkpoint payload was imported as an artifact the old epoch already had"
        );
    }
    let new_ids = live_artifact_ids(&f);
    assert!(!new_ids.is_empty());
    assert!(
        new_ids.is_disjoint(&old_ids),
        "the restored boundary's media are fresh artifacts, not the old epoch's"
    );
    assert_eq!(
        f.harness.coordinator.audio_positions(),
        positions,
        "crash restore preserves the sample position"
    );

    f.harness.coordinator.resume().unwrap();
    within("resume", f.harness.coordinator.run(1)).await.unwrap();
    let chunk = f
        .harness
        .coordinator
        .observation()
        .expect("a restored world")
        .audio
        .first()
        .cloned()
        .expect("one chunk per transition");
    assert!(
        chunk.discontinuity,
        "the first chunk of a fresh epoch marks the discontinuity the recovery established"
    );
    assert_eq!(
        chunk.first_sample,
        positions["arena"],
        "and it starts where the checkpointed stream stopped"
    );
    f.shutdown().await;
}

async fn a_checkpoint_taken_by_another_parser_is_refused_by_name(via: Via) {
    another_parser_is_refused(via, ExecutionMode::InProcess).await;
}

async fn another_parser_is_refused_in_every_mode(mode: ExecutionMode) {
    another_parser_is_refused(Via::Unix, mode).await;
}

/// A checkpoint whose recorded parser identity is not this composition's is refused before a
/// single participant is asked to stage, and the refusal names the identity that differs.
async fn another_parser_is_refused(via: Via, mode: ExecutionMode) {
    let mut f = fx(via, mode, HarnessConfig::default()).await;
    let checkpoint_id = ckpt(1);
    run_and_checkpoint(&mut f, 1, &checkpoint_id).await;
    let root = f.harness.checkpoint_root().to_path_buf();
    fail_the_epoch(&mut f).await;
    rewrite_generation(&root, &checkpoint_id, |manifest, _payloads| {
        manifest["compatibility"]["parserDigest"] =
            json!(digest_of_bytes(b"some other inspection schema"));
    });
    reload_store(&mut f).await;
    within("replace", f.harness.replace_all_participants()).await.unwrap();
    let failure = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e2")),
    )
    .await
    .expect_err("another parser's state is not this composition's");
    assert_eq!(failure.error.code, ErrorCode::IncompatibleState);
    assert!(
        failure.error.message.contains("parser"),
        "the refusal names the identity that differs: {failure}"
    );
    assert!(
        f.harness.coordinator.tainted().is_empty(),
        "nothing was asked to stage, so nothing has to be replaced"
    );
    nothing_is_installed(&mut f, &id("e2")).await;
    f.shutdown().await;
}

// ===============================================================================================
// Failure-injection rows

async fn a_group_where_one_participant_will_not_stage_resumes_nothing(via: Via) {
    a_refused_stage(via, ExecutionMode::InProcess).await;
}

async fn a_refused_stage_resumes_nothing_in_every_mode(mode: ExecutionMode) {
    a_refused_stage(Via::Unix, mode).await;
}

/// The checklist row: the install validates the participants ahead of it and the last one
/// fails. Nothing is resumed.
async fn a_refused_stage(via: Via, mode: ExecutionMode) {
    let mut f = fx(via, mode, HarnessConfig::default()).await;
    let checkpoint_id = ckpt(1);
    run_and_checkpoint(&mut f, 1, &checkpoint_id).await;
    fail_the_epoch(&mut f).await;
    f.harness.set_agent_faults(
        &fly_b(),
        AgentFaults { fail_stage_restore: true, ..AgentFaults::default() },
    );
    within("replace", f.harness.replace_all_participants()).await.unwrap();
    let failure = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e2")),
    )
    .await
    .expect_err("a participant that will not validate stops the install");
    assert_eq!(failure.error.code, ErrorCode::IncompatibleState);
    assert_eq!(failure.participant.as_deref(), Some(fly_b().as_str()));
    assert_eq!(
        count(&f.harness.coordinator.audit, &format!("activated:{}", fly_a())),
        0,
        "no participant activates when one of them will not stage"
    );
    nothing_is_installed(&mut f, &id("e2")).await;
    f.shutdown().await;
}

async fn a_restore_token_activates_only_once(via: Via) {
    a_token_activates_once(via, ExecutionMode::InProcess).await;
}

async fn a_token_activates_once_in_every_mode(mode: ExecutionMode) {
    a_token_activates_once(Via::Unix, mode).await;
}

/// A token is bound to its checkpoint, scope and payload and activates once. A fresh request
/// naming it again is a conflict, and the old epoch's scope is stale on the new participants.
async fn a_token_activates_once(via: Via, mode: ExecutionMode) {
    let mut f = fx(via, mode, HarnessConfig::default()).await;
    let checkpoint_id = ckpt(1);
    run_and_checkpoint(&mut f, 1, &checkpoint_id).await;
    fail_the_epoch(&mut f).await;
    within("replace", f.harness.replace_all_participants()).await.unwrap();
    let report = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e2")),
    )
    .await
    .unwrap();
    let (who, token) = report.tokens.first().cloned().expect("a staged token");
    let worker = if who == f.harness.environment_id() {
        f.harness.coordinator.environment_ref().clone()
    } else {
        f.harness.coordinator.agent_ref(&who).expect("a participant").clone()
    };
    let scope = scope_at("demo", "e2", 1);
    let refused = within(
        "activate",
        f.harness.coordinator.probe_raw(
            &worker,
            "State.ActivateRestore",
            Some(scope),
            json!({"restoreToken": token.as_str()}),
        ),
    )
    .await
    .expect_err("a token activates once");
    assert_eq!(refused.code, ErrorCode::Conflict);

    // And the epoch the restore left behind is stale on every replacement.
    let stale = within(
        "prepare",
        f.harness.coordinator.probe_raw(
            &worker,
            if who == f.harness.environment_id() { "Environment.Advance" } else { "Agent.Prepare" },
            Some(scope_at("demo", "e1", 1)),
            json!({}),
        ),
    )
    .await
    .expect_err("the old epoch is gone");
    assert!(
        matches!(stale.code, ErrorCode::StaleEpoch | ErrorCode::InvalidArgument),
        "an old-epoch request is refused: {stale}"
    );
    f.shutdown().await;
}

async fn the_fence_lifts_only_through_a_coherent_restore(via: Via) {
    the_fence_lifts_only_by_restore(via, ExecutionMode::InProcess).await;
}

async fn the_fence_lifts_only_by_restore_in_every_mode(mode: ExecutionMode) {
    the_fence_lifts_only_by_restore(Via::Unix, mode).await;
}

/// Nothing but a coherent restore moves a fenced session, and the restore lands on
/// `Paused(k)` rather than straight back into play.
async fn the_fence_lifts_only_by_restore(via: Via, mode: ExecutionMode) {
    let mut f = fx(via, mode, HarnessConfig::default()).await;
    let checkpoint_id = ckpt(1);
    run_and_checkpoint(&mut f, 1, &checkpoint_id).await;
    fail_the_epoch(&mut f).await;

    for refused in [
        within("step", f.harness.coordinator.step()).await.err(),
        within("bootstrap", f.harness.coordinator.bootstrap()).await.err(),
        within("capture", f.harness.coordinator.capture(&ckpt(9), false))
            .await
            .err(),
    ] {
        let refused = refused.expect("a fenced session refuses");
        assert_eq!(refused.error.code, ErrorCode::InvalidPhase);
    }
    assert!(f.harness.coordinator.resume().is_err(), "a fenced session does not resume");
    assert!(f.harness.coordinator.is_fenced());

    // A restore into the epoch that failed is refused: recovery establishes a fresh one.
    within("replace", f.harness.replace_all_participants()).await.unwrap();
    let same = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e1")),
    )
    .await
    .expect_err("a restore installs a fresh epoch");
    assert_eq!(same.error.code, ErrorCode::StaleEpoch);

    let report = within(
        "restore",
        f.harness.coordinator.restore(Some(&checkpoint_id), &id("e2")),
    )
    .await
    .unwrap();
    assert_eq!(report.boundary, 1);
    assert!(!f.harness.coordinator.is_fenced());
    assert_eq!(f.harness.coordinator.phase(), Phase::Paused(1));
    f.harness.coordinator.resume().unwrap();
    within("resume", f.harness.coordinator.run(1)).await.unwrap();
    assert_eq!(f.harness.coordinator.phase(), Phase::Ready(2));
    f.shutdown().await;
}

/// A capture belongs to a committed boundary, and the phase machine is what says so.
async fn a_capture_is_refused_anywhere_but_a_committed_boundary(via: Via) {
    let mut f = fx(via, ExecutionMode::InProcess, HarnessConfig::default()).await;
    // Before bootstrap the session is Starting, which is not a boundary at all.
    let refused = within("capture", f.harness.coordinator.capture(&ckpt(1), false))
        .await
        .expect_err("Starting is not a committed boundary");
    assert_eq!(refused.error.code, ErrorCode::InvalidPhase);
    f.shutdown().await;

    // From a pause, which is a committed boundary, it works, and it returns there.
    let mut f = fx(via, ExecutionMode::InProcess, HarnessConfig::default()).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(1)).await.unwrap();
    f.harness.coordinator.request_pause();
    within("run", f.harness.coordinator.run(1)).await.unwrap();
    assert_eq!(f.harness.coordinator.phase(), Phase::Paused(2));
    let outcome = within("checkpoint", f.harness.coordinator.checkpoint(&ckpt(1)))
        .await
        .unwrap();
    assert!(matches!(outcome, SaveOutcome::Committed { boundary: 2, .. }), "{outcome:?}");
    assert_eq!(
        f.harness.coordinator.phase(),
        Phase::Paused(2),
        "a capture returns to the boundary it came from"
    );
    f.shutdown().await;
}
