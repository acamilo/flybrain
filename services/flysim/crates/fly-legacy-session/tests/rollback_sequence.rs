//! The coordinator's boundary actions on the legacy workers, without a ROM: ENV-01's toy
//! cartridge, AGENT-01's toy connectome, and a scripted task that asks for a slot save at
//! boundary 3, a reward at 4 and a rollback onto the slot at 6.
//!
//! What it holds, in-process and as processes:
//!
//! - the rollback runs `Ready(e, 6) -> RollingBack(6) -> Ready(e', 6)`, in the policy's order
//!   (`RestoreSlot`, the task's part, `Agent.Rollback`), and the session steps on in `e'`;
//! - the restored boundary is the saved one: the task reads O'[6]'s memory image and it is O[3]'s;
//! - the step details record the save's state digest and the rollback, and the trace carries them
//!   as boundary actions, saves first;
//! - admitted sugar is cut into the next Prepare and reported applied at the boundary it committed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use flybus::{Grants, Pattern, Policy, RouterConfig};

use fly_session::coordinator::{AdmissionEnd, AgentSlot, Coordinator};
use fly_session::fly_session_types::extensions::ROLLBACK_POLICY;
use fly_session::fly_session_types::gameboy::{
    self, ChannelsDecision, ReadoutContext, RollbackRequest,
};
use fly_session::fly_session_types::trace::BoundaryActionKind;
use fly_session::launcher::{
    ExecutionMode, Launcher, LegacyAgentLaunch, LegacyEnvironmentLaunch, SUPERVISOR_CLIENT,
    ThreadBudget, Via,
};
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_env::{BackendConfig, MEMORY_ATTACHMENT, STATE_FORMAT_ID};
use fly_session::legacy_env_parity::toy_cart;
use fly_session::task::{
    ActionExecutor, Bootstrap, Evaluation, Inspection, RollbackEvaluation, Task,
};
use fly_session::types::*;

fn grants(f: impl FnOnce(&mut Grants)) -> Grants {
    let mut g = Grants::default();
    f(&mut g);
    g
}

fn schema(name: &str) -> SchemaRef {
    SchemaRef::new(name, 1, &digest_of_bytes(name.as_bytes())).unwrap()
}

/// What the scripted task saw.
#[derive(Default)]
struct Seen {
    /// Boundary -> the image digest the task read for it (the last reading wins).
    images: BTreeMap<u64, String>,
    restored: Option<(u64, String)>,
}

struct Scripted {
    agent: Id,
    seen: Arc<Mutex<Seen>>,
    evaluations: u64,
}

fn context() -> TypedValue {
    ReadoutContext {
        boot: false,
        bound: Vec::new(),
        location: None,
    }
    .to_typed()
}

fn image_digest(inspection: &Inspection) -> DomainResult<String> {
    let bytes = inspection
        .attachments
        .get(MEMORY_ATTACHMENT)
        .ok_or_else(|| DomainError::invalid("no image"))?;
    assert_eq!(bytes.len(), 65_536);
    Ok(digest_of_bytes(bytes))
}

impl Task for Scripted {
    fn schema(&self) -> SchemaRef {
        schema("scripted.progress")
    }

    fn inspection_attachments(&self) -> Vec<String> {
        vec![MEMORY_ATTACHMENT.to_owned()]
    }

    fn bootstrap(
        &mut self,
        initial: &Inspection,
        _bindings: &[PortBinding],
    ) -> DomainResult<Bootstrap> {
        let digest = image_digest(initial)?;
        self.seen
            .lock()
            .unwrap()
            .images
            .insert(initial.boundary, digest);
        Ok(Bootstrap {
            contexts: BTreeMap::from([(self.agent.clone(), context())]),
            progress: self.progress(),
            events: Vec::new(),
        })
    }

    fn evaluate_transition(
        &mut self,
        scope: &Scope,
        old: &Inspection,
        new: &Inspection,
        _controls: &[PortControl],
    ) -> DomainResult<Evaluation> {
        assert_eq!(old.boundary, scope.step, "O[k] is the old inspection");
        assert_eq!(new.boundary, scope.step + 1, "O[k+1] is the new one");
        self.evaluations += 1;
        let k1 = scope.step + 1;
        self.seen
            .lock()
            .unwrap()
            .images
            .insert(k1, image_digest(new)?);
        let mut outcome = AgentOutcome::default();
        let mut events = Vec::new();
        if k1 == 4 {
            let event = TaskEvent {
                id: event_id(&scope.epoch, k1, "scripted", 0),
                kind_id: id("scripted.reward"),
                source_step: k1,
                agent_id: Some(self.agent.clone()),
                payload: TypedValue::new(schema("scripted.event"), json!({})).unwrap(),
            };
            outcome.rewards.push(Reward {
                event_id: event.id.clone(),
                rule_id: id("scripted"),
                value: 0.5,
            });
            outcome.stimulations.push(Stimulus {
                id: event.id.clone(),
                kind_id: gameboy::STIMULUS_REWARD_PULSE.to_owned(),
                duration_ms: 100.0,
            });
            events.push(event);
        }
        Ok(Evaluation {
            outcomes: BTreeMap::from([(self.agent.clone(), outcome)]),
            next_contexts: BTreeMap::from([(self.agent.clone(), context())]),
            progress: self.progress(),
            events,
            episode: (k1 == 6).then(|| EpisodeRequest {
                kind: EpisodeRequestKind::Rollback,
                reason: id("stall"),
                outcome: RollbackRequest {
                    slot_id: "best".into(),
                    trigger: "stall".into(),
                }
                .to_typed(),
            }),
            slot_saves: if k1 == 3 {
                vec![id("best")]
            } else {
                Vec::new()
            },
        })
    }

    fn rollback(
        &mut self,
        scope: &Scope,
        restored: &Inspection,
    ) -> DomainResult<RollbackEvaluation> {
        assert_eq!(restored.boundary, scope.step);
        self.seen.lock().unwrap().restored = Some((restored.boundary, image_digest(restored)?));
        Ok(RollbackEvaluation {
            next_contexts: BTreeMap::from([(self.agent.clone(), context())]),
            events: Vec::new(),
        })
    }

    fn progress(&self) -> TypedValue {
        TypedValue::new(
            schema("scripted.progress"),
            json!({"evaluations": self.evaluations}),
        )
        .unwrap()
    }

    fn evaluations(&self) -> u64 {
        self.evaluations
    }

    fn capture(&self) -> DomainResult<TypedValue> {
        Ok(self.progress())
    }

    fn validate_restore(&self, _state: &TypedValue) -> DomainResult<()> {
        Ok(())
    }

    fn install_restore(&mut self, _epoch: &Id, _state: &TypedValue) -> DomainResult<()> {
        Ok(())
    }

    fn rebase_ids(&self, _to_epoch: &Id) -> DomainResult<BTreeMap<Id, Id>> {
        Ok(BTreeMap::new())
    }

    fn event_watermarks(&self) -> (u64, u64) {
        (0, 0)
    }
}

/// The joypad the decision's buttons ask for.
struct Buttons;

impl ActionExecutor for Buttons {
    fn apply(
        &mut self,
        _scope: &Scope,
        decision: &TypedValue,
        _current: &Inspection,
        _progress: &TypedValue,
        _clock: &RationalNs,
    ) -> DomainResult<(ControllerIntent, Vec<TaskEvent>)> {
        let decision =
            ChannelsDecision::from_typed(decision).map_err(|e| DomainError::invalid(e.0))?;
        let control = fly_session::legacy_env::control_of("p1", decision.mask());
        Ok((
            ControllerIntent {
                buttons: control.buttons,
                axes: control.axes,
            },
            Vec::new(),
        ))
    }

    fn capture(&self) -> DomainResult<TypedValue> {
        Ok(TypedValue::new(schema("buttons"), json!({})).unwrap())
    }

    fn validate_restore(&self, _state: &TypedValue) -> DomainResult<()> {
        Ok(())
    }

    fn install_restore(&mut self, _state: &TypedValue) -> DomainResult<()> {
        Ok(())
    }
}

async fn run(mode: ExecutionMode) {
    let root = tempfile::tempdir().unwrap();
    let rom = toy_cart::rom(0);
    let rom_path = root.path().join("toy.gb");
    std::fs::write(&rom_path, &rom).unwrap();
    let store = root.path().join("store");
    let sockets = root.path().join("sockets");
    std::fs::create_dir_all(&sockets).unwrap();
    let policy = Policy::closed()
        .client(
            "coordinator",
            grants(|g| {
                g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")];
                g.publish = vec![Pattern::prefix("session.")];
                g.manage_topics = vec![Pattern::prefix("session.")];
                g.register = vec![Pattern::prefix("session.")];
            }),
        )
        .client(
            SUPERVISOR_CLIENT,
            grants(|g| g.call = vec![Pattern::prefix("agent."), Pattern::prefix("env.")]),
        )
        .client(
            "world",
            grants(|g| g.register = vec![Pattern::exact("env.world")]),
        )
        .client(
            "fly",
            grants(|g| g.register = vec![Pattern::exact("agent.fly")]),
        );
    let mut config = RouterConfig::new(&store);
    config.policy = policy;
    let router = flybus::Router::new(config).unwrap();
    let mut launcher = Launcher::start(
        router,
        mode,
        Via::Unix,
        &store,
        &sockets,
        ThreadBudget::new(3, 1).unwrap(),
    )
    .await
    .unwrap();
    let mut backend = BackendConfig::legacy(&digest_of_bytes(&rom));
    backend.slots = vec![id("best")];
    launcher
        .launch_legacy_environment(LegacyEnvironmentLaunch {
            session_id: id("s"),
            worker_id: id("world"),
            incarnation_id: id("world-1"),
            worker_threads: 1,
            rom_path,
            backend: backend.clone(),
            client_id: "world".into(),
            service: "env.world".into(),
        })
        .await
        .unwrap();
    launcher
        .launch_legacy_agent(LegacyAgentLaunch {
            session_id: id("s"),
            agent_id: id("fly"),
            port_id: id("p1"),
            incarnation_id: id("fly-1"),
            worker_threads: 1,
            dataset_dir: fly_session::legacy_parity::toy::dir(),
            profile: LegacyProfileKind::Toy,
            macro_channels: Vec::new(),
            client_id: "fly".into(),
            service: "agent.fly".into(),
        })
        .await
        .unwrap();
    let mut slot = AgentSlot::new(
        launcher.worker(&id("fly")).unwrap().worker_ref(),
        id("fly"),
        id("p1"),
        LegacyProfileKind::Toy.profile().asset,
        fly_session::legacy_parity::LEGACY_SEED,
    );
    slot.model_version = gameboy::KERNEL_VERSION.into();
    slot.plasticity_version = gameboy::PLASTICITY_VERSION.into();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let task = Scripted {
        agent: id("fly"),
        seen: seen.clone(),
        evaluations: 0,
    };
    let executors: BTreeMap<Id, Box<dyn ActionExecutor>> =
        BTreeMap::from([(id("fly"), Box::new(Buttons) as Box<dyn ActionExecutor>)]);
    let mut coordinator = Coordinator::new(
        launcher.connect("coordinator").await.unwrap(),
        id("s"),
        id("e1"),
        id("ep1"),
        launcher.worker(&id("world")).unwrap().worker_ref(),
        vec![slot],
        Box::new(task),
        executors,
    );
    coordinator.set_environment_config(
        backend.asset_ref(),
        fly_session::environment::synthetic_asset("scripted", "scripted-v1"),
    );
    coordinator.set_media(
        &[gameboy::VIEW_ID],
        &[fly_session::legacy_env::AUDIO_STREAM_ID],
    );
    coordinator.set_state_format(STATE_FORMAT_ID);
    coordinator.set_decision_schema(gameboy::CHANNELS.schema_ref());
    coordinator
        .declare_rollback_policy(ROLLBACK_POLICY)
        .unwrap();
    coordinator.record_details(true);
    coordinator
        .bootstrap()
        .await
        .unwrap_or_else(|e| panic!("bootstrap: {}", e.error.message));
    coordinator.disable_pacing();
    let admissions = coordinator.admissions();
    assert_eq!(
        admissions.stimulus_remaining_ms(&id("fly")),
        None,
        "no commit yet"
    );

    let mut actions = Vec::new();
    for k in 0..9u64 {
        if k == 2 {
            // Two sugars before the next commit: the legacy drain admits the first and refuses
            // the second (its pulse is already active), and so does the session's admission.
            let mut legacy = flysim::ratelimit::RateLimiter::new(6);
            let mut remaining = admissions
                .stimulus_remaining_ms(&id("fly"))
                .expect("committed");
            let legacy_answers: Vec<_> = [300.0, 250.0]
                .iter()
                .map(|duration: &f64| {
                    let answer = legacy.admit(1_000, remaining);
                    if answer.is_ok() {
                        remaining = remaining.max(*duration);
                    }
                    answer
                })
                .collect();
            let mut sugar =
                fly_legacy_session::admission::LegacyAdmission::new(admissions.clone(), id("fly"));
            let first = sugar.sugar(1_000, Some(300.0)).map(|(_, d)| d);
            let second = sugar.sugar(1_000, Some(250.0)).map(|(_, d)| d);
            assert_eq!(first, Ok(300.0));
            assert_eq!(legacy_answers[0], Ok(()));
            assert_eq!(
                second.err(),
                legacy_answers[1].err(),
                "the second is refused as legacy refuses it"
            );
            assert_eq!(admissions.queued(), 1);
        }
        let report = coordinator.step().await.unwrap_or_else(|e| {
            panic!(
                "{} step {k}: {} ({})",
                mode.label(),
                e.error.message,
                e.detail
            )
        });
        assert_eq!(report.boundary, k + 1);
        assert!(
            !report.paused && !report.terminal,
            "a rollback is not an episode end"
        );
        let details = coordinator.take_details().unwrap();
        if k == 2 {
            assert_eq!(
                details.admissions.len(),
                1,
                "the sugar is cut into Prepare(2)"
            );
        }
        for action in &details.boundary_actions {
            actions.push((k + 1, action.kind, action.state_digest.clone()));
        }
        if k + 1 == 6 {
            let rollback = details.rollback.as_ref().expect("the rollback's details");
            assert_eq!(rollback.epoch, id("e1.rb1"));
            assert_eq!(rollback.observation.boundary, 6);
            assert!(
                rollback.observation.audio.is_empty(),
                "a restore plays no interval"
            );
            assert_eq!(rollback.agents.len(), 1);
        }
    }
    assert_eq!(
        coordinator.epoch(),
        &id("e1.rb1"),
        "the session steps on in the new epoch"
    );
    assert_eq!(actions.len(), 2);
    assert_eq!(
        (actions[0].0, actions[0].1),
        (3, BoundaryActionKind::SaveSlot)
    );
    assert!(
        actions[0].2.is_some(),
        "a save records the saved state's digest"
    );
    assert_eq!(
        (actions[1].0, actions[1].1, actions[1].2.clone()),
        (6, BoundaryActionKind::Rollback, None)
    );
    let phases: Vec<String> = coordinator
        .trace
        .phases
        .iter()
        .map(|p| format!("{}->{}", p.from, p.to))
        .collect();
    assert!(
        phases.contains(&"Ready(6)->RollingBack(6)".to_owned()),
        "{phases:?}"
    );
    assert!(
        phases.contains(&"RollingBack(6)->Ready(6)".to_owned()),
        "{phases:?}"
    );
    let traced: Vec<(u64, usize)> = coordinator
        .trace
        .transitions
        .iter()
        .map(|t| {
            (
                t.behaviour.acknowledged_boundary,
                t.behaviour.boundary_actions.len(),
            )
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    assert_eq!(
        traced,
        vec![(3, 1), (6, 1)],
        "the trace carries the boundary actions"
    );
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    let (boundary, restored) = seen.restored.clone().expect("the task rolled back");
    assert_eq!(boundary, 6);
    assert_eq!(
        restored, seen.images[&3],
        "O'[6] is the world saved at boundary 3"
    );
    assert_ne!(
        restored, seen.images[&6],
        "and not the one the rollback threw away"
    );
    let ended = admissions.take_ended();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].1, AdmissionEnd::Applied { boundary: 3 });
    assert_eq!(
        admissions.pending_stimulus_ms(&id("fly")),
        None,
        "nothing pending once committed"
    );
    assert!(admissions.stimulus_remaining_ms(&id("fly")).is_some());
    let audit = coordinator.audit.join(" ");
    for needle in [
        "save-slot:best@3",
        "restore-slot:best@6",
        "agent-rollback:fly@6",
        "rolled-back:best@6",
    ] {
        assert!(audit.contains(needle), "{needle} in {audit}");
    }
    launcher.reap_all(&id("done")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_then_a_rollback_in_process() {
    run(ExecutionMode::InProcess).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_then_a_rollback_as_processes() {
    if !fly_session::launcher::default_worker_program().is_file() {
        eprintln!("process mode skipped: no worker program");
        return;
    }
    run(ExecutionMode::Process).await;
}
