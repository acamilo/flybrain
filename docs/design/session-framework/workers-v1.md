# Worker and task interfaces v1

Status: **draft 2**. Uses [Flybus](bus-v1.md) for every call/publication and the domain
types/outcomes in [session RPC](ipc-v1.md), ordering in
[step protocol](step-v1.md), and binary/state types in [state and media](state-media-v1.md).
All method results below are the `result` object inside the domain result carried by a bus
rpc.result. Large inputs/outputs use owned bus attachments, never another worker data channel.

## Method registry

| Method | Caller → receiver | State/owner |
| --- | --- | --- |
| `Worker.Hello` | Authorized caller → named worker service | Domain identity/capabilities after bus negotiation |
| `Worker.Status` | Authorized caller → named worker service | Read-only; responsive during compute |
| `Worker.Acknowledge` | Coordinator → worker | Bounded lifecycle-result retention |
| `Worker.Shutdown` | Coordinator → worker | Terminal lifecycle request |
| `Agent.Initialize` | Coordinator → agent | Uninitialized → Ready(0) |
| `Agent.Prepare` | Coordinator → agent | Ready(k) → Prepared(k) |
| `Agent.Commit` | Coordinator → agent | Prepared(k) → Ready(k+1) |
| `Environment.Initialize` | Coordinator → environment | Uninitialized → boundary 0 |
| `Environment.Advance` | Coordinator → environment | Boundary k → boundary k+1 |
| `State.Capture` | Coordinator → agent/environment | Immutable snapshot of committed boundary |
| `State.StageRestore` | Coordinator → agent/environment | Validate replacement state under new epoch |
| `State.ActivateRestore` | Coordinator → agent/environment | Install staged state; remain quiescent |
| `Environment.SaveSlot` | Coordinator → environment | Capability `gameboy-slots-v1`; record a slot at the committed boundary (section 7) |
| `Environment.RestoreSlot` | Coordinator → environment | Capability `gameboy-slots-v1`; boundary k under a new epoch (section 7) |
| `Agent.Rollback` | Coordinator → agent | Capability `legacy-ratchet-rollback-v1`; Ready(e,k) → Ready(e',k), no tick (section 7) |

State methods have payloads in [state and media](state-media-v1.md). Artifact lifetime and
message consumption are bus operations managed by the SDK, not Worker/Coordinator methods.
Worker.Acknowledge releases a domain result cache, distinct from consuming a bus delivery.
Task/executor interfaces below are local library methods, not extra communication protocols.

## 1. Shared data model

```ts
interface AssetRef {
  id: Id; digest: Digest; byteLength: U64; format: Id;
}
interface SensoryInput {
  boundary: U64;           // environment boundary being observed
  views: ViewRef[];        // only the views this agent is allowed to consume
  structured: TypedValue | null;
}
interface Stimulus {
  id: Id; kindId: Id; durationMs: number;
}
interface Reward {
  eventId: Id; ruleId: Id; value: number;
}
interface AgentTelemetry {
  brainTicks: U64;
  populationRateHz: number;
  rates: { roleId: Id; hz: number }[];
  learning: { enabled: boolean; updates: U64; changed: U64; signal: number };
}
// Amendment 2026-09-23: AgentTelemetry also carries
//   stimulusRemainingMs: number | null;  // pulse still running after the operation; null = reports none
```

`AssetRef` names persistent content in a preprovisioned local registry; it is not an arbitrary path or
URL for a worker to fetch. A profile artifact contains all effective numerical, sensory,
readout, learning and schema identities. Dataset artifacts must already be installed and
verified. No implicit runtime network download or latest-version selection is allowed.

Transient `ArtifactRef` is instead defined by Flybus and resolves only through owned handles.
Views and structured input are separate capabilities. A pixel-only profile rejects non-null
structured input. Max views per sensory input: 8; any individual TypedValue is at most 32 KiB
of canonical JSON, and the complete bus envelope must fit 64 KiB. Larger typed state uses
an explicit artifact-backed schema. Frame bytes never go into JSON.

`Stimulus.kindId` resolves through a profile-declared capability to an anatomical binding
and fixed drive, including supported duration bounds. A caller cannot specify arbitrary
neuron indices or change drive values. `durationMs` must be finite and >0. Arrays of stimuli
or rewards are bounded to 64 per operation and retain their supplied order.

Rates must be finite/nonnegative, unique by role ID and in profile-defined order, at most
64 entries. Learning values must be finite; integer counters fit U64. Reward values are
finite; shipped positive-only task profiles reject negatives. Empty rewards do not imply a
different numerical rule. `id`/`eventId` is unique within its outcome or command namespace;
the coordinator assigns stable IDs before sending a mutating request.

**Amendment, 2026-09-23 (RT-01a; operator decision of 2026-09-23).** `AgentTelemetry` gains
`stimulusRemainingMs: number | null`: the milliseconds of stimulation pulse still running when
the operation that reports the telemetry completed, or `null` for an agent that has no pulse to
report. The operator decided that sugar admission reads the legacy `reward_remaining` from the
last commit's telemetry (section 5 amendment), and no field carried it. It is finite and
nonnegative; it is a report, never an input. The field is required-and-nullable like every
optional field in these contracts, so every existing producer now writes `null`. It changes
`contractDigest`, which [session RPC](ipc-v1.md) section 4 already provides for.

## 2. Agent methods

### Agent.Initialize

Allowed only on an uninitialized agent with negotiated `agent-step-v1`. Scope is the new
epoch at step `0`; restore uses the state interface instead of Initialize.

```ts
interface AgentInitializeParams {
  agentId: Id;
  profile: AssetRef;
  seed: number;             // signed 32-bit integer, matching current RNG input
  initialInput: SensoryInput;
  initialDecisionContext: TypedValue;
  workerThreads: number;    // integer >=1; within launcher allocation
}
interface AgentInitializeResult {
  agentId: Id; profileDigest: Digest; tickDuration: RationalNs;
  warmupTicks: U64; committedStep: U64; // committedStep == "0"
  decisionContextDigest: Digest;
  telemetry: AgentTelemetry;
  graph: AgentGraph;
}
interface AgentGraph {
  datasetDigest: Digest; indexDigest: Digest; neuronCount: U64;
  rateRoles: Id[];          // <=64, unique; AgentTelemetry.rates is in this order
  supportedStimuli: Id[];   // <=64, unique; an undeclared kind is UNSUPPORTED
}
```

**Amendment, 2026-09-22 (PUBLISH-01).** `AgentInitializeResult` gains `graph`, because
[publishing-v1](publishing-v1.md) section 3 requires `datasetDigest`, `indexDigest`,
`neuronCount`, `rateRoles` and `supportedStimuli` in every published `AgentDescriptor` and no
worker method carried any of them. Without this the only available source is the composition
that asked for the agent, so a descriptor could only ever agree with itself and the section 3
rule that "geometry/spike mapping requires indexDigest, not merely the same number of neurons"
would have nothing to compare. Initialize is where the agent has just loaded its dataset and
built its index, so the attestation belongs there. `rateRoles` is the "profile-defined order"
section 1 already requires `AgentTelemetry.rates` to be in, and the result is refused when the
two disagree; `supportedStimuli` is the profile capability section 1 already requires a
stimulus kind to resolve through, and a kind outside it is refused with `UNSUPPORTED` before
the model is touched. It changes `contractDigest`, which [session RPC](ipc-v1.md) section 4
already provides for.

**Amendment, 2026-09-22 (SESSION-02).** `HelloResult.limits` gains `workerThreads`, an
integer >=1 reporting the allocation the launcher started that worker within, because
"within launcher allocation" above had no wire-level proof: the launcher passes the number to
the worker out of band, and a coordinator that is not also its own launcher had no contract
path to it. Hello is where a worker already proves its identity and reports its limits, so the
allocation belongs there. A caller asking for more than the worker reports is refused with
`BUSY` before the model is constructed, which this section already required; the amendment
only makes the number visible to whoever must respect it. It changes `contractDigest`, which
[session RPC](ipc-v1.md) section 4 already provides for.

The profile fixes warm-up/calibration behavior and supported schema versions. Validate inputs
and required roles before model construction. Install the initial sensory input, warm the
brain with learning disabled, calibrate the fixed readout and establish Ready(0). Do not
generate gameplay rewards or controls that advance the world during warm-up.

Seed is persisted as run configuration and state; the coordinator derives independent seeds
from its recorded master seed and stable agent IDs under a versioned derivation algorithm.
That algorithm is part of composition identity and MUST be specified/tested before the real
agent slice; hand-selected explicit seeds are supported for the first synthetic composition.
Identical explicit seeds are allowed only when the experiment intentionally declares them.
The profile artifact digest identifies the profile definition; the capture compatibility
digest additionally covers the resolved seed, numerical model version and effective instance
configuration. Never assume identical profile digests make differently initialized state
interchangeable without matching that instance configuration.

### Agent.Prepare

Allowed from Ready(k), or as an exact domain retry under the session RPC deduplication rules.

```ts
interface PrepareParams {
  agentId: Id; profileDigest: Digest;
  interval: RationalNs;
  decisionContextDigest: Digest;
  preStepStimulations: Stimulus[];
}
interface PreparedDecision {
  agentId: Id;
  ticksAdvanced: U64; brainTicks: U64; remainder: RationalNs;
  decision: TypedValue;
}
```

Verify the cached context digest from Initialize/last Commit, expected agent/profile and
interval. Apply pre-step stimuli, advance ticks and decode as specified by the step protocol.
The returned decision schema is the profile's registered intent schema. It may describe a
controller state or selected semantic action, but cannot assign a port or include hidden
task-inspection fields. Context may mask declared available actions; it cannot change the
readout weights, invent a default winner or inject arbitrary neural observations.

Successful response leaves the worker at Prepared(k). A duplicate returns the same decision,
ticks and remainder. It MUST NOT resample randomness, repeat stimulation or calibrate again.

### Agent.Commit

Allowed only at Prepared(k), matching the current transition and exact Prepare request.

```ts
interface CommitParams {
  agentId: Id;
  preparedRequestId: Id;
  nextInput: SensoryInput;             // boundary == k+1
  nextDecisionContext: TypedValue;
  rewards: Reward[];
  taskStimulations: Stimulus[];
}
interface AgentCommitResult {
  agentId: Id; committedStep: U64;     // k+1
  decisionContextDigest: Digest;
  telemetry: AgentTelemetry;
}
```

Validate the complete request and required owned artifacts before applying it. Follow exact input→
stimulation→reinforcement ordering in the step protocol. A missing required view is an error,
not zero input. An outcome requesting unsupported learning/stimulation is an error, not a
silent no-op. Disabled learning is a declared profile/run state, not “unsupported.”

No tick is executed in Commit. Cache its response before accepting the following Prepare.
The next context is retained for that Prepare; its canonical digest is returned and checked.
After encoding/copying and all asynchronous use finish, the worker drops its input Artifact
handles. The SDK consumes the delivery when the last associated guard disappears. A stored
input pointer must retain its handle. Cached replies retain their own artifact ownership.

### Agent interface implementation boundary

An agent worker bundles numerical model, sensor encoder and readout. It need not copy the
legacy `NeuralAgent::tick` call order: the existing service already orchestrates substeps.
Use a small adapter over the reference primitives and preserve the new specified ordering.
New numerical semantics require reference-first implementation and new model identities;
this protocol is not permission to change the pinned default kernel.

## 3. Environment methods

### Controller and descriptor types

```ts
interface ControllerSchema {
  schema: SchemaRef;
  buttons: Id[]; // <=32, unique, fixed order
  axes: { id: Id; range: "bipolar" | "unit"; neutral: number }[]; // <=16
}
interface PortControl {
  portId: Id;
  buttons: { id: Id; down: boolean }[];
  axes: { id: Id; value: number }[];
}
interface EnvironmentDescriptor {
  backendDigest: Digest; contentDigest: Digest; configurationDigest: Digest;
  stepDuration: RationalNs;
  ports: { portId: Id; controls: ControllerSchema }[];
  inspectionSchema: SchemaRef;
  views: ViewDescriptor[];
  audio: AudioDescriptor[];
  recovery: "exact-checkpoint" | "episode-restart";
  determinism: "fixed-build" | "unverified";
}
```

Every active port control must include every declared button and axis in descriptor order.
All IDs must match exactly; no duplicates, extra controls or omissions. Bipolar axes are
finite [-1,1]; unit axes [0,1]. Neutral lies in range. Do not silently clamp an out-of-range
caller value. Hardware-specific quantization/dead zones are backend configuration, applied
exactly once and tested against observed controls.

`fixed-build` asserts tested repeatability under the pinned configuration, not universal
bit-exact behavior across CPU architectures, GPU drivers or emulator versions. Those limits
must be in the backend implementation guide and run manifest. `unverified` cannot satisfy
an exact-replay production composition without an explicit scope change.

### Environment.Initialize

```ts
interface EnvironmentInitializeParams {
  backendConfig: AssetRef;
  taskConfig: AssetRef;
  episodeId: Id;
  portBindings: { portId: Id; agentId: Id }[];
}
interface EnvironmentInitializeResult {
  descriptor: EnvironmentDescriptor;
  observation: WorldObservation; // boundary 0, worldTime zero
}
interface WorldObservation {
  boundary: U64;
  worldTime: RationalNs;         // logical time since episode start; preserved on crash restore
  engineFrame: string | null;    // backend-defined signed counter; <=64 characters
  sensoryViews: ViewRef[];
  inspection: TypedValue;
  broadcastViews: ViewRef[];
  audio: AudioRef[];
}
```

Scope is new epoch step 0. Backend/task config artifacts identify exact game content,
patches, initial-state/setup policy, graphics/timing/parser and controller conversion.
No unresolved “latest” settings. Environment setup may be application-specific, but it is
declared lifecycle scaffold, not actions attributed to a fly. The environment is stopped
when it returns O[0] and cannot free-run during brain initialization.

The environment only needs backend-relevant portions of task setup, not reward rules or
neural policies. `taskConfig` resolves a declared setup configuration; the complete task
implementation and ledger stay in the coordinator.

**Amendment, 2026-09-23 (RT-01a; operator decision of 2026-09-23).** Three readings for the
Game Boy environment of the legacy composition ([legacy-gameboy-v1](legacy-gameboy-v1.md)
sections 8 and 9), stated here because they are about this method's shape:

- *Setup scaffold.* The backend configuration may declare setup frames the environment runs
  with every control neutral before it returns O[0]. The legacy composition declares exactly
  **one frame with no button down**, which is what its fresh start does; O[0] then has
  `engineFrame` `"1"`, `worldTime` `0/1` and no audio chunk. These frames are declared
  scaffold, attributed to no fly, and never a transition.
- *Inspection may be artifact-backed.* `inspection` is a `TypedValue` under the descriptor's
  schema; the legacy schema `gameboy-memory-inspection-v1` carries a 64-KiB memory image as an
  `ArtifactRef` (listed in the bus attachments) plus the ROM's digest. This is the "explicit
  artifact-backed schema" section 1 requires for typed state over 32 KiB, and it is the one
  bulk transfer per boundary the section 4 rule against per-byte remote reads asks for. The
  environment's only write to a running game remains the controller batch.
- *Audio.* The environment publishes native samples in the declared f32 format; converting a
  backend's integer samples to f32 is the environment's job (binjgb: `sample / 255`), and any
  filtering for listening -- the legacy DC blocker -- is presentation, applied by the edge.

### Environment.Advance

```ts
interface AdvanceParams {
  batchId: Id;
  controls: PortControl[];
}
interface StepResult {
  batchId: Id;
  appliedFromStep: U64; nextStep: U64;
  appliedControlsDigest: Digest;
  observation: WorldObservation;
}
```

Validate scope k, complete controls and identities before releasing a backend input barrier.
Batch IDs are unique within an epoch; reusing one for a different request/step is a conflict.
Apply the complete batch to its interval and advance one framework step. Record batch ID,
result and next boundary before acknowledging. StepResult's control digest is over validated
canonical requested controls; backend quantization does not silently change that definition.
Observed game-pad values, if provided, are separately schema-labeled inspection data.

The environment returns exactly boundary k+1, worldTime advanced by its stepDuration, and
the required sensory views or a typed failure. An adapter with measured input latency must
describe it in its versioned backend config and prove its frame mapping; it cannot claim an
unmeasured same-frame response. An emulator may execute internal cycles/polls, but cannot
hide multiple framework steps behind one result.

### Environment pause behavior

At a committed world boundary the backend already awaits the next Advance; normal session
pause does not need a separate per-frame RPC. When an emulator requires an explicit hardware/
CPU pause to hold that invariant, the adapter owns it and must prove it. Status/Shutdown
remain responsive. A render/audio worker may drain already-produced data while stopped,
but no new gameplay state may advance.

## 4. Coordinator-local task and executor interfaces

These are library interfaces in v1, not additional bus services. Equivalent typed interfaces
may be implemented in Rust; names below specify semantics rather than compilable code.

```text
Task.bootstrap(initialInspection, bindings)
  → perAgentDecisionContexts, progress, initialEvents

Task.evaluate_transition(scope, oldInspection, newInspection, appliedControls)
  → perAgentOutcomes, perAgentNextDecisionContexts, progress, events, episodeRequest

ActionExecutor.apply(scope, agentDecision, currentGameState, progressView, clock)
  → ControllerIntent, executionEvents

Task.capture / validate_restore / install_restore
ActionExecutor.capture / validate_restore / install_restore
```

Task owns a checkpointable ledger. The coordinator calls each transition evaluation exactly
once after its acknowledged world step and retains the output until all agent commits finish.
If task mutation is followed by failure, restore the group; never reevaluate against a later
observation. Task-produced outcomes are keyed by configured agent ID; unknown/missing agents
are errors. Every agent receives explicit outcome arrays, including empty ones.

`ControllerIntent` contains buttons/axes conforming to the assigned port's controller schema,
but not a port assignment. The coordinator supplies the port. Per-agent executor state is
private; a running macro may emit controls according to its declared policy, but only after
neural selection. The first implementation supports the stateless identity executor only.

**Amendment, 2026-09-23 (RT-01a; operator decision of 2026-09-23).** The legacy composition
declares the extension **`executor: pokered-macros-v1`**: its task and its action executor are
**one object** implementing both interfaces above, serving one agent on one port
([legacy-gameboy-v1](legacy-gameboy-v1.md) section 10). They share state that neither could
reach through a declared channel if split: the executor's scene observation is the `bound` set
the task hands the decoder, the macros read the task's exploration ledgers, and the macro layer's
progress signal feeds the ratchet. The executor reads the boundary's memory image and the ROM
`AssetRef`, never the emulator. "Stateless identity executor only" remains true of the
synthetic composition; a stateful executor is permitted exactly where a composition declares
one by name, and its state is captured or declared transient by that composition's restore
semantics (state-media-v1 section 4 amendment).

The executor's currentGameState is a coherent read-only inspector view at this boundary;
progressView supplies task history/objectives. It updates its selected action every step
(movement, path replanning, interaction, completion), not merely replaying a blind button
sequence. These game-aware inputs stay in the task/executor layer. For an external backend,
they arrive as typed observation/artifact data over the same bus, not per-byte remote reads.

Task contexts sent to the neural readout are typed, bounded, versioned and allowlisted by its profile.
They may express boot state or available actions. They are distinct from neural sensory
input and broadcast telemetry. A profile using game-state features as neural input must
explicitly declare structured sensing; the task cannot smuggle it into an opaque context.

Task events carry `{id, kindId, sourceStep, agentId:null|Id, payload:TypedValue}`. Event order
is task-defined and deterministic; `sourceStep` is the newly reached boundary k+1 for a
transition event (0 for bootstrap events). Event IDs are derived deterministically from
epoch, source step, task/rule and event ordinal, encoded as an Id. Rewards and stimulation
referencing these events must
have a configured recipient. Broadcast text is generated by task/presentation templates,
not arbitrary raw inspector memory or incoming chat.

`episodeRequest` is either null or `{kind:"terminal", reason:Id, outcome:TypedValue}`.
It requests a coordinator-owned policy transition after final reward commit; it cannot reset
the environment directly. Generic progress is a TypedValue, not mandatory Pokémon ladder data.

**Amendment, 2026-09-23 (RT-01a; operator decision of 2026-09-23).** `episodeRequest.kind` is
`"terminal"` or **`"rollback"`**. A rollback request asks for the composition's declared
rollback policy; its `outcome` is that policy's registered schema. The only policy defined is
`legacy-ratchet-rollback-v1` (outcome `{slotId, trigger}`), applied at the boundary the
transition just committed without a pause ([step-v1](step-v1.md) section 6 amendment). A
composition that declares no rollback policy treats the request as a task failure. It still
"cannot reset the environment directly": the coordinator applies the policy through the
section 7 methods.

## 5. Admission and audience boundary

The first synthetic implementation has no audience input. Later integration maps permitted
public requests into a coordinator admission record containing stable interaction ID, agent
ID, accepted epoch/earliest step and profile-supported stimulus. Only the coordinator can
place that stimulus in Prepare. Viewer names/chat never enter the neural worker contract.

Rate limits and target resolution occur before the step's command cut. A later-selected
UI focus or new match cannot retarget an already accepted interaction. Bus/domain RPC deduplication
does not itself define payment/redemption semantics across epoch recovery; a future public
v2 contract must specify accepted/applied/rolled-back/aborted states and reconciliation before
paid interactions are enabled. Do not inherit a claim of durable exactly-once stimulation
from these in-memory worker request caches.

**Amendment, 2026-09-23 (RT-01a; operator decision of 2026-09-23).** Sugar in the legacy
composition is admitted by the coordinator with the legacy rules -- the rate limiter and "no
overlap with an active pulse" -- reading the pulse from `stimulusRemainingMs` of the **last
completed commit**. The operator accepted that this value can be one commit old; both
consequences are bounded to one frame and are listed in
[legacy-gameboy-v1](legacy-gameboy-v1.md) section 15. Admitted sugar is a profile-supported
`reward-pulse` stimulus in the next Prepare's `preStepStimulations`. Because admission and
application are now separate, each admission record carries its interaction id and ends
`applied` or, if the epoch fails before that Prepare commits, `aborted`; the edge fulfils the
first and refunds the second, and the slice wiring the bridge tests both. This is the
accepted/applied/aborted distinction the paragraph above asks for, for this one interaction
kind; paid interactions in general still need the public v2 contract.

## 6. Health, shutdown and extensions

All workers implement the common Hello/Status/Shutdown/Acknowledge methods. Capture/restore
methods are in the state contract and mandatory only when exact-checkpoint capability is
advertised. Unsupported methods return `UNSUPPORTED`, mutation none.

New task-specific fields belong in registered TypedValue schemas. New worker capabilities,
variable-duration stepping, subscriptions or additional sensor modalities require a contract
change and shared fixtures. An unconstrained plugin dictionary is not a substitute for that.

## 7. Extension methods (amendment, 2026-09-23)

**Amendment, 2026-09-23 (RT-01a; operator decision of 2026-09-23).** The operator decided on a
full port of the live fly, whose ratchet rolls the *game* back to a saved state while the brain
continues. Neither half of that is expressible with sections 2 and 3: there is no method that
saves or restores world state outside a coherent group checkpoint, and none that installs a new
input in an agent without a transition. These three methods are that, generically shaped, each
behind a capability a worker advertises in `Worker.Hello`; a worker without it answers
`UNSUPPORTED`, mutation none. Payloads are in `fly-session-types` (`extensions`) and
`@flybrain/session-types`, in the session schema set.

```ts
// Environment.SaveSlot -- capability gameboy-slots-v1. Scope: the committed boundary (e, k).
interface SaveSlotParams { slotId: Id }                        // a slot the composition declares
interface SaveSlotResult { slotId: Id; boundary: U64;          // == scope.step
                           stateDigest: Digest; byteLength: U64 }

// Environment.RestoreSlot -- capability gameboy-slots-v1. Scope: (e', k), a NEW epoch.
interface RestoreSlotParams { slotId: Id; priorEpoch: Id;      // the environment is Ready(e, k)
                              policy: "legacy-ratchet-rollback-v1" }
interface RestoreSlotResult { slotId: Id; committedStep: U64;  // == k: no transition ran
                              observation: WorldObservation }  // boundary k, no audio chunk

// Agent.Rollback -- capability legacy-ratchet-rollback-v1. Scope: (e', k), the same new epoch.
interface AgentRollbackParams { agentId: Id; priorEpoch: Id;   // the agent is Ready(e, k)
                                policy: "legacy-ratchet-rollback-v1";
                                input: SensoryInput;           // boundary k: the restored world
                                decisionContext: TypedValue }  // for the next Prepare
interface AgentRollbackResult { agentId: Id; committedStep: U64; // == k: no tick ran
                                decisionContextDigest: Digest; telemetry: AgentTelemetry }
```

- Both environment methods run only at a committed boundary with no Advance outstanding.
  `SaveSlot` replaces the slot's contents with the world state and the frame on screen at that
  boundary; slots are environment state and belong to its `State.Capture` payload. It is one
  operation per boundary under the section-5 operation key of [session RPC](ipc-v1.md).
- `RestoreSlot` and `Agent.Rollback` move a participant from `(priorEpoch, k)` to
  `(scope.epoch, k)`; `priorEpoch` must differ from the scoped epoch, and after the move every
  request scoped to the prior epoch is `STALE_EPOCH`. The restored observation keeps the
  boundary number, `worldTime` and `engineFrame`, and returns fresh artifacts; it carries no
  audio chunk, and the next chunk marks a discontinuity (state-media-v1 section 2).
- `Agent.Rollback` applies the policy's agent half and nothing else: for
  `legacy-ratchet-rollback-v1`, clear decoder holds and plastic eligibility, install `input`
  without a tick, reset the readout's transient (held channel, blocked window, location from
  the context), keep the brain clock, membrane, RNG, rates and gains. No reward, stimulation or
  calibration. It is the only way to install an input outside a Commit.
- A failure of any of these methods mid-policy fails the epoch; recovery is the coherent group
  restore of [state-media-v1](state-media-v1.md) section 6. There is no partial rollback.
- `maxSlots` is 4 (a crate-chosen bound, published in the schema set).

The sequence that uses them is [step-v1](step-v1.md) section 6's amendment and
[legacy-gameboy-v1](legacy-gameboy-v1.md) section 11.
