# Seed derivation v1

Status: **draft 1**, 2026-09-22. Specified by CONTRACT-01 of the
[implementation guide](implementation.md), required by
[worker interfaces](workers-v1.md) section 2 before the real-agent slice. Reference
implementations: `services/flysim/crates/fly-session-types/src/seed.rs` and
`packages/session-types/src/seed.ts`; test vectors:
`services/flysim/crates/fly-session-types/fixtures/seed-vectors.json`.

## 1. What this is for

`Agent.Initialize` takes `seed`, a signed 32-bit integer, matching the current RNG input.
Workers-v1 section 2 requires that the coordinator derive independent per-agent seeds from
**its recorded master seed and stable agent IDs** under a versioned algorithm, and that the
algorithm be specified and tested before the real agent slice. This is that algorithm.

It is a reproducibility rule, not a secret: a run manifest records the master seed in the
clear, and anyone with the manifest can recompute every agent's seed. It is not a key
derivation function and must not be used as one.

`seed-derivation-v1` is part of composition identity. Changing any byte of it requires a new
identifier (`seed-derivation-v2`), because two runs that agree on every other identity but
disagree here are not the same experiment.

## 2. Inputs

| Input | Type | Source |
| --- | --- | --- |
| `masterSeed` | `U64` decimal string | Recorded once per run by the application/supervisor |
| `agentId` | `Id` | The configured agent identity, stable across restarts and epochs |

Both are the ipc-v1 section 2 scalars. An `agentId` that is not an `Id` is an error, not
something to normalize. The master seed is the whole 64-bit range: a 32-bit master seed would
be no wider than the seed it derives.

## 3. Derivation

```text
material = "flybrain/seed-derivation-v1" LF masterSeed LF agentId LF
digest   = SHA-256(material)
lanes    = digest read as eight big-endian uint32 values, in order
seed     = the first nonzero lane, reinterpreted as a two's-complement int32
```

`LF` is one `0x0a` byte. `masterSeed` is its canonical decimal form: `"0"`, or no leading
zero. The prefix is a domain separator, so a digest from this algorithm can never collide with
one taken over some other pair of strings.

Zero lanes are skipped because the pinned kernel's RNG is an xorshift generator, whose state
must not be zero: a derivation that could hand out `0` would silently produce a stalled
generator. If every one of the eight lanes were zero, the material is rehashed with a counter
suffix (`material || "1" LF`, then `"2" LF`, then `"3" LF`) and the search repeats; no input
has ever needed it, and four rounds exhausted is an error rather than a fallback seed.

The seed is the *negative* number when the lane's high bit is set. That is deliberate: the
existing RNG input is a signed 32-bit integer, and half the range is negative.

## 4. Properties

- **Deterministic.** The seed is a function of the two recorded inputs and nothing else: not
  of wall time, agent order, port assignment, worker process or thread count.
- **Independent per agent.** Distinct agent IDs give unrelated seeds; there is no arithmetic
  relationship between `fly-a` and `fly-b` for a caller to exploit or accidentally rely on.
- **Stable across recovery.** Restore, episode reset and a new epoch do not re-derive a
  different seed for the same agent ID under the same master seed. The seed is persisted as run
  configuration and state, and the capture compatibility digest covers the resolved seed
  (workers-v1 section 2), so a checkpoint cannot be installed into a differently seeded
  instance.
- **Equal IDs give equal seeds.** That is the only way to get identical seeds, and workers-v1
  allows identical seeds only when an experiment declares them. A composition therefore
  refuses a repeated agent ID rather than quietly sharing a seed between two agents.

Non-properties, stated so nobody assumes them: this is not uniform over the int32 range beyond
what SHA-256 gives, it is not a stream (one seed per agent per run, not per step), and it says
nothing about how a model consumes its seed.

## 5. Test vectors

`fixtures/seed-vectors.json` carries the full table: five master seeds (`0`, `1`, `42`, `2^63`
and the `U64` maximum) across four agent IDs, each with the exact material string, its SHA-256
and the derived seed, plus one four-agent composition and the inputs that must be refused.
Both implementations reproduce every row, and each records the material as well as the seed so
a third implementation can find where it diverges.

The first two rows:

| masterSeed | agentId | material | seed |
| --- | --- | --- | ---: |
| `0` | `fly-a` | `flybrain/seed-derivation-v1\n0\nfly-a\n` | 1828176714 |
| `0` | `fly-b` | `flybrain/seed-derivation-v1\n0\nfly-b\n` | 1218785088 |

Refused: an agent ID that is not an `Id` (uppercase, empty, over 64 characters), a master seed
that is not a canonical `U64`, and a composition with a repeated agent ID.

## 6. Out of scope

Choosing the master seed, recording it in the run manifest, and the hand-selected explicit
seeds that workers-v1 allows for the first synthetic composition. This document defines only
the derivation. A profile that needs several independent streams inside one agent derives them
from the agent's own seed under its own documented rule; that is a profile concern, not a
session one.
