# flysim

The service: one process that runs the fly brain and its Game Boy, publishes the snapshot feed,
serves the control API and checkpoints itself. It owns wall-clock time, sockets and disk;
`flybrain-core` owns the neurons and `flybrain-gb` owns the cartridge.

Binding contracts, and the only place the wire format is specified:

- `docs/feed-protocol.md` — the WebSocket snapshot feed, `ws://127.0.0.1:7400/feed`
- `docs/control-api.md` — the localhost HTTP control API, `http://127.0.0.1:7401`

`docs/design/flysim.md` sections 4 to 9 are the implementation plan, as amended by the contract
override at the top of that document. `packages/feed/src/fake/server.ts` is the behavioural
reference: this service is a drop-in replacement for it, and
`packages/feed/src/schema.json` pins the header for both languages.

## Running it

```sh
cargo run --release -p flysim -- --config flysim.toml
```

`../../flysim.toml.example` is a commented copy of every default. The file is **optional**:
`infra/units/flysim.service` starts the binary with no `--config` at all and configures it
entirely through the environment. `flysim --check-config` prints the resolved configuration and
exits.

```sh
# what the systemd unit sets, and the shortest useful local invocation
FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
FLY_DATASET=data/fafb-v783 \
FLY_STATE=/tmp/fly/state FLY_STATE_HOT=/tmp/fly/hot \
  cargo run --release -p flysim
```

### Environment

The environment always wins over the file. Two families are accepted: the `FLY_*` names the
infra units already set, and `FLYSIM_<SECTION>_<KEY>` for every field.

| Variable | Sets | Set by |
| --- | --- | --- |
| `FLY_GAME` | `loop.game` (`pokemon-red` or `platformer`) | `/etc/fly/fly.env`, from `infra/env/example.env` |
| `FLY_ROM` | `paths.rom` | `/etc/fly/fly.env` (`/srv/fly/rom/<sha>.gb`) |
| `FLY_ROM_SHA256` | `control.expect_rom_sha256`; only logged | `/etc/fly/fly.env` |
| `FLY_DATASET` | `paths.dataset` | — |
| `FLY_STATE` | `paths.save_dir` | `flysim.service` (`/srv/fly/state`) |
| `FLY_STATE_HOT` | `paths.hot_dir` | `flysim.service` (`/run/fly/state`, tmpfs) |
| `FLY_FEED_BIND` | `feed.bind` | `flysim.service` (`127.0.0.1:7400`) |
| `FLY_CONTROL_BIND` | `control.bind` | `flysim.service` (`127.0.0.1:7401`) |
| `FLY_METRICS_ADDR` | `control.metrics_bind`, the read-only listener | `flysim.service` (`0.0.0.0:9101`) |
| `FLY_CHAT_ENABLED` | `chat.enabled`, the on-screen chat kill switch | `/etc/fly/fly.env`, from `infra/env/example.env` |
| `FLY_CHAT_DENY_LIST` | `chat.deny_list` | `/etc/fly/fly.env` (`/srv/fly/chat-deny.txt`) |
| `RAYON_NUM_THREADS` | `loop.threads` | `flysim.service` (`6`) |
| `FLYSIM_LOG` | log filter, e.g. `flysim=debug` | — |

`FLYSIM_LOOP_SPEED=0` runs unthrottled, for soak tests. `FLYSIM_LOOP_HOT_SECONDS` and
`FLYSIM_LOOP_CHECKPOINT_SECONDS` move the two checkpoint cadences.

### Under systemd

`Type=notify` with `WatchdogSec=30`: `READY=1` goes out once the listeners are bound and the
simulation has restored or warmed up, and `WATCHDOG=1` is sent **from inside the simulation
loop** every `WATCHDOG_USEC / 3`. A ping from a timer task would keep the unit alive while the
loop was wedged, which is the one failure the watchdog exists to catch. `sd_notify` is 40 lines
of `UnixDatagram` in `src/sdnotify.rs` rather than a dependency.

Logs go to **stderr**. binjgb's `init_emulator` prints the cartridge header to stdout on every
boot and silencing it would mean patching a vendored source
(`crates/flybrain-gb/README.md`, "Known quirk"), so stdout is left to it.

## What runs where

```text
                     +-- watch<Snapshot> --> feed  :7400/feed   (axum + ws, per-client `wants`)
 sim thread ---------+
  agent              +-- Shared ------------> api   :7401        (axum)
  emulator           |                          /status /status.json /stimulate /reward
  adapter            |                          /checkpoint /pause /resume /events
  ratchet            +<-- mpsc<Command> ------ api  /healthz /metrics
  event log
  checkpoints -------> store thread --------> save_dir + hot_dir
```

One sim thread owns the agent, the emulator, the adapter and the ratchet; nothing else touches
them. Commands are drained at exactly one point per frame, immediately before the brain step, so
command application is deterministic. Snapshots leave through a `watch` slot, which is
drop-oldest by construction: a slow client misses snapshots (counted as `fly_feed_dropped_total`)
instead of slowing the loop down. Serialising a checkpoint happens on the sim thread, so the
snapshot is consistent; the write and the fsyncs happen on the store thread.

| Module | What it is |
| --- | --- |
| `config` | `flysim.toml` plus the environment, every field defaulted and validated |
| `simloop` | the loop, the commands, the sugar admission, the checkpoint scheduling |
| `pacing` | absolute frame deadlines, `loop.speed`, the lag accounting, the realtime window |
| `snapshot` | the feed protocol types, the exact framing, the spike bitset, the audio conversion |
| `feed` | the WebSocket listener and the client hello |
| `api` | the control routes, built from one route table |
| `ratelimit` | the per-minute sugar budget and the no-overlap rule |
| `store` | the `FLYSIM01` envelope, atomic commits, generations, milestone archives, restore order |
| `eventlog` | append-only JSONL plus the in-memory tail the feed and `/events` read |
| `metrics` | Prometheus text, including the two series `fly-watchdog` scrapes |
| `sdnotify` | `READY`, `WATCHDOG`, `STOPPING` |

### The frame

Pinned to the prototype worker (`fly-plays-pokemon/src/simulation.worker.ts`, `tick()`) rather
than to `NeuralAgent::tick`'s argument order, because the prototype samples reward inside the
same frame it produced:

1. drain commands
2. step the brain 16 or 17 ms (`1000 / (4194304 / 70224)`, the remainder carried and checkpointed)
3. decode
4. apply the buttons
5. run one emulator frame
6. set the visual frame from it
7. sample rewards from it
8. stimulate once per reward event
9. reinforce with the summed value
10. ratchet observe, and recover if it says so
11. publish

No frame is ever skipped: brain time and game time are one clock. When the loop cannot keep up
the shortfall accumulates as `fly_lag_seconds` and is warned about, first past 1 s and then every
30 s.

### Checkpoints

The envelope is `flybrain-core`'s frozen `encodeEnvelope` layout with the magic `FLYSIM01`, so
the TypeScript `decodeEnvelope` can read a flysim checkpoint for recap and inspection tooling. It
carries the seven agent chunks (`membrane`, `refractory`, `lastSpikeMs`, `visualDrive`,
`plasticGains`, `plasticTraces`, `plasticTouched`) plus `emulator`, `framebuffer`, `ratchetGame`
and `ratchetFrame`, and a manifest holding the compatibility string, the ROM hash, the frame
counter and the adapter's and ratchet's own state.

A commit writes `<gen>.checkpoint.tmp`, fsyncs it, renames it, fsyncs the directory, and then does
the same for `manifest.json`. **The manifest rename is the commit point**: a crash before it
leaves an unreferenced file that is never loaded.

- durable, to `save_dir`: every `checkpoint_seconds` (300), and on startup, on a rank-up, after a
  recovery, on pause, on `POST /checkpoint` and on shutdown;
- hot, to `hot_dir` on tmpfs: every `hot_seconds` (5). Same envelope, same atomic sequence, so it
  is a first-class restore candidate. `infra/bin/fly-watchdog` alarms if its mtime ages past 30 s;
- the first commit at a new best rank also writes `milestone-<rank>.checkpoint`, which later
  rotations never unlink.

Restore order: hot latest, hot previous, durable latest, durable previous, then the archives by
descending rank. Each candidate is checked for magic, CRC32, schema, ROM hash and the exact
compatibility string before anything is imported, and `NeuralAgent::import_state` is
self-validating and a no-op on failure, so a refused candidate leaves the agent untouched and the
next one starts clean. A fallback sets `fly_restore_fallback` and logs an event. **If candidates
exist and every one fails, the process exits non-zero** — no automatic fresh start, ever. A
deliberate reset means moving the save directory by hand.

## On-screen chat

`POST /chat` appends one line to a bounded ring that every snapshot header carries as `chat`
(`docs/feed-protocol.md`). It is a caption track: **chat never reaches the simulation**. The
handler touches the ring, the event log and two counters, and nothing else — no button, no
stimulation, no plasticity, no emulator memory.

The path, and where each step is enforced:

```
Twitch -> AutoMod -> flybridge (validateDisplayName, sanitizeChatText, bot/command filtering)
       -> POST /chat -> flysim: name rule, sanitizer, deny list, rate limits -> ring -> header
```

Nothing in that chain is trusted from the step before it. `docs/control-api.md`: limits are
enforced here "regardless of what the bridge does".

- **The sanitizer** (`src/chat.rs`) is the same rule set as `packages/feed/src/chat.ts`: NFC, no
  control characters, an allowlist of letters, digits, spaces and common punctuation (so no emoji,
  no combining marks, no zero-width or bidi characters), whitespace collapsing, a 200-code-point
  cap, and a refusal for anything that looks like a link. The two implementations are pinned to
  each other by `packages/feed/tests/fixtures/chat-cases.json`, which `tests/chat.rs` and the
  TypeScript suite both load — a rule changed in one language fails in both.
- **The deny list** is `[chat] deny_list`, one pattern per line, `#` comments, matched
  case-insensitively anywhere in the name or the sanitized text. It is re-read on **SIGHUP**
  (`systemctl kill -s HUP flysim`, which does nothing else) and at most once a minute anyway. A
  missing file is an empty list and a warning, not a startup failure.
- **Admission** is 1 accepted line per name per 2 s and 5 per second globally. A line a rule
  refused never spends rate budget.
- **The event log** gets one `viewer` event labelled `chat` per accepted line, carrying the name
  only. Chat text is never written to `events.jsonl`.
- **The kill switch** is `[chat] enabled = false` (`FLY_CHAT_ENABLED=0`): the endpoint answers
  403 and the header omits `chat` entirely, so the page can tell "chat is off" from "nobody has
  said anything yet". It needs a restart of `flysim` only, nothing else in the stack.
- **Metrics**: `fly_chat_accepted_total`, `fly_chat_ring_lines`, and
  `fly_chat_rejected_total{reason}` with one series per rule (`control`, `charset`, `empty`,
  `too_long`, `url`, `name`, `deny_list`, `rate_limited`, `malformed`), all present from boot.

Why any of this exists rather than rendering chat directly: `docs/stream-mvp-plan.md` records the
Nothing, Forever precedent (a 14-day ban for generated text on stream) and the rule that follows —
never render raw chat into the frame.

### There is no button endpoint

The decoder is the only writer of joypad state. `api::ROUTES` is the whole control surface, the
router is built from it, and `tests/api.rs` asserts both over the table and against the live
router for thirteen spellings of such a route. The prototype's `manualButtons` field and its
`learn: false` suppression are dropped, not disabled.

## Tests

```sh
cargo test --workspace            # ROM-gated tests skip
cargo clippy --all-targets
FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
  cargo test --release -p flysim --test integration -- --nocapture
```

| Test | Covers |
| --- | --- |
| `tests/schema.rs` | a produced header validates against `packages/feed/src/schema.json` (the `jsonschema` crate, so no Node toolchain), plus the mistakes the schema exists to catch |
| `tests/codec.rs` | round-trip against a decoder transcribed from `packages/feed/src/codec.ts`, every `wants` subset, the contract's own attachment sizes |
| `tests/api.rs` | the whole control surface over the built router, and the no-button-route guarantee |
| `tests/store.rs` | the candidate walk: hot before durable, corruption skipped, another cartridge or build refused, the archive as last resort |
| `tests/integration.rs` | ROM-gated end-to-end: boot, 10 snapshots with all three attachments, sugar, a forced checkpoint, `SIGKILL`, restore at the same frame counter, `SIGTERM` |
| `src/*.rs` unit tests | config and its env overrides, pacing, the rate limiter, the envelope and atomic commit, the event log and its daily rotation, the metrics text, `sd_notify` |

Measured on the development laptop (WSL2, release build, six other things running):

| | |
| --- | --- |
| boot to a healthy `/healthz`, fresh | 1.8 to 2.4 s (dataset load plus the 2,500 ms warm-up) |
| boot after a restore | 0.2 to 0.4 s (no warm-up) |
| feed rate published | 30.15 Hz, `seq` continuous, nothing dropped |
| realtime factor | 0.87x to 1.02x, moving with what else the box is doing |
| header size | 1.1 to 1.4 KB (the contract's ceiling is 4 KB) |
| snapshot size, all three attachments | about 122 KB, 3.7 MB/s at 30 Hz |

The realtime factor is the number that matters for the stream and it is not settled by this
crate: `crates/flybrain-core/README.md`, "Performance", measures 1.33x on a 5800X3D and explains
why more threads will not raise it (spike propagation is 61% of a tick and is sequential by
design). `loop.threads = 0` — a sequential sweep — is the fastest setting measured so far.
