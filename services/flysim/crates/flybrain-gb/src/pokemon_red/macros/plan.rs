//! Plan mode, which since section 14 is the palette.
//!
//! This file was section 9's *ordering* of a scene's macros into a plan, and section 12 took the
//! meaning out of the order: "nothing ranks a macro any more, and the screen sorts the cells by
//! type". What it kept it for was the *set* -- it was the dealer that knew `GO OBJECTIVE`'s rung,
//! `GO WARP` indoors and `TALK` in front of an untalked thing, which section 3's fixed
//! six-channel table never did, so `MacroMode::Macros` asked for this one.
//!
//! Section 14 removes the last reason the two differed. With a slot per macro type
//! ([`super::palette::SLOTS`]) nothing is ordered and nothing is truncated, so the order this file
//! computed had no observable left; [`super::palette::scene_set`] is the one table, and
//! [`plan_for`] is [`Palette::for_scene`]. Two dealers that could disagree about which buttons a
//! scene has are now one, which is worth more than the function name.
//!
//! What stays here is the handful of *questions* the palette and the log line share and that have
//! no other home: whether the fly is standing on the objective's map, whether a passage leads to
//! it, and section 9's two battle measures.

use super::cartridge::MacroState;
use super::palette::{Palette, objective_place, ways};
use super::path::Way;
use super::state::Scene;

/// HP fraction below which section 9 calls the active Pokémon worth switching out: a quarter.
///
/// Integer arithmetic, so no rounding decides it: `hp * 4 < max_hp`.
const SWITCH_NUMERATOR: u32 = 4;


/// The plan for `scene`, as a palette whose slots are ranks.
///
/// Deterministic: the same game state gives the same order, every time, with no clock and no
/// randomness anywhere in it. That is what makes plan mode measurable against the other two arms
/// (`docs/design/macros.md` section 7) and what makes the screen readable — the rows move when
/// the *game* changes, not when the policy feels differently about it.
pub fn plan_for(scene: Scene, state: &mut dyn MacroState) -> Palette {
    Palette::for_scene(scene, state)
}

/// Whether the fly is standing on the map the ladder's next rung -- or this area's errand -- is on.
pub fn on_objective_map(state: &mut dyn MacroState) -> bool {
    let Some(objective) = objective_place(state) else { return false };
    state.player().is_some_and(|player| player.map == objective.map)
}

/// Whether a passage on this map leads to the objective's own map.
pub fn passage_to_objective(state: &mut dyn MacroState) -> bool {
    let Some(objective) = objective_place(state) else { return false };
    ways(state, Way::Passage).iter().any(|exit| exit.into == Some(objective.map))
}

/// Whether the Pokémon that is out is under a quarter of its HP.
///
/// Section 9's own measure, kept for the log line and the tests. Nothing binds a button on it any
/// more: `SWITCH`'s precondition is "a healthier reserve exists" and `RUN`'s is section 13.1's
/// [`super::palette::losing`], which reads a third rather than a quarter and which is the one
/// place either question is asked.
pub fn failing(state: &mut dyn MacroState) -> bool {
    let Some(battle) = state.battle() else { return false };
    let Some(mon) = battle.own.or_else(|| state.party().active_mon().copied()) else {
        return false;
    };
    mon.max_hp > 0 && u32::from(mon.hp) * SWITCH_NUMERATOR < u32::from(mon.max_hp)
}

/// Whether every Pokémon that can still fight is under a quarter of its HP.
///
/// Section 9's `RUN` condition, kept for the log line and the tests; section 13.1's
/// [`super::palette::losing`] is what the button is bound on.
pub fn party_weak(state: &mut dyn MacroState) -> bool {
    let party = state.party();
    !party.mons.iter().any(|mon| {
        !mon.fainted() && u32::from(mon.hp) * SWITCH_NUMERATOR >= u32::from(mon.max_hp)
    })
}
