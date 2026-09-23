# Macro palette: scene-appropriate actions for the fly

Queued by the operator 2026-09-16 ("give the system something more than randomly mashing buttons");
contract written by Fable the same day. `docs/stream-mvp-plan.md` has the assessment. This file is
the binding contract for the agents building it; where it differs from prose elsewhere, this wins.

## 1. Doctrine

- **The readout stays fixed.** The population decoder, its channels (UP DOWN LEFT RIGHT as one
  exclusive group; A B as pulses; START SELECT on boot) and its numbers do not change. Palette
  mode is a *scene-dependent meaning* of those same channels, applied after the decoder, in the
  game layer: UP means "slot 0", DOWN "slot 1", LEFT "slot 2", RIGHT "slot 3", A "slot 4",
  B "slot 5". A scene defines up to six macros and binds them to slots; an unbound slot is
  "no action".
- **The fly chooses the action.** Nothing chooses for it: no default macro, no fallback action
  on timeout, no scripted objective. If the fly's channels are silent, the game waits.
- **A macro is a button script**, executed by the sim on the emulator with the same button
  register the fly's raw buttons use. There is still no button endpoint on the control API and
  no button path from the bridge or the page.
- **Disclosed.** The honesty panel sentence becomes "the fly chooses the action"; the mode
  (`RAW` or `PALETTE`), the scene, the palette and the chosen macro are on screen at all times.
- **Raw mode remains** and is the default until measured; the mode is a config/env knob and a
  `[macros]` block in `flysim.toml`, never a chat command.

## 2. Scenes (Pokémon Red, pret/pokered at the pinned commit in `docs/design/ladder.md`)

```rust
pub enum Scene {
    Title,                                  // no palette: boot variant of the readout applies
    Overworld,                              // player can walk
    Dialog,                                 // a text box is open and waiting
    Menu,                                   // start menu / submenu open, outside battle
    Battle { own_turn: bool, forced_switch: bool },
    Shop,                                   // mart buy/sell menu
    Pc,                                     // PC / storage menu
    Unknown,                                // detection failed: treated like Dialog (advance only)
}
```

Detection is from WRAM only, sampled once per game frame after the frame, by
`pokemon_red::scene::detect(&mut dyn MemoryReader) -> Scene`. Sources and offsets are listed in
`docs/design/macros-wram.md` with the pokered symbol names and the verification method for each.

## 3. Palettes

Small on purpose (3 to 6 slots). Slot binding is fixed per scene so the meaning of a channel is
stable inside a scene. Every macro has a name of at most 14 characters for the screen.

| scene | slot 0 (UP) | slot 1 (DOWN) | slot 2 (LEFT) | slot 3 (RIGHT) | slot 4 (A) | slot 5 (B) |
| --- | --- | --- | --- | --- | --- | --- |
| Overworld | `GO EXIT` nearest unvisited warp or map-edge exit | `GO NPC` nearest visible person and face it | `GO ITEM` nearest interactable object or sign, and face it | `WANDER` 8 random steps (raw flail, bounded) | `TALK` press A | `MENU` open start menu |
| Dialog | `NEXT` advance text | `NEXT` | `NEXT` | `NEXT` | `YES` A | `NO` B |
| Menu | `CLOSE` back out to overworld | `CLOSE` | `CLOSE` | `CLOSE` | `CONFIRM` A | `BACK` B |
| Battle own turn | `ATTACK` best damaging move with PP | `SWITCH` healthiest other Pokémon | `ITEM` potion if own HP < 50% and one is held, else no action | `RUN` (wild only; trainer: no action) | `ATTACK` | `BACK` |
| Battle forced switch | `SWITCH` healthiest | `SWITCH` | `SWITCH` | `SWITCH` | `SWITCH` | no action |
| Shop | `BUY POTION` if money allows | `BUY BALL` if money allows | `LEAVE` | `LEAVE` | `CONFIRM` | `LEAVE` |
| Pc | `LEAVE` | `LEAVE` | `LEAVE` | `LEAVE` | `CONFIRM` | `LEAVE` |

"Best damaging move" is the highest base power move with PP, type effectiveness applied from the
ROM's type chart; ties by index. "Healthiest" is highest HP fraction, not fainted, not the active
one. Visit sets for `GO EXIT` are the adapter's existing per-map exploration ledger.

**Amended 2026-09-16, after the smoke run** (`infra/docs/macros-bench.md`). Palette mode left the
house eleven times faster than raw and then stalled in Oak's lab, one rung short of the starter,
and the two causes were both in this row:

- **Slot 2 was `LOOK`, and `LOOK` was `TALK`.** Both pressed A at the tile ahead; they differed
  only in a precondition, so the palette spent a channel on a duplicate and nothing in it ever
  *walked* the fly to a thing. The starter Pokéballs are `object_event`s with `SPRITE_POKE_BALL`
  and item balls, signs and bookshelves are the same kind of target — none of them people, so
  `GO NPC` is not for them. Slot 2 is now **`GO ITEM`** ("nearest object"), the same walk-and-turn
  as `GO NPC` over the other half of the map's object data: the object sprites
  (`picture >= FIRST_STILL_SPRITE`) and the `bg_event` sign tiles. `TALK`'s gloss becomes
  "talk / look", because that is what the one A press was doing all along. `GO NPC` is now
  "nearest **person**" in fact as well as in its gloss: the two macros partition the sprite list
  rather than both reading all of it.
- **"Unvisited" was never true.** `GO EXIT` asked `MacroState::exit_visited`, whose default is
  `false`, so every exit read as unvisited and the *nearest* one always won — which, for a fly
  standing outside its own front door, is that front door. It is now wired to the adapter's
  `boundary` ledger through `GameAdapter::exit_visited`, a read accessor that changes no version
  string: an exit counts as visited once the ledger holds either half of its `boundary` key, i.e.
  once this run has stood on it or beside it. "Found", not "used" — the `:on` half alone is
  unobservable for a town door, which fires on the step onto it. The fallback to every exit when
  the unvisited set is empty is unchanged.

## 4. Execution

```rust
pub struct MacroId(pub u8);             // slot within the current scene's palette
pub struct Palette { pub scene: Scene, pub slots: [Option<MacroSpec>; 6] }
pub struct MacroSpec { pub name: &'static str, pub kind: MacroKind }

pub trait MacroExecutor {
    /// Begin a macro. Returns Err if the slot is unbound or the precondition fails; nothing
    /// is pressed in that case.
    fn start(&mut self, palette: &Palette, slot: MacroId, memory: &mut dyn MemoryReader)
        -> Result<(), MacroRefused>;
    /// Called once per game frame while running: returns the button mask to hold this frame.
    /// `None` means the macro has finished (or aborted) and the fly is consulted again.
    fn step(&mut self, memory: &mut dyn MemoryReader) -> Option<u8>;
    fn running(&self) -> Option<&'static str>;
}
```

- A macro owns the buttons from `start` until `step` returns `None`. Hard cap 600 frames
  (10 s); a macro that exceeds it aborts with `MacroAbort::Timeout`, logged and shown.
- A scene change mid-macro (a wild battle starting during `GO EXIT`) aborts the macro at the
  next frame; the fly is consulted on the new scene's palette.
- Between macros the sim consults the decoder's current channels exactly as raw mode does; the
  active exclusive channel or the most recent pulse selects the slot. A decision is taken at
  most once per `holdMs`; while no channel is active nothing happens.
- Overworld movement uses A* over the current map's walkable tiles (collision from the tileset's
  collision table and the map blocks, warps and connections from the tables the adapter already
  reads) with one step per tile and a per-step check that the player moved; three failed steps
  abort with `MacroAbort::Blocked`.
- Battle and menu macros navigate by reading cursor state from WRAM, never by counting presses.

## 5. Learning and rewards

Unchanged reward catalog and adapter version. Every macro start and finish is a `FeedEvent`
(`kind: macro`, label = name, outcome = done/blocked/timeout/refused) so the ticker shows it and
the event log records it. Reward attribution is what the design is for: the KC->MBON rule
already reinforces the recent spike history on payout; the interval between a macro decision and
its payout is now one macro long.

## 6. Feed and control

- `docs/feed-protocol.md` header gains `game.scene: string`, `game.mode: "raw" | "palette"`,
  `game.palette: {slot: number, name: string}[]`, `game.macro: {name, sinceMs} | null`. Additive,
  protocol version unchanged.
- `docs/control-api.md`: `/status` mirrors the same fields. No new endpoints.
- Stage (the operator, 2026-09-16): the palette is always visible directly under the Game Boy screen,
  not inside a tab. Layout (the operator): "slide the fly over and put the macro palette right next to
  it": the strip under the screen becomes the 3D fly on one side, shrunk to make room, and the
  palette beside it filling the rest of the strip's width; the two share one baseline and one
  frame so they read as a single instrument. Six cells in the D-pad/A/B order, each with the macro's name and a two or
  three word gloss of what it does in this scene ("GO EXIT · nearest door"), the button it hangs
  off as a small glyph, empty cells shown dim as "—". When the fly chooses one the cell lights
  and stays lit while the macro runs, then shows its outcome for a beat (done / blocked / timeout).
  Scene changes re-deal the cells with the same halfway-arcade motion the rest of the rail uses. A
  MODE chip (RAW / PALETTE) sits by the version chip. The existing button afterglow stays, since
  the macro's presses are real presses. Ticker shows macro events. Terse copy, PNG mockups for
  The operator before it is wired.

## 7. Measurement (the gate for turning it on in release)

From the same archived checkpoint, 6 brain hours each, unthrottled, on the dev box: raw mode vs
palette mode, reporting rungs reached, time to each rung, macro outcome counts, and reward per
brain hour. Ship palette mode as default only if it reaches rung 6 or higher in fewer brain hours
in 3 of 3 runs. The decoder preset is not touched in either arm.

## 8. Work split

- **Agent A, scenes and WRAM:** `docs/design/macros-wram.md`, `pokemon_red/scene.rs`
  (`Scene`, `detect`), and the state accessors macros need: party (species, HP, max HP, moves,
  PP, status), enemy species and HP, battle menu and cursor state, text box open flag, start menu
  state, shop state, money, bag contents, NPC sprite positions and facing, collision for the
  current map (walkable predicate over tile coordinates), warps and connections. Tests: synthetic
  WRAM traces and ROM-gated checks from the archived snapshots under the repo's fixture policy
  (never the ROM itself).
- **Agent B, executor:** `pokemon_red/macros/` with `Palette::for_scene`, A* pathing, the
  macro scripts and `MacroExecutor`. Depends on A's accessors by the signatures in
  `macros-wram.md`; until A lands, B codes against a trait `GameState` declared in
  `macros/state.rs` and A implements it.
- **Agent C, integration (after A and B):** sim loop mode, feed and status fields, stage chip and
  palette cells, event log, measurement harness (`examples/palette_bench.rs`), docs.

Not for release until section 7 passes and the operator has seen the screen.

## 9. Plan mode: the map plans, the fly drives (the operator, 2026-09-16 ~16:30 UTC: "we want the fly
## to act not roll dice" … "yes. go frontier. approved.")

A third mode beside `raw` and `palette`: `FLY_MACRO_MODE=plan`. The doctrine sentence for it is
"the map plans, the fly drives".

- **The plan.** Each scene has a deterministic policy that orders its macros by game knowledge
  into a plan; the palette shows that order, top row first. Overworld: `GO OBJECTIVE` (path
  toward the next unreached ladder rung's place, using the map, warp, connection and object data
  the adapter already reads; if the rung has no known place, fall through), then `GO EXIT`
  (unvisited), `GO ITEM` (untalked object), `GO NPC` (untalked person), then `GO FRONTIER`.
  Dialog: `NEXT`. Menu: `CLOSE`. Battle: `ATTACK`, `SWITCH` when the active Pokémon is under
  a quarter HP and a healthier one exists, `RUN` for a wild battle when the party is weak.
  Shop and PC: `LEAVE`. No macro is ever started by the policy itself.
- **The drive.** The fly's readout chooses when and how hard, never which: any active direction
  channel executes the plan's next step; A repeats the step just finished; B skips to the next
  fallback in the order; silence waits. One decision per `holdMs` as before; a running macro
  owns the buttons; scene change re-plans.
- **`GO FRONTIER`** replaces `WANDER` in every mode: walk to the nearest tile bordering ground
  this run has never stood on, using the exploration ledger, and face it. No random steps
  remain anywhere in the palette.
- **Screen.** Same strip: rows in plan order, the running row lit, the MODE chip says `PLAN`.
  Honesty panel line for this mode: "the map plans, the fly drives".
- **Gate.** Same bench as section 7, three arms: raw, palette, plan.

### 9.1 Building exits, floor changes and routes (the operator, 2026-09-16: "make a distinction between building exit and something like stairs")

`GO EXIT` conflated three different moves. Every warp on a map is classified by *where it goes*,
from the destination map id the adapter already reads out of `wWarpEntries`:

- an **exit** — its destination is an outdoor map, i.e. leaving a building;
- a **passage** — its destination is another interior map: stairs, a ladder, a door between rooms;
- a **route** — on an outdoor map, a step off a connected map edge, or a door into a building
  whose interior this run has not visited.

"Outdoor" is `constants/map_constants.asm`'s own ordering rather than a tileset read: the towns,
Indigo Plateau, Saffron and every route are map ids `$00`..`$24` (`ROUTE_25`), and `REDS_HOUSE_1F`
at `$25` begins the interiors. A warp whose destination is `LAST_MAP` (`$ff`) — which is how
pokered writes a building's own front door — counts as outdoors, because that is what it is on
every interior map in the game. Verified with the survey method on the cartridge
(`tests/rom_macros.rs`): Red's ground floor has one passage (the staircase, destination `$26`) and
two exits (the doormats, destination `$ff`), and Pallet Town has no exits and no passages, only
routes.

So `GO EXIT` becomes three macros:

| macro | gloss | target |
| --- | --- | --- |
| `GO OUT` | leave building | nearest unvisited exit |
| `GO WARP` | floor change | nearest unvisited passage |
| `GO ROUTE` | next area | nearest unvisited connection, or door into an unvisited interior |

The overworld palette keeps six slots and depends on which side of a door the fly is standing:

| slot | indoors | outdoors |
| --- | --- | --- |
| 0 (UP) | `GO OUT` | `GO ROUTE` |
| 1 (DOWN) | `GO WARP` | `GO NPC` |
| 2 (LEFT) | `GO ITEM` | `GO ITEM` |
| 3 (RIGHT) | `GO FRONTIER` | `GO FRONTIER` |
| 4 (A) | `TALK` | `TALK` |
| 5 (B) | `MENU` | `MENU` |

Indoors `GO NPC` is not on a channel at all; it stays in plan mode's fallbacks, because a room
with a person in it almost always has that person between the fly and the door, and `TALK` already
reaches whatever is ahead.

The plan's overworld order becomes `GO OBJECTIVE`, then `GO OUT` or `GO ROUTE`, `GO ITEM`,
`GO NPC`, `GO WARP`, `GO FRONTIER` — with `GO WARP` promoted above `GO OUT` when a passage on
this map leads to the objective's own map, which is what "the objective is on another floor of the
same building" is observable as.

**Amended 2026-09-16, after the first cartridge run of plan mode**
(`services/flysim/crates/flysim/tests/rom_plan.rs`): the way onward — `GO OUT` or `GO ROUTE` —
ranks *below* `GO ITEM`, `GO NPC` and `GO WARP` when the fly is standing on the objective's own
map. Every place the rung catalog carries is a map and nothing finer, so arriving on the
objective's map makes `GO OBJECTIVE` fall through, and with the order above unchanged the next
entry is `GO OUT` — which walks straight back out of the building the rung is in. The cartridge
showed it: plan mode reached Oak's lab in four hundred frames and then bounced between the lab and
Pallet Town, once per hold, for as long as it was driven. Leaving the map the ladder is asking for
is the one move the map knows is wrong, so it goes last but for the frontier.

**Amended 2026-09-16 (the operator, the contract answer to the rung-4 stall):** `TALK` belongs in the
overworld order, and it ranks **first** whenever the fly is facing an untalked object or person.
Otherwise it is absent from the plan: a tile ahead with nothing on it is not a reason to press A.
So the plan's two-step for a starter Pokéball, a sign or a villager is `GO ITEM` (or `GO NPC`) and
then `TALK`, and A keeps its drive meaning of "repeat the step just finished".

Without it plan mode could not take a starter at all, which is what the first plan smoke hour
measured: rung 4 in 0.005 brain hours and then fifty-nine minutes on it, because the one press that
accepts a Pokéball can only come from the plan and the plan did not carry it
(`infra/docs/macros-bench.md`).

Building it took three attempts on the cartridge, and the two halves of "facing an untalked thing"
turned out to live in different places:

- **Facing something** is the plan's own test: the tile ahead is one of the map's object sprites,
  one of its people or one of its `bg_event` signs.
- **Untalked** is `GO ITEM`'s and `GO NPC`'s choice of target — "nearest *untalked*" is the nearest
  thing with a tile beside it this run has never stood on — so the walk that puts the fly in front
  of something is what makes it untalked, and a thing the run has already stood beside is not
  walked to again. It cannot be answered from *in front of* the thing: nothing in WRAM records
  that a shelf was read, the fly's own tile enters the exploration ledger three stable samples
  after it arrives (before the next decision is due), and a rule about the thing's *other* sides
  never closes for an object on a table, which has only one. Both readings were measured and both
  failed — the first offered the press once in 549 walks, the second offered it 615 times at one
  shelf — and neither took a starter.
- **What bounds the press is the drive**, which is where a repeat belongs, and it needed two rules
  that are now part of what "scene change re-plans" means. The plan is re-derived every frame
  either way; what the cursor does is: it is kept **per scene**, so the dialog a press opens does
  not put the overworld back at the press that opened it; and it returns to rank 0 whenever the
  plan's **head changes**, because a different macro at rank 0 is the map saying there is a better
  thing to do than the one the fly was working down towards. `TALK` is exactly that kind of entry:
  it becomes rank 0 the moment `GO ITEM` finishes facing a ball.

With both rules the stub readout of `services/flysim/crates/flysim/tests/rom_plan.rs` — one
direction burst per `holdMs`, no A, no B, no brain — takes a starter from Oak's lab in 3.5 brain
minutes on 96 macros.

### 9.2 Visited, untalked, and the way to the next town (the operator, 2026-09-16 ~22:10 UTC: "fly is looping")

Kept by section 12: this is knowledge inside macros, not ranking. The stream showed a fly on rung 5
in Pallet Town cycling through the town's houses — GO ROUTE, GO WARP, TALK, GO OUT, GO ROUTE —
and never taking the Route 1 connection north. Four things were wrong at once and all four are in
this chain.

- **"Visited" is a question about the map on the other side.** A connection or a door counts as
  visited only once the run has stood on the map it leads to (`GameAdapter::map_visited`), never
  because the fly once stood on the boundary tile. The old test was the adapter's `boundary`
  ledger, which pays for standing *on or beside* an exit, so one walk along the top row of Pallet
  Town marked the way to Route 1 used up while a front door the fly had been through twice still
  read fresh — and with every route "visited" the fallback to all of them let *nearest* pick a
  house door, once per hold, for ever. `exit_visited` stays on the adapter as the boundary
  question; it is no longer what "somewhere new" means.
- **A map edge knows where it goes.** `wCurMapConnections` is four bits and this crate has no
  reviewed symbol for the connection headers beside it, so a step off an edge used to carry no
  destination at all — invisible to "unvisited interior" and unusable as "the way to Viridian".
  `macros/geography.rs` is a static adjacency table (the routes and towns of
  `constants/map_constants.asm` with the connection tables of `data/maps/headers`, plus the doors
  the ladder's places need), and `next_hop` is breadth-first over it. A map with no row has no
  known neighbours: an exit into it counts as unvisited and `GO OBJECTIVE` falls through. Nothing
  is guessed.
- **Ranking.** Exits into a map this run has not stood on come first, and among those a
  *connection* before a door: the next area is a bigger unknown than a room off this one. When
  everything of a kind is exhausted, the ones that take the first hop toward the objective; failing
  that, all of them, nearest.
- **`GO OBJECTIVE` crosses maps.** When the rung's place is on another map, the goal is whichever
  of this map's exits takes the first hop of the breadth-first route, asked again on arrival. The
  search itself steps into `Walkable::Unknown` at eight tiles a step — a map edge is off the screen
  buffer by definition, so a search that refused every unknown tile could not plan one step toward
  Route 1 from the middle of town. A known way round always wins, and a wrong guess costs three
  failed steps.
- **"Untalked" is a ledger, not a guess about the ground.** A `talked` ledger keyed by map and
  object index — sprite slot, or a sign's text id — is written when a `TALK` *finishes* facing the
  thing. `GO NPC` and `GO ITEM` are offered only for targets it does not hold, with no fallback to
  the talked ones, so a map whose people have all been talked to leaves the button off the pad
  entirely; `TALK` is on the pad only while the tile ahead holds something untalked. The two
  readings tried before it — the ground beside the thing, and the thing's other sides — each failed
  in a different direction, and the "never stood on a tile beside it" one never closes at all for a
  villager standing in the open, which is why the fly talked to the same people all hour. The
  ledger lives in the executor layer and never reaches the checkpoint: a restored run offers every
  person once more, which is the honest answer for a ledger that did not survive.

**Rung places** (`pokemon_red/mod.rs`). A place is a map and, where the game makes one derivable,
an edge of that map; no rung carries a tile, because a tile would have to come from map data in a
ROM bank this crate cannot reach. Two are conditional, because the game moves them: rungs 4 and 5
are Pallet Town's **north connection** until `EVENT_FOLLOWED_OAK_INTO_LAB` — Oak stops the fly on
the path out of town and walks it in — and the lab afterwards; rung 6 is **Viridian's mart** until
`EVENT_GOT_OAKS_PARCEL` and the lab after it, because the rung is the parcel *delivered* and the
errand starts two maps north. The old catalog answered "Oak's lab" at rank 5, which told a fly
standing beside the lab to go back into it. Badge rungs name the gym where the interior ordering
derives its id from a verified anchor (Pewter, Cerulean, Viridian) and the town otherwise. Still
unknown, and left `None`: rung 19, the Thunder Badge, because Vermilion Gym's id is not derived.

## 10. Biased slots: the scene weighs the fly's vote (the operator, 2026-09-16 ~18:45 UTC: "make each
## macro slot its own button and have a bias per scene. battles are very structured.
## exploration, less so." … "ok good. plan it build it push it.")

Replaces the drive rules of section 9 (direction executes / A repeats / B skips). Mode name
stays `plan`; sections 9's policy ordering, `GO OBJECTIVE`, `GO FRONTIER` and the exit split are
unchanged.

- **Each slot is its own channel.** The six slots keep the fixed D-pad/A/B binding of section 1,
  so a slot's channel is the fly's population for that button. The decoder is not modified; the
  game layer reads its per-channel normalized scores.
- **The scene's prior.** The section 9 policy ranks the scene's bound macros; the rank becomes a
  prior over slots: rank 0 gets 1.0, rank 1 gets 0.5, rank 2 gets 0.25, and so on; unbound
  slots 0.
- **The blend.** Once per `holdMs`, for every bound slot,
  `score = w(scene) * prior[slot] + (1 - w(scene)) * fly[slot]`, where `fly[slot]` is the
  channel's normalized score in [0, 1]; the slot with the highest score starts, ties to the
  lower rank. The fly must be awake: if every `fly[slot]` is below the readout's own activity
  floor, nothing starts (silence still waits; the scene never acts alone).
- **Weights `w(scene)`** (the operator, defaults; env-tunable `FLY_MACRO_BIAS_<SCENE>`):
  battle 0.9, battle-switch 0.95, dialog 0.95, menu 0.9, shop 0.9, pc 0.9, overworld 0.3,
  unknown 0.5. Overworld is mostly the fly; battles are mostly the plan.
- **Screen.** Each cell gains a thin weight bar (the scene's prior for that slot, scaled by
  `w`) under the gloss; the cell that fired lights as before. Honesty line for this mode: "the
  scene weighs the fly's vote". MODE chip still `PLAN`.
- **Feed.** `game.palette[i]` gains `prior: number` (0..1, already weighted by `w`) — additive.
- **Gate.** Same bench, arms raw / palette / plan(biased); report macros chosen by rank.

**Amended 2026-09-16 (the operator, after the first plan(biased) hour): `fly[slot]` is the spread across
the bound slots, not the distance above each channel's own baseline.**

```
fly[slot] = (score[slot] - min) / (max - min)     over the bound slots at that decision
```

so the fly's strongest bound channel votes 1.0 and its weakest 0.0. Nothing starts when the
strongest bound channel is at or under the readout's activity floor, and nothing starts when every
bound slot reads exactly alike — a fly with no preference to express is not a fly choosing the
head. The one exception is a plan of a single entry, which has no spread at all: an awake fly votes
1.0 for it, because otherwise a one-entry plan could never run and a dialog is a one-entry plan.

The first reading of "the channel's normalized score in [0, 1]" was each channel against its own
baseline, `clamp(score - 1, 0, 1)`, and the measured hour is what ruled it out: the readout's score
is a ratio around 1 and a settled network's directions sit inside a 9% spread (`docs/readout.md`),
so every vote landed in 0..0.1, every decision went to rank 0, and the fly chose **0 of 4,399
macros** — the blend was the plan alone (`infra/docs/macros-bench.md`). Against the spread, a fly
that leans at all leans by 1.0, which is what makes `w = 0.3` "mostly the fly" and `w = 0.95`
"mostly the plan" mean what the weights were chosen to mean. The weights, the prior ladder, the
tie rule, the screen and the feed field are unchanged.

## 11. Macro channels of their own (the operator, 2026-09-16 ~21:15 UTC: "not double mappings on
## existing buttons" … "the real keys and the macros. all pressed by neurons like buttons.")

Section 10's blend stays, but the six macro slots stop borrowing the button channels. The eight
Game Boy buttons keep their eight descending-neuron populations (`command_0..7`, round-robin over
the 1,305 descending neurons). The six macro slots get six populations of their own, `macro_0..5`,
built from the 96 mushroom body output neurons (MBONs) split round-robin into six groups of 16.
This is the honest choice: the MBONs are exactly the neurons whose input synapses the reward rule
changes, so a learned preference for a macro in a scene is the mushroom body doing what it does in
the real fly. The roles are a dataset artifact (`circuit-roles.json`, `tools/build_flywire.py`),
regenerated with checksums updated in `tools/artifact-checksums.txt`; the neuron ids, edges and
kernel are untouched, so the compatibility string must not move (rates by role are restored by
name; new roles start at zero).

- Decoder: a second exclusive group `macros` with channels `m0..m5` over `macro_0..5`, same
  hold, hysteresis, fatigue and blocked rules as the direction group; no pulses. In `plan` mode
  the macro group's normalized scores are the fly's votes of section 10; the button group is
  ignored for decisions but still drives the on-screen button afterglow so the audience sees the
  motor side alive. In `raw` and `palette` modes the macro group is unused.
- Screen: each palette cell's glyph becomes its channel, `M1`..`M6`, and the SENSES rate bars
  gain a MACROS row beside DRIVE and A/B.
- Feed: `game.palette[i].channel: string` (additive). Honesty line: "the mushroom body picks
  the macro; the descending neurons press the buttons".

**Amended minutes later (the operator: "better yet map each macro type to a new motor neuron").** Not
six slot channels that change meaning per scene: every macro TYPE gets a population of its own,
stable across scenes, so a channel always means the same action and learning can attach to it.
Types (22): GO OBJECTIVE, GO OUT, GO WARP, GO ROUTE, GO ITEM, GO NPC, GO FRONTIER, TALK, MENU,
NEXT, YES, NO, CLOSE, CONFIRM, BACK, ATTACK, SWITCH, ITEM, RUN, BUY POTION, BUY BALL, LEAVE.
Populations `macro_<type>` are drawn round-robin from the pool of the 96 MBONs plus the 110
brain motor neurons (206 neurons, 9 or 10 per type; a single neuron would be too noisy to read,
a population is what a button is too). The eight buttons keep `command_0..7`. Decoder: the
`macros` exclusive group has 22 channels; at a decision only the channels of the scene's bound
macros compete (the others are masked, the same mechanism as a blocked channel), and section 10's
blend uses their normalized scores. Screen: a cell's glyph is the type's short channel tag (e.g.
`MB·GO`, `MB·ATK`), and the SENSES tab's MACROS row shows the bound channels' rates.

## 12. Simplification: macros are buttons (the operator, 2026-09-16 ~21:40 UTC: "it seems like we're
## defeating what we set out to do … perhaps a simplification." … "good. build it.")

Supersedes sections 9, 9.1 (the drive rules), 10 and the blend of section 11. Kept from them:
scene detection, the executor and every macro script, GO OBJECTIVE / GO OUT / GO WARP /
GO ROUTE / GO ITEM / GO NPC / GO FRONTIER, the exit/stairs/route split, the visited and talked
ledgers, the rung places, and the per-type populations of section 11.

- **Macros are buttons.** Each macro type is a button on the pad, pressed by its own population
  (`macro_<type>`, section 11), decided by the decoder's `macros` exclusive group with the same
  hold, hysteresis, fatigue and blocked rules as the direction group. Whichever bound channel
  wins, starts. No ranking, no prior, no scene weight, no cursor.
- **The scene decides which buttons exist.** Overworld: GO OBJECTIVE, GO OUT or GO ROUTE,
  GO WARP (indoors), GO ITEM, GO NPC, GO FRONTIER, TALK (only when facing something untalked),
  MENU. Dialog: NEXT (YES / NO when a choice is open). Menu: CLOSE, CONFIRM, BACK. Battle own
  turn: ATTACK, SWITCH (ITEM when a potion is held and HP is low; RUN in a wild battle). Battle
  text between turns: NEXT (it waits for a press; live deadlock fixed in v0.2.4). Forced
  switch: SWITCH. Shop: BUY POTION, BUY BALL, LEAVE. PC: LEAVE. Unbound channels are masked
  from the decision. A precondition failure means the button is not on the pad.
- **Knowledge lives inside macros**, never in the choice. The only thing that can make the
  choice deliberate is the reward rule acting on the MBON populations.
- **Modes:** `raw` (buttons only) and `macros` (buttons plus the scene's macro buttons; a
  raw button press and a macro cannot overlap: while a macro runs it owns the pad). `plan`
  and `palette` are removed; env values `palette`/`plan` map to `macros` with a warning for one
  release.
- **Screen:** cells in a fixed order by type, present cells lit dim, absent cells hidden, the
  running cell lit; glyph = the channel tag; no weight bar. MODE chip: RAW / MACROS. SENSES
  gets the MACROS rate row. Honesty line: "the scene sets which buttons exist; the fly presses;
  the mushroom body learns which".
- **Feed:** `game.macroMode: "raw" | "macros"`, `game.palette[i]` keeps `slot`, `name`,
  `gloss`, `channel`; `prior` removed.
- **Gate:** bench arms raw vs macros, 6 brain hours from the live checkpoint.

### 12.1 Target ledgers (the operator, 2026-09-16 ~23:30 UTC, 45 minutes live in Viridian City)

The event log cycled `GO ITEM start, GO ITEM blocked` — the nearest object was unreachable from
where the fly stood — and `GO NPC start, GO NPC done`, walking to the same person again and again,
while `GO OBJECTIVE` toward Route 2 sat on the pad unchosen. Two ledgers, both inside the macros
where section 12 says knowledge lives, both read by the *target* choice and neither by the choice
of macro:

- **Blocked.** When `GO ITEM`, `GO NPC`, `GO OBJECTIVE`, `GO OUT`, `GO WARP`, `GO ROUTE` or
  `GO FRONTIER` aborts `Blocked` or `Timeout`, the target it set out for — the exit, the thing or
  the tile, keyed with the map — is excluded from that macro's target choice for **10 brain
  minutes** (`FLY_MACRO_BLOCKED_MINUTES`). So the macro picks the next candidate, and with no
  candidate left the button is not on the pad. A window rather than a permanent entry because
  "unreachable" is a fact about where the fly was standing: a ledge or a person in a doorway stops
  being in the way once the fly has moved, and a target excluded for ever would lose the map its
  item.
- **Reached.** When `GO ITEM` or `GO NPC` *completes* — arrived and facing, which is all either
  one promises — the target is retired for the rest of the session. Only a completed `TALK` marked
  a person talked before this, and the fly is free never to press A, which is why the same
  villager was worth walking to for forty-five minutes. A later `TALK` writes the talked ledger,
  which excludes it anyway. `GO FRONTIER` needs no entry here: a frontier tile stood on stops
  being a frontier through the exploration ledger, which is now a test rather than a claim.

The abort's target is the one the walk set out for, recorded at `start` beside `TALK`'s facing
entry, because by the time a walk gives up the candidate list has moved on. Both ledgers are
session state in the executor layer and neither reaches the checkpoint, exactly like `talked`: a
restored run offers every target once more. A rollback's cancellation writes neither — that is the
loop's doing, not the map's. The decoder, the reward catalog, the adapter version and the
compatibility string are untouched (648 bytes, `0d9bfde7…707fa`).

**Screen, same review:** the lit cell hid the macro's name — `--ink-0` on a 30% accent wash, which
measures 5.4:1 and reads as pale green-grey type on muddy amber. The accent moves to the row's
*glyph chip* (page accent behind the darkest ink, 9.5:1) and the row keeps a 16% wash the name
stands clear of (7.4:1); the three outcome states take the same arrangement at 10%. Theme tokens
throughout, so the three themes keep their own palettes. `apps/stage/tests/e2e/macros.spec.ts`
holds the floor at 4.5:1 for the lit row and each outcome, and the baselines are re-shot.

### 12.2 Traps: a macro that completes without moving (2026-09-17, rung 8, two hours seventeen)

`infra/docs/macros-traps.md` is the reproduction, the audit of every macro and scene binding, and
the before/after of the loop detector this added
(`services/flysim/crates/flysim/examples/trap_hunt.rs`). The rule the whole of it comes from, and
the one to hold new macros to:

> **A macro that completes without moving because its precondition is already satisfied where the
> fly stands is a trap.** It must either not be on the pad, or count as reached.

The live loop was GO ROUTE into a house, GO OUT straight back out, GO NPC at the girl by the door
and GO FRONTIER one tile, every three brain seconds, on five tiles, with GO OBJECTIVE off the pad.
What changed, all of it inside the macros:

- **The frame cap is not a fact about the target.** A `Timeout` no longer excludes what the walk was
  aimed at when the walk ended *nearer* a goal than it began: it costs one of `TIMEOUT_STRIKES`
  (three, section 4's own number at the macro's scale) and the next hold resumes the walk. A
  `Timeout` that ended no nearer excludes the target at once. Without this, every attempt at a
  connection more than ten seconds' walk away excluded it for ten brain minutes — and took
  `GO OBJECTIVE` off the pad with it, because both aim at the same `TargetKey::Exit`.
- **`GO ROUTE` has no last-resort fallback.** With nothing fresh and nothing on the way to the
  objective it leaves the pad, instead of letting "nearest" pick the door underfoot. `GO OUT` and
  `GO WARP` keep the fallback of section 9.2's third tier, because a room still has to be
  leavable.
- **A front door nobody can name is not somewhere new.** A `LAST_MAP` warp whose town
  `geography::outdoor_of` cannot name counts as *visited*: the only way onto an interior map is
  through its own front door, so the map outside it is one the run has stood on.
- **A thing the fly is already facing is `TALK`'s, not `GO NPC`'s or `GO ITEM`'s.** "Nearest
  untalked" excludes the tile ahead, which is the state `TALK`'s own precondition is.
- **A frontier tile whose new ground cannot be stood on is excluded.** The arrival press faced it,
  which stays `Done`, and the tile enters the blocked ledger, because the exploration ledger only
  ever records ground somebody stood on.
- **A `no route` refusal records what it could not reach.** The preconditions are deliberately the
  cheap question and the route search is the real one, so a bound macro could refuse once per hold
  for ever while its candidate list never changed.
- **Every playable scene deals a pad that can press something.** Dialog: `NEXT`, `YES`, `NO` — there
  is no WRAM observable for "a choice is open" (`docs/design/macros-wram.md`), and A and B both
  advance a plain box, so the approximation costs nothing and the fly can answer no. `Unknown`:
  `NEXT`, `BACK`, because the Pokédex, the trainer card and OPTION land there and B is what leaves
  them. Menu: `CLOSE`, `CONFIRM`, `BACK`. Shop: `BUY POTION`, `BUY BALL`, `LEAVE`. Battle own turn:
  `ATTACK`, `SWITCH`, `ITEM`, `RUN`, then `NEXT`; forced switch: `SWITCH`, `NEXT` — the last entry of
  each is what keeps a turn with nothing to attack, switch, heal or flee with from dealing an empty
  pad, which is the battle-text deadlock of v0.2.4 in a different scene.
- **Six rows, eight buttons: the tail survives.** The overworld order is truncated in the middle, so
  `GO FRONTIER` — the fallback that always has somewhere to go — is never the entry that is cut.

Still open, and named rather than worked around: the name-entry screen reads `Unknown` and needs
START, which the pad has no button for; `MENU` is on no pad at all, because the overworld row is
full at six slots and widening it is a feed and stage change; the mart cannot buy until
`MacroState::shop_stock` reads the counter's stock. The ratchet is not a backstop for any of this —
at three attempts on one rung it stops firing by design, and one new tile resets its stall window —
which is why the loop detector runs on the dev box before a release.

### 12.3 Walks that finish: a budget from the plan, a route it keeps, a resume (2026-09-17 08:10 UTC, eight hours on rung 8)

Section 12.2 left the frame cap as a fact about the *macro* and fixed what a `Timeout` means. This
amends what it measures. Live from 02:04 to 07:00 UTC after v0.3.2 the fly sat on rank 8 in
Viridian City and the last 40 KB of the event log held 116 `GO ITEM start` with 117 `GO ITEM
timeout`, 36 `GO ROUTE` starts and timeouts, 12 `GO OBJECTIVE` starts and timeouts, and nothing
else: **every walk spent its whole 600-frame cap and not one macro completed in five hours.**
`infra/docs/macros-traps.md` has the reproduction and the numbers; the mechanism was two tiles
wide.

- **The frame cap is not a fact about the ground.** A walk's cap is now its own plan's: 24 frames
  per planned tile plus `FRAME_CAP` as the floor, capped at **60 s of brain time** (3,583 frames).
  Viridian City is about twenty tiles by eighteen and 600 frames is twenty tiles of walking at
  worst, so the walk from its south end to the Route 2 connection could not finish inside one
  macro however well it went. Every other script keeps section 4's flat ten seconds.
- **A walk that is making progress does not time out.** The budget only ends a walk that has got
  no nearer a goal for two tiles' worth of frames; the ceiling is what always ends one. "Ever got
  closer" (12.2) is the question a *finished* walk is judged by; "closer lately" is the question a
  running one is.
- **A walk the cap cut short resumes.** The remaining route is kept per target — map and
  `TargetKey`, so `GO OBJECTIVE` and `GO ROUTE` resume each other's journey to one exit — and the
  next start continues it instead of planning the same first tiles again. Only from the tile the
  walk was suspended at: between two holds the fly may press a raw button, take a warp or be
  rolled back, and a route is a list of directions from one tile. Session state in the executor
  layer beside the talked, blocked and reached ledgers; it does not reach the checkpoint, and a
  rollback drops it.
- **The planner commits to its route.** The walkable predicate's window is the ten-by-nine screen
  buffer and it moves with the player, so an A* re-planned after every tile is an A* over a
  different map after every tile: a tile priced as plausible ground at eight becomes a wall as the
  window reaches it, the cheapest route flips to the far side of the obstacle, and the step back
  makes the tile unknown again. The route is planned once and followed while the player is still
  on it; it is re-planned only when the ground says something new — a step was refused, the plan
  ran out, or the player is not where the plan left it. This is 12.2's row 23, which three strikes
  bounded rather than fixed.
- **A refused step is a wall in the direction it refuses.** Ledges (`HandleLedges` matches a
  facing, the tile stood on and the tile in front against `LedgeTiles`, and hops two tiles) and
  tile-pair collisions (`TilePairCollisionsLand`/`...Water` refuse a step *between* two tiles that
  are each passable) are invisible to the tileset collision list the predicate reads. The ledge
  tile is absent from every passable list, so the search already treats a ledge as a wall in both
  directions and declines the hop; a tile-pair refusal is measured instead — a step that spends its
  whole window with the player where it started becomes a directed wall for the rest of that walk
  and the re-plan goes round it. Per walk, carried across a resume, never longer: a refusal is a
  fact about the frame it happened in, and a person in a doorway is the same shape.
- **A goal the search cannot reach is excluded, not walked toward.** The search covers the whole
  map and prices an off-screen tile as passable, so "no goal reachable" means walled off from where
  the fly stands rather than too far to see. It refuses at `start`, presses nothing and writes every
  goal key to the blocked ledger. The closest-approach route was what the two-tile oscillation
  walked: a step toward the nearest reachable tile, a re-plan that answered with the tile just left,
  and the pair of them trading the fly back and forth for the whole cap — while the frame cap read
  as "closing", because it *had* been one tile nearer once.

The decoder, the reward catalog, the adapter version and the compatibility string are untouched.

### 12.4 The gate, and the rung under the rank (2026-09-17, audit row 28)

Section 12.3's walks worked and walked the fly into a wall. `infra/docs/macros-traps.md` has the
reproduction: at Viridian City's (18, 10) the fly stepped north into a still sprite at (18, 9), the
cartridge drew a box reading "This is private property!", took the joypad and walked it back one
tile — and every macro that hit it ended `Done`, because a scene change is "the world moved on
under it" and records nothing. So it was walked into once per hold, for eight hours.

**What shut the road was two rungs under the rank.** The save has `EVENT_GOT_OAKS_PARCEL` set and
`EVENT_OAK_GOT_PARCEL` clear: the parcel is carried and undelivered, so rungs 6 (the delivery) and
7 (the Pokédex) are unearned — while the rank reads **8**, because the rank is the *maximum* over
satisfied rungs and Viridian City had been stood on. The ladder's rungs are not a chain: rung 8 is
a map the fly can walk to, and the two under it are a script two maps south.

- **The objective is the lowest rung this run has not earned**, not `rank + 1`. Nothing in the
  macros learns any plot: the catalog already held rung 6's place (Oak's lab, once the parcel is in
  the bag) and the arithmetic was picking the wrong rung out of it. The rank itself is unchanged —
  it is what the stream shows and what the ratchet measures.
- **A conversation counts as talked to only when the dialog closed without the game moving the
  fly.** `TALK` is one A press and it finishes while the box is still open, so the ledger entry is
  *pending* until the box closes: with the fly on the tile it pressed from and nothing driving it,
  it is written; if the cartridge takes the joypad or the fly ends up somewhere else, it is not; and
  a `NO` drops it, because what the fly said no to is still on offer. The gate was marked talked on
  the first press, which took it off `TALK`'s pad and out of `GO ITEM`'s list — the one conversation
  that might have changed something, had once.
- **A macro the cartridge answers by moving the fly excludes its target for the window.** A scene
  change with `MacroState::scripted` — a non-zero `wSimulatedJoypadStatesIndex` or `wStatusFlags5`'s
  scripted-movement bit — is the cartridge refusing the step, which is a fact about the target, so
  it goes to the blocked ledger like any other refusal. An ordinary scene change (a wild battle
  starting mid-walk) still records nothing. `scripted` is false during an ordinary text box, which
  is what makes it the test for "the conversation ended by moving the fly" rather than for "a
  conversation happened".
- **`YES` and `NO` both stay on the pad** in every text box, unchanged from 12.2's row 15: there is
  no WRAM observable for "a choice is open", and the fly being able to answer no is what makes the
  rule above meaningful.
- **Reached is a window, not a retirement.** 12.1 retired a `GO NPC`/`GO ITEM` target for the
  session on arrival, which also retired every person the fly walked to and wandered away from
  without pressing A, with nothing able to bring it back. Arriving is not talking: the entry now
  expires with the same window the blocked ledger uses, so the loop stays bounded at one walk per
  target per window and the conversation stays available.

The decoder, the reward catalog, the adapter version and the compatibility string are untouched.

### 12.5 An objective can name a target, not just a map (2026-09-17, audit row 29)

Row 29 went live: `GO OBJECTIVE done, GO FRONTIER done, GO OUT done, NEXT done` every three brain
seconds at Oak's lab door, from about 09:00 UTC. Section 12.4 pointed the objective at the right
rung and the rung's place was still only a *map*, so `GO OBJECTIVE` arrived on the lab's own doormat
and called that arriving, and `GO OUT` walked straight back out.

- **A place can name what earns the rung.** `MapPlace::target` is a `PlaceKind` — `Person` or
  `Object` — and the catalog uses it for every rung the early game earns by talking: rung 5 the
  starter's Pokéball (`object`), rungs 6 and 7 the parcel and the Pokédex (`person`, both out of
  Oak's one script), and the badges (`person`, the gym leader). Where a rung's place is only a map,
  nothing changes.
- **Not a sprite slot.** A slot is an index into the map's `object_event` list, which is in a ROM
  bank this crate cannot reach, and `docs/design/ladder.md`'s rule is that an unverified number does
  not go in. A *kind* needs no number: the macro layer already reads the loaded map's sprite list
  and already splits it at `FIRST_STILL_SPRITE`.
- **The arrival is `GO NPC`'s.** `GO OBJECTIVE` aims at the four tiles around the nearest untalked,
  unexcluded target of that kind, with the press that turns to face it, and leaves out the tile
  ahead exactly as `untalked` does — so a fly already facing the thing has nothing left for a walk
  to do, `GO OBJECTIVE` leaves the pad, and `TALK`, whose precondition is precisely that state, is
  what the next hold is offered.
- **The ways out are not on the pad while the objective's own thing is here.** `GO OUT`,
  `GO ROUTE` and `GO WARP` have no candidates while the ladder's target is on this map, untalked
  and unexcluded. Knowledge inside the macro, the way `GO EXIT` knows which exits the run has
  taken: nothing ranks anything, the button simply has nothing to aim at. The earlier attempt at
  this suppressed the way out with nothing leading to the target and stalled the fly in the room;
  the escape hatch is the blocked ledger — a target no walk can reach is excluded for the window,
  the objective's list empties, and the door is a candidate again.
- **The `reached` ledger does not apply to the objective's target.** That ledger is `GO NPC`'s
  politeness; the ladder is not being polite. The rung *is* the conversation, so the thing stays
  worth walking back to until it has been had.
- **The header names the rung the fly is going for.** `milestone.next` is `GameAdapter::next_rung`
  rather than `rank + 1` (`docs/feed-protocol.md`, dated).

Measured from the live checkpoint with the rotating stub: Oak's lab in 1.4 brain minutes,
`EVENT_OAK_GOT_PARCEL` at 9.0 brain minutes on 504 macros, and `EVENT_GOT_POKEDEX` in the same
script — the first time any harness has earned a rung that is a conversation. The decoder, the
reward catalog, the adapter version and the compatibility string are untouched.

### 12.6 The battle's own sub-states (2026-09-17, live in Viridian Forest)

Rank 9, Viridian Forest, an hour and forty-one minutes on the rung: a wild Kakuna, Bulbasaur at
8/28, the FIGHT move list open with TACKLE showing 0 of 40 PP — and the event log was `NEXT start`,
`NEXT done`, every 268 brain milliseconds. `Battle::own_turn` was the **top-level menu alone**, so a
frame with the move list open read as *between turns*, whose pad is one `NEXT`: an A press on
whatever the cursor happens to be sitting on. The cursor sat on TACKLE, TACKLE had no PP, the game
said so, the box closed, the list came back.

- **A battle menu that is accepting input is the fly's turn.** The top-level menu, the move list,
  and the party list outside a forced switch. The party list is the exception that proves it: opened
  by `ChooseNextMon` after a faint it cannot be cancelled and is a forced switch, which has its own
  pad; opened by choosing PKMN it is an ordinary part of the turn.
- **A list whose cursor cannot be placed is not accepting input.** `MoveSelectionMenu`'s
  coordinates appear on a battle's opening frames, before the engine has copied the active Pokémon
  into `wBattleMon*` and while `wCurrentMenuItem` is still 0 rather than the one-based slot the menu
  keeps — measured on the cartridge as `Moves { cursor: None, count: 2 }` with no own Pokémon. That
  frame is between turns, which is what it was before.
- **The pad is the sub-state's.** Main menu: `ATTACK`, `SWITCH`, `ITEM`, `RUN`, then `NEXT` (the
  row-7 backstop for a turn where all four drop). Move list: `ATTACK`, `BACK`. Party list: `SWITCH`,
  `BACK`. Forced switch: `SWITCH`, `NEXT`, unchanged. **`NEXT` is on no pad while a list is
  accepting input**, because there are exactly two answers to a list — choose from it, or back out
  of it — and an A press on whatever the cursor holds is neither.
- **`ATTACK` over an open list** moves the cursor to the best damaging move **with PP** and confirms
  it; `best_move` has always skipped a move with no PP, and what was missing is the button being on
  the pad at all and the script going straight to the move rather than through FIGHT. **If every
  move is out of PP it confirms the cursor's own slot**, because Struggle is the cartridge's answer
  there and the way to it is to choose a move anyway — refusing would take `ATTACK` off the pad on
  the one turn the fly has nothing else to press, which is the shape the loop had.
- **The battle bag is the one sub-state with no observable, and it is named rather than guessed.**
  `BattleMenu` has no item variant: the bag inside a battle reports no cursor through agent A's seam
  (`listing()` answers `None` for it), so an open battle bag reads as `BattleMenu::None` and the
  between-turns `NEXT` advances it — which is survivable, because `ITEM`'s own precondition needs a
  potion and a hurt Pokémon, so the bag is rarely opened at all. Giving it a pad needs a verified
  reading first, exactly as `MacroState::shop_stock` does.

The decoder, the reward catalog, the adapter version and the compatibility string are untouched.

### 12.7 A doormat is ground, and Route 2 is two maps (2026-09-17 17:32 UTC, four hours on rung 9)

Rank 9 (Viridian Forest, next Pewter City), four hours and eighteen minutes on the rung, the fly on
map 50 — `VIRIDIAN_FOREST_SOUTH_GATE`, the ten-by-eight gate house between Route 2 and the forest.
The pad was **`GO FRONTIER` and nothing else**, and the last 40 KB of the event log was 175
`GO FRONTIER start`/`done` pairs, each `done` about 420 brain milliseconds after its start — one
tile of walking — and a new one every 800 ms. `GO OBJECTIVE`, `GO ROUTE` and `GO OUT` were absent.
`infra/docs/macros-traps.md` has the reproduction and the numbers; two things were wrong and both
are here.

- **A doormat is walkable, standable ground, and the reward ledger can never record it.**
  `MacroState::tile_visited` was the adapter's `exploration` ledger alone, and that ledger belongs
  to a *payout*: `sample` inserts a coordinate only on a frame its gate accepts, and the gate
  rejects `wMovementFlags & 0xc7` — bits 0, 1 and 2 are `BIT_STANDING_ON_DOOR`, `BIT_EXITING_DOOR`
  and `BIT_STANDING_ON_WARP`. So **every warp tile in the game was a permanent frontier**. In the
  gate house the two southern doormats are the whole south wall, four tiles from everything, and
  `GO FRONTIER` walked one tile at them for ever. The gate stays exactly as it is — it is the
  reward rule's own and the catalog is not this section's business — and the macro layer keeps its
  **own** answer to its own question: a session ledger in the executor layer beside `talked`,
  `blocked` and `reached`, written once per frame from the tile the fly is standing on, never
  checkpointed, and OR'd with the adapter's. It can only ever make the frontier smaller. A frame
  the cartridge is driving records nothing, because the coordinates and the loaded map header come
  from different frames then.
- **A tile a sprite is standing on is not ground.** The collision table knows nothing about people
  or objects, so a villager standing in the open reads walkable underneath; the fly can never stand
  there while the villager does, so the tile can never enter either ledger. Row 5 bounded that at
  one walk per exclusion window; leaving the tile out of `path::frontier` is what lets the frontier
  **exhaust**, which is what "the button leaves the pad" needs. The gate house had a guard on one.
- **`ROUTE_2` is one map id whose ground is in two halves the player cannot walk between.** Its
  north half touches Pewter City and the forest's north gate; its south half touches Viridian City
  and the forest's south gate; the belt of trees between them needs CUT. `geography` had one node
  for it, so from inside the gate house the breadth-first answer to "which way is Pewter" was
  `ROUTE_2` — two hops, south, through the door the fly had just walked in by. A road that does not
  exist. `geography::Region` is a map *and a piece of it*, `next_hop` is asked from the piece the
  fly is standing in, and the one split row in the table carries the two pieces' own neighbour
  lists and the two doorway rows they are told apart by — 11 and 43, surveyed from the cartridge's
  own warp table. Maps 46, 48 and 49 stay off the graph, which is the table's standing rule for a
  building no rung place needs a route through: 49's two doors are both warps of `ROUTE_2` itself.
- **An exit into the objective's own map is toward the objective, even with no route in the
  graph.** `ways`' second tier asked `next_hop` and gave up when the graph answered nothing, so a
  way out whose every destination was visited had no candidate at any tier and the button left the
  pad. The direct answer is the one `objective_goals` already had and `ways` did not.

What the fly does with all this is unchanged: the gate house now deals a pad with a way north on
it, and which button is pressed is the readout's. The decoder, the reward catalog, the adapter
version and the compatibility string are untouched.

### 12.8 FIGHT is always pressable (2026-09-17, audit row 34)

Section 12.6 put `ATTACK` on the pad over an open move list and made it confirm the cursor's own
slot when nothing has PP, "because Struggle is the cartridge's answer there and the way to it is to
choose a move anyway". The blind spot was one step earlier: **the move list is only ever reached
through FIGHT**, and `ATTACK`'s precondition over the *top-level* menu was `best_move(..).is_some()`
— a move with PP. So the one turn where the fly has nothing left to attack with took the button off
the pad, which is the state measured in Viridian Forest at 99.1 brain minutes: a wild battle, a
party of one, TACKLE, GROWL and LEECH SEED all at 0 PP, a `RUN` the cartridge refused, and a pad of
`ITEM` over a bag it could only close and `NEXT` on whatever the cursor held — which opens the party
list, whose only bound button with one Pokémon is `BACK`. Six hundred thousand frames of it.

- **The precondition is "is there a move list to open", not "is there a move with PP".** FIGHT
  always opens; what is behind it is the cartridge's business. `can_fight` is the Pokémon that is
  out having any move at all, and the one case that still answers no is a Pokémon with no move in
  any slot, which nothing in the game reaches.
- **What FIGHT does with no PP is measured, not assumed.** Surveyed on the cartridge from the state
  above (`examples/scene_probe.rs`, `FLY_PROBE_CATCH=nopp`): backing out to the top-level menu and
  confirming FIGHT leaves the move list **closed on every frame** — `CheckPlayerHasUsableMoves`
  prints "has no moves left!", sets Struggle, and the turn moves on. So the macro's script is
  "confirm FIGHT and stop": a second cursor step would press A at text.
- **A move list that is open with nothing having PP keeps section 12.6's answer.** The same survey
  found the list *can* be open in that state, and confirming a 0-PP slot from it earns "There's no
  PP left for this move!" and the list again. `BACK` is on the pad beside `ATTACK` and leads to the
  menu where FIGHT now works, so the cost is one wasted press rather than a loop — and taking
  `ATTACK` off the list's pad would leave `BACK` alone on it, which is the shape this section is
  about.
- **The fly's own turn never deals one button.** That is the invariant, and it is what the party
  list with a one-Pokémon party breaks if it is reached at all: its one entry is the Pokémon that is
  already out, so there is nothing to choose and `BACK` is the only press. Nothing is added there —
  a second button would be a macro that completes without moving, which section 12.2 calls a trap —
  and what changed is where `BACK` leads: a menu with `ATTACK` on it.

The decoder, the reward catalog, the adapter version and the compatibility string are untouched.

### 12.9 `BACK` belongs to a list, and a ball is not thrown at what the party already has (2026-09-22, rung 9, sixty-nine hours)

Rank 9 (VIRIDIAN FOREST, next PEWTER CITY), sixty-nine hours on the rung, the ratchet's three
attempts spent, the fly on Route 2 and in the forest and mostly in wild battles. Since the restart
the macro starts were `BACK` 135, `THROW BALL` 28, `MOVE 2` 10, `RUN` 5 and `GO ROUTE` 5, and the
event log repeated `RUN blocked, BACK start, BACK done`. `infra/docs/macros-traps.md` has the
reproduction and the numbers; two macros, and section 12.2's one rule between them.

- **`BACK` is a button only where there is a list to leave.** The top-level battle menu never had
  it and does not now: FIGHT, PKMN, ITEM and RUN are the four answers to it and B is not a fifth.
  What dealt it was the *between-turns* row, which section 13.1 gave `NEXT, BACK` for the sake of
  the battle bag -- the bag reads as nobody's turn (12.6), so it lands there -- and which is also
  every frame of battle text, every animation and every turn resolving. On those frames there is
  nothing open, so the B press changes nothing the `NEXT` beside it does not, the macro completes
  in a handful of frames on the tile it started on, and half the pad is a button that cannot move
  the game on. That is exactly **a macro that completes without moving because its precondition is
  already satisfied where the fly stands** (12.2). The row is now the sub-state's, like the own
  turn's: `BACK` with the bag open, `NEXT` alone otherwise, and the move list and the party list
  keep it as before.
- **`RUN blocked` was the script, not the cartridge.** Not "can't escape": `RUN` is one cursor
  navigation, and a cursor macro whose list is not accepting input *waits* rather than pressing
  blind (section 4), for `CURSOR_WAIT` = 180 frames before it reports `Blocked`. So a `RUN` that
  won a hold as the menu closed spent three brain seconds pressing nothing and then gave up --
  row 19 of the audit, bounded and left, and the reason it was visible at all is the `BACK` above
  filling the frames on either side of it. `RUN`'s own precondition is unchanged (13.1: only a
  wild battle the fly is losing).
- **A ball is not thrown at a species the party already holds.** `THROW BALL`'s precondition was
  three facts -- a wild battle, a ball in the bag, room in the party -- and it now has a fourth:
  the Pokémon on the other side is not one the party already has. Viridian Forest holds five
  species and the fly had caught its own, so every throw spent a ball on a Caterpie or a Weedle it
  was carrying, and a catch opens the nickname screen, which reads `Unknown`, needs the START the
  pad has no button for (row 14), and is the one screen in the game neither `NEXT` nor `BACK`
  leaves. The party is the caught set here and it is the honest one: it is the cartridge's own
  lifetime record, it survives the restart the session ledgers do not, and it is in the same
  numbering the enemy is read in -- the internal species index. **`wPokedexOwned` is not asked**,
  because that bitset is by Pokédex *number* and the table converting an internal index into one
  lives in a ROM bank this crate cannot read: `docs/design/ladder.md`'s rule is that an unverified
  number does not go in. It costs nothing measurable -- the button is already off the pad while
  the party is full and nothing in the vocabulary deposits into a box (row 17), so every species
  this run has caught is in the party this reads. An enemy the seam cannot place leaves the button
  where it was: a precondition this crate cannot observe is not a precondition (13.1).
- **The ratchet is not the backstop, and on this rung it cannot be.** `budget_spent` precedes both
  triggers and three attempts on rank 9 were spent sixty-nine hours ago, so no recovery can fire
  again until the rank *improves* -- row 22, contract, and the reason the trap hunt runs on the dev
  box before a release. What has to carry the run is the road: rung 9's place stays
  `VIRIDIAN_FOREST`, the objective is the lowest **unearned** rung (12.4), which is rung 10's
  `PEWTER_CITY`, and `geography::next_hop` from the forest answers `VIRIDIAN_FOREST_NORTH_GATE`
  (47), then Route 2's north piece, then Pewter (2) -- already asserted hop by hop in
  `macros::geography`'s own tests since 12.7. `GO OBJECTIVE` is on the overworld pad there, which
  the ROM-gated run from this checkpoint holds.

The decoder, the reward catalog, the adapter version and the compatibility string are untouched.

### 12.10 No two buttons on a battle pad undo each other (2026-09-22, rung 9, seventy-one hours)

Thirty-five minutes after v0.4.2 deployed, the watchdog flagged the same rung again. Rank 9
(VIRIDIAN FOREST, next PEWTER CITY), seventy-one hours on it, and since the restart the macro
starts were `NEXT` **1264**, `BACK` **1241**, `THROW BALL` 5 and `GO WARP` 3 -- the event log
alternating `NEXT start/done, BACK start/done` every hold, on map 51.

Reproduced from the live checkpoint with the real cartridge and the real brain
(`infra/docs/macros-traps.md` has the numbers): twenty brain minutes, **71,673 frames of them all
in one battle**, 73 of 73 windows flagged, one distinct tile, and the two counts that name the
mechanism -- `BACK` **739 starts on the move list** and `NEXT` **739 on the top-level menu**.

- **The pair was split across two sub-states of one turn, which is why 12.9 did not catch it.**
  `NEXT`'s script is one press of A. On a frame of battle text that A advances the text, which is
  what the button is for. On the *top-level battle menu* the same A press **confirms whatever the
  cursor is sitting on**, and the cursor sits on FIGHT, so `NEXT` opened the move list -- whose
  `BACK` closed it again. Each button was legitimate where it stood: 12.9's rule is about `BACK`
  and a list, and there *was* a list to leave. What is a trap is the **pair**: two buttons that
  undo each other with nothing else changing, so the roll lands on one of them nearly every hold
  and the turn never resolves. Section 12.2's rule, stated at the pad instead of at one macro:
  **no pair of buttons on any battle pad may undo each other with nothing else changing.**
- **`NEXT` is the A that advances text, so it belongs only on a frame with no cursor accepting
  input.** It is off every own-turn pad: the top-level menu, the move list, the party list and the
  bag. The between-turns row keeps it alone, and the forced switch keeps it as row 8's backstop --
  the one arm with a cursor and no `BACK` at all, because a forced switch cannot be cancelled.
- **`MOVE 1` is the backstop the top-level menu keeps.** `NEXT` was there for the turn where every
  other button drops (row 7), and it was the wrong button for the job twice over: it does not end
  a turn, and what it does instead is reopen the list `BACK` had just closed. `MOVE 1`'s
  precondition over that menu is 12.8's -- "is there a move list to open", and FIGHT always opens,
  because `CheckPlayerHasUsableMoves` sets Struggle without opening it -- so it is bound there
  whatever the seam makes of `wBattleMon*`, and its script over that menu is "confirm FIGHT and
  stop", which reads no move at all. A battler the seam cannot read is not a reason to take the
  turn's one ending button away.
- **The bag is the fly's turn, and its pad is the bag's own answers.** `Battle::own_turn` answered
  `false` for it, which was the last frame in the game with a cursor accepting input and no own
  turn -- so it landed on the between-turns row, and that row's `NEXT` on an open bag is the A
  press that *uses* whatever the cursor holds. The invariant is now whole: **a battle frame with a
  cursor accepting input is the fly's turn**, the forced switch excepted because it has a pad of
  its own. The bag's row is `ITEM` (use the thing) / `THROW BALL` / `BACK`; `CONFIRM` is gone from
  it, because it was the same blind A press under another name, and both scripts already navigated
  this list by reading its cursor (7.1 of `macros-wram.md`).
- **`wListMenuID` is not stale, which is why the bag reading stands.** It is zeroed by
  `DisplayTextIDInit` at the start of every text display (`macros-wram.md`), so a battle's text
  frames cannot inherit an `ITEMLISTMENU` from a bag the fly closed. Checked because a stale byte
  there would have put the bag's pad on every frame of battle text.
- **What the harness now holds, rather than the pads alone.** The ROM-gated run from this
  checkpoint asserts that no battle pad deals both buttons, that `NEXT` is on no frame with a
  cursor accepting input, that the longest `NEXT`/`BACK` alternation is under four, and that every
  battle it enters *ends* -- 25 of 25, worst 275 macros -- where v0.4.2 spent a whole hunt inside
  one that never did. The battle rules are asked of the **scene the pad was dealt for** and not of
  `wIsInBattle`: the `$ff` frame a lost battle passes through reads `Unknown`, whose pad is
  `NEXT, BACK` by contract (row 9), and it is not under a battle's rules.

The decoder, the reward catalog, the adapter version and the compatibility string are untouched.

### 12.11 `MENU` opens a screen its own `BACK` closes, and Red's battle menu is two columns (2026-09-22, rung 10, thirty-one minutes after v0.4.3)

Rank 10 (PEWTER CITY, next the BOULDER BADGE), the fly inside a Pewter building, and since the
restart the macro starts were `MENU` **82**, `BACK` **82**, `GO FRONTIER` 8 -- the event log
alternating `MENU start/done, BACK start/done` on **map 0x35**. Reproduced from the live checkpoint
with the real cartridge: the map is the **upper floor of the Pewter museum**, fourteen blocks by
eight, 81 walkable tiles all reachable and four never stood on, one warp at (7, 7) down to the
museum's ground floor (0x34), two signs and three exhibits.
`infra/docs/macros-traps.md` has the probe whole.

- **`MENU` is a trap by section 12.2's own definition, and it was the *only* button left.** Opening
  the start menu changes nothing in the world, so the macro completes on the tile it started on --
  and the scene it opens deals `CLOSE`, `CONFIRM` and `BACK`, of which `BACK` presses B and closes
  it again. Two buttons that undo each other with nothing else changing: section 12.10's rule, one
  scene wider than a battle. **`MENU` is on no pad now.** Not narrowed -- there is nothing behind
  it to press it for, because no macro in the vocabulary uses the start menu except as a scene to
  leave, and "MENU to save the game" is not a macro that exists. It stays a type, a population, a
  tag and a script, so the thirty-one channels, the roles and `--print-compatibility` do not move,
  and the start menu is still the fly's to open with the **raw** START button, which reaches the
  cartridge in macros mode.
- **Why that map's pad was `MENU` and `GO FRONTIER` and nothing else.** Every candidate list on the
  museum's upper floor empties: `geography` has no row for the museum, so `next_hop` answers `None`
  and `GO OBJECTIVE` has nothing to aim at -- the objective itself is right, map 0x36 with a
  *person* on it, which is rung 11's gym leader; the three exhibits and two signs are *reached* by
  `GO NPC` and `GO ITEM`, which retires them for the session (12.1); the four unstood tiles are
  walked or excluded and `GO FRONTIER` empties for the window; and the one way out is classified a
  **passage** and not an exit -- it is a staircase -- so `unexcluded_exits` drops it while the
  blocked ledger rests it, and `ways`' tier 3 for a passage is built out of the same list.
- **`MENU` was also what made an empty overworld pad impossible, so that guarantee moves to the way
  out.** `ways` gains, for a *room*, the last resort `GO ROUTE` has had outdoors since 13.1: with
  nothing else on this map worth walking to (`palette::stranded`), the exits of this kind come back
  **ignoring the blocked window**, the one toward the objective preferred. A target the ledger is
  resting is still the only place to go, and a door the run has been through is a better answer
  than a pad that cannot move. It is a last resort and not a tier: one unstood tile and it goes
  away again, because "the nearest door, once per hold" is row 2's own two hours seventeen.
- **A map with no way out at all is now a genuinely empty pad, and that is said rather than
  papered over.** No map in Red is that -- an interior has its front door or its staircase, an
  outdoor map has its connections -- it is asserted as a named residual in the pad-empty sweep, and
  `game.padEmptyMs` is what reports it if one ever appears.
- **A cursor step waits for the list it was built for.** `THROW BALL` was **63 starts and 63
  `blocked`** on v0.4.3, mean sixty-nine frames, which is the cursor to ITEM, the A that confirms
  it, the twenty settle frames, and a refusal on the next frame. Red keeps one cursor for every
  menu in the game, so "where is the cursor" was half a question: the step that should have walked
  the bag list read the battle *menu*'s four entries instead. A `Listing` now says which list it is
  and a cursor step says which list its target indexes into; a step whose list is not up waits, as
  it already waited for a list reporting no cursor at all (section 4), and takes its press order
  and its budget from that list on the first frame it accepts input. A confirming press that has
  begun still finishes, because the press is what answers the list.
- **And the reason `THROW BALL` never reached the bag: Red's battle menu is two columns, so its
  order is FIGHT, ITEM, PKMN, RUN.** The screen reads `FIGHT PKMN` over `ITEM RUN` and the game's
  index does not: `wCurrentMenuItem` is the row inside the column the cursor is in, and selection
  adds two for the right column. `battle_entry` had `PKMN` 1 and `ITEM` 2 -- the row-major reading
  of the picture -- so **every macro that meant to open the bag opened the party list and every
  macro that meant to open the party list opened the bag**, for as long as the four constants have
  existed. Surveyed on the cartridge: A at `wTopMenuItemX` 15 with `wCurrentMenuItem` 0 opens the
  party list (`wTopMenuItemY` 1, `wTopMenuItemX` 0, `wListMenuID` `$02`) and the game writes
  `wCurrentMenuItem` 2 on the frame after. `THROW BALL` and `ITEM` and `SWITCH` have never once
  completed on the release box; they do now. The fake had the same mistake in its own two-by-two
  geometry, which is why no unit test could have caught it, and it is column-major now.
- **`BACK` on the move list only where the moves can be read.** The other v0.4.3 residual: `BACK`
  was 263 of 797 macro starts and every one was over an open move list. A move list whose battler
  the seam cannot place binds no `MOVE n` at all, so its pad was `BACK` alone -- and closing the
  list is exactly undoing the `MOVE 1` on the menu underneath that opened it, which is 12.10's pair
  with `MOVE 1` in `NEXT`'s place. With nothing readable the pad is `MOVE 1` alone, and its script
  confirms wherever the cursor stands, which is the press that ends a turn.

**What the harness holds.** No scene's set and no scene's pad contains `MENU`, in any state; a room
whose one door the ledger rests still offers it and the pad is that walk; the move list's pad is
`MOVE n` plus `BACK` only with a readable battler and `MOVE 1` alone otherwise; a cursor step waits
rather than reading the list it has already answered; and `battle_entry`'s four numbers are pinned
against the survey. ROM-gated from the live checkpoint: the fly leaves the museum's upper floor on
**frame 182**, and over thirty-three brain minutes across seven maps `MENU` is on no pad, no
overworld pad is empty, and `MENU`/`BACK` never alternate -- where v0.4.3 deals `MENU` on all seven
with 147 starts. ROM-gated from the rung-9 forest checkpoint: `THROW BALL` 15 starts and **0**
blocked and `SWITCH` 17 and **0**, against 6 of 6 and 15 of 17 before.

**Two things measured and not fixed here.** The trap hunt from this checkpoint does not reproduce
this loop at all, and it cannot: the reached and blocked ledgers are session state that a restore
clears (12.1), so a restored fly walks straight out of the museum, and the live loop needed thirty
minutes of ledger to build. What the hunt reproduces instead is **row 41**, the Pokémon Center
nurse's box -- 62,804 of 71,673 frames on one tile of map 0x3a with `YES` 1,278 of 1,295 macro
starts -- and that is the next trap. Inside a battle the largest residual is `MOVE n`: 890 of 1,431
macros report `blocked`, every one with the move list drawn and its cursor placeable but not
accepting input, so no press moves it and the step spends its budget. That is row 30b's unplaceable
cursor inverted and it needs a WRAM reading rather than a pad change.

The decoder, the reward catalog, the adapter version, the roles and the compatibility string are
untouched.

### 12.12 The nurse's box is a ring, and `YES` and `NEXT` are one press (2026-09-22, rung 10, row 41)

Rank 10 (PEWTER CITY), the fly in the Pewter Pokémon Center, and since the v0.4.4 restart the
macro starts were `YES` **2,142**, `TALK` 107, `GO FRONTIER` 26, `BACK` 24, with the event log's
tail `YES start/done` for ever. This is **row 41**, first measured in the rung-9 trap hunt (`YES`
1,278 of 1,295 starts on one tile of map `0x3a`) and named by 12.11 as the next trap. Reproduced
from the live checkpoint with the real cartridge and surveyed press by press
(`examples/scene_probe.rs`, `FLY_PROBE_CATCH=nurse`); `infra/docs/macros-traps.md` has the survey
whole.

- **The conversation is a ring of forty-six A presses, and the box is a *choice* on one of them.**
  Welcome, "We heal your POKéMON back to perfect health!", the **YES/NO box**, "OK. We'll need your
  POKéMON.", the machine, "Your POKéMON are fighting fit!", "We hope to see you again!", the box
  closes for a single frame, and the next A press at a nurse two tiles away over her counter opens
  the whole thing again. The party read **70/70 and healthy** on every frame of it, so not one of
  those presses changed anything. That is section 12.2's rule at conversation scale: a macro whose
  precondition is already satisfied where the fly stands is a trap.
- **The brief allowed three readings and the survey settles it as none of them.** The box open at
  the checkpoint is not the prompt, it is the closing line, and `YES` there is an A press on plain
  text -- forty-five of the forty-six frames are like that. `HEAL` is not in the loop at all: its
  precondition already reads the live party, `party_needs_rest` answers `false`, and the button was
  off the pad the whole time. And `HEAL`'s own wait is a *read* of the party, not a loop waiting
  for a timer, so it cannot spin on a party that is already full. What was in the loop was the
  **dialog pad**, dealt unconditionally, and `TALK` to get back into it.
- **On a box that is a choice, `NEXT` is `YES` under another name.** An A press at a two-option menu
  confirms the option the cursor is on, and the cursor opens on YES, so `NEXT` and `YES` are one
  press with two channels -- 12.10's rule about a pair of buttons, in a dialog rather than a
  battle. `NEXT` is off any pad dealt for a readable YES/NO prompt; the pad is the box's own two
  answers.
- **At the nurse's prompt the answer that changes something is the only one bound.** Hurt or
  statused, `YES`; full and healthy, `NO`. Section 13 has read that byte for `HEAL` since the
  errands existed; this is the same byte read for the box `HEAL` opens. Knowledge inside the macro
  as a precondition, and nothing ranks the two: one of them simply is not there.
- **`TALK` is not offered at a nurse the party has no use for.** The nurse is an object with a
  purpose rather than a person to chat with, and she is the one person in Red whose conversation
  has a precondition the cartridge publishes. This is the door into the ring, and closing it is
  what makes the rest of the section a backstop rather than the fix. Nobody else is narrowed: an
  ordinary villager is `TALK`'s whatever the party reads.
- **The nurse enters the *talked* ledger on a completed heal or a declined prompt.** A completed
  `HEAL` has had the conversation with its own presses, so `TALK` has nothing left to open, and the
  reached window would otherwise expire in ten brain minutes and offer the ring again. A declined
  prompt is 12.4's rule deliberately inverted, for one person: "the thing it said no to is still on
  offer" is true of a villager with something to say and false of a service the party does not
  need -- and the pad only ever offers `NO` at her prompt when the party is already full.
- **`TALK`'s precondition and `TALK`'s ledger entry were two different questions, which is why she
  was never retired.** The precondition reaches over a counter, because
  `IsSpriteOrSignInFrontOfPlayer` does; the entry was read one tile ahead. So `TALK` was *bound* at
  the counter and *recorded* nothing at all, 107 times. A precondition and the ledger that answers
  it have to be the same reading, and they are now.
- **The general rule: an answer that brings the same prompt straight back is excluded for the
  blocked window.** Same map, same tile, same readable prompt, within one hold of the answer
  finishing -- nothing moved and nothing was settled, so the press did nothing the next press will
  not undo. It is 12.1's ledger doing for an answer what it does for a walk, with the same ten
  brain minutes, keyed `TargetKey::Answer { at, yes }`. The exclusion *narrows* a pad and never
  empties one: with both answers excluded both come back, because a box nothing can answer is a
  screen nothing can leave.
- **What the reading rests on, and what it does not claim.** `docs/design/macros-wram.md` said
  outright that there is no "a choice is open" flag, and there is not -- so `yes_no_prompt` is the
  construction `text_box`'s `waiting` already makes: `wFontLoaded` plus the figure the game draws,
  a border at (11, 6)-(19, 11) with the shared cursor parked at row 8, column 12, one item below
  the first, watching A and B. **Both halves are needed**: the cursor bytes are not cleared when
  the box closes, so all forty-six of the nurse's frames carry that geometry while the box itself
  is drawn on exactly one. Red places a two-option menu where the script asking for it says, so a
  prompt drawn somewhere else reads `false` and its dialog keeps the pad it has always had. That is
  a named limit, not a gap being papered over.

**What the harness holds.** Unit: `HEAL` is off a full party's pad, including the rung-10 party's
own numbers; `TALK` is off a rested nurse's pad and on a hurt one's; the nurse's prompt deals one
answer and a plain box still deals three; a readable prompt that is not hers deals both answers and
no `NEXT`; an answer whose prompt reopens is excluded and an answer that settled the box is not; a
completed heal and a declined prompt both write the nurse into the talked ledger. ROM-gated from
the live checkpoint: the fly leaves map `0x3a`, `YES` starts stay under five, `NEXT` is on no pad
while a prompt is readable, and `TALK` is on no pad at a rested nurse.

The decoder, the reward catalog, the adapter version, the roles and the compatibility string are
untouched.

### 12.13 `UNKNOWN` is two states, and one of them has nothing to press (2026-09-22, rung 10, five and a half hours)

Live on v0.4.5, rank 10, the fly inside the Pewter museum with rung 11's BOULDER BADGE two doors
away: since the 11:30 restart the macro starts were `GO FRONTIER` **1,235**, `BACK` **678**,
`GO OBJECTIVE` 267, `GO OUT` 242, `YES` 169, and the previous review had already named the shape --
`BACK` pressed 189 times "in a text box" on map `0x02`, surrounded by `GO OBJECTIVE` and
`GO FRONTIER`. `infra/docs/macros-traps.md` has the reproduction; three things were wrong and all
three are here, and the first of them is not a text box at all.

- **`BACK` was dealt by `Scene::Unknown`, on frames with nothing drawn.** `BACK` is on no overworld
  pad and on no dialog pad, so every one of those presses came from `Unknown` — and `Unknown` holds
  two states under one name. One is a screen this crate cannot name: the Pokédex, the trainer card,
  OPTION, where A and B are what leave it and 13.1 put them there on purpose. The other is a frame
  of the **overworld** where the buttons are not reaching the player — a warp in flight, a scripted
  push-back, the museum guide walking the fly through the door — which `scene::detect` calls
  `Unknown` because its overworld branch needs `controllable`. On the second, `NEXT` and `BACK` are
  an A and a B pressed into somebody else's script: they change nothing, they complete where the fly
  stands, and that is 12.2's trap with no box to advance. So the pad is dealt on **whether a box is
  drawn** (`wFontLoaded`, the reading `text_box` already makes), and a scripted overworld frame is an
  empty pad the fly waits out. Nothing else can wait it out: the cartridge gives the buttons back by
  itself, which is the difference between this and every other empty pad 13.1 enumerates.

### 12.14 A frontier no walk can reach is a fact about the map (2026-09-22, the same run)

The museum's ground floor is 98 walkable tiles, **62** of them reachable from the door and **39**
never stood on — and almost all of those 39 are behind the admission desk. `GO FRONTIER` aims at
ground the run has not stood on, the route search cannot reach any of it, so the macro refuses
`no route`, presses nothing and writes every goal to the blocked ledger (12.1). That ledger is a
**ten brain minute window**: it lapsed, all of it was a candidate again, and the refusal happened
again, once per hold, for hours.

A window is the right shape for a target somebody is standing in front of and the wrong shape for
ground the map has fenced off. So the refusal is remembered **per map** instead, with no window,
beside the pushed-tile ledger of row 37 — and it is *cleared* by the one event that can change the
answer: the fly standing somewhere on that map it has not stood on before, because a door opened, a
script carried it through, or somebody moved out of a doorway. Re-entering the map clears nothing;
that was the loop the window made. Session state like every other ledger, never checkpointed.

### 12.15 The ratchet's stall window cannot see a fly walking a road it has already covered

Two "Stuck" rollbacks fired on this rung inside half an hour, attempts 0 → 2, each one putting the
fly back where it had started. Both were the ratchet working exactly to contract: its stall window
is restarted by *exploration* — `progress.unique_locations`, ground the run has never stood on
(`docs/design/ladder.md`, and row 22) — and 120 brain seconds of safe overworld samples without one
new tile is a stall by definition. Entering a map for the first time already counts, because a new
map is a map's worth of tiles nobody has stood on; **re-entering** one does not, and two museum
floors and a covered town are exactly that.

What is plainly progress and is not ground: **being nearer the objective than this run has ever
been**, counted in hops over the same map graph `GO OBJECTIVE` walks (`geography::hops`). The macro
layer answers it, the sim loop passes it to the ratchet beside the coverage figure, and the ratchet
treats it exactly as it treats a rise in coverage — it restarts the window and does nothing else: no
budget spent, no snapshot taken, no trigger skipped. It can fire at most once per step of the road,
and nothing in the macro layer reads it back: no macro is ranked by it and no button is bound on it.

The checkpointed ratchet state does not move. The signal is a level on one sample, not a counter, so
there is nothing to serialise and nothing to drift across a restore.

### 12.16 What is still in the way of the badge, measured rather than fixed

With 12.13, 12.14 and the museum's two rows on the map graph, the ROM-gated run from the live
checkpoint reaches the gym's own interior on **15 macros** — against never, in five and a half live
hours. Two things it then does are worth naming, because neither is a bug and both cost the rung:

- **the town's errands come first, and they are session state.** Section 13 puts an unvisited mart
  or Pokémon Center ahead of the rung's place for every map in that area, and the gym is in Pewter's
  area like everything else. The ledger does not survive a restart, so the 11:30 restart re-armed
  both errands and `GO OBJECTIVE` aimed at them before the leader. They are paid once and the run
  goes on; the cost is minutes, not hours.
- **`GO FRONTIER` is still most of the run** — 1,247 starts in 55 brain minutes, 649 of them in
  Pewter City itself. There the frontier is genuinely reachable, one tile at a time, because the
  whole-map grid is refused on a walking fly (below) and the windowed frontier is the nearest
  unstood tile on screen. It is covering ground rather than standing still, which is why it is a
  residual and not a trap.

**The trap hunt does not improve.** Distinct tiles 193 -> 175 and flagged windows 59 -> 69, with
`BACK` in a text box 295 -> 0 and battle frames 6,948 -> 20,894. What the old cycle is replaced by
is a new one on the same five tiles -- `GO FRONTIER`, `GO HEAL`, `GO ROUTE`, x42, for seven and a
half brain minutes -- and then four brain minutes inside one battle, which the hunt's tile rule
flags as hard as it flags a stall. `infra/docs/macros-traps.md` has both arms whole and row 54 is
the next brief. The ethos check's "the trap hunt improves" does not hold for this branch; the
ROM-gated run does, and both are reported rather than one of them.

**The whole-map grid is refused while the fly is moving.** `pokemon_red::state::map_grid` checks its
decode against the screen buffer over the fly's own tile and its four neighbours, and on a frame
mid-step the two are a tile apart. Measured on Pewter City from the rung-10 checkpoint: standing
still it decodes on **118 of 120** frames, and the frame the survey caught disagreed on three tiles
by exactly one row in the direction of travel. A walk planned on such a frame is planned over the
ten-by-nine window of section 15's "before".

*Worked in 12.17*, and the guess above was the wrong way round: the survey found the coordinates
change at the **end** of the step, so it is the screen that is a tile ahead of `wYCoord` rather
than `wYCoord` ahead of the screen.

### 12.17 The coordinates change at the end of a step, and an errand arrives inside (2026-09-22, row 54)

Section 12.16's two residuals turned out to be one fact and one old rule that had been left off one
walk. Both were measured from the same rung-10 checkpoint, with
`FLY_PROBE_CATCH=step` in `examples/scene_probe.rs`; the bytes are in
`docs/design/macros-wram.md` section 9.

- **`wXCoord` and `wYCoord` change at the *end* of a step, not at its start.** Holding UP out of the
  Pewter museum, `wYCoord` read 7 for frames 0 to 15 of a sixteen-frame step and 6 from frame 16,
  while from frame 2 the screen buffer already held the view centred on (10, 6). The grid's
  cross-check compared the decode of (10, 7) with the screen's reading of (10, 6) -- `$20` against
  `$01` -- and refused, on fourteen frames of every sixteen. Pewter City decoded on **118 of 120**
  standing frames and on **none** of the moving ones, so every walk the fly actually took was
  re-planned over the ten-by-nine window: section 15's "before", and row 23's oscillation with it.
- **So the decode is read from the tile the screen is centred on.** Nothing in the pinned symbol
  table says "a step is in progress" and a new address cannot be pinned without the disassembly
  `gen_symbols.py` reads, so the anchor is *measured* rather than named: the screen is centred on
  the fly's tile or on one of its four neighbours, and the one it is centred on is the one whose
  whole neighbourhood agrees with the decode. The check keeps the property it exists for -- a wrong
  stride, a wrong quadrant, a half-loaded map or the mid-warp tear agrees with **none** of the five,
  because the whole neighbourhood has to agree under one anchor rather than each tile finding an
  anchor of its own.
- **The tile a step is landing on is ground the run has covered.** The other half of the same fact:
  for fifteen frames of every sixteen the stood ledger recorded the tile the fly had already left,
  so the ground under it stayed *unstood*, `path::frontier` kept offering it, and `GO FRONTIER` was
  dealt aiming one tile away -- a walk that reports `done` the instant the step it did not make
  lands. A step that has begun always finishes, and the screen has already centred on it.
- **An errand arrives inside the building, facing the counter, never on the doormat outside it.**
  `GO SHOP` and `GO HEAL` aim at a door, and a door's aim carries no press because the warp fires
  when it is stepped on -- so an aim on the tile the fly is already standing on settles for
  `SETTLE_FRAMES` and reports `done` with the world exactly as it was. Section 12.2's trap in its
  own words, and `exit_goals` has excluded a settled goal underfoot since row 13: this was the one
  walk that did not have the rule. A completed errand walk also writes the reached ledger, which
  `goals_toward` does not filter, so the same button came back every hold: `GO HEAL` **204** starts
  at a mean net of 0.0 tiles and a mean reach of 0.0.
- **An errand is paid by a building this run has already been inside.** `areaVisited` is session
  state, so a restore re-armed every errand in the town and walked the fly back to a counter it had
  already used -- section 13's own residual. `MacroState::map_visited` is the adapter's lifetime
  answer to the same question and it does survive a restore, so both are asked and either pays.

Nothing here changes which button the fly presses. The decoder, the reward catalog, the adapter
version and the compatibility string are untouched.

### 12.18 A menu is up while its box is on screen, not while its cursor bytes say so (2026-09-22, row 50)

The largest thing left inside a battle after 12.17: `MOVE n` reported `blocked` **890 times in
1,431 macros** from the rung-9 forest checkpoint, `MOVE 4` **222 of 224**, and 82% of a fixed run's
frames were battle time with one battle running 30,809 of them. Row 50 called it "the move list
drawn and its cursor placeable but not accepting input". Half of that turned out to be wrong, and
finding out which half is the whole fix.

- **The cursor bytes outlive the list, so the list was not drawn at all.** `MoveSelectionMenu`
  writes `wTopMenuItemY` 12 and `wTopMenuItemX` 5 and **nothing in the game clears them**. That is
  the same fact 12.12 rested the YES/NO box on — "the cursor bytes survive the box closing" — and
  the reason the *top-level* battle menu never had it is that `wTextBoxID` = `$0b` sits beside its
  geometry and is written by somebody else. `SelectMenuItem` then decrements `wCurrentMenuItem` back
  into a 0-based move slot on its way out, which lands inside the one-based range the accessor reads
  as valid, so a turn spent on move 2, 3 or 4 leaves a *placeable* cursor behind it. Every frame of
  the text, the animation, the damage and the enemy's reply read as the fly's own turn on an open
  move list. The pad dealt `MOVE 1..4` and `BACK` on all of them, the roll landed on one, and the
  cursor step pressed at a list nobody was reading until its budget ran out. That is section 12.2's
  trap wearing 12.6's clothes: a macro whose precondition is satisfied where the fly stands.
- **So a menu is up while its box is on screen.** The reading is the figure `MoveSelectionMenu`
  draws — a box at (4, 12) fourteen wide, with a horizontal run over its top-left corner and the
  `┘` junction over (10, 12) — read whole, exactly as `text_box`'s `waiting` and `yes_no_prompt`
  are. `docs/design/macros-wram.md` section 10 has the accessor.
- **It was surveyed by pressing, not by nominating a flag.** `examples/scene_probe.rs`,
  `FLY_PROBE_CATCH=accept`: on every battle frame the emulator exports its state, one directional
  pulse is issued, `wCurrentMenuItem` is read and the state goes straight back, so every frame has a
  ground truth and the run is not perturbed by the measurement. By the cursor bytes alone a press
  was honoured on **264 frames of 3,102**; by the cursor bytes and the box on **231 of 231**. Beside
  it every byte of WRAM and HRAM was asked whether it separates the two classes, so a reading was
  found rather than guessed — nothing separates once the box is in it, which is what "exact" means
  here.
- **A frame whose list is not on screen is between turns**, whose pad is the one `NEXT` that
  advances text (12.10). No pad gains or loses a button anywhere else: the top-level menu, the party
  list, the bag and the forced switch keep exactly the rows 13.1 gives them, and which move the fly
  uses is still the fly's.
- **The bag is the same trap and it is named rather than fixed.** `wListMenuID` = `ITEMLISTMENU`
  outlives the bag as surely as the cursor bytes outlive the move list: over the frames the seam
  calls an open battle bag, the same survey refused **449** presses against 36 honoured. The bag's
  list is drawn in the top half of the screen and the survey has not yet found the figure that tells
  it from the frame after it closes, so `ITEM` and `THROW BALL` still pay for it and that is
  reported. A reading this crate cannot verify does not go in (`docs/design/ladder.md`).

The decoder, the reward catalog, the adapter version, the roles and the compatibility string are
untouched.

### 12.19 A mart's counter is four screens, and one of them is the clerk talking (2026-09-22, row 55)

Minutes after v0.4.7 went live the watchdog flagged the narrowest loop yet: map `0x38`, scene
`shop`, `sequence: [BUY ANTIDOTE], period 1, repeats 747, distinctMacros 1` over ten brain
minutes, with `BUY ANTIDOTE start` then `BUY ANTIDOTE blocked` every 0.8 s and **nothing else
starting at all**. Surveyed from the live checkpoint with a new probe mode
(`FLY_PROBE_CATCH=shop` in `examples/scene_probe.rs`); the bytes are in
`docs/design/macros-wram.md` section 7.1.

- **`wListMenuID` says the counter is open, not which of its screens is up.** The mart prints its
  own text from inside `DisplayPokemartDialogue_`, which never calls the routine that clears that
  byte, so it holds `PRICEDITEMLISTMENU` for the **whole visit**. The frame the stream sat on was
  the clerk's "Here you are! Thank you!" box waiting for a press, and the seam called it the buy
  list; the cursor bytes on it belong to a two-option box (`max` 1). So which screen is up is now
  read from the figure the game draws -- the construction the dialogue box's `waiting` test and the
  YES/NO prompt already use. The clerk talking is `ShopScreen::Talking`, it reports **no listing at
  all**, and no purchase starts on it: a scene whose menu is not open offers what actually opens it,
  which here is `CONFIRM` and `LEAVE`, and both are already on the shop's pad.
- **A mart's buy list scrolls, so an item's position in the stock is its cursor index only for the
  first three entries.** Walked one pulse at a time on the live list, the cursor went `0, 1, 2` and
  then stopped moving while the window scrolled under it. The offset that names the scrolled
  position is not a pinned address, so the fourth item of a counter and after have no index this
  seam can aim at. Pewter's counter carries seven items and its ANTIDOTE is the **fourth**:
  `BUY ANTIDOTE` aimed a cursor step at index 3 in a list reporting a max of 1 and returned
  `Blocked` **on its own first frame, having pressed nothing** -- three of three attempts, zero
  frames. A macro that cannot run is not on the pad, so the four purchases are bound on the rows
  the cursor can reach, and Viridian's four-item counter is why this went unseen: its ANTIDOTE is
  index 1.
- **A purchase has no target, so a blocked purchase records nothing.** Section 12.1's ledger is
  keyed by what a walk set out for and a press sets out for nothing, so the button was dealt again
  on the very next hold, for ever -- row 6's shape in a scene with no walk in it. The fix is the
  precondition rather than a new ledger: the question the ledger would have answered is a fact about
  the counter, and the pad can ask it before the fly presses.

The loop was also **the fly's own choice landing on the one button it could afford**: the wallet
read 104, so of Pewter's stock only the Antidote was under it, and `BUY ANTIDOTE` was the only
purchase bound. Nothing about the choice changes here. What changes is that the button is not
offered, and the two presses that leave a counter are.

Nothing here changes which button the fly presses. The decoder, the reward catalog, the adapter
version and the compatibility string are untouched.

### 12.20 A two-option box is the one the cartridge drew, and a `NO` inside a conversation declines nothing (2026-09-23, row 56)

Live on the release box: map 54 (**Pewter Gym**), scene `dialog`, rank 10, **thirty-plus brain
minutes of zero progress** with the explore and wild-win counters frozen, **747 macro starts in ten
brain minutes**, and a mix of `YES` 64 / `NO` 62 / `NEXT` 59 / `TALK` 6 with **no walk macro dealt
at all**. The watchdog did not flag it: four distinct macros is exactly its threshold. Surveyed
from the live checkpoint with a new probe mode (`examples/scene_probe.rs`,
`FLY_PROBE_CATCH=dialog`), which walks the conversation one raw pulse at a time and prints, per
frame, what the seam makes of it beside **every complete `TextBoxBorder` the cartridge actually
drew**. `infra/docs/macros-traps.md` has the survey whole.

The shape is **row 41's ring one town over**: the gym guide's conversation is fifty-two presses --
"Hiya! I can tell you have what it takes to become a POKeMON champ!", "Let me take you to the
top!" with a YES/NO box, the type-matchup tutorial, "matches could be made easier!" -- the box
closes for a frame, and the next A press at the guide two tiles away opens the whole thing again.
Nothing in it changes the world. Two things kept the fly walking it, and both are readings rather
than pads.

- **The box was drawn where this crate was not looking.** 12.12 read the border at
  (11, 6)-(19, 11), because that is where a Pokemon Center's script puts it, and said so in its own
  residual: "Red places a two-option menu where the script asking for it says, so a prompt drawn
  elsewhere reads `false` and its dialog keeps the pad it has always had". The guide's box is at
  **(14, 7)-(19, 11)** with the cursor at column 15. Over 260 surveyed presses the box was drawn on
  **10 frames** and `yes_no_prompt` answered `false` on **all 260** -- so the pad was
  `NEXT, YES, NO` on a frame that was a *choice*, which is 12.10's forbidden pair (an A press at a
  two-option menu confirms the option the cursor is on, and that is what `YES` is), and the
  reopened-prompt exclusion of 12.12 never armed, because it only judges an answer to a prompt this
  crate can read. **The whole of 12.12 was inert in that gym.**
- **So the figure is found rather than pinned.** One fact about `DisplayTwoOptionMenu` rather than
  about any one script: the cursor goes in the box's **first interior column**, so the border's
  left edge is one column to the left of `wTopMenuItemX` -- true of both boxes surveyed. The *top*
  obeys no such rule, because the nurse's box begins two rows above the first item and the guide's
  one, so the top is found by looking up for the border's own corner and the figure is then read
  **whole**, exactly as `waiting` and the move list are. `docs/design/macros-wram.md` section 11
  has the accessor. Measured over both checkpoints, 400 frames: the reading is true on the 14
  frames a two-option box is drawn and false on the other 386, and `wTextBoxID` = `TWO_OPTION_MENU`
  agrees with it exactly -- which is recorded as a third reading and **not** put in the accessor,
  because no survey here covers Red's other two-option menus.
- **And a `NO` pressed inside a conversation declines nothing.** 12.4's rule -- "the fly said no, so
  whatever it said no to is still on offer" -- took the pending `TALK` off the moment any `NO`
  finished. A `NO`'s B press advances a plain text box exactly as `NEXT`'s A does, about a third of
  the fifty-two presses that walk the guide's ring are `NO`, so the talked ledger **never learned
  the conversation had happened**: `TALK` was on the overworld pad every hold and was the ring's
  own door. In the twenty-brain-minute reproduction `TALK` started **25** times on that one map.
- **Which of the two a `NO` was is decided where it can be seen: by whether the box closes on it.**
  The decision moves to the frame the text goes away, which is where the talked entry is written
  anyway, and the reading is the answer still standing there -- `pending_answer`, armed only by an
  answer to a prompt this crate can read and alive for one hold (12.12). A declining `NO` still
  standing when the box closes is a `NO` the box closed *on*, and the offer stands; anything else
  is a conversation walked through to its end, and the person is retired. The nurse's own declined
  heal is unchanged: it writes her into the ledger by 12.12's named inversion, one person wide.
- **What the pad does instead, which is the point.** With the guide retired, the gym's overworld pad
  is `GO OBJECTIVE`, `GO OUT`, `GO FRONTIER`, `GO HEAL` -- the walks -- and the fly is out of the
  room in 0.37 brain minutes against never in twenty. Nothing new is on any pad and nothing is
  ranked: `TALK` goes off a person this run has already had the conversation with, which is the
  ledger 9.2 added doing exactly what it was added for, and `NEXT` goes off a readable prompt,
  which is 12.10.

**What the harness holds.** Unit: a two-option box reads as a prompt at both surveyed geometries
and at neither without its border, its font flag or its two-option cursor; a box the cursor is not
parked in is not the cursor's box; the pads the two frames are dealt (`YES, NO` against
`NEXT, YES, NO`); a declined offer leaves the thing on offer; and a `NO` deeper inside a
conversation leaves the conversation counted and `TALK` off the pad. ROM-gated from the live
checkpoint: the fly leaves map 54, `NEXT` is on no pad while a readable prompt is open, and **no
readable prompt is answered more than four times for one person in a session**, against 439
answers at one person in the twenty-minute reproduction.

The decoder, the reward catalog, the adapter version, the roles and the compatibility string are
untouched.

### 12.21 A button refused from here is not dealt again from here, a last resort walks where it can, and an escort walls the tile it fired on (2026-09-23, row 57)

Live on v0.5.3, rank 10, map 2 (**Pewter City**), scene `overworld`: the pad was **`GO ROUTE` and
nothing else**, and it was refused about **740 times per ten brain minutes for more than two
hours**, with one `GO ROUTE start` / `blocked` pair every ten brain minutes, no button pressed and
the exploration count frozen. The watchdog read one start and one name, and never flagged it. The
`refused` event's `value 3.0` is the slot, not a reason (row 55); the feed carries no reason.

The ledgers that dealt that pad are session state and a restore starts them empty, so the trap
does not come back from the checkpoint by itself: a twenty-brain-minute hunt with the real brain
from the live frame covers 356 tiles. It was reproduced by **earning** them -- a new probe mode
(`examples/scene_probe.rs`, `FLY_PROBE_CATCH=route`) drives the real palette from the checkpoint and
can seed each ledger -- and three facts came out, each measured on the cartridge.

- **The pushed ledger walled the tile a walk set out from, not the tile the script fired on.**
  Pewter City's youngster takes the joypad on four tiles by the road east
  (`PewterCityPlayerLeavingEastCoords`) and walks the fly to the gym until Brock is beaten. `GO
  ROUTE` aims east every time, because Route 3 is the one connection the run has not crossed, so it
  is escorted every time -- and row 37's ledger, which has **no window**, recorded the macro's
  starting tile. Measured from the ratchet's rollback snapshot: one `GO ROUTE` from the town's
  south entrance walked 26 tiles to (37, 18), was escorted, and walled **(18, 35)**, the south
  entrance. Row 37's rule is right for a press (the fly is standing on the tile the script fires
  on) and wrong for a walk. Walks start wherever the last one ended, so the walls accumulate
  until the fly stands in a pocket no route leaves.
- **In the pocket every walk refuses `no route`**, and each refusal is recorded where it belongs:
  `GO FRONTIER`'s marks the map exhausted (12.14, no window, cleared only by new ground, and there
  was none for hours), and `GO OBJECTIVE`'s only goal, the gym's door, goes to the blocked ledger.
  With every person and sign already talked to and the errands paid, nothing else is left.
- **The last resort is the one list that ignores the blocked ledger**, by design ("a target the
  ledger is resting is still the only place to go", 13.1). So `ways` dealt `GO ROUTE` at the gym's
  door, the route search refused it, the refusal wrote the door to a ledger the dealer does not
  read, and the button was dealt again on the next hold. The same refusal **re-stamped the door's
  window every hold**, which is why `GO OBJECTIVE` never came back either. Once per window the
  road east lapsed, `GO ROUTE` walked it, and it was excluded again.

Seeded with that pocket -- three pushed tiles sealing the strip by the road from the town, the
frontier mark, everything talked to, the road east resting -- the cartridge deals exactly the live
pad: `GO ROUTE`, refused `no route` **746 holds running** on one tile over ten brain minutes.

**The fix, all three parts inside the macros:**

- **A walk walls the tile it last stood the fly on**, which is where the cartridge took over; every
  other macro keeps the tile it started on. The same walk now walls (37, 18), one of the four
  tiles the youngster fires on.
- **A refusal that taught the blocked ledger nothing is recorded with the tile the fly stood on,
  and the dealer does not deal that button from that tile for the blocked window.** The dealer
  asks the cheap question and `start` the real one, and the blocked ledger closes that gap for
  every list but a last resort; this closes it for all of them without a route search in the
  dealer, which runs every frame. Only a last resort deals goals the ledger is already resting, so
  a `no route` whose every goal was resting already -- and any `precondition` refusal, which writes
  nothing -- is the one remembered; a refusal that writes a new exclusion changes the next deal
  by itself. (Measured: holding *every* refusal against the tile kept the real brain, which
  pressed `GO ROUTE` first while the door was still the second tier's answer, from ever reaching
  the last resort below.) It is a fact about *here*: the button is dealt again the moment the fly stands
  on any other tile, or when the window closes. A pad with nothing left that can run is empty and
  the fly waits, which is section 13.1's honest answer.
- **A last resort that cannot reach the objective's door takes a way out it can reach.** The
  narrowing to "the ways toward the objective" is a preference the dealer cannot check, and in the
  pocket it chose the gym's door beyond the fence while the road east was three tiles away,
  resting in its window (which a last resort ignores). When `start`'s route search cannot reach
  the preferred ways it tries the rest of the last resort, nearest reachable first, as every walk
  chooses. The pad does not change; only where the pressed macro walks.

In the rebuilt pocket the fly now leaves on **frame 517** (0.14 brain minutes) -- walked east, met
by the youngster, carried to the gym -- against frame 36,325 on the base, when the road's window
lapsed; `GO ROUTE` is refused **once** there against 746 holds running.

Nothing presses for the fly and nothing is ranked: one button leaves a pad it could not run from,
one wall moves to the tile that earned it, and one walk goes where it can. The decoder, the reward catalog, the adapter
version, the roles and the compatibility string are untouched.

### 12.22 The rung's people are in the room when the screen does not show them (2026-09-23, row 58)

Live on v0.5.3, rank 10, for twenty-five minutes: `GO OBJECTIVE` into the Pewter Gym, `GO OUT`
straight back out, with `GO ITEM`, `GO FRONTIER`, `YES` and `NO` mixed in. Per ten brain minutes
about 93 `GO OUT`, 47 `GO OBJECTIVE`, 200 starts in all, every one `done`, **no reward event of any
kind**, the exploration count frozen at 1,892. Check 10 saw ten distinct names and said nothing.
Surveyed from the live checkpoint with the route probe (`FLY_PROBE_CATCH=route`,
`FLY_PROBE_CATCH_MAP=54`), which reads the room on the fly's Nth arrival.

- **The objective saw the room through the screen.** `objective_targets` read `npcs`, which is
  what the cartridge *draws*, and `CheckSpriteAvailability` writes `$ff` into the image index of
  every sprite outside a window of the player's coordinate. From the doormat at (4, 13) that window
  holds the guide at (7, 10) and nobody else: BROCK at (4, 1) and the Jr. Trainer at (3, 6) are not
  drawn. With the guide talked to (12.20), the rung's list was empty, so `GO OBJECTIVE` had nothing
  to aim at inside and 12.5's rule -- the ways out are withheld while the rung's person is in the
  room -- let `GO OUT` onto the pad. Outside, `GO OBJECTIVE` aimed at the gym's door. The pair
  undoes itself in about a second, and nothing on either side of the door earns anything.
- **So the rung reads the people the cartridge hides only for being off the screen.** The window
  is a function of `wYCoord`, `wXCoord` and the sprite's own biased coordinates, all already read,
  so a sprite whose `$ff` falls outside it is one the cartridge would hide for that reason whatever
  else were true, and a sprite the cartridge is not updating does not move
  (`state::offscreen_npcs`). A `$ff` *inside* the window, or on a scripted mover, is not the
  screen's and is not reported. Only the rung reads the list: a sprite outside the window may also
  be a toggleable object switched off, which reads the same, so `GO NPC`, `TALK` and objects keep
  what is drawn.
- **Facing any of the rung's people is the arrival.** 12.5 left out only the one ahead, which was
  enough for one target; a gym names three, and in front of BROCK `GO OBJECTIVE` still had the
  trainer to walk to. A fly facing a person the rung is waiting on has nothing left for a walk to
  do, and `TALK` is the press.

Three frames the seam read as the fly's own were the cartridge's, and each wrote a ledger entry that
emptied the room again once the first fix let the fly into it:

- **A warp's tear.** `wCurMap` changes thirty-two frames before the header, the coordinates and the
  warp table follow it, while the screen fades, and no joypad bit is set until the fade is over.
  The seam read "map 54 at (16, 17)" -- Pewter City's doormat under the gym's id -- as an overworld
  and dealt it a pad; a walk started there planned over the wrong map, and what it aimed at went
  into the blocked ledger under the gym's id (live: `GO OUT` started and finished in 0.05 s). The
  driver now reads a tear as the map byte having changed while the fly still stands on a warp of
  the loaded table that leads to the map the byte names, deals it as `Unknown` with an empty pad,
  and records no ground from it. Teleport pads -- Saffron Gym, two Silph Co. floors -- do not change
  the map byte, so they are never a tear; a tear is bounded at ninety frames all the same.
- **A battle's transition.** Between a trainer's challenge closing and the battle screen there are
  219 frames with every joypad and script bit clear. The pad was dealt, a walk toward BROCK pressed
  into the animation and gave up after three refused steps -- BROCK blocked for ten brain minutes --
  and the Jr. Trainer's conversation read as over, so the trainer the fly then lost to was
  "talked to" for the session. `wCurOpponent` is set when a battle is decided and cleared by
  `EndOfBattle` with `wIsInBattle`; it is not in the generated table and is derived as the byte
  between two that are, both neighbours checked in a test (`macros-wram.md` section 12), and
  `controllable` reads it.
- **A trainer walking up.** 12.4 reads a macro the cartridge ended by taking the joypad as a
  refusal and wrote the target blocked and the tile pushed at once. A trainer who sees the fly
  takes the joypad the same way. The entries now wait until the cartridge gives the joypad back:
  in the overworld it was a refusal and is written as before; a battle teaches the ledgers nothing.

Nothing is ranked, nothing presses for the fly, and no button is added to any pad: `GO OBJECTIVE`
has a person to walk to where it had none, `GO OUT` is withheld by 12.5's own rule, and three
frames that were never the fly's deal nothing. The decoder, the reward catalog, the adapter
version, the roles and the compatibility string are untouched.

### 12.23 A move the cartridge answers with nothing is not dealt beside one it does not (2026-09-23, row 60)

Live on v0.5.5, early game after the reset to milestone 1: Route 1, Squirtle L5 (TACKLE, TAIL
WHIP) against a wild Pidgey, "Nothing happened!" on the screen. Since the reset `MOVE 2` 183
times and `MOVE 1` once; the last reward 25 brain minutes before the checkpoint, one wild win in
the whole run. Check 10 flagged `unrewarded` (1,600 decisions, no reward event, no new ground on
two probes), which is right, and it is unchanged.

- **The pad was at fault, not only the choice.** `MOVE n`'s precondition was "the slot holds a
  move with PP", so TAIL WHIP stayed on the pad after it had walked the Pidgey's DEFENSE to the
  point the cartridge refuses it. `StatModifierDownEffect` answers "Nothing happened!" when the
  stage is already -6 **or the stat itself is already 1**, restoring the stage. The checkpoint is
  the frame Squirtle fainted to a Pidgey L3 at DEFENSE -6 (stat 2); in the next battle the stat
  reached 1 at -5. From there the pad dealt `MOVE 1, MOVE 2, RUN` and the fly pressed `MOVE 2`
  until Squirtle fainted, woke at home and walked back: every battle lost, one wild win in the
  run. The readout's favourite being `MOVE 2` is the fly's; a button that can do nothing at all
  being on the pad is a macro that knows nothing about its own effect, section 12.2's trap.
- **What a move does is the cartridge's, read the same way for every move.** `state::move_data`
  reads the move's row of `Moves` from the cartridge image (`$0E:$4000`, each row checked against
  its own id) and `state::move_without_effect` answers the refusals the effect routines make on
  bytes already in WRAM: a stat stage at its limit or a stat at 1 or 999, Mist or a substitute in
  front of a stat-lowering move, a sleep, poison or paralysis move against a target that already has
  a status, is Poison type, or is Ground type to an Electric move. No move is named; a miss is a
  roll and is not answered. `macros-wram.md` section 13 has the bytes.
- **It is PP's rule.** A move the cartridge answers with nothing is not dealt beside one it does
  not, exactly as a spent move is not (12.6, 12.8), and `MOVE 1` over the menu stops being FIGHT's
  backstop only in that case. When no move would do anything the moves stay as PP deals them:
  taking the last ones away would leave an open list whose only button is `BACK`, 12.11's pair,
  and a turn that ends on "Nothing happened!" still ends. RUN, ITEM and SWITCH are untouched.

Nothing is ranked, weighted or pressed for the fly: a button leaves the pad while it cannot change
anything and comes back when it can (a new battle resets the stages). The decoder, the reward
catalog, the adapter version, the roles and the compatibility string are untouched.

### 12.24 The map graph is the disassembly's, piece by piece (2026-09-23, row 59)

Opened pre-emptively: the row-58 review carried the route survey past the Boulder Badge and the
fly walked Pewter City (39, 17) to Route 3 (0, 9) and back from about frame 68,000, `GO OBJECTIVE`
done on Route 3 538 times and `GO ROUTE` done on Pewter 537. Reproduced from a rank-11 checkpoint
the survey writes (`FLY_PROBE_SAVE_RANK=11`): 509 and 508, never on Route 4. The live fly got
through Route 3 anyway and met the next half on v0.6.0 at 22:20 UTC: rank 12, MT. MOON, the
objective Cerulean, on Route 4 per ten minutes `GO ROUTE` 215, `GO OBJECTIVE` 113, `GO OUT` 103,
two new tiles, in and out of the Pokécenter and the cave mouth. Route 4 was one node with Cerulean
off its east edge, which the mountain cuts off from the cave mouth's side. From the live
checkpoint the route survey on `main` walks it 930 times in 72,000 frames.

**The geography table disagreed with the headers.** Checked row by row against
`data/maps/headers/*.asm` and `data/maps/objects/*.asm` at the pinned commit:

- Route 4 is **north** of Route 3, not east (`Route3.asm`: `connection north, Route4`); Route 3's
  top edge is the road to Mt. Moon's Pokécenter. Mt. Moon's doors are both **on Route 4**:
  (18, 5) into the first floor and (24, 5) into B1F, whose (27, 3) is the way out. Route 3 has no
  warps. So Route 3's north edge named no map, and nothing on Route 3 was the way to the rung.
- Route 14 / 15 and Route 24 / 25 had the right neighbour in the wrong column (west and east,
  not south and north). Route 24's east edge is Nugget Bridge's far end, rung 16's road.

**Four maps are pieces the player cannot walk between** (12.7's rule, measured by flooding every
tile of every map on the graph from the blocks, blockset, collision list, tile-pair walls and
ledges): Route 2 as before; **Route 4**, cut by the mountain into the cave mouth's side and
Cerulean's side; **Mt. Moon B1F**, four chambers of two ladders each; **B2F**, one large piece and
two small ones. The one road through is 1F (5, 5), B1F (21, 17), B2F (5, 7), B1F (27, 3); the
other two ladders on 1F lead to dead ends.

A split row is now any number of pieces, each with its doors (the warp index and tile) and what is
one step from it, a whole map or another map's piece. Three rules keep it honest:

- **Which piece the fly is in** is what its walk can reach on the decoded grid (section 15): the
  piece whose doors it reaches, when exactly one piece's are. The grid has no ledges, so where it
  reaches none (Route 4 below the ledges) the nearest door answers. Flooded over every tile of
  the four maps' ground in the disassembly, the rule names the right piece for all of them.
- **Which piece a door lands in** is the cartridge's own answer: a warp names the destination
  warp it arrives at (`wWarpEntries` byte 2), and each piece lists its warps. An edge lands in
  the piece that lists the map it is stepped off.
- **The hop is a piece**, and an exit is toward the objective only if it lands in that piece. On
  1F three ladders go down to B1F and one of them is the road.

**A connection nobody can walk across is not a road.** Four header connections have no tile where
both sides are land: Pallet Town / Route 21, Cinnabar / Route 20, Route 20 / 19 (sea) and Route 22 /
23 (the League's fence). They keep their name and offer no exit and no hop. Without this the road
from Pallet Town to Cerulean was by sea, and a fly that whited out in Mt. Moon, which the survey's
did, walked into Pallet's shore every two seconds.

**One seam frame, found on the way.** Route 3's first trainer closes his challenge onto five frames
of plain overworld before `StartTrainerBattle` decides the battle (`home/trainers.asm`: it runs
after `DisplayTextID`'s close-down). Row 58's pending push-back was decided on the first of them
and walled (11, 6), the one gap between Route 3's west end and the rest of the road, for the
session. A push-back is now a refusal only once the overworld has been the fly's for thirty frames
running; a battle inside them drops it.

From the badge, the route survey reaches Route 4 at frame 70,356 and Mt. Moon at 70,707 (rung 12),
no pushed tile; after whiting out in the cave it walks the land road back from Pallet Town. The
ROM test on the stub rotation reaches Mt. Moon in 56.7 brain minutes with 4 Pewter / Route 3
crossings; the base makes 3,391 in 80.4 and never stands on Route 4. From the live 2026-09-22
rank-11 checkpoint the branch reaches Mt. Moon too, where the base ends fenced on Route 3. From
the live Route 4 checkpoint the branch is in the cave on frame 279 and stays on the road; on the
stub rotation Route 4's west doors are crossed 13 times in twenty brain minutes (whiteouts and
walks back included) against 56 on `main`.

Nothing is ranked and nothing presses for the fly: a table of maps says what the headers say, a
split map has the pieces its ground has, and a frame that was the cartridge's is not read as the
fly's. The decoder, the reward catalog, the adapter version, the roles and the compatibility
string are untouched.

### 12.25 A trainer's challenge is the cartridge's until its battle is over (2026-09-23, row 61)

Live on v0.5.5, rung 9, for twenty minutes: `GO OBJECTIVE` into Viridian Forest's south gate (map
50, `$32`), `GO OUT` straight back onto Route 2, `GO WARP` back from the forest, `GO OBJECTIVE
blocked` in the forest, no reward. Reproduced with the route survey from the live checkpoint
(uniform choice per hold, xorshift seed 7), which walks the same ring for twenty brain minutes.

- **The ring's cause was a wall, not the gate.** The forest's only road to its north gate is a
  two-wide corridor at x = 1-2; a Bug Catcher stands on (2, 18) facing west. A walk up the
  corridor steps onto (1, 18), the trainer takes the joypad, and row 58's held push-back waits for
  the joypad to come back. `DisplayEnemyTrainerTextAndStartBattle` clears `wJoyIgnore` before the
  challenge text and `StartTrainerBattle` writes `wCurOpponent` only after that text's close-down:
  **five frames** with no box, no script bit and `wCurOpponent` zero, which the seam read as the
  fly's overworld. The push was written there; (1, 18) went into the pushed ledger, which has no
  window, and from then on every walk to the north gate had no road. `GO OBJECTIVE` walked to the
  nearest reachable tile, a dead end at (6, 1), and was blocked; `GO WARP`'s last tier took the
  south gate, whose `GO OUT` is Route 2, whose `GO OBJECTIVE` is the gate.
- **The fact is `wStatusFlags7` bit 3, `BIT_TRAINER_BATTLE`**: set by `CheckFightingMapTrainers`
  on the "!", cleared at `.battleOccurred` after every battle (before the blackout check). An
  overworld frame with it set is `Unknown` in the macros' own scene, with no text box, so the pad
  is empty, no ground is recorded and no held entry is decided on it. A trainer talked to by the
  fly never sets it; its `wCurOpponent` is written inside the text.
- **The macros' reading only.** `controllable` and `scene::detect` are shared with the reward
  adapter and the feed and do not change; `PokeState`'s `scene` and `scripted` read the bit
  beside them.
- **The gates were modelled right.** Both forest gates are on the graph and `next_hop` answers
  the forest from the south gate and Route 2 from the north one. The south gate's `GO OUT` is the
  "a room has to be leavable" tier, a way back that is the fly's choice: with the corridor open,
  the survey seed that walked into the gate twelve times still earned the badge, on both arms.

Nothing is ranked or pressed for the fly: five frames that were never the fly's deal nothing. The
decoder, the reward catalog, the adapter version, the roles and the compatibility string are
untouched.

## 13. Shops and Pokémon Centers (the operator, 2026-09-17: "refactor the shop macros. make it a
## priority to visit the shop at least once per area; make shop macros item purchases. same
## for the Pokécenter. heal should be a macro.")

Knowledge inside macros, never in the choice; the pad still lists buttons and the fly picks.

- **Errands.** `GO OBJECTIVE` gains an errand list ahead of the rung's place: on a map whose
  area (the town the map belongs to, per geography) has a mart or a Pokémon Center this run has
  not yet entered, the objective target is that building's door first, then the rung's place.
  One visit per area per run (session ledger `areaVisited(kind, area)`), so the errand is
  paid once and never loops. Money below the cheapest purchase skips the mart errand.
- **`GO SHOP` / `GO HEAL`** are the errands as buttons of their own too (populations
  `macro_go_shop`, `macro_go_heal`, from the same MBON/motor pool by the same rule): on the pad
  when the area's mart / center is unvisited (GO SHOP also needs money), walk to its door and
  in; inside, walk to the counter / nurse and face them.
- **Shop scene.** Buttons are purchases: `BUY POTION`, `BUY BALL`, `BUY ANTIDOTE`, `BUY REPEL`,
  each bound only when the mart's stock (read from WRAM, `shop_stock` accessor, verified by
  the survey method) lists it and money allows at least one; each macro buys ONE unit through
  the real menus by reading the cursor; `LEAVE` closes the menu and walks out. A purchase
  marks nothing visited; the errand was paid on entering.
- **Center scene.** `HEAL` is a macro: walk to the counter, face the nurse, talk, answer YES,
  wait for the heal animation to end (read the party HP back to full), close the box. On the
  pad only when at least one party member is not at full HP or has a status — verified against the
  live party on the cartridge (**12.12**), which is also what takes `TALK` at the nurse off the pad
  and what picks the one answer her YES/NO box is dealt. `LEAVE` walks out.
- **Ladder.** Unchanged; no reward for shopping or healing (rewards are the adapter's,
  untouched).
- **Screen.** Two new channel tags; the cells and the MACROS rate row take them as they come.
- **Proof.** ROM-gated from the Viridian checkpoint: `GO SHOP` enters the Viridian mart and
  `BUY POTION` buys one; `GO HEAL` enters the center and `HEAL` restores the party; the errand is
  not offered again in Viridian; trap hunt before/after.

## 14. The full pad and four move buttons (the operator, 2026-09-17: "scoot fly over. make it 2
## columns" … "four, one per move slot" … "2 cols.")

- **Screen.** The strip under the game shows EVERY macro type at once as a two-column grid
  (rows as needed, 14 with the types below), the fly shrunk and scooted further left to make
  room. A cell is lit when the button is on the pad, dim when it is not, bright while its
  macro runs, with the outcome beat as before. Fixed order by type so a cell never moves.
- **Battle.** `ATTACK` is replaced by four buttons `MOVE 1` .. `MOVE 4`, one per move slot,
  each with its own population (`macro_move_1..4`), bound only when that slot holds a move
  with PP; the fly picks the move. No "best move" knowledge remains. `SWITCH`, `ITEM`, `RUN`
  (section 13.1 precondition) unchanged. Over an open move list the four buttons select their
  slot and confirm; with every move at 0 PP, `MOVE 1` confirms the cursor so Struggle happens.
- **Types (27):** GO OBJECTIVE, GO OUT, GO WARP, GO ROUTE, GO ITEM, GO NPC, GO FRONTIER,
  GO SHOP, GO HEAL, TALK, MENU, NEXT, YES, NO, CLOSE, CONFIRM, BACK, MOVE 1, MOVE 2, MOVE 3,
  MOVE 4, SWITCH, ITEM, RUN, BUY POTION, BUY BALL, HEAL, LEAVE (BUY ANTIDOTE / BUY REPEL fold
  into the shop scene's BUY buttons only if the grid has room; otherwise they stay as
  purchases reachable through BUY POTION's script? No: keep them as types; the grid takes
  rows as needed).

**Section 14 layout, decided 2026-09-17 (the operator: "your rec is good").** The full
two-column grid under the game does not fit at the 24 px text floor (31 types need about 410 px;
the strip has 172), so: the strip under the game shows every button ON THE PAD NOW, up to 14 as
two columns of seven at the floor, tag and short name, the running one bright; a new rail tab
MACROS shows the whole keyboard, three columns of eleven, bound cells lit and unbound dim, the
running one bright. The game stays at 5x; the afterglow row stays. The population re-deal that
came with the nine new types is accepted.

### 13.1 The full complement, scene by scene (the operator, 2026-09-17: "check to make sure each
### scene has the full complement of options. we run away a lot." … "sometimes the macro buttons
### disappear and everything just hangs there.")

Every scene and sub-state audited against section 12's table and against the game's real options
there. "Read from WRAM" is the standing rule for a new button: a precondition this crate cannot
observe is not a precondition, it is a guess.

| scene / sub-state | pad | change |
| --- | --- | --- |
| Overworld, outdoors | GO OBJECTIVE, GO ROUTE, GO SHOP, GO HEAL, GO ITEM, GO NPC, GO FRONTIER, TALK | MENU was added by row 18 and **taken back off by 12.11**: it opens a screen whose own `BACK` closes it again, and nothing in the vocabulary uses the start menu. Errands added |
| Overworld, indoors | GO OBJECTIVE, GO OUT, GO WARP, GO SHOP, GO HEAL, GO ITEM, GO NPC, GO FRONTIER, TALK | **MENU off, by 12.11**, as above; `GO NPC` was off section 3's fixed indoor row and on the plan's, and with one dealer it is on both |
| Overworld, inside a Pokémon Center | the indoor pad plus HEAL | new. A centre is a sub-state of the overworld, not a `Scene`: pokered has no "a Pokémon Center is open" byte, so the only honest observable is the map id, and a new `Scene` would be a new `game.scene` on the wire |
| Overworld, inside a mart | the indoor pad | the counter is a `Shop`; the mart's *floor* is an ordinary interior, with the one exception below |
| Overworld, inside a mart or a centre, counter unfaced | GO SHOP or GO HEAL, TALK when facing the counter | new, and it is the one place a pad is deliberately *narrow*. The errand is paid on entering and never offered again, so a walk that leaves the building spends the one visit the area gets — measured: the fly reached the mart in 1.7 brain minutes and `GO OBJECTIVE` walked it straight back out over the doormat. While the counter is unfaced nothing on the pad leaves (row 34b) |
| Overworld, inside a mart or a centre, counter faced | the indoor pad, plus HEAL in a centre | the suppression is released by facing the counter, by talking to it, or by a walk to it failing. Since **12.12** `TALK` is not on it at a *nurse* the party has no use for: her conversation is a service whose need the cartridge publishes, and a ring of text that ends where it began is section 12.2's trap |
| Dialog, a plain text box | NEXT, YES, NO | A and B both advance a plain box, so all three are dealt for one — what it buys is the fly being able to answer *no*. Forty-five of the nurse's forty-six frames are this row (**12.12**) |
| Dialog, a readable YES/NO box (**the box the cartridge drew**, 12.20) | YES, NO — or **one of them** at a Pokémon Center's nurse | **new, 12.12.** `NEXT` is off it: an A press at a two-option menu confirms the option the cursor is on, which is what `YES` is, so the two are one press under two names (12.10). At the nurse's own prompt the bound answer is the one that changes something — `YES` with a hurt or statused party, `NO` with a full one. An answer whose prompt comes straight back is excluded for the blocked window, and the exclusion never empties the pad |
| Menu (the start menu) | CLOSE, CONFIRM, BACK | unchanged as a *scene*, and since **12.11** nothing on any other pad opens it: the fly reaches it with the **raw** START button, which still reaches the cartridge in macros mode, and moves its cursor with the raw D-pad. A SAVE or a POKéDEX button would be a macro per start-menu entry and is not asked for -- which is precisely why `MENU` had nothing behind it |
| Menu (the bag, an elevator, the party list outside a battle) | CLOSE, CONFIRM, BACK | unchanged |
| Unknown (the Pokédex, the trainer card, OPTION, a naming screen, a mid-warp frame) | NEXT, **BACK** | **BACK added** (row 9): B is what leaves the first three, and A leaves none of them |
| Battle, own turn, main menu | MOVE 1..4, SWITCH, ITEM, THROW BALL, RUN (whose cursor indices are FIGHT 0, **ITEM 1, PKMN 2**, RUN 3 -- two columns, 12.11) | four move buttons for `ATTACK` (section 14); THROW BALL added, and gated on the species since 12.9; **RUN gated**, below. No `BACK`: the four entries are the answers to this menu. **`NEXT` removed by 12.10** — an A press here confirms FIGHT and reopens the list the move list's `BACK` just closed, and `MOVE 1` is the backstop instead, bound here whatever the battler reads as |
| Battle, own turn, move list (**the box on screen**, 12.18) | MOVE 1..4, BACK -- or **MOVE 1 alone** | as above, plus **12.11**: `BACK` is dealt here only while `wBattleMon*` reads, because a list that binds no `MOVE n` has a pad whose one button closes the list `MOVE 1` underneath had just opened. With nothing readable the pad is `MOVE 1` and its script confirms where the cursor stands |
| Battle, own turn, party list | SWITCH, BACK | unchanged |
| Battle, own turn, the bag | ITEM, THROW BALL, **BACK** | the bag reports a *cursor* (`macros-wram.md` 7.1), and since **12.10** it is the own turn, because a cursor accepting input is one. Its pad is the list's own answers; `NEXT` and `CONFIRM` are both off it, being the same blind A press that *uses* whatever the cursor holds |
| Battle, forced switch | SWITCH, NEXT | unchanged (row 8). The one arm that keeps `NEXT` with a cursor up, because it cannot be cancelled and has no `BACK` to undo it |
| Battle, between turns | NEXT | since **12.18** this row is most of a battle, and correctly so: a frame whose move list is remembered rather than drawn lands here. `BACK` was added here for the bag and **taken back out by 12.9**: on a frame of battle text there is no list to leave, and a `BACK` that changes nothing is the trap of section 12.2. Since **12.10** the bag is not on this row at all, so `NEXT` here is only ever the A that advances text |
| Shop | BUY POTION, BUY BALL, BUY ANTIDOTE, BUY REPEL, CONFIRM, LEAVE | two purchases to four; CONFIRM added |
| PC | **CONFIRM**, LEAVE | **CONFIRM added**: a list the fly opened is one it can answer rather than only close. Depositing and withdrawing are still not in the vocabulary (row 17) |
| Title | nothing | unchanged, by contract: the readout's boot variant applies |

**`RUN` is bound only in a wild battle the fly is losing.** Two readings, both from the
cartridge's own numbers: the Pokémon that is out is under a third of its HP with no healthier
reserve to send in (the same reserve `SWITCH` chooses by, so the two buttons cannot disagree about
whether one exists), or every move it has is out of PP with nothing to switch to, where Struggle
is a countdown. Anything else and the button is off the pad. Knowledge inside the macro, as a
precondition: nothing ranks `RUN` below `MOVE 1`, it simply is not there while the fight is worth
having — and a fled battle pays nothing and teaches nothing.

**`LEAVE` in a shop closes the menu and does not walk out**, and that is a deliberate limit rather
than the section's words. A scene-spanning walk would have to be planned while the screen buffer
holds the mart's menu, so every tile of it reads `Walkable::Unknown` and the route is a guess over
ground the search cannot see. `GO OUT` is on the mart's floor pad one press later and is the same
walk with the map on screen. Stated here so nobody has to rediscover it.

**Sections 3 and 12 had two dealers.** `Palette::for_scene` was section 3's fixed six-channel table
and `plan::plan_for` was section 9's ordering, and macros mode asked for the second because it was
the one that knew `GO OBJECTIVE`'s rung. They could disagree about which buttons a scene had, and
did: `GO OBJECTIVE` was on the plan's overworld and on no fixed row for two releases. Section 14
removes the last reason they differ — with a slot per type there is nothing to order and nothing to
truncate — so `palette::scene_set` is the table above and `plan_for` *is* `for_scene`.

#### Every way the pad can be empty

An empty pad in a playable scene is the one state the doctrine cannot recover from on its own:
nothing presses for the fly, so a scene with no button waits, and waiting is *correct behaviour
that looks exactly like a hang*. So it is enumerated, closed where a macro can close it, and
measured where it cannot.

| cause | closed by |
| --- | --- |
| a dialog whose one answer the reopen ledger excludes | **not a cause** (12.12): the exclusion narrows a pad and never empties one, so a box with both answers excluded is dealt both again. A box nothing can answer is a screen nothing can leave |
| a scene that binds nothing at all | the table above: every playable scene but the overworld has at least one unconditional button (`NEXT` in a dialog and in a battle, `CLOSE` in a menu, `LEAVE` in a shop and a PC). The overworld's was `MENU`, and **12.11** took it off rather than keep a button whose only effect is a screen its own scene closes again; what stands in its place is the row below |
| an overworld map with no way out at all | **not closed, and named**: with `MENU` gone this is a genuinely empty pad. No map in Red is that -- an interior has its front door or its staircase, an outdoor map has its connections -- so it is asserted as a residual in the pad-empty sweep rather than covered, and `game.padEmptyMs` reports it |
| an *indoors* overworld where every ledger excludes everything and the blocked window is resting the one door | **fixed** (12.11): `ways`' last resort is a room's too, not only `GO ROUTE`'s. With nothing else on this map worth walking to, the exits of that kind come back ignoring the blocked window, the one toward the objective preferred. This is the rung-10 museum: its only way out is a *passage*, so tier 3 was built out of the excluded list and emptied with it |
| an overworld where the talked, reached and blocked ledgers exclude every person, object and frontier tile, every route leads somewhere visited, and there is no objective hop | **fixed**: `ways`' last resort. With *nothing else on this map worth walking to* (`palette::stranded`), `GO ROUTE` offers the exit toward the objective and, failing that, all of them — **ignoring the blocked window**, because a target the ledger is resting is still the only place to go. It is a last resort and not a tier: with anything else on the pad it stays off, because "all of them, nearest" once per hold is row 2's own loop |
| the frontier is empty because `path::frontier` answers about the ten-by-nine walkable window, while most of the map is unstood | **fixed**: `frontier_aims` falls back to the nearest tile of the *whole* map the stood ledger has no entry for. A fallback and not the rule, because the windowed answer is the correct one whenever it has anything in it |
| a running macro aborts and the pad is not re-dealt until the next frame | not a cause: `observe` runs after every frame and the decoder is handed the bound channels again on the very next one |
| a restore, before the first `observe` | not a cause: the loop calls `observe` once at the end of boot and once after a ratchet recovery, so the first frame is decided on a real palette |
| the title screen, and raw mode | not an empty pad by contract: the readout's boot variant applies and no palette is dealt |
| a scene the detector cannot name | `Unknown` deals `NEXT` and `BACK`; a screen neither press leaves (the naming screen, row 14) is a genuine stall and still needs START, which is a contract change |
| an `Unknown` frame that is the **overworld with the cartridge driving** -- a warp in flight, a scripted push-back, a guide walking the fly through a door | **empty on purpose** (12.13). There is no box to advance and no screen to leave, so an A or a B press is a press into somebody else's script: it changes nothing and completes where the fly stands. This is the one empty pad that ends itself -- the cartridge gives the buttons back within a few frames -- and `game.padEmptyMs` reports it like any other |
| the fly's own turn where the seam cannot read the battler, now that `NEXT` is off that row (12.10) | **fixed**: `MOVE 1` is bound over the top-level menu whatever `wBattleMon*` reads as, because FIGHT is one of that menu's four entries and always opens |

And because "closed where a macro can close it" is not "closed":
`game.padEmptyMs` publishes how long a **playable** scene has had nothing on the pad, in brain
milliseconds, 0 otherwise; a running macro counts as not empty, because it owns the pad, which is
the honest reading of what the audience is looking at. `fly-watchdog` exports it as
`fly_pad_empty_seconds`. Both are **report only**: nothing in the loop reads it back and nothing
presses a button because of it.

### 14.1 One slot per type, and what the four move buttons removed (2026-09-17)

Section 14's screen asks for every macro type at once, and the battle's own turn wants nine
buttons. Six slots was section 1's D-pad and A/B, and it stayed a cap on how many buttons a scene
could deal long after section 12 stopped a macro being a meaning laid over a button. **A slot is
now a type index**: thirty-one slots, the screen draws every cell in the fixed order and lights
the ones this scene binds, the wire carries the bound ones with their type index as `slot`, and
nothing truncates anything. The decoder never cared — it competes among the bound *channels*.

That retires two fixes: row 12 (`GO FRONTIER` rescued from the truncation) and row 18 (`MENU` on
no pad at all) are both about a cap that no longer exists.

- **Types (31)**, in channel order: GO OBJECTIVE, GO OUT, GO WARP, GO ROUTE, GO ITEM, GO NPC,
  GO FRONTIER, GO SHOP, GO HEAL, TALK, MENU, NEXT, YES, NO, CLOSE, CONFIRM, BACK, MOVE 1, MOVE 2,
  MOVE 3, MOVE 4, SWITCH, ITEM, THROW BALL, RUN, BUY POTION, BUY BALL, BUY ANTIDOTE, BUY REPEL,
  HEAL, LEAVE. `GO STAIRS` is **`GO WARP`** (tag `MB·WARP`): stairs, ladders and the doors
  between rooms are all the same move and the old name only described one of them.
- **`MOVE n`** is bound when slot `n` holds a move with PP. `MOVE 1` is also bound when *nothing*
  anywhere has PP, because Struggle is what the cartridge does then and the only way to it is to
  choose a move anyway (rows 30a and 34); its script confirms wherever the cursor stands rather
  than moving it onto some other spent move. Over the main menu a move button presses FIGHT and
  then its slot; over an open list it goes straight to the slot. **The `best_move` knowledge, the
  ROM move table and the type chart are removed**, not narrowed: which move is used is the fly's
  choice and the mushroom body's to learn, and that was the point of the four buttons.
- **`THROW BALL`** ("throw pokeball should be a macro") is on the own-turn pad in a **wild**
  battle when the bag holds a ball of any kind and the party has room. The script opens ITEM and
  moves the cursor to the ball's own bag index by reading the list; the throw, the shakes, the
  result text and a caught Pokémon's nickname prompt are the between-turns `NEXT`'s and the
  dialog's `NO`. No catch rate and no HP: when to throw is the fly's. **A full party takes the
  button off the pad** even where a PC box would have taken the catch, because this crate has no
  reviewed symbol for the box count — the ordinary narrowing, said out loud.
- **Populations.** Thirty-one `macro_<type>` roles, cut from the same 96 MBONs plus 110 brain
  motor neurons by the same round-robin: seven or six per type. 206 neurons dealt thirty-one ways
  is not twenty-two ways plus nine, so **every population was re-dealt**, exactly as it was when
  section 11's amendment went from six slot channels to twenty-two type channels. The neuron ids,
  the edges and the kernel are untouched and rates are restored by role name, so
  `--print-compatibility` is byte-identical (648 bytes) and the live checkpoint carries over: the
  new and renamed roles start at zero, and what a run loses is the mushroom body's learned
  preference for particular macros. `circuit-roles.json` regenerated with
  `tools/build_flywire.py --macro-roles`, checksums updated.
- **Tags.** `MB·MV1`..`MB·MV4`, `MB·BALL` for `THROW BALL`, `MB·WARP`, `MB·SHOP`, `MB·HEAL`,
  `MB·NURSE` for the conversation at the counter, `MB·ANTI`, `MB·REPEL`, and **`BUY BALL` becomes
  `MB·PBALL`**: the ball the fly throws takes `MB·BALL`, and two channels cannot share a tag on a
  screen that identifies a cell by it.

### 14.2 The grid was reverted, then the layout was decided

**Built, measured, and taken out again.** The two-column grid of all thirty-one types needed
**11 px** type — the widest name and tag are `BUY ANTIDOTE` and `MB·NURSE`, and at 24 px they want
two columns of 346 px in the strip's 364 and 272 px of height in its 208 — and this page's floor
is 24 px at the 0.31 phone downscale. A floor with an exemption in it is not a floor, so the strip
is section 12's again: **six rows, the scene's bound macros in type order**, 24 px, and
`apps/stage/src/lib/geometry.ts` is byte-identical to what it was.

What section 14 *keeps* is the part that is not about pixels: thirty-one types, a slot that is a
type index on the wire, and a pad that binds as many as the scene has. The strip is now narrower
than the pad, and the cost is real and asserted rather than hidden:

- **a centre binds nine macros and `HEAL` is type 29, so the strip cannot show it.** The audience
  sees the fly heal in the ticker and not the button it pressed
  (`apps/stage/tests/e2e/macros.spec.ts`, "the mart deals purchases, and the centre deals HEAL").
- the battle's own turn binds nine too, so two of `MOVE 1`..`MOVE 4`, `THROW BALL`, `RUN` or `NEXT`
  are off the strip whenever all nine hold.

**And then it was decided** (the operator, 2026-09-17: "your rec is good"), which is the paragraph
at the end of section 14 above: the strip under the game shows every button *on the pad now*, up to
fourteen as two columns of seven at the floor, tag and short name, the running one bright; and a new
rail tab, MACROS, shows the whole keyboard as three columns of eleven with the bound cells lit and
the unbound dim. The game stays at 5x and the afterglow row stays. That is what is built; this
section stays because the arithmetic behind the decision is the measurement above, and because the
six-row strip is what the branch shipped for an afternoon in between.

### 14.3 What the grid cost while it existed, for the record

Two consequences of drawing all thirty-one cells at 1920x1080, both measured in the built page
rather than argued:

- **The 24 px type floor was broken inside the grid, and only there — which is why it is gone.** The widest name and tag are
  `BUY ANTIDOTE` and `MB·NURSE`; at 24 px they want two columns of 346 px in the grid's 364 and
  272 px of height in its 208. **11 px** is the largest that fits. `apps/stage/tests/e2e/text-size.spec.ts`
  now measures the grid against `MACRO_CELL_FONT_SIZE` and holds 24 px everywhere else — a
  documented exemption for one region with the arithmetic behind it. On-screen copy is the
  operator's, and an exemption is not this branch's to grant, so it was reverted.
- **The fly was not scooted over.** Section 14 asks for it and the grid cannot move left of
  x = 480 without putting text into the bottom-left no-content zone (0,900 → 480,1080), where
  Twitch overlays chat: taking 80 px off the fly would widen a column 179 → 219 and the type
  11 → 14 px at the price of nine of the sixteen rows' glyph chips sitting under the overlay. What
  the grid was short of was *height*, and the button row's 36 px band is where that came from
  (the button row now spans the fly's 416 px column rather than the strip's 792). Two constants
  reverse it if the type matters more than the overlay.

`LAYOUT.macroPalette` was `{x: 480, y: 816, w: 364, h: 212}` while the grid existed — one box
moved, the strip's top edge from 852 to 816 — and it is back to `{x: 480, y: 852, w: 364, h: 176}`.
Nothing in the layout differs from main.

## 15. Map-aware walks: one plan over the whole map (the operator, 2026-09-22: "the frontier and
## warp macros need to be map aware: A* over walkable tiles.")

Section 4 has said "A* over the current map's walkable tiles" since the first draft, and it was
never that. The walkable predicate answers for the ten tiles by nine of the screen buffer and
`Walkable::Unknown` for everything else (`docs/design/macros-wram.md`), so every walk in the game
planned through guesses at eight times the price of a known tile, re-planned at every window edge,
and `GO FRONTIER` aimed at whatever unstood ground was on screen -- never the far side of a town,
which nothing could see.

What changes is where the tile ids come from. The rule does not change at all: a tile is walkable
when the current tileset's collision list holds its id, which is `CheckTilePassable`.

- **The whole map is decoded** from the tables the cartridge has already loaded: the block ids out
  of `wOverworldMap`, the block-to-tiles blockset out of the tileset header, the collision list as
  before, and the `TilePairCollisionsLand` pairs as **directed walls**. `MapGrid` is every tile of
  the loaded map with those walls, and `docs/design/macros-wram.md` section 9 is the byte-level
  evidence, the new addresses and the verification.
- **One read of a ROM bank was needed, so `MemoryReader` gained one method.** The blockset does
  not live in bank 0, and the only way to reach another bank through the CPU bus would be to
  *write* the mapper's bank register. `MemoryReader::read_rom(bank, address)` reads the cartridge image the process
  already holds instead: the same bytes, addressed the way the disassembly addresses them, and no
  write into a running game. The joypad register is still the only write (section 12).
- **`path::route` and `path::frontier` plan over the grid**, and the plan is the same A*: one step
  per tile, the multi-goal heuristic, the committed route of section 12.3, re-planned only on a
  refusal or a displacement. `GO WARP`, `GO OUT` and `GO ROUTE` route to their warp or connection
  tile across the whole map; `GO OBJECTIVE` takes the exit that is the first hop; the frontier is
  the nearest unstood walkable tile *anywhere on the map*, by the stood ledger of section 12.7.
- **The window stays as the fallback, and it says when.** A frame with no grid falls back to the
  ten-by-nine reading exactly as before, and `GridRefusal` names which of the five reasons it is:
  no map header, no player, no collision list, no blockset (which is what a reader with no
  cartridge behind it answers), the map not on screen, or the decode disagreeing with the screen.
  Nothing is guessed and nothing silently degrades.
- **The grid is checked against the screen before it is trusted, and again whenever it is served.**
  The decode is compared with the window predicate over the tile the fly is standing on and its
  four neighbours, and a frame where the window can answer for none of them is refused — the block
  data shares its bytes with the picture buffer, so a battle is exactly when it belongs to somebody
  else. Serving a cached grid re-checks the fly's own tile, because a warp writes the map id before
  the header and the blocks: for a frame or two on a doormat, `wCurMap` is the map the fly is
  arriving on and the blocks are still the map it is leaving.
- **Cached per map, decoded once on arrival**, dropped when the map or its size changes. Session
  state beside the talked, blocked, reached, stood and errand ledgers; never checkpointed, so a
  restored run decodes the map again on its first overworld frame.
- **The probes report it.** `examples/scene_probe.rs` prints the grid's size, its walkable count,
  the count reachable from where the fly stands and the count never stood on, draws the ground with
  the reading it used, and names the refusal when there is none; `examples/trap_hunt.rs` carries the
  same line into every trace line and into the summary table. Three numbers read a stalled walk at
  a glance: a fly with forty walkable tiles and four reachable ones is fenced in, and no amount of
  re-planning will help it.

**Nothing about the choice moves.** The scene deals the same buttons, the readout presses them, and
what changed is what a chosen walk knows about the ground — which is where section 12 puts
knowledge. The decoder, the reward catalog, the adapter version and the compatibility string are
untouched: 648 bytes, `0d9bfde7…707fa`, byte-identical across the change.

Two things the cartridge settled rather than the design, both measured and both written up with
their bytes in `docs/design/macros-wram.md` section 9: the collision id of a map tile is the
**lower left** of its four screen tiles and not the upper left, and **a warp writes the map id
before the map**, which is what the per-serve check above is for.

### 15.1 The proof

- **Unit tests, no cartridge.** The decoder against a made-up tileset: a block's four quadrants,
  a collision list over a map wider than the window, a tile-pair collision as a wall in both
  directions and not in another tileset, a block id past the end of the blockset staying `Unknown`,
  and the reachable count over a fenced region. The reader against synthetic WRAM with a synthetic
  blockset in a synthetic bank: the whole map decoded, a screen that disagrees refused, a reader
  with no cartridge refused, a battle frame refused, and the cache holding one map. The search over
  a grid: one plan across a map larger than the window where the window's own plan walks into a
  wall it cannot see, a frontier beyond the window where the window's frontier is empty, a
  tile-pair wall planned around, and a connection whose walls are not goals.
- **ROM-gated, from the release container's own checkpoints** (`tests/rom_map_grid.rs`, `FLY_ROM`
  plus a checkpoint, skipped cleanly without either). On **Pallet Town** — 20x18, 221 walkable
  tiles, 207 of them reachable, none unknown — and on **Viridian Forest** — 34x48, 719 walkable,
  all reachable, none unknown: the decode agrees with the window predicate on all ninety tiles the
  window can answer for, and it agrees with a **survey of real presses** on every one of the first
  120 tiles the walk can stand on (58 refused presses on the town, 74 in the forest, every one of
  them a wall, a directed wall or a tile a sprite was standing on; 10 presses in the forest that
  the cartridge answered with a battle, which say nothing about the ground either way).
  `GO FRONTIER` in the forest planned to ground outside the window and walked there in 215 frames
  (615 of its frontier tiles are outside the window); every way out of the forest is one plan away,
  27 to 149 steps, with no guessed tile in any of them.
- **The trap hunt**, twenty brain minutes from the rung-9 checkpoint, against `main` at v0.4.2:
  **286 distinct (map, tile) become 489**, the median flagged window holds 55 tiles instead of 16,
  `GO FRONTIER` runs 64 walks worth up to twelve net tiles instead of four worth one, and one walk
  spends its cap where six did. It costs battle: flagged windows go 61 to 70 and windows under four
  tiles 5 to 14, every one of them a window spent inside a battle, because a fly that covers more
  ground walks into more grass. `infra/docs/macros-traps.md` has both runs whole and says so at
  length; this is the one measurement in this file where more ground and fewer flags do not both
  hold.
