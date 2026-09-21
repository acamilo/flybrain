<!-- Draft replacement for README.md. Links are written relative to the repository root. -->

# flybrain

A simulated fruit-fly brain, built from the FlyWire FAFB connectome at 139,255 neurons and
2,700,513 signed synapses, playing Pokémon Red on a Game Boy emulator 24 hours a day on a Twitch
channel. The game screen is projected onto the fly's visual columns, the network is stepped one
simulated millisecond at a time, and the firing rates of its descending command populations are
read out as joypad presses. A scalar reward computed from the game's own memory stimulates the
modelled dopamine pathway, and that is the only thing in the loop that changes a synapse.

## What is the fly and what is ours

Both halves are named here because the interesting claim is small and the uninteresting scaffolding
around it is large.

**The brain is the fly.** Leaky integrate-and-fire units over the connectome's own neurons and
edges, in CSR form, stepped every simulated millisecond: 20 ms membrane decay, threshold 1, 2 ms
refractory, a fixed noise drive, a deterministic xorshift RNG. Anatomical roles (Kenyon cells,
mushroom body output neurons, PAM dopamine neurons, descending command neurons, the L1 retina
columns) are labels carried by the dataset, not functions we inferred. Learning is three-factor
reward-modulated STDP on the 16,384 strongest excitatory Kenyon-cell to MBON synapses and nowhere
else. The measured connectome weights are never modified; a per-edge gain clamped to [0.9, 1.1]
multiplies them.

**The retina is ours, and it is not fly optics.** A 160x144 frame is projected onto 1,572 L1
columns by luminance. That is a stand-in for the visual system, not a model of one.

**The readout is ours and it is fixed.** A population decoder turns role firing rates into
channels: the four directions as one exclusive group with 800 ms holds, hysteresis and fatigue;
A and B as pulse channels; START and SELECT throttled. It calibrates once on resting rates and
then applies constant ratios and thresholds. No channel gain adapts and nothing in it learns. Its
one input that is not a firing rate is the name of a direction the loop has watched produce no
movement for a whole hold, which raises that channel's habituation and nothing else. That is "the
button you are holding did nothing", not "the door is south": it names no position, no destination
and no alternative.

**The reward rule is ours, and it is a design choice rather than a finding.** A catalog of
positive-only payouts read out of game memory after a frame the fly's own buttons produced: story
flags +1, a new map +0.2, each 8 new walkable tiles +0.05 up to a cap per map, a new species
+0.5, a beaten trainer +0.5, a wild win +0.1 decaying, a badge +3, standing next to and then on a
map exit +0.05 and +0.10 once each per exit. There are no penalties, no blackout cost and no
negative values. Every payout is gated on a playable, unscripted sample, and the first sample after
a load baselines everything already achieved so restoring progress replays none of it. Semantic
rewards are enabled for exactly one audited cartridge hash; any other cartridge runs with rewards
off and the reason on screen.

**The macros are buttons the fly presses with its own populations.** Beyond the eight Game Boy
buttons there are 22 macro types (walk to the objective, walk out, walk to an item, walk to a
person, talk, attack, switch, buy a potion, heal, and so on). Each is a channel of its own decided
by a second exclusive group with the direction group's rules, and each is driven by its own
population, `macro_<type>`, cut from the 96 mushroom body output neurons and the 110 brain motor
neurons. The scene decides which of those buttons exist; unbound channels are masked out of the
decision entirely, so they cannot win. Whichever bound channel wins, starts, and while a macro
runs it owns the joypad. The knowledge lives inside the macro script, never in the choice: a macro
knows how to route to a door, and nothing weights, ranks or biases which macro is chosen. The
honest sentence for the split is that the mushroom body picks the macro and the descending neurons
press the buttons, and the reason it matters is that the plastic edges end on MBONs, so a learned
preference between two macros in one scene is the one thing a reward in this loop can actually
move. Whether it makes the fly play better is unmeasured; the bench that would answer it has not
been run. Raw mode (the eight buttons only) is the other setting and is a config knob, never a
chat command.

**The ratchet is ours.** A 38-rung ladder of game states (boot, the bedroom, each town, each
badge, the Elite Four, Champion). On first reaching a higher rung in a state safe to snapshot, the
emulator state is archived; on a stall or a game over, the game is rolled back to the best
snapshot. Only the game rolls back. The brain keeps its clock, its RNG and its learned gains;
decoder holds and eligibility traces are cleared. Without it a mostly random walker cannot hold net
progress over days, and saying so is more useful than pretending the progress bar is all the fly.

**The sugar pulse is the viewers'.** `!sugar` in chat fires a timed pulse on the same modulatory
pathway a game reward uses: transient, rate-limited server side at 6 per minute with no overlap,
logged, and shown on screen with the name of whoever sent it. It changes no synapse directly and
presses no button. It is the only viewer influence on the running fly.

## Honesty rules

These are structural, not policy:

- **Game memory is read, never written.** The adapter samples audited memory addresses once per
  frame to compute rewards, a game mode and a progress rank. It writes nothing back.
- **Only the joypad is written.** The single path from the network into the game is the emulator's
  button register.
- **Nothing presses for the fly.** There is no default macro, no fallback action on a timeout and
  no scripted objective. A scene whose buttons the fly ignores waits. If the fly's channels are
  silent, nothing happens.
- **No scripted play.** Nothing in the loop chooses or biases a button. The reward is read out of
  memory after the fact, and the only thing it can do is stimulate the modulatory pathway.
- **No button endpoint exists.** The control API has no route that presses a button, edits game
  memory or changes the reward catalog, and a test asserts the route table has none. Chat reaches
  the page, not the simulation.
- **The readout knows nothing about the game.** The decoder does not know that a map exists, let
  alone where its doors are.
- **The broadcast page is a display.** It renders finished frames and PCM. No ROM, no emulator and
  no input path is in the browser.
- **What is not claimed:** the reward modulator is synthetic, the retina is not fly optics, and
  learning has not been shown to improve play. `docs/limitations.md` is the list.

## Architecture

One process owns the fly. It holds the network, the emulator linked natively as C, the game
adapter and the ratchet, and on a single simulation thread it drains control commands, steps the
brain 16 or 17 ms, decodes the channels, applies buttons, runs one emulator frame, projects the new
frame onto the retina, samples rewards, stimulates, reinforces, lets the ratchet observe, and
publishes a snapshot. Snapshots go out over a loopback WebSocket at 30 Hz as one binary message
each: a JSON header (status, role rates, learning statistics, game mode, milestone rank, macro
state, events, a chat ring) followed by the RGBA frame, f32 stereo audio and a spike bitset. A
React page subscribes to that socket and draws it at 1920x1080 in a kiosk browser, which a capture
pipeline encodes. A Node service connects the channel's chat to a loopback HTTP control API whose
only mutating routes are the sugar pulse, an on-screen chat line, a forced checkpoint and
pause/resume. Checkpoints are written in an envelope format with a CRC footer, a hot copy every 5
seconds and a durable copy every 300, plus one per archived rung; restore order is latest, then
previous, then archives, and if every candidate fails the service exits non-zero rather than
quietly starting a fresh brain.

| Path | What it is |
| --- | --- |
| `packages/brain` | The reference library in TypeScript: dataset format, LIF kernel, plasticity rule, population decoder, activity-map geometry. The oracle: no other implementation's semantics are allowed to change it. |
| `packages/feed` | The contracts in TypeScript: types, the binary snapshot codec, a pinned JSON Schema, the chat and display-name sanitizers, the `.flyfeed` fixture container, and a runnable fake simulation service. |
| `services/flysim` | The Rust service that owns the fly: a bit-exact port of the kernel, the emulator, the game adapters, the ratchet, the feed, the control API, checkpoints and the event log. |
| `services/bridge` | The Node chat bridge: commands, rate limits, template-only replies, on-screen chat forwarding, Channel Points redemptions. Every outbound string is a constant. |
| `apps/stage` | The broadcast page: React, fixed 1920x1080, display only, with a player mode that replays recorded fixtures. |
| `data/fafb-v783` | The connectome artifacts the brain loads. |
| `tools/` | The Python builder that regenerates `data/fafb-v783` from the official exports, with checksums. |
| `infra/` | Provisioning scripts, service units and the deploy gates for the 24/7 run. |
| `docs/` | The binding contracts and the design record. |

## Running it locally, without a ROM

Most of the repository runs with no cartridge and no connectome download.

```sh
npm ci
npm test                 # 477 TypeScript tests
npm run typecheck

# the fake simulation service: the real feed and the full control API, no dataset, no ROM
npx tsx packages/feed/src/fake/server.ts --scenario running

# the broadcast page, replaying a recorded fixture (this is the default mode)
npm run dev -w @flybrain/stage        # then ?mode=player&fixture=steady
npm run test:e2e -w @flybrain/stage   # Playwright screenshot, legibility and structure gates
npm run mockups -w @flybrain/stage    # the review PNGs

# the whole 139,255-neuron brain on noise frames, in plain Node
npx tsx packages/brain/examples/node-random-frames.ts 60

cd services/flysim && cargo test --workspace   # ROM-gated tests skip and say so
bash infra/tests/lint.sh
```

- **The fake sim** (`packages/feed/src/fake/`) is a synthetic service that speaks the feed protocol
  and implements the control API in full, including the rate limits and the 403s. It is
  deterministic given a seed, depends on neither the brain nor the dataset, and has four
  scenarios: `boot`, `running`, `stuck` and `milestone`. Its frames are procedurally drawn in the
  Game Boy palette; there are no game assets in this repository.
- **Fixtures** are recorded runs of the feed in the `.flyfeed` container (a manifest plus the exact
  wire messages), replayed by the page's player mode. `cold-open`, `steady`, `big-moment` and
  `macros` are committed, and `npm run record -w @flybrain/stage` cuts new ones against the fake
  sim or a real one. Player mode is the page's default, so the page, its animations and its
  screenshot baselines all run with no service at all.
- **The oracle tests** are how the two implementations are kept honest. Verbatim copies of the
  prototype's modules live in `packages/brain/tests/legacy/`, and every generalized module has a
  bit-exact test against them, including a run on the real dataset. The default configuration
  reproduces that prototype's kernel bit for bit, and the version strings `lif-1ms-f64-v2` and
  `fly-kc-mbon-rstdp-v2` are pinned so checkpoints stay compatible.
- **The golden tests** pin the Rust port to the TypeScript at 0 ulp. Golden files under
  `services/flysim/golden/` are generated by `packages/brain/tools/golden.ts` and compared field by
  field. Getting to 0 ulp needed a transcription of fdlibm's `tanh` and a `libm` `exp`, because the
  system math library and the JavaScript engine disagree by 1 ulp. Threading is sharded so that
  per-target addition order and per-slot trace order are preserved, and results are identical for
  any thread count.

## What needs a ROM

A cartridge is required only to run the real service and the tests that drive a real game. All of
them are gated on the `FLY_ROM` environment variable and skip cleanly, with a line saying why, when
it is unset:

```sh
FLY_ROM=/path/to/cartridge.gb cargo test --workspace   # in services/flysim
cargo run --release -p flysim -- --config flysim.toml  # needs paths.rom and data/fafb-v783
```

The ROM-gated suites are the game adapter's reward, scene and macro tests, the service's
integration and macros-mode tests, and the platformer adapter's. The platformer's memory addresses
are marked UNVERIFIED until a cartridge has been watchpointed against them.

ROMs and save states are never committed to this repository, copied into the tree, linked, or shown
on the broadcast.

## How a release is cut

- **Tagged commits only.** The release role deploys from a clean tree at an annotated tag matching
  `^v[0-9]+\.[0-9]+\.[0-9]+$`, the release directory is named after the tag, and the deploy refuses
  an untagged or dirty tree, a package whose version does not match the tag, and any attempt to
  push unit files onto a box that has already had a tagged deploy.
- **The compatibility gate.** Every build prints a compatibility string: kernel version,
  plasticity version, adapter version, dataset fingerprint, emulator revision, game-disassembly
  commit and emulator state size. A checkpoint restores only on an exact match. The deploy compares
  the incoming build's string against the installed state and refuses to switch over when they
  differ, unless the operator opts in with `FLY_RESET_STATE=1`, which archives the durable state
  and clears the hot ring first. This gate exists because a release once changed the adapter
  version, correctly refused its own container's checkpoints, and exited by design.
- Macro roles are deliberately outside the dataset fingerprint, so adding them did not invalidate
  existing checkpoints; their rates start at zero on a restore that predates them.

## Data attribution and licence

The artifacts in `data/fafb-v783` are independently generated from the FlyWire FAFB public Codex
v783 exports and are licensed
[CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/). Source URLs, the list of
modifications, the citations (Dorkenwald et al. 2024; Schlegel et al. 2024; Matsliah et al. 2024)
and the statement that no endorsement by FlyWire is implied are in
[`data/fafb-v783/ATTRIBUTION.md`](data/fafb-v783/ATTRIBUTION.md). Connectivity aggregation,
role predicates and model signs in those artifacts are project modelling choices, not FlyWire
annotations.

The vendored Game Boy emulator keeps its own MIT licence and provenance note under
`services/flysim/vendor/`.

**No licence has been chosen for the code in this repository yet.** Until one is, assume no rights
are granted beyond reading it.

## Documentation

Contracts, which win where a design doc disagrees:

- [Feed protocol](docs/feed-protocol.md): framing, the header, attachments, cadence.
- [Control API](docs/control-api.md): every route, the rate limits, the config file.

The model:

- [Overview](docs/overview.md), [Dataset format](docs/dataset-format.md), [Model](docs/model.md),
  [Plasticity](docs/plasticity.md), [Readout](docs/readout.md).
- [Rewards and learning](docs/rewards-learning.md): the reward catalog, its gates, and what a reward
  can and cannot move.
- [Integration](docs/integration.md): what a game must provide, with the Pokémon Red adapter as the
  worked example.
- [Limitations](docs/limitations.md) and [Verification](docs/verification.md).

The system:

- [Architecture tour](docs/architecture-tour.md): every layer, what it is and why it is that way.
- [Stream plan](docs/stream-mvp-plan.md): the decisions and the running status.
- Designs: [macros](docs/design/macros.md), [ladder](docs/design/ladder.md),
  [flysim](docs/design/flysim.md), [stage and bridge](docs/design/stage-bridge.md),
  [fly avatar](docs/design/fly-avatar.md), [animation](docs/design/animation.md),
  [platformer](docs/design/platformer.md), [describe tab](docs/design/describe-tab.md).
- Package documentation: [`packages/feed`](packages/feed/README.md),
  [`services/bridge`](services/bridge/README.md), [`apps/stage`](apps/stage/README.md),
  [artifact builder](tools/README.md).
