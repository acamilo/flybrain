# Feed protocol v1 (flysim to flystage)

The simulation service publishes snapshots over a WebSocket on `ws://127.0.0.1:7400/feed`. The
page is a display: it never sends anything but a one-time `hello`. Every consumer (stage, bridge,
tests) uses the same message shape, defined in TypeScript in `packages/feed` and in Rust with
serde in `services/flysim/crates/flysim`. A JSON schema test in each side pins the shape.

## Framing

Each snapshot is ONE binary WebSocket message:

```
u32 LE headerLength | header JSON (UTF-8) | attachments...
```

Attachments follow the header in the order listed by `header.attachments`, each as
`u32 LE byteLength | bytes`, so a consumer can skip attachments it does not want. The header
is small (under 4 KB); attachments carry the bulk.

Cadence: 30 snapshots per second wall-clock while running, 2 per second while paused or
booting (header only). The service drops snapshots rather than queueing when the socket is slow.

## Header

```ts
interface FeedHeader {
  protocol: 1;
  seq: number;                 // monotonically increasing
  wallMs: number;              // Date.now() at publish
  status: 'booting' | 'running' | 'paused' | 'recovering' | 'error';
  realtimeFactor: number;      // simulated ms per wall ms over the last second
  uptimeSeconds: number;       // since service start
  runSeconds: number;          // total simulated seconds across restores (from checkpoint)
  brainMs: number;             // simulation clock
  frame: number;               // emulator frame counter
  buttons: number;             // bitmask, GAMEBOY_BUTTON_BITS order (up,down,left,right,a,b,start,select)
  rates: Record<string, number>;   // Hz per tracked role: command_0..7, steer_left, steer_right,
                                   // forward, backward, proboscis, reward_pam, and in macros
                                   // mode the bound macro types' macro_* roles
  populationRate: number;      // Hz
  spikeCount: number;          // set bits in the spikes attachment (0 when omitted)
  learning: { enabled: boolean; updates: number; changed: number; synapses: number; signal: number };
  game: {
    mode: 'BOOT' | 'OVERWORLD' | 'BATTLE' | 'TRANSITION' | 'DEMO' | 'SAFARI' | 'UNKNOWN';
                               // closed set; adapters fold onto it (see below)
    semanticRewards: boolean;  // false when the ROM hash is not the audited one
    map: number | null;        // current map id when known
    badges: number;            // 0..8
    uniqueLocations: number;   // exploration coverage
    rewardTotal: number;
    rewardCounts: Record<RewardKind, number>;
    // The macro buttons (docs/design/macros.md section 12). Added 2026-09-16, additive,
    // optional: a producer that predates them omits all five and a consumer falls back to
    // raw-mode rendering.
    scene?: Scene;             // closed set; the scene whose macro buttons are bound
    macroMode?: 'raw' | 'macros';
    palette?: PaletteSlot[];   // the scene's bound macro buttons, ascending by slot,
                               // one per macro type at most; [] in raw mode
    macro?: RunningMacro | null;       // the macro that owns the buttons
    macroOutcome?: MacroOutcome | null; // the one that finished most recently
    padEmptyMs?: number;       // BRAIN ms the pad has had nothing on it in a playable
                               // scene; 0 otherwise. Report only.
  };
  milestone: {
    rank: number;              // position on the ratchet ladder, 0..total-1
    label: string;             // human label, e.g. "Left the bedroom"
    next: string;              // label of the rung the fly is going for (see below)
    sinceSeconds: number;      // simulated seconds at current rank (the stuck-o-meter)
    attempts: number;          // game rollbacks since reaching this rank
    total?: number;            // rungs the game's ladder has (>= 1); omitted by older producers
  };
  sugar: {
    active: boolean;           // a stimulation pulse is currently applied
    remainingMs: number;
    cooldownMs: number;        // until the next viewer sugar is accepted
    lastBy: string | null;     // display name of the last redeemer
    todayCount: number;
  };
  events: FeedEvent[];         // events since the previous snapshot (usually empty)
  chat?: ChatLine[];           // last N (<= 12, `[chat] ring`) lines accepted by the control API,
                               // oldest first. ABSENT ENTIRELY (not an empty array) while
                               // `[chat] enabled = false`, so a page can tell "chat is off" from
                               // "nobody has said anything yet". Chat never reaches the simulation.
  attachments: AttachmentKind[];
}

type RewardKind = 'story' | 'explore' | 'area' | 'pokedex' | 'trainer' | 'wildwin' | 'badge';

type Scene =
  | 'title' | 'overworld' | 'dialog' | 'menu'
  | 'battle' | 'battle-switch' | 'shop' | 'pc' | 'unknown';

interface PaletteSlot {
  slot: number;                // the macro TYPE's own index, 0..30; unbound types are
                               // omitted, and a type's index never changes
  name: string;                // the macro's name, at most 14 characters
  gloss: string;               // two or three words of what it does here ("nearest door")
  channel: string;             // the type's short channel tag, e.g. "MB·GO", "MB·ATK"
}

interface RunningMacro {
  slot: number;
  name: string;
  sinceMs: number;             // BRAIN milliseconds since it started (a duration, not a clock)
}

interface MacroOutcome {
  slot: number;
  name: string;
  outcome: 'done' | 'blocked' | 'timeout' | 'refused';
  atMs: number;                // the BRAIN clock at which it finished (a timestamp)
}

interface FeedEvent {
  id: number;                  // monotonically increasing across the run
  wallMs: number;
  brainMs: number;
  kind: 'reward' | 'sugar' | 'milestone' | 'recovery' | 'checkpoint' | 'viewer' | 'system'
      | 'macro';
  label: string;               // short human text, template-generated, never raw chat
  value?: number;              // reward value, milestone rank, or macro slot
  rewardKind?: RewardKind;
  by?: string;                 // viewer display name for sugar/viewer events
}

interface ChatLine {
  id: number;                  // the event id of the accepted line
  wallMs: number;
  by: string;                  // validated display name (packages/feed names.ts rules)
  text: string;                // sanitized: <= 200 code points, letters/digits/space and a fixed
                               // punctuation allowlist only — no control, zero-width or bidi
                               // characters, no combining marks, no emoji, no URLs, deny-list
                               // filtered; the service refuses anything else whole, never trimmed.
                               // The rules live in packages/feed/src/chat.ts and, identically, in
                               // services/flysim/crates/flysim/src/chat.rs, pinned to each other
                               // by packages/feed/tests/fixtures/chat-cases.json
  bot?: boolean;               // true for the bridge's own template replies
}

type AttachmentKind = 'frame' | 'audio' | 'spikes';
```

`game.mode`, `game.scene` and `RewardKind` are closed sets, because every consumer switches
exhaustively on them.
An adapter with other states or other reward kinds folds onto these names rather than extending
them; the per-game config in `apps/stage/src/games/` supplies the words on screen.

| Adapter mode | `game.mode` | Why |
| --- | --- | --- |
| Pokémon `BOOT` / `OVERWORLD` / `BATTLE` / `TRANSITION` / `DEMO / SAFARI` | as named | — |
| Platformer `IN LEVEL <world>-<stage>` | `OVERWORLD` | Both mean "controllable in the world". The level itself is `game.map`, so nothing is lost. |
| Platformer `GAME OVER` | `TRANSITION` | The run is over and the fly is not controllable. The recovery event says what happened. |
| Platformer `DEMO` (attract demo) | `DEMO` | — |
| `UNSUPPORTED ROM · SEMANTIC REWARDS OFF` | `UNKNOWN` | `game.semanticRewards` already carries it. |

The platformer's nine reward kinds share the seven published counters: `band` -> `explore`,
`coin` -> `wildwin`, `score` -> `area`, `powerup` -> `pokedex`, `life` -> `trainer`,
`level` -> `story`, `world` -> `badge`. `started` and `clear` map to nothing: each pays once in a
lifetime, so a counter for them is noise, and their events carry their own labels.

## The macro buttons (2026-09-16)

`docs/design/macros.md` section 12 is the design; these are the fields, and where the two differ
this file wins. Additive: `protocol` stays 1, the five fields are optional in
`packages/feed/src/schema.json`, and a consumer that ignores them renders exactly what it rendered
before. The Rust producer always sends all five, in both modes.

- **`game.scene`** is the scene whose macro buttons are bound, from the closed set above. It is
  `unknown` in raw mode — nothing is on the pad but the eight buttons, so no scene is claimed —
  and `title` through the intro, where nothing is bound by design and the readout's boot variant
  applies.
- **`game.macroMode`** is `raw` or `macros`, straight from `flysim.toml`'s `[macros] mode`. A
  consumer that does not know a mode name renders the raw layout rather than publishing a word it
  cannot explain.
- **`game.palette`** carries the scene's **bound** macro buttons, ascending by slot, at most one
  per macro type. `slot` is the macro *type's own index* since `docs/design/macros.md` section 14
  (2026-09-17) — not a Game Boy button, not a rank, and no longer a position among the bound ones,
  which is what let a cell move between two frames as a precondition came and went. A type that
  this scene does not bind is omitted rather than sent as a null, and the page draws its cell dim.
  Always `[]` in raw mode.
- **`game.macro`** is the macro that owns the buttons, or `null`. `sinceMs` is a *duration* in
  brain milliseconds, so a page can show how long it has been running without doing arithmetic
  against `brainMs`.
- **`game.macroOutcome`** is the macro that finished most recently and is **kept until the next
  one starts**, so the page can light a cell's result for a beat rather than racing a single
  frame. `atMs` is a brain-clock *timestamp*, so it can be compared with `brainMs`.

- **`game.padEmptyMs`** is how long the pad has had nothing on it **in a playable scene**, in
  brain milliseconds, and 0 otherwise — in raw mode, on the title screen, when something is bound,
  and while a macro is running, because a running macro owns the pad. It is **report only**: no
  producer acts on it and no consumer should, and nothing in the loop reads it back. It exists
  because an empty pad is the doctrine working — nothing presses for the fly, so a scene with no
  button waits — and on screen that is indistinguishable from a hang
  (`docs/design/macros.md` section 13.1). `fly-watchdog` exports it as `fly_pad_empty_seconds`.

`game.macro` and `game.macroOutcome` carry that same slot, so a display lights the cell that ran
without matching on the name.

### One channel per macro type (2026-09-16)

`docs/design/macros.md` sections 11 and 12: a macro type is a button pressed by its own neuron
population, and the decoder's second exclusive group picks between the types the scene has bound
(`docs/readout.md`, "Macro group"). Two consequences here.

- **`game.palette[i].channel`** is that type's short tag, which the page draws as the cell's glyph
  — `MB·GO`, `MB·ATK`. The rate role behind it is `macro_` plus the macro's name lowercased
  with spaces as underscores (`GO ITEM` -> `macro_go_item`), which is how a consumer finds the
  type's Hz in `rates`. It is also what orders the cells on screen: the cells are drawn in the
  contract's fixed type order, which since section 14 *is* `slot`. Every entry a dealt mode sends
  carries it; it is absent only from a producer that predates this paragraph.
- **`prior` is removed.** There is no rank, no ladder and no blend left to publish: whichever
  bound channel wins the decision starts, and `game.macro` says which did. A producer still
  sending `prior` is stale, and a consumer ignores the field.

`rates` carries the bound types' `macro_*` roles alongside `command_0..7`, which is what the
SENSES tab's MACROS row draws. Those populations are shared out over the mushroom body output
neurons and the brain motor neurons, so a rate there is the mushroom body's, not a ninth button.

One `macro` event per start and per finish, so the ticker and `events.jsonl` both carry them.
Labels are exactly `<NAME> start` on a start and `<NAME> done|blocked|timeout|refused` on a
finish; `value` is the slot. An unbound type produces no event of any kind: it never entered the
decision, so there is nothing to report.

### Why `macroMode` and not `mode`

`docs/design/macros.md` asks for `game.mode: "raw" | "macros"`. That name has been taken since
protocol v1 by the adapter's own mode string (`BOOT`, `OVERWORLD`, …), which is a
closed set every consumer switches exhaustively on, and repointing it would be a breaking change
to a published field for a feature that is explicitly additive. So the mode is published as
`game.macroMode`, and the design doc's field list is wrong on that one name. `macroMode` is the
name both sides are built against.

## `milestone.next` is the rung the fly is going for (2026-09-17)

`next` was the label of `rank + 1`. It is the label of the adapter's own **next rung** now —
`GameAdapter::next_rung`, the lowest rung the run has *not* earned — falling back to `rank + 1` for
an adapter that does not answer. The type is unchanged (a string, never empty: the current label is
still the fallback at the top of the ladder), so no client changes and no version bump.

Why: the rank is the *maximum* over satisfied rungs, and a ladder's rungs are not a chain. The live
save of 2026-09-17 had Pokémon Red's rungs 6 and 7 — Oak's parcel delivered, and the Pokédex —
unearned while rung 8 (Viridian City, a map) was stood on, so the rank read 8 and the header said
"→ VIRIDIAN FOREST" while `GO OBJECTIVE` was walking two maps south to deliver a parcel. The screen
was naming a rung nothing was working toward. `rank`, `label`, `sinceSeconds`, `attempts` and
`total` are untouched, and so is the ratchet: the rank is still the maximum, because that is what
"best reached" means.

## Attachments

- `frame`: 160 x 144 RGBA, 92,160 bytes, the emulator framebuffer after the last frame.
- `audio`: interleaved stereo `f32` PCM at 48,000 Hz (Web Audio's native rate on Linux, so the
  page never resamples), all samples produced since the previous snapshot (about 1,600 frames =
  12,800 bytes at 30 Hz). binjgb emits unsigned 8-bit stereo; the service converts. The page keeps
  a ring buffer with varispeed drift correction.
- `spikes`: a bitset of `ceil(neurons / 8)` bytes (17,407 bytes for 139,255 neurons), bit `i`
  (byte `i >> 3`, mask `1 << (i & 7)`) set when neuron `i` spiked at least once since the previous
  snapshot. Fixed size, order-free, and exactly what the page's density accumulator consumes.
  `header.spikeCount` carries the number of set bits.

Bandwidth at 30 Hz: about 3.7 MB/s on localhost (frame 92 KB, spikes 17 KB, audio 13 KB).

## Client hello

On connect the client sends one JSON text message:
`{"protocol":1,"client":"stage"|"bridge"|"test","wants":["frame","audio","spikes"]}`.
The service honours `wants` per connection. The bridge asks for no attachments.

## Compatibility

`protocol` bumps on any breaking change. Adding optional header fields is not breaking.

## Audio source note (2026-09-15)

binjgb's mixer emits unsigned 8-bit samples that are unipolar: silence is 0, not 128 (measured
range 0..44 over a Pokémon Red boot). The service converts with `v / 255` to `[0, 1]` and then
applies a DC-blocking one-pole high-pass (`y[n] = x[n] - x[n-1] + 0.995 * y[n-1]`) per channel
before publishing, so the `audio` attachment is ordinary bipolar f32 PCM centred on zero. The page
does no further offset correction.
