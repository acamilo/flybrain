# Rewards and learning

The live reward catalog of the Pokémon Red adapter, `pokered-unique8-v5`. The code of record is
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
| `boundary` | `explore` | +0.05, +0.10 | 100 ms | First tile adjacent to one of the map's exits, and the exit tile itself; once per `(map, exit)` for the lifetime of the ledger |

Every value is positive: there are no loss or blackout penalties, and `catalog::rule("blackout")`
is `None` by test. The values in one frame sum into `R`, and the network reinforces once with
`m = tanh(R)`. Stimulation pulses that overlap take their maximum, which the neural side owns.

The feed-kind column is `RewardKind::from_adapter` in `services/flysim/crates/flysim/src/snapshot.rs`:
`docs/feed-protocol.md` publishes seven counters, and an adapter kind that has no counter of its
own shares the nearest one. It still reaches the page as an event with its own label.

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

The catalog now includes exits. That is worth saying plainly on the honesty panel, because paying
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
