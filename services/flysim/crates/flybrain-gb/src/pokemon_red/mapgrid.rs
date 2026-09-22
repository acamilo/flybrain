//! The whole current map's walkability, decoded from the tables the cartridge has loaded.
//!
//! `docs/design/macros.md` section 15, the operator 2026-09-22: "the frontier and warp macros need
//! to be map aware: A* over walkable tiles." [`super::state::walkable`] answers for the ten-by-nine
//! window of the screen buffer and [`super::macros::state::Walkable::Unknown`] for everything
//! else, which is honest and is also why every walk planned through guesses, re-planned at each
//! window edge, and called "frontier" whatever unstood ground happened to be on screen.
//!
//! This module is the same predicate over the whole map. Nothing about the *rule* changes -- a tile
//! is walkable when the current tileset's collision list holds its tile id, which is
//! `CheckTilePassable` -- what changes is where the tile id comes from:
//!
//! | what | where the cartridge keeps it | how it is read |
//! | --- | --- | --- |
//! | the map's blocks | `wOverworldMap`, one byte per 4x4-tile block, rows of `width + MAP_BORDER * 2` with the map three rows and three columns in (`LoadTileBlockMap`) | WRAM, through the ordinary reader |
//! | a block's tiles | the tileset header's blockset, 16 bytes per block id, four rows of four tile ids (`DrawTileBlock`) | ROM, through [`crate::adapter::MemoryReader::read_rom`], because the blockset is not in bank 0 |
//! | which tiles are passable | `wTilesetCollisionPtr`, a `$ff`-terminated list in bank 0 | WRAM pointer, ROM bank 0 read, exactly as the window predicate already did |
//! | which steps are refused between two passable tiles | `TilePairCollisionsLand`, keyed by `wCurMapTileset` | the table's values, quoted below |
//!
//! Two coordinate systems meet here and keeping them apart is the whole of the arithmetic. The
//! cartridge's *blocks* are 4x4 screen tiles; the player moves in *map tiles* of 2x2 screen tiles,
//! which is the unit `wXCoord`, the warp table and everything in `macros/` is in. So one block is
//! [`TILES_PER_BLOCK`] map tiles each way, and the screen tile a map tile's walkability is read
//! from is the top left of its 2x2 quadrant -- the same corner `_GetTileAndCoordsInFrontOfPlayer`
//! reads at screen `(8, 9)` for the tile the player stands on, which is what
//! [`super::state::map_grid`]'s cross-check against the window predicate proves on a cartridge.
//!
//! What it does **not** model is what it did not model before: sprites standing on ground (the
//! sprite list answers that, and [`super::macros::path::frontier`] reads it), warps that fire on
//! the step onto them, and scripts that push the fly off a tile (a session ledger answers that).

use std::sync::Arc;

use super::macros::state::{Facing, MapGrid, Walkable};

/// Screen tiles across and down one block: `BLOCK_WIDTH` and `BLOCK_HEIGHT`
/// (`constants/gfx_constants.asm`).
pub const BLOCK_TILES: usize = 4;

/// Bytes one block takes in a tileset's blockset: its sixteen tile ids, four rows of four.
pub const BLOCK_BYTES: usize = BLOCK_TILES * BLOCK_TILES;

/// Blocks of border `wOverworldMap` keeps on every side: `MAP_BORDER`
/// (`constants/map_data_constants.asm`), which is what lets the view centre on a map smaller than
/// the screen and what a map's own blocks are offset by.
pub const MAP_BORDER: usize = 3;

/// Map tiles across one block. A block is four screen tiles and the player walks two at a time,
/// which is why `wCurMapWidth` in blocks is `map_size().width` in tiles divided by this.
pub const TILES_PER_BLOCK: u8 = 2;

/// Bytes `wOverworldMap` has for the loaded map: `ds 1300` (`ram/wram.asm`).
///
/// A map plus its border of [`MAP_BORDER`] blocks has to fit in this, and every real map does. A
/// header that says otherwise is a header read mid-load, which is why the bound is a refusal
/// rather than a clamp.
pub const OVERWORLD_MAP_BYTES: usize = 1300;

/// Which of a quadrant's two rows the collision read takes its tile id from: the lower one.
///
/// A map tile is a 2x2 patch of screen tiles and only one of the four is ever asked about, because
/// `CheckTilePassable` matches a single tile id. Which one is **measured, not derived**: the
/// decoded ids were compared against the screen buffer on the cartridge, tile by tile, and the
/// screen agrees with the lower-left tile of each quadrant and not the upper-left -- Viridian
/// Forest's (4, 32) reads `$23`, the second row of its block, where the first row holds `$04`
/// (`tests/rom_map_grid.rs`, which is the test that pins it).
///
/// That is the same corner `_GetTileAndCoordsInFrontOfPlayer` reads at screen `(8, 9)` for the
/// tile the player is standing on: the view is aligned so that the player's own 2x2 begins on
/// screen row 8, so row 9 is its lower half. An upper-left decode still answers, and answers
/// plausibly -- on the open ground of a town most quadrants hold one tile id four times over --
/// which is why the cross-check in [`super::state::map_grid`] is not optional.
const ANCHOR_ROW: usize = 1;

/// Tileset ids the tile-pair lists name (`constants/tileset_constants.asm`, counted in the order
/// that file declares them: OVERWORLD 0 … FOREST 3 … CAVERN 17).
pub mod tileset {
    pub const FOREST: u8 = 3;
    pub const CAVERN: u8 = 17;
}

/// `TilePairCollisionsLand` at the pinned pokered commit, as `(tileset, one tile, the other)`.
///
/// `data/tilesets/pair_collision_tile_ids.asm`. Values rather than an address, like the item
/// prices in [`super::macros::cartridge`]: what the cartridge keeps here is eleven triples, and
/// the file they came from is quoted so a wrong one is a review comment rather than a mystery.
///
/// `CheckForTilePairCollisions` walks the list comparing the tile the player stands on against
/// *either* member of the pair and the tile in front against the other, so the refusal is
/// symmetric -- a forest's tree line refuses the step in both directions -- and each triple
/// becomes two directed walls. Both tiles are passable on their own, which is why nothing in the
/// collision list can predict it and why the walk used to learn it one refusal at a time
/// ([`super::macros::path::Refusal`]).
///
/// `TilePairCollisionsWater` is deliberately absent: it is the list
/// `CheckForJumpingAndTilePairCollisions` uses while surfing, the fly has no Surf, and a rule for
/// a movement mode the palette cannot enter is not knowledge this grid should carry.
pub const TILE_PAIRS_LAND: [(u8, u8, u8); 11] = [
    (tileset::CAVERN, 0x20, 0x05),
    (tileset::CAVERN, 0x41, 0x05),
    (tileset::FOREST, 0x30, 0x2e),
    (tileset::CAVERN, 0x2a, 0x05),
    (tileset::CAVERN, 0x05, 0x21),
    (tileset::FOREST, 0x52, 0x2e),
    (tileset::FOREST, 0x55, 0x2e),
    (tileset::FOREST, 0x56, 0x2e),
    (tileset::FOREST, 0x20, 0x2e),
    (tileset::FOREST, 0x5e, 0x2e),
    (tileset::FOREST, 0x5f, 0x2e),
];

/// The tileset's two tables, as the decoder needs them.
///
/// Gathered by [`super::state::map_grid`] from WRAM and ROM; separate from the decoding so that
/// the decoding is a pure function of bytes and can be tested against a made-up tileset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tileset {
    /// `wCurMapTileset`: which tileset, for the tile-pair lists.
    pub id: u8,
    /// The blockset: sixteen tile ids per block id, in block-id order from zero.
    pub blocks: Vec<u8>,
    /// The `$ff`-terminated passable-tile list, terminator included or not.
    pub passable: Vec<u8>,
}

impl Tileset {
    /// The screen tile id at `(column, row)` of block `block`, or `None` past the blockset.
    fn tile(&self, block: u8, column: usize, row: usize) -> Option<u8> {
        let offset = usize::from(block) * BLOCK_BYTES + row * BLOCK_TILES + column;
        self.blocks.get(offset).copied()
    }

    /// Whether the collision list holds `tile`, which is `CheckTilePassable` and nothing else.
    fn passable(&self, tile: u8) -> bool {
        for candidate in &self.passable {
            if *candidate == TERMINATOR {
                return false;
            }
            if *candidate == tile {
                return true;
            }
        }
        false
    }
}

/// The `$ff` that ends a collision list.
pub const TERMINATOR: u8 = 0xff;

/// Decode the whole map into a [`MapGrid`].
///
/// `blocks` is the map's block ids, row-major, `width_blocks * height_blocks` of them, already
/// lifted out of `wOverworldMap`'s bordered rows. Every map tile gets one answer:
/// [`Walkable::Yes`] or [`Walkable::No`] from the collision list, and [`Walkable::Unknown`] only
/// for a tile whose block id is past the end of the blockset -- which is a table that was read
/// short rather than a tile the game is unsure about.
pub fn decode(map: u8, width_blocks: u8, height_blocks: u8, blocks: &[u8], tiles: &Tileset) -> MapGrid {
    let width = width_blocks.saturating_mul(TILES_PER_BLOCK);
    let height = height_blocks.saturating_mul(TILES_PER_BLOCK);
    let mut grid = MapGrid::new(map, width, height);
    for y in 0..height {
        for x in 0..width {
            let block_index =
                usize::from(y / TILES_PER_BLOCK) * usize::from(width_blocks)
                    + usize::from(x / TILES_PER_BLOCK);
            let Some(block) = blocks.get(block_index).copied() else {
                continue;
            };
            // The screen tile the collision read uses, inside the map tile's own 2x2 quadrant of
            // the block: the **lower** left one ([`ANCHOR_ROW`]).
            let column = usize::from(x % TILES_PER_BLOCK) * usize::from(TILES_PER_BLOCK);
            let row = usize::from(y % TILES_PER_BLOCK) * usize::from(TILES_PER_BLOCK) + ANCHOR_ROW;
            let Some(tile) = tiles.tile(block, column, row) else {
                continue;
            };
            grid.set(x, y, tile, if tiles.passable(tile) { Walkable::Yes } else { Walkable::No });
        }
    }
    add_pair_walls(&mut grid, tiles.id);
    grid
}

/// Turn every tile-pair collision the loaded tileset has into two directed walls.
fn add_pair_walls(grid: &mut MapGrid, tileset: u8) {
    let pairs: Vec<(u8, u8)> = TILE_PAIRS_LAND
        .iter()
        .filter(|(id, _, _)| *id == tileset)
        .map(|(_, one, other)| (*one, *other))
        .collect();
    if pairs.is_empty() {
        return;
    }
    for y in 0..grid.height() {
        for x in 0..grid.width() {
            let Some(here) = grid.tile_id(x, y) else { continue };
            for facing in [Facing::Down, Facing::Right] {
                let (dx, dy) = facing.delta();
                let Some(nx) = checked_step(x, dx) else { continue };
                let Some(ny) = checked_step(y, dy) else { continue };
                if nx >= grid.width() || ny >= grid.height() {
                    continue;
                }
                let Some(there) = grid.tile_id(nx, ny) else { continue };
                if pairs.iter().any(|(one, other)| {
                    (*one == here && *other == there) || (*one == there && *other == here)
                }) {
                    grid.wall(x, y, facing);
                    grid.wall(nx, ny, opposite(facing));
                }
            }
        }
    }
}

fn checked_step(value: u8, delta: i16) -> Option<u8> {
    u8::try_from(i16::from(value) + delta).ok()
}

fn opposite(facing: Facing) -> Facing {
    match facing {
        Facing::Down => Facing::Up,
        Facing::Up => Facing::Down,
        Facing::Left => Facing::Right,
        Facing::Right => Facing::Left,
    }
}

/// The decoded grid of the map that is loaded, kept for as long as it is the loaded one.
///
/// One slot, keyed by map id and size: arriving on another map drops it, and so does a map whose
/// header reads a different size, because both mean the block data under it has been replaced.
/// A decode is a few thousand WRAM reads and a walk of the blockset, so it happens once per
/// arrival rather than once per plan -- and never per frame, which is what a precondition asking
/// for the frontier would otherwise cost.
///
/// Session state like the other ledgers of `docs/design/macros.md` section 12: owned by
/// [`super::macros::driver::PokemonPalette`], never checkpointed, and rebuilt from the cartridge
/// on the first frame after a restore.
#[derive(Debug, Clone, Default)]
pub struct MapGrids {
    current: Option<Arc<MapGrid>>,
}

impl MapGrids {
    /// The cached grid for `map` at this size, or `None` when the cache is for somewhere else.
    ///
    /// Handed out behind an [`Arc`] so that a caller that asks once a frame -- a precondition
    /// wanting to know whether the frontier is empty -- pays a refcount rather than a copy of the
    /// map.
    pub fn get(&self, map: u8, width: u8, height: u8) -> Option<Arc<MapGrid>> {
        self.current
            .as_ref()
            .filter(|grid| grid.map() == map && grid.width() == width && grid.height() == height)
            .map(Arc::clone)
    }

    /// Keep `grid`, dropping whatever map the cache held before.
    pub fn store(&mut self, grid: MapGrid) -> Arc<MapGrid> {
        Arc::clone(self.current.insert(Arc::new(grid)))
    }

    /// Which map the cache is holding, for a log line and the tests.
    pub fn held(&self) -> Option<u8> {
        self.current.as_ref().map(|grid| grid.map())
    }
}

#[cfg(test)]
mod tests;
