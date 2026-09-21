# @flybrain/brain

A connectome-constrained spiking network, reward-modulated plasticity and a population-rate
readout, for driving games from a simulated fly brain.

Four layers:

- **dataset** (`src/dataset/`): the schema-1 artifact format, validation and a seven-part SHA-256
  fingerprint used for checkpoint compatibility. Two loaders, Node and browser, produce identical
  arrays and identical fingerprints.
- **model** (`src/model/`): a leaky integrate-and-fire kernel stepped in integer 1-ms ticks over
  the connectome, with a deterministic xorshift noise stream, an image-to-column retina
  projection and three-factor plasticity on a bounded set of excitatory Kenyon-cell to MBON edges.
- **readout** (`src/readout/`): `PopulationDecoder` turns `role -> rate` into active channel names
  through one optional exclusive group and any number of pulse channels. Device specifics live in
  `presets/`; the only one shipped is the Game Boy's eight buttons.
- **agent** (`src/agent/`): `NeuralAgent` composes network and decoder into one per-frame
  `tick()`, with warm-up, calibration, remainder accumulation, transient reset, checkpoint
  export/import with rollback, and a checksummed envelope for serializing state.
- **view** (`src/view/`): `ConnectomeView`, the fixed orthographic activity map, plus pure layout
  helpers. `three` is an optional peer dependency for it.

The default configuration reproduces the `fly-plays-pokemon` prototype bit for bit, pinned by
oracle tests against verbatim copies of its modules in `tests/legacy/`. Version strings
`lif-1ms-f64-v2` and `fly-kc-mbon-rstdp-v2` are kept verbatim so prototype checkpoints stay
loadable.

## Exports

| Subpath | Module | Exports |
| --- | --- | --- |
| `@flybrain/brain` | `src/index.ts` | `LifNetwork` (alias `FlyBrain`), `DEFAULT_LIF_CONFIG`, `kernelVersion`, `NEURAL_KERNEL_VERSION`, `MAX_RATE_ROLES`, `RewardModulatedStdp` (alias `Plasticity`), `DEFAULT_PLASTICITY_CONFIG`, `plasticityVersion`, `PLASTICITY_VERSION`, `projectFrame`, `DEFAULT_RETINA_CONFIG`, `Xorshift32`, `PopulationDecoder`, `gameboyDecoderConfig`, `GAMEBOY_BUTTONS`, `GAMEBOY_BUTTON_BITS`, `toButtonMask`, `fromButtonMask`, `fingerprintDataset`, `validateDataset`, `NeuralAgent`, `GAMEBOY_MS_PER_FRAME`, `DEFAULT_WARMUP_MS`, `DEFAULT_STIMULATION_MS`, `encodeEnvelope`, `decodeEnvelope`, `agentToChunks`, `agentFromChunks`, `AGENT_CHUNK_NAMES`, `ENVELOPE_SCHEMA_VERSION`, and the accompanying types |
| `@flybrain/brain/node` | `src/dataset/load-node.ts` | `loadBrainDatasetFromDir(dir)`, using `node:fs`, `node:zlib` and `node:crypto` |
| `@flybrain/brain/browser` | `src/dataset/load-browser.ts` | `loadBrainDataset(baseUrl)`, using `fetch`, `DecompressionStream` and `crypto.subtle` |
| `@flybrain/brain/view` | `src/view/connectome.ts` | `ConnectomeView`, `normalizePositions`, `classifyByRoles`; needs a DOM and WebGL |

The loaders are separate subpaths so a bundle never pulls in `node:zlib` and a server never
depends on `DecompressionStream`.

## Scripts

```sh
npm test         # node --import tsx --test tests/**/*.test.ts
npm run typecheck
```

## Documentation

Full documentation is in [`../../docs`](../../docs): [overview](../../docs/overview.md),
[dataset format](../../docs/dataset-format.md), [model](../../docs/model.md),
[plasticity](../../docs/plasticity.md), [readout](../../docs/readout.md),
[integration](../../docs/integration.md), [limitations](../../docs/limitations.md),
[verification](../../docs/verification.md).
