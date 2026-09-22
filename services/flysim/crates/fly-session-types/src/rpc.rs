//! The domain request/reply envelope of ipc-v1 section 3 and the error codes of section 7.
//!
//! A domain reply is the `outcome` object inside a bus `rpc.result`. Bus route or admission
//! failure is not one of these: it never reaches a handler, so it cannot carry a mutation
//! certainty.

use flybus::wire::Fields;
use serde_json::Value;

use crate::canonical;
use crate::scalar::{
    DomainRequestId, DomainType, Result, Scope, bounded_string, constant, enumeration, err, is_id,
    obj,
};
use crate::workers::MAX_MESSAGE_CODE_POINTS;

/// The domain error codes of ipc-v1 section 7.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// Invalid schema/range, before mutation.
    InvalidArgument,
    /// Missing method or capability.
    Unsupported,
    /// Wrong session/profile/port/build/asset identity.
    IdentityMismatch,
    StaleEpoch,
    StaleStep,
    FutureStep,
    /// Wrong worker phase.
    InvalidPhase,
    /// Existing logical operation with a changed id or body.
    Conflict,
    /// The original operation is still executing; this duplicate bus call started no work.
    InProgress,
    /// Domain capacity unavailable before admission.
    Busy,
    /// Missing, unowned or mismatched artifact, or an invalid media shape.
    BufferInvalid,
    /// Safe replay is no longer available; never recompute to replace it.
    ResultExpired,
    /// Restore validation failed before activation.
    IncompatibleState,
    BackendFailure,
    Internal,
}

impl ErrorCode {
    pub const ALL: &'static [&'static str] = &[
        "INVALID_ARGUMENT",
        "UNSUPPORTED",
        "IDENTITY_MISMATCH",
        "STALE_EPOCH",
        "STALE_STEP",
        "FUTURE_STEP",
        "INVALID_PHASE",
        "CONFLICT",
        "IN_PROGRESS",
        "BUSY",
        "BUFFER_INVALID",
        "RESULT_EXPIRED",
        "INCOMPATIBLE_STATE",
        "BACKEND_FAILURE",
        "INTERNAL",
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidArgument => "INVALID_ARGUMENT",
            ErrorCode::Unsupported => "UNSUPPORTED",
            ErrorCode::IdentityMismatch => "IDENTITY_MISMATCH",
            ErrorCode::StaleEpoch => "STALE_EPOCH",
            ErrorCode::StaleStep => "STALE_STEP",
            ErrorCode::FutureStep => "FUTURE_STEP",
            ErrorCode::InvalidPhase => "INVALID_PHASE",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::InProgress => "IN_PROGRESS",
            ErrorCode::Busy => "BUSY",
            ErrorCode::BufferInvalid => "BUFFER_INVALID",
            ErrorCode::ResultExpired => "RESULT_EXPIRED",
            ErrorCode::IncompatibleState => "INCOMPATIBLE_STATE",
            ErrorCode::BackendFailure => "BACKEND_FAILURE",
            ErrorCode::Internal => "INTERNAL",
        }
    }

    pub fn parse(s: &str) -> Result<ErrorCode> {
        Ok(match s {
            "INVALID_ARGUMENT" => ErrorCode::InvalidArgument,
            "UNSUPPORTED" => ErrorCode::Unsupported,
            "IDENTITY_MISMATCH" => ErrorCode::IdentityMismatch,
            "STALE_EPOCH" => ErrorCode::StaleEpoch,
            "STALE_STEP" => ErrorCode::StaleStep,
            "FUTURE_STEP" => ErrorCode::FutureStep,
            "INVALID_PHASE" => ErrorCode::InvalidPhase,
            "CONFLICT" => ErrorCode::Conflict,
            "IN_PROGRESS" => ErrorCode::InProgress,
            "BUSY" => ErrorCode::Busy,
            "BUFFER_INVALID" => ErrorCode::BufferInvalid,
            "RESULT_EXPIRED" => ErrorCode::ResultExpired,
            "INCOMPATIBLE_STATE" => ErrorCode::IncompatibleState,
            "BACKEND_FAILURE" => ErrorCode::BackendFailure,
            "INTERNAL" => ErrorCode::Internal,
            _ => return err("code is not one of the fifteen domain error codes"),
        })
    }

    /// The codes that are raised strictly before any mutation, so their certainty is `none`.
    pub fn is_before_mutation(self) -> bool {
        matches!(
            self,
            ErrorCode::InvalidArgument
                | ErrorCode::Unsupported
                | ErrorCode::IdentityMismatch
                | ErrorCode::StaleEpoch
                | ErrorCode::StaleStep
                | ErrorCode::FutureStep
                | ErrorCode::InvalidPhase
                | ErrorCode::Conflict
                | ErrorCode::InProgress
                | ErrorCode::Busy
                | ErrorCode::BufferInvalid
                | ErrorCode::IncompatibleState
        )
    }
}

/// How certain the responder is that the operation mutated state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MutationCertainty {
    /// Nothing was applied.
    None,
    /// The mutation completed.
    Applied,
    /// Completion is not established. "Errors after partial mutation use unknown unless
    /// completion is established." (ipc-v1 section 7)
    Unknown,
}

impl MutationCertainty {
    pub const ALL: &'static [&'static str] = &["none", "applied", "unknown"];

    pub fn as_str(self) -> &'static str {
        match self {
            MutationCertainty::None => "none",
            MutationCertainty::Applied => "applied",
            MutationCertainty::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Result<MutationCertainty> {
        match s {
            "none" => Ok(MutationCertainty::None),
            "applied" => Ok(MutationCertainty::Applied),
            "unknown" => Ok(MutationCertainty::Unknown),
            _ => err("mutation must be none, applied or unknown"),
        }
    }
}

/// `SessionRpcRequest`: one domain operation, independent of the bus callId that carries it.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRpcRequest {
    pub request_id: DomainRequestId,
    pub scope: Option<Scope>,
    pub params: Value,
}

impl SessionRpcRequest {
    /// The canonical body digest of this request under `method` (ipc-v1 section 5).
    pub fn body_digest(&self, method: &str) -> Result<String> {
        canonical::body_digest(method, self.scope.as_ref(), &self.params)
    }

    /// The operation key of a step mutation issued by `worker_id` under `method`. Lifecycle
    /// calls with a null scope have no step operation key.
    pub fn operation_key(&self, method: &str, worker_id: &str) -> Result<canonical::OperationKey> {
        let scope = self
            .scope
            .clone()
            .ok_or_else(|| crate::scalar::wire_err("operation key: a step mutation has a scope"))?;
        canonical::OperationKey::new(scope, method, worker_id)
    }
}

impl DomainType for SessionRpcRequest {
    const TYPE_NAME: &'static str = "SessionRpcRequest";

    fn from_json(value: &Value) -> Result<SessionRpcRequest> {
        let mut f = Fields::new(value, "SessionRpcRequest")?;
        let request_id = DomainRequestId::read(&mut f, "requestId")?;
        let scope = Scope::nullable_from_json(f.value("scope")?)?;
        let params = f.object("params")?.clone();
        f.finish()?;
        let r = SessionRpcRequest {
            request_id,
            scope,
            params: Value::Object(params),
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("requestId", self.request_id.to_json()),
            ("scope", Scope::nullable_to_json(self.scope.as_ref())),
            ("params", self.params.clone()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !self.params.is_object() {
            return err("SessionRpcRequest: params must be an object");
        }
        if let Some(scope) = &self.scope {
            scope.validate()?;
        }
        canonical::reject_bus_identities(&self.params)
    }
}

/// `SessionRpcSuccess`: a terminal domain success, echoing the request scope.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRpcSuccess {
    pub request_id: DomainRequestId,
    pub worker_id: String,
    pub incarnation_id: String,
    pub scope: Option<Scope>,
    pub result: Value,
}

impl DomainType for SessionRpcSuccess {
    const TYPE_NAME: &'static str = "SessionRpcSuccess";

    fn from_json(value: &Value) -> Result<SessionRpcSuccess> {
        let mut f = Fields::new(value, "SessionRpcSuccess")?;
        constant(&mut f, "type", "result")?;
        let request_id = DomainRequestId::read(&mut f, "requestId")?;
        let worker_id = f.id("workerId")?;
        let incarnation_id = f.id("incarnationId")?;
        let scope = Scope::nullable_from_json(f.value("scope")?)?;
        let result = f.object("result")?.clone();
        f.finish()?;
        let s = SessionRpcSuccess {
            request_id,
            worker_id,
            incarnation_id,
            scope,
            result: Value::Object(result),
        };
        s.validate()?;
        Ok(s)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("type", "result".into()),
            ("requestId", self.request_id.to_json()),
            ("workerId", self.worker_id.clone().into()),
            ("incarnationId", self.incarnation_id.clone().into()),
            ("scope", Scope::nullable_to_json(self.scope.as_ref())),
            ("result", self.result.clone()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.worker_id) || !is_id(&self.incarnation_id) {
            return err("SessionRpcSuccess: workerId and incarnationId must be valid ids");
        }
        if !self.result.is_object() {
            return err("SessionRpcSuccess: result must be an object");
        }
        if let Some(scope) = &self.scope {
            scope.validate()?;
        }
        Ok(())
    }
}

/// `SessionRpcFailure`: a terminal domain error with an explicit mutation certainty.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRpcFailure {
    pub request_id: DomainRequestId,
    pub worker_id: String,
    pub incarnation_id: String,
    pub scope: Option<Scope>,
    pub code: ErrorCode,
    pub message: String,
    pub mutation: MutationCertainty,
}

impl DomainType for SessionRpcFailure {
    const TYPE_NAME: &'static str = "SessionRpcFailure";

    fn from_json(value: &Value) -> Result<SessionRpcFailure> {
        let mut f = Fields::new(value, "SessionRpcFailure")?;
        constant(&mut f, "type", "error")?;
        let request_id = DomainRequestId::read(&mut f, "requestId")?;
        let worker_id = f.id("workerId")?;
        let incarnation_id = f.id("incarnationId")?;
        let scope = Scope::nullable_from_json(f.value("scope")?)?;
        let (code, message, mutation) = {
            let v = f.value("error")?;
            let mut e = Fields::new(v, "SessionRpcFailure.error")?;
            let code = ErrorCode::parse(&enumeration(&mut e, "code", ErrorCode::ALL)?)?;
            let message = bounded_string(&mut e, "message", MAX_MESSAGE_CODE_POINTS)?;
            let mutation = MutationCertainty::parse(&enumeration(
                &mut e,
                "mutation",
                MutationCertainty::ALL,
            )?)?;
            e.finish()?;
            (code, message, mutation)
        };
        f.finish()?;
        let failure = SessionRpcFailure {
            request_id,
            worker_id,
            incarnation_id,
            scope,
            code,
            message,
            mutation,
        };
        failure.validate()?;
        Ok(failure)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("type", "error".into()),
            ("requestId", self.request_id.to_json()),
            ("workerId", self.worker_id.clone().into()),
            ("incarnationId", self.incarnation_id.clone().into()),
            ("scope", Scope::nullable_to_json(self.scope.as_ref())),
            (
                "error",
                obj(vec![
                    ("code", self.code.as_str().into()),
                    ("message", self.message.clone().into()),
                    ("mutation", self.mutation.as_str().into()),
                ]),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.worker_id) || !is_id(&self.incarnation_id) {
            return err("SessionRpcFailure: workerId and incarnationId must be valid ids");
        }
        if self.message.chars().count() > MAX_MESSAGE_CODE_POINTS {
            return err("SessionRpcFailure: message is at most 512 code points");
        }
        if self.code.is_before_mutation() && self.mutation != MutationCertainty::None {
            return err(format!(
                "SessionRpcFailure: {} is raised before mutation, so mutation is \"none\"",
                self.code.as_str()
            ));
        }
        if let Some(scope) = &self.scope {
            scope.validate()?;
        }
        Ok(())
    }
}

/// A terminal domain outcome: success or failure.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionRpcOutcome {
    Success(SessionRpcSuccess),
    Failure(SessionRpcFailure),
}

impl SessionRpcOutcome {
    pub fn request_id(&self) -> &DomainRequestId {
        match self {
            SessionRpcOutcome::Success(s) => &s.request_id,
            SessionRpcOutcome::Failure(f) => &f.request_id,
        }
    }

    /// Replies echo the original scope (ipc-v1 section 3).
    pub fn echoes(&self, request: &SessionRpcRequest) -> bool {
        let scope = match self {
            SessionRpcOutcome::Success(s) => &s.scope,
            SessionRpcOutcome::Failure(f) => &f.scope,
        };
        self.request_id() == &request.request_id && scope == &request.scope
    }
}

impl DomainType for SessionRpcOutcome {
    const TYPE_NAME: &'static str = "SessionRpcOutcome";

    fn from_json(value: &Value) -> Result<SessionRpcOutcome> {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| crate::scalar::wire_err("SessionRpcOutcome: missing type"))?;
        match kind {
            "result" => SessionRpcSuccess::from_json(value).map(SessionRpcOutcome::Success),
            "error" => SessionRpcFailure::from_json(value).map(SessionRpcOutcome::Failure),
            _ => err("SessionRpcOutcome: type must be \"result\" or \"error\""),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            SessionRpcOutcome::Success(s) => s.to_json(),
            SessionRpcOutcome::Failure(f) => f.to_json(),
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            SessionRpcOutcome::Success(s) => s.validate(),
            SessionRpcOutcome::Failure(f) => f.validate(),
        }
    }
}
