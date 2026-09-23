//! The trace format of step-v1 section 8, split into behaviour and operational metadata.
//!
//! Section 8 requires a record, for every transition, of the scope, the Prepare request ids,
//! the agent/profile ids, tick counts and remainders, decision digests, the complete batch id
//! and control digest, the acknowledged world boundary, observation producing boundaries, task
//! event/outcome ids in order, every Commit acknowledgment and the published boundary. It then
//! requires that sequential, concurrent and reversed runs "match, excluding wall time, request
//! ids and other explicitly operational metadata".
//!
//! So this record has two halves. [`TraceBehaviour`] is what must match: it is ordered by
//! agent id rather than by completion order, so a reversed dispatch produces an identical
//! value. [`TraceOperational`] is what section 8 requires recording but excludes from the
//! comparison: wall time, the domain request ids, the bus callIds and the delivery ids.
//! [`TransitionTrace::behaviour_equals`] compares only the first half, and
//! [`TransitionTrace::behaviour_diff`] names the fields that differ.

use flybus::wire::Fields;
use serde_json::Value;

use crate::canonical;
use crate::scalar::{
    BusCallId, DomainRequestId, DomainType, OwnerToken, RationalNs, Result, Scope, err, is_digest,
    is_id, list, obj, require_unique, u64_json,
};
use crate::workers::{MAX_AGENTS, MAX_RATE_ROLES};

/// One agent's behaviour in one transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceAgent {
    pub agent_id: String,
    pub profile_digest: String,
    pub ticks_advanced: u64,
    pub brain_ticks: u64,
    pub remainder: RationalNs,
    pub decision_digest: String,
    /// The boundary this agent acknowledged in its Commit reply.
    pub committed_step: u64,
}

/// One view's producing boundary, as observed in this transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceObservation {
    pub view_id: String,
    pub produced_step: u64,
}

/// The kind of an action taken at the boundary a transition reached, after all of its commits
/// (amendment of 2026-09-23, RT-01a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryActionKind {
    SaveSlot,
    Rollback,
}

impl BoundaryActionKind {
    pub const ALL: &'static [&'static str] = &["save-slot", "rollback"];

    pub fn as_str(self) -> &'static str {
        match self {
            BoundaryActionKind::SaveSlot => "save-slot",
            BoundaryActionKind::Rollback => "rollback",
        }
    }

    pub fn parse(s: &str) -> Result<BoundaryActionKind> {
        match s {
            "save-slot" => Ok(BoundaryActionKind::SaveSlot),
            "rollback" => Ok(BoundaryActionKind::Rollback),
            _ => err("boundary action kind must be save-slot or rollback"),
        }
    }
}

/// One boundary action, in the order the coordinator applied it: an `Environment.SaveSlot`
/// (with the saved state's digest) or a rollback to a slot (no digest).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryAction {
    pub kind: BoundaryActionKind,
    pub slot_id: String,
    pub state_digest: Option<String>,
}

/// The fields two runs of the same transition must agree on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceBehaviour {
    pub scope: Scope,
    /// Sorted by agent id, never by completion order.
    pub agents: Vec<TraceAgent>,
    pub batch_id: String,
    pub control_digest: String,
    pub acknowledged_boundary: u64,
    /// Sorted by view id.
    pub observation_boundaries: Vec<TraceObservation>,
    /// Task outcome ids in task order.
    pub outcome_ids: Vec<String>,
    /// Task event ids in task order.
    pub event_ids: Vec<String>,
    pub published_boundary: u64,
    /// Slot saves and a rollback at the reached boundary, in application order. Empty for a
    /// composition without them.
    pub boundary_actions: Vec<BoundaryAction>,
}

impl TraceBehaviour {
    /// Sorts the order-free collections, so a trace recorded in completion order compares
    /// equal to one recorded in dispatch order.
    pub fn normalized(&self) -> TraceBehaviour {
        let mut out = self.clone();
        out.agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        out.observation_boundaries
            .sort_by(|a, b| a.view_id.cmp(&b.view_id));
        out
    }

    pub fn digest(&self) -> Result<String> {
        canonical::digest_of(&self.normalized().to_json())
    }
}

impl DomainType for TraceBehaviour {
    const TYPE_NAME: &'static str = "TraceBehaviour";

    fn from_json(value: &Value) -> Result<TraceBehaviour> {
        let mut f = Fields::new(value, "TraceBehaviour")?;
        let scope = Scope::from_json(f.value("scope")?)?;
        let agents = list(&mut f, "agents", 1, MAX_AGENTS, |v| {
            let mut a = Fields::new(v, "TraceBehaviour.agents")?;
            let agent_id = a.id("agentId")?;
            let profile_digest = a.string("profileDigest")?.to_owned();
            let ticks_advanced = a.u64_string("ticksAdvanced")?;
            let brain_ticks = a.u64_string("brainTicks")?;
            let remainder = RationalNs::from_json(a.value("remainder")?)?;
            let decision_digest = a.string("decisionDigest")?.to_owned();
            let committed_step = a.u64_string("committedStep")?;
            a.finish()?;
            Ok(TraceAgent {
                agent_id,
                profile_digest,
                ticks_advanced,
                brain_ticks,
                remainder,
                decision_digest,
                committed_step,
            })
        })?;
        let batch_id = f.id("batchId")?;
        let control_digest = f.string("controlDigest")?.to_owned();
        let acknowledged_boundary = f.u64_string("acknowledgedBoundary")?;
        let observation_boundaries = list(
            &mut f,
            "observationBoundaries",
            0,
            crate::media::MAX_VIEWS * 2,
            |v| {
                let mut o = Fields::new(v, "TraceBehaviour.observationBoundaries")?;
                let view_id = o.id("viewId")?;
                let produced_step = o.u64_string("producedStep")?;
                o.finish()?;
                Ok(TraceObservation {
                    view_id,
                    produced_step,
                })
            },
        )?;
        let outcome_ids = crate::scalar::id_list(&mut f, "outcomeIds", 0, MAX_RATE_ROLES)?;
        let event_ids = crate::scalar::id_list(&mut f, "eventIds", 0, MAX_RATE_ROLES)?;
        let published_boundary = f.u64_string("publishedBoundary")?;
        let boundary_actions = list(&mut f, "boundaryActions", 0, MAX_BOUNDARY_ACTIONS, |v| {
            let mut a = Fields::new(v, "TraceBehaviour.boundaryActions")?;
            let kind = BoundaryActionKind::parse(a.string("kind")?)?;
            let slot_id = a.id("slotId")?;
            let state_digest = match a.value("stateDigest")? {
                Value::Null => None,
                Value::String(d) => Some(d.clone()),
                _ => return err("stateDigest must be null or a digest"),
            };
            a.finish()?;
            Ok(BoundaryAction {
                kind,
                slot_id,
                state_digest,
            })
        })?;
        f.finish()?;
        let b = TraceBehaviour {
            scope,
            agents,
            batch_id,
            control_digest,
            acknowledged_boundary,
            observation_boundaries,
            outcome_ids,
            event_ids,
            published_boundary,
            boundary_actions,
        };
        b.validate()?;
        Ok(b)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("scope", self.scope.to_json()),
            (
                "agents",
                Value::Array(
                    self.agents
                        .iter()
                        .map(|a| {
                            obj(vec![
                                ("agentId", a.agent_id.clone().into()),
                                ("profileDigest", a.profile_digest.clone().into()),
                                ("ticksAdvanced", u64_json(a.ticks_advanced)),
                                ("brainTicks", u64_json(a.brain_ticks)),
                                ("remainder", a.remainder.to_json()),
                                ("decisionDigest", a.decision_digest.clone().into()),
                                ("committedStep", u64_json(a.committed_step)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("batchId", self.batch_id.clone().into()),
            ("controlDigest", self.control_digest.clone().into()),
            ("acknowledgedBoundary", u64_json(self.acknowledged_boundary)),
            (
                "observationBoundaries",
                Value::Array(
                    self.observation_boundaries
                        .iter()
                        .map(|o| {
                            obj(vec![
                                ("viewId", o.view_id.clone().into()),
                                ("producedStep", u64_json(o.produced_step)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "outcomeIds",
                Value::Array(self.outcome_ids.iter().map(|i| i.clone().into()).collect()),
            ),
            (
                "eventIds",
                Value::Array(self.event_ids.iter().map(|i| i.clone().into()).collect()),
            ),
            ("publishedBoundary", u64_json(self.published_boundary)),
            (
                "boundaryActions",
                Value::Array(
                    self.boundary_actions
                        .iter()
                        .map(|a| {
                            obj(vec![
                                ("kind", a.kind.as_str().into()),
                                ("slotId", a.slot_id.clone().into()),
                                (
                                    "stateDigest",
                                    a.state_digest.clone().map_or(Value::Null, Value::String),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        if self.agents.is_empty() || self.agents.len() > MAX_AGENTS {
            return err("TraceBehaviour: 1..=4 agents");
        }
        require_unique(
            self.agents.iter().map(|a| a.agent_id.as_str()),
            "TraceBehaviour.agents",
        )?;
        for agent in &self.agents {
            if !is_id(&agent.agent_id) {
                return err("TraceBehaviour: agentId is not a valid id");
            }
            if !is_digest(&agent.profile_digest) || !is_digest(&agent.decision_digest) {
                return err("TraceBehaviour: agent digests must be 64 lowercase hex digits");
            }
            agent.remainder.validate()?;
            if agent.committed_step != self.scope.step + 1 {
                return err(
                    "TraceBehaviour: every commit acknowledgment is the transition's next boundary",
                );
            }
        }
        if !is_id(&self.batch_id) {
            return err("TraceBehaviour: batchId is not a valid id");
        }
        if !is_digest(&self.control_digest) {
            return err("TraceBehaviour: controlDigest must be 64 lowercase hex digits");
        }
        if self.acknowledged_boundary != self.scope.step + 1 {
            return err("TraceBehaviour: the acknowledged boundary is scope.step + 1");
        }
        if self.published_boundary != self.acknowledged_boundary {
            return err(
                "TraceBehaviour: the published boundary is the boundary every agent committed",
            );
        }
        require_unique(
            self.observation_boundaries
                .iter()
                .map(|o| o.view_id.as_str()),
            "TraceBehaviour.observationBoundaries",
        )?;
        require_unique(
            self.event_ids.iter().map(String::as_str),
            "TraceBehaviour.eventIds",
        )?;
        require_unique(
            self.outcome_ids.iter().map(String::as_str),
            "TraceBehaviour.outcomeIds",
        )?;
        self.validate_boundary_actions()
    }
}

/// Boundary actions per transition: every slot at most once, plus one rollback.
pub const MAX_BOUNDARY_ACTIONS: usize = crate::extensions::MAX_SLOTS + 1;

impl TraceBehaviour {
    /// Slot saves come first, each slot at most once, with the saved state's digest; at most
    /// one rollback, last, naming a slot and no digest (step-v1 section 6 amendment).
    fn validate_boundary_actions(&self) -> Result<()> {
        let mut rolled_back = false;
        let mut saved: Vec<&str> = Vec::new();
        for action in &self.boundary_actions {
            if !is_id(&action.slot_id) {
                return err("TraceBehaviour: boundary action slotId is not a valid id");
            }
            if rolled_back {
                return err("TraceBehaviour: nothing follows a rollback at the same boundary");
            }
            match action.kind {
                BoundaryActionKind::SaveSlot => {
                    match &action.state_digest {
                        Some(d) if is_digest(d) => {}
                        _ => return err("TraceBehaviour: a slot save records its state digest"),
                    }
                    if saved.contains(&action.slot_id.as_str()) {
                        return err("TraceBehaviour: a slot is saved at most once per boundary");
                    }
                    saved.push(&action.slot_id);
                }
                BoundaryActionKind::Rollback => {
                    if action.state_digest.is_some() {
                        return err("TraceBehaviour: a rollback records no state digest");
                    }
                    rolled_back = true;
                }
            }
        }
        Ok(())
    }

    /// How many leading boundary actions are slot saves.
    pub fn slot_saves(&self) -> usize {
        self.boundary_actions
            .iter()
            .take_while(|a| a.kind == BoundaryActionKind::SaveSlot)
            .count()
    }
}

/// One agent's domain request id for one phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceRequest {
    pub agent_id: String,
    pub request_id: DomainRequestId,
}

/// What step-v1 section 8 records but excludes from the comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceOperational {
    /// Wall time is for pacing, health and presentation only (step-v1 section 5).
    pub wall_time_ns: u64,
    pub prepare_request_ids: Vec<TraceRequest>,
    pub advance_request_id: DomainRequestId,
    pub commit_request_ids: Vec<TraceRequest>,
    /// The transport correlation ids this transition happened to use. A safe retry changes
    /// these and nothing in [`TraceBehaviour`].
    pub bus_call_ids: Vec<BusCallId>,
    pub delivery_ids: Vec<OwnerToken>,
    /// Checkpoint captures (and FLYSIM01 exports) taken at the reached boundary, each with the
    /// number of boundary actions already applied when it was taken. Operational because a
    /// capture's schedule is wall-clock policy; the ordering rule against slot saves is checked
    /// by [`TransitionTrace`] (amendment of 2026-09-23, legacy-gameboy-v1 section 16).
    pub captures: Vec<TraceCapture>,
}

/// One checkpoint capture at the reached boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceCapture {
    pub checkpoint_id: String,
    pub after_actions: u64,
}

impl DomainType for TraceOperational {
    const TYPE_NAME: &'static str = "TraceOperational";

    fn from_json(value: &Value) -> Result<TraceOperational> {
        let mut f = Fields::new(value, "TraceOperational")?;
        let wall_time_ns = f.u64_string("wallTimeNs")?;
        let read_requests = |v: &Value| -> Result<TraceRequest> {
            let mut r = Fields::new(v, "TraceOperational request")?;
            let agent_id = r.id("agentId")?;
            let request_id = DomainRequestId::read(&mut r, "requestId")?;
            r.finish()?;
            Ok(TraceRequest {
                agent_id,
                request_id,
            })
        };
        let prepare_request_ids = list(&mut f, "prepareRequestIds", 1, MAX_AGENTS, read_requests)?;
        let advance_request_id = DomainRequestId::read(&mut f, "advanceRequestId")?;
        let commit_request_ids = list(&mut f, "commitRequestIds", 1, MAX_AGENTS, read_requests)?;
        let bus_call_ids = list(&mut f, "busCallIds", 0, 64, |v| match v.as_str() {
            Some(s) => BusCallId::parse(s),
            None => err("every busCallId must be a string"),
        })?;
        let delivery_ids = list(&mut f, "deliveryIds", 0, 64, |v| match v.as_str() {
            Some(s) => OwnerToken::parse(s),
            None => err("every deliveryId must be a string"),
        })?;
        let captures = list(&mut f, "captures", 0, MAX_BOUNDARY_ACTIONS + 1, |v| {
            let mut c = Fields::new(v, "TraceOperational.captures")?;
            let checkpoint_id = c.id("checkpointId")?;
            let after_actions = c.int("afterActions", 0, MAX_BOUNDARY_ACTIONS as u64)?;
            c.finish()?;
            Ok(TraceCapture {
                checkpoint_id,
                after_actions,
            })
        })?;
        f.finish()?;
        let o = TraceOperational {
            wall_time_ns,
            prepare_request_ids,
            advance_request_id,
            commit_request_ids,
            bus_call_ids,
            delivery_ids,
            captures,
        };
        o.validate()?;
        Ok(o)
    }

    fn to_json(&self) -> Value {
        let requests = |items: &[TraceRequest]| {
            Value::Array(
                items
                    .iter()
                    .map(|r| {
                        obj(vec![
                            ("agentId", r.agent_id.clone().into()),
                            ("requestId", r.request_id.to_json()),
                        ])
                    })
                    .collect(),
            )
        };
        obj(vec![
            ("wallTimeNs", u64_json(self.wall_time_ns)),
            ("prepareRequestIds", requests(&self.prepare_request_ids)),
            ("advanceRequestId", self.advance_request_id.to_json()),
            ("commitRequestIds", requests(&self.commit_request_ids)),
            (
                "busCallIds",
                Value::Array(self.bus_call_ids.iter().map(BusCallId::to_json).collect()),
            ),
            (
                "deliveryIds",
                Value::Array(
                    self.delivery_ids
                        .iter()
                        .map(|t| Value::String(t.as_str().to_owned()))
                        .collect(),
                ),
            ),
            (
                "captures",
                Value::Array(
                    self.captures
                        .iter()
                        .map(|c| {
                            obj(vec![
                                ("checkpointId", c.checkpoint_id.clone().into()),
                                ("afterActions", Value::from(c.after_actions)),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        require_unique(
            self.prepare_request_ids.iter().map(|r| r.agent_id.as_str()),
            "TraceOperational.prepareRequestIds",
        )?;
        require_unique(
            self.commit_request_ids.iter().map(|r| r.agent_id.as_str()),
            "TraceOperational.commitRequestIds",
        )?;
        require_unique(
            self.bus_call_ids.iter().map(BusCallId::as_str),
            "TraceOperational.busCallIds",
        )?;
        require_unique(
            self.delivery_ids.iter().map(OwnerToken::as_str),
            "TraceOperational.deliveryIds",
        )?;
        require_unique(
            self.captures.iter().map(|c| c.checkpoint_id.as_str()),
            "TraceOperational.captures",
        )?;
        let mut last = 0;
        for capture in &self.captures {
            if capture.after_actions < last {
                return err("TraceOperational: captures are recorded in the order they were taken");
            }
            last = capture.after_actions;
        }
        Ok(())
    }
}

/// One transition's trace: behaviour plus operational metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionTrace {
    pub behaviour: TraceBehaviour,
    pub operational: TraceOperational,
}

impl TransitionTrace {
    /// Behaviour equality: the comparison step-v1 section 8 asks for.
    pub fn behaviour_equals(&self, other: &TransitionTrace) -> bool {
        self.behaviour.normalized() == other.behaviour.normalized()
    }

    /// The behaviour fields that differ, named. Empty when [`Self::behaviour_equals`] holds.
    pub fn behaviour_diff(&self, other: &TransitionTrace) -> Vec<String> {
        let (a, b) = (self.behaviour.normalized(), other.behaviour.normalized());
        let mut out = Vec::new();
        if a.scope != b.scope {
            out.push(format!("scope: {:?} vs {:?}", a.scope, b.scope));
        }
        if a.batch_id != b.batch_id {
            out.push(format!("batchId: {} vs {}", a.batch_id, b.batch_id));
        }
        if a.control_digest != b.control_digest {
            out.push("controlDigest differs".to_owned());
        }
        if a.acknowledged_boundary != b.acknowledged_boundary {
            out.push(format!(
                "acknowledgedBoundary: {} vs {}",
                a.acknowledged_boundary, b.acknowledged_boundary
            ));
        }
        if a.published_boundary != b.published_boundary {
            out.push(format!(
                "publishedBoundary: {} vs {}",
                a.published_boundary, b.published_boundary
            ));
        }
        if a.observation_boundaries != b.observation_boundaries {
            out.push("observationBoundaries differ".to_owned());
        }
        if a.outcome_ids != b.outcome_ids {
            out.push("outcomeIds differ".to_owned());
        }
        if a.event_ids != b.event_ids {
            out.push("eventIds differ".to_owned());
        }
        if a.boundary_actions != b.boundary_actions {
            out.push("boundaryActions differ".to_owned());
        }
        let ids_a: Vec<&str> = a.agents.iter().map(|x| x.agent_id.as_str()).collect();
        let ids_b: Vec<&str> = b.agents.iter().map(|x| x.agent_id.as_str()).collect();
        if ids_a != ids_b {
            out.push(format!(
                "agents: [{}] vs [{}]",
                ids_a.join(", "),
                ids_b.join(", ")
            ));
        } else {
            for (left, right) in a.agents.iter().zip(&b.agents) {
                if left != right {
                    out.push(format!("agent {}: behaviour differs", left.agent_id));
                }
            }
        }
        out
    }

    /// Two whole runs agree on behaviour, transition by transition.
    pub fn runs_equal(left: &[TransitionTrace], right: &[TransitionTrace]) -> bool {
        left.len() == right.len() && left.iter().zip(right).all(|(a, b)| a.behaviour_equals(b))
    }
}

impl DomainType for TransitionTrace {
    const TYPE_NAME: &'static str = "TransitionTrace";

    fn from_json(value: &Value) -> Result<TransitionTrace> {
        let mut f = Fields::new(value, "TransitionTrace")?;
        let behaviour = TraceBehaviour::from_json(f.value("behaviour")?)?;
        let operational = TraceOperational::from_json(f.value("operational")?)?;
        f.finish()?;
        let t = TransitionTrace {
            behaviour,
            operational,
        };
        t.validate()?;
        Ok(t)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("behaviour", self.behaviour.to_json()),
            ("operational", self.operational.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        self.behaviour.validate()?;
        self.operational.validate()?;
        let behaviour_agents: Vec<&str> = self
            .behaviour
            .agents
            .iter()
            .map(|a| a.agent_id.as_str())
            .collect();
        for phase in [
            &self.operational.prepare_request_ids,
            &self.operational.commit_request_ids,
        ] {
            for request in phase {
                if !behaviour_agents.contains(&request.agent_id.as_str()) {
                    return err(format!(
                        "TransitionTrace: request recorded for {:?}, which is not in the transition",
                        request.agent_id
                    ));
                }
            }
        }
        // A slot save due at a boundary completes before any capture at that boundary, so a
        // checkpoint never pairs a task ledger that names the new slot with the old slot
        // contents (legacy-gameboy-v1 section 16, review round 1 of 2026-09-23).
        let saves = self.behaviour.slot_saves() as u64;
        let actions = self.behaviour.boundary_actions.len() as u64;
        for capture in &self.operational.captures {
            if capture.after_actions < saves {
                return err(format!(
                    "TransitionTrace: capture {:?} was taken before this boundary's slot saves completed",
                    capture.checkpoint_id
                ));
            }
            if capture.after_actions > actions {
                return err(format!(
                    "TransitionTrace: capture {:?} counts more boundary actions than were applied",
                    capture.checkpoint_id
                ));
            }
        }
        Ok(())
    }
}
