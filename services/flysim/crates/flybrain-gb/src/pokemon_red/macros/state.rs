//! The game-state interface the macro palette is written against.
//!
//! `docs/design/macros.md` section 8 splits the work: agent A reads WRAM and agent B writes the
//! executor, and this file is the seam. It is deliberately **self-contained** — no `use`, no
//! dependency on anything else in this crate — so that it can be read, copied and compiled on its
//! own while the two halves are built in parallel.
//!
//! Three rules shaped it:
//!
//! - **No raw bytes leave.** Every accessor returns a named, typed value. A caller never sees an
//!   address, a bit mask, a BCD digit or a `$ff` terminator; `docs/design/macros-wram.md` holds
//!   those, one row per symbol, with the pokered evidence for each.
//! - **Every read is `&mut self`,** because reads are memoized per frame behind this trait
//!   (`Emulator` caches each address for the current frame and the Pokémon adapter caches again
//!   per sample). Nothing here mutates the game.
//! - **Absence is a value, not a guess.** A state the cartridge is not currently in reads as
//!   `None`, and a tile whose walkability cannot be known from WRAM reads as
//!   [`Walkable::Unknown`]. A macro is expected to refuse rather than to act on a guess
//!   (`docs/design/macros.md` section 4: an unbound slot or a failed precondition presses
//!   nothing).
//!
//! [`Scene`] lives here, rather than in `pokemon_red/scene.rs` where `macros.md` section 2 names
//! it, for exactly one reason: this file may not depend on that one, and two copies of an enum are
//! not one type. `pokemon_red::scene` re-exports it, so `pokemon_red::scene::Scene` is the path
//! the contract promises and it is this type.

/// Which of the game's interaction modes the player is in, as
/// `docs/design/macros.md` section 2 declares it.
///
/// Detection is from WRAM only, once per game frame, by `pokemon_red::scene::detect`. It is
/// deliberately conservative: a state that cannot be identified is [`Scene::Unknown`], never the
/// nearest guess, because the cost of a wrong scene is a palette of actions that do not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    /// Not playable yet: title, intro, naming screens. No palette; the readout's boot variant
    /// applies, which is what lets Start fire.
    Title,
    /// The player can walk.
    Overworld,
    /// A text box is open and waiting.
    Dialog,
    /// Start menu or one of its submenus, outside battle.
    Menu,
    /// In a battle. `own_turn` is any battle menu waiting for input -- the top-level
    /// FIGHT/PKMN/ITEM/RUN one, the move list, the party list or the bag;
    /// `forced_switch` is the party list the game opens when the active Pokémon has fainted, which
    /// cannot be backed out of.
    Battle { own_turn: bool, forced_switch: bool },
    /// A mart's buy/sell menu.
    Shop,
    /// A PC menu.
    Pc,
    /// Detection failed. Treated like [`Scene::Dialog`] (advance only).
    Unknown,
}

impl Scene {
    /// Whether the fly can be offered a palette at all. False for [`Scene::Title`], where the
    /// readout's boot variant applies instead.
    pub fn playable(self) -> bool {
        !matches!(self, Scene::Title)
    }

    /// Short upper-case name for the screen and the feed (`game.scene`).
    pub fn label(self) -> &'static str {
        match self {
            Scene::Title => "TITLE",
            Scene::Overworld => "OVERWORLD",
            Scene::Dialog => "DIALOG",
            Scene::Menu => "MENU",
            Scene::Battle { forced_switch: true, .. } => "BATTLE SWITCH",
            Scene::Battle { own_turn: true, .. } => "BATTLE TURN",
            Scene::Battle { .. } => "BATTLE",
            Scene::Shop => "SHOP",
            Scene::Pc => "PC",
            Scene::Unknown => "UNKNOWN",
        }
    }
}

/// A facing, for the player and for every NPC sprite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    Down,
    Up,
    Left,
    Right,
}

impl Facing {
    /// The tile offset this facing points at, as `(dx, dy)`.
    pub fn delta(self) -> (i16, i16) {
        match self {
            Facing::Down => (0, 1),
            Facing::Up => (0, -1),
            Facing::Left => (-1, 0),
            Facing::Right => (1, 0),
        }
    }
}

/// Where the player is standing, and which way it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Player {
    /// `wCurMap`.
    pub map: u8,
    pub x: u8,
    pub y: u8,
    pub facing: Facing,
}

/// The current map's size in walkable tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapSize {
    pub width: u8,
    pub height: u8,
}

/// A non-volatile status condition. Sleep carries its remaining turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Healthy,
    Sleep(u8),
    Poison,
    Burn,
    Freeze,
    Paralysis,
}

/// One move slot. An empty slot is `None` in [`Mon::moves`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    /// Move id, as `constants/move_constants.asm` numbers them.
    pub id: u8,
    /// Remaining PP.
    pub pp: u8,
    /// How many PP Ups have been applied, 0 to 3.
    pub pp_up: u8,
}

/// One Pokémon, whether a party member or the active battler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mon {
    /// Party slot, 0-based.
    pub slot: u8,
    /// Species id, as `constants/pokemon_constants.asm` numbers them — the cartridge's
    /// *internal* index, not the Pokédex number. Charmander is `$b0`, not 4. (The reward
    /// adapter's `wPokedexOwned` bitset is by Pokédex number; these two numberings are not the
    /// same and nothing converts between them here.)
    pub species: u8,
    pub level: u8,
    pub hp: u16,
    pub max_hp: u16,
    pub status: Status,
    pub moves: [Option<Move>; 4],
}

impl Mon {
    pub fn fainted(self) -> bool {
        self.hp == 0
    }

    /// HP as a fraction of maximum, 0.0 for a fainted or unreadable Pokémon.
    pub fn hp_fraction(self) -> f64 {
        if self.max_hp == 0 {
            return 0.0;
        }
        f64::from(self.hp) / f64::from(self.max_hp)
    }
}

/// The player's party, in slot order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Party {
    pub mons: Vec<Mon>,
    /// Slot of the Pokémon that is out, during a battle.
    pub active: Option<u8>,
}

impl Party {
    /// The healthiest Pokémon that is neither fainted nor the active one, by HP fraction, ties
    /// broken by slot — `docs/design/macros.md` section 3's "healthiest".
    pub fn healthiest_reserve(&self) -> Option<&Mon> {
        self.mons
            .iter()
            .filter(|mon| !mon.fainted() && Some(mon.slot) != self.active)
            .max_by(|a, b| {
                a.hp_fraction()
                    .total_cmp(&b.hp_fraction())
                    .then(b.slot.cmp(&a.slot))
            })
    }

    /// The Pokémon that is out, during a battle.
    pub fn active_mon(&self) -> Option<&Mon> {
        let active = self.active?;
        self.mons.iter().find(|mon| mon.slot == active)
    }
}

/// The opposing Pokémon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnemyMon {
    /// Species id, the internal index, as [`Mon::species`].
    pub species: u8,
    pub level: u8,
    pub hp: u16,
    pub max_hp: u16,
}

/// What kind of battle is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleKind {
    /// A wild Pokémon: RUN is available.
    Wild,
    /// A trainer: RUN is not.
    Trainer,
}

/// Which menu inside a battle is waiting for input.
///
/// The cursors are already translated out of pokered's own indexing, which differs per menu; the
/// raw geometry is in `docs/design/macros-wram.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BattleMenu {
    /// Nothing is waiting: text, an animation, or the turn resolving.
    None,
    /// FIGHT / PKMN / ITEM / RUN, a 2x2 grid. `cursor` is 0 FIGHT, 1 PKMN, 2 ITEM, 3 RUN: UP and
    /// DOWN move inside a column, LEFT and RIGHT change column.
    Main { cursor: u8 },
    /// The move list. `cursor` is the 0-based move slot when it names one.
    Moves { cursor: Option<u8>, count: u8 },
    /// The party list. `cursor` is the 0-based party slot.
    Party { cursor: u8 },
    /// The bag, opened from a battle's ITEM entry: `wListMenuID` is `ITEMLISTMENU`.
    ///
    /// Not one of the three `docs/design/macros.md` section 12.6 named, and the gap was
    /// observable twice over. First the list needed a *cursor* the scripts can read, which is what
    /// `ITEM` and section 14's `THROW BALL` navigate by (2026-09-17). Then it needed to be the
    /// fly's *turn*: a bag reading as nobody's turn landed on the between-turns row, whose `NEXT`
    /// is the A that advances text and on an open bag is the A that uses an item (12.10).
    Bag { cursor: u8, count: u8 },
}

/// Everything a battle macro needs, when a battle is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battle {
    pub kind: BattleKind,
    /// A battle menu is open and waiting for the fly: the top-level one, the move list, the bag,
    /// or the party list outside a forced switch (`pokemon_red::state::battle`). Every frame with a
    /// cursor accepting input is one of these, which is section 12.10's invariant.
    pub own_turn: bool,
    /// The party list is open because the active Pokémon fainted; it cannot be cancelled.
    pub forced_switch: bool,
    pub menu: BattleMenu,
    /// The Pokémon that is out. It is a copy of its party entry and it is the copy the battle
    /// engine damages, so this is "own HP" in a battle.
    pub own: Option<Mon>,
    pub enemy: Option<EnemyMon>,
}

/// Whether a text box is on screen, and whether it is the kind that waits for a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextBox {
    /// A text display is open: the game has loaded the dialogue font and the player cannot walk.
    pub open: bool,
    /// The full-width dialogue box is drawn along the bottom of the screen. The game is either
    /// printing into it or waiting for A; in both cases A is the button that advances it.
    pub waiting: bool,
}

/// A menu cursor, as `HandleMenuInput` keeps it. Shared by every menu in the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Currently highlighted item, 0-based in the menu's own indexing.
    pub current: u8,
    /// Highest selectable index.
    pub max: u8,
    /// Screen row and column of the first item, which is what identifies *which* menu is up.
    pub top_y: u8,
    pub top_x: u8,
    /// The buttons this menu reacts to, as a Game Boy pad mask. A menu that omits B cannot be
    /// backed out of.
    pub watched_keys: u8,
}

impl Cursor {
    /// Whether B closes this menu.
    pub fn cancellable(self) -> bool {
        const PAD_B: u8 = 1 << 1;
        self.watched_keys & PAD_B != 0
    }
}

/// The start menu, when it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartMenu {
    pub cursor: Cursor,
    /// Number of entries: 7 once the Pokédex is in hand, 6 before.
    pub items: u8,
}

/// Which screen of a mart is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShopScreen {
    /// BUY / SELL / QUIT.
    BuySellQuit,
    /// The priced buy list.
    Buying,
    /// The bag list, for selling.
    Selling,
}

/// A mart, when one is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shop {
    pub screen: ShopScreen,
    pub cursor: Cursor,
}

/// A PC, when one is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pc {
    pub cursor: Cursor,
}

/// One stack in the bag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BagItem {
    /// Item id, as `constants/item_constants.asm` numbers them.
    pub id: u8,
    pub count: u8,
}

/// A visible NPC sprite on the current map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Npc {
    /// Sprite slot, 1 to 15. Slot 0 is the player and is never reported here.
    pub slot: u8,
    /// Picture id, which is the sprite's appearance rather than its identity.
    pub picture: u8,
    pub x: u8,
    pub y: u8,
    pub facing: Facing,
}

impl Npc {
    /// Whether this sprite is a person rather than an object on the floor.
    ///
    /// pokered's sprite list is ordered, and `FIRST_STILL_SPRITE` is where it stops being people:
    /// everything from `SPRITE_POKE_BALL` (`$3d`) up is a four-tile still sprite — a ball, a
    /// fossil, a boulder, a clipboard, a sleeping Snorlax — and `map_sprites.asm` uses exactly
    /// this comparison to tell one from a walker (`constants/sprite_constants.asm`,
    /// `engine/overworld/map_sprites.asm:101`). It is an *appearance* test, which is all a picture
    /// id can carry: the sleeping gambler is furniture by this rule and the Snorlax in the road is
    /// an object, and both of those are things to walk up to and press A at, which is what the
    /// test is for.
    pub fn person(self) -> bool {
        const FIRST_STILL_SPRITE: u8 = 0x3d;
        self.picture < FIRST_STILL_SPRITE
    }
}

/// One sign on the current map: a `bg_event`, i.e. a tile that prints text when it is faced.
///
/// Signs, bookshelves, televisions, maps on a wall and the Pokémon-centre notice boards are all
/// this. They are not sprites and they are not walkable, so nothing about them reaches
/// [`GameState::npcs`] or [`GameState::walkable`]; the tile named here is the tile to *face*, not
/// one to stand on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sign {
    pub x: u8,
    pub y: u8,
    /// Text id the game prints for it, which is its identity on this map and nothing more.
    pub text_id: u8,
}

/// Whether a tile can be walked onto.
///
/// `Unknown` is load-bearing: the tile ids a walkability test needs live in the screen buffer, so
/// only the 10x9 window around the player can be answered at all, and only while the overworld is
/// on screen. See `docs/design/macros-wram.md`, "The walkable predicate and its window".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walkable {
    Yes,
    No,
    Unknown,
}

impl Walkable {
    /// True only for [`Walkable::Yes`]: an unknown tile is not a tile to path through.
    pub fn is_walkable(self) -> bool {
        matches!(self, Walkable::Yes)
    }
}

/// One entry of the current map's warp table: a door, a staircase, a cave mouth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Warp {
    pub x: u8,
    pub y: u8,
    /// Which warp of the destination map the player arrives at.
    pub destination_warp: u8,
    /// Destination map id. `$ff` means "the map the player came from".
    pub destination_map: u8,
}

/// Which edges of the current map lead to another map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Connections {
    pub north: bool,
    pub south: bool,
    pub east: bool,
    pub west: bool,
}

impl Connections {
    pub fn any(self) -> bool {
        self.north || self.south || self.east || self.west
    }
}

/// Everything the macro palette may know about the game.
///
/// One implementation reads the live cartridge (`pokemon_red::state::PokeState`); tests build
/// others from synthetic WRAM. Nothing here presses a button, chooses an action or knows what a
/// macro is.
pub trait GameState {
    /// Which scene the palette should be dealt for.
    fn scene(&mut self) -> Scene;

    /// Where the player is, when the overworld is loaded.
    fn player(&mut self) -> Option<Player>;

    /// The current map's size in walkable tiles, when the overworld is loaded.
    fn map_size(&mut self) -> Option<MapSize>;

    /// The party, in slot order. Empty before the starter.
    ///
    /// One frame-level caveat, measured on the cartridge: `wPartyCount` leads the party structs.
    /// `AddPartyMon` writes the count first and fills the 44 bytes over the frames that follow —
    /// 3,245 of them when Oak hands over the starter, because the gift is spread across the
    /// script — so a member can be reported with species 0, level 0 and no HP. A caller that
    /// cares should require a non-zero species, which is the check the ROM-gated test uses.
    fn party(&mut self) -> Party;

    /// The battle, when one is running.
    fn battle(&mut self) -> Option<Battle>;

    /// Whether a text box is open, and whether it waits for a button.
    fn text_box(&mut self) -> TextBox;

    /// The start menu, when it is open.
    fn start_menu(&mut self) -> Option<StartMenu>;

    /// The mart, when one is open.
    fn shop(&mut self) -> Option<Shop>;

    /// The PC, when one is open.
    fn pc(&mut self) -> Option<Pc>;

    /// Money, in whole units.
    fn money(&mut self) -> u32;

    /// The bag, in bag order.
    fn bag(&mut self) -> Vec<BagItem>;

    /// Visible NPC sprites on the current map.
    ///
    /// Every sprite slot the map loaded, people and objects alike: an item ball on the floor and
    /// the starter Pokéballs on Oak's table are `object_event`s like any villager and they occupy
    /// the same sixteen slots. [`Npc::person`] is the test that separates the two.
    fn npcs(&mut self) -> Vec<Npc>;

    /// The current map's signs, i.e. its `bg_event` text tiles.
    ///
    /// Empty on a map with none. Required rather than defaulted like the rest of this trait: an
    /// implementation that cannot answer should say "no signs" in its own words, where the reason
    /// is visible, and not inherit it from here.
    fn signs(&mut self) -> Vec<Sign>;

    /// Whether the player could stand on this tile of the current map.
    fn walkable(&mut self, x: u8, y: u8) -> Walkable;

    /// The current map's warp table.
    fn warps(&mut self) -> Vec<Warp>;

    /// Which of the current map's edges lead somewhere.
    fn connections(&mut self) -> Connections;
}
