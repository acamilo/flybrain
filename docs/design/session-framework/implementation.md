# Future-agent implementation guide

Status: **draft 2**, paired with the [contract index](README.md). This is the execution guide
for the new process architecture; it refines the broader
[MaleCNS/modular backlog](../malecns-modular-implementation.md), not a request to implement
every future feature in one branch.

## 1. Start here

Before coding:

1. Read repository instructions, the architecture tour, public feed/control contracts, and all
   documents in this directory. Reconcile any newer main/feature-branch changes with this baseline.
2. Record what the next slice will change, its compatibility surface and expected tests.
3. Use a dedicated branch/worktree. Follow the repository's coordinator/build/review roles.
4. Preserve the TypeScript oracle, default numerical version strings and legacy deployment.
5. Resolve contract contradictions before implementing; do not fill gaps with an implicit
   asynchronous best-effort policy or a public controller API.

The immediate target is **one small Rust Flybus example with RPC, pub/sub and a frame artifact
held beyond the message object's lifetime**. Then build the synthetic two-agent session over
that same router using in-memory and Unix-socket transports. No separate worker transport,
NATS service, coordinator-owned lease manager or raw-frame socket channel is to be implemented.

## 2. Proposed implementation map

Begin under the existing Rust workspace; extract physical package locations separately.
The following are proposed names, not files that exist today:

```text
services/flysim/crates/
  fly-session-types/     scopes, contracts, schema validation, canonical digests
  flybus/               generic router/client/wire/artifact store; embedded or standalone
  fly-session-rpc/       domain schemas/deduplication over flybus; NOT another transport
  fly-session/           phase machine, coordinator, admission, task/executor traits
  fly-session-worker/    dispatch shell, status/shutdown, agent/environment adapters
  fly-session-store/     participant captures, manifest commit, group recovery

services/flysim/crates/flysim/
  legacy/               compatibility composition (extract without changing behavior)
  composition/          new config/registries and worker launching

packages/feed/          existing public v1, later public v2 schemas/fixtures
packages/brain/         reference model/readout and new contract-relevant golden generators
```

Do not create empty crates to satisfy this tree. In the first slice, types/transport/coordinator
may be modules in one small crate; split when dependencies and consumers justify it. Keep
Melee parser/codecs and Game Boy FFI out of the session-types crate. Worker executables can
be subcommands of one binary initially; process boundaries do not require separate repos.

## 3. Ordered slices

### CONTRACT-01 — Executable schemas and trace format

**Inputs:** [bus spec](bus-v1.md), session RPC, step, worker, state/media and publishing documents.

**Implement:** Flybus wire schema separately from session domain schemas; common scalar types,
closed enums, method payload validation and schemas;
canonical digests; fixture loaders in Rust/TypeScript. Specify seed derivation and exact
checkpoint envelope bytes before their respective real-agent/store slices. Create the trace
format from step-v1 section 8, distinguishing behavior fields from operational IDs/time.

**Acceptance:**

- JSON round trips across both languages; U64/float boundaries reject correctly.
- Duplicate keys, invalid UTF-8, envelopes over 64 KiB and unknown required fields fail.
- Distinguish bus callId, domain requestId, artifact identity and delivery/hold owner tokens.
- Fixtures include rational zero/reduced form, overflow, duplicate ports and analog limits.
- Contract digest is generated from a documented canonical schema set, not source formatting.

**Stop:** do not wire a real worker until payload ambiguities and retry identity rules agree.

### BUS-01 — Router and RPC, in-memory and Unix socket parity

**Depends on:** CONTRACT-01.

**Implement:** one flybus crate with bounded framing, bus.hello, exclusive service registration,
incarnation-pinned RPC/reply/cancel, typed route/admission errors and independent read/write
dispatch. Begin with artifact-free calls; do not claim full bus conformance until BUS-03.

**Acceptance:**

- Partial frames/writes, disconnect after request, lost result and retransmission fixtures.
- No automatic retry/failover; timeout/cancel-after-dispatch reports uncertain execution.
- Service incarnation replacement is visible. Request/reply correlation survives out-of-order
  replies; status RPC can respond while another handler is delayed. Saturation is bounded.
- Both transports produce equivalent behavior traces for the same scenario.

### BUS-02 — Pub/sub, retention and backpressure

**Depends on:** BUS-01.

**Implement:** exact topics, subscribe/unsubscribe, bounded FIFO and latest policies, optional
retained latest/clear, per-recipient delivery IDs/consumption credits and fair control lanes.

**Acceptance:** overflow rejects a bounded publication before partial fan-out; latest replaces
only queued messages; delivery consumption returns credits; unsubscribe preserves already-
delivered ownership; retained replay is ordered; stalled observers cannot starve RPC replies.
Durable event history is a storage client, not a second broker built into Flybus.

### BUS-03 — Artifact-backed messages and automatic lifetimes

**Depends on:** BUS-02.

**Implement:** immutable file-backed store, allocate/seal/open, bus attachment validation,
producer/queue/delivery/retention owners, RAII DeliveryGuard and explicit cache holds. Root
creation/admission is atomic. Start with ordinary local files/tmpfs; no pooled slot reuse yet.

**Acceptance:** last owner collects; extracted handle survives message drop; forward-before-
release is safe; lost replies/cache replay remain valid; disconnect releases logical ownership
without mutating still-mapped bytes; retained latest and queue replacement release correct roots.
Measure 640×480 RGBA×60 with three readers: one stored image, no raw pixels in router messages,
bounded CPU/RSS/owners/queues. Record reader/copy costs rather than claiming zero-copy capture.

### SESSION-01 — Synthetic sequential transaction

**Depends on:** BUS-03.

**Implement:** small fake agent workers, one counter/arena environment, identity executors
and a deterministic task. Follow Prepare→Advance→Evaluate→Commit exactly. Use explicit seeds
and rational clock accumulation; the fake model must expose a mutation counter for tests.
Implement domain request deduplication/result caches over bus calls; retaining result artifacts
is an endpoint responsibility. Domain Acknowledge differs from bus delivery.consumed.

**Acceptance:**

- One world advance per complete batch; every agent Prepared before Advance.
- Task evaluates once; every agent commits before next Prepare or committed publication.
- A synthetic 60-Hz/1-ms profile produces 16,17,17 ticks and remainder zero after three steps.
- Pause mid-step completes the step and pauses at its committed boundary.
- Bootstrap/warm-up cannot advance the environment or produce gameplay rewards.
- Domain retries use a new callId with the original requestId; they never repeat ticks/reward.

### SESSION-02 — Parallel processes and fault behavior

**Depends on:** SESSION-01.

**Implement:** one agent process per fly and one environment process under the coordinator;
compare with in-process and dedicated-thread variants. Enforce total thread budgets and
configured agent/port identities. Failure stops the epoch rather than neutralizing a player.

**Acceptance:** sequential, reversed order and parallel completion produce equivalent traces;
delayed one-agent result holds the world; worker/helper death has a bounded diagnosed outcome;
an uncertain Advance never creates a second batch; partial Commit never permits next-step play.

### MEDIA-01 — Native observation schemas and presentation handoff

**Depends on:** SESSION-02.

**Implement:** view/sample descriptors and producing-step validation on top of bus ArtifactRef,
not a second buffer system. Environment outputs native media; sensor transforms remain agent
profiles, while presentation owns viewer resizing/composition/audio/streaming.

**Acceptance:** bad strides/lengths/producer times fail; shared image reaches both agents through
owned attachments; spectators use latest subscriptions and cannot corrupt sensory state;
delayed rendering retains its handle. Distinguish AssetRef from transient ArtifactRef.

### AGENT-01 — Existing neural core worker

**Depends on:** SESSION-02, MEDIA-01 and the profile/identity foundation in the broader backlog.

**Implement:** adapter over existing LIF, plasticity, retina and fixed readout primitives;
reference-first composition/goldens; independently seeded agent state and shared immutable data.
Avoid using the old whole-frame `tick` wrapper if it changes the specified phase ordering.

**Acceptance:** per-agent state agrees with the reference across Prepare/Commit, stimulation,
zero/nonzero reward, warm-up and pauses. Two agents cannot share gains/RNG/holds; swapping
dispatch order and varying worker count preserves results. Keep 64-role limits explicit.

### ENV-01 — Game Boy compatibility environment

**Depends on:** AGENT-01 and environment/task extraction in the broader backlog.

**Implement:** binjgb environment, task-local memory inspector and identity/existing action
adapter. Keep `legacy-gameboy-v1` separately routed with exact old ordering/hash semantics.

**Acceptance:** legacy fixtures/goldens/restore outcomes unchanged; the new composition has
its own identity and public adapter selection. No console-specific state enters generic
session types. ROM-backed checks are optional explicit jobs, not required downloads.

### STATE-01 — Coherent all-participant checkpoint/recovery

**Depends on:** SESSION-02, MEDIA-01; validate with fake agents first, then AGENT-01/ENV-01.

**Implement:** exact new envelope schema, compatibility manifest, Capture/StageRestore/
ActivateRestore, bounded writer, durable commit acknowledgments and fresh-epoch fencing.
Keep old `FLYSIM01` reader separate. Payloads and coordinator state must refer to one boundary.

**Acceptance:** uninterrupted versus resumed synthetic/real-agent traces match after accounting
for new epoch metadata; corrupt any participant and installation fails as a group; lost save
reply doesn't advance durable metadata; failure during activation cannot resume half a world.
Checkpoint queue stress remains bounded; verify old media/parser data cannot cross recovery.

### PUBLISH-01 — Committed snapshots and observer isolation

**Depends on:** SESSION-02, MEDIA-01; public v2 contract work is a separate prerequisite to
publishing a supported multi-agent browser feed.

**Implement:** internal [publication boundary](publishing-v1.md) over the SAME bus, latest-value
observations, application-owned state/cues, bounded events, descriptor repair/query and a fake
multi-agent consumer. Presentation gateway owns browser delivery; no generic show/tournament
service or codec is added to the router. Then implement
the approved public v2 wire schemas/fixtures and stage adapters together.

**Acceptance:** browser disconnect/backpressure never advances/stalls the world; future agent
state is not mixed with old media; descriptor/index mismatch is visible; committed actions
are labeled as the transition that just ended. PNG/browser review gates apply to actual UI.

### DOLPHIN-01 — Substitute the backend, not the coordinator

**Depends on:** measured MELEE-01/02 spikes from the [Melee audit](../melee-framework-audit.md),
SESSION-02, MEDIA-01 and declared recovery support.

**Implement:** bus-connected helper over pinned Dolphin/libmelee or the chosen narrow hook,
complete port batch adapter, frame-identified sensory output and task-local Melee inspection.
Keep actual rendering/input/save semantics behind the same Environment API.

**Acceptance:** all generic backend conformance tests plus delayed-port/flush ordering,
game-frame reset, parser recovery, pixel latency and neutral/release tests. If exact snapshot
is unsupported, advertise episode-restart and use only the matching prototype policy.

## 4. Failure injection checklist

Tests must deliberately inject these cases; success-path demos are insufficient:

| Injection | Required invariant |
| --- | --- |
| Duplicate Prepare after lost reply | No extra ticks, RNG draws, stimulation or decode |
| Same batch with altered controls | Conflict, never a second world mutation |
| Lost Advance result after world step | Resolve same operation or fail epoch |
| One Commit fails after another succeeds | No next world step; coherent recovery only |
| Old worker replies after restore | Stale epoch/incarnation rejected |
| Message drops while extracted image is rendering | DeliveryGuard keeps bytes alive through last use |
| First agent releases a shared image early | Artifact remains until every other owner finishes |
| Cached RPC artifact is consumed by its first caller | Domain cache still owns it for retry |
| Latest queued frame is replaced | Only that queue root drops; in-use images remain valid |
| Router restarts during a world advance | Old handles/routes invalid; epoch fails and restores coherently |
| Viewer holds output indefinitely | Only spectator data is dropped/disconnected |
| Backend waits for input while capture is requested | No deadlock; capture only at valid quiescent boundary |
| Capture writer stalls | Finite queue/memory, honest durable status |
| StageRestore validates three participants, fourth fails | Nothing is resumed |
| ActivateRestore fails halfway | Group remains fenced, no new gameplay |
| Old epoch audio arrives after reset | Discontinuity handling; no stale playback as current |

## 5. Verification and handoff

Before each implementation merge, run the repository's required `npm test`,
`npm run typecheck`, `cargo test --workspace` from the Rust workspace, and
`infra/tests/lint.sh`. Run affected browser/PNG gates for presentation changes. Keep the
existing committed FAFB real-data goldens mandatory; larger new datasets and game-backed
jobs report explicit optional skips.

Performance checks report total physical-core allocation, one/two/four agents, within-agent
worker count, router/RPC latency, artifact production/read/copy time, critical-path percentiles,
memory peaks, owner/GC statistics and
bounded queue behavior. No host capacity claim follows from a local synthetic timing test.
Deployment-host work is separately authorized/claimed/serialized under repository rules.

Every completed slice leaves:

1. The implemented contract/schema revision and compatibility decisions.
2. A minimal runnable synthetic example and exact test commands/results.
3. Behavior traces demonstrating its acceptance criteria.
4. Known unsupported capabilities and remaining measured questions.
5. Updated planning status, with no claims that a stub provides real emulator semantics.

Do not start by moving every directory, adding a service mesh, or rewriting the model. The
first useful deliverable is the small artifact-backed bus example, followed by the synthetic
distributed step transaction using it. No one-off communication stack per component.
