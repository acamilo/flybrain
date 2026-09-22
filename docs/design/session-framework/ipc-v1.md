# Session RPC contracts over Flybus

Status: **draft 2**, 2026-09-18. The filename is retained for existing links. This document
now defines **domain contracts carried by [Flybus v1](bus-v1.md)**. It no longer defines a
separate socket protocol, direct worker connections, or coordinator-owned buffer service.
The [architecture index](README.md) states scope and precedence. Public feed/control v1 stay
unchanged; this is the new internal session path.

## 1. One transport, domain-specific meaning

Every session/worker RPC is a Flybus call to a named, incarnation-pinned service. Pub/sub,
application supervision and artifact bookkeeping use the same bus. The router moves messages;
the receiver validates its method payload and the [session step machine](step-v1.md).

The bus owns framing, connection identity, route registration, bounded delivery and artifact
ownership. This document owns Scope, model-related scalar types, worker capability negotiation,
domain operation deduplication and errors. Domain request identity is independent of the bus
callId: a safe retry has a new transport callId but the original domain requestId/body.

## 2. Common domain types

```ts
type Id = string;       // ^[a-z0-9][a-z0-9._-]{0,63}$
type U64 = string;      // "0" or [1-9][0-9]*; <= 18446744073709551615
type Digest = string;   // 64 lowercase hexadecimal digits (SHA-256)
interface Scope { sessionId: Id; epoch: Id; step: U64 }
interface RationalNs { numerator: U64; denominator: U64 }
interface SchemaRef { id: Id; version: number; digest: Digest }
interface TypedValue { schema: SchemaRef; value: object }
interface SessionRpcRequest { requestId: Id; scope: Scope | null; params: object }
```

All fields are required unless marked `?`. Schema version is integer 1..65535. Fractions
are reduced, denominators positive, durations positive; zero is encoded 0/1. Arithmetic is
checked. JSON numbers representing rates/rewards/controls are finite. Counters/clocks use
decimal strings. Task/profile schemas bound collections and numeric ranges before mutation.
First session composition limit: 4 agents, 4 ports and 64 rate roles per agent; these are
session/model limits, not limits on the number of application personas or generic bus clients.

Each TypedValue has a canonical JSON size limit of **32 KiB**, while the complete envelope
must still fit Flybus's 64-KiB maximum. Large typed state goes in a listed Artifact attachment
under an explicit schema, not an oversized inline object. Changing the old draft's 1-MiB
worker envelope to Flybus must not silently truncate a payload.

## 3. Request/reply mapping

Illustrative client call:

```text
bus.call(
  target = {service: "agent.fly-a", expectedIncarnation: pinnedRegistration},
  method = "Agent.Prepare",
  payload = {requestId: "req-41", scope: {sessionId, epoch, step: "41"}, params},
  attachments = ownedArtifactHandles
)
```

Flybus's eventual rpc.result `outcome` is one of:

```ts
interface SessionRpcSuccess {
  type: "result"; requestId: Id; workerId: Id; incarnationId: Id;
  scope: Scope | null; result: object;
}
interface SessionRpcFailure {
  type: "error"; requestId: Id; workerId: Id; incarnationId: Id;
  scope: Scope | null;
  error: { code: ErrorCode; message: string; mutation: "none" | "applied" | "unknown" };
}
```

Replies echo the original scope. The receiver identity and bus service incarnation must
match the negotiated worker. Bus route/admission failure is not a SessionRpcFailure produced
by the handler. A bus admission acknowledgment is not an Agent.Prepare/Environment.Advance
completion. Only a matching terminal domain reply resolves a simulation phase.

ArtifactRefs inside request/result payloads must be declared in bus attachments and backed by
live owned handles. Domain canonical-body digests include the references but exclude changing
bus callIds, deliveryIds and owner tokens. A cached result owns Artifact handles independently
of the first delivery; it is not a JSON object holding unowned pointers.

## 4. Worker negotiation and status

After bus connection/registration, call Worker.Hello (`scope:null`):

```ts
interface HelloParams {
  sessionId: Id; expectedWorkerId: Id;
  role: "agent" | "environment" | "coordinator";
  supportedMajors: number[];
}
interface HelloResult {
  selectedMajor: 1; selectedMinor: 0;
  workerId: Id; incarnationId: Id; role: "agent" | "environment" | "coordinator";
  buildDigest: Digest; contractDigest: Digest;
  capabilities: Id[];
  limits: { maxAgents: number; maxPorts: number };
}
```

The bus supplies caller identity; do not accept a forged caller in params. Bind a worker's
session authority to the expected coordinator identity/incarnation during negotiation and
initialization. Wrong worker/role, no common major or missing required capability refuses
the composition. Required capabilities are agent-step-v1 and world-step-v1 for their roles;
checkpoint-v1 and pixel-observation-v1 are conditional. Artifact transport capability is
negotiated once by Flybus, not as another worker memory API.

Worker.Status has params `{}` and the caller's last known scope (null before initialization):

```ts
interface StatusResult {
  state: "uninitialized" | "ready" | "preparing" | "prepared" | "advancing"
       | "committing" | "capturing" | "staged-restore" | "restoring"
       | "failed" | "stopping";
  currentScope: Scope | null;
  activeRequestId: Id | null; lastCompletedRequestId: Id | null;
  lastBatchId: Id | null; progressCounter: U64;
}
```

ProgressCounter advances on computational/phase progress, not on status queries. Dispatch
Status through the same service without waiting for a long numerical operation. One mutation
executes at a time; at most one may be pending, and normal stepping pipelines neither. The
router's larger RPC capacity is not permission to overlap worker mutations. Never hold a
simulation lock while waiting for network I/O, artifact resolution or release bookkeeping.

## 5. Domain idempotency and retention

`requestId` is `req-` plus a canonical U64 serial, increasing for newly issued operations
per caller/worker pair. Retries reuse it unchanged even though bus callId changes. Flybus
preserves first-dispatch order per caller/service; worker handlers maintain that request
admission order while allowing read-only status alongside compute.

The operation key for step mutations is `(sessionId, epoch, step, method, workerId)`.
There is at most one Prepare, Commit or Advance for that key.

- Same key/request/body returns its cached reply, with fresh bus delivery ownership over
  retained artifacts. It never repeats ticks, stimulation, controller execution or reward.
- Changed ID/body for an existing key is CONFLICT. Canonical comparison uses RFC 8785 over
  method, scope and validated params. A rejected duplicate does not undo the earlier result.
- Check retained request identity before phase checks or artifact dereferencing. A duplicate
  may arrive after the original input delivery was consumed; it needs only the cached result.
- Keep current/immediately previous step result records. Eviction never enables reexecution:
  highest-issued request serial and step watermarks reject expired retries/old steps.
- Original expired serial → RESULT_EXPIRED; fresh serial naming an old step → STALE_STEP.

An exact duplicate arriving while execution is active receives the terminal domain error
IN_PROGRESS for that **bus call**. The original bus call still completes normally. Retry or
query Status later; no second mutation is started. This avoids multiple terminal replies to
one bus call and does not mistake IN_PROGRESS for the original operation's failure.

Lifecycle/capture replies are retained until Worker.Acknowledge:
`params:{requestIds:Id[]}` (1..16), result `{acknowledged:Id[]}`. It drops domain cache handles,
not another consumer's bus delivery. Already released/unknown IDs are ignored. Serial
watermarks reject reuse after acknowledgment without an unbounded tombstone list.

**Amendment, 2026-09-22 (CONTRACT-01):** those ids are domain request ids in the `req-<U64>`
serial form, not arbitrary `Id`s. The serial watermark rule in the sentence above cannot reject
reuse after acknowledgment unless the acknowledged id carries its serial, so a bus callId or a
bare `Id` is refused there.

Bound unacknowledged lifecycle replies at 16, then BUSY before application. Status and
Acknowledge use a cache of their last 16 replies; current/previous step records have their
separate finite retention. Caches containing big artifacts consume bus owner/byte budgets;
configure capture limits consistently. Never evict a promised replay artifact but keep a
successful pointer-only reply. An intentionally expired result returns RESULT_EXPIRED.

## 6. Timeout and failure handling

Timeouts are measured on the caller's monotonic clock. Prototype defaults: probe after two
seconds without reply, fail after ten seconds without progress; long boot/capture have separate
budgets. These are failure-detection values, not a gameplay latency goal.

After an uncertain call:

1. Stop further world-step dispatch.
2. If the same bus/service/worker incarnation still exists, query Status or issue a fresh
   bus call with the original domain requestId/body and retained input attachments.
3. Resolve only a matching terminal result. Never repeat an Advance with a new domain ID.
4. If routes/ownership were lost, incarnation changed or retained result expired, fail the
   epoch and restore/reset the group.

Endpoint crash or bus restart is not covered by in-memory deduplication. Router/store restart
invalidates all transient artifacts and routes. Worker disconnect also invalidates its bus
owners and registration; v1 does not silently reattach that worker to an active epoch.
Recover coherently even if an OS process survived with some numerical state in memory.

## 7. Domain errors

| Code | Meaning |
| --- | --- |
| INVALID_ARGUMENT | Invalid schema/range, before mutation |
| UNSUPPORTED | Missing method/capability |
| IDENTITY_MISMATCH | Wrong session/profile/port/build/asset identity |
| STALE_EPOCH / STALE_STEP / FUTURE_STEP | Timeline/order mismatch |
| INVALID_PHASE | Wrong worker phase |
| CONFLICT | Existing logical operation with changed ID/body |
| IN_PROGRESS | Original operation still executing; duplicate bus call did not start work |
| BUSY | Domain capacity unavailable before admission |
| BUFFER_INVALID | Missing/unowned/mismatched artifact or invalid media shape |
| RESULT_EXPIRED | Safe replay is no longer available; never recompute to replace it |
| INCOMPATIBLE_STATE | Restore validation failed before activation |
| BACKEND_FAILURE / INTERNAL | Runtime fault, with explicit mutation certainty |

Messages are <=512 code points and exclude raw game memory/credentials. Errors after partial
mutation use unknown unless completion is established. No error authorizes skipping a fly,
pressing fallback controls, or continuing a partially committed match.

Worker.Shutdown, params `{reason:Id}`, returns `{stopping:true}` if responsive and terminates
the worker after replying. It does not imply saved state. Only configured supervisors may
invoke it; workers have no authority to shut down the coordinator. Shutdown/release notifications
travel on the same bus; there is no reverse lease socket or Buffer.Release/Reclaim RPC.
