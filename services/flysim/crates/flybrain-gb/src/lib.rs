//! Game Boy half of `flysim`: the binjgb emulator core linked natively, plus
//! the game adapters that turn WRAM into reward and progress.
//!
//! ```text
//! ROM bytes -> Emulator::run_frame -> RGBA frame + PCM + WRAM
//!                                       |
//!                            GameAdapter::sample -> RewardEvent
//!                                       |
//!                            Ratchet::observe -> recover_game
//! ```
//!
//! The crate deliberately knows nothing about neurons. [`recovery`] takes the
//! neural side as closures so `flysim` can wire `flybrain-core` without this
//! crate depending on it.
//!
//! What is vendored, how to regenerate the symbol table, the exact audio format
//! and the save-state caveat are all in the crate README.

pub mod adapter;
pub mod compatibility;
pub mod emulator;
mod ffi;
pub mod macros;
mod ordered;
pub mod platformer;
pub mod pokemon_red;
pub mod ratchet;
pub mod recovery;

pub use adapter::{
    AdapterError, DecoderPresetId, GameAdapter, MemoryReader, ProgressSnapshot, RewardEvent,
    adapter_for, adapter_for_with_rom_pin,
};
pub use compatibility::Compatibility;
pub use emulator::{
    DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, FRAMEBUFFER_LEN, GbError,
    SCREEN_HEIGHT, SCREEN_WIDTH, buttons,
};
pub use macros::{
    AdapterLedger, MacroPalette, NoLedger, Observed, Outcome, PaletteMode, RunLedger, SceneId,
    SlotBinding, Started, macro_channels, palette_for,
};
pub use platformer::PlatformerAdapter;
pub use pokemon_red::PokemonRedReward;
pub use ratchet::{Ratchet, RatchetState, RecoveryPolicy, Snapshot};
pub use recovery::{ClosureRecovery, NeuralRecovery, recover_game};
