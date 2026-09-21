//! Rolling the game back to the ratchet's best safe snapshot.
//!
//! A port of the prototype's `src/runtime/game-recovery.ts`. That function took
//! the brain, the decoder, the emulator and the reward adapter; this crate must
//! not depend on `flybrain-core`, so the neural half is a trait with a
//! closure-backed implementation the `flysim` binary supplies.
//!
//! What recovery does and does not touch (`docs/ratchet-sprint.md`): it imports
//! only emulator bytes, releases every button, clears decoder holds and plastic
//! eligibility, clears transient reward observations, and refreshes visual drive
//! from a *copy* of the archived framebuffer. The neural RNG, membrane state,
//! learned gains, brain clock and lifetime reward history all continue.

use crate::adapter::GameAdapter;
use crate::emulator::{Emulator, FRAMEBUFFER_LEN, GbError, buttons};
use crate::ratchet::Snapshot;

/// The neural side of a recovery, as `flysim` wires it to `flybrain-core`.
pub trait NeuralRecovery {
    /// `decoder.clearHolds(brain.ms)`: drop held buttons and winner habituation.
    fn clear_decoder_holds(&mut self);
    /// `brain.plasticity.clearEligibility(brain.ms)`: drop eligibility traces,
    /// keeping learned gains.
    fn clear_eligibility(&mut self);
    /// `brain.setVisualFrame(frame)`: re-drive the retina from the archived
    /// framebuffer so the first post-recovery step does not see the frame the
    /// run stalled on.
    fn set_visual_frame(&mut self, frame: &[u8]);
}

/// Build a [`NeuralRecovery`] out of three closures, so the caller needs no new
/// type and this crate needs no neural dependency.
pub struct ClosureRecovery<H, E, V> {
    pub clear_holds: H,
    pub clear_eligibility: E,
    pub set_visual_frame: V,
}

impl<H, E, V> NeuralRecovery for ClosureRecovery<H, E, V>
where
    H: FnMut(),
    E: FnMut(),
    V: FnMut(&[u8]),
{
    fn clear_decoder_holds(&mut self) {
        (self.clear_holds)();
    }

    fn clear_eligibility(&mut self) {
        (self.clear_eligibility)();
    }

    fn set_visual_frame(&mut self, frame: &[u8]) {
        (self.set_visual_frame)(frame);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryError {
    /// The snapshot's emulator state did not load.
    Emulator(GbError),
    /// The snapshot's framebuffer is not 160x144 RGBA.
    FrameSize { expected: usize, actual: usize },
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Emulator(error) => write!(f, "{error}"),
            Self::FrameSize { expected, actual } => {
                write!(f, "archived framebuffer is {actual} bytes, expected {expected}")
            }
        }
    }
}

impl std::error::Error for RecoveryError {}

/// Restore `snapshot` and return the framebuffer the caller should treat as the
/// current frame. The returned buffer is an owned copy, as the prototype's
/// `framebuffer.slice()` was: the archived bytes must not be aliased by the
/// live retina input.
pub fn recover_game(
    emulator: &mut Emulator,
    adapter: &mut dyn GameAdapter,
    neural: &mut dyn NeuralRecovery,
    snapshot: &Snapshot,
) -> Result<Vec<u8>, RecoveryError> {
    if snapshot.frame.len() != FRAMEBUFFER_LEN {
        return Err(RecoveryError::FrameSize {
            expected: FRAMEBUFFER_LEN,
            actual: snapshot.frame.len(),
        });
    }
    emulator.import_state(&snapshot.game).map_err(RecoveryError::Emulator)?;
    emulator.set_buttons(buttons::NONE);
    neural.clear_decoder_holds();
    neural.clear_eligibility();
    adapter.clear_transient();
    let frame = snapshot.frame.clone();
    neural.set_visual_frame(&frame);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pokemon_red::PokemonRedReward;
    use std::cell::RefCell;

    #[test]
    fn the_closure_adapter_forwards_each_hook_once() {
        let calls = RefCell::new(Vec::<&'static str>::new());
        let mut neural = ClosureRecovery {
            clear_holds: || calls.borrow_mut().push("holds"),
            clear_eligibility: || calls.borrow_mut().push("eligibility"),
            set_visual_frame: |frame: &[u8]| {
                assert_eq!(frame.len(), FRAMEBUFFER_LEN);
                calls.borrow_mut().push("frame");
            },
        };
        neural.clear_decoder_holds();
        neural.clear_eligibility();
        neural.set_visual_frame(&vec![0; FRAMEBUFFER_LEN]);
        assert_eq!(*calls.borrow(), ["holds", "eligibility", "frame"]);
    }

    #[test]
    fn clearing_transients_blocks_replaying_an_already_paid_battle() {
        let mut adapter = PokemonRedReward::new();
        adapter.clear_transient();
        assert!(!adapter.safe());
    }
}
