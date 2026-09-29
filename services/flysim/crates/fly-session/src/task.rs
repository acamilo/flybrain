//! Coordinator-local task and executor interfaces (`workers-v1` section 4), plus the
//! deterministic counter task and the identity executor the synthetic composition uses.
//!
//! These are library interfaces, not extra bus services. The task interprets inspection data
//! and asks for outcomes; the executor translates a selected decision using read-only current
//! game state and task progress; the coordinator orders and applies the results. Nothing here
//! writes a controller or neural state directly.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

// `crate::types` is this crate's facade over the shared `fly-session-types` crate; the
// glob keeps the contract's own names in sight instead of restating them.
use crate::types::*;

/// The schemas the synthetic arena composition registers.
pub fn inspection_schema() -> SchemaRef {
    synthetic_schema("arena.inspection.v1", 1)
}

pub fn decision_schema() -> SchemaRef {
    synthetic_schema("arena.decision.v1", 1)
}

pub fn context_schema() -> SchemaRef {
    synthetic_schema("arena.context.v1", 1)
}

pub fn progress_schema() -> SchemaRef {
    synthetic_schema("arena.progress.v1", 1)
}

pub fn event_schema() -> SchemaRef {
    synthetic_schema("arena.event.v1", 1)
}

pub fn episode_schema() -> SchemaRef {
    synthetic_schema("arena.episode.v1", 1)
}

/// The schema of a captured task ledger.
pub fn ledger_schema() -> SchemaRef {
    synthetic_schema("arena.ledger.v1", 1)
}

/// The schema of a captured action-executor state.
pub fn executor_schema() -> SchemaRef {
    synthetic_schema("arena.executor.v1", 1)
}

pub fn controller_schema_ref() -> SchemaRef {
    synthetic_schema("arena.controller.v1", 1)
}

/// One boundary's inspection as the task and the executor read it: the observation's typed
/// `inspection` value plus the bytes of the attachments the task declared
/// ([`Task::inspection_attachments`]).
///
/// An artifact-backed inspection (`gameboy-memory-inspection-v1`: a 64 KiB image per boundary)
/// names its bytes by `ArtifactRef`; the coordinator holds the artifact until the transition
/// that reads it has been evaluated (`step-v1` section 3, Phase C amendment) and hands the task
/// the bytes, so neither the task nor the executor ever reads a live world. The synthetic
/// composition declares no attachment, and its inspection derefs to the typed value it always
/// was.
#[derive(Clone, Debug)]
pub struct Inspection {
    /// The boundary the observation belongs to.
    pub boundary: u64,
    pub value: TypedValue,
    /// Attachment name to bytes, for the names the task declared.
    pub attachments: BTreeMap<String, Arc<Vec<u8>>>,
}

impl Inspection {
    /// An inspection with no attachment: the synthetic composition's.
    pub fn plain(boundary: u64, value: TypedValue) -> Inspection {
        Inspection { boundary, value, attachments: BTreeMap::new() }
    }
}

impl std::ops::Deref for Inspection {
    type Target = TypedValue;

    fn deref(&self) -> &TypedValue {
        &self.value
    }
}

/// What a task's part of a rollback produced (`step-v1` section 6 amendment, step 3): the next
/// decision contexts for the restored boundary, and any events it reports.
#[derive(Clone, Debug)]
pub struct RollbackEvaluation {
    pub next_contexts: BTreeMap<Id, TypedValue>,
    pub events: Vec<TaskEvent>,
}

/// What `Task.bootstrap` produced.
#[derive(Clone, Debug)]
pub struct Bootstrap {
    pub contexts: BTreeMap<Id, TypedValue>,
    pub progress: TypedValue,
    pub events: Vec<TaskEvent>,
}

/// What `Task.evaluate_transition` produced, for exactly one transition.
#[derive(Clone, Debug)]
pub struct Evaluation {
    /// Every configured agent has an entry, including an empty one.
    pub outcomes: BTreeMap<Id, AgentOutcome>,
    pub next_contexts: BTreeMap<Id, TypedValue>,
    pub progress: TypedValue,
    pub events: Vec<TaskEvent>,
    pub episode: Option<EpisodeRequest>,
    /// Slot saves the transition asks for at the boundary it reached, applied by the coordinator
    /// after every Commit and before any capture or rollback there (`step-v1` section 3, Phase C
    /// amendment; environment capability `gameboy-slots-v1`). Empty in every composition that
    /// declares no slots.
    pub slot_saves: Vec<Id>,
}

/// A checkpointable task ledger and the two evaluation entry points.
pub trait Task: Send {
    fn schema(&self) -> SchemaRef;

    /// The inspection attachments this task and its executor read, which the coordinator asks
    /// the environment for, holds and hands over as bytes. None by default.
    fn inspection_attachments(&self) -> Vec<String> {
        Vec::new()
    }

    /// Called once, at boundary 0, before any agent is initialized.
    fn bootstrap(
        &mut self,
        initial_inspection: &Inspection,
        bindings: &[PortBinding],
    ) -> DomainResult<Bootstrap>;

    /// Called exactly once per acknowledged world step, never against a later observation.
    fn evaluate_transition(
        &mut self,
        scope: &Scope,
        old_inspection: &Inspection,
        new_inspection: &Inspection,
        applied_controls: &[PortControl],
    ) -> DomainResult<Evaluation>;

    /// Step 3 of a declared rollback policy, coordinator-local (`step-v1` section 6 amendment):
    /// the task clears its transient observations and the executor cancels any running action
    /// and observes the restored boundary, which yields the next decision contexts. `scope` is
    /// the new epoch at the restored boundary. A task that declares no rollback policy refuses.
    fn rollback(
        &mut self,
        _scope: &Scope,
        _restored: &Inspection,
    ) -> DomainResult<RollbackEvaluation> {
        Err(DomainError::before(
            ErrorCode::Unsupported,
            "this task declares no rollback policy",
        ))
    }

    /// After a group restore installed this task's ledger: the restored boundary's inspection
    /// and the agents' restored brain clock. A task whose executor state is transient
    /// (`restore: legacy-transient-reset`) re-observes the world here and returns the decision
    /// contexts it would hand the agents, which the coordinator holds against the ones the
    /// checkpoint recorded. `None` (the default) means the recorded contexts stand unchecked.
    fn restored(
        &mut self,
        _scope: &Scope,
        _restored: &Inspection,
        _clock: &RationalNs,
    ) -> DomainResult<Option<BTreeMap<Id, TypedValue>>> {
        Ok(None)
    }

    fn progress(&self) -> TypedValue;

    /// How many times `evaluate_transition` has run. A transition must evaluate once.
    fn evaluations(&self) -> u64;

    /// The checkpointable ledger at a committed boundary (`workers-v1` section 4).
    fn capture(&self) -> DomainResult<TypedValue>;

    /// Validates a captured ledger without installing it, so a group install can fail before
    /// anything is changed.
    fn validate_restore(&self, state: &TypedValue) -> DomainResult<()>;

    /// Installs a validated ledger under `epoch`. Event identity is derived from the epoch,
    /// so the new one is part of the install rather than something the ledger keeps from the
    /// epoch it was captured in.
    fn install_restore(&mut self, epoch: &Id, state: &TypedValue) -> DomainResult<()>;

    /// Every event identity this ledger has issued, mapped onto the identity it would have
    /// under `to_epoch`.
    ///
    /// `workers-v1` section 4 derives an event id from the epoch, so a trace recorded in one
    /// epoch cannot be compared with a trace recorded in another until these are rebased.
    /// The ledger owns the derivation, so it is the only thing that can do it.
    fn rebase_ids(&self, to_epoch: &Id) -> DomainResult<BTreeMap<Id, Id>>;

    /// How far event identity has reached: the highest source step and the number issued.
    fn event_watermarks(&self) -> (u64, u64);
}

/// Translates one selected decision into a controller intent, with no port assignment.
pub trait ActionExecutor: Send {
    fn apply(
        &mut self,
        scope: &Scope,
        decision: &TypedValue,
        current_game_state: &Inspection,
        progress: &TypedValue,
        clock: &RationalNs,
    ) -> DomainResult<(ControllerIntent, Vec<TaskEvent>)>;

    /// Per-executor state at a committed boundary (`workers-v1` section 4).
    fn capture(&self) -> DomainResult<TypedValue>;

    /// Validates a captured executor state without installing it.
    fn validate_restore(&self, state: &TypedValue) -> DomainResult<()>;

    /// Installs a validated executor state.
    fn install_restore(&mut self, state: &TypedValue) -> DomainResult<()>;
}

/// The only executor v1 supports: it passes a direct-control decision through unchanged.
#[derive(Clone, Debug, Default)]
pub struct IdentityExecutor;

impl ActionExecutor for IdentityExecutor {
    fn apply(
        &mut self,
        _scope: &Scope,
        decision: &TypedValue,
        _current_game_state: &Inspection,
        _progress: &TypedValue,
        _clock: &RationalNs,
    ) -> DomainResult<(ControllerIntent, Vec<TaskEvent>)> {
        if decision.schema != decision_schema() {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the decision does not carry the profile's registered intent schema",
            ));
        }
        let intent = ControllerIntent::from_json(&decision.value)
            .map_err(|e| DomainError::invalid(format!("decision: {e}")))?;
        Ok((intent, Vec::new()))
    }

    /// The identity executor is stateless, and says so rather than capturing nothing.
    ///
    /// An empty object would be indistinguishable from a stateful executor whose capture went
    /// missing, so the capture names the executor it came from and a restore refuses any
    /// other one.
    fn capture(&self) -> DomainResult<TypedValue> {
        TypedValue::new(executor_schema(), json!({"executor": "identity-v1"}))
            .map_err(|e| DomainError::invalid(e.0))
    }

    fn validate_restore(&self, state: &TypedValue) -> DomainResult<()> {
        if state.schema != executor_schema() {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the captured executor state does not carry the executor schema",
            ));
        }
        match state.value.get("executor").and_then(Value::as_str) {
            Some("identity-v1") => Ok(()),
            other => Err(DomainError::before(
                ErrorCode::IncompatibleState,
                format!("the captured executor is {other:?}, not the identity executor"),
            )),
        }
    }

    fn install_restore(&mut self, state: &TypedValue) -> DomainResult<()> {
        // Stateless: validation is the whole of the install, and it is not skipped.
        self.validate_restore(state)
    }
}

/// When the counter task asks for a terminal episode transition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Terminal {
    /// The episode runs until the application stops it.
    #[default]
    Never,
    /// The counter reached this value or higher.
    Counter(i64),
    /// This many transitions were evaluated.
    AfterTransitions(u64),
    /// After this many transitions, ask for a *rollback* instead: the request a composition with
    /// a declared rollback policy may make, and one without it must treat as a task failure
    /// (`workers-v1` section 4, 2026-09-23). Only a test asks the counter task for this.
    RollbackAfterTransitions(u64),
}

/// The deterministic counter task: rewards come from the arena counter each agent moved.
pub struct CounterTask {
    epoch: Id,
    agents: Vec<Id>,
    bindings: Vec<PortBinding>,
    transitions: u64,
    evaluations: u64,
    total_reward: f64,
    counter: i64,
    /// The highest source step any issued event belongs to, and how many were issued. These
    /// are the event watermarks a checkpoint records and a resumed epoch continues from.
    last_source_step: u64,
    issued_events: u64,
    terminal: Terminal,
}

impl CounterTask {
    pub fn new(epoch: &Id, terminal: Terminal) -> CounterTask {
        CounterTask {
            epoch: epoch.clone(),
            agents: Vec::new(),
            bindings: Vec::new(),
            transitions: 0,
            evaluations: 0,
            total_reward: 0.0,
            counter: 0,
            last_source_step: 0,
            issued_events: 0,
            terminal,
        }
    }

    fn context(&self, step: u64, boot: bool) -> TypedValue {
        TypedValue::new(context_schema(), json!({
                "available": ["inc", "dec"],
                "boot": boot,
                "step": step,
            }))
                .expect("a synthetic typed value fits the contract")
    }

    fn progress_value(&self) -> TypedValue {
        TypedValue::new(progress_schema(), json!({
                "counter": self.counter,
                "transitions": self.transitions,
                "totalReward": self.total_reward,
            }))
                .expect("a synthetic typed value fits the contract")
    }

    /// The counter delta one port control asks for: `inc` adds one, `dec` subtracts one.
    ///
    /// This is the task's reading of a control, kept identical to the environment's rule so a
    /// reward describes the transition the world actually took.
    pub fn delta_of(control: &PortControl) -> i64 {
        let mut delta = 0;
        for button in &control.buttons {
            if button.down {
                match button.id.as_str() {
                    "inc" => delta += 1,
                    "dec" => delta -= 1,
                    _ => {}
                }
            }
        }
        delta
    }

    fn agent_of_port(&self, port_id: &Id) -> Option<&Id> {
        self.bindings.iter().find(|b| b.port_id == *port_id).map(|b| &b.agent_id)
    }
}

impl Task for CounterTask {
    fn schema(&self) -> SchemaRef {
        progress_schema()
    }

    fn bootstrap(
        &mut self,
        initial_inspection: &Inspection,
        bindings: &[PortBinding],
    ) -> DomainResult<Bootstrap> {
        if initial_inspection.schema != inspection_schema() {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the environment's inspection schema is not the one this task reads",
            ));
        }
        self.counter = initial_inspection.integer("counter").map_err(DomainError::invalid)?;
        self.bindings = bindings.to_vec();
        self.agents = bindings.iter().map(|b| b.agent_id.clone()).collect();
        self.agents.sort();
        let contexts = self
            .agents
            .iter()
            .map(|agent| (agent.clone(), self.context(0, true)))
            .collect();
        // A bootstrap event has sourceStep 0 and carries no reward: warm-up produces no
        // gameplay outcome at all.
        let events = vec![TaskEvent {
            id: event_id(&self.epoch, 0, "bootstrap", 0),
            kind_id: id("arena.bootstrap"),
            source_step: 0,
            agent_id: None,
            payload: TypedValue::new(event_schema(), json!({"counter": self.counter}))
                    .expect("a synthetic typed value fits the contract"),
        }];
        self.issued_events += events.len() as u64;
        Ok(Bootstrap { contexts, progress: self.progress_value(), events })
    }

    fn evaluate_transition(
        &mut self,
        scope: &Scope,
        old_inspection: &Inspection,
        new_inspection: &Inspection,
        applied_controls: &[PortControl],
    ) -> DomainResult<Evaluation> {
        let old = old_inspection.integer("counter").map_err(DomainError::invalid)?;
        let new = new_inspection.integer("counter").map_err(DomainError::invalid)?;
        let source_step = scope.step + 1;
        self.evaluations += 1;
        self.transitions += 1;
        self.counter = new;

        let mut outcomes: BTreeMap<Id, AgentOutcome> = self
            .agents
            .iter()
            .map(|agent| (agent.clone(), AgentOutcome::default()))
            .collect();
        let mut events = Vec::new();
        let mut ordinal = 0u32;
        // Controls arrive in descriptor port order, so the reward order is deterministic.
        for control in applied_controls {
            let Some(agent) = self.agent_of_port(&control.port_id).cloned() else {
                return Err(DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("port {} is bound to no agent", control.port_id),
                ));
            };
            let delta = CounterTask::delta_of(control);
            let event = event_id(&self.epoch, source_step, "counter-delta", ordinal);
            ordinal += 1;
            events.push(TaskEvent {
                id: event.clone(),
                kind_id: id("arena.counter-delta"),
                source_step,
                agent_id: Some(agent.clone()),
                payload: TypedValue::new(event_schema(), json!({"delta": delta, "counter": new}))
                        .expect("a synthetic typed value fits the contract"),
            });
            let outcome = outcomes.get_mut(&agent).ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IdentityMismatch,
                    format!("port {} names agent {agent}, which is not configured", control.port_id),
                )
            })?;
            // A shipped positive-only profile would reject a negative value; this task is
            // signed on purpose, so the profile that consumes it declares signed rewards.
            outcome.rewards.push(Reward {
                event_id: event,
                rule_id: id("counter-delta"),
                value: delta as f64,
            });
            self.total_reward += delta as f64;
            // One declared stimulus when the counter moved past a multiple of five, so the
            // stimulation path is exercised without depending on reward.
            if delta != 0 && new.rem_euclid(5) == 0 {
                outcome.stimulations.push(Stimulus {
                    id: event_id(&self.epoch, source_step, "milestone", ordinal),
                    kind_id: id("arena.milestone"),
                    duration_ms: 4.0,
                });
            }
        }
        if new != old + applied_controls.iter().map(CounterTask::delta_of).sum::<i64>() {
            return Err(DomainError::new(
                ErrorCode::BackendFailure,
                "the world did not move by the batch the task read",
                MutationCertainty::Unknown,
            ));
        }

        self.last_source_step = self.last_source_step.max(source_step);
        self.issued_events += events.len() as u64;
        let next_contexts = self
            .agents
            .iter()
            .map(|agent| (agent.clone(), self.context(source_step, false)))
            .collect();
        let terminal = match self.terminal {
            Terminal::Never => false,
            Terminal::Counter(target) => new >= target,
            Terminal::AfterTransitions(n) | Terminal::RollbackAfterTransitions(n) => {
                self.transitions >= n
            }
        };
        let rollback = matches!(self.terminal, Terminal::RollbackAfterTransitions(_));
        // The synthetic task asks for the end of the episode; `rollback` belongs to a composition
        // that declares a rollback policy (workers-v1 section 4, 2026-09-23), and the synthetic
        // composition declares none, so asking for one is how a test meets the refusal.
        let episode = terminal.then(|| {
            if rollback {
                EpisodeRequest {
                    kind: EpisodeRequestKind::Rollback,
                    reason: id("stall"),
                    outcome: fly_session_types::gameboy::RollbackRequest {
                        slot_id: "best".to_owned(),
                        trigger: "stall".to_owned(),
                    }
                    .to_typed(),
                }
            } else {
                EpisodeRequest {
                    kind: EpisodeRequestKind::Terminal,
                    reason: id("counter-target"),
                    outcome: TypedValue::new(
                        episode_schema(),
                        json!({"counter": new, "transitions": self.transitions}),
                    )
                    .expect("a synthetic typed value fits the contract"),
                }
            }
        });
        Ok(Evaluation {
            outcomes,
            next_contexts,
            progress: self.progress_value(),
            events,
            episode,
            slot_saves: Vec::new(),
        })
    }

    fn progress(&self) -> TypedValue {
        self.progress_value()
    }

    fn evaluations(&self) -> u64 {
        self.evaluations
    }

    fn capture(&self) -> DomainResult<TypedValue> {
        TypedValue::new(
            ledger_schema(),
            json!({
                "epoch": self.epoch.as_str(),
                "agents": self.agents.iter().map(String::as_str).collect::<Vec<_>>(),
                "bindings": self
                    .bindings
                    .iter()
                    .map(|b| json!({"portId": b.port_id.as_str(), "agentId": b.agent_id.as_str()}))
                    .collect::<Vec<_>>(),
                "transitions": self.transitions,
                "evaluations": self.evaluations,
                "totalReward": self.total_reward,
                "counter": self.counter,
                "lastSourceStep": self.last_source_step,
                "issuedEvents": self.issued_events,
            }),
        )
        .map_err(|e| DomainError::invalid(e.0))
    }

    fn validate_restore(&self, state: &TypedValue) -> DomainResult<()> {
        if state.schema != ledger_schema() {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the captured ledger does not carry this task's schema",
            ));
        }
        for field in [
            "epoch",
            "agents",
            "bindings",
            "transitions",
            "evaluations",
            "totalReward",
            "counter",
            "lastSourceStep",
            "issuedEvents",
        ] {
            if state.value.get(field).is_none() {
                return Err(DomainError::before(
                    ErrorCode::IncompatibleState,
                    format!("the captured ledger has no {field}"),
                ));
            }
        }
        let bindings = state
            .value
            .get("bindings")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "the captured ledger's bindings are not a list",
                )
            })?;
        if bindings.len() != self.bindings.len() && !self.bindings.is_empty() {
            return Err(DomainError::before(
                ErrorCode::IncompatibleState,
                "the captured ledger binds another number of ports",
            ));
        }
        Ok(())
    }

    fn install_restore(&mut self, epoch: &Id, state: &TypedValue) -> DomainResult<()> {
        self.validate_restore(state)?;
        let number = |key: &str| -> DomainResult<u64> {
            state.value.get(key).and_then(Value::as_u64).ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    format!("the captured ledger's {key} is not a whole number"),
                )
            })
        };
        let mut agents = Vec::new();
        for value in state.value["agents"].as_array().expect("validated") {
            let agent = value.as_str().ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "the captured ledger names an agent that is not a string",
                )
            })?;
            agents.push(parse_id(agent).map_err(|e| {
                DomainError::before(ErrorCode::IncompatibleState, format!("ledger: {e}"))
            })?);
        }
        let mut bindings = Vec::new();
        for value in state.value["bindings"].as_array().expect("validated") {
            let port_id = value.get("portId").and_then(Value::as_str).ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "the captured ledger has a binding with no portId",
                )
            })?;
            let agent_id = value.get("agentId").and_then(Value::as_str).ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "the captured ledger has a binding with no agentId",
                )
            })?;
            bindings.push(PortBinding {
                port_id: parse_id(port_id).map_err(|e| {
                    DomainError::before(ErrorCode::IncompatibleState, format!("ledger: {e}"))
                })?,
                agent_id: parse_id(agent_id).map_err(|e| {
                    DomainError::before(ErrorCode::IncompatibleState, format!("ledger: {e}"))
                })?,
            });
        }
        let counter = state.value.get("counter").and_then(Value::as_i64).ok_or_else(|| {
            DomainError::before(
                ErrorCode::IncompatibleState,
                "the captured ledger's counter is not an integer",
            )
        })?;
        let total_reward = state
            .value
            .get("totalReward")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .ok_or_else(|| {
                DomainError::before(
                    ErrorCode::IncompatibleState,
                    "the captured ledger's totalReward is not a finite number",
                )
            })?;
        // The epoch is the caller's, not the capture's: event identity belongs to the epoch
        // the ledger is being installed into.
        self.epoch = epoch.clone();
        self.agents = agents;
        self.bindings = bindings;
        self.transitions = number("transitions")?;
        self.evaluations = number("evaluations")?;
        self.total_reward = total_reward;
        self.counter = counter;
        self.last_source_step = number("lastSourceStep")?;
        self.issued_events = number("issuedEvents")?;
        Ok(())
    }

    fn rebase_ids(&self, to_epoch: &Id) -> DomainResult<BTreeMap<Id, Id>> {
        let mut out = BTreeMap::new();
        out.insert(
            event_id(&self.epoch, 0, "bootstrap", 0),
            event_id(to_epoch, 0, "bootstrap", 0),
        );
        // The counter task issues exactly one `counter-delta` event per bound port per
        // evaluated transition, in descriptor port order, so every identity it has ever
        // issued is re-derivable from its ledger without keeping a list of them.
        let ports = self.bindings.len() as u32;
        for source_step in 1..=self.last_source_step {
            for ordinal in 0..ports {
                out.insert(
                    event_id(&self.epoch, source_step, "counter-delta", ordinal),
                    event_id(to_epoch, source_step, "counter-delta", ordinal),
                );
            }
        }
        Ok(out)
    }

    fn event_watermarks(&self) -> (u64, u64) {
        (self.last_source_step, self.issued_events)
    }
}

/// The inspection value the counter environment publishes.
pub fn inspection(counter: i64, boundary: u64) -> TypedValue {
    let mut value = Map::new();
    value.insert("counter".into(), counter.into());
    value.insert("boundary".into(), boundary.into());
    TypedValue::new(inspection_schema(), Value::Object(value))
        .expect("the arena inspection fits the contract")
}
