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
use crate::media::{RenderCounter, SensorLog};
use crate::launcher::{
    AgentLaunch, EnvironmentLaunch, Launcher, ReapOutcome, SUPERVISOR_CLIENT, ThreadBudget,
};
use crate::state::{CheckpointStore, CheckpointWriter, StoreConfig, StoreFaults, WriterConfig, WriterFaults};
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
    /// Which graph this fly builds. A replacement worker started on another variant has the
    /// same neuron count and another `indexDigest`, which is the composition change a
    /// descriptor revision exists to make visible.
    pub graph_variant: u64,
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
            graph_variant: 0,
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
    /// The view's declared render delay, in steps. Zero is same-boundary output.
    pub observation_delay_steps: u64,
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
    /// How many committed generations the durable checkpoint store keeps.
    pub store: StoreConfig,
    /// The durable write faults this composition injects.
    pub store_faults: StoreFaults,
    /// The checkpoint queue's bounds.
    pub writer: WriterConfig,
    /// The writer faults this composition injects.
    pub writer_faults: WriterFaults,
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
            observation_delay_steps: 0,
            environment_faults: EnvironmentFaults::default(),
            mode: ExecutionMode::InProcess,
            thread_budget: None,
            coordinator_threads: 1,
            environment_threads: 1,
            store: StoreConfig::default(),
            store_faults: StoreFaults::default(),
            writer: WriterConfig::default(),
            writer_faults: WriterFaults::default(),
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
/// The checkpoint writer's own bus identity. It publishes checkpoint events and nothing else.
const WRITER_CLIENT: &str = "checkpoint-writer";

/// How many times one participant may be replaced in a composition.
///
/// Each replacement connects under its own client id, so a restart is visibly a new
/// participant rather than a silent reattachment, and the policy has to name them all.
const MAX_GENERATIONS: u32 = 8;

fn agent_service(agent_id: &Id) -> String {
    format!("agent.{agent_id}")
}

fn agent_client(agent_id: &Id) -> String {
    format!("worker-{agent_id}")
}

/// What a presentation consumer may do: subscribe, and ask the read-only repair service.
fn consumer_grants() -> Grants {
    grants(|g| {
        g.subscribe = vec![Pattern::prefix("session."), Pattern::prefix("app.")];
        g.call = vec![Pattern::prefix("session.")];
    })
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
    /// The media instrumentation of the participants that live in this process. Both are
    /// shared memory, so both are empty for a participant with a process of its own; the
    /// accessors below return `None` there rather than zero.
    renders: RenderCounter,
    sensors: BTreeMap<Id, SensorLog>,
    /// The supervisor. It owns every participant's lifetime and thread allocation.
    pub launcher: Launcher,
    observers: Mutex<Vec<Client>>,
    /// Which configured observer identity the next consumer takes.
    next_observer: std::sync::atomic::AtomicUsize,
    /// Which generation of each participant is running: 1 is the one the composition started.
    generations: BTreeMap<Id, u32>,
    /// Where the durable checkpoint store lives, for a test that reads the files themselves.
    checkpoint_root: std::path::PathBuf,
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
                    // The read-only repair service of publishing-v1 section 2. It is the
                    // session's own address and answers two queries; naming it is not
                    // authority over anything, and no method on it mutates.
                    g.register = vec![Pattern::prefix("session.")];
                }),
            )
            .client(
                SUPERVISOR_CLIENT,
                grants(|g| {
                    g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")];
                }),
            )
            // The writer publishes the checkpoint events and never calls a participant.
            .client(WRITER_CLIENT, grants(|g| g.publish = vec![Pattern::prefix("session.")]))
            .client(ENV_CLIENT, grants(|g| g.register = vec![Pattern::exact(ENV_SERVICE)]))
            // A presentation consumer subscribes and may call the repair service. It can
            // publish nothing, register nothing and reach no worker: "viewers/browser clients
            // never obtain worker control" (publishing-v1 section 7). A bus client id is one
            // connection, so a composition with several consumers configures several of them;
            // they are the same grants, because a second viewer is not a more privileged one.
            .client("observer", consumer_grants())
            .client("observer-2", consumer_grants())
            .client("observer-3", consumer_grants())
            .client("observer-4", consumer_grants())
            // The application's own publisher. Its addresses are its own, and it has no
            // reach into the session's.
            .client(
                "application",
                grants(|g| {
                    g.publish = vec![Pattern::prefix("app.")];
                    g.manage_topics = vec![Pattern::prefix("app.")];
                    g.subscribe = vec![Pattern::prefix("session.")];
                }),
            )
            // The publication boundary, when a composition places it on a client of its own
            // rather than on the coordinator's.
            .client(
                "publisher",
                grants(|g| {
                    g.publish = vec![Pattern::prefix("session.")];
                    g.manage_topics = vec![Pattern::prefix("session.")];
                }),
            );
        // A replacement environment connects under its own client id, one per generation;
        // this subsumes the single `-r2` identity the publication slice had configured.
        for generation in 2..=MAX_GENERATIONS {
            policy = policy.client(
                &format!("{ENV_CLIENT}-r{generation}"),
                grants(|g| g.register = vec![Pattern::exact(ENV_SERVICE)]),
            );
        }
        for spec in &config.agents {
            let service = agent_service(&spec.agent_id);
            policy = policy.client(
                &agent_client(&spec.agent_id),
                grants(|g| g.register = vec![Pattern::exact(&service)]),
            );
            // A replacement worker connects under its own client id, so a restart is visibly a
            // new participant rather than a silent reattachment to the active epoch.
            for generation in 2..=MAX_GENERATIONS {
                policy = policy.client(
                    &format!("{}-r{generation}", agent_client(&spec.agent_id)),
                    grants(|g| g.register = vec![Pattern::exact(&service)]),
                );
            }
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
        let renders = RenderCounter::new();
        let sensors: BTreeMap<Id, SensorLog> = config
            .agents
            .iter()
            .map(|spec| (spec.agent_id.clone(), SensorLog::new()))
            .collect();

        // The environment first: it owns the world and the descriptor.
        let environment = launcher
            .launch_environment(EnvironmentLaunch {
                session_id: config.session_id.clone(),
                worker_id: id(ENV_WORKER),
                incarnation_id: id("arena-inc-1"),
                step_duration,
                ports: config.agents.iter().map(|a| a.port_id.clone()).collect(),
                worker_threads: config.environment_threads,
                observation_delay_steps: config.observation_delay_steps,
                renders: renders.clone(),
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
                    graph_variant: spec.graph_variant,
                    sensors: sensors[&spec.agent_id].clone(),
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
        // The durable store lives beside the router's artifact store and never inside it: a
        // committed generation is outside the bus's ephemeral collection.
        let checkpoint_root = root.join("checkpoints");
        let mut store = CheckpointStore::open(&checkpoint_root, config.store).map_err(refusal)?;
        *store.faults_mut() = config.store_faults.clone();
        let writer_client = launcher.connect(WRITER_CLIENT).await?;
        let writer = CheckpointWriter::start(
            store,
            config.writer,
            config.writer_faults.clone(),
            Some((
                writer_client,
                format!("session.{}.checkpoints", config.session_id),
            )),
        );
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
        let mut coordinator = coordinator;
        coordinator.attach_store(writer);

        Ok(SessionHarness {
            coordinator,
            config,
            via,
            mode: launcher.mode(),
            renders,
            sensors,
            launcher,
            observers: Mutex::new(Vec::new()),
            next_observer: std::sync::atomic::AtomicUsize::new(0),
            generations: BTreeMap::new(),
            checkpoint_root,
        })
    }

    /// Where the durable checkpoint store's generations and store manifest live.
    pub fn checkpoint_root(&self) -> &std::path::Path {
        &self.checkpoint_root
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
    ///
    /// Each call takes the next configured observer identity: one bus client id is one
    /// connection, so two consumers are two configured participants and not one identity
    /// used twice.
    pub async fn observer(&self) -> Result<Client, flybus::BusError> {
        let index = self
            .next_observer
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let id = match index {
            0 => "observer".to_owned(),
            n => format!("observer-{}", n + 1),
        };
        let client = self.launcher.connect(&id).await?;
        self.observers.lock().expect("not poisoned").push(client.clone());
        Ok(client)
    }

    /// A client for the application that owns its own state and cues.
    pub async fn application(&self) -> Result<Client, flybus::BusError> {
        let client = self.launcher.connect("application").await?;
        self.observers.lock().expect("not poisoned").push(client.clone());
        Ok(client)
    }

    /// A client for a publication boundary of its own.
    pub async fn publisher(&self) -> Result<Client, flybus::BusError> {
        let client = self.launcher.connect("publisher").await?;
        self.observers.lock().expect("not poisoned").push(client.clone());
        Ok(client)
    }

    /// A fake multi-agent presentation consumer attached to this session's topics.
    pub async fn consumer(&self) -> Result<crate::publish::PresentationConsumer, flybus::BusError> {
        let client = self.observer().await?;
        crate::publish::PresentationConsumer::attach(
            client,
            &self.config.session_id,
            self.coordinator.topics(),
        )
        .await
    }

    /// Replaces one agent's worker with a fresh incarnation, as a restore would.
    ///
    /// The coordinator still pins the old registration, so its next call to that agent fails
    /// rather than silently reaching another brain.
    pub async fn restart_agent(&mut self, agent_id: &Id) -> Result<Restarted, flybus::BusError> {
        self.restart_agent_on_graph(agent_id, None).await
    }

    /// Replaces one agent's worker, optionally with a fly that built another graph.
    ///
    /// `Some(variant)` is the composition change a descriptor revision exists for: the same
    /// neuron count, another `indexDigest`.
    pub async fn restart_agent_on_graph(
        &mut self,
        agent_id: &Id,
        graph_variant: Option<u64>,
    ) -> Result<Restarted, flybus::BusError> {
        let spec = self
            .config
            .agents
            .iter()
            .find(|spec| spec.agent_id == *agent_id)
            .expect("a configured agent")
            .clone();
        let generation = self.next_generation(agent_id)?;
        self.launcher.kill(agent_id).await;
        let tick_duration = millis(self.config.tick_ms).expect("a positive tick");
        let incarnation_id = parse_id(&format!("{agent_id}-inc-{generation}"))
            .expect("an agent id plus a suffix is an Id");
        self.launcher
            .launch_agent(AgentLaunch {
                session_id: self.config.session_id.clone(),
                agent_id: agent_id.clone(),
                port_id: spec.port_id.clone(),
                incarnation_id: incarnation_id.clone(),
                tick_duration,
                warmup_ticks: self.config.warmup_ticks,
                worker_threads: spec.worker_threads,
                graph_variant: graph_variant.unwrap_or(spec.graph_variant),
                // The same log: a replacement worker in this process keeps writing where its
                // predecessor wrote, so a restore's sensory input is visible beside it.
                sensors: self.sensors.get(agent_id).cloned().unwrap_or_default(),
                faults: spec.faults.clone(),
                client_id: format!("{}-r{generation}", agent_client(agent_id)),
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

    /// Replaces the environment with a fresh, uninitialized incarnation, as a restore needs.
    pub async fn restart_environment(&mut self) -> Result<Restarted, flybus::BusError> {
        let worker_id = id(ENV_WORKER);
        let generation = self.next_generation(&worker_id)?;
        self.launcher.kill(&worker_id).await;
        let step_duration = hz(self.config.step_hz).expect("a positive cadence");
        let incarnation_id = parse_id(&format!("arena-inc-{generation}"))
            .expect("a worker id plus a suffix is an Id");
        self.launcher
            .launch_environment(EnvironmentLaunch {
                session_id: self.config.session_id.clone(),
                worker_id: worker_id.clone(),
                incarnation_id: incarnation_id.clone(),
                step_duration,
                ports: self.config.agents.iter().map(|a| a.port_id.clone()).collect(),
                worker_threads: self.config.environment_threads,
                observation_delay_steps: self.config.observation_delay_steps,
                renders: self.renders.clone(),
                faults: self.config.environment_faults.clone(),
                client_id: format!("{ENV_CLIENT}-r{generation}"),
                service: ENV_SERVICE.to_owned(),
            })
            .await
            .map_err(refusal)?;
        let worker = self.launcher.worker(&worker_id).expect("just launched");
        Ok(Restarted {
            service: worker.identity.service.clone(),
            service_incarnation: worker.service_incarnation.clone(),
            incarnation_id,
        })
    }

    fn next_generation(&mut self, worker_id: &Id) -> Result<u32, flybus::BusError> {
        let slot = self.generations.entry(worker_id.clone()).or_insert(1);
        if *slot >= MAX_GENERATIONS {
            return Err(flybus::BusError::new(
                flybus::ErrorCode::QuotaExceeded,
                format!(
                    "{worker_id} has used all {MAX_GENERATIONS} configured client identities; a composition declares how many replacements it allows"
                ),
            ));
        }
        *slot += 1;
        Ok(*slot)
    }

    /// Replaces every participant and points the fenced coordinator at the replacements.
    ///
    /// This is what a recovery does before it restores: the old participants belong to an
    /// invalid epoch, and the references the coordinator pinned are exchanged deliberately.
    pub async fn replace_all_participants(&mut self) -> Result<(), flybus::BusError> {
        let environment = self.environment_id();
        self.restart_environment().await?;
        let worker = self
            .launcher
            .worker(&environment)
            .expect("just launched")
            .worker_ref();
        self.coordinator
            .replace_participant(&environment, worker)
            .map_err(|e| refusal(e.error))?;
        for agent_id in self.config.agents.iter().map(|a| a.agent_id.clone()).collect::<Vec<_>>() {
            self.restart_agent(&agent_id).await?;
            let worker = self
                .launcher
                .worker(&agent_id)
                .expect("just launched")
                .worker_ref();
            self.coordinator
                .replace_participant(&agent_id, worker)
                .map_err(|e| refusal(e.error))?;
        }
        Ok(())
    }

    /// Changes one agent's injected faults, so the replacement the next restart launches is
    /// a participant without them.
    ///
    /// A fault is launch configuration, so clearing one is a relaunch and not a live change:
    /// the worker running now keeps whatever it was started with.
    pub fn set_agent_faults(&mut self, agent_id: &Id, faults: AgentFaults) {
        if let Some(spec) = self
            .config
            .agents
            .iter_mut()
            .find(|spec| spec.agent_id == *agent_id)
        {
            spec.faults = faults;
        }
    }

    /// Changes which graph one agent builds, so the replacement the next restart launches is
    /// a fly with the same neuron count and another index.
    ///
    /// The same relaunch rule as a fault: the worker running now keeps what it was started
    /// with, and the change reaches the composition through the next replacement.
    pub fn set_agent_graph(&mut self, agent_id: &Id, graph_variant: u64) {
        if let Some(spec) = self
            .config
            .agents
            .iter_mut()
            .find(|spec| spec.agent_id == *agent_id)
        {
            spec.graph_variant = graph_variant;
        }
    }

    /// Changes the environment's injected faults, with the same relaunch rule.
    pub fn set_environment_faults(&mut self, faults: EnvironmentFaults) {
        self.config.environment_faults = faults;
    }

    /// Ends one participant without asking it, as a crash would.
    pub async fn kill(&mut self, worker_id: &Id) -> ReapOutcome {
        self.launcher.kill(worker_id).await
    }

    /// The worker id the environment answers to.
    pub fn environment_id(&self) -> Id {
        id(ENV_WORKER)
    }

    /// What one agent read out of its sensory attachments, in order, when this process is
    /// where that log lives.
    ///
    /// `None` means "not observable from here", not "nothing was read": an agent with a
    /// process of its own records into its own copy. The media path itself crosses a process
    /// boundary -- the frame is one artifact in the shared store, reached through owned
    /// handles -- but this instrumentation does not, because it is shared memory.
    pub fn sensor_log(&self, agent_id: &Id) -> Option<SensorLog> {
        match self.mode {
            ExecutionMode::Process => None,
            _ => self.sensors.get(agent_id).cloned(),
        }
    }

    /// How many native frames the environment rendered, when the world lives in this process.
    ///
    /// `None` for a world with a process of its own, for the same reason as above.
    pub fn renders(&self) -> Option<u64> {
        match self.mode {
            ExecutionMode::Process => None,
            _ => Some(self.renders.count()),
        }
    }

    /// The agent worker's progress counter, which is its fake model's mutation count, when
    /// this process is where that counter lives.
    ///
    /// `None` means "not observable from here", not "nothing happened": a participant with a
    /// process of its own keeps its counter there. [`SessionHarness::progress_of`] reads it
    /// over the bus and works in every mode.
    pub fn agent_mutations(&self, agent_id: &Id) -> Option<u64> {
        self.launcher
            .worker(agent_id)
            .and_then(crate::launcher::LaunchedWorker::progress_counter)
    }

    pub fn environment_mutations(&self) -> Option<u64> {
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
        let SessionHarness { mut coordinator, mut launcher, observers, .. } = self;
        // The writer task owns artifact handles and a blocking store. Leaving it running
        // would leave both behind.
        coordinator.shutdown_store().await;
        drop(coordinator);
        launcher.reap_all(&id("shutdown")).await;
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
