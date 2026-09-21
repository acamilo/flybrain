//! Synthetic WRAM for the scene and accessor tests.
//!
//! A flat 64 KiB address space with a [`MemoryReader`] over it, plus builders that write the byte
//! patterns the real cartridge produces. The patterns are the interesting part: each one is
//! assembled from the same disassembly evidence as the accessor it exercises, so a test that
//! passes here is a test against what `docs/design/macros-wram.md` claims, not against the
//! implementation's own opinion. The ROM-gated tests in `tests/rom_scene.rs` are what check the
//! claims against the cartridge.
//!
//! The space covers ROM bank 0 as well as WRAM, because one accessor reads it: the tileset
//! collision lists live at `00:17xx` and [`Wram::house_collision`] puts the real `RedsHouse1_Coll`
//! bytes there.

use crate::adapter::MemoryReader;

use super::state::poke;
use super::symbols::ram;

/// `constants/map_constants.asm`.
pub const REDS_HOUSE_1F: u8 = 0x25;
pub const PALLET_TOWN: u8 = 0x00;
pub const OAKS_LAB: u8 = 0x28;

/// `data/tilesets/collision_tile_ids.asm`: `RedsHouse1_Coll` and `RedsHouse2_Coll` share a list.
pub const REDS_HOUSE_COLL: [u8; 9] =
    [0x01, 0x02, 0x03, 0x11, 0x12, 0x13, 0x14, 0x1c, 0x1a];
/// Where that list sits in the cartridge (`pokered.sym`: `00:1749 RedsHouse1_Coll`).
pub const REDS_HOUSE_COLL_ADDRESS: u16 = 0x1749;

/// A tile id that is in no collision list in the game, for "this tile is a wall".
pub const WALL_TILE: u8 = 0x60;

pub struct Wram {
    bytes: Vec<u8>,
}

impl MemoryReader for Wram {
    fn read8(&mut self, address: u16) -> u8 {
        self.bytes[address as usize]
    }
}

impl Default for Wram {
    fn default() -> Self {
        Self::new()
    }
}

impl Wram {
    /// All zero: the title screen, since nothing has set the game-timer bit.
    pub fn new() -> Self {
        Self { bytes: vec![0; 0x1_0000] }
    }

    pub fn set(&mut self, address: u16, value: u8) -> &mut Self {
        self.bytes[address as usize] = value;
        self
    }

    /// A big-endian 16-bit quantity, which is how the cartridge stores HP.
    pub fn set_word_be(&mut self, address: u16, value: u16) -> &mut Self {
        self.set(address, (value >> 8) as u8).set(address + 1, (value & 0xff) as u8)
    }

    pub fn peek(&self, address: u16) -> u8 {
        self.bytes[address as usize]
    }

    /// One byte of the screen's tile buffer.
    pub fn screen_tile(&mut self, x: u16, y: u16, tile: u8) -> &mut Self {
        self.set(ram::wTileMap + y * poke::SCREEN_WIDTH + x, tile)
    }

    /// Fill the whole screen buffer with one tile id.
    pub fn fill_screen(&mut self, tile: u8) -> &mut Self {
        for index in 0..poke::SCREEN_WIDTH * poke::SCREEN_HEIGHT {
            self.set(ram::wTileMap + index, tile);
        }
        self
    }

    /// Draw a `TextBoxBorder` box: the four corners are what the detector looks at, and the edges
    /// are drawn too so the pattern is the one the game leaves behind.
    pub fn draw_box(&mut self, left: u16, top: u16, right: u16, bottom: u16) -> &mut Self {
        for x in left..=right {
            self.screen_tile(x, top, poke::frame::HORIZONTAL);
            self.screen_tile(x, bottom, poke::frame::HORIZONTAL);
        }
        for y in top..=bottom {
            self.screen_tile(left, y, poke::frame::VERTICAL);
            self.screen_tile(right, y, poke::frame::VERTICAL);
        }
        self.screen_tile(left, top, poke::frame::TOP_LEFT);
        self.screen_tile(right, top, poke::frame::TOP_RIGHT);
        self.screen_tile(left, bottom, poke::frame::BOTTOM_LEFT);
        self.screen_tile(right, bottom, poke::frame::BOTTOM_RIGHT);
        self
    }

    /// The game has started: `MainMenu`'s game-timer bit.
    pub fn started(&mut self) -> &mut Self {
        self.set(ram::wStatusFlags6, poke::BIT_GAME_TIMER_COUNTING)
    }

    /// A loaded map: id, size in blocks, and the player's coordinates in tiles.
    pub fn map(&mut self, id: u8, blocks_wide: u8, blocks_high: u8, x: u8, y: u8) -> &mut Self {
        self.set(ram::wCurMap, id)
            .set(ram::wCurMapWidth, blocks_wide)
            .set(ram::wCurMapHeight, blocks_high)
            .set(ram::wXCoord, x)
            .set(ram::wYCoord, y)
    }

    /// Point `wTilesetCollisionPtr` at a real collision list, written where the cartridge keeps it.
    pub fn house_collision(&mut self) -> &mut Self {
        for (offset, tile) in REDS_HOUSE_COLL.iter().enumerate() {
            self.set(REDS_HOUSE_COLL_ADDRESS + offset as u16, *tile);
        }
        self.set(REDS_HOUSE_COLL_ADDRESS + REDS_HOUSE_COLL.len() as u16, 0xff);
        self.set(ram::wTilesetCollisionPtr, (REDS_HOUSE_COLL_ADDRESS & 0xff) as u8)
            .set(ram::wTilesetCollisionPtr + 1, (REDS_HOUSE_COLL_ADDRESS >> 8) as u8)
    }

    /// Write a map tile's id into the screen buffer at the position the game would hold it, given
    /// where the player is. Silently does nothing for a tile outside the screen's window, which is
    /// exactly the tile the accessor must report as unknown.
    pub fn map_tile(&mut self, x: u8, y: u8, tile: u8) -> &mut Self {
        let player_x = self.peek(ram::wXCoord);
        let player_y = self.peek(ram::wYCoord);
        let screen_x = poke::PLAYER_SCREEN_X + 2 * (i32::from(x) - i32::from(player_x));
        let screen_y = poke::PLAYER_SCREEN_Y + 2 * (i32::from(y) - i32::from(player_y));
        if (0..poke::SCREEN_WIDTH as i32).contains(&screen_x)
            && (0..poke::SCREEN_HEIGHT as i32).contains(&screen_y)
        {
            self.screen_tile(screen_x as u16, screen_y as u16, tile);
        }
        self
    }

    /// The player's facing, in sprite slot 0.
    pub fn facing(&mut self, sprite_facing: u8) -> &mut Self {
        self.set(ram::wSpriteStateData1 + 9, sprite_facing)
    }

    /// One party member, written into its 44-byte `party_struct`.
    #[allow(clippy::too_many_arguments)]
    pub fn party_mon(
        &mut self,
        slot: u8,
        species: u8,
        level: u8,
        hp: u16,
        max_hp: u16,
        status: u8,
        moves: &[(u8, u8)],
    ) -> &mut Self {
        let base = ram::wPartyMon1 + u16::from(slot) * poke::PARTY_MON_BYTES;
        self.set(ram::wPartySpecies + u16::from(slot), species);
        self.set(base, species)
            .set_word_be(base + 1, hp)
            .set(base + 4, status)
            .set(base + 33, level)
            .set_word_be(base + 34, max_hp);
        for (index, (id, pp)) in moves.iter().enumerate().take(4) {
            self.set(base + 8 + index as u16, *id).set(base + 29 + index as u16, *pp);
        }
        let count = self.peek(ram::wPartyCount).max(slot + 1);
        self.set(ram::wPartyCount, count)
    }

    /// The active battler's copy of a party entry.
    #[allow(clippy::too_many_arguments)]
    pub fn battle_mon(
        &mut self,
        slot: u8,
        species: u8,
        level: u8,
        hp: u16,
        max_hp: u16,
        status: u8,
        moves: &[(u8, u8)],
    ) -> &mut Self {
        self.set(ram::wPlayerMonNumber, slot)
            .set(ram::wBattleMonSpecies, species)
            .set(ram::wBattleMonLevel, level)
            .set_word_be(ram::wBattleMonHP, hp)
            .set_word_be(ram::wBattleMonMaxHP, max_hp)
            .set(ram::wBattleMonStatus, status);
        for (index, (id, pp)) in moves.iter().enumerate().take(4) {
            self.set(ram::wBattleMonMoves + index as u16, *id)
                .set(ram::wBattleMonPP + index as u16, *pp);
        }
        self.set(ram::wNumMovesMinusOne, moves.len().clamp(1, 4) as u8 - 1)
    }

    pub fn enemy_mon(&mut self, species: u8, level: u8, hp: u16, max_hp: u16) -> &mut Self {
        self.set(ram::wEnemyMonSpecies, species)
            .set(ram::wEnemyMonLevel, level)
            .set_word_be(ram::wEnemyMonHP, hp)
            .set_word_be(ram::wEnemyMonMaxHP, max_hp)
    }

    /// `HandleMenuInput`'s state.
    pub fn cursor(&mut self, top_y: u8, top_x: u8, current: u8, max: u8, keys: u8) -> &mut Self {
        self.set(ram::wTopMenuItemY, top_y)
            .set(ram::wTopMenuItemX, top_x)
            .set(ram::wCurrentMenuItem, current)
            .set(ram::wMaxMenuItem, max)
            .set(ram::wMenuWatchedKeys, keys)
    }

    /// The four corner tiles of a box and nothing else, which is what a map can look like.
    ///
    /// The overworld tilesets use the frame's own tile ids for ordinary ground, so these four
    /// screen positions hold them from time to time — measured at 315 frames of 43,004 on the
    /// cartridge (`infra/docs/macros-traps.md`, 2026-09-17). This is that pattern, for a test that
    /// the detector is not fooled by it.
    pub fn draw_box_corners(&mut self, left: u16, top: u16, right: u16, bottom: u16) -> &mut Self {
        self.screen_tile(left, top, poke::frame::TOP_LEFT);
        self.screen_tile(right, top, poke::frame::TOP_RIGHT);
        self.screen_tile(left, bottom, poke::frame::BOTTOM_LEFT);
        self.screen_tile(right, bottom, poke::frame::BOTTOM_RIGHT);
        self
    }

    /// A text display is open, with the bottom-of-screen dialogue box drawn.
    pub fn dialogue_box(&mut self) -> &mut Self {
        self.set(ram::wFontLoaded, poke::BIT_FONT_LOADED).draw_box(0, 12, 19, 17)
    }

    /// The start menu, Pokédex entry included.
    pub fn start_menu(&mut self) -> &mut Self {
        self.set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
            .draw_box(10, 0, 19, 15)
            .cursor(2, 11, 0, 7, poke::pad::DOWN | poke::pad::UP | poke::pad::START | poke::pad::B | poke::pad::A)
    }

    /// A wild or trainer battle, with no menu up yet.
    pub fn battle(&mut self, is_in_battle: u8) -> &mut Self {
        self.set(ram::wIsInBattle, is_in_battle)
            .set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
    }

    /// The top-level battle menu, in the left column (FIGHT / PKMN) or the right (ITEM / RUN).
    pub fn battle_menu(&mut self, right_column: bool, current: u8) -> &mut Self {
        let (x, keys) = if right_column {
            (15, poke::pad::LEFT | poke::pad::A)
        } else {
            (9, poke::pad::RIGHT | poke::pad::A)
        };
        self.set(ram::wTextBoxID, poke::BATTLE_MENU_TEMPLATE).cursor(14, x, current, 1, keys)
    }

    /// The move list, `MoveSelectionMenu`'s regular menu. `slot` is the 0-based move.
    pub fn move_menu(&mut self, slot: u8, moves: u8) -> &mut Self {
        self.set(ram::wNumMovesMinusOne, moves.saturating_sub(1)).cursor(
            12,
            5,
            slot + 1,
            moves + 1,
            poke::pad::UP | poke::pad::DOWN | poke::pad::A,
        )
    }

    /// The party list. `forced` is the state `ChooseNextMon` leaves: A only, no way out.
    pub fn party_list(&mut self, current: u8, forced: bool) -> &mut Self {
        let count = self.peek(ram::wPartyCount).max(1);
        let keys = if forced { poke::pad::A } else { poke::pad::A | poke::pad::B };
        self.set(
            ram::wPartyMenuTypeOrMessageID,
            if forced { poke::BATTLE_PARTY_MENU } else { 0 },
        )
        .set(ram::wFontLoaded, poke::BIT_FONT_LOADED)
        .cursor(1, 0, current, count - 1, keys)
    }

    /// Money, as three bytes of big-endian BCD.
    pub fn money(&mut self, amount: u32) -> &mut Self {
        let digits = amount.min(999_999);
        let bcd = |value: u32| ((value / 10) << 4 | (value % 10)) as u8;
        self.set(ram::wPlayerMoney, bcd(digits / 10_000))
            .set(ram::wPlayerMoney + 1, bcd(digits / 100 % 100))
            .set(ram::wPlayerMoney + 2, bcd(digits % 100))
    }

    /// The bag, as `(id, quantity)` pairs and a `$ff` terminator.
    pub fn bag(&mut self, items: &[(u8, u8)]) -> &mut Self {
        self.set(ram::wNumBagItems, items.len() as u8);
        for (index, (id, count)) in items.iter().enumerate() {
            self.set(ram::wBagItems + index as u16 * 2, *id)
                .set(ram::wBagItems + index as u16 * 2 + 1, *count);
        }
        self.set(ram::wBagItems + items.len() as u16 * 2, 0xff)
    }

    /// One NPC sprite, in the slots the game keeps it: picture id and facing in
    /// `wSpriteStateData1`, map coordinates plus four in `wSpriteStateData2`.
    pub fn npc(&mut self, slot: u8, picture: u8, x: u8, y: u8, sprite_facing: u8) -> &mut Self {
        let data1 = ram::wSpriteStateData1 + u16::from(slot) * poke::SPRITE_BYTES;
        let data2 = ram::wSpriteStateData2 + u16::from(slot) * poke::SPRITE_BYTES;
        self.set(data1, picture)
            .set(data1 + 2, 0)
            .set(data1 + 9, sprite_facing)
            .set(data2 + 4, y + poke::SPRITE_COORD_BIAS)
            .set(data2 + 5, x + poke::SPRITE_COORD_BIAS);
        let count = self.peek(ram::wNumSprites).max(slot);
        self.set(ram::wNumSprites, count)
    }

    /// The current map's sign table: `bg_event`s, `Y, X` per entry with no bias, and a text id
    /// each.
    pub fn signs(&mut self, signs: &[(u8, u8, u8)]) -> &mut Self {
        self.set(ram::wNumSigns, signs.len() as u8);
        for (index, (x, y, text_id)) in signs.iter().enumerate() {
            let coords = ram::wSignCoords + index as u16 * 2;
            self.set(coords, *y).set(coords + 1, *x).set(ram::wSignTextIDs + index as u16, *text_id);
        }
        self
    }

    /// The current map's warp table.
    pub fn warps(&mut self, warps: &[(u8, u8, u8, u8)]) -> &mut Self {
        self.set(ram::wNumberOfWarps, warps.len() as u8);
        for (index, (x, y, destination_warp, destination_map)) in warps.iter().enumerate() {
            let entry = ram::wWarpEntries + index as u16 * 4;
            self.set(entry, *y)
                .set(entry + 1, *x)
                .set(entry + 2, *destination_warp)
                .set(entry + 3, *destination_map);
        }
        self
    }

    /// A playable overworld frame: Red's ground floor, the fly standing where a cold boot's walk
    /// out of the bedroom lands it, every tile a wall until a test opens one.
    pub fn overworld() -> Self {
        let mut wram = Self::new();
        wram.started()
            .map(REDS_HOUSE_1F, 4, 4, 3, 6)
            .facing(0)
            .house_collision()
            .fill_screen(WALL_TILE);
        wram
    }
}
