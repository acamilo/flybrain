//! The shim's one bulk read, on the real cartridge (MEM-01, `legacy-gameboy-v1` section 8).
//!
//! Gated on `FLY_ROM` like every ROM test here; skips cleanly without it:
//!
//! ```sh
//! . the operator's rom env script   # or FLY_ROM=... alone
//! cargo test --release -p flybrain-gb --test rom_memory_image -- --nocapture
//! ```
//!
//! What section 8 asks the implementing slice to prove, at every boundary of a run that goes from
//! power-on through the intro, the title and the naming screens into the bedroom:
//!
//! 1. **Read-only.** The emulator's exported state is byte-identical before and after the read.
//! 2. **Equal to the single reads.** Every captured byte equals the `fly_gb_read_mem` call for
//!    its address and what the per-frame cached read (the one every task and executor use)
//!    returns; every uncaptured one is `$FF`.
//! 3. **Nothing moves downstream.** Two emulators in lockstep, one read in bulk at every boundary
//!    and one never, produce the same framebuffers, audio and states frame for frame.
//! 4. **Why the register windows are out.** Reading them one byte at a time, as a full image
//!    would, does move the exported state (binjgb's lazy PPU/timer/serial catch-up), which is
//!    the amendment of 2026-09-29 to section 8.

use flybrain_gb::adapter::MemoryReader;
use flybrain_gb::{
    CAPTURED, Cartridge, DEFAULT_AUDIO_FRAMES, DEFAULT_AUDIO_FREQUENCY, Emulator, ImageReader,
    MEMORY_IMAGE_LEN, MemoryImage, NOT_CAPTURED, buttons, captured,
};

fn rom() -> Option<Vec<u8>> {
    let path = std::env::var_os("FLY_ROM")?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => panic!("FLY_ROM is set to {path:?} but could not be read: {error}"),
    }
}

fn boot(rom: &[u8]) -> Emulator {
    Emulator::new(rom, DEFAULT_AUDIO_FREQUENCY, DEFAULT_AUDIO_FRAMES)
        .expect("binjgb should accept the ROM")
}

/// A pad that gets from power-on into the game and walks: A through the intro and the naming
/// screens (a pulse every sixteen frames), then a slow rotation of directions with A between.
fn pad(frame: u32) -> u8 {
    if frame < 3_000 {
        if frame % 16 < 2 { buttons::A } else { buttons::NONE }
    } else {
        match (frame / 24) % 6 {
            0 => buttons::DOWN,
            1 => buttons::RIGHT,
            2 => buttons::UP,
            3 => buttons::A,
            4 => buttons::LEFT,
            _ => buttons::START,
        }
    }
}

const FRAMES: u32 = 4_200;

#[test]
fn the_bulk_read_is_read_only_and_equals_the_single_reads() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let mut gb = boot(&rom);
    let mut image = MemoryImage::zeroed();
    let mut checked = 0u32;
    for frame in 0..FRAMES {
        gb.set_buttons(pad(frame));
        gb.run_frame().expect("a frame");
        let _ = gb.take_audio_u8();

        let before = gb.export_state().expect("state");
        gb.read_memory_image_into(&mut image);
        let after = gb.export_state().expect("state");
        assert!(before == after, "frame {frame}: the bulk read moved the emulator's state");

        // Every byte, every 7th frame (a full sweep is 65,536 FFI calls); the WRAM/HRAM windows
        // the macros read, every frame.
        let full = frame % 7 == 0;
        for address in 0..=u16::MAX {
            let wanted = full || (0xc000..0xe000).contains(&address) || address >= 0xff80;
            if !wanted {
                continue;
            }
            if !captured(address) {
                assert_eq!(image.byte(address), NOT_CAPTURED, "frame {frame}: {address:#06x}");
                continue;
            }
            assert_eq!(
                image.byte(address),
                gb.read_uncached(address),
                "frame {frame}: {address:#06x} differs from the single read"
            );
            assert_eq!(
                image.byte(address),
                gb.read_wram(address),
                "frame {frame}: {address:#06x} differs from the cached read"
            );
        }
        if full {
            checked += 1;
        }
        // The single reads above moved nothing either.
        assert!(after == gb.export_state().expect("state"), "frame {frame}: the single reads moved it");
    }
    assert!(checked > 500, "{checked} full sweeps");
}

#[test]
fn reading_in_bulk_every_frame_changes_nothing_downstream() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let mut read = boot(&rom);
    let mut plain = boot(&rom);
    let mut image = MemoryImage::zeroed();
    for frame in 0..FRAMES {
        read.read_memory_image_into(&mut image);
        for gb in [&mut read, &mut plain] {
            gb.set_buttons(pad(frame));
            gb.run_frame().expect("a frame");
        }
        assert_eq!(read.take_audio_u8(), plain.take_audio_u8(), "frame {frame}: audio");
        assert!(read.framebuffer() == plain.framebuffer(), "frame {frame}: framebuffer");
        assert_eq!(read.ticks(), plain.ticks(), "frame {frame}: ticks");
        if frame % 50 == 0 {
            assert!(
                read.export_state().unwrap() == plain.export_state().unwrap(),
                "frame {frame}: state"
            );
        }
    }
    assert!(read.export_state().unwrap() == plain.export_state().unwrap());
}

#[test]
fn an_image_reader_answers_what_the_emulator_answers() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let mut gb = boot(&rom);
    let cartridge = Cartridge::verified(&rom, &gb.rom_sha256()).expect("the emulator's own ROM");
    assert_eq!(cartridge.sha256_hex(), gb.cartridge().sha256_hex());
    for frame in 0..3_200 {
        gb.set_buttons(pad(frame));
        gb.run_frame().expect("a frame");
    }
    let image = gb.read_memory_image();
    assert_eq!(image.as_bytes().len(), MEMORY_IMAGE_LEN);
    let mut reader = ImageReader::new(&image, &cartridge);
    for &(low, high) in &CAPTURED {
        for address in low..=high {
            assert_eq!(
                reader.read8(address),
                MemoryReader::read8(&mut gb, address),
                "{address:#06x}"
            );
        }
    }
    // Every bank the cartridge has, and one past it.
    let banks = u8::try_from(rom.len().div_ceil(0x4000)).unwrap_or(u8::MAX);
    for bank in 0..=banks {
        for address in (0u16..0x8000).step_by(97).chain([0x3fff, 0x4000, 0x7fff, 0x8000, 0xc000]) {
            assert_eq!(
                reader.read_rom(bank, address),
                MemoryReader::read_rom(&mut gb, bank, address),
                "bank {bank} {address:#06x}"
            );
        }
    }
}

/// The amendment's reason, measured: over the same run, reading any of the three register windows
/// one byte at a time -- which is what a full 64 KiB image through `fly_gb_read_mem` would do --
/// changes the exported state on some boundary, and reading the captured ranges never does.
#[test]
fn the_register_windows_are_the_reads_that_move_the_state() {
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let mut gb = boot(&rom);
    let windows: [(&str, u16, u16); 3] =
        [("vram", 0x8000, 0x9fff), ("oam", 0xfe00, 0xfe9f), ("io", 0xff00, 0xff7f)];
    let mut moved = [0u32; 3];
    let mut sampled = 0u32;
    for frame in 0..FRAMES {
        gb.set_buttons(pad(frame));
        gb.run_frame().expect("a frame");
        if frame % 40 != 0 {
            continue;
        }
        sampled += 1;
        for (index, &(_, low, high)) in windows.iter().enumerate() {
            // A throwaway copy of this boundary, so each window is measured from the same state.
            let state = gb.export_state().expect("state");
            let mut probe = boot(&rom);
            probe.import_state(&state).expect("same build");
            for address in low..=high {
                let _ = probe.read_uncached(address);
            }
            if probe.export_state().expect("state") != state {
                moved[index] += 1;
            }
        }
    }
    eprintln!(
        "{sampled} boundaries: {}",
        windows
            .iter()
            .zip(moved)
            .map(|((name, ..), n)| format!("{name} moved the state on {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    // The PPU's and the timer's catch-ups run on almost every boundary.
    assert!(moved[0] > 0, "reading VRAM never moved the state");
    assert!(moved[2] > 0, "reading I/O never moved the state");
}

/// The per-boundary cost (MEM-01 deliverable 4), printed for the report rather than asserted:
/// it is wall time on whatever box runs it. What it does assert is only that one read fits
/// comfortably inside a frame (16.74 ms), which a loaded box also meets.
///
/// - `bulk`: the shim's read into an owned image, which is what the environment pays per boundary;
/// - `copy`: cloning an image (64 KiB `memcpy`), what handing it to a store that copies costs;
/// - `digest`: SHA-256 of the image, which the live artifact may omit;
/// - `single`: the same captured addresses through 57,120 single FFI reads, for comparison.
#[test]
fn the_bulk_read_costs_a_fraction_of_a_frame() {
    use std::time::{Duration, Instant};
    let Some(rom) = rom() else {
        eprintln!("skipped: FLY_ROM is not set");
        return;
    };
    let mut gb = boot(&rom);
    for frame in 0..3_200 {
        gb.set_buttons(pad(frame));
        gb.run_frame().expect("a frame");
    }
    const ROUNDS: usize = 2_000;
    let mut bulk = Vec::with_capacity(ROUNDS);
    let mut copy = Vec::with_capacity(ROUNDS);
    let mut digest = Vec::with_capacity(ROUNDS);
    let mut single = Vec::with_capacity(ROUNDS / 20);
    let mut image = MemoryImage::zeroed();
    let mut sink = 0u64;
    for round in 0..ROUNDS {
        gb.set_buttons(pad(3_200 + round as u32));
        gb.run_frame().expect("a frame");
        let start = Instant::now();
        gb.read_memory_image_into(&mut image);
        bulk.push(start.elapsed());
        let start = Instant::now();
        let copied = image.clone();
        copy.push(start.elapsed());
        sink += u64::from(copied.byte(0xd35e));
        let start = Instant::now();
        sink += image.sha256_hex().len() as u64;
        digest.push(start.elapsed());
        if round % 20 == 0 {
            let start = Instant::now();
            for &(low, high) in &CAPTURED {
                for address in low..=high {
                    sink += u64::from(gb.read_uncached(address));
                }
            }
            single.push(start.elapsed());
        }
    }
    let stats = |name: &str, samples: &mut Vec<Duration>| {
        samples.sort();
        let at = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e6;
        let mean = samples.iter().sum::<Duration>().as_secs_f64() * 1e6 / samples.len() as f64;
        eprintln!(
            "{name:>7}: mean {mean:8.1} us  median {:8.1} us  p99 {:8.1} us  ({} samples)",
            at(0.5),
            at(0.99),
            samples.len()
        );
        at(0.5)
    };
    let median = stats("bulk", &mut bulk);
    stats("copy", &mut copy);
    stats("digest", &mut digest);
    stats("single", &mut single);
    let fps = 59.7275;
    eprintln!(
        "image {} B/boundary = {:.3} MB/s at {fps} fps; bulk median {:.2}% of a {:.2} ms frame; \
         bulk read throughput {:.0} MB/s (sink {sink})",
        MEMORY_IMAGE_LEN,
        MEMORY_IMAGE_LEN as f64 * fps / 1e6,
        median / 1e3 / (1e3 / fps) * 100.0,
        1e3 / fps,
        MEMORY_IMAGE_LEN as f64 / median,
    );
    assert!(median < 8_000.0, "a bulk read took {median} us at the median");
}
