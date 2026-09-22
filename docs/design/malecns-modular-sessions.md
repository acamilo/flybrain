# MaleCNS and reusable streamed simulation sessions

Status: **proposal, not an implemented contract**. Written 2026-09-18 against `f7bc13a`
on `main`. This document covers two related projects: adding MaleCNS v1.0 as another
connectome, and extracting reusable modules for other emulators, embodied environments,
and multiple flies. No dataset, neural semantics, deployed configuration, or wire contract
is changed by this document.

The binding [feed](../feed-protocol.md) and [control](../control-api.md) contracts take
precedence. The TypeScript brain remains the oracle. Existing default versions
`lif-1ms-f64-v2` and `fly-kc-mbon-rstdp-v2` remain pinned.

Reading map: sections 2–3 contain the code audit and MaleCNS analysis; sections 4–6
define the proposed module/session boundaries; sections 7–8 give the extraction order,
implementation workstreams and acceptance gates; sections 9–10 record open questions and
sources.

Execution queue: [implementation backlog](malecns-modular-implementation.md), with branch-sized
deliverables, dependencies and completion criteria. Start at FOUNDATION-01 when work resumes.

Concrete follow-up: [Melee emulator and multi-fly framework audit](melee-framework-audit.md),
including source-checked Dolphin/libmelee integration options and full-stack performance gates.

Implementation contracts: [session framework](session-framework/README.md), defining private
Flybus RPC/pub-sub, lockstep phases, worker methods, artifact ownership, recovery and publication.
The [bus specification](session-framework/bus-v1.md) is the selected communications design:
one small Rust router, external immutable artifacts and delivery-scoped GC. Application
orchestration/presentation are developed together; tournaments are examples, not framework types.

## 1. Recommendation

1. **Add MaleCNS as a dataset/profile combination, not a replacement neural model.**
   First run it through the existing LIF semantics with explicit, independently versioned
   sensory, population, and readout mappings. Study different neuron dynamics separately.
2. **Make a session the unit of simulation ownership.** A session has one environment and
   one or more independently stateful agents bound to its control ports. A shared match
   advances once after all players have chosen actions from the same observation boundary.
3. **Extract along ownership and timing boundaries.** Separate anatomy, neural dynamics,
   sensor encoding, action decoding, environment execution, task semantics, persistence,
   observation transport, presentation, and audience interaction. Preserve current behavior
   through a legacy composition while extracting these modules.
4. **Prove the design on a ROM-free two-player arena before a larger emulator.** Then build
   a frame-stepped emulator integration. For “flies play Smash,” the first candidate should
   be a specifically chosen title/backend, such as Melee with a pinned Dolphin integration;
   “Smash” alone is not an emulator requirement.
5. **Retain a monorepo and one Rust workspace initially.** Reusable libraries do not require
   a network of microservices, dynamic native plugins, or publishing unstable packages.

The two tracks can progress independently after the identity/profile boundary is established.
MaleCNS does not require multiplayer; multiplayer does not require MaleCNS. The first useful
deliverables are a reproducible MaleCNS characterization run and a behavior-preserving
single-agent session API, not a wholesale rewrite.

## 2. What the code actually does today

Paths below are relative to the repository root. Rust paths beginning `core/`, `gb/`, or
`sim/` in this document abbreviate `services/flysim/crates/flybrain-core/`,
`services/flysim/crates/flybrain-gb/`, and `services/flysim/crates/flysim/` respectively.
These aliases refer to **current** paths, not proposed directories.

| Boundary | Evidence inspected | Consequence |
| --- | --- | --- |
| Reference brain | `packages/brain/src/agent/agent.ts`; `core/src/agent.rs` | `NeuralAgent` composes network and decoder; image size and milliseconds/frame are configurable, but defaults are Game Boy-specific. It is already more reusable than the service. |
| Anatomy | `packages/brain/src/dataset/format.ts`; `core/src/dataset.rs` | CSR graph dimensions are dynamic. Weights are signed `i16`; roles and a two-dimensional visual-column table are part of the model input. Validation currently checks array lengths, not every graph invariant. |
| Dataset construction | `tools/build_flywire.py` | Five checksum-pinned Codex exports; stable indices from sorted root IDs; directed-pair aggregation; transmitter signs; clipping to ±32767; FAFB-specific role and L1-column extraction. Game macro populations are also generated here. |
| Neural dynamics | `core/src/lif.rs`; `packages/brain/src/model/lif.ts` | One-ms ticks, configurable gain/noise, role stimulation, image drive, and up to 64 tracked rate roles. It is not a general multimodal sensory API. |
| Learning | `core/src/plasticity.rs` | Strongest positive pre-role→post-role edges, default KC→MBON budget 16,384; gains/eligibility are per-agent. A caller supplies the reward scalar; PAM spikes do not generate it. |
| Parallelism | `core/src/lif.rs` (`SweepPlan`); `core/src/pool.rs` | Persistent deterministic within-brain pool. `broadcast` has one shared job slot and assumes one dispatcher at a time; cloning a plan is not permission to dispatch it concurrently. |
| Game interface | `gb/src/adapter.rs` | `GameAdapter` mixes reward detection, progress, decoder preset, recovery, ROM checks, and Pokémon-like tile/exit/objective queries. `MemoryReader::read8(u16)` is specifically a Game Boy-shaped interface. |
| Emulator | `gb/src/emulator.rs` | Concrete binjgb wrapper, 160×144 RGBA, eight-bit pad, one frame step, audio conversion, native save-state format. Explicit handles and `Send` allow multiple instances; `Sync` is deliberately absent. |
| Session ownership | `sim/src/simloop.rs` (`Sim`, `step_frame`) | One agent, emulator, adapter, ratchet, button mask, framebuffer, audio queue, sugar state, chat ring, and set of clocks. Application orchestration and game behavior share a large struct. |
| Actions | `sim/src/macros.rs`; `gb/src/macros.rs`; `gb/src/pokemon_red/macros/` | Neural channels select available macros, but the game-specific executor owns button sequences. The title screen uses raw input; scene handling, routing and targets are engineered behavior. |
| Recovery | `gb/src/recovery.rs`; `gb/src/ratchet.rs` | Game-only rewind retains brain clock, membrane, RNG, and gains; clears holds/eligibility and refreshes vision. A scalar progress ladder chooses a best save. This is not a general multiplayer reset policy. |
| Persistence | `sim/src/store.rs`; `gb/src/compatibility.rs` | Durable atomic envelope and manifest commit are reusable. Payload is one agent plus one emulator and ratchet. Compatibility names binjgb and `pokered`; native state identity includes size and target. |
| Public observation | `sim/src/snapshot.rs`; `packages/feed/src/{types,codec}.ts` | One flat brain/game snapshot; one attachment per kind; Game Boy buttons, fixed frame dimensions, Pokémon-shaped reward counters and a closed game-mode set. |
| Stage | `apps/stage/src/{App.tsx,feed/store.ts,feed/decode.ts}` | Good hot/paint/cold clock split, but mutable stores/scalers are singletons and the decoder checks 160×144 frames. Dataset URL is fixed to FAFB. |
| Game presentation | `apps/stage/src/games/` | A useful registry already exists, but config relabels v1 counters rather than declaring independent task schemas. |
| Twitch bridge | `services/bridge/src/{index,sim,commands,redemptions,templates}.ts` | Transport/client abstraction, test fakes, templates, rate limits, and redemption persistence are valuable. There is one sim URL and no agent target identity. |
| Packaging | `apps/stage/vite.config.ts`; `infra/build/package-release.sh`; `infra/05-deploy.sh` | Stage build copies FAFB artifacts; packaging defaults to FAFB; deploy preflights one compatibility string. Runtime/data/frontend are still one release composition. |

### 2.1 Behaviors to preserve before extracting

`Sim::step_frame` advances the brain from the previous visual input, decodes, applies
buttons, steps the emulator, sets the new visual input, samples rewards, stimulates per
reward event, reinforces their sum, updates macro availability, and observes the ratchet.
Control commands are drained before the frame step. This ordering is part of behavior.

Do not replace this with `NeuralAgent::tick` merely because it looks like a convenient
wrapper: the service currently orchestrates substeps to sample reward from the frame just
produced. Moving reward or visual drive across that boundary changes trajectories.

Other invariants:

- Deterministic arithmetic and per-target propagation order, including across thread counts.
- Browser/network clients never stall the sim; snapshots are latest-value/drop-oldest.
- Checkpoint capture is coherent; encoding and storage happen off the sim thread.
- Failed restore does not silently reset a run. Existing legacy restore policies remain exact.
- No public control endpoint for button presses or game-memory writes.
- Sugar and learning reward are distinct mechanisms. Chat text never becomes neural input.
- The current positive-only reward doctrine remains the default for all shipped tasks.

### 2.2 Existing compatibility gaps to handle deliberately

The dataset fingerprint hashes metadata (including anatomical circuit roles) and six arrays.
`macro_*` roles are merged **after** hashing to preserve old checkpoints. Both language
implementations document that changing those populations could restore rates onto different
neurons without invalidating the checkpoint. The kernel parameter version also omits role
names; the service compatibility string does not fully identify the decoder/action mapping.

Keep these historical behaviors in the legacy reader. For new profiles, add an explicit
behavior identity covering population bindings, sensor encoding, decoder configuration,
action executor, and reward catalog. Do not repair the old hash by changing it in place.

Open macro/shop and recovery branches existed when this proposal was written. Before
implementation, rebase the inventory against their merged state and rerun characterization;
this proposal neither incorporates nor supersedes their unmerged changes.

## 3. MaleCNS: anatomy, connectivity, and model are different things

### 3.1 Dataset comparison and evidence limits

| Property | FAFB v783 used here | MaleCNS v1.0 |
| --- | --- | --- |
| Specimen | Adult female | Adult male, independently imaged/reconstructed |
| Territory | Brain including optic lobes | Central brain, optic lobes, ventral nerve cord (VNC), intact neck connective |
| Local artifact | 139,255 neurons; 2,700,513 directed-pair edges | None imported in this repository |
| Available inventory counts | Codex lists 139,255 neurons | Codex lists 166,700; the Minecraft project's neuPrint `:Neuron` export reports 176,422. Selection rules must be reconciled before fixing a local count. |
| Input labels | Codex `root_id`, classification, consolidated types, column assignment | neuPrint/flat export `bodyId`/body IDs, class hierarchy, transmitter properties, sides, neuropils, cross-dataset type annotations |
| Added anatomical opportunity | Brain sensory→descending circuits | Brain↔VNC circuits, local motor circuitry, ascending feedback, additional sensory and motor populations |
| License evidence | Repository attribution: CC BY-NC 4.0 | Official MaleCNS download site: CC-BY; verify and retain the exact release license text when importing |

MaleCNS is not FlyWire with extra neurons appended. IDs and dense array indices do not
correspond. Homologous cell types and registered anatomical spaces enable comparisons, not
automatic one-to-one neuron matching, state transfer, or graph concatenation. Male-specific
and sexually dimorphic circuits make a universal matching assumption especially misleading.

The official MaleCNS site describes a finished, proofread and annotated CNS reconstruction.
This does not mean every synapse, cell type, or sensory column is equally certain. The
Minecraft derivative reports asymmetric visual-column coverage and missing soma positions;
our import must quantify coverage from its own pinned source. A connectome also does not
supply all synaptic physiology, electrical coupling, neuromodulation, body dynamics, or
behavioral competence.

### 3.2 What “connections” means

Separate these quantities in metadata, reports, and on-screen claims:

1. Source neuron/segment inventory and the chosen included-neuron inventory.
2. Individual synaptic contacts/partner pairs (and separately pre-sites and post-sites).
3. Directed neuron-pair edges after aggregation.
4. Retained edges and retained synaptic weight after confidence/weight filtering.
5. Effective signed weights after the simulation's transmitter and clipping policy.

The FAFB builder aggregates export rows by `(pre, post)` across rows, drops endpoints not
in its classification inventory, assigns a sign, and clips the summed magnitude. It adds
no explicit five-synapse threshold of its own. The source exports may already be filtered;
the builder cannot recover contacts absent upstream. Codex headline connection counts are
not necessarily the local aggregated graph's edge count.

The Minecraft project's provenance reports ~25.9 million MaleCNS neuron-pair edges at
weight ≥1 and a bundled derivative of 6,287,749 edges at weight ≥5, representing
90,296,905 of 125,024,863 neuron-to-neuron synapses. It removes 40 autapses. Those are
**that project's reported query/build results**, not counts independently reproduced here.
Its five-contact cutoff retains roughly 24% of edges but 72% of synaptic weight. It is a
performance/modeling choice, not the definition of a complete CNS.

The official bulk weight table includes **segments**, not only curated neurons. Loading
all rows as if they were all validated neurons would be a different experiment. Likewise,
neuPrint ROI-level adjacency rows must not be summed together with their already-aggregated
totals. Specify one authoritative edge representation and count every exclusion.

### 3.3 Acquisition and reproducible construction

Prefer official versioned bulk tables for the repeatable build, with a neuPrint query tool
for inspection and cross-checking. The official download page lists:

- `body-annotations-male-cns-v1.0-minconf-0.5.feather` — curated annotations.
- `body-neurotransmitters-male-cns-v1.0.feather` — neuron-level transmitter information.
- `body-stats-male-cns-v1.0-minconf-0.5.feather` — segment statistics; large and broader than
  the curated neuron list.
- `connectome-weights-male-cns-v1.0-minconf-0.5.feather` — full segment connection graph,
  approximately 1.1 GB as listed by upstream.

The much larger synaptic-point and partner tables are unnecessary for an initial point-neuron
simulation. Download them only for a question requiring synapse-level geometry. Neither
research downloads nor anatomy conversion belong in service startup or ordinary unit tests.

Proposed build stages:

```text
release manifest + checksummed source cache
  → source-specific parser
  → normalized neuron/edge tables + exclusion report
  → selected anatomical graph
  → model-specific signed-weight transform
  → runtime CSR bundle + profile bindings + separate viewer bundle
```

Each stage records source release, database revision if queried, query/filter definitions,
source byte hashes, tool revision, and output hashes. Fetch time is provenance, not a random
input to the semantic graph hash. Sort body IDs numerically; encode original IDs as strings
in JSON so this common format also preserves FAFB IDs beyond JavaScript's safe integer range.
Preserve original annotations and cross-dataset type aliases separately from normalized roles.

The first import report must reconcile the Codex/neuPrint count difference, or explicitly
choose and document one inventory without claiming equivalence. It must also report missing
IDs, unannotated neurons, empty required populations, missing geometry, unknown sides,
transmitter confidence/fallback counts, duplicate edges, autapses, clipped weights, and
retained contacts by region and threshold.

Use deterministic serialization and gzip headers, following our existing reproducible
builder rather than copying the Minecraft artifact's timestamp-dependent container format.
Keep the source cache outside tracked artifacts; pin published runtime bundles by digest.

When adding actual data, update `NOTICE`, `LICENSES.md` and bundle-local attribution/license
files in the same change. Keep FAFB-derived assets under their existing terms; a separately
licensed MaleCNS bundle does not relicense mixed fixtures, old goldens or viewer assets.

### 3.4 Graph and sign policy

Keep raw positive contact counts and transmitter evidence in the normalized data. Sign is a
model transform, not a measured property that should overwrite the source evidence.

The legacy policy assigns GABA/GLUT negative and other/unknown transmitters positive;
conflicting per-edge transmitter rows become `MIXED`, then positive. Do not silently extend
that policy to histaminergic photoreceptor input. A MaleCNS policy must explicitly define
histamine, monoamines, mixed/unknown labels, confidence fallbacks, and whether transmitter
is chosen per neuron or per connection. None implies receptor-specific physiology.

Recommended first profiles:

- **`malecns-v1-lif-baseline`**: existing kernel, explicitly versioned sign policy and L1
  input mapping, fixed readout, plasticity initially disabled for characterization.
- **`malecns-v1-lif-learning`**: same anatomy/input/readout plus audited KC→MBON selection
  and the existing reward rule. Learning is an experimental condition, not an assumed gain.
- Later **sensorimotor research profiles**: photoreceptors, mechanosensation, VNC outputs,
  and possibly another neural model, each with its own identity and validation.

Build weight≥1 and weight≥5 variants as **different graph identities** for the benchmark.
Do not select a production cutoff until activity, retained connectivity, memory and speed
have been measured. Preserve autapses by default in the new canonical graph; if an experiment
removes them, record the policy. The claim that point-neuron models have no use for autapses
is not a reason to discard observed connectivity silently.

Schema-1 `i16` weights may be sufficient, but measure overflow rather than assume it. For
the first existing-kernel comparison, emit a schema-1-compatible runtime view only if its
quantization/clipping is explicitly reported. If wider weights are needed, implement an
additive format/loader and matching oracle path; do not reinterpret old `weights.binz`.

Strengthen validation before constructing a network: CSR starts at zero, is monotone, ends
at edge count; targets/roles/visual indices are in range; required populations are present;
geometry is finite where marked valid; array lengths and index widths are representable;
declared hashes and artifact sizes match. Test the same invalid fixtures in both languages.

### 3.5 Population and sensory mapping

Existing role predicates cannot be copied unchanged. The current builder recognizes Codex
names such as `Kenyon_Cell`, `brain_motor_neuron`, `DAN` and `PAM*`; MaleCNS uses another
annotation vocabulary. Maintain a reviewed mapping table with source predicates, resulting
counts, hemisphere policy, aliases, and citations. Resolve each profile's required roles
at startup; an absent role must be an unsupported capability, not a silent empty population.

In particular:

- **Brain motor ≠ all motor.** Current macro pools combine 96 MBONs and 110 brain motor
  neurons. Adding hundreds of VNC motor neurons under `motor` would silently change that
  behavior. Use qualified roles such as `brain.motor`, `vnc.motor`, `brain.descending`,
  `mb.kenyon`, `mb.output`, and `mb.pam` in new profiles, with legacy aliases only as needed.
- **Action groups are not anatomical facts.** `command_*` buckets use index modulo eight;
  `macro_*` groups use a round-robin MBON/brain-motor pool. Move their construction into a
  versioned task readout profile, outside the anatomy builder. Do not describe those groups
  as natural “attack,” “jump,” or game-objective circuits.
- **Rate budgets are finite.** Both kernels currently cap tracked populations at 64. A
  full CNS has many more interesting populations. Select a bounded control/telemetry set
  initially; arbitrary bulk population analysis belongs in offline tooling. A larger mask
  is a measured, oracle-tested change, not an unbounded string map in the tick loop.
- **L1 first, photoreceptors later.** Reuse the existing luminance projection only after
  auditing MaleCNS L1 hex coordinates, both sides, and a declared hex→2D transform. Soma
  coordinates are not visual-field coordinates. Missing columns remain explicitly missing;
  do not synthesize them from array order or infer them from another specimen's neuron IDs.
- **Input profile and display view differ.** A game image may be resized/cropped for the
  agent while the full frame is shown to viewers. Record crop, orientation, color transform,
  and sampling geometry. Neither player may accidentally receive another player's private
  view or adapter-only task observations.

The existing `set_visual_frame` and `stimulate` API cannot represent a general collection of
odor/touch/proprioceptive inputs. A later sensory-drive interface needs explicit units,
target populations, additive/overriding rules, tick ordering, and deterministic noise streams.
It must be specified in TypeScript before a matching Rust implementation. Keep the old
image/stimulation path available byte-for-byte through its compatibility facade.

### 3.6 What MaleCNS lets us investigate

| Experiment | New capability | What still needs engineering/measurement |
| --- | --- | --- |
| Same game, another connectome | Compare datasets under a matched task interface | Cell-type mapping, gain/activity calibration, readout comparability, multiple seeds |
| Brain↔VNC control | Read actual descending, ascending and motor populations | Body/control mapping; gamepad commands are not muscles |
| Embodied fly arena | World smell/taste/touch/vision mapped into annotated sensory populations | Sensor transduction, proprioception and body dynamics; identify every reflex shortcut |
| Mixed-dataset two-fly match | FAFB and MaleCNS agents share one environment | Balanced observations, controller mapping, compute budgets, intervention rules |
| Circuit perturbation | Compare full graph with VNC feedback or defined pathways ablated | Separate graph identities; activity and behavioral controls; no biological claims from gameplay alone |

The Minecraft project is a useful engineering comparison, not our validation oracle. It uses
a Shiu-style current-based LIF model with synaptic dynamics/delay, reports gain calibration,
and explicitly supplies odor-approach reflexes and higher-level looming drive where its
simulated pathways do not work. Its source graph, numerical model and embodiment differ
from ours simultaneously. Do not attribute its behavior solely to MaleCNS.

### 3.7 Identity, restore, and performance

Name the components independently:

```text
anatomyId     = source release + included inventory + graph/filter digest
modelId       = numerical semantics + effective numeric configuration
sensorId      = input encoding + anatomical binding digest
readoutId     = population partition + decoder + action mapping digest
learningId    = rule + selected-edge topology + reward-catalog identity
viewerId      = positions/geometry + index mapping digest (presentation only)
```

The composite behavioral identity covers all behavior-affecting components; original source
IDs and display geometry cannot replace it. Same neuron count does not establish compatibility.
No FAFB neural checkpoint is restored into MaleCNS. A deliberately fresh MaleCNS brain may
start from a compatible game-only save, with a new run identity, clean calibration and reward
baselining; that is a new experiment, not continuation of the old fly. Gains are not mapped
between specimens by cell-type name.

For rough capacity planning, the current CSR is `4(N+1) + 6E` bytes, excluding roles,
geometry, derived propagation structures and mutable state. At 176,422 neurons that is
about 38.4 MB for 6.29 M edges, or 156.1 MB for 25.9 M edges (decimal MB). This is roughly
2.3× or 9.6× our edge count, not a prediction of the same slowdown. Spike activity, fan-out,
plasticity selection, memory bandwidth and sharding determine runtime cost. Checkpoint
copies and renderer assets also need separate memory budgets.

`LifNetwork` accepts shared `Arc<BrainDataset>` already. Reuse immutable anatomy across
same-profile agents; keep membrane, refractory state, RNG, rates, decoder holds, stimulation,
eligibility and learned gains private. Audit constructor-derived caches before moving them
into a shared topology object. Never share mutable gains merely because graphs match.

Measure headless one-, two-, and four-agent runs with plasticity on/off, fixed inputs and
representative activity. Record resident/peak memory, initialization, state capture cost,
per-phase p50/p95/p99, spike distribution, and real-time factor. Existing CUDA code is an
optional backend requiring its own new-dataset equivalence/capacity gate, not assumed capacity.

## 4. Reusable architecture

### 4.1 Define the nouns first

- **Dataset bundle:** immutable anatomical graph and source annotations.
- **Brain profile:** dataset plus numerical model, sensory/readout bindings and learning rule.
- **Agent:** one independently stateful brain, encoder, decoder and action executor.
- **Environment:** the world being advanced: one emulator instance, a linked-emulator group,
  or an embodied simulator. Owns controller ports, world state, media, and native clock.
- **Task:** interpretation of environment state: rewards, progress, episode endings, allowed
  macro actions and recovery policy. Pokémon is a task, not an environment API.
- **Session:** one environment plus agents, port assignments, scheduler, task state and clocks.
- **Application:** composes sessions/components and their presentation; owns supervision,
  persistent identities/history, run/intervention rules and application-specific schemas.
- **Bus:** generic RPC/pub-sub routing and artifact ownership, with no game/simulation semantics.
- **Broadcast:** presentation of one or several sessions, plus chat and audience interactions.

An agent ID is not a Twitch username, controller port, array position or dataset ID. Session,
episode, agent, port, view, and event identities must be explicit and stable across restore.

### 4.2 Dependency direction

```text
source importers → dataset bundles
                         ↓
                  neural core (TS oracle / Rust runtime)
                         ↓
             agent composition: sensors + readout + executor
                         ↓
environment backend + task plugin → session runtime → observations/checkpoints
                                           ↑                  ↓
                                    command admission   protocol adapters
                                           ↑                  ↓
                                      Twitch bridge        stage / recorder
```

The neural core knows no emulator, task, network socket, chat, or UI. The environment knows
no neural populations or Twitch. The task may inspect backend-specific state through a
typed inspector, but neither inspection nor public presentation gives clients a memory-write
or controller-write API. Only the session commits agent-produced controls.

### 4.3 Proposed modules and staged layout

These are target responsibilities, not instructions to create every package immediately.
Start as modules; extract crates/packages once a second consumer demonstrates the boundary.
Keep the Rust workspace under `services/flysim` during semantic extraction so paths and
behavior do not change together. A later mechanical move can place reusable crates at the
root, updating CI/build/golden paths in one dedicated change.

| Module / eventual location | Owns | Extraction source |
| --- | --- | --- |
| `packages/brain` | Reference numerical behavior and legacy public facade | Existing package; keep imports compatible |
| `crates/flybrain-core` | Rust numerical kernel, plasticity, generic population decoder | Existing `core/`; leave compatibility re-exports for presets |
| `crates/fly-dataset` | Manifest validation, artifact loading, source-ID/index mapping | `core/src/dataset.rs`; retain legacy fingerprint implementation |
| `crates/flybus` | One Rust RPC/pub-sub client/router, immutable artifact store, delivery guards and GC | New generic library; embedded router or small executable, no separate worker transport |
| `tools/datasets/{fafb,malecns}` | Source-specific conversion to common bundles | Existing Python builder plus new importer; existing CLI wrapper remains |
| `crates/fly-session` | Agent ownership, clock coordination, action commit, event/reward routing | Orchestration extracted from `sim/src/simloop.rs` |
| `crates/fly-environment` | Backend capabilities, ports, observations, media, save-state interfaces | New small contract proven with binjgb and synthetic arena |
| `crates/fly-env-gb` | binjgb FFI, memory inspector and native save-state identity | `gb/src/{emulator,ffi}.rs`, build glue and vendor boundary |
| `crates/fly-task-pokemon`, `fly-task-platformer` | Audited reward rules, semantic state, macros, progress/recovery | `gb/src/{pokemon_red,platformer}/`; do not generalize tile routing into the core |
| `crates/fly-checkpoint` | Atomic storage and session envelope; legacy payload adapter | `sim/src/store.rs` plus core envelope helpers |
| `crates/fly-protocol` / `packages/feed` | Versioned wire schemas/codecs, legacy adapters, synthetic fixtures | `sim/src/snapshot.rs`, existing feed package; canonical schema with cross-language tests |
| `services/flysim` | Composition/config, HTTP/WS, process lifecycle, metrics | Thin host over reusable session library |
| `packages/stage-runtime` | Feed ingestion, per-session stores, paint loop, audio, fixture clock | Extract from `apps/stage/src/{feed,paint,audio,motion}` after multi-view prototype |
| `apps/stage` + presentation plugins | Layout, branding, task panels, audience-facing explanations | Existing page with legacy layout preserved |
| `services/bridge` + audience client module | Twitch transport/auth/redemptions; session-targeted interaction client | Existing bridge; extract provider-independent logic only when reused |
| `infra/` | Release composition, process supervision, capture, recordings | Existing tooling parameterized by session/broadcast manifest |

Use static Rust composition or a small closed registry initially, with trait boundaries at
backend/task seams. Do not require stable native dynamic-plugin ABI. An out-of-process
emulator helper implements the backend through Flybus RPC, using the same bus as application
events and publication. Native emulator protocols stay inside its adapter. This is not a
new public action API or a reason to maintain a second framework transport.
Keep backend-specific memory access private to its task implementation instead of widening
`read8(u16)` into a supposedly universal game-state abstraction.

### 4.4 Environment and agent contracts

Illustrative interfaces; concrete types must be written with tests during contract work:

```rust
trait Environment {
    fn descriptor(&self) -> &EnvironmentDescriptor;
    fn observe(&mut self) -> Result<WorldObservation>;
    fn advance(&mut self, actions: &ActionBatch) -> Result<WorldStep>;
    fn capture(&mut self) -> Result<EnvironmentCheckpoint>;
    fn restore(&mut self, state: &EnvironmentCheckpoint) -> Result<()>;
}

// One decision boundary, one action per configured controller port.
struct ActionBatch {
    session_tick: u64,
    ports: Vec<PortAction>,
}
```

`descriptor` declares rational step duration, controller schemas, views, audio streams,
save/restore availability, task inspection capabilities and determinism level. Capture and
restore return an explicit unsupported error when unavailable; configuration validates that
the chosen recovery policy can work. `WorldObservation` is a frame-boundary snapshot or
immutable handle, not an object allowing agents to advance the backend.

Control schemas support digital buttons and bounded analog axes/triggers, with neutral
values, axis ranges, dead zones, and mutually exclusive directions where appropriate.
Preserve the Game Boy mask as one concrete codec. Analog controls need a fixed, versioned
decoder mapping; an 800-ms direction hold is not a sensible default for every fighting game.

Separate three observation surfaces:

1. **Agent sensory view:** pixels or declared synthetic senses the profile may consume.
2. **Task inspector:** audited state for reward/macro/episode logic. Access is part of the
   disclosed scaffold, not implicitly available to the neural encoder.
3. **Broadcast view:** media and summaries for viewers, potentially richer than either player's
   allowed sensory input.

Readout produces semantic channel activations or continuous signals. A task-local action
executor translates these into port actions, optionally running a selected macro. It is
explicitly resettable/checkpointable and reports selected action versus actual controller
output. Pokémon pathfinding, dialog logic, and objective catalogs stay in its task plugin.

Task outputs become scoped `RewardEvent`, `Progress`, `EpisodeEvent` and `ActionAvailability`.
Progress is a tagged value (`ladder`, `score`, `match`, `exploration`, or task extension), not
always a scalar rank. Rewards carry recipient agent/team, rule ID, event ID and observation
tick. A task cannot mutate neural state directly; the session routes accepted rewards once.

An agent-facing API similarly separates `advance_brain(interval)`, `decide(observation,
availability)`, `encode_next(sensory_view)`, and `apply_outcome(rewards, stimulation)`.
The session owns their order. Agents cannot call `Environment::advance`, select another
port, or inspect another agent's mutable state. Start with a concrete LIF agent composition;
introduce a controller trait when synthetic controllers or a second neural model require it.
Test controllers implement the same decision surface but are identified as non-neural agents
in descriptors and experiment records.

### 4.5 Example composition

Illustrative configuration, not syntax supported by today's `flysim.toml`:

```toml
[session]
id = "arena-demo"
environment = "synthetic-arena-v1"
task = "two-player-rounds-v1"
scheduler = "lockstep-v1"
master_seed = 1234
recovery = "round-reset-keep-gains-v1"

[[agents]]
id = "fly-a"
port = "player-1"
profile = "fafb-arena-baseline-v1"
sensory_view = "shared-camera"

[[agents]]
id = "fly-b"
port = "player-2"
profile = "malecns-arena-baseline-v1"
sensory_view = "shared-camera"

[broadcast]
layout = "shared-match-two-agents"
audio = "world"
audience_stimulation = false
```

Resolve profile IDs through a local, digest-pinned registry. Both agents may instead select
the same profile and share immutable topology while retaining independent state. The host
validates unique agent IDs and exclusive port ownership, view accessibility, profile/controller
compatibility, recovery capability and resource budget before starting. An independent-games
broadcast composes two such sessions; it does not misrepresent them as ports in one world.

## 5. Multiple flies: concurrency is not multiplayer

### 5.1 Three supported arrangements

| Arrangement | Ownership / synchronization | First use |
| --- | --- | --- |
| Independent flies in independent games | One session/process each; optional broadcast composition | Parallel streams and experiments; existing deployment pattern generalizes easily |
| Several flies in one game | One environment, several ports, one session barrier | Local fighting/multiplayer games |
| Linked emulator instances | One composite environment owns all instances and link state | A later link-cable experiment; requires cycle-accurate link support, not two independent frame loops |

For a shared arena there must not be one `Sim` loop per player, each calling `run_frame`.
That advances the world multiple times and gives an ordering advantage to one agent.

### 5.2 Shared-world step semantics

For decision boundary `t`, freeze observation `O[t]`, then:

1. Admit queued audience/operator commands against stable session/agent identities; log their
   effective tick. Chat remains presentation state.
2. Each agent advances its brain for the same environment interval, using its previously
   encoded sensory input. Its clock remainder and RNG are private.
3. Each agent decodes and advances its action executor using the same `O[t]` task boundary.
4. Barrier: collect all port actions, validate ownership/ranges, and commit one complete batch.
5. Advance the environment **once** to obtain `O[t+1]` and timestamped media.
6. Encode the next sensory inputs, evaluate task events from the completed transition, apply
   explicitly routed stimulation and rewards, and compute next action availability.
7. Apply any whole-session episode/recovery transition; capture coherent state and publish.

Preserve the detailed legacy ordering inside the legacy single-agent composition. New
profiles identify their scheduling semantics explicitly rather than silently adopting a
different reward phase. Use rational environment time and integer substep accumulation for
new sessions; keep the legacy floating remainder arithmetic for old trajectories. The neural
clock may lead environment time by warm-up; persist that offset instead of pretending all
clocks start at zero. Rendering, physics and decision cadence may differ, but the backend
must define their relationship.

Start with sequential agent evaluation for reproducibility. Then compare parallel agent
evaluation against the same action trace. Cap total worker budget: `agents × brain_threads`
can otherwise oversubscribe the machine. Use private pools for concurrent agents or serialize
dispatch into a pool; the current `WorkerPool` must not be concurrently reused through a
cloned `SweepPlan`. Sharing immutable graph buffers is independent of scheduling workers.

If one agent is late, the default is to slow the **whole session** and report lag. A crashed
agent pauses/fails the match rather than silently becoming a neutral or scripted opponent.
A realtime external world that cannot pause requires a separate declared deadline/hold-last
policy, dropped-action telemetry and a different determinism claim. Do not hide that policy
inside the environment adapter or use wall-clock completion order as an action tie-breaker.

### 5.3 Match state and learning

Every agent has its own seed, calibration, gain vector, eligibility, reward totals and
stimulation cooldown. Derive seeds deterministically from a stored master seed and stable
agent ID; do not use thread scheduling or the default identical seed for every fly.

For an initial fighting-game task:

- Both agents receive the same shared camera unless the game has genuine private views.
- Controller-port swaps and seed repeats are part of evaluation; wins alone are confounded
  by character, spawn, arena, action interface and side advantage.
- Award positive, explicitly attributed events such as a scored hit/round win; define
  damage/self-damage/team attribution and duplicate detection before turning learning on.
  A loss need not produce a negative reward; changing reward doctrine is a separate decision.
- Episode transitions may retain learned gains while clearing transient traces, or create
  fresh brains for controlled trials. Record which policy was selected.
- Do not select a “best checkpoint” separately for each player in a shared world. There is
  one world state. Tournament scores and historical results should not rewind with a match.
- Disable sugar for balanced evaluation. If enabled for a show, target a named agent under
  a documented rule and log the intervention; do not call that an uncontrolled fair benchmark.

### 5.4 Checkpoint and recovery semantics

Distinguish three operations:

| Operation | Restored/reset state |
| --- | --- |
| Crash resume | Coherent environment, every agent, scheduler remainders, task ledgers, pending actions/commands and executor state at one boundary |
| Task recovery | Policy-defined world rewind/reset and per-agent transient clearing; continued gains/brain clocks only when declared, as in the legacy ratchet |
| New episode | Task initial world state, explicit retained/fresh agent policy, new episode identity; session event sequence remains monotonic |

Use a new session envelope version with a manifest mapping stable agent IDs to chunks and
listing graph/model/profile/backend/content/task/state-format identities. The current
envelope restricts chunk names to letters; do not simply append `agent/1/membrane` to it.
Specify a new container format or an explicit manifest-to-valid-chunk-name indirection.

Validate every participant into staged state before mutating any live participant. A failed
environment import must not leave half the brains restored. For an external emulator,
restore a replacement stopped process when transactional in-place validation is impossible.
Capture at the action barrier with no backend step in flight. Reuse atomic payload write,
manifest commit, hot/durable tiers and off-thread serialization; bound outstanding snapshot
jobs so repeated copies cannot exhaust memory under slow storage.

Exact replay requires action-executor and admission state, not just the neural envelope.
Legacy macros intentionally discard transient execution on restart; preserve that behavior
for v1 and label it as legacy continuation semantics, not exact session replay. New sessions
persist all behavior-affecting state or explicitly restart an episode under a documented rule.

Keep `FLYSIM01` readable through a legacy adapter. Never silently rewrite a checkpoint on
read. Conversion is an explicit offline operation writing a new directory and run record.
Environment identity includes title/content digest, backend build/configuration, relevant
platform/native state format, and patch/symbol provenance; state size alone is not sufficient.

## 6. Feed, stage and audience modules

### 6.1 Feed v2 is required

V1 is not a generic multi-agent format. Its attachment map forbids duplicate kinds, so two
`spikes` arrays cannot coexist; its button mask and 160×144 image are Game Boy-specific.
Platformer counters are currently folded onto names such as `pokedex` and `wildwin`.
Extending those conventions to fighting games would preserve syntax while losing meaning.

Keep v1 stable for the legacy session. Introduce a negotiated v2 or a separate `/v2/feed`
endpoint, with a descriptor delivered before dependent snapshots and available on reconnect.
This is the application's browser/presentation gateway over Flybus, not another internal
bus. Native participants use the same RPC/pub-sub protocol for all framework communication.
The contract change must update Rust, TypeScript, schema, fake service, fixture player,
stage and bridge together. Proposed shape:

```text
SessionDescriptor
  sessionId, protocol, descriptorRevision, environment/task identities, clock definition
  agents[{agentId, profileId, datasetId, neuronCount, roles, controlPort}]
  ports[{portId, controllerSchema}]
  views[{viewId, dimensions, pixelFormat, sensory/broadcast use}]
  audioStreams[{streamId, sampleRate, channels}]
  assets[{id, contentHash, datasetIndexHash, localUrl, license/credit}]

SessionSnapshot
  descriptorRevision, seq, sessionTick, episodeId, environmentTime, wallTime, status
  agents[{agentId, brainTime, rates, learning, selectedAction, actualControls, stimulation}]
  progress: tagged task payload; events: scoped and sequenced
  attachments[{id, kind, ownerId, byteLength, format, mediaTimestamp}]
```

Use bounded, schema-validated tagged payloads/namespaced task extensions, not arbitrary
unlimited JSON or executable server-supplied UI. Dataset identity and neuron index mapping
must accompany spike geometry: matching bitset length alone cannot establish alignment.
Include descriptor revision in every snapshot and reject stale/mismatched buffers. Large
monotonic IDs/times use a specified safe-integer bound or decimal strings across languages.

Publish one shared camera/audio stream once, not once per agent. Independent sessions may
have independent streams. Timestamp audio/video to the session clock; define discontinuities
on reset, reconnect and lag. Latest-value video/telemetry may drop, while audio needs a
bounded timestamped buffer and explicit gap handling. Durable event IDs permit recovering
missed events; a drop-oldest snapshot feed is not an exactly-once event log.

At 640×480 RGBA, native frame production is 36.864 MB/s at 30 Hz or 73.728 MB/s at 60 Hz.
Publish one immutable artifact and pass owned references through Flybus to all consumers.
Bytes stay outside router messages; producer/read/copy costs still need measurement. A
renderer retains its handle past message drop; cached RPC results and latest retention also
own data until release. Last-owner GC replaces coordinator-managed slots/reader acknowledgments.
Resizing, overlays, codecs and streaming are presentation-layer choices, not bus requirements.

### 6.2 Stage composition

Extract `createSessionStore()` rather than adding `agent2` fields to the singleton `hot`.
Own rate scalers, button afterglow, ticker state, fixture clock and audio queues per session/
agent. Keep one page paint scheduler; register surfaces against explicit view/agent IDs.
Geometry is loaded by descriptor/hash through an asset manifest, replacing the hardcoded
FAFB route in both `App.tsx` and `vite.config.ts`.

Retain the existing Game Boy layout as a presentation plugin. Add composition primitives for
a shared match view with two agent summaries, or independent session tiles with one focused
audio source. A generic fallback shows status, media, controls and task labels without
inventing Pokémon counters. Unknown optional extensions can be omitted; unknown required
capabilities or mismatched descriptors must be visible rather than silently showing Pokémon.

Combine common framework measurements with application-owned state/events. Describe values
by owner, type, units, range and timestamp; distinguish zero, unknown, unsupported and stale.
Game-specific progress/collections remain validated schema extensions. The application defines
its supervisory/story behavior alongside its UI, not inside a mandatory generic Director service.

Keep UI runtime dependencies out of the numerical package. The current `@flybrain/brain`
view exports and optional Three.js peer can remain compatibility re-exports when viewer
geometry/helpers move to a dedicated view module. Controller labels belong in controller
schemas, not a UI import of the neural package's Game Boy preset.

Review actual layout proposals as PNGs under `apps/stage/mockups/`, with existing legibility,
phone-scale and browser gates. This document proposes data/layout boundaries, not screen
copy or a replacement for visual sign-off.

### 6.3 Audience interaction

Keep Twitch authentication/EventSub and template-only replies in the application bridge. Extract a
session-targeted interaction client with explicit `sessionId`, optional `agentId`, interaction
kind and idempotency key. A multi-agent request with no target is rejected unless a fixed,
declared target policy exists; presentation focus must never choose the recipient.

Service admission owns per-session, per-agent and global limits. A profile advertises its
supported stimulation capability; an agent without PAM support returns unsupported rather
than pretending to accept “sugar.” `!stuck` becomes task-aware (ladder time versus round
time), while chat remains broadcast-scoped and independent of neural state. Future boons
use task/backend-declared capabilities with explicit target, timing and outcome; gifts do
not automatically become earned learning rewards. These need separate capability/admission
contracts before enabling them, carried over the same bus rather than an arbitrary write API.

For redemptions, persist the resolved target and request identity before retrying. Distinguish
an RPC timeout (HTTP on legacy v1) from a definite refusal: it may occur after the sim accepted the
effect. V2 needs a durable or explicitly recoverable deduplication/status contract so bridge
restart cannot apply the same pulse twice or retarget a redemption to a new match. Do not
promise exactly-once behavior from the bridge intent log alone. Existing v1 remains as-is.

### 6.4 Deployment and observability

A deployable composition selects runtime binary, backend/task, agent profiles, dataset
bundles, view assets, session state namespace and broadcast layout. Generate service config
from that manifest plus the operator's external environment. Keep tokens and network-specific
values outside this repository. Package viewer artifacts separately from the full simulation
graph so the browser does not need every edge.

Preserve existing process isolation: sim, bridge, browser, capture, local relay and Twitch
push can restart independently. One coordinator/session with separate worker processes is
the default composition; a shared match remains one logical failure/recovery group across
those workers. A worker restart cannot silently rejoin. Router failure also invalidates its
ephemeral handles/routes. Multi-session placement and presentation composition are application
deployment concerns; router scope sets an explicit shared-failure boundary.

Metrics distinguish session lag, environment step cost, per-agent step cost, barrier wait,
snapshot drops, audio discontinuities, checkpoint queue age and resource budget. Bound agent
labels to configured IDs; never label metrics by viewer name or arbitrary event text.
Health must distinguish paused, slow, disconnected backend and dead agent. A generic
watchdog cannot treat “no new Pokémon tiles” as a stall detector for all tasks.

Deployment preflight checks every referenced profile/bundle and complete checkpoint identity
before selecting a release. Switching the binary back does not convert newer state: retain
the previous release's state namespace for rollback. Twitch publishing still requires the
operator's explicit approval for that run; implementation benchmarks use local sinks.

## 7. Cleanup strategy: extract, then reorganize

Prioritize coupling that prevents a second application, rather than renaming everything.

1. **Characterize behavior and identify state owners.** Capture deterministic synthetic
   observation→action→reward traces and legacy restore outcomes before moving code.
2. **Remove task construction from anatomy.** Introduce separately hashed readout bindings;
   leave the current artifact and macro-role merge path frozen for legacy compatibility.
3. **Extract orchestration from transport.** A session step returns observations/events and
   capture requests; it does not serialize HTTP/feed headers. `flysim` owns listener setup,
   wall-clock publication and systemd integration.
4. **Split emulator from task.** Move binjgb behind an environment implementation, preserving
   FFI/cache behavior. Move map/exit/objective queries into Pokémon task interfaces rather
   than forcing every future game to implement them.
5. **Split task recovery from storage.** The ratchet decides a task transition; storage
   commits an opaque coherent capture. Matches use round resets, not milestone archives.
6. **Version observation/control at the boundary.** Internal typed session snapshots become
   v1 or v2 through adapters; core modules do not depend on wire enums.
7. **Make stage state instantiable and assets descriptor-driven.** Preserve hot/cold cadence
   and fixture determinism while removing singletons and hardcoded dataset selection.
8. **Only then move directories/extract packages.** Keep re-exports/CLI wrappers during the
   move, fix build/CI/vendor paths, and compile tiny consumers proving Rust/TS libraries can
   be used without starting Twitch, a browser, or an emulator.
9. **Reconcile documentation.** Separate current contracts/reference from dated deployment
   history. Audit contradictory “raw buttons only,” weighted-macro, throughput, token/setup,
   and training-improvement claims against code. Update templates and scientific limitations
   with actual profile capabilities, not a new generic claim of biological fidelity.

Do not introduce a generic reward engine, universal memory address model, central plugin
marketplace, or per-neuron network transport. Those abstractions have no demonstrated second
consumer and would obscure the useful, small seams already present.

## 8. Implementation plan and acceptance gates

Each row is a reviewable change or small workstream, not one large feature branch. Follow
the repository's branch/worktree build-and-review workflow. Contract changes precede their
consumers. No estimate here assumes an emulator backend or biological mapping already works.

| Phase | Deliverable and principal files | Dependencies | Acceptance / stop condition |
| --- | --- | --- | --- |
| P0 — baseline | Trace fixtures and boundary tests around `Sim::step_frame`, restore, dataset loader, stage singleton behavior; current-state documentation inventory | None; reconcile open branches first | Current TS/Rust goldens and legacy API/feed fixtures pinned; behavior ledger distinguishes intentional transient reset from exact replay |
| P1 — identities | Dataset/profile manifest, role resolver, behavior hash, synthetic fixtures; new strict validators in both languages | P0 | Missing/ambiguous roles fail; wrong profile refuses restore; FAFB legacy fingerprints/version strings unchanged |
| B1 — Flybus | Small Rust router/client with RPC, pub/sub, owned artifacts and GC; see BUS-01..03 in the contract implementation guide | Can start alongside P0/P1; no dataset/emulator dependency | Same semantics in-memory/Unix sockets; cache/retention holds safe; 480p three-reader test; no raw binary in message envelopes |
| M1 — MaleCNS import | `tools/datasets/malecns`, source lock, inventory/exclusion report, schema-compatible baseline bundle, attribution | P1 | Counts reconcile to selected inventory; reproducible output hashes; graph invariants and both loaders agree; no runtime downloads |
| M2 — headless characterization | Profile-specific L1/sensor binding, role/readout audit, learning-off/on benches and new goldens | M1 | Stable/finite activity measured over repeated seeds; no silent empty populations; exact TS/Rust state agreement; unsupported inputs remain marked unsupported |
| M3 — task integration | Explicit MaleCNS profile selected by service config and matching stage assets, fresh state namespace | M2 and minimal descriptor/asset support from P5 | Fresh brain on audited game state; sugar capability verified; stage index/geometry identity correct; one-hour local soak plus restore drill; no automatic promotion over FAFB |
| P2 — environment/task split | Environment contract, binjgb wrapper, task-specific inspector; facade for existing `flybrain-gb` imports | P1 | Legacy action/reward traces identical; ROM-free tests pass; optional ROM-backed sample confirms stepping/audio/state behavior |
| P3 — session library | Single-agent session owns clocks, agent/task/executor state; service composes bus-connected workers | P2, B1 | Same legacy order and compatibility; renderer/bridge restart leaves run intact; fake backend runs without binjgb/ROM/Twitch |
| P4 — multi-agent + persistence | Two-agent synthetic arena, action barrier, isolated state, session envelope/recovery and failure behavior | P3 | One world step per batch; no port/order advantage; resume/parallel-vs-sequential equivalence; failure cannot partly commit a match |
| P5 — feed/control v2 | Descriptor, scoped snapshots/events/media, targeted stimulation and idempotency, TS/Rust schema fixtures/fake server | P1, P3; validate with P4 fixture | V1 still works for legacy; multi-agent attachments cannot collide; unknown target/profile rejected; retry/reconnect tests pass |
| P6 — modular stage/bridge | Instantiable stores, dataset assets, shared/independent views, target-aware commands/redemptions | P4–P5 | PNG review for two-agent layout; browser legibility/fixture tests; correct audio ownership and no agent cross-talk |
| E1 — new emulator spike | Select title/backend; bus-connected helper or native wrapper; record native frame, ports, inspection and restore capabilities | P2, can run beside P4–P6; framework integration uses B1 | Reliable bounded step + simultaneous controls, pinned content/backend identity, reproducible state round trip; stop before task implementation if unavailable |
| E2 — fighting-game vertical slice | Two flies, chosen game task, analog/digital readout, episode logic, attributed rewards, match view | E1, P4–P6 | Repeated local matches and side swaps; measured compute headroom; documented scaffold and interventions; win-rate claims require controls |
| P7 — packaging/reorg | Optional root Rust workspace move, library consumers, profile-based release/asset manifests, updated infra and docs | Useful second backend + P6 | Four merge suites and affected browser gates pass; old composition deploys locally; preflight rejects incompatible multi-agent state |

M3 may use a small v1-compatible **additive descriptor extension**, if contracts and both
consumers are updated and the existing frame semantics stay unchanged. It must not publish
MaleCNS spikes under implicit FAFB geometry. Full multi-agent publishing still requires v2.

### 8.1 A concrete first alternative-emulator spike

Before committing to Melee/Dolphin or an N64 backend, establish:

- An exact title/version and backend revision, available to the operator externally.
- A supported pause/advance boundary with all configured controller ports applied together.
- Whether rendering is required for stepping and whether frame capture is synchronous.
- Sample rate/channel metadata, audio latency and timestamps.
- Analog sticks/triggers and button semantics; neutral state on disconnect.
- Save-state completeness, version/platform constraints and reproducibility after restore.
- Supported task-state inspection (match/round/port state) without guessing memory offsets.
- Headless/runtime packaging, process lifecycle, resource cost and failure behavior.

A library used for competitive tooling may expose controller and game-state APIs without
supporting arbitrary frame stepping or faithful visual capture. Verify capabilities instead
of assuming its name solves integration. Prefer a pinned private backend process if native
embedding would force emulator internals into our Rust runtime. Desktop keyboard automation
is unsuitable for simultaneous deterministic multi-port input.

Start with synthetic constant/alternating controller traces and a test opponent before neural
control. Such traces are backend tests, not public “fly playing” footage. Then validate one
agent, two agents, episode boundaries, crashes and resume in that order. No copyrighted game
content is added to source control or test fixtures.

### 8.2 Validation matrix

**Numerical/format:** existing `core/tests/golden_{toy,real,agent,restore,platformer,versions}.rs`
remain gates. Add pinned MaleCNS subgraph fixtures and a full-artifact optional golden run,
with generated TS goldens and exact Rust comparisons. Include malformed CSR, missing roles,
wide IDs, hemisphere/coordinate errors, overflowing weights, and graph/profile mismatch.
The subgraph tests prove arithmetic/loader agreement, not full-CNS dynamics.

**Scheduler:** synthetic backend asserts one advance per complete batch; swap agent evaluation
order, vary worker count, and inject a late/failing participant. Check equal observation
boundaries, deterministic seeds, no cross-agent gains/holds/stimulation, correct tick remainder,
and no reward twice at an episode boundary. Mixed profiles are allowed only if their clocks
and capabilities satisfy the same session contract.

**Persistence:** kill/fault injection around capture/write/manifest commit; corrupt one agent
chunk, backend state or profile hash; verify all-or-none restore and fallback reporting.
Compare uninterrupted and resumed new-session action/state traces. For legacy runs compare
against the documented transient-reset behavior instead of demanding a newly invented one.

**Protocol/UI/bridge:** cross-language v1/v2 fixtures; two agents with different neuron counts;
shared and private views; out-of-order descriptor/media, missing optional attachments, stale
snapshots and reconnect; replay seeking; duplicate redemption, lost HTTP response and bridge
restart; explicitly unsupported stimulation. Visual changes require PNG review and browser
tests, not prose approval of a hypothetical layout.

**Performance/science:** benchmark full graph and thresholded graph with learning disabled
and enabled, fixed sensory traces, multiple seeds and side swaps. For game-performance
claims compare against random/readout baselines and learning-off, reporting scaffold,
recovery, intervention and episode policies. Measure sustained real-time factor and tail
latency under two/four flies plus actual browser/capture load; do not extrapolate a single
kernel throughput figure to an entire show. Initial target is ≥1.0× sustained at the declared
agent count with p99 step time within its cadence budget and no growing queues; select a
resource/headroom margin from the measured backend before release.

Before each merge run the repository-required `npm test`, `npm run typecheck`,
`cargo test --workspace` from the Rust workspace, and `infra/tests/lint.sh`. Run affected
Playwright/PNG gates for stage changes. The existing committed FAFB real-data goldens remain
mandatory. New full-MaleCNS integration jobs and ROM-backed tests are explicit optional jobs
with recorded skips, never a hidden network/ROM dependency of normal CI.

## 9. Decisions and open questions

**Recommended decisions now:** preserve FAFB as the baseline; use official MaleCNS provenance;
freeze legacy arithmetic/identities; version profile behavior independently; make environment
and agent separate objects; use one session barrier for a shared match; retain static plugins
and process/session isolation; introduce v2 instead of stretching Game Boy fields indefinitely.

**Questions answered by spikes rather than assumptions:**

1. Which MaleCNS neuron inventory and confidence/threshold policy will be the published bundle?
   Can we explain the differing inventories and quantify left/right sensory coverage?
2. Do existing LIF parameters give useful, stable activity on MaleCNS? If not, which explicit
   profile calibration is justified, and does a different neural model warrant separate work?
3. Are verified L1 mappings adequate, or is the intended project really an embodied sensory
   simulation requiring new encoders and VNC feedback?
4. Which Smash title/backend can satisfy deterministic stepping, simultaneous ports, media
   capture and restoration at acceptable cost?
5. How many simultaneous brains fit the actual budget, with which mix of within-brain versus
   between-brain workers? Is a compressed media path required?
6. Which match reset/learning/intervention policy defines the show, and which defines a
   controlled comparison? They should be separate run configurations.

Success is not just “another connectome loads” or “a second pad moves.” It is a new session
assembled from modules whose anatomy, numerical model, controller, task, recovery and
presentation assumptions are explicit, testable and reusable without changing the old fly.

## 10. Sources and scope of the analysis

Repository evidence is enumerated in section 2 and tied to the baseline commit above. Existing
reference documents: [dataset format](../dataset-format.md), [model](../model.md),
[plasticity](../plasticity.md), [readout](../readout.md), [limitations](../limitations.md),
[macros](macros.md), [architecture tour](../architecture-tour.md), and
[contribution/compatibility rules](../../CONTRIBUTING.md). Historical status notes are not
evidence that an unmeasured experiment succeeded.

External sources inspected 2026-09-18:

- [Official MaleCNS overview](https://www.janelia.org/project-team/flyem/male-cns-connectome):
  anatomical coverage, collaboration, release dates and licensing statement.
- [Official MaleCNS downloads](https://male-cns.janelia.org/download): versioned bulk tables,
  confidence cutoffs, segment versus neuron distinction, coordinate units and API guidance.
- [FlyWire overview](https://flywire.ai/): FAFB reconstruction provenance and brain coverage.
- [Codex dataset listing](https://codex.flywire.ai/): portal inventory counts, which are not
  assumed to be identical to neuPrint query inventories or local runtime graphs.
- [Minecraft fly README](https://github.com/blendi-remade/fly-brain-minecraft/blob/main/README.md)
  and [provenance](https://github.com/blendi-remade/fly-brain-minecraft/blob/main/PROVENANCE.md):
  a separately authored MaleCNS derivative, thresholding and mapping decisions, and disclosed
  sensory/motor limitations. These moving links are comparison material, not a locked data
  dependency; M1 must acquire its own official source lock.

This analysis reads the current implementation and upstream documentation. It does not
download/build the full MaleCNS dataset, independently validate the Minecraft benchmarks,
run a new emulator, or establish a performance/behavioral improvement. Those are explicit
deliverables with gates above.
