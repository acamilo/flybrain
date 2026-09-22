//! The launcher and supervisor: it starts participants, gives them their identities, checks
//! that they are the participants the composition configured, and reaps them.
//!
//! SESSION-02's subject is that one agent process per fly and one environment process under
//! the coordinator behave exactly as the in-process composition does. The same launcher also
//! starts the two comparison variants -- every participant as a task on the coordinator's
//! runtime, and every participant on its own dedicated thread -- so the three can be compared
//! without writing a second composition.
//!
//! ```text
//! Launcher ── thread budget   ──> one allocation per participant
//!          ── identity        ──> client id, service name, worker id, agent/port binding
//!          ── Worker.Hello    ──> the registration the coordinator then pins
//!          ── Worker.Status   ──> health, bounded by the supervisor's own clock
//!          ── Worker.Shutdown ──> reaped, and terminated if it does not stop
//! ```
//!
//! The launcher is the configured supervisor: it holds `Worker.Shutdown` authority and the
//! bus grants that go with it. A worker has no authority over it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use flybus::{Client, ClientConfig, Router, ServiceConfig, Transport, UnixListenerHandle};

use crate::agent::{AgentConfig, AgentFaults, FakeAgentWorker};
use crate::environment::{CounterEnvironment, EnvironmentConfig, EnvironmentFaults};
use crate::rpc::WorkerRef;
use crate::types::*;
use crate::worker::{StatusCell, WorkerHandle, serve};

/// Which transport a participant's connection runs over. Both must produce the same
/// behaviour, which is a test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Memory,
    Unix,
}

/// Where a participant runs.
///
/// A separate process is the SESSION-02 subject; the other two are the comparison variants
/// the slice is measured against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Every participant is a task on the coordinator's own runtime. This is SESSION-01.
    #[default]
    InProcess,
    /// Every participant owns a dedicated OS thread and its own runtime, in this process.
    Thread,
    /// One agent process per fly and one environment process, over Unix-domain sockets.
    Process,
}

impl ExecutionMode {
    pub fn label(&self) -> &'static str {
        match self {
            ExecutionMode::InProcess => "in-process",
            ExecutionMode::Thread => "thread",
            ExecutionMode::Process => "process",
        }
    }

    /// A separate process reaches the router only over a socket; the other two may use either.
    pub fn transport(&self, configured: Via) -> Via {
        match self {
            ExecutionMode::InProcess => configured,
            ExecutionMode::Thread | ExecutionMode::Process => Via::Unix,
        }
    }

    pub fn all() -> [ExecutionMode; 3] {
        [ExecutionMode::InProcess, ExecutionMode::Thread, ExecutionMode::Process]
    }
}

// -------------------------------------------------------------------------------------------
// The thread budget

/// The total thread allocation the launcher may hand out, and what it has handed out.
///
/// `workers-v1` requires `Agent.Initialize`'s `workerThreads` to lie within the launcher
/// allocation. This is that allocation: the launcher refuses to start a participant whose
/// request would take the composition over its configured total, and an agent endpoint
/// refuses an `Agent.Initialize` asking for more threads than its launcher gave it.
#[derive(Clone, Debug)]
pub struct ThreadBudget {
    total: usize,
    coordinator: usize,
    allocated: BTreeMap<Id, usize>,
}

impl ThreadBudget {
    /// A budget of `total` threads, `coordinator` of them reserved for the coordinator, its
    /// router and its store.
    pub fn new(total: usize, coordinator: usize) -> Result<ThreadBudget, DomainError> {
        if total == 0 {
            return Err(DomainError::invalid("a thread budget is at least one thread"));
        }
        if coordinator > total {
            return Err(DomainError::invalid(
                "the coordinator's reservation exceeds the total thread budget",
            ));
        }
        Ok(ThreadBudget { total, coordinator, allocated: BTreeMap::new() })
    }

    /// The default budget: one thread per physical core, one of them the coordinator's.
    pub fn for_this_machine() -> ThreadBudget {
        let cores = crate::metrics::physical_cores().max(2);
        ThreadBudget::new(cores, 1).expect("two or more cores make a valid budget")
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn coordinator(&self) -> usize {
        self.coordinator
    }

    pub fn used(&self) -> usize {
        self.coordinator + self.allocated.values().sum::<usize>()
    }

    pub fn remaining(&self) -> usize {
        self.total.saturating_sub(self.used())
    }

    pub fn allocation(&self, who: &Id) -> Option<usize> {
        self.allocated.get(who).copied()
    }

    /// Reserves `want` threads for `who`. A request the total cannot cover is refused before
    /// anything is started: a capacity refusal, not a runtime fault.
    pub fn allocate(&mut self, who: &Id, want: usize) -> Result<usize, DomainError> {
        if want == 0 {
            return Err(DomainError::invalid("workerThreads must be >= 1"));
        }
        if self.allocated.contains_key(who) {
            return Err(DomainError::before(
                ErrorCode::Conflict,
                format!("{who} already holds a thread allocation"),
            ));
        }
        if want > self.remaining() {
            return Err(DomainError::before(
                ErrorCode::Busy,
                format!(
                    "{who} asked for {want} threads; {} of {} remain in the launcher allocation",
                    self.remaining(),
                    self.total
                ),
            ));
        }
        self.allocated.insert(who.clone(), want);
        Ok(want)
    }

    pub fn release(&mut self, who: &Id) {
        self.allocated.remove(who);
    }

    /// Every allocation, in agent-id order, for the report.
    pub fn allocations(&self) -> Vec<(Id, usize)> {
        self.allocated.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }
}

// -------------------------------------------------------------------------------------------
// Identities and policies

/// Everything the launcher configures about one participant before it exists.
///
/// The bus client id, the service name and the worker id are launcher configuration. The
/// worker proves it is that participant in `Worker.Hello`; a process started under any other
/// identity is refused there rather than adopted into the session.
#[derive(Clone, Debug)]
pub struct WorkerIdentity {
    pub session_id: Id,
    pub client_id: String,
    pub service: String,
    pub worker_id: Id,
    pub incarnation_id: Id,
    pub role: Role,
    /// The port an agent is bound to. The environment owns the ports, not one of them.
    pub port_id: Option<Id>,
    /// The thread allocation this participant runs within.
    pub worker_threads: usize,
}

/// How long the supervisor waits before it calls a participant unhealthy.
///
/// The `ipc-v1` section 6 prototype values: probe after two seconds without a reply, fail
/// after ten without progress, with a separate budget for a participant that is still
/// starting. These are failure-detection values, not a latency goal.
#[derive(Clone, Copy, Debug)]
pub struct HealthPolicy {
    pub probe: Duration,
    pub fail: Duration,
    pub boot: Duration,
}

impl Default for HealthPolicy {
    fn default() -> HealthPolicy {
        HealthPolicy {
            probe: Duration::from_secs(2),
            fail: Duration::from_secs(10),
            boot: Duration::from_secs(30),
        }
    }
}

/// What one agent participant is started with.
#[derive(Clone, Debug)]
pub struct AgentLaunch {
    pub session_id: Id,
    pub agent_id: Id,
    pub port_id: Id,
    pub incarnation_id: Id,
    pub tick_duration: RationalNs,
    pub warmup_ticks: u64,
    /// What the launcher asks the budget for.
    pub worker_threads: usize,
    pub faults: AgentFaults,
    /// The configured client id. A replacement worker connects under its own.
    pub client_id: String,
    pub service: String,
}

/// What the environment participant is started with.
#[derive(Clone, Debug)]
pub struct EnvironmentLaunch {
    pub session_id: Id,
    pub worker_id: Id,
    pub incarnation_id: Id,
    pub step_duration: RationalNs,
    pub ports: Vec<Id>,
    pub worker_threads: usize,
    pub faults: EnvironmentFaults,
    pub client_id: String,
    pub service: String,
}

// -------------------------------------------------------------------------------------------
// A launched participant

/// A participant on its own thread, with its own runtime.
struct ThreadWorker {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ThreadWorker {
    /// Ends the thread and waits for its runtime to finish.
    fn stop(&mut self) {
        drop(self.stop.take());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    fn finished(&self) -> bool {
        self.join.as_ref().map(|j| j.is_finished()).unwrap_or(true)
    }
}

enum Body {
    Task(Option<WorkerHandle>),
    Thread(ThreadWorker),
    Process(Option<std::process::Child>),
}

/// One started participant: its configured identity, the registration a caller pins, and
/// whatever the launcher needs to reap it.
pub struct LaunchedWorker {
    pub identity: WorkerIdentity,
    /// The bus `serviceIncarnation` a caller pins. Not a process id.
    pub service_incarnation: String,
    /// The domain `incarnationId` `Worker.Hello` reported.
    pub domain_incarnation: Id,
    /// The operating-system process, when this participant has one of its own.
    pub pid: Option<u32>,
    /// The highest peak resident set the launcher has read for that process, in KiB.
    pub peak_rss_kib: u64,
    body: Body,
    status: Option<StatusCell>,
    /// The per-participant socket endpoint, kept alive for the connection's lifetime.
    _listener: Option<UnixListenerHandle>,
}

impl LaunchedWorker {
    /// The reference a caller pins: service, registration, worker id and negotiated
    /// incarnation.
    pub fn worker_ref(&self) -> WorkerRef {
        let mut r = WorkerRef::new(
            &self.identity.service,
            &self.service_incarnation,
            &self.identity.worker_id,
        );
        r.domain_incarnation = Some(self.domain_incarnation.clone());
        r
    }

    /// The local status cell of a participant in this process. A separate process answers
    /// `Worker.Status` over the bus instead, which every mode also supports.
    pub fn status(&self) -> Option<StatusCell> {
        self.status.clone()
    }

    /// The progress counter of a participant in this process, or `None` for a separate one.
    pub fn progress_counter(&self) -> Option<u64> {
        self.status.as_ref().map(StatusCell::progress_counter)
    }

    /// True while the participant is still running, as far as the operating system knows.
    pub fn alive(&mut self) -> bool {
        match &mut self.body {
            Body::Task(handle) => handle.is_some(),
            Body::Thread(thread) => !thread.finished(),
            Body::Process(Some(child)) => matches!(child.try_wait(), Ok(None)),
            Body::Process(None) => false,
        }
    }

    fn refresh_rss(&mut self) {
        if let Some(pid) = self.pid
            && let Some(kib) = crate::metrics::peak_rss_kib_of(pid)
        {
            self.peak_rss_kib = self.peak_rss_kib.max(kib);
        }
    }
}

/// How a participant ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReapOutcome {
    /// It answered `Worker.Shutdown` and stopped on its own.
    Stopped,
    /// It did not stop within the supervisor's budget and was terminated.
    Terminated,
    /// It was already gone when the launcher reached it.
    AlreadyGone,
}

// -------------------------------------------------------------------------------------------
// Endpoints: how a participant reaches the router

struct Endpoints {
    router: Router,
    via: Via,
    store_root: PathBuf,
    sockets: PathBuf,
    next_socket: AtomicU64,
    listeners: Mutex<Vec<UnixListenerHandle>>,
}

impl Endpoints {
    fn socket_path(&self, client_id: &str) -> PathBuf {
        let n = self.next_socket.fetch_add(1, Ordering::Relaxed);
        self.sockets.join(format!("{client_id}-{n}.sock"))
    }

    /// A connection this process owns, over the configured transport.
    async fn connect(&self, client_id: &str) -> Result<Client, flybus::BusError> {
        let transport = match self.via {
            Via::Memory => self.router.connect_in_memory_as(client_id),
            Via::Unix => {
                let path = self.socket_path(client_id);
                let listener = self.listen(&path, client_id).await?;
                let transport = Transport::unix(&path).await.map_err(|e| {
                    flybus::BusError::new(flybus::ErrorCode::RouterLost, format!("connect: {e}"))
                })?;
                self.listeners.lock().expect("not poisoned").push(listener);
                transport
            }
        };
        Client::connect(transport, ClientConfig::new(client_id, &self.store_root)).await
    }

    /// An endpoint for someone else to connect to: a separate process, or a thread with its
    /// own runtime.
    async fn endpoint_for(
        &self,
        client_id: &str,
    ) -> Result<(PathBuf, UnixListenerHandle), flybus::BusError> {
        let path = self.socket_path(client_id);
        let listener = self.listen(&path, client_id).await?;
        Ok((path, listener))
    }

    async fn listen(
        &self,
        path: &Path,
        client_id: &str,
    ) -> Result<UnixListenerHandle, flybus::BusError> {
        self.router.listen_unix_as(path, client_id).await.map_err(|e| {
            flybus::BusError::new(flybus::ErrorCode::RouterLost, format!("listen: {e}"))
        })
    }
}

// -------------------------------------------------------------------------------------------
// The launcher

/// Starts, identifies, health-checks and reaps the session's participants.
pub struct Launcher {
    endpoints: Endpoints,
    mode: ExecutionMode,
    /// The worker program, for [`ExecutionMode::Process`].
    program: PathBuf,
    budget: ThreadBudget,
    health: HealthPolicy,
    /// The supervisor's own bus connection. It calls `Worker.Hello`, `Worker.Status` and
    /// `Worker.Shutdown`, and nothing else.
    supervisor: Client,
    workers: BTreeMap<Id, LaunchedWorker>,
    serial: u64,
}

/// The bus client id the supervisor connects under.
pub const SUPERVISOR_CLIENT: &str = "launcher";

/// The environment variable that names the worker program, for a checkout whose binary is not
/// beside the running executable.
pub const WORKER_PROGRAM_ENV: &str = "FLY_SESSION_WORKER";

/// Where the worker program is: by configuration, or beside the running executable.
///
/// The worker is a subcommand of this crate's one binary, so a build that produced the tests
/// produced it too, one directory above them.
pub fn default_worker_program() -> PathBuf {
    if let Some(configured) = std::env::var_os(WORKER_PROGRAM_ENV) {
        return PathBuf::from(configured);
    }
    let name = "fly-session";
    if let Ok(exe) = std::env::current_exe() {
        let here = exe.parent().map(Path::to_path_buf);
        let above = exe.parent().and_then(Path::parent).map(Path::to_path_buf);
        for dir in [here, above].into_iter().flatten() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from(name)
}

impl Launcher {
    /// Connects the supervisor and prepares the launcher. Nothing is started yet.
    pub async fn start(
        router: Router,
        mode: ExecutionMode,
        via: Via,
        store_root: impl Into<PathBuf>,
        sockets: impl Into<PathBuf>,
        budget: ThreadBudget,
    ) -> Result<Launcher, flybus::BusError> {
        let sockets: PathBuf = sockets.into();
        std::fs::create_dir_all(&sockets).map_err(|e| {
            flybus::BusError::new(flybus::ErrorCode::StoreFailure, format!("sockets: {e}"))
        })?;
        let endpoints = Endpoints {
            router,
            via: mode.transport(via),
            store_root: store_root.into(),
            sockets,
            next_socket: AtomicU64::new(0),
            listeners: Mutex::new(Vec::new()),
        };
        let supervisor = endpoints.connect(SUPERVISOR_CLIENT).await?;
        Ok(Launcher {
            endpoints,
            mode,
            program: default_worker_program(),
            budget,
            health: HealthPolicy::default(),
            supervisor,
            workers: BTreeMap::new(),
            serial: 0,
        })
    }

    pub fn mode(&self) -> ExecutionMode {
        self.mode
    }

    pub fn budget(&self) -> &ThreadBudget {
        &self.budget
    }

    pub fn health_policy(&self) -> HealthPolicy {
        self.health
    }

    pub fn set_health_policy(&mut self, health: HealthPolicy) {
        self.health = health;
    }

    pub fn set_program(&mut self, program: impl Into<PathBuf>) {
        self.program = program.into();
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn router(&self) -> &Router {
        &self.endpoints.router
    }

    pub fn supervisor(&self) -> &Client {
        &self.supervisor
    }

    pub fn worker(&self, worker_id: &Id) -> Option<&LaunchedWorker> {
        self.workers.get(worker_id)
    }

    pub fn worker_mut(&mut self, worker_id: &Id) -> Option<&mut LaunchedWorker> {
        self.workers.get_mut(worker_id)
    }

    pub fn worker_ids(&self) -> Vec<Id> {
        self.workers.keys().cloned().collect()
    }

    /// An ordinary connection for a participant this process drives, such as the coordinator.
    pub async fn connect(&self, client_id: &str) -> Result<Client, flybus::BusError> {
        self.endpoints.connect(client_id).await
    }

    fn next_serial(&mut self) -> u64 {
        self.serial += 1;
        self.serial
    }

    // ---------------------------------------------------------------------------------------
    // Starting participants

    /// Starts one agent, in the launcher's configured mode, and identifies it.
    pub async fn launch_agent(&mut self, spec: AgentLaunch) -> Result<WorkerIdentity, DomainError> {
        let threads = self.budget.allocate(&spec.agent_id, spec.worker_threads)?;
        let identity = WorkerIdentity {
            session_id: spec.session_id.clone(),
            client_id: spec.client_id.clone(),
            service: spec.service.clone(),
            worker_id: spec.agent_id.clone(),
            incarnation_id: spec.incarnation_id.clone(),
            role: Role::Agent,
            port_id: Some(spec.port_id.clone()),
            worker_threads: threads,
        };
        let started = self.start_participant(&identity, Started::Agent(spec.clone()), threads).await;
        match started {
            Ok(worker) => {
                let identity = worker.identity.clone();
                self.workers.insert(spec.agent_id.clone(), worker);
                Ok(identity)
            }
            Err(e) => {
                self.budget.release(&spec.agent_id);
                Err(e)
            }
        }
    }

    /// Starts the environment, in the launcher's configured mode, and identifies it.
    pub async fn launch_environment(
        &mut self,
        spec: EnvironmentLaunch,
    ) -> Result<WorkerIdentity, DomainError> {
        let threads = self.budget.allocate(&spec.worker_id, spec.worker_threads)?;
        let identity = WorkerIdentity {
            session_id: spec.session_id.clone(),
            client_id: spec.client_id.clone(),
            service: spec.service.clone(),
            worker_id: spec.worker_id.clone(),
            incarnation_id: spec.incarnation_id.clone(),
            role: Role::Environment,
            port_id: None,
            worker_threads: threads,
        };
        let started = self
            .start_participant(&identity, Started::Environment(spec.clone()), threads)
            .await;
        match started {
            Ok(worker) => {
                let identity = worker.identity.clone();
                self.workers.insert(spec.worker_id.clone(), worker);
                Ok(identity)
            }
            Err(e) => {
                self.budget.release(&spec.worker_id);
                Err(e)
            }
        }
    }

    async fn start_participant(
        &mut self,
        identity: &WorkerIdentity,
        what: Started,
        threads: usize,
    ) -> Result<LaunchedWorker, DomainError> {
        let (body, status, listener) = match self.mode {
            ExecutionMode::InProcess => {
                let (handle, status) = self.serve_here(identity, &what).await?;
                (Body::Task(Some(handle)), Some(status), None)
            }
            ExecutionMode::Thread => {
                let (thread, listener) = self.serve_on_a_thread(identity, &what, threads).await?;
                (Body::Thread(thread), None, Some(listener))
            }
            ExecutionMode::Process => {
                let (child, listener) = self.spawn_process(identity, &what, threads).await?;
                (Body::Process(Some(child)), None, Some(listener))
            }
        };
        let pid = match &body {
            Body::Process(Some(child)) => Some(child.id()),
            _ => None,
        };
        // The registration is discovered, not assumed: the launcher says hello with the
        // identity it configured, and the reply is what the coordinator later pins.
        let identified = self.identify(identity).await;
        let (service_incarnation, domain_incarnation) = match identified {
            Ok(pair) => pair,
            Err(e) => {
                let mut dying = LaunchedWorker {
                    identity: identity.clone(),
                    service_incarnation: String::new(),
                    domain_incarnation: identity.incarnation_id.clone(),
                    pid,
                    peak_rss_kib: 0,
                    body,
                    status,
                    _listener: listener,
                };
                terminate(&mut dying);
                return Err(e);
            }
        };
        let mut worker = LaunchedWorker {
            identity: identity.clone(),
            service_incarnation,
            domain_incarnation,
            pid,
            peak_rss_kib: 0,
            body,
            status,
            _listener: listener,
        };
        worker.refresh_rss();
        Ok(worker)
    }

    async fn serve_here(
        &self,
        identity: &WorkerIdentity,
        what: &Started,
    ) -> Result<(WorkerHandle, StatusCell), DomainError> {
        let client = self
            .endpoints
            .connect(&identity.client_id)
            .await
            .map_err(|e| launch_error(&identity.worker_id, &e))?;
        let service = register_with_retry(&client, &identity.service, self.health.boot)
            .await
            .map_err(|e| launch_error(&identity.worker_id, &e))?;
        Ok(match what {
            Started::Agent(spec) => {
                let endpoint = FakeAgentWorker::new(agent_config(spec, identity.worker_threads));
                let status = endpoint.status();
                (serve(client, service, endpoint), status)
            }
            Started::Environment(spec) => {
                let endpoint = CounterEnvironment::new(environment_config(spec));
                let status = endpoint.status();
                (serve(client, service, endpoint), status)
            }
        })
    }

    async fn serve_on_a_thread(
        &self,
        identity: &WorkerIdentity,
        what: &Started,
        threads: usize,
    ) -> Result<(ThreadWorker, UnixListenerHandle), DomainError> {
        let (path, listener) = self
            .endpoints
            .endpoint_for(&identity.client_id)
            .await
            .map_err(|e| launch_error(&identity.worker_id, &e))?;
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let client_id = identity.client_id.clone();
        let service_name = identity.service.clone();
        let store_root = self.endpoints.store_root.clone();
        let what = what.clone();
        let worker_threads = identity.worker_threads;
        let join = std::thread::Builder::new()
            .name(format!("fly-session-{}", identity.worker_id))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(threads)
                    .enable_all()
                    .build();
                let runtime = match runtime {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("runtime: {e}")));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let served = serve_one(
                        &path,
                        &client_id,
                        &service_name,
                        &store_root,
                        &what,
                        worker_threads,
                    )
                    .await;
                    let handle = match served {
                        Ok(handle) => handle,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };
                    let _ = ready_tx.send(Ok(()));
                    // The thread lives until the launcher drops its stop sender, whether the
                    // worker answered Shutdown or not.
                    let _ = stop_rx.await;
                    handle.stop().await;
                });
            })
            .map_err(|e| {
                DomainError::new(
                    ErrorCode::Internal,
                    format!("{}: thread: {e}", identity.worker_id),
                    MutationCertainty::None,
                )
            })?;
        match ready_rx.recv_timeout(self.health.boot) {
            Ok(Ok(())) => Ok((ThreadWorker { stop: Some(stop_tx), join: Some(join) }, listener)),
            Ok(Err(e)) => {
                drop(stop_tx);
                let _ = join.join();
                Err(DomainError::new(
                    ErrorCode::BackendFailure,
                    format!("{}: {e}", identity.worker_id),
                    MutationCertainty::None,
                ))
            }
            Err(_) => {
                drop(stop_tx);
                Err(DomainError::new(
                    ErrorCode::BackendFailure,
                    format!("{} did not register within its boot budget", identity.worker_id),
                    MutationCertainty::None,
                ))
            }
        }
    }

    async fn spawn_process(
        &self,
        identity: &WorkerIdentity,
        what: &Started,
        threads: usize,
    ) -> Result<(std::process::Child, UnixListenerHandle), DomainError> {
        let (path, listener) = self
            .endpoints
            .endpoint_for(&identity.client_id)
            .await
            .map_err(|e| launch_error(&identity.worker_id, &e))?;
        let mut command = std::process::Command::new(&self.program);
        command
            .arg(what.subcommand())
            .arg("--socket")
            .arg(&path)
            .arg("--store-root")
            .arg(&self.endpoints.store_root)
            .arg("--client-id")
            .arg(&identity.client_id)
            .arg("--service")
            .arg(&identity.service)
            .arg("--threads")
            .arg(threads.to_string());
        for (flag, value) in what.arguments() {
            command.arg(flag).arg(value);
        }
        command.stdin(std::process::Stdio::null());
        let child = command.spawn().map_err(|e| {
            DomainError::new(
                ErrorCode::BackendFailure,
                format!(
                    "{}: starting {}: {e}",
                    identity.worker_id,
                    self.program.display()
                ),
                MutationCertainty::None,
            )
        })?;
        Ok((child, listener))
    }

    // ---------------------------------------------------------------------------------------
    // Identity and health

    /// Says hello as the supervisor and returns the registration and domain incarnation.
    ///
    /// The `expectedWorkerId` and role are the launcher's own configuration, so a participant
    /// that is not the one the composition configured is refused here, before the coordinator
    /// has pinned anything.
    async fn identify(&mut self, identity: &WorkerIdentity) -> Result<(String, Id), DomainError> {
        let params = HelloParams {
            session_id: identity.session_id.clone(),
            expected_worker_id: identity.worker_id.clone(),
            role: identity.role,
            supported_majors: vec![1],
        };
        let request_id = DomainRequestId::from_serial(self.next_serial());
        let deadline = Instant::now() + self.health.boot;
        loop {
            let payload = object(
                SessionRpcRequest {
                    request_id: request_id.clone(),
                    scope: None,
                    params: Value::Object(object(params.to_json())),
                }
                .to_json(),
            );
            // The registration is not pinned yet: this call is how the launcher learns it.
            let pending = self
                .supervisor
                .call(&identity.service, None, "Worker.Hello", payload, &[])
                .await;
            match pending {
                Ok(mut pending) => {
                    let service_incarnation = pending.service_incarnation().to_owned();
                    let result = tokio::time::timeout(self.health.fail, pending.result()).await;
                    let result = match result {
                        Ok(Ok(result)) => result,
                        // The registration this call reached was the predecessor's, which is
                        // still letting go: a handover, not the new worker's answer. Nothing
                        // is adopted from it -- the loop asks again for the live one.
                        Ok(Err(e)) if handover(&e) && Instant::now() < deadline => {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                            continue;
                        }
                        Ok(Err(e)) => return Err(launch_error(&identity.worker_id, &e)),
                        Err(_) => {
                            return Err(DomainError::new(
                                ErrorCode::BackendFailure,
                                format!("{} did not answer Worker.Hello", identity.worker_id),
                                MutationCertainty::Unknown,
                            ));
                        }
                    };
                    let outcome =
                        SessionRpcOutcome::from_json(&Value::Object(result.outcome().clone()))
                            .map_err(|e| {
                                DomainError::invalid(format!(
                                    "{}: unreadable Worker.Hello outcome: {e}",
                                    identity.worker_id
                                ))
                            })?;
                    let value = outcome_result(&outcome)?;
                    let hello: HelloResult = HelloResult::from_json(value).map_err(|e| {
                        DomainError::invalid(format!(
                            "{}: unreadable HelloResult: {e}",
                            identity.worker_id
                        ))
                    })?;
                    if hello.worker_id != identity.worker_id
                        || hello.role != identity.role
                        || hello.incarnation_id != identity.incarnation_id
                    {
                        return Err(DomainError::before(
                            ErrorCode::IdentityMismatch,
                            format!(
                                "{} answered as another worker, role or incarnation",
                                identity.worker_id
                            ),
                        ));
                    }
                    return Ok((service_incarnation, hello.incarnation_id));
                }
                Err(e) if handover(&e) && Instant::now() < deadline => {
                    // Still starting: it has not registered its service yet, or the
                    // registration it replaces has not finished closing.
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(e) => return Err(launch_error(&identity.worker_id, &e)),
            }
        }
    }

    /// Asks one participant for its status, bounded by the supervisor's own clock.
    ///
    /// The answer never waits for a numerical operation, so this is health and not progress:
    /// a worker in the middle of a mutation still answers.
    pub async fn health_check(&mut self, worker_id: &Id) -> Result<StatusResult, DomainError> {
        let Some(worker) = self.workers.get(worker_id) else {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                format!("{worker_id} is not a launched participant"),
            ));
        };
        let service = worker.identity.service.clone();
        let incarnation = worker.service_incarnation.clone();
        let request_id = DomainRequestId::from_serial(self.next_serial());
        let payload = object(
            SessionRpcRequest {
                request_id,
                scope: None,
                params: Value::Object(Map::new()),
            }
            .to_json(),
        );
        let call = self
            .supervisor
            .call(&service, Some(&incarnation), "Worker.Status", payload, &[]);
        let result = match tokio::time::timeout(self.health.fail, call).await {
            Ok(Ok(mut pending)) => {
                match tokio::time::timeout(self.health.fail, pending.result()).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(e)) => return Err(launch_error(worker_id, &e)),
                    Err(_) => return Err(unresponsive(worker_id)),
                }
            }
            Ok(Err(e)) => return Err(launch_error(worker_id, &e)),
            Err(_) => return Err(unresponsive(worker_id)),
        };
        let outcome = SessionRpcOutcome::from_json(&Value::Object(result.outcome().clone()))
            .map_err(|e| DomainError::invalid(format!("{worker_id}: {e}")))?;
        let value = outcome_result(&outcome)?;
        let status = StatusResult::from_json(value)
            .map_err(|e| DomainError::invalid(format!("{worker_id}: {e}")))?;
        if let Some(worker) = self.workers.get_mut(worker_id) {
            worker.refresh_rss();
        }
        Ok(status)
    }

    /// Health-checks every participant, and reports each one's answer or its failure.
    pub async fn health_check_all(&mut self) -> Vec<(Id, Result<StatusResult, DomainError>)> {
        let mut out = Vec::new();
        for worker_id in self.worker_ids() {
            let status = self.health_check(&worker_id).await;
            out.push((worker_id, status));
        }
        out
    }

    // ---------------------------------------------------------------------------------------
    // Ending participants

    /// Asks one participant to stop, then makes sure it has.
    ///
    /// `Worker.Shutdown` is the supervisor's request; the operating system is its guarantee.
    /// A participant that does not stop within the budget is terminated, which is reported as
    /// such rather than as a clean stop.
    pub async fn reap(&mut self, worker_id: &Id, reason: &str) -> ReapOutcome {
        let Some(worker) = self.workers.get(worker_id) else {
            return ReapOutcome::AlreadyGone;
        };
        let service = worker.identity.service.clone();
        let incarnation = worker.service_incarnation.clone();
        let request_id = DomainRequestId::from_serial(self.next_serial());
        let params = ShutdownParams { reason: parse_id(reason).unwrap_or_else(|_| id("stop")) };
        let payload = object(
            SessionRpcRequest {
                request_id,
                scope: None,
                params: Value::Object(object(params.to_json())),
            }
            .to_json(),
        );
        let asked = tokio::time::timeout(
            self.health.probe,
            self.supervisor.call_and_wait(
                &service,
                Some(&incarnation),
                "Worker.Shutdown",
                payload,
                &[],
            ),
        )
        .await;
        let answered = matches!(asked, Ok(Ok(_)));
        let mut worker = self.workers.remove(worker_id).expect("just looked it up");
        self.budget.release(worker_id);
        worker.refresh_rss();
        match &mut worker.body {
            Body::Task(handle) => {
                match handle.take() {
                    Some(handle) => {
                        handle.stop().await;
                        if answered { ReapOutcome::Stopped } else { ReapOutcome::Terminated }
                    }
                    None => ReapOutcome::AlreadyGone,
                }
            }
            Body::Thread(thread) => {
                thread.stop();
                if answered { ReapOutcome::Stopped } else { ReapOutcome::Terminated }
            }
            Body::Process(child) => match child.take() {
                Some(mut child) => wait_or_terminate(&mut child, self.health.probe, answered),
                None => ReapOutcome::AlreadyGone,
            },
        }
    }

    /// Reaps every participant. Used at the end of a session and on every failure path.
    pub async fn reap_all(&mut self, reason: &str) -> Vec<(Id, ReapOutcome)> {
        let mut out = Vec::new();
        for worker_id in self.worker_ids() {
            let outcome = self.reap(&worker_id, reason).await;
            out.push((worker_id, outcome));
        }
        out
    }

    /// Ends one participant without asking it, as a crash would.
    ///
    /// This is the deliberate injection behind the worker-death row: the process is killed,
    /// the thread's runtime is dropped, or the serving task is aborted, and in every case the
    /// registration and every owner the connection held go with it.
    pub async fn kill(&mut self, worker_id: &Id) -> ReapOutcome {
        let Some(mut worker) = self.workers.remove(worker_id) else {
            return ReapOutcome::AlreadyGone;
        };
        self.budget.release(worker_id);
        worker.refresh_rss();
        match &mut worker.body {
            Body::Task(handle) => match handle.take() {
                Some(handle) => {
                    handle.stop().await;
                    ReapOutcome::Terminated
                }
                None => ReapOutcome::AlreadyGone,
            },
            Body::Thread(thread) => {
                thread.stop();
                ReapOutcome::Terminated
            }
            Body::Process(child) => match child.take() {
                Some(mut child) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    ReapOutcome::Terminated
                }
                None => ReapOutcome::AlreadyGone,
            },
        }
    }

    /// The peak resident set of every participant with a process of its own, in KiB, plus the
    /// coordinator's own.
    pub fn peak_rss_kib(&mut self) -> BTreeMap<String, u64> {
        let mut out = BTreeMap::new();
        out.insert(
            "coordinator".to_owned(),
            crate::metrics::peak_rss_kib().unwrap_or_default(),
        );
        for (worker_id, worker) in &mut self.workers {
            worker.refresh_rss();
            if worker.pid.is_some() {
                out.insert(worker_id.clone(), worker.peak_rss_kib);
            }
        }
        out
    }
}

impl Drop for Launcher {
    /// A launcher that goes away takes its participants with it. Leaving a child process
    /// behind would be a leak the supervisor is exactly responsible for not producing.
    fn drop(&mut self) {
        for worker in self.workers.values_mut() {
            terminate(worker);
        }
    }
}

fn terminate(worker: &mut LaunchedWorker) {
    match &mut worker.body {
        Body::Task(handle) => {
            if let Some(handle) = handle.take() {
                handle.abort();
            }
        }
        Body::Thread(thread) => thread.stop(),
        Body::Process(child) => {
            if let Some(mut child) = child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn wait_or_terminate(
    child: &mut std::process::Child,
    budget: Duration,
    answered: bool,
) -> ReapOutcome {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return if answered { ReapOutcome::Stopped } else { ReapOutcome::Terminated };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ReapOutcome::Terminated;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return ReapOutcome::AlreadyGone,
        }
    }
}

/// True for a refusal that means "the service this call reached is not the live one yet".
///
/// A replacement participant registers only once its predecessor's connection has finished
/// closing, so until then a call to that name either finds no route or reaches the route the
/// predecessor is still holding. Neither is an answer, and neither is adopted.
fn handover(e: &flybus::BusError) -> bool {
    matches!(
        e.code,
        flybus::ErrorCode::NoService
            | flybus::ErrorCode::CallGone
            | flybus::ErrorCode::TargetChanged
    )
}

fn unresponsive(worker_id: &Id) -> DomainError {
    DomainError::new(
        ErrorCode::BackendFailure,
        format!("{worker_id} did not answer within the supervisor's budget"),
        MutationCertainty::Unknown,
    )
}

fn launch_error(worker_id: &Id, e: &flybus::BusError) -> DomainError {
    let mutation = match e.dispatch {
        flybus::Dispatch::NotDispatched => MutationCertainty::None,
        flybus::Dispatch::Dispatched | flybus::Dispatch::Unknown => MutationCertainty::Unknown,
    };
    let code = match e.code {
        flybus::ErrorCode::NoService | flybus::ErrorCode::TargetChanged => {
            ErrorCode::IdentityMismatch
        }
        flybus::ErrorCode::NotAuthorized => ErrorCode::IdentityMismatch,
        flybus::ErrorCode::Backpressure | flybus::ErrorCode::QuotaExceeded => ErrorCode::Busy,
        _ => ErrorCode::BackendFailure,
    };
    DomainError::new(
        code,
        format!("{worker_id}: bus {:?}: {}", e.code, e.message),
        mutation,
    )
}

// -------------------------------------------------------------------------------------------
// What a participant is

/// The endpoint a launched participant serves.
#[derive(Clone, Debug)]
pub(crate) enum Started {
    Agent(AgentLaunch),
    Environment(EnvironmentLaunch),
}

impl Started {
    pub(crate) fn subcommand(&self) -> &'static str {
        match self {
            Started::Agent(_) => "agent",
            Started::Environment(_) => "environment",
        }
    }

    /// The arguments a separate process needs to be exactly this participant.
    fn arguments(&self) -> Vec<(String, String)> {
        match self {
            Started::Agent(spec) => {
                let mut args = vec![
                    ("--session".to_owned(), spec.session_id.clone()),
                    ("--agent".to_owned(), spec.agent_id.clone()),
                    ("--port".to_owned(), spec.port_id.clone()),
                    ("--incarnation".to_owned(), spec.incarnation_id.clone()),
                    ("--tick-numerator".to_owned(), spec.tick_duration.numerator.to_string()),
                    (
                        "--tick-denominator".to_owned(),
                        spec.tick_duration.denominator.to_string(),
                    ),
                    ("--warmup-ticks".to_owned(), spec.warmup_ticks.to_string()),
                    (
                        "--prepare-delay-ms".to_owned(),
                        spec.faults.prepare_delay_ms.to_string(),
                    ),
                    (
                        "--commit-delay-ms".to_owned(),
                        spec.faults.commit_delay_ms.to_string(),
                    ),
                ];
                if let Some(step) = spec.faults.fail_commit_at_step {
                    args.push(("--fail-commit-at-step".to_owned(), step.to_string()));
                }
                args
            }
            Started::Environment(spec) => {
                let mut args = vec![
                    ("--session".to_owned(), spec.session_id.clone()),
                    ("--worker".to_owned(), spec.worker_id.clone()),
                    ("--incarnation".to_owned(), spec.incarnation_id.clone()),
                    ("--step-numerator".to_owned(), spec.step_duration.numerator.to_string()),
                    (
                        "--step-denominator".to_owned(),
                        spec.step_duration.denominator.to_string(),
                    ),
                    ("--ports".to_owned(), spec.ports.join(",")),
                    (
                        "--advance-delay-ms".to_owned(),
                        spec.faults.advance_delay_ms.to_string(),
                    ),
                ];
                if let Some(boundary) = spec.faults.omit_view_at_boundary {
                    args.push(("--omit-view-at-boundary".to_owned(), boundary.to_string()));
                }
                args
            }
        }
    }
}

pub(crate) fn agent_config(spec: &AgentLaunch, worker_threads: usize) -> AgentConfig {
    AgentConfig {
        session_id: spec.session_id.clone(),
        agent_id: spec.agent_id.clone(),
        incarnation_id: spec.incarnation_id.clone(),
        tick_duration: spec.tick_duration,
        warmup_ticks: spec.warmup_ticks,
        worker_threads,
        faults: spec.faults.clone(),
    }
}

pub(crate) fn environment_config(spec: &EnvironmentLaunch) -> EnvironmentConfig {
    EnvironmentConfig {
        session_id: spec.session_id.clone(),
        worker_id: spec.worker_id.clone(),
        incarnation_id: spec.incarnation_id.clone(),
        step_duration: spec.step_duration,
        ports: spec.ports.clone(),
        faults: spec.faults.clone(),
    }
}

/// Connects, registers and serves one participant. Used by a thread with its own runtime and,
/// through the worker subcommand, by a separate process.
pub(crate) async fn serve_one(
    socket: &Path,
    client_id: &str,
    service_name: &str,
    store_root: &Path,
    what: &Started,
    worker_threads: usize,
) -> Result<WorkerHandle, String> {
    let client = Client::connect_unix(socket, ClientConfig::new(client_id, store_root))
        .await
        .map_err(|e| format!("connect: {}", e.message))?;
    let service = register_with_retry(&client, service_name, Duration::from_secs(30))
        .await
        .map_err(|e| format!("register: {}", e.message))?;
    Ok(match what {
        Started::Agent(spec) => serve(
            client,
            service,
            FakeAgentWorker::new(agent_config(spec, worker_threads)),
        ),
        Started::Environment(spec) => {
            serve(client, service, CounterEnvironment::new(environment_config(spec)))
        }
    })
}

/// Registers one exclusive service name, waiting out a predecessor that is still letting go.
///
/// Registration is exclusive, so a replacement worker meets `CONFLICT` until the connection it
/// replaces has finished closing and the router has released its routes. That is a handover,
/// not a refusal, so it is waited out; every other refusal is returned as it stands.
pub(crate) async fn register_with_retry(
    client: &Client,
    name: &str,
    budget: Duration,
) -> Result<flybus::Service, flybus::BusError> {
    let deadline = Instant::now() + budget;
    loop {
        match client.register(name, ServiceConfig::default()).await {
            Ok(service) => return Ok(service),
            Err(e) if e.code == flybus::ErrorCode::Conflict && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(e) => return Err(e),
        }
    }
}
