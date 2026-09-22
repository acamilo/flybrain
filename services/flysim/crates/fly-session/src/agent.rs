//! A small fake agent worker: an explicit seed, a mutation counter the tests read, and a fixed
//! readout stub.
//!
//! There is no neural model here and no attempt to imitate one. What it does model exactly is
//! the *ordering* `workers-v1` section 2 requires: Prepare applies pre-step stimulation, then
//! advances whole ticks, then decodes; Commit installs the next input, then applies task
//! stimulation, then reinforces once, and executes no tick at all. Every mutating step bumps
//! one counter, which is how a test proves a duplicate request changed nothing.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::clock::TickAccumulator;
use crate::dedup::OpClass;
use crate::task::{context_schema, decision_schema};
// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;
use crate::worker::{BoxFuture, HandlerCtx, HandlerReply, StatusCell, WorkerEndpoint};

/// The fake numerical model: a seeded stream and a count of everything that mutated it.
#[derive(Clone, Debug)]
pub struct FakeModel {
    seed: i32,
    state: u64,
    mutations: u64,
    ticks: u64,
    stimulations: u64,
    reinforcements: u64,
    learning_enabled: bool,
    learning_updates: u64,
    learning_changed: u64,
    last_signal: f64,
    input_value: i64,
    input_installs: u64,
}

impl FakeModel {
    /// A model at its initial state for `seed`. The seed is run configuration and state, and
    /// the same seed always produces the same stream.
    pub fn new(seed: i32) -> FakeModel {
        FakeModel {
            seed,
            // Sign-extend so a negative seed is a distinct stream rather than a truncation.
            state: (seed as i64 as u64) ^ 0x9e37_79b9_7f4a_7c15,
            mutations: 0,
            ticks: 0,
            stimulations: 0,
            reinforcements: 0,
            learning_enabled: false,
            learning_updates: 0,
            learning_changed: 0,
            last_signal: 0.0,
            input_value: 0,
            input_installs: 0,
        }
    }

    pub fn seed(&self) -> i32 {
        self.seed
    }

    /// Every mutation this model has taken: ticks, stimulations, reinforcements and installs.
    ///
    /// Tests read this to prove a duplicate request repeated nothing.
    pub fn mutations(&self) -> u64 {
        self.mutations
    }

    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    pub fn reinforcements(&self) -> u64 {
        self.reinforcements
    }

    pub fn stimulations(&self) -> u64 {
        self.stimulations
    }

    /// How many times the next sensory input was installed: once per Initialize and Commit.
    pub fn input_installs(&self) -> u64 {
        self.input_installs
    }

    /// The scalar the encoder last installed.
    pub fn input_value(&self) -> i64 {
        self.input_value
    }

    fn draw(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.mutations += 1;
        self.state
    }

    /// Advances `ticks` whole model ticks. This is the only place ticks are executed.
    fn advance(&mut self, ticks: u64) {
        for _ in 0..ticks {
            self.draw();
            self.ticks += 1;
        }
    }

    /// Applies one declared stimulus. The kind resolves through the profile; a caller never
    /// names a neuron or a drive value.
    fn stimulate(&mut self, stimulus: &Stimulus) {
        let kind = u64::from_le_bytes({
            let d = digest_of_bytes(stimulus.kind_id.as_bytes());
            let bytes = d.as_bytes();
            let mut out = [0u8; 8];
            out.copy_from_slice(&bytes[..8]);
            out
        });
        self.state ^= kind ^ (stimulus.duration_ms.to_bits());
        self.mutations += 1;
        self.stimulations += 1;
    }

    /// Installs the next encoded sensory input for the following Prepare.
    fn install_input(&mut self, value: i64) {
        self.input_value = value;
        self.state ^= value as u64;
        self.mutations += 1;
        self.input_installs += 1;
    }

    /// Reinforces once at the current brain time. A zero sum still reinforces: the profile's
    /// legacy-equivalent behaviour is not optimized away without evidence.
    fn reinforce(&mut self, signal: f64) {
        self.last_signal = signal;
        self.reinforcements += 1;
        self.mutations += 1;
        if self.learning_enabled {
            self.learning_updates += 1;
            if signal != 0.0 {
                self.learning_changed += 1;
                self.state ^= signal.to_bits();
            }
        }
    }

    /// The fixed readout: a deterministic decode of the current state, masked by the declared
    /// available actions. It never invents a default winner or changes its own weights.
    fn readout(&self, available: &[String]) -> (bool, bool, f64) {
        // Separate bits of one rotated word, because an LCG's low bits are too regular to
        // read a decision off directly.
        let draw = self.state.rotate_right(29);
        let mut inc = draw & 1 == 1;
        let mut dec = !inc && (draw >> 7) & 1 == 1;
        if !available.iter().any(|a| a == "inc") {
            inc = false;
        }
        if !available.iter().any(|a| a == "dec") {
            dec = false;
        }
        // Exactly representable in f64, so a digest over the decision is stable.
        let bias = (((draw >> 13) % 5) as f64 - 2.0) / 4.0;
        (inc, dec, bias)
    }

    fn telemetry(&self) -> AgentTelemetry {
        let draw = self.state >> 29;
        AgentTelemetry {
            brain_ticks: self.ticks,
            population_rate_hz: (draw % 1000) as f64 / 10.0,
            rates: vec![
                RateSample { role_id: id("kc"), hz: (draw % 700) as f64 / 10.0 },
                RateSample { role_id: id("mbon"), hz: (draw % 310) as f64 / 10.0 },
            ],
            learning: LearningTelemetry {
                enabled: self.learning_enabled,
                updates: self.learning_updates,
                changed: self.learning_changed,
                signal: self.last_signal,
            },
        }
    }
}

/// Deliberate faults a test can ask this worker to produce.
#[derive(Clone, Debug, Default)]
pub struct AgentFaults {
    /// Fail `Agent.Commit` at this step, after the next input was installed, so the coordinator
    /// meets a partially applied mutation rather than a clean refusal.
    pub fail_commit_at_step: Option<u64>,
    /// Hold `Agent.Prepare` open for this long, to reorder completions.
    pub prepare_delay_ms: u64,
    /// Hold `Agent.Commit` open for this long.
    pub commit_delay_ms: u64,
    /// Refuse `State.StageRestore`, so a group install meets one participant that will not
    /// validate while the others already have.
    pub fail_stage_restore: bool,
    /// Refuse `State.ActivateRestore` after this worker has already staged, so a group meets
    /// a failure halfway through activation.
    pub fail_activate_restore: bool,
    /// Add this id to every `Worker.Acknowledge` reply, so the caller meets a worker
    /// reporting about an id it was never asked about.
    pub acknowledge_extra_id: Option<Id>,
}

/// One fake agent worker's configuration.
#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub session_id: Id,
    pub agent_id: Id,
    pub incarnation_id: Id,
    pub tick_duration: RationalNs,
    pub warmup_ticks: u64,
    /// The thread allocation the launcher started this worker within. `workers-v1` requires
    /// `Agent.Initialize`'s `workerThreads` to lie inside it.
    pub worker_threads: usize,
    /// Records every view this agent read, so a test can see which artifact reached it.
    ///
    /// It is this process's log: an agent with a process of its own writes to its own copy,
    /// which the supervisor cannot read. `SessionHarness::sensor_log` says so with `None`.
    pub sensors: crate::media::SensorLog,
    pub faults: AgentFaults,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentPhase {
    Uninitialized,
    Ready(u64),
    Prepared(u64),
    Failed,
}

/// The agent worker endpoint: model, sensor encoder and readout in one bundle.
pub struct FakeAgentWorker {
    config: AgentConfig,
    status: StatusCell,
    phase: AgentPhase,
    epoch: Option<Id>,
    profile: Option<AssetRef>,
    accumulator: Option<TickAccumulator>,
    model: FakeModel,
    context: Option<TypedValue>,
    context_digest: Option<Digest>,
    prepared: Option<(DomainRequestId, PreparedDecision)>,
    /// A validated replacement state that the live session cannot see yet.
    staged: Option<StagedAgent>,
    /// Restore tokens this worker has activated. A token activates once.
    activated: BTreeSet<Id>,
}

impl FakeAgentWorker {
    pub fn new(config: AgentConfig) -> FakeAgentWorker {
        FakeAgentWorker {
            status: StatusCell::new(),
            phase: AgentPhase::Uninitialized,
            epoch: None,
            profile: None,
            accumulator: None,
            model: FakeModel::new(0),
            context: None,
            context_digest: None,
            prepared: None,
            staged: None,
            activated: BTreeSet::new(),
            config,
        }
    }

    /// True while a validated replacement state is staged and not yet activated.
    pub fn has_staged_restore(&self) -> bool {
        self.staged.is_some()
    }

    pub fn status(&self) -> StatusCell {
        self.status.clone()
    }

    fn check_epoch(&self, scope: &Scope) -> DomainResult<()> {
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this worker belongs to another session",
            ));
        }
        match &self.epoch {
            Some(epoch) if *epoch == scope.epoch => Ok(()),
            Some(_) => Err(DomainError::before(
                ErrorCode::StaleEpoch,
                "this scope names an epoch this worker has left",
            )),
            None => Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "this worker is uninitialized",
            )),
        }
    }

    /// Encodes a sensory input into the one scalar the fake model consumes.
    ///
    /// Reading the pixels is what proves the attachment was a live owned handle rather than a
    /// bare reference; a missing required view is an error, never zero input.
    async fn encode(&self, ctx: &HandlerCtx<'_>, input: &SensoryInput) -> DomainResult<i64> {
        input.validate().map_err(DomainError::invalid)?;
        let mut total: i64 = 0;
        for view in &input.views {
            let name = format!("view.{}", view.view_id);
            let artifact = ctx.artifact(&name)?;
            if artifact.reference() != &view.pixels {
                return Err(DomainError::before(
                    ErrorCode::BufferInvalid,
                    format!("attachment {name} is not the artifact the payload names"),
                ));
            }
            let bytes = artifact.read_all().await.map_err(|e| {
                DomainError::before(
                    ErrorCode::BufferInvalid,
                    format!("view {} could not be read: {}", view.view_id, e.message),
                )
            })?;
            if bytes.len() as u64 != view.pixels.byte_length {
                return Err(DomainError::before(
                    ErrorCode::BufferInvalid,
                    format!("view {} is the wrong length", view.view_id),
                ));
            }
            // What this agent read, from the bytes it read: the artifact it was given and the
            // digest of its content.
            self.config.sensors.record(crate::media::SensedView {
                boundary: input.boundary,
                view_id: view.view_id.clone(),
                artifact_id: artifact.reference().artifact_id.clone(),
                produced_step: view.produced_step,
                digest: digest_of_bytes(&bytes),
            });
            total += i64::from(bytes.first().copied().unwrap_or_default());
        }
        if let Some(structured) = &input.structured {
            total += structured.integer("counter").map_err(DomainError::invalid)?;
        }
        Ok(total)
    }

    fn available_actions(context: &TypedValue) -> DomainResult<Vec<String>> {
        if context.schema != context_schema() {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the decision context does not carry the schema this profile allows",
            ));
        }
        let list = context
            .value
            .get("available")
            .and_then(Value::as_array)
            .ok_or_else(|| DomainError::invalid("the decision context declares no actions"))?;
        list.iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| DomainError::invalid("an available action is not a string"))
            })
            .collect()
    }

    fn decision(&self, available: &[String]) -> TypedValue {
        let (inc, dec, bias) = self.model.readout(available);
        let intent = ControllerIntent {
            buttons: vec![
                ButtonState { id: id("inc"), down: inc },
                ButtonState { id: id("dec"), down: dec },
            ],
            axes: vec![AxisValue { id: id("bias"), value: bias }],
        };
        TypedValue::new(decision_schema(), intent.to_json())
            .expect("a direct-control decision fits the contract")
    }

    async fn initialize(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        if self.phase != AgentPhase::Uninitialized {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                "Agent.Initialize is only allowed on an uninitialized agent; restore uses the \
                 state interface",
            ));
        }
        if scope.step != 0 {
            return Err(DomainError::before(
                ErrorCode::FutureStep,
                "Agent.Initialize uses the new epoch at step 0",
            ));
        }
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this worker belongs to another session",
            ));
        }
        let params: AgentInitializeParams = ctx.params()?;
        if params.agent_id != self.config.agent_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Initialize names another agent",
            ));
        }
        if params.worker_threads == 0 {
            return Err(DomainError::invalid("workerThreads must be >= 1"));
        }
        // `workers-v1`: workerThreads is "within launcher allocation". This worker was started
        // with that allocation, so a request for more than it is a capacity refusal made
        // before the model is constructed, not a silent reduction to what is available.
        if params.worker_threads > self.config.worker_threads as u64 {
            return Err(DomainError::before(
                ErrorCode::Busy,
                format!(
                    "Agent.Initialize asks for {} worker threads; the launcher allocated {}",
                    params.worker_threads, self.config.worker_threads
                ),
            ));
        }
        params.initial_decision_context.validate().map_err(DomainError::invalid)?;
        let available = FakeAgentWorker::available_actions(&params.initial_decision_context)?;
        // Everything is validated before the model is constructed.
        let encoded = self.encode(ctx, &params.initial_input).await?;
        if params.initial_input.boundary != 0 {
            return Err(DomainError::invalid("the initial input must observe boundary 0"));
        }

        let mut accumulator =
            TickAccumulator::new(self.config.tick_duration).map_err(DomainError::invalid)?;
        let mut model = FakeModel::new(params.seed);
        model.install_input(encoded);
        // Warm-up runs with learning disabled and produces no gameplay reward or control.
        model.advance(self.config.warmup_ticks);
        accumulator.warm_up(self.config.warmup_ticks).map_err(|e| {
            DomainError::new(ErrorCode::Internal, e, MutationCertainty::Applied)
        })?;
        // Calibration happens on settled rates, after warm-up. The readout is a pure read of
        // the model, so calibrating it mutates nothing.
        let _calibration = model.readout(&available);
        model.learning_enabled = true;

        self.model = model;
        self.accumulator = Some(accumulator);
        self.epoch = Some(scope.epoch.clone());
        self.profile = Some(params.profile.clone());
        self.context_digest = Some(params.initial_decision_context.digest());
        self.context = Some(params.initial_decision_context);
        self.phase = AgentPhase::Ready(0);
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope.clone()));
        self.status.advance_to(self.model.mutations());

        let result = AgentInitializeResult {
            agent_id: self.config.agent_id.clone(),
            profile_digest: params.profile.digest.clone(),
            tick_duration: self.config.tick_duration,
            warmup_ticks: self.config.warmup_ticks,
            committed_step: 0,
            decision_context_digest: self.context_digest.clone().expect("just set"),
            telemetry: self.model.telemetry(),
        };
        Ok(HandlerReply::from(&result))
    }

    async fn prepare(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        self.check_epoch(&scope)?;
        let AgentPhase::Ready(k) = self.phase.clone() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                format!("Agent.Prepare needs Ready(k); this worker is {:?}", self.phase),
            ));
        };
        if scope.step < k {
            return Err(DomainError::before(
                ErrorCode::StaleStep,
                "Agent.Prepare names a step this worker has left",
            ));
        }
        if scope.step > k {
            return Err(DomainError::before(
                ErrorCode::FutureStep,
                "Agent.Prepare names a step beyond this worker's committed boundary",
            ));
        }
        let params: PrepareParams = ctx.params()?;
        if params.agent_id != self.config.agent_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Prepare names another agent",
            ));
        }
        let profile = self.profile.as_ref().expect("initialized");
        if params.profile_digest != profile.digest {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Prepare names another profile",
            ));
        }
        if Some(&params.decision_context_digest) != self.context_digest.as_ref() {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the cached decision context digest does not match",
            ));
        }
        params.interval.validate().map_err(DomainError::invalid)?;
        if params.pre_step_stimulations.len() > MAX_STIMULI {
            return Err(DomainError::invalid("at most 64 pre-step stimulations"));
        }
        for stimulus in &params.pre_step_stimulations {
            stimulus.validate().map_err(DomainError::invalid)?;
        }
        let available =
            FakeAgentWorker::available_actions(self.context.as_ref().expect("initialized"))?;

        self.status.set_state(WorkerState::Preparing);
        if self.config.faults.prepare_delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(
                self.config.faults.prepare_delay_ms,
            ))
            .await;
        }

        // 1. admitted pre-step stimulation, in deterministic command sequence order
        for stimulus in &params.pre_step_stimulations {
            self.model.stimulate(stimulus);
        }
        // 2. advance the numerical model for the environment interval
        let ticks = {
            let accumulator = self.accumulator.as_mut().expect("initialized");
            accumulator.advance(&params.interval).map_err(|e| {
                DomainError::new(ErrorCode::InvalidArgument, e, MutationCertainty::Applied)
            })?
        };
        self.model.advance(ticks);
        // 3. read rates and perform the fixed readout with the declared decision context
        let decision = self.decision(&available);
        let (brain_ticks, remainder) = {
            let accumulator = self.accumulator.as_ref().expect("initialized");
            (accumulator.brain_ticks(), accumulator.remainder())
        };
        let prepared = PreparedDecision {
            agent_id: self.config.agent_id.clone(),
            ticks_advanced: ticks,
            brain_ticks,
            remainder,
            decision,
        };
        self.prepared = Some((ctx.request.request_id.clone(), prepared.clone()));
        self.phase = AgentPhase::Prepared(k);
        self.status.set_state(WorkerState::Prepared);
        self.status.advance_to(self.model.mutations());
        Ok(HandlerReply::from(&prepared))
    }

    async fn commit(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        self.check_epoch(&scope)?;
        let AgentPhase::Prepared(k) = self.phase.clone() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                format!("Agent.Commit needs Prepared(k); this worker is {:?}", self.phase),
            ));
        };
        if scope.step != k {
            return Err(DomainError::before(
                if scope.step < k { ErrorCode::StaleStep } else { ErrorCode::FutureStep },
                "Agent.Commit must carry the step of its transition, not the new boundary",
            ));
        }
        let params: CommitParams = ctx.params()?;
        if params.agent_id != self.config.agent_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Commit names another agent",
            ));
        }
        let (prepared_request, _) = self.prepared.as_ref().expect("prepared");
        if params.prepared_request_id != *prepared_request {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "Agent.Commit does not match this worker's Prepare request",
            ));
        }
        if params.next_input.boundary != k + 1 {
            return Err(DomainError::invalid(
                "the next sensory input must observe boundary k+1",
            ));
        }
        if params.rewards.len() > MAX_REWARDS || params.task_stimulations.len() > MAX_STIMULI
        {
            return Err(DomainError::invalid("at most 64 rewards and 64 stimulations"));
        }
        for reward in &params.rewards {
            reward.validate().map_err(DomainError::invalid)?;
        }
        for stimulus in &params.task_stimulations {
            stimulus.validate().map_err(DomainError::invalid)?;
        }
        params.next_decision_context.validate().map_err(DomainError::invalid)?;
        FakeAgentWorker::available_actions(&params.next_decision_context)?;
        // The complete request and every required owned artifact are validated before anything
        // is applied.
        let encoded = self.encode(ctx, &params.next_input).await?;

        self.status.set_state(WorkerState::Committing);
        if self.config.faults.commit_delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(
                self.config.faults.commit_delay_ms,
            ))
            .await;
        }

        // 1. encode and install the next sensory input for the following Prepare
        self.model.install_input(encoded);
        if self.config.faults.fail_commit_at_step == Some(k) {
            // A deliberate fault after the input was installed: the brain is already mutated,
            // so the coordinator has to recover the group rather than retry this agent.
            self.phase = AgentPhase::Failed;
            self.status.set_state(WorkerState::Failed);
            return Err(DomainError::new(
                ErrorCode::BackendFailure,
                "injected commit failure after the next input was installed",
                MutationCertainty::Applied,
            ));
        }
        // 2. apply task-derived stimulation in returned event order
        for stimulus in &params.task_stimulations {
            self.model.stimulate(stimulus);
        }
        // 3. sum this agent's rewards in returned event order and reinforce once
        let mut signal = 0.0f64;
        for reward in &params.rewards {
            signal += reward.value;
        }
        self.model.reinforce(signal);
        // 4. retain the next decision context and acknowledge boundary k+1
        self.context_digest = Some(params.next_decision_context.digest());
        self.context = Some(params.next_decision_context);
        self.prepared = None;
        self.phase = AgentPhase::Ready(k + 1);
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope_at(&scope.session_id, &scope.epoch, k + 1)));
        self.status.advance_to(self.model.mutations());

        let result = AgentCommitResult {
            agent_id: self.config.agent_id.clone(),
            committed_step: k + 1,
            decision_context_digest: self.context_digest.clone().expect("just set"),
            telemetry: self.model.telemetry(),
        };
        Ok(HandlerReply::from(&result))
    }

    /// The model, for a test that wants its mutation counter directly.
    pub fn model(&self) -> &FakeModel {
        &self.model
    }
}

impl WorkerEndpoint for FakeAgentWorker {
    fn worker_id(&self) -> Id {
        self.config.agent_id.clone()
    }

    fn incarnation_id(&self) -> Id {
        self.config.incarnation_id.clone()
    }

    fn session_id(&self) -> Id {
        self.config.session_id.clone()
    }

    fn role(&self) -> Role {
        Role::Agent
    }

    fn capabilities(&self) -> Vec<Id> {
        vec![
            id("agent-step-v1"),
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

    fn acknowledge_extra_id(&self) -> Option<Id> {
        self.config.faults.acknowledge_extra_id.clone()
    }

    fn methods(&self) -> Vec<&'static str> {
        vec![
            "Agent.Initialize",
            "Agent.Prepare",
            "Agent.Commit",
            "State.Capture",
            "State.StageRestore",
            "State.ActivateRestore",
        ]
    }

    fn handle<'a>(&'a mut self, ctx: HandlerCtx<'a>) -> BoxFuture<'a, DomainResult<HandlerReply>> {
        Box::pin(async move {
            match ctx.method {
                "Agent.Initialize" => self.initialize(&ctx).await,
                "Agent.Prepare" => self.prepare(&ctx).await,
                "Agent.Commit" => self.commit(&ctx).await,
                "State.Capture" => self.state_capture(&ctx).await,
                "State.StageRestore" => self.state_stage_restore(&ctx).await,
                "State.ActivateRestore" => self.state_activate_restore(&ctx).await,
                other => Err(DomainError::before(
                    ErrorCode::Unsupported,
                    format!("{other} is not an agent method"),
                )),
            }
        })
    }
}

/// The retention class table an agent endpoint follows, for a caller that wants it.
pub fn agent_op_class(method: &str) -> Option<OpClass> {
    match method {
        // `ipc-v1` section 5: lifecycle *and capture* replies are retained until
        // `Worker.Acknowledge`, which is also what lets a duplicate restore request replay
        // its cached reply rather than staging or activating twice.
        "Agent.Initialize" | "State.Capture" | "State.StageRestore" | "State.ActivateRestore" => {
            Some(OpClass::Lifecycle)
        }
        "Agent.Prepare" | "Agent.Commit" => Some(OpClass::StepMutation),
        _ => None,
    }
}

/// A synthetic profile asset for one agent. The digest covers its effective identities.
pub fn synthetic_profile(agent_id: &Id, tick_duration: &RationalNs, warmup_ticks: u64) -> AssetRef {
    let text = format!(
        "arena-direct-v1\nagent={agent_id}\ntick={}/{}\nwarmup={warmup_ticks}\n",
        tick_duration.numerator, tick_duration.denominator
    );
    AssetRef {
        id: id("arena-direct-v1"),
        digest: digest_of_bytes(text.as_bytes()),
        byte_length: text.len() as u64,
        format: id("fly-profile-v1"),
    }
}

/// The per-agent contexts a bootstrap produced, keyed by agent id.
pub type Contexts = BTreeMap<Id, TypedValue>;

// -------------------------------------------------------------------------------------------
// STATE-01: capture and restore

/// The numerical model version this worker implements. It is part of a capture's
/// compatibility identity: the same profile and seed under another model is not the same
/// state (`workers-v1` section 2).
pub const MODEL_VERSION: &str = "fake-lcg-v1";

/// The plasticity rule version, for the same reason.
pub const PLASTICITY_VERSION: &str = "fake-reinforce-v1";

/// The version this payload layout is written and read under.
pub const AGENT_PAYLOAD_VERSION: u64 = 1;

/// The dataset identity a synthetic agent resolves.
///
/// There is no connectome dataset behind this worker, and a checkpoint says so with a stable
/// identity rather than omitting the field: "no dataset" has to be distinguishable from "the
/// dataset was not recorded".
pub fn dataset_digest() -> Digest {
    digest_of_bytes(b"fly-session/no-dataset-v1")
}

/// The capture compatibility digest of one agent (`workers-v1` section 2).
///
/// The profile digest identifies the profile definition; this additionally covers the
/// resolved seed, the numerical model version and the plasticity rule, because two agents
/// with the same profile digest and different seeds hold state that is not interchangeable.
/// Every field it covers is one the checkpoint manifest already records in that agent's row,
/// so a restore derives the expected digest from the manifest rather than from the payload it
/// is about to validate.
pub fn agent_compatibility_digest(
    agent_id: &Id,
    profile_digest: &Digest,
    dataset_digest: &Digest,
    model_version: &str,
    plasticity_version: &str,
    seed: i32,
) -> Digest {
    let value = serde_json::json!({
        "agentId": agent_id.as_str(),
        "profileDigest": profile_digest.as_str(),
        "datasetDigest": dataset_digest.as_str(),
        "modelVersion": model_version,
        "plasticityVersion": plasticity_version,
        "seed": seed,
    });
    digest_of(&value).expect("an agent compatibility block canonicalizes")
}

impl FakeModel {
    /// Every field of the model, so a resumed agent is this agent and not a fresh one.
    fn capture(&self) -> Value {
        serde_json::json!({
            "seed": self.seed,
            "state": self.state.to_string(),
            "mutations": self.mutations.to_string(),
            "ticks": self.ticks.to_string(),
            "stimulations": self.stimulations.to_string(),
            "reinforcements": self.reinforcements.to_string(),
            "learningEnabled": self.learning_enabled,
            "learningUpdates": self.learning_updates.to_string(),
            "learningChanged": self.learning_changed.to_string(),
            "lastSignal": self.last_signal,
            "inputValue": self.input_value.to_string(),
            "inputInstalls": self.input_installs.to_string(),
        })
    }

    fn restored(value: &Value) -> DomainResult<FakeModel> {
        let number = |key: &str| -> DomainResult<u64> {
            value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| incompatible(format!("the agent payload has no {key}")))?
                .parse::<u64>()
                .map_err(|_| incompatible(format!("the agent payload's {key} is not a U64")))
        };
        let seed = value
            .get("seed")
            .and_then(Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| incompatible("the agent payload has no seed"))?;
        let input_value = value
            .get("inputValue")
            .and_then(Value::as_str)
            .ok_or_else(|| incompatible("the agent payload has no inputValue"))?
            .parse::<i64>()
            .map_err(|_| incompatible("the agent payload's inputValue is not an integer"))?;
        let last_signal = value
            .get("lastSignal")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .ok_or_else(|| incompatible("the agent payload's lastSignal is not finite"))?;
        let learning_enabled = value
            .get("learningEnabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| incompatible("the agent payload has no learningEnabled"))?;
        Ok(FakeModel {
            seed,
            state: number("state")?,
            mutations: number("mutations")?,
            ticks: number("ticks")?,
            stimulations: number("stimulations")?,
            reinforcements: number("reinforcements")?,
            learning_enabled,
            learning_updates: number("learningUpdates")?,
            learning_changed: number("learningChanged")?,
            last_signal,
            input_value,
            input_installs: number("inputInstalls")?,
        })
    }
}

fn incompatible(message: impl std::fmt::Display) -> DomainError {
    DomainError::before(ErrorCode::IncompatibleState, message)
}

/// One staged restore, held outside the live agent until it is activated.
struct StagedAgent {
    token: Id,
    checkpoint_id: Id,
    scope: Scope,
    model: FakeModel,
    accumulator: TickAccumulator,
    context: TypedValue,
    profile: AssetRef,
    committed_step: u64,
}

impl FakeAgentWorker {
    /// This worker's own compatibility identity, from its configuration and a resolved seed.
    fn compatibility_digest(&self, profile: &AssetRef, seed: i32) -> Digest {
        agent_compatibility_digest(
            &self.config.agent_id,
            &profile.digest,
            &dataset_digest(),
            MODEL_VERSION,
            PLASTICITY_VERSION,
            seed,
        )
    }

    /// `State.Capture`: an immutable snapshot of this agent at its committed boundary.
    ///
    /// It is allowed at `Ready(k)` only. A Prepared agent holds half a transition, and there
    /// is no coherent boundary to file that under.
    async fn state_capture(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        self.check_epoch(&scope)?;
        let AgentPhase::Ready(k) = self.phase.clone() else {
            return Err(DomainError::before(
                ErrorCode::InvalidPhase,
                format!(
                    "State.Capture needs a quiescent Ready(k); this worker is {:?}",
                    self.phase
                ),
            ));
        };
        if scope.step != k {
            return Err(DomainError::before(
                if scope.step < k { ErrorCode::StaleStep } else { ErrorCode::FutureStep },
                "State.Capture names a boundary this worker is not at",
            ));
        }
        let params: CaptureParams = ctx.params()?;
        let profile = self.profile.clone().expect("initialized");
        let context = self.context.clone().expect("initialized");
        let accumulator = self.accumulator.as_ref().expect("initialized");
        let previous = self.status.state();
        self.status.set_state(WorkerState::Capturing);
        let payload = serde_json::json!({
            "payloadVersion": AGENT_PAYLOAD_VERSION,
            "kind": "agent",
            "agentId": self.config.agent_id.as_str(),
            "checkpointId": params.checkpoint_id.as_str(),
            "sourceScope": scope.to_json(),
            "committedStep": k.to_string(),
            "profile": profile.to_json(),
            "modelVersion": MODEL_VERSION,
            "plasticityVersion": PLASTICITY_VERSION,
            "datasetDigest": dataset_digest().as_str(),
            "model": self.model.capture(),
            "accumulator": {
                "tickDuration": accumulator.tick_duration().to_json(),
                "remainder": accumulator.remainder().to_json(),
                "executedTicks": accumulator.executed_ticks().to_string(),
                "warmupOffset": accumulator.warmup_offset().to_string(),
            },
            "context": context.to_json(),
        });
        let bytes = canonicalize(&payload)
            .map_err(|e| DomainError::invalid(format!("State.Capture: {}", e.0)))?
            .into_bytes();
        let digest = digest_of_bytes(&bytes);
        let artifact = crate::state::seal_payload(ctx.client, &bytes, &digest).await?;
        // Capture is a read of the model, not a mutation of it: nothing above changed a
        // counter, and the worker goes back to the boundary it was already at.
        self.status.set_state(previous);
        let result = CaptureResult {
            checkpoint_id: params.checkpoint_id,
            boundary: k,
            compatibility_digest: self.compatibility_digest(&profile, self.model.seed()),
            payload: artifact.reference().clone(),
        };
        Ok(HandlerReply::with_artifacts(
            object(result.to_json()),
            vec![(crate::state::PAYLOAD_ATTACHMENT.to_owned(), artifact)],
        ))
    }

    /// `State.StageRestore`: validate a replacement state into a staging slot.
    ///
    /// Nothing the live session can see changes here, and the worker keeps whatever state it
    /// had. It is allowed on an uninitialized replacement or a quiescent worker only; a
    /// failed one is neither, which is why a group that failed is replaced rather than
    /// reused.
    async fn state_stage_restore(&mut self, ctx: &HandlerCtx<'_>) -> DomainResult<HandlerReply> {
        let scope = ctx.scope()?.clone();
        if scope.session_id != self.config.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "this worker belongs to another session",
            ));
        }
        match &self.phase {
            AgentPhase::Uninitialized | AgentPhase::Ready(_) => {}
            other => {
                return Err(DomainError::before(
                    ErrorCode::InvalidPhase,
                    format!(
                        "State.StageRestore needs an uninitialized replacement or a quiescent \
worker; this worker is {other:?}"
                    ),
                ));
            }
        }
        if let Some(epoch) = &self.epoch
            && *epoch == scope.epoch
        {
            return Err(DomainError::before(
                ErrorCode::StaleEpoch,
                "State.StageRestore proposes the epoch this worker is already running",
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
                .ok_or_else(|| incompatible(format!("the agent payload has no {key}")))
        };
        if value.get("payloadVersion").and_then(Value::as_u64) != Some(AGENT_PAYLOAD_VERSION) {
            return Err(incompatible("the agent payload is another payload version"));
        }
        if text("kind")? != "agent" {
            return Err(incompatible("this payload is not an agent's state"));
        }
        if text("agentId")? != self.config.agent_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the staged payload belongs to another agent",
            ));
        }
        if text("checkpointId")? != params.checkpoint_id {
            return Err(incompatible("the staged payload belongs to another checkpoint"));
        }
        if text("modelVersion")? != MODEL_VERSION || text("plasticityVersion")? != PLASTICITY_VERSION
        {
            return Err(incompatible(
                "the staged payload was captured under another numerical model",
            ));
        }
        let source_scope = Scope::from_json(
            value
                .get("sourceScope")
                .ok_or_else(|| incompatible("the agent payload has no sourceScope"))?,
        )
        .map_err(|e| incompatible(format!("the agent payload's sourceScope: {}", e.0)))?;
        if source_scope != params.source_scope {
            return Err(incompatible(
                "the staged payload was captured at another source scope",
            ));
        }
        let committed_step: u64 = text("committedStep")?
            .parse()
            .map_err(|_| incompatible("the agent payload's committedStep is not a U64"))?;
        if committed_step != params.source_scope.step {
            return Err(incompatible(
                "the staged payload's committed step is not the source boundary",
            ));
        }
        let profile = AssetRef::from_json(
            value
                .get("profile")
                .ok_or_else(|| incompatible("the agent payload has no profile"))?,
        )
        .map_err(|e| incompatible(format!("the agent payload's profile: {}", e.0)))?;
        let model = FakeModel::restored(
            value
                .get("model")
                .ok_or_else(|| incompatible("the agent payload has no model"))?,
        )?;
        // The compatibility digest is recomputed from this worker's own configuration and the
        // identity the payload declares. A capture of the same profile under another seed, or
        // of another agent's brain, fails here and never reaches activation.
        let computed = self.compatibility_digest(&profile, model.seed());
        if computed != params.compatibility_digest {
            return Err(incompatible(format!(
                "the staged state's compatibility {computed} is not the {} the restore \
requires",
                params.compatibility_digest
            )));
        }
        let accumulator_value = value
            .get("accumulator")
            .ok_or_else(|| incompatible("the agent payload has no accumulator"))?;
        let rational = |key: &str| -> DomainResult<RationalNs> {
            RationalNs::from_json(
                accumulator_value
                    .get(key)
                    .ok_or_else(|| incompatible(format!("the accumulator has no {key}")))?,
            )
            .map_err(|e| incompatible(format!("the accumulator's {key}: {}", e.0)))
        };
        let counter = |key: &str| -> DomainResult<u64> {
            accumulator_value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| incompatible(format!("the accumulator has no {key}")))?
                .parse::<u64>()
                .map_err(|_| incompatible(format!("the accumulator's {key} is not a U64")))
        };
        let tick_duration = rational("tickDuration")?;
        if tick_duration != self.config.tick_duration {
            return Err(incompatible(
                "the staged state was captured at another model tick duration",
            ));
        }
        let accumulator = TickAccumulator::restored(
            tick_duration,
            rational("remainder")?,
            counter("executedTicks")?,
            counter("warmupOffset")?,
        )
        .map_err(incompatible)?;
        let context = TypedValue::from_json(
            value
                .get("context")
                .ok_or_else(|| incompatible("the agent payload has no context"))?,
        )
        .map_err(|e| incompatible(format!("the agent payload's context: {}", e.0)))?;
        FakeAgentWorker::available_actions(&context)?;

        if self.config.faults.fail_stage_restore {
            // The row where a group validates three participants and the fourth does not.
            // Nothing is staged here and nothing is staged anywhere else either: the
            // coordinator abandons the whole install.
            return Err(incompatible(
                "injected staging refusal: this participant's replacement state does not \
validate",
            ));
        }
        // One staged restore at a time. A second proposal replaces nothing silently.
        if let Some(staged) = &self.staged {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                format!(
                    "this worker already holds the staged restore {} for checkpoint {}",
                    staged.token, staged.checkpoint_id
                ),
            ));
        }
        let token = restore_token(&params.checkpoint_id, &scope, &actual, &self.config.incarnation_id);
        if self.activated.contains(&token) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                "this exact restore was already activated on this worker",
            ));
        }
        self.staged = Some(StagedAgent {
            token: token.clone(),
            checkpoint_id: params.checkpoint_id.clone(),
            scope: scope.clone(),
            model,
            accumulator,
            context,
            profile,
            committed_step,
        });
        self.status.set_state(WorkerState::StagedRestore);
        let result = StageRestoreResult {
            checkpoint_id: params.checkpoint_id,
            restore_token: token,
        };
        Ok(HandlerReply::from(&result))
    }

    /// `State.ActivateRestore`: install the staged state under its new scope, without a tick.
    ///
    /// The token activates once. A duplicate domain request replays the cached reply through
    /// the shell's result cache; a fresh request naming an already activated token is a
    /// conflict, which is what stops a second group from being resumed from the same bytes.
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
                "this worker holds no staged restore",
            ));
        };
        if staged.token != params.restore_token {
            // Put it back: naming another token is not a reason to discard this one.
            let token = staged.token.clone();
            self.staged = Some(staged);
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!("this worker's staged restore is {token}, not {}", params.restore_token),
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
        let StagedAgent {
            token,
            checkpoint_id,
            scope,
            model,
            accumulator,
            context,
            profile,
            committed_step,
        } = staged;
        self.model = model;
        self.accumulator = Some(accumulator);
        self.context_digest = Some(context.digest());
        self.context = Some(context);
        self.profile = Some(profile);
        self.epoch = Some(scope.epoch.clone());
        self.prepared = None;
        self.phase = AgentPhase::Ready(committed_step);
        self.activated.insert(token);
        self.status.set_state(WorkerState::Ready);
        self.status.set_scope(Some(scope_at(
            &scope.session_id,
            &scope.epoch,
            committed_step,
        )));
        self.status.advance_to(self.model.mutations());
        let result = ActivateRestoreResult {
            committed_step,
            checkpoint_id,
            // An agent returns a null observation; the environment returns the world's.
            observation: None,
        };
        result
            .validate_for_role(Role::Agent)
            .map_err(|e| DomainError::invalid(e.0))?;
        Ok(HandlerReply::from(&result))
    }
}

/// A restore token bound to the checkpoint, the proposed scope, the payload bytes and the
/// worker incarnation staging them.
///
/// `state-media-v1` section 5 binds a token to scope, payload and checkpoint. Binding it to
/// the incarnation as well is what keeps a token minted by a worker that has since been
/// replaced from activating anything on its replacement.
pub fn restore_token(checkpoint_id: &Id, scope: &Scope, payload_digest: &Digest, incarnation: &Id) -> Id {
    let digest = digest_of_bytes(
        format!(
            "fly-session/restore-token-v1\n{checkpoint_id}\n{}\n{}\n{}\n{payload_digest}\n{incarnation}\n",
            scope.session_id, scope.epoch, scope.step
        )
        .as_bytes(),
    );
    parse_id(&format!("rt-{}", &digest[..32])).expect("a hex suffix is an Id")
}
