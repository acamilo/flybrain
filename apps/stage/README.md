# flystage

The 1920x1080 broadcast page for the 24/7 stream: a simulated fruit-fly connectome (139,255
neurons, FlyWire FAFB v783) plays a Game Boy game, and this page is what the encoder captures.

It is a **display only**. The simulation runs as a service (`flysim`) and this page renders what
arrives over a local WebSocket. There is no ROM here, no emulator, no input path, and no code
path that accepts text from anywhere but the feed.

Binding contracts: [`docs/feed-protocol.md`](../../docs/feed-protocol.md) and
[`docs/control-api.md`](../../docs/control-api.md). Design: `docs/design/stage-bridge.md`
sections 0 and A. Deviations from that design are listed at the bottom of this file.

## Running it

```sh
npm ci                       # from the repo root

# Player mode: replay a recorded fixture, no service needed. This is the default.
npm run dev -w @flybrain/stage
#   http://127.0.0.1:5273/?mode=player&fixture=steady

# Live mode: the real feed. Needs flysim, or the fake one:
npx tsx packages/feed/src/fake/server.ts --scenario running
npm run dev -w @flybrain/stage
#   http://127.0.0.1:5273/?mode=live
```

### Query parameters

Everything the page can be told is a query parameter, because the only two things that launch it
are a `chromium --kiosk` line in a systemd unit and a Playwright test.

| Parameter | Values | Meaning |
|---|---|---|
| `mode` | `player` (default), `live` | Replay a fixture, or open the feed socket. |
| `fixture` | `cold-open`, `steady`, `big-moment`, `macros` | Which recording to replay. `macros` is the only one with `[macros] mode = macros`; the other three predate the macro channels and replay the raw layout. |
| `t` | seconds | Seek there. **Without `play=1` the page holds and freezes its clock**, which is what makes a screenshot reproducible. |
| `play` | `1` | Keep playing after a seek. |
| `loop` | `0` | Stop at the end instead of looping. |
| `res` | `1080` (default), `720` | `720` is `transform: scale(0.6667)` on the one 1920x1080 stage: thumbnails and the downscale tests. |
| `theme` | `t1` (default), `t2`, `t3` | T1 Instrument, T2 Phosphor, T3 Field lab. |
| `fly` | `webgl` (default), `paper`, `off` | Which renderer draws the fly strip. `paper` is the 2D fallback for a host with no usable WebGL, and `webgl` **falls back to it by itself** when the GL context cannot be created (the capture container's `--disable-gpu` Chromium, measured on the P0 spike); `off` draws no fly and creates no GL context. `window.__stage.fly()` reports the renderer actually drawing as `mode` and the query parameter as `requested`. |
| `tab` | `senses`, `connectome`, `ladder` | Pin the tab slot and stop the cycle. Every visual test and every mockup uses it. |
| `chat` | `off` (or `0`) | The chat kill switch: no panel at all, whatever the feed carries. |
| `game` | `pokemon-red` (default), `platformer` | Per-game config. |
| `feed` | ws URL | Feed override for `mode=live`. Default `ws://127.0.0.1:7400/feed`. |
| `gain`, `gamegain`, `sfxgain` | 0..1 | Master / game / SFX gain. Defaults 0.9 / 0.8 / 0.5. |
| `audio` | `0` | Do not create an AudioContext at all. |

`window.__stage` exposes the operator surface: `metrics()` (per-stage paint timings), `audio()`
(context state, ring fill, underruns, drops), `health()` (accepted snapshots, feed gaps, decode
errors), `manifest()`, `seek(seconds)`, `stopFeed()`, `gameScale()`, `fly()` (renderer mode, gait
phase, leg tips, proboscis extension), `motion()` (which tab and why, the moment on stage and its
phase, the queue depth, live particles), `pam()` (the PAM centroid the flare spreads from), and
`fire(type, label, detail)` — the one deliberate way to drive the moment catalogue by hand, which
is what the moment mockups and the moment assertions use instead of waiting for a fixture to
contain one of each.

### Recording fixtures

```sh
# Starts a fake flysim itself, records 120 s, writes public/fixtures/steady.flyfeed.gz
npm run record -w @flybrain/stage -- --name steady --scenario running --seconds 120

# From a service that is already running, with two sugar redemptions in the middle
npm run record -w @flybrain/stage -- --name live --url ws://127.0.0.1:7400/feed \
  --control-url http://127.0.0.1:7401 --seconds 60 --stimulate-at 8,34
```

The `.flyfeed` container lives in `packages/feed/src/fixture.ts` (shared, because the recorder is
Node and the player is the browser). `--spikes-stride 1 --audio-seconds 0` records at full
fidelity; the committed fixtures do not, and the reasons and numbers are in the recorder's header
comment and in each file's own manifest.

### Audio in player mode

Two things look like bugs in player mode and are not:

- the committed fixtures carry audio only for their first 12 s, so past that the ring buffer
  underruns continuously and `__stage.audio().underruns` climbs by about 375 a second (one per
  128-sample block). Record with `--audio-seconds 999` if you need a continuous stream.
- the ring fill sits near 60 ms rather than the 250 ms target, and the servo holds its rate at
  0.997 trying to grow it. A fixture delivers audio at exactly 1x real time, so there is no
  surplus to build the cushion out of; a real `flysim` running slightly ahead of the sound card
  builds it in a few seconds. The servo is pulling in the right direction either way.

### Mockups (the sign-off gate)

```sh
npm run mockups -w @flybrain/stage
```

Builds, serves, and writes sixteen PNGs to `mockups/`, at 1920x1080 and DPR 1:
`steady-t1-{senses,connectome,ladder}` and `describe` for the four tabs, `big-moment-t1` 2.5 s into that
fixture's milestone, `moment-{milestone,badge,sugar,rollback}` shot 300 ms after the trigger (the
middle of every arrival in the catalogue), `macros-{overworld,running,outcome,battle,indoors}` for
the macro strip's five states, and the two `fly-*` review crops at 2x. `--only <substring>` shoots
just the ones whose name contains it, which is how one panel gets re-reviewed without rewriting
every committed PNG.

The theme sweep these used to be is gone: T1 was chosen in September, so what the images are for
now is the layout and the motion. They still serve the kickoff playbook's purpose — a human looks
at a picture before anyone argues about prose.

```sh
npm run fonts -w @flybrain/stage
```

Writes the eleventh PNG, `mockups/gameboy-fonts.png`: the three OFL pixel body candidates
`docs/design/gameboy-theme.md` names, each rendered in the parts of the rail where a body face has
to work — the 30 px rung line, the 38-cell spine, the cluster's readouts and footer, two ticker
lines and two chat lines — at their real token sizes and inside the dialogue-box frame. This was
the document's gate, and **the operator picked Silkscreen from it on 2026-09-15**; the frame marks the
pick, and the other two faces stay committed so the comparison can be re-rendered.

Neither the mockup tool nor the e2e suite waits on a timer for the page any more. `data-ready="1"`
means the fonts and the brain map's base bitmap are in — not that the fixture is — and on a cold
browser context `steady` (23.3 MB) reports ready at 1.7 s and its first accepted snapshot at 4.2 s.
Both now wait for `__stage.health().accepted` to move before they shoot, because a mockup of the
page's own initial zeroes is worse than a slow one.

### Tests

```sh
npm test -w @flybrain/stage          # node --test: 213 unit tests
npm run typecheck -w @flybrain/stage
npm run test:e2e -w @flybrain/stage  # Playwright, against the real build via vite preview

npx playwright install chromium      # once
npm run test:e2e -w @flybrain/stage -- --update-snapshots   # after an intended visual change
```

The e2e suite builds the app and serves it with `vite preview`, because the page that goes on air
is the build output, not the dev server. The downscale artifacts the legibility test produces land
in `tests/e2e/artifacts/` and are attached to the HTML report.

### Building a release

`__STAGE_VERSION__` (the title strip's version chip, `data-testid="stage-version"`) is
`vite.config.ts`'s `git describe --tags --always --dirty`, read from the working tree at build
time — a tagged, clean checkout gives `v0.1.0`; an untagged one falls back to the short sha on its
own; a dirty tree appends `-dirty`; the dev server always reads `dev` instead, since HMR does not
represent a build. **The stage build has to run from the tagged checkout for the string to be
right** — `npm run build -w @flybrain/stage` (or the `npm run build` a release pipeline calls)
reads `git describe` from wherever it is invoked, not from a version this repo tracks separately,
so a build off a branch that has moved past the tag, or off a dirty tree, bakes that into the page.
`infra/build/package-release.sh` and `infra/05-deploy.sh` do not pass the stage build anything
extra for this — they already require and record the tag independently (`infra/05-deploy.sh`'s
`require_release_tag`) — so getting the version chip right is entirely about building from the
right commit before packaging.

## Geometry

Layout B at **1920x1080**, 48 px safe insets, all geometry in `src/lib/geometry.ts`. There is one
authoring resolution and it is the broadcast resolution, so `#stage` carries no transform at all
on air; `?res=720` is `transform: scale(0.6667)` on that one element, for thumbnails and the
downscale tests.

The arithmetic, which closes exactly in both columns:

```
left column   800 wide:   title 40 + game 720 + gap 4 + fly strip 220       = 984 = 1080 - 2x48
fly strip     220 tall:   4 border + button row 32 + gap 4 + row 176        = 220
  its row:    fly canvas 416 + gap 12 + macro palette 364                   = 792 = 800 - 2x4
right rail   1012 wide:   cluster 144 + slot 420 + events 100 + chat 244
                          + 3 gutters of 12                                 = 944 = 720 + 4 + 220
width                     800 + 12 + 1012                                   = 1824 = 1920 - 2x48
tab slot      420 tall:   48 tab strip + 370 pane + 2 border                = 420
```

The title strip spans the full usable width and the game abuts it with no gutter, which is what
makes the left column close on 984. The rail starts level with the top of the game and ends level
with the bottom of the fly strip, at y=1032.

Text floors, in authoring pixels: body/ticker >= 24, labels 30-36 — 10% down from 27/33-39
(2026-09-16, once the VT323 split below gave the rail room to spare) — and
`tests/e2e/text-size.spec.ts` enforces both those and the 16 / 20-24 they land on in the 720p
thumbnail mode. Layout v2 has no 72 px hero: see the deviations.

## Rail layout v2

Locked in `docs/stream-mvp-plan.md` ("Rail layout v2, locked 2026-09-15 night") and built here.
Four rail panels instead of layout v1's five, on a 12 px gutter; the left column is unchanged:

| Panel | Box | What it shows | Source |
|---|---|---|---|
| Title strip | 1824x40 | Wordmark, mode chip (hidden when unknown), the DAY N slide | `panels/TitleStrip.tsx` |
| Game | 800x720 | The framebuffer at exactly 5x, and the rollback rewind wipe | `panels/GamePanel.tsx` |
| Fly strip | 800x220 | A plain 32 px button row along the top edge, the 3D fly filling the rest | `panels/FlyStrip.tsx` |
| Progress cluster | 1012x144 | The whole progress readout, in one panel | `panels/ProgressCluster.tsx` |
| Tab slot | 1012x420 | A 48 px tab strip and one of four panes | `panels/TabSlot.tsx` |
| EVENTS | 1012x100 | Three ticker rows, dwell-gated, tiered. No title | `panels/EventsTicker.tsx` |
| CHAT | 1012x244 | The last seven chat lines, or nothing at all | `panels/ChatPanel.tsx` |
| Moment layer | — | Caption band, rail flash, particles, day slide | `panels/MomentLayer.tsx` |
| Stale feed banner | 1824x40 | Over the title strip after 2 s of silence | `panels/StaleBanner.tsx` |

### The progress cluster

One panel where layout v1 had four (ladder, stuck-o-meter, run clock, sugar), because that was 14
panel borders and four titles for eight numbers (the middot and the ring are drawn, not typed):

```
                                                   here for   brain
0/37  Boot screen  ->  Left the bedroom              1m34s     11.4 Hz
###############################################################
0/8 badges * 214 places                 try 1   06:12:33 * day 3   () SUGAR READY
```

The rung line is one line of 30 px mono — between the 27 px body floor and the 33 px label band,
deliberately neither. The spine draws one cell per rung of the ladder, from `milestone.total` when
the service sends it and from the game config's ladder otherwise (`src/lib/ladder.ts`), which is 38
for this demo. The SUGAR chip carries the cooldown ring.

### The tabs

Four tabs — SENSES, CONNECTOME, LADDER, DESCRIBE — in one 420 px slot, with an amber underline that
slides and a 300 ms crossfade. `src/lib/tabs.ts` decides which one is up, and `?tab=` pins it:

- **Focus.** A moment gives its own tab the slot for the moment's duration and then hands it back.
  This is what replaced layout v1's promotion of the brain map over the whole rail.
- **Event steering.** A rung change goes to LADDER, a reward to CONNECTOME, gated by a 12 s dwell
  so a reward every twenty seconds cannot make the slot flicker.
- **The cycle.** Otherwise the slot advances on its own every 45 to 60 s, and "walking with high
  command activity" biases that rotation toward SENSES rather than owning it. DESCRIBE only ever
  arrives this way: nothing steers to it and no moment focuses it, because it says what the stream
  is rather than what just happened.

| Pane | What it shows |
|---|---|
| SENSES | The retina raster at 548x316, both eyes, beside six labelled circuit groups with their peak-hold and decision-threshold ticks, plus a MACROS row of whichever macro channels the scene has bound |
| CONNECTOME | The 2D brain map at the pane's own 1008x370, with the reward flare spreading from the PAM cluster's measured centroid |
| LADDER | All 38 rungs in three column-major columns, the rollback budget (this rung and lifetime) and the stall meter |
| DESCRIBE | What this is, one card at a time: a Silkscreen label, a paragraph of VT323 at the body floor, and a cell per card showing where the cycle is |

**All four panes stay mounted.** Only one is `data-visible="1"` — which is what the structural
test counts — and the others are `visibility: hidden`, out of paint entirely. They stay mounted
because the connectome's base bitmap is a worker's 139,255-point raster: unmounting the canvas
would throw it away and re-raster it on every tab cycle, twenty times an hour, for ever.

DESCRIBE's copy is `src/games/describe.ts` and nowhere else — one entry per card, in the reading
order, pending the operator's review (`docs/design/describe-tab.md`). The cards' numbers are placeholders
the page fills from the dataset metadata, the game config and the build's version
(`src/lib/describe.ts`), so a count on that tab cannot outlive the connectome it describes, and
`tests/unit/describe.test.ts` holds the file against the doc card for card. One card is up per
appearance of the tab, which is the rail's own 45-to-60 s cadence; `src/motion/director.ts`
advances it.

### Chat

The last seven lines of `header.chat`. Strictly text: no links, no images, no markup, no embeds.

The service is the authority — `packages/feed/src/chat.ts` is the sanitizer, and
`services/flysim/crates/flysim/src/chat.rs` enforces byte-identical rules in Rust — and
`src/chat/sanitize.ts` runs **that same shared implementation** again at the point of render,
dropping any line that fails. Not a local copy of the rules: there is no second set to drift.

A line that fails is not truncated or masked, it is not shown. With no `chat` in the header (an
older service, or `[chat] enabled = false`, which omits the key) and with `?chat=off`, the panel
renders nothing at all — no border, no title, no empty box. "The panel is there but empty" is what
a viewer reads as "the stream is broken", so it is the case the structural test pins.

Bot lines green, names amber, text ink: a viewer has to be able to tell the bridge's own template
replies from a person at a glance, because the bridge is the only thing on this stream that can be
made to say something by accident.

### Moments

`docs/design/animation.md`'s catalogue, wired. The engine (`src/motion/`, landed separately) owns
the queue, the particle pool and the catalogue as data; `src/motion/director.ts` is the half that
touches the rail, and it runs once per animation frame:

| Trigger | What happens |
|---|---|
| Milestone | Caption band in from the left over the tab slot, the new rung pulses twice then fills, amber sparks from that rung, LADDER focus for 9 s, chime |
| Badge | All of that plus the rail border flashing 120/600 ms, the badge count bouncing 1.15x, a fountain over the rail, a shockwave from the count, the connectome flaring, fanfare |
| Sugar | Proboscis and head glow (already rate-driven), the ring filling and draining over the cooldown, warm sparks drifting from the fly's head to the sugar chip, tone |
| Small reward | The ticker row slides up 240 ms, its value flashes amber 120/600 ms, sparks scaled by the reward's value tier, tick |
| Rollback | A 400 ms horizontal rewind wipe over the game canvas with a scanline flicker and backward streaks, the try count, "REWIND . try 3", rewind sweep |
| Mode change | The chip's text rolls vertically over 200 ms |
| Day rollover | "DAY N" slides across the title strip once, 2 s, soft stinger |
| HERE FOR 1 h / 3 h / 6 h | The number pulses once and its colour steps warmer |

Two things about the numbers. The holds are 9 s of *total* stage time (320 ms in, 8360 holding,
320 out), because that is what the design's own verification measures and what "LADDER focus 9 s"
means on screen. And every readout is lerped by the paint loop rather than committed by React
(`src/motion/readouts.ts`): the components render the numeric elements *empty* and the loop owns
their text, with a 120-300 ms time constant. The three discrete ones — the rung index, the try
count, the rollback budgets — are deliberately not lerped, because "RUNG 4.6/37" is not smooth,
it is wrong.

The particle layer is one 2D canvas over the frame, capped at 400, additive, pooled, seeded so a
screenshot is reproducible. It spans the whole stage because the sugar sparks have to cross from
the fly's head to the sugar chip, and it is clipped to the rail plus the fly strip so a badge's
shockwave can never cross the game.

**Paint cost**, measured on this laptop over 10 s of the `steady` fixture with a badge fired 4 s
in — the most expensive frame the page ever draws: a fountain, a shockwave, the rail flash, the
caption band, the connectome flare and the counter bounce, all at once. About 600 frames per tab,
and `tests/e2e/behaviour.spec.ts` prints the same figures on every run:

| Tab | whole frame p50 | p95 | `motion` p95 | brain map p95 |
|---|---|---|---|---|
| SENSES | 1.8 ms | 3.5 ms | 1.7 ms | 1.0 ms |
| CONNECTOME | 1.8 ms | 3.3 ms | 1.8 ms | 0.8 ms |
| LADDER | 1.7 ms | 2.9 ms | 1.4 ms | 0.8 ms |
| idle, no moment | 1.8 ms | 3.2 ms | — | 0.8 ms |

The design's budget is a whole-frame p95 under 4 ms, and the `motion` stage — the moment queue,
the particle simulation and its draw, the lerped readouts and the tab slot — is under 2 ms of it.
The `max` column is left out on purpose: it is 20 to 46 ms on every one of these runs, always on
the first frame, and always the WebGL fly's context creation and shader compile. It never recurs,
and `?fly=paper` or `?fly=off` removes it.

### The fly

A small 3D fly sits under the game, facing the screen, with a plain row of eight button
indicators along the top of its strip. Every motion of the *fly itself* is a real population
rate, and nothing about it presses a button: tripod gait speed from `forward`/`backward`, body
yaw and stride asymmetry from `steer_left`/`steer_right`, wing beat amplitude and frequency (and
haltere jitter) from the sum of `command_0..7` — a stand-in for a `motor` role rate until one is
in the feed, see below — the proboscis from `proboscis` and sugar events, the head and thorax
glow from `reward_pam`, and the abdomen's breathing from the population rate. Nothing is scripted
or random except a small idle floor on the wing beat. The binding brief is
[`docs/design/fly-avatar.md`](../../docs/design/fly-avatar.md).

| File | What it is |
|---|---|
| `src/fly/rig.ts` | The animal: proportions, the tripod gait, leg IK, the wing beat, drives to joints. Emits world-space points and knows nothing about drawing. |
| `src/fly/drives.ts` | Feed rates to 0..1 drives, through the same running reference the circuit bars use. Holds `WING_DRIVE_ROLES`, the one table that maps the wing/flight drive to its source roles. |
| `src/fly/camera.ts` | The one camera both renderers share. |
| `src/fly/webgl.ts` | three.js, one GL context, well under 1,600 triangles, flat-shaded Lambert. |
| `src/fly/paper.ts` | The same rig projected by hand into a 2D canvas, painter's algorithm. |

TODO: `docs/design/fly-avatar.md`'s neuron table calls for a `motor` role (110 neurons) driving
the wings; `docs/feed-protocol.md`'s `rates` does not carry `motor` yet, so `WING_DRIVE_ROLES` in
`src/fly/drives.ts` sums the eight `command_0..7` descending-command rates instead. Swapping in
`motor` once the feed grows it is a one-line change in that file.

Every drive is normalized against its role's own running reference (`src/lib/circuit-scale.ts`)
and then re-centred on the resting level that scale implies (1 / headroom, about 0.67), so the fly
reads as still when the fly is at its own normal and moves when a rate rises above it. A raw
fraction would leave the proboscis half out and the head half lit for ever.

**Paint cost**, measured over 12 s of the `steady` fixture playing on this laptop, one sample per
*drawn* frame at the 30 fps cap (`window.__stage.metrics().stages.fly`):

| Renderer | n | mean | p50 | p95 | max | whole-frame p95 |
|---|---|---|---|---|---|---|
| `webgl` | 371 | 0.35 ms | 0.30 ms | 0.40 ms | 27.20 ms | 1.30 ms |
| `paper` | 371 | 0.09 ms | 0.10 ms | 0.20 ms | 1.50 ms | 1.10 ms |
| `off` | — | — | — | — | — | 1.00 ms |

The WebGL `max` is the first frame — context creation and shader compilation — and never recurs;
every later frame is inside the design's 4 ms budget with two orders of magnitude to spare. The
paper fly is cheaper still and looks plainer, which is the trade the design accepts for a host
with no usable WebGL.

### Copy

Terse and instrument-like, per the direction of 2026-09-15: short nouns for panel titles, no
parenthetical justifications, no sentences under widgets, no captions.

```
A FLY BRAIN PLAYS POKEMON RED
SENSES  CONNECTOME  LADDER   RETINA  CIRCUITS   RUNG  HERE FOR  BRAIN   CHAT   SUGAR READY
```

The neuron count and a learning/frozen chip used to sit beside the wordmark; both are gone
(2026-09-15 review) — the count duplicated the rotating card's own credit line, and "learning" was
one more piece of operator status. The mode chip stays, in plain words (`walking`, `battle`,
`menu`, `boot`, `demo`, …), and disappears entirely rather than show a placeholder when the
adapter reports `UNKNOWN`.

**No explainer card anywhere**, per the locked layout. Layout v1 rotated one card through the
narrative lane for the last minute of every four minutes; v2 gives that lane to the ticker
outright, and the persistent chat panel is what fills the space an explanation used to. The eight
explainer cards and the two-column real-vs-scaffolding panel went in the copy pass before it, and
so did the per-widget captions they duplicated: "one dot per L1 column", "legs: real, wired to
nothing", "stimulates dopamine, never a button", "what the fly sees", "what just happened".

`ROTATING_CARDS` and `src/lib/schedule.ts` are still in the tree, unrendered, and that is a
deliberate loose end rather than dead code left by accident: the four lines include the FlyWire
credit with its CC BY-NC licence, which has to end up *somewhere* (the channel's about page, a
periodic bridge message, or a panel nobody has designed yet). Deleting the strings would lose the
only reviewed copy of them; rendering them would break the locked layout. They stay until that is
decided, in one place, with this paragraph attached.

`tests/unit/labels.test.ts` enforces the register mechanically: every on-screen label is 24
characters or fewer, and none of them contains a full stop, an exclamation mark or a parenthesis.

The middot in the wordmark is in the committed Press Start 2P subset — checked with
`document.fonts.check` and a width comparison against a glyph the subset does not have, because a
missing glyph falls back to a proportional face and shows as tofu on air. The É was in the subset
too, but the face draws it at x-height, so POKÉMON read as POKéMON — a little smaller than the
caps around it. The wordmark in `src/games/pokemon-red.ts` drops the accent (POKEMON) for that
reason; Pokémon Red keeps its accent everywhere else this doc names the game.

### Why the brain map has no WebGL

Decision 3 of the plan: the capture VM has no GPU, and an accidental SwiftShader context costs
one to two cores silently. So the map is three tiers of 2D canvas (design A5): a base bitmap of
all 139,255 neurons rasterised once in a worker, a 252x185 density accumulator over the spike
bitset with a 110 ms decay, and at most 256 pre-rendered sprites on the brightest cells. On top of
those, one moment effect: the reward flare, an expanding ring from the PAM cluster's own centroid,
which the worker computes from `meta.json`'s `reward_pam` role and the normalized positions rather
than from a hand-placed coordinate that would point at the wrong part of the brain the first time
the dataset is rebuilt.

The accumulator's grid takes a different divisor per axis — 1008/4 and 370/2 — because 370 is not
a multiple of 4 and a fractional cell would put the sprite pass a subpixel off the cell it belongs
to. The particle layer is 2D canvas too, for the same reason.

That decision still holds for the map. The fly is the one deliberate exception, and it is
counted rather than trusted: `tests/e2e/structure.spec.ts` asserts the page requests **exactly
one** GL context with `?fly=webgl` and **zero** with `?fly=paper` or `?fly=off`.

## The game config contract

There will be a second demo, so the page is game-agnostic by construction. `src/games/types.ts`
is the contract; `src/games/pokemon-red.ts` and `src/games/platformer.ts` (a stub) implement it,
and `?game=` selects one.

**The split:** live values always come from the feed header. A config only supplies the human copy
for them, plus the wordmark.

| On screen | Value from | Copy from |
|---|---|---|
| Current rung name | `milestone.label` | — (the service is authoritative) |
| Next rung | `milestone.next` | — |
| The rung count | `milestone.total` | `milestoneLadder`'s length, as the fallback (`src/lib/ladder.ts`) |
| The ladder's rung names | — | `milestoneLadder` (the header carries only the current one) |
| Game mode | `game.mode` | `modeLabels` |
| Ticker rows | `events[].rewardKind` | `rewardCopy[kind].label` + `tier` + `dedupeMs` |
| Counters | `game.badges`, `game.uniqueLocations` | `counters[]` |
| The DESCRIBE cards' game name | — | `name`, reached through `{game}` in `src/games/describe.ts` |

Two rules the tests enforce:

- No game is named outside `src/games/`. `tests/unit/labels.test.ts` asserts the dataset-level
  copy mentions no game vocabulary, and `tests/e2e/structure.spec.ts` loads the page with
  `?game=platformer` and asserts the other game's name appears nowhere in the frame.
- The role-to-label mapping for the circuit bars is **dataset-level**, not game-level — it is the
  same fly for both demos — so it lives in `src/lib/labels.ts`, and a unit test asserts it covers
  every role key in `data/fafb-v783/meta.json` and `circuit-roles.json`. A dataset rebuild that
  adds a role fails the test instead of silently dropping a bar.

Adding the platformer for real means replacing the ladder and the copy in
`src/games/platformer.ts`. Nothing else.

Three places outside `src/games/` still contain the string, and none of them is display copy:

- `src/lib/query.ts` and `src/games/index.ts` carry `'pokemon-red'` as the default `?game=` id.
  A default has to name something, and this is a registry key.
- `RewardKind` includes `'pokedex'`, and `src/feed/store.ts` watches `rewardCounts.pokedex` to
  promote the brain map when a counter moves without its event. That key is in the feed protocol
  contract, not in this page, and the config is what turns it into words ("found a secret" for the
  platformer).
- two source comments cite the research the layout came from.

`grep -ril pokemon apps/stage/src` is the check, and `tests/e2e/structure.spec.ts` asserts the
rendered frame contains no trace of the other game when `?game=platformer` is loaded.

## Deviations from design A and from the locked layout

Every one of these is a case where a specification's numbers do not close, or where rendering it
showed the choice failing its own legibility requirement. The first five are about rail layout v2
and supersede the layout v1 deviations they replace.

1. **The rail is the locked four rows** (superseding A2's four *and* layout v1's five). Layout v1
   needed five rows because the brain map was an inset with its own band; v2 makes the map a tab,
   so the rows are the locked 144 / 420 / 100 / 244 on a 12 px gutter, which closes on 944
   exactly. `src/motion/catalogue.ts`'s region boxes read those numbers out of `geometry.ts`
   rather than restating them.
2. **The button row lives inside the fly strip, not above the game** (A2). A2 puts eight glyph
   cells at the bottom of the left column, inside the zone Twitch overlays with chat; this page
   puts them along the fly strip's own top edge instead (32 of its 220 px, full width), with the
   fly's canvas filling the rest. The row briefly moved further still, into the fly's own 3D scene
   as the Game Boy's caps it tapped, but came back out on review: the fly's legs and wings are
   wired to real motor neurons, not to button presses, so tapping a button was never honest
   (`docs/design/fly-avatar.md`). Only the fly's own canvas reaches into the bottom-left
   no-content zone; the button row sits well above it.

   Since 2026-09-16 the strip's lower row is **the fly on the left and the macro strip on the
   right**, sharing one baseline and the strip's one frame (the operator: "slide the fly over and put the
   macro palette right next to it", `docs/design/macros.md` section 6). The fly's canvas is 416 px
   because that is the widest it can be while the strip's left edge stays at x = 480, the
   no-content zone's right edge — the macro cells carry text and its bottom rows are inside the
   zone's band. It is one column of six 27 px cells, not two columns of three, because a cell has
   to hold "BUY POTION" in Silkscreen at the 24 px floor (168 px) and two columns leave 164. Since
   section 12 a cell is its channel tag and the macro's name: the tag is eight characters at its
   longest (`MB·WARP`), which is 140 px of the row's 356, and that is where the gloss column went
   — the gloss is still on the wire, it just has no room on the strip.
3. **The brain map is a tab, and there is no promotion at all** (A2/A5, and superseding layout
   v1's promote-over-the-rail). A2 promotes the map "over the left column", which puts it over the
   game — the one thing a broadcast overlay must never do. Layout v1 promoted it over the rail
   instead; v2 deletes the promotion outright, because the locked layout makes the map one of three
   tabs and "big moments pre-empt for 9 s" is a *tab focus*. So the backing store is the pane's own
   1008x370, allocated once, never rescaled, and the 520 ms transform is off the broadcast's
   critical path. The one thing a moment now draws outside a panel is the caption band, and
   `tests/e2e/behaviour.spec.ts` asserts it is inside the tab slot and clear of the game, the fly
   and the title.
4. **The spine runs across the cluster, not down a panel** (A3). A vertical spine gives each rung
   a couple of pixels, which is under one pixel at the phone downscale the legibility test checks:
   gone. Across the progress cluster's full 984 px, 38 rungs are 24.7 px each, which survives —
   and the current rung's 20 px floor (`docs/design/ladder.md`, measured in
   `tests/e2e/ladder.spec.ts`) no longer even binds.
5. **No 72 px hero anywhere** (A2/A4). The type floors are unchanged, but layout v1's hero role is
   unused: the four panels that carried one (the run clock's Hz, the stuck-o-meter's time) are one
   144 px cluster now, and 72 px of anything does not fit beside a 38-rung spine. The cluster's
   headline is one line of 30 px mono — deliberately between the 27 px body floor and the 33 px
   label band — with the two right-hand readouts at `label-lg`. `tests/e2e/text-size.spec.ts`
   asserts the absence, so a hero cannot creep back in unmeasured.
6. **The grid LUT is a `Uint32Array`** (A5), not a `Uint16Array`. The grid is 252x185 (46,620
   cells), which is inside a u16 — but the LUT is indexed by *neuron*, and there are 139,255 of
   them, so the array is 139,255 entries of cell index either way. It stays `Uint32Array` because
   a future larger grid would overflow silently, which is the failure a `Uint16Array` would buy
   for 278 KB in a worker.
7. **There is no backing-store multiplier at all** (A1). A1 multiplies the game and retina backing
   stores by 1.5 for the 1080p mode; 1080p *is* the authoring size now, so every backing store is
   already 1:1 with the encoded frame and `?res=720` only ever scales down.
8. **Resolved 2026-09-16 (was: the rung line abbreviates the *next* rung, and the LADDER tab
   abbreviates every rung).** The Game Boy pass gave both to Press Start 2P or Silkscreen, and the
   arithmetic never closed at any size inside the floors — a single rung name alone wanted 330 to
   450 px in Press Start 2P or 250 to 335 in Silkscreen, against a rung line with about 370 to
   spare, and the LADDER tab's three columns left each name 147 px, seven characters of Silkscreen,
   so "Viridian City" and "Viridian Forest" both read "Viridia…". The 2026-09-16 pairing moves both
   to `--font-text` (VT323, `src/theme/tokens.css`) — 0.4 em a character against Silkscreen's
   0.73 — and neither needs the cap or the abbreviation any more: the ladder's longest names
   (15 characters) measure about 162 px at 27 px, and both rung names together with the arrow and
   gaps come to roughly 350 px of the rung line's own column. `src/theme/rail.css`'s
   `.rung-line__name`/`.rung-line__next` keep a generous `max-width` and an ellipsis as a
   structural backstop for a service that free-texts something longer than any name in this
   build's ladder, not because either is expected to hit it.
9. **Tabular digits come from a second face, not from the body/label face**
   (`docs/design/gameboy-theme.md`, Type). The design asks for tabular digits "everywhere numbers
   change" and notes that monospace makes it automatic. Silkscreen is proportional and carries no
   `tnum` — measured, every digit is 20/27 em except "1", which is 17 — so `.num` renders in Press
   Start 2P (`--font-num`), the one monospaced face on the page and the design's own face for big
   numbers. This held through the 2026-09-16 font-pairing revision too: VT323 *is* monospaced, but
   `.num`'s digits stayed on Press Start 2P rather than following `--font-text`, because the two
   faces have different cap-heights and swapping mid-line under a `--font-label` word would set a
   readout and its label on two different baselines (`src/theme/tokens.css`). Three consequences of
   the original pick: the two cluster readouts sit at 30 px rather than 36 (2026-09-16's -10% pass;
   33/39 before it), the label band's floor rather than its ceiling, because a full em per
   character took more of the cluster's 980 than the rung line could give up; the clock's day and
   the Hz unit are split out of `.num` into words; and the try count and the ladder's rung index
   are *not* `.num`, because neither is a number that changes often enough for a viewer to catch it
   moving.
10. **Resolved 2026-09-16 (was: the progress cluster's counters line truncates)**. Silkscreen was
    1.8x VT323's width, and the cluster's footer held five readouts wanting about 1,060 px in 980
    even after the splits in (9), so the counters line carried `truncate` and "0/8 badges · 214
    places" ellipsised. Under `--font-text` (VT323) the same line measures well under its column
    (`ProgressCluster.tsx` no longer sets `truncate` on it). **The SENSES head's lost unit word is
    still gone**, though, and is unrelated to the font pairing: the head is 422 px, of which
    "circuits" (`--font-label`) is 229 and the gap 12, and " spikes" alone was 121 against 135 to
    189 for the number it labels — the number is the half that cannot be paraphrased, so the word
    stays off. Visible in `mockups/steady-t1-senses.png`.
11. **The realtime factor is not on screen.** A2 does not ask for it, and "1.00x" is exactly the
    operator status vocabulary the audit complained about. It is on `window.__stage` and in the
    feed header for whoever is on call.
12. **`@flybrain/brain/view/layout` is a new subpath export.** The design says to import
    `normalizePositions`/`classifyByRoles` from `@flybrain/brain/view` and *not* from
    `view/connectome.ts`, but `./view` resolves to `connectome.ts`, which imports `three`. The
    new subpath reaches `view/layout.ts` directly, which is what the design meant.
13. **Fixtures are over the 5 MB target** (cold-open 0.28 MB, big-moment 13.0 MB, steady
    23.3 MB), and gzip does not fix it. Measured: one snapshot from the fake flysim gzips to
    9.7 KB, so 120 s at 30 Hz is 35 MB at full fidelity, and at 30 Hz the *headers alone* are
    2.3 MB gzipped over 120 s while the spike bitsets are close to incompressible. The recorder's
    attachment stride already cuts 442 MB to 9.8 MB; getting under 5 MB would have meant 15 Hz
    frames and 5 Hz spikes, which costs more than the bytes do. big-moment and steady roughly
    doubled on 2026-09-16 when `@flybrain/feed`'s fake simulator changed its default
    `spikesPerTick` from 2,000 to the live service's measured ~30,000 (`packages/feed`'s
    `fake/simulator.ts` and README): the bitset's raw size does not change — it is fixed at
    `ceil(139_255 / 8) = 17,407` bytes regardless of density — but a ~21.5% full bitset is far
    less compressible than a ~1.4% full one, so the kept-every-third-snapshot spike bytes gzip
    much worse. cold-open is unaffected because its `boot` scenario never reaches `running` and
    so never emits spikes.
14. **Fixtures live in `public/fixtures/`**, not `fixtures/`, so Vite serves and copies them
    without a second bespoke middleware (the dataset artifacts already need one).
15. **The SFX bank is synthesised, not sampled** (A8 asks for six short OFL/CC0 samples). Eight
    recipes of oscillators and gain envelopes rendered into `AudioBuffer`s at startup — the
    original six plus the rollback's rewind sweep and the day-rollover stinger, which the moment
    catalogue's sound tiers had been borrowing other samples for. No audio assets in the repo and
    no third-party licence to track. `src/audio/sfx.ts`.
16. **`apps/stage` does not set `noUncheckedIndexedAccess`.** It typechecks `packages/brain` and
    `packages/feed` sources directly (they are source-only workspace packages), and those are
    written against the workspace's own stricter-than-default-but-not-that-strict settings.
17. **The particle layer spans the stage, not just the rail**
    (`docs/design/animation.md`, addendum). The addendum puts particles "on a 2D canvas layer over
    the rail" and then asks for sugar sparks that "drift from the fly's head toward the sugar
    chip" — a path from x=448 in the left column to x=1790 in the rail, which a rail-sized canvas
    cannot draw. The canvas is the whole frame and is *clipped* to the rail plus the fly strip
    instead, so the wider surface buys the effect the addendum describes without buying a licence
    to paint over the game. It also costs nothing on an idle frame: with no live particles the
    layer is not cleared and not drawn.
18. **HERE FOR's thresholds are not queued moments** (`docs/design/animation.md`, catalogue). Every
    other row of the catalogue is a moment; this one is a property of a number that is on screen
    all the time, so the warmth is a function of the *value* (a page that loads into a fly stuck
    for four hours is already warm) and only the crossing pulses. Queueing it would have meant a
    moment that could be pre-empted by a badge and then never seen.
19. **`viewer` events do not reach the EVENTS ticker.** Every accepted chat line logs one, and v2
    gives chat its own panel, so a `viewer` row in EVENTS is the same line twice — once with its
    text and once as the word "chat". Sugar is its own event kind and still lands in EVENTS, which
    is the viewer action that panel is about.
20. **The body face is split in two, VT323 replacing Silkscreen for running text**
    (2026-09-16, the operator, from the sign-off mockup). `--font-body` is `--font-label` (Silkscreen) and
    `--font-text` (VT323) now, not one face doing both jobs: `--font-label` for panel titles, tab
    titles, chips (mode, sugar, button-row glyphs) and the circuit bar row labels — everything
    short and closed-vocabulary, which is what Silkscreen's boxy weight was picked for; `--font-text`
    for everything an open vocabulary or the feed writes — the rung names, the ladder tab, the
    ticker, chat, the clock's day word. `--font-pixel` (Press Start 2P) keeps exactly `.num` and the
    wordmark, unchanged. This is what let deviations 8 and 10 above resolve, and it came with a
    10% cut to the type floors (`--fs-body` 27 -> 24, `--fs-label` 33-39 -> 30-36) once the
    narrower running-text face gave the rail room to spare — `tests/e2e/legibility.spec.ts`'s 0.31
    downscale check is the guard on that cut, and every region cleared it with margin to spare
    (chat 0.104, events 0.126, the rung line 0.177-0.190, the retina 0.094-0.146 by tab, all against
    a 0.035 floor), so nothing reverted to the old size.
21. **The retina raster's coordinates are an axial hex lattice, not Cartesian** (found 2026-09-16,
    The operator: "the retina seems squashed"). `column_assignment.csv`'s `x`/`y`, copied straight through
    by `tools/build_flywire.py`, are integer axial hex column coordinates (measured: 18 columns by
    60 rows, uniform raw nearest-neighbour distance of 1.0) — plotted as Cartesian, one eye's
    bounding box is 17 units wide and 59 tall, which is the squash. `src/paint/retina.ts`'s
    `hexToCartesian` (standard pointy-top axial-to-pixel) runs once in `setColumns`, ahead of the
    existing per-eye letterboxed fit, and takes the aspect to about 1.5 (taller than wide, a real
    compound eye's own shape) rather than the mirrored assignment's 0.25 (also measured, and worse).
22. **The LADDER tab has no best-snapshot thumbnail** (dropped 2026-09-16, the operator: give the width
    back to the rung names). It cost 160 of the stats column's 236 px for a picture, not the ratchet
    the tab is about; `LADDER_STATS_WIDTH` is 130 now (rollbacks, lifetime, last and the stall meter
    only), and the freed width plus the VT323 split in (20) is what lets all three columns show
    every rung name whole rather than the seven-character "Viridia…" deviation 8 used to describe.
    A name that still cannot fit — none of this build's do — gets a slow stepped marquee pan
    (`src/lib/marquee.ts`, `useLadderMarquee` in `LadderTab.tsx`) instead of an ellipsis, because
    the pixel cursor makes a rung's name the one thing on this pane a viewer is meant to read in
    full.
23. **The title strip carries a release version, last and quietest** (2026-09-16, the operator).
    `__STAGE_VERSION__` (`vite.config.ts`, `git describe --tags --always --dirty`; `dev` from the
    dev server) renders beside the mode chip in `--font-label` at the label floor, dim ink — for
    whoever is on call, not a viewer's read of the game. The string is only right when the build
    runs from the tagged checkout the release is cut from; see "Building a release" below.

## Things this page deliberately cannot do

- It cannot press a button. The feed is one-directional and the control API has no button
  endpoint; both are structural, not configuration.
- It cannot render text from outside the feed. Viewer display names arrive only inside a
  `FeedEvent`, are validated by the bridge before the sim call, and are re-validated here on
  render (`safeDisplayName`, one chokepoint, tested).
- It cannot generate prose. Every string on screen is a constant in `src/lib/labels.ts` or a game
  config, or a template-generated label from the service.
