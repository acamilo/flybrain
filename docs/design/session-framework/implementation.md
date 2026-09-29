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
- **FLYSIM01 as the start** (`fly-legacy-session::import`). This is the import AGENT-01 and ENV-01
  left open. The world, agent and task halves go into each participant's own capture format, and
  the ordinary group restore installs them: stage, validate, activate, `Paused(k)`, resume. The
  world is replaced by a fresh one first, because ENV-01 stages only on a replacement. The context
  the agent resumes with is computed from the checkpoint's own memory image, and the coordinator
  holds the task to it after the install (`Task::restored`).
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
