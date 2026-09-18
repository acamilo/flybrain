# Implementation backlog: MaleCNS and modular sessions

Status: **planned, not started**. Written 2026-09-18. Companion to the
[design and code analysis](malecns-modular-sessions.md), based on `f7bc13a`.

This is the execution order for that proposal. It does not authorize deployment or a live
stream. Reconcile the baseline with merged macro/shop/recovery work before implementation.
Existing feed/control contracts and TypeScript oracle rules remain binding.

Concrete second-game audit: [Melee framework and emulator plan](melee-framework-audit.md).
Its MELEE-01/02 spikes specialize EMULATOR-01 below and can proceed alongside framework
extraction; they do not depend on importing MaleCNS first.

## 1. Delivery strategy

Deliver working vertical slices; keep the existing FAFB/Game Boy composition usable throughout.

1. Establish a behavior baseline and explicit dataset/profile identities.
2. In parallel workstreams, characterize MaleCNS and extract the single-agent session runtime.
3. Demonstrate two isolated brains driving one ROM-free shared environment.
4. Expose that session through versioned feed/control contracts and a multi-agent broadcast.
5. Integrate a specifically chosen alternative emulator after its capability spike passes.
6. Finish physical package reorganization once the second consumer proves the boundaries.

**First milestone:** a reproducible headless MaleCNS run and a reusable single-agent session.
**Second milestone:** two flies in a synthetic arena with coherent resume and a local broadcast.
**Third milestone:** two flies in the chosen fighting game, with documented task scaffolding.

No elapsed-time estimate is committed before the import and emulator spikes establish their
unknowns. Split an item further when its contract and implementation cannot be reviewed together.

## 2. Work queue

Every item starts pending. Branch names are suggested implementation branches, not branches
already created. Builders use separate worktrees; the coordinator reviews contracts and results.

### FOUNDATION-01 — Pin existing behavior

- **Branch:** `test/session-baseline`
- **Depends on:** reconciliation with current main and related open work.
- Record effective legacy configuration, fingerprint, version strings and frame ordering.
- Add a ROM-free transition harness around the service's frame orchestration; capture brain
  ticks, decoded actions, reward application and recovery effects with explicit clocks.
- Distinguish exact numerical replay from intentional macro/hold/transient reset on restore.
- **Done:** existing goldens pass; a trace fixture detects reordered vision/reward application;
  legacy feed/API fixtures and compatibility identity are unchanged.

### FOUNDATION-02 — Specify bundle and behavior identities

- **Branch:** `feat/brain-profile-contract`
- **Depends on:** FOUNDATION-01.
- Specify dataset manifests, original-ID mapping, anatomical roles, sensory bindings,
  readout bindings and composite behavior identity in a focused contract.
- Add profile resolution/validation around the existing core in TypeScript and Rust.
- Keep schema-1 fingerprinting and the legacy macro-role exception behind the legacy path.
- Add strict graph validation for new bundles, shared invalid fixtures and profile mismatch tests.
- **Done:** empty required populations, malformed CSR and incorrect profile restores fail;
  legacy FAFB artifacts and default numerical version strings remain unchanged.

### DATA-01 — Acquire and normalize MaleCNS

- **Branch:** `feat/malecns-import`
- **Depends on:** FOUNDATION-02.
- Build a source-specific importer from official v1.0 tables with checksummed source locks.
- Reconcile the Codex versus neuPrint inventories, or explicitly select and document one.
- Preserve raw contact counts, transmitter evidence, original IDs and missing-data indicators.
- Emit deterministic graph bundles, weight≥1/weight≥5 comparison variants, and an exclusion/
  clipping/coverage report. Decide schema-1 feasibility from measured weight ranges.
- Add attribution and license records with actual artifacts; keep raw downloads out of git.
- **Done:** repeated builds match; endpoint/role/index invariants pass; both loaders agree;
  no service startup or ordinary unit test needs a download.

### DATA-02 — Characterize MaleCNS in the current kernel

- **Branch:** `feat/malecns-baseline-profile`
- **Depends on:** DATA-01.
- Audit L1 geometry, hemisphere handling, KC/MBON/PAM mappings and brain-versus-VNC motor roles.
- Define a versioned fixed readout; keep task action partitions out of anatomical truth.
- Run learning-off first, then learning-on, using fixed sensory traces and multiple seeds.
- Generate TS reference goldens and compare Rust exactly; measure activity, saturation,
  initialization, memory and per-phase latency for both graph thresholds.
- **Done:** publish a reproducible characterization report and profile choice. Stop task
  integration if required mappings are missing or dynamics are unusable; any recalibration
  becomes a named profile rather than an edit to the legacy model.

### RUNTIME-01 — Separate environment execution from task interpretation

- **Branch:** `refactor/environment-task-boundary`
- **Depends on:** FOUNDATION-02.
- Specify controller ports, digital/analog controls, rational cadence, media descriptors,
  observation ownership and backend capabilities.
- Wrap binjgb as the first environment; retain Game Boy FFI/cache/state behavior.
- Keep Pokémon memory inspection, objective routing, macros and reward rules in its task.
- Preserve existing imports through a facade; avoid simultaneous directory moves.
- **Done:** existing single-agent action/reward traces match and a fake environment can be
  driven through the same boundary without importing binjgb or task-specific addresses.

### RUNTIME-02 — Extract the single-agent session

- **Branch:** `refactor/session-runtime`
- **Depends on:** RUNTIME-01.
- Move deterministic agent/environment/task orchestration out of `Sim` into a library.
- Keep HTTP, WebSocket serialization, wall-clock publication and process supervision in flysim.
- Give session clock, action executor, task ledger and recovery state explicit owners.
- Wrap the existing composition with legacy ordering, checkpoint and reset semantics.
- **Done:** headless consumer runs a session without Twitch/browser; the legacy composition
  passes its traces and restore tests; a slow snapshot consumer cannot stall simulation.

### RUNTIME-03 — Add synchronized multi-agent sessions

- **Branch:** `feat/multi-agent-arena`
- **Depends on:** RUNTIME-02.
- Implement a ROM-free two-player arena and per-port controller ownership.
- Evaluate both brains against one observation boundary; apply one complete action batch;
  advance the world once. Start sequentially, then verify parallel execution equivalence.
- Isolate RNG, stimulation, decoder holds, gains, traces and rewards per agent; share only
  immutable topology. Enforce a total worker budget and single-dispatcher pool ownership.
- Define participant failure, lateness and episode reset policies.
- **Done:** no cross-agent state leakage; swapping evaluation order leaves results unchanged;
  one failed participant cannot accidentally advance a half-controlled match.

### STATE-01 — Capture and resume whole sessions

- **Branch:** `feat/session-checkpoints`
- **Depends on:** RUNTIME-03; specify the state contract during RUNTIME-02.
- Define the new envelope/manifest and preserve the `FLYSIM01` reader.
- Capture all agents, environment, task/executor/admission state and clock remainders at one
  boundary. Bound off-thread write jobs and retain atomic manifest commit semantics.
- Validate all components before installing any restored state; define external-backend staging.
- **Done:** uninterrupted and resumed synthetic matches agree; corrupting any participant
  refuses the generation without partial restore; crash-injection fallback tests pass.

### WIRE-01 — Introduce session feed/control v2

- **Branch:** `feat/session-protocol-v2`
- **Depends on:** FOUNDATION-02, RUNTIME-02; use RUNTIME-03 fixtures for integration.
- Write binding contracts before consumer implementation: descriptors, scoped agents/events,
  media IDs/timestamps, task progress, targeted stimulation and retry/idempotency behavior.
- Implement Rust/TS codecs, schemas and a fake server; preserve the legacy v1 surface.
- Specify descriptor reconnect behavior, asset/index identity, bounded message sizes and audio gaps.
- **Done:** cross-language fixtures pass for unequal neuron counts and shared/private views;
  duplicate attachment kinds no longer collide; ambiguous targets and incompatible schemas fail.

### PRESENTATION-01 — Compose multi-agent stage and bridge

- **Branch:** `feat/multi-agent-broadcast`
- **Depends on:** WIRE-01, RUNTIME-03.
- Replace stage store/scaler singletons with session/agent instances and one paint scheduler.
- Resolve geometry from hashed descriptors, preserve the Game Boy presentation, and add a
  shared-match layout with explicit audio ownership.
- Route bridge commands/redemptions to persistent session/agent identities; test lost responses,
  retries and restart without applying an interaction twice or to a different agent.
- **Done:** local synthetic match broadcast works; two-agent PNGs receive operator review;
  browser/fixture/legibility checks pass; bridge remains template-only and quiet-mode capable.

### DATA-03 — Run MaleCNS through the complete application

- **Branch:** `feat/malecns-session`
- **Depends on:** DATA-02 and descriptor-aware assets from WIRE-01/PRESENTATION-01.
- Expose explicit profile selection and create a fresh MaleCNS state namespace.
- Verify task/controller bindings, stimulation capability and displayed anatomy identity.
- A narrow single-agent descriptor extension may ship earlier only with matching v1 contract
  and consumer updates; do not publish MaleCNS spikes as implicit FAFB indices.
- **Done:** local one-hour soak and restore drill pass; paired learning-off/on observations
  are recorded without claiming improved play; FAFB remains available unchanged.

### EMULATOR-01 — Establish the alternative backend's capabilities

- **Branch:** `spike/fighting-game-backend`
- **Depends on:** RUNTIME-01; can proceed alongside later runtime work.
- Choose the exact game/version and emulator; Melee/Dolphin is a candidate, not a commitment.
- Prove pause/step, simultaneous ports, analog input, frame/audio capture, state inspection,
  save/restore, process lifecycle and achievable cadence with synthetic controller traces.
- Prefer private IPC if embedding would leak emulator internals into the session library.
- **Done:** capability report includes pinned backend/content identity and reproducible results.
  If bounded stepping or coherent restore fails, stop and revise the backend/requirements
  before writing neural game logic. No game content enters repository fixtures.

### EMULATOR-02 — Build the two-fly fighting-game slice

- **Branch:** `feat/two-fly-fighting-game`
- **Depends on:** EMULATOR-01, STATE-01, PRESENTATION-01.
- Implement fixed controller mapping, match/round interpretation, positive attributed rewards,
  observation policy and episode recovery. Display selected actions and actual controls.
- Validate one fly, two flies, round transitions, backend failure and resume in that order.
- Run side swaps and repeated seeds; compare learning-off and simple control baselines before
  interpreting win rates. Separate show settings from controlled evaluation settings.
- **Done:** repeated local matches sustain declared cadence; restoration/failure policies work;
  scaffold, interventions and limits are documented; reviewed match presentation is legible.

### PACKAGE-01 — Finalize reusable packages and release compositions

- **Branch:** `refactor/reusable-package-layout`
- **Depends on:** a useful second backend plus PRESENTATION-01.
- Extract proven crate/package boundaries from the design's module table; preserve facades.
- Move the Rust workspace only in a mechanical follow-up if it makes library consumption clearer.
- Update CI/build/vendor/golden paths, dataset/view asset packaging and compatibility preflight.
- Add minimal external-style Rust/TS consumers and a synthetic example composition.
- Reconcile current docs, stale template explanations and licensing/asset attribution.
- **Done:** both legacy and new compositions package successfully; incompatible state is
  rejected before release selection; libraries run without importing broadcast services.

## 3. Dependency map and first execution batch

```text
FOUNDATION-01 → FOUNDATION-02 ┬→ DATA-01 → DATA-02 ───────────────→ DATA-03
                            └→ RUNTIME-01 → RUNTIME-02 → RUNTIME-03 → STATE-01
                                  │             └→ WIRE-01 ───────┐
                                  └→ EMULATOR-01          PRESENTATION-01
                                                              │
                         STATE-01 + EMULATOR-01 + PRESENTATION-01 → EMULATOR-02
                                         second backend + presentation → PACKAGE-01
```

The item dependency lists are authoritative; the diagram is a reading aid.

When implementation begins, take **FOUNDATION-01 only** as the first build task. Then review
FOUNDATION-02's contract before assigning DATA-01 and RUNTIME-01 to independent worktrees.
Contract/schema authorship is serialized to avoid conflicting definitions. Deployment host
work remains serialized under the repository's claim protocol.

## 4. Definition of done for every implementation branch

- Scope and intentional behavior changes are stated; compatibility impact is explicit.
- Meaningful boundary tests cover the changed behavior; the TS oracle is not adjusted to
  accommodate Rust output. Existing committed real-data goldens stay mandatory.
- `npm test`, `npm run typecheck`, `cargo test --workspace` (Rust workspace) and
  `infra/tests/lint.sh` pass before merge. Visual changes also pass applicable Playwright
  checks and PNG review. Optional full-MaleCNS/ROM runs record skips honestly.
- Performance-sensitive changes report representative activity, agent count, thread budget,
  memory and tail latency. New experiments state what is modeled versus handwritten.
- Review the complete diff, merge with `--no-ff` when authorized, and update this queue with
  commit, evidence and unresolved follow-ups. Rollback includes compatible state, not just code.

## 5. Decisions needed before the relevant work starts

| Decision | Deadline | Default recommendation |
| --- | --- | --- |
| MaleCNS inventory/filter policy | DATA-01 completion | Official versioned source; retain both threshold variants until measured |
| MaleCNS sensory/readout profile | DATA-02 | Audited L1 mapping with existing numerical model first |
| Exact fighting game and backend | EMULATOR-01 | Evaluate one concrete title/backend rather than supporting a console family at once |
| Number of flies and target resource budget | RUNTIME-03 performance gate | Two first; characterize four before promising it |
| Learning retention and sugar in matches | EMULATOR-02 task contract | Retention explicit; stimulation disabled in controlled comparisons |
| Package publication versus monorepo reuse | PACKAGE-01 | Monorepo libraries/examples first; public package publishing later |

There is no need to resolve these now to plan another feature. This backlog is ready for
resumption at FOUNDATION-01.
