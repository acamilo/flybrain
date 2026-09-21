# flybrain-core

The neural core of `packages/brain`, ported to Rust and bit-exact with it under the default
configuration: the dataset loader, the 1-ms LIF kernel, reward-modulated plasticity, the
population readout, the agent loop and the checkpoint envelope.

The TypeScript library stays the reference implementation and the oracle
(`docs/verification.md`). This crate is not allowed to be *close* to it.

## Layout

| Module | Ports |
| --- | --- |
| `dataset` | `dataset/format.ts`, `dataset/load-node.ts` |
| `rng` | `model/rng.ts` |
| `retina` | `model/retina.ts` |
| `lif` | `model/lif.ts` |
| `plasticity` | `model/plasticity.ts` |
| `decoder`, `decoder::gameboy` | `readout/decoder.ts`, `readout/presets/gameboy.ts` |
| `agent` | `agent/agent.ts` |
| `envelope` | `agent/envelope.ts` |
| `version` | `model/version.ts` |
| `jsmath`, `json`, `ordered` | JavaScript semantics the port needs (see below) |
| `bitset`, `pool` | nothing — the index and threading machinery the kernel's speed rests on |

## What bit-exactness required

Three classes of difference between Rust and JavaScript had to be handled explicitly. All three
are in `src/jsmath.rs`.

**Storage rounding.** JavaScript computes in `f64` and rounds to `f32` only when a value is stored
into a `Float32Array`. Every such store in the kernel is written as one `f64` expression with a
single `as f32`, which is the IEEE round-to-nearest-even a `Float32Array` store performs. Nothing
in the kernel computes in `f32`, and nothing uses `mul_add`: contracting
`membrane * decay + baseline` into an FMA would keep a wider intermediate and change the result.

**NaN handling.** `f64::min` and `f64::max` return the non-NaN operand; `Math.min` and `Math.max`
propagate NaN. `js_min` and `js_max` follow JavaScript, and are used for the membrane floor, the
trace clamp, the gain clamp and the retina's pixel clamp.

**Transcendentals.** `Math.exp` and `Math.tanh` in V8 are the fdlibm ports in
`src/base/ieee754.cc`, not the platform libm, and the difference is not cosmetic: a 1-ulp change in
`exp(-dt/20)` can move a Float32 gain, which moves a membrane, which moves a spike, after which the
two implementations have nothing in common. Measured against V8 (`golden/math.flygold`):

| Function | Arguments checked | `f64::_` (glibc) | `libm::_` | This crate |
| --- | --- | --- | --- | --- |
| `exp` | `exp(-k/5000)`, k in 0..=200,000 | 19,758 differ, 1 ulp | exact | exact (`libm::exp`) |
| `exp` | `exp(-dt/20)`, dt in 1..=100 | 10 differ, 1 ulp | exact | exact (`libm::exp`) |
| `tanh` | 1,400,000 seeded doubles | 7,095 differ, ≤ 3 ulp | 139,693 differ, ≤ 3 ulp | exact |

`libm::exp` is the MUSL port of the same fdlibm `__ieee754_exp` V8 carries, so `exp` comes straight
from `libm`. `libm::tanh` is a different algorithm and is in fact *further* from V8 than glibc's, so
`jsmath::tanh` transcribes fdlibm's `s_tanh.c` wrapper — which is a thin shell around `expm1`, and
`libm::expm1` *is* bit-identical to V8's. The golden test re-checks all of this on every run,
including a SHA-256 over `exp(-k/5000)` for k in 0..2,000,000.

Two smaller things in the same family:

- **`serde_json` needs its `float_roundtrip` feature.** Without it the fast float parser can land
  1 ulp away from `JSON.parse`, which first showed up as a bogus `fround(exp(-1/25))` mismatch
  before any kernel code was involved. It is pinned in the workspace manifest with a comment.
- **`JSON.stringify` is reimplemented** (`src/json.rs`) because the dataset fingerprint hashes
  `JSON.stringify(meta)`. `serde_json` writes `1.0` where JavaScript writes `1`, and key order has
  to survive the circuit-role merge. Parsing still goes through `serde_json` with `preserve_order`;
  only the writer and the number formatting (`ryu-js`) are ours.

## Observed divergence from the oracle

**None.** Every golden scenario compares with exact equality — integer arrays, RNG state,
spike times, membranes, refractory counters, traces, gains, touched timestamps, rates, digests and
per-frame button masks alike — and not one value differs by one ulp. The task this port was
commissioned under allowed a documented tolerance of up to 2 ulp for traces and gains if
transcendentals forced it; that allowance is unused, and the comparison helpers in
`tests/common/mod.rs` report ulp distance only so that a *future* failure says immediately whether
it is a rounding difference or a real divergence.

## Golden tests

`packages/brain/tools/golden.ts` writes `../../golden/*.flygold`; regenerate with:

```sh
npx tsx packages/brain/tools/golden.ts            # all of them
npx tsx packages/brain/tools/golden.ts platformer  # one, leaving the rest byte-identical
```

Golden files are the library's own checkpoint envelope with the magic `FLYGOLD1` — a JSON manifest
plus named binary chunks — so array values are carried verbatim rather than reformatted, the files
stay small, and every golden test also exercises the Rust envelope port.

| Scenario | Covers |
| --- | --- |
| `math` | the transcendental arguments the kernel produces, on their own |
| `toy` | the four-neuron fixture over 3,000 ms; every array compared entry by entry at three dumps |
| `agent` | the 4,096-neuron synthetic connectome through `NeuralAgent` for 600 frames; every button mask, digests at frames 200 and 400, full arrays at 600 |
| `real` | `data/fafb-v783` for 200 ms; the fingerprint, role order, plastic-edge selection, topology hash, per-millisecond spike counts, digests and 256 sampled values |
| `versions` | kernel and plasticity version strings for the default configuration and every individual numeric parameter |
| `platformer` | the platformer decoder preset over 4,000 quantized steps: every preset field, every button mask, and the final decoder state. Rates, clock, boot flags and clear points travel as chunks, so the Rust side decodes the oracle's input rather than a reconstruction of it |

Scenario fixtures travel inside the golden file — the toy connectome as manifest JSON, the
synthetic connectome as chunks — rather than being re-derived in Rust. Only the seeded RGBA frame
generator is reimplemented here, and each manifest carries a SHA-256 of its frame pool so a mistake
there fails as itself instead of as a mystery state divergence.

## Deterministic parallelism

**The rule: no result may depend on the thread count.** `golden_toy`, `golden_agent` and
`golden_real` each replay their whole scenario at several thread counts and byte-compare the
exported state against the sequential run, so this is a test and not an intention. `golden_real`
covers 1, 2, 3, 7 and 16 — an even split of the neuron range, two odd ones, and more workers than
the machine has cores — and `golden_toy`, whose fixture has four neurons, goes to 16 so that the
degenerate partition with empty shards is covered too.

Three phases of a tick are parallel. All three are partitioned into fixed contiguous index ranges
derived from nothing but the dataset and the worker count, and every argument below is about
*order*, not about locking — no value is ever written by two workers.

**The neuron sweep**, partitioned by neuron index. Each range writes its spikes into its own slice
of a scratch buffer and the slices are compacted in range order, so the spike array is the
sequential one for every thread count. Each neuron's update reads only its own membrane, refractory
counter and baseline.

**Spike propagation**, partitioned by *target* range. This is the one that needs an argument.

Each worker owns a disjoint, contiguous slice of `membrane` and walks the whole spike list,
applying the edges of each spiked source that land inside its own range. Claim: for every target,
the sequence of additions is exactly the sequential one.

- The sequential kernel visits `(source, edge)` in source-major order, and within a source in
  ascending CSR slot order.
- A worker visits the same spike list in the same order and the same rows in the same order, and
  applies a *subsequence* of that same edge sequence — the edges whose target is in its range. A
  subsequence preserves the relative order of every pair of elements it keeps.
- Restrict that subsequence to a single target `t`. It is the sequential sequence restricted to
  `t`: the same `(weight, gain)` pairs in the same order.
- `membrane[t]` depends on nothing else. Each step reads only `membrane[t]`'s previous value, adds
  `weights[e] * gain(e) * synapseScale`, applies the floor clamp and rounds once to f32. Same
  operands in the same order means the same bits at every step, including where the clamp bites.
- Targets in different shards never interact, and every target is in exactly one shard.

Note what the argument does *not* need: the shards need not be balanced, the worker count need not
be stable, and a source need not hit a given target at most once. It needs only that every worker
traverse the spike list and each row in the sequential order.

Two details ride on top of it and change nothing:

- The shard bounds are balanced by **in-degree**, not by neuron count, because in-degree runs from
  0 to 5,080 on `fafb-v783` and an even index split leaves one worker several times behind the
  rest. A different split only moves *which* worker applies an edge.
- When every CSR row is target-ascending — `fafb-v783`'s are; the synthetic golden fixtures' are
  not, so it is checked at plan time rather than assumed — a worker binary-searches its window in
  the row instead of filtering per edge, so each edge is read by exactly one worker rather than by
  all of them. Same edges, same order.

**`plasticity.observe`**, partitioned by *slot* range, on the same argument as propagation with
the slots in place of the targets.

Each worker owns a disjoint, contiguous run of plastic slots — their `traces` and `touched` entries
— and walks the whole spike list, writing only the pairings whose slot is in its own range. The
sequential walk visits the spike list in order and, per spiking neuron, its arriving slots and then
its leaving slots, each in ascending slot order; a shard writes a subsequence of that same write
sequence, which restricted to one slot is that slot's sequential sequence. A trace write depends on
nothing but that slot's own previous value and touch stamp, so the lazy decay, the `[-1, 1]` clamp
and the single rounding to f32 all happen at the same points with the same operands. One slot can be
written twice in a tick — once as its target's incoming pairing and once as its source's outgoing
one — and that order is inside one worker's range, so it is preserved too.

`lastSpikeMs` is read and never written during the phase (the kernel stamps this tick's spikes
after `observe` returns), so the shards need no ordering between them at all. Each per-neuron slot
list is built in ascending slot order, so a shard binary-searches its window in the list instead of
testing every slot; the lists that matter are the post-role neurons' and they run to thousands of
entries.

Everything else stays sequential in the original order: the 300 noise draws, the visual and
stimulation drive, the spike stamp, role counting and the rate EMAs.

The workers are a small pool of our own (`src/pool.rs`), not rayon and not a process-wide default,
so a host that embeds the core keeps control of its own threads. `SweepPlan::sequential()` and
`SweepPlan::with_threads(1)` build no pool at all and run every phase inline.

### Why not rayon, and why not a target-indexed edge list

Both were tried.

Rayon's per-tick `join` was the dominant cost rather than a rounding error on it: a 1-ms tick has
two short parallel phases, so a brain-second is two thousand fan-outs, and the sweep alone measured
152 us/tick at one thread and 559 at eight. The replacement is one thread per worker, spawned once,
parked on a generation counter — a dispatch publishes a `&dyn Fn(usize)`, runs index 0 on the
calling thread and drains a running count. Workers spin for about a microsecond and then yield
before parking on a condvar, because the gap between two phases is one sequential phase, a few
microseconds, and a futex round trip is a large fraction of that.

The plan of record was a target-indexed (CSC) edge list, so each worker could scan its own targets'
incoming edges and test each source against a spiked bitset. That is the wrong shape for this
kernel and was not built. It reads **every** one of the 2,700,513 incoming entries every tick —
21.6 MB of streaming per tick, 21.6 GB/s at real time — where the source-major walk reads only the
33,500 edges a spike actually crossed. It would also have cost 21.6 MB of resident memory and made
the single-threaded path several times slower, since one core cannot stream 21.6 MB in 456 us.
Sharding the source-major walk by target range gives the same disjoint-writes property with no
extra structure and no extra traffic: each edge is still read exactly once in total.

## Performance

Ryzen 7 5800X3D (8 cores / 16 threads), `data/fafb-v783`, warm-up 2,500 ms then 300 frames of noise
input through `NeuralAgent`. `brain ms / wall ms` is the real-time factor; 1.0 is break-even for a
live stream.

Measure it the way these numbers were measured — best of five rounds, pinned to one hardware thread
per physical core — because SMT siblings sharing a core make a thread-count sweep meaningless and a
single round on a loaded machine moves by 10%:

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release --examples
taskset -c 0,2,4,6,8,10,12,14 ./target/release/examples/bench
```

| threads | before | after | after, `x86-64-v2` |
| --- | --- | --- | --- |
| sequential | 1.426 | **1.896** | 1.848 |
| 1 | 1.294 | **1.889** | 1.844 |
| 2 | 1.411 | **2.571** | 2.455 |
| 4 | 1.362 | **3.301** | 3.226 |
| 6 | 1.284 | **3.551** | 3.439 |

"Before" is this crate at the merge of `feat/flysim-core`, measured the same way rather than quoted
from the table it used to print. The third column is the default cargo profile, which targets
`x86-64-v2` for the host's Haswell Xeon rather than the build host; it is the binary that actually
ships, and it costs 2-3%.

`cargo run --release --example ablate` prints where a tick goes. The split is read off
`LifNetwork::profile`, which laps an `Instant` between the five phases; five clock reads is about
125 ns against a tick of 150 us and up, and the tool prints the instrumented sum beside the
uninstrumented wall time so a reader can confirm they agree.

| us/tick | drive | sweep | observe | propagate | rates | sum | x real |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 thread | 3.2 | 169.3 | 79.5 | 288.2 | 1.4 | 541.5 | 1.85 |
| 2 threads | 4.6 | 89.3 | 79.8 | 228.7 | 1.4 | 403.8 | 2.48 |
| 4 threads | 5.4 | 48.9 | 82.4 | 178.7 | 1.4 | 316.8 | 3.24 |
| 6 threads | 5.7 | 35.8 | 83.6 | 164.9 | 1.4 | 291.4 | 3.37 |
| 8 threads | 6.4 | 31.9 | 91.2 | 165.6 | 1.6 | 296.7 | 3.35 |

What moved, in the order it was done, each step gated on the golden tests:

1. **`Plasticity::gain(edge)` stopped reading a 10.8 MB array.** It was a dense `Vec<i32>` of
   "plastic slot, or -1" with one entry per base edge, read once per propagated edge. A presence
   bitset (338 KB) plus a prefix-popcount table (169 KB) answers "not plastic" — almost every
   answer — from one word, and a rank in that set *is* the old slot number. Propagation 452 ->
   305 us/tick, sequential real time 1.43x -> 1.66x. Interleaving `targets` and `weights` into one
   8-byte array was measured too and made no difference against 10% run-to-run noise while costing
   21.6 MB, so it is not kept: a median row of 11 edges is one cache line of each either way.
2. **Propagation sharded by target range** (above). Correct but invisible until 3, because rayon's
   fan-out ate it.
3. **The persistent pool.** The change that mattered: 1.66x -> 1.77x sequential and 1.21x -> 2.47x
   at four threads, because the sweep finally scaled — 152 to 53 us/tick.
4. **The sweep blocked eight neurons at a time**, so the common case (no refractory neuron and no
   spike anywhere in the block, about three blocks in four) is branchless and vectorizes. Neutral
   at one thread and 27% better at eight, which is the useful finding: **at one thread the sweep is
   bandwidth-bound, not compute-bound.** It touches 1.25 MB of membrane, baseline and refractory
   per tick, all of it dirtied by the previous tick's scattered propagation, and 1.25 MB / 167 us
   is 7.5 GB/s, one core's share. In an edge-free network, where membrane stays cache-resident, the
   same change takes the sweep from 132 to 41 us/tick.
5. **`plasticity.observe` stopped calling `exp` 7,800 times a tick.** Instrumenting the loops
   showed 5,312 slot visits and 3,894 pairings per tick, two `exp` calls each — one for the
   spike-pair term, one for the trace's lazy decay — and every argument is an integer number of
   milliseconds over a fixed time constant, because `ms` is an integer tick counter and `touched`
   and `lastSpikeMs` only ever hold an `ms`. Tabulating them (101 entries for the pairing window,
   1,024 for trace gaps, each built with the identical expression) took observe from 128.5 to
   81.2 us/tick. This is a lookup that returns identical bits, not an approximation: a non-integer
   or out-of-range argument falls through to `exp`, and a unit test checks both paths bit for bit
   at three time constants.

   The per-neuron "has plastic edges" bit went in at the same time and bought nothing measurable on
   this dataset. The neurons that spike *and* carry plastic edges are the ones that spike often, so
   the test passes 218 times per tick out of 1,729 spikes and the loops behind it were the cost. It
   stays because it is free, and because a dataset with a less concentrated mushroom body needs it.

6. **`observe` sharded by slot range** (2026-09-16, `perf/simloop-main-thread`). The 84 us above
   was the last phase that did not move with the thread count, and by the time the sim loop was
   profiled as a whole it was a quarter of the sim thread's serial work
   (`infra/docs/simloop-profile.md`). Four threads pinned to four physical cores, `FLYSIM_ROUNDS=3`:

   | us/tick | drive | sweep | observe | propagate | rates | sum | x real |
   | --- | --- | --- | --- | --- | --- | --- | --- |
   | sequential `observe` | 5.4 | 47.8 | 80.2 | 178.0 | 1.4 | 312.9 | 3.21 |
   | sharded `observe` | 5.4 | 48.5 | **32.6** | 174.7 | 1.4 | **262.7** | **3.84** |

   Sharding it does cost something elsewhere in principle — every worker now touches `lastSpikeMs`
   and the trace arrays, so propagation's private-cache footprint grows — but it is below this
   box's run-to-run noise in both directions (propagation moved by -0.6 and +0.2 ms per game frame
   in two same-trajectory pairs). A compact `lastSpikeMs` (below) would shrink it either way.

### What is left

**One fan-out per tick instead of three.** A tick now dispatches the pool three times, and a
brain-second is 3,000 fan-outs; workers spin about a microsecond and then park, so every dispatch
that finds them parked costs the *dispatching* thread a futex wake per worker. On four whole cores
they stay spinning and the third dispatch is free, but on three cores shared with the service's own
tokio and store threads they park, and the third dispatch costs the sim thread about what the
shorter `observe` saves it (`infra/docs/simloop-profile.md` section 6). `observe` and propagation
can share one dispatch: they write disjoint arrays (`traces`/`touched` against `membrane`), neither
reads what the other writes, and propagation does not read `lastSpikeMs` at all — which is why the
spike stamp and the role tally already sit between the two phases rather than inside the edge walk.
Moving the stamp to after a fused dispatch puts a tick back to two fan-outs and changes no order:
the stamp writes the same `ms` per spiking neuron either way, and nothing in the fused phase reads
it.

Propagation is now three quarters of a four-thread tick and scales only 1.7x across four workers,
which is the next target: 33,500 scattered read-modify-writes into a 557 KB array per tick, under a
static in-degree split that assumes spiking is spread evenly across target ranges when it is not.
After that, `observe`'s remaining cost is memory rather than arithmetic — 5,312 random reads of a
1.1 MB `lastSpikeMs` plus scattered trace writes — and the fix would be a compact `lastSpikeMs`
over the 5,000 neurons that carry a plastic edge, which would shrink what every `observe` shard now
pulls into its own cache.

### Soak

`examples/soak.rs` runs 60 s of brain time per thread count in one unbroken run with no best-of,
and reports the slowest single frame and the resident set: a stream drops on its worst frame rather
than on its mean, and 16.74 ms is the frame budget.

```sh
taskset -c 0,2,4,6,8,10,12,14 ./target/release/examples/soak
```

| threads | brain ms / wall ms | frames/s | worst frame ms | RSS |
| --- | --- | --- | --- | --- |
| 1 | 1.847 | 110.3 | 14.11 | 39 MiB |
| 2 | 2.433 | 145.3 | 10.84 | 39 MiB |
| 4 | 3.101 | 185.2 | 7.48 | 39 MiB |
| 6 | 3.319 | 198.2 | 10.22 | 39 MiB |

The sustained figures sit 2-6% under the best-of-five ones and resident memory does not move, which
is what the run is there to show. 39 MiB is the whole process: the connectome is 16.2 MB of
`targets` and `weights` plus 557 KB of `indptr`, and the derived indices add 506 KB for the
plastic-edge bitset and 34 KB for the two per-neuron ones.

### The streaming gate

`docs/stream-mvp-plan.md`'s G0 gate asks for >= 1.0x on the host's six cores, and puts that
E5-2660 v3 at roughly half this box's single-thread speed. At 3.44x on six cores in the shipping
`x86-64-v2` profile, that projects to about 1.7x. The gate is met with room; the previous
measurement projected to 0.65x and did not meet it.

## Deviations from the TypeScript library

Each of these is a deliberate narrowing, not an accident.

- **Legacy decoder checkpoints are not supported.** `readout/decoder.ts` also accepts the
  prototype `MotorDecoder` states (versions `undefined`, 2 and 3). Nothing on the Rust side has
  ever written one, and the port was commissioned without them. `import_state` takes a version 4
  `DecoderState` and rejects anything else with `Invalid decoder version`.
- **Checks that the type system already makes are not repeated at runtime.** The oracle validates
  `typeof calibrated === 'boolean'`, `Number.isInteger(state.rng)` and similar, because a JavaScript
  caller can pass anything. Here `calibrated` is a `bool` and `rng` is an `i32`, so those cases are
  unrepresentable. Every check over a *value* — finiteness, ranges, lengths, array bounds, the
  plasticity radius, the fatigue interval, the remainder interval — is reproduced.
- **`ms` is an `f64`**, as it is in JavaScript, rather than a `u64` converted at each use. It is
  only ever incremented by one and compared or subtracted, and `ms - lastSpike` has to be the same
  `f64` subtraction the oracle performs.
- **Error messages are reproduced verbatim** rather than restructured into a Rust error taxonomy,
  because the oracle's tests match on them and they are part of the behaviour being ported. Hence
  the single `Error` newtype in `src/error.rs`.
- **`rates` and the decoder's records are insertion-ordered maps** (`src/ordered.rs`), because
  their key order is observable: it decides `JSON.stringify` order in a checkpoint manifest, and
  `rates` order is the network's tracked-role order.

## Build profile

`../../.cargo/config.toml` targets `x86-64-v2` by default, for the host's Haswell Xeon rather than the
build host. Rust performs no floating-point contraction or reassociation at any `target-cpu` level,
so this does not affect bit-exactness — which the golden tests confirm, since they run under it.
The three examples opt into the build host with `RUSTFLAGS="-C target-cpu=native"`; the Performance
table above reports both, and the difference is 2-3%.

The one codegen setting this crate does depend on is `inline(always)` on `propagate_shard`. Its
unsharded caller passes constant bounds, and only after inlining does the shard-window search fold
away; leaving that to the inliner's judgement measured 8% slower.
