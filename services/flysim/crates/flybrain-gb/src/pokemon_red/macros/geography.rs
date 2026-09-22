//! Kanto as a graph: which map is on the other side of an edge, and the way from here to there.
//!
//! `docs/design/macros.md` section 9.2. Two questions the cartridge's own WRAM cannot answer, and
//! one that it can only answer about the map that is loaded:
//!
//! - **where does this map edge go?** `wCurMapConnections` is four bits — north, south, west,
//!   east — and the connected map's *id* lives in the connection headers beside it, which this
//!   crate has no reviewed symbol for. So a step off an edge used to carry no destination at all
//!   ([`super::path::Exit::into`] was `None`), which made it invisible to every question about
//!   where the fly has already been.
//! - **which way is Viridian from here?** The warp table names the destination of each of *this*
//!   map's doors and nothing further, so "the mart is two maps north" is not a thing one map's
//!   data can say.
//!
//! Both are answered here, from a static adjacency table rather than from memory: the routes and
//! towns of `constants/map_constants.asm` with the connection tables of `data/maps/headers`, plus
//! the doors the ladder's own places need. That is enough for a breadth-first route over maps,
//! which is all `GO OBJECTIVE` asks for — it still finds its own way *across* the map it is
//! standing on with the collision data it can see.
//!
//! **An unknown entry is not a guess.** A map with no row in [`CONNECTIONS`] and no link in
//! [`LINKS`] has no known neighbours, which reads as "destination unknown": an exit into it counts
//! as *unvisited* (so `GO ROUTE` still offers it) and [`next_hop`] answers `None` (so
//! `GO OBJECTIVE` falls through to the next plan entry). Nothing invents a route it cannot justify,
//! and the table can grow one row at a time without any other behaviour moving.

use std::collections::{HashMap, HashSet, VecDeque};

use super::super::maps;
use super::cartridge::Edge;

/// A column with no connection on it.
const NONE: u8 = 0xff;

/// Compass columns of [`CONNECTIONS`], in [`Edge`]'s own order.
const NORTH: usize = 0;
const SOUTH: usize = 1;
const WEST: usize = 2;
const EAST: usize = 3;

/// Every outdoor map's connections, as north, south, west, east.
///
/// `data/maps/headers/*.asm`: each header's `connection` lines, which is the same data
/// `wCurMapConnections`' four bits are loaded from. Kanto's overworld is one grid, so the table is
/// symmetric by construction and [`neighbours`] does not rely on that — it reads both directions.
///
/// Two rows are worth a note. `ROUTE_3` and `ROUTE_4` are connected along the east-west axis even
/// though Mt. Moon stands between them, so the walkable path is the cave and not the edge; that
/// costs nothing, because an edge whose tiles are not walkable produces no exit at all
/// ([`super::path::exits`] filters on the walkable predicate) and the cave is in [`LINKS`].
const CONNECTIONS: &[(u8, [u8; 4])] = &[
    (maps::PALLET_TOWN, [maps::ROUTE_1, maps::ROUTE_21, NONE, NONE]),
    (maps::VIRIDIAN_CITY, [maps::ROUTE_2, maps::ROUTE_1, maps::ROUTE_22, NONE]),
    (maps::PEWTER_CITY, [NONE, maps::ROUTE_2, NONE, maps::ROUTE_3]),
    (maps::CERULEAN_CITY, [maps::ROUTE_24, maps::ROUTE_5, maps::ROUTE_4, maps::ROUTE_9]),
    (maps::LAVENDER_TOWN, [maps::ROUTE_10, maps::ROUTE_12, maps::ROUTE_8, NONE]),
    (maps::VERMILION_CITY, [maps::ROUTE_6, NONE, NONE, maps::ROUTE_11]),
    (maps::CELADON_CITY, [NONE, NONE, maps::ROUTE_16, maps::ROUTE_7]),
    (maps::FUCHSIA_CITY, [NONE, maps::ROUTE_19, maps::ROUTE_18, maps::ROUTE_15]),
    (maps::CINNABAR_ISLAND, [maps::ROUTE_21, NONE, NONE, maps::ROUTE_20]),
    (maps::INDIGO_PLATEAU, [NONE, maps::ROUTE_23, NONE, NONE]),
    (maps::SAFFRON_CITY, [maps::ROUTE_5, maps::ROUTE_6, maps::ROUTE_7, maps::ROUTE_8]),
    (maps::ROUTE_1, [maps::VIRIDIAN_CITY, maps::PALLET_TOWN, NONE, NONE]),
    (maps::ROUTE_2, [maps::PEWTER_CITY, maps::VIRIDIAN_CITY, NONE, NONE]),
    (maps::ROUTE_3, [NONE, NONE, maps::PEWTER_CITY, maps::ROUTE_4]),
    (maps::ROUTE_4, [NONE, NONE, maps::ROUTE_3, maps::CERULEAN_CITY]),
    (maps::ROUTE_5, [maps::CERULEAN_CITY, maps::SAFFRON_CITY, NONE, NONE]),
    (maps::ROUTE_6, [maps::SAFFRON_CITY, maps::VERMILION_CITY, NONE, NONE]),
    (maps::ROUTE_7, [NONE, NONE, maps::CELADON_CITY, maps::SAFFRON_CITY]),
    (maps::ROUTE_8, [NONE, NONE, maps::SAFFRON_CITY, maps::LAVENDER_TOWN]),
    (maps::ROUTE_9, [NONE, NONE, maps::CERULEAN_CITY, maps::ROUTE_10]),
    (maps::ROUTE_10, [NONE, maps::LAVENDER_TOWN, maps::ROUTE_9, NONE]),
    (maps::ROUTE_11, [NONE, NONE, maps::VERMILION_CITY, maps::ROUTE_12]),
    (maps::ROUTE_12, [maps::LAVENDER_TOWN, maps::ROUTE_13, maps::ROUTE_11, NONE]),
    (maps::ROUTE_13, [maps::ROUTE_12, NONE, maps::ROUTE_14, NONE]),
    (maps::ROUTE_14, [NONE, maps::ROUTE_15, NONE, maps::ROUTE_13]),
    (maps::ROUTE_15, [maps::ROUTE_14, NONE, maps::FUCHSIA_CITY, NONE]),
    (maps::ROUTE_16, [NONE, maps::ROUTE_17, NONE, maps::CELADON_CITY]),
    (maps::ROUTE_17, [maps::ROUTE_16, maps::ROUTE_18, NONE, NONE]),
    (maps::ROUTE_18, [maps::ROUTE_17, NONE, NONE, maps::FUCHSIA_CITY]),
    (maps::ROUTE_19, [maps::FUCHSIA_CITY, NONE, maps::ROUTE_20, NONE]),
    (maps::ROUTE_20, [NONE, NONE, maps::CINNABAR_ISLAND, maps::ROUTE_19]),
    (maps::ROUTE_21, [maps::PALLET_TOWN, maps::CINNABAR_ISLAND, NONE, NONE]),
    // Route 22 ends at the League gate, which is a building rather than an edge, and this
    // table has no id for it: Indigo Plateau is on the graph but not reachable from the south.
    (maps::ROUTE_22, [NONE, NONE, NONE, maps::VIRIDIAN_CITY]),
    (maps::ROUTE_23, [maps::INDIGO_PLATEAU, NONE, NONE, NONE]),
    (maps::ROUTE_24, [maps::ROUTE_25, maps::CERULEAN_CITY, NONE, NONE]),
    (maps::ROUTE_25, [NONE, maps::ROUTE_24, NONE, NONE]),
];

/// Doors and floor changes, as undirected pairs of maps.
///
/// Only the ones a rung place needs a route through, because that is all [`next_hop`] is for: an
/// unlisted building is simply not on the graph, which makes it a place `GO OBJECTIVE` cannot aim
/// at from another map and changes nothing else. A cave with two mouths appears twice, which is
/// what makes Mt. Moon a way from Route 3 to Route 4.
const LINKS: &[(u8, u8)] = &[
    (maps::REDS_HOUSE_1F, maps::PALLET_TOWN),
    (maps::REDS_HOUSE_2F, maps::REDS_HOUSE_1F),
    (maps::BLUES_HOUSE, maps::PALLET_TOWN),
    (maps::OAKS_LAB, maps::PALLET_TOWN),
    (maps::VIRIDIAN_MART, maps::VIRIDIAN_CITY),
    (maps::VIRIDIAN_POKECENTER, maps::VIRIDIAN_CITY),
    (maps::VIRIDIAN_GYM, maps::VIRIDIAN_CITY),
    // Route 2's two forest gates. Both rows say `ROUTE_2`, and which *piece* of Route 2 each one
    // opens onto is [`SPLIT`]'s business: the map is one id with two disconnected halves, and
    // without that the graph thought Pewter was two hops south of the south gate
    // (`infra/docs/macros-traps.md` row 33).
    (maps::VIRIDIAN_FOREST_SOUTH_GATE, maps::ROUTE_2),
    (maps::VIRIDIAN_FOREST_SOUTH_GATE, maps::VIRIDIAN_FOREST),
    (maps::VIRIDIAN_FOREST_NORTH_GATE, maps::ROUTE_2),
    (maps::VIRIDIAN_FOREST_NORTH_GATE, maps::VIRIDIAN_FOREST),
    (maps::PEWTER_GYM, maps::PEWTER_CITY),
    // The museum, both floors. It is on the graph for the same reason the gym is: the rung-10
    // loop of 2026-09-22 spent its hours on these two maps with `GO OBJECTIVE` off the pad
    // entirely, because a map with no row here has no neighbours, so `next_hop` answers nothing
    // and `area_of` answers nothing -- no road to the gym from indoors, and no errand either.
    // Both rows are the cartridge's own warp table, surveyed from the rung-10 checkpoint: Pewter
    // City names `$34` at (14, 7) and (19, 5), and `$34` names `$35` at (7, 7).
    (maps::PEWTER_MUSEUM_1F, maps::PEWTER_CITY),
    (maps::PEWTER_MUSEUM_2F, maps::PEWTER_MUSEUM_1F),
    (maps::PEWTER_MART, maps::PEWTER_CITY),
    (maps::PEWTER_POKECENTER, maps::PEWTER_CITY),
    (maps::MT_MOON_1F, maps::ROUTE_3),
    (maps::MT_MOON_1F, maps::ROUTE_4),
    (maps::MT_MOON_1F, maps::MT_MOON_B1F),
    (maps::MT_MOON_B1F, maps::MT_MOON_B2F),
    (maps::CERULEAN_GYM, maps::CERULEAN_CITY),
    (maps::CERULEAN_MART, maps::CERULEAN_CITY),
    (maps::CERULEAN_POKECENTER, maps::CERULEAN_CITY),
    (maps::ROCK_TUNNEL_1F, maps::ROUTE_10),
    (maps::INDIGO_PLATEAU_LOBBY, maps::INDIGO_PLATEAU),
];

/// One piece of walkable ground: a map, and which of its pieces when the map has more than one.
///
/// Almost every map in Kanto is one piece and `part` is 0 for all of them. The exception is the
/// thing row 33 of `infra/docs/macros-traps.md` is about: **`ROUTE_2` is one map id whose ground
/// is in two halves the player cannot walk between.** The south half touches Viridian City and
/// the forest's south gate; the north half touches Pewter City and the forest's north gate; the
/// belt of trees between them needs CUT. A graph with one node for it answered "Pewter is two
/// hops from the south gate, south" — which is a road that does not exist — and sent the fly back
/// out of the gate it had just walked into, once per hold, for four hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Region {
    pub map: u8,
    /// Which piece, for a map [`SPLIT`] has a row for; 0 everywhere else.
    pub part: u8,
}

impl Region {
    /// A map whose ground is all one piece, which is every map but [`SPLIT`]'s.
    pub const fn whole(map: u8) -> Self {
        Self { map, part: 0 }
    }
}

/// [`SPLIT`]'s two piece numbers.
const NORTH_PIECE: u8 = 0;
const SOUTH_PIECE: u8 = 1;

/// A map whose walkable ground is in two pieces, and which of its neighbours each piece touches.
struct Split {
    map: u8,
    /// The tile rows each piece's own doorway is on, measured from the cartridge: a tile belongs
    /// to the piece whose row it is nearer to. Anchoring on the doorways rather than on a row in
    /// the middle means the number comes from the warp table rather than from a claim about where
    /// the trees are, and a tile in the impassable belt between them — ground the fly cannot
    /// stand on — is the only place the answer could be wrong.
    north_door: u8,
    south_door: u8,
    /// The neighbours reachable from each piece. Together they are exactly [`neighbours`]'s answer
    /// for the map, which [`tests::a_split_maps_pieces_divide_its_neighbours_between_them`] pins.
    north: &'static [u8],
    south: &'static [u8],
}

/// Every map whose ground is in two pieces. One row, and it took four hours of stream to find.
///
/// `ROUTE_2`, surveyed from the cartridge on 2026-09-17 (`docs/design/macros-wram.md`'s method,
/// the run recorded in `infra/docs/macros-traps.md` row 33). The map is 20 by 72 and its warp
/// table reads:
///
/// | warp | tile | into |
/// | ---: | --- | --- |
/// | 0 | (12, 9) | `DIGLETTS_CAVE_ROUTE_2` (46) |
/// | 1 | (3, 11) | `VIRIDIAN_FOREST_NORTH_GATE` (47) |
/// | 2 | (15, 19) | `ROUTE_2_TRADE_HOUSE` (48) |
/// | 3 | (16, 35) | `ROUTE_2_GATE` (49) |
/// | 4 | (15, 39) | `ROUTE_2_GATE` (49) |
/// | 5 | (3, 43) | `VIRIDIAN_FOREST_SOUTH_GATE` (50) |
///
/// with `north: true` and `south: true` in `wCurMapConnections` — Pewter off the top row, Viridian
/// off the bottom one. The two forest gates at rows 11 and 43 are the doorways this splits on.
///
/// Maps 46, 48 and 49 are deliberately *not* on the graph, which is this module's standing rule
/// for a building no rung place needs a route through: 49's two doors are both warps of `ROUTE_2`
/// itself, so it is a shortcut within one map rather than a way between two, and 46 and 48 are
/// ends of the line. A route the table does not carry is simply not offered; nothing is guessed.
const SPLIT: &[Split] = &[Split {
    map: maps::ROUTE_2,
    north_door: 11,
    south_door: 43,
    north: &[maps::PEWTER_CITY, maps::VIRIDIAN_FOREST_NORTH_GATE],
    south: &[maps::VIRIDIAN_CITY, maps::VIRIDIAN_FOREST_SOUTH_GATE],
}];

fn split_of(map: u8) -> Option<&'static Split> {
    SPLIT.iter().find(|split| split.map == map)
}

/// The piece of `map` a tile on row `y` is in.
///
/// For every map but [`SPLIT`]'s rows this is [`Region::whole`]. Callers pass the player's own
/// row, which is the only thing that can tell the two halves of `ROUTE_2` apart.
pub fn region_at(map: u8, y: u8) -> Region {
    match split_of(map) {
        None => Region::whole(map),
        Some(split) => {
            let north = y.abs_diff(split.north_door);
            let south = y.abs_diff(split.south_door);
            Region { map, part: if north <= south { NORTH_PIECE } else { SOUTH_PIECE } }
        }
    }
}

/// The piece of `map` that `from` opens onto, or `None` when no piece of it touches `from`.
///
/// This is the reverse of [`region_at`] and it needs no tile: a door or an edge is listed under
/// exactly one piece, so "which half of Route 2 does the north gate open onto" is a table lookup.
/// `None` is an edge the graph does not have -- the south half of Route 2 is not reachable from
/// Pewter City, whatever the map ids alone would suggest.
fn region_toward(map: u8, from: u8) -> Option<Region> {
    match split_of(map) {
        None => Some(Region::whole(map)),
        Some(split) => {
            if split.north.contains(&from) {
                Some(Region { map, part: NORTH_PIECE })
            } else if split.south.contains(&from) {
                Some(Region { map, part: SOUTH_PIECE })
            } else {
                None
            }
        }
    }
}

/// Every piece of ground one step from `region`.
fn region_neighbours(region: Region) -> Vec<Region> {
    let of = |map: u8| region_toward(map, region.map);
    match split_of(region.map) {
        None => neighbours(region.map).into_iter().filter_map(of).collect(),
        Some(split) => {
            let own = if region.part == NORTH_PIECE { split.north } else { split.south };
            own.iter().copied().filter_map(of).collect()
        }
    }
}

/// The map on the other side of `map`'s `edge`, or `None` where the table does not know.
pub fn connected(map: u8, edge: Edge) -> Option<u8> {
    let column = match edge {
        Edge::North => NORTH,
        Edge::South => SOUTH,
        Edge::West => WEST,
        Edge::East => EAST,
    };
    let row = CONNECTIONS.iter().find(|(id, _)| *id == map)?;
    (row.1[column] != NONE).then_some(row.1[column])
}

/// The outdoor map a building's own front door opens onto, when the table knows.
///
/// pokered writes a front door's destination as `LAST_MAP` (`$ff`, "back where you came from"), so
/// the warp table cannot name it and this is the only place that can. Used for two things: what
/// "has the map on the other side of this door been visited" means for a `GO OUT`, and the first
/// hop of a route that starts indoors.
pub fn outdoor_of(interior: u8) -> Option<u8> {
    neighbours(interior).into_iter().find(|map| super::cartridge::outdoors(*map))
}

/// Every map one step from `map`, doors and edges together, deduplicated and in id order.
pub fn neighbours(map: u8) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    if let Some(row) = CONNECTIONS.iter().find(|(id, _)| *id == map) {
        out.extend(row.1.iter().copied().filter(|id| *id != NONE));
    }
    for (a, b) in LINKS {
        if *a == map {
            out.push(*b);
        }
        if *b == map {
            out.push(*a);
        }
    }
    // Symmetric closure: a row that names a neighbour is a connection whichever side lists it.
    for (id, row) in CONNECTIONS {
        if row.contains(&map) {
            out.push(*id);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The neighbour of `from` on a shortest route to the map `to`, or `None` when none is known.
///
/// Breadth-first over [`neighbours`], so the answer is a *map* rather than a path: the caller aims
/// at whichever of the current map's exits leads to that one hop and asks again when it arrives,
/// which is the same re-plan-every-tile shape the walk itself has. `None` for an unknown map, for
/// an unreachable one, and for `from == to` -- there is no hop to take when the fly is already
/// there, and `GO OBJECTIVE` has its own answer for that case.
pub fn next_hop(from: Region, to: u8) -> Option<u8> {
    if from.map == to {
        return None;
    }
    let mut seen: HashSet<Region> = HashSet::from([from]);
    // piece -> the first hop out of `from` that reaches it
    let mut first: HashMap<Region, u8> = HashMap::new();
    let mut queue: VecDeque<Region> = VecDeque::new();
    for hop in region_neighbours(from) {
        if seen.insert(hop) {
            first.insert(hop, hop.map);
            queue.push_back(hop);
        }
    }
    while let Some(region) = queue.pop_front() {
        let hop = *first.get(&region)?;
        if region.map == to {
            return Some(hop);
        }
        for next in region_neighbours(region) {
            if seen.insert(next) {
                first.insert(next, hop);
                queue.push_back(next);
            }
        }
    }
    None
}

/// A building an area has at most one of, and which this run may not have been into yet.
///
/// `docs/design/macros.md` section 13: the two errands. A kind rather than two parallel tables
/// because everything about them is the same shape -- one building per area, one session ledger
/// entry per area, one walk to its door -- and the only thing that differs is which precondition
/// the button carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Amenity {
    /// A Poké Mart.
    Mart,
    /// A Pokémon Center.
    Center,
}

impl Amenity {
    /// Both kinds, in the order section 13 lists them.
    pub const ALL: [Self; 2] = [Self::Mart, Self::Center];

    /// The word the log line and the tests use.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mart => "mart",
            Self::Center => "center",
        }
    }
}

/// Which mart and which Pokémon Center each area has, by the area's own map id.
///
/// The same standing rule as [`LINKS`] and [`SPLIT`]: **a row this table does not carry is not a
/// guess.** An area with no row has no errand, `GO SHOP` and `GO HEAL` are off its pad, and
/// `GO OBJECTIVE`'s errand list is empty there -- which is what the routes, Pallet Town (which
/// has neither) and every town this run cannot reach yet all are. The table grows a row at a time
/// as the ladder does, and nothing else moves when it does.
///
/// Every id in it is also a [`LINKS`] row, because a building with no way in is a place
/// `GO OBJECTIVE` can never route to ([`tests::every_amenity_is_on_the_map_graph`]).
const AMENITIES: &[(u8, Amenity, u8)] = &[
    (maps::VIRIDIAN_CITY, Amenity::Mart, maps::VIRIDIAN_MART),
    (maps::VIRIDIAN_CITY, Amenity::Center, maps::VIRIDIAN_POKECENTER),
    (maps::PEWTER_CITY, Amenity::Mart, maps::PEWTER_MART),
    (maps::PEWTER_CITY, Amenity::Center, maps::PEWTER_POKECENTER),
    (maps::CERULEAN_CITY, Amenity::Mart, maps::CERULEAN_MART),
    (maps::CERULEAN_CITY, Amenity::Center, maps::CERULEAN_POKECENTER),
];

/// The area a map belongs to: the town or route the errands are counted once per.
///
/// Outdoors a map *is* its own area. Indoors the area is the outdoor map the building's front door
/// opens onto ([`outdoor_of`]), so standing in a Viridian house, in the mart or in the gym are all
/// "in Viridian" -- which is what makes one visit per area per run a question the fly can answer
/// from wherever it is standing. `None` for a building the graph has no row for, which is most
/// houses: such a map has no area, so it offers no errand, and the errand comes back the moment
/// the fly is outside again.
pub fn area_of(map: u8) -> Option<u8> {
    if super::cartridge::outdoors(map) {
        return Some(map);
    }
    outdoor_of(map)
}

/// The map of `area`'s mart or Pokémon Center, when the table has a row for it.
pub fn amenity_of(area: u8, kind: Amenity) -> Option<u8> {
    AMENITIES
        .iter()
        .find(|(id, of, _)| *id == area && *of == kind)
        .map(|(_, _, map)| *map)
}

/// Which kind of amenity `map` *is*, when it is one.
///
/// The reverse lookup, and what tells the fly it is standing in a mart rather than in a house: the
/// shop scene's own buttons and the centre's `HEAL` are dealt on this answer rather than on a
/// tileset read, because a map id is a byte the adapter already has and a tileset is not.
pub fn amenity_at(map: u8) -> Option<Amenity> {
    AMENITIES.iter().find(|(_, _, id)| *id == map).map(|(_, kind, _)| *kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edge_knows_which_map_is_on_the_other_side() {
        assert_eq!(connected(maps::PALLET_TOWN, Edge::North), Some(maps::ROUTE_1));
        assert_eq!(connected(maps::PALLET_TOWN, Edge::South), Some(maps::ROUTE_21));
        assert_eq!(connected(maps::PALLET_TOWN, Edge::East), None, "the town has no east exit");
        assert_eq!(connected(maps::ROUTE_1, Edge::North), Some(maps::VIRIDIAN_CITY));
        // An interior has no connections at all, and an unknown map answers nothing rather than
        // guessing -- which is what keeps an exit into it a candidate.
        assert_eq!(connected(maps::OAKS_LAB, Edge::North), None);
        assert_eq!(connected(0xf0, Edge::North), None);
    }

    #[test]
    fn the_overworld_grid_agrees_with_itself() {
        // Every connection named in one row is named back in the other's, which is what makes a
        // breadth-first route reversible and a typo in the table visible.
        for (map, row) in CONNECTIONS {
            for (column, other) in row.iter().enumerate() {
                if *other == NONE {
                    continue;
                }
                let back = match column {
                    NORTH => SOUTH,
                    SOUTH => NORTH,
                    WEST => EAST,
                    _ => WEST,
                };
                let there = CONNECTIONS
                    .iter()
                    .find(|(id, _)| id == other)
                    .unwrap_or_else(|| panic!("{other:#04x} is connected to but has no row"));
                assert_eq!(
                    there.1[back], *map,
                    "{map:#04x} and {other:#04x} disagree about which way they touch"
                );
            }
        }
    }

    #[test]
    fn a_front_door_resolves_to_the_town_outside_it() {
        assert_eq!(outdoor_of(maps::OAKS_LAB), Some(maps::PALLET_TOWN));
        assert_eq!(outdoor_of(maps::REDS_HOUSE_1F), Some(maps::PALLET_TOWN));
        assert_eq!(outdoor_of(maps::VIRIDIAN_MART), Some(maps::VIRIDIAN_CITY));
        // A bedroom's only neighbour is the floor below, which is not outdoors.
        assert_eq!(outdoor_of(maps::REDS_HOUSE_2F), None);
        assert_eq!(outdoor_of(0xf0), None);
    }

    #[test]
    fn the_first_hop_of_a_route_is_the_map_to_aim_at() {
        let at = |map: u8| Region::whole(map);
        // The rung-6 errand, which is the one the town loop never took: the mart is two maps north
        // of Pallet Town, and the first hop is the connection the fly was walking past.
        assert_eq!(next_hop(at(maps::PALLET_TOWN), maps::VIRIDIAN_MART), Some(maps::ROUTE_1));
        assert_eq!(next_hop(at(maps::ROUTE_1), maps::VIRIDIAN_MART), Some(maps::VIRIDIAN_CITY));
        assert_eq!(
            next_hop(at(maps::VIRIDIAN_CITY), maps::VIRIDIAN_MART),
            Some(maps::VIRIDIAN_MART)
        );
        // Out of a building first: the lab's only way anywhere is its own front door.
        assert_eq!(next_hop(at(maps::OAKS_LAB), maps::VIRIDIAN_MART), Some(maps::PALLET_TOWN));
        // Upstairs is two hops from the town, through the ground floor.
        assert_eq!(next_hop(at(maps::PALLET_TOWN), maps::REDS_HOUSE_2F), Some(maps::REDS_HOUSE_1F));
        // Through the cave, because Route 3 and Route 4 are the same two maps either way round.
        assert_eq!(next_hop(at(maps::PEWTER_CITY), maps::CERULEAN_GYM), Some(maps::ROUTE_3));
        // Nowhere to go, and nowhere known.
        assert_eq!(next_hop(at(maps::PALLET_TOWN), maps::PALLET_TOWN), None);
        assert_eq!(next_hop(at(maps::PALLET_TOWN), 0xf0), None);
    }

    #[test]
    fn a_split_maps_pieces_divide_its_neighbours_between_them() {
        // The invariant that keeps [`SPLIT`] honest: a piece's own list is a real subset of the
        // map's neighbours, the two pieces together are all of them, and neither claims the same
        // neighbour twice. A typo here is a road that does not exist.
        for split in SPLIT {
            let mut both: Vec<u8> =
                split.north.iter().chain(split.south.iter()).copied().collect();
            both.sort_unstable();
            let mut once = both.clone();
            once.dedup();
            assert_eq!(both, once, "{:#04x} lists a neighbour under both pieces", split.map);
            assert_eq!(
                both,
                neighbours(split.map),
                "{:#04x}'s pieces do not add up to its neighbours",
                split.map
            );
            assert_ne!(split.north_door, split.south_door);
        }
    }

    #[test]
    fn route_2s_halves_are_told_apart_by_the_row_the_fly_is_standing_on() {
        // The two doorways are rows 11 and 43, measured from the cartridge's own warp table.
        assert_eq!(region_at(maps::ROUTE_2, 11).part, NORTH_PIECE);
        assert_eq!(region_at(maps::ROUTE_2, 43).part, SOUTH_PIECE);
        assert_eq!(region_at(maps::ROUTE_2, 0).part, NORTH_PIECE, "Pewter's end");
        assert_eq!(region_at(maps::ROUTE_2, 71).part, SOUTH_PIECE, "Viridian's end");
        // Every other map is one piece, whatever row is asked about.
        assert_eq!(region_at(maps::ROUTE_1, 30), Region::whole(maps::ROUTE_1));
        assert_eq!(region_at(maps::VIRIDIAN_FOREST_SOUTH_GATE, 7).part, 0);
    }

    #[test]
    fn the_way_to_pewter_from_the_forests_south_gate_is_north_through_the_forest() {
        // Row 33 of `infra/docs/macros-traps.md`, as a number. Standing in the gate house, the
        // graph used to answer `ROUTE_2` -- the door the fly had just come in through -- because
        // Route 2's north edge touches Pewter and the table had one node for the whole map.
        let gate = Region::whole(maps::VIRIDIAN_FOREST_SOUTH_GATE);
        assert_eq!(next_hop(gate, maps::PEWTER_CITY), Some(maps::VIRIDIAN_FOREST));
        // And on through: the forest, the north gate, Route 2's north half, Pewter.
        assert_eq!(
            next_hop(Region::whole(maps::VIRIDIAN_FOREST), maps::PEWTER_CITY),
            Some(maps::VIRIDIAN_FOREST_NORTH_GATE)
        );
        assert_eq!(
            next_hop(Region::whole(maps::VIRIDIAN_FOREST_NORTH_GATE), maps::PEWTER_CITY),
            Some(maps::ROUTE_2)
        );
        // Route 2's north half steps off its own top edge; its south half walks to the gate.
        assert_eq!(next_hop(region_at(maps::ROUTE_2, 11), maps::PEWTER_CITY), Some(maps::PEWTER_CITY));
        assert_eq!(
            next_hop(region_at(maps::ROUTE_2, 43), maps::PEWTER_CITY),
            Some(maps::VIRIDIAN_FOREST_SOUTH_GATE)
        );
        // From Viridian City the first hop is still Route 2, which is the road the fly takes north.
        assert_eq!(next_hop(Region::whole(maps::VIRIDIAN_CITY), maps::PEWTER_CITY), Some(maps::ROUTE_2));
        // And the way back south from the north half is the forest, not Route 2's own bottom edge.
        assert_eq!(
            next_hop(region_at(maps::ROUTE_2, 11), maps::VIRIDIAN_CITY),
            Some(maps::VIRIDIAN_FOREST_NORTH_GATE)
        );
    }

    #[test]
    fn the_museums_two_floors_know_the_road_to_the_gym() {
        // The rung-10 loop of 2026-09-22: five and a half hours on `PEWTER_CITY` with the
        // objective two doors away, and inside the museum `GO OBJECTIVE` was off the pad because
        // the map had no row here at all. Both floors, because the fly spent the run on both.
        let at = |map: u8| Region::whole(map);
        assert_eq!(
            next_hop(at(maps::PEWTER_MUSEUM_1F), maps::PEWTER_GYM),
            Some(maps::PEWTER_CITY),
            "the way to the gym from the museum's ground floor is out of its front door"
        );
        assert_eq!(
            next_hop(at(maps::PEWTER_MUSEUM_2F), maps::PEWTER_GYM),
            Some(maps::PEWTER_MUSEUM_1F),
            "and from the upper floor it is the staircase"
        );
        assert_eq!(next_hop(at(maps::PEWTER_CITY), maps::PEWTER_GYM), Some(maps::PEWTER_GYM));
        // The other direction, which is what `GO OBJECTIVE` asks when the errand is the museum's
        // own town: the museum is one hop from Pewter City and two from its upper floor.
        assert_eq!(
            next_hop(at(maps::PEWTER_CITY), maps::PEWTER_MUSEUM_2F),
            Some(maps::PEWTER_MUSEUM_1F)
        );
        // And the area, which is what puts the town's errands on the pad indoors. The upper
        // floor has no area, exactly as `REDS_HOUSE_2F` has none: a floor with no front door of
        // its own is not "in" anywhere, so it offers no errand -- and the road out is still the
        // staircase above, which is what the fly needs there.
        assert_eq!(area_of(maps::PEWTER_MUSEUM_1F), Some(maps::PEWTER_CITY));
        assert_eq!(area_of(maps::PEWTER_MUSEUM_2F), None);
        // The museum is neither a mart nor a centre, so it is nobody's errand.
        assert_eq!(amenity_at(maps::PEWTER_MUSEUM_1F), None);
        assert_eq!(amenity_at(maps::PEWTER_MUSEUM_2F), None);
    }

    #[test]
    fn a_maps_area_is_its_town_indoors_and_out() {
        // Outdoors a map is its own area, including a route -- which has no amenity row, so it
        // offers no errand, which is the point.
        assert_eq!(area_of(maps::VIRIDIAN_CITY), Some(maps::VIRIDIAN_CITY));
        assert_eq!(area_of(maps::ROUTE_1), Some(maps::ROUTE_1));
        // Indoors it is the town outside the front door, so the mart, the centre, the gym and an
        // ordinary house on the graph all answer the same town.
        assert_eq!(area_of(maps::VIRIDIAN_MART), Some(maps::VIRIDIAN_CITY));
        assert_eq!(area_of(maps::VIRIDIAN_POKECENTER), Some(maps::VIRIDIAN_CITY));
        assert_eq!(area_of(maps::VIRIDIAN_GYM), Some(maps::VIRIDIAN_CITY));
        assert_eq!(area_of(maps::OAKS_LAB), Some(maps::PALLET_TOWN));
        // A floor with no front door of its own, and a map nobody can name, have no area.
        assert_eq!(area_of(maps::REDS_HOUSE_2F), None);
        assert_eq!(area_of(0xf0), None);
    }

    #[test]
    fn an_area_knows_its_mart_and_its_centre_and_nothing_it_does_not() {
        assert_eq!(amenity_of(maps::VIRIDIAN_CITY, Amenity::Mart), Some(maps::VIRIDIAN_MART));
        assert_eq!(
            amenity_of(maps::VIRIDIAN_CITY, Amenity::Center),
            Some(maps::VIRIDIAN_POKECENTER)
        );
        assert_eq!(amenity_of(maps::PEWTER_CITY, Amenity::Mart), Some(maps::PEWTER_MART));
        assert_eq!(amenity_of(maps::CERULEAN_CITY, Amenity::Mart), Some(maps::CERULEAN_MART));
        // Pallet Town has neither, and a route has neither: no row, no errand, nothing guessed.
        assert_eq!(amenity_of(maps::PALLET_TOWN, Amenity::Mart), None);
        assert_eq!(amenity_of(maps::PALLET_TOWN, Amenity::Center), None);
        assert_eq!(amenity_of(maps::ROUTE_1, Amenity::Center), None);
        // And the reverse lookup, which is how the fly knows which building it is standing in.
        assert_eq!(amenity_at(maps::VIRIDIAN_MART), Some(Amenity::Mart));
        assert_eq!(amenity_at(maps::VIRIDIAN_POKECENTER), Some(Amenity::Center));
        assert_eq!(amenity_at(maps::VIRIDIAN_GYM), None);
        assert_eq!(amenity_at(maps::VIRIDIAN_CITY), None);
    }

    #[test]
    fn every_amenity_is_on_the_map_graph() {
        // A building with no `LINKS` row is one `GO OBJECTIVE` and `GO SHOP` could never route to,
        // and the errand would sit on the pad for ever with nothing to aim at.
        for (area, kind, map) in AMENITIES {
            assert!(
                neighbours(*map).contains(area),
                "{} {:#04x} is not linked to its area {area:#04x}",
                kind.label(),
                map
            );
            assert_eq!(area_of(*map), Some(*area), "{:#04x}", map);
            assert_eq!(
                next_hop(Region::whole(*area), *map),
                Some(*map),
                "the first hop to {}'s own {} is the building itself",
                area,
                kind.label()
            );
        }
    }

    #[test]
    fn every_rung_place_the_catalog_carries_is_on_the_graph() {
        // A place with no neighbours is a place `GO OBJECTIVE` can never be routed to, which is
        // worth knowing at build time rather than on the stream.
        for place in crate::pokemon_red::RUNG_PLACES.iter().flatten() {
            assert!(
                !neighbours(place.map).is_empty(),
                "rung place {:#04x} is not on the map graph",
                place.map
            );
        }
    }
}
