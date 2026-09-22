//! The counter arena: one environment worker with no emulator behind it.
//!
//! It holds a signed counter, applies one complete control batch per Advance, advances exactly
//! one interval, and returns boundary `k+1` with its world time advanced by `stepDuration`. It
//! never advances while waiting for the next request, and it does not free-run during agent
//! initialization.

use std::collections::BTreeSet;
use std::io::Write;

use crate::task::{controller_schema_ref, inspection, inspection_schema};
// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;
use crate::worker::{BoxFuture, HandlerCtx, HandlerReply, StatusCell, WorkerEndpoint};

/// The arena's view: a 4x4 RGBA8 tile whose bytes carry the counter.
pub const VIEW_WIDTH: u64 = 4;
pub const VIEW_HEIGHT: u64 = 4;

/// Deliberate faults a test can ask the environment for.
#[derive(Clone, Debug, Default)]
pub struct EnvironmentFaults {
    /// Hold `Environment.Advance` open for this long, after the world already moved.
    pub advance_delay_ms: u64,
    /// Drop the required sensory view from the result at this boundary, so the coordinator
    /// meets a world that advanced with no usable sensory data.
    pub omit_view_at_boundary: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct EnvironmentConfig {
    pub session_id: Id,
    pub worker_id: Id,
    pub incarnation_id: Id,
    /// The world's fixed reduced step duration. 60 Hz is `1/60` s.
    pub step_duration: RationalNs,
    pub ports: Vec<Id>,
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

    pub fn view_descriptor() -> ViewDescriptor {
        ViewDescriptor {
            view_id: id("arena"),
            width: VIEW_WIDTH,
            height: VIEW_HEIGHT,
            row_stride: VIEW_WIDTH * 4,
            pixel_aspect_numerator: 1,
            pixel_aspect_denominator: 1,
            observation_delay_steps: 0,
        }
    }

    fn build_descriptor(&self) -> DomainResult<EnvironmentDescriptor> {
        let descriptor = EnvironmentDescriptor {
            backend_digest: digest_of_bytes(b"counter-arena-backend-v1"),
            content_digest: digest_of_bytes(b"counter-arena-content-v1"),
            configuration_digest: digest_of_bytes(
                format!(
                    "counter-arena-config-v1\nstep={}/{}\nports={}\n",
                    self.config.step_duration.numerator,
                    self.config.step_duration.denominator,
                    self.config.ports.len()
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
            views: vec![CounterEnvironment::view_descriptor()],
            audio: Vec::new(),
            recovery: Recovery::ExactCheckpoint,
            determinism: Determinism::FixedBuild,
        };
        descriptor.validate().map_err(DomainError::invalid)?;
        Ok(descriptor)
    }

    /// Seals one immutable native frame for the current counter and returns the handle.
    async fn render(
        &self,
        ctx: &HandlerCtx<'_>,
    ) -> DomainResult<(ViewRef, flybus::Artifact)> {
        let descriptor = CounterEnvironment::view_descriptor();
        let len = descriptor.byte_length();
        let mut writer = ctx
            .client
            .artifacts()
            .allocate(len, "image/x-rgba")
            .await
            .map_err(|e| {
                DomainError::new(
                    ErrorCode::BackendFailure,
                    format!("frame allocation failed: {}", e.message),
                    MutationCertainty::Applied,
                )
            })?;
        // Every pixel carries the counter's low byte, so an agent reading the frame reads the
        // world rather than a constant.
        let byte = (self.counter & 0xff) as u8;
        writer
            .write_all(&vec![byte; len as usize])
            .map_err(|e| {
                DomainError::new(
                    ErrorCode::BackendFailure,
                    format!("frame write failed: {e}"),
                    MutationCertainty::Applied,
                )
            })?;
        let artifact = writer.seal().await.map_err(|e| {
            DomainError::new(
                ErrorCode::BackendFailure,
                format!("frame seal failed: {}", e.message),
                MutationCertainty::Applied,
            )
        })?;
        let view = ViewRef {
            view_id: descriptor.view_id.clone(),
            produced_step: descriptor.required_produced_step(self.boundary),
            pixels: artifact.reference().clone(),
        };
        Ok((view, artifact))
    }

    async fn observation(
        &self,
        ctx: &HandlerCtx<'_>,
    ) -> DomainResult<(WorldObservation, Vec<(String, flybus::Artifact)>)> {
        let omit = self.config.faults.omit_view_at_boundary == Some(self.boundary);
        let (views, attachments) = if omit {
            (Vec::new(), Vec::new())
        } else {
            let (view, artifact) = self.render(ctx).await?;
            let name = format!("view.{}", view.view_id);
            (vec![view], vec![(name, artifact)])
        };
        let observation = WorldObservation {
            boundary: self.boundary,
            world_time: self.world_time,
            engine_frame: Some(self.boundary.to_string()),
            sensory_views: views.clone(),
            inspection: inspection(self.counter, self.boundary),
            // The same immutable object serves the broadcast view; nothing is rendered twice.
            broadcast_views: views,
            audio: Vec::new(),
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
