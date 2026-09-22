# Application and session framework: architecture and contracts

Status: **implementation specification, draft 2**, 2026-09-18. This is a guide for future
agents; none of the new runtime is implemented yet. Baseline code is `83090a9` on
`docs/improvement-suggestions`. MUST/SHOULD requirements apply to the proposed new path, not
retroactively to existing [public feed](../../feed-protocol.md),
[control API](../../control-api.md), or legacy numerical/checkpoint behavior.

## Decisions from the architecture discussion

1. **Applications orchestrate components and develop their presentation alongside them.**
   “Director” is application code, not a mandatory framework service. A tournament is an
   example application, not the system's organizing data model.
2. **One lightweight Rust bus supports RPC and pub/sub everywhere internally.** Flybus replaces
   separate direct worker transports and an application broker. No NATS dependency.
3. **Messages stay small; large artifacts live in managed storage.** Delivery guards and
   explicit cache/retention owners keep data alive until its last actual use, then GC reclaims it.
4. **The router moves messages and tracks generic ownership.** It never schedules game frames,
   understands macro actions, composites video or operates a stream.
5. **Sessions synchronize worlds; agent workers compute in parallel.** One logical clock is
   not one execution thread. The coordinator alone commits complete world-control batches.
6. **Game-aware executors receive current game state and task progress.** Rich inspection data
   does not become undeclared neural input.
7. **Native observations are framework outputs.** Resizing, overlays, browser delivery, audio
   mixing, encoding, narration and streaming belong to the application/presentation layer.

## Read in this order

1. [Flybus v1](bus-v1.md) — authoritative wire/routing/RPC/pub-sub/artifact lifecycle contract.
2. [Session RPCs](ipc-v1.md) — domain payloads, worker capability negotiation and safe retries.
3. [Step protocol](step-v1.md) — session state machine, ordering and clocks.
4. [Worker/task interfaces](workers-v1.md) — exact method bodies and game-aware executor boundary.
5. [Session media/state](state-media-v1.md) — observation timing and coherent recovery.
6. [Application/presentation boundary](publishing-v1.md) — snapshots, flexible data and effects.
7. [Implementation guide](implementation.md) — sequenced build tasks and acceptance tests.
8. [Flybus conformance report](bus-conformance.md) — the `flybus` crate audited sentence by
   sentence against bus-v1, with the test that proves each row, the measurements and the
   draft's own contradictions. A review artifact, not a contract.

Two derived specifications, written by CONTRACT-01 because the slices that need them cannot
be built without them:

- [Seed derivation v1](seed-derivation-v1.md) — independent per-agent seeds from one recorded
  master seed and stable agent ids, with test vectors in both languages.
- [Checkpoint envelope v1](checkpoint-envelope-v1.md) — the exact bytes of the new `FLYSESS1`
  envelope and the durable commit sequence. `FLYSIM01` is unchanged and stays separately
  readable.

For context: [modular-session analysis](../malecns-modular-sessions.md) and
[Melee audit](../melee-framework-audit.md). Each contract owns its named subject; step ordering
wins over an informal diagram, and Flybus owns transport/resource rules. Resolve contradictions
before implementation. The [existing HTML report](report.html) is an overview, not a contract.

## 1. Composition and processes

```text
Application / supervisor                 Application presentation
  run policies, identity, history           UI, media composition, audience, stream
                \                           /
                   Flybus: RPC + pub/sub
                /          |              \
        Session        Agent workers      Environment worker
      coordinator      brain + encoder    emulator/world + native observations
      task/executors    + fixed readout
                           |
                  artifact store (same bus API)
```

This diagram is connectivity, not execution order. The step contract defines causal order.
One router can host several independent sessions and application consumers; a deployment may
choose one router per application for fault isolation. A router crash affects all its clients,
so choose that boundary deliberately. In-process mode still exercises routing and ownership.

Defaults: coordinator per session, worker per fly, environment worker per world. Task and
per-agent executors begin as coordinator-local libraries. An environment helper may own a
separate emulator child process and adapt its native protocol. There is no second framework
socket/lease API between those logical components.

Multiple players in one game share one environment/barrier. Independent games use independent
sessions. Linked emulators require a composite backend with link-appropriate timing.

## 2. Ownership and authority

| Owner | State and responsibility |
| --- | --- |
| Application | Composition, persistent personas/brain lineage, lifecycle policies, supported interventions, application schema/history |
| Session | Clock/epoch, port assignments, admission, task ledger, executor state, barriers and coherent recovery |
| Agent | Private membrane, RNG, rates, learning, sensory encoding, decoder and tick remainder |
| Environment | World, actual controller application, backend parser, native media and state capabilities |
| Flybus | Opaque endpoint/topic routing, delivery/call correlation, bounded queues, artifact-owner graph and GC |
| Storage client | Durable event/checkpoint writes and replay APIs; owns artifact handles during writes |
| Presentation | Application UI, display focus, clocks/buffers, media processing, audio/stream output and narrative cues |

Immutable graph data can be shared; neural mutable state cannot. Do not concurrently dispatch
the existing single-job WorkerPool through cloned handles from different brains.

Applications use declared session capabilities, not arbitrary emulator writes. Bus registration
and method privileges preserve a single controller authority for a session. Browser/audience
clients do not gain controller access by knowing a service name. Existing public rules remain.

## 3. Domain independence

The kernel knows no task or bus. The environment knows no neuron populations. A task interprets
game state and requests outcomes/recovery; its executor translates a selected decision using
read-only current game/progress context. The coordinator orders and applies these results.
The bus handles no such semantics. Presentation combines framework observations with an
application-owned schema and can change without changing simulation behavior.

Persistent AssetRefs identify installed release content. Transient Flybus ArtifactRefs identify
live bytes with ownership. Domain epoch/step identity and bus route/store incarnation are
different: the first protects simulation order, the second protects delivery/resource validity.
Never substitute game-frame number, persona identity or array position for either.

## 4. Initial scope and compatibility

First build a generic bus example (RPC + pub/sub + artifact retained beyond message lifetime),
then a synthetic two-agent session using it. One local machine, Unix sockets/in-memory parity,
immutable file-backed artifacts, fixed cadence, 1-ms LIF, direct control and exact-checkpoint
synthetic backend are sufficient. Dolphin, MaleCNS and richer effects are later integrations.

Deferred: cross-machine artifact access, durable broker queues, wildcard/queue-group routing,
dynamic native plugins, hot-join, speculative netplay rollback and pooled GPU buffers.

Keep legacy-gameboy-v1 distinct from lockstep-v1. Preserve TypeScript as oracle, existing default
versions, historical arithmetic/fingerprints and FLYSIM01 reader. New identities include sensor,
readout/executor/task/scheduler semantics. New public feed v2 is an application/presentation
gateway contract built on the same internal bus; it does not replace the bus or expose it raw.

## 5. Reuse criterion

Adding a third environment/application requires a backend, task/profile, composition and
application presentation. It must not require game-specific edits to the coordinator, router,
artifact manager, kernel, generic stores or transport. Schematized task extensions are valid;
an unchecked data blob or universal tournament schema is not a substitute for interfaces.
