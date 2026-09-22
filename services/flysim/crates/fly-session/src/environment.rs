//! The counter arena: one environment worker with no emulator behind it.
//!
//! It holds a signed counter, applies one complete control batch per Advance, advances exactly
//! one interval, and returns boundary `k+1` with its world time advanced by `stepDuration`. It
//! never advances while waiting for the next request, and it does not free-run during agent
//! initialization.
//!
//! Its native output is real: one immutable RGBA8 frame per boundary through a
//! [`ViewPipeline`](crate::media::ViewPipeline) that honours the declared
//! `observationDelaySteps`, and one audio chunk per transition with an exact sample budget.
//! Nothing here resizes, mixes, composites or encodes anything; that is the presentation
//! layer's work.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::media::{self, AudioSource, RenderCounter, ViewPipeline};
use crate::task::{controller_schema_ref, inspection, inspection_schema};
// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;
use crate::worker::{BoxFuture, HandlerCtx, HandlerReply, StatusCell, WorkerEndpoint};

/// The arena's one view: a small native RGBA8 image.
pub const VIEW_ID: &str = "arena";
pub const VIEW_WIDTH: u64 = 32;
pub const VIEW_HEIGHT: u64 = 24;

/// The arena's one audio stream. 48 kHz stereo is a native rate, not a presentation choice.
pub const AUDIO_STREAM_ID: &str = "arena";
pub const SAMPLE_RATE: u64 = 48_000;
pub const CHANNELS: u64 = 2;

/// Deliberate faults a test can ask the environment for.
#[derive(Clone, Debug, Default)]
pub struct EnvironmentFaults {
    /// Hold `Environment.Advance` open for this long, after the world already moved.
    pub advance_delay_ms: u64,
    /// Drop the required sensory view from the result at this boundary, so the coordinator
    /// meets a world that advanced with no usable sensory data.
    pub omit_view_at_boundary: Option<u64>,
    /// Serve the previous boundary's frame at this boundary: an extra-delayed sensory input,
    /// which is a step failure rather than an acceptable latest frame.
    pub stale_view_at_boundary: Option<u64>,
    /// Seal a frame one row short at this boundary, so its artifact length is not
    /// `rowStride x height`.
    pub truncated_view_at_boundary: Option<u64>,
    /// Leave the audio chunk out of the result at this boundary.
    pub omit_audio_at_boundary: Option<u64>,
    /// Emit an audio chunk that starts before the previous chunk ended.
    pub overlapping_audio_at_boundary: Option<u64>,
    /// Refuse `State.StageRestore`, so a group install meets a participant that will not
    /// validate.
    pub fail_stage_restore: bool,
    /// Refuse `State.ActivateRestore` after staging, so a group meets a failure halfway
    /// through activation.
    pub fail_activate_restore: bool,
}

#[derive(Clone, Debug)]
pub struct EnvironmentConfig {
    pub session_id: Id,
    pub worker_id: Id,
    pub incarnation_id: Id,
    /// The world's fixed reduced step duration. 60 Hz is `1/60` s.
    pub step_duration: RationalNs,
    pub ports: Vec<Id>,
    /// The thread allocation the launcher started this worker within.
    pub worker_threads: usize,
    /// The view's declared render delay, in steps. Zero is same-boundary output.
    pub observation_delay_steps: u64,
    /// Counts frames actually rendered, so a test can prove one image was not rendered twice.
    ///
    /// It counts in this process only: a world with a process of its own counts there.
    pub renders: RenderCounter,
    pub faults: EnvironmentFaults,
}

/// The counter environment endpoint.
pub struct CounterEnvironment {
    config: EnvironmentConfig,
    status: StatusCell,
    epoch: Option<Id>,
    episode_id: Option<Id>,
    descriptor: Option<EnvironmentDescriptor>,
    bindings: Vec<PortBinding>,
    boundary: u64,
    counter: i64,
    world_time: RationalNs,
    advances: u64,
    batches: BTreeSet<Id>,
    pipeline: Option<ViewPipeline>,
    audio: Option<AudioSource>,
    /// The frame served at the previous boundary, kept only so a fault can serve it again.
    previous_view: Option<(ViewRef, flybus::Artifact)>,
    /// A validated replacement world the live session cannot see yet.
    staged: Option<StagedWorld>,
    /// Restore tokens this world has activated. A token activates once.
    activated: BTreeSet<Id>,
}

impl CounterEnvironment {
    pub fn new(config: EnvironmentConfig) -> CounterEnvironment {
        CounterEnvironment {
            status: StatusCell::new(),
            epoch: None,
            episode_id: None,
            descriptor: None,
            bindings: Vec::new(),
            boundary: 0,
            counter: 0,
            world_time: RationalNs::ZERO,
            advances: 0,
            batches: BTreeSet::new(),
            pipeline: None,
            audio: None,
            previous_view: None,
            staged: None,
            activated: BTreeSet::new(),
            config,
        }
    }

    /// True while a validated replacement world is staged and not yet activated.
    pub fn has_staged_restore(&self) -> bool {
        self.staged.is_some()
    }

    pub fn status(&self) -> StatusCell {
        self.status.clone()
    }

    /// How many intervals this world has advanced. One complete batch advances it once.
    pub fn advances(&self) -> u64 {
        self.advances
    }

    pub fn counter(&self) -> i64 {
        self.counter
    }

    pub fn boundary(&self) -> u64 {
        self.boundary
    }

    /// The controller every port of this arena declares: two buttons and one bipolar axis.
    pub fn controller_schema() -> ControllerSchema {
        ControllerSchema {
            schema: controller_schema_ref(),
            buttons: vec![id("inc"), id("dec")],
            axes: vec![AxisSchema {
                id: id("bias"),
                range: AxisRange::Bipolar,
                neutral: 0.0,
            }],
        }
    }

    /// The arena's native view, with the configured render delay.
    pub fn view_descriptor(observation_delay_steps: u64) -> ViewDescriptor {
        ViewDescriptor {
            view_id: id(VIEW_ID),
            width: VIEW_WIDTH,
            height: VIEW_HEIGHT,
            row_stride: VIEW_WIDTH * 4,
            pixel_aspect_numerator: 1,
            pixel_aspect_denominator: 1,
            observation_delay_steps,
        }
    }

    /// The arena's native audio stream.
    pub fn audio_descriptor() -> AudioDescriptor {
        AudioDescriptor {
            stream_id: id(AUDIO_STREAM_ID),
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
        }
    }

    fn build_descriptor(&self) -> DomainResult<EnvironmentDescriptor> {
        let descriptor = EnvironmentDescriptor {
            backend_digest: digest_of_bytes(b"counter-arena-backend-v1"),
            content_digest: digest_of_bytes(b"counter-arena-content-v1"),
            configuration_digest: digest_of_bytes(
                format!(
                    "counter-arena-config-v1\nstep={}/{}\nports={}\ndelay={}\n",
                    self.config.step_duration.numerator,
                    self.config.step_duration.denominator,
                    self.config.ports.len(),
                    self.config.observation_delay_steps
                )
                .as_bytes(),
            ),
            step_duration: self.config.step_duration,
            ports: self
                .config
                .ports
                .iter()
                .map(|port_id| PortDescriptor {
                    port_id: port_id.clone(),
                    controls: CounterEnvironment::controller_schema(),
                })
                .collect(),
            inspection_schema: inspection_schema(),
            views: vec![CounterEnvironment::view_descriptor(
                self.config.observation_delay_steps,
            )],
            audio: vec![CounterEnvironment::audio_descriptor()],
            recovery: Recovery::ExactCheckpoint,
            determinism: Determinism::FixedBuild,
        };
        descriptor.validate().map_err(DomainError::invalid)?;
        Ok(descriptor)
    }

    /// Renders this boundary's native media and returns the observation with its owned
    /// handles. The same immutable object serves the sensory and the broadcast view; nothing
    /// is rendered twice and no second copy of the pixels exists.
    async fn observation(
        &mut self,
        ctx: &HandlerCtx<'_>,
    ) -> DomainResult<(WorldObservation, Vec<(String, flybus::Artifact)>)> {
        let boundary = self.boundary;
        let counter = self.counter;
        let pipeline = self
            .pipeline
            .as_mut()
            .ok_or_else(|| DomainError::before(ErrorCode::InvalidPhase, "no view pipeline"))?;
        if self.config.faults.truncated_view_at_boundary == Some(boundary) {
            pipeline
                .render_truncated(ctx.client, boundary, counter)
                .await?;
        } else {
            pipeline.render(ctx.client, boundary, counter).await?;
        }
        let produced = pipeline.at(boundary);

        let mut attachments = Vec::new();
        let mut views = Vec::new();
        if self.config.faults.omit_view_at_boundary == Some(boundary) {
            // A world that advanced with no usable sensory data.
        } else if self.config.faults.stale_view_at_boundary == Some(boundary) {
            if let Some((view, artifact)) = self.previous_view.clone() {
                attachments.push((media::view_attachment(&view.view_id), artifact));
                views.push(view);
            }
        } else if let Some((view, artifact)) = produced.clone() {
            attachments.push((media::view_attachment(&view.view_id), artifact));
            views.push(view);
        } else {
            return Err(DomainError::new(
                ErrorCode::BackendFailure,
                "the view pipeline has no frame for this boundary",
                MutationCertainty::Applied,
            ));
        }
        self.previous_view = produced;

        let mut audio = Vec::new();
        if boundary > 0 && self.config.faults.omit_audio_at_boundary != Some(boundary) {
            let step = self.config.step_duration;
            let overlap = self.config.faults.overlapping_audio_at_boundary == Some(boundary);
            let source = self
                .audio
                .as_mut()
                .ok_or_else(|| DomainError::before(ErrorCode::InvalidPhase, "no audio source"))?;
            let (mut chunk, artifact) = source.produce(ctx.client, &step, counter).await?;
            if overlap {
                // A chunk that starts inside the previous one: the timeline refuses it rather
                // than playing the same samples twice.
                chunk.first_sample = chunk.first_sample.saturating_sub(1);
            }
            attachments.push((media::audio_attachment(&chunk.stream_id), artifact));
            audio.push(chunk);
        }

        let observation = WorldObservation {
            boundary,
            world_time: self.world_time,
            engine_frame: Some(boundary.to_string()),
            sensory_views: views.clone(),
            inspection: inspection(counter, boundary),
            // The same immutable object serves the broadcast view; nothing is rendered twice.
            broadcast_views: views,
            audio,
        };
        Ok((observation, attachments))
    }

    async fn initialize(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        if self.descriptor.is_some() {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "this environment is already initialized",
            ));
        }
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this environment belongs to another session",
            ));
        }
        if scope.step != 0 {
            return Err(DomainError::before(
                ErrorCode::FutureStep,
                "Environment.Initialize uses the new epoch at step 0",
            ));
        }
        let params: EnvironmentInitializeParams = ctx.params()?;
        if params.port_bindings.len() > MAX_PORTS {
            return Err(DomainError::invalid("at most 4 ports in the first composition"));
        }
        let mut seen = BTreeSet::new();
        for (port_id, _agent_id) in &params.port_bindings {
            if !self.config.ports.contains(port_id) {
                return Err(DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("port {port_id} is not a port of this arena"),
                ));
            }
            if !seen.insert(port_id.clone()) {
                return Err(DomainError::invalid(format!("port {port_id} is bound twice")));
            }
        }
        let descriptor = self.build_descriptor()?;
        self.epoch = Some(scope.epoch.clone());
        self.episode_id = Some(params.episode_id.clone());
        self.bindings = params
            .port_bindings
            .iter()
            .map(|(port_id, agent_id)| PortBinding {
                port_id: port_id.clone(),
                agent_id: agent_id.clone(),
            })
            .collect();
        self.boundary = 0;
        self.counter = 0;
        self.world_time = RationalNs::ZERO;
        self.batches.clear();
        self.pipeline = Some(ViewPipeline::new(
            CounterEnvironment::view_descriptor(self.config.observation_delay_steps),
            self.config.renders.clone(),
        ));
        // A fresh episode starts at audio origin zero; a restore would resume the preserved
        // sample position instead, and its first chunk would mark the discontinuity.
        self.audio = Some(AudioSource::new(CounterEnvironment::audio_descriptor(), 0));
        self.descriptor = Some(descriptor.clone());
        // The world is stopped when O[0] goes out and cannot free-run while the brains boot.
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope.clone()));
        self.status.progress(1);

        let (observation, attachments) = self.observation(ctx).await?;
        let result = EnvironmentInitializeResult { descriptor, observation };
        let mut reply = HandlerReply::from(&result);
        reply.artifacts = attachments;
        Ok(reply)
    }

    async fn advance(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        let Some(descriptor) = self.descriptor.clone() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "this environment is uninitialized",
            ));
        };
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this environment belongs to another session",
            ));
        }
        match &self.epoch {
            Some(epoch) if *epoch == scope.epoch => {}
            _ => {
                return Err(DomainError::before(
                    ErrorCode::StaleEpoch,
                    "this scope names an epoch this environment has left",
                ));
            }
        }
        if scope.step != self.boundary {
            return Err(DomainError::before(
                if scope.step < self.boundary {
                    ErrorCode::StaleStep
                } else {
                    ErrorCode::FutureStep
                },
                "Environment.Advance must name the boundary the world is at",
            ));
        }
        let params: AdvanceParams = ctx.params()?;
        // Batch ids are unique within an epoch; reusing one for a different request or step is
        // a conflict, not a second world mutation.
        if self.batches.contains(&params.batch_id) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                format!("batch {} was already applied in this epoch", params.batch_id),
            ));
        }
        self.check_batch(&descriptor, &params.controls)?;
        let digest = controls_digest(&params.controls);

        let applied_from = self.boundary;
        // Apply the complete batch to its interval and advance exactly one framework step.
        let mut delta = 0i64;
        for control in &params.controls {
            delta += crate::task::CounterTask::delta_of(control);
        }
        self.counter += delta;
        self.boundary += 1;
        self.world_time = self
            .world_time
            .checked_add(&descriptor.step_duration)
            .map_err(|e| DomainError::new(ErrorCode::Internal, e, MutationCertainty::Applied))?;
        self.advances += 1;
        self.batches.insert(params.batch_id.clone());
        self.status.set_batch(params.batch_id.clone());
        self.status.set_scope(Some(scope_at(
            &scope.session_id,
            &scope.epoch,
            self.boundary,
        )));
        self.status.progress(1);

        if self.config.faults.advance_delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(
                self.config.faults.advance_delay_ms,
            ))
            .await;
        }

        let (observation, attachments) = self.observation(ctx).await?;
        // The record of batch id, result and next boundary exists before the acknowledgment.
        let result = StepResult {
            batch_id: params.batch_id,
            applied_from_step: applied_from,
            next_step: self.boundary,
            applied_controls_digest: digest,
            observation,
        };
        let mut reply = HandlerReply::from(&result);
        reply.artifacts = attachments;
        Ok(reply)
    }

    /// Every active port must appear exactly once, with every declared button and axis in
    /// descriptor order. Uncontrolled ports were configured neutral before the epoch.
    fn check_batch(
        &self,
        descriptor: &EnvironmentDescriptor,
        controls: &[PortControl],
    ) -> DomainResult<()> {
        if controls.len() != descriptor.ports.len() {
            return Err(DomainError::invalid(format!(
                "the batch has {} port controls; the descriptor declares {}",
                controls.len(),
                descriptor.ports.len()
            )));
        }
        for (declared, given) in descriptor.ports.iter().zip(controls) {
            if declared.port_id != given.port_id {
                return Err(DomainError::invalid(format!(
                    "port {} is out of descriptor order; expected {}",
                    given.port_id, declared.port_id
                )));
            }
            declared.controls.check(given).map_err(DomainError::invalid)?;
        }
        Ok(())
    }
}

impl WorkerEndpoint for CounterEnvironment {
    fn worker_id(&self) -> Id {
        self.config.worker_id.clone()
    }

    fn incarnation_id(&self) -> Id {
        self.config.incarnation_id.clone()
    }

    fn session_id(&self) -> Id {
        self.config.session_id.clone()
    }

    fn role(&self) -> Role {
        Role::Environment
    }

    fn capabilities(&self) -> Vec<Id> {
        vec![
            id("world-step-v1"),
            id("pixel-observation-v1"),
            id(crate::state::CHECKPOINT_CAPABILITY),
        ]
    }

    fn status_cell(&self) -> StatusCell {
        self.status.clone()
    }

    fn worker_threads(&self) -> u64 {
        self.config.worker_threads as u64
    }

    fn methods(&self) -> Vec<&'static str> {
        vec![
            "Environment.Initialize",
            "Environment.Advance",
            "State.Capture",
            "State.StageRestore",
            "State.ActivateRestore",
        ]
    }

    fn handle<'a>(&'a mut self, ctx: HandlerCtx<'a>) -> BoxFuture<'a, DomainResult<HandlerReply>> {
        Box::pin(async move {
            match ctx.method {
                "Environment.Initialize" => self.initialize(&ctx).await,
                "Environment.Advance" => self.advance(&ctx).await,
                "State.Capture" => self.state_capture(&ctx).await,
                "State.StageRestore" => self.state_stage_restore(&ctx).await,
                "State.ActivateRestore" => self.state_activate_restore(&ctx).await,
                other => Err(DomainError::before(
                    ErrorCode::Unsupported,
                    format!("{other} is not an environment method"),
                )),
            }
        })
    }
}

/// A synthetic backend/task configuration asset for the arena.
pub fn synthetic_asset(asset_id: &str, body: &str) -> AssetRef {
    AssetRef {
        id: id(asset_id),
        digest: digest_of_bytes(body.as_bytes()),
        byte_length: body.len() as u64,
        format: id("fly-config-v1"),
    }
}

// -------------------------------------------------------------------------------------------
// STATE-01: capture and restore

/// The version this payload layout is written and read under.
pub const WORLD_PAYLOAD_VERSION: u64 = 1;

fn incompatible(message: impl std::fmt::Display) -> DomainError {
    DomainError::before(ErrorCode::IncompatibleState, message)
}

/// One staged restore, held outside the live world until it is activated.
struct StagedWorld {
    token: Id,
    checkpoint_id: Id,
    scope: Scope,
    episode_id: Id,
    descriptor: EnvironmentDescriptor,
    boundary: u64,
    counter: i64,
    world_time: RationalNs,
    advances: u64,
    frames: Vec<(u64, i64)>,
    audio_next_sample: u64,
    audio_phase: u64,
    audio_accumulator: u128,
    audio_denominator: u128,
}

impl CounterEnvironment {
    /// `State.Capture`: the world at its committed boundary, including its pending sensor
    /// pipeline.
    ///
    /// The pipeline is recorded as reconstruction inputs -- the producing boundary and the
    /// world counter of every retained frame -- and never as an artifact identity: a
    /// transient artifact belongs to the router that is running now, and a checkpoint outlives
    /// it.
    async fn state_capture(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        let Some(descriptor) = self.descriptor.clone() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "this environment is uninitialized",
            ));
        };
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this environment belongs to another session",
            ));
        }
        match &self.epoch {
            Some(epoch) if *epoch == scope.epoch => {}
            _ => {
                return Err(DomainError::before(
                    ErrorCode::StaleEpoch,
                    "State.Capture names an epoch this environment has left",
                ));
            }
        }
        if scope.step != self.boundary {
            return Err(DomainError::before(
                if scope.step < self.boundary {
                    ErrorCode::StaleStep
                } else {
                    ErrorCode::FutureStep
                },
                "State.Capture must name the boundary the world is at",
            ));
        }
        let params: CaptureParams = ctx.params()?;
        let pipeline = self
            .pipeline
            .as_ref()
            .ok_or_else(|| DomainError::before(ErrorCode::InvalidPhase, "no view pipeline"))?;
        let audio = self
            .audio
            .as_ref()
            .ok_or_else(|| DomainError::before(ErrorCode::InvalidPhase, "no audio source"))?;
        let (accumulator, denominator) = audio.accumulator();
        let previous = self.status.state();
        self.status.set_state(WorkerState::Capturing);
        let payload = serde_json::json!({
            "payloadVersion": WORLD_PAYLOAD_VERSION,
            "kind": "world",
            "workerId": self.config.worker_id.as_str(),
            "checkpointId": params.checkpoint_id.as_str(),
            "sourceScope": scope.to_json(),
            "episodeId": self.episode_id.clone().expect("initialized").as_str(),
            "committedStep": self.boundary.to_string(),
            "counter": self.counter.to_string(),
            "worldTime": self.world_time.to_json(),
            "advances": self.advances.to_string(),
            "descriptor": descriptor.to_json(),
            "pipeline": {
                // The declared delay's whole queue, oldest first.
                "frames": pipeline
                    .retained()
                    .into_iter()
                    .map(|(boundary, counter)| serde_json::json!({
                        "boundary": boundary.to_string(),
                        "counter": counter.to_string(),
                    }))
                    .collect::<Vec<_>>(),
            },
            "audio": {
                "nextSample": audio.next_sample().to_string(),
                "phase": audio.phase().to_string(),
                "accumulator": accumulator.to_string(),
                "denominator": denominator.to_string(),
                "chunks": audio.chunks().to_string(),
            },
        });
        let bytes = canonicalize(&payload)
            .map_err(|e| DomainError::invalid(format!("State.Capture: {}", e.0)))?
            .into_bytes();
        let digest = digest_of_bytes(&bytes);
        let artifact = crate::state::seal_payload(ctx.client, &bytes, &digest).await?;
        // A capture reads the world; it does not advance it.
        self.status.set_state(previous);
        let result = CaptureResult {
            checkpoint_id: params.checkpoint_id,
            boundary: self.boundary,
            compatibility_digest: crate::state::Compatibility::of(&descriptor).digest(),
            payload: artifact.reference().clone(),
        };
        Ok(HandlerReply::with_artifacts(
            object(result.to_json()),
            vec![(crate::state::PAYLOAD_ATTACHMENT.to_owned(), artifact)],
        ))
    }

    /// `State.StageRestore`: validate a replacement world into a staging slot.
    async fn state_stage_restore(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this environment belongs to another session",
            ));
        }
        if let Some(epoch) = &self.epoch
            && *epoch == scope.epoch
        {
            return Err(DomainError::before(
                ErrorCode::StaleEpoch,
                "State.StageRestore proposes the epoch this environment is already running",
            ));
        }
        if self.descriptor.is_some() {
            // A world that is already running a boundary is not a quiescent replacement: the
            // group replaces it rather than restoring over a live one.
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "State.StageRestore needs an uninitialized replacement environment",
            ));
        }
        let params: StageRestoreParams = ctx.params()?;
        if params.source_scope.step != scope.step {
            return Err(DomainError::invalid(
                "State.StageRestore's scope step must be the source boundary",
            ));
        }
        let artifact = ctx.artifact(crate::state::PAYLOAD_ATTACHMENT)?;
        if artifact.reference() != &params.payload {
            return Err(DomainError::before(
                ErrorCode::BufferInvalid,
                "the staged payload attachment is not the artifact the request names",
            ));
        }
        let bytes = artifact.read_all().await.map_err(|e| {
            DomainError::before(
                ErrorCode::BufferInvalid,
                format!("the staged payload could not be read: {}", e.message),
            )
        })?;
        let declared = params
            .payload
            .digest
            .clone()
            .ok_or_else(|| incompatible("a checkpoint payload must carry a content digest"))?;
        let actual = digest_of_bytes(&bytes);
        if actual != declared || bytes.len() as u64 != params.payload.byte_length {
            return Err(incompatible(
                "the staged payload is not the content the request declares",
            ));
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| DomainError::invalid(format!("the staged payload is not JSON: {e}")))?;
        let text = |key: &str| -> DomainResult<String> {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| incompatible(format!("the world payload has no {key}")))
        };
        let number = |key: &str| -> DomainResult<u64> {
            text(key)?
                .parse::<u64>()
                .map_err(|_| incompatible(format!("the world payload's {key} is not a U64")))
        };
        if value.get("payloadVersion").and_then(Value::as_u64) != Some(WORLD_PAYLOAD_VERSION) {
            return Err(incompatible("the world payload is another payload version"));
        }
        if text("kind")? != "world" {
            return Err(incompatible("this payload is not a world's state"));
        }
        if text("workerId")? != self.config.worker_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the staged payload belongs to another world",
            ));
        }
        if text("checkpointId")? != params.checkpoint_id {
            return Err(incompatible("the staged payload belongs to another checkpoint"));
        }
        let source_scope = Scope::from_json(
            value
                .get("sourceScope")
                .ok_or_else(|| incompatible("the world payload has no sourceScope"))?,
        )
        .map_err(|e| incompatible(format!("the world payload's sourceScope: {}", e.0)))?;
        if source_scope != params.source_scope {
            return Err(incompatible(
                "the staged payload was captured at another source scope",
            ));
        }
        let committed_step = number("committedStep")?;
        if committed_step != params.source_scope.step {
            return Err(incompatible(
                "the staged payload's committed step is not the source boundary",
            ));
        }
        let descriptor = EnvironmentDescriptor::from_json(
            value
                .get("descriptor")
                .ok_or_else(|| incompatible("the world payload has no descriptor"))?,
        )
        .map_err(|e| incompatible(format!("the world payload's descriptor: {}", e.0)))?;
        // The replacement builds the descriptor it would advertise and compares. A world
        // started with other ports, another cadence or another declared render delay is a
        // different backend, not this one resumed.
        let live = self.build_descriptor()?;
        if descriptor != live {
            return Err(incompatible(
                "the staged world was captured under another environment descriptor",
            ));
        }
        let expected = crate::state::Compatibility::of(&descriptor).digest();
        if expected != params.compatibility_digest {
            return Err(incompatible(format!(
                "the staged world's compatibility {expected} is not the {} the restore requires",
                params.compatibility_digest
            )));
        }
        let counter: i64 = text("counter")?
            .parse()
            .map_err(|_| incompatible("the world payload's counter is not an integer"))?;
        let world_time = RationalNs::from_json(
            value
                .get("worldTime")
                .ok_or_else(|| incompatible("the world payload has no worldTime"))?,
        )
        .map_err(|e| incompatible(format!("the world payload's worldTime: {}", e.0)))?;
        let pipeline_value = value
            .get("pipeline")
            .and_then(|p| p.get("frames"))
            .and_then(Value::as_array)
            .ok_or_else(|| incompatible("the world payload has no pipeline frames"))?;
        let mut frames = Vec::with_capacity(pipeline_value.len());
        for frame in pipeline_value {
            let boundary = frame
                .get("boundary")
                .and_then(Value::as_str)
                .ok_or_else(|| incompatible("a captured frame has no boundary"))?
                .parse::<u64>()
                .map_err(|_| incompatible("a captured frame's boundary is not a U64"))?;
            let frame_counter = frame
                .get("counter")
                .and_then(Value::as_str)
                .ok_or_else(|| incompatible("a captured frame has no counter"))?
                .parse::<i64>()
                .map_err(|_| incompatible("a captured frame's counter is not an integer"))?;
            frames.push((boundary, frame_counter));
        }
        match frames.last() {
            Some((boundary, _)) if *boundary == committed_step => {}
            _ => {
                return Err(incompatible(
                    "the captured pipeline does not end at the committed boundary",
                ));
            }
        }
        let audio_value = value
            .get("audio")
            .ok_or_else(|| incompatible("the world payload has no audio state"))?;
        let audio_number = |key: &str| -> DomainResult<u128> {
            audio_value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| incompatible(format!("the captured audio state has no {key}")))?
                .parse::<u128>()
                .map_err(|_| incompatible(format!("the captured audio {key} is not a number")))
        };
        let audio_next_sample = u64::try_from(audio_number("nextSample")?)
            .map_err(|_| incompatible("the captured audio position is outside U64"))?;
        let audio_phase = u64::try_from(audio_number("phase")?)
            .map_err(|_| incompatible("the captured audio phase is outside U64"))?;

        if self.config.faults.fail_stage_restore {
            return Err(incompatible(
                "injected staging refusal: this participant's replacement state does not \
validate",
            ));
        }
        if let Some(staged) = &self.staged {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                format!(
                    "this environment already holds the staged restore {} for checkpoint {}",
                    staged.token, staged.checkpoint_id
                ),
            ));
        }
        let token = crate::agent::restore_token(
            &params.checkpoint_id,
            &scope,
            &actual,
            &self.config.incarnation_id,
        );
        if self.activated.contains(&token) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                "this exact restore was already activated on this environment",
            ));
        }
        self.staged = Some(StagedWorld {
            token: token.clone(),
            checkpoint_id: params.checkpoint_id.clone(),
            scope,
            episode_id: parse_id(&text("episodeId")?)
                .map_err(|e| incompatible(format!("the world payload's episodeId {e}")))?,
            descriptor,
            boundary: committed_step,
            counter,
            world_time,
            advances: number("advances")?,
            frames,
            audio_next_sample,
            audio_phase,
            audio_accumulator: audio_number("accumulator")?,
            audio_denominator: audio_number("denominator")?,
        });
        self.status.set_state(WorkerState::StagedRestore);
        let result = StageRestoreResult {
            checkpoint_id: params.checkpoint_id,
            restore_token: token,
        };
        Ok(HandlerReply::from(&result))
    }

    /// `State.ActivateRestore`: install the staged world and return its coherent observation.
    ///
    /// Nothing advances. The pipeline's frames are rendered again into fresh artifacts of the
    /// current store, which is what "the durable store imports fresh immutable bus artifacts"
    /// means on the producing side, and the observation carries no audio chunk because no
    /// interval was played.
    async fn state_activate_restore(
        &mut self,
        ctx: &HandlerCtx<'_>,
    ) -> DomainResult<HandlerReply> {
        let params: ActivateRestoreParams = ctx.params()?;
        if self.activated.contains(&params.restore_token) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                "this restore token has already been activated",
            ));
        }
        let Some(staged) = self.staged.take() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "this environment holds no staged restore",
            ));
        };
        if staged.token != params.restore_token {
            let token = staged.token.clone();
            self.staged = Some(staged);
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!(
                    "this environment's staged restore is {token}, not {}",
                    params.restore_token
                ),
            ));
        }
        if self.config.faults.fail_activate_restore {
            let token = staged.token.clone();
            self.staged = Some(staged);
            self.status.set_state(WorkerState::Failed);
            return Err(DomainError::new(
                ErrorCode::BackendFailure,
                format!("injected activation failure; {token} stays staged and unresumed"),
                MutationCertainty::None,
            ));
        }
        self.status.set_state(WorkerState::Restoring);
        let mut pipeline = ViewPipeline::new(
            CounterEnvironment::view_descriptor(self.config.observation_delay_steps),
            self.config.renders.clone(),
        );
        pipeline.restore(ctx.client, &staged.frames).await?;
        let audio = AudioSource::restored_from(
            CounterEnvironment::audio_descriptor(),
            staged.audio_next_sample,
            staged.audio_phase,
            staged.audio_accumulator,
            staged.audio_denominator,
        )?;
        self.epoch = Some(staged.scope.epoch.clone());
        self.episode_id = Some(staged.episode_id.clone());
        self.descriptor = Some(staged.descriptor.clone());
        self.boundary = staged.boundary;
        self.counter = staged.counter;
        self.world_time = staged.world_time;
        self.advances = staged.advances;
        // Batch ids are unique within an epoch, and this is a new one. Keeping the old set
        // would refuse nothing extra: a request under the old epoch is already refused by its
        // scope.
        self.batches.clear();
        self.pipeline = Some(pipeline);
        self.audio = Some(audio);
        self.previous_view = None;
        self.activated.insert(staged.token);
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope_at(
            &staged.scope.session_id,
            &staged.scope.epoch,
            staged.boundary,
        )));

        let (observation, attachments) = self.restored_observation()?;
        let result = ActivateRestoreResult {
            committed_step: staged.boundary,
            checkpoint_id: staged.checkpoint_id,
            observation: Some(observation),
        };
        result
            .validate_for_role(Role::Environment)
            .map_err(|e| DomainError::invalid(e.0))?;
        let mut reply = HandlerReply::from(&result);
        reply.artifacts = attachments;
        Ok(reply)
    }

    /// The observation the restored world is already at: no render, no advance, no audio.
    fn restored_observation(
        &mut self,
    ) -> DomainResult<(WorldObservation, Vec<(String, flybus::Artifact)>)> {
        let boundary = self.boundary;
        let counter = self.counter;
        let pipeline = self
            .pipeline
            .as_ref()
            .ok_or_else(|| DomainError::before(ErrorCode::InvalidPhase, "no view pipeline"))?;
        let (view, artifact) = pipeline.at(boundary).ok_or_else(|| {
            incompatible("the restored pipeline holds no frame for the restored boundary")
        })?;
        self.previous_view = Some((view.clone(), artifact.clone()));
        let observation = WorldObservation {
            boundary,
            world_time: self.world_time,
            engine_frame: Some(boundary.to_string()),
            sensory_views: vec![view.clone()],
            inspection: inspection(counter, boundary),
            broadcast_views: vec![view.clone()],
            // No interval was played, so there is no chunk. A chunk here would be an old
            // epoch's audio offered as current.
            audio: Vec::new(),
        };
        Ok((
            observation,
            vec![(media::view_attachment(&view.view_id), artifact)],
        ))
    }
}
