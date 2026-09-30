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
    /// The worker's shell in this process, when the launcher offers it (PERF-01): endpoint
    /// methods then go over the local lane instead of the bus. Same shell, same admission,
    /// same outcomes; no router in between.
    pub local: Option<crate::worker::LocalLane>,
}

impl WorkerRef {
    pub fn new(service: &str, bus_incarnation: &str, worker_id: &Id) -> WorkerRef {
        WorkerRef {
            service: service.to_owned(),
            bus_incarnation: bus_incarnation.to_owned(),
            worker_id: worker_id.clone(),
            domain_incarnation: None,
            local: None,
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
    send(bus, target, method, scope, params, attachments, request_id, want_artifacts)
        .await?
        .finish()
        .await
}

/// A domain call that has been sent and not yet answered: the bus has it in order, so a call
/// sent after it to the same worker is dispatched after it.
pub struct SentCall {
    method: String,
    pending: Pending,
    request_id: DomainRequestId,
    want_artifacts: Vec<String>,
}

enum Pending {
    Bus(flybus::PendingCall),
    Local(crate::worker::LocalPending),
}

/// Sends one domain call without waiting for its reply ([`SentCall::finish`] waits).
#[allow(clippy::too_many_arguments)]
pub async fn send(
    bus: &flybus::Client,
    target: &WorkerRef,
    method: &str,
    scope: Option<Scope>,
    params: Map<String, Value>,
    attachments: &[(&str, &flybus::Artifact)],
    request_id: DomainRequestId,
    want_artifacts: &[String],
) -> Result<SentCall, DomainError> {
    let request = SessionRpcRequest { request_id: request_id.clone(), scope, params: Value::Object(params) };
    if let Some(lane) = &target.local
        && crate::worker::local_lane_carries(method)
    {
        let attachments = attachments
            .iter()
            .map(|(name, artifact)| ((*name).to_owned(), (*artifact).clone()))
            .collect();
        let pending = lane.send(method, request, attachments).await?;
        return Ok(SentCall {
            method: method.to_owned(),
            pending: Pending::Local(pending),
            request_id,
            want_artifacts: want_artifacts.to_vec(),
        });
    }
    // An in-memory artifact is in no router's store: a bus call carries a sealed copy of it.
    let promoted = promote_all(bus, attachments).await?;
    let attachments: Vec<(&str, &flybus::Artifact)> =
        promoted.iter().map(|(n, a)| (n.as_str(), a)).collect();
    let attachments = attachments.as_slice();
    let pending = bus
        .call(
            &target.service,
            Some(&target.bus_incarnation),
            method,
            object(request.to_json()),
            attachments,
        )
        .await
        .map_err(|e| bus_error(method, &e))?;
    Ok(SentCall {
        method: method.to_owned(),
        pending: Pending::Bus(pending),
        request_id,
        want_artifacts: want_artifacts.to_vec(),
    })
}

impl SentCall {
    /// Waits for the terminal reply.
    pub async fn finish(self) -> Result<DomainReply, DomainError> {
        let method = self.method.as_str();
        let mut pending = match self.pending {
            Pending::Bus(pending) => pending,
            Pending::Local(pending) => {
                let reply = pending.finish().await.ok_or_else(|| {
                    DomainError::new(
                        ErrorCode::BackendFailure,
                        format!("{method}: the worker stopped before it answered"),
                        MutationCertainty::Unknown,
                    )
                })?;
                // The handles are the worker's own (in memory, or sealed holds its cache
                // keeps too); only the wanted ones are kept, as the bus path extracts.
                // A sealed handle is only valid as an attachment on the worker's own
                // connection, so a sealed reply artifact (a handler that sealed through the
                // bus) reaches the caller in memory, under the same reference.
                let mut artifacts = BTreeMap::new();
                for (name, artifact) in reply.artifacts {
                    if !self.want_artifacts.contains(&name) {
                        continue;
                    }
                    let artifact = artifact.to_memory().await.map_err(|e| bus_error(method, &e))?;
                    artifacts.insert(name, artifact);
                }
                return Ok(DomainReply {
                    outcome: reply.outcome,
                    request_id: self.request_id,
                    artifacts,
                });
            }
        };
        let result = pending.result().await.map_err(|e| bus_error(method, &e))?;
        let outcome = SessionRpcOutcome::from_json(&Value::Object(result.outcome().clone()))
            .map_err(|e| DomainError::invalid(format!("{method}: {e}")))?;
        // An independent explicit hold on each wanted attachment, so the handle outlives this
        // delivery and can be forwarded to several Commit calls and to publication. The holds
        // are independent router round trips, so they are taken concurrently.
        let wanted: Vec<(String, flybus::Artifact)> = self
            .want_artifacts
            .iter()
            .filter_map(|name| result.artifact(name).ok().map(|a| (name.clone(), a)))
            .collect();
        let retains: Vec<_> = wanted
            .into_iter()
            .map(|(name, artifact)| {
                tokio::spawn(async move {
                    match artifact.retain().await {
                        Ok(hold) => (name, hold),
                        Err(_) => (name, artifact),
                    }
                })
            })
            .collect();
        let mut artifacts = BTreeMap::new();
        for retain in retains {
            if let Ok((name, artifact)) = retain.await {
                artifacts.insert(name, artifact);
            }
        }
        drop(result);
        Ok(DomainReply { outcome, request_id: self.request_id, artifacts })
    }
}

/// Every attachment as a bus artifact: sealed handles as they are, in-memory ones as a sealed
/// copy in the caller's store.
pub async fn promote_all(
    bus: &flybus::Client,
    attachments: &[(&str, &flybus::Artifact)],
) -> Result<Vec<(String, flybus::Artifact)>, DomainError> {
    let mut out = Vec::with_capacity(attachments.len());
    for (name, artifact) in attachments {
        out.push(((*name).to_owned(), promote(bus, artifact).await?));
    }
    Ok(out)
}

/// One artifact as a bus artifact ([`promote_all`]).
pub async fn promote(
    bus: &flybus::Client,
    artifact: &flybus::Artifact,
) -> Result<flybus::Artifact, DomainError> {
    match artifact.memory() {
        None => Ok(artifact.clone()),
        Some(bytes) => {
            crate::media::seal_copy(bus, artifact.reference().content_type.clone(), bytes).await
        }
    }
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
