//! Safe wrapper over the vendored binjgb core.

use std::ffi::c_void;
use std::fmt;

use sha2::{Digest, Sha256};

use crate::adapter::MemoryReader;
use crate::ffi;

/// Game Boy screen geometry, from `emulator.h`.
pub const SCREEN_WIDTH: usize = 160;
pub const SCREEN_HEIGHT: usize = 144;
/// RGBA bytes in one framebuffer (`sizeof(FrameBuffer)`).
pub const FRAMEBUFFER_LEN: usize = SCREEN_WIDTH * SCREEN_HEIGHT * 4;

/// `PPU_FRAME_TICKS` = `PPU_LINE_TICKS * SCREEN_HEIGHT_WITH_VBLANK` = 456 * 154.
/// The prototype's `FRAME_TICKS` constant.
pub const PPU_FRAME_TICKS: u64 = 70_224;

/// CPU ticks per second, for converting a tick count to wall time.
pub const CPU_TICKS_PER_SECOND: u64 = 4_194_304;

/// Attempts `run_frame` makes before giving up, matching the prototype's loop
/// bound. The LCD is off for many nominal frame intervals after a cold boot,
/// so there is no `NEW_FRAME` event to wait for yet.
const MAX_FRAME_ATTEMPTS: u32 = 120;

/// Bytes in one ROM bank (`$4000`), which is how [`Emulator::read_rom_bank`]
/// turns a bank number and a CPU address into an offset in the cartridge image.
const ROM_BANK_BYTES: usize = 0x4000;

/// Audio frequency flysim runs the emulator at. binjgb resamples internally to
/// whatever is requested, so this is only a default.
pub const DEFAULT_AUDIO_FREQUENCY: u32 = 48_000;
/// Frames per `AudioBuffer`, i.e. how much audio may queue before binjgb raises
/// `EMULATOR_EVENT_AUDIO_BUFFER_FULL`. The prototype passed 4,096; one Game Boy
/// frame is about 804 frames at 48 kHz, so a frame never overruns the buffer.
pub const DEFAULT_AUDIO_FRAMES: u32 = 4_096;

/// Sample value binjgb emits for silence.
///
/// Its mixer sums *unsigned* channel samples (`write_audio_frame` in
/// `emulator.c`: each channel contributes `0..15`, scaled by the master volume
/// and divided by the channel count), so the native buffer is unipolar. Silence
/// is 0, not mid-scale, and a Pokémon Red boot measures a raw range of 0..=44
/// with a mean of 1.2. Treating 128 as silence would put a full-scale DC offset
/// on every quiet frame.
pub const AUDIO_SILENCE_LEVEL: u8 = 0;

/// Full-scale sample value. binjgb's own SDL host divides by this
/// (`AUDIO_CONVERT_SAMPLE_FROM_U8` in `host.c` is `volume * X * (1/255)`).
pub const AUDIO_FULL_SCALE: f32 = 255.0;

/// Button mask bits, in the prototype's `BUTTON_BITS` order
/// (`fly-plays-pokemon/src/protocol.ts`).
pub mod buttons {
    pub const UP: u8 = 1 << 0;
    pub const DOWN: u8 = 1 << 1;
    pub const LEFT: u8 = 1 << 2;
    pub const RIGHT: u8 = 1 << 3;
    pub const A: u8 = 1 << 4;
    pub const B: u8 = 1 << 5;
    pub const START: u8 = 1 << 6;
    pub const SELECT: u8 = 1 << 7;
    pub const NONE: u8 = 0;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GbError {
    /// binjgb refused the ROM (empty, oversized, or an unusable cartridge header).
    RomRejected,
    /// `EMULATOR_EVENT_INVALID_OPCODE`.
    InvalidOpcode,
    /// `EMULATOR_EVENT_BREAKPOINT`. Unreachable unless the vendored core is
    /// rebuilt with `-DRGBDS_LIVE`.
    Breakpoint,
    /// 120 frame intervals elapsed with no `EMULATOR_EVENT_NEW_FRAME`.
    NoFrame,
    /// Save state is not exactly `Emulator::state_size()` bytes.
    StateSize { expected: usize, actual: usize },
    /// binjgb rejected the state bytes (header mismatch).
    StateRejected,
}

impl fmt::Display for GbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RomRejected => write!(f, "binjgb rejected this ROM"),
            Self::InvalidOpcode => write!(f, "Game Boy encountered an invalid opcode"),
            Self::Breakpoint => write!(f, "Game Boy stopped at a breakpoint"),
            Self::NoFrame => write!(f, "Game Boy did not produce a framebuffer"),
            Self::StateSize { expected, actual } => {
                write!(f, "Game Boy state size mismatch: {actual} != {expected}")
            }
            Self::StateRejected => write!(f, "Unable to restore Game Boy state"),
        }
    }
}

impl std::error::Error for GbError {}

/// Per-frame memoization of `emulator_read_mem`, so a reward sample crosses the
/// FFI boundary at most once per address per frame.
struct FrameCache {
    valid: [u64; 1024],
    bytes: [u8; 65536],
}

impl FrameCache {
    fn new() -> Box<Self> {
        Box::new(Self { valid: [0; 1024], bytes: [0; 65536] })
    }

    fn clear(&mut self) {
        self.valid = [0; 1024];
    }

    fn get(&self, address: u16) -> Option<u8> {
        let index = address as usize;
        if self.valid[index >> 6] & (1u64 << (index & 63)) != 0 {
            Some(self.bytes[index])
        } else {
            None
        }
    }

    fn put(&mut self, address: u16, value: u8) {
        let index = address as usize;
        self.valid[index >> 6] |= 1u64 << (index & 63);
        self.bytes[index] = value;
    }
}

/// A live Game Boy.
///
/// Everything the shim exposes takes an explicit handle, and the handle owns its
/// ROM copy, so the emulator is `Send`: flysim can move one onto a worker
/// thread. It is deliberately not `Sync`.
pub struct Emulator {
    gb: *mut ffi::FlyGb,
    /// The cartridge image, for [`MemoryReader::read_rom`].
    ///
    /// The shim owns its own padded copy behind the handle and does not hand it
    /// back, so this is a second one. It is read-only from here: nothing in this
    /// workspace writes a ROM byte, and the bank-addressed read is the only
    /// reason it is kept.
    rom: std::sync::Arc<[u8]>,
    rom_sha256: [u8; 32],
    audio_frequency: u32,
    /// Raw binjgb samples drained since the last [`Emulator::take_audio`].
    pending_audio: Vec<u8>,
    cache: Box<FrameCache>,
    frames: u64,
}

// Safety: the shim holds no global state and the handle owns everything it
// points at (the Emulator and the padded ROM copy).
unsafe impl Send for Emulator {}

impl Emulator {
    /// Bytes in one binjgb save state (`sizeof(EmulatorState)` on this target).
    pub fn state_size() -> usize {
        unsafe { ffi::fly_gb_state_size() }
    }

    /// Boot `rom` with an audio buffer of `audio_frames` stereo frames at
    /// `audio_rate` Hz. The ROM is copied and zero-padded up to a multiple of
    /// 32 KiB, as the prototype did before handing bytes to WASM.
    pub fn new(rom: &[u8], audio_rate: u32, audio_frames: u32) -> Result<Self, GbError> {
        let gb = unsafe {
            ffi::fly_gb_new(rom.as_ptr() as *const c_void, rom.len(), audio_rate, audio_frames)
        };
        if gb.is_null() {
            return Err(GbError::RomRejected);
        }
        let audio_frequency = unsafe { ffi::fly_gb_audio_frequency(gb) };
        debug_assert_eq!(unsafe { ffi::fly_gb_frame_buffer_size() }, FRAMEBUFFER_LEN);
        Ok(Self {
            gb,
            rom: rom.into(),
            rom_sha256: Sha256::digest(rom).into(),
            audio_frequency,
            pending_audio: Vec::new(),
            cache: FrameCache::new(),
            frames: 0,
        })
    }

    /// Advance to the next completed video frame.
    ///
    /// Mirrors `Binjgb.stepFrame` in the prototype: run to a one-frame tick
    /// deadline, extend the deadline on `UNTIL_TICKS`, and stop on `NEW_FRAME`.
    /// Audio is drained whenever binjgb reports its buffer full and again at the
    /// frame boundary, so no samples are lost across the reset `run_until`
    /// performs internally.
    pub fn run_frame(&mut self) -> Result<(), GbError> {
        self.cache.clear();
        let mut deadline = self.ticks() + PPU_FRAME_TICKS;
        let mut produced = false;
        for _ in 0..MAX_FRAME_ATTEMPTS {
            let event = unsafe { ffi::fly_gb_run_until(self.gb, deadline) };
            if event & ffi::EVENT_INVALID_OPCODE != 0 {
                return Err(GbError::InvalidOpcode);
            }
            if event & ffi::EVENT_BREAKPOINT != 0 {
                return Err(GbError::Breakpoint);
            }
            if event & ffi::EVENT_AUDIO_BUFFER_FULL != 0 {
                self.drain_audio();
            }
            if event & ffi::EVENT_NEW_FRAME != 0 {
                produced = true;
                break;
            }
            if event & ffi::EVENT_UNTIL_TICKS != 0 {
                deadline += PPU_FRAME_TICKS;
            }
        }
        if !produced {
            return Err(GbError::NoFrame);
        }
        self.drain_audio();
        self.frames += 1;
        Ok(())
    }

    /// The current framebuffer: RGBA, 160x144, row-major from the top left.
    pub fn framebuffer(&self) -> &[u8] {
        let ptr = unsafe { ffi::fly_gb_frame_buffer_ptr(self.gb) } as *const u8;
        unsafe { std::slice::from_raw_parts(ptr, FRAMEBUFFER_LEN) }
    }

    /// Hold exactly the buttons in `mask` (see [`buttons`]); everything else is
    /// released. binjgb keeps the state until the next call.
    pub fn set_buttons(&mut self, mask: u8) {
        unsafe { ffi::fly_gb_set_buttons(self.gb, u32::from(mask)) }
    }

    /// One byte off the CPU bus (`emulator_read_mem`), memoized for the current
    /// frame. This is the call the reward adapter samples WRAM with.
    pub fn read_wram(&mut self, address: u16) -> u8 {
        if let Some(value) = self.cache.get(address) {
            return value;
        }
        let value = unsafe { ffi::fly_gb_read_mem(self.gb, address) };
        self.cache.put(address, value);
        value
    }

    /// Bypass the per-frame cache. Only useful for probing memory that changes
    /// within a frame; the reward adapter must not use it.
    pub fn read_uncached(&self, address: u16) -> u8 {
        unsafe { ffi::fly_gb_read_mem(self.gb, address) }
    }

    /// One byte of ROM bank `bank`, from the cartridge image rather than the bus.
    ///
    /// `address` is a CPU address: `$0000..$4000` is bank 0 whatever `bank` says
    /// (that is what "always mapped" means) and `$4000..$8000` is the banked
    /// window. `None` for any other address and for an offset past the end of the
    /// image, which is what a bank a smaller cartridge does not have reads as.
    /// No bank register is written and the emulator's state does not move: this
    /// is a read of bytes the process already owns.
    pub fn read_rom_bank(&self, bank: u8, address: u16) -> Option<u8> {
        let offset = match address {
            0x0000..=0x3fff => usize::from(address),
            0x4000..=0x7fff => {
                usize::from(bank) * ROM_BANK_BYTES + usize::from(address) - ROM_BANK_BYTES
            }
            _ => return None,
        };
        self.rom.get(offset).copied()
    }

    /// Sample rate of the raw buffer, as binjgb configured it.
    pub fn audio_frequency(&self) -> u32 {
        self.audio_frequency
    }

    /// Take everything produced since the last call, as binjgb produced it:
    /// unsigned 8-bit interleaved stereo (left, right) at
    /// [`Emulator::audio_frequency`], unipolar with silence at 0.
    pub fn take_audio_u8(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_audio)
    }

    /// Take everything produced since the last call as interleaved stereo `f32`
    /// at [`Emulator::audio_frequency`], using binjgb's own host conversion:
    /// `sample / 255`.
    ///
    /// The result is in `[0, 1]`, not `[-1, 1]`, because binjgb's samples are
    /// unipolar with silence at 0 ([`AUDIO_SILENCE_LEVEL`]). Silence is exactly
    /// `0.0`, nothing clips, and the residual DC while the APU plays is
    /// binjgb's mixer, not this conversion. See the crate README, "Audio
    /// format", and [`Emulator::take_audio_bipolar`] for the alternative.
    pub fn take_audio(&mut self) -> Vec<f32> {
        let raw = std::mem::take(&mut self.pending_audio);
        raw.into_iter().map(|value| f32::from(value) / AUDIO_FULL_SCALE).collect()
    }

    /// As [`Emulator::take_audio`], but rescaled to fill `[-1, 1]` by treating
    /// mid-scale as zero. Louder, at the cost of a -1.0 floor during silence,
    /// so a consumer wanting this must high-pass it.
    pub fn take_audio_bipolar(&mut self) -> Vec<f32> {
        let raw = std::mem::take(&mut self.pending_audio);
        raw.into_iter()
            .map(|value| (f32::from(value) - AUDIO_FULL_SCALE / 2.0) / (AUDIO_FULL_SCALE / 2.0))
            .collect()
    }

    /// Serialize as binjgb itself does: a copy of `EmulatorState`, which is
    /// what the prototype's checkpoints embedded.
    pub fn export_state(&mut self) -> Result<Vec<u8>, GbError> {
        let size = Self::state_size();
        let mut out = vec![0u8; size];
        let code =
            unsafe { ffi::fly_gb_write_state(self.gb, out.as_mut_ptr() as *mut c_void, size) };
        if code != 0 {
            return Err(GbError::StateRejected);
        }
        Ok(out)
    }

    /// Restore a state produced by [`Emulator::export_state`] on this build.
    ///
    /// The framebuffer is not part of `EmulatorState`, so it still shows the
    /// pre-restore image until the next [`Emulator::run_frame`]. The prototype
    /// stored the matching framebuffer as a separate checkpoint chunk for that
    /// reason.
    pub fn import_state(&mut self, state: &[u8]) -> Result<(), GbError> {
        let size = Self::state_size();
        if state.len() != size {
            return Err(GbError::StateSize { expected: size, actual: state.len() });
        }
        let code =
            unsafe { ffi::fly_gb_read_state(self.gb, state.as_ptr() as *const c_void, size) };
        if code != 0 {
            return Err(GbError::StateRejected);
        }
        self.cache.clear();
        Ok(())
    }

    /// SHA-256 of the ROM bytes as supplied, lowercase hex. This is the
    /// `romSha256` half of the checkpoint compatibility string.
    pub fn rom_sha256(&self) -> String {
        let mut hex = String::with_capacity(64);
        for byte in self.rom_sha256 {
            use fmt::Write;
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    }

    /// Emulated CPU ticks since boot.
    pub fn ticks(&self) -> u64 {
        unsafe { ffi::fly_gb_get_ticks(self.gb) }
    }

    /// Frames [`Emulator::run_frame`] has completed. A lifetime processing
    /// count: it is not reset by [`Emulator::import_state`].
    pub fn frames(&self) -> u64 {
        self.frames
    }

    fn drain_audio(&mut self) {
        let size = unsafe { ffi::fly_gb_audio_size(self.gb) };
        if size > 0 {
            let ptr = unsafe { ffi::fly_gb_audio_ptr(self.gb) } as *const u8;
            self.pending_audio.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr, size) });
        }
        unsafe { ffi::fly_gb_audio_reset(self.gb) };
    }
}

impl fmt::Debug for Emulator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Emulator")
            .field("rom_sha256", &self.rom_sha256())
            .field("audio_frequency", &self.audio_frequency)
            .field("frames", &self.frames)
            .field("ticks", &self.ticks())
            .finish_non_exhaustive()
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        unsafe { ffi::fly_gb_delete(self.gb) }
    }
}

impl MemoryReader for Emulator {
    fn read8(&mut self, address: u16) -> u8 {
        self.read_wram(address)
    }

    fn read_rom(&mut self, bank: u8, address: u16) -> Option<u8> {
        self.read_rom_bank(bank, address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_and_framebuffer_sizes_match_the_headers() {
        assert_eq!(FRAMEBUFFER_LEN, unsafe { ffi::fly_gb_frame_buffer_size() });
        // sizeof(EmulatorState) is ABI dependent; assert only that it is a
        // plausible, non-zero size so a broken link is caught here.
        assert!(Emulator::state_size() > 100_000, "{}", Emulator::state_size());
    }

    #[test]
    fn an_empty_rom_is_rejected_rather_than_crashing() {
        assert_eq!(Emulator::new(&[], DEFAULT_AUDIO_FREQUENCY, 512).unwrap_err(), GbError::RomRejected);
    }

    #[test]
    fn the_unipolar_conversion_puts_silence_at_zero_and_never_clips() {
        // Exercised through a real emulator in tests/rom.rs; here the mapping
        // itself is the thing under test.
        let convert = |value: u8| f32::from(value) / AUDIO_FULL_SCALE;
        assert_eq!(convert(AUDIO_SILENCE_LEVEL), 0.0);
        assert_eq!(convert(255), 1.0);
        assert!((0..=255).all(|value| (0.0..=1.0).contains(&convert(value as u8))));
    }

    #[test]
    fn the_bipolar_conversion_spans_the_full_range() {
        let convert =
            |value: u8| (f32::from(value) - AUDIO_FULL_SCALE / 2.0) / (AUDIO_FULL_SCALE / 2.0);
        assert_eq!(convert(0), -1.0);
        assert_eq!(convert(255), 1.0);
    }
}
