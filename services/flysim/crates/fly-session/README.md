# fly-session

The lockstep session coordinator, its phase machine and a synthetic composition over
[`flybus`](../flybus).

This crate is the SESSION-01 and SESSION-02 slices of the session-framework implementation
guide: the transaction of `step-v1`, driven over the Flybus router, with small fake workers
standing in for a brain and an emulator, run either in the coordinator's process, on dedicated
threads, or as one agent process per fly and one environment process under a launcher. It
contains no public controller API, no implicit best-effort retry, no real emulator and no real
brain.

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
| `launcher` | The supervisor: thread budget, identities, start, health check, reap |
| `metrics` | Latency percentiles and the machine's core and memory counters |
| `measure` | The execution-mode comparison of the guide's section 5 |
| `cli` | The binary's subcommands: `agent`, `environment`, `measure` |
| `harness` | The runnable composition: router, the flies, one arena, one coordinator |

## Execution modes and the launcher

A participant runs in one of three places, and the same composition code starts it in any of
them. The separate-process mode is the SESSION-02 subject; the other two are what it is
compared against.

| Mode | Where each participant runs | Transport |
| --- | --- | --- |
| `InProcess` | A task on the coordinator's runtime | in-memory or Unix socket |
| `Thread` | Its own OS thread, with its own runtime | Unix socket |
| `Process` | Its own process: one per fly, one for the world | Unix socket |

The launcher is the configured supervisor. It owns four things:

- **The thread budget.** A total allocation, one slice of it reserved for the coordinator and
  its router, and one allocation per participant. A request the total cannot cover is refused
  as `BUSY` before anything starts. `Agent.Initialize` carries exactly the allocation the
  launcher handed out, and an agent refuses an Initialize asking for more than its own, which
  is what `workers-v1` means by "within launcher allocation". The allocation is on the wire,
  not only in the launcher's own record: `HelloResult.limits.workerThreads` reports it, under
  the dated 2026-09-22 amendment to `workers-v1` section 2 that this slice added, so a
  coordinator that is not also its own launcher can read the bound it has to respect.
- **Identity.** The bus client id, the service name, the worker id and an agent's port binding
  are launcher configuration. The launcher says `Worker.Hello` with the identity it configured
  and refuses anything that answers as another worker, role, incarnation or thread allocation
  -- before the coordinator has pinned a registration. The registration the coordinator pins
  is the one that hello returned, never one that was assumed.
- **Health.** `Worker.Status` on the supervisor's own monotonic clock, with the `ipc-v1`
  section 6 prototype budgets: probe at two seconds, fail at ten, a separate budget for boot.
  A status answer never waits for a mutation, so a busy participant is still a healthy one.
- **Reaping.** `Worker.Shutdown` is the request and the operating system is the guarantee. A
  participant that does not stop inside the budget is terminated, and the supervisor reports
  which of the two happened. A launcher that is dropped takes its children with it.

A separate-process participant is a subcommand of this crate's one binary, which is what
`implementation.md` section 2 allows instead of separate worker crates:

```sh
fly-session agent       --socket S --store-root D --client-id C --service N --threads T ...
fly-session environment --socket S --store-root D --client-id C --service N --threads T ...
fly-session measure     --steps 300 --agents 1,2,4
```

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
- **A failure stops the epoch rather than neutralising a player.** Every failure carries the
  participant it is attributed to, and failing fences the session: the committed boundary
  stops moving, the artifact handles are dropped, and no further transition or publication is
  allowed. Lifting the fence is a coherent group restore, which is STATE-01's.
- **The `ipc-v1` section 6 procedure, on the path that reaches it.** A call that goes without
  a terminal reply for the probe budget is *uncertain*, not failed. The coordinator then
  queries the same operation -- a fresh bus call carrying the original domain request id and
  body, pinned to the same incarnation, with its retained attachments -- for a bounded number
  of attempts within a bounded budget, absorbing `IN_PROGRESS` while the original is still
  running. Only when that ends without a definite answer, or the incarnation is gone, or the
  retained result expired, is the epoch failed. A merely slow participant therefore finishes
  its step, and `step-v1` section 7's "query/retransmit same request to same incarnation;
  never new batch" is the same code path for a slow Advance.
- **A bounded diagnosed outcome.** Those budgets are the coordinator's own, on its own clock,
  so a participant that dies or stops answering produces a typed failure naming it rather than
  a hang. An expired deadline is `unknown`, never `none`: a caller-side timeout is not
  evidence that nothing was mutated.
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
  trace's phase path is recorded beside the contract's `TransitionTrace`. The mid-step pause
  it takes -- the transition finishes, then the session pauses at the boundary it just
  committed -- is now written into the section 2 machine as a dated amendment.

## Limitations

- **Fake workers.** There is no neural model and no emulator. What is modelled exactly is the
  ordering, the identity rules and the retry rules, not any numerical behaviour.
- **No state methods.** `State.Capture`, `State.StageRestore` and `State.ActivateRestore` are
  STATE-01. The phase machine has their edges (`Capturing`, `Restoring`) and the workers do not
  advertise them as implemented methods.
- **No audience input.** The admitted pre-step stimulation list exists and is always empty.
- **Pacing is coarse.** The pacing deadline rounds one step to whole nanoseconds for sleeping
  only; simulation time stays rational and that rounding never re-enters the accumulator.

## Measurements

`fly-session measure` runs the same composition in each mode at one, two and four agents and
reports the thread allocation, the RPC and critical-path percentiles, the memory peaks and the
router's owner, collection and queue counters. **These are local synthetic timings on one
machine and no host capacity claim follows from any of them**; they exist so the three modes
can be compared with each other. Pacing is off for the run, so the samples are work rather
than sleep, and the run report carries the full table.

Every row runs in a child process of its own. A peak-memory figure is a high-water mark that
never falls, so rows sharing one process would each report where that process had already
been: the column would sort itself by row position rather than by mode, and the mode ranking
would reverse when the rows were reordered. One child per row is what makes the number belong
to the row.

What the numbers said on a four-core development box, at 300 transitions per row:

- A process boundary costs about a fifth of the critical path at the median. Two agents: 10.0
  ms p50 in-process, 12.2 ms on threads, 12.1 ms across processes, with p99 at 21.4 / 19.4 /
  21.4 ms. The dedicated-thread and separate-process variants are within noise of each other,
  so what is being paid for is leaving the coordinator's runtime, not crossing a socket.
- `Worker.Status` -- an RPC answered from a cell with no domain work behind it -- is the
  router and transport floor: 0.60 / 0.89 / 1.17 ms p50 for the three modes at two agents.
- Four agents needs six threads, which that box does not have, and every mode's tail widens
  together. That is the budget being honest about oversubscription, not a property of the
  process split.
- Memory is where the split really shows, but not where the first version of this note said.
  The coordinator's own peak is roughly the same in all three modes and is *lowest* in process
  mode -- 8.9 / 9.2 / 7.9 MiB at one agent -- because the workers are no longer inside it.
  What the split costs is the children: about 5.7 MiB per participant process, so the whole
  composition is roughly 9 MiB on threads against 39 MiB across processes at four agents.
- Ownership, collection and queues stayed bounded in every mode and at every agent count: at
  most 15 live owners, 11 artifact roots and one queued entry per agent, with the store at
  rest holding two sealed frames and 128 bytes. Of 311 frames observed, 309 were collected --
  the two still owned are the current and previous boundary. The frame count is taken from the
  behaviour trace's observation boundaries rather than calculated from the step count, so a
  backend sealing two frames per boundary would show up instead of being hidden.

## Tests

```text
cargo test -p fly-session                                    # unit + all three integration suites
cargo run -p fly-session --example session                   # the runnable synthetic session
cargo build -p fly-session --bin fly-session                 # the worker binary the launcher starts
cargo run -p fly-session --example processes                 # the same session in all three modes
```

The three integration suites do not all run over both transports, and cannot:

- `tests/session.rs` and `tests/failures.rs` are in-process compositions and run over both,
  through the same router code. All but one test in them is generated twice by
  `both_transports!`; `sequential_concurrent_and_reversed_orders_agree` walks both transports
  inside one test, because it compares their behaviour traces against each other.
- `tests/processes.rs` runs over the Unix socket only, in all three execution modes. A
  participant in a process of its own has no in-memory transport to reach the router by, so
  the mode is the axis that suite varies and the transport is fixed.

- `tests/session.rs`: one world advance per complete batch; every agent Prepared before the
  advance; one task evaluation per transition; every agent committed before the next Prepare or
  any committed publication; the 16/17/17 tick profile with a zero remainder; a mid-step pause
  completing its transition; bootstrap advancing nothing; the committed snapshot naming the
  transition that just ended; a terminal episode pausing at its own boundary; `Worker.Status`
  during a session; and sequential, concurrent and reversed dispatch producing one behaviour
  trace.
- `tests/processes.rs`: the SESSION-02 acceptance bullets, each generated once per execution
  mode -- a slow participant resolved rather than failed, a delayed one-agent result holding
  the world, a worker or helper death with a
  bounded diagnosed outcome, an uncertain Advance that creates no second batch, a partial
  Commit that permits no next-step play, supervision and identity, and the launcher thread
  allocation -- plus the sequential/reversed/parallel trace comparison across all three modes
  and the two process-mode section 4 rows: a router restart during a world advance, and an old
  worker's reply after a restart.
- `tests/failures.rs`: a duplicate Prepare after a lost reply; a duplicate Commit; the same
  batch with altered controls; a lost Advance result; a cached artifact consumed by its first
  caller; one Commit failing after another succeeded; a replaced registration; a reply from
  another incarnation; a world that advanced without sensory data; an exact duplicate of a
  running operation; and an old-epoch operation.
