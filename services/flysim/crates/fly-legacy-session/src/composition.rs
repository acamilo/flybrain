//! The legacy Game Boy composition on the session framework: one legacy agent (AGENT-01), the
//! legacy environment (ENV-01), and the `pokered-macros-v1` task and executor (this crate) in the
//! coordinator, over one router, in any of the launcher's execution modes.
//!
//! This is the whole new runtime of the live fly short of its edge (EDGE-01) and its FLYSIM01
//! export (STATE-02): what CUT-01's shadow runs beside the legacy service.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use flybus::{Grants, Pattern, Policy, RouterConfig};

use fly_session::coordinator::{AgentSlot, Coordinator, StepDetails, StepReport};
use fly_session::fly_session_types::extensions::ROLLBACK_POLICY;
use fly_session::fly_session_types::gameboy;
use fly_session::launcher::{
    ExecutionMode, Launcher, LegacyAgentLaunch, LegacyEnvironmentLaunch, SUPERVISOR_CLIENT,
    ThreadBudget, Via,
};
use fly_session::legacy_agent::{LegacyProfileKind, SPIKES_ATTACHMENT};
use fly_session::legacy_env::BackendConfig;
use fly_session::task::ActionExecutor;
use fly_session::types::*;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flysim::snapshot::MacroMode;

use crate::task::{PokeredConfig, PokeredTask, TaskRecord};

fn grants(f: impl FnOnce(&mut Grants)) -> Grants {
    let mut g = Grants::default();
    f(&mut g);
    g
}

const COORDINATOR_CLIENT: &str = "coordinator";
const ENV_CLIENT: &str = "legacy-world";
const ENV_SERVICE: &str = "env.world";
const ENV_WORKER: &str = "world";
const AGENT_CLIENT: &str = "legacy-fly";

/// How one legacy session is composed.
#[derive(Clone, Debug)]
pub struct LegacyConfig {
    pub mode: ExecutionMode,
    pub rom_path: PathBuf,
    pub dataset_dir: PathBuf,
    pub profile: LegacyProfileKind,
    pub macro_mode: MacroMode,
    pub agent_id: Id,
    pub agent_threads: usize,
    /// Record the step details and the task records (a parity run).
    pub record: bool,
}

/// The macro channels and the macro group's hold for a mode: the decoder preset flysim builds.
pub fn channels_and_hold(mode: MacroMode) -> (Vec<String>, f64) {
    let channels: Vec<&str> = if mode.dealt() {
        flybrain_gb::macro_channels(crate::task::GAME)
    } else {
        Vec::new()
    };
    let preset = gameboy_decoder_config_with_macros(&channels);
    let hold_ms = preset
        .macros
        .as_ref()
        .or(preset.exclusive.as_ref())
        .map_or(0.0, |group| group.hold_ms);
    (channels.iter().map(|c| (*c).to_owned()).collect(), hold_ms)
}

/// A running legacy session: its launcher, its coordinator and the task object.
pub struct LegacySession {
    pub coordinator: Coordinator,
    pub task: PokeredTask,
    pub launcher: Launcher,
    pub rom: Vec<u8>,
    pub config: LegacyConfig,
    session_id: Id,
    backend: BackendConfig,
    /// Which generation of the world is running: 1 is the one the session started.
    world_generation: u32,
}

/// One transition as a parity run sees it.
pub struct Step {
    pub report: StepReport,
    pub details: Option<StepDetails>,
    pub records: Vec<TaskRecord>,
}

impl LegacySession {
    /// Builds the router, launches the world and the fly, and builds the coordinator with the task
    /// object's two faces. Nothing has been initialized yet.
    pub async fn start(root: &Path, config: LegacyConfig) -> Result<LegacySession, String> {
        let rom = std::fs::read(&config.rom_path)
            .map_err(|e| format!("the cartridge {}: {e}", config.rom_path.display()))?;
        let rom_digest = sha256_hex(&rom);
        let session_id = id("legacy");
        let epoch = id("e1");
        let store_root = root.join("store");
        let sockets = root.join("sockets");
        std::fs::create_dir_all(&sockets).map_err(|e| format!("sockets: {e}"))?;
        let agent_service = format!("agent.{}", config.agent_id);
        let policy = Policy::closed()
            .client(
                COORDINATOR_CLIENT,
                grants(|g| {
                    g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")];
                    g.publish = vec![Pattern::prefix("session.")];
                    g.manage_topics = vec![Pattern::prefix("session.")];
                    g.register = vec![Pattern::prefix("session.")];
                }),
            )
            // The launcher's supervisor: health checks and shutdown, never a registration.
            .client(
                SUPERVISOR_CLIENT,
                grants(|g| g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")]),
            )
            .client(
                ENV_CLIENT,
                grants(|g| g.register = vec![Pattern::exact(ENV_SERVICE)]),
            )
            .client(
                &format!("{ENV_CLIENT}-r2"),
                grants(|g| g.register = vec![Pattern::exact(ENV_SERVICE)]),
            )
            .client(
                AGENT_CLIENT,
                grants(|g| g.register = vec![Pattern::exact(&agent_service)]),
            );
        let mut router_config = RouterConfig::new(&store_root);
        router_config.policy = policy;
        let router = flybus::Router::new(router_config).map_err(|e| format!("router: {e}"))?;
        let budget = ThreadBudget::new(2 + config.agent_threads, 1).map_err(|e| e.message)?;
        let mut launcher = Launcher::start(
            router,
            config.mode,
            Via::Unix,
            &store_root,
            &sockets,
            budget,
        )
        .await
        .map_err(|e| format!("launcher: {}", e.message))?;

        let mut backend = BackendConfig::legacy(&rom_digest);
        backend.slots = vec![id(crate::task::SLOT)];
        launcher
            .launch_legacy_environment(LegacyEnvironmentLaunch {
                session_id: session_id.clone(),
                worker_id: id(ENV_WORKER),
                incarnation_id: id("world-inc-1"),
                worker_threads: 1,
                rom_path: config.rom_path.clone(),
                backend: backend.clone(),
                client_id: ENV_CLIENT.to_owned(),
                service: ENV_SERVICE.to_owned(),
            })
            .await
            .map_err(|e| format!("the world: {}", e.message))?;
        let environment = launcher
            .worker(&id(ENV_WORKER))
            .expect("launched")
            .worker_ref();

        let (macro_channels, hold_ms) = channels_and_hold(config.macro_mode);
        let identity = launcher
            .launch_legacy_agent(LegacyAgentLaunch {
                session_id: session_id.clone(),
                agent_id: config.agent_id.clone(),
                port_id: id(fly_session::legacy_env::DEFAULT_PORT),
                incarnation_id: parse_id(&format!("{}-inc-1", config.agent_id))?,
                worker_threads: config.agent_threads,
                dataset_dir: config.dataset_dir.clone(),
                profile: config.profile,
                macro_channels: macro_channels.clone(),
                client_id: AGENT_CLIENT.to_owned(),
                service: agent_service,
            })
            .await
            .map_err(|e| format!("the fly: {}", e.message))?;
        let agent_ref = launcher
            .worker(&config.agent_id)
            .expect("launched")
            .worker_ref();
        let mut slot = AgentSlot::new(
            agent_ref,
            config.agent_id.clone(),
            id(fly_session::legacy_env::DEFAULT_PORT),
            config.profile.profile().asset,
            fly_session::legacy_parity::LEGACY_SEED,
        );
        slot.worker_threads = identity.worker_threads as u64;
        slot.model_version = gameboy::KERNEL_VERSION.to_owned();
        slot.plasticity_version = gameboy::PLASTICITY_VERSION.to_owned();

        let task = PokeredTask::new(
            PokeredConfig {
                agent_id: config.agent_id.clone(),
                port_id: id(fly_session::legacy_env::DEFAULT_PORT),
                mode: config.macro_mode,
                macro_channels,
                hold_ms,
                record: config.record,
            },
            &rom,
            &rom_digest,
            &epoch,
        )?;
        let client = launcher
            .connect(COORDINATOR_CLIENT)
            .await
            .map_err(|e| format!("connect: {}", e.message))?;
        let executors: BTreeMap<Id, Box<dyn ActionExecutor>> =
            BTreeMap::from([(config.agent_id.clone(), task.executor())]);
        let mut coordinator = Coordinator::new(
            client,
            session_id.clone(),
            epoch,
            id("ep1"),
            environment,
            vec![slot],
            task.task(),
            executors,
        );
        coordinator.set_environment_config(
            backend.asset_ref(),
            fly_session::environment::synthetic_asset("pokered-task", gameboy::EXECUTOR_ID),
        );
        coordinator.set_media(
            &[gameboy::VIEW_ID],
            &[fly_session::legacy_env::AUDIO_STREAM_ID],
        );
        coordinator.set_state_format(fly_session::legacy_env::STATE_FORMAT_ID);
        coordinator.set_decision_schema(gameboy::CHANNELS.schema_ref());
        coordinator
            .declare_rollback_policy(ROLLBACK_POLICY)
            .map_err(|e| e.message)?;
        // The legacy loop runs as fast as its brain allows when measured, and the service paces
        // itself; a parity run is a measurement.
        if config.record {
            coordinator.request_commit_attachments(&[SPIKES_ATTACHMENT]);
        }
        Ok(LegacySession {
            coordinator,
            task,
            launcher,
            rom,
            config,
            session_id,
            backend,
            world_generation: 1,
        })
    }

    /// Hello, `Environment.Initialize` (the one-frame scaffold), the task's bootstrap on O[0],
    /// `Agent.Initialize` (the warm-up): `Ready(0)`.
    pub async fn bootstrap(&mut self) -> Result<(), String> {
        self.coordinator
            .bootstrap()
            .await
            .map_err(|e| format!("bootstrap ({}): {}", e.detail, e.error.message))?;
        if self.config.record {
            self.coordinator.disable_pacing();
        }
        Ok(())
    }

    /// Installs a FLYSIM01 checkpoint as this session's start (after [`LegacySession::bootstrap`],
    /// before any transition): the world is replaced by a fresh one (the environment stages a
    /// restore only on a replacement, as every group restore does), then [`crate::import`] runs
    /// the group install and the session resumes at the checkpoint's boundary.
    pub async fn import_flysim01(
        &mut self,
        bytes: &[u8],
        checkpoint_id: &Id,
        epoch: &Id,
    ) -> Result<crate::import::Imported, String> {
        if self.world_generation != 1 {
            return Err("this session has already replaced its world".to_owned());
        }
        self.launcher.reap(&id(ENV_WORKER), &id("import")).await;
        self.world_generation = 2;
        self.launcher
            .launch_legacy_environment(LegacyEnvironmentLaunch {
                session_id: self.session_id.clone(),
                worker_id: id(ENV_WORKER),
                incarnation_id: id("world-inc-2"),
                worker_threads: 1,
                rom_path: self.config.rom_path.clone(),
                backend: self.backend.clone(),
                client_id: format!("{ENV_CLIENT}-r2"),
                service: ENV_SERVICE.to_owned(),
            })
            .await
            .map_err(|e| format!("the replacement world: {}", e.message))?;
        let world = self
            .launcher
            .worker(&id(ENV_WORKER))
            .expect("launched")
            .worker_ref();
        self.coordinator
            .replace_participant(&id(ENV_WORKER), world)
            .map_err(|e| format!("replacing the world: {}", e.error.message))?;
        let rom = self.rom.clone();
        let imported = crate::import::import_flysim01(
            &mut self.coordinator,
            &self.task,
            bytes,
            &rom,
            &self.config.agent_id,
            checkpoint_id,
            self.backend.audio_sample_rate,
            epoch,
        )
        .await?;
        if self.config.record {
            self.coordinator.disable_pacing();
        }
        Ok(imported)
    }

    /// One transition, with its details when recorded.
    pub async fn step(&mut self) -> Result<Step, String> {
        let report = self
            .coordinator
            .step()
            .await
            .map_err(|e| format!("step ({} at {}): {}", e.detail, e.phase, e.error.message))?;
        Ok(Step {
            report,
            details: self.coordinator.take_details(),
            records: self.task.take_records(),
        })
    }

    pub async fn stop(mut self) {
        self.launcher.reap_all(&id("done")).await;
    }
}
