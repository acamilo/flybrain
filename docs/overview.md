# Overview

`@flybrain/brain` simulates a fruit-fly brain and turns its activity into discrete output
channels. It holds four things: a connectome dataset format, a leaky integrate-and-fire (LIF)
kernel with reward-modulated plasticity, a population-rate readout, and viewer geometry. It holds
no game, no emulator, no reward rules and no server. Those live in the application that embeds it.

The library was extracted from the `fly-plays-pokemon` prototype so that more than one game demo
can share one core. The default configuration reproduces that prototype's kernel bit for bit, and
oracle tests hold it there (see [verification](verification.md)).

## Pipeline

```
image -> retina drive -> LIF network -> population rates -> decoder -> output channels
```

| Stage | Module | Input | Output |
| --- | --- | --- | --- |
| retina drive | `model/retina.ts` | RGBA frame of any size | one drive value per retina column |
| LIF network | `model/lif.ts` | column drive, 1-ms ticks | spikes, per-role rate estimates |
| population rates | `model/lif.ts` | spike counts per role | `role -> rate` record, spikes/s |
| decoder | `readout/decoder.ts` | rates plus a calibration baseline | active channel names |
| output channels | caller, or `readout/presets/` | channel names | device input (for example a joypad mask) |

Two side paths feed the network and never touch the readout:

```
reward (scalar, per frame) -> plasticity.reinforce() -> gains on selected KC->MBON edges
stimulation (pulse) -> network.stimulate() -> extra drive to the reward_pam role
```

`reinforce()` supplies the synthetic learning signal. `stimulate()` excites an anatomical
population and has no causal link to the gain update
(`fly-plays-pokemon/docs/rewards-learning.md`, "Plasticity").

## Layer map

- **dataset** (`src/dataset/`): the artifact format, validation and fingerprinting. `format.ts`
  defines `BrainDataset`; `load-node.ts` and `load-browser.ts` are platform loaders that produce
  identical arrays and identical fingerprints. See [dataset format](dataset-format.md).
- **model** (`src/model/`): `lif.ts` (the 1-ms kernel), `plasticity.ts` (reward-modulated STDP),
  `retina.ts` (image to column drive), `rng.ts` (deterministic xorshift), `version.ts` (version
  string hashing). See [model](model.md) and [plasticity](plasticity.md).
- **readout** (`src/readout/`): `decoder.ts` (`PopulationDecoder`) plus device presets under
  `presets/`. See [readout](readout.md).
- **agent** (`src/agent/`): `NeuralAgent` composes a network and a decoder into one per-frame
  call: a 2,500-ms warm-up with plasticity disabled and zero visual drive, one `calibrate()`
  against the resting rates, then per-frame stepping with remainder accumulation of
  1000/59.7275 ms so a frame advances the brain by 16 or 17 integer 1-ms ticks. It also owns
  `resetTransients()` (the hook a host calls after rolling the game back), `exportState()` and
  `importState()` with rollback on a rejected checkpoint, and `compatibility()`. `envelope.ts`
  is the checksummed binary container (`encodeEnvelope`/`decodeEnvelope`) and the chunk helpers
  that split an `AgentState` into named typed-array chunks. See [integration](integration.md).
- **view** (`src/view/`): `ConnectomeView`, reachable only through the `@flybrain/brain/view`
  subpath because it needs a DOM and WebGL. It renders the fixed orthographic activity map from
  `positions.binz`, `classes.binz`, `viewer-edges.binz` and `circuit-roles.json`; classification
  and colours are options. `layout.ts` holds the pure helpers (`normalizePositions`,
  `classifyByRoles`).

## Design principles

**Anatomy-constrained, and roles are labels.** Connectivity comes from the FlyWire FAFB Codex
v783 export. Role lists (`kenyon`, `mbon`, `descending`, `reward_pam`, ...) are anatomical
annotations from that export, not functions inferred from behaviour. The `command_0..7` buckets
are a round-robin partition of the descending population and the transmitter sign table is a
modeling choice; both are called out as such in `tools/README.md`.

**Synthetic learning signal.** The modulator is `m = tanh(R)` for a caller-supplied scalar `R`.
It is not a fitted model of dopamine release and the plastic sites are anatomical, not fitted
compartments (`model/plasticity.ts`, class doc comment).

**Fixed readout.** The decoder calibrates once and then applies fixed ratios, timings and
thresholds. No button-level gain adapts. Learning lives in the network
(`readout/decoder.ts`, module doc comment).

**Determinism and bit-exact checkpoints.** Noise is one xorshift32 stream; membrane state is
Float32; spike and eligibility timestamps are Float64 milliseconds. A checkpoint plus the dataset
reproduces a run exactly. `exportState()` and `importState()` validate every field before writing
anything, and a checkpoint is only accepted against a matching `kernelVersion()`, dataset
fingerprint and `plasticityVersion()`.

**Default config pinned to the prototype.** `DEFAULT_LIF_CONFIG` and
`DEFAULT_PLASTICITY_CONFIG` carry the original constants, and the default version strings
`lif-1ms-f64-v2` and `fly-kc-mbon-rstdp-v2` are kept verbatim so existing checkpoints stay
loadable. Verbatim copies of the prototype's modules live in `packages/brain/tests/legacy/` and
are used as oracles.
