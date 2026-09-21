# Macro palette: the WRAM it reads

Agent A's half of `docs/design/macros.md` section 8, built 2026-09-16. `macros.md` is the binding
contract; this file is the interface agent B codes against and the evidence behind it: every byte
the palette reads, its address at the pinned pokered commit, how it is encoded, and how the reading
was verified.

Sources: pret/pokered at **0cd19d3b877b7dc66d12c7050bed9a7f38154d4b**, the commit
`docs/design/ladder.md` pins and `symbols::POKERED_COMMIT` records. Addresses come from that
checkout's `pokered.sym` through `services/flysim/tools/gen_symbols.py`, which refuses to run
against any other revision and refuses to take a hand-written address; 30 names were added to its
`EXTRA_RAM` allowlist for this work, taking the table from 28 addresses to 58, and nothing else in
it moved — no event flag, no milestone, no existing address — so no compatibility string changes.
**2026-09-16, `GO ITEM` and the exit ledger:** three more names, taking the table from 58
addresses to 61 (`wNumSigns`, `wSignCoords`, `wSignTextIDs`), regenerated the same way and again
with nothing else moved; `flysim --print-compatibility` is byte-identical across the change, 648
bytes, checked on the WSL box. Encodings come from the `.asm` that writes them, cited per row.

Where it lives:

| file | what |
| --- | --- |
| `pokemon_red/macros/state.rs` | `Scene`, every value type, and `trait GameState`. Self-contained: no imports, nothing from the rest of this work, so B can read and compile it alone. |
| `pokemon_red/scene.rs` | `detect`, and `Scene` re-exported so `pokemon_red::scene::Scene` is the path `macros.md` section 2 promises. |
| `pokemon_red/state.rs` | the accessors, twice: free functions over a `&mut dyn MemoryReader`, and `PokeState` implementing `GameState` on top of them. |
| `pokemon_red/fake_wram.rs` | synthetic WRAM for the tests (test-only). |
| `tests/rom_scene.rs` | the ROM-gated checks, `FLY_ROM`. |

## 1. The scene

```rust
pub fn detect(memory: &mut dyn MemoryReader) -> Scene;
pub fn why_unknown(memory: &mut dyn MemoryReader) -> String;   // diagnostics, not a decision
```

Pokémon Red has no "which screen am I on" byte. It has a font flag, a text box id, a list menu id,
one shared menu cursor and a screen buffer, and `detect` is an **ordered** sequence of tests over
those, because the ordering is the safety property `macros.md` asks for:

| order | test | reads | scene |
| ---: | --- | --- | --- |
| 1 | the game has not started | `wStatusFlags6` bit 0 | `Title` |
| 2 | the map header or the party count is out of range | `wCurMap`, `wCurMapWidth`, `wCurMapHeight`, `wPartyCount` | `Unknown` |
| 3 | a battle is running | `wIsInBattle`, `wBattleType` | `Battle { own_turn, forced_switch }`, or `Unknown` |
| 4 | a PC is open | `wMiscFlags` bit 3 | `Pc` |
| 5 | a mart is open | `wTextBoxID`, `wListMenuID` | `Shop` |
| 6 | a text display is open | `wFontLoaded` bit 0, then the drawn box in `wTileMap` | `Dialog`, `Menu`, else `Unknown` |
| 7 | everything else | `wJoyIgnore`, `wSimulatedJoypadStatesIndex`, `wStatusFlags5`, `wStatusFlags6`, `wMovementFlags`, the coordinates | `Overworld` |

Battle is tested before anything on screen because a battle is *made* of text boxes and menus and
every frame of it must still read as a battle. The overworld is last and is the only branch with a
positive requirement on every gate. Anything left over is `Unknown`, which the doctrine treats like
`Dialog` — advance only.

`Unknown` is not a bug to drive to zero. The Pokédex, the trainer card, a naming screen and a
mid-warp frame are none of them pinned down by the bytes above, and reporting them as `Unknown` is
the design working. It is also what a half-written frame reads as.

### Two findings that shaped it

- **The reward adapter's scripted-frame mask cannot be reused whole.** `PokemonRedReward::sample`
  rejects a frame with `wMovementFlags & 0xc7`, and bits 0, 1 and 2 of that byte are
  `BIT_STANDING_ON_DOOR`, `BIT_EXITING_DOOR` and `BIT_STANDING_ON_WARP`
  (`constants/ram_constants.asm`). `docs/design/room-escape.md` section 3 is the story of a fly
  that spends its life on doormats: masking those would report `Unknown` on the four tiles that
  matter most in the game's first hour. The scene gate keeps `0xc0` — the ledge hop and the spin
  tile, which genuinely are not the fly's to act on — and drops the door bits.
- **A menu is recognised from the box the game drew, not from the cursor it parked.**
  `HandleMenuInput`'s state (`wTopMenuItemY`, `wTopMenuItemX`, `wCurrentMenuItem`, `wMaxMenuItem`,
  `wMenuWatchedKeys`) survives the menu closing, so the start menu's geometry is still in WRAM
  while the fly walks around. Every menu test therefore requires `wFontLoaded` **and** the drawn
  box **and** the geometry. A test for that is in `scene/tests.rs`
  (`the_cursor_geometry_alone_does_not_open_a_menu`).

## 2. The state table

Addresses are at the pinned commit. "Verified" is one of:

- **survey** — walked on a real cartridge with real button presses on throwaway emulators, the
  method `docs/design/room-escape.md` section 3 used (`tests/rom_scene.rs`).
- **ROM** — asserted against the running cartridge in `tests/rom_scene.rs`.
- **trace** — a synthetic WRAM trace in `scene/tests.rs` or `state/tests.rs`, built from the
  disassembly rather than from the implementation.
- **decomp** — read out of the `.asm` named in the row. Every row has this; the others are what a
  synthetic trace or a cartridge added.

### Scene detection inputs

| state | symbol | address | encoding | verified |
| --- | --- | ---: | --- | --- |
| the game has started | `wStatusFlags6` | `$d732` | bit 0 `BIT_GAME_TIMER_COUNTING`. `MainMenu` is the only writer (`engine/menus/main_menu.asm:334`) and nothing clears it, so it is "a save is running", not "the timer ticks". Same gate the adapter calls `active`, so `Title` and the adapter's `BOOT` agree by construction. | ROM (3,000 idle frames on the title screen are one scene, and the adapter says `BOOT`), trace |
| in a battle | `wIsInBattle` | `$d057` | 0 none, 1 wild, 2 trainer, `$ff` the frame a battle is lost | ROM, trace |
| which kind of battle | `wBattleType` | `$d05a` | 0 normal, 1 the old man's tutorial, 2 Safari. Both non-zero values have their own menus, so they read as `Unknown`. | trace |
| a text display is open | `wFontLoaded` | `$cfc4` | bit 0 `BIT_FONT_LOADED`. `DisplayTextIDInit` sets it for *every* text display, the start menu included; `CloseTextDisplay` clears it (`engine/menus/display_text_id_init.asm:33`, `home/text_script.asm:130`). | ROM, trace |
| which box is drawn | `wTileMap` | `$c3a0` | 20x18 screen tile ids. `DisplayTextIDInit` draws the dialogue box at screen (0, 12), four rows by eighteen columns, so rows 12 to 17 full width; the start menu's box at (10, 0), fourteen rows with the Pokédex and twelve without. Recognised by all four `TextBoxBorder` corners — `┌ ┐ └ ┘` are `$79 $7b $7d $7e` (`constants/charmap.asm`) — because a map tile can hold one frame tile id and not two. | ROM, trace |
| which text box template | `wTextBoxID` | `$d125` | `constants/menu_constants.asm`: `BATTLE_MENU_TEMPLATE` `$0b`, `BUY_SELL_QUIT_MENU` `$15`. | ROM, trace |
| which list menu | `wListMenuID` | `$cf94` | `constants/list_constants.asm`: `PRICEDITEMLISTMENU` `$02` (a mart's buy list), `ITEMLISTMENU` `$03` (the bag), `SPECIALLISTMENU` `$04` (an elevator). Zeroed by `DisplayTextIDInit` at the start of every text display, so a stale value cannot outlive one. | trace |
| a PC is open | `wMiscFlags` | `$cd60` | bit 3 `BIT_USING_GENERIC_PC`. `ActivatePC` sets it, `LogOff` clears it (`engine/menus/pc.asm`), covering Bill's, the player's and Oak's. | trace |
| the buttons reach the player | `wJoyIgnore` | `$cd6b` | non-zero means the joypad is being ignored | trace |
| ” | `wSimulatedJoypadStatesIndex` | `$cd38` | non-zero means the game is walking the player itself; `CollisionCheckOnLand` skips collision entirely then | trace |
| ” | `wStatusFlags5` | `$d730` | mask `$a1`: bits 0, 5, 7 = scripted NPC movement, joypad disabled, scripted movement state | trace |
| ” | `wStatusFlags6` | `$d732` | mask `$5c`: bits 2, 3, 4, 6 = fly, dungeon and escape warps in flight | trace |
| ” | `wMovementFlags` | `$d736` | mask `$c0`: bit 6 a ledge hop, bit 7 a spin tile. **Not** bits 0 to 2, see above. | trace |
| where the player is | `wCurMap`, `wXCoord`, `wYCoord` | `$d35e`, `$d362`, `$d361` | map id (`$f7` is the highest real one), tile coordinates. Y is *before* X in WRAM. | ROM, trace |
| the map's size | `wCurMapWidth`, `wCurMapHeight` | `$d369`, `$d368` | in blocks; one block is two tiles each way, so tiles = byte × 2 — the conversion the reward adapter already makes | ROM (Red's rooms 8×8, Pallet Town 20×18), trace |

### Party, the battler and the enemy

| state | symbol | address | encoding | verified |
| --- | --- | ---: | --- | --- |
| party size | `wPartyCount` | `$d163` | 0 to 6. **Leads the structs it counts:** `AddPartyMon` writes the count first and fills the 44 bytes over the frames after it. Measured on the cartridge: the count became 1 3,245 frames before the starter's species was written, because Oak's gift is spread across a script. A caller that cares must require a non-zero species. | ROM (the gap is asserted), trace |
| species list | `wPartySpecies` | `$d164` | six bytes then `$ff` | decomp |
| party member | `wPartyMon1` | `$d16b` | `party_struct`, `PARTYMON_STRUCT_LENGTH` = `$2c` = 44 bytes, six of them (`wPartyMon2` is `$d197` = `$d16b + $2c`). Offsets: species 0, HP 1 (big-endian), status 4, moves 8 to 11, PP 29 to 32, level 33, max HP 34 (big-endian). | ROM (the starter: Charmander `$b0`, level 5, 19/19, SCRATCH 35 PP, GROWL 40 PP), trace |
| species numbering | — | — | the cartridge's **internal index**, not the Pokédex number: Bulbasaur `$99`, Charmander `$b0`, Squirtle `$b1` (`constants/pokemon_constants.asm`). The reward adapter's `wPokedexOwned` bitset is by Pokédex number; the two numberings are different and nothing converts between them. | ROM |
| status | — | — | `constants/battle_constants.asm`: `SLP_MASK` `%111` is sleep turns, then bit 3 poison, 4 burn, 5 freeze, 6 paralysis | trace (every byte) |
| PP | — | — | one byte per slot: bits 0 to 5 remaining PP, bits 6 and 7 the number of PP Ups | ROM, trace |
| the Pokémon that is out | `wBattleMonSpecies` … `wBattleMonPP` | `$d014`, HP `$d015`, status `$d018`, moves `$d01c`, level `$d022`, max HP `$d023`, PP `$d02d` | the battle engine's own copy of a party entry, and the copy it damages, so this is "own HP" in a battle | ROM (it matches the party entry it was copied from), trace |
| which slot is out | `wPlayerMonNumber` | `$cc2f` | 0-based party slot | ROM, trace |
| the enemy | `wEnemyMonSpecies`, `wEnemyMonHP`, `wEnemyMonLevel`, `wEnemyMonMaxHP` | `$cfe5`, `$cfe6`, `$cff3`, `$cff4` | HP big-endian. Not written on the frame a battle starts — the reward adapter's own comment says the same — so the enemy is `None` for the first few hundred frames of a battle. | ROM (the rival's Squirtle, level 5, 20/20, and `None` on the first frame), trace |
| how many moves | `wNumMovesMinusOne` | `$cd6c` | the move count minus one, valid in a battle | trace |

### Battle menu and cursor, own turn against forced switch

`HandleMenuInput` is shared by every menu in the game, so which menu is up is read from where it
parked its cursor. All five bytes are contiguous: `wTopMenuItemY` `$cc24`, `wTopMenuItemX` `$cc25`,
`wCurrentMenuItem` `$cc26`, `wMaxMenuItem` `$cc28`, `wMenuWatchedKeys` `$cc29`.

| menu | signature | cursor | verified |
| --- | --- | --- | --- |
| the top-level battle menu | `wTextBoxID` = `$0b`, `wTopMenuItemY` = 14, `wTopMenuItemX` = 9 with watched keys `PAD_RIGHT\|PAD_A` (left column) or 15 with `PAD_LEFT\|PAD_A` (right), `wMaxMenuItem` = 1 (`DisplayBattleMenu`, `engine/battle/core.asm:2081` and `:2114`) | reported 0 FIGHT, 1 PKMN, 2 ITEM, 3 RUN: the game keeps the index *within* the column and `.rightColumn` adds two on selection | ROM (a fresh menu is FIGHT; RIGHT is ITEM; DOWN from there is RUN), trace |
| the move list | `wTopMenuItemY` = 12, `wTopMenuItemX` = 5 (`MoveSelectionMenu`'s regular menu, `:2492`) | the game's list is **one-based** — `wCurrentMenuItem` is `wPlayerMoveListIndex + 1` and `wMaxMenuItem` is the move count plus one — so the accessor reports the 0-based slot, and `None` for an index that names no move | trace |
| the party list | `wTopMenuItemY` = 1, `wTopMenuItemX` = 0, `wMaxMenuItem` = `wPartyCount - 1`, watched keys `PAD_A\|PAD_B` or `PAD_A` alone (`PartyMenuInit`, `home/pokemon.asm:201`) | 0-based party slot | trace |
| **a forced switch** | the party list, in a battle, with `wPartyMenuTypeOrMessageID` = `BATTLE_PARTY_MENU` (`$02`) at `$d07d`. `ChooseNextMon` is the battle path that sets it (`engine/battle/core.asm:1088`, and `:1389` for the "use next mon?" branch); choosing PKMN from the menu sets `NORMAL_PARTY_MENU` (`$00`, `:2316`), which is why the two are distinguishable. `wForcePlayerToChooseMon` (`$d11f`) is the byte `PartyMenuInit` turns into "A only, no way out". | — | trace |

`own_turn` is the top-level menu being open. `forced_switch` is the party list a fainted Pokémon
forces. A battle that is neither — text, an animation, the turn resolving — is
`Battle { own_turn: false, forced_switch: false }`, which is most of a battle's frames.

### Text box, start menu, mart, PC

| state | how | verified |
| --- | --- | --- |
| text box open / waiting | `open` is `wFontLoaded` bit 0; `waiting` is the full-width dialogue box drawn at rows 12 to 17. "Waiting" is honest about what it can know: the game is either printing into that box or waiting for A, and A is the button that advances it either way. Pokered has no "the text has stopped and wants a button" flag. | ROM (a dialogue in Oak's lab), trace |
| start menu | the box at (10, 0) with its bottom at row 15 (with the Pokédex) or 13 (without), plus `wTopMenuItemY` = 2 and `wTopMenuItemX` = 11 and `wMaxMenuItem` 6 or 7 (`DrawStartMenu`). `DrawStartMenu` stores the item *count* in `wMaxMenuItem`, not the highest index, and `DisplayStartMenu` wraps at one less. | trace (both with and without the Pokédex) |
| a submenu | the weakest rule here, and the reason `Unknown` exists: `wListMenuID` is the bag or an elevator list, or the party list geometry outside a battle. A submenu none of those catch reads as `Unknown`, never as `Overworld`. | trace |
| mart | `wTextBoxID` = `BUY_SELL_QUIT_MENU` (`$15`) for the BUY / SELL / QUIT choice, and `engine/events/pokemart.asm:17` is its only user in the game; the buy list is `wListMenuID` = `$02` and the sell list is the bag's own `$03`, recognised only while the mart's template is still the last one drawn. | trace |
| PC | `wMiscFlags` bit 3, above. | trace |

### Money and bag

| state | symbol | address | encoding | verified |
| --- | --- | ---: | --- | --- |
| money | `wPlayerMoney` | `$d347` | three bytes of big-endian BCD, two digits each. A nibble above 9 is not BCD and reads as 0 rather than as a plausible number. | ROM (3,000 on a fresh save), trace |
| bag | `wNumBagItems`, `wBagItems` | `$d31d`, `$d31e` | a count capped at `BAG_ITEM_CAPACITY` = 20, then `(id, quantity)` pairs, then `$ff`. The terminator wins over the count. | ROM (empty on a fresh save), trace |

### NPC sprites

| state | symbol | address | encoding | verified |
| --- | --- | ---: | --- | --- |
| sprite slots | `wSpriteStateData1`, `wSpriteStateData2` | `$c100`, `$c200` | 16 slots of 16 bytes each (`NUM_SPRITESTATEDATA_STRUCTS`). Slot 0 is the player. In `wSpriteStateData1`: byte 0 picture id, byte 2 image index (`$ff` = not on screen, which is what `LoadMapSpriteData` writes into the unused slots), byte 9 facing (`SPRITE_FACING_DOWN` 0, `UP` 4, `LEFT` 8, `RIGHT` 12). In `wSpriteStateData2`: byte 4 map Y, byte 5 map X, **both plus four**, because `MACRO object_event` emits `db \2 + 4` then `db \1 + 4` (`macros/scripts/maps.asm:16`). | survey + ROM (Mom, `object_event 5, 4` in `RedsHouse1F.asm`, is reported at (5, 4) and is exactly the tile three surveyed presses cannot enter), trace |
| how many | `wNumSprites` | `$d4e1` | sprites on the current map, bounding the walk | trace |
| the player's facing | `wSpriteStateData1 + 9` | `$c109` | slot 0's facing byte | ROM, trace |
| a person or an object | — | — | `FIRST_STILL_SPRITE` = `SPRITE_POKE_BALL` = `$3d` (`constants/sprite_constants.asm`): the sprite list is ordered and everything from there up is a four-tile still sprite — a ball, a fossil, a boulder, a Pokédex on a table, a sleeping Snorlax. `engine/overworld/map_sprites.asm:101` uses exactly this comparison to tell one from a walker. `Npc::person` is the predicate; it is an *appearance* test, which is all a picture id can carry. | ROM (Oak's lab reports three `$3d` sprites where `OaksLab.asm` declares `object_event 6, 3` / `7, 3` / `8, 3` with `SPRITE_POKE_BALL`), trace |

### Signs

| state | symbol | address | encoding | verified |
| --- | --- | ---: | --- | --- |
| sign count | `wNumSigns` | `$d4b0` | capped at `MAX_BG_EVENTS` = 16 | trace |
| sign table | `wSignCoords` | `$d4b1` | two bytes each, `Y, X`. `MACRO bg_event x, y, text` emits `db \2, \1, \3`, so **Y comes first** and — unlike `object_event` — there is **no +4 bias**: `home/overworld.asm`'s loader copies the pair straight across, and `IsSpriteOrSignInFrontOfPlayer` compares them against what `GetTileAndCoordsInFrontOfPlayer` returns, which is raw tile coordinates. A sign is therefore in the same space as `wXCoord` / `wYCoord`, exactly as the warp table is. | trace |
| sign text ids | `wSignTextIDs` | `$d4d1` | one byte each, parallel to the coordinates; a sign's identity on this map and nothing more | trace |

Signs are `bg_event`s: signposts, bookshelves, televisions, notice boards, the map on a gym wall.
They are not sprites and their tiles are not walkable, so nothing about them reaches `npcs()` or
`walkable()`, and `GO ITEM` is the only thing that reads them. Most indoor maps and every route
have none — Oak's lab has none either, which is why the `GO ITEM` ROM test there is about the
balls.

### Warps and connections

| state | symbol | address | encoding | verified |
| --- | --- | ---: | --- | --- |
| warp count | `wNumberOfWarps` | `$d3ae` | capped at `MAX_WARP_EVENTS` = 32 | ROM, trace |
| warp table | `wWarpEntries` | `$d3af` | four bytes each, `Y, X, destination warp id, destination map id` (`ram/wram.asm:1829`'s own comment). `MACRO warp_event x, y, map, warp` emits `db \2, \1, \4 - 1, \3`, so **Y comes first** and the destination warp id is stored **zero-based**: Red's bedroom declares `warp_event 7, 1, REDS_HOUSE_1F, 3` and the bytes read 1, 7, 2, 37. A destination map of `$ff` is `LAST_MAP`, "back the way you came", which is what both of the ground floor's doormats carry. | ROM (all three warps of Red's ground floor and the bedroom's one), trace |
| connections | `wCurMapConnections` | `$d370` | bitmask, `shift_const EAST, WEST, SOUTH, NORTH` = 1, 2, 4, 8 (`constants/map_data_constants.asm`) | ROM (Pallet Town is north and south only; an indoor map has none), trace |

### The walkable predicate and its window

```rust
pub fn walkable(memory: &mut dyn MemoryReader, x: u8, y: u8) -> Walkable;  // Yes | No | Unknown
```

It mirrors `CheckTilePassable` (`home/overworld.asm:1259`): take the tile id and look it up in the
current tileset's list of passable tiles, walking it until it matches or hits `$ff`.

- **the tile id** comes out of `wTileMap` at the offset `_GetTileAndCoordsInFrontOfPlayer`
  (`engine/overworld/player_state.asm:260`) would use. That routine reads screen (8, 9) for the
  tile the player stands on and (8, 11), (8, 7), (6, 9), (10, 9) for its four neighbours, which
  pins the mapping exactly: one map tile is two screen tiles each way and the player is always at
  (8, 9). `wOverworldMap` carries three blocks of border around the real map so the view can centre
  even on a map smaller than the screen, which is what makes "always" true.
- **the list** is at `wTilesetCollisionPtr` (`$d530`), a little-endian pointer. Every `*_Coll`
  label at this commit resolves inside `00:172f`..`00:17f0` — ROM **bank 0**, which is always
  mapped — so a `MemoryReader` over the CPU bus can follow it. A pointer outside `$0100`..`$4000`
  is not followed and the answer is `Unknown`, because banks 1 and up are whatever the last bank
  switch left mapped. This is the only ROM read in the module and the reason it is allowed.

Three things bound it, all reported as `Unknown` rather than guessed:

1. **The window is ten tiles by nine and it follows the player**: `x - 4 ..= x + 5` and
   `y - 4 ..= y + 4`. A house is answerable whole from the middle of it and only in part from a
   corner; a town or a route never is. An A\* over this has to treat `Unknown` as impassable and
   re-plan as it moves, which is what the per-step check in `macros.md` section 4 already requires.
2. **The screen buffer is the map only while the map is on screen.** In a battle or under a text
   box it holds the battle or the box, so both read as `Unknown`.
3. **`wTileMap` is the current view only on a running machine.** Read straight after
   `Emulator::import_state`, with no frame in between, it holds the view from wherever the state was
   taken and the predicate answers about the wrong tiles — measured: four tiles of Red's ground
   floor disagreed exactly that way. The sim loop is safe by construction (the adapter samples
   after a completed frame); anything that restores a snapshot must run a frame first, and
   `tests/rom_scene.rs` gives every restored state twenty.

What it does **not** model, and what the per-step "did the player move" check is for: NPCs standing
in the way (`npcs()` reports those separately, and the survey found exactly three presses blocked
that way, all of them into Mom), ledges, the tile-pair rules that stop a player walking between
certain tiles (only CAVERN and FOREST have any, `data/tilesets/pair_collision_tile_ids.asm`), and
warps that fire the instant they are stepped on.

## 3. The accessors, exactly

Agent B holds `trait GameState`; `PokeState` implements it over live WRAM. Every method takes
`&mut self` because reads are memoized per frame underneath (the emulator caches each address for
the current frame, and the adapter caches again per sample), and nothing here mutates the game. No
raw byte, address, mask or terminator crosses the trait.

```rust
// pokemon_red/macros/state.rs — self-contained: no imports, nothing from the rest of this work.

pub enum Scene {
    Title,
    Overworld,
    Dialog,
    Menu,
    Battle { own_turn: bool, forced_switch: bool },
    Shop,
    Pc,
    Unknown,
}
impl Scene {
    pub fn playable(self) -> bool;          // false only for Title
    pub fn label(self) -> &'static str;     // "OVERWORLD", "BATTLE TURN", … at most 14 characters
}

pub enum Facing { Down, Up, Left, Right }
impl Facing { pub fn delta(self) -> (i16, i16); }

pub struct Player   { pub map: u8, pub x: u8, pub y: u8, pub facing: Facing }
pub struct MapSize  { pub width: u8, pub height: u8 }

pub enum Status { Healthy, Sleep(u8), Poison, Burn, Freeze, Paralysis }
pub struct Move { pub id: u8, pub pp: u8, pub pp_up: u8 }
pub struct Mon {
    pub slot: u8,
    pub species: u8,            // internal index, not the Pokédex number
    pub level: u8,
    pub hp: u16,
    pub max_hp: u16,
    pub status: Status,
    pub moves: [Option<Move>; 4],
}
impl Mon { pub fn fainted(self) -> bool; pub fn hp_fraction(self) -> f64; }

pub struct Party { pub mons: Vec<Mon>, pub active: Option<u8> }
impl Party {
    pub fn healthiest_reserve(&self) -> Option<&Mon>;   // macros.md section 3's "healthiest"
    pub fn active_mon(&self) -> Option<&Mon>;
}

pub struct EnemyMon { pub species: u8, pub level: u8, pub hp: u16, pub max_hp: u16 }
pub enum BattleKind { Wild, Trainer }
pub enum BattleMenu {
    None,
    Main { cursor: u8 },                        // 0 FIGHT, 1 PKMN, 2 ITEM, 3 RUN
    Moves { cursor: Option<u8>, count: u8 },    // 0-based move slot
    Party { cursor: u8 },                       // 0-based party slot
}
pub struct Battle {
    pub kind: BattleKind,
    pub own_turn: bool,
    pub forced_switch: bool,
    pub menu: BattleMenu,
    pub own: Option<Mon>,
    pub enemy: Option<EnemyMon>,
}

pub struct TextBox { pub open: bool, pub waiting: bool }
pub struct Cursor {
    pub current: u8, pub max: u8, pub top_y: u8, pub top_x: u8, pub watched_keys: u8,
}
impl Cursor { pub fn cancellable(self) -> bool; }   // whether B closes this menu

pub struct StartMenu { pub cursor: Cursor, pub items: u8 }
pub enum ShopScreen { BuySellQuit, Buying, Selling }
pub struct Shop { pub screen: ShopScreen, pub cursor: Cursor }
pub struct Pc { pub cursor: Cursor }
pub struct BagItem { pub id: u8, pub count: u8 }
pub struct Npc { pub slot: u8, pub picture: u8, pub x: u8, pub y: u8, pub facing: Facing }
impl Npc { pub fn person(self) -> bool; }          // picture < FIRST_STILL_SPRITE
pub struct Sign { pub x: u8, pub y: u8, pub text_id: u8 }
pub enum Walkable { Yes, No, Unknown }
impl Walkable { pub fn is_walkable(self) -> bool; }  // Yes only
pub struct Warp { pub x: u8, pub y: u8, pub destination_warp: u8, pub destination_map: u8 }
pub struct Connections { pub north: bool, pub south: bool, pub east: bool, pub west: bool }
impl Connections { pub fn any(self) -> bool; }

pub trait GameState {
    fn scene(&mut self) -> Scene;
    fn player(&mut self) -> Option<Player>;
    fn map_size(&mut self) -> Option<MapSize>;
    fn party(&mut self) -> Party;
    fn battle(&mut self) -> Option<Battle>;
    fn text_box(&mut self) -> TextBox;
    fn start_menu(&mut self) -> Option<StartMenu>;
    fn shop(&mut self) -> Option<Shop>;
    fn pc(&mut self) -> Option<Pc>;
    fn money(&mut self) -> u32;
    fn bag(&mut self) -> Vec<BagItem>;
    fn npcs(&mut self) -> Vec<Npc>;         // people and objects alike
    fn signs(&mut self) -> Vec<Sign>;
    fn walkable(&mut self, x: u8, y: u8) -> Walkable;
    fn warps(&mut self) -> Vec<Warp>;
    fn connections(&mut self) -> Connections;
}
```

The same rules as free functions, for callers that already hold a reader — `detect` uses these, so
there is one implementation of each rule and not two:

```rust
// pokemon_red/state.rs
pub struct PokeState<'a>;
impl<'a> PokeState<'a> {
    pub fn new(memory: &'a mut dyn MemoryReader) -> Self;                       // empty ledger
    pub fn with_exits(memory: &'a mut dyn MemoryReader, exits: &'a dyn ExitLedger) -> Self;
}
impl GameState for PokeState<'_> { … }

pub fn started(memory: &mut dyn MemoryReader) -> bool;         // the game has begun at all
pub fn controllable(memory: &mut dyn MemoryReader) -> bool;    // the buttons reach the player
pub fn map_size(memory: &mut dyn MemoryReader) -> Option<MapSize>;
pub fn player(memory: &mut dyn MemoryReader) -> Option<Player>;
pub fn party(memory: &mut dyn MemoryReader) -> Party;
pub fn cursor(memory: &mut dyn MemoryReader) -> Cursor;
pub fn battle(memory: &mut dyn MemoryReader) -> Option<Battle>;
pub fn text_box(memory: &mut dyn MemoryReader) -> TextBox;
pub fn start_menu(memory: &mut dyn MemoryReader) -> Option<StartMenu>;
pub fn submenu(memory: &mut dyn MemoryReader) -> bool;
pub fn shop(memory: &mut dyn MemoryReader) -> Option<Shop>;
pub fn pc(memory: &mut dyn MemoryReader) -> Option<Pc>;
pub fn money(memory: &mut dyn MemoryReader) -> u32;
pub fn bag(memory: &mut dyn MemoryReader) -> Vec<BagItem>;
pub fn npcs(memory: &mut dyn MemoryReader) -> Vec<Npc>;
pub fn signs(memory: &mut dyn MemoryReader) -> Vec<Sign>;
pub fn walkable(memory: &mut dyn MemoryReader, x: u8, y: u8) -> Walkable;
pub fn warps(memory: &mut dyn MemoryReader) -> Vec<Warp>;
pub fn connections(memory: &mut dyn MemoryReader) -> Connections;

pub mod poke { … }   // the disassembly's own constants: pad bits, box tiles, template ids, masks
```

### The exploration ledger (2026-09-16)

`MacroState::exit_visited` is no longer on its default. `PokeState::with_exits` carries a
`crate::macros::ExitLedger` — one question, `exit_visited(MapExit) -> bool`, asked across
`crate::adapter`'s seam — and `GameAdapter::exit_visited` answers it from the `boundary` keys the
reward rule already writes into its lifetime `seen` set:

| the palette asks | the adapter looks up |
| --- | --- |
| `ExitId::Warp(i)` | `boundary:<map>:<y>:<x>:on` or `:near`, with `<x>`/`<y>` from `warps()[i]` |
| `ExitId::Edge(e)` | `boundary:<map>:edge:<n\|s\|e\|w>:on` or `:near` |

Three things about it are deliberate. It is a **read**: `&self`, no payout, no ledger write, so
`REWARD_ADAPTER` and the compatibility string do not move. A warp is named by its **tile** on the
adapter's side and by its **index** on the palette's, because a warp index is only stable while
the map is loaded and the ledger is lifetime state. And "visited" means **either half** of the
key: the `:on` half is what an interior warp records on arrival, but a town door fires on the step
*onto* it, so the adapter never samples a stable frame there and `:on` alone would call every
house door in the game unvisited forever. Either half means "this run has been paid for finding
this exit", which is the question `GO EXIT` is actually asking.

`pokemon_red/state.rs` deliberately does **not** reach the cartridge's ROM tables: the type chart,
base powers and item prices are in banks 1 and up, where a `MemoryReader` reads whatever bank the
last switch left mapped. So `macros.md` section 3's "best damaging move, type effectiveness applied
from the ROM's type chart" is agent B's, and it needs a source this module cannot be. What B has
from here is the move ids, their PP, the enemy's species and level, and the party.

## 4. Tests

`cargo test -p flybrain-gb` — 146 lib tests in the crate against 98 before this work, so 48 new,
and none of them needs a cartridge:

- **every scene from a synthetic trace** (`scene/tests.rs`, 22 tests), including the four the brief
  names: a battle never reads as overworld even with the dialogue box drawn over it, a dialog never
  reads as overworld, an unreadable frame reads as `Unknown`, and standing on a doormat is still
  the overworld. Plus `every_scene_the_enum_declares_has_a_synthetic_trace`, which is one assertion
  over all nine.
- **every accessor encoding** (`state/tests.rs`, 26 tests): big-endian HP, every status byte,
  packed PP, BCD money and a nibble that is not BCD, the bag's terminator winning over its count,
  sprite coordinates biased by four, the one-based move list, the battle menu's four positions,
  the party list with and without a way out, `MAX_WARP_EVENTS` bounding the table, each connection
  bit, the window edges of the walkable predicate, and a collision pointer outside bank 0.
- `the_live_implementation_answers_the_whole_trait` walks every method of `GameState` through
  `PokeState`.

`FLY_ROM=… cargo test --release -p flybrain-gb --test rom_scene` — four ROM-gated tests, nine
seconds, skipped cleanly without the variable. There are no archived save states in the repository
(`.gitignore` excludes `*.state` with the ROM), so they produce their own the way
`examples/room_escape.rs` does: boot once, then drive stage by stage with a fixed-seed walk, each
stage ending on a condition. Observed frames, for reproducibility:

| state | how | frame |
| --- | --- | ---: |
| title | 600 idle frames past the boot logo, then 3,000 more | 600 |
| bedroom | Start and A through the intro, then B until the adapter reports a settled overworld sample | 3,845 |
| Red's ground floor | uniform random walk | 5,233 |
| Pallet Town | uniform random walk | 9,593 |
| a dialogue in Oak's lab | Oak's script, triggered by reaching `wYCoord == 1` in Pallet Town | 12,825 |
| the starter | A on a ball and A on YES; species written at | 18,148 |
| a trainer battle | the rival challenges the player where he stands | 19,468 |
| the battle menu | B advances the opening text; A would overshoot into the move list | 20,100 |

What each one asserts:

- **title** — 3,000 idle frames are `Title` and nothing else, the adapter says `BOOT`, and no
  player, party or walkable tile is readable.
- **bedroom, ground floor, Pallet Town** — `Overworld` at each, the map sizes, the bedroom's one
  warp with its zero-based destination warp id, an empty party, a 3,000 wallet, an empty bag,
  Pallet Town's north and south connections, and `Unknown` for a tile across the town.
- **the walkable predicate against a survey** — the survey reproduces
  `docs/design/room-escape.md` section 3 exactly: 48 reachable tiles and the same six presses that
  leave Red's ground floor. Then the predicate is compared against it from all 48 tiles and all
  four directions — 171 comparisons, no disagreements — plus the three presses the survey cannot
  make because Mom is standing on a passable tile, which `npcs()` reports at exactly
  `object_event 5, 4`. This is the assertion that catches a wrong screen origin or a wrong stride,
  because both of those still answer, and answer plausibly.
- **Oak's lab** — the dialogue is a `Dialog` with an open, waiting box over a still-loaded map
  whose tiles read `Unknown`; the starter is one of the three internal species ids at level 5 with
  full HP and a level-1 learnset; the battle is a `Trainer` battle whose combatants are not written
  on its first frame, then a `Main { cursor: 0 }` menu that RIGHT moves to ITEM and DOWN to RUN.

Two facts about the emulator came out of building these and are worth keeping:

- a state exported while a button was held does not respond to that button, or any other, being
  held again after the import — measured, all four directions, 64 frames each, no movement. Every
  state these tests keep is exported after twenty released frames.
- a press the player is not already facing turns it first and steps second, and the pair took 53
  frames from the ground floor's staircase, so `examples/room_escape.rs`'s 48-frame window would
  read that as a wall. The survey here holds for 120.

## 5. What could not be verified

- **The mart and the PC have synthetic traces only.** Neither is reachable from a fresh cartridge
  inside a test's worth of frames — Viridian City's mart is rungs away — so `Shop` and `Pc` are
  built from the disassembly and checked against traces. `BUY_SELL_QUIT_MENU` having exactly one
  user in the game is the strongest thing said about the mart, and it is a grep, not a run.
- **`Menu` is verified for the start menu and three submenus**, and those three are a floor, not a
  ceiling: the Pokédex, the trainer card, OPTION and the naming screens read as `Unknown`. That is
  safe (advance only) and it is not complete.
- **A forced switch has no ROM test.** It needs a fainted Pokémon with a second one in the party,
  which is two battles away from anything a fixed-seed walk reaches quickly. The trace is built
  from `ChooseNextMon` and `PartyMenuInit`, and the distinguishing byte
  (`wPartyMenuTypeOrMessageID` = `BATTLE_PARTY_MENU`) is asserted both ways.
- **`text_box().waiting` is "the dialogue box is drawn", not "the game wants a button".** Pokered
  has no flag for the second thing; A is the right press either way, so the distinction has no
  consequence for the palette, but it is not what the field's name might suggest.
- **The walkable predicate is tile-only**, with the three bounds above. Ledges, tile pairs, and
  warps that fire on the step onto them are not modelled at all.
- **Nothing here is verified against a second cartridge revision.** The adapter is pinned to one
  SHA-256 and so is this.

## 6. For agent B

- Copy `pokemon_red/macros/state.rs` verbatim; it compiles alone. `PokeState::new(memory)` is the
  implementation.
- `Scene::Unknown` and `Scene::Dialog` share a palette (`macros.md` section 2), and `Unknown` is
  an ordinary reading, not an error: every menu the detector does not name, the frames a warp
  passes through, and the `$ff` frame a lost battle passes through all land there.
- Battle and menu macros navigate by cursor, never by counting presses (`macros.md` section 4).
  The cursors here are already translated out of pokered's own indexing: `Main` is 0 FIGHT, 1 PKMN,
  2 ITEM, 3 RUN with LEFT and RIGHT changing column, and `Moves` is 0-based.
- A macro that reads the party immediately after a gift or a catch can see a member with species 0:
  `wPartyCount` leads the struct. Require a non-zero species.
- `walkable` answers about a ten-by-nine window that moves with the player, and `Unknown` is not
  `No`. An A\* has to re-plan as it walks, and the per-step movement check is not optional.

## 7. Shops, Pokémon Centers and the counter (2026-09-17, `docs/design/macros.md` section 13)

Two more names through `services/flysim/tools/gen_symbols.py`, taking the table from 61 addresses
to **63**, regenerated the same way and again with nothing else moved — no event flag, no
milestone, no existing address. `flysim --print-compatibility` is byte-identical across the
change, 648 bytes, checked on the WSL box.

| state | symbol | address | encoding | verified |
| --- | --- | ---: | --- | --- |
| the open mart's stock | `wItemList` | `$cf7b` | `ds 16`. `LoadItemList` (`home/text_script.asm:156`) copies the clerk's `script_mart` list out of its text script the moment the counter opens: **a count byte** (the macro's `_NARG`), then the item ids, then `$ff`. `DisplayPokemartDialogue_` points the buy list's `wListPointer` at the same buffer for `PRICEDITEMLISTMENU`, so **an item's position in this list is its cursor index** in the buy menu — which is what lets a purchase be navigated by reading the cursor rather than by counting presses. The terminator wins over the count, as it does for the bag, and the sixteen-byte buffer bounds both. | ROM (Viridian's counter reads POKE BALL, ANTIDOTE, PARLYZ HEAL, BURN HEAL, in that order, and no Potion), trace |
| the tileset's counter tiles | `wTilesetTalkingOverTiles` | `$d532` | three tile ids from the tileset header (`data/tilesets/tileset_headers.asm`), `$ff` for a tileset with fewer. Mart and Pokecenter are both `$18 $19 $1e`; the overworld and an ordinary house have none. `IsSpriteOrSignInFrontOfPlayer`'s `.extendRangeOverCounter` branch (`home/overworld.asm:1115`) walks exactly this list and doubles the talking range from `$10` to `$20` pixels — one tile to two — when the tile in front of the player is one of them. | ROM (the Viridian mart's column 1 and the centre's (3, 2) read as counters and the floor either side does not), trace |

**Why the second one is load-bearing.** A mart clerk is `object_event 0, 5, SPRITE_CLERK` behind a
counter running down column 1; a Pokémon Center nurse is `object_event 3, 1, SPRITE_NURSE` behind
the counter tile at (3, 2). **None of the four tiles around either of them is standable** — they are
wall, or floor behind the desk. An approach that only knew how to stand beside somebody could
never reach a counter at all, and `GO SHOP` would refuse `no route` once per hold for ever. The
counter rule is what makes the tile *two* away, facing in, a place to talk from.

New accessors in `pokemon_red/state.rs`, all three over the same reader:

```rust
pub fn shop_stock(memory: &mut dyn MemoryReader) -> Vec<u8>;
pub fn counter_tiles(memory: &mut dyn MemoryReader) -> [u8; 3];
pub fn map_tile_id(memory: &mut dyn MemoryReader, x: u8, y: u8) -> Option<u8>;
pub fn counter_tile(memory: &mut dyn MemoryReader, x: u8, y: u8) -> bool;
```

`map_tile_id` is `walkable`'s own first half, lifted out so that "is this tile passable" and "is
this tile a counter" read one byte by one rule and not two: the same ten-by-nine window, the same
screen origin, and the same three bounds — off the map, off the window, or a screen holding a
battle or a text box instead of the map.

`PokeState`'s `shop_stock` is **gated on the mart scene being up**, and that gate is the whole of
its accuracy: `wItemList` is a scratch buffer that nobody clears, so off the counter it holds
whatever the last list was — a previous mart's stock, or a `MonsterNames` list from a battle.
Answering `[]` everywhere else is the narrowing the rest of this seam is made of.

### The errand ledger and the area

`MacroState::area_visited(kind, area)` is section 13's `areaVisited`: session state in the executor
layer beside the talked, blocked, reached and stood ledgers, never checkpointed, written by
`PokemonPalette::observe` on the frame the fly is seen standing on a mart's or a centre's own map.
"Which area" is `macros::geography::area_of`: outdoors a map is its own area, indoors it is the
outdoor map the front door opens onto, so standing in a Viridian house, in the mart or in the gym
are all "in Viridian". A building the graph has no row for has no area, and therefore no errand.

The mart and centre **map ids** are in `pokemon_red/maps.rs`, each counted out between two ids that
module already pinned, which is the only way one of them could be wrong without a test noticing:
`VIRIDIAN_POKECENTER` `$29` immediately precedes `VIRIDIAN_MART` `$2a`; `PEWTER_MART` `$38` and
`PEWTER_POKECENTER` `$3a` lie in the five ids between `PEWTER_GYM` `$36` and `MT_MOON_1F` `$3b`;
`CERULEAN_POKECENTER` `$40` and `CERULEAN_MART` `$43` lie around `CERULEAN_GYM` `$41`.

### 7.1 The in-battle bag list (2026-09-17, `docs/design/macros.md` section 14)

`BattleMenu` gains a fourth member, and it closes a gap rather than adding a feature.

| menu | signature | cursor | verified |
| --- | --- | --- | --- |
| the bag, in a battle | `wIsInBattle` non-zero, none of the three geometries above, and `wListMenuID` = `ITEMLISTMENU` (`$03`) | `wCurrentMenuItem`, with the bag's own entry count as the bound | trace |

It is **not** the fly's own turn: the bag is a list the fly opened *during* its turn, and the pad
that belongs to it is the two answers any list has, which is what the between-turns row deals
(`NEXT`, `BACK`, section 13.1). Reporting it as the own turn would put the four move buttons on a
screen they cannot press.

Before this the bag read as `BattleMenu::None`, so `palette::listing` answered `None` for it, and
`ITEM` could open the bag and then had nothing to read: it waited out `CURSOR_WAIT` — 180 frames,
pressing nothing, which is the correct behaviour for a list with no cursor — and reported
`Blocked`. Every potion the fly ever chose ended that way. `THROW BALL` needs the same list.

## 8. What section 14 removed

The move table's base power and type, the ROM's type chart and the opposing Pokémon's types are
**gone from the seam**, not defaulted. They existed for `ATTACK`'s "highest base power move with
PP, type effectiveness applied from the ROM's type chart", and section 14 replaced `ATTACK` with
one button per move slot: which move is used is the fly's choice and the mushroom body's to learn.
Knowledge that nothing reads is not narrowed, it is deleted — `MacroState` is four methods
shorter and `pokemon_red/state.rs` never needed them.
