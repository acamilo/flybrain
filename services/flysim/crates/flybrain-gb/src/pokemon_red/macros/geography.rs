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
use super::state::MapGrid;

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
/// Every row is the header's own, checked line by line against the disassembly at the pinned
/// commit (row 59). Four pairs had the right neighbour in the wrong column, and a wrong column is
/// a wrong map on the other side of an edge: `ROUTE_3` / `ROUTE_4` (Route 4 is north of Route 3,
/// not east, and Route 3's top edge is the road to Mt. Moon's Pokécenter), `ROUTE_14` /
/// `ROUTE_15` and `ROUTE_24` / `ROUTE_25` (west and east, not south and north). One header line
/// is left out on purpose: `ROUTE_22`'s `connection north, Route23`, below.
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
    (maps::ROUTE_3, [maps::ROUTE_4, NONE, maps::PEWTER_CITY, NONE]),
    (maps::ROUTE_4, [NONE, maps::ROUTE_3, NONE, maps::CERULEAN_CITY]),
    (maps::ROUTE_5, [maps::CERULEAN_CITY, maps::SAFFRON_CITY, NONE, NONE]),
    (maps::ROUTE_6, [maps::SAFFRON_CITY, maps::VERMILION_CITY, NONE, NONE]),
    (maps::ROUTE_7, [NONE, NONE, maps::CELADON_CITY, maps::SAFFRON_CITY]),
    (maps::ROUTE_8, [NONE, NONE, maps::SAFFRON_CITY, maps::LAVENDER_TOWN]),
    (maps::ROUTE_9, [NONE, NONE, maps::CERULEAN_CITY, maps::ROUTE_10]),
    (maps::ROUTE_10, [NONE, maps::LAVENDER_TOWN, maps::ROUTE_9, NONE]),
    (maps::ROUTE_11, [NONE, NONE, maps::VERMILION_CITY, maps::ROUTE_12]),
    (maps::ROUTE_12, [maps::LAVENDER_TOWN, maps::ROUTE_13, maps::ROUTE_11, NONE]),
    (maps::ROUTE_13, [maps::ROUTE_12, NONE, maps::ROUTE_14, NONE]),
    (maps::ROUTE_14, [NONE, NONE, maps::ROUTE_15, maps::ROUTE_13]),
    (maps::ROUTE_15, [NONE, NONE, maps::FUCHSIA_CITY, maps::ROUTE_14]),
    (maps::ROUTE_16, [NONE, maps::ROUTE_17, NONE, maps::CELADON_CITY]),
    (maps::ROUTE_17, [maps::ROUTE_16, maps::ROUTE_18, NONE, NONE]),
    (maps::ROUTE_18, [maps::ROUTE_17, NONE, NONE, maps::FUCHSIA_CITY]),
    (maps::ROUTE_19, [maps::FUCHSIA_CITY, NONE, maps::ROUTE_20, NONE]),
    (maps::ROUTE_20, [NONE, NONE, maps::CINNABAR_ISLAND, maps::ROUTE_19]),
    (maps::ROUTE_21, [maps::PALLET_TOWN, maps::CINNABAR_ISLAND, NONE, NONE]),
    // Route 22's header says `connection north, Route23`, and no tile of that strip is walkable on
    // either side: the road north is the League gate, a building the graph has no row for. An
    // edge in the table is a road `next_hop` will route along, so this one stays out, and Indigo
    // Plateau is on the graph but not reachable from the south.
    (maps::ROUTE_22, [NONE, NONE, NONE, maps::VIRIDIAN_CITY]),
    (maps::ROUTE_23, [maps::INDIGO_PLATEAU, NONE, NONE, NONE]),
    (maps::ROUTE_24, [NONE, maps::CERULEAN_CITY, NONE, maps::ROUTE_25]),
    (maps::ROUTE_25, [NONE, NONE, maps::ROUTE_24, NONE]),
];

/// Doors and floor changes, as undirected pairs of maps.
///
/// Only the ones a rung place needs a route through, because that is all [`next_hop`] is for: an
/// unlisted building is simply not on the graph, which makes it a place `GO OBJECTIVE` cannot aim
/// at from another map and changes nothing else. Every pair is two warp tables that name each
/// other (a `LAST_MAP` door resolved to the one outdoor map whose warps lead in).
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
    // Mt. Moon has two mouths, and both are on Route 4 (`data/maps/objects/Route4.asm`): (18, 5)
    // into the first floor, and (24, 5) into B1F, whose (27, 3) is the way back out on the far
    // side of the mountain. Route 3 has no warps at all. Which chamber of B1F and B2F each ladder
    // opens onto is [`SPLIT`]'s business.
    (maps::MT_MOON_1F, maps::ROUTE_4),
    (maps::MT_MOON_1F, maps::MT_MOON_B1F),
    (maps::MT_MOON_B1F, maps::MT_MOON_B2F),
    (maps::MT_MOON_B1F, maps::ROUTE_4),
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
/// out of the gate it had just walked into, once per hold, for four hours. Row 59 found three more
/// on the road to Cerulean: Route 4, and Mt. Moon's two lower floors ([`SPLIT`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Region {
    pub map: u8,
    /// Which piece, for a map [`SPLIT`] has a row for: its index in that row. 0 everywhere else.
    pub part: u8,
}

impl Region {
    /// A map whose ground is all one piece, which is every map but [`SPLIT`]'s.
    pub const fn whole(map: u8) -> Self {
        Self { map, part: 0 }
    }

    /// Piece `part` of a map [`SPLIT`] has a row for.
    pub const fn piece(map: u8, part: u8) -> Self {
        Self { map, part }
    }
}

/// One piece of a split map.
struct Piece {
    /// The map's own warps that stand on this piece's ground: the index into its warp table (as
    /// `wWarpEntries` holds it, and as a warp elsewhere names it for its destination, 0-based) and
    /// the tile. A warp that lands on one of these lands in this piece; the tiles are what the
    /// fly's own piece is told apart by ([`region_on`]).
    doors: &'static [(u8, u8, u8)],
    /// Everything one step from this piece: a whole map, or a piece of another split map. Their
    /// maps together are exactly [`neighbours`]'s answer for the map, and every step is listed
    /// back from the other side; [`tests::a_split_maps_pieces_add_up_and_answer_each_other`] pins
    /// both.
    next: &'static [Region],
}

/// A map whose walkable ground is in pieces the player cannot walk between.
struct Split {
    map: u8,
    pieces: &'static [Piece],
}

/// Route 2's two halves, in [`SPLIT`]'s order.
#[cfg(test)]
const NORTH_PIECE: u8 = 0;
#[cfg(test)]
const SOUTH_PIECE: u8 = 1;

/// Route 4's two sides of the mountain.
#[cfg(test)]
const WEST_SIDE: u8 = 0;
const EAST_SIDE: u8 = 1;

/// Mt. Moon B1F's four chambers, named by what they hold.
const B1F_EXIT: u8 = 0;
const B1F_WEST: u8 = 1;
const B1F_MIDDLE: u8 = 2;
const B1F_SOUTH: u8 = 3;

/// Mt. Moon B2F's three pieces.
const B2F_MAIN: u8 = 0;
const B2F_NORTH: u8 = 1;
const B2F_SOUTH: u8 = 2;

/// Every map whose ground is in pieces, with each piece's doors and neighbours.
///
/// Every row is measured from the disassembly at the pinned commit: the map's blocks, its
/// tileset's blockset and collision list, the tile-pair walls and the ledges, flooded tile by tile
/// (`infra/docs/macros-traps.md` row 59 has the method). A map is listed here only when two of its
/// ways out are on different pieces, and the audit ran over every map on the graph.
///
/// - **`ROUTE_2`** (row 33): the forest's north gate at (3, 11) and Pewter's edge; the south gate
///   at (3, 43) and Viridian's. Maps 46, 48 and 49 stay off the graph, the module's standing rule
///   for a building no rung place needs a route through: 49's two doors are both Route 2's own.
/// - **`ROUTE_4`** (row 59): Mt. Moon stands across it. The west side holds the Pokécenter at
///   (11, 5), the cave mouth at (18, 5) and the road down to Route 3; the east side holds B1F's
///   exit at (24, 5) and the ledges down to Cerulean. From the Pewter side the only way east is
///   through the mountain.
/// - **`MT_MOON_B1F`** (row 59): four chambers, each two ladders and nothing between them. The
///   one road through is 1F (5, 5) to the west chamber, (21, 17) down to B2F, B2F (5, 7) up to the
///   exit chamber, (27, 3) out onto Route 4's east side. The middle and south chambers are ladders
///   to dead ends on B2F.
/// - **`MT_MOON_B2F`** (row 59): the fossil floor, one large piece with the two ladders the road
///   uses, and two small pieces under the dead-end ladders.
const SPLIT: &[Split] = &[
    Split {
        map: maps::ROUTE_2,
        pieces: &[
            Piece {
                doors: &[(1, 3, 11)],
                next: &[
                    Region::whole(maps::PEWTER_CITY),
                    Region::whole(maps::VIRIDIAN_FOREST_NORTH_GATE),
                ],
            },
            Piece {
                doors: &[(5, 3, 43)],
                next: &[
                    Region::whole(maps::VIRIDIAN_CITY),
                    Region::whole(maps::VIRIDIAN_FOREST_SOUTH_GATE),
                ],
            },
        ],
    },
    Split {
        map: maps::ROUTE_4,
        pieces: &[
            Piece {
                doors: &[(0, 11, 5), (1, 18, 5)],
                next: &[Region::whole(maps::ROUTE_3), Region::whole(maps::MT_MOON_1F)],
            },
            Piece {
                doors: &[(2, 24, 5)],
                next: &[
                    Region::piece(maps::MT_MOON_B1F, B1F_EXIT),
                    Region::whole(maps::CERULEAN_CITY),
                ],
            },
        ],
    },
    Split {
        map: maps::MT_MOON_B1F,
        pieces: &[
            Piece {
                doors: &[(6, 23, 3), (7, 27, 3)],
                next: &[
                    Region::piece(maps::MT_MOON_B2F, B2F_MAIN),
                    Region::piece(maps::ROUTE_4, EAST_SIDE),
                ],
            },
            Piece {
                doors: &[(0, 5, 5), (4, 21, 17)],
                next: &[
                    Region::whole(maps::MT_MOON_1F),
                    Region::piece(maps::MT_MOON_B2F, B2F_MAIN),
                ],
            },
            Piece {
                doors: &[(1, 17, 11), (2, 25, 9)],
                next: &[
                    Region::whole(maps::MT_MOON_1F),
                    Region::piece(maps::MT_MOON_B2F, B2F_NORTH),
                ],
            },
            Piece {
                doors: &[(3, 25, 15), (5, 13, 27)],
                next: &[
                    Region::whole(maps::MT_MOON_1F),
                    Region::piece(maps::MT_MOON_B2F, B2F_SOUTH),
                ],
            },
        ],
    },
    Split {
        map: maps::MT_MOON_B2F,
        pieces: &[
            Piece {
                doors: &[(1, 21, 17), (3, 5, 7)],
                next: &[
                    Region::piece(maps::MT_MOON_B1F, B1F_EXIT),
                    Region::piece(maps::MT_MOON_B1F, B1F_WEST),
                ],
            },
            Piece {
                doors: &[(0, 25, 9)],
                next: &[Region::piece(maps::MT_MOON_B1F, B1F_MIDDLE)],
            },
            Piece {
                doors: &[(2, 15, 27)],
                next: &[Region::piece(maps::MT_MOON_B1F, B1F_SOUTH)],
            },
        ],
    },
];

fn split_of(map: u8) -> Option<&'static Split> {
    SPLIT.iter().find(|split| split.map == map)
}

/// The pieces of a split map with their numbers, which are their [`Region::part`]s.
fn pieces(split: &'static Split) -> impl Iterator<Item = (u8, &'static Piece)> {
    split.pieces.iter().enumerate().filter_map(|(part, piece)| Some((u8::try_from(part).ok()?, piece)))
}

/// The piece of `map` the tile `(x, y)` is in, by the doors alone.
///
/// For every map but [`SPLIT`]'s rows this is [`Region::whole`]. On a split map it is the piece
/// with the nearest door, counting tiles across and down, the first piece on a tie. That is exact
/// for every tile of Route 2's and Route 4's ground, which is where the grid cannot answer
/// ([`region_on`]: a ledge is a one-way step the grid does not model), and it is only the fallback
/// on Mt. Moon's floors, whose chambers wrap round each other.
pub fn region_at(map: u8, x: u8, y: u8) -> Region {
    let Some(split) = split_of(map) else { return Region::whole(map) };
    let distance = |piece: &Piece| {
        piece
            .doors
            .iter()
            .map(|(_, dx, dy)| u16::from(x.abs_diff(*dx)) + u16::from(y.abs_diff(*dy)))
            .min()
            .unwrap_or(u16::MAX)
    };
    let part = pieces(split).min_by_key(|(part, piece)| (distance(piece), *part)).map_or(0, |(part, _)| part);
    Region { map, part }
}

/// The piece of `map` the fly standing on `(x, y)` is in.
///
/// The ground decides: with the decoded map grid (section 15) the piece is the one whose doors a
/// walk from here can reach, when exactly one piece's can. The grid has no ledges -- a ledge is a
/// step one way only, and the grid reads it as a wall -- so on the part of Route 4 below the
/// ledges no door is reachable and [`region_at`] answers from the doors. Row 59 flooded every tile
/// of every piece's ground in the disassembly under this rule and it names the right piece for
/// all of them.
pub fn region_on(map: u8, x: u8, y: u8, grid: Option<&MapGrid>) -> Region {
    let Some(split) = split_of(map) else { return Region::whole(map) };
    if let Some(grid) = grid.filter(|grid| grid.map() == map) {
        let walk = grid.reachable(x, y);
        // A door tile the collision list refuses is still stepped onto from beside it.
        let reached = |dx: u8, dy: u8| {
            walk.contains(dx, dy)
                || [(0i16, 1i16), (0, -1), (1, 0), (-1, 0)].iter().any(|(ox, oy)| {
                    match (u8::try_from(i16::from(dx) + ox), u8::try_from(i16::from(dy) + oy)) {
                        (Ok(nx), Ok(ny)) => walk.contains(nx, ny),
                        _ => false,
                    }
                })
        };
        let mut hit =
            pieces(split).filter(|(_, piece)| piece.doors.iter().any(|(_, dx, dy)| reached(*dx, *dy)));
        if let (Some((part, _)), None) = (hit.next(), hit.next()) {
            return Region { map, part };
        }
    }
    region_at(map, x, y)
}

/// The piece of `map` a warp lands in when it names `map`'s warp `index` as its destination.
///
/// `None` only for a split map whose table does not list that warp, which is not guessed at.
pub fn arrival_by_warp(map: u8, index: u8) -> Option<Region> {
    let Some(split) = split_of(map) else { return Some(Region::whole(map)) };
    pieces(split)
        .find(|(_, piece)| piece.doors.iter().any(|(door, _, _)| *door == index))
        .map(|(part, _)| Region { map, part })
}

/// The piece of `map` that stepping off an edge of `from` lands in.
///
/// An edge is listed under exactly one piece of a split map: Pewter's south edge opens onto Route
/// 2's north half, Route 3's north edge onto Route 4's west side and Cerulean's west edge onto its
/// east side. `None` is an edge the graph does not have.
pub fn arrival_by_edge(map: u8, from: u8) -> Option<Region> {
    let Some(split) = split_of(map) else { return Some(Region::whole(map)) };
    pieces(split)
        .find(|(_, piece)| piece.next.iter().any(|next| next.map == from))
        .map(|(part, _)| Region { map, part })
}

/// Every piece of ground one step from `region`.
fn region_neighbours(region: Region) -> Vec<Region> {
    if let Some(split) = split_of(region.map) {
        return split.pieces.get(usize::from(region.part)).map_or_else(Vec::new, |piece| piece.next.to_vec());
    }
    // A whole map steps onto every piece of a split neighbour that lists it back: Mt. Moon's
    // first floor has a ladder into three of B1F's four chambers.
    let mut out = Vec::new();
    for map in neighbours(region.map) {
        match split_of(map) {
            None => out.push(Region::whole(map)),
            Some(split) => out.extend(
                pieces(split)
                    .filter(|(_, piece)| piece.next.contains(&region))
                    .map(|(part, _)| Region { map, part }),
            ),
        }
    }
    out
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
    next_step(from, to).map(|hop| hop.map)
}

/// [`next_hop`] with the piece it lands in, which is what an exit has to match on a map with two
/// doors into one split map: three of Mt. Moon's first-floor ladders go down to B1F, and only
/// one of them reaches the way out ([`arrival_by_warp`] names where each one lands).
pub fn next_step(from: Region, to: u8) -> Option<Region> {
    if from.map == to {
        return None;
    }
    let mut seen: HashSet<Region> = HashSet::from([from]);
    // piece -> the first hop out of `from` that reaches it
    let mut first: HashMap<Region, Region> = HashMap::new();
    let mut queue: VecDeque<Region> = VecDeque::new();
    for hop in region_neighbours(from) {
        if seen.insert(hop) {
            first.insert(hop, hop);
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

/// How many hops the shortest known route from `from` to the map `to` takes, or `None` when none
/// is known.
///
/// [`next_hop`]'s own breadth-first walk, counting instead of naming: `Some(0)` when the fly is
/// already on `to`, `Some(1)` for a door out of this map into it, and `None` for a map the table
/// cannot route to -- which is the same "nothing is guessed" [`next_hop`] answers with.
///
/// What it is *for* is the ratchet (`docs/design/ladder.md`, the 2026-09-17 progress rule): the
/// stall window is reset by exploration, and a fly crossing a town it has already covered to
/// reach the rung's own door earns no new ground while it does it. "Nearer the objective than
/// this run has ever been" is the other thing that is plainly progress, and it is this number
/// falling. Nothing about the *choice* reads it: no macro is ranked by it and no button is bound
/// on it.
pub fn hops(from: Region, to: u8) -> Option<u32> {
    if from.map == to {
        return Some(0);
    }
    let mut seen: HashSet<Region> = HashSet::from([from]);
    let mut queue: VecDeque<(Region, u32)> = VecDeque::new();
    queue.push_back((from, 0));
    while let Some((region, depth)) = queue.pop_front() {
        if region.map == to {
            return Some(depth);
        }
        for next in region_neighbours(region) {
            if seen.insert(next) {
                queue.push_back((next, depth + 1));
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
    fn the_rows_the_disassembly_corrected_say_what_its_headers_say() {
        // Row 59, `data/maps/headers/*.asm` at the pinned commit. Route 4 is north of Route 3:
        // Route 3's top edge is the road to Mt. Moon's Pokécenter, and Route 3 has no east exit.
        assert_eq!(connected(maps::ROUTE_3, Edge::North), Some(maps::ROUTE_4));
        assert_eq!(connected(maps::ROUTE_3, Edge::East), None);
        assert_eq!(connected(maps::ROUTE_4, Edge::South), Some(maps::ROUTE_3));
        assert_eq!(connected(maps::ROUTE_4, Edge::West), None);
        assert_eq!(connected(maps::ROUTE_4, Edge::East), Some(maps::CERULEAN_CITY));
        // Nugget Bridge's far end is Route 24's east edge, not its north one.
        assert_eq!(connected(maps::ROUTE_24, Edge::East), Some(maps::ROUTE_25));
        assert_eq!(connected(maps::ROUTE_24, Edge::North), None);
        assert_eq!(connected(maps::ROUTE_25, Edge::West), Some(maps::ROUTE_24));
        assert_eq!(connected(maps::ROUTE_25, Edge::South), None);
        assert_eq!(connected(maps::ROUTE_14, Edge::West), Some(maps::ROUTE_15));
        assert_eq!(connected(maps::ROUTE_15, Edge::East), Some(maps::ROUTE_14));
        // Mt. Moon's two mouths are both on Route 4, and Route 3 has no warps: a cave door that
        // is not there is a road the fly walks up and down for ever (row 59's Pewter ring).
        assert!(!neighbours(maps::ROUTE_3).contains(&maps::MT_MOON_1F));
        assert_eq!(outdoor_of(maps::MT_MOON_1F), Some(maps::ROUTE_4));
        assert_eq!(outdoor_of(maps::MT_MOON_B1F), Some(maps::ROUTE_4));
        assert_eq!(
            neighbours(maps::ROUTE_3),
            vec![maps::PEWTER_CITY, maps::ROUTE_4],
            "Route 3 is a road between two maps and nothing else"
        );
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
        // Out along Route 3, whose only other end is the road up to Mt. Moon.
        assert_eq!(next_hop(at(maps::PEWTER_CITY), maps::CERULEAN_GYM), Some(maps::ROUTE_3));
        // Nowhere to go, and nowhere known.
        assert_eq!(next_hop(at(maps::PALLET_TOWN), maps::PALLET_TOWN), None);
        assert_eq!(next_hop(at(maps::PALLET_TOWN), 0xf0), None);
    }

    #[test]
    fn a_split_maps_pieces_add_up_and_answer_each_other() {
        // The invariants that keep [`SPLIT`] honest. A typo in any of them is a road that does
        // not exist, or a door that leads nowhere.
        for split in SPLIT {
            // The pieces' neighbours together are exactly the map's.
            let mut maps_of: Vec<u8> =
                split.pieces.iter().flat_map(|piece| piece.next.iter().map(|r| r.map)).collect();
            maps_of.sort_unstable();
            maps_of.dedup();
            assert_eq!(
                maps_of,
                neighbours(split.map),
                "{:#04x}'s pieces do not add up to its neighbours",
                split.map
            );
            let mut doors: Vec<u8> =
                split.pieces.iter().flat_map(|piece| piece.doors.iter().map(|d| d.0)).collect();
            doors.sort_unstable();
            let count = doors.len();
            doors.dedup();
            assert_eq!(doors.len(), count, "{:#04x} lists one warp under two pieces", split.map);
            for (part, piece) in pieces(split) {
                let here = Region { map: split.map, part };
                assert!(!piece.doors.is_empty(), "{here:?} has no door to be told apart by");
                // A door's own tile is in its own piece.
                for (_, x, y) in piece.doors {
                    assert_eq!(region_at(split.map, *x, *y), here, "door ({x}, {y})");
                }
                // Every step is listed back from the other side, so a route is reversible.
                for next in piece.next {
                    assert!(
                        region_neighbours(*next).contains(&here),
                        "{next:?} does not step back onto {here:?}"
                    );
                    if let Some(other) = split_of(next.map) {
                        assert!(usize::from(next.part) < other.pieces.len(), "{next:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_road_from_pewter_to_cerulean_is_through_mt_moon_one_chamber_at_a_time() {
        // Row 59. Every step of the road, as the pieces the fly stands in, measured from the
        // disassembly: Route 3's top edge, Route 4's west side, the cave mouth at (18, 5), 1F's
        // ladder at (5, 5), B1F's west chamber, its ladder at (21, 17), B2F, its ladder at (5, 7),
        // B1F's exit chamber, (27, 3), Route 4's east side, Cerulean.
        let road = [
            Region::whole(maps::PEWTER_CITY),
            Region::whole(maps::ROUTE_3),
            Region::piece(maps::ROUTE_4, WEST_SIDE),
            Region::whole(maps::MT_MOON_1F),
            Region::piece(maps::MT_MOON_B1F, B1F_WEST),
            Region::piece(maps::MT_MOON_B2F, B2F_MAIN),
            Region::piece(maps::MT_MOON_B1F, B1F_EXIT),
            Region::piece(maps::ROUTE_4, EAST_SIDE),
            Region::whole(maps::CERULEAN_CITY),
        ];
        for pair in road.windows(2) {
            assert_eq!(next_step(pair[0], maps::CERULEAN_CITY), Some(pair[1]), "from {:?}", pair[0]);
        }
        assert_eq!(hops(road[0], maps::CERULEAN_CITY), Some(8));
        // Mt. Moon's rung is the first floor, one hop from the cave mouth's side of Route 4 --
        // which is where the Pewter ring said the fly could never get to.
        assert_eq!(next_hop(Region::whole(maps::PEWTER_CITY), maps::MT_MOON_1F), Some(maps::ROUTE_3));
        assert_eq!(next_hop(Region::whole(maps::ROUTE_3), maps::MT_MOON_1F), Some(maps::ROUTE_4));
        assert_eq!(
            next_hop(Region::piece(maps::ROUTE_4, WEST_SIDE), maps::MT_MOON_1F),
            Some(maps::MT_MOON_1F)
        );
        // The dead ends lead back the way they came.
        assert_eq!(
            next_step(Region::piece(maps::MT_MOON_B1F, B1F_MIDDLE), maps::CERULEAN_CITY),
            Some(Region::whole(maps::MT_MOON_1F))
        );
        assert_eq!(
            next_step(Region::piece(maps::MT_MOON_B2F, B2F_SOUTH), maps::CERULEAN_CITY),
            Some(Region::piece(maps::MT_MOON_B1F, B1F_SOUTH))
        );
        // From Cerulean's side of the mountain the way back to Pewter is the cave, not Route 4's
        // south edge, which is on the other side.
        assert_eq!(
            next_step(Region::piece(maps::ROUTE_4, EAST_SIDE), maps::PEWTER_CITY),
            Some(Region::piece(maps::MT_MOON_B1F, B1F_EXIT))
        );
        assert_eq!(
            next_step(Region::whole(maps::CERULEAN_CITY), maps::ROUTE_24),
            Some(Region::whole(maps::ROUTE_24))
        );
        assert_eq!(next_hop(Region::whole(maps::ROUTE_24), maps::ROUTE_25), Some(maps::ROUTE_25));
    }

    #[test]
    fn a_door_or_an_edge_lands_in_the_piece_it_opens_onto() {
        // Which warp of the destination a warp names is the cartridge's own answer (`wWarpEntries`
        // byte 2, 0-based): 1F's ladders are B1F's warps 0, 2 and 3.
        assert_eq!(arrival_by_warp(maps::MT_MOON_B1F, 0), Some(Region::piece(maps::MT_MOON_B1F, B1F_WEST)));
        assert_eq!(arrival_by_warp(maps::MT_MOON_B1F, 2), Some(Region::piece(maps::MT_MOON_B1F, B1F_MIDDLE)));
        assert_eq!(arrival_by_warp(maps::MT_MOON_B1F, 3), Some(Region::piece(maps::MT_MOON_B1F, B1F_SOUTH)));
        // B1F's exit is `LAST_MAP` warp 2: Route 4's (24, 5), the far side of the mountain.
        assert_eq!(arrival_by_warp(maps::ROUTE_4, 2), Some(Region::piece(maps::ROUTE_4, EAST_SIDE)));
        // 1F's doormat is `LAST_MAP` warp 1: the cave mouth, the Pewter side.
        assert_eq!(arrival_by_warp(maps::ROUTE_4, 1), Some(Region::piece(maps::ROUTE_4, WEST_SIDE)));
        // The forest gates' doormats onto Route 2 are its warps 1 and 5.
        assert_eq!(arrival_by_warp(maps::ROUTE_2, 1), Some(Region::piece(maps::ROUTE_2, NORTH_PIECE)));
        assert_eq!(arrival_by_warp(maps::ROUTE_2, 5), Some(Region::piece(maps::ROUTE_2, SOUTH_PIECE)));
        // A warp the table does not list is not guessed at; a whole map is always whole.
        assert_eq!(arrival_by_warp(maps::ROUTE_2, 0), None);
        assert_eq!(arrival_by_warp(maps::PEWTER_GYM, 0), Some(Region::whole(maps::PEWTER_GYM)));
        // Edges.
        assert_eq!(arrival_by_edge(maps::ROUTE_4, maps::ROUTE_3), Some(Region::piece(maps::ROUTE_4, WEST_SIDE)));
        assert_eq!(
            arrival_by_edge(maps::ROUTE_4, maps::CERULEAN_CITY),
            Some(Region::piece(maps::ROUTE_4, EAST_SIDE))
        );
        assert_eq!(arrival_by_edge(maps::ROUTE_2, maps::PEWTER_CITY), Some(Region::piece(maps::ROUTE_2, NORTH_PIECE)));
        assert_eq!(arrival_by_edge(maps::ROUTE_3, maps::ROUTE_4), Some(Region::whole(maps::ROUTE_3)));
    }

    #[test]
    fn route_4s_sides_are_told_apart_by_the_doors_where_the_ground_cannot() {
        // With no grid, the nearest door: the cave mouth's side reaches down to Route 3's road at
        // (7..11, 17), and Cerulean's side is everything east of the mountain.
        assert_eq!(region_at(maps::ROUTE_4, 9, 17), Region::piece(maps::ROUTE_4, WEST_SIDE));
        assert_eq!(region_at(maps::ROUTE_4, 18, 6), Region::piece(maps::ROUTE_4, WEST_SIDE));
        assert_eq!(region_at(maps::ROUTE_4, 24, 6), Region::piece(maps::ROUTE_4, EAST_SIDE));
        assert_eq!(region_at(maps::ROUTE_4, 89, 10), Region::piece(maps::ROUTE_4, EAST_SIDE));
        assert_eq!(region_at(maps::ROUTE_3, 60, 0), Region::whole(maps::ROUTE_3));
        // B2F's chambers wrap round each other, so there the grid decides.
        let mut grid = MapGrid::new(maps::MT_MOON_B2F, 40, 36);
        // A corridor from B2F's (5, 7) ladder east along row 7 and down column 33 to (33, 31):
        // nearer the (15, 27) ladder than either of its own, and on the main piece.
        for x in 4..=33 {
            grid.set(x, 7, 0, super::super::state::Walkable::Yes);
        }
        for y in 7..=31 {
            grid.set(33, y, 0, super::super::state::Walkable::Yes);
        }
        assert_eq!(region_at(maps::MT_MOON_B2F, 33, 31), Region::piece(maps::MT_MOON_B2F, B2F_SOUTH));
        assert_eq!(
            region_on(maps::MT_MOON_B2F, 33, 31, Some(&grid)),
            Region::piece(maps::MT_MOON_B2F, B2F_MAIN)
        );
        // A grid of another map says nothing, and neither does none.
        let other = MapGrid::new(maps::ROUTE_4, 90, 18);
        assert_eq!(
            region_on(maps::MT_MOON_B2F, 33, 31, Some(&other)),
            Region::piece(maps::MT_MOON_B2F, B2F_SOUTH)
        );
        assert_eq!(region_on(maps::MT_MOON_B2F, 33, 31, None), Region::piece(maps::MT_MOON_B2F, B2F_SOUTH));
    }

    #[test]
    fn route_2s_halves_are_told_apart_by_the_row_the_fly_is_standing_on() {
        // The two doorways are rows 11 and 43, measured from the cartridge's own warp table.
        assert_eq!(region_at(maps::ROUTE_2, 3, 11).part, NORTH_PIECE);
        assert_eq!(region_at(maps::ROUTE_2, 3, 43).part, SOUTH_PIECE);
        assert_eq!(region_at(maps::ROUTE_2, 8, 0).part, NORTH_PIECE, "Pewter's end");
        assert_eq!(region_at(maps::ROUTE_2, 8, 71).part, SOUTH_PIECE, "Viridian's end");
        // Every other map is one piece, whatever row is asked about.
        assert_eq!(region_at(maps::ROUTE_1, 10, 30), Region::whole(maps::ROUTE_1));
        assert_eq!(region_at(maps::VIRIDIAN_FOREST_SOUTH_GATE, 4, 7).part, 0);
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
        assert_eq!(next_hop(region_at(maps::ROUTE_2, 3, 11), maps::PEWTER_CITY), Some(maps::PEWTER_CITY));
        assert_eq!(
            next_hop(region_at(maps::ROUTE_2, 3, 43), maps::PEWTER_CITY),
            Some(maps::VIRIDIAN_FOREST_SOUTH_GATE)
        );
        // From Viridian City the first hop is still Route 2, which is the road the fly takes north.
        assert_eq!(next_hop(Region::whole(maps::VIRIDIAN_CITY), maps::PEWTER_CITY), Some(maps::ROUTE_2));
        // And the way back south from the north half is the forest, not Route 2's own bottom edge.
        assert_eq!(
            next_hop(region_at(maps::ROUTE_2, 3, 11), maps::VIRIDIAN_CITY),
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
    fn the_hop_count_is_the_road_measured_rather_than_named() {
        let at = |map: u8| Region::whole(map);
        assert_eq!(hops(at(maps::PEWTER_CITY), maps::PEWTER_CITY), Some(0), "already there");
        assert_eq!(hops(at(maps::PEWTER_CITY), maps::PEWTER_GYM), Some(1), "one door");
        assert_eq!(hops(at(maps::PEWTER_MUSEUM_1F), maps::PEWTER_GYM), Some(2));
        assert_eq!(hops(at(maps::PEWTER_MUSEUM_2F), maps::PEWTER_GYM), Some(3));
        // The count agrees with the hop by hop answer, which is the thing it has to: walking the
        // road one `next_hop` at a time takes exactly this many steps.
        let mut here = at(maps::PEWTER_MUSEUM_2F);
        let mut steps = 0;
        while let Some(hop) = next_hop(here, maps::PEWTER_GYM) {
            here = Region::whole(hop);
            steps += 1;
            assert!(steps < 10, "the road to the gym does not wander");
        }
        assert_eq!(here.map, maps::PEWTER_GYM);
        assert_eq!(steps, 3);
        // A split map is measured from the piece the fly is standing in, exactly as `next_hop` is.
        assert_eq!(hops(region_at(maps::ROUTE_2, 3, 11), maps::PEWTER_CITY), Some(1));
        // Five from the south half, because the belt of trees between the halves needs CUT and
        // the road is the forest: the south gate, the forest, the north gate, Route 2's north
        // half, Pewter.
        assert_eq!(hops(region_at(maps::ROUTE_2, 3, 43), maps::PEWTER_CITY), Some(5));
        // And nothing is guessed.
        assert_eq!(hops(at(maps::PALLET_TOWN), 0xf0), None);
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
