# @flybrain/session-types

The session framework contracts in TypeScript: types, validation, canonical JSON (RFC 8785)
and canonical digests.

The other half of [`services/flysim/crates/fly-session-types`](../../services/flysim/crates/fly-session-types).
Same rules, same canonical bytes, same digests, and the same fixture corpus: this package
loads the crate's `fixtures/` directory rather than keeping a copy, so a case written once
holds both languages to it. Nothing here opens a socket; it reads, validates and hashes.

This is the internal session path (`docs/design/session-framework/`). The public feed and
control contracts are unchanged and still live in [`@flybrain/feed`](../feed).

## Modules

| Module | Contents |
| --- | --- |
| `canonical` | `canonicalize`, `digestOf`, `parseStrict`, `requireEnvelopeFit`, `rejectBusIdentities` |
| `scalar` | `Id`, `U64`, `Digest`, `Scope`, `RationalNs` with checked arithmetic, and the four identities as branded types |
| `reader` | `Reader`, which reads one object field by field and then refuses any field it did not read |
| `common` | `readScope`, `readSchemaRef`, `readTypedValue`, `operationKeyDigest`, `bodyDigest` |
| `media` | View and audio descriptors and refs, and the `State.*` payloads |
| `workers` | The closed enums and every Agent/Environment/Worker method payload |
| `rpc` | `SessionRpcRequest`, the success and failure replies, `ErrorCode`, `MutationCertainty` |
| `publishing` | `SessionDescriptor`, `CommittedSnapshot` |
| `trace` | The step-v1 section 8 record and the behaviour-only comparator |
| `seed` | `seed-derivation-v1` |
| `checkpoint` | The `FLYSESS1` envelope layout |
| `fixtures` | Loading the shared corpus |

## Reading a payload

Every reader takes `unknown`, validates, and hands back a value whose fields are exactly the
ones it read. A payload with an unknown or misspelled field fails instead of silently
defaulting, and a round trip through a reader is the test that no field is dropped.

```ts
import { canonicalize, digestOf, readScope, readPrepareParams, bodyDigest } from '@flybrain/session-types';

const scope = readScope(payload.scope);
const params = readPrepareParams(payload.params);
const digest = bodyDigest('Agent.Prepare', scope, params); // the ipc-v1 section 5 comparison
```

Rules that need another value in hand are separate functions, because a payload cannot check
them alone: `validatePortControlAgainst`, `validateBatch`, `validateSensoryInputAgainst`,
`validateObservationAgainst`, `validateStepResultAgainst`, `validateSnapshotAgainst`,
`validateTelemetryRoles`, `validateRemainder`, `validateCommitAgainstScope`.

## Canonical JSON

Three rules make the two implementations agree byte for byte:

- object keys sort by UTF-16 code unit, which is what comparing JavaScript strings does;
- numbers print with `String(number)`, the ECMAScript algorithm RFC 8785 requires;
- a number is canonicalizable when it is finite and, if integral, no larger in magnitude than
  `Number.MAX_SAFE_INTEGER`. Larger integers are refused rather than rounded: every counter
  and clock in these contracts is a `U64` decimal string. The rule is on the value, not on how
  it was written, because `JSON.parse` cannot tell `1e21` from the same digits written out.

`parseStrict` is a small recursive-descent parser rather than a wrapper around `JSON.parse`,
which keeps the last of two duplicate keys instead of failing.

Digests use `node:crypto`. This package is contract tooling for services and tests, not
browser code; the presentation layer consumes the public feed package instead.

## Tests

```sh
npm test --workspace @flybrain/session-types
npm run typecheck --workspace @flybrain/session-types
```

Nine files, all fixture-driven. The one that says the most about the two implementations is in
`tests/checkpoint.test.ts`: a `FLYSESS1` envelope written here is byte-identical to the one the
Rust crate wrote into the fixture.
