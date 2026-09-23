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
they are not `Id`s. The context is what the task hands the decoder with each Prepare (the
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

- **The image.** Byte *i* is what `fly_gb_read_mem(i)` returns at this boundary, which is
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

## 17. PROF-02b: MaleCNS bundles (later)

Dataset manifests, original-ID mapping, anatomical roles, sensory and readout bindings, strict
graph validation for new bundles, and the composite behaviour identity of FOUNDATION-02 are
**not** in this document. They ship with PROF-02b, before DATA-01, as their own contract.
Nothing here constrains them, except that a new profile never reuses this profile's id or its
legacy exception.
