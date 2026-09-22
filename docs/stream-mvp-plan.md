# Fly on Twitch: stream, theme, engagement plan

## Context

`~/flybrain` now holds the extracted neural core (76 tests green) and `docs/streaming-plan.md` holds
the VM/Twitch capture architecture. The next goal is the actual product: a 24/7 Twitch stream of the
fly playing Pokémon Red (then a Game Boy platformer), with a viewer-facing theme and audience
engagement. Decisions taken so far (2026-09-15):

- The fly really plays. All buttons always come from the network. Viewer influence, if any, goes
  through the two real neural inputs the model already has (PAM stimulation, reward pulse). Exact
  boundary still being workshopped.
- Two new dedicated Twitch channels, one per demo. They start unaffiliated: chat-command bot only
  until Affiliate unlocks channel points, polls, predictions.
- Game audio from the emulator (accepting copyright-mute risk on VODs; revisit if muted).
- Front end: React + shadcn/ui for the stream page and overlays (per kickoff playbook).
- Host: the host, 40 threads Xeon E5-2660 v3, 188 GB RAM (60 GB free), 5% busy. Two 8 vCPU VMs fit.
  Risk: Haswell single-thread is slower than the WSL laptop where the sim ran at 0.92x real time.

## Findings: audit of the current stream page (fly-plays-pokemon)

Structural bugs:
- `src/style.css:41-43` sit outside the closing `@media` block, so the mobile type scale applies
  everywhere: live page renders at 7 to 11 px. At Twitch player scales (0.88 theater, 0.70 desktop
  with chat, 0.31 phone) nothing reaches 14 px effective. Need 20 px+ source at 720p, or design at
  1080p with 24 px+ body.
- Footer declares 6 grid columns for 7 cells; CHECKPOINT wraps, leaving an empty band.
- Game Boy canvas is 2.62x (non-integer) scaled: row-thickness jitter the encoder amplifies. 4x
  (640x576) fits a 720p canvas and matches how Pokémon streams size the game.
- Dead space about a third of the frame; the brain panel wastes 25% (orthographic camera shrinks
  the cloud as the panel widens).
- Spike glow steps at the 20 Hz snapshot rate, not animated; A/B 85 ms pulses are often invisible
  (requirement WEB-03 "recent-action indicator" unmet).
- `#reward` footer label is sticky forever; a badge and a +0.05 explore tick get identical 2 s tint.

Comprehension: a newcomer cannot learn what this is (the one-line explainer exists only as a
`<meta description>`), where the fly is, whether it is learning (all-zero boot state looks broken),
or what just happened (no event feed). Jargon on screen: DESCENDING, UPDATES, MEAN Δ, NETWORK Hz
(reads as internet), bare `16384`, operator status vocabulary.

Already shipped in `WorkerSnapshot` but unused (usable with zero worker changes): `rates` per role,
`rewardStats.recent[8]` (event feed), `rewardStats.mode` (BOOT/OVERWORLD/BATTLE...), `learning.
enabled/signal`, `status`. Needs new protocol fields: ratchet rank (0-15 milestone ladder already
computed in `src/runtime/ratchet.ts`), map id/area name/coords, badge count, wall-clock uptime,
per-direction decoder scores and fatigue (the most legible "fly is deciding" visual available).

Theme today: lab-terminal brutalism in Press Start 2P at one weight, hairline 1px borders that
x264 erases, hardcoded palette, no CSS variables. Consistent, but an instrument panel, not a
broadcast graphic.

## Research: stream landscape (dossier delivered 2026-09-15, 60 sources; full text in agent output)

Case-study lessons:
- Twitch Plays Pokémon: retention came from myth accumulation (Helix Fossil, Bird Jesus), names and
  artifacts, not production. Still running since 2014 on emulated Pokémon; Twitch itself celebrated it.
- Claude Plays Pokémon: layout B (reasoning panel left, game right, party strip bottom). Slow is fine
  if legible; visible internal state + visible long-horizon goal held ~100-120 avg viewers for 15
  months, peak 2,598. Weeks-long stalls were the most discussed content.
- Gemini Plays Pokémon: layout evolved toward CONSOLIDATING panels (tabs), added token counter,
  live diffs, public repo. Author's lessons: transparency builds investment; mishaps beat competence;
  milestone tracking creates arcs; disclosed "dev interventions" rather than hiding them.
- Fish Plays Pokémon / Mutekimaru: the viral moment was an accident with real authority (fish bought
  eShop credit). Operational rule: the agent's input surface must never reach beyond the emulator.
- Nothing, Forever: 14-day ban for generated text. Our fly cannot talk; any chat bot we write can.
- DishBrain / CL1 Doom / rat neurons Doom: episodic, none is a 24/7 channel. "First continuously
  streaming connectome" is the clean claim. Public already knows "stimulus in, spikes out, reward tone".
- Infinite Sugar: name-the-fly entry ritual; labelled channels (proboscis, wings, PAM dopamine);
  blue/pink/yellow sensory/internal/motor; limitations stated on the page.
- Lofi Girl: one warm unchanging character-centred frame held 20,843 hours. Do not redesign monthly.

Layout recommendation: B with mechanism inset. Game ~55% left; right rail = named-circuit activity
bars + "what the fly sees" retina raster (top), reward event ticker (middle), goals/badges/run clock
(bottom); connectome map as corner inset, promotable for clip moments. Language analogs for the fly:
retina raster, named-circuit bars, sparse reward ticker, per-button "pressure" bars (anticipation),
colour-coded map with legend.

Twitch mechanics (Sept 2026): "Monetization for All" (2026-05-13) opened Channel Points, Bits, subs
to eligible channels WITHOUT Affiliate; Affiliate thresholds now 4 h streamed, 4 days, 3 avg viewers,
25 followers in 30 days. Polls, Predictions, Hype Train still Affiliate-only. Day one: anonymous chat
read, EventSub chat, bot send (needs user:bot + channel:bot scopes), hosted bots, custom overlays,
stream markers. PubSub is dead; EventSub only. Redemption EventSub needs the broadcaster token.

Content rules: emulated Pokémon = low-to-moderate DMCA risk concentrated in VODs/clips; never show
setup or link ROMs; keep platformer as pivot. Game music: Twitch mutes VODs/deletes clips on
recognition; research recommends mute + spike sonification (the operator chose game audio; revisit if
muted). 24/7 allowed if genuinely live and original; strict AutoMod, human mods, never render raw chat
into the frame. Category: Pokémon Red/Blue for discovery. 48 h cap confirmed.

Engagement ideas ranked (honesty-safe first): name the fly and its Pokémon; milestone timeline;
stuck-o-meter (highest value per hour, cheapest); predictions on milestones (Affiliate); daily recap
clip via automated stream markers; fly cam retina raster; science explainer rotation; permanent
"what is real / what is scaffolding" panel + public repo. Managed-risk: "sugar" Channel Points PAM
stimulation (cap, log with redeemer on screen, one-line honesty statement). Defer: voting on reward
weights, chat-volume-as-noise (only as a labelled scheduled segment).

Theme recommendation: B scientific instrument as the base (legibility, credibility) + A CRT phosphor
warmth as colour/texture + D Game-Boy-UI panel ergonomics (no Nintendo assets/fonts/names) + C
microscope/petri feel for the connectome inset and thumbnails. E terminal-dense as accent only.

Numbers: 720p30 3000 kbps CBR 2 s keyframes. Text floors at 1280x720: body/ticker >= 18 px, labels
22-26 px, hero numbers >= 48 px, backing behind text over the map; x1.5 at 1080p. 48 px safe inset;
nothing load-bearing bottom-left. Mobile share unclear (14% to 50% by source): design for 3-4 big
readouts. Success calibration: 100-120 avg viewers is a hit in this genre.

## Decisions recorded 2026-09-15

1. **Option 2: the browser is a display only.** The simulation runs as a service; the page renders
   what the service sends over a local WebSocket. Reasons: sim survives browser/encoder restarts,
   no tab-throttling exposure, bridge talks to the sim directly, headless testability.
2. **The service is multithreaded Rust.** Measured: Node reaches 0.92x real time on a Ryzen
   5800X3D; the host's E5-2660 v3 has roughly half the single-thread speed, so Node would land near
   0.4-0.5x. Rust with rayon over the per-ms neuron sweep, SIMD, binjgb linked natively as C.
   The TypeScript library in `~/flybrain/packages/brain` stays the reference implementation and
   oracle; the Rust port gets cross-language golden tests (seeded input, N ms, byte-compare state)
   and keeps the pinned version strings while bit-exact.
3. Capture: Xvfb + headful Chromium + ffmpeg x11grab, PulseAudio null sink for audio, one LXC per
   demo (8 cores, 8 GB, nesting on). OBS deferred. Brain map drawn on 2D canvas (static base image
   + recently fired neurons) to avoid SwiftShader WebGL.
4. Two dedicated Twitch channels; React + shadcn/ui for the stream page; game audio from the
   emulator (accepted VOD-mute risk; sonification remains the fallback).
5. **Viewer influence: sugar only, for now.** Channel Points (or `!sugar` before the channel is
   set up) fires a timed PAM stimulation pulse via the control API; transient, no synapse change,
   rate-capped, every pulse shown on screen with the redeemer's name. Everything else is
   observational: naming, milestones, stuck-o-meter, predictions later. Parked for a later phase:
   a chat-voted "fly / democracy" slider that blends WHO TRAINS the fly (game reward catalog vs
   chat votes as reinforcement), never who presses buttons. Buttons are always the fly's.
10. **The emulator lives in `flysim`**, not the browser: binjgb linked natively as C, frame-
   synchronous WRAM sampling, retina input and button writes inside the sim loop, emulator state
   inside the checkpoint envelope. The page receives finished frames and PCM only; no ROM, no
   emulator, no input path in the browser.
6. **Process topology per demo container:** `flysim` (Rust: brain, plasticity, decoder, emulator,
   reward adapter, checkpoints, milestone archive; WebSocket snapshot feed at 30 Hz; localhost HTTP
   control API with stimulate/reward/status/checkpoint/pause and deliberately NO button endpoint),
   `flystage` (React + shadcn static page in Chromium on Xvfb, 1280x720, 4x integer game scale, 2D
   canvas brain map), `flybridge` (Node + twurple: chat commands, Channel Points redemptions,
   later predictions and markers; every action echoed to chat and the on-screen ticker), `flycast`
   (ffmpeg x11grab + Pulse capture, systemd, 23 h restart timer), watchdog unit restarting only the
   failed unit.
7. **MediaMTX** runs permanently in each container as a local RTMP/HLS/WebRTC ingest. Phase 0 gate:
   VLC plays the stage from MediaMTX with production ffmpeg flags. Twitch is one env var away.
   Also record 10-minute files for frame-by-frame legibility and A/V sync inspection.
8. **Audio is played by the page.** The feed carries emulator PCM; `flystage` plays it through Web
   Audio into a PulseAudio null sink; ffmpeg captures video and audio from the same display and
   sink so sync is the browser's. Reward-triggered sound effects and any future sonification layer
   live in the page too. Precedent: TPP, Claude and Gemini streams all broadcast game audio; the
   exposure is the ROM, not the music; VOD mutes and clip deletions are the only music risk.
9. Daily recap is cut from our own rolling recording in the container, not from Twitch clips.

11. **Scope: MVP first** (the operator, 2026-09-15). One container, Pokémon only, the fly live and legible
   on Twitch with `!sugar`. Everything else (second demo, predictions, recaps, milestone archive
   UI polish, sonification, slider) is a later phase and is listed only as "after MVP".

## MVP implementation plan (draft; detail from the three design agents to be folded in)

Definition of MVP: the fly plays Pokémon Red live on one Twitch channel from one LXC on the host,
the page is legible on a phone, viewers can `!sugar` in chat and see it happen on screen, the
stream survives crashes and restores from checkpoints. Nothing else.

Repo: everything lands in `~/flybrain` on feature branches merged with --no-ff, no AI attribution.

Workstreams (parallel where independent; agent model in brackets, never fable):

W1 `services/flysim` Rust port [opus]
  - Crates: flybrain-core (dataset .binz loader, LIF, plasticity, decoder, agent loop, envelope),
    flybrain-gb (binjgb C FFI, Pokémon Red reward adapter, ratchet), flysim binary (loop, WebSocket
    feed, HTTP control API, checkpoints).
  - Golden tests against the TS oracle: a `tools/golden.ts` in packages/brain dumps state after
    seeded sequences; Rust byte-compares. Deterministic multithreading (partition neuron sweep by
    index; spike propagation partitioned by target range to preserve addition order).
  - Control API has stimulate/status/checkpoint/pause only. No button endpoint. Reward endpoint
    compiled in but disabled by config.
  - MVP cut: no milestone archive UI, no metrics beyond /status, no speed control.

W2 `apps/stage` React + shadcn page [opus for shell, sonnet for panels]
  - Fixture player mode first (recorded feed), 2 mockup PNGs at 1280x720 for the operator sign-off
    (kickoff playbook gate), then wire panels: game 4x, circuit bars, retina raster, event ticker,
    milestone ladder + stuck timer, sugar indicator, title/explainer strip, 2D canvas brain inset,
    button afterglow. Web Audio playback of feed PCM + sugar/milestone SFX.
  - Text floors enforced by a Playwright lint (>= 18 px). Screenshot tests on the fixture.

W3 `services/bridge` Node + twurple [sonnet]
  - EventSub chat, `!fly !how !stuck !sugar` with per-user and global rate limits, template replies
    only, periodic explainer, POST /stimulate to flysim. Channel Points "Sugar" reward creation +
    redemption handling once the channel exists. Tests with a fake sim and Twitch CLI mocks.

W4 `infra/` [sonnet, with the operator running the host-side steps]
  - `pct create` script for the release container fly-pokemon (Debian 13, unprivileged, nesting, 8 cores, 8 GB),
    package install, `fly` user, systemd units (xvfb, pulse null sink, mediamtx, flysim, flystage
    chromium kiosk, flybridge, flycast ffmpeg with tee to MediaMTX + Twitch + local segments,
    flycast-restart 23 h timer, watchdog timer, nightly backup to the backup host), secrets via 0600
    EnvironmentFile from `pass`. Runbook. Infra-repo update.

W5 `packages/feed` shared TS types for the feed protocol [sonnet], consumed by stage and bridge;
   Rust side mirrors it with serde and a JSON schema test.

Order and gates:
  G0 spike: build flysim M1-M2 and measure real-time factor on the host in a throwaway CT
     (go: >= 1.0x on 6 cores; else GPU/kernel work before anything else).
  G1 mockup sign-off (the operator picks a PNG).
  G2 local end-to-end: flysim + stage + flycast to MediaMTX, VLC plays it, 10-minute recording
     inspected for scaling, legibility, A/V sync. Restore drills pass (kill -9, corrupt latest).
  G3 Twitch test channel live 48 h, bridge `!sugar` visible on screen.
  G4 public channel.

After MVP: fly-mario container, predictions, daily recap, sonification, fly/democracy slider,
milestone archive browser, rack-panel status, extension.

## Verification

- `cargo test` golden equivalence on toy and real dataset; `npm test` in packages/brain stays green.
- Playwright: stage screenshots at 1280x720, text-size lint, downscaled legibility check.
- Bridge unit tests with fake sim; Twitch CLI `event trigger` for redemptions.
- Container: 48 h soak against MediaMTX with watchdog logs; restore drills; VLC and phone playback.

## Decisions 2026-09-15 (evening)

- Theme: **T1 Instrument** (amber on slate) is the default; T2/T3 remain selectable by `?theme=`.
- Big moments promote the connectome **over the right rail**, never over the game.
- Local end-to-end smoke test passed (see infra/docs/p0-measurements.md): 1.0x real time at 6
  threads on the laptop, 30 Hz feed, 0.45 core for Chrome, 0.31 core for ffmpeg, restore in 0.21 s.
  Audio and MediaMTX were not testable on WSL (no PulseAudio); they move to the spike CT.
- The operator approved a throwaway spike container the dev container on the host for the P0 measurements.
- Spike runs with its rootfs on the bulk array (the SSD pool was nearly full; reclaiming it is the operator's
  call later). P1 sizing on the SSD pool stays open.
- **Broadcast canvas moves to native 1920x1080**: game at 5x (800x720), a ~220 px strip under it
  for a small 3D fly facing the screen whose legs, steering, proboscis and head glow are driven by
  the real motor/descending/proboscis/PAM role rates, and whose front legs tap a small Game Boy's
  buttons from the command roles. Twitch target becomes 1080p30 at 6000 kbps. See
  docs/design/fly-avatar.md.
- Copy direction from the operator: terse, instrument-like; explanations only in the rotating card.
- Built and merged 2026-09-15 late: 1080p canvas, 3D fly (webgl default, paper fallback,
  `?fly=`), terse copy, adaptive circuit bars, rail overlay. Spike on the host cut short for rack
  work: nesting OK, brain 1.82x at 6 threads (1.89x at 4; CT cpuset spanned both sockets), full
  stack up under systemd; open: ffmpeg dup/drop ~4/s, lag 1.0 s under systemd, audio/AV/soak/
  SwiftShader untaken. Eight infra bugs fixed by the spike. SSD pool headroom recovered (models evicted).
  Host work PARKED until the operator finishes in the rack.

## Rail layout v2, locked 2026-09-15 night (the operator)

Right rail (1012 wide), top to bottom: compact progress cluster 144 (rung name → next, 38-rung
spine, badges · places, HERE FOR + try, BRAIN Hz, clock · day, SUGAR READY chip); one tabbed
slot 420 with three tabs auto-cycling (SENSES = retina + circuit bars side by side, CONNECTOME,
LADDER = full ladder list + best snapshot thumbnail + rollback budget + stall meter), slow cadence
steered by activity, big moments pre-empt for 9 s; EVENTS ticker 100 (3 lines); persistent CHAT
244 (last ~7 lines, AutoMod-passed, service-sanitized, deny-listed, kill switch). Title strip
without the neuron count or the LEARNING chip; mode chip in plain words (WALKING, BATTLE, MENU,
BOOT, DEMO). No explainer card anywhere. Fly strip: no Game Boy prop; limbs and wings driven by
motor/descending rates; button row along its top edge.
Mockups: <artifact link> (revision 5).
Next: animations and pizzazz plan (the operator + Fable), then build.

**Amended 2026-09-17 (the operator):** a fourth tab, DESCRIBE, after LADDER in the rotation and the strip
— "add a describe tab where we describe the project. this needs manual review from me for copy."
It supersedes "No explainer card anywhere" above: the explaining happens on one tab, one card at a
time, and nowhere else on the page. Brief and draft copy in `docs/design/describe-tab.md`; the copy
itself lives in `apps/stage/src/games/describe.ts`, one entry per card, and is **pending the operator's
review**.

## Hard rule (the operator, 2026-09-16): no going live without explicit approval

The Twitch stream key is in `pass twitch/<channel>-key` as of 2026-09-16. `flypush.service` stays
disabled and no agent enables it, pushes to any RTMP host other than the container's own
MediaMTX, or changes the channel's live state until the operator explicitly approves going live for that
specific run. Local MediaMTX demos are fine.

## Queued (the operator, 2026-09-16): VirtualGL EGL spike

After the new-fly demo lands on the dev container: try VirtualGL 3.x's EGL back end (`vglrun -d egl0
chromium ...`) so Chromium renders WebGL on the Quadro and blits into Xvfb, keeping x11grab and the
shared audio clock. Needs `/dev/nvidia-modeset` bound into the CT, VirtualGL installed in the CT,
and the `chromium-flags.gpu` profile. Pass: `chrome://gpu` reports hardware WebGL with
`GL_RENDERER` naming the Quadro, the WebGL fly holds 30 fps, total Chromium CPU drops, no
renderer-sandbox failures over an hour. Fail: revert to the paper fly; no other change. See
docs/design/gpu.md section 3(d).

## Release container (the operator, 2026-09-16)

Once the Game Boy theme pass lands: reset the game (fresh state) and prepare to stream from a
dedicated release container, the release container `fly-pokemon` (provision.sh <release-env>, GPU=1,
cpuset, NVENC, onboot=1). The dev container stays the development container. Go-live still requires the operator's
explicit approval (hard rule above). Prep order: provision the release container -> deploy theme build -> verify,
watchdog, local MediaMTX, 1 h A/V check -> bridge on the channel while offline (needs dev-app creds
and channel name in pass) -> channel setup (title, category, About, AutoMod, bot mod, Sugar reward)
-> approval -> enable flypush.
- The operator 2026-09-16: the first release is CPU-only. The release container gets GPU=0 and FLY_ENCODER=x264; no
  NVIDIA passthrough on the release container. NVENC stays on the dev container (the dev container) only.
- The operator 2026-09-16: the release container runs TAGGED commits only. Tag on main (`vX.Y.Z`), the
  release directory under /opt/fly/releases is named after the tag, `05-deploy.sh` refuses an
  untagged tree for the release container, and the tag is recorded in the claim-log deploy line.
- 2026-09-16: Twitch channel for demo 1 is `<twitch-channel>`; dev-app client id/secret and the
  stream key are in `pass twitch/`. Bot account still to be created (bridge can start as the
  broadcaster for the first test).
- The operator 2026-09-16: no more mockup review rounds; every stage merge is redeployed to the dev
  container (the dev container, stage-only, flystage restart) and reviewed on the live stream. Tagged
  releases to the release container unchanged.

## Go-live approval (the operator, 2026-09-16 night)

"allright, verify you have keys. perform your checks, go live when you're ready, you have approval.
i'm going to bed." Scope: channel `<twitch-channel>`, release box the release container on `v0.1.0`, CPU-only.
Conditions applied: verify.sh green, stream key installed, HLS verified locally, Helix confirms
the channel live, watchdog and 23 h restart timer active, bridge only if its tokens exist.
- Blocked overnight 2026-09-16: `pass` cannot decrypt without a terminal (GPG key is
  passphrase-protected, no cached passphrase, no pinentry in the agent's shell) and the bridge
  tokens were never authorized. Everything else proceeds; going live needs the operator to run, from his
  prompt: `! FORCE_SECRETS=1 infra/06-secrets.sh <release-env>` (installs the stream key),
  then `! ssh the host pct exec <release-ctid> -- systemctl enable --now flypush.service` (goes live), and
  optionally the bridge authorize command for chat/sugar.
- **LIVE 2026-09-16 05:55:19 UTC** on twitch.tv/<twitch-channel> from the release container (`v0.1.0`, CPU-only
  x264). Stream key and dev-app creds installed via 06-secrets after the operator unlocked pass; flypush
  enabled by the operator; Helix `GET /streams` returned type=live. Title and category are still empty
  (no broadcaster token yet); bridge (chat/sugar) not authorized yet.

- **v0.1.1 readout fix, 2026-09-16.** The release fly spent forty minutes on rung 2 (DOWNSTAIRS).
  Diagnosed from its own generation-525 checkpoint, pulled read-only: with the four direction
  scores inside a 8.6% spread and hysteresis at 1.15, no direction could win on score, so the
  readout rotated through a frozen preference order in which `down` -- the only press that opens
  the front door -- came last, and 59 to 65% of its 800 ms holds moved it not one tile. Fixed by
  `hysteresis` 1.05, `fatigueGain` back to 0.08 and a new blocked-direction cooldown: 14 of 15
  runs out of the house within ten brain minutes against 1, and 13 inside five against 0. Details
  in `docs/design/room-escape.md` section 3 and `infra/docs/room-escape.md`. Tagging and deploy
  are the operator's; the compatibility string did not move, so the live run restores onto it.

## Action items after go-live (the operator, 2026-09-16 morning): "get prod working, chat, then the list"

1. Chat on prod: bridge deployed on the release container with the broadcaster+bot tokens (in progress).
2. Widen the release container to all ten node-1 cores (sim 4 / page 3 / encoder 3), one container restart.
3. Watchdog process-age guard + three-way CPU partition rule in 05-deploy (in progress).
4. NVENC on the release box: decision deferred (the operator chose extra cores first); dev stays NVENC.
5. Sugar channel-points reward creation and the sugar loop proven live.
6. Bot account (separate from the broadcaster), then re-authorize `--role bot`.
7. The operator's git remote: create `<owner>/flybrain` via API and push (needs token); Git LFS decision for the 35 MB
   of fixtures (now 22 MB + 12 MB after the spike-rate re-record).
8. the router reservation the reserved address for the release container; the operator's infra repo docs already updated.
9. 60-minute A/V drift measurement on the release box; VirtualGL spike on dev (3D fly on stream).
10. Second demo: Super Mario Land ROM hash pin, the platformer container provisioning, second channel.
- 2026-09-16 06:30 UTC: chat live on prod (bridge up, startup notice posted; Channel Points need
  Affiliate after all, so sugar is chat-command only for now). Repo pushed public:
  <operator-git-remote> (main + v0.1.0). v0.1.1 rework started: fly stuck on
  rung 2 (house 1F) 40+ min despite exit rewards.
- 06:36 UTC: the release container widened to ten node-1 cores (sim 1,3,5,7 / page 9,11,13 / encoder 15,17,19),
  39 s restart. After: unchanged captured frames 3/57 (was 73/115), sim lag 0, rtf 1.0+, encoder
  0 dup/drop. Bridge merged (fix/bridge-deploy). Twitch broadcast restarted at 06:36:11 UTC.
  **CORRECTION (2026-09-16 pm): the 3/57 and 73/115 readings are junk** — they came from a second,
  independent `x11grab` of `:99`, which measures the display, not the stream. The display was fine;
  the broadcast was frozen from the 06:35:52 boot until a manual flycast restart at 10:26
  (174-177 identical frames of 180 in every recorded segment in between). `infra/docs/capture-freeze.md`.
- VirtualGL spike PASSED on the dev container (3D fly in WebGL on the Quadro under Xvfb, Chromium CPU 1.38 ->
  1.06 core). Requires --disable-gpu-sandbox for Chromium's GPU process; release envs stay on the
  paper fly until the operator accepts that trade and a multi-hour soak with the GPU workload in the neighbouring container load passes.
- 09:39 UTC: v0.1.1 deployed to the release container (compatibility gate COMPATIBLE, no reset; flysim, flystage
  and flybridge restarted, flycast and flypush not, no broadcast gap; chip v0.1.1; rank 5
  preserved). The live fly had already left the house on v0.1.0 at 06:59 UTC and reached GOT A
  STARTER at 08:36 UTC; the v0.1.1 decoder is now what it plays on. Record:
  `infra/docs/release-v0.1.1.md`.
- Owed from the deploy: `05-deploy.sh` runs its in-container work through `pct exec` on the whole
  cpuset, so extraction and MANIFEST verification competed with the sim (lag 3 s -> 5 s during
  the run). Pin those steps to the page cpus with taskset; never re-run the deploy on the release container as
  an idempotency check. Also: the on-screen chat ring is not checkpointed (panel empties on a
  bridge restart), and `flybridge.service.d/override.conf` is now redundant.
- Perf finding (release box, 09:50 UTC): flysim's main thread is at 100% while the three pool
  workers idle at ~70%; realtime factor oscillates 0.82-1.10 all morning independent of deploys.
  The serial part of the loop is the ceiling, not the sweep or the core count; profiling branch
  `perf/simloop-main-thread` in progress (local only, bit-exact).

## v0.2: the release box gets the card (the operator, 2026-09-16 ~10:50 UTC)

Asked whether release should get the Quadro in v0.2, reversing "CPU only for the first release":
"if so. then yeah. 0.2". Scope for v0.2, in order: NVENC in flycast on the release container (device block from
docs/design/gpu.md section 1, shared card, x264 fallback stays), the CUDA LIF tick if the spike on
`spike/lif-cuda` proves bit-exact with `lif-1ms-f64-v2` (same kernel string, live checkpoint carries
over; if not bit-exact it is a separate backend and a separate decision), then the WebGL fly via
VirtualGL behind a nightly Chromium restart (soak showed GPU-process memory growth). v0.1.2 (capture
freeze fix, watchdog probe, deploy pinning) ships first and stays CPU-only.

- 2026-09-16 pm: **capture freeze found and fixed** (`fix/capture-freeze`, local only, for v0.1.2).
  A pulse client attaching during flycast's first 1-3 s starves its x11grab leg for the life of the
  process — 3 h 50 min of frozen broadcast on the release container, six reproductions on the dev container, audio and
  `frame=` perfect throughout. Fix: `flycast.service` is `After=flystage.service` with a new
  `wait-for-stage` ExecStartPre (X up, kiosk window mapped, sink-input present, held 5 s; 120 s
  then start anyway) and `TimeoutStartSec=180`; `fly-watchdog` check 9 probes the newest segment
  every 5 min and restarts flycast after two bad probes, at most once per 30 min; `05-deploy.sh`
  pins extraction/verify/chown to the page cpus. Measure freezes on the ENCODER OUTPUT only — the
  earlier 3/57 reading was taken with the wrong instrument. `infra/docs/capture-freeze.md`.
- 11:40 UTC: `spike/lif-cuda` merged (feature `cuda`, off by default, `FLY_LIF_CUDA=1` to attach).
  Bit-exact with `lif-1ms-f64-v2` over 10,000 ticks on the real dataset (every field, every
  tick), golden suite passes on the GPU, so the kernel string and live checkpoints carry over.
  GPU tick 316 us (propagation 290 of it). On the dev container, four pinned cores: CPU 4 threads 0.79x
  (contended, 2.4 cores obtained), CUDA 1.53x on 1.02 host cores; the agent loop's serial
  main-thread work is now the whole ceiling. Record: `infra/docs/lif-cuda-spike.md`.
- 11:50 UTC: `perf/simloop-main-thread` merged. Profile (WSL, 4 threads/4 cores, real ROM, feed
  on): work 5.95 ms of a 16.7 ms frame; observe 1.39 ms (23%, serial) and the hot checkpoint's
  envelope encode (11.5 ms once per 5 s) were the sim-thread hogs. Observe is now sharded (bit-exact)
  and the encode runs on the writer thread: work 5.40 ms, brain-only soak 3.07x -> 3.58x, sim-thread
  CPU 36% -> 32%. Owed: fuse the observe and propagation dispatches (3-core case gains nothing yet);
  re-run the GPU golden suite on the dev container with `FLY_LIF_CUDA=1` now that observe is sharded, before
  v0.2 turns CUDA on. Record: `infra/docs/simloop-profile.md`.

- 2026-09-16 pm: **chat was dead for an hour on the release container and the bridge said `active`** (11:45:34 to
  12:47:13 UTC). One 1006 disconnect, Twitch refusing the re-created `channel.chat.message` with
  "websocket transports limit exceeded" (two sockets from one account against a limit of 3), and
  twurple never retrying a failed create. Fixed on `fix/bridge-eventsub-resilience`, local only,
  for v0.1.4: one EventSub socket when the bot and broadcaster roles are one account (scope-routing
  auth provider), a 60 s `EVENTSUB_GRACE_MS` watchdog that exits 75 so systemd restarts with a
  fresh transport (`RestartSec=15`, no start limit), `chatSubscriptionHealthy` in `/health`, and a
  rate-limited startup/recovery notice. `docs/design/stage-bridge.md` B5, runbook "chat dead,
  bridge active".

## v0.2 plan (Fable, 2026-09-16 11:50 UTC; the operator approved the card for release)

Gates in order, each measured on the dev container before release sees it:
1. `v0.1.3` (CPU-only, no infra change): the merged perf branch. Deploy to the release container with a flysim
   restart from its hot checkpoint (one-second feed gap, no Twitch gap). Watch lag drift for an
   hour; expect the 0.3 s/min drift to fall.
2. GPU on the release container: section 1 device block from `docs/design/gpu.md`, userspace driver in the CT,
   one container restart (Twitch gap, and the first real run of `wait-for-stage` at boot: the
   capture must wait for the page and the output must be checked on the HLS, not the display).
3. `v0.2.0`: `FLY_ENCODER=nvenc` with the x264 fallback, `FLY_LIF_CUDA=1` in the release env
   (compatibility string unchanged, checkpoint carries over), cpuset re-partitioned (sim needs one
   core plus the GPU; give the page more). Gate: GPU golden suite green on the dev container, 3 h soak on the dev container
   with lag flat and GPU memory flat, output freeze probe clean.
4. WebGL fly via VirtualGL: only with a nightly Chromium restart, after a 12 h soak; separate tag.

## Queued (the operator, 2026-09-16 ~13:00 UTC): scene-appropriate macro palette

The operator, watching the release stream: random button pressing "doesn't seem to get anywhere"; the
ratchet only retains progress. Proposal in his words: go through the game and, for each scene,
identify the set of actions a player would take (overworld: go to an objective, interact; battle:
attack, switch; shop: buy), and give the fly, "in addition to the buttons", a scene-appropriate
macro it can hit instead of flailing. "We have the code, we have the binary, we understand the
state of the game": statically analyse the scenes from the pokered decomp and build a dynamic
palette of macros. Queue for execution after the v0.2 gates.

Fable's assessment (to be turned into docs/design/macros.md when it starts):
- Worth doing, and the strongest argument is learning, not progress: button-level rewards are
  too sparse and too far from the decision for KC->MBON plasticity to attribute anything. A
  macro is a decision with a reward one step away, in a scene the retina can distinguish, which
  is the first setting where the honesty panel's "learning" could be shown to do something.
- It is a readout change, not a button path: the same motor population groups that today mean
  UP/DOWN/A/B mean, in a scene, "macro slot 0..K"; the sim executes the chosen macro as a button
  script (pathfinding on the collision map, menu navigation from WRAM). The fly still decides,
  nothing is chosen for it, a timeout means no action, raw-button mode remains selectable and the
  mode is shown on screen. Doctrine sentence changes from "the fly presses the buttons" to "the
  fly chooses the action"; disclosed on the honesty panel with the palette visible.
- Scenes from WRAM: overworld, dialog, battle (own turn / forced switch), menu, shop, PC, title.
  Palettes small (3-6 slots). Overworld macros: walk to nearest unvisited warp or route exit,
  talk to nearest NPC, interact with the tile ahead, advance dialog. Battle: attack with best
  damaging move, switch to the healthiest, use item, run. Shop: buy potions/balls if money.
- Risks: it reads as a scripted bot with a random seed if the palette is rich or a default
  action exists; keep palettes small, never default, show the choice. Ladder rungs unchanged.
- Sizing: adapter scene detection + macro executor + decoder mode + panel + ladder audit; plan
  with fable, build with opus agents, dev box first, measured as "rungs per hour" against the
  current run from the same checkpoint.
Open decision for the operator: the doctrine wording on the honesty panel ("chooses the action").
- 13:10 UTC: v0.1.3 live on the release container since 12:03 (flysim restarted from hot checkpoint, rank 5
  kept, no Twitch gap). Lag drift over the next 60 min: 0.000 s/min (v0.1.2: 0.313). Record:
  `infra/docs/release-v0.1.3.md`. Debts noted there: `systemctl status flypush` prints the stream
  key in ffmpeg's argv (add `ProtectProc=invisible` to the unit; never paste that output);
  measure deploy cost with two samples either side of the single 05-deploy call, before restarts.
- 13:35 UTC: v0.1.4 live (bridge self-heal, one EventSub socket, flypush process hiding). Deploy
  cost to the sim 0.000 s with the pinned deploy; flysim untouched. The supervised flypush restart
  ENDED the Twitch session (4.8 s ingest gap, new session 13:23:18 UTC), so `flypush-restart.timer`
  ends the session nightly: owed, decide between a longer timer, a reconnect that keeps the session
  (Twitch tolerates ~90 s), or accepting the nightly split. Record: `infra/docs/release-v0.1.4.md`.
- 14:40 UTC: macro palette merged on main behind `[macros] mode` / `FLY_MACRO_MODE` (default raw;
  raw trajectory byte-identical to before, compatibility string unchanged; feed field is
  `game.macroMode` because `game.mode` was already taken). Smoke bench, 1 brain hour, fresh
  boot, one seed: palette leaves the house 11x faster (Pallet Town at 0.013 brain hours vs
  0.148) then stalls in Oak's lab at rung 4 while raw reached rung 5; palette covers 244
  locations vs 141 and earns 6.65 vs 9.20 reward/hour; 63% of palette frames press nothing.
  Read: the lab needs a "go to the nearest interactable object" macro (the ball is not an NPC)
  and GO EXIT should prefer unvisited exits (ledger accessor not wired). Not the gate
  (6 h x 3 from the live checkpoint). Record: `infra/docs/macros-bench.md`.

## Next sprint (the operator, 2026-09-16 18:20 UTC): move PII to the infra repo, purge it from this repo

flybrain is public; anything that identifies the operator's own network (hostnames, LAN
addresses, container ids, host paths, account ids, people's names, the agent claim-log
protocol, `pass` entry names, release records with box details) belongs in the operator's
infra repo, not here. Steps, in order:
1. Inventory: an agent greps the tree AND the history for those patterns and writes the list
   with file:line and a keep/move/redact verdict per hit. Done — inventory in the operator's infra repo (`de-pii-inventory.md`
   has the pattern classes, the totals, and the rule applied per file.
2. Split: `infra/env/example.env` become `infra/env/example.env` here with real envs in the infra
   repo; release records, room-escape/capture-freeze measurements and the runbook keep their
   method here with the box specifics moved out and linked by name only; docs refer to "the
   host" and "the release container". Done.
3. History: rewrite with `git filter-repo` (paths and replace-text), re-create the annotated
   release tags on the rewritten commits, force-push main and tags, then re-clone the deploy
   checkouts on the host. The tag gate only needs the tag to exist; MANIFEST git_commit values
   of past releases become historical. NOT DONE — the paths and the replace-text list it needs
   are at the end of that inventory.
4. Guard: a lint check that refuses the step-1 patterns in any new commit. Done —
   `infra/tests/lint.sh` section 9, with `infra/tests/de-pii-allow.txt`.
Do not start before v0.2.1 (plan mode) ships.
- 18:40 UTC: plan mode merged; release env set to `FLY_MACRO_MODE=plan` for v0.2.1. Smoke: plan
  reaches Oak's lab at 0.005 brain hours (palette 0.249, raw 0.156) and takes the starter once
  Oak's script has run. Owed next: rung 5's objective place must be the north edge of Pallet Town
  (Oak's trigger) until the script flag is set, then the lab; 23 of 38 rungs still have no place.
- 20:30 UTC: biased slots merged (section 10, min-max fly votes). Smoke, plan (biased): lab at
  0.025 brain hours (raw 0.156, palette 0.249); 14% of decisions taken off the plan's head in
  the overworld, none in dialog; 440 walks interrupted by the fly. Owed: rank 1 never wins; rung
  5 objective place; starter leg not in the ROM test. Shipping as v0.2.2.
- 21:55 UTC: town loop fixed (exits visited by destination map, talked ledger, 30 of 38 rung
  places, cross-map objective routing; rung 19 unknown). Shipping as v0.2.3 ahead of the
  section 12 simplification (macros are buttons, own populations), which follows as v0.2.4.
- 22:10 UTC: v0.2.3 live (town loop fix): the fly left Pallet Town onto Route 1 within a minute
  of the restart. Chat was down 22:06-22:10: the bridge validates the stored access token
  before refreshing it, so a broadcaster token that expired while the previous process held a
  fresher one in memory (persisted only for the bot role) failed startup with 401 in a crash
  loop. Recovered by refreshing from the backed-up refresh token with the app credential inside
  the container. OWED for v0.2.4: refresh-before-validate on load (or addUser by id), and
  persist refreshed tokens for BOTH roles when they share an account. One startup notice was
  posted at 22:10 despite the "nothing in chat for now" ask; unavoidable with a bridge restart.
- 22:25 UTC: live deadlock: a battle frame that is not the fly's turn bound no buttons, so the
  "Wild PIDGEY appeared!" text (which waits for a press) left the fly with an empty pad. Hotfix
  binds NEXT there; v0.2.4.
- 22:30 UTC: section 12 built (v0.2.4 candidate, the hotfix above merged into it). Every macro type has its own population,
  `macro_<type>`, cut round-robin from the 96 MBONs plus the 110 brain motor neurons and emitted
  into `circuit-roles.json`; the readout gains a second exclusive group over those channels, with
  the direction group's numbers and a per-decision mask of the scene's own buttons. The plan, the
  ranks, the priors, the blend and the `[macros.bias]` weights are gone; the modes are `raw` and
  `macros`. The artifact diff is additive to the byte (the first 150,440 bytes unchanged),
  `--print-compatibility` is byte-identical to the v0.1.3 release binary's (648 bytes,
  `0d9bfde7…707fa`), and the macro roles are outside the dataset fingerprint by construction.
  On the cartridge the stub readout reaches Oak's lab in 2.1 brain minutes on 61 macros, against
  3.5 minutes on 96 for the drive it replaces. Section 12's own gate -- the 6 brain hour bench,
  raw against macros, from the live checkpoint -- has NOT been run.
- 22:50 UTC: section 12 merged: macros are buttons on their own populations (22 `macro_<type>`
  roles from MBON + brain motor neurons, second decoder group with the direction rules, masked
  to the scene's buttons), modes `raw` / `macros`, blend and priors removed, MACROS rate row.
  Compatibility string unchanged. Release env `FLY_MACRO_MODE=macros`. Shipping as v0.3.0.
  Owed: the 6 h raw-vs-macros gate from the live checkpoint; bridge refresh-before-validate.
- 22:51 UTC: v0.3.0 live, macros mode (own populations). The fly climbed rank 5 -> 8 between
  22:28 and 22:51 under v0.2.4 (parcel fetched and delivered) and is in Viridian City. Screen
  calls open for the operator: no gloss column in the macro cells now (tag + name), MACROS rate row as
  two columns of three. Owed: raw-vs-macros gate; bridge refresh-before-validate.
- 23:55 UTC: blocked- and reached-target ledgers (section 12.1) merged; v0.3.1. Owed: a `no
  route` refusal records nothing and can be re-chosen once per hold.
- 01:50 UTC (09-17): Viridian loop diagnosed and fixed (timeouts on a wide map excluded the
  only exit toward the objective; door bounce; instant-complete macros). 23-row trap audit,
  15 fixed, 8 left with reasons; `examples/trap_hunt.rs` flags no-progress windows from a
  checkpoint. Record: `infra/docs/macros-traps.md`. Shipping as v0.3.2.
- 08:20 UTC (09-17): walks fixed (every Viridian walk had spent 600 frames trading two tiles:
  closest-approach goal + per-tile re-plans across the moving window). Budget scales with the
  route, walks resume, plans commit, refused steps become directed walls, unreachable goals are
  excluded. Trap hunt from the stalled checkpoint: 0 done / 44 timeout -> 1,129 done / 14
  timeout. Exposed next: a `dialog` reading at Viridian (19,9) the pad cannot close; agent on it.
  Shipping walks as v0.3.3.
- 08:40 UTC (09-17): the Viridian "dialog wall" is a real scripted gate on the road north at
  (19,9) (the game walks the player back), not a detector fault; the detector still gained a
  whole-border text-box test (315 of 43,004 frames had drawn four corner-lookalike ground tiles).
  Shipping as v0.3.4; agent on passing the gate (audit row 28).
- 09:30 UTC (09-17): the "gate" was Oak's parcel: carried, never delivered (rung 7 never
  earned; rank reads 8 because rank is the max satisfied rung and Viridian counts as visited).
  The objective is now the lowest UNSATISFIED rung (Oak's lab), and the still-sprite sign
  "This is private property!" push-back is a scripted refusal: talked entries are pending
  until the box closes unmoved, push-backs exclude the walk target, reached is a window.
  Trap hunt: text-box frames 41,012 -> 0, 1,332 of 1,335 macros done. Shipping v0.3.5
  (with the v0.3.4 detector fix). Owed: row 29, doormat bounce on the objective's own map.
- 09-17 (v0.3.6): row 29 fixed: objective places name the person/object that earns the rung,
  GO OBJECTIVE walks to it and faces it (then TALK is the pad's offer), the ways out wait while
  the target is reachable, `milestone.next` is the lowest unsatisfied rung. Stub from the live
  checkpoint delivers the parcel at 9 brain minutes (rung 7). Trap hunt: flagged windows 35 -> 6,
  GO OUT starts 321 -> 2. Shipping v0.3.6.
- 09-17 (v0.3.7): the macro channels were never calibrated on release (restored decoder state
  predates the roles; raw rates 27-145 Hz decided, TALK at 41 Hz never won). Roles without a
  baseline now calibrate on the first decode after restore, both twins, golden `restore`;
  `/status.decoder` shows baselines, scores, pending. Shipping v0.3.7.
- 09-17 (v0.3.8): bridge quiet mode on release (the operator: "speaks only when spoken to. doesn't
  greet"): no startup/recovered notice, no explainer rotation, no follow/raid thanks; commands
  and on-screen chat unchanged. Chat copy from 09-16 ships with it.
- 09-17: the release watchdog gained check 10, "loop suspected" (`infra/bin/fly-watchdog`,
  `infra/docs/runbook.md`): every 5 minutes it reads the last 10 brain minutes of `macro` events
  from `events.jsonl` plus `/status.json`, and flags a short cycle covering no new ground — at
  most 3 distinct macro names with the block repeating 20+ times, or one macro at 95% of the
  window, and in either case no growth in `game.uniqueLocations`. It exports
  `fly_loop_suspected`, `fly_loop_period`, `fly_loop_repeats`, `fly_loop_distinct_macros` and
  `fly_places_delta`, logs one line when it flags and one when it clears, and writes
  `/run/fly/wd/loop.json` (sequence, window, map, milestone) for a review agent to pick up. It
  never acts: no restart, no press, nothing about the game changes — a loop is a macro
  target-choice bug and the decision is a human's or a review agent's, not the watchdog's.
- 09-17 (v0.3.9): battle move list is the fly's turn (was: main menu only, so an open list got
  NEXT = A on a 0-PP move, 268 ms a press for 1 h 41 min); pads per battle sub-state; NEXT never
  over an open list. Watchdog check 10 (loop suspected, report only, distinct ceiling 4) ships
  with it. Owed: row 30c (battle bag has no observable), row 31 (talked everyone, rung unearned).
- 09-17 18:20 UTC (v0.3.10, loop review, auto): forest south gate stall (4.3 h, pad = GO FRONTIER
  only). Warp tiles never counted as visited (the reward ledger's sample gate skips door frames)
  and geography had one node for Route 2, so the hop toward Pewter pointed back at the door the
  fly came in by. Stood ledger, Route 2 split, passage toward the objective. Trap hunt: 16
  starved windows -> 0, 78 -> 193 tiles. Ethos check held on every item. Row 34 (all moves at
  0 PP deals a one-button pad) found, fix in progress.
- 09-17 (v0.3.11): DESCRIBE is one dense card with the operator's approved copy; a new chatter pins
  DESCRIBE for 8 s (120 s cooldown); row 34 (all moves at 0 PP) fixed: ATTACK stays on the pad
  and FIGHT ends the turn via Struggle.

## Parked (2026-09-17 ~22:50 UTC, weekly usage): resume from here

Live: v0.3.11 on the release container (macros mode, GPU, quiet bridge), rung 9. Main at
66a6025 has the pad strip + MACROS tab merged (not yet released). Open branches, each with a
worktree under .claude/worktrees/: `feat/macros-shops` (section 13, 14, 13.1, GO WARP, THROW
BALL, MOVE 1..4, pad audit, pad-empty metric; held for a dialog stall in the trap hunt),
`fix/loop-20260917T2214` (ratchet rollbacks undoing Route 2 progress; findings in branch docs).
Parked: the history purge (plan recorded by the publish coordinator; run only after the open
branches merge; env for deploys lives on the host, see the operator's infra repo), the stale-info
fixes (report in the coordinator session's scratchpad; 188 items), the floated embarrassment
list. The loop watcher schedule is stopped; the in-box watchdog check 10 keeps reporting.
Next release order: merge shops (after its stall fix), tag, deploy; merge the ratchet fix;
then purge; then the stale-doc pass.
- 2026-09-22 02:22 UTC (v0.4.1): first release cut from the public repository (fresh history at
  v0.4.0); the DESCRIBE card carries the repository URL. Deployed to the release container with a
  flysim and flystage restart, checkpoint carried over at rank 9, lag 0, encoder output clean.
- 2026-09-22 (v0.4.2, loop review, auto): rung 9 for 69 h. The between-turns battle row dealt
  BACK with no list open (175 of 959 starts, instant, net nothing) and THROW BALL threw at species
  the party already held. Fixed: BACK only where a list is open; THROW BALL skips held species.
  Trap hunt: tiles 216 -> 286, listless BACK 175 -> 0, dialog frames 23,548 -> 0. Ethos check
  held. Row 41 named: a nurse box answered YES 474 times on one tile.
- 2026-09-22 (for v0.4.3): map-aware walks merged (section 15): the whole loaded map is decoded
  into a walkability grid from the map and tileset data (read-only, ROM bank reads over the
  cartridge image), A* plans over the map, the frontier is the nearest unstood tile anywhere,
  the window reader is the fallback. Verified against real presses on two maps. Trap hunt: 286 ->
  489 tiles, timeouts 6 -> 1; flagged windows rose 61 -> 70, all battle windows (the battle pad
  review in flight).
- 2026-09-22 (v0.4.3, loop review, auto): the watchdog flagged NEXT/BACK repeating 373 times in
  ten brain minutes after v0.4.2. NEXT on the main battle menu confirmed FIGHT and opened the move
  list, whose BACK closed it: a pair that undoes itself. NEXT is now off every pad with an
  input-accepting cursor, MOVE 1 is the main menu backstop, the bag is the fly's turn. ROM test
  fails on v0.4.2 and passes here. Trap hunt: 1 -> 260 tiles, windows under four tiles 73 -> 2.
  Ships with map-aware walks. Ethos check held.
- 2026-09-22 (v0.4.3, loop review, auto): the watchdog flagged NEXT/BACK repeating 373 times in
  ten brain minutes after v0.4.2. NEXT on the main battle menu confirmed FIGHT and opened the move
  list, whose BACK closed it: a pair that undoes itself. NEXT is now off every pad with an
  input-accepting cursor, MOVE 1 is the main menu backstop, the bag is the fly's turn. ROM test
  fails on v0.4.2 and passes here. Trap hunt: 1 -> 260 tiles, windows under four tiles 73 -> 2.
  Ships with map-aware walks. Ethos check held.
- 2026-09-22 (v0.4.4, loop review, auto): rung 10 reached 06:49 UTC. In the Pewter museum's upper
  floor every list emptied (no geography row, exhibits reached, staircase blocked-windowed), leaving
  MENU alone; the menu scene's BACK undid it. MENU is off every pad; a stranded room offers its
  way out regardless of ledger windows. Also found: the battle menu is two columns, so ITEM opened
  the party list and SWITCH the bag since v0.4.0; fixed, THROW BALL now 15/0 in the forest run.
  ROM test fails on v0.4.3 and passes here. Ethos check held. Row 41 (a nurse box answered YES
  1,278 times) is next.
- 2026-09-22 (v0.4.5, loop review, auto): row 41. In the Pewter center the nurse's conversation is a
  ring of 46 A presses with one YES/NO choice; the dialog pad dealt NEXT and YES unconditionally
  (one press, two names) and TALK was bound over the counter but recorded one tile ahead, so the
  nurse never entered the talked ledger: YES x2,142. Fixed: a readable prompt deals its answers with
  NEXT off it, only the answer that changes something is bound, TALK is off at a rested nurse, the
  nurse is talked after a heal or a decline, a prompt that reopens unchanged is excluded. ROM test:
  leaves the center on frame 326. Hunt: 1 -> 437 tiles, YES 1,424 -> 4. Ethos check held.
- 2026-09-22 (session framework, wave 1): BUS slice merged. The flybus crate is audited section
  by section against bus-v1 (195 rows: 178 conform, 9 allowed deviations each quoting the
  sentence that permits it, 7 not implemented and owned), the teardown-versus-in-flight-poll
  race is fixed (the write gate now separates "closing" from "a poll is in progress", so
  teardown waits one poll instead of a frame), the BUS-01 to BUS-03 acceptance bullets are
  named tests over both transports with a 29-event trace equivalence, and the guide's first
  deliverable exists: one example with a counter RPC, a latest observer and a frame artifact
  held past its message. Measured on the dev VM, not capacity claims: 640x480 RGBA at 60 Hz to
  three consumers, one delayed, RPC p50 0.5-1.1 ms, seal p50 1.1-1.4 ms, router 0.18-0.22 cores,
  RSS 11-17 MB. Two spec contradictions were resolved in bus-v1 rather than in the code (the
  per-client byte budget now names bounded queues only, with latest slots capped separately;
  the illustrative client sketch drops its budget argument for a caller-side deadline).
- 2026-09-22 (session framework, wave 1 complete): the CONTRACT and SESSION slices merged. There
  is now one executable contract for the new architecture: a types crate and a matching
  TypeScript package that read the same fixtures, RFC 8785 canonical JSON, a contract digest
  generated from the schema set rather than from source formatting, the four identities typed so
  they cannot be mistaken for one another, and two new specifications with test vectors for seed
  derivation and the checkpoint envelope. On top of it a synthetic lockstep session runs over the
  bus: fake agents, a counter world, identity executors and a deterministic task, stepping
  prepare, advance, evaluate, commit in that order, with the rational clock proving 16, 17 and 17
  ticks and a zero remainder, request deduplication that refuses a changed body and replays a
  cached one, and the guide's failure injections as tests that compare an injected run against a
  clean one on behaviour, mutation count and world counter. Reviews caught two defects worth
  naming: a merge that stopped short of the TypeScript half, whose absence silently took its own
  test gate with it, and an unchecked addition that would have wrapped in release. Both fixed
  before merge.
- 2026-09-22 (v0.4.6, loop review, auto): two rollbacks on rung 10. The "BACK in a text box" was
  a scripted overworld frame with no box (the cartridge holding the joypad) dealt NEXT/BACK; the
  museum had no map-graph row so no hop reached the gym; a frontier behind the admission desk
  never retired; the ratchet counted covered ground as no progress. Fixed: a scripted frame with
  nothing drawn deals nothing; the museum rows are on the graph; an unreachable frontier is a
  per-map mark; nearer the objective in map hops counts as progress. ROM test: museum to the gym
  interior on 15 macros, TALK on the pad at the leader. The trap hunt did NOT improve on the
  reviewer's criterion (tiles 193 -> 175, windows 59 -> 69) because the after arm reaches new
  ground with a new trap (row 54: GO FRONTIER, GO HEAL, GO ROUTE cycling, GO HEAL x204 at net 0);
  Fable shipped it anyway: the hunt criterion compares within ground both arms reach, and a trap on
  newly opened ground is a new row, not a regression. Row 54 review started at once.

## 2026-09-22 - session framework wave 2

Per-fly processes and native observations landed on top of the wave-1 contract and
lockstep session.

- The session runs in three execution modes that share one coordinator, one worker and
  one router: in process, one thread per participant, and one process per fly plus one
  for the environment over Unix sockets. The worker is a subcommand of the existing
  binary, not a new crate. A launcher owns the thread budget, proves each participant's
  configured identity and allocation on the wire before the coordinator pins anything,
  polls health on its own clock, and reaps its children.
- A caller deadline that expires no longer fails the epoch on its own. It runs the
  contract's resolution procedure against the same request and the same incarnation, so
  a merely slow participant completes its step, and the epoch fails only on a definite
  refusal, a lost incarnation or an exhausted budget. Which of the two bounds ended a
  resolution is recorded and named in the failure rather than inferred.
- Failures name the participant, and a failed session fences its epoch: the boundary
  stops, handles are dropped, and no further transition or publication is possible.
  Worker death, helper death, a router restart mid-advance and stale replies after a
  restart all have bounded, diagnosed outcomes, each proved in every mode.
- The environment now emits native output: one shared RGBA frame per boundary reaching
  both flies through owned attachments, and one audio chunk per transition on an exact
  rational sample budget. Observation delay is a real queue, so nothing stale can be
  substituted. Spectators watch a latest subscription with finite credits and cannot
  perturb what the flies sense. Audio never enters sensory input.
- Every media rule is enforced rather than assumed: strides, dimensions, formats,
  lengths, producing step, timeline continuity, and the distinction between a persistent
  asset and a transient artifact. A missing or malformed frame or chunk fails its step.
- Three contract silences were closed by dated amendments rather than by convention: the
  launcher's thread allocation is now on the wire in the worker's hello, the audio
  discontinuity rule is one-directional, and a mid-step pause is defined.

Measured on the development box, not capacity claims: with two flies the critical path
per transition sat near 10 to 12 ms at the median across all three modes, so a process
boundary costs little at the median and shows in the tail. What the split costs is
memory, roughly 5.7 MiB per participant process, while the coordinator's own footprint
is flat and lowest once the workers leave it.

Two flaky bus tests predating this work assert timing rather than contract and are being
rewritten separately.
- 2026-09-22 (v0.4.7, loop review, auto): row 54 in Pewter and on Route 3. The player's coordinates
  change at the END of a sixteen-frame step, so the whole-map grid refused every moving frame and
  walks fell back to the window; a landing tile went unrecorded for fifteen frames and stayed a
  frontier; an errand aimed at a door underfoot settled where it stood; the errand ledger was
  session state and re-armed on restore. Fixed: the walk anchor is measured from the screen
  neighbourhood, landing tiles retire, errands arrive inside facing the counter, the errand ledger
  persists. Route 3's north edge is a survey item (table says west+east). From the Pewter checkpoint
  the fly wins the Boulder Badge at 10.78 brain minutes. The hunt's flagged windows did not fall
  (73/73) because 82% of the fixed run is battle time; Fable shipped it on the same judgement as
  v0.4.6 and started row 50 (MOVE n blocked on an unresponsive move list). The on-screen chat ring
  now survives a sim restart (sidecar in the hot dir, never in the checkpoint).
