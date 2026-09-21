# Plasticity

`RewardModulatedStdp` (`packages/brain/src/model/plasticity.ts`) is a three-factor learning rule:
pre-post spike timing writes an eligibility trace, and a caller-supplied scalar reward converts
that trace into a change in a per-edge gain. `Plasticity` is a historical alias of the same class.
A `LifNetwork` constructs one and exposes it as `network.plasticity`.

Nothing else learns. Original weights stay immutable, no connections are created, and the readout
never adapts (`fly-plays-pokemon/docs/rewards-learning.md`, "Plasticity").

## Default configuration

`DEFAULT_PLASTICITY_CONFIG`:

| Field | Default | Meaning |
| --- | --- | --- |
| `preRole` | `kenyon` | role a plastic edge's source must belong to |
| `postRole` | `mbon` | role a plastic edge's target must belong to |
| `budget` | 16384 | maximum number of plastic edges |
| `traceMs` | 5000 | eligibility-trace decay time constant, ms |
| `pairMs` | 20 | spike-pair exponential time constant, ms |
| `pairWindowMs` | 100 | largest spike interval that still pairs, ms |
| `potentiation` | 0.1 | eligibility added for a causal pair at dt = 0 |
| `depression` | 0.05 | eligibility subtracted for an anti-causal pair at dt = 0 |
| `learningRate` | 0.002 | gain step per unit of modulator times eligibility |
| `restoring` | 0.0001 | pull back towards a gain of 1, on reinforcement only |
| `minGain` | 0.9 | lower gain clamp |
| `maxGain` | 1.1 | upper gain clamp |

## Edge selection

Selection runs once, in the constructor, and is deterministic. A base CSR edge `e` from `source`
to `target` is a candidate when all of:

- `weights[e] > 0` (excitatory only; no inhibitory edge is ever plastic),
- `source !== target` (no self-edges),
- `source` is in `preRole` and `target` is in `postRole`.

Candidates are then sorted by descending `weights[e]`, with ascending edge index as the
tie-break, and the first `budget` are taken. The winners are re-sorted into ascending edge order
so that traversal follows the base CSR order.

With the defaults on `data/fafb-v783` this selects exactly 16,384 edges, the strongest positive
KC to MBON connections, which `packages/brain/tests/model.test.ts` asserts against the oracle.

Two lookup structures are built from the winners:

- `slots`: one Int32 per base edge, the plastic slot index or -1 for the immutable majority.
  `gain(e)` reads it and returns 1 for -1.
- `incoming[neuron]` and `outgoing.get(neuron)`: per-neuron slot lists, so `observe()` never
  touches the base CSR arrays. Both preserve ascending edge order, which keeps pairing order
  identical to the original base-edge traversal.

## Eligibility

Every trace update goes through one lazy helper, so a trace is only advanced when it is read or
written:

```
decay          = exp(-max(0, ms - touched[slot]) / traceMs)
traces[slot]   = clamp(traces[slot] * decay + pair, -1, 1)
touched[slot]  = ms
```

`observe(spikes, count, lastSpike, ms)` is called by the kernel once per tick, before this tick's
spikes are written into `lastSpike`. For each spiking neuron:

- **causal**, for each slot arriving at it: `dt = ms - lastSpike[source of that slot]`, and if
  `0 < dt <= pairWindowMs` the pair term is

  ```
  +potentiation * exp(-dt / pairMs)     // +0.1 exp(-dt/20) at the defaults
  ```

- **anti-causal**, for each slot leaving it: `dt = ms - lastSpike[target of that slot]`, and if
  `0 < dt <= pairWindowMs` the pair term is

  ```
  -depression * exp(-dt / pairMs)       // -0.05 exp(-dt/20) at the defaults
  ```

Source: `model/plasticity.ts`, `observe()` and `trace()`; original wording in
`fly-plays-pokemon/docs/rewards-learning.md`, "Plasticity".

Consequences:

- The window is `0 < dt <= 100 ms`, five time constants of support. Simultaneous spikes have
  `dt = 0` and do not pair, in either direction.
- Depression is half the size of potentiation at the same `dt`.
- Traces are clipped to `[-1, 1]` after every update, not at reinforcement time.
- The 5-second trace decay runs from the last touch, so a trace untouched for 5 s is down to
  `exp(-1)` = 0.37 of its value by the time a reward arrives.
- `observe()` returns immediately when `enabled` is false, so a warm-up with plasticity off costs
  nothing and writes nothing.

`clearEligibility(ms)` zeroes every trace, sets every `touched` to `ms` and zeroes the reported
signal. It does not touch gains.

## Reinforcement

```ts
network.plasticity.reinforce(reward, network.ms);
```

`reinforce()` returns immediately when plasticity is disabled, when `reward` is not finite, or
when `reward` is exactly 0. No reward means no gain update and no restoring step.

Otherwise the modulator is `m = tanh(reward)`, and for every selected edge, after first advancing
its trace to `ms` with a zero pair term:

```
gain <- clamp(gain + 0.002 m e - 0.0001 (gain - 1), 0.9, 1.1)
```

with `e` the edge's eligibility, `0.002` = `learningRate`, `0.0001` = `restoring`, and the clamp
bounds `minGain` and `maxGain`. Source: `model/plasticity.ts`, `reinforce()`; original equation in
`fly-plays-pokemon/docs/rewards-learning.md`, "Plasticity".

The restoring term is a small pull back towards 1 that limits long-run drift. It acts only on a
nonzero reinforcement, so an idle network does not decay its learning. Because the clamp is
`[0.9, 1.1]` and only positive weights are selected, effective transmission stays the same sign
as the measured connectome weight.

`updates` increments once per reinforcement that changed at least one Float32 gain.

## Statistics

`statistics(): LearningStats` returns:

| Field | Meaning |
| --- | --- |
| `version` | this instance's `plasticityVersion()` string |
| `enabled` | whether observation and reinforcement are active |
| `synapses` | number of selected edges (16,384 by default) |
| `mushroom` | count of edges in hash group 1, which is all of them |
| `output` | `synapses - mushroom`, always 0 |
| `updates` | reinforcements that changed at least one gain |
| `changed` | gains displaced from 1 by more than 1e-6 |
| `meanChange` | mean of `abs(gain - 1)` over all selected edges |
| `maxChange` | maximum of `abs(gain - 1)` |
| `signal` | the last `tanh(reward)` |

`mushroom` and `output` are the historical field names from the prototype, when two edge groups
were envisaged. There is one plastic site, so every selected edge belongs to group 1 and `output`
is always 0. The names are kept because they appear in existing checkpoints and UI code; treat
`mushroom` as "selected edge count" and ignore `output`.

## Topology hash

The constructor folds the selected edges into an FNV-1a-32 hash, starting from 2166136261 and
consuming, for each slot in ascending edge order, the five values `edge`, `source`, `target`,
`weights[edge]`, `group`. The result is `state.topology`.

This hash is what makes a checkpoint safe against a dataset or selection change: it covers which
edges were chosen and what their measured weights were, which is why `preRole`, `postRole` and
`budget` are deliberately absent from the version string.

## Version string

```ts
plasticityVersion();                  // 'fly-kc-mbon-rstdp-v2'
plasticityVersion({ pairMs: 25 });    // 'rstdp-v2:<fnv1a32>'
```

`PLASTICITY_VERSION` is the pinned string `fly-kc-mbon-rstdp-v2`. `plasticityVersion(config)`
merges over the defaults and returns it when every rule parameter is default; otherwise it returns
`rstdp-v2:` plus an FNV-1a-32 hash of the parameters joined with `,`. Note the non-default prefix
is `rstdp-v2:`, not the full default string.

Hashed parameters, in this frozen order:

```
traceMs, pairMs, pairWindowMs, potentiation, depression, learningRate, restoring, minGain, maxGain
```

`preRole`, `postRole` and `budget` are excluded: they change which edges are selected, which the
topology hash already covers, and they are dataset-specific rather than rule-defining.

## Checkpoints

`exportState(): PlasticityState` writes `version`, `topology`, `enabled`, `updates`, `signal`, and
copies of `gains`, `traces` and `touched`.

`importState(state)` validates everything before mutating anything:

- No state at all resets: gains to 1, traces to 0, touched to 0, counters to 0.
- `state.version` and `state.topology` must both match, or
  `Incompatible plasticity topology/version`.
- Array lengths must match, or `Invalid plasticity dimensions`.
- `enabled` must be a boolean, `updates` a non-negative integer, `signal` finite with
  `abs(signal) <= 1`, or `Invalid plasticity metadata`.
- Per edge: `gain` finite with `abs(gain - 1) <= radius`, `trace` finite with `abs(trace) <= 1`,
  `touched` finite and non-negative, or `Invalid plasticity values`. The radius is
  `max(1 - minGain, maxGain - 1) + 1e-6`, which is 0.100001 for the default clamps: the widest
  legal displacement plus a Float32 tolerance.

## What this is not

Stated plainly, from `fly-plays-pokemon/docs/rewards-learning.md` and `README.md`:

- The scalar modulator is **synthetic**. It is whatever the embedding application computes and
  passes to `reinforce()`. It is not a fitted model of measured dopamine release.
- The plastic sites are **anatomical**, not fitted dopamine compartments. Kenyon cell to MBON is
  chosen because the connectome labels those populations, not because the rule was tuned against
  fly learning data.
- **PAM stimulation is not the causal learning signal.** `stimulate()` excites the `reward_pam`
  population and changes network activity; it writes no eligibility and no gain. A reward reaches
  learning only through `reinforce()`. The two calls happen to be triggered by the same game
  events in the prototype, which is a wiring choice by the adapter, not a mechanism.
- No claim is made that this rule is a quantitatively fitted fly learning mechanism, or that these
  reward weights improve play. See [limitations](limitations.md).
