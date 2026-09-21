# Integration

The library simulates a brain and decodes its activity. Everything else is the embedding
application's job: producing frames, applying channels, computing a reward, and persisting state.
Below is that contract, then the original Pokemon Red integration as a worked example.

## What an environment must provide

**1. Frames.** An RGBA byte array of any size, plus its width and height.

```ts
network.setVisualFrame(rgba, width, height);
```

The size defaults to `config.retina.width` and `config.retina.height` (160x144) when omitted.
Projection is resolution independent and the alpha channel is ignored, so an upscaled or
letterboxed framebuffer works as long as the aspect is the game's. Call it once per frame before
stepping. Until the first call, visual drive is zero, which is a well-defined warm-up state.

**2. A channel-to-input map.** Pick a decoder config and decide what its channel names mean.
For a Game Boy, `gameboyDecoderConfig()` plus `toButtonMask()` gives a joypad byte directly. For
anything else, write a config: one optional exclusive group for mutually exclusive inputs
(directions, gears, weapon slots) and a pulse channel per momentary input. Roles must be roles the
network actually tracks, which by default is `command_0..7` plus the motor and reward roles
(see [model](model.md)).

**3. A scalar reward per frame.** Sample your own game state and reduce it to one number `R`, then

```ts
if (reward !== 0) network.plasticity.reinforce(reward, network.ms);
```

`reinforce()` ignores 0 and non-finite values, so an unrewarded frame costs nothing. The modulator
is `tanh(R)` and saturates: `tanh(1) = 0.76`, `tanh(2) = 0.96`. The library has no opinion on what
deserves reward. That is the adapter's design, and the part that most needs to be conservative
(see the Pokemon notes below).

**4. Optional stimulation.** `network.stimulate(durationMs)` drives the `reward_pam` population
for that many ticks, taking the maximum of overlapping pulses. It is separate from `reinforce()`
and supplies no learning signal.

## Per-frame loop

```ts
import { LifNetwork, PopulationDecoder, gameboyDecoderConfig, toButtonMask } from '@flybrain/brain';
import { loadBrainDatasetFromDir } from '@flybrain/brain/node';

const dataset = await loadBrainDatasetFromDir('data/fafb-v783');
const network = new LifNetwork(dataset);
const decoder = new PopulationDecoder(gameboyDecoderConfig());

// Warm-up: no frames, no learning, then one calibration against the resting rates.
network.plasticity.enabled = false;
network.step(2500);
decoder.calibrate(network.rates);
network.plasticity.enabled = true;

const MS_PER_FRAME = 1000 / 59.7275; // 16.742706458499015
let remainder = 0;

for (;;) {
  const { rgba, width, height } = emulator.runFrame();
  network.setVisualFrame(rgba, width, height);

  remainder += MS_PER_FRAME;
  const ticks = Math.floor(remainder);   // 16 or 17
  remainder -= ticks;
  network.step(ticks);

  const reward = sampleReward();         // your game-state sampling
  if (reward !== 0) {
    network.plasticity.reinforce(reward, network.ms);
    network.stimulate(120);
  }

  emulator.setInput(toButtonMask(decoder.decode(network.rates, network.ms)));
}
```

Two details carry over from the prototype:

- The warm-up is 2,500 ms with plasticity disabled and zero visual drive, and `calibrate()` runs
  once, at its end. Calibrating earlier records rates that are still climbing from zero.
- The fractional remainder is carried across frames so one Game Boy frame advances the brain by 16
  or 17 integer 1-ms ticks, averaging 16.74. Rounding per frame instead would drift
  (`fly-plays-pokemon/docs/architecture.md`, "Data flow").

`NeuralAgent` (`src/agent/agent.ts`) wraps exactly this: it composes a network and a decoder,
owns the warm-up, the calibration and the remainder accumulator, and exposes one per-frame call:

```ts
import { NeuralAgent, gameboyDecoderConfig, toButtonMask } from '@flybrain/brain';

const agent = new NeuralAgent(dataset, { decoder: gameboyDecoderConfig() });
agent.warmup(firstFrame);
// per emulator frame:
const { active } = agent.tick(frameRgba, { rewards: events, boot: !playing, learn: !manualInput });
emulator.setButtons(toButtonMask(active));
```

`rewards` is a list of `{ value, stimulationMs? }`; values are summed into the modulator and each
event also drives the stimulation population for its duration (default 120 ms). After a host rolls
the game back to an earlier snapshot, call `agent.resetTransients(frame)`: it clears decoder holds
and eligibility traces but keeps the RNG, clock and learned gains, as the prototype's game
recovery did. `examples/node-random-frames.ts` runs the whole loop in plain Node on noise frames.

The scheduler does not guarantee real time. `step()` is synchronous and takes as long as it takes;
see [limitations](limitations.md) for measured throughput.

## Checkpoints

Export the agent's state alongside your emulator state, and refuse a restore unless the
compatibility string matches exactly.

```ts
const compatibility = [
  network.version,                      // kernelVersion(config), e.g. 'lif-1ms-f64-v2'
  dataset.fingerprint,                  // seven SHA-256 digests joined with ':'
  network.plasticity.version,           // plasticityVersion(config), e.g. 'fly-kc-mbon-rstdp-v2'
  romSha256,                            // your environment's identity
  adapterVersion,                       // your reward catalog's version
].join('|');
```

The three library components are the mandatory part. `network.version` and
`network.plasticity.version` change whenever a numeric parameter changes;
`dataset.fingerprint` changes whenever any loaded array or role list changes. A semantic change
that does not move a numeric parameter requires bumping `NEURAL_KERNEL_VERSION` or
`PLASTICITY_VERSION` by hand.

Payload: whatever your environment needs, plus `network.exportState()` (which nests the plasticity
state) and `decoder.exportState()`. Import order matters, because each component validates before
mutating: check the compatibility string first, then import, then treat any throw as a failed
restore. The prototype took a pre-restore checkpoint so it could roll back if a later component
rejected after an earlier one had already mutated
(`fly-plays-pokemon/docs/architecture.md`, "Checkpoint contract and migration").

Call `decoder.clearHolds(network.ms)` after a restore if the environment may have moved on, so a
restored hold cannot leak into a new context.

## Worked example: Pokemon Red

The prototype's data flow was:

```
local ROM -> binjgb WASM -> RGBA frame -> visual drive -> LIF network -> fixed motor decoder -> buttons
```

with `reward events -> synthetic scalar modulator -> eligibility traces` and a separate
`reward events -> PAM stimulation` path. Neither reward path touched the decoder
(`fly-plays-pokemon/docs/architecture.md`, "Data flow").

**State sampling.** WRAM was read synchronously after one completed emulator frame, with no
emulator execution interleaved. Addresses and flag names were generated from the pret/pokered
disassembly at commit `0cd19d3b877b7dc66d12c7050bed9a7f38154d4b`: 23 RAM addresses and 368
selected named flags. A per-sample immutable byte cache read each address at most once across all
flag and bit loops, including the 19 Pokedex bytes, avoiding repeated WASM crossings.

**Gating.** Rewards only counted from a "playable" sample: game timer active, map ID <= 247,
nonzero map dimensions, coordinates inside the map, party count <= 6, ordinary battle type, no
test-battle flag. Map and exploration observations additionally required overworld battle state,
three stable samples at the same location, no scripted or disabled joypad state, no ignored joypad
input, and no door, warp, ledge or spin movement. The first valid sample baselined every
already-set event, species and badge bit, so loading existing progress did not replay past
achievements.

**Reward catalog** (`pokered-unique8-v3`), from
`fly-plays-pokemon/docs/rewards-learning.md`, "Reward rules":

| Kind | Value | Trigger and budget |
| --- | ---: | --- |
| Story | +1 | each selected story bit once; adventure started after observed boot |
| Explore | +0.05 | each additional 8 unique controllable `(map,x,y)` locations, up to 25 payouts (200 locations) per map |
| Area | +0.2 | first stable controllable visit to a new map after the initial baseline |
| Pokedex | +0.5 | each of 151 newly owned species bits, including gifts and evolution |
| Trainer | +0.5 | each named `EVENT_BEAT_*` flag once, except flags classified as story milestones |
| Wild win | +0.1, +0.05, +0.0333 | at most three observed wild KOs per `(map,species,level)` |
| Badge | +3 | each newly set badge bit |

Every value is positive; there were no loss or blackout penalties. Values in a frame summed into
`R`, then `m = tanh(R)`. PAM stimulation ran 80 to 400 ms depending on reward kind, with
overlapping pulses taking their maximum.

**Timing.** One Game Boy frame advanced the brain by 16 or 17 integer 1-ms steps using a saved
fractional accumulator.

**Compatibility string.** Centralized in `src/runtime/compatibility.ts`, pinning the ROM SHA-256,
the adapter version, the dataset fingerprint, the explicit LIF and plasticity kernel versions, the
binjgb revision `c60e138da5a795ebb55e56b11b7e90024e41112c` and the pokered commit. All of these
were local build constants, never runtime fetches.

**What to copy from it.** The shape: sample after a completed frame, cache bytes per sample, gate
hard on "is the game in a state I understand", baseline on first valid sample so a restore does
not replay history, keep rewards sparse and novelty-limited, and pin everything that could change
semantics into one string. The Pokemon-specific addresses, flags and values belong to that adapter
and do not generalize.

## Sketch: a platformer adapter

A second demo would reuse the network, the decoder and the Game Boy preset unchanged, and replace
only the reward adapter. From `docs/streaming-plan.md` section 6, which surveyed Super Mario Land
(GB, 1989):

- **Prefer WRAM and HRAM.** That section quotes a datacrystal RAM map with lives at `0xDA15`
  (BCD), coins at `0xFFFA` (BCD), score at `0xC0A0` (3 bytes BCD), powerup status at `0xFF99` and
  `0xFFB5`, screen-relative position at `0xC201` and `0xC202`, and a game-over marker `0x39` at
  `0xC0A4` and `0xFFB3`. The map is quoted as given and was not independently verified against a
  ROM.
- **Two gaps.** World and stage appear in that map only in VRAM, as HUD tile indices rather than
  values, and a global scroll position for level progress is not in the map at all. Both need a
  RAM search with watchpoints on a level transition. VRAM reads also need care: CPU-bus reads are
  blocked during LCD mode 3 on real hardware, and whether binjgb enforces that is unverified, so
  sample during VBlank or find the WRAM source.
- **Reward sketch**, deliberately mirroring the Pokemon catalog's sparse, gated, novelty-limited
  shape: stage advance +3, a rightmost-progress checkpoint +0.05 capped per stage, coin +0.05,
  score delta scaled and capped +0.1, first powerup per stage +0.5, and a death event. Whether to
  use negative reward at all is an open question: the existing catalog is strictly non-negative
  and the rule is eligibility-trace based, so a sign change is a real semantic change.
- **Open design questions** recorded there: how to stop the fly farming the first screen's coins,
  whether standing still needs a timer penalty, and whether precise jumps are possible at the
  measured frame rate. Progress-only-forward with a per-stage cap is the safest start.
- **A gentler fallback.** Kirby's Dream Land has a real scroll position at `0xD051` plus health,
  lives and score all in WRAM, so it needs no VRAM decoding. That section recommends deciding
  between the two after a RAM-search spike rather than from the wiki alone.

Read that section before starting. The platformer reward rules there are marked "needs review
before implementation". ROM sourcing and legality stay the same as for Pokemon: local only, never
committed, hash-pinned into the compatibility string.
