> **Contract override (read this first).** The binding feed protocol is
> `docs/feed-protocol.md`: RGBA frame attachment, f32 stereo 48 kHz audio
> converted from binjgb's u8 samples, spikes as a `ceil(n/8)`-byte bitset, the header fields exactly
> as listed there, port **7400** for the feed and **7401** for control. The binding control API is
> `docs/control-api.md`. Where sections 6 and 7 below differ from those two
> documents, **the documents win** (in particular: the 2bpp indexed video proposal, the u8 audio
> passthrough, the adaptive index-list-versus-bitset spike encoding, the single-port axum layout and
> the single `7390` port in section 9 are all superseded).
>
> **Also binding, from the infra design:** durable checkpoints go to disk every **300 s**, with a hot
> copy every **5 s** to a tmpfs path (SSD endurance). Both intervals and both paths are configurable.
> Section 8's "every 5 s to disk" cadence is superseded accordingly; the atomicity, manifest,
> archive, restore-order and compatibility rules in section 8 still stand, and the restore order gains
> the tmpfs hot copy as the first candidate ahead of the durable latest.

---

# flysim: implementation plan

A multithreaded Rust service that ports the neural core of `~/flybrain/packages/brain` plus the
Pokemon Red game layer of `~/fly-plays-pokemon`, keeping the TypeScript library as the bit-exact
oracle. Binding context: decisions 1-10 of `the operator's own planning notes`
(service, not browser; Rust; emulator inside `flysim`; sugar-only viewer influence; no button
endpoint).

## 1. Workspace and crate layout

New self-contained Cargo workspace at `~/flybrain/services/flysim`. The monorepo root is npm
workspaces with the glob `packages/*`, so `services/flysim` is invisible to npm and needs no
`package.json`. Add a `services/` row to the README layout table only.

```
services/flysim/
  Cargo.toml              workspace, resolver = "3", members = crates/*, [workspace.dependencies]
  Cargo.lock              committed
  rust-toolchain.toml     channel = "1.94.1", components = rustfmt, clippy
  .cargo/config.toml      rustflags: -C target-cpu=x86-64-v2 (portable across WSL and Haswell)
  flysim.toml.example
  vendor/binjgb/          git submodule pinned to c60e138da5a795ebb55e56b11b7e90024e41112c
  crates/flybrain-core/   dataset, rng, retina, lif, plasticity, decoder, gameboy preset,
                          agent, envelope, version, jsnum, math, parallel
  crates/flybrain-gb/     binjgb FFI + shim, reward catalog/symbols/pokemon-red, ratchet, recovery,
                          compatibility
  crates/flysim/          binary: sim loop, pacing, snapshot feed, control API, store, event log,
                          metrics
```

`flybrain-core` mirrors the TS module tree one file per module so a reviewer can diff them side by
side. It has no knowledge of Game Boys beyond `presets/gameboy.rs` (the TS library's own split).
`flybrain-gb` owns everything ROM specific. `flysim` owns wall-clock time, sockets and disk.

Pinned dependencies (exact patch versions chosen at implementation time with a 30-day minimum age,
mirroring `fly-plays-pokemon/tools/check_package_age.mjs`; add `services/flysim/tools/check_crate_age.mjs`
against the crates.io API and run it in CI):

| Crate | Where | Why |
| --- | --- | --- |
| `serde` 1.0, `serde_json` 1.0 (feature `preserve_order`) | core, gb, bin | meta.json key order matters for the fingerprint |
| `flate2` 1.1 (`rust_backend`) | core | the `.binz` artifacts are raw gzip |
| `sha2` 0.10 | core, gb | dataset fingerprint, ROM hash |
| `crc32fast` 1.5 | core | envelope footer |
| ~~`rayon` 1.11~~ | ~~core~~ | dropped: a persistent pool of std threads replaced it, because a join per 1-ms tick cost more than the parallelism bought. `flybrain-core/src/pool.rs`, and the crate README for the measurement |
| `libm` 0.2 | core | `exp`/`tanh` independent of the system libm (see section 2) |
| `indexmap` 2 | core | ordered role maps (pulled in by `preserve_order`) |
| `thiserror` 2.0 | core, gb | typed errors |
| `cc` 1.2, `bindgen` 0.72 | gb build-dep | compile binjgb C, generate `emulator.h` bindings |
| `tokio` 1.4x (`rt-multi-thread`, `net`, `sync`, `time`, `signal`) | bin | feed and API |
| `axum` 0.8 (feature `ws`) | bin | HTTP control and the WebSocket feed |
| `toml` 0.9, `anyhow` 1.0, `tracing` 0.1, `tracing-subscriber` 0.3, `hex` 0.4 | bin | config, logs |
| `criterion` 0.7 | core dev-dep | sweep benchmark for M2 |

`axum`'s built-in upgrade removes the need for `tungstenite`/`tokio-tungstenite`. Fallback if we
want to drop async entirely: `tiny_http` 0.12 plus `tungstenite` 0.28 on two std threads. Recorded
as a fallback, not the default. (Note the override: the feed and control listeners are on two
separate ports, 7400 and 7401.)

## 2. Bit-exactness against the TypeScript oracle

The contract: integer paths, the RNG, spike decisions, decoder decisions and checkpoint bytes are
**exactly** equal; only values that pass through `exp`/`tanh` may fall back to a measured tolerance,
and only if measurement shows a divergence.

Every place the TS mixes f32 storage with f64 arithmetic, and the Rust equivalent. `as f32` in Rust
is IEEE round-to-nearest-even, which is exactly what a `Float32Array` store does, so a single
`as f32` at the end of an f64 expression reproduces JS exactly.

| TS site | JS semantics | Rust |
| --- | --- | --- |
| `decay = Math.fround(Math.exp(-1/decayMs))` (`lif.ts:120`) | f64 exp, rounded to f32, held in an f64 slot | `let decay: f64 = (libm::exp(-1.0/decay_ms) as f32) as f64;` assert it equals `0.951229453086853` for the default |
| `baseline[i] = rng.next() * baselineMax` | f64 product stored to f32 | `(((raw as f64) / 4294967296.0) * 0.06) as f32` |
| `membrane[idx] += noiseAmount` | f32 read promoted, f64 add with the f64 literal `0.42`, f32 store | `m[i] = ((m[i] as f64) + 0.42) as f32`; not an f32 add, because `0.42f32 != 0.42f64` |
| `membrane[visualIndices[i]] += visualDrive[i]` | both f32, added in f64, stored f32 | `((m[t] as f64) + (drive[i] as f64)) as f32` (operands are f32-exact so the double rounding is provably innocuous, but write it in f64 anyway) |
| `membrane[n] += stimulation.drive` | f64 literal `0.20` | same pattern as noise |
| `voltage = membrane[i] * decay + baseline[i]` | product of two f32 is exact in f64, then one f64 add, then one f32 store: effectively a fused multiply-add with a final single rounding | `let v = (m[i] as f64) * decay + (base[i] as f64); ... m[i] = v as f32;` Do **not** use `f32::mul_add` or f32 arithmetic here: they give different bits |
| `membrane[i] *= decay` (refractory branch) | exact f64 product, one f32 store | `m[i] = ((m[i] as f64) * decay) as f32` (identical to an f32 multiply here; kept in f64 for uniformity) |
| `max(floor, membrane[t] + weights[e] * gain(e) * synapseScale)` | Int16 to f64, f32 gain to f64, left-to-right `(w*gain)*0.005`, f64 add, `Math.max`, f32 store, **per edge** | `let d = ((w as f64) * (g as f64)) * 0.005; let v = (m[t] as f64) + d; m[t] = js_max(-2.0, v) as f32;` with `js_max` returning NaN if either operand is NaN (Rust's `f64::max` does not) |
| `lastSpikeMs` | `Float64Array`, init `-1_000_000` | `Vec<f64>`; keep f64, not i64, so the checkpoint chunk stays byte-compatible |
| `traces[slot] = clamp(traces[slot]*decay + pair, -1, 1)` | f32 read, f64 arithmetic with two `Math.exp` results, f32 store | `((t as f64) * decay + pair).clamp(-1.0, 1.0) as f32` with `clamp` written as nested `Math.min`/`Math.max` order |
| `gains[slot] = clamp(old + lr*signal*trace - restoring*(old-1))` | f64 with left-to-right grouping, f32 store | transcribe the parenthesisation literally |
| `rates[r] += (inst - rates[r]) * rateAlpha`, `populationRate` | plain f64 numbers | f64; write `1.0/25.0`, and `(count as f64) * 1000.0 / (size as f64)` |
| retina luminance | `(r*0.2126 + g*0.7152 + b*0.0722)/255` in f64, `* gain`, f32 store | keep the exact association order; one `as f32` at the end |
| retina normalisation | `(xy - min) / (max - min || 1)`, `Math.round`, clamp | `||1` means "if the span is 0 or NaN use 1"; arguments to `round` are non-negative, so `f64::round` matches `Math.round` here |
| `Xorshift32` | `^= value << 13`, `^= value >>> 17`, `^= value << 5` with int32 coercion by the shift operators | `u32` with wrapping shifts: bit-identical. No `Math.imul` anywhere in the RNG. Checkpoints store the **signed** state, so export `state as i32` and import with JS `ToUint32` semantics (`(v as i64).rem_euclid(1<<32) as u32`) |
| `Math.imul(hash ^ value, 16777619) >>> 0` (topology hash, FNV) | int32 multiply | `u32::wrapping_mul`, with `hash ^ (weight as i32 as u32)` so negative Int16 weights sign-extend as JS does |
| `versionFor(...)` FNV over `params.join(',')` | hashes a JS number-formatted string | flysim pins the default configs and returns the frozen strings `lif-1ms-f64-v2` and `fly-kc-mbon-rstdp-v2`; any non-default numeric parameter is a hard startup error rather than a guess at V8 number formatting |

Transcendentals. V8's `Math.exp` and `Math.tanh` come from its own fdlibm port
(`src/base/ieee754.cc`) and are accurate to under 1 ulp, not correctly rounded. Rust's
`f64::exp` calls the system libm (glibc here), which uses a different algorithm. **They are not
guaranteed to agree.** Strategy, in order:

1. Use the `libm` crate rather than `std`. It is a port of musl, which derives from the same
   FreeBSD msun/fdlibm sources V8 ported, so bit-identity is likely and, more importantly, it is
   independent of the host glibc version, so flysim gives the same answer on the WSL box and in the
   container.
2. Exploit the fact that the argument domains are **discrete**. `exp(-dt/pairMs)` has dt an integer
   in 1..100 (brain ms is an integer tick counter and `lastSpikeMs` holds integers), so there are
   exactly 100 possible arguments: precompute and verify them against a TS dump. The trace decay
   `exp(-max(0, ms - touched)/5000)` also has an integer numerator, so a golden test can hash the
   f64 bits of `exp(-k/5000)` for k in 0..2,000,000 on both sides and compare one digest.
3. `tanh(R)` has a small realistic domain (sums of the seven catalog values). Dump the set produced
   by a long TS run plus 4,096 fuzzed sums and compare bit for bit.
4. If any mismatch survives: vendor a direct Rust transcription of V8's `ieee754.cc` `exp`/`tanh`
   into `flybrain-core/src/math.rs`, gated behind no feature flag (it becomes the implementation).
5. Only if that is somehow still divergent: declare traces, gains and `signal` tolerance-compared
   (relative 1e-7, the f32 epsilon scale) in the golden tests while keeping spikes, RNG, `ms`,
   membrane, refractory, decoder output and button masks exact. Record the decision in
   `docs/verification.md` if it is ever used.

Codegen hygiene: rustc never contracts `a*b + c` into an FMA and never enables fast-math, so
`-C target-cpu` is safe for bit-exactness. A test asserts `decay`, one `voltage` and one propagation
step against hardcoded bit patterns so a future codegen change cannot silently drift.

## 3. Deterministic parallelism

The rule: the result must not depend on the thread count. A test runs the same 500 ms with pools of
1, 2, 3, 6 and 16 threads and byte-compares the exported state.

> **As built** (`feat/flysim-perf`): the propagation fallback below became the default, the bitset
> at the end of this list shipped, rayon is gone, and `observe` was left sequential because its cost
> turned out to be `exp` calls rather than the walk. `flybrain-core/README.md` carries the
> determinism argument for target-sharded propagation and the measurements behind each choice,
> including why the target-indexed (CSC) edge list this section reaches for is the wrong shape.
>
> **As built** (`perf/simloop-main-thread`): `observe` is sharded by slot range after all, exactly
> as this section planned it. Tabulating the `exp` calls left a walk that was still 80 us of a
> 313 us tick at four threads and did not move with the thread count, which by then was a quarter
> of the sim thread's serial work. `infra/docs/simloop-profile.md` is the measurement.

- **Noise kicks** (300 per ms): sequential. The RNG is a state chain, indices may repeat, and a
  repeat is order-sensitive in f32. Cost is negligible.
- **Visual drive** (1,572 columns) and **stimulation** (307 neurons): sequential, in index order.
  Two columns can share a neuron, so order is observable.
- **Integrate and fire sweep** (139,255 neurons): rayon, partitioned by **index ranges** with a
  fixed chunk count derived from the pool size (not from rayon's work stealing). Each chunk owns its
  slice of `membrane`/`refractory` exclusively, appends to a per-chunk spike `Vec`, and the chunks
  are concatenated in chunk order, which reproduces the ascending-index spike list exactly. Use
  `par_chunks_mut` over zipped slices so the borrow checker proves disjointness; `baseline` is
  shared read-only.
- **Spike propagation**: default to **single-threaded in source order**, exactly as the TS does.
  Rationale: the per-target `f32` accumulate-and-clamp sequence is order dependent, and propagation
  is roughly 20k edge updates per ms against 139k neuron updates, so it is not the bottleneck. If
  M2 measurement says otherwise, switch to **partition by target range**: every thread walks the
  same global spike list in order and applies only the edges whose target falls in its range, which
  preserves the per-target addition order exactly at the cost of re-reading the edge arrays per
  thread. Both variants are implemented behind one function so the determinism test covers them.
- **Role counting**: per-chunk `u32` counters summed at the end (integer addition is
  order-independent). `lastSpikeMs[source] = ms` is a conflict-free scatter.
- **Plasticity `observe`**: partition **by slot range**. Each thread walks the spike list in order,
  computes both the incoming and the outgoing candidate pairs for each spiking neuron, and applies
  only the ones whose slot is in its range. A slot can be touched twice in one tick (as its target's
  incoming and its source's outgoing), and this scheme preserves that order. Given a few hundred
  spiking Kenyon/MBON cells per tick this stays cheap; start sequential and parallelise only if the
  benchmark shows it matters. *As built:* sequential first, then sharded exactly as written here
  once the benchmark did show it (`plasticity::observe_shard` carries the order argument, and the
  per-neuron slot lists are ascending so a shard binary-searches its window).
- **Plasticity `reinforce`**: 16,384 independent slots, `par_chunks_mut` by slot range, exact.
- Memory optimisation that changes no bits: replace the 2.7M-entry `Int32Array slots` (10.8 MB) with
  a 2.7M-bit presence bitset (338 KB) plus a rank table, so `gain(edge)` stops evicting the membrane
  arrays from L2. Gated by a test proving identical output.

## 4. Threading and timing

- One **sim thread** owns the agent, the emulator, the reward adapter and the ratchet. Nothing else
  touches them. Rayon is used only inside the kernel calls.
- Rayon pool: `max(1, available_parallelism() - 2)`, configurable, named threads, built explicitly
  so no global pool leaks in. The two spare cores are for Chromium and ffmpeg in the same container.
- Tokio runtime: 2 worker threads for axum, the WebSocket feed and the checkpoint writer.
- Cross-thread plumbing: a bounded `mpsc` of control commands drained at exactly one point per
  frame (immediately before the brain step) so command application is deterministic and the tick
  index can be recorded in the event log; a `tokio::sync::watch<Arc<Snapshot>>` for the feed so a
  slow client is dropped-oldest and can never stall the sim.
- Frame cadence: `MS_PER_FRAME = 1000.0 / (4_194_304.0 / 70_224.0)`, the fractional remainder
  accumulated in the agent and checkpointed, `floor` giving 16 or 17 integer ticks per frame. This
  is the same accumulator the TS `NeuralAgent` keeps, and the golden test pins it.
- Pacing: absolute deadlines (`next += period / speed`), never `sleep(period)`, so sleep jitter does
  not accumulate. Sleep to 1.0x by default. `speed` is configurable in 0.25..8 plus `0` meaning
  unthrottled (soak tests).
- Faster than real time: sleep the difference. Slower: **no frame skipping ever** (brain time and
  game time are one clock); accumulate `lag_seconds`, warn once past 1 s and then every 30 s, and
  expose it.
- Real-time factor: simulated ms over wall ms on a 1 s window and a 60 s window, plus the emulator
  fps, in `/status`, `/metrics` and the feed header.
- Where a frame goes: `FLY_PROFILE_SECONDS=N` logs one per-phase table every N seconds — mean and
  worst ms per game frame for each loop phase, with the kernel's own five phases folded in from
  `LifNetwork::profile` (`crates/flysim/src/profile.rs`). Off by default and one `bool` test per lap
  when off, so it ships enabled-able rather than as a patch someone has to re-derive.
- Loop order, pinned to the prototype worker (`simulation.worker.ts:106-145`) rather than to
  `agent.tick`'s argument order, because the prototype samples reward inside the same frame:
  drain commands, step brain (16 or 17 ms), decode, apply buttons, run one emulator frame, set
  visual frame, sample reward, stimulate, reinforce, ratchet observe (possibly recover), publish.
  The golden agent test (section 5, G6) pins this order. As of v0.1.1 the decode step also passes
  the readout's blocked-direction input, which the loop computes from the adapter's position and
  the channel the group is holding (`docs/readout.md`); it is still the decoder that chooses every
  button.

## 5. Golden tests

New `packages/brain/tools/golden.ts` (run with `npx tsx`, no new TS dependency) writes cases as
`encodeEnvelope`-format files with magic `FLYGOLD1` into `packages/brain/tests/golden/`. Small
cases (toy, synthetic, decoder, rng) are committed; real-dataset blobs are gitignored and
regenerated by `npm run golden`, with only their SHA-256 digests committed, so the repo does not
gain 20 MB. Rust tests byte-compare the blob when present and otherwise compare digests, and skip
with an explicit message when a file or the dataset is absent (the same convention the TS suite
already uses).

| Case | Mirrors | Content |
| --- | --- | --- |
| G1 rng | `model.test.ts` Xorshift32 | 4,096 draws from seeds 22222 and 1, final state |
| G2 retina | `model.test.ts` projectFrame | 3 seeded frames at 160x144 and 320x288, real and toy columns |
| G3 toy kernel | `model.test.ts` LifNetwork oracle | 3,000 ms with a frame, a 120 ms pulse and a reinforce; full state |
| G4 plasticity | `plasticity.test.ts` | 5 s eligibility decay, anti-causal pairing, export/import continuation, clamp edges, disabled-plasticity no-op |
| G5 decoder | `readout.test.ts` oracle | 20,000 steps of driven rates through `gameboyDecoderConfig()`, masks per step plus final state, plus the legacy v2/v3 import cases |
| G6 agent | `agent.test.ts` 600-frame oracle | the synthetic 4,096-neuron connectome (emitted as artifacts, not regenerated in Rust), 600 frames of rewards/boot/learn, mask per frame, final state |
| G7 real dataset | `model.test.ts` + `agent.test.ts` FAFB runs | fingerprint string, plastic edge list digest, topology hash, role name order, 200 ms with frame and pulse, full-state digest |
| G8 envelope | `agent.test.ts` envelope tests | a TS-written checkpoint that Rust decodes, imports and re-encodes byte-identically, plus the corruption/truncation/trailing-byte rejections |
| G9 transcendentals | new | `fround(exp(-1/20))`, `exp(-k/20)` for k in 1..100, a digest of `exp(-k/5000)` for k in 0..2e6, `tanh` over the catalog sum set plus 4,096 fuzzed sums |
| G10 reward adapter | ported from `fly-plays-pokemon/tests` | synthetic WRAM traces as JSONL (address to byte per sample) with expected event lists: boot/import baselines, bitset novelty, map gates, oscillation versus coverage, per-map cap, wild run/capture versus observed KO, single-faint non-penalty |

G7 also covers the fingerprint, which is the one place Rust must reproduce V8's
`JSON.stringify(meta)` byte for byte. Verified: both JSON files are ASCII, integer-only and already
compact, so `serde_json` with `preserve_order` plus a merge that updates existing keys in place and
appends new ones (JS `Object.assign` semantics) reproduces it. Merged role order is
`backward, command_0..7, descending, forward, motor, proboscis, reward_pam, steer_left, steer_right, visual_l1, sensory, kenyon, mbon`,
and the 14 default rate roles take their bit positions from that order.

## 6. Snapshot feed protocol

**Superseded by `docs/feed-protocol.md` (see the override note at the top).**
Retained below as the original reasoning and the bandwidth arithmetic.

One WebSocket at `ws://127.0.0.1:<port>/feed`, 30 Hz, one binary message per snapshot:
`u32 headerLen` then UTF-8 JSON, then length-prefixed attachments in a fixed order.

Header: `{ seq, ms, frame, wallMs, buttons, rates{14 roles}, populationRate, spikeCount,
rewardStats{counts, total, recent[8], mode, uniqueTiles}, learning{updates, changed, meanChange,
maxChange, signal, enabled}, rank, stuckSeconds, recoveries, realtimeFactor, speed, mode, status,
events[] (since the last snapshot, from the event log), sugarQueue[{viewer, source, at}],
attachments[{kind, len}] }`.

Attachments:

1. **Video**: 2bpp indexed, 160x144 packed row-major, 5,760 bytes. binjgb emits RGBA u32 with the
   four DMG palette entries, so the sim maps each word to 0..3 and sends a `palette[4]` RGBA array
   in the header; if a word falls outside the palette (CGB or SGB content) it falls back to raw
   RGBA (92,160 bytes) and says so in `attachments[].kind`. (The binding doc chooses the RGBA
   attachment unconditionally: 92,160 bytes, about 2.76 MB/s at 30 Hz, which loopback absorbs.)
2. **Audio**: binjgb's native format, verified in `emulator.h:148-156` and `emulator.c:3900-3921`:
   **unsigned 8-bit, 2 channels, interleaved, at the configured frequency**. Configure 48,000 Hz to
   match Web Audio, which is 803.6 frames per Game Boy frame, about 3,200 bytes per snapshot. (The
   binding doc converts those u8 samples to **f32 stereo at 48 kHz** in the sim, which is 4x the
   bytes and removes the conversion from the page.)
3. **Spikes**: the neurons that spiked since the last snapshot. Not the 139,255-entry Float32 array
   the prototype sent (557 KB per snapshot). The binding doc fixes this as a `ceil(n/8)`-byte
   bitset, 17,407 bytes for 139,255 neurons, with the unique count in the header. (The adaptive
   index-list variant sketched here is dropped.)

Bandwidth at 30 Hz with the binding encodings: 1.5 KB header + 90 KB RGBA video + 12.8 KB f32 audio
+ 17.4 KB spike bitset = about 122 KB per snapshot, **3.7 MB/s, roughly 29 Mbit/s** on loopback.
With the 2bpp and u8 variants sketched above it would have been 28 KB per snapshot (840 KB/s).
Either is fine on loopback; the binding numbers are what the page and the capture chain must budget
for. Backpressure: one `watch` slot per client, drop-oldest, and a `feed_dropped_total` counter.

## 7. Control API

**Superseded by `docs/control-api.md` (see the override note at the top).**
Retained below because the rate-limit semantics, the event-log coupling and the deliberate absence
of a button endpoint are the parts a build agent most needs to get right.

HTTP on `127.0.0.1` only (port 7401 per the override), no auth (localhost, trusted machine, matching
the `server-sessions.md` stance), `Cache-Control: no-store`, JSON in and out.

| Route | Behaviour |
| --- | --- |
| `POST /stimulate {durationMs, source, viewer}` | The sugar path. `durationMs` clamped to 40..400 (default 120). Rate limited: max N per minute globally (default 6), max 1 concurrent pulse (a request while `rewardRemaining > 0` is rejected with 429 and `retryAfterMs`, rather than silently taking the max), per-viewer cooldown (default 60 s). Returns the queued position and the tick it will apply on |
| `POST /reward {value, source, viewer}` | Present but **disabled by default** (`[reward] enabled = false`), returning 403 with a body naming the sugar-only decision. When enabled: value clamped to a configured range, same rate limits, always logged with the viewer |
| `GET /status` | Full snapshot header plus dataset/kernel/plasticity/adapter versions, compatibility string, generation, archives, rank, uptime, realtime factor, lag |
| `POST /checkpoint` | Forces a durable commit, returns the committed generation; 409 if one is in flight |
| `POST /pause`, `POST /resume` | Stops and starts the loop; a pause takes an implicit durable checkpoint |
| `GET /metrics` | Prometheus text (see section 4 for the list) |
| `GET /healthz` | Liveness for the watchdog unit |

**There is deliberately no button endpoint, and no manual-input path of any kind.** The decoder is
the only writer of joypad state. The prototype's `manualButtons` field and its `learn: false`
suppression are dropped, not disabled, so there is nothing to re-enable by accident. Rate limits are
enforced server side, never client side. Every accepted and every rejected action is appended to the
event log with its source, viewer, tick and outcome, and the feed publishes those events, so the
on-screen ticker cannot disagree with what the sim did.

## 8. Persistence

- **Envelope**: reuse the frozen `encodeEnvelope` layout (`envelope.ts:61-80`): ASCII magic,
  `u32` manifest length, compact JSON manifest with `schemaVersion: 2` and the chunk name list, then
  length-prefixed chunks, then a `u32` CRC32 footer over everything preceding. Magic `FLYSIM01`.
  Chunk names stay `[a-zA-Z]+` so the TS `decodeEnvelope` can read a flysim checkpoint for recap
  and inspection tooling: `membrane, refractory, lastSpikeMs, visualDrive, plasticGains,
  plasticTraces, plasticTouched` (the frozen `AGENT_CHUNK_NAMES`) plus `emulator, framebuffer,
  ratchetGame, ratchetFrame`. The manifest is the TS `AgentManifest` plus the runtime fields the
  prototype used (`romHash, emulatorFrame, compatibility, speed, buttons, reward, ratchet`). Nothing
  hashes the manifest text, only the CRC32, so Rust's key order is free.
- **Atomic commit**, exactly the `server-sessions.md` sequence: write `<gen>.checkpoint.tmp`,
  fsync the file, rename, fsync the directory, then the same for `manifest.json`. The manifest
  rename is the commit point. `manifest.json` holds `{generation, latest, previous, archives:{rank:
  generation}}`. The previous valid checkpoint is retained; obsolete unreferenced files are removed
  after commit; a crash before the manifest rename leaves an unreferenced file that is never loaded.
- **Cadence (per the infra override)**: a **durable** commit to `save_dir` on disk every **300 s**,
  and a **hot** copy every **5 s** to a tmpfs path, both intervals and both paths configurable
  (`[persistence] durable_seconds = 300, hot_seconds = 5, hot_dir = "/dev/shm/flysim"`). The hot copy
  uses the identical envelope and the identical atomic sequence, so it is a first-class restore
  candidate and not a partial dump; it simply does not survive a host reboot. Rationale: SSD
  endurance, since a full checkpoint is roughly 2.5 MB and a 5 s disk cadence would write about
  43 GB per day. A durable commit also happens immediately after startup, restore, a ratchet
  recovery, a rank change and a pause, plus on `POST /checkpoint` and on graceful shutdown, so the
  300 s interval is a ceiling on routine loss and not on event-driven loss. The **state clone**
  happens on the sim thread, because that is what makes the snapshot consistent; the envelope
  encoding, the write and the fsyncs all happen on the checkpoint writer thread. Encoding used to be
  on the sim thread too and cost 8 to 10 ms in whichever frame carried the hot copy, which is a
  dropped frame every 5 s (`infra/docs/simloop-profile.md`).
- **Milestone archive**: the first accepted durable commit at a new best ratchet rank keeps its
  generation permanently and also writes an independent `milestone-<rank>.checkpoint`. Later
  rotations never unlink archived generations. Rank regression is rejected.
- **Startup restore order**: manifest, then the **tmpfs hot copy** (newest, if present and newer
  than latest), then durable latest, previous, the highest archived generation, then the independent
  `milestone-<bestRank>.checkpoint`. Each candidate: magic, CRC32, schema, ROM SHA-256, exact
  compatibility string, then agent import (self-validating and a no-op on failure), emulator, reward,
  ratchet. On a fallback, set `recoveryFallback` in `/status` and log an event. If every candidate
  fails, **exit non-zero**: no automatic fresh start, ever. A deliberate reset means moving the save
  directory by hand.
- **Compatibility string**, mirroring `src/runtime/compatibility.ts:9-11` field for field and order:
  `lif-1ms-f64-v2/pokered-unique8-v3/<datasetFingerprint>/fly-kc-mbon-rstdp-v2/binjgb:c60e138.../pokered:0cd19d3...`
  plus one new segment, `statefmt:<abi>`, derived from `s_emulator_state_size` and the target triple.
  Justification: `emulator_write_state` is a raw struct `memcpy` (`emulator.c:5010-5011`), so the
  bytes are ABI dependent. `EmulatorState` contains no pointers and no `size_t`, so wasm32 and
  x86-64 layouts are probably identical; M3 includes a probe test that compares the native size
  against the prototype's WASM size and diffs a known save. If they match, prototype checkpoints
  import and the segment records the shared tag; if not, milestone saves must be re-earned and that
  is a stated M3 finding.
- **A running macro is not checkpointed** (2026-09-16, `docs/design/macros.md`). Palette mode's
  state — the scene, the palette, the running macro, its plan and its frame count — is transient,
  like the readout's blocked-direction cooldown and for the same reason: a restore that resumed a
  half-finished route over a map the emulator state no longer shows would press buttons for a
  world that is not there. So a restore starts with **no macro running**, the scene is detected
  on the restored frame like any other frame, and the fly is consulted again on that frame's
  palette. The cost is at most one abandoned macro per restart. Nothing about this is in the
  checkpoint, so **the compatibility string does not move** and every existing checkpoint keeps
  loading — checked with `flysim --print-compatibility` before and after the change, byte for
  byte. A rollback does the same thing for the same reason: `MacroLayer::cancel` abandons the
  running macro and the next frame decides again.
- **Event log**: rolling JSONL at `events-YYYYMMDD.jsonl`, one object per line
  `{t, wall, ms, frame, kind, ...}`, fsynced at most once per second, size-capped with rotation and
  a retention count. Kinds: reward, milestone, rank, recovery, sugar (accepted and rejected, with
  source and viewer), pause, resume, checkpoint, restore, fallback, lag, error. The daily recap
  tooling and the feed's `events[]` both read it.

## 9. Configuration

`flysim.toml` plus `FLYSIM_*` environment overrides (env wins), parsed at startup, logged in full
at INFO with no redaction needed because **the service holds no secrets** (Twitch credentials live
in `flybridge`).

```toml
[paths]       rom, save_dir, hot_dir, dataset_dir, event_log_dir
[server]      feed_addr = "127.0.0.1:7400", control_addr = "127.0.0.1:7401"
[sim]         speed = 1.0, snapshot_hz = 30, rayon_threads = 0  # 0 = cores-2
[persistence] durable_seconds = 300, hot_seconds = 5, keep_generations = 2
[sugar]       enabled = true, max_per_minute = 6, per_viewer_cooldown_seconds = 60,
              default_ms = 120, max_ms = 400, max_concurrent = 1
[reward]      enabled = false, min = -1.0, max = 1.0
[feed]        audio_hz = 48000
[macros]      mode = "raw"  # or "palette"; docs/control-api.md has the rules
```

Ports, addresses and the wire encodings follow the two binding protocol documents; the table above
is the shape of the file, not a competing contract.

## 10. Symbol generation

Extend `fly-plays-pokemon/tools/build_reward_symbols.py` to emit a Rust module beside the TS one,
from the same single parse of `pokered.sym` and `constants/event_constants.asm` at the pinned commit:

```rust
// Generated by tools/build_reward_symbols.py; do not hand-edit addresses.
pub const POKERED_COMMIT: &str = "0cd19d3b...";
pub mod ram { pub const W_CUR_MAP: u16 = 0xD35E; /* ...23 addresses... */ }
pub static EVENTS: [(&str, u16); 368] = [ /* insertion order preserved */ ];
pub static MILESTONES: [&str; 16] = [ /* ... */ ];
```

`EVENTS` must be an ordered array, not a map: `pokemon-red.ts:75-78` iterates
`Object.entries(EVENTS)` and the iteration order determines the order events are emitted within a
frame, which is observable in `recent[8]`, the ticker and the event log (the reward sum itself is
order independent). Output path is passed as an argument so the generator does not hardcode a
sibling repo; a CI check regenerates both files and fails on any diff. The seven-entry reward
catalog is hand-ported to `flybrain-gb/src/reward/catalog.rs` with a golden test against a JSON dump
of `catalog.ts`.

## 11. Milestones, in order, each with a go/no-go

**M1: core kernel plus golden tests on the toy dataset.** `flybrain-core` with dataset loader,
rng, retina, lif, plasticity, decoder, gameboy preset, agent, envelope. `tools/golden.ts` producing
G1-G6, G8, G9. Single-threaded only.
*Go/no-go*: G1-G6, G8 and G9 pass byte-exact with zero tolerances, and the thread-count determinism
test passes trivially at one thread. If G9 shows an `exp`/`tanh` divergence, the vendored V8
`ieee754` port lands here before M2 starts.

**M2: real dataset golden plus measured rayon speedup.** G7, the bitset `slots` optimisation, the
partitioned sweep, `criterion` benchmarks, the determinism test across 1/2/3/6/16 threads.
*Go/no-go*: G7 byte-exact including the fingerprint string; identical state across all thread
counts; **at least 1.5x real time on 6 cores** on a Haswell-class CT, with the WSL 5800X3D number
recorded for reference. Falling short means profiling the sweep (likely the f64 conversions or the
`slots` read) before any further work.

**M3: binjgb FFI, frame loop, audio.** `vendor/binjgb` submodule, `csrc/flysim_gb.c` shim replacing
`wrapper.c` (whose `static Emulator* e` and `static JoypadButtons s_buttons` globals make it unusable
for a safe `Send` wrapper), `cc` plus `bindgen` build, the frame loop with the 120-attempt LCD-off
retry from `binjgb.ts:59-72`, audio drained every frame (the prototype never drained, so the buffer
silently reset on `AUDIO_BUFFER_FULL`; flysim must drain on that event and keep running to
`NEW_FRAME`), the u8-to-f32 stereo conversion the feed doc requires, and the save-state ABI probe.
*Go/no-go*: the ROM boots to the title screen; 10,000 frames run with no invalid opcode; the
framebuffer of frame N matches a WASM-produced reference frame byte for byte; audio has no gaps
across 60 s; `flysim_gb_state_size()` is recorded and the WASM-state import verdict is written down.

**M4: reward adapter, ratchet, ported tests.** `pokemon_red.rs`, `catalog.rs`, generated
`symbols.rs`, `ratchet.rs`, `recovery.rs`, `compatibility.rs`, G10, and the prototype's ratchet unit
tests (gates, count limits, game-only restore preserving neural state).
*Go/no-go*: every ported reward and ratchet test passes; a 30-minute run from a seeded milestone
state produces the same event sequence as the TS adapter fed the same WRAM traces; `recoverGame`
preserves RNG, clock and gains while clearing holds, eligibility and transient reward state.

**M5: feed, control API, checkpoints.** `flysim` binary complete: pacing, snapshot encoder, the two
listeners on 7400 and 7401, rate limits, the durable-plus-hot store with archives, event log,
metrics.
*Go/no-go*: a scripted client consumes 10 minutes of feed at 30 Hz within the budgeted bandwidth
with zero drops; checkpoint restore round-trips byte-identically from both the durable and the hot
copy; kill -9 during a commit leaves a loadable store in 100 injected-crash trials; the sugar
limiter holds under a 100 requests-per-second flood; `/metrics` scrapes clean; **no button endpoint
exists** (an assertion over the route table).

**M6: 24-hour soak plus restore drills.** Unthrottled and 1.0x runs, memory and RTF trend, five
scheduled kill-and-restore drills (at least two from the tmpfs hot copy and two from durable
latest), one forced-corruption fallback drill, one milestone-archive fallback drill.
*Go/no-go*: 24 h with no crash, no leak (RSS flat after warm-up), RTF at or above 1.0x throughout,
every drill resuming from the expected generation, measured disk write volume consistent with the
300 s durable cadence, and the event log plus recap tooling covering the whole window.

## Files to create

- `services/flysim/Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/config.toml`,
  `flysim.toml.example`, `README.md`, `tools/check_crate_age.mjs`
- `crates/flybrain-core/src/{lib,dataset,jsnum,math,rng,retina,lif,plasticity,decoder,version,agent,envelope,parallel}.rs`,
  `src/presets/gameboy.rs`, `tests/{golden,dataset,determinism}.rs`, `benches/sweep.rs`
- `crates/flybrain-gb/{build.rs,csrc/flysim_gb.c}`,
  `src/{lib,emulator,audio,memory,ratchet,recovery,compatibility}.rs`,
  `src/reward/{mod,catalog,symbols,pokemon_red}.rs`, `tests/{reward,ratchet,state_abi}.rs`
- `crates/flysim/src/{main,config,simloop,pacing,snapshot,feed,api,ratelimit,store,eventlog,metrics}.rs`,
  `tests/{api,store,soak}.rs`
- `packages/brain/tools/golden.ts` plus a `golden` script in `packages/brain/package.json`, and
  `packages/brain/tests/golden/` (small cases committed, real-dataset blobs gitignored with digests
  committed)
- `services/flysim/docs/{bit-exactness,operations}.md`, and a `services/` row in the flybrain README
  layout table. The feed and control contracts live in the two binding documents at
  `docs/`, not in a third copy under `services/`.

## Files to reuse unchanged

`packages/brain/src/**` stays the oracle and must not be edited (the `tests/legacy/README.md` rule
extends to it in spirit: change the Rust to match the TS, never the reverse). The `.binz`,
`meta.json` and `circuit-roles.json` artifacts in `data/fafb-v783` are read as-is. `binjgb` at
`c60e138` is vendored as a submodule with only an added shim file, never a patch.
`tools/build_reward_symbols.py` gains an output path, not a rewrite.

## Risks

1. **V8 versus libm transcendentals.** The highest risk to the whole bit-exact premise. Mitigated by
   the discrete argument domains (100 values for pairing, integer-over-5000 for trace decay), the
   `libm` crate instead of the host glibc, a vendored V8 `ieee754` port as the fallback, and a
   pre-declared tolerance scope (traces, gains, `signal` only) as the last resort. Resolved or
   escalated at the M1 gate, before any code depends on it.
2. **binjgb build flags and save-state ABI.** `emulator_write_state` is a raw struct `memcpy`, so a
   layout change between the WASM build and the native build invalidates existing milestone saves.
   Probed explicitly in M3. Secondary: `assert()` in `write_audio_frame` must stay enabled in debug
   builds, `-fno-strict-aliasing` for the type-punning paths, and only `emulator.c`, `joypad.c`,
   `memory.c`, `common.c` and the shim get compiled (never `host.c` or `tester.c`, which pull in
   SDL).
3. **Rayon nondeterminism.** Any accidental use of a work-stealing iterator whose reduction order
   varies, or of the global pool, silently breaks reproducibility in a way no single-threaded test
   catches. Mitigated by fixed index-range partitioning everywhere, no floating-point reductions
   across threads (integer counters only), an explicit non-global pool, and the 1/2/3/6/16-thread
   byte-compare test running in CI on every commit.
4. **Audio format and drift.** binjgb produces unsigned 8-bit stereo; the feed contract requires f32
   stereo at 48 kHz, so the conversion and its scaling (u8 128 as silence) live in the sim and need a
   test against a known tone. The prototype never drained the buffer, so there is no working
   reference for continuous audio. Frame pacing at 59.7275 fps against a 48 kHz clock drifts, so the
   page needs a small queue with a target depth and the header must carry exact sample counts. If the
   browser path proves fragile, the fallback is ffmpeg reading a raw PCM pipe from `flysim` directly,
   which costs the decision-8 property that sync is the browser's problem.
5. **Real-time factor on Haswell.** The M2 target of 1.5x on 6 cores is an extrapolation from a
   0.92x single-threaded Node measurement on a 5800X3D. If Haswell lands under 1.0x even with the
   full pool, the options are a wider SIMD sweep, dropping the f64 promotion where the double-rounding
   theorem provably permits f32 arithmetic (membrane times decay), or accepting sub-real-time and
   saying so on screen.
6. **Dataset fingerprint reproduction.** Depends on V8's `JSON.stringify` of the merged metadata.
   Verified safe today (ASCII, integer-only, compact input, `Object.assign` merge semantics
   reproducible with `preserve_order`), but a future `build_flywire.py` that emits a float or a
   non-ASCII string would break it silently. G7 catches it; the dataset builder should also gain a
   comment saying so.
7. **Checkpoint loss window widened by the 300 s durable cadence.** The tmpfs hot copy covers process
   crashes but not host reboots or container migrations, so an unclean host loses up to 300 s of
   learning. Mitigated by the event-driven durable commits (rank change, recovery, pause, shutdown)
   and by the milestone archive, and measured in the M6 drills.

### Critical Files for Implementation

- `packages/brain/src/model/lif.ts`
- `packages/brain/src/model/plasticity.ts`
- `packages/brain/src/agent/agent.ts`
- `~/fly-plays-pokemon/src/simulation.worker.ts`
- `~/fly-plays-pokemon/.tools/binjgb/src/emscripten/wrapper.c`
