# flybrain

A simulated fruit-fly brain that plays video games. A connectome-constrained spiking network reads
the screen, its population rates become controller inputs, and a scalar reward nudges a bounded set
of Kenyon-cell to MBON gains.

The library is `@flybrain/brain` in `packages/brain`. It holds the connectome dataset format, the
LIF kernel, the plasticity rule, the population-rate readout and the activity-map geometry. It
holds no game, no emulator and no reward rules: those belong to whatever embeds it.

## Workspace layout

| Path | Contents |
| --- | --- |
| `packages/brain` | the library (`@flybrain/brain`) |
| `data/fafb-v783` | FlyWire-derived browser artifacts (CC BY-NC 4.0) |
| `tools/` | the Python builder that regenerates `data/` from official Codex exports |
| `docs/` | overview, dataset format, model, plasticity, readout, integration, limitations, verification |
| `services/flysim` | the Rust service: brain, emulator, snapshot feed, control API, checkpoints |
| `apps/` | planned: one directory per game demo |
| `infra/` | planned: deployment for the 24/7 stream (see `docs/streaming-plan.md`) |

## Quick start

```sh
npm ci
npm test
npm run typecheck
```

76 tests, about 7 seconds. There is no build step. `npx tsx packages/brain/examples/node-random-frames.ts 60`
runs the full 139,255-neuron brain with the Game Boy readout on noise frames in plain Node
(about 0.9x Game Boy real time single-threaded on a WSL laptop).

## Usage

```ts
import { NeuralAgent, gameboyDecoderConfig, toButtonMask } from '@flybrain/brain';
import { loadBrainDatasetFromDir } from '@flybrain/brain/node';

const dataset = await loadBrainDatasetFromDir('data/fafb-v783');
const agent = new NeuralAgent(dataset, { decoder: gameboyDecoderConfig() });
agent.warmup(firstFrame);                        // 2,500 ms with plasticity off, then calibrate

// every emulator frame:
const { active } = agent.tick(framebuffer, {     // RGBA 160x144 by default; any size via config
  rewards: [{ value: 0.5 }],                     // scalar rewards your game adapter detected
  boot: !inGame,                                 // relaxes Start/Select throttling on title screens
});
emulator.setButtons(toButtonMask(active));

const checkpoint = agent.exportState();          // bit-exact resume, validated on import
```

The lower layers (`LifNetwork`, `RewardModulatedStdp`, `PopulationDecoder`) are exported too for
hosts that want to run the loop themselves.

[integration.md](docs/integration.md) has the full per-frame loop, the fractional frame timing and
the checkpoint contract.

## Documentation

- [Overview](docs/overview.md): the pipeline, the layer map and the design principles.
- [Dataset format](docs/dataset-format.md): artifacts, CSR layout, weight encoding, every role and
  its count, fingerprinting, regeneration, license.
- [Model](docs/model.md): the 1-ms LIF kernel step by step, every default constant, the retina
  projection, the RNG, state export and version strings.
- [Plasticity](docs/plasticity.md): edge selection, the eligibility and reinforcement equations,
  statistics, topology hash and the explicit non-claims.
- [Readout](docs/readout.md): scores, exclusive groups, pulse channels, the blocked-direction
  cooldown, the Game Boy preset table and checkpoint versions.
- [Integration](docs/integration.md): what a game must provide, the Pokemon Red integration as a
  worked example, and a sketch of a platformer adapter.
- [Limitations](docs/limitations.md): what is not claimed, what is unproven, measured throughput.
- [Verification](docs/verification.md): the oracle-test strategy and what each test file covers.
- [Streaming plan](docs/streaming-plan.md): headless capture, Twitch, VM design and the phased
  plan for a 24/7 stream.
- [Artifact builder](tools/README.md): how to regenerate and verify `data/fafb-v783`.
- [Data attribution](data/fafb-v783/ATTRIBUTION.md): source, license and citations.

## Provenance

The network, plasticity rule, readout, FlyWire pipeline and activity viewer were extracted from
the `fly-plays-pokemon` prototype so several game demos can share one core. The default
configuration reproduces that prototype's kernel bit for bit, and verbatim copies of its modules
live in `packages/brain/tests/legacy/` as oracles. Built with Astra.

## Licensing

The `data/fafb-v783` artifacts are derived from the FlyWire FAFB public Codex v783 exports and are
licensed [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/). That is a
non-commercial license, so a commercial demo needs a different data source or separate permission.
Citations and the list of modifications are in
[`data/fafb-v783/ATTRIBUTION.md`](data/fafb-v783/ATTRIBUTION.md).

No license has been chosen for the code in this repository yet.

ROMs and save states never enter this repository.
