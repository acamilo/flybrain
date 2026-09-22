//! The decoder, against a made-up tileset.
//!
//! Every byte here is synthetic on purpose: a block table, a blockset and a collision list of this
//! module's own making, so a failure says which of the three readings is wrong rather than "the
//! cartridge disagrees". The cartridge half — that the decode matches real presses on Pallet Town
//! and Viridian Forest — is `tests/rom_map_grid.rs`.

use super::*;
use crate::pokemon_red::macros::state::{Facing, Walkable};

/// A blockset with three blocks: all floor, all wall, and one whose four map-tile quadrants are
/// four different tile ids.
///
/// A block is four screen tiles each way and a map tile is two, so the quadrants are the four
/// corners and the tile a map tile's walkability comes from is the lower left of its own quadrant
/// ([`ANCHOR_ROW`], which the cartridge settled).
fn blockset() -> Vec<u8> {
    let floor = [FLOOR; 16];
    let wall = [WALL; 16];
    // The four ids sit on the rows the decode reads -- the *lower* row of each 2x2 quadrant
    // (`ANCHOR_ROW`) -- and the rows it does not read hold ids that would be wrong answers.
    let quadrants = [
        0x90, 0x91, 0x92, 0x93, //
        NORTH_WEST, 0x94, NORTH_EAST, 0x95, //
        0x96, 0x97, 0x98, 0x99, //
        SOUTH_WEST, 0x9a, SOUTH_EAST, 0x9b,
    ];
    let mut out = Vec::new();
    out.extend_from_slice(&floor);
    out.extend_from_slice(&wall);
    out.extend_from_slice(&quadrants);
    out
}

const FLOOR: u8 = 0x01;
const WALL: u8 = 0x60;
const NORTH_WEST: u8 = 0x11;
const NORTH_EAST: u8 = 0x12;
const SOUTH_WEST: u8 = 0x13;
const SOUTH_EAST: u8 = 0x14;

/// Floor and the four quadrant tiles are passable; the wall tile is in no list.
fn tileset(id: u8) -> Tileset {
    Tileset {
        id,
        blocks: blockset(),
        passable: vec![FLOOR, NORTH_WEST, NORTH_EAST, SOUTH_WEST, SOUTH_EAST, TERMINATOR],
    }
}

#[test]
fn one_block_becomes_four_map_tiles_from_its_four_quadrants() {
    let grid = decode(7, 1, 1, &[2], &tileset(0));
    assert_eq!((grid.map(), grid.width(), grid.height()), (7, 2, 2));
    assert_eq!(grid.tile_id(0, 0), Some(NORTH_WEST));
    assert_eq!(grid.tile_id(1, 0), Some(NORTH_EAST));
    assert_eq!(grid.tile_id(0, 1), Some(SOUTH_WEST));
    assert_eq!(grid.tile_id(1, 1), Some(SOUTH_EAST));
}

#[test]
fn the_collision_list_answers_every_tile_of_a_map_larger_than_the_window() {
    // Ten blocks by nine is twenty tiles by eighteen: wider than the ten-by-nine window the
    // screen buffer can answer for, which is the point of the whole grid.
    let (wide, high) = (10u8, 9u8);
    let mut blocks = vec![0u8; usize::from(wide) * usize::from(high)];
    // A wall down the middle column of blocks.
    for row in 0..usize::from(high) {
        blocks[row * usize::from(wide) + 5] = 1;
    }
    let grid = decode(0, wide, high, &blocks, &tileset(0));
    assert_eq!((grid.width(), grid.height()), (20, 18));
    assert_eq!(grid.walkable(0, 0), Walkable::Yes);
    assert_eq!(grid.walkable(19, 17), Walkable::Yes, "the far corner, which no window reaches");
    assert_eq!(grid.walkable(10, 9), Walkable::No);
    assert_eq!(grid.walkable(11, 9), Walkable::No);
    assert_eq!(grid.unknown_count(), 0);
    assert_eq!(grid.walkable_count(), 20 * 18 - 18 * 2);
    // Off the map is not a tile to stand on, which is the window predicate's own answer for it.
    assert_eq!(grid.walkable(20, 0), Walkable::No);
    assert_eq!(grid.walkable(0, 18), Walkable::No);
}

#[test]
fn a_tile_pair_collision_is_a_wall_in_both_directions_and_only_in_its_own_tileset() {
    // `FOREST, $30, $2E`: two tiles that are each passable on their own and that the cartridge
    // refuses the step between (`data/tilesets/pair_collision_tile_ids.asm`).
    let (one, other) = (0x30, 0x2e);
    let tiles = Tileset {
        id: tileset::FOREST,
        blocks: [[one; 16], [other; 16]].concat(),
        passable: vec![one, other, TERMINATOR],
    };
    let grid = decode(0, 2, 1, &[0, 1], &tiles);
    assert_eq!(grid.walkable(1, 0), Walkable::Yes);
    assert_eq!(grid.walkable(2, 0), Walkable::Yes);
    assert!(grid.walled(1, 0, Facing::Right), "the step onto the other tile");
    assert!(grid.walled(2, 0, Facing::Left), "and the step back, which is the same rule");
    assert!(!grid.walled(0, 0, Facing::Right), "two tiles of the same id are not a pair");

    // The same two tile ids in a tileset the list does not name are ordinary ground.
    let elsewhere = Tileset { id: tileset::CAVERN, ..tiles };
    let grid = decode(0, 2, 1, &[0, 1], &elsewhere);
    assert!(!grid.walled(1, 0, Facing::Right));
    assert!(!grid.walled(2, 0, Facing::Left));
}

#[test]
fn a_block_id_the_blockset_does_not_reach_stays_unknown() {
    // A blockset read short — a header pointer near the end of its bank — is not a tile the game
    // is unsure about, and the search prices `Unknown` as plausible ground rather than refusing
    // it, so saying so is the honest answer.
    let grid = decode(0, 2, 1, &[0, 9], &tileset(0));
    assert_eq!(grid.walkable(0, 0), Walkable::Yes);
    assert_eq!(grid.walkable(2, 0), Walkable::Unknown);
    assert_eq!(grid.tile_id(2, 0), None);
    // Block 9 is one block, which is four map tiles: two rows of two.
    assert_eq!(grid.unknown_count(), 4);
}

#[test]
fn reachable_from_counts_what_a_walk_can_get_to_and_not_what_it_can_see() {
    // Two floor blocks with a wall block between them: eight walkable tiles, four of them fenced
    // off from the fly. This is the number that tells a stalled walk from a long one.
    let grid = decode(0, 3, 1, &[0, 1, 0], &tileset(0));
    assert_eq!(grid.walkable_count(), 8);
    assert_eq!(grid.reachable_from(0, 0), 4);
    assert_eq!(grid.reachable_from(4, 0), 4);

    // With the wall gone, the whole row is one region.
    let grid = decode(0, 3, 1, &[0, 0, 0], &tileset(0));
    assert_eq!(grid.reachable_from(0, 0), 12);
}

#[test]
fn a_directed_wall_fences_a_region_off_for_the_reachable_count_too() {
    let (one, other) = (0x30, 0x2e);
    let tiles = Tileset {
        id: tileset::FOREST,
        blocks: [[one; 16], [other; 16]].concat(),
        passable: vec![one, other, TERMINATOR],
    };
    // A column of `$30` and a column of `$2e`, four tiles each, with the tile-pair rule between
    // every pair of them: every tile is walkable and half of them are unreachable.
    let grid = decode(0, 2, 1, &[0, 1], &tiles);
    assert_eq!(grid.walkable_count(), 8);
    assert_eq!(grid.reachable_from(0, 0), 4);
}

#[test]
fn the_cache_holds_one_map_and_drops_it_on_arrival_somewhere_else() {
    let mut grids = MapGrids::default();
    assert_eq!(grids.held(), None);
    let stored = grids.store(decode(3, 2, 2, &[0, 0, 0, 0], &tileset(0)));
    assert_eq!(grids.held(), Some(3));
    assert_eq!(grids.get(3, 4, 4).as_deref(), Some(&*stored));
    // The same map at another size is another map's block data under the same id, which is what a
    // half-loaded header looks like.
    assert!(grids.get(3, 8, 8).is_none());
    assert!(grids.get(4, 4, 4).is_none());
    grids.store(decode(4, 1, 1, &[0], &tileset(0)));
    assert_eq!(grids.held(), Some(4));
    assert!(grids.get(3, 4, 4).is_none());
}
