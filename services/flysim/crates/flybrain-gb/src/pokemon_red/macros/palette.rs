//! Which macro each readout channel means in the current scene.
//!
//! `docs/design/macros.md` section 3 is the table this file implements, row for row. Slot binding
//! is fixed per scene so that the meaning of a channel is stable inside a scene, and a slot whose
//! precondition fails is *unbound* rather than bound-and-refusing: the stage shows it dim, the
//! fly's channel does nothing, and no button is pressed.
//!
//! Slot order is the readout's, from section 1: UP is slot 0, DOWN 1, LEFT 2, RIGHT 3, A 4,
//! B 5.

use super::cartridge::{
    CHEAPEST_PURCHASE, FACINGS, MART_CURSOR_ROWS, opposite,
    ExitId, ListKind, Listing, MacroState, Objective, PARTY_CAPACITY, PURCHASES, TalkTarget,
    TargetKey, Tile, item, outdoors,
};
use crate::adapter::PlaceKind;

use super::geography::{self, Amenity};
use super::path::{self, Exit, Way};
use super::state::{BattleMenu, Facing, Mon, Move, Scene, ShopScreen, Status};

/// Slots in one palette: **one per macro type**, so a cell never moves.
///
/// Six until section 14 (the operator, 2026-09-17: "make it 2 columns"). Six was the readout's D-pad and
/// A/B, from section 1, and it survived section 12's "macros are buttons" as an arbitrary cap on
/// how many buttons a scene could deal at once -- which is how `MENU` ended up on no pad at all
/// (row 18 of `infra/docs/macros-traps.md`), how `GO FRONTIER` had to be rescued from truncation
/// (row 12), and why the battle's own turn could not hold section 14's four move buttons beside
/// `SWITCH`, `ITEM`, `THROW BALL`, `RUN` and `NEXT`.
///
/// A slot is now a *type index* ([`MacroKind::slot`]): the screen draws every cell in a fixed
/// order and lights the ones this scene binds, the wire carries the bound ones with their type
/// index as `slot`, and nothing truncates anything. The decoder never cared -- it competes among
/// the bound *channels* and a slot is only a name for one of them.
pub const SLOTS: usize = MacroKind::BY_CHANNEL.len();

/// A slot within the current scene's palette (`docs/design/macros.md` section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MacroId(pub u8);

/// What a macro does, independent of which slot or scene it is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroKind {
    // Overworld
    GoObjective,
    GoOut,
    GoWarp,
    GoRoute,
    GoNpc,
    GoItem,
    GoFrontier,
    Talk,
    Menu,
    // Dialog
    Next,
    Yes,
    No,
    // Menu
    Close,
    Confirm,
    Back,
    // Battle. Section 14 replaces `ATTACK` with one button per move slot: the fly picks the move,
    // and no "best move" knowledge remains anywhere in this crate.
    Move1,
    Move2,
    Move3,
    Move4,
    Switch,
    Item,
    ThrowBall,
    Run,
    // Errands (`docs/design/macros.md` section 13)
    GoShop,
    GoHeal,
    // Shop and PC
    BuyPotion,
    BuyBall,
    BuyAntidote,
    BuyRepel,
    // Pokémon Center
    Heal,
    Leave,
}

impl MacroKind {
    /// Every macro, for the tests and the docs.
    pub const ALL: [Self; 31] = [
        Self::GoObjective,
        Self::GoOut,
        Self::GoWarp,
        Self::GoRoute,
        Self::GoNpc,
        Self::GoItem,
        Self::GoFrontier,
        Self::GoShop,
        Self::GoHeal,
        Self::Talk,
        Self::Menu,
        Self::Next,
        Self::Yes,
        Self::No,
        Self::Close,
        Self::Confirm,
        Self::Back,
        Self::Move1,
        Self::Move2,
        Self::Move3,
        Self::Move4,
        Self::Switch,
        Self::Item,
        Self::ThrowBall,
        Self::Run,
        Self::BuyPotion,
        Self::BuyBall,
        Self::BuyAntidote,
        Self::BuyRepel,
        Self::Heal,
        Self::Leave,
    ];

    /// The twenty-two types in the order `docs/design/macros.md` section 11 lists them.
    ///
    /// Not the same order as [`MacroKind::ALL`], which is the order this enum happens to declare
    /// them in, and the difference matters three times over: it is the order
    /// `tools/build_flywire.py` cut the populations in (so it decides which neurons are whose),
    /// the decoder's macro-channel order (so it decides which of two equal scores wins a tie), and
    /// the screen's cell order. One list, quoted from the contract, rather than three that could
    /// drift.
    pub const BY_CHANNEL: [Self; 31] = [
        Self::GoObjective,
        Self::GoOut,
        Self::GoWarp,
        Self::GoRoute,
        Self::GoItem,
        Self::GoNpc,
        Self::GoFrontier,
        // Section 13's two errands, beside the other walks: a cell's neighbours on the screen are
        // the macros it is most often dealt with.
        Self::GoShop,
        Self::GoHeal,
        Self::Talk,
        Self::Menu,
        Self::Next,
        Self::Yes,
        Self::No,
        Self::Close,
        Self::Confirm,
        Self::Back,
        // Section 14: one button per move slot, where `ATTACK` was.
        Self::Move1,
        Self::Move2,
        Self::Move3,
        Self::Move4,
        Self::Switch,
        Self::Item,
        Self::ThrowBall,
        Self::Run,
        Self::BuyPotion,
        Self::BuyBall,
        Self::BuyAntidote,
        Self::BuyRepel,
        Self::Heal,
        Self::Leave,
    ];

    /// This type's slot, which is its position in [`MacroKind::BY_CHANNEL`].
    ///
    /// Section 14: a slot is a type index, so a cell never moves and nothing is ever truncated
    /// off a pad. The screen draws every cell in this order and lights the bound ones.
    pub const fn slot(self) -> u8 {
        let mut index = 0;
        while index < Self::BY_CHANNEL.len() {
            if Self::BY_CHANNEL[index] as u8 == self as u8 {
                return index as u8;
            }
            index += 1;
        }
        // Unreachable: `the_channel_order_is_every_type` pins that the list is complete.
        0
    }

    /// This type's neuron population, which is also its decoder channel
    /// (`docs/design/macros.md` sections 11 and 12).
    ///
    /// `macro_` plus the macro's name lowercased with spaces as underscores, which is the rule
    /// `tools/build_flywire.py` writes the roles by and the rule the stage reads a bound macro's
    /// rate by; `the_channel_is_the_name_lowercased` pins the three ends together. The population
    /// is the button, so the role name and the channel name are deliberately the same string.
    pub const fn channel(self) -> &'static str {
        match self {
            Self::GoObjective => "macro_go_objective",
            Self::GoOut => "macro_go_out",
            Self::GoWarp => "macro_go_warp",
            Self::GoRoute => "macro_go_route",
            Self::GoNpc => "macro_go_npc",
            Self::GoItem => "macro_go_item",
            Self::GoFrontier => "macro_go_frontier",
            Self::Talk => "macro_talk",
            Self::Menu => "macro_menu",
            Self::Next => "macro_next",
            Self::Yes => "macro_yes",
            Self::No => "macro_no",
            Self::Close => "macro_close",
            Self::Confirm => "macro_confirm",
            Self::Back => "macro_back",
            Self::Move1 => "macro_move_1",
            Self::Move2 => "macro_move_2",
            Self::Move3 => "macro_move_3",
            Self::Move4 => "macro_move_4",
            Self::ThrowBall => "macro_throw_ball",
            Self::Switch => "macro_switch",
            Self::Item => "macro_item",
            Self::Run => "macro_run",
            Self::GoShop => "macro_go_shop",
            Self::GoHeal => "macro_go_heal",
            Self::BuyPotion => "macro_buy_potion",
            Self::BuyBall => "macro_buy_ball",
            Self::BuyAntidote => "macro_buy_antidote",
            Self::BuyRepel => "macro_buy_repel",
            Self::Heal => "macro_heal",
            Self::Leave => "macro_leave",
        }
    }

    /// The channel's tag, which is the glyph on the palette cell and the label on the SENSES
    /// tab's MACROS row (`docs/design/macros.md` section 12).
    ///
    /// Six characters at most, because the cell's glyph column is the width it was when it held a
    /// D-pad arrow. `MB` for the mushroom body, whose output neurons most of the population is:
    /// the tag says on air which part of the brain pressed the button.
    pub const fn channel_tag(self) -> &'static str {
        match self {
            Self::GoObjective => "MB·GOAL",
            Self::GoOut => "MB·OUT",
            Self::GoWarp => "MB·WARP",
            Self::GoRoute => "MB·ROUTE",
            Self::GoNpc => "MB·NPC",
            Self::GoItem => "MB·ITEM",
            Self::GoFrontier => "MB·FRONT",
            Self::Talk => "MB·TALK",
            Self::Menu => "MB·MENU",
            Self::Next => "MB·NEXT",
            Self::Yes => "MB·YES",
            Self::No => "MB·NO",
            Self::Close => "MB·CLOSE",
            Self::Confirm => "MB·CONF",
            Self::Back => "MB·BACK",
            Self::Move1 => "MB·MV1",
            Self::Move2 => "MB·MV2",
            Self::Move3 => "MB·MV3",
            Self::Move4 => "MB·MV4",
            // `MB·BALL` is the ball the fly *throws*; the mart's is `MB·PBALL`. Two channels
            // cannot share a tag on a screen that identifies a cell by it.
            Self::ThrowBall => "MB·BALL",
            Self::Switch => "MB·SWAP",
            Self::Item => "MB·BAG",
            Self::Run => "MB·RUN",
            Self::GoShop => "MB·SHOP",
            Self::GoHeal => "MB·HEAL",
            Self::BuyPotion => "MB·POTN",
            Self::BuyBall => "MB·PBALL",
            Self::BuyAntidote => "MB·ANTI",
            Self::BuyRepel => "MB·REPEL",
            // Not `MB·HEAL`, which is the walk to the door: this is the conversation at the
            // counter, and two channels cannot share a tag on a screen that identifies a cell by
            // it.
            Self::Heal => "MB·NURSE",
            Self::Leave => "MB·LEAVE",
        }
    }

    /// The name on screen. At most fourteen characters, as section 3 requires.
    pub const fn name(self) -> &'static str {
        match self {
            Self::GoObjective => "GO OBJECTIVE",
            Self::GoOut => "GO OUT",
            Self::GoWarp => "GO WARP",
            Self::GoRoute => "GO ROUTE",
            Self::GoNpc => "GO NPC",
            Self::GoItem => "GO ITEM",
            Self::GoFrontier => "GO FRONTIER",
            Self::Talk => "TALK",
            Self::Menu => "MENU",
            Self::Next => "NEXT",
            Self::Yes => "YES",
            Self::No => "NO",
            Self::Close => "CLOSE",
            Self::Confirm => "CONFIRM",
            Self::Back => "BACK",
            Self::Move1 => "MOVE 1",
            Self::Move2 => "MOVE 2",
            Self::Move3 => "MOVE 3",
            Self::Move4 => "MOVE 4",
            Self::ThrowBall => "THROW BALL",
            Self::Switch => "SWITCH",
            Self::Item => "ITEM",
            Self::Run => "RUN",
            Self::GoShop => "GO SHOP",
            Self::GoHeal => "GO HEAL",
            Self::BuyPotion => "BUY POTION",
            Self::BuyBall => "BUY BALL",
            Self::BuyAntidote => "BUY ANTIDOTE",
            Self::BuyRepel => "BUY REPEL",
            Self::Heal => "HEAL",
            Self::Leave => "LEAVE",
        }
    }

    /// The item id and price this macro buys, for a `BUY …` and `None` for everything else.
    ///
    /// Indices into [`PURCHASES`], so the four buttons, their four preconditions and
    /// [`super::cartridge::CHEAPEST_PURCHASE`] -- which is what the mart errand's money test
    /// measures against -- are one list rather than three that could drift.
    pub const fn purchase(self) -> Option<(u8, u32)> {
        match self {
            Self::BuyPotion => Some(PURCHASES[0]),
            Self::BuyBall => Some(PURCHASES[1]),
            Self::BuyAntidote => Some(PURCHASES[2]),
            Self::BuyRepel => Some(PURCHASES[3]),
            _ => None,
        }
    }

    /// Two or three words of what this macro *does*, for the palette cell under the screen.
    ///
    /// `docs/design/macros.md` section 6: "each with the macro's name and a two or three word
    /// gloss of what it does in this scene (`GO EXIT · nearest door`)". One gloss per macro rather
    /// than one per scene-and-slot, because a macro means the same thing wherever it is bound —
    /// `NEXT` advances text in a dialog and in an `Unknown` frame alike — and a second table
    /// keyed by scene could only drift from the first.
    ///
    /// Terse copy, as the screen rules require, and it goes over the feed rather than living on
    /// the page: `game.palette[].gloss` (`docs/feed-protocol.md`).
    pub const fn gloss(self) -> &'static str {
        match self {
            // Section 9.1 splits the old `GO EXIT` three ways, and the gloss is where the
            // distinction has to land for a viewer: out of the building, between its floors, or
            // on to the next area. The operator's own words for the three.
            Self::GoObjective => "next rung",
            Self::GoOut => "leave building",
            Self::GoWarp => "stairs or door",
            Self::GoRoute => "next area",
            Self::GoNpc => "nearest person",
            Self::GoItem => "nearest object",
            Self::GoFrontier => "new ground",
            // One press of A at whatever is ahead, which is a person, a ball, a sign or nothing.
            // `LOOK` used to sit in slot 2 with its own name for the second half of that, and it
            // was the same press at the same tile: section 3, 2026-09-16.
            Self::Talk => "talk / look",
            Self::Menu => "open start menu",
            Self::Next => "advance text",
            Self::Yes => "answer yes",
            Self::No => "answer no",
            Self::Close => "back to overworld",
            Self::Confirm => "take the choice",
            Self::Back => "one step back",
            Self::Move1 => "first move",
            Self::Move2 => "second move",
            Self::Move3 => "third move",
            Self::Move4 => "fourth move",
            Self::ThrowBall => "throw a ball",
            Self::Switch => "healthiest reserve",
            Self::Item => "use a potion",
            Self::Run => "flee the battle",
            Self::GoShop => "to the mart",
            Self::GoHeal => "to the centre",
            Self::BuyPotion => "buy one potion",
            Self::BuyBall => "buy one ball",
            Self::BuyAntidote => "buy one antidote",
            Self::BuyRepel => "buy one repel",
            Self::Heal => "rest the party",
            Self::Leave => "close the menu",
        }
    }
}

/// One bound slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacroSpec {
    pub name: &'static str,
    pub kind: MacroKind,
}

impl MacroSpec {
    pub const fn of(kind: MacroKind) -> Self {
        Self { name: kind.name(), kind }
    }
}

/// A tile a walking macro aims at, and how the session's ledgers name what is there.
///
/// `tile` is somewhere to stand, `press` the one press that arriving asks for (a doormat's step
/// off, `None` for a warp that fires by itself), and `key` how [`MacroState::blocked`] and
/// [`MacroState::reached`] name the target -- `None` for a tile that is not a target in its own
/// right. The key travels with the goal rather than being re-derived at the end because by the
/// time a walk aborts the fly may be standing somewhere else entirely, and "which thing did this
/// macro fail to reach" is only answerable where the thing was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aim {
    pub tile: Tile,
    pub press: Option<Facing>,
    /// The direction to turn once standing on `tile`, for an aim that is "beside it, facing it".
    ///
    /// A rung earned by a conversation is a person to stand next to and look at, which is exactly
    /// `GO NPC`'s arrival; a rung earned by standing somewhere is a tile or a door, which is
    /// `press`. Both in one type because `GO OBJECTIVE` is one macro over all of them.
    pub face: Option<Facing>,
    pub key: Option<TargetKey>,
}

/// The six slots of one scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub scene: Scene,
    pub slots: [Option<MacroSpec>; SLOTS],
}

impl Palette {
    /// The palette for `scene`, with every precondition in section 3 evaluated against `state`.
    ///
    /// The contract writes this as taking a `&dyn GameState`; agent A's seam reads WRAM behind
    /// `&mut self` accessors — reads are memoized per frame behind it — so it is `&mut dyn` here.
    /// Nothing else about the signature moves, and nothing here presses a button.
    ///
    /// The three preconditions the table states in words are:
    ///
    /// - `ITEM` — "potion if own HP < 50% and one is held, else no action";
    /// - `RUN` — "(wild only; trainer: no action)";
    /// - `BUY POTION` / `BUY BALL` — "if money allows".
    ///
    /// The rest follow from the same principle, because a macro with nothing to act on is not an
    /// action: `GO EXIT` needs an exit, `GO NPC` needs a visible person, `GO ITEM` needs an
    /// object or a sign, `SWITCH` needs another Pokémon that can fight, and `ATTACK` needs a move
    /// with PP left.
    ///
    /// `GO EXIT`'s binding test is deliberately the cheap one — does this map have an exit at all
    /// — because `for_scene` runs every frame and a route search does not.
    /// [`super::executor::MacroMachine::start`] settles whether a route exists, and refuses the
    /// slot rather than pressing anything.
    pub fn for_scene(scene: Scene, state: &mut dyn MacroState) -> Self {
        let mut slots: [Option<MacroSpec>; SLOTS] = [None; SLOTS];
        for kind in scene_set(scene, state) {
            // A button refused from this very tile inside the window is not dealt again from it
            // (row 57): the dealer's question is the cheap one, and `start`'s answer to the real
            // one outranks it until the fly stands somewhere else or the window closes.
            if precondition(kind, state) && !state.refused_here(kind.slot()) {
                slots[usize::from(kind.slot())] = Some(MacroSpec::of(kind));
            }
        }
        Self { scene, slots }
    }

    /// The spec in `slot`, or `None` when the slot is unbound or out of range.
    pub fn slot(&self, slot: MacroId) -> Option<MacroSpec> {
        self.slots.get(usize::from(slot.0)).copied().flatten()
    }

    /// How many slots this palette binds, for the log line and the tests.
    pub fn bound(&self) -> usize {
        self.slots.iter().flatten().count()
    }
}

/// Which macro types this scene deals *at all*, before any precondition is asked.
///
/// The one table, and `docs/design/macros.md` sections 12, 12.6, 13, 13.1 and 14 are what it says.
/// Section 12 gave the fixed six-channel table of section 3 and the plan's ordering as two
/// dealers, which could disagree about which buttons a scene had -- and did, for two releases:
/// `GO OBJECTIVE` was on the plan's overworld and on no fixed row. Section 14 removes the last
/// reason they differed, because with a slot per type there is nothing to order and nothing to
/// truncate. [`super::plan::plan_for`] is this function.
///
/// A set, not a ranking. Nothing here decides which of them the fly presses.
pub fn scene_set(scene: Scene, state: &mut dyn MacroState) -> Vec<MacroKind> {
    use MacroKind::*;
    match scene {
        // Section 2: "no palette: boot variant of the readout applies".
        Scene::Title => Vec::new(),
        // Section 2 treats `Unknown` like a dialog -- advance only -- and `BACK` is the other half
        // of advancing, because `Unknown` is also where the Pokédex, the trainer card and OPTION
        // land (`docs/design/macros-wram.md`) and B is what leaves all three.
        //
        // **Only while there is something on screen with words in it** (section 12.13, the
        // rung-10 Pewter loop). `Unknown` is the detector's residue and it holds two states, not
        // one: a screen this crate cannot name, where A and B are what leave it, and a frame of
        // the *overworld* where the cartridge is driving -- a warp in flight, a scripted
        // push-back, the museum guide walking the fly through the door -- which
        // `scene::detect` calls `Unknown` because the buttons are not reaching the player. On the
        // second, `NEXT` and `BACK` are an A and a B pressed into somebody else's script: they
        // change nothing, they complete where the fly stands, and they are section 12.2's trap
        // with no text box to advance. Measured live on rung 10: `BACK` **678** macro starts in
        // 47 minutes, 189 of them on map `0x02` with no box on screen at all. So the pad is
        // empty there and the fly waits, which is the doctrine's own answer for a scene with
        // nothing sensible to press -- and the cartridge gives the buttons back by itself.
        Scene::Unknown => {
            if state.text_open() {
                vec![Next, Back]
            } else {
                Vec::new()
            }
        }
        // `NEXT`, `YES`, `NO` for a plain box -- A and B both advance one, and what the three
        // buy is the fly being *able* to answer no. On the one box that **is** a choice, the pad
        // is the choice's own answers: section 12.12.
        Scene::Dialog => dialog_set(state),
        Scene::Menu => vec![Close, Confirm, Back],
        // Section 9.1's split, plus section 13's errands and centre. Indoors the ways out of a
        // room are the building's door and its passages; outdoors there is no building to leave.
        //
        // **`MENU` is on no pad** (section 12.11). It was here as the unconditional button that
        // made an empty overworld pad impossible, and that is exactly what made it a trap: opening
        // the start menu changes nothing in the world, so the macro completes where the fly stands
        // -- section 12.2's rule -- and the scene it opens deals `CLOSE` and `BACK`, which close it
        // again. Live on rung 10, thirty minutes inside the Pewter museum's upper floor: `MENU` 82
        // starts, `BACK` 82, `GO FRONTIER` 8, the log alternating `MENU start/done, BACK
        // start/done`. Nothing in the macro vocabulary uses the start menu for anything -- there is
        // no SAVE macro and no POKéDEX macro -- so there is nothing behind the button worth
        // pressing it for. What keeps this pad from being empty instead is the way out, which
        // [`ways`] offers regardless of the ledgers when nothing else on the map is worth walking
        // to.
        Scene::Overworld => {
            let mut set = vec![GoObjective];
            if outdoors_now(state) {
                set.push(GoRoute);
            } else {
                set.push(GoOut);
                set.push(GoWarp);
            }
            if inside_center(state) {
                set.push(Heal);
            }
            set.extend([GoShop, GoHeal, GoItem, GoNpc, GoFrontier, Talk]);
            set
        }
        // Section 12: "Forced switch: SWITCH" -- plus the press that advances a battle, because
        // `SWITCH` needs a Pokémon to switch *to* and a forced switch with none leaves a menu the
        // game will not let the fly cancel and a pad with nothing on it (row 8).
        Scene::Battle { forced_switch: true, .. } => vec![Switch, Next],
        // The fly's turn, by which menu of it is accepting input (section 12.6). **`NEXT` is on
        // no own-turn pad at all** (section 12.10): it is the A that advances *text*, and on a menu
        // that is accepting input the same A press opens or confirms whatever the cursor happens to
        // be sitting on, which is never one of the answers to that menu.
        //
        // Live on rung 9 after v0.4.2, 71 hours in Viridian Forest: `NEXT` 1264 macro starts and
        // `BACK` 1241, the log alternating `NEXT start/done, BACK start/done` every hold. `NEXT` on
        // the *top-level* menu pressed A on FIGHT and opened the move list; `BACK` on the *move
        // list* closed it again; neither spent a turn, and the two of them were half the pad
        // between them. Two buttons that undo each other with nothing else changing are section
        // 12.2's trap spread over two sub-states of one turn.
        Scene::Battle { own_turn: true, .. } => match battle_menu(state) {
            // The move list. `BACK` is a button here because there is a list to leave (12.9) --
            // but only while the moves can be *read*: a battler the seam cannot place leaves all
            // four `MOVE n` unbound, and a pad of `BACK` alone closes the list that `MOVE 1` on
            // the menu underneath had just opened. That is 12.10's pair again, with `MOVE 1` in
            // `NEXT`'s place. With nothing readable the pad is `MOVE 1` alone and its script
            // confirms wherever the cursor stands, which is the press that ends the turn
            // (section 12.11).
            BattleMenu::Moves { .. } => {
                if state.battle().and_then(|battle| battle.own).is_some() {
                    vec![Move1, Move2, Move3, Move4, Back]
                } else {
                    vec![Move1]
                }
            }
            BattleMenu::Party { .. } => vec![Switch, Back],
            // The bag, which is the fly's turn since 12.10. Its three answers: use the thing the
            // cursor is on (`ITEM`), throw the ball (`THROW BALL`), or leave the list (`BACK`).
            // `CONFIRM` was an A press on whatever the cursor held, which is the same press
            // `NEXT` was and reads nothing; the two scripts navigate the list's own cursor.
            BattleMenu::Bag { .. } => vec![Item, ThrowBall, Back],
            // `None` cannot land here -- `own_turn` is false for it -- and the arm exists because
            // `own_turn` is a boolean and this match is over the menu. `MOVE 1` is what keeps this
            // pad from ever being one the fly cannot end the turn from: FIGHT is one of these four
            // entries and it always opens (section 12.8), which is the backstop `NEXT` was.
            BattleMenu::Main { .. } | BattleMenu::None => {
                vec![Move1, Move2, Move3, Move4, Switch, Item, ThrowBall, Run]
            }
        },
        // 2026-09-16 hotfix (live deadlock on Route 1): battle text between turns waits for a
        // press exactly like a dialog.
        //
        // **One button, and it is the A that advances text.** Section 13.1 added `BACK` here for
        // the bag, because a bag read as nobody's turn; 12.9 narrowed it back to the bag alone;
        // 12.10 moves the bag to the own turn where its cursor says it belongs, so nothing is open
        // on this row any more and nothing but `NEXT` is on it. A frame that reaches here has no
        // cursor accepting input -- text, an animation, a turn resolving -- so there is nothing to
        // back out of (`BACK` would be 12.2's trap, the rung-9 loop of 12.9) and nothing for an A
        // press to open (`NEXT` here cannot be the press that opened a menu, which is 12.10).
        Scene::Battle { .. } => vec![Next],
        // Section 13: the shop's buttons are the four purchases, plus the two answers any list has.
        Scene::Shop => vec![BuyPotion, BuyBall, BuyAntidote, BuyRepel, Confirm, Leave],
        // Section 13.1: `CONFIRM` as well as `LEAVE`, so a PC the fly opened is a list it can
        // answer rather than only close. Nothing in the macro vocabulary deposits or withdraws
        // (row 17, contract).
        Scene::Pc => vec![Confirm, Leave],
    }
}

/// Section 3's preconditions, one arm each.
pub fn precondition(kind: MacroKind, state: &mut dyn MacroState) -> bool {
    match kind {
        // A place for the next rung, and a way to aim at it from here; the route search is
        // `start`'s job as it is for every walk.
        MacroKind::GoObjective => !objective_goals(state).is_empty(),
        // Any way out of this kind at all; the route search is `start`'s job.
        MacroKind::GoOut => !ways(state, Way::Exit).is_empty(),
        MacroKind::GoWarp => !ways(state, Way::Passage).is_empty(),
        MacroKind::GoRoute => !ways(state, Way::Route).is_empty(),
        // A tile that borders ground this run has never stood on. Unlike the three above this is
        // the *whole* question rather than a cheap proxy for it, because the frontier search is
        // the same walk of the map the walkable predicate would do anyway.
        MacroKind::GoFrontier => !frontier_aims(state).is_empty(),
        // A *person*, not any sprite. The sixteen sprite slots hold the map's whole
        // `object_event` list and an item ball is one of those, so "is there a sprite" would have
        // bound `GO NPC` on a map with nothing but furniture on it — and walked the fly to a ball
        // under a macro whose gloss says "nearest person". The two macros partition the list.
        MacroKind::GoNpc => !untalked_people(state).is_empty(),
        // The other half of it: the objects, plus the map's sign tiles. This is the slot that
        // reaches the starter Pokéballs on Oak's table, which are `object_event`s with
        // `SPRITE_POKE_BALL` and are not people (section 3, 2026-09-16).
        MacroKind::GoItem => !untalked_objects(state).is_empty(),
        // A press of A at the tile ahead, and only where the map says there is something there
        // that this run has not already talked to (`docs/design/macros.md` section 12: "TALK
        // (only when facing something untalked)"). A tile ahead with nothing on it is not a
        // reason to press A, and a shelf that has been read is not a reason to read it again.
        // ...and only where there is something to say. The nurse of a Pokemon Center is an
        // object with a purpose, not a person to chat with: with the party already full her whole
        // conversation is forty-six text frames that end where they began, which is section
        // 12.2's trap at conversation scale (section 12.12).
        MacroKind::Talk => facing_untalked(state) && !rested_nurse(state),
        // The start menu opens from anywhere, and that is exactly why `MENU` is on no pad:
        // "the precondition is satisfied wherever the fly stands" is section 12.2's trap, and a
        // macro whose whole effect is a screen its own scene's `BACK` closes again is 12.10's
        // pair one scene wider (section 12.11). The refusal belongs at the **dealer** and not
        // here, because this arm is a true fact about the macro and [`scene_set`] is where the
        // reason lives: nothing in the vocabulary uses the start menu, so there is nothing behind
        // the button to press it for -- and the day a SAVE macro exists, one row changes.
        // `MENU` stays a type, a population, a tag and a script, so the roles, the channel order
        // and `--print-compatibility` are untouched.
        MacroKind::Menu => true,
        // Advancing text, answering a prompt and backing out never need anything.
        MacroKind::Next
        | MacroKind::Yes
        | MacroKind::No
        | MacroKind::Close
        | MacroKind::Confirm
        | MacroKind::Back
        | MacroKind::Leave => true,
        // Section 14: one button per move slot, bound only when that slot holds a move with PP --
        // and `MOVE 1` also when *nothing* does, because Struggle is what the cartridge does then
        // and the only way to it is to choose a move anyway (row 30a, row 34). The fly picks the
        // move; nothing here scores one.
        MacroKind::Move1 | MacroKind::Move2 | MacroKind::Move3 | MacroKind::Move4 => {
            move_slot_bound(state, kind)
        }
        // Section 14's addition (the operator: "throw pokeball should be a macro"). A wild battle, a ball
        // in the bag, and room in the party. No catch-rate and no HP knowledge: when to throw is
        // the fly's.
        MacroKind::ThrowBall => throw_slot(state).is_some(),
        MacroKind::Switch => healthiest_other(state).is_some(),
        MacroKind::Item => hurt(state) && potion_slot(state).is_some(),
        // A wild battle the fly is *losing* (`docs/design/macros.md` section 13.1, the operator: "we run
        // away a lot"). Knowledge inside the macro, as a precondition: nothing ranks `RUN` below
        // `ATTACK` -- the button simply is not there while the fight is still worth having.
        MacroKind::Run => {
            state.battle().is_some_and(|battle| battle.kind == super::state::BattleKind::Wild)
                && losing(state)
        }
        MacroKind::BuyPotion
        | MacroKind::BuyBall
        | MacroKind::BuyAntidote
        | MacroKind::BuyRepel => match kind.purchase() {
            Some((id, cost)) => affordable(state, id, cost),
            // Unreachable while [`MacroKind::purchase`] covers the four arms above, and a `false`
            // rather than a panic if it ever stops: an unpriced purchase is a button off the pad.
            None => false,
        },
        // Section 13's two errands. The goal list *is* the whole question here, unlike the three
        // ways out: `amenity_goals` is either a person on this map or this map's exits toward one,
        // so a precondition that held with nothing to walk to could only ever refuse.
        MacroKind::GoShop => !amenity_goals(state, Amenity::Mart).is_empty(),
        MacroKind::GoHeal => !amenity_goals(state, Amenity::Center).is_empty(),
        // On the pad only inside a Pokémon Center, and only while the party needs it
        // (section 13: "at least one party member is not at full HP or has a status").
        MacroKind::Heal => party_needs_rest(state) && !heal_goals(state).is_empty(),
    }
}

/// The dialog pad: the answers to a box that is a choice, the three presses for one that is not.
///
/// **Section 12.12, rung 10, the Pewter Pokemon Center.** Since the 09:39 restart the macro starts
/// were `YES` **2,142**, `TALK` 107, `GO FRONTIER` 26, `BACK` 24, the log ending `YES start/done`
/// for ever, on one tile of map `0x3a`. Surveyed on the cartridge from the live checkpoint: the
/// nurse's conversation is a **ring of forty-six A presses** -- welcome, "We heal your POKeMON
/// back to perfect health!", the YES/NO box on **one** frame of the forty-six, "OK. We'll need
/// your POKeMON.", the machine, "Your POKeMON are fighting fit!", "We hope to see you again!",
/// the box closes for a single frame, and the next A press at a nurse two tiles away over the
/// counter opens the whole thing again. The party was **70/70 and healthy** throughout, so every
/// press of it changed nothing.
///
/// Two readings the survey settles, because the brief allowed three:
///
/// - the box open at the checkpoint is **not** the prompt, it is the closing line, and `YES` there
///   is an A press on plain text. Forty-five of the forty-six frames are like that, and on every
///   one of them `NEXT` and `YES` are the identical press with two names;
/// - `HEAL` is **not** in the loop at all: its precondition already reads the live party and
///   `party_needs_rest` answers `false`, so the button was off the pad the whole time. What was on
///   the pad was the *dialog*, unconditionally, and `TALK` to get back into it.
///
/// So: on a box that is a readable choice the pad is that choice's answers and `NEXT` is off it,
/// which is 12.10's rule about two buttons that are one press; at the nurse's own prompt the
/// answer that changes something is the only one bound, which is 12.2's rule about a macro whose
/// precondition is already satisfied; and an answer that brings the same prompt straight back is
/// excluded for the blocked window, which is 12.1's ledger doing what it does for a walk.
fn dialog_set(state: &mut dyn MacroState) -> Vec<MacroKind> {
    use MacroKind::*;
    if !state.yes_no_prompt() {
        return vec![Next, Yes, No];
    }
    // A choice is open. An A press here confirms whichever option the cursor is on, which is what
    // `YES` is, so `NEXT` is off this pad for exactly 12.10's reason: two buttons that are one
    // press cannot both be answers to the box.
    let answers = if nurse_prompt(state) {
        // The nurse's box is an offer about the party, and the party is a byte. Hurt or statused,
        // the answer worth making is `YES`; full and healthy, the offer is for nothing and the
        // only answer that changes anything is `NO`. Knowledge inside the macro as a
        // precondition, section 13's rule for `HEAL` applied to the box `HEAL` opens.
        if party_needs_rest(state) { vec![Yes] } else { vec![No] }
    } else {
        vec![Yes, No]
    };
    let kept: Vec<MacroKind> =
        answers.iter().copied().filter(|kind| !answer_excluded(state, *kind)).collect();
    // A box must stay answerable: the exclusion narrows a pad, it never empties one. With both
    // answers excluded the fly is offered both again, because a dialog with nothing on its pad is
    // a screen nothing can leave.
    if kept.is_empty() { answers } else { kept }
}

/// Whether this answer to the box at this tile is inside its reopened-prompt exclusion window.
fn answer_excluded(state: &mut dyn MacroState, kind: MacroKind) -> bool {
    let yes = match kind {
        MacroKind::Yes => true,
        MacroKind::No => false,
        _ => return false,
    };
    match answer_key(state, yes) {
        Some(key) => state.blocked(key),
        None => false,
    }
}

/// The blocked-ledger key for answering the box at the tile the fly is standing on.
pub fn answer_key(state: &mut dyn MacroState, yes: bool) -> Option<TargetKey> {
    let player = state.player()?;
    Some(TargetKey::Answer { at: Tile::new(player.x, player.y), yes })
}

/// Whether the box on screen is the Pokemon Center nurse's own YES/NO prompt.
///
/// Three readings, each a byte: the two-option box is drawn ([`MacroState::yes_no_prompt`]), the
/// map is a centre ([`inside_center`]), and the thing the fly is facing is the nurse. The map id
/// is in there because Red draws a two-option box for a dozen scripts and only this one is an
/// offer about the party.
pub fn nurse_prompt(state: &mut dyn MacroState) -> bool {
    state.yes_no_prompt() && inside_center(state) && facing_nurse(state)
}

/// Whether the thing the fly is facing -- over a counter, which is how a nurse is ever faced -- is
/// a Pokemon Center's nurse.
pub fn facing_nurse(state: &mut dyn MacroState) -> bool {
    let Some(TalkTarget::Sprite(slot)) = facing_target(state) else { return false };
    state.npcs().iter().any(|npc| npc.slot == slot && npc.picture == poke_sprite::NURSE)
}

/// Whether the fly is facing a nurse with nothing to ask her for: what takes `TALK` off the pad.
///
/// The nurse is the one person in Red whose conversation has a *precondition*, because her
/// conversation is a service and the cartridge publishes whether the service is needed. `HEAL` has
/// read that byte since section 13; this is the same byte read for the press that opens the same
/// box. Section 12.2's rule, at conversation scale: a `TALK` whose whole effect is a ring of text
/// that ends where it began is a trap, so it is not on the pad.
pub fn rested_nurse(state: &mut dyn MacroState) -> bool {
    inside_center(state) && !party_needs_rest(state) && facing_nurse(state)
}

/// Whether at least one party member is below full HP or carries a status: `HEAL`'s precondition.
///
/// The party rather than the battler, because this is asked in the overworld where the battle
/// engine's copy is stale. A member with species 0 is skipped: `wPartyCount` leads the structs it
/// counts (`docs/design/macros-wram.md`), so a fresh gift can read as a Pokémon with no HP at all
/// and resting for it would be resting for nothing.
pub fn party_needs_rest(state: &mut dyn MacroState) -> bool {
    state.party().mons.iter().any(|mon| {
        mon.species != 0
            && mon.max_hp > 0
            && (mon.hp < mon.max_hp || mon.status != Status::Healthy)
    })
}

/// Whether the party is at full HP with no statuses: what the end of a `HEAL` looks like.
///
/// An empty party answers `false`, so a `HEAL` can never read "already done" off a party that is
/// not there yet.
pub fn party_rested(state: &mut dyn MacroState) -> bool {
    let party = state.party();
    !party.mons.is_empty()
        && party
            .mons
            .iter()
            .all(|mon| mon.species == 0 || (mon.hp == mon.max_hp && mon.status == Status::Healthy))
}

/// Whether a wild battle is one the fly is losing: `RUN`'s precondition (section 13.1).
///
/// Two readings, both of them "there is nothing left to do here but leave", and both taken from
/// the cartridge's own numbers rather than from a judgement about the fight:
///
/// - **the Pokémon that is out is under a third of its HP and no healthier one can come in.**
///   "No healthier one" is `Party::healthiest_reserve` compared on HP fraction, which is the same
///   measure `SWITCH` chooses by, so the two buttons cannot disagree about whether there is a
///   reserve worth sending in.
/// - **every move it has is out of PP and there is nothing to switch to.** Struggle is what the
///   cartridge does then and it costs recoil; with a reserve, `SWITCH` is the move, and without
///   one the battle is a countdown.
///
/// Anything else and `RUN` is *off the pad*: a Pokémon with health and PP is in a fight worth
/// having, and a wild win is a reward the ladder pays for. That is the whole of "we run away a
/// lot" -- `RUN` was bound for every wild battle, so the fly could flee one it was winning, and a
/// fled battle pays nothing and teaches nothing.
pub fn losing(state: &mut dyn MacroState) -> bool {
    let Some(battle) = state.battle() else { return false };
    let Some(own) = battle.own.or_else(|| state.party().active_mon().copied()) else {
        return false;
    };
    if own.max_hp == 0 {
        return false;
    }
    let party = state.party();
    let healthier = party
        .healthiest_reserve()
        .is_some_and(|reserve| reserve.hp_fraction() > own.hp_fraction());
    if u32::from(own.hp) * 3 < u32::from(own.max_hp) && !healthier {
        return true;
    }
    let has_moves = own.moves.iter().flatten().any(|entry| entry.id != 0);
    let no_pp = has_moves && own.moves.iter().flatten().all(|entry| entry.pp == 0);
    no_pp && party.healthiest_reserve().is_none()
}

/// The area the fly is standing in, which is the town an errand is counted once per.
pub fn area_here(state: &mut dyn MacroState) -> Option<u8> {
    state.player().and_then(|player| geography::area_of(player.map))
}

/// The map of this area's outstanding errand of `kind`, or `None` when there is none.
///
/// `docs/design/macros.md` section 13, and the three ways it can be `None`:
///
/// - the area has no building of this kind, or no row in [`geography`]'s table -- a route, Pallet
///   Town, a town the table has not grown a row for yet. Nothing is guessed;
/// - this run has already been inside it ([`MacroState::area_visited`]), which is what makes the
///   errand paid once and never a loop;
/// - for a mart, the money on hand is below [`CHEAPEST_PURCHASE`], so there would be nothing to
///   do when the fly got there.
pub fn errand(state: &mut dyn MacroState, kind: Amenity) -> Option<u8> {
    let area = area_here(state)?;
    if state.area_visited(kind, area) {
        return None;
    }
    if kind == Amenity::Mart && state.money() < CHEAPEST_PURCHASE {
        return None;
    }
    let map = geography::amenity_of(area, kind)?;
    // **A building this run has already been inside is an errand already discharged.** The session
    // ledger above is the errand's own record and it is the one that can be missing: it is written
    // from the frame the fly stands on the building's map, so a restore starts with it empty and
    // the run walks back to a counter it has already used. [`MacroState::map_visited`] is the
    // adapter's lifetime answer to the same question and it does survive, so the two together are
    // "has this run been in there", asked twice (`infra/docs/macros-traps.md` row 54).
    if state.map_visited(map) {
        return None;
    }
    Some(map)
}

/// The errand `GO OBJECTIVE` puts *ahead* of the rung's place, when this area has one.
///
/// Section 13: "on a map whose area has a mart or a Pokémon Center this run has not yet entered,
/// the objective target is that building's door first, then the rung's place."
///
/// Which of the two comes first when both are outstanding is the one piece of ordering in here,
/// and it is knowledge inside the macro rather than a ranking of buttons: **the centre first when
/// the party is hurt or statused, the mart first otherwise.** A hurt party is what ends runs, and
/// a full-price Potion is the expensive way to fix what the nurse fixes for nothing.
pub fn errand_place(state: &mut dyn MacroState) -> Option<Objective> {
    let order = if party_needs_rest(state) {
        [Amenity::Center, Amenity::Mart]
    } else {
        [Amenity::Mart, Amenity::Center]
    };
    let map = order.into_iter().find_map(|kind| errand(state, kind))?;
    Some(Objective { map, tile: None, warp: None, edge: None, target: None })
}

/// Where `GO OBJECTIVE` is going: the errand if this area has one, else the ladder's next rung.
///
/// One accessor rather than `state.objective()` in five places, so the errand cannot be ahead of
/// the rung for the walk and behind it for the ranking of exits.
pub fn objective_place(state: &mut dyn MacroState) -> Option<Objective> {
    errand_place(state).or_else(|| state.objective())
}

/// Whether the open mart stocks `item` within reach of its cursor, and the money on hand covers
/// it.
///
/// Three halves now, and each one has been the answer at a different counter.
///
/// - **Stock.** Viridian's counter sells POKE BALL, ANTIDOTE, PARLYZ HEAL and BURN HEAL and **no
///   Potion at all** (`data/items/marts.asm` at the pinned commit), so `BUY POTION` is correctly
///   off the pad in the first mart the fly ever walks into. That is the precondition working, not
///   a gap.
/// - **Money**, which is section 13's "money allows at least one".
/// - **Reach**, which is row 55. A purchase is navigated by *reading the cursor*, and a mart's
///   buy list scrolls: the cursor sits on rows `0, 1, 2` and the window moves under it, so the
///   absolute position of the item the cursor is on is the index plus a scroll offset this seam
///   cannot read ([`super::cartridge::MART_CURSOR_ROWS`]). Pewter's counter carries seven items and
///   ANTIDOTE is its fourth, so `BUY ANTIDOTE` there is a button whose script gives up before it
///   presses anything -- 747 starts and 747 `blocked` in ten brain minutes, none of them pressing
///   a button and none of them changing a byte. A macro that cannot run is not on the pad, so the
///   first three of a counter's stock are what the four purchases are bound on, and the rest wait
///   for `wListScrollOffset` to be a pinned address.
///
/// The clerk's own text box is the fourth thing this refuses, and it refuses it through the seam
/// rather than here: [`ShopScreen::Talking`] is not one of the two screens a purchase can start
/// from (`docs/design/macros-wram.md` section 7, row 55).
fn affordable(state: &mut dyn MacroState, item: u8, cost: u32) -> bool {
    // Sell cursors index the bag, not the counter's stock; and while the clerk is talking there is
    // no list up at all.
    if !state
        .shop()
        .is_some_and(|shop| matches!(shop.screen, ShopScreen::BuySellQuit | ShopScreen::Buying))
    {
        return false;
    }
    state.money() >= cost && stock_index(state, item).is_some()
}

/// The cursor index of `item` in the open counter's stock, when the cursor can reach it.
///
/// One accessor rather than the same `position` in the precondition and in the script, so the
/// button and the plan cannot disagree about which items a mart can sell the fly (row 6's rule:
/// the cheap question and the real one have to be the same question when they are the same fact).
pub fn stock_index(state: &mut dyn MacroState, item: u8) -> Option<u8> {
    let index = state.shop_stock().iter().position(|stocked| *stocked == item)?;
    if index >= MART_CURSOR_ROWS {
        return None;
    }
    u8::try_from(index).ok()
}

/// What `GO SHOP` or `GO HEAL` walks to, or empty when there is nothing to walk to.
///
/// Two cases, which is section 13's "walk to its door and in; inside, walk to the counter / nurse
/// and face them":
///
/// - **standing in the building already**: the goals are the tiles to face its counter person
///   from ([`counter_aims`]). The errand ledger is written on arrival, so this case is *not*
///   gated on the ledger -- the errand is discharged and the button's remaining job is the walk
///   to the counter, which is what puts the fly where `BUY …` and `HEAL` can be pressed.
/// - **anywhere else in the area, with the errand outstanding**: the goals are this map's exits
///   toward the building, over the same breadth-first hop `GO OBJECTIVE` crosses maps with.
pub fn amenity_goals(state: &mut dyn MacroState, kind: Amenity) -> Vec<Aim> {
    let Some(here) = state.player().map(|player| player.map) else { return Vec::new() };
    if geography::amenity_at(here) == Some(kind) {
        return counter_aims(state, counter_sprite(kind));
    }
    match errand(state, kind) {
        Some(map) => {
            let here = state.player().map(|player| Tile::new(player.x, player.y));
            goals_toward(state, map)
                .into_iter()
                // **An errand arrives inside the building, never on the doormat outside it**
                // (section 12.2's rule, row 54). An aim with no press settles where it stands, so
                // an aim on the tile the fly is already on is `Done` in `SETTLE_FRAMES` with the
                // world exactly as it was -- and a completed errand walk writes the reached ledger,
                // so the same button is dealt on the next hold and the same nothing happens again:
                // `GO HEAL` 204 starts at a mean net of 0.0 tiles and a mean reach of 0.0. The same
                // exclusion [`super::executor::exit_goals`] has made since row 13, for the same
                // reason, on the one walk that did not have it.
                .filter(|aim| aim.press.is_some() || Some(aim.tile) != here)
                .collect()
        }
        None => Vec::new(),
    }
}

/// `HEAL`'s own walk: the tiles the nurse of this Pokémon Center can be talked to from.
///
/// Empty anywhere but inside a centre, which is half of `HEAL`'s precondition.
pub fn heal_goals(state: &mut dyn MacroState) -> Vec<Aim> {
    let Some(here) = state.player().map(|player| player.map) else { return Vec::new() };
    if geography::amenity_at(here) != Some(Amenity::Center) {
        return Vec::new();
    }
    counter_aims(state, poke_sprite::NURSE)
}

/// The picture id of the person who stands behind `kind`'s counter.
const fn counter_sprite(kind: Amenity) -> u8 {
    match kind {
        Amenity::Mart => poke_sprite::CLERK,
        Amenity::Center => poke_sprite::NURSE,
    }
}

/// `constants/sprite_constants.asm` picture ids, which is what the sprite table's byte 0 holds.
///
/// The same numbering `FIRST_STILL_SPRITE` is compared against ([`super::state::Npc::person`]), so
/// "which of these people is the clerk" is a byte the cartridge already publishes rather than a
/// guess from where somebody is standing.
pub mod poke_sprite {
    /// `SPRITE_CLERK`, `$26`.
    pub const CLERK: u8 = 0x26;
    /// `SPRITE_NURSE`, `$29`.
    pub const NURSE: u8 = 0x29;
}

/// Somewhere to stand and face a person with picture id `picture`, counters included.
///
/// The ordinary four tiles around them, **plus the tile two away in each direction when the tile
/// between is a counter** ([`MacroState::counter_tile`]). The second half is not an optimisation:
/// a mart clerk and a Pokémon Center nurse stand behind a desk, so all four tiles around either
/// of them are walls or floor behind the desk, and a walk that only knew how to stand beside
/// somebody could never reach one at all -- `GO SHOP` would refuse `no route` once per hold for
/// ever. `IsSpriteOrSignInFrontOfPlayer`'s `.extendRangeOverCounter` branch is what makes the
/// longer reach real, and it reaches exactly two tiles.
///
/// Nearest first by Manhattan distance, ties by tile, which is `approach`'s own rule. Whether any
/// of the tiles is *reachable* is the route search's answer, not this one's.
pub fn counter_aims(state: &mut dyn MacroState, picture: u8) -> Vec<Aim> {
    let Some(player) = state.player() else { return Vec::new() };
    let here = Tile::new(player.x, player.y);
    let mut people: Vec<(u32, Tile, u8)> = state
        .npcs()
        .iter()
        .filter(|npc| npc.picture == picture)
        .map(|npc| (Tile::new(npc.x, npc.y).distance(here), Tile::new(npc.x, npc.y), npc.slot))
        .collect();
    people.sort_unstable();
    let Some((_, tile, slot)) = people.first().copied() else { return Vec::new() };
    let in_amenity = geography::amenity_at(player.map).is_some();
    let target = TalkTarget::Sprite(slot);
    // The same two exclusions `untalked` applies, and for the same reason: a counter the fly has
    // already been stood in front of, or already talked to, is a job done. Without them
    // `amenity_goals` never empties, `GO SHOP` never leaves the pad inside the building, and the
    // suppression in [`ways`] would keep the fly in there for ever.
    if state.talked(target)
        || state.reached(TargetKey::Thing(target))
        || state.blocked(TargetKey::Thing(target))
    {
        return Vec::new();
    }
    let key = Some(TargetKey::Thing(target));
    let mut aims = Vec::with_capacity(8);
    for facing in FACINGS {
        let Some(beside) = tile.step(facing) else { continue };
        // **Only a tile the collision table does not call a wall.** Every other approach in this
        // file leaves reachability to the route search, and for an ordinary villager that is
        // right; for somebody behind a desk it is not. The floor behind a mart's counter reads
        // walkable and is walled off, so the multi-goal search happily routes *toward* it, spends
        // three failed steps at (0, 7) and reports `Blocked` — which excludes the clerk for ten
        // brain minutes and releases the suppression that was keeping the fly in the shop.
        // Measured from the release container's checkpoint, 2026-09-17: the fly walked the mart's
        // bottom row to the corner and then out of the building. `Unknown` is kept, because a
        // tile off the walkable window is not a wall.
        if state.walkable(beside.x, beside.y) != super::state::Walkable::No
            && !state.pushed_tile(beside.x, beside.y)
        {
            aims.push(Aim { tile: beside, press: None, face: Some(opposite(facing)), key });
        }
        // And over the counter, when that is what the tile beside them is -- or when the fly is
        // inside a mart or a centre at all, for the reason [`facing_target`] gives: the tile read
        // is unreliable on a map smaller than the screen and the map id is not.
        if !in_amenity && !state.counter_tile(beside.x, beside.y) {
            continue;
        }
        let Some(over) = beside.step(facing) else { continue };
        if state.walkable(over.x, over.y) == super::state::Walkable::No
            || state.pushed_tile(over.x, over.y)
        {
            continue;
        }
        aims.push(Aim { tile: over, press: None, face: Some(opposite(facing)), key });
    }
    // The tile the fly is already standing on facing the right way is not somewhere to walk to:
    // that is `TALK`'s state, and the same exclusion `untalked` makes (row 4).
    let ahead = Tile::new(player.x, player.y).step(player.facing);
    aims.retain(|aim| aim.tile != here || Some(tile) != ahead);
    aims
}

/// Whether the fly is standing on an outdoor map, which is what picks the overworld row.
pub fn outdoors_now(state: &mut dyn MacroState) -> bool {
    state.player().is_some_and(|player| outdoors(player.map))
}

/// Whether the fly is standing inside a Pokémon Center, which is what deals `HEAL`.
///
/// Section 13's "centre scene" is a sub-state of the overworld rather than a [`Scene`] of its own,
/// and that is deliberate: pokered has no "a Pokémon Center is open" byte -- the nurse's own
/// dialogue is an ordinary text box -- so the only honest observable is the map id, which
/// [`geography`]'s table already names. A new `Scene` would also be a new `game.scene` on the
/// wire, and section 13 changes no schema.
pub fn inside_center(state: &mut dyn MacroState) -> bool {
    state
        .player()
        .is_some_and(|player| geography::amenity_at(player.map) == Some(Amenity::Center))
}

/// Whether the fly is standing in a mart or a centre whose counter it has not faced yet.
///
/// The one thing that keeps a building shut (2026-09-17, measured from the release container's own
/// checkpoint): the errand is paid on *entering* and never offered again, so a macro that walks
/// the fly straight back out spends the one visit this area gets and nothing can ever bring it
/// back. It reached the Viridian mart in 1.7 brain minutes and could not buy in it — the fly
/// arrives on the building's doormat, `exit_goals` keeps a doormat underfoot because "its press is
/// the point" (row 13 of `infra/docs/macros-traps.md`), and *any* walk that presses DOWN there
/// warps out. The measured culprit was `GO OBJECTIVE`, not `GO OUT`: the rung's place is on
/// another map, so the objective's own goal was the doormat the fly was standing on.
///
/// So while this is true, neither the ways out ([`ways`]) nor the objective ([`objective_goals`])
/// offer anything, and the pad's walks are `GO SHOP` / `GO HEAL` to the counter. It is the same
/// rule row 29 gave the rung's own target — "the room the rung is in is not left while the thing
/// that earns it is standing in it" — and it has the same escape hatches: facing the counter
/// retires it, a `TALK` retires it, and a walk that cannot reach it is excluded for the window.
pub fn counter_pending(state: &mut dyn MacroState) -> bool {
    let Some(here) = state.player().map(|player| player.map) else { return false };
    let Some(kind) = geography::amenity_at(here) else { return false };
    !counter_aims(state, counter_sprite(kind)).is_empty()
}

/// Whether the fly is standing inside a mart, which is where `BUY …` can be reached at all.
pub fn inside_mart(state: &mut dyn MacroState) -> bool {
    state
        .player()
        .is_some_and(|player| geography::amenity_at(player.map) == Some(Amenity::Mart))
}

/// The ways out of the current map of one kind, the ones that lead somewhere new preferred.
///
/// **"Unvisited" is a question about the map on the other side.** Section 9.2, after the town
/// loop: a connection or a door counts as visited only once the run has stood on the map it leads
/// to ([`MacroState::map_visited`]), never because the fly once stood on the boundary tile. The
/// old test was the adapter's `boundary` ledger, which pays for standing on *or beside* an exit,
/// so walking along the top row of Pallet Town marked the way to Route 1 "visited" while the
/// door of an already-explored house still read unvisited -- and with every route exhausted the
/// fallback to *all* of them let "nearest" pick a front door, once per hold, for ever
/// (`infra/docs/macros-bench.md`, 2026-09-16). [`MacroState::exit_visited`] is still the adapter's
/// answer about the boundary and is still what [`visited_exits`] reports for the log line; it is
/// no longer what "somewhere new" means.
///
/// Three tiers, in order:
///
/// 1. **exits into a map this run has not stood on**, and among those, a *connection* before a
///    door: the next area is a bigger unknown than a room off this one, which is the whole of
///    what "rank connections to unvisited maps above doors to unvisited interiors" buys.
/// 2. **exhausted** -- everything of this kind leads somewhere the run has been -- falls back to
///    the ones that lead *toward the objective* ([`toward_objective`]), because a map the fly has
///    already seen is still the way to the next rung.
/// 3. **failing that, all of them**, and the route search picks the nearest -- but only where a map
///    would otherwise be impossible to leave. A room whose doors are all in the ledger still has to
///    be left, so `GO OUT` keeps its last-resort fallback, and so does `GO WARP` on a map with no
///    way out but a passage (Red's bedroom, the upper floor of a house). `GO ROUTE` does not, and
///    neither does `GO WARP` on a floor that has its own front door: outdoors there is no room to
///    be stuck in, a staircase already been up is exploration already done, and "the nearest door,
///    once per hold, for ever" is the loop itself -- measured twice, once as `GO ROUTE` into a house
///    and once as `GO WARP` up its stairs (`infra/docs/macros-traps.md`). Viridian City, 2026-09-17: with every route visited and the one connection
///    toward the objective excluded, tier 3 handed `nearest` the house door the fly was standing
///    at, and the fly walked in, walked out and walked in again for two hours seventeen
///    (`infra/docs/macros-traps.md`). Section 9.2 fixed *which* exits count as visited and left
///    the fallback standing; this is the other half. With nothing fresh and nothing on the way,
///    `GO ROUTE` leaves the pad, which is section 12's "a precondition failure means the button is
///    not on the pad".
///
/// An exit whose destination nobody can name counts as unvisited, which keeps it a candidate --
/// **except a way out of a building**. The only way onto an interior map is through its own front
/// door, so the map outside it is one this run has stood on whether or not the geography table has
/// a row to name it; treating an unnameable front door as "somewhere new" made `GO OUT`
/// permanently tier-one fresh for every house in the game, which is the other half of the same
/// bounce. The fly still chooses whether to press it; nothing here decides to leave.
pub fn ways(state: &mut dyn MacroState, way: Way) -> Vec<Exit> {
    let tiers = exit_tiers(state, way);
    if !tiers.is_empty() {
        return tiers;
    }
    let all = unexcluded_exits(state, way);
    // Tier 3, and only where a map would otherwise be impossible to leave: see the doc comment.
    match way {
        // A room still has to be leavable, and since section 12.11 that holds even while the
        // ledgers are resting its one door: `unexcluded_exits` is emptied by the blocked window,
        // by the rung's own target being on this map (row 29) and by an unfaced counter, and with
        // `MENU` off the pad an emptied way out is an overworld with nothing on it at all.
        Way::Exit => {
            if all.is_empty() {
                last_resort(state, way)
            } else {
                all
            }
        }
        // A staircase the run has already been up is exploration already done, and the same bounce
        // `GO ROUTE` had: up, straight back down, up again, once per hold. The exception is the map
        // whose only way anywhere *is* a passage -- Red's bedroom, the upper floor of any house --
        // where dropping the fallback would strand the fly.
        Way::Passage => {
            if path::exits(state).iter().any(|exit| exit.way == Way::Exit) {
                Vec::new()
            } else if all.is_empty() {
                last_resort(state, way)
            } else {
                all
            }
        }
        // Section 13.1's empty-pad rule, and *only* as a last resort (the operator, 2026-09-17:
        // "sometimes the macro buttons disappear and everything just hangs there").
        //
        // Row 2 of `infra/docs/macros-traps.md` removed `GO ROUTE`'s unconditional tier 3 because
        // "all of them, nearest" walked the fly in and out of the same house door for two hours
        // seventeen. That reasoning still holds and this does not undo it: the fallback is reached
        // only when **nothing else on this map is worth walking to at all** ([`stranded`]) -- no
        // objective, no errand, no person, no object, no frontier, no other kind of exit. In that
        // state the choice is not "a visited door or something better", it is "a visited door or a
        // pad that cannot move", and a door the run has been through is the honest answer.
        //
        // It also ignores the blocked window, which nothing else does: a target the ledger is
        // resting is still the only place to go.
        Way::Route => last_resort(state, way),
    }
}

/// Every way out of this kind, ignoring the ledgers, when the map offers nothing else at all.
///
/// Section 13.1's never-empty rule, and since section 12.11 it is what the rule *rests* on: the
/// overworld has no unconditional button any more, so the pad of a map with every ledger against it
/// is this list or nothing. Guarded by [`stranded`], which is built out of [`exit_tiers`] rather
/// than [`ways`] so that asking "is the fly stranded" cannot recurse into the answer it decides.
///
/// It ignores the blocked window, which nothing else does: a target the ledger is resting is still
/// the only place to go. And it is a last resort rather than a tier -- with anything else on the
/// pad it stays off, because "all of them, nearest" once per hold is row 2's own loop, measured as
/// two hours seventeen in and out of one house door.
fn last_resort(state: &mut dyn MacroState, way: Way) -> Vec<Exit> {
    if !stranded(state) {
        return Vec::new();
    }
    let every: Vec<Exit> =
        path::exits(state).into_iter().filter(|exit| exit.way == way).collect();
    let toward = toward_objective(state, &every);
    if toward.is_empty() { every } else { toward }
}

/// Every way out of this kind, when [`ways`] answered with its last resort and that answer was
/// narrowed to the ways toward the objective; empty otherwise.
///
/// Row 57 (`infra/docs/macros-traps.md`). The narrowing is a preference -- "a map the fly has
/// already seen is still the way to the next rung" -- and the dealer cannot search, so it can
/// prefer a door the fly is walled off from over one it can walk to. Live in Pewter City the last
/// resort was the gym's door on the far side of a fence while the road east, resting in the
/// blocked window, was three tiles away. This is the rest of the last resort, for `start` to try
/// when its route search cannot reach the preferred ones; the choice of *which* reachable way is
/// the route search's, nearest first, exactly as it is for every walk.
pub fn last_resort_wide(state: &mut dyn MacroState, way: Way) -> Vec<Exit> {
    // Only where `ways` fell through to the last resort: a tier that has anything in it is
    // already the answer, and so is a room's or a floor's own unexcluded list.
    if !exit_tiers(state, way).is_empty() {
        return Vec::new();
    }
    match way {
        Way::Exit => {
            if !unexcluded_exits(state, way).is_empty() {
                return Vec::new();
            }
        }
        Way::Passage => {
            if path::exits(state).iter().any(|exit| exit.way == Way::Exit)
                || !unexcluded_exits(state, way).is_empty()
            {
                return Vec::new();
            }
        }
        Way::Route => {}
    }
    if !stranded(state) {
        return Vec::new();
    }
    let every: Vec<Exit> =
        path::exits(state).into_iter().filter(|exit| exit.way == way).collect();
    if toward_objective(state, &every).is_empty() {
        // Not narrowed: the last resort was every one of them already.
        return Vec::new();
    }
    every
}

/// The exits of the current map of one kind that no ledger excludes.
fn unexcluded_exits(state: &mut dyn MacroState, way: Way) -> Vec<Exit> {
    if !objective_targets(state).is_empty() {
        return Vec::new();
    }
    // **A mart or a centre is not left while the thing the errand came for is still unfaced**
    // (2026-09-17, measured from the release container's own checkpoint). The same rule row 29
    // gave the rung's own target, for the same failure: the fly arrives on the building's doormat,
    // `exit_goals` keeps a doormat underfoot because "its press is the point" (row 13), `GO OUT`
    // presses DOWN and the map changes ten frames later. The errand is paid on entering and is
    // never offered again, so that bounce spent the *one* visit this area gets and no macro could
    // ever walk the fly back: it reached the mart in 1.7 brain minutes and could not buy in it.
    //
    // The escape hatches are the ones every other suppression has: facing the counter retires it
    // ([`counter_aims`]), a `TALK` retires it, and a walk that cannot reach it is excluded for the
    // window -- after which `amenity_goals` empties and the way out is a candidate again.
    if counter_pending(state) {
        return Vec::new();
    }
    path::exits(state)
        .into_iter()
        .filter(|exit| exit.way == way && !state.blocked(TargetKey::Exit(exit.id)))
        .collect()
}

/// Whether this map offers *nothing* worth walking to: the trigger for `GO ROUTE`'s last resort.
///
/// Section 13.1's pad-empty audit. Deliberately built out of [`exit_tiers`] rather than [`ways`],
/// so that asking "is the fly stranded" cannot recurse into the fallback the answer decides.
///
/// What this measures is the state the operator saw on stream: a map where nothing the pad offers
/// can move the fly anywhere. It used to be the weaker claim -- `MENU` was unconditional, so the
/// pad was never *literally* empty, only useless -- and since section 12.11 took `MENU` off the
/// overworld it is the literal one, which is why [`last_resort`] is what answers it.
pub fn stranded(state: &mut dyn MacroState) -> bool {
    objective_goals(state).is_empty()
        && amenity_goals(state, Amenity::Mart).is_empty()
        && amenity_goals(state, Amenity::Center).is_empty()
        && !facing_untalked(state)
        && untalked_people(state).is_empty()
        && untalked_objects(state).is_empty()
        && frontier_aims(state).is_empty()
        && exit_tiers(state, Way::Exit).is_empty()
        && exit_tiers(state, Way::Passage).is_empty()
        && exit_tiers(state, Way::Route).is_empty()
}

/// Tiers one and two of [`ways`]: somewhere new, then somewhere on the way to the objective.
fn exit_tiers(state: &mut dyn MacroState, way: Way) -> Vec<Exit> {
    let Some(here) = state.player().map(|player| player.map) else { return Vec::new() };
    // The room the rung is in is not left while the thing that earns it is standing in it.
    //
    // Knowledge inside the macro, the way `GO EXIT` knows which exits the run has taken: a way out
    // is not a candidate while the ladder's own target is on this map, untalked and unexcluded.
    // Nothing here ranks or chooses — the button simply has nothing to aim at, exactly as it has
    // nothing to aim at on a map with no doors.
    //
    // This is row 29 of `infra/docs/macros-traps.md`, and the second half of its fix: `GO OBJECTIVE`
    // used to arrive on the objective map's *doormat* and `GO OUT` walked straight back out, every
    // three brain seconds at Oak's lab door. Suppressing the way out on its own stalled the fly in
    // the room (measured: 600,000 frames in the lab, nothing delivered) because nothing led to Oak;
    // with the objective naming him, something does. And the escape hatch is the blocked ledger: a
    // target no walk can reach is excluded for the window, `objective_targets` empties, and the
    // way out is a candidate again.
    if !objective_targets(state).is_empty() {
        return Vec::new();
    }
    // The same for section 13's errand: the building it came for is not left while its counter is
    // unfaced ([`counter_pending`]).
    let all = unexcluded_exits(state, way);
    let mut fresh = Vec::with_capacity(all.len());
    for exit in &all {
        let known_visited = match exit.destination(here) {
            Some(map) => state.map_visited(map),
            // A front door nobody can name opens on the map the fly came in from, which is a map
            // this run has stood on by construction.
            None if exit.way == Way::Exit => true,
            // **An edge the geography table has no row for** (2026-09-22, the rung-11 reading of
            // row 54). A warp's destination is a byte the cartridge publishes, so `None` there is
            // the `LAST_MAP` case above; an edge's destination comes only from
            // [`geography::connected`], so `None` here means the table cannot name the map on the
            // other side and never will. Route 3 is the measured one: the cartridge reports its
            // connections as **north and west** while the table carries west and *east*, so its
            // seven walkable north-edge tiles answered "leads somewhere this run has not stood
            // on" on every hold for ever, and `GO ROUTE` aimed at them once per hold.
            //
            // The only record left is the adapter's own boundary ledger, which is what section
            // 9.2 replaced as the *general* test and which is still the honest answer for an exit
            // nothing else can say anything about: an edge the run has already stood on is not
            // somewhere new. It narrows, so a genuinely new edge is still first-tier until the
            // fly reaches it.
            None => state.exit_visited(exit.id),
        };
        if !known_visited {
            fresh.push(*exit);
        }
    }
    if !fresh.is_empty() {
        let connections: Vec<Exit> =
            fresh.iter().copied().filter(|exit| matches!(exit.id, ExitId::Edge(_))).collect();
        return if connections.is_empty() { fresh } else { connections };
    }
    toward_objective(state, &all)
}

/// The exits of `candidates` that take the first hop of the route to the objective's map.
///
/// [`geography::next_hop`] is breadth-first over the static map graph, so this is "which of these
/// doors is on the way", answered without a route table and without any map data beyond the one
/// loaded. The region rather than the map id, because `ROUTE_2` is one map whose two halves are
/// not connected to each other and "which way is Pewter" has two answers on it
/// (`docs/design/macros.md` section 12.7).
///
/// When the graph knows no route at all there is still one exit that is plainly toward the
/// objective: one whose destination *is* the objective's map. That is the tier-2 answer for a map
/// the table has no row for, and without it a way out whose whole destination list is visited had
/// no candidate of any tier and left the button off the pad — which is the state the gate house
/// was in. Empty when there is no objective and when it is on this map.
pub fn toward_objective(state: &mut dyn MacroState, candidates: &[Exit]) -> Vec<Exit> {
    let Some(objective) = objective_place(state) else { return Vec::new() };
    let Some(player) = state.player() else { return Vec::new() };
    let here = player.map;
    if here == objective.map {
        return Vec::new();
    }
    let hop = geography::next_hop(geography::region_at(here, player.y), objective.map);
    let aim = hop.unwrap_or(objective.map);
    candidates.iter().copied().filter(|exit| exit.destination(here) == Some(aim)).collect()
}

/// The people on this map still worth walking to, each with the key the ledgers name it by.
pub fn untalked_people(state: &mut dyn MacroState) -> Vec<(Tile, TalkTarget)> {
    if counter_pending(state) {
        return Vec::new();
    }
    let targets = path::person_targets(state);
    untalked(state, targets)
}

/// The objects and signs on this map still worth walking to, with their keys.
pub fn untalked_objects(state: &mut dyn MacroState) -> Vec<(Tile, TalkTarget)> {
    if counter_pending(state) {
        return Vec::new();
    }
    let targets = path::interactable_targets(state);
    untalked(state, targets)
}

/// Whichever of `targets` the talked ledger has no entry for.
///
/// No fallback to all of them: a map whose people have all been talked to leaves `GO NPC`
/// *unbound*, which is section 12's "a precondition failure means the button is not on the pad".
/// That is the point of the ledger -- the loop it replaced was a fallback that could never
/// empty, so the button was always there and always aimed at the same villager.
fn untalked(
    state: &mut dyn MacroState,
    targets: Vec<(Tile, TalkTarget)>,
) -> Vec<(Tile, TalkTarget)> {
    // A fourth exclusion, and the one that stops a macro completing without going anywhere: the
    // thing the fly is *already facing*. `GO NPC` and `GO ITEM` promise "arrive and face it", and
    // standing in front of it is that promise already kept -- so the walk is a press of a button
    // that finishes in twenty frames having moved nothing, once per hold (Viridian City,
    // 2026-09-17: `GO NPC start, GO NPC done (150 ms)`). The press that belongs on the pad there
    // is `TALK`, whose precondition is exactly this state, and the moment the fly turns away the
    // target is a candidate again.
    let ahead = facing_target(state);
    targets
        .into_iter()
        .filter(|(_, target)| Some(*target) != ahead)
        .filter(|(_, target)| {
            // Three exclusions, one candidate list (`docs/design/macros.md` section 12):
            //
            // - **talked** -- a `TALK` finished facing it, which is the ledger section 9.2 added;
            // - **reached** -- a `GO ITEM` or `GO NPC` arrived and faced it, which is all either
            //   macro promises, and the half that was missing while the fly walked to the same
            //   person in Viridian City for forty-five minutes because it never pressed A;
            // - **blocked** -- a walk to it aborted within the exclusion window, which is the
            //   half that was missing while `GO ITEM` re-chose the same unreachable object.
            //
            // Still no fallback to the excluded ones: a map whose things are all accounted for
            // leaves the button off the pad, which is what makes the loop impossible rather than
            // merely unlikely.
            !state.talked(*target)
                && !state.reached(TargetKey::Thing(*target))
                && !state.blocked(TargetKey::Thing(*target))
        })
        .collect()
}

/// The frontier tiles `GO FRONTIER` may aim at: [`path::frontier`] minus the excluded ones.
///
/// The reached half needs nothing here, and that is worth stating because it looks like an
/// omission: a frontier tile the fly *stood on* stops being a frontier by itself, since
/// [`path::frontier`] asks [`MacroState::tile_visited`] about the new ground and the adapter's
/// exploration ledger records the tile three stable samples after the fly arrives. The blocked
/// half is real: a tile the route search cannot reach stays a frontier for ever otherwise.
pub fn frontier_aims(state: &mut dyn MacroState) -> Vec<(Tile, Facing)> {
    // The measured culprit, second time round: a mart's unstood ground includes its own doormats,
    // and `Arrival::Step` presses DOWN onto one, which warps out. While the errand's counter is
    // unfaced the only walks on the pad are the ones that go to it ([`counter_pending`]).
    if counter_pending(state) {
        return Vec::new();
    }
    // **A map whose frontier this run has already proved it cannot reach** (section 12.14, the
    // rung-10 museum). The unstood tiles are still there and still unstood -- the exhibit hall
    // behind the admission desk, the far side of a fence -- and a walk that could not reach any
    // of them refuses `no route`, writes them all to the blocked ledger and comes back ten brain
    // minutes later when the window lapses, for ever. The mark has no window; it is cleared by
    // the fly standing somewhere on this map it had not stood before, which is the only thing
    // that can have changed the answer.
    if state.frontier_exhausted() {
        return Vec::new();
    }
    let mut local: Vec<(Tile, Facing)> = Vec::new();
    for (tile, facing) in path::frontier(state) {
        // A tile the blocked ledger is resting, or one a script pushes the fly off (row 37).
        if state.blocked(TargetKey::Tile(tile)) || state.pushed_tile(tile.x, tile.y) {
            continue;
        }
        local.push((tile, facing));
    }
    if !local.is_empty() {
        return local;
    }
    // Section 13.1's pad-empty rule, second half: **the nearest unstood tile on the whole map**.
    //
    // [`path::frontier`] answers about the ten-by-nine walkable window, so a fly standing in a
    // corner of a route with the near ground covered has an empty frontier while most of the map
    // is unstood -- and with every exit visited that is the pad the operator saw hanging. This fallback
    // asks the stood ledger about every tile of the map instead and walks toward the nearest one
    // the run has not been on, treating an off-window tile as plausible ground exactly as the
    // route search does ([`path::UNKNOWN_STEP`]).
    //
    // It is a fallback and not the rule, because the windowed answer is the *correct* one whenever
    // it has anything in it: those tiles are known-walkable and known-adjacent.
    far_frontier(state)
}

/// The nearest tile of the whole map this run has not stood on, as a one-goal frontier.
///
/// Excludes the tile underfoot, the tiles a sprite is standing in, tiles the collision table calls
/// walls, and anything the blocked ledger is resting. An off-window tile reads
/// [`super::state::Walkable::Unknown`] and is kept: the walk finds out on the way, which is the
/// same bargain every cross-map walk makes.
fn far_frontier(state: &mut dyn MacroState) -> Vec<(Tile, Facing)> {
    let Some(size) = state.map_size() else { return Vec::new() };
    let Some(player) = state.player() else { return Vec::new() };
    let here = Tile::new(player.x, player.y);
    let held: Vec<Tile> = state.npcs().iter().map(|npc| Tile::new(npc.x, npc.y)).collect();
    let mut best: Option<(u32, Tile)> = None;
    for y in 0..size.height {
        for x in 0..size.width {
            let tile = Tile::new(x, y);
            if tile == here || held.contains(&tile) {
                continue;
            }
            if state.walkable(x, y) == super::state::Walkable::No {
                continue;
            }
            if state.tile_visited(x, y)
                || state.blocked(TargetKey::Tile(tile))
                || state.pushed_tile(x, y)
            {
                continue;
            }
            let distance = tile.distance(here);
            if best.is_none_or(|(known, _)| distance < known) {
                best = Some((distance, tile));
            }
        }
    }
    best.map(|(_, tile)| vec![(tile, player.facing)]).unwrap_or_default()
}

/// Whether the tile ahead holds something this run has not talked to: `TALK`'s precondition.
///
/// Both halves in one place now (`docs/design/macros.md` section 12). "Facing something" is the
/// map's own object data: the tile ahead is one of its sprites or one of its `bg_event` signs.
/// "Untalked" is the ledger, which is written when a `TALK` finishes facing the thing -- the
/// question the cartridge could not answer before the ledger existed, and which the two readings
/// tried on it (the ground beside the thing, and the thing's other sides) each got wrong in a
/// different direction.
pub fn facing_untalked(state: &mut dyn MacroState) -> bool {
    match facing_target(state) {
        Some(target) => !state.talked(target),
        None => false,
    }
}

/// What the fly is facing, whether or not it has been talked to: the tile-ahead half of
/// [`facing_untalked`], which [`untalked`] needs on its own.
pub fn facing_target(state: &mut dyn MacroState) -> Option<TalkTarget> {
    let player = state.player()?;
    let here = Tile::new(player.x, player.y);
    let ahead = here.step(player.facing)?;
    let size = state.map_size()?;
    if ahead.x >= size.width || ahead.y >= size.height {
        return None;
    }
    if let Some(target) = path::target_at(state, ahead) {
        return Some(target);
    }
    // **Over a counter**, which is the symmetric half of [`counter_aims`] and was missing from it
    // (2026-09-17, measured on the cartridge in the Viridian mart). The game doubles its own
    // talking range over the tileset's counter tiles, so a fly standing at a counter facing the
    // clerk *is* facing something -- but `target_at` looks one tile ahead, found the counter tile,
    // and answered nothing. `GO SHOP` walked the fly to the counter and `TALK` was not on the pad
    // there, so the counter could never be opened: the two macros between them could reach a mart
    // and not buy in it.
    // `counter_tile` is a screen-buffer read and it is **not reliable on a map smaller than the
    // screen**: measured in the Viridian mart on 2026-09-17, the same tile reads `Yes`/counter
    // from one of the fly's tiles and `No`/not-counter from another two tiles away, because an
    // 8x8 map cannot centre under a ten-by-nine view and the player is no longer at the buffer's
    // fixed point. So the reach is taken from the *map* instead, which is a byte the adapter
    // already has: inside a mart or a Pokémon Center, a counter is the only thing the game puts
    // between the fly and a person, and those two building kinds are the only ones this reach is
    // for. The tile read is still consulted, because it is right everywhere it can be trusted.
    let in_amenity = geography::amenity_at(player.map).is_some();
    if !in_amenity && !state.counter_tile(ahead.x, ahead.y) {
        return None;
    }
    let over = ahead.step(player.facing)?;
    if over.x >= size.width || over.y >= size.height {
        return None;
    }
    path::target_at(state, over)
}

/// The tiles `GO OBJECTIVE` aims at, or empty when it cannot aim at anything from here.
///
/// Section 9: "`GO OBJECTIVE` paths toward the next unreached ladder rung's place where the
/// catalog knows one ... using the map, warp, connection and object data the adapter already
/// reads; if the rung has no known place, fall through." Three cases, in order:
///
/// - **the objective is on this map** and the catalog names a tile or a warp on it: walk there.
///   With neither — which is every rung the Pokémon catalog knows today — there is nothing finer
///   than "be here" to aim at and the fly is already here, so this answers empty and the plan
///   falls through to `GO ITEM` and `GO NPC`, which is exactly right in Oak's lab.
/// - **a warp on this map names the objective's map**: walk to that warp. This is the
///   cross-map step, and it is honest map knowledge rather than a route table — the warp's
///   destination is a byte the adapter already reads for every warp of the loaded map.
/// - **the objective is outdoors and the fly is indoors**: a `LAST_MAP` warp leads outside, so it
///   counts. Without this the front door of a house would never be the objective's door, because
///   pokered writes it as "back where you came from" rather than as the town's id.
pub fn objective_goals(state: &mut dyn MacroState) -> Vec<Aim> {
    // Section 13 puts the area's errands *ahead* of the rung's place, and this is the one line
    // that does it: everything below is unchanged, aimed at whichever place is in front.
    // The building the errand came for is not left while its counter is unfaced, and this is the
    // macro that was measured leaving it ([`counter_pending`]).
    if counter_pending(state) {
        return Vec::new();
    }
    let Some(objective) = objective_place(state) else { return Vec::new() };
    let Some(player) = state.player() else { return Vec::new() };
    let here = player.map;
    // Every way out this macro could aim at, minus the ones a walk has just failed at: the same
    // blocked-target ledger `ways` applies, and the reason `GO OBJECTIVE` can fall through to
    // nothing and leave its button off the pad rather than sit on the pad unchosen.
    let exits: Vec<Exit> = path::exits(state)
        .into_iter()
        .filter(|exit| !state.blocked(TargetKey::Exit(exit.id)))
        .collect();
    let of = |exits: Vec<Exit>| -> Vec<Aim> {
        exits
            .into_iter()
            .map(|exit| Aim {
                tile: exit.tile,
                press: exit.press,
                face: None,
                key: Some(TargetKey::Exit(exit.id)),
            })
            .collect()
    };
    if objective.map == here {
        // A rung earned by talking to something: the aim is the four tiles around it with the
        // press that turns to face it, which is `GO NPC`'s arrival. `approach` in the executor
        // builds the same shape for the same reason.
        //
        // The tile ahead is left out, exactly as `untalked` leaves it out (row 4): a fly already
        // standing in front of the thing has nothing left for a *walk* to do, and that is the state
        // `TALK`'s own precondition is. So `GO OBJECTIVE` completes, leaves the pad, and `TALK`
        // takes its place on the very next hold.
        if objective.target.is_some() {
            let ahead = Tile::new(player.x, player.y).step(player.facing);
            let here_tile = Tile::new(player.x, player.y);
            let targets = objective_targets(state);
            // **Facing any of them is the arrival** (row 58). With one target this was already
            // true -- the thing ahead is left out and nothing else is left -- but a gym has three
            // people the ladder names, and standing in front of the leader left the Jr. Trainer
            // to walk to: `GO OBJECTIVE` walked to him, then back to the leader, and `TALK` was
            // the one press it never made room for. A fly facing a person the rung is waiting on
            // has nothing left for a walk to do.
            if targets.iter().any(|(tile, _)| Some(*tile) == ahead) {
                return Vec::new();
            }
            let mut ranked: Vec<(u32, Tile, TalkTarget)> = targets
                .into_iter()
                .map(|(tile, target)| (tile.distance(here_tile), tile, target))
                .collect();
            ranked.sort_unstable();
            let Some((_, tile, target)) = ranked.first().copied() else { return Vec::new() };
            let mut aims = Vec::with_capacity(4);
            for facing in FACINGS {
                let Some(stand) = tile.step(facing) else { continue };
                // Not a tile a script pushes the fly off (row 37); `approach` excludes the same.
                if state.pushed_tile(stand.x, stand.y) {
                    continue;
                }
                aims.push(Aim {
                    tile: stand,
                    press: None,
                    face: Some(opposite(facing)),
                    key: Some(TargetKey::Thing(target)),
                });
            }
            return aims;
        }
        if let Some(tile) = objective.tile {
            if state.blocked(TargetKey::Tile(tile)) || state.pushed_tile(tile.x, tile.y) {
                return Vec::new();
            }
            return vec![Aim { tile, press: None, face: None, key: Some(TargetKey::Tile(tile)) }];
        }
        if let Some(index) = objective.warp {
            return of(exits.into_iter().filter(|exit| exit.id == ExitId::Warp(index)).collect());
        }
        if let Some(edge) = objective.edge {
            return of(exits.into_iter().filter(|exit| exit.id == ExitId::Edge(edge)).collect());
        }
        return Vec::new();
    }
    goals_toward(state, objective.map)
}

/// This map's exits that take the first step of the way to `target`.
///
/// The cross-map half of [`objective_goals`], lifted out because section 13's two errands need
/// exactly the same walk to a different map: the first hop of a breadth-first route over the map
/// graph, and whichever of this map's exits takes it, asked again on arrival -- which is the same
/// re-plan-every-tile shape the walk itself has.
///
/// When the graph knows no route at all there is still one exit that is plainly toward the target:
/// one whose destination *is* that map. And for a target outdoors reached from indoors, the way
/// out of the building counts, because pokered writes a front door as "back where you came from"
/// rather than as the town's id. Empty when `target` is this map, and when nothing leads there.
pub fn goals_toward(state: &mut dyn MacroState, target: u8) -> Vec<Aim> {
    let Some(player) = state.player() else { return Vec::new() };
    let here = player.map;
    if here == target {
        return Vec::new();
    }
    let exits: Vec<Exit> = path::exits(state)
        .into_iter()
        .filter(|exit| !state.blocked(TargetKey::Exit(exit.id)))
        .collect();
    let of = |exits: Vec<Exit>| -> Vec<Aim> {
        exits
            .into_iter()
            .map(|exit| Aim {
                tile: exit.tile,
                press: exit.press,
                face: None,
                key: Some(TargetKey::Exit(exit.id)),
            })
            .collect()
    };
    if let Some(hop) = geography::next_hop(geography::region_at(here, player.y), target) {
        let toward: Vec<Exit> =
            exits.iter().copied().filter(|exit| exit.destination(here) == Some(hop)).collect();
        if !toward.is_empty() {
            return of(toward);
        }
    }
    let outward = !outdoors(here) && outdoors(target);
    of(exits
        .into_iter()
        .filter(|exit| match exit.into {
            Some(map) => map == target,
            None => outward && exit.way == Way::Exit,
        })
        .collect())
}

/// The people or objects on this map that the objective's place names, untalked and unexcluded.
///
/// Empty unless the ladder's next rung is earned *here* and by talking to something
/// ([`crate::adapter::PlaceKind`]). Two ledgers filter it and a third deliberately does not:
///
/// - **talked** — a conversation already had is not the one the rung is waiting for;
/// - **blocked** — a target a walk has just failed to reach is excluded for the window, which is
///   what lets the leaving macros back onto the pad when the objective's own thing is unreachable;
/// - **reached** is *not* applied. That ledger is `GO NPC`'s politeness — "you have stood in front
///   of this villager once" — and the ladder is not being polite: the rung is the conversation, so
///   the thing stays worth walking back to until it has been had.
pub fn objective_targets(state: &mut dyn MacroState) -> Vec<(Tile, TalkTarget)> {
    let Some(objective) = objective_place(state) else { return Vec::new() };
    let Some(kind) = objective.target else { return Vec::new() };
    let Some(here) = state.player().map(|player| player.map) else { return Vec::new() };
    if objective.map != here {
        return Vec::new();
    }
    let targets = match kind {
        // Row 58: the room's people, drawn or not. From the Pewter Gym's doormat the only person
        // on screen is the guide, and with him talked to this list was empty -- so `GO OUT` was a
        // candidate and `GO OBJECTIVE` had nothing to aim at, while BROCK stood twelve tiles up the
        // room, outside the window the cartridge draws. The whole map's grid is what the walk
        // plans over (section 15), so a person off the screen is somewhere a walk can go.
        PlaceKind::Person => {
            let mut all = path::person_targets(state);
            all.extend(path::offscreen_person_targets(state));
            all
        }
        // Not objects: every item ball in the game is a toggleable object, so a ball the run has
        // picked up and one out of sight read alike from outside the window.
        PlaceKind::Object => path::interactable_targets(state),
    };
    targets
        .into_iter()
        .filter(|(_, target)| {
            !state.talked(*target) && !state.blocked(TargetKey::Thing(*target))
        })
        .collect()
}

/// The exits the ledger has already recorded, for the log line.
pub fn visited_exits(state: &mut dyn MacroState) -> Vec<ExitId> {
    path::exits(state)
        .iter()
        .map(|exit| exit.id)
        .filter(|id| state.exit_visited(*id))
        .collect()
}

/// Whether the Pokémon that is out is below half health: `ITEM`'s first half.
///
/// The battle's own copy of the active Pokémon is the one the engine damages, so that is the HP
/// this measures; the party entry is stale during a battle.
pub fn hurt(state: &mut dyn MacroState) -> bool {
    let Some(battle) = state.battle() else { return false };
    let Some(mon) = battle.own.or_else(|| state.party().active_mon().copied()) else {
        return false;
    };
    mon.max_hp > 0 && u32::from(mon.hp) * 2 < u32::from(mon.max_hp)
}

/// The bag index of the first potion, which is also its cursor index in the bag menu.
pub fn potion_slot(state: &mut dyn MacroState) -> Option<u8> {
    state
        .bag()
        .iter()
        .position(|stack| stack.id == item::POTION && stack.count > 0)
        .and_then(|index| u8::try_from(index).ok())
}

/// The party slot of the healthiest Pokémon that is neither fainted nor the one already out.
///
/// Section 3: "highest HP fraction, not fainted, not the active one", ties by slot — which is
/// what agent A's `Party::healthiest_reserve` already answers, so this is a thin wrapper over it
/// rather than a second opinion.
pub fn healthiest_other(state: &mut dyn MacroState) -> Option<u8> {
    state.party().healthiest_reserve().map(|mon: &Mon| mon.slot)
}

/// Which battle menu is accepting input, or `None` outside a battle and between turns.
///
/// The sub-state the pad depends on (`docs/design/macros.md` section 12.6): the top-level menu, the
/// move list, or the party list. `state::battle` decides which; this is the accessor the palette
/// and the executor both read so they cannot disagree about it.
pub fn battle_menu(state: &mut dyn MacroState) -> BattleMenu {
    state.battle().map_or(BattleMenu::None, |battle| battle.menu)
}

/// The move list, when it is the menu that is up: `(cursor, count)`.
pub fn move_list(state: &mut dyn MacroState) -> Option<(Option<u8>, u8)> {
    match battle_menu(state) {
        BattleMenu::Moves { cursor, count } => Some((cursor, count)),
        _ => None,
    }
}

/// The 0-based move slot a `MOVE n` button names.
pub const fn move_index(kind: MacroKind) -> Option<u8> {
    match kind {
        MacroKind::Move1 => Some(0),
        MacroKind::Move2 => Some(1),
        MacroKind::Move3 => Some(2),
        MacroKind::Move4 => Some(3),
        _ => None,
    }
}

/// Whether `kind`'s move slot holds a move with PP that the battle engine will not answer with
/// nothing: the four buttons' precondition (section 14, row 60).
///
/// Three things it is *not*, each of them a bug this palette has had:
///
/// - it is not "the best move", because there is no such knowledge in this crate any more. Section
///   14 replaced `ATTACK` with four buttons precisely so that which move is used is the fly's
///   choice and the mushroom body's to learn, rather than a base-power table's.
/// - it is not "is there a move list to open at all" (row 34 of `infra/docs/macros-traps.md`). That
///   rule existed because one button had to stand for every move; with a button per slot the
///   question is per slot again, and the Struggle case is `MOVE 1`'s exception below.
/// - it is not gated on which menu is up: the top-level menu and the open move list deal the same
///   four buttons, and it is the *script* that differs (FIGHT first, or straight to the slot).
///
/// **`MOVE 1` with nothing anywhere.** A Pokémon whose every move is out of PP uses Struggle, and
/// the way to get there is to choose a move anyway. Taking all four buttons off the pad there is
/// how an hour and forty-one minutes of `NEXT` began in Viridian Forest (row 34), so `MOVE 1`
/// stays bound and its script confirms whatever the cursor is on. A Pokémon with no move in slot
/// one at all -- which nothing in the game reaches -- is the one case that still answers `false`.
pub fn move_slot_bound(state: &mut dyn MacroState, kind: MacroKind) -> bool {
    let Some(index) = move_index(kind) else { return false };
    let Some(battle) = state.battle() else { return false };
    // Over the **top-level menu** the question is section 12.8's -- "is there a move list to
    // open" -- and FIGHT always opens: what is behind it is the cartridge's business, because
    // `CheckPlayerHasUsableMoves` prints "has no moves left!" and sets Struggle without opening
    // the list. `MOVE 1`'s script over that menu is "confirm FIGHT and stop" and reads no move at
    // all, so the button is bound there whatever the seam can make of the battler. That is the
    // backstop `NEXT` used to be on this row (section 12.10): the own turn's main menu always has
    // a button that ends the turn, and it is never one that merely reopens a list.
    let main = matches!(battle.menu, BattleMenu::Main { .. });
    // And the same backstop over an **open move list** whose battler the seam cannot read
    // (section 12.11). That frame used to deal `BACK` alone -- the only button on it closed the
    // list `MOVE 1` on the menu underneath had just opened, which is 12.10's pair with `MOVE 1` in
    // `NEXT`'s place. `MOVE 1`'s script over an open list confirms wherever the cursor stands, so
    // it reads no move either, and confirming a move is what ends a turn.
    let Some(own) = battle.own else {
        return index == 0
            && (main || matches!(battle.menu, BattleMenu::Moves { cursor: Some(_), .. }));
    };
    let holds = |slot: usize| -> Option<&Move> {
        own.moves.get(slot).and_then(|entry| entry.as_ref()).filter(|entry| entry.id != 0)
    };
    // **A move the cartridge will answer with nothing is not dealt beside one it will not**
    // (row 60, section 12.23). Live on Route 1: Squirtle's TAIL WHIP against a Pidgey whose
    // DEFENSE was already at -6 was `MOVE 2` 183 times, "Nothing happened!" every time, and no
    // battle ended by the fly's hand. What the move does is the move table's and the effect
    // routine's answer ([`MacroState::move_without_effect`]), read the same way for every move,
    // and it is PP's rule over again: a spent move is not offered beside a usable one, and when
    // nothing is usable what was dealt stays dealt -- taking the last moves away would leave a list
    // whose only button is `BACK`, which is 12.11's pair.
    let mut useful = [false; 4];
    for (slot, flag) in useful.iter_mut().enumerate() {
        if let Some(entry) = holds(slot).copied() {
            *flag = entry.pp > 0 && !state.move_without_effect(entry.id);
        }
    }
    let any_useful = useful.iter().any(|flag| *flag);
    let entry = holds(usize::from(index)).copied();
    if index == 0 && main {
        return !any_useful || useful[0] || entry.is_none_or(|entry| entry.pp == 0);
    }
    let Some(entry) = entry else { return false };
    if entry.pp > 0 {
        return useful[usize::from(index)] || !any_useful;
    }
    // Out of PP. Only `MOVE 1` stays, and only when nothing else has any either -- otherwise the
    // fly would be offered a spent move beside a usable one.
    index == 0 && own.moves.iter().flatten().all(|entry| entry.id == 0 || entry.pp == 0)
}

/// The bag index of the first Poké Ball of any kind: `THROW BALL`'s target, and its precondition.
///
/// `None` when there is no ball, when the battle is not a wild one -- the cartridge refuses a ball
/// against a trainer and prints "The TRAINER blocked the BALL!" -- or when the party is full.
///
/// **Party room only, and this is the limit worth stating.** A caught Pokémon goes to the party if
/// there is space and to the current PC box otherwise, and `docs/design/macros-wram.md` has no
/// reviewed symbol for the box count, so "a box has room" is not a question this crate can ask.
/// Six in the party therefore takes the button off the pad even where a box would have taken the
/// catch -- a narrowing, like every other thing the seam cannot answer, and not a guess.
///
/// No catch rate, no enemy HP, no status: section 14 leaves *when* to throw to the fly.
pub fn throw_slot(state: &mut dyn MacroState) -> Option<u8> {
    let battle = state.battle()?;
    if battle.kind != super::state::BattleKind::Wild {
        return None;
    }
    if state.party().mons.len() >= PARTY_CAPACITY {
        return None;
    }
    // **Not a species this run already has** (section 12.9). Live on rung 9, 69 hours in Viridian
    // Forest: `THROW BALL` was 28 of 183 macro starts, spending balls on the Caterpie and Weedle
    // already in the party -- and a catch opens the nickname screen, which reads `Unknown` and
    // needs the START the pad has no button for (row 14 of `infra/docs/macros-traps.md`), so the
    // throw costs a ball and then a stall.
    //
    // The *party* is the caught set here, and it is the honest one: it is the cartridge's own
    // lifetime record, it survives a restart the session ledgers do not, and it is in the same
    // numbering the enemy is read in -- the internal species index
    // (`docs/design/macros-wram.md`). `wPokedexOwned` is not usable for this: that bitset is by
    // Pokédex *number*, the table that converts an internal index to one is in a ROM bank this
    // crate cannot read, and `docs/design/ladder.md`'s rule is that an unverified number does not
    // go in. It costs nothing measurable: the button is already off the pad while the party is
    // full, nothing in the macro vocabulary deposits into a box (row 17), so every species this
    // run has caught is in the party this reads.
    //
    // An enemy species the seam could not place leaves the button where it was -- a precondition
    // this crate cannot observe is not a precondition, it is a guess (section 13.1).
    if let Some(species) = battle.enemy.map(|enemy| enemy.species).filter(|species| *species != 0)
        && state.party().mons.iter().any(|mon| mon.species == species)
    {
        return None;
    }
    let index = state
        .bag()
        .iter()
        .position(|stack| item::BALLS.contains(&stack.id) && stack.count > 0)?;
    u8::try_from(index).ok()
}

/// The cursor the current scene's macros navigate, derived from whichever of agent A's menus is
/// up.
///
/// `None` means no list is accepting input right now: an animation, a message, or a screen the
/// seam does not report a cursor for. A script that needs one *waits* for it rather than pressing
/// blind, which is section 4's "never by counting presses" taken seriously.
pub fn listing(state: &mut dyn MacroState) -> Option<Listing> {
    if let Some(battle) = state.battle() {
        return match battle.menu {
            BattleMenu::Main { cursor } => {
                Some(Listing { kind: ListKind::BattleMain, current: cursor, max: 3 })
            }
            BattleMenu::Moves { cursor: Some(cursor), count } => Some(Listing {
                kind: ListKind::BattleMoves,
                current: cursor,
                max: count.saturating_sub(1),
            }),
            BattleMenu::Moves { cursor: None, .. } | BattleMenu::None => None,
            BattleMenu::Bag { cursor, count } => (count > 0).then(|| Listing {
                kind: ListKind::BattleBag,
                current: cursor,
                max: count.saturating_sub(1),
            }),
            BattleMenu::Party { cursor } => {
                let max = u8::try_from(state.party().mons.len().saturating_sub(1)).unwrap_or(0);
                Some(Listing { kind: ListKind::BattleParty, current: cursor, max })
            }
        };
    }
    if let Some(menu) = state.start_menu() {
        return Some(Listing {
            kind: ListKind::StartMenu,
            current: menu.cursor.current,
            max: menu.cursor.max,
        });
    }
    if let Some(shop) = state.shop() {
        // **The clerk talking is not a list.** Row 55: `wListMenuID` keeps `PRICEDITEMLISTMENU`
        // across the mart's own text, and the cursor bytes left behind belong to a two-option box
        // -- so a cursor step that read this would take its length and its direction from a menu
        // that is not on screen, which is section 12.11's rule in the one scene it had not
        // reached. Reporting nothing makes the step *wait*, pressing nothing, exactly as it waits
        // for a list that has not drawn yet.
        if shop.screen == ShopScreen::Talking {
            return None;
        }
        return Some(Listing {
            kind: ListKind::Shop,
            current: shop.cursor.current,
            max: shop.cursor.max,
        });
    }
    if let Some(pc) = state.pc() {
        return Some(Listing { kind: ListKind::Pc, current: pc.cursor.current, max: pc.cursor.max });
    }
    None
}

/// Which screen of the mart is up, for the purchase script.
pub fn shop_screen(state: &mut dyn MacroState) -> Option<ShopScreen> {
    state.shop().map(|shop| shop.screen)
}
