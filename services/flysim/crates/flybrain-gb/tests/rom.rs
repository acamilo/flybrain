//! Integration tests that need a real cartridge.
//!
//! Gated on `FLY_ROM` pointing at a Game Boy ROM file. The ROM never enters
//! this repository (`.gitignore` excludes `*.gb`), so these skip cleanly when
//! the variable is unset:
//!
//! ```sh
//! FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
//!   cargo test --release --test rom -- --nocapture
//! ```
//!
//! `FLY_CHECKPOINT` optionally points at a prototype `.checkpoint` file, which
//! probes whether a save state written by the Emscripten build imports into the
//! native build. See the crate README, "State format".

use std::time::Instant;

use flybrain_gb::adapter::{MapExit, MemoryReader};
use flybrain_gb::emulator::{AUDIO_SILENCE_LEVEL, CPU_TICKS_PER_SECOND};
use flybrain_gb::pokemon_red::symbols::ram;
use flybrain_gb::pokemon_red::{PokemonRedReward, SUPPORTED_ROM, engage};
use flybrain_gb::{
    DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, FRAMEBUFFER_LEN, GameAdapter,
    buttons,
};

/// The ROM under test, or `None` to skip.
fn rom() -> Option<Vec<u8>> {
    let path = std::env::var_os("FLY_ROM")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM is set to {path:?} but could not be read: {error}"),
    }
}

fn boot() -> Option<Emulator> {
    let rom = rom()?;
    Some(
        Emulator::new(&rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
            .expect("binjgb should accept the ROM"),
    )
}

macro_rules! skip_without_rom {
    ($emulator:expr) => {
        match $emulator {
            Some(emulator) => emulator,
            None => {
                eprintln!("skipped: FLY_ROM is not set");
                return;
            }
        }
    };
}

#[test]
fn six_hundred_idle_frames_produce_moving_video_and_continuous_audio() {
    let mut emulator = skip_without_rom!(boot());
    emulator.set_buttons(buttons::NONE);

    let first = emulator.framebuffer().to_vec();
    assert_eq!(first.len(), FRAMEBUFFER_LEN);

    let mut distinct = std::collections::HashSet::new();
    let mut raw_min = u8::MAX;
    let mut raw_max = u8::MIN;
    let mut raw_total: u64 = 0;
    let mut raw_count: u64 = 0;
    let mut samples = 0usize;
    let mut silent_frames = 0usize;
    let ticks_before = emulator.ticks();

    for frame in 0..600 {
        emulator.run_frame().expect("a frame should complete");
        distinct.insert(emulator.framebuffer().to_vec());

        let raw = emulator.take_audio_u8();
        if raw.is_empty() {
            silent_frames += 1;
        }
        assert!(raw.len() % 2 == 0, "frame {frame}: audio must be stereo pairs");
        for byte in &raw {
            raw_min = raw_min.min(*byte);
            raw_max = raw_max.max(*byte);
            raw_total += u64::from(*byte);
            raw_count += 1;
        }
        samples += raw.len();
    }

    assert!(
        distinct.len() > 1,
        "the framebuffer never changed over 600 frames: the LCD is stuck"
    );
    assert_ne!(emulator.framebuffer(), first.as_slice());

    // Audio is produced per emulated tick, not per run_frame call, and a
    // run_frame during the boot interval where the LCD is off consumes several
    // frame periods. Expect samples for the ticks actually emulated.
    let ticks = (emulator.ticks() - ticks_before) as f64;
    let expected = ticks * f64::from(DEFAULT_AUDIO_FREQUENCY) * 2.0 / CPU_TICKS_PER_SECOND as f64;
    let drift = (samples as f64 - expected).abs();
    assert!(
        drift < expected * 0.001,
        "audio is {samples} samples against {expected:.0} expected over {ticks:.0} ticks: \
         the buffer is dropping blocks"
    );
    assert_eq!(silent_frames, 0, "every frame must yield some audio");
    // One Game Boy frame is 70,224 ticks of a 4,194,304 Hz clock, which at
    // 48 kHz is 803.6 stereo frames, 1,607.2 interleaved samples.
    assert!(
        samples > 600 * 1_600,
        "600 frames should carry at least 600 frames' worth of audio"
    );

    eprintln!(
        "audio: {samples} interleaved u8 samples over 600 frames and {ticks:.0} ticks \
         ({expected:.0} expected, {:.1} per frame), raw range {raw_min}..={raw_max}, \
         mean {:.1}, assumed silence level {AUDIO_SILENCE_LEVEL}",
        samples as f64 / 600.0,
        raw_total as f64 / raw_count as f64
    );
    eprintln!("video: {} distinct framebuffers over 600 frames", distinct.len());
}

#[test]
fn a_state_round_trip_replays_to_an_identical_framebuffer() {
    let mut emulator = skip_without_rom!(boot());
    for _ in 0..600 {
        emulator.run_frame().unwrap();
    }

    let state = emulator.export_state().expect("state export");
    assert_eq!(state.len(), Emulator::state_size());
    let at_export = emulator.framebuffer().to_vec();
    emulator.take_audio_u8();

    for _ in 0..60 {
        emulator.run_frame().unwrap();
    }
    let reference = emulator.framebuffer().to_vec();
    let reference_audio = emulator.take_audio_u8();
    assert_ne!(reference, at_export, "60 frames should change the picture");

    emulator.import_state(&state).expect("state import");
    // EmulatorState excludes the framebuffer, so the restored emulator still
    // shows the frame it was on until the next frame completes.
    emulator.take_audio_u8();
    for _ in 0..60 {
        emulator.run_frame().unwrap();
    }
    assert_eq!(
        emulator.framebuffer(),
        reference.as_slice(),
        "a restored state must replay to the same picture"
    );
    // Audio is NOT bit-identical across a restore, and cannot be: the
    // resampler's phase (AudioBuffer.freq_counter and .divisor) lives outside
    // EmulatorState, so emulator_read_state does not restore it. The block
    // length stays within a sample or two of the reference.
    let restored_audio = emulator.take_audio_u8();
    let difference = restored_audio.len().abs_diff(reference_audio.len());
    assert!(
        difference <= 2,
        "restored audio is {} samples against {} in the reference",
        restored_audio.len(),
        reference_audio.len()
    );
    if restored_audio != reference_audio {
        eprintln!(
            "state: audio differs after restore by {difference} samples of length \
             (the resampler phase is outside EmulatorState)"
        );
    }

    let mut truncated = state.clone();
    truncated.pop();
    assert!(emulator.import_state(&truncated).is_err(), "a short state is refused");

    eprintln!("state: {} bytes, round trip identical", state.len());
}

#[test]
fn the_reward_adapter_leaves_boot_once_the_game_starts() {
    let mut emulator = skip_without_rom!(boot());
    assert_eq!(
        emulator.rom_sha256(),
        SUPPORTED_ROM,
        "FLY_ROM is not the cartridge the adapter is pinned to; \
         semantic rewards would be disabled"
    );

    let mut adapter = PokemonRedReward::new();
    let mut ms = 0.0;

    // Hold Start, then A, on a slow alternation: the title screen wants Start,
    // the menu and Oak's introduction want A, and a held button is ignored, so
    // each press has to be released. 4,000 frames is about 67 seconds of game
    // time, enough for the intro and the naming screens' default answers.
    //
    // Note that a cold boot's WRAM is pseudorandom (binjgb seeds it from
    // EmulatorInit.random_seed), so the very first samples can read a garbage
    // "game timer active" bit and report TRANSITION before the game clears
    // WRAM. The assertion is therefore that BOOT is the settled early mode,
    // not that frame 1 is BOOT.
    const FRAMES: u32 = 4_000;
    let mut timeline: Vec<(u32, String)> = Vec::new();
    let mut last_boot = None;
    let mut events = 0usize;
    for frame in 0..FRAMES {
        let mask = match frame % 32 {
            0..=7 => buttons::START,
            16..=23 => buttons::A,
            _ => buttons::NONE,
        };
        emulator.set_buttons(mask);
        emulator.run_frame().unwrap();
        ms += 1000.0 / 59.7275;
        events += adapter.sample(&mut emulator, ms).len();
        if adapter.mode() == "BOOT" {
            last_boot = Some(frame);
        }
        if timeline.last().map(|(_, mode)| mode.as_str()) != Some(adapter.mode()) {
            timeline.push((frame, adapter.mode().to_string()));
        }
    }

    eprintln!("reward: mode timeline (frame, mode) = {timeline:?}");
    let last_boot = last_boot.expect("the adapter never reported BOOT");
    assert!(
        last_boot < FRAMES - 1,
        "still in BOOT at frame {last_boot}: the game never started"
    );
    assert_ne!(adapter.mode(), "BOOT", "the run should end out of BOOT");
    eprintln!("reward: left BOOT for good at frame {}, {events} payouts", last_boot + 1);

    let progress = GameAdapter::progress(&adapter);
    eprintln!(
        "reward: rank {} ({}), {} unique locations, total {:.3}, counts {:?}",
        progress.rank, progress.rank_label, progress.unique_locations, progress.reward_total,
        progress.counts
    );
    // The intro ends standing in Red's bedroom, which is rung 1 of the 38-rung
    // ladder (`docs/design/ladder.md`). The first playable sample baselines
    // REDS_HOUSE_2F into the lifetime map ledger, so this is the ladder's floor
    // for a real cold boot, not something the fly had to earn.
    assert_eq!(progress.rank, 1, "the intro should end in the bedroom");
    assert_eq!(progress.rank_label, "BEDROOM");

    // Now walk out of the house, which is rung 2 (REDS_HOUSE_1F) and then rung 3
    // (PALLET_TOWN). Both are map rungs, so what is being tested is the whole path
    // from a real cartridge: the map id enters the adapter's lifetime ledger only
    // from an unscripted, stable, overworld sample, and the rung follows from the
    // ledger.
    //
    // The route is a fixed-seed random walk rather than steering, because steering
    // needs a collision map to be any good and the house has furniture in it: the
    // player spawns at (3, 6) in the bedroom with (3, 5) blocked, and the ground
    // floor's table blocks the straight line from the stairs at (7, 1) to the front
    // door at (2, 7). Both were measured stalling for the whole budget. A random
    // walk needs no map knowledge, is exactly as deterministic (the PRNG is seeded
    // and binjgb is deterministic), and clears the house in about 12,000 frames.
    //
    // Buttons are pulsed rather than held because a held button is ignored. B is
    // pulsed too: the intro's A-mashing leaves a text box open, and until it is
    // dismissed the player cannot move at all (measured: 3,000 frames of pure
    // directions never left (3, 6) while `wFontLoaded` stayed set). B is the right
    // button for it -- it advances text but, unlike A, never starts a new
    // conversation with whatever the player happens to be facing.
    const WALK_FRAMES: u32 = 24_000;
    let mut seed: u64 = 0x5eed_1234_5678_9abc;
    let mut maps: Vec<u32> = Vec::new();
    let mut climbed: Vec<(u32, &'static str)> = Vec::new();
    let mut best = progress.rank;
    for frame in 0..WALK_FRAMES {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let step = [buttons::UP, buttons::DOWN, buttons::LEFT, buttons::RIGHT]
            [((seed >> 33) % 4) as usize];
        let mask = match frame % 24 {
            0..=7 => step,
            16..=19 => buttons::B,
            _ => buttons::NONE,
        };
        emulator.set_buttons(mask);
        emulator.run_frame().unwrap();
        ms += 1000.0 / 59.7275;
        adapter.sample(&mut emulator, ms);
        if let Some(map) = adapter.map()
            && maps.last() != Some(&map)
        {
            maps.push(map);
        }
        let progress = GameAdapter::progress(&adapter);
        assert!(
            progress.rank >= best,
            "rank fell from {best} to {} at frame {frame}",
            progress.rank
        );
        if progress.rank > best {
            climbed.push((progress.rank, progress.rank_label));
        }
        best = progress.rank;
    }

    let progress = GameAdapter::progress(&adapter);
    eprintln!(
        "reward: after {WALK_FRAMES} walking frames, rank {} ({}), maps {maps:?}, \
         climbed {climbed:?}",
        progress.rank, progress.rank_label
    );
    assert!(
        climbed.len() >= 2,
        "walking out of the house should climb at least two rungs, got {climbed:?} \
         having visited maps {maps:?}"
    );
    assert_eq!(
        &climbed[..2],
        &[(2, "DOWNSTAIRS"), (3, "PALLET TOWN")],
        "the first two rungs off the bedroom are the ground floor and Pallet Town"
    );
}

#[test]
fn the_emulator_alone_runs_at_a_reported_frame_rate() {
    let mut emulator = skip_without_rom!(boot());
    // Warm up past the boot sequence so the measurement covers real rendering.
    for _ in 0..600 {
        emulator.run_frame().unwrap();
        emulator.take_audio_u8();
    }

    const FRAMES: u32 = 6_000;
    let start = Instant::now();
    for _ in 0..FRAMES {
        emulator.run_frame().unwrap();
        emulator.take_audio_u8();
    }
    let elapsed = start.elapsed();
    let fps = f64::from(FRAMES) / elapsed.as_secs_f64();
    eprintln!(
        "emulator: {fps:.0} fps over {FRAMES} frames in {:.3} s ({:.1}x real time at 59.7275 fps){}",
        elapsed.as_secs_f64(),
        fps / 59.7275,
        if cfg!(debug_assertions) { ", DEBUG BUILD" } else { ", release" },
    );
    assert!(fps > 30.0, "{fps:.0} fps is too slow to be believable");
}

#[test]
fn reading_wram_is_cached_within_a_frame_and_refreshed_across_frames() {
    let mut emulator = skip_without_rom!(boot());
    for _ in 0..600 {
        emulator.run_frame().unwrap();
    }
    // 0xff04 is DIV, which increments every 256 ticks and so advances by about
    // 274 over one frame: it must look constant within a frame and different
    // across one.
    const DIV: u16 = 0xff04;
    let first = emulator.read_wram(DIV);
    for _ in 0..1_000 {
        assert_eq!(emulator.read_wram(DIV), first);
    }
    assert_eq!(emulator.read8(DIV), first, "the MemoryReader impl shares the cache");

    let mut changed = false;
    for _ in 0..8 {
        emulator.run_frame().unwrap();
        if emulator.read_wram(DIV) != first {
            changed = true;
            break;
        }
    }
    assert!(changed, "the cache is not cleared at a frame boundary");
}

/// Probe: can a save state the Emscripten build wrote be imported here?
///
/// `emulator_write_state` memcpy's binjgb's `EmulatorState`, so this answers a
/// question about struct layout, not about binjgb's format. Reports and skips
/// rather than failing: an incompatible answer is expected and is what the
/// `statefmt:` compatibility segment exists for.
#[test]
fn a_prototype_wasm_checkpoint_state_is_probed_for_importability() {
    let mut emulator = skip_without_rom!(boot());
    let Some(path) = std::env::var_os("FLY_CHECKPOINT") else {
        eprintln!("skipped: FLY_CHECKPOINT is not set");
        return;
    };
    let bytes = std::fs::read(&path).expect("FLY_CHECKPOINT should be readable");
    let Some(state) = prototype_chunk(&bytes, "emulator") else {
        eprintln!("skipped: {path:?} has no `emulator` chunk");
        return;
    };

    eprintln!(
        "statefmt probe: prototype chunk is {} bytes, this build's EmulatorState is {} bytes",
        state.len(),
        Emulator::state_size()
    );
    match emulator.import_state(state) {
        Ok(()) => eprintln!(
            "statefmt probe: the Emscripten-era state IMPORTED; sizes agree and the header matched"
        ),
        Err(error) => eprintln!(
            "statefmt probe: the Emscripten-era state did NOT import ({error}); \
             prototype milestone saves cannot be reused by the native build"
        ),
    }
}

/// Minimal reader for the prototype's `FLYPOKE1` checkpoint container
/// (`fly-plays-pokemon/src/runtime/checkpoint.ts`): magic, a little-endian
/// manifest length, the JSON manifest naming the chunks in order, then each
/// chunk as a little-endian length followed by its bytes.
fn prototype_chunk<'a>(bytes: &'a [u8], want: &str) -> Option<&'a [u8]> {
    const MAGIC: &[u8] = b"FLYPOKE1";
    if !bytes.starts_with(MAGIC) {
        return None;
    }
    let mut offset = MAGIC.len();
    let manifest_len = read_u32(bytes, &mut offset)? as usize;
    let manifest: serde_json::Value =
        serde_json::from_slice(bytes.get(offset..offset + manifest_len)?).ok()?;
    offset += manifest_len;
    for name in manifest.get("chunks")?.as_array()? {
        let len = read_u32(bytes, &mut offset)? as usize;
        let chunk = bytes.get(offset..offset + len)?;
        offset += len;
        if name.as_str() == Some(want) {
            return Some(chunk);
        }
    }
    None
}

fn read_u32(bytes: &[u8], offset: &mut usize) -> Option<u32> {
    let slice = bytes.get(*offset..*offset + 4)?;
    *offset += 4;
    Some(u32::from_le_bytes(slice.try_into().ok()?))
}

/// The boundary rule, against the cartridge rather than a synthetic trace.
///
/// `docs/design/room-escape.md` section 2, Verification: "from the bedroom archive, the first
/// payouts are boundary events near the stairs". Since `pokered-unique8-v7` the bedroom is
/// *indoors* (`engage::indoor`: tileset `REDS_HOUSE_2`, neither outside nor bike-ridable), so
/// the same walk now proves the other half of the rule: the stairs enter the ledger where they
/// always did, and nothing is paid for them. Three things are checked here that no synthetic
/// WRAM trace can check:
///
/// 1. **the warp table layout.** `RedsHouse2F_Object` declares exactly one warp,
///    `warp_event 7, 1, REDS_HOUSE_1F, 3`, and `MACRO warp_event` emits `db \2, \1, ...` -- so
///    the bytes at `wWarpEntries` must read Y = 1 then X = 7 on a running cartridge. If the
///    layout were X-then-Y, or the stride were not four, this assertion is what fails.
/// 2. **that the rule fires at the right place.** The fly spawns at (3, 6) with the stairs at
///    (7, 1), four tiles and five rows away, so the stairs are not baselined by the first
///    playable sample and have to be walked to -- and the ledger records them from beside them.
/// 3. **that the cartridge calls the bedroom a building**: `wCurMapTileset` reads `REDS_HOUSE_2`
///    on the running game, so the rule pays nothing there.
#[test]
fn the_boundary_rule_records_the_bedroom_stairs_and_pays_nothing_indoors() {
    let mut emulator = skip_without_rom!(boot());
    assert_eq!(emulator.rom_sha256(), SUPPORTED_ROM, "FLY_ROM is not the pinned cartridge");
    let mut adapter = PokemonRedReward::new();
    let mut ms = 0.0;

    // The intro, then B to dismiss the text box the A-mashing leaves open; the same pattern as
    // `the_reward_adapter_leaves_boot_once_the_game_starts`, which explains why.
    for frame in 0..6_000u32 {
        let mask = match frame % 32 {
            0..=7 => buttons::START,
            16..=23 => buttons::A,
            _ => buttons::NONE,
        };
        emulator.set_buttons(mask);
        emulator.run_frame().unwrap();
        ms += 1000.0 / 59.7275;
        adapter.sample(&mut emulator, ms);
        if frame > 3_000 && adapter.mode() == "OVERWORLD" {
            break;
        }
    }
    for frame in 0..12_000u32 {
        emulator.set_buttons(if frame % 24 < 8 { buttons::B } else { buttons::NONE });
        emulator.run_frame().unwrap();
        ms += 1000.0 / 59.7275;
        adapter.sample(&mut emulator, ms);
        if adapter.safe_for_snapshot() {
            break;
        }
    }
    assert!(adapter.safe_for_snapshot(), "the intro never settled (mode {})", adapter.mode());
    assert_eq!(adapter.map_id(), Some(0x26), "a cold boot ends in Red's bedroom");

    // 1. The warp table, as the decomp declares it.
    assert_eq!(emulator.read_wram(ram::wNumberOfWarps), 1, "the bedroom has one warp");
    assert_eq!(emulator.read_wram(ram::wWarpEntries), 1, "the stairs' Y comes first");
    assert_eq!(emulator.read_wram(ram::wWarpEntries + 1), 7, "then its X");
    assert_eq!(
        emulator.read_wram(ram::wCurMapConnections),
        0,
        "an indoor map has no connected edges"
    );
    assert_eq!(emulator.read_wram(ram::wCurMapTileset), 4, "REDS_HOUSE_2");
    assert!(engage::indoor(4), "and the rule calls it a building");
    let (spawn_x, spawn_y) =
        (emulator.read_wram(ram::wXCoord), emulator.read_wram(ram::wYCoord));
    assert!(
        spawn_x.abs_diff(7) + spawn_y.abs_diff(1) > 1,
        "the fly spawns at ({spawn_x}, {spawn_y}), which the first sample would baseline"
    );

    // 2. Walk, with no map knowledge, and watch where the boundary payouts land.
    let mut seed: u64 = 0x5eed_1234_5678_9abc;
    let mut boundary: Vec<(u8, u8, f64)> = Vec::new();
    let mut kinds: Vec<&'static str> = Vec::new();
    let mut found_at: Option<(u8, u8)> = None;
    for frame in 0..24_000u32 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let step = [buttons::UP, buttons::DOWN, buttons::LEFT, buttons::RIGHT]
            [((seed >> 33) % 4) as usize];
        let mask = match frame % 24 {
            0..=7 => step,
            16..=19 => buttons::B,
            _ => buttons::NONE,
        };
        emulator.set_buttons(mask);
        emulator.run_frame().unwrap();
        ms += 1000.0 / 59.7275;
        let x = emulator.read_wram(ram::wXCoord);
        let y = emulator.read_wram(ram::wYCoord);
        for event in adapter.sample(&mut emulator, ms) {
            kinds.push(event.kind);
            if event.kind == "boundary" {
                boundary.push((x, y, event.value));
            }
        }
        if found_at.is_none() && adapter.exit_visited(MapExit::Warp { map: 0x26, x: 7, y: 1 }) {
            found_at = Some((x, y));
        }
        if adapter.map_id() != Some(0x26) {
            break;
        }
    }

    eprintln!("boundary: payouts in the bedroom {boundary:?}, all kinds {kinds:?}");
    let (x, y) = found_at.expect("the stairs were never found");
    assert!(
        x.abs_diff(7) + y.abs_diff(1) <= 1,
        "the stairs entered the ledger at ({x}, {y}), not next to the stairs at (7, 1)"
    );
    assert!(boundary.is_empty(), "an indoor exit pays nothing, got {boundary:?}");
}
