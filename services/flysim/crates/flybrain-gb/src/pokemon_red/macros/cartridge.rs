//! What the executor needs that agent A's seam does not carry, and the small shapes both halves
//! talk in.
//!
//! [`state`](super::state) is agent A's file and agent A's to change: it is copied into this
//! branch verbatim. What `docs/design/macros.md` names and it does not carry are cartridge
//! *tables* and session *ledgers* rather than WRAM:
//!
//! - the mart's stock list, for section 13's four purchases;
//! - the per-map visit set, for "nearest **unvisited** warp or map-edge exit";
//! - the talked, blocked, reached, stood and errand ledgers of sections 12 and 13.
//!
//! **The move table and the type chart are gone from here.** They existed for `ATTACK`'s "highest
//! base power move with PP, type effectiveness applied from the ROM's type chart", and section 14
//! replaced `ATTACK` with one button per move slot: which move is used is the fly's choice now,
//! so the knowledge that used to choose it is not narrowed, defaulted or degraded -- it is
//! removed.
//!
//! [`MacroState`] is where they go: one extension of A's trait, every method defaulted so that
//! `impl MacroState for PokeState {}` is enough to compile, and every default chosen so that a
//! missing table *narrows* what the palette offers instead of guessing. A slot the seam cannot
//! answer for is unbound, which is exactly section 3's own mechanism, and no macro ever presses a
//! button on a value it had to invent.

use crate::adapter::PlaceKind;

use super::geography::Amenity;
use super::state::{Facing, GameState, MapGrid};

/// `constants/pokemon_data_constants.asm`: `PARTY_LENGTH`, how many Pokémon fit in the party.
///
/// `THROW BALL`'s only precondition beyond a ball and a wild battle: a full party sends a catch to
/// the current PC box, and this crate has no reviewed symbol for the box count, so a full party
/// takes the button off the pad. See [`super::palette::throw_slot`].
pub const PARTY_CAPACITY: usize = 6;

/// Every direction, in the readout's channel order.
pub const FACINGS: [Facing; 4] = [Facing::Up, Facing::Down, Facing::Left, Facing::Right];

/// Item ids the palette's preconditions name, from `constants/item_constants.asm` at the pinned
/// pokered commit. Only the palette's preconditions depend on them, so a wrong id leaves a slot
/// unbound rather than pressing anything.
pub mod item {
    pub const MASTER_BALL: u8 = 1;
    pub const ULTRA_BALL: u8 = 2;
    pub const GREAT_BALL: u8 = 3;
    pub const POKE_BALL: u8 = 4;

    /// Every ball a wild battle can be caught with, cheapest last.
    ///
    /// `SAFARI_BALL` (`$08`) is deliberately absent: a Safari battle has its own menus and reads
    /// as `Scene::Unknown` (`docs/design/macros-wram.md`), so `THROW BALL` is never dealt there
    /// and a ball that only exists inside it is not one this palette can throw.
    pub const BALLS: [u8; 4] = [MASTER_BALL, ULTRA_BALL, GREAT_BALL, POKE_BALL];
    /// `$0b`. Section 13's third purchase: the cheapest thing any mart in Kanto sells.
    pub const ANTIDOTE: u8 = 11;
    pub const POTION: u8 = 20;
    /// `$1e`.
    pub const REPEL: u8 = 30;
}

/// Mart prices at the pinned pokered commit, for section 3's "if money allows".
///
/// `data/items/prices.asm`, which is a `bcd3` table indexed by item id: `POKE_BALL` 200,
/// `ANTIDOTE` 100, `POTION` 300, `REPEL` 350. The table lives in a ROM bank this crate may not
/// read ([`super::super::state`]'s own note), so the four numbers are quoted here with their
/// source, and a wrong one can only ever leave a purchase *unbound* -- the cartridge charges what
/// it charges, and the macro never types a quantity other than one.
pub mod price {
    pub const POKE_BALL: u32 = 200;
    pub const ANTIDOTE: u32 = 100;
    pub const POTION: u32 = 300;
    pub const REPEL: u32 = 350;
}

/// The four purchases section 13 names, as `(item id, price)`, in the order the palette deals them.
///
/// One list, three uses: the shop scene's buttons, each purchase's own "the mart stocks it and the
/// money covers it" precondition, and [`CHEAPEST_PURCHASE`], which is what "money below the
/// cheapest purchase skips the mart errand" measures against.
pub const PURCHASES: [(u8, u32); 4] = [
    (item::POTION, price::POTION),
    (item::POKE_BALL, price::POKE_BALL),
    (item::ANTIDOTE, price::ANTIDOTE),
    (item::REPEL, price::REPEL),
];

/// The least money any of [`PURCHASES`] can be made with: 100, the Antidote.
///
/// Section 13's "money below the cheapest purchase skips the mart errand". Deliberately the
/// cheapest of the *four buttons* rather than of the mart's stock: what a given counter stocks is
/// only readable once the counter is open, and an errand the fly cannot pay for anywhere is one
/// not worth walking to. A mart that turns out not to stock the Antidote simply leaves that button
/// off the pad when the fly gets there, which is the ordinary narrowing.
pub const CHEAPEST_PURCHASE: u32 = {
    let mut least = u32::MAX;
    let mut index = 0;
    while index < PURCHASES.len() {
        if PURCHASES[index].1 < least {
            least = PURCHASES[index].1;
        }
        index += 1;
    }
    least
};

/// Cursor indices of the battle menu, as `state::BattleMenu::Main` documents them: "0 FIGHT,
/// 1 PKMN, 2 ITEM, 3 RUN".
pub mod battle_entry {
    pub const FIGHT: u8 = 0;
    /// `$01`. **The left column's second row, not the right column's first.**
    ///
    /// Red draws the battle menu as `FIGHT PKMN` over `ITEM RUN`, which reads as two rows -- and
    /// the game's own index is two *columns*: `wCurrentMenuItem` is the row inside the column the
    /// cursor is in and selection adds two for the right one. So the order is FIGHT, `ITEM`,
    /// `PKMN`, RUN, and this pair was the other way round for as long as the four constants have
    /// existed.
    ///
    /// **Measured on the cartridge** (2026-09-22, `infra/docs/macros-traps.md`): a `THROW BALL`
    /// aiming at 2 walked the cursor to `wTopMenuItemX` 15, `wCurrentMenuItem` 0, pressed A, and
    /// the **party list** opened -- `wTopMenuItemY` 1, `wTopMenuItemX` 0, `wListMenuID` `$02`.
    /// The frame after the press the game wrote `wCurrentMenuItem` 2, which is the right column's
    /// first row plus two, and the right column's first row is PKMN. So `THROW BALL` and `ITEM`
    /// opened the party list and `SWITCH` opened the bag, every single time: `THROW BALL` was 63
    /// starts and 63 `blocked` on v0.4.3, and `SWITCH` 15 of them.
    pub const ITEM: u8 = 1;
    /// `$02`. The right column's first row: see [`ITEM`].
    pub const PKMN: u8 = 2;
    pub const RUN: u8 = 3;
}

/// The button that walks, looks or moves a cursor this way.
pub const fn button(facing: Facing) -> u8 {
    use crate::emulator::buttons;
    match facing {
        Facing::Down => buttons::DOWN,
        Facing::Up => buttons::UP,
        Facing::Left => buttons::LEFT,
        Facing::Right => buttons::RIGHT,
    }
}

/// The direction that points back the way `facing` came.
pub const fn opposite(facing: Facing) -> Facing {
    match facing {
        Facing::Down => Facing::Up,
        Facing::Up => Facing::Down,
        Facing::Left => Facing::Right,
        Facing::Right => Facing::Left,
    }
}

/// The last outdoor map id (`ROUTE_25`), which is how a warp is classified (section 9.1).
///
/// `constants/map_constants.asm` at the pinned pokered commit numbers the nine towns and cities,
/// Indigo Plateau, Saffron, one unused id and then `ROUTE_1` through `ROUTE_25` as `$00`..`$24`,
/// and `REDS_HOUSE_1F` at `$25` begins the interiors. So "is this map outdoors" is a comparison
/// rather than a table, and it is the decomp's own ordering rather than a tileset read -- which
/// matters because a map header lives in a ROM bank this crate cannot reach.
pub const LAST_OUTDOOR_MAP: u8 = 0x24;

/// A warp destination of `$ff`: "the map the player came from" (`LAST_MAP`).
///
/// How pokered writes a building's own front door, rather than naming the town outside. Counted
/// as outdoors by [`outdoors`], because on every interior map in the game that is what it is.
pub const LAST_MAP: u8 = 0xff;

/// Whether a map id is one of the outdoor maps.
pub const fn outdoors(map: u8) -> bool {
    map <= LAST_OUTDOOR_MAP
}

/// Whether a warp's destination is outdoors, `LAST_MAP` included.
pub const fn destination_outdoors(destination: u8) -> bool {
    destination == LAST_MAP || outdoors(destination)
}

/// A tile coordinate on the current map, in the same unit the player's coordinates are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tile {
    pub x: u8,
    pub y: u8,
}

impl Tile {
    pub const fn new(x: u8, y: u8) -> Self {
        Self { x, y }
    }

    /// Manhattan distance: the A* heuristic's unit, and the "nearest" in "nearest exit".
    pub fn distance(self, other: Self) -> u32 {
        u32::from(self.x.abs_diff(other.x)) + u32::from(self.y.abs_diff(other.y))
    }

    /// The neighbour one step in `facing`, or `None` where that leaves the coordinate space.
    pub fn step(self, facing: Facing) -> Option<Self> {
        let (dx, dy) = facing.delta();
        let x = i32::from(self.x) + i32::from(dx);
        let y = i32::from(self.y) + i32::from(dy);
        Some(Self { x: u8::try_from(x).ok()?, y: u8::try_from(y).ok()? })
    }
}

/// One way out of the current map, as the exploration ledger names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExitId {
    /// The warp at this index of `GameState::warps`.
    Warp(u8),
    /// A step off this edge of the map.
    Edge(Edge),
}

/// A map edge that leads to another map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Edge {
    North,
    South,
    East,
    West,
}

impl Edge {
    /// Every edge with the direction a step off it travels.
    pub const ALL: [(Self, Facing); 4] = [
        (Self::North, Facing::Up),
        (Self::South, Facing::Down),
        (Self::West, Facing::Left),
        (Self::East, Facing::Right),
    ];
}

/// Where the ladder's next unreached rung is, in the executor's own tile type.
///
/// [`crate::adapter::MapPlace`] crossing the seam: `state.rs` converts one into this so that
/// nothing under `macros/` has to name an adapter type. `tile` and `warp` are both `None` for
/// every rung the Pokémon catalog knows today -- it knows maps and nothing finer -- and the
/// script handles all three cases rather than assuming the sparse one away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Objective {
    /// The map the rung is earned on.
    pub map: u8,
    /// A tile on that map, when the catalog knows one.
    pub tile: Option<Tile>,
    /// An index into that map's warp table, when the catalog knows one.
    pub warp: Option<u8>,
    /// An edge of that map to step off, when the place is a connection.
    pub edge: Option<Edge>,
    /// What on that map earns the rung, when the rung is a conversation rather than a place:
    /// [`crate::adapter::PlaceKind`] as the macro layer sees it.
    pub target: Option<PlaceKind>,
}

/// One thing on the current map that prints something when it is faced.
///
/// The key of the *talked* ledger (`docs/design/macros.md` section 12): map plus object index,
/// where the index is the identity the cartridge itself gives the thing on this map. A sprite is
/// its sprite slot, which is stable while the map is loaded and is the same number the map's
/// `object_event` list is in; a sign is its text id, which is what a `bg_event` *is*.
///
/// Not a tile, deliberately: a person walks, and the tile it was talked to on says nothing about
/// whether it has been talked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TalkTarget {
    /// A sprite -- a person, or an object such as a Pokeball on a table -- by its sprite slot.
    Sprite(u8),
    /// A sign, by its text id.
    Sign(u8),
}

/// One thing a macro can be *aimed at*, as the session's target ledgers name it.
///
/// `docs/design/macros.md` section 12 keeps knowledge inside macros, so the two ledgers of the
/// 2026-09-16 Viridian stall live behind [`MacroState`] and are read where a macro chooses its
/// target -- never where the fly chooses its macro. One key covers all five walking macros
/// because all five abort the same way and the exclusion is about the *target*, not the button:
///
/// - [`TargetKey::Exit`] for `GO OUT`, `GO WARP`, `GO ROUTE` and the cross-map step of
///   `GO OBJECTIVE`, by the same [`ExitId`] the exploration ledger names a way out with;
/// - [`TargetKey::Thing`] for `GO ITEM` and `GO NPC`, by the same [`TalkTarget`] the talked
///   ledger uses, so a thing that is walked to, reached and then talked to is one key throughout;
/// - [`TargetKey::Tile`] for `GO FRONTIER` and for an objective the catalog knows a tile for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TargetKey {
    /// A way out of the map.
    Exit(ExitId),
    /// A thing on the map to stand beside and face.
    Thing(TalkTarget),
    /// A tile of the map to stand on.
    Tile(Tile),
    /// One answer to the YES/NO box at a tile: the key of the reopened-prompt exclusion
    /// (`docs/design/macros.md` section 12.12).
    ///
    /// Not a walk's target -- nothing is aimed at it -- but the same ledger and the same window,
    /// because it is the same fact: an answer that changed nothing is an answer not worth making
    /// again from this tile for a while. Keyed by the tile rather than by the person, because what
    /// the box belongs to is whatever the fly is standing in front of, and the box is the only
    /// thing on screen while it is open.
    Answer { at: Tile, yes: bool },
}

/// Which list the shared cursor belongs to right now.
///
/// Red keeps one cursor for every menu in the game (`wCurrentMenuItem`), so "where is the cursor"
/// is only half a question: a script that opens the bag from the battle menu and then navigates to
/// a bag index has to know that the index it is aiming at belongs to the *bag* and not to the four
/// entries it was reading a moment ago. Measured on the cartridge 2026-09-22: `THROW BALL` was
/// **63 starts and 63 `blocked`**, mean sixty-nine frames, because the bag takes longer than the
/// twenty settle frames to draw -- so the step that should have walked the bag list read the
/// battle menu's `max` of 3, found the ball's bag index above it, and gave up at once
/// (`docs/design/macros.md` section 12.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    /// FIGHT / PKMN / ITEM / RUN.
    BattleMain,
    /// The move list.
    BattleMoves,
    /// The party list, inside a battle.
    BattleParty,
    /// The bag, inside a battle.
    BattleBag,
    /// The start menu.
    StartMenu,
    /// A mart's counter, on whichever of its screens is up.
    Shop,
    /// A PC.
    Pc,
}

/// A cursor the current scene's macros navigate: which list it is, where it is, and how far it can
/// go.
///
/// Derived from whichever of agent A's menus is up, so a script asks "where is the cursor" once
/// and does not care whether it is in a battle, a mart or the start menu -- but it can ask *which*
/// list answered, which is what a script that crosses from one list into another needs
/// ([`ListKind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listing {
    pub kind: ListKind,
    pub current: u8,
    pub max: u8,
}

/// The state a macro reads: agent A's seam, plus the tables and the ledger above.
///
/// Every method is defaulted, and every default is a *narrowing*:
///
/// - no stock list means the four `BUY …` buttons stay unbound;
/// - an empty ledger means every exit is unvisited, which is a fresh run rather than an
///   exhausted one.
pub trait MacroState: GameState {
    /// What the open mart sells, in menu order, so an item's position is its cursor index.
    fn shop_stock(&mut self) -> Vec<u8> {
        Vec::new()
    }

    /// Whether a tile of the loaded map is one the game lets the player talk *over*: a counter.
    ///
    /// `docs/design/macros.md` section 13. A mart clerk and a Pokémon Center nurse both stand
    /// behind a desk, so none of the four tiles around either is walkable and the approach every
    /// other person takes cannot reach them. `IsSpriteOrSignInFrontOfPlayer` doubles the talking
    /// range over exactly these tiles, so the second tile out is a place to stand and face from.
    ///
    /// The default is `false` -- no counters anywhere -- which narrows to the ordinary approach.
    fn counter_tile(&mut self, _x: u8, _y: u8) -> bool {
        false
    }

    /// Whether the cartridge has pushed the fly off this tile of the loaded map.
    ///
    /// [`PushedLedger`] is the evidence and the measurement. The default is `false`: a state that
    /// cannot answer has found no such tile, which is a fresh session.
    fn pushed_tile(&mut self, _x: u8, _y: u8) -> bool {
        false
    }

    /// The whole loaded map's walkability, when the cartridge's tables can be decoded.
    ///
    /// `docs/design/macros.md` section 15, the operator 2026-09-22: "the frontier and warp macros
    /// need to be map aware: A* over walkable tiles." [`GameState::walkable`] answers for the
    /// ten-by-nine window of the screen buffer and [`super::state::Walkable::Unknown`] for
    /// everything else, so before this every walk planned through guesses, re-planned at every
    /// window edge, and `GO FRONTIER` aimed at whatever unstood ground happened to be on screen.
    ///
    /// The default is `None`, which is this trait's usual narrowing and here it is also the
    /// documented fallback: [`super::path::route`] and [`super::path::frontier`] use the window
    /// predicate when the grid is absent, exactly as they did before, and
    /// [`crate::pokemon_red::state::GridRefusal`] is what says why it is absent on a frame the
    /// reader could not decode.
    fn map_grid(&mut self) -> Option<std::sync::Arc<MapGrid>> {
        None
    }

    /// Whether this run has already been into `area`'s mart or Pokémon Center.
    ///
    /// Section 13's `areaVisited(kind, area)`: **one visit per area per run**, so the errand is
    /// paid once and never loops. Written when the fly is observed standing on the building's own
    /// map, which is the moment the errand is discharged -- a purchase or a heal marks nothing,
    /// because what the errand asked for was the visit.
    ///
    /// Session state beside [`MacroState::talked`] and [`MacroState::blocked`], owned by
    /// [`super::driver::PokemonPalette`] and never checkpointed. A restored run offers every
    /// errand once more, which is the honest answer for a ledger that did not survive.
    ///
    /// The default is `false` -- nothing visited -- which is a fresh run.
    fn area_visited(&mut self, _kind: Amenity, _area: u8) -> bool {
        false
    }

    /// Whether `GO OUT`, `GO WARP` or `GO ROUTE` has already been through this exit.
    ///
    /// Section 3: "visit sets for `GO EXIT` are the adapter's existing per-map exploration
    /// ledger", and section 9.1 splits `GO EXIT` into three macros over the same ledger.
    fn exit_visited(&mut self, _exit: ExitId) -> bool {
        false
    }

    /// Whether this run has stood on this tile of the current map: `GO FRONTIER`'s question
    /// (section 9).
    ///
    /// The default is `false`, i.e. no ground covered, which makes every walkable tile frontier.
    /// That is the narrowing this trait's defaults all are: `GO FRONTIER` still only ever walks
    /// to a tile it can see, and with no ledger it walks to the nearest one it is not standing on
    /// rather than pretending to know where the run has been.
    fn tile_visited(&mut self, _x: u8, _y: u8) -> bool {
        false
    }

    /// Whether this run has ever been on `map`: `GO ROUTE`'s "a door into a building whose
    /// interior is unvisited" (section 9.1).
    fn map_visited(&mut self, _map: u8) -> bool {
        false
    }

    /// Whether this run has already talked to `target` on the map that is loaded.
    ///
    /// `docs/design/macros.md` section 12's talked ledger, and the observable that ends the
    /// town loop: "nearest untalked" used to mean "nearest thing with a tile beside it this run
    /// has never stood on", which never closes for a villager standing in the open, so `GO NPC`
    /// walked the fly back to the same person for ever (`infra/docs/macros-bench.md`, 2026-09-16).
    /// A ledger entry is written when a `TALK` *finishes* facing the thing, which is the only
    /// moment the press is known to have happened.
    ///
    /// Session state rather than run state: it lives in the executor layer
    /// ([`super::driver::PokemonPalette`]) and never reaches the checkpoint, because the
    /// checkpoint's format is a compatibility string and this question is not worth moving it.
    /// A restored run offers every person once more, which is the honest answer for a ledger that
    /// did not survive.
    ///
    /// The default is `false` -- nothing talked to -- which is a fresh session.
    /// Whether the cartridge is driving the player instead of the fly.
    ///
    /// `pokemon_red::state::controllable` inverted: a non-zero `wJoyIgnore` or
    /// `wSimulatedJoypadStatesIndex`, `wStatusFlags5`'s scripted-movement or joypad-disabled bits,
    /// a warp in flight, a ledge hop. It is **false during an ordinary text box** — measured at the
    /// Viridian gate, where a plain conversation reads `joy=0 sim=0 flags5=0x00` and the gate's own
    /// push-back reads `sim=1 flags5=0x80` (`infra/docs/macros-traps.md`, 2026-09-17) — which is
    /// what makes it the test for "the conversation ended with the game moving the fly" rather than
    /// a test for "a conversation happened".
    ///
    /// The default is `false`: a state that cannot answer has nothing driving the player.
    fn scripted(&mut self) -> bool {
        false
    }

    fn talked(&mut self, _target: TalkTarget) -> bool {
        false
    }

    /// Whether a text box is open at all: `wFontLoaded`'s bit, and nothing drawn.
    ///
    /// The one thing that tells a screen with words on it from a frame of the overworld the
    /// cartridge happens to be driving, and [`super::palette::scene_set`] deals
    /// [`Scene::Unknown`]'s pad on it (**section 12.13**). `Unknown` is two different states
    /// wearing one name: a screen this crate cannot name -- the Pokedex, the trainer card,
    /// OPTION -- where `NEXT` and `BACK` are the A and B that leave it; and a *scripted* overworld
    /// frame, where `scene::detect` falls through to `Unknown` because the buttons are not
    /// reaching the player, and where an A or a B press is a press into somebody else's script.
    ///
    /// The default is `false`, which narrows: with no reading, `Unknown` deals nothing and the
    /// fly waits, which is what the doctrine says a scene with nothing to press does.
    fn text_open(&mut self) -> bool {
        false
    }

    /// Whether the box on screen is the two-option YES/NO prompt rather than a plain text box.
    ///
    /// `pokemon_red::state::yes_no_prompt`: the border `DisplayTwoOptionMenu` draws plus the
    /// cursor it parks inside it, surveyed on the cartridge (`docs/design/macros.md` section
    /// 12.12). It is what tells the *one* frame of the nurse's conversation that is a choice from
    /// the forty-five that are text, and the pad is dealt differently for it -- on a choice, an A
    /// press *is* `YES`, so `NEXT` is the same press under another name (12.10).
    ///
    /// The default is `false`: a state that cannot answer has no choice open, which leaves the
    /// dialog pad exactly what it has always been.
    fn yes_no_prompt(&mut self) -> bool {
        false
    }

    /// Whether `target` is inside its blocked-target exclusion window on the map that is loaded.
    ///
    /// Written when `GO ITEM`, `GO NPC`, `GO OBJECTIVE`, `GO OUT`, `GO WARP`, `GO ROUTE` or
    /// `GO FRONTIER` aborts `Blocked` or `Timeout`, and true for [`BLOCKED_MINUTES_DEFAULT`]
    /// brain minutes after that. The observable it closes (Viridian City, 2026-09-16): the
    /// nearest object was unreachable from where the fly stood, so `GO ITEM` started and blocked,
    /// started and blocked, once per hold for forty-five minutes -- because "nearest untalked"
    /// re-chose the same unreachable thing every time and nothing recorded that the walk to it
    /// had already failed.
    ///
    /// A *window* rather than a permanent entry, because "unreachable" is a fact about where the
    /// fly was standing and not about the target: a ledge, a closed door or a person in a doorway
    /// all stop being in the way once the fly has moved, and a target excluded for ever would
    /// lose the map its item. Ten brain minutes is long enough that the macro has to do something
    /// else first and short enough that the thing is not forgotten.
    ///
    /// Session state, like [`MacroState::talked`]: it lives in the executor layer
    /// ([`super::driver::PokemonPalette`]) and never reaches the checkpoint.
    fn blocked(&mut self, _target: TargetKey) -> bool {
        false
    }

    /// Whether `GO ITEM` or `GO NPC` has already arrived at `target` and faced it this session.
    ///
    /// The other half of the same stall: `GO NPC` walked to the same villager again and again,
    /// reporting `done` every time, because only a *completed* `TALK` marks a person talked and
    /// the fly is free never to press it. Arriving and facing is what `GO NPC` promises, so
    /// arriving and facing is what retires the target -- for the rest of the session, since
    /// nothing about a person the fly has already stood in front of changes by standing there
    /// again. A later `TALK` writes the talked ledger instead, which excludes it anyway.
    ///
    /// No window on this one: a reached target is not a failure to retry, it is a job done.
    fn reached(&mut self, _target: TargetKey) -> bool {
        false
    }

    /// Where the ladder's next unreached rung is, when the adapter's rung catalog knows.
    ///
    /// `None` leaves `GO OBJECTIVE` unbound, which is section 9's own fall-through: the plan's
    /// next entry takes rank 0 and nothing is invented.
    fn objective(&mut self) -> Option<Objective> {
        None
    }
}

/// What this session has already talked to, as the macros ask it.
///
/// Separate from [`crate::macros::RunLedger`], which is the adapter's lifetime exploration state,
/// because this one is neither the adapter's nor lifetime: it is written by the executor when a
/// `TALK` finishes and it dies with the process. `docs/design/macros.md` section 12 puts it here
/// deliberately -- the checkpoint's format is pinned to a compatibility string and a per-session
/// convenience is not worth moving it.
pub trait TalkLedger {
    /// Whether a `TALK` has finished facing `target` on `map` in this session.
    fn talked(&self, map: u8, target: TalkTarget) -> bool;
}

/// A ledger that has recorded nothing: every person and every sign untalked.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTalk;

impl TalkLedger for NoTalk {
    fn talked(&self, _map: u8, _target: TalkTarget) -> bool {
        false
    }
}

/// The session's talked ledger: every (map, thing) a `TALK` has finished facing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Talked(std::collections::BTreeSet<(u8, TalkTarget)>);

impl Talked {
    /// Record a finished `TALK`. Idempotent.
    pub fn record(&mut self, map: u8, target: TalkTarget) {
        self.0.insert((map, target));
    }

    /// How many things this session has talked to, for a log line and the tests.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TalkLedger for Talked {
    fn talked(&self, map: u8, target: TalkTarget) -> bool {
        self.0.contains(&(map, target))
    }
}

/// The ground this session has watched the fly stand on, as `GO FRONTIER` asks it.
///
/// The second half of [`MacroState::tile_visited`], and the fix for the 2026-09-17 gate-house
/// stall (`infra/docs/macros-traps.md` row 32). The adapter's `exploration` ledger is the first
/// half and it is a *reward* ledger: `sample` inserts a coordinate only on a frame its payout gate
/// accepts, and that gate rejects `wMovementFlags & 0xc7` — bits 0, 1 and 2 are
/// `BIT_STANDING_ON_DOOR`, `BIT_EXITING_DOOR` and `BIT_STANDING_ON_WARP`. So **a doormat is a tile
/// the reward ledger can never record**, and a doormat is walkable, standable ground: every warp
/// tile in the game stayed a frontier for ever, and the pad in Viridian Forest's south gate was
/// `GO FRONTIER` and nothing else for four hours, one tile per macro, once per hold.
///
/// The gate is the reward rule's own and it stays exactly as it is — the catalog, the payouts and
/// the checkpoint are not this file's business. What changes is that the macro layer keeps its own
/// answer to its own question: session state in the executor layer beside [`Talked`] and
/// [`Targets`], written once per frame from the tile the fly is standing on, never checkpointed.
/// A restored run starts with the adapter's lifetime ground and re-learns the doormats it stands
/// on, which is the honest answer for a ledger that did not survive.
///
/// It can only ever make the frontier *smaller*: `tile_visited` is the two ledgers OR'd, so no
/// tile either one has recorded is offered as new ground.
pub trait StoodLedger {
    /// Whether the fly has been seen standing on `tile` of `map` this session.
    fn stood(&self, map: u8, tile: Tile) -> bool;
}

/// A ledger that has watched nothing: no ground covered.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoStood;

impl StoodLedger for NoStood {
    fn stood(&self, _map: u8, _tile: Tile) -> bool {
        false
    }
}

/// The session's own record of where the fly has stood.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stood(std::collections::BTreeSet<(u8, Tile)>);

impl Stood {
    /// Record the tile the fly is standing on. Idempotent.
    pub fn record(&mut self, map: u8, tile: Tile) {
        self.0.insert((map, tile));
    }

    /// How much ground this session has watched, for a log line and the tests.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl StoodLedger for Stood {
    fn stood(&self, map: u8, tile: Tile) -> bool {
        self.0.contains(&(map, tile))
    }
}

/// Tiles the cartridge has pushed the fly off, as the macros ask it.
///
/// **The Viridian private-property tile** (2026-09-17, `infra/docs/macros-traps.md` row 37).
/// `ViridianCityCheckGotPokedexScript` fires on *every frame* the fly stands on (19, 9) of
/// Viridian City without the Pokédex: it prints "This is private property!" and walks the player
/// down a tile. The sleeping old man is at (18, 9) and `approach` offers the four tiles around
/// him, one of which is that tile — so `GO NPC` walked onto it, got a box, the fly advanced the
/// box, and the walk went back. Measured from the release container's own checkpoint: **53,266 of
/// 54,377 text-box frames in twenty brain minutes on that one tile**, 991 of them answered `YES`.
///
/// The blocked ledger could not close it. It is keyed on the *target* -- the old man -- so it
/// excluded a villager for ten brain minutes and said nothing about the ground, and the window
/// reopened. A scripted push-back is a fact about the **tile**, which is why this ledger is
/// separate and, unlike [`Targets`], has **no window**: the map does not stop being like that.
///
/// Session state beside [`Talked`] and [`Stood`], never checkpointed; a restored run walks onto
/// the tile once more and learns it again, which is the honest answer for a ledger that did not
/// survive.
pub trait PushedLedger {
    /// Whether the cartridge has pushed the fly off `tile` of `map` this session.
    fn pushed(&self, map: u8, tile: Tile) -> bool;
}

/// A ledger that has recorded nothing: no tile has pushed the fly anywhere.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPushed;

impl PushedLedger for NoPushed {
    fn pushed(&self, _map: u8, _tile: Tile) -> bool {
        false
    }
}

/// The session's record of the tiles the cartridge drives the fly off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pushed(std::collections::BTreeSet<(u8, Tile)>);

impl Pushed {
    /// Record a scripted push-back. Idempotent.
    pub fn record(&mut self, map: u8, tile: Tile) {
        self.0.insert((map, tile));
    }

    /// How many such tiles this session has found, for a log line and the tests.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl PushedLedger for Pushed {
    fn pushed(&self, map: u8, tile: Tile) -> bool {
        self.0.contains(&(map, tile))
    }
}

/// Which of an area's errands this run has discharged, as the macros ask it.
///
/// `docs/design/macros.md` section 13. Session state beside [`Talked`] and [`Stood`], for exactly
/// the same reason: it is a fact about this run's walking rather than about the save, and the
/// checkpoint's format is pinned to a compatibility string that a per-session convenience is not
/// worth moving.
pub trait AreaLedger {
    /// Whether the fly has been seen inside `area`'s building of this kind this run.
    fn visited(&self, kind: Amenity, area: u8) -> bool;
}

/// A ledger that has recorded nothing: every errand still outstanding.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAreas;

impl AreaLedger for NoAreas {
    fn visited(&self, _kind: Amenity, _area: u8) -> bool {
        false
    }
}

/// The session's errand ledger: every `(kind, area)` the fly has been inside.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Areas(std::collections::BTreeSet<(Amenity, u8)>);

impl Areas {
    /// Record a visit. Idempotent, which is what "once per area per run" is made of.
    pub fn record(&mut self, kind: Amenity, area: u8) {
        self.0.insert((kind, area));
    }

    /// How many errands this run has discharged, for a log line and the tests.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AreaLedger for Areas {
    fn visited(&self, kind: Amenity, area: u8) -> bool {
        self.0.contains(&(kind, area))
    }
}

/// Brain minutes a blocked or timed-out target is excluded from its macro's target choice.
///
/// `docs/design/macros.md` section 12's fix for the Viridian stall, and the operator's number. Tunable
/// with [`BLOCKED_MINUTES_ENV`] because it is the one figure in the pair that is a judgement: too
/// short and the macro walks back into the same wall, too long and a map's only item is forgotten
/// for an hour.
pub const BLOCKED_MINUTES_DEFAULT: f64 = 10.0;

/// The environment variable that overrides [`BLOCKED_MINUTES_DEFAULT`].
///
/// Read once, when the session's ledger is built. A value that is not a finite positive number is
/// ignored with a warning rather than clamped to something invented.
pub const BLOCKED_MINUTES_ENV: &str = "FLY_MACRO_BLOCKED_MINUTES";

const MINUTE_MS: f64 = 60_000.0;

/// Frame-cap timeouts one target may cost a *closing* walk before it is excluded anyway.
///
/// Section 4 gives a walk three failed steps before it is `Blocked`; this is the same number at the
/// macro's own scale, and it is what bounds a walk that keeps the cap busy without ever arriving.
/// A `Timeout` that ended no nearer than it began excludes its target at once -- ten seconds of
/// buttons and no ground gained is a fact about the target. A `Timeout` that got closer is a walk
/// that is simply longer than ten seconds, so it costs one strike and the next hold resumes it;
/// three of them is thirty brain seconds of walking, which is wider than any town on the cartridge,
/// so a target still not reached by then is one to leave alone for a while.
///
/// Both readings were measured. "Did the player move" alone turned the Viridian loop into a
/// different one: the A* re-plans every tile over a walkable predicate whose window is the screen,
/// so a walk can step between two tiles for the whole cap, move every time, and arrive nowhere --
/// twelve such macros every two brain minutes over two tiles (`infra/docs/macros-traps.md`).
pub const TIMEOUT_STRIKES: u32 = 3;

/// The two target ledgers of `docs/design/macros.md` section 12, as the macros ask them.
///
/// Both are session state and neither is checkpointed, for the same reason the talked ledger is
/// not: a restored run offers every target once more, which is the honest answer for a ledger
/// that did not survive.
pub trait TargetLedger {
    /// Whether `target` on `map` is inside its blocked-target exclusion window.
    fn blocked(&self, map: u8, target: TargetKey) -> bool;


    /// Whether `target` on `map` has been arrived at and faced this session.
    fn reached(&self, map: u8, target: TargetKey) -> bool;
}

/// Ledgers that have recorded nothing: every target still a candidate.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTargets;

impl TargetLedger for NoTargets {
    fn blocked(&self, _map: u8, _target: TargetKey) -> bool {
        false
    }

    fn reached(&self, _map: u8, _target: TargetKey) -> bool {
        false
    }
}

/// The session's blocked- and reached-target ledgers, and the brain clock they are read against.
///
/// One type for both halves because they are written from the same place (the executor, as a
/// macro finishes), read from the same place (a macro's target choice) and owned by the same one
/// ([`super::driver::PokemonPalette`]). The clock is carried here rather than passed per question
/// so that a `blocked` asked twice in one frame cannot answer differently.
#[derive(Debug, Clone, PartialEq)]
pub struct Targets {
    /// (map, target) -> the brain millisecond the abort was recorded at.
    blocked: std::collections::BTreeMap<(u8, TargetKey), f64>,
    /// (map, target) -> frame-cap timeouts a closing walk has spent on it, up to
    /// [`TIMEOUT_STRIKES`].
    strikes: std::collections::BTreeMap<(u8, TargetKey), u32>,
    /// (map, target) -> the brain millisecond a walk arrived at it and faced it.
    ///
    /// A window rather than a retirement, for the same reason `blocked` is one
    /// (`docs/design/macros.md` section 12.4): **arriving is not talking.** 12.1 retired a reached
    /// target for the session, to stop the fly walking to one villager for forty-five minutes —
    /// and that also retired every person the fly walked to and then wandered away from without
    /// pressing A, with nothing able to bring it back. Oak is the case that found it: the fly
    /// reached his lab in 1.4 brain minutes, stood in front of him, chose something else, and
    /// could never be offered him again — so the parcel stayed undelivered and the road north
    /// stayed shut. The window keeps the loop bounded at one walk per target per window while
    /// leaving the conversation available.
    reached: std::collections::BTreeMap<(u8, TargetKey), f64>,
    /// The brain clock of the frame being decided, from [`super::driver::PokemonPalette`].
    now_ms: f64,
    /// How long an entry excludes its target, in brain milliseconds.
    window_ms: f64,
}

impl Default for Targets {
    fn default() -> Self {
        Self::with_minutes(BLOCKED_MINUTES_DEFAULT)
    }
}

impl Targets {
    /// The session's ledgers, with the exclusion window [`BLOCKED_MINUTES_ENV`] asks for.
    pub fn new() -> Self {
        let minutes = match std::env::var(BLOCKED_MINUTES_ENV) {
            Err(_) => BLOCKED_MINUTES_DEFAULT,
            Ok(raw) => match raw.trim().parse::<f64>() {
                Ok(value) if value.is_finite() && value > 0.0 => value,
                // Not clamped to something invented: an unreadable knob is the default, said out
                // loud, because a silently halved exclusion window is a stall nobody can explain.
                _ => {
                    // This crate has no logger of its own -- `flysim` owns tracing -- so the
                    // complaint goes to stderr, where the unit's journal picks it up.
                    eprintln!(
                        "{BLOCKED_MINUTES_ENV}={raw:?} is not a positive number of brain minutes; \
                         using {BLOCKED_MINUTES_DEFAULT}"
                    );
                    BLOCKED_MINUTES_DEFAULT
                }
            },
        };
        Self::with_minutes(minutes)
    }

    /// Ledgers with an explicit window, for the tests and for [`Targets::new`].
    pub fn with_minutes(minutes: f64) -> Self {
        Self {
            blocked: std::collections::BTreeMap::new(),
            strikes: std::collections::BTreeMap::new(),
            reached: std::collections::BTreeMap::new(),
            now_ms: 0.0,
            window_ms: minutes * MINUTE_MS,
        }
    }

    /// The brain clock this frame is being decided at.
    pub fn clock(&mut self, now_ms: f64) {
        if now_ms.is_finite() {
            self.now_ms = now_ms;
        }
    }

    /// The exclusion window in brain minutes, for a log line and the tests.
    pub fn minutes(&self) -> f64 {
        self.window_ms / MINUTE_MS
    }

    /// Record a `Blocked` or `Timeout` abort against the target it was aimed at.
    ///
    /// Re-recording restarts the window, which is the honest reading: the walk failed *again*.
    /// Entries whose window has closed are dropped as they are passed, so the map stays the size
    /// of what is currently excluded rather than of everything a session ever failed at.
    pub fn record_blocked(&mut self, map: u8, target: TargetKey) {
        let now = self.now_ms;
        let window = self.window_ms;
        self.blocked.retain(|_, at| now - *at < window);
        self.blocked.insert((map, target), now);
        // The exclusion supersedes the strikes: when the window closes the target comes back with
        // a clean slate rather than one strike from being excluded again.
        self.strikes.remove(&(map, target));
    }

    /// Record that a walk to `target` spent the frame cap, and exclude the target when that is what
    /// the cap means.
    ///
    /// `closer` is whether the walk ended nearer a goal than it began. A walk that did not gets its
    /// target excluded at once; one that did costs a strike, and [`TIMEOUT_STRIKES`] of them
    /// exclude it. Either way the fly keeps choosing: this is the *target* choice inside the macro,
    /// never the choice of macro (`docs/design/macros.md` section 12).
    pub fn record_timeout(&mut self, map: u8, target: TargetKey, closer: bool) {
        if !closer {
            self.record_blocked(map, target);
            return;
        }
        let strikes = self.strikes.entry((map, target)).or_insert(0);
        *strikes += 1;
        if *strikes >= TIMEOUT_STRIKES {
            self.record_blocked(map, target);
        }
    }

    /// Record that `GO ITEM` or `GO NPC` arrived at `target` and faced it.
    ///
    /// Re-recording restarts the window, exactly as [`Targets::record_blocked`] does: the fly
    /// arrived *again*.
    pub fn record_reached(&mut self, map: u8, target: TargetKey) {
        let now = self.now_ms;
        let window = self.window_ms;
        self.reached.retain(|_, at| now - *at < window);
        self.reached.insert((map, target), now);
        self.strikes.remove(&(map, target));
    }

    /// Frame-cap strikes standing against `target` on `map`, for a log line and the tests.
    pub fn strikes(&self, map: u8, target: TargetKey) -> u32 {
        self.strikes.get(&(map, target)).copied().unwrap_or(0)
    }

    /// How many targets are excluded right now, and how many have been reached, for a log line.
    pub fn len(&self) -> (usize, usize) {
        let standing = |entries: &std::collections::BTreeMap<(u8, TargetKey), f64>| {
            entries.values().filter(|at| self.now_ms - **at < self.window_ms).count()
        };
        (standing(&self.blocked), standing(&self.reached))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == (0, 0)
    }
}

impl TargetLedger for Targets {
    fn blocked(&self, map: u8, target: TargetKey) -> bool {
        // A clock that has gone backwards -- which only a rollback can do, and the loop cancels
        // the running macro then -- leaves the entry standing rather than expiring it early.
        self.blocked.get(&(map, target)).is_some_and(|at| self.now_ms - *at < self.window_ms)
    }

    fn reached(&self, map: u8, target: TargetKey) -> bool {
        self.reached.get(&(map, target)).is_some_and(|at| self.now_ms - *at < self.window_ms)
    }
}
