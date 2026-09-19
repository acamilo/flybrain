//! Transport error codes and the error value every fallible call returns.

use std::fmt;

/// Transport error codes (bus-v1 section 9, plus the three this crate adds; see the README).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    InvalidEnvelope,
    VersionMismatch,
    NotAuthorized,
    NoService,
    TargetChanged,
    Backpressure,
    CallGone,
    ArtifactUnsealed,
    ArtifactGone,
    OwnerInvalid,
    QuotaExceeded,
    StoreFailure,
    RouterLost,
    /// Addition: a name is already registered, a redeclaration disagrees, or a topic still has
    /// subscribers.
    Conflict,
    /// Addition: publish or subscribe named a topic nobody declared.
    NoTopic,
    /// Addition: a seal found the wrong length or digest, or a reference disagrees with the
    /// artifact it names.
    ArtifactMismatch,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 16] = [
        ErrorCode::InvalidEnvelope,
        ErrorCode::VersionMismatch,
        ErrorCode::NotAuthorized,
        ErrorCode::NoService,
        ErrorCode::TargetChanged,
        ErrorCode::Backpressure,
        ErrorCode::CallGone,
        ErrorCode::ArtifactUnsealed,
        ErrorCode::ArtifactGone,
        ErrorCode::OwnerInvalid,
        ErrorCode::QuotaExceeded,
        ErrorCode::StoreFailure,
        ErrorCode::RouterLost,
        ErrorCode::Conflict,
        ErrorCode::NoTopic,
        ErrorCode::ArtifactMismatch,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidEnvelope => "INVALID_ENVELOPE",
            ErrorCode::VersionMismatch => "VERSION_MISMATCH",
            ErrorCode::NotAuthorized => "NOT_AUTHORIZED",
            ErrorCode::NoService => "NO_SERVICE",
            ErrorCode::TargetChanged => "TARGET_CHANGED",
            ErrorCode::Backpressure => "BACKPRESSURE",
            ErrorCode::CallGone => "CALL_GONE",
            ErrorCode::ArtifactUnsealed => "ARTIFACT_UNSEALED",
            ErrorCode::ArtifactGone => "ARTIFACT_GONE",
            ErrorCode::OwnerInvalid => "OWNER_INVALID",
            ErrorCode::QuotaExceeded => "QUOTA_EXCEEDED",
            ErrorCode::StoreFailure => "STORE_FAILURE",
            ErrorCode::RouterLost => "ROUTER_LOST",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::NoTopic => "NO_TOPIC",
            ErrorCode::ArtifactMismatch => "ARTIFACT_MISMATCH",
        }
    }

    pub fn parse(s: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether the operation may have reached its target (bus-v1 section 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dispatch {
    NotDispatched,
    Dispatched,
    Unknown,
}

impl Dispatch {
    pub fn as_str(self) -> &'static str {
        match self {
            Dispatch::NotDispatched => "not-dispatched",
            Dispatch::Dispatched => "dispatched",
            Dispatch::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Dispatch> {
        match s {
            "not-dispatched" => Some(Dispatch::NotDispatched),
            "dispatched" => Some(Dispatch::Dispatched),
            "unknown" => Some(Dispatch::Unknown),
            _ => None,
        }
    }
}

/// A transport error: code, a short message and the dispatch certainty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusError {
    pub code: ErrorCode,
    pub message: String,
    pub dispatch: Dispatch,
}

impl BusError {
    /// An error raised before anything could have been dispatched.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> BusError {
        BusError {
            code,
            message: message.into(),
            dispatch: Dispatch::NotDispatched,
        }
    }

    pub fn with_dispatch(mut self, dispatch: Dispatch) -> BusError {
        self.dispatch = dispatch;
        self
    }

    pub(crate) fn invalid(message: impl Into<String>) -> BusError {
        BusError::new(ErrorCode::InvalidEnvelope, message)
    }

    pub(crate) fn lost(message: impl Into<String>) -> BusError {
        BusError::new(ErrorCode::RouterLost, message).with_dispatch(Dispatch::Unknown)
    }
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.code,
            self.dispatch.as_str(),
            self.message
        )
    }
}

impl std::error::Error for BusError {}
