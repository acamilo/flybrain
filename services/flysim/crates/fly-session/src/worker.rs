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

/// The rest of a reply a handler finishes outside the endpoint's lock.
pub type DeferredReply =
    std::pin::Pin<Box<dyn std::future::Future<Output = DomainResult<HandlerReply>> + Send>>;

/// What a handler produced: a domain `result` object and the artifacts it attaches.
pub struct HandlerReply {
    pub result: Map<String, Value>,
    pub artifacts: Vec<(String, flybus::Artifact)>,
    /// True when a failure left the endpoint mutated; the shell reports it as such.
    pub mutated: bool,
    /// When set, the reply is this future's: the handler took what it needs from the endpoint
    /// (a state snapshot at the committed boundary) and the rest -- encoding, digesting and
    /// sealing a capture payload -- runs after the endpoint's lock is released, so the next
    /// mutation (the next `Agent.Prepare`) does not wait for it. The bus call is answered
    /// when it completes, and cached like any other reply.
    pub deferred: Option<DeferredReply>,
}

impl HandlerReply {
    pub fn new(result: Map<String, Value>) -> HandlerReply {
        HandlerReply { result, artifacts: Vec::new(), mutated: true, deferred: None }
    }

    pub fn with_artifacts(
        result: Map<String, Value>,
        artifacts: Vec<(String, flybus::Artifact)>,
    ) -> HandlerReply {
        HandlerReply { result, artifacts, mutated: true, deferred: None }
    }

    /// A reply finished by `rest` outside the endpoint's lock ([`HandlerReply::deferred`]).
    pub fn deferred(rest: DeferredReply) -> HandlerReply {
        HandlerReply { result: Map::new(), artifacts: Vec::new(), mutated: true, deferred: Some(rest) }
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
    pub incoming: &'a Incoming,
    /// Staging writers allocated ahead for the reply artifacts this worker seals every step.
    pub(crate) writers: &'a WriterPool,
}

/// Staging writers allocated ahead of need (BUS-01). A bus worker seals the same shapes of reply
/// artifact every step -- the environment a 92,160-byte frame, a 65,536-byte memory image and an
/// audio chunk of one of two lengths, the agent a 17,407-byte spike bitset -- and allocating one
/// is a router round trip on the step's critical path. The pool remembers the last few
/// `(content type, length)` shapes [`HandlerCtx::seal`] asked for and, after each reply, allocates
/// a writer for every remembered shape that has none, off the critical path. A seal that finds a
/// ready writer of its exact shape writes and seals it; one that does not allocates as before. A
/// writer is an artifact like any other: unsealed, it is released when dropped, and the bytes it
/// seals are the bytes written, so nothing a caller sees depends on the pool.
#[derive(Default)]
pub struct WriterPool {
    state: Mutex<PoolState>,
}

#[derive(Default)]
struct PoolState {
    /// The most recent distinct shapes, newest last.
    recent: Vec<(String, u64)>,
    ready: Vec<((String, u64), flybus::ArtifactWriter)>,
    allocating: Vec<(String, u64)>,
}

/// Shapes the pool keeps writers for: an environment's three reply artifacts, with the audio
/// chunk's two lengths, and an agent's one.
const POOL_SHAPES: usize = 4;

impl WriterPool {
    fn take(&self, content_type: &str, len: u64) -> Option<flybus::ArtifactWriter> {
        let mut st = self.state.lock().expect("never poisoned");
        let shape = (content_type.to_owned(), len);
        if let Some(at) = st.recent.iter().position(|s| *s == shape) {
            st.recent.remove(at);
        } else if st.recent.len() >= POOL_SHAPES {
            let gone = st.recent.remove(0);
            st.ready.retain(|(s, _)| *s != gone);
        }
        st.recent.push(shape.clone());
        let at = st.ready.iter().position(|(s, _)| *s == shape)?;
        Some(st.ready.swap_remove(at).1)
    }

    /// Allocates, in the background, a writer for every remembered shape that has none.
    fn refill(self: &Arc<Self>, client: &flybus::Client) {
        let wanted: Vec<(String, u64)> = {
            let mut st = self.state.lock().expect("never poisoned");
            let wanted: Vec<_> = st
                .recent
                .iter()
                .filter(|s| !st.ready.iter().any(|(r, _)| r == *s) && !st.allocating.contains(s))
                .cloned()
                .collect();
            st.allocating.extend(wanted.iter().cloned());
            wanted
        };
        for shape in wanted {
            let (pool, client) = (self.clone(), client.clone());
            tokio::spawn(async move {
                let writer = client.artifacts().allocate(shape.1, &shape.0).await;
                let mut st = pool.state.lock().expect("never poisoned");
                st.allocating.retain(|s| *s != shape);
                if let Ok(writer) = writer
                    && st.recent.contains(&shape)
                {
                    st.ready.push((shape, writer));
                }
            });
        }
    }
}

/// How a domain request reached the shell, and so where its attachments are.
pub enum Incoming {
    /// A bus request delivery: the attachments are the delivery's.
    Bus(Box<flybus::Request>),
    /// The in-process local lane ([`LocalLane`]): the caller's own handles, in memory or
    /// sealed, handed over without a bus message.
    Local(Vec<(String, flybus::Artifact)>),
}

impl Incoming {
    fn artifact(&self, name: &str) -> Result<flybus::Artifact, String> {
        match self {
            Incoming::Bus(request) => request.artifact(name).map_err(|e| e.message),
            Incoming::Local(list) => list
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, a)| a.clone())
                .ok_or_else(|| format!("no attachment named {name:?}")),
        }
    }
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
                format!("attachment {name:?} is missing or unowned: {e}"),
            )
        })
    }

    /// True when the request came over the in-process local lane.
    pub fn is_local(&self) -> bool {
        matches!(self.incoming, Incoming::Local(_))
    }

    /// One immutable reply artifact holding `bytes`. Over the local lane it is an in-memory
    /// artifact that owns `bytes` (no copy, no store file, no router round trip); over the bus
    /// it is allocated, written and sealed in the router's store as before.
    pub async fn seal(&self, content_type: &str, bytes: Vec<u8>) -> DomainResult<flybus::Artifact> {
        if self.is_local() {
            return Ok(flybus::Artifact::in_memory(content_type, bytes));
        }
        // A writer allocated ahead for this shape saves the allocation's round trip ([`WriterPool`]).
        let Some(mut writer) = self.writers.take(content_type, bytes.len() as u64) else {
            let _span = crate::profile::span("shell.seal.allocated");
            return crate::media::seal_copy(self.client, content_type.to_owned(), &bytes).await;
        };
        let _span = crate::profile::span("shell.seal.pooled");
        use std::io::Write as _;
        writer
            .write_all(&bytes)
            .map_err(|e| crate::media::store_error("write", &e.to_string()))?;
        writer
            .seal()
            .await
            .map_err(|e| crate::media::store_error("seal", &e.message))
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

    /// The thread allocation this worker's launcher started it within.
    ///
    /// `Worker.Hello` reports it, so a caller bounded by `workers-v1`'s "within launcher
    /// allocation" can read the allocation instead of being told it out of band.
    fn worker_threads(&self) -> u64;

    /// An id this worker will add to every `Worker.Acknowledge` reply, for a test that needs a
    /// worker reporting about something it was never asked about. `None` for a worker that
    /// behaves.
    fn acknowledge_extra_id(&self) -> Option<Id> {
        None
    }

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
    /// The same shell, reachable without the bus by a caller in this process.
    pub local: LocalLane,
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
        self.local.close();
        self.task.abort();
        let _ = self.task.await;
        self.client.close().await;
    }

    /// Stops serving without waiting. The connection closes when the last handle to it is
    /// dropped, which this does. For a supervisor's `Drop`, where there is no runtime to wait
    /// on.
    pub fn abort(self) {
        self.local.close();
        self.task.abort();
    }

    /// Waits until the worker stops serving, which `Worker.Shutdown` makes it do.
    ///
    /// A worker process awaits this and then exits, so the supervisor's `Worker.Shutdown` and
    /// the process's exit are the same event rather than two racing ones.
    pub async fn join(self) {
        let _ = self.task.await;
        self.local.close();
        self.client.close().await;
    }
}

// -------------------------------------------------------------------------------------------
// The in-process local lane (PERF-01)

/// A terminal domain reply delivered over the local lane, with the artifacts it attaches.
pub struct LocalReply {
    pub outcome: SessionRpcOutcome,
    pub artifacts: Vec<(String, flybus::Artifact)>,
}

/// A request admitted by the local lane and not yet answered. Like an admitted bus call, it is
/// in the worker's order already: a request sent after it to the same worker runs after it.
pub struct LocalPending(tokio::sync::oneshot::Receiver<LocalReply>);

impl LocalPending {
    /// Waits for the terminal reply. `None` when the worker stopped before answering, which
    /// the caller treats exactly as a bus call whose connection was lost.
    pub async fn finish(self) -> Option<LocalReply> {
        self.0.await.ok()
    }
}

/// What the local lane dispatches into: one worker's shell, the same one its bus loop uses.
trait LocalDispatch: Send + Sync {
    fn send(
        &self,
        method: String,
        request: SessionRpcRequest,
        attachments: Vec<(String, flybus::Artifact)>,
    ) -> BoxFuture<'_, Result<LocalPending, DomainError>>;
    fn close(&self);
}

/// A handle on a worker's shell for a caller in the same process (`workers-v1` PERF-01
/// amendment). A request sent this way goes through exactly the admission a bus request goes
/// through -- deduplication and the operation key, the result cache, the arrival-order lock and
/// the endpoint -- and returns the same `SessionRpcOutcome`; only the transport differs: no bus
/// message, no router, and reply artifacts the handler may keep in memory
/// ([`HandlerCtx::seal`]). The common `Worker.*` methods are not served here: they stay on the
/// bus, where the supervisor asks them.
#[derive(Clone)]
pub struct LocalLane(Arc<dyn LocalDispatch>);

impl std::fmt::Debug for LocalLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalLane")
    }
}

impl LocalLane {
    /// Admits one domain request in this worker's order and returns without waiting for it
    /// to execute. Refuses what the bus loop would answer without entering the endpoint's
    /// order (the common methods) with UNSUPPORTED, before any mutation.
    pub async fn send(
        &self,
        method: &str,
        request: SessionRpcRequest,
        attachments: Vec<(String, flybus::Artifact)>,
    ) -> Result<LocalPending, DomainError> {
        self.0.send(method.to_owned(), request, attachments).await
    }

    fn close(&self) {
        self.0.close();
    }
}

/// True for a method the local lane carries: every method the shell admits into the endpoint
/// (step mutations, lifecycle and capture, and an endpoint's own read-only extensions such as
/// SERVE-01's `Legacy.FeedStatus`). The common `Worker.*` methods stay on the bus.
pub fn local_lane_carries(method: &str) -> bool {
    !method.starts_with("Worker.")
}

/// How the shell answers one request: over the bus, or into a local waiter.
enum Answer {
    Bus(flybus::Responder),
    Local(tokio::sync::oneshot::Sender<LocalReply>),
}

impl Answer {
    async fn send(self, outcome: SessionRpcOutcome, artifacts: &[(String, flybus::Artifact)]) {
        match self {
            Answer::Bus(responder) => {
                let list: Vec<(&str, &flybus::Artifact)> =
                    artifacts.iter().map(|(n, a)| (n.as_str(), a)).collect();
                let _ = responder.reply(outcome.to_outcome(), &list).await;
            }
            Answer::Local(tx) => {
                let _ = tx.send(LocalReply { outcome, artifacts: artifacts.to_vec() });
            }
        }
    }
}

/// Everything fixed about one worker's shell, shared by its bus loop and its local lane.
struct Shell<E> {
    client: flybus::Client,
    endpoint: Arc<tokio::sync::Mutex<E>>,
    cache: Arc<tokio::sync::Mutex<ResultCache>>,
    order: Arc<LockOrder>,
    status: StatusCell,
    worker_id: Id,
    incarnation_id: Id,
    session_id: Id,
    methods: Vec<&'static str>,
    /// Admission is one request at a time across both lanes, so tickets follow arrival.
    admission: tokio::sync::Mutex<()>,
    closed: std::sync::atomic::AtomicBool,
    /// The admitted mutations' tasks, awaited when the shell stops.
    running: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Reply writers allocated ahead (BUS-01).
    writers: Arc<WriterPool>,
}

/// The local lane's hold on a shell.
struct ShellRef<E>(Arc<Shell<E>>);

impl<E: WorkerEndpoint> LocalDispatch for ShellRef<E> {
    fn send(
        &self,
        method: String,
        request: SessionRpcRequest,
        attachments: Vec<(String, flybus::Artifact)>,
    ) -> BoxFuture<'_, Result<LocalPending, DomainError>> {
        let shell = &self.0;
        Box::pin(async move {
            if shell.closed.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("{method}: the worker {} has stopped", shell.worker_id),
                ));
            }
            if !local_lane_carries(&method) {
                return Err(DomainError::before(
                    ErrorCode::Unsupported,
                    format!("{method} is not carried by the local lane"),
                ));
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            shell.admit(method, request, Incoming::Local(attachments), Answer::Local(tx)).await;
            Ok(LocalPending(rx))
        })
    }

    fn close(&self) {
        self.0.closed.store(true, std::sync::atomic::Ordering::SeqCst);
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
    // Identity and capabilities are fixed for the endpoint's lifetime, so the shell reads them
    // once and never takes the endpoint mutex to answer Hello or Status.
    let common = Common {
        role: endpoint.role(),
        capabilities: endpoint.capabilities(),
        threads: endpoint.worker_threads(),
        extra_ack: endpoint.acknowledge_extra_id(),
    };
    let shell = Arc::new(Shell {
        client: client.clone(),
        session_id: endpoint.session_id(),
        methods: endpoint.methods(),
        endpoint: Arc::new(tokio::sync::Mutex::new(endpoint)),
        cache: cache.clone(),
        order: Arc::new(LockOrder::default()),
        status: status.clone(),
        worker_id: worker_id.clone(),
        incarnation_id: incarnation_id.clone(),
        admission: tokio::sync::Mutex::new(()),
        closed: std::sync::atomic::AtomicBool::new(false),
        running: Mutex::new(Vec::new()),
        writers: Arc::default(),
    });
    let local = LocalLane(Arc::new(ShellRef(shell.clone())));
    let task = tokio::spawn(run(shell, service, common));
    WorkerHandle {
        worker_id,
        incarnation_id,
        service_name,
        service_incarnation,
        status,
        cache,
        local,
        task,
        client,
    }
}

/// What the bus loop answers without entering the endpoint.
struct Common {
    role: Role,
    capabilities: Vec<Id>,
    threads: u64,
    extra_ack: Option<Id>,
}

async fn run<E: WorkerEndpoint>(shell: Arc<Shell<E>>, mut service: flybus::Service, common: Common) {
    let Common { role, capabilities, threads, extra_ack } = common;
    let (worker_id, incarnation_id, session_id) =
        (shell.worker_id.clone(), shell.incarnation_id.clone(), shell.session_id.clone());
    let (status, cache) = (shell.status.clone(), shell.cache.clone());
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
                    threads,
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
                let outcome = match acknowledge(&request, &cache, extra_ack.as_ref()).await {
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
                        // The local lane stops with the bus loop: nothing is admitted after a
                        // Shutdown on either.
                        shell.closed.store(true, std::sync::atomic::Ordering::SeqCst);
                        let _ = responder.reply(outcome.to_outcome(), &[]).await;
                        // Admitted mutations still finish and answer.
                        shell.drain().await;
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
        shell
            .admit(method, request, Incoming::Bus(Box::new(incoming)), Answer::Bus(responder))
            .await;
    }
    shell.drain().await;
}

impl<E: WorkerEndpoint> Shell<E> {
    /// Waits until every admitted mutation has answered.
    async fn drain(&self) {
        let running = std::mem::take(&mut *self.running.lock().expect("never poisoned"));
        for task in running {
            let _ = task.await;
        }
    }

    /// Classification, deduplication and the arrival-order ticket for one endpoint request,
    /// then its execution in a task of its own. Both lanes come here, one request at a time.
    async fn admit(
        self: &Arc<Self>,
        method: String,
        request: SessionRpcRequest,
        incoming: Incoming,
        answer: Answer,
    ) {
        let (worker_id, incarnation_id) = (&self.worker_id, &self.incarnation_id);
        let refuse = |e: DomainError, scope: Option<Scope>| {
            failure(&request.request_id, worker_id, incarnation_id, scope, e)
        };
        // One admission at a time across both lanes: the ticket order is the arrival order.
        let _admission = self.admission.lock().await;
        let _span = crate::profile::span("shell.admit");
        // The contract type already validated the `req-<U64>` form when it read the envelope.
        let serial = request.request_id.clone();
        let class = if self.methods.contains(&method.as_str()) {
            classify_default(&method)
        } else {
            None
        };
        let Some(class) = class else {
            let outcome = refuse(
                DomainError::before(ErrorCode::Unsupported, format!("{method} is not supported")),
                request.scope.clone(),
            );
            answer.send(outcome, &[]).await;
            return;
        };

        let digest_span = crate::profile::span("shell.digest");
        let body = request.body_digest(&method);
        drop(digest_span);
        let body = match body {
            Ok(body) => body,
            Err(e) => {
                let outcome = refuse(DomainError::invalid(format!("{method}: {e}")), request.scope.clone());
                answer.send(outcome, &[]).await;
                return;
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
                let outcome = refuse(DomainError::invalid(format!("{method} requires a scope")), None);
                answer.send(outcome, &[]).await;
                return;
            }
            _ => OperationKey {
                session_id: self.session_id.clone(),
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
            let mut c = self.cache.lock().await;
            c.admit(class, &key, serial.clone(), &body)
        };
        match admission {
            Admission::Replay(reply) => {
                answer.send(reply.outcome.clone(), &reply.artifacts).await;
                return;
            }
            Admission::Refuse(e) => {
                let outcome = refuse(e, request.scope.clone());
                answer.send(outcome, &[]).await;
                return;
            }
            Admission::Execute => {}
        }

        self.status.set_active(Some(request.request_id.clone()));
        // The mutation runs in its own task so the shell keeps reading. That is what lets an
        // exact duplicate arriving mid-execution be refused with IN_PROGRESS while the
        // original call still completes normally. The endpoint mutex, not this loop,
        // enforces one mutation at a time.
        let ticket = self.order.issue();
        let task = tokio::spawn(execute(
            self.clone(),
            ticket,
            Admitted { class, key, serial, body, method, request, incoming },
            answer,
        ));
        let mut running = self.running.lock().expect("never poisoned");
        running.retain(|task| !task.is_finished());
        running.push(task);
    }
}

/// Admitted mutations take the endpoint's lock in the order they arrived.
///
/// Each runs in a task of its own, so the shell keeps reading, and tasks are not scheduled in
/// spawn order: without this, a `State.Capture` followed on the wire by the next
/// `Agent.Prepare` could find the agent already prepared. A ticket per admitted mutation, taken
/// in arrival order, and the lock taken only on the ticket's turn keep the wire's order.
#[derive(Default)]
struct LockOrder {
    issued: std::sync::atomic::AtomicU64,
    turn: std::sync::atomic::AtomicU64,
    changed: tokio::sync::Notify,
}

impl LockOrder {
    fn issue(&self) -> u64 {
        self.issued.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    async fn lock_in_turn<'a, E>(
        &self,
        ticket: u64,
        endpoint: &'a tokio::sync::Mutex<E>,
    ) -> tokio::sync::MutexGuard<'a, E> {
        loop {
            let changed = self.changed.notified();
            if self.turn.load(std::sync::atomic::Ordering::SeqCst) == ticket {
                break;
            }
            changed.await;
        }
        // Queue on the lock first, then hand the turn on: the next ticket queues behind this
        // one on the (fair) mutex.
        let lock = endpoint.lock();
        tokio::pin!(lock);
        let guard = match futures_poll_once(lock.as_mut()).await {
            Some(guard) => guard,
            None => {
                self.advance();
                return lock.await;
            }
        };
        self.advance();
        guard
    }

    fn advance(&self) {
        self.turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_waiters();
    }
}

/// Polls a future once: its output if it is ready, else `None` with the future registered.
async fn futures_poll_once<F: std::future::Future + Unpin>(mut f: F) -> Option<F::Output> {
    std::future::poll_fn(|cx| match std::pin::Pin::new(&mut f).poll(cx) {
        std::task::Poll::Ready(v) => std::task::Poll::Ready(Some(v)),
        std::task::Poll::Pending => std::task::Poll::Ready(None),
    })
    .await
}

/// One admitted request, as `execute` needs it.
struct Admitted {
    class: OpClass,
    key: OperationKey,
    serial: DomainRequestId,
    body: Digest,
    method: String,
    request: SessionRpcRequest,
    incoming: Incoming,
}

/// Runs one admitted mutation, records its reply and answers the call.
async fn execute<E: WorkerEndpoint>(
    shell: Arc<Shell<E>>,
    ticket: u64,
    admitted: Admitted,
    answer: Answer,
) {
    let Admitted { class, key, serial, body, method, request, incoming } = admitted;
    let (worker_id, incarnation_id, status, cache) =
        (&shell.worker_id, &shell.incarnation_id, &shell.status, &shell.cache);
    let outcome = {
        // One mutation at a time: the endpoint mutex is the worker's simulation lock, and it is
        // never held across a bus round trip taken by anything else.
        let mut e = shell.order.lock_in_turn(ticket, &shell.endpoint).await;
        let _span = crate::profile::span("shell.handle");
        let ctx = HandlerCtx {
            method: &method,
            request: &request,
            client: &shell.client,
            incoming: &incoming,
            writers: &shell.writers,
        };
        if matches!(incoming, Incoming::Bus(_)) && long_method(&method) && blocking_allowed() {
            // A long bus request's handler runs with this worker thread handed over to blocking
            // work (BUS-01): the legacy agent's warm-up is seconds of synchronous computation, and
            // a Tokio worker blocked that long strands the tasks queued on it -- the shell's own
            // bus loop and its connection's reader among them -- so a duplicate arriving
            // mid-execution could not be answered IN_PROGRESS and `ipc-v1` section 6's resolution
            // of a slow Initialize ran out ("never resolved"). `block_in_place` moves those tasks
            // to another thread first. A step's handlers (milliseconds) run in place: the hand-over
            // is a thread wake-up of its own. The local lane's caller awaits the handler on its own
            // task and is unchanged.
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(e.handle(ctx))
            })
        } else {
            e.handle(ctx).await
        }
    };
    // A deferred reply finishes here, with the endpoint's lock released.
    let outcome = match outcome {
        Ok(HandlerReply { deferred: Some(rest), .. }) => rest.await,
        other => other,
    };
    let _span = crate::profile::span("shell.record");
    match outcome {
        Ok(mut reply) => {
            let result = std::mem::take(&mut reply.result);
            let outcome = success(&request, worker_id, incarnation_id, result);
            // The cache owns its own hold on every artifact, so a replay survives the first
            // caller consuming its delivery. The holds are independent router round trips, so
            // they are taken concurrently; an in-memory artifact is its own hold.
            let retains: Vec<_> = reply
                .artifacts
                .iter()
                .map(|(name, artifact)| {
                    let (name, artifact) = (name.clone(), artifact.clone());
                    async move {
                        // An in-memory artifact is its own hold, and so is a sealed one this
                        // worker holds explicitly (its own seal): a clone keeps it as long as the
                        // cache needs it, without another router round trip (BUS-01). Only a
                        // delivery's handle -- a forwarded request attachment -- needs a hold of
                        // its own, so the delivery can be consumed.
                        if artifact.is_in_memory() || artifact.is_hold() {
                            return (name, artifact);
                        }
                        let task = tokio::spawn(async move {
                            match artifact.retain().await {
                                Ok(hold) => (name, hold),
                                Err(_) => (name, artifact),
                            }
                        });
                        task.await.expect("a retain task never panics")
                    }
                })
                .collect();
            let mut holds = Vec::with_capacity(retains.len());
            for retain in retains {
                holds.push(retain.await);
            }
            let cache_span = crate::profile::span("shell.record.cache");
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
            drop(cache_span);
            let reply_span = crate::profile::span("shell.reply");
            answer.send(outcome, &reply.artifacts).await;
            drop(reply_span);
            // The next step's writers, allocated while the caller goes on (bus requests only:
            // the lane seals nothing into the store).
            if matches!(incoming, Incoming::Bus(_)) {
                shell.writers.refill(&shell.client);
            }
        }
        Err(e) => {
            if e.mutation == MutationCertainty::None {
                // Nothing happened, so the key stays free for the corrected request.
                let mut c = cache.lock().await;
                c.abandon(&key);
            } else {
                let cached = CachedReply::new(failure_outcome(
                    &request.request_id,
                    worker_id,
                    incarnation_id,
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
                worker_id,
                incarnation_id,
                request.scope.clone(),
                e,
            );
            answer.send(outcome, &[]).await;
        }
    }
}

/// The methods whose handlers can run for seconds: the warm-ups, the restores and the captures.
/// A step's methods (Prepare, Advance, Commit: milliseconds) are not among them; neither are the
/// read-only extensions.
fn long_method(method: &str) -> bool {
    method.ends_with(".Initialize") || method.starts_with("State.") || method == "Agent.Rollback"
}

/// Whether this thread is a worker of a multi-thread Tokio runtime, where
/// [`tokio::task::block_in_place`] is allowed.
fn blocking_allowed() -> bool {
    tokio::runtime::Handle::try_current()
        .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
}

/// The retention class of every method name the contracts define.
fn classify_default(method: &str) -> Option<OpClass> {
    match method {
        "Agent.Prepare" | "Agent.Commit" | "Environment.Advance" => Some(OpClass::StepMutation),
        // The extension methods of `workers-v1` section 7 are mutations under the step operation
        // key (`legacy-gameboy-v1` section 11): `SaveSlot` at `(e, k)`, `RestoreSlot` and
        // `Agent.Rollback` at the new epoch `(e', k)`. A lost reply replays from the same cache
        // rather than rolling back twice.
        "Agent.Rollback" | "Environment.SaveSlot" | "Environment.RestoreSlot" => {
            Some(OpClass::StepMutation)
        }
        // `ipc-v1` section 5 retains lifecycle *and capture* replies until
        // `Worker.Acknowledge`. The restore methods join them: their replies carry a
        // once-only token and, for an environment, the restored observation's artifact, and a
        // duplicate domain request must replay that reply rather than stage or activate a
        // second time. They are not step mutations -- they carry no committed step of their
        // own and are not keyed by one.
        "Agent.Initialize"
        | "Environment.Initialize"
        | "State.Capture"
        | "State.StageRestore"
        | "State.ActivateRestore" => Some(OpClass::Lifecycle),
        "Worker.Hello" | "Worker.Status" | "Worker.Acknowledge" | "Worker.Shutdown" => {
            Some(OpClass::ReadOnly)
        }
        // The legacy agent's feed status (SERVE-01): a read of the committed boundary that
        // changes nothing, so a duplicate simply reads again.
        crate::legacy_agent::METHOD_FEED_STATUS => Some(OpClass::ReadOnly),
        _ => None,
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

#[allow(clippy::too_many_arguments)]
fn hello(
    request: &SessionRpcRequest,
    session_id: &Id,
    worker_id: &Id,
    incarnation_id: &Id,
    role: Role,
    capabilities: &[Id],
    worker_threads: u64,
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
        worker_threads,
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
    extra: Option<&Id>,
) -> DomainResult<Map<String, Value>> {
    let params: AcknowledgeParams = AcknowledgeParams::from_json(&request.params)
        .map_err(|e| DomainError::invalid(format!("Worker.Acknowledge: {e}")))?;
    if params.request_ids.is_empty() || params.request_ids.len() > MAX_ACKNOWLEDGE {
        return Err(DomainError::invalid("Worker.Acknowledge takes 1..=16 request ids"));
    }
    let mut acknowledged = {
        let mut c = cache.lock().await;
        c.acknowledge(&params.request_ids)
    };
    // A deliberately misbehaving worker, for the caller-side subset check to refuse.
    if let Some(extra) = extra
        && let Ok(id) = DomainRequestId::parse(extra)
        && !params.request_ids.contains(&id)
    {
        acknowledged.push(id);
    }
    let result = AcknowledgeResult { acknowledged };
    Ok(object(result.to_json()))
}
