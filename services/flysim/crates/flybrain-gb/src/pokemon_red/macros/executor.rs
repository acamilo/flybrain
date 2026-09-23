//! The macro executor: `docs/design/macros.md` section 4, implemented.
//!
//! A macro owns the buttons from `start` until `step` returns `None`. Everything below is either
//! one of that section's rules or one of section 3's scripts, and nothing below chooses *which*
//! macro runs — that is the fly's, through the readout, and this type is only ever handed a slot.
//!
//! | rule | here |
//! | --- | --- |
//! | hard cap 600 frames, then `Timeout` | [`FRAME_CAP`], checked first in [`MacroMachine::step`] |
//! | a scene change aborts at the next frame | [`Class`], checked second in [`MacroMachine::step`] |
//! | one step per tile with a moved check | [`Walk`] |
//! | three failed steps abort with `Blocked` | [`MAX_FAILED_STEPS`] |
//! | navigate by reading cursor state, never by counting presses | [`Cursor`] |
//! | nothing is pressed when a slot is unbound or a precondition fails | [`MacroMachine::start`] |
//!
//! Two types, for one reason. [`MacroMachine`] is the executor proper and reads the game through
//! [`MacroState`], which is how everything else in this module is written and how the tests drive
//! it. [`Executor`] wraps one in the contract's own `MacroExecutor` shape, whose `start` and
//! `step` take a `&mut dyn MemoryReader`, by asking a [`StateSource`] to turn that reader into a
//! state for the frame. Agent C wires the source; nothing in the executor knows an address.

use std::collections::VecDeque;

use crate::adapter::MemoryReader;
use crate::emulator::buttons;

use super::cartridge::{
    FACINGS, ListKind, MacroState, TalkTarget, TargetKey, Tile, battle_entry, button, item,
    opposite,
};
use super::geography::Amenity;
use super::palette::{
    MacroId, MacroKind, Palette, amenity_goals, frontier_aims, heal_goals, healthiest_other,
    facing_target, listing, move_index, move_list, nurse_prompt, objective_goals, party_rested,
    potion_slot,
    precondition,
    shop_screen, stock_index, throw_slot, untalked_objects, untalked_people, ways,
};
use super::path::{self, Route, Way};
use super::state::{Facing, Scene, ShopScreen};

/// Hard cap on one macro, in game frames. `docs/design/macros.md` section 4: "hard cap 600 frames
/// (10 s); a macro that exceeds it aborts with `MacroAbort::Timeout`".
///
/// This is the cap for a macro whose length is a fact about a *menu* — a press, a cursor, a
/// backout — and it is also the floor under a walk's budget ([`walk_budget`]). A walk is the one
/// script whose length is a fact about the ground, so it is budgeted from its plan instead.
pub const FRAME_CAP: u32 = 600;

/// Frames budgeted per tile of a walk's *planned* route.
///
/// [`TILE_FRAMES`] is the window one tile gets, and a tile that moves the player ends the window
/// early, so this is the worst case for a step that lands rather than the average: a route of `n`
/// tiles gets `n` windows and [`FRAME_CAP`] on top ([`walk_budget`]). Viridian City is about
/// twenty tiles by eighteen and the walk from its south end to the Route 2 connection is thirty-
/// odd tiles; at the flat 600-frame cap that walk could not finish, timed out on every attempt,
/// and never resumed — eight hours on rung 8, 164 walks, 165 timeouts, no macro completing at all
/// (`infra/docs/macros-traps.md`, 2026-09-17).
pub const FRAMES_PER_PLANNED_TILE: u32 = 24;

/// The ceiling on one walk however long its plan is: sixty seconds of brain time.
///
/// 59.7275 frames a second is the emulator's rate everywhere in this workspace, so sixty seconds
/// is 3,583 frames. A walk is still a macro and the fly is still owed the pad back: a plan long
/// enough to need more than a minute is a plan across several screens of unknown ground, and the
/// honest answer there is to hand the buttons back and let the next hold resume from where this
/// one stopped.
pub const WALK_FRAME_CEILING: u32 = 3_583;

/// Frames a walk may spend getting no nearer a goal before its budget is allowed to end it.
///
/// "A walk that is making progress never times out" needs a measure of *now* rather than of the
/// whole walk: `best_distance` against `start_distance` answers "did this walk ever close", which
/// a walk that closed once and then stuck answers yes for ever. Two tiles' worth of frames, so a
/// walk held up by one NPC stepping aside is not called stalled and a walk oscillating over the
/// walkable window's edge is.
const PROGRESS_GRACE: u32 = 2 * (TILE_FRAMES + STEP_GAP);

/// Suspended walks kept for a later hold to resume ([`MacroMachine::resume`]).
///
/// Small on purpose: the fly is standing in one place, so the walks worth resuming are the ones
/// aimed at this map's handful of targets. Oldest out first.
const RESUMES: usize = 8;

/// Consecutive tiles a walk may fail to move before it gives up: "three failed steps abort with
/// `MacroAbort::Blocked`".
pub const MAX_FAILED_STEPS: u8 = 3;

/// Frames one button pulse is held, and released after.
///
/// The Game Boy polls the joypad once per frame and the game's own menus debounce, so a press has
/// to be both held and released to register at all — the same eight-on eight-off pattern
/// `tests/rom.rs` and `examples/room_escape.rs` drive the intro with.
const PRESS_HOLD: u32 = 8;
const PRESS_GAP: u32 = 8;

/// Frames a walk holds one direction before calling the tile a failure.
///
/// A tile is a *pulse*, not an indefinite hold, and that is a measured requirement rather than a
/// style: on the frame after a warp the game ignores a direction that is simply held down. The
/// staircase landing in Red's house is the case that found it — sixty-four frames of held DOWN
/// move the player nowhere, while twelve-on twelve-off walks six tiles — and it is the same
/// latched-joypad behaviour the intro needs, which is why `tests/rom.rs` presses Start in pulses.
///
/// One overworld step is about sixteen frames at walking speed, and a direction the player is not
/// already facing spends part of the press turning, so the window is a little over one step. The
/// step is never *assumed* to have happened: [`Walk`] reads the player's coordinates every frame,
/// releases the moment they change, and only calls a tile failed when the window closes with the
/// player where it started (section 4, "a per-step check that the player moved").
const TILE_FRAMES: u32 = 24;

/// Frames the D-pad is released between tiles, so the next direction is a fresh press.
const STEP_GAP: u32 = 6;

/// Frames a script waits, pressing nothing, for a menu to open or close before reading its
/// cursor.
const SETTLE_FRAMES: u32 = 20;

/// Frames a cursor step waits for a list to start accepting input before it gives up. Generous,
/// because a battle spends whole seconds animating, and a wait presses nothing.
const CURSOR_WAIT: u32 = 180;

/// Extra presses a cursor navigation may spend beyond twice the length of its list, to cover a
/// press the game swallows while a menu is still drawing.
const CURSOR_SLACK: u8 = 6;

/// Presses a `CLOSE` or `LEAVE` may spend backing out before it reports `Blocked`.
const BACKOUT_PRESSES: u8 = 8;

/// Presses a `MENU` may spend opening the start menu before it reports `Blocked`.
const OPEN_PRESSES: u8 = 3;

/// Frames after an answer inside which the same YES/NO prompt coming back is that answer's doing.
///
/// One hold of the Game Boy preset's macro group (268 brain milliseconds, sixteen frames at
/// 59.7275 fps) and half of one again, which is the fly's own next decision plus the frames the
/// cartridge spends redrawing: the nurse's prompt is back **two frames** after a `YES` at the
/// rung-10 checkpoint (`infra/docs/macros-traps.md`, row 41). Later than this and the prompt came
/// back because something else happened, which is not the answer's fault.
const ANSWER_REOPEN_FRAMES: u32 = 24;

/// Frames a `HEAL` waits, pressing nothing, for the healing machine to finish.
///
/// `docs/design/macros.md` section 13: "wait for the heal animation to end (read the party HP
/// back to full)". The observable is the party, not a timer -- the wait ends the frame every
/// member reads full and healthy -- and this is only the bound on waiting for it, so that a
/// conversation that went somewhere else (the nurse's link-cable line, a full bag) hands the pad
/// back instead of sitting through the frame ceiling. Six seconds of brain time: the machine's
/// animation is about three.
const HEAL_WAIT_FRAMES: u32 = 360;

/// How a macro ended, i.e. the `outcome` field of the `macro` feed event (section 5: "outcome =
/// done/blocked/timeout/refused").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroAbort {
    /// The script ran out, or the world moved on under it. A scene change is this rather than a
    /// failure: the presses happened, nothing is stuck, and the fly is consulted again on the new
    /// scene's palette.
    Done,
    /// Three tiles in a row that did not move the player, no route to any goal, or a menu whose
    /// cursor would not go where the script needed it.
    Blocked,
    /// [`FRAME_CAP`] frames elapsed.
    Timeout,
    /// The slot was unbound or its precondition failed. Nothing was pressed.
    Refused,
}

impl MacroAbort {
    /// The lowercase word the feed and the stage use.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Timeout => "timeout",
            Self::Refused => "refused",
        }
    }
}

/// Why [`MacroMachine::start`] pressed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacroRefused {
    pub slot: MacroId,
    pub reason: Refusal,
}

/// The reasons in [`MacroRefused`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The slot has no macro in this scene.
    Unbound,
    /// The palette is for a different scene than the frame is in.
    WrongScene,
    /// Section 3's precondition for this macro does not hold any more.
    Precondition,
    /// A walking macro found nothing to walk to over the tiles it can see.
    NoRoute,
    /// A macro is already running and owns the buttons.
    Busy,
}

impl Refusal {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unbound => "unbound",
            Self::WrongScene => "wrong scene",
            Self::Precondition => "precondition",
            Self::NoRoute => "no route",
            Self::Busy => "busy",
        }
    }
}

/// `docs/design/macros.md` section 4's trait, with `outcome` added for section 5's feed event.
pub trait MacroExecutor {
    /// Begin a macro. Returns Err if the slot is unbound or the precondition fails; nothing
    /// is pressed in that case.
    fn start(
        &mut self,
        palette: &Palette,
        slot: MacroId,
        memory: &mut dyn MemoryReader,
    ) -> Result<(), MacroRefused>;

    /// Called once per game frame while running: returns the button mask to hold this frame.
    /// `None` means the macro has finished (or aborted) and the fly is consulted again.
    fn step(&mut self, memory: &mut dyn MemoryReader) -> Option<u8>;

    fn running(&self) -> Option<&'static str>;

    /// How the last macro ended, and what it was called.
    ///
    /// Additive on the contract's three methods and defaulted, so the contract's own signature is
    /// still a complete implementation. Section 5 needs it: "every macro start and finish is a
    /// `FeedEvent` … outcome = done/blocked/timeout/refused".
    fn outcome(&self) -> Option<(&'static str, MacroAbort)> {
        None
    }
}

/// Turns raw memory into the frame's game state.
///
/// The contract's `MacroExecutor` takes a `&mut dyn MemoryReader` and agent A's seam is a set of
/// `&mut self` accessors, so something has to make one out of the other; this is that seam, and it
/// is the only thing in the module that touches a `MemoryReader` at all. Agent A's `PokeState` is
/// the real implementation.
pub trait StateSource {
    /// Read the game for this frame and hand it to `visit` exactly once.
    fn with_state(&mut self, memory: &mut dyn MemoryReader, visit: &mut dyn FnMut(&mut dyn MacroState));
}

/// Which scenes count as the same scene for the abort rule.
///
/// `Battle { own_turn }` flips several times inside one turn as the game animates, so the payload
/// cannot be part of the comparison; a forced switch is a different palette, so it is its own
/// class. `Unknown` and `Dialog` are one class because section 2 says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Title,
    Overworld,
    Talking,
    Menu,
    Battle,
    ForcedSwitch,
    Shop,
    Pc,
}

fn class(scene: Scene) -> Class {
    match scene {
        Scene::Title => Class::Title,
        Scene::Overworld => Class::Overworld,
        Scene::Dialog | Scene::Unknown => Class::Talking,
        Scene::Menu => Class::Menu,
        Scene::Battle { forced_switch: true, .. } => Class::ForcedSwitch,
        Scene::Battle { .. } => Class::Battle,
        Scene::Shop => Class::Shop,
        Scene::Pc => Class::Pc,
    }
}

/// A tile to walk to, and what to do once standing on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Goal {
    tile: Tile,
    arrival: Arrival,
    /// How the session's target ledgers name what this goal is for, when it is a target in its
    /// own right (`docs/design/macros.md` section 12). Every goal of one `GO ITEM` carries the
    /// same key -- the four tiles around one object -- and every goal of one `GO ROUTE` carries
    /// its own, which is why the key travels per goal and the walk reports the one it aimed at.
    key: Option<TargetKey>,
}

/// What arriving on a goal tile means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
    /// Standing on it was the point: an interior warp fires by itself, so the walk waits a beat
    /// for the map to change and then reports done either way.
    Settle,
    /// One press, to turn and face what is on the next tile. Walking into an occupied tile turns
    /// the player without moving them, which is how `GO NPC` faces an NPC with the same button
    /// the fly's raw presses use.
    Face(Facing),
    /// Press outward until the map changes, which is how a doormat is stepped off. A press that
    /// leaves the player where they were is a failed step and counts toward `Blocked`.
    Leave(Facing),
    /// One full step's worth of a press this way: it faces what is there, and when the tile is
    /// walkable it steps onto it. `GO FRONTIER`'s arrival (`docs/design/macros.md` section 9,
    /// "walk there and face it"): the tile it aims at is ground the run has not stood on, and a
    /// press held for a whole tile is what turns "facing new ground" into "standing on it". A
    /// press that does not move the player is not a failure -- it faced it, which is the
    /// contract's own word -- so this always finishes as `Done`.
    Step(Facing),
}

/// One instruction of a script.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// One button pulse.
    Press { mask: u8, phase: u32 },
    /// Press nothing while the game redraws.
    Settle { phase: u32 },
    /// Pulse `mask` until the scene changes out from under the macro, at most `left` times. The
    /// scene change is what ends it, so this step only ever *runs out*, and running out is
    /// `Blocked`.
    Repeat { mask: u8, left: u8, phase: u32 },
    /// Walk to the nearest goal, one tile at a time, re-planned after every tile.
    Walk(Walk),
    /// Put the cursor on `target` by watching it move, then confirm.
    Cursor(Cursor),
    /// Press nothing until the party reads full and healthy, or until the wait runs out.
    ///
    /// `HEAL`'s middle (`docs/design/macros.md` section 13). A *read*, not a timer: the step ends
    /// the frame `party_rested` is true, so a heal that was quick and a heal that was slow both
    /// end when the cartridge says the party is well. Running out of frames is not `Blocked` --
    /// the presses happened and the conversation may simply have been about something else -- so
    /// it reports `Next` and the close press still happens.
    Rested { waited: u32 },
}

/// Progress through one A* walk.
#[derive(Debug, Clone, PartialEq)]
struct Walk {
    goals: Vec<Goal>,
    /// The map the walk was planned on. A different one means it arrived somewhere.
    map: u8,
    /// The route the walk is committed to: one direction per tile still to walk.
    ///
    /// **Committed, not re-planned per tile**, which is the oscillation fix. The walkable
    /// predicate's window is the ten-by-nine screen buffer and it moves with the player, so a
    /// fresh A* after every tile is a fresh *map* after every tile: a tile that read
    /// [`Walkable::Unknown`] at eight tiles a step becomes a wall as the window reaches it, the
    /// cheapest route flips to the other side of the obstacle, and the step back makes it unknown
    /// again. That is a walk that moves every frame, never arrives and spends the whole cap — two
    /// tiles and twelve timeouts every two brain minutes when the trap hunt first measured it, and
    /// every walk in Viridian for eight hours once the town was wider than one cap.
    ///
    /// So the plan is made once and followed while the player is still on it ([`Walk::expect`]).
    /// It is re-planned only when the ground says so: the plan ran out, a step was refused, or the
    /// player is not where the plan left it (a ledge hop, a spin tile, a warp).
    plan: VecDeque<Facing>,
    /// The tile the head of [`Walk::plan`] steps from, i.e. where the plan expects the player.
    expect: Option<Tile>,
    /// Steps this walk has watched the game refuse, as directed walls for the re-plan.
    ///
    /// [`path::Refusal`] has the two cartridge rules the collision table cannot answer — ledges
    /// and tile-pair collisions — and why a refusal is one-way.
    refused: Vec<path::Refusal>,
    /// Frames this walk may spend, from the length of the route it planned ([`walk_budget`]).
    budget: u32,
    /// Frames since the walk last got nearer a goal than it had ever been.
    ///
    /// The live half of "a walk that is making progress never times out": while this is under
    /// [`PROGRESS_GRACE`] the budget does not end the walk, and only [`WALK_FRAME_CEILING`] does.
    idle: u32,
    /// The direction being held, and the tile it started from.
    holding: Option<(Facing, Tile)>,
    held: u32,
    /// Frames of released D-pad still owed before the next tile.
    gap: u32,
    failures: u8,
    /// What the walk is doing now that it stands on its goal.
    arrival: Option<Arrival>,
    /// The tile the arrival began on, for [`Arrival::Step`]'s moved check.
    arrived: Option<Tile>,
    /// The distance to the nearest goal when the walk began, and the least it has been since.
    ///
    /// What separates "unreachable" from "further away than ten seconds of frames": the frame cap
    /// ends a walk across a town half way, and a walk that got *closer* has not failed at its
    /// target, so it writes no blocked entry and the next hold resumes it. Viridian City,
    /// 2026-09-17: a `GO ROUTE` toward the Route 2 connection timed out on every attempt, excluded
    /// that connection for ten brain minutes each time, and took `GO OBJECTIVE` off the pad with it
    /// -- both aim at the same exit key (`infra/docs/macros-traps.md`).
    ///
    /// Distance and not "did the player move", which was the first reading of it and which the
    /// trap hunt ruled out in one run: the A* re-plans every tile over a walkable predicate whose
    /// window is the screen, so a walk can step between two tiles for the whole cap, moving every
    /// time and arriving nowhere. Twelve such macros every two brain minutes, on two tiles, was
    /// what "moved" bought. Closer is the honest question.
    start_distance: u32,
    best_distance: u32,
    /// Whether this walk is the *first* step of a longer script rather than the whole of one.
    ///
    /// Arriving normally ends the macro — every walking macro is one walk and nothing else, and
    /// [`Progress::Finished`] says "the whole macro is done, whatever is left of the plan". Section
    /// 13's `HEAL` is the first script with a walk *and* presses after it, so its walk reports
    /// [`Progress::Next`] on arrival and the presses follow. Nothing else sets it, so no existing
    /// script changes by a frame.
    continues: bool,
    /// Whether the arrival press ended with the player on the tile it began on.
    ///
    /// `Arrival::Step` is `GO FRONTIER`'s, and the contract's word for a press that does not move
    /// the player is that it *faced* the new ground, so the outcome stays `Done`. But the tile is
    /// still a frontier -- the exploration ledger never records ground nobody stood on -- so
    /// without this the same tile is aimed at once per hold for ever, which is what a ledge, a
    /// tile-pair rule or a person on the far side of it looks like.
    stalled: bool,
}

/// Progress through one cursor navigation.
#[derive(Debug, Clone, PartialEq)]
struct Cursor {
    target: u8,
    confirm: bool,
    /// Which list `target` indexes into, when the script knows -- and it always does when it has
    /// just pressed A to open one.
    ///
    /// `None` navigates whatever list is up, which is what a step that does not cross a boundary
    /// wants: the shop's own screens are one [`ListKind`] and the step after a purchase's A press
    /// is still the counter's.
    ///
    /// While the list up is a *different* one, the step waits rather than pressing at it, exactly
    /// as it waits for a list that reports no cursor at all (section 4: "never by counting
    /// presses"). Twenty settle frames is not always enough for the cartridge to draw the next
    /// list, and a step that reads the list it has already answered is pressing blind: it takes
    /// its length and its direction from four entries that are not the ones it is walking.
    ///
    /// Section 12.11 is where this came from, and it is also what *found* the reason `THROW BALL`
    /// was 63 starts and 63 `blocked`: the step began reporting which list it had been left
    /// looking at, and the answer was never the bag -- it was the party list, because
    /// `battle_entry`'s `ITEM` and `PKMN` were the other way round. That is fixed at the
    /// constants; this rule stands on its own.
    want: Option<ListKind>,
    /// The directions to try, in order, aimed at the target from where the cursor actually is.
    ///
    /// `None` until the list this step is for is accepting input, because "which way is the
    /// target" is a fact about that list and not about whatever was up when the script was built.
    order: Option<[Facing; 4]>,
    /// Which of them is being tried now.
    at: u8,
    /// The cursor value the current pulse started from.
    before: u8,
    /// Presses left in the budget, `None` until the list is up: twice the list plus slack, and
    /// the list whose length that is has to be the one being walked.
    left: Option<u8>,
    phase: u32,
    /// Frames spent waiting for the list to accept input.
    waited: u32,
    /// Set once the confirming A press has been issued.
    confirmed: bool,
}

/// The macro that currently owns the buttons.
#[derive(Debug, Clone, PartialEq)]
struct Active {
    name: &'static str,
    kind: MacroKind,
    class: Class,
    /// The scene classes this macro is allowed to run *through*, beyond the one it started in.
    ///
    /// Section 4's rule is "a scene change aborts at the next frame", and it stays that for every
    /// macro but one. `HEAL` is the exception and it has to be: the conversation at the nurse's
    /// counter *is* the macro (`docs/design/macros.md` section 13 -- "talk, answer YES, wait for
    /// the heal animation to end, close the box"), and the text box that opens the moment the A
    /// press lands is a different class from the overworld the walk began in. With the flat rule
    /// the macro ended `Done` on the frame the box appeared, having pressed A at a nurse and
    /// nothing else, so the YES and the close were never its to make.
    ///
    /// Declared per macro and empty for all twenty-six others, so nothing else's behaviour moves
    /// by a frame ([`spanned`]).
    spans: &'static [Class],
    frames: u32,
    /// Frames this macro may spend: [`FRAME_CAP`] for a script, the walk's own budget for a walk.
    cap: u32,
    plan: VecDeque<Step>,
    /// What a `TALK` was facing when it started, and the map it was on: the talked ledger's entry,
    /// written only if the macro finishes *and* the conversation then ends cleanly
    /// (`docs/design/macros.md` sections 12 and 12.4).
    facing: Option<(u8, TalkTarget)>,
    /// The tile the macro started on, for the two questions that are about the fly being moved:
    /// whether a conversation ended with the game walking it away, and whether a push-back
    /// happened under a macro that was not walking.
    from: Option<Tile>,
    /// The box a `YES` or `NO` is answering, read at `start` because by the time the press has
    /// landed the box has moved on (section 12.12). `None` for every other macro.
    answered: Option<Answered>,
    /// What this macro is walking to, and the map it was chosen on: the target ledgers' entry.
    ///
    /// Chosen at `start`, from the state the fly chose in, for the same reason `facing` is: by the
    /// time the macro aborts the candidate list has moved on, and "which target did this fail at"
    /// has to be the one it set out for. `None` for every macro that does not walk.
    target: Option<(u8, TargetKey)>,
}

/// The box a `YES` or `NO` was answering, as it read when the answer was chosen.
///
/// `docs/design/macros.md` section 12.12. Read at `start`: the whole point of the reading is that
/// the *same* prompt comes back, and by the time the press has landed the box on screen is
/// whatever the answer led to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Answered {
    map: u8,
    /// The tile the fly answered from, which is what the box belongs to.
    at: Tile,
    yes: bool,
    /// Whether the box was a prompt this crate can read at all. An answer to a plain text box
    /// cannot be judged by "the same prompt came back", because no prompt was there to come back.
    prompt: bool,
    /// The nurse this prompt belongs to, when it is hers: the talked-ledger entry a *declined*
    /// heal earns (section 12.12).
    nurse: Option<TalkTarget>,
}

/// An answer that has been made and whose box may yet come straight back
/// ([`MacroMachine::pending_answer`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingAnswer {
    answered: Answered,
    /// Frames since the answer finished. The window is one hold: a prompt that comes back later
    /// than that came back because something else happened.
    frames: u32,
}

/// A conversation that has been started and has not ended yet ([`MacroMachine::pending_talk`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingTalk {
    map: u8,
    target: TalkTarget,
    /// The tile the fly was standing on when it pressed A.
    at: Tile,
}

/// A walk the frame cap cut short, as the next start needs it.
///
/// Keyed by the target rather than by the macro, because that is what a resumed walk *is*: the
/// same journey to the same exit, thing or tile, whichever button the fly presses to carry it on.
/// `GO OBJECTIVE` and `GO ROUTE` aim at one `TargetKey::Exit` between them, so either resumes the
/// other's route — which is the pair the Viridian stall poisoned together.
#[derive(Debug, Clone, PartialEq)]
struct Suspended {
    map: u8,
    key: TargetKey,
    /// Where the walk stopped. A resume is only honest from the tile the route was suspended at.
    at: Tile,
    /// The steps still to walk.
    plan: VecDeque<Facing>,
    /// What the walk had learned the cartridge refuses, so a resume does not pay for it twice.
    refused: Vec<path::Refusal>,
}

/// The executor proper: one script at a time, over agent A's seam.
#[derive(Debug, Clone)]
pub struct MacroMachine {
    active: Option<Active>,
    outcome: Option<(&'static str, MacroAbort)>,
    /// Targets a `Blocked`, `Timeout` or `no route` earned, waiting to be taken into the session's
    /// blocked ledger, and one a completed `GO ITEM` or `GO NPC` earned for the reached ledger.
    ///
    /// Recorded rather than kept, exactly as `talked` is, and for the same reason: the state a
    /// macro reads is built around the ledgers, so a machine that owned them could not be read
    /// from and written to in one frame.
    ///
    /// A queue rather than one entry because a refusal can earn several at once: a walking macro
    /// that finds no route to *any* of its goals has failed at all of them, and each is a key.
    blocked: Vec<(u8, TargetKey)>,
    reached: Option<(u8, TargetKey)>,
    /// A map whose frontier a `GO FRONTIER` has just proved unreachable, waiting to be taken
    /// into the session's ledger ([`super::cartridge::Frontiers`], section 12.14).
    ///
    /// The same shape as `pushed_tile` and for the same reason: it is a fact about the *ground*
    /// rather than about a target, so the blocked ledger's ten-minute window is the wrong home
    /// for it -- the window is what brought forty unreachable museum tiles back every ten
    /// minutes, once per hold, for hours.
    exhausted: Option<u8>,
    /// A tile the cartridge pushed the fly off, waiting to be taken into the session's ledger.
    ///
    /// Row 37 of `infra/docs/macros-traps.md`: a scripted push-back is a fact about the *ground*,
    /// and the blocked ledger could only say it about the target the walk was aimed at. Recorded
    /// from the frame the push is seen, because by the time the next macro starts the fly has been
    /// walked somewhere else.
    pushed_tile: Option<(u8, Tile)>,
    /// A refusal the route search or the precondition made, and where the fly stood for it --
    /// `(map, slot, tile)`, waiting to be taken into the session's ledger
    /// ([`super::cartridge::Targets::record_refused`], row 57 of `infra/docs/macros-traps.md`).
    refused_at: Option<(u8, u8, Tile)>,
    /// The target a frame-cap `Timeout` spent itself on, and whether the walk ended nearer a goal
    /// than it began: [`super::cartridge::Targets::record_timeout`]'s two arguments.
    ///
    /// Separate from `blocked` because the ledger, not the machine, decides what a timeout means:
    /// one that got no closer excludes its target at once and one that did costs a strike, and
    /// counting strikes is session state that lives with the other two ledgers.
    timed_out: Option<(u8, TargetKey, bool)>,
    /// Walks the frame cap cut short, kept so the next hold can carry them on.
    ///
    /// `docs/design/macros.md` section 12.1, amended 2026-09-17: "a walk interrupted by the cap
    /// resumes from where it stopped on the next start". Without it every attempt at a target
    /// further than one budget away re-planned from the fly's tile, walked the same first tiles
    /// again and timed out again in the same place — which is what eight hours in Viridian was.
    ///
    /// Session state in the executor layer, exactly like the talked and blocked ledgers, and for
    /// the same reason: it is a fact about this run's walking and not about the save. It does not
    /// reach the checkpoint, so a restored run plans afresh.
    resume: VecDeque<Suspended>,
    /// A finished `TALK` whose conversation has not ended yet.
    ///
    /// `docs/design/macros.md` section 12.4: **the talked ledger records a conversation only when
    /// the dialog closed without the game moving the fly.** `TALK` is one A press and it finishes
    /// while the box is still open, so "talked to" cannot be decided there. Three ways it ends,
    /// and only the first writes the ledger:
    ///
    /// - the box closes with the fly where it was and nothing driving it — talked;
    /// - the cartridge takes the joypad and walks the fly away ([`MacroState::scripted`]) — not
    ///   talked, because what the conversation did was refuse;
    /// - the fly answers `NO` — not talked, because the thing it said no to is still on offer.
    ///
    /// The Viridian gate is the first case's opposite: a still sprite in the road whose script
    /// shows a box and pushes the fly back one tile. Marked talked, it left `TALK` off the pad and
    /// `GO ITEM`'s list, so the one conversation that might change something could not be had
    /// again.
    pending_talk: Option<PendingTalk>,
    /// A finished `YES` or `NO` whose prompt may yet come straight back.
    ///
    /// `docs/design/macros.md` section 12.12: **a YES/NO box that reopens after an answer with
    /// nothing changed is section 12.2's trap** -- the answer completed, the fly is on the tile it
    /// answered from, and the same question is being asked again, so the press did nothing that
    /// the next press will not undo. One hold of frames is the window, because that is how long
    /// the fly has to choose again; anything later and something else happened in between.
    pending_answer: Option<PendingAnswer>,
    /// A finished `TALK`'s target, waiting to be taken into the session's talked ledger.
    ///
    /// The machine records rather than keeps: the ledger is the driver's
    /// ([`super::driver::PokemonPalette`]), because the state a macro reads is built around it and
    /// a machine that owned it could not be read from and written to in the same frame.
    talked: Option<(u8, TalkTarget)>,
    /// Kept for a macro with a random component. Nothing in the palette has one since
    /// `GO FRONTIER` replaced `WANDER` (`docs/design/macros.md` section 9): the whole palette is
    /// now deterministic given the game state, which is what "no random steps remain anywhere"
    /// means. Still seeded and still carried, so that a run stays reproducible if one ever does.
    #[allow(dead_code)]
    rng: u32,
}

impl MacroMachine {
    /// A machine seeded with `seed`.
    ///
    /// Seeded rather than sampled from the clock so a run is reproducible. No macro has a random
    /// component any more (`docs/design/macros.md` section 9), so today this changes nothing about
    /// a run; section 7's measurement compares runs and the seed stays for that.
    pub fn new(seed: u32) -> Self {
        Self {
            active: None,
            outcome: None,
            blocked: Vec::new(),
            reached: None,
            exhausted: None,
            pushed_tile: None,
            refused_at: None,
            timed_out: None,
            resume: VecDeque::new(),
            pending_talk: None,
            pending_answer: None,
            talked: None,
            rng: if seed == 0 { 1 } else { seed },
        }
    }

    /// Begin a macro, or refuse without pressing anything.
    pub fn start(
        &mut self,
        palette: &Palette,
        slot: MacroId,
        state: &mut dyn MacroState,
    ) -> Result<(), MacroRefused> {
        if self.active.is_some() {
            return Err(MacroRefused { slot, reason: Refusal::Busy });
        }
        let Some(spec) = palette.slot(slot) else {
            self.outcome = Some(("", MacroAbort::Refused));
            return Err(MacroRefused { slot, reason: Refusal::Unbound });
        };
        let scene = state.scene();
        let refuse = |machine: &mut Self, reason| {
            machine.outcome = Some((spec.name, MacroAbort::Refused));
            Err(MacroRefused { slot, reason })
        };
        // Where the fly stands for a refusal that is a fact about *here* (row 57): no route from
        // this tile, or a precondition the dealer and the starter answered differently on it.
        let here = state.player().map(|player| (player.map, Tile::new(player.x, player.y)));
        let refused_here = |machine: &mut Self| {
            if let Some((map, tile)) = here {
                machine.refused_at = Some((map, slot.0, tile));
            }
        };
        if class(scene) != class(palette.scene) {
            return refuse(self, Refusal::WrongScene);
        }
        if !precondition(spec.kind, state) {
            refused_here(self);
            return refuse(self, Refusal::Precondition);
        }
        let mut unreachable: Vec<TargetKey> = Vec::new();
        let Some((mut plan, aimed)) = script(spec.kind, state, &mut unreachable) else {
            // Section 12.1's blocked ledger, from the other end of the same fact. The palette's
            // preconditions are deliberately the cheap question -- "does this map have an exit at
            // all" -- and the route search is the real one, so a bound macro can refuse `no route`
            // once per hold for ever while the candidate list never changes. Recording what it
            // could not reach is what empties the list and takes the button off the pad.
            if let Some(map) = state.player().map(|player| player.map) {
                self.blocked.extend(unreachable.into_iter().map(|key| (map, key)));
                // And for the frontier, the same fact one level up: every tile of this map the
                // run has not stood on is unreachable from where the fly is standing. The
                // blocked ledger is a window and this is not -- ground the map has fenced off is
                // still fenced off ten brain minutes later (section 12.14).
                if spec.kind == MacroKind::GoFrontier {
                    self.exhausted = Some(map);
                }
            }
            refused_here(self);
            return refuse(self, Refusal::NoRoute);
        };
        // The map the target was chosen on, so an entry cannot be read back on another map.
        let target = aimed.and_then(|key| state.player().map(|player| (player.map, key)));
        // A walk toward a target a previous hold ran out of frames on carries on from where that
        // one stopped, instead of planning the same first tiles again.
        if let Some((map, key)) = target
            && let Some(here) = state.player().map(|player| Tile::new(player.x, player.y))
        {
            self.take_resume(&mut plan, map, key, here);
        }
        // A walk's cap is its own; every other script keeps section 4's flat ten seconds.
        let cap = match plan.front() {
            Some(Step::Walk(walk)) => walk.budget,
            _ => FRAME_CAP,
        };
        // What this macro is facing, if it is the one press that talks to it. Read here, from the
        // state the fly chose in, rather than at the end: a dialog is open by then and the tile
        // ahead is not answerable.
        let facing = (spec.kind == MacroKind::Talk)
            .then(|| talk_target(state))
            .flatten();
        // And which box a `YES` or `NO` is answering, read here for the same reason: the answer
        // is about the box that was on screen when the fly chose it (section 12.12).
        let answered = matches!(spec.kind, MacroKind::Yes | MacroKind::No)
            .then(|| {
                let prompt = state.yes_no_prompt();
                let nurse = nurse_prompt(state).then(|| talk_target(state)).flatten();
                state.player().map(|player| Answered {
                    map: player.map,
                    at: Tile::new(player.x, player.y),
                    yes: spec.kind == MacroKind::Yes,
                    prompt,
                    nurse: nurse.map(|(_, target)| target),
                })
            })
            .flatten();
        self.outcome = None;
        let from = state.player().map(|player| Tile::new(player.x, player.y));
        self.active = Some(Active {
            name: spec.name,
            kind: spec.kind,
            class: class(scene),
            spans: spanned(spec.kind),
            frames: 0,
            cap,
            plan,
            facing,
            answered,
            from,
            target,
        });
        Ok(())
    }

    /// One frame of the running macro: the mask to hold, or `None` when it has finished.
    pub fn step(&mut self, state: &mut dyn MacroState) -> Option<u8> {
        let (frames, started_in, spans, cap, closing) = {
            let active = self.active.as_ref()?;
            (active.frames, active.class, active.spans, active.cap, closing(active))
        };
        // Rule one: the cap, before anything is read or pressed.
        //
        // Two numbers rather than section 4's one, and only for a walk. A walk's `cap` is its own
        // plan's ([`walk_budget`]), and a walk that is *still closing on a goal* is not out of
        // frames at all: "a walk that is making progress never times out" (section 12.1, amended
        // 2026-09-17), bounded by [`WALK_FRAME_CEILING`] so the pad always comes back. Every other
        // script has `cap == FRAME_CAP`, `closing == false` and `frames` far under the ceiling,
        // which is section 4 unchanged.
        if frames >= WALK_FRAME_CEILING || (frames >= cap && !closing) {
            return self.finish(MacroAbort::Timeout, false, false, None);
        }
        // Rule two: a scene change ends the macro at the next frame. The macro did not arrive
        // anywhere -- the world moved on under it -- so it earns neither ledger entry: `Done`
        // here is "nothing is stuck", not "the target was reached".
        //
        // Unless the world moved on *by moving the fly*. A script that takes the joypad and walks
        // the player back is the cartridge refusing the step, and it is the one scene change that
        // is a fact about the target (`docs/design/macros.md` section 12.4): the Viridian gate
        // ended every walk north this way, `Done` with nothing recorded, once per hold for eight
        // hours.
        let now = class(state.scene());
        if now != started_in && !spans.contains(&now) {
            let pushed = state.scripted() || self.moved_away(state);
            let at = pushed.then(|| state.player()).flatten();
            return self.finish(MacroAbort::Done, false, pushed, at);
        }
        loop {
            let decided = {
                let active = self.active.as_mut()?;
                match active.plan.front_mut() {
                    None => Decided::End(MacroAbort::Done),
                    Some(step) => match advance(step, state) {
                        Progress::Hold(mask) => Decided::Hold(mask),
                        Progress::Next => Decided::Pop,
                        Progress::Blocked => Decided::End(MacroAbort::Blocked),
                        Progress::Finished => Decided::End(MacroAbort::Done),
                    },
                }
            };
            match decided {
                Decided::Hold(mask) => {
                    self.active.as_mut()?.frames += 1;
                    return Some(mask);
                }
                Decided::End(outcome) => {
                    let pushed = state.scripted();
                    let at = pushed.then(|| state.player()).flatten();
                    return self.finish(outcome, true, pushed, at);
                }
                Decided::Pop => {
                    self.active.as_mut()?.plan.pop_front();
                }
            }
        }
    }

    /// The name of the macro that owns the buttons, if any.
    pub fn running(&self) -> Option<&'static str> {
        self.active.as_ref().map(|active| active.name)
    }

    /// How the last macro ended.
    pub fn outcome(&self) -> Option<(&'static str, MacroAbort)> {
        self.outcome
    }

    /// How the last macro ended, taken rather than read.
    ///
    /// The sim loop polls this once per frame and turns each answer into one `macro` feed event
    /// (`docs/design/macros.md` section 5), so it needs "what is new" rather than "what is
    /// current": reading [`MacroMachine::outcome`] every frame would emit the same finish until
    /// the next macro started. `running()` and `outcome()` are unchanged for callers that want
    /// the standing value.
    pub fn take_outcome(&mut self) -> Option<(&'static str, MacroAbort)> {
        self.outcome.take()
    }

    /// The talked-ledger entry a finished `TALK` earned, taken rather than read.
    ///
    /// Polled once per frame beside [`MacroMachine::take_outcome`], which is what makes it "when
    /// TALK completes facing it" rather than "when TALK was chosen": a refused or aborted press
    /// records nothing.
    pub fn take_talked(&mut self) -> Option<(u8, TalkTarget)> {
        self.talked.take()
    }

    /// One blocked-ledger entry a `Blocked`, `Timeout` or `no route` earned, taken rather than
    /// read. Call it until it answers `None`: a refusal can earn several.
    ///
    /// Polled once per frame beside [`MacroMachine::take_outcome`], so the entries are written from
    /// the same finish the feed reports and a macro that never started records nothing.
    pub fn take_blocked(&mut self) -> Option<(u8, TargetKey)> {
        if self.blocked.is_empty() { None } else { Some(self.blocked.remove(0)) }
    }

    /// The reached-ledger entry a completed `GO ITEM` or `GO NPC` earned, taken rather than read.
    pub fn take_reached(&mut self) -> Option<(u8, TargetKey)> {
        self.reached.take()
    }

    /// The map a `no route` from `GO FRONTIER` earned, taken rather than read (section 12.14).
    pub fn take_exhausted(&mut self) -> Option<u8> {
        self.exhausted.take()
    }

    /// The tile a scripted push-back earned, taken rather than read (row 37).
    pub fn take_pushed(&mut self) -> Option<(u8, Tile)> {
        self.pushed_tile.take()
    }

    /// Where the last `no route` or `precondition` refusal happened, taken rather than read
    /// (row 57).
    pub fn take_refused(&mut self) -> Option<(u8, u8, Tile)> {
        self.refused_at.take()
    }

    /// The frame-cap timeout a walk earned, with whether it ended nearer its goal, taken rather
    /// than read. The ledger decides what it means.
    pub fn take_timeout(&mut self) -> Option<(u8, TargetKey, bool)> {
        self.timed_out.take()
    }

    /// Frames the running macro has spent so far, for the stage's `sinceMs`.
    pub fn frames(&self) -> u32 {
        self.active.as_ref().map_or(0, |active| active.frames)
    }

    /// Give up on whatever is running, e.g. because the sim loop is rolling the game back.
    pub fn cancel(&mut self) {
        if let Some(active) = self.active.take() {
            self.outcome = Some((active.name, MacroAbort::Blocked));
        }
        // A rollback is the loop's doing and not the map's, so nothing the abandoned macro was
        // aiming at is excluded: the driver drops these too, and this is the machine's own half of
        // that so a cancelled queue cannot leak into the next macro's finish.
        self.blocked.clear();
        self.reached = None;
        // A rollback is not the map pushing the fly anywhere, and it is not the frontier being
        // out of reach either: the fly is about to be somewhere else entirely.
        self.pushed_tile = None;
        self.exhausted = None;
        self.refused_at = None;
        self.timed_out = None;
        // A rollback puts the fly somewhere else on the map, so every suspended route is a route
        // from a tile it is no longer standing on. `take_resume` would refuse them one at a time;
        // dropping them here says so once.
        self.resume.clear();
        // A rollback is not the end of a conversation; it is the end of the frames it happened in.
        self.pending_talk = None;
        // Nor is it a prompt reopening: the frames the answer was made in are being thrown away.
        self.pending_answer = None;
    }

    /// Whether the fly is standing somewhere other than where the running macro began.
    ///
    /// The other half of "the game moved the fly": a script can finish its push-back inside the
    /// same frame the scene changes, so [`MacroState::scripted`] has already gone false while the
    /// fly is a tile from where it pressed. Only asked of a macro that was not walking — a walk
    /// moves the fly on purpose, and its own three-failure and refusal rules are what judge it.
    fn moved_away(&self, state: &mut dyn MacroState) -> bool {
        let Some(active) = self.active.as_ref() else { return false };
        if matches!(active.plan.front(), Some(Step::Walk(_))) {
            return false;
        }
        let Some(from) = active.from else { return false };
        state.player().is_some_and(|player| Tile::new(player.x, player.y) != from)
    }

    /// One frame with no macro running: decide whether a conversation has ended, and how.
    ///
    /// Called once per frame by the driver beside `observe`, because the answer arrives *after*
    /// the `TALK` that asked the question has given the buttons back (`docs/design/macros.md`
    /// section 12.4). Three ends, and only the first writes the talked ledger:
    ///
    /// - the box closed with the fly on the tile it pressed from and nothing driving it — talked;
    /// - the cartridge took the joypad, or the fly is somewhere else — not talked;
    /// - the fly answered `NO` — not talked, and that one is decided in [`MacroMachine::finish`].
    pub fn observe_frame(&mut self, state: &mut dyn MacroState) {
        self.observe_answer(state);
        let Some(pending) = self.pending_talk else { return };
        if state.scripted() {
            self.pending_talk = None;
            return;
        }
        let Some(player) = state.player() else { return };
        if player.map != pending.map || Tile::new(player.x, player.y) != pending.at {
            self.pending_talk = None;
            return;
        }
        // Still on the tile it pressed from, still its own master. The conversation is over when
        // the text is gone.
        if class(state.scene()) != Class::Talking {
            self.pending_talk = None;
            // A conversation the fly *declined its way out of* is not a conversation it has had:
            // whatever it said no to is still on offer, which is section 12.4's rule and the one
            // 12.12 inverts for the nurse alone. The box closing is what tells that apart from a
            // `NO` pressed dozens of boxes deep, and [`MacroMachine::pending_answer`] is the
            // reading: it is armed only by an answer to a prompt this crate can read and it lives
            // for one hold, so a declining `NO` still standing here is a `NO` this box closed on.
            if !self.declined_out_of(pending.map) {
                self.talked = Some((pending.map, pending.target));
            }
        }
    }

    /// Whether the answer still standing on `map` is a `NO` to a readable prompt: a declined offer
    /// rather than a conversation walked through (section 12.20).
    fn declined_out_of(&self, map: u8) -> bool {
        self.pending_answer.is_some_and(|pending| {
            pending.answered.map == map && pending.answered.prompt && !pending.answered.yes
        })
    }

    /// One frame after a `YES` or `NO`: decide whether the box it answered has come straight back.
    ///
    /// `docs/design/macros.md` section 12.12. The evidence is all in one frame: the fly is on the
    /// tile it answered from, and the prompt it answered is up again. Nothing moved and nothing
    /// was settled, so the answer goes into the blocked ledger for its window and the dialog pad
    /// offers the *other* one -- which at the nurse's counter is the `NO` that ends the ring.
    ///
    /// Only for an answer to a prompt this crate can **read**. An answer to a plain text box is
    /// not judged here, because "the same prompt came back" is not a question that has a meaning
    /// there: a conversation is many boxes and advancing one is exactly what `YES` should do.
    fn observe_answer(&mut self, state: &mut dyn MacroState) {
        let Some(pending) = self.pending_answer.as_mut() else { return };
        pending.frames += 1;
        let answered = pending.answered;
        if pending.frames > ANSWER_REOPEN_FRAMES || !answered.prompt {
            self.pending_answer = None;
            return;
        }
        let Some(player) = state.player() else { return };
        if player.map != answered.map || Tile::new(player.x, player.y) != answered.at {
            self.pending_answer = None;
            return;
        }
        if state.yes_no_prompt() {
            self.blocked.push((
                answered.map,
                TargetKey::Answer { at: answered.at, yes: answered.yes },
            ));
            self.pending_answer = None;
        }
    }

    /// Keep `walk`'s remaining route, so the next walk toward the same target carries it on.
    ///
    /// Only the route and what the walk learned about the ground: the goals are chosen again from
    /// the state the fly is in when it presses, because the candidate list is the palette's and it
    /// may have moved on. Nothing is kept for a walk with nothing left to walk, and nothing is
    /// kept while the player is mid-step — [`Walk::expect`] is where the route resumes from and a
    /// walk holding a direction is between two tiles.
    fn suspend(&mut self, map: u8, key: TargetKey, walk: &Walk) {
        let Some(at) = walk.expect else { return };
        if walk.plan.is_empty() || walk.holding.is_some() {
            self.forget_resume(map, key);
            return;
        }
        self.forget_resume(map, key);
        if self.resume.len() >= RESUMES {
            self.resume.pop_front();
        }
        self.resume.push_back(Suspended {
            map,
            key,
            at,
            plan: walk.plan.clone(),
            refused: walk.refused.clone(),
        });
    }

    /// Drop any suspended route for this target.
    fn forget_resume(&mut self, map: u8, key: TargetKey) {
        self.resume.retain(|held| held.map != map || held.key != key);
    }

    /// Put a suspended route back into `plan`'s walk, if one belongs to this target and tile.
    ///
    /// The tile has to match. A route is a list of directions from one tile, so resuming it from
    /// anywhere else would walk the fly along somebody else's path — and between two holds the fly
    /// is free to press a raw button, take a warp or be stood up by a battle. A mismatch is not an
    /// error: the fresh plan `script` just made is already in place and the stale route is dropped.
    fn take_resume(&mut self, plan: &mut VecDeque<Step>, map: u8, key: TargetKey, here: Tile) {
        let Some(index) =
            self.resume.iter().position(|held| held.map == map && held.key == key)
        else {
            return;
        };
        if self.resume[index].at != here {
            self.resume.remove(index);
            return;
        }
        let held = self.resume.remove(index).expect("the index came from this deque");
        if let Some(Step::Walk(walk)) = plan.front_mut() {
            walk.budget = walk_budget(held.plan.len());
            walk.plan = held.plan;
            walk.refused = held.refused;
            walk.expect = Some(here);
        }
    }

    /// End the running macro, and write whatever ledger entries the way it ended earns.
    ///
    /// `completed` is whether the script itself ran out -- the walk arrived, the arrival's press
    /// happened -- as opposed to the frame cap or a scene change cutting it short. It is the
    /// difference between "`GO NPC` is standing in front of the villager" and "something else
    /// happened while it was walking", and only the first retires the target.
    fn finish(
        &mut self,
        outcome: MacroAbort,
        completed: bool,
        pushed: bool,
        at: Option<super::state::Player>,
    ) -> Option<u8> {
        if let Some(active) = self.active.take() {
            // A conversation the cartridge ended by moving the fly is not a conversation had.
            if pushed {
                self.pending_talk = None;
                // And the *tile* is what did it, which is the half the blocked ledger could not
                // say (row 37 of `infra/docs/macros-traps.md`). The fly may already have been
                // walked a tile by the script, so the tile the macro set out from is the honest
                // answer when there is one and the current tile otherwise.
                //
                // **Except for a walk** (row 57). A walk set out from wherever the last one left
                // the fly, and the script fired on the tile the walk had *reached*: Pewter City's
                // youngster takes the joypad on four tiles by the road east, and a `GO ROUTE` that
                // set out from the town's south entrance twenty-six tiles away walled the south
                // entrance -- with no window, in the middle of the town. A dozen of those fenced
                // the fly into a pocket no walk could leave. The tile a walk last stood the fly on
                // is its own record of where the cartridge took over.
                if let Some(player) = at {
                    let current = Tile::new(player.x, player.y);
                    let tile = match active.plan.front() {
                        Some(Step::Walk(walk)) => walk.expect.unwrap_or(current),
                        _ => active.from.unwrap_or(current),
                    };
                    self.pushed_tile = Some((player.map, tile));
                }
            }
            let (closer, stalled) = walk_flags(&active);
            // A walk the cap cut short keeps its route for the next hold; any other ending means
            // the route is spent, so a stale one is dropped rather than left to be resumed from a
            // tile the walk has moved off.
            match (outcome, active.target, active.plan.front()) {
                (MacroAbort::Timeout, Some((map, key)), Some(Step::Walk(walk))) => {
                    self.suspend(map, key, walk);
                }
                (_, Some((map, key)), _) => self.forget_resume(map, key),
                _ => {}
            }
            self.outcome = Some((active.name, outcome));
            match outcome {
                MacroAbort::Done => {
                    // A `TALK` that pressed its A is a conversation *begun*. Whether it counts as
                    // talked to is decided when the box closes, in
                    // [`MacroMachine::observe_frame`].
                    if let Some((map, target)) = active.facing
                        && let Some(at) = active.from
                    {
                        self.pending_talk = Some(PendingTalk { map, target, at });
                    }
                    // The fly said no, and whether that retires what it was facing is decided
                    // when the box closes rather than here (section 12.20). A `NO` inside a
                    // conversation is the B that advances a plain box -- it declines nothing --
                    // and clearing the pending talk on it kept the Pewter Gym guide `untalked`
                    // for thirty brain minutes: his conversation is fifty-two boxes long and
                    // about a third of the presses that walk it are `NO`.
                    // An answer, and the box it answered: armed so that the same prompt coming
                    // straight back is recorded (section 12.12), and the nurse written into the
                    // talked ledger when what was declined was *her* offer. That is 12.4's rule
                    // inverted, deliberately and for one person: "the thing it said no to is
                    // still on offer" is true of a villager with something to say and false of a
                    // service the party does not need -- the pad only ever offers `NO` at her
                    // prompt when the party is already full, and declining is the errand's end.
                    if let Some(answered) = active.answered {
                        if let Some(nurse) = answered.nurse.filter(|_| !answered.yes) {
                            self.talked = Some((answered.map, nurse));
                        }
                        self.pending_answer = Some(PendingAnswer { answered, frames: 0 });
                    }
                    // The cartridge answered the macro by walking the fly away: that is the
                    // target's own fact, not the world's, so it is excluded for the window like
                    // any other refusal. Without it the gate was walked into once per hold for
                    // ever, because every macro that hit it ended `Done`.
                    if pushed && let Some(entry) = active.target {
                        self.blocked.push(entry);
                    }
                    // A `GO FRONTIER` whose press faced new ground it could not stand on: `Done`,
                    // because facing it is what the arrival promises, and excluded, because the
                    // ledger is what stops the same tile being aimed at once per hold for ever.
                    if stalled && let Some(entry) = active.target {
                        self.blocked.push(entry);
                    }
                    // Only the macros whose whole promise is "arrive and face it" -- and section
                    // 13's three, because a counter the fly is standing at is what retires the
                    // errand's target and lets the way out of the building back onto the pad
                    // ([`super::palette::ways`]). A
                    // completed `GO ROUTE` has left the map, which the exploration ledger already
                    // knows, and a completed `GO FRONTIER` has stood on the new ground, which the
                    // visited ledger already knows -- neither needs a second opinion here.
                    if completed
                        && matches!(
                            active.kind,
                            MacroKind::GoNpc
                                | MacroKind::GoItem
                                | MacroKind::GoShop
                                | MacroKind::GoHeal
                                | MacroKind::Heal
                        )
                        && let Some(entry) = active.target
                    {
                        self.reached = Some(entry);
                        // And a completed `HEAL` has *had* the conversation: the box was opened,
                        // answered and closed by this macro's own presses, so the nurse is talked
                        // to and `TALK` has nothing left to open (section 12.12). Without it the
                        // reached window expires after ten brain minutes and the fly is offered
                        // the same forty-six text frames again with a party that is already full.
                        if active.kind == MacroKind::Heal
                            && let (map, TargetKey::Thing(target)) = entry
                        {
                            self.talked = Some((map, target));
                        }
                    }
                }
                // Section 12's blocked-target ledger: three failed steps, or the frame cap on a
                // walk that never moved. The fly may choose this macro again on the very next
                // hold, so the target it just failed at is what has to change.
                //
                // A `Timeout` on a walk that got *closer* records nothing, and that is the whole
                // of the Viridian fix: the frame cap is ten seconds and a town is wider than ten
                // seconds of walking, so timing out half way across one is not evidence that the
                // far side is unreachable. The next hold resumes the walk from where this one
                // stopped, and the exit the run is heading for stays on the pad -- for
                // `GO OBJECTIVE`, which aims at the same key, as much as for the walk itself. A
                // walk that spent the cap and ended no nearer than it began did fail at its
                // target, whether it stood still or stepped between two tiles for ten seconds.
                MacroAbort::Blocked => {
                    if let Some(entry) = active.target {
                        self.blocked.push(entry);
                    }
                }
                MacroAbort::Timeout => {
                    if let Some((map, key)) = active.target {
                        self.timed_out = Some((map, key, closer));
                    }
                }
                // Nothing was pressed and nothing was aimed at.
                MacroAbort::Refused => {}
            }
        }
        None
    }
}

/// A [`MacroMachine`] in the contract's `MacroExecutor` shape.
#[derive(Debug, Clone)]
pub struct Executor<S> {
    source: S,
    machine: MacroMachine,
}

impl<S: StateSource> Executor<S> {
    pub fn new(source: S, seed: u32) -> Self {
        Self { source, machine: MacroMachine::new(seed) }
    }

    /// The machine underneath, for the status fields the stage needs.
    pub fn machine(&self) -> &MacroMachine {
        &self.machine
    }

    /// The palette for this frame's scene.
    pub fn palette(&mut self, memory: &mut dyn MemoryReader) -> Option<Palette> {
        let mut out = None;
        self.source.with_state(memory, &mut |state| {
            out = Some(Palette::for_scene(state.scene(), state));
        });
        out
    }

    pub fn cancel(&mut self) {
        self.machine.cancel();
    }
}

impl<S: StateSource> MacroExecutor for Executor<S> {
    fn start(
        &mut self,
        palette: &Palette,
        slot: MacroId,
        memory: &mut dyn MemoryReader,
    ) -> Result<(), MacroRefused> {
        let Self { source, machine } = self;
        let mut out = None;
        source.with_state(memory, &mut |state| out = Some(machine.start(palette, slot, state)));
        out.unwrap_or(Err(MacroRefused { slot, reason: Refusal::Unbound }))
    }

    fn step(&mut self, memory: &mut dyn MemoryReader) -> Option<u8> {
        let Self { source, machine } = self;
        let mut out = None;
        source.with_state(memory, &mut |state| out = Some(machine.step(state)));
        out.flatten()
    }

    fn running(&self) -> Option<&'static str> {
        self.machine.running()
    }

    fn outcome(&self) -> Option<(&'static str, MacroAbort)> {
        self.machine.outcome()
    }
}

/// What [`MacroMachine::step`] settled on for this frame, once every borrow is back.
enum Decided {
    Hold(u8),
    Pop,
    End(MacroAbort),
}

/// What one frame of one step decided.
enum Progress {
    /// Hold this mask for the frame.
    Hold(u8),
    /// This step is done; move to the next one without spending a frame.
    Next,
    /// The whole macro is blocked.
    Blocked,
    /// The whole macro is done, whatever is left of the plan.
    Finished,
}

/// One frame of one step.
fn advance(step: &mut Step, state: &mut dyn MacroState) -> Progress {
    match step {
        Step::Press { mask, phase } => pulse(*mask, phase, PRESS_HOLD, PRESS_GAP),
        Step::Settle { phase } => {
            if *phase >= SETTLE_FRAMES {
                Progress::Next
            } else {
                *phase += 1;
                Progress::Hold(buttons::NONE)
            }
        }
        Step::Repeat { mask, left, phase } => {
            if *left == 0 {
                return Progress::Blocked;
            }
            match pulse(*mask, phase, PRESS_HOLD, PRESS_GAP) {
                Progress::Next => {
                    *left -= 1;
                    *phase = 0;
                    Progress::Hold(buttons::NONE)
                }
                other => other,
            }
        }
        Step::Walk(walk) => walk_frame(walk, state),
        Step::Cursor(cursor) => cursor_frame(cursor, state),
        Step::Rested { waited } => {
            if party_rested(state) || *waited >= HEAL_WAIT_FRAMES {
                Progress::Next
            } else {
                *waited += 1;
                Progress::Hold(buttons::NONE)
            }
        }
    }
}

/// Hold `mask` for `hold` frames, release for `gap`, then report [`Progress::Next`].
fn pulse(mask: u8, phase: &mut u32, hold: u32, gap: u32) -> Progress {
    if *phase < hold {
        *phase += 1;
        Progress::Hold(mask)
    } else if *phase < hold + gap {
        *phase += 1;
        Progress::Hold(buttons::NONE)
    } else {
        Progress::Next
    }
}

/// One frame of a walk: the moved check, the three-failure rule and the re-plan.
fn walk_frame(walk: &mut Walk, state: &mut dyn MacroState) -> Progress {
    let Some(player) = state.player() else { return Progress::Blocked };
    // Arriving on a different map is what the three ways out are for, and it voids any plan.
    if player.map != walk.map {
        return Progress::Finished;
    }
    if walk.gap > 0 {
        walk.gap -= 1;
        return Progress::Hold(buttons::NONE);
    }
    let here = Tile::new(player.x, player.y);
    // "Making progress" is measured here, once a frame, against the closest this walk has ever
    // been: a new best resets the idle count and the cap stops applying for another
    // [`PROGRESS_GRACE`] frames.
    let distance = nearest_goal(&walk.goals, here);
    if distance < walk.best_distance {
        walk.best_distance = distance;
        walk.idle = 0;
    } else {
        walk.idle = walk.idle.saturating_add(1);
    }

    if let Some(arrival) = walk.arrival {
        return match arrival {
            Arrival::Settle => {
                walk.held += 1;
                if walk.held <= SETTLE_FRAMES {
                    Progress::Hold(buttons::NONE)
                } else {
                    arrived(walk.continues)
                }
            }
            Arrival::Face(facing) => {
                let continues = walk.continues;
                match pulse(button(facing), &mut walk.held, PRESS_HOLD, PRESS_GAP) {
                    Progress::Next => arrived(continues),
                    other => other,
                }
            }
            Arrival::Step(facing) => {
                walk.held += 1;
                if here != goal_tile(walk) {
                    return arrived(walk.continues);
                }
                if walk.held > TILE_FRAMES {
                    // The press faced the new ground and did not stand on it, which is `Done` by
                    // the contract and an excluded target by section 12.1's own reasoning.
                    walk.stalled = true;
                    return arrived(walk.continues);
                }
                Progress::Hold(button(facing))
            }
            Arrival::Leave(facing) => {
                walk.held += 1;
                if walk.held <= TILE_FRAMES {
                    return Progress::Hold(button(facing));
                }
                // Still here, still on this map: the door did not open. Count it and re-plan.
                walk.arrival = None;
                walk.plan.clear();
                walk.expect = None;
                walk.held = 0;
                walk.gap = STEP_GAP;
                walk.failures += 1;
                if walk.failures >= MAX_FAILED_STEPS {
                    Progress::Blocked
                } else {
                    Progress::Hold(buttons::NONE)
                }
            }
        };
    }

    if let Some((facing, from)) = walk.holding {
        if here != from {
            // Moved: this tile is done and the failure run is broken. The plan's head is spent,
            // and the next one steps from where the player now stands.
            walk.plan.pop_front();
            walk.expect = Some(here);
            walk.holding = None;
            walk.held = 0;
            walk.failures = 0;
            walk.gap = STEP_GAP;
            return Progress::Hold(buttons::NONE);
        }
        walk.held += 1;
        if walk.held <= TILE_FRAMES {
            return Progress::Hold(button(facing));
        }
        // The window closed with the player on the same tile: one failed step. The game refuses
        // this step out of this tile — a ledge, a tile-pair collision, an NPC — so it becomes a
        // directed wall and the plan is thrown away for one that goes round it
        // ([`path::Refusal`]). Re-planning happens *here*, on a refusal, and nowhere else.
        if !walk.refused.contains(&(from, facing)) {
            walk.refused.push((from, facing));
        }
        walk.plan.clear();
        walk.expect = None;
        walk.holding = None;
        walk.held = 0;
        walk.gap = STEP_GAP;
        walk.failures += 1;
        return if walk.failures >= MAX_FAILED_STEPS {
            Progress::Blocked
        } else {
            Progress::Hold(buttons::NONE)
        };
    }

    // Plan, or re-plan.
    if let Some(goal) = walk.goals.iter().find(|goal| goal.tile == here) {
        walk.arrival = Some(goal.arrival);
        walk.arrived = Some(here);
        walk.plan.clear();
        walk.expect = None;
        walk.held = 0;
        // A released gap first, so the arrival press is a fresh one rather than a continuation of
        // whatever direction walked the last tile.
        walk.gap = STEP_GAP;
        return Progress::Hold(buttons::NONE);
    }
    // Commit to the plan while the player is still on it. A re-plan here is a fresh A* over a
    // walkable window that has moved with the player, which is the oscillation [`Walk::plan`]
    // documents; the plan is only remade when the ground has said something new.
    if walk.plan.is_empty() || walk.expect != Some(here) {
        let tiles: Vec<Tile> = walk.goals.iter().map(|goal| goal.tile).collect();
        let Some(Route { steps, .. }) = path::route_avoiding(state, &tiles, &walk.refused) else {
            return Progress::Blocked;
        };
        if steps.is_empty() {
            return arrived(walk.continues);
        }
        walk.plan = steps.into();
        walk.expect = Some(here);
    }
    let Some(first) = walk.plan.front().copied() else {
        return arrived(walk.continues);
    };
    walk.holding = Some((first, here));
    walk.held = 1;
    Progress::Hold(button(first))
}

/// The extra scene classes a macro is allowed to run through ([`Active::spans`]).
///
/// One row, and the empty slice everywhere else. `HEAL` walks in the overworld and finishes in a
/// text box, so it names both; every other macro keeps section 4's flat "a scene change aborts at
/// the next frame" exactly as it was.
const fn spanned(kind: MacroKind) -> &'static [Class] {
    match kind {
        MacroKind::Heal => &[Class::Overworld, Class::Talking],
        _ => &[],
    }
}

/// What a walk reports when it has arrived: the end of the macro, or the end of this step.
const fn arrived(continues: bool) -> Progress {
    if continues { Progress::Next } else { Progress::Finished }
}

/// Whether `active` is a walk that has got nearer a goal in the last [`PROGRESS_GRACE`] frames.
///
/// The live reading of "making progress", against `best_distance`'s "ever got closer". Only a walk
/// can answer yes, so every other script keeps section 4's flat cap.
fn closing(active: &Active) -> bool {
    match active.plan.front() {
        Some(Step::Walk(walk)) => walk.idle < PROGRESS_GRACE,
        _ => false,
    }
}

/// Frames a walk of `tiles` planned steps may spend.
///
/// [`FRAMES_PER_PLANNED_TILE`] each plus [`FRAME_CAP`] as the floor, capped at
/// [`WALK_FRAME_CEILING`]. The floor is what keeps a one-tile walk's budget the ten seconds
/// section 4 gives every other script, and it is also the slack for the arrival press and for the
/// gap between tiles; the per-tile term is what makes a walk across a town possible at all.
pub fn walk_budget(tiles: usize) -> u32 {
    let tiles = u32::try_from(tiles).unwrap_or(u32::MAX / FRAMES_PER_PLANNED_TILE);
    tiles
        .saturating_mul(FRAMES_PER_PLANNED_TILE)
        .saturating_add(FRAME_CAP)
        .min(WALK_FRAME_CEILING)
}

/// Whether the walk at the front of `active`'s plan ever got closer to a goal than it started, and
/// whether its arrival press ended where it began.
///
/// Read at the finish rather than carried on [`Active`], because the walk is the one step that
/// knows: [`Progress::Finished`] and [`Progress::Blocked`] leave it at the front of the plan, and
/// every macro that does not walk answers "got no closer, stalled at nothing", which is what makes
/// the [`MacroAbort::Timeout`] rule below read the same for a menu script as for a walk that never
/// got a tile.
fn walk_flags(active: &Active) -> (bool, bool) {
    match active.plan.front() {
        Some(Step::Walk(walk)) => (walk.best_distance < walk.start_distance, walk.stalled),
        _ => (false, false),
    }
}

/// Manhattan distance from `here` to the nearest of `goals`, which is [`path::route`]'s own
/// heuristic and therefore the same measure of "closer" the search itself uses.
fn nearest_goal(goals: &[Goal], here: Tile) -> u32 {
    goals.iter().map(|goal| goal.tile.distance(here)).min().unwrap_or(0)
}

/// The tile a walk is standing on, i.e. the goal its arrival belongs to.
///
/// Recorded when the arrival begins rather than searched for again, because a `Step` arrival is
/// over the moment the player is somewhere else and "somewhere else" has to be measured against
/// where it started.
fn goal_tile(walk: &Walk) -> Tile {
    walk.arrived.unwrap_or(Tile::new(0, 0))
}

/// One frame of a cursor navigation.
///
/// The cursor is read after every pulse and the next press is chosen from where it actually went,
/// which is what section 4 means by "never by counting presses": a list that is a column moves
/// under DOWN and UP, Red's two-by-two battle menu also needs RIGHT and LEFT, and this finds out
/// which by watching rather than by knowing. A list that is not accepting input yet is *waited*
/// for, never pressed at -- and since section 12.11 so is a list that is up but is **not the one
/// this step is walking**, because Red keeps one cursor for every menu in the game and reading the
/// wrong one is not reading.
fn cursor_frame(cursor: &mut Cursor, state: &mut dyn MacroState) -> Progress {
    // A confirming press that has begun finishes, and it is read before the list: the press is
    // what *answers* the list, so on the very frames it is being issued the cartridge is already
    // drawing the next one, and waiting for the list this step walked would wait for a list the
    // step has just left.
    if cursor.confirmed {
        return pulse(buttons::A, &mut cursor.phase, PRESS_HOLD, PRESS_GAP);
    }
    let list = listing(state).filter(|list| cursor.want.is_none_or(|want| want == list.kind));
    let Some(list) = list else {
        cursor.waited += 1;
        return if cursor.waited > CURSOR_WAIT {
            Progress::Blocked
        } else {
            Progress::Hold(buttons::NONE)
        };
    };
    let here = list.current;
    if here == cursor.target {
        if !cursor.confirm {
            return Progress::Next;
        }
        cursor.confirmed = true;
        cursor.phase = 0;
        return pulse(buttons::A, &mut cursor.phase, PRESS_HOLD, PRESS_GAP);
    }
    // The first frame the list this step is for is accepting input is where the navigation is
    // aimed from and budgeted by. Both are facts about *this* list, and a script that opened it
    // could not have known either when it was built.
    let aimed = aim_order(cursor.target, here);
    let order = *cursor.order.get_or_insert(aimed);
    let sized = list.max.saturating_add(1).saturating_mul(2).saturating_add(CURSOR_SLACK);
    let budget = *cursor.left.get_or_insert(sized);
    if budget == 0 || cursor.target > list.max {
        return Progress::Blocked;
    }
    if cursor.phase == 0 {
        cursor.before = here;
    }
    let facing = order[usize::from(cursor.at) % order.len()];
    match pulse(button(facing), &mut cursor.phase, PRESS_HOLD, PRESS_GAP) {
        Progress::Next => {
            let closer = here.abs_diff(cursor.target) < cursor.before.abs_diff(cursor.target);
            cursor.at = if closer { 0 } else { (cursor.at + 1) % 4 };
            cursor.left = Some(budget.saturating_sub(1));
            cursor.phase = 0;
            Progress::Hold(buttons::NONE)
        }
        other => other,
    }
}

/// The directions a cursor tries, in order, to get from `here` to `target`.
///
/// A column moves under DOWN and UP; Red's two-by-two battle menu needs RIGHT and LEFT as well,
/// and which of the four works is found by watching the cursor rather than by knowing the
/// geometry, so this only decides which to try *first*.
const fn aim_order(target: u8, here: u8) -> [Facing; 4] {
    if target > here {
        [Facing::Down, Facing::Right, Facing::Up, Facing::Left]
    } else {
        [Facing::Up, Facing::Left, Facing::Down, Facing::Right]
    }
}

/// Build the script for one macro, or `None` when there is nothing to walk to.
///
/// Every target is computed here, once, from the state the macro started in — which move, which
/// party slot, which bag entry, which exit — and the plan is fixed after that. Cursor *positions*
/// are still read every pulse; only the destination is decided up front, because the fly chose
/// this macro at this instant and not at a later one.
fn script(
    kind: MacroKind,
    state: &mut dyn MacroState,
    unreachable: &mut Vec<TargetKey>,
) -> Option<(VecDeque<Step>, Option<TargetKey>)> {
    let mut aimed: Option<TargetKey> = None;
    let plan: Vec<Step> = match kind {
        // Section 9.1's three ways out of a map, one script over the three of them: they differ
        // only in which warps they are pointed at, exactly as `GO NPC` and `GO ITEM` differ only
        // in which half of the object data they read.
        MacroKind::GoOut | MacroKind::GoWarp | MacroKind::GoRoute => {
            let way = match kind {
                MacroKind::GoOut => Way::Exit,
                MacroKind::GoWarp => Way::Passage,
                _ => Way::Route,
            };
            let goals = exit_goals(state, way);
            unreachable.extend(goal_keys(&goals));
            let (walk, target) = walk_to(state, goals)?;
            aimed = target;
            vec![Step::Walk(walk)]
        }
        // The next unreached rung's place, aimed at over the map and warp data of the map the fly
        // is standing on (section 9). It walks and turns and gives the buttons back; it never
        // presses A at what it arrives at, because that is `TALK`'s and the fly's.
        MacroKind::GoObjective => {
            // Three arrivals, because the ladder's rungs are three shapes: a person or an object
            // to stand beside and look at (`face`, which is `GO NPC`'s own arrival), a doormat to
            // step off (`press`), or a tile that fires by itself. [`aim_goals`] is that mapping,
            // shared with section 13's two errands.
            let goals = aim_goals(objective_goals(state));
            unreachable.extend(goal_keys(&goals));
            let (walk, target) = walk_to(state, goals)?;
            aimed = target;
            vec![Step::Walk(walk)]
        }
        // The nearest tile bordering ground this run has never stood on, facing the new ground
        // (section 9). The press that faces it is a step onto it when the tile is walkable, which
        // is how the frontier becomes ground the run has stood on rather than something the fly
        // stares at.
        MacroKind::GoFrontier => {
            let goals: Vec<Goal> = frontier_aims(state)
                .into_iter()
                .map(|(tile, facing)| Goal {
                    tile,
                    arrival: Arrival::Step(facing),
                    key: Some(TargetKey::Tile(tile)),
                })
                .collect();
            unreachable.extend(goal_keys(&goals));
            let (walk, target) = walk_to(state, goals)?;
            aimed = target;
            vec![Step::Walk(walk)]
        }
        MacroKind::GoNpc => {
            let people = untalked_people(state);
            let goals = approach(state, &people);
            unreachable.extend(goal_keys(&goals));
            let (walk, target) = walk_to(state, goals)?;
            aimed = target;
            vec![Step::Walk(walk)]
        }
        // The objects and the signs, the same walk-and-turn as `GO NPC` over the other half of
        // the map's object data. It stops facing the thing and gives the buttons back; pressing A
        // at it is `TALK`, and it is the fly's to choose (section 3, 2026-09-16).
        MacroKind::GoItem => {
            let targets = untalked_objects(state);
            let goals = approach(state, &targets);
            unreachable.extend(goal_keys(&goals));
            let (walk, target) = walk_to(state, goals)?;
            aimed = target;
            vec![Step::Walk(walk)]
        }
        MacroKind::Talk => vec![press(buttons::A)],
        MacroKind::Menu => {
            vec![Step::Repeat { mask: buttons::START, left: OPEN_PRESSES, phase: 0 }]
        }
        MacroKind::Next => vec![press(buttons::A)],
        // A is YES and B is NO in every yes/no box Red draws. Agent A's seam reports no cursor
        // for one, so there is none to read and nothing to navigate: these are the two presses
        // the table names and no more.
        MacroKind::Yes | MacroKind::Confirm => vec![press(buttons::A)],
        MacroKind::No | MacroKind::Back => vec![press(buttons::B)],
        MacroKind::Close | MacroKind::Leave => {
            vec![Step::Repeat { mask: buttons::B, left: BACKOUT_PRESSES, phase: 0 }]
        }
        // Section 14: one button per move slot. Which move is used is the fly's choice, so the
        // script's only job is to reach that slot -- through FIGHT from the menu above, or
        // straight to it over an open list.
        MacroKind::Move1 | MacroKind::Move2 | MacroKind::Move3 | MacroKind::Move4 => {
            let slot = move_index(kind)?;
            let mut steps = Vec::new();
            if let Some((cursor_at, count)) = move_list(state) {
                // The list is already open. Move to this slot and confirm -- unless the slot has no
                // PP, which for a bound button means *nothing* has and `MOVE 1` is carrying
                // Struggle: then confirm where the cursor stands, because moving it first would be
                // choosing a different spent move for no reason (row 30a). `count` is the guard
                // against a list whose cursor the seam could not place.
                let target = if slot < count && slot_has_pp(state, slot) {
                    slot
                } else {
                    cursor_at.filter(|at| *at < count)?
                };
                steps.push(cursor_on(target, true, Some(ListKind::BattleMoves)));
                return Some((steps.into(), aimed));
            }
            // From the menu above, FIGHT has to be chosen first, whether or not anything has PP:
            // row 34 of `infra/docs/macros-traps.md`, and the move list is the only way to
            // Struggle.
            if !in_main_menu(state) {
                return None;
            }
            steps.push(cursor_on(battle_entry::FIGHT, true, Some(ListKind::BattleMain)));
            steps.push(settle());
            // What happens after FIGHT is the cartridge's own answer and it is measured rather
            // than assumed (row 34): with a move that has PP the list opens and this slot is
            // chosen; with none, `CheckPlayerHasUsableMoves` prints "has no moves left!" and sets
            // Struggle **without opening the list**, so confirming FIGHT is the whole macro and a
            // second cursor step would press A at text. `MOVE 1` is the button that reaches that
            // state, because it is the only one bound there.
            if slot_has_pp(state, slot) {
                steps.push(cursor_on(slot, true, Some(ListKind::BattleMoves)));
            }
            steps
        }
        // Section 14's addition: the ball comes out of the bag, so the script is `ITEM`'s with a
        // ball's index instead of a potion's -- and it stops at the confirmation. The throw
        // animation, the shake count, the "Gotcha!" and the nickname prompt are all text the fly
        // answers with the between-turns `NEXT` and the dialog's `NO` (section 12).
        MacroKind::ThrowBall => {
            let bag_slot = throw_slot(state)?;
            let mut steps = Vec::new();
            if in_main_menu(state) {
                steps.push(cursor_on(battle_entry::ITEM, true, Some(ListKind::BattleMain)));
                steps.push(settle());
            }
            // The bag, and it has to *be* the bag: this is the step that reported `blocked` 63
            // times out of 63 on the cartridge while it was allowed to read the battle menu's
            // cursor instead (section 12.11).
            steps.push(cursor_on(bag_slot, true, Some(ListKind::BattleBag)));
            steps
        }
        MacroKind::Switch => {
            let slot = healthiest_other(state)?;
            let mut steps = Vec::new();
            // A forced switch is already looking at the party list; a chosen switch has to get
            // there through the battle menu's PKMN entry first.
            if in_main_menu(state) {
                steps.push(cursor_on(battle_entry::PKMN, true, Some(ListKind::BattleMain)));
                steps.push(settle());
            }
            steps.push(cursor_on(slot, true, Some(ListKind::BattleParty)));
            steps.push(settle());
            // The party entry's action list opens on SWITCH.
            steps.push(press(buttons::A));
            steps
        }
        MacroKind::Item => {
            let bag_slot = potion_slot(state)?;
            let active = state.battle().and_then(|battle| battle.own).map(|mon| mon.slot)?;
            let mut steps = Vec::new();
            if in_main_menu(state) {
                steps.push(cursor_on(battle_entry::ITEM, true, Some(ListKind::BattleMain)));
                steps.push(settle());
            }
            steps.push(cursor_on(bag_slot, true, Some(ListKind::BattleBag)));
            steps.push(settle());
            // Which Pokémon to heal: the one that is out, because that is the HP the precondition
            // measured.
            steps.push(cursor_on(active, true, Some(ListKind::BattleParty)));
            steps
        }
        MacroKind::Run => {
            vec![cursor_on(battle_entry::RUN, true, Some(ListKind::BattleMain))]
        }
        MacroKind::BuyPotion => shop_plan(state, item::POTION)?,
        MacroKind::BuyBall => shop_plan(state, item::POKE_BALL)?,
        MacroKind::BuyAntidote => shop_plan(state, item::ANTIDOTE)?,
        MacroKind::BuyRepel => shop_plan(state, item::REPEL)?,
        // Section 13's two errands: the same walk `GO OBJECTIVE` makes, aimed at the area's mart
        // or centre instead of at the rung's place -- to the door and in from outside, to the
        // counter person from inside ([`super::palette::amenity_goals`]).
        MacroKind::GoShop | MacroKind::GoHeal => {
            let kind = if kind == MacroKind::GoShop { Amenity::Mart } else { Amenity::Center };
            let goals = aim_goals(amenity_goals(state, kind));
            unreachable.extend(goal_keys(&goals));
            let (walk, target) = walk_to(state, goals)?;
            aimed = target;
            vec![Step::Walk(walk)]
        }
        // Section 13's `HEAL`, which is the one macro that spans two scene classes ([`spanned`]):
        // walk to the counter, face the nurse, talk, answer YES, wait for the party to read back
        // full, close the box. Every press but the walk is a press at a text box, and the box only
        // exists because this macro's own A opened it -- which is why the conversation belongs to
        // the macro rather than to the fly's next hold. Nothing here decides *to* heal: the fly
        // chose the button, and the button is only on the pad while the party needs it.
        MacroKind::Heal => {
            let goals = aim_goals(heal_goals(state));
            unreachable.extend(goal_keys(&goals));
            let (walk, target) = walk_then(state, goals, true)?;
            aimed = target;
            vec![
                Step::Walk(walk),
                // The A that talks to the nurse.
                press(buttons::A),
                settle(),
                // "Shall we heal your Pokémon?" opens on YES, so A is the answer.
                press(buttons::A),
                Step::Rested { waited: 0 },
                // And the two boxes after it: "fighting fit", "we hope to see you again". Two
                // bounded presses rather than a `Repeat`, because a `Repeat` ends only on a scene
                // change and this macro *spans* the scene change -- it would run out and report
                // `Blocked` on a heal that worked. A third box, if the cartridge ever draws one,
                // is `NEXT`'s on the next hold, which is on the dialog's pad.
                press(buttons::A),
                settle(),
                press(buttons::A),
            ]
        }
    };
    Some((plan.into(), aimed))
}

/// Whether the Pokémon that is out has PP left in move slot `slot`.
///
/// What tells "choose this move" from "confirm whatever the cursor is on so that Struggle
/// happens": a bound `MOVE n` whose own slot is spent can only be `MOVE 1` with nothing anywhere
/// ([`super::palette::move_slot_bound`]).
fn slot_has_pp(state: &mut dyn MacroState, slot: u8) -> bool {
    state
        .battle()
        .and_then(|battle| battle.own)
        .and_then(|own| own.moves.get(usize::from(slot)).copied().flatten())
        .is_some_and(|entry| entry.pp > 0)
}

/// [`super::palette::Aim`]s as the walker's own goals.
///
/// The same three arrivals `GO OBJECTIVE` derives from an aim -- stand beside it and turn, step
/// off a doormat, or settle on a tile that fires by itself -- so an errand's walk and the
/// objective's walk cannot disagree about what arriving means.
fn aim_goals(aims: Vec<super::palette::Aim>) -> Vec<Goal> {
    aims.into_iter()
        .map(|aim| Goal {
            tile: aim.tile,
            arrival: match (aim.face, aim.press) {
                (Some(facing), _) => Arrival::Face(facing),
                (None, Some(facing)) => Arrival::Leave(facing),
                (None, None) => Arrival::Settle,
            },
            key: aim.key,
        })
        .collect()
}

fn press(mask: u8) -> Step {
    Step::Press { mask, phase: 0 }
}

fn settle() -> Step {
    Step::Settle { phase: 0 }
}

/// A cursor step aimed at `target` in whichever list is accepting input when it runs.
///
/// For a step that does not cross from one list into another: the shop's own screens, and the
/// first step of any script, which reads the list its macro was dealt on.
fn cursor(target: u8, confirm: bool) -> Step {
    cursor_on(target, confirm, None)
}

/// A cursor step aimed at `target` in `want`, waiting for that list rather than pressing at
/// whichever one happens to be up.
///
/// The press order and the budget are taken from the list on the first frame it accepts input
/// ([`cursor_frame`]) and not from here, because a script that has just pressed A to open a list
/// is still reading the list it pressed A *in*: twenty settle frames is not always enough for the
/// cartridge to draw the next one, and the one it has is the wrong length and points the wrong
/// way (section 12.11).
fn cursor_on(target: u8, confirm: bool, want: Option<ListKind>) -> Step {
    Step::Cursor(Cursor {
        target,
        confirm,
        want,
        order: None,
        at: 0,
        before: target,
        left: None,
        phase: 0,
        waited: 0,
        confirmed: false,
    })
}

/// Whether the top-level FIGHT/PKMN/ITEM/RUN menu is the list that is up.
fn in_main_menu(state: &mut dyn MacroState) -> bool {
    matches!(
        state.battle().map(|battle| battle.menu),
        Some(super::state::BattleMenu::Main { .. })
    )
}

/// A walk to `goals`, refusing at `start` rather than blocking at frame one if there is nothing to
/// walk to.
fn walk_to(state: &mut dyn MacroState, goals: Vec<Goal>) -> Option<(Walk, Option<TargetKey>)> {
    walk_then(state, goals, false)
}

/// [`walk_to`], with `continues` set when presses follow the walk in the same script.
fn walk_then(
    state: &mut dyn MacroState,
    goals: Vec<Goal>,
    continues: bool,
) -> Option<(Walk, Option<TargetKey>)> {
    let tiles: Vec<Tile> = goals.iter().map(|goal| goal.tile).collect();
    let route = path::route(state, &tiles)?;
    // A goal the search could not reach is excluded, not walked toward
    // (`docs/design/macros.md` section 12.1, amended 2026-09-17). The search is over the whole map
    // and it prices an off-screen tile as passable ([`path::UNKNOWN_STEP`]), so the only thing that
    // can make a goal unreachable is a wall inside the walkable window: `Route::goal` of `None`
    // means "walled off from where the fly stands", not "too far to see". Walking the
    // closest-approach route instead spent a whole budget arriving nowhere and left the candidate
    // list untouched, so the same walk was available on the next hold for ever — 116 `GO ITEM`
    // starts and 117 timeouts in one night. Refusing here reports every goal key to `start`, which
    // writes them to the blocked ledger and takes the button off the pad when the list empties.
    route.goal?;
    let player = state.player()?;
    let map = player.map;
    let distance = nearest_goal(&goals, Tile::new(player.x, player.y));
    // Which target this walk set out for, in three readings, most certain first:
    //
    // 1. the goal the route actually reaches;
    // 2. the key every goal shares, which is what the four tiles around one object are;
    // 3. for a route that only gets *closer* to a goal it cannot reach, the nearest goal -- which is
    //    what the search's own heuristic drove it toward.
    //
    // The third is not a nicety. `GO FRONTIER`'s goals are one key each (a tile), so reading 2
    // answers `None` for it, and a closest-approach walk therefore carried no target at all: it
    // spent the whole frame cap wandering toward a frontier it could not reach, recorded nothing,
    // and was chosen again on the next hold for ever. Ninety-nine timeouts in twenty brain minutes,
    // twelve per two-minute window over five tiles (`infra/docs/macros-traps.md`).
    let nearest = goals
        .iter()
        .filter(|goal| goal.key.is_some())
        .min_by_key(|goal| goal.tile.distance(Tile::new(player.x, player.y)))
        .and_then(|goal| goal.key);
    let target = route
        .goal
        .and_then(|index| goals.get(index))
        .and_then(|goal| goal.key)
        .or_else(|| one_target(&goals))
        .or(nearest);
    let here = Tile::new(player.x, player.y);
    let budget = walk_budget(route.steps.len());
    Some((
        Walk {
            goals,
            map,
            plan: route.steps.into(),
            expect: Some(here),
            refused: Vec::new(),
            budget,
            idle: 0,
            holding: None,
            held: 0,
            gap: 0,
            failures: 0,
            arrival: None,
            arrived: None,
            start_distance: distance,
            best_distance: distance,
            continues,
            stalled: false,
        },
        target,
    ))
}

/// Every distinct target the goals of one walk carry, in order.
///
/// What a `no route` refusal has failed at: not one target but all of them, because the search was
/// multi-goal and got to none of them. Deduplicated, so the four tiles around one object are one
/// key rather than four.
fn goal_keys(goals: &[Goal]) -> Vec<TargetKey> {
    let mut out: Vec<TargetKey> = Vec::new();
    for key in goals.iter().filter_map(|goal| goal.key) {
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// The one target every goal of a walk shares, or `None` when they disagree or carry none.
fn one_target(goals: &[Goal]) -> Option<TargetKey> {
    let mut keys = goals.iter().map(|goal| goal.key);
    let first = keys.next().flatten()?;
    keys.all(|key| key == Some(first)).then_some(first)
}

/// The goals for one of section 9.1's three ways out of the current map.
///
/// Shared by `GO OUT`, `GO WARP` and `GO ROUTE`. An exit the player is *standing on* that fires
/// by itself has plainly not fired, so it is not an exit to walk to — which is the state the fly
/// is in the moment it comes down a staircase and lands on the warp tile. A doormat underfoot is
/// different: its press is the point, so it stays.
fn exit_goals(state: &mut dyn MacroState, way: Way) -> Vec<Goal> {
    let mut goals: Vec<Goal> = ways(state, way)
        .into_iter()
        .map(|exit| Goal {
            tile: exit.tile,
            arrival: match exit.press {
                Some(facing) => Arrival::Leave(facing),
                None => Arrival::Settle,
            },
            key: Some(TargetKey::Exit(exit.id)),
        })
        .collect();
    if let Some(here) = state.player().map(|player| Tile::new(player.x, player.y))
        && goals.iter().any(|goal| goal.tile != here)
    {
        goals.retain(|goal| goal.tile != here || goal.arrival != Arrival::Settle);
    }
    goals
}

/// The four tiles next to the nearest of `targets`, each with the press that turns to face it.
///
/// Shared by `GO NPC` and `GO ITEM`, which differ only in which half of the map's object data
/// they are pointed at. The tile to stand on is one step away from the thing and the press faces
/// back at it: walking into an occupied tile turns the player without moving them, which is how
/// either macro ends up looking at what it walked to, with the same button the fly's raw presses
/// use.
///
/// "Nearest" is Manhattan distance from the player, ties broken by tile, so the choice is the
/// same one [`Tile::distance`] makes for a way out. Whether any of the four is *reachable* is the
/// route search's answer, not this one's.
fn approach(state: &mut dyn MacroState, targets: &[(Tile, TalkTarget)]) -> Vec<Goal> {
    let Some(player) = state.player() else { return Vec::new() };
    let here = Tile::new(player.x, player.y);
    // "Nearest untalked" (`docs/design/macros.md` section 12): the caller has already dropped
    // every target the talked ledger holds ([`super::palette::untalked_people`] and
    // [`untalked_objects`]), so this ranks what is left and nothing here decides what counts as
    // talked to. There is no fallback to the talked ones: a map whose things have all been talked
    // to leaves the button off the pad instead of walking the fly back to the nearest of them,
    // which is the loop this replaced -- 549 walks to one shelf on the cartridge, and a villager
    // in the open that "stood next to" could never close over.
    let mut ranked: Vec<(u32, Tile, TalkTarget)> = targets
        .iter()
        .map(|(tile, target)| (tile.distance(here), *tile, *target))
        .collect();
    ranked.sort_unstable();
    let Some((_, tile, target)) = ranked.first().copied() else { return Vec::new() };
    let mut goals = Vec::new();
    for facing in FACINGS {
        let Some(stand) = tile.step(facing) else { continue };
        // **Not a tile the cartridge pushes the fly off** (row 37 of
        // `infra/docs/macros-traps.md`). Viridian City's (19, 9) is one of the four tiles around
        // the sleeping old man at (18, 9), and standing on it without the Pokédex prints "This is
        // private property!" and walks the fly down, every frame: 53,266 of 54,377 text-box frames
        // in a twenty-brain-minute run were that one tile. The other three sides of him are still
        // offered, so the villager is not written off for the ground beside him.
        if state.pushed_tile(stand.x, stand.y) {
            continue;
        }
        goals.push(Goal {
            tile: stand,
            arrival: Arrival::Face(opposite(facing)),
            // All four tiles around the thing are the same target: whichever side the walk comes
            // from, what it reached -- or failed to reach -- is the thing itself.
            key: Some(TargetKey::Thing(target)),
        });
    }
    goals
}


/// A mart purchase: BUY, the item, quantity one, and the price confirmation.
///
/// One script over all four of section 13's purchases, which differ only in which item id they are
/// pointed at -- exactly as `GO NPC` and `GO ITEM` differ only in which half of the object data
/// they read. The item's position in the stock list is its cursor index in the buy list *while the
/// list is not scrolled* (`docs/design/macros-wram.md`, `wItemList`), so the navigation is a
/// cursor read and never a count of presses; an item the counter does not stock, or one past the
/// rows the cursor can reach, has no index and the script refuses before anything is pressed
/// ([`super::palette::stock_index`], row 55).
///
/// Quantity one, always: the prompt opens on one and this macro presses A at it. "Buy ONE unit"
/// is section 13's own word, and a quantity the fly did not choose is not one this crate types.
///
/// Which screen the mart is on is read from agent A's `ShopScreen` rather than assumed, so the
/// script works both from a freshly opened counter and from the buy list.
fn shop_plan(state: &mut dyn MacroState, want: u8) -> Option<Vec<Step>> {
    let index = stock_index(state, want)?;
    let mut steps = Vec::new();
    if shop_screen(state)? == ShopScreen::BuySellQuit {
        // BUY is the counter menu's first entry.
        steps.push(cursor(0, true));
        steps.push(settle());
    }
    steps.push(cursor(index, true));
    steps.push(settle());
    // The quantity prompt opens on one, and the price confirmation opens on YES.
    steps.push(press(buttons::A));
    steps.push(settle());
    steps.push(press(buttons::A));
    Some(steps)
}

/// What the fly is facing right now, as the talked ledger names it, with the map it is on.
///
/// `None` when the tile ahead is off the map or has nothing on it, which is also when `TALK`'s
/// precondition refuses, so a started `TALK` normally has one.
///
/// **`palette::facing_target`, which is `TALK`'s own precondition, and not a second reading of
/// it.** This looked one tile ahead, and a mart clerk and a Pokemon Center nurse stand two tiles
/// away behind a counter -- so `TALK` was *bound* at a counter by the reach
/// `IsSpriteOrSignInFrontOfPlayer` really has and *recorded* by a reach one tile shorter, which is
/// no entry at all: the ledger never learned that the counter had been talked to, and the pad
/// offered the same conversation once per hold for ever. Measured at the rung-10 Pokemon Center
/// (`infra/docs/macros-traps.md` row 41): `TALK` 107 starts on one tile, none of them retiring the
/// nurse. A precondition and the ledger that answers it have to be the same question.
fn talk_target(state: &mut dyn MacroState) -> Option<(u8, TalkTarget)> {
    let target = facing_target(state)?;
    let player = state.player()?;
    Some((player.map, target))
}
