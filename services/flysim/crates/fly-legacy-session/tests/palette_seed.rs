//! The palette seed is never read (TASK-01 review N3).
//!
//! The legacy loop seeds the macro palette with the brain's RNG state after boot; a fresh session
//! start cannot see it and uses `PALETTE_SEED`. That is equivalent only while no macro reads its
//! generator. This holds it on the cartridge: two macro layers seeded differently, handed the same
//! decisions (the ROM tests' rotation driver) over the same frames, press the same buttons and
//! report the same events, frame by frame. A macro that ever draws from its generator breaks it
//! here rather than in a shadow run.
//!
//! ROM job: `FLY_ROM` and `FLY_DOOR_CHECKPOINT` (the operator's rom env script).

use fly_legacy_session::composition::channels_and_hold;
use fly_legacy_session::driver::{DecisionDriver, RotationDriver};
use flybrain_core::decoder::gameboy::to_button_mask;
use flybrain_gb::pokemon_red::PokemonRedReward;
use flybrain_gb::{AdapterLedger, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator};
use flysim::config::Config;
use flysim::macros::macro_layer;
use flysim::snapshot::MacroMode;

#[test]
fn two_palette_seeds_press_the_same_buttons() {
    let (Some(rom), Some(path)) = (
        std::env::var_os("FLY_ROM"),
        std::env::var_os("FLY_DOOR_CHECKPOINT"),
    ) else {
        eprintln!("skipping: FLY_ROM or FLY_DOOR_CHECKPOINT is not set");
        return;
    };
    let rom = std::fs::read(rom).expect("the cartridge");
    let checkpoint = flysim::store::load(std::path::Path::new(&path)).expect("FLYSIM01");
    let mut emulator =
        Emulator::new(&rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES).expect("binjgb");
    emulator
        .import_state(&checkpoint.runtime.emulator)
        .expect("the emulator state");
    let mut adapter = PokemonRedReward::new();
    adapter
        .import_state(&checkpoint.runtime.reward)
        .expect("the adapter state");
    let mut config = Config::default();
    config.loop_.game = "pokemon-red".to_owned();
    config.macros.mode = MacroMode::Macros;
    let (_, hold_ms) = channels_and_hold(MacroMode::Macros);
    let mut a = macro_layer(&config, hold_ms, 0).expect("a layer");
    let mut b = macro_layer(&config, hold_ms, 0xdead_beef).expect("a layer");
    let mut driver = RotationDriver::new();
    let mut ms = checkpoint.agent.network.ms;
    let frames = std::env::var("FLY_TASK01_SEED_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let mut macros = 0;
    for frame in 0..frames {
        assert_eq!(
            a.bound_channels(),
            b.bound_channels(),
            "frame {frame}: bound"
        );
        let mut active = Vec::new();
        driver.readout(ms, &a.bound_channels(), &mut active);
        let raw = to_button_mask(&active);
        let da = a.decide(&active, raw, ms, &mut emulator, &AdapterLedger(&adapter));
        let db = b.decide(&active, raw, ms, &mut emulator, &AdapterLedger(&adapter));
        assert_eq!(da.mask, db.mask, "frame {frame}: mask");
        assert_eq!(da.events, db.events, "frame {frame}: events");
        macros += da.events.iter().filter(|e| e.outcome.is_none()).count();
        emulator.set_buttons(da.mask as u8);
        emulator.run_frame().expect("a frame");
        let _ = emulator.take_audio_u8();
        ms += 17.0;
        let _ = adapter.sample(&mut emulator, ms);
        let ea = a.observe(&mut emulator, &AdapterLedger(&adapter), ms);
        let eb = b.observe(&mut emulator, &AdapterLedger(&adapter), ms);
        assert_eq!(ea, eb, "frame {frame}: observe");
    }
    assert!(macros > 10, "the driver started macros ({macros})");
    eprintln!("{frames} frames, {macros} macros started: two seeds, the same presses");
}
