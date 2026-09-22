# flybus: the communications bus

Status: **crate landed, nothing wired onto it**. Written 2026-09-22. Index only; the
authority for the API and the wire format is the crate's own
[README](../../services/flysim/crates/flybus/README.md), and the audit of the crate against
the draft is the [conformance report](session-framework/bus-conformance.md).

## What it is

`flybus` is a local RPC and pub/sub bus for Tokio processes on one host, with immutable
file-backed artifacts for large payloads. One library, one router, one wire protocol:
messages are small strict-JSON envelopes carrying metadata and artifact references, while
bulk bytes (frames, audio, spike bitsets) live in a store directory the router owns.
Ownership follows deliveries and explicit holds; the router moves messages and tracks
ownership and does not interpret them. It implements the Flybus v1 draft (`bus-v1`, draft 1
of 2026-09-18) over the `ipc-v1` scalar encodings.

Transports are an in-memory pair, an accepted `AsyncRead + AsyncWrite` stream, or a Unix
socket. Identity is bound by the launcher before Hello and checked against a `Policy` of
per-client grants; open/unbound mode is for trusted tests only and is not authentication.

Nothing in the crate is specific to a game, a brain or a stream.

## What it is meant to replace

Today the three processes talk over two ad-hoc loopback surfaces:

| Today | Under the bus |
| --- | --- |
| Feed: WebSocket `127.0.0.1:7400/feed`, one binary message per snapshot at 30 Hz, header plus RGBA frame, audio and spike attachments, re-serialized per consumer | One `latest`-mode topic per stream, with the frame as a sealed artifact shared by fan-out instead of copied per subscriber |
| Control: loopback HTTP `127.0.0.1:7401` (`/stimulate`, `/chat`, `/checkpoint`, `/pause`, `/status`, ...) | RPC services with per-client grants, FIFO dispatch, explicit cancel states and backpressure |
| Per-surface limits, rate limits and timeouts written twice | `Limits` and `Policy` in one place, negotiated at Hello |

The feed and control contracts in [feed-protocol.md](../feed-protocol.md) and
[control-api.md](../control-api.md) stay binding until a migration replaces them. The bus
does not change any published contract by existing.

## Crate layout

`services/flysim/crates/flybus`, a workspace member of the flysim workspace; no other crate
depends on it yet.

| Module | Contents |
| --- | --- |
| `wire` | Scalars, strict JSON, `Envelope`, `ArtifactRef`, `Attachment`, `Location`, framing, `CONTRACT` and `contract_digest()` |
| `error` | `ErrorCode`, `Dispatch`, `BusError` |
| `limits` | `Limits` and its hello encoding |
| `policy` | `Policy`, `Grants`, `Pattern` |
| `router` | `Router`, `RouterConfig`, `RouterStats`, `UnixListenerHandle`; the state machine in `router/state.rs` |
| `client` | `Client` and the handle types |
| `store` | The router-side file store and client-side location resolution |
| `transport` | `Transport` and the `Stream` trait |

Dependencies are already in the workspace lockfile: `tokio`, `serde`, `serde_json`, `sha2`,
`libc`. Tests run every integration case twice, once in memory and once over a Unix socket:
wire negotiation, RPC authority and cancellation, pub/sub credits and retention, artifact
allocate/seal/read with quotas and router restarts, plus the conformance suites and an
`--ignored` perf measurement.

## Wiring still pending

- **flysim publisher.** Router startup inside the sim service, a store root under its
  runtime directory, and snapshot publication as artifact plus header envelope.
- **flysim control services.** The control endpoints as RPC services with grants, so the
  "no button endpoint" structural guarantee is expressed as a grant table.
- **Stage and bridge clients.** Both are TypeScript/Node; the crate is Rust only, so either
  a binding or a thin translating edge process is required before they leave the WebSocket
  and HTTP surfaces.
- **Sizing.** `max_store_bytes`, `max_retained_bytes` and `max_latest_in_flight` need values
  chosen for 1.2 MB frames at 30 to 60 Hz with a slow consumer, not the defaults.
- **Lifecycle.** Orphaned store directories are cleaned only when a new router starts on the
  same root, so service restart order and the store root's location need a decision.
- **Migration order.** The feed is the cheaper first move; control should follow only once
  the bus carries the feed in production for a full session.
