# flybus

A small local RPC and pub/sub bus with immutable file-backed artifacts, for Tokio.

One library, one router, one wire protocol. Messages are small JSON envelopes carrying
metadata and artifact references. Large immutable bytes live in a file store the router
manages. Ownership follows deliveries and explicit holds. The router moves messages and tracks
ownership; it does not know what the messages mean.

This crate implements the Flybus v1 draft (session-framework design, `bus-v1`, draft 1 of
2026-09-18), using the scalar encodings of its companion `ipc-v1` (`Id`, `U64`, `Digest`).
Where this crate narrows or extends the draft, the difference is listed under
[Differences from the draft](#differences-from-the-draft). Every sentence of the draft's
sections 2 to 11 is audited against this code, with the test that proves it, in
`docs/design/session-framework/bus-conformance.md`.

Nothing in the crate is specific to a game, a brain or a stream. It is a workspace member and
no other crate depends on it yet.

## Layout

| Module | Contents |
| --- | --- |
| `wire` (public) | Scalars, strict JSON, `Envelope`, `ArtifactRef`, `Attachment`, `Location`, framing, `CONTRACT` and `contract_digest()` |
| `error` (public) | `ErrorCode`, `Dispatch`, `BusError` |
| `limits` (public) | `Limits` and its hello encoding |
| `policy` (public) | `Policy`, `Grants`, `Pattern` |
| `router` | `Router`, `RouterConfig`, `RouterStats`, `UnixListenerHandle`; the state machine in `router/state.rs` |
| `client` | `Client` and every handle type |
| `store` | The file store (router side) and location resolution (client side) |
| `transport` | `Transport`, the `Stream` trait |

Dependencies: `tokio`, `serde`, `serde_json`, `sha2` and `libc` (for `flock`,
`posix_fallocate` and `O_NOFOLLOW`), all already in the workspace lockfile.

## API

### Router

```rust
let mut config = RouterConfig::new("/run/example/bus-store"); // default Limits, closed Policy
config.policy = Policy::closed()
    .client("coordinator", Grants { call: vec![Pattern::prefix("agent.")], ..Grants::default() })
    .client("agent-a", Grants { register: vec![Pattern::exact("agent.fly-a")], ..Grants::default() });
let router = Router::new(config)?;                 // creates <root>/<storeId>, cleans orphans
let listener = router.listen_unix_as("/run/example/coordinator.sock", "coordinator").await?;
let transport = router.connect_in_memory_as("coordinator");
router.serve_as(any_async_read_write_stream, "coordinator");
router.stats();                                    // RouterStats (logical registry counters)
router.shutdown();                                 // notices, then closes every connection
```

- `Router` is `Clone`; all clones are one router. `serve_as`, `connect_in_memory_as` and
  `listen_unix_as` bind the launcher's expected participant id to the transport before Hello.
  A mismatching Hello is refused before registration or routing.
- The unbound `serve`, `connect_in_memory` and `listen_unix` entry points are only for explicit
  `Policy::open()` trusted/test deployments. Restricted policies refuse unbound transports.
  Identity in open/unbound mode is self-asserted and is not authentication.
- `RouterConfig::hello_timeout` defaults to 5 seconds. `max_clients` counts all accepted
  connections, pending and active.
- `router_id()`, `store_id()`, `store_root()` and `store_dir()` report the incarnation and
  paths.
- `RouterStats` has `connections`, `services`, `topics`, `subscriptions`, `calls` (correlation
  records, detached included), `active_calls`, `artifacts`, `sealed_artifacts`,
  `artifact_roots`, `owners`, `reply_capabilities`, `queued`, `store_bytes` and
  `retained_bytes`.
- `Limits` (defaults in brackets): `max_clients` [64], `max_services` [256], `max_topics`
  [512], `max_subscriptions_per_client` [128], `max_subscriptions` [1024],
  `max_active_calls_per_client` [64], `max_service_queued` / `max_service_in_flight` [16 / 16],
  `max_latest_in_flight` [2], `max_bounded_queued` / `max_bounded_in_flight` [64 / 16],
  `max_owners_per_client` [256], `reserved_owners_per_client` [64], `max_store_bytes`
  [512 MiB], `max_artifact_bytes` [128 MiB], `max_retained_bytes` [128 MiB],
  `max_queued_bytes_per_client` [1 MiB], `max_control_frames` / `max_control_bytes`
  [128 / 1 MiB]. `Router::new` refuses a configuration `Limits::validate` rejects.
- `Policy::open()` explicitly enables trusted unbound transports and admits any client id with
  every grant. `Policy::closed()` admits only the
  ids added with `.client(id, grants)`. `.with_default(Some(grants))` admits unlisted ids too.
  `Grants` lists `register`, `call`, `publish`, `subscribe` and `manage_topics` (declare, clear,
  delete) as `Pattern::Any`, `Pattern::exact(name)` or `Pattern::prefix(prefix)`. An empty list
  grants nothing.

### Client

```rust
let bus = Client::connect(transport, ClientConfig::new("coordinator", store_root)).await?;
let bus = Client::connect_unix(socket_path, ClientConfig::new("coordinator", store_root)).await?;
bus.info();          // SessionInfo: router_id, connection_id, identity, selected_major,
                     // selected_minor, contract_digest, limits
bus.closed();        // Some(reason) once the connection has closed
bus.control_errors();// fire-and-forget releases/consumes/unregisters the router refused
bus.close().await;   // flush queued releases, close, wait for the reader to stop
```

`ClientConfig` has `client_id`, `client_incarnation` (generated when `None`; a reconnect needs a
new one), `store_root` (the router's) and `control_lane_capacity` [4096]. `Client` is `Clone`.
The connection closes when the client and every handle made from it are dropped, or on
`close()`. After that, handles are inert and the router has released what they owned.

#### RPC

```rust
let mut svc = bus.register("agent.fly-a", ServiceConfig { max_queued: 16, max_in_flight: 16 }).await?;
while let Some(req) = svc.next().await {           // Request
    req.method(); req.payload(); req.caller();     // authenticated only on a launcher-bound transport
    let frame = req.artifact("frame")?;            // shares the request's delivery guard
    req.reply(outcome_map, &[("result", &artifact)]).await?; // Ok(true) routed, Ok(false) detached
}                                                  // dropping a Request consumes its delivery

let mut pending = bus.call("agent.fly-a", Some(&incarnation), "Agent.Prepare", payload, &[("frame", &frame)]).await?;
let result = pending.result().await?;              // RpcResult: outcome(), responder(), artifact()
let state = pending.cancel().await?;               // CancelState
let result = bus.call_and_wait(service, pin, method, payload, &attachments).await?;
```

- `register` is exclusive; `Service::incarnation()` is the id callers pin. Dropping the
  `Service` unregisters it.
- `call` returns once the router has admitted the call. The `PendingCall` then yields exactly
  one terminal outcome. `result()` is cancel-safe: wrap it in `tokio::time::timeout`, then call
  it again or `cancel()`. There is no deadline parameter; the deadline belongs to the caller.
- `cancel()` returns `CancelledBeforeDispatch`, `ExecutionUnknown`, `Completed` or `CallGone`.
  After any state except `Completed`, `result()` fails with `CALL_GONE`. The error's dispatch is
  `not-dispatched` only for `CancelledBeforeDispatch`.
- Dropping an unfinished `PendingCall` sends a best-effort `rpc.cancel`. The reactor consumes
  any result that still arrives.
- `Request::responder()` returns a `Responder` that can reply after the request itself has
  been dropped. Request delivery credit returns normally; a separate router-visible reply
  capability preserves correlation until the last `Request`/`Responder` is dropped or a reply
  is admitted. If the final capability is dropped while the caller is still attached, the call
  terminates with `CALL_GONE` and dispatch `dispatched` while the route remains live, or the
  existing `NO_SERVICE`/`dispatched` terminal if the route has ended. Capabilities share the
  service connection's owner bound.
- A call fails with `call.failed` data in its `BusError`:
  - Queued calls whose service unregisters or disconnects fail with `NO_SERVICE` and dispatch
    `not-dispatched`.
  - Dispatched calls whose service disconnects without replying fail with `NO_SERVICE` and
    dispatch `dispatched`.
  - Dispatched calls whose last responder is released without replying fail with `CALL_GONE`
    and dispatch `dispatched`.
  - A lost connection fails pending calls with `ROUTER_LOST` and dispatch `unknown`.

#### Pub/sub

```rust
bus.declare_topic("session.demo.snapshots", Retained::Latest).await?;  // TopicInfo
bus.clear_topic(name).await?;    // bool: a retained value was released
bus.delete_topic(name).await?;   // bool; CONFLICT while it has subscribers
let mut sub = bus.subscribe(name, SubscriptionConfig::latest().in_flight(1).replay(true)).await?;
let receipt = bus.publish(name, payload, &[("frame", &frame)]).await?; // topic_sequence, subscribers, replaced
let msg = sub.next().await;      // Option<Message>; try_next() does not wait
msg.topic_sequence(); msg.replaced(); msg.topic_incarnation(); msg.payload(); msg.artifact("frame")?;
```

- `SubscriptionConfig::latest()` is 1 queued and 2 in flight. `SubscriptionConfig::bounded()`
  is 64 queued and 16 in flight. `.queued(n)`, `.in_flight(n)` and `.replay(bool)` adjust them.
- A credit returns only when the delivery is consumed: when the `Message` and every artifact
  or `ArtifactFile` taken from it have been dropped.
- A retained replay into a bounded subscription is admitted atomically under the same queued
  envelope-byte quota as an ordinary bounded publication. Latest replay remains outside that
  byte pool and is bounded by one slot per subscription.
- Dropping a `Subscription` unsubscribes and discards its queue. Messages already handed out
  stay valid.

#### Artifacts

```rust
let mut writer = bus.artifacts().allocate(len, "image/x-rgba").await?; // ArtifactWriter: io::Write
writer.write_all(&pixels)?;
let frame = writer.seal().await?;              // or seal_with_digest(Some(sha256_hex))
frame.reference();                             // ArtifactRef
let bytes = frame.read_all().await?;           // Vec<u8>
let file = frame.open().await?;                // ArtifactFile: io::Read + io::Seek, keeps the handle
let kept = frame.retain().await?;              // an independent explicit hold
```

- `Artifact` is a read-only, cloneable handle on one owner: a delivery or an explicit hold.
  Its last clone (including any `ArtifactFile`) releases that owner.
- `ArtifactWriter` is unique. Dropping it unsealed releases the staging storage. Writes past
  the allocated length fail. Unwritten bytes read as zeros.
- Sealing copies staging into a fresh read-only (0444) file and unlinks staging. A descriptor
  the producer kept or duplicated afterwards writes only to the unlinked staging inode. Sealing
  therefore needs quota for both copies while it runs.

### Errors

`BusError { code: ErrorCode, message, dispatch: Dispatch }`. The codes are the draft's
`INVALID_ENVELOPE`, `VERSION_MISMATCH`, `NOT_AUTHORIZED`, `NO_SERVICE`, `TARGET_CHANGED`,
`BACKPRESSURE`, `CALL_GONE`, `ARTIFACT_UNSEALED`, `ARTIFACT_GONE`, `OWNER_INVALID`,
`QUOTA_EXCEEDED`, `STORE_FAILURE` and `ROUTER_LOST`, plus `CONFLICT`, `NO_TOPIC` and
`ARTIFACT_MISMATCH`. `Dispatch` is `not-dispatched`, `dispatched` or `unknown`. Refusals
before admission are `not-dispatched`. A command in flight when the connection is lost reports
`unknown`.

## Wire summary

- Frames are a `u32` little-endian length followed by 1..=65,536 bytes of UTF-8 JSON. The length
  is checked before any allocation.
- Every frame is parsed strictly:
  - Duplicate keys at any depth, invalid UTF-8, non-finite numbers and trailing bytes are
    refused. Nesting is limited to 128 levels.
  - Unknown fields are refused in the envelope, attachments, references and every management
    body. `payload` and `outcome` are opaque objects.
- Frame or envelope errors close the connection after a `connection.closing` notice. They
  include a bad length, bad JSON, an unknown envelope field, a wrong kind, a non-null `replyTo`,
  a command id that is not canonical or does not increase, a command before `bus.hello`, a
  second hello, and a major/minor other than 1.0 after hello. A body-level error (unknown op,
  bad field, out-of-range value, attachments on a management op) gets an `INVALID_ENVELOPE`
  reply and the connection stays up.
- The SDK applies symmetric checks to router frames: exact 1.0 version, strictly increasing
  canonical `bus-<U64>` ids, kind/replyTo/attachment direction rules, expected reply operation,
  delivery correlations and complete strict reply, delivery and notice body parsing.
- Ids:
  - Commands: `msg-<U64>`, strictly increasing per connection.
  - Router envelopes: `bus-<n>`. Connections: `conn-<n>`. Services: `svc-<n>`. Topics:
    `top-<n>`. Subscriptions: `sub-<n>`. Calls: `call-<U64>`, increasing per client.
  - Artifacts: `a-<n>`. Deliveries: `dlv-<n>`. Holds and writers: `own-<n>`.
  - `routerId` is `router-<16 hex>` and `storeId` is `store-<16 hex>` (same tag).
  - Delivery and hold serials have separate per-connection watermarks.
- Notices:
  - `call.failed {callId, code, message, dispatch}`
  - `route.removed {name, serviceIncarnation, reason}`
  - `subscription.closed {subscriptionId, topic, topicIncarnation, reason}`
  - `connection.closing {code, message}`
- The hello reply's `limits` object gives counts as JSON integers and byte sizes as U64 strings.
  Its fields: `maxEnvelopeBytes`, `maxAttachments`, `maxBatch`, `maxClients`, `maxServices`,
  `maxTopics`, `maxSubscriptionsPerClient`, `maxSubscriptions`, `maxActiveCallsPerClient`,
  `maxServiceQueued`, `maxServiceInFlight`, `maxLatestInFlight`, `maxBoundedQueued`,
  `maxBoundedInFlight`, `maxOwnersPerClient`, `reservedOwnersPerClient`, `maxControlFrames`,
  `maxStoreBytes`, `maxArtifactBytes`, `maxRetainedBytes`, `maxQueuedBytesPerClient` and
  `maxControlBytes`.
- `contractDigest` is the SHA-256 of `wire::CONTRACT`, a text listing of every operation,
  delivery and notice shape. The client refuses a router whose digest differs from its own.

## Semantics worth knowing

- **Admission is all or nothing.** `rpc.call`, `rpc.reply` and `publish` validate every
  attachment and every bound before changing any state. A refused publication creates no
  delivery, does not move the retained value and spends no topic sequence number. A refused
  syntactically valid call id advances the issued-id watermark even when semantic admission is
  refused, while creating no call, delivery or root. Reuse or decrease is rejected.
- **Ownership.** Each sealed artifact has a root count. Roots are held by queued deliveries,
  delivered-but-unconsumed deliveries, explicit holds and a retained topic value. A delivery
  that names one artifact twice holds one root. Fan-out adds roots and never copies bytes. When
  the last root goes, the artifact leaves the quota and its file is unlinked, outside the router
  lock. A process that still has the file open keeps its pages until it closes it.
- **Owner budget.** `max_owners_per_client` counts deliveries handed to the client and not
  consumed, explicit holds and writers. Topic deliveries, holds and writers may use all but
  `reserved_owners_per_client`; the reserve is for RPC requests and results. A topic delivery
  over budget waits in its queue. A hold or allocation over budget fails with `QUOTA_EXCEEDED`.
  Request reply capabilities consume the same finite bound independently of request delivery
  credit, preventing retained responders from growing detached correlation state without limit.
- **Lanes.** The router sends each client its replies and notices first, then RPC requests and
  results, then topic messages. Topic messages still get one frame after 16 consecutive
  higher-priority frames. Router envelope ids are assigned only after this scheduler selects
  the next item, so they increase in actual write order. Replies and notices waiting for one client are bounded by
  `max_control_frames` / `max_control_bytes`; exceeding either closes that client with a
  `connection.closing` notice. The client sends consumes, releases and cancels ahead of
  ordinary commands and batches up to 64 ids per command. A full client control lane closes the
  connection rather than drop a release.
- **No waiting on readers.** A client that stops reading stalls only its own writer task. The
  router keeps admitting until that client's queues refuse, then returns `BACKPRESSURE` to
  publishers of bounded topics. Connection teardown is ordered against each synchronous
  `poll_write`/`poll_flush` call without holding a mutex across an await: it marks the stream
  closing once, without waiting, and only then waits for a poll already in progress, so no
  later poll reaches the transport however long the writer had been holding it. Teardown either
  observes a complete frame before reclaiming its delivery owner, or marks a partial frame
  canceled before cleanup and appends no final notice to the truncated stream. At a frame
  boundary, normal final notices are still attempted.
- **RPC.** First dispatch is FIFO per service, which includes per caller. A call is marked
  dispatched when its bytes are about to be written. Replies correlate by call id and may
  arrive in any order. A second reply to one call fails with `CALL_GONE`. A reply to a detached
  call returns `routed:false` and creates no roots. Unregistering a service fails its queued
  calls; dispatched calls can still be answered while a `Request` or `Responder` retains reply
  capability. Both cancel/consume orderings retire cleanly.
- **Seals** copy (and hash, when a digest was given) on Tokio's blocking pool, alongside the
  connection's reader, so a large copy does not hold up that client's releases. If the writer is released or disconnects
  mid-seal, the copy is discarded and the artifact is never published.

## Differences from the draft

1. **Extra error codes.** `CONFLICT` covers a duplicate registration, a conflicting topic
   redeclaration and deleting a topic that has subscribers. `NO_TOPIC` covers publishing or
   subscribing to an undeclared topic. `ARTIFACT_MISMATCH` covers a seal whose length or digest
   is wrong, and a reference that disagrees with the artifact it names. All three are now
   amendments to the draft (bus-v1 section 12), which its inclusive error list allows.
2. **Topics must be declared.** Publish and subscribe fail with `NO_TOPIC` otherwise.
   `topic.delete` of an unknown topic returns `deleted:false`. `topic.clear` of an unknown
   topic returns `NO_TOPIC`.
3. **When notices are sent.**
   - `subscription.closed` is sent only when the router shuts down, because `topic.delete`
     requires no subscribers and nothing else closes a subscription.
   - `route.removed` goes to callers with queued or dispatched calls on the removed
     registration, not to every client.
   - `connection.closing` is an extra notice that precedes every router-initiated close.
4. **Byte budgets.**
   - `max_queued_bytes_per_client` counts only `bounded` subscriptions. A `latest` slot is bounded
     by subscription count times envelope size.
   - `max_retained_bytes` (not in the draft's table) counts the artifact bytes pinned by
     retained values, once per topic.
5. **Delivery size.** Admission computes the delivery's size with the router-added ids at their
   longest. If that would exceed 65,536 bytes it refuses with `INVALID_ENVELOPE`, so an inbound
   envelope near the limit can be refused even though it fits.
6. **Identity.** One live connection per client id. The router remembers each client id's last
   incarnation and refuses its reuse. Launcher-bound `*_as` endpoints authenticate the Hello id;
   trusted open/unbound endpoints do not. With an open policy this is one small record per
   distinct client id ever seen.
7. **Seal reply.** The reply's `ownerId` is the writer's own id, now an explicit hold.
8. **No `budget` argument on calls, and no router executable.** Timeouts are the caller's
   (`tokio::time::timeout` plus `cancel`). The draft's executable is optional; embed `Router`.
9. **Wire strictness.** Management bodies reject unknown fields. After hello, envelopes must
   carry `minor: 0`.
10. **Reply capability release.** The SDK sends `rpc.responder.release {callId,
    requestDeliveryId} -> {released}` when the last local reply capability is dropped, an
    operation the draft does not list and now carries as an amendment (bus-v1 section 12). This
    keeps request consumption independent from late-reply correlation while bounding that
    correlation under the service connection's owner limit. For an attached dispatched call,
    final release atomically retires the correlation and caller slot and emits `call.failed`
    with dispatch `dispatched`: `CALL_GONE` while the route remains live, or `NO_SERVICE` after
    route loss.

## Limitations

- **One host, one user.** Clients must run as the router's user and see the same store root.
  The store directory is 0700, staging files 0600, sealed files 0444 and the socket 0600. Bus
  authority (grants, connection-scoped owner ids, location grants) is enforced on bus
  operations, not on the filesystem: a process running as the same user can read, mutate,
  replace, chmod or unlink store files directly and can deny service. Mode 0600 authenticates
  only the shared OS user, not a Flybus participant.
- **No memory maps.** Artifacts are read through `std::fs::File` (`ArtifactFile`) or
  `read_all()`; there is no mmap API. `ArtifactFile` reads are blocking I/O.
- **Stats are logical.** `RouterStats` counts registry entries. Unlinked files that are still
  open, client memory and OS pages are not in it.
- **Fairness is modest.** Fairness between clients is Tokio's scheduling plus a yield after
  every 32 commands a connection sends. Within a connection, subscriptions and services are
  served round robin. There is no weighting.
- **Orphan cleanup needs a restart.** Store directories left by a stopped router are removed
  only when a new router starts on the same root. A directory counts as orphaned when it carries
  the store marker and its `flock` is free.
- **Rust only.** There are no other language bindings.
- **Two perf runs, not capacity data.** `tests/perf.rs` (ignored by default) measures 640x480
  RGBA frames at 60 Hz over a Unix socket to three latest-mode consumers, one delayed 40 ms per
  frame, with 1, 2 and 4 agent services called every frame. The router gets its own two-thread
  runtime with a distinct thread name, so its CPU is separable from the clients' in the same
  process. Two release runs on a shared 4-CPU development VM: producer copy into staging p50
  0.4 ms, seal copy p50 1.1 to 1.4 ms, consumer readback p50 0.7 to 1.0 ms, publish admission
  p50 0.4 ms, RPC round trip p50 0.5 to 1.1 ms; router 0.18 to 0.22 cores, whole process 0.33
  to 0.46; 11 to 17 MB RSS; store peak 3.7 MB and zero live after drain. The second run's p99s
  were three to five times the first's because other work shared the host. The full table, and
  what it does not claim, are in `docs/design/session-framework/bus-conformance.md`.

## Tests

```text
cargo test -p flybus                                              # unit + integration
cargo test --release -p flybus --test perf -- --ignored --nocapture  # the measurement above
cargo run -p flybus --example demo                                # counter RPC, observer, held frame
```

Every integration test runs twice, once over the in-memory transport and once over a Unix
socket, through the same router code:

- `tests/wire.rs`: hello negotiation and refusals, malformed frames of every kind, body errors
  that keep the connection, the exact 65,536-byte limit and control-lane exhaustion.
- `tests/rpc.rs`: exclusive and pinned registration, authority, FIFO dispatch with
  out-of-order completion, backpressure, all four cancel states, single replies, service
  disconnect and unregister, forged replies and call ids, an endpoint cache replaying an
  artifact result, and abandoned calls.
- `tests/sol_review_races.rs`: launcher-bound identity and Hello resource bounds, detached RPC
  orderings, responder/service teardown, reply/cancel races, replay admission, call-id
  watermarking, strict client router validation, unsent rollback and shared task shutdown.
- `tests/pubsub.rs`: bounded FIFO with atomic backpressure, latest coalescing, credits,
  retention and incarnations, zero-subscriber publication, unsubscribe, validation, quotas, a
  saturated subscriber that does not block RPC, and shutdown notices.
- `tests/artifacts.rs`: allocate, seal and read; unsealed use; immutability against live
  writable descriptors; length and digest checks; quotas; one physical object shared by
  fan-out and collected after the last consumer; extracted artifacts and holds; atomic
  admission; release watermarks; connection-scoped owners; abandoned futures; disconnects; and
  router restarts.
- `tests/integration.rs`: two agents called in parallel with a forwarded frame, an environment
  service, committed snapshot publication, a slow latest consumer and a bounded recorder.
- `tests/bus_acceptance.rs`: the implementation guide's BUS-01/02/03 acceptance bullets that the
  suites above do not already prove, one test per bullet - a lost result, a retransmission
  fixture, no failover onto a replacement registration, a status RPC answering while another
  handler is delayed, and a disconnect that reclaims ownership without touching an open file -
  plus `both_transports_produce_equivalent_behaviour_traces`, which replays one RPC scenario
  and one pub/sub-and-artifact scenario through the `Trace` recorder in `tests/common/mod.rs`
  and requires the two transports to record the same 29 behaviour events. `Trace::record`
  panics on a router-issued id, so a trace cannot drift into operational detail.
  `FLYBUS_TRACE=1` prints it.
- `tests/example_demo.rs`: runs `examples/demo.rs` and asserts every line it prints.
