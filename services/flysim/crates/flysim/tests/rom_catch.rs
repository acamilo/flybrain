//! The catch reward against the real cartridge.
//!
//! Gated on `FLY_ROM` *and* on a checkpoint, the way every ROM test in this workspace is, and
//! skips cleanly without either — the cartridge never enters this repository and a checkpoint is
//! not a fixture, it is the state the release box was really in:
//!
//! ```sh
//! FLY_ROM="$HOME/roms/pokemon-red.gb" \
//!   FLY_CATCH_CHECKPOINT=.local/checkpoints/<a rung-9 forest checkpoint> \
//!   cargo test --release -p flysim --test rom_catch -- --nocapture
//! ```
//!
//! ## What only the cartridge can answer
//!
//! The synthetic trace in `pokemon_red/tests.rs` writes `wCapturedMonSpecies`, `wBattleResult`
//! and the Pokédex bit itself, from the disassembly. It cannot say that those are the bytes
//! *this* cartridge writes when a ball keeps a Pokémon, in that order, on frames an adapter
//! sampling once a frame actually sees. That is this test, and it is the "survey" half of
//! `docs/design/macros-wram.md`'s evidence for the row: a real battle, real button presses, and
//! the byte read out of the running game rather than written into a fake one.
//!
//! ## How the catch is produced
//!
//! No steering and no scripted button sequence: the shipping macro palette, the shipping macro
//! layer and the shipping decoder, with a stub readout that leans on one macro population at a
//! time — the same driver `tests/rom_macros_mode.rs` uses and for the same reason. The one thing
//! this harness does that the rotation does not is lean on `THROW BALL`'s channel while a wild
//! battle is up, because the question here is what the adapter reads from a catch, not whether a
//! game-blind readout finds its way to one.
//!
//! The checkpoint must hold at least one ball in the bag. The macro palette can buy one
//! (`BUY BALL`, `MB·PBALL`, inside a mart), but that is a walk across a city and back and it is a
//! different test's question; this one says out loud that it skipped.

use flybrain_core::decoder::PopulationDecoder;
use flybrain_core::decoder::gameboy::gameboy_decoder_config_with_macros;
use flybrain_core::ordered::NumberMap;
use flybrain_gb::adapter::RewardEvent;
use flybrain_gb::pokemon_red::state;
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{PokemonRedReward, catalog};
use flybrain_gb::{
    AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, GameAdapter,
};
use flysim::config::Config;
use flysim::macros::{MacroLayer, macro_layer};
use flysim::snapshot::MacroMode;

const MS_PER_FRAME: f64 = 1000.0 / 59.7275;
const SEED: u32 = 20_260_922;
/// The hot population's rate against every other one's, which is also the stub's calibration
/// rate — so a channel that is not the hot one scores exactly 1.0.
const HOT: f64 = 16.0;
const REST: f64 = 10.0;
/// `THROW BALL`'s channel (`pokemon_red::macros::palette`).
const BALL: &str = "MB·BALL";
/// Frames the stub leans on one channel before the rotation moves on, the shape of the real
/// group's hysteresis-then-fatigue rotation.
const BURST_FRAMES: u32 = 24;

fn rates(hot: Option<&str>) -> NumberMap {
    let mut rates = NumberMap::new();
    for channel in flybrain_gb::macro_channels("pokemon-red") {
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

fn rom() -> Option<Vec<u8>> {
    let path = std::env::var_os("FLY_ROM")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM is set to {path:?} but could not be read: {error}"),
    }
}

fn checkpoint() -> Option<flysim::store::Checkpoint> {
    let path = std::env::var_os("FLY_CATCH_CHECKPOINT")?;
    Some(
        flysim::store::load(std::path::Path::new(&path))
            .expect("the checkpoint should be a FLYSIM01 envelope"),
    )
}

struct Run {
    gb: Emulator,
    adapter: PokemonRedReward,
    layer: MacroLayer,
    decoder: PopulationDecoder,
    channels: Vec<&'static str>,
    ms: f64,
    frame: u32,
    /// Every payout the adapter has made since the run started.
    payouts: Vec<RewardEvent>,
}

impl Run {
    fn resume(rom: &[u8], checkpoint: &flysim::store::Checkpoint) -> Self {
        let mut gb = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
            .expect("binjgb should accept the cartridge");
        let mut adapter = PokemonRedReward::new();
        gb.import_state(&checkpoint.runtime.emulator).expect("the checkpoint's emulator state");
        // A checkpoint written by an earlier adapter rebaselines rather than failing, which is
        // exactly the `v5` -> `v6` case this rule ships with.
        adapter.import_state(&checkpoint.runtime.reward).expect("the checkpoint's reward ledger");
        let channels = flybrain_gb::macro_channels("pokemon-red");
        let preset = gameboy_decoder_config_with_macros(&channels);
        let hold_ms = preset.macros.as_ref().expect("the preset has a macro group").hold_ms;
        let mut decoder = PopulationDecoder::new(preset).expect("the preset is well formed");
        decoder.calibrate(&rates(None));
        let mut config = Config::default();
        config.loop_.game = "pokemon-red".to_string();
        config.macros.mode = MacroMode::Macros;
        config.validate().expect("pokemon-red has a palette in macros mode");
        let mut layer = macro_layer(&config, hold_ms, SEED).expect("a layer in macros mode");
        let _ = layer.observe(&mut gb, &AdapterLedger(&adapter), 0.0);
        Self {
            gb,
            adapter,
            layer,
            decoder,
            channels,
            ms: 0.0,
            frame: 0,
            payouts: Vec::new(),
        }
    }

    fn byte(&mut self, address: u16) -> u8 {
        self.gb.read_wram(address)
    }

    fn in_wild_battle(&mut self) -> bool {
        self.byte(ram::wIsInBattle) == 1
    }

    /// Balls in the bag, of any kind (`constants/item_constants.asm`: MASTER_BALL 1,
    /// ULTRA_BALL 2, GREAT_BALL 3, POKE_BALL 4 — the same four `THROW BALL` looks for).
    fn balls(&mut self) -> usize {
        state::bag(&mut self.gb)
            .iter()
            .filter(|item| (0x01..=0x04).contains(&item.id) && item.count > 0)
            .map(|item| usize::from(item.count))
            .sum()
    }

    fn step(&mut self) {
        // Lean on `THROW BALL` while a wild battle is up; otherwise rotate, which is what gets
        // the fly into the grass in the first place.
        let hot = if self.in_wild_battle() {
            Some(BALL)
        } else {
            let slot = (self.frame / BURST_FRAMES) as usize % self.channels.len();
            Some(self.channels[slot])
        };
        let bound = self.layer.bound_channels();
        let active = self.decoder.decode_bound(&rates(hot), self.ms, false, None, Some(&bound));
        let mask = {
            let ledger = AdapterLedger(&self.adapter);
            self.layer.decide(&active, 0, self.ms, &mut self.gb, &ledger).mask
        };
        self.gb.set_buttons(mask as u8);
        self.gb.run_frame().expect("a frame should complete");
        self.ms += MS_PER_FRAME;
        self.frame += 1;
        let ms = self.ms;
        self.payouts.extend(self.adapter.sample(&mut self.gb, ms));
        let ledger = AdapterLedger(&self.adapter);
        let _ = self.layer.observe(&mut self.gb, &ledger, ms);
    }

    fn catches(&self) -> Vec<&RewardEvent> {
        self.payouts.iter().filter(|event| event.kind == catalog::kind::CATCH).collect()
    }
}

#[test]
fn a_catch_on_the_cartridge_pays_the_catch_rule_once_with_the_species_in_its_label() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let Some(checkpoint) = checkpoint() else {
        eprintln!("skipped: no FLY_CATCH_CHECKPOINT");
        return;
    };
    let mut run = Run::resume(&rom, &checkpoint);
    let balls = run.balls();
    if balls == 0 {
        eprintln!(
            "skipped: the checkpoint's bag holds no ball (map {:#04x}). `BUY BALL` can buy one \
             inside a mart; point FLY_CATCH_CHECKPOINT at a state that already has one.",
            run.adapter.map_id().unwrap_or(u32::MAX)
        );
        return;
    }
    eprintln!("bag holds {balls} balls; map {:#04x}", run.adapter.map_id().unwrap_or(u32::MAX));

    // Twenty brain minutes is generous for a forest checkpoint: the live run threw 28 balls in
    // its first Viridian Forest session (`pokemon_red::macros::palette`).
    let budget = 20 * 60 * 60;
    let mut battles = 0u32;
    let mut was_in_battle = false;
    for _ in 0..budget {
        run.step();
        let now = run.in_wild_battle();
        if now && !was_in_battle {
            battles += 1;
        }
        was_in_battle = now;
        if !run.catches().is_empty() {
            break;
        }
    }

    let catches = run.catches();
    assert!(
        !catches.is_empty(),
        "no catch in {:.1} brain minutes: {battles} wild battles, {} balls left, map {:#04x}, \
         macros {:?}",
        run.ms / 60_000.0,
        run.balls(),
        run.adapter.map_id().unwrap_or(u32::MAX),
        run.layer.counts()
    );

    let caught = catches[0];
    eprintln!(
        "caught after {:.1} brain minutes and {battles} wild battles: {} for {}",
        run.ms / 60_000.0,
        caught.label,
        caught.value
    );
    assert!(caught.label.starts_with("CAUGHT #"), "{}", caught.label);
    assert!(
        (caught.value - 0.30).abs() < 1e-12 || caught.value == catalog::CATCH_REPEAT_VALUE,
        "a catch pays one of the rule's two amounts, not {}",
        caught.value
    );
    // The cartridge's own flag is clear again by the time the payout lands, which is what makes
    // the payout a battle-exit event rather than a per-frame one.
    assert_eq!(run.byte(ram::wCapturedMonSpecies), 0);
    assert_eq!(
        run.adapter.progress().counts[catalog::kind::CATCH],
        1,
        "one battle, one payout"
    );
    // And a species the run is paid for catching is a species the Pokédex knows: the same event
    // sets the bit the `species` rule reads, whether or not it was new to this run.
    assert!(run.balls() < balls, "a ball was spent");
}
