//! Typed accessors over Pokémon Red's WRAM, for the macro palette.
//!
//! `docs/design/macros.md` section 8 is the contract and `docs/design/macros-wram.md` is the
//! table: every symbol, its address at the pinned pokered commit, its encoding, and how it was
//! verified. This module turns those bytes into the types in
//! [`crate::pokemon_red::macros::state`], and nothing else: it decides nothing, presses nothing
//! and caches nothing beyond the reader it was handed.
//!
//! Every accessor exists twice on purpose. The free functions take a `&mut dyn MemoryReader`, so
//! [`crate::pokemon_red::scene::detect`] and the tests can call one without building anything;
//! [`PokeState`] wraps a reader and implements [`GameState`] on top of the same functions, which
//! is what agent B's executor holds. There is one implementation of each rule.
//!
//! ## What is *not* here
//!
//! Anything that needs the cartridge's ROM tables: the type chart, base powers, item prices,
//! species names. "Best damaging move with type effectiveness" (`macros.md` section 3) is the
//! executor's, and it needs data this module cannot reach — a `MemoryReader` reads the CPU bus,
//! where ROM banks 1 and up are whatever the last bank switch left mapped. The one ROM read this
//! module does make is safe for exactly that reason: the tileset collision lists all live in bank
//! 0, which is always mapped. See "The walkable predicate and its window" in
//! `docs/design/macros-wram.md`.

use crate::adapter::{MapEdge, MapExit, MapTile, MemoryReader};
use crate::macros::{RunLedger, NoLedger};

use super::macros::cartridge::{
    AreaLedger, Edge, ExitId, MacroState, NoAreas, NoPushed, NoStood, NoTalk, NoTargets,
    Objective, PushedLedger, StoodLedger, TalkLedger, TalkTarget, TargetKey, TargetLedger, Tile,
};
use super::macros::geography::Amenity;
use super::mapgrid::{self, MapGrids};
use super::macros::state::{
    BagItem, Battle, BattleKind, BattleMenu, Connections, Cursor, EnemyMon, Facing, GameState,
    MapGrid, MapSize, Mon, Move, Npc, Party, Pc, Player, Scene, Shop, ShopScreen, Sign, StartMenu,
    Status, TextBox, Walkable, Warp,
};
use super::symbols::ram;

/// Constants from the disassembly at [`super::symbols::POKERED_COMMIT`]. Values, not addresses:
/// `gen_symbols.py` owns the addresses and refuses to be hand-edited, but these are `EQU`s and
/// `const`s that no symbol table carries, so each one names its file.
pub mod poke {
    /// `constants/hardware.inc`.
    pub mod pad {
        pub const A: u8 = 1 << 0;
        pub const B: u8 = 1 << 1;
        pub const SELECT: u8 = 1 << 2;
        pub const START: u8 = 1 << 3;
        pub const RIGHT: u8 = 1 << 4;
        pub const LEFT: u8 = 1 << 5;
        pub const UP: u8 = 1 << 6;
        pub const DOWN: u8 = 1 << 7;
    }

    /// `constants/charmap.asm`: the text box frame, which is how a drawn box is recognised.
    pub mod frame {
        pub const TOP_LEFT: u8 = 0x79;
        pub const HORIZONTAL: u8 = 0x7a;
        pub const TOP_RIGHT: u8 = 0x7b;
        pub const VERTICAL: u8 = 0x7c;
        pub const BOTTOM_LEFT: u8 = 0x7d;
        pub const BOTTOM_RIGHT: u8 = 0x7e;
    }

    /// `constants/menu_constants.asm` text box ids.
    pub const BATTLE_MENU_TEMPLATE: u8 = 0x0b;
    pub const BUY_SELL_QUIT_MENU: u8 = 0x15;

    /// `constants/list_constants.asm` list menu ids.
    pub const PRICED_ITEM_LIST_MENU: u8 = 0x02;
    pub const ITEM_LIST_MENU: u8 = 0x03;
    pub const SPECIAL_LIST_MENU: u8 = 0x04;

    /// `constants/menu_constants.asm` party menu types.
    pub const BATTLE_PARTY_MENU: u8 = 0x02;

    /// The two-option YES/NO box, as surveyed on the cartridge (`infra/docs/macros-traps.md`,
    /// row 41): the border `DisplayTwoOptionMenu` draws, and where it parks the cursor.
    ///
    /// Values rather than a symbol because `wTwoOptionMenuID` is not in the reviewed address list
    /// and the box's geometry is what is on screen. Read from the rung-10 Pokemon Center
    /// checkpoint, one raw A pulse at a time: a box at (11, 6)-(19, 11) with the cursor at
    /// row 8, column 12, one item below the first, watching A and B.
    pub const YES_NO_BOX: (u16, u16, u16, u16) = (11, 6, 19, 11);
    pub const YES_NO_CURSOR_Y: u8 = 8;
    pub const YES_NO_CURSOR_X: u8 = 12;

    /// `constants/ram_constants.asm`: `wMiscFlags` bit 3.
    pub const BIT_USING_GENERIC_PC: u8 = 1 << 3;
    /// `wFontLoaded` bit 0.
    pub const BIT_FONT_LOADED: u8 = 1 << 0;
    /// `wStatusFlags6` bit 0, set once when the game starts and never cleared.
    pub const BIT_GAME_TIMER_COUNTING: u8 = 1 << 0;

    /// `wStatusFlags5` bits 0, 5 and 7: scripted NPC movement, joypad disabled, scripted movement.
    pub const SCRIPTED_STATUS5: u8 = 0xa1;
    /// `wStatusFlags6` bits 2, 3, 4 and 6: fly, dungeon and escape warps in flight.
    pub const SCRIPTED_STATUS6: u8 = 0x5c;
    /// `wMovementFlags` bits 6 and 7: a ledge hop and a spin tile. The door bits (0, 1, 2) are
    /// deliberately *not* here: standing on a doormat is an ordinary overworld state, and it is the
    /// one `docs/design/room-escape.md` cares most about.
    pub const SCRIPTED_MOVEMENT: u8 = 0xc0;

    /// `constants/battle_constants.asm`: the non-volatile status byte.
    pub const SLP_MASK: u8 = 0b111;
    pub const PSN: u8 = 3;
    pub const BRN: u8 = 4;
    pub const FRZ: u8 = 5;
    pub const PAR: u8 = 6;

    /// `constants/pokemon_data_constants.asm`: `PARTYMON_STRUCT_LENGTH`.
    pub const PARTY_MON_BYTES: u16 = 0x2c;
    /// `constants/menu_constants.asm`: `BAG_ITEM_CAPACITY`.
    pub const BAG_CAPACITY: u8 = 20;
    /// `ram/wram.asm`: `wItemList:: ds 16`, which bounds the open mart's inventory.
    ///
    /// One count byte, then the ids, then `$ff`, so at most fourteen items can be both counted
    /// and terminated inside the buffer. The real marts carry four to nine.
    pub const MART_LIST_BYTES: u8 = 16;
    /// `data/tilesets/tileset_headers.asm`: three counter tile ids per tileset, `-1` for none.
    pub const COUNTER_TILES: u16 = 3;
    /// The `-1` a tileset with fewer than three counter tiles pads its header with.
    pub const NO_COUNTER_TILE: u8 = 0xff;
    /// `constants/sprite_constants.asm`: the two people who stand behind a counter.
    ///
    /// Picture ids, which is what `wSpriteStateData1`'s byte 0 holds -- the same numbering
    /// `FIRST_STILL_SPRITE` is compared against, so a clerk and a nurse are identifiable from the
    /// sprite table alone and nothing has to guess which person a shop's person is.
    pub const SPRITE_CLERK: u8 = 0x26;
    pub const SPRITE_NURSE: u8 = 0x29;
    /// `constants/map_data_constants.asm`: `MAX_WARP_EVENTS`.
    pub const MAX_WARPS: u8 = 32;
    /// `constants/map_data_constants.asm`: `MAX_BG_EVENTS`, which bounds the sign table.
    pub const MAX_SIGNS: u8 = 16;
    /// `constants/map_object_constants.asm`: `NUM_SPRITESTATEDATA_STRUCTS` and the struct length.
    pub const SPRITE_SLOTS: u8 = 16;
    pub const SPRITE_BYTES: u16 = 16;
    /// `MACRO object_event` stores map coordinates plus four.
    pub const SPRITE_COORD_BIAS: u8 = 4;

    /// `constants/map_data_constants.asm`: `wCurMapConnections` bits.
    pub const CONNECTION_EAST: u8 = 1;
    pub const CONNECTION_WEST: u8 = 2;
    pub const CONNECTION_SOUTH: u8 = 4;
    pub const CONNECTION_NORTH: u8 = 8;

    /// The highest real map id (`constants/map_constants.asm` ends at `$f7`), which is the same
    /// bound the reward adapter uses to reject a half-loaded frame.
    pub const MAX_MAP_ID: u8 = 0xf7;

    /// The screen's tile buffer is `SCREEN_WIDTH` x `SCREEN_HEIGHT`
    /// (`constants/gfx_constants.asm`).
    pub const SCREEN_WIDTH: u16 = 20;
    pub const SCREEN_HEIGHT: u16 = 18;

    /// Where the player's own tile sits in that buffer. `_GetTileAndCoordsInFrontOfPlayer` reads
    /// `(8, 9)` for the tile it stands on and `(8, 11)`, `(8, 7)`, `(6, 9)`, `(10, 9)` for the
    /// four neighbours, so one map tile is two screen tiles in each axis and the player is at
    /// this fixed point.
    pub const PLAYER_SCREEN_X: i32 = 8;
    pub const PLAYER_SCREEN_Y: i32 = 9;
}

fn read(memory: &mut dyn MemoryReader, address: u16) -> u8 {
    memory.read8(address)
}

/// A big-endian 16-bit quantity, which is how the cartridge stores HP.
fn word_be(memory: &mut dyn MemoryReader, address: u16) -> u16 {
    u16::from(read(memory, address)) * 256 + u16::from(read(memory, address + 1))
}

/// One byte of the screen's tile buffer.
fn screen_tile(memory: &mut dyn MemoryReader, x: u16, y: u16) -> u8 {
    if x >= poke::SCREEN_WIDTH || y >= poke::SCREEN_HEIGHT {
        return 0;
    }
    read(memory, ram::wTileMap + y * poke::SCREEN_WIDTH + x)
}

/// Whether the *whole* `TextBoxBorder` is drawn: the four corners, the horizontal runs along the
/// top and bottom rows, and the vertical runs down both sides.
///
/// `TextBoxBorder` (`home/text_box.asm`) draws exactly this — `$79` then `$7a` x width then `$7b`,
/// `$7c` down each side, `$7d` then `$7a` x width then `$7e` — so a real box satisfies all of it
/// and four map tiles that happen to hold frame ids satisfy only [`box_drawn`]. The corners are
/// 4 bytes out of a 2·(width+height) byte figure; this is the rest of it, and it is what tells a
/// text box from a map.
fn border_drawn(
    memory: &mut dyn MemoryReader,
    left: u16,
    top: u16,
    right: u16,
    bottom: u16,
) -> bool {
    if !box_drawn(memory, left, top, right, bottom) {
        return false;
    }
    for x in (left + 1)..right {
        if screen_tile(memory, x, top) != poke::frame::HORIZONTAL
            || screen_tile(memory, x, bottom) != poke::frame::HORIZONTAL
        {
            return false;
        }
    }
    for y in (top + 1)..bottom {
        if screen_tile(memory, left, y) != poke::frame::VERTICAL
            || screen_tile(memory, right, y) != poke::frame::VERTICAL
        {
            return false;
        }
    }
    true
}

/// Whether a `TextBoxBorder` box is drawn with these corners. Four corners rather than one,
/// because a map tile can legitimately hold the frame's tile id and two of them cannot.
fn box_drawn(memory: &mut dyn MemoryReader, left: u16, top: u16, right: u16, bottom: u16) -> bool {
    screen_tile(memory, left, top) == poke::frame::TOP_LEFT
        && screen_tile(memory, right, top) == poke::frame::TOP_RIGHT
        && screen_tile(memory, left, bottom) == poke::frame::BOTTOM_LEFT
        && screen_tile(memory, right, bottom) == poke::frame::BOTTOM_RIGHT
}

/// Whether the game has started at all: `wStatusFlags6`'s game-timer bit, which `MainMenu` sets
/// for a new game and for a continue and nothing ever clears. False on the title screen, through
/// the intro and on the naming screens.
///
/// This is the same gate the reward adapter calls `active`, so [`Scene::Title`] and the adapter's
/// `BOOT` mode agree by construction.
pub fn started(memory: &mut dyn MemoryReader) -> bool {
    read(memory, ram::wStatusFlags6) & poke::BIT_GAME_TIMER_COUNTING != 0
}

/// Whether the player's buttons reach the player: no ignored joypad, no simulated input, no
/// scripted movement, no warp in flight, not mid-ledge-hop.
///
/// The masks are the reward adapter's own scripted gate, minus the door bits — see
/// [`poke::SCRIPTED_MOVEMENT`].
pub fn controllable(memory: &mut dyn MemoryReader) -> bool {
    read(memory, ram::wJoyIgnore) == 0
        && read(memory, ram::wSimulatedJoypadStatesIndex) == 0
        && read(memory, ram::wStatusFlags5) & poke::SCRIPTED_STATUS5 == 0
        && read(memory, ram::wStatusFlags6) & poke::SCRIPTED_STATUS6 == 0
        && read(memory, ram::wMovementFlags) & poke::SCRIPTED_MOVEMENT == 0
}

/// The current map's size in walkable tiles, or `None` on a frame whose map header is not loaded.
///
/// `wCurMapWidth` and `wCurMapHeight` are in blocks and one block is two tiles each way, which is
/// the conversion the reward adapter already makes.
pub fn map_size(memory: &mut dyn MemoryReader) -> Option<MapSize> {
    if read(memory, ram::wCurMap) > poke::MAX_MAP_ID {
        return None;
    }
    let width = u16::from(read(memory, ram::wCurMapWidth)) * 2;
    let height = u16::from(read(memory, ram::wCurMapHeight)) * 2;
    if width == 0 || height == 0 || width > 255 || height > 255 {
        return None;
    }
    Some(MapSize { width: width as u8, height: height as u8 })
}

fn facing_from(byte: u8) -> Facing {
    // `constants/sprite_data_constants.asm`: SPRITE_FACING_DOWN/UP/LEFT/RIGHT.
    match byte & 0x0c {
        0x04 => Facing::Up,
        0x08 => Facing::Left,
        0x0c => Facing::Right,
        _ => Facing::Down,
    }
}

/// Where the player is standing, or `None` when the coordinates are not inside a loaded map.
pub fn player(memory: &mut dyn MemoryReader) -> Option<Player> {
    let size = map_size(memory)?;
    let map = read(memory, ram::wCurMap);
    let x = read(memory, ram::wXCoord);
    let y = read(memory, ram::wYCoord);
    if x >= size.width || y >= size.height {
        return None;
    }
    // Sprite slot 0 is the player; byte 9 of its wSpriteStateData1 struct is its facing.
    let facing = facing_from(read(memory, ram::wSpriteStateData1 + 9));
    Some(Player { map, x, y, facing })
}

fn status_from(byte: u8) -> Status {
    if byte & poke::SLP_MASK != 0 {
        return Status::Sleep(byte & poke::SLP_MASK);
    }
    if byte & (1 << poke::PSN) != 0 {
        return Status::Poison;
    }
    if byte & (1 << poke::BRN) != 0 {
        return Status::Burn;
    }
    if byte & (1 << poke::FRZ) != 0 {
        return Status::Freeze;
    }
    if byte & (1 << poke::PAR) != 0 {
        return Status::Paralysis;
    }
    Status::Healthy
}

/// One `party_struct`'s four move slots and their PP.
///
/// The PP byte packs remaining PP in bits 0 to 5 and the number of PP Ups in bits 6 and 7
/// (`data/moves/moves.asm` and `AddPP`), so neither can be read without the other.
fn moves_at(memory: &mut dyn MemoryReader, ids: u16, pps: u16) -> [Option<Move>; 4] {
    let mut moves = [None; 4];
    for (slot, move_slot) in moves.iter_mut().enumerate() {
        let id = read(memory, ids + slot as u16);
        if id == 0 {
            continue;
        }
        let pp = read(memory, pps + slot as u16);
        *move_slot = Some(Move { id, pp: pp & 0x3f, pp_up: pp >> 6 });
    }
    moves
}

/// The player's party, in slot order, empty before the starter or on an unreadable frame.
///
/// `active` is filled only during a battle, from `wPlayerMonNumber`.
///
/// `wPartyCount` leads the structs it counts: `AddPartyMon` writes the count and then fills the
/// 44 bytes over the following frames, so a member can read back with species 0 and no HP. That
/// is reported rather than hidden — nothing here can tell a half-written member from a real one
/// without inventing a rule — and both the trait's documentation and
/// `docs/design/macros-wram.md` say so.
pub fn party(memory: &mut dyn MemoryReader) -> Party {
    let count = read(memory, ram::wPartyCount);
    if count > 6 {
        return Party::default();
    }
    let mut mons = Vec::with_capacity(count as usize);
    for slot in 0..count {
        let base = ram::wPartyMon1 + u16::from(slot) * poke::PARTY_MON_BYTES;
        mons.push(Mon {
            slot,
            species: read(memory, base),
            level: read(memory, base + 33),
            hp: word_be(memory, base + 1),
            max_hp: word_be(memory, base + 34),
            status: status_from(read(memory, base + 4)),
            moves: moves_at(memory, base + 8, base + 29),
        });
    }
    let active = in_battle(memory)
        .is_some()
        .then(|| read(memory, ram::wPlayerMonNumber))
        .filter(|slot| *slot < count);
    Party { mons, active }
}

/// Which kind of battle is running, or `None`.
///
/// `wIsInBattle` is 0 outside a battle, 1 for a wild Pokémon, 2 for a trainer and `$ff` on the
/// frame a battle is lost. The Safari Zone and the old man's tutorial (`wBattleType`) have their
/// own menus, so they are not battles this module claims to understand.
fn in_battle(memory: &mut dyn MemoryReader) -> Option<BattleKind> {
    if read(memory, ram::wBattleType) != 0 {
        return None;
    }
    match read(memory, ram::wIsInBattle) {
        1 => Some(BattleKind::Wild),
        2 => Some(BattleKind::Trainer),
        _ => None,
    }
}

/// `HandleMenuInput`'s state, whichever menu is up.
pub fn cursor(memory: &mut dyn MemoryReader) -> Cursor {
    Cursor {
        current: read(memory, ram::wCurrentMenuItem),
        max: read(memory, ram::wMaxMenuItem),
        top_y: read(memory, ram::wTopMenuItemY),
        top_x: read(memory, ram::wTopMenuItemX),
        watched_keys: read(memory, ram::wMenuWatchedKeys),
    }
}

/// Whether the party list is the menu that is up.
///
/// `PartyMenuInit` is the only thing in the game that puts the first item at row 1, column 0 with
/// a maximum of `wPartyCount - 1`, and it watches A and B — or A alone, when
/// `wForcePlayerToChooseMon` said the player may not back out.
fn party_list(memory: &mut dyn MemoryReader) -> bool {
    let cursor = cursor(memory);
    let count = read(memory, ram::wPartyCount);
    count > 0
        && count <= 6
        && cursor.top_y == 1
        && cursor.top_x == 0
        && cursor.max == count - 1
        && (cursor.watched_keys == poke::pad::A | poke::pad::B
            || cursor.watched_keys == poke::pad::A)
}

/// The battle, when one is running.
pub fn battle(memory: &mut dyn MemoryReader) -> Option<Battle> {
    let kind = in_battle(memory)?;
    let cursor = cursor(memory);
    let text_box_id = read(memory, ram::wTextBoxID);

    // The top-level menu: DisplayBattleMenu draws BATTLE_MENU_TEMPLATE, then parks the cursor at
    // row 14 in the left column (x 9, watching RIGHT and A) or the right column (x 15, watching
    // LEFT and A) with one item per column.
    let left = cursor.top_x == 9 && cursor.watched_keys == poke::pad::RIGHT | poke::pad::A;
    let right = cursor.top_x == 15 && cursor.watched_keys == poke::pad::LEFT | poke::pad::A;
    let main = text_box_id == poke::BATTLE_MENU_TEMPLATE
        && cursor.top_y == 14
        && cursor.max == 1
        && (left || right);

    let menu = if main {
        // **FIGHT and ITEM are the left column, PKMN and RUN the right.** The screen reads
        // `FIGHT PKMN` over `ITEM RUN` and the game's index is by column: `wCurrentMenuItem` is
        // the row inside the column the cursor is in, and `.rightColumn` adds two to it on
        // selection -- so the order is FIGHT, ITEM, PKMN, RUN. Surveyed on the cartridge
        // 2026-09-22 (`infra/docs/macros-traps.md`): A at `wTopMenuItemX` 15 with
        // `wCurrentMenuItem` 0 opens the **party** list and the game then writes
        // `wCurrentMenuItem` 2. `macros::cartridge::battle_entry` had this pair the other way
        // round, so `ITEM` and `THROW BALL` opened the party list and `SWITCH` opened the bag.
        let column = if right { 2 } else { 0 };
        BattleMenu::Main { cursor: column + cursor.current.min(1) }
    } else if cursor.top_y == 12 && cursor.top_x == 5 {
        // MoveSelectionMenu's regular menu. Its list is one-based: `wCurrentMenuItem` is
        // `wPlayerMoveListIndex + 1` and `wMaxMenuItem` is the move count plus one.
        let count = read(memory, ram::wNumMovesMinusOne).saturating_add(1).min(4);
        let slot = cursor.current.checked_sub(1).filter(|slot| *slot < count);
        BattleMenu::Moves { cursor: slot, count }
    } else if party_list(memory) {
        BattleMenu::Party { cursor: cursor.current }
    } else if read(memory, ram::wListMenuID) == poke::ITEM_LIST_MENU {
        // The bag, opened from the battle menu's ITEM entry. `DisplayListMenuID` keeps its
        // position in the same shared cursor every other menu uses, and the entry count is the
        // bag's own, so the scripts that reach into it (`ITEM`, `THROW BALL`) navigate by reading
        // rather than by counting presses -- which is what section 4 requires of them and what
        // they could not do while this read as no list at all.
        let count = bag(memory).len().min(usize::from(poke::BAG_CAPACITY));
        BattleMenu::Bag { cursor: cursor.current, count: u8::try_from(count).unwrap_or(0) }
    } else {
        BattleMenu::None
    };

    // A forced switch is the party list that `ChooseNextMon` opens: it is the only battle path
    // that sets BATTLE_PARTY_MENU, where choosing PKMN from the menu above sets
    // NORMAL_PARTY_MENU.
    let forced_switch = matches!(menu, BattleMenu::Party { .. })
        && read(memory, ram::wPartyMenuTypeOrMessageID) == poke::BATTLE_PARTY_MENU;

    let own = own_mon(memory);
    // **The fly's turn is any menu of the battle that is accepting input**, not only the top-level
    // one (2026-09-17, live: an hour and forty-one minutes in Viridian Forest).
    //
    // `own_turn` used to be `Main` alone, so a frame with the move list open read as "between
    // turns" — and the between-turns pad is one `NEXT`, an A press on whatever the cursor happens
    // to be sitting on. The cursor sits on TACKLE, TACKLE was out of PP, the game said so, the box
    // closed, the list came back, and `NEXT` pressed A again: `NEXT start`/`NEXT done` every
    // 268 brain milliseconds for over an hour, with a Kakuna in front of it and Bulbasaur at 8/28.
    //
    // A list that is accepting input is the game waiting for the player to choose, which is what a
    // turn *is*. The party list is the exception that proves it: opened by `ChooseNextMon` after a
    // faint it cannot be cancelled and is a forced switch, which has its own pad; opened by
    // choosing PKMN it is an ordinary part of the fly's turn.
    let own_turn = match menu {
        BattleMenu::Main { .. } => true,
        // A move list whose cursor the seam cannot place is not a list accepting input: the
        // coordinates `MoveSelectionMenu` uses appear on the first frames of a battle, before the
        // engine has copied the active Pokémon into `wBattleMon*` and while `wCurrentMenuItem` is
        // still 0 rather than the one-based slot the menu keeps. Measured on the cartridge: a wild
        // Weedle's opening frame reads `Moves { cursor: None, count: 2 }` with `own: None`. That
        // frame is between turns, which is what it was before this change.
        BattleMenu::Moves { cursor, .. } => cursor.is_some(),
        BattleMenu::Party { .. } => !forced_switch,
        // The bag is a list the fly opened *during* its turn, and it is a menu cursor accepting
        // input, so by the rule above it is the fly's turn (2026-09-22, section 12.10). Reading it
        // as nobody's turn put it on the between-turns row, whose one button is the `NEXT` that
        // advances *text* -- and on an open bag that same A press *uses* whatever the cursor
        // happens to be sitting on. The pad that belongs to a bag is the bag's own three answers,
        // `ITEM`, `THROW BALL` and `BACK`, which is what `palette::scene_set` deals here now.
        BattleMenu::Bag { .. } => true,
        BattleMenu::None => false,
    };
    Some(Battle {
        kind,
        own_turn,
        forced_switch,
        menu,
        own,
        enemy: enemy_mon(memory),
    })
}

/// The Pokémon that is out, read from the battle engine's own copy — the copy it damages.
fn own_mon(memory: &mut dyn MemoryReader) -> Option<Mon> {
    let species = read(memory, ram::wBattleMonSpecies);
    if species == 0 {
        return None;
    }
    Some(Mon {
        slot: read(memory, ram::wPlayerMonNumber),
        species,
        level: read(memory, ram::wBattleMonLevel),
        hp: word_be(memory, ram::wBattleMonHP),
        max_hp: word_be(memory, ram::wBattleMonMaxHP),
        status: status_from(read(memory, ram::wBattleMonStatus)),
        moves: moves_at(memory, ram::wBattleMonMoves, ram::wBattleMonPP),
    })
}

fn enemy_mon(memory: &mut dyn MemoryReader) -> Option<EnemyMon> {
    let species = read(memory, ram::wEnemyMonSpecies);
    if species == 0 {
        return None;
    }
    Some(EnemyMon {
        species,
        level: read(memory, ram::wEnemyMonLevel),
        hp: word_be(memory, ram::wEnemyMonHP),
        max_hp: word_be(memory, ram::wEnemyMonMaxHP),
    })
}

/// Whether a text box is open, and whether the bottom-of-screen dialogue box is the one drawn.
///
/// `open` is `wFontLoaded`'s bit 0, which `DisplayTextIDInit` sets for every text display — the
/// start menu included — and `CloseTextDisplay` clears. It is the WRAM half, and it is the gate:
/// `waiting` is never true without it.
///
/// `waiting` is the box itself: `DisplayTextIDInit` draws a border at screen (0, 12) spanning the
/// full width, rows 12 to 17, for any text id but the start menu's. It is checked as the **whole**
/// `TextBoxBorder` — corners, both horizontal runs and both vertical runs — and not as the four
/// corners alone, because the frame's tile ids are ordinary map tiles in the overworld tilesets and
/// the four corner positions do hold them: **315 frames of 43,004** in the 2026-09-17 reproduction
/// had all four corners drawn with the font flag clear (`infra/docs/macros-traps.md`). Nothing came
/// of it there, because `open` was false on every one of those frames — but four bytes is a thin
/// thing to hang a scene on when the figure the game draws is seventy-six.
pub fn text_box(memory: &mut dyn MemoryReader) -> TextBox {
    let open = read(memory, ram::wFontLoaded) & poke::BIT_FONT_LOADED != 0;
    TextBox { open, waiting: open && border_drawn(memory, 0, 12, 19, 17) }
}

/// Whether the two-option YES/NO box is the thing on screen: a *choice*, not a plain text box.
///
/// `docs/design/macros-wram.md` says there is no "a choice is open" flag, and there is not -- so
/// this is the same construction [`text_box`] makes for `waiting`: a WRAM flag plus the figure the
/// game draws. `DisplayTwoOptionMenu` draws its own little box in the top right and parks the
/// shared cursor inside it, and **both halves are needed**: the cursor bytes are not cleared when
/// the box closes, so at the rung-10 checkpoint every one of the nurse's forty-six text frames
/// reads `wTopMenuItemY` 8, `wTopMenuItemX` 12, `wMaxMenuItem` 1 and `wMenuWatchedKeys` `$03`
/// while the box itself is drawn on exactly one of them (`infra/docs/macros-traps.md`, row 41).
///
/// **What it does not claim.** Red places a two-option menu where the script that asks for it
/// says, so a prompt drawn somewhere else reads `false` here and its dialog keeps the pad it has
/// always had. This is the box the nurse's "heal your POKeMON?" is drawn in, surveyed; it is not a
/// general answer to "is a choice open", and nothing in the palette treats it as one.
pub fn yes_no_prompt(memory: &mut dyn MemoryReader) -> bool {
    if read(memory, ram::wFontLoaded) & poke::BIT_FONT_LOADED == 0 {
        return false;
    }
    let cursor = cursor(memory);
    if cursor.top_y != poke::YES_NO_CURSOR_Y
        || cursor.top_x != poke::YES_NO_CURSOR_X
        || cursor.max != 1
        || cursor.watched_keys != poke::pad::A | poke::pad::B
    {
        return false;
    }
    let (left, top, right, bottom) = poke::YES_NO_BOX;
    border_drawn(memory, left, top, right, bottom)
}

/// The four screen tiles the dialogue box's `waiting` test reads, in the order `box_drawn` reads
/// them: top-left, top-right, bottom-left, bottom-right of a box at (0, 12)-(19, 17).
///
/// Exposed for the diagnostics ([`super::scene::why_unknown`], `examples/scene_probe.rs`), because
/// they are the one input of the dialog branch that is not a WRAM flag: a map tile can hold a
/// frame tile id, and what tells a real box from four map tiles that look like one is these four
/// bytes read beside `wFontLoaded`.
/// How much of the dialogue box's border is actually on screen: `(corners, whole border)`.
///
/// The two answers the diagnostics compare. `corners` is the test [`text_box`]'s `waiting` has
/// always made; `whole` is [`border_drawn`]. A frame with `corners` and not `whole` is four map
/// tiles wearing a text box's clothes.
pub fn dialog_border(memory: &mut dyn MemoryReader) -> (bool, bool) {
    (box_drawn(memory, 0, 12, 19, 17), border_drawn(memory, 0, 12, 19, 17))
}

pub fn dialog_corners(memory: &mut dyn MemoryReader) -> [u8; 4] {
    [
        screen_tile(memory, 0, 12),
        screen_tile(memory, 19, 12),
        screen_tile(memory, 0, 17),
        screen_tile(memory, 19, 17),
    ]
}

/// The start menu, when it is open.
///
/// `DrawStartMenu` puts a box at screen (10, 0) — fourteen rows tall with the Pokédex entry,
/// twelve without — and parks the cursor at row 2, column 11. Both are checked: the geometry alone
/// survives the menu closing, and a frame of the map alone can hold a frame tile id.
pub fn start_menu(memory: &mut dyn MemoryReader) -> Option<StartMenu> {
    if read(memory, ram::wFontLoaded) & poke::BIT_FONT_LOADED == 0 {
        return None;
    }
    let cursor = cursor(memory);
    if cursor.top_y != 2 || cursor.top_x != 11 {
        return None;
    }
    // With the Pokédex the border's `b` is $0e, without it $0c, and TextBoxBorder's bottom row is
    // `b + 1` below the top: row 15 or row 13. The whole border rather than its corners, for the
    // reason [`text_box`] gives.
    if !border_drawn(memory, 10, 0, 19, 15) && !border_drawn(memory, 10, 0, 19, 13) {
        return None;
    }
    // `DrawStartMenu` stores the item *count* in wMaxMenuItem rather than the highest index: 7
    // with the Pokédex entry, 6 without, and `DisplayStartMenu` wraps at 6 and 5 respectively.
    if cursor.max != 6 && cursor.max != 7 {
        return None;
    }
    Some(StartMenu { cursor, items: cursor.max })
}

/// Whether one of the start menu's submenus is on screen: the bag list, an elevator's floor list,
/// or the party list outside a battle.
///
/// This is the weakest rule in the module and it is why [`Scene::Unknown`] exists: pokered has no
/// "a submenu is open" flag, so a submenu the three tests below do not recognise reads as
/// `Unknown`, which the doctrine treats as advance-only. It never reads as `Overworld`.
pub fn submenu(memory: &mut dyn MemoryReader) -> bool {
    if read(memory, ram::wFontLoaded) & poke::BIT_FONT_LOADED == 0 {
        return false;
    }
    let list = read(memory, ram::wListMenuID);
    list == poke::ITEM_LIST_MENU || list == poke::SPECIAL_LIST_MENU || party_list(memory)
}

/// The mart, when one is open.
///
/// `DisplayPokemartDialogue_` draws `BUY_SELL_QUIT_MENU` for the BUY / SELL / QUIT choice, and it
/// is the only user of that template in the game; the buy list is `PRICEDITEMLISTMENU` and the
/// sell list is the bag's own `ITEMLISTMENU`, which is why selling is only recognised while the
/// mart's choice is still the last template drawn.
pub fn shop(memory: &mut dyn MemoryReader) -> Option<Shop> {
    if read(memory, ram::wFontLoaded) & poke::BIT_FONT_LOADED == 0 {
        return None;
    }
    let cursor = cursor(memory);
    let list = read(memory, ram::wListMenuID);
    if list == poke::PRICED_ITEM_LIST_MENU {
        return Some(Shop { screen: ShopScreen::Buying, cursor });
    }
    if read(memory, ram::wTextBoxID) != poke::BUY_SELL_QUIT_MENU {
        return None;
    }
    let screen =
        if list == poke::ITEM_LIST_MENU { ShopScreen::Selling } else { ShopScreen::BuySellQuit };
    Some(Shop { screen, cursor })
}

/// The PC, when one is open. `ActivatePC` sets `wMiscFlags`' generic-PC bit and `LogOff` clears
/// it, so it covers Bill's PC, the player's PC and Oak's alike.
pub fn pc(memory: &mut dyn MemoryReader) -> Option<Pc> {
    if read(memory, ram::wMiscFlags) & poke::BIT_USING_GENERIC_PC == 0 {
        return None;
    }
    Some(Pc { cursor: cursor(memory) })
}

/// Money, in whole units. Three bytes of big-endian BCD, two digits each.
pub fn money(memory: &mut dyn MemoryReader) -> u32 {
    let mut total = 0u32;
    for offset in 0..3u16 {
        let byte = read(memory, ram::wPlayerMoney + offset);
        let high = u32::from(byte >> 4);
        let low = u32::from(byte & 0x0f);
        // A nibble above 9 is not BCD: the cartridge cannot produce one here, and a half-written
        // frame should not turn into a plausible number.
        if high > 9 || low > 9 {
            return 0;
        }
        total = total * 100 + high * 10 + low;
    }
    total
}

/// The bag, in bag order. `wNumBagItems` counts `(id, quantity)` pairs, capped at
/// `BAG_ITEM_CAPACITY`, and the list ends with `$ff`.
pub fn bag(memory: &mut dyn MemoryReader) -> Vec<BagItem> {
    let count = read(memory, ram::wNumBagItems);
    if count > poke::BAG_CAPACITY {
        return Vec::new();
    }
    let mut items = Vec::with_capacity(count as usize);
    for index in 0..u16::from(count) {
        let id = read(memory, ram::wBagItems + index * 2);
        if id == 0xff {
            break;
        }
        items.push(BagItem { id, count: read(memory, ram::wBagItems + index * 2 + 1) });
    }
    items
}

/// What the open mart sells, in menu order.
///
/// `LoadItemList` (`home/text_script.asm`) copies the clerk's `script_mart` list into `wItemList`
/// the moment the counter opens -- a count byte, the item ids, then `$ff` -- and
/// `DisplayPokemartDialogue_` points the buy list's `wListPointer` at the same buffer, so an
/// item's **position in this list is its cursor index** in `PRICEDITEMLISTMENU`. That is the whole
/// reason a purchase can be navigated by reading the cursor rather than by counting presses.
///
/// The terminator wins over the count, exactly as it does for the bag ([`bag`]): a count that
/// disagrees with a `$ff` is a half-written buffer, and the shorter answer is the safe one. The
/// buffer is sixteen bytes, so nothing past it is read whatever either says.
///
/// Empty when no mart is open -- the buffer is not cleared between visits, so the honest reading
/// is gated on the mart scene being up, and [`PokeState`]'s `shop_stock` is where that gate is.
pub fn shop_stock(memory: &mut dyn MemoryReader) -> Vec<u8> {
    let count = read(memory, ram::wItemList);
    if count == 0 || count >= poke::MART_LIST_BYTES {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(count as usize);
    for index in 1..=u16::from(count) {
        let id = read(memory, ram::wItemList + index);
        if id == 0xff || id == 0 {
            break;
        }
        out.push(id);
    }
    out
}

/// The current tileset's three counter tile ids, `$ff` where the header has none.
///
/// `data/tilesets/tileset_headers.asm` gives Mart and Pokecenter `$18`, `$19`, `$1e`; the
/// overworld and an ordinary house have none at all.
pub fn counter_tiles(memory: &mut dyn MemoryReader) -> [u8; 3] {
    let mut out = [poke::NO_COUNTER_TILE; 3];
    for index in 0..poke::COUNTER_TILES {
        out[index as usize] = read(memory, ram::wTilesetTalkingOverTiles + index);
    }
    out
}

/// The screen-buffer tile id of a tile of the current map, or `None` where it cannot be read.
///
/// The same window and the same origin [`walkable`] uses, and the same three bounds: off the map,
/// off the ten-by-nine window that moves with the player, or a screen holding a battle or a text
/// box instead of the map. Split out of `walkable` so that "is this tile passable" and "is this
/// tile a counter" read one byte by one rule rather than two.
pub fn map_tile_id(memory: &mut dyn MemoryReader, x: u8, y: u8) -> Option<u8> {
    let size = map_size(memory)?;
    if x >= size.width || y >= size.height {
        return None;
    }
    if in_battle(memory).is_some() || read(memory, ram::wFontLoaded) & poke::BIT_FONT_LOADED != 0 {
        return None;
    }
    let player = player(memory)?;
    let screen_x = poke::PLAYER_SCREEN_X + 2 * (i32::from(x) - i32::from(player.x));
    let screen_y = poke::PLAYER_SCREEN_Y + 2 * (i32::from(y) - i32::from(player.y));
    if !(0..poke::SCREEN_WIDTH as i32).contains(&screen_x)
        || !(0..poke::SCREEN_HEIGHT as i32).contains(&screen_y)
    {
        return None;
    }
    Some(screen_tile(memory, screen_x as u16, screen_y as u16))
}

/// Whether a tile of the current map is one the game lets the player talk *over*.
///
/// `IsSpriteOrSignInFrontOfPlayer`'s `.extendRangeOverCounter` branch: when the tile in front of
/// the player is one of the tileset's three counter tiles it doubles the talking range from `$10`
/// to `$20` pixels, i.e. from one tile to two. That branch is the only reason a mart clerk or a
/// Pokémon Center nurse can be spoken to at all -- both stand behind a desk, so none of the four
/// tiles around either of them is walkable, and a macro that only knew how to stand beside a
/// person could never reach one (`docs/design/macros.md` section 13).
///
/// `false` for a tile that cannot be read, which narrows: the approach falls back to the four
/// adjacent tiles, which is what every other person on the map needs anyway.
pub fn counter_tile(memory: &mut dyn MemoryReader, x: u8, y: u8) -> bool {
    let Some(tile) = map_tile_id(memory, x, y) else { return false };
    counter_tiles(memory)
        .iter()
        .any(|counter| *counter != poke::NO_COUNTER_TILE && *counter == tile)
}

/// Visible NPC sprites on the current map, by sprite slot.
///
/// Slot 0 is the player and is skipped. A slot with picture id 0, or with `$ff` in its image
/// index — which is what `LoadMapSpriteData` writes into the slots the map does not use — is not
/// on screen. Map coordinates are stored plus four, because `MACRO object_event` emits them that
/// way.
pub fn npcs(memory: &mut dyn MemoryReader) -> Vec<Npc> {
    let count = read(memory, ram::wNumSprites).min(poke::SPRITE_SLOTS - 1);
    let mut npcs = Vec::with_capacity(count as usize);
    for slot in 1..=count {
        let data1 = ram::wSpriteStateData1 + u16::from(slot) * poke::SPRITE_BYTES;
        let data2 = ram::wSpriteStateData2 + u16::from(slot) * poke::SPRITE_BYTES;
        let picture = read(memory, data1);
        if picture == 0 || read(memory, data1 + 2) == 0xff {
            continue;
        }
        let y = read(memory, data2 + 4);
        let x = read(memory, data2 + 5);
        if y < poke::SPRITE_COORD_BIAS || x < poke::SPRITE_COORD_BIAS {
            continue;
        }
        npcs.push(Npc {
            slot,
            picture,
            x: x - poke::SPRITE_COORD_BIAS,
            y: y - poke::SPRITE_COORD_BIAS,
            facing: facing_from(read(memory, data1 + 9)),
        });
    }
    npcs
}

/// The current tileset's list of passable tile ids, terminator included.
///
/// `CheckTilePassable` walks the list at `wTilesetCollisionPtr` — a little-endian pointer into the
/// collision tables, which all live in ROM bank 0 at this commit and so are always mapped — until
/// it matches or hits `$ff`. `None` means the pointer is not one this module will follow, or the
/// list is not terminated inside the bound below.
///
/// One read of the list serves both callers: [`walkable`] asks about one tile of the window, and
/// [`map_grid`] asks about every tile of the map, and neither is allowed its own copy of the rule.
fn collision_list(memory: &mut dyn MemoryReader) -> Option<Vec<u8>> {
    let low = u16::from(read(memory, ram::wTilesetCollisionPtr));
    let high = u16::from(read(memory, ram::wTilesetCollisionPtr + 1));
    let base = high * 256 + low;
    // Bank 0 only, and not the interrupt vectors: every `*_Coll` label at this commit resolves
    // inside 00:1700..00:1800. A pointer outside bank 0 would read whichever bank happens to be
    // mapped, which is not an answer.
    if !(0x0100..0x4000).contains(&base) {
        return None;
    }
    // No collision list in the game is longer than this; the bound is what stops a bad pointer
    // from walking the cartridge.
    let mut list = Vec::new();
    for offset in 0..64u16 {
        let byte = read(memory, base + offset);
        list.push(byte);
        if byte == mapgrid::TERMINATOR {
            return Some(list);
        }
    }
    None
}

/// Whether the current tileset calls this tile id passable.
///
/// [`collision_list`]'s own walk, and `CheckTilePassable`'s: match or `$ff`, whichever comes
/// first. `None` means the list could not be read at all.
fn passable(memory: &mut dyn MemoryReader, tile: u8) -> Option<bool> {
    let list = collision_list(memory)?;
    for candidate in list {
        if candidate == mapgrid::TERMINATOR {
            return Some(false);
        }
        if candidate == tile {
            return Some(true);
        }
    }
    Some(false)
}

/// Why a whole-map grid could not be decoded on this frame.
///
/// `docs/design/macros.md` section 15 asks the fallback to *say when*, so every way out of
/// [`map_grid`] is named rather than being one `None`. Each one leaves the window predicate in
/// charge, which is what the walks did before the grid existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridRefusal {
    /// The map header is not loaded, or its size is out of range (`map_size` said `None`).
    NoHeader,
    /// The player's coordinates are not readable, so nothing can be cross-checked.
    NoPlayer,
    /// The tileset's collision list could not be followed ([`collision_list`]).
    NoCollisionList,
    /// The blockset could not be read: the seam has no cartridge behind it
    /// ([`MemoryReader::read_rom`] answered `None`), or the header's pointer runs off the image.
    NoBlockset,
    /// The screen is not showing the map — a battle, a text box, a frame mid-warp — so there is
    /// nothing to check the decode against, and `wOverworldMap` shares its bytes with the picture
    /// buffer (`ram/wram.asm`'s own union), which is exactly when it must not be trusted.
    NoScreen,
    /// The decode and the screen buffer disagree about a tile the window can answer for. A wrong
    /// stride, a wrong quadrant or a half-loaded map all land here, and all of them answer
    /// plausibly, which is why this check is not optional.
    ScreenDisagrees,
}

impl GridRefusal {
    /// A short label for a log line and the probes.
    pub fn label(self) -> &'static str {
        match self {
            GridRefusal::NoHeader => "no map header",
            GridRefusal::NoPlayer => "no player",
            GridRefusal::NoCollisionList => "no collision list",
            GridRefusal::NoBlockset => "no blockset",
            GridRefusal::NoScreen => "map not on screen",
            GridRefusal::ScreenDisagrees => "screen disagrees",
        }
    }
}

/// The whole loaded map's walkability, decoded from the tables the cartridge has loaded.
///
/// `docs/design/macros.md` section 15. The rule is [`walkable`]'s rule — the tileset's collision
/// list — and what this adds is the tile id of every tile of the map rather than of the ten-by-nine
/// window:
///
/// - the map's **blocks** come from `wOverworldMap`, which `LoadTileBlockMap` fills from the map's
///   own ROM bank as rows of `wCurMapWidth + MAP_BORDER * 2` bytes with the map itself three rows
///   and three columns in. That is WRAM, so it needs no bank at all.
/// - a block's **tiles** come from the tileset header's blockset, sixteen bytes per block id
///   (`DrawTileBlock`). That is ROM, and not bank 0, so it is the one read that goes through
///   [`MemoryReader::read_rom`] — the cartridge image as the process already holds it, because the
///   alternative would be *writing* the mapper's bank register and the joypad is the only write
///   this workspace makes into a running game.
/// - the **tile-pair** refusals come from the values of `TilePairCollisionsLand`, keyed by
///   `wCurMapTileset`, and become directed walls ([`mapgrid::TILE_PAIRS_LAND`]).
///
/// The last thing it does is check itself: the decoded tile ids are compared against
/// [`map_tile_id`] for the player's own tile and its four neighbours, every one the window can
/// answer for. A frame where the window can answer for none of them is refused
/// ([`GridRefusal::NoScreen`]) rather than trusted, because `wOverworldMap` shares its bytes with
/// the picture buffer and a battle is exactly when the blocks under it are somebody else's.
pub fn map_grid(memory: &mut dyn MemoryReader) -> Result<MapGrid, GridRefusal> {
    // The decode first, so a frame with no header answers `NoHeader` rather than whatever the
    // player's coordinates happen to read as: the refusals are in the order they are checked.
    let grid = map_grid_decode(memory)?;
    let player = player(memory).ok_or(GridRefusal::NoPlayer)?;
    // The cross-check. `map_tile_id` reads the screen buffer at the offset
    // `_GetTileAndCoordsInFrontOfPlayer` uses, so agreeing with it on the tiles it can answer for
    // is agreeing with the cartridge's own reading of the same ground.
    let mut checked = 0;
    for (x, y) in neighbourhood(player.x, player.y) {
        let Some(screen) = map_tile_id(memory, x, y) else { continue };
        if grid.tile_id(x, y) != Some(screen) {
            return Err(GridRefusal::ScreenDisagrees);
        }
        checked += 1;
    }
    if checked == 0 {
        return Err(GridRefusal::NoScreen);
    }
    Ok(grid)
}

/// [`map_grid`] without the cross-check: the blocks, the blockset and the collision list, decoded.
///
/// Split out so [`grid_disagreement`] can say what the decode answered on a frame the check
/// refused. Nothing outside this module and the probes may use it: a grid that has not been
/// checked against the screen is exactly the reading section 15 refuses to trust.
fn map_grid_decode(memory: &mut dyn MemoryReader) -> Result<MapGrid, GridRefusal> {
    let size = map_size(memory).ok_or(GridRefusal::NoHeader)?;
    let player = player(memory).ok_or(GridRefusal::NoPlayer)?;
    let passable = collision_list(memory).ok_or(GridRefusal::NoCollisionList)?;
    let width_blocks = read(memory, ram::wCurMapWidth);
    let height_blocks = read(memory, ram::wCurMapHeight);
    let stride = u16::from(width_blocks) + (mapgrid::MAP_BORDER as u16) * 2;
    let border = mapgrid::MAP_BORDER as u16;
    // The map plus its border has to fit in `wOverworldMap`, which every real map does. One that
    // does not is a header caught mid-load, and reading past the buffer would be reading somebody
    // else's WRAM.
    if usize::from(stride) * (usize::from(height_blocks) + mapgrid::MAP_BORDER * 2)
        > mapgrid::OVERWORLD_MAP_BYTES
    {
        return Err(GridRefusal::NoHeader);
    }
    let mut blocks = Vec::with_capacity(usize::from(width_blocks) * usize::from(height_blocks));
    for row in 0..u16::from(height_blocks) {
        for column in 0..u16::from(width_blocks) {
            blocks.push(read(memory, ram::wOverworldMap + (row + border) * stride + column + border));
        }
    }
    // Only as much of the blockset as this map's blocks index into: a tileset has up to 256 of
    // them and a room uses a dozen, and a read that stops at the highest block id used is a read
    // that cannot run off the end of a bank for tiles nothing asks about.
    let highest = blocks.iter().copied().max().unwrap_or(0);
    let bank = read(memory, ram::wTilesetBank);
    let base = u16::from(read(memory, ram::wTilesetBlocksPtr))
        + u16::from(read(memory, ram::wTilesetBlocksPtr + 1)) * 256;
    let wanted = (usize::from(highest) + 1) * mapgrid::BLOCK_BYTES;
    let mut blockset = Vec::with_capacity(wanted);
    for offset in 0..wanted {
        let address = base.checked_add(u16::try_from(offset).map_err(|_| GridRefusal::NoBlockset)?);
        let byte = address
            .and_then(|address| memory.read_rom(bank, address))
            .ok_or(GridRefusal::NoBlockset)?;
        blockset.push(byte);
    }
    let tiles = mapgrid::Tileset { id: read(memory, ram::wCurMapTileset), blocks: blockset, passable };
    let grid = mapgrid::decode(player.map, width_blocks, height_blocks, &blocks, &tiles);
    if grid.width() != size.width || grid.height() != size.height {
        return Err(GridRefusal::NoHeader);
    }
    Ok(grid)
}

/// The decode and the screen, tile by tile, for the five tiles [`map_grid`] cross-checks.
///
/// The diagnostic half of [`GridRefusal::ScreenDisagrees`]: the refusal says the two readings
/// disagree and this says *where* and *by how much*, which is the difference between "the grid is
/// off on this map" and "this frame was mid-warp". `(x, y, decoded, screen)`, with `None` for a
/// tile either reading cannot answer for. It decodes the map a second time rather than being
/// folded into [`map_grid`], because the check's job on the hot path is to refuse and this is only
/// ever asked by a probe.
pub fn grid_disagreement(memory: &mut dyn MemoryReader) -> Vec<(u8, u8, Option<u8>, Option<u8>)> {
    let Some(player) = player(memory) else { return Vec::new() };
    let grid = map_grid_decode(memory).ok();
    neighbourhood(player.x, player.y)
        .into_iter()
        .map(|(x, y)| {
            (x, y, grid.as_ref().and_then(|grid| grid.tile_id(x, y)), map_tile_id(memory, x, y))
        })
        .collect()
}

/// Whether a cached grid is still the map that is loaded, checked from the tile the fly is on.
///
/// The map id, the map header and the block data are written by different parts of a warp, so
/// there is a frame or two on the way through a door where `wCurMap` is the map the fly is
/// arriving on and the header and the blocks are still the map it is leaving: the decode agrees
/// with the screen (both are the old map) and is filed under the new id. Measured on the
/// cartridge — the fly on Oak's lab doormat with `wCurMap` already reading `PALLET_TOWN` and the
/// header still the lab's ten-by-twelve (`tests/rom_map_grid.rs`).
///
/// Nothing in WRAM says "the map has finished loading", so the cache asks the cheapest question
/// that can tell: does the grid still agree with the screen about the tile the fly is standing on?
/// One byte, once per question. A grid that does not is dropped and decoded again, so a torn
/// frame's grid lives exactly as long as the tear does — and through it the cartridge is walking
/// the fly, which is not a frame any macro plans on.
fn still_the_loaded_map(
    memory: &mut dyn MemoryReader,
    grid: &MapGrid,
    x: u8,
    y: u8,
) -> bool {
    match map_tile_id(memory, x, y) {
        // The screen is not showing the map (a battle, a text box): nothing to check against, and
        // the grid was checked when it was decoded.
        None => true,
        Some(tile) => grid.tile_id(x, y) == Some(tile),
    }
}

/// The player's own tile and its four neighbours, which is every tile the window is certain to be
/// able to answer for from where the fly is standing.
fn neighbourhood(x: u8, y: u8) -> Vec<(u8, u8)> {
    let mut out = vec![(x, y)];
    for (dx, dy) in [(0i16, 1i16), (0, -1), (-1, 0), (1, 0)] {
        if let (Ok(nx), Ok(ny)) =
            (u8::try_from(i16::from(x) + dx), u8::try_from(i16::from(y) + dy))
        {
            out.push((nx, ny));
        }
    }
    out
}

/// Whether the player could stand on this tile of the current map.
///
/// Mirrors `CheckTilePassable`: the tile id comes out of the screen buffer at the offset
/// `_GetTileAndCoordsInFrontOfPlayer` would use, and is looked up in the current tileset's
/// passable list. Two things bound it, both reported as [`Walkable::Unknown`] rather than guessed:
///
/// - **the window.** The screen buffer holds ten tiles by nine and the player is always at the
///   middle of it — `wOverworldMap` carries three blocks of border around the real map precisely
///   so that the view can centre even on a map smaller than the screen — so only
///   `x - 4 ..= x + 5` and `y - 4 ..= y + 4` can be answered at all, and the window moves with the
///   player. A house is answerable whole from the middle of it and only in part from a corner; a
///   town or a route never is. An A* over this has to treat `Unknown` as impassable and re-plan as
///   it moves, which is what the per-step check in `docs/design/macros.md` section 4 already
///   requires of it.
/// - **the screen.** While a text box or a battle is up, the buffer holds the box, not the map.
///
/// What it does *not* model, and what the executor's per-step "did the player move" check is for
/// (`docs/design/macros.md` section 4): NPCs standing in the way — [`npcs`] reports those
/// separately — ledges, the tile-pair rules that stop a player walking from water to land, and
/// warps that fire the instant they are stepped on.
pub fn walkable(memory: &mut dyn MemoryReader, x: u8, y: u8) -> Walkable {
    let Some(size) = map_size(memory) else {
        return Walkable::Unknown;
    };
    if x >= size.width || y >= size.height {
        // Off the map is not a tile to stand on. The tiles a connection leads through are the
        // map's own edge rows, which are inside it.
        return Walkable::No;
    }
    let Some(tile) = map_tile_id(memory, x, y) else {
        return Walkable::Unknown;
    };
    match passable(memory, tile) {
        Some(true) => Walkable::Yes,
        Some(false) => Walkable::No,
        None => Walkable::Unknown,
    }
}

/// The current map's warp table: `wNumberOfWarps` entries of four bytes, `Y, X, warp id, map id`.
pub fn warps(memory: &mut dyn MemoryReader) -> Vec<Warp> {
    let count = read(memory, ram::wNumberOfWarps).min(poke::MAX_WARPS);
    let mut warps = Vec::with_capacity(count as usize);
    for index in 0..u16::from(count) {
        let entry = ram::wWarpEntries + index * 4;
        warps.push(Warp {
            y: read(memory, entry),
            x: read(memory, entry + 1),
            destination_warp: read(memory, entry + 2),
            destination_map: read(memory, entry + 3),
        });
    }
    warps
}

/// The current map's signs: `wNumSigns` entries of `wSignCoords` as `Y, X`, with `wSignTextIDs`
/// parallel to them.
///
/// `MACRO bg_event x, y, text` emits `db \2, \1, \3`, so Y comes first and -- unlike
/// `object_event`, whose coordinates are stored plus four -- there is **no bias**: the loader in
/// `home/overworld.asm` copies the two bytes straight across, and
/// `IsSpriteOrSignInFrontOfPlayer` compares them against the coordinates
/// `GetTileAndCoordsInFrontOfPlayer` returns. A sign's coordinates are therefore in the same tile
/// space as `wXCoord` and `wYCoord` with no conversion, which is true of the warp table too and
/// is exactly what is *not* true of the sprite slots.
pub fn signs(memory: &mut dyn MemoryReader) -> Vec<Sign> {
    let count = read(memory, ram::wNumSigns).min(poke::MAX_SIGNS);
    let mut signs = Vec::with_capacity(count as usize);
    for index in 0..u16::from(count) {
        let coords = ram::wSignCoords + index * 2;
        signs.push(Sign {
            y: read(memory, coords),
            x: read(memory, coords + 1),
            text_id: read(memory, ram::wSignTextIDs + index),
        });
    }
    signs
}

/// Which of the current map's edges lead to another map, from `wCurMapConnections`.
pub fn connections(memory: &mut dyn MemoryReader) -> Connections {
    let bits = read(memory, ram::wCurMapConnections);
    Connections {
        north: bits & poke::CONNECTION_NORTH != 0,
        south: bits & poke::CONNECTION_SOUTH != 0,
        east: bits & poke::CONNECTION_EAST != 0,
        west: bits & poke::CONNECTION_WEST != 0,
    }
}

/// A [`GameState`] over live WRAM.
///
/// Holds the reader and the exploration ledger, and nothing else: every call reads through,
/// because the reader underneath memoizes per frame and a second cache here could only go stale.
pub struct PokeState<'a> {
    memory: &'a mut dyn MemoryReader,
    /// Which ledger this run has already been through ([`MacroState::exit_visited`]).
    ledger: &'a dyn RunLedger,
    /// What this session has already talked to ([`MacroState::talked`]). Not the adapter's: it
    /// belongs to the executor layer and never reaches the checkpoint.
    talk: &'a dyn TalkLedger,
    /// Which targets are excluded and which have been reached ([`MacroState::blocked`] and
    /// [`MacroState::reached`]). Session state beside the talked ledger, and owned by the same
    /// type.
    targets: &'a dyn TargetLedger,
    /// The ground this session has watched the fly stand on ([`MacroState::tile_visited`]'s
    /// second half). Session state beside the two above, owned by the same type, and the answer
    /// the adapter's reward ledger cannot give for a warp tile.
    stood: &'a dyn StoodLedger,
    /// Which of each area's errands this run has discharged ([`MacroState::area_visited`]).
    /// Session state beside the three above, owned by the same type
    /// (`docs/design/macros.md` section 13).
    areas: &'a dyn AreaLedger,
    /// Tiles the cartridge has pushed the fly off ([`MacroState::pushed_tile`]). Session state
    /// beside the four above, owned by the same type (`infra/docs/macros-traps.md` row 37).
    pushed: &'a dyn PushedLedger,
    /// Where the decoded map grid is kept between frames ([`MacroState::map_grid`],
    /// `docs/design/macros.md` section 15).
    ///
    /// Mutable, unlike every ledger above, because this is the one thing the state *computes*
    /// rather than looks up: a decode is a few thousand reads and a walk of the blockset, and it
    /// is valid for as long as the map is loaded. Without a cache every caller decodes again,
    /// which is correct and is what the tests do; the sim loop passes one
    /// ([`PokeState::caching_grid`]) so that a precondition asking for the frontier costs a
    /// refcount instead of a map.
    grids: Option<&'a mut MapGrids>,
}

impl<'a> PokeState<'a> {
    /// A state over WRAM alone, with an empty exploration ledger.
    ///
    /// Every exit reads as unvisited, which is what a fresh run is. Callers that have the
    /// adapter's ledger should use [`PokeState::with_ledger`], and everything the sim loop builds
    /// does.
    pub fn new(memory: &'a mut dyn MemoryReader) -> Self {
        Self {
            memory,
            ledger: &NoLedger,
            talk: &NoTalk,
            targets: &NoTargets,
            stood: &NoStood,
            areas: &NoAreas,
            pushed: &NoPushed,
            grids: None,
        }
    }

    /// A state over WRAM and the adapter's exploration ledger, with nothing talked to yet.
    pub fn with_ledger(memory: &'a mut dyn MemoryReader, ledger: &'a dyn RunLedger) -> Self {
        Self {
            memory,
            ledger,
            talk: &NoTalk,
            targets: &NoTargets,
            stood: &NoStood,
            areas: &NoAreas,
            pushed: &NoPushed,
            grids: None,
        }
    }

    /// A state over WRAM, the adapter's exploration ledger and the session's own three.
    ///
    /// What the sim loop builds: [`super::macros::driver::PokemonPalette`] owns the talked and
    /// target halves and passes them in on every call, exactly as the adapter owns the
    /// exploration half.
    pub fn with_ledgers(
        memory: &'a mut dyn MemoryReader,
        ledger: &'a dyn RunLedger,
        talk: &'a dyn TalkLedger,
        targets: &'a dyn TargetLedger,
        stood: &'a dyn StoodLedger,
        areas: &'a dyn AreaLedger,
        pushed: &'a dyn PushedLedger,
    ) -> Self {
        Self { memory, ledger, talk, targets, stood, areas, pushed, grids: None }
    }

    /// Keep the decoded map grid in `grids` instead of decoding it per question.
    ///
    /// The cache is keyed by map id and size and holds one map, so arriving somewhere else drops
    /// it (`docs/design/macros.md` section 15). Session state: it is owned by
    /// [`super::macros::driver::PokemonPalette`], never checkpointed, and rebuilt from the
    /// cartridge on the first overworld frame after a restore.
    pub fn caching_grid(mut self, grids: &'a mut MapGrids) -> Self {
        self.grids = Some(grids);
        self
    }
}

impl GameState for PokeState<'_> {
    fn scene(&mut self) -> Scene {
        super::scene::detect(self.memory)
    }

    fn player(&mut self) -> Option<Player> {
        player(self.memory)
    }

    fn map_size(&mut self) -> Option<MapSize> {
        map_size(self.memory)
    }

    fn party(&mut self) -> Party {
        party(self.memory)
    }

    fn battle(&mut self) -> Option<Battle> {
        battle(self.memory)
    }

    fn text_box(&mut self) -> TextBox {
        text_box(self.memory)
    }

    fn start_menu(&mut self) -> Option<StartMenu> {
        start_menu(self.memory)
    }

    fn shop(&mut self) -> Option<Shop> {
        shop(self.memory)
    }

    fn pc(&mut self) -> Option<Pc> {
        pc(self.memory)
    }

    fn money(&mut self) -> u32 {
        money(self.memory)
    }

    fn bag(&mut self) -> Vec<BagItem> {
        bag(self.memory)
    }

    fn npcs(&mut self) -> Vec<Npc> {
        npcs(self.memory)
    }

    fn signs(&mut self) -> Vec<Sign> {
        signs(self.memory)
    }

    fn walkable(&mut self, x: u8, y: u8) -> Walkable {
        walkable(self.memory, x, y)
    }

    fn warps(&mut self) -> Vec<Warp> {
        warps(self.memory)
    }

    fn connections(&mut self) -> Connections {
        connections(self.memory)
    }
}

/// The cartridge tables on their defaults, and the exploration ledger wired through.
///
/// `pokemon_red/macros/cartridge.rs` defaults every [`MacroState`] method and every default
/// *narrows* what the palette offers, so the executor runs over live WRAM with no overrides at
/// all and each one turned on later widens it without changing a signature. Two are still on
/// their defaults — the move table and chart for `ATTACK`, the mart's stock for `BUY POTION` —
/// because both live in ROM banks this module may not reach.
///
/// `exit_visited` is the one that is wired: the adapter's `boundary` ledger, through
/// [`crate::macros::RunLedger`], which is what makes `GO EXIT` aim at the ledger this run has not
/// taken rather than at the nearest door (`docs/design/macros.md` section 3).
impl MacroState for PokeState<'_> {
    fn scripted(&mut self) -> bool {
        !controllable(self.memory)
    }

    fn text_open(&mut self) -> bool {
        text_box(self.memory).open
    }

    fn yes_no_prompt(&mut self) -> bool {
        yes_no_prompt(self.memory)
    }

    /// The whole loaded map's walkability, from the cache when it is for this map
    /// (`docs/design/macros.md` section 15).
    ///
    /// `None` is the honest answer on every frame [`map_grid`] refuses — no cartridge behind the
    /// seam, a battle or a text box over the map, a header that is not loaded — and every caller
    /// falls back to the ten-by-nine window predicate then, which is what all of them did before
    /// this existed. [`GridRefusal`] names which, for the probes.
    fn map_grid(&mut self) -> Option<std::sync::Arc<MapGrid>> {
        let player = player(self.memory)?;
        let size = map_size(self.memory)?;
        if let Some(grids) = self.grids.as_deref()
            && let Some(grid) = grids.get(player.map, size.width, size.height)
            && still_the_loaded_map(self.memory, &grid, player.x, player.y)
        {
            return Some(grid);
        }
        let grid = map_grid(self.memory).ok()?;
        match self.grids.as_deref_mut() {
            Some(grids) => Some(grids.store(grid)),
            None => Some(std::sync::Arc::new(grid)),
        }
    }

    /// What the open mart sells, in menu order (`docs/design/macros.md` section 13).
    ///
    /// Gated on the mart scene being up, and that gate is the whole of the accuracy here:
    /// `wItemList` is a scratch buffer that `LoadItemList` fills when a counter opens and nobody
    /// clears afterwards, so off the mart screen it holds whatever the last list was -- a previous
    /// mart's stock, or a `MonsterNames` list from a battle. Answering `[]` everywhere else is the
    /// narrowing this trait's defaults all are: the four purchase buttons are unbound off the
    /// counter, which is where they could not be pressed anyway.
    fn shop_stock(&mut self) -> Vec<u8> {
        if shop(self.memory).is_none() {
            return Vec::new();
        }
        shop_stock(self.memory)
    }

    /// Whether a tile of the loaded map is one the game lets the player talk over: a counter.
    fn counter_tile(&mut self, x: u8, y: u8) -> bool {
        counter_tile(self.memory, x, y)
    }

    /// Whether this run has already been into `area`'s mart or Pokémon Center (section 13).
    fn area_visited(&mut self, kind: Amenity, area: u8) -> bool {
        self.areas.visited(kind, area)
    }

    /// Whether the cartridge has pushed the fly off this tile of the loaded map (row 37).
    fn pushed_tile(&mut self, x: u8, y: u8) -> bool {
        let Some(map) = player(self.memory).map(|player| player.map) else { return false };
        self.pushed.pushed(map, Tile::new(x, y))
    }

    fn exit_visited(&mut self, exit: ExitId) -> bool {
        let Some(map) = player(self.memory).map(|player| player.map) else {
            // No loaded map, so no exit of it to have visited. The palette is not offering
            // `GO EXIT` on such a frame anyway: `path::ledger` needs a map size first.
            return false;
        };
        let asked = match exit {
            // A warp is named by its index into `warps()` on the palette's side and by its tile on
            // the ledger's, because the ledger is lifetime state and a warp index is only stable
            // while the map is loaded. An index past the end of the table is not an exit at all.
            ExitId::Warp(index) => {
                let warps = warps(self.memory);
                let Some(warp) = warps.get(usize::from(index)) else {
                    return false;
                };
                MapExit::Warp { map, x: warp.x, y: warp.y }
            }
            ExitId::Edge(edge) => MapExit::Edge {
                map,
                edge: match edge {
                    Edge::North => MapEdge::North,
                    Edge::South => MapEdge::South,
                    Edge::East => MapEdge::East,
                    Edge::West => MapEdge::West,
                },
            },
        };
        self.ledger.exit_visited(asked)
    }

    /// Whether this run has stood on this tile of the loaded map (`GO FRONTIER`).
    ///
    /// The coordinates are the loaded map's, so the map id comes from WRAM rather than from the
    /// caller: a tile is only ever asked about while its map is the one on screen.
    fn tile_visited(&mut self, x: u8, y: u8) -> bool {
        let Some(map) = player(self.memory).map(|player| player.map) else { return false };
        // Two ledgers, OR'd, because the first one has a hole the macros cannot ask it to close.
        // The adapter's is the `exploration` payout's own ledger and its gate rejects every frame
        // with `wMovementFlags`' door and warp bits set, so a *doormat* — walkable, standable
        // ground the fly is free to stand on all day — can never be recorded in it. That left
        // every warp tile in the game a permanent frontier, which is the four hours the fly spent
        // in Viridian Forest's south gate on 2026-09-17 (`infra/docs/macros-traps.md` row 32).
        // [`StoodLedger`] is the macro layer's own answer to its own question, and it can only
        // ever shrink the frontier.
        self.ledger.tile_visited(MapTile { map, x, y })
            || self.stood.stood(map, Tile::new(x, y))
    }

    /// Whether this run has ever been on `map` (`GO ROUTE`'s unvisited interior).
    fn map_visited(&mut self, map: u8) -> bool {
        self.ledger.map_visited(map)
    }

    /// Whether this session has talked to `target` on the map that is loaded.
    fn talked(&mut self, target: TalkTarget) -> bool {
        let Some(map) = player(self.memory).map(|player| player.map) else { return false };
        self.talk.talked(map, target)
    }

    /// Whether `target` is still inside its blocked-target window on the map that is loaded.
    ///
    /// Keyed by the loaded map for the same reason the talked ledger is: a warp index and a
    /// sprite slot only mean anything while their map is on screen.
    fn blocked(&mut self, target: TargetKey) -> bool {
        let Some(map) = player(self.memory).map(|player| player.map) else { return false };
        self.targets.blocked(map, target)
    }

    /// Whether `GO ITEM` or `GO NPC` has already reached `target` on the map that is loaded.
    fn reached(&mut self, target: TargetKey) -> bool {
        let Some(map) = player(self.memory).map(|player| player.map) else { return false };
        self.targets.reached(map, target)
    }

    /// Where the ladder's next unreached rung is (`GO OBJECTIVE`).
    ///
    /// The adapter's [`crate::adapter::MapPlace`] in the executor's own types, which is the only
    /// conversion this seam needs: nothing under `macros/` names an adapter type, and the place is
    /// a place rather than a route — the macro still has to find its own way there.
    fn objective(&mut self) -> Option<Objective> {
        let place = self.ledger.objective()?;
        Some(Objective {
            map: place.map,
            tile: place.tile.map(|(x, y)| Tile::new(x, y)),
            warp: place.warp,
            edge: place.edge.map(|edge| match edge {
                MapEdge::North => Edge::North,
                MapEdge::South => Edge::South,
                MapEdge::East => Edge::East,
                MapEdge::West => Edge::West,
            }),
            target: place.target,
        })
    }
}

#[cfg(test)]
mod tests;
