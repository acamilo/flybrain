//! The publication types of publishing-v1 section 3.
//!
//! A descriptor changes rarely and a snapshot changes every boundary; both are published on
//! the same bus, and a snapshot names the descriptor revision it was shaped by.

use flybus::wire::Fields;
use serde_json::Value;

use crate::media::{AudioRef, MAX_VIEWS, ViewRef, audio_list, view_list};
use crate::scalar::{
    DomainType, RationalNs, Result, SchemaRef, Scope, TypedValue, constant, err, id_list,
    is_digest, is_id, list, obj, require_unique, u64_json,
};
use crate::workers::{
    AgentTelemetry, AssetRef, EnvironmentDescriptor, MAX_AGENTS, MAX_RATE_ROLES, PortControl,
};

/// Declared stimulus kinds per agent. Not a stated bound; recorded in the schema set.
pub const MAX_SUPPORTED_STIMULI: usize = 64;
/// Installed assets in one descriptor. Not a stated bound; recorded in the schema set.
pub const MAX_ASSETS: usize = 64;
/// Scoped event ids in one snapshot. Not a stated bound; recorded in the schema set.
pub const MAX_SNAPSHOT_EVENTS: usize = 64;

/// One agent's place in the composition.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentDescriptor {
    pub agent_id: String,
    pub port_id: String,
    pub profile_digest: String,
    pub dataset_digest: String,
    pub index_digest: String,
    pub neuron_count: u64,
    pub rate_roles: Vec<String>,
    pub supported_stimuli: Vec<String>,
}

/// `SessionDescriptor`: the framework shape of one running session.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionDescriptor {
    pub session_id: String,
    pub revision: u64,
    pub composition_digest: String,
    pub environment: EnvironmentDescriptor,
    pub task_schema: SchemaRef,
    pub agents: Vec<AgentDescriptor>,
    pub assets: Vec<AssetRef>,
}

impl DomainType for SessionDescriptor {
    const TYPE_NAME: &'static str = "SessionDescriptor";

    fn from_json(value: &Value) -> Result<SessionDescriptor> {
        let mut f = Fields::new(value, "SessionDescriptor")?;
        let session_id = f.id("sessionId")?;
        let revision = f.u64_string("revision")?;
        let composition_digest = f.string("compositionDigest")?.to_owned();
        constant(&mut f, "schedulerId", "lockstep-v1")?;
        let environment = EnvironmentDescriptor::from_json(f.value("environment")?)?;
        let task_schema = SchemaRef::from_json(f.value("taskSchema")?)?;
        let agents = list(&mut f, "agents", 1, MAX_AGENTS, |v| {
            let mut a = Fields::new(v, "SessionDescriptor.agents")?;
            let agent_id = a.id("agentId")?;
            let port_id = a.id("portId")?;
            let profile_digest = a.string("profileDigest")?.to_owned();
            let dataset_digest = a.string("datasetDigest")?.to_owned();
            let index_digest = a.string("indexDigest")?.to_owned();
            let neuron_count = a.u64_string("neuronCount")?;
            let rate_roles = id_list(&mut a, "rateRoles", 0, MAX_RATE_ROLES)?;
            let supported_stimuli = id_list(&mut a, "supportedStimuli", 0, MAX_SUPPORTED_STIMULI)?;
            a.finish()?;
            Ok(AgentDescriptor {
                agent_id,
                port_id,
                profile_digest,
                dataset_digest,
                index_digest,
                neuron_count,
                rate_roles,
                supported_stimuli,
            })
        })?;
        let assets = list(&mut f, "assets", 0, MAX_ASSETS, AssetRef::from_json)?;
        f.finish()?;
        let d = SessionDescriptor {
            session_id,
            revision,
            composition_digest,
            environment,
            task_schema,
            agents,
            assets,
        };
        d.validate()?;
        Ok(d)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("sessionId", self.session_id.clone().into()),
            ("revision", u64_json(self.revision)),
            ("compositionDigest", self.composition_digest.clone().into()),
            ("schedulerId", "lockstep-v1".into()),
            ("environment", self.environment.to_json()),
            ("taskSchema", self.task_schema.to_json()),
            (
                "agents",
                Value::Array(
                    self.agents
                        .iter()
                        .map(|a| {
                            obj(vec![
                                ("agentId", a.agent_id.clone().into()),
                                ("portId", a.port_id.clone().into()),
                                ("profileDigest", a.profile_digest.clone().into()),
                                ("datasetDigest", a.dataset_digest.clone().into()),
                                ("indexDigest", a.index_digest.clone().into()),
                                ("neuronCount", u64_json(a.neuron_count)),
                                (
                                    "rateRoles",
                                    Value::Array(
                                        a.rate_roles.iter().map(|r| r.clone().into()).collect(),
                                    ),
                                ),
                                (
                                    "supportedStimuli",
                                    Value::Array(
                                        a.supported_stimuli
                                            .iter()
                                            .map(|s| s.clone().into())
                                            .collect(),
                                    ),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "assets",
                Value::Array(self.assets.iter().map(AssetRef::to_json).collect()),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.session_id) {
            return err("SessionDescriptor: sessionId is not a valid id");
        }
        if !is_digest(&self.composition_digest) {
            return err("SessionDescriptor: compositionDigest must be 64 lowercase hex digits");
        }
        self.environment.validate()?;
        self.task_schema.validate()?;
        if self.agents.is_empty() || self.agents.len() > MAX_AGENTS {
            return err("SessionDescriptor: 1..=4 agents in the first composition");
        }
        require_unique(
            self.agents.iter().map(|a| a.agent_id.as_str()),
            "SessionDescriptor.agents agentId",
        )?;
        require_unique(
            self.agents.iter().map(|a| a.port_id.as_str()),
            "SessionDescriptor.agents portId",
        )?;
        for agent in &self.agents {
            if !is_id(&agent.agent_id) || !is_id(&agent.port_id) {
                return err("SessionDescriptor: agentId and portId must be valid ids");
            }
            for (what, digest) in [
                ("profileDigest", &agent.profile_digest),
                ("datasetDigest", &agent.dataset_digest),
                ("indexDigest", &agent.index_digest),
            ] {
                if !is_digest(digest) {
                    return err(format!(
                        "SessionDescriptor: agent {what} must be 64 lowercase hex digits"
                    ));
                }
            }
            if agent.rate_roles.len() > MAX_RATE_ROLES {
                return err("SessionDescriptor: at most 64 rate roles per agent");
            }
            require_unique(
                agent.rate_roles.iter().map(String::as_str),
                "SessionDescriptor.agents rateRoles",
            )?;
            require_unique(
                agent.supported_stimuli.iter().map(String::as_str),
                "SessionDescriptor.agents supportedStimuli",
            )?;
            if self.environment.port(&agent.port_id).is_none() {
                return err(format!(
                    "SessionDescriptor: agent {:?} is bound to port {:?}, which the environment does not declare",
                    agent.agent_id, agent.port_id
                ));
            }
        }
        require_unique(
            self.assets.iter().map(|a| a.id.as_str()),
            "SessionDescriptor.assets",
        )?;
        for asset in &self.assets {
            asset.validate()?;
        }
        Ok(())
    }
}

/// One agent's committed values in a snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotAgent {
    pub agent_id: String,
    pub telemetry: AgentTelemetry,
    pub selected_decision: Option<TypedValue>,
    pub applied_controls: Option<PortControl>,
}

/// `CommittedSnapshot`: the values of one committed boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct CommittedSnapshot {
    pub descriptor_revision: u64,
    pub publisher_incarnation: String,
    pub scope: Scope,
    pub episode_id: String,
    pub sequence: u64,
    pub world_time: RationalNs,
    pub agents: Vec<SnapshotAgent>,
    pub progress: TypedValue,
    pub views: Vec<ViewRef>,
    pub audio: Vec<AudioRef>,
    pub event_ids: Vec<String>,
}

impl CommittedSnapshot {
    /// Descriptor agreement: the revision, the agent set and the port each control names.
    pub fn validate_against(&self, descriptor: &SessionDescriptor) -> Result<()> {
        self.validate()?;
        if self.descriptor_revision != descriptor.revision {
            return err("CommittedSnapshot: descriptorRevision does not match the descriptor");
        }
        if self.scope.session_id != descriptor.session_id {
            return err("CommittedSnapshot: sessionId does not match the descriptor");
        }
        for agent in &self.agents {
            let declared = descriptor
                .agents
                .iter()
                .find(|a| a.agent_id == agent.agent_id)
                .ok_or_else(|| {
                    crate::scalar::wire_err(format!(
                        "CommittedSnapshot: agent {:?} is not in the descriptor",
                        agent.agent_id
                    ))
                })?;
            agent
                .telemetry
                .validate_against_roles(&declared.rate_roles)?;
            if let Some(controls) = &agent.applied_controls {
                if controls.port_id != declared.port_id {
                    return err(format!(
                        "CommittedSnapshot: agent {:?} controls port {:?}, not its assigned {:?}",
                        agent.agent_id, controls.port_id, declared.port_id
                    ));
                }
                let port = descriptor
                    .environment
                    .port(&declared.port_id)
                    .ok_or_else(|| {
                        crate::scalar::wire_err("CommittedSnapshot: assigned port is not declared")
                    })?;
                controls.validate_against(&port.controls)?;
            }
        }
        Ok(())
    }
}

impl DomainType for CommittedSnapshot {
    const TYPE_NAME: &'static str = "CommittedSnapshot";

    fn from_json(value: &Value) -> Result<CommittedSnapshot> {
        let mut f = Fields::new(value, "CommittedSnapshot")?;
        let descriptor_revision = f.u64_string("descriptorRevision")?;
        let publisher_incarnation = f.id("publisherIncarnation")?;
        let scope = Scope::from_json(f.value("scope")?)?;
        let episode_id = f.id("episodeId")?;
        let sequence = f.u64_string("sequence")?;
        let world_time = RationalNs::from_json(f.value("worldTime")?)?;
        let agents = list(&mut f, "agents", 1, MAX_AGENTS, |v| {
            let mut a = Fields::new(v, "CommittedSnapshot.agents")?;
            let agent_id = a.id("agentId")?;
            let telemetry = AgentTelemetry::from_json(a.value("telemetry")?)?;
            let selected_decision = TypedValue::nullable_from_json(a.value("selectedDecision")?)?;
            let applied_controls = match a.value("appliedControls")? {
                Value::Null => None,
                v => Some(PortControl::from_json(v)?),
            };
            a.finish()?;
            Ok(SnapshotAgent {
                agent_id,
                telemetry,
                selected_decision,
                applied_controls,
            })
        })?;
        let progress = TypedValue::from_json(f.value("progress")?)?;
        let (views, audio) = {
            let v = f.value("media")?;
            let mut m = Fields::new(v, "CommittedSnapshot.media")?;
            let views = view_list(&mut m, "views")?;
            let audio = audio_list(&mut m, "audio")?;
            m.finish()?;
            (views, audio)
        };
        let event_ids = id_list(&mut f, "eventIds", 0, MAX_SNAPSHOT_EVENTS)?;
        f.finish()?;
        let s = CommittedSnapshot {
            descriptor_revision,
            publisher_incarnation,
            scope,
            episode_id,
            sequence,
            world_time,
            agents,
            progress,
            views,
            audio,
            event_ids,
        };
        s.validate()?;
        Ok(s)
    }

    fn to_json(&self) -> Value {
        obj(vec![
            ("descriptorRevision", u64_json(self.descriptor_revision)),
            (
                "publisherIncarnation",
                self.publisher_incarnation.clone().into(),
            ),
            ("scope", self.scope.to_json()),
            ("episodeId", self.episode_id.clone().into()),
            ("sequence", u64_json(self.sequence)),
            ("worldTime", self.world_time.to_json()),
            (
                "agents",
                Value::Array(
                    self.agents
                        .iter()
                        .map(|a| {
                            obj(vec![
                                ("agentId", a.agent_id.clone().into()),
                                ("telemetry", a.telemetry.to_json()),
                                (
                                    "selectedDecision",
                                    TypedValue::nullable_to_json(a.selected_decision.as_ref()),
                                ),
                                (
                                    "appliedControls",
                                    a.applied_controls
                                        .as_ref()
                                        .map_or(Value::Null, PortControl::to_json),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("progress", self.progress.to_json()),
            (
                "media",
                obj(vec![
                    (
                        "views",
                        Value::Array(self.views.iter().map(ViewRef::to_json).collect()),
                    ),
                    (
                        "audio",
                        Value::Array(self.audio.iter().map(AudioRef::to_json).collect()),
                    ),
                ]),
            ),
            (
                "eventIds",
                Value::Array(self.event_ids.iter().map(|e| e.clone().into()).collect()),
            ),
        ])
    }

    fn validate(&self) -> Result<()> {
        if !is_id(&self.publisher_incarnation) || !is_id(&self.episode_id) {
            return err("CommittedSnapshot: publisherIncarnation and episodeId must be valid ids");
        }
        self.scope.validate()?;
        self.world_time.validate()?;
        if self.agents.is_empty() || self.agents.len() > MAX_AGENTS {
            return err("CommittedSnapshot: 1..=4 agents");
        }
        require_unique(
            self.agents.iter().map(|a| a.agent_id.as_str()),
            "CommittedSnapshot.agents",
        )?;
        for agent in &self.agents {
            if !is_id(&agent.agent_id) {
                return err("CommittedSnapshot: agentId is not a valid id");
            }
            agent.telemetry.validate()?;
            if let Some(decision) = &agent.selected_decision {
                decision.validate()?;
            }
            if let Some(controls) = &agent.applied_controls {
                controls.validate()?;
            }
            // "Decisions/controls describe the transition ending at that boundary, null at
            // initial boundary 0." (publishing-v1 section 3)
            if self.scope.step == 0
                && (agent.selected_decision.is_some() || agent.applied_controls.is_some())
            {
                return err(
                    "CommittedSnapshot: at boundary 0 selectedDecision and appliedControls are null",
                );
            }
            if self.scope.step > 0
                && (agent.selected_decision.is_none() || agent.applied_controls.is_none())
            {
                return err(
                    "CommittedSnapshot: past boundary 0 every agent has a decision and applied controls",
                );
            }
        }
        self.progress.validate()?;
        if self.views.len() > MAX_VIEWS {
            return err("CommittedSnapshot: at most 8 views");
        }
        require_unique(
            self.views.iter().map(|v| v.view_id.as_str()),
            "CommittedSnapshot.media.views",
        )?;
        require_unique(
            self.audio.iter().map(|a| a.stream_id.as_str()),
            "CommittedSnapshot.media.audio",
        )?;
        require_unique(
            self.event_ids.iter().map(String::as_str),
            "CommittedSnapshot.eventIds",
        )?;
        Ok(())
    }
}
