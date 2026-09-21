//! The macro palette: scene-appropriate actions, and the executor that runs them.
//!
//! `docs/design/macros.md` is the binding contract. This module is agent B's
//! (`Palette::for_scene`, A* pathing, the macro scripts, `MacroExecutor`); [`state`] is the seam
//! it is written against and belongs to agent A, who implements it in
//! [`crate::pokemon_red::state`].
//!
//! In one paragraph: the population decoder and its six channels do not change, and palette mode
//! gives those same channels a *scene-dependent meaning* in the game layer. UP is slot 0, DOWN
//! slot 1, LEFT slot 2, RIGHT slot 3, A slot 4, B slot 5; [`Palette::for_scene`] says what each
//! slot means in the current scene and leaves a slot unbound when its precondition fails;
//! [`MacroMachine`] runs the chosen slot's script with the same button register the fly's raw
//! presses use, for at most six hundred frames, giving the buttons back the moment the scene
//! changes.
//!
//! What is *not* here is as much the point as what is:
//!
//! - **No choosing.** Nothing in this module picks a macro. `start` is handed a slot, and the
//!   slot comes from the readout. There is no default, no fallback on timeout and no objective
//!   (`docs/design/macros.md` section 1).
//! - **No addresses.** Every read goes through [`state::GameState`] and [`cartridge::MacroState`].
//!   The executor cannot drift from the decomp because it never names an offset.
//! - **No new button path.** A macro presses the emulator's button register, the same one the
//!   decoder's raw masks go to. There is still no button endpoint on the control API.
//!
//! ## Wiring it (agent C)
//!
//! [`cartridge::MacroState`] is one extension of agent A's trait for the four things section 3
//! names and the seam does not carry, and every method is defaulted, so agent A's reader needs
//! one line:
//!
//! ```ignore
//! impl MacroState for PokeState<'_> {}
//! ```
//!
//! That compiles today, and it is checked: a trial merge of `feat/macros-executor` and
//! `feat/macros-scene` builds and passes both halves' tests, conflicting only in the two module
//! lists. Overriding a default is what turns `BUY POTION` on, gives `ATTACK` the real move table
//! and type chart, and points the three ways out at the adapter's exploration ledger; until then the
//! palette offers less rather than guessing.
//!
//! ```text
//! decoder channels --> MacroId --> Palette::for_scene --> MacroMachine::start
//!                                                             |
//!                          per frame: MacroMachine::step --> button mask --> emulator
//! ```

pub mod cartridge;
pub mod driver;
pub mod executor;
pub mod geography;
pub mod palette;
pub mod path;
pub mod plan;
pub mod state;

#[cfg(test)]
mod tests;

pub use cartridge::{Edge, ExitId, Listing, MacroState, Tile};
pub use driver::PokemonPalette;
pub use executor::{
    Executor, FRAME_CAP, MAX_FAILED_STEPS, MacroAbort, MacroExecutor, MacroMachine, MacroRefused,
    Refusal, StateSource,
};
pub use palette::{MacroId, MacroKind, MacroSpec, Palette, SLOTS};
pub use path::{Exit, Route, exits, route};
