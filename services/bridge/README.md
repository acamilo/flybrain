# @flybrain/bridge (flybridge)

The Twitch integration service for the 24/7 fly stream: chat commands, rate limiting, template-only
replies, Channel Points "Sugar" redemptions, and `/health` + `/metrics`. Talks to `flysim`'s
localhost control API (`docs/control-api.md`) and to Twitch via `@twurple/*` over EventSub
WebSocket. Never touches the game: there is deliberately no button endpoint on the control API,
and this service never tries to add one.

Milestone **B1** (this package, as it stands): everything is testable offline against a fake sim
and fake Twitch clients. There are no real Twitch credentials yet — `tools/authorize.mts` is
written and ready for the day there are.

Binding design doc: [`docs/design/stage-bridge.md`](../../docs/design/stage-bridge.md) section B.
Binding contracts: [`docs/control-api.md`](../../docs/control-api.md) and
[`docs/feed-protocol.md`](../../docs/feed-protocol.md).

## What is in here

| File | What |
| --- | --- |
| `src/config.ts` | Env-driven config, validated at startup (`loadConfig`, throws `ConfigError` listing every problem). |
| `src/names.ts` | Re-exports `validateDisplayName` from `@flybrain/feed` — see "Display names" below. |
| `src/templates.ts` | Every outbound chat string as a typed constant (`renderTemplate`, `TEMPLATE_IDS`). |
| `src/duration.ts` | `formatDuration` for the `!stuck` reply. |
| `src/ratelimit.ts` | Token-bucket rate limiting (`RateLimiter`), clock-injected for deterministic tests. |
| `src/sim.ts` | Typed control API client (`HttpSimClient`/`SimClient`), timeouts, typed 429/403 handling. |
| `src/commands.ts` | `!fly` `!brain` `!how` `!stuck` `!sugar` dispatch. Twurple-free. |
| `src/chat.ts` | `ChatSender` interface, the `send()` template gate, `TwurpleChatSender`. |
| `src/scopes.ts` | The B1 scope table and startup scope assertion. |
| `src/auth.ts` | Loads `tokens.json`, builds one `RefreshingAuthProvider` per role plus the scope-routing provider for the one-account case, runs the scope assertion. |
| `src/onscreen-chat.ts` | Forwards AutoMod-passed chat to flysim's `POST /chat` for the on-screen CHAT panel. Twurple-free. |
| `src/eventsub.ts` | EventSub WS wiring: chat, follow, raid, redemption-add; one socket when both roles are one account. Twurple-facing glue. |
| `src/redemptions.ts` | Sugar reward creation, redemption state machine, persisted intent log. Behind `FEATURE_REDEMPTIONS`. |
| `src/predictions.ts` | Disabled stub. Behind `FEATURE_PREDICTIONS` (Affiliate-only; not on for either demo yet). |
| `src/explainer.ts` | Rotating explainer poster with the chat-silence rule. |
| `src/health.ts` | `node:http` `/health` (JSON) and `/metrics` (Prometheus text). |
| `src/subscription-health.ts` | The self-healing watchdog: exits non-zero when the chat subscription stays unconfirmed. Twurple-free. |
| `src/notice.ts` | Startup/recovery notice gate, rate-limited across restarts via a small state file. |
| `src/atomic-file.ts` | Atomic, 0600 JSON writes shared by `tokens.json` and the redemption intent log. |
| `src/index.ts` | Wires everything, posts the startup notice, graceful shutdown on SIGINT/SIGTERM. |
| `tools/authorize.mts` | One-time interactive OAuth authorization-code flow producing `tokens.json`. |
| `tools/mock-twitch.sh` | Documented Twitch CLI commands for manually exercising each handler. |

## Display names: the chokepoint

`validateDisplayName` (`^[\p{L}\p{N}_]{1,25}$`, else the literal `"a viewer"`) lives in
[`packages/feed/src/names.ts`](../../packages/feed/src/names.ts) so flybridge and flystage
validate viewer names identically — see `docs/design/stage-bridge.md` section C, "How viewer
names reach the ticker." `src/names.ts` here is a thin re-export so bridge code imports it like
every other local module. Every viewer name that reaches `sim.stimulate()` (`!sugar`, Sugar
redemptions) or a chat template (follow/raid thanks) goes through this function first — never raw
chat or raw Twitch profile data.

## Template-only chat

Every outbound chat string is a constant in `src/templates.ts`, keyed by `TemplateId`. `send()`
(`src/chat.ts`) only accepts a `TemplateId` plus a typed params object — never a plain string, and
never anything built from chat message text. `tests/templates.test.ts` includes a static-analysis
test asserting every `send(...)` call site in `src/` passes a string literal from `TEMPLATE_IDS`,
not a variable or template literal.

Game-specific words (the title, the channel) are never hardcoded in a template: they arrive as
params sourced from `BridgeConfig`, so the same code runs both demo channels.

## On-screen chat

The stream's right rail ends in a persistent CHAT panel (`docs/stream-mvp-plan.md`, "Rail layout
v2": last ~7 lines, AutoMod-passed, service-sanitized, deny-listed, kill switch). This service is
the middle of that path:

```
Twitch -> AutoMod -> flybridge (src/onscreen-chat.ts) -> POST /chat -> flysim (sanitize, deny
                                                                       list, ring) -> feed -> page
```

**AutoMod comes first and nothing here replaces it.** `channel.chat.message` fires only for
messages that were delivered to chat: a message AutoMod held produces **no event at all**, so it
never reaches this service, and a message a moderator deletes afterwards produces a
`channel.chat.message_delete` we do not subscribe to. Everything below is what the bridge adds on
top of the channel's own moderation settings.

What `OnscreenChat` does with each message it sees:

| Case | Outcome |
| --- | --- |
| Starts with `!` | dropped (`command`) — it is an instruction to this bot, not chatter, and `!sugar` should not appear on the broadcast twice |
| From a known third-party bot (`KNOWN_BOT_LOGINS`, plus `BOT_USER`) | dropped (`bot`) |
| Display name fails `validateDisplayName` | dropped (`invalid_name`) — attributing a line to the literal `"a viewer"` would be putting words in a fiction's mouth |
| `sanitizeChatText` refuses it | dropped (`rejected`) — never trimmed, never partially cleaned |
| An echo of a reply this bridge just posted | dropped (`echo`), so nothing appears twice |
| Anything else | `POST /chat` (`sent`) |

The bridge's own template replies go on screen too, with `bot: true`: `wrapSendWithOnscreenEcho`
wraps `createSend` once in `src/index.ts`, so every reply that reaches Twitch also reaches the
panel, and no call site has to know. A reply longer than the sanitizer's 200-character cap is
shortened **for the panel only** (`shortenForPanel`, at a word boundary with an ellipsis); the
Twitch message goes out in full first. That trimming applies exclusively to text this service
wrote itself — a viewer's line over the cap is refused whole.

None of this is authoritative. flysim re-runs every rule, applies the operator deny list and
enforces its own rate limits (1 per name per 2 s, 5 per second globally) regardless of what this
service does — `docs/control-api.md` is explicit that the limits hold "regardless of what the
bridge does". The bridge's copy is the cheap first pass, so hostile text never leaves the process.

`FEATURE_ONSCREEN_CHAT=false` turns the forwarding off here; `[chat] enabled = false` in
`flysim.toml` (`FLY_CHAT_ENABLED=0`) turns the panel off at the source and makes `POST /chat`
answer 403. Either end is a working kill switch, and `flybridge_onscreen_chat_total{outcome}`
counts every decision above.

Why the paranoia: `docs/stream-mvp-plan.md` records the Nothing, Forever precedent — a 14-day ban
for generated text on stream — and the rule the research note draws from it, never render raw chat
into the frame. `tests/onscreen-chat.test.ts` ends with a static lint asserting that this module is
the *only* place in `src/` that reads a message body for anything but `parseCommand`, the only
place that calls `sanitizeChatText`, and the only place that calls `POST /chat`, and that the text
it posts is always the sanitizer's own output.

## Quiet mode

`FEATURE_QUIET=1` makes the bridge speak only when spoken to (the operator, 2026-09-17: "make bot less
chatty. speaks only when spoken to. doesn't greet."). Nothing self-initiated goes to chat:

| Off (default) | With `FEATURE_QUIET=1` |
| --- | --- |
| Startup / recovery notice on every allowed start (`src/notice.ts`) | none — `NoticeLog.decide()` authorises nothing and the state file is not written |
| Explainer rotation every `EXPLAINER_INTERVAL_MS` (`src/explainer.ts`) | none — no interval timer is armed |
| `Thanks for the follow` / `Thanks for the raid` (`src/eventsub.ts`) | none |

Unchanged either way: `!fly` `!brain` `!how` `!stuck` `!sugar`, the Sugar redemption replies, and
forwarding viewer chat to the on-screen CHAT panel. The `channel.follow` and `channel.raid`
subscriptions are still created, so `/health` reports the same subscription list and
`flybridge_follows_total` / `flybridge_raids_total` still count every one — only the chat line is
dropped.

`EXPLAINER_INTERVAL_MS=0` is the narrow version of the same idea: it turns off just the rotation,
on a channel that still greets and still posts notices. The two switches are independent.

## Scopes (B1 table)

Startup calls `getTokenInfo` on both the bot and broadcaster tokens and refuses to start if any
required scope is missing, naming exactly which ones (`src/scopes.ts`, `assertRequiredScopes`).

| Scope | Token | Needed for | Required |
| --- | --- | --- | --- |
| `user:read:chat` | bot | `channel.chat.message` over EventSub WS | always |
| `user:write:chat` | bot | Send Chat Message | always |
| `user:bot` | bot | send as bot | always |
| `channel:bot` | broadcaster | authorize the bot on the channel | always |
| `moderator:read:followers` | broadcaster | `channel.follow` v2 | always |
| `channel:read:redemptions` | broadcaster | `channel.channel_points_custom_reward_redemption.add` | `FEATURE_REDEMPTIONS=true` |
| `channel:manage:redemptions` | broadcaster | create the Sugar reward, fulfil/refund | `FEATURE_REDEMPTIONS=true` |
| `channel:manage:broadcast` | broadcaster | stream markers (future work) | `FEATURE_REDEMPTIONS=true` |
| `channel:manage:predictions` | broadcaster | predictions (Affiliate-gated, disabled in B1) | `FEATURE_PREDICTIONS=true` |

`channel.raid` needs no scope, just a user token and a `to_broadcaster_user_id` condition.

## Environment reference

Required, no default:

| Var | Notes |
| --- | --- |
| `TWITCH_CLIENT_ID` / `TWITCH_CLIENT_SECRET` | Or a `CREDENTIALS_DIRECTORY` with `twitch-client-id` / `twitch-client-secret` files (systemd `LoadCredential=`/`LoadCredentialEncrypted=`). |
| `CHANNEL` | Broadcaster channel login, no leading `#`. |
| `BOT_USER` | Bot account login. |
| `GAME_TITLE` | Substituted into templates — the only game-specific word in config. |

Optional, with defaults:

| Var | Default |
| --- | --- |
| `TOKENS_FILE` | `/var/lib/flybridge/tokens.json` |
| `REDEMPTION_STATE_FILE` | `/var/lib/flybridge/redemption-state.json` |
| `NOTICE_STATE_FILE` | `/var/lib/flybridge/notice-state.json` |
| `EVENTSUB_GRACE_MS` | `60000` (chat subscription unconfirmed this long ⇒ log one line and exit 75) |
| `NOTICE_MIN_INTERVAL_MS` | `600000` (at most one startup/recovery notice per 10 min, across restarts) |
| `SIM_CONTROL_URL` | `http://127.0.0.1:7401` |
| `SIM_TIMEOUT_MS` | `2000` |
| `FEATURE_REDEMPTIONS` | `false` |
| `FEATURE_PREDICTIONS` | `false` |
| `FEATURE_ONSCREEN_CHAT` | `true` (the only flag that defaults on — see "On-screen chat") |
| `FEATURE_QUIET` | `false` (speaks only when spoken to — see "Quiet mode") |
| `HEALTH_HOST` | `127.0.0.1` |
| `HEALTH_PORT` | `7410` |
| `EXPLAINER_INTERVAL_MS` | `1200000` (20 min; `0` turns the rotation off) |
| `EXPLAINER_SILENCE_MS` | `1200000` (20 min) |
| `RATE_LIMIT_PER_USER_PER_COMMAND_MS` | `60000` |
| `RATE_LIMIT_GLOBAL_REPLY_MS` | `5000` |
| `RATE_LIMIT_CHAT_MESSAGES_PER_30S` | `15` |
| `RATE_LIMIT_SUGAR_COOLDOWN_MS` | `10000` |

Two demos run this same service with different env (`CHANNEL`, `BOT_USER`, `GAME_TITLE`,
`HEALTH_PORT` if colocated) — no game-specific word belongs in code.

## Running against the fake sim

```sh
# terminal 1: a fake flysim on the real default ports
npx tsx packages/feed/src/fake/server.ts --scenario running

# terminal 2: flybridge, pointed at it, with dev-mode env credentials
TWITCH_CLIENT_ID=devclient TWITCH_CLIENT_SECRET=devsecret \
CHANNEL=flyplayspokemon BOT_USER=flybridgebot GAME_TITLE="Pokemon Red" \
TOKENS_FILE=/tmp/flybridge-tokens.json \
npx tsx services/bridge/src/index.ts
```

`src/index.ts` needs `tokens.json` to exist (run `tools/authorize.mts` first, against a real
Twitch app, once one exists) — there is currently no way to run `index.ts` end-to-end without
real Twitch credentials, which is expected for B1. Everything below `index.ts` (commands,
rate limits, templates, the sim client, the redemption state machine, scope assertion, the
explainer's silence rule) is exercised directly in `npm test` with `tests/fake-sim.ts` (a
fully scriptable 429/500/timeout control API fake) and fakes for `ChatSender`/`RedemptionApi`, so
none of it needs Twitch or a browser to verify.

## Twitch CLI mocks

`tools/mock-twitch.sh` documents the exact `twitch event trigger ...` commands for each handler
(chat commands, follow, raid, Sugar redemption, subscription revocation) against
`twitch event websocket start-server`. Reference only — `npm test` never requires the Twitch CLI.

## Authorizing (once a real Twitch app exists)

```sh
TWITCH_CLIENT_ID=... TWITCH_CLIENT_SECRET=... \
  npx tsx tools/authorize.mts --role bot --tokens-file /var/lib/flybridge/tokens.json

TWITCH_CLIENT_ID=... TWITCH_CLIENT_SECRET=... \
  npx tsx tools/authorize.mts --role broadcaster --tokens-file /var/lib/flybridge/tokens.json \
  --with-redemptions --with-predictions
```

Opens a one-time authorization-code flow on `http://localhost:3000/callback`. Run once per
identity (bot and broadcaster are different Twitch accounts). Writes `tokens.json` atomically at
mode `0600`. `--with-redemptions`/`--with-predictions` add those features' scopes up front even
though B1 ships with both off, so the token does not need re-authorizing the day they switch on.

The device-code grant is the documented fallback for headless authorization; twurple's first-class
support for it is unverified as of this writing, so it is intentionally not implemented here — do
the one-time flow from a machine with a browser instead.

## systemd expectations

The unit itself lives in `infra/`, not here (W4 in `docs/stream-mvp-plan.md`). What it needs to
provide:

- `LoadCredentialEncrypted=twitch-client-id:...` and `...twitch-client-secret:...`, sourced from
  `pass` (`twitch/fly-pokemon-client-{id,secret}`), giving `$CREDENTIALS_DIRECTORY` the two files
  `src/config.ts` reads.
- A writable, persistent directory for `TOKENS_FILE`, `REDEMPTION_STATE_FILE` and
  `NOTICE_STATE_FILE` (default `/var/lib/flybridge/`), owned by the service user.
- `After=`/`Requires=` on the `flysim` unit (this service calls `SIM_CONTROL_URL` on startup and
  continuously; it degrades gracefully — commands that need the sim go quiet, `/health` reports
  `simReachable: false` — but it is not useful without it).
- `Restart=always` with `RestartSec` of at least 15 s and NO start limit
  (`StartLimitIntervalSec=0`). This is load-bearing, not hygiene: the bridge exits itself with
  code 75 when its `channel.chat.message` subscription has been unconfirmed for
  `EVENTSUB_GRACE_MS`, because that is the only way to get a fresh EventSub websocket transport
  (`src/subscription-health.ts`). 15 s is how long Twitch needs to reap the stale transports; a
  start limit would park the unit during exactly the outage it exists for.
- `/health` and `/healthz`-style checks are cheap enough to poll frequently. `chatSubscriptionHealthy`
  is the field that answers "is chat actually working" — `eventSubConnected` was `true` throughout
  the hour of dead chat on 2026-09-16.

## Testing

```sh
npm test --workspace @flybrain/bridge
npm run typecheck --workspace @flybrain/bridge
```

156 tests. `tests/fake-sim.ts` is a small, fully scriptable fake of the control API (429/500/
timeout on demand) — not `packages/feed`'s fake simulator, which is built for realistic feed data
rather than fault injection, though it is used indirectly wherever a test only needs a real
`/status`/`/stimulate` happy path. `src/eventsub.ts` and `src/chat.ts`'s twurple-backed pieces
(`TwurpleChatSender`, `TwurpleRedemptionApi`) are exercised manually via `tools/mock-twitch.sh`
rather than unit-tested — they are thin glue over small interfaces (`ChatSender`, `RedemptionApi`,
`SimClient`), and everything they call into *is* unit-tested with fakes.

`src/eventsub.ts` is the exception to that last sentence now: `tests/eventsub.test.ts` drives it
through its `createListener` seam with a hand-written `EventSubListenerLike`, which is how "one
websocket when both roles are one Twitch account, two when they are not" and the self-healing
grace period are asserted without a Twitch connection. `tests/selfheal-exit.test.ts` spawns a real
child process (`tests/fixtures/selfheal-exit.ts`) to prove the exit code systemd sees is non-zero.
