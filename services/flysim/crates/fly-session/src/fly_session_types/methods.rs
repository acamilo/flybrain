//! The domain envelope, the error table and every method payload of `ipc-v1` and `workers-v1`.
//!
//! These are the bodies carried inside a Flybus `rpc.call` payload and `rpc.result` outcome.
//! The bus owns framing, routing and artifact ownership; nothing below knows about either.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use super::canonical::canonical_digest;
use super::scalars::{Digest, Id, RationalNs, SchemaRef, Scope, TypedValue, U64};

/// A transient artifact identity, as it appears inside a domain payload.
///
/// Every one of these must also be listed in the surrounding bus attachments and backed by a
/// live owned handle; the reference alone is not authority to read.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArtifactRef(pub flybus::ArtifactRef);

impl Serialize for ArtifactRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.to_json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for ArtifactRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<ArtifactRef, D::Error> {
        let v = Value::deserialize(d)?;
        flybus::ArtifactRef::from_json(&v).map(ArtifactRef).map_err(D::Error::custom)
    }
}

// ---------------------------------------------------------------------------------------------
// Errors

/// `ipc-v1` section 7.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    #[serde(rename = "INVALID_ARGUMENT")]
    InvalidArgument,
    #[serde(rename = "UNSUPPORTED")]
    Unsupported,
    #[serde(rename = "IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[serde(rename = "STALE_EPOCH")]
    StaleEpoch,
    #[serde(rename = "STALE_STEP")]
    StaleStep,
    #[serde(rename = "FUTURE_STEP")]
    FutureStep,
    #[serde(rename = "INVALID_PHASE")]
    InvalidPhase,
    #[serde(rename = "CONFLICT")]
    Conflict,
    #[serde(rename = "IN_PROGRESS")]
    InProgress,
    #[serde(rename = "BUSY")]
    Busy,
    #[serde(rename = "BUFFER_INVALID")]
    BufferInvalid,
    #[serde(rename = "RESULT_EXPIRED")]
    ResultExpired,
    #[serde(rename = "INCOMPATIBLE_STATE")]
    IncompatibleState,
    #[serde(rename = "BACKEND_FAILURE")]
    BackendFailure,
    #[serde(rename = "INTERNAL")]
    Internal,
}

/// How much of the operation had already happened when the error was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mutation {
    None,
    Applied,
    Unknown,
}

/// A domain failure: the code, a bounded message and the mutation certainty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainError {
    pub code: ErrorCode,
    pub message: String,
    pub mutation: Mutation,
}

/// `ipc-v1` section 7: messages are at most 512 code points and carry no raw memory.
pub const MAX_ERROR_MESSAGE_CHARS: usize = 512;

impl DomainError {
    pub fn new(code: ErrorCode, message: impl Into<String>, mutation: Mutation) -> DomainError {
        let message: String = message.into();
        let message = if message.chars().count() > MAX_ERROR_MESSAGE_CHARS {
            message.chars().take(MAX_ERROR_MESSAGE_CHARS).collect()
        } else {
            message
        };
        DomainError { code, message, mutation }
    }

    /// An error raised before anything was mutated.
    pub fn before(code: ErrorCode, message: impl Into<String>) -> DomainError {
        DomainError::new(code, message, Mutation::None)
    }

    pub fn invalid(message: impl Into<String>) -> DomainError {
        DomainError::before(ErrorCode::InvalidArgument, message)
    }
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} ({:?}): {}", self.code, self.mutation, self.message)
    }
}

impl std::error::Error for DomainError {}

pub type DomainResult<T> = Result<T, DomainError>;

// ---------------------------------------------------------------------------------------------
// Envelope

/// The request body of every session RPC: `{requestId, scope, params}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionRpcRequest {
    pub request_id: Id,
    pub scope: Option<Scope>,
    pub params: Map<String, Value>,
}

impl SessionRpcRequest {
    pub fn new(request_id: &RequestId, scope: Option<Scope>, params: Map<String, Value>) -> Self {
        SessionRpcRequest { request_id: request_id.id(), scope, params }
    }

    /// The canonical body digest the deduplication cache compares: method, scope and params,
    /// and nothing that changes between two safe retries of one operation.
    pub fn body_digest(&self, method: &str) -> Digest {
        let mut m = Map::new();
        m.insert("method".into(), method.into());
        m.insert(
            "scope".into(),
            self.scope.as_ref().map_or(Value::Null, |s| {
                serde_json::to_value(s).expect("Scope serializes")
            }),
        );
        m.insert("params".into(), Value::Object(self.params.clone()));
        canonical_digest(&Value::Object(m))
    }

    pub fn to_payload(&self) -> Map<String, Value> {
        match serde_json::to_value(self).expect("SessionRpcRequest serializes") {
            Value::Object(m) => m,
            _ => unreachable!("a struct serializes to an object"),
        }
    }

    pub fn from_payload(payload: &Map<String, Value>) -> Result<SessionRpcRequest, String> {
        serde_json::from_value(Value::Object(payload.clone())).map_err(|e| e.to_string())
    }
}

/// `req-<U64>`: the domain operation identity, independent of the bus `callId`.
///
/// A safe retry keeps this and takes a fresh `callId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

impl RequestId {
    pub fn id(&self) -> Id {
        Id::parse(&format!("req-{}", self.0)).expect("req- plus a U64 is an Id")
    }

    /// Reads the serial back out of `req-<U64>`.
    pub fn parse(id: &Id) -> Result<RequestId, String> {
        let s = id.as_str();
        let rest = s
            .strip_prefix("req-")
            .ok_or_else(|| format!("request id {s:?} is not req-<U64>"))?;
        U64::parse(rest).map(|v| RequestId(v.0))
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "req-{}", self.0)
    }
}

/// The success half of a domain reply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionRpcSuccess {
    #[serde(rename = "type")]
    pub kind: SuccessTag,
    pub request_id: Id,
    pub worker_id: Id,
    pub incarnation_id: Id,
    pub scope: Option<Scope>,
    pub result: Map<String, Value>,
}

/// The failure half of a domain reply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionRpcFailure {
    #[serde(rename = "type")]
    pub kind: FailureTag,
    pub request_id: Id,
    pub worker_id: Id,
    pub incarnation_id: Id,
    pub scope: Option<Scope>,
    pub error: DomainError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuccessTag {
    Result,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureTag {
    Error,
}

/// One terminal domain reply, success or failure.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionRpcOutcome {
    Success(SessionRpcSuccess),
    Failure(SessionRpcFailure),
}

impl SessionRpcOutcome {
    pub fn to_outcome(&self) -> Map<String, Value> {
        let v = match self {
            SessionRpcOutcome::Success(s) => {
                serde_json::to_value(s).expect("success serializes")
            }
            SessionRpcOutcome::Failure(f) => serde_json::to_value(f).expect("failure serializes"),
        };
        match v {
            Value::Object(m) => m,
            _ => unreachable!("a struct serializes to an object"),
        }
    }

    pub fn from_outcome(outcome: &Map<String, Value>) -> Result<SessionRpcOutcome, String> {
        let tag = outcome.get("type").and_then(Value::as_str).unwrap_or_default();
        let v = Value::Object(outcome.clone());
        match tag {
            "result" => serde_json::from_value(v)
                .map(SessionRpcOutcome::Success)
                .map_err(|e| e.to_string()),
            "error" => serde_json::from_value(v)
                .map(SessionRpcOutcome::Failure)
                .map_err(|e| e.to_string()),
            other => Err(format!("domain outcome has type {other:?}")),
        }
    }

    pub fn request_id(&self) -> &Id {
        match self {
            SessionRpcOutcome::Success(s) => &s.request_id,
            SessionRpcOutcome::Failure(f) => &f.request_id,
        }
    }

    pub fn error(&self) -> Option<&DomainError> {
        match self {
            SessionRpcOutcome::Success(_) => None,
            SessionRpcOutcome::Failure(f) => Some(&f.error),
        }
    }

    /// The `result` object of a success, or the domain error.
    pub fn result(&self) -> DomainResult<&Map<String, Value>> {
        match self {
            SessionRpcOutcome::Success(s) => Ok(&s.result),
            SessionRpcOutcome::Failure(f) => Err(f.error.clone()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Common worker methods

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Agent,
    Environment,
    Coordinator,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HelloParams {
    pub session_id: Id,
    pub expected_worker_id: Id,
    pub role: Role,
    pub supported_majors: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HelloResult {
    pub selected_major: u16,
    pub selected_minor: u16,
    pub worker_id: Id,
    pub incarnation_id: Id,
    pub role: Role,
    pub build_digest: Digest,
    pub contract_digest: Digest,
    pub capabilities: Vec<Id>,
    pub limits: HelloLimits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HelloLimits {
    pub max_agents: u32,
    pub max_ports: u32,
}

/// The worker phase names of `ipc-v1` section 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StatusResult {
    pub state: WorkerState,
    pub current_scope: Option<Scope>,
    pub active_request_id: Option<Id>,
    pub last_completed_request_id: Option<Id>,
    pub last_batch_id: Option<Id>,
    pub progress_counter: U64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AcknowledgeParams {
    pub request_ids: Vec<Id>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AcknowledgeResult {
    pub acknowledged: Vec<Id>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ShutdownParams {
    pub reason: Id,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ShutdownResult {
    pub stopping: bool,
}

// ---------------------------------------------------------------------------------------------
// Shared data model (workers-v1 section 1)

/// Persistent installed release content: a profile, a dataset, a backend build.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AssetRef {
    pub id: Id,
    pub digest: Digest,
    pub byte_length: U64,
    pub format: Id,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewDescriptor {
    pub view_id: Id,
    pub width: u32,
    pub height: u32,
    pub format: ViewFormat,
    pub row_stride: u32,
    pub pixel_aspect: PixelAspect,
    pub observation_delay_steps: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewFormat {
    #[serde(rename = "rgba8")]
    Rgba8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelAspect {
    pub numerator: u16,
    pub denominator: u16,
}

impl ViewDescriptor {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=4096).contains(&self.width) || !(1..=4096).contains(&self.height) {
            return Err("view dimensions must be 1..=4096".to_owned());
        }
        if self.row_stride != self.width * 4 {
            return Err("rowStride must be exactly 4 x width; v1 has no padded rows".to_owned());
        }
        if self.pixel_aspect.numerator == 0 || self.pixel_aspect.denominator == 0 {
            return Err("pixel aspect must be positive".to_owned());
        }
        if self.observation_delay_steps > 8 {
            return Err("observationDelaySteps must be 0..=8".to_owned());
        }
        Ok(())
    }

    pub fn byte_length(&self) -> u64 {
        u64::from(self.row_stride) * u64::from(self.height)
    }

    /// `state-media-v1` section 2: the boundary a required sensory view must come from.
    pub fn required_produced_step(&self, boundary: u64) -> u64 {
        boundary.saturating_sub(u64::from(self.observation_delay_steps))
    }
}

/// One view of one boundary, with the bytes behind an owned artifact handle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ViewRef {
    pub view_id: Id,
    pub produced_step: U64,
    pub pixels: ArtifactRef,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioDescriptor {
    pub stream_id: Id,
    pub sample_rate: u32,
    pub channels: u8,
    pub format: AudioFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFormat {
    #[serde(rename = "f32le-interleaved")]
    F32LeInterleaved,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioRef {
    pub stream_id: Id,
    pub first_sample: U64,
    pub sample_frames: u32,
    pub samples: ArtifactRef,
    pub discontinuity: bool,
}

/// Only the views this agent may consume, plus its permitted structured input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensoryInput {
    pub boundary: U64,
    pub views: Vec<ViewRef>,
    pub structured: Option<TypedValue>,
}

/// `workers-v1` section 1: at most 8 views per sensory input.
pub const MAX_VIEWS: usize = 8;
/// `workers-v1` section 1: at most 64 stimuli or rewards per operation.
pub const MAX_EVENT_ARRAY: usize = 64;
/// `ipc-v1` section 2: the first composition's session limits.
pub const MAX_AGENTS: u32 = 4;
/// `ipc-v1` section 2: the first composition's session limits.
pub const MAX_PORTS: u32 = 4;
/// `ipc-v1` section 2: rate roles per agent.
pub const MAX_RATE_ROLES: usize = 64;

impl SensoryInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.views.len() > MAX_VIEWS {
            return Err(format!("{} views, over the limit of {MAX_VIEWS}", self.views.len()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for view in &self.views {
            if !seen.insert(view.view_id.clone()) {
                return Err(format!("view {} appears twice", view.view_id));
            }
        }
        if let Some(structured) = &self.structured {
            structured.validate()?;
        }
        Ok(())
    }
}

/// A profile-declared stimulus kind and duration. Never a neuron index or a drive value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Stimulus {
    pub id: Id,
    pub kind_id: Id,
    pub duration_ms: f64,
}

impl Stimulus {
    pub fn validate(&self) -> Result<(), String> {
        if !self.duration_ms.is_finite() || self.duration_ms <= 0.0 {
            return Err("stimulus durationMs must be finite and > 0".to_owned());
        }
        Ok(())
    }
}

/// One reward value, attributed to the task event and rule that produced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Reward {
    pub event_id: Id,
    pub rule_id: Id,
    pub value: f64,
}

impl Reward {
    pub fn validate(&self) -> Result<(), String> {
        if !self.value.is_finite() {
            return Err("reward value must be finite".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentTelemetry {
    pub brain_ticks: U64,
    pub population_rate_hz: f64,
    pub rates: Vec<RateSample>,
    pub learning: LearningTelemetry,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RateSample {
    pub role_id: Id,
    pub hz: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LearningTelemetry {
    pub enabled: bool,
    pub updates: U64,
    pub changed: U64,
    pub signal: f64,
}

impl AgentTelemetry {
    pub fn validate(&self) -> Result<(), String> {
        if !self.population_rate_hz.is_finite() || self.population_rate_hz < 0.0 {
            return Err("populationRateHz must be finite and nonnegative".to_owned());
        }
        if self.rates.len() > MAX_RATE_ROLES {
            return Err(format!("{} rate roles, over 64", self.rates.len()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for rate in &self.rates {
            if !rate.hz.is_finite() || rate.hz < 0.0 {
                return Err("rate hz must be finite and nonnegative".to_owned());
            }
            if !seen.insert(rate.role_id.clone()) {
                return Err(format!("rate role {} appears twice", rate.role_id));
            }
        }
        if !self.learning.signal.is_finite() {
            return Err("learning signal must be finite".to_owned());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Agent methods (workers-v1 section 2)

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentInitializeParams {
    pub agent_id: Id,
    pub profile: AssetRef,
    pub seed: i32,
    pub initial_input: SensoryInput,
    pub initial_decision_context: TypedValue,
    pub worker_threads: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentInitializeResult {
    pub agent_id: Id,
    pub profile_digest: Digest,
    pub tick_duration: RationalNs,
    pub warmup_ticks: U64,
    pub committed_step: U64,
    pub decision_context_digest: Digest,
    pub telemetry: AgentTelemetry,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PrepareParams {
    pub agent_id: Id,
    pub profile_digest: Digest,
    pub interval: RationalNs,
    pub decision_context_digest: Digest,
    pub pre_step_stimulations: Vec<Stimulus>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PreparedDecision {
    pub agent_id: Id,
    pub ticks_advanced: U64,
    pub brain_ticks: U64,
    pub remainder: RationalNs,
    pub decision: TypedValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CommitParams {
    pub agent_id: Id,
    pub prepared_request_id: Id,
    pub next_input: SensoryInput,
    pub next_decision_context: TypedValue,
    pub rewards: Vec<Reward>,
    pub task_stimulations: Vec<Stimulus>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentCommitResult {
    pub agent_id: Id,
    pub committed_step: U64,
    pub decision_context_digest: Digest,
    pub telemetry: AgentTelemetry,
}

// ---------------------------------------------------------------------------------------------
// Environment methods (workers-v1 section 3)

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ControllerSchema {
    pub schema: SchemaRef,
    pub buttons: Vec<Id>,
    pub axes: Vec<AxisSchema>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AxisSchema {
    pub id: Id,
    pub range: AxisRange,
    pub neutral: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AxisRange {
    Bipolar,
    Unit,
}

impl AxisRange {
    pub fn contains(&self, v: f64) -> bool {
        match self {
            AxisRange::Bipolar => (-1.0..=1.0).contains(&v),
            AxisRange::Unit => (0.0..=1.0).contains(&v),
        }
    }
}

impl ControllerSchema {
    pub fn validate(&self) -> Result<(), String> {
        if self.buttons.len() > 32 {
            return Err("at most 32 buttons".to_owned());
        }
        if self.axes.len() > 16 {
            return Err("at most 16 axes".to_owned());
        }
        let mut seen = std::collections::BTreeSet::new();
        for b in &self.buttons {
            if !seen.insert(b.clone()) {
                return Err(format!("button {b} appears twice"));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for a in &self.axes {
            if !seen.insert(a.id.clone()) {
                return Err(format!("axis {} appears twice", a.id));
            }
            if !a.neutral.is_finite() || !a.range.contains(a.neutral) {
                return Err(format!("axis {} neutral lies outside its range", a.id));
            }
        }
        Ok(())
    }

    /// Checks that `control` names every declared button and axis, in descriptor order, with
    /// every value inside its range. Out-of-range values are refused, never clamped.
    pub fn check(&self, control: &PortControl) -> Result<(), String> {
        if control.buttons.len() != self.buttons.len() {
            return Err(format!(
                "port {} has {} buttons, the schema declares {}",
                control.port_id,
                control.buttons.len(),
                self.buttons.len()
            ));
        }
        for (declared, given) in self.buttons.iter().zip(&control.buttons) {
            if *declared != given.id {
                return Err(format!(
                    "port {} button {} is out of descriptor order; expected {declared}",
                    control.port_id, given.id
                ));
            }
        }
        if control.axes.len() != self.axes.len() {
            return Err(format!(
                "port {} has {} axes, the schema declares {}",
                control.port_id,
                control.axes.len(),
                self.axes.len()
            ));
        }
        for (declared, given) in self.axes.iter().zip(&control.axes) {
            if declared.id != given.id {
                return Err(format!(
                    "port {} axis {} is out of descriptor order; expected {}",
                    control.port_id, given.id, declared.id
                ));
            }
            if !given.value.is_finite() || !declared.range.contains(given.value) {
                return Err(format!(
                    "port {} axis {} value lies outside its declared range",
                    control.port_id, given.id
                ));
            }
        }
        Ok(())
    }

    /// Every button up and every axis at its declared neutral.
    pub fn neutral(&self, port_id: &Id) -> PortControl {
        PortControl {
            port_id: port_id.clone(),
            buttons: self
                .buttons
                .iter()
                .map(|id| ButtonState { id: id.clone(), down: false })
                .collect(),
            axes: self
                .axes
                .iter()
                .map(|a| AxisValue { id: a.id.clone(), value: a.neutral })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ButtonState {
    pub id: Id,
    pub down: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AxisValue {
    pub id: Id,
    pub value: f64,
}

/// One port's complete controller state. The port is assigned by the coordinator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PortControl {
    pub port_id: Id,
    pub buttons: Vec<ButtonState>,
    pub axes: Vec<AxisValue>,
}

/// What an executor returns: buttons and axes, with no port assignment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ControllerIntent {
    pub buttons: Vec<ButtonState>,
    pub axes: Vec<AxisValue>,
}

impl ControllerIntent {
    /// Binds this intent to a port, which only the coordinator may do.
    pub fn at_port(self, port_id: &Id) -> PortControl {
        PortControl { port_id: port_id.clone(), buttons: self.buttons, axes: self.axes }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PortDescriptor {
    pub port_id: Id,
    pub controls: ControllerSchema,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Recovery {
    ExactCheckpoint,
    EpisodeRestart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Determinism {
    FixedBuild,
    Unverified,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnvironmentDescriptor {
    pub backend_digest: Digest,
    pub content_digest: Digest,
    pub configuration_digest: Digest,
    pub step_duration: RationalNs,
    pub ports: Vec<PortDescriptor>,
    pub inspection_schema: SchemaRef,
    pub views: Vec<ViewDescriptor>,
    pub audio: Vec<AudioDescriptor>,
    pub recovery: Recovery,
    pub determinism: Determinism,
}

impl EnvironmentDescriptor {
    pub fn validate(&self) -> Result<(), String> {
        self.step_duration.validate()?;
        if !self.step_duration.is_positive() {
            return Err("stepDuration must be positive".to_owned());
        }
        if self.ports.len() > MAX_PORTS as usize {
            return Err(format!("{} ports, over the limit of {MAX_PORTS}", self.ports.len()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for port in &self.ports {
            if !seen.insert(port.port_id.clone()) {
                return Err(format!("port {} appears twice", port.port_id));
            }
            port.controls.validate()?;
        }
        for view in &self.views {
            view.validate()?;
        }
        Ok(())
    }

    pub fn port(&self, port_id: &Id) -> Option<&PortDescriptor> {
        self.ports.iter().find(|p| p.port_id == *port_id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PortBinding {
    pub port_id: Id,
    pub agent_id: Id,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnvironmentInitializeParams {
    pub backend_config: AssetRef,
    pub task_config: AssetRef,
    pub episode_id: Id,
    pub port_bindings: Vec<PortBinding>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnvironmentInitializeResult {
    pub descriptor: EnvironmentDescriptor,
    pub observation: WorldObservation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorldObservation {
    pub boundary: U64,
    pub world_time: RationalNs,
    pub engine_frame: Option<String>,
    pub sensory_views: Vec<ViewRef>,
    pub inspection: TypedValue,
    pub broadcast_views: Vec<ViewRef>,
    pub audio: Vec<AudioRef>,
}

impl WorldObservation {
    pub fn validate(&self, descriptor: &EnvironmentDescriptor) -> Result<(), String> {
        self.world_time.validate()?;
        self.inspection.validate()?;
        if let Some(frame) = &self.engine_frame
            && frame.chars().count() > 64
        {
            return Err("engineFrame is at most 64 characters".to_owned());
        }
        for view in &descriptor.views {
            let got = self
                .sensory_views
                .iter()
                .find(|v| v.view_id == view.view_id)
                .ok_or_else(|| format!("required sensory view {} is missing", view.view_id))?;
            let want = view.required_produced_step(self.boundary.0);
            if got.produced_step.0 != want {
                return Err(format!(
                    "view {} was produced at boundary {} but the declared delay requires {want}",
                    view.view_id, got.produced_step
                ));
            }
            if got.pixels.0.byte_length != view.byte_length() {
                return Err(format!(
                    "view {} is {} bytes; its descriptor says {}",
                    view.view_id,
                    got.pixels.0.byte_length,
                    view.byte_length()
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdvanceParams {
    pub batch_id: Id,
    pub controls: Vec<PortControl>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StepResult {
    pub batch_id: Id,
    pub applied_from_step: U64,
    pub next_step: U64,
    pub applied_controls_digest: Digest,
    pub observation: WorldObservation,
}

/// The digest of a validated, canonical control batch, in descriptor port order.
pub fn controls_digest(controls: &[PortControl]) -> Digest {
    canonical_digest(&serde_json::to_value(controls).expect("controls serialize"))
}

// ---------------------------------------------------------------------------------------------
// Task outputs (workers-v1 section 4)

/// `{id, kindId, sourceStep, agentId, payload}`, in task-defined deterministic order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskEvent {
    pub id: Id,
    pub kind_id: Id,
    pub source_step: U64,
    pub agent_id: Option<Id>,
    pub payload: TypedValue,
}

/// A deterministic event id from epoch, source step, rule and ordinal.
pub fn event_id(epoch: &Id, source_step: u64, rule: &str, ordinal: u32) -> Id {
    let digest = Digest::of(format!("{epoch}\n{source_step}\n{rule}\n{ordinal}\n").as_bytes());
    Id::parse(&format!("ev-{}", &digest.as_str()[..16])).expect("ev- plus hex is an Id")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EpisodeRequest {
    pub kind: EpisodeKind,
    pub reason: Id,
    pub outcome: TypedValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EpisodeKind {
    Terminal,
}

/// Everything the task routes to one agent for this transition. Always explicit, even empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentOutcome {
    pub rewards: Vec<Reward>,
    pub stimulations: Vec<Stimulus>,
}
