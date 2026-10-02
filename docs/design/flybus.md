# flybus: the communications bus

Status: **crate landed; the feed rides it behind `FLY_FEED_VIA=bus` and the control API behind
`FLY_CONTROL_VIA=bus`, both off by default; EDGE-02 (2026-10-01) made the feed switchable in
production with one command, `fly-feed bus|direct`**. Written 2026-09-22, amended 2026-09-23
(EDGE-01) and 2026-10-01 (EDGE-02 and CTRL-01, below). Index only; the
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
- ~~**flysim control services.**~~ Done 2026-10-01 behind `FLY_CONTROL_VIA=bus`: see "Control
  over the bus". The "no button endpoint" guarantee is a grant table there.
- **Stage and bridge clients.** Both are TypeScript/Node; the crate is Rust only, so either
  a binding or a thin translating edge process is required before they leave the WebSocket
  and HTTP surfaces. The operator chose the edge process (port decisions, 2026-09-23); for
  the feed it is `fly-edge` and for the control API `fly-control-edge`, and both keep their
  contracts byte for byte.
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
  them. flysim's own copies read 0 in bus mode; `/status` is otherwise unchanged. The edge also
  exports `fly_edge_bus_connected`, `fly_edge_bus_lost_total`, `fly_edge_bind_failures_total`
  (the bus answered but :7400 was taken, most likely by a flysim still in direct mode) and
  `fly_edge_decode_failures_total`.
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
| `max_clients` | 8 | connections, pending handshakes included: the in-process publisher and the edge's one socket seat |
| `max_subscriptions_per_client` | 4 | the edge needs 1; this is what bounds the worst case |
| `max_latest_in_flight` | 2 | the default; the edge asks for 1 |
| `max_artifact_bytes` | 4 MiB | ten seconds of audio that piled up behind a late publish |
| `max_store_bytes` | 32 MiB | tmpfs, so RAM; ten times the worst case below |
| `max_retained_bytes` | 8 MiB | one retained snapshot, plus a large header artifact |
| `max_owners_per_client` / reserved | 64 / 8 | three artifacts per delivery, a few deliveries |
| others | small counts | one topic, no services |

A `latest` subscriber that never consumes pins at most its queued slot plus its in-flight
credits (3 snapshots); the topic pins one retained value; the publisher holds one snapshot of
staging plus the sealed copy while sealing. Only one client can subscribe at all: the publisher
is in process, and `edge.sock` is launcher-bound to `fly-edge`, which the router admits once at
a time (a second connection is refused as already connected). The worst case is therefore that
one client holding all 4 subscriptions it may open, none consuming: 4 x 3 + 1 + 2 = **15
snapshots, about 1.8 MB**, and publication never waits on any of them (a latest subscriber is
never a reason to refuse a publication, bus-v1 section 9). `crates/fly-edge/tests/stall.rs`
measures exactly that seat: four hoarding subscriptions, a fifth refused, a second connection
refused, pacer lag 0, no publication refused. The 15 is an upper bound; the measured store
was 472,061 bytes (under 4 snapshots), because fan-out adds roots and never copies, so four
subscriptions stuck on the same publications pin the same artifacts.

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
  `05-deploy.sh` writes `FLY_FEED_VIA=direct` unless the env file says otherwise, and refuses
  anything but `direct` or `bus` (any case, written lowercased). The switch and the way back
  are in the unit's header. The edge gets a cpuset drop-in on the page's CPUs with the other
  units, so once enabled it never runs on flysim's.
- **Paths.** `feed.bus_dir` must be absolute and non-empty (checked in both modes), since
  flysim and the edge each resolve it.
- **Migration order** is unchanged: the feed first; control only after the bus has carried
  the feed in production for a full session.

### Known limits (review round 1, 2026-09-23)

Accepted for now and written down rather than fixed:

- **Feed counters off the container.** In bus mode flysim's `:9101` reports
  `fly_feed_clients` and `fly_frames_sent_total` as 0, and the edge's copies are on loopback
  `:9102` only. The watchdog follows `FLY_FEED_VIA`; anything that scrapes `:9101` from off
  the container (the metrics dashboard) goes blind to the feed until it also scrapes the edge.
- **Store quota is per router, not per client.** Any client on `edge.sock` may allocate
  artifacts up to the store cap; a hostile process running as the same user could fill the
  store and make flysim's publications fail. The loop is unaffected (a refusal is counted, never
  waited on), but the feed would stall. Same-user processes are inside the trust boundary
  (crate README, "Limitations").
- **Rollback while in bus mode.** Rolling back to a release without `fly-edge` while `fly.env`
  still says `bus` leaves no one on :7400, and check 2 then reads the edge's absent `:9102` and
  escalates. Switch back to `direct` first (the unit header's way back), then roll back.
- **Old fixtures.** `cold-open`, `steady` and `big-moment` predate `game.scene` and cannot be a
  Rust `FeedHeader`, so fixture parity covers `macros`, `shop`, `center` and `bigpad`.

### Amendment 2026-10-01: the feed in production (EDGE-02)

- **The session runtime needed nothing.** `flysim-session` serves its listeners through
  `flysim::serve`, the same function `flysim` does, so `FLY_FEED_VIA=bus` works on it as it was
  written for legacy. It is now a test, not an inference: `fly-legacy-session/tests/feed_bus.rs`
  runs the real `flysim-session` binary (toy connectome, the real cartridge) twice from copies of
  one store, direct and bus, records both feeds over a WebSocket and requires every frame in
  common to be identical (header minus the wall-clock fields, frame, audio and spike bytes), then
  stops the binary and starts a new one under the same edge, which must unbind, reconnect and
  serve. In the session runtime the router and publisher (`flysim-bus`, two threads) inherit the
  host mask of `place_workers` (PERF-02), so they run on the host CPU with the listeners, never on
  a pinned sweep CPU.
- **One switch.** `FLY_FEED_VIA` in `fly.env` decides everything: `flyedge.service` has an
  `ExecCondition=` on it (skipped, not failed, in direct mode) and no `[Install]`;
  `flysim.service` has `Wants=flyedge.service`, so every start of flysim, including the stop and
  start of `fly-loop-reset` that `Requires=` alone left without an edge, brings the edge up in bus
  mode. Checked on systemd 257 with the repo's own units: crash, restart and stop/start of flysim
  all end with the edge serving; direct mode never starts it. `infra/bin/fly-feed bus|direct`
  rewrites the line, restarts flysim, waits for a new invocation, the edge's `/healthz` and the
  page on the feed (frames moving), and rolls itself back (to `direct`) when that does not happen
  or when it is interrupted. It works on either runtime and survives `fly-runtime` changes.
- **Edge health.** `/healthz` is 503 unless the edge is subscribed **and** has taken a snapshot off
  the bus within 15 s (`fly_edge_snapshot_age_seconds`): a loop publishes at least the 2 Hz idle
  header, so silence means a wedged subscription, which `Subscription::next` alone would never
  report.
- **Watchdog.** `feed_via` follows the *running* flysim's environment (a deploy that rewrote
  fly.env but has not restarted flysim is not an outage). New check 2a judges the edge only while
  flysim answers: second failed pass restarts it, third adds the page and the encoder, fourth
  switches the feed back to flysim (`fly-feed direct --no-wait`, sudoers for exactly that line).
  Check 2 in bus mode leaves a silent edge to 2a instead of restarting a page with nothing to
  connect to.
- **Deploy.** `05-deploy.sh` installs `fly-feed`, refuses a release without `fly-edge` while the
  env file or the container says `bus` (before `current` moves; `fly-feed direct` needs no
  binary), and warns when the container's `FLY_FEED_VIA` differs from the env file's. flysim and
  the edge both follow fly.env at their next start, so a disagreement never leaves the stage with
  no feed.
- **Known limits, revisited.** *Feed counters off the container*: the edge's `/metrics` now listens
  on `0.0.0.0:9102` like flysim's `:9101`; a dashboard adds it as a second target (the watchdog
  stays on loopback). *Rollback while in bus mode*: refused by the deploy and one command by
  `fly-feed direct`. *Store quota per router* and *old fixtures*: unchanged, accepted as written.

## Control over the bus (2026-10-01, CTRL-01)

`control.via` (`FLY_CONTROL_VIA`) picks who serves `http://127.0.0.1:7401`. `direct` is the
default and the behaviour that predates the bus. With `bus`:

```text
 api router (flysim::api)       BusBackend            flybus (Unix socket)       control host (in flysim)
 :7401 in fly-control-edge  --> encode_request --> <bus_dir>/control-edge.sock --> fly.control.s.main.* services
   same routes, extractors,       (controlbus)        bound to "fly-control-edge"     -> flysim::control::handle
   404s, headers, bodies                                                              -> the same Command queue
```

- `docs/control-api.md` is unchanged and still binding: every endpoint, status code and body.
  flysim's control code is split in two. `flysim::control` holds the semantics: one
  `ControlRequest` per endpoint, and `handle`, the only code that validates a request, talks to
  the loop and picks a status. `flysim::api` is the HTTP layer over any `ControlBackend`. Direct
  mode is `api` over `control` in one process. Bus mode is the same `api` in `fly-control-edge`
  over a `BusBackend`, which carries the request to the `control` services in flysim. Neither
  process has an HTTP table or a validation rule of its own, so the bytes on `:7401` are the
  same in both modes. `crates/fly-control-edge/tests/parity.rs` sends every endpoint, each
  validation case and each refusal to both paths and requires equal status, headers and
  body. Over real sockets, it also requires the whole HTTP/1.1 response to be equal apart from
  `date`. `fly-legacy-session/tests/service_parity.rs` runs the session runtime once more with
  its control on the bus and compares it with the legacy loop. The comparison covers 120
  frames, 19 control responses, the event log, the journal and the final checkpoint.
- Both runtimes get it: `flysim` and `flysim-session` both run `flysim::serve`, which in bus mode
  does not bind `control.bind`. Instead it registers the services on the embedded router. That
  is the feed's router when the feed is on the bus too: one router, one store, one closed policy
  (`flysim::bus`). The read-only `control.metrics_bind` listener (`:9101`) stays in flysim in
  both modes.
- Nothing about the fly changes. The command queue, its bound and its timeouts are the same.
  The compatibility string is byte-identical in both modes.

### Services and methods

flybus grants name services, not methods. The surface is therefore split into six service
*families*, and a grant names families. Every family is scoped either to a session or to one
agent (fly) of it. Each method's payload and outcome:

| Service (live names) | Method | Endpoint | Payload |
| --- | --- | --- | --- |
| `fly.control.s.main.read` | `Control.Status` | `GET /status`, `/status.json` | `{}` |
| | `Control.Healthz` | `GET /healthz` | `{}` |
| | `Control.Metrics` | `GET /metrics` | `{}` |
| | `Control.Events` | `GET /events` | `{since?: string, limit?: string}`, the raw query values |
| `fly.control.s.main.a.fly.status` | `Control.Status` | (none: the fly's own status, for native clients) | `{}` |
| `fly.control.s.main.a.fly.sugar` | `Control.Stimulate` | `POST /stimulate` | `{request?: <body JSON>}` |
| `fly.control.s.main.a.fly.reward` | `Control.Reward` | `POST /reward` | `{request?: <body JSON>}` |
| `fly.control.s.main.chat` | `Control.Chat` | `POST /chat` | `{request?: <body JSON>}` |
| `fly.control.s.main.ops` | `Control.Checkpoint`, `Control.Pause`, `Control.Resume` | `POST /checkpoint`, `/pause`, `/resume` | `{}` |

- **Outcome.** `{"status": <the control-api.md code>, "body": <the JSON body>}`, or `{"status",
  "text"}` for `/metrics`. Above 48 KiB (a full `/events` page can be 4,096 events) the body
  travels as a sealed `body` artifact instead, with `bodyArtifact` naming its type. The status
  code is the outcome's vocabulary, not transport. A bus caller reads `429 {retryAfterMs}` as an
  HTTP caller does, so the edge needs no table of its own.
- **Requests** carry the body as the caller sent it (`request` is absent when it was empty or
  not JSON), so validation stays in `control::handle`. A `request` over 48 KiB of JSON travels as
  a sealed `request` artifact with `requestArtifact: true` in the payload, so a large body gets
  the answer direct mode gives it, not an envelope error (`tests/parity.rs`). An unknown payload field is a 400. A
  method the service does not have is a 404. Neither reaches the loop.
- **Concurrency.** Each service has 16 queued and 16 in flight (`controlbus::SERVICE_CONFIG`).
  The router dispatches FIFO per service. The host answers each request in a task of its own, so
  a pause waiting on a durable write does not hold up a resume. Families are separate services,
  so a stuck `ops` never delays `/status`.
- **Backpressure.** The bound is on the queue at the moment of admission. A burst can
  therefore be refused before the router has dispatched its first calls into the in-flight
  slots. That is measured, not assumed (`tests/backpressure.rs`). A full service refuses at
  once with `BACKPRESSURE`, and the edge answers it
  with direct mode's `503 {"error": "the simulation command queue is full"}`. Bus failures use
  direct mode's words for the same failure (`control::unavailable`). `NO_SERVICE` before
  dispatch is "has stopped"; `NO_SERVICE` after dispatch, `CALL_GONE` and `ROUTER_LOST` are
  "dropped the request". `NOT_AUTHORIZED` is a 403.
- **Deadlines and cancel.** The host keeps direct mode's own timeouts: 2 s for a command,
  none for `/checkpoint` and `/pause`, which wait for a durable write. The edge adds a guard of
  that plus 2 s for timed requests only. At the guard it cancels the call and answers direct
  mode's timeout 503. A call cancelled before dispatch never reaches the loop. A dispatched call
  may still act, as a direct request whose caller timed out may. An HTTP client that goes away
  drops its handler, and with it the pending call. The router discards the late result, and
  the call record goes once the host answers (`tests/backpressure.rs`).

### Grants: the structural guarantee as a table

| Participant (socket) | read | status | sugar | reward | chat | ops | register |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `flysim-control`, the host (in-process, no socket) | | | | | | | every control service, exact names |
| `fly-stage` (`control/stage.sock`) | yes | yes | | | | | |
| `fly-bridge` (`control/bridge.sock`) | yes | yes | yes | | yes | | |
| `fly-watchdog` (`control/watchdog.sock`): watchdog, loop-recover ladder, `fly-runtime`, probation, shadow guard, stream check | yes | yes | | | | | |
| `fly-operator` (`control/operator.sock`) | yes | yes | yes | yes | yes | yes | |
| `fly-control-edge` (`control-edge.sock`): everything on `:7401` | yes | yes | yes | yes | yes | yes | |
| a player backing one fly (`player_grants`, not wired yet) | yes | that fly | that fly | | | | |

The status, sugar and reward columns are per agent: a role gets them for every fly of the
session (`grants`), or for one fly by id or by controller port (`grants_for`, `player_grants`
with `Agents::Id` or `Agents::Port`). The table is `controlbus::Role` and `controlbus::grants`. It reflects what each client calls
today: the stage only waits on `/healthz`; the bridge calls `/status`, `/stimulate`, `/chat`,
`/events` and `/healthz`; the infra scripts only read; the operator uses `curl`. Each socket is
launcher-bound to its participant. The watchdog and the ladder restart through systemd, never
through a control call.

"Nothing can press buttons" is structurally true because of four properties. Each is tested:

1. **No method takes input.** `ControlRequest` and the loop's `Command` have no variant that
   presses a button, edits game memory or changes the reward catalog, so no method can be added
   without a contract change. Service and method names are scanned for the same words `api.rs`
   scans routes for (`controlbus` unit tests).
2. **Grants are exact calls and nothing else.** No role is granted `Any`, a prefix, `register`,
   `publish`, `subscribe` or topic management (`every_grant_is_an_exact_call_...`). On a router
   that also carries an environment's input service, no control participant can call that
   service, the operator and the edge included. The router refuses before dispatch
   (`tests/grants.rs`, `on_a_shared_router_no_control_participant_can_reach_an_input_service`).
3. **Only the host registers.** The one register grant belongs to an in-process participant
   with no socket, so nothing outside flysim can stand in for a control service. A socket admits
   only its own participant; claiming another id at hello is refused.
4. **The policy is closed.** An id that is not in the table cannot connect.

What this does not change: loopback HTTP has no caller identity, so `fly-control-edge` carries
the whole surface. That is exactly the trust `:7401` has today. A client is narrowed by moving
it off HTTP onto its own role socket. That needs a native client for the TypeScript processes
(a later slice; the operator chose edges over a Node binding for now). The bus's own limit still
applies: processes running as the same OS user are inside the trust boundary (crate README,
"Limitations").

### N flies, M environments, controller ports, players

Operator direction (2026-10-01): first the existing Game Boy fly goes fully onto the framework
and the bus. A later project puts several flies on multi-controller consoles, one fly per
controller port. This slice does not build that, but the services and grants are scoped per
session, per agent and per port from the start, so multi-fly is configuration, not a redesign.
A single-fly deployment is the same design with one agent.

- **Scope** (`controlbus::Scope`): a session id and its agents, each an id and, where the
  environment has controller ports, the port it plays on (`AgentScope { id, port }`). The live
  fly is session `main`, agent `fly` on port 0, the Game Boy's one pad (`Scope::live`). A scope
  refuses two agents with one id or on one port.
- **Session-scoped** families (read, chat, ops) are `fly.control.s.<session>.<family>`. Pause and
  checkpoint act on a whole session: one coordinator steps every agent, and one save is one
  generation. The chat ring is the session's stage. `read` holds the session's status, health,
  metrics and event log.
- **Agent-scoped** families (status, sugar, reward) are
  `fly.control.s.<session>.a.<agent>.<family>`, one service per fly. A sugar or reward pulse goes
  into one brain. Each fly's own view is its `status` service. Adding a fly adds its three
  services, and nothing else changes.
- **Ports.** A port is a property of an agent's binding, not part of a name, because a pulse
  goes into a brain, not into a controller. A grant limited to a port
  (`Agents::Port(n)`) is resolved to the agent bound to that port when the policy is built. The
  policy is fixed for a router's lifetime, so rebinding ports is a session restart. A port nobody
  plays on grants no agent service.
- **M sessions** on one router (several games, or one multiplayer game) have disjoint names, so
  a grant on one session never names another's. `:7401` addresses the live scope only. A second
  session gets native bus clients, or an edge of its own on another port with its own scope.
- **Environments.** Environment-scoped status is reserved as
  `fly.control.s.<session>.e.<env>.read`. Environment *input* services (`Environment.Step` and
  the like) are never in a control scope. An agent's decision reaches its environment through
  the coordinator, never through control, and on a multi-controller console each fly's decoder
  writes only its own port. Property 2 holds however many environments and ports share the
  router.
- **Players.** A viewer who backs one fly gets `player_grants(scope, Agents::Port(n))` or
  `Agents::Id(..)`: read the session, see and sugar that fly, nothing else
  (`a_player_sugars_their_own_fly_and_no_other`). In a multiplayer game the players on the pads
  are flies, and their presses come from their decoders. A human input path would be a new
  contract, never a control grant.

### What in `docs/control-api.md` is single-fly

The contract is unchanged by this slice. These parts of it assume one fly per session. Each
would need a contract amendment before a second fly could be served over HTTP. The bus services
above are already addressed per agent, so the amendments are to the HTTP shapes, not the bus.

| Endpoint or rule | Why it is single-fly | Multi-fly direction |
| --- | --- | --- |
| `GET /status` | The feed header reshaped: one `buttons`, `rates`, `decoder`, `learning`, `game`, `milestone`, `sugar`, `version` | Per agent: each fly's `status` service. The session's `Control.Status` becomes a session shape listing its agents and ports |
| `POST /stimulate` | No agent field; addresses "the fly" | The agent's `sugar` service; over HTTP an `agent` (or `port`) field, absent meaning the only fly |
| sugar limits ("6 per minute globally, no overlap with an active pulse") | The overlap rule is per brain; the budget is per deployment | Overlap per agent; whether the budget is per fly or per session is a policy decision |
| `POST /reward` | No agent field | The agent's `reward` service; same field as `/stimulate` |
| `GET /events`, `events.jsonl` | `FeedEvent` has no agent: reward, sugar and macro events cannot say whose they are | An `agent` field on agent events, absent for session events |
| `POST /checkpoint` | One generation of one FLYSIM01 envelope, which is single-fly by format | A session checkpoint holding every agent. FLYSIM01 stays the format of record until RETIRE-01 |
| `GET /metrics` | Series carry no agent label (`fly_reward_*`, the rates) | An `agent` label on per-fly series |
| `[macros]`, `loop.game` | One mode and one game per deployment | Per environment |
| `POST /chat`, `/pause`, `/resume`, `/healthz` | Session-level already | None |

### Lifecycle and operations

- `bus_dir` (`FLY_BUS_DIR`, `/run/fly/bus`) as for the feed. Sockets are `control-edge.sock` and
  `control/{stage,bridge,watchdog,operator}.sock`. Stale sockets are removed at start. With
  control on the bus, the router's `max_clients` and `max_services` are the feed's plus 8.
- `flycontrol-edge.service` has no `[Install]` section: `flysim.service` `Wants=` it, and its
  `ExecCondition=` on `FLY_CONTROL_VIA` skips it in direct mode (EDGE-02's pattern for
  `flyedge`), so every start of flysim, `fly-loop-reset`'s stop and start included, brings
  `:7401` back. It is `After=` and `Requires=flysim.service`, gets the page's CPUs from the cpuset plan, and exports its own
  counters on loopback `:9103` (`fly_control_edge_bus_connected`, `_calls_total`,
  `_call_failures_total`, `_cancelled_total`, `_bus_lost_total`, `_bind_failures_total`).
- The edge binds `:7401` only once the services answer. When the bus goes away it unbinds and
  reconnects every 500 ms. To a client that looks exactly like flysim restarting
  (`tests/backpressure.rs`, `tests/serve.rs`).
- Watchdog. Check 1 reads flysim's own `/healthz` on `:9101` when `FLY_CONTROL_VIA=bus`, so an
  edge outage never restarts the fly. Check 1b restarts the edge alone when `:7401` fails while
  flysim is healthy. Every other `:7401` reader (`fly-runtime`, probation, shadow guard,
  loop-recover, the stream check) goes through the edge unchanged.
- Switch and rollback: `infra/docs/runbook.md`, "Control over the bus".
- **Migration order.** This document's rule stands: the control API moves after the feed has
  been on the bus in production for a full session. The switch exists independently of the
  feed's, and when to throw it is the coordinator's call.
