//! Unit tests for the palette table, the pathfinder and every script's control flow.
//!
//! Everything here runs against a fake game, for the same reason the adapter's tests run against
//! synthetic WRAM traces: a test that needs the cartridge cannot say *why* it failed. The
//! cartridge-gated half — that `GO EXIT` really does leave Red's house, that `NEXT` really does
//! advance a text box — is in `tests/rom_macros.rs`.
//!
//! [`World`] is not a Game Boy, but the three facts about walking that decide whether `GO EXIT`
//! works are the measured ones from `docs/design/room-escape.md` section 3: a step takes a fixed
//! number of frames of held direction, a doormat on the bottom row fires when it is stepped
//! *down* onto or stepped down *off*, and a sideways step onto one does nothing. It implements
//! agent A's seam and nothing more, so a script that the seam cannot support fails here the way
//! it would on the cartridge.

use std::collections::{BTreeSet, VecDeque};

use crate::adapter::PlaceKind;
use crate::emulator::buttons;

use super::cartridge::{
    BLOCKED_MINUTES_DEFAULT, CHEAPEST_PURCHASE, Edge, ExitId, FACINGS, MacroState, Objective,
    TalkTarget, TargetKey, TargetLedger, Targets, Tile, battle_entry, button, item, price,
};
use super::geography::Amenity;
use super::executor::{
    FRAME_CAP, FRAMES_PER_PLANNED_TILE, MacroAbort, MacroMachine, MacroRefused, Refusal,
    WALK_FRAME_CEILING, walk_budget,
};
use super::palette::{
    MacroId, MacroKind, Palette, SLOTS, amenity_goals, answer_key, errand, facing_nurse,
    healthiest_other, heal_goals, nurse_prompt, rested_nurse,
    listing, losing, move_slot_bound, objective_goals, party_needs_rest, party_rested,
    poke_sprite, precondition, throw_slot, untalked_objects, untalked_people, ways,
};
use super::path::{self, Exit, Way};
use super::super::maps;
use super::plan;
use super::state::{
    Battle, BattleKind, BattleMenu, Connections, Cursor, EnemyMon, Facing, GameState, MapSize,
    Mon, Move, Npc, Party, Pc, Player, Scene, Shop, ShopScreen, Sign, StartMenu, Status, TextBox,
    Walkable, Warp,
};

/// `SPRITE_POKE_BALL`, the first still sprite: the picture id a fake gives an object rather than a
/// person (`constants/sprite_constants.asm`).
const BALL_SPRITE: u8 = 0x3d;

/// Which list is accepting input, and therefore what the shared cursor means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    /// Nothing: an animation, a message, or a screen the seam reports no cursor for.
    None,
    /// FIGHT / PKMN / ITEM / RUN.
    BattleMain,
    /// The move list, with this many moves.
    Moves(u8),
    /// The party list.
    BattleParty,
    /// The start menu.
    Start,
    /// A mart, on this screen.
    Shop(ShopScreen),
    /// A PC.
    Pc,
    /// The bag inside a battle, which agent A's seam does not report a cursor for.
    BattleBag,
}

/// What an A press in a menu opens next.
#[derive(Debug, Clone, Copy)]
struct Opens {
    list: List,
    cursor: u8,
    max: u8,
    grid: bool,
}

/// A fake game: agent A's seam over plain fields, plus the button behaviour that matters.
struct World {
    scene: Scene,
    map: u8,
    size: MapSize,
    player: Tile,
    facing: Facing,
    /// Tiles that are not walkable; everything else inside `size` is.
    walls: BTreeSet<Tile>,
    /// Tiles the predicate cannot answer for, which is agent A's windowed `Unknown`.
    unseen: BTreeSet<Tile>,
    warps: Vec<Warp>,
    connections: Connections,
    npcs: Vec<Npc>,
    signs: Vec<Sign>,

    list: List,
    cursor: u8,
    cursor_max: u8,
    /// Red's two-by-two menu geometry rather than a plain column.
    grid: bool,

    battle: Option<(BattleKind, bool, bool)>,
    mons: Vec<Mon>,
    active: Option<u8>,
    /// The Pokémon on the other side, when a test is about which one it is.
    ///
    /// `None` is the seam answering nothing, which is what most of these fixtures want: the
    /// species only matters to `THROW BALL`'s precondition (section 12.9).
    enemy: Option<EnemyMon>,

    money: u32,
    bag: Vec<(u8, u8)>,
    stock: Vec<u8>,
    /// Tiles the game lets the player talk *over*: a mart's or a centre's counter.
    counters: BTreeSet<Tile>,
    /// Errands this run has discharged (`docs/design/macros.md` section 13).
    areas: BTreeSet<(Amenity, u8)>,
    /// Tiles the cartridge pushes the fly off (`infra/docs/macros-traps.md` row 37).
    pushes: BTreeSet<Tile>,
    /// Maps a `GO FRONTIER` has proved it cannot reach the frontier of (section 12.14).
    ///
    /// Written by [`drive`] from the machine, exactly as `PokemonPalette` writes it in the sim
    /// loop, so a test sees the ledger the next decision would see.
    exhausted: BTreeSet<u8>,
    visited: BTreeSet<ExitId>,
    /// Tiles of this map the run has stood on, for `GO FRONTIER` and the plan's "untalked" test.
    stood: BTreeSet<Tile>,
    /// Maps the run has been on, for `GO ROUTE`'s unvisited interior.
    seen_maps: BTreeSet<u8>,
    /// Where the ladder's next rung is, for `GO OBJECTIVE`.
    objective: Option<Objective>,
    /// What this session has talked to, for `GO NPC`, `GO ITEM` and `TALK`.
    talked: BTreeSet<TalkTarget>,
    /// The session's blocked- and reached-target ledgers, which [`run`] writes for the machine
    /// exactly as `PokemonPalette` does in the sim loop (`docs/design/macros.md` section 12).
    targets: Targets,

    /// Steps this world refuses: a ledge, a tile-pair collision, somebody in the way.
    ///
    /// Directed, because that is what the cartridge encodes ([`path::Refusal`]): the entry
    /// `((3, 4), Up)` refuses a step north out of (3, 4) and says nothing about coming back down.
    refuse: Vec<(Tile, Facing)>,
    /// Agent A's ten-by-nine window, when a test wants the real predicate's blind spot.
    ///
    /// `pokemon_red::state::walkable` can only answer for `x - 4 ..= x + 5` and `y - 4 ..= y + 4`,
    /// and the window moves with the player: every farther tile reads [`Walkable::Unknown`]. A
    /// fake with a static `unseen` set cannot show what that does to a search that re-plans every
    /// tile, which is the whole of the 2026-09-17 stall.
    window: bool,
    /// Frames of continuous held direction one tile takes.
    tile_frames: u32,
    /// A world where the player never moves, for the three-failure rule.
    glue: bool,
    /// B presses still needed to back out of a menu.
    b_to_close: u8,
    /// START presses still needed to open the start menu.
    start_to_open: u8,
    /// What the next A presses open.
    opens: VecDeque<Opens>,
    /// Frames the cartridge spends *drawing* a list an A press opened, before it accepts input.
    ///
    /// Zero everywhere but the one test this is for. On the cartridge it is not zero and it is not
    /// bounded by the twenty frames a script's `settle` waits: measured 2026-09-22, `THROW BALL`
    /// pressed ITEM, settled, and then read the battle *menu*'s cursor because the bag had not
    /// drawn yet -- 63 starts, 63 `blocked` (section 12.11).
    opens_draw_in: u32,
    /// The list an A press opened and the frame it starts accepting input on.
    pending: Option<(u32, Opens)>,
    /// A scene the world switches to at this frame, for the abort rule.
    switch: Option<(u32, Scene)>,
    /// A frame at which the cartridge heals the party, which is what a Pokémon Center does while
    /// its text box is open (`docs/design/macros.md` section 13).
    heal_at: Option<u32>,
    /// Whether a text box is drawn on a scene that is not [`Scene::Dialog`]
    /// ([`MacroState::text_open`]).
    ///
    /// `Dialog` *is* an open box, so the reading is true there by construction; the field is for
    /// `Unknown`, which holds both a screen with words on it -- the Pokedex, the trainer card,
    /// OPTION -- and a frame of the overworld the cartridge is driving (section 12.13).
    box_open: bool,
    /// Whether the two-option YES/NO box is the thing on screen ([`MacroState::yes_no_prompt`]).
    ///
    /// A field rather than a shape of the `list`, because on the cartridge it is a *drawn box*
    /// beside a cursor the game never clears, and what the palette asks is only "is a choice
    /// open" (section 12.12).
    prompt: bool,
    /// Whether the cartridge is driving the player right now ([`MacroState::scripted`]).
    scripted: bool,
    /// A frame at which the cartridge takes the joypad, which is what the Viridian gate does.
    scripted_at: Option<u32>,
    /// Every completed pulse, in order.
    pulses: Vec<u8>,
    held: u32,
    previous: u8,
    frames: u32,
}

fn mon(slot: u8, hp: u16, max_hp: u16, moves: &[(u8, u8)]) -> Mon {
    let mut slots = [None, None, None, None];
    for (index, (id, pp)) in moves.iter().enumerate() {
        slots[index] = Some(Move { id: *id, pp: *pp, pp_up: 0 });
    }
    Mon {
        slot,
        species: slot + 1,
        level: 5,
        hp,
        max_hp,
        status: Status::Healthy,
        moves: slots,
    }
}

impl World {
    /// An empty eight-by-eight room with no exits and nothing in it.
    fn room() -> Self {
        Self {
            scene: Scene::Overworld,
            map: 0x25,
            size: MapSize { width: 8, height: 8 },
            player: Tile::new(3, 3),
            facing: Facing::Down,
            walls: BTreeSet::new(),
            unseen: BTreeSet::new(),
            warps: Vec::new(),
            connections: Connections::default(),
            npcs: Vec::new(),
            signs: Vec::new(),
            list: List::None,
            cursor: 0,
            cursor_max: 0,
            grid: false,
            battle: None,
            enemy: None,
            mons: vec![mon(0, 20, 20, &[(33, 30)])],
            active: None,
            money: 0,
            bag: Vec::new(),
            counters: BTreeSet::new(),
            areas: BTreeSet::new(),
            pushes: BTreeSet::new(),
            exhausted: BTreeSet::new(),
            stock: Vec::new(),
            visited: BTreeSet::new(),
            stood: BTreeSet::new(),
            seen_maps: BTreeSet::new(),
            objective: None,
            talked: BTreeSet::new(),
            targets: Targets::default(),
            refuse: Vec::new(),
            window: false,
            tile_frames: 16,
            glue: false,
            b_to_close: 1,
            start_to_open: 1,
            opens: VecDeque::new(),
            opens_draw_in: 0,
            pending: None,
            switch: None,
            heal_at: None,
            box_open: false,
            prompt: false,
            scripted: false,
            scripted_at: None,
            pulses: Vec::new(),
            held: 0,
            previous: buttons::NONE,
            frames: 0,
        }
    }

    /// Red's ground floor as the survey in `docs/design/room-escape.md` measured it: two doormats
    /// side by side on the bottom row and a staircase inside the room.
    fn ground_floor() -> Self {
        let mut world = Self::room();
        world.warps = vec![
            Warp { x: 7, y: 1, destination_warp: 2, destination_map: 0x26 },
            Warp { x: 2, y: 7, destination_warp: 1, destination_map: 0x00 },
            Warp { x: 3, y: 7, destination_warp: 1, destination_map: 0x00 },
        ];
        world
    }

    /// A wild battle on the player's turn: one hurt Pokémon out, one healthy on the bench, one
    /// fainted, and three moves of which the last is a status move.
    fn battle() -> Self {
        let mut world = Self::room();
        world.scene = Scene::Battle { own_turn: true, forced_switch: false };
        world.battle = Some((BattleKind::Wild, true, false));
        world.list = List::BattleMain;
        world.cursor_max = 3;
        world.grid = true;
        world.mons = vec![
            mon(0, 4, 20, &[(33, 30), (52, 20), (45, 40)]),
            mon(1, 18, 20, &[(33, 30)]),
            mon(2, 0, 20, &[(33, 30)]),
        ];
        world.active = Some(0);
        world
    }

    /// Viridian City's mart, as `data/maps/objects/ViridianMart.asm` declares it.
    ///
    /// Four blocks by four, so eight tiles by eight; two doormats on the bottom row at (3, 7) and
    /// (4, 7); `object_event 0, 5, SPRITE_CLERK` behind a counter that runs down column 1. The
    /// counter and the floor behind it are walls, which is the fact that matters: **none of the
    /// four tiles around the clerk can be stood on**, so an approach that only knew how to stand
    /// beside somebody could never reach the counter at all.
    fn mart() -> Self {
        let mut world = Self::room();
        world.map = maps::VIRIDIAN_MART;
        world.player = Tile::new(3, 6);
        world.warps = vec![
            Warp { x: 3, y: 7, destination_warp: 1, destination_map: 0xff },
            Warp { x: 4, y: 7, destination_warp: 1, destination_map: 0xff },
        ];
        world.npcs = vec![Npc {
            slot: 1,
            picture: poke_sprite::CLERK,
            x: 0,
            y: 5,
            facing: Facing::Right,
        }];
        world.counters = BTreeSet::from([Tile::new(1, 5)]);
        world.walls = BTreeSet::from([
            Tile::new(0, 4),
            Tile::new(0, 5),
            Tile::new(0, 6),
            Tile::new(1, 5),
        ]);
        world.money = 3_000;
        world
    }

    /// Viridian City's Pokémon Center, as `data/maps/objects/ViridianPokecenter.asm` declares it.
    ///
    /// Seven blocks by four, so fourteen tiles by eight; `object_event 3, 1, SPRITE_NURSE` behind
    /// the counter tile at (3, 2), with the wall above her and the floor either side of her not
    /// standable. The only way to talk to her is from (3, 3), facing up, over the counter.
    fn center() -> Self {
        let mut world = Self::room();
        world.map = maps::VIRIDIAN_POKECENTER;
        world.size = MapSize { width: 14, height: 8 };
        world.player = Tile::new(3, 6);
        world.warps = vec![
            Warp { x: 3, y: 7, destination_warp: 1, destination_map: 0xff },
            Warp { x: 4, y: 7, destination_warp: 1, destination_map: 0xff },
        ];
        world.npcs = vec![Npc {
            slot: 1,
            picture: poke_sprite::NURSE,
            x: 3,
            y: 1,
            facing: Facing::Down,
        }];
        world.counters = BTreeSet::from([Tile::new(3, 2)]);
        world.walls = BTreeSet::from([
            Tile::new(3, 0),
            Tile::new(2, 1),
            Tile::new(4, 1),
            Tile::new(3, 2),
        ]);
        world
    }

    /// [`World::center`] with the fly at the counter facing the nurse, mid-conversation.
    ///
    /// The rung-10 state (`infra/docs/macros-traps.md` row 41): map `0x3a` at (3, 3) facing up,
    /// a text box open, the nurse two tiles away over the counter at (3, 1).
    fn at_the_nurse() -> Self {
        let mut world = Self::center();
        world.player = Tile::new(3, 3);
        world.facing = Facing::Up;
        world.scene = Scene::Dialog;
        world
    }

    fn at(mut self, x: u8, y: u8) -> Self {
        self.player = Tile::new(x, y);
        self
    }

    fn wall(mut self, tiles: &[(u8, u8)]) -> Self {
        for (x, y) in tiles {
            self.walls.insert(Tile::new(*x, *y));
        }
        self
    }

    fn frame(&mut self, mask: u8) {
        self.frames += 1;
        if let Some((at, next)) = self.pending
            && self.frames >= at
        {
            self.list = next.list;
            self.cursor = next.cursor;
            self.cursor_max = next.max;
            self.grid = next.grid;
            self.pending = None;
        }
        if let Some(at) = self.scripted_at
            && self.frames >= at
        {
            self.scripted = true;
        }
        if let Some((at, scene)) = self.switch
            && self.frames >= at
        {
            self.scene = scene;
            self.switch = None;
        }
        if let Some(at) = self.heal_at
            && self.frames >= at
        {
            for mon in &mut self.mons {
                mon.hp = mon.max_hp;
                mon.status = Status::Healthy;
            }
            self.heal_at = None;
        }
        if mask == buttons::NONE && self.previous != buttons::NONE {
            self.pulses.push(self.previous);
            self.on_pulse(self.previous);
        }
        if self.scene == Scene::Overworld {
            match direction_of(mask) {
                Some(facing) => {
                    if mask == self.previous {
                        self.held += 1;
                    } else {
                        self.held = 1;
                        self.facing = facing;
                    }
                    if self.held >= self.tile_frames {
                        self.held = 0;
                        self.attempt(facing);
                    }
                }
                None => self.held = 0,
            }
        }
        self.previous = mask;
    }

    fn on_pulse(&mut self, mask: u8) {
        if self.scene == Scene::Overworld {
            if mask == buttons::START {
                self.start_to_open = self.start_to_open.saturating_sub(1);
                if self.start_to_open == 0 {
                    self.scene = Scene::Menu;
                    self.list = List::Start;
                    self.cursor_max = 5;
                }
            }
            return;
        }
        if let Some(facing) = direction_of(mask) {
            self.move_cursor(facing);
        }
        if mask == buttons::B {
            self.b_to_close = self.b_to_close.saturating_sub(1);
            if self.b_to_close == 0 {
                self.scene = Scene::Overworld;
                self.list = List::None;
            }
        }
        if mask == buttons::A
            && let Some(next) = self.opens.pop_front()
        {
            if self.opens_draw_in > 0 {
                self.pending = Some((self.frames + self.opens_draw_in, next));
            } else {
                self.list = next.list;
                self.cursor = next.cursor;
                self.cursor_max = next.max;
                self.grid = next.grid;
            }
        }
    }

    fn move_cursor(&mut self, facing: Facing) {
        if self.list == List::None {
            return;
        }
        let here = i16::from(self.cursor);
        let target = if self.grid {
            // Red's battle menu: two **columns** of two, indexed by column. `wCurrentMenuItem` is
            // the row inside the column the cursor is in and selection adds two for the right
            // column, so the order is FIGHT, ITEM, PKMN, RUN -- up and down move one inside a
            // column, left and right move two across (surveyed 2026-09-22; the fake had it
            // row-major, which is the same mistake `battle_entry` had).
            match facing {
                Facing::Down if here % 2 == 0 => here + 1,
                Facing::Up if here % 2 == 1 => here - 1,
                Facing::Right => here + 2,
                Facing::Left => here - 2,
                _ => here,
            }
        } else {
            match facing {
                Facing::Down | Facing::Right => here + 1,
                Facing::Up | Facing::Left => here - 1,
            }
        };
        if target >= 0 && target <= i16::from(self.cursor_max) {
            self.cursor = target as u8;
        }
    }

    /// One attempted overworld step, with the cartridge's warp behaviour.
    fn attempt(&mut self, facing: Facing) {
        if self.glue || self.refuse.contains(&(self.player, facing)) {
            return;
        }
        let (dx, dy) = facing.delta();
        let x = i16::from(self.player.x) + dx;
        let y = i16::from(self.player.y) + dy;
        let off_map =
            x < 0 || y < 0 || x >= i16::from(self.size.width) || y >= i16::from(self.size.height);
        if off_map {
            // Standing on a doormat and stepping off it goes through the door; otherwise only a
            // connected edge leads anywhere.
            let mat = self
                .warps
                .iter()
                .find(|warp| warp.x == self.player.x && warp.y == self.player.y)
                .copied();
            if let Some(warp) = mat {
                self.map = warp.destination_map;
            } else if self.connected(facing) {
                self.map = 0xfe;
            }
            return;
        }
        let next = Tile::new(x as u8, y as u8);
        if self.walkable(next.x, next.y) != Walkable::Yes {
            return;
        }
        self.player = next;
        let Some(warp) =
            self.warps.iter().find(|warp| warp.x == next.x && warp.y == next.y).copied()
        else {
            return;
        };
        // An interior warp fires on the step onto it; a doormat only when the step onto it is the
        // same direction the step off the map would be.
        match outward(next, self.size.height) {
            None => self.map = warp.destination_map,
            Some(out) if out == facing => self.map = warp.destination_map,
            Some(_) => {}
        }
    }

    fn connected(&self, facing: Facing) -> bool {
        match facing {
            Facing::Up => self.connections.north,
            Facing::Down => self.connections.south,
            Facing::Left => self.connections.west,
            Facing::Right => self.connections.east,
        }
    }

    fn cursor_state(&self) -> Cursor {
        Cursor {
            current: self.cursor,
            max: self.cursor_max,
            top_y: 0,
            top_x: 0,
            watched_keys: 0xff,
        }
    }
}

impl GameState for World {
    fn scene(&mut self) -> Scene {
        self.scene
    }

    fn player(&mut self) -> Option<Player> {
        Some(Player { map: self.map, x: self.player.x, y: self.player.y, facing: self.facing })
    }

    fn map_size(&mut self) -> Option<MapSize> {
        Some(self.size)
    }

    fn party(&mut self) -> Party {
        Party { mons: self.mons.clone(), active: self.active }
    }

    fn battle(&mut self) -> Option<Battle> {
        let (kind, own_turn, forced_switch) = self.battle?;
        let menu = match self.list {
            List::BattleMain => BattleMenu::Main { cursor: self.cursor },
            List::Moves(count) => BattleMenu::Moves { cursor: Some(self.cursor), count },
            List::BattleParty => BattleMenu::Party { cursor: self.cursor },
            // The bag opened from a battle's ITEM entry, which the seam reports a cursor for since
            // section 14: `ITEM` and `THROW BALL` both navigate it by reading.
            List::BattleBag => {
                BattleMenu::Bag { cursor: self.cursor, count: self.cursor_max.saturating_add(1) }
            }
            _ => BattleMenu::None,
        };
        let own = self.active.and_then(|slot| self.mons.iter().find(|mon| mon.slot == slot)).copied();
        Some(Battle { kind, own_turn, forced_switch, menu, own, enemy: self.enemy })
    }

    fn text_box(&mut self) -> TextBox {
        let talking = matches!(self.scene, Scene::Dialog | Scene::Unknown);
        TextBox { open: talking, waiting: talking }
    }

    fn start_menu(&mut self) -> Option<StartMenu> {
        (self.list == List::Start)
            .then(|| StartMenu { cursor: self.cursor_state(), items: self.cursor_max + 1 })
    }

    fn shop(&mut self) -> Option<Shop> {
        match self.list {
            List::Shop(screen) => Some(Shop { screen, cursor: self.cursor_state() }),
            _ => None,
        }
    }

    fn pc(&mut self) -> Option<Pc> {
        (self.list == List::Pc).then(|| Pc { cursor: self.cursor_state() })
    }

    fn money(&mut self) -> u32 {
        self.money
    }

    fn bag(&mut self) -> Vec<super::state::BagItem> {
        self.bag.iter().map(|(id, count)| super::state::BagItem { id: *id, count: *count }).collect()
    }

    fn npcs(&mut self) -> Vec<Npc> {
        self.npcs.clone()
    }

    fn signs(&mut self) -> Vec<Sign> {
        self.signs.clone()
    }

    fn walkable(&mut self, x: u8, y: u8) -> Walkable {
        let tile = Tile::new(x, y);
        let offscreen = self.window && {
            let dx = i32::from(x) - i32::from(self.player.x);
            let dy = i32::from(y) - i32::from(self.player.y);
            !((-4..=5).contains(&dx) && (-4..=4).contains(&dy))
        };
        if x >= self.size.width || y >= self.size.height {
            Walkable::No
        } else if offscreen || self.unseen.contains(&tile) {
            Walkable::Unknown
        } else if self.walls.contains(&tile) {
            Walkable::No
        } else {
            Walkable::Yes
        }
    }

    fn warps(&mut self) -> Vec<Warp> {
        self.warps.clone()
    }

    fn connections(&mut self) -> Connections {
        self.connections
    }
}

impl MacroState for World {
    fn scripted(&mut self) -> bool {
        self.scripted
    }

    /// `wFontLoaded` is set for every dialogue box, which is what `Dialog` is; on `Unknown` the
    /// fixture has to say, because that is the reading that tells a screen from a scripted
    /// overworld frame (section 12.13).
    fn text_open(&mut self) -> bool {
        self.scene == Scene::Dialog || self.box_open
    }

    fn frontier_exhausted(&mut self) -> bool {
        self.exhausted.contains(&self.map)
    }

    /// A drawn box is what the reading rests on, so a prompt cannot be open with no box open:
    /// `pokemon_red::state::yes_no_prompt` gates on `wFontLoaded` before it looks at the tiles.
    fn yes_no_prompt(&mut self) -> bool {
        self.prompt && self.scene == Scene::Dialog
    }

    fn shop_stock(&mut self) -> Vec<u8> {
        self.stock.clone()
    }

    fn counter_tile(&mut self, x: u8, y: u8) -> bool {
        self.counters.contains(&Tile::new(x, y))
    }

    fn area_visited(&mut self, kind: Amenity, area: u8) -> bool {
        self.areas.contains(&(kind, area))
    }

    fn pushed_tile(&mut self, x: u8, y: u8) -> bool {
        self.pushes.contains(&Tile::new(x, y))
    }

    fn exit_visited(&mut self, exit: ExitId) -> bool {
        self.visited.contains(&exit)
    }

    fn tile_visited(&mut self, x: u8, y: u8) -> bool {
        self.stood.contains(&Tile::new(x, y))
    }

    fn map_visited(&mut self, map: u8) -> bool {
        self.seen_maps.contains(&map)
    }

    fn talked(&mut self, target: TalkTarget) -> bool {
        self.talked.contains(&target)
    }

    fn blocked(&mut self, target: TargetKey) -> bool {
        self.targets.blocked(self.map, target)
    }

    fn reached(&mut self, target: TargetKey) -> bool {
        self.targets.reached(self.map, target)
    }

    fn objective(&mut self) -> Option<Objective> {
        self.objective
    }
}

fn direction_of(mask: u8) -> Option<Facing> {
    FACINGS.into_iter().find(|facing| button(*facing) == mask)
}

/// The cartridge's rule, as `docs/design/room-escape.md` section 3 measured it: a doormat is on
/// the bottom row and needs a DOWN step; every other warp fires on the step onto it.
fn outward(tile: Tile, height: u8) -> Option<Facing> {
    (tile.y + 1 == height).then_some(Facing::Down)
}

/// Start `kind`'s slot and run to completion, returning the outcome.
///
/// The slot is found by name in the palette the world's own scene produces, so a test never
/// hardcodes a slot number that section 3 could move.
fn run(world: &mut World, kind: MacroKind) -> Result<MacroAbort, MacroRefused> {
    let mut machine = MacroMachine::new(0x1234_5678);
    run_with(&mut machine, world, kind)
}

/// [`run`], on a machine that outlives the call.
///
/// The executor's session state — the routes a frame cap cut short — lives on the machine, so a
/// test about resuming a walk has to press the same machine twice, exactly as the sim loop does
/// (`PokemonPalette` owns one for the run).
fn run_with(
    machine: &mut MacroMachine,
    world: &mut World,
    kind: MacroKind,
) -> Result<MacroAbort, MacroRefused> {
    let (palette, slot) = pick(world, kind);
    drive(machine, &palette, slot, world)
}

/// [`run_with`], on a palette the caller dealt: start the slot, drive to the outcome, and do the
/// driver's own bookkeeping after it.
fn drive(
    machine: &mut MacroMachine,
    palette: &Palette,
    slot: MacroId,
    world: &mut World,
) -> Result<MacroAbort, MacroRefused> {
    let kind = palette.slot(slot).expect("the caller dealt this slot").kind;
    if let Err(refused) = machine.start(palette, slot, world) {
        // The driver drains the ledgers after every `start`, refusal included: a `no route`
        // refusal earns blocked entries and presses nothing, so nothing else would collect them
        // (`PokemonPalette::start`).
        while let Some((map, target)) = machine.take_blocked() {
            world.targets.record_blocked(map, target);
        }
        if let Some(map) = machine.take_exhausted() {
            world.exhausted.insert(map);
        }
        return Err(refused);
    }
    // A walk's cap is its plan's, so the bound here is the ceiling on any macro plus slack.
    for _ in 0..(WALK_FRAME_CEILING + 64) {
        match machine.step(world) {
            Some(mask) => world.frame(mask),
            None => break,
        }
    }
    assert!(machine.running().is_none(), "{} never gave the buttons back", kind.name());
    // The driver hands the machine every frame, not only the ones a macro owns, because whether a
    // conversation ended cleanly is a question about the frames after `TALK` gave the buttons back
    // (`PokemonPalette::observe`, `docs/design/macros.md` section 12.4). One frame is enough here:
    // the fake's scene does not linger.
    machine.observe_frame(world);
    // The loop's own bookkeeping, so a test sees what the next decision would see: whatever the
    // finish earned goes into the session's ledgers, which is `PokemonPalette::record_talk`'s job
    // in the sim loop and this line's here.
    while let Some((map, target)) = machine.take_blocked() {
        world.targets.record_blocked(map, target);
    }
    if let Some(map) = machine.take_exhausted() {
        world.exhausted.insert(map);
    }
    if let Some((map, target, closer)) = machine.take_timeout() {
        world.targets.record_timeout(map, target, closer);
    }
    if let Some((map, target)) = machine.take_reached() {
        world.targets.record_reached(map, target);
    }
    if let Some((map, target)) = machine.take_talked() {
        assert_eq!(map, world.map);
        world.talked.insert(target);
    }
    Ok(machine.outcome().expect("a finished macro has an outcome").1)
}

/// A palette of exactly one button, for a script whose macro no scene binds any more.
///
/// `MENU` is the only one (section 12.11): it is still a type, a population, a tag and a script --
/// the roles and `--print-compatibility` depend on the type list -- and it is on no pad, so
/// [`pick`] cannot find it. Its script is exercised here rather than deleted, because what took it
/// off the pad is that the start menu has nothing in it for the fly and not that pressing START is
/// wrong.
fn forced(kind: MacroKind, world: &mut World) -> (Palette, MacroId) {
    let mut slots = [None; super::palette::SLOTS];
    slots[usize::from(kind.slot())] = Some(super::palette::MacroSpec::of(kind));
    (Palette { scene: world.scene(), slots }, MacroId(kind.slot()))
}

fn pick(world: &mut World, kind: MacroKind) -> (Palette, MacroId) {
    let scene = world.scene();
    // The shipping mode is `PaletteMode::Plan` (`MacroMode::Macros` in `flysim::snapshot`), which
    // deals the *set* section 12 asks for; `Palette::for_scene` is the older six fixed channels and
    // does not carry `GO OBJECTIVE` at all. Tests ask the channel palette first, because most of
    // them are about a macro that is on both, and fall back to the plan's — which is what
    // `on_the_pad` has always asked.
    let palette = Palette::for_scene(scene, world);
    if let Some(slot) = palette.slots.iter().position(|slot| slot.is_some_and(|spec| spec.kind == kind))
    {
        return (palette, MacroId(slot as u8));
    }
    let palette = plan::plan_for(scene, world);
    let slot = palette
        .slots
        .iter()
        .position(|slot| slot.is_some_and(|spec| spec.kind == kind))
        .unwrap_or_else(|| panic!("{} is not on the {} pad", kind.name(), scene.label()));
    (palette, MacroId(slot as u8))
}

/// The macros a palette binds, in type order, which is also slot order (section 14).
///
/// The bound ones only: with a slot per type an unbound cell is a hole in the array rather than a
/// place in a row of six, so listing the holes would be listing the twenty-odd types this scene
/// simply does not have.
fn names(palette: &Palette) -> Vec<&'static str> {
    palette.slots.iter().flatten().map(|spec| spec.name).collect()
}

// ---------------------------------------------------------------------------------------------
// Section 3: the palette table
// ---------------------------------------------------------------------------------------------

#[test]
fn the_title_screen_has_no_palette() {
    let mut world = World::room();
    assert_eq!(Palette::for_scene(Scene::Title, &mut world).bound(), 0);
}

#[test]
fn the_indoor_overworld_row_is_section_nine_ones_row() {
    let mut world = World::ground_floor().wall(&[(3, 4)]);
    // One person and one object, so the sprite list holds both kinds. `GO NPC` is on the indoor
    // pad too since section 14 -- the six-slot cap was the only reason it was not.
    world.npcs = vec![
        Npc { slot: 1, picture: 1, x: 4, y: 3, facing: Facing::Down },
        Npc { slot: 2, picture: BALL_SPRITE, x: 3, y: 4, facing: Facing::Down },
    ];
    // A second object away from the tile ahead, because the ball at (3, 4) is what the fly is
    // facing and a thing already faced is `TALK`'s and not `GO ITEM`'s (2026-09-17).
    world.signs = vec![Sign { x: 6, y: 6, text_id: 3 }];
    let palette = Palette::for_scene(Scene::Overworld, &mut world);
    assert_eq!(
        names(&palette),
        ["GO OUT", "GO WARP", "GO ITEM", "GO NPC", "GO FRONTIER", "TALK"],
        "and no `MENU`, which section 12.11 took off this row"
    );
}

#[test]
fn the_outdoor_overworld_row_has_the_route_and_no_passage_in_it() {
    // Pallet Town: no building to leave and no floor to change, so the route onward is there and
    // `GO OUT` and `GO WARP` are not (section 9.1).
    let mut world = World::ground_floor();
    world.map = 0x00;
    world.connections.south = true;
    world.npcs = vec![Npc { slot: 1, picture: 1, x: 4, y: 3, facing: Facing::Down }];
    let palette = Palette::for_scene(Scene::Overworld, &mut world);
    assert_eq!(names(&palette), ["GO ROUTE", "GO NPC", "GO FRONTIER"]);
}

#[test]
fn an_overworld_with_no_way_out_leaves_the_leaving_buttons_unbound() {
    let mut world = World::room();
    let palette = Palette::for_scene(Scene::Overworld, &mut world);
    assert_eq!(palette.slot(MacroId(MacroKind::GoOut.slot())), None, "a sealed room binds no way out");
    assert_eq!(names(&palette), ["GO FRONTIER"], "and there is always ground to cover");
}

#[test]
fn a_warp_is_classified_by_where_it_goes() {
    // Section 9.1, on Red's ground floor: the staircase leads to another interior map and the two
    // doormats lead outdoors, and that is the whole difference between GO WARP and GO OUT.
    let mut world = World::ground_floor();
    let exits = path::exits(&mut world);
    let way_of = |tile: Tile, exits: &[Exit]| {
        exits.iter().find(|exit| exit.tile == tile).expect("an exit there").way
    };
    assert_eq!(way_of(Tile::new(7, 1), &exits), Way::Passage, "the staircase");
    assert_eq!(way_of(Tile::new(2, 7), &exits), Way::Exit, "the left doormat");
    assert_eq!(way_of(Tile::new(3, 7), &exits), Way::Exit, "the right doormat");
    assert!(precondition(MacroKind::GoOut, &mut world));
    assert!(precondition(MacroKind::GoWarp, &mut world));
    assert!(!precondition(MacroKind::GoRoute, &mut world), "indoors there is no route");

    // Outdoors every warp is a door into somewhere and every edge is the next area.
    let mut town = World::ground_floor();
    town.map = 0x00;
    assert!(path::exits(&mut town).iter().all(|exit| exit.way == Way::Route));
    assert!(!precondition(MacroKind::GoOut, &mut town), "outdoors there is nothing to leave");
    assert!(!precondition(MacroKind::GoWarp, &mut town));
    assert!(precondition(MacroKind::GoRoute, &mut town));
}

#[test]
fn a_connected_edge_is_a_way_out_even_with_no_warps() {
    let mut world = World::room();
    world.connections.south = true;
    assert!(precondition(MacroKind::GoOut, &mut world));
}

#[test]
fn go_npc_is_unbound_when_nobody_is_visible() {
    let mut world = World::room();
    assert!(!precondition(MacroKind::GoNpc, &mut world));
}

#[test]
fn go_npc_and_go_item_split_the_sprite_list_between_them() {
    // The sixteen sprite slots hold the whole `object_event` list, people and objects alike, so
    // "is there a sprite" would have bound both slots for either. `Npc::person` is the split.
    let mut balls = World::room();
    balls.npcs = vec![Npc { slot: 1, picture: BALL_SPRITE, x: 4, y: 3, facing: Facing::Down }];
    assert!(!precondition(MacroKind::GoNpc, &mut balls), "a Pokeball is not a person");
    assert!(precondition(MacroKind::GoItem, &mut balls));

    let mut person = World::room();
    person.npcs = vec![Npc { slot: 1, picture: 3, x: 4, y: 3, facing: Facing::Down }];
    assert!(precondition(MacroKind::GoNpc, &mut person));
    assert!(!precondition(MacroKind::GoItem, &mut person), "a person is not an object");
}

#[test]
fn go_item_binds_on_a_sign_with_no_sprites_at_all() {
    // A sign is a `bg_event`, not a sprite: nothing about it reaches `npcs` or `walkable`, which
    // is why the old `LOOK` precondition (a wall ahead) was the only thing that ever saw one.
    let mut world = World::room();
    assert!(!precondition(MacroKind::GoItem, &mut world), "an empty room has nothing to face");
    world.signs = vec![Sign { x: 2, y: 0, text_id: 1 }];
    assert!(precondition(MacroKind::GoItem, &mut world));
    let palette = Palette::for_scene(Scene::Overworld, &mut world);
    assert!(names(&palette).contains(&"GO ITEM"));
}

#[test]
fn the_overworld_row_binds_no_object_slot_in_an_empty_room() {
    let mut world = World::room();
    let palette = Palette::for_scene(Scene::Overworld, &mut world);
    assert_eq!(palette.slots[1], None, "nobody to walk to");
    assert_eq!(palette.slots[2], None, "and nothing to walk to");
    assert_eq!(
        palette.slots[4], None,
        "and nothing to press A at: section 12 keeps TALK off the pad unless the tile ahead \
         holds something untalked"
    );
}

#[test]
fn interactables_are_the_objects_and_the_signs_deduplicated() {
    let mut world = World::room();
    world.npcs = vec![
        Npc { slot: 1, picture: 3, x: 1, y: 1, facing: Facing::Down },
        Npc { slot: 2, picture: BALL_SPRITE, x: 5, y: 2, facing: Facing::Down },
        Npc { slot: 3, picture: BALL_SPRITE + 2, x: 2, y: 0, facing: Facing::Down },
    ];
    // One sign on a tile an object already occupies, and one of its own.
    world.signs = vec![Sign { x: 2, y: 0, text_id: 1 }, Sign { x: 7, y: 7, text_id: 2 }];
    assert_eq!(
        path::interactables(&mut world),
        vec![Tile::new(2, 0), Tile::new(5, 2), Tile::new(7, 7)],
        "the person is not one, and the doubled tile is one goal"
    );
}

#[test]
fn the_dialog_row_is_next_yes_and_no() {
    let mut world = World::room();
    let palette = Palette::for_scene(Scene::Dialog, &mut world);
    assert_eq!(names(&palette), ["NEXT", "YES", "NO"]);
}

#[test]
fn an_unknown_scene_is_dialog_with_advance_only() {
    let mut world = World::room();
    world.box_open = true;
    let palette = Palette::for_scene(Scene::Unknown, &mut world);
    // Row 9 of `infra/docs/macros-traps.md`, closed by section 13.1: B is what leaves the Pokédex,
    // the trainer card and OPTION, and all three read `Unknown` with a box drawn.
    assert_eq!(names(&palette), ["NEXT", "BACK"]);
}

#[test]
fn an_unknown_frame_with_no_box_on_it_deals_nothing() {
    // Section 12.13, the rung-10 Pewter loop. The other half of `Unknown` is the overworld with
    // the cartridge driving -- a warp in flight, a push-back, the museum guide walking the fly in
    // -- where `scene::detect` falls through because the buttons are not reaching the player.
    // There is no box to advance and no screen to leave, so `NEXT` and `BACK` are an A and a B
    // pressed into somebody else's script: they change nothing and they complete where the fly
    // stands, which is section 12.2's trap. The pad is empty and the fly waits.
    let mut world = World::room();
    world.scripted = true;
    assert!(!world.text_open());
    let palette = Palette::for_scene(Scene::Unknown, &mut world);
    assert_eq!(palette.bound(), 0, "an A and a B into a script are not buttons");
    // And the moment the cartridge draws something, both are back.
    world.box_open = true;
    assert_eq!(names(&Palette::for_scene(Scene::Unknown, &mut world)), ["NEXT", "BACK"]);
}

#[test]
fn the_menu_row_is_close_confirm_back() {
    let mut world = World::room();
    let palette = Palette::for_scene(Scene::Menu, &mut world);
    assert_eq!(names(&palette), ["CLOSE", "CONFIRM", "BACK"]);
}

#[test]
fn the_battle_row_is_the_move_buttons_switch_item_and_never_next() {
    let mut world = World::battle();
    world.bag = vec![(item::POTION, 1)];
    let scene = world.scene();
    // Section 13.1: `RUN` is bound only in a wild battle the fly is *losing*, and `World::battle`
    // has an eighteen-of-twenty Pokémon on the bench -- something healthier to send in, so the
    // fight is still worth having and the slot is empty.
    //
    // Section 12.10: **`NEXT` is gone from this row.** It was the backstop for a turn where every
    // other button dropped, and it was an A press on the cursor -- which sits on FIGHT, so it
    // opened the move list, whose `BACK` closed it again: `NEXT` 1264 starts and `BACK` 1241 on
    // rung 9 after v0.4.2. `MOVE 1` is the backstop now, and it ends the turn.
    let palette = Palette::for_scene(scene, &mut world);
    assert_eq!(
        names(&palette),
        ["MOVE 1", "MOVE 2", "MOVE 3", "SWITCH", "ITEM"],
        "three moves with PP, an empty fourth slot, a healthy bench and a potion"
    );

    // Take the bench away and the same frame is one to run from.
    world.mons.truncate(1);
    let scene = world.scene();
    assert_eq!(
        names(&Palette::for_scene(scene, &mut world)),
        ["MOVE 1", "MOVE 2", "MOVE 3", "ITEM", "RUN"]
    );
}

#[test]
fn a_move_button_is_bound_by_its_own_slots_pp_and_move_one_carries_struggle() {
    // Section 14: one button per move slot, asked **over the open move list**, which is where a
    // slot is a thing to aim at. `World::battle`'s Pokémon has three moves and an empty fourth
    // slot, so three buttons are on the list's pad and the fourth never is.
    let mut world = World::battle();
    world.list = List::Moves(3);
    assert!(move_slot_bound(&mut world, MacroKind::Move1));
    assert!(move_slot_bound(&mut world, MacroKind::Move2));
    assert!(move_slot_bound(&mut world, MacroKind::Move3));
    assert!(!move_slot_bound(&mut world, MacroKind::Move4), "an empty slot is not a button");

    // A spent slot leaves the pad while a usable one is there: the fly is not offered a move it
    // cannot use beside one it can.
    world.mons[0] = mon(0, 4, 20, &[(33, 0), (52, 20)]);
    assert!(!move_slot_bound(&mut world, MacroKind::Move1));
    assert!(move_slot_bound(&mut world, MacroKind::Move2));

    // Row 34 of `infra/docs/macros-traps.md`: with **every** slot out of PP the cartridge uses
    // Struggle, and the way to it is to choose a move anyway. `MOVE 1` stays, and it alone.
    world.mons[0] = mon(0, 4, 20, &[(33, 0), (52, 0)]);
    assert!(move_slot_bound(&mut world, MacroKind::Move1), "Struggle is reachable");
    assert!(!move_slot_bound(&mut world, MacroKind::Move2));
    let scene = world.scene();
    assert!(
        Palette::for_scene(scene, &mut world)
            .slot(MacroId(MacroKind::Move1.slot()))
            .is_some()
    );

    // A Pokémon with no move in slot one at all answers no over the list: there is no slot to aim
    // at, and inventing a press for it is what this crate does not do.
    world.mons[0] = mon(0, 4, 20, &[]);
    assert!(!move_slot_bound(&mut world, MacroKind::Move1));
    let scene = world.scene();
    assert_eq!(Palette::for_scene(scene, &mut world).slot(MacroId(MacroKind::Move1.slot())), None);

    // Over the **top-level menu** the question is a different one -- section 12.8's "is there a
    // move list to open" -- and FIGHT always opens, so `MOVE 1` is bound there whatever the seam
    // makes of the battler. That is what carries the turn now that `NEXT` is off this row
    // (section 12.10): the script over this menu is "confirm FIGHT and stop" and reads no move.
    world.list = List::BattleMain;
    assert!(move_slot_bound(&mut world, MacroKind::Move1), "FIGHT is always pressable");
    assert!(!move_slot_bound(&mut world, MacroKind::Move2), "and it is MOVE 1 that carries it");
    world.active = None;
    assert!(
        move_slot_bound(&mut world, MacroKind::Move1),
        "a battler the seam cannot read is not a reason to take the turn's one button away"
    );
}

#[test]
fn a_move_button_confirms_its_own_slot_over_an_open_list() {
    // The list is already up: the script goes straight to that slot and confirms, by reading the
    // cursor rather than by counting presses.
    let mut world = World::battle();
    world.list = List::Moves(3);
    world.grid = false;
    world.cursor = 0;
    world.cursor_max = 2;
    assert_eq!(run(&mut world, MacroKind::Move3).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 2, "the third slot, by watching where the cursor went");
    assert_eq!(world.pulses.last(), Some(&buttons::A));

    // And from the menu above, FIGHT first and then the slot. The list has to actually open:
    // since section 12.11 the step that walks it *waits* for the move list rather than reading
    // whatever cursor is up, so a fixture where FIGHT opens nothing waits out `CURSOR_WAIT` --
    // which is the right answer, and is what the cartridge was doing to `THROW BALL` in reverse.
    let mut world = World::battle();
    world.opens.push_back(Opens { list: List::Moves(3), cursor: 0, max: 2, grid: false });
    assert_eq!(run(&mut world, MacroKind::Move2).unwrap(), MacroAbort::Done);
    assert!(
        world.pulses.contains(&buttons::A),
        "FIGHT was confirmed: {:?}",
        world.pulses
    );
}

#[test]
fn switch_is_unbound_when_the_bench_is_fainted_or_empty() {
    let mut world = World::battle();
    world.mons = vec![mon(0, 4, 20, &[(33, 30)]), mon(1, 0, 20, &[(33, 30)])];
    assert!(!precondition(MacroKind::Switch, &mut world));
    world.mons.truncate(1);
    assert!(!precondition(MacroKind::Switch, &mut world));
}

#[test]
fn item_needs_both_halves_of_its_precondition() {
    let mut world = World::battle();
    assert!(!precondition(MacroKind::Item, &mut world), "hurt but no potion");
    world.bag = vec![(item::POTION, 1)];
    assert!(precondition(MacroKind::Item, &mut world));
    world.mons[0].hp = 19;
    assert!(!precondition(MacroKind::Item, &mut world), "a potion but above half health");
    world.mons[0].hp = 10;
    assert!(!precondition(MacroKind::Item, &mut world), "exactly half is not below half");
}

#[test]
fn run_is_bound_in_a_wild_battle_and_unbound_against_a_trainer() {
    let mut world = World::battle();
    // One Pokémon, on four of twenty: a wild battle with nothing to switch to and nothing left to
    // do but leave.
    world.mons.truncate(1);
    assert!(precondition(MacroKind::Run, &mut world));
    world.battle = Some((BattleKind::Trainer, true, false));
    assert!(!precondition(MacroKind::Run, &mut world), "the cartridge refuses to flee a trainer");
    let scene = world.scene();
    assert_eq!(Palette::for_scene(scene, &mut world).slots[3], None);
}

/// Section 13.1, the operator: "we run away a lot". `RUN` used to be bound for every wild battle, so a
/// fight the fly was winning was one it could flee -- and a fled battle pays nothing and teaches
/// nothing. The precondition is now the two states in which there is nothing else to do.
#[test]
fn run_is_off_the_pad_in_a_wild_battle_the_fly_is_not_losing() {
    let mut world = World::battle();

    // Full health, moves with PP: not losing, whatever else is true.
    world.mons = vec![mon(0, 20, 20, &[(33, 30)])];
    assert!(!losing(&mut world), "a healthy Pokémon with PP is in a fight worth having");
    assert!(!precondition(MacroKind::Run, &mut world));

    // Under a third, but something healthier can come in: `SWITCH` is the answer, not `RUN`, and
    // the two read the same reserve so they cannot both be wrong at once.
    world.mons = vec![mon(0, 6, 20, &[(33, 30)]), mon(1, 18, 20, &[(33, 30)])];
    assert!(healthiest_other(&mut world).is_some());
    assert!(!losing(&mut world));

    // Under a third with nothing healthier: losing.
    world.mons = vec![mon(0, 6, 20, &[(33, 30)]), mon(1, 1, 20, &[(33, 30)])];
    assert!(losing(&mut world));

    // Exactly a third is not under it.
    world.mons = vec![mon(0, 7, 21, &[(33, 30)])];
    assert!(!losing(&mut world), "hp * 3 == max_hp is not under a third");

    // Full health, every move out of PP, nothing to switch to: Struggle and recoil, so losing.
    world.mons = vec![mon(0, 20, 20, &[(33, 0), (52, 0)])];
    assert!(losing(&mut world));
    // ... but with a reserve, `SWITCH` is the move.
    world.mons = vec![mon(0, 20, 20, &[(33, 0)]), mon(1, 20, 20, &[(33, 30)])];
    assert!(!losing(&mut world));

    // And outside a battle it is never true, so nothing can bind `RUN` in the overworld.
    let mut room = World::room();
    assert!(!losing(&mut room));
}

#[test]
fn a_forced_switch_binds_switch_five_times_and_nothing_on_b() {
    let mut world = World::battle();
    world.scene = Scene::Battle { own_turn: true, forced_switch: true };
    world.battle = Some((BattleKind::Wild, true, true));
    let scene = world.scene();
    let palette = Palette::for_scene(scene, &mut world);
    assert_eq!(names(&palette), ["NEXT", "SWITCH"]);
}

#[test]
fn a_battle_frame_that_is_not_the_players_turn_binds_next_to_advance_its_text() {
    // 2026-09-16 hotfix: battle text waits for a press like a dialog (live deadlock on Route 1).
    let mut world = World::battle();
    world.scene = Scene::Battle { own_turn: false, forced_switch: false };
    world.battle = Some((BattleKind::Wild, false, false));
    world.list = List::None;
    let scene = world.scene();
    // Section 12.9: `NEXT` alone. Section 13.1 put `BACK` here for the bag a battle's ITEM entry
    // opens, which read as nobody's turn -- but on a frame of text there is no list to leave, and
    // a `BACK` that changes nothing is the trap of section 12.2 (live, rung 9: 135 of 183 macro
    // starts).
    let palette = Palette::for_scene(scene, &mut world);
    assert_eq!(names(&palette), ["NEXT"]);

    // Section 12.10: the bag does not land on this arm any more. It is a cursor accepting input,
    // so it is the fly's turn, and its `NEXT` would have been the A that *uses* what the cursor
    // holds rather than the A that advances text. Nothing is open here, so nothing but `NEXT` is.
    world.list = List::BattleBag;
    world.battle = Some((BattleKind::Wild, true, false));
    world.scene = Scene::Battle { own_turn: true, forced_switch: false };
    world.bag = vec![(item::POTION, 1)];
    let palette = Palette::for_scene(world.scene(), &mut world);
    assert_eq!(names(&palette), ["BACK", "ITEM"], "a hurt Pokémon, a potion, and no ball");
}

#[test]
fn the_shop_row_binds_a_purchase_only_when_money_allows() {
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::BuySellQuit);
    world.cursor_max = 2;
    world.stock = vec![item::POKE_BALL, item::POTION];
    world.money = price::POKE_BALL;
    let palette = Palette::for_scene(Scene::Shop, &mut world);
    assert_eq!(names(&palette), ["CONFIRM", "BUY BALL", "LEAVE"]);
    world.money = price::POTION;
    assert!(names(&Palette::for_scene(Scene::Shop, &mut world)).contains(&"BUY POTION"));
    world.stock = vec![item::POKE_BALL];
    assert!(
        !precondition(MacroKind::BuyPotion, &mut world),
        "a mart that does not stock potions"
    );
}

/// Section 13's four purchases, each bound by the counter's own stock and by the money on hand.
///
/// Viridian's real inventory is the case worth pinning: `data/items/marts.asm` gives it
/// POKE BALL, ANTIDOTE, PARLYZ HEAL and BURN HEAL, and **no Potion at all** -- so `BUY POTION` is
/// correctly off the pad in the first mart the fly ever walks into, and `BUY ANTIDOTE` is the
/// purchase that is on it.
#[test]
fn the_four_purchases_are_bound_by_the_counters_own_stock() {
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::Buying);
    world.cursor_max = 3;
    // Viridian's counter, in menu order.
    world.stock = vec![item::POKE_BALL, item::ANTIDOTE, 15, 12];
    world.money = 3_000;
    assert_eq!(
        names(&Palette::for_scene(Scene::Shop, &mut world)),
        ["CONFIRM", "BUY BALL", "BUY ANTIDOTE", "LEAVE"],
        "Viridian stocks no Potion and no Repel"
    );

    // Cerulean's, which stocks both of the other two -- and whose fourth entry is out of the
    // cursor's reach, exactly as Pewter's Antidote is (row 55): the list scrolls, so only the
    // first three of a counter's stock have an index this seam can aim at.
    world.stock = vec![item::POKE_BALL, item::POTION, item::REPEL, item::ANTIDOTE];
    assert_eq!(
        names(&Palette::for_scene(Scene::Shop, &mut world)),
        ["CONFIRM", "BUY POTION", "BUY BALL", "BUY REPEL", "LEAVE"]
    );

    // Money is the other half, per item: 150 buys an Antidote and nothing else, at a counter
    // whose Antidote the cursor can reach.
    world.stock = vec![item::POKE_BALL, item::ANTIDOTE, 15, 12];
    world.money = 150;
    assert_eq!(
        names(&Palette::for_scene(Scene::Shop, &mut world)),
        ["CONFIRM", "BUY ANTIDOTE", "LEAVE"]
    );
    // And the cheapest purchase is what the mart errand's money test measures against.
    assert_eq!(CHEAPEST_PURCHASE, price::ANTIDOTE);
}

#[test]
fn a_shop_whose_stock_cannot_be_read_binds_no_purchase() {
    // The seam's default: no stock list, so the slot stays unbound rather than guessing an index.
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.money = 9999;
    let palette = Palette::for_scene(Scene::Shop, &mut world);
    assert_eq!(names(&palette), ["CONFIRM", "LEAVE"]);
}

#[test]
fn the_pc_row_is_confirm_and_leave() {
    let mut world = World::room();
    let palette = Palette::for_scene(Scene::Pc, &mut world);
    assert_eq!(names(&palette), ["CONFIRM", "LEAVE"]);
}

#[test]
fn every_macro_name_fits_the_screen() {
    for kind in MacroKind::ALL {
        assert!(kind.name().len() <= 14, "{} is {} characters", kind.name(), kind.name().len());
    }
}

#[test]
fn the_healthiest_other_skips_the_active_slot_and_the_fainted() {
    let mut world = World::battle();
    assert_eq!(healthiest_other(&mut world), Some(1));
    world.mons.push(mon(3, 20, 20, &[(33, 30)]));
    assert_eq!(healthiest_other(&mut world), Some(3), "by fraction, not by index");
    world.mons[0] = mon(0, 99, 99, &[(33, 30)]);
    assert_eq!(healthiest_other(&mut world), Some(3), "the active one is never a candidate");
}

#[test]
fn the_listing_follows_whichever_menu_is_up() {
    let mut world = World::battle();
    assert_eq!(listing(&mut world).map(|list| (list.current, list.max)), Some((0, 3)));
    world.list = List::Moves(3);
    world.cursor = 2;
    assert_eq!(listing(&mut world).map(|list| (list.current, list.max)), Some((2, 2)));
    world.list = List::BattleParty;
    world.cursor = 1;
    assert_eq!(listing(&mut world).map(|list| (list.current, list.max)), Some((1, 2)));
    // The bag inside a battle was the seam's gap until section 14: `ITEM` could open it and then
    // had nothing to read, so it waited out `CURSOR_WAIT` and reported `Blocked`. It reports a
    // cursor now, which is what `ITEM` and `THROW BALL` navigate by.
    world.list = List::BattleBag;
    world.cursor_max = 1;
    world.cursor = 0;
    assert_eq!(listing(&mut world).map(|list| (list.current, list.max)), Some((0, 1)));
}

// ---------------------------------------------------------------------------------------------
// Section 4: A* and the exits
// ---------------------------------------------------------------------------------------------

#[test]
fn a_star_walks_around_a_wall() {
    let mut world = World::room().at(0, 0).wall(&[(1, 0), (1, 1), (1, 2)]);
    let route = path::route(&mut world, &[Tile::new(2, 0)]).expect("a route around the wall");
    assert_eq!(route.goal, Some(0));
    assert_eq!(route.steps.len(), 8, "down past the wall, across, and back up: {:?}", route.steps);
}

#[test]
fn a_star_picks_the_nearest_of_several_goals() {
    let mut world = World::room().at(3, 3);
    let route = path::route(&mut world, &[Tile::new(7, 7), Tile::new(3, 5), Tile::new(0, 0)])
        .expect("a route");
    assert_eq!(route.goal, Some(1));
    assert_eq!(route.steps, vec![Facing::Down, Facing::Down]);
}

#[test]
fn a_star_gives_up_when_nothing_gets_any_closer() {
    let mut world = World::room().at(0, 0).wall(&[(0, 1), (1, 1), (1, 0)]);
    assert_eq!(path::route(&mut world, &[Tile::new(4, 4)]), None);
}

#[test]
fn a_goal_outside_the_predicates_window_is_walked_toward_rather_than_refused() {
    // Agent A's `Walkable::Unknown`: the predicate can only answer near the player, so the door is
    // off the screen buffer. The search walks toward it anyway at [`UNKNOWN_STEP`] a tile, because
    // a map edge is off the window by definition and a search that refused every unknown tile
    // could not plan one step toward Route 1 from the middle of Pallet Town. The window travels
    // along and the walk re-plans every tile.
    let mut world = World::room().at(0, 0);
    for y in 0..8u8 {
        for x in 0..8u8 {
            if u32::from(x) + u32::from(y) > 4 {
                world.unseen.insert(Tile::new(x, y));
            }
        }
    }
    let route = path::route(&mut world, &[Tile::new(7, 7)]).expect("a route through the unknown");
    assert!(!route.steps.is_empty(), "it moved toward the door");
    assert!(
        matches!(route.steps[0], Facing::Right | Facing::Down),
        "and toward it rather than away: {:?}",
        route.steps[0]
    );

    // A wall is still a wall: unknown is a price, not a claim, and nothing paths through `No`.
    let mut walled = World::room().at(0, 0).wall(&[(0, 1), (1, 1), (1, 0)]);
    assert_eq!(path::route(&mut walled, &[Tile::new(4, 4)]), None);

    // And a known way round always beats the unknown one: the detour is four tiles, the straight
    // line through the unseen middle is two.
    let mut round = World::room().at(0, 0);
    for y in 1..3u8 {
        round.unseen.insert(Tile::new(1, y));
    }
    let route = path::route(&mut round, &[Tile::new(1, 3)]).expect("a route");
    assert_eq!(route.goal, Some(0));
    assert_eq!(
        route.steps.len(),
        4,
        "it went the four known tiles round rather than the two unseen ones through: {:?}",
        route.steps
    );
    assert!(!route.steps.contains(&Facing::Left));
}

#[test]
fn standing_on_the_goal_is_a_route_with_no_steps() {
    let mut world = World::room().at(3, 3);
    let route = path::route(&mut world, &[Tile::new(3, 3)]).expect("a zero step route");
    assert_eq!(route, path::Route { goal: Some(0), steps: Vec::new() });
}

#[test]
fn the_player_tile_is_passable_even_when_the_predicate_says_otherwise() {
    // A doormat the game will not let the player stop on, with the player standing on it.
    let mut world = World::room().at(3, 7).wall(&[(3, 7)]);
    assert!(path::route(&mut world, &[Tile::new(3, 3)]).is_some());
}

#[test]
fn exits_lists_the_warps_and_every_connected_edge_tile() {
    let mut world = World::ground_floor();
    assert_eq!(path::exits(&mut world).len(), 3, "three warps and no connections");
    world.connections.south = true;
    assert_eq!(path::exits(&mut world).len(), 3 + 8, "the whole bottom row is an edge exit too");
}

#[test]
fn a_doormat_on_the_bottom_row_carries_the_outward_press() {
    let mut world = World::ground_floor();
    let exits = path::exits(&mut world);
    let mat = exits
        .iter()
        .find(|exit| exit.tile == Tile::new(2, 7))
        .expect("the left doormat is an exit");
    assert_eq!(mat.press, Some(Facing::Down), "the front door needs DOWN, and only DOWN");
    let stairs = exits
        .iter()
        .find(|exit| exit.tile == Tile::new(7, 1))
        .expect("the staircase is an exit");
    assert_eq!(stairs.press, None, "an interior warp fires on the step onto it");
}

#[test]
fn a_way_out_is_visited_only_when_the_map_on_the_other_side_is() {
    // Section 9.2, after the town loop: standing on a doormat, or beside it, is what the
    // adapter's `boundary` ledger pays for, and it is no longer what "visited" means. Both
    // doormats of the ground floor lead to Pallet Town; the staircase leads upstairs.
    let mut world = World::ground_floor();
    world.visited.insert(ExitId::Warp(1));
    let tiles: Vec<Tile> =
        ways(&mut world, Way::Exit).iter().map(|exit: &Exit| exit.tile).collect();
    assert_eq!(
        tiles,
        vec![Tile::new(2, 7), Tile::new(3, 7)],
        "the boundary ledger does not decide this any more: {tiles:?}"
    );
    // The map on the other side, which is the question that does decide it.
    world.seen_maps.insert(0x00);
    assert_eq!(
        ways(&mut world, Way::Exit).len(),
        2,
        "a room whose outside is known still has to be left, and nearest picks"
    );
    // And upstairs is a different map, asked separately.
    assert_eq!(ways(&mut world, Way::Passage).len(), 1);
    world.seen_maps.insert(0x26);
    assert!(
        ways(&mut world, Way::Passage).is_empty(),
        "a staircase already been up has no fallback on a floor with its own front door: up and \
         straight back down was the same bounce as the door's (2026-09-17)"
    );
}

#[test]
fn a_connection_to_an_unvisited_map_outranks_a_door_into_an_unvisited_interior() {
    // Section 9.2: "rank connections to unvisited maps above doors to unvisited interiors". Pallet
    // Town with a door into a house nobody has been in and the connection north to Route 1, which
    // is the choice the fly got wrong for an hour on the stream.
    let mut world = World::room();
    world.map = maps::PALLET_TOWN;
    world.warps = vec![Warp { x: 2, y: 4, destination_warp: 0, destination_map: maps::BLUES_HOUSE }];
    world.connections.north = true;
    let routes = ways(&mut world, Way::Route);
    assert!(
        routes.iter().all(|exit: &Exit| matches!(exit.id, ExitId::Edge(Edge::North))),
        "the connection wins outright: {routes:?}"
    );
    assert_eq!(routes[0].into, Some(maps::ROUTE_1), "and it knows where it goes");

    // Once Route 1 has been walked, the door is what is left.
    world.seen_maps.insert(maps::ROUTE_1);
    let routes = ways(&mut world, Way::Route);
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].tile, Tile::new(2, 4));

    // And with both known, the exhausted fallback is the exit toward the objective.
    world.seen_maps.insert(maps::BLUES_HOUSE);
    world.objective = Some(Objective { map: maps::VIRIDIAN_CITY, tile: None, warp: None, edge: None, target: None });
    let routes = ways(&mut world, Way::Route);
    assert!(
        routes.iter().all(|exit: &Exit| matches!(exit.id, ExitId::Edge(Edge::North))),
        "north is the way to Viridian: {routes:?}"
    );
}

#[test]
fn go_route_prefers_a_door_whose_interior_this_run_has_not_seen() {
    // Outdoors, with two doors: one into a house this run has been in and one into a house it has
    // not (section 9.1, "a door into a building whose interior is unvisited").
    let mut world = World::room();
    world.map = 0x00;
    world.warps = vec![
        Warp { x: 2, y: 0, destination_warp: 0, destination_map: 0x25 },
        Warp { x: 5, y: 0, destination_warp: 0, destination_map: 0x28 },
    ];
    world.seen_maps.insert(0x25);
    let tiles: Vec<Tile> =
        ways(&mut world, Way::Route).iter().map(|exit: &Exit| exit.tile).collect();
    assert_eq!(tiles, vec![Tile::new(5, 0)], "the unvisited interior wins: {tiles:?}");
    // And with both interiors seen, `GO ROUTE` has nothing: no fresh destination, nothing on the
    // way to an objective, and no last-resort fallback to the nearest door (2026-09-17). That
    // fallback is what let the fly walk into the same house once per hold for two hours.
    world.seen_maps.insert(0x28);
    assert!(
        ways(&mut world, Way::Route).is_empty(),
        "every route leads somewhere this run has been, so the button leaves the pad"
    );
    assert!(!on_the_pad(&mut world, MacroKind::GoRoute));
    // A way *out of a building* keeps its fallback: a room still has to be leavable.
    let mut indoors = World::ground_floor();
    indoors.seen_maps.extend([0x00, 0x26, 0xff]);
    assert_eq!(ways(&mut indoors, Way::Exit).len(), 2, "both doormats, visited or not");
    // The staircase does not, on a floor that has its own front door: up and straight back down is
    // the same bounce measured on the stairs instead of the door (2026-09-17).
    assert!(ways(&mut indoors, Way::Passage).is_empty(), "the upper floor has been seen");
    assert!(!on_the_pad(&mut indoors, MacroKind::GoWarp));

    // But a map whose only way anywhere is a passage keeps it, or the fly is stranded upstairs.
    let mut bedroom = World::room();
    bedroom.map = 0x26;
    bedroom.warps = vec![Warp { x: 7, y: 1, destination_warp: 2, destination_map: 0x25 }];
    bedroom.seen_maps.insert(0x25);
    assert!(ways(&mut bedroom, Way::Exit).is_empty(), "no front door up here");
    assert_eq!(ways(&mut bedroom, Way::Passage).len(), 1, "so the staircase is still offered");
    assert!(on_the_pad(&mut bedroom, MacroKind::GoWarp));
}

#[test]
fn an_edge_exit_is_named_by_its_edge() {
    let mut world = World::room();
    world.connections.north = true;
    let exits = path::exits(&mut world);
    assert!(exits.iter().all(|exit| exit.id == ExitId::Edge(Edge::North)));
    assert!(exits.iter().all(|exit| exit.press == Some(Facing::Up)));
}

// ---------------------------------------------------------------------------------------------
// Section 4: refusals
// ---------------------------------------------------------------------------------------------

#[test]
fn an_unbound_slot_is_refused_and_nothing_is_pressed() {
    let mut world = World::room();
    let palette = Palette::for_scene(Scene::Overworld, &mut world);
    let mut machine = MacroMachine::new(1);
    let refused = machine.start(&palette, MacroId(0), &mut world).expect_err("no exit here");
    assert_eq!(refused, MacroRefused { slot: MacroId(0), reason: Refusal::Unbound });
    assert!(machine.step(&mut world).is_none(), "a refusal presses nothing");
    assert!(world.pulses.is_empty());
    assert_eq!(machine.outcome().map(|(_, outcome)| outcome), Some(MacroAbort::Refused));
}

#[test]
fn a_slot_past_the_end_of_the_palette_is_unbound() {
    let mut world = World::room();
    let palette = Palette::for_scene(Scene::Overworld, &mut world);
    let mut machine = MacroMachine::new(1);
    let refused = machine.start(&palette, MacroId(9), &mut world).expect_err("there are six");
    assert_eq!(refused.reason, Refusal::Unbound);
}

#[test]
fn a_second_start_while_a_macro_owns_the_buttons_is_refused() {
    let mut world = World::room();
    let (palette, slot) = pick(&mut world, MacroKind::GoFrontier);
    let mut machine = MacroMachine::new(1);
    machine.start(&palette, slot, &mut world).expect("GO FRONTIER starts");
    assert_eq!(machine.running(), Some("GO FRONTIER"));
    assert_eq!(
        machine.start(&palette, slot, &mut world).expect_err("busy").reason,
        Refusal::Busy
    );
}

#[test]
fn a_palette_for_another_scene_is_refused() {
    let mut world = World::room();
    let menu = Palette::for_scene(Scene::Menu, &mut world);
    let mut machine = MacroMachine::new(1);
    assert_eq!(
        machine
            .start(&menu, MacroId(MacroKind::Close.slot()), &mut world)
            .expect_err("wrong scene")
            .reason,
        Refusal::WrongScene
    );
    assert!(world.pulses.is_empty());
}

#[test]
fn a_precondition_that_lapsed_since_the_palette_was_dealt_refuses() {
    let mut world = World::battle();
    world.mons.truncate(1);
    let scene = world.scene();
    let palette = Palette::for_scene(scene, &mut world);
    let run_slot = MacroId(MacroKind::Run.slot());
    assert!(palette.slot(run_slot).is_some(), "RUN was bound when the palette was dealt");
    world.battle = Some((BattleKind::Trainer, true, false));
    let mut machine = MacroMachine::new(1);
    assert_eq!(
        machine.start(&palette, run_slot, &mut world).expect_err("not wild now").reason,
        Refusal::Precondition
    );
    assert!(world.pulses.is_empty());
}

#[test]
fn go_out_refuses_when_there_is_nothing_to_walk_toward() {
    let mut world = World::ground_floor().at(0, 0).wall(&[(0, 1), (1, 1), (1, 0)]);
    assert_eq!(
        run(&mut world, MacroKind::GoOut).expect_err("walled in").reason,
        Refusal::NoRoute
    );
}

// ---------------------------------------------------------------------------------------------
// Section 4: the scripts
// ---------------------------------------------------------------------------------------------

#[test]
fn talk_presses_a_exactly_once_and_records_what_it_faced() {
    // Section 12: the press is on the pad only while the tile ahead holds something untalked, so
    // the world has to have something in front of the fly for `TALK` to be startable at all.
    let mut world = World::room().at(3, 3);
    world.facing = Facing::Down;
    world.npcs = vec![Npc { slot: 4, picture: 1, x: 3, y: 4, facing: Facing::Up }];
    let mut machine = MacroMachine::new(1);
    let (palette, slot) = pick(&mut world, MacroKind::Talk);
    machine.start(&palette, slot, &mut world).expect("TALK is bound at a villager");
    for _ in 0..(FRAME_CAP + 64) {
        match machine.step(&mut world) {
            Some(mask) => world.frame(mask),
            None => break,
        }
    }
    assert_eq!(machine.outcome().expect("a finished macro").1, MacroAbort::Done);
    assert_eq!(world.pulses, vec![buttons::A]);
    // The press is a conversation *begun*: nothing is in the ledger until the box has closed with
    // the fly where it pressed from (`docs/design/macros.md` section 12.4), which is the frame the
    // driver hands the machine next.
    assert_eq!(machine.take_talked(), None, "the conversation has not ended yet");
    machine.observe_frame(&mut world);
    // And then the ledger entry is the villager's sprite slot on this map, taken once.
    assert_eq!(machine.take_talked(), Some((world.map, TalkTarget::Sprite(4))));
    assert_eq!(machine.take_talked(), None, "taken rather than read");

    // A press that never finishes records nothing.
    let mut cancelled = World::room().at(3, 3);
    cancelled.facing = Facing::Down;
    cancelled.npcs = vec![Npc { slot: 4, picture: 1, x: 3, y: 4, facing: Facing::Up }];
    let (palette, slot) = pick(&mut cancelled, MacroKind::Talk);
    let mut machine = MacroMachine::new(1);
    machine.start(&palette, slot, &mut cancelled).unwrap();
    machine.cancel();
    assert_eq!(machine.take_talked(), None);
}

#[test]
fn go_item_walks_next_to_the_nearest_object_and_turns_to_face_it() {
    // A ball on a table in the middle of the room and a sign on the far wall: the ball is nearer,
    // and the tile to stand on is beside it rather than on it.
    let mut world = World::room().at(1, 1).wall(&[(1, 4), (7, 7)]);
    world.npcs = vec![
        Npc { slot: 1, picture: 3, x: 2, y: 1, facing: Facing::Down },
        Npc { slot: 2, picture: BALL_SPRITE, x: 1, y: 4, facing: Facing::Down },
    ];
    world.signs = vec![Sign { x: 7, y: 7, text_id: 1 }];
    assert_eq!(run(&mut world, MacroKind::GoItem).unwrap(), MacroAbort::Done);
    assert_eq!(world.player, Tile::new(1, 3), "one tile above the ball");
    assert_eq!(world.facing, Facing::Down, "facing it");
    assert!(
        world.pulses.iter().all(|mask| *mask == buttons::DOWN),
        "straight down and then a turn that was already down: {:?}",
        world.pulses
    );
}

#[test]
fn go_item_reaches_a_sign_in_a_wall_from_the_one_side_it_has() {
    // A signpost is part of a wall: three of the four tiles around it are wall too, and only the
    // tile below it can be stood on. The route search finds that out; nothing tells it.
    let mut world = World::room().at(3, 3).wall(&[(2, 0), (1, 0), (3, 0)]);
    world.signs = vec![Sign { x: 2, y: 0, text_id: 1 }];
    assert_eq!(run(&mut world, MacroKind::GoItem).unwrap(), MacroAbort::Done);
    assert_eq!(world.player, Tile::new(2, 1), "below the sign, which is the only way to it");
    assert_eq!(world.facing, Facing::Up, "looking at it");
}

#[test]
fn go_item_refuses_when_the_object_cannot_be_reached() {
    // The fly walled into a corner with the ball across the room: the precondition holds — there
    // *is* an object — and the route search is what refuses, before a button is pressed. A ball
    // merely walled *in* is different and is not a refusal: the search returns the route that
    // gets closest to it, the window travels along, and the walk runs out of failed steps.
    let mut world = World::room().at(0, 0).wall(&[(0, 1), (1, 0), (1, 1)]);
    world.npcs = vec![Npc { slot: 1, picture: BALL_SPRITE, x: 5, y: 5, facing: Facing::Down }];
    assert!(precondition(MacroKind::GoItem, &mut world));
    assert_eq!(
        run(&mut world, MacroKind::GoItem).expect_err("no way in").reason,
        Refusal::NoRoute
    );
    assert!(world.pulses.is_empty(), "nothing was pressed: {:?}", world.pulses);
}

#[test]
fn next_presses_a_and_no_presses_b() {
    let mut world = World::room();
    world.scene = Scene::Dialog;
    world.b_to_close = u8::MAX;
    assert_eq!(run(&mut world, MacroKind::Next).unwrap(), MacroAbort::Done);
    assert_eq!(world.pulses, vec![buttons::A]);

    let mut world = World::room();
    world.scene = Scene::Dialog;
    world.b_to_close = u8::MAX;
    assert_eq!(run(&mut world, MacroKind::No).unwrap(), MacroAbort::Done);
    assert_eq!(world.pulses, vec![buttons::B]);
}

#[test]
fn yes_presses_a() {
    let mut world = World::room();
    world.scene = Scene::Dialog;
    assert_eq!(run(&mut world, MacroKind::Yes).unwrap(), MacroAbort::Done);
    assert_eq!(world.pulses, vec![buttons::A]);
}

#[test]
fn go_frontier_walks_to_new_ground_and_never_rolls_a_die() {
    // Section 9: `GO FRONTIER` replaces `WANDER`, and "no random steps remain anywhere in the
    // palette". The room's right-hand column is the only ground this run has not stood on, so the
    // walk is rightward and nothing about it depends on the seed.
    let mut world = World::room().at(0, 0);
    for y in 0..8 {
        for x in 0..7 {
            world.stood.insert(Tile::new(x, y));
        }
    }
    assert_eq!(run(&mut world, MacroKind::GoFrontier).unwrap(), MacroAbort::Done);
    assert!(
        world.pulses.iter().all(|mask| *mask == buttons::RIGHT),
        "straight at the frontier: {:?}",
        world.pulses
    );
    assert_eq!(world.player.x, 7, "and it stood on the new ground: {:?}", world.player);
    assert_eq!(world.facing, Facing::Right, "having faced it to get there");

    // Two runs with different seeds press exactly the same buttons, which is the "no random
    // steps" claim as a test rather than as a sentence.
    let pressed = |seed: u32| {
        let mut world = World::room().at(0, 0);
        for y in 0..8 {
            for x in 0..7 {
                world.stood.insert(Tile::new(x, y));
            }
        }
        let (palette, slot) = pick(&mut world, MacroKind::GoFrontier);
        let mut machine = MacroMachine::new(seed);
        machine.start(&palette, slot, &mut world).expect("GO FRONTIER starts");
        while let Some(mask) = machine.step(&mut world) {
            world.frame(mask);
        }
        world.pulses.clone()
    };
    assert_eq!(pressed(1), pressed(0xdead_beef));
}

#[test]
fn go_frontier_is_unbound_once_every_reachable_tile_has_been_stood_on() {
    let mut world = World::room();
    for y in 0..8 {
        for x in 0..8 {
            world.stood.insert(Tile::new(x, y));
        }
    }
    assert!(!precondition(MacroKind::GoFrontier, &mut world));
    let palette = Palette::for_scene(Scene::Overworld, &mut world);
    assert_eq!(palette.slots[3], None, "nothing is left to explore, so the slot is no action");
}

#[test]
fn menu_holds_start_until_the_start_menu_opens() {
    let mut world = World::room();
    world.start_to_open = 2;
    // Off every pad since section 12.11, so the palette is built by hand: the script is what is
    // under test here and no scene offers the button any more.
    let mut machine = MacroMachine::new(0x1234_5678);
    let (palette, slot) = forced(MacroKind::Menu, &mut world);
    assert_eq!(drive(&mut machine, &palette, slot, &mut world).unwrap(), MacroAbort::Done);
    assert_eq!(world.pulses, vec![buttons::START, buttons::START]);
    assert_eq!(world.scene, Scene::Menu);
}

#[test]
fn menu_reports_blocked_when_the_start_menu_never_opens() {
    let mut world = World::room();
    world.start_to_open = u8::MAX;
    let mut machine = MacroMachine::new(0x1234_5678);
    let (palette, slot) = forced(MacroKind::Menu, &mut world);
    assert_eq!(drive(&mut machine, &palette, slot, &mut world).unwrap(), MacroAbort::Blocked);
}

#[test]
fn close_backs_out_of_a_submenu_and_stops_at_the_overworld() {
    let mut world = World::room();
    world.scene = Scene::Menu;
    world.list = List::Start;
    world.cursor_max = 5;
    world.b_to_close = 3;
    assert_eq!(run(&mut world, MacroKind::Close).unwrap(), MacroAbort::Done);
    assert_eq!(world.pulses, vec![buttons::B; 3]);
    assert_eq!(world.scene, Scene::Overworld);
}

#[test]
fn close_reports_blocked_when_the_menu_never_closes() {
    let mut world = World::room();
    world.scene = Scene::Menu;
    world.list = List::Start;
    world.b_to_close = u8::MAX;
    assert_eq!(run(&mut world, MacroKind::Close).unwrap(), MacroAbort::Blocked);
}

#[test]
fn go_out_walks_to_the_front_door_and_steps_through_it() {
    let mut world = World::ground_floor().at(3, 4);
    assert_eq!(run(&mut world, MacroKind::GoOut).unwrap(), MacroAbort::Done);
    assert_eq!(world.map, 0x00, "the fly left the house");
    assert!(
        world.pulses.iter().all(|mask| *mask == buttons::DOWN),
        "three tiles straight down: {:?}",
        world.pulses
    );
}

#[test]
fn go_out_takes_a_sideways_doormat_and_then_presses_down() {
    // Standing next to the left mat: `GO OUT` is pointed at the doormats and not at the
    // staircase, so the route is one step left onto the mat, which does nothing on its own, and
    // then the outward press.
    let mut world = World::ground_floor().at(4, 7);
    assert_eq!(run(&mut world, MacroKind::GoOut).unwrap(), MacroAbort::Done);
    assert_eq!(world.map, 0x00);
    // The outward press was still held when the door opened, so it never released into `pulses`.
    assert_eq!(world.pulses, vec![buttons::LEFT]);
    assert_eq!(world.previous, buttons::DOWN, "and DOWN is what opened it");
}

#[test]
fn go_warp_reaches_an_interior_warp_without_an_extra_press() {
    let mut world = World::ground_floor().at(7, 3);
    assert_eq!(run(&mut world, MacroKind::GoWarp).unwrap(), MacroAbort::Done);
    assert_eq!(world.map, 0x26, "up the stairs");
    assert_eq!(world.pulses, vec![buttons::UP], "the second press fired the staircase");
    assert_eq!(world.previous, buttons::UP);
}

#[test]
fn go_out_leaves_by_a_connected_map_edge() {
    let mut world = World::room().at(3, 5);
    world.connections.south = true;
    assert_eq!(run(&mut world, MacroKind::GoOut).unwrap(), MacroAbort::Done);
    assert_eq!(world.map, 0xfe);
}

#[test]
fn three_steps_that_do_not_move_the_player_abort_as_blocked() {
    let mut world = World::ground_floor().at(3, 4);
    world.glue = true;
    assert_eq!(run(&mut world, MacroKind::GoOut).unwrap(), MacroAbort::Blocked);
    assert_eq!(world.player, Tile::new(3, 4), "and it never moved");
}

#[test]
fn a_walk_longer_than_the_old_cap_finishes_because_it_keeps_making_progress() {
    // The Viridian stall's own arithmetic, from the other side (2026-09-17,
    // `infra/docs/macros-traps.md`): a corridor forty tiles long, a tile costing most of a second,
    // and a door at the far end. Under section 4's flat 600-frame cap this walk could not finish —
    // and the fly, which chooses the macro and not the route, had no way to ask for more frames.
    // The budget is the plan's now ([`walk_budget`]) and a walk still closing on its goal is not
    // out of frames at all, so the corridor is walked and the door is reached.
    let mut world = World::room().at(0, 0);
    world.size = MapSize { width: 1, height: 40 };
    world.warps = vec![Warp { x: 0, y: 39, destination_warp: 1, destination_map: 0x00 }];
    world.tile_frames = 20;
    assert_eq!(run(&mut world, MacroKind::GoOut).unwrap(), MacroAbort::Done);
    assert_eq!(world.map, 0x00, "it arrived: {:?}", world.player);
    assert!(
        world.frames > FRAME_CAP,
        "and it took more than the old cap to do it: {} frames",
        world.frames
    );
}

#[test]
fn the_walk_budget_is_the_plan_plus_a_floor_and_stops_at_a_minute() {
    // `docs/design/macros.md` section 12.1, amended 2026-09-17: "24 frames per planned tile plus a
    // floor, capped at 60 s brain time". The floor is section 4's own ten seconds, so a one-tile
    // walk is budgeted exactly as every other script is and nothing short got shorter.
    assert_eq!(walk_budget(0), FRAME_CAP);
    assert_eq!(walk_budget(1), FRAME_CAP + FRAMES_PER_PLANNED_TILE);
    // Viridian City is about twenty tiles by eighteen: the walk across it now has the frames.
    assert!(walk_budget(38) > 1_400, "a walk across Viridian: {}", walk_budget(38));
    // And a plan no walk should own the pad for is a minute, whatever its length.
    assert_eq!(walk_budget(10_000), WALK_FRAME_CEILING);
    assert_eq!(walk_budget(usize::MAX), WALK_FRAME_CEILING);
    // 59.7275 frames a second, which is the emulator's rate everywhere in this workspace.
    assert_eq!(WALK_FRAME_CEILING, (60.0 * 59.7275) as u32);
}

#[test]
fn a_walk_the_ceiling_cuts_short_resumes_from_where_it_stopped() {
    // The other half of the amendment: "a walk interrupted by the cap resumes from where it
    // stopped on the next start". A corridor far longer than a minute of walking, so the ceiling
    // is what ends the first walk; the second one carries the same route on instead of planning the
    // same first tiles again.
    let mut world = World::room().at(0, 0);
    world.size = MapSize { width: 1, height: 200 };
    world.warps = vec![Warp { x: 0, y: 199, destination_warp: 1, destination_map: 0x00 }];
    let mut machine = MacroMachine::new(0x1234_5678);

    assert_eq!(run_with(&mut machine, &mut world, MacroKind::GoOut).unwrap(), MacroAbort::Timeout);
    let first = world.player.y;
    assert!(first > 0 && first < 199, "it walked part of the way: {:?}", world.player);
    assert_eq!(world.map, 0x25, "and it did not arrive");
    // The cap is not a fact about the target: the exit keeps its place on the pad.
    assert!(on_the_pad(&mut world, MacroKind::GoOut));

    // The next start carries the same route on and finishes the journey.
    assert_eq!(run_with(&mut machine, &mut world, MacroKind::GoOut).unwrap(), MacroAbort::Done);
    assert_eq!(world.map, 0x00, "the resumed walk arrived: {:?}", world.player);

    // A route is a list of directions *from one tile*, so a resume is only honest from the tile the
    // walk was suspended at. Between two holds the fly is free to take a warp, lose a battle or be
    // rolled back, and following a stale route then would walk it along somebody else's path: the
    // same corridor, suspended and then restarted from the top, plans afresh and gets no further
    // than the first walk did.
    let mut moved = World::room().at(0, 0);
    moved.size = MapSize { width: 1, height: 200 };
    moved.warps = vec![Warp { x: 0, y: 199, destination_warp: 1, destination_map: 0x00 }];
    let mut machine = MacroMachine::new(0x1234_5678);
    assert_eq!(run_with(&mut machine, &mut moved, MacroKind::GoOut).unwrap(), MacroAbort::Timeout);
    let suspended = moved.player;
    moved.player = Tile::new(0, 0);
    assert_eq!(run_with(&mut machine, &mut moved, MacroKind::GoOut).unwrap(), MacroAbort::Timeout);
    assert_eq!(moved.map, 0x25, "the stale route was not followed to the door");
    assert!(
        moved.player.y <= suspended.y,
        "it planned afresh from the top: {:?} against {suspended:?}",
        moved.player
    );
}

#[test]
fn a_goal_walled_off_from_the_fly_is_excluded_without_a_press() {
    // **The 2026-09-17 stall, reproduced.** Eight hours on rung 8 in Viridian City: 164 walks, 165
    // timeouts, not one macro finishing, and every walk spending its whole 600 frames over *two*
    // tiles with a net displacement of zero (`infra/docs/macros-traps.md`). The mechanism is the
    // closest-approach answer: with no goal reachable, `path::route` returns the route to the tile
    // that gets nearest one, the walk takes a step, the re-plan from the new tile answers with the
    // tile it just left, and the two of them trade the fly back and forth for the whole cap. The
    // frame cap then read as "closing" — it had been one tile nearer once — so it cost a strike
    // rather than excluding anything, and ten brain minutes later the same walk was available
    // again.
    //
    // A goal the search cannot reach is now excluded at `start`: nothing is pressed, the key goes
    // to the blocked ledger, and the button leaves the pad when the list empties.
    let mut world = World::room().at(0, 0);
    world.map = 0x00;
    // An item ball sealed in the far corner behind two walls: all four tiles beside it are either
    // walls or enclosed by them.
    world.npcs = vec![Npc { slot: 2, picture: BALL_SPRITE, x: 7, y: 7, facing: Facing::Down }];
    world.walls.extend([Tile::new(6, 6), Tile::new(6, 7), Tile::new(7, 6), Tile::new(5, 7)]);
    let ball = TargetKey::Thing(TalkTarget::Sprite(2));

    assert!(on_the_pad(&mut world, MacroKind::GoItem), "the ball is a candidate to begin with");
    let refused = run(&mut world, MacroKind::GoItem).expect_err("no route to any of the four");
    assert_eq!(refused.reason, Refusal::NoRoute);
    assert!(world.pulses.is_empty(), "and nothing was pressed: {:?}", world.pulses);
    assert_eq!(world.player, Tile::new(0, 0), "the fly did not move a tile");
    assert!(world.targets.blocked(world.map, ball), "the ball is excluded, not retried");
    assert!(!on_the_pad(&mut world, MacroKind::GoItem), "so the button leaves the pad");
}

#[test]
fn a_step_the_cartridge_refuses_is_a_wall_in_that_direction_only() {
    // What the collision table cannot answer, and what the walk measures instead
    // ([`path::Refusal`]): a ledge is a facing/tile/tile triple in `LedgeTiles` and a tile-pair
    // collision refuses a step between two tiles that are each passable, so neither shows up in
    // `walkable`. A step that spends its whole window with the player where it started is
    // recorded as a directed wall and the re-plan goes round it — without which the walk spends
    // its three failures re-planning the same refused step.
    let mut world = World::room().at(0, 3);
    world.size = MapSize { width: 4, height: 8 };
    world.warps = vec![Warp { x: 3, y: 3, destination_warp: 1, destination_map: 0x00 }];
    // The direct way east out of each tile of row 3 is refused, as a ledge along it would be.
    for x in 0..3 {
        world.refuse.push((Tile::new(x, 3), Facing::Right));
    }
    assert_eq!(run(&mut world, MacroKind::GoOut).unwrap(), MacroAbort::Done);
    assert_eq!(world.map, 0x00, "it went round: {:?}", world.player);

    // Directed: the same refusal does not close the way back. A walk from the far side crosses
    // row 3 westward with nothing in its way.
    let mut back = World::room().at(3, 3);
    back.size = MapSize { width: 4, height: 8 };
    back.warps = vec![Warp { x: 0, y: 3, destination_warp: 1, destination_map: 0x00 }];
    for x in 0..3 {
        back.refuse.push((Tile::new(x, 3), Facing::Right));
    }
    assert_eq!(run(&mut back, MacroKind::GoOut).unwrap(), MacroAbort::Done);
    assert_eq!(back.map, 0x00);
}

#[test]
fn the_walk_commits_to_its_route_across_the_walkable_windows_edge() {
    // The window is agent A's ten-by-nine screen buffer and it *moves with the player*
    // (`pokemon_red::state::walkable`), so an A* re-planned after every tile is an A* over a
    // different map after every tile: a tile priced as plausible ground at eight
    // (`path::UNKNOWN_STEP`) becomes a wall as the window reaches it, the cheapest route flips to
    // the far side of the obstacle, and the step back makes the tile unknown again. The route is
    // therefore planned once and followed while the player is still on it.
    //
    // A map wider than the window, with the way out at the far end: the fly cannot see the exit
    // when it sets off and has to keep walking on a plan made through unknown ground.
    let mut world = World::room().at(1, 4);
    world.size = MapSize { width: 24, height: 9 };
    world.window = true;
    world.warps = vec![Warp { x: 23, y: 4, destination_warp: 1, destination_map: 0x00 }];
    assert_eq!(run(&mut world, MacroKind::GoOut).unwrap(), MacroAbort::Done);
    assert_eq!(world.map, 0x00, "it crossed the map: {:?}", world.player);
    // The signature of the loop this replaced is a reversal: a step back the way the walk came.
    // There is none, because the plan was not remade under the fly's feet.
    let steps: Vec<u8> = world.pulses.iter().copied().filter(|mask| direction_of(*mask).is_some()).collect();
    assert!(!steps.is_empty());
    assert!(
        steps.iter().all(|mask| *mask == buttons::RIGHT),
        "the walk went one way and kept going: {steps:?}"
    );
}

#[test]
fn a_scene_change_under_a_running_macro_ends_it_as_done() {
    let mut world = World::ground_floor().at(3, 0);
    world.switch = Some((40, Scene::Battle { own_turn: true, forced_switch: false }));
    assert_eq!(run(&mut world, MacroKind::GoOut).unwrap(), MacroAbort::Done);
    assert_ne!(world.player, Tile::new(3, 7), "the walk was cut short");
    assert_eq!(world.map, 0x25, "and the door was never reached");
}

#[test]
fn go_npc_walks_next_to_the_nearest_person_and_turns_to_face_it() {
    // Outdoors, because that is where `GO NPC` is on a channel (section 9.1); indoors it is a
    // plan fallback and the script under test is the same either way.
    let mut world = World::room().at(1, 1).wall(&[(1, 4), (6, 6)]);
    world.map = 0x00;
    world.npcs = vec![
        Npc { slot: 1, picture: 1, x: 6, y: 6, facing: Facing::Up },
        Npc { slot: 2, picture: 2, x: 1, y: 4, facing: Facing::Up },
    ];
    assert_eq!(run(&mut world, MacroKind::GoNpc).unwrap(), MacroAbort::Done);
    assert_eq!(world.player, Tile::new(1, 3), "one tile above the nearer sprite");
    assert_eq!(world.facing, Facing::Down, "facing it");
}

#[test]
fn go_npc_walks_past_a_nearer_object_to_reach_a_person() {
    // Oak's lab in miniature: a ball one tile away and a person across the room. `GO NPC` is
    // "nearest person", so the ball is not a candidate however close it is.
    let mut world = World::room().at(1, 1).wall(&[(1, 2), (6, 6)]);
    world.map = 0x00;
    world.npcs = vec![
        Npc { slot: 1, picture: BALL_SPRITE, x: 1, y: 2, facing: Facing::Down },
        Npc { slot: 2, picture: 3, x: 6, y: 6, facing: Facing::Up },
    ];
    assert_eq!(run(&mut world, MacroKind::GoNpc).unwrap(), MacroAbort::Done);
    assert!(
        Tile::new(6, 6).distance(world.player) == 1,
        "it ended up beside the person, not the ball: {:?}",
        world.player
    );
}

#[test]
fn a_move_button_selects_fight_and_then_its_own_slot() {
    let mut world = World::battle();
    // A on FIGHT opens the move list, a plain column of three.
    world.opens.push_back(Opens { list: List::Moves(3), cursor: 0, max: 2, grid: false });
    assert_eq!(run(&mut world, MacroKind::Move1).unwrap(), MacroAbort::Done);
    assert_eq!(world.list, List::Moves(3));
    assert_eq!(world.cursor, 0, "slot one, because that is the button that was pressed");
    assert_eq!(world.pulses.iter().filter(|mask| **mask == buttons::A).count(), 2);
}

#[test]
fn a_move_button_walks_the_list_to_its_own_slot() {
    let mut world = World::battle();
    world.opens.push_back(Opens { list: List::Moves(3), cursor: 0, max: 2, grid: false });
    assert_eq!(run(&mut world, MacroKind::Move3).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 2, "two DOWN presses, each one read back");
    assert_eq!(world.pulses.iter().filter(|mask| **mask == buttons::DOWN).count(), 2);
}

#[test]
fn move_one_confirms_the_cursor_when_nothing_has_pp_so_struggle_happens() {
    // Row 30a and row 34 together: `CheckPlayerHasUsableMoves` prints "has no moves left!" and
    // sets Struggle *without opening the list*, so confirming FIGHT is the whole macro and a
    // second cursor step would press A at text.
    let mut world = World::battle();
    world.mons[0] = mon(0, 4, 20, &[(33, 0), (52, 0), (45, 0)]);
    assert_eq!(run(&mut world, MacroKind::Move1).unwrap(), MacroAbort::Done);
    assert_eq!(
        world.pulses.iter().filter(|mask| **mask == buttons::A).count(),
        1,
        "FIGHT and nothing after it: {:?}",
        world.pulses
    );

    // With the list already open and everything spent, it confirms where the cursor stands.
    let mut world = World::battle();
    world.mons[0] = mon(0, 4, 20, &[(33, 0), (52, 0), (45, 0)]);
    world.list = List::Moves(3);
    world.grid = false;
    world.cursor = 1;
    world.cursor_max = 2;
    assert_eq!(run(&mut world, MacroKind::Move1).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 1, "wherever it was, so Struggle happens");
    assert_eq!(world.pulses.last(), Some(&buttons::A));
}

#[test]
fn a_cursor_reaches_the_far_corner_of_reds_two_by_two_menu() {
    // RUN is the fourth entry of a grid, which no number of DOWN presses can reach. The cursor is
    // read after every press, so the script finds that out and changes axis.
    let mut world = World::battle();
    // One Pokémon on four of twenty: `RUN` is on the pad only in a wild battle the fly is losing
    // (section 13.1), and this test is about the cursor rather than about the precondition.
    world.mons.truncate(1);
    assert_eq!(run(&mut world, MacroKind::Run).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, battle_entry::RUN, "RUN, by watching where the cursor went");
    assert!(world.pulses.contains(&buttons::RIGHT), "it needed the other axis: {:?}", world.pulses);
    assert_eq!(world.pulses.last(), Some(&buttons::A));
}

#[test]
fn switch_picks_the_healthiest_bench_entry_through_the_party_list() {
    let mut world = World::battle();
    world.opens.push_back(Opens { list: List::BattleParty, cursor: 0, max: 2, grid: false });
    assert_eq!(run(&mut world, MacroKind::Switch).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 1, "the healthy bench entry");
    assert_eq!(
        world.pulses.iter().filter(|mask| **mask == buttons::A).count(),
        3,
        "PKMN, the party entry, then SWITCH: {:?}",
        world.pulses
    );
}

#[test]
fn a_forced_switch_goes_straight_to_the_party_list() {
    let mut world = World::battle();
    world.scene = Scene::Battle { own_turn: true, forced_switch: true };
    world.battle = Some((BattleKind::Wild, true, true));
    world.list = List::BattleParty;
    world.cursor_max = 2;
    world.grid = false;
    assert_eq!(run(&mut world, MacroKind::Switch).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 1);
    assert_eq!(
        world.pulses.iter().filter(|mask| **mask == buttons::A).count(),
        2,
        "the party entry and then SWITCH, with no battle menu in between: {:?}",
        world.pulses
    );
}

#[test]
fn item_reaches_the_potion_by_reading_the_bags_cursor() {
    // The one script agent A's seam cannot yet support: `BattleMenu` has no bag variant, so the
    // bag list reports no cursor. The contract's rule wins over finishing the job — "navigate by
    // reading cursor state, never by counting presses" — and since section 14 the seam reports one
    // for the in-battle bag, so `ITEM` reaches the potion instead of waiting out `CURSOR_WAIT`.
    let mut world = World::battle();
    world.bag = vec![(item::POKE_BALL, 3), (item::POTION, 2)];
    world.opens.push_back(Opens { list: List::BattleBag, cursor: 0, max: 1, grid: false });
    // And confirming the potion opens the party list, which is where the script says which
    // Pokémon to heal. Section 12.11: that step waits for the *party* list, so the fixture has to
    // open it -- the cartridge does.
    world.opens.push_back(Opens { list: List::BattleParty, cursor: 0, max: 2, grid: false });
    assert_eq!(run(&mut world, MacroKind::Item).unwrap(), MacroAbort::Done);
    assert!(
        world.pulses.iter().filter(|mask| **mask == buttons::A).count() >= 2,
        "the bag was opened and the potion confirmed: {:?}",
        world.pulses
    );
}

#[test]
fn buy_potion_takes_buy_then_the_item_then_confirms_twice() {
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::BuySellQuit);
    world.cursor_max = 2;
    world.stock = vec![item::POKE_BALL, item::POTION];
    world.money = 1000;
    world.b_to_close = u8::MAX;
    // A on BUY opens the priced stock list.
    world.opens.push_back(Opens {
        list: List::Shop(ShopScreen::Buying),
        cursor: 0,
        max: 1,
        grid: false,
    });
    assert_eq!(run(&mut world, MacroKind::BuyPotion).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 1, "the potion is the second thing the mart stocks");
    assert_eq!(
        world.pulses.iter().filter(|mask| **mask == buttons::A).count(),
        4,
        "BUY, the item, the quantity and the price: {:?}",
        world.pulses
    );
}

#[test]
fn buy_ball_skips_the_counter_menu_when_the_buy_list_is_already_up() {
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::Buying);
    world.cursor_max = 1;
    world.stock = vec![item::POKE_BALL, item::POTION];
    world.money = 1000;
    world.b_to_close = u8::MAX;
    assert_eq!(run(&mut world, MacroKind::BuyBall).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 0);
    assert_eq!(
        world.pulses.iter().filter(|mask| **mask == buttons::A).count(),
        3,
        "the item, the quantity and the price: {:?}",
        world.pulses
    );
}

#[test]
fn leave_backs_out_of_a_pc() {
    let mut world = World::room();
    world.scene = Scene::Pc;
    world.list = List::Pc;
    world.b_to_close = 2;
    assert_eq!(run(&mut world, MacroKind::Leave).unwrap(), MacroAbort::Done);
    assert_eq!(world.pulses, vec![buttons::B; 2]);
    assert_eq!(world.scene, Scene::Overworld);
}

#[test]
fn a_cursor_that_cannot_reach_its_target_reports_blocked() {
    // A list whose cursor is pinned to one entry: the target is off the end of it.
    let mut world = World::battle();
    world.mons.truncate(1);
    world.cursor_max = 0;
    assert_eq!(run(&mut world, MacroKind::Run).unwrap(), MacroAbort::Blocked);
    assert!(!world.pulses.contains(&buttons::A), "it never confirmed the wrong entry");
}

#[test]
fn running_names_the_macro_while_it_owns_the_buttons() {
    let mut world = World::ground_floor().at(3, 4);
    let (palette, slot) = pick(&mut world, MacroKind::GoOut);
    let mut machine = MacroMachine::new(1);
    assert_eq!(machine.running(), None);
    machine.start(&palette, slot, &mut world).expect("GO OUT starts");
    assert_eq!(machine.running(), Some("GO OUT"));
    assert!(machine.outcome().is_none(), "nothing has finished yet");
    let mut frames = 0;
    while let Some(mask) = machine.step(&mut world) {
        world.frame(mask);
        frames += 1;
        assert_eq!(machine.running(), Some("GO OUT"));
        assert_eq!(machine.frames(), frames);
    }
    assert!(frames > 0 && frames < FRAME_CAP, "{frames} frames");
    assert_eq!(machine.running(), None, "the buttons go back between macros");
    assert_eq!(machine.outcome(), Some(("GO OUT", MacroAbort::Done)));
}

#[test]
fn cancel_gives_the_buttons_back_as_blocked() {
    let mut world = World::ground_floor().at(3, 4);
    let (palette, slot) = pick(&mut world, MacroKind::GoOut);
    let mut machine = MacroMachine::new(1);
    machine.start(&palette, slot, &mut world).expect("GO OUT starts");
    machine.cancel();
    assert_eq!(machine.running(), None);
    assert_eq!(machine.outcome(), Some(("GO OUT", MacroAbort::Blocked)));
    assert!(machine.step(&mut world).is_none());
}

#[test]
fn every_outcome_and_refusal_has_a_word_for_the_ticker() {
    for (outcome, label) in [
        (MacroAbort::Done, "done"),
        (MacroAbort::Blocked, "blocked"),
        (MacroAbort::Timeout, "timeout"),
        (MacroAbort::Refused, "refused"),
    ] {
        assert_eq!(outcome.label(), label);
    }
    for reason in [
        Refusal::Unbound,
        Refusal::WrongScene,
        Refusal::Precondition,
        Refusal::NoRoute,
        Refusal::Busy,
    ] {
        assert!(!reason.label().is_empty());
    }
}

// ---------------------------------------------------------------------------------------------
// Section 9: plan mode's policy
// ---------------------------------------------------------------------------------------------

/// The plan's entries in rank order, with `-` for a rank nothing reached.
fn plan(world: &mut World) -> Vec<&'static str> {
    let scene = world.scene();
    names(&plan::plan_for(scene, world))
}

/// A person and an object on the ground floor, so both walking entries have something to aim at.
///
/// The fly faces *away* from both of them, because `TALK` ranks first whenever it is already
/// facing one (section 9.1, amended) and these fixtures are about the rest of the order. The
/// facing case has its own test.
fn populated() -> World {
    let mut world = World::ground_floor().wall(&[(3, 4)]);
    world.facing = Facing::Up;
    world.npcs = vec![
        Npc { slot: 1, picture: 1, x: 4, y: 3, facing: Facing::Down },
        Npc { slot: 2, picture: BALL_SPRITE, x: 3, y: 4, facing: Facing::Down },
    ];
    world
}

#[test]
fn the_indoor_pad_is_section_nine_ones_set() {
    let mut world = populated();
    assert_eq!(
        plan(&mut world),
        ["GO OUT", "GO WARP", "GO ITEM", "GO NPC", "GO FRONTIER"],
        "no objective is known, so `GO OBJECTIVE` is not on the pad"
    );
}

#[test]
fn the_outdoor_pad_has_the_route_and_no_passage_in_it() {
    let mut world = populated();
    world.map = 0x00;
    assert_eq!(plan(&mut world), ["GO ROUTE", "GO ITEM", "GO NPC", "GO FRONTIER"]);
}

#[test]
fn a_known_objective_takes_rank_zero() {
    // Standing on Red's ground floor with the next rung in Pallet Town: the doormats are written
    // `LAST_MAP`, and an outdoor objective is what makes them the objective's own door (section 9).
    let mut world = populated();
    world.warps[1].destination_map = 0xff;
    world.warps[2].destination_map = 0xff;
    world.objective = Some(Objective { map: 0x00, tile: None, warp: None, edge: None, target: None });
    assert_eq!(plan(&mut world)[0], "GO OBJECTIVE");
    let goals: Vec<Tile> = objective_goals(&mut world).into_iter().map(|aim| aim.tile).collect();
    assert_eq!(goals, vec![Tile::new(2, 7), Tile::new(3, 7)], "the two doormats and not the stairs");
}

#[test]
fn a_rung_on_another_floor_of_this_building_is_reached_through_the_passage() {
    // Section 9.1 used to say "`GO WARP` ranks above `GO OUT` only when the objective is on
    // another floor of the same building". Section 14 leaves no ranking to state: both buttons are
    // on the pad, the fly chooses, and what the *objective* aims at is the passage -- which is the
    // half that was ever observable.
    let mut world = populated();
    world.objective = Some(Objective { map: 0x26, tile: None, warp: None, edge: None, target: None });
    assert!(plan::passage_to_objective(&mut world));
    assert_eq!(
        plan(&mut world),
        ["GO OBJECTIVE", "GO OUT", "GO WARP", "GO ITEM", "GO NPC", "GO FRONTIER"]
    );
    // And the objective's own goal is the staircase, because that is the warp that names it.
    let goals: Vec<Tile> = objective_goals(&mut world).into_iter().map(|aim| aim.tile).collect();
    assert_eq!(goals, vec![Tile::new(7, 1)]);
}

#[test]
fn an_objective_on_this_map_with_no_finer_place_falls_through() {
    // Every rung the Pokémon catalog knows names a map and nothing finer, so standing on that map
    // is as close as `GO OBJECTIVE` can get: its button leaves the pad, which is exactly the
    // behaviour wanted in Oak's lab.
    let mut world = populated();
    world.objective = Some(Objective { map: world.map, tile: None, warp: None, edge: None, target: None });
    assert!(objective_goals(&mut world).is_empty());
    // The way out is still on the pad, because section 14 leaves no ranking to demote it in.
    // What kept the fly from bouncing between Oak's lab and Pallet Town once per hold is the
    // *ledger* half of that fix (`ways`' tiers), and it is unchanged.
    assert_eq!(
        plan(&mut world),
        ["GO OUT", "GO WARP", "GO ITEM", "GO NPC", "GO FRONTIER"]
    );

    // A rung the catalog does know a tile for is walked to instead.
    world.objective = Some(Objective { map: world.map, tile: Some(Tile::new(6, 3)), warp: None, edge: None, target: None });
    assert_eq!(
        plan(&mut world),
        ["GO OBJECTIVE", "GO OUT", "GO WARP", "GO ITEM", "GO NPC", "GO FRONTIER"]
    );
    let aims = objective_goals(&mut world);
    assert_eq!(aims.iter().map(|aim| (aim.tile, aim.press)).collect::<Vec<_>>(), vec![
        (Tile::new(6, 3), None)
    ]);
    // And it carries the key the blocked ledger would name it by, which is the tile itself.
    assert_eq!(aims[0].key, Some(TargetKey::Tile(Tile::new(6, 3))));
}

#[test]
fn a_thing_this_session_has_talked_to_leaves_the_plan() {
    // Section 12's talked ledger: a person this session has talked to stops being a reason to walk
    // anywhere, and when the map's people are all talked to `GO NPC` is not on the pad at all.
    // The test it replaces asked the exploration ledger whether the fly had stood on every tile
    // around the thing, which never closes for a villager standing in the open -- the town loop.
    let mut world = populated();
    world.talked.insert(TalkTarget::Sprite(1));
    assert_eq!(
        plan(&mut world),
        ["GO OUT", "GO WARP", "GO ITEM", "GO FRONTIER"],
        "the person drops out and the object stays"
    );
    assert!(!precondition(MacroKind::GoNpc, &mut world), "and the button is gone");

    // The object is the other half, keyed by its own sprite slot.
    world.talked.insert(TalkTarget::Sprite(2));
    assert_eq!(plan(&mut world), ["GO OUT", "GO WARP", "GO FRONTIER"]);

    // Palette mode reads the same ledger: an unbound slot is dim rather than aimed at a villager
    // the fly has already heard out.
    let mut outdoors = populated();
    outdoors.map = 0x00;
    outdoors.talked.insert(TalkTarget::Sprite(1));
    assert!(!names(&Palette::for_scene(Scene::Overworld, &mut outdoors)).contains(&"GO NPC"));
}

#[test]
fn the_battle_plan_attacks_first_and_switches_only_under_a_quarter() {
    // `World::battle`'s active Pokémon is on 4 of 20 HP, which is under a quarter, with a healthy
    // one on the bench: both of section 9's conditions hold.
    let mut world = World::battle();
    assert!(plan::failing(&mut world));
    let pad = plan(&mut world);
    assert!(pad.contains(&"MOVE 1") && pad.contains(&"SWITCH"), "{pad:?}");

    // Healthy again: `ATTACK` alone, because nothing is wrong.
    let mut healthy = World::battle();
    healthy.mons[0] = mon(0, 20, 20, &[(33, 30)]);
    assert!(!plan::failing(&mut healthy));
    // `MOVE 1` is what keeps the own-turn pad from being empty when every other button drops out:
    // FIGHT is one of this menu's four entries and it always opens (12.8), where the `NEXT` that
    // used to carry the job merely reopened the list `BACK` had just closed (12.10). `ITEM` needs
    // a potion and low HP, so it is absent here; this Pokémon has one move.
    assert_eq!(plan(&mut healthy), ["MOVE 1", "SWITCH"]);
}

#[test]
fn the_battle_plan_runs_from_a_wild_battle_only_when_the_whole_party_is_weak() {
    let mut world = World::battle();
    assert!(!plan::party_weak(&mut world), "there is a healthy one on the bench");
    assert!(!plan(&mut world).contains(&"RUN"));

    // Every Pokémon that can still fight is under a quarter: there is nothing to switch to and
    // the next hit ends the battle, which is the state section 9 puts `RUN` in.
    world.mons = vec![mon(0, 4, 20, &[(33, 30)]), mon(1, 3, 20, &[(33, 30)])];
    assert!(plan::party_weak(&mut world));
    assert_eq!(plan(&mut world), ["MOVE 1", "SWITCH", "RUN"]);

    // A trainer battle has no RUN at all, in the plan or in the palette.
    world.battle = Some((BattleKind::Trainer, true, false));
    assert!(!plan(&mut world).contains(&"RUN"));
}

#[test]
fn a_forced_switch_plans_one_entry_and_the_other_scenes_plan_their_one_move() {
    let mut world = World::battle();
    world.scene = Scene::Battle { own_turn: false, forced_switch: true };
    world.battle = Some((BattleKind::Wild, false, true));
    world.list = List::BattleParty;
    world.cursor_max = 2;
    // `SWITCH` plus the press that advances a battle: a forced switch with nothing healthy to
    // switch to is a menu the game will not let the fly cancel, and the pad was empty there.
    assert_eq!(plan(&mut world), ["NEXT", "SWITCH"]);

    // Section 12's sets, one row each. `YES`/`NO` are on every dialog because the cartridge has no
    // "a choice is open" flag to gate them on; `BACK` is on `Unknown` because that is where the
    // Pokédex, the trainer card and OPTION land and A does not leave any of them; the mart's four
    // purchases stay unbound until the stock list is read.
    for (scene, entries) in [
        (Scene::Dialog, vec!["NEXT", "YES", "NO"]),
        (Scene::Unknown, vec!["NEXT", "BACK"]),
        (Scene::Menu, vec!["CLOSE", "CONFIRM", "BACK"]),
        // Section 13: the shop's four purchases stay unbound until a counter's stock list is
        // read, so what is left is the two answers any list has.
        (Scene::Shop, vec!["CONFIRM", "LEAVE"]),
        (Scene::Pc, vec!["CONFIRM", "LEAVE"]),
    ] {
        let mut world = World::room();
        world.scene = scene;
        world.box_open = scene == Scene::Unknown;
        assert_eq!(plan(&mut world), entries, "{}", scene.label());
    }

    // The title screen has no plan, exactly as it has no palette (section 2).
    let mut title = World::room();
    title.scene = Scene::Title;
    assert!(plan(&mut title).is_empty());
}

#[test]
fn the_plan_is_deterministic_and_offers_nothing_the_palette_would_leave_unbound() {
    let mut world = populated();
    let once = plan(&mut world);
    let twice = plan(&mut world);
    assert_eq!(once, twice, "the same game state plans the same order");
    for name in once.iter().filter(|name| **name != "-") {
        let kind = MacroKind::ALL
            .into_iter()
            .find(|kind| kind.name() == *name)
            .expect("a plan entry is a macro");
        assert!(precondition(kind, &mut world), "{name} is in the plan with no precondition");
    }
}

#[test]
fn talk_is_on_the_pad_while_the_fly_faces_something_it_has_not_talked_to() {
    // Section 9.1, amended 2026-09-16, and section 14: `TALK` is on the overworld pad whenever the
    // fly is facing an untalked object or person and absent otherwise. It used to be "ranks
    // first"; with a slot per type there is no ranking left, only the precondition.
    let mut world = populated();
    world.facing = Facing::Down; // the ball at (3, 4) is one step down from (3, 3)
    assert!(super::palette::facing_untalked(&mut world));
    assert!(plan(&mut world).contains(&"TALK"));

    // A person is the other half of it, and a bare tile is neither.
    let mut person = populated();
    person.facing = Facing::Right; // the villager at (4, 3)
    assert!(plan(&mut person).contains(&"TALK"));
    let mut nothing = populated();
    nothing.facing = Facing::Left; // (2, 3) has nothing on it
    assert!(!super::palette::facing_untalked(&mut nothing));
    assert!(!plan(&mut nothing).contains(&"TALK"), "an empty tile is not a reason to press A");

    // A sign is the third table it reads, and a sign in a wall is the case a rule about the
    // *thing's* other sides could never have handled.
    let mut sign = World::room().at(3, 3);
    sign.facing = Facing::Down;
    sign.signs = vec![Sign { x: 3, y: 4, text_id: 1 }];
    sign.walls.insert(Tile::new(3, 4));
    assert!(super::palette::facing_untalked(&mut sign));
    assert!(plan(&mut sign).contains(&"TALK"));

    // And what stops the press repeating is the ledger: a thing this session has talked to is
    // neither faced nor walked to again.
    let mut touched = populated();
    touched.facing = Facing::Down;
    touched.talked.insert(TalkTarget::Sprite(2));
    assert!(!super::palette::facing_untalked(&mut touched), "the ball has been talked to");
    assert!(!plan(&mut touched).contains(&"TALK"));
    assert!(!plan(&mut touched).contains(&"GO ITEM"));
}

#[test]
fn the_two_dealers_are_one_table() {
    // Section 14: `plan_for` *is* `Palette::for_scene`. Two dealers that could disagree about
    // which buttons a scene has -- and did, for two releases, over `GO OBJECTIVE` -- are one, and
    // a slot is a type index in both.
    let mut world = populated();
    world.facing = Facing::Down;
    assert_eq!(
        names(&Palette::for_scene(Scene::Overworld, &mut world)),
        plan(&mut world)
    );
    assert!(plan(&mut world).contains(&"TALK"));
    assert_eq!(
        Palette::for_scene(Scene::Overworld, &mut world)
            .slot(MacroId(MacroKind::Talk.slot()))
            .map(|spec| spec.name),
        Some("TALK")
    );
}

// ---------------------------------------------------------------------------------------------
// Section 12: the blocked- and reached-target ledgers (Viridian City, 2026-09-16)
// ---------------------------------------------------------------------------------------------

/// Two objects on the floor, neither next to the fly, in a room where no step ever lands.
///
/// `glue` is what makes a walk fail the way the cartridge failed in Viridian City: the route
/// search finds a way, the macro starts, and three tiles in a row leave the player where it was,
/// which is [`MacroAbort::Blocked`]. The two objects are at different distances so that "the
/// macro picks the next candidate" is observable rather than inferred.
fn two_unreachable_objects() -> World {
    let mut world = World::room();
    world.facing = Facing::Up;
    world.npcs = vec![
        Npc { slot: 2, picture: BALL_SPRITE, x: 1, y: 1, facing: Facing::Down },
        Npc { slot: 3, picture: BALL_SPRITE, x: 6, y: 6, facing: Facing::Down },
    ];
    world.glue = true;
    world
}

/// Whether `kind`'s button is on the pad in the scene the world is in.
fn on_the_pad(world: &mut World, kind: MacroKind) -> bool {
    let scene = world.scene();
    plan::plan_for(scene, world).slots.iter().flatten().any(|spec| spec.kind == kind)
}

#[test]
fn a_blocked_target_leaves_the_choice_for_ten_brain_minutes_and_the_button_leaves_the_pad() {
    // The live stall: `GO ITEM start, GO ITEM blocked`, once per hold for forty-five minutes,
    // because "nearest untalked" re-chose the same unreachable object every time. The fix is a
    // ledger of what a walk has just failed at, read where the target is chosen.
    let mut world = two_unreachable_objects();
    assert!(on_the_pad(&mut world, MacroKind::GoItem));
    assert_eq!(
        untalked_objects(&mut world).len(),
        2,
        "both objects are candidates before anything has failed"
    );

    // The first walk aims at the nearer ball and blocks. That excludes *that* ball, and the macro
    // is left with the other one -- which is the whole difference from the live behaviour.
    assert_eq!(run(&mut world, MacroKind::GoItem), Ok(MacroAbort::Blocked));
    assert_eq!(
        untalked_objects(&mut world),
        vec![(Tile::new(6, 6), TalkTarget::Sprite(3))],
        "the ball the walk failed at is out of the choice; the other one is not"
    );
    assert!(on_the_pad(&mut world, MacroKind::GoItem), "there is still something to walk to");

    // The second blocks too, and now there is no candidate left: the button leaves the pad rather
    // than sitting on it and blocking once per hold.
    assert_eq!(run(&mut world, MacroKind::GoItem), Ok(MacroAbort::Blocked));
    assert!(untalked_objects(&mut world).is_empty());
    assert!(!on_the_pad(&mut world, MacroKind::GoItem), "no candidate, no button");
    assert_eq!(world.targets.len(), (2, 0), "two exclusions standing, nothing reached");

    // A window, not a life sentence: unreachable is a fact about where the fly was standing, so
    // ten brain minutes later both balls are candidates again.
    world.targets.clock(BLOCKED_MINUTES_DEFAULT * 60_000.0 - 1.0);
    assert!(!on_the_pad(&mut world, MacroKind::GoItem), "still inside the window");
    world.targets.clock(BLOCKED_MINUTES_DEFAULT * 60_000.0);
    assert_eq!(untalked_objects(&mut world).len(), 2, "the window has closed on both");
    assert!(on_the_pad(&mut world, MacroKind::GoItem));
    assert_eq!(world.targets.len(), (0, 0));
}

#[test]
fn a_timeout_costs_a_closing_walk_a_strike_and_a_stalled_one_its_target() {
    // The Viridian loop's first cause (2026-09-17, `infra/docs/macros-traps.md`). The frame cap is
    // ten seconds and a town is wider than ten seconds of walking, so a walk that spends the cap
    // getting *closer* has not failed at its target -- it is half way there. Excluding it anyway is
    // what took the Route 2 connection off `GO ROUTE`'s list for ten brain minutes at a time, and
    // `GO OBJECTIVE` with it, because both aim at the same exit key.
    //
    // The cap that ends such a walk is [`WALK_FRAME_CEILING`] since 2026-09-17 — a minute of brain
    // time, not ten seconds — so the map here is one no minute of walking crosses.
    let mut world = World::room();
    world.size = MapSize { width: 254, height: 254 };
    world.facing = Facing::Up;
    world.npcs = vec![Npc { slot: 2, picture: BALL_SPRITE, x: 250, y: 250, facing: Facing::Down }];
    let ball = TargetKey::Thing(TalkTarget::Sprite(2));
    let began = world.player.distance(Tile::new(250, 250));
    let mut machine = MacroMachine::new(0x1234_5678);

    assert!(on_the_pad(&mut world, MacroKind::GoItem));
    assert_eq!(run_with(&mut machine, &mut world, MacroKind::GoItem), Ok(MacroAbort::Timeout));
    assert!(!world.targets.blocked(world.map, ball), "a closing walk keeps its target");
    assert_eq!(world.targets.strikes(world.map, ball), 1, "it costs one strike");
    // So the next hold resumes it, from closer than it started.
    assert!(on_the_pad(&mut world, MacroKind::GoItem));
    assert!(world.player.distance(Tile::new(250, 250)) < began);

    // But not for ever: three ceilings is three brain minutes of walking at one thing, and a target
    // still not reached by then is one to leave alone for a while.
    assert_eq!(run_with(&mut machine, &mut world, MacroKind::GoItem), Ok(MacroAbort::Timeout));
    assert!(!world.targets.blocked(world.map, ball));
    assert_eq!(run_with(&mut machine, &mut world, MacroKind::GoItem), Ok(MacroAbort::Timeout));
    assert!(world.targets.blocked(world.map, ball), "the third cap excludes it");
    assert_eq!(world.targets.strikes(world.map, ball), 0, "and the exclusion supersedes the count");
    assert!(!on_the_pad(&mut world, MacroKind::GoItem));

    // A walk that spends the cap without ever getting nearer is a different fact, and it excludes
    // the target at once: ten seconds of buttons and no ground gained is about the target.
    let mut away = World::room();
    away.size = MapSize { width: 64, height: 64 };
    away.facing = Facing::Up;
    away.npcs = vec![Npc { slot: 2, picture: BALL_SPRITE, x: 60, y: 60, facing: Facing::Down }];
    away.glue = true;
    // Glued in place, the three-failure rule fires first -- which is the other abort, and it
    // excludes the target as it always has.
    assert_eq!(run(&mut away, MacroKind::GoItem), Ok(MacroAbort::Blocked));
    assert!(away.targets.blocked(away.map, ball));
    assert_eq!(away.targets.strikes(away.map, ball), 0, "a block is not a strike");
    assert!(!on_the_pad(&mut away, MacroKind::GoItem));
}

#[test]
fn a_reached_person_is_skipped_for_the_window() {
    // The other half of the stall: `GO NPC start, GO NPC done`, over and over, because only a
    // completed `TALK` marked a person talked and the fly is free never to press A. Arriving and
    // facing is all `GO NPC` promises, so arriving and facing retires the target.
    let mut world = World::room();
    // Pallet Town: an outdoor map, which is where `GO NPC` is one of the six channels.
    world.map = 0x00;
    world.facing = Facing::Up;
    world.npcs = vec![Npc { slot: 1, picture: 1, x: 4, y: 3, facing: Facing::Down }];

    assert!(on_the_pad(&mut world, MacroKind::GoNpc));
    assert_eq!(run(&mut world, MacroKind::GoNpc), Ok(MacroAbort::Done));
    assert!(
        world.targets.reached(world.map, TargetKey::Thing(TalkTarget::Sprite(1))),
        "arrived and facing is what the reached ledger records"
    );
    assert!(untalked_people(&mut world).is_empty());
    assert!(!on_the_pad(&mut world, MacroKind::GoNpc), "the map's only person is accounted for");

    // But a window, not a retirement: **arriving is not talking**
    // (`docs/design/macros.md` section 12.4). 12.1 retired a reached target for the session, which
    // also retired every person the fly walked to and then wandered off from without pressing A --
    // and nothing could bring it back. Oak is the case that found it: the fly stood in front of
    // him, chose something else, and the parcel stayed undelivered for ever. An hour later he is
    // on offer again.
    world.targets.clock(60.0 * 60_000.0);
    assert_eq!(world.targets.len(), (0, 0), "the window has closed");
    // From a tile that is not in front of him, which is the state `TALK` owns (row 4).
    world.player = Tile::new(4, 1);
    assert!(on_the_pad(&mut world, MacroKind::GoNpc), "so the conversation is available again");
}

#[test]
fn a_finished_talk_still_excludes_the_thing_it_talked_to() {
    // The ledger section 9.2 added is untouched: a completed `TALK` writes it, and a thing that
    // has been talked to is neither walked to nor faced again -- which is what makes the reached
    // ledger an addition rather than a replacement.
    let mut world = World::room();
    world.map = 0x00;
    world.facing = Facing::Right;
    world.npcs = vec![Npc { slot: 1, picture: 1, x: 4, y: 3, facing: Facing::Left }];

    assert!(on_the_pad(&mut world, MacroKind::Talk));
    assert_eq!(run(&mut world, MacroKind::Talk), Ok(MacroAbort::Done));
    assert!(world.talked.contains(&TalkTarget::Sprite(1)), "the talked ledger, not the reached");
    assert_eq!(world.targets.len(), (0, 0), "a TALK reaches nothing: it is already standing there");
    assert!(!on_the_pad(&mut world, MacroKind::Talk));
    assert!(!on_the_pad(&mut world, MacroKind::GoNpc));
}

#[test]
fn a_blocked_way_out_leaves_the_pad_rather_than_being_aimed_at_again() {
    // The same ledger over `GO OUT`, `GO WARP` and `GO ROUTE`, whose key is the exit rather
    // than the thing. Red's ground floor has two doormats, so the first failure leaves one.
    let mut world = World::ground_floor();
    world.glue = true;
    assert!(on_the_pad(&mut world, MacroKind::GoOut));
    assert_eq!(ways(&mut world, Way::Exit).len(), 2, "two doormats");

    assert_eq!(run(&mut world, MacroKind::GoOut), Ok(MacroAbort::Blocked));
    assert_eq!(ways(&mut world, Way::Exit).len(), 1, "the mat the walk failed at is excluded");
    assert!(on_the_pad(&mut world, MacroKind::GoOut));

    assert_eq!(run(&mut world, MacroKind::GoOut), Ok(MacroAbort::Blocked));
    assert!(ways(&mut world, Way::Exit).is_empty());
    assert!(!on_the_pad(&mut world, MacroKind::GoOut), "no way out left to aim at");
    // The staircase is a different key and a different macro: excluding the mats says nothing
    // about it.
    assert_eq!(ways(&mut world, Way::Passage).len(), 1);
    assert!(on_the_pad(&mut world, MacroKind::GoWarp));
}

#[test]
fn a_frontier_tile_stood_on_stops_being_one_and_a_blocked_one_is_excluded() {
    // Section 12's second ledger needs nothing for `GO FRONTIER`'s own arrival, and this is the
    // verification of that claim rather than an assumption: the tile the macro steps onto enters
    // the adapter's exploration ledger, and `path::frontier` already asks it. Only the *blocked*
    // half is new here.
    let mut world = World::room();
    for y in 0..8 {
        for x in 0..8 {
            world.stood.insert(Tile::new(x, y));
        }
    }
    world.stood.remove(&Tile::new(0, 0));
    assert_eq!(
        super::palette::frontier_aims(&mut world),
        vec![(Tile::new(0, 1), Facing::Up), (Tile::new(1, 0), Facing::Left)],
        "the two tiles bordering the one square of new ground"
    );

    // Standing on it is what retires it, and nothing else has to record anything.
    world.stood.insert(Tile::new(0, 0));
    assert!(super::palette::frontier_aims(&mut world).is_empty());
    assert!(!on_the_pad(&mut world, MacroKind::GoFrontier));

    // And a frontier the walk cannot get to is excluded by the blocked ledger, one tile at a
    // time, until the button leaves the pad.
    world.stood.remove(&Tile::new(0, 0));
    world.glue = true;
    assert_eq!(run(&mut world, MacroKind::GoFrontier), Ok(MacroAbort::Blocked));
    assert_eq!(super::palette::frontier_aims(&mut world).len(), 1);
    assert_eq!(run(&mut world, MacroKind::GoFrontier), Ok(MacroAbort::Blocked));
    // Both *bordering* tiles are excluded now, so the windowed answer is empty -- and section
    // 13.1's fallback offers the unstood tile itself, which is the one thing left on this map
    // worth walking to. One more blocked walk excludes that too and the button leaves the pad, so
    // the loop is still bounded at one walk per target per window.
    assert_eq!(
        super::palette::frontier_aims(&mut world),
        vec![(Tile::new(0, 0), Facing::Down)],
        "the nearest unstood tile on the whole map"
    );
    assert_eq!(run(&mut world, MacroKind::GoFrontier), Ok(MacroAbort::Blocked));
    assert!(super::palette::frontier_aims(&mut world).is_empty());
    assert!(!on_the_pad(&mut world, MacroKind::GoFrontier));
}

#[test]
fn the_exclusion_window_is_ten_brain_minutes_by_default() {
    // The operator's number, and the one figure of the pair that is a judgement call
    // (`FLY_MACRO_BLOCKED_MINUTES` overrides it in the session's ledger).
    assert_eq!(BLOCKED_MINUTES_DEFAULT, 10.0);
    let mut ledger = Targets::default();
    assert_eq!(ledger.minutes(), 10.0);
    assert!(ledger.is_empty());

    let target = TargetKey::Exit(ExitId::Warp(1));
    ledger.clock(1_000.0);
    ledger.record_blocked(0x22, target);
    assert!(ledger.blocked(0x22, target));
    assert!(!ledger.blocked(0x23, target), "the same warp index on another map is another target");
    assert!(!ledger.reached(0x22, target), "blocked is not reached");

    ledger.clock(1_000.0 + 10.0 * 60_000.0 - 1.0);
    assert!(ledger.blocked(0x22, target));
    ledger.clock(1_000.0 + 10.0 * 60_000.0);
    assert!(!ledger.blocked(0x22, target), "the window has closed");
    assert!(ledger.is_empty());

    // A second failure restarts the window rather than keeping the first instant.
    ledger.record_blocked(0x22, target);
    assert!(ledger.blocked(0x22, target));
    assert_eq!(ledger.len(), (1, 0));
}

// ---------------------------------------------------------------------------------------------
// The Viridian loop of 2026-09-17 and the traps the audit beside it found
// (`infra/docs/macros-traps.md`).
// ---------------------------------------------------------------------------------------------

#[test]
fn a_thing_the_fly_is_already_facing_is_talks_and_not_a_walks() {
    // `GO NPC start, GO NPC done (150 ms)`, once per hold. Standing beside the girl and looking at
    // her is everything `GO NPC` promises, so the walk finishes in twenty frames having moved
    // nothing; the press that belongs on the pad there is `TALK`.
    let mut world = World::room();
    world.map = 0x00;
    world.player = Tile::new(3, 3);
    world.facing = Facing::Right;
    world.npcs = vec![Npc { slot: 1, picture: 1, x: 4, y: 3, facing: Facing::Left }];

    assert!(untalked_people(&mut world).is_empty(), "the person ahead is not a walk");
    assert!(!on_the_pad(&mut world, MacroKind::GoNpc));
    assert!(on_the_pad(&mut world, MacroKind::Talk), "the press is what is offered instead");

    // Turn away and she is a walk again: nothing is retired here, and a fly that never presses A
    // has not lost the person.
    world.facing = Facing::Left;
    assert_eq!(untalked_people(&mut world).len(), 1);
    assert!(on_the_pad(&mut world, MacroKind::GoNpc));
    assert!(!on_the_pad(&mut world, MacroKind::Talk));
}

#[test]
fn a_front_door_nobody_can_name_is_not_somewhere_new() {
    // The other half of the warp bounce. A house with no row in the geography table has a front
    // door whose destination is `LAST_MAP`, and treating "nobody can name it" as "somewhere new"
    // made `GO OUT` permanently first-tier fresh -- so the fly, standing on the doormat it had just
    // walked in over, left again inside ten frames, every hold.
    let mut world = World::ground_floor();
    world.map = 0x2b; // An interior with no row in `macros::geography`.
    world.warps = vec![
        Warp { x: 7, y: 1, destination_warp: 2, destination_map: 0x26 },
        Warp { x: 2, y: 7, destination_warp: 1, destination_map: 0xff },
    ];
    let fresh: Vec<Exit> = ways(&mut world, Way::Exit);
    assert_eq!(fresh.len(), 1, "the door is still a candidate: a room has to be leavable");
    // By the last-resort tier rather than as somewhere new, which is what the staircase beside it
    // demonstrates: a passage into an unvisited interior *is* fresh, and stops being offered once
    // the floor above has been seen.
    assert_eq!(ways(&mut world, Way::Passage).len(), 1);
    world.seen_maps.insert(0x26);
    assert!(ways(&mut world, Way::Passage).is_empty());
}

#[test]
fn a_frontier_tile_whose_new_ground_cannot_be_stood_on_is_excluded() {
    // `GO FRONTIER done (420 ms)`, over and over, on the same tile. The arrival press faces the new
    // ground, and the contract calls that `Done` whether or not the player moved -- so a ledge, a
    // tile-pair rule or a person on the far side leaves the tile a frontier for ever, because the
    // exploration ledger only ever records ground somebody stood on.
    let mut world = World::room();
    for y in 0..8 {
        for x in 0..8 {
            world.stood.insert(Tile::new(x, y));
        }
    }
    world.stood.remove(&Tile::new(4, 3));
    world.player = Tile::new(3, 3);
    // The fly is standing on a tile that borders the one square of new ground, so the walk has no
    // tiles to walk and goes straight to its arrival.
    assert!(super::palette::frontier_aims(&mut world).contains(&(Tile::new(3, 3), Facing::Right)));
    world.glue = true;

    assert_eq!(run(&mut world, MacroKind::GoFrontier), Ok(MacroAbort::Done), "it faced it");
    assert!(
        world.targets.blocked(world.map, TargetKey::Tile(Tile::new(3, 3))),
        "and the tile it could not stand on is excluded for the window"
    );
    assert!(!super::palette::frontier_aims(&mut world).contains(&(Tile::new(3, 3), Facing::Right)));
}

#[test]
fn a_tile_a_sprite_is_standing_on_is_not_new_ground() {
    // Row 32 of `infra/docs/macros-traps.md`. The collision table knows nothing about people or
    // objects -- that is what the executor's per-step "did the player move" check is for -- so a
    // villager standing in the open reads walkable underneath. The fly can never stand on that
    // tile while the villager is there, so it can never enter the visited ledger either: it is a
    // frontier for ever, and the arrival press faces it, reports `Done` and changes nothing. The
    // blocked ledger bounds that at one walk per window (row 5); leaving the tile out of the list
    // is what lets the frontier *exhaust*, which is what the button leaving the pad needs.
    let mut world = World::room();
    for y in 0..8 {
        for x in 0..8 {
            world.stood.insert(Tile::new(x, y));
        }
    }
    world.stood.remove(&Tile::new(4, 3));
    world.player = Tile::new(3, 3);
    assert!(
        super::palette::frontier_aims(&mut world).contains(&(Tile::new(3, 3), Facing::Right)),
        "one square of new ground, and the tile the fly is standing on borders it"
    );
    assert!(precondition(MacroKind::GoFrontier, &mut world));

    // A person walks onto the one square of new ground.
    world.npcs = vec![Npc { slot: 1, picture: 0x0d, x: 4, y: 3, facing: Facing::Down }];
    assert!(
        super::palette::frontier_aims(&mut world).is_empty(),
        "the ground under a person is not ground: {:?}",
        super::palette::frontier_aims(&mut world)
    );
    assert!(
        !precondition(MacroKind::GoFrontier, &mut world),
        "and a map with no frontier leaves the button off the pad"
    );

    // The same for the tile to *stand on*: a sprite on it is not somewhere to walk to.
    world.npcs = vec![Npc { slot: 1, picture: 0x0d, x: 3, y: 3, facing: Facing::Down }];
    world.player = Tile::new(6, 6);
    let aims = super::palette::frontier_aims(&mut world);
    assert!(!aims.contains(&(Tile::new(3, 3), Facing::Right)), "{aims:?}");
    assert!(aims.contains(&(Tile::new(4, 2), Facing::Down)), "the other three sides remain: {aims:?}");
}

#[test]
fn a_door_into_the_objectives_own_map_is_toward_it_with_no_route_in_the_graph() {
    // Tier 2 of `ways`, for a map the geography table has no row for. Every destination visited,
    // so nothing is fresh; `geography::next_hop` answers `None`, because an unknown map has no
    // known neighbours; and the door that plainly leads to the objective's map is still the one
    // toward it. Without this a way out whose whole destination list is visited had no candidate
    // of any tier on an outdoor map and the button left the pad (`GO ROUTE` has no tier 3).
    let mut world = World::room();
    // An outdoor map id the table has no row for, so `Way::Route` is what its warps classify as.
    world.map = 0x0b;
    world.warps = vec![
        Warp { x: 2, y: 7, destination_warp: 0, destination_map: 0x05 },
        Warp { x: 5, y: 7, destination_warp: 0, destination_map: 0x06 },
    ];
    world.seen_maps.extend([0x05, 0x06]);
    world.objective = Some(Objective {
        map: 0x05,
        tile: None,
        warp: None,
        edge: None,
        target: None,
    });
    let toward = ways(&mut world, Way::Route);
    assert_eq!(
        toward.iter().map(|exit| exit.id).collect::<Vec<_>>(),
        vec![ExitId::Warp(0)],
        "the door into the objective's map, and only that one: {toward:?}"
    );
    // And with no objective at all there is no tier 2 and no tier 3 for a route.
    world.objective = None;
    assert!(ways(&mut world, Way::Route).is_empty());
}

#[test]
fn a_no_route_refusal_records_what_it_could_not_reach() {
    // The refusal that recorded nothing: the palette's preconditions are the cheap question ("is
    // there an object on this map") and the route search is the real one, so a bound macro could
    // refuse `no route` once per hold for ever while its candidate list never changed.
    let mut world = World::room().wall(&[(2, 3), (4, 3), (3, 2), (3, 4)]);
    world.map = 0x00;
    world.facing = Facing::Up;
    world.npcs = vec![Npc { slot: 2, picture: BALL_SPRITE, x: 6, y: 6, facing: Facing::Down }];

    assert!(on_the_pad(&mut world, MacroKind::GoItem), "the map has an object");
    let slot = pick(&mut world, MacroKind::GoItem).1;
    assert_eq!(
        run(&mut world, MacroKind::GoItem),
        Err(MacroRefused { slot, reason: Refusal::NoRoute })
    );
    assert!(
        world.targets.blocked(world.map, TargetKey::Thing(TalkTarget::Sprite(2))),
        "the thing the sealed-in fly could not reach is excluded"
    );
    assert!(!on_the_pad(&mut world, MacroKind::GoItem), "so the button leaves the pad");
}

#[test]
fn the_overworld_plan_never_truncates_the_frontier_away() {
    // Eleven buttons and six rows: the entry that used to be cut was the last one, which is the
    // fallback that always has somewhere to go while any ground is unexplored. `MENU` was the
    // other, and section 12.11 took it off the row rather than rescuing it again.
    let mut world = populated();
    world.objective = Some(Objective { map: 0x00, tile: None, warp: None, edge: None, target: None });
    world.signs = vec![Sign { x: 6, y: 6, text_id: 3 }];
    let rows = plan(&mut world);
    assert!(rows.len() >= 6, "a full overworld pad is more than five buttons: {rows:?}");
    assert_eq!(
        rows.last(),
        Some(&"GO FRONTIER"),
        "the explorer is dealt last and is never cut: {rows:?}"
    );
}

/// Section 12.11: `MENU` is on no scene's pad, in any state of any scene.
///
/// **What was live** (2026-09-22, rung 10, thirty minutes inside the Pewter museum's upper floor):
/// macro starts `MENU` 82, `BACK` 82, `GO FRONTIER` 8, the event log alternating `MENU
/// start/done, BACK start/done`. `MENU` pressed START, the start menu opened, its pad is `CLOSE`,
/// `CONFIRM` and `BACK`, and `BACK` pressed B and closed it again -- two buttons that undo each
/// other with nothing else changing, which is section 12.10's rule one scene wider than a battle.
///
/// `MENU` was there as the overworld's *unconditional* button, the one that made an empty pad
/// impossible. It is also section 12.2's trap by definition -- a precondition satisfied wherever
/// the fly stands, and a macro that completes without moving -- and nothing in the vocabulary uses
/// the start menu for anything, so there is nothing behind it worth pressing it for. What keeps the
/// pad from being empty instead is the never-empty way out of [`ways`](super::palette::ways).
#[test]
fn menu_is_on_no_scenes_pad() {
    let mut world = populated();
    for scene in [
        Scene::Overworld,
        Scene::Dialog,
        Scene::Unknown,
        Scene::Menu,
        Scene::Shop,
        Scene::Pc,
        Scene::Title,
        Scene::Battle { own_turn: true, forced_switch: false },
        Scene::Battle { own_turn: false, forced_switch: false },
        Scene::Battle { own_turn: false, forced_switch: true },
    ] {
        world.scene = scene;
        world.battle = matches!(scene, Scene::Battle { .. })
            .then_some((BattleKind::Wild, true, false));
        assert!(
            !super::palette::scene_set(scene, &mut world).contains(&MacroKind::Menu),
            "{} deals MENU",
            scene.label()
        );
        assert!(!plan(&mut world).contains(&"MENU"), "{} binds MENU", scene.label());
    }
    // The start menu is still reachable and still has its own pad: the fly's raw START reaches the
    // cartridge in macros mode (section 13.1), and what is on that pad is what leaves it.
    world.scene = Scene::Menu;
    world.list = List::Start;
    assert_eq!(pad_of(&mut world), ["CLOSE", "CONFIRM", "BACK"]);
}

#[test]
fn no_playable_scene_deals_an_empty_pad() {
    // The battle-text deadlock of v0.2.4 was one scene with nothing on the pad. This is the sweep
    // for the rest of them, in each scene's *worst* state: nothing to attack with, nobody to switch
    // to, no potion, a trainer battle, no money and an empty bag.
    let mut worst = World::battle();
    worst.battle = Some((BattleKind::Trainer, true, false));
    worst.mons = vec![mon(0, 4, 20, &[(33, 0)])];
    worst.bag = Vec::new();
    worst.money = 0;
    assert!(!move_slot_bound(&mut worst, MacroKind::Move2), "no second move");
    assert!(healthiest_other(&mut worst).is_none());
    assert!(plan::plan_for(worst.scene(), &mut worst).bound() > 0, "own turn, nothing to do");

    let mut forced = World::battle();
    forced.scene = Scene::Battle { own_turn: false, forced_switch: true };
    forced.battle = Some((BattleKind::Wild, false, true));
    forced.mons = vec![mon(0, 4, 20, &[(33, 30)])];
    assert!(healthiest_other(&mut forced).is_none(), "nothing healthy to switch to");
    assert!(plan::plan_for(forced.scene(), &mut forced).bound() > 0);

    for scene in [
        Scene::Dialog,
        Scene::Unknown,
        Scene::Menu,
        Scene::Shop,
        Scene::Pc,
        Scene::Battle { own_turn: false, forced_switch: false },
    ] {
        let mut world = World::room();
        world.scene = scene;
        // `Unknown` is dealt on what is drawn (section 12.13): a screen with words on it has
        // `NEXT` and `BACK`, and a scripted overworld frame is the one deliberate empty pad,
        // which `an_unknown_frame_with_no_box_on_it_deals_nothing` is about.
        world.box_open = scene == Scene::Unknown;
        world.battle = matches!(scene, Scene::Battle { .. })
            .then_some((BattleKind::Wild, false, false));
        assert!(
            plan::plan_for(scene, &mut world).bound() > 0,
            "{} deals nothing at all",
            scene.label()
        );
    }
    // The title screen is the one exception, and it is not a trap: the scene is not playable, so
    // the sim loop hands the cartridge the fly's raw buttons there (`flysim::macros::decide`).
    let mut title = World::room();
    title.scene = Scene::Title;
    assert_eq!(plan::plan_for(Scene::Title, &mut title).bound(), 0);
}

#[test]
fn the_badges_rung_resolves_to_the_person_standing_in_the_gym() {
    // Rung 11 is BOULDER BADGE and `docs/design/ladder.md` gives its place as a *person* in the
    // Pewter gym (12.5's `PlaceKind`). What the rung-10 loop needed was the two halves of that
    // working together: the road into map `0x36` (`geography`, the museum rows and the gym's
    // own), and, once inside, the objective naming somebody to walk to rather than a map to be
    // on. This is the second half, on the map the rung is earned on.
    let mut world = World::room();
    world.map = maps::PEWTER_GYM;
    // Both of Pewter's errands discharged. Section 13 puts an unvisited mart or centre *ahead*
    // of the rung's place, and the gym is inside Pewter's area like everything else in the town,
    // so until they are paid the objective is a building and not the leader. That is the errand
    // working, and it is why the ROM run below spends its first minutes in the town's shops.
    world.areas.insert((Amenity::Mart, maps::PEWTER_CITY));
    world.areas.insert((Amenity::Center, maps::PEWTER_CITY));
    world.npcs = vec![Npc { slot: 1, picture: 0x05, x: 4, y: 2, facing: Facing::Down }];
    world.objective = Some(Objective {
        map: maps::PEWTER_GYM,
        tile: None,
        warp: None,
        edge: None,
        target: Some(PlaceKind::Person),
    });
    let targets = super::palette::objective_targets(&mut world);
    assert_eq!(targets.len(), 1, "the person the rung names: {targets:?}");
    assert_eq!(targets[0].0, Tile::new(4, 2));
    // And the walk aims at standing beside them and turning to face them, which is `GO NPC`'s own
    // arrival and all `GO OBJECTIVE` ever promises: the press is `TALK`'s and the fly's.
    let goals: Vec<Tile> = objective_goals(&mut world).into_iter().map(|aim| aim.tile).collect();
    assert!(goals.contains(&Tile::new(4, 3)), "a tile beside them: {goals:?}");
    assert!(goals.iter().all(|tile| tile.distance(Tile::new(4, 2)) == 1), "{goals:?}");
    // The room the rung is in is not left while the thing that earns it is standing in it (12.5).
    assert!(ways(&mut world, Way::Exit).is_empty(), "the gym's door is not a candidate yet");
    // Talked to, and the objective has nothing left here: the button leaves the pad and the ways
    // out come back, which is what carries the run on to the next rung.
    world.talked.insert(targets[0].1);
    assert!(super::palette::objective_targets(&mut world).is_empty());
    assert!(objective_goals(&mut world).is_empty());
}

#[test]
fn a_frontier_no_walk_can_reach_takes_go_frontier_off_the_pad_and_keeps_it_off() {
    // Section 12.14, the rung-10 museum. The sealed pocket below is map `0x34` in miniature: the
    // unstood ground is real and it is fenced off, so `GO FRONTIER` refuses `no route` and writes
    // every tile it could not reach to the blocked ledger -- which is a *window*. Before this the
    // window lapsed after ten brain minutes and all of it was a candidate again: 1,235 starts in
    // 47 minutes over two museum floors and a town. The mark has no window.
    let mut world = World::room();
    for y in 0..8 {
        for x in 0..8 {
            world.stood.insert(Tile::new(x, y));
        }
    }
    world.stood.remove(&Tile::new(7, 7));
    world.walls.insert(Tile::new(6, 7));
    world.walls.insert(Tile::new(6, 6));
    world.walls.insert(Tile::new(7, 5));
    world.player = Tile::new(0, 0);
    assert!(!super::palette::frontier_aims(&mut world).is_empty(), "the pocket is a frontier");
    assert!(precondition(MacroKind::GoFrontier, &mut world), "so the button is on the pad");

    let refused = run(&mut world, MacroKind::GoFrontier).expect_err("the pocket is sealed");
    assert_eq!(refused.reason, Refusal::NoRoute);
    assert!(world.exhausted.contains(&world.map), "the map is marked: {:?}", world.exhausted);
    assert!(super::palette::frontier_aims(&mut world).is_empty(), "nothing left to aim at");
    assert!(!precondition(MacroKind::GoFrontier, &mut world), "and the button is off the pad");

    // Ten brain minutes later the blocked window has lapsed and every tile of the pocket is a
    // candidate again -- and the button is still off the pad, because the ground has not moved.
    world.targets.clock(11.0 * 60_000.0);
    assert!(!world.targets.blocked(world.map, TargetKey::Tile(Tile::new(7, 6))));
    assert!(
        super::palette::frontier_aims(&mut world).is_empty(),
        "the mark is not a window"
    );
    // A map the fly walks to instead is untouched: the mark is one map's.
    world.map = 0x35;
    assert!(!super::palette::frontier_aims(&mut world).is_empty(), "another map is its own");
}

#[test]
fn a_frontier_mark_is_the_stood_ledgers_to_clear() {
    // The two halves of the rule as the types have them: the stood ledger answers "this is
    // ground the run had not stood on", which is the only event that can change which tiles the
    // fly can reach, and that answer is what clears the map's mark.
    let mut stood = super::cartridge::Stood::default();
    assert!(stood.record(2, Tile::new(4, 4)), "the first time is new ground");
    assert!(!stood.record(2, Tile::new(4, 4)), "the second time is not");
    assert!(stood.record(0x34, Tile::new(4, 4)), "and a tile is a tile of one map");

    use super::cartridge::FrontierLedger;
    let mut frontiers = super::cartridge::Frontiers::default();
    assert!(!frontiers.frontier_exhausted(0x34));
    frontiers.record(0x34);
    frontiers.record(0x34);
    assert!(frontiers.frontier_exhausted(0x34), "idempotent");
    assert!(!frontiers.frontier_exhausted(0x35), "and one map's");
    assert_eq!(frontiers.len(), 1);
    frontiers.clear(0x34);
    assert!(!frontiers.frontier_exhausted(0x34));
    assert!(frontiers.is_empty());
}

#[test]
fn a_walk_that_could_only_get_closer_now_excludes_what_it_cannot_reach() {
    // Two readings of the same pocket, in the order they were measured
    // (`infra/docs/macros-traps.md`). `GO FRONTIER`'s goals are one key each, so "the key every
    // goal shares" answers `None` for it, and a closest-approach route -- the search's answer for a
    // goal it cannot reach -- carried no target at all: the macro spent the whole frame cap
    // wandering, recorded nothing, and was chosen again on the next hold for ever. Naming the
    // nearest goal fixed the recording and left the wandering, which became the 2026-09-17 stall:
    // a closest-approach walk that trades two tiles for a whole cap. A goal the search cannot reach
    // is now excluded at `start` instead of walked toward, and the key it is excluded under is
    // still the one the walk set out for.
    let mut world = World::room();
    for y in 0..8 {
        for x in 0..8 {
            world.stood.insert(Tile::new(x, y));
        }
    }
    // One square of new ground in a sealed pocket: the tile to stand on is walkable and the new
    // ground beside it is walkable, so it is a frontier, and neither can be reached from the room.
    world.stood.remove(&Tile::new(7, 7));
    world.walls.insert(Tile::new(6, 7));
    world.walls.insert(Tile::new(6, 6));
    world.walls.insert(Tile::new(7, 5));
    world.player = Tile::new(0, 0);
    let aims = super::palette::frontier_aims(&mut world);
    assert!(!aims.is_empty(), "the tiles beside the new ground are frontier: {aims:?}");

    let refused = run(&mut world, MacroKind::GoFrontier).expect_err("the pocket is sealed");
    assert_eq!(refused.reason, Refusal::NoRoute);
    assert!(world.pulses.is_empty(), "nothing was pressed: {:?}", world.pulses);
    // The ledger moved, so the next hold cannot be the same walk.
    assert_ne!(
        (world.targets.len(), super::palette::frontier_aims(&mut world).len()),
        ((0, 0), aims.len()),
        "a walk that got nowhere left a mark on the ledger"
    );
}

// ---------------------------------------------------------------------------------------------
// Section 12.4: the gate
// ---------------------------------------------------------------------------------------------

#[test]
fn a_conversation_the_game_ends_by_moving_the_fly_is_not_talked_to() {
    // The Viridian gate (2026-09-17, `infra/docs/macros-traps.md` row 28). A still sprite in the
    // road north, a text box reading "This is private property!", and a script that takes the
    // joypad and walks the fly back one tile. `TALK` finished, the ledger was written, and the one
    // conversation that might have changed something was off the pad for the rest of the session.
    let mut world = World::room().at(3, 3);
    world.facing = Facing::Down;
    world.npcs = vec![Npc { slot: 4, picture: BALL_SPRITE, x: 3, y: 4, facing: Facing::Up }];
    let mut machine = MacroMachine::new(1);
    let (palette, slot) = pick(&mut world, MacroKind::Talk);
    machine.start(&palette, slot, &mut world).expect("TALK is bound at the thing ahead");
    while machine.step(&mut world).is_some() {
        world.frame(buttons::NONE);
    }
    assert_eq!(machine.take_talked(), None, "nothing is written while the box is open");

    // The cartridge takes the joypad: the conversation ended by refusing.
    world.scripted = true;
    machine.observe_frame(&mut world);
    world.scripted = false;
    machine.observe_frame(&mut world);
    assert_eq!(machine.take_talked(), None, "a push-back is not a conversation had");
    // So it is still something to press A at, and -- once the fly is not standing in front of it,
    // which is the state `TALK` owns (row 4) -- still something to walk to.
    assert!(on_the_pad(&mut world, MacroKind::Talk));
    world.player = Tile::new(3, 1);
    assert!(
        !untalked_objects(&mut world).is_empty(),
        "the thing the conversation refused is still on offer"
    );
}

#[test]
fn a_conversation_that_walks_the_fly_off_its_tile_is_not_talked_to() {
    // The same fact without the flag: a script can finish its push-back on the frame the box
    // closes, so where the fly is standing is the other half of the question.
    let mut world = World::room().at(3, 3);
    world.facing = Facing::Down;
    world.npcs = vec![Npc { slot: 4, picture: 1, x: 3, y: 4, facing: Facing::Up }];
    let mut machine = MacroMachine::new(1);
    let (palette, slot) = pick(&mut world, MacroKind::Talk);
    machine.start(&palette, slot, &mut world).expect("TALK is bound at a person");
    while machine.step(&mut world).is_some() {
        world.frame(buttons::NONE);
    }
    world.player = Tile::new(3, 2);
    machine.observe_frame(&mut world);
    assert_eq!(machine.take_talked(), None, "the fly is not where it pressed from");
}

#[test]
fn a_fly_that_declines_an_offer_has_not_talked_to_anything() {
    // The catching tutorial's own shape: a yes/no box, and `NO` is a real answer the pad has to be
    // able to give. What it must not do is retire the thing that asked -- the offer stands.
    //
    // Since row 56 the box has to be a **readable prompt** for that to be the reading: a `NO` on a
    // plain text box declines nothing, and the test below is the other half.
    let mut world = World::room().at(3, 3);
    world.facing = Facing::Down;
    world.npcs = vec![Npc { slot: 4, picture: 1, x: 3, y: 4, facing: Facing::Up }];
    let mut machine = MacroMachine::new(1);
    let (palette, slot) = pick(&mut world, MacroKind::Talk);
    machine.start(&palette, slot, &mut world).expect("TALK is bound at a person");
    while machine.step(&mut world).is_some() {
        world.frame(buttons::NONE);
    }
    // The box is open and it is the choice, so the scene is a dialog and the pad is its answers.
    world.scene = Scene::Dialog;
    world.prompt = true;
    let (dialog, no) = pick(&mut world, MacroKind::No);
    assert_eq!(names(&dialog), ["YES", "NO"], "a readable choice deals its own answers (12.12)");
    machine.start(&dialog, no, &mut world).expect("NO is bound in a dialog");
    while machine.step(&mut world).is_some() {
        world.frame(buttons::NONE);
    }
    world.scene = Scene::Overworld;
    machine.observe_frame(&mut world);
    assert_eq!(machine.take_talked(), None, "a declined offer is not a conversation had");
    assert!(on_the_pad(&mut world, MacroKind::Talk), "the offer stands");
}

#[test]
fn a_no_pressed_inside_a_conversation_is_not_a_declined_offer() {
    // Row 56, the Pewter Gym guide. His conversation is fifty-two boxes long and `NO`'s B advances
    // a plain one exactly as `NEXT`'s A does -- it declines nothing. Clearing the pending `TALK` on
    // it meant the talked ledger never learned the conversation had happened, so `TALK` was on the
    // overworld pad every hold, and an A press at him reopened the whole ring: thirty brain
    // minutes of scene `dialog` with no walk macro dealt at all.
    //
    // Which of the two a `NO` was is decided where it can be seen: by whether the box closes on it.
    let mut world = World::room().at(3, 3);
    world.facing = Facing::Down;
    world.npcs = vec![Npc { slot: 4, picture: 1, x: 3, y: 4, facing: Facing::Up }];
    let mut machine = MacroMachine::new(1);
    let (palette, slot) = pick(&mut world, MacroKind::Talk);
    machine.start(&palette, slot, &mut world).expect("TALK is bound at a person");
    while machine.step(&mut world).is_some() {
        world.frame(buttons::NONE);
    }

    // Deep inside the conversation: a plain text box, not a choice.
    world.scene = Scene::Dialog;
    world.prompt = false;
    let (dialog, no) = pick(&mut world, MacroKind::No);
    assert!(names(&dialog).contains(&"NEXT"), "a plain box deals all three: {:?}", names(&dialog));
    machine.start(&dialog, no, &mut world).expect("NO is bound in a dialog");
    while machine.step(&mut world).is_some() {
        world.frame(buttons::NONE);
    }

    // The box is still open, so nothing is decided yet -- the conversation is still running.
    machine.observe_frame(&mut world);
    assert_eq!(machine.take_talked(), None, "the box is still open");

    // And when the text is gone the conversation counts, which is what shuts the ring's door.
    world.scene = Scene::Overworld;
    machine.observe_frame(&mut world);
    assert_eq!(
        machine.take_talked(),
        Some((world.map, TalkTarget::Sprite(4))),
        "a conversation walked through to its end is a conversation had"
    );
    world.talked.insert(TalkTarget::Sprite(4));
    assert!(
        !on_the_pad(&mut world, MacroKind::Talk),
        "`TALK` is the ring's door and this run has been through it"
    );
}

#[test]
fn a_walk_the_cartridge_pushes_back_excludes_what_it_was_walking_to() {
    // Row 28's other half. Every macro that walked into the gate ended `Done` -- a scene change,
    // which is "the world moved on under it" and writes nothing -- so `GO OBJECTIVE` and
    // `GO ROUTE` aimed at the same closed road once per hold for eight hours. A scene change that
    // is the cartridge *moving the fly* is the target's own fact.
    let mut world = World::room().at(3, 3);
    world.map = 0x00;
    world.connections = Connections { north: true, south: false, east: false, west: false };
    // The gate: the scene changes to a dialog four frames in, with the game driving the player.
    world.switch = Some((4, Scene::Dialog));
    world.scripted_at = Some(4);
    let north = TargetKey::Exit(ExitId::Edge(Edge::North));

    assert!(on_the_pad(&mut world, MacroKind::GoRoute));
    assert_eq!(run(&mut world, MacroKind::GoRoute), Ok(MacroAbort::Done));
    assert!(
        world.targets.blocked(world.map, north),
        "the road the cartridge refused is excluded for the window"
    );
    assert!(!on_the_pad(&mut world, MacroKind::GoRoute), "so the button leaves the pad");

    // And an ordinary scene change -- a wild battle starting mid-walk -- still records nothing.
    let mut wild = World::room().at(3, 3);
    wild.map = 0x00;
    wild.connections = Connections { north: true, south: false, east: false, west: false };
    wild.switch = Some((4, Scene::Battle { own_turn: true, forced_switch: false }));
    assert_eq!(run(&mut wild, MacroKind::GoRoute), Ok(MacroAbort::Done));
    assert!(
        !wild.targets.blocked(wild.map, north),
        "a battle is the world moving on, not the road refusing"
    );
}

// ---------------------------------------------------------------------------------------------
// Section 12.5: the objective names a target, not just a map
// ---------------------------------------------------------------------------------------------

#[test]
fn an_objective_that_names_a_person_is_walked_to_and_faced() {
    // Row 29 (`infra/docs/macros-traps.md`). A rung earned by a conversation is not a map: with the
    // place only naming Oak's lab, `GO OBJECTIVE` arrived on the lab's own doormat and called that
    // arriving, and `GO OUT` walked straight back out -- live, every three brain seconds, for most
    // of a morning. The catalog names the person now, and the arrival is `GO NPC`'s: beside it,
    // facing it, so `TALK` is what the next hold is offered.
    let mut world = World::room().at(1, 1);
    world.map = 0x00;
    world.npcs = vec![Npc { slot: 3, picture: 1, x: 5, y: 5, facing: Facing::Down }];
    world.objective = Some(Objective {
        map: world.map,
        tile: None,
        warp: None,
        edge: None,
        target: Some(PlaceKind::Person),
    });

    assert!(on_the_pad(&mut world, MacroKind::GoObjective));
    assert_eq!(run(&mut world, MacroKind::GoObjective), Ok(MacroAbort::Done));
    assert_eq!(world.player.distance(Tile::new(5, 5)), 1, "beside him: {:?}", world.player);
    let ahead = world.player.step(world.facing);
    assert_eq!(ahead, Some(Tile::new(5, 5)), "and facing him: {:?} {:?}", world.player, world.facing);

    // Which is exactly `TALK`'s own precondition, so the press is what the pad now offers -- and
    // `GO OBJECTIVE` has nothing left to walk to.
    assert!(on_the_pad(&mut world, MacroKind::Talk));
    assert!(!on_the_pad(&mut world, MacroKind::GoObjective), "the walk is done; the press is not");
}

#[test]
fn the_ways_out_leave_the_pad_while_the_objectives_own_thing_is_here() {
    // The second half of row 29's fix, and the reason the first rollback stalled: suppressing the
    // way out is only safe when something else leads to the thing. Knowledge inside the macro --
    // a way out is not a candidate while the ladder's target is standing in the room.
    let mut world = World::ground_floor().at(3, 3);
    world.npcs = vec![Npc { slot: 3, picture: 1, x: 5, y: 2, facing: Facing::Down }];
    world.objective = Some(Objective {
        map: world.map,
        tile: None,
        warp: None,
        edge: None,
        target: Some(PlaceKind::Person),
    });
    assert!(!on_the_pad(&mut world, MacroKind::GoOut), "the room the rung is in is not left");
    assert!(!on_the_pad(&mut world, MacroKind::GoWarp));
    assert!(on_the_pad(&mut world, MacroKind::GoObjective), "and something leads to him");

    // The conversation had: the thing is off the objective's list, so the ways out come back.
    world.talked.insert(TalkTarget::Sprite(3));
    assert!(on_the_pad(&mut world, MacroKind::GoOut), "with nothing left here, the door is back");

    // And so does an unreachable one, through the blocked ledger -- which is what keeps the room
    // from becoming a trap when the target is walled off.
    let mut walled = World::ground_floor().at(3, 3);
    walled.npcs = vec![Npc { slot: 3, picture: 1, x: 5, y: 2, facing: Facing::Down }];
    walled.objective = Some(Objective {
        map: walled.map,
        tile: None,
        warp: None,
        edge: None,
        target: Some(PlaceKind::Person),
    });
    assert!(!on_the_pad(&mut walled, MacroKind::GoOut));
    walled.targets.record_blocked(walled.map, TargetKey::Thing(TalkTarget::Sprite(3)));
    assert!(on_the_pad(&mut walled, MacroKind::GoOut), "excluded, so the way out is a candidate");

    // A place that is only a map keeps today's behaviour exactly.
    let mut plain = World::ground_floor().at(3, 3);
    plain.npcs = vec![Npc { slot: 3, picture: 1, x: 5, y: 2, facing: Facing::Down }];
    plain.objective =
        Some(Objective { map: plain.map, tile: None, warp: None, edge: None, target: None });
    assert!(on_the_pad(&mut plain, MacroKind::GoOut));
}

// ---------------------------------------------------------------------------------------------
// Section 12.6: the battle's own sub-states
// ---------------------------------------------------------------------------------------------

#[test]
fn each_battle_menu_deals_its_own_pad() {
    // Live 2026-09-17, an hour and forty-one minutes in Viridian Forest: the move list was open
    // and the pad was one `NEXT`, because `own_turn` was the top-level menu alone and the
    // between-turns pad is an A press on whatever the cursor is sitting on. The cursor sat on
    // TACKLE, TACKLE had 0 of 40 PP, the game said so, the box closed, the list came back.
    //
    // A list that is accepting input is a choice the game is waiting for, and there are exactly two
    // answers to a list: choose from it, or back out of it. `NEXT` is on neither.
    let mut main = World::battle();
    let pad = names(&plan::plan_for(main.scene(), &mut main));
    assert!(pad.contains(&"MOVE 1"), "{pad:?}");
    assert!(pad.contains(&"SWITCH"), "{pad:?}");
    // Section 12.10: the top-level menu is a list accepting input too, so `NEXT` is off it as
    // well. Its backstop is `MOVE 1`, which is bound here whatever the battler reads as, because
    // FIGHT always opens (12.8) -- and unlike `NEXT` it ends the turn instead of opening the list
    // that `BACK` closes again.
    assert!(!pad.contains(&"NEXT"), "never NEXT on a menu accepting input: {pad:?}");
    // `ITEM` and `RUN` have their own preconditions -- a potion, and a party with nothing healthy
    // left -- and this fixture satisfies neither; row 7 covers the turn where all four drop.

    let mut moves = World::battle();
    moves.list = List::Moves(3);
    moves.cursor = 0;
    let pad = names(&plan::plan_for(moves.scene(), &mut moves));
    assert!(pad.contains(&"MOVE 1"), "{pad:?}");
    assert!(pad.contains(&"BACK"), "{pad:?}");
    assert!(!pad.contains(&"NEXT"), "never NEXT over an open list: {pad:?}");
    assert!(!pad.contains(&"SWITCH"), "the move list is not where a switch is chosen: {pad:?}");
    assert!(!pad.contains(&"RUN"), "{pad:?}");

    let mut party = World::battle();
    party.list = List::BattleParty;
    party.cursor = 1;
    let pad = names(&plan::plan_for(party.scene(), &mut party));
    assert!(pad.contains(&"SWITCH"), "{pad:?}");
    assert!(pad.contains(&"BACK"), "{pad:?}");
    assert!(!pad.contains(&"NEXT"), "{pad:?}");
    assert!(!pad.contains(&"ATTACK"), "{pad:?}");

    // A forced switch is its own scene and its own pad, and it keeps the `NEXT` that stops a
    // forced switch with nothing to switch to from dealing an empty pad (row 8).
    let mut forced = World::battle();
    forced.scene = Scene::Battle { own_turn: false, forced_switch: true };
    forced.battle = Some((BattleKind::Wild, false, true));
    forced.list = List::BattleParty;
    let pad = names(&plan::plan_for(forced.scene(), &mut forced));
    assert!(pad.contains(&"SWITCH"), "{pad:?}");
    assert!(pad.contains(&"NEXT"), "{pad:?}");
    assert!(!pad.contains(&"BACK"), "a forced switch cannot be backed out of: {pad:?}");

    // And a battle frame with no menu up is still the between-turns press, which is the 2026-09-16
    // deadlock fix and is untouched.
    let mut between = World::battle();
    between.scene = Scene::Battle { own_turn: false, forced_switch: false };
    between.battle = Some((BattleKind::Wild, false, false));
    between.list = List::None;
    let pad = names(&plan::plan_for(between.scene(), &mut between));
    assert_eq!(pad.iter().filter(|name| **name == "NEXT").count(), 1, "{pad:?}");
    assert!(!pad.contains(&"BACK"), "nothing is open to back out of: {pad:?}");
}

/// Section 12.9: `BACK` is on a battle's pad only where a list is open.
///
/// **Live on rung 9**, 69 hours in Viridian Forest: `BACK` was 135 of 183 macro starts since the
/// restart and the event log repeated `RUN blocked, BACK start, BACK done`. Between turns there is
/// no list to leave, so the B press changes nothing the `NEXT` beside it does not and the macro
/// completes on the tile it started on -- section 12.2's trap, on the pad the fly spends most of a
/// wild battle looking at.
///
/// The three lists keep it, because backing out of a list is one of exactly two answers to one.
#[test]
fn back_is_on_a_battle_pad_only_where_a_list_is_open() {
    let with_back = |world: &mut World| {
        let pad = names(&plan::plan_for(world.scene(), world));
        assert!(pad.contains(&"BACK"), "a list can be left: {pad:?}");
    };
    let without = |world: &mut World| {
        let pad = names(&plan::plan_for(world.scene(), world));
        assert!(!pad.contains(&"BACK"), "nothing to back out of: {pad:?}");
        assert!(plan::plan_for(world.scene(), world).bound() > 0, "and never an empty pad");
    };

    // The top-level menu: FIGHT, PKMN, ITEM and RUN are the four answers and B is not a fifth.
    let mut main = World::battle();
    without(&mut main);

    // The three lists.
    let mut moves = World::battle();
    moves.list = List::Moves(3);
    with_back(&mut moves);
    let mut party = World::battle();
    party.list = List::BattleParty;
    with_back(&mut party);
    // The bag, which is the fly's own turn since section 12.10 because its cursor accepts input.
    let mut bag = World::battle();
    bag.list = List::BattleBag;
    with_back(&mut bag);

    // Text, an animation, a turn resolving: no list, no `BACK`.
    let mut between = World::battle();
    between.scene = Scene::Battle { own_turn: false, forced_switch: false };
    between.battle = Some((BattleKind::Wild, false, false));
    between.list = List::None;
    without(&mut between);

    // And a forced switch, which could never be backed out of anyway (row 8).
    let mut forced = World::battle();
    forced.scene = Scene::Battle { own_turn: false, forced_switch: true };
    forced.battle = Some((BattleKind::Wild, false, true));
    forced.list = List::BattleParty;
    without(&mut forced);
}

/// Section 12.10: **no battle pad deals a pair of buttons that undo each other.**
///
/// Live on rung 9 after v0.4.2, 71 hours in Viridian Forest: `NEXT` 1264 macro starts, `BACK`
/// 1241, and the event log alternating `NEXT start/done, BACK start/done` every hold on map 51.
/// The pair was split across two sub-states of one turn -- `NEXT` on the top-level menu was an A
/// press on FIGHT, which opened the move list, and `BACK` on the move list closed it again -- so
/// neither the pad rule of 12.9 nor the "no `BACK` without a list" rule caught it: both buttons
/// were legitimate where they stood, and between them they were a 2-cycle that never spent a turn.
///
/// The rule that closes it is about the pair rather than about either button: `NEXT` is the A that
/// advances **text**, so it belongs only on a frame with no cursor accepting input, and `BACK` is
/// the B that leaves a **list**, so it belongs only on a frame that has one. The two conditions are
/// exclusive, so no pad can hold both -- and the forced switch, which keeps `NEXT` as row 8's
/// backstop, is the one arm with a cursor and no `BACK` at all, because it cannot be cancelled.
#[test]
fn no_battle_pad_holds_both_next_and_back() {
    // Every battle sub-state the seam can report, on a turn where every precondition is satisfied
    // (a potion, a ball, a hurt Pokémon, a bench) and on one where none is.
    let sub_states = [
        (Scene::Battle { own_turn: true, forced_switch: false }, true, false, List::BattleMain),
        (Scene::Battle { own_turn: true, forced_switch: false }, true, false, List::Moves(3)),
        (Scene::Battle { own_turn: true, forced_switch: false }, true, false, List::BattleParty),
        (Scene::Battle { own_turn: true, forced_switch: false }, true, false, List::BattleBag),
        (Scene::Battle { own_turn: false, forced_switch: false }, false, false, List::None),
        (Scene::Battle { own_turn: false, forced_switch: true }, false, true, List::BattleParty),
    ];
    for stocked in [false, true] {
        for (scene, own_turn, forced, list) in sub_states {
            let mut world = World::battle();
            world.scene = scene;
            world.battle = Some((BattleKind::Wild, own_turn, forced));
            world.list = list;
            if stocked {
                world.bag = vec![(item::POTION, 1), (item::POKE_BALL, 3)];
                world.enemy = Some(EnemyMon { species: 0x99, level: 3, hp: 5, max_hp: 11 });
            }
            let pad = names(&plan::plan_for(world.scene(), &mut world));
            let next = pad.contains(&"NEXT");
            let back = pad.contains(&"BACK");
            assert!(
                !(next && back),
                "{list:?} (stocked {stocked}) deals a pair that undoes itself: {pad:?}"
            );
            // And the pair is not the only way to waste a hold: a pad of one button that cannot
            // end the turn is the shape row 34 had, so every sub-state is checked for having one.
            assert!(
                !pad.is_empty(),
                "{list:?} (stocked {stocked}) deals nothing: {pad:?}"
            );
            // `NEXT` is on a frame with no cursor accepting input, or on the forced switch that
            // has no other answer (row 8). Nowhere else.
            if next {
                assert!(
                    forced || list == List::None,
                    "NEXT on a menu accepting input: {list:?} deals {pad:?}"
                );
            }
        }
    }
}

/// Section 12.10: the bag inside a battle is the fly's turn, and its pad is the bag's answers.
///
/// `NEXT` on an open bag is the A press that **uses** whatever the cursor is sitting on, which is
/// not one of the answers to a list, and `CONFIRM` beside `BACK` was the same press by another
/// name. The two scripts that navigate this list by reading its cursor are `ITEM` and
/// `THROW BALL`, and they are what the row deals.
#[test]
fn the_battle_bag_is_the_flys_turn_and_deals_its_own_two_uses() {
    let mut world = World::battle();
    world.list = List::BattleBag;
    world.bag = vec![(item::POTION, 2), (item::POKE_BALL, 4)];
    world.enemy = Some(EnemyMon { species: 0x99, level: 3, hp: 5, max_hp: 11 });
    // `World::battle` is a wild battle with the active Pokémon on 4 of 20, so `ITEM`'s two facts
    // hold and `THROW BALL`'s four do: a wild battle, a ball, room in a party of three, and an
    // enemy species the party does not hold.
    assert_eq!(
        names(&plan::plan_for(world.scene(), &mut world)),
        ["BACK", "ITEM", "THROW BALL"]
    );
    // Nothing in the bag: leaving the list is the press, and there is no `NEXT` to use a thing
    // that is not there.
    world.bag.clear();
    let pad = names(&plan::plan_for(world.scene(), &mut world));
    assert_eq!(pad, ["BACK"]);
}

#[test]
fn a_spent_slot_is_off_the_pad_and_a_slot_with_pp_is_on_it_over_an_open_list() {
    // The live state row 34 came from: slot 0 is TACKLE with 0 of 40 PP and the cursor is on it.
    // Section 14 makes that a *per slot* question -- `MOVE 1` is gone from the pad and `MOVE 2`
    // and `MOVE 3` are on it -- and the script goes straight to the slot rather than through
    // FIGHT.
    let mut world = World::battle();
    world.mons = vec![mon(0, 8, 28, &[(33, 0), (45, 40), (73, 10)])];
    world.active = Some(0);
    world.list = List::Moves(3);
    world.grid = false;
    world.cursor = 0;
    world.cursor_max = 2;

    assert!(!on_the_pad(&mut world, MacroKind::Move1), "slot one is spent");
    assert!(on_the_pad(&mut world, MacroKind::Move2));
    assert!(on_the_pad(&mut world, MacroKind::Move3));
    assert_eq!(run(&mut world, MacroKind::Move3), Ok(MacroAbort::Done));
    assert_eq!(world.cursor, 2, "the cursor moved onto the slot the button names");
    assert!(world.pulses.contains(&buttons::A), "and confirmed it: {:?}", world.pulses);
}

#[test]
fn move_one_confirms_anyway_when_every_move_is_out_of_pp() {
    // Struggle is the cartridge's answer to a Pokémon with no PP anywhere, and the way to it is to
    // choose a move regardless. Refusing would take every move button off the pad on the one turn
    // the fly has nothing else to press -- which is the shape the live loop had.
    let mut world = World::battle();
    world.mons = vec![mon(0, 8, 28, &[(33, 0), (45, 0), (73, 0)])];
    world.active = Some(0);
    world.list = List::Moves(3);
    world.cursor = 1;

    world.grid = false;
    world.cursor_max = 2;
    assert!(on_the_pad(&mut world, MacroKind::Move1), "`MOVE 1` carries Struggle");
    assert!(!on_the_pad(&mut world, MacroKind::Move2), "and it alone");
    assert_eq!(run(&mut world, MacroKind::Move1), Ok(MacroAbort::Done));
    assert_eq!(world.cursor, 1, "the cursor's own slot is the choice");
    assert!(world.pulses.contains(&buttons::A));

    // And from the menu above, with nothing to attack with, `MOVE 1` presses FIGHT and stops
    // there: `CheckPlayerHasUsableMoves` answers it without opening a list, so a second cursor
    // step would press A at text. Row 34 -- this used to refuse, which is what left the turn with
    // `ITEM` over a bag it could only close.
    let mut menu = World::battle();
    menu.mons = vec![mon(0, 8, 28, &[(33, 0)])];
    menu.active = Some(0);
    menu.list = List::BattleMain;
    menu.cursor = battle_entry::RUN;
    assert!(on_the_pad(&mut menu, MacroKind::Move1));
    assert_eq!(run(&mut menu, MacroKind::Move1), Ok(MacroAbort::Done));
    assert_eq!(menu.cursor, battle_entry::FIGHT, "the cursor moved onto FIGHT");
    assert!(menu.pulses.contains(&buttons::A), "and confirmed it: {:?}", menu.pulses);
}

#[test]
fn a_turn_with_nothing_to_attack_switch_or_flee_with_still_has_a_button() {
    // The state measured in Viridian Forest at 99.1 brain minutes (`infra/docs/macros-traps.md`
    // row 34): a wild battle, a party of one, TACKLE / GROWL / LEECH SEED all at 0 PP, a potion in
    // the bag. Before this branch the fly's own turn dealt `ITEM` and `NEXT` and nothing that
    // could end the turn; `NEXT` was an A press on whatever the cursor held, which opened the
    // party list, whose only bound button with one Pokémon is `BACK`.
    let mut world = World::battle();
    world.mons = vec![mon(0, 11, 34, &[(33, 0), (45, 0), (73, 0)])];
    world.active = Some(0);
    world.bag = vec![(item::POTION, 1)];
    world.list = List::BattleMain;

    assert!(!move_slot_bound(&mut world, MacroKind::Move2), "nothing but slot one is offered");
    assert!(healthiest_other(&mut world).is_none(), "a party of one has no bench");

    // Every battle sub-state of that same state. The two lists that *are* a choice deal a button
    // that ends the turn; the party list, whose one entry is the Pokémon already out, has nothing
    // to choose and backing out is the only press -- which now returns to a menu with `MOVE 1` on
    // it, where before this branch it returned to `ITEM` and `NEXT`.
    for list in [List::BattleMain, List::Moves(3)] {
        world.list = list;
        let pad = pad_of(&mut world);
        assert!(pad.contains(&"MOVE 1"), "{list:?} deals {pad:?}");
        assert!(pad.len() > 1, "{list:?} deals one button: {pad:?}");
        assert_ne!(pad, vec!["BACK"], "no battle pad is BACK alone: {list:?}");
    }
    world.list = List::BattleParty;
    assert_eq!(
        pad_of(&mut world),
        vec!["BACK"],
        "the party list with one Pokémon: nothing to switch to, so backing out is the press"
    );
    // And what it backs out to is a turn the fly can end.
    world.list = List::BattleMain;
    assert!(pad_of(&mut world).contains(&"MOVE 1"));
}

/// The bound buttons of the macros-mode pad for the scene the world is in, unbound slots dropped.
fn pad_of(world: &mut World) -> Vec<&'static str> {
    let scene = world.scene();
    names(&plan::plan_for(scene, world)).into_iter().filter(|name| *name != "-").collect()
}

// ---------------------------------------------------------------------------------------------
// Section 13: shops and Pokémon Centers
// ---------------------------------------------------------------------------------------------

/// A town map with one door into the mart and one into the centre, so an errand has something to
/// aim at from outside.
///
/// Viridian City's own id, because that is the area [`geography`](super::geography)'s table has
/// rows for; the two warps are the only thing this fixture needs to be right about.
fn viridian() -> World {
    let mut world = World::room();
    world.map = maps::VIRIDIAN_CITY;
    world.size = MapSize { width: 8, height: 8 };
    world.player = Tile::new(3, 6);
    world.warps = vec![
        Warp { x: 1, y: 1, destination_warp: 0, destination_map: maps::VIRIDIAN_MART },
        Warp { x: 6, y: 1, destination_warp: 0, destination_map: maps::VIRIDIAN_POKECENTER },
    ];
    world.money = 3_000;
    world
}

#[test]
fn the_errand_is_outstanding_once_per_area_per_run() {
    let mut world = viridian();
    assert_eq!(errand(&mut world, Amenity::Mart), Some(maps::VIRIDIAN_MART));
    assert_eq!(errand(&mut world, Amenity::Center), Some(maps::VIRIDIAN_POKECENTER));

    // Paid: the ledger entry takes it off, and it never comes back this run.
    world.areas.insert((Amenity::Mart, maps::VIRIDIAN_CITY));
    assert_eq!(errand(&mut world, Amenity::Mart), None);
    assert_eq!(errand(&mut world, Amenity::Center), Some(maps::VIRIDIAN_POKECENTER));

    // The ledger is per area, so the same kind in another town is a different errand -- and an
    // area the table has no row for has none at all.
    world.map = maps::PEWTER_CITY;
    assert_eq!(errand(&mut world, Amenity::Mart), Some(maps::PEWTER_MART));
    world.map = maps::PALLET_TOWN;
    assert_eq!(errand(&mut world, Amenity::Mart), None, "Pallet Town has neither");
    world.map = maps::ROUTE_1;
    assert_eq!(errand(&mut world, Amenity::Center), None, "a route has neither");
}

#[test]
fn the_mart_errand_is_skipped_when_the_money_is_under_the_cheapest_purchase() {
    let mut world = viridian();
    world.money = CHEAPEST_PURCHASE;
    assert_eq!(errand(&mut world, Amenity::Mart), Some(maps::VIRIDIAN_MART));
    world.money = CHEAPEST_PURCHASE - 1;
    assert_eq!(errand(&mut world, Amenity::Mart), None, "nothing it could buy when it got there");
    // The centre costs nothing, so it is never skipped for money.
    assert_eq!(errand(&mut world, Amenity::Center), Some(maps::VIRIDIAN_POKECENTER));
}

#[test]
fn the_errand_comes_ahead_of_the_rungs_place_and_the_centre_first_when_the_party_is_hurt() {
    let mut world = viridian();
    // The ladder is pointing two maps north; the errand is in this town.
    world.objective = Some(Objective {
        map: maps::VIRIDIAN_FOREST,
        tile: None,
        warp: None,
        edge: None,
        target: None,
    });
    // Healthy party: the mart first, so `GO OBJECTIVE` aims at the mart's door (warp 0 at (1, 1)).
    assert!(!party_needs_rest(&mut world));
    let goals = objective_goals(&mut world);
    assert_eq!(
        goals.iter().map(|aim| aim.tile).collect::<Vec<_>>(),
        vec![Tile::new(1, 1)],
        "the mart's door, not the way north"
    );

    // Hurt party: the centre first, because a hurt party is what ends runs.
    world.mons[0].hp = 9;
    assert!(party_needs_rest(&mut world));
    assert_eq!(
        objective_goals(&mut world).iter().map(|aim| aim.tile).collect::<Vec<_>>(),
        vec![Tile::new(6, 1)]
    );

    // Both errands paid: the rung's place is what is left, and with no route the graph knows from
    // an eight-by-eight fake town the two doors are what the fall-through offers -- the point being
    // that the *errand* is no longer what is aimed at.
    world.areas.insert((Amenity::Mart, maps::VIRIDIAN_CITY));
    world.areas.insert((Amenity::Center, maps::VIRIDIAN_CITY));
    assert_eq!(errand(&mut world, Amenity::Mart), None);
    assert_eq!(errand(&mut world, Amenity::Center), None);
    let goals = objective_goals(&mut world);
    assert!(
        !goals.iter().any(|aim| aim.tile == Tile::new(1, 1) && goals.len() == 1),
        "the mart's door is no longer the single answer"
    );
}

#[test]
fn go_shop_and_go_heal_are_on_the_pad_while_their_errand_stands() {
    let mut world = viridian();
    assert!(on_the_pad(&mut world, MacroKind::GoShop));
    assert!(on_the_pad(&mut world, MacroKind::GoHeal));

    // Paid, and the button leaves the pad -- which is section 12's "a precondition failure means
    // the button is not on the pad", and what makes the errand impossible to loop on.
    world.areas.insert((Amenity::Mart, maps::VIRIDIAN_CITY));
    world.areas.insert((Amenity::Center, maps::VIRIDIAN_CITY));
    assert!(!on_the_pad(&mut world, MacroKind::GoShop));
    assert!(!on_the_pad(&mut world, MacroKind::GoHeal));

    // And in Pallet Town, which has neither, they were never on it.
    let mut pallet = World::room();
    pallet.map = maps::PALLET_TOWN;
    pallet.money = 3_000;
    assert!(!on_the_pad(&mut pallet, MacroKind::GoShop));
    assert!(!on_the_pad(&mut pallet, MacroKind::GoHeal));
}

#[test]
fn an_edge_the_table_cannot_name_stops_being_somewhere_new_once_it_is_stood_on() {
    // The rung-11 reading of row 54 (`infra/docs/macros-traps.md`). The cartridge reports Route
    // 3's connections as north and west; `geography`'s row carries west and east, so the north
    // edge's destination is unnameable -- and an unnameable destination counted as *unvisited*,
    // which made those tiles first-tier for `GO ROUTE` on every hold for ever, with
    // `GO OBJECTIVE` off the pad beside them because nothing on this map leads to the objective.
    let mut world = World::room();
    world.map = maps::ROUTE_3;
    world.size = MapSize { width: 8, height: 8 };
    world.player = Tile::new(4, 4);
    world.connections = Connections { north: true, south: false, east: false, west: true };
    // West is Pewter City, which the table does name and the run has stood on.
    world.seen_maps.insert(maps::PEWTER_CITY);

    let north: Vec<ExitId> = ways(&mut world, Way::Route).iter().map(|exit| exit.id).collect();
    assert!(
        north.iter().all(|id| *id == ExitId::Edge(Edge::North)),
        "the unnameable north edge is the only fresh way out: {north:?}"
    );

    // Stood on, and it is no longer somewhere new -- so the walk falls to the tier that leads
    // toward the objective instead of aiming at the same edge once per hold for ever.
    world.visited.insert(ExitId::Edge(Edge::North));
    let left: Vec<ExitId> = ways(&mut world, Way::Route).iter().map(|exit| exit.id).collect();
    assert!(
        !left.contains(&ExitId::Edge(Edge::North)),
        "an edge nothing can name, already crossed, is not first-tier: {left:?}"
    );
}

#[test]
fn an_errand_does_not_settle_on_the_doormat_it_is_standing_on() {
    // Row 54 of `infra/docs/macros-traps.md`, and section 12.2's rule: "a macro that completes
    // without moving because its precondition is already satisfied where the fly stands is a
    // trap". An errand's aim at a door carries no press -- the warp fires when it is stepped on --
    // so an aim on the tile the fly is already on settles for `SETTLE_FRAMES` and reports `done`
    // with the world exactly as it was. A completed errand walk writes the reached ledger, which
    // `goals_toward` does not filter, so the same button was dealt on the next hold and the same
    // nothing happened again: `GO HEAL` 204 starts at a mean net of 0.0 tiles and a mean reach of
    // 0.0, in a cycle with `GO ROUTE` and `GO FRONTIER` over five tiles.
    let mut world = viridian();
    world.player = Tile::new(6, 1);
    assert!(
        amenity_goals(&mut world, Amenity::Center).is_empty(),
        "the centre's own doormat is not somewhere to walk to"
    );
    assert!(!on_the_pad(&mut world, MacroKind::GoHeal), "so the button is not on the pad");

    // The other errand is a tile away and untouched: this excludes one aim, not the walk.
    assert_eq!(
        amenity_goals(&mut world, Amenity::Mart).iter().map(|aim| aim.tile).collect::<Vec<_>>(),
        vec![Tile::new(1, 1)]
    );
    assert!(on_the_pad(&mut world, MacroKind::GoShop));

    // And one tile off the doormat the centre is a walk again.
    world.player = Tile::new(6, 2);
    assert_eq!(
        amenity_goals(&mut world, Amenity::Center).iter().map(|aim| aim.tile).collect::<Vec<_>>(),
        vec![Tile::new(6, 1)]
    );
    assert!(on_the_pad(&mut world, MacroKind::GoHeal));
}

#[test]
fn an_errand_is_paid_by_a_building_this_run_has_already_been_inside() {
    // The errand ledger is session state and the adapter's map ledger is not, so a restored run
    // re-armed every errand in the town and walked back to a counter it had already used
    // (`docs/design/macros.md` section 13's own residual). Asking both is asking "has this run
    // been in there" twice, and either answer pays the errand.
    let mut world = viridian();
    assert_eq!(errand(&mut world, Amenity::Center), Some(maps::VIRIDIAN_POKECENTER));
    world.seen_maps.insert(maps::VIRIDIAN_POKECENTER);
    assert_eq!(errand(&mut world, Amenity::Center), None, "already been inside it");
    assert!(!on_the_pad(&mut world, MacroKind::GoHeal));
    // The mart is a different building and a different errand.
    assert_eq!(errand(&mut world, Amenity::Mart), Some(maps::VIRIDIAN_MART));
    assert!(on_the_pad(&mut world, MacroKind::GoShop));
}

#[test]
fn go_shop_walks_to_the_marts_door_and_then_to_the_counter() {
    // Outside: the goal is the door, and it is the warp's own tile.
    let mut world = viridian();
    let goals = amenity_goals(&mut world, Amenity::Mart);
    assert_eq!(goals.iter().map(|aim| aim.tile).collect::<Vec<_>>(), vec![Tile::new(1, 1)]);
    assert_eq!(run(&mut world, MacroKind::GoShop).unwrap(), MacroAbort::Done);
    assert_eq!(world.player, Tile::new(1, 1), "standing on the mart's doormat");

    // Inside, the errand is already paid -- the driver writes the ledger on arrival -- and the
    // button's remaining job is the counter. The clerk's four sides are all walls, so the only
    // goal is the tile *over* the counter.
    let mut mart = World::mart();
    mart.areas.insert((Amenity::Mart, maps::VIRIDIAN_CITY));
    let goals = amenity_goals(&mut mart, Amenity::Mart);
    // The reach over the counter is what puts a standable tile in the list at all: every tile
    // beside the clerk is a wall, so without it there is nothing to walk to.
    assert!(
        goals.contains(&super::palette::Aim {
            tile: Tile::new(2, 5),
            press: None,
            face: Some(Facing::Left),
            key: goals[0].key,
        }),
        "over the counter at (1, 5), facing the clerk at (0, 5): {goals:?}"
    );
    // The other standable aims are the two tiles *past* the counter above and below the clerk,
    // which the map-id reach also offers; the route search is what picks among them, and in this
    // fake it picks the one it can reach first.
    assert!(
        goals
            .iter()
            .filter(|aim| mart.walkable(aim.tile.x, aim.tile.y) == Walkable::Yes)
            .all(|aim| aim.tile.distance(Tile::new(0, 5)) == 2),
        "every standable aim is two tiles from the clerk, over something: {goals:?}"
    );
    assert!(on_the_pad(&mut mart, MacroKind::GoShop), "still on the pad, aimed at the counter");
    assert_eq!(run(&mut mart, MacroKind::GoShop).unwrap(), MacroAbort::Done);
    assert_eq!(
        mart.player.distance(Tile::new(0, 5)),
        2,
        "two tiles from the clerk, over something: {:?}",
        mart.player
    );
}

#[test]
fn a_counter_is_the_only_way_to_reach_a_clerk_or_a_nurse() {
    // Without the counter rule the four tiles around the clerk are the whole candidate list, and
    // every one of them is a wall: the walk finds no route and `GO SHOP` refuses once per hold for
    // ever. This is that fact, stated from both sides.
    // The four tiles *beside* a clerk behind a desk: not one of them is standable, which is the
    // whole reason the reach exists.
    let mut mart = World::mart();
    let clerk = Tile::new(0, 5);
    for facing in FACINGS {
        let Some(beside) = clerk.step(facing) else { continue };
        assert_ne!(
            mart.walkable(beside.x, beside.y),
            Walkable::Yes,
            "{beside:?} is beside the clerk and standable"
        );
    }

    let mut center = World::center();
    let goals = heal_goals(&mut center);
    assert!(
        goals.contains(&super::palette::Aim {
            tile: Tile::new(3, 3),
            press: None,
            face: Some(Facing::Up),
            key: goals[0].key,
        }),
        "over the counter at (3, 2), facing the nurse at (3, 1): {goals:?}"
    );
    // And the nurse's four sides, likewise.
    let nurse = Tile::new(3, 1);
    for facing in FACINGS {
        let Some(beside) = nurse.step(facing) else { continue };
        assert_ne!(
            center.walkable(beside.x, beside.y),
            Walkable::Yes,
            "{beside:?} is beside the nurse and standable"
        );
    }

    // The reach is taken from the *map id* as well as from the tile, because the tile read is
    // unreliable on a map smaller than the screen (`palette::facing_target`). So clearing the
    // fake's counter tiles does **not** take it away inside a centre, and that is the point: the
    // map id is a byte the adapter already has and it does not move.
    center.counters.clear();
    assert!(
        heal_goals(&mut center).iter().any(|aim| aim.tile == Tile::new(3, 3)),
        "the reach survives a tile read that has gone wrong: {:?}",
        heal_goals(&mut center)
    );
}

#[test]
fn heal_is_on_the_centres_pad_only_while_the_party_needs_it() {
    let mut center = World::center();
    // A full, healthy party: nothing to rest, so the button is not there.
    assert!(party_rested(&mut center));
    assert!(!precondition(MacroKind::Heal, &mut center));
    assert!(!on_the_pad(&mut center, MacroKind::Heal));

    // The rung-10 party, as the live checkpoint of 2026-09-22 reads it: one Pokemon, 70 of 70,
    // healthy (`infra/docs/macros-traps.md` row 41). `HEAL` was **not** what looped there -- its
    // precondition reads the live party and answers no, and the survey confirmed it on the
    // cartridge. So this is the assertion that the loop was never the heal's.
    let mut rung10 = World::center();
    rung10.mons = vec![Mon { hp: 70, max_hp: 70, ..mon(0, 70, 70, &[(33, 30)]) }];
    assert!(!party_needs_rest(&mut rung10));
    assert!(!precondition(MacroKind::Heal, &mut rung10));
    assert!(!on_the_pad(&mut rung10, MacroKind::Heal));

    // Hurt.
    center.mons[0].hp = 9;
    assert!(precondition(MacroKind::Heal, &mut center));
    assert!(on_the_pad(&mut center, MacroKind::Heal));

    // Statused at full HP counts too, which is what "not at full HP *or* has a status" means.
    center.mons[0].hp = 20;
    center.mons[0].status = Status::Poison;
    assert!(party_needs_rest(&mut center));
    assert!(precondition(MacroKind::Heal, &mut center));

    // And nowhere but a centre, whatever the party looks like.
    let mut mart = World::mart();
    mart.mons[0].hp = 1;
    assert!(!precondition(MacroKind::Heal, &mut mart));
    assert!(!on_the_pad(&mut mart, MacroKind::Heal));
}

#[test]
fn heal_walks_to_the_counter_talks_answers_yes_and_waits_for_the_party_to_read_full() {
    let mut center = World::center();
    center.mons[0].hp = 3;
    center.mons[0].status = Status::Poison;
    // The nurse's box opens once the macro has walked to the counter and pressed A, so the scene
    // changes *under* the macro -- which every other macro treats as the end of it. `HEAL`
    // declares that it spans the two classes, because the conversation is the macro (section 13).
    // The walk is three tiles at sixteen frames each plus the arrival press, so 200 is after it.
    center.switch = Some((200, Scene::Dialog));
    // And the cartridge heals the party while the box is open, which is what the wait reads.
    center.heal_at = Some(240);

    assert_eq!(run(&mut center, MacroKind::Heal).unwrap(), MacroAbort::Done);
    assert_eq!(center.player, Tile::new(3, 3), "at the counter");
    assert_eq!(center.facing, Facing::Up, "facing the nurse");
    assert!(party_rested(&mut center), "the party reads back full and healthy");
    assert!(
        center.pulses.iter().filter(|mask| **mask == buttons::A).count() >= 2,
        "the talk and the YES at least: {:?}",
        center.pulses
    );
    // And the precondition is gone, so the button leaves the pad rather than being pressed again.
    center.scene = Scene::Overworld;
    assert!(!precondition(MacroKind::Heal, &mut center));
}

#[test]
fn a_purchase_navigates_by_the_items_place_in_the_stock_list() {
    // Viridian's counter in menu order: the Antidote is index 1, so that is the cursor the script
    // aims at -- read from the stock list, never counted in presses.
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::Buying);
    world.cursor_max = 3;
    world.stock = vec![item::POKE_BALL, item::ANTIDOTE, 15, 12];
    world.money = 3_000;
    assert_eq!(run(&mut world, MacroKind::BuyAntidote).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 1, "the Antidote's own index");
    assert_eq!(world.pulses.last(), Some(&buttons::A), "the price confirmation");

    // An item the counter does not stock has no index, so the script refuses before anything is
    // pressed: `BUY REPEL` in Viridian.
    let mut world = World::room();
    world.scene = Scene::Shop;
    world.list = List::Shop(ShopScreen::Buying);
    world.stock = vec![item::POKE_BALL, item::ANTIDOTE];
    world.money = 3_000;
    let scene = world.scene();
    let palette = Palette::for_scene(scene, &mut world);
    assert!(
        palette.slots.iter().flatten().all(|spec| spec.kind != MacroKind::BuyRepel),
        "not on the pad at a counter that does not stock it"
    );
    assert!(world.pulses.is_empty());
}

#[test]
fn every_macro_type_has_a_population_a_tag_and_a_gloss() {
    // The three ends of section 11's rule, over the twenty-seven types section 13 leaves.
    assert_eq!(MacroKind::ALL.len(), 31);
    assert_eq!(SLOTS, MacroKind::ALL.len(), "a slot per type");
    assert_eq!(MacroKind::BY_CHANNEL.len(), MacroKind::ALL.len());
    let mut channels: Vec<&str> = MacroKind::ALL.iter().map(|kind| kind.channel()).collect();
    let mut tags: Vec<&str> = MacroKind::ALL.iter().map(|kind| kind.channel_tag()).collect();
    channels.sort_unstable();
    tags.sort_unstable();
    let unique = |mut list: Vec<&str>| {
        let before = list.len();
        list.dedup();
        list.len() == before
    };
    assert!(unique(channels), "a channel means one action");
    assert!(unique(tags), "a tag names one cell");
    for kind in MacroKind::ALL {
        assert!(MacroKind::BY_CHANNEL.contains(&kind), "{}", kind.name());
        assert!(kind.name().len() <= 14, "{}", kind.name());
        assert!(kind.channel_tag().chars().count() <= 8, "{}", kind.channel_tag());
        assert_eq!(
            MacroKind::BY_CHANNEL[usize::from(kind.slot())],
            kind,
            "{} is not at its own slot",
            kind.name()
        );
        assert!(kind.gloss().split_whitespace().count() <= 3, "{}", kind.name());
    }
}

/// Every scene and sub-state deals at least one button (section 13.1's audit, as an assertion).
///
/// The sweep that catches an empty pad, which is the one state the doctrine cannot recover from on
/// its own: nothing presses for the fly, so a scene with no buttons waits for ever.
///
/// **What the overworld's half rests on since section 12.11** is the way out and no longer the
/// unconditional `MENU`: every map in the game has one -- an interior's front door or its
/// staircase, an outdoor map's connection -- and `ways`' last resort offers it regardless of the
/// ledgers when the map holds nothing else worth walking to. So the fixtures below have their
/// doors, where the old ones did not need them, and the map with *no way out at all* is the named
/// residual at the end of this test rather than a case the rule covers.
#[test]
fn no_playable_scene_and_no_sub_state_deals_an_empty_pad() {
    let worst = |world: &mut World| {
        let scene = world.scene();
        assert!(
            plan::plan_for(scene, world).bound() > 0,
            "{} deals nothing",
            scene.label()
        );
    };

    // The overworld, in all three of its rows, on a map with nothing on it but its own door.
    let mut bare = World::ground_floor();
    bare.seen_maps.insert(0x00);
    bare.seen_maps.insert(0x26);
    bare.stood = (0..8).flat_map(|y| (0..8).map(move |x| Tile::new(x, y))).collect();
    assert!(super::palette::stranded(&mut bare), "nothing on this floor to walk to");
    worst(&mut bare);
    let mut outdoors = viridian();
    outdoors.warps.clear();
    outdoors.connections = Connections { north: true, south: false, east: false, west: false };
    outdoors.seen_maps.insert(maps::ROUTE_2);
    outdoors.areas.insert((Amenity::Mart, maps::VIRIDIAN_CITY));
    outdoors.areas.insert((Amenity::Center, maps::VIRIDIAN_CITY));
    outdoors.stood = (0..8).flat_map(|y| (0..8).map(move |x| Tile::new(x, y))).collect();
    worst(&mut outdoors);
    let mut center = World::center();
    center.npcs.clear();
    worst(&mut center);

    // Every other scene, with every precondition false: no stock, no money, no party.
    for scene in [
        Scene::Dialog,
        Scene::Unknown,
        Scene::Menu,
        Scene::Shop,
        Scene::Pc,
        Scene::Battle { own_turn: false, forced_switch: false },
        Scene::Battle { own_turn: true, forced_switch: true },
    ] {
        let mut world = World::room();
        world.scene = scene;
        // See the sweep in `no_playable_scene_deals_an_empty_pad`: `Unknown` with nothing drawn
        // on it is the overworld being driven by the cartridge, and its pad is empty by design.
        world.box_open = scene == Scene::Unknown;
        world.mons.clear();
        worst(&mut world);
    }

    // The fly's own turn in a battle it cannot attack, switch, item or flee in.
    let mut cornered = World::battle();
    cornered.mons = vec![mon(0, 20, 20, &[])];
    cornered.battle = Some((BattleKind::Trainer, true, false));
    worst(&mut cornered);

    // And every battle sub-state of that same cornered turn, which since section 12.10 includes
    // the bag: an empty bag deals `BACK` and nothing else, which is row 34a's answer -- there is
    // nothing to choose, so leaving the list is the press, and what it leaves to is a menu with
    // `MOVE 1` on it.
    for list in [List::BattleMain, List::Moves(1), List::BattleParty, List::BattleBag] {
        cornered.list = list;
        worst(&mut cornered);
    }

    // And the one overworld that still deals nothing, said out loud rather than papered over: a
    // map with **no way out at all**, every tile stood on and nothing on it. No map in Red is
    // that -- an interior has its front door or its staircase and an outdoor map has its
    // connections -- so this is a shape the cartridge does not hold, and `game.padEmptyMs` is what
    // would report it if one ever did (section 13.1).
    let mut sealed = World::room();
    sealed.stood = (0..8).flat_map(|y| (0..8).map(move |x| Tile::new(x, y))).collect();
    assert!(path::exits(&mut sealed).is_empty(), "the fixture really has no way out");
    assert_eq!(plan::plan_for(Scene::Overworld, &mut sealed).bound(), 0);
}

/// Section 13.1's pad-empty rule: an outdoor map with every ledger against it still offers a walk.
///
/// The operator, on stream 2026-09-17: "sometimes the macro buttons disappear and everything just hangs
/// there." The state below is the one that produces it — every route visited, nothing untalked,
/// the near ground covered, no objective — and before this rule the pad was `MENU` and nothing
/// else, which is a pad that cannot move the fly one tile.
#[test]
fn a_map_with_every_ledger_against_it_still_offers_a_way_out() {
    let mut world = viridian();
    world.areas.insert((Amenity::Mart, maps::VIRIDIAN_CITY));
    world.areas.insert((Amenity::Center, maps::VIRIDIAN_CITY));
    world.warps.clear();
    world.connections = Connections { north: true, south: false, east: false, west: false };
    // The map on the other side has been stood on, so tier 1 is empty; there is no objective, so
    // tier 2 is empty.
    world.seen_maps.insert(maps::ROUTE_2);
    // And every tile of it has been stood on, so the frontier is empty in both of its readings.
    world.stood = (0..8).flat_map(|y| (0..8).map(move |x| Tile::new(x, y))).collect();

    assert!(super::palette::stranded(&mut world), "nothing else on this map to walk to");
    assert!(
        !ways(&mut world, Way::Route).is_empty(),
        "the visited connection comes back as the last resort"
    );
    assert!(on_the_pad(&mut world, MacroKind::GoRoute));

    // And it is *only* a last resort: give the map one unstood tile and the visited connection
    // goes away again, because row 2's loop -- in and out of the same door, once per hold -- is
    // what an unconditional tier 3 buys.
    world.stood.remove(&Tile::new(0, 0));
    assert!(!super::palette::stranded(&mut world));
    assert!(ways(&mut world, Way::Route).is_empty());
    assert!(on_the_pad(&mut world, MacroKind::GoFrontier));
}

/// The exit *toward the objective* is the one the last resort prefers, when the graph knows one.
#[test]
fn the_last_resort_prefers_the_exit_toward_the_objective() {
    let mut world = viridian();
    world.areas.insert((Amenity::Mart, maps::VIRIDIAN_CITY));
    world.areas.insert((Amenity::Center, maps::VIRIDIAN_CITY));
    world.warps.clear();
    world.connections = Connections { north: true, south: true, east: false, west: false };
    world.seen_maps.insert(maps::ROUTE_1);
    world.seen_maps.insert(maps::ROUTE_2);
    world.stood = (0..8).flat_map(|y| (0..8).map(move |x| Tile::new(x, y))).collect();
    world.objective = Some(Objective {
        map: maps::PEWTER_CITY,
        tile: None,
        warp: None,
        edge: None,
        target: None,
    });
    // `GO OBJECTIVE` is on the pad here -- its own goals ignore the visited ledger -- so the map is
    // not stranded at all, which is the point: the last resort is for a map with *nothing*.
    assert!(!super::palette::stranded(&mut world));
    // Exclude the north edge and the objective has nowhere to aim either.
    world.targets.record_blocked(world.map, TargetKey::Exit(ExitId::Edge(Edge::North)));
    world.targets.record_blocked(world.map, TargetKey::Exit(ExitId::Edge(Edge::South)));
    assert!(super::palette::stranded(&mut world));
    // Pewter is north of Viridian by way of Route 2, so the north edge is what comes back -- and
    // it comes back *although the blocked ledger is resting it*, because a resting target is
    // still the only place to go.
    let last = ways(&mut world, Way::Route);
    assert!(
        last.iter().all(|exit| exit.id == ExitId::Edge(Edge::North)),
        "toward Pewter and not away from it: {last:?}"
    );
}

/// Section 14's `THROW BALL` (the operator: "throw pokeball should be a macro").
///
/// The precondition is three facts and no judgement: a wild battle, a ball in the bag, and room in
/// the party. When to throw is the fly's -- there is no catch-rate and no enemy-HP knowledge here,
/// and there is none anywhere in this crate.
#[test]
fn throw_ball_needs_a_wild_battle_a_ball_and_room_in_the_party() {
    let mut world = World::battle();
    world.bag = vec![(item::POKE_BALL, 3)];
    assert_eq!(throw_slot(&mut world), Some(0));
    assert!(on_the_pad(&mut world, MacroKind::ThrowBall));

    // Any ball kind, at its own bag index.
    world.bag = vec![(item::POTION, 1), (item::GREAT_BALL, 1)];
    assert_eq!(throw_slot(&mut world), Some(1));
    world.bag = vec![(item::ULTRA_BALL, 1)];
    assert_eq!(throw_slot(&mut world), Some(0));
    world.bag = vec![(item::MASTER_BALL, 1)];
    assert_eq!(throw_slot(&mut world), Some(0));

    // No ball, or a stack of none, is no button.
    world.bag = vec![(item::POTION, 1)];
    assert_eq!(throw_slot(&mut world), None);
    world.bag = vec![(item::POKE_BALL, 0)];
    assert_eq!(throw_slot(&mut world), None);
    assert!(!on_the_pad(&mut world, MacroKind::ThrowBall));

    // A trainer battle refuses a ball on the cartridge ("blocked the BALL!"), so it is off the pad.
    world.bag = vec![(item::POKE_BALL, 1)];
    world.battle = Some((BattleKind::Trainer, true, false));
    assert_eq!(throw_slot(&mut world), None);
    world.battle = Some((BattleKind::Wild, true, false));
    assert_eq!(throw_slot(&mut world), Some(0));

    // A full party is the limit worth stating: a catch would go to the PC box, and this crate has
    // no reviewed symbol for the box count, so the button leaves the pad rather than guessing.
    world.mons = (0..6).map(|slot| mon(slot, 20, 20, &[(33, 30)])).collect();
    assert_eq!(throw_slot(&mut world), None, "six in the party");
    world.mons.truncate(5);
    assert_eq!(throw_slot(&mut world), Some(0));

    // And never outside a battle.
    let mut room = World::room();
    room.bag = vec![(item::POKE_BALL, 1)];
    assert_eq!(throw_slot(&mut room), None);
    assert!(!on_the_pad(&mut room, MacroKind::ThrowBall));
}

/// Section 12.9: a ball is not thrown at a species the party already holds.
///
/// **Live on rung 9**, 69 hours in Viridian Forest: `THROW BALL` was 28 of 183 macro starts, and
/// the forest holds Caterpie, Weedle, Metapod, Kakuna and Pidgey -- the fly had caught its own and
/// went on throwing at them. Every throw spends a ball, and a catch opens the nickname screen,
/// which reads `Unknown` and needs a START the pad has no button for (row 14).
///
/// The party is the caught set: the cartridge's own lifetime record, in the same internal species
/// numbering the enemy is read in. `wPokedexOwned` is by Pokédex number and the conversion is in a
/// ROM bank this crate cannot read, so it is not asked.
#[test]
fn throw_ball_refuses_a_species_the_party_already_holds() {
    // Two distinct internal species indices -- the forest's Weedle and Caterpie, whose exact
    // numbers nothing below depends on.
    let weedle = 0x70;
    let caterpie = 0x7b;
    let mut world = World::battle();
    world.mons.truncate(1);
    world.mons[0].species = caterpie;
    world.bag = vec![(item::POKE_BALL, 5)];

    // A species the party does not hold: the button is on the pad, as before.
    world.enemy = Some(EnemyMon { species: weedle, level: 6, hp: 20, max_hp: 20 });
    assert_eq!(throw_slot(&mut world), Some(0));
    assert!(on_the_pad(&mut world, MacroKind::ThrowBall));

    // The one that is already in the party: off the pad, whatever the bag holds.
    world.enemy = Some(EnemyMon { species: caterpie, level: 6, hp: 20, max_hp: 20 });
    assert_eq!(throw_slot(&mut world), None, "a Caterpie is already in the party");
    assert!(!on_the_pad(&mut world, MacroKind::ThrowBall));

    // Any party slot counts, not only the one that is out.
    world.mons.push(mon(1, 20, 20, &[(33, 30)]));
    world.mons[1].species = weedle;
    world.enemy = Some(EnemyMon { species: weedle, level: 6, hp: 20, max_hp: 20 });
    assert_eq!(throw_slot(&mut world), None, "a Weedle is on the bench");

    // A species the seam could not place leaves the button where it was: an unobservable
    // precondition is a guess, and this crate does not guess (section 13.1).
    world.enemy = None;
    assert_eq!(throw_slot(&mut world), Some(0), "no reading is not a refusal");
    world.enemy = Some(EnemyMon { species: 0, level: 0, hp: 0, max_hp: 0 });
    assert_eq!(throw_slot(&mut world), Some(0), "species 0 is not a species");
}

#[test]
fn throw_ball_opens_the_bag_and_moves_the_cursor_to_the_ball_by_reading_it() {
    let mut world = World::battle();
    world.mons.truncate(1);
    // Two potions before the ball, so the ball's own bag index is 2 and the script has to move.
    world.bag = vec![(item::POTION, 1), (item::ANTIDOTE, 1), (item::POKE_BALL, 5)];
    assert_eq!(throw_slot(&mut world), Some(2));
    // A on ITEM opens the bag, a plain column of three.
    world.opens.push_back(Opens { list: List::BattleBag, cursor: 0, max: 2, grid: false });
    assert_eq!(run(&mut world, MacroKind::ThrowBall).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 2, "the ball's own index, by watching where the cursor went");
    assert!(
        world.pulses.contains(&buttons::DOWN),
        "it walked the list rather than counting presses: {:?}",
        world.pulses
    );
    // The script stops at the confirmation: the throw animation, the shake count and the result
    // text are the between-turns `NEXT`'s, and a nickname prompt is the dialog's `NO`.
    assert_eq!(world.pulses.last(), Some(&buttons::A));
}

/// Section 12.11: a room whose one way out every ledger is resting still offers it.
///
/// **What was live** (2026-09-22, rung 10, the release container's own checkpoint): the fly on
/// **map 0x35, the Pewter museum's upper floor** -- fourteen blocks by eight, one warp at (7, 7)
/// down to the floor below (0x34), two signs and three exhibits. The reproduction is in
/// `infra/docs/macros-traps.md`; the shape of it is that every candidate list on that map empties:
///
/// - `geography` has no row for the museum, so `next_hop` from it answers `None` and
///   `GO OBJECTIVE` has nothing to aim at -- the objective itself is fine (map 0x36, the gym
///   leader, rung 11's BOULDER BADGE);
/// - the three exhibits and two signs are *reached* by `GO NPC` and `GO ITEM`, which retires them
///   for the session;
/// - the four unstood tiles are walked or excluded, and `GO FRONTIER` empties for the window;
/// - the one warp out is classified a **passage** and not an exit -- it is a staircase -- and
///   `unexcluded_exits` drops it while the blocked ledger rests it.
///
/// That left `MENU` and nothing else, and `MENU` opened the start menu whose `BACK` closed it
/// again: 82 starts each in thirty minutes. With `MENU` gone the same state has to deal a walk, and
/// the walk is the staircase **although the ledger is resting it** -- a target the ledger has
/// parked is still the only place to go.
#[test]
fn a_room_whose_only_way_out_the_ledger_rests_still_offers_it() {
    let mut world = World::room();
    // One staircase, no front door: the museum's upper floor, and Red's bedroom, and every other
    // map whose only way anywhere is a passage.
    world.warps = vec![Warp { x: 7, y: 1, destination_warp: 2, destination_map: 0x26 }];
    world.seen_maps.insert(0x26);
    world.stood = (0..8).flat_map(|y| (0..8).map(move |x| Tile::new(x, y))).collect();
    assert_eq!(
        path::exits(&mut world).iter().map(|exit| exit.way).collect::<Vec<_>>(),
        vec![Way::Passage],
        "a staircase and no front door"
    );
    // With the staircase unexcluded the pad is the ordinary indoor one.
    assert!(on_the_pad(&mut world, MacroKind::GoWarp));

    // Now rest it, which is what a refused walk does for ten brain minutes.
    world.targets.record_blocked(world.map, TargetKey::Exit(ExitId::Warp(0)));
    assert!(super::palette::stranded(&mut world), "nothing else on this floor to walk to");
    assert!(
        on_the_pad(&mut world, MacroKind::GoWarp),
        "the resting staircase comes back as the last resort"
    );
    let pad = pad_of(&mut world);
    assert_eq!(pad, ["GO WARP"], "and it is the whole pad: {pad:?}");

    // And it is *only* a last resort: one unstood tile and the resting staircase goes away again,
    // because "the nearest door, once per hold" is row 2's own loop.
    world.stood.remove(&Tile::new(0, 0));
    assert!(!super::palette::stranded(&mut world));
    assert!(!on_the_pad(&mut world, MacroKind::GoWarp));
    assert!(on_the_pad(&mut world, MacroKind::GoFrontier));
}

/// Section 12.11, stated at the pad: no overworld pad is one button that undoes itself.
///
/// The test the museum loop would have failed. `MENU` on the overworld and `BACK` on the start
/// menu are a pair across two scenes, so a per-pad rule cannot see it -- what can is that `MENU`
/// is on no pad at all, and that what is left when every ledger is against the map is a *walk*.
#[test]
fn an_overworld_pad_is_never_one_button_that_undoes_itself() {
    // The museum shape, indoors, and the town shape, outdoors: both stranded, both dealt a walk.
    let mut indoors = World::ground_floor();
    indoors.seen_maps.insert(0x00);
    indoors.seen_maps.insert(0x26);
    indoors.stood = (0..8).flat_map(|y| (0..8).map(move |x| Tile::new(x, y))).collect();
    for exit in path::exits(&mut indoors) {
        indoors.targets.record_blocked(indoors.map, TargetKey::Exit(exit.id));
    }
    let mut outdoors = viridian();
    outdoors.warps.clear();
    outdoors.connections = Connections { north: true, south: false, east: false, west: false };
    outdoors.seen_maps.insert(maps::ROUTE_2);
    outdoors.areas.insert((Amenity::Mart, maps::VIRIDIAN_CITY));
    outdoors.areas.insert((Amenity::Center, maps::VIRIDIAN_CITY));
    outdoors.stood = (0..8).flat_map(|y| (0..8).map(move |x| Tile::new(x, y))).collect();
    outdoors.targets.record_blocked(outdoors.map, TargetKey::Exit(ExitId::Edge(Edge::North)));

    for world in [&mut indoors, &mut outdoors] {
        let pad = pad_of(world);
        assert!(!pad.is_empty(), "an overworld with a way out deals something");
        assert!(!pad.contains(&"MENU"), "MENU is on no pad: {pad:?}");
        assert!(
            pad.iter().any(|name| name.starts_with("GO ")),
            "what is left is a walk rather than a screen to open and close: {pad:?}"
        );
    }
}

/// Section 12.11: the move list deals `BACK` only where the moves can be read.
///
/// The v0.4.3 residual (`infra/docs/macros-traps.md`): `BACK` was 263 of 797 macro starts, every
/// one over an open move list, and 142 of the run's `NEXT` starts were on a move list whose cursor
/// the seam could not place. A move list the seam cannot read the battler for binds no `MOVE n` at
/// all, so its pad was `BACK` alone -- and the only thing that button does is close the list that
/// `MOVE 1` on the menu underneath had just opened. That is 12.10's pair with `MOVE 1` in `NEXT`'s
/// place. `MOVE 1` alone confirms wherever the cursor stands, which is the press that ends a turn.
#[test]
fn the_move_list_deals_back_only_where_the_moves_can_be_read() {
    let mut world = World::battle();
    world.list = List::Moves(3);
    world.grid = false;
    world.cursor_max = 2;
    assert_eq!(pad_of(&mut world), ["BACK", "MOVE 1", "MOVE 2", "MOVE 3"]);

    // The battler the seam cannot place: no active slot, so `battle.own` is `None`.
    world.active = None;
    let pad = pad_of(&mut world);
    assert_eq!(pad, ["MOVE 1"], "one button, and it ends the turn: {pad:?}");
    assert!(move_slot_bound(&mut world, MacroKind::Move1));
    assert!(!move_slot_bound(&mut world, MacroKind::Move2));
    // And it really presses: the cursor is confirmed where it stands, which is Struggle's own
    // path (row 30a) and the only reading available here.
    assert_eq!(run(&mut world, MacroKind::Move1).unwrap(), MacroAbort::Done);
    assert_eq!(world.pulses.last(), Some(&buttons::A));
}

/// Section 12.11: Red's battle menu is two columns, so its order is FIGHT, ITEM, PKMN, RUN.
///
/// The screen reads `FIGHT PKMN` over `ITEM RUN` and the game's own index does not: it is the row
/// inside the column the cursor is in, plus two for the right column. `battle_entry` had `PKMN` 1
/// and `ITEM` 2, which is the row-major reading of the picture, so **every macro that opened the
/// bag opened the party list and every macro that opened the party list opened the bag**.
///
/// **Surveyed on the cartridge** (`infra/docs/macros-traps.md`): a `THROW BALL` aiming at 2 walked
/// the cursor to `wTopMenuItemX` 15 / `wCurrentMenuItem` 0, pressed A, and the party list opened --
/// `wTopMenuItemY` 1, `wTopMenuItemX` 0, `wListMenuID` `$02` -- with the game writing
/// `wCurrentMenuItem` 2 on the frame after the press. On v0.4.3 `THROW BALL` was 63 starts and 63
/// `blocked`, and `SWITCH` 15 of them: never the macro's own list, always the other one.
#[test]
fn the_battle_menus_two_columns_put_item_under_fight_and_pkmn_beside_it() {
    use super::cartridge::battle_entry;
    assert_eq!(
        [battle_entry::FIGHT, battle_entry::ITEM, battle_entry::PKMN, battle_entry::RUN],
        [0, 1, 2, 3],
        "the left column is FIGHT then ITEM, the right is PKMN then RUN"
    );
    // And the seam reads the same order back off the fake's own geometry: DOWN moves one inside a
    // column, RIGHT moves two across.
    let mut world = World::battle();
    let menu = |world: &mut World| super::palette::battle_menu(world);
    assert_eq!(menu(&mut world), BattleMenu::Main { cursor: battle_entry::FIGHT });
    world.on_pulse(buttons::DOWN);
    assert_eq!(menu(&mut world), BattleMenu::Main { cursor: battle_entry::ITEM });
    world.on_pulse(buttons::UP);
    world.on_pulse(buttons::RIGHT);
    assert_eq!(menu(&mut world), BattleMenu::Main { cursor: battle_entry::PKMN });
    world.on_pulse(buttons::DOWN);
    assert_eq!(menu(&mut world), BattleMenu::Main { cursor: battle_entry::RUN });
}

/// Section 12.11: a cursor step waits for the list it was built for.
///
/// **Measured on the cartridge, v0.4.3** (`infra/docs/macros-traps.md`): `THROW BALL` was **63
/// starts and 63 `blocked`**, mean sixty-nine frames -- which is the cursor to ITEM, the A that
/// confirms it, the twenty settle frames, and then a refusal on the very next frame. The bag had
/// not drawn yet, so `listing` still answered for the battle *menu*: four entries, `max` 3. The
/// ball's own bag index was above that, so the step read "off the end of the list" and gave up at
/// once -- and where the index was inside it, the step pressed UP and LEFT at the battle menu
/// instead, which is the blind pressing section 4 forbids.
#[test]
fn throw_ball_waits_for_the_bag_rather_than_reading_the_menu_it_came_from() {
    let mut world = World::battle();
    world.mons.truncate(1);
    // Five items with the ball last, so its bag index is 4 -- above the battle menu's `max` of 3,
    // which is the number the old step compared it against.
    world.bag = vec![
        (item::POTION, 1),
        (item::ANTIDOTE, 1),
        (item::REPEL, 1),
        (item::POTION, 1),
        (item::POKE_BALL, 5),
    ];
    assert_eq!(throw_slot(&mut world), Some(4));
    // The bag takes longer to draw than the script's twenty settle frames, which is the cartridge's
    // own timing and the whole of the trap.
    world.opens_draw_in = 40;
    world.opens.push_back(Opens { list: List::BattleBag, cursor: 0, max: 4, grid: false });
    assert_eq!(run(&mut world, MacroKind::ThrowBall).unwrap(), MacroAbort::Done);
    assert_eq!(world.cursor, 4, "the ball's own bag index, by reading the bag's cursor");
    assert_eq!(world.pulses.last(), Some(&buttons::A));
    // Nothing was pressed at the menu it came from while the bag was drawing: the presses are the
    // one that chose ITEM and then the bag's own.
    assert_eq!(
        world.pulses.iter().filter(|mask| **mask == buttons::UP).count(),
        0,
        "no blind press at the list it had already answered: {:?}",
        world.pulses
    );

    // And a list that never opens is `blocked` rather than pressed at blind.
    let mut never = World::battle();
    never.mons.truncate(1);
    never.bag = vec![(item::POKE_BALL, 5)];
    assert_eq!(run(&mut never, MacroKind::ThrowBall).unwrap(), MacroAbort::Blocked);
}

/// Row 37 of `infra/docs/macros-traps.md`: a tile the cartridge pushes the fly off is not a tile
/// to stand on, and the ground beside a villager is not the villager's fault.
///
/// **The measurement.** From the release container's Viridian checkpoint, twenty brain minutes:
/// **53,266 of 54,377 text-box frames on one tile**, (19, 9) of Viridian City, 991 of them
/// answered `YES`. `ViridianCityCheckGotPokedexScript` fires on *every frame* the fly stands there
/// without the Pokédex — "This is private property!", then it walks the player down — and (19, 9)
/// is one of the four tiles `approach` offers around the sleeping old man at (18, 9). So `GO NPC`
/// walked onto it, the fly advanced the box, the walk went back.
///
/// The blocked ledger could not close it: it is keyed on the *target*, so it excluded the old man
/// for ten brain minutes and said nothing about the ground, and the window reopened.
#[test]
fn a_tile_the_cartridge_pushes_the_fly_off_is_not_a_tile_to_walk_to() {
    // The shape of the Viridian case: a person with the fly on one side and a scripted tile on
    // the other. The fly starts away from both so nothing is excluded for being already faced.
    let mut world = World::room().at(3, 6);
    world.facing = Facing::Up;
    world.npcs = vec![Npc { slot: 1, picture: 1, x: 3, y: 3, facing: Facing::Down }];

    // All four sides of the villager are offered, as they always were.
    let before: Vec<Tile> = untalked_people(&mut world)
        .into_iter()
        .map(|(tile, _)| tile)
        .collect();
    assert_eq!(before, vec![Tile::new(3, 3)], "the person is the target");
    assert_eq!(run(&mut world, MacroKind::GoNpc).unwrap(), MacroAbort::Done);
    assert_eq!(world.player.distance(Tile::new(3, 3)), 1, "it stood beside the villager");

    // Now make the tile it chose a scripted one and let the ledger learn it. The walk still has
    // three other sides, so the villager is not written off for the ground beside him.
    let chosen = world.player;
    let mut world = World::room().at(3, 6);
    world.facing = Facing::Up;
    world.npcs = vec![Npc { slot: 1, picture: 1, x: 3, y: 3, facing: Facing::Down }];
    world.pushes.insert(chosen);
    assert!(on_the_pad(&mut world, MacroKind::GoNpc), "the villager is still worth walking to");
    assert_eq!(run(&mut world, MacroKind::GoNpc).unwrap(), MacroAbort::Done);
    assert_ne!(world.player, chosen, "and it stood on one of his other sides");
    assert_eq!(world.player.distance(Tile::new(3, 3)), 1);

    // With every side of him scripted there is nothing to walk to and the button leaves the pad,
    // which is section 12's "a precondition failure means the button is not on the pad".
    for facing in FACINGS {
        if let Some(side) = Tile::new(3, 3).step(facing) {
            world.pushes.insert(side);
        }
    }
    assert!(!on_the_pad(&mut world, MacroKind::GoNpc));

    // The route will not cross one either, which is the other half of the fix: excluding a
    // scripted tile as a *goal* left the A* routing across it on the way to somewhere else, and
    // the script fires on any frame the fly stands there. Measured: (19, 9) went from 53,266
    // text-box frames in twenty brain minutes to 12,919 on the goal fix alone.
    let mut through = World::room().at(0, 3);
    through.walls = (0..8).filter(|y| *y != 3).map(|y| Tile::new(1, y)).collect();
    assert!(
        path::route(&mut through, &[Tile::new(3, 3)]).is_some(),
        "a corridor through (1, 3) is walkable"
    );
    through.pushes.insert(Tile::new(1, 3));
    assert!(
        path::route(&mut through, &[Tile::new(3, 3)]).is_none(),
        "and it is a wall once the cartridge has pushed the fly off it"
    );

    // And the frontier will not walk onto one either: the tile is not new ground, it is a script.
    let mut frontier = World::room().at(3, 3);
    frontier.stood = (0..8).flat_map(|y| (0..8).map(move |x| Tile::new(x, y))).collect();
    frontier.stood.remove(&Tile::new(0, 0));
    assert!(!super::palette::frontier_aims(&mut frontier).is_empty());
    frontier.pushes.insert(Tile::new(0, 0));
    frontier.pushes.insert(Tile::new(0, 1));
    frontier.pushes.insert(Tile::new(1, 0));
    assert!(
        super::palette::frontier_aims(&mut frontier).is_empty(),
        "neither the bordering tiles nor the far fallback offers a scripted tile"
    );
}

/// The push-back writes the ledger, and it writes the *tile* rather than the target.
#[test]
fn a_scripted_push_back_records_the_tile_it_happened_on() {
    let mut world = World::ground_floor().at(3, 3);
    world.facing = Facing::Up;
    world.npcs = vec![Npc { slot: 1, picture: 1, x: 3, y: 2, facing: Facing::Down }];
    // The cartridge takes the joypad two frames in, which is what the Viridian gate and the
    // private-property tile both do (`MacroState::scripted`).
    world.scripted_at = Some(2);
    world.switch = Some((3, Scene::Dialog));

    let mut machine = MacroMachine::new(1);
    let _ = run_with(&mut machine, &mut world, MacroKind::Talk);
    let pushed = machine.take_pushed();
    assert_eq!(
        pushed,
        Some((world.map, Tile::new(3, 3))),
        "the tile the macro was standing on, not the person it was facing"
    );
}

mod map_aware;
mod shop_purchase;

// ---------------------------------------------------------------------------------------------
// Section 12.12: the nurse's box (row 41)
// ---------------------------------------------------------------------------------------------

#[test]
fn talk_is_off_the_pad_at_a_nurse_the_party_has_no_use_for() {
    // The overworld frame the ring starts from: at the counter, facing the nurse, party full.
    let mut center = World::center().at(3, 3);
    center.facing = Facing::Up;
    assert!(facing_nurse(&mut center), "the nurse is the thing ahead, over the counter");
    assert!(rested_nurse(&mut center));
    assert!(!precondition(MacroKind::Talk, &mut center));
    assert!(!on_the_pad(&mut center, MacroKind::Talk));

    // Hurt, and she is worth talking to again -- the conversation now does something.
    center.mons[0].hp = 4;
    assert!(!rested_nurse(&mut center));
    assert!(precondition(MacroKind::Talk, &mut center));
    assert!(on_the_pad(&mut center, MacroKind::Talk));

    // Statused at full HP counts as needing her, exactly as `HEAL`'s own precondition does.
    center.mons[0].hp = center.mons[0].max_hp;
    center.mons[0].status = Status::Poison;
    assert!(precondition(MacroKind::Talk, &mut center));

    // And nobody else in the game is narrowed by this: an ordinary person on the same map is
    // still `TALK`'s whatever the party reads.
    let mut villager = World::center().at(3, 3);
    villager.facing = Facing::Up;
    villager.npcs = vec![Npc { slot: 1, picture: 1, x: 3, y: 2, facing: Facing::Down }];
    villager.counters.clear();
    villager.walls.clear();
    assert!(!facing_nurse(&mut villager));
    assert!(precondition(MacroKind::Talk, &mut villager), "a full party is not a reason to ignore a person");
}

#[test]
fn the_nurses_prompt_offers_only_the_answer_that_changes_something() {
    let mut center = World::at_the_nurse();
    center.prompt = true;
    assert!(nurse_prompt(&mut center));

    // Full and healthy: the offer is for nothing, so `NO` is the answer and `YES` is not on the
    // pad. `NEXT` is off it too -- an A press at a two-option box *is* `YES` (12.10).
    assert_eq!(names(&plan::plan_for(Scene::Dialog, &mut center)), ["NO"]);

    // Hurt: `YES` is the answer, and `NO` is the one that changes nothing.
    center.mons[0].hp = 4;
    assert_eq!(names(&plan::plan_for(Scene::Dialog, &mut center)), ["YES"]);

    // A plain text box, which is forty-five of the nurse's forty-six frames, keeps all three: A
    // and B both advance one and there is no choice for `NEXT` to be the wrong name for.
    center.prompt = false;
    assert_eq!(names(&plan::plan_for(Scene::Dialog, &mut center)), ["NEXT", "YES", "NO"]);
}

#[test]
fn a_readable_prompt_that_is_not_the_nurses_keeps_both_answers_and_loses_next() {
    // Red draws a two-option box for a dozen scripts and only the nurse's is an offer about the
    // party, so nothing else is narrowed by the party: both answers, and no `NEXT`.
    let mut world = World::room();
    world.scene = Scene::Dialog;
    world.prompt = true;
    assert!(!nurse_prompt(&mut world));
    assert_eq!(names(&plan::plan_for(Scene::Dialog, &mut world)), ["YES", "NO"]);
}

#[test]
fn a_yes_no_box_that_reopens_unchanged_takes_that_answer_off_the_pad() {
    // Section 12.12's general rule, away from the nurse: the answer completed, the fly is on the
    // tile it answered from, and the same prompt is up again -- so the press did nothing, which
    // is section 12.2's trap, and the answer joins the blocked ledger for its window.
    let mut world = World::room();
    world.scene = Scene::Dialog;
    world.prompt = true;
    assert_eq!(names(&plan::plan_for(Scene::Dialog, &mut world)), ["YES", "NO"]);

    assert_eq!(run(&mut world, MacroKind::Yes).unwrap(), MacroAbort::Done);
    let key = answer_key(&mut world, true).expect("a loaded map has a tile");
    assert!(world.targets.blocked(world.map, key), "the answer that changed nothing");
    assert_eq!(
        names(&plan::plan_for(Scene::Dialog, &mut world)),
        ["NO"],
        "the other answer is still there, which is what ends the ring"
    );

    // And `NO` is not excluded by `YES`'s entry: one answer, one key.
    let no = answer_key(&mut world, false).expect("a loaded map has a tile");
    assert!(!world.targets.blocked(world.map, no));
}

#[test]
fn a_prompt_that_does_not_come_back_excludes_nothing() {
    // The other half of the same rule: an answer that settled the box is an answer worth making
    // again. Nothing is excluded, because nothing looped.
    let mut world = World::room();
    world.scene = Scene::Dialog;
    world.prompt = true;
    // The box closes on the frame after the press, which is what answering it does.
    world.switch = Some((2, Scene::Overworld));
    assert_eq!(run(&mut world, MacroKind::Yes).unwrap(), MacroAbort::Done);
    let key = answer_key(&mut world, true).expect("a loaded map has a tile");
    assert!(!world.targets.blocked(world.map, key));
}

#[test]
fn a_declined_heal_writes_the_nurse_into_the_talked_ledger() {
    // 12.4's rule is "the fly said no, so the thing is still on offer", and for one person in Red
    // that is wrong: the pad only ever offers `NO` at her prompt when the party is already full,
    // so declining is the errand's end rather than a conversation postponed.
    let mut center = World::at_the_nurse();
    center.prompt = true;
    assert!(party_rested(&mut center));

    assert_eq!(run(&mut center, MacroKind::No).unwrap(), MacroAbort::Done);
    assert!(center.talked.contains(&TalkTarget::Sprite(1)), "the nurse: {:?}", center.talked);

    // So `TALK` is off the pad there even if the party is hurt later: the ledger is the record
    // that this run has had her conversation.
    center.scene = Scene::Overworld;
    center.mons[0].hp = 4;
    assert!(!precondition(MacroKind::Talk, &mut center));
}

#[test]
fn a_completed_heal_writes_the_nurse_into_the_talked_ledger() {
    let mut center = World::center();
    center.mons[0].hp = 3;
    center.switch = Some((200, Scene::Dialog));
    center.heal_at = Some(240);

    assert_eq!(run(&mut center, MacroKind::Heal).unwrap(), MacroAbort::Done);
    assert!(party_rested(&mut center));
    assert!(
        center.talked.contains(&TalkTarget::Sprite(1)),
        "a completed heal has had the conversation: {:?}",
        center.talked
    );

    // And `TALK` cannot reopen it. The reached window would expire in ten brain minutes and offer
    // the fly the same forty-six text frames again; the talked entry is for the session.
    center.scene = Scene::Overworld;
    assert_eq!(center.player, Tile::new(3, 3));
    assert!(!precondition(MacroKind::Talk, &mut center));
}
