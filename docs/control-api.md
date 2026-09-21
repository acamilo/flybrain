# Control API v1 (flysim, localhost HTTP)

`http://127.0.0.1:7401`. JSON in and out. Bound to loopback only; there is no auth because
nothing outside the container can reach it, and the bridge is the only intended caller.

There is deliberately **no endpoint that presses buttons, edits game memory, or changes the
reward catalog**. That is a structural guarantee, not a configuration.

| Method and path | Body | Effect |
|---|---|---|
| `GET /status` | | Same fields as the feed header minus events and attachments, plus `version` strings (kernel, plasticity, adapter, binjgb, dataset fingerprint) and `checkpoint: { latestWallMs, generation }`. Includes the macro buttons' `game.scene`, `game.macroMode`, `game.palette`, `game.macro` and `game.macroOutcome` (2026-09-16), because the body is the header reshaped rather than a parallel struct. `game.macroMode` is `"raw"` or `"macros"`, and each `game.palette` entry carries its `slot` (0..5), its `name`, its `gloss` and its `channel` tag (`docs/design/macros.md` section 12). |
| `POST /stimulate` | `{ durationMs?: number, by: string, source: 'chat' \| 'points' \| 'operator' }` | Fires a PAM stimulation pulse (`stimulate()` on the network; same pathway game rewards use). Default and maximum `durationMs` come from config (default 400, max 1000). Returns `202 { eventId }` or `429 { retryAfterMs }` when the global rate limit or an active pulse blocks it. Every accepted call appends a `sugar` event to the feed and event log. |
| `POST /reward` | `{ value: number, by: string, source }` | Reinforcement pulse into plasticity. **Disabled by default** (`control.allowReward = false`) and returns `403` when disabled. Present so the later "who trains the fly" work does not change the API. |
| `POST /chat` | `{ by: string, text: string, bot?: boolean }` | Appends one chat line to the feed's `chat` ring (last 12). The bridge forwards only messages that Twitch AutoMod has already let through; the service additionally validates the name, caps text at 200 chars, strips control characters and URLs, rejects non-printable or non-allowlisted characters, and applies the deny list in `flysim.toml [chat]`. Returns `202` or `422`. Rate limit per name 1 per 2 s, global 5 per s. Chat never reaches the simulation. |
| `POST /checkpoint` | | Forces a checkpoint now. Returns the generation written. |
| `POST /pause` and `POST /resume` | | Operator use only. Paused state is visible in the feed. |
| `GET /events?since=<id>&limit=<n>` | | Event log page for the bridge and recap tooling. |
| `GET /healthz` | | `200` when the loop advanced in the last 2 seconds, else `503`. Used by systemd and the watchdog. |

Rate limits (config, defaults): sugar accepted at most 6 per minute globally, no overlap with an
active pulse, per-viewer limits are the bridge's job. Limits are enforced here regardless of
what the bridge does.

Event log: append-only JSONL at `<saveDir>/events.jsonl`, one `FeedEvent` per line, rotated
daily. The feed's `events` array is the live tail of this file.

Config (`flysim.toml`, overridable by `FLYSIM_*` env):

```toml
[paths]
rom = "/srv/fly/rom/pokemon-red.gb"
dataset = "/srv/fly/data/fafb-v783"
save_dir = "/srv/fly/saves"

[loop]
speed = 1.0            # target realtime factor; the loop sleeps when ahead
threads = 6            # rayon pool size
checkpoint_seconds = 5

[feed]
bind = "127.0.0.1:7400"

[control]
bind = "127.0.0.1:7401"

allow_reward = false
sugar_default_ms = 400
sugar_max_ms = 1000
sugar_per_minute = 6

[chat]
enabled = true
ring = 12                              # 1..12; the feed header schema's own ceiling
deny_list = "/srv/fly/chat-deny.txt"   # one pattern per line; operator-maintained

[macros]
mode = "raw"                           # "raw" or "macros"; FLY_MACRO_MODE overrides it
```

`[macros]` (2026-09-16, `docs/design/macros.md` section 12):

- `raw` is the default and unchanged behaviour: the population decoder's eight button channels
  drive the emulator's button register and nothing else is on the pad.
- `macros` puts the current scene's macro types on the pad as well, each pressed by its own
  neuron population through the decoder's second exclusive group (`docs/readout.md`, "Macro
  group"). The eight buttons keep working the whole time; a running macro owns the pad until it
  ends, so a raw press and a macro can never overlap. Which types the scene binds is the game
  layer's, per decision, and an unbound type is masked out of the decision rather than losing it.
- `FLY_MACRO_MODE` (what `infra/env/example.env` sets) and `FLYSIM_MACROS_MODE` override the file, and
  both are case-insensitive. `palette` and `plan`, the two modes section 12 removed, are accepted
  as `macros` with a warning **for one release** and are then a startup failure like any other
  unknown word: a box configured for a macro mode that quietly streams raw is the harder failure
  to notice. There is no `[macros.bias]` table and no `FLY_MACRO_BIAS_*` variable any more —
  nothing is weighed against anything, so there is no weight to set.
- `FLY_MACRO_BLOCKED_MINUTES` (default 10) is how long a macro leaves a target alone after a walk
  to it aborted `blocked` or `timeout` (`docs/design/macros.md` section 12.1). Session state, no
  endpoint, and a value that is not a positive number is the default with a complaint on stderr.
- **There is no endpoint and no chat command for this.** Which mode a box runs in is a property
  of the deployment, and the structural guarantee at the top of this document is unchanged:
  nothing here presses a button, and a macro's presses come from the sim thread's own decoder
  path, not from an API.
- Only `pokemon-red` has macros. `mode = "macros"` with any other `loop.game` is refused at
  startup rather than silently downgraded.
- Every macro start and finish appends one `macro` event to `events.jsonl`, labelled
  `<NAME> start` or `<NAME> done|blocked|timeout|refused` with the slot in `value`.

`[chat]` in detail:

- `enabled = false` is the kill switch: `POST /chat` answers `403` and the feed header omits
  `chat` entirely (rather than sending an empty array), so the page can tell "chat is off" from
  "nobody has said anything yet". `FLY_CHAT_ENABLED=0` sets it from `infra/env/example.env`.
- `deny_list` patterns match case-insensitively anywhere in the display name or the sanitized
  text. The file is re-read on **SIGHUP** — which does nothing else, so
  `systemctl kill -s HUP flysim` cannot disturb the stream — and at most once a minute anyway. A
  missing file is an empty list and a warning, never a startup failure.
- The admission limits (1 accepted line per name per 2 s, 5 per second globally) and the text
  rules are not configurable. The text rules live twice, in `packages/feed/src/chat.ts` and
  `services/flysim/crates/flysim/src/chat.rs`, pinned to each other by the shared fixture
  `packages/feed/tests/fixtures/chat-cases.json`.
- Refusals are counted as `fly_chat_rejected_total{reason}`, one series per rule: `control`,
  `charset`, `empty`, `too_long`, `url`, `name`, `deny_list`, `rate_limited`, `malformed`.
  Acceptances are `fly_chat_accepted_total`, and the ring depth is `fly_chat_ring_lines`.

`POST /chat` status codes: `202 { eventId }` accepted, `400` malformed body, `403` chat disabled,
`422 { error }` a rule refused the line (the error names the rule), `429 { retryAfterMs }` a rate
limit refused it. Every accepted line also appends one `viewer` event labelled `chat` to
`events.jsonl`, carrying the display name only — **chat text is never written to the event log**,
and never reaches the simulation.

No secrets live in this service or its config.
