//! One place that knows how to read every type named by a fixture.

use flybus::wire::WireError;
use fly_session_types::extensions::*;
use fly_session_types::gameboy::{
    ChannelsDecision, LegacyComposition, LegacyProfile, MemoryInspection, ReadoutContext,
    RollbackRequest,
};
use fly_session_types::media::*;
use fly_session_types::publishing::*;
use fly_session_types::rpc::*;
use fly_session_types::scalar::*;
use fly_session_types::trace::*;
use fly_session_types::workers::*;
use serde_json::Value;

/// Reads the value as `type_name`, re-runs its validate step and writes it back out.
///
/// Every fixture assertion goes through this, so a type that reads a field but forgets to
/// write it back fails the round trip.
pub fn round_trip(type_name: &str, value: &Value) -> std::result::Result<Value, WireError> {
    macro_rules! arm {
        ($t:ty) => {
            if type_name == <$t as DomainType>::TYPE_NAME {
                let parsed = <$t as DomainType>::from_json(value)?;
                parsed.validate()?;
                return Ok(parsed.to_json());
            }
        };
    }
    arm!(Scope);
    arm!(RationalNs);
    arm!(SchemaRef);
    arm!(TypedValue);
    arm!(SessionRpcRequest);
    arm!(SessionRpcSuccess);
    arm!(SessionRpcFailure);
    arm!(AssetRef);
    arm!(SensoryInput);
    arm!(Stimulus);
    arm!(Reward);
    arm!(AgentTelemetry);
    arm!(AgentInitializeParams);
    arm!(AgentInitializeResult);
    arm!(PrepareParams);
    arm!(PreparedDecision);
    arm!(CommitParams);
    arm!(AgentCommitResult);
    arm!(ControllerSchema);
    arm!(PortControl);
    arm!(EnvironmentDescriptor);
    arm!(EnvironmentInitializeParams);
    arm!(EnvironmentInitializeResult);
    arm!(WorldObservation);
    arm!(AdvanceParams);
    arm!(StepResult);
    arm!(HelloParams);
    arm!(HelloResult);
    arm!(StatusResult);
    arm!(AcknowledgeParams);
    arm!(AcknowledgeResult);
    arm!(ShutdownParams);
    arm!(ShutdownResult);
    arm!(TaskEvent);
    arm!(EpisodeRequest);
    arm!(ViewDescriptor);
    arm!(ViewRef);
    arm!(AudioDescriptor);
    arm!(AudioRef);
    arm!(CaptureParams);
    arm!(CaptureResult);
    arm!(StageRestoreParams);
    arm!(StageRestoreResult);
    arm!(ActivateRestoreParams);
    arm!(ActivateRestoreResult);
    arm!(SessionDescriptor);
    arm!(CommittedSnapshot);
    arm!(TraceBehaviour);
    arm!(TraceOperational);
    arm!(TransitionTrace);
    arm!(SaveSlotParams);
    arm!(SaveSlotResult);
    arm!(RestoreSlotParams);
    arm!(RestoreSlotResult);
    arm!(AgentRollbackParams);
    arm!(AgentRollbackResult);
    arm!(ReadoutContext);
    arm!(ChannelsDecision);
    arm!(MemoryInspection);
    arm!(RollbackRequest);
    arm!(LegacyProfile);
    arm!(LegacyComposition);
    Err(WireError(format!(
        "no fixture reader for type {type_name:?}"
    )))
}

/// The type names `round_trip` knows.
pub const READABLE_TYPES: &[&str] = &[
    "Scope",
    "RationalNs",
    "SchemaRef",
    "TypedValue",
    "SessionRpcRequest",
    "SessionRpcSuccess",
    "SessionRpcFailure",
    "AssetRef",
    "SensoryInput",
    "Stimulus",
    "Reward",
    "AgentTelemetry",
    "AgentInitializeParams",
    "AgentInitializeResult",
    "PrepareParams",
    "PreparedDecision",
    "CommitParams",
    "AgentCommitResult",
    "ControllerSchema",
    "PortControl",
    "EnvironmentDescriptor",
    "EnvironmentInitializeParams",
    "EnvironmentInitializeResult",
    "WorldObservation",
    "AdvanceParams",
    "StepResult",
    "HelloParams",
    "HelloResult",
    "StatusResult",
    "AcknowledgeParams",
    "AcknowledgeResult",
    "ShutdownParams",
    "ShutdownResult",
    "TaskEvent",
    "EpisodeRequest",
    "ViewDescriptor",
    "ViewRef",
    "AudioDescriptor",
    "AudioRef",
    "CaptureParams",
    "CaptureResult",
    "StageRestoreParams",
    "StageRestoreResult",
    "ActivateRestoreParams",
    "ActivateRestoreResult",
    "SessionDescriptor",
    "CommittedSnapshot",
    "TraceBehaviour",
    "TraceOperational",
    "TransitionTrace",
    "SaveSlotParams",
    "SaveSlotResult",
    "RestoreSlotParams",
    "RestoreSlotResult",
    "AgentRollbackParams",
    "AgentRollbackResult",
    "GameboyReadoutContext",
    "GameboyChannelsDecision",
    "GameboyMemoryInspection",
    "LegacyRatchetRollbackRequest",
    "LegacyGameboyProfile",
    "LegacyGameboyComposition",
];
