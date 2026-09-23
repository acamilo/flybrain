# Rewards and learning

The live reward catalog of the Pokémon Red adapter, `pokered-unique8-v7`. The code of record is
`services/flysim/crates/flybrain-gb/src/pokemon_red/` (`catalog.rs` holds the values, `mod.rs` the
gates and the rules); this page says what each rule pays for and why it is allowed to. The
prototype's own `docs/rewards-learning.md` in `fly-plays-pokemon` is where the first seven rules
were argued from the pret/pokered disassembly, and `docs/integration.md` records the v3 catalog as
it shipped there.

Rewards are *design*. The buttons are always the fly's, the readout is fixed and global
([readout](readout.md)), and learning happens on the plastic edges described in
[plasticity](plasticity.md). Changing this page changes what the fly is paid for; it does not
change what the fly can do.

## Reward rules

| Kind | Feed kind | Value | Stimulation | Trigger and budget |
| --- | --- | ---: | ---: | --- |
| `milestone` | `story` | +1 | 250 ms | Each selected story flag once; "adventure started" after an observed boot |
| `exploration` | `explore` | +0.05 | 80 ms | Each additional 8 unique controllable `(map, x, y)` locations, capped at 25 payouts (200 locations) per map |
| `map` | `area` | +0.2 | 100 ms | First stable controllable visit to a new map, after the first sample's baseline |
| `species` | `pokedex` | +0.5 | 200 ms | Each of the 151 newly owned species bits, gifts and evolutions included |
| `trainer` | `trainer` | +0.5 | 200 ms | Each named `EVENT_BEAT_*` flag once, except the flags classified as story milestones |
| `battle` | `wildwin` | +0.1, +0.05, +0.0333 | 100 ms | At most three observed wild KOs per `(map, species, level)` |
| `badge` | `badge` | +3 | 400 ms | Each newly set badge bit |
| `boundary` | `explore` | +0.05, +0.10 | 100 ms | First tile adjacent to one of the map's exits, and the exit tile itself; once per `(map, exit)` for the lifetime of the ledger. **Nothing on an indoor map** (since v7): the exit is still recorded, and pays 0 |
| `catch` | `wildwin` | +0.30, +0.10 | 150 ms | A wild Pokémon kept by a ball: +0.30 for a species this run had never owned, +0.10 for a repeat; at most three payouts per species for the lifetime of the ledger |
| `talk` | `explore` | +0.10 | 100 ms | A conversation the fly opened with a person or a sign **indoors**, paid when its box closes; once per `(map, sprite slot or sign text id)` for the lifetime of the ledger |
| `item` | `explore` | +0.15 | 120 ms | An item ball or a hidden item picked up, on any map; once per item for the lifetime of the ledger |

Every value is positive: there are no loss or blackout penalties, and `catalog::rule("blackout")`
is `None` by test. The values in one frame sum into `R`, and the network reinforces once with
`m = tanh(R)`. Stimulation pulses that overlap take their maximum, which the neural side owns.

The feed-kind column is `RewardKind::from_adapter` in `services/flysim/crates/flysim/src/snapshot.rs`:
`docs/feed-protocol.md` publishes seven counters, and an adapter kind that has no counter of its
own shares the nearest one. It still reaches the page as an event with its own label.

Two consequences of that sharing are worth stating rather than discovering. `catch` publishes on
`wildwin` because a catch is a wild battle the fly won by keeping the Pokémon, and *not* on
`pokedex` because the `species` rule already pays for the Pokédex bit the same catch sets --
counting it twice would be the dishonest option. And the stage's ticker copy is keyed on the feed
kind, not on the catalog kind (`apps/stage/src/games/pokemon-red.ts`), so the row for a catch
currently reads "wild win". The event's own label, `CAUGHT #<species>`, is what reaches the event
log, `/status` and the checkpoint. Changing the ticker copy means opening the feed's closed kind
set, which this rule deliberately did not do.

`talk` and `item` publish on `explore`, for the same reason `boundary` does: each is the fly
finding what is in a place -- new ground, a door, a person or sign it opened, an item it picked up
-- at the same quiet scale (0.05 to 0.15). Not `area`, which counts maps and is a notable row; not
`story`, which is the plot; not `wildwin`, which is a battle. No feed kind was added, so
`docs/feed-protocol.md` and the stage's switch statements did not move. What did move is the one
word that would have been untrue: the Pokémon Red ticker's `explore` row said "new place", which is
not what a conversation or an item is, and now says **"new find"** ("3 new finds" collapsed), which
is true of all four. The event labels -- `TALKED TO #<slot> IN AREA <map>`, `READ SIGN #<id> IN
AREA <map>`, `FOUND ITEM #<item>`, `FOUND A HIDDEN ITEM` -- reach the event log, `/status` and the
checkpoint. Both also reset the stage's stall meter, which counts `explore`: engaging with a
building is progress in the sense the operator asked for.

## Catch rewards

The operator's decision of 2026-09-22: the fly is paid for *keeping* a wild Pokémon, not only for
knocking one out. The rule is one kind with two payouts, the way `boundary` is.

**How a catch is read.** From `wCapturedMonSpecies` (`$d11c`), whose comment in `ram/wram.asm` at
the pinned commit is "0 if no mon was captured". `ItemUseBall` zeroes it before every throw
(`.canUseBall`) and writes `wEnemyMonSpecies` into it only on the branch that keeps the Pokémon;
`UseBagItem`'s `.returnAfterCapturingMon` zeroes it again and sets `wBattleResult` to 2 on the way
out of the battle. `wBattleResult` is 2 on exactly two paths in the whole game -- that one, and a
link battle whose opponent ran -- so requiring both the species and the result means a byte read
out of a half-initialised battle cannot pay. The adapter records the species during the battle and
pays on the way out, where the wild-KO payout already lives.

Not from `wPartyCount`. A catch with a full party raises `wBoxCount` instead, and `wPartyCount`
also rises for a gift, a trade and a Pokémon taken out of the PC, so it would need a second rule
to mean anything. The cartridge's own flag needs none.

**What counts as a new species.** The `species` payout inside the same battle. Nothing but a catch
can set a `wPokedexOwned` bit during a wild battle, so a `species` payout between the battle
starting and the ball keeping the Pokémon *is* that Pokémon being new to the run. It is read this
way rather than off `wCapturedMonSpecies` because that byte is the cartridge's **internal** species
index while the owned bitset is by **Pokédex number**, and nothing in WRAM converts between the two
(`docs/design/macros-wram.md` section 2, "species numbering"). A battle restored from a checkpoint
written before this rule existed carries no "species payouts when it started", which reads as
"cannot tell" and pays the repeat amount: the conservative half, and at most 0.20 once.

**The budget.** Three payouts per species for the lifetime of the ledger, the same cap and the
same reason as the wild-KO rule's three: a species the fly can find over and over is a farm, and
three is enough for the behaviour to be learned. A rollback blocks every species already paid,
exactly as it blocks every wild-KO key already paid, so the same catch cannot be replayed for
reward. A Safari Zone or old-man battle pays nothing, because the whole sample is dropped a step
earlier with a visible mode; a trainer battle pays nothing, because balls cannot be thrown in one.

**The scale.** 0.30 on its own is below a new Pokédex entry (0.50), below a story flag (1.0) and
well below a badge (3.0). A catch of a new species pays 0.80 across two kinds, which sits between
a story flag and a badge -- deliberately, because it is the one event that is both a discovery and
a thing the fly had to do on purpose.

## Engagement rewards

The operator's decision of 2026-09-23, recorded with the port decisions: reward the fly for
engaging *inside* buildings and stop paying it for leaving them. It was chosen over a pad rule and
over weighting the choice, and it is a catalog change -- an operator decision, like the catch
reward -- not a loop-review fix (`docs/loop-review.md`). It answers a shape the loop reviews kept
finding in Pewter: `GO OBJECTIVE` into a building and `GO OUT` straight back, paid for the door on
the way out and for nothing inside.

**Indoors** is two of the cartridge's own tables, and nothing hand-classified
(`pokemon_red/engage.rs`, `indoor`). `CheckIfInOutsideMap` (`home/overworld.asm`) is the game's
outdoor test -- tileset `OVERWORLD` or `PLATEAU` -- and `WarpFound2` labels its other branch
`.indoorMaps`; on its own that would call Viridian Forest and every cave indoors, and their exits
are how the fly gets anywhere. `BikeRidingTilesets` (`data/tilesets/bike_riding_tilesets.asm`) is
the list of places the bicycle may be ridden -- `OVERWORLD`, `FOREST`, `UNDERGROUND`, `SHIP_PORT`,
`CAVERN` -- and the bike is the one thing the cartridge refuses inside a building by rule. A map is
indoors when its `wCurMapTileset` is in neither: every house, mart, Pokémon Center, gym, gate, lab
and museum, the S.S. Anne, Silph Co., the Pokémon Tower, the Mansion, the Rocket Hideout and the
Indigo Plateau's rooms. Not the forest, a cave, the Underground Path or Vermilion's dock.

**`talk`, +0.10.** Paid on the sample the text box closes, for a conversation that

1. *the fly opened*: on the last sample before the font bit (`wFontLoaded` bit 0) rose, the fly had
   the joypad -- no `wJoyIgnore`, no simulated input, no scripted movement -- was standing still
   (`wWalkCounter` zero, the only state the overworld reads A in) and stood where it stands now. A
   script's text opens with the joypad taken, or on the frame a step onto a trigger tile ends;
2. *is with the thing in front of it*: `DisplayTextID` copies its argument into `wSpriteIndex` --
   a sprite slot up to `wNumSprites`, or a text id -- and the sprite must stand on the tile the
   player faces (or one further, across a counter, on a tileset that has counter tiles, which is
   `IsSpriteOrSignInFrontOfPlayer`'s own long reach), or the text id must be the sign's on that
   tile. The byte arrives about **twenty frames after** the font bit (measured on the cartridge:
   `DisplayTextIDInit` loads the font's tiles first) and until then still names the previous
   text's subject, so it is read once it has changed or 45 samples have passed, and only while the
   bottom dialogue box is drawn -- the start menu draws its own box elsewhere. An item ball is a
   sprite but not a person, and pays `item`;
3. *opened indoors*, on the map the box opened on;
4. *finished*: the box closed on the same map. A conversation that ends in a warp, a rollback or a
   restore pays nothing.

The ledger is the adapter's lifetime `seen` set, keyed `talk:<map>:sprite:<slot>` or
`talk:<map>:sign:<text id>` -- the same "map and object index" the macros' `talked` ledger uses,
but **not** that ledger: the macros' ledger is session state and is thrown away on a restore; this
one is checkpointed and survives a rollback, so talking to the same person again, after a restore
or not, pays nothing. The trainer the fly speaks to before a battle is a person and pays once; the
nurse, a clerk and a sign each pay once per map.

**`item`, +0.15.** Read from the cartridge's own "this one has been taken" bits, on any map.
An item ball is one of the map's toggleable sprites (`wToggleableObjectList`, sprite slot and
global index) whose `wMapSpriteExtraData` is `(item id, 0)` -- the shape `LoadMapHeader` writes for
an `ITEM` `object_event` and for nothing else (a trainer's is `(class, number)` with numbers from
1, a person's two zeroes); `PickUpItem` sets its global bit in `wToggleableObjectFlags` through
`HideObject`, and only after `GiveItem` succeeded, so a full bag pays nothing. A hidden item is a
bit of `wObtainedHiddenItemsFlags`, set by `FoundHiddenItemText` after `GiveItem` and by nothing
else; hidden *coins* have their own bitset and are not items. Either pays when its bit rises
between two playable samples, keyed `item:<global index>` or `hidden:<index>`, once for the life
of the ledger. A gift item from a script (the Old Amber, a TM from a person) is not an item ball:
the conversation pays `talk`, and the item nothing.

**The seed.** The first playable sample that finds the key `items:seeded` absent -- a fresh
adapter, or a `v6` ledger restored under `v7` -- writes a key for every bit the game already shows
as taken and pays for none of them, so a rollback to a slot from before a `v6`-era pickup cannot
pay for taking it again. The two item balls a script *reveals* are left out of the seed, because
their bits are set from a new game until Giovanni's defeat clears them: the Rocket Hideout's Silph
Scope and Lift Key (`$87`, `$88`), the only `ITEM` entries `data/maps/toggleable_objects.asm` starts
`OFF`.

**`boundary` indoors.** Every exit on an indoor map is still written to the ledger, so
`exit_visited` answers exactly what it did and the macros see no change, but nothing is paid. A
town's doors, a route's edges, the forest's gates and a cave's ladders pay as before. One
consequence, measured on the cartridge (`tests/rom_engage.rs`): for the thirty-odd frames of
`PlayMapChangeSound` the cartridge has written the destination into `wCurMap` while the tileset and
the warp table are still the map being left, so the exit the fly is standing on is classified by
the map it belongs to. Walking into a building through a town door still pays that door's on-exit
half, 0.10, once, keyed under the building's id as it always was; walking out pays nothing.

**The scale.** A building's worth of engagement -- a few people, a sign, perhaps a ball -- is
0.3 to 0.6: more than the 0.15 its door paid for being left, less than a new Pokédex entry per
person, far below a badge. Everything is once per thing for the lifetime of the ledger, so no
building can be farmed.

## Gates

Semantic rewards are enabled for exactly one cartridge, the SHA-256 in `SUPPORTED_ROM`. Any other
cartridge samples without paying anything and the adapter reports
`UNSUPPORTED ROM . SEMANTIC REWARDS OFF`, so the stream keeps running with the reason on screen.
Even the canonical pret build stays off the list until someone has checked that its WRAM layout is
the one these addresses were resolved against: a reward rule reading the wrong byte is worse than
no reward rule, because it looks like it works.

A payout needs a *playable* sample: game timer active, map id at most 247, non-zero map
dimensions, coordinates inside the map, party count at most 6, an ordinary battle type and no
test-battle flag. Map, exploration and boundary observations additionally need overworld battle
state and three stable samples at the same location.

Map and exploration then require an unscripted sample: `wStatusFlags5 & 0xa1`, `wJoyIgnore`,
`wMovementFlags & 0xc7` and `wStatusFlags6 & 0x5c` all clear. `boundary` keeps its own gate, which
is the same except that it allows `wMovementFlags` bits 0 to 2 — `BIT_STANDING_ON_DOOR`,
`BIT_EXITING_DOOR` and `BIT_STANDING_ON_WARP`. Standing on a door is the state that rule exists to
pay for, so under the shared gate its on-exit half could never fire at all. A ledge hop, a spin
tile, an ignored joypad and both status-flag masks still block it, and no other payout or
observation reads that gate.

The first valid sample baselines every already-set event, species and badge bit, the current map
id, and the exits within one tile of where the fly is standing. Loading existing progress
therefore replays none of it.

## Boundary rewards

Since `pokered-unique8-v7` everything below holds **outdoors** -- towns, routes, the forest,
caves -- and on an indoor map the same keys are written and nothing is paid ("Engagement rewards"
above has the definition of indoors and the one transition frame worth knowing about).

`docs/design/room-escape.md` section 2. The rule pays 0.05 the first time the fly stands on a tile
orthogonally adjacent to one of the current map's exits, and 0.10 the first time it stands on the
exit tile itself. Both are keyed into the adapter's lifetime `seen` ledger as
`boundary:<map>:<y>:<x>:near` / `:on` for a warp and `boundary:<map>:edge:<e|w|s|n>:near` / `:on`
for a map edge, so each `(map, exit)` pays at most 0.15 in the lifetime of a run and oscillating on
and off a doormat cannot farm it. The ledger survives a rollback and round-trips through a
checkpoint, which is why no new state was added for this.

Exits come from two places in WRAM, both verified against the disassembly at the pinned commit
`0cd19d3b877b7dc66d12c7050bed9a7f38154d4b`:

- **the warp table.** `wNumberOfWarps` (`0xd3ae`) entries of four bytes at `wWarpEntries`
  (`0xd3af`), each `Y, X, destination warp id, destination map id` — `ram/wram.asm`'s own comment,
  and `MACRO warp_event` emits `db \2, \1, …`, so Y really is first. `CheckWarpsCollision` compares
  those bytes against `wYCoord` and `wXCoord` with no conversion, so a warp's coordinates are in
  the same tile space the adapter already samples. Doors, stairs, cave mouths and building
  entrances are all warps. `MAX_WARP_EVENTS` is 32 and the count is clamped to it.
- **the connected edges.** `wCurMapConnections` (`0xd370`) is a bitmask over EAST 1, WEST 2,
  SOUTH 4, NORTH 8. The crossing happens one step *outside* the map — `CheckMapConnections` fires
  on `wXCoord == $ff` going west and `wXCoord == wCurrentMapWidth2` going east, and
  `wCurrentMapWidth2` is `wCurMapWidth` doubled, which is the adapter's own `width` — so the exit
  tiles are column 0, column `width - 1`, row 0 and row `height - 1`. No further address is needed.

The design names these `wMapConnections`, `wNorthConnection` and friends; at this commit the
symbols are `wCurMapConnections` and `wNorthConnectionHeader`, and the four per-direction headers
are not read at all.

Consequences worth stating rather than discovering:

- arriving on a map *through* an exit lands on that exit, so the arrival pays it: on a connection
  it is the connection's own edge row, and on a warp it is the warp tile the destination put the
  player on. The live release run holds both halves of Red's staircase in its ledger
  (`boundary:37:1:7` and `boundary:38:1:7`, `infra/docs/room-escape.md`), paid for arriving down
  and up it. One payout of 0.10 per exit in a lifetime is not a farm, but it is not a discovery
  either.
- a warp that fires on the step onto it can have its on-exit half go unobserved in the *outbound*
  direction, because the cartridge overwrites the coordinates as part of the warp. The ROM-gated
  test in `services/flysim/crates/flybrain-gb/tests/rom.rs` sees exactly that: from a cold boot
  in Red's bedroom the fly is paid 0.05 at (6, 1), one tile west of the stairs at (7, 1), and
  never the 0.10. Doors and map edges are ordinary standable tiles and pay both halves outbound.

## Macros and the mushroom body (2026-09-16)

`docs/design/macros.md` section 12 puts each macro type on the pad as a button of its own, pressed
by its own neuron population. The honest sentence for what learning touches is: **the mushroom
body picks the macro; the descending neurons press the buttons.**

- Nothing in the catalog above moved. The values, the gates, the ledgers and the adapter version
  `pokered-unique8-v5` are what they were, and a macro's payout is read out of WRAM after the
  frames the macro produced, exactly as a raw press's is.
- The eight Game Boy buttons stay on `command_0..7` over the descending neurons. The 22 macro
  populations, `macro_<type>`, are shared out over the 96 mushroom body output neurons and the 110
  brain motor neurons instead (`docs/readout.md`, "Macro group").
- That split is the point. The plastic edges are the strongest Kenyon cell to MBON connections
  ([plasticity](plasticity.md)), so what `reinforce()` can move is MBON *input* synapses — and the
  MBONs are part of what every macro population is made of. A learned preference for one macro in
  one scene is therefore the one thing in this loop a reward can actually change, and it changes it
  where the real fly changes it.
- The claim stops there. Each population is a mix of MBONs and brain motor neurons in whatever
  proportion the round-robin over the sorted pool gives it, only the MBON share of it sits
  downstream of a plastic edge, and the choice between two bound macros is still the decoder's
  argmax over rates ([readout](readout.md)), not a policy.
- Payout reinforces the recent spike history, so the interval between a decision and the reward
  that follows it is now one macro long rather than one button press long.

## Honesty

The catalog now includes conversations and items (v7). Paying for a conversation is the closest
the catalog has come to paying for a *button*: A is what opens one. It is still a reward, not a
press. Nothing in the adapter presses A, chooses when, or tells the fly who is there; the payout is
read out of WRAM after a conversation the fly's own buttons -- or the macro the mushroom body chose
-- opened and finished, and it is once per person or sign for the life of the run, so the thing
that is learned is "the people in a building are worth a visit", not "press A". It is also why the
rule demands evidence that the fly opened the box: text a script started pays nothing.

The catalog also includes catches. The honesty panel's copy is not data-driven from the catalog --
`apps/stage/src/lib/schedule.ts`'s rotating card is four written lines and lists no kinds -- so
there was nothing to regenerate and the copy is unchanged. The sentences below are where the
argument lives.

Paying for a catch does not move the fly: the ball is thrown by a macro the mushroom body chose
among the ones the battle scene put on the pad, and the payout is read out of WRAM after the
frame. What it does do is make one of the palette's existing macros worth choosing, which is the
same kind of pressure every other rule applies. The cap is what keeps it from becoming a farm: a
run that finds one patch of grass and throws balls at the same species all night earns 0.50 from
it and then nothing.

The catalog also includes exits. That is worth saying plainly on the honesty panel, because paying
for a door is closer to telling the fly where to go than paying for a badge is:

- **still no button path.** Nothing in the adapter chooses or biases a button. The reward is read
  out of WRAM *after* a frame the fly's own buttons produced, and the only thing it can do is
  stimulate the modulatory pathway. In macros mode the scene decides which macro buttons are on
  the pad and the macro decides which presses it makes, but nothing decides *which* button the fly
  hits: there is no default macro, no fallback on a timeout and no scripted objective, and a scene
  whose buttons the fly ignores waits. That is the panel's own sentence for the mode — "the scene
  sets which buttons exist; the fly presses; the mushroom body learns which".
- **still no map knowledge in the readout.** `PopulationDecoder` does not know a map exists, let
  alone where its doors are. As of v0.1.1 it does take one input that is not a rate -- the name of
  a direction the sim loop has watched produce no movement for a whole hold
  (`docs/readout.md`, "Blocked-direction cooldown"), which raises that channel's habituation and
  nothing else. That is "the button you are holding did nothing", not "the door is south": it names
  no position, no destination and no alternative, the winner is still the argmax of the same scores
  over the same rates, and every button still comes from the network. What the fly gains is the
  ability to stop pushing against a wall, which before v0.1.1 was consuming two thirds of its motor
  output (`infra/docs/room-escape.md`).
- **the fly still has to find the door.** A reward for standing next to an exit is not a hint about
  where the exit is. It pays when the fly is already there.
- **what a reward can move is which macro, not which button.** The macro populations reach into
  the mushroom body's output layer, which is where the plastic edges end; the eight button
  populations are descending neurons and no plastic edge ends on them. That is what "the mushroom
  body picks the macro; the descending neurons press the buttons" means in mechanism. Whether it
  makes the fly play better is not known: `docs/design/macros.md` section 12's bench — raw against
  macros, 6 brain hours each from the live checkpoint — is the gate for running macros mode in
  release, and it has not been run. Nothing on this page rests on it.
- **it is a design choice, not a discovery.** The values in the table were argued and written down;
  none of them was learned or tuned against the fly's behaviour. A random walker collecting these
  payouts (`services/flysim/crates/flysim/examples/room_escape.rs` runs one, and the platformer's
  `random_walker.rs` is the calibration baseline the same doctrine gave that game) says something
  about the *scale*, never about the fly. The ceiling is small and bounded on purpose: a whole map's
  exits are worth less than one badge.
