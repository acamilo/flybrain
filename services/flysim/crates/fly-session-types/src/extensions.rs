//! The extension methods of the 2026-09-23 amendments (RT-01a): environment slots and the
//! agent side of a declared rollback policy.
//!
//! `workers-v1` section 7 specifies them. They exist because the operator decided (2026-09-23)
//! that the live Game Boy fly runs on this framework rather than beside it, and its ratchet
//! rolls the game back to a saved slot while the brain continues. The shapes are generic: a
//! slot is an environment-held saved state named by an id, and a rollback is an agent
//! installing a new input and context under a new epoch without a tick. Which composition may
//! use them is a capability question, answered by `Worker.Hello`:
//!
//! - an environment that answers `Environment.SaveSlot` / `Environment.RestoreSlot` advertises
//!   [`SLOTS_CAPABILITY`];
//! - an agent that answers `Agent.Rollback` advertises [`ROLLBACK_CAPABILITY`].
//!
//! A worker that does not advertise the capability answers `UNSUPPORTED`, mutation none. Nothing
//! Game Boy specific is in these payloads: the console lives in the registered schemas of
//! [`crate::gameboy`], carried inside `TypedValue`s.

use flybus::wire::Fields;
use serde_json::Value;

use crate::scalar::{DomainType, Result, Scope, TypedValue, err, is_digest, is_id, obj, u64_json};
use crate::workers::{AgentTelemetry, SensoryInput, WorldObservation};

/// The environment capability that carries `Environment.SaveSlot` and `Environment.RestoreSlot`.
pub const SLOTS_CAPABILITY: &str = "gameboy-slots-v1";
/// The agent capability that carries `Agent.Rollback`, and the policy id it applies.
pub const ROLLBACK_CAPABILITY: &str = "legacy-ratchet-rollback-v1";
/// The only rollback policy this contract defines.
pub const ROLLBACK_POLICY: &str = "legacy-ratchet-rollback-v1";

pub const METHOD_SAVE_SLOT: &str = "Environment.SaveSlot";
pub const METHOD_RESTORE_SLOT: &str = "Environment.RestoreSlot";
pub const METHOD_AGENT_ROLLBACK: &str = "Agent.Rollback";

/// Slots one environment may hold. Not a stated bound; recorded in the schema set.
pub const MAX_SLOTS: usize = 4;

fn policy_ok(policy: &str, what: &str) -> Result<()> {
    if policy != ROLLBACK_POLICY {
        return err(format!(
            "{what}: policy must be {ROLLBACK_POLICY}, the only rollback policy defined"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Environment.SaveSlot

/// `Environment.SaveSlot` params. Scope is the committed boundary the slot records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveSlotParams {
    pub slot_id: String,
}

impl DomainType for SaveSlotParams {
    const TYPE_NAME: &'static str = "SaveSlotParams";

    fn from_json(value: &Value) -> Result<SaveSlotParams> {
        let mut f = Fields::new(value, "SaveSlotParams")?;
        let slot_id = f.id("slotId")?;
        f.finish()?;
        let p = SaveSlotParams { slot_id };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![("slotId", self.slot_id.clone().into())])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.slot_id) {
            return err("SaveSlotParams: slotId is not a valid id");
        }
        Ok(())
    }
}

/// `Environment.SaveSlot` result: which boundary the slot now holds, and the digest and length
/// of the saved state bytes, so a later restore can be tied to exactly this save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveSlotResult {
    pub slot_id: String,
    pub boundary: u64,
    pub state_digest: String,
    pub byte_length: u64,
}

impl SaveSlotResult {
    /// The slot records the committed boundary the call was scoped to.
    pub fn validate_against_scope(&self, scope: &Scope) -> Result<()> {
        self.validate()?;
        if self.boundary != scope.step {
            return err(format!(
                "SaveSlotResult: boundary {} must be the scoped committed step {}",
                self.boundary, scope.step
            ));
        }
        Ok(())
    }
}

impl DomainType for SaveSlotResult {
    const TYPE_NAME: &'static str = "SaveSlotResult";

    fn from_json(value: &Value) -> Result<SaveSlotResult> {
        let mut f = Fields::new(value, "SaveSlotResult")?;
        let slot_id = f.id("slotId")?;
        let boundary = f.u64_string("boundary")?;
        let state_digest = f.string("stateDigest")?.to_owned();
        let byte_length = f.u64_string("byteLength")?;
        f.finish()?;
        let r = SaveSlotResult {
            slot_id,
            boundary,
            state_digest,
            byte_length,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("slotId", self.slot_id.clone().into()),
            ("boundary", u64_json(self.boundary)),
            ("stateDigest", self.state_digest.clone().into()),
            ("byteLength", u64_json(self.byte_length)),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.slot_id) {
            return err("SaveSlotResult: slotId is not a valid id");
        }
        if !is_digest(&self.state_digest) {
            return err("SaveSlotResult: stateDigest must be 64 lowercase hex digits");
        }
        if self.byte_length == 0 {
            return err("SaveSlotResult: byteLength must be positive");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Environment.RestoreSlot

/// `Environment.RestoreSlot` params. Scope is the NEW epoch at the committed boundary the
/// rollback is applied at; `priorEpoch` names the epoch the environment must currently be in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreSlotParams {
    pub slot_id: String,
    pub prior_epoch: String,
    pub policy: String,
}

impl RestoreSlotParams {
    /// A rollback moves to a new epoch: the prior epoch cannot be the scoped one.
    pub fn validate_against_scope(&self, scope: &Scope) -> Result<()> {
        self.validate()?;
        if self.prior_epoch == scope.epoch {
            return err("RestoreSlotParams: priorEpoch must differ from the scoped (new) epoch");
        }
        Ok(())
    }
}

impl DomainType for RestoreSlotParams {
    const TYPE_NAME: &'static str = "RestoreSlotParams";

    fn from_json(value: &Value) -> Result<RestoreSlotParams> {
        let mut f = Fields::new(value, "RestoreSlotParams")?;
        let slot_id = f.id("slotId")?;
        let prior_epoch = f.id("priorEpoch")?;
        let policy = f.id("policy")?;
        f.finish()?;
        let p = RestoreSlotParams {
            slot_id,
            prior_epoch,
            policy,
        };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("slotId", self.slot_id.clone().into()),
            ("priorEpoch", self.prior_epoch.clone().into()),
            ("policy", self.policy.clone().into()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.slot_id) {
            return err("RestoreSlotParams: slotId is not a valid id");
        }
        if !is_id(&self.prior_epoch) {
            return err("RestoreSlotParams: priorEpoch is not a valid id");
        }
        policy_ok(&self.policy, "RestoreSlotParams")
    }
}

/// `Environment.RestoreSlot` result: the restored world at the same boundary number under the
/// new epoch. It ran no transition, so it carries no audio chunk.
#[derive(Clone, Debug, PartialEq)]
pub struct RestoreSlotResult {
    pub slot_id: String,
    pub committed_step: u64,
    pub observation: WorldObservation,
}

impl RestoreSlotResult {
    pub fn validate_against_scope(&self, scope: &Scope) -> Result<()> {
        self.validate()?;
        if self.committed_step != scope.step {
            return err(format!(
                "RestoreSlotResult: committedStep {} must be the scoped step {}",
                self.committed_step, scope.step
            ));
        }
        Ok(())
    }
}

impl DomainType for RestoreSlotResult {
    const TYPE_NAME: &'static str = "RestoreSlotResult";

    fn from_json(value: &Value) -> Result<RestoreSlotResult> {
        let mut f = Fields::new(value, "RestoreSlotResult")?;
        let slot_id = f.id("slotId")?;
        let committed_step = f.u64_string("committedStep")?;
        let observation = WorldObservation::from_json(f.value("observation")?)?;
        f.finish()?;
        let r = RestoreSlotResult {
            slot_id,
            committed_step,
            observation,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("slotId", self.slot_id.clone().into()),
            ("committedStep", u64_json(self.committed_step)),
            ("observation", self.observation.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.slot_id) {
            return err("RestoreSlotResult: slotId is not a valid id");
        }
        self.observation.validate()?;
        if self.observation.boundary != self.committed_step {
            return err("RestoreSlotResult: the observation boundary must be the committed step");
        }
        if !self.observation.audio.is_empty() {
            return err(
                "RestoreSlotResult: a restored slot ran no transition and carries no audio chunk",
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Agent.Rollback

/// `Agent.Rollback` params. Scope is the new epoch at the committed boundary; the agent must be
/// Ready at `priorEpoch` and that same step.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRollbackParams {
    pub agent_id: String,
    pub prior_epoch: String,
    pub policy: String,
    pub input: SensoryInput,
    pub decision_context: TypedValue,
}

impl AgentRollbackParams {
    /// The installed input is the restored boundary, which is the scoped step.
    pub fn validate_against_scope(&self, scope: &Scope) -> Result<()> {
        self.validate()?;
        if self.prior_epoch == scope.epoch {
            return err("AgentRollbackParams: priorEpoch must differ from the scoped (new) epoch");
        }
        if self.input.boundary != scope.step {
            return err(format!(
                "AgentRollbackParams: input.boundary {} must be the scoped step {}",
                self.input.boundary, scope.step
            ));
        }
        Ok(())
    }
}

impl DomainType for AgentRollbackParams {
    const TYPE_NAME: &'static str = "AgentRollbackParams";

    fn from_json(value: &Value) -> Result<AgentRollbackParams> {
        let mut f = Fields::new(value, "AgentRollbackParams")?;
        let agent_id = f.id("agentId")?;
        let prior_epoch = f.id("priorEpoch")?;
        let policy = f.id("policy")?;
        let input = SensoryInput::from_json(f.value("input")?)?;
        let decision_context = TypedValue::from_json(f.value("decisionContext")?)?;
        f.finish()?;
        let p = AgentRollbackParams {
            agent_id,
            prior_epoch,
            policy,
            input,
            decision_context,
        };
        p.validate()?;
        Ok(p)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("agentId", self.agent_id.clone().into()),
            ("priorEpoch", self.prior_epoch.clone().into()),
            ("policy", self.policy.clone().into()),
            ("input", self.input.to_json()),
            ("decisionContext", self.decision_context.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.agent_id) {
            return err("AgentRollbackParams: agentId is not a valid id");
        }
        if !is_id(&self.prior_epoch) {
            return err("AgentRollbackParams: priorEpoch is not a valid id");
        }
        policy_ok(&self.policy, "AgentRollbackParams")?;
        self.input.validate()?;
        self.decision_context.validate()
    }
}

/// `Agent.Rollback` result: the same acknowledgment shape as a commit, at the same boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRollbackResult {
    pub agent_id: String,
    pub committed_step: u64,
    pub decision_context_digest: String,
    pub telemetry: AgentTelemetry,
}

impl AgentRollbackResult {
    pub fn validate_against_scope(&self, scope: &Scope) -> Result<()> {
        self.validate()?;
        if self.committed_step != scope.step {
            return err(format!(
                "AgentRollbackResult: committedStep {} must be the scoped step {}; a rollback runs no tick",
                self.committed_step, scope.step
            ));
        }
        Ok(())
    }
}

impl DomainType for AgentRollbackResult {
    const TYPE_NAME: &'static str = "AgentRollbackResult";

    fn from_json(value: &Value) -> Result<AgentRollbackResult> {
        let mut f = Fields::new(value, "AgentRollbackResult")?;
        let agent_id = f.id("agentId")?;
        let committed_step = f.u64_string("committedStep")?;
        let decision_context_digest = f.string("decisionContextDigest")?.to_owned();
        let telemetry = AgentTelemetry::from_json(f.value("telemetry")?)?;
        f.finish()?;
        let r = AgentRollbackResult {
            agent_id,
            committed_step,
            decision_context_digest,
            telemetry,
        };
        r.validate()?;
        Ok(r)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("agentId", self.agent_id.clone().into()),
            ("committedStep", u64_json(self.committed_step)),
            (
                "decisionContextDigest",
                self.decision_context_digest.clone().into(),
            ),
            ("telemetry", self.telemetry.to_json()),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.agent_id) {
            return err("AgentRollbackResult: agentId is not a valid id");
        }
        if !is_digest(&self.decision_context_digest) {
            return err(
                "AgentRollbackResult: decisionContextDigest must be 64 lowercase hex digits",
            );
        }
        self.telemetry.validate()
    }
}
