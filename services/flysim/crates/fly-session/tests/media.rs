//! MEDIA-01 acceptance: native observations on the bus artifact, and the presentation handoff.
//!
//! Every test here is one of the slice's acceptance bullets or one row of the domain retention
//! table in `state-media-v1` section 3. The shape rules themselves are proved against the
//! contract crate in `fly-session-types/tests/media_shapes.rs`; these prove the session's use
//! of them: one shared image, spectators that cannot touch sensory state, a renderer that
//! keeps its handle, and a persistent asset that is not a transient artifact.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Map;

use common::{Fixture, default_fixture, fixture, fly_a, fly_b, mode_fixture, within};
use fly_session::environment::{
    AUDIO_STREAM_ID, CHANNELS, EnvironmentFaults, SAMPLE_RATE, VIEW_HEIGHT, VIEW_WIDTH,
    synthetic_asset,
};
use fly_session::harness::{ExecutionMode, HarnessConfig, Via};
use fly_session::media::{
    AssetRegistry, AudioSource, AudioTimelines, SensedView, Spectator, SpectatorFrame,
    arena_frame, audio_attachment, detach_frame, view_attachment,
};
use fly_session::phase::Phase;
use fly_session::types::*;
use fly_session_types::media::{AudioTimeline, check_imported_asset, require_finite_samples};

both_transports!(
    one_shared_image_reaches_both_agents_through_owned_attachments,
    a_spectator_cannot_corrupt_sensory_state,
    a_slow_spectator_exhausts_only_its_own_credits,
    delayed_rendering_retains_its_handle_after_the_message_drops,
    a_declared_render_delay_repeats_o0_until_the_pipeline_fills,
    an_extra_delayed_sensory_view_fails_the_step,
    a_frame_of_the_wrong_length_fails_the_step,
    overlapping_audio_fails_the_step,
    one_audio_chunk_per_boundary_with_an_exact_sample_budget,
    required_agent_input_is_never_coalesced_while_spectator_snapshots_are,
    a_persistent_asset_and_a_transient_artifact_are_different_identities,
    a_restored_audio_source_resumes_and_marks_the_discontinuity,
    a_missing_audio_chunk_fails_the_step,
    restored_timelines_need_every_declared_streams_position,
    a_cadence_that_does_not_divide_the_sample_rate_still_lands_on_whole_samples,
);

all_modes!(the_media_path_works_in_every_execution_mode);

const STEPS: u64 = 3;

/// Polls until `ok` holds, so a test never asserts a collection that has not happened yet.
async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(std::time::Instant::now() < deadline, "{what}: never happened");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Drains a latest subscription until the snapshot for `boundary` arrives.
///
/// A latest subscription converges on the newest value rather than delivering every one, so a
/// test that wants a particular committed boundary reads until it gets there.
async fn frame_at(spectator: &mut Spectator, boundary: u64) -> SpectatorFrame {
    for _ in 0..64 {
        let frame = within("snapshot", spectator.take_frame())
            .await
            .expect("a snapshot");
        if frame.boundary == boundary {
            return frame;
        }
    }
    panic!("the spectator never reached boundary {boundary}");
}

/// The views one agent read.
///
/// The sensor log is shared memory, so it is readable only where that agent lives. These tests
/// run in the default in-process composition; the process-mode test below asserts the media
/// path over the bus instead, which is what crosses a process boundary.
fn sensed(f: &Fixture, agent_id: &Id) -> Vec<SensedView> {
    f.harness
        .sensor_log(agent_id)
        .expect("this composition keeps its agents in this process")
        .entries()
}

/// How many native frames the world rendered, for a world in this process.
fn renders(f: &Fixture) -> u64 {
    f.harness
        .renders()
        .expect("this composition keeps its world in this process")
}

fn boundaries(entries: &[SensedView]) -> Vec<u64> {
    entries.iter().map(|e| e.boundary).collect()
}

fn produced(entries: &[SensedView]) -> Vec<u64> {
    entries.iter().map(|e| e.produced_step).collect()
}

/// One image per boundary reaches both agents as an owned attachment, and the same handle is
/// published for presentation. There is no second copy of the pixels anywhere.
async fn one_shared_image_reaches_both_agents_through_owned_attachments(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let observer = f.harness.observer().await.unwrap();
    let topic = f.harness.coordinator.topics().snapshots.clone();
    let mut spectator = Spectator::attach(&observer, &topic, 2).await.unwrap();
    within("run", f.harness.coordinator.run(STEPS)).await.unwrap();

    // One render per boundary: forwarding the handle to two agents and to publication does not
    // render or copy it again.
    assert_eq!(
        renders(&f),
        STEPS + 1,
        "one native frame per boundary, whatever the number of recipients"
    );

    let a = sensed(&f, &fly_a());
    let b = sensed(&f, &fly_b());
    assert_eq!(boundaries(&a), (0..=STEPS).collect::<Vec<_>>());
    assert_eq!(a, b, "both agents read the same artifact and the same bytes");
    assert_eq!(produced(&a), (0..=STEPS).collect::<Vec<_>>(), "no declared delay");

    // The handle the agents were given is the handle presentation was published.
    let published = frame_at(&mut spectator, STEPS).await;
    assert_eq!(
        published.agents,
        vec![fly_a(), fly_b()],
        "one snapshot carries the whole multi-agent session"
    );
    let last = a.last().expect("an entry per boundary");
    assert_eq!(published.view.pixels.artifact_id, last.artifact_id);
    assert_eq!(published.view.produced_step, last.produced_step);
    let bytes = published.artifact.read_all().await.expect("the published frame");
    assert_eq!(bytes.len() as u64, VIEW_WIDTH * VIEW_HEIGHT * 4);
    assert_eq!(digest_of_bytes(&bytes), last.digest, "the bytes both agents read");

    // The coordinator holds exactly one handle per declared view and stream at this boundary.
    let handles = f.harness.coordinator.media_handles();
    assert_eq!(handles.len(), 2, "one view and one audio chunk: {handles:?}");
    assert!(handles.iter().any(|(name, r)| *name == view_attachment("arena")
        && r.artifact_id == last.artifact_id));
    f.shutdown().await;
}

/// A spectator reads committed snapshots and cannot touch what the agents sense: it has no
/// authority over the environment, and its own reads leave sensory state exactly as produced.
async fn a_spectator_cannot_corrupt_sensory_state(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let observer = f.harness.observer().await.unwrap();
    let topic = f.harness.coordinator.topics().snapshots.clone();
    let mut spectator = Spectator::attach(&observer, &topic, 2).await.unwrap();

    // Naming the environment is not authority to drive it: a subscriber cannot call it.
    let refused = observer
        .call("env.arena", None, "Environment.Advance", Map::new(), &[])
        .await;
    assert!(
        refused.is_err(),
        "a spectator must not be able to call the environment"
    );

    within("run", f.harness.coordinator.run(STEPS)).await.unwrap();
    // The spectator reads the snapshot and releases it while the session keeps stepping.
    let frame = within("snapshot", spectator.take_frame()).await.expect("a snapshot");
    let seen = frame.artifact.read_all().await.expect("readable");
    drop(frame);
    within("run more", f.harness.coordinator.run(STEPS)).await.unwrap();

    let a = sensed(&f, &fly_a());
    let b = sensed(&f, &fly_b());
    assert_eq!(boundaries(&a), (0..=2 * STEPS).collect::<Vec<_>>());
    assert_eq!(a, b);
    assert_eq!(
        f.harness.coordinator.committed_boundary(),
        Some(2 * STEPS),
        "the spectator changed nothing about the session's progress"
    );
    // What the spectator read was one of the frames the agents encoded, unchanged.
    let digest = digest_of_bytes(&seen);
    assert!(
        a.iter().any(|entry| entry.digest == digest),
        "a spectator sees the committed frame, and only reads it"
    );
    f.shutdown().await;
}

/// A slow spectator exhausts its own credits. New snapshots replace its queued value; the
/// world never waits for it.
async fn a_slow_spectator_exhausts_only_its_own_credits(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let observer = f.harness.observer().await.unwrap();
    let topic = f.harness.coordinator.topics().snapshots.clone();
    // One credit and one queued value: the smallest spectator the bus allows.
    let mut slow = Spectator::attach(&observer, &topic, 1).await.unwrap();
    let mut fast = Spectator::attach(&observer, &topic, 2).await.unwrap();

    // The slow one takes one snapshot and stops rendering, holding its only credit.
    assert!(within("first snapshot", slow.hold_one()).await);
    assert_eq!(slow.held(), 1);

    let steps = 5;
    within("run", f.harness.coordinator.run(steps)).await.unwrap();
    assert_eq!(f.harness.coordinator.stats().advances, steps);

    // With its credit in use it receives nothing more, and its queued value is replaced.
    assert!(!slow.try_hold_one(), "a spectator out of credits gets nothing more");
    assert_eq!(slow.seen(), 1);
    slow.release();
    let next = within("after release", slow.hold_one()).await;
    assert!(next, "releasing its own delivery returns its own credit");
    assert!(
        slow.coalesced() > 0,
        "new snapshots replaced the value queued for a spectator that was not reading"
    );

    // The session and the other spectator are untouched.
    let latest = frame_at(&mut fast, steps).await;
    assert_eq!(
        latest.boundary, steps,
        "the reading spectator reaches the latest boundary"
    );
    let a = sensed(&f, &fly_a());
    assert_eq!(boundaries(&a), (0..=steps).collect::<Vec<_>>());
    f.shutdown().await;
}

/// A renderer keeps its extracted handle after the message is gone, and after the boundary it
/// came from has been replaced everywhere else.
async fn delayed_rendering_retains_its_handle_after_the_message_drops(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let observer = f.harness.observer().await.unwrap();
    let topic = f.harness.coordinator.topics().snapshots.clone();
    let mut spectator = Spectator::attach(&observer, &topic, 2).await.unwrap();
    within("step", f.harness.coordinator.run(1)).await.unwrap();

    // Take the message, keep only the frame, and drop the message itself.
    let message = within("snapshot", spectator.next_message()).await.expect("a snapshot");
    let (view, artifact) = detach_frame(message).expect("a frame in the snapshot");

    // Everything else moves on: the coordinator drops that boundary's handles, and the topic's
    // retained value is replaced twice.
    within("more steps", f.harness.coordinator.run(2)).await.unwrap();
    let handles = f.harness.coordinator.media_handles();
    assert!(
        !handles.iter().any(|(_, r)| r.artifact_id == view.pixels.artifact_id),
        "the session no longer holds the frame the renderer is still using"
    );

    // The rendering finishes now, long after its message is gone.
    let bytes = artifact.read_all().await.expect("the guard kept the bytes alive");
    assert_eq!(bytes.len() as u64, VIEW_WIDTH * VIEW_HEIGHT * 4);
    let entries = sensed(&f, &fly_a());
    let at_boundary = entries
        .iter()
        .find(|e| e.produced_step == view.produced_step)
        .expect("the agents encoded this frame too");
    assert_eq!(
        digest_of_bytes(&bytes),
        at_boundary.digest,
        "the retained handle still reads exactly the frame that was published"
    );
    f.shutdown().await;
}

/// A declared render delay repeats `O[0]` while the pipeline fills, then advances one frame
/// per boundary. The repetition is the same artifact, not a re-render.
async fn a_declared_render_delay_repeats_o0_until_the_pipeline_fills(via: Via) {
    let config = HarnessConfig {
        observation_delay_steps: 2,
        ..HarnessConfig::default()
    };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(4)).await.unwrap();

    let a = sensed(&f, &fly_a());
    assert_eq!(boundaries(&a), vec![0, 1, 2, 3, 4]);
    assert_eq!(
        produced(&a),
        vec![0, 0, 0, 1, 2],
        "max(0, boundary - 2) at every boundary"
    );
    assert_eq!(a[0].artifact_id, a[1].artifact_id);
    assert_eq!(a[0].artifact_id, a[2].artifact_id, "O[0] repeats while the delay fills");
    assert_ne!(a[2].artifact_id, a[3].artifact_id, "then the pipeline advances");
    assert_ne!(a[3].artifact_id, a[4].artifact_id);
    // The world still renders once per boundary; the delay is a queue, not a missing frame.
    assert_eq!(renders(&f), 5);
    f.shutdown().await;
}

/// Beyond the declared delay, an extra-delayed sensory view is a step failure, never an
/// arbitrary latest frame.
async fn an_extra_delayed_sensory_view_fails_the_step(via: Via) {
    let config = HarnessConfig {
        environment_faults: EnvironmentFaults {
            stale_view_at_boundary: Some(2),
            ..EnvironmentFaults::default()
        },
        ..HarnessConfig::default()
    };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let failure = within("run", f.harness.coordinator.run(STEPS))
        .await
        .expect_err("a stale frame fails the transition");
    assert_eq!(failure.error.code, ErrorCode::BufferInvalid);
    assert_eq!(failure.error.mutation, MutationCertainty::Unknown);
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert_eq!(
        f.harness.coordinator.stats().advances,
        1,
        "the transition that met the stale frame committed nothing"
    );
    // The agents did not encode the stale frame.
    let a = sensed(&f, &fly_a());
    assert_eq!(boundaries(&a), vec![0, 1]);
    f.shutdown().await;
}

/// A frame whose artifact is not `rowStride x height` bytes fails the step.
async fn a_frame_of_the_wrong_length_fails_the_step(via: Via) {
    let config = HarnessConfig {
        environment_faults: EnvironmentFaults {
            truncated_view_at_boundary: Some(1),
            ..EnvironmentFaults::default()
        },
        ..HarnessConfig::default()
    };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let failure = within("run", f.harness.coordinator.run(1))
        .await
        .expect_err("a short frame fails the transition");
    assert_eq!(failure.error.code, ErrorCode::BufferInvalid);
    assert_eq!(f.harness.coordinator.phase(), Phase::Failed);
    assert_eq!(f.harness.coordinator.stats().advances, 0);
    f.shutdown().await;
}

/// Within an epoch, an audio chunk that starts inside the previous one is refused.
async fn overlapping_audio_fails_the_step(via: Via) {
    let config = HarnessConfig {
        environment_faults: EnvironmentFaults {
            overlapping_audio_at_boundary: Some(2),
            ..EnvironmentFaults::default()
        },
        ..HarnessConfig::default()
    };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let failure = within("run", f.harness.coordinator.run(STEPS))
        .await
        .expect_err("an overlapping chunk fails the transition");
    assert_eq!(failure.error.code, ErrorCode::BufferInvalid);
    assert_eq!(f.harness.coordinator.stats().advances, 1);
    f.shutdown().await;
}

/// One chunk per boundary, with an exact sample budget, finite samples and the byte length the
/// descriptor implies. Audio is published for presentation and never enters sensory input.
async fn one_audio_chunk_per_boundary_with_an_exact_sample_budget(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let observer = f.harness.observer().await.unwrap();
    let topic = f.harness.coordinator.topics().snapshots.clone();
    let mut spectator = Spectator::attach(&observer, &topic, 2).await.unwrap();
    within("run", f.harness.coordinator.run(STEPS)).await.unwrap();

    // 48 kHz at 60 Hz is exactly 800 frames a step, and the positions are contiguous.
    let per_step = SAMPLE_RATE / f.harness.config.step_hz;
    let positions = f.harness.coordinator.audio_positions();
    assert_eq!(positions[AUDIO_STREAM_ID], per_step * STEPS);

    let observation = f.harness.coordinator.observation().expect("an observation").clone();
    assert_eq!(observation.audio.len(), 1, "one chunk per boundary");
    let chunk = &observation.audio[0];
    assert_eq!(chunk.sample_frames, per_step);
    assert_eq!(chunk.first_sample, per_step * (STEPS - 1));
    assert!(!chunk.discontinuity, "an uninterrupted epoch has no discontinuity");
    assert_eq!(chunk.samples.byte_length, per_step * CHANNELS * 4);
    // Sensory input is pixels only: audio never becomes an agent's input.
    assert!(observation.sensory_views.iter().all(|v| v.view_id == "arena"));

    let published = frame_at(&mut spectator, STEPS).await;
    let (published_chunk, artifact) = published.audio.first().expect("the published chunk");
    assert_eq!(published_chunk.samples, chunk.samples);
    assert_eq!(
        artifact.reference().artifact_id,
        chunk.samples.artifact_id,
        "the same owned handle is published, not a copy"
    );
    let bytes = artifact.read_all().await.expect("the published chunk");
    assert_eq!(bytes.len() as u64, per_step * CHANNELS * 4);
    require_finite_samples(&bytes).expect("native samples are finite f32");
    assert_eq!(
        published.audio.len(),
        1,
        "the attachment list names every published chunk"
    );
    assert_eq!(audio_attachment(AUDIO_STREAM_ID), "audio.arena");
    f.shutdown().await;
}

/// A declared stream that produces no chunk for a transition is a step failure, not a silently
/// shorter epoch.
async fn a_missing_audio_chunk_fails_the_step(via: Via) {
    let config = HarnessConfig {
        environment_faults: EnvironmentFaults {
            omit_audio_at_boundary: Some(2),
            ..EnvironmentFaults::default()
        },
        ..HarnessConfig::default()
    };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let failure = within("run", f.harness.coordinator.run(STEPS))
        .await
        .expect_err("a missing chunk fails the transition");
    assert_eq!(failure.error.code, ErrorCode::BufferInvalid);
    assert_eq!(f.harness.coordinator.stats().advances, 1);
    // Boundary 0 has no preceding interval, so its empty audio list is not a missing chunk.
    let mut clean = default_fixture(via).await;
    within("bootstrap", clean.harness.coordinator.bootstrap()).await.unwrap();
    assert!(clean.harness.coordinator.observation().unwrap().audio.is_empty());
    clean.shutdown().await;
    f.shutdown().await;
}

/// Restored timelines resume every declared stream at its recorded position. A stream with no
/// recorded position is refused, not restarted at zero.
async fn restored_timelines_need_every_declared_streams_position(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    within("run", f.harness.coordinator.run(1)).await.unwrap();
    let descriptor = f.harness.coordinator.descriptor().expect("a descriptor").clone();
    let observation = f.harness.coordinator.observation().expect("an observation").clone();

    // A fresh epoch accepts the transition's chunk and advances its position.
    let mut fresh = AudioTimelines::fresh(&descriptor);
    fresh
        .accept(&descriptor, &observation)
        .expect("the first chunk of a fresh epoch");
    assert_eq!(fresh.accepted(AUDIO_STREAM_ID), 1);
    let chunk = observation.audio.first().expect("a chunk").clone();
    assert_eq!(
        fresh.positions()[AUDIO_STREAM_ID],
        chunk.first_sample + chunk.sample_frames
    );

    // A restore with nothing recorded for a declared stream is an error, not sample zero.
    let missing = AudioTimelines::restored(&descriptor, &BTreeMap::new())
        .expect_err("a declared stream needs its preserved position");
    assert_eq!(missing.code, ErrorCode::IncompatibleState);

    // With the position recorded, the restored epoch resumes there and its first chunk must
    // mark the discontinuity.
    let positions = f.harness.coordinator.audio_positions();
    let mut resumed = observation.clone();
    let resumed_chunk = resumed.audio.first_mut().expect("a chunk");
    resumed_chunk.first_sample = positions[AUDIO_STREAM_ID];
    resumed_chunk.discontinuity = false;
    let mut restored = AudioTimelines::restored(&descriptor, &positions).expect("positions");
    restored
        .accept(&descriptor, &resumed)
        .expect_err("the first chunk after a restore marks discontinuity");
    resumed.audio.first_mut().expect("a chunk").discontinuity = true;
    let mut restored = AudioTimelines::restored(&descriptor, &positions).expect("positions");
    restored
        .accept(&descriptor, &resumed)
        .expect("the restored epoch resumes at the preserved position");
    assert_eq!(restored.accepted(AUDIO_STREAM_ID), 1);
    f.shutdown().await;
}

/// A world cadence that does not divide the sample rate still produces whole samples, with the
/// remainder carried rather than rounded: seven steps of a 7 Hz world are exactly one second.
async fn a_cadence_that_does_not_divide_the_sample_rate_still_lands_on_whole_samples(via: Via) {
    let config = HarnessConfig {
        step_hz: 7,
        ..HarnessConfig::default()
    };
    let mut f = fixture(via, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let observer = f.harness.observer().await.unwrap();
    let topic = f.harness.coordinator.topics().snapshots.clone();
    let mut spectator = Spectator::attach(&observer, &topic, 2).await.unwrap();

    // 48000 / 7 is 6857.14..., so no chunk can be the exact share of a second.
    let mut frames = Vec::new();
    for step in 1..=7 {
        within("step", f.harness.coordinator.run(1)).await.unwrap();
        let chunk = f
            .harness
            .coordinator
            .observation()
            .expect("an observation")
            .audio
            .first()
            .expect("a chunk")
            .clone();
        assert_eq!(chunk.first_sample, frames.iter().sum::<u64>());
        assert!(
            chunk.sample_frames == 6_857 || chunk.sample_frames == 6_858,
            "step {step} produced {} frames",
            chunk.sample_frames
        );
        assert_eq!(chunk.samples.byte_length, chunk.sample_frames * CHANNELS * 4);
        frames.push(chunk.sample_frames);
    }
    assert_eq!(
        frames.iter().sum::<u64>(),
        SAMPLE_RATE,
        "seven steps of a 7 Hz world are exactly one second of samples: {frames:?}"
    );
    assert_eq!(f.harness.coordinator.audio_positions()[AUDIO_STREAM_ID], SAMPLE_RATE);

    // The published chunk is readable and finite whatever the cadence.
    let published = frame_at(&mut spectator, 7).await;
    let (chunk, artifact) = published.audio.first().expect("the published chunk");
    let bytes = artifact.read_all().await.expect("the published chunk");
    assert_eq!(bytes.len() as u64, chunk.sample_frames * CHANNELS * 4);
    require_finite_samples(&bytes).expect("native samples are finite f32");
    f.shutdown().await;
}

/// The media path itself is mode-agnostic: one native image per boundary, forwarded to every
/// agent as an owned attachment and published once for presentation, whether the participants
/// are tasks on one runtime, threads with their own runtimes, or separate processes.
///
/// What a *test* can see differs by mode, and this test only asserts what crosses a process
/// boundary. An agent validates that each attachment is the artifact its payload names and
/// reads the pixels before it commits, so a session that commits every boundary in process
/// mode has carried one shared image across that boundary through the store, not through
/// shared memory. The sensor log and the render counter are shared memory, so they are
/// asserted where they exist and their absence is asserted where they do not.
async fn the_media_path_works_in_every_execution_mode(mode: ExecutionMode) {
    // A declared render delay as well, so the option reaches a world in another process.
    let config = HarnessConfig {
        observation_delay_steps: 1,
        ..HarnessConfig::default()
    };
    let mut f = mode_fixture(mode, config).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let observer = f.harness.observer().await.unwrap();
    let topic = f.harness.coordinator.topics().snapshots.clone();
    let mut spectator = Spectator::attach(&observer, &topic, 2).await.unwrap();
    within("run", f.harness.coordinator.run(STEPS)).await.unwrap();

    // Every agent read its attachment and committed, in every mode.
    assert_eq!(f.harness.coordinator.committed_boundary(), Some(STEPS));

    // The coordinator holds one view handle and one audio handle for this boundary, and the
    // observation names exactly those artifacts.
    let observation = f.harness.coordinator.observation().expect("an observation").clone();
    let handles = f.harness.coordinator.media_handles();
    assert_eq!(handles.len(), 2, "one view and one chunk: {handles:?}");
    let view = observation.sensory_views.first().expect("a required view");
    assert_eq!(
        view.produced_step,
        STEPS - 1,
        "the declared one-step delay reached the world in {mode:?} mode"
    );
    assert!(handles.iter().any(|(name, r)| *name == view_attachment(&view.view_id)
        && r.artifact_id == view.pixels.artifact_id));

    // The same object is what presentation was published, and its bytes read back at the
    // declared shape through an ordinary subscription.
    let published = frame_at(&mut spectator, STEPS).await;
    assert_eq!(published.view.pixels.artifact_id, view.pixels.artifact_id);
    assert_eq!(published.view.produced_step, view.produced_step);
    let pixels = published.artifact.read_all().await.expect("the published frame");
    assert_eq!(pixels.len() as u64, VIEW_WIDTH * VIEW_HEIGHT * 4);
    let (chunk, audio) = published.audio.first().expect("the published chunk");
    let samples = audio.read_all().await.expect("the published chunk");
    assert_eq!(samples.len() as u64, chunk.sample_frames * CHANNELS * 4);
    require_finite_samples(&samples).expect("native samples are finite f32");

    // The shared-memory instrumentation exists exactly where the participants do.
    match (mode, f.harness.sensor_log(&fly_a()), f.harness.renders()) {
        (ExecutionMode::Process, sensors, renders) => {
            assert!(sensors.is_none() && renders.is_none(), "not observable from here");
        }
        (_, Some(sensors), Some(renders)) => {
            let a = sensors.entries();
            let b = f.harness.sensor_log(&fly_b()).expect("in this process").entries();
            assert_eq!(a, b, "both agents read the same artifact and the same bytes");
            assert_eq!(boundaries(&a), (0..=STEPS).collect::<Vec<_>>());
            assert_eq!(
                digest_of_bytes(&pixels),
                a.last().expect("an entry per boundary").digest,
                "the published bytes are the bytes the agents encoded"
            );
            assert_eq!(renders, STEPS + 1, "one render per boundary");
        }
        (mode, _, _) => panic!("{mode:?} keeps its participants in this process"),
    }
    f.shutdown().await;
}

/// The retention table: a required agent input is retained through encoding and Commit with no
/// coalescing, while a spectator's snapshots are a latest subscription with finite credits.
async fn required_agent_input_is_never_coalesced_while_spectator_snapshots_are(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let observer = f.harness.observer().await.unwrap();
    let topic = f.harness.coordinator.topics().snapshots.clone();
    let mut spectator = Spectator::attach(&observer, &topic, 1).await.unwrap();
    assert!(within("first snapshot", spectator.hold_one()).await);

    let steps = 5;
    within("run", f.harness.coordinator.run(steps)).await.unwrap();

    // The spectator's queue coalesced while it was not reading.
    spectator.release();
    within("after release", spectator.hold_one()).await;
    assert!(spectator.coalesced() > 0);
    assert!(
        spectator.seen() < steps + 1,
        "a latest subscription does not deliver every boundary to a slow reader"
    );

    // Every agent's required input arrived once per boundary, in order, with nothing dropped
    // or replaced.
    for agent in [fly_a(), fly_b()] {
        let entries = sensed(&f, &agent);
        assert_eq!(
            boundaries(&entries),
            (0..=steps).collect::<Vec<_>>(),
            "{agent} encoded every boundary exactly once"
        );
    }
    f.shutdown().await;
}

/// A persistent `AssetRef` names installed content; a transient `ArtifactRef` names live bytes
/// in one store. Importing the asset makes a new artifact, and collecting that artifact leaves
/// the asset installed.
async fn a_persistent_asset_and_a_transient_artifact_are_different_identities(via: Via) {
    let mut f = default_fixture(via).await;
    within("bootstrap", f.harness.coordinator.bootstrap()).await.unwrap();
    let client = f.harness.observer().await.unwrap();

    let body = "counter-arena-backend-v1";
    let asset = synthetic_asset("counter-arena-backend", body);
    let mut registry = AssetRegistry::new();
    registry
        .install(asset.clone(), body.as_bytes().to_vec())
        .expect("installed content matches its reference");
    // Installing content that is not what the reference claims is refused.
    let mut wrong = asset.clone();
    wrong.byte_length += 1;
    registry
        .install(wrong, body.as_bytes().to_vec())
        .expect_err("an asset reference is its content's identity");

    let before = f.harness.router().stats().sealed_artifacts;
    let artifact = registry.import(&client, &asset).await.expect("the import");
    assert_eq!(
        f.harness.router().stats().sealed_artifacts,
        before + 1,
        "the import is a new object in the store"
    );

    // The identities are different, and the content is the same.
    assert_ne!(artifact.reference().artifact_id, asset.id);
    assert_eq!(artifact.reference().byte_length, asset.byte_length);
    assert_eq!(artifact.reference().digest.as_deref(), Some(asset.digest.as_str()));
    check_imported_asset(&asset, artifact.reference()).expect("the import carries the content");
    assert_eq!(
        artifact.read_all().await.expect("readable"),
        body.as_bytes(),
        "the imported artifact is the installed bytes"
    );

    // The transient artifact is collected with its last handle; the asset is still installed.
    drop(artifact);
    let router = f.harness.router().clone();
    until("the imported artifact is collected", || {
        router.stats().sealed_artifacts == before
    })
    .await;
    assert!(registry.contains(&asset));
    assert_eq!(
        registry.resolve(&asset).expect("still installed"),
        body.as_bytes(),
        "a persistent asset outlives the bus objects imported from it"
    );
    f.shutdown().await;
}

/// A restored epoch resumes the preserved sample position, and its first chunk marks the
/// discontinuity that says so. The producer and the validator agree about which epoch it is.
async fn a_restored_audio_source_resumes_and_marks_the_discontinuity(via: Via) {
    let f = default_fixture(via).await;
    let client = f.harness.observer().await.unwrap();
    let descriptor = fly_session::environment::CounterEnvironment::audio_descriptor();
    let step = hz(f.harness.config.step_hz).expect("a positive cadence");
    let resumed_at = 2_400;

    let mut source = AudioSource::restored_at(descriptor.clone(), resumed_at);
    let (first, _first_handle) = source
        .produce(&client, &step, 3)
        .await
        .expect("the restored source produces");
    assert_eq!(first.first_sample, resumed_at, "the sample position is preserved");
    assert!(first.discontinuity, "the first chunk after a restore marks it");
    let (second, _second_handle) = source
        .produce(&client, &step, 3)
        .await
        .expect("the next chunk");
    assert!(!second.discontinuity, "only the first chunk of the epoch marks it");
    assert_eq!(second.first_sample, resumed_at + first.sample_frames);

    // The validator accepts exactly this sequence under a restored timeline, and refuses it
    // under a fresh one: the flag is what distinguishes the two epochs.
    let mut restored = AudioTimeline::restored_at(&descriptor, resumed_at);
    restored.accept(&first, &descriptor).expect("the restored epoch");
    restored.accept(&second, &descriptor).expect("and its next chunk");
    // A fresh episode at the audio origin refuses it: a restore preserves the sample
    // position, and the position is what distinguishes the two epochs. The flag is required
    // after a restore and free at an origin, so it cannot carry that distinction by itself.
    let mut fresh = AudioTimeline::fresh(&descriptor, 0);
    fresh
        .accept(&first, &descriptor)
        .expect_err("a fresh episode starts at its own origin, not a resumed position");
    f.shutdown().await;
}

/// The sample budget is exact when the cadence does not divide the sample rate: 8 kHz at 60 Hz
/// is 133, 133, 134 and the total after three steps is exactly 400.
#[test]
fn the_sample_budget_is_exact_when_the_cadence_does_not_divide_the_rate() {
    let descriptor = fly_session_types::media::AudioDescriptor {
        stream_id: "arena".into(),
        sample_rate: 8_000,
        channels: 1,
    };
    let mut source = AudioSource::new(descriptor, 0);
    let step = hz(60).expect("a positive cadence");
    let frames: Vec<u64> = (0..6)
        .map(|_| source.frames_for_step(&step).expect("an exact budget"))
        .collect();
    assert_eq!(frames, vec![133, 133, 134, 133, 133, 134]);
    assert_eq!(frames.iter().sum::<u64>(), 800, "8000 samples in a tenth of a second");

    // 48 kHz at 60 Hz divides exactly.
    let exact = fly_session_types::media::AudioDescriptor {
        stream_id: "arena".into(),
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
    };
    let mut source = AudioSource::new(exact, 0);
    assert_eq!(source.frames_for_step(&step).unwrap(), 800);
}

/// The native frame is a real pattern with no padded rows: `rowStride` is `4 x width`, the
/// rows are top-left first, and consecutive boundaries differ.
#[test]
fn the_native_frame_is_top_left_rgba8_with_no_padding() {
    let descriptor = fly_session::environment::CounterEnvironment::view_descriptor(0);
    let frame = arena_frame(&descriptor, 5, 3);
    assert_eq!(frame.len() as u64, descriptor.row_stride * descriptor.height);
    assert_eq!(descriptor.row_stride, descriptor.width * 4);
    // Every pixel is opaque and carries the world counter in its red channel.
    for pixel in frame.chunks_exact(4) {
        assert_eq!(pixel[0], 5);
        assert_eq!(pixel[3], 255);
    }
    assert_ne!(
        arena_frame(&descriptor, 5, 3),
        arena_frame(&descriptor, 5, 4),
        "consecutive boundaries are different images"
    );
    assert_ne!(
        arena_frame(&descriptor, 5, 3),
        arena_frame(&descriptor, 6, 3),
        "the counter changes the image"
    );
}
