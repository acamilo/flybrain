//! The closed enums and method payloads of workers-v1.
//!
//! Every bound stated in that document is a constant here, and every constant is named in the
//! canonical schema set, so a bound cannot be changed without changing `contractDigest`.

use flybus::wire::Fields;
use serde_json::Value;

use crate::media::{AudioDescriptor, MAX_VIEWS, ViewDescriptor, ViewRef, audio_list, view_list};
use crate::scalar::{
    DomainRequestId, DomainType, RationalNs, Result, SchemaRef, TypedValue, constant,
    constant_true, enumeration, err, finite, finite_in, i32_field, id_list, is_digest, is_id, list,
    obj, require_same_order, require_unique, u64_json,
};

/// First session composition limit: 4 agents (ipc-v1 section 2).
pub const MAX_AGENTS: usize = 4;
/// First session composition limit: 4 ports.
pub const MAX_PORTS: usize = 4;
/// 64 rate roles per agent.
pub const MAX_RATE_ROLES: usize = 64;
/// Arrays of stimuli or rewards are bounded to 64 per operation (workers-v1 section 1).
pub const MAX_STIMULI: usize = 64;
/// Arrays of stimuli or rewards are bounded to 64 per operation.
pub const MAX_REWARDS: usize = 64;
/// Controller buttons: <=32, unique, fixed order.
pub const MAX_BUTTONS: usize = 32;
/// Controller axes: <=16.
pub const MAX_AXES: usize = 16;
/// Worker.Acknowledge carries 1..=16 request ids, and 16 bounds the unacknowledged replies.
pub const MAX_ACKNOWLEDGE: usize = 16;
/// `engineFrame` is a backend-defined counter of at most 64 characters.
pub const MAX_ENGINE_FRAME_LEN: usize = 64;
/// Negotiated capability ids. Not a stated bound; recorded in the schema set.
pub const MAX_CAPABILITIES: usize = 32;
/// Supported majors in Worker.Hello. Not a stated bound; recorded in the schema set.
pub const MAX_SUPPORTED_MAJORS: usize = 8;
/// Domain error messages are <=512 code points (ipc-v1 section 7).
pub const MAX_MESSAGE_CODE_POINTS: usize = 512;

/// A worker's negotiated role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Agent,
    Environment,
    Coordinator,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Agent => "agent",
            Role::Environment => "environment",
            Role::Coordinator => "coordinator",
        }
    }

    pub fn parse(s: &str) -> Result<Role> {
        match s {
            "agent" => Ok(Role::Agent),
            "environment" => Ok(Role::Environment),
            "coordinator" => Ok(Role::Coordinator),
            _ => err("role must be agent, environment or coordinator"),
        }
    }

    pub const ALL: &'static [&'static str] = &["agent", "environment", "coordinator"];
}

/// The worker phases of Worker.Status (ipc-v1 section 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkerState {
    Uninitialized,
    Ready,
    Preparing,
    Prepared,
    Advancing,
    Committing,
    Capturing,
    StagedRestore,
    Restoring,
    Failed,
    Stopping,
}

impl WorkerState {
    pub const ALL: &'static [&'static str] = &[
        "uninitialized",
        "ready",
        "preparing",
        "prepared",
        "advancing",
        "committing",
        "capturing",
        "staged-restore",
        "restoring",
        "failed",
        "stopping",
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WorkerState::Uninitialized => "uninitialized",
            WorkerState::Ready => "ready",
            WorkerState::Preparing => "preparing",
            WorkerState::Prepared => "prepared",
            WorkerState::Advancing => "advancing",
            WorkerState::Committing => "committing",
            WorkerState::Capturing => "capturing",
            WorkerState::StagedRestore => "staged-restore",
            WorkerState::Restoring => "restoring",
            WorkerState::Failed => "failed",
            WorkerState::Stopping => "stopping",
        }
    }

    pub fn parse(s: &str) -> Result<WorkerState> {
        Ok(match s {
            "uninitialized" => WorkerState::Uninitialized,
            "ready" => WorkerState::Ready,
            "preparing" => WorkerState::Preparing,
            "prepared" => WorkerState::Prepared,
            "advancing" => WorkerState::Advancing,
            "committing" => WorkerState::Committing,
            "capturing" => WorkerState::Capturing,
            "staged-restore" => WorkerState::StagedRestore,
            "restoring" => WorkerState::Restoring,
            "failed" => WorkerState::Failed,
            "stopping" => WorkerState::Stopping,
            _ => return err("state is not one of the eleven worker phases"),
        })
    }
}

/// How an environment recovers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    ExactCheckpoint,
    EpisodeRestart,
}

impl Recovery {
    pub const ALL: &'static [&'static str] = &["exact-checkpoint", "episode-restart"];

    pub fn as_str(self) -> &'static str {
        match self {
            Recovery::ExactCheckpoint => "exact-checkpoint",
            Recovery::EpisodeRestart => "episode-restart",
        }
    }

    pub fn parse(s: &str) -> Result<Recovery> {
        match s {
            "exact-checkpoint" => Ok(Recovery::ExactCheckpoint),
            "episode-restart" => Ok(Recovery::EpisodeRestart),
            _ => err("recovery must be exact-checkpoint or episode-restart"),
        }
    }
}

/// How repeatable an environment claims to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Determinism {
    FixedBuild,
    Unverified,
}

impl Determinism {
    pub const ALL: &'static [&'static str] = &["fixed-build", "unverified"];

    pub fn as_str(self) -> &'static str {
        match self {
            Determinism::FixedBuild => "fixed-build",
            Determinism::Unverified => "unverified",
        }
    }

    pub fn parse(s: &str) -> Result<Determinism> {
        match s {
            "fixed-build" => Ok(Determinism::FixedBuild),
            "unverified" => Ok(Determinism::Unverified),
            _ => err("determinism must be fixed-build or unverified"),
        }
    }
}

/// An axis range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisRange {
    Bipolar,
    Unit,
}

impl AxisRange {
    pub const ALL: &'static [&'static str] = &["bipolar", "unit"];

    pub fn as_str(self) -> &'static str {
        match self {
            AxisRange::Bipolar => "bipolar",
            AxisRange::Unit => "unit",
        }
    }

    pub fn parse(s: &str) -> Result<AxisRange> {
        match s {
            "bipolar" => Ok(AxisRange::Bipolar),
            "unit" => Ok(AxisRange::Unit),
            _ => err("range must be bipolar or unit"),
        }
    }

    pub fn bounds(self) -> (f64, f64) {
        match self {
            AxisRange::Bipolar => (-1.0, 1.0),
            AxisRange::Unit => (0.0, 1.0),
        }
    }

    pub fn contains(self, value: f64) -> bool {
        let (lo, hi) = self.bounds();
        value.is_finite() && (lo..=hi).contains(&value)
    }
}

// ---------------------------------------------------------------------------------------------
// Shared data model (workers-v1 section 1)

/// `AssetRef`: persistent installed content. Never a path or a URL, and never a transient
/// bus `ArtifactRef`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetRef {
    pub id: String,
    pub digest: String,
    pub byte_length: u64,
    pub format: String,
}

impl DomainType for AssetRef {
    const TYPE_NAME: &'static str = "AssetRef";

    fn from_json(value: &Value) -> Result<AssetRef> {
        let mut f = Fields::new(value, "AssetRef")?;
        let id = f.id("id")?;
        let digest = f.string("digest")?.to_owned();
        let byte_length = f.u64_string("byteLength")?;
        let format = f.id("format")?;
        f.finish()?;
        let a = AssetRef {
            id,
            digest,
            byte_length,
            format,
        };
        a.validate()?;
        Ok(a)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("id", self.id.clone().into()),
            ("digest", self.digest.clone().into()),
            ("byteLength", u64_json(self.byte_length)),
            ("format", self.format.clone().into()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.id) {
            return err("AssetRef: id is not a valid id");
        }
        if !is_digest(&self.digest) {
            return err("AssetRef: digest must be 64 lowercase hex digits");
        }
        if self.byte_length == 0 {
            return err("AssetRef: byteLength must be positive");
        }
        if !is_id(&self.format) {
            return err("AssetRef: format is not a valid id");
        }
        Ok(())
    }
}

/// `SensoryInput`: the views this agent may consume at one boundary, plus optional structured
/// input. A pixel-only profile rejects non-null structured input; that check needs the
/// profile, so it is [`SensoryInput::validate_for_profile`].
#[derive(Clone, Debug, PartialEq)]
pub struct SensoryInput {
    pub boundary: u64,
    pub views: Vec<ViewRef>,
    pub structured: Option<TypedValue>,
}

impl SensoryInput {
    /// Views and structured input are separate capabilities (workers-v1 section 1).
    pub fn validate_for_profile(&self, structured_sensing: bool) -> Result<()> {
        self.validate()?;
        if self.structured.is_some() && !structured_sensing {
            return err("SensoryInput: a pixel-only profile rejects non-null structured input");
        }
        Ok(())
    }

    /// Required sensory views must be produced at the boundary the descriptor's declared
    /// delay implies, and their artifacts must have the descriptor's byte shape.
    pub fn validate_against(&self, descriptors: &[ViewDescriptor]) -> Result<()> {
        for view in &self.views {
            let descriptor = descriptors
                .iter()
                .find(|d| d.view_id == view.view_id)
                .ok_or_else(|| {
                    crate::scalar::wire_err(format!(
                        "SensoryInput: view {:?} is not declared by the environment",
                        view.view_id
                    ))
                })?;
            view.validate_against(descriptor, Some(self.boundary))?;
        }
        Ok(())
    }
}

impl DomainType for SensoryInput {
    const TYPE_NAME: &'static str = "SensoryInput";

    fn from_json(value: &Value) -> Result<SensoryInput> {
        let mut f = Fields::new(value, "SensoryInput")?;
        let boundary = f.u64_string("boundary")?;
        let views = view_list(&mut f, "views")?;
        let structured = TypedValue::nullable_from_json(f.value("structured")?)?;
        f.finish()?;
        let s = SensoryInput {
            boundary,
            views,
            structured,
        };
        s.validate()?;
        Ok(s)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("boundary", u64_json(self.boundary)),
            (
                "views",
                Value::Array(self.views.iter().map(ViewRef::to_json).collect()),
            ),
            (
                "structured",
                TypedValue::nullable_to_json(self.structured.as_ref()),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if self.views.len() > MAX_VIEWS {
            return err("SensoryInput: at most 8 views per sensory input");
        }
        require_unique(
            self.views.iter().map(|v| v.view_id.as_str()),
            "SensoryInput.views",
        )?;
        for view in &self.views {
            view.validate()?;
            if view.produced_step > self.boundary {
                return err(format!(
                    "SensoryInput: view {:?} was produced after the observed boundary",
                    view.view_id
                ));
            }
        }
        if let Some(structured) = &self.structured {
            structured.validate()?;
        }
        Ok(())
    }
}

/// `Stimulus`: a profile-declared stimulation kind with a positive finite duration.
#[derive(Clone, Debug, PartialEq)]
pub struct Stimulus {
    pub id: String,
    pub kind_id: String,
    pub duration_ms: f64,
}

impl DomainType for Stimulus {
    const TYPE_NAME: &'static str = "Stimulus";

    fn from_json(value: &Value) -> Result<Stimulus> {
        let mut f = Fields::new(value, "Stimulus")?;
        let id = f.id("id")?;
        let kind_id = f.id("kindId")?;
        let duration_ms = finite(&mut f, "durationMs")?;
        f.finish()?;
        let s = Stimulus {
            id,
            kind_id,
            duration_ms,
        };
        s.validate()?;
        Ok(s)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("id", self.id.clone().into()),
            ("kindId", self.kind_id.clone().into()),
            ("durationMs", Value::from(self.duration_ms)),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.id) || !is_id(&self.kind_id) {
            return err("Stimulus: id and kindId must be valid ids");
        }
        if !self.duration_ms.is_finite() || self.duration_ms <= 0.0 {
            return err("Stimulus: durationMs must be finite and > 0");
        }
        Ok(())
    }
}

/// `Reward`: a finite value attributed to one task event.
#[derive(Clone, Debug, PartialEq)]
pub struct Reward {
    pub event_id: String,
    pub rule_id: String,
    pub value: f64,
}

impl Reward {
    /// Shipped positive-only task profiles reject negatives (workers-v1 section 1).
    pub fn validate_for_profile(&self, positive_only: bool) -> Result<()> {
        self.validate()?;
        if positive_only && self.value < 0.0 {
            return err("Reward: this task profile is positive-only and rejects a negative value");
        }
        Ok(())
    }
}

impl DomainType for Reward {
    const TYPE_NAME: &'static str = "Reward";

    fn from_json(value: &Value) -> Result<Reward> {
        let mut f = Fields::new(value, "Reward")?;
        let event_id = f.id("eventId")?;
        let rule_id = f.id("ruleId")?;
        let v = finite(&mut f, "value")?;
        f.finish()?;
        let r = Reward {
            event_id,
            rule_id,
            value: v,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("eventId", self.event_id.clone().into()),
            ("ruleId", self.rule_id.clone().into()),
            ("value", Value::from(self.value)),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.event_id) || !is_id(&self.rule_id) {
            return err("Reward: eventId and ruleId must be valid ids");
        }
        if !self.value.is_finite() {
            return err("Reward: value must be finite");
        }
        Ok(())
    }
}

/// One tracked role's rate.
#[derive(Clone, Debug, PartialEq)]
pub struct RateSample {
    pub role_id: String,
    pub hz: f64,
}

/// Learning telemetry.
#[derive(Clone, Debug, PartialEq)]
pub struct LearningTelemetry {
    pub enabled: bool,
    pub updates: u64,
    pub changed: u64,
    pub signal: f64,
}

/// `AgentTelemetry`: rates in profile-defined order, unique by role id, at most 64.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentTelemetry {
    pub brain_ticks: u64,
    pub population_rate_hz: f64,
    pub rates: Vec<RateSample>,
    pub learning: LearningTelemetry,
}

impl AgentTelemetry {
    /// Rates are "in profile-defined order": the check needs the profile's role list.
    pub fn validate_against_roles(&self, role_order: &[String]) -> Result<()> {
        self.validate()?;
        require_same_order(
            self.rates.iter().map(|r| r.role_id.as_str()),
            role_order.iter().map(String::as_str),
            "AgentTelemetry.rates",
        )
    }
}

impl DomainType for AgentTelemetry {
    const TYPE_NAME: &'static str = "AgentTelemetry";

    fn from_json(value: &Value) -> Result<AgentTelemetry> {
        let mut f = Fields::new(value, "AgentTelemetry")?;
        let brain_ticks = f.u64_string("brainTicks")?;
        let population_rate_hz = finite_in(&mut f, "populationRateHz", 0.0, f64::MAX)?;
        let rates = list(&mut f, "rates", 0, MAX_RATE_ROLES, |v| {
            let mut r = Fields::new(v, "AgentTelemetry.rates")?;
            let role_id = r.id("roleId")?;
            let hz = finite_in(&mut r, "hz", 0.0, f64::MAX)?;
            r.finish()?;
            Ok(RateSample { role_id, hz })
        })?;
        let learning = {
            let v = f.value("learning")?;
            let mut l = Fields::new(v, "AgentTelemetry.learning")?;
            let enabled = l.boolean("enabled")?;
            let updates = l.u64_string("updates")?;
            let changed = l.u64_string("changed")?;
            let signal = finite(&mut l, "signal")?;
            l.finish()?;
            LearningTelemetry {
                enabled,
                updates,
                changed,
                signal,
            }
        };
        f.finish()?;
        let t = AgentTelemetry {
            brain_ticks,
            population_rate_hz,
            rates,
            learning,
        };
        t.validate()?;
        Ok(t)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("brainTicks", u64_json(self.brain_ticks)),
            ("populationRateHz", Value::from(self.population_rate_hz)),
            (
                "rates",
                Value::Array(
                    self.rates
                        .iter()
                        .map(|r| {
                            obj(vec![
                                ("roleId", r.role_id.clone().into()),
                                ("hz", Value::from(r.hz)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "learning",
                obj(vec![
                    ("enabled", Value::Bool(self.learning.enabled)),
                    ("updates", u64_json(self.learning.updates)),
                    ("changed", u64_json(self.learning.changed)),
                    ("signal", Value::from(self.learning.signal)),
                ]),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if self.rates.len() > MAX_RATE_ROLES {
            return err("AgentTelemetry: at most 64 rate roles per agent");
        }
        require_unique(
            self.rates.iter().map(|r| r.role_id.as_str()),
            "AgentTelemetry.rates",
        )?;
        if !self.population_rate_hz.is_finite() || self.population_rate_hz < 0.0 {
            return err("AgentTelemetry: populationRateHz must be finite and nonnegative");
        }
        for rate in &self.rates {
            if !is_id(&rate.role_id) {
                return err("AgentTelemetry: roleId is not a valid id");
            }
            if !rate.hz.is_finite() || rate.hz < 0.0 {
                return err("AgentTelemetry: rates must be finite and nonnegative");
            }
        }
        if self.learning.changed > self.learning.updates {
            return err("AgentTelemetry: learning.changed cannot exceed learning.updates");
        }
        if !self.learning.signal.is_finite() {
            return err("AgentTelemetry: learning.signal must be finite");
        }
        Ok(())
    }
}

/// A bounded, unique, order-preserving stimulus array.
pub fn stimulus_list(f: &mut Fields<'_>, key: &'static str) -> Result<Vec<Stimulus>> {
    let items = list(f, key, 0, MAX_STIMULI, Stimulus::from_json)?;
    require_unique(items.iter().map(|s| s.id.as_str()), key)?;
    Ok(items)
}

/// A bounded, unique, order-preserving reward array.
pub fn reward_list(f: &mut Fields<'_>, key: &'static str) -> Result<Vec<Reward>> {
    let items = list(f, key, 0, MAX_REWARDS, Reward::from_json)?;
    require_unique(items.iter().map(|r| r.event_id.as_str()), key)?;
    Ok(items)
}

// ---------------------------------------------------------------------------------------------
// Agent methods (workers-v1 section 2)

/// `Agent.Initialize` params.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentInitializeParams {
    pub agent_id: String,
    pub profile: AssetRef,
    pub seed: i32,
    pub initial_input: SensoryInput,
    pub initial_decision_context: TypedValue,
    pub worker_threads: u64,
}

impl DomainType for AgentInitializeParams {
    const TYPE_NAME: &'static str = "AgentInitializeParams";

    fn from_json(value: &Value) -> Result<AgentInitializeParams> {
        let mut f = Fields::new(value, "AgentInitializeParams")?;
        let agent_id = f.id("agentId")?;
        let profile = AssetRef::from_json(f.value("profile")?)?;
        let seed = i32_field(&mut f, "seed")?;
        let initial_input = SensoryInput::from_json(f.value("initialInput")?)?;
        let initial_decision_context = TypedValue::from_json(f.value("initialDecisionContext")?)?;
        let worker_threads = f.int("workerThreads", 1, 4_096)?;
        f.finish()?;
        let p = AgentInitializeParams {
            agent_id,
            profile,
            seed,
            initial_input,
            initial_decision_context,
            worker_threads,
        };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("agentId", self.agent_id.clone().into()),
            ("profile", self.profile.to_json()),
            ("seed", Value::from(i64::from(self.seed))),
            ("initialInput", self.initial_input.to_json()),
            (
                "initialDecisionContext",
                self.initial_decision_context.to_json(),
            ),
            ("workerThreads", Value::from(self.worker_threads)),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.agent_id) {
            return err("AgentInitializeParams: agentId is not a valid id");
        }
        self.profile.validate()?;
        self.initial_input.validate()?;
        self.initial_decision_context.validate()?;
        if self.worker_threads < 1 {
            return err("AgentInitializeParams: workerThreads must be an integer >= 1");
        }
        Ok(())
    }
}

/// `Agent.Initialize` result. Scope is the new epoch at step 0, so `committedStep` is `"0"`.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentInitializeResult {
    pub agent_id: String,
    pub profile_digest: String,
    pub tick_duration: RationalNs,
    pub warmup_ticks: u64,
    pub committed_step: u64,
    pub decision_context_digest: String,
    pub telemetry: AgentTelemetry,
}

impl DomainType for AgentInitializeResult {
    const TYPE_NAME: &'static str = "AgentInitializeResult";

    fn from_json(value: &Value) -> Result<AgentInitializeResult> {
        let mut f = Fields::new(value, "AgentInitializeResult")?;
        let agent_id = f.id("agentId")?;
        let profile_digest = f.string("profileDigest")?.to_owned();
        let tick_duration = RationalNs::from_json(f.value("tickDuration")?)?;
        let warmup_ticks = f.u64_string("warmupTicks")?;
        let committed_step = f.u64_string("committedStep")?;
        let decision_context_digest = f.string("decisionContextDigest")?.to_owned();
        let telemetry = AgentTelemetry::from_json(f.value("telemetry")?)?;
        f.finish()?;
        let r = AgentInitializeResult {
            agent_id,
            profile_digest,
            tick_duration,
            warmup_ticks,
            committed_step,
            decision_context_digest,
            telemetry,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("agentId", self.agent_id.clone().into()),
            ("profileDigest", self.profile_digest.clone().into()),
            ("tickDuration", self.tick_duration.to_json()),
            ("warmupTicks", u64_json(self.warmup_ticks)),
            ("committedStep", u64_json(self.committed_step)),
            (
                "decisionContextDigest",
                self.decision_context_digest.clone().into(),
            ),
            ("telemetry", self.telemetry.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.agent_id) {
            return err("AgentInitializeResult: agentId is not a valid id");
        }
        if !is_digest(&self.profile_digest) || !is_digest(&self.decision_context_digest) {
            return err("AgentInitializeResult: digests must be 64 lowercase hex digits");
        }
        self.tick_duration.validate()?;
        self.tick_duration
            .require_positive("AgentInitializeResult.tickDuration")?;
        if self.committed_step != 0 {
            return err("AgentInitializeResult: committedStep must be \"0\"");
        }
        self.telemetry.validate()
    }
}

/// `Agent.Prepare` params.
#[derive(Clone, Debug, PartialEq)]
pub struct PrepareParams {
    pub agent_id: String,
    pub profile_digest: String,
    pub interval: RationalNs,
    pub decision_context_digest: String,
    pub pre_step_stimulations: Vec<Stimulus>,
}

impl DomainType for PrepareParams {
    const TYPE_NAME: &'static str = "PrepareParams";

    fn from_json(value: &Value) -> Result<PrepareParams> {
        let mut f = Fields::new(value, "PrepareParams")?;
        let agent_id = f.id("agentId")?;
        let profile_digest = f.string("profileDigest")?.to_owned();
        let interval = RationalNs::from_json(f.value("interval")?)?;
        let decision_context_digest = f.string("decisionContextDigest")?.to_owned();
        let pre_step_stimulations = stimulus_list(&mut f, "preStepStimulations")?;
        f.finish()?;
        let p = PrepareParams {
            agent_id,
            profile_digest,
            interval,
            decision_context_digest,
            pre_step_stimulations,
        };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("agentId", self.agent_id.clone().into()),
            ("profileDigest", self.profile_digest.clone().into()),
            ("interval", self.interval.to_json()),
            (
                "decisionContextDigest",
                self.decision_context_digest.clone().into(),
            ),
            (
                "preStepStimulations",
                Value::Array(
                    self.pre_step_stimulations
                        .iter()
                        .map(Stimulus::to_json)
                        .collect(),
                ),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.agent_id) {
            return err("PrepareParams: agentId is not a valid id");
        }
        if !is_digest(&self.profile_digest) || !is_digest(&self.decision_context_digest) {
            return err("PrepareParams: digests must be 64 lowercase hex digits");
        }
        self.interval.validate()?;
        self.interval.require_positive("PrepareParams.interval")?;
        if self.pre_step_stimulations.len() > MAX_STIMULI {
            return err("PrepareParams: at most 64 stimuli per operation");
        }
        require_unique(
            self.pre_step_stimulations.iter().map(|s| s.id.as_str()),
            "PrepareParams.preStepStimulations",
        )?;
        for stimulus in &self.pre_step_stimulations {
            stimulus.validate()?;
        }
        Ok(())
    }
}

/// `Agent.Prepare` result.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedDecision {
    pub agent_id: String,
    pub ticks_advanced: u64,
    pub brain_ticks: u64,
    pub remainder: RationalNs,
    pub decision: TypedValue,
}

impl PreparedDecision {
    /// The remainder is always `>= 0` and `< one model tick` (step-v1 section 5).
    pub fn validate_remainder(&self, tick_duration: &RationalNs) -> Result<()> {
        tick_duration.require_positive("tickDuration")?;
        if self.remainder >= *tick_duration {
            return err("PreparedDecision: remainder must be less than one model tick");
        }
        Ok(())
    }
}

impl DomainType for PreparedDecision {
    const TYPE_NAME: &'static str = "PreparedDecision";

    fn from_json(value: &Value) -> Result<PreparedDecision> {
        let mut f = Fields::new(value, "PreparedDecision")?;
        let agent_id = f.id("agentId")?;
        let ticks_advanced = f.u64_string("ticksAdvanced")?;
        let brain_ticks = f.u64_string("brainTicks")?;
        let remainder = RationalNs::from_json(f.value("remainder")?)?;
        let decision = TypedValue::from_json(f.value("decision")?)?;
        f.finish()?;
        let d = PreparedDecision {
            agent_id,
            ticks_advanced,
            brain_ticks,
            remainder,
            decision,
        };
        d.validate()?;
        Ok(d)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("agentId", self.agent_id.clone().into()),
            ("ticksAdvanced", u64_json(self.ticks_advanced)),
            ("brainTicks", u64_json(self.brain_ticks)),
            ("remainder", self.remainder.to_json()),
            ("decision", self.decision.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.agent_id) {
            return err("PreparedDecision: agentId is not a valid id");
        }
        if self.ticks_advanced > self.brain_ticks {
            return err("PreparedDecision: ticksAdvanced cannot exceed the total brainTicks");
        }
        self.remainder.validate()?;
        self.decision.validate()
    }
}

/// `Agent.Commit` params. `nextInput.boundary` is `k+1`, checked against the request scope by
/// [`CommitParams::validate_against_scope`].
#[derive(Clone, Debug, PartialEq)]
pub struct CommitParams {
    pub agent_id: String,
    pub prepared_request_id: DomainRequestId,
    pub next_input: SensoryInput,
    pub next_decision_context: TypedValue,
    pub rewards: Vec<Reward>,
    pub task_stimulations: Vec<Stimulus>,
}

impl CommitParams {
    /// The commit of transition `k -> k+1` carries `scope.step = k` and the input for `k+1`.
    pub fn validate_against_scope(&self, scope: &crate::scalar::Scope) -> Result<()> {
        self.validate()?;
        let expected = scope
            .step
            .checked_add(1)
            .ok_or_else(|| crate::scalar::wire_err("CommitParams: step overflows U64"))?;
        if self.next_input.boundary != expected {
            return err(format!(
                "CommitParams: nextInput.boundary must be {expected} for scope.step {}",
                scope.step
            ));
        }
        Ok(())
    }
}

impl DomainType for CommitParams {
    const TYPE_NAME: &'static str = "CommitParams";

    fn from_json(value: &Value) -> Result<CommitParams> {
        let mut f = Fields::new(value, "CommitParams")?;
        let agent_id = f.id("agentId")?;
        let prepared_request_id = DomainRequestId::read(&mut f, "preparedRequestId")?;
        let next_input = SensoryInput::from_json(f.value("nextInput")?)?;
        let next_decision_context = TypedValue::from_json(f.value("nextDecisionContext")?)?;
        let rewards = reward_list(&mut f, "rewards")?;
        let task_stimulations = stimulus_list(&mut f, "taskStimulations")?;
        f.finish()?;
        let p = CommitParams {
            agent_id,
            prepared_request_id,
            next_input,
            next_decision_context,
            rewards,
            task_stimulations,
        };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("agentId", self.agent_id.clone().into()),
            ("preparedRequestId", self.prepared_request_id.to_json()),
            ("nextInput", self.next_input.to_json()),
            ("nextDecisionContext", self.next_decision_context.to_json()),
            (
                "rewards",
                Value::Array(self.rewards.iter().map(Reward::to_json).collect()),
            ),
            (
                "taskStimulations",
                Value::Array(
                    self.task_stimulations
                        .iter()
                        .map(Stimulus::to_json)
                        .collect(),
                ),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.agent_id) {
            return err("CommitParams: agentId is not a valid id");
        }
        self.next_input.validate()?;
        self.next_decision_context.validate()?;
        if self.rewards.len() > MAX_REWARDS || self.task_stimulations.len() > MAX_STIMULI {
            return err("CommitParams: at most 64 rewards and 64 stimuli per operation");
        }
        require_unique(
            self.rewards.iter().map(|r| r.event_id.as_str()),
            "CommitParams.rewards",
        )?;
        require_unique(
            self.task_stimulations.iter().map(|s| s.id.as_str()),
            "CommitParams.taskStimulations",
        )?;
        for reward in &self.rewards {
            reward.validate()?;
        }
        for stimulus in &self.task_stimulations {
            stimulus.validate()?;
        }
        Ok(())
    }
}

/// `Agent.Commit` result.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentCommitResult {
    pub agent_id: String,
    pub committed_step: u64,
    pub decision_context_digest: String,
    pub telemetry: AgentTelemetry,
}

impl DomainType for AgentCommitResult {
    const TYPE_NAME: &'static str = "AgentCommitResult";

    fn from_json(value: &Value) -> Result<AgentCommitResult> {
        let mut f = Fields::new(value, "AgentCommitResult")?;
        let agent_id = f.id("agentId")?;
        let committed_step = f.u64_string("committedStep")?;
        let decision_context_digest = f.string("decisionContextDigest")?.to_owned();
        let telemetry = AgentTelemetry::from_json(f.value("telemetry")?)?;
        f.finish()?;
        let r = AgentCommitResult {
            agent_id,
            committed_step,
            decision_context_digest,
            telemetry,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("agentId", self.agent_id.clone().into()),
            ("committedStep", u64_json(self.committed_step)),
            (
                "decisionContextDigest",
                self.decision_context_digest.clone().into(),
            ),
            ("telemetry", self.telemetry.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.agent_id) {
            return err("AgentCommitResult: agentId is not a valid id");
        }
        if !is_digest(&self.decision_context_digest) {
            return err("AgentCommitResult: decisionContextDigest must be 64 lowercase hex digits");
        }
        self.telemetry.validate()
    }
}

// ---------------------------------------------------------------------------------------------
// Environment methods (workers-v1 section 3)

/// One declared axis.
#[derive(Clone, Debug, PartialEq)]
pub struct AxisSchema {
    pub id: String,
    pub range: AxisRange,
    pub neutral: f64,
}

/// `ControllerSchema`: buttons and axes in fixed order, unique, bounded.
#[derive(Clone, Debug, PartialEq)]
pub struct ControllerSchema {
    pub schema: SchemaRef,
    pub buttons: Vec<String>,
    pub axes: Vec<AxisSchema>,
}

impl DomainType for ControllerSchema {
    const TYPE_NAME: &'static str = "ControllerSchema";

    fn from_json(value: &Value) -> Result<ControllerSchema> {
        let mut f = Fields::new(value, "ControllerSchema")?;
        let schema = SchemaRef::from_json(f.value("schema")?)?;
        let buttons = id_list(&mut f, "buttons", 0, MAX_BUTTONS)?;
        let axes = list(&mut f, "axes", 0, MAX_AXES, |v| {
            let mut a = Fields::new(v, "ControllerSchema.axes")?;
            let id = a.id("id")?;
            let range = AxisRange::parse(&enumeration(&mut a, "range", AxisRange::ALL)?)?;
            let (lo, hi) = range.bounds();
            let neutral = finite_in(&mut a, "neutral", lo, hi)?;
            a.finish()?;
            Ok(AxisSchema { id, range, neutral })
        })?;
        f.finish()?;
        let c = ControllerSchema {
            schema,
            buttons,
            axes,
        };
        c.validate()?;
        Ok(c)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("schema", self.schema.to_json()),
            (
                "buttons",
                Value::Array(self.buttons.iter().map(|b| b.clone().into()).collect()),
            ),
            (
                "axes",
                Value::Array(
                    self.axes
                        .iter()
                        .map(|a| {
                            obj(vec![
                                ("id", a.id.clone().into()),
                                ("range", a.range.as_str().into()),
                                ("neutral", Value::from(a.neutral)),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        self.schema.validate()?;
        if self.buttons.len() > MAX_BUTTONS {
            return err("ControllerSchema: at most 32 buttons");
        }
        if self.axes.len() > MAX_AXES {
            return err("ControllerSchema: at most 16 axes");
        }
        require_unique(
            self.buttons.iter().map(String::as_str),
            "ControllerSchema.buttons",
        )?;
        require_unique(
            self.axes.iter().map(|a| a.id.as_str()),
            "ControllerSchema.axes",
        )?;
        for button in &self.buttons {
            if !is_id(button) {
                return err("ControllerSchema: a button id is not a valid id");
            }
        }
        for axis in &self.axes {
            if !is_id(&axis.id) {
                return err("ControllerSchema: an axis id is not a valid id");
            }
            if !axis.range.contains(axis.neutral) {
                return err(format!(
                    "ControllerSchema: axis {:?} neutral must lie in its {} range",
                    axis.id,
                    axis.range.as_str()
                ));
            }
        }
        Ok(())
    }
}

/// One button state in a port control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonState {
    pub id: String,
    pub down: bool,
}

/// One axis value in a port control.
#[derive(Clone, Debug, PartialEq)]
pub struct AxisValue {
    pub id: String,
    pub value: f64,
}

/// `PortControl`: one port's complete control state for one transition.
#[derive(Clone, Debug, PartialEq)]
pub struct PortControl {
    pub port_id: String,
    pub buttons: Vec<ButtonState>,
    pub axes: Vec<AxisValue>,
}

impl PortControl {
    /// "Every active port control must include every declared button and axis in descriptor
    /// order. All IDs must match exactly; no duplicates, extra controls or omissions. Bipolar
    /// axes are finite [-1,1], unit axes [0,1] ... Do not silently clamp an out-of-range
    /// caller value." (workers-v1 section 3)
    pub fn validate_against(&self, controls: &ControllerSchema) -> Result<()> {
        self.validate()?;
        require_same_order(
            self.buttons.iter().map(|b| b.id.as_str()),
            controls.buttons.iter().map(String::as_str),
            "PortControl.buttons",
        )?;
        require_same_order(
            self.axes.iter().map(|a| a.id.as_str()),
            controls.axes.iter().map(|a| a.id.as_str()),
            "PortControl.axes",
        )?;
        for (value, schema) in self.axes.iter().zip(&controls.axes) {
            if !schema.range.contains(value.value) {
                return err(format!(
                    "PortControl: axis {:?} value {} is outside its {} range and is refused, not clamped",
                    value.id,
                    value.value,
                    schema.range.as_str()
                ));
            }
        }
        Ok(())
    }
}

impl DomainType for PortControl {
    const TYPE_NAME: &'static str = "PortControl";

    fn from_json(value: &Value) -> Result<PortControl> {
        let mut f = Fields::new(value, "PortControl")?;
        let port_id = f.id("portId")?;
        let buttons = list(&mut f, "buttons", 0, MAX_BUTTONS, |v| {
            let mut b = Fields::new(v, "PortControl.buttons")?;
            let id = b.id("id")?;
            let down = b.boolean("down")?;
            b.finish()?;
            Ok(ButtonState { id, down })
        })?;
        let axes = list(&mut f, "axes", 0, MAX_AXES, |v| {
            let mut a = Fields::new(v, "PortControl.axes")?;
            let id = a.id("id")?;
            let value = finite(&mut a, "value")?;
            a.finish()?;
            Ok(AxisValue { id, value })
        })?;
        f.finish()?;
        let c = PortControl {
            port_id,
            buttons,
            axes,
        };
        c.validate()?;
        Ok(c)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("portId", self.port_id.clone().into()),
            (
                "buttons",
                Value::Array(
                    self.buttons
                        .iter()
                        .map(|b| {
                            obj(vec![
                                ("id", b.id.clone().into()),
                                ("down", Value::Bool(b.down)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "axes",
                Value::Array(
                    self.axes
                        .iter()
                        .map(|a| {
                            obj(vec![
                                ("id", a.id.clone().into()),
                                ("value", Value::from(a.value)),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.port_id) {
            return err("PortControl: portId is not a valid id");
        }
        if self.buttons.len() > MAX_BUTTONS || self.axes.len() > MAX_AXES {
            return err("PortControl: at most 32 buttons and 16 axes");
        }
        require_unique(
            self.buttons.iter().map(|b| b.id.as_str()),
            "PortControl.buttons",
        )?;
        require_unique(self.axes.iter().map(|a| a.id.as_str()), "PortControl.axes")?;
        for axis in &self.axes {
            if !axis.value.is_finite() {
                return err("PortControl: axis values must be finite");
            }
        }
        Ok(())
    }
}

/// One port and the controller schema it accepts.
#[derive(Clone, Debug, PartialEq)]
pub struct PortDescriptor {
    pub port_id: String,
    pub controls: ControllerSchema,
}

/// `EnvironmentDescriptor`: the fixed shape of one world for one epoch.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentDescriptor {
    pub backend_digest: String,
    pub content_digest: String,
    pub configuration_digest: String,
    pub step_duration: RationalNs,
    pub ports: Vec<PortDescriptor>,
    pub inspection_schema: SchemaRef,
    pub views: Vec<ViewDescriptor>,
    pub audio: Vec<AudioDescriptor>,
    pub recovery: Recovery,
    pub determinism: Determinism,
}

impl EnvironmentDescriptor {
    pub fn port(&self, port_id: &str) -> Option<&PortDescriptor> {
        self.ports.iter().find(|p| p.port_id == port_id)
    }

    pub fn view(&self, view_id: &str) -> Option<&ViewDescriptor> {
        self.views.iter().find(|v| v.view_id == view_id)
    }

    pub fn audio_stream(&self, stream_id: &str) -> Option<&AudioDescriptor> {
        self.audio.iter().find(|a| a.stream_id == stream_id)
    }

    /// One complete batch: every configured port exactly once, in descriptor order, each
    /// control validated against its own schema (step-v1 section 3 phase B).
    pub fn validate_batch(&self, controls: &[PortControl]) -> Result<()> {
        require_same_order(
            controls.iter().map(|c| c.port_id.as_str()),
            self.ports.iter().map(|p| p.port_id.as_str()),
            "Environment.Advance controls",
        )?;
        for (control, port) in controls.iter().zip(&self.ports) {
            control.validate_against(&port.controls)?;
        }
        Ok(())
    }
}

impl DomainType for EnvironmentDescriptor {
    const TYPE_NAME: &'static str = "EnvironmentDescriptor";

    fn from_json(value: &Value) -> Result<EnvironmentDescriptor> {
        let mut f = Fields::new(value, "EnvironmentDescriptor")?;
        let backend_digest = f.string("backendDigest")?.to_owned();
        let content_digest = f.string("contentDigest")?.to_owned();
        let configuration_digest = f.string("configurationDigest")?.to_owned();
        let step_duration = RationalNs::from_json(f.value("stepDuration")?)?;
        let ports = list(&mut f, "ports", 1, MAX_PORTS, |v| {
            let mut p = Fields::new(v, "EnvironmentDescriptor.ports")?;
            let port_id = p.id("portId")?;
            let controls = ControllerSchema::from_json(p.value("controls")?)?;
            p.finish()?;
            Ok(PortDescriptor { port_id, controls })
        })?;
        let inspection_schema = SchemaRef::from_json(f.value("inspectionSchema")?)?;
        let views = list(&mut f, "views", 0, MAX_VIEWS, ViewDescriptor::from_json)?;
        let audio = list(
            &mut f,
            "audio",
            0,
            crate::media::MAX_AUDIO_STREAMS,
            AudioDescriptor::from_json,
        )?;
        let recovery = Recovery::parse(&enumeration(&mut f, "recovery", Recovery::ALL)?)?;
        let determinism =
            Determinism::parse(&enumeration(&mut f, "determinism", Determinism::ALL)?)?;
        f.finish()?;
        let d = EnvironmentDescriptor {
            backend_digest,
            content_digest,
            configuration_digest,
            step_duration,
            ports,
            inspection_schema,
            views,
            audio,
            recovery,
            determinism,
        };
        d.validate()?;
        Ok(d)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("backendDigest", self.backend_digest.clone().into()),
            ("contentDigest", self.content_digest.clone().into()),
            (
                "configurationDigest",
                self.configuration_digest.clone().into(),
            ),
            ("stepDuration", self.step_duration.to_json()),
            (
                "ports",
                Value::Array(
                    self.ports
                        .iter()
                        .map(|p| {
                            obj(vec![
                                ("portId", p.port_id.clone().into()),
                                ("controls", p.controls.to_json()),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("inspectionSchema", self.inspection_schema.to_json()),
            (
                "views",
                Value::Array(self.views.iter().map(ViewDescriptor::to_json).collect()),
            ),
            (
                "audio",
                Value::Array(self.audio.iter().map(AudioDescriptor::to_json).collect()),
            ),
            ("recovery", self.recovery.as_str().into()),
            ("determinism", self.determinism.as_str().into()),
        ])
    }

    fn validate(&self) -> Result<()> {
        for (what, digest) in [
            ("backendDigest", &self.backend_digest),
            ("contentDigest", &self.content_digest),
            ("configurationDigest", &self.configuration_digest),
        ] {
            if !is_digest(digest) {
                return err(format!(
                    "EnvironmentDescriptor: {what} must be 64 lowercase hex digits"
                ));
            }
        }
        self.step_duration.validate()?;
        self.step_duration
            .require_positive("EnvironmentDescriptor.stepDuration")?;
        if self.ports.is_empty() || self.ports.len() > MAX_PORTS {
            return err("EnvironmentDescriptor: 1..=4 ports in the first composition");
        }
        require_unique(
            self.ports.iter().map(|p| p.port_id.as_str()),
            "EnvironmentDescriptor.ports",
        )?;
        for port in &self.ports {
            if !is_id(&port.port_id) {
                return err("EnvironmentDescriptor: portId is not a valid id");
            }
            port.controls.validate()?;
        }
        self.inspection_schema.validate()?;
        if self.views.len() > MAX_VIEWS {
            return err("EnvironmentDescriptor: at most 8 views");
        }
        require_unique(
            self.views.iter().map(|v| v.view_id.as_str()),
            "EnvironmentDescriptor.views",
        )?;
        for view in &self.views {
            view.validate()?;
        }
        require_unique(
            self.audio.iter().map(|a| a.stream_id.as_str()),
            "EnvironmentDescriptor.audio",
        )?;
        for stream in &self.audio {
            stream.validate()?;
        }
        Ok(())
    }
}

/// `Environment.Initialize` params.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentInitializeParams {
    pub backend_config: AssetRef,
    pub task_config: AssetRef,
    pub episode_id: String,
    pub port_bindings: Vec<(String, String)>,
}

impl DomainType for EnvironmentInitializeParams {
    const TYPE_NAME: &'static str = "EnvironmentInitializeParams";

    fn from_json(value: &Value) -> Result<EnvironmentInitializeParams> {
        let mut f = Fields::new(value, "EnvironmentInitializeParams")?;
        let backend_config = AssetRef::from_json(f.value("backendConfig")?)?;
        let task_config = AssetRef::from_json(f.value("taskConfig")?)?;
        let episode_id = f.id("episodeId")?;
        let port_bindings = list(&mut f, "portBindings", 1, MAX_PORTS, |v| {
            let mut b = Fields::new(v, "EnvironmentInitializeParams.portBindings")?;
            let port_id = b.id("portId")?;
            let agent_id = b.id("agentId")?;
            b.finish()?;
            Ok((port_id, agent_id))
        })?;
        f.finish()?;
        let p = EnvironmentInitializeParams {
            backend_config,
            task_config,
            episode_id,
            port_bindings,
        };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("backendConfig", self.backend_config.to_json()),
            ("taskConfig", self.task_config.to_json()),
            ("episodeId", self.episode_id.clone().into()),
            (
                "portBindings",
                Value::Array(
                    self.port_bindings
                        .iter()
                        .map(|(port_id, agent_id)| {
                            obj(vec![
                                ("portId", port_id.clone().into()),
                                ("agentId", agent_id.clone().into()),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        self.backend_config.validate()?;
        self.task_config.validate()?;
        if !is_id(&self.episode_id) {
            return err("EnvironmentInitializeParams: episodeId is not a valid id");
        }
        if self.port_bindings.is_empty() || self.port_bindings.len() > MAX_PORTS {
            return err("EnvironmentInitializeParams: 1..=4 port bindings");
        }
        require_unique(
            self.port_bindings.iter().map(|(p, _)| p.as_str()),
            "EnvironmentInitializeParams.portBindings portId",
        )?;
        require_unique(
            self.port_bindings.iter().map(|(_, a)| a.as_str()),
            "EnvironmentInitializeParams.portBindings agentId",
        )?;
        Ok(())
    }
}

/// `WorldObservation`: one coherent world boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldObservation {
    pub boundary: u64,
    pub world_time: RationalNs,
    pub engine_frame: Option<String>,
    pub sensory_views: Vec<ViewRef>,
    pub inspection: TypedValue,
    pub broadcast_views: Vec<ViewRef>,
    pub audio: Vec<crate::media::AudioRef>,
}

impl WorldObservation {
    /// Byte shapes, producing boundaries and declared identities against the descriptor.
    pub fn validate_against(&self, descriptor: &EnvironmentDescriptor) -> Result<()> {
        self.validate()?;
        if self.inspection.schema != descriptor.inspection_schema {
            return err("WorldObservation: inspection must use the descriptor's inspectionSchema");
        }
        for view in self.sensory_views.iter().chain(&self.broadcast_views) {
            let declared = descriptor.view(&view.view_id).ok_or_else(|| {
                crate::scalar::wire_err(format!(
                    "WorldObservation: view {:?} is not declared by the descriptor",
                    view.view_id
                ))
            })?;
            view.validate_against(declared, Some(self.boundary))?;
        }
        for chunk in &self.audio {
            let declared = descriptor.audio_stream(&chunk.stream_id).ok_or_else(|| {
                crate::scalar::wire_err(format!(
                    "WorldObservation: audio stream {:?} is not declared by the descriptor",
                    chunk.stream_id
                ))
            })?;
            chunk.validate_against(declared)?;
        }
        Ok(())
    }
}

impl DomainType for WorldObservation {
    const TYPE_NAME: &'static str = "WorldObservation";

    fn from_json(value: &Value) -> Result<WorldObservation> {
        let mut f = Fields::new(value, "WorldObservation")?;
        let boundary = f.u64_string("boundary")?;
        let world_time = RationalNs::from_json(f.value("worldTime")?)?;
        let engine_frame =
            crate::scalar::nullable_bounded_string(&mut f, "engineFrame", MAX_ENGINE_FRAME_LEN)?;
        let sensory_views = view_list(&mut f, "sensoryViews")?;
        let inspection = TypedValue::from_json(f.value("inspection")?)?;
        let broadcast_views = view_list(&mut f, "broadcastViews")?;
        let audio = audio_list(&mut f, "audio")?;
        f.finish()?;
        let o = WorldObservation {
            boundary,
            world_time,
            engine_frame,
            sensory_views,
            inspection,
            broadcast_views,
            audio,
        };
        o.validate()?;
        Ok(o)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("boundary", u64_json(self.boundary)),
            ("worldTime", self.world_time.to_json()),
            (
                "engineFrame",
                self.engine_frame.clone().map_or(Value::Null, Value::String),
            ),
            (
                "sensoryViews",
                Value::Array(self.sensory_views.iter().map(ViewRef::to_json).collect()),
            ),
            ("inspection", self.inspection.to_json()),
            (
                "broadcastViews",
                Value::Array(self.broadcast_views.iter().map(ViewRef::to_json).collect()),
            ),
            (
                "audio",
                Value::Array(
                    self.audio
                        .iter()
                        .map(crate::media::AudioRef::to_json)
                        .collect(),
                ),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        self.world_time.validate()?;
        if let Some(frame) = &self.engine_frame
            && frame.chars().count() > MAX_ENGINE_FRAME_LEN
        {
            return err("WorldObservation: engineFrame is at most 64 characters");
        }
        if self.sensory_views.len() > MAX_VIEWS || self.broadcast_views.len() > MAX_VIEWS {
            return err("WorldObservation: at most 8 views per list");
        }
        require_unique(
            self.sensory_views.iter().map(|v| v.view_id.as_str()),
            "WorldObservation.sensoryViews",
        )?;
        require_unique(
            self.broadcast_views.iter().map(|v| v.view_id.as_str()),
            "WorldObservation.broadcastViews",
        )?;
        require_unique(
            self.audio.iter().map(|a| a.stream_id.as_str()),
            "WorldObservation.audio",
        )?;
        for view in self.sensory_views.iter().chain(&self.broadcast_views) {
            view.validate()?;
            if view.produced_step > self.boundary {
                return err("WorldObservation: a view cannot be produced after the boundary");
            }
        }
        for chunk in &self.audio {
            chunk.validate()?;
        }
        self.inspection.validate()
    }
}

/// `Environment.Initialize` result: boundary 0 with world time zero.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentInitializeResult {
    pub descriptor: EnvironmentDescriptor,
    pub observation: WorldObservation,
}

impl DomainType for EnvironmentInitializeResult {
    const TYPE_NAME: &'static str = "EnvironmentInitializeResult";

    fn from_json(value: &Value) -> Result<EnvironmentInitializeResult> {
        let mut f = Fields::new(value, "EnvironmentInitializeResult")?;
        let descriptor = EnvironmentDescriptor::from_json(f.value("descriptor")?)?;
        let observation = WorldObservation::from_json(f.value("observation")?)?;
        f.finish()?;
        let r = EnvironmentInitializeResult {
            descriptor,
            observation,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("descriptor", self.descriptor.to_json()),
            ("observation", self.observation.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        self.descriptor.validate()?;
        self.observation.validate()?;
        if self.observation.boundary != 0 {
            return err("EnvironmentInitializeResult: the initial observation is boundary 0");
        }
        if !self.observation.world_time.is_zero() {
            return err("EnvironmentInitializeResult: initial worldTime is zero (0/1)");
        }
        self.observation.validate_against(&self.descriptor)
    }
}

/// `Environment.Advance` params: exactly one complete batch.
#[derive(Clone, Debug, PartialEq)]
pub struct AdvanceParams {
    pub batch_id: String,
    pub controls: Vec<PortControl>,
}

impl DomainType for AdvanceParams {
    const TYPE_NAME: &'static str = "AdvanceParams";

    fn from_json(value: &Value) -> Result<AdvanceParams> {
        let mut f = Fields::new(value, "AdvanceParams")?;
        let batch_id = f.id("batchId")?;
        let controls = list(&mut f, "controls", 1, MAX_PORTS, PortControl::from_json)?;
        f.finish()?;
        let p = AdvanceParams { batch_id, controls };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("batchId", self.batch_id.clone().into()),
            (
                "controls",
                Value::Array(self.controls.iter().map(PortControl::to_json).collect()),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.batch_id) {
            return err("AdvanceParams: batchId is not a valid id");
        }
        if self.controls.is_empty() || self.controls.len() > MAX_PORTS {
            return err("AdvanceParams: 1..=4 port controls");
        }
        require_unique(
            self.controls.iter().map(|c| c.port_id.as_str()),
            "AdvanceParams.controls",
        )?;
        for control in &self.controls {
            control.validate()?;
        }
        Ok(())
    }
}

/// `Environment.Advance` result.
#[derive(Clone, Debug, PartialEq)]
pub struct StepResult {
    pub batch_id: String,
    pub applied_from_step: u64,
    pub next_step: u64,
    pub applied_controls_digest: String,
    pub observation: WorldObservation,
}

impl StepResult {
    /// The environment returns exactly boundary `k+1` with world time advanced by exactly one
    /// `stepDuration` (workers-v1 section 3, step-v1 section 3 phase B).
    pub fn validate_against(
        &self,
        descriptor: &EnvironmentDescriptor,
        previous: &WorldObservation,
    ) -> Result<()> {
        self.validate()?;
        self.observation.validate_against(descriptor)?;
        let expected = previous.world_time.checked_add(&descriptor.step_duration)?;
        if self.observation.world_time != expected {
            return err("StepResult: worldTime must advance by exactly one stepDuration");
        }
        if self.observation.boundary != previous.boundary + 1 {
            return err("StepResult: the observation must be exactly the next boundary");
        }
        Ok(())
    }
}

impl DomainType for StepResult {
    const TYPE_NAME: &'static str = "StepResult";

    fn from_json(value: &Value) -> Result<StepResult> {
        let mut f = Fields::new(value, "StepResult")?;
        let batch_id = f.id("batchId")?;
        let applied_from_step = f.u64_string("appliedFromStep")?;
        let next_step = f.u64_string("nextStep")?;
        let applied_controls_digest = f.string("appliedControlsDigest")?.to_owned();
        let observation = WorldObservation::from_json(f.value("observation")?)?;
        f.finish()?;
        let r = StepResult {
            batch_id,
            applied_from_step,
            next_step,
            applied_controls_digest,
            observation,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("batchId", self.batch_id.clone().into()),
            ("appliedFromStep", u64_json(self.applied_from_step)),
            ("nextStep", u64_json(self.next_step)),
            (
                "appliedControlsDigest",
                self.applied_controls_digest.clone().into(),
            ),
            ("observation", self.observation.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.batch_id) {
            return err("StepResult: batchId is not a valid id");
        }
        if !is_digest(&self.applied_controls_digest) {
            return err("StepResult: appliedControlsDigest must be 64 lowercase hex digits");
        }
        if self.next_step != self.applied_from_step + 1 {
            return err("StepResult: nextStep must be appliedFromStep + 1; one result is one step");
        }
        self.observation.validate()?;
        if self.observation.boundary != self.next_step {
            return err("StepResult: the observation boundary must be nextStep");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Common worker methods (ipc-v1 section 4, workers-v1 sections 1 and 6)

/// `Worker.Hello` params. Scope is null.
#[derive(Clone, Debug, PartialEq)]
pub struct HelloParams {
    pub session_id: String,
    pub expected_worker_id: String,
    pub role: Role,
    pub supported_majors: Vec<u64>,
}

impl DomainType for HelloParams {
    const TYPE_NAME: &'static str = "HelloParams";

    fn from_json(value: &Value) -> Result<HelloParams> {
        let mut f = Fields::new(value, "HelloParams")?;
        let session_id = f.id("sessionId")?;
        let expected_worker_id = f.id("expectedWorkerId")?;
        let role = Role::parse(&enumeration(&mut f, "role", Role::ALL)?)?;
        let supported_majors = list(
            &mut f,
            "supportedMajors",
            1,
            MAX_SUPPORTED_MAJORS,
            |v| match v.as_u64() {
                Some(n) if (1..=65_535).contains(&n) => Ok(n),
                _ => err("every supported major must be an integer 1..=65535"),
            },
        )?;
        f.finish()?;
        let p = HelloParams {
            session_id,
            expected_worker_id,
            role,
            supported_majors,
        };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("sessionId", self.session_id.clone().into()),
            ("expectedWorkerId", self.expected_worker_id.clone().into()),
            ("role", self.role.as_str().into()),
            (
                "supportedMajors",
                Value::Array(
                    self.supported_majors
                        .iter()
                        .map(|n| Value::from(*n))
                        .collect(),
                ),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.session_id) || !is_id(&self.expected_worker_id) {
            return err("HelloParams: sessionId and expectedWorkerId must be valid ids");
        }
        if self.supported_majors.is_empty() {
            return err("HelloParams: supportedMajors must name at least one major");
        }
        let mut seen: Vec<u64> = Vec::new();
        for major in &self.supported_majors {
            if seen.contains(major) {
                return err("HelloParams: supportedMajors must not repeat a major");
            }
            seen.push(*major);
        }
        Ok(())
    }
}

/// `Worker.Hello` result. v1 selects major 1, minor 0.
#[derive(Clone, Debug, PartialEq)]
pub struct HelloResult {
    pub worker_id: String,
    pub incarnation_id: String,
    pub role: Role,
    pub build_digest: String,
    pub contract_digest: String,
    pub capabilities: Vec<String>,
    pub max_agents: u64,
    pub max_ports: u64,
}

impl HelloResult {
    /// Required capabilities are agent-step-v1 and world-step-v1 for their roles
    /// (ipc-v1 section 4).
    pub fn required_capability(role: Role) -> Option<&'static str> {
        match role {
            Role::Agent => Some("agent-step-v1"),
            Role::Environment => Some("world-step-v1"),
            Role::Coordinator => None,
        }
    }

    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

impl DomainType for HelloResult {
    const TYPE_NAME: &'static str = "HelloResult";

    fn from_json(value: &Value) -> Result<HelloResult> {
        let mut f = Fields::new(value, "HelloResult")?;
        let selected_major = f.int("selectedMajor", 1, 1)?;
        let selected_minor = f.int("selectedMinor", 0, 0)?;
        debug_assert_eq!((selected_major, selected_minor), (1, 0));
        let worker_id = f.id("workerId")?;
        let incarnation_id = f.id("incarnationId")?;
        let role = Role::parse(&enumeration(&mut f, "role", Role::ALL)?)?;
        let build_digest = f.string("buildDigest")?.to_owned();
        let contract_digest = f.string("contractDigest")?.to_owned();
        let capabilities = id_list(&mut f, "capabilities", 0, MAX_CAPABILITIES)?;
        let (max_agents, max_ports) = {
            let v = f.value("limits")?;
            let mut l = Fields::new(v, "HelloResult.limits")?;
            let max_agents = l.int("maxAgents", 1, MAX_AGENTS as u64)?;
            let max_ports = l.int("maxPorts", 1, MAX_PORTS as u64)?;
            l.finish()?;
            (max_agents, max_ports)
        };
        f.finish()?;
        let r = HelloResult {
            worker_id,
            incarnation_id,
            role,
            build_digest,
            contract_digest,
            capabilities,
            max_agents,
            max_ports,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("selectedMajor", Value::from(1u64)),
            ("selectedMinor", Value::from(0u64)),
            ("workerId", self.worker_id.clone().into()),
            ("incarnationId", self.incarnation_id.clone().into()),
            ("role", self.role.as_str().into()),
            ("buildDigest", self.build_digest.clone().into()),
            ("contractDigest", self.contract_digest.clone().into()),
            (
                "capabilities",
                Value::Array(self.capabilities.iter().map(|c| c.clone().into()).collect()),
            ),
            (
                "limits",
                obj(vec![
                    ("maxAgents", Value::from(self.max_agents)),
                    ("maxPorts", Value::from(self.max_ports)),
                ]),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.worker_id) || !is_id(&self.incarnation_id) {
            return err("HelloResult: workerId and incarnationId must be valid ids");
        }
        if !is_digest(&self.build_digest) || !is_digest(&self.contract_digest) {
            return err("HelloResult: buildDigest and contractDigest must be 64 hex digits");
        }
        require_unique(
            self.capabilities.iter().map(String::as_str),
            "HelloResult.capabilities",
        )?;
        if let Some(required) = HelloResult::required_capability(self.role)
            && !self.has(required)
        {
            return err(format!(
                "HelloResult: a {} worker must advertise {required}",
                self.role.as_str()
            ));
        }
        if !(1..=MAX_AGENTS as u64).contains(&self.max_agents)
            || !(1..=MAX_PORTS as u64).contains(&self.max_ports)
        {
            return err("HelloResult: the first composition allows at most 4 agents and 4 ports");
        }
        Ok(())
    }
}

/// `Worker.Status` result.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusResult {
    pub state: WorkerState,
    pub current_scope: Option<crate::scalar::Scope>,
    pub active_request_id: Option<DomainRequestId>,
    pub last_completed_request_id: Option<DomainRequestId>,
    pub last_batch_id: Option<String>,
    pub progress_counter: u64,
}

impl DomainType for StatusResult {
    const TYPE_NAME: &'static str = "StatusResult";

    fn from_json(value: &Value) -> Result<StatusResult> {
        let mut f = Fields::new(value, "StatusResult")?;
        let state = WorkerState::parse(&enumeration(&mut f, "state", WorkerState::ALL)?)?;
        let current_scope = crate::scalar::Scope::nullable_from_json(f.value("currentScope")?)?;
        let active_request_id = DomainRequestId::read_nullable(&mut f, "activeRequestId")?;
        let last_completed_request_id =
            DomainRequestId::read_nullable(&mut f, "lastCompletedRequestId")?;
        let last_batch_id = f.nullable_id("lastBatchId")?;
        let progress_counter = f.u64_string("progressCounter")?;
        f.finish()?;
        let r = StatusResult {
            state,
            current_scope,
            active_request_id,
            last_completed_request_id,
            last_batch_id,
            progress_counter,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("state", self.state.as_str().into()),
            (
                "currentScope",
                crate::scalar::Scope::nullable_to_json(self.current_scope.as_ref()),
            ),
            (
                "activeRequestId",
                self.active_request_id
                    .as_ref()
                    .map_or(Value::Null, DomainRequestId::to_json),
            ),
            (
                "lastCompletedRequestId",
                self.last_completed_request_id
                    .as_ref()
                    .map_or(Value::Null, DomainRequestId::to_json),
            ),
            (
                "lastBatchId",
                self.last_batch_id
                    .clone()
                    .map_or(Value::Null, Value::String),
            ),
            ("progressCounter", u64_json(self.progress_counter)),
        ])
    }

    fn validate(&self) -> Result<()> {
        if let Some(scope) = &self.current_scope {
            scope.validate()?;
        }
        if self.state == WorkerState::Uninitialized && self.current_scope.is_some() {
            return err("StatusResult: an uninitialized worker has a null currentScope");
        }
        if let Some(batch) = &self.last_batch_id
            && !is_id(batch)
        {
            return err("StatusResult: lastBatchId is not a valid id");
        }
        Ok(())
    }
}

/// `Worker.Acknowledge` params: 1..=16 retained lifecycle replies.
#[derive(Clone, Debug, PartialEq)]
pub struct AcknowledgeParams {
    pub request_ids: Vec<DomainRequestId>,
}

impl DomainType for AcknowledgeParams {
    const TYPE_NAME: &'static str = "AcknowledgeParams";

    fn from_json(value: &Value) -> Result<AcknowledgeParams> {
        let mut f = Fields::new(value, "AcknowledgeParams")?;
        let request_ids = list(&mut f, "requestIds", 1, MAX_ACKNOWLEDGE, |v| {
            match v.as_str() {
                Some(s) => DomainRequestId::parse(s),
                None => err("every requestId must be a string"),
            }
        })?;
        f.finish()?;
        let p = AcknowledgeParams { request_ids };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![(
            "requestIds",
            Value::Array(
                self.request_ids
                    .iter()
                    .map(DomainRequestId::to_json)
                    .collect(),
            ),
        )])
    }

    fn validate(&self) -> Result<()> {
        if self.request_ids.is_empty() || self.request_ids.len() > MAX_ACKNOWLEDGE {
            return err("AcknowledgeParams: requestIds carries 1..=16 ids");
        }
        require_unique(
            self.request_ids.iter().map(DomainRequestId::as_str),
            "AcknowledgeParams.requestIds",
        )
    }
}

/// `Worker.Acknowledge` result. Already released or unknown ids are ignored, so the
/// acknowledged list is a subset of the request.
#[derive(Clone, Debug, PartialEq)]
pub struct AcknowledgeResult {
    pub acknowledged: Vec<DomainRequestId>,
}

impl AcknowledgeResult {
    pub fn validate_against(&self, params: &AcknowledgeParams) -> Result<()> {
        self.validate()?;
        for id in &self.acknowledged {
            if !params.request_ids.contains(id) {
                return err(format!(
                    "AcknowledgeResult: {} was not in the request",
                    id.as_str()
                ));
            }
        }
        Ok(())
    }
}

impl DomainType for AcknowledgeResult {
    const TYPE_NAME: &'static str = "AcknowledgeResult";

    fn from_json(value: &Value) -> Result<AcknowledgeResult> {
        let mut f = Fields::new(value, "AcknowledgeResult")?;
        let acknowledged = list(&mut f, "acknowledged", 0, MAX_ACKNOWLEDGE, |v| {
            match v.as_str() {
                Some(s) => DomainRequestId::parse(s),
                None => err("every acknowledged id must be a string"),
            }
        })?;
        f.finish()?;
        let r = AcknowledgeResult { acknowledged };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![(
            "acknowledged",
            Value::Array(
                self.acknowledged
                    .iter()
                    .map(DomainRequestId::to_json)
                    .collect(),
            ),
        )])
    }

    fn validate(&self) -> Result<()> {
        if self.acknowledged.len() > MAX_ACKNOWLEDGE {
            return err("AcknowledgeResult: at most 16 acknowledged ids");
        }
        require_unique(
            self.acknowledged.iter().map(DomainRequestId::as_str),
            "AcknowledgeResult.acknowledged",
        )
    }
}

/// `Worker.Shutdown` params.
#[derive(Clone, Debug, PartialEq)]
pub struct ShutdownParams {
    pub reason: String,
}

impl DomainType for ShutdownParams {
    const TYPE_NAME: &'static str = "ShutdownParams";

    fn from_json(value: &Value) -> Result<ShutdownParams> {
        let mut f = Fields::new(value, "ShutdownParams")?;
        let reason = f.id("reason")?;
        f.finish()?;
        let p = ShutdownParams { reason };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![("reason", self.reason.clone().into())])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.reason) {
            return err("ShutdownParams: reason is not a valid id");
        }
        Ok(())
    }
}

/// `Worker.Shutdown` result. It does not imply saved state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownResult;

impl DomainType for ShutdownResult {
    const TYPE_NAME: &'static str = "ShutdownResult";

    fn from_json(value: &Value) -> Result<ShutdownResult> {
        let mut f = Fields::new(value, "ShutdownResult")?;
        constant_true(&mut f, "stopping")?;
        f.finish()?;
        Ok(ShutdownResult)
    }

    fn to_json(&self) -> Value {
        obj(vec![("stopping", Value::Bool(true))])
    }

    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Task outputs (workers-v1 section 4)

/// A task event: `{id, kindId, sourceStep, agentId, payload}`, deterministic in order.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskEvent {
    pub id: String,
    pub kind_id: String,
    pub source_step: u64,
    pub agent_id: Option<String>,
    pub payload: TypedValue,
}

impl DomainType for TaskEvent {
    const TYPE_NAME: &'static str = "TaskEvent";

    fn from_json(value: &Value) -> Result<TaskEvent> {
        let mut f = Fields::new(value, "TaskEvent")?;
        let id = f.id("id")?;
        let kind_id = f.id("kindId")?;
        let source_step = f.u64_string("sourceStep")?;
        let agent_id = f.nullable_id("agentId")?;
        let payload = TypedValue::from_json(f.value("payload")?)?;
        f.finish()?;
        let e = TaskEvent {
            id,
            kind_id,
            source_step,
            agent_id,
            payload,
        };
        e.validate()?;
        Ok(e)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("id", self.id.clone().into()),
            ("kindId", self.kind_id.clone().into()),
            ("sourceStep", u64_json(self.source_step)),
            (
                "agentId",
                self.agent_id.clone().map_or(Value::Null, Value::String),
            ),
            ("payload", self.payload.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.id) || !is_id(&self.kind_id) {
            return err("TaskEvent: id and kindId must be valid ids");
        }
        if let Some(agent_id) = &self.agent_id
            && !is_id(agent_id)
        {
            return err("TaskEvent: agentId is not a valid id");
        }
        self.payload.validate()
    }
}

/// `episodeRequest`: null, or a terminal request the coordinator may act on.
#[derive(Clone, Debug, PartialEq)]
pub struct EpisodeRequest {
    pub reason: String,
    pub outcome: TypedValue,
}

impl DomainType for EpisodeRequest {
    const TYPE_NAME: &'static str = "EpisodeRequest";

    fn from_json(value: &Value) -> Result<EpisodeRequest> {
        let mut f = Fields::new(value, "EpisodeRequest")?;
        constant(&mut f, "kind", "terminal")?;
        let reason = f.id("reason")?;
        let outcome = TypedValue::from_json(f.value("outcome")?)?;
        f.finish()?;
        let r = EpisodeRequest { reason, outcome };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("kind", "terminal".into()),
            ("reason", self.reason.clone().into()),
            ("outcome", self.outcome.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.reason) {
            return err("EpisodeRequest: reason is not a valid id");
        }
        self.outcome.validate()
    }
}
