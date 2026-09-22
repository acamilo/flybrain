//! Domain RPC over Flybus: `req-<u64>` serials, incarnation pinning and the retry rule.
//!
//! A domain retry keeps its `requestId` and body and takes a fresh bus `callId`. Nothing here
//! retries on its own: an uncertain call is resolved by the caller, which is the coordinator.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;

/// A named worker endpoint, pinned to one bus registration and one domain incarnation.
#[derive(Clone, Debug)]
pub struct WorkerRef {
    pub service: String,
    /// The bus `serviceIncarnation` every call pins. Not a worker process id.
    pub bus_incarnation: String,
    pub worker_id: Id,
    /// The domain `incarnationId` Hello negotiated, once it has.
    pub domain_incarnation: Option<Id>,
}

impl WorkerRef {
    pub fn new(service: &str, bus_incarnation: &str, worker_id: &Id) -> WorkerRef {
        WorkerRef {
            service: service.to_owned(),
            bus_incarnation: bus_incarnation.to_owned(),
            worker_id: worker_id.clone(),
            domain_incarnation: None,
        }
    }
}

/// One terminal domain reply and the artifacts it brought.
pub struct DomainReply {
    pub outcome: SessionRpcOutcome,
    pub request_id: DomainRequestId,
    pub artifacts: BTreeMap<String, flybus::Artifact>,
}

impl DomainReply {
    /// The success `result`, or the domain error.
    pub fn result(&self) -> Result<&Value, DomainError> {
        outcome_result(&self.outcome)
    }

    /// Reads and validates the success `result` as a method payload.
    pub fn parse<T: DomainType>(&self) -> Result<T, DomainError> {
        let result = outcome_result(&self.outcome)?;
        T::from_json(result).map_err(|e| DomainError::invalid(format!("unreadable result: {e}")))
    }
}

/// Issues one domain call and waits for its terminal reply.
///
/// `want_artifacts` names the attachments to extract before the result delivery is dropped.
/// A bus-level failure is not a domain failure: it is reported with the dispatch certainty the
/// bus gave, because a caller-side timeout must not imply that nothing was mutated.
#[allow(clippy::too_many_arguments)]
pub async fn call(
    bus: &flybus::Client,
    target: &WorkerRef,
    method: &str,
    scope: Option<Scope>,
    params: Map<String, Value>,
    attachments: &[(&str, &flybus::Artifact)],
    request_id: DomainRequestId,
    want_artifacts: &[String],
) -> Result<DomainReply, DomainError> {
    let request = SessionRpcRequest { request_id: request_id.clone(), scope, params: Value::Object(params) };
    let mut pending = bus
        .call(
            &target.service,
            Some(&target.bus_incarnation),
            method,
            object(request.to_json()),
            attachments,
        )
        .await
        .map_err(|e| bus_error(method, &e))?;
    let result = pending.result().await.map_err(|e| bus_error(method, &e))?;
    let outcome = SessionRpcOutcome::from_json(&Value::Object(result.outcome().clone()))
        .map_err(|e| DomainError::invalid(format!("{method}: {e}")))?;
    let mut artifacts = BTreeMap::new();
    for name in want_artifacts {
        if let Ok(artifact) = result.artifact(name) {
            // An independent explicit hold, so the handle outlives this delivery and can be
            // forwarded to several Commit calls and to publication.
            match artifact.retain().await {
                Ok(hold) => {
                    artifacts.insert(name.clone(), hold);
                }
                Err(_) => {
                    artifacts.insert(name.clone(), artifact);
                }
            }
        }
    }
    drop(result);
    Ok(DomainReply { outcome, request_id, artifacts })
}

/// Maps a bus failure onto a domain error, preserving how certain the mutation is.
fn bus_error(method: &str, e: &flybus::BusError) -> DomainError {
    let mutation = match e.dispatch {
        flybus::Dispatch::NotDispatched => MutationCertainty::None,
        flybus::Dispatch::Dispatched | flybus::Dispatch::Unknown => MutationCertainty::Unknown,
    };
    let code = match e.code {
        flybus::ErrorCode::TargetChanged | flybus::ErrorCode::NoService => {
            ErrorCode::IdentityMismatch
        }
        flybus::ErrorCode::Backpressure | flybus::ErrorCode::QuotaExceeded => ErrorCode::Busy,
        flybus::ErrorCode::ArtifactGone
        | flybus::ErrorCode::ArtifactUnsealed
        | flybus::ErrorCode::OwnerInvalid
        | flybus::ErrorCode::ArtifactMismatch => ErrorCode::BufferInvalid,
        _ => ErrorCode::BackendFailure,
    };
    DomainError::new(code, format!("{method}: bus {:?}: {}", e.code, e.message), mutation)
}

/// Per-worker request serials. A newly issued operation takes the next one; a retry does not.
#[derive(Clone, Debug, Default)]
pub struct Serials(BTreeMap<String, u64>);

impl Serials {
    pub fn next(&mut self, service: &str) -> DomainRequestId {
        let slot = self.0.entry(service.to_owned()).or_insert(0);
        *slot += 1;
        DomainRequestId::from_serial(*slot)
    }

    pub fn highest(&self, service: &str) -> u64 {
        self.0.get(service).copied().unwrap_or_default()
    }
}
