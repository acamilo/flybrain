//! A* over the current map's walkable tiles, and what counts as a way out of the map.
//!
//! `docs/design/macros.md` section 4: "overworld movement uses A* over the current map's walkable
//! tiles (collision from the tileset's collision table and the map blocks, warps and connections
//! from the tables the adapter already reads) with one step per tile and a per-step check that the
//! player moved; three failed steps abort with `MacroAbort::Blocked`." This module is the search
//! half of that sentence; the step timing and the moved check are in [`super::executor`], because
//! they are about frames rather than tiles.
//!
//! Two things shape the search.
//!
//! **It is multi-goal.** `GO OUT` wants the *nearest* of several doors, so the heuristic is the
//! minimum Manhattan distance to any goal. That stays admissible because a step costs one and
//! moves one tile.
//!
//! **It plans over the whole map when the map can be decoded** (`docs/design/macros.md` section
//! 15, the operator 2026-09-22: "the frontier and warp macros need to be map aware: A* over
//! walkable tiles"). [`MacroState::map_grid`] is every tile of the loaded map, walkability and
//! directed walls, decoded from the block and collision tables the cartridge has loaded
//! ([`crate::pokemon_red::mapgrid`]). With it, one plan crosses a town: `GO WARP` routes to its
//! warp tile, `GO OUT` and `GO ROUTE` to their door or connection tile, and the frontier is the
//! nearest unstood ground *anywhere on the map* rather than the nearest on screen.
//!
//! **Without it, the window is still the fallback.** Agent A's [`Walkable::Unknown`] is
//! load-bearing on a frame the grid cannot be decoded — no cartridge behind the seam, a battle or
//! a text box over the map, a header that is not loaded
//! ([`crate::pokemon_red::state::GridRefusal`] says which): the tile ids a walkability test needs
//! live in the screen buffer then, so only the tiles around the player can be answered at all. An
//! unknown tile is *expensive* to path through rather than forbidden ([`UNKNOWN_STEP`]): a
//! known-walkable way round is always preferred, and the search steps into the unknown only when
//! nothing known gets any closer. That is what a map edge six tiles away needed — it is off the
//! screen by definition, so a search that refused every unknown tile could not plan a single step
//! toward Route 1 from the middle of Pallet Town, which was half of why the fly never took it
//! (`infra/docs/macros-bench.md`, 2026-09-16).
//!
//! Either way the walk is re-planned only when the ground says so — a refusal, or the player not
//! where the plan expects it — which is [`super::executor`]'s committed route and not this
//! module's business.
//!
//! The search still has its second answer for a goal it cannot reach at all: when no goal is
//! reachable, it returns the route to the reachable tile that gets closest to one.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use super::cartridge::{
    Edge, ExitId, LAST_MAP, MacroState, TalkTarget, Tile, destination_outdoors, outdoors,
};
use super::geography;
use super::state::{Facing, MapGrid, Walkable};

/// Whether the player could stand on a tile of the current map: the grid's answer, or the
/// window's when there is no grid.
///
/// One place asks the question so that the search, the exit list and the frontier cannot disagree
/// about which reading they are on (`docs/design/macros.md` section 15).
fn walkable_at(
    state: &mut dyn MacroState,
    grid: Option<&MapGrid>,
    x: u8,
    y: u8,
) -> Walkable {
    match grid {
        Some(grid) => grid.walkable(x, y),
        None => state.walkable(x, y),
    }
}

/// What one step onto a tile the walkable predicate cannot answer for costs.
///
/// Eight, which is longer than any detour worth taking inside one screen, so a known way round
/// always wins and the unknown is only ever stepped into when nothing known gets closer. A step is
/// still a step on the ground: the cost is a preference, not a claim about the tile.
const UNKNOWN_STEP: u32 = 8;

/// A route found by [`route`]: what it leads to, and the presses that walk it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Index into the `goals` slice, or `None` for a route that only gets closer to one.
    pub goal: Option<usize>,
    /// One direction per tile. Empty only when the player already stands on a goal.
    pub steps: Vec<Facing>,
}

/// One way out of the current map: a tile to stand on, and sometimes one last press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// How the exploration ledger names this exit.
    pub id: ExitId,
    /// The tile to walk to.
    pub tile: Tile,
    /// A press to make once standing there, for an exit that does not fire on the step onto it.
    pub press: Option<Facing>,
    /// What kind of move this is: out of a building, between its floors, or on to the next area.
    pub way: Way,
    /// The map on the other side, when it is known.
    ///
    /// For a warp, the destination byte of the warp table. For a step off a map edge, the
    /// connection table's own answer ([`geography::connected`]) -- the connection *headers* carry
    /// the id in WRAM but this crate has no reviewed symbol for them, and a static table is
    /// enough for a question about which map is north of this one. `None` means nobody knows:
    /// a building's own front door, whose destination pokered writes as `LAST_MAP`, and any map
    /// the geography table has no row for.
    pub into: Option<u8>,
}

impl Exit {
    /// The map this exit leads to, `LAST_MAP` front doors resolved, or `None` when unknown.
    ///
    /// `here` is the map the fly is standing on, which is what makes a front door answerable:
    /// `LAST_MAP` means "the map outside", and which map that is, is the one thing a building
    /// knows about itself ([`geography::outdoor_of`]).
    ///
    /// This is what "visited" means for an exit (`docs/design/macros.md` section 9.2): the *map
    /// on the other side* has been stood on, never that the fly has stood on the boundary tile.
    /// An unknown destination is not visited, so an exit nobody can resolve stays a candidate,
    /// which is the exploratory answer rather than the convenient one.
    pub fn destination(&self, here: u8) -> Option<u8> {
        self.into.or_else(|| match self.way {
            Way::Exit => geography::outdoor_of(here),
            Way::Passage | Way::Route => None,
        })
    }
}

/// What a way out of the current map *is* (`docs/design/macros.md` section 9.1, the operator: "make a
/// distinction between building exit and something like stairs").
///
/// Classified from the destination map id the warp table already carries, because the three are
/// different moves in the game and one macro over all of them could only ever aim at the nearest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Way {
    /// Out of a building: the destination is an outdoor map (`LAST_MAP` included).
    Exit,
    /// Between the floors or rooms of one building: the destination is another interior map.
    Passage,
    /// On to the next area, from an outdoor map: a connected map edge, or a door into a building.
    Route,
}

/// One step the cartridge refused: the tile it was made from, and the direction it went.
///
/// A *directed* wall, which is the only shape that fits what the game encodes. Two of its rules
/// are invisible to the collision table [`super::state::walkable`] reads:
///
/// - **ledges.** `HandleLedges` (`home/overworld.asm`) matches a triple of facing, the tile the
///   player stands on and the tile in front against `LedgeTiles`, and hops the player *two* tiles
///   that way. The ledge tile itself is absent from every tileset's passable list, so the
///   predicate already calls it [`Walkable::No`] and the search never plans through one in either
///   direction — a hop is a move the walk declines to use rather than one it gets wrong.
/// - **tile-pair collisions.** `TilePairCollisionsLand` and `...Water`
///   (`data/tilesets/tile_pair_collisions.asm`) refuse a step *between* two tiles that are each
///   passable on their own — the water/land boundary, a forest's tree line, a gym's floor edge.
///   Both tiles read `Yes`, so nothing in the collision table can predict the refusal.
///
/// So the walk measures instead of guessing: a step that spent its whole window with the player
/// where it started is recorded here and the re-plan treats it as a wall **in that direction from
/// that tile only**, which is exactly what the two rules are. A person in the way is recorded the
/// same way and costs the walk one detour, which is the honest price for a wall that moves. The
/// entries are per walk and travel with a suspended route (`super::executor`), never longer: a
/// refusal is a fact about the frame it happened in.
pub type Refusal = (Tile, Facing);

/// The cheapest route to the nearest of `goals`, or failing that the one that gets closest.
///
/// The player's own tile is passable whether or not the predicate says so: the player is standing
/// on it, and a doormat the game will not let you stop on would otherwise make a route out of the
/// room impossible to plan from the tile the fly is actually on.
pub fn route(state: &mut dyn MacroState, goals: &[Tile]) -> Option<Route> {
    route_avoiding(state, goals, &[])
}

/// [`route`], with the steps this walk has already found the cartridge refuses treated as walls.
///
/// `refused` is directed ([`Refusal`]): it forbids one step out of one tile and says nothing about
/// the reverse, because a ledge and a tile-pair collision are both one-way facts. Without it the
/// search re-plans the same refused step after every failure and the walk spends its three
/// failures on one tile; with it the second plan goes round.
pub fn route_avoiding(
    state: &mut dyn MacroState,
    goals: &[Tile],
    refused: &[Refusal],
) -> Option<Route> {
    if goals.is_empty() {
        return None;
    }
    let player = state.player()?;
    let size = state.map_size()?;
    // One decode per plan, from the cache the sim loop keeps: with a grid the search is over the
    // whole map, without one it is over the ten-by-nine window as it always was.
    let grid = state.map_grid();
    let grid = grid.as_deref();
    let start = Tile::new(player.x, player.y);
    if let Some(goal) = goals.iter().position(|tile| *tile == start) {
        return Some(Route { goal: Some(goal), steps: Vec::new() });
    }
    let heuristic = |tile: Tile| goals.iter().map(|goal| tile.distance(*goal)).min().unwrap_or(0);

    // tile -> (cost so far, the tile stepped from, the direction stepped)
    let mut best: HashMap<Tile, (u32, Tile, Facing)> = HashMap::new();
    let mut open = BinaryHeap::new();
    open.push(Reverse((heuristic(start), 0u32, start)));
    best.insert(start, (0, start, Facing::Down));
    // The closest the search has got to a goal, for the approach answer.
    let mut nearest = (heuristic(start), 0u32, start);

    while let Some(Reverse((_, cost, tile))) = open.pop() {
        if best.get(&tile).is_some_and(|(known, _, _)| *known < cost) {
            continue;
        }
        if let Some(goal) = goals.iter().position(|candidate| *candidate == tile) {
            return Some(Route { goal: Some(goal), steps: unwind(&best, start, tile) });
        }
        let distance = heuristic(tile);
        if (distance, cost) < (nearest.0, nearest.1) {
            nearest = (distance, cost, tile);
        }
        for facing in super::cartridge::FACINGS {
            let Some(next) = tile.step(facing) else { continue };
            if next.x >= size.width || next.y >= size.height {
                continue;
            }
            // A step this walk has already watched the game refuse. Directed, so the way back
            // stays open.
            if refused.contains(&(tile, facing)) {
                continue;
            }
            // A step the *tables* refuse: a tile-pair collision, which is passable ground on both
            // sides and a wall between them (`mapgrid::TILE_PAIRS_LAND`). The walk used to learn
            // each of these by spending a step on it; with the grid the first plan goes round.
            if grid.is_some_and(|grid| grid.walled(tile.x, tile.y, facing)) {
                continue;
            }
            // **A tile the cartridge pushes the fly off is not a tile to walk through**, either
            // (row 37 of `infra/docs/macros-traps.md`). Excluding it as a *goal* was half the fix
            // and the measurement said so: Viridian City's (19, 9) went from 53,266 text-box
            // frames in twenty brain minutes to 12,919, because the A* still routed *across* it on
            // the way to somewhere else and the script fires on any frame the fly stands there.
            // A wall is the honest model of a tile the game will not let the fly stand on.
            if next != start && state.pushed_tile(next.x, next.y) {
                continue;
            }
            let step = match walkable_at(state, grid, next.x, next.y) {
                _ if next == start => 1,
                Walkable::Yes => 1,
                // Off the screen buffer, or a block the blockset was read short of: plausible
                // ground, priced so that anything known beats it.
                Walkable::Unknown => UNKNOWN_STEP,
                Walkable::No => continue,
            };
            let step_cost = cost + step;
            if best.get(&next).is_none_or(|(known, _, _)| step_cost < *known) {
                best.insert(next, (step_cost, tile, facing));
                open.push(Reverse((step_cost + heuristic(next), step_cost, next)));
            }
        }
    }

    // Nothing reachable is a goal. Walk to whatever gets closest, so the predicate's window has
    // somewhere new to look from.
    if nearest.2 == start {
        return None;
    }
    Some(Route { goal: None, steps: unwind(&best, start, nearest.2) })
}

/// Walk the parent links back from `tile` to `start` and reverse them into presses.
fn unwind(
    best: &HashMap<Tile, (u32, Tile, Facing)>,
    start: Tile,
    tile: Tile,
) -> Vec<Facing> {
    let mut steps = Vec::new();
    let mut cursor = tile;
    while cursor != start {
        let Some((_, from, facing)) = best.get(&cursor) else { break };
        steps.push(*facing);
        cursor = *from;
    }
    steps.reverse();
    steps
}

/// Every tile on the current map the game prints something for when it is faced.
///
/// `GO ITEM`'s targets (`docs/design/macros.md` section 3, 2026-09-16), from the two places the
/// cartridge keeps them, both already in WRAM:
///
/// - **the object sprites.** The map's `object_event` list occupies the same sixteen sprite slots
///   the villagers do, and the ones that are not people are the things on the floor and the
///   tables: item balls, the starter Pokéballs in Oak's lab, a fossil, a boulder, a sleeping
///   Snorlax. [`super::state::Npc::person`] is the split, and it is pokered's own
///   (`FIRST_STILL_SPRITE`).
/// - **the signs.** `bg_event` tiles: signposts, bookshelves, televisions, notice boards. They are
///   not sprites and not walkable, so nothing else in this module can see them, and they are the
///   half of the old `LOOK` slot that was worth keeping.
///
/// The tile reported is the thing's own tile, which is a tile to *face* rather than one to stand
/// on — an object sprite blocks its tile and a sign is part of a wall. Turning that into somewhere
/// to walk is [`super::executor`]'s job, because it is the same "stand beside it and turn" that
/// `GO NPC` already does.
///
/// Deduplicated and in tile order, so the nearest of two things on one tile is one goal and the
/// choice between equally near ones does not depend on which table they came out of.
pub fn interactables(state: &mut dyn MacroState) -> Vec<Tile> {
    let mut out: Vec<Tile> = state
        .npcs()
        .iter()
        .filter(|npc| !npc.person())
        .map(|npc| Tile::new(npc.x, npc.y))
        .collect();
    out.extend(state.signs().iter().map(|sign| Tile::new(sign.x, sign.y)));
    out.sort_unstable();
    out.dedup();
    out
}

/// Every object and sign on the map with the key the talked ledger records it under.
///
/// [`interactables`]'s own two tables, paired with their identities rather than flattened to
/// tiles: a sprite by its slot, a sign by its text id ([`TalkTarget`]). This is what makes
/// "untalked" a question about the *thing* rather than about the ground beside it.
pub fn interactable_targets(state: &mut dyn MacroState) -> Vec<(Tile, TalkTarget)> {
    let mut out: Vec<(Tile, TalkTarget)> = state
        .npcs()
        .iter()
        .filter(|npc| !npc.person())
        .map(|npc| (Tile::new(npc.x, npc.y), TalkTarget::Sprite(npc.slot)))
        .collect();
    out.extend(
        state.signs().iter().map(|sign| (Tile::new(sign.x, sign.y), TalkTarget::Sign(sign.text_id))),
    );
    out.sort_unstable();
    out.dedup();
    out
}

/// Every person on the map with the key the talked ledger records them under.
pub fn person_targets(state: &mut dyn MacroState) -> Vec<(Tile, TalkTarget)> {
    let mut out: Vec<(Tile, TalkTarget)> = state
        .npcs()
        .iter()
        .filter(|npc| npc.person())
        .map(|npc| (Tile::new(npc.x, npc.y), TalkTarget::Sprite(npc.slot)))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The people of this map the cartridge is not drawing only because they are off the screen,
/// keyed as [`person_targets`] keys the drawn ones (row 58).
///
/// Kept apart from [`person_targets`] on purpose: that list is what `GO NPC`, `TALK` and the
/// talked ledger's facing test read, and a sprite outside the window may also be a toggleable
/// object the cartridge has switched off ([`GameState::offscreen_npcs`]). The one reader is the
/// ladder's own target list, which has to know the leader is in the room before the fly can see
/// him.
///
/// [`GameState::offscreen_npcs`]: super::state::GameState::offscreen_npcs
pub fn offscreen_person_targets(state: &mut dyn MacroState) -> Vec<(Tile, TalkTarget)> {
    let mut out: Vec<(Tile, TalkTarget)> = state
        .offscreen_npcs()
        .iter()
        .filter(|npc| npc.person())
        .map(|npc| (Tile::new(npc.x, npc.y), TalkTarget::Sprite(npc.slot)))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// What is on `tile`: the thing a press at it would talk to, or `None` for bare ground.
///
/// People first, because a person standing on a sign's tile is what the press would reach.
pub fn target_at(state: &mut dyn MacroState, tile: Tile) -> Option<TalkTarget> {
    person_targets(state)
        .into_iter()
        .chain(interactable_targets(state))
        .find(|(at, _)| *at == tile)
        .map(|(_, target)| target)
}

/// Every exit the current map has: its warps, and a step off each connected edge.
///
/// Two behaviours of the cartridge are encoded here, both measured rather than assumed
/// (`docs/design/room-escape.md` section 3, "What the room actually is"):
///
/// - An interior warp — a staircase — fires on the step onto it, so it needs no extra press.
/// - A warp on the map's bottom row — a doormat — fires on the step *off* it, downward. Red's
///   ground floor has two of them side by side and "the front door needs DOWN, and only DOWN". A
///   sideways step onto a mat does nothing, which is why the press belongs to the exit rather than
///   being something the walker can leave out.
///
/// Stepping down onto a mat happens to fire it too, so a route that ends on one can finish either
/// way; the extra press is what makes it work when the route arrived sideways.
pub fn exits(state: &mut dyn MacroState) -> Vec<Exit> {
    let Some(size) = state.map_size() else { return Vec::new() };
    let Some(player) = state.player() else { return Vec::new() };
    let grid = state.map_grid();
    let grid = grid.as_deref();
    let here_outdoors = outdoors(player.map);
    let mut out = Vec::new();
    for (index, warp) in state.warps().iter().enumerate() {
        let Ok(id) = u8::try_from(index) else { continue };
        let tile = Tile::new(warp.x, warp.y);
        // Section 9.1's classification, from the destination map id and nothing else. Outdoors
        // every warp is a door into somewhere new, which is a route; indoors the destination
        // decides whether it is the way out of the building or the way to another of its floors.
        let way = if here_outdoors {
            Way::Route
        } else if destination_outdoors(warp.destination_map) {
            Way::Exit
        } else {
            Way::Passage
        };
        out.push(Exit {
            id: ExitId::Warp(id),
            tile,
            press: outward(tile, size.height),
            way,
            into: (warp.destination_map != LAST_MAP).then_some(warp.destination_map),
        });
    }
    let connections = state.connections();
    for (edge, facing) in Edge::ALL {
        let connected = match edge {
            Edge::North => connections.north,
            Edge::South => connections.south,
            Edge::East => connections.east,
            Edge::West => connections.west,
        };
        if !connected {
            continue;
        }
        // A step off the edge of an outdoor map is the next area; off an interior one -- which
        // the cartridge does not normally do -- it is still a way out of the building.
        let way = if here_outdoors { Way::Route } else { Way::Exit };
        let into = geography::connected(player.map, edge);
        for tile in edge_tiles(facing, size.width, size.height) {
            // Not `== Yes`: without a grid the walkable predicate's window is the screen, so the
            // far edge of an outdoor map reads `Unknown` from anywhere but next to it, and
            // filtering on `Yes` left a town's connections out of the exit list entirely -- which
            // is half of why nothing could ever aim at Route 1 from the middle of Pallet Town. An
            // unknown tile is a goal worth walking towards; the route search's own approach answer
            // handles a goal it cannot reach yet, and a tile that turns out to be a wall costs one
            // blocked walk. With a grid the answer is `Yes` or `No` for every edge tile of the map
            // and this rejects the walls, which is what lets one plan reach the right end of a
            // connection instead of the nearest of twenty tiles along it.
            if walkable_at(state, grid, tile.x, tile.y) != Walkable::No {
                out.push(Exit { id: ExitId::Edge(edge), tile, press: Some(facing), way, into });
            }
        }
    }
    out
}

/// Every tile of the current map that borders ground this run has never stood on.
///
/// `GO FRONTIER` (`docs/design/macros.md` section 9): "walk to the nearest tile bordering ground
/// this run has never stood on, using the exploration ledger, and face it. No random steps remain
/// anywhere in the palette."
///
/// Each answer is a tile to stand on paired with the direction the new ground lies in, which is
/// the same shape `GO NPC` and `GO ITEM` use -- and the same press, which in the overworld walks
/// onto the tile when it is walkable, so the frontier the fly is looking at becomes ground it has
/// stood on. Both tiles have to be walkable: an unreachable one is not ground.
///
/// **With a grid this is the whole map** (`docs/design/macros.md` section 15): the nearest unstood
/// walkable tile anywhere on it, which is what the operator asked for and what the route search
/// then plans one walk to. Without a grid it is what it always was -- the ten-by-nine window, so
/// the answer is local to the player and the long walk is done by re-planning.
///
/// Deduplicated by the tile to stand on, in tile order, so the choice between two equally near
/// frontiers does not depend on iteration order.
///
/// **A tile a sprite is standing on is not ground.** The collision table knows nothing about
/// people or objects — that is what the executor's per-step "did the player move" check is for —
/// so a villager standing in the open, or an item ball on a floor tile, reads [`Walkable::Yes`]
/// underneath. Such a tile can never be stood on while the sprite is there, so it can never enter
/// the visited ledger either: it is a frontier for as long as the sprite stands in it, and the
/// arrival press faces it, reports `Done` and changes nothing. The blocked ledger bounds that at
/// one walk per ten brain minutes (row 5); leaving the tile out of the list is what makes the
/// frontier *exhaust*, which is what the button leaving the pad needs. The gate house at
/// Viridian Forest's south end had one of each (`infra/docs/macros-traps.md` row 32).
pub fn frontier(state: &mut dyn MacroState) -> Vec<(Tile, Facing)> {
    let Some(size) = state.map_size() else { return Vec::new() };
    let Some(player) = state.player() else { return Vec::new() };
    let grid = state.map_grid();
    let grid = grid.as_deref();
    let here = Tile::new(player.x, player.y);
    let held: Vec<Tile> = state.npcs().iter().map(|npc| Tile::new(npc.x, npc.y)).collect();
    let mut out: Vec<(Tile, Facing)> = Vec::new();
    for y in 0..size.height {
        for x in 0..size.width {
            let tile = Tile::new(x, y);
            if tile != here && walkable_at(state, grid, x, y) != Walkable::Yes {
                continue;
            }
            if tile != here && held.contains(&tile) {
                continue;
            }
            for facing in super::cartridge::FACINGS {
                let Some(next) = tile.step(facing) else { continue };
                if next.x >= size.width || next.y >= size.height || next == here {
                    continue;
                }
                if walkable_at(state, grid, next.x, next.y) != Walkable::Yes {
                    continue;
                }
                // A step the tables refuse is not a way onto that ground, so the tile it leads to
                // is not this tile's frontier -- somebody else's, if anything reaches it.
                if grid.is_some_and(|grid| grid.walled(tile.x, tile.y, facing)) {
                    continue;
                }
                if held.contains(&next) {
                    continue;
                }
                if state.tile_visited(next.x, next.y) {
                    continue;
                }
                out.push((tile, facing));
                break;
            }
        }
    }
    out.sort_by_key(|(tile, _)| *tile);
    out.dedup_by_key(|(tile, _)| *tile);
    out
}

/// The press a warp on `tile` needs after it is stood on, or `None` when it fires by itself.
///
/// Only the bottom row answers, and it answers `Down`. That is the measured behaviour rather than
/// a rule about geometry: `docs/design/room-escape.md` section 3 walked Red's ground floor with
/// real presses and found the front door needs DOWN, while the staircase — which is against the
/// right-hand wall, so a rule about outer columns would have claimed it — fires the instant it is
/// stepped on. A warp that turns out to need some other press is not pressed into one: the walk
/// settles on it, reports `done`, and the fly chooses again.
fn outward(tile: Tile, height: u8) -> Option<Facing> {
    (height > 0 && tile.y == height - 1).then_some(Facing::Down)
}

/// Every tile on the edge a step in `facing` would cross.
fn edge_tiles(facing: Facing, width: u8, height: u8) -> Vec<Tile> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    match facing {
        Facing::Up => (0..width).map(|x| Tile::new(x, 0)).collect(),
        Facing::Down => (0..width).map(|x| Tile::new(x, height - 1)).collect(),
        Facing::Left => (0..height).map(|y| Tile::new(0, y)).collect(),
        Facing::Right => (0..height).map(|y| Tile::new(width - 1, y)).collect(),
    }
}
