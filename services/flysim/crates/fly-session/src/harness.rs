//! The runnable synthetic composition: one router, two fake agents, one counter arena and one
//! coordinator, over either transport.
//!
//! All participants use router semantics even when colocated, so the in-memory and
//! Unix-socket runs exercise the same code. The caller owns the store root directory, which
//! keeps this module free of a temporary-directory dependency.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use flybus::{
    Client, ClientConfig, Grants, Pattern, Policy, Router, RouterConfig, ServiceConfig, Transport,
    UnixListenerHandle,
};

use crate::agent::{AgentConfig, AgentFaults, FakeAgentWorker, synthetic_profile};
use crate::coordinator::{AgentSlot, Coordinator};
use crate::environment::{CounterEnvironment, EnvironmentConfig, EnvironmentFaults};
use crate::media::{RenderCounter, SensorLog};
use crate::rpc::WorkerRef;
use crate::task::{ActionExecutor, CounterTask, IdentityExecutor, Terminal};
// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;
use crate::worker::{StatusCell, WorkerHandle, serve};

/// Which transport the session runs over. Both must produce the same behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Memory,
    Unix,
}

/// One agent in the composition.
#[derive(Clone, Debug)]
pub struct AgentSpec {
    pub agent_id: Id,
    pub port_id: Id,
    /// An explicit seed. The first synthetic composition supports hand-selected seeds; the
    /// derivation algorithm is specified before the real agent slice.
    pub seed: i32,
    pub faults: AgentFaults,
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
}

impl Default for HarnessConfig {
    fn default() -> HarnessConfig {
        HarnessConfig {
            session_id: id("demo"),
            epoch: id("e1"),
            episode_id: id("ep1"),
            agents: vec![
                AgentSpec {
                    agent_id: id("fly-a"),
                    port_id: id("p1"),
                    seed: 7,
                    faults: AgentFaults::default(),
                },
                AgentSpec {
                    agent_id: id("fly-b"),
                    port_id: id("p2"),
                    seed: 11,
                    faults: AgentFaults::default(),
                },
            ],
            step_hz: 60,
            tick_ms: 1,
            warmup_ticks: 10,
            terminal: Terminal::Never,
            observation_delay_steps: 0,
            environment_faults: EnvironmentFaults::default(),
        }
    }
}

const ENV_SERVICE: &str = "env.arena";
const ENV_CLIENT: &str = "environment";
const ENV_WORKER: &str = "arena";

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

/// Makes a connection for one launcher-bound participant, over the chosen transport.
struct Connector {
    router: Router,
    via: Via,
    store_root: PathBuf,
    sockets: PathBuf,
    next_socket: AtomicU64,
    listeners: Mutex<Vec<UnixListenerHandle>>,
}

impl Connector {
    async fn client(&self, id: &str) -> Result<Client, flybus::BusError> {
        let transport = match self.via {
            Via::Memory => self.router.connect_in_memory_as(id),
            Via::Unix => {
                let n = self.next_socket.fetch_add(1, Ordering::Relaxed);
                let path = self.sockets.join(format!("{id}-{n}.sock"));
                let listener = self.router.listen_unix_as(&path, id).await.map_err(|e| {
                    flybus::BusError::new(flybus::ErrorCode::RouterLost, format!("listen: {e}"))
                })?;
                let transport = Transport::unix(&path).await.map_err(|e| {
                    flybus::BusError::new(flybus::ErrorCode::RouterLost, format!("connect: {e}"))
                })?;
                self.listeners.lock().expect("not poisoned").push(listener);
                transport
            }
        };
        Client::connect(transport, ClientConfig::new(id, &self.store_root)).await
    }
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
    pub environment: WorkerHandle,
    pub agents: BTreeMap<Id, WorkerHandle>,
    pub config: HarnessConfig,
    pub via: Via,
    /// How many native frames the environment has actually rendered.
    pub renders: RenderCounter,
    /// What each agent read out of its sensory attachments.
    pub sensors: BTreeMap<Id, SensorLog>,
    connector: Connector,
    observers: Mutex<Vec<Client>>,
}

impl SessionHarness {
    /// Builds the router, the workers and the coordinator. Nothing has stepped yet.
    pub async fn start(
        via: Via,
        root: &Path,
        config: HarnessConfig,
    ) -> Result<SessionHarness, flybus::BusError> {
        let store_root = root.join("store");
        let sockets = root.join("sockets");
        std::fs::create_dir_all(&sockets).expect("the caller owns a writable directory");

        let mut policy = Policy::closed()
            .client(
                "coordinator",
                grants(|g| {
                    g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")];
                    g.publish = vec![Pattern::prefix("session.")];
                    g.manage_topics = vec![Pattern::prefix("session.")];
                }),
            )
            .client(ENV_CLIENT, grants(|g| g.register = vec![Pattern::exact(ENV_SERVICE)]))
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
        let connector = Connector {
            router,
            via,
            store_root,
            sockets,
            next_socket: AtomicU64::new(0),
            listeners: Mutex::new(Vec::new()),
        };

        let step_duration = hz(config.step_hz).expect("a positive cadence");
        let tick_duration = millis(config.tick_ms).expect("a positive tick");
        let renders = RenderCounter::new();
        let sensors: BTreeMap<Id, SensorLog> = config
            .agents
            .iter()
            .map(|spec| (spec.agent_id.clone(), SensorLog::new()))
            .collect();

        // The environment first: it owns the world and the descriptor.
        let env_client = connector.client(ENV_CLIENT).await?;
        let env_service = env_client.register(ENV_SERVICE, ServiceConfig::default()).await?;
        let env_incarnation = env_service.incarnation().to_owned();
        let environment = serve(
            env_client,
            env_service,
            CounterEnvironment::new(EnvironmentConfig {
                session_id: config.session_id.clone(),
                worker_id: id(ENV_WORKER),
                incarnation_id: id("arena-inc-1"),
                step_duration,
                ports: config.agents.iter().map(|a| a.port_id.clone()).collect(),
                observation_delay_steps: config.observation_delay_steps,
                renders: renders.clone(),
                faults: config.environment_faults.clone(),
            }),
        );

        let mut slots = Vec::new();
        let mut agents = BTreeMap::new();
        for spec in &config.agents {
            let service_name = agent_service(&spec.agent_id);
            let client = connector.client(&agent_client(&spec.agent_id)).await?;
            let service = client.register(&service_name, ServiceConfig::default()).await?;
            let incarnation = service.incarnation().to_owned();
            let handle = serve(
                client,
                service,
                FakeAgentWorker::new(AgentConfig {
                    session_id: config.session_id.clone(),
                    agent_id: spec.agent_id.clone(),
                    incarnation_id: parse_id(&format!("{}-inc-1", spec.agent_id))
                        .expect("an agent id plus a suffix is an Id"),
                    tick_duration,
                    warmup_ticks: config.warmup_ticks,
                    sensors: sensors[&spec.agent_id].clone(),
                    faults: spec.faults.clone(),
                }),
            );
            slots.push(AgentSlot::new(
                WorkerRef::new(&service_name, &incarnation, &spec.agent_id),
                spec.agent_id.clone(),
                spec.port_id.clone(),
                synthetic_profile(&spec.agent_id, &tick_duration, config.warmup_ticks),
                spec.seed,
            ));
            agents.insert(spec.agent_id.clone(), handle);
        }

        let coordinator_client = connector.client("coordinator").await?;
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
            WorkerRef::new(ENV_SERVICE, &env_incarnation, &id(ENV_WORKER)),
            slots,
            Box::new(CounterTask::new(&config.epoch, config.terminal)),
            executors,
        );

        Ok(SessionHarness {
            coordinator,
            environment,
            agents,
            config,
            via,
            renders,
            sensors,
            connector,
            observers: Mutex::new(Vec::new()),
        })
    }

    pub fn router(&self) -> &Router {
        &self.connector.router
    }

    /// A client for `id`, connected the same way every participant is.
    pub async fn client(&self, id: &str) -> Result<Client, flybus::BusError> {
        self.connector.client(id).await
    }

    /// An extra subscriber, for a test that watches the published boundaries.
    pub async fn observer(&self) -> Result<Client, flybus::BusError> {
        let client = self.connector.client("observer").await?;
        self.observers.lock().expect("not poisoned").push(client.clone());
        Ok(client)
    }

    /// Replaces one agent's worker with a fresh incarnation, as a restore would.
    ///
    /// The coordinator still pins the old registration, so its next call to that agent fails
    /// rather than silently reaching another brain.
    pub async fn restart_agent(&mut self, agent_id: &Id) -> Result<Restarted, flybus::BusError> {
        let tick_duration = millis(self.config.tick_ms).expect("a positive tick");
        if let Some(old) = self.agents.remove(agent_id) {
            old.stop().await;
        }
        let service_name = agent_service(agent_id);
        let client = self.connector.client(&format!("{}-r2", agent_client(agent_id))).await?;
        let service = loop {
            match client.register(&service_name, ServiceConfig::default()).await {
                Ok(service) => break service,
                Err(e) if e.code == flybus::ErrorCode::Conflict => {
                    // The old registration is released when its connection finishes closing.
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                Err(e) => return Err(e),
            }
        };
        let spec = self
            .config
            .agents
            .iter()
            .find(|spec| spec.agent_id == *agent_id)
            .expect("a configured agent")
            .clone();
        let incarnation_id =
            parse_id(&format!("{agent_id}-inc-2")).expect("an agent id plus a suffix is an Id");
        let restarted = Restarted {
            service: service_name,
            service_incarnation: service.incarnation().to_owned(),
            incarnation_id: incarnation_id.clone(),
        };
        let handle = serve(
            client,
            service,
            FakeAgentWorker::new(AgentConfig {
                session_id: self.config.session_id.clone(),
                agent_id: agent_id.clone(),
                incarnation_id,
                tick_duration,
                warmup_ticks: self.config.warmup_ticks,
                sensors: self.sensor_log(agent_id),
                faults: spec.faults,
            }),
        );
        self.agents.insert(agent_id.clone(), handle);
        Ok(restarted)
    }

    /// What one agent read out of its sensory attachments, in order.
    pub fn sensor_log(&self, agent_id: &Id) -> SensorLog {
        self.sensors.get(agent_id).cloned().unwrap_or_default()
    }

    /// How many native frames the environment rendered. Forwarding one image to several
    /// recipients does not render it again.
    pub fn renders(&self) -> u64 {
        self.renders.count()
    }

    /// The agent worker's progress counter, which is its fake model's mutation count.
    pub fn agent_mutations(&self, agent_id: &Id) -> u64 {
        self.agents.get(agent_id).map(WorkerHandle::progress_counter).unwrap_or_default()
    }

    pub fn environment_mutations(&self) -> u64 {
        self.environment.progress_counter()
    }

    pub fn agent_status(&self, agent_id: &Id) -> Option<StatusCell> {
        self.agents.get(agent_id).map(|handle| handle.status.clone())
    }

    /// Stops every worker and closes the router.
    pub async fn shutdown(self) {
        let SessionHarness { coordinator, environment, agents, connector, observers, .. } =
            self;
        drop(coordinator);
        environment.stop().await;
        for (_, handle) in agents {
            handle.stop().await;
        }
        for observer in observers.into_inner().expect("not poisoned") {
            observer.close().await;
        }
        connector.router.shutdown();
    }
}
