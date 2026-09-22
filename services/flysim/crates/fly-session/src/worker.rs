//! The worker dispatch shell: one Flybus service, the common `Worker.*` methods, and the
//! domain deduplication of `ipc-v1` section 5 in front of every mutation.
//!
//! The shell owns request admission order and the result cache. An endpoint owns the
//! mutation. Exactly one mutation runs at a time -- the endpoint sits behind its own mutex --
//! while `Worker.Status` is answered from a small shared cell, so a status query never waits
//! for a numerical operation and never advances the progress counter itself.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use crate::dedup::{Admission, CachedReply, OpClass, OperationKey, ResultCache};
// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;

/// `Worker.Acknowledge` accepts 1..=16 request ids.
pub const MAX_ACKNOWLEDGE_IDS: usize = 16;

/// The build identity a worker reports in Hello. It is not a profile digest.
pub fn build_digest() -> Digest {
    digest_of_bytes(b"fly-session/synthetic-workers-v1")
}

/// The status a worker reports, kept outside the endpoint mutex so `Worker.Status` stays
/// responsive while a mutation runs.
#[derive(Clone)]
pub struct StatusCell(Arc<Mutex<StatusInner>>);

struct StatusInner {
    state: WorkerState,
    current_scope: Option<Scope>,
    active_request_id: Option<DomainRequestId>,
    last_completed_request_id: Option<DomainRequestId>,
    last_batch_id: Option<Id>,
    progress_counter: u64,
}

impl Default for StatusCell {
    fn default() -> StatusCell {
        StatusCell::new()
    }
}

impl StatusCell {
    pub fn new() -> StatusCell {
        StatusCell(Arc::new(Mutex::new(StatusInner {
            state: WorkerState::Uninitialized,
            current_scope: None,
            active_request_id: None,
            last_completed_request_id: None,
            last_batch_id: None,
            progress_counter: 0,
        })))
    }

    fn with<T>(&self, f: impl FnOnce(&mut StatusInner) -> T) -> T {
        let mut inner = self.0.lock().expect("the status cell is never poisoned");
        f(&mut inner)
    }

    pub fn set_state(&self, state: WorkerState) {
        self.with(|s| s.state = state);
    }

    pub fn state(&self) -> WorkerState {
        self.with(|s| s.state)
    }

    pub fn set_scope(&self, scope: Option<Scope>) {
        self.with(|s| s.current_scope = scope);
    }

    pub fn set_active(&self, request_id: Option<DomainRequestId>) {
        self.with(|s| s.active_request_id = request_id);
    }

    pub fn set_completed(&self, request_id: DomainRequestId) {
        self.with(|s| {
            s.active_request_id = None;
            s.last_completed_request_id = Some(request_id);
        });
    }

    pub fn set_batch(&self, batch_id: Id) {
        self.with(|s| s.last_batch_id = Some(batch_id));
    }

    /// Records computational or phase progress. A status query never calls this.
    pub fn progress(&self, by: u64) {
        self.with(|s| s.progress_counter = s.progress_counter.saturating_add(by));
    }

    /// Raises the progress counter to `value`, which lets a worker report its model's own
    /// mutation count as its progress. It never moves backwards.
    pub fn advance_to(&self, value: u64) {
        self.with(|s| s.progress_counter = s.progress_counter.max(value));
    }

    pub fn progress_counter(&self) -> u64 {
        self.with(|s| s.progress_counter)
    }

    pub fn snapshot(&self) -> StatusResult {
        self.with(|s| StatusResult {
            state: s.state,
            current_scope: s.current_scope.clone(),
            active_request_id: s.active_request_id.clone(),
            last_completed_request_id: s.last_completed_request_id.clone(),
            last_batch_id: s.last_batch_id.clone(),
            progress_counter: s.progress_counter,
        })
    }
}

/// What a handler produced: a domain `result` object and the artifacts it attaches.
pub struct HandlerReply {
    pub result: Map<String, Value>,
    pub artifacts: Vec<(String, flybus::Artifact)>,
    /// True when a failure left the endpoint mutated; the shell reports it as such.
    pub mutated: bool,
}

impl HandlerReply {
    pub fn new(result: Map<String, Value>) -> HandlerReply {
        HandlerReply { result, artifacts: Vec::new(), mutated: true }
    }

    pub fn with_artifacts(
        result: Map<String, Value>,
        artifacts: Vec<(String, flybus::Artifact)>,
    ) -> HandlerReply {
        HandlerReply { result, artifacts, mutated: true }
    }

    /// The canonical JSON of one method result.
    pub fn from<T: DomainType>(value: &T) -> HandlerReply {
        HandlerReply::new(object(value.to_json()))
    }
}

/// Everything a handler is given: the parsed domain request and the bus request behind it.
pub struct HandlerCtx<'a> {
    pub method: &'a str,
    pub request: &'a SessionRpcRequest,
    pub client: &'a flybus::Client,
    pub incoming: &'a flybus::Request,
}

impl HandlerCtx<'_> {
    /// Reads and validates `params` as a method payload, reporting INVALID_ARGUMENT.
    ///
    /// The contract crate does the reading, so a misspelled required field fails here rather
    /// than silently defaulting.
    pub fn params<T: DomainType>(&self) -> DomainResult<T> {
        T::from_json(&self.request.params)
            .map_err(|e| DomainError::invalid(format!("{}: {e}", self.method)))
    }

    /// The scope the request must carry.
    pub fn scope(&self) -> DomainResult<&Scope> {
        self.request
            .scope
            .as_ref()
            .ok_or_else(|| DomainError::invalid(format!("{} requires a scope", self.method)))
    }

    /// An owned handle on one of the request's declared attachments.
    pub fn artifact(&self, name: &str) -> DomainResult<flybus::Artifact> {
        self.incoming.artifact(name).map_err(|e| {
            DomainError::before(
                ErrorCode::BufferInvalid,
                format!("attachment {name:?} is missing or unowned: {}", e.message),
            )
        })
    }
}

/// A handler's boxed future, so the endpoint trait stays dyn-compatible.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One worker's domain behaviour. The shell owns everything else.
pub trait WorkerEndpoint: Send + 'static {
    fn worker_id(&self) -> Id;
    fn incarnation_id(&self) -> Id;
    fn session_id(&self) -> Id;
    fn role(&self) -> Role;
    fn capabilities(&self) -> Vec<Id>;
    fn status_cell(&self) -> StatusCell;

    /// The domain methods this endpoint implements, beyond the common `Worker.*` set.
    /// Anything else returns UNSUPPORTED without entering the endpoint.
    fn methods(&self) -> Vec<&'static str>;

    fn handle<'a>(&'a mut self, ctx: HandlerCtx<'a>) -> BoxFuture<'a, DomainResult<HandlerReply>>;
}

/// A running worker: its bus client, its service and the task serving it.
pub struct WorkerHandle {
    pub worker_id: Id,
    pub incarnation_id: Id,
    pub service_name: String,
    pub service_incarnation: String,
    pub status: StatusCell,
    pub cache: Arc<tokio::sync::Mutex<ResultCache>>,
    task: tokio::task::JoinHandle<()>,
    client: flybus::Client,
}

impl WorkerHandle {
    /// The worker's own progress counter, which is the fake model's mutation count.
    pub fn progress_counter(&self) -> u64 {
        self.status.progress_counter()
    }

    pub fn state(&self) -> WorkerState {
        self.status.state()
    }

    /// Stops serving and closes the worker's bus connection, which drops its registration and
    /// every owner it held. A later reply from it can attach to nothing.
    pub async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
        self.client.close().await;
    }
}

/// Registers `service_name` and serves `endpoint` on it until the service ends or Shutdown.
pub fn serve<E: WorkerEndpoint>(
    client: flybus::Client,
    service: flybus::Service,
    endpoint: E,
) -> WorkerHandle {
    let worker_id = endpoint.worker_id();
    let incarnation_id = endpoint.incarnation_id();
    let status = endpoint.status_cell();
    let service_name = service.name().to_owned();
    let service_incarnation = service.incarnation().to_owned();
    let cache = Arc::new(tokio::sync::Mutex::new(ResultCache::new()));
    let task = tokio::spawn(run(client.clone(), service, Arc::new(tokio::sync::Mutex::new(endpoint)), cache.clone()));
    WorkerHandle {
        worker_id,
        incarnation_id,
        service_name,
        service_incarnation,
        status,
        cache,
        task,
        client,
    }
}

async fn run<E: WorkerEndpoint>(
    client: flybus::Client,
    mut service: flybus::Service,
    endpoint: Arc<tokio::sync::Mutex<E>>,
    cache: Arc<tokio::sync::Mutex<ResultCache>>,
) {
    // Identity and capabilities are fixed for the endpoint's lifetime, so the shell reads them
    // once and never takes the endpoint mutex to answer Hello or Status.
    let (worker_id, incarnation_id, session_id, role, capabilities, status, methods) = {
        let e = endpoint.lock().await;
        (
            e.worker_id(),
            e.incarnation_id(),
            e.session_id(),
            e.role(),
            e.capabilities(),
            e.status_cell(),
            e.methods(),
        )
    };
    let mut running: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    while let Some(incoming) = service.next().await {
        let method = incoming.method().to_owned();
        let responder = incoming.responder();
        let request = match SessionRpcRequest::from_json(&Value::Object(
            incoming.payload().clone(),
        )) {
            Ok(request) => request,
            Err(e) => {
                // A malformed envelope has no usable requestId, so the reply names req-0.
                let failure = failure(
                    &DomainRequestId::from_serial(0),
                    &worker_id,
                    &incarnation_id,
                    None,
                    DomainError::invalid(format!("{method}: {e}")),
                );
                let _ = responder.reply(failure.to_outcome(), &[]).await;
                continue;
            }
        };
        // The contract type already validated the `req-<U64>` form when it read the envelope.
        let serial = request.request_id.clone();

        // The common methods never enter the endpoint mutex, so they answer during a mutation.
        match method.as_str() {
            "Worker.Hello" => {
                let outcome = hello(
                    &request,
                    &session_id,
                    &worker_id,
                    &incarnation_id,
                    role,
                    &capabilities,
                );
                let _ = responder.reply(outcome.to_outcome(), &[]).await;
                continue;
            }
            "Worker.Status" => {
                let result = status.snapshot().to_json();
                let outcome = success(
                    &request,
                    &worker_id,
                    &incarnation_id,
                    object(result),
                );
                let _ = responder.reply(outcome.to_outcome(), &[]).await;
                continue;
            }
            "Worker.Acknowledge" => {
                let outcome = match acknowledge(&request, &cache).await {
                    Ok(result) => success(&request, &worker_id, &incarnation_id, result),
                    Err(e) => {
                        failure(&request.request_id, &worker_id, &incarnation_id, request.scope.clone(), e)
                    }
                };
                let _ = responder.reply(outcome.to_outcome(), &[]).await;
                continue;
            }
            "Worker.Shutdown" => {
                let outcome = match request.params.get("reason") {
                    Some(_) => {
                        match ShutdownParams::from_json(&request.params) {
                            Ok(_) => {
                                status.set_state(WorkerState::Stopping);
                                Ok(ShutdownResult)
                            }
                            Err(e) => Err(DomainError::invalid(format!("Worker.Shutdown: {e}"))),
                        }
                    }
                    None => Err(DomainError::invalid("Worker.Shutdown requires a reason")),
                };
                match outcome {
                    Ok(result) => {
                        let value = result.to_json();
                        let outcome =
                            success(&request, &worker_id, &incarnation_id, object(value));
                        let _ = responder.reply(outcome.to_outcome(), &[]).await;
                        return;
                    }
                    Err(e) => {
                        let outcome = failure(
                            &request.request_id,
                            &worker_id,
                            &incarnation_id,
                            request.scope.clone(),
                            e,
                        );
                        let _ = responder.reply(outcome.to_outcome(), &[]).await;
                        continue;
                    }
                }
            }
            _ => {}
        }

        let class = if methods.contains(&method.as_str()) {
            classify_default(&method)
        } else {
            None
        };
        let Some(class) = class else {
            let outcome = failure(
                &request.request_id,
                &worker_id,
                &incarnation_id,
                request.scope.clone(),
                DomainError::before(ErrorCode::Unsupported, format!("{method} is not supported")),
            );
            let _ = responder.reply(outcome.to_outcome(), &[]).await;
            continue;
        };

        let body = match request.body_digest(&method) {
            Ok(body) => body,
            Err(e) => {
                let outcome = failure(
                    &request.request_id,
                    &worker_id,
                    &incarnation_id,
                    request.scope.clone(),
                    DomainError::invalid(format!("{method}: {e}")),
                );
                let _ = responder.reply(outcome.to_outcome(), &[]).await;
                continue;
            }
        };
        let key = match (class, &request.scope) {
            (OpClass::StepMutation, Some(scope)) => OperationKey {
                session_id: scope.session_id.clone(),
                epoch: scope.epoch.clone(),
                step: scope.step,
                method: method.clone(),
                worker_id: worker_id.clone(),
            },
            (OpClass::StepMutation, None) => {
                let outcome = failure(
                    &request.request_id,
                    &worker_id,
                    &incarnation_id,
                    None,
                    DomainError::invalid(format!("{method} requires a scope")),
                );
                let _ = responder.reply(outcome.to_outcome(), &[]).await;
                continue;
            }
            _ => OperationKey {
                session_id: session_id.clone(),
                epoch: id("lifecycle"),
                step: 0,
                method: method.clone(),
                worker_id: worker_id.clone(),
            },
        };

        // Retained request identity is checked before phase checks and before any attachment
        // is dereferenced, because a duplicate may arrive after its input delivery was
        // consumed and needs only the cached result.
        let admission = {
            let mut c = cache.lock().await;
            c.admit(class, &key, serial.clone(), &body)
        };
        match admission {
            Admission::Replay(reply) => {
                let _ = responder.reply(reply.outcome.to_outcome(), &reply.attachments()).await;
                continue;
            }
            Admission::Refuse(e) => {
                let outcome = failure(
                    &request.request_id,
                    &worker_id,
                    &incarnation_id,
                    request.scope.clone(),
                    e,
                );
                let _ = responder.reply(outcome.to_outcome(), &[]).await;
                continue;
            }
            Admission::Execute => {}
        }

        status.set_active(Some(request.request_id.clone()));
        // The mutation runs in its own task so the shell keeps reading. That is what lets an
        // exact duplicate arriving mid-execution be refused with IN_PROGRESS while the
        // original bus call still completes normally. The endpoint mutex, not this loop,
        // enforces one mutation at a time.
        running.retain(|task| !task.is_finished());
        running.push(tokio::spawn(execute(
            client.clone(),
            endpoint.clone(),
            cache.clone(),
            status.clone(),
            worker_id.clone(),
            incarnation_id.clone(),
            class,
            key,
            serial,
            body,
            method,
            request,
            incoming,
        )));
    }
    for task in running {
        let _ = task.await;
    }
}

/// Runs one admitted mutation, records its reply and answers the bus call.
#[allow(clippy::too_many_arguments)]
async fn execute<E: WorkerEndpoint>(
    client: flybus::Client,
    endpoint: Arc<tokio::sync::Mutex<E>>,
    cache: Arc<tokio::sync::Mutex<ResultCache>>,
    status: StatusCell,
    worker_id: Id,
    incarnation_id: Id,
    class: OpClass,
    key: OperationKey,
    serial: DomainRequestId,
    body: Digest,
    method: String,
    request: SessionRpcRequest,
    incoming: flybus::Request,
) {
    let responder = incoming.responder();
    let outcome = {
        // One mutation at a time: the endpoint mutex is the worker's simulation lock, and it is
        // never held across a bus round trip taken by anything else.
        let mut e = endpoint.lock().await;
        let ctx = HandlerCtx {
            method: &method,
            request: &request,
            client: &client,
            incoming: &incoming,
        };
        e.handle(ctx).await
    };
    match outcome {
        Ok(reply) => {
            let outcome = success(&request, &worker_id, &incarnation_id, reply.result.clone());
            let mut holds = Vec::with_capacity(reply.artifacts.len());
            for (name, artifact) in &reply.artifacts {
                // The cache owns its own hold, so a replay survives the first caller
                // consuming its delivery.
                match artifact.retain().await {
                    Ok(hold) => holds.push((name.clone(), hold)),
                    Err(_) => holds.push((name.clone(), artifact.clone())),
                }
            }
            let cached = CachedReply::with_artifacts(outcome.clone(), holds);
            {
                let mut c = cache.lock().await;
                match class {
                    OpClass::StepMutation => c.record(key, serial, body, cached),
                    OpClass::Lifecycle => c.record_lifecycle(serial, body, cached),
                    OpClass::ReadOnly => c.record_readonly(serial, cached),
                }
            }
            status.set_completed(request.request_id.clone());
            let attachments: Vec<(&str, &flybus::Artifact)> =
                reply.artifacts.iter().map(|(n, a)| (n.as_str(), a)).collect();
            let _ = responder.reply(outcome.to_outcome(), &attachments).await;
        }
        Err(e) => {
            if e.mutation == MutationCertainty::None {
                // Nothing happened, so the key stays free for the corrected request.
                let mut c = cache.lock().await;
                c.abandon(&key);
            } else {
                let cached = CachedReply::new(failure_outcome(
                    &request.request_id,
                    &worker_id,
                    &incarnation_id,
                    request.scope.clone(),
                    e.clone(),
                ));
                let mut c = cache.lock().await;
                if class == OpClass::StepMutation {
                    c.record(key, serial, body, cached);
                } else {
                    c.abandon(&key);
                }
                status.set_state(WorkerState::Failed);
            }
            status.set_active(None);
            let outcome = failure(
                &request.request_id,
                &worker_id,
                &incarnation_id,
                request.scope.clone(),
                e,
            );
            let _ = responder.reply(outcome.to_outcome(), &[]).await;
        }
    }
}

/// The retention class of every method name the contracts define.
fn classify_default(method: &str) -> Option<OpClass> {
    match method {
        "Agent.Prepare" | "Agent.Commit" | "Environment.Advance" => Some(OpClass::StepMutation),
        "Agent.Initialize" | "Environment.Initialize" => Some(OpClass::Lifecycle),
        "Worker.Hello" | "Worker.Status" | "Worker.Acknowledge" | "Worker.Shutdown" => {
            Some(OpClass::ReadOnly)
        }
        _ => None,
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(m) => m,
        _ => unreachable!("a struct serializes to an object"),
    }
}

fn success(
    request: &SessionRpcRequest,
    worker_id: &Id,
    incarnation_id: &Id,
    result: Map<String, Value>,
) -> SessionRpcOutcome {
    success_outcome(
        &request.request_id,
        worker_id,
        incarnation_id,
        request.scope.clone(),
        result,
    )
}

fn failure(
    request_id: &DomainRequestId,
    worker_id: &Id,
    incarnation_id: &Id,
    scope: Option<Scope>,
    error: DomainError,
) -> SessionRpcOutcome {
    failure_outcome(request_id, worker_id, incarnation_id, scope, error)
}

fn hello(
    request: &SessionRpcRequest,
    session_id: &Id,
    worker_id: &Id,
    incarnation_id: &Id,
    role: Role,
    capabilities: &[Id],
) -> SessionRpcOutcome {
    let params: HelloParams = match HelloParams::from_json(&request.params) {
            Ok(params) => params,
            Err(e) => {
                return failure(
                    &request.request_id,
                    worker_id,
                    incarnation_id,
                    None,
                    DomainError::invalid(format!("Worker.Hello: {e}")),
                );
            }
        };
    if request.scope.is_some() {
        return failure(
            &request.request_id,
            worker_id,
            incarnation_id,
            None,
            DomainError::invalid("Worker.Hello has no scope"),
        );
    }
    if params.session_id != *session_id
        || params.expected_worker_id != *worker_id
        || params.role != role
    {
        return failure(
            &request.request_id,
            worker_id,
            incarnation_id,
            None,
            DomainError::before(
                ErrorCode::IdentityMismatch,
                "this worker is not the session, worker or role the caller expected",
            ),
        );
    }
    if !params.supported_majors.contains(&1) {
        return failure(
            &request.request_id,
            worker_id,
            incarnation_id,
            None,
            DomainError::before(ErrorCode::Unsupported, "no common major version"),
        );
    }
    let result = HelloResult {
        worker_id: worker_id.clone(),
        incarnation_id: incarnation_id.clone(),
        role,
        build_digest: build_digest(),
        contract_digest: contract_digest(),
        capabilities: capabilities.to_vec(),
        max_agents: MAX_AGENTS as u64,
        max_ports: MAX_PORTS as u64,
    };
    success(
        request,
        worker_id,
        incarnation_id,
        object(result.to_json()),
    )
}

async fn acknowledge(
    request: &SessionRpcRequest,
    cache: &Arc<tokio::sync::Mutex<ResultCache>>,
) -> DomainResult<Map<String, Value>> {
    let params: AcknowledgeParams = AcknowledgeParams::from_json(&request.params)
        .map_err(|e| DomainError::invalid(format!("Worker.Acknowledge: {e}")))?;
    if params.request_ids.is_empty() || params.request_ids.len() > MAX_ACKNOWLEDGE_IDS {
        return Err(DomainError::invalid("Worker.Acknowledge takes 1..=16 request ids"));
    }
    let acknowledged = {
        let mut c = cache.lock().await;
        c.acknowledge(&params.request_ids)
    };
    let result = AcknowledgeResult { acknowledged };
    Ok(object(result.to_json()))
}
