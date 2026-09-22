//! The session coordinator: the transaction of `step-v1` section 3, exactly in order.
//!
//! Phase A prepares every agent concurrently. Phase B runs each executor once in sorted
//! agent-id order, assembles one complete port batch in descriptor order, and sends exactly
//! one `Environment.Advance`. Phase C evaluates the task once. Phase D commits every agent
//! concurrently, and only when all of them have succeeded does the committed boundary move,
//! the snapshot publish and the next Prepare become allowed.
//!
//! There is no implicit best-effort retry here. An uncertain call is resolved against the same
//! domain request id, and anything that cannot be resolved fails the epoch.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::clock::Pacing;
use crate::media::{self, AudioTimelines};
use crate::metrics::Metrics;
use crate::phase::{Phase, PhaseMachine};
use crate::rpc::{self, DomainReply, Serials, WorkerRef};
use crate::task::{ActionExecutor, Task};
// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;

/// The order the coordinator dispatches and awaits its per-agent phases in.
///
/// Completion order never affects port or action order, so all three must produce the same
/// behaviour trace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DispatchOrder {
    /// Dispatch every agent, then await them all.
    #[default]
    Concurrent,
    /// Dispatch and await one agent at a time, in sorted agent-id order.
    Sequential,
    /// Dispatch and await one agent at a time, in reverse sorted agent-id order.
    Reversed,
}

/// Deliberate message-level faults, injected at one step.
#[derive(Clone, Debug, Default)]
pub struct Injections {
    /// The step these injections apply to.
    pub at_step: u64,
    /// Re-send this agent's Prepare on a new bus call with the original request id and body.
    pub duplicate_prepare: Option<Id>,
    /// Re-send this agent's Commit the same way.
    pub duplicate_commit: Option<Id>,
    /// Abandon the first Advance result and resolve the same operation afterwards.
    pub lose_advance_result: bool,
    /// Re-send the Advance with the original request id and an altered control batch.
    pub altered_advance_controls: bool,
    /// Read and release the Advance result's frame, then replay the same operation.
    pub consume_advance_artifact_then_retry: bool,
}

/// What an injection produced, for a test to assert on.
#[derive(Clone, Debug, PartialEq)]
pub struct InjectionOutcome {
    pub what: String,
    pub code: Option<ErrorCode>,
    pub identical: bool,
}

/// A pause request that can be set from outside the transaction.
///
/// A request arriving mid-step means "finish this transition, then pause"; it is read once
/// the committed boundary has moved, never in the middle of one.
#[derive(Clone, Debug)]
pub struct PauseRequest(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl PauseRequest {
    pub fn request(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Why the epoch failed.
#[derive(Clone, Debug)]
pub struct SessionFailure {
    pub error: DomainError,
    pub phase: String,
    pub detail: String,
    /// The participant the failure is attributed to, where one is.
    ///
    /// `step-v1` section 7 stops the epoch rather than neutralising a player, so a diagnosed
    /// outcome has to say which participant it was: the coordinator names it here instead of
    /// leaving a caller to read it out of a message.
    pub participant: Option<Id>,
}

impl std::fmt::Display for SessionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.participant {
            Some(who) => write!(f, "{} at {} ({who}): {}", self.detail, self.phase, self.error),
            None => write!(f, "{} at {}: {}", self.detail, self.phase, self.error),
        }
    }
}

impl std::error::Error for SessionFailure {}

type Outcome<T> = Result<T, SessionFailure>;

/// The caller-side failure-detection budgets of `ipc-v1` section 6, on the coordinator's own
/// monotonic clock.
///
/// Section 6 is a two-stage procedure, and these are its two stages. `probe` is how long a
/// call may go without a terminal reply before it is *uncertain*; an uncertain call is not a
/// failed one, so the coordinator then runs the section 6 resolution -- a fresh bus call
/// carrying the original domain request id and body, pinned to the same incarnation -- for at
/// most `resolve` and at most `resolve_attempts` tries. Only when that ends without a definite
/// answer, or the incarnation is gone, or the retained result expired, is the epoch failed.
///
/// The prototype values follow section 6: probe at two seconds without a reply, give up at
/// ten seconds without progress -- two to notice plus eight to resolve -- with a separate
/// budget for a long boot. They are failure-detection values, not a gameplay latency goal.
///
/// **Which bound ends a resolution.** `resolve` is the one that does, at these values.
/// Between attempts the procedure sleeps [`RESOLVE_PAUSE`], so the fastest the attempt count
/// can be spent is `resolve_attempts * RESOLVE_PAUSE`; the default 8192 attempts is over
/// sixteen seconds of pauses alone, twice the eight-second budget, and an attempt whose call
/// expires costs a whole `probe` on top. `resolve_attempts` is therefore a second, coarser
/// stop for a pathological loop that costs nothing per turn, not the working limit. Both are
/// explicit because section 6 forbids filling this gap with an implicit best-effort policy,
/// and [`ResolutionEnd`] says which of them fired.
#[derive(Clone, Copy, Debug)]
pub struct Deadlines {
    /// Without a terminal reply for this long, the call is uncertain.
    pub probe: Duration,
    /// The resolution's own budget, measured from its first attempt. The working limit.
    pub resolve: Duration,
    /// How many times the resolution may re-ask, as a guard rather than the working limit.
    /// Never a retry of the operation: every attempt carries the original request id and body.
    pub resolve_attempts: u32,
    /// A separate, larger budget for `Worker.Hello` and the `Initialize` methods.
    pub boot: Duration,
    /// A separate budget for the `State.*` methods, which `ipc-v1` section 6 gives one:
    /// a capture serializes a participant and a restore validates and installs one, and
    /// neither is a step whose latency the probe was chosen for.
    pub capture: Duration,
    /// How long a caller waits for a *durable* acknowledgment.
    ///
    /// It is not [`Deadlines::capture`]: that one bounds a call to a participant, and this
    /// one bounds two `fsync`s, a queue the caller shares with other captures and a disk.
    /// Reusing the call budget here would make a slow disk look like an unresponsive worker.
    pub durable: Duration,
}

/// How long the resolution waits between attempts.
pub const RESOLVE_PAUSE: Duration = Duration::from_millis(2);

/// What ended a resolution, so a caller can tell an exhausted budget from an exhausted
/// attempt count rather than reading one number out of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionEnd {
    /// A matching terminal result arrived.
    Answered,
    /// The `resolve` budget ran out. At the default values this is the one that fires.
    BudgetExpired,
    /// The `resolve_attempts` guard ran out first, which needs a pause short enough or an
    /// attempt count low enough for it to be reached before the budget.
    AttemptsExhausted,
}

impl Default for Deadlines {
    fn default() -> Deadlines {
        Deadlines {
            probe: Duration::from_secs(2),
            resolve: Duration::from_secs(8),
            // Over sixteen seconds of pauses: the budget above is what terminates.
            resolve_attempts: 8192,
            boot: Duration::from_secs(30),
            capture: Duration::from_secs(30),
            durable: Duration::from_secs(60),
        }
    }
}

/// The bus addresses this session publishes on. Chosen by the composition, not the router.
#[derive(Clone, Debug)]
pub struct Topics {
    pub descriptor: String,
    pub snapshots: String,
    pub events: String,
    /// Where the distinct captured/queued/committed/failed/superseded checkpoint events go.
    pub checkpoints: String,
}

impl Topics {
    pub fn for_session(session_id: &Id) -> Topics {
        Topics {
            descriptor: format!("session.{session_id}.descriptor"),
            snapshots: format!("session.{session_id}.snapshots"),
            events: format!("session.{session_id}.events"),
            checkpoints: format!("session.{session_id}.checkpoints"),
        }
    }
}

/// One agent's session-side record: its port, its profile, its retained context.
pub struct AgentSlot {
    pub worker: WorkerRef,
    pub agent_id: Id,
    pub port_id: Id,
    pub profile: AssetRef,
    pub seed: i32,
    /// The thread allocation the launcher gave this agent, which is what `Agent.Initialize`
    /// asks for. It is within the launcher allocation by construction.
    pub worker_threads: u64,
    pub tick_duration: RationalNs,
    pub warmup_ticks: u64,
    pub committed_step: u64,
    /// The tick count and remainder this agent last reported, which are what the checkpoint
    /// manifest records for it. They are metadata about the payload, never a substitute for
    /// it: the agent's own capture is the state that is restored.
    pub brain_ticks: u64,
    pub remainder: RationalNs,
    context: TypedValue,
    context_digest: Digest,
    prepared: Option<PreparedDecision>,
    prepare_request: Option<DomainRequestId>,
}

impl AgentSlot {
    pub fn new(
        worker: WorkerRef,
        agent_id: Id,
        port_id: Id,
        profile: AssetRef,
        seed: i32,
    ) -> AgentSlot {
        AgentSlot {
            worker,
            agent_id,
            port_id,
            profile,
            seed,
            worker_threads: 1,
            tick_duration: RationalNs::ZERO,
            warmup_ticks: 0,
            committed_step: 0,
            brain_ticks: 0,
            remainder: RationalNs::ZERO,
            context: TypedValue::new(crate::task::context_schema(), Value::Object(Map::new()))
                .expect("an empty context object is a valid typed value"),
            context_digest: digest_of_bytes(b""),
            prepared: None,
            prepare_request: None,
        }
    }
}

/// The session's counters, for the acceptance assertions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub advances: u64,
    pub publications: u64,
    pub prepares: u64,
    pub commits: u64,
}

pub struct Coordinator {
    bus: flybus::Client,
    session_id: Id,
    epoch: Id,
    episode_id: Id,
    phases: PhaseMachine,
    agents: Vec<AgentSlot>,
    environment: WorkerRef,
    descriptor: Option<EnvironmentDescriptor>,
    task: Box<dyn Task>,
    executors: BTreeMap<Id, Box<dyn ActionExecutor>>,
    observation: Option<WorldObservation>,
    /// Explicit holds on the current boundary's views, forwarded to every Commit and to
    /// publication and dropped once the boundary is committed.
    views: BTreeMap<String, flybus::Artifact>,
    /// Holds on the boundary the world has just reached, before it is committed.
    pending_views: BTreeMap<String, flybus::Artifact>,
    /// The same handles for this boundary's audio chunks. Audio is presentation data: it is
    /// published and never attached to an agent's sensory input.
    audio: BTreeMap<String, flybus::Artifact>,
    pending_audio: BTreeMap<String, flybus::Artifact>,
    /// One chunk sequence per declared audio stream, for this epoch.
    timelines: AudioTimelines,
    /// The attachment names this session's native media travels under. The composition
    /// supplies them for Initialize; afterwards they come from the environment descriptor.
    media_names: Vec<String>,
    serials: Serials,
    topics: Topics,
    pacing: Option<Pacing>,
    /// Set by whoever asks for a normal pause, possibly while a transition is in flight.
    pause: std::sync::Arc<std::sync::atomic::AtomicBool>,
    episode: Option<EpisodeRequest>,
    lifecycle_acks: Vec<(WorkerRef, DomainRequestId)>,
    stats: Stats,
    /// The ordered actions this session took, for the ordering assertions.
    pub audit: Vec<String>,
    pub trace: TraceLog,
    pub dispatch: DispatchOrder,
    pub injections: Injections,
    pub injection_log: Vec<InjectionOutcome>,
    /// How many times an exact duplicate met IN_PROGRESS while resolving an uncertain call.
    pub in_progress_replies: u64,
    /// How many uncertain calls ran the `ipc-v1` section 6 resolution.
    pub resolutions: u64,
    /// How the last resolution ended, so a test or a supervisor can tell which bound fired.
    pub last_resolution: Option<ResolutionEnd>,
    /// How many attempts the last resolution spent. Counted, not inferred from the clock.
    pub last_resolution_attempts: u32,
    /// The caller-side failure-detection budgets of `ipc-v1` section 6.
    pub deadlines: Deadlines,
    /// Per-method and critical-path latency samples. Local synthetic timings, never a
    /// capacity claim.
    pub metrics: Metrics,
    /// The participant the next failure is attributed to, set around each call to one.
    blame: Option<Id>,
    /// Set when the epoch failed: every old handle, route and reply is invalid from here on
    /// and only a coherent restore may lift it.
    fenced: bool,
    /// The durable store's bounded writer, when this composition has one.
    writer: Option<crate::state::CheckpointWriter>,
    /// The last checkpoint whose saved acknowledgment arrived, and its boundary.
    durable: Option<(Id, u64)>,
    /// Participants that installed a restore a group install then abandoned.
    tainted: BTreeSet<Id>,
    started: std::time::Instant,
    last_advance_request: Option<DomainRequestId>,
    last_commit_requests: Vec<TraceRequest>,
}

impl Coordinator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bus: flybus::Client,
        session_id: Id,
        epoch: Id,
        episode_id: Id,
        environment: WorkerRef,
        agents: Vec<AgentSlot>,
        task: Box<dyn Task>,
        executors: BTreeMap<Id, Box<dyn ActionExecutor>>,
    ) -> Coordinator {
        let mut agents = agents;
        // Sorted agent-id order is the executor and control order, so it is fixed here once.
        agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        let topics = Topics::for_session(&session_id);
        Coordinator {
            bus,
            session_id,
            epoch,
            episode_id,
            phases: PhaseMachine::new(),
            agents,
            environment,
            descriptor: None,
            task,
            executors,
            observation: None,
            views: BTreeMap::new(),
            pending_views: BTreeMap::new(),
            audio: BTreeMap::new(),
            pending_audio: BTreeMap::new(),
            timelines: AudioTimelines::default(),
            media_names: vec![
                media::view_attachment(crate::environment::VIEW_ID),
                media::audio_attachment(crate::environment::AUDIO_STREAM_ID),
            ],
            serials: Serials::default(),
            topics,
            pacing: None,
            pause: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            episode: None,
            lifecycle_acks: Vec::new(),
            stats: Stats::default(),
            audit: Vec::new(),
            trace: TraceLog::default(),
            dispatch: DispatchOrder::default(),
            injections: Injections::default(),
            injection_log: Vec::new(),
            in_progress_replies: 0,
            resolutions: 0,
            last_resolution: None,
            last_resolution_attempts: 0,
            deadlines: Deadlines::default(),
            metrics: Metrics::default(),
            blame: None,
            fenced: false,
            writer: None,
            durable: None,
            tainted: BTreeSet::new(),
            started: std::time::Instant::now(),
            last_advance_request: None,
            last_commit_requests: Vec::new(),
        }
    }

    pub fn phase(&self) -> Phase {
        self.phases.phase()
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn topics(&self) -> &Topics {
        &self.topics
    }

    pub fn epoch(&self) -> &Id {
        &self.epoch
    }

    pub fn descriptor(&self) -> Option<&EnvironmentDescriptor> {
        self.descriptor.as_ref()
    }

    pub fn observation(&self) -> Option<&WorldObservation> {
        self.observation.as_ref()
    }

    /// The media handles this committed boundary holds: the one the agents were given and the
    /// one presentation was published. They are the same objects, named by attachment.
    pub fn media_handles(&self) -> Vec<(String, ArtifactRef)> {
        self.views
            .iter()
            .chain(self.audio.iter())
            .map(|(name, artifact)| (name.clone(), artifact.reference().clone()))
            .collect()
    }

    /// Where each declared audio stream's next chunk may start.
    pub fn audio_positions(&self) -> BTreeMap<String, u64> {
        self.timelines.positions()
    }

    pub fn episode_request(&self) -> Option<&EpisodeRequest> {
        self.episode.as_ref()
    }

    /// The committed boundary, when the session is at one.
    pub fn committed_boundary(&self) -> Option<u64> {
        self.phases.phase().committed_boundary()
    }

    pub fn agent_ids(&self) -> Vec<Id> {
        self.agents.iter().map(|a| a.agent_id.clone()).collect()
    }

    /// How many times the task evaluated a transition.
    pub fn evaluations(&self) -> u64 {
        self.task.evaluations()
    }

    pub fn task_progress(&self) -> TypedValue {
        self.task.progress()
    }

    /// "Finish this transition, then pause." It never truncates a transition.
    pub fn request_pause(&mut self) {
        self.pause.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// A handle a supervisor can use to request a pause while a transition is in flight.
    pub fn pause_handle(&self) -> PauseRequest {
        PauseRequest(self.pause.clone())
    }

    pub fn pause_requested(&self) -> bool {
        self.pause.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Stops pacing to the wall clock, so transitions follow one another as fast as the
    /// participants answer.
    ///
    /// Only one pacing authority is ever active; this removes the coordinator's. It is for a
    /// measurement run, where a 60 Hz sleep would be most of every sample and none of it the
    /// thing being compared. A session that presents to anyone keeps its pacing.
    pub fn disable_pacing(&mut self) {
        self.pacing = None;
    }

    /// Leaves a normal pause at its committed boundary.
    pub fn resume(&mut self) -> Outcome<()> {
        let Phase::Paused(k) = self.phases.phase() else {
            return Err(self.fail_now(
                DomainError::before(ErrorCode::InvalidPhase, "the session is not paused"),
                "resume",
            ));
        };
        self.transition(Phase::Ready(k))?;
        Ok(())
    }

    fn scope(&self, step: u64) -> Scope {
        scope_at(&self.session_id, &self.epoch, step)
    }

    fn transition(&mut self, next: Phase) -> Outcome<()> {
        match self.phases.to(next) {
            Ok((from, to)) => {
                self.trace.phase(from, to);
                Ok(())
            }
            Err(error) => Err(self.fail_now(error, "phase")),
        }
    }

    /// Fails the epoch and records the transition to Failed.
    ///
    /// The epoch is fenced at the same moment: the session's routes, pinned registrations and
    /// artifact handles are no longer valid, and nothing but a coherent restore into a new
    /// epoch may lift that.
    fn fail_now(&mut self, error: DomainError, detail: &str) -> SessionFailure {
        let phase = self.phases.phase().label();
        let participant = self.blame.take();
        let (from, to) = self.phases.fail();
        self.trace.phase(from, to);
        self.fenced = true;
        // Whatever replies this session still owed an acknowledgment for belong to
        // participants of an invalid epoch. Carrying them across a recovery would send
        // `Worker.Acknowledge` to a registration that is gone.
        self.lifecycle_acks.clear();
        // Every handle this session held on the old epoch's media goes with the fence: a
        // recovery imports fresh artifacts and never expects one of these back.
        self.views.clear();
        self.pending_views.clear();
        self.audio.clear();
        self.pending_audio.clear();
        match &participant {
            Some(who) => self.audit.push(format!("fail:{detail}:{who}")),
            None => self.audit.push(format!("fail:{detail}")),
        }
        SessionFailure { error, phase, detail: detail.to_owned(), participant }
    }

    /// Names the participant the next failure belongs to.
    fn blame(&mut self, who: Option<Id>) {
        self.blame = who;
    }

    /// True once the epoch has failed. Old handles and routes are invalid; the session takes
    /// no further step and publishes nothing.
    ///
    /// Lifting the fence is STATE-01's: a group restore into a fresh epoch from a coherent
    /// checkpoint. This slice only establishes it.
    pub fn is_fenced(&self) -> bool {
        self.fenced
    }

    /// How many artifact handles this session still owns: the committed boundary's views plus
    /// any the world has produced but not yet committed.
    ///
    /// A fence drops both sets, which is what "old handles are invalid" means on this side of
    /// the router. It is exposed so that can be asserted rather than described.
    pub fn live_view_handles(&self) -> usize {
        self.views.len() + self.pending_views.len()
    }

    fn agent(&self, agent_id: &Id) -> Option<&AgentSlot> {
        self.agents.iter().find(|a| a.agent_id == *agent_id)
    }

    // ---------------------------------------------------------------------------------------
    // Initialization

    /// Negotiates with every worker, initializes the environment and then the agents, and
    /// establishes Ready(0).
    ///
    /// Nothing here advances the environment or produces a gameplay reward.
    pub async fn bootstrap(&mut self) -> Outcome<()> {
        if self.fenced {
            // Defence in depth: the phase machine refuses `Failed -> Ready(0)` anyway, but a
            // fenced session should not be sending Hello and Initialize to live workers on the
            // way to finding that out.
            return Err(SessionFailure {
                error: DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the epoch is fenced; only a coherent restore resumes play",
                ),
                phase: self.phases.phase().label(),
                detail: "fenced".to_owned(),
                participant: None,
            });
        }
        self.hello_environment().await?;
        for index in 0..self.agents.len() {
            self.hello_agent(index).await?;
        }
        self.declare_topics().await?;
        self.initialize_environment().await?;
        self.bootstrap_task()?;
        for index in 0..self.agents.len() {
            self.initialize_agent(index).await?;
        }
        self.acknowledge_lifecycle().await?;
        self.transition(Phase::Ready(0))?;
        let pacing = self
            .descriptor
            .as_ref()
            .map(|descriptor| Pacing::new(descriptor.step_duration));
        self.pacing = pacing;
        self.publish_descriptor().await?;
        self.publish_snapshot(0, &BTreeMap::new(), &[], &[]).await?;
        Ok(())
    }

    async fn hello(&mut self, worker: WorkerRef, role: Role, required: &str) -> Outcome<Id> {
        let params = HelloParams {
            session_id: self.session_id.clone(),
            expected_worker_id: worker.worker_id.clone(),
            role,
            supported_majors: vec![1],
        };
        let reply = self
            .call(&worker, "Worker.Hello", None, object(params.to_json()), &[], &[])
            .await?;
        let result: HelloResult = reply.parse().map_err(|e| self.fail_now(e, "hello"))?;
        if result.contract_digest != contract_digest() {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the worker implements another contract revision",
                ),
                "hello",
            ));
        }
        if result.role != role || result.worker_id != worker.worker_id {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the worker is not the role or the worker the composition expected",
                ),
                "hello",
            ));
        }
        let required = id(required);
        if !result.capabilities.contains(&required) {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::Unsupported,
                    format!("the worker does not advertise {required}"),
                ),
                "hello",
            ));
        }
        self.audit.push(format!("hello:{}", worker.worker_id));
        Ok(result.incarnation_id)
    }

    async fn hello_environment(&mut self) -> Outcome<()> {
        let worker = self.environment.clone();
        let incarnation = self.hello(worker, Role::Environment, "world-step-v1").await?;
        self.environment.domain_incarnation = Some(incarnation);
        Ok(())
    }

    async fn hello_agent(&mut self, index: usize) -> Outcome<()> {
        let worker = self.agents[index].worker.clone();
        let incarnation = self.hello(worker, Role::Agent, "agent-step-v1").await?;
        self.agents[index].worker.domain_incarnation = Some(incarnation);
        Ok(())
    }

    async fn declare_topics(&mut self) -> Outcome<()> {
        for (name, retained) in [
            (self.topics.descriptor.clone(), flybus::Retained::Latest),
            (self.topics.snapshots.clone(), flybus::Retained::Latest),
            (self.topics.events.clone(), flybus::Retained::None),
            // Checkpoint events are a stream of distinct facts, not a latest value: a
            // "committed" that replaced a "queued" would erase the distinction the durable
            // commit rules are built on.
            (self.topics.checkpoints.clone(), flybus::Retained::None),
        ] {
            self.bus.declare_topic(&name, retained).await.map_err(|e| {
                let error = DomainError::new(
                    ErrorCode::BackendFailure,
                    format!("declaring {name}: {}", e.message),
                    MutationCertainty::None,
                );
                self.fail_now(error, "declare-topic")
            })?;
        }
        Ok(())
    }

    async fn initialize_environment(&mut self) -> Outcome<()> {
        let bindings: Vec<PortBinding> = self
            .agents
            .iter()
            .map(|a| PortBinding { port_id: a.port_id.clone(), agent_id: a.agent_id.clone() })
            .collect();
        let params = EnvironmentInitializeParams {
            backend_config: crate::environment::synthetic_asset(
                "counter-arena-backend",
                "counter-arena-backend-v1",
            ),
            task_config: crate::environment::synthetic_asset(
                "counter-arena-setup",
                "counter-arena-setup-v1",
            ),
            episode_id: self.episode_id.clone(),
            port_bindings: bindings.iter().map(PortBinding::pair).collect(),
        };
        let worker = self.environment.clone();
        let scope = self.scope(0);
        let want = self.media_names.clone();
        let reply = self
            .call(
                &worker,
                "Environment.Initialize",
                Some(scope),
                object(params.to_json()),
                &[],
                &want,
            )
            .await?;
        let result: EnvironmentInitializeResult =
            reply.parse().map_err(|e| self.fail_now(e, "environment-initialize"))?;
        result
            .descriptor
            .validate()
            .map_err(|e| self.fail_now(DomainError::invalid(e), "environment-descriptor"))?;
        result
            .observation
            .validate_against(&result.descriptor)
            .map_err(|e| self.fail_now(DomainError::invalid(e), "observation-0"))?;
        if result.observation.boundary != 0 || !result.observation.world_time.is_zero() {
            return Err(self.fail_now(
                DomainError::invalid("boundary 0 must have world time zero"),
                "observation-0",
            ));
        }
        // Every port the descriptor declares must be bound to a configured agent, and every
        // agent's port must exist.
        let declared: BTreeSet<Id> =
            result.descriptor.ports.iter().map(|p| p.port_id.clone()).collect();
        let assigned: BTreeSet<Id> = self.agents.iter().map(|a| a.port_id.clone()).collect();
        if declared != assigned {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the descriptor's ports and the composition's port assignment disagree",
                ),
                "port-assignment",
            ));
        }
        let (views, audio) = media::split_attachments(reply.artifacts);
        self.views = views;
        self.audio = audio;
        self.media_names = media::attachment_names(&result.descriptor);
        self.timelines = AudioTimelines::fresh(&result.descriptor);
        if let Err(e) = self.timelines.accept(&result.descriptor, &result.observation) {
            return Err(self.fail_now(e, "observation-0"));
        }
        if let Err(e) = media::check_required_views(&result.descriptor, &result.observation) {
            return Err(self.fail_now(e, "observation-0"));
        }
        // O[0] ran no transition either, so it carries no chunk, and that is checked rather
        // than assumed from its boundary number.
        if let Err(e) = media::check_required_audio(
            &result.descriptor,
            &result.observation,
            media::ObservationOrigin::Installed,
        ) {
            return Err(self.fail_now(e, "observation-0"));
        }
        self.descriptor = Some(result.descriptor);
        self.observation = Some(result.observation);
        self.lifecycle_acks.push((worker, reply.request_id.clone()));
        self.audit.push("environment.initialize".to_owned());
        Ok(())
    }

    fn bootstrap_task(&mut self) -> Outcome<()> {
        let observation = self.observation.clone().expect("initialized");
        let bindings: Vec<PortBinding> = self
            .agents
            .iter()
            .map(|a| PortBinding { port_id: a.port_id.clone(), agent_id: a.agent_id.clone() })
            .collect();
        let bootstrap = self
            .task
            .bootstrap(&observation.inspection, &bindings)
            .map_err(|e| self.fail_now(e, "task-bootstrap"))?;
        for event in &bootstrap.events {
            if event.source_step != 0 {
                return Err(self.fail_now(
                    DomainError::invalid("a bootstrap event has source step 0"),
                    "task-bootstrap",
                ));
            }
        }
        for index in 0..self.agents.len() {
            let agent_id = self.agents[index].agent_id.clone();
            let context = bootstrap.contexts.get(&agent_id).cloned().ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("the task bootstrapped no context for {agent_id}"),
                )
            });
            let context = match context {
                Ok(context) => context,
                Err(e) => return Err(self.fail_now(e, "task-bootstrap")),
            };
            self.agents[index].context_digest = context.digest();
            self.agents[index].context = context;
        }
        self.audit.push("task.bootstrap".to_owned());
        Ok(())
    }

    async fn initialize_agent(&mut self, index: usize) -> Outcome<()> {
        let observation = self.observation.clone().expect("initialized");
        let slot_worker = self.agents[index].worker.clone();
        let params = AgentInitializeParams {
            agent_id: self.agents[index].agent_id.clone(),
            profile: self.agents[index].profile.clone(),
            seed: self.agents[index].seed,
            initial_input: self.sensory_input(&observation, 0),
            initial_decision_context: self.agents[index].context.clone(),
            // The launcher's allocation for this agent. `workers-v1` requires it to lie
            // within that allocation, and the worker refuses anything larger.
            worker_threads: self.agents[index].worker_threads,
        };
        let attachments = self.view_attachments();
        let scope = self.scope(0);
        let reply = {
            let refs: Vec<(&str, &flybus::Artifact)> =
                attachments.iter().map(|(n, a)| (n.as_str(), a)).collect();
            self.call(&slot_worker, "Agent.Initialize", Some(scope), object(params.to_json()), &refs, &[])
                .await?
        };
        let result: AgentInitializeResult =
            reply.parse().map_err(|e| self.fail_now(e, "agent-initialize"))?;
        if result.committed_step != 0 {
            return Err(self.fail_now(
                DomainError::invalid("Agent.Initialize must establish committed step 0"),
                "agent-initialize",
            ));
        }
        if result.profile_digest != self.agents[index].profile.digest {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the agent initialized another profile",
                ),
                "agent-initialize",
            ));
        }
        if result.decision_context_digest != self.agents[index].context_digest {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the agent retained another decision context",
                ),
                "agent-initialize",
            ));
        }
        result
            .telemetry
            .validate()
            .map_err(|e| self.fail_now(DomainError::invalid(e), "agent-initialize"))?;
        self.agents[index].tick_duration = result.tick_duration;
        self.agents[index].warmup_ticks = result.warmup_ticks;
        self.agents[index].committed_step = 0;
        // At boundary 0 the agent has executed exactly its warm-up, with no interval
        // consumed, so the remainder is zero.
        self.agents[index].brain_ticks = result.telemetry.brain_ticks;
        self.agents[index].remainder = RationalNs::ZERO;
        self.lifecycle_acks.push((slot_worker, reply.request_id.clone()));
        let agent_id = self.agents[index].agent_id.clone();
        self.audit.push(format!("agent.initialize:{agent_id}"));
        Ok(())
    }

    /// Releases every lifecycle reply the workers are holding for us.
    ///
    /// This is the domain acknowledgment of `ipc-v1` section 5, which drops a worker's result
    /// cache. It is not a bus `delivery.consumed`, which the SDK did when we dropped the
    /// replies.
    async fn acknowledge_lifecycle(&mut self) -> Outcome<()> {
        let mut by_worker: BTreeMap<String, (WorkerRef, Vec<DomainRequestId>)> = BTreeMap::new();
        for (worker, request_id) in std::mem::take(&mut self.lifecycle_acks) {
            by_worker
                .entry(worker.service.clone())
                .or_insert_with(|| (worker.clone(), Vec::new()))
                .1
                .push(request_id);
        }
        for (worker, ids) in by_worker.into_values() {
            self.acknowledge_replies(&worker, &ids).await?;
        }
        self.audit.push("acknowledge.lifecycle".to_owned());
        Ok(())
    }

    /// Releases a worker's retained lifecycle replies, and accepts a short answer.
    ///
    /// **`ipc-v1` section 5: "Already released/unknown IDs are ignored."** The reply lists what
    /// *this* call released, which is not always everything it asked about, and the contract
    /// type already holds that list to a subset of the request. So a second Acknowledge of the
    /// same ids answers with an empty list by design, and an empty list is success.
    ///
    /// This matters beyond tidiness. The `ipc-v1` section 6 resolution turns any Acknowledge
    /// whose reply is slower than the probe into a second Acknowledge of the same ids, so the
    /// short answer is not an edge case -- it is what the contract produces on an ordinarily
    /// slow worker. Requiring the whole list back made the contract's own idempotence a failed
    /// epoch, which is what
    /// `an_acknowledge_that_releases_nothing_is_not_a_failure` guards against.
    ///
    /// Returns the ids the worker actually released.
    pub async fn acknowledge_replies(
        &mut self,
        worker: &WorkerRef,
        request_ids: &[DomainRequestId],
    ) -> Outcome<Vec<DomainRequestId>> {
        let params = AcknowledgeParams { request_ids: request_ids.to_vec() };
        let reply = self
            .call(worker, "Worker.Acknowledge", None, object(params.to_json()), &[], &[])
            .await?;
        let result: AcknowledgeResult =
            reply.parse().map_err(|e| self.fail_now(e, "acknowledge"))?;
        Ok(result.acknowledged)
    }

    /// Queries one worker's status without waiting for its current mutation.
    pub async fn status(&mut self, worker: &WorkerRef) -> Outcome<StatusResult> {
        let reply = self
            .call(worker, "Worker.Status", None, Map::new(), &[], &[])
            .await?;
        reply.parse().map_err(|e| self.fail_now(e, "status"))
    }

    /// Asks one worker to stop. Only the configured supervisor may do this.
    pub async fn shutdown(&mut self, worker: &WorkerRef, reason: &str) -> Outcome<()> {
        let params = ShutdownParams { reason: id(reason) };
        let reply = self
            .call(worker, "Worker.Shutdown", None, object(params.to_json()), &[], &[])
            .await?;
        // A responsive worker reports `stopping: true`, which is the only shape the contract
        // type reads at all.
        let _: ShutdownResult = reply.parse().map_err(|e| self.fail_now(e, "shutdown"))?;
        Ok(())
    }

    pub fn environment_ref(&self) -> &WorkerRef {
        &self.environment
    }

    pub fn agent_ref(&self, agent_id: &Id) -> Option<&WorkerRef> {
        self.agent(agent_id).map(|slot| &slot.worker)
    }
}

// -------------------------------------------------------------------------------------------
// One transaction

/// What one completed transition reports back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepReport {
    /// The new committed boundary.
    pub boundary: u64,
    /// True when the session is now paused at that boundary.
    pub paused: bool,
    /// True when the task asked for a terminal episode transition.
    pub terminal: bool,
}

/// One per-agent bus call, ready to dispatch.
struct Job {
    agent_id: Id,
    worker: WorkerRef,
    method: &'static str,
    scope: Option<Scope>,
    params: Map<String, Value>,
    attachments: Vec<(String, flybus::Artifact)>,
    request_id: DomainRequestId,
}

/// Issues one domain call with owned arguments, so it can run in its own task.
///
/// What one bus call came back as.
///
/// The expiry is its own variant rather than an error, because `ipc-v1` section 6 treats the
/// two differently: a refusal is an answer and can fail the epoch, while an expired deadline
/// is only an *uncertain* call and owes the resolution procedure first. Collapsing them into
/// one error is how a merely slow participant loses an epoch.
enum CallOutcome {
    // Boxed: a `DomainReply` carries its artifact handles, and the other two variants are a
    // unit and one error. Without the box every caller's `Result` is sized for the reply.
    Answered(Box<DomainReply>),
    /// The caller's deadline expired with no terminal reply.
    Expired,
    /// The bus or the reply itself refused. This is an answer, even when it is a bad one.
    Refused(DomainError),
}

/// Issues one domain call with owned arguments, so it can run in its own task.
///
/// The deadline is the caller's, on the caller's monotonic clock. A participant that has died
/// mid-call is usually reported by the bus itself, because its connection took its
/// registration with it; this bound is what makes the remaining cases -- a live process that
/// stopped answering -- a diagnosed outcome rather than a hang.
#[allow(clippy::too_many_arguments)]
async fn call_owned(
    bus: flybus::Client,
    worker: WorkerRef,
    method: &'static str,
    scope: Option<Scope>,
    params: Map<String, Value>,
    attachments: Vec<(String, flybus::Artifact)>,
    request_id: DomainRequestId,
    want: Vec<String>,
    deadline: Duration,
) -> CallOutcome {
    let refs: Vec<(&str, &flybus::Artifact)> =
        attachments.iter().map(|(n, a)| (n.as_str(), a)).collect();
    let call = rpc::call(&bus, &worker, method, scope, params, &refs, request_id, &want);
    match tokio::time::timeout(deadline, call).await {
        Ok(Ok(reply)) => CallOutcome::Answered(Box::new(reply)),
        Ok(Err(e)) => CallOutcome::Refused(e),
        Err(_) => CallOutcome::Expired,
    }
}

/// The error an unresolved uncertain call finally becomes.
///
/// Deliberately `unknown`: `ipc-v1` section 6 forbids reading a caller-side timeout as proof
/// that nothing was mutated.
fn unresolved(method: &str, worker: &WorkerRef, end: ResolutionEnd, bound: &str) -> DomainError {
    let why = match end {
        ResolutionEnd::BudgetExpired => "its resolution budget",
        ResolutionEnd::AttemptsExhausted => "its resolution attempt guard",
        ResolutionEnd::Answered => "its resolution",
    };
    DomainError::new(
        ErrorCode::BackendFailure,
        format!(
            "{method}: {} never resolved; {why} of {bound} ran out and the operation's \
outcome is unknown",
            worker.worker_id
        ),
        MutationCertainty::Unknown,
    )
}

/// One per-agent call's result, in the shape the phase loops read it.
///
/// The request is carried back beside the outcome so an uncertain call can be resolved
/// against its *original* domain request id and body. Recomputing it would be a second
/// operation, which `step-v1` section 7 forbids.
struct JobResult {
    agent_id: Id,
    outcome: CallOutcome,
    scope: Option<Scope>,
    worker: WorkerRef,
    method: &'static str,
    request_id: DomainRequestId,
    params: Map<String, Value>,
    attachments: Vec<(String, flybus::Artifact)>,
    /// How long the call took, for the section 5 percentiles.
    elapsed: Duration,
}

impl Coordinator {
    /// Takes the next request serial for this worker, calls, and checks
    /// the reply's identity and echoed scope.
    async fn call(
        &mut self,
        worker: &WorkerRef,
        method: &'static str,
        scope: Option<Scope>,
        params: Map<String, Value>,
        attachments: &[(&str, &flybus::Artifact)],
        want: &[String],
    ) -> Outcome<DomainReply> {
        let request_id = self.serials.next(&worker.service);
        let owned: Vec<(String, flybus::Artifact)> = attachments
            .iter()
            .map(|(n, a)| ((*n).to_owned(), (*a).clone()))
            .collect();
        // Whatever goes wrong from here until the reply is checked belongs to this worker.
        self.blame(Some(worker.worker_id.clone()));
        let deadline = if method.ends_with("Initialize") || method == "Worker.Hello" {
            self.deadlines.boot
        } else if method.starts_with("State.") {
            self.deadlines.capture
        } else {
            self.deadlines.probe
        };
        let started = Instant::now();
        let outcome = call_owned(
            self.bus.clone(),
            worker.clone(),
            method,
            scope.clone(),
            params.clone(),
            owned.clone(),
            request_id.clone(),
            want.to_vec(),
            deadline,
        )
        .await;
        self.metrics.record(method, started.elapsed());
        let reply = match outcome {
            CallOutcome::Answered(reply) => *reply,
            // Uncertain, not failed: section 6 owes this call its resolution first.
            CallOutcome::Expired => {
                return self
                    .resolve(worker, method, scope.clone(), params, owned, request_id, want)
                    .await;
            }
            CallOutcome::Refused(e) => return Err(self.fail_now(e, method)),
        };
        self.check_reply(worker, &reply, &scope, method)?;
        match reply.result() {
            Ok(reply_value) => {
                let _ = reply_value;
                self.blame(None);
                Ok(reply)
            }
            Err(e) => Err(self.fail_now(e, method)),
        }
    }

    /// The worker's identity and the echoed scope must both match, whatever the outcome was.
    ///
    /// A reply from another incarnation means every live participant belongs to an invalid
    /// epoch, so it fails the epoch instead of being read as this session's answer.
    fn check_reply(
        &mut self,
        worker: &WorkerRef,
        reply: &DomainReply,
        scope: &Option<Scope>,
        method: &'static str,
    ) -> Outcome<()> {
        self.blame(Some(worker.worker_id.clone()));
        let (worker_id, incarnation, echoed) = match &reply.outcome {
            SessionRpcOutcome::Success(s) => {
                (&s.worker_id, &s.incarnation_id, &s.scope)
            }
            SessionRpcOutcome::Failure(f) => {
                (&f.worker_id, &f.incarnation_id, &f.scope)
            }
        };
        if *worker_id != worker.worker_id {
            return Err(self.fail_now(
                DomainError::before(ErrorCode::IdentityMismatch, "another worker answered"),
                method,
            ));
        }
        if let Some(expected) = &worker.domain_incarnation
            && incarnation != expected
        {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the reply comes from another worker incarnation",
                ),
                method,
            ));
        }
        if echoed != scope {
            return Err(self.fail_now(
                DomainError::before(ErrorCode::IdentityMismatch, "the reply echoes another scope"),
                method,
            ));
        }
        if *reply.outcome.request_id() != reply.request_id {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the reply echoes another request id",
                ),
                method,
            ));
        }
        Ok(())
    }

    /// The `ipc-v1` section 6 resolution of an uncertain call.
    ///
    /// Step 2 of that section: while the same bus, service and worker incarnation still exist,
    /// issue a fresh bus call carrying the **original** domain request id and body with its
    /// retained input attachments. The worker's own deduplication answers it from the record
    /// of the first attempt, so this queries rather than repeats: it never issues a new request
    /// id, never recomputes a decision, and never becomes a second batch.
    ///
    /// Step 3: only a matching terminal result resolves it. `IN_PROGRESS` means the original is
    /// still running and no second mutation was started, so the procedure waits and asks again.
    /// An expiry inside the procedure is likewise not an answer.
    ///
    /// Step 4: the epoch fails when the routes or ownership were lost, the incarnation changed,
    /// the retained result expired, or the procedure's own bounded budget ran out. Those bounds
    /// are [`Deadlines`] and are explicit, because section 6 refuses an implicit best-effort
    /// policy here.
    #[allow(clippy::too_many_arguments)]
    async fn resolve(
        &mut self,
        worker: &WorkerRef,
        method: &'static str,
        scope: Option<Scope>,
        params: Map<String, Value>,
        attachments: Vec<(String, flybus::Artifact)>,
        request_id: DomainRequestId,
        want: &[String],
    ) -> Outcome<DomainReply> {
        // The original may still be running, which answers IN_PROGRESS for this bus call and
        // starts no second mutation. Waiting and asking again is the resolution, not a retry
        // of the operation.
        self.blame(Some(worker.worker_id.clone()));
        let probe = self.deadlines.probe;
        let budget = self.deadlines.resolve;
        let attempts = self.deadlines.resolve_attempts;
        let started = Instant::now();
        self.resolutions += 1;
        self.last_resolution = None;
        self.audit.push(format!("resolve:{}:{method}", worker.worker_id));
        // The budget is the working limit and the attempt count is a guard; whichever runs
        // out is recorded, so "it gave up" is never an unexplained number.
        let mut end = ResolutionEnd::AttemptsExhausted;
        let mut spent = 0u32;
        for _ in 0..attempts {
            if started.elapsed() >= budget {
                end = ResolutionEnd::BudgetExpired;
                break;
            }
            spent += 1;
            self.last_resolution_attempts = spent;
            let outcome = call_owned(
                self.bus.clone(),
                worker.clone(),
                method,
                scope.clone(),
                params.clone(),
                attachments.clone(),
                request_id.clone(),
                want.to_vec(),
                probe,
            )
            .await;
            let reply = match outcome {
                CallOutcome::Answered(reply) => *reply,
                // Still no answer. The original may simply be slow; asking again is the
                // procedure, and the request id it carries is unchanged.
                CallOutcome::Expired => {
                    tokio::time::sleep(RESOLVE_PAUSE).await;
                    continue;
                }
                // Step 4: routes or ownership lost, or the incarnation is gone.
                CallOutcome::Refused(e) => return Err(self.fail_now(e, method)),
            };
            self.check_reply(worker, &reply, &scope, method)?;
            match reply.result() {
                Ok(_) => {
                    self.blame(None);
                    self.last_resolution = Some(ResolutionEnd::Answered);
                    return Ok(reply);
                }
                Err(e) if e.code == ErrorCode::InProgress => {
                    self.in_progress_replies += 1;
                    tokio::time::sleep(RESOLVE_PAUSE).await;
                }
                // A terminal refusal, including RESULT_EXPIRED: definite, so the epoch fails.
                Err(e) => return Err(self.fail_now(e, method)),
            }
        }
        self.last_resolution = Some(end);
        let bound = match end {
            ResolutionEnd::AttemptsExhausted => format!("{attempts} attempts"),
            _ => format!("{budget:?}"),
        };
        let error = unresolved(method, worker, end, &bound);
        Err(self.fail_now(error, method))
    }

    /// Sends the same domain request again on a fresh bus call and reports what came back,
    /// without letting the answer change session state.
    #[allow(clippy::too_many_arguments)]
    async fn probe_duplicate(
        &mut self,
        worker: &WorkerRef,
        method: &'static str,
        scope: Option<Scope>,
        params: Map<String, Value>,
        attachments: Vec<(String, flybus::Artifact)>,
        request_id: DomainRequestId,
        expected: Option<&Value>,
        what: &str,
    ) {
        let deadline = self.deadlines.probe;
        let reply = call_owned(
            self.bus.clone(),
            worker.clone(),
            method,
            scope,
            params,
            attachments,
            request_id,
            Vec::new(),
            deadline,
        )
        .await;
        let outcome = match reply {
            CallOutcome::Answered(reply) => match reply.result() {
                Ok(result) => InjectionOutcome {
                    what: what.to_owned(),
                    code: None,
                    identical: expected == Some(result),
                },
                Err(e) => InjectionOutcome {
                    what: what.to_owned(),
                    code: Some(e.code),
                    identical: false,
                },
            },
            CallOutcome::Refused(e) => InjectionOutcome {
                what: what.to_owned(),
                code: Some(e.code),
                identical: false,
            },
            // A probe is a diagnostic, not a phase of the transaction: it reports what it saw
            // and never resolves anything on the session's behalf.
            CallOutcome::Expired => InjectionOutcome {
                what: what.to_owned(),
                code: Some(ErrorCode::BackendFailure),
                identical: false,
            },
        };
        self.injection_log.push(outcome);
    }

    /// Sends one domain request exactly as given and returns the worker's own terminal
    /// outcome, without letting the answer change session state.
    ///
    /// This is how a supervisor or a test observes a worker's refusal -- a stale epoch, a
    /// wrong phase -- rather than inferring it from a failed session.
    pub async fn probe_raw(
        &mut self,
        worker: &WorkerRef,
        method: &'static str,
        scope: Option<Scope>,
        params: Value,
    ) -> Result<Value, DomainError> {
        let params = match params {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let request_id = self.serials.next(&worker.service);
        let outcome = call_owned(
            self.bus.clone(),
            worker.clone(),
            method,
            scope,
            params,
            Vec::new(),
            request_id,
            Vec::new(),
            self.deadlines.probe,
        )
        .await;
        match outcome {
            CallOutcome::Answered(reply) => reply.result().cloned(),
            CallOutcome::Refused(e) => Err(e),
            CallOutcome::Expired => Err(unresolved(
                method,
                worker,
                ResolutionEnd::BudgetExpired,
                &format!("{:?}", self.deadlines.probe),
            )),
        }
    }

    /// The sensory input one agent is permitted to consume at `boundary`.
    fn sensory_input(&self, observation: &WorldObservation, boundary: u64) -> SensoryInput {
        SensoryInput {
            boundary,
            views: observation.sensory_views.clone(),
            // This profile senses pixels only, so structured input stays null rather than
            // smuggling inspection data into the neural path.
            structured: None,
        }
    }

    fn view_attachments(&self) -> Vec<(String, flybus::Artifact)> {
        self.views.iter().map(|(n, a)| (n.clone(), a.clone())).collect()
    }

    /// Runs one set of per-agent jobs in the configured dispatch order.
    /// A job's deadline is the section 6 probe, not the failure point: an expiry here starts
    /// the resolution, which the phase loops run one at a time with the session in hand.
    async fn run_jobs(&mut self, jobs: Vec<Job>, order: DispatchOrder) -> Vec<JobResult> {
        let deadline = self.deadlines.probe;
        let mut out = Vec::new();
        match order {
            DispatchOrder::Sequential | DispatchOrder::Reversed => {
                let mut jobs = jobs;
                if order == DispatchOrder::Reversed {
                    jobs.reverse();
                }
                for job in jobs {
                    let started = Instant::now();
                    let outcome = call_owned(
                        self.bus.clone(),
                        job.worker.clone(),
                        job.method,
                        job.scope.clone(),
                        job.params.clone(),
                        job.attachments.clone(),
                        job.request_id.clone(),
                        Vec::new(),
                        deadline,
                    )
                    .await;
                    out.push(JobResult {
                        agent_id: job.agent_id,
                        outcome,
                        scope: job.scope,
                        worker: job.worker,
                        method: job.method,
                        request_id: job.request_id,
                        params: job.params,
                        attachments: job.attachments,
                        elapsed: started.elapsed(),
                    });
                }
            }
            DispatchOrder::Concurrent => {
                let mut tasks = Vec::new();
                for job in jobs {
                    let bus = self.bus.clone();
                    let agent_id = job.agent_id.clone();
                    let worker = job.worker.clone();
                    let scope = job.scope.clone();
                    let method = job.method;
                    tasks.push(tokio::spawn(async move {
                        let started = Instant::now();
                        let outcome = call_owned(
                            bus,
                            job.worker,
                            job.method,
                            job.scope,
                            job.params.clone(),
                            job.attachments.clone(),
                            job.request_id.clone(),
                            Vec::new(),
                            deadline,
                        )
                        .await;
                        JobResult {
                            agent_id,
                            outcome,
                            scope,
                            worker,
                            method,
                            request_id: job.request_id,
                            params: job.params,
                            attachments: job.attachments,
                            elapsed: started.elapsed(),
                        }
                    }));
                }
                for task in tasks {
                    match task.await {
                        Ok(result) => out.push(result),
                        Err(e) => panic!("a dispatch task panicked: {e}"),
                    }
                }
            }
        }
        // Completion order never affects anything downstream, so the results are put back in
        // sorted agent-id order here and nowhere else.
        out.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        for result in &out {
            self.metrics.record(result.method, result.elapsed);
        }
        out
    }
}

impl Coordinator {
    /// One complete transition `k -> k+1`.
    pub async fn step(&mut self) -> Outcome<StepReport> {
        if self.fenced {
            // A failed epoch's routes, registrations and handles are invalid. There is no
            // partial continuation: only a coherent restore into a new epoch resumes play.
            return Err(SessionFailure {
                error: DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the epoch is fenced; only a coherent restore resumes play",
                ),
                phase: self.phases.phase().label(),
                detail: "fenced".to_owned(),
                participant: None,
            });
        }
        let mut step_started = Instant::now();
        let Phase::Ready(k) = self.phases.phase() else {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::InvalidPhase,
                    "a transition starts only from a committed Ready boundary",
                ),
                "step",
            ));
        };
        if self.episode.is_some() {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the task asked for a terminal episode; the episode policy runs first",
                ),
                "step",
            ));
        }
        let descriptor = self.descriptor.clone().ok_or_else(|| {
            DomainError::before(ErrorCode::InvalidPhase, "the session never bootstrapped")
        });
        let descriptor = match descriptor {
            Ok(descriptor) => descriptor,
            Err(e) => return Err(self.fail_now(e, "step")),
        };
        let old_observation = self.observation.clone().expect("bootstrapped");
        if let Some(pacing) = self.pacing.as_mut() {
            // Wall time is only pacing. Being late omits the sleep and is reported; it never
            // skips a world step or a neural tick.
            pacing.wait().await;
            // The critical path is the transaction, not the sleep in front of it: the
            // deadline the coordinator was waiting for is pacing, and pacing is not work.
            step_started = Instant::now();
        }

        // ---- Phase A: prepare all agents concurrently
        self.transition(Phase::Preparing(k))?;
        let prepared = self.prepare_all(k, descriptor.step_duration).await?;

        // ---- Phase B: build and apply one complete batch
        self.transition(Phase::Applying(k))?;
        let controls = self.build_batch(k, &descriptor, &old_observation)?;
        let batch_id = self.batch_id(k);
        let step_result = self.advance(k, &batch_id, &controls).await?;

        // ---- Phase C: observe and evaluate the task once
        self.transition(Phase::Observing(k + 1))?;
        self.verify_step_result(k, &descriptor, &batch_id, &controls, &old_observation, &step_result)?;
        let scope = self.scope(k);
        let evaluation = self
            .task
            .evaluate_transition(
                &scope,
                &old_observation.inspection,
                &step_result.observation.inspection,
                &controls,
            )
            .map_err(|e| {
                // Task interpretation failed after prepared brains and the world already
                // changed, so the epoch is failed rather than re-evaluated.
                self.fail_now(e, "task-evaluate")
            })?;
        self.audit.push(format!("evaluate:{k}"));
        let mut outcomes = evaluation.outcomes.clone();
        let mut next_contexts = evaluation.next_contexts.clone();
        for agent in self.agent_ids() {
            if !outcomes.contains_key(&agent) || !next_contexts.contains_key(&agent) {
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::IdentityMismatch,
                        format!("the task produced no outcome or context for {agent}"),
                    ),
                    "task-evaluate",
                ));
            }
        }

        // ---- Phase D: commit all agent outcomes concurrently
        self.transition(Phase::Committing(k))?;
        let new_views: BTreeMap<String, flybus::Artifact> = step_result
            .observation
            .sensory_views
            .iter()
            .filter_map(|view| {
                let name = media::view_attachment(&view.view_id);
                self.pending_views.get(&name).map(|a| (name, a.clone()))
            })
            .collect();
        let new_audio: BTreeMap<String, flybus::Artifact> = step_result
            .observation
            .audio
            .iter()
            .filter_map(|chunk| {
                let name = media::audio_attachment(&chunk.stream_id);
                self.pending_audio.get(&name).map(|a| (name, a.clone()))
            })
            .collect();
        let commits = self
            .commit_all(k, &step_result.observation, &mut outcomes, &mut next_contexts, &new_views)
            .await?;

        // Only once every commit succeeded does the committed boundary move.
        self.transition(Phase::Ready(k + 1))?;
        for index in 0..self.agents.len() {
            let agent_id = self.agents[index].agent_id.clone();
            let context = next_contexts.remove(&agent_id).expect("checked above");
            self.agents[index].context_digest = context.digest();
            self.agents[index].context = context;
            self.agents[index].committed_step = k + 1;
        }
        // The previous boundary's handles are no longer needed; the new ones take over.
        self.views = new_views;
        self.audio = new_audio;
        self.pending_views.clear();
        self.pending_audio.clear();
        self.observation = Some(step_result.observation.clone());
        self.stats.advances += 1;

        let event_ids: Vec<Id> = evaluation.events.iter().map(|e| e.id.clone()).collect();
        let decisions: BTreeMap<Id, TypedValue> = prepared
            .iter()
            .map(|(agent, decision)| (agent.clone(), decision.decision.clone()))
            .collect();
        self.record_trace(
            k,
            &prepared,
            &commits,
            &controls,
            &batch_id,
            &step_result,
            &outcomes,
            &event_ids,
        );
        // The prepared decisions and their request ids are needed by the trace, so they are
        // released only after it has been recorded.
        for slot in &mut self.agents {
            slot.prepared = None;
            slot.prepare_request = None;
        }
        self.publish_events(k + 1, &evaluation.events).await?;
        self.publish_snapshot(k + 1, &decisions, &controls, &event_ids).await?;

        let terminal = evaluation.episode.is_some();
        if terminal {
            // A terminal event's final rewards are committed; then the session pauses at this
            // boundary and the declared episode policy runs. No worker resets itself.
            self.episode = evaluation.episode.clone();
        }
        let paused = self.pause_requested() || terminal;
        if paused {
            self.transition(Phase::Paused(k + 1))?;
            self.pause.store(false, std::sync::atomic::Ordering::SeqCst);
            self.audit.push(format!("pause:{}", k + 1));
        }
        // The critical path: one whole transition, pacing sleep included.
        self.metrics.record("step", step_started.elapsed());
        Ok(StepReport { boundary: k + 1, paused, terminal })
    }

    /// Runs `steps` transitions, stopping early at a pause or a terminal episode.
    pub async fn run(&mut self, steps: u64) -> Outcome<Vec<StepReport>> {
        let mut reports = Vec::new();
        for _ in 0..steps {
            let report = self.step().await?;
            let stop = report.paused;
            reports.push(report);
            if stop {
                break;
            }
        }
        Ok(reports)
    }

    fn batch_id(&self, k: u64) -> Id {
        parse_id(&format!("batch-{}-{k}", self.epoch)).expect("epoch and step make an Id")
    }

    /// Phase A. Every agent sees the same environment interval and the same world boundary.
    async fn prepare_all(
        &mut self,
        k: u64,
        interval: RationalNs,
    ) -> Outcome<Vec<(Id, PreparedDecision)>> {
        let scope = self.scope(k);
        // The task's per-agent decision contexts and the admitted pre-step stimulation list
        // are frozen here; anything accepted later waits for the next boundary.
        let mut jobs = Vec::new();
        let mut bodies = Vec::new();
        for index in 0..self.agents.len() {
            let slot = &self.agents[index];
            let params = PrepareParams {
                agent_id: slot.agent_id.clone(),
                profile_digest: slot.profile.digest.clone(),
                interval,
                decision_context_digest: slot.context_digest.clone(),
                // No audience input exists in the first synthetic composition.
                pre_step_stimulations: Vec::<Stimulus>::new(),
            };
            let params = match params.to_json() {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            let request_id = self.serials.next(&slot.worker.service);
            self.agents[index].prepare_request = Some(request_id.clone());
            let slot = &self.agents[index];
            bodies.push((
                slot.agent_id.clone(),
                slot.worker.clone(),
                params.clone(),
                request_id.clone(),
            ));
            jobs.push(Job {
                agent_id: slot.agent_id.clone(),
                worker: slot.worker.clone(),
                method: "Agent.Prepare",
                scope: Some(scope.clone()),
                params,
                attachments: Vec::new(),
                request_id,
            });
        }
        let results = self.run_jobs(jobs, self.dispatch).await;
        let mut prepared = Vec::new();
        for job in results {
            let JobResult {
                agent_id,
                outcome,
                scope,
                worker,
                method,
                request_id,
                params,
                attachments,
                elapsed: _,
            } = job;
            self.blame(Some(agent_id.clone()));
            let reply = match outcome {
                CallOutcome::Answered(reply) => *reply,
                // Uncertain: the agent may be slow rather than gone. Query the same operation
                // against the same incarnation before the epoch is failed. A Prepare that
                // already ran answers from its own record, so no tick is repeated.
                CallOutcome::Expired => {
                    self.resolve(&worker, method, scope.clone(), params, attachments, request_id, &[])
                        .await?
                }
                // Some agents are already Prepared. Dispatch stops and the epoch fails; a
                // prepared agent is never asked to prepare again.
                CallOutcome::Refused(e) => return Err(self.fail_now(e, method)),
            };
            self.check_reply(&worker, &reply, &scope, method)?;
            let decision: PreparedDecision = match reply.parse() {
                Ok(decision) => decision,
                Err(e) => return Err(self.fail_now(e, method)),
            };
            if decision.agent_id != agent_id {
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::IdentityMismatch,
                        "a PreparedDecision names another agent",
                    ),
                    method,
                ));
            }
            if decision.decision.schema != crate::task::decision_schema() {
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::IdentityMismatch,
                        "the decision does not carry the profile's registered intent schema",
                    ),
                    method,
                ));
            }
            if let Some(slot) = self.agents.iter_mut().find(|slot| slot.agent_id == agent_id) {
                // What the agent will be at once this transition commits: Prepare is the only
                // phase that advances the accumulator.
                slot.brain_ticks = decision.brain_ticks;
                slot.remainder = decision.remainder;
            }
            self.audit.push(format!("prepared:{agent_id}@{k}"));
            self.stats.prepares += 1;
            self.blame(None);
            prepared.push((agent_id, decision));
        }
        prepared.sort_by(|a, b| a.0.cmp(&b.0));
        for index in 0..self.agents.len() {
            let agent_id = self.agents[index].agent_id.clone();
            let decision = prepared
                .iter()
                .find(|(id, _)| *id == agent_id)
                .map(|(_, decision)| decision.clone());
            self.agents[index].prepared = decision;
        }
        if self.agents.iter().any(|slot| slot.prepared.is_none()) {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the batch is built only after every PreparedDecision has arrived",
                ),
                "prepare",
            ));
        }

        if self.injections.at_step == k
            && let Some(target) = self.injections.duplicate_prepare.clone()
        {
            let expected = prepared
                .iter()
                .find(|(id, _)| *id == target)
                .map(|(_, decision)| decision.to_json());
            if let Some((agent_id, worker, params, request_id)) =
                bodies.into_iter().find(|(id, _, _, _)| *id == target)
            {
                let _ = agent_id;
                self.probe_duplicate(
                    &worker,
                    "Agent.Prepare",
                    Some(self.scope(k)),
                    params,
                    Vec::new(),
                    request_id,
                    expected.as_ref(),
                    "duplicate-prepare",
                )
                .await;
            }
        }
        Ok(prepared)
    }

    /// Phase B steps 1 to 3: validate, run each executor once, assemble the complete batch.
    fn build_batch(
        &mut self,
        k: u64,
        descriptor: &EnvironmentDescriptor,
        observation: &WorldObservation,
    ) -> Outcome<Vec<PortControl>> {
        let scope = self.scope(k);
        let progress = self.task.progress();
        let clock = observation.world_time;
        let mut intents: BTreeMap<Id, PortControl> = BTreeMap::new();
        // Sorted agent-id order, never completion order.
        for index in 0..self.agents.len() {
            let agent_id = self.agents[index].agent_id.clone();
            let port_id = self.agents[index].port_id.clone();
            let decision = self.agents[index]
                .prepared
                .clone()
                .expect("every agent is Prepared before the batch is built");
            let executor = self.executors.get_mut(&agent_id).ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("{agent_id} has no configured action executor"),
                )
            });
            let executor = match executor {
                Ok(executor) => executor,
                Err(e) => return Err(self.fail_now(e, "executor")),
            };
            let applied = executor.apply(
                &scope,
                &decision.decision,
                &observation.inspection,
                &progress,
                &clock,
            );
            let (intent, _events) = match applied {
                Ok(applied) => applied,
                Err(e) => return Err(self.fail_now(e, "executor")),
            };
            // Only the coordinator assigns a port.
            if intents.insert(port_id.clone(), intent.at_port(&port_id)).is_some() {
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::IdentityMismatch,
                        format!("port {port_id} is claimed by two agents"),
                    ),
                    "executor",
                ));
            }
        }
        // All configured port controls, in descriptor port order; duplicates and omissions are
        // refused rather than filled in.
        let mut controls = Vec::with_capacity(descriptor.ports.len());
        for port in &descriptor.ports {
            let control = intents.remove(&port.port_id).ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("port {} has no control in this batch", port.port_id),
                )
            });
            let control = match control {
                Ok(control) => control,
                Err(e) => return Err(self.fail_now(e, "batch")),
            };
            if let Err(e) = port.controls.check(&control) {
                return Err(self.fail_now(DomainError::invalid(e), "batch"));
            }
            controls.push(control);
        }
        if !intents.is_empty() {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the batch names a port the descriptor does not declare",
                ),
                "batch",
            ));
        }
        Ok(controls)
    }

    /// Phase B step 4: exactly one `Environment.Advance` per complete batch.
    async fn advance(
        &mut self,
        k: u64,
        batch_id: &Id,
        controls: &[PortControl],
    ) -> Outcome<StepResult> {
        let scope = self.scope(k);
        let params = AdvanceParams {
            batch_id: batch_id.clone(),
            controls: controls.to_vec(),
        };
        let params = match params.to_json() {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let worker = self.environment.clone();
        let request_id = self.serials.next(&worker.service);
        self.last_advance_request = Some(request_id.clone());
        let want = self.media_names.clone();
        self.audit.push(format!("advance:{k}"));
        self.blame(Some(worker.worker_id.clone()));
        let advance_deadline = self.deadlines.probe;
        let advance_started = Instant::now();

        let injected = self.injections.at_step == k;
        let reply = if injected && self.injections.lose_advance_result {
            // The result is lost after the world already stepped: the call is abandoned
            // mid-flight, so its execution outcome is unknown. Further dispatch stops and the
            // same operation is resolved; a new batch is never sent.
            let pending = self
                .bus
                .call(
                    &worker.service,
                    Some(&worker.bus_incarnation),
                    "Environment.Advance",
                    object(
                        SessionRpcRequest {
                            request_id: request_id.clone(),
                            scope: Some(scope.clone()),
                            params: Value::Object(params.clone()),
                        }
                        .to_json(),
                    ),
                    &[],
                )
                .await
                .map_err(|e| {
                    let error = DomainError::new(
                        ErrorCode::BackendFailure,
                        format!("Environment.Advance: bus {:?}", e.code),
                        MutationCertainty::Unknown,
                    );
                    self.fail_now(error, "advance")
                })?;
            // The loss has to happen *after* the world stepped, so the injection waits for
            // the worker to report the operation before abandoning the call. A status probe
            // is exactly what the uncertain-call procedure does first anyway.
            let mut knows = false;
            for _ in 0..500u32 {
                let status = self.status(&worker).await?;
                knows = status.last_batch_id.as_ref() == Some(batch_id)
                    || status.active_request_id.as_ref() == Some(&request_id)
                    || status.last_completed_request_id.as_ref() == Some(&request_id);
                if knows {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            self.injection_log.push(InjectionOutcome {
                what: "status-after-loss".to_owned(),
                code: None,
                identical: knows,
            });
            let state = pending.cancel().await.ok();
            drop(pending);
            // Cancellation after dispatch cannot undo the work: the execution outcome is
            // uncertain, which is the case this injects.
            self.injection_log.push(InjectionOutcome {
                what: "lost-advance-result".to_owned(),
                code: None,
                identical: state != Some(flybus::CancelState::CancelledBeforeDispatch),
            });
            self.resolve(
                &worker,
                "Environment.Advance",
                Some(scope.clone()),
                params.clone(),
                Vec::new(),
                request_id.clone(),
                &want,
            )
            .await?
        } else {
            let outcome = call_owned(
                self.bus.clone(),
                worker.clone(),
                "Environment.Advance",
                Some(scope.clone()),
                params.clone(),
                Vec::new(),
                request_id.clone(),
                want.clone(),
                advance_deadline,
            )
            .await;
            self.metrics.record("Environment.Advance", advance_started.elapsed());
            match outcome {
                CallOutcome::Answered(reply) => {
                    let reply = *reply;
                    self.check_reply(&worker, &reply, &Some(scope.clone()), "advance")?;
                    match reply.result() {
                        Ok(_) => reply,
                        Err(e) => return Err(self.fail_now(e, "advance")),
                    }
                }
                // `step-v1` section 7 is imperative for this row: "Advance acknowledgment
                // lost | Query/retransmit same request to same incarnation; never new batch."
                // The resolution carries the original batch id and controls, so the world is
                // asked about the operation it already has rather than given another one.
                CallOutcome::Expired => {
                    self.resolve(
                        &worker,
                        "Environment.Advance",
                        Some(scope.clone()),
                        params.clone(),
                        Vec::new(),
                        request_id.clone(),
                        &want,
                    )
                    .await?
                }
                CallOutcome::Refused(e) => return Err(self.fail_now(e, "advance")),
            }
        };

        let result: StepResult = match reply.parse() {
            Ok(result) => result,
            Err(e) => return Err(self.fail_now(e, "advance")),
        };
        self.blame(None);
        let (pending_views, pending_audio) = media::split_attachments(reply.artifacts);
        self.pending_views = pending_views;
        self.pending_audio = pending_audio;

        if injected && self.injections.altered_advance_controls {
            // The same request id with a different body: a conflict, never a second world
            // mutation.
            let mut altered = controls.to_vec();
            if let Some(control) = altered.first_mut()
                && let Some(button) = control.buttons.first_mut()
            {
                button.down = !button.down;
            }
            let altered_params = object(
                AdvanceParams { batch_id: batch_id.clone(), controls: altered }.to_json(),
            );
            self.probe_duplicate(
                &worker,
                "Environment.Advance",
                Some(scope.clone()),
                altered_params,
                Vec::new(),
                request_id.clone(),
                None,
                "altered-advance-controls",
            )
            .await;
        }

        if injected && self.injections.consume_advance_artifact_then_retry {
            // The first caller consumes the result's frame, then replays the operation. The
            // endpoint's cache owns its own hold, so the replay still has valid bytes.
            let first = match self.pending_views.get("view.arena") {
                Some(artifact) => artifact.read_all().await.ok(),
                None => None,
            };
            self.pending_views.clear();
            self.pending_audio.clear();
            let replay = self
                .resolve(
                    &worker,
                    "Environment.Advance",
                    Some(scope.clone()),
                    params.clone(),
                    Vec::new(),
                    request_id.clone(),
                    &want,
                )
                .await?;
            let again = match replay.artifacts.get("view.arena") {
                Some(artifact) => artifact.read_all().await.ok(),
                None => None,
            };
            self.injection_log.push(InjectionOutcome {
                what: "cached-artifact-after-consumption".to_owned(),
                code: None,
                identical: first.is_some() && first == again,
            });
            let (pending_views, pending_audio) = media::split_attachments(replay.artifacts);
            self.pending_views = pending_views;
            self.pending_audio = pending_audio;
        }
        Ok(result)
    }
}

impl Coordinator {
    /// Phase C's verification: batch identity, boundary, cadence, schema and required views.
    #[allow(clippy::too_many_arguments)]
    fn verify_step_result(
        &mut self,
        k: u64,
        descriptor: &EnvironmentDescriptor,
        batch_id: &Id,
        controls: &[PortControl],
        old: &WorldObservation,
        result: &StepResult,
    ) -> Outcome<()> {
        if result.batch_id != *batch_id {
            return Err(self.fail_now(
                DomainError::before(ErrorCode::IdentityMismatch, "the result names another batch"),
                "step-result",
            ));
        }
        if result.applied_from_step != k || result.next_step != k + 1 {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the result does not describe exactly this transition",
                ),
                "step-result",
            ));
        }
        if result.applied_controls_digest != controls_digest(controls) {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the applied control digest is not the batch that was requested",
                ),
                "step-result",
            ));
        }
        if result.observation.boundary != k + 1 {
            return Err(self.fail_now(
                DomainError::before(ErrorCode::IdentityMismatch, "the observation is not k+1"),
                "step-result",
            ));
        }
        // Fixed cadence within an epoch: exactly one step duration of world time.
        let expected_time = old
            .world_time
            .checked_add(&descriptor.step_duration)
            .map_err(DomainError::invalid);
        let expected_time = match expected_time {
            Ok(time) => time,
            Err(e) => return Err(self.fail_now(e, "step-result")),
        };
        if result.observation.world_time != expected_time {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "world time did not advance by exactly one step duration",
                ),
                "step-result",
            ));
        }
        if result.observation.inspection.schema != descriptor.inspection_schema {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the inspection payload is not the declared schema",
                ),
                "step-result",
            ));
        }
        // A missing required sensory input is never silently replaced by an older frame,
        // and neither is one produced further back than the declared delay allows. The
        // contract's validator checks the views that are present against their descriptors;
        // requiring each declared view to be there at all is this Phase C check.
        if let Err(e) = media::check_required_views(descriptor, &result.observation) {
            return Err(self.fail_now(e, "step-result"));
        }
        // Audio has no sensory role here, but its chunks still cannot overlap or go backwards
        // inside an epoch, and a stale one must not reach presentation as current.
        if let Err(e) = media::check_required_audio(
            descriptor,
            &result.observation,
            media::ObservationOrigin::Transition,
        ) {
            return Err(self.fail_now(e, "step-result"));
        }
        if let Err(e) = self.timelines.accept(descriptor, &result.observation) {
            return Err(self.fail_now(e, "step-result"));
        }
        if let Err(e) = result.observation.validate_against(descriptor) {
            return Err(self.fail_now(
                DomainError::new(ErrorCode::BufferInvalid, e, MutationCertainty::Unknown),
                "step-result",
            ));
        }
        for view in &result.observation.sensory_views {
            let name = media::view_attachment(&view.view_id);
            match self.pending_views.get(&name) {
                Some(artifact) if artifact.reference() == &view.pixels => {}
                _ => {
                    return Err(self.fail_now(
                        DomainError::new(
                            ErrorCode::BufferInvalid,
                            format!("required view {} arrived without a live owned handle", view.view_id),
                            MutationCertainty::Unknown,
                        ),
                        "step-result",
                    ));
                }
            }
        }
        for chunk in &result.observation.audio {
            let name = media::audio_attachment(&chunk.stream_id);
            match self.pending_audio.get(&name) {
                Some(artifact) if artifact.reference() == &chunk.samples => {}
                _ => {
                    return Err(self.fail_now(
                        DomainError::new(
                            ErrorCode::BufferInvalid,
                            format!(
                                "audio chunk {} arrived without a live owned handle",
                                chunk.stream_id
                            ),
                            MutationCertainty::Unknown,
                        ),
                        "step-result",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Phase D. Every agent commits before the next Prepare or any committed publication.
    async fn commit_all(
        &mut self,
        k: u64,
        observation: &WorldObservation,
        outcomes: &mut BTreeMap<Id, AgentOutcome>,
        next_contexts: &mut BTreeMap<Id, TypedValue>,
        views: &BTreeMap<String, flybus::Artifact>,
    ) -> Outcome<Vec<(Id, AgentCommitResult)>> {
        let scope = self.scope(k);
        let attachments: Vec<(String, flybus::Artifact)> =
            views.iter().map(|(n, a)| (n.clone(), a.clone())).collect();
        let mut jobs = Vec::new();
        let mut bodies = Vec::new();
        for index in 0..self.agents.len() {
            let agent_id = self.agents[index].agent_id.clone();
            let prepared_request = self.agents[index]
                .prepare_request
                .clone()
                .expect("every agent prepared");
            let outcome = outcomes.get(&agent_id).cloned().unwrap_or_default();
            let next_context = next_contexts.get(&agent_id).cloned().expect("checked");
            let params = CommitParams {
                agent_id: agent_id.clone(),
                prepared_request_id: prepared_request.clone(),
                next_input: self.sensory_input(observation, k + 1),
                next_decision_context: next_context,
                rewards: outcome.rewards.clone(),
                task_stimulations: outcome.stimulations.clone(),
            };
            let params = match params.to_json() {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            let request_id = self.serials.next(&self.agents[index].worker.service);
            let worker = self.agents[index].worker.clone();
            bodies.push((
                agent_id.clone(),
                worker.clone(),
                params.clone(),
                request_id.clone(),
            ));
            jobs.push(Job {
                agent_id,
                worker,
                method: "Agent.Commit",
                scope: Some(scope.clone()),
                params,
                attachments: attachments.clone(),
                request_id,
            });
        }
        self.last_commit_requests = bodies
            .iter()
            .map(|(agent_id, _, _, request_id)| TraceRequest {
                agent_id: agent_id.clone(),
                request_id: request_id.clone(),
            })
            .collect();
        let results = self.run_jobs(jobs, self.dispatch).await;
        let mut commits = Vec::new();
        let mut first_failure = None;
        let mut blamed: Option<Id> = None;
        for job in results {
            let JobResult {
                agent_id,
                outcome,
                scope,
                worker,
                method,
                request_id,
                params,
                attachments: job_attachments,
                elapsed: _,
            } = job;
            self.blame(Some(agent_id.clone()));
            // Uncertain: resolve the same Commit against the same incarnation first. A Commit
            // that already ran replays its cached reply, so no reward is applied twice.
            let outcome = match outcome {
                CallOutcome::Expired => {
                    match self
                        .resolve(
                            &worker,
                            method,
                            scope.clone(),
                            params,
                            job_attachments,
                            request_id,
                            &[],
                        )
                        .await
                    {
                        Ok(reply) => CallOutcome::Answered(Box::new(reply)),
                        Err(failure) => return Err(failure),
                    }
                }
                other => other,
            };
            match outcome {
                CallOutcome::Answered(reply) => {
                    let reply = *reply;
                    self.check_reply(&worker, &reply, &scope, method)?;
                    match reply.result() {
                        Ok(_) => {}
                        Err(e) => {
                            if first_failure.is_none() {
                                blamed = Some(agent_id.clone());
                            }
                            first_failure = Some(first_failure.unwrap_or(e));
                            continue;
                        }
                    }
                    let result: AgentCommitResult = match reply.parse() {
                        Ok(result) => result,
                        Err(e) => return Err(self.fail_now(e, method)),
                    };
                    if result.committed_step != k + 1 {
                        return Err(self.fail_now(
                            DomainError::before(
                                ErrorCode::IdentityMismatch,
                                "an agent acknowledged another boundary",
                            ),
                            method,
                        ));
                    }
                    let expected = next_contexts.get(&agent_id).expect("checked").digest();
                    if result.decision_context_digest != expected {
                        return Err(self.fail_now(
                            DomainError::before(
                                ErrorCode::IdentityMismatch,
                                "an agent retained another decision context",
                            ),
                            method,
                        ));
                    }
                    if let Err(e) = result.telemetry.validate() {
                        return Err(self.fail_now(DomainError::invalid(e), method));
                    }
                    self.audit.push(format!("committed:{agent_id}@{k}"));
                    self.stats.commits += 1;
                    self.blame(None);
                    commits.push((agent_id, result));
                }
                CallOutcome::Refused(e) => {
                    if first_failure.is_none() {
                        blamed = Some(agent_id.clone());
                    }
                    first_failure = Some(first_failure.unwrap_or(e));
                }
                CallOutcome::Expired => unreachable!("an expiry was resolved just above"),
            }
        }
        if let Some(error) = first_failure {
            // One Commit failed after others succeeded. There is no partial-match
            // continuation: the epoch is failed and the group recovers together, and the
            // failure names the agent whose Commit failed.
            let _ = bodies;
            self.blame(blamed);
            return Err(self.fail_now(error, "commit"));
        }
        if commits.len() != self.agents.len() {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::InvalidPhase,
                    "no next world step until every agent has committed",
                ),
                "commit",
            ));
        }

        if self.injections.at_step == k
            && let Some(target) = self.injections.duplicate_commit.clone()
        {
            let expected = commits
                .iter()
                .find(|(id, _)| *id == target)
                .map(|(_, result)| result.to_json());
            if let Some((_, worker, params, request_id)) =
                bodies.into_iter().find(|(id, _, _, _)| *id == target)
            {
                self.probe_duplicate(
                    &worker,
                    "Agent.Commit",
                    Some(self.scope(k)),
                    params,
                    attachments.clone(),
                    request_id,
                    expected.as_ref(),
                    "duplicate-commit",
                )
                .await;
            }
        }
        commits.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(commits)
    }

    /// Records the `step-v1` section 8 trace for this transition.
    /// Records the `step-v1` section 8 trace for this transition.
    ///
    /// Behaviour and operational metadata are separated by the contract type: the behaviour is
    /// what a reordered run must reproduce exactly, and the request ids, batch correlation and
    /// wall time are recorded beside it rather than inside it.
    #[allow(clippy::too_many_arguments)]
    fn record_trace(
        &mut self,
        k: u64,
        prepared: &[(Id, PreparedDecision)],
        commits: &[(Id, AgentCommitResult)],
        controls: &[PortControl],
        batch_id: &Id,
        result: &StepResult,
        outcomes: &BTreeMap<Id, AgentOutcome>,
        event_ids: &[Id],
    ) {
        let mut agents = Vec::new();
        let mut outcome_ids = Vec::new();
        let mut prepare_request_ids = Vec::new();
        for (agent_id, decision) in prepared {
            let slot = self.agent(agent_id).expect("configured");
            let committed = commits
                .iter()
                .find(|(id, _)| id == agent_id)
                .map(|(_, result)| result.committed_step)
                .unwrap_or(0);
            agents.push(TraceAgent {
                agent_id: agent_id.clone(),
                profile_digest: slot.profile.digest.clone(),
                ticks_advanced: decision.ticks_advanced,
                brain_ticks: decision.brain_ticks,
                remainder: decision.remainder,
                decision_digest: decision.decision.digest(),
                committed_step: committed,
            });
            if let Some(request_id) = slot.prepare_request.clone() {
                prepare_request_ids.push(TraceRequest { agent_id: agent_id.clone(), request_id });
            }
            // Outcome ids in task order: the reward events this agent was routed.
            if let Some(outcome) = outcomes.get(agent_id) {
                for reward in &outcome.rewards {
                    outcome_ids.push(reward.event_id.clone());
                }
            }
        }
        let mut observation_boundaries: Vec<TraceObservation> = result
            .observation
            .sensory_views
            .iter()
            .map(|view| TraceObservation {
                view_id: view.view_id.clone(),
                produced_step: view.produced_step,
            })
            .collect();
        observation_boundaries.sort_by(|a, b| a.view_id.cmp(&b.view_id));
        let behaviour = TraceBehaviour {
            scope: self.scope(k),
            agents,
            batch_id: batch_id.clone(),
            control_digest: controls_digest(controls),
            acknowledged_boundary: result.next_step,
            observation_boundaries,
            outcome_ids,
            event_ids: event_ids.to_vec(),
            published_boundary: k + 1,
        };
        let operational = TraceOperational {
            // Wall time is for pacing, health and presentation only.
            wall_time_ns: u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            prepare_request_ids,
            advance_request_id: self
                .last_advance_request
                .clone()
                .unwrap_or_else(|| DomainRequestId::from_serial(0)),
            commit_request_ids: self.last_commit_requests.clone(),
            // SESSION-01 does not record transport correlation ids; a safe retry changes them
            // and nothing in the behaviour above.
            bus_call_ids: Vec::new(),
            delivery_ids: Vec::new(),
        };
        self.trace.transition(TransitionTrace { behaviour, operational });
    }

    // -----------------------------------------------------------------------------------
    // Publication

    async fn publish(
        &mut self,
        topic: &str,
        payload: Map<String, Value>,
        attachments: Vec<(String, flybus::Artifact)>,
    ) -> Outcome<()> {
        let refs: Vec<(&str, &flybus::Artifact)> =
            attachments.iter().map(|(n, a)| (n.as_str(), a)).collect();
        match self.bus.publish(topic, payload, &refs).await {
            Ok(_) => Ok(()),
            Err(e) => {
                // A disconnected or backpressured observer never stalls the world; only a
                // real resource fault reaches here, and it fails the epoch honestly.
                let error = DomainError::new(
                    ErrorCode::BackendFailure,
                    format!("publishing {topic}: {}", e.message),
                    MutationCertainty::None,
                );
                Err(self.fail_now(error, "publish"))
            }
        }
    }

    async fn publish_descriptor(&mut self) -> Outcome<()> {
        let descriptor = self.descriptor.clone().expect("bootstrapped");
        let agents: Vec<Value> = self
            .agents
            .iter()
            .map(|slot| {
                json!({
                    "agentId": slot.agent_id.as_str(),
                    "portId": slot.port_id.as_str(),
                    "profileDigest": slot.profile.digest.as_str(),
                    "tickDuration": slot.tick_duration.to_json(),
                    "warmupTicks": slot.warmup_ticks.to_string(),
                })
            })
            .collect();
        let payload = json!({
            "sessionId": self.session_id.as_str(),
            "revision": "1",
            "compositionDigest": self.composition_digest().as_str(),
            "schedulerId": "lockstep-v1",
            "environment": descriptor.to_json(),
            "taskSchema": self.task.schema().to_json(),
            "agents": agents,
        });
        let topic = self.topics.descriptor.clone();
        self.publish(&topic, match payload {
            Value::Object(m) => m,
            _ => Map::new(),
        }, Vec::new())
        .await
    }

    /// The composition identity: session, epoch, agents, ports and the contract revision.
    pub fn composition_digest(&self) -> Digest {
        let mut text = format!(
            "fly-session/composition-v1\nsession={}\nepoch={}\ncontract={}\n",
            self.session_id,
            self.epoch,
            contract_digest()
        );
        for slot in &self.agents {
            text.push_str(&format!(
                "agent={} port={} profile={}\n",
                slot.agent_id, slot.port_id, slot.profile.digest
            ));
        }
        digest_of_bytes(text.as_bytes())
    }

    async fn publish_events(&mut self, source_step: u64, events: &[TaskEvent]) -> Outcome<()> {
        if events.is_empty() {
            return Ok(());
        }
        let payload = json!({
            "sessionId": self.session_id.as_str(),
            "epoch": self.epoch.as_str(),
            "sourceStep": source_step.to_string(),
            "events": Value::Array(events.iter().map(DomainType::to_json).collect()),
        });
        let topic = self.topics.events.clone();
        self.publish(&topic, match payload {
            Value::Object(m) => m,
            _ => Map::new(),
        }, Vec::new())
        .await
    }

    /// Publishes the committed boundary. Never an in-progress mix of new agent state and an
    /// old world, and never before every agent has committed.
    async fn publish_snapshot(
        &mut self,
        boundary: u64,
        decisions: &BTreeMap<Id, TypedValue>,
        controls: &[PortControl],
        event_ids: &[Id],
    ) -> Outcome<()> {
        if !self.phases.phase().is_committed_boundary() {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::InvalidPhase,
                    "a snapshot represents a committed boundary only",
                ),
                "publish",
            ));
        }
        let observation = self.observation.clone().expect("bootstrapped");
        let agents: Vec<Value> = self
            .agents
            .iter()
            .map(|slot| {
                let control = controls
                    .iter()
                    .find(|c| c.port_id == slot.port_id)
                    .map(|c| c.to_json());
                json!({
                    "agentId": slot.agent_id.as_str(),
                    "selectedDecision": decisions
                        .get(&slot.agent_id)
                        .map(|d| d.to_json()),
                    "appliedControls": control,
                    "committedStep": slot.committed_step.to_string(),
                })
            })
            .collect();
        let payload = json!({
            "descriptorRevision": "1",
            "publisherIncarnation": self.bus.info().connection_id.clone(),
            "scope": self.scope(boundary).to_json(),
            "episodeId": self.episode_id.as_str(),
            "sequence": self.stats.publications.to_string(),
            "worldTime": observation.world_time.to_json(),
            "agents": agents,
            "progress": self.task.progress().to_json(),
            "media": json!({
                "views": Value::Array(observation.broadcast_views.iter().map(DomainType::to_json).collect()),
                "audio": Value::Array(observation.audio.iter().map(DomainType::to_json).collect()),
            }),
            "eventIds": event_ids.iter().map(Id::as_str).collect::<Vec<_>>(),
        });
        // The same owned handles the agents were given, published once for presentation.
        let mut attachments = self.view_attachments();
        attachments.extend(self.audio.iter().map(|(n, a)| (n.clone(), a.clone())));
        let topic = self.topics.snapshots.clone();
        self.publish(
            &topic,
            match payload {
                Value::Object(m) => m,
                _ => Map::new(),
            },
            attachments,
        )
        .await?;
        self.stats.publications += 1;
        self.audit.push(format!("publish:{boundary}"));
        Ok(())
    }
}

// -------------------------------------------------------------------------------------------
// STATE-01: coherent all-participant capture and recovery

/// A capture that exists and has been queued, whose durable outcome has not arrived yet.
///
/// `State.Capture` completes when an immutable capture exists, not when a backend save was
/// requested, and only durable completion produces a saved acknowledgment. Those are two
/// events, so they are two calls.
#[derive(Debug)]
pub struct CaptureTicket {
    pub checkpoint_id: Id,
    pub boundary: u64,
    receiver: tokio::sync::oneshot::Receiver<crate::state::SaveOutcome>,
}

/// What one completed group restore installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreReport {
    pub checkpoint_id: Id,
    pub boundary: u64,
    pub epoch: Id,
    /// Every participant that staged, in the order they were asked.
    pub staged: Vec<Id>,
    /// Every participant that activated, in the order they were asked.
    pub activated: Vec<Id>,
    /// The once-only token each participant staged under, so a caller can prove a token
    /// activates once rather than being told so.
    pub tokens: Vec<(Id, Id)>,
    /// The artifacts the payloads were imported as. None of them existed before this restore.
    pub imported: Vec<String>,
}

/// The payload name the coordinator's own session record travels under.
impl Coordinator {
    /// Attaches the durable store's bounded writer. Without one this session captures nothing
    /// and says so, rather than pretending to.
    pub fn attach_store(&mut self, writer: crate::state::CheckpointWriter) {
        self.writer = Some(writer);
    }

    pub fn writer(&self) -> Option<&crate::state::CheckpointWriter> {
        self.writer.as_ref()
    }

    /// Stops the checkpoint writer and waits for its task.
    pub async fn shutdown_store(&mut self) {
        if let Some(writer) = self.writer.take() {
            writer.shutdown().await;
        }
    }

    /// The last checkpoint whose *saved acknowledgment* this coordinator received, and its
    /// boundary. It moves on durable completion and on nothing else.
    pub fn durable(&self) -> Option<(Id, u64)> {
        self.durable.clone()
    }

    /// Participants that installed a restore in a group install that then failed.
    ///
    /// They hold state no group ever resumed. A further restore is refused until each one has
    /// been replaced, which is how "a failure during activation cannot resume half a world"
    /// survives the next attempt as well as this one.
    pub fn tainted(&self) -> Vec<Id> {
        self.tainted.iter().cloned().collect()
    }

    /// The live composition's compatibility identities.
    pub fn compatibility(&self) -> Outcome<crate::state::Compatibility> {
        match &self.descriptor {
            Some(descriptor) => Ok(crate::state::Compatibility::of(descriptor)),
            None => Err(SessionFailure {
                error: DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the session has no environment descriptor to take compatibility from",
                ),
                phase: self.phases.phase().label(),
                detail: "compatibility".to_owned(),
                participant: None,
            }),
        }
    }

    fn agent_compatibility(&self, slot: &AgentSlot) -> Digest {
        crate::agent::agent_compatibility_digest(
            &slot.agent_id,
            &slot.profile.digest,
            &crate::agent::dataset_digest(),
            crate::agent::MODEL_VERSION,
            crate::agent::PLASTICITY_VERSION,
            slot.seed,
        )
    }

    /// The coordinator's own session record: what it must hold again to resume this boundary.
    ///
    /// `state-media-v1` section 4 puts the next decision state, the admission state and the
    /// event watermarks in the coordinator's own payloads. This is that payload: the
    /// composition has no audience input, so the admission record says so explicitly rather
    /// than being absent.
    fn coordinator_record(&self, boundary: u64) -> Value {
        let (last_source_step, issued) = self.task.event_watermarks();
        json!({
            "payloadVersion": 1,
            "kind": "coordinator",
            "committedStep": boundary.to_string(),
            "admission": {
                "audienceInput": "none-configured",
                "admitted": Value::Array(Vec::new()),
            },
            "eventWatermarks": {
                "lastSourceStep": last_source_step.to_string(),
                "issued": issued.to_string(),
            },
            "audioPositions": Value::Object(
                self.timelines
                    .positions()
                    .into_iter()
                    .map(|(stream, sample)| (stream, Value::String(sample.to_string())))
                    .collect(),
            ),
            "agents": Value::Array(
                self.agents
                    .iter()
                    .map(|slot| json!({
                        "agentId": slot.agent_id.as_str(),
                        "portId": slot.port_id.as_str(),
                        "profile": slot.profile.to_json(),
                        "seed": slot.seed,
                        "workerThreads": slot.worker_threads.to_string(),
                        "committedStep": slot.committed_step.to_string(),
                        "brainTicks": slot.brain_ticks.to_string(),
                        "remainder": slot.remainder.to_json(),
                        "context": slot.context.to_json(),
                    }))
                    .collect(),
            ),
        })
    }

    /// Seals one coordinator-owned payload as an immutable artifact.
    async fn seal_own(&mut self, name: &str, value: &Value) -> Outcome<crate::state::CapturedPayload> {
        let bytes = match canonicalize(value) {
            Ok(text) => text.into_bytes(),
            Err(e) => {
                return Err(self.fail_now(
                    DomainError::invalid(format!("checkpoint payload {name}: {}", e.0)),
                    "capture",
                ));
            }
        };
        let digest = digest_of_bytes(&bytes);
        let artifact = match crate::state::seal_payload(&self.bus, &bytes, &digest).await {
            Ok(artifact) => artifact,
            Err(e) => return Err(self.fail_now(e, "capture")),
        };
        Ok(crate::state::CapturedPayload {
            name: name.to_owned(),
            byte_length: bytes.len() as u64,
            digest,
            artifact,
        })
    }

    /// Takes one coherent all-participant capture at the committed boundary and queues it.
    ///
    /// The queue slot is taken *first*: a saturated writer refuses before a single
    /// `State.Capture` is sent, which is the only way "reject or defer before capture" can be
    /// true rather than aspirational.
    pub async fn capture(&mut self, checkpoint_id: &Id, replaceable: bool) -> Outcome<CaptureTicket> {
        if self.fenced {
            return Err(SessionFailure {
                error: DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the epoch is fenced; a fenced session captures nothing",
                ),
                phase: self.phases.phase().label(),
                detail: "fenced".to_owned(),
                participant: None,
            });
        }
        // A capture that arrives while a transition is in flight is a race, not a bug in the
        // transaction: the supervisor asking for one does not know where the session is. It
        // is refused by name and the epoch is untouched, unlike a phase edge the machine
        // itself takes.
        let Some(boundary) = self.phases.phase().committed_boundary() else {
            self.audit.push(format!("capture-refused:{checkpoint_id}"));
            return Err(SessionFailure {
                error: DomainError::before(
                    ErrorCode::InvalidPhase,
                    "a coherent checkpoint is taken at a committed boundary only",
                ),
                phase: self.phases.phase().label(),
                detail: "capture".to_owned(),
                participant: None,
            });
        };
        let origin = self.phases.phase();
        if self.writer.is_none() {
            return Err(SessionFailure {
                error: DomainError::before(
                    ErrorCode::Unsupported,
                    "this session has no checkpoint store attached",
                ),
                phase: self.phases.phase().label(),
                detail: "capture".to_owned(),
                participant: None,
            });
        }
        // Before anything is captured.
        let reservation = match self.writer.as_ref().expect("checked").reserve() {
            Ok(reservation) => reservation,
            Err(e) => {
                // A refused capture is not a session failure: the boundary stands, the
                // session keeps stepping and the caller is told the queue is full.
                self.audit.push(format!("capture-refused:{checkpoint_id}"));
                return Err(SessionFailure {
                    error: e,
                    phase: self.phases.phase().label(),
                    detail: "capture".to_owned(),
                    participant: None,
                });
            }
        };
        self.transition(Phase::Capturing(boundary))?;
        let result = self
            .capture_group(checkpoint_id, boundary, replaceable, reservation)
            .await;
        match result {
            Ok(ticket) => {
                self.transition(origin)?;
                self.audit.push(format!("captured:{checkpoint_id}@{boundary}"));
                Ok(ticket)
            }
            Err(e) => Err(e),
        }
    }

    async fn capture_group(
        &mut self,
        checkpoint_id: &Id,
        boundary: u64,
        replaceable: bool,
        reservation: crate::state::Reservation,
    ) -> Outcome<CaptureTicket> {
        let scope = self.scope(boundary);
        let compatibility = self.compatibility()?;
        let params = object(CaptureParams { checkpoint_id: checkpoint_id.clone() }.to_json());
        let want = vec![crate::state::PAYLOAD_ATTACHMENT.to_owned()];
        let mut payloads = Vec::new();
        let mut acknowledge: Vec<(WorkerRef, DomainRequestId)> = Vec::new();

        // The world first, then the agents in sorted order: one boundary, every participant.
        let environment = self.environment.clone();
        let reply = self
            .call(
                &environment,
                "State.Capture",
                Some(scope.clone()),
                params.clone(),
                &[],
                &want,
            )
            .await?;
        let world: CaptureResult = reply.parse().map_err(|e| self.fail_now(e, "capture"))?;
        let world_payload = self.accept_capture(
            &reply,
            &world,
            checkpoint_id,
            boundary,
            &compatibility.digest(),
            crate::state::WORLD_PAYLOAD,
        )?;
        payloads.push(world_payload);
        acknowledge.push((environment, reply.request_id.clone()));

        let mut agent_rows = Vec::new();
        for index in 0..self.agents.len() {
            let slot_worker = self.agents[index].worker.clone();
            let agent_id = self.agents[index].agent_id.clone();
            let expected = self.agent_compatibility(&self.agents[index]);
            let reply = self
                .call(
                    &slot_worker,
                    "State.Capture",
                    Some(scope.clone()),
                    params.clone(),
                    &[],
                    &want,
                )
                .await?;
            let captured: CaptureResult = reply.parse().map_err(|e| self.fail_now(e, "capture"))?;
            let name = crate::state::agent_payload(&agent_id);
            let payload =
                self.accept_capture(&reply, &captured, checkpoint_id, boundary, &expected, &name)?;
            payloads.push(payload);
            acknowledge.push((slot_worker, reply.request_id.clone()));
            let slot = &self.agents[index];
            agent_rows.push(crate::state::AgentEntry {
                agent_id: agent_id.clone(),
                profile_digest: slot.profile.digest.clone(),
                dataset_digest: crate::agent::dataset_digest(),
                model_version: crate::agent::MODEL_VERSION.to_owned(),
                plasticity_version: crate::agent::PLASTICITY_VERSION.to_owned(),
                seed: slot.seed,
                brain_ticks: slot.brain_ticks,
                remainder: slot.remainder,
                payload: name,
            });
        }

        // The coordinator's own ledgers, sealed the same way so the writer treats every
        // payload alike.
        let ledger = self.task.capture().map_err(|e| self.fail_now(e, "capture"))?;
        payloads.push(
            self.seal_own(crate::state::TASK_LEDGER_PAYLOAD, &ledger.to_json())
                .await?,
        );
        let inspection = self
            .observation
            .as_ref()
            .map(|observation| observation.inspection.to_json())
            .ok_or_else(|| {
                DomainError::before(ErrorCode::InvalidPhase, "the session never bootstrapped")
            });
        let inspection = match inspection {
            Ok(value) => value,
            Err(e) => return Err(self.fail_now(e, "capture")),
        };
        payloads.push(
            self.seal_own(crate::state::PRIOR_INSPECTION_PAYLOAD, &inspection)
                .await?,
        );
        let mut executor_rows = Vec::new();
        for agent_id in self.agent_ids() {
            let state = {
                let executor = self.executors.get(&agent_id).ok_or_else(|| {
                    DomainError::before(
                        ErrorCode::IdentityMismatch,
                        format!("{agent_id} has no configured action executor"),
                    )
                });
                match executor {
                    Ok(executor) => executor.capture().map_err(|e| (e, agent_id.clone())),
                    Err(e) => Err((e, agent_id.clone())),
                }
            };
            let state = match state {
                Ok(state) => state,
                Err((e, who)) => {
                    self.blame(Some(who));
                    return Err(self.fail_now(e, "capture"));
                }
            };
            let name = crate::state::executor_payload(&agent_id);
            payloads.push(self.seal_own(&name, &state.to_json()).await?);
            executor_rows.push((agent_id, name));
        }
        let record = self.coordinator_record(boundary);
        payloads.push(
            self.seal_own(crate::state::ADMISSION_PAYLOAD, &record)
                .await?,
        );

        let (last_source_step, issued) = self.task.event_watermarks();
        let world_time = match self.observation.as_ref().map(|o| o.world_time) {
            Some(world_time) => Ok(world_time),
            None => Err(self.fail_now(
                DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the session has no observation, so it is at no world time to record",
                ),
                "capture",
            )),
        };
        let manifest = crate::state::CheckpointManifest {
            checkpoint_id: checkpoint_id.clone(),
            source_scope: scope.clone(),
            episode_id: self.episode_id.clone(),
            world_time: world_time?,
            scheduler_id: "lockstep-v1".to_owned(),
            composition_digest: self.composition_digest(),
            port_map: self
                .agents
                .iter()
                .map(|slot| (slot.port_id.clone(), slot.agent_id.clone()))
                .collect(),
            compatibility: compatibility.clone(),
            agents: agent_rows,
            coordinator: crate::state::CoordinatorEntry {
                task_ledger: crate::state::TASK_LEDGER_PAYLOAD.to_owned(),
                prior_inspection: crate::state::PRIOR_INSPECTION_PAYLOAD.to_owned(),
                executor_state: executor_rows,
                admission_state: crate::state::ADMISSION_PAYLOAD.to_owned(),
                event_watermarks: crate::state::EventWatermarks {
                    last_source_step,
                    issued,
                },
            },
            environment: crate::state::EnvironmentEntry {
                worker_id: self.environment.worker_id.clone(),
                payload: crate::state::WORLD_PAYLOAD.to_owned(),
            },
            // No external helper takes part in this composition, and the manifest says so
            // rather than leaving the field out.
            helper_state: Vec::new(),
            payloads: payloads
                .iter()
                .map(|p| (p.name.clone(), p.byte_length, p.digest.clone()))
                .collect(),
        };

        let submission = crate::state::CaptureSubmission {
            checkpoint_id: checkpoint_id.clone(),
            boundary,
            session_id: self.session_id.clone(),
            epoch: self.epoch.clone(),
            episode_id: self.episode_id.clone(),
            compatibility_digest: compatibility.digest(),
            manifest: manifest.to_json(),
            payloads,
            replaceable,
        };
        // The writer takes its own ownership of every payload here. Only then are the
        // workers' cached capture replies released.
        let receiver = {
            let writer = self.writer.as_ref().expect("checked");
            match writer.submit(reservation, submission).await {
                Ok(receiver) => receiver,
                Err(e) => return Err(self.fail_now(e, "capture")),
            }
        };
        self.publish_checkpoint_event("captured", checkpoint_id, boundary, None).await?;
        for (worker, request_id) in acknowledge {
            self.lifecycle_acks.push((worker, request_id));
        }
        self.acknowledge_lifecycle().await?;
        Ok(CaptureTicket {
            checkpoint_id: checkpoint_id.clone(),
            boundary,
            receiver,
        })
    }

    /// Checks one participant's capture and turns it into a payload the writer can own.
    fn accept_capture(
        &mut self,
        reply: &DomainReply,
        result: &CaptureResult,
        checkpoint_id: &Id,
        boundary: u64,
        expected_compatibility: &Digest,
        name: &str,
    ) -> Outcome<crate::state::CapturedPayload> {
        if result.checkpoint_id != *checkpoint_id {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "a capture names another checkpoint",
                ),
                "capture",
            ));
        }
        if result.boundary != boundary {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "a capture names another boundary; one checkpoint is one boundary",
                ),
                "capture",
            ));
        }
        if result.compatibility_digest != *expected_compatibility {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "a capture reports a compatibility identity the composition does not hold",
                ),
                "capture",
            ));
        }
        let digest = match &result.payload.digest {
            Some(digest) => digest.clone(),
            None => {
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::BufferInvalid,
                        "a checkpoint payload must carry a content digest",
                    ),
                    "capture",
                ));
            }
        };
        let artifact = match reply.artifacts.get(crate::state::PAYLOAD_ATTACHMENT) {
            Some(artifact) if artifact.reference() == &result.payload => artifact.clone(),
            _ => {
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::BufferInvalid,
                        "a capture arrived without a live owned handle on its payload",
                    ),
                    "capture",
                ));
            }
        };
        Ok(crate::state::CapturedPayload {
            name: name.to_owned(),
            byte_length: result.payload.byte_length,
            digest,
            artifact,
        })
    }

    /// Waits for one capture's durable outcome and moves the durable mark only on a commit.
    ///
    /// A lost reply is an outcome here, not a hang and not a save: the high-water mark stays
    /// where it was until [`Coordinator::resolve_durable`] asks the store about the same
    /// operation.
    pub async fn await_durable(&mut self, ticket: CaptureTicket) -> Outcome<crate::state::SaveOutcome> {
        let CaptureTicket { checkpoint_id, boundary, receiver } = ticket;
        let budget = self.deadlines.durable;
        let outcome =
            crate::state::CheckpointWriter::wait(receiver, &checkpoint_id, budget).await;
        if outcome.is_durable() {
            self.durable = Some((checkpoint_id.clone(), boundary));
            self.audit.push(format!("durable:{checkpoint_id}@{boundary}"));
        } else {
            // Named rather than lumped together: a failed write, a superseded capture, a lost
            // reply and an expired caller budget are four different things to have to explain.
            self.audit
                .push(format!("not-durable:{}:{checkpoint_id}@{boundary}", outcome.event()));
        }
        Ok(outcome)
    }

    /// Resolves a save whose reply was lost, by asking the store's durable metadata about the
    /// *same* checkpoint. It never saves again.
    ///
    /// `Some(boundary)` means the store manifest lists that generation, which is the durable
    /// commit point; `None` means it does not, and an unreferenced generation file stays
    /// unreferenced.
    pub async fn resolve_durable(&mut self, checkpoint_id: &Id) -> Outcome<Option<u64>> {
        let Some(writer) = self.writer.as_ref() else {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::Unsupported,
                    "this session has no checkpoint store attached",
                ),
                "resolve-durable",
            ));
        };
        let wanted = checkpoint_id.clone();
        let found = writer
            .with_store(move |store| store.lookup(&wanted).map(|g| g.boundary))
            .await;
        match found {
            Some(boundary) => {
                self.durable = Some((checkpoint_id.clone(), boundary));
                self.audit.push(format!("durable:{checkpoint_id}@{boundary}"));
                Ok(Some(boundary))
            }
            None => {
                self.audit.push(format!("not-durable:{checkpoint_id}"));
                Ok(None)
            }
        }
    }

    /// Captures and waits for the durable outcome, which is what an ordinary caller wants.
    pub async fn checkpoint(&mut self, checkpoint_id: &Id) -> Outcome<crate::state::SaveOutcome> {
        let ticket = self.capture(checkpoint_id, false).await?;
        self.await_durable(ticket).await
    }

    async fn publish_checkpoint_event(
        &mut self,
        event: &str,
        checkpoint_id: &Id,
        boundary: u64,
        detail: Option<&str>,
    ) -> Outcome<()> {
        let payload = json!({
            "event": event,
            "sessionId": self.session_id.as_str(),
            "epoch": self.epoch.as_str(),
            "checkpointId": checkpoint_id.as_str(),
            "boundary": boundary.to_string(),
            "detail": detail.map_or(Value::Null, |d| Value::String(d.to_owned())),
        });
        let topic = self.topics.checkpoints.clone();
        self.publish(&topic, object(payload), Vec::new()).await
    }

    // ---------------------------------------------------------------------------------------
    // Recovery

    /// Points the coordinator at a replacement participant while the epoch is fenced.
    ///
    /// A replacement is the only way a fenced participant comes back: `step-v1` section 7's
    /// incarnation row says every live participant of a failed epoch belongs to an invalid
    /// one, so the reference is exchanged deliberately here and never repaired in place.
    pub fn replace_participant(&mut self, worker_id: &Id, worker: WorkerRef) -> Outcome<()> {
        if !self.fenced {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::InvalidPhase,
                    "participants are replaced while the epoch is fenced, not during play",
                ),
                "replace",
            ));
        }
        if worker.worker_id != *worker_id {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    "the replacement reference names another worker",
                ),
                "replace",
            ));
        }
        if self.environment.worker_id == *worker_id {
            self.environment = worker;
            self.tainted.remove(worker_id);
            self.audit.push(format!("replaced:{worker_id}"));
            return Ok(());
        }
        match self.agents.iter_mut().find(|slot| slot.agent_id == *worker_id) {
            Some(slot) => {
                slot.worker = worker;
                self.tainted.remove(worker_id);
                self.audit.push(format!("replaced:{worker_id}"));
                Ok(())
            }
            None => Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("{worker_id} is not a participant of this composition"),
                ),
                "replace",
            )),
        }
    }

    /// Installs a coherent checkpoint into a fresh epoch and lifts the fence.
    ///
    /// The whole of `state-media-v1` section 6 in order: select a complete compatible durable
    /// checkpoint, import its payloads as *new* artifacts, stage every participant, activate
    /// every participant, install the coordinator's own staged state, verify identity and
    /// boundary, flush the old media and parser state, and establish `Paused(k)`. A failure
    /// at any point leaves the fence exactly where it was.
    pub async fn restore(
        &mut self,
        checkpoint_id: Option<&Id>,
        new_epoch: &Id,
    ) -> Outcome<RestoreReport> {
        if self.phases.phase() != Phase::Failed {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::InvalidPhase,
                    "a coherent restore starts from a failed epoch",
                ),
                "restore",
            ));
        }
        if *new_epoch == self.epoch {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::StaleEpoch,
                    "a restore installs a fresh epoch, never the one that failed",
                ),
                "restore",
            ));
        }
        if !self.tainted.is_empty() {
            let who: Vec<&str> = self.tainted.iter().map(String::as_str).collect();
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::InvalidPhase,
                    format!(
                        "{} installed a restore that no group resumed and must be replaced first",
                        who.join(", ")
                    ),
                ),
                "restore",
            ));
        }
        let Some(writer) = self.writer.as_ref() else {
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::Unsupported,
                    "this session has no checkpoint store attached",
                ),
                "restore",
            ));
        };
        let wanted = checkpoint_id.cloned();
        let read = writer
            .with_store(move |store| {
                let record = store.select(wanted.as_ref())?;
                let envelope = store.read(&record)?;
                Ok::<_, DomainError>((record, envelope))
            })
            .await;
        let (record, envelope) = match read {
            Ok(pair) => pair,
            Err(e) => return Err(self.fail_now(e, "restore")),
        };
        let manifest = match crate::state::CheckpointManifest::from_json(&envelope.manifest) {
            Ok(manifest) => manifest,
            Err(e) => {
                return Err(self.fail_now(
                    DomainError::before(ErrorCode::IncompatibleState, e),
                    "restore",
                ));
            }
        };
        if let Err(e) = self.check_restore_identity(&manifest, &record) {
            return Err(self.fail_now(e, "restore"));
        }
        let boundary = manifest.source_scope.step;
        self.transition(Phase::Restoring(boundary))?;
        let outcome = self.restore_group(&envelope, &manifest, new_epoch, boundary).await;
        match outcome {
            Ok(report) => Ok(report),
            Err(e) => Err(e),
        }
    }

    /// Marks every participant of an abandoned group install as one that must be replaced.
    ///
    /// A participant that staged holds a replacement state nothing installed; one that
    /// activated holds installed state no group resumed. Neither is a participant this
    /// session may reuse, and `ipc-v1` section 6's last paragraph is explicit that v1 does
    /// not silently reattach one.
    fn taint_group(&mut self, staged: &[(Id, WorkerRef, Id)]) {
        for (who, _, _) in staged {
            self.tainted.insert(who.clone());
        }
    }

    /// Every identity a restore checks before a single participant is asked to stage.
    fn check_restore_identity(
        &self,
        manifest: &crate::state::CheckpointManifest,
        record: &crate::state::GenerationRecord,
    ) -> DomainResult<()> {
        if manifest.source_scope.session_id != self.session_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the checkpoint belongs to another session",
            ));
        }
        if manifest.episode_id != self.episode_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the checkpoint belongs to another episode",
            ));
        }
        if manifest.scheduler_id != "lockstep-v1" {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                format!(
                    "the checkpoint was scheduled by {}, not lockstep-v1",
                    manifest.scheduler_id
                ),
            ));
        }
        if record.compatibility_digest != manifest.compatibility.digest() {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the store manifest and the envelope disagree about compatibility",
            ));
        }
        let live = match &self.descriptor {
            Some(descriptor) => crate::state::Compatibility::of(descriptor),
            None => {
                return Err(DomainError::before(
                    ErrorCode::InvalidPhase,
                    "the session has no environment descriptor to compare compatibility with",
                ));
            }
        };
        // Names the identity that differs -- backend, content, patch, controller, parser or
        // state format -- rather than one opaque digest mismatch.
        manifest
            .compatibility
            .compare(&live)
            .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e))?;
        let live_ports: Vec<(Id, Id)> = self
            .agents
            .iter()
            .map(|slot| (slot.port_id.clone(), slot.agent_id.clone()))
            .collect();
        if manifest.port_map != live_ports {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the checkpoint's port map is not this composition's",
            ));
        }
        if manifest.environment.worker_id != self.environment.worker_id {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the checkpoint's world is not this composition's environment",
            ));
        }
        let recorded: Vec<Id> = manifest.agents.iter().map(|a| a.agent_id.clone()).collect();
        if recorded != self.agent_ids() {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the checkpoint's agents are not this composition's",
            ));
        }
        for (row, slot) in manifest.agents.iter().zip(&self.agents) {
            if row.profile_digest != slot.profile.digest || row.seed != slot.seed {
                return Err(DomainError::before(
                    ErrorCode::IncompatibleState,
                    format!(
                        "{}'s checkpoint was taken under another profile or seed",
                        row.agent_id
                    ),
                ));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn restore_group(
        &mut self,
        envelope: &fly_session_types::checkpoint::Envelope,
        manifest: &crate::state::CheckpointManifest,
        new_epoch: &Id,
        boundary: u64,
    ) -> Outcome<RestoreReport> {
        let scope = scope_at(&self.session_id, new_epoch, boundary);
        // Step 3: import the payloads as *new* artifacts. Nothing the fence dropped is asked
        // to come back, and a router that restarted has none of the old roots anyway.
        let mut imported: BTreeMap<String, (flybus::Artifact, Digest, u64)> = BTreeMap::new();
        for (name, bytes) in &envelope.payloads {
            let digest = digest_of_bytes(bytes);
            let artifact = match crate::state::seal_payload(&self.bus, bytes, &digest).await {
                Ok(artifact) => artifact,
                Err(e) => return Err(self.fail_now(e, "restore")),
            };
            imported.insert(name.clone(), (artifact, digest, bytes.len() as u64));
        }
        let imported_ids: Vec<String> = imported
            .values()
            .map(|(artifact, _, _)| artifact.reference().artifact_id.clone())
            .collect();

        // Step 3, continued: stage every participant. A refusal anywhere leaves nothing
        // staged that will ever be activated, because the whole install is abandoned.
        let mut staged: Vec<(Id, WorkerRef, Id)> = Vec::new();
        let mut order: Vec<(Id, WorkerRef, String, Digest)> = Vec::new();
        order.push((
            self.environment.worker_id.clone(),
            self.environment.clone(),
            manifest.environment.payload.clone(),
            manifest.compatibility.digest(),
        ));
        for index in 0..self.agents.len() {
            let slot = &self.agents[index];
            let row = manifest
                .agents
                .iter()
                .find(|row| row.agent_id == slot.agent_id)
                .expect("the agent set was checked");
            order.push((
                slot.agent_id.clone(),
                slot.worker.clone(),
                row.payload.clone(),
                crate::agent::agent_compatibility_digest(
                    &row.agent_id,
                    &row.profile_digest,
                    &row.dataset_digest,
                    &row.model_version,
                    &row.plasticity_version,
                    row.seed,
                ),
            ));
        }
        for (who, worker, payload_name, compatibility_digest) in &order {
            let Some((artifact, _, _)) = imported.get(payload_name) else {
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::IncompatibleState,
                        format!("the checkpoint has no payload {payload_name} for {who}"),
                    ),
                    "stage-restore",
                ));
            };
            let params = StageRestoreParams {
                checkpoint_id: manifest.checkpoint_id.clone(),
                source_scope: manifest.source_scope.clone(),
                compatibility_digest: compatibility_digest.clone(),
                payload: artifact.reference().clone(),
            };
            let attachments = [(crate::state::PAYLOAD_ATTACHMENT, artifact)];
            let reply = match self
                .call(
                    worker,
                    "State.StageRestore",
                    Some(scope.clone()),
                    object(params.to_json()),
                    &attachments,
                    &[],
                )
                .await
            {
                Ok(reply) => reply,
                Err(failure) => {
                    // Whoever already staged is holding a replacement state this group will
                    // never install. Nothing is resumed, and none of them is reused.
                    for (done, _, _) in &staged {
                        self.tainted.insert(done.clone());
                    }
                    return Err(failure);
                }
            };
            let result: StageRestoreResult = match reply.parse() {
                Ok(result) => result,
                Err(e) => {
                    for (done, _, _) in &staged {
                        self.tainted.insert(done.clone());
                    }
                    return Err(self.fail_now(e, "stage-restore"));
                }
            };
            if result.checkpoint_id != manifest.checkpoint_id {
                for (done, _, _) in &staged {
                    self.tainted.insert(done.clone());
                }
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::IdentityMismatch,
                        "a staged restore names another checkpoint",
                    ),
                    "stage-restore",
                ));
            }
            self.lifecycle_acks.push((worker.clone(), reply.request_id.clone()));
            self.audit.push(format!("staged:{who}"));
            staged.push((who.clone(), worker.clone(), result.restore_token));
        }

        // `state-media-v1` section 5: activation happens "after every participant **and
        // coordinator** state validates". The coordinator's own staged ledgers are checked
        // here, before a single token is activated, so a checkpoint whose task ledger or
        // executor state is unreadable installs nothing anywhere.
        let payloads: BTreeMap<String, Vec<u8>> = envelope
            .payloads
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.clone()))
            .collect();
        let descriptor = match &self.descriptor {
            Some(descriptor) => descriptor.clone(),
            None => {
                self.taint_group(&staged);
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::InvalidPhase,
                        "the session has no environment descriptor to restore against",
                    ),
                    "restore",
                ));
            }
        };
        if let Err(e) = Coordinator::validate_coordinator_state(
            manifest,
            &payloads,
            &descriptor,
            self.agents.len(),
            &*self.task,
            &self.executors,
        ) {
            self.taint_group(&staged);
            return Err(self.fail_now(e, "restore"));
        }

        // Step 3, last part: activate. Anything that activates before a failure holds state
        // no group resumed, so it is recorded as tainted and must be replaced.
        let mut activated: Vec<Id> = Vec::new();
        let mut restored_observation: Option<(WorldObservation, BTreeMap<String, flybus::Artifact>)> =
            None;
        for (who, worker, token) in &staged {
            let params = ActivateRestoreParams { restore_token: token.clone() };
            let want = if *who == self.environment.worker_id {
                self.media_names.clone()
            } else {
                Vec::new()
            };
            let reply = match self
                .call(
                    worker,
                    "State.ActivateRestore",
                    Some(scope.clone()),
                    object(params.to_json()),
                    &[],
                    &want,
                )
                .await
            {
                Ok(reply) => reply,
                Err(failure) => {
                    self.taint_group(&staged);
                    return Err(failure);
                }
            };
            let result: ActivateRestoreResult = match reply.parse() {
                Ok(result) => result,
                Err(e) => {
                    self.taint_group(&staged);
                    return Err(self.fail_now(e, "activate-restore"));
                }
            };
            let role = if *who == self.environment.worker_id {
                Role::Environment
            } else {
                Role::Agent
            };
            if let Err(e) = result.validate_for_role(role) {
                self.taint_group(&staged);
                return Err(self.fail_now(DomainError::invalid(e.0), "activate-restore"));
            }
            if result.committed_step != boundary || result.checkpoint_id != manifest.checkpoint_id {
                self.taint_group(&staged);
                return Err(self.fail_now(
                    DomainError::before(
                        ErrorCode::IdentityMismatch,
                        "an activation names another boundary or checkpoint",
                    ),
                    "activate-restore",
                ));
            }
            if let Some(observation) = result.observation {
                let (views, audio) = media::split_attachments(reply.artifacts);
                if !audio.is_empty() {
                    self.taint_group(&staged);
                    return Err(self.fail_now(
                        DomainError::new(
                            ErrorCode::BufferInvalid,
                            "a restored observation carried an audio chunk; no interval was played",
                            MutationCertainty::Unknown,
                        ),
                        "activate-restore",
                    ));
                }
                restored_observation = Some((observation, views));
            }
            self.lifecycle_acks.push((worker.clone(), reply.request_id.clone()));
            self.audit.push(format!("activated:{who}"));
            activated.push(who.clone());
        }

        // Step 4: verify identity and boundary, and flush the old media and parser state.
        let Some((observation, views)) = restored_observation else {
            self.taint_group(&staged);
            return Err(self.fail_now(
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "no participant returned the restored world observation",
                ),
                "activate-restore",
            ));
        };
        let install = self.install_restored(
            manifest,
            new_epoch,
            boundary,
            &descriptor,
            observation,
            views,
            &payloads,
        );
        if let Err(e) = install {
            self.taint_group(&staged);
            return Err(self.fail_now(e, "restore"));
        }

        // Step 5: Paused(k), and only now is the fence lifted.
        self.epoch = new_epoch.clone();
        self.transition(Phase::Paused(boundary))?;
        self.fenced = false;
        self.acknowledge_lifecycle().await?;
        self.publish_checkpoint_event("restored", &manifest.checkpoint_id, boundary, None)
            .await?;
        self.publish_descriptor().await?;
        self.publish_snapshot(boundary, &BTreeMap::new(), &[], &[]).await?;
        self.audit.push(format!("restored:{}@{boundary}", manifest.checkpoint_id));
        Ok(RestoreReport {
            checkpoint_id: manifest.checkpoint_id.clone(),
            boundary,
            epoch: new_epoch.clone(),
            staged: staged.iter().map(|(who, _, _)| who.clone()).collect(),
            tokens: staged
                .iter()
                .map(|(who, _, token)| (who.clone(), token.clone()))
                .collect(),
            activated,
            imported: imported_ids,
        })
    }

    /// Reads one of the checkpoint's payloads as JSON, or says which one is unreadable.
    fn read_payload(payloads: &BTreeMap<String, Vec<u8>>, name: &str) -> DomainResult<Value> {
        let bytes = payloads.get(name).ok_or_else(|| {
            DomainError::before(
                ErrorCode::IncompatibleState,
                format!("the checkpoint has no payload {name}"),
            )
        })?;
        serde_json::from_slice(bytes).map_err(|e| {
            DomainError::before(
                ErrorCode::IncompatibleState,
                format!("payload {name} is not JSON: {e}"),
            )
        })
    }

    /// Validates every coordinator-owned payload, changing nothing.
    ///
    /// This runs after the group has staged and before anything activates, which is the order
    /// `state-media-v1` section 5 sets. It is a separate pass from the install below on
    /// purpose: a group that cannot be resumed coherently must not have resumed some of it.
    fn validate_coordinator_state(
        manifest: &crate::state::CheckpointManifest,
        payloads: &BTreeMap<String, Vec<u8>>,
        descriptor: &EnvironmentDescriptor,
        agents: usize,
        task: &dyn crate::task::Task,
        executors: &BTreeMap<Id, Box<dyn ActionExecutor>>,
    ) -> DomainResult<()> {
        let ledger = TypedValue::from_json(&Coordinator::read_payload(
            payloads,
            &manifest.coordinator.task_ledger,
        )?)
        .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e.0))?;
        task.validate_restore(&ledger)?;
        for (agent_id, name) in &manifest.coordinator.executor_state {
            let state = TypedValue::from_json(&Coordinator::read_payload(payloads, name)?)
                .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e.0))?;
            let executor = executors.get(agent_id).ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("{agent_id} has no configured action executor"),
                )
            })?;
            executor.validate_restore(&state)?;
        }
        let inspection = TypedValue::from_json(&Coordinator::read_payload(
            payloads,
            &manifest.coordinator.prior_inspection,
        )?)
        .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e.0))?;
        if inspection.schema != descriptor.inspection_schema {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the recorded prior inspection is not the declared inspection schema",
            ));
        }
        let record =
            Coordinator::read_payload(payloads, &manifest.coordinator.admission_state)?;
        let recorded = record
            .get("agents")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "the coordinator record has no agent list",
                )
            })?;
        if recorded.len() != agents {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the coordinator record names another number of agents",
            ));
        }
        if record.get("audioPositions").and_then(Value::as_object).is_none() {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the coordinator record has no audio positions",
            ));
        }
        Ok(())
    }

    /// Installs the coordinator's own staged state, after every participant activated.
    #[allow(clippy::too_many_arguments)]
    fn install_restored(
        &mut self,
        manifest: &crate::state::CheckpointManifest,
        new_epoch: &Id,
        boundary: u64,
        descriptor: &EnvironmentDescriptor,
        observation: WorldObservation,
        views: BTreeMap<String, flybus::Artifact>,
        payloads: &BTreeMap<String, Vec<u8>>,
    ) -> DomainResult<()> {
        if observation.boundary != boundary {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the restored observation is not at the restored boundary",
            ));
        }
        if observation.world_time != manifest.world_time {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the restored observation's world time is not the checkpoint's",
            ));
        }
        observation
            .validate_against(descriptor)
            .map_err(|e| DomainError::new(ErrorCode::BufferInvalid, e, MutationCertainty::Unknown))?;
        media::check_required_views(descriptor, &observation)?;
        // An installed observation ran no transition, so it carries no chunk. Old epoch audio
        // arriving as current is exactly what this refuses.
        media::check_required_audio(descriptor, &observation, media::ObservationOrigin::Installed)?;
        for view in &observation.sensory_views {
            let name = media::view_attachment(&view.view_id);
            match views.get(&name) {
                Some(artifact) if artifact.reference() == &view.pixels => {}
                _ => {
                    return Err(DomainError::new(
                        ErrorCode::BufferInvalid,
                        format!(
                            "the restored view {} arrived without a live owned handle",
                            view.view_id
                        ),
                        MutationCertainty::Unknown,
                    ));
                }
            }
        }

        let read = |name: &str| Coordinator::read_payload(payloads, name);

        let ledger = TypedValue::from_json(&read(&manifest.coordinator.task_ledger)?)
            .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e.0))?;
        self.task.validate_restore(&ledger)?;
        for (agent_id, name) in &manifest.coordinator.executor_state {
            let state = TypedValue::from_json(&read(name)?)
                .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e.0))?;
            let executor = self.executors.get(agent_id).ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("{agent_id} has no configured action executor"),
                )
            })?;
            executor.validate_restore(&state)?;
        }
        let record = read(&manifest.coordinator.admission_state)?;
        let inspection = TypedValue::from_json(&read(&manifest.coordinator.prior_inspection)?)
            .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e.0))?;
        if inspection.schema != descriptor.inspection_schema {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the recorded prior inspection is not the declared inspection schema",
            ));
        }
        let agents = record
            .get("agents")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "the coordinator record has no agent list",
                )
            })?
            .clone();
        if agents.len() != self.agents.len() {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the coordinator record names another number of agents",
            ));
        }
        let positions = record
            .get("audioPositions")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "the coordinator record has no audio positions",
                )
            })?;
        let mut audio_positions = BTreeMap::new();
        for (stream, value) in positions {
            let sample: u64 = value
                .as_str()
                .ok_or_else(|| {
                    DomainError::before(
                        ErrorCode::IncompatibleState,
                        "a recorded audio position is not a canonical U64",
                    )
                })?
                .parse()
                .map_err(|_| {
                    DomainError::before(
                        ErrorCode::IncompatibleState,
                        "a recorded audio position is not a canonical U64",
                    )
                })?;
            audio_positions.insert(stream.clone(), sample);
        }
        // Everything above validated. From here the coordinator installs, in one pass.
        self.task.install_restore(new_epoch, &ledger)?;
        for (agent_id, name) in &manifest.coordinator.executor_state {
            let state = TypedValue::from_json(&read(name)?)
                .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e.0))?;
            let executor = self
                .executors
                .get_mut(agent_id)
                .expect("checked immediately above");
            executor.install_restore(&state)?;
        }
        for value in &agents {
            let agent_id = value.get("agentId").and_then(Value::as_str).ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "a coordinator agent record has no agentId",
                )
            })?;
            let slot = self
                .agents
                .iter_mut()
                .find(|slot| slot.agent_id == agent_id)
                .ok_or_else(|| {
                    DomainError::before(
                        ErrorCode::IdentityMismatch,
                        format!("the coordinator record names {agent_id}, which is not configured"),
                    )
                })?;
            let context = TypedValue::from_json(value.get("context").ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "a coordinator agent record has no decision context",
                )
            })?)
            .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e.0))?;
            let committed: u64 = value
                .get("committedStep")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    DomainError::before(
                        ErrorCode::IncompatibleState,
                        "a coordinator agent record has no committedStep",
                    )
                })?
                .parse()
                .map_err(|_| {
                    DomainError::before(
                        ErrorCode::IncompatibleState,
                        "a recorded committed step is not a canonical U64",
                    )
                })?;
            if committed != boundary {
                return Err(DomainError::before(
                    ErrorCode::IncompatibleState,
                    "a coordinator agent record is at another boundary",
                ));
            }
            let brain_ticks: u64 = value
                .get("brainTicks")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    DomainError::before(
                        ErrorCode::IncompatibleState,
                        "a coordinator agent record has no brainTicks",
                    )
                })?
                .parse()
                .map_err(|_| {
                    DomainError::before(
                        ErrorCode::IncompatibleState,
                        "a recorded tick count is not a canonical U64",
                    )
                })?;
            let remainder = RationalNs::from_json(value.get("remainder").ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "a coordinator agent record has no remainder",
                )
            })?)
            .map_err(|e| DomainError::before(ErrorCode::IncompatibleState, e.0))?;
            slot.context_digest = context.digest();
            slot.context = context;
            slot.committed_step = committed;
            slot.brain_ticks = brain_ticks;
            slot.remainder = remainder;
            slot.prepared = None;
            slot.prepare_request = None;
        }
        // Old media and old parser state are replaced, never carried: the previous epoch's
        // handles were dropped by the fence and the timelines start again at their preserved
        // sample positions with a discontinuity.
        self.views = views;
        self.pending_views.clear();
        self.audio.clear();
        self.pending_audio.clear();
        self.timelines = AudioTimelines::restored(descriptor, &audio_positions)?;
        self.observation = Some(observation);
        self.episode = None;
        self.last_advance_request = None;
        self.last_commit_requests.clear();
        self.pacing = Some(Pacing::new(descriptor.step_duration));
        Ok(())
    }

    /// The epoch-derived identities of this session's behaviour trace, mapped onto `to_epoch`.
    ///
    /// `step-v1` section 8 compares behaviour across runs. A resumed run runs in a new epoch,
    /// and scope, batch identity and every task event identity are derived from it, so a
    /// comparison either accounts for that or compares nothing. This is what "accounting for
    /// new epoch metadata" is: an explicit, total rewrite of the epoch-derived fields, which
    /// fails rather than passing anything through it does not recognise.
    pub fn rebase(&self, to_epoch: &Id) -> Outcome<EpochRebase> {
        let events = self.task.rebase_ids(to_epoch).map_err(|e| SessionFailure {
            error: e,
            phase: self.phases.phase().label(),
            detail: "rebase".to_owned(),
            participant: None,
        })?;
        Ok(EpochRebase {
            from: self.epoch.clone(),
            to: to_epoch.clone(),
            events,
        })
    }
}
