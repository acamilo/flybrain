//! Walks planned over the whole map, and the same walks with the window as the only reading.
//!
//! `docs/design/macros.md` section 15. [`super::super::path`] has two readings of the ground now
//! and the interesting tests are the ones that tell them apart: a map bigger than the ten-by-nine
//! window, a frontier on the far side of it, and a wall the collision list cannot predict. The
//! fake here is deliberately not [`super::World`] — it implements the seam and nothing else, so a
//! failure is about the search rather than about a script's frames.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::pokemon_red::macros::cartridge::{Edge, ExitId, MacroState, Tile};
use crate::pokemon_red::macros::path::{self, Way};
use crate::pokemon_red::macros::state::{
    BagItem, Battle, Connections, Facing, GameState, MapGrid, MapSize, Mon, Npc, Party, Pc, Player,
    Scene, Shop, Sign, StartMenu, TextBox, Walkable, Warp,
};

/// A passable tile id and a wall tile id, for a grid built by hand.
const FLOOR: u8 = 0x01;
const WALL: u8 = 0x60;

/// The ground, and nothing else: the seam's questions about a map and the fly standing on it.
struct Ground {
    map: u8,
    size: MapSize,
    player: Tile,
    /// Tiles that are not walkable; everything else inside `size` is.
    walls: BTreeSet<Tile>,
    /// Steps the cartridge refuses although both tiles are passable, as the grid records them.
    pair_walls: Vec<(Tile, Facing)>,
    /// Ground the run has stood on, which is what makes a tile not a frontier.
    stood: BTreeSet<Tile>,
    warps: Vec<Warp>,
    connections: Connections,
    npcs: Vec<Npc>,
    /// Whether the whole map is decoded, or only the window can answer.
    decoded: bool,
}

impl Ground {
    /// A map `wide` by `high` tiles with the fly at `player` and every tile walkable.
    fn new(wide: u8, high: u8, player: (u8, u8)) -> Self {
        Self {
            map: 0,
            size: MapSize { width: wide, height: high },
            player: Tile::new(player.0, player.1),
            walls: BTreeSet::new(),
            pair_walls: Vec::new(),
            stood: BTreeSet::new(),
            warps: Vec::new(),
            connections: Connections::default(),
            npcs: Vec::new(),
            decoded: true,
        }
    }

    fn wall(mut self, x: u8, y: u8) -> Self {
        self.walls.insert(Tile::new(x, y));
        self
    }

    /// A column of wall with one gap in it, which is the shape that tells the two readings apart.
    fn wall_column(mut self, x: u8, gap: u8) -> Self {
        for y in 0..self.size.height {
            if y != gap {
                self.walls.insert(Tile::new(x, y));
            }
        }
        self
    }

    /// Everything within `distance` of the fly counts as stood on, which is what a fly that has
    /// been walking around one corner of a route has.
    fn stood_around(mut self, distance: u32) -> Self {
        for y in 0..self.size.height {
            for x in 0..self.size.width {
                let tile = Tile::new(x, y);
                if tile.distance(self.player) <= distance {
                    self.stood.insert(tile);
                }
            }
        }
        self
    }

    /// The window reading only: what every walk had before section 15.
    fn window_only(mut self) -> Self {
        self.decoded = false;
        self
    }

    fn pair_wall(mut self, from: (u8, u8), facing: Facing) -> Self {
        self.pair_walls.push((Tile::new(from.0, from.1), facing));
        self
    }

    fn connected(mut self, connections: Connections) -> Self {
        self.connections = connections;
        self
    }

    /// The ten-by-nine window agent A's predicate can answer for, which moves with the fly.
    fn in_window(&self, x: u8, y: u8) -> bool {
        let dx = i32::from(x) - i32::from(self.player.x);
        let dy = i32::from(y) - i32::from(self.player.y);
        (-4..=5).contains(&dx) && (-4..=4).contains(&dy)
    }

    fn walkable_tile(&self, x: u8, y: u8) -> bool {
        x < self.size.width && y < self.size.height && !self.walls.contains(&Tile::new(x, y))
    }
}

impl GameState for Ground {
    fn scene(&mut self) -> Scene {
        Scene::Overworld
    }

    fn player(&mut self) -> Option<Player> {
        Some(Player { map: self.map, x: self.player.x, y: self.player.y, facing: Facing::Down })
    }

    fn map_size(&mut self) -> Option<MapSize> {
        Some(self.size)
    }

    fn party(&mut self) -> Party {
        Party { mons: Vec::<Mon>::new(), active: None }
    }

    fn battle(&mut self) -> Option<Battle> {
        None
    }

    fn text_box(&mut self) -> TextBox {
        TextBox { open: false, waiting: false }
    }

    fn start_menu(&mut self) -> Option<StartMenu> {
        None
    }

    fn shop(&mut self) -> Option<Shop> {
        None
    }

    fn pc(&mut self) -> Option<Pc> {
        None
    }

    fn money(&mut self) -> u32 {
        0
    }

    fn bag(&mut self) -> Vec<BagItem> {
        Vec::new()
    }

    fn npcs(&mut self) -> Vec<Npc> {
        self.npcs.clone()
    }

    fn signs(&mut self) -> Vec<Sign> {
        Vec::new()
    }

    /// Agent A's predicate: the window, and `Unknown` outside it.
    fn walkable(&mut self, x: u8, y: u8) -> Walkable {
        if x >= self.size.width || y >= self.size.height {
            return Walkable::No;
        }
        if !self.in_window(x, y) {
            return Walkable::Unknown;
        }
        if self.walkable_tile(x, y) { Walkable::Yes } else { Walkable::No }
    }

    fn warps(&mut self) -> Vec<Warp> {
        self.warps.clone()
    }

    fn connections(&mut self) -> Connections {
        self.connections
    }
}

impl MacroState for Ground {
    fn map_grid(&mut self) -> Option<Arc<MapGrid>> {
        if !self.decoded {
            return None;
        }
        let mut grid = MapGrid::new(self.map, self.size.width, self.size.height);
        for y in 0..self.size.height {
            for x in 0..self.size.width {
                let walkable = self.walkable_tile(x, y);
                grid.set(
                    x,
                    y,
                    if walkable { FLOOR } else { WALL },
                    if walkable { Walkable::Yes } else { Walkable::No },
                );
            }
        }
        for (tile, facing) in &self.pair_walls {
            grid.wall(tile.x, tile.y, *facing);
        }
        Some(Arc::new(grid))
    }

    fn tile_visited(&mut self, x: u8, y: u8) -> bool {
        self.stood.contains(&Tile::new(x, y))
    }
}

/// Walk a route's presses and report where they land and whether every tile was walkable.
fn walk(ground: &Ground, from: Tile, steps: &[Facing]) -> (Tile, bool) {
    let mut at = from;
    let mut clean = true;
    for facing in steps {
        let Some(next) = at.step(*facing) else {
            clean = false;
            break;
        };
        if !ground.walkable_tile(next.x, next.y) {
            clean = false;
        }
        at = next;
    }
    (at, clean)
}

#[test]
fn one_plan_crosses_a_map_larger_than_the_window() {
    // Twenty by eighteen — Pallet Town's size — with a wall down the middle and one gap in it,
    // which is where a plan has to go and is off the screen from where the fly starts.
    let start = (2, 2);
    let goal = Tile::new(18, 16);
    let mut ground = Ground::new(20, 18, start).wall_column(10, 1);
    let route = path::route(&mut ground, &[goal]).expect("a route across the map");
    assert_eq!(route.goal, Some(0), "the goal itself, not an approach to it");
    let (landed, clean) = walk(&ground, Tile::new(start.0, start.1), &route.steps);
    assert_eq!(landed, goal);
    assert!(clean, "every tile of the plan is walkable ground");
    // The gap is at the top, so the plan is longer than the Manhattan distance and knows it.
    assert!(route.steps.len() > Tile::new(start.0, start.1).distance(goal) as usize);

    // The same map with only the window to read: the plan sets off through a wall it cannot see.
    let mut blind = Ground::new(20, 18, start).wall_column(10, 1).window_only();
    let route = path::route(&mut blind, &[goal]).expect("a route through the unknown");
    let (_, clean) = walk(&blind, Tile::new(start.0, start.1), &route.steps);
    assert!(!clean, "the window cannot see the wall, so the plan walks into it");
}

#[test]
fn the_frontier_is_the_nearest_unstood_tile_anywhere_on_the_map() {
    // A fly that has covered the ground around it. Nine tiles is the furthest corner of the
    // ten-by-nine window (five across and four down), so every tile the window can answer for has
    // been stood on and the nearest new ground is the first tile outside it.
    let mut ground = Ground::new(20, 18, (3, 3)).stood_around(9);
    let frontier = path::frontier(&mut ground);
    assert!(!frontier.is_empty(), "the rest of the map is still new ground");
    let (tile, facing) = frontier
        .iter()
        .copied()
        .min_by_key(|(tile, _)| tile.distance(Tile::new(3, 3)))
        .expect("a nearest frontier");
    let new_ground = tile.step(facing).expect("the ground it faces");
    assert!(!ground.tile_visited(new_ground.x, new_ground.y));
    assert_eq!(tile.distance(Tile::new(3, 3)), 9, "the near side of the unstood ground");
    // And a route to it, in one plan.
    let goals: Vec<Tile> = frontier.iter().map(|(tile, _)| *tile).collect();
    let route = path::route(&mut ground, &goals).expect("a route to the frontier");
    assert!(route.goal.is_some());

    // The window reading has nothing to offer here at all, which is the pad the operator saw
    // hanging: every tile it can answer for has been stood on.
    let mut blind = Ground::new(20, 18, (3, 3)).stood_around(9).window_only();
    assert!(path::frontier(&mut blind).is_empty());
}

#[test]
fn a_step_the_tables_refuse_is_planned_around_rather_than_walked_into() {
    // A tile-pair collision: both tiles passable, the step between them refused
    // (`data/tilesets/pair_collision_tile_ids.asm`). The wall is the only way east on its row, so
    // a search that did not know about it would plan straight through.
    let goal = Tile::new(3, 0);
    let mut ground = Ground::new(4, 3, (1, 0))
        .pair_wall((1, 0), Facing::Right)
        .pair_wall((2, 0), Facing::Left);
    let route = path::route(&mut ground, &[goal]).expect("a route round the pair wall");
    assert_eq!(route.steps.first(), Some(&Facing::Down), "round it, not through it");
    let (landed, clean) = walk(&ground, Tile::new(1, 0), &route.steps);
    assert_eq!(landed, goal);
    assert!(clean);
    assert!(!route.steps.is_empty());

    // Without the pair rule the same map is a straight line east.
    let mut open = Ground::new(4, 3, (1, 0));
    let route = path::route(&mut open, &[goal]).expect("a route east");
    assert_eq!(route.steps, vec![Facing::Right, Facing::Right]);
}

#[test]
fn a_connection_on_the_far_edge_is_a_goal_and_the_walls_along_it_are_not() {
    // A route's north edge, off the screen from where the fly stands, with two walkable tiles on
    // it. With the map decoded the exit list is those two tiles and not the whole row.
    let mut ground = Ground::new(20, 18, (3, 16))
        .connected(Connections { north: true, south: false, east: false, west: false });
    for x in 0..20u8 {
        if x != 7 && x != 8 {
            ground = ground.wall(x, 0);
        }
    }
    let exits = path::exits(&mut ground);
    let north: Vec<ExitId> = exits
        .iter()
        .filter(|exit| exit.id == ExitId::Edge(Edge::North))
        .map(|exit| exit.id)
        .collect();
    assert_eq!(north.len(), 2, "the two tiles a step north can be taken from");
    let goals: Vec<Tile> = exits
        .iter()
        .filter(|exit| exit.way == Way::Route && exit.id == ExitId::Edge(Edge::North))
        .map(|exit| exit.tile)
        .collect();
    assert!(goals.iter().all(|tile| tile.y == 0 && (tile.x == 7 || tile.x == 8)));
    let route = path::route(&mut ground, &goals).expect("a route to the connection");
    assert!(route.goal.is_some());
    let (landed, clean) = walk(&ground, Tile::new(3, 16), &route.steps);
    assert!(goals.contains(&landed));
    assert!(clean, "one plan, across sixteen tiles of map, every tile of it known ground");

    // With only the window, every tile of that edge is `Unknown` and so every one of them is an
    // exit, walls included: the search's own approach answer is what used to carry the walk.
    let mut blind = Ground::new(20, 18, (3, 16))
        .connected(Connections { north: true, south: false, east: false, west: false })
        .window_only();
    let count = path::exits(&mut blind)
        .iter()
        .filter(|exit| exit.id == ExitId::Edge(Edge::North))
        .count();
    assert_eq!(count, 20);
}
