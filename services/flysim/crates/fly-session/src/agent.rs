//! A small fake agent worker: an explicit seed, a mutation counter the tests read, and a fixed
//! readout stub.
//!
//! There is no neural model here and no attempt to imitate one. What it does model exactly is
//! the *ordering* `workers-v1` section 2 requires: Prepare applies pre-step stimulation, then
//! advances whole ticks, then decodes; Commit installs the next input, then applies task
//! stimulation, then reinforces once, and executes no tick at all. Every mutating step bumps
//! one counter, which is how a test proves a duplicate request changed nothing.

use std::collections::BTreeMap;

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
    /// Which graph this fly built. Two variants have the same `neuronCount` and different
    /// `indexDigest`, which is the case `publishing-v1` section 3 says a consumer must not
    /// mistake for the same mapping.
    pub graph_variant: u64,
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
            config,
        }
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
            // The worker attests to the graph it loaded. A descriptor built from this can
            // disagree with the composition; one built from the composition never could.
            graph: synthetic_graph(&self.config.agent_id, self.config.graph_variant),
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
            check_supported(stimulus)?;
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
            check_supported(stimulus)?;
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
        vec![id("agent-step-v1"), id("pixel-observation-v1")]
    }

    fn status_cell(&self) -> StatusCell {
        self.status.clone()
    }

    fn worker_threads(&self) -> u64 {
        self.config.worker_threads as u64
    }

    fn methods(&self) -> Vec<&'static str> {
        vec!["Agent.Initialize", "Agent.Prepare", "Agent.Commit"]
    }

    fn handle<'a>(&'a mut self, ctx: HandlerCtx<'a>) -> BoxFuture<'a, DomainResult<HandlerReply>> {
        Box::pin(async move {
            match ctx.method {
                "Agent.Initialize" => self.initialize(&ctx).await,
                "Agent.Prepare" => self.prepare(&ctx).await,
                "Agent.Commit" => self.commit(&ctx).await,
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
        "Agent.Initialize" => Some(OpClass::Lifecycle),
        "Agent.Prepare" | "Agent.Commit" => Some(OpClass::StepMutation),
        _ => None,
    }
}

/// A synthetic profile asset for one agent. The digest covers its effective identities.
/// Refuses a stimulus kind this profile does not resolve, before the model is touched.
///
/// `supportedStimuli` in a published descriptor is exactly this list, so the declaration is
/// what the worker enforces rather than a label printed beside it.
fn check_supported(stimulus: &Stimulus) -> DomainResult<()> {
    if SUPPORTED_STIMULI.contains(&stimulus.kind_id.as_str()) {
        return Ok(());
    }
    Err(DomainError::before(
        ErrorCode::Unsupported,
        format!(
            "stimulus kind {} is not one this profile resolves",
            stimulus.kind_id
        ),
    ))
}

/// The rate roles this fake model reports, in the order it reports them.
pub const RATE_ROLES: [&str; 2] = ["kc", "mbon"];

/// The stimulus kinds this synthetic profile resolves. An undeclared kind is refused before
/// the model is touched, so `supportedStimuli` in a descriptor is what the worker enforces
/// rather than a label beside it.
pub const SUPPORTED_STIMULI: [&str; 1] = ["arena.milestone"];

/// This fly's graph identity. Every variant has the same neuron count and its own index, so
/// "the same number of neurons" can never be mistaken for the same mapping.
pub const NEURON_COUNT: u64 = 1024;

pub fn synthetic_graph(agent_id: &Id, variant: u64) -> AgentGraph {
    AgentGraph {
        dataset_digest: digest_of_bytes(
            format!("arena-dataset-v1\nvariant={variant}\n").as_bytes(),
        ),
        index_digest: digest_of_bytes(
            format!(
                "arena-index-v1\nagent={agent_id}\nvariant={variant}\nneurons={NEURON_COUNT}\n"
            )
            .as_bytes(),
        ),
        neuron_count: NEURON_COUNT,
        rate_roles: RATE_ROLES.iter().map(|r| id(r)).collect(),
        supported_stimuli: SUPPORTED_STIMULI.iter().map(|s| id(s)).collect(),
    }
}

pub fn synthetic_profile(agent_id: &Id, tick_duration: &RationalNs, warmup_ticks: u64) -> AssetRef {
    let text = format!(
        "arena-direct-v1\nagent={agent_id}\ntick={}/{}\nwarmup={warmup_ticks}\n",
        tick_duration.numerator, tick_duration.denominator
    );
    AssetRef {
        // One installed asset per fly: a descriptor's `assets` are unique by id, and two
        // profiles that differ in content are two assets, not one id with two digests.
        id: parse_id(&format!("arena-direct-v1-{agent_id}")).expect("a prefix plus an agent id"),
        digest: digest_of_bytes(text.as_bytes()),
        byte_length: text.len() as u64,
        format: id("fly-profile-v1"),
    }
}

/// The per-agent contexts a bootstrap produced, keyed by agent id.
pub type Contexts = BTreeMap<Id, TypedValue>;
