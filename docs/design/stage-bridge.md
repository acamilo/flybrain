> Design document produced 2026-09-15 by a planning agent. Binding contracts are ../feed-protocol.md and ../control-api.md; where this document differs, the contracts win.

# flystage + flybridge implementation plan

Binding context read: `the operator's own planning notes` (decisions 1-10), `docs/streaming-plan.md` (capture, flags, CPU budget), current page (`~/fly-plays-pokemon/src/main.ts`, `src/style.css`, `src/view/connectome.ts`), the live screenshot, and both memory files.

## 0. Two corrections to carry forward before anything is built

1. `docs/streaming-plan.md` section 2(a) lists `--mute-audio` in the Chromium flag set. Decision 8 makes the page the audio source, so that flag must be removed and `--autoplay-policy=no-user-gesture-required` kept. Also drop `--enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader`: decision 3 moves the map to 2D canvas, so no WebGL context should exist at all, and an accidental SwiftShader context is a silent 1-2 core regression.
2. Taste reconciliation. The operator's own UI-taste notes is about tools the operator drives (compact Fluent chrome, 14 px type, quiet greys). This is a 3000 kbps broadcast graphic read at 0.31 scale on phones, so the research theme wins on merits. What carries over from the taste memory because it is objectively right here: grouped compact panels rather than airy centred columns, one accent colour, 8 px radius, no monospace-everything, no hairline 1 px borders (x264 erases them, see the audit). What is explicitly overridden: the 14 px type scale and the neutral-grey-only palette.

---

# A. flystage (`~/flybrain/apps/stage`)

## A1. Stack and authoring resolution

Vite 7 + React 19 + TypeScript 5.9 + Tailwind v4 (CSS-first `@theme`, no `tailwind.config.js`) + shadcn/ui via `components.json`. Workspace member of `flybrain` (add `apps/*` to the root `workspaces` array). Respect the existing 30-day package cooling-off gate by copying `tools/check_package_age.mjs` from `fly-plays-pokemon` into the root and running it in CI.

**One authoring resolution, not two.** Author everything in 1280x720 CSS pixels inside `#stage { width:1280px; height:720px; }`. The 1920x1080 option is `transform: scale(1.5); transform-origin: top left` on `#stage` plus `window.devicePixelRatio`-style backing-store multipliers of 1.5 on the three canvases. This is exact: the game is 4x in CSS, 6x on screen, still integer, still jitter-free with `image-rendering: pixelated`, and every text floor scales automatically (18 -> 27, 48 -> 72). Mode selected by `?res=720|1080`. This removes a whole class of dual-layout bugs and satisfies the 1080p requirement in one line.

Safe insets: `--inset: 32px` at 720p (48 after the 1.5 scale). Background art may bleed past it, nothing load-bearing may. Bottom-left 320x120 is declared a no-content zone in the grid (Twitch chat/extension overlap) and holds only the tiled background.

## A2. Layout B geometry at 1280x720 (fits exactly, verified arithmetically)

Usable box after insets: 1216 x 656.

| Region | Size | Content |
|---|---|---|
| Title strip | 1216 x 40 | "A FLY BRAIN IS PLAYING POKEMON RED" (26 px) + "139,255 real neurons, FlyWire FAFB v783. Day N." (18 px) |
| Left column | 640 x 612 | game 640x576 (4x), gap 4, button row 640x32 |
| Right rail | 568 x 612 | four stacked panels, 4 px gutters |
| Gap | 8 px | between columns |

40 + 4 + 576 + 4 + 32 = 656. Exact.

Right rail rows (568 wide):
- **Row 1, 568 x 228, "WHAT THE FLY SEES" + "NAMED CIRCUITS".** Left cell 240 x 216 retina raster (two eye rasters side by side, 786 L1 columns each). Right cell 320 wide, 6 labelled circuit groups at 34 px pitch.
- **Row 2, 568 x 132, "WHAT JUST HAPPENED".** Reward ticker, 3 rows at 40 px, 22 px text, minimum dwell 4 s per item.
- **Row 3, 568 x 136.** Milestone ladder (16-step spine, current rank name at 26 px) + stuck-o-meter beside it.
- **Row 4, 568 x 104.** Run clock / uptime / day N (hero number 48 px) + sugar queue with cooldown ring + honesty/explainer card, rotating.
- **Brain map inset** floats bottom-right of the rail at 240 x 168 over row 4's right third, with an opaque backing plate behind any text that crosses it.

**Hero promotion.** On a "big event" (badge, Pokedex first catch, milestone rank increase, sugar redemption), a `MomentOverlay` animates the brain map to 912 x 684 over the left column for 9 s with a full-width caption band, then returns. Implemented as a CSS transform on the inset's container plus a z-index raise; the canvas backing store is always allocated at hero size and downscaled by `drawImage` for the inset, so promotion costs no reallocation.

## A3. Panel inventory and the label mapping

`src/lib/labels.ts` is the single source of human copy. Role mapping (roles confirmed present in `data/fafb-v783/meta.json`):

| Feed role | Neurons | On-screen group | On-screen label |
|---|---|---|---|
| `command_0..3` | 151/174/171/141 | MOTOR DRIVE (4 sub-bars) | "motor drive: up / down / left / right" |
| `command_4`, `command_5` | 163, 161 | PRESS A, PRESS B | "press A", "press B" |
| `command_6`, `command_7` | 149, 195 | MENU | "open menu (Start / Select)" |
| `reward_pam` | 307 | DOPAMINE | "dopamine (PAM cluster): the fly's reward signal" |
| `proboscis` | 24 | TASTE | "taste / proboscis: what sugar touches" |
| `forward`, `backward`, `steer_left`, `steer_right` | 2/4/2/2 | LEGS | "walking circuit (real, but not wired to buttons)" |
| `populationRate` | all | WHOLE BRAIN | hero Hz number, 48 px |

The LEGS row is deliberate honesty content: those roles have 2-4 neurons and drive nothing. Saying so on screen is exactly the Gemini-Plays-Pokemon transparency lesson.

Bars are `transform: scaleX()` with a 120 ms transition so they animate on the compositor and never touch layout. Peak-hold tick decays over 600 ms so a spike is visible after the value drops.

**Button indicators (fixes audit WEB-03).** 8 cells of 76 x 32. On rising edge, add a class; remove it 250 ms after the *falling* edge. The 85 ms A/B pulse (from `gameboyDecoderConfig`, `holdMs: 85`) therefore lights for at least 335 ms. Afterglow is `opacity` + `background-color` transition only.

**Retina raster.** Load `visual-xy.binz` (1572 x 2 f32), `visual-hemisphere.binz`, `visual-indices.binz` via `loadCompressed` from `@flybrain/brain/browser`. Compute drive with `projectFrame` from `@flybrain/brain` on the decoded RGBA frame, exactly the kernel the sim uses, so the panel is not a lookalike. Precompute panel pixel coordinates once; per frame write 1572 dots into a small ImageData.

**Reward ticker.** Human-paced queue, not a log. `minDwellMs: 4000`, `maxQueue: 12`, dedupe identical `exploration` events within 20 s into "+3 new places". Value tiers drive presentation: `badge` (3.0) and `species` (0.5) get the full-width moment; `exploration` (0.05) gets a single quiet line. This fixes the audit finding that a badge and a +0.05 explore tick got identical treatment.

**Milestone ladder.** 16 ranks from the ratchet (`~/fly-plays-pokemon/src/reward/pokemon-red.ts:126-134` is the authority): 0 booting, 1 bedroom, 2 downstairs, 3 outside, 4 Oak's lab, 5 got a starter, 6 Oak's parcel, 7 Pokedex, 8-15 badges 1-8 (Boulder, Cascade, Thunder, Rainbow, Soul, Marsh, Volcano, Earth). Rendered as a vertical spine with reached/current/future states, current rank name at 26 px.

**Stuck-o-meter.** `milestone.stuckSeconds` as a filling arc plus "stuck here for 3 h 41 m, attempt 2 of 3" from `milestone.attempts`. Research ranks this highest value per hour and cheapest. Cross a 6 h threshold and it earns its own colour and a "this is the interesting part" caption.

**Honesty panel.** Rotating on a schedule in `src/lib/schedule.ts`: a permanent two-column "WHAT IS REAL / WHAT IS SCAFFOLDING" state every 4 minutes for 60 s, and 8 explainer cards in between. Never free text; all strings are constants in `labels.ts`.

**Sugar queue.** Shows last redeemer name, a countdown cooldown ring, queue depth, and the fixed one-line honesty statement ("a sugar pulse stimulates the dopamine cluster for 400 ms; it does not press buttons"). Names arrive only through `events[]` (see section C).

## A4. Typography

Press Start 2P survives only as game-adjacent accent: the title-strip wordmark, the 8 button glyph labels, and the milestone rank name. Everything else moves to a real UI face.

Proposal: **Inter Variable 4.x (SIL OFL 1.1)** for all UI text with `font-variation-settings: 'opsz' 24` and `font-feature-settings: 'tnum','cv05'`, plus **IBM Plex Mono (OFL 1.1)** for hero numerals and the Hz/clock readouts, where fixed-advance digits stop the layout jittering 30 times a second. Inter's tall x-height and open apertures survive both the 3000 kbps encode and the 0.31 downscale; Plex Mono's numerals stay distinct at 14 px effective. Alternate if Inter reads too neutral in the mockups: **Archivo** (OFL) for labels, which is a touch more instrument-like.

Self-host woff2 in `public/fonts`, subset to Latin + the exact glyph set, `font-display: block` with a 2 s block period, and a `@font-face` fallback with `size-adjust` so the pre-swap metrics match. The page sets `data-ready="1"` on `<html>` only after `await document.fonts.ready` plus the brain base bitmap arriving; `flycast` and every Playwright test wait on that attribute. This is the fix for the font-loading-before-first-paint risk, and it matters more here than in a normal app because the first paint is broadcast.

Floors enforced by lint: body/ticker >= 18, labels 22-26, hero >= 48 (all at 720p authoring).

## A5. Brain map on 2D canvas: the throughput design

The naive design (one glow sprite per spiked neuron) does not survive arithmetic. At a plausible 2-4 Hz mean population rate over 139,255 neurons, each 33 ms snapshot carries **9,000 to 18,000 spike indices** (36-72 KB, fine on localhost). 14,000 `drawImage` calls at 2-6 us each is 30-80 ms per frame. Dead.

Two-tier design instead:

1. **Static base, once.** `src/workers/brain-base.worker.ts` fetches `positions.binz` and `viewer-edges.binz` with `loadCompressed`, calls `normalizePositions` and `classifyByRoles` from `@flybrain/brain/view` (both are pure and already unit-tested in `packages/brain/tests/view.test.ts`), rasterises 139,255 points **and** the faint edge scatter additively into a `Uint32Array` pixel buffer at hero size 912 x 684, `putImageData` into an `OffscreenCanvas`, then `createImageBitmap` and transfer the bitmap to the main thread. Direct pixel writes cost roughly 139k x ~15 ns = 2-3 ms, versus 15-30 ms for 139k `fillRect` calls. Off the main thread so first paint is never blocked.
2. **Per-frame density accumulator.** The same worker emits a `Uint16Array(139255)` LUT mapping neuron index to a cell in a 304 x 228 grid (hero size / 3). Per snapshot: `for each spike index: accum[lut[i]] += 1`, which is 14,000 integer increments, about 0.05 ms. Then decay `accum *= exp(-dt/110)`, expand into a 304 x 228 ImageData with the role colour ramp, `putImageData` to a scratch canvas, and `drawImage` it scaled up with `globalCompositeOperation: 'lighter'` over the base.
3. **Sprites only for the top N cells.** Sort-free selection of cells above a threshold, capped at **256 sprites per frame** (`MAX_SPRITES`, config), drawn from a pre-rendered 16 x 16 radial-gradient sprite. This keeps the "individual neurons flaring" read without the per-neuron cost.

No WebGL, no `filter: blur()`, no `box-shadow` on animated elements: all three are brutal in Chromium's software rasteriser.

**Frame budget estimate, 8 vCPU VM, E5-2660 v3 at 2.6 GHz (Haswell, no GPU):**

| Work | Rate | Estimate |
|---|---|---|
| Base bitmap raster + LUT build | once | 40-120 ms in a worker |
| WS decode + JSON parse of header | 30 Hz | 0.15-0.4 ms |
| Game: 160x144 ImageData -> offscreen -> `drawImage` 4x | 30 Hz | 0.8-2.0 ms |
| Retina: `projectFrame` 1572 cols + 1572 dot writes | 30 Hz | 0.1-0.3 ms |
| Brain map: accumulate + decay + expand + 2 blits + 256 sprites | 30 Hz | 2.5-6.0 ms |
| DOM text commit (throttled to 4 Hz) | 4 Hz | 1-4 ms on commit frames |
| CSS transform/opacity animations | 60 Hz | compositor, near zero |
| **Total on a data frame** | | **3.5-9 ms of a 33 ms budget** |

Comfortable margin, so the design is expected to fit. Repaint policy: one `requestAnimationFrame` loop at 60 Hz that only repaints surfaces marked dirty. Game and retina redraw on new frames only (30 Hz). Brain map redraws at 30 Hz (110 ms decay does not need 60). React re-renders are throttled to 4 Hz for numbers and event-driven for the ticker and moments.

### Phase 0 measurements to take (before panels are wired)

1. Base raster wall time and peak worker RSS, on the host, in Chromium on Xvfb.
2. Actual spike-index count per snapshot at steady state from flysim, p50 and p99. If p99 exceeds 30,000, the sim should pre-decimate.
3. `performance.now()` deltas around each paint stage, logged as a histogram to the console for one hour, plus dropped-frame count from `requestAnimationFrame` deltas.
4. Chromium renderer + GPU-process CPU percent via `pidstat`, with and without the brain map, to isolate its cost.
5. Confirm no GL context exists: `chrome://gpu` and a `canvas.getContext('webgl')` assertion that returns null in production config.
6. Encoded-output legibility: 10-minute MediaMTX recording, frame-stepped, checking hairline survival and text edges at 3000 kbps.
7. `document.visibilityState` and rAF cadence over an hour (the streaming-plan's open unverified item).
8. Audio: Pulse sink underrun count over an hour, and measured A/V offset from a clap test (a visible flash plus an SFX on the same event).
9. RGBA versus indexed frame payload: measure decode cost both ways and decide whether to ask flysim for 2 bpp indices plus a palette (23 KB versus 92 KB per frame at 30 Hz).

## A6. Feed transport and store

Single binary WebSocket message per snapshot, so frame, audio and spikes are atomic: 8-byte magic/version, u32 header length, UTF-8 JSON header, then one contiguous attachment blob. The header carries `attachments: [{kind, offset, length, ...meta}]` with `kind` in `frame | audio | spikes`, and `frame.format` in `rgba8 | indexed2`, `audio.format` in `f32le | s16le` with `sampleRate` and `channels`. Encoder and decoder both live in `packages/feed` (section C) so the Rust side has one normative spec to match and a fixture to byte-compare against.

`src/feed/store.ts`: zustand with two disjoint slices. A **hot ref store** (plain mutable object, no React subscription) holds the typed arrays, button mask, spike list and rates, read directly by the paint loop. A **cold zustand store** holds only what React renders, updated at 4 Hz by a coalescing scheduler, plus event-driven pushes for the ticker and moments. This is the decoupling requirement: 30 Hz ingest, 60 Hz rAF paint, 4 Hz React.

`src/feed/socket.ts` handles reconnect with backoff, a `STALE FEED` banner after 2 s of silence (honest, and better than the current page's frozen numbers), and a monotonic `frame` gap counter surfaced in the honesty panel.

## A7. Player mode (fixture feed)

`?mode=player&fixture=cold-open` swaps `socket.ts` for `src/feed/fixture.ts`, which replays a recorded `.flyfeed` file (length-prefixed concatenation of the exact wire messages) at 30 Hz with loop and seek, driven by `?t=` for deterministic screenshots. `tools/record-fixture.mts` records from a live flysim. Three fixtures to author: `cold-open` (boot, all-zero state, the case the audit says looks broken), `steady` (overworld exploring, sparse rewards), `big-moment` (badge + sugar redemption + rank change within 20 s, for the moment overlay and the SFX). Fixtures are the only way design and tests run with no service, and they are what the mockup gate renders.

## A8. Audio

`AudioWorkletNode` with a ring buffer fed by `port.postMessage` of transferred Float32Arrays, deliberately **not** SharedArrayBuffer, because SAB needs `crossOriginIsolated` (COOP/COEP) and 30 messages per second is free. Target fill 250 ms, high watermark 400 ms, low 120 ms.

Drift correction inside the worklet: a fractional read index with a varispeed correction clamped to +/-0.3 percent, servo'd on the fill level. Inaudible, and it absorbs the permanent clock difference between the emulator's 30 Hz chunk cadence and the AudioContext's hardware clock. Only if fill exceeds 2x target or hits zero does it hard-drop or hard-insert, with the incident counted and exposed in `/health`. Accept both `f32le` and `s16le` by branching at decode (`s16 -> f32` is one multiply).

SFX bank in `src/audio/sfx.ts`: 6 short OFL/CC0 samples (reward tick, badge, milestone, sugar, stuck-threshold, feed-stale), decoded once, mixed through their own `GainNode` so game audio and SFX have independent gains under a master gain. Config via `?gain=` and a JSON config file, defaults game 0.8 / sfx 0.5 / master 0.9.

Runtime requirements to encode in the `flystage` systemd unit: `--autoplay-policy=no-user-gesture-required`, **no** `--mute-audio`, `PULSE_SINK` pointing at the null sink, and a `/health` assertion that `audioContext.state === 'running'`.

## A9. Theme system and the mockup gate

`src/theme/tokens.css` defines the contract as CSS variables only (`--bg-0..2`, `--panel`, `--bezel`, `--ink-0..2`, `--accent`, `--accent-warm`, `--sensory`, `--motor`, `--dopamine`, `--radius`, `--u`). Three variants selected by `<html data-theme>`, all implementing the research recommendation (instrument base + CRT phosphor warmth + Game-Boy panel ergonomics, zero Nintendo assets):

- **T1 Instrument.** Near-black slate bases, amber phosphor accent, GB-style recessed bezels around each panel group, 2 px borders (not hairlines), texture only on panel bodies.
- **T2 Phosphor.** Warmer olive-charcoal, green phosphor primary with amber secondary, a 6 percent scanline on panel backgrounds only and never behind text, heavier bezels, slight bloom on the brain map.
- **T3 Field lab.** Lighter neutral panel cards on near-black, white ink, one cyan/amber pair, minimal texture, maximum legibility. This is the variant closest to the operator's recorded Fluent taste and exists so the gate is a real choice, not a rubber stamp.

**Gate (blocking, per the kickoff playbook).** `tools/mockup.mts`: Playwright against `vite preview`, fixture-fed, `?mode=player&fixture=steady&t=95s&theme=t1|t2|t3&res=720`, waits for `[data-ready="1"]`, screenshots 1280x720 at `deviceScaleFactor: 1` into `mockups/`. Plus one `big-moment` frame per theme so the operator sees the promoted brain map. Six PNGs, sent as images with no prose layout description. **No panel is wired to the live feed until the operator picks one.**

## A10. Tests

`tests/e2e/` with `@playwright/test`, viewport 1280x720, `deviceScaleFactor: 1`, fixture feed, `?t=` fixed, waiting on `[data-ready="1"]`:

1. **Screenshot tests.** One per fixture per breakpoint (720 and 1080), `toHaveScreenshot` with `maxDiffPixelRatio: 0.002`. Canvas content is deterministic because the fixture and the seek time are.
2. **Text-size lint.** Walk every element with a non-empty visible text node, read `getComputedStyle().fontSize`, multiply by the accumulated ancestor `scale`, fail below 18 px. Also assert elements tagged `data-role="label"` land in 22-26 and `data-role="hero"` at or above 48.
3. **Legibility check, dependency-free.** Take the 720p screenshot, then open a second page containing only `<img>` at `width: 397px` (0.31) and `width: 896px` (0.70) with `image-rendering: auto`, and screenshot that. This is a true downscale of the encoded-size frame, not a re-render at low DPI, which is the thing that actually matters. Emit both PNGs as review artifacts and assert programmatically that each `data-legible="critical"` region's RMS contrast stays above a calibrated floor at 0.31.
4. **Structural regressions from the audit.** No scrollbars (`scrollHeight <= clientHeight`), no `@media` rule applies at 1280x720, footer grid columns equal cell count, game canvas scale is exactly integer, zero WebGL contexts, no console errors.
5. **Behavioural.** A/B afterglow lasts >= 335 ms (drive an 85 ms pulse through the fixture, sample `getComputedStyle` over time); ticker dwell >= 4 s; moment overlay opens and closes; stale-feed banner appears 2 s after the fixture stops.
6. **Unit tests** (`node --test`, matching this repo's own style): label mapping completeness against `meta.json` roles, ticker queue pacing, rotation schedule, ring-buffer drift math, envelope decode round-trip.

---

# B. flybridge (`~/flybrain/services/bridge`)

Node 22, TypeScript, `tsx` for dev, `node --test` for tests. Deps: `@twurple/auth`, `@twurple/api`, `@twurple/eventsub-ws`, `@twurple/chat` only if the EventSub path proves insufficient. No web framework; one `node:http` server for `/health` and `/metrics`.

## B1. Auth and secrets

One-time interactive authorization in `tools/authorize.mts`, run by hand on the WSL box, producing a refresh token. Primary path: authorization-code flow with a temporary `http://localhost:3000/callback` listener (Twitch permits `http://localhost` redirects) and `exchangeCode` from `@twurple/auth`. Device-code grant against `https://id.twitch.tv/oauth2/device` is the fallback for headless authorization; twurple's first-class support for it is **unverified**, so if used it is 30 lines of hand-rolled polling rather than a library claim.

Runtime uses `RefreshingAuthProvider` with `onRefresh` persisting to `/var/lib/flybridge/tokens.json`, written `0600` by an atomic temp-file rename, owned by the service user, never in git (`.gitignore` plus a repo-level check). `TWITCH_CLIENT_ID` and `TWITCH_CLIENT_SECRET` arrive via systemd `LoadCredentialEncrypted=`, sourced from `pass` on the WSL box under `twitch/fly-pokemon-client-{id,secret}`, matching the stream-key posture already documented.

Two identities, both registered on the provider: the **bot account** (chat send) and the **broadcaster account** (redemptions, predictions, markers). Scopes to request, with the intent recorded per scope:

| Scope | Token | Needed for |
|---|---|---|
| `user:read:chat` | bot | `channel.chat.message` over EventSub WS |
| `user:write:chat` | bot | Send Chat Message |
| `user:bot` | bot | send as bot |
| `channel:bot` | broadcaster | authorize the bot on the channel |
| `channel:read:redemptions` | broadcaster | `channel.channel_points_custom_reward_redemption.add` |
| `channel:manage:redemptions` | broadcaster | create the Sugar reward, fulfil/refund |
| `moderator:read:followers` | broadcaster | `channel.follow` v2 |
| `channel:manage:broadcast` | broadcaster | stream markers |
| `channel:manage:predictions` | broadcaster | predictions (later, Affiliate-gated) |

Startup asserts the actual granted scopes via `getTokenInfo` and refuses to start with a named missing scope rather than failing at the first subscription. `channel.raid` needs no scope but does need a user token and a `to_broadcaster_user_id` condition.

## B2. Day one (pre-channel-setup)

- **EventSub WebSocket** listener (`src/eventsub.ts`), PubSub is dead, handling reconnect messages, keepalive timeouts and the revocation event with an alert into `/health`.
- **Commands** (`src/commands.ts`): `!fly`, `!brain`, `!how`, `!stuck`, `!sugar`. Rate limiting in `src/ratelimit.ts`: per-user token bucket (1 per 60 s per command), global bucket (1 reply per 5 s), and a per-command global cooldown. `!sugar` additionally consults the sim's own cooldown so the answer is truthful.
- **Template-only replies** (`src/templates.ts`). Every outbound string is a constant with typed numeric/name placeholders, and names pass the allowlist described in section C. No model, no user text, no concatenation of chat input. This is the Nothing-Forever lesson encoded as a type: `send()` accepts only `TemplateId` plus a validated params object, never a `string`.
- **Explainer poster** every 20 min from a rotating list, skipped if chat has been silent, so it does not shout into an empty room.
- **Follower and raid echo**: on `channel.follow` or `channel.raid`, POST a ticker event to the sim control API so the name appears on the broadcast (subject to the same name validation).
- **Chat rate limits**: 20 messages per 30 s per channel for a plain account, 100 per 30 s if the bot is a moderator or the broadcaster. Make the bot a moderator on both channels and still self-limit to 15 per 30 s. Helix is 800 points per minute per client id. Both figures are Twitch-documented but should be re-verified at implementation time.
- **Bot flagging**: request known-bot status for the bot account through Twitch developer support, and always use `user:bot` + `channel:bot` so Twitch can attribute the traffic correctly.

## B3. When the channel is set up

`src/redemptions.ts`:
1. Create the "Sugar" custom reward through the Helix API (`createCustomReward`) so the same client id owns it and may therefore fulfil and refund. Idempotent: look up by title first, store the reward id in `state.json`. Configure `is_user_input_required: false` (no free text to render), a global cooldown, and a per-stream max.
2. Subscribe to `channel.channel_points_custom_reward_redemption.add` filtered to that reward id.
3. On redemption: `POST /stimulate` on flysim with `{ source: 'channel_points', displayName, redemptionId, durationMs }`. On 2xx, `updateRedemptionStatusByIds(..., 'FULFILLED')`. On 4xx/5xx, rate-limit rejection, or timeout, `'CANCELED'`, which refunds the points. Never leave a redemption pending: a bounded retry then a forced refund, logged.
4. `POST /marker` (stream markers via `createStreamMarker`) on badge and rank-change events, which is what the daily recap cut uses.
5. Predictions (`src/predictions.ts`, Affiliate-gated, feature-flagged off): open on milestone start ("will the fly reach X within 4 hours?"), resolve on rank change or timeout. Behind a flag because the plan records Predictions as still Affiliate-only.

## B4. Testing, health, metrics

- `twitch event trigger channel.chat.message --transport=websocket` etc. against `twitch event websocket start-server`, driven by `tools/mock-twitch.sh`; a documented list of the exact trigger commands per handler.
- `tests/fake-sim.ts`: a `node:http` server implementing `/stimulate`, `/status`, `/ticker` with scriptable failures (429, 500, timeout) so the fulfil/refund state machine is unit-tested without Twitch.
- Unit tests for rate limiters (fake timers), template rendering with hostile names, the redemption state machine, and scope assertion.
- `/health` returns token expiry, EventSub subscription list and socket state, sim reachability, last redemption outcome, and the audio/feed staleness the stage reports. `/metrics` is Prometheus text: commands served, replies suppressed by rate limit, redemptions fulfilled/refunded, EventSub reconnects, sim call latency.
- Policy constraints as code: a lint test asserting no call site passes a non-constant string to `send()`, and that no sim payload field is ever sourced from chat message text.

## B5. EventSub resilience (added 2026-09-16 after an hour of dead chat)

This section is written after the fact. B2 said "handling reconnect messages, keepalive timeouts and the revocation event with an alert into `/health`", and that is what was built. It was not enough, because the failure that actually happened was none of those three and `/health` is not something that can fix anything.

**What happened.** the release container, 2026-09-16, bridge from `v0.1.2`/`v0.1.3`, twurple 8.1.4, Node 22. At 11:45:34 UTC both EventSub websockets — the bot listener and the broadcaster listener, which on `<twitch-channel>` are the SAME Twitch account — disconnected with code 1006. twurple reconnected immediately and, on `session_welcome`, re-created every subscription for that user. Twitch answered the `channel.chat.message` creation with

```
HTTP 429 {"error":"Too Many Requests","status":429,"message":"number of websocket transports limit exceeded"}
```

at 11:45:35 and again at 11:45:45. The bot socket then closed with 4003 "connection unused". After that the process stayed up, `systemctl status flybridge` said `active (running)` with `NRestarts=0`, and the channel had **no chat subscription at all for an hour** — on-stream chat dead, every command dead — until a hand-typed `systemctl restart flybridge` at 12:47:13, after which the subscriptions were created inside a second.

**Two causes, both of them ours.**

1. *Two transports for one account.* Twitch allows three websocket transports per user. `EventSubWsListener` opens one socket per distinct auth user id, and B1's "one provider per role" (the correct fix for the token-map collision) gave the same account two listeners, hence two of the three. A single disconnect then made twurple ask for two fresh ones while the two stale ones were still counted — four against a limit of three.
2. *No recovery path.* `EventSubSubscription._subscribeAndSave()` in `@twurple/eventsub-base` fires the create request and, on rejection, logs it and emits `onSubscriptionCreateFailure`. No retry, no backoff, no later attempt: the subscription object stays unsubscribed for the life of the process. Nothing turned that into a non-zero exit, so systemd — the only component that could have fixed it — was never told.

**The fix, in three parts.**

- **Fewer transports** (`src/eventsub.ts`, `src/auth.ts`). When the bot and broadcaster roles resolve to the same user id, ONE listener carries every subscription, over an `ApiClient` backed by a new scope-routing `AuthProvider`: it fronts the two per-role `RefreshingAuthProvider`s and picks between them by the scope set each Helix call asks for (`user:read:chat` → bot; `moderator:read:followers`, `channel:*:redemptions` → broadcaster; `channel.raid`, which needs no scope, → broadcaster by default). It is NOT a provider holding both tokens — that is the B1 collision, and one account would lose one of them. Two accounts (the planned separate bot account) keep two listeners, because then each account has its own budget and one socket per account is already the minimum.
- **Self-healing** (`src/subscription-health.ts`). The bridge tracks one fact: has `channel.chat.message` been confirmed created since the last time it was lost? `onSubscriptionCreateSuccess` confirms; `onUserSocketDisconnect`, `onSubscriptionCreateFailure`, `onRevoke` and "startup, not yet created" arm a grace timer of `EVENTSUB_GRACE_MS` (default 60 s), measured from the FIRST loss so a flapping socket cannot postpone recovery. If the grace elapses unconfirmed, the bridge logs one `FATAL:` line naming every reason and exits 75, and `flybridge.service` (`Restart=always`, `RestartSec=15`, `StartLimitIntervalSec=0`) restarts it with a fresh transport. The exit IS the 429 backoff: a transport limit is cleared by time, not by another attempt, and 15 s is what Twitch needed on 2026-09-16 to reap the stale ones. There is no Helix `eventsub/subscriptions` poll; the listener's own events say everything. Two twurple traps found writing this, both recorded in the module header: `EventSubSubscription.verified` is permanently `false` for websocket subscriptions (`_verify()` is only called by the webhook listeners, which are not in `@twurple/eventsub-ws`), and `onSubscriptionActivate` fires BEFORE the create request, so it also fires on the way into the 429.
- **One notice, not a stream of them** (`src/notice.ts`, `templates.ts`). The startup notice behaviour is unchanged for a cold or hand-typed start. But the self-healing exit could restart the process every ~75 s through a bad half hour, so the notice clock is persisted in `/var/lib/flybridge/notice-state.json` and at most one notice goes out per `NOTICE_MIN_INTERVAL_MS` (default 10 min) across restarts. The dying process leaves a `pendingRecovery` marker there, so the next start that is allowed to speak posts the `recovered` template ("its Twitch chat connection dropped and it reconnected. The fly never stopped playing…") instead of `startup`. A marker older than an hour is reported as a plain startup, because a self-heal at 02:00 and a restart at 09:00 are not the same event.

**What `/health` now says.** `chatSubscriptionHealthy` is a separate field from `eventSubConnected`, because the distinction is the whole incident: the sockets were connected the entire hour. `eventSubListenerCount` (1 or 2) makes the transport saving observable, and `lastSelfHealAtIso` says whether this process is a recovery. New counters: `flybridge_eventsub_chat_subscription_confirmed_total`, `..._lost_total`, `flybridge_eventsub_subscription_create_failures_total`, `flybridge_eventsub_transport_limit_total`.

**Tests.** `tests/subscription-health.test.ts` (state machine, manual timers), `tests/eventsub.test.ts` (the `createListener` seam: one listener for one account, two for two, failure-then-no-confirmation trips, confirmation does not), `tests/auth.test.ts` (scope routing), `tests/notice.test.ts` (the ten-minute gate across simulated restarts), `tests/selfheal-exit.test.ts` (a real child process, asserting the exit code systemd sees).

**Still owed.** The sugar/redemption path shares the collapsed socket, so a redemption subscription failure is logged and counted but does not restart the bridge — chat is the only invariant worth a restart, and losing follows must not cost the channel its chat bot. Operator entry: `infra/docs/runbook.md`, "chat dead, bridge active".

---

# C. Shared: `packages/feed`

New workspace package `@flybrain/feed`, zero runtime dependencies so it is importable from the browser, from Node, and as the normative spec for the Rust encoder.

- `src/types.ts`: `FeedHeader` (ms, frame, buttons, rates, populationRate, rewardStats, learning, milestone, events, realtimeFactor, uptimeSeconds), `FeedEvent` as a discriminated union (`reward | viewer_action | follow | raid | milestone | notice`), `Attachment`, `FrameFormat`, `AudioFormat`, `MilestoneRank` as a 0-15 literal union, and `MILESTONE_LABELS` as a 16-entry const tuple.
- `src/envelope.ts`: `encodeSnapshot` / `decodeSnapshot`, pure, operating on `ArrayBuffer`/`Uint8Array`, with a version check that throws a named error the stage renders as a banner rather than a blank page.
- `src/fixture.ts`: read and write `.flyfeed` (length-prefixed messages plus a small manifest).
- `src/control.ts`: request/response types for the flysim control API (`/stimulate`, `/reward`, `/status`, `/checkpoint`, `/pause`, `/ticker`), so flybridge and flysim share one contract. Deliberately no button endpoint, matching decision 6.
- `src/names.ts`: `validateDisplayName`, the single chokepoint.
- `tests/envelope.test.ts`: round-trip plus a committed golden binary that the Rust side's own test byte-compares against.

Rust keeps its own serializer; the golden fixture is the cross-language contract, the same pattern already used for the kernel oracle in decision 2.

**How viewer names reach the ticker.** Strictly one direction: Twitch -> flybridge -> `POST /stimulate` or `POST /ticker` on flysim -> flysim emits a `viewer_action` entry in the next snapshot's `events[]` -> flystage renders it. The page has no Twitch credentials, no Twitch network access, and no code path that accepts text from outside the feed. `validateDisplayName` runs in flybridge before the sim call (`^[\p{L}\p{N}_]{1,25}$`, else the literal `"a viewer"`), and the stage re-validates on render as defence in depth. Only login/display names ever cross, never message bodies, which satisfies the "never render raw chat into the video" rule structurally rather than by discipline.

---

# D. Milestones with go/no-go

| ID | Deliverable | Go/no-go gate |
|---|---|---|
| **S0** | Phase 0 measurements (A5 list 1-9), on the host, in the real Chromium/Xvfb config | Brain map paint p99 < 12 ms and no GL context. If not: shrink the map, raise `MAX_SPRITES` cap downward, ask flysim to pre-decimate spikes |
| **S1** | `packages/feed` + fixture player + 6 mockup PNGs (3 themes x 2 moments) | **the operator picks a theme from the images.** Blocking. No panel is wired to the live feed before this |
| **S2** | All panels on the fixture, Playwright screenshot tests, text-size lint, legibility artifacts | Zero text below 18 px; the three fixtures screenshot-stable; the 0.31 downscale artifacts approved by the operator |
| **S3** | Live feed against flysim, reconnect and stale-feed handling, 1080p mode | One hour live with zero unhandled errors, rAF cadence within 10 percent of 60 Hz, sim fps unchanged versus stage-off baseline |
| **S4** | Audio ring buffer + SFX + Pulse sink + MediaMTX end-to-end | VLC plays from MediaMTX with production ffmpeg flags; measured A/V offset under 120 ms; zero underruns in one hour |
| **B1** | Bridge chat commands against Twitch CLI mocks, template lint, fake-sim tests | All five commands correct under mocks; rate limiters provably bound; `send()` lint passes |
| **B2** | Live on a throwaway test channel | Bot replies within limits for one hour; EventSub reconnect survives a forced socket kill; tokens refresh across a restart |
| **B3** | Channel Points Sugar | Reward created idempotently; 20 redemptions with injected failures all end FULFILLED or refunded, none pending; every pulse appears on the broadcast with a validated name |

Parallelisable from the start, per the kickoff playbook: `packages/feed` types, the Playwright harness, the mockups, and the bridge's mock-driven work are four independent tracks. Serial: the paint loop and the feed store.

---

# Critical files to create

**flystage**: `apps/stage/{package.json,vite.config.ts,index.html,components.json}`, `src/main.tsx`, `src/App.tsx`, `src/feed/{socket,fixture,store}.ts`, `src/paint/{loop,game,brainmap,retina}.ts`, `src/workers/brain-base.worker.ts`, `src/audio/{engine,ring-worklet,sfx}.ts`, `src/panels/*.tsx` (13 panels), `src/theme/{tokens,t1-instrument,t2-phosphor,t3-fieldlab}.css`, `src/lib/{labels,schedule}.ts`, `tools/{mockup,record-fixture}.mts`, `tests/e2e/{screenshots,text-size,legibility,structure,behaviour}.spec.ts`, `fixtures/{cold-open,steady,big-moment}.flyfeed`.

**flybridge**: `services/bridge/{package.json,tsconfig.json}`, `src/{index,config,auth,chat,commands,templates,ratelimit,eventsub,redemptions,predictions,markers,sim,explainer,health,metrics}.ts`, `tools/{authorize.mts,mock-twitch.sh}`, `tests/{fake-sim.ts,commands.test.ts,redemptions.test.ts,ratelimit.test.ts,templates.test.ts}`, `deploy/flybridge.service`.

**shared**: `packages/feed/src/{index,types,envelope,fixture,control,names}.ts`, `packages/feed/tests/envelope.test.ts` plus the committed golden binary.

# What to reuse from `packages/brain` (no new code needed)

- `@flybrain/brain/browser` -> `loadCompressed` for `positions.binz`, `viewer-edges.binz`, `visual-xy.binz`, `visual-hemisphere.binz`, `visual-indices.binz`. Do **not** call `loadBrainDataset`: it also pulls `indptr`/`targets`/`weights`, about 9.5 MB compressed of connectivity the display never touches.
- `@flybrain/brain/view` -> `normalizePositions`, `classifyByRoles` (`src/view/layout.ts`, already unit-tested, DOM-free, safe in a worker). Do **not** import `view/connectome.ts`: it drags in `three`.
- `@flybrain/brain` -> `projectFrame`, `DEFAULT_RETINA_CONFIG` for the retina panel, so it is the same kernel the sim runs.
- `readout/presets/gameboy.ts` -> `GAMEBOY_BUTTONS`, `GAMEBOY_BUTTON_BITS`, `fromButtonMask` for the button row and the bitmask decode; `gameboyDecoderConfig()` supplies the true `holdMs: 85` and `cooldownMs: 480` that set the afterglow window.
- `data/fafb-v783/meta.json` `roles` and `circuit-roles.json` as the authority for the label table; a unit test asserts `labels.ts` covers every role key so a dataset change cannot silently drop a bar.
- Milestone labels transcribed from `~/fly-plays-pokemon/src/reward/pokemon-red.ts:126-134` and `~/fly-plays-pokemon/src/runtime/ratchet.ts` (`best > 15` is the existing rank bound, confirming 0-15).
- Patterns, not code: `playwright.config.ts` and `tests/browser/shell.spec.ts` from `fly-plays-pokemon` for the harness shape, and `tools/check_package_age.mjs` for the dependency cooling-off gate.

# Risks

1. **2D canvas throughput.** Mitigated by the accumulator design (A5) rather than hoped away, but the spike-count-per-snapshot figure is an estimate. If flysim's p99 is far higher than 18,000, the fix is server-side decimation or a coarser LUT grid, both cheap. Measured in S0 before any panel depends on it.
2. **Web Audio in headless-ish Chromium.** Needs `--autoplay-policy=no-user-gesture-required`, a live Pulse null sink visible to the Chromium process, and the removal of `--mute-audio` from the documented flag set. Three separate ways to get silence that looks like success, so `/health` must assert `audioContext.state === 'running'` **and** non-zero RMS on the sink, and `flycast` must refuse to go live without both.
3. **Font loading before first paint.** A broadcast page that paints once and runs for weeks will happily broadcast a fallback font forever if `document.fonts.ready` is not gated. Mitigated by the `data-ready` attribute that both the capture launcher and every test wait on, plus `size-adjust` fallback metrics so even a failed load does not reflow the grid.
4. **twurple EventSub token scopes.** Redemptions need the broadcaster token, chat send needs the bot token with `user:bot` plus `channel:bot`, and `channel.follow` v2 needs `moderator:read:followers`. Getting one wrong fails at subscription time, hours later, in a way that looks like a network problem. Mitigated by the startup scope assertion that names the missing scope and refuses to start.
5. **Pending redemptions.** A crash between `POST /stimulate` and `updateRedemptionStatus` leaves viewer points in limbo, which is the fastest way to lose an audience's trust. Mitigated by a persisted intent log replayed on startup and a forced refund after a bounded retry window.
6. **Affiliate gating drift.** Predictions and polls are Affiliate-only as of Sept 2026 while Channel Points are not. Both are feature-flagged so the bridge boots on an unaffiliated channel with the gated features dark, not crashed.
7. **Mockup-gate discipline.** The recorded failure mode is building first and showing prose later. S1 is structurally blocking: the panels have no live-feed wiring until a theme PNG is chosen.