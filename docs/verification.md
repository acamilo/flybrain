# Verification

The library was extracted from a working prototype, so the test question is not "does it behave
sensibly" but "does it behave identically".

## Oracle strategy

`packages/brain/tests/legacy/` holds verbatim copies of the prototype's neural modules, from
`fly-plays-pokemon` commit `9ad160b` plus the uncommitted 2026-09-14 ratchet-sprint working tree.
Only import paths were changed. The files are `lif.ts`, `plasticity.ts`, `decoder.ts`
(`MotorDecoder`) and `protocol.ts` (the button constants).

Three rules follow from that:

1. **Never edit the legacy modules.** They are the reference, not code under maintenance. If the
   kernel has to change, bump `NEURAL_KERNEL_VERSION` or `PLASTICITY_VERSION` instead
   (`packages/brain/tests/legacy/README.md`).
2. **The default path is bit-exact.** Generalizing the modules added configuration, but with
   `DEFAULT_LIF_CONFIG`, `DEFAULT_PLASTICITY_CONFIG` and `gameboyDecoderConfig()` the new code
   must produce the same numbers, not merely close ones. The comparisons use
   `assert.deepEqual` over exported state and `assert.equal` over individual floats.
3. **The real dataset is part of the test.** One oracle test runs against the committed
   `data/fafb-v783` artifacts rather than a fixture, so it catches a change in the data as well as
   a change in the code. It skips with an explicit message when `data/fafb-v783/meta.json` is
   absent, which happens on a branch where the dataset has not been merged.

Beyond the oracles, each module has tests for the configuration surface the prototype did not
have: non-default constants, version-string derivation, alternate roles and budgets, and generic
decoder shapes.

## Running

```sh
npm ci
npm test
npm run typecheck
```

Both scripts fan out across the workspace with `--workspaces --if-present`. `npm test` runs
`node --import tsx --test tests/**/*.test.ts` in `packages/brain`; `npm run typecheck` runs
`tsc -p tsconfig.json` with `noEmit`, `strict`, `noUnusedLocals` and `noUnusedParameters`.

There is no build step and no test framework beyond `node:test` and `node:assert/strict`. The only
dev dependencies are `tsx`, `typescript` and `@types/node`. `three` is an optional peer dependency
used by the view layer.

As of this commit: 76 tests, all passing, 0 skipped, about 7 seconds (the agent oracle and the
real-dataset runs dominate).

## What each file covers

### `tests/dataset.test.ts` (21 tests)

Four groups, all against the committed FlyWire artifacts plus a four-neuron fixture.

- **node loader**: the artifacts decode to the expected metadata (schema 1, `FlyWire FAFB Codex
  v783`, 139,255 neurons, 2,700,513 edges, L1 population of 1,572); the CSR graph is internally
  consistent; the retina column tables decode; `circuit-roles.json` merges into `meta.roles` with
  the expected sizes (`kenyon` 5,177, `mbon` 96, `command_0..7`, `reward_pam` 307, `visual_l1`
  1,572); the fingerprint is seven hex digests joined by `:`.
- **validateDataset**: accepts the fixture and the real dataset, and rejects each length mismatch
  individually (`indptr` not neurons + 1, `targets` and `weights` against `meta.edges`, a neuron
  count that disagrees with the arrays, and each of the three visual arrays), plus circuit roles
  built against a different connectome.
- **fingerprintDataset**: deterministic for the real dataset and for the fixture, seven parts, and
  changes when any hashed part changes.
- **browser loader**: fingerprints identically to the node loader, decodes the same arrays, and
  reports a missing dataset instead of hanging.

The loader equivalence tests are the end-to-end check that the artifacts in `data/` still decode
to the connectome the model was tuned against.

### `tests/model.test.ts` (9 tests)

- `Xorshift32` reproduces the original kernel stream bit-exactly.
- `projectFrame` matches the original 160x144 retina projection, and is resolution independent:
  320x288 maps to the same relative pixels.
- `LifNetwork` is bit-exact with the legacy `FlyBrain` over 3,000 ms on the toy dataset, including
  a frame, a stimulation pulse and a reinforcement.
- `reward()` is an alias of `stimulate()` and drives the configured role.
- The default configuration keeps `lif-1ms-f64-v2`; a numeric change derives a new version string.
- Non-default constants change the dynamics, not only the version string.
- Rate roles default to the dataset order and are capped at 32.
- **Real-dataset oracle**: on `data/fafb-v783`, the new and legacy kernels agree on the selected
  plastic edge list (exactly 16,384 edges), the topology hash, the plasticity version, the tracked
  role names, the spike counts over 200 ms with a frame and a pulse, and the entire exported state
  afterwards. Skips with a message when the dataset is absent.

### `tests/plasticity.test.ts` (13 tests)

- The `Plasticity` and `FlyBrain` aliases point at the generalized classes.
- Causal eligibility is necessary; zero reward leaves exact weights; learning stays inside the
  anatomical scope.
- Eligibility decays over five seconds; anti-causal pairing depresses; bounds preserve sign.
- Exact neural continuation across export and import, including RNG, eligibility, gains and
  long-run Float64 pairing.
- Invalid imports reject before any neural or plastic mutation.
- A warm-up without a framebuffer keeps visual neurons finite. This is the regression test for the
  prototype's uninitialized warm-up bug.
- Sparse observation is exactly equivalent to the former base-edge traversal, and fails if
  `observe()` reads the base CSR index at all.
- The default configuration keeps `fly-kc-mbon-rstdp-v2`; a numeric change derives a new one; a
  default-version state is incompatible with a non-default configuration.
- `preRole`, `postRole` and `budget` choose which edges are plastic.
- `statistics()` keeps the historical `mushroom` and `output` field names.
- Clamps follow the configured gain bounds.
- Disabled plasticity ignores observation and reinforcement.

### `tests/readout.test.ts` (19 tests)

- **Oracle**: the Game Boy preset reproduces the legacy `MotorDecoder` over 20,000 steps of driven
  rates, and again on hold, cooldown and threshold edges.
- Commitment survives reversals, hysteresis rejects small leads, and the system channels share a
  throttle.
- The decoder chooses only the strongest direction; action channels respond to activity above
  baseline.
- A legacy version 3 checkpoint resumes bit-identically; a version 2 checkpoint (no version field,
  no fatigue) imports rested.
- Generic configurations: an exclusive group where hysteresis holds the incumbent until fatigue
  lets the runner-up win; exact ties keep the earlier channel; a pulse channel without a boot
  variant behaves the same in and out of boot; a throttle group shares one cooldown.
- An uncalibrated decoder reports nothing.
- Invalid checkpoints throw without mutating the decoder.
- The platformer preset (`docs/design/platformer.md` §4): a held direction never gaps because
  `holdMs` equals `decisionMs`; B is a sustained hold that releases within 600 ms of its score
  dropping; A fires at most once per 420 ms and leaves a grounded gap; Start is blocked during play
  at threshold 2.0 and permissive in boot; A, B, Start and Select are never active in the same frame
  over 20,000 steps, which is what makes Super Mario Land's four-button soft reset unreachable; and
  `clearHolds` locks every pulse out for 300 ms.

### `tests/agent.test.ts` (14 tests)

A 600-frame oracle replays the prototype worker's initialize-and-tick sequence by hand with the
legacy `FlyBrain` and `MotorDecoder` on a 4,096-neuron synthetic connectome (the four-neuron toy
saturates and can never fire a pulse channel), asserting identical button masks every frame and
deep-equal network and decoder state at the end. Further tests cover frame-clock drift, warm-up
guards, frame-size validation before any mutation, `resetTransients`, export/import resume,
`snapshot`, eight rejected-checkpoint cases that must leave state untouched (including the
decoder-only corruption that exercises the rollback), envelope structure (checksum, truncation,
trailing bytes, chunk names), the agent chunk round trip through a real envelope, and a real
FAFB v783 run through the node loader.

### `tests/view.test.ts` (6 tests)

`normalizePositions` centring, 3D-radius scaling to 1.3 then z flattening, and `classifyByRoles`
producing 0/1/2 per the prototype rule.

### `tests/fixtures/toy-dataset.ts`

A four-neuron connectome with five edges: neuron 0 is a Kenyon cell, 1 and 2 are MBONs, 3 is a
motor and `command_0` neuron. Small enough that a bit-exact comparison over 3,000 ticks is fast,
and structured so the plastic-edge selection rule has both a qualifying edge and disqualifying
ones (a negative weight, a non-KC source). It also exports a standalone `xorshift` for
deterministic test frames.

## What the tests do not cover

There is no browser-driven test of `ConnectomeView` (only its pure layout helpers are tested), and
no performance test. The prototype's throughput
figures came from a Playwright run against a ROM and are quoted in [limitations](limitations.md)
as short-run diagnostics, not benchmarks.
