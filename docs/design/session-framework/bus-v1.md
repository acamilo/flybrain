# Flybus v1: a small Rust RPC and pub/sub bus

Status: **draft 1**, 2026-09-18. Selected architecture for the new framework, not implemented
runtime code. This document supersedes the earlier proposal for direct worker sockets plus
a separate application bus and coordinator-owned buffer release protocol.

**One library, one router, one wire protocol. Small messages carry metadata and artifact
handles; large immutable bytes live in a managed local store. Ownership follows deliveries
and explicit retention.** The router moves messages and tracks generic resource ownership.
Applications own orchestration, presentation, effects and streaming. Sessions own simulation
ordering. The bus knows nothing about brains, game frames, matches or Twitch.

## 1. Scope and implementation shape

Start with a Rust/Tokio library, an embeddable router and an optional small router executable.
The same client API supports an in-memory test transport and Unix-domain stream sockets.
All participants use router semantics even when colocated; there is no separate fast-path
RPC protocol to maintain. Python helpers may implement a thin binding to the same schema;
browser integration belongs to the application's presentation gateway.

```text
Application / supervisor ─┐
Session coordinator ──────┤                 managed artifact store
Agent workers ────────────┼── Flybus router ── immutable bytes
Environment backend ──────┤                 handles in messages
Presentation / recording ─┤
Audience adapter ─────────┘
```

V1 supports one trusted local deployment with a shared local store. It does not require NATS,
a service mesh, durable broker queues, cross-machine artifact transfer, load-balanced stateful
workers, global transactions, dynamic plugins or a video codec. A private backend adapter may
still speak an emulator's native pipes/protocol internally; that is not a second framework bus.

Suggested crate: `flybus`, with `wire`, `router`, `client`, `artifact`, and `transport` modules.
Split crates only when useful. Keep session/game types out of this library. Use Tokio, serde
and a checked framing layer; persistent histories/checkpoints stay in application/session storage.

## 2. Client API

Illustrative Rust surface (not yet implemented):

```rust
let bus = Client::connect(config).await?;
let service = bus.register("agent.fly-a", service_config).await?;
let reply = timeout(deadline, bus.call(target, "Agent.Prepare", payload, attachments)).await?;
let subscription = bus.subscribe("session.demo.snapshots", subscription_config).await?;
bus.publish("session.demo.snapshots", payload, attachments).await?;

let mut writer = bus.artifacts().allocate(size, content_type).await?;
writer.write_all(&pixels)?;
let frame = writer.seal().await?; // consumes writer; immutable Artifact handle
bus.publish("world.demo.frame", metadata, [("frame", frame.clone())]).await?;

let message = subscription.next().await?;
let image = message.artifact("frame")?;
drop(message);                 // image still owns the delivery guard
render(image).await?;          // last handle drop releases ownership
```

RPC, pub/sub and artifacts all use this client/connection. The artifact store is a data
structure/storage backend of the bus, not another messaging service. Bulk data does not
pass through the router's socket payloads or require a separate application data-transfer API.

`Artifact` is a read-only, cloneable handle. `ArtifactWriter` is unique and not cloneable;
sealing consumes its writable lifetime. Mapped slices cannot outlive their handle. Rust RAII
automates releases; other language bindings provide equivalent explicit close/context-manager
behavior. Garbage collection means reclaiming an unowned artifact, not inspecting game state.

## 3. Addressing and identities

| Type | Meaning |
| --- | --- |
| `routerId` | Fresh router/store incarnation; changes after restart |
| `clientId`, `clientIncarnation` | Configured participant identity and SDK client lifetime; reconnect creates a new incarnation |
| `connectionId` | Fresh connection; v1 does not resume its queues or delivery owners |
| `service`, `serviceIncarnation` | Named endpoint and opaque router-issued registration identity |
| `callId` | Unique RPC correlation ID for this client incarnation; not a domain operation ID |
| `topic`, `topicIncarnation`, `topicSequence` | Exact topic, declaration lifetime and acceptance order within it |
| `deliveryId` | One recipient's message delivery and artifact ownership root |
| `artifactId`, `generation` | Immutable byte object identity within a store incarnation |
| `ownerId` | A connection-owned delivery or explicit artifact hold |

Identifiers are bounded ASCII strings; scalar Id and U64 encodings match the common types
in [session RPC contracts](ipc-v1.md). Service/topic names use 1..192 characters from
`[a-z0-9._-]`, with no empty dot-separated segment. Exact names only in v1; wildcard routing
and queue groups are deferred. The router treats names as opaque addresses.

One live registration owns a service name. Duplicate registration fails; there is no implicit
round-robin balancing or replacement of a stateful agent. Registration returns the incarnation.
Callers pin it after discovery. If it changes, calls fail with `TARGET_CHANGED` rather than
silently reaching another brain/environment. `Worker.Hello` remains a domain capability RPC,
distinct from transport connection negotiation.

Service/topic access is configured per participant by the launcher/application. Presentation
subscribes to observations; it does not receive authority to invoke environment Advance.
Audience effects go through application/session admission. Naming a target is not authority
to control it. Public feed/control interfaces remain separate compatibility boundaries.

## 4. Wire envelope and framing

One connection handles both directions and every operation:

```text
u32 little-endian JSON byte length | UTF-8 JSON object
```

```ts
interface BusEnvelope {
  protocol: "flybus"; major: 1; minor: 0;
  id: Id; replyTo: Id | null;
  kind: "command" | "reply" | "delivery" | "notice";
  op: string;
  body: object;
  attachments: Attachment[];
}
interface ArtifactRef {
  storeId: Id; artifactId: Id; generation: U64;
  byteLength: U64; contentType: string;
  digest: Digest | null;
}
interface Attachment {
  name: Id; ref: ArtifactRef; ownerId: Id;
}
```

Maximum total JSON envelope is **65,536 bytes**. Up to 32 attachments, with unique names;
contentType is a nonempty ASCII string <=127 bytes. No pixel/base64/checkpoint bytes in JSON.
Artifact sizes are independent of envelope size. Domain schemas referencing artifacts MUST
enumerate every referenced artifact in attachments; generated client bindings enforce this.
The router validates attachment declarations/ownership, not the contents of domain payloads.

Commands have unique monotonically issued `id` serials per connection (canonical `msg-<U64>`).
Replies correlate with `replyTo`; routing notices/deliveries have router-generated IDs.
The router supplies authenticated sender/target metadata on deliveries; senders cannot forge
it by putting another participant's name in body. Domain `requestId` and callId remain distinct.

Reject duplicate JSON keys, invalid UTF-8, NaN/Infinity, unknown envelope fields, zero/oversize
frames and invalid ranges. Read length before allocating. Handle partial reads/writes and
serialize one writer per connection. There are no ancillary-FD tricks in the first file-backed
implementation. A future memory backend must retain the same client/ownership API.

### Connection negotiation

First command `bus.hello` has body `{clientId, clientIncarnation, supportedMajors}` and no
attachments. Its reply reports `{routerId, connectionId, selectedMajor, selectedMinor,
contractDigest, limits}`. The launcher provides expected client/registration privileges and
local endpoint/store configuration. Refuse incompatible majors or identity mismatch before
registration. Changes to these draft schemas change contractDigest; incompatible released
schemas require a major version bump.

The connection reader must dispatch incoming replies and requests without blocking on user
handlers. Blocking neural computation runs on dedicated workers; artifact I/O/hashing runs
outside the router's routing critical section. No mutable routing-state lock across slow I/O.

Example RPC command (the counter service is a generic test, not a built-in router feature):

```json
{
  "protocol": "flybus", "major": 1, "minor": 0,
  "id": "msg-7", "replyTo": null, "kind": "command", "op": "rpc.call",
  "body": {
    "callId": "call-2", "target": "example.counter", "expectedIncarnation": "service-a",
    "method": "Counter.Increment", "payload": { "amount": 1 }
  },
  "attachments": []
}
```

An image-bearing publish has the same envelope shape. Its payload contains dimensions/time
and an application-defined attachment binding; its attachment entry contains ArtifactRef and
ownerId. byteLength may be `"1228800"`, while the entire message remains a small JSON object.

## 5. Operation registry

All operations use the envelope above. Replies are `{ok:true, value:object}` or
`{ok:false, error:{code, message, dispatch}}`, where dispatch is `not-dispatched`, `dispatched`
or `unknown`. Message length includes this wrapper. Transport error != domain error.

| Command | Body / reply value | Semantics |
| --- | --- | --- |
| `service.register` | `{name, maxQueued, maxInFlight}` → `{serviceIncarnation}` | Exclusive endpoint, bounded capacities |
| `service.unregister` | `{name, serviceIncarnation}` → `{removed}` | Stop new calls; queued calls fail; dispatched results follow §6 |
| `rpc.call` | `{callId, target, expectedIncarnation:null|Id, method, payload}` → `{accepted, serviceIncarnation}` | Admission acknowledgment only, eventual rpc.result follows |
| `rpc.reply` | `{callId, requestDeliveryId, outcome}` → `{routed}` | Reply only by the registered recipient; outcome is domain success/error |
| `rpc.cancel` | `{callId}` → `{state}` | Best effort; only canceled-before-dispatch establishes no invocation |
| `topic.declare` | `{name, retained:"none"|"latest"}` → `{declared, topicIncarnation}` | Compatible redeclaration allowed; conflicting settings fail |
| `topic.clear` | `{name}` → `{cleared}` | Release retained topic root; does not invalidate deliveries |
| `topic.delete` | `{name}` → `{deleted}` | Only when no subscribers; releases retained state |
| `subscribe` | `{topic, mode:"latest"|"bounded", maxQueued, maxInFlight, replayLatest}` → `{subscriptionId, topicIncarnation}` | Exact topic; no durable history |
| `unsubscribe` | `{subscriptionId}` → `{removed}` | Discard queued messages; already delivered handles remain valid |
| `publish` | `{topic, payload}` → `{topicSequence, subscribers, replaced}` | Atomic publication admission/fan-out, not consumption |
| `delivery.consumed` | `{deliveryIds:Id[]}` → `{released}` | Idempotent; processing and all local artifact uses have finished |
| `artifact.allocate` | `{byteLength, contentType}` → `{artifactId, generation, ownerId, writeLocation}` | Reserve quota and private writable staging storage |
| `artifact.seal` | `{artifactId, generation, ownerId, digest:null|Digest}` → `{ref, ownerId}` | Finish immutable publication; writeLocation no longer valid |
| `artifact.open` | `{ref, ownerId}` → `{readLocation}` | Resolve an owned sealed object for read-only mapping, never return its bytes |
| `artifact.retain` | `{ref, ownerId}` → `{ownerId}` | Create independent explicit hold while source ownership still exists |
| `artifact.release` | `{ownerIds:Id[]}` → `{released}` | Drop explicit holds/staging writers; not another client's owners |

Names/arrays/ranges follow §3/4/9. Release batches contain 1..64 IDs. No attachments are
allowed on management commands except rpc.call/rpc.reply/publish. `outcome` is a bounded
domain object; an incoming RPC result has its own attachments and delivery ownership.

`accepted`, `routed`, `removed`, `declared`, `cleared`, `deleted`, `replayLatest` are booleans;
`subscribers`, `replaced`, `released` are U64 counts. Released counts count newly released
roots, so an idempotent repeat may report zero. Queue/credit requests are integers 1..65535
and cannot exceed configured limits. Method strings are 1..128 printable ASCII characters.
call target is a service name; expectedIncarnation is its registration ID, not worker process ID.
Location grants are `{storeId, relativePath}` resolved by the client beneath the configured
local store root; absolute paths, parent traversal and symlink escapes are rejected. They
are SDK-private and do not appear in the application's ArtifactRef. Runtime paths are not
committed into application schemas or source configuration.

Deliveries:

- `rpc.request`: `{deliveryId, callId, caller, target, serviceIncarnation, method, payload}`.
- `rpc.result`: `{deliveryId, callId, responder, serviceIncarnation, outcome}`.
- `topic.message`: `{deliveryId, subscriptionId, topic, topicIncarnation, topicSequence, replaced, payload}`.

`caller`/`responder` include clientId and clientIncarnation. Delivery attachment ownerIds are
replaced by the recipient's deliveryId; source owner tokens are never delegated verbatim.
`topicSequence` and counters are U64 strings. The router assigns these IDs; the SDK exposes
typed payloads plus Artifact handles. Required bounded notices are route removal, subscription
closure and call failure. If even notice capacity is exhausted, close the connection instead
of silently losing control-plane correctness; disconnect is itself a typed client failure.

## 6. RPC behavior

callId uses `call-<U64>` with increasing serials per connected client. Keep an admission
watermark plus active-call entries; reused/retired call IDs are rejected, never executed again.
Reconnecting creates a new clientIncarnation/connection rather than reviving its old calls.
An RPC targets one registered service, not a broadcast subject. Preserve first-dispatch FIFO
per caller/service; responses may complete out of order and correlate by callId. Service
dispatchers can answer status concurrently with a long mutation, subject to their domain
state rules. The router does not implement frame barriers or numerical ordering.

Admission validates route, pinned incarnation, size, quotas and every source artifact owner.
It establishes request-delivery roots atomically before accepting. Rejection establishes no
delivery and drops any provisional roots. An accepted call is not proof that its handler ran.
Before any request bytes can reach the target, mark it dispatched; subsequent transport loss
is conservatively an unknown execution outcome.

The service publishes a reply with its own owned artifact handles. The router establishes
caller-result ownership before accepting that reply. It keeps only bounded call correlation
metadata until result delivery is consumed or the caller detaches/disconnects. It is not an
indefinite RPC result cache. A second rpc.reply for the same call is rejected, not routed twice.
Responding does not release the request's delivery guard; the handler does so when finished.

**No automatic retry or failover.** A deadline belongs to the calling client. On timeout the
client may send rpc.cancel, but cancellation after dispatch cannot undo work. Domain retries
use a fresh bus callId containing the **same domain requestId/body**, pinned to the same
service incarnation. Endpoint-level deduplication supplies safe replay; the router does not
infer it from method names. Never route a retry automatically to a restarted worker.

Queued cancellation releases its queued artifact roots and returns `cancelled-before-dispatch`.
After dispatch return `execution-unknown`; keep the recipient's delivery alive until consumed
or disconnected. A later reply to a detached call returns `routed:false`, with no caller-result
roots. The service still owns any retained result; domain state may have changed.
If a terminal result is already admitted, cancellation reports `completed`; the client drains
and consumes any result it no longer exposes to its caller. A retired/unknown correlation
reports `call-gone`. These four strings are the complete rpc.cancel state enum. None authorizes
re-execution, and cancelling a future does not abandon incoming delivery ownership.

To replay an artifact-bearing response safely, endpoint code caches **Artifact handles plus
payload**, not bare references. That cache owns explicit holds or delivery guards until domain
acknowledgment/eviction. Re-delivery gets new delivery IDs pointing to the same immutable bytes.
Once the domain cache expires it returns RESULT_EXPIRED; it cannot regenerate the operation
merely because the transport correlation entry was removed.

## 7. Pub/sub semantics

Topic declaration does not teach the router what a topic means. An application can publish
session observations, presentation cues, dataset jobs or unrelated typed events through the
same API. There are no hardcoded frame/brain topics inside the router.

- `latest`: one queued value per subscription, replacing only an **undelivered** value.
  Replacing it releases that queue entry's artifact roots. Already delivered/in-use messages
  are never reclaimed early. maxQueued is exactly 1 in this mode.
- `bounded`: FIFO queue, no coalescing or silent loss. When required queue/owner capacity
  is unavailable, reject the publication with BACKPRESSURE before admitting any deliveries.
- maxInFlight credits are returned only by delivery.consumed, not socket write completion.
  A latest subscriber with all credits in use still has one replaceable queued value.

Take an atomic subscriber/retention snapshot at admission. Validate and reserve all required
queue entries and artifact-owner budgets before accepting. For a bounded subscriber overflow,
reject the **whole publish**; no partial fan-out or retained-latest update. On acceptance,
assign one topicSequence and create roots for every delivery and optional retained value.
Different topics have no total ordering. Multiple publishers on one topic follow router
acceptance order, which is not automatically a deterministic application event order.

Publication reply counts accepted subscriptions/replaced queue entries, not consumers that
processed data. `replaced` on a delivery reports how many undelivered messages were coalesced
since that subscription's preceding delivery. Sequence gaps can also arise from joining late;
they are not evidence of a simulation step being skipped.

Optional `retained:latest` holds one last message and its artifacts independent of subscribers.
New subscriptions with replayLatest enqueue it before subsequent accepted publications;
bounded mode preserves that order, while latest mode may coalesce it before delivery under
the ordinary latest rule. Replay uses the original topicSequence, a fresh deliveryId and
explicit roots. Without retention,
zero-subscriber publication retains no artifact ownership after admission. Clearing a topic
releases only its retained root, not active consumers. Topic count and retained bytes are capped.

There is no durable replay, automatic redelivery, or exactly-once processing claim in v1.
Deleting/redeclaring a topic creates a fresh topicIncarnation; a reset sequence cannot be
mistaken for continuation of the deleted topic. Old subscription deliveries retain their
original incarnation and ownership until consumed.
If an application requires history, its recorder persists events and exposes a normal RPC
for recovery/query. Bus admission, message consumption and durable storage acknowledgment
are three different events. The bus must not conflate them.

## 8. Artifact lifecycle and garbage collection

### 8.1 Immutable object lifecycle

```text
ALLOCATED / WRITING → SEALED → referenced by owners → last owner drops → COLLECTED
              └─ writer abandoned/disconnected ──────────────────────→ COLLECTED
```

`ArtifactRef` is an identity, not an address, filename or authority to read. Opening it requires
a current ownership root belonging to that connection. storeId is the router/store incarnation;
old handles fail after restart. V1 does not reuse an artifact ID/inode; generation is 1 and
remains in the contract for future pool implementations. Content hashes are optional for live
frames and mandatory for checkpoint/durable-content handoff, as specified by domain contracts.

The first storage backend uses runtime-configured local files, optionally on tmpfs. Producer
writes staging storage outside the message stream. Seal closes writable mappings/handles in
the SDK, checks length and any requested digest, then finishes an immutable store-owned
object before acknowledging. A correctness-first implementation may copy into a fresh sealed
inode; account for both allocations during sealing. No per-frame fsync for transient media.

Consumers resolve a store-issued readLocation through artifact.open and map/read it read-only.
Locations are private grants; they are not placed in application bodies or public browser feeds.
All filesystem access stays behind the client Artifact API. Do not inline binary data or create
a second bulk-transfer server just because it is stored outside the socket.

### 8.2 What owns an artifact?

The store tracks an ownership graph, not only a naive refcount incremented by every packet:

| Root | Lifetime |
| --- | --- |
| Producer explicit hold / active writer | Until last local handle releases, seal transfers its unique writer, or connection is lost |
| Accepted queued delivery | Until replaced/cancelled or transferred into recipient delivery ownership |
| In-flight delivery | Until message processing AND all extracted artifact uses finish |
| Retained latest topic | Until replaced, cleared, deleted, or router stops |
| Explicit retained hold | Until release; used for caches, rendering, checkpoint writes and forwarding |

Admission creates destination roots before the sender may relinquish source roots. A forward
or reply uses a live handle/owner; the client holds it until admission succeeds or definitively
fails. A timeout must not drop a source guard while an unsent operation might still be admitted.
The client cancels/discards the unsent frame or retains the guard until the transport outcome
is known; connection teardown ends that ambiguity for the old connection.

Every envelope must list its complete artifact set. Duplicate references in one delivery are
counted once. A retained topic and several consumers can reference the same physical bytes.
The router performs metadata updates only; it does not copy image bytes for fan-out.

### 8.3 Consumed means no remaining use

The incoming message owns a shared **DeliveryGuard**. Extracting an Artifact clones the guard;
dropping the message alone does not consume the delivery while a renderer/encoder still uses
its artifact. Local handle clones do not each require a bus round trip. Dropping the last
guard queues delivery.consumed through a bounded control lane.

V1 deliberately owns at delivery granularity: keeping one artifact from a message may keep
the other attachments alive too. For independent long-lived retention, artifact.retain creates
a specific explicit hold before the original guard is dropped. Domain RPC caches must use
that hold when they outlive message processing/credits. An acknowledgment of domain success
does not implicitly drop either the incoming guard or cached outgoing holds.

If an allocation/seal grant arrives after its caller abandoned the future, the SDK reactor
still processes and releases that grant. It must not leak an owner the application never saw.
An in-progress seal/copy has a bounded internal I/O hold; on producer disconnect it either
finishes cleanup or aborts safely, never publishes an ownerless object into a new connection.

Async task cancellation/drop of a response future is not necessarily consumption: the client
must own queued results until surfaced, explicitly discarded, or disconnected. Receivers must
await completion of asynchronous CPU/GPU use before releasing its guard. A pointer extracted
from a mapping cannot outlive its Artifact; FFI wrappers must enforce this lifetime explicitly.

Release commands are batched, idempotent and scoped to the connection that owns the IDs.
Delivery/hold IDs use monotonic per-connection serials. Keep issued watermarks plus active
owner maps; releasing an already retired ID is a no-op, a never-issued/future ID is an error.
This avoids a tombstone per frame forever. Control-lane exhaustion closes the connection
instead of silently losing releases and leaking an unbounded ownership graph.

### 8.4 Crash, disconnect and safe physical reclamation

On disconnect, unregister services/subscriptions; cancel queued deliveries and release that
connection's active writers/explicit/delivery roots. Retained topic roots remain router-owned.
Late replies and releases cannot attach to a new connection or service incarnation.

GC removes the registry entry and unlinks/closes the sealed object after its final root is
gone. Existing immutable file mappings may remain valid until the OS closes the last mapping;
do not overwrite their inode or reuse their bytes. Logical reclamation is not proof that a
disconnected process released its physical pages. The supervisor handles stuck processes;
resource measurements include OS mappings and client memory, not just registry totals.

No TTL may reclaim a live owned artifact. Limits may disconnect a consumer, triggering the
explicit cleanup above, but cannot overwrite memory under a renderer. Future pooled shared
memory must prove equivalent lifetime/generation safety before replacing immutable files.

Router restart creates a new routerId/storeId, loses routes/queues/retention and invalidates
all old handles. Live sessions fail their current epoch and use coherent recovery. Persistent
artifacts come from the application's durable store and are re-imported as new bus objects.
Orphan files from a stopped router are cleaned without treating them as durable checkpoints.

## 9. Bounds, scheduling and failure reporting

Configure limits explicitly; these defaults are a prototype starting point, not capacity data:

| Resource | Default |
| --- | ---: |
| Connected clients / services / topics | 64 / 256 / 512 |
| Subscriptions per client / total | 128 / 1024 |
| Control envelope | 64 KiB |
| Active calls per client | 64 |
| Service queued / in-flight calls | 16 / 16; worker dispatcher further limits mutations |
| Latest subscription queued / in-flight deliveries | 1 / 2 |
| Bounded subscription queued / in-flight deliveries | 64 / 16 |
| Active owners per client | 256 |
| Total artifact storage / per object | 512 MiB / 128 MiB |
| Per-client ordinary bounded queued envelope bytes | 1 MiB |
| Latest subscription slots | subscriptions × 64 KiB |
| Reserved management/reply lane | 128 frames and 1 MiB per client |

Reserve an owner allowance for lifecycle/results separately from ordinary telemetry; memory
quotas account for staging/seal copies, queued deliveries and caches. Ownership metadata is
bounded even if many roots share one artifact. Disk-full, allocation failure or hash mismatch
returns a typed artifact error and cleans provisional storage/roots.

The router fairly services clients. Replies, release, cancellation and route-health control
cannot be starved by telemetry. Preserve FIFO for calls to a target despite lane scheduling;
classification is an explicit generic envelope operation/policy, not a topic-name heuristic.
No indefinite wait inside the router on subscriber readiness or artifact I/O. Admission is
bounded; rejected callers choose their own retry/fail/pause policy.

Transport errors include `INVALID_ENVELOPE`, `VERSION_MISMATCH`, `NOT_AUTHORIZED`,
`NO_SERVICE`, `TARGET_CHANGED`, `BACKPRESSURE`, `CALL_GONE`, `ARTIFACT_UNSEALED`,
`ARTIFACT_GONE`, `OWNER_INVALID`, `QUOTA_EXCEEDED`, `STORE_FAILURE`, `ROUTER_LOST`, and the
three of section 12.
Before admission use dispatch:not-dispatched. Once dispatch might have occurred, report
unknown/dispatched conservatively; a caller-side timeout must not imply no mutation.

Bounded event subscriptions can reject publication; latest spectator subscriptions cannot
hold a required session transaction indefinitely. Per-client credit/owner budgets and
supervision enforce that distinction. Sustained pinned-artifact quota exhaustion is surfaced
as resource pressure, not solved by freeing live data. Session/app policies choose whether to
disconnect an observer, pause, or fail; the router does not know which outcome is appropriate.

## 10. Native-frame bandwidth check

At 640×480 RGBA, a frame is 1,228,800 bytes. At 60 fps, production is **73.728 MB/s**
(decimal); at 30 fps, 36.864 MB/s. These are planning dimensions; a backend advertises its
actual output. Two agent workers plus one presentation consumer can read the same immutable
frame object. Bus messages contain only references; there is no 3× byte fan-out through the
router. Readers still incur memory traffic/page faults, and renderer readback/seal copying
remain real costs. This is not a claim of zero-copy GPU capture or measured host performance.

The environment emits native game frames/audio. The application/presentation layer owns
resizing, overlays, compositing, browser delivery, codec choice and stream output. Do not put
1080p rendering, Twitch publishing or game-specific sampling logic in Flybus. A presentation
pipeline may itself exchange large artifacts through this same bus if useful.

## 11. Acceptance tests and implementation sequence

1. **Wire/router:** schema/framing, Hello, exclusive routes, pinned incarnations, request/reply,
   disconnect and bounds. In-memory transport must pass the same tests as Unix sockets.
2. **Pub/sub:** exact topics, FIFO/bounded rejection, latest coalescing, retained replay/clear,
   atomic fan-out and fair control/reply delivery under a saturated subscriber.
3. **Artifacts:** allocate/seal/read; publication before seal fails; fan-out owns one physical
   object; last consumer releases; retaining an extracted frame after message drop works.
4. **Faults:** sender drops after admission; consumer dies mid-read; reply is lost; queued frame
   is replaced; subscription closes with in-use deliveries; router restarts; old release arrives.
   No double-free, use-after-reuse, unbounded tombstones or hidden operation replay.
5. **RPC cache:** endpoint retains an artifact-bearing result, original caller consumes it,
   and a domain retry still returns valid bytes. Eviction drops the last cache hold correctly.
6. **Integration:** two parallel fake agents, complete-batch environment RPC, committed snapshot
   publication and a deliberately slow presentation consumer over the same router.
7. **Performance:** 640×480×60 artifact production with three consumers, one delayed; measure
   p50/p95/p99 RPC latency, router CPU, copy/readback cost separately, RSS, store live/peak bytes,
   outstanding roots, collection lag and queue lengths. Compare one/two/four agent schedules.

The first executable example should show a counter RPC, a pub/sub observer, and a frame
artifact held past message consumption in one small Rust program. No game or browser required.
Distributed simulation ordering remains the [session contract's](step-v1.md) responsibility.

## 12. Amendments

Draft 1 stands as written above. Each amendment below names something the draft requires but
left unnamed, and is dated. The implementation and the sentence-by-sentence audit behind these
entries are in the [conformance report](bus-conformance.md).

**2026-09-22, from the flybus conformance audit.** Three error codes, because section 9's list
is inclusive and these three refusals had no name:

| Code | Reason |
| --- | --- |
| `CONFLICT` | Section 3's duplicate registration, section 5's conflicting topic redeclaration and section 5's `topic.delete` with subscribers are refusals of a live claim, not a missing route, a quota or a bad envelope. |
| `NO_TOPIC` | Publishing to or subscribing to a name nobody declared is a missing topic, and `NO_SERVICE` names the service case only. |
| `ARTIFACT_MISMATCH` | Section 9's "hash mismatch returns a typed artifact error", plus a sealed length that disagrees with the allocation and a reference that disagrees with the artifact it names; `STORE_FAILURE` would blame the store for the caller's claim. |

**2026-09-22, same audit.** One added operation, because section 6 requires bounded call
correlation and gives no way to end it when a handler keeps reply authority after releasing the
request delivery:

| Command | Body / reply value | Semantics |
| --- | --- | --- |
| `rpc.responder.release` | `{callId, requestDeliveryId}` -> `{released}` | The recipient gives up reply authority for a dispatched call. The final release for an attached call retires the correlation and emits `call.failed` with dispatch `dispatched`: `CALL_GONE` while the route is live, `NO_SERVICE` after route loss. Request consumption (section 8.3) stays independent of it. |

Both amendments change `contractDigest`, which section 4 already provides for.

**2026-09-22, coordinator decision on the audit's contradiction 1.** Section 9's table row
"Per-client ordinary queued envelope bytes | 1 MiB" now reads "Per-client ordinary **bounded**
queued envelope bytes", and latest slots get their own row, "subscriptions × 64 KiB", because
section 7's unconditional one-slot guarantee and the structural 1/2 cap outweigh one imprecise
table row: a budget whose overflow rejects a publication cannot contain a subscription that
this same section forbids to reject one.

**2026-09-22, coordinator decision on the audit's contradiction 2.** Section 2's sketch no
longer passes a `budget` into `bus.call` and shows the deadline at the caller instead, because
section 5's wire contract for `rpc.call` has no budget field and section 2 is self-labelled
illustrative.
