# Session artifacts, native media and recovery

Status: **draft 2**, 2026-09-18. [Flybus](bus-v1.md) owns generic artifact storage, delivery
ownership, retention and garbage collection. This document specifies the **domain meaning**
of those artifacts: native observations, clock association and coherent session checkpoints.
Read [session RPC](ipc-v1.md), [step ordering](step-v1.md) and [worker interfaces](workers-v1.md).

## 1. Use the bus ArtifactRef

Large payload fields use `ArtifactRef` from bus-v1, and every referenced artifact is listed
in the surrounding bus attachments. There is no separate BinaryRef, buffer-region registry,
coordinator lease endpoint, or Buffer.Release/Reclaim protocol. Profile/dataset release assets
use `AssetRef` (a persistent content identity); transient bus ArtifactRefs are not those assets.

The environment publishes one immutable native image. The coordinator may forward the same
owned handle to multiple agent Commit calls and publish it for presentation. Flybus creates
destination ownership before releasing the source. It does not send another copy of the
pixel bytes per recipient through its sockets.

An agent consumes/encodes pixels during Initialize/Commit and drops its handle when no longer
used. A renderer may keep its extracted handle after dropping the message; the DeliveryGuard
keeps the artifact alive until rendering has finished. A domain cached RPC result keeps its
own handles so replay remains valid after the original recipient consumes its delivery.

Content digests are optional on transient live frames, mandatory on checkpoint payloads and
persistent asset import. Ownership/index/byte-shape validation is always required. A digest
does not replace epoch or observation-time identity.

## 2. Native observation types

```ts
interface ViewDescriptor {
  viewId: Id;
  width: number; height: number;
  format: "rgba8"; rowStride: number;
  pixelAspect: { numerator: number; denominator: number };
  observationDelaySteps: number;
}
interface ViewRef { viewId: Id; producedStep: U64; pixels: ArtifactRef }
interface AudioDescriptor {
  streamId: Id; sampleRate: number; channels: number;
  format: "f32le-interleaved";
}
interface AudioRef {
  streamId: Id; firstSample: U64; sampleFrames: number;
  samples: ArtifactRef; discontinuity: boolean;
}
```

Epoch is inherited from the domain observation; the bus treats it as opaque payload. View
dimensions are integers 1..4096, rowStride exactly 4×width, no padded rows in v1. Pixel aspect
numerator/denominator are positive integers <=65535; observationDelaySteps is integer 0..8.
Pixels are top-left RGBA8 and artifact length equals rowStride×height. Other formats require
a media-schema change, not special-case code inside the router.

Required sensory views have producedStep equal to
`max(0, observation.boundary - observationDelaySteps)`. Bootstrap may repeat O[0] until the
declared pipeline delay fills. Beyond that, missing/extra-delay sensory input is a step
failure, not an arbitrary latest frame. Observer publication may omit/coalesce frames while
preserving each artifact's actual producing boundary.

Audio sampleRate is integer 8000..192000, channels 1..8, sampleFrames 0..192000 per chunk.
Samples are finite f32; artifact length is sampleFrames×channels×4. firstSample identifies
the sample position relative to the episode's configured audio origin, with intended PTS
firstSample/sampleRate. Crash restore preserves sample position under a new epoch; first
chunk marks discontinuity. Within an epoch, chunks cannot overlap or go backwards.

**Amendment, 2026-09-22 (MEDIA-01).** Two readings of the paragraphs above, made explicit
because they are now enforced:

- The bootstrap window is exactly the boundaries where `max(0, boundary - observationDelaySteps)`
  is zero, that is `boundary <= observationDelaySteps`. Inside it the repeated `O[0]` is the
  **same artifact**, not a fresh render of the same scene; outside it the producing boundary
  advances one per step, and a frame from any other boundary -- older or newer -- is a step
  failure. A producer therefore keeps a queue of `observationDelaySteps + 1` frames and nothing
  more, so there is no older frame available to substitute.
- Within an epoch, `discontinuity` marks a range the stream actually skipped. The first chunk
  after a restore marks it, and a later chunk may mark it when it starts past where the previous
  chunk ended; a chunk that continues the previous one exactly is continuous by construction and
  its flag is refused. Without that reading the restore rule is advisory, because a stream could
  set the flag on every chunk and satisfy it by accident.

The environment provides **native game output**. Sensor transformations belong to the agent
profile. Resizing for viewers, overlays, composition, audio mixing/resampling, encoding,
browser delivery and streaming belong to the application/presentation layer. No bus or
generic session configuration assumes a 1080p show or Twitch output.

640×480 RGBA at 60 fps produces 73.728 MB/s of raw image data. Artifact fan-out references
one stored object; reads and any staging/seal copy still consume memory bandwidth. This is
reasonable to measure before introducing codecs or pooled GPU buffers. Native dimensions
come from the backend, not a hardcoded GameCube or broadcast resolution.

## 3. Domain retention and backpressure

Use the same bus call/publish API for observations and artifacts. Bus ownership tracks bytes;
the session decides which observations are required and when they have been used.

| Use | Rule |
| --- | --- |
| Required agent input | Retain through encoding/Commit; no coalescing or overwrite |
| Step-result replay | Retain in the endpoint's current/previous-step cache until domain eviction |
| Spectator snapshot | Latest subscription, finite in-flight credits; release after actual use |
| Long rendering/storage job | Explicit artifact hold with a finite byte/count budget |
| Hot checkpoint | Coalesce only queued replaceable captures, releasing their holds |
| Durable checkpoint | Acknowledge after durable commit; reject/defer before capture when saturated |

Initial session defaults: two outstanding coherent captures and at most the bus-configured
latest/in-flight frame credits per observer. Presentation audio can target 250 ms and cap at
one second, but that is a presentation policy, not a bus or brain-clock requirement.

Budget cached step observations, active agent deliveries, retained latest and spectator holds
together. The producer dropping its handle does not free cached/queued/in-use objects.
A slow spectator exhausts its own credits; new latest messages replace its queued value.
If it violates configured resource policy, disconnect/restart that observer instead of freeing
live data or silently skipping simulation input. Global store exhaustion is an explicit fault
or pause condition; the router cannot guess that a particular live object is disposable.

No coordinator tracks per-reader socket acknowledgments or calls a producer's reclaim method.
The SDK and bus perform that bookkeeping. File-backed immutable mappings are safe after
unlink; physical pages disappear when all OS mappings close. Pooled reuse is deferred until
it provides equivalent safety. Bare handles in history are not persistent saved bytes.

## 4. Checkpoint identity and content

Exact-checkpoint sessions capture at Ready(k) or Paused(k), after all Agent.Commit replies,
with no Advance in flight. Block next Prepare until all participants supply immutable captures.

The manifest records:

- Envelope version, checkpoint ID and source session/epoch/step/episode/world time.
- Coordinator scheduler/configuration identity and exact port-to-agent map.
- Backend/content/patch/controller/parser/state-format compatibility.
- Per-agent profile/dataset/model identities, seed, tick count/remainder and payload digests.
- Task ledger, prior world inspection, per-agent executor state, next sensory/decision state
  or reproducible reconstruction inputs, admission state and event watermarks.
- Payload names, lengths and hashes, including external-helper state required for exact resume.

Use a new envelope version; specify exact byte layout before production files. The historical
letter-only chunk-name constraint is not silently widened, and FLYSIM01 remains separately
readable. Persist payload **bytes and durable content identity**, not transient bus storeId,
artifact IDs, ownership tokens, mappings or pointers.

A checkpoint writer owns the bus Artifact handles until bytes are committed or the job fails.
It then drops them; durable files are outside Flybus's ephemeral GC. On restore, the durable
store imports fresh immutable bus artifacts. sourceScope is provenance, while the new handles
belong to the current router/store. Broker retention is never a substitute for a checkpoint.

## 5. State RPCs over the bus

Common to workers advertising checkpoint-v1:

```ts
interface CaptureParams { checkpointId: Id }
interface CaptureResult {
  checkpointId: Id; boundary: U64;
  compatibilityDigest: Digest;
  payload: ArtifactRef; // listed attachment; digest required
}
interface StageRestoreParams {
  checkpointId: Id; sourceScope: Scope;
  compatibilityDigest: Digest;
  payload: ArtifactRef; // newly imported owned attachment
}
interface StageRestoreResult { checkpointId: Id; restoreToken: Id }
interface ActivateRestoreParams { restoreToken: Id }
interface ActivateRestoreResult {
  committedStep: U64; checkpointId: Id;
  observation: WorldObservation | null; // environment required, agent null
}
```

State.Capture uses the committed scope. It completes after immutable capture exists, not when
a backend save was requested. The cached reply retains its artifact until Worker.Acknowledge.
The coordinator/writer obtains its own live ownership before acknowledging that cache.

State.StageRestore uses a proposed **new epoch** at source boundary k and is allowed only
on an uninitialized replacement or a quiescent worker. Launcher configuration supplies the
expected profile/backend identities; no implicit warm-up/reinitialization changes the saved
brain. It validates into replacement state, without exposing mutations to the live session.

After every participant and coordinator state validates, State.ActivateRestore installs
each staged token under that new scope without advancing a tick. Tokens are bound to scope/
payload/checkpoint and can activate only once; duplicate domain requests replay the cached
reply, while a fresh request trying to reuse an activated token is a conflict.

The environment returns the coherent restored observation with fresh artifact references and
restored time. It cannot advance gameplay to manufacture it. Capture/reconstruction therefore
covers render/inspection state and any pending sensor pipeline. Agent state agrees with it;
do not replay reward or recalibrate merely to fill missing cached data.

If emulator validation requires mutation, stage a stopped replacement emulator. If that cannot
provide externally atomic resume, advertise episode-restart, not exact-checkpoint. After all
activation acknowledgments, install the coordinator's staged task/executor/admission state
and establish Paused(new epoch,k). Failure during activation never permits half a group to run.

## 6. Durable commit, router failure and recovery

Write payload/envelope temporary generation, fsync, rename, fsync directory, then atomically
write/fsync/rename the store manifest and fsync its directory. **Manifest commit is the durable
commit point.** Unreferenced temporary generations are not automatic restore candidates.

Publish distinct captured/queued/committed/failed/superseded events over the same bus. Only
durable completion produces a saved acknowledgment/high-water mark. Failed writes release
owned ephemeral captures according to retry policy, without reporting false durability.

After participant/coordinator/router failure:

1. Stop steps, abandon the epoch and fence old participants/routes.
2. Connect to a live router and select a complete compatible durable checkpoint.
3. Import its payloads as new artifacts; stage/activate every participant and coordinator.
4. Verify identity/boundary, flush old media/parser queues and publish recovery/discontinuity.
5. Establish Paused(k), then resume only after the group invariant holds.

A router restart loses ephemeral topics, queues, roots and correlations. Continuing with
old handles is invalid even if some mapped bytes survived. Reconnect is not transparent
mid-step recovery. The durable log records old/new epochs and abandoned step ranges; rollback
can lose post-checkpoint work. Durable input replay requires a separate application/session
journal policy, not an exactly-once claim about Flybus.

## 7. Episode reset

Reset differs from crash restore. The application selects a policy; the coordinator records
the old episode's result/abort and creates a new epoch/episode at step zero. World initial
state and retained/fresh brain components are explicit. Gain retention, eligibility/hold
clearing, calibration and first sensory input are part of the policy, tested independently.
Legacy Pokémon ratchet behavior remains in the legacy composition. Shared competitive worlds
never restore one player's environment independently of the other players.
