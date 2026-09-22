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
}

#[derive(Clone, Debug)]
pub struct EnvironmentConfig {
    pub session_id: Id,
    pub worker_id: Id,
    pub incarnation_id: Id,
    /// The world's fixed reduced step duration. 60 Hz is `1/60` s.
    pub step_duration: RationalNs,
    pub ports: Vec<Id>,
    /// The view's declared render delay, in steps. Zero is same-boundary output.
    pub observation_delay_steps: u64,
    /// Counts frames actually rendered, so a test can prove one image was not rendered twice.
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
            config,
        }
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
            id("checkpoint-v1"),
        ]
    }

    fn status_cell(&self) -> StatusCell {
        self.status.clone()
    }

    fn methods(&self) -> Vec<&'static str> {
        vec!["Environment.Initialize", "Environment.Advance"]
    }

    fn handle<'a>(&'a mut self, ctx: HandlerCtx<'a>) -> BoxFuture<'a, DomainResult<HandlerReply>> {
        Box::pin(async move {
            match ctx.method {
                "Environment.Initialize" => self.initialize(&ctx).await,
                "Environment.Advance" => self.advance(&ctx).await,
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
