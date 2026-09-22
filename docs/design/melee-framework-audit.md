# Melee: multi-fly runtime, emulator and broadcast audit

Status: **research and proposed implementation plan**. Written 2026-09-18 against local
`f7bc13a`. Extends the [modular-session design](malecns-modular-sessions.md) and
[implementation backlog](malecns-modular-implementation.md) with a concrete second game.
Existing [feed](../feed-protocol.md) and [control](../control-api.md) contracts still win.
No emulator, game image, deployment host or live broadcast was run for this audit.

Follow-on [session framework contracts](session-framework/README.md) define the concrete
multi-process synchronization and worker interfaces this backend will implement.
The subsequent [Flybus decision](session-framework/bus-v1.md) selects one small Rust router
for RPC and pub/sub, with immutable artifacts outside messages and delivery-scoped GC.
The application owns supervisory/presentation behavior; the bus owns no game or stream logic.

## 1. Executive decision

**Use Dolphin, initially a pinned mainline-based Slippi Dolphin build, as a separate backend
process. Use the maintained libmelee fork to accelerate the integration spike. Keep the
brains and session coordinator in Rust.** Prove its synchronization, rendered sensory input,
and recovery behavior before selecting a production build. Keep stock Dolphin plus a narrow
backend hook as the fallback if the Slippi path cannot satisfy those requirements cleanly.

Do not port Melee to native code, embed Dolphin into the neural crate, run one emulator per
fighter, or assume “libmelee has `step()`” supplies the complete environment contract.

Recommended first target:

- Melee US 1.02, local two-player versus, one emulator and two independently stateful flies.
- Existing FAFB brains first. MaleCNS is an independent axis of experimentation, not a
  prerequisite for solving the emulator and multiplayer problems.
- Fixed declared characters, stage, stock/time rules and input profiles; no netplay rollback.
- Pixel-based sensory input with an explicit resolution/aspect transform; state inspection
  is for task measurement, match lifecycle and display, not a hidden fighting policy.
- Fixed GameCube controller readout with bounded analog values and short frame-based holds.
- Native-resolution rendering initially, local broadcast at 30 fps first; simulation/input
  continue at the backend's approximately 60-Hz cadence. Promote to a 60-fps show only after
  the full media path and encoder are verified.
- Two-fly synthetic arena remains the framework test case before game integration.

The important scaling change is **one environment with many agents**, not simply a larger
ROM. Disc size is mostly a loading/storage concern. Runtime cost comes from PowerPC emulation,
graphics/audio, multiple neural simulations, synchronization and media copies.

### Confidence labels used below

- **Observed in source:** checked implementation or explicit upstream documentation.
- **Recommended:** proposed architecture/configuration, not implemented here.
- **Must measure:** cannot be established by reading code, including throughput, latency,
  correct input-to-frame association and reliable recovery on the target platform.

## 2. What the Melee decompilation gives us

The supplied [doldecomp/melee](https://github.com/doldecomp/melee) repository is a matching
decompilation of **US 1.02**. Its README is `.github/README.md`, not the repository-root
`README.md`. The inspected revision is recorded in section 13.

The README and `config/GALE01/config.yml` identify the matching `main.dol` SHA-1 as
`08e0bf20134dfcb260699671004527b2d6bb1a45`. That identifies the executable, **not the entire
disc image**. Our run manifest must separately identify externally supplied game content,
effective executable, modifications, emulator build and task interpretation.

The decomp builds a GameCube executable, not a supported desktop port or emulator replacement.
The `dolphin` code within that source tree refers to Nintendo's SDK, not the Dolphin emulator
project. Rebuilding/relocating a DOL for instrumentation changes address identity; never use
stock symbol addresses on a shifted build.

### 2.1 Useful inspection map

| Upstream source | What was observed | How it helps our task |
| --- | --- | --- |
| `config/GALE01/{config.yml,symbols.txt}`; `docs/symbols.md` | Matching binary identity and named symbols with addresses, sections and attributes | Reproducible symbol/inspection manifest analogous to the Pokémon symbol generator |
| `src/melee/pl/player.h` and `player.c` | `StaticPlayer`, getters for stocks/damage/controller index, KO-by-player counters and self-destructs | Distinguish controller port, player slot and match attribution instead of assuming they are identical |
| `src/melee/ft/types.h` | `Fighter`, player/controller identifiers, buffered sticks/triggers/buttons, pressed/released edges, damage state, source-player field and move-instance information | Audit controls and potential reward attribution; source fields are hypotheses to validate against live transitions |
| `src/melee/gm/types.h` | Match frame/timer fields, `MatchEnd`, winner arrays and exit/results structures | Episode boundaries, timeout/results interpretation, one-time terminal rewards |
| `src/melee/gm/gmvsmelee.h` | Character/stage select, versus entry/exit, sudden-death and results transitions | An explicit lifecycle model instead of treating every screen as a playable frame |
| `src/melee/{cm,gr,mp,it}/` | Camera, stages, map/collision and item subsystems identified by upstream module structure | Follow-up inspection points for view geometry, hazards and projectiles; not all audited in this pass |

Examples of concrete distinctions:

- `StaticPlayer` has a controller index, a player ID and up to two sub-fighter entities.
  Ice Climbers and transformations defeat “one visible fighter object = one agent.”
- The input struct tracks three-entry analog/button histories plus pressed/released buttons
  and threshold timers. A constant button hold and a sequence of taps are different actions.
- Damage includes an annotated source-player number, but it must be checked for projectiles,
  stale ownership, self-damage and indirect KOs before it becomes a reward source.
- The match structures include winner counts/arrays. A terminal result is not safely inferred
  from whichever player's stock decrement happened to be sampled first.

### 2.2 How to use it without making the framework Melee-specific

Create a **task-local** inspection catalog: field name, binary/decomp revision, symbol or
pointer traversal, type/endianness, valid scenes, tested transitions and unsupported cases.
Prefer Slippi telemetry for fields it supplies reliably; use decomp-grounded inspection for
missing fields only after verification. Do not expose all emulator memory to every agent.

If memory inspection is needed, decode big-endian integer/float fields and emulated 32-bit
pointers explicitly. Never cast emulated bytes to a native Rust/C struct whose layout,
pointer width or bitfield ordering is different. Sample a consistent backend boundary rather
than reading a moving process asynchronously. Accessors named in the decomp explain semantics;
they are not functions our host process can call in place of an adapter.

Derive small constant/schema outputs and synthetic fixtures where appropriate. Do not vendor
the entire decomp, game executable, assets or save states merely to read stocks and damage.
The first working backend does not require rebuilding Melee. Custom hooks or patches are
separately identified scaffold and enter the run's behavior/content identity.

## 3. Emulator options and recommendation

All candidates below emulate GameCube software. The differentiator is the host integration,
not whether Melee can theoretically boot.

| Candidate | Evidence / useful capability | Gap or cost | Decision |
| --- | --- | --- | --- |
| **Mainline-based Slippi Dolphin + maintained libmelee** | Structured game/port events, controller pipes, explicit blocking-input support; Linux rendered path available in the ecosystem | Pixel/audio access and coherent external save-state control still need integration; pin emulator, Gecko codes and parser together | **First spike and preferred initial integration** |
| **Stock Dolphin + narrow host hook** | Source has `Core::DoFrameStep`, CPU-thread coordination and `State::{SaveToBuffer,LoadFromBuffer}` | These are internal APIs, not a stable remote environment SDK; own a small patch and state parser/telemetry bridge | Fallback or eventual generic Dolphin backend if its maintenance cost is justified |
| **Felk Python-scripting Dolphin branch** | Inspected stubs expose controller/memory/save-state scripting and rendered-frame events | Historical branch; `await frameadvance()` is documented as waiting for a rendered-frame event, not proof of a paused one-step transaction | Research reference or temporary probe, not default production dependency |
| **Custom EXI/fast-forward Slippi-Ishiiruka** | Maintained libmelee README describes accelerated ML mode and EXI inputs | That documented fast path disables rendering; inspected libmelee rejects non-Null graphics for the EXI_AI build | Useful for explicitly state-driven offline research, not the pixel-fed live baseline |
| **Libretro Dolphin core** | Potential common frontend ABI | No core-specific synchronization/state/render benchmark was performed; another integration layer does not remove task semantics | Defer rather than introduce an unverified second dependency stack |
| **Native game port built from the decomp** | Source enables modding/research | Matching DOL compilation is not native execution; graphics, OS/SDK, timing and assets remain substantial work | Outside this project's first Melee phase |

Use the maintained **`vladfi1/libmelee`**, not a floating install selected by an old tutorial.
`altf4/libmelee` says it is archived and points there. The maintained fork says it became
the PyPI `melee` source starting at 0.45.0; pin the actual chosen package/source revision and
its dependencies instead of assuming an unversioned `pip install melee` reproduces a run.

The maintained README describes raw-state compatibility, but the inspected `Console.step()`
still invokes `__fixframeindexing` and `__fixiasa`. Therefore, confirm field semantics from
the installed source and observation fixtures rather than trusting README wording. Our
adapter identifies parser/normalization revision as part of task identity.

### 3.1 What the blocking path actually does

**Observed in source:**

1. `libmelee.Console` defaults `blocking_input=False`. Setting it true writes
   `Slippi/BlockingPipes` for its mainline backend.
2. `Console.step()` flushes its registered controllers and then dispatches game/menu events
   until a frame boundary. It is not a method returning RGBA pixels or arbitrary game state.
3. Slippi's `EXI_DeviceSlippi.cpp` sets `g_need_input_for_frame` on game setup, menu frames
   and frame bookends.
4. `Pipes.cpp::UpdateInput` checks the blocking setting and flag, waiting for commands through
   `FLUSH`. Its Linux wait path uses `select`; the inspected Windows wait helper is not implemented.
5. `ControllerInterface::UpdateInput` updates devices and only then clears the flag. The
   `FLUSH` handler explicitly avoids clearing it before the other devices have been read.

That is strong evidence for trying a Linux multi-port blocking backend. It is **not yet a
measurement** that action batch `t` affects exactly our desired frame `t+1`, that menu/game
boundaries behave identically, or that rendering/audio are coherent with the telemetry.

Keep at most **one batch outstanding**. The pipe implementation consumes buffered commands,
and a backlog of frame batches must not collapse into a latest-state input. Create an isolated
Dolphin user directory containing only the intended pipe devices: unused/abandoned devices
can participate in updates and leave blocking input waiting on a controller nobody drives.

For two flies, stage both full controller states before calling the single owner's
`Console.step()`. Do not give each agent a `Console` loop. Verify which input sample corresponds
to returned pre/post-frame telemetry with distinguishable action pulses and deliberate delays
on each port. “Both bots sent commands” is weaker than “both commands landed on one frame.”

### 3.2 What libmelee does not establish for us

- No framebuffer-returning or whole-session save/load interface was found in the inspected
  `Console` API. `DumpConfig` configures media dumping; a dump is not automatically a bounded,
  timestamped sensory-frame transport.
- GameCube pad values are stateful. Unchanged buttons remain held; the backend must emit an
  explicit complete state or compute trustworthy deltas, including release/neutral values.
- The library applies analog normalization (`fix_analog_stick`, `fix_analog_trigger`). Define
  our canonical ranges and apply conversion exactly once; test round trips at neutral,
  extremes, diagonals, dead zones and trigger-click thresholds.
- Rollback skipping and internal controller flushes are present. Use local offline matches
  first; do not confuse filtered rollback frames with advancing brains through speculative time.
- Initial game events can flush neutral input internally. Record startup as lifecycle scaffold;
  do not attribute that to a neural decision.
- Spectator transport keepalive, pipe blocking and process liveness are separate. A healthy
  connection does not prove the match or renderer is advancing.

## 4. Audit of our current system: keep, extract, replace

Rust path aliases: `core/`, `gb/`, `sim/` mean the respective crates under
`services/flysim/crates/` named `flybrain-core`, `flybrain-gb`, and `flysim`.

| Finding | Current evidence | Required change for Melee/multiple flies | Priority |
| --- | --- | --- | --- |
| Single world and brain bundled together | `sim/src/simloop.rs::Sim` owns one `NeuralAgent`, concrete `Emulator`, adapter and ratchet | Session owns one environment plus agent collection and explicit port map | Blocking |
| Direct Game Boy calls in frame loop | `step_frame`, `to_button_mask`, `set_buttons(u8)`, `run_frame`, fixed framebuffer copy | Backend interface with full action batch, rational cadence, observations and media capabilities | Blocking |
| Task trait carries Pokémon concepts | `gb/src/adapter.rs` includes `read8(u16)`, map/exit/objective hooks | Keep inspector and macros task-local; use generic reward/episode/progress outputs | Blocking |
| Timing defaults embed Game Boy | `core/src/agent.rs`, `sim/src/config.rs` | Session clock derived from backend, per-agent neural remainders; preserve old arithmetic in legacy facade | Blocking |
| Decoder tuned to walking through Pokémon maps | `packages/brain/src/readout/presets/gameboy.ts`: 800-ms directions, 85-ms A/B pulses with 480-ms cooldown | New fixed GameCube mapping; frame-scale controls, analog sticks/triggers, concurrent movement/action | Blocking |
| Neural code is already independently useful | `core/src/lif.rs`, `agent.rs`, `plasticity.rs`; TS counterparts | Reuse exact kernel and private per-agent state; do not replace neural semantics to integrate a game | Keep |
| Graph can be shared, pool cannot be concurrently dispatched | `Arc<BrainDataset>`, `SweepPlan`, `core/src/pool.rs` shared job slot | Immutable topology shared; distinct mutable state and controlled total scheduling budget | Blocking |
| CUDA exists but not an automatic service optimization | `core/src/lif/cuda.rs`; no `enable_cuda` call found in `sim/src/simloop.rs` | Explicit backend selection, equivalence/restore tests, profile first; don't promise GPU brains from graphics availability | Measured option |
| Snapshot header is a single Pokémon-shaped view | `sim/src/snapshot.rs`, `packages/feed/src/{types,codec}.ts` | Session descriptors, multiple agents/ports, task-specific progress, named attachments/media | Blocking for proper broadcast |
| Stage assumes Game Boy geometry and one fly | `apps/stage/src/{App.tsx,feed/store.ts,feed/decode.ts,lib/geometry.ts}` | Per-session/agent store instances, dynamic view aspect/dimensions, multi-agent match layout | Blocking for proper broadcast |
| Browser owns game audio | `apps/stage/src/audio/engine.ts`, 48-kHz feed, Pulse capture | Explicit audio producer and media-clock policy; do not play native Dolphin audio and forwarded PCM twice | Blocking |
| Capture already offers NVENC | `infra/bin/flycast-launch` | Reuse encode/relay/recording; measure new compositor/readback cost and revise 60-fps settings | Keep with changes |
| Encoder hardcodes H.264 level 4.1 | Both encoder functions in `flycast-launch` | 1080p60 needs a suitable level (normally 4.2 or automatic selection); changing only `FLY_FPS` is insufficient | Required for 1080p60 |
| Existing checkpoint payload is single-agent/binary-specific | `sim/src/store.rs`, `gb/src/compatibility.rs` | Coherent all-agent/world checkpoint plus backend/parser/patch/controller identity | Blocking for exact resume |
| Checkpoint writer queue is unbounded | `Sim::start_writer` uses `std::sync::mpsc::channel` | Bound/coalesce background work; larger emulator captures and several brain copies must not grow an unlimited queue | High |
| Existing “saved” event precedes durable commit | `checkpoint_with_reply` emits after enqueue; writer updates durable metrics on success | Distinguish capture/enqueue/commit/failure events; public status must not report a queued Melee save as durable | High |
| Recovery assumes a best progress ladder | `gb/src/{ratchet,recovery}.rs` | Match/episode reset policy; never rewind one player's world independently | Blocking |
| Health is mostly loop heartbeat | `sim/src/simloop.rs::Shared`, `sim/src/lib.rs` | Distinguish waiting at input barrier, intentional pause, backend timeout and deadlock; keep host supervision responsive | High |
| Deployment resource partitions reflect the old stack | `infra/units/flysim.service`, deploy cpuset construction | Budget Dolphin CPU/GPU plus N brains and media; measure and set new memory/process limits | Required before release |
| Bridge targets one sim | `services/bridge/src/{sim,commands,redemptions}.ts` | Explicit stable agent targeting and intervention policy; no viewer control-port endpoint | Before interactive show |

Do not interpret dated CPU/GPU measurements in the repository as current free capacity.
The records identify bandwidth contention and graphics-sharing constraints, but this audit
does not inspect the host or establish that it can run two brains plus Dolphin in real time.

## 5. A framework architecture that survives a third game

### 5.1 Four runtime components, not one enormous adapter

```text
Application / supervisory logic                 Presentation / recording
                   \                              /
                         Flybus RPC + pub/sub
                   /             |                \
       Session coordinator   Agent workers      Backend helper
       clock / barrier       brain / encoder    Python + libmelee initially
       task + executors      fixed readout      owns Dolphin process/user dir
       episode policy                           native pipes, parser, media/state hooks
                   \             |                /
                      owned artifact references
                                |
                       immutable local store

Presentation owns stage/compositor → capture → local relay → optional Twitch push.
```

The helper is an **internal backend implementation**, not an audience-accessible controller
service. The session remains the sole authority assigning actions to ports. Python never
simulates the neurons or selects actions. Keep it if measurements say its overhead is small;
replace its internals with Rust only when an actual bottleneck or maintenance
requirement justifies it.
Its Flybus binding uses the same protocol as every component. Dolphin-specific input pipes
stay behind the environment adapter, not a second framework communications system.

An external emulator is a normal `Environment` implementation, not a special `if melee`
branch sprinkled throughout the session. For Game Boy, the same interface has an in-process
implementation. For an embodied world, it may be a native physics engine. Consumers do not
need to know which one owns the world.

### 5.2 Framework contracts to extract

| Contract | Owns | Must not know |
| --- | --- | --- |
| `Brain` / numerical core | Tick semantics, state, spikes, rates, learning updates | Game, process, controller labels or viewer |
| `SensorEncoder` | Declared observation→neural drive transform | Reward inspector state not declared as input |
| `Readout` | Fixed rate/signal→control-channel mapping | Opponent strategy, game addresses or pathfinding |
| `ActionExecutor` | Selected action + coherent game state + task progress + clock → controller state | Authority to invent a winning action when brain is silent |
| `Environment` | Native clock, port schema, action commit, observations/media, snapshot capabilities | Neural roles, Twitch or task reward weights |
| `Task` | Typed state interpretation, rewards, progress and episode outcomes | Direct neural mutation or direct controller writes |
| `EpisodePolicy` | Start/end/reset/recovery semantics | Hidden per-player rewind in a shared world |
| `Session` | Barrier, identity, agent isolation, routing, state capture and supervision | Melee memory offsets or Pokémon map IDs |
| `Flybus` | RPC/pub-sub routing, bounded deliveries, artifact ownership and GC | Game timing, neural roles, presentation/stream semantics |
| `Presentation` | Descriptor-driven layout, media/audio and task panels | Emulator stepping or access to controller pipes |

Use modules first, then crates/packages as second consumers appear. The existing monorepo
and Rust workspace can stay in place during extraction. Keep static compiled registries
initially; a stable dynamic-plugin ABI and public package registry are not prerequisites.

**Framework acceptance test:** adding a synthetic third environment/task requires a backend
implementation, a task/profile and a composition manifest, not edits to the session loop,
neural core, protocol enums or generic stage store. A game-specific presentation plugin is
allowed. Wire extensions must be namespaced/schema-validated, not hardcoded into every panel.

### 5.3 Backend RPCs over the common bus

Use Flybus for these conceptual capabilities; do not implement another socket/message
envelope. Exact method names/bodies are in the session-framework contracts. Pause/Resume
are session lifecycle intents; an environment adapter holds its boundary between Advances.

```text
Hello → backend build/content/patch identity, capabilities, cadence, ports, views
Initialize(runConfig) → Ready(epoch, observationBoundary)
Advance(epoch, expectedBoundary, completePortBatch)
    → StepResult(epoch, newBoundary, appliedBatchId, observation, mediaRefs)
Pause / Resume → explicit acknowledgment
Capture(epoch, boundary) → capture token + state digest + snapshot bytes/reference
Restore(captureToken) → new epoch + restored observation + success/failure
Shutdown → acknowledgment or bounded forced process termination
```

Only advertise `Capture/Restore` if implemented and tested. Otherwise expose an explicit
`restart_episode` capability and visible aborted-match policy; never claim exact resume.
Port batches use canonical buttons plus sticks in [-1,1] and triggers in [0,1], converted
once by the backend. Descriptor/schema versions pin conversions and active ports.

Commands and pub/sub envelopes contain metadata and ArtifactRefs; native media/checkpoint
bytes live in Flybus's managed immutable store. Include producing epoch/frame/sample identity
and format in domain descriptors. DeliveryGuard keeps bytes alive beyond message drop if an
encoder/renderer retains a handle. Cached RPC replies own handles for retry; last-owner GC
reclaims data. Start file-backed; pooled shared-memory reuse is an optional later optimization.
Router restart invalidates routes/handles and requires coherent session recovery.

One request in flight, explicit timeouts, bounded queues. After an uncertain `Advance`
response, do **not** resend blindly: the world may already have advanced. Resolve batch ID/
boundary through an idempotent reply cache or fail/recover the session. A pipe transport with
no application acknowledgment needs validation against returned input telemetry, not a claim
of atomicity it cannot prove.

Separate lifecycle I/O from the blocked advance operation so diagnostics/shutdown stay alive.
Do not issue a save operation scheduled on Dolphin's CPU thread while that same thread is
waiting forever for pipe input. Capture/pause needs a backend-owned quiescent point where
both the simulation and pending input consumption have known state.

## 6. Multiple flies and the timing contract

### 6.1 One match, one world clock

Two flies in Melee normally means two controller ports in **one Dolphin instance**. Four
flies means four ports, not four copies of Melee joined through netplay. Several independent
matches are separate sessions/process trees, orchestrated by application code developed with
its presentation. A tournament director is one example, not a mandatory framework service.

For each committed boundary:

1. Freeze each agent's allowed observation from the same world state.
2. Advance every brain by the environment's elapsed emulated time using its private remainder.
3. Decode independent actions; step any declared executors; assemble a complete port batch.
4. Commit the batch once and let the backend advance to the next acknowledged boundary.
5. Associate rendered sensory frames and telemetry with their actual producing boundary.
6. Route task events/rewards exactly once; encode the next input and publish a snapshot.

Warm-up settles/calibrates brains with learning off while the environment is held at its
initial boundary. The next observation must not be from a game that ran freely through the
warm-up. Fixed offsets between brain and environment clocks are recorded.

Use the selected backend's rational emulated cadence, not `GAMEBOY_MS_PER_FRAME` or an
unexamined exact 60. An approximately 60-Hz budget is about 16.7 ms, but logical game frame,
video interrupt, input poll, rendered presentation and Slippi frame bookend are distinct
events until the spike establishes their mapping. Session ticks are monotonic even when
Melee's signed frame number resets, starts before zero, or changes across menu scenes.

### 6.2 Render latency and agent fairness

Dual-threaded graphics can present frame `n` after telemetry for `n` is available. Label the
actual frame; do not attach “latest screenshot” to current state and assume equivalence.
Start with one declared fixed observation latency shared by all agents, and measure it.
If a pipeline deliberately adds one frame of latency, record that in the sensor profile.
The full-resolution spectator view may be delayed separately, provided overlays use the
matching presentation timestamps rather than future task data.

Also test a potential pipeline deadlock: the helper waits for a rendered image while Dolphin
is waiting for the next controller flush needed to reach that presentation event. Fix the
backend's rendezvous or select a declared previous-frame sensory latency; do not unblock it
with an undisclosed neutral gameplay input. Game-state bookends alone do not prove the GPU
has completed a matching frame.

Do not reduce brain integration from 1,000 to 500 ticks per emulated second to meet wall-clock
deadlines. That changes the model. A declared action-repeat interval can reduce decisions,
but normally still requires all neural ticks and correctly accumulated intermediate task
events; it does not halve the principal neural cost. Rendering every second game frame is
also a sensor change if the brain otherwise sees each frame, not merely a broadcast setting.

On slow compute, the default is to slow the entire local match and report real-time factor.
Do not let one fly continue while the other misses turns. On a dead participant/backend,
pause or abort the match visibly; neutral fallback play is not silently substituted.

### 6.3 No rollback netplay in the first release

Slippi supports online play, but we do not need it to connect two local flies. Online rollback
would require rewinding **all** neural states, RNG, decoder/executor state, reward ledgers
and admission decisions at the same speculative boundary as the game, then replaying inputs.
Filtering repeated frames in libmelee is not that system. Keep offline local matches and
assert monotonic committed observations per epoch; classify unexpected rollback as an error
or explicit recovery transition rather than double-rewarding it.

## 7. Controller, sensory and learning design

### 7.1 A GameCube controller is not an eight-bit pad

Support independent main and C sticks, analog shoulders, digital trigger clicks, face
buttons, start and D-pad. Movement and attack can overlap. Canonical neutral/release state
must be complete, so a missing command cannot accidentally leave attack or shield held.

At about 60 Hz, the current 800-ms direction hold is roughly **48 game frames** and the
85-ms pulse roughly five. Reusing these values would dominate the fly's behavior regardless
of the dataset. Create a fixed Melee readout with explicit decisions in integer game frames,
bounded analog mappings, dead zones, tie handling and pulse/hold policies. Start with a small
declared set of stick magnitudes and directions if that makes validation easier; continuously
valued mappings can follow as a separate profile.

Audit tap-jump, directional aerials/smash attacks, jump release, shields, simultaneous axes,
and conflicting inputs. Don't add state-conditioned auto-aim, automatic edge recovery or
combo execution under the label “controller mapping.” If later desired, publish those as
separate macro/assistance profiles with their own identity and comparison baseline.

Start/system controls are lifecycle-sensitive. During an active match the profile may omit
pause entirely; initial match setup and between-match reset are disclosed episode scaffolding.
This is not permission for an API to press buttons. Specify whether setup uses an audited
initial state, internal deterministic menu setup, or a reset hook, and mark those frames as
non-neural setup with learning disabled. That expands the legacy “all presses” phrasing and
requires a deliberate task-policy/documentation decision before shipping it.

### 7.2 What the fly sees

For the initial pixel profile, both flies receive the same shared game camera with fixed
crop/aspect treatment, independent of spectator overlays. The legacy 160×144 input should
not stretch a 4:3 scene silently. Compare an aspect-preserving downsample/letterbox transform
with a separately versioned input-size profile; changing kernel retina dimensions currently
affects the numeric configuration identity.

Current L1 projection samples luminance at 1,572 columns. Higher broadcast resolution does
not produce more sensory neurons, color recognition, motion estimation or knowledge of which
fighter the fly controls. Test whether each selected character remains visible across zoom
and stage movement; record the sparse sensory representation rather than assuming a human-
readable video is an adequate neural input.

Three distinct modes must not be conflated:

| Mode | Neural observation | Rendering implications |
| --- | --- | --- |
| Pixel baseline | Actual game image through fixed encoder | Needs real rendered frames even if no desktop GUI is shown |
| Structured-state experiment | Explicitly encoded positions, velocities, stocks, etc. | Can potentially use Null/fast-forward, but it is a new privileged-input model |
| Spectator-only rendering | Whatever the profile specifies; video for audience | May be independently compressed/delayed, never silently substituted for sensory input |

“Headless” can mean no GUI while still rendering; “Null graphics” generally means no useful
pixel observation. The documented EXI fast-forward speed path cannot be advertised as the
performance of our pixel-fed broadcast.

### 7.3 Reward and outcome attribution

Implement a `MeleeTask` with typed per-player observations, match state, a ledger and positive
reward events. Its schema belongs to the task, not a generic `GameMode` enum. Start small:

- Terminal match outcome, once, based on validated results/termination reason.
- Opponent damage and credited KOs only after verified ownership information is available.
- No reward for mere button activation, for losing a stock, or for scripted setup.

Do not reward A for every increase in B's percent: self-damage, stage effects, reflected
projectiles, teams and another sub-fighter can invalidate that inference. The decomp's source-
player and KO tables guide inspection, but no runtime correctness is claimed until tested.
Unknown attribution produces a logged observation without a guessed reward. Keep fractional
damage until the rule deliberately quantizes; HUD damage and fighter damage may differ.

Deduplication keys include epoch/match, producing frame and event identity. Handle multihits,
trades, simultaneous KOs, respawn percent reset, timeout, sudden death and disconnection as
separate cases. If source telemetry cannot distinguish a required case, narrow the first
ruleset or add a specific audited observation hook.

Learning remains private per fly and synthetic reward modulation remains distinct from PAM
stimulation. Disable learning during kernel/controller/backend characterization; later compare
learning-on with learning-off, repeat seeds and swap sides/characters. Retaining gains between
rounds is a run policy. Competitive success is not guaranteed by increased model complexity.

## 8. Performance plan: measure the actual critical path

### 8.1 Budget equation

For a lockstep pixel-fed match, approximate the critical wall-time interval as:

```text
T_step = T_brains + T_readout/task + T_bus_RPC
       + T_emulation_to_observation + T_required_render_readback + T_boundary_overhead

T_brains ≈ sum(T_agent_i)                 [sequential evaluation]
T_brains ≳ max(T_agent_i) + barrier cost  [parallel with sufficient independent resources]
```

The parallel estimate is a lower bound, not a promise: shared caches, memory bandwidth,
GPU contention and scheduling can make every brain slower. Media publication/encoding and
storage should be off the critical path, but their resource use and state capture still
affect it. Do not obtain a “60 fps” claim solely from Dolphin's display counter while the
brains advance fewer milliseconds or repeat stale observations.

Proposed capacity gate: warm full-stack **unthrottled** throughput at least 1.2× the selected
game cadence for two flies, then a paced one-hour soak with no growing queues/lag and a
24-hour local endurance run before release. In the paced run, distinguish intentional wait
from compute time; report p50/p95/p99/max compute interval and deadline misses. The 1.2×
margin is a proposed engineering target, not a measured capability of current hardware.

### 8.2 CPU/GPU strategy

1. **Keep the first two brains on CPU.** Establish Dolphin JIT/render/media cost separately.
   The repository's current service is CPU-composed even though a CUDA kernel exists.
2. **Allocate a total physical-core budget.** Compare sequential agents with modest within-
   brain pools against agents running concurrently on disjoint core groups. Include Dolphin's
   CPU/JIT thread, graphics worker, helper, browser, encoder and storage in the budget. Do not
   launch four copies of the current per-brain pool size by default.
3. **Use native-resolution hardware rendering first.** Compare OpenGL/Vulkan on the chosen
   build/platform; no blanket claim that one is faster. Measure render correctness and readback.
   JIT is the performance baseline; an interpreter is a diagnostic baseline, not the live plan.
4. **Benchmark shader compilation and caches.** Report cold and warm starts separately. Choose
   supported shader modes from measurements; a cache that hides startup hitches is not a
   guarantee that a new stage/character will not compile something mid-match.
5. **Use NVENC where available, with a measured fallback policy.** Its encode engine does not
   remove GPU rendering, memory allocation, color conversion or framebuffer-readback cost.
   Automatic fallback to x264 can consume the cores the brains/emulator need; expose the
   resulting degradation and test whether the declared session can still meet cadence.
6. **Only then test CUDA brains.** The current backend retains RNG/plasticity observation/rate
   work on the host, uploads state inputs, and by default synchronizes membrane/refractory
   state back each batch. It also allocates device graph/state per backend instance. Measure
   two/four agents alongside Dolphin, browser graphics and NVENC; zero-copy shared graphs and
   a GPU-wide scheduler are possible later work, not present features.

For CUDA, preserve bit-exactness, gain-update ordering and checkpoint synchronization.
Batching across a future game action boundary is not valid just because it improves kernel
throughput. Keep the TypeScript oracle and existing version strings intact.

The existing VirtualGL/Xvfb result demonstrates one Chromium rendering path, not that
Dolphin Vulkan/OpenGL works or is performant in the same container. Test the complete selected
graphics path. Reusing GPU passthrough requires no assumption of exclusive VRAM availability.
Resource availability must be measured in an approved, serialized deployment-host session.

### 8.3 Media bandwidth and copies

Uncompressed RGBA estimates, before copies/framing:

| Image/cadence | Bytes per second |
| --- | ---: |
| Existing 160×144 at 30 fps | 2.76 MB/s |
| 640×480 at 30 fps | 36.86 MB/s |
| 640×480 at 60 fps | 73.73 MB/s |
| 1920×1080 at 60 fps | 497.66 MB/s |

640×480 is a planning example, not an asserted fixed Dolphin framebuffer size. The backend
advertises actual dimensions/format/aspect. One shared camera is delivered once for the
match; two flies can sample one immutable image without duplicating its transport. If their
sensor transforms differ, encode separately against the same source frame.

Start with native frames as immutable artifacts: 480p is not an automatic reason for a codec
or second transport. Flybus carries handles only; multiple readers map the same object. Measure
renderer readback, optional seal copy and consumer reads separately from router overhead.
Sensor downsampling remains a declared profile operation (optionally optimized near capture
after measurement). Viewer scaling/composition/encoding belongs to presentation. Last-use
handles, not receipt acknowledgments, govern GC; required sensory input cannot be coalesced.

### 8.4 Benchmark ladder and decision records

| Run | Configuration | Question / recorded output |
| --- | --- | --- |
| B0 | Synthetic two-port backend plus common Flybus, no neurons | Routed RPC latency, pub/sub fairness, step barrier, timeout and artifact-GC behavior |
| B1 | Dolphin with fixed input traces, rendering/audio on, no brains | Cold/warm emulator cost, step timing, port alignment, render-to-state latency |
| B2 | Same run + helper/media extraction | Incremental parsing, copying, downsampling and audio cost |
| B3 | One FAFB brain | End-to-end reference and per-phase costs |
| B4 | Two FAFB brains, sequential vs parallel schedules | CPU/cache/bandwidth limits, balanced observation and input timing |
| B5 | B4 + actual stage, capture, relay, recording, checkpoints | Full critical path, A/V drift, encoder fallback and queue growth |
| B6 | Four brains and four active ports | Capacity characterization only until this independently passes the same gates |
| B7 | Matched MaleCNS and optional CUDA variants | Dataset and backend effects, measured independently before combined variants |

Record backend/content/patch/profile digests, physical-core allocation, exact graphics settings,
sensor/broadcast resolutions, all clock rates, resident/peak memory, VRAM, thread usage,
real-time factor, latency distributions, audio under/overruns and dropped frames by purpose.
Store operator-specific machine details externally and publish only the portable methodology
and non-identifying results. No measurements were performed by this document-writing task.

## 9. Broadcast architecture and audio ownership

### 9.1 Two viable routes

Both routes are **application/presentation implementations**, not additional framework buses.
The environment emits native frame/audio artifacts through Flybus in either case.

**Route A — stage receives game media.** A presentation gateway subscribes to native artifacts,
delivers them to the browser, and the stage composites game/overlays for capture. Start with
native uncompressed artifacts internally. Browser-edge delivery can remain raw or use a codec
after measurement; avoid encode→decode→encode unless its tradeoff is justified. The gateway
holds artifacts through conversion/use and then releases them; browser backpressure cannot
pin required sensory state without a bound. Overlay timestamps track displayed video.

**Route B — compositor combines native game output and stage overlay.** Dolphin supplies its
rendered output to a compositor; the browser supplies a separate overlay surface. This can
avoid moving full-resolution game pixels through JavaScript, but requires an explicit shared
clock and a new capture composition. The fly's sensory image still needs a frame-identified
path from the backend. Capturing a desktop window on a wall clock is insufficient to establish
which image a brain used at a given game boundary.

**Recommendation:** prototype Route A for the two-player local slice; benchmark Route B in
the media spike before committing to the long-run high-resolution pipeline. Preserve media
as a capability behind the environment interface so the choice does not change brain/task code.
The browser-facing v2 contract can reference media streams; it does not dictate the internal
bus, native sensor format or artifact-store implementation.

### 9.2 Audio and clock policy

Today the page plays binjgb PCM and stream SFX into the Pulse sink. With Dolphin, select one
of these explicitly:

- Dolphin PCM is captured/forwarded and played by the page, with native device output muted.
- Dolphin renders audio to the capture sink directly, and the page contributes only SFX.

Do not run both. Declare sample format/rate, resampling location, timestamps, buffering and
discontinuity handling. Pause/reset/restore must flush or relabel buffered old-episode audio.
If wall time falls behind, measure pitch/time-stretch behavior; do not let “async resample”
hide minutes of simulation lag. Game timestamps, not arbitrary browser receipt time, define
the intended A/V relationship.

Keep 30-fps broadcast and approximately 60-Hz gameplay as independent settings. For 1080p60,
the existing H.264 level 4.1 is too low for the normal macroblock-rate requirement; use a
compatible level such as 4.2 or encoder-selected level and validate the actual stream. Also
measure capture/compositor cadence, bitrate quality, encoder lookahead/latency, local recording
and audio synchronization. `FLY_FPS=60` alone is not a completed performance upgrade.

### 9.3 Presentation changes

The generic stage needs descriptor-driven game aspect ratio, two/four agent cards, per-port
button/stick indicators, per-agent learning/sugar state, shared match stocks/percent/results,
and scoped events. Neither “badges” nor “highest ladder rung” describes a match.

Separate task data schema from layout. Keep one compositor clock and one selected world audio
stream; keep neural maps and rate scalers private per agent/dataset identity. Defer expensive
four-avatar/whole-connectome rendering until measured. All actual layout decisions require
PNG mockups and existing legibility/browser checks. This text does not approve a screen.

## 10. Persistence and unattended operation

### 10.1 Savestates are a capability, not a libmelee assumption

Dolphin source provides buffer/file state operations, but `State.h` documents that operations
called off its CPU thread may be scheduled rather than executed immediately. An external
“save requested” is therefore not proof of a consistent capture at our agent boundary.
Slippi's internal rollback save-state commands likewise do not constitute an audited public
multi-agent checkpoint API.

The selected backend must supply acknowledgment of the frozen boundary, resulting state
digest and completion. Save every brain, RNG, rate/calibration state, learning state, sensor/
executor state, pending action identity, task ledger and environment together. Clock and
media epochs change on restore; discard pre-restore spectator/parser buffers.

Re-create or explicitly reinitialize libmelee's parser caches and controller history after
restore. An emulator savestate does not include an external helper's `_frame`, previous
game state, normalization state or queued pipe data. Test how game-start metadata is supplied
when loading into mid-match; some telemetry protocols may need reseeding or restarting.

When exact mid-match capture is unavailable, an initial prototype may visibly abort and
restart a match while retaining a declared brain checkpoint. Mark `resume=episode-restart`
in the descriptor. That is a deliberate narrower capability, not equivalent to crash resume.
It is not ready for a release that promises uninterrupted exact match continuation.

### 10.2 Storage and health

Bound pending captures and coalesce replaceable hot checkpoints. A durable request either
completes with a commit acknowledgment or fails explicitly; never drop it while reporting
success. Match-end result records are append-only and independent of world rewind.

Keep backend/helper/brain health separate from “game frame did not advance.” An intentional
pause or waiting barrier is not a crash; a dead helper must not keep the session green by
merely refreshing an HTTP heartbeat. Export last completed boundary, in-flight request age,
barrier participant status and renderer progress. A hard stall has a timeout and explicit
match abort/recovery path, not repeated blind restarts of unrelated services.

Manage Dolphin under the session's lifecycle or an explicitly coordinated systemd unit. If
Dolphin restarts, the session cannot keep sending frame `t+1` to a fresh match. Allocate unique
user directories, pipe names, telemetry ports and state namespaces per independent session.
Use deterministic configuration provisioning and checksums instead of reusing a developer's
desktop Dolphin settings or permitting auto-updates.

The existing `flysim.service` memory ceiling and CPU partition were sized for a different
process graph. Set new cgroup/resource limits from measured high-water marks; account for
backend process, multiple brain copies and checkpoint transients. Release preflight checks
the complete backend/game/patch/parser/profile identity. Rollback retains compatible state
as well as the old executable.

## 11. Staged implementation and go/no-go gates

This specializes the existing backlog rather than replacing its foundation/session work.
Melee-specific spikes can start before the full framework reorganization is finished.

| Item | Work and dependency | Evidence required before the next step |
| --- | --- | --- |
| **MELEE-01: backend selection spike** | Specializes EMULATOR-01. Pin mainline Slippi + maintained libmelee, content and Gecko codes; use isolated user config and two synthetic controllers | Boot/render/audio; block one then both ports; one batch/frame mapping; menu→match→results lifecycle; cleanup/restart. Choose this build or stock Dolphin + narrow hook based on results |
| **MELEE-02: capture/restore spike** | Alongside MELEE-01; prove sensory-frame identity, media export and save/load acknowledgment independently | Fixed pixel↔telemetry latency, bounded media storage, correct input after restore, parser/cache recovery. Explicit decision: exact resume or prototype-only episode restart |
| **MELEE-03: task observation audit** | Pin decomp; build field catalog, typed parser/inspector, lifecycle and synthetic event fixtures | Verified port/player/sub-fighter mapping; stocks/results; no guessed rewards; content/patch mismatches visibly disable unsupported semantic interpretation |
| **FRAMEWORK-01: generic backend + session** | Existing FOUNDATION-01/02, BUS-01..03 and RUNTIME-01/02; all workers/application events use Flybus | Same legacy Game Boy traces; synthetic environment uses identical session API; artifact-backed requests/results and no Melee logic in router/core |
| **FRAMEWORK-02: multi-agent and state** | Existing RUNTIME-03/STATE-01; integrate complete action batches, worker budget and chosen backend recovery capability | No cross-agent state leakage; one world step; changed evaluation order invariant; failed restore cannot partly install a match |
| **MELEE-04: fixed readout and sensory profile** | MELEE-01/02 + framework boundary; TS specification then Rust implementation for any new decoder semantics | Neutral/release, analog conversion, tap/hold/direction combinations, aspect-preserved neural input and recorded latency; no hidden combo/aim policy |
| **MELEE-05: first two-fly match** | MELEE-03/04 + FRAMEWORK-02; learning off, then audited positive rewards | Recorded action/observation timelines; paired side/seed trials; match terminal deduplication and visible reset/failure semantics |
| **MEDIA-01: full local show** | Existing WIRE-01/PRESENTATION-01; compare Route A/B, define audio owner and 30/60-fps profiles | PNG review, fake multi-agent fixtures, measured copies/latency/A/V drift; sustained media pipeline under checkpoint and shader-load events |
| **PERF-01: two-fly capacity gate** | Benchmark ladder B0–B5; optimize measured limiting phase | ≥1.2× warm unthrottled capacity target, one-hour paced soak and 24-hour local endurance; no queue/lag growth; documented CPU/GPU/memory envelope |
| **MELEE-06: expand carefully** | Passing two-fly slice | Four ports/teams, broader characters/stages, MaleCNS and CUDA are separate experiments, each with new tests and its own capacity result |
| **FRAMEWORK-03: finish packaging** | Existing PACKAGE-01 after useful second backend | Example third backend can be added without core/session/schema edits; isolated compositions package and preflight correctly |

**Stop conditions:** no dependable step/input barrier; no identifiable pixel source for a
pixel-input claim; inability to attribute rewards under the claimed ruleset; unsupported
restore marketed as exact resume; or sustained capacity below the declared cadence.
Respond by changing the explicit supported scope, backend or resources—not by quietly skipping
neural ticks, adding a gameplay bot, hiding game stalls or reporting guessed measurements.

### 11.1 Suggested first experiment script

The first implementation should be a local measurement harness, not the final stream:

1. Launch one pinned backend with known local content and two configured bot pads.
2. Enter a fixed local match through the declared setup procedure; record episode boundary.
3. Send distinct short left/right and A/jump pulse patterns on each port, including neutral
   frames; log intended batches and observed raw/processed controller values.
4. Delay one port by a controlled wall-clock interval and verify no game boundary commits
   until the complete batch is available. Repeat with port order reversed and four ports.
5. Capture a sequence of images/telemetry with frame identities; measure their association.
6. Save/restore at a known barrier if supported, replay the same actions, and compare task/
   input traces; verify helper state and buffered media are reset coherently.
7. Kill the helper/backend separately and verify bounded failure without accidental continued
   play or permanent hangs. Test paused-state health independently.
8. Measure compute with no brains, one brain, two brains, then the full broadcast stack.

Synthetic controller traces are test machinery, not footage presented as neural play. Keep
game content and environment-specific records outside source control; store portable metrics,
synthetic schemas and independently authored tests in the repository.

### 11.2 Test matrix that catches Melee-specific failures

- **Input:** two/four ports, inactive slots, delayed/missing flush, stale buffered commands,
  full release, analog endpoints/deadzones, short taps, pressed versus held edges.
- **Identity:** controller↔player mapping, swapped ports, sub-fighters, transformations,
  character/stage changes, wrong game revision, changed patch/parser normalization.
- **Events:** multi-hit, trade, self-damage, projectile ownership, stock reset, simultaneous
  KO, timeout, sudden death, results re-entry, disconnect, restart after accepted reward.
- **Clocks/media:** game-frame reset, renderer lag, stale artifact/store identity, last-use GC, dropped
  spectator frame versus required sensory frame, paused audio, mismatched overlay timestamps.
- **Recovery:** all-agent atomic validation, one corrupt state chunk, backend import failure,
  helper parser not reinitialized, asynchronous save completion, hot-store coalescing and
  durable-write failure. Test new exact resume separately from legacy transient-reset behavior.
- **Performance:** cold/warm shaders, high-activity matches, checkpoint capture bursts, CPU
  encoder fallback, browser reconnect, two/four neural agents and measured GPU contention.

All implementation merges retain repository-required TS tests/typecheck, Rust workspace
tests and infra lint; UI changes add Playwright and PNG review. Game-backed jobs are explicit
operator-provided tests. Normal CI uses synthetic observations/backends and existing goldens.

## 12. Decisions to carry into implementation

| Question | Recommended answer now | Still requires evidence/choice |
| --- | --- | --- |
| Which emulator? | Dolphin, first trying mainline-based Slippi + maintained libmelee | Exact build selected by synchronized-input/media/state spikes |
| Use the decomp to run the game natively? | No; use it to audit task/state/controller semantics | Custom instrumentation only for specifically missing observations |
| One emulator per fly? | No for one match; one per independent session | Four-port capability must be tested, not inferred from two ports |
| Which brain? | Two existing FAFB agents for integration baseline | MaleCNS comparison after mappings/dynamics pass their independent gates |
| CPU or GPU brain? | CPU baseline, share immutable graph | CUDA versus CPU benchmark under Dolphin + capture, not in isolation |
| Inputs to the brain? | Pixels with explicit fixed transform | Structured state is a distinct optional research profile |
| Start with macros? | Fixed controller mapping, no hidden aim/combo policy | Any later assist profile is separately disclosed and evaluated |
| How fast? | Backend-native gameplay/input cadence, 30-fps initial show | Full-stack two-agent capacity; optional 60-fps broadcast and four flies |
| How to resume? | Whole-session coherent state where supported | Episode-restart prototype if exact state interface is not yet available |
| How generic? | Concrete environment/task/agent/session contracts and composition examples | Extract public packages only after second/third consumers prove the seam |

The first operator choices needed are the initial characters/stage/ruleset, desired show
cadence, and whether a visibly restarted match is acceptable during the prototype. They do
not block the synthetic framework work or source-level backend spike design.

## 13. Sources and audit scope

Local code evidence appears in section 4. Additional local files inspected include
`core/src/lif/cuda.rs`, `sim/src/pacing.rs`, `sim/src/simloop.rs::start_writer`,
`packages/brain/src/readout/presets/{gameboy,platformer}.ts`,
`apps/stage/src/audio/engine.ts`, `infra/bin/flycast-launch`,
`infra/units/flysim.service`, and the profiling/VirtualGL methods under `infra/docs/`.

External source snapshots inspected on 2026-09-18 (pin actual dependencies again at spike start):

| Repository/ref | Observed revision | Files used |
| --- | --- | --- |
| [doldecomp/melee](https://github.com/doldecomp/melee/tree/b9ec8a2eb48520753b2f8159ccc94d033fbf60ea) `master` | `b9ec8a2eb48520753b2f8159ccc94d033fbf60ea` | `.github/README.md`, `docs/symbols.md`, config, player/fighter/match headers and player implementation |
| [vladfi1/libmelee](https://github.com/vladfi1/libmelee/tree/bce21f09984b286e6d36bfd2939e4cd4691f94c2) `master` | `bce21f09984b286e6d36bfd2939e4cd4691f94c2` | README, `melee/console.py`, `melee/controller.py`, license metadata |
| [project-slippi/dolphin](https://github.com/project-slippi/dolphin/tree/41a7a3a110ed52999486ae1901c8fbb9a63d4f13) `slippi` | `41a7a3a110ed52999486ae1901c8fbb9a63d4f13` | Pipe backend, controller update loop, Slippi EXI events, `Core/State.h` |
| [dolphin-emu/dolphin](https://github.com/dolphin-emu/dolphin/tree/ee018d00e60b9eb727489908a8daec5c537f44a8) `master` | `ee018d00e60b9eb727489908a8daec5c537f44a8` | `Source/Core/Core/Core.h`, state/core module inventory |
| [Felk/dolphin](https://github.com/Felk/dolphin/tree/46b7eacd5c810c2d21ec5fe51ea1a9c61a7ceb3d) historical `scripting` branch | `46b7eacd5c810c2d21ec5fe51ea1a9c61a7ceb3d` | Scripting README, `python-stubs/dolphin/{event,savestate}.pyi` |
| [altf4/libmelee](https://github.com/altf4/libmelee/tree/1da979657122facd0750ea99cf6858255e198326) `main` | `1da979657122facd0750ea99cf6858255e198326` | Archive notice directing users to maintained fork |

Dolphin files inspected carry GPL-2.0-or-later headers; libmelee repository metadata reports
LGPL-3.0. Pin and retain actual dependency licenses/notices when packaging. A separate process
is an architectural boundary, not an assertion that distribution obligations disappear.

Source inspection supports the integration hypotheses and concrete constraints above. It
does not establish Dolphin throughput on the deployment hardware, verify any game-memory
field live, demonstrate a new neural behavior, or prove exact multi-port/frame/save semantics.
Those are the measured deliverables of MELEE-01/02 and the performance ladder.
