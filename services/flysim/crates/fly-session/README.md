# fly-session

The lockstep session coordinator, its phase machine and a synthetic composition over
[`flybus`](../flybus).

This crate is the SESSION-01 slice of the session-framework implementation guide: the
sequential transaction of `step-v1`, driven over the Flybus router, with small fake workers
standing in for a brain and an emulator. It contains no public controller API, no implicit
best-effort retry, no real emulator and no real brain.

The domain scalars, method payloads, their validation, the canonical digests and the trace
format all come from [`fly-session-types`](../fly-session-types), the CONTRACT-01 crate. This
crate adds only what is not part of the type contract: a session-side error value, the
synthetic composition's schema and event-id derivations, and the coordinator-local
`ControllerIntent`, `PortBinding` and `AgentOutcome` that never cross the bus.

```text
Ready(k) ─ Prepare all agents concurrently ────────────> every agent Prepared(k)
         ─ one executor per agent, sorted agent-id order
         ─ one complete port batch, descriptor port order
         ─ exactly one Environment.Advance(k, batch) ──> boundary k+1
         ─ task.evaluate_transition, once
         ─ Commit all agents concurrently ─────────────> every agent Ready(k+1)
         ─ committed boundary k+1, publish, next Prepare allowed
```

## Layout

| Module | Contents |
| --- | --- |
| `types` | A facade over the [`fly-session-types`](../fly-session-types) crate, plus the session-side additions a coordinator needs |
| `clock` | The `step-v1` section 5 rational tick accumulator and the coordinator's pacing |
| `phase` | The `step-v1` section 2 state machine as an explicit edge table |
| `dedup` | The `ipc-v1` section 5 operation keys, result caches and retention |
| `worker` | The worker dispatch shell: one service, the common `Worker.*` methods, admission |
| `agent` | A fake agent worker: seeded model, mutation counter, fixed readout stub |
| `environment` | The counter arena: one complete batch per advance, one native frame |
| `task` | The task and executor traits, the deterministic counter task, the identity executor |
| `rpc` | Domain calls: `req-<U64>` serials, incarnation pinning, the retry rule |
| `coordinator` | The transaction, the trace, the failure rules and the publication boundary |
| `harness` | The runnable composition: router, two agents, one arena, one coordinator |

## What it implements

- **The transaction, in order.** Prepare all agents concurrently; run each task-local executor
  once in sorted agent-id order; assemble all configured port controls in descriptor port
  order; send exactly one `Environment.Advance`; evaluate the task once; commit all agents
  concurrently. The committed boundary moves only when every commit has succeeded.
- **The state machine**, including `Paused` and `Failed`, with every transition recorded. A
  transition the `step-v1` section 2 table does not list returns `INVALID_PHASE`.
- **The committed boundary rule.** Only `Ready(k)` or `Paused(k)` is a committed boundary; a
  snapshot publishes one of those and never an in-progress mix of new agent state and an old
  world.
- **Time and pacing** with checked rational accumulation. A 60 Hz world with a 1 ms model tick
  produces 16, 17, 17 ticks over three steps, totalling 50, with a remainder of exactly zero.
  Wall time is only pacing: when behind, the coordinator omits the sleep and reports the lag.
- **Initialization, pause and episodes.** The environment initializes first, while stopped;
  the task bootstraps; then the agents warm up with learning disabled. Nothing in bootstrap
  advances the world or produces a gameplay reward. A pause arriving mid-step completes the
  transition and pauses at its committed boundary. A terminal task event commits its final
  rewards, then the session pauses; no worker resets itself.
- **The failure rules.** A partial commit fails the epoch; an uncertain Advance is resolved
  against its original domain request id and never becomes a second batch; a worker
  incarnation change invalidates the epoch.
- **Domain deduplication over bus calls.** Same key, request and body replays its cached
  reply with fresh delivery ownership over retained artifacts; a changed body is `CONFLICT`; a
  duplicate of a running operation is `IN_PROGRESS` for that bus call while the original
  completes; an evicted record is `RESULT_EXPIRED`; a newly issued request naming an old step
  is `STALE_STEP`. `Worker.Acknowledge` releases a domain result cache, which is not a bus
  `delivery.consumed`.

## API

```rust
let harness = SessionHarness::start(Via::Unix, dir.path(), HarnessConfig::default()).await?;
harness.coordinator.bootstrap().await?;            // Ready(0), world stopped at boundary 0
let reports = harness.coordinator.run(3).await?;   // three transitions
harness.coordinator.pause_handle().request();      // finish this transition, then pause
harness.coordinator.trace.behavior();              // the step-v1 section 8 behaviour trace
harness.shutdown().await;
```

- `Coordinator::dispatch` selects `Sequential`, `Concurrent` or `Reversed` per-agent dispatch.
  All three must produce the same behaviour trace; that is a test.
- `Coordinator::injections` asks for one deliberate message fault at one step: a duplicate
  Prepare or Commit, an abandoned Advance result, an altered control batch, or a consumed
  result artifact followed by a replay. `injection_log` reports what came back.
- `Coordinator::probe_raw` sends one domain request as it stands and returns the worker's own
  terminal outcome, without letting the answer change session state.
- `AgentFaults` and `EnvironmentFaults` ask a worker for a deliberate delay or failure.

## The synthetic composition

- **Agents.** A fake model is an LCG with an explicit seed and one counter of everything that
  mutated it: ticks, stimulations, reinforcements and input installs. The worker reports that
  counter as its `progressCounter`, which is how a test proves a duplicate repeated nothing.
  The readout is a fixed stub: it reads bits of the current state, masked by the declared
  available actions, and never changes its own weights or invents a default winner.
- **Environment.** A signed counter. `inc` adds one, `dec` subtracts one, and one bipolar
  `bias` axis is carried and validated but does not move the world. Each observation seals one
  immutable 4x4 RGBA frame whose bytes carry the counter, so an agent reading its sensory view
  reads the world rather than a constant.
- **Task.** Rewards are the counter delta of each agent's own port control, with deterministic
  event ids derived from epoch, source step, rule and ordinal.
- **Executors.** The stateless identity executor only, as v1 specifies.

## Where this crate narrows or adds to the contract crate

- **Required views.** `WorldObservation::validate_against` checks the views a result carries
  against their descriptors. Requiring every *declared* view to be there at all is the
  coordinator's Phase C check, so `verify_step_result` makes it: a missing required sensory
  view fails the transition with `BUFFER_INVALID` rather than being replaced by an older frame.
- **`ControllerIntent`.** `workers-v1` section 4 calls the task and executor interfaces local
  libraries, so their types live here rather than in the payload contract. An intent is a
  `PortControl` without its port, and only the coordinator adds the port.
- **The phase machine.** `step-v1` section 2 is this crate's, not the contract crate's; the
  trace's phase path is recorded beside the contract's `TransitionTrace`.

## Limitations

- **Fake workers.** There is no neural model and no emulator. What is modelled exactly is the
  ordering, the identity rules and the retry rules, not any numerical behaviour.
- **No state methods.** `State.Capture`, `State.StageRestore` and `State.ActivateRestore` are
  STATE-01. The phase machine has their edges (`Capturing`, `Restoring`) and the workers do not
  advertise them as implemented methods.
- **One process.** SESSION-01 runs every participant in one process over the same router.
  SESSION-02 is the per-fly process split.
- **No audience input.** The admitted pre-step stimulation list exists and is always empty.
- **Pacing is coarse.** The pacing deadline rounds one step to whole nanoseconds for sleeping
  only; simulation time stays rational and that rounding never re-enters the accumulator.

## Tests

```text
cargo test -p fly-session                                    # unit + both integration suites
cargo run -p fly-session --example session                   # the runnable synthetic session
```

Every integration test runs twice, once over the in-memory transport and once over a Unix
socket, through the same router code:

- `tests/session.rs`: one world advance per complete batch; every agent Prepared before the
  advance; one task evaluation per transition; every agent committed before the next Prepare or
  any committed publication; the 16/17/17 tick profile with a zero remainder; a mid-step pause
  completing its transition; bootstrap advancing nothing; the committed snapshot naming the
  transition that just ended; a terminal episode pausing at its own boundary; `Worker.Status`
  during a session; and sequential, concurrent and reversed dispatch producing one behaviour
  trace.
- `tests/failures.rs`: a duplicate Prepare after a lost reply; a duplicate Commit; the same
  batch with altered controls; a lost Advance result; a cached artifact consumed by its first
  caller; one Commit failing after another succeeded; a replaced registration; a reply from
  another incarnation; a world that advanced without sensory data; an exact duplicate of a
  running operation; and an old-epoch operation.
