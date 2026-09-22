//! The runnable synthetic composition: one router, the configured flies, one counter arena and
//! one coordinator, in whichever execution mode the composition asks for.
//!
//! All participants use router semantics even when colocated, so every mode and both
//! transports exercise the same code. The caller owns the store root directory, which keeps
//! this module free of a temporary-directory dependency.
//!
//! The three execution modes are the SESSION-02 comparison:
//!
//! | Mode | Where each participant runs | Transport |
//! | --- | --- | --- |
//! | [`ExecutionMode::InProcess`] | A task on the coordinator's runtime | either |
//! | [`ExecutionMode::Thread`] | Its own OS thread and runtime | Unix socket |
//! | [`ExecutionMode::Process`] | Its own process, one per fly plus one world | Unix socket |

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use flybus::{Client, Grants, Pattern, Policy, Router, RouterConfig};

use crate::agent::{AgentFaults, synthetic_profile};
use crate::coordinator::{AgentSlot, Coordinator};
use crate::environment::EnvironmentFaults;
use crate::launcher::{
    AgentLaunch, EnvironmentLaunch, Launcher, ReapOutcome, SUPERVISOR_CLIENT, ThreadBudget,
};
use crate::task::{ActionExecutor, CounterTask, IdentityExecutor, Terminal};
// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;
use crate::worker::StatusCell;

pub use crate::launcher::{ExecutionMode, Via};

/// One agent in the composition.
#[derive(Clone, Debug)]
pub struct AgentSpec {
    pub agent_id: Id,
    pub port_id: Id,
    /// An explicit seed. The first synthetic composition supports hand-selected seeds; the
    /// derivation algorithm is specified before the real agent slice.
    pub seed: i32,
    pub faults: AgentFaults,
    /// The threads this agent asks the launcher for. `Agent.Initialize` carries exactly what
    /// the launcher allocated, which `workers-v1` requires it to lie within.
    pub worker_threads: usize,
}

impl AgentSpec {
    /// One agent on one thread, with no injected fault.
    pub fn new(agent_id: &str, port_id: &str, seed: i32) -> AgentSpec {
        AgentSpec {
            agent_id: id(agent_id),
            port_id: id(port_id),
            seed,
            faults: AgentFaults::default(),
            worker_threads: 1,
        }
    }
}

/// The composition the harness builds.
#[derive(Clone, Debug)]
pub struct HarnessConfig {
    pub session_id: Id,
    pub epoch: Id,
    pub episode_id: Id,
    pub agents: Vec<AgentSpec>,
    /// The world's cadence. 60 Hz with a 1 ms tick is the `step-v1` section 5 example.
    pub step_hz: u64,
    pub tick_ms: u64,
    pub warmup_ticks: u64,
    pub terminal: Terminal,
    pub environment_faults: EnvironmentFaults,
    /// Where each participant runs.
    pub mode: ExecutionMode,
    /// The total thread allocation the launcher may hand out. `None` sizes it from the
    /// composition and the machine, which is what an ordinary run wants; a test that means to
    /// exhaust the budget names a number.
    pub thread_budget: Option<usize>,
    /// The threads reserved for the coordinator, its router and its store.
    pub coordinator_threads: usize,
    pub environment_threads: usize,
}

impl Default for HarnessConfig {
    fn default() -> HarnessConfig {
        HarnessConfig {
            session_id: id("demo"),
            epoch: id("e1"),
            episode_id: id("ep1"),
            agents: vec![
                AgentSpec { seed: 7, ..AgentSpec::new("fly-a", "p1", 7) },
                AgentSpec { seed: 11, ..AgentSpec::new("fly-b", "p2", 11) },
            ],
            step_hz: 60,
            tick_ms: 1,
            warmup_ticks: 10,
            terminal: Terminal::Never,
            environment_faults: EnvironmentFaults::default(),
            mode: ExecutionMode::InProcess,
            thread_budget: None,
            coordinator_threads: 1,
            environment_threads: 1,
        }
    }
}

impl HarnessConfig {
    /// The threads this composition needs at a minimum: the coordinator, the world and every
    /// agent's own allocation.
    pub fn required_threads(&self) -> usize {
        self.coordinator_threads
            + self.environment_threads
            + self.agents.iter().map(|a| a.worker_threads).sum::<usize>()
    }

    /// The budget the launcher runs under: what was configured, or a budget that covers both
    /// this composition and this machine's physical cores.
    pub fn budget(&self) -> Result<ThreadBudget, DomainError> {
        let total = self
            .thread_budget
            .unwrap_or_else(|| crate::metrics::physical_cores().max(self.required_threads()));
        ThreadBudget::new(total, self.coordinator_threads)
    }
}

const ENV_SERVICE: &str = "env.arena";
const ENV_CLIENT: &str = "environment";
const ENV_WORKER: &str = "arena";
const COORDINATOR_CLIENT: &str = "coordinator";

fn agent_service(agent_id: &Id) -> String {
    format!("agent.{agent_id}")
}

fn agent_client(agent_id: &Id) -> String {
    format!("worker-{agent_id}")
}

fn grants(f: impl FnOnce(&mut Grants)) -> Grants {
    let mut g = Grants::default();
    f(&mut g);
    g
}

/// What a restarted worker looks like from the outside: a new registration and a new
/// incarnation, both different from the ones the coordinator pinned.
#[derive(Clone, Debug)]
pub struct Restarted {
    pub service: String,
    pub service_incarnation: String,
    pub incarnation_id: Id,
}

/// A running synthetic session.
pub struct SessionHarness {
    pub coordinator: Coordinator,
    pub config: HarnessConfig,
    pub via: Via,
    pub mode: ExecutionMode,
    /// The supervisor. It owns every participant's lifetime and thread allocation.
    pub launcher: Launcher,
    observers: Mutex<Vec<Client>>,
}

impl SessionHarness {
    /// Builds the router, launches the workers and builds the coordinator. Nothing has
    /// stepped yet.
    pub async fn start(
        via: Via,
        root: &Path,
        config: HarnessConfig,
    ) -> Result<SessionHarness, flybus::BusError> {
        let store_root = root.join("store");
        let sockets = root.join("sockets");
        std::fs::create_dir_all(&sockets).expect("the caller owns a writable directory");

        // The launcher's policy: who may connect, and what each may do. Naming a target is not
        // authority to use it, so the supervisor calls but never registers or publishes, and a
        // worker registers exactly one service and calls nothing.
        let mut policy = Policy::closed()
            .client(
                COORDINATOR_CLIENT,
                grants(|g| {
                    g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")];
                    g.publish = vec![Pattern::prefix("session.")];
                    g.manage_topics = vec![Pattern::prefix("session.")];
                }),
            )
            .client(
                SUPERVISOR_CLIENT,
                grants(|g| {
                    g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")];
                }),
            )
            .client(ENV_CLIENT, grants(|g| g.register = vec![Pattern::exact(ENV_SERVICE)]))
            .client(
                &format!("{ENV_CLIENT}-r2"),
                grants(|g| g.register = vec![Pattern::exact(ENV_SERVICE)]),
            )
            .client("observer", grants(|g| g.subscribe = vec![Pattern::prefix("session.")]));
        for spec in &config.agents {
            let service = agent_service(&spec.agent_id);
            policy = policy.client(
                &agent_client(&spec.agent_id),
                grants(|g| g.register = vec![Pattern::exact(&service)]),
            );
            // A replacement worker connects under its own client id, so a restart is visibly a
            // new participant rather than a silent reattachment to the active epoch.
            policy = policy.client(
                &format!("{}-r2", agent_client(&spec.agent_id)),
                grants(|g| g.register = vec![Pattern::exact(&service)]),
            );
        }
        let mut router_config = RouterConfig::new(&store_root);
        router_config.policy = policy;
        let router = Router::new(router_config).map_err(|e| {
            flybus::BusError::new(flybus::ErrorCode::StoreFailure, format!("router: {e}"))
        })?;

        let budget = config.budget().map_err(refusal)?;
        let mut launcher =
            Launcher::start(router, config.mode, via, &store_root, &sockets, budget).await?;

        let step_duration = hz(config.step_hz).expect("a positive cadence");
        let tick_duration = millis(config.tick_ms).expect("a positive tick");

        // The environment first: it owns the world and the descriptor.
        let environment = launcher
            .launch_environment(EnvironmentLaunch {
                session_id: config.session_id.clone(),
                worker_id: id(ENV_WORKER),
                incarnation_id: id("arena-inc-1"),
                step_duration,
                ports: config.agents.iter().map(|a| a.port_id.clone()).collect(),
                worker_threads: config.environment_threads,
                faults: config.environment_faults.clone(),
                client_id: ENV_CLIENT.to_owned(),
                service: ENV_SERVICE.to_owned(),
            })
            .await
            .map_err(refusal)?;
        let environment_ref = launcher
            .worker(&environment.worker_id)
            .expect("just launched")
            .worker_ref();

        let mut slots = Vec::new();
        for spec in &config.agents {
            let identity = launcher
                .launch_agent(AgentLaunch {
                    session_id: config.session_id.clone(),
                    agent_id: spec.agent_id.clone(),
                    port_id: spec.port_id.clone(),
                    incarnation_id: parse_id(&format!("{}-inc-1", spec.agent_id))
                        .expect("an agent id plus a suffix is an Id"),
                    tick_duration,
                    warmup_ticks: config.warmup_ticks,
                    worker_threads: spec.worker_threads,
                    faults: spec.faults.clone(),
                    client_id: agent_client(&spec.agent_id),
                    service: agent_service(&spec.agent_id),
                })
                .await
                .map_err(refusal)?;
            let worker_ref = launcher
                .worker(&spec.agent_id)
                .expect("just launched")
                .worker_ref();
            let mut slot = AgentSlot::new(
                worker_ref,
                spec.agent_id.clone(),
                spec.port_id.clone(),
                synthetic_profile(&spec.agent_id, &tick_duration, config.warmup_ticks),
                spec.seed,
            );
            slot.worker_threads = identity.worker_threads as u64;
            slots.push(slot);
        }

        let coordinator_client = launcher.connect(COORDINATOR_CLIENT).await?;
        let executors: BTreeMap<Id, Box<dyn ActionExecutor>> = config
            .agents
            .iter()
            .map(|spec| {
                (spec.agent_id.clone(), Box::new(IdentityExecutor) as Box<dyn ActionExecutor>)
            })
            .collect();
        let coordinator = Coordinator::new(
            coordinator_client,
            config.session_id.clone(),
            config.epoch.clone(),
            config.episode_id.clone(),
            environment_ref,
            slots,
            Box::new(CounterTask::new(&config.epoch, config.terminal)),
            executors,
        );

        Ok(SessionHarness {
            coordinator,
            config,
            via,
            mode: launcher.mode(),
            launcher,
            observers: Mutex::new(Vec::new()),
        })
    }

    pub fn router(&self) -> &Router {
        self.launcher.router()
    }

    /// The coordinator and its supervisor, borrowed apart.
    ///
    /// A supervisor acts while a transition is in flight -- that is what a supervisor is for
    /// -- so the two have to be reachable at the same time.
    pub fn parts(&mut self) -> (&mut Coordinator, &mut Launcher) {
        (&mut self.coordinator, &mut self.launcher)
    }

    /// The router's own counters: owners, roots, queued messages and store bytes.
    pub fn router_stats(&self) -> flybus::RouterStats {
        self.launcher.router().stats()
    }

    /// A client for `id`, connected the same way every participant is.
    ///
    /// An unconfigured client id is refused by the launcher's policy before it can route, so
    /// this is not a way around the composition.
    pub async fn client(&self, id: &str) -> Result<Client, flybus::BusError> {
        self.launcher.connect(id).await
    }

    /// An extra subscriber, for a test that watches the published boundaries.
    pub async fn observer(&self) -> Result<Client, flybus::BusError> {
        let client = self.launcher.connect("observer").await?;
        self.observers.lock().expect("not poisoned").push(client.clone());
        Ok(client)
    }

    /// Replaces one agent's worker with a fresh incarnation, as a restore would.
    ///
    /// The coordinator still pins the old registration, so its next call to that agent fails
    /// rather than silently reaching another brain.
    pub async fn restart_agent(&mut self, agent_id: &Id) -> Result<Restarted, flybus::BusError> {
        let spec = self
            .config
            .agents
            .iter()
            .find(|spec| spec.agent_id == *agent_id)
            .expect("a configured agent")
            .clone();
        self.launcher.kill(agent_id).await;
        let tick_duration = millis(self.config.tick_ms).expect("a positive tick");
        let incarnation_id =
            parse_id(&format!("{agent_id}-inc-2")).expect("an agent id plus a suffix is an Id");
        self.launcher
            .launch_agent(AgentLaunch {
                session_id: self.config.session_id.clone(),
                agent_id: agent_id.clone(),
                port_id: spec.port_id.clone(),
                incarnation_id: incarnation_id.clone(),
                tick_duration,
                warmup_ticks: self.config.warmup_ticks,
                worker_threads: spec.worker_threads,
                faults: spec.faults.clone(),
                client_id: format!("{}-r2", agent_client(agent_id)),
                service: agent_service(agent_id),
            })
            .await
            .map_err(refusal)?;
        let worker = self.launcher.worker(agent_id).expect("just launched");
        Ok(Restarted {
            service: worker.identity.service.clone(),
            service_incarnation: worker.service_incarnation.clone(),
            incarnation_id,
        })
    }

    /// Ends one participant without asking it, as a crash would.
    pub async fn kill(&mut self, worker_id: &Id) -> ReapOutcome {
        self.launcher.kill(worker_id).await
    }

    /// The worker id the environment answers to.
    pub fn environment_id(&self) -> Id {
        id(ENV_WORKER)
    }

    /// The agent worker's progress counter, which is its fake model's mutation count.
    ///
    /// A participant in another process keeps its counter there; use
    /// [`SessionHarness::progress_of`], which reads it over the bus in every mode.
    pub fn agent_mutations(&self, agent_id: &Id) -> u64 {
        self.launcher
            .worker(agent_id)
            .and_then(crate::launcher::LaunchedWorker::progress_counter)
            .unwrap_or_default()
    }

    pub fn environment_mutations(&self) -> u64 {
        self.agent_mutations(&id(ENV_WORKER))
    }

    /// One participant's progress counter, read over the bus. Works in every execution mode.
    pub async fn progress_of(&mut self, worker_id: &Id) -> Result<u64, DomainError> {
        Ok(self.launcher.health_check(worker_id).await?.progress_counter)
    }

    /// The local status cell of a participant in this process, or `None` for one with a
    /// process of its own.
    pub fn agent_status(&self, agent_id: &Id) -> Option<StatusCell> {
        self.launcher.worker(agent_id).and_then(crate::launcher::LaunchedWorker::status)
    }

    /// Reaps every participant and closes the router.
    pub async fn shutdown(self) {
        let SessionHarness { coordinator, mut launcher, observers, .. } = self;
        drop(coordinator);
        launcher.reap_all("shutdown").await;
        for observer in observers.into_inner().expect("not poisoned") {
            observer.close().await;
        }
        launcher.router().shutdown();
    }
}

/// A launcher refusal, as a bus error: the harness's one error type stays the bus's.
fn refusal(e: DomainError) -> flybus::BusError {
    let code = match e.code {
        ErrorCode::Busy => flybus::ErrorCode::QuotaExceeded,
        ErrorCode::IdentityMismatch => flybus::ErrorCode::NotAuthorized,
        _ => flybus::ErrorCode::RouterLost,
    };
    flybus::BusError::new(code, e.to_string())
}
