//! A stub readout for parity runs that must earn real rewards: the rotation driver of flysim's ROM
//! tests (`tests/rom_catch.rs`, `tests/rom_macros_mode.rs`) -- a decoder of its own, fed rates
//! that lean on one macro population at a time, rotating every [`BURST_FRAMES`] frames.
//!
//! The brain still ticks and decodes exactly as it would; the driver only replaces the decision
//! the executor is handed, at the one point the legacy loop lets a harness do that
//! (`flysim::frame::FrameObserver::readout`). A session run hands the same driver the same
//! `(ms, bound)` in its executor ([`crate::task::PokeredTask::set_driver`]), so both arms press the
//! same macros and the rewards those macros earn reach `Agent.Commit`. Not a service path.

use flybrain_core::decoder::PopulationDecoder;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::ordered::NumberMap;

/// Frames the stub leans on one channel before the rotation moves on (`rom_catch.rs`).
pub const BURST_FRAMES: u32 = 24;
/// The hot population's rate, and every other one's (`rom_catch.rs`).
const HOT: f64 = 16.0;
const REST: f64 = 10.0;

/// Replaces a decision, given the brain clock and the scene's bound channels.
pub trait DecisionDriver: Send {
    fn readout(&mut self, ms: f64, bound: &[String], active: &mut Vec<String>);
}

/// The rotation: one macro population hot at a time, in channel order.
pub struct RotationDriver {
    decoder: PopulationDecoder,
    channels: Vec<&'static str>,
    frame: u32,
}

impl Default for RotationDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl RotationDriver {
    pub fn new() -> RotationDriver {
        let channels = flybrain_gb::macro_channels(crate::task::GAME);
        let mut decoder = PopulationDecoder::new(gameboy_decoder_config_with_macros(&channels))
            .expect("the preset is well formed");
        decoder.calibrate(&rates(&channels, None));
        RotationDriver {
            decoder,
            channels,
            frame: 0,
        }
    }
}

fn rates(channels: &[&str], hot: Option<&str>) -> NumberMap {
    let mut rates = NumberMap::new();
    for channel in channels {
        rates.set(channel, REST);
    }
    for bucket in 0..8 {
        rates.set(&format!("command_{bucket}"), REST);
    }
    if let Some(channel) = hot {
        rates.set(channel, HOT);
    }
    rates
}

impl DecisionDriver for RotationDriver {
    fn readout(&mut self, ms: f64, bound: &[String], active: &mut Vec<String>) {
        let hot = self.channels[(self.frame / BURST_FRAMES) as usize % self.channels.len()];
        self.frame += 1;
        *active = self.decoder.decode_bound(
            &rates(&self.channels, Some(hot)),
            ms,
            false,
            None,
            Some(bound),
        );
    }
}
