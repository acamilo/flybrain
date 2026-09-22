//! Native observations and the presentation handoff: the MEDIA-01 slice.
//!
//! Everything here sits on top of the bus `ArtifactRef` and its ownership rules. There is no
//! second buffer system: an environment allocates, writes and seals one immutable object per
//! boundary, the coordinator forwards that one owned handle to every agent and to publication,
//! and a spectator reads it through an ordinary latest subscription.
//!
//! The module owns four things:
//!
//! 1. Production. [`ViewPipeline`] renders one native frame per boundary and hands out the
//!    frame a declared `observationDelaySteps` requires, so a pipeline delay is a real queue
//!    rather than a number in a descriptor. [`AudioSource`] produces one chunk per boundary
//!    with an exact rational sample budget.
//! 2. Acceptance. [`check_required_views`] and [`AudioTimelines`] are the coordinator's Phase C
//!    media checks: a required sensory view must exist at exactly the producing boundary its
//!    declared delay implies, and audio chunks cannot overlap or go backwards inside an epoch.
//! 3. Consumption. [`Spectator`] is a presentation-side consumer on a latest subscription with
//!    finite credits, and [`detach_frame`] is the renderer that keeps its handle after the
//!    message is gone.
//! 4. Identity. [`AssetRegistry`] holds installed persistent content named by `AssetRef`.
//!    Importing an asset produces a *new* transient artifact; the two identities never convert.
//!
//! Resizing, overlays, compositing, mixing, encoding and streaming are not here and are not
//! anywhere else in this crate: they belong to the application's presentation layer.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fly_session_types::media::AudioTimeline;

// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the glob
// keeps the contract's own names in sight instead of restating them.
use crate::types::*;

/// The content type of a native RGBA8 frame. Top-left origin, no padded rows.
pub const FRAME_CONTENT_TYPE: &str = "image/x-rgba8";

/// The content type of a native audio chunk: interleaved little-endian f32.
pub const AUDIO_CONTENT_TYPE: &str = "audio/x-f32le";

/// The attachment name one view's pixels travel under.
pub fn view_attachment(view_id: &str) -> String {
    format!("view.{view_id}")
}

/// The attachment name one audio stream's samples travel under.
pub fn audio_attachment(stream_id: &str) -> String {
    format!("audio.{stream_id}")
}

fn store_error(what: &str, message: &str) -> DomainError {
    DomainError::new(
        ErrorCode::BackendFailure,
        format!("{what}: {message}"),
        MutationCertainty::Applied,
    )
}

fn media_error(message: impl std::fmt::Display) -> DomainError {
    // A world that advanced without usable media leaves the transition's certainty unknown:
    // the mutation happened, the observation of it did not.
    DomainError::new(ErrorCode::BufferInvalid, message, MutationCertainty::Unknown)
}

// ----------------------------------------------------------------------------------------------
// Production

/// How many frames a producer has actually rendered.
///
/// A shared counter, so a test can prove that forwarding one image to several recipients
/// renders it once.
#[derive(Clone, Debug, Default)]
pub struct RenderCounter(Arc<AtomicU64>);

impl RenderCounter {
    pub fn new() -> RenderCounter {
        RenderCounter::default()
    }

    pub fn bump(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    pub fn count(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// One native frame of the counter arena: a real synthetic pattern, not a constant fill.
///
/// Top-left RGBA8 with `rowStride` exactly `4 x width` and no padded rows, which is the only
/// pixel layout v1 has. The red channel carries the world counter, so a reader that samples
/// one pixel still reads the world; green is a horizontal ramp and blue a vertical ramp with
/// a one-column bar that walks with the boundary, so consecutive frames differ.
pub fn arena_frame(descriptor: &ViewDescriptor, counter: i64, boundary: u64) -> Vec<u8> {
    let width = descriptor.width;
    let height = descriptor.height;
    let stride = descriptor.row_stride as usize;
    let mut out = vec![0u8; stride * height as usize];
    let counter_byte = (counter & 0xff) as u8;
    let bar = boundary % width;
    for y in 0..height {
        let row = y as usize * stride;
        for x in 0..width {
            let p = row + x as usize * 4;
            let ramp_x = if width > 1 {
                (x * 255 / (width - 1)) as u8
            } else {
                0
            };
            let ramp_y = if height > 1 {
                (y * 255 / (height - 1)) as u8
            } else {
                0
            };
            out[p] = counter_byte;
            out[p + 1] = ramp_x;
            out[p + 2] = if x == bar { 255 } else { ramp_y };
            out[p + 3] = 255;
        }
    }
    out
}

/// One view's production pipeline: render at every boundary, deliver with the declared delay.
///
/// `observationDelaySteps` is a real queue here. At boundary `b` the required frame is the one
/// produced at `max(0, b - delay)`, so a declared delay of two repeats `O[0]` at boundaries 0,
/// 1 and 2 -- the bootstrap repetition the contract allows -- and then advances one frame per
/// boundary. Nothing beyond that is retained: an older frame is dropped, so a later boundary
/// cannot be served an arbitrary stale image.
pub struct ViewPipeline {
    descriptor: ViewDescriptor,
    frames: VecDeque<(u64, flybus::Artifact)>,
    renders: RenderCounter,
}

impl ViewPipeline {
    pub fn new(descriptor: ViewDescriptor, renders: RenderCounter) -> ViewPipeline {
        ViewPipeline {
            descriptor,
            frames: VecDeque::new(),
            renders,
        }
    }

    pub fn descriptor(&self) -> &ViewDescriptor {
        &self.descriptor
    }

    /// Seals one immutable frame for `boundary` and files it under its producing boundary.
    pub async fn render(
        &mut self,
        client: &flybus::Client,
        boundary: u64,
        counter: i64,
    ) -> DomainResult<()> {
        let bytes = arena_frame(&self.descriptor, counter, boundary);
        let artifact = seal(client, FRAME_CONTENT_TYPE, &bytes).await?;
        self.renders.bump();
        self.frames.push_back((boundary, artifact));
        // Keep exactly the frames a declared delay can still require.
        while self.frames.len() > self.descriptor.observation_delay_steps as usize + 1 {
            self.frames.pop_front();
        }
        Ok(())
    }

    /// Seals a frame of the wrong length, which is what a broken backend produces. The
    /// reference it returns describes the artifact honestly, so the shape check is the thing
    /// under test rather than a lie in the payload.
    pub async fn render_truncated(
        &mut self,
        client: &flybus::Client,
        boundary: u64,
        counter: i64,
    ) -> DomainResult<()> {
        let mut bytes = arena_frame(&self.descriptor, counter, boundary);
        bytes.truncate(bytes.len() - self.descriptor.row_stride as usize);
        let artifact = seal(client, FRAME_CONTENT_TYPE, &bytes).await?;
        self.renders.bump();
        self.frames.push_back((boundary, artifact));
        while self.frames.len() > self.descriptor.observation_delay_steps as usize + 2 {
            self.frames.pop_front();
        }
        Ok(())
    }

    /// The view reference and the owned handle a required sensory view has at `boundary`.
    pub fn at(&self, boundary: u64) -> Option<(ViewRef, flybus::Artifact)> {
        let produced = self.descriptor.required_produced_step(boundary);
        self.frame_produced_at(produced)
    }

    /// The frame produced at exactly `produced`, if it is still retained.
    pub fn frame_produced_at(&self, produced: u64) -> Option<(ViewRef, flybus::Artifact)> {
        self.frames
            .iter()
            .find(|(step, _)| *step == produced)
            .map(|(step, artifact)| {
                (
                    ViewRef {
                        view_id: self.descriptor.view_id.clone(),
                        produced_step: *step,
                        pixels: artifact.reference().clone(),
                    },
                    artifact.clone(),
                )
            })
    }

    /// How many boundaries this pipeline has rendered.
    pub fn renders(&self) -> u64 {
        self.renders.count()
    }

}

/// One audio stream's production: an exact sample budget and a deterministic waveform.
///
/// The number of frames in a step is `sampleRate x stepDuration`, accumulated as a rational so
/// a cadence that does not divide the sample rate never drifts: 8 kHz at 60 Hz produces
/// 133, 133, 134, ... and the sum is exact at every boundary. The waveform is integer-phase
/// arithmetic only, so a `fixed-build` environment produces the same bytes on every run.
pub struct AudioSource {
    descriptor: AudioDescriptor,
    /// The unconsumed fraction of a frame, over `denominator`.
    accumulator: u128,
    denominator: u128,
    next_sample: u64,
    phase: u64,
    chunks: u64,
    discontinuous: bool,
}

impl AudioSource {
    /// A fresh episode, whose first chunk starts at the configured audio origin.
    pub fn new(descriptor: AudioDescriptor, origin: u64) -> AudioSource {
        AudioSource {
            descriptor,
            accumulator: 0,
            denominator: 1,
            next_sample: origin,
            phase: 0,
            chunks: 0,
            discontinuous: false,
        }
    }

    /// A new epoch after a restore: the sample position is preserved and the first chunk of
    /// this epoch marks a discontinuity.
    pub fn restored_at(descriptor: AudioDescriptor, sample: u64) -> AudioSource {
        let mut source = AudioSource::new(descriptor, sample);
        source.discontinuous = true;
        source
    }

    pub fn descriptor(&self) -> &AudioDescriptor {
        &self.descriptor
    }

    pub fn next_sample(&self) -> u64 {
        self.next_sample
    }

    pub fn chunks(&self) -> u64 {
        self.chunks
    }

    /// The exact number of sample frames one step of `step` nanoseconds contains.
    ///
    /// The remainder is kept, never rounded: the accumulator is integer arithmetic over the
    /// common denominator `stepDenominator x 1e9`.
    pub fn frames_for_step(&mut self, step: &RationalNs) -> DomainResult<u64> {
        let denominator = u128::from(step.denominator)
            .checked_mul(1_000_000_000)
            .ok_or_else(|| DomainError::invalid("audio: the step denominator overflows"))?;
        if self.denominator != denominator {
            // A cadence change would need a new epoch; carrying a remainder across one would
            // be a silent resample.
            if self.chunks > 0 {
                return Err(DomainError::invalid(
                    "audio: the cadence changed inside an epoch",
                ));
            }
            self.denominator = denominator;
        }
        let per_step = u128::from(self.descriptor.sample_rate)
            .checked_mul(u128::from(step.numerator))
            .ok_or_else(|| DomainError::invalid("audio: the sample budget overflows"))?;
        self.accumulator = self
            .accumulator
            .checked_add(per_step)
            .ok_or_else(|| DomainError::invalid("audio: the sample accumulator overflows"))?;
        let frames = self.accumulator / self.denominator;
        self.accumulator %= self.denominator;
        u64::try_from(frames).map_err(|_| DomainError::invalid("audio: too many frames in a step"))
    }

    /// Produces one chunk covering exactly one step of world time.
    pub async fn produce(
        &mut self,
        client: &flybus::Client,
        step: &RationalNs,
        counter: i64,
    ) -> DomainResult<(AudioRef, flybus::Artifact)> {
        let frames = self.frames_for_step(step)?;
        let bytes = self.samples(frames, counter);
        let artifact = seal(client, AUDIO_CONTENT_TYPE, &bytes).await?;
        let chunk = AudioRef {
            stream_id: self.descriptor.stream_id.clone(),
            first_sample: self.next_sample,
            sample_frames: frames,
            samples: artifact.reference().clone(),
            discontinuity: self.discontinuous && self.chunks == 0,
        };
        self.next_sample = self
            .next_sample
            .checked_add(frames)
            .ok_or_else(|| DomainError::invalid("audio: the sample position overflows"))?;
        self.chunks += 1;
        Ok((chunk, artifact))
    }

    /// A deterministic triangle wave whose pitch follows the world counter, interleaved across
    /// the declared channels. Every sample is finite by construction.
    fn samples(&mut self, frames: u64, counter: i64) -> Vec<u8> {
        let rate = self.descriptor.sample_rate;
        let channels = self.descriptor.channels;
        let step = 220 + (counter.rem_euclid(8) as u64) * 55;
        let mut out = Vec::with_capacity((frames * channels * 4) as usize);
        for _ in 0..frames {
            self.phase = (self.phase + step) % rate;
            let position = self.phase as f32 / rate as f32;
            // 1 - 2|2p - 1| is a triangle in [-1, 1] built from exact IEEE operations.
            let value = 1.0 - 2.0 * (2.0 * position - 1.0).abs();
            for channel in 0..channels {
                let scaled = value * 0.25 / (channel + 1) as f32;
                out.extend_from_slice(&scaled.to_le_bytes());
            }
        }
        out
    }
}

/// Allocates, writes and seals one immutable artifact of an arbitrary content type.
///
/// Used where a test needs a second object with the same bytes, so that "the handle is not
/// the artifact the payload names" can be produced without corrupting the bytes.
pub async fn seal_copy(
    client: &flybus::Client,
    content_type: String,
    bytes: &[u8],
) -> DomainResult<flybus::Artifact> {
    seal(client, &content_type, bytes).await
}

/// Allocates, writes and seals one immutable artifact.
async fn seal(
    client: &flybus::Client,
    content_type: &str,
    bytes: &[u8],
) -> DomainResult<flybus::Artifact> {
    let mut writer = client
        .artifacts()
        .allocate(bytes.len() as u64, content_type)
        .await
        .map_err(|e| store_error("allocate", &e.message))?;
    writer
        .write_all(bytes)
        .map_err(|e| store_error("write", &e.to_string()))?;
    writer
        .seal()
        .await
        .map_err(|e| store_error("seal", &e.message))
}

/// Splits one reply's attachments into the view handles and the audio handles.
///
/// Views are sensory data forwarded to agents; audio is presentation data that is published
/// and never attached to a sensory input.
pub fn split_attachments(
    artifacts: BTreeMap<String, flybus::Artifact>,
) -> (
    BTreeMap<String, flybus::Artifact>,
    BTreeMap<String, flybus::Artifact>,
) {
    let mut views = BTreeMap::new();
    let mut audio = BTreeMap::new();
    for (name, artifact) in artifacts {
        if name.starts_with("audio.") {
            audio.insert(name, artifact);
        } else {
            views.insert(name, artifact);
        }
    }
    (views, audio)
}

/// The attachment names one environment's declared media travel under.
pub fn attachment_names(descriptor: &EnvironmentDescriptor) -> Vec<String> {
    descriptor
        .views
        .iter()
        .map(|view| view_attachment(&view.view_id))
        .chain(
            descriptor
                .audio
                .iter()
                .map(|stream| audio_attachment(&stream.stream_id)),
        )
        .collect()
}

// ----------------------------------------------------------------------------------------------
// Acceptance

/// Every declared view must be present at exactly the producing boundary its delay implies.
///
/// A missing spectator frame is tolerable; a missing required sensory input is not. Neither is
/// one that arrived from an older boundary than the declared delay allows: the transition
/// fails instead of the session substituting whatever frame it happens to hold.
pub fn check_required_views(
    descriptor: &EnvironmentDescriptor,
    observation: &WorldObservation,
) -> DomainResult<()> {
    for view in &descriptor.views {
        let want = required_produced_step(view, observation.boundary);
        match observation
            .sensory_views
            .iter()
            .find(|given| given.view_id == view.view_id)
        {
            Some(given) if given.produced_step == want => {}
            Some(given) => {
                return Err(media_error(format!(
                    "view {} came from boundary {}, and its declared delay of {} requires {want}",
                    view.view_id, given.produced_step, view.observation_delay_steps
                )));
            }
            None => {
                return Err(media_error(format!(
                    "required sensory view {} is missing",
                    view.view_id
                )));
            }
        }
    }
    Ok(())
}

/// Every declared audio stream produces exactly one chunk per transition.
///
/// The contract states the shape and the ordering of chunks, not whether one has to exist, so
/// this is MEDIA-01's choice and it is deliberate: a session that tolerates a silently missing
/// chunk cannot tell "this world produced no audio for this interval" from "the chunk was
/// lost", and the second is the case the retention rules care about. Boundary 0 has no
/// preceding interval and so carries no chunk.
pub fn check_required_audio(
    descriptor: &EnvironmentDescriptor,
    observation: &WorldObservation,
) -> DomainResult<()> {
    if observation.boundary == 0 {
        return Ok(());
    }
    for stream in &descriptor.audio {
        if !observation
            .audio
            .iter()
            .any(|chunk| chunk.stream_id == stream.stream_id)
        {
            return Err(media_error(format!(
                "declared audio stream {} produced no chunk for this transition",
                stream.stream_id
            )));
        }
    }
    Ok(())
}

/// Every declared audio stream's chunk sequence, one timeline per stream and epoch.
#[derive(Clone, Debug, Default)]
pub struct AudioTimelines(BTreeMap<String, AudioTimeline>);

impl AudioTimelines {
    /// Fresh timelines for a new episode: every declared stream starts at origin zero.
    pub fn fresh(descriptor: &EnvironmentDescriptor) -> AudioTimelines {
        AudioTimelines(
            descriptor
                .audio
                .iter()
                .map(|stream| (stream.stream_id.clone(), AudioTimeline::fresh(stream, 0)))
                .collect(),
        )
    }

    /// Timelines for a new epoch after a restore: each stream resumes at its preserved sample
    /// position, and each one's first chunk must mark a discontinuity.
    ///
    /// A declared stream with no recorded position is an error. Resuming it at sample zero
    /// would restart the episode's audio clock silently, which is exactly the best-effort
    /// policy the restore rules refuse: crash restore *preserves* the sample position.
    pub fn restored(
        descriptor: &EnvironmentDescriptor,
        positions: &BTreeMap<String, u64>,
    ) -> DomainResult<AudioTimelines> {
        let mut timelines = BTreeMap::new();
        for stream in &descriptor.audio {
            let at = positions.get(&stream.stream_id).copied().ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    format!(
                        "audio stream {} has no restored sample position",
                        stream.stream_id
                    ),
                )
            })?;
            timelines.insert(stream.stream_id.clone(), AudioTimeline::restored_at(stream, at));
        }
        Ok(AudioTimelines(timelines))
    }

    /// Accepts one observation's chunks. Unknown streams and out-of-sequence chunks fail.
    pub fn accept(
        &mut self,
        descriptor: &EnvironmentDescriptor,
        observation: &WorldObservation,
    ) -> DomainResult<()> {
        for chunk in &observation.audio {
            let declared = descriptor
                .audio_stream(&chunk.stream_id)
                .ok_or_else(|| media_error(format!("audio stream {} is not declared", chunk.stream_id)))?;
            let timeline = self
                .0
                .get_mut(&chunk.stream_id)
                .ok_or_else(|| media_error(format!("audio stream {} has no timeline", chunk.stream_id)))?;
            timeline.accept(chunk, declared).map_err(media_error)?;
        }
        Ok(())
    }

    /// Where each stream's next chunk may start.
    pub fn positions(&self) -> BTreeMap<String, u64> {
        self.0
            .iter()
            .map(|(id, timeline)| (id.clone(), timeline.next_sample()))
            .collect()
    }

    pub fn accepted(&self, stream_id: &str) -> u64 {
        self.0.get(stream_id).map_or(0, AudioTimeline::accepted)
    }
}

// ----------------------------------------------------------------------------------------------
// Consumption

/// What one agent actually sensed, recorded where a test can read it.
///
/// The fake agent is the only thing that reads the pixels, so this is how "one shared image
/// reached both agents" is proved from the agents' side rather than from the producer's.
#[derive(Clone, Debug, Default)]
pub struct SensorLog(Arc<std::sync::Mutex<Vec<SensedView>>>);

/// One view an agent read: which boundary it was consumed at, which artifact it was, and the
/// digest of the bytes the agent actually read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SensedView {
    pub boundary: u64,
    pub view_id: Id,
    pub artifact_id: String,
    pub produced_step: u64,
    pub digest: Digest,
}

impl SensorLog {
    pub fn new() -> SensorLog {
        SensorLog::default()
    }

    pub fn record(&self, view: SensedView) {
        self.0.lock().expect("the sensor log is never poisoned").push(view);
    }

    pub fn entries(&self) -> Vec<SensedView> {
        self.0.lock().expect("the sensor log is never poisoned").clone()
    }

    /// The artifacts this agent read, in order.
    pub fn artifact_ids(&self) -> Vec<String> {
        self.entries().into_iter().map(|v| v.artifact_id).collect()
    }
}

/// One frame a spectator took off its subscription.
pub struct SpectatorFrame {
    pub boundary: u64,
    /// Every agent the committed snapshot carries, in publication order. A presentation
    /// consumer is a multi-agent consumer: one snapshot holds the whole session.
    pub agents: Vec<Id>,
    pub sequence: u64,
    /// How many undelivered snapshots were coalesced into this one.
    pub replaced: u64,
    pub view: ViewRef,
    pub artifact: flybus::Artifact,
    /// Each published chunk with the handle it travelled on.
    pub audio: Vec<(AudioRef, flybus::Artifact)>,
}

/// A presentation-side consumer of committed snapshots.
///
/// It subscribes `latest` with finite credits, which is the spectator row of the domain
/// retention table: new snapshots replace its queued value, it never blocks the session, and
/// the only thing a slow one exhausts is its own credits.
pub struct Spectator {
    subscription: flybus::Subscription,
    held: Vec<flybus::Message>,
    seen: u64,
    coalesced: u64,
}

impl Spectator {
    /// Subscribes to `topic` in latest mode with `credits` in flight.
    ///
    /// A latest subscription always has exactly one queued value; `credits` is its in-flight
    /// bound, which the router limits (two by default). Finite credits are the spectator row
    /// of the domain retention table: they are the only thing a slow viewer exhausts.
    pub async fn attach(
        client: &flybus::Client,
        topic: &str,
        credits: u32,
    ) -> Result<Spectator, flybus::BusError> {
        let subscription = client
            .subscribe(
                topic,
                flybus::SubscriptionConfig::latest().in_flight(credits).replay(true),
            )
            .await?;
        Ok(Spectator {
            subscription,
            held: Vec::new(),
            seen: 0,
            coalesced: 0,
        })
    }

    /// Takes the next snapshot, reads its frame and releases the delivery.
    pub async fn take_frame(&mut self) -> Option<SpectatorFrame> {
        let message = self.subscription.next().await?;
        self.seen += 1;
        self.coalesced += message.replaced();
        let frame = snapshot_frame(&message);
        drop(message);
        frame
    }

    /// Takes the next snapshot message itself, for a renderer that keeps its own handle
    /// after the message is gone.
    pub async fn next_message(&mut self) -> Option<flybus::Message> {
        let message = self.subscription.next().await?;
        self.seen += 1;
        self.coalesced += message.replaced();
        Some(message)
    }

    /// Takes a snapshot without consuming it, which is what a viewer that stops rendering
    /// does. Its credits run out and nothing else in the session notices.
    pub async fn hold_one(&mut self) -> bool {
        match self.subscription.next().await {
            Some(message) => {
                self.seen += 1;
                self.coalesced += message.replaced();
                self.held.push(message);
                true
            }
            None => false,
        }
    }

    /// Takes a snapshot without consuming it if one is queued right now.
    pub fn try_hold_one(&mut self) -> bool {
        match self.subscription.try_next() {
            Some(message) => {
                self.seen += 1;
                self.coalesced += message.replaced();
                self.held.push(message);
                true
            }
            None => false,
        }
    }

    /// Releases everything this spectator was holding, returning its credits.
    pub fn release(&mut self) {
        self.held.clear();
    }

    pub fn held(&self) -> usize {
        self.held.len()
    }

    pub fn seen(&self) -> u64 {
        self.seen
    }

    /// How many snapshots were replaced in this spectator's queue while it was busy.
    pub fn coalesced(&self) -> u64 {
        self.coalesced
    }
}

/// Reads one committed snapshot's first view and its handle.
pub fn snapshot_frame(message: &flybus::Message) -> Option<SpectatorFrame> {
    let payload = message.payload();
    let boundary = payload
        .get("scope")
        .and_then(|s| s.get("step"))
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<u64>().ok())?;
    let media = payload.get("media")?;
    let views = media.get("views")?.as_array()?;
    let view = ViewRef::from_json(views.first()?).ok()?;
    // An unreadable chunk, or one whose handle is not attached, makes the whole snapshot
    // unreadable rather than a snapshot that quietly has less audio in it than was published.
    let mut audio = Vec::new();
    for value in media.get("audio")?.as_array()? {
        let chunk = AudioRef::from_json(value).ok()?;
        let artifact = message.artifact(&audio_attachment(&chunk.stream_id)).ok()?;
        audio.push((chunk, artifact));
    }
    let artifact = message.artifact(&view_attachment(&view.view_id)).ok()?;
    let agents = payload
        .get("agents")
        .and_then(serde_json::Value::as_array)
        .map(|agents| {
            agents
                .iter()
                .filter_map(|a| a.get("agentId").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(SpectatorFrame {
        boundary,
        agents,
        sequence: message.topic_sequence(),
        replaced: message.replaced(),
        view,
        artifact,
        audio,
    })
}

/// Takes the frame out of a message and drops the message, as a renderer that finishes later
/// does. The delivery stays alive because the extracted handle still owns it.
pub fn detach_frame(message: flybus::Message) -> Option<(ViewRef, flybus::Artifact)> {
    let frame = snapshot_frame(&message)?;
    drop(message);
    Some((frame.view, frame.artifact))
}

// ----------------------------------------------------------------------------------------------
// Persistent assets

/// The preprovisioned local registry an `AssetRef` names.
///
/// An asset is installed and verified before a run; it is not a path, a URL or something a
/// worker fetches. Nothing in this registry can be addressed by an `ArtifactRef`, and
/// [`AssetRegistry::import`] hands back a fresh transient artifact rather than turning the
/// asset into one.
#[derive(Clone, Debug, Default)]
pub struct AssetRegistry {
    installed: BTreeMap<Id, (AssetRef, Vec<u8>)>,
}

impl AssetRegistry {
    pub fn new() -> AssetRegistry {
        AssetRegistry::default()
    }

    /// Installs content, verifying that it is the content the reference claims.
    pub fn install(&mut self, asset: AssetRef, bytes: Vec<u8>) -> DomainResult<()> {
        asset.validate().map_err(DomainError::invalid)?;
        if asset.byte_length != bytes.len() as u64 {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!("asset {}: byteLength is not the installed length", asset.id),
            ));
        }
        if asset.digest != digest_of_bytes(&bytes) {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!("asset {}: digest is not the installed content", asset.id),
            ));
        }
        self.installed.insert(asset.id.clone(), (asset, bytes));
        Ok(())
    }

    /// Resolves installed content. Identity and digest must both match: the same id with
    /// another digest is a different asset, not an upgrade.
    pub fn resolve(&self, asset: &AssetRef) -> DomainResult<&[u8]> {
        match self.installed.get(&asset.id) {
            Some((installed, bytes)) if installed == asset => Ok(bytes),
            Some(_) => Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!("asset {} is installed with another identity", asset.id),
            )),
            None => Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!("asset {} is not installed", asset.id),
            )),
        }
    }

    pub fn contains(&self, asset: &AssetRef) -> bool {
        self.resolve(asset).is_ok()
    }

    /// Imports installed content into the bus as a fresh immutable artifact.
    ///
    /// The artifact is sealed against the asset's digest, which is mandatory for a persistent
    /// asset import, and its identity belongs to the current store incarnation. The asset
    /// reference is unchanged and outlives it.
    pub async fn import(
        &self,
        client: &flybus::Client,
        asset: &AssetRef,
    ) -> DomainResult<flybus::Artifact> {
        let bytes = self.resolve(asset)?;
        let mut writer = client
            .artifacts()
            .allocate(asset.byte_length, "application/octet-stream")
            .await
            .map_err(|e| store_error("allocate", &e.message))?;
        writer
            .write_all(bytes)
            .map_err(|e| store_error("write", &e.to_string()))?;
        let artifact = writer
            .seal_with_digest(Some(asset.digest.clone()))
            .await
            .map_err(|e| store_error("seal", &e.message))?;
        fly_session_types::media::check_imported_asset(asset, artifact.reference())
            .map_err(|e| DomainError::before(ErrorCode::IdentityMismatch, e))?;
        Ok(artifact)
    }
}
