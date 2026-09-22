//! Coordinator-local task and executor interfaces (`workers-v1` section 4), plus the
//! deterministic counter task and the identity executor the synthetic composition uses.
//!
//! These are library interfaces, not extra bus services. The task interprets inspection data
//! and asks for outcomes; the executor translates a selected decision using read-only current
//! game state and task progress; the coordinator orders and applies the results. Nothing here
//! writes a controller or neural state directly.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::types::{
    AgentOutcome, ControllerIntent, DomainError, DomainResult, EpisodeKind, EpisodeRequest,
    ErrorCode, Id, PortBinding, PortControl, Reward, SchemaRef, Scope, Stimulus, TaskEvent,
    TypedValue, U64, event_id,
};

/// The schemas the synthetic arena composition registers.
pub fn inspection_schema() -> SchemaRef {
    SchemaRef::synthetic("arena.inspection.v1", 1)
}

pub fn decision_schema() -> SchemaRef {
    SchemaRef::synthetic("arena.decision.v1", 1)
}

pub fn context_schema() -> SchemaRef {
    SchemaRef::synthetic("arena.context.v1", 1)
}

pub fn progress_schema() -> SchemaRef {
    SchemaRef::synthetic("arena.progress.v1", 1)
}

pub fn event_schema() -> SchemaRef {
    SchemaRef::synthetic("arena.event.v1", 1)
}

pub fn episode_schema() -> SchemaRef {
    SchemaRef::synthetic("arena.episode.v1", 1)
}

pub fn controller_schema_ref() -> SchemaRef {
    SchemaRef::synthetic("arena.controller.v1", 1)
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
}

/// A checkpointable task ledger and the two evaluation entry points.
pub trait Task: Send {
    fn schema(&self) -> SchemaRef;

    /// Called once, at boundary 0, before any agent is initialized.
    fn bootstrap(
        &mut self,
        initial_inspection: &TypedValue,
        bindings: &[PortBinding],
    ) -> DomainResult<Bootstrap>;

    /// Called exactly once per acknowledged world step, never against a later observation.
    fn evaluate_transition(
        &mut self,
        scope: &Scope,
        old_inspection: &TypedValue,
        new_inspection: &TypedValue,
        applied_controls: &[PortControl],
    ) -> DomainResult<Evaluation>;

    fn progress(&self) -> TypedValue;

    /// How many times `evaluate_transition` has run. A transition must evaluate once.
    fn evaluations(&self) -> u64;
}

/// Translates one selected decision into a controller intent, with no port assignment.
pub trait ActionExecutor: Send {
    fn apply(
        &mut self,
        scope: &Scope,
        decision: &TypedValue,
        current_game_state: &TypedValue,
        progress: &TypedValue,
        clock: &crate::types::RationalNs,
    ) -> DomainResult<(ControllerIntent, Vec<TaskEvent>)>;
}

/// The only executor v1 supports: it passes a direct-control decision through unchanged.
#[derive(Clone, Debug, Default)]
pub struct IdentityExecutor;

impl ActionExecutor for IdentityExecutor {
    fn apply(
        &mut self,
        _scope: &Scope,
        decision: &TypedValue,
        _current_game_state: &TypedValue,
        _progress: &TypedValue,
        _clock: &crate::types::RationalNs,
    ) -> DomainResult<(ControllerIntent, Vec<TaskEvent>)> {
        if decision.schema != decision_schema() {
            return Err(DomainError::before(
                ErrorCode::IdentityMismatch,
                "the decision does not carry the profile's registered intent schema",
            ));
        }
        let intent: ControllerIntent =
            serde_json::from_value(Value::Object(decision.value.clone()))
                .map_err(|e| DomainError::invalid(format!("decision: {e}")))?;
        Ok((intent, Vec::new()))
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
            terminal,
        }
    }

    fn context(&self, step: u64, boot: bool) -> TypedValue {
        TypedValue::new(
            context_schema(),
            match json!({
                "available": ["inc", "dec"],
                "boot": boot,
                "step": step,
            }) {
                Value::Object(m) => m,
                _ => unreachable!(),
            },
        )
    }

    fn progress_value(&self) -> TypedValue {
        TypedValue::new(
            progress_schema(),
            match json!({
                "counter": self.counter,
                "transitions": self.transitions,
                "totalReward": self.total_reward,
            }) {
                Value::Object(m) => m,
                _ => unreachable!(),
            },
        )
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
        initial_inspection: &TypedValue,
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
            kind_id: Id::lit("arena.bootstrap"),
            source_step: U64(0),
            agent_id: None,
            payload: TypedValue::new(
                event_schema(),
                match json!({"counter": self.counter}) {
                    Value::Object(m) => m,
                    _ => unreachable!(),
                },
            ),
        }];
        Ok(Bootstrap { contexts, progress: self.progress_value(), events })
    }

    fn evaluate_transition(
        &mut self,
        scope: &Scope,
        old_inspection: &TypedValue,
        new_inspection: &TypedValue,
        applied_controls: &[PortControl],
    ) -> DomainResult<Evaluation> {
        let old = old_inspection.integer("counter").map_err(DomainError::invalid)?;
        let new = new_inspection.integer("counter").map_err(DomainError::invalid)?;
        let source_step = scope.step.0 + 1;
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
            let id = event_id(&self.epoch, source_step, "counter-delta", ordinal);
            ordinal += 1;
            events.push(TaskEvent {
                id: id.clone(),
                kind_id: Id::lit("arena.counter-delta"),
                source_step: U64(source_step),
                agent_id: Some(agent.clone()),
                payload: TypedValue::new(
                    event_schema(),
                    match json!({"delta": delta, "counter": new}) {
                        Value::Object(m) => m,
                        _ => unreachable!(),
                    },
                ),
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
                event_id: id,
                rule_id: Id::lit("counter-delta"),
                value: delta as f64,
            });
            self.total_reward += delta as f64;
            // One declared stimulus when the counter moved past a multiple of five, so the
            // stimulation path is exercised without depending on reward.
            if delta != 0 && new.rem_euclid(5) == 0 {
                outcome.stimulations.push(Stimulus {
                    id: event_id(&self.epoch, source_step, "milestone", ordinal),
                    kind_id: Id::lit("arena.milestone"),
                    duration_ms: 4.0,
                });
            }
        }
        if new != old + applied_controls.iter().map(CounterTask::delta_of).sum::<i64>() {
            return Err(DomainError::new(
                ErrorCode::BackendFailure,
                "the world did not move by the batch the task read",
                crate::types::Mutation::Unknown,
            ));
        }

        let next_contexts = self
            .agents
            .iter()
            .map(|agent| (agent.clone(), self.context(source_step, false)))
            .collect();
        let terminal = match self.terminal {
            Terminal::Never => false,
            Terminal::Counter(target) => new >= target,
            Terminal::AfterTransitions(n) => self.transitions >= n,
        };
        let episode = terminal.then(|| EpisodeRequest {
            kind: EpisodeKind::Terminal,
            reason: Id::lit("counter-target"),
            outcome: TypedValue::new(
                episode_schema(),
                match json!({"counter": new, "transitions": self.transitions}) {
                    Value::Object(m) => m,
                    _ => unreachable!(),
                },
            ),
        });
        Ok(Evaluation {
            outcomes,
            next_contexts,
            progress: self.progress_value(),
            events,
            episode,
        })
    }

    fn progress(&self) -> TypedValue {
        self.progress_value()
    }

    fn evaluations(&self) -> u64 {
        self.evaluations
    }
}

/// The inspection value the counter environment publishes.
pub fn inspection(counter: i64, boundary: u64) -> TypedValue {
    let mut value = Map::new();
    value.insert("counter".into(), counter.into());
    value.insert("boundary".into(), boundary.into());
    TypedValue::new(inspection_schema(), value)
}
