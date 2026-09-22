//! The canonical schema set, and `contractDigest`.
//!
//! The digest is taken over a *declaration*, not over this file's text: every type below is
//! a row of `(name, source document, fields)` and every field a row of
//! `(name, kind, required, constraint)`. Reformatting the source, reordering the rows,
//! renaming a Rust struct field or adding a comment cannot change the digest; changing a
//! type name, a JSON field name, a kind, a bound or a closed enum's members does. The rows
//! are sorted and rendered as canonical JSON (RFC 8785) before hashing, so two
//! implementations that agree on the schema set agree on the digest.
//!
//! `fixtures/schema-set.json` is the rendered set, and `fixtures/contract-digest.json`
//! records the digest; `tests/schema_set.rs` regenerates both, and the TypeScript package
//! hashes the same file with its own canonical JSON.

use serde_json::Value;

use crate::canonical;
use crate::scalar::{Result, obj};

/// The schema set version. Bumped when the set gains or loses types, so an old digest is
/// never mistaken for a new one.
pub const SCHEMA_SET_VERSION: u64 = 1;

/// One field of one type.
#[derive(Clone, Copy, Debug)]
pub struct FieldSchema {
    pub name: &'static str,
    pub kind: &'static str,
    pub required: bool,
    pub constraint: &'static str,
}

/// One domain type.
#[derive(Clone, Copy, Debug)]
pub struct TypeSchema {
    pub name: &'static str,
    /// The contract and section this type is specified in.
    pub source: &'static str,
    pub fields: &'static [FieldSchema],
}

/// One closed enum.
#[derive(Clone, Copy, Debug)]
pub struct EnumSchema {
    pub name: &'static str,
    pub source: &'static str,
    pub members: &'static [&'static str],
}

/// One named bound.
#[derive(Clone, Copy, Debug)]
pub struct LimitSchema {
    pub name: &'static str,
    pub value: u64,
    /// Where the bound comes from: a contract section, or `crate` when this crate chose it
    /// because the documents state none.
    pub source: &'static str,
}

const fn req(name: &'static str, kind: &'static str, constraint: &'static str) -> FieldSchema {
    FieldSchema {
        name,
        kind,
        required: true,
        constraint,
    }
}

const fn opt(name: &'static str, kind: &'static str, constraint: &'static str) -> FieldSchema {
    FieldSchema {
        name,
        kind,
        required: false,
        constraint,
    }
}

pub const ENUMS: &[EnumSchema] = &[
    EnumSchema {
        name: "ErrorCode",
        source: "ipc-v1 7",
        members: crate::rpc::ErrorCode::ALL,
    },
    EnumSchema {
        name: "MutationCertainty",
        source: "ipc-v1 3",
        members: crate::rpc::MutationCertainty::ALL,
    },
    EnumSchema {
        name: "Role",
        source: "ipc-v1 4",
        members: crate::workers::Role::ALL,
    },
    EnumSchema {
        name: "WorkerState",
        source: "ipc-v1 4",
        members: crate::workers::WorkerState::ALL,
    },
    EnumSchema {
        name: "Recovery",
        source: "workers-v1 3",
        members: crate::workers::Recovery::ALL,
    },
    EnumSchema {
        name: "Determinism",
        source: "workers-v1 3",
        members: crate::workers::Determinism::ALL,
    },
    EnumSchema {
        name: "AxisRange",
        source: "workers-v1 3",
        members: crate::workers::AxisRange::ALL,
    },
    EnumSchema {
        name: "ViewFormat",
        source: "state-media-v1 2",
        members: &["rgba8"],
    },
    EnumSchema {
        name: "AudioFormat",
        source: "state-media-v1 2",
        members: &["f32le-interleaved"],
    },
    EnumSchema {
        name: "SchedulerId",
        source: "publishing-v1 3",
        members: &["lockstep-v1"],
    },
    EnumSchema {
        name: "EpisodeRequestKind",
        source: "workers-v1 4",
        members: &["terminal"],
    },
];

pub const LIMITS: &[LimitSchema] = &[
    LimitSchema {
        name: "maxAgents",
        value: crate::workers::MAX_AGENTS as u64,
        source: "ipc-v1 2",
    },
    LimitSchema {
        name: "maxPorts",
        value: crate::workers::MAX_PORTS as u64,
        source: "ipc-v1 2",
    },
    LimitSchema {
        name: "maxRateRoles",
        value: crate::workers::MAX_RATE_ROLES as u64,
        source: "ipc-v1 2",
    },
    LimitSchema {
        name: "maxStimuliPerOperation",
        value: crate::workers::MAX_STIMULI as u64,
        source: "workers-v1 1",
    },
    LimitSchema {
        name: "maxRewardsPerOperation",
        value: crate::workers::MAX_REWARDS as u64,
        source: "workers-v1 1",
    },
    LimitSchema {
        name: "maxViews",
        value: crate::media::MAX_VIEWS as u64,
        source: "workers-v1 1",
    },
    LimitSchema {
        name: "maxButtons",
        value: crate::workers::MAX_BUTTONS as u64,
        source: "workers-v1 3",
    },
    LimitSchema {
        name: "maxAxes",
        value: crate::workers::MAX_AXES as u64,
        source: "workers-v1 3",
    },
    LimitSchema {
        name: "maxAcknowledge",
        value: crate::workers::MAX_ACKNOWLEDGE as u64,
        source: "ipc-v1 5",
    },
    LimitSchema {
        name: "maxTypedValueBytes",
        value: crate::scalar::MAX_TYPED_VALUE_BYTES as u64,
        source: "ipc-v1 2",
    },
    LimitSchema {
        name: "maxEnvelopeBytes",
        value: canonical::MAX_ENVELOPE_BYTES as u64,
        source: "bus-v1 4",
    },
    LimitSchema {
        name: "maxAttachments",
        value: flybus::wire::MAX_ATTACHMENTS as u64,
        source: "bus-v1 4",
    },
    LimitSchema {
        name: "maxMessageCodePoints",
        value: crate::workers::MAX_MESSAGE_CODE_POINTS as u64,
        source: "ipc-v1 7",
    },
    LimitSchema {
        name: "maxViewDimension",
        value: crate::media::MAX_VIEW_DIMENSION,
        source: "state-media-v1 2",
    },
    LimitSchema {
        name: "maxPixelAspectPart",
        value: crate::media::MAX_PIXEL_ASPECT,
        source: "state-media-v1 2",
    },
    LimitSchema {
        name: "maxObservationDelaySteps",
        value: crate::media::MAX_OBSERVATION_DELAY_STEPS,
        source: "state-media-v1 2",
    },
    LimitSchema {
        name: "maxSampleFrames",
        value: crate::media::MAX_SAMPLE_FRAMES,
        source: "state-media-v1 2",
    },
    LimitSchema {
        name: "maxEngineFrameLength",
        value: crate::workers::MAX_ENGINE_FRAME_LEN as u64,
        source: "workers-v1 3",
    },
    LimitSchema {
        name: "maxSchemaVersion",
        value: 65_535,
        source: "ipc-v1 2",
    },
    LimitSchema {
        name: "maxAudioStreams",
        value: crate::media::MAX_AUDIO_STREAMS as u64,
        source: "crate",
    },
    LimitSchema {
        name: "maxCapabilities",
        value: crate::workers::MAX_CAPABILITIES as u64,
        source: "crate",
    },
    LimitSchema {
        name: "maxWorkerThreads",
        value: crate::workers::MAX_WORKER_THREADS,
        source: "workers-v1 2",
    },
    LimitSchema {
        name: "maxSupportedMajors",
        value: crate::workers::MAX_SUPPORTED_MAJORS as u64,
        source: "crate",
    },
    LimitSchema {
        name: "maxSupportedStimuli",
        value: crate::publishing::MAX_SUPPORTED_STIMULI as u64,
        source: "crate",
    },
    LimitSchema {
        name: "maxAssets",
        value: crate::publishing::MAX_ASSETS as u64,
        source: "crate",
    },
    LimitSchema {
        name: "maxSnapshotEvents",
        value: crate::publishing::MAX_SNAPSHOT_EVENTS as u64,
        source: "crate",
    },
];

pub const SCHEMAS: &[TypeSchema] = &[
    TypeSchema {
        name: "Scope",
        source: "ipc-v1 2",
        fields: &[
            req("sessionId", "Id", "^[a-z0-9][a-z0-9._-]{0,63}$"),
            req("epoch", "Id", "^[a-z0-9][a-z0-9._-]{0,63}$"),
            req("step", "U64", "decimal string, <= 18446744073709551615"),
        ],
    },
    TypeSchema {
        name: "RationalNs",
        source: "ipc-v1 2",
        fields: &[
            req("numerator", "U64", "reduced against denominator"),
            req(
                "denominator",
                "U64",
                "positive; zero is encoded 0/1; arithmetic is checked",
            ),
        ],
    },
    TypeSchema {
        name: "SchemaRef",
        source: "ipc-v1 2",
        fields: &[
            req("id", "Id", ""),
            req("version", "int", "1..=65535"),
            req("digest", "Digest", "64 lowercase hex digits"),
        ],
    },
    TypeSchema {
        name: "TypedValue",
        source: "ipc-v1 2",
        fields: &[
            req("schema", "SchemaRef", ""),
            req(
                "value",
                "object",
                "canonical JSON of the whole TypedValue <= 32768 bytes",
            ),
        ],
    },
    TypeSchema {
        name: "SessionRpcRequest",
        source: "ipc-v1 2",
        fields: &[
            req("requestId", "DomainRequestId", "req-<U64>"),
            opt("scope", "Scope|null", "null for lifecycle calls"),
            req("params", "object", "no bus callId/deliveryId/ownerId keys"),
        ],
    },
    TypeSchema {
        name: "SessionRpcSuccess",
        source: "ipc-v1 3",
        fields: &[
            req("type", "const", "\"result\""),
            req("requestId", "DomainRequestId", "echoes the request"),
            req("workerId", "Id", ""),
            req("incarnationId", "Id", ""),
            opt("scope", "Scope|null", "echoes the request scope"),
            req("result", "object", ""),
        ],
    },
    TypeSchema {
        name: "SessionRpcFailure",
        source: "ipc-v1 3",
        fields: &[
            req("type", "const", "\"error\""),
            req("requestId", "DomainRequestId", "echoes the request"),
            req("workerId", "Id", ""),
            req("incarnationId", "Id", ""),
            opt("scope", "Scope|null", "echoes the request scope"),
            req("error.code", "ErrorCode", ""),
            req("error.message", "string", "<= 512 code points"),
            req(
                "error.mutation",
                "MutationCertainty",
                "none for every code raised before mutation",
            ),
        ],
    },
    TypeSchema {
        name: "AssetRef",
        source: "workers-v1 1",
        fields: &[
            req("id", "Id", ""),
            req("digest", "Digest", ""),
            req("byteLength", "U64", "positive"),
            req("format", "Id", ""),
        ],
    },
    TypeSchema {
        name: "SensoryInput",
        source: "workers-v1 1",
        fields: &[
            req("boundary", "U64", "the observed environment boundary"),
            req(
                "views",
                "array<ViewRef>",
                "<= 8, unique viewId, producedStep <= boundary",
            ),
            opt(
                "structured",
                "TypedValue|null",
                "a pixel-only profile rejects non-null",
            ),
        ],
    },
    TypeSchema {
        name: "Stimulus",
        source: "workers-v1 1",
        fields: &[
            req("id", "Id", "unique within its command namespace"),
            req("kindId", "Id", "resolved through a profile capability"),
            req("durationMs", "number", "finite and > 0"),
        ],
    },
    TypeSchema {
        name: "Reward",
        source: "workers-v1 1",
        fields: &[
            req("eventId", "Id", "unique within its outcome namespace"),
            req("ruleId", "Id", ""),
            req(
                "value",
                "number",
                "finite; positive-only profiles reject negatives",
            ),
        ],
    },
    TypeSchema {
        name: "AgentTelemetry",
        source: "workers-v1 1",
        fields: &[
            req("brainTicks", "U64", ""),
            req("populationRateHz", "number", "finite and nonnegative"),
            req(
                "rates",
                "array<{roleId:Id,hz:number}>",
                "<= 64, unique roleId, profile order, finite nonnegative hz",
            ),
            req(
                "learning",
                "{enabled:bool,updates:U64,changed:U64,signal:number}",
                "changed <= updates; signal finite",
            ),
        ],
    },
    TypeSchema {
        name: "AgentInitializeParams",
        source: "workers-v1 2",
        fields: &[
            req("agentId", "Id", ""),
            req("profile", "AssetRef", ""),
            req("seed", "int", "signed 32-bit"),
            req("initialInput", "SensoryInput", ""),
            req("initialDecisionContext", "TypedValue", ""),
            req("workerThreads", "int", ">= 1"),
        ],
    },
    TypeSchema {
        name: "AgentInitializeResult",
        source: "workers-v1 2",
        fields: &[
            req("agentId", "Id", ""),
            req("profileDigest", "Digest", ""),
            req("tickDuration", "RationalNs", "positive"),
            req("warmupTicks", "U64", ""),
            req("committedStep", "U64", "\"0\""),
            req("decisionContextDigest", "Digest", ""),
            req("telemetry", "AgentTelemetry", ""),
            req("graph", "AgentGraph", "rates are in graph.rateRoles order"),
        ],
    },
    TypeSchema {
        name: "AgentGraph",
        source: "workers-v1 2",
        fields: &[
            req("datasetDigest", "Digest", ""),
            req(
                "indexDigest",
                "Digest",
                "geometry mapping needs this, not neuronCount",
            ),
            req("neuronCount", "U64", ""),
            req("rateRoles", "array<Id>", "<= 64, unique"),
            req("supportedStimuli", "array<Id>", "<= 64, unique"),
        ],
    },
    TypeSchema {
        name: "PrepareParams",
        source: "workers-v1 2",
        fields: &[
            req("agentId", "Id", ""),
            req("profileDigest", "Digest", ""),
            req("interval", "RationalNs", "positive"),
            req("decisionContextDigest", "Digest", ""),
            req(
                "preStepStimulations",
                "array<Stimulus>",
                "<= 64, unique id, supplied order retained",
            ),
        ],
    },
    TypeSchema {
        name: "PreparedDecision",
        source: "workers-v1 2",
        fields: &[
            req("agentId", "Id", ""),
            req("ticksAdvanced", "U64", "<= brainTicks"),
            req("brainTicks", "U64", ""),
            req("remainder", "RationalNs", ">= 0 and < one model tick"),
            req(
                "decision",
                "TypedValue",
                "the profile's registered intent schema",
            ),
        ],
    },
    TypeSchema {
        name: "CommitParams",
        source: "workers-v1 2",
        fields: &[
            req("agentId", "Id", ""),
            req("preparedRequestId", "DomainRequestId", ""),
            req("nextInput", "SensoryInput", "boundary == scope.step + 1"),
            req("nextDecisionContext", "TypedValue", ""),
            req(
                "rewards",
                "array<Reward>",
                "<= 64, unique eventId, order retained",
            ),
            req(
                "taskStimulations",
                "array<Stimulus>",
                "<= 64, unique id, order retained",
            ),
        ],
    },
    TypeSchema {
        name: "AgentCommitResult",
        source: "workers-v1 2",
        fields: &[
            req("agentId", "Id", ""),
            req("committedStep", "U64", "k+1"),
            req("decisionContextDigest", "Digest", ""),
            req("telemetry", "AgentTelemetry", ""),
        ],
    },
    TypeSchema {
        name: "ControllerSchema",
        source: "workers-v1 3",
        fields: &[
            req("schema", "SchemaRef", ""),
            req("buttons", "array<Id>", "<= 32, unique, fixed order"),
            req(
                "axes",
                "array<{id:Id,range:AxisRange,neutral:number}>",
                "<= 16, unique id, neutral inside its range",
            ),
        ],
    },
    TypeSchema {
        name: "PortControl",
        source: "workers-v1 3",
        fields: &[
            req("portId", "Id", ""),
            req(
                "buttons",
                "array<{id:Id,down:bool}>",
                "every declared button, descriptor order, no extras",
            ),
            req(
                "axes",
                "array<{id:Id,value:number}>",
                "every declared axis in order; bipolar [-1,1], unit [0,1], refused not clamped",
            ),
        ],
    },
    TypeSchema {
        name: "EnvironmentDescriptor",
        source: "workers-v1 3",
        fields: &[
            req("backendDigest", "Digest", ""),
            req("contentDigest", "Digest", ""),
            req("configurationDigest", "Digest", ""),
            req("stepDuration", "RationalNs", "fixed, reduced, positive"),
            req(
                "ports",
                "array<{portId:Id,controls:ControllerSchema}>",
                "1..=4, unique portId, fixed order",
            ),
            req("inspectionSchema", "SchemaRef", ""),
            req("views", "array<ViewDescriptor>", "<= 8, unique viewId"),
            req("audio", "array<AudioDescriptor>", "unique streamId"),
            req("recovery", "Recovery", ""),
            req("determinism", "Determinism", ""),
        ],
    },
    TypeSchema {
        name: "EnvironmentInitializeParams",
        source: "workers-v1 3",
        fields: &[
            req("backendConfig", "AssetRef", ""),
            req("taskConfig", "AssetRef", ""),
            req("episodeId", "Id", ""),
            req(
                "portBindings",
                "array<{portId:Id,agentId:Id}>",
                "1..=4, unique portId and unique agentId",
            ),
        ],
    },
    TypeSchema {
        name: "EnvironmentInitializeResult",
        source: "workers-v1 3",
        fields: &[
            req("descriptor", "EnvironmentDescriptor", ""),
            req(
                "observation",
                "WorldObservation",
                "boundary 0 and worldTime 0/1",
            ),
        ],
    },
    TypeSchema {
        name: "WorldObservation",
        source: "workers-v1 3",
        fields: &[
            req("boundary", "U64", ""),
            req(
                "worldTime",
                "RationalNs",
                "logical time since episode start",
            ),
            opt("engineFrame", "string|null", "<= 64 characters"),
            req("sensoryViews", "array<ViewRef>", "<= 8, unique viewId"),
            req(
                "inspection",
                "TypedValue",
                "the descriptor's inspectionSchema",
            ),
            req("broadcastViews", "array<ViewRef>", "<= 8, unique viewId"),
            req("audio", "array<AudioRef>", "unique streamId"),
        ],
    },
    TypeSchema {
        name: "AdvanceParams",
        source: "workers-v1 3",
        fields: &[
            req("batchId", "Id", "unique within an epoch"),
            req(
                "controls",
                "array<PortControl>",
                "one complete batch: every declared port once, descriptor order",
            ),
        ],
    },
    TypeSchema {
        name: "StepResult",
        source: "workers-v1 3",
        fields: &[
            req("batchId", "Id", "echoes the request"),
            req("appliedFromStep", "U64", "k"),
            req("nextStep", "U64", "appliedFromStep + 1"),
            req(
                "appliedControlsDigest",
                "Digest",
                "over validated canonical requested controls",
            ),
            req("observation", "WorldObservation", "boundary == nextStep"),
        ],
    },
    TypeSchema {
        name: "HelloParams",
        source: "ipc-v1 4",
        fields: &[
            req("sessionId", "Id", ""),
            req("expectedWorkerId", "Id", ""),
            req("role", "Role", ""),
            req(
                "supportedMajors",
                "array<int>",
                "nonempty, unique, 1..=65535",
            ),
        ],
    },
    TypeSchema {
        name: "HelloResult",
        source: "ipc-v1 4",
        fields: &[
            req("selectedMajor", "const", "1"),
            req("selectedMinor", "const", "0"),
            req("workerId", "Id", ""),
            req("incarnationId", "Id", ""),
            req("role", "Role", ""),
            req("buildDigest", "Digest", ""),
            req("contractDigest", "Digest", ""),
            req(
                "capabilities",
                "array<Id>",
                "unique; agent-step-v1 or world-step-v1 required for that role",
            ),
            req(
                "limits",
                "{maxAgents:int,maxPorts:int,workerThreads:int}",
                "1..=4 agents, 1..=4 ports, and the launcher allocation this worker runs in",
            ),
        ],
    },
    TypeSchema {
        name: "StatusResult",
        source: "ipc-v1 4",
        fields: &[
            req("state", "WorkerState", ""),
            opt("currentScope", "Scope|null", "null before initialization"),
            opt("activeRequestId", "DomainRequestId|null", ""),
            opt("lastCompletedRequestId", "DomainRequestId|null", ""),
            opt("lastBatchId", "Id|null", ""),
            req(
                "progressCounter",
                "U64",
                "advances on progress, not on status queries",
            ),
        ],
    },
    TypeSchema {
        name: "AcknowledgeParams",
        source: "ipc-v1 5",
        fields: &[req(
            "requestIds",
            "array<DomainRequestId>",
            "1..=16, unique",
        )],
    },
    TypeSchema {
        name: "AcknowledgeResult",
        source: "ipc-v1 5",
        fields: &[req(
            "acknowledged",
            "array<DomainRequestId>",
            "<= 16, unique, a subset of the request",
        )],
    },
    TypeSchema {
        name: "ShutdownParams",
        source: "ipc-v1 7",
        fields: &[req("reason", "Id", "")],
    },
    TypeSchema {
        name: "ShutdownResult",
        source: "ipc-v1 7",
        fields: &[req("stopping", "const", "true")],
    },
    TypeSchema {
        name: "TaskEvent",
        source: "workers-v1 4",
        fields: &[
            req(
                "id",
                "Id",
                "derived from epoch, source step, rule and ordinal",
            ),
            req("kindId", "Id", ""),
            req(
                "sourceStep",
                "U64",
                "the newly reached boundary, 0 for bootstrap",
            ),
            opt("agentId", "Id|null", ""),
            req("payload", "TypedValue", ""),
        ],
    },
    TypeSchema {
        name: "EpisodeRequest",
        source: "workers-v1 4",
        fields: &[
            req("kind", "EpisodeRequestKind", ""),
            req("reason", "Id", ""),
            req("outcome", "TypedValue", ""),
        ],
    },
    TypeSchema {
        name: "ViewDescriptor",
        source: "state-media-v1 2",
        fields: &[
            req("viewId", "Id", ""),
            req("width", "int", "1..=4096"),
            req("height", "int", "1..=4096"),
            req("format", "ViewFormat", ""),
            req("rowStride", "int", "exactly 4 x width"),
            req(
                "pixelAspect",
                "{numerator:int,denominator:int}",
                "positive integers <= 65535",
            ),
            req("observationDelaySteps", "int", "0..=8"),
        ],
    },
    TypeSchema {
        name: "ViewRef",
        source: "state-media-v1 2",
        fields: &[
            req("viewId", "Id", ""),
            req(
                "producedStep",
                "U64",
                "max(0, boundary - observationDelaySteps) for required sensory views",
            ),
            req(
                "pixels",
                "ArtifactRef",
                "listed attachment; byteLength == rowStride x height",
            ),
        ],
    },
    TypeSchema {
        name: "AudioDescriptor",
        source: "state-media-v1 2",
        fields: &[
            req("streamId", "Id", ""),
            req("sampleRate", "int", "8000..=192000"),
            req("channels", "int", "1..=8"),
            req("format", "AudioFormat", ""),
        ],
    },
    TypeSchema {
        name: "AudioRef",
        source: "state-media-v1 2",
        fields: &[
            req("streamId", "Id", ""),
            req("firstSample", "U64", "no overlap or rewind within an epoch"),
            req("sampleFrames", "int", "0..=192000"),
            req(
                "samples",
                "ArtifactRef",
                "byteLength == sampleFrames x channels x 4, finite f32",
            ),
            req(
                "discontinuity",
                "bool",
                "true on the first chunk after restore",
            ),
        ],
    },
    TypeSchema {
        name: "CaptureParams",
        source: "state-media-v1 5",
        fields: &[req("checkpointId", "Id", "")],
    },
    TypeSchema {
        name: "CaptureResult",
        source: "state-media-v1 5",
        fields: &[
            req("checkpointId", "Id", ""),
            req("boundary", "U64", "the committed boundary"),
            req("compatibilityDigest", "Digest", ""),
            req(
                "payload",
                "ArtifactRef",
                "listed attachment; digest required",
            ),
        ],
    },
    TypeSchema {
        name: "StageRestoreParams",
        source: "state-media-v1 5",
        fields: &[
            req("checkpointId", "Id", ""),
            req("sourceScope", "Scope", "provenance, not the new handles"),
            req("compatibilityDigest", "Digest", ""),
            req("payload", "ArtifactRef", "newly imported; digest required"),
        ],
    },
    TypeSchema {
        name: "StageRestoreResult",
        source: "state-media-v1 5",
        fields: &[
            req("checkpointId", "Id", ""),
            req(
                "restoreToken",
                "Id",
                "activates once, bound to scope and payload",
            ),
        ],
    },
    TypeSchema {
        name: "ActivateRestoreParams",
        source: "state-media-v1 5",
        fields: &[req("restoreToken", "Id", "")],
    },
    TypeSchema {
        name: "ActivateRestoreResult",
        source: "state-media-v1 5",
        fields: &[
            req("committedStep", "U64", ""),
            req("checkpointId", "Id", ""),
            opt(
                "observation",
                "WorldObservation|null",
                "required from an environment, null from an agent",
            ),
        ],
    },
    TypeSchema {
        name: "SessionDescriptor",
        source: "publishing-v1 3",
        fields: &[
            req("sessionId", "Id", ""),
            req("revision", "U64", ""),
            req("compositionDigest", "Digest", ""),
            req("schedulerId", "SchedulerId", ""),
            req("environment", "EnvironmentDescriptor", ""),
            req("taskSchema", "SchemaRef", ""),
            req(
                "agents",
                "array<AgentDescriptor>",
                "1..=4, unique agentId and portId, each portId declared by the environment",
            ),
            req("assets", "array<AssetRef>", "unique id"),
        ],
    },
    TypeSchema {
        name: "AgentDescriptor",
        source: "publishing-v1 3",
        fields: &[
            req("agentId", "Id", ""),
            req("portId", "Id", ""),
            req("profileDigest", "Digest", ""),
            req("datasetDigest", "Digest", ""),
            req(
                "indexDigest",
                "Digest",
                "geometry mapping needs this, not neuronCount",
            ),
            req("neuronCount", "U64", ""),
            req("rateRoles", "array<Id>", "<= 64, unique"),
            req("supportedStimuli", "array<Id>", "unique"),
        ],
    },
    TypeSchema {
        name: "CommittedSnapshot",
        source: "publishing-v1 3",
        fields: &[
            req("descriptorRevision", "U64", ""),
            req("publisherIncarnation", "Id", ""),
            req("scope", "Scope", "the committed boundary"),
            req("episodeId", "Id", ""),
            req("sequence", "U64", "monotonic within publisherIncarnation"),
            req("worldTime", "RationalNs", ""),
            req(
                "agents",
                "array<SnapshotAgent>",
                "1..=4, unique agentId, telemetry in profile role order",
            ),
            req("progress", "TypedValue", ""),
            req(
                "media",
                "{views:array<ViewRef>,audio:array<AudioRef>}",
                "declared attachments held through publication admission",
            ),
            req("eventIds", "array<Id>", "unique, task order"),
        ],
    },
    TypeSchema {
        name: "SnapshotAgent",
        source: "publishing-v1 3",
        fields: &[
            req("agentId", "Id", ""),
            req("telemetry", "AgentTelemetry", ""),
            opt(
                "selectedDecision",
                "TypedValue|null",
                "null exactly at boundary 0",
            ),
            opt(
                "appliedControls",
                "PortControl|null",
                "null exactly at boundary 0; the agent's assigned port",
            ),
        ],
    },
    TypeSchema {
        name: "TransitionTrace",
        source: "step-v1 8",
        fields: &[
            req("behaviour", "TraceBehaviour", "compared between runs"),
            req(
                "operational",
                "TraceOperational",
                "recorded, never compared: wall time and transport identities",
            ),
        ],
    },
    TypeSchema {
        name: "TraceBehaviour",
        source: "step-v1 8",
        fields: &[
            req("scope", "Scope", ""),
            req(
                "agents",
                "array<TraceAgent>",
                "sorted by agentId, independent of dispatch order",
            ),
            req("batchId", "Id", ""),
            req("controlDigest", "Digest", ""),
            req(
                "acknowledgedBoundary",
                "U64",
                "the world boundary the environment acknowledged",
            ),
            req(
                "observationBoundaries",
                "array<{viewId:Id,producedStep:U64}>",
                "sorted by viewId",
            ),
            req("outcomeIds", "array<Id>", "task outcome ids in task order"),
            req("eventIds", "array<Id>", "task event ids in task order"),
            req("publishedBoundary", "U64", ""),
        ],
    },
    TypeSchema {
        name: "TraceAgent",
        source: "step-v1 8",
        fields: &[
            req("agentId", "Id", ""),
            req("profileDigest", "Digest", ""),
            req("ticksAdvanced", "U64", ""),
            req("brainTicks", "U64", ""),
            req("remainder", "RationalNs", ""),
            req("decisionDigest", "Digest", ""),
            req("committedStep", "U64", "the commit acknowledgment"),
        ],
    },
    TypeSchema {
        name: "TraceOperational",
        source: "step-v1 8",
        fields: &[
            req("wallTimeNs", "U64", ""),
            req(
                "prepareRequestIds",
                "array<{agentId:Id,requestId:DomainRequestId}>",
                "",
            ),
            req("advanceRequestId", "DomainRequestId", ""),
            req(
                "commitRequestIds",
                "array<{agentId:Id,requestId:DomainRequestId}>",
                "",
            ),
            req("busCallIds", "array<BusCallId>", "call-<U64>"),
            req("deliveryIds", "array<OwnerToken>", "dlv-<U64> or own-<U64>"),
        ],
    },
];

/// The schema set as canonical-JSON-ready data.
pub fn schema_set() -> Value {
    let mut types: Vec<&TypeSchema> = SCHEMAS.iter().collect();
    types.sort_by_key(|t| t.name);
    let mut enums: Vec<&EnumSchema> = ENUMS.iter().collect();
    enums.sort_by_key(|e| e.name);
    let mut limits: Vec<&LimitSchema> = LIMITS.iter().collect();
    limits.sort_by_key(|l| l.name);

    obj(vec![
        ("contract", "fly-session-types".into()),
        ("version", Value::from(SCHEMA_SET_VERSION)),
        (
            "scalars",
            obj(vec![
                ("Id", "^[a-z0-9][a-z0-9._-]{0,63}$".into()),
                (
                    "U64",
                    "\"0\" or [1-9][0-9]*, <= 18446744073709551615".into(),
                ),
                ("Digest", "64 lowercase hexadecimal digits (SHA-256)".into()),
                ("BusCallId", "call-<U64>".into()),
                ("DomainRequestId", "req-<U64>".into()),
                ("OwnerToken", "dlv-<U64> or own-<U64>".into()),
                (
                    "ArtifactIdentity",
                    "storeId, artifactId and generation of a bus ArtifactRef".into(),
                ),
            ]),
        ),
        (
            "limits",
            Value::Array(
                limits
                    .iter()
                    .map(|l| {
                        obj(vec![
                            ("name", l.name.into()),
                            ("value", Value::from(l.value)),
                            ("source", l.source.into()),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "enums",
            Value::Array(
                enums
                    .iter()
                    .map(|e| {
                        obj(vec![
                            ("name", e.name.into()),
                            ("source", e.source.into()),
                            (
                                "members",
                                Value::Array(e.members.iter().map(|m| (*m).into()).collect()),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "types",
            Value::Array(
                types
                    .iter()
                    .map(|t| {
                        obj(vec![
                            ("name", t.name.into()),
                            ("source", t.source.into()),
                            (
                                "fields",
                                Value::Array(
                                    t.fields
                                        .iter()
                                        .map(|f| {
                                            obj(vec![
                                                ("name", f.name.into()),
                                                ("kind", f.kind.into()),
                                                ("required", Value::Bool(f.required)),
                                                ("constraint", f.constraint.into()),
                                            ])
                                        })
                                        .collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

/// The canonical JSON text of the schema set.
pub fn schema_set_json() -> Result<String> {
    canonical::canonicalize(&schema_set())
}

/// `contractDigest`: the SHA-256 of the canonical schema set.
pub fn contract_digest() -> String {
    canonical::digest_of(&schema_set()).expect("the schema set is canonicalizable")
}
