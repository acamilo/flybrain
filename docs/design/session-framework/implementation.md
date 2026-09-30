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

**2026-09-22:** blocked. The profile/identity foundation is FOUNDATION-02
(`feat/brain-profile-contract`) in the [MaleCNS backlog](../malecns-modular-implementation.md),
which has not been built. Not started.

**2026-09-23:** unblocked. The operator split FOUNDATION-02 (decision of 2026-09-23): its legacy
half, PROF-02a, is the [legacy Game Boy composition](legacy-gameboy-v1.md) contract with
machine-readable profile, readout-context and decision schemas in `fly-session-types` and
`@flybrain/session-types`; the MaleCNS half (PROF-02b) is later and does not gate this slice.
AGENT-01 builds the legacy profile `gameboy-legacy-fafb-v783-v1` first. It must: report
`brainTicks` equal to the legacy `network.ms` (legacy-gameboy-v1 section 3); consume
`gameboy-readout-context-v1` and return `gameboy-channels-v1`; keep the held channel, blocked
window and last location as private readout state; report `stimulusRemainingMs`; answer
`Agent.Rollback` under capability `legacy-ratchet-rollback-v1`; and choose the mapping from the
legacy rate-role names (`command_0`, `macro_*`, which are not `Id`s) to `AgentGraph.rateRoles`,
which that contract leaves open.

**2026-09-23 (AGENT-01 built, `feat/agent-legacy-worker`).** `LegacyAgentWorker`
(`fly-session/src/legacy_agent.rs`) serves the legacy profile under the launcher in all three
execution modes (`legacy-agent` subcommand). Its parity harness (`legacy_parity.rs`) drives
`NeuralAgent` directly in `Sim::step_frame` order from recorded inputs. On the committed toy
connectome, the worker's records match that reference exactly in every mode, including a
rollback and a restore, and are pinned as goldens. A gated FAFB run
(`FLY_AGENT01_FAFB=1`) shows the same on `gameboy-legacy-fafb-v783-v1`. The rate-role mapping
turned out to be the identity: the legacy names are already `Id`s. It is recorded in
[legacy-gameboy-v1](legacy-gameboy-v1.md) section 13, with the other agent-adapter choices. The
harness's `ReferenceSource` is the seam for FND-01's `FLY_TRACE`.

**2026-09-29 (AGENT-01 rebased onto v0.6.5 and checked against FND-01's trace).** The rebase onto
main had no conflicts. Main changed nothing the worker reads except the adapter id in the
fixtures (v7). The toy goldens and the FAFB reference digest are unchanged. `FLY_TRACE` carries
the frames only as digests and not the `{boot, bound, location}` context, so it checks a worker
and cannot drive one. `legacy_parity::read_frame_trace` and `check_against_trace` hold a run's
records to it: ticks, clock, exact remainder, rates digest, spike-set digest and decision. The
script is checked too: the same sugar, frames, reward events and rollback.
`flysim/tests/agent_trace_parity.rs` is the host. It runs `LegacyFrame` on the ROM with the trace
on, records the missing inputs and checks the worker (in-process, thread and process) and the
direct reference against the trace:

- the toy connectome in raw mode from power-on, for every ROM run;
- `gameboy-legacy-fafb-v783-v1` in macros mode from stream checkpoints (`FLY_AGENT01_FAFB=1`),
  with sugar and a ratchet rollback, the worker seeded from the checkpoint's agent state.

Both are identical. Found on the way: a `FLYSIM01` import must start `learning.updates` at the
legacy `plasticity.updates` (legacy-gameboy-v1 section 13, amendment of 2026-09-29).

**Implement:** adapter over existing LIF, plasticity, retina and fixed readout primitives;
reference-first composition/goldens; independently seeded agent state and shared immutable data.
Avoid using the old whole-frame `tick` wrapper if it changes the specified phase ordering.

**Acceptance:** per-agent state agrees with the reference across Prepare/Commit, stimulation,
zero/nonzero reward, warm-up and pauses. Two agents cannot share gains/RNG/holds; swapping
dispatch order and varying worker count preserves results. Keep 64-role limits explicit.

### ENV-01 — Game Boy compatibility environment

**Depends on:** AGENT-01 and environment/task extraction in the broader backlog.

**2026-09-22:** blocked. AGENT-01 is blocked, and environment/task extraction is
RUNTIME-01 (`refactor/environment-task-boundary`) in the same backlog, which has not been
built. Not started.

**2026-09-23:** unblocked. RUNTIME-01's contract is the RT-01a amendments of 2026-09-23 to
[workers-v1](workers-v1.md) (sections 1, 3, 4, 5 and the new section 7),
[step-v1](step-v1.md) (sections 2, 3, 5 and 6) and [state-media-v1](state-media-v1.md)
(sections 2, 3, 4, 5 and 7), with the Game Boy specifics in
[legacy-gameboy-v1](legacy-gameboy-v1.md), all implementing the operator's decisions of
2026-09-23. ENV-01 and AGENT-01 may proceed in parallel against the schemas and fixtures; ENV-01
needs FND-01's trace harness for its "legacy fixtures unchanged" acceptance. ENV-01 must also:
add the one read-only bulk memory read to the shim and prove it mutates nothing; publish the
memory image per boundary; declare the one-frame setup scaffold; convert audio to f32 and leave
the DC blocker to the edge; implement `gameboy-slots-v1`; and make the coordinator refuse
`episodeRequest.kind = "rollback"` in any composition that declares no rollback policy (the
synthetic coordinator today pauses on every episode request, which is safe but not the rule).

**Amendment, 2026-09-23 (operator decision of 2026-09-23).** The "Implement" paragraph below said
to keep `legacy-gameboy-v1` separately routed. The operator decided on a full port instead: the
legacy composition runs on `lockstep-v1` as declared in legacy-gameboy-v1, and "exact old
ordering/hash semantics" is kept by construction and by test rather than by a separate route --
the frame order maps one to one (section 4), the legacy clock equals the rational one
(section 3), and the fingerprint and compatibility string are embedded unchanged (sections 2
and 12). The acceptance criteria below stand.

**2026-09-29: built on `port/env-01` (off main `9f2345d`).** `fly-session::legacy_env` is the
environment worker (`LegacyGameboyEnvironment`), launched by `Launcher::launch_legacy_environment`
and the `legacy-environment` subcommand in all three execution modes. It makes exactly the emulator
calls flysim's `LegacyFrame` makes, in the same order: `Environment.Initialize` runs the one-frame
setup scaffold with no button down (O[0] has `engineFrame` "1", no audio chunk);
`Environment.Advance` takes one complete `gameboy-joypad-v1` batch for the one port, runs one frame
and returns the `lcd` view, the `apu` chunk as `f32le` (`sample / 255`, unfiltered) and the
`gameboy-memory-inspection-v1` image; `Environment.SaveSlot` / `RestoreSlot` are
`gameboy-slots-v1` (a restore imports, releases the pad, shows the slot's frame, runs no frame and
moves to the new epoch at the same boundary); `State.Capture` / `StageRestore` / `ActivateRestore`
carry every slot and stage on a stopped replacement emulator. The joypad is the only write into a
running game. The coordinator now refuses `episodeRequest.kind = "rollback"` (task failure, epoch
fenced) unless the composition declares `legacy-ratchet-rollback-v1`
(`Coordinator::declare_rollback_policy`); with the policy declared it still pauses at the boundary,
because the rollback sequence is TASK-01's. Parity (`tests/legacy_env.rs`, harness
`legacy_env_parity`): records equal to the emulator driven directly, frame by frame (view, 64 KiB
image, WRAM, audio chunk, slot and capture state digests), in-process, thread and process, on a
toy cartridge the harness assembles (committed golden `fixtures/legacy-env/toy-cart.golden.json`)
and, with rom-env, on the real cartridge from the row-58 checkpoint; and the service's own
`FLY_TRACE`s (FND-01's harness; `FLY_ENV01_TRACE_DIR`) replayed through the worker reproduce every
recorded frame, WRAM and slot-state digest, including a ratchet slot save and a rollback. Not in
this slice: the shim's bulk read (MEM-01, behind the one seam `legacy_env::memory_image`), the
coordinator's save/rollback/`Agent.Rollback` sequence and the executor (TASK-01), and assembling
the FLYSIM01 export from the three halves (STATE-02; the world's half is
`Flysim01World::{manifest_entries, chunks}`).

**Amendment, 2026-09-29 (ENV-01).** Four readings the contract left open, fixed by this slice:

- ~~*The image read is not state-neutral, so it is guarded.* `emulator_read_mem` of OAM, VRAM,
  `FF01`/`FF02`, `FF04`-`FF06`, `IF`, `STAT` or `LY` runs binjgb's lazy synchronisation, and
  `export_state` afterwards differs (never, over 17,398 replayed frames, a frame or WRAM). The
  legacy loop reads none of these between a frame and the ratchet's capture, so an unguarded image
  would make every slot and capture differ from the service's bytes. The worker reads the image
  between `export_state` and `import_state` of the same bytes, which leaves the emulator exactly as
  the frame left it; `legacy-gameboy-v1` section 8's proof ("export before and after, compare")
  therefore fails for a plain 65,536-read loop and holds for the guarded one. MEM-01's bulk read
  must keep that guard (or equivalent).~~
  *Struck 2026-09-29 (ENV-01 review, R1):* superseded by MEM-01 (legacy-gameboy-v1 section 8
  amendment). The register windows are not captured, so `legacy_env::memory_image` is MEM-01's
  `Emulator::read_memory_image` with no guard, neutral by construction; the service traces still
  match, and the toy golden's image digests were regenerated.
- *A `FLYSIM01` world starts at `k = emulatorFrame - setupFrames`.* The legacy file has no
  boundary, world clock or audio position. The frame counter advances once per transition and
  never across a rollback, so `engineFrame = k + 1` from a fresh start on; `worldTime` is
  `k x stepDuration` and the audio position is that time's sample at the configured rate, rounded
  down. The first chunk after the restore marks the discontinuity. The ratchet's one slot is the
  composition's first declared slot (`best`).
- *Names.* The image travels as the attachment `inspection.memory`; the audio stream is `apu`;
  the port is `p1`. `backendConfig` is the canonical JSON of `gameboy-backend-config-v1`
  `{form, romDigest, portId, slots, audio {sampleRate, channels, bufferFrames}, setupFrames, view,
  controllerSchema, inspectionSchema}`; `Environment.Initialize` naming any other document, and a
  cartridge on disk that is not `romDigest`, are refused `INCOMPATIBLE_STATE` with nothing mutated.
  The capture payload is flybrain-core's envelope with magic `FLYENV01`, and its participant
  compatibility uses the state format `fly-gb-env-v1`.
- *`legacy-transient-reset` for the world is empty.* Everything the environment holds is restored:
  emulator, frame on screen, joypad, frame counter, slots, clock and audio position. The reset
  applies to the executor, the agent's readout transient and the task's transient observations.

**Implement:** binjgb environment, task-local memory inspector and identity/existing action
adapter. Keep `legacy-gameboy-v1` separately routed with exact old ordering/hash semantics.

**Acceptance:** legacy fixtures/goldens/restore outcomes unchanged; the new composition has
its own identity and public adapter selection. No console-specific state enters generic
session types. ROM-backed checks are optional explicit jobs, not required downloads.

### MEM-01 — The boundary memory image (port slice)

**2026-09-29: built** on `port/mem-01` off v0.6.5 and awaiting review. It changes no live
behaviour: nothing in `flysim` calls the new read, and the compatibility string is unchanged. It
delivers the piece of ENV-01 above that reads "add the one read-only bulk memory read to the shim
and prove it mutates nothing", together with the executor-side reader that TASK-01 and the
CUT-01 shadow need.

- **Shim.** `fly_gb_read_memory_image(gb, out, 65536)` fills the buffer in address order.
  `Emulator::read_memory_image{,_into}` wraps it. There is no new write path.
  [legacy-gameboy-v1](legacy-gameboy-v1.md) section 8 carries a dated amendment (2026-09-29):
  VRAM, OAM and I/O are not captured and read `$FF`. binjgb's read of those windows runs a
  catch-up that moves the exported state. Every memory byte is captured exactly.
- **Executor reader.** `flybrain_gb::{MemoryImage, Cartridge, ImageReader}`. `ImageReader`
  implements `MemoryReader`. The palette, `MacroLayer`, `PokeState` and `PokemonRedReward` run
  over it unchanged; this is the same code, not a port. The emulator's own bank reads now go
  through the same `Cartridge::read_bank`.
- **Proof.** flybrain-gb `tests/rom_memory_image.rs` (ROM) checks three things. The exported
  state is byte-identical before and after each bulk read over 4,200 boot frames. The image
  equals the single and cached reads. Reading every frame leaves framebuffers, audio and state
  frame-for-frame equal to a run that is never read. flysim `tests/rom_memory_image.rs` runs a
  live arm and an image-only shadow arm (task and executor) from nine rom-env checkpoints for
  3,000 frames each: 27,000 frames with about 5.5 M `MacroState` answers, deals, pads,
  decisions, rewards, ledgers and rollbacks. It finds 0 mismatches and 0 reads outside the
  captured ranges.
- **Cost.** A bulk read has a median of 0.31 ms per boundary, 1.9 % of a 16.74 ms frame or
  about 209 MB/s of reading. This is the same cost as the 57,120 single reads it replaces. The
  image is 3.91 MB/s at 59.7275 fps. A 64 KiB copy has a median of 5 µs, and the optional
  SHA-256 digest a median of 0.36 ms. Means and p99 values are dominated by preemption: the
  measurement ran on the shared 4-core build box at load 16.

**For ENV-01:** after `run_frame` and after a slot import, publish
`read_memory_image` as the `gameboy-memory-inspection-v1` artifact (`MEMORY_IMAGE_CONTENT_TYPE`,
65,536 bytes, digest optional, `romDigest` = `Cartridge::sha256_hex`). **For TASK-01:** build
the executor over `ImageReader::new(&O[k], &cartridge)` in Phase B and over O[k+1] in Phase C,
retaining O[k] until then. The `Cartridge` comes from `Cartridge::verified(rom, contentDigest)`.

### TASK-01 — The `pokered-macros-v1` task and executor (port slice)

**2026-09-29: built** on `port/task-01` off `port/integration` (main v0.6.5 + AGENT-01 + MEM-01 +
ENV-01) and awaiting review. It changes no live behaviour: `flysim` is only a library dependency of
the new crate, nothing in the service calls it, and the compatibility string is unchanged. With it
the live fly's whole composition -- agent, world, task and executor -- runs on the session
framework, from a fresh start or from a FLYSIM01 checkpoint.

- **The object** (`fly-legacy-session::task::PokeredTask`). The task and the executor are one
  object with two faces, `Task` and `ActionExecutor`, which the coordinator holds where it holds
  any task and executor (legacy-gameboy-v1 section 10). Phase B runs flysim's `MacroLayer::decide`
  over `ImageReader(O[k], cartridge)` at the agent's brain time. Phase C runs
  `PokemonRedReward::sample`, `MacroLayer::observe`, the location, the progress and the ratchet's
  decision over `ImageReader(O[k+1])`. A rollback runs `clear_transient`, `cancel` and `observe`
  over `ImageReader(O'[k+1])`. These are the legacy engine's own calls in `LegacyFrame`'s order;
  there is no port of a rule. The cartridge is `Cartridge::verified(rom, contentDigest)`, and O[k] is
  retained until Phase C has read it.
- **The coordinator** (fly-session). A task declares the inspection attachments it reads, and the
  coordinator holds them and hands over their bytes (`task::Inspection`). The executor's clock is the
  agent's brain time after Prepare. `Evaluation.slot_saves` asks for `Environment.SaveSlot`. A
  declared rollback policy now runs itself: `Ready(e, k) -> RollingBack(e', k) -> Ready(e', k)`, with
  `RestoreSlot`, then `Task::rollback`, then `Agent.Rollback` on every agent. Composition setters
  cover the backend and task config, the media, the decision schema, the world's state format and
  per-agent model versions. The admission queue (`AdmissionQueue`) is cut into each Prepare's
  `preStepStimulations` and reports every admission applied or aborted. `Coordinator::import`
  installs a checkpoint of another format as a session's start. Step details (`StepDetails`) are
  kept for a parity run.
- **FLYSIM01 as the start** (`fly-legacy-session::import`, on STATE-02's
  `legacy_checkpoint::{halves, agent_payload, world_payload}`). The world, agent and task halves
  go into each participant's own capture format, and the ordinary group restore installs them:
  stage, validate, activate, `Paused(k)`, resume. The world is replaced by a fresh one first,
  because ENV-01 stages only on a replacement. The context the agent resumes with is computed
  from the checkpoint's own memory image, and the coordinator holds the task to it after the
  install (`Task::restored`). The task ledger is STATE-02's `{reward, ratchet, slotFilled}`.
- **The store and the boot** (`fly-legacy-session::composition`, on STATE-02's
  `LegacyCheckpointer`).
  - `boot` is `Sim::boot`. It restores from the live hot and durable stores in the legacy
    candidate order, holding each candidate to the legacy gate. Any refusal falls to the next
    candidate; each attempt runs on a session of its own, because a refused install fences its
    session. It is a fresh start when the stores are empty. After either, the startup durable
    save is written and the intervals start.
  - `advance` keeps the declared boundary order: the slot save (inside the step), the milestone
    archive on a rank climb, the rollback (deferred by the coordinator until the host has
    captured), a durable save after a rollback, then the interval saves.
  - `shutdown_save` is the legacy shutdown's durable save.
  - The session runtime writes no sugar journal yet: its admission is the coordinator's, and the
    journal's boot header needs the edge.
  - `lastEventId` carries the restored file's watermark; the runtime has no feed event log
    before EDGE-01.
- **Sugar** (`fly-legacy-session::admission`). flysim's `RateLimiter` and clamp run unchanged,
  against the last commit's `stimulusRemainingMs` (one commit stale). An admission waits for the
  next cut.
- **The operator reward pulse is refused.** See the legacy-gameboy-v1 section 15 amendment.

**Parity** (`fly-legacy-session/tests/session_trace.rs`, rom-env). A whole new-runtime session
writes its own `FLY_TRACE` line per transition (`fly-legacy-session::trace`). Each line is built
from what crossed its boundaries: `Agent.Prepare`, `Agent.Commit`'s rates and spike bitset, the
executor's batch, the view, the image and the boundary actions. It is compared field by field with
the legacy loop's. The decision is compared as a set, because `gameboy-channels-v1` carries no list
order. Where the legacy side runs in the test, the ledgers are compared after every boundary as
well: adapter export, ratchet state, and the executor's scene, bound channels, running macro,
counts and "nearer". Results: see the TASK-01 run report and the amendment below.

**Amendment, 2026-09-29 (TASK-01 review fix round).** The review blocked on B1 and noted
N1 to N5. All are fixed here; the dated amendments to legacy-gameboy-v1 section 10 and 15 cover
B1, N1 and N3.

- **B1: the task ledger is artifact-backed.** On the live fly the adapter ledger is 42-46 KB,
  past the 32 KiB `TypedValue` bound.
  - `Task` gains `capture_attachments`, `validate_restore_with` and `install_restore_with`.
  - The coordinator files those attachments as `task-ledger-<name>` payloads in captures and
    imports.
  - A bad ledger is an error, never a panic, so a boot falls to the next candidate.
  - Boot parity now covers row 64 (42 KB) and the row 65 yard (46 KB).
- **N1: a sugar admitted before the next commit is an active pulse.**
- **N3: a restore seeds the palette as `Sim::boot` does.** `tests/palette_seed.rs` shows that the
  seed is never read.
- **N4: saves are queued to the writer thread.**
- **N5: speed.** The profile is below. Changes:
  - The world seals its three artifacts concurrently.
  - Cache and caller holds are taken concurrently.
  - In-process participants use the in-memory transport.
  - A service session publishes every other boundary (the feed's 30 Hz) and keeps a bounded
    history.
  - `State.Capture` is off the loop. The worker shell lets a handler finish its reply outside
    the endpoint's lock, and takes mutations in arrival order. The legacy agent copies its state
    under the lock, then encodes, digests and seals it off the lock. The coordinator's
    `begin_capture` / `finish_capture` let the session step on while the payload is written.
- **Measured** (`tests/speed.rs`, `tests/speed_ab.rs`):
  - The brain is the same: the legacy `ticks` phase and the agent's `ticks` span agree within a
    few percent.
  - What remains is a fixed per-frame overhead of the process boundary. It is about 3-5 ms on a
    quiet 8-core build box: roughly 0.7-1 ms per RPC round trip for Prepare, Advance and Commit,
    plus the world's and the agent's artifact seals and reads.
  - Against it, the legacy loop spends about 0.7 ms per frame outside the ticks.
  - The report `claude-task-01` has the per-phase table and what would close the rest.

### PERF-01 — The session runtime's per-frame cost (port slice)

**2026-09-30: built** on `port/perf-01` (off `port/task-01`, with `main` v0.6.7 merged) and
awaiting review. Goal: cutover must not slow the live stream. TASK-01's fix round left the
in-process session about 4.7 ms a frame above its brain, against 0.7 ms for everything the legacy
loop does outside its ticks. No behaviour changes: the parity runs are identical frame by frame,
in-process and process, and the compatibility string is unchanged.

- **The local lane** (the big one; ipc-v1 section 1 and bus-v1 section 12, amended 2026-09-30).
  An in-process worker's shell is offered to the coordinator directly (`worker::LocalLane`,
  `launcher::Via::Local`, `WorkerRef::local`); `rpc::send` takes it for every endpoint method.
  The request still goes through the shell's admission, deduplication, arrival order and
  endpoint, and the reply is the same outcome. Handler media over the lane are in-memory
  artifacts (`flybus::Artifact::in_memory`, `HandlerCtx::seal`): no store file, no seal copy,
  no retain or open round trip. A bus call or a publication carries a sealed copy
  (`rpc::promote`), begun while the task and the commits run. Process mode is untouched.
  `FLY_SESSION_LOCAL_LANE=0` runs in-process over the bus. Every synthetic transport test also
  runs over the lane.
- **The spike bitset** is the union of the kernel's per-tick spike lists, gathered while Prepare
  ticks (`LifNetwork::tick_spikes`), instead of a 139,255-neuron scan in Commit. Same bits,
  proved by `flybrain-core/tests/tick_spikes.rs` and compared every frame by the parity runs.
- **The session topic** gets a committed snapshot every 60th boundary in a service session (and
  at every boundary with events or boundary actions), not every other one. Nothing in the
  release subscribes; the stream is the legacy feed.
- Small ones: a single dispatch job is called in place, the shell moves the reply result
  instead of cloning it, `digest_views` and `media_bytes` as SERVE-01 has them, and profiling
  spans for the shell, the lane, each handler and the brain's own phase clock.
- **Tried and dropped**: publishing without waiting for the router (a deferred publish) and any
  other overlap with Prepare. On the release CPU the sweep owns every core of the cpuset, and
  work beside the ticks slowed them by up to 30% in one run. Spikes only at the feed rate
  (an AGENT-01 amendment) was not needed once the bitset cost nothing.
- **Measured** (`tests/speed_ab.rs` with `FLY_PERF_ARMS`, `FLY_PERF_SERVICE`, `FLY_PERF_WORKERS`;
  the report `claude-perf-01` has the tables): on the release host's CPU model, pinned to the
  release cpuset's size, the in-process session's cost outside the brain fell from about 9.3 ms
  to about 3.5 ms a frame. The legacy loop's is about 1.3 ms. What is left is mostly the per-frame
  memory image (0.37 ms, 65,536 shim reads), the three domain calls' JSON, digests and cache
  (about 0.5 ms together) and the task's evaluation, which the legacy loop does too.

### SHADOW-01 — The session runtime beside the live fly (port slice)

**2026-09-29: built** on `port/shadow-01` off `port/task-01`, and awaiting review. It changes no
live behaviour. flysim gains trace switches that are off unless set, and the compatibility string
is unchanged. The contract is [legacy-gameboy-v1](legacy-gameboy-v1.md) section 18: the inputs,
the architecture, what is compared, the declared differences, the window and the verdict CUT-01
reads.

- **flysim** (`trace.rs`, `frame.rs`). These additions are recording only:
  - `FLY_TRACE_DIR` writes one trace file per process. A restart truncated the one `FLY_TRACE`
    path.
  - `FLY_TRACE_MAX_BYTES` caps a file; the default in directory mode is 4 GiB.
  - `FLY_TRACE_LEDGERS=<n>` adds `ledgersDigest` every *n* transitions.
  - `frame::ledgers_string` is now the one definition of the ledger string. The session task's
    `ledgers_of` calls it.
- **`fly-legacy-session::shadow`** and the binary `fly-shadow`:
  - `follow` reads the per-process files. It hands out complete lines only, and a file ends when
    a newer one exists.
  - `spool` copies every new store generation as it appears. It keeps a floor and a byte bound,
    and never writes to a live store.
  - `checkpoint` exports the shadow's capture of a live save's boundary and compares it with the
    live file byte for byte, after `host-fields` and `archive-order`.
  - `verdict` is `fly-shadow-verdict-v1` and the cutover rule (`allows_cutover`,
    `fly-shadow check`).
  - The runner boots each segment from the live process's startup save (TASK-01's import). It
    replays the admissions, runs the transition, and takes the live captures in the live order
    around a deferred rollback. It compares with `trace::compare_line`, which is now shared with
    `trace::compare`. It stops on the first divergence and writes `divergence.json` with 30
    transitions of context.
- **infra.** `units/flyshadow.service` is report-only and has no `[Install]`. It is not bound to
  flysim, runs `SCHED_IDLE` and stays stopped on exit status 3. `bin/fly-shadow-run` has the
  commands `start`, `stop`, `status` and `check`. `05-deploy.sh` converges both and pins the
  unit to the page and encoder CPUs. `build-flysim.sh` and `package-release.sh` ship
  `fly-shadow` and `fly-session`. `lint.sh` holds the unit to all of this. The runbook has a
  "Shadow run (SHADOW-01)" section.
- **Rehearsal** (`tools/shadow-rehearsal.sh`). It runs the real `flysim` service and the real
  `fly-shadow` side by side from one checkpoint, with sugar posted to the control API and service
  restarts. It runs on a build box, never on a host.

**Offline rehearsal** (a build box, `tools/shadow-rehearsal.sh`, FAFB in macros mode). The real
service ran at real time or unthrottled, with sugar posted every 10 to 20 s and a restart in every
run. It totalled **4,090 brain seconds (68 brain minutes), 244,327 transitions, 13 processes and
zero divergence**:

| Run | Start | Brain s | Transitions | What it covered |
| --- | --- | --- | --- | --- |
| main | the row 67 Pewter Gym stream checkpoint (rung 10), real time, restarts at 12 and 24 min | 2,161 | 129,086 | 126 sugar, 7 rewards, 4,134 macro events, 484 saves byte-identical, a ledger digest every boundary |
| reward | row 58 door, adapter ledger thinned (the TASK-01 brain arm, `examples/reward_bearing.rs`) | 601 | 35,894 | the real brain earns `map`, `exploration`, `boundary`, `talk` and `battle` (11 rewards); 138 saves |
| rollback | FND-01's `rollback` checkpoint | 302 | 18,056 | a ratchet rollback and the durable save after it; a `talk` reward |
| climb | FND-01's `climb` checkpoint | 303 | 18,069 | a ratchet slot save, and the milestone archive before it compared under `archive-order` |
| smoke | row 67, unthrottled | 122 | 7,305 | the first run |
| final | row 67, real time, the final binary, the lag guard on | 601 | 35,917 | the box was contended by other runs, so the live loop fell behind real time 5 times; each time the guard paused the shadow (667 s in all), and all 143 saves were still compared |

The reward run's box was loaded: the shadow ran at 25 frames a second against the live 52 and
finished 12 minutes behind. Every save was still compared, from the spool.

`tests/shadow.rs` (ROM, the toy connectome in raw mode) runs the real service through a restart
under a shadow, then plants four differences on copies of that run's trace and spool. Each one
stops a fresh shadow with the right kind, field and step:

- a work-RAM digest;
- a ledger digest;
- a sugar removed, which diverges on the brain at that transition;
- a live save's `rankSinceMs`.

**Cost.** These figures come from a shared 8-core build box, so they are indicative only. Other
agents' runs made paired measurements vary by up to 2x.

- *The shadow.* Over the whole main run it used 1.47 cores to follow 52.3 frames a second. The
  live service used 1.43 cores for 53.5 frames a second. That is about 28 ms and 27 ms of CPU a
  frame, so the session runtime costs about what the legacy loop does (x1.05).
- *Wall time a frame.* With 2 sweep threads the mean was 14.7 ms on the quieter box. Replaying the
  main trace unthrottled gave these means:
  - in-process with 1, 2 and 3 threads: 23.7, 19.7 and 17.0 ms;
  - thread mode with 2 threads: 18.5 ms;
  - process mode with 2 threads: 18.5 ms.

  Every replay was identical again: five more comparisons of 20,000 transitions each.
- *Memory.* The shadow's resident size was about 130 MB, against the live service's 54 MB.
- *The live trace* with a ledger digest every frame costs about 1 ms of CPU a frame in the best
  paired sample, which is within this box's noise; FND-01 measured about 2 ms. The live loop has
  1.7-1.8x real-time capacity on 3 threads. The trace is about 750 bytes a transition, which is
  160 MB per live hour.

The resource plan for the release container is in the SHADOW-01 run report and in
`units/flyshadow.service`:

- `SCHED_IDLE` on the page and encoder CPUs, never flysim's;
- 2 sweep threads;
- a back-off whenever the live `fly_lag_seconds` grows;
- a 3 GB memory ceiling;
- a ledger digest every 60 transitions.

To keep up, the shadow needs about 1.7 idle cores. With less it runs behind real time: the trace
is on disk, the spool covers about 15 minutes of hot saves, and the verdict comes later.

**Found on the way.** binjgb's exported state does not carry the audio resampler's phase. An
emulator imported from a state therefore drifts from a powered-on one in its channel accumulators.
The drift shows in the exported bytes only (frame and WRAM are identical). Every restart restores
through an import in both runtimes, so the two agree. A fresh start does not import, and the
shadow follows one by powering on as well.

**Gates** (a build box with rom-env): `cargo test --workspace --release` with `--no-fail-fast`
had three failures, each accounted for:

- `flysim` `integration` is the known load-sensitive test. It failed at box load 25 and passes
  alone.
- `rom_macros_mode`'s no-PP turn is red on this base. Row 67 fixes it on main.
- A shadow-test threshold depended on the box's speed. It was fixed, and the test passes at load
  22.

`clippy --workspace --all-targets -D warnings` is clean. `npm test` passed 688 with 0 failures,
typecheck is clean, and `infra/tests/lint.sh` passed every check. The compatibility string is
648 B `8ce67b97...` in raw and macros mode, unchanged.

**Review fix round, 2026-09-29** (APPROVE-WITH-NOTES). The branch was rebased onto port/task-01's
fix round, `2fc9fd7`: an artifact-backed task ledger, the palette seed, queued saves and bounded
history. The shadow now bounds its coordinator's history per segment. The fixes:

- *R1.* The verdict hashes `flysim-session`, SERVE-01's service, with the other release binaries.
  `check` defaults to `--binary flysim-session --current /opt/fly/current`. It requires the
  verdict's release to be the directory the link resolves to, with every shadowed binary
  unchanged there.
- *R2.* Every session-side failure is a divergence:
  - a session that does not start;
  - a restore-gate refusal (`tests/shadow.rs` now plants one);
  - a capture with no agent;
  - an unrecorded transition;
  - an unbuildable trace line.

  Skips carry a kind, and only the live side's own kinds allow a cutover.
- *N1.* The live trace needs the shadow's heartbeat to start and to keep going, and flysim keeps
  the trace directory under 8 GiB. The runbook's claim is corrected.
- *N2.* `fly-shadow-run` records a baseline and runs `flyshadow-guard.timer`. The guard stops
  everything and restarts flysim without the trace on 1 s of lag growth, or when the realtime
  factor drops below min(0.97, baseline - 0.03). `lint.sh` drives the rule on six cases.
- *N3.* Both `pass` and the cutover require one live save compared per 600 brain seconds, with at
  most 10 % unavailable. The cutover also requires a shadow at most 3,600 transitions behind.

**Cost re-measured on the rebased tip.** This was a 12-brain-minute rehearsal on the same box,
at real time, with a restart and the guard on. It had 43,104 transitions, 197 saves byte-identical
and zero divergence. The box was contended by other runs again; the guard paused the shadow for
911 s and it still passed. Unthrottled replays of its trace, all identical, gave these mean
milliseconds per frame:

| Mode | ms per frame (mean) |
| --- | --- |
| in-process, 2 threads | 15.1 |
| in-process, 3 threads | 13.3 |
| thread mode, 3 threads | 14.9 |
| process mode, 2 threads | 15.4 |

Earlier the same 2-thread and 3-thread in-process arms took 19.7 and 17.0 ms. With 3 threads the
shadow keeps real time with about 20 % to spare.

**Review round 2, 2026-09-30** (APPROVE-WITH-NOTES; R3 and G1 were required before `start`).
The branch was rebased onto port/task-01 `eaf588d`, and main v0.6.7 was merged in: row 68 and
adapter `pokered-unique8-v8`. The live shadow will therefore carry the v8 compatibility string.
The fixtures were already v8 on main.

- *R3.* `allows_cutover` floors the window at `REQUIRED_BRAIN_SECONDS` (10,800). A verdict run with
  a shorter `--required-brain-seconds` passes as a rehearsal but never passes `check`. There is a
  test case for this.
- *G1, the host guard.*
  - `start` measures a 10-minute baseline of 60 samples before the shadow exists: the median RTF,
    its spread as 1.4826 x MAD, and the lag growth rate over the most recent half.
  - The guard trips only on a sustained degradation *against that baseline*: 3 checks in a row in
    which the last 5 samples of the same process fall below it, in RTF or in lag rate, by more
    than max(0.05, 4 x spread / sqrt 5).
  - It stops itself when the shadow is not running. `check` refuses after a trip or with the
    shadow down.
  - Tested over two kinds of trace:
    - *Synthetic*, shaped on the release container: a flat 0.66, 0.77-0.99 and 0.6-1.0 noisy, per
      second and per sample, and real time with hiccups. There was no false trip in 800 3-hour
      runs, and every one of 400 degradations tripped (to 0.55-0.75, and 0.66 to 0.58).
    - *Recorded* from a real flysim, now fixtures: alone (at about 1.0, and at 0.4-1.0 under box
      load) no trip; with a real idle shadow on other cpus no trip; with a real shadow on the fly's
      own cpu (0.66 to 0.44-0.60) a trip at check 7.
  - One recording had a load step inside its baseline, which widened a mean/sd baseline to a
    0.25 margin. That is why the baseline is robust.
- *G1, the in-shadow back-off* (`LagJudge`). It pauses the shadow while the live lag grows faster
  than the baseline rate plus the margin (re-read from `baseline.json`). It lifts itself for
  10 minutes when a pause did not help, because then the shadow is not the cause. Tests show:
  - a flat 0.66 fly is never paused;
  - with no baseline, at most 1 minute in 11 is paused;
  - a shadow that costs the fly time stays paused about half the time.
- *N3.* `lagTransitions` is recomputed at every verdict write, over the rest of the current file
  and every newer one, including while the shadow is backed off.

**2026-09-30: `tests/shadow.rs` "one trace file per process" (0 vs 2) on a build box.** It is not
a timing, load or segment-handling bug.
- *Cause.* The failing runs used a `flysim` binary that was not built from the tree under test.
  `cargo test -p fly-legacy-session` does not build `flysim`'s binary, and the build box's bx
  target directory was shared by name with other trees. That binary predated `FLY_TRACE_DIR`, so
  it wrote no trace at all. The failure was reproduced by running such a test binary against its
  own stale `flysim`.
- *Fix.* The test now builds the `flysim` binary itself: `cargo build -p flysim --bin flysim`,
  with the same target directory and profile, which does nothing when the binary is current.
  This was proved on a target that another tree's `flysim` had populated.
- *What production could miss, and now cannot.* A live process that runs without a trace is now
  a `coverage` divergence (above). That covers a process whose consumer check failed, and one
  whose trace could not be created. In `FLY_TRACE_DIR` mode, a trace-creation failure no longer
  stops flysim from starting. The test adds such a process, untraced, and the shadow fails.

### SHADOW-02 — The shadow on a build box (port slice)

**2026-09-30: built** on `port/shadow-02` off main (v0.7.0), and awaiting review. It changes no
live behaviour until `fly-shadow-run start` runs; the flysim binary is unchanged. The contract is
the SHADOW-02 amendment of [legacy-gameboy-v1](legacy-gameboy-v1.md) section 18.

**Why.** On the release container the shadow (v0.7.0, started 2026-09-30) compared 1,119 brain
seconds with zero divergence, but at about 27 ms a frame on the page and encoder CPUs it fell
further behind every hour, and the live realtime factor fell from 0.9998 to 0.93-0.98. It went
back to 0.999-1.0 the moment the shadow was stopped.

- **`shadow::remote`**: the frame protocol (a JSON header line and a body), the name checks that
  guard every path, the forwarded settings (`FORWARDED_ENV`: game, macro mode, accepted adapters,
  pins; no path, no secret).
- **`shadow::relay`** (`fly-shadow relay`, the container's `flyshadow.service` through a drop-in):
  per quarter-second tick, the new saves, then the trace bytes of this run in file order, then
  the journal read before the listing. A writer thread owns the ssh pipe, so a stalled link never
  blocks it: saves are read as they appear and queued in memory (256 MiB), trace bytes wait on
  disk, and a newer file and the journal wait for every older file. The heartbeat is touched only
  while healthy (connected, the box's shadow of this run alive within 30 s, the box has every
  byte that was on disk a minute ago). The box's verdict is written back only when it is this
  run's, with the unsynced tail added to `lagTransitions` and a `relay` member.
- **`shadow::ingest`** (`fly-shadow ingest --root`, the forced command of the relay's key on the
  box): the release directory and binary hashes must equal its own; a new run id resets the mirror
  and restarts the shadow; appends only at the offset it holds (else `resync`); a file the shadow
  finished is never written again (`done`, and the relay then deletes the container's copy);
  reports liveness (heartbeat fresh, verdict this run's and running or passed).
- **The shadow**: `--run-id-file` (the verdict's `runId`; a new id exits 4); a `no-consumer` trace
  stop while following is a `coverage` divergence; coverage reads the journal before listing the
  traces. `shadow::release` holds the release hashing the binary and `check` share.
- **infra.** `fly-shadow-run start` runs remote when `/etc/fly/shadow-remote.env` exists and
  refuses otherwise (`--local` keeps the old placement); `check` also needs `relay.json` healthy
  and under a minute old; `status` shows the relay. `infra/box/` (never deployed by 05-deploy):
  `flyshadow-remote.service` (the same release at the same path, `--run-id-file`, no lag back-off,
  `Restart=always`, stays stopped on exit 3, `CPUWeight=1000`), `flyshadow-remote.path` (starts it
  on a new run id) and `fly-shadow-remote-setup` (MANIFEST-verified root-owned release, the
  `flyshadow` user, a root-owned `authorized_keys` with `restrict,command=` and optional `from=`).
  `02-base.sh` installs `openssh-client` (no sshd). Lint holds all of it; the runbook's shadow
  section has the set-up.

**Tests.** `tests/shadow_remote.rs` (ROM, the toy connectome) runs the real `flysim`, the relay and
the real `fly-shadow ingest` binary (over pipes where production has ssh) and a remote shadow:
a fresh fly and a restart with sugar pass and come back with the run id; a dead remote shadow stops
the heartbeat and flysim's trace, and the new shadow skips that stop as history (`trace-cap`) and
follows the next process; a stalled ingest leaves the relay reporting "not healthy", flysim stops
its trace, and when the link is back the box fails the verdict as `coverage` and the relay brings
it and `divergence.json` back and exits 3. Unit tests: frames, names, the release refusal, offsets
and `resync`, the run reset, the env allowlist.

**Two-box rehearsal** (FAFB, macros, row 67, real time, sugar every 10-30 s; a real flysim on one
build box standing in for the release container, the remote shadow on another behind a real sshd
with a `restrict,command=` key): see the SHADOW-02 run report. Cost of the shadow per live frame
(unthrottled replay of a recorded trace, mean of the step): fly-build-1 14.1 ms with 3 threads,
11.9 ms with 6; fly-build-2 13.9 / 13.0 / 12.2 ms with 3 / 4 / 6; fly-build-3 11.9 / 11.8 ms with
3 / 4 (beside another agent's 1.6-core soak). The relay took about 0.5 % of a core; the stream to
the box is about 2.1 GB per live hour (a 2.7 MB hot save every 5 s, and about 140 MB of trace).

**The trace's own cost on the live loop** (the coordinator's question): +1.3 ms a frame on the
sim thread (11.14 to 12.45 ms, unpaced, 3 threads, 4 A/B rounds on a build box), about half in
the post-frame digests (frame and work RAM) and half in the record's digests and JSON. A writer
thread for the digests and the file was tried and rejected: on flysim's pinned CPUs it preempts
the brain's parallel sweep and the frame took 19 ms. The live guard's baseline is taken before
the trace is on, so the trace's cost counts against it.

### STATE-01 — Coherent all-participant checkpoint/recovery

**Depends on:** SESSION-02, MEDIA-01; validate with fake agents first, then AGENT-01/ENV-01.

**Implement:** exact new envelope schema, compatibility manifest, Capture/StageRestore/
ActivateRestore, bounded writer, durable commit acknowledgments and fresh-epoch fencing.
Keep old `FLYSIM01` reader separate. Payloads and coordinator state must refer to one boundary.

**Acceptance:** uninterrupted versus resumed synthetic/real-agent traces match after accounting
for new epoch metadata; corrupt any participant and installation fails as a group; lost save
reply doesn't advance durable metadata; failure during activation cannot resume half a world.
Checkpoint queue stress remains bounded; verify old media/parser data cannot cross recovery.

### STATE-02 — The legacy composition's checkpoints in `FLYSIM01` (port slice)

**2026-09-29: built** on `port/state-02` off `port/integration` and awaiting review. The operator's
decision of 2026-09-23 makes `FLYSIM01` the format of record until RETIRE-01, and the session
runtime exports it on every durable save ([legacy-gameboy-v1](legacy-gameboy-v1.md) section 16).
This slice is that export, the import back, the store policy and the restore selection. It adds
no `FLYSESS1` production file. Nothing in `flysim`'s frame, checkpoint bytes or compatibility
string changes.

- **One store, two runtimes.** `flysim`'s `store.rs` and `journal.rs` move unchanged into the new
  crate `flysim-store`, which `flysim` re-exports (`flysim::store`, `flysim::journal`) and
  `fly-session` links. Both runtimes write through the one `store::encode` and commit through the
  one `Store`, so they share the envelope bytes, the generations, `manifest.json`, the
  `milestone-<N>` archives, `keep_generations` rotation and the restore order by construction.
- **Export** (`fly-session::legacy_checkpoint`). A `FLYSIM01` file is assembled from its owners'
  halves: the agent's `FLYAGT01` capture payload (the seven agent chunks, the remainder already the
  accumulator's), the world's `FLYENV01` payload (emulator, frame on screen, joypad, frame
  counter, cartridge digest, and slot `best` as `ratchetGame`/`ratchetFrame`), the task's
  `{reward, ratchet}` and the host's `{generation, wallMs, compatibility, speed, rankSinceMs,
  lastEventId}`. `runtime_state` builds the legacy `RuntimeState` field for field as
  `Sim::snapshot_state` does.
- **Import.** `split`/`halves` read a `FLYSIM01` file back into those halves. `agent_payload` is
  the shipped form of AGENT-01's `seed_from_legacy_state` prototype. It builds the `FLYAGT01`
  payload a replacement agent stages, using the same encoder the worker's own capture uses
  (`legacy_agent::encode_payload`). The accumulator is `{remainder, executedTicks = network.ms,
  warmupOffset = 2500}`. `learning.updates` is the file's `reinforcements`. A file from before
  that member starts at `plasticity.updates` once (`legacy_reinforcements`, section 13
  amendment). The checkpoint id is the file's generation (`flysim01-g<N>`). No template worker
  is involved. This fixes the four points the AGENT-01 review raised against the prototype:
  template dependence, borrowed ids, no compatibility gate (the gate is `restore_from_store`'s,
  below), and the count falling back on every round trip. The payload also carries the file's
  `framebuffer` as an `inputFrame` chunk, which `State.StageRestore` installs as the next input.
  `LegacyFrame::restore` re-projects the framebuffer and does not keep the saved visual drive,
  and the two differ on the row 65 yard survey checkpoint (found here; see the section 13
  amendment). `world_payload` is ENV-01's `FLYENV01` from
  the same world. The source scope of both is `(session, "legacy", emulatorFrame - 1)`.
  `legacy_parity` now seeds its workers through this import.
- **`reinforcements`, an optional `FLYSIM01` manifest member.** `LegacyFrame` now counts
  reinforcement calls the way the agent worker does: one per commit while the rule is enabled,
  zero sums included. `Sim` writes the count, and `LegacyFrame::restore` reads it (falling back
  to `plasticity.updates`), so the count survives every round trip in either runtime. Readers
  that predate it ignore it. The member changes no behaviour and not the compatibility string,
  but it does change legacy checkpoint bytes: a file written from now on is the old layout plus
  this member.
- **Store policy.** `LegacyCheckpointer` follows `Sim` at each call site:
  - one generation counter over both stores, starting above the highest either has allocated;
  - a hot copy every `hot_seconds`, and a durable one every `checkpoint_seconds`, which also
    restarts the hot interval;
  - a durable save for a climb, archived as `milestone-<rank>` when the rank is above every rank
    this process has archived (`best_archived_rank`, per process as in the legacy loop);
  - rank tracking as in `track_rank`;
  - encoding and fsyncs on a writer thread.
- **Restore selection.** `restore_from_store` walks `store::restore_order`: hot latest, hot
  previous, durable latest, durable previous, then the archives by descending rank, each as its
  generation and then as its `milestone-<N>` file. It checks the legacy gate: the cartridge, and
  `flybrain_gb::compatibility::decide` with `FLY_ACCEPT_ADAPTERS`. It moves to the next candidate
  on *any* refusal, including a participant's `State.StageRestore`. It reports `Fresh` when no
  candidate exists, and `Refused` when every candidate failed. In that case the runtime must exit
  as `Sim::restore_or_warm_up` does.
- **The agent worker** now refuses a conflicting stage (a restore already staged, or a token
  already activated) before it builds a replacement network (AGENT-01 review).
- **Sugar journal** (the FND-01 review note, before SHADOW-01). A per-process boot header
  records the runtime, start frame, brain ms, restore origin, generation and compatibility. The
  journal rotates at 4 MiB and keeps three files; every rotated-in file starts with a
  `continues` line. `journal::clear` removes the journal. `flysim` writes the header at boot, and
  `flysim --reset-to-milestone` clears the journal after it has copied both stores aside.
  `read_segments` cuts the journal into per-process segments. `read` still returns input lines
  only.

**Boundary order the session host must keep** (section 16, and section 11 step 6). At `Ready(k+1)`
the order is:

1. `Environment.SaveSlot`, if one is due.
2. The milestone capture, if the rank climbed.
3. The rollback, if one was requested.
4. The durable capture after the rollback, then the hot and durable interval saves.

The capture itself uses `State.Capture` on the world and the agent at the same committed
boundary, plus the task's ledger, `{reward, ratchet, slotFilled}` (`TaskHalf::to_ledger`). Wiring
it into the coordinator and the boot-time restore path is left to the legacy composition's
coordinator (TASK-01); this slice ships the pieces and proves them on the workers.

**Proof** (flysim `tests/state_02.rs`, `tests/state_02_store.rs`; `fly-session`
`legacy_checkpoint` unit tests):

- *legacy -> session -> legacy on 18 real checkpoints* (every distinct rom-env checkpoint, which
  includes the row 64, row 65 and row 67 ones). Each file is split and the world is restored
  into the real environment worker and captured again. The agent goes through the imported
  `FLYAGT01` payload and the task through the Pokémon adapter and the ratchet. Every export is
  byte-identical to the legacy loop's own restore outcome of the same file, checkpointed at once.
  Against the files themselves, every export differs only in named fields. All 18 gain
  `reinforcements`, because the files predate it. Twelve written by adapters v5 or v6 also
  differ in `reward`, which the adapter's import migrates. One survey checkpoint also differs in
  a joypad byte of its emulator state that its own `buttons` field does not match; both runtimes
  apply `buttons` on restore. Before the counter was added, six of the 18 were byte-identical to
  the file.
- *Restore then continue, toy connectome, raw mode* (every ROM run). The legacy frame writes a
  checkpoint. The session runtime selects it from a store whose hot copy is torn. The legacy
  loop and the session runtime then each run 600 frames from it. World: 1,800 values identical
  to `FLY_TRACE` (frame, WRAM). Agent: 600 transitions identical (ticks, clock, exact remainder,
  rates, spikes, decision). The session runtime's export at the last boundary is byte-identical
  to the legacy loop's checkpoint of that boundary. That export also survives session -> legacy
  -> session unchanged: a fresh legacy loop restores it and checkpoints the same bytes, and a
  fresh world and agent import it and export the same bytes.
- *The same with `gameboy-legacy-fafb-v783-v1` in macros mode* from the row 67 checkpoint, with
  sugar and a ratchet rollback (`FLY_STATE02_FAFB=1`). Four checkpoints were run for 1,800 frames
  each: rows 58, 64 and 67, and the row 65 yard survey. Each run has 5,400 world values and 1,800
  agent transitions identical to the trace, with 2 sugar and 1 rollback. Each export is
  byte-identical to the legacy checkpoint and survives session -> legacy -> session.
- *The tools, both ways* (`tests/state_02_store.rs`, the real `infra/bin` scripts and the real
  binary). `fly-loop-reset --list` and `--print-state-compatibility` read a store the session
  runtime wrote. `fly-reset-to-milestone` promotes its rung with the recovery budget back and
  clears the journal. The legacy order and the session runtime then choose the same candidate,
  and the session runtime carries on above every generation and archives its next climb. The
  other way round, the session runtime restores a store the legacy writer wrote and the tool
  reset, and it would save it as the same bytes.

**Not in this slice, and why:**

- *The coordinator hook and the boot-time restore.* Both need the legacy composition's
  bootstrap, which is TASK-01's. The restore order is Initialize-free: stage and activate the
  world, have the task install `{reward, ratchet}` and observe O[k] for the first context
  (`{boot, bound, location: null}`), then build the agent's payload with that context
  (`agent_payload`) and stage and activate it.
- *The task's per-frame half in the proofs.* The executor and the task's evaluation are TASK-01's.
  The proofs take the joypad masks from the trace and the task's end-of-run `{reward, ratchet}`
  from the legacy loop.
- *Declared difference, archive order* (sections 4 and 16). A session-runtime milestone archive
  holds the post-capture ratchet and slot. It is excluded from the shadow comparison.

### SERVE-01 — The session runtime as the live service (port slice)

**2026-09-29: built** on `port/serve-01` off `port/task-01` and awaiting review. It makes the
session runtime a drop-in for the live `flysim` service, so CUT-01 can switch the stream over
with one command and roll back with one. It changes no live behaviour: the legacy loop's frame,
feed, checkpoint bytes and compatibility string are unchanged, and nothing runs the new binary
until `fly-runtime session` is run on a container.

- **One service shell, two runtimes** (`flysim::serve`). `flysim::run` is split: `serve(config,
  sim)` binds the feed (`:7400`, or the feed bus that `fly-edge` serves), the control API
  (`:7401`) and the metrics listener (`:9101`), handles SIGTERM/SIGINT/SIGHUP, and runs `sim` on
  the calling thread with the `Shared` state, the snapshot slot and the command queue. The
  legacy loop is `serve(config, Sim::boot + Sim::run)`. The pieces of a feed header that come
  from the fly (`feed_rates`, `feed_learning`, `feed_game`, `feed_milestone`, `feed_sugar`) and
  the chat admission (`admit_chat`) move out of `Sim` into free functions both runtimes call.
  The feed codec, the bus publisher, the router, `/status`, `/events`, `/metrics` and the
  WebSocket framing are therefore one copy of the code whichever runtime is behind them.
- **The service host** (`fly-legacy-session::service`, binary `flysim-session`). It reads
  `flysim`'s own `Config` (so `/etc/fly/fly.env`, `FLY_ACCEPT_ADAPTERS` through STATE-02's
  `RestoreGate`, the macro mode, speed, threads, chat, the store paths) and keeps `Sim`'s order
  call site for call site: boot by TASK-01's `boot_unsaved` (the legacy candidate order and gate),
  the boot event, the chat sidecar, the journal's boot header (`runtime: fly-session`), the
  startup durable save, then per frame the command drain, `Coordinator::step`, the task's macro
  and reward events, the rank and its milestone archive, the deferred ratchet rollback with its
  recovery event and durable save, the 30 Hz publish, the hot (5 s) and durable (300 s) wall-clock
  saves, the event log fsync and flysim's `Pacer`. Saves are queued (`queue_save`, TASK-01) so the
  encoding and the fsyncs stay off the loop, as the legacy writer thread keeps them.
- **Where each header field comes from.** The frame is the committed `lcd` view; the audio is
  each transition's `apu` chunk through the legacy DC blocker (`DcBlocker::process_f32_into`,
  the same filter on the environment's `v / 255`); the spike bitset is the OR of the commits'
  `telemetry.spikes` since the last publish, which equals the legacy window exactly; `buttons`
  is the executor's mask (the checkpoint's after an import, 0 after a rollback, as
  `LegacyFrame` has it); the game, milestone and macro fields are the task's own adapter,
  ratchet and macro layer (`PokeredTask::inspect`, `take_feed`). The numbers from inside the
  network that no session message carries -- the plasticity statistics (`learning.changed`,
  `learning.synapses`), the decoder's baselines and scores for `/status`, and the rates and pulse
  at the boundary -- come from one declared extension of the legacy agent,
  `Legacy.FeedStatus` (capability `legacy-feed-status-v1`): read-only, answered at a committed
  `Ready(k)`, once per published snapshot, never in a trace. `Coordinator::read_agent_extension`
  calls it without fencing the epoch; `Coordinator::media_bytes` reads the committed view and
  audio chunk.
- **Sugar.** flysim's `RateLimiter` and clamp, over the pulse the network holds: the last commit's
  `stimulusRemainingMs`, raised by each sugar admitted since (`network.stimulate` is a maximum),
  so a second request in one drain is refused with the legacy `retryAfterMs`. The admission lands
  in the next Prepare, which is the frame the legacy drain applies it before, and the journal
  stamps it with that frame.
- **Long-running hygiene.** The coordinator kept every transition's trace, every phase change,
  an audit line per step and every timing sample: about 120 KB/s of heap on the live fly
  (the first soak's RSS). `Coordinator::trim_records` bounds them at each boundary; the timings
  are logged once a minute as a `session profile` line and cleared.
- **Declared differences** (the rest is byte-equal, below): `POST /reward` is always 403
  (`control.allow_reward` is forced off; the operator pulse has no session-framework
  counterpart, `legacy-gameboy-v1` section 15); a fresh start publishes no audio for its setup
  frame (the environment discards O[0]'s); `FLY_TRACE` and `FLY_PROFILE_SECONDS` are the legacy
  loop's tools; only the Pokémon composition exists (another `FLY_GAME` is refused at start); the
  toy connectome cannot run macros mode (AGENT-01 refuses a macro channel the dataset does not
  track, and the toy tracks none).
- **Infra.** `flysim.service` stays the fly's one unit. `infra/bin/fly-runtime session|legacy|status`
  switches the binary it runs with one drop-in (`flysim.service.d/10-runtime.conf`), so every
  consumer that names `flysim.service` -- `fly.target`, `flyedge.service`'s `Requires=`, the
  watchdog, `fly-loop-recover`'s sudoers-granted restart, `fly-loop-reset`,
  `fly-reset-to-milestone`, the unstick rule -- works unchanged and the choice survives reboots and
  deploys. It refuses a release without `flysim-session` or with a different compatibility
  string, waits for `/healthz` and an advancing frame, and falls back to legacy by itself if the
  session runtime does not come up. `flysim-session.service` is the standalone unit (flysim's
  limits and cpuset, `Conflicts=flysim.service`, no `[Install]`) for rehearsals.
  `build-flysim.sh`/`package-release.sh` ship `flysim-session` and `fly-session`; `05-deploy.sh`
  installs `fly-runtime`, writes the standalone unit's cpuset drop-in and refuses a release
  without `flysim-session` while the drop-in is present; tmpfiles creates `/run/fly/session`.
  The runbook's "Switch the runtime" is the operator's page.

**Proof** (`fly-legacy-session/tests/service_parity.rs`, `journal_replay.rs`, rom-env; the SERVE-01
run report has the numbers). Both runtimes are started from the same seeded store behind
flysim's own control router and driven by one script of control requests, each landing before
the same frame: every running feed header is equal field by field (wall-clock fields excepted)
and every frame, audio and spike attachment byte for byte; every control response is equal
(`/stimulate` 202/429 with the same `retryAfterMs`, `/reward` 403, `/chat` 202/422,
`/checkpoint` the same generation, `/pause`, `/resume`, `/events`, 404s); the event log, the
journal and the final checkpoint are equal. Toy connectome raw mode, and the real connectome in
macros mode through a ratchet rollback, each in-process and as processes. A soak of both
runtimes side by side with the stage's decoder and the bridge's `HttpSimClient`
(`tools/runtime-soak.ts`), the hot/durable cadence sampled from the stores, and a `SIGKILL`
whose journal is replayed from the seed in the legacy loop onto the killed process's last hot
checkpoint.

Results (2026-09-29, the build boxes; the run report has the logs):

| Proof | Result |
| --- | --- |
| Toy raw, 120 frames, 19 scripted requests incl. two sugars in one drain | identical, in-process and process |
| Real connectome, macros, `rollback` checkpoint (ratchet rollback on frame 1), 120 frames | identical, both modes, before and after the TASK-01 fixes |
| Real connectome, macros, engage checkpoint (ledger thinned), 2400 frames, 46 macro and 2 reward events | identical, both modes |
| Real connectome, macros, row 64 (a 42 KB adapter ledger, TASK-01 B1), 1200 frames | identical, both modes |
| Soak, 35 min, both runtimes from the row 67 checkpoint, production configuration | 0 decode or schema errors, no 2 s stall, `/healthz` 100 %, event ids gapless, hot every 5 s and durable at 300 s on both (same intervals) |
| Soak, 36 min, the session killed -9 at +6 min | back in about 1 s from the hot checkpoint 3.5 s old, 1 `/healthz` 503, the feed client reconnected (533 ms gap), two journal segments |
| The killed process's journal, 17 sugars, replayed from the seed in the legacy loop | 18,838 frames onto its last hot checkpoint: agent, emulator, frame, adapter, ratchet equal |
| Feed over the bus (`FLY_FEED_VIA=bus`) with `fly-edge` | 3,050 snapshots through the edge, 0 errors |

Open for CUT-01: **speed.** Unpaced on a shared box the session ran 54-61 fps where the legacy
loop ran 68-118 (per frame p50: `Agent.Prepare` 7.8 ms, `Environment.Advance` 2.6,
`Agent.Commit` 1.6, publication 0.4; `Legacy.FeedStatus` 0.9 ms at 30 Hz). The live container
must be measured (SHADOW-01's cost run) before cutover. **Memory.** After `trim_records` the
session's RSS still grows about 5-8 MB an hour, tied to the saves (an unpaced run without saves
is flat); at `MemoryMax=4G` that is weeks, and a restart clears it, but it is not explained yet.
**Number form.** A checkpoint the session writes carries the adapter ledger's integral numbers
as `7580031` where the legacy loop writes `7580031.0` (the ledger crossed canonical JSON); equal
values, and both runtimes read both.

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
