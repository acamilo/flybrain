# fly-session-types

The executable schemas of the session framework: domain scalars, closed enums, method
payloads, canonical JSON, canonical digests and the step trace format.

This crate is CONTRACT-01 of
[`docs/design/session-framework/implementation.md`](../../../../docs/design/session-framework/implementation.md).
It holds no transport, no worker, no coordinator and no store; it never opens a socket or a
file other than its own fixtures. The bus owns the wire
([`flybus`](../flybus)), and this crate owns what the messages mean.

## Layout

| Module | Contents |
| --- | --- |
| `scalar` | `Scope`, `RationalNs`, `SchemaRef`, `TypedValue`, the `DomainType` trait, and `BusCallId` / `DomainRequestId` / `ArtifactIdentity` / `OwnerToken` |
| `canonical` | RFC 8785 canonical JSON, SHA-256 digests, `OperationKey`, canonical bodies, the 64-KiB envelope check |
| `rpc` | `SessionRpcRequest`, `SessionRpcSuccess`, `SessionRpcFailure`, `ErrorCode`, `MutationCertainty` |
| `workers` | The closed enums and every Agent/Environment/Worker method payload of workers-v1 |
| `media` | `ViewDescriptor`, `ViewRef`, `AudioDescriptor`, `AudioRef` and the `State.*` payloads |
| `publishing` | `SessionDescriptor` and `CommittedSnapshot` |
| `trace` | `TraceBehaviour`, `TraceOperational`, `TransitionTrace` and the behaviour comparator |
| `schema` | The canonical schema set and `contract_digest()` |
| `seed` | `seed-derivation-v1` |
| `checkpoint` | The `FLYSESS1` envelope layout |
| `extensions` | The 2026-09-23 extension methods: `Environment.SaveSlot`/`RestoreSlot` (`gameboy-slots-v1`) and `Agent.Rollback` (`legacy-ratchet-rollback-v1`) |
| `gameboy` | The legacy Game Boy composition ([`legacy-gameboy-v1`](../../../../docs/design/session-framework/legacy-gameboy-v1.md)): registered payload schemas, the legacy profile, the composition declaration. Not part of `contractDigest` |
| `fixtures` | Loading `fixtures/`, shared with `packages/session-types` |

`Id`, `U64` and `Digest` are the bus encodings: `scalar` calls into `flybus::wire` instead of
restating them, and `tests/encodings.rs` pins that the two agree for every edge case.

## Reading and validating

Every type implements `DomainType`:

```rust
use fly_session_types::scalar::{DomainType, Scope};

let scope = Scope::from_json(&value)?;   // reads, refusing unknown fields, then validates
scope.validate()?;                        // the cross-field rules, re-runnable
let json = scope.to_json();               // the canonical shape
```

Rules that need another value in hand are separate, because a payload cannot check them alone:

```rust
control.validate_against(&port.controls)?;            // complete batch, descriptor order, ranges
input.validate_against(&descriptor.views)?;           // max(0, boundary - observationDelaySteps)
result.validate_against(&descriptor, &previous)?;     // exactly one stepDuration of world time
snapshot.validate_against(&session_descriptor)?;      // revision, agent set, assigned ports
telemetry.validate_against_roles(&profile_roles)?;    // rates in profile-defined order
```

## Digests

- `contract_digest()` is the SHA-256 of the canonical schema set (`schema::schema_set()`),
  which is a declaration: type names, JSON field names, kinds, bounds and closed enums.
  Reformatting this crate cannot change it; changing a field or a bound does.
- `canonical::body_digest(method, scope, params)` is the comparison ipc-v1 section 5 uses to
  tell a safe replay from a `CONFLICT`. It refuses a body that carries a bus identity.
- `OperationKey` is `(sessionId, epoch, step, method, workerId)`, and deliberately not the
  request id: a changed id for an existing key is the conflict to detect.

## Fixtures

`fixtures/` is loaded by these tests and by `packages/session-types`, so a case is written
once and holds both languages to it.

| File | Contents |
| --- | --- |
| `valid.json` | Payloads every implementation accepts, with their canonical JSON and digest |
| `invalid.json` | Payloads every implementation refuses, each with the rule it breaks |
| `raw.json` | Byte sequences refused before validation: duplicate keys, invalid UTF-8, `NaN`, trailing data |
| `generated.json` | Recipes for payloads too large to store: the 32-KiB and 64-KiB boundaries, 512-code-point messages |
| `boundaries.json` | The `U64` decimal-string and double boundaries |
| `rational.json` | Checked rational arithmetic and the 16, 17, 17 tick accumulator |
| `identities.json` | Which of the four identity types accepts which spelling |
| `descriptor-checks.json` | Rules that need a descriptor: batches, delays, byte shapes, descriptor agreement |
| `operations.json` | Operation keys, canonical bodies and the pairs that are or are not the same operation |
| `traces.json` | A baseline transition and the variants that must or must not compare equal |
| `schema-set.json`, `contract-digest.json` | The canonical schema set and its digest |
| `seed-vectors.json` | `seed-derivation-v1` test vectors |
| `checkpoint-envelope.json` | One `FLYSESS1` envelope, its layout and the corruptions a reader refuses |
| `gameboy-decoder-config.json` | The `decoderConfigDigest` vectors; written and checked by `flysim`'s `legacy_profile_identity` test (`FLY_UPDATE_FIXTURES=1` rewrites), reproduced by `@flybrain/session-types` from the oracle preset |
| `gameboy-legacy.json` | The legacy Game Boy extension set and digest, every registered `SchemaRef`, the legacy profile and its `AssetRef`, the frame clock, an example composition and its digest |

The derived files (`schema-set.json`, `contract-digest.json`, the `canonical`/`digest` fields
of `valid.json`, the digests in `operations.json`, `seed-vectors.json`,
`checkpoint-envelope.json` and `gameboy-legacy.json`) come from
`cargo run -p fly-session-types --example update_fixtures`;
`tests/schema_set.rs` fails if the checked-in files are stale.

## Tests

```sh
cargo test -p fly-session-types
cargo clippy -p fly-session-types --all-targets
```

## Bounds this crate chose

Every bound in the schema set names its source. Seven are marked `crate` because no document
states them: `maxAudioStreams` (8), `maxCapabilities` (32), `maxSupportedMajors` (8),
`maxSupportedStimuli` (64), `maxAssets` (64), `maxSnapshotEvents` (64) and `maxSlots` (4). They exist so an
unbounded array cannot fill an envelope, and they are in the digest, so widening one is a
contract change rather than a quiet edit.
