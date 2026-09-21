//! Which scene the game is in, from WRAM alone.
//!
//! `docs/design/macros.md` section 2 declares the enum and the entry point; this is the
//! implementation, and `docs/design/macros-wram.md` is the evidence: every byte it reads, its
//! address at the pinned pokered commit, and how the reading was verified.
//!
//! ## The rule that shapes it
//!
//! A wrong scene deals the fly a palette of actions that do not apply, so two misreadings are
//! specifically forbidden (`macros.md`, agent A's brief): a battle must never read as overworld,
//! and a dialog must never read as overworld. [`detect`] enforces that by *order*: the battle test
//! comes before everything, the overworld test comes last, and it is the only branch with a
//! positive requirement on every gate. Anything left over is [`Scene::Unknown`], which the
//! doctrine treats like [`Scene::Dialog`] — advance only, nothing that assumes a map or a menu.
//!
//! The corollary is that `Unknown` is not a bug to be driven to zero. Pokémon Red has no "which
//! screen am I on" byte; it has a font flag, a text box id, a menu cursor and a screen buffer, and
//! there are states (the Pokédex, the trainer card, the naming screens, a mid-warp frame) that
//! none of those pin down. Reporting those as `Unknown` is the design working.

pub use super::macros::state::Scene;

use crate::adapter::MemoryReader;

use super::state;
use super::symbols::ram;

/// Which scene the game is in. Sampled once per game frame, after the frame.
///
/// The order of the tests is the contract:
///
/// 1. **Not started** — `wStatusFlags6`'s game-timer bit is clear, so the cartridge is on the
///    title, in the intro or on a naming screen: [`Scene::Title`].
/// 2. **Not readable** — the map header or the party count is outside its range, which is what a
///    frame in the middle of a load or a warp looks like: [`Scene::Unknown`].
/// 3. **Battle** — `wIsInBattle` says a wild or trainer battle is running:
///    [`Scene::Battle`]. Checked before any text or menu test, because a battle is full of both.
///    The Safari Zone and the old man's tutorial (`wBattleType`) have different menus and read as
///    `Unknown`.
/// 4. **PC**, then **mart**, both of which are menus with their own flag.
/// 5. **A text display is open** — the dialogue box at the bottom of the screen is
///    [`Scene::Dialog`]; the start menu's box or a recognised submenu is [`Scene::Menu`]; anything
///    else is [`Scene::Unknown`].
/// 6. **Overworld** — and only here: a loaded map, in-range coordinates, no text display, and the
///    buttons reaching the player.
pub fn detect(memory: &mut dyn MemoryReader) -> Scene {
    if !state::started(memory) {
        return Scene::Title;
    }
    if state::map_size(memory).is_none() || memory.read8(ram::wPartyCount) > 6 {
        return Scene::Unknown;
    }

    // Battle first, and from `wIsInBattle` rather than from anything on screen: a battle is made
    // of text boxes and menus, and every one of them must still read as a battle.
    match memory.read8(ram::wIsInBattle) {
        0 => {}
        _ => {
            return match state::battle(memory) {
                Some(battle) => Scene::Battle {
                    own_turn: battle.own_turn,
                    forced_switch: battle.forced_switch,
                },
                // A battle byte with no battle behind it: the `$ff` frame a lost battle passes
                // through, a Safari or tutorial battle, or a garbage read.
                None => Scene::Unknown,
            };
        }
    }

    if state::pc(memory).is_some() {
        return Scene::Pc;
    }
    if state::shop(memory).is_some() {
        return Scene::Shop;
    }

    let text = state::text_box(memory);
    if text.open {
        if text.waiting {
            return Scene::Dialog;
        }
        if state::start_menu(memory).is_some() || state::submenu(memory) {
            return Scene::Menu;
        }
        return Scene::Unknown;
    }

    if state::player(memory).is_some() && state::controllable(memory) {
        return Scene::Overworld;
    }
    Scene::Unknown
}

/// Bits of the scene that are worth logging when it is [`Scene::Unknown`] — or when it is
/// [`Scene::Dialog`] and should not be — for the survey harness and for a stuck stream. Reads
/// nothing the detector does not.
///
/// `corners` are the four screen tiles the dialogue box's `waiting` test reads
/// ([`state::dialog_corners`]). They are in here because they are the only input of the dialog
/// branch that is not a WRAM flag, and a stuck `dialog` reading is told from a real text box by
/// exactly those four bytes against `font`.
pub fn why_unknown(memory: &mut dyn MemoryReader) -> String {
    let box_corners = state::dialog_corners(memory);
    format!(
        "started={} map={:?} party={} battle={} type={} font={:#04x} textbox={:#04x} \
         list={:#04x} cursor=({},{},{},{},{:#04x}) joy={} sim={} flags5={:#04x} \
         flags6={:#04x} move={:#04x} \
         corners=({:#04x},{:#04x},{:#04x},{:#04x})",
        state::started(memory),
        state::map_size(memory).map(|size| (size.width, size.height)),
        memory.read8(ram::wPartyCount),
        memory.read8(ram::wIsInBattle),
        memory.read8(ram::wBattleType),
        memory.read8(ram::wFontLoaded),
        memory.read8(ram::wTextBoxID),
        memory.read8(ram::wListMenuID),
        memory.read8(ram::wTopMenuItemY),
        memory.read8(ram::wTopMenuItemX),
        memory.read8(ram::wCurrentMenuItem),
        memory.read8(ram::wMaxMenuItem),
        memory.read8(ram::wMenuWatchedKeys),
        memory.read8(ram::wJoyIgnore),
        memory.read8(ram::wSimulatedJoypadStatesIndex),
        memory.read8(ram::wStatusFlags5),
        memory.read8(ram::wStatusFlags6),
        memory.read8(ram::wMovementFlags),
        box_corners[0],
        box_corners[1],
        box_corners[2],
        box_corners[3],
    )
}

#[cfg(test)]
mod tests;
