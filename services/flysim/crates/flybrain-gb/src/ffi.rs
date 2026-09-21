//! Hand-written declarations for `csrc/shim.c`. No bindgen.
//!
//! Every function takes the opaque `FlyGb` handle explicitly; the shim holds no
//! global state, so one process can own several emulators.

use std::ffi::{c_int, c_void};

/// Opaque handle returned by [`fly_gb_new`]. Owns an `Emulator*` and the padded
/// ROM copy binjgb borrows.
#[repr(C)]
pub struct FlyGb {
    _private: [u8; 0],
}

/// `EMULATOR_EVENT_*` bits from `emulator.h`.
pub const EVENT_NEW_FRAME: u32 = 0x01;
pub const EVENT_AUDIO_BUFFER_FULL: u32 = 0x02;
pub const EVENT_UNTIL_TICKS: u32 = 0x04;
pub const EVENT_BREAKPOINT: u32 = 0x08;
pub const EVENT_INVALID_OPCODE: u32 = 0x10;

unsafe extern "C" {
    pub fn fly_gb_state_size() -> usize;
    pub fn fly_gb_frame_buffer_size() -> usize;

    pub fn fly_gb_new(
        rom_data: *const c_void,
        rom_size: usize,
        audio_frequency: u32,
        audio_frames: u32,
    ) -> *mut FlyGb;
    pub fn fly_gb_delete(gb: *mut FlyGb);

    pub fn fly_gb_get_ticks(gb: *mut FlyGb) -> u64;
    pub fn fly_gb_run_until(gb: *mut FlyGb, until_ticks: u64) -> u32;

    pub fn fly_gb_frame_buffer_ptr(gb: *mut FlyGb) -> *const c_void;

    pub fn fly_gb_set_buttons(gb: *mut FlyGb, mask: u32);
    pub fn fly_gb_read_mem(gb: *mut FlyGb, address: u16) -> u8;

    pub fn fly_gb_audio_frequency(gb: *mut FlyGb) -> u32;
    pub fn fly_gb_audio_size(gb: *mut FlyGb) -> usize;
    pub fn fly_gb_audio_ptr(gb: *mut FlyGb) -> *const c_void;
    pub fn fly_gb_audio_reset(gb: *mut FlyGb);

    pub fn fly_gb_write_state(gb: *mut FlyGb, out: *mut c_void, size: usize) -> c_int;
    pub fn fly_gb_read_state(gb: *mut FlyGb, input: *const c_void, size: usize) -> c_int;
}
