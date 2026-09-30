# Legacy Game Boy composition v1

Status: **contract**, 2026-09-23. It covers PROF-02a, the legacy half of the FOUNDATION-02 split,
and the Game Boy parts of RT-01a. The generic parts of RT-01a are dated amendments to
[worker interfaces](workers-v1.md), [step protocol](step-v1.md) and
[session media/state](state-media-v1.md), and they point back here. This document changes no
runtime behaviour: `flysim` is unchanged and so is its compatibility string, 648 bytes, sha256
`4929f3409b591ae21cf4a6d53e8e758b975c70f424eabb0b37db75c658b9ebd9`.

## 1. The decision and what it replaces

On 2026-09-23 the operator decided that the live fly gets a **full port** onto the session
framework. It is not kept outside the framework as a separately routed legacy loop. The
decisions this document implements are:

| Subject | Decision |
| --- | --- |
| FOUNDATION-02 | Split. The legacy profile ships now; MaleCNS bundles ship later (02b, section 17) |
| Profile | `gameboy-legacy-fafb-v783-v1`. It embeds today's schema-1 fingerprint, and the kernel `lif-1ms-f64-v2` and plasticity `fly-kc-mbon-rstdp-v2` are unchanged |
| Readout context | Location is allowed and declared: `gameboy-readout-context-v1 {boot, bound[], location\|null}` |
| Decision | `gameboy-channels-v1`: the eight buttons plus the macro group's winner |
| Decoder identity | The decoder and macro-channel configuration go in the composition digest, not the legacy compatibility string |
| Environment boundary | Macros run in the coordinator's ActionExecutor. It reads a 64 KiB memory image each boundary, carried as an artifact in `inspection`, plus the ROM as an AssetRef |
| Emulator shim | One read-only bulk memory read is added. The joypad stays the only write |
| Task and executor | One object, declared as the extension `executor: pokered-macros-v1` |
| Audio | The environment converts u8 samples to f32, and the edge applies the DC blocker |
| Initialize | `Environment.Initialize` runs one frame with no button pressed |
| Rollback | `legacy-ratchet-rollback-v1` plus the environment extension `gameboy-slots-v1` |
| Restore | `restore: legacy-transient-reset`, which clears the ledgers, the location and the held channel |
| Sugar | Admission reads `reward_remaining` from the last commit's telemetry. A lag of one commit is accepted |
| Checkpoint | FLYSIM01 stays the format of record until RETIRE-01 |

Several earlier statements said the legacy composition stays outside `lockstep-v1`:
[README](README.md) section 4, [implementation guide](implementation.md) ENV-01,
[state-media-v1](state-media-v1.md) section 7 and the
[modular-session analysis](../malecns-modular-sessions.md) section 5.2. Each now carries a
dated amendment that cites this decision. None was silently rewritten. The reason for the change is in
section 4: the legacy frame order already *is* the lockstep order, with one agent, one port
and one world. Moving it onto the framework therefore keeps every ordering fact those
statements protected.

## 2. The profile `gameboy-legacy-fafb-v783-v1`

Exactly one document exists. Every field of it is fixed, so both the document and its digest are constants of
this contract. Any other value is another profile and needs another id. The document is canonical
JSON (RFC 8785), its `AssetRef` is `{id: "gameboy-legacy-fafb-v783-v1", format:
"fly-profile-v1", digest, byteLength}`, and the digest and length are taken over the canonical
bytes. The current values are in `fixtures/gameboy-legacy.json`: digest `41e5d1ac…c60878`, length 1137.

| Field | Value | Why it is fixed |
| --- | --- | --- |
| `profileId` | `gameboy-legacy-fafb-v783-v1` | |
| `datasetId`, `fingerprintSchema` | `fafb-v783`, `1` | The schema-1 fingerprint is the seven SHA-256 digests joined with `:` |
| `datasetFingerprint` | Today's value, byte for byte the compatibility string's segment 2 | This is the "embeds today's schema-1 fingerprint" of the decision. `flysim`'s `legacy_profile_identity` test recomputes it from `data/fafb-v783` |
| `kernelVersion`, `plasticityVersion` | `lif-1ms-f64-v2`, `fly-kc-mbon-rstdp-v2` | Pinned defaults (CLAUDE.md). The same test compares them with the built network |
| `tickDuration` | `1000000/1` ns | One model tick |
| `warmupMs` | `2500` | Fresh-start warm-up with learning disabled, `DEFAULT_WARMUP_MS`. A service configured with another warm-up (`loop.warmup_ms`, `FLYSIM_LOOP_WARMUP_MS`) is **another profile** and needs its own id; the legacy composition refuses to start this profile with any other value. It matters only on a fresh start, because a restore never warms up, but it is identity all the same |
| `view` | `lcd`, 160 x 144 | The retina's native frame |
| `supportedStimuli` | `["reward-pulse"]` | Sugar and task reward events both drive `stimulate(durationMs)` |
| `readoutContextSchema`, `decisionSchema` | The registered references of sections 5 and 6 | |
| `legacyExceptions` | `["macro-roles-outside-fingerprint"]` | The `macro_*` roles are merged after the fingerprint is taken ([modular analysis](../malecns-modular-sessions.md) 2.2). This profile declares the gap instead of repairing it |

The profile does not name the decoder timings or the macro channels. Those belong to the
composition (section 12), because the legacy compatibility string never covered them and the
decision puts them in the composition digest.

## 3. Clock

`stepDuration` is one Game Boy frame: 70224 cycles of a 4194304 Hz clock, which is
**`8572265625/512` ns** exactly. The legacy loop accumulates the `f64` constant
`1000 / (4194304 / 70224)`. That constant is exactly `548625/32768` ms, because it is dyadic and
the division rounds to the true value. Every remainder of the loop's `remainder += ms_per_frame`
is therefore a multiple of 2^-15 ms below 32, which is exact in `f64`. As a result, the legacy accumulator and the rational
accumulator of [step-v1](step-v1.md) section 5 produce **identical** tick counts and remainders.
Both languages assert this over the first 100,000 (Rust) and 20,000 (TypeScript) frames, and
the fixture records the first twelve frames: 16, 17, 17, 16, and so on. No separate legacy
arithmetic is needed, and step-v1's rule that the clock must not accumulate rounded time holds unchanged. A
FLYSIM01 remainder (`f64` ms) converts exactly to a `RationalNs`.

The task's clock is the agent's brain time. `PreparedDecision.brainTicks` times 1 ms is the
legacy `network.ms`, and it counts warm-up. AGENT-01 must make `brainTicks` equal to that value,
because the ratchet windows and the reward adapter's timing read it.

## 4. Placement in `lockstep-v1`

The legacy `Sim::step_frame` order maps onto the transaction phases one to one:

| Legacy step | Lockstep phase |
| --- | --- |
| Drain commands (sugar) | Admission cut at `Ready(k)`. The sugar enters `Prepare.preStepStimulations` (section 15) |
| `network.step(ticks)` | Phase A: `Agent.Prepare` advances the ticks |
| `decode_bound(rates, ms, boot, blocked, bound)` | Phase A: readout with the context of section 5. The blocked rule stays inside the agent |
| `MacroLayer::decide` | Phase B: the `pokered-macros-v1` executor reads O[k]'s memory image (section 10) |
| `set_buttons`, `run_frame` | Phase B: one `Environment.Advance` with the complete joypad batch |
| Framebuffer, `take_audio_u8` | The environment returns O[k+1]: view, audio chunk, memory image |
| `adapter.sample` | Phase C: the task, which is the same object, evaluates old/new inspection once |
| `stimulate` per event, `reinforce(sum)` | Phase D: `Agent.Commit` installs the input, then the stimulations in event order, then one reinforcement |
| `MacroLayer::observe`, `location()` | Phase C: this produces the next context's `bound` and `location` |
| Ratchet observe, capture, recover | Phase C decides. `Environment.SaveSlot` and the rollback run at `Ready(k+1)` (section 11) |
| Milestone archive | A durable save at `Ready(k+1)`, exported as FLYSIM01, **after** that boundary's slot save (section 16) |

**Amended 2026-09-23, review round 1.** One order differs from the legacy loop and is declared.
Legacy `track_rank` archives the milestone *before* the ratchet captures, in the same frame, so a
legacy archive holds the pre-capture ratchet (`best` = the previous rung) with the previous
snapshot. Here the ratchet ledger commits `best = r` in Phase C, and the slot is only filled by
`Environment.SaveSlot` at `Ready(k+1)`. A capture ordered before that save would pair `best = r`
with the previous slot's contents -- or an empty slot on the first climb -- on every rank climb.
Section 16 therefore orders the save first, and a ported archive holds the **post-capture**
ratchet and slot. Both are internally consistent; they are not the same bytes.

The input installed at Commit and ticked at the next Prepare is the frame the legacy loop
hands `set_visual_frame` before it samples rewards. Rewards are sampled from the frame just
produced. This is the ordering that [modular analysis](../malecns-modular-sessions.md) 2.1 says must not
move, and it does not. FND-01's trace harness is where this mapping is proved against the running
loop.

## 5. Readout context `gameboy-readout-context-v1`

```ts
interface GameboyReadoutContext {
  boot: boolean;               // the adapter's boot gate after the last transition
  bound: ChannelName[];        // the executor's bound macro channels, composition order; [] in raw mode
  location: { area: number; x: number; y: number } | null;  // the adapter's location, or no information
}
```

`ChannelName` is `^[a-z][a-z0-9_]{0,63}$`. Decoder channel and rate-role names carry `_`, so
they are not `Id`s (corrected 2026-09-23, AGENT-01: they are; see the section 13 amendment).
The context is what the task hands the decoder with each Prepare (the
`initialDecisionContext`, then every `nextDecisionContext`). `bound` is an ordered subset of the
composition's `macroChannels`.

**Location is allowed and declared.** It is task inspection data, and it reaches exactly one
place: the readout's blocked-direction window ([readout](../../readout.md), "Blocked-direction
cooldown"), which restarts when the location changes. It never reaches the network, it
changes no score and it is not neural input. Declaring it here satisfies
[workers-v1](workers-v1.md) section 4: the context is typed, bounded, versioned and
allowlisted by the profile. The blocked direction itself is **not** in the context. The agent
computes it from its own held channel, its own clock, `blockedMs` and the location history.
The held channel, the start of the blocked window and the last location are private readout
state of the agent.

## 6. Decision `gameboy-channels-v1`

```ts
interface GameboyChannelsDecision {
  buttons: { id: "up"|"down"|"left"|"right"|"a"|"b"|"start"|"select"; down: boolean }[]; // all eight, this order
  macro: ChannelName | null;   // the macro-group channel active in this decode
}
```

The `buttons` array is the decoder's active set packed in `GAMEBOY_BUTTON_BITS` order, so bit *i* is
`buttons[i]`. `macro` is the macro group's channel in the active set, which is always one of the
context's `bound` channels. Together they are everything `MacroLayer::decide` reads: the raw
mask and the active macro. No port assignment or inspection field is in the decision.

## 7. Controller

One port, `ControllerSchema {schema: gameboy-joypad-v1, buttons: [up, down, left, right, a, b,
start, select], axes: []}`. The executor's `ControllerIntent` is the joypad mask, and it
becomes the port's `PortControl`. It is the only input the environment applies to a running game.

## 8. Inspection `gameboy-memory-inspection-v1`

```ts
interface GameboyMemoryInspection {
  memory: ArtifactRef;   // 65,536 bytes: $0000..=$FFFF as the CPU sees it at this boundary
  romDigest: Digest;     // == EnvironmentDescriptor.contentDigest == the executor's rom AssetRef digest
}
```

- **The image.** (Amended 2026-09-29, below: the register windows are not captured.) Byte *i*
  is what `fly_gb_read_mem(i)` returns at this boundary, which is
  what every task and executor read in the legacy loop sees through the per-frame read cache.
  It is a listed bus attachment, content type `application/octet-stream`. Its digest is optional,
  because it is a transient live artifact ([state-media-v1](state-media-v1.md) section 1). At
  59.73 frames per second it is about 3.9 MB/s.
- **The shim.** The emulator shim gains one function that fills a 65,536-byte buffer from
  `emulator_read_mem` in address order. It is read-only. The implementing slice proves this by
  exporting the emulator state before and after the call, comparing the bytes, and comparing
  the buffer with 65,536 single reads. Nothing else in the shim changes. `fly_gb_set_buttons` stays
  the only write, and there is no memory-write path. `read_uncached` is a probe tool and is
  not available to a task or executor.
- **The ROM.** Macros read ROM banks (`MemoryReader::read_rom(bank, address)`) that the image
  does not map. The executor gets the cartridge as a persistent `AssetRef` in the composition
  (section 12), and it is refused unless its digest is the environment's `contentDigest`. The
  image never carries ROM banks beyond the ones the CPU has mapped.
- **Retention.** The coordinator keeps O[k]'s image until transition k→k+1 has been evaluated.
  The executor reads it in Phase B, and the task reads old and new images in Phase C.

**Amendment, 2026-09-29 (MEM-01).** The image captures the address space's *memory*, not its
*registers*. The two proofs this section demands contradicted each other for three windows: a
read of VRAM (`$8000-$9FFF`), OAM (`$FE00-$FE9F`) or I/O with the APU and wave RAM
(`$FF00-$FF7F`) goes through binjgb's lazy catch-up (`ppu_synchronize`, `timer_synchronize`,
`serial_synchronize`, `intr_synchronize`, `apu_synchronize`) before it answers. The catch-up
rewrites the subsystem's sync bookkeeping, so the exported state changes. On a boot run it
changed on 104 to 105 of 105 sampled boundaries for each window. A full 65,536-byte read
through `emulator_read_mem` is therefore not read-only by the state-comparison test. The amended
rule:

- **Captured**: `$0000-$7FFF` (ROM as mapped), `$A000-$FDFF` (cartridge RAM, work RAM and its
  echo), `$FEA0-$FEFF` (the unused block) and `$FF80-$FFFF` (high RAM and `IE`). Byte *i* is
  exactly what `fly_gb_read_mem(i)` returns at this boundary. binjgb reads each of these
  straight out of an array, with no catch-up.
- **Not captured**: the three register windows above. They read `$FF` (binjgb's
  `INVALID_READ_BYTE`), and the shim does not call `emulator_read_mem` for them.
- **Unchanged**: the artifact is still 65,536 bytes in address order,
  `application/octet-stream`, with an optional digest. `gameboy-memory-inspection-v1` is
  therefore unchanged, and so are its schema digest and the contract digest.
- **Why no task or executor loses anything**: none of them reads a register window. The Pokémon
  adapter, scene detector and macro engine read WRAM, HRAM and ROM only. MEM-01's equivalence
  harness (flysim `tests/rom_memory_image.rs`) counts every address the image-backed arm reads
  over 27,000 frames from nine checkpoints and finds no read outside the captured ranges. A future
  task that needs a register must amend this section first. A register cannot be captured
  read-only through this shim.

The shim function is `fly_gb_read_memory_image(gb, out, 65536)`, and in Rust it is
`Emulator::read_memory_image{,_into}`. The executor's reader is `flybrain_gb::ImageReader`,
which pairs a `MemoryImage` with a `Cartridge`. `Cartridge::verified` refuses a ROM whose SHA-256
is not the environment's `contentDigest`. The emulator answers its own bank reads through the same
`Cartridge::read_bank`.

## 9. Environment

- **Initialize.** The backend configuration declares a setup scaffold of **one frame with no
  button down**, as the legacy fresh start runs. O[0] follows it, with `engineFrame` `"1"` and
  `worldTime` `0/1`. That frame's audio is not published: O[0] carries no chunk
  ([state-media-v1](state-media-v1.md) 2), and the audio origin is the first sample of
  transition 0→1.
- **Advance.** Apply the mask, run one frame, and return O[k+1]. The view is `lcd` 160 x 144 `rgba8`
  (row stride 640) with `observationDelaySteps` 0. `engineFrame` is the legacy frame counter as a
  decimal string.
- **Audio.** The environment converts binjgb's unsigned 8-bit interleaved stereo to f32 with
  binjgb's host rule, `sample / 255`. The result is unipolar in [0, 1] with silence at 0.0. The
  stream is `f32le-interleaved`, 2 channels, at the configured rate (48,000 by default). The
  environment does not filter. The **edge** applies the DC blocker (pole 0.995, per channel)
  before presentation. It is presentation state: it is reset only when the edge restarts, it
  is never in a checkpoint, and it never reaches an agent.
- **Descriptor.** `stepDuration` is `8572265625/512`, `inspectionSchema` is section 8, `recovery` is
  `exact-checkpoint`, and `determinism` is `fixed-build`.
- **Slots, `gameboy-slots-v1`.** This is an environment capability for `Environment.SaveSlot` and
  `Environment.RestoreSlot` ([workers-v1](workers-v1.md) section 7). A slot holds the emulator's
  exported state and the framebuffer that was on screen, as the ratchet's `Snapshot` does. Slot
  ids are declared by the composition (the legacy composition declares one, `best`). A save
  replaces the slot. A restore imports the state, releases the buttons and returns the
  archived framebuffer as a fresh view artifact, with a memory image read after the import.
  It runs no frame. Every slot is part of the environment's `State.Capture` payload, as
  FLYSIM01's `ratchet_game` and `ratchet_frame` are today. A slot save due at a boundary
  completes before any `State.Capture` or FLYSIM01 export at that boundary (section 16).

**Amendment, 2026-09-29 (ENV-01).** Two consequences of building this section, recorded in the
[implementation guide](implementation.md) ENV-01 entry: ~~the memory image cannot be taken with a
plain loop of `emulator_read_mem`, because reading OAM, VRAM, the serial and timer registers,
`IF`, `STAT` or `LY` advances binjgb's lazy synchronisation and changes what `export_state` writes
-- the environment reads it between an export and an import of the same bytes, so slots and
captures stay byte for byte the legacy loop's;~~ and a `FLYSIM01` world is restored at boundary
`emulatorFrame - 1`, its world time and audio position derived from that boundary, with the
ratchet's snapshot as the slot `best`.

*Struck 2026-09-29 (ENV-01 review, R1).* The first consequence is superseded by section 8's MEM-01
amendment of the same day: the register windows are not captured and read `$FF`, so the image is
MEM-01's one bulk read with no export/import guard around it, read-only by construction
(`legacy_env::memory_image`). With it the service's traces still reproduce every frame, WRAM and
slot-state digest (rollback 3,199, climb 3,156, r58 11,043 transitions), because the legacy
adapter and macros never read VRAM, OAM or I/O.

## 10. Executor `pokered-macros-v1`

The Pokémon Red task (reward adapter, ladder, ratchet ledger) and its action executor (the
macro layer) are **one object**. It implements both [workers-v1](workers-v1.md) section 4
interfaces and is declared as the extension `executor: pokered-macros-v1`. They cannot be
split without an undeclared channel between them, for three reasons. The executor's scene
observation produces the `bound` set the task hands the decoder. The macros read the adapter's
exploration and boundary ledgers. The macro layer's "nearer the objective" is a ratchet progress
signal. The object serves exactly one agent on one port.

- **Phase B** (`ActionExecutor.apply`): inputs are the decision (section 6), O[k]'s memory
  image, the ROM and the brain clock. The output is a joypad mask plus the macro start and finish
  events. In raw mode it passes the decision's mask through. While a macro runs, the macro owns the pad.
- **Phase C** (`Task.evaluate_transition`): the adapter samples O[k+1]. Each reward event
  becomes one `Reward` (its value) and one `Stimulus` of kind `reward-pulse` (its
  `stimulation_ms`), in event order. The macro layer observes O[k+1]. The location is read. The
  ratchet observes, and may request `SaveSlot` at `Ready(k+1)` or a rollback (section 11). Then
  the next contexts `{boot, bound, location}` are produced.
- **Capture.** The task ledger is the adapter state (FLYSIM01's `reward` chunk) and the
  ratchet state. The executor's ledgers (blocked, reached, talked, errand) and a running macro
  are session state. They are **not** captured (section 14).
- **Cancel.** `cancel(ms)` abandons a running macro. It is followed by `observe` on the world
  the fly now stands in.

**Amendment, 2026-09-29 (TASK-01).** Choices the section above leaves open, fixed as built by
`fly-legacy-session::task`:

- *The ratchet's slot.* The ratchet ledger stays in the task. Its snapshot lives in the environment
  as the slot `best`. The ratchet only asks whether one exists: its budget test is "no snapshot".
  So the task holds a marker snapshot in its place, and a task ledger records `slotFilled`. A slot
  save is asked for exactly when the legacy capture closure runs: safe, and rank above best.
- *The task ledger is artifact-backed* (amended 2026-09-29, TASK-01 review B1). The adapter's
  lifetime ledger grows with play: it is 42-46 KB on the live fly at rungs 12-15, which is past
  the 32 KiB `TypedValue` bound. So the typed ledger is `{reward: {digest, byteLength}, ratchet,
  slotFilled}`, and the adapter state's JSON travels as the ledger attachment `reward`. A
  checkpoint files it as the payload `task-ledger-reward` ([workers-v1](workers-v1.md) section 1's
  explicit artifact-backed schema; `Task::capture_attachments`). A missing, altered or unreadable
  attachment is `INCOMPATIBLE_STATE`, never a panic, so a boot falls to the next candidate.
- *The palette seed* (amended 2026-09-29, TASK-01 review N3). The legacy loop seeds the palette
  from the brain's RNG state after boot. A restore does the same: it seeds with the restored
  agent state's RNG, which it holds. A fresh start cannot see the agent's RNG after the warm-up,
  so it uses a constant. That is exact because `MacroMachine` never reads its generator: no macro
  has a random step. `fly-legacy-session` `tests/palette_seed.rs` holds this on the cartridge:
  two seeds, driven by the same decisions, press the same buttons for 3000 frames. A future
  macro that draws from the generator fails there.
- *The clock of the first observation.* A fresh start's layer observes O[0] at the end of the
  warm-up, `warmupMs` = 2500 ms, which the profile pins. That is the same time the legacy loop's
  `Sim::boot` observes at, although the agent is initialized after the task bootstraps. After a
  restore the layer observes at the restored brain time.
- *A reward event with `stimulation_ms` = 0* is one `Reward` and no `Stimulus`, because
  `Stimulus.durationMs` is positive. The legacy `stimulate(0)` changes nothing.
- *Executor events.* Macro starts and finishes are not `executionEvents`. They are reported as
  `FLY_TRACE` `macroEvents` (phases `execute`, `evaluate`, `rollback`). Reward events are the
  task's `TaskEvent`s.

## 11. Episode policy `legacy-ratchet-rollback-v1`

The ratchet's game-only rollback is a declared episode policy. When the ratchet fires during
transition k→k+1, the task returns `episodeRequest {kind: "rollback", reason, outcome}`. The
outcome is `legacy-ratchet-rollback-v1 {slotId, trigger: "stall" | "game-over"}`. The
transition's rewards commit first. The coordinator then applies the policy **at `Ready(k+1)`,
before the next Prepare, without pausing**:

1. If this boundary also has a slot save due, `Environment.SaveSlot` runs first.
2. It picks a new epoch e'. `Environment.RestoreSlot(scope e',k+1; slotId, priorEpoch e)` restores
   the slot and returns O'[k+1]. The boundary number stays the same. `worldTime` continues,
   `engineFrame` continues, the view is the slot's archived frame, and there is no audio chunk.
3. Coordinator-local: the task clears the adapter's transient reward observations. The
   executor runs `cancel`, then `observe(O'[k+1])`, which gives the next context.
4. `Agent.Rollback(scope e',k+1; priorEpoch e, input O'[k+1], context)` runs on every agent.
   It clears the decoder holds (`clearHolds(now)`: holds, winners, fatigue, lockout) and the
   plastic eligibility. It installs the slot frame as the next input, drops the held channel,
   restarts the blocked window at now, and takes the location from the context. It runs **no
   tick**, no reinforcement, no stimulation and no calibration.
5. With every reply in hand, the session is `Ready(e', k+1)`. Any failure fails the epoch and the
   group restores from the last durable checkpoint. No participant resets alone.
6. A durable save follows, as the legacy loop checkpoints after a recovery. Any capture at this
   boundary -- before or after the rollback -- is taken after step 1's slot save.

**Worker status and lost replies (amended 2026-09-23, review round 1).** While it executes
`Environment.SaveSlot` a worker's `Worker.Status` reports `capturing`; while it executes
`Environment.RestoreSlot` or `Agent.Rollback` it reports `restoring`, with `currentScope` still
the prior `(e, k+1)`. After the reply it reports `ready` at `(e', k+1)`. These are mutations
under the [session RPC](ipc-v1.md) section 5 operation key `(session, e', k+1, method, worker)`
(`SaveSlot`: `(session, e, k+1, …)`), so a lost reply goes through ipc-v1 section 6 first: stop
dispatch, query `Worker.Status` or retransmit the same request id and body to the same
incarnation, and resolve only the matching cached result. Only an unresolvable outcome -- a
changed incarnation, lost routes or ownership, `RESULT_EXPIRED` -- fails the epoch, and then the
group restores from the last durable checkpoint. No participant is ever left in `e'` while
another continues in `e`: the coordinator issues no Prepare until every rollback reply is
resolved.

The following continue through the rollback: brain clock, membrane, RNG, learned gains, rates and
reward history, the ratchet ledger (its attempt and lifetime budgets were spent in Phase C), and
the adapter's persistent state. The first audio chunk after the rollback marks a
discontinuity. This is the legacy `recover_game` sequence with the neural half moved into the agent.
Each half touches only its own state, so the order between the halves does not matter.

**Amendment, 2026-09-29 (TASK-01).** The coordinator applies the policy itself, as the section
describes. The phase is `RollingBack(k)`. The new epoch is the session's epoch root plus `.rb<n>`,
which is deterministic, so two runs of one session name the same epochs. A composition that
declares the policy refuses to start unless every agent advertises `legacy-ratchet-rollback-v1`.
The task's rollback request carries `{slotId: best, trigger}`. The world must advertise
`gameboy-slots-v1` before anything is saved or restored. Service traces with a ratchet slot save
and a ratchet rollback reproduce as described in the implementation guide's TASK-01 entry.

## 12. Composition declaration and digest

```ts
interface LegacyGameboyComposition {
  compositionId: Id;
  scheduler: "lockstep-v1";
  profile: AssetRef;                       // the section 2 document
  executor: { id: "pokered-macros-v1"; rom: AssetRef; adapter: Id; symbolProvenance: string;
              mode: "raw" | "macros"; macroChannels: ChannelName[] };  // [] iff raw
  decoderConfigDigest: Digest;             // SHA-256 of the gameboy-decoder-config-v1 form
  environment: { extensions: ["gameboy-slots-v1"]; slots: Id[]; stepDuration: RationalNs;
                 inspectionSchema: SchemaRef; controllerSchema: SchemaRef; setupFrames: 1;
                 audio: { sampleRate: number; channels: 2 } };
  episodePolicy: "legacy-ratchet-rollback-v1";
  restore: "legacy-transient-reset";
  checkpointFormatOfRecord: "FLYSIM01";
  flysimCompatibility: string;             // the FLYSIM01 string, recorded, not reinterpreted
}
```

- `decoderConfigDigest` is the SHA-256 of the canonical JSON of the form
  `gameboy-decoder-config-v1` of the effective decoder configuration (amended 2026-09-23,
  review round 1): `{form, exclusive, macros, pulses, clearLockoutMs}`, each group
  `{channels: [{channel, role}], decisionMs, holdMs, hysteresis, fatigueGain, fatigueDecay,
  blockedFatigue, blockedMs}` or `null`, each pulse `{channel, role, holdMs, cooldownMs,
  threshold, boot: {cooldownMs, threshold} | null, throttleGroup: string | null}`. Channels are
  an array because their order breaks argmax ties and canonical JSON sorts object keys. The
  shared vectors are `fixtures/gameboy-decoder-config.json` (raw mode and the 31-channel
  Pokémon Red group): `flysim`'s `legacy_profile_identity` test computes them from
  `gameboy_decoder_config_with_macros` (and rewrites them under `FLY_UPDATE_FIXTURES=1`), and
  `@flybrain/session-types` reproduces them from the oracle's `gameboyDecoderConfig`. The
  example composition carries the real macros-mode digest. A change to a decoder
  timing, a threshold or the macro channel set therefore changes the composition digest.
  None of those changes touches the legacy compatibility string, which never covered them.
- The declaration's digest is the SHA-256 of its canonical JSON. The coordinator's
  `compositionDigest` recipe (`fly-session/composition-v1`: session, epoch, contract, one line
  per agent) gains one final line, `declaration=<digest>`, for a composition that has a
  declaration. The synthetic composition has none and gains no line.
- `flysimCompatibility` must agree with the declaration in its kernel, adapter, fingerprint,
  plasticity and `pokered:` segments, so the two cannot describe two different flies. Until RETIRE-01
  the **restore gate** is still that string and the legacy decision rules
  (`flybrain_gb::compatibility::decide`, `FLY_ACCEPT_ADAPTERS`). The composition digest is the identity
  for publication, traces and descriptor revisions, not a restore gate. This matches today's
  behaviour, where a decoder change does not refuse a checkpoint.

**Amendment, 2026-09-30 (PERF-01): the in-process release configuration.** Nothing in the
declaration, the digest or the compatibility string changes; these are execution choices of the
composition as `fly-legacy-session` builds it, recorded here because the shadow compares the
behaviour they must not change.

- *In-process participants are called over the local lane* ([session RPCs](ipc-v1.md) section 1,
  2026-09-30): the frame, the memory image, the audio chunk and the spike bitset are in-memory
  artifacts, handed over without a store file. Process mode is unchanged and still goes through
  the router. `FLY_SESSION_LOCAL_LANE=0` puts an in-process session back on the bus.
- *`telemetry.spikes` is unchanged*: one bitset per transition, bit `i` set when neuron `i`
  spiked in `[transition start, brain time)`, the legacy layout. The agent now builds it as the
  union of the kernel's per-tick spike lists while Prepare ticks (`LifNetwork::tick_spikes`)
  instead of scanning 139,255 last-spike times in Commit; the two are equal by construction and
  by test, and the parity runs compare the bitset every frame. The AGENT-01 alternative -- attach
  it only at the feed rate -- was not needed and was not made.
- *Committed snapshots on the session topic*: a service session publishes every 60th boundary and
  every boundary with task events or boundary actions (it was every other boundary). The live
  presentation is the legacy feed, which the service host builds from the committed boundary at
  30 Hz; nothing in the release subscribes to the session topic, and each publication seals a
  store copy of the frame and the audio chunk. Every publication is still of a committed
  boundary and still attaches the media it names (publishing-v1 section 3).
- *Nothing runs beside the brain's ticks.* On the release CPU with the sweep owning every core of
  the flysim cpuset, work overlapping Prepare (a publication left in flight, for instance) slowed
  the ticks by up to a third, so the coordinator finishes each boundary before the next Prepare.

## 13. Machine-readable parts

| Where | What |
| --- | --- |
| `fly-session-types/src/gameboy.rs`, `packages/session-types/src/gameboy.ts` | The five registered payload schemas, the profile, the composition declaration and their readers and cross-checks |
| `fly-session-types/src/extensions.rs`, `packages/session-types/src/extensions.ts` | `SaveSlot*`, `RestoreSlot*` and `AgentRollback*` payloads, which are in the session schema set |
| `fixtures/gameboy-legacy.json` (derived) | The extension set and its digest, every `SchemaRef`, the profile document with its canonical bytes and `AssetRef`, the frame clock, and an example composition with its digest and the digest recipe |
| `fixtures/gameboy-decoder-config.json` | The `decoderConfigDigest` vectors (raw and Pokémon Red macros), written and checked by `flysim`'s `legacy_profile_identity` test, reproduced by the TypeScript oracle |
| `TraceBehaviour.boundaryActions`, `TraceOperational.captures` | The section 16 order rule, refused by `TransitionTrace` validation in both languages |
| `fixtures/valid.json`, `invalid.json` | Accepted and refused cases for every new type, held to both languages |
| `flysim/tests/legacy_profile_identity.rs` | Recomputes the fingerprint, versions, frame size, warm-up, clock and button order from the committed dataset and the service defaults |

A payload schema's `SchemaRef.digest` is the SHA-256 of its canonical declaration
`{registry, id, version, source, fields}`. The legacy schemas are digested by their own
extension set, not by `contractDigest`: the session contract stays free of console state.
The generic changes of RT-01a (`stimulusRemainingMs`, `EpisodeRequestKind.rollback`, the six
extension payloads, `maxSlots`) *are* in the session schema set, and they moved
`contractDigest` to the value in `fixtures/contract-digest.json`. Regenerate with
`cargo run -p fly-session-types --example update_fixtures`. `tests/schema_set.rs` refuses
stale files.

**Open for AGENT-01:** the legacy rate roles (`command_0`, `macro_go_item`, and so on) are not
valid `Id`s, while `AgentGraph.rateRoles` and `AgentTelemetry.rates[].roleId` are `Id`s. The
mapping belongs to the agent adapter. This contract does not choose it.

**Amendment, 2026-09-23 (AGENT-01): the rate-role mapping `legacy-rate-role-id-v1`.** The
premise above is wrong. The session `Id` grammar is `^[a-z0-9][a-z0-9._-]{0,63}$`
([session RPC](ipc-v1.md) section 1, `flybus::wire::is_id`, `isId` in `@flybrain/session-types`),
and it admits `_`. Every role `data/fafb-v783` declares, including the 31 `macro_*` populations
merged after the fingerprint, and every role the kernel tracks, is already an `Id`; so is every
`ChannelName`, whose grammar is a subset. The mapping is therefore the identity:

- a legacy rate-role name `n` is published as `roleId = n` when `n` is an `Id`, and a `roleId`
  names the rate role of the same spelling. The mapping is reversible by construction;
- a name that is not an `Id` has no `roleId`. The agent refuses to initialize over a dataset
  that tracks one (`INCOMPATIBLE_STATE`), rather than invent an encoding no consumer could read
  back. No shipped dataset has one;
- the published list keeps the kernel's tracked-role order and its bounds: at most 64
  (`MAX_RATE_ROLES`, the kernel's role bitmask) and unique. On `fafb-v783` it is 45 roles.

`gameboy::rate_role_id`, `rate_role_name` and `rate_role_ids` in `fly-session-types`, and
`rateRoleId`, `rateRoleName` and `rateRoleIds` in `@flybrain/session-types`, implement it.
`fixtures/gameboy-rate-roles.json` holds both languages to the same accepted and refused names and
records the FAFB tracked list, which `fly-session`'s gated FAFB test recomputes from the
committed dataset. The mapping changes no schema, digest or fixture digest.

**Amendment, 2026-09-23 (AGENT-01): what the legacy agent reports and captures.** These are
agent-adapter choices that the sections above leave open. `LegacyAgentWorker`
(`fly-session/src/legacy_agent.rs`) implements them:

- *Seed.* The kernel version `lif-1ms-f64-v2` hashes the LIF seed. Every seed except the legacy
  default `22222` gives a different kernel version, so the profile pins the seed.
  `Agent.Initialize` refuses any other seed (`INCOMPATIBLE_STATE`) before it builds a model. The
  coordinator of this composition passes `22222`. It does not use a seed derived under
  [seed-derivation-v1](seed-derivation-v1.md).
- *Learning telemetry.* `AgentTelemetry.learning` requires `changed <= updates`. The legacy
  counters mean something else, so they map as follows. `updates` is the number of reinforcement
  calls the agent has applied: one per commit while the rule is enabled, zero sums included.
  `changed` is the legacy `plasticity.updates`: the reinforcements that moved at least one gain.
  `signal` is `plasticity.signal`. The legacy per-synapse count (`LearningStats.changed`, the
  gains away from 1.0) is not carried. It can still be read from a captured state.
  `stimulusRemainingMs` is the network's `reward_remaining` after the operation.
  *Amended 2026-09-29 (AGENT-01 rebase):* `FLYSIM01` records `plasticity.updates` but not the
  reinforcement calls. An agent imported from a `FLYSIM01` agent state therefore starts
  `updates` at that state's `plasticity.updates`, which is a lower bound on the calls made. Starting
  it at zero makes the first telemetry violate `changed <= updates`: on the live fly `changed` is
  in the thousands. `legacy_parity::legacy_reinforcements` is the rule. The import that ENV-01
  or STATE-01 ships must apply it.
- *Spikes.* Each `Agent.Commit` reply carries the attachment `telemetry.spikes`, with content type
  `application/x-fly-spike-bitset`. It uses the legacy feed's layout: bit *i* is neuron *i*,
  `ceil(neurons/8)` bytes. It covers the transition's ticks: a neuron is set when its last spike
  is at or after the brain time before that transition's Prepare ticked. The mapping of bit
  *i* is the graph's `indexDigest`.
- *Graph.* `datasetDigest` is the SHA-256 of the schema-1 fingerprint string. `indexDigest` is the
  SHA-256 of `fly-session/legacy-agent-index-v1`, the fingerprint, the neuron count and every
  `macro_*` population's members. The macro populations are the profile's declared
  `macro-roles-outside-fingerprint` exception. Two datasets that relabel them differently
  therefore get the same fingerprint and different index digests.
- *Initialize.* The readout transient starts as it does in a fresh legacy process: no held
  channel, no last location, and a blocked window from 0 ms. `initialDecisionContext.location`
  is **not** taken as the last location. The legacy loop reads a location only at the end of a
  frame, so the window first restarts on it at the first commit, as it does in the legacy loop.
- *Capture.* The `State.Capture` payload is a `flybrain-core` checkpoint envelope with magic
  `FLYAGT01`. Its manifest and chunks are exactly `agent_to_chunks`, with the frame remainder
  taken from the session accumulator, as `Sim::checkpoint` does. It adds one `session` member: the
  accumulator, the context, the profile, the seed, the reinforcement count, the macro channels
  and the decoder configuration digest. It does not carry the readout transient (section 14). It
  is not FLYSIM01. Writing a FLYSIM01 export from it and from the environment's state is still
  STATE-01/ENV-01 work (section 16).
- *Restore.* `State.StageRestore` builds a replacement network over the same dataset, imports
  the state and refuses any mismatch between the state's clock and the accumulator.
  `ActivateRestore` then installs the state and resets the readout transient, as a fresh legacy
  process does. The visual drive is the restored `visualDrive` chunk. Legacy `try_restore`
  instead re-projects the saved framebuffer, and the two are the same values (the parity
  harness compares the full state after a restore).
- *Decision.* `macro` is the first channel in the context's `bound` order that the decode holds.
  This is the channel the legacy macro layer's `asked` would start.

## 14. Restore `legacy-transient-reset`

The legacy composition declares that a restore (a FLYSIM01 load today, and any group restore
under this composition) is **not** an exact replay. The restored parts are those FLYSIM01
carries: agent state, emulator, framebuffer, adapter state, ratchet state and slots, frame
counter, remainder, buttons and event watermark. The rest starts cleared, as it does in a fresh
process:

- the executor's ledgers (blocked, reached, talked, errand) are empty, with no macro running;
- the agent's readout transient starts exactly as a fresh legacy process has it (amended
  2026-09-23, review round 1): no held channel, no last location, and the blocked window
  starting at brain time **0 ms**, not at the restored clock. The decoder state itself
  (`DecoderState`: holds, winners, fatigue) *is* restored. The consequence AGENT-01 must
  reproduce: on the first decode after a restore, `now - 0 >= blockedMs`, so the restored
  direction winner, if any, is passed as `blocked` and its fatigue is raised to
  `blockedFatigue`. Only after that decode does the window restart, because the held channel
  changed from none to the winner, and again when the first location is observed. A rollback
  (section 11) differs: it clears the holds and winners and restarts the window at the
  current brain time;
- the adapter's transient observations are cleared, as they are on a rollback.

Resume tests compare against the legacy restore outcome, not against an uninterrupted trace
([state-media-v1](state-media-v1.md) section 4 amendment). This is the property the operator's
unstick procedure depends on: restarting the service clears the ledger-shaped traps and keeps
the rung.

**Amendment, 2026-09-29 (TASK-01): a FLYSIM01 checkpoint as the start.** A new-runtime session
starts from the live fly's own save through the ordinary group restore
(`Coordinator::import`, `fly-legacy-session::import`):

- The world is ENV-01's `WorldState::from_flysim01` at `k = emulatorFrame - 1`, staged on a
  replacement world.
- The agent is the checkpoint's agent state in the worker's own capture format: STATE-02's
  `legacy_checkpoint::agent_payload`. It carries the file's reinforcement count and the file's
  framebuffer as the next input, and its ids come from the file.
- The task is STATE-02's ledger `{reward, ratchet, slotFilled}`.

A session boots from the live stores the way `Sim::boot` does: the legacy candidate order, the
legacy gate, the next candidate on any refusal, then a startup durable save. See the
implementation guide's TASK-01 entry.

This section's resets then apply, and nothing else. The executor is fresh and observes the restored
image once, at the restored brain time. The agent's readout transient is a fresh process's. The
decision context is computed from the checkpoint's memory image before the install, and the
coordinator refuses the install if the task, re-observing the restored world, derives another
context.

## 15. Sugar admission

The coordinator owns admission, with the legacy rules: the per-minute limiter, and "no overlap
with an active pulse". The pulse is read from `AgentTelemetry.stimulusRemainingMs` of the **last
completed commit** (an `Agent.Rollback` reply counts as one). The value can therefore be one
commit old, and the operator accepted this lag. The consequences, each bounded to one frame:

- a pulse that ended inside the in-flight transition still reads as running, so the request is
  refused and retried;
- a reward pulse added by the in-flight transition is not yet visible, so a sugar can be
  admitted over it where the legacy loop would have refused;
- after a restore, and until the first commit of the new epoch, the pulse is unknown and every
  request is refused with a retry, where the legacy loop admits against the restored pulse at
  once (amended 2026-09-23, review round 1).

The duration is clamped to `[1, sugar_max_ms]`. An admitted sugar is a `reward-pulse`
`Stimulus` in the next Prepare's `preStepStimulations`, which is the position of the legacy
drain at the top of a frame. Legacy admission *is* application. Here, the epoch can fail
between the two, so each admission record carries its interaction id. If the Prepare that
applies it never commits, the admission is reported aborted and the edge refunds it. The
bridge's fulfil path and refund path both get tests in the slice that wires them
([workers-v1](workers-v1.md) section 5 amendment).

**Amendment, 2026-09-29 (TASK-01).** As built:

- *Admission.* The coordinator's `AdmissionQueue` is cut at the top of each Prepare. It reports
  every admission `applied` at the boundary its Prepare committed at, or `aborted` when the epoch
  fails first. The legacy rules run on the edge side, unchanged: flysim's `RateLimiter` and the
  clamp. They read `stimulusRemainingMs` from the last `Agent.Commit` or `Agent.Rollback` reply.
  After a restore the pulse is unknown and a request is refused with a retry of one frame.
  *Amended 2026-09-29 (TASK-01 review N1):* a sugar admitted but not yet reported on by a commit
  counts as an active pulse, at its full duration (`AdmissionQueue::pending_stimulus_ms`). The
  legacy `stimulate` activates the pulse the moment it admits, so this matches:
  - two sugars between two commits get one admission and one `PulseActive`, as the legacy drain
    gives them;
  - the rate-limit slot is not spent on the refused one.

  The retry advice may differ by less than a frame. Legacy reports the pulse decayed by the
  ticks between drains; here it is undecayed until the commit reports it.
- *The operator's `POST /reward` pulse is refused (declared).* In the legacy loop it is off unless
  an operator sets `control.allow_reward`. Every shipped configuration leaves it off, and the API
  answers 403. The session runtime has no counterpart to turn on, because a reinforcement outside
  `Agent.Commit` is not a session-framework operation. `LegacyAdmission::reward` therefore always
  refuses. A legacy trace that carries a reward pulse cannot be reproduced; FND-01's checker
  refuses one as well.

## 16. Checkpoint format of record

**Order at a boundary (amended 2026-09-23, review round 1).** A slot save due at `Ready(k)`
completes -- its `Environment.SaveSlot` reply in hand -- before any `State.Capture` or FLYSIM01
export at `Ready(k)`, whether that export is periodic, a milestone archive or the post-rollback
save. So a checkpoint whose ratchet ledger names `best = r` always carries the slot saved for
rung `r`. This is a **declared difference** from the legacy loop, which archives a milestone
before capturing the ratchet snapshot. A legacy milestone archive holds `best = r-1` and the
rung `r-1` snapshot; a ported one holds `best = r` and the rung `r` snapshot. In practice the
two converge: after `fly-reset-to-milestone` onto a legacy archive, the ratchet captures rung
`r` again on the first safe frame (the rung is above the recorded best) and resets the attempt
counter. The difference is only where the rung `r` slot sits -- on the exact frame of the climb
(ported) or on the first safe frame after the reset (legacy) -- and it is visible only if the
fly stalls or hits game over before any safe frame, when legacy falls back to rung `r-1`'s save
and the ported loop to rung `r`'s (amended 2026-09-23, review round 2). The operator's confirmation of this difference is requested with the
CUT-01 shadow run. The rule is machine-checked in the step trace: `TraceBehaviour.boundaryActions`
records the saves and the rollback in order, `TraceOperational.captures` records each capture
with the number of boundary actions before it, and a `TransitionTrace` in which a capture
precedes a slot save is refused ([step-v1](step-v1.md) section 8 amendment; fixtures in
`valid.json` and `invalid.json`).

FLYSIM01 stays the format of record until RETIRE-01. Every durable save of this composition
(periodic, milestone archive, after a rollback) **exports a FLYSIM01 envelope** that the
current `flysim` reads, under the unchanged compatibility string. The deploy gate
(`--print-compatibility`) and `fly-reset-to-milestone` keep working on those files. A FLYSESS1
checkpoint may be written beside it, but it is not what a restore selects until RETIRE-01
says so.

**Amendment, 2026-09-29 (STATE-02).** Six readings of this section, fixed by building it
([implementation guide](implementation.md) STATE-02):

- *"A FLYSIM01 envelope that the current `flysim` reads" means the same bytes.* The session
  runtime writes through the legacy loop's own encoder and store, which now live in the crate
  `flysim-store` that both link. For the same state it writes the same bytes, and it keeps the
  same generations, `manifest.json`, `milestone-<N>` archives, rotation and restore order. The
  owners of the fields are: the agent (the seven agent chunks, the remainder), the environment
  (`emulator`, `framebuffer`, `emulatorFrame`, `buttons`, `romHash`, and slot `best` as
  `ratchetGame` and `ratchetFrame`), the task (`reward`, `ratchet`) and the host (`generation`,
  `wallMs`, `compatibility`, `speed`, `rankSinceMs`, `lastEventId`); the agent's reinforcement
  count, `reinforcements`, is new (below).
- *The milestone archive is per process, as in the legacy loop.* A rank climb is archived when
  the rank is above every rank *this process* has archived. After a restart, the first climb
  rewrites `milestone-<rank>` even if an older process had archived that rank. That is the
  legacy behaviour, and the reset tools rely on the newest archive of a rung.
- *A restore is the legacy restore, candidate for candidate.* The candidate order is hot latest,
  hot previous, durable latest, durable previous, then the archives by descending rank. The gate
  is the cartridge and the compatibility decision. A refusal at any step, including a
  participant's `State.StageRestore`, moves to the next candidate. If every candidate fails, the
  runtime does not start. The world applies the recorded `buttons` on restore, as
  `LegacyFrame::restore` does, so a file whose emulator state disagrees with its own `buttons`
  restores to the same state in both runtimes, not to the file's bytes.
- *The visual drive after a `FLYSIM01` import is the framebuffer's projection* (a correction to
  section 13's *Restore*, "the two are the same values"). They are the same on stream
  checkpoints. On the row 65 yard survey checkpoint they are not: the first transition's rates
  and spikes differed from the legacy loop's. The import therefore carries the framebuffer in
  the agent payload (chunk `inputFrame`), and `State.StageRestore` installs it after the import,
  as `LegacyFrame::restore` does. A worker's own capture carries no such chunk and restores
  exactly.
- *`learning.updates` is carried in `FLYSIM01`.* This corrects section 13's *Learning telemetry*
  amendment, which started every import at `plasticity.updates` and so dropped the count on
  every round trip. Both runtimes count reinforcement calls the same way and write the count
  as the optional manifest member `reinforcements`. A file without it (every file from before
  2026-09-29) starts at `plasticity.updates` once.
- *The sugar journal is the shadow run's input record.* It gets a per-process boot header, is
  rotated at 4 MiB with three kept files, and `fly-reset-to-milestone` clears it
  (`flysim-store::journal`). It is not a checkpoint and nothing restores from it.

## 17. PROF-02b: MaleCNS bundles (later)

Dataset manifests, original-ID mapping, anatomical roles, sensory and readout bindings, strict
graph validation for new bundles, and the composite behaviour identity of FOUNDATION-02 are
**not** in this document. They ship with PROF-02b, before DATA-01, as their own contract.
Nothing here constrains them, except that a new profile never reuses this profile's id or its
legacy exception.

## 18. The shadow run (SHADOW-01)

**Amendment, 2026-09-29 (SHADOW-01).** The operator's decision of 2026-09-23 gates the automatic
cutover (CUT-01) on "a 3-hour shadow run with zero divergence". This section fixes what that
means. The implementation is `fly-legacy-session::shadow` and the binary `fly-shadow`, with the unit
`infra/units/flyshadow.service` and the script `infra/bin/fly-shadow-run`.

**The inputs.** The legacy loop is deterministic, given two things: the state a process starts
from and the admissions applied at the top of each frame. Every nondeterministic input reaches
the fly through one of those two:

| Input | How it reaches the fly | How the shadow gets it |
| --- | --- | --- |
| Restart, watchdog kill, `fly-loop-recover`, `fly-reset-to-milestone`, deploy | A new process restores a checkpoint (section 14), or warms up a fresh fly, and writes a durable startup save of the state it starts from | The trace's start line names that save (`g<N>`); the shadow boots a new session from it through the ordinary FLYSIM01 import |
| Sugar (`POST /stimulate`) | Admitted on wall time and the live pulse (rate limiter, "no overlap"), applied at the top of a frame | The transition's `admissions`, replayed into the admission queue (`LegacyAdmission::replay_sugar`) |
| Operator reward pulse (`POST /reward`) | Off in every shipped configuration (403) | Cannot be reproduced (section 15 amendment): the rest of that segment is reported uncompared |
| Wall time | Pacing, publish rate, checkpoint intervals, pause (no frame runs). None changes a transition | Not needed. The trace records which saves were taken where |
| Chat | A caption track; it reaches nothing the fly does | Not needed |
| Threads | The sweep's result does not depend on the thread count (AGENT-01) | The shadow picks its own |

So "the same inputs" means *the same start state per process and the same admissions per
transition*. Admission itself is not compared. The session runtime decides admission against a
pulse one commit stale, by the operator's decision (section 15). A shadow that re-decided sugar on
its own clock would diverge by design, not by fault.

**The architecture: the legacy fly stays primary, and the shadow follows its trace.** The live
service writes `FLY_TRACE` (FND-01) with three switches added for the shadow. None changes a
behaviour field or anything the fly does:

- `FLY_TRACE_DIR` writes one file per process, `trace-<start wall ms>-<pid>.jsonl`.
- `FLY_TRACE_MAX_BYTES` caps each file (4 GiB by default in directory mode).
- `FLY_TRACE_LEDGERS=<n>` adds `ledgersDigest` every *n* transitions. This is the SHA-256 of
  `flysim::frame::ledgers_string` after the boundary: adapter export, ratchet, slot, and the
  executor's scene, bound set, running macro, counts and "nearer".

`fly-shadow` follows the files in start order. For each file it takes these steps:

1. It boots a session (agent, world, task and executor, as in TASK-01) from that process's startup
   save. A fresh start is different: the stores were empty and the startup save is at boundary 0.
   In that case the shadow runs the same power-on scaffold and warm-up
   (`Environment.Initialize`, `Agent.Initialize`), and holds the startup save to its own
   boundary 0, byte for byte. An import would not give the same emulator. binjgb's audio
   resampler phase is not in its exported state, so the channel accumulators of an imported
   emulator drift from those of a powered-on one, in the exported state only. Both restores of a
   restart import, so they agree.
2. For every transition, it replays the admissions and runs `Coordinator::step`. It takes a
   capture wherever the live trace records one, before or after the boundary's rollback as the
   live loop took it, then applies the deferred rollback.
3. It builds its own trace line from what crossed the session's boundaries (`trace::line`) and
   compares it with the live line, field by field.

A spool thread copies every new store generation as it appears, because a hot file lives only
about ten seconds. The shadow never writes to the live stores.

The reverse arrangement was considered: the session runtime as primary, with legacy as the
checker. It would make the shadow run the cutover itself, and a divergence would already be on
the stream. A second legacy process in lockstep was also considered. It adds nothing, because the
live process's own trace *is* the legacy behaviour.

**What is compared, per boundary.**

- *The trace line*, every transition. The fields are `step`, `admissions`, `ticksAdvanced`,
  `brainTicks`, `remainder`, `ratesDigest`, `spikesDigest`, `spikeCount`, `decision`, `mask`,
  `macroEvents`, `framebufferDigest`, `wramDigest`, `rewards`, `rank`, `acknowledgedBoundary`
  and `boundaryActions`. `boundaryActions` covers the slot saves with their state digests and
  the rollbacks.
- *The ledgers*: `ledgersDigest` on the transitions that carry it. The shadow digests the same
  string from its own task (`task::ledgers_of` is `ledgers_string`). This covers the executor's
  ledgers, which no checkpoint carries.
- *The checkpoint bytes*, at every live save the spool still holds. The shadow captures the same
  boundary and exports FLYSIM01 through the shared encoder (STATE-02). It compares the export
  byte for byte with the live file after the substitutions named below. `compatibility`,
  `speed` and `rankSinceMs` are the shadow's own, so they are compared too.

**Declared differences**, excluded by name. The verdict lists the same names.

| Name | What is excluded |
| --- | --- |
| `archive-order` | A legacy milestone archive taken before that boundary's slot save (`afterActions` 0 ahead of a `save-slot`, sections 4 and 16) holds the pre-capture ratchet and slot. The live file's `ratchet`, `ratchetGame` and `ratchetFrame` stand in for the shadow's in that capture only. The operator accepted this on 2026-09-23 ("Archive order") |
| `decision-list-order` | The decision is compared as `gameboy-channels-v1` sees it (section 6, `trace::channels`) |
| `host-fields` | `generation`, `wallMs` and `lastEventId` are the live file's. They are host bookkeeping: the shadow keeps no feed event log (SERVE-01's service host keeps its own) and allocates no generations, because it writes no store |
| `admission-replay` | Sugar is replayed, not re-decided (above) |
| `operator-reward-pulse` | A segment with one is reported uncompared from that transition on |

**The window.** The verdict is `pass` once the compared transitions add up to **10,800 s of
live brain time** (the sum of `ticksAdvanced`, 3 h at real time) with zero divergence, and at least
one live save per 600 of those brain seconds has been compared byte for byte. That time may span
several processes.

*Skips* are only the live side's own events (`SKIP_KINDS`):

- `startup-save-gone`: a process whose startup save was already gone when the shadow reached it,
  because the shadow started late or its spool evicted the file;
- `operator-reward-pulse`;
- `trace-cap`: the live trace stopped at its byte cap, or with no consumer;
- `no-transition`: a process that ran none.

A skip adds nothing to the window. A trace the shadow cannot read is `trace-malformed`, which
refuses the cutover. A process killed hard loses its unflushed tail, and those transitions are
not compared.

The first difference of any kind stops the shadow with `diverged` (exit status 3) and writes
`divergence.json` with the 30 transition pairs before it. The same applies to every session-side
failure, which is **never** a skip:

- a trace field;
- the ledgers;
- the checkpoint bytes;
- a session that does not start;
- a startup save the candidate's restore gate refuses or the session runtime cannot restore;
- a step, capture or rollback error;
- a transition the session did not record;
- *coverage*: a live flysim process that booted while the shadow ran and left no trace
  (amended 2026-09-30).

The shadow reads every boot header in the live sugar journal. Every flysim process writes one,
traced or not. It holds each header to a trace file created after the previous boot and before
this one. A process that ran untraced can never be compared, so the verdict fails. It does not
pass on the processes that were traced.

A shadow that keeps running after `pass` turns the verdict to `diverged` on a later difference.

**The live fly never pays for the shadow (amended by the SHADOW-01 review).** Three mechanisms
make sure of it:

- *The trace is on only while a shadow consumes it.* The shadow writes a heartbeat
  (`<trace dir>/consumer`) every 30 s and removes it when it stops. flysim starts no trace without
  a fresh heartbeat, stops a running one within a minute of frames once the heartbeat is more than
  10 minutes old, and keeps the directory under 8 GiB. A heartbeat dated ahead of flysim's clock
  counts as fresh. A trace flysim cannot create (a full disk, the directory's permissions) never
  stops the live fly from starting: that process runs untraced, and the shadow reports it as a
  coverage divergence.
- *The guard* (`fly-shadow-run guard`, every 60 s, root) judges the live fly against its own pace
  before the shadow existed. The live fly does not always keep real time: the release container
  has run at a realtime factor of 0.66, and at 0.77-0.99 after its cpuset rebalance. So `start`
  first measures a 10-minute baseline over sixty 10-s `fly_realtime_factor` means. It records
  their median and their spread (1.4826 x MAD), so a step in the window cannot set or widen it,
  and the `fly_lag_seconds` growth rate over the most recent half. The guard trips only on a *sustained* degradation against
  that baseline: 3 checks in a row in which the last 5 samples of the same process fall below the
  baseline realtime factor, or grow lag faster than the baseline rate, by more than
  max(0.05, 4 x spread / sqrt 5). A trip stops everything and restarts flysim without the trace.
  The guard stops itself when the shadow is not running. `lint.sh` holds the rule to the release
  container's profiles, synthesized, and to four traces recorded from a real flysim with and
  without a real shadow (`infra/tests/fixtures/shadow-guard`). In 800 synthetic 3-hour runs it
  found no false trip, and it caught all 400 degradations.
- *The shadow* runs at `SCHED_IDLE` off flysim's CPUs. It pauses for a minute while the live lag
  grows faster than the baseline rate. When a pause does not help, the shadow is not the cause,
  and it stops pausing for 10 minutes.

**The verdict contract (for CUT-01).** The shadow writes `verdict.json` in the format
`fly-shadow-verdict-v1` (`shadow::verdict`, whose module notes list the fields). CUT-01 cuts over
automatically only if all of the following hold when it reads the file, at the moment of cutting
over:

- `status` is `pass` and `firstDivergence` is `null`;
- `compared.brainSeconds` ≥ `required.brainSeconds`, and never less than the operator's 10,800 s,
  whatever window the shadow was run with;
- *Saves were compared.* At least one per 600 brain seconds was compared byte for byte, and at
  most 10 % of those the trace named were unavailable.
- *The release.* `candidate.release` is the directory `/opt/fly/current` resolves to now. Every
  binary in `candidate.binaries` still has its recorded SHA-256 there. The binary switched to,
  `flysim-session` (SERVE-01's service), is one of them.
- `candidate.compatibility` is the live `--print-compatibility`;
- every `skipped` entry is one of the live side's own kinds;
- *the shadow is alive and caught up*: `updatedAt` is at most 5 minutes old and
  `lagTransitions` is at most 3,600, counted at every write (paused or not) over the rest of the
  current file and every newer one;
- (`fly-shadow-run check`) the guard has not tripped and `flyshadow.service` is running.

`fly-shadow check` (and `fly-shadow-run check`) implements exactly this rule and exits 0 only when
it holds. Anything else keeps the legacy fly. The one-command rollback `fly-runtime legacy` is
CUT-01's.

**Amendment, 2026-09-30 (SHADOW-02): the shadow runs on a build box.** On the release container
the shadow cost the live fly real time: SCHED_IDLE on the page and encoder CPUs, sharing them with
the encoder and the browser and the memory bandwidth of flysim's NUMA node, it took about 27 ms a
frame, fell further behind every hour (so it could never meet the catch-up bound), and the live
realtime factor fell from 0.9998 to 0.93-0.98. It recovered to 0.999-1.0 the moment the shadow
stopped. The comparison itself is unchanged: the shadow is the same binary of the same release,
following the same trace. What moves is where it runs, and so how its inputs reach it and its
verdict comes back (`shadow::remote`, `shadow::relay`, `shadow::ingest`).

- *The relay* (`fly-shadow relay`, the container's `flyshadow.service` with the drop-in
  `fly-shadow-run start` writes) opens one ssh connection to the box and streams, every quarter
  second: the new live saves of both stores, then the new bytes of every trace file of this run
  (oldest first; a file's size read after the directory listing, so an older file is complete
  when a newer one reaches the box), then the sugar journal as read before the listing. Trace files
  that started before the run (`FLY_SHADOW_RUN_ID`, the start's Unix ms) and saves older than a
  minute before it are not sent. A writer thread owns the link, so a stall never blocks the relay:
  new saves are read as they appear and held in memory (256 MiB, about 8 minutes; a hot save lives
  about ten seconds), trace bytes wait on disk, and a newer file and the journal wait until every
  older file is complete on its way.
- *The ingest* (`fly-shadow ingest --root <dir>`) is the forced command of the relay's key on the
  box (`authorized_keys` `restrict,command=`); the box holds no credential for the container. It
  refuses a relay whose release directory or binary SHA-256 differ from its own, so the box runs the
  container's release, installed from the same tarball at the same path, and the verdict's
  `candidate` is exactly what `check` recomputes on the container. A new run id resets the mirror
  and restarts the box's shadow (it exits with status 4; `--run-id-file`).
- *The heartbeat.* flysim's `<trace dir>/consumer` on the container is now the relay's. The relay
  touches it only while it is healthy: connected; the box reported within 30 s that the shadow of
  this run is alive (its own heartbeat fresh and its verdict this run's, running or passed); and
  the box has acknowledged every byte that was on the container's disk a minute earlier. A dead box,
  shadow or link, and a stalled sync, therefore stop the live trace exactly as a dead local shadow
  does. The relay removes the file when it stops.
- *The verdict* on the container is the box's `verdict.json`, written back only when its new
  `runId` member is this run's, with two changes: `lagTransitions` is the box's plus the live
  trace not yet acknowledged by the box, so the catch-up bound covers the sync; and a `relay`
  member (`relayedAt`, `remoteLagTransitions`, `unsyncedBytes`, `unsyncedTransitions`). A diverged
  verdict brings `divergence.json` and the checkpoint files back, and the relay exits with status 3.
- *Coverage, tightened for both placements.* A trace that stops for want of a consumer
  (`"truncated":true,"reason":"no-consumer"`) while the shadow is following it is now a `coverage`
  divergence, not a `trace-cap` skip: the rest of that live process can never be compared, and with
  a relay a stalled sync is exactly how that happens. A stop already in the file when the shadow
  started is history and stays a `trace-cap` skip, and the byte cap stays a skip. The coverage
  check reads the sugar journal before it lists the trace directory (a process creates its trace
  file before it writes its boot header, so a boot between the two reads could look untraced
  the other way round).
- *`fly-shadow-run check`*, remote: in addition to everything above, `/srv/fly/shadow/relay.json`
  must be less than a minute old and say `healthy`. `fly-shadow check` (the Rust rule) is
  unchanged. `fly-shadow-run start` runs the shadow on the container only with `--local`.
- *The guard* stays on the container and guards the live fly as before; it now watches the relay's
  cost, which is a few file reads and one ssh stream.
