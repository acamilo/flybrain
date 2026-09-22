//! The trace format of `step-v1` section 8, split into behaviour and operational metadata.
//!
//! Behaviour is what two runs of the same composition must agree on whatever order their
//! messages took. Operational metadata -- request ids, batch ids, bus call ids, wall time --
//! is recorded but excluded from that comparison, which is exactly what section 8 asks for.

use serde::Serialize;

use super::methods::{Reward, Stimulus};
use super::scalars::{Digest, Id, RationalNs, Scope, U64};

/// One agent's contribution to one transition.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTransitionTrace {
    pub agent_id: Id,
    pub profile_digest: Digest,
    pub ticks_advanced: U64,
    pub brain_ticks: U64,
    pub remainder: RationalNs,
    pub decision_digest: Digest,
    pub committed_step: U64,
    pub rewards: Vec<Reward>,
    pub stimulations: Vec<Stimulus>,
    /// Operational: the `req-<U64>` of this agent's Prepare.
    #[serde(skip_serializing)]
    pub prepare_request_id: Id,
    /// Operational: the `req-<U64>` of this agent's Commit.
    #[serde(skip_serializing)]
    pub commit_request_id: Id,
}

/// The producing boundary of one observation view.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewProvenance {
    pub view_id: Id,
    pub produced_step: U64,
}

/// One complete transition `k -> k+1`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionTrace {
    pub scope: Scope,
    /// Agents in sorted agent-id order, never completion order.
    pub agents: Vec<AgentTransitionTrace>,
    pub controls_digest: Digest,
    pub acknowledged_boundary: U64,
    pub observation_boundaries: Vec<ViewProvenance>,
    pub task_event_ids: Vec<Id>,
    pub published_boundary: U64,
    /// Operational: the complete batch's id.
    #[serde(skip_serializing)]
    pub batch_id: Id,
    /// Operational: the `req-<U64>` of the single Environment.Advance.
    #[serde(skip_serializing)]
    pub advance_request_id: Id,
}

impl TransitionTrace {
    /// The canonical behaviour encoding: everything a reordered run must reproduce exactly.
    pub fn behavior(&self) -> String {
        let value = serde_json::to_value(self).expect("a trace serializes");
        super::canonical::canonical_json(&value)
    }

    /// Operational identities, for a report rather than a comparison.
    pub fn operational(&self) -> Vec<(String, String)> {
        let mut out = vec![
            ("batchId".to_owned(), self.batch_id.to_string()),
            ("advanceRequestId".to_owned(), self.advance_request_id.to_string()),
        ];
        for agent in &self.agents {
            out.push((
                format!("prepareRequestId.{}", agent.agent_id),
                agent.prepare_request_id.to_string(),
            ));
            out.push((
                format!("commitRequestId.{}", agent.agent_id),
                agent.commit_request_id.to_string(),
            ));
        }
        out
    }
}

/// One session phase transition, recorded whether or not it ends a step.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseTransition {
    pub from: String,
    pub to: String,
}

/// Everything a run recorded: its phase transitions and its completed transitions.
#[derive(Clone, Debug, Default)]
pub struct TraceLog {
    pub phases: Vec<PhaseTransition>,
    pub transitions: Vec<TransitionTrace>,
}

impl TraceLog {
    pub fn phase(&mut self, from: String, to: String) {
        self.phases.push(PhaseTransition { from, to });
    }

    pub fn transition(&mut self, trace: TransitionTrace) {
        self.transitions.push(trace);
    }

    /// The behaviour of every transition, in order, with operational metadata excluded.
    pub fn behavior(&self) -> Vec<String> {
        self.transitions.iter().map(TransitionTrace::behavior).collect()
    }

    /// The phase path, as `from -> to` strings.
    pub fn phase_path(&self) -> Vec<String> {
        self.phases.iter().map(|p| format!("{} -> {}", p.from, p.to)).collect()
    }
}
