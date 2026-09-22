# Checkpoint envelope v1: `FLYSESS1`

Status: **draft 1**, 2026-09-22. Specified by CONTRACT-01 of the
[implementation guide](implementation.md); required by
[session artifacts, native media and recovery](state-media-v1.md) section 4, which says to
"use a new envelope version; specify exact byte layout before production files". Reference
implementations of the layout: `services/flysim/crates/fly-session-types/src/checkpoint.rs`
and `packages/session-types/src/checkpoint.ts`; fixture:
`.../fly-session-types/fixtures/checkpoint-envelope.json`.

This is the byte layout and the durable commit sequence. The store itself, generations,
rotation, the writer thread and the capture RPC flow are the STATE-01 slice.

## 1. Why a new format

The historical envelope (`FLYSIM01`, `crates/flybrain-core/src/envelope.rs`) is a magic, a
`u32` manifest length, a JSON manifest, `u32`-prefixed chunks in manifest order and a CRC32
footer, with chunk names restricted to ASCII letters so the TypeScript reader can never name a
prototype key. It stays exactly as it is, and its reader stays separately readable: nothing in
this document changes a byte of it, and a `FLYSIM01` file is refused by a `FLYSESS1` reader at
the magic.

A coherent all-participant session checkpoint needs what that format does not have:

- payload names that are `Id`s (`agent-fly-a`, `executor-fly-a`), so the letters-only
  constraint is widened **deliberately, in a new version**, rather than quietly;
- a per-payload content digest, because state-media-v1 section 1 makes digests mandatory on
  checkpoint payloads and a group install must be able to fail one participant's bytes;
- a payload table with explicit offsets and lengths, so a reader can map one participant's
  payload without walking every preceding chunk;
- SHA-256 over the whole prefix instead of CRC32, matching the `Digest` type these contracts
  already use everywhere else.

## 2. Byte layout

All integers are unsigned little-endian. All digests are raw 32-byte SHA-256 (the manifest
records the same digests as lowercase hex `Digest` strings).

### Header, 32 bytes

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | Magic, ASCII `FLYSESS1` |
| 8 | 4 | `envelopeVersion`, `1` |
| 12 | 4 | `headerBytes`, `32` |
| 16 | 4 | `manifestBytes` |
| 20 | 4 | `payloadCount`, at most 64 |
| 24 | 4 | `tableOffset` |
| 28 | 4 | Reserved, must be zero |

### Manifest

`manifestBytes` bytes of canonical JSON (RFC 8785) at offset 32, no trailing newline. It is
canonical so the envelope's own digest is stable under reserialization, and a reader rejects a
manifest that is not already canonical rather than silently accepting a second spelling.

### Payload table

At `tableOffset`, which is `32 + manifestBytes` rounded up to a multiple of 8.
`payloadCount` entries of 112 bytes each, in write order:

| Offset in entry | Size | Field |
| ---: | ---: | --- |
| 0 | 64 | Name: an `Id` in ASCII, NUL-padded, no bytes after the terminator |
| 64 | 8 | `offset` |
| 72 | 8 | `byteLength` |
| 80 | 32 | SHA-256 of exactly `byteLength` bytes at `offset` |

### Payloads

Each payload starts at its declared offset. The first starts at the end of the table rounded
up to a multiple of 8; each subsequent one starts at the previous payload's end rounded up the
same way. Padding bytes are zero. Offsets are ascending and non-overlapping, which a reader
checks rather than assumes.

### Footer, 48 bytes

| Offset from end | Size | Field |
| ---: | ---: | --- |
| 48 | 8 | `fileBytes`, the total length including the footer |
| 40 | 32 | SHA-256 of every byte before the footer |
| 8 | 8 | Magic, ASCII `FLYSESSF` |

A truncated file therefore fails at the footer magic or the recorded length, not at an
arbitrary payload.

## 3. Manifest fields

State-media-v1 section 4 lists what the manifest records. The names below are the JSON field
names; a manifest missing any of them is not a complete checkpoint.

| Field | Contents |
| --- | --- |
| `envelopeVersion` | `1` |
| `checkpointId` | `Id`, the identity every participant's capture shares |
| `sourceScope` | `Scope`: session, epoch and the committed step |
| `episodeId` | `Id` |
| `worldTime` | `RationalNs`, the environment's logical time at that boundary |
| `schedulerId` | The coordinator's scheduler identity, `lockstep-v1` in v1 |
| `compositionDigest` | Coordinator scheduler and configuration identity |
| `portMap` | The exact port-to-agent map, `[{portId, agentId}]` |
| `compatibility` | Backend, content, patch, controller, parser and state-format identities |
| `agents` | Per agent: profile, dataset and model identities, resolved seed, tick count, remainder and the payload name holding its state |
| `coordinator` | Task ledger, prior world inspection, per-agent executor state, admission state and event watermarks, each as a payload name or an inline value |
| `helperState` | External-helper state required for exact resume, as payload names |
| `payloads` | `[{name, byteLength, digest}]`, mirroring the payload table |

**Amendment, 2026-09-22 (STATE-01).** The table above names a holder for every payload except
the environment's own, although section 6's fixture has one (`world`) and a group install has
to map it by name like any other participant's. The manifest therefore also records:

| Field | Contents |
| --- | --- |
| `environment` | `{workerId, payload}`: which worker the world belonged to and the payload name holding its state |

The reference implementations' required-field set was also missing `helperState`, which this
section has listed from the start. Both are now in `REQUIRED_MANIFEST_FIELDS` in Rust and in
TypeScript, and the fixture was regenerated by the existing example. The schema set is
untouched, so `contractDigest` is unchanged.

`payloads` is redundant with the table on purpose: the table is what a reader needs to map
bytes, and the manifest is what a store lists, compares and reports without opening the
payload area. A reader checks that the two agree.

What the manifest must **not** contain (state-media-v1 section 4): a transient bus `storeId`,
artifact ID, owner token, mapping or pointer. Payload bytes and durable content identity are
the only things that survive; on restore the durable store imports fresh bus artifacts, and
`sourceScope` is provenance, not a claim on the current router.

## 4. What a reader enforces

In this order, so a corrupt file fails on its own terms rather than on a derived value:

1. Length at least header plus footer; magic; version; `headerBytes`; reserved word zero.
2. Footer magic, `fileBytes` equal to the actual length, and the prefix digest.
3. Manifest inside the payload area, valid strict JSON (duplicate keys, invalid UTF-8 and
   non-finite numbers refused) and already canonical.
4. `tableOffset` exactly at the laid-out position; the table inside the payload area.
5. Per entry: an `Id` name with no bytes after its terminator, names unique, the declared
   offset exactly at the aligned end of the previous payload, the payload inside the payload
   area, and its digest matching its bytes.
6. No padding between the last payload and the footer.
7. The required manifest field set, `envelopeVersion` of 1, and a `payloads` list that matches
   the table name for name, length for length and digest for digest.

Failing any of these is a corrupt or foreign file. The group install rule of state-media-v1
section 5 then applies: corrupt any participant and installation fails as a group.

## 5. Durable commit

State-media-v1 section 6, in the order the writer performs it:

1. Write the envelope to a temporary generation file in the store directory.
2. `fsync` the file.
3. `rename` it to its final generation name.
4. `fsync` the store directory.
5. Write the store manifest to its own temporary file, `fsync`, `rename`, `fsync` the
   directory.

**The store manifest rename is the durable commit point.** Before it, the generation file is
an unreferenced temporary that is never a restore candidate. After it, and only after it, the
writer reports a saved acknowledgment and moves the high-water mark.

Consequences the writer must respect rather than reinterpret:

- Bus publications for `captured`, `queued`, `committed`, `failed` and `superseded` are
  distinct events; only durable completion produces the saved acknowledgment.
- A lost save reply never advances durable metadata: the coordinator resolves the same
  operation or fails the epoch, and an unreferenced generation stays unreferenced.
- A failed write releases its owned ephemeral captures under the configured retry policy and
  reports the failure. It never reports false durability.
- The writer owns the bus artifact handles until the bytes are committed or the job fails, and
  drops them afterwards; durable files are outside the bus's ephemeral collection.
- No per-payload `fsync` inside one envelope: the single file `fsync` in step 2 covers it.

## 6. Fixture

`fixtures/checkpoint-envelope.json` holds one complete envelope: the manifest, five payloads
(one agent, one executor, the task ledger, the prior inspection and a world payload), the
envelope's base64 bytes, its exact layout (header size, manifest offset and length, table
offset, every entry's offset, length and digest, footer offset, total length) and six
corruptions a reader must refuse, each naming the byte to flip.

The two implementations are held to it from both directions: each parses the fixture and
checks every recorded offset, and the TypeScript side re-encodes the same manifest and
payloads and requires the bytes to be identical to the fixture. A layout change that only one
language makes therefore fails on the next test run.

## 7. Out of scope

Generations, rotation, hot versus durable copies, the capture queue and its bounds, the
`State.Capture` / `State.StageRestore` / `State.ActivateRestore` flow, compatibility
comparison rules and group fencing. Those are STATE-01, over this layout. `FLYSIM01` and the
legacy composition keep their own format and their own reader, unchanged.
