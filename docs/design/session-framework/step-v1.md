# Lockstep step protocol v1

Status: **draft 2**. This is the authoritative new-session ordering contract. All method
arrows below are RPCs through the same [Flybus router](bus-v1.md); the router itself never
implements the barrier. Read [architecture](README.md) and [session RPC](ipc-v1.md) first. Method payloads are in
[worker interfaces](workers-v1.md).

**Amendment, 2026-09-23 (operator decision of 2026-09-23).** The live Game Boy fly is ported
onto this protocol as the legacy composition of [legacy-gameboy-v1](legacy-gameboy-v1.md),
scheduled by `lockstep-v1` with one agent, one port and one world; it is no longer a separate
ordering. Its frame order is this document's transaction order (legacy-gameboy-v1 section 4
maps it step by step), so the amendments below add capabilities to the protocol and change
none of its ordering rules.

## 1. Committed boundary

At `Ready(epoch, k)`:

- Environment state is at boundary `k`, with no outstanding action batch.
- Every agent has consumed the outcome of transition `k-1 → k`, including its new encoded
  sensory input and rewards, and is ready to compute the decision for transition `k → k+1`.
- Task ledger, action-executor state, event identity and agent tick remainders agree with `k`.
- The current sensory observation may have a declared fixed render delay; its producing
  boundary is explicit. “Ready” does not imply latest wall-clock screenshot.
- No normal step operation from an older boundary may mutate the session.

Only a committed boundary is eligible for a coherent checkpoint or normal pause. Boot/reset
establishes the same invariant with no preceding reward. The public snapshot represents this
boundary, not an in-progress combination of some new agent states and an old world.

## 2. State machine

```text
Starting → Ready(k) → Preparing(k) → Applying(k) → Observing(k+1)
              ↑                                      │
              └──────────── Ready(k+1) ← Committing(k) ┘

Ready(k) → Paused(k) → Ready(k)
Ready(k) / Paused(k) → Capturing(k) → same boundary
pause requested mid-step → Committing(k) → Ready(k+1) → Paused(k+1)
any unresolved partial failure → Failed → Restoring(new epoch) → Paused(k)
terminal episode → Paused(k) → Resetting(new epoch) → Ready(0)
```

`Committing(k)` refers to completing transition `k → k+1`. Requests throughout that
transition carry `scope.step=k`; result fields identify `nextStep=k+1` where applicable.
Do not send Agent.Commit with step `k+1` merely because the observation is newer.

**Amendment, 2026-09-22.** The mid-step pause line above adds no new edge: a pause requested
during a transition is served by the ordinary `Committing(k) → Ready(k+1)` edge followed by
`Ready(k+1) → Paused(k+1)`. It is written into the machine because section 6 requires the
transition to finish first, so the only boundary such a pause can land on is the one the
transition just committed.

**Amendment, 2026-09-23 (RT-01a; operator decision of 2026-09-23).** One edge is added, for a
composition that declares a rollback policy:

```text
Ready(e, k) → RollingBack(e', k) → Ready(e', k)
```

It is taken only when the transition that reached `k` returned `episodeRequest.kind =
"rollback"`, after that transition fully committed and before the next Prepare (section 6
amendment). The boundary number does not change; the epoch does. A pause requested during it
lands on `Ready(e', k)`; any failure inside it is `Failed → Restoring(new epoch)`. Capture is
not allowed in `RollingBack`.

## 3. Transaction sequence

### Phase A: prepare all agents concurrently

At Ready(k), freeze the task's per-agent decision contexts and the coordinator's admitted
pre-step stimulation list. Inputs accepted after this cut wait for the next boundary.

Send `Agent.Prepare(scope=k)` to every active agent. Each worker:

1. Verifies its committed boundary/profile/context and applies admitted pre-step stimulation
   in deterministic command sequence order. Chat text is never included.
2. Advances the numerical model for the environment interval, using the input encoded at
   the preceding Commit (or initialization).
3. Reads rates and performs the fixed readout with the declared decision context.
4. Stores and returns `PreparedDecision`; it then enters Prepared(k) and waits for Commit.

This operation **mutates** the brain, RNG, clock and decoder. “Prepare” does not mean a
database transaction that can be rolled back cheaply. If another agent fails, do not ask a
prepared agent to prepare again or advance to the next step. Resolve/recover the whole session.

All agents see the same environment interval and the same world boundary, with only their
permitted view/context differences. Their completion order never affects port/action order.

### Phase B: build and apply one complete batch

After every PreparedDecision arrives:

1. Validate agent IDs, intent schemas and profile identities.
2. Run each task-local action executor once, in sorted agent-ID order, against coherent current
   game state, task progress/objectives and clock from this boundary.
   Direct-control profiles use an identity executor. Macro profiles are explicit extensions.

   *Amendment, 2026-09-23 (RT-01a):* the legacy composition's extension is
   `pokered-macros-v1`. "Coherent current game state" is the boundary's 64-KiB memory image
   from `O[k].inspection` plus the ROM `AssetRef` -- never a live emulator read -- and "clock"
   is the agent's brain time after its Prepare ([legacy-gameboy-v1](legacy-gameboy-v1.md)
   sections 8 and 10).
3. Assemble all configured port controls in descriptor port order; reject duplicates/missing
   ports. Uncontrolled ports are configured neutral before the epoch, not supplied ad hoc.
4. Send exactly one `Environment.Advance(scope=k, batchId, controls)`.

The environment applies all controls at its agreed boundary, advances exactly one interval,
and returns StepResult for `k+1`. It MUST NOT advance another interval while waiting for
the next request. Transport/control scaffolding may have a measured fixed latency; it must
be declared in its descriptor and conformance tests.

### Phase C: observe and evaluate the task

The coordinator receives the environment result and verifies batch identity, boundary,
cadence, inspection schema and required sensory views. A missing spectator frame is tolerable;
a missing required sensory input is not silently replaced.

Call the task's `evaluate_transition` once with old/new inspection observations and applied
controls. It returns scoped rewards/stimulation, next decision contexts, progress/events and
an optional episode request. Commit its ledger update in memory and retain the result for
this transition. No task output directly writes controllers or neural state.

**Amendment, 2026-09-23 (RT-01a).** The task may also ask for two things that happen at the
boundary this transition reaches, after Phase D, never inside it: a slot save
(`Environment.SaveSlot`, composition capability `gameboy-slots-v1`) and a rollback
(`episodeRequest.kind = "rollback"`). Both are recorded with the transition's result and
applied by the coordinator in the order *save, then rollback* (section 6 amendment). A slot
save due at a boundary completes before any `State.Capture` or FLYSIM01 export at that boundary
(amended 2026-09-23, review round 1; [legacy-gameboy-v1](legacy-gameboy-v1.md) section 16). The
retained old inspection is what makes "evaluate once against old/new inspection" possible when
the inspection is artifact-backed: the coordinator keeps `O[k]`'s image until this evaluation
finishes.

### Phase D: commit all agent outcomes concurrently

Send `Agent.Commit(scope=k)` with that agent's next sensory observation and routed outcomes.
Each agent, in this order:

1. Encodes/installs the next sensory input for the following Prepare.
2. Applies task-derived stimulation in returned event order.
3. Sums that agent's reward values in returned event order and calls reinforcement once at
   its current brain time when learning is enabled. A zero sum still follows the profile's
   specified legacy-equivalent reinforce behavior; do not optimize it away without evidence.
4. Retains the next decision context/digest and acknowledges committed boundary `k+1`.

There are no additional neural ticks in Commit. Sugar accepted for a future Prepare is not
silently merged with task reward modulation. The default synthetic learning mechanism remains
separate from the neural stimulation path.

Once **all** commits succeed, the coordinator advances its committed boundary to `k+1`,
finalizes the observation snapshot and scoped events, drops no-longer-needed artifact handles and
allows the next Prepare. If one Commit fails after others succeeded, the epoch is failed;
there is no partial-match continuation.

These phases establish logical coordination, not a distributed durable two-phase commit.
Crash recovery returns to the last complete checkpoint, not necessarily the last displayed step.

## 4. Sequence example

```text
Coordinator         Agent A          Agent B          Environment
    | Prepare(k) ------>|                |                  |
    | Prepare(k) ----------------------->|                  |
    |<-- Prepared(A) ---|                |                  |
    |<-- Prepared(B) --------------------|                  |
    | [executor + complete port batch]                      |
    | Advance(k, batch-x) --------------------------------->|
    |<-------------------- StepResult(k+1, batch-x) ----------|
    | [task evaluation; route each reward once]              |
    | Commit(k, O[k+1], R_A) ->|           |                 |
    | Commit(k, O[k+1], R_B) ------------>|                 |
    |<-- Committed(k+1) -----|            |                 |
    |<-- Committed(k+1) -----------------|                 |
    | [Ready(k+1); publish; next boundary]                   |
```

The shared camera is one immutable artifact forwarded through bus-owned deliveries; both
workers can encode it without two renders or routing two full images through sockets.
Domain RPC replay caches retain handles, so consumption by one client cannot invalidate a
promised replay. Publication uses bus pub/sub and never waits for spectator consumption.
The coordinator retains each domain request's input handles until its terminal outcome is
resolved, beyond the shorter bus-admission lifetime, so safe domain retries still have valid
attachments. If ownership is lost, fail/recover instead of sending bare expired references.
The coordinator cannot publish a committed state as soon as the faster agent answers.

## 5. Time and pacing

The environment descriptor supplies a fixed reduced `stepDuration: RationalNs`. Each agent
profile supplies `tickDuration: RationalNs`. The existing LIF adapter uses exactly one ms.

For each Prepare:

```text
accumulator += environment step duration
ticks = floor(accumulator / model tick duration)
accumulator -= ticks * model tick duration
```

Use checked rational/integer arithmetic; remainder is always >=0 and < one model tick.
Do not accumulate rounded microseconds or nanoseconds for a fractional frame period.
Persist remainder, executed tick count and warm-up offset. Language implementations must
agree on remainder fixtures. Conversion to the legacy model's f64 millisecond clock must
preserve its representable integral ticks; refuse a run exceeding the supported exact range.

Example: a synthetic 60-Hz environment with a 1-ms model tick produces 16,17,17 ticks
over three steps, totaling 50. A real backend's measured/declared emulated cadence may
differ; never substitute this example's duration for Game Boy or Dolphin clocks.

**Amendment, 2026-09-23 (PROF-02a).** The Game Boy's declared cadence is one frame of 70224
cycles at 4194304 Hz, `stepDuration = 8572265625/512` ns. The legacy service accumulates the
`f64` constant `1000 / (4194304 / 70224)` ms, which is exactly `548625/32768` ms, and every
remainder it produces is a multiple of 2^-15 ms below 32 -- exact in `f64`. The legacy
"floating remainder arithmetic" and this section's rational accumulator therefore give identical
ticks and remainders for every frame; both implementations assert it and
`fixtures/gameboy-legacy.json` records the first twelve frames (16, 17, 17, 16, ...). No legacy
exception to this section is needed ([legacy-gameboy-v1](legacy-gameboy-v1.md) section 3).

Wall time is only for pacing, health and presentation. The coordinator schedules absolute
deadlines after committed boundaries; when behind, it omits sleep and reports lag. It does
not skip world steps, drop neural ticks, or let one agent advance more slowly than another.
Only one pacing authority is active. Backend throttling and coordinator pacing must be
configured/tested so they do not unintentionally double-throttle the session.

First-version profiles have a fixed cadence within an epoch. Supporting variable-duration
world advances requires a new capability and tests before enabling it.

## 6. Initialization, pause and episodes

Initialize the environment first while stopped, obtaining observation O[0]. Bootstrap the
task and initial decision contexts, then initialize agent workers with their permitted inputs.
Agent warm-up has learning disabled; calibration occurs on settled rates; no warm-up actions
advance the environment. All required acknowledgments establish Ready(0).

A normal pause request arriving mid-step means “finish this transition, then pause.” It does
not truncate neural computation or capture half an action batch. If completing the transition
is impossible, use failure/recovery, not an apparently successful Pause acknowledgment.
Paused workers retain state and answer Status; world controls do not advance the world.

Task terminal events are evaluated and their final rewards committed once. Before another
gameplay transition, enter Paused and apply the declared episode policy. Reset uses a new
epoch/episode and step 0. A profile may retain learned gains/brain state, but must identify
exactly what is retained, cleared, warmed or recalibrated. No worker independently resets.

Changing port assignment, agent membership, model/profile, cadence or task schema requires
a new composition/epoch. Hot-join and hot-swap during an active match are not v1 capabilities.

**Amendment, 2026-09-23 (RT-01a; operator decision of 2026-09-23).** A declared rollback policy
is an episode policy that does **not** pass through Paused and does **not** start a new episode.
For `legacy-ratchet-rollback-v1` ([legacy-gameboy-v1](legacy-gameboy-v1.md) section 11), once
every Commit of the transition that reached `k` has succeeded:

1. If the same transition asked for a slot save, `Environment.SaveSlot(scope e,k)` first.
2. Choose a new epoch `e'`. `Environment.RestoreSlot(scope e',k; priorEpoch e)` returns the
   restored `O'[k]`: same boundary, `worldTime` and `engineFrame` continue, no audio chunk.
3. Coordinator-local: the task clears its transient observations; the executor cancels any
   running action and observes `O'[k]`, which yields the next decision contexts.
4. `Agent.Rollback(scope e',k; priorEpoch e)` on every agent concurrently: holds and
   eligibility cleared, `O'[k]`'s view installed, no tick.
5. With every reply in hand: `Ready(e', k)`, then the usual durable save. Every capture at
   this boundary, before or after the rollback, follows step 1.

A worker reports `capturing` during `SaveSlot` and `restoring` during `RestoreSlot` or
`Agent.Rollback`. A lost reply is resolved by [session RPC](ipc-v1.md) section 6 against the
same operation key before anything fails the epoch (legacy-gameboy-v1 section 11).

Exactly what is retained, cleared and installed is named by the policy, as this section already
requires; nothing is reset by a worker on its own initiative, and a failure at any step fails
the epoch and restores the group coherently. "Reset uses a new epoch/episode and step 0"
remains the rule for terminal episodes; a rollback keeps the episode and the step numbering
because the brain, the audience's history and the frame counter all continue across it, as they
do in the running service. The policy is single-agent: a shared competitive world must not
declare it, because it would rewind one world under every player on one player's stall, which
[state-media-v1](state-media-v1.md) section 7 forbids.

**Amendment, 2026-09-23 (RT-01a).** Sugar admission in the legacy composition reads the pulse
from the last completed commit's telemetry, so an admission decided while a transition is in
flight is at most one commit stale; the admitted stimulus still enters only at the next
admission cut of section 3, Phase A ([workers-v1](workers-v1.md) section 5 amendment).

## 7. Failure rules

| Failure point | Required response |
| --- | --- |
| Before any Prepare admitted | Reject request/config or remain Ready; nothing advanced |
| Some agents Prepared | Stop dispatch; resolve matching requests or fail epoch/group restore |
| Advance acknowledgment lost | Query/retransmit same request to same incarnation; never new batch |
| World advanced, sensory data unavailable | Fail transition; do not reward/continue using guessed input |
| Task interpretation fails | Fail epoch; prepared brains/world already changed |
| Some Commit replies missing | Resolve exact requests; no next world step until all committed |
| Worker incarnation changes | All live participants belong to an invalid epoch; restore/reset together |
| Publisher/browser disconnected | Simulation continues; bound/drop spectator work |
| Durable storage fails | Report actual failure; apply configured pause/continue-with-stale-checkpoint policy |

Retries return original results; they never recompute a decision with updated rates or new
world data. A coordinator restart has no authority to assume any remote participant's phase;
recover from a coherent checkpoint into a new epoch or start an explicitly new episode.

## 8. Required trace assertions

The synthetic integration test must record, for every transition:

- Scope, Prepare request IDs, agent/profile IDs, tick counts/remainders and decision digests.
- Complete batch ID/control digest and acknowledged world boundary.
- Observation producing boundaries and task event/outcome IDs in order.
- All Commit acknowledgments and published committed boundary.

**Amendment, 2026-09-23 (RT-01a, review round 1).** For a composition with boundary actions the
record also carries, for the boundary the transition reached:

- in behaviour, `boundaryActions`: every `Environment.SaveSlot` (slot id and saved state
  digest) and a rollback (slot id), in the order applied -- saves first, each slot once, at most
  one rollback, last. FND-01's harness compares it like any other behaviour field;
- in operational metadata, `captures`: every checkpoint capture or FLYSIM01 export at that
  boundary, in the order taken, each with the number of boundary actions already applied.
  Captures are operational because their schedule is wall-clock policy.

A trace whose capture precedes one of the boundary's slot saves, or counts more actions than
were applied, is refused. The synthetic composition records both lists empty.

Evaluate agents sequentially, concurrently, and in reversed dispatch/completion order. All
committed state/action/reward results must match, excluding wall time, request IDs and other
explicitly operational metadata. Delayed/lost/duplicate messages must not add a neural tick,
world step, reward update or controller flush corresponding to another logical step.
