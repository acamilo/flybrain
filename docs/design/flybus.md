# flybus: the communications bus

Status: **crate landed; the feed rides it behind `FLY_FEED_VIA=bus`, off by default**.
Written 2026-09-22, amended 2026-09-23 (EDGE-01, below). Index only; the
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

`services/flysim/crates/flybus`, a workspace member of the flysim workspace. `flysim` depends
on it for the feed publisher (`src/feedbus.rs`) and `fly-edge` for the subscriber.

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

- ~~**flysim publisher.**~~ Done 2026-09-23 behind `FLY_FEED_VIA=bus`: see "Feed over the bus".
- **flysim control services.** The control endpoints as RPC services with grants, so the
  "no button endpoint" structural guarantee is expressed as a grant table.
- **Stage and bridge clients.** Both are TypeScript/Node; the crate is Rust only, so either
  a binding or a thin translating edge process is required before they leave the WebSocket
  and HTTP surfaces. The operator chose the edge process (port decisions, 2026-09-23); for
  the feed it exists (`fly-edge`), and they keep the WebSocket contract unchanged. The
  control API (:7401) is the next slice and stays in flysim until then.
- ~~**Sizing.**~~ Decided 2026-09-23: amendment "Feed sizing" below.
- ~~**Lifecycle.**~~ Decided 2026-09-23: amendment "Feed store lifecycle" below.
- **Migration order.** The feed is the cheaper first move; control should follow only once
  the bus carries the feed in production for a full session.

## Feed over the bus (2026-09-23, EDGE-01)

`feed.via` (`FLY_FEED_VIA`) picks who serves `ws://127.0.0.1:7400/feed`. `direct` is the
default and is the behaviour that predates the bus. With `bus`:

```text
 sim thread --watch<Snapshot>--> publisher task --flybus (in memory)--> Router
   (unchanged)                   (flysim-bus runtime)                     |  <bus_dir>/edge.sock
                                                                          v  (bound to "fly-edge")
                                              fly-edge: Subscription -> watch -> flysim::feed :7400
```

- flysim does not bind `feed.bind`. It starts a `Router` on a runtime of its own (two
  threads, `flysim-bus`), store root `<bus_dir>/store`, closed policy: `flysim` may declare
  and publish `fly.feed.snapshots`, `fly-edge` may only subscribe to it, and the Unix socket
  `<bus_dir>/edge.sock` is launcher-bound to `fly-edge`.
- The topic is `retained: latest`. Each publication is one snapshot: the attachments the
  header lists as sealed artifacts named `frame` (`image/x-rgba`), `audio`
  (`audio/x-f32le`), `spikes` (`application/x-spike-bitset`), and the header as the payload
  `{"header": {...}}`. A header over 48 KiB of JSON goes as a `header` artifact instead, so
  the 65,536-byte envelope limit can never make a snapshot unpublishable.
- The sim thread is untouched. The publisher reads the same `watch` slot the direct server
  reads, so a slow bus skips snapshots the way a slow WebSocket client does, and nothing on
  the bus can hold the loop's publish. `fly_bus_published_total` and
  `fly_bus_publish_failures_total` count it.
- `fly-edge` subscribes `latest`, one in flight, with replay, rebuilds each `Snapshot` with
  `feedbus::receive` and serves it with flysim's own `feed::router`. `hello`, `wants`,
  drop-oldest, the idle header and the framing are therefore the same code, and the bytes are
  the same bytes: `crates/fly-edge/tests/parity.rs` replays the committed stage fixtures
  through both paths at once and requires byte-equal messages per client flavour.
- `fly_frames_sent_total` and `fly_feed_clients` move to the edge with the clients; it exports
  them under the same names on `FLY_EDGE_METRICS_ADDR` (`127.0.0.1:9102` in
  `infra/units/flyedge.service`), and watchdog check 2 follows `FLY_FEED_VIA` in `fly.env` to
  them. flysim's own copies read 0 in bus mode; `/status` is otherwise unchanged.
- Nothing about the fly changes: the readout, the reward catalog, the adapter version and the
  compatibility string are byte-identical in both modes (`--print-compatibility`).

### Amendment 2026-09-23: feed sizing

Measured on the live fly (release build, the real cartridge): a running snapshot is a
**92,160-byte** frame (160x144 RGBA; not the 640x480 "1.2 MB" the pending list assumed), a
**17,407-byte** spike bitset (139,255 neurons), about **12,800 bytes** of audio at realtime
(1,600 stereo f32 frames per 30 Hz snapshot at 48 kHz) and a 2 to 3 KB header: **122,367
bytes** of artifacts, about 3.7 MB/s at 30 Hz. `flysim::feedbus::limits()`:

| Limit | Value | Why |
| --- | --- | --- |
| `max_clients` | 8 | flysim's publisher, the edge, and room for a recorder or a probe |
| `max_latest_in_flight` | 2 | the default; the edge asks for 1 |
| `max_artifact_bytes` | 4 MiB | ten seconds of audio that piled up behind a late publish |
| `max_store_bytes` | 32 MiB | tmpfs, so RAM; ten times the worst case below |
| `max_retained_bytes` | 8 MiB | one retained snapshot, plus a large header artifact |
| `max_owners_per_client` / reserved | 64 / 8 | three artifacts per delivery, a few deliveries |
| others | small counts | one topic, no services |

A `latest` subscriber that never consumes pins at most its queued slot plus its in-flight
credits (3 snapshots); the topic pins one retained value; the publisher holds one snapshot of
staging plus the sealed copy while sealing. Seven stuck subscribers are therefore 24 snapshots,
about 3 MB, and publication never waits on any of them (a latest subscriber is never a
reason to refuse a publication, bus-v1 section 9). Measured in
`crates/fly-edge/tests/stall.rs`: a subscriber that hoards every delivery holds the store at
4 snapshots (489,468 bytes) while 179 of 179 snapshots are published, with pacer lag 0.

### Amendment 2026-09-23: feed store lifecycle

- **Location.** `feed.bus_dir` (`FLY_BUS_DIR`), `/run/fly/bus` on the containers: tmpfs,
  0700, owned by `fly`, created by tmpfiles and again by flysim. The store root is
  `<bus_dir>/store`, the socket `<bus_dir>/edge.sock`. A reboot empties it.
- **Owner.** The router lives in flysim; its lifetime is flysim's. flysim removes a stale
  socket file at start, and `Router::new` removes any store directory whose `flock` is free,
  i.e. one a crashed flysim left behind. A clean stop removes its own directory. The edge owns
  nothing on disk.
- **Order.** flysim first, the edge after it: `flyedge.service` is `After=` and
  `Requires=flysim.service`, so an explicit stop or restart of flysim (the unstick rule's
  restart included) takes the edge with it. A crash-restart of flysim needs nothing: the edge
  sees the connection close, drops every WebSocket client, unbinds :7400 and reconnects every
  500 ms, binding :7400 again only when the first snapshot of the new router arrives. To the
  stage that is exactly a flysim restart in direct mode: refused, then back.
- **Default.** `flyedge.service` is in no target and `07-enable.sh` does not enable it;
  `05-deploy.sh` writes `FLY_FEED_VIA=direct` unless the env file says otherwise. The switch
  and the way back are in the unit's header.
- **Migration order** is unchanged: the feed first; control only after the bus has carried
  the feed in production for a full session.
