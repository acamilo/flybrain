# Model

`LifNetwork` (`packages/brain/src/model/lif.ts`) is a leaky integrate-and-fire network over a
connectome dataset, stepped in integer 1-ms ticks. `step(n)` runs `n` ticks and returns the total
spike count. `FlyBrain` is a historical alias of the same class.

Construction takes the dataset and an optional partial config:

```ts
const network = new LifNetwork(dataset, { /* LifConfig fields */, plasticity: { /* ... */ } });
```

## Default configuration

`DEFAULT_LIF_CONFIG`, the original constants of the FAFB kernel:

| Field | Default | Meaning |
| --- | --- | --- |
| `decayMs` | 20 | membrane decay time constant, ms |
| `threshold` | 1 | spike threshold, membrane units |
| `refractoryMs` | 2 | ticks a neuron stays refractory after spiking |
| `synapseScale` | 0.005 | multiplier on dataset weights when a spike propagates |
| `baselineMax` | 0.06 | upper bound of the per-neuron random baseline drive |
| `noiseKicks` | 300 | random membrane kicks per tick |
| `noiseAmount` | 0.42 | membrane increment per noise kick |
| `rateAlpha` | 1/25 | EMA coefficient for rate estimates |
| `membraneFloor` | -2 | lower clamp on the membrane after inhibitory input |
| `seed` | 22222 | seed of the noise generator |
| `stimulation.role` | `reward_pam` | role driven by `stimulate()` |
| `stimulation.drive` | 0.20 | drive added to that role per tick while a pulse runs |
| `retina.gain` | 0.20 | membrane drive per unit luminance |
| `retina.width` | 160 | default frame width assumed by `setVisualFrame()` |
| `retina.height` | 144 | default frame height |

Derived once in the constructor:

- `decay = Math.fround(Math.exp(-1 / decayMs))`, which is `0.951229453086853` for the default
  20 ms. The `fround` is part of the contract: the original kernel stored the decay in a Float32
  and the oracle tests compare bit for bit.
- `baseline[i] = rng.next() * baselineMax` for every neuron in index order, drawn from the same
  xorshift stream the noise later uses. The baseline is fixed for the network's life and is not
  part of exported state; it is reproduced from `seed`.

## The 1-ms kernel

`stepOne()` in this exact order. Order matters: it is what the bit-exact oracle tests pin.

1. **Noise.** `noiseKicks` times: `membrane[rng.nextUint() % neurons] += noiseAmount`. Indices
   can repeat, so 300 kicks are 300 draws and not 300 distinct neurons.
2. **Visual drive.** For every retina column `i`: `membrane[visualIndices[i]] += visualDrive[i]`.
   `visualDrive` is zero until `setVisualFrame()` is called, so a warm-up without a framebuffer
   is well defined.
3. **Stimulation.** If `rewardRemaining > 0`, add `stimulation.drive` to every neuron of
   `stimulation.role`, then decrement `rewardRemaining`.
4. **Integrate and fire**, sweeping neurons in ascending index order:
   - if `refractory[i] > 0`: decrement it, apply `membrane[i] *= decay`, and skip the rest;
   - otherwise `voltage = membrane[i] * decay + baseline[i]`;
   - if `voltage >= threshold`: set `membrane[i] = 0`, `refractory[i] = refractoryMs`, and record
     the spike;
   - else `membrane[i] = voltage`.

   A refractory neuron therefore still leaks but receives no baseline drive, and a spiking neuron
   resets to exactly 0 rather than subtracting the threshold.
5. **Observe pairs.** `plasticity.observe(spikes, spikeCount, lastSpikeMs, ms)` runs *before*
   `lastSpikeMs` is updated for this tick, so a pair needs a strictly positive `dt` and
   simultaneous spikes never pair. See [plasticity](plasticity.md).
6. **Propagate.** For each spiking neuron `s`, in the order it was recorded (ascending index):
   set `lastSpikeMs[s] = ms`, accumulate its role counts, then for each CSR slot `e` of `s`:

   ```
   membrane[targets[e]] = max(membraneFloor, membrane[targets[e]] + weights[e] * gain(e) * synapseScale)
   ```

   `gain(e)` is 1 for every non-plastic edge. The floor is applied per edge, not once per tick, so
   the clamp order is part of the numerics.
7. **Rate EMAs.** For each tracked role: `instantaneous = roleCount * 1000 / roleSize` spikes per
   second (0 when the role is empty), then
   `rates[role] += (instantaneous - rates[role]) * rateAlpha`. Then
   `populationRate += (spikeCount * 1000 / neurons - populationRate) * rateAlpha`.
8. `ms++`.

## Retina projection

`projectFrame()` (`model/retina.ts`) writes one drive value per column from one RGBA frame of any
size. `setVisualFrame(rgba, width, height)` calls it with the dataset's column arrays and
`retina.gain`; the size defaults to `retina.width` and `retina.height`.

Per call, the column bounding box is recomputed over `count` columns. Recomputing it every frame
is redundant in practice (columns are immutable) but reproduces the original kernel's arithmetic.

For column `i`:

```
normalizedX = (xy[2i]     - minX) / (maxX - minX || 1)
if (hemisphere[i] === 0) normalizedX = 1 - normalizedX
normalizedY = (xy[2i + 1] - minY) / (maxY - minY || 1)
x = clamp(round(normalizedX * (width  - 1)), 0, width  - 1)
y = clamp(round(normalizedY * (height - 1)), 0, height - 1)
offset = (y * width + x) * 4
luminance = (rgba[offset] * 0.2126 + rgba[offset+1] * 0.7152 + rgba[offset+2] * 0.0722) / 255
out[i] = luminance * gain
```

- Normalization is to `[0, 1]` against the column bounding box, with a `|| 1` guard so a
  degenerate axis maps to 0 instead of NaN.
- Hemisphere 0 (left) is mirrored on X. Hemisphere 1 is not.
- Sampling is nearest neighbour with `Math.round`, then clamped to the last pixel index. There is
  no filtering and no averaging, so one column reads exactly one pixel.
- Luminance uses Rec. 709 weights (0.2126, 0.7152, 0.0722) and divides by 255, giving `[0, 1]`.
  The alpha channel is ignored.
- Drive is `luminance * gain`, so the default gain of 0.20 puts a white pixel at 0.20 membrane
  units per tick.

The projection is resolution independent: a 320x288 frame maps to the same relative pixels as a
160x144 one, which `packages/brain/tests/model.test.ts` asserts.

## Stimulation pulse

```ts
network.stimulate(durationMs = 120); // reward() is a historical alias
```

`rewardRemaining = max(rewardRemaining, durationMs)`, so overlapping pulses take the maximum
rather than summing. While it is positive, step 3 of each tick adds `stimulation.drive` to every
neuron in `stimulation.role` and decrements the counter by one. The prototype used 80 to 400 ms
depending on reward kind (`fly-plays-pokemon/docs/rewards-learning.md`, "Plasticity").

This pulse is drive, not learning. It does not touch eligibility traces or gains.

## Rate estimates

Which roles are tracked is `config.rateRoles`. When it is absent, the default is every role whose
name starts with `command_`, plus `steer_left`, `steer_right`, `forward`, `backward`,
`proboscis`, `reward_pam`, taken in dataset role order. Names not present in the dataset are
dropped. For `data/fafb-v783` that is 14 roles.

Role membership is packed one bit per role into a `Uint32Array`, one word per neuron, so at most
`MAX_RATE_ROLES` = 32 roles can be tracked. Exceeding that throws at construction:
`Too many tracked rate roles (N); at most 32 fit the role bitmask`. A caller that needs more
roles has to run a second network or narrow `rateRoles`.

`rates` is a `role -> spikes/s` record; `populationRate` is the same EMA over the whole network.
Both start at 0, so early ticks read low. That is what the warm-up before `calibrate()` is for.

## RNG

`Xorshift32` (`model/rng.ts`) is bit-exact with the original inline kernel generator:

```
value ^= value << 13;
value ^= value >>> 17;
value ^= value << 5;
```

`nextUint()` returns `value >>> 0`; `next()` returns `nextUint() / 2**32`, so draws are in
`[0, 1)` with 2^-32 resolution. The internal state is stored raw and never coerced on assignment,
because checkpoints store the signed value verbatim and the shift operators already do the int32
coercion, exactly as the original code did.

One stream serves both the baseline draws at construction and the per-tick noise, so the noise
sequence depends on the neuron count as well as the seed.

## State export and import

`exportState(): LifState` copies `membrane` (Float32Array), `refractory` (Uint8Array),
`lastSpikeMs` (Float64Array), `visualDrive` (Float32Array), `rng` (number), `rewardRemaining`,
`ms`, `populationRate`, a copy of `rates`, and the nested plasticity state. `baseline`, role masks
and the config are not exported: they are rebuilt from the dataset and the config.

`importState(state)` imports plasticity first, then validates before writing anything:

- `membrane`, `refractory` and `lastSpikeMs` lengths must match the loaded dataset, otherwise
  `Brain checkpoint dimensions do not match the loaded dataset`.
- `visualDrive.length` must equal the retina column count; `ms` must be a safe non-negative
  integer; `rng` must be an integer; `populationRate` must be finite; `rewardRemaining` must be
  finite and non-negative; every value in `rates` must be finite; every value in `membrane`,
  `lastSpikeMs` and `visualDrive` must be finite. Otherwise
  `Invalid neural checkpoint values`.

Missing rate roles import as 0. `lastSpikeMs` is initialized to -1,000,000 in a fresh network, so
a neuron that has never fired is far outside every pairing window.

## Version strings

```ts
kernelVersion();                    // 'lif-1ms-f64-v2'
kernelVersion({ decayMs: 25 });     // 'lif-1ms-f64-v2:<fnv1a32>'
```

`NEURAL_KERNEL_VERSION` is the pinned string `lif-1ms-f64-v2`, kept verbatim so prototype
checkpoints stay loadable. `kernelVersion(config)` merges the partial config over the defaults and
returns that string when every numeric parameter equals its default; otherwise it returns
`lif-1ms-f64-v2:` plus an FNV-1a-32 hash (eight lowercase hex digits) of the parameters joined
with `,` (`model/version.ts`).

The hashed parameters, in this frozen order:

```
decayMs, threshold, refractoryMs, synapseScale, baselineMax, noiseKicks, noiseAmount,
rateAlpha, membraneFloor, seed, stimulation.drive, retina.gain, retina.width, retina.height
```

Role names never enter the hash, including `stimulation.role` and `rateRoles`. They change which
neurons are involved, not the arithmetic, and the dataset fingerprint already covers the role
lists. Parameter order is part of the version contract: reordering it would invalidate every
non-default checkpoint.

`network.version` is the string for the instance's own config. Put it in the checkpoint
compatibility string (see [integration](integration.md)). Changing kernel semantics without
changing a numeric parameter requires bumping `NEURAL_KERNEL_VERSION` by hand.

## Float32 versus Float64

| State | Type | Why |
| --- | --- | --- |
| `membrane` | Float32 | 139,255 entries stepped every tick; the original kernel's precision |
| `baseline` | Float32 | same, and derived from the seed rather than stored |
| `decay` | Float32 value in a Float64 slot | `Math.fround` reproduces the original constant |
| `refractory` | Uint8 | small integer counter, 2 by default |
| `lastSpikeMs` | Float64 | 1-ms differences must survive a long session |
| `plasticity.touched` | Float64 | same, for eligibility timestamps |
| `plasticity.gains`, `plasticity.traces` | Float32 | 16,384 entries, and the change threshold in `statistics()` is 1e-6 |
| `ms`, `rates`, `populationRate` | Float64 (plain numbers) | `ms` is an exact integer count of ticks |

The original doc's note still applies: only viewer timestamps convert to Float32, so long-run
display precision can degrade without changing learning
(`fly-plays-pokemon/docs/architecture.md`, "Dataset and neural state").
