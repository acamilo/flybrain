# @flybrain/feed

Shared TypeScript types, a binary snapshot codec, a pinned JSON Schema and a runnable fake
`flysim` for local development — the contract between `flysim` (the Rust simulation service) and
its consumers, `flystage` (the display page) and `flybridge` (the Twitch chat/points bridge).

Binding contracts: [`docs/feed-protocol.md`](../../docs/feed-protocol.md) (the WebSocket
snapshot feed) and [`docs/control-api.md`](../../docs/control-api.md) (the localhost HTTP control
API). This package is the TypeScript side of those contracts; `services/flysim` mirrors the same
shapes with serde, and both sides test against `src/schema.json`.

## What is in here

- **`src/types.ts`**: every interface from the two contract docs (`FeedHeader`, `FeedEvent`,
  `ClientHello`, `StatusResponse`, `StimulateRequest`/`StimulateResponse`, `RewardRequest`,
  `EventsResponse`, and friends), plus the shared constants (`FEED_PROTOCOL`, `FEED_PORT`,
  `CONTROL_PORT`, `FRAME_WIDTH`, `FRAME_HEIGHT`, `AUDIO_RATE`).
- **`src/codec.ts`**: `encodeSnapshot` / `decodeSnapshot` implement the exact binary framing from
  `docs/feed-protocol.md` (`u32 LE headerLength | header JSON | attachments...`). Pure functions
  on `Uint8Array`/`DataView`/`TextEncoder`/`TextDecoder` only, so the browser stage can use the
  same code as Node tests.
- **`src/schema.json`**: a strict JSON Schema (draft 2020-12) for `FeedHeader`, hand-written to
  match `types.ts`. `additionalProperties: false` on every fixed-shape object and enums on every
  closed set of string values (`status`, `game.mode`, `milestone` fields, event `kind`, reward
  `kind`, attachment `kind`). The one deliberate exception is `rates`, which is genuinely an open
  string-keyed map per the contract doc (`Record<string, number>`) — see the comment in the
  schema for why it is not locked down the same way.
- **`src/chat.ts`**: `sanitizeChatText` — the chat text rules from `docs/control-api.md`'s
  `POST /chat`, in the same order the Rust side applies them (NFC, no control characters, a
  letters/digits/space/punctuation allowlist that excludes emoji and combining marks, whitespace
  collapsing, a 200-code-point cap, and a URL refusal). `classifyChatText` returns the rejection
  reason, which is the label on flysim's `fly_chat_rejected_total{reason}`.
  `tests/fixtures/chat-cases.json` is the hostile-input corpus **both** this package's tests and
  `services/flysim/crates/flysim/tests/chat.rs` load, so the two implementations cannot drift.
- **`src/names.ts`**: `validateDisplayName`, the display-name chokepoint (see the bridge README).
- **`src/fixture.ts`**: the `.flyfeed` fixture container — a recorded run of the feed
  (`"FLYFEED\0"`, a version, a JSON manifest, then the exact wire messages length-prefixed).
  `encodeFlyfeed`/`decodeFlyfeed` for whole files, `encodeFlyfeedHeader`/`encodeFlyfeedRecord`
  for a streaming recorder, and `iterateFlyfeedRecords` yielding zero-copy views. Pure
  `Uint8Array` functions, so `apps/stage/tools/record-fixture.mts` (Node) writes the files the
  stage's player mode (browser) reads. A `.flyfeed.gz` is this format gzipped; the caller
  inflates and `isGzip` sniffs which it has. The manifest's `attachmentPolicy` records what the
  recorder kept per attachment kind, because a full-rate 120 s recording is hundreds of MB.
- **`src/fake/`**: a self-contained synthetic `flysim` (`simulator.ts`, the state machine; `prng.ts`,
  a small seeded PRNG; `server.ts`, the WebSocket + HTTP wiring and CLI). Deterministic given a
  seed, and has no dependency on `@flybrain/brain` or the real dataset.

## Running the fake server

```sh
npx tsx packages/feed/src/fake/server.ts \
  [--feed-port 7400] [--control-port 7401] \
  [--scenario boot|running|stuck|milestone] \
  [--allow-reward] [--seed 12345] [--no-chat] [--no-chatter] \
  [--spikes-per-tick 30000]
```

- **`boot`**: stays in `booting` status forever — useful for testing the boot screen.
- **`running`** (default): skips the boot wait so the feed is immediately `running`.
- **`stuck`**: milestone rank fixed, `sinceSeconds` climbs without bound, `attempts` increments
  periodically — exercises the stuck-o-meter.
- **`milestone`**: the milestone rank steps up the ladder every 90 s. The ladder is 38 rungs, the
  same length as the real Pokémon Red one (`docs/design/ladder.md`), so `milestone.total` is 38 and
  `rank` runs 0..37. The wording differs from the real ladder's on purpose: the stage draws the
  current rung's label from the feed and the others from its own per-game config, and the two
  differing is what proves which one is authoritative.

All four scenarios wander `command_0..7` and the other tracked rates, hold a dominant D-pad
direction for ~400 ms at a time with occasional A/B pulses, emit reward events roughly every 20 s,
auto-checkpoint every 5 s, and stream a procedurally drawn 160x144 Game Boy-palette test frame (no
Nintendo assets), a quiet 220 Hz sine at 48 kHz stereo `f32`, and a spikes bitset with ~30,000 bits
set per snapshot by default — matching the live service's measured rate, not a placeholder — at
30 Hz while running and 2 Hz otherwise. Override with `--spikes-per-tick` (or `spikesPerTick` on
`startFakeServer`/`FakeFlysim`) to test against a different density.

It also implements the control API in full: `GET /status`, `POST /stimulate` (6/min global limit,
no overlap with an active pulse, emits a `sugar` `FeedEvent`), `POST /reward` (403 unless
`--allow-reward`), `POST /chat` (name validation, `sanitizeChatText`, 1 per 2 s per name and 5 per
s globally, 403 with `--no-chat`), `POST /checkpoint`, `POST /pause`/`POST /resume`,
`GET /events?since=&limit=`, `GET /healthz`.

The fake also scripts plausible viewer chatter into the header's `chat` ring every 8 to 22
simulated seconds, including the bridge's own `bot: true` template replies, so the stage's
persistent CHAT panel has something to render before either real service exists. `--no-chatter`
leaves the ring to whatever posts to `/chat`; `--no-chat` is the kill switch, and makes the header
omit `chat` entirely.

`startFakeServer(options)` (exported from the `./fake` subpath) starts the same thing
in-process on ports you choose — pass `feedPort: 0` / `controlPort: 0` for ephemeral ports — which
is how the integration tests drive it without spawning a child process.

## How `flystage` and `flybridge` consume this

- **`flystage`** imports `@flybrain/feed` for the types and `decodeSnapshot`, opens the feed
  WebSocket, sends `{"protocol":1,"client":"stage","wants":["frame","audio","spikes"]}`, and
  renders each decoded `FeedHeader` plus attachments. Against `@flybrain/feed/fake`'s fake server
  during development, before `flysim` exists.
- **`flybridge`** imports the control API request/response types, calls `POST /stimulate` on chat
  commands and Channel Points redemptions, and reads `GET /status`/`GET /events` for its own
  state. It asks for no feed attachments (`wants: []`) since it never renders anything.

## Scripts

```sh
npm test         # node --import tsx --test tests/**/*.test.ts
npm run typecheck
```
