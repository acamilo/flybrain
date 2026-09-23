//! What the engagement rules of `pokered-unique8-v7` read: indoors, a conversation the fly
//! opened, and an item picked up.
//!
//! The operator's decision of 2026-09-23 (`docs/rewards-learning.md`, "Engagement rewards"):
//! pay the fly for engaging with what is inside a building -- `talk` and `item` -- and stop
//! paying `boundary` for walking back out of one. Everything here is a read of game memory
//! after a frame; nothing chooses, biases or presses a button, and nothing is checkpointed.
//! The lifetime ledgers the payouts are keyed into are the adapter's own `seen` set, in
//! [`super::PokemonRedReward`].

use crate::adapter::MemoryReader;

use super::macros::state::{Facing, Player};
use super::state::{self, poke};
use super::symbols::ram;

/// Tileset ids, `constants/tileset_constants.asm` at [`super::symbols::POKERED_COMMIT`]. Only the
/// ones the indoor rule names.
pub mod tileset {
    pub const OVERWORLD: u8 = 0;
    pub const FOREST: u8 = 3;
    pub const UNDERGROUND: u8 = 11;
    pub const SHIP_PORT: u8 = 14;
    pub const CAVERN: u8 = 17;
    pub const PLATEAU: u8 = 23;
    /// `DEF NUM_TILESETS EQU const_value`: 24 tilesets, ids 0 to 23.
    pub const COUNT: u8 = 24;
}

/// Whether a map with this tileset is **inside a building**, by the cartridge's own two tables.
///
/// - `CheckIfInOutsideMap` (`home/overworld.asm`) is the game's own outdoor test: tileset
///   `OVERWORLD` or `PLATEAU` is "a town or route", and `WarpFound2` labels the other branch
///   `.indoorMaps`. On its own that also calls Viridian Forest and every cave indoor.
/// - `BikeRidingTilesets` (`data/tilesets/bike_riding_tilesets.asm`) is the game's list of places
///   a bicycle may be ridden -- `OVERWORLD`, `FOREST`, `UNDERGROUND`, `SHIP_PORT`, `CAVERN` -- and
///   the bike is the one thing the cartridge refuses *inside a building* by rule.
///
/// Indoor is neither: not outside, and not somewhere the bike is allowed. That is every house,
/// mart, Pokémon Center, gym, gate, lab, museum, the S.S. Anne, Silph Co., the Pokémon Tower, the
/// Mansion, the Rocket Hideout and the Indigo Plateau's rooms -- and *not* Viridian Forest, a
/// cave, the Underground Path or Vermilion's dock, whose exits are how the fly gets anywhere.
/// A tileset id past the table is not indoor: an unreadable map is never a reason to withhold
/// `boundary`.
pub fn indoor(tileset: u8) -> bool {
    use tileset::*;
    tileset < COUNT
        && !matches!(
            tileset,
            OVERWORLD | PLATEAU | FOREST | UNDERGROUND | SHIP_PORT | CAVERN
        )
}

/// What a conversation was with, as `DisplayTextID` names it: a sprite slot, or a sign's text id.
///
/// The same split the macros' session `talked` ledger uses, but this is not that ledger: the
/// payout is keyed into the adapter's lifetime `seen` set, which is checkpointed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thing {
    Sprite(u8),
    Sign(u8),
}

/// A conversation the fly opened and the cartridge has now closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conversation {
    pub map: u8,
    pub thing: Thing,
}

impl Conversation {
    /// The `seen` ledger key: one payout per `(map, object)` for the lifetime of the ledger.
    pub fn key(&self) -> String {
        match self.thing {
            Thing::Sprite(slot) => format!("talk:{}:sprite:{slot}", self.map),
            Thing::Sign(id) => format!("talk:{}:sign:{id}", self.map),
        }
    }

    pub fn label(&self) -> String {
        match self.thing {
            Thing::Sprite(slot) => format!("TALKED TO #{slot} IN AREA {}", self.map),
            Thing::Sign(id) => format!("READ SIGN #{id} IN AREA {}", self.map),
        }
    }
}

/// Samples after the box opens by which `DisplayTextID` has certainly written its argument.
///
/// `DisplayTextIDInit` sets the font bit and then loads the font's tiles into VRAM, which takes
/// frames; `DisplayTextID` copies its argument into `wSpriteIndex` only after that. Measured on
/// the cartridge (`tests/rom_engage.rs`, the Viridian Forest north gate): the bit rose on one
/// frame and the argument arrived **twenty frames** later. Until then the byte still holds
/// whatever the *last* text was about -- which may well be the person in front of the fly, from
/// a conversation that did not pay -- so it is not read as this conversation's argument until it
/// has changed, or until this many samples have gone by, after which an unchanged byte means the
/// new text is about the same thing as the last one. More than twice the measured delay.
const ARGUMENT_SETTLED: u8 = 45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Armed {
    map: u8,
    x: u32,
    y: u32,
    /// The fly had the joypad and was standing still: [`state::controllable`] and a zero
    /// `wWalkCounter`. The overworld only reads an A press in that state.
    ready: bool,
    /// `wSpriteIndex` before the box opened: the previous text's argument.
    stale: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Opening {
    map: u8,
    samples: u8,
    stale: u8,
    /// The bottom dialogue box has been on screen during this opening.
    dialogue: bool,
}

/// The `talk` rule's per-frame watch. Transient: a restore or a rollback clears it, so a
/// conversation in flight at a checkpoint pays nothing, which is the conservative answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TalkWatch {
    armed: Option<Armed>,
    opening: Option<Opening>,
    pending: Option<Conversation>,
}

impl TalkWatch {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// A sample that is not the overworld -- a battle. Whatever the fly was doing before it is
    /// not what opens the next text box, so the arming is dropped; a conversation already open
    /// (a trainer the fly spoke to) stays pending and pays when its box is finally closed.
    pub fn interrupt(&mut self) {
        self.armed = None;
        self.opening = None;
    }

    /// One overworld sample (`wIsInBattle` zero, every playability gate passed).
    ///
    /// A conversation pays when all of this holds, each read out of WRAM:
    ///
    /// 1. **The fly started it.** On the last sample before the text box opened
    ///    (`wFontLoaded` bit 0 rising) the fly had the joypad -- no ignored buttons, no simulated
    ///    input, no scripted movement ([`state::controllable`]) -- was standing still
    ///    (`wWalkCounter` zero, which is the only state the overworld reads A in) and stood on the
    ///    tile it is on now. Most script text opens with the joypad already taken, and is not
    ///    `ready`; a map script that runs the frame after a step ends can open text while the fly
    ///    is still `ready`, so it pays only if rule 2 names the thing in front (none in the early
    ///    game; the Fighting Dojo master and the Elite Four once each).
    /// 2. **It is with the thing in front of the fly.** `DisplayTextID` copies its argument into
    ///    `wSpriteIndex` once the font is loaded ([`ARGUMENT_SETTLED`] has the timing, and why
    ///    the byte is read only once it has changed or settled), in the bottom dialogue box --
    ///    the start menu is drawn elsewhere. A value up to `wNumSprites` is a sprite slot, and that sprite must stand
    ///    on the tile the player faces -- or one further, across a counter, on a tileset that has
    ///    counter tiles (`IsSpriteOrSignInFrontOfPlayer`'s `.extendRangeOverCounter`). A larger
    ///    value is a text id, and it must be the text id of the sign on the tile the player faces.
    ///    An item ball is a sprite but not a person: it pays `item`, not `talk`.
    /// 3. **Indoors**, by [`indoor`], on the map the box opened on.
    /// 4. **It finished.** The box closed again on the same map. A conversation that ends in a
    ///    warp, a restore or a blackout pays nothing.
    ///
    /// Returns the conversation on the sample the box closes; the caller pays it once per key.
    pub fn observe(
        &mut self,
        memory: &mut dyn MemoryReader,
        map: u8,
        x: u32,
        y: u32,
    ) -> Option<Conversation> {
        let open = memory.read8(ram::wFontLoaded) & poke::BIT_FONT_LOADED != 0;
        if !open {
            // A box that closes before its argument was ever seen to change: a short text about
            // the same thing as the last one. The argument was written before a letter printed,
            // so it is this text's; the dialogue box must have been drawn for it.
            let argument = memory.read8(ram::wSpriteIndex);
            if let Some(opening) = self.opening.take()
                && opening.dialogue
                && opening.map == map
                && let Some(thing) = thing_named(memory, argument)
            {
                self.pending = Some(Conversation { map, thing });
            }
            let finished = self
                .pending
                .take()
                .filter(|conversation| conversation.map == map);
            let ready = state::controllable(memory) && memory.read8(ram::wWalkCounter) == 0;
            let stale = argument;
            self.armed = Some(Armed {
                map,
                x,
                y,
                ready,
                stale,
            });
            return finished;
        }
        if let Some(armed) = self.armed.take()
            && armed.ready
            && armed.map == map
            && armed.x == x
            && armed.y == y
            && self.pending.is_none()
            && indoor(memory.read8(ram::wCurMapTileset))
        {
            self.opening = Some(Opening {
                map,
                samples: 0,
                stale: armed.stale,
                dialogue: false,
            });
        }
        if let Some(opening) = self.opening.as_mut() {
            opening.samples += 1;
            opening.dialogue |= state::text_box(memory).waiting;
            let map = opening.map;
            let argument = memory.read8(ram::wSpriteIndex);
            if argument != opening.stale || opening.samples >= ARGUMENT_SETTLED {
                // One reading, whichever way it goes: the argument this text was opened with,
                // and only while the dialogue box is what is drawn (not the start menu's).
                let dialogue = state::text_box(memory).waiting;
                self.opening = None;
                if dialogue && let Some(thing) = thing_named(memory, argument) {
                    self.pending = Some(Conversation { map, thing });
                }
            }
        }
        None
    }
}

/// The step `facing` points at from `(x, y)`, or `None` off the top or left of the map.
fn ahead(x: u8, y: u8, facing: Facing) -> Option<(u8, u8)> {
    let (dx, dy) = facing.delta();
    let x = u8::try_from(i16::from(x) + dx).ok()?;
    let y = u8::try_from(i16::from(y) + dy).ok()?;
    Some((x, y))
}

/// `wMapSpriteExtraData`'s two bytes for a sprite slot (1-based, as `hSpriteIndex` is).
fn extra_data(memory: &mut dyn MemoryReader, slot: u8) -> (u8, u8) {
    let entry = ram::wMapSpriteExtraData + (u16::from(slot) - 1) * 2;
    (memory.read8(entry), memory.read8(entry + 1))
}

/// Whether sprite `slot` is an item ball: `LoadMapHeader` writes `(item id, 0)` into its extra
/// data for an `ITEM`-flagged `object_event`, `(trainer class, trainer number)` for a `TRAINER`
/// one -- trainer numbers start at 1 -- and two zeroes for everything else.
fn is_item_ball(memory: &mut dyn MemoryReader, slot: u8) -> bool {
    let (item, second) = extra_data(memory, slot);
    item != 0 && second == 0
}

/// `DisplayTextID`'s argument, if it names something the player is facing.
///
/// The caller also asks for the bottom dialogue box ([`state::text_box`]'s `waiting`):
/// `DisplayTextIDInit` draws it for every text id but the start menu's, which it draws at the top
/// right instead.
fn thing_named(memory: &mut dyn MemoryReader, argument: u8) -> Option<Thing> {
    if argument == 0 {
        // TEXT_START_MENU.
        return None;
    }
    let player: Player = state::player(memory)?;
    let one = ahead(player.x, player.y, player.facing)?;
    let sprites = memory.read8(ram::wNumSprites).min(poke::SPRITE_SLOTS - 1);
    if argument <= sprites {
        let npc = state::npcs(memory)
            .into_iter()
            .find(|npc| npc.slot == argument)?;
        let at = (npc.x, npc.y);
        let reached = at == one
            || (ahead(one.0, one.1, player.facing) == Some(at)
                && state::counter_tiles(memory)
                    .iter()
                    .any(|tile| *tile != poke::NO_COUNTER_TILE));
        if reached && !is_item_ball(memory, argument) {
            return Some(Thing::Sprite(argument));
        }
        return None;
    }
    state::signs(memory)
        .into_iter()
        .any(|sign| sign.text_id == argument && (sign.x, sign.y) == one)
        .then_some(Thing::Sign(argument))
}

/// `wToggleableObjectFlags` is `flag_array $100`.
const TOGGLE_BYTES: usize = 32;
/// `wObtainedHiddenItemsFlags` is `flag_array MAX_HIDDEN_ITEMS`, and `MAX_HIDDEN_ITEMS` is 112
/// (`constants/item_constants.asm`).
const HIDDEN_ITEM_BYTES: usize = 14;
/// `wToggleableObjectList` is `ds 16 * 2 + 1`: sixteen `(sprite slot, global index)` pairs and a
/// `$ff` terminator.
const TOGGLE_LIST_ENTRIES: u16 = 16;

/// The two item balls a script *reveals*: `TOGGLE_ROCKET_HIDEOUT_B4F_ITEM_4` (`$87`, the Silph
/// Scope) and `TOGGLE_ROCKET_HIDEOUT_B4F_ITEM_5` (`$88`, the Lift Key), the only `ITEM`
/// `object_event`s `data/maps/toggleable_objects.asm` starts `OFF`, and the only item entries
/// `constants/toggle_constants.asm` does not mark "X, never toggled by a script".
///
/// Every other item ball's bit is clear from a new game until `PickUpItem` sets it, so a set bit
/// is a pickup. These two are set from the start and cleared when Giovanni's defeat shows them,
/// so seeding them as "already taken" would withhold two payouts for ever. They are the only
/// bits the seed leaves out.
pub const SCRIPT_SHOWN_ITEM_BALLS: [u8; 2] = [0x87, 0x88];

/// The cartridge's two "this item has been taken" bitsets, as of one sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemFlags {
    toggles: [u8; TOGGLE_BYTES],
    hidden: [u8; HIDDEN_ITEM_BYTES],
}

fn bit(bytes: &[u8], index: usize) -> bool {
    bytes
        .get(index / 8)
        .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
}

impl ItemFlags {
    pub fn read(memory: &mut dyn MemoryReader) -> Self {
        let mut toggles = [0; TOGGLE_BYTES];
        for (offset, byte) in toggles.iter_mut().enumerate() {
            *byte = memory.read8(ram::wToggleableObjectFlags + offset as u16);
        }
        let mut hidden = [0; HIDDEN_ITEM_BYTES];
        for (offset, byte) in hidden.iter_mut().enumerate() {
            *byte = memory.read8(ram::wObtainedHiddenItemsFlags + offset as u16);
        }
        Self { toggles, hidden }
    }

    /// Ledger keys for every item this state already shows as taken: the seed that stops a
    /// pickup made before the rule existed from paying after a rollback un-takes it.
    ///
    /// Every set toggle bit but [`SCRIPT_SHOWN_ITEM_BALLS`] -- which includes the bits of people
    /// a script has hidden, harmlessly, because only an item ball's key is ever looked up -- and
    /// every set hidden-item bit.
    pub fn seed(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for index in 0..TOGGLE_BYTES * 8 {
            if bit(&self.toggles, index) && !SCRIPT_SHOWN_ITEM_BALLS.contains(&(index as u8)) {
                keys.push(ball_key(index as u8));
            }
        }
        for index in 0..HIDDEN_ITEM_BYTES * 8 {
            if bit(&self.hidden, index) {
                keys.push(hidden_key(index as u8));
            }
        }
        keys
    }
}

/// One item the fly has just picked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pickup {
    pub key: String,
    pub label: String,
}

pub fn ball_key(global: u8) -> String {
    format!("item:{global}")
}

pub fn hidden_key(index: u8) -> String {
    format!("hidden:{index}")
}

/// Items whose "taken" bit rose between `before` and `now`.
///
/// - **An item ball** is one of this map's toggleable sprites (`wToggleableObjectList`) whose
///   extra data says item ([`is_item_ball`]). `PickUpItem` sets its global bit in
///   `wToggleableObjectFlags` through `HideObject`, and only after `GiveItem` succeeded, so a full
///   bag pays nothing. A toggleable that is not an item -- a person a script hides, a legendary
///   after its battle -- is never looked at.
/// - **A hidden item** is a bit of `wObtainedHiddenItemsFlags`, which `FoundHiddenItemText` sets
///   after `GiveItem` succeeded and nothing else in the game writes. Hidden coins have a bitset of
///   their own and are not items.
///
/// A bit that was already set on the previous sample is not a pickup, which is what keeps a
/// restored or seeded state from paying for anything it already holds.
pub fn pickups(memory: &mut dyn MemoryReader, before: &ItemFlags, now: &ItemFlags) -> Vec<Pickup> {
    let mut out = Vec::new();
    if now == before {
        // The overwhelmingly common frame: nothing was taken, and nothing more need be read.
        return out;
    }
    let sprites = memory.read8(ram::wNumSprites).min(poke::SPRITE_SLOTS - 1);
    for entry in 0..TOGGLE_LIST_ENTRIES {
        let slot = memory.read8(ram::wToggleableObjectList + entry * 2);
        if slot == 0xff {
            break;
        }
        let global = memory.read8(ram::wToggleableObjectList + entry * 2 + 1);
        let index = usize::from(global);
        if slot == 0 || slot > sprites || !bit(&now.toggles, index) || bit(&before.toggles, index) {
            continue;
        }
        let (item, second) = extra_data(memory, slot);
        if item != 0 && second == 0 {
            out.push(Pickup {
                key: ball_key(global),
                label: format!("FOUND ITEM #{item}"),
            });
        }
    }
    for index in 0..HIDDEN_ITEM_BYTES * 8 {
        if bit(&now.hidden, index) && !bit(&before.hidden, index) {
            out.push(Pickup {
                key: hidden_key(index as u8),
                label: "FOUND A HIDDEN ITEM".to_string(),
            });
        }
    }
    out
}
