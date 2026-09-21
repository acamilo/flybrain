//! The platformer demo against a real cartridge.
//!
//! Gated on `FLY_ROM_PLATFORMER` pointing at a Super Mario Land ROM, and skips cleanly without it,
//! the same convention as `flybrain-gb/tests/rom.rs` and `tests/integration.rs`. No such cartridge
//! exists on the machine this was written on, so the test is written to run when one does rather
//! than to pass by being empty:
//!
//! ```sh
//! FLY_ROM_PLATFORMER="$HOME/roms/Super Mario Land (World) (Rev A).gb" \
//!   cargo test --release -p flysim --test rom_platformer -- --nocapture
//! ```
//!
//! `FLY_ROM_PLATFORMER_SHA256` is optional: set it and the test asserts the cartridge on disk is
//! that one; leave it unset and the test pins whatever is on disk and prints the hash, which is how
//! the value for `[game.platformer] rom_sha256` gets established in the first place
//! (`docs/design/platformer.md` §8.5 records only the disassembly's SHA-1).
//!
//! This lives in `flysim` rather than in `flybrain-gb` on purpose: `flybrain-gb` deliberately does
//! not depend on `flybrain-core`, and driving the game to a playable state needs the readout.
//!
//! The restore case seeds the ratchet from a snapshot it captures itself, which is the
//! `RATCHET_CHECKPOINT` idea from the design without needing a file: the point is to exercise the
//! restore path without waiting out a 300-second stall.

use std::path::PathBuf;

use flybrain_core::agent::GAMEBOY_MS_PER_FRAME;
use flybrain_core::decoder::PopulationDecoder;
use flybrain_core::decoder::gameboy::to_button_mask;
use flybrain_core::decoder::platformer::platformer_decoder_config;
use flybrain_core::ordered::NumberMap;
use flybrain_gb::adapter::{GameAdapter, MemoryReader};
use flybrain_gb::platformer::{PlatformerAdapter, symbols};
use flybrain_gb::ratchet::{Ratchet, Snapshot};
use flybrain_gb::recovery::{ClosureRecovery, recover_game};
use flybrain_gb::{DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, FRAMEBUFFER_LEN};

/// Roles the readout reads, and the resting rate every score is measured against.
const ROLES: [&str; 8] = [
    "command_0",
    "command_1",
    "command_2",
    "command_3",
    "command_4",
    "command_5",
    "command_6",
    "command_7",
];
/// A flat calibration point in the middle of the range the walker draws from, so scores straddle 1.
const BASELINE_HZ: f64 = 30.0;
const MAX_HZ: f64 = 60.0;

fn rom() -> Option<Vec<u8>> {
    let path = PathBuf::from(std::env::var_os("FLY_ROM_PLATFORMER")?);
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM_PLATFORMER is {path:?} but could not be read: {error}"),
    }
}

macro_rules! skip_without_rom {
    ($value:expr) => {
        match $value {
            Some(value) => value,
            None => {
                eprintln!("skipped: FLY_ROM_PLATFORMER is not set");
                return;
            }
        }
    };
}

/// The `xorshift` generator the rest of the workspace's fixtures use.
fn xorshift(seed: i32) -> impl FnMut() -> f64 {
    let mut state = if seed == 0 { 1 } else { seed };
    move || {
        state ^= state << 13;
        state ^= ((state as u32) >> 17) as i32;
        state ^= state << 5;
        f64::from(state as u32) / 4_294_967_296.0
    }
}

struct Run {
    emulator: Emulator,
    decoder: PopulationDecoder,
    adapter: PlatformerAdapter,
    random: Box<dyn FnMut() -> f64>,
    ms: f64,
    frames: u64,
    rewards: usize,
}

impl Run {
    fn start(rom: &[u8], seed: i32) -> Self {
        let emulator = Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
            .expect("binjgb should accept the cartridge");
        let sha256 = emulator.rom_sha256();
        match std::env::var("FLY_ROM_PLATFORMER_SHA256") {
            Ok(expected) => assert_eq!(
                expected.to_ascii_lowercase(),
                sha256,
                "the cartridge on disk is not the pinned one"
            ),
            Err(_) => eprintln!(
                "FLY_ROM_PLATFORMER_SHA256 is unset; pinning the cartridge on disk.\n\
                 set `[game.platformer] rom_sha256 = \"{sha256}\"` once it is reviewed"
            ),
        }
        let adapter = PlatformerAdapter::with_rom_pin(Some(&sha256));
        assert!(adapter.rom_allowed(&sha256), "the pin must enable semantic rewards");

        let mut decoder =
            PopulationDecoder::new(platformer_decoder_config()).expect("a valid preset");
        decoder.calibrate(&NumberMap::from_pairs(
            ROLES.iter().map(|role| (*role, BASELINE_HZ)),
        ));
        Self {
            emulator,
            decoder,
            adapter,
            random: Box::new(xorshift(seed)),
            ms: 0.0,
            frames: 0,
            rewards: 0,
        }
    }

    /// One frame: seeded random role rates in, buttons out, one emulator frame, one reward sample.
    fn step(&mut self) {
        self.ms += GAMEBOY_MS_PER_FRAME;
        let mut rates = NumberMap::new();
        for role in ROLES {
            rates.set(role, (self.random)() * MAX_HZ);
        }
        let boot = self.adapter.boot();
        let active = self.decoder.decode(&rates, self.ms, boot);
        self.emulator.set_buttons(to_button_mask(&active) as u8);
        self.emulator.run_frame().expect("a frame should complete");
        self.frames += 1;
        self.rewards += self.adapter.sample(&mut self.emulator, self.ms).len();
    }

    fn read(&mut self, address: u16) -> u8 {
        self.emulator.read8(address)
    }

    /// Step until `playable`, or fail after `limit` frames.
    fn play(&mut self, limit: u64) {
        for _ in 0..limit {
            self.step();
            if !self.adapter.boot() {
                return;
            }
        }
        panic!(
            "no playable sample in {limit} frames ({:.0} brain seconds); last mode {:?}",
            self.ms / 1000.0,
            self.adapter.mode()
        );
    }
}

#[test]
fn random_rates_boot_the_cartridge_into_level_1_1_and_earn_something() {
    let rom = skip_without_rom!(rom());
    let mut run = Run::start(&rom, 20260915);

    // 6,000 frames is about 100 brain seconds: the intro plus a title screen where Start fires at
    // its boot cooldown of 2.5 s.
    run.play(6_000);
    eprintln!(
        "playable after {} frames ({:.1} brain seconds), mode {:?}",
        run.frames,
        run.ms / 1000.0,
        run.adapter.mode()
    );

    assert_eq!(run.read(symbols::hram::hWorldAndLevel), 0x11, "the first level is 1-1");
    assert_eq!(run.read(symbols::hram::hLevelIndex), 0);
    assert_eq!(
        u32::from(run.read(symbols::hram::hScreenIndex)),
        symbols::FIRST_SCREEN,
        "every level starts on screen 3"
    );
    assert_eq!(run.read(symbols::hram::hGamePaused), 0);
    assert_eq!(run.read(symbols::hram::UNNAMED_DEMO_GATE), 0, "not the attract demo");
    assert_eq!(run.adapter.mode(), "IN LEVEL 1-1");
    assert!(run.adapter.rank() >= 1);
    assert_eq!(run.adapter.map_id(), Some(0));

    // Sixty brain seconds of play. A walker that holds right for a quarter second at a time covers
    // ground, and ground is the primary reward.
    let until = run.ms + 60_000.0;
    while run.ms < until {
        run.step();
    }
    let statistics = run.adapter.statistics();
    eprintln!(
        "60 s of play: {} payouts, total {:.3}, bands {}, counts {:?}",
        run.rewards, statistics.total, statistics.bands, statistics.counts
    );
    assert!(run.rewards > 0, "60 brain seconds produced no reward at all");
    assert!(statistics.total > 0.0);
}

#[test]
fn a_seeded_archive_restores_without_waiting_out_a_stall() {
    let rom = skip_without_rom!(rom());
    let mut run = Run::start(&rom, 4242);
    run.play(6_000);

    // Step until the adapter offers a safe sample, then archive it. This is the seeded-archive
    // variant from the design: the restore path is exercised without a 300-second stall.
    let mut ratchet = Ratchet::with_policy(run.adapter.recovery_policy());
    let mut archived: Option<Snapshot> = None;
    for _ in 0..6_000 {
        run.step();
        if run.adapter.safe_for_snapshot() {
            archived = Some(Snapshot {
                game: run.emulator.export_state().expect("an emulator state"),
                frame: run.emulator.framebuffer().to_vec(),
            });
            break;
        }
    }
    let snapshot = archived.expect("no safe sample in 6,000 frames of play");
    assert_eq!(snapshot.frame.len(), FRAMEBUFFER_LEN);
    let rank = run.adapter.rank();
    let total = run.adapter.statistics().total;
    let bands = run.adapter.statistics().bands;
    // Archiving a new best rank is not itself a recovery.
    assert!(!ratchet.observe_with_game_over(
        true,
        u64::from(rank),
        bands,
        run.ms as u64,
        false,
        || snapshot.clone()
    ));
    assert_eq!(ratchet.state.best, u64::from(rank));

    // Play on, then claim a game over: the platformer policy restores immediately.
    for _ in 0..600 {
        run.step();
    }
    assert!(
        ratchet.observe_with_game_over(false, u64::from(rank), bands, run.ms as u64, true, || {
            unreachable!("no capture is due")
        }),
        "a game over must restore at once under the platformer policy"
    );
    assert_eq!(ratchet.state.recoveries, 1);

    let mut cleared = (false, false, false);
    {
        let mut neural = ClosureRecovery {
            clear_holds: || cleared.0 = true,
            clear_eligibility: || cleared.1 = true,
            set_visual_frame: |frame: &[u8]| {
                assert_eq!(frame.len(), FRAMEBUFFER_LEN);
                cleared.2 = true;
            },
        };
        let restored = recover_game(
            &mut run.emulator,
            &mut run.adapter,
            &mut neural,
            ratchet.snapshot.as_ref().expect("the ratchet archived a snapshot"),
        )
        .expect("the archived state should load");
        assert_eq!(restored.len(), FRAMEBUFFER_LEN);
    }
    assert_eq!(cleared, (true, true, true), "every recovery hook must fire");

    // The lifetime ledger survives the rollback, and the transients do not.
    assert_eq!(run.adapter.statistics().total, total);
    assert_eq!(run.adapter.statistics().bands, bands);
    assert_eq!(run.adapter.rank(), rank);
    assert_eq!(run.adapter.level(), None, "the level latch is transient");
    assert!(!run.adapter.safe_for_snapshot());

    // And the restored game is playable again within a few frames, paying nothing for the ground it
    // has already been paid for.
    let before = run.adapter.statistics().total;
    for _ in 0..10 {
        run.step();
    }
    assert!(run.adapter.statistics().total >= before);
}
