//! The domain types this crate uses, all from the shared `fly-session-types` crate.
//!
//! CONTRACT-01 owns the scalars, the method payloads, their validation, the canonical digests
//! and the trace format. This module is only a facade over that crate plus the few things a
//! coordinator needs that are not part of the contract: a session-side error value, the
//! synthetic composition's schema and event-id derivations, and a log of recorded traces.
//!
//! `Id` and `Digest` are type aliases, because the shared crate carries both as validated
//! `String`s from `flybus::wire` rather than forking the encodings into newtypes.

use serde_json::{Map, Value};

pub use fly_session_types::ArtifactRef;
pub use fly_session_types::canonical::{
    self, OperationKey, body_digest, canonicalize, digest_of, sha256_hex,
};
pub use fly_session_types::media::{AudioDescriptor, AudioRef, ViewDescriptor, ViewRef};
pub use fly_session_types::rpc::{
    ErrorCode, MutationCertainty, SessionRpcFailure, SessionRpcOutcome, SessionRpcRequest,
    SessionRpcSuccess,
};
pub use fly_session_types::scalar::{
    ArtifactIdentity, BusCallId, DomainRequestId, DomainType, MAX_TYPED_VALUE_BYTES, RationalNs,
    SchemaRef, Scope, TypedValue, is_digest, is_id,
};
pub use fly_session_types::schema::contract_digest;
pub use fly_session_types::trace::{
    TraceAgent, TraceBehaviour, TraceObservation, TraceOperational, TraceRequest, TransitionTrace,
};
pub use fly_session_types::publishing::{
    AgentDescriptor, CommittedSnapshot, SessionDescriptor, SnapshotAgent,
};
pub use fly_session_types::workers::{
    AcknowledgeParams, AcknowledgeResult, AdvanceParams, AgentCommitResult, AgentInitializeParams,
    AgentGraph, AgentInitializeResult, AgentTelemetry, AssetRef, AxisRange, AxisSchema, AxisValue, ButtonState,
    CommitParams, ControllerSchema, Determinism, EnvironmentDescriptor,
    EnvironmentInitializeParams, EnvironmentInitializeResult, EpisodeRequest, HelloParams,
    HelloResult, LearningTelemetry, MAX_ACKNOWLEDGE, MAX_AGENTS, MAX_PORTS, MAX_RATE_ROLES,
    MAX_REWARDS, MAX_STIMULI, PortControl, PortDescriptor, PrepareParams, PreparedDecision,
    RateSample, Recovery, Reward, Role, SensoryInput, ShutdownParams, ShutdownResult, StatusResult,
    StepResult, Stimulus, TaskEvent, WorkerState, WorldObservation,
};

/// A validated `Id`: `^[a-z0-9][a-z0-9._-]{0,63}$`, the bus encoding the contracts reuse.
pub type Id = String;

/// 64 lowercase hexadecimal digits: a SHA-256.
pub type Digest = String;

/// An `Id` from a trusted literal. It panics on a malformed one, which is a bug in the
/// composition rather than a runtime condition.
pub fn id(s: &str) -> Id {
    assert!(is_id(s), "{s:?} is not a valid Id");
    s.to_owned()
}

/// An `Id` from an untrusted string.
pub fn parse_id(s: &str) -> Result<Id, String> {
    if is_id(s) {
        Ok(s.to_owned())
    } else {
        Err(format!("{s:?} is not a valid Id"))
    }
}

/// The SHA-256 of some bytes, hex-encoded.
pub fn digest_of_bytes(bytes: &[u8]) -> Digest {
    sha256_hex(bytes)
}

/// A `Scope` from trusted composition values.
pub fn scope_at(session_id: &str, epoch: &str, step: u64) -> Scope {
    Scope::new(session_id, epoch, step).expect("a composition scope is valid")
}

/// `hz` steps per second as an exact nanosecond duration.
pub fn hz(hz: u64) -> Result<RationalNs, String> {
    RationalNs::reduced(1_000_000_000, u128::from(hz)).map_err(|e| e.0)
}

/// A whole number of milliseconds as an exact nanosecond duration.
pub fn millis(ms: u64) -> Result<RationalNs, String> {
    RationalNs::reduced(u128::from(ms) * 1_000_000, 1).map_err(|e| e.0)
}

/// A schema reference whose digest is derived from its own name and version, so the synthetic
/// composition has stable identities without a schema registry file.
pub fn synthetic_schema(name: &str, version: u16) -> SchemaRef {
    let digest = digest_of_bytes(format!("fly-session-schema-v1\n{name}\n{version}\n").as_bytes());
    SchemaRef::new(name, version, &digest).expect("a synthetic schema reference is valid")
}

/// A deterministic task event id from epoch, source step, rule and ordinal.
pub fn event_id(epoch: &str, source_step: u64, rule: &str, ordinal: u32) -> Id {
    let digest = digest_of_bytes(format!("{epoch}\n{source_step}\n{rule}\n{ordinal}\n").as_bytes());
    id(&format!("ev-{}", &digest[..16]))
}

/// The digest of a validated, canonical control batch, in descriptor port order.
pub fn controls_digest(controls: &[PortControl]) -> Digest {
    let array = Value::Array(controls.iter().map(DomainType::to_json).collect());
    digest_of(&array).expect("a validated control batch canonicalizes")
}

/// The canonical digest of a typed value, schema identity included.
pub fn typed_digest(value: &TypedValue) -> Digest {
    digest_of(&value.to_json()).expect("a validated typed value canonicalizes")
}

/// The byte length one view's artifact must have.
pub fn view_byte_length(descriptor: &ViewDescriptor) -> u64 {
    descriptor.row_stride * descriptor.height
}

/// The boundary a required sensory view must have been produced at.
pub fn required_produced_step(descriptor: &ViewDescriptor, boundary: u64) -> u64 {
    boundary.saturating_sub(descriptor.observation_delay_steps)
}

/// Reads a finite number out of a typed value.
pub fn typed_number(value: &TypedValue, key: &str) -> Result<f64, String> {
    value
        .value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("typed value has no finite number {key:?}"))
}

/// Reads an integer out of a typed value.
pub fn typed_integer(value: &TypedValue, key: &str) -> Result<i64, String> {
    value
        .value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("typed value has no integer {key:?}"))
}

/// Builds a typed value from a JSON object, refusing one the contract would reject.
pub fn typed(schema: SchemaRef, value: Value) -> Result<TypedValue, String> {
    TypedValue::new(schema, value).map_err(|e| e.0)
}

/// The object form of a payload, for a bus `payload` or `outcome` field.
pub fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// `ipc-v1` section 7: the domain error a worker returns, with its mutation certainty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainError {
    pub code: ErrorCode,
    pub message: String,
    pub mutation: MutationCertainty,
}

/// `ipc-v1` section 7: messages are at most 512 code points and carry no raw memory.
pub const MAX_ERROR_MESSAGE_CHARS: usize = 512;

impl DomainError {
    pub fn new(
        code: ErrorCode,
        message: impl std::fmt::Display,
        mutation: MutationCertainty,
    ) -> DomainError {
        let message: String = message.to_string();
        let message = if message.chars().count() > MAX_ERROR_MESSAGE_CHARS {
            message.chars().take(MAX_ERROR_MESSAGE_CHARS).collect()
        } else {
            message
        };
        DomainError { code, message, mutation }
    }

    /// An error raised before anything was mutated.
    pub fn before(code: ErrorCode, message: impl std::fmt::Display) -> DomainError {
        DomainError::new(code, message, MutationCertainty::None)
    }

    pub fn invalid(message: impl std::fmt::Display) -> DomainError {
        DomainError::before(ErrorCode::InvalidArgument, message)
    }
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.code.as_str(), self.mutation.as_str(), self.message)
    }
}

impl std::error::Error for DomainError {}

pub type DomainResult<T> = Result<T, DomainError>;

/// The success `result` of a terminal outcome, or its domain error.
pub fn outcome_result(outcome: &SessionRpcOutcome) -> DomainResult<&Value> {
    match outcome {
        SessionRpcOutcome::Success(s) => Ok(&s.result),
        SessionRpcOutcome::Failure(f) => Err(DomainError::new(
            f.code,
            f.message.clone(),
            f.mutation,
        )),
    }
}

/// The worker id and incarnation a terminal outcome came from, and the scope it echoes.
pub fn outcome_identity(
    outcome: &SessionRpcOutcome,
) -> (&Id, &Id, &Option<Scope>) {
    match outcome {
        SessionRpcOutcome::Success(s) => (&s.worker_id, &s.incarnation_id, &s.scope),
        SessionRpcOutcome::Failure(f) => (&f.worker_id, &f.incarnation_id, &f.scope),
    }
}

/// One session phase transition, recorded whether or not it ends a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseTransition {
    pub from: String,
    pub to: String,
}

/// Everything a run recorded: its phase transitions and its completed transitions.
///
/// The transitions are the contract's [`TransitionTrace`]; the phase path is this crate's own,
/// because the state machine lives here and not in the type contract.
#[derive(Clone, Debug, Default)]
pub struct TraceLog {
    pub phases: Vec<PhaseTransition>,
    pub transitions: Vec<TransitionTrace>,
}

impl TraceLog {
    pub fn phase(&mut self, from: String, to: String) {
        self.phases.push(PhaseTransition { from, to });
    }

    pub fn transition(&mut self, trace: TransitionTrace) {
        self.transitions.push(trace);
    }

    /// The behaviour of every transition, in order, with operational metadata excluded.
    ///
    /// This is what `step-v1` section 8 compares across dispatch and completion orders.
    pub fn behavior(&self) -> Vec<String> {
        self.transitions
            .iter()
            .map(|t| {
                canonicalize(&t.behaviour.to_json())
                    .expect("a recorded behaviour canonicalizes")
            })
            .collect()
    }

    /// The phase path, as `from -> to` strings.
    pub fn phase_path(&self) -> Vec<String> {
        self.phases.iter().map(|p| format!("{} -> {}", p.from, p.to)).collect()
    }
}

// -------------------------------------------------------------------------------------------
// Coordinator-local types
//
// `workers-v1` section 4 calls these library interfaces rather than payloads, so the contract
// crate does not carry them: they never cross the bus.

/// What an executor returns: buttons and axes, with no port assignment.
///
/// The coordinator supplies the port, which is why this is not a [`PortControl`].
#[derive(Clone, Debug, PartialEq)]
pub struct ControllerIntent {
    pub buttons: Vec<ButtonState>,
    pub axes: Vec<AxisValue>,
}

impl ControllerIntent {
    /// The canonical JSON of the intent: exactly a `PortControl` without its port.
    pub fn to_json(&self) -> Value {
        let buttons = self
            .buttons
            .iter()
            .map(|b| {
                let mut m = Map::new();
                m.insert("id".into(), b.id.clone().into());
                m.insert("down".into(), Value::Bool(b.down));
                Value::Object(m)
            })
            .collect();
        let axes = self
            .axes
            .iter()
            .map(|a| {
                let mut m = Map::new();
                m.insert("id".into(), a.id.clone().into());
                m.insert("value".into(), Value::from(a.value));
                Value::Object(m)
            })
            .collect();
        let mut m = Map::new();
        m.insert("buttons".into(), Value::Array(buttons));
        m.insert("axes".into(), Value::Array(axes));
        Value::Object(m)
    }

    /// Reads an intent out of a decision payload.
    pub fn from_json(value: &Value) -> Result<ControllerIntent, String> {
        let control = Value::Object({
            let mut m = object(value.clone());
            m.insert("portId".into(), "p0".into());
            m
        });
        let control = PortControl::from_json(&control).map_err(|e| e.0)?;
        Ok(ControllerIntent { buttons: control.buttons, axes: control.axes })
    }

    /// Binds this intent to a port, which only the coordinator may do.
    pub fn at_port(self, port_id: &str) -> PortControl {
        PortControl {
            port_id: port_id.to_owned(),
            buttons: self.buttons,
            axes: self.axes,
        }
    }
}

/// One port-to-agent assignment. It crosses the bus as a pair inside
/// [`EnvironmentInitializeParams`]; inside the session it is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortBinding {
    pub port_id: Id,
    pub agent_id: Id,
}

impl PortBinding {
    pub fn pair(&self) -> (Id, Id) {
        (self.port_id.clone(), self.agent_id.clone())
    }
}

/// Everything the task routes to one agent for this transition. Always explicit, even empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentOutcome {
    pub rewards: Vec<Reward>,
    pub stimulations: Vec<Stimulus>,
}

/// Every button up and every axis at its declared neutral.
///
/// Uncontrolled ports are configured neutral before the epoch, not supplied ad hoc.
pub fn neutral_control(schema: &ControllerSchema, port_id: &str) -> PortControl {
    PortControl {
        port_id: port_id.to_owned(),
        buttons: schema
            .buttons
            .iter()
            .map(|id| ButtonState { id: id.clone(), down: false })
            .collect(),
        axes: schema
            .axes
            .iter()
            .map(|a| AxisValue { id: a.id.clone(), value: a.neutral })
            .collect(),
    }
}

// -------------------------------------------------------------------------------------------
// Session-side conveniences over the contract types
//
// These are extension traits rather than forks: the data and the rules stay in the contract
// crate, and these only spell out the readings a coordinator and its workers keep needing.

/// Reading a typed value the way the synthetic composition writes it.
pub trait TypedValueExt {
    /// The canonical digest of the whole typed value, schema identity included.
    fn digest(&self) -> Digest;
    fn number(&self, key: &str) -> Result<f64, String>;
    fn integer(&self, key: &str) -> Result<i64, String>;
}

impl TypedValueExt for TypedValue {
    fn digest(&self) -> Digest {
        typed_digest(self)
    }

    fn number(&self, key: &str) -> Result<f64, String> {
        typed_number(self, key)
    }

    fn integer(&self, key: &str) -> Result<i64, String> {
        typed_integer(self, key)
    }
}

/// Checking and building one port's complete controller state.
pub trait ControllerSchemaExt {
    /// Checks that `control` names every declared button and axis, in descriptor order, with
    /// every value inside its range.
    fn check(&self, control: &PortControl) -> Result<(), String>;
    /// Every button up and every axis at its declared neutral.
    fn neutral(&self, port_id: &str) -> PortControl;
}

impl ControllerSchemaExt for ControllerSchema {
    fn check(&self, control: &PortControl) -> Result<(), String> {
        control.validate_against(self).map_err(|e| e.0)
    }

    fn neutral(&self, port_id: &str) -> PortControl {
        neutral_control(self, port_id)
    }
}

/// The byte shape and producing boundary one declared view requires.
pub trait ViewDescriptorExt {
    fn byte_length(&self) -> u64;
    fn required_produced_step(&self, boundary: u64) -> u64;
}

impl ViewDescriptorExt for ViewDescriptor {
    fn byte_length(&self) -> u64 {
        view_byte_length(self)
    }

    fn required_produced_step(&self, boundary: u64) -> u64 {
        required_produced_step(self, boundary)
    }
}

/// `RationalNs` readings the session uses.
pub trait RationalExt {
    fn is_positive(&self) -> bool;
}

impl RationalExt for RationalNs {
    fn is_positive(&self) -> bool {
        !self.is_zero()
    }
}

/// The next step of a scope, which is the only arithmetic a coordinator does on one.
pub trait ScopeExt {
    fn next(&self) -> Scope;
}

impl ScopeExt for Scope {
    fn next(&self) -> Scope {
        scope_at(&self.session_id, &self.epoch, self.step + 1)
    }
}

/// The `Id` form of a domain request id, for a payload field that carries it as a string.
pub trait DomainRequestIdExt {
    fn id(&self) -> Id;
}

impl DomainRequestIdExt for DomainRequestId {
    fn id(&self) -> Id {
        self.as_str().to_owned()
    }
}

/// Carrying a terminal domain outcome in a bus `outcome` object.
pub trait SessionRpcOutcomeExt: Sized {
    fn to_outcome(&self) -> Map<String, Value>;
    fn from_outcome(outcome: &Map<String, Value>) -> Result<Self, String>;
    fn result(&self) -> DomainResult<&Value>;
}

impl SessionRpcOutcomeExt for SessionRpcOutcome {
    fn to_outcome(&self) -> Map<String, Value> {
        object(self.to_json())
    }

    fn from_outcome(outcome: &Map<String, Value>) -> Result<SessionRpcOutcome, String> {
        SessionRpcOutcome::from_json(&Value::Object(outcome.clone())).map_err(|e| e.0)
    }

    fn result(&self) -> DomainResult<&Value> {
        outcome_result(self)
    }
}

/// One domain failure, ready to send.
pub fn failure_outcome(
    request_id: &DomainRequestId,
    worker_id: &str,
    incarnation_id: &str,
    scope: Option<Scope>,
    error: DomainError,
) -> SessionRpcOutcome {
    SessionRpcOutcome::Failure(SessionRpcFailure {
        request_id: request_id.clone(),
        worker_id: worker_id.to_owned(),
        incarnation_id: incarnation_id.to_owned(),
        scope,
        code: error.code,
        message: error.message,
        mutation: error.mutation,
    })
}

/// One domain success, ready to send.
pub fn success_outcome(
    request_id: &DomainRequestId,
    worker_id: &str,
    incarnation_id: &str,
    scope: Option<Scope>,
    result: Map<String, Value>,
) -> SessionRpcOutcome {
    SessionRpcOutcome::Success(SessionRpcSuccess {
        request_id: request_id.clone(),
        worker_id: worker_id.to_owned(),
        incarnation_id: incarnation_id.to_owned(),
        scope,
        result: Value::Object(result),
    })
}
