/*
 * flybrain-gb C shim over the vendored binjgb emulator core.
 *
 * This replaces binjgb's own src/emscripten/wrapper.c. It mirrors only the
 * entry points the TypeScript prototype's src/emulator/binjgb.ts actually
 * used, and deliberately differs from wrapper.c in three ways:
 *
 *   1. wrapper.c keeps a file-static `Emulator* e` plus static EmulatorInit,
 *      EmulatorConfig, JoypadButtons and RewindState globals. Every function
 *      here takes an explicit handle instead, so two emulators can coexist and
 *      the Rust wrapper can be Send.
 *   2. binjgb keeps a pointer to the ROM bytes rather than copying them
 *      (set_rom_file_data stores the FileData by value) and frees them in
 *      emulator_delete, so the shim hands over a heap copy of the ROM and
 *      never frees it itself. The prototype leaked its ROM allocation in the
 *      WASM heap, which hid this ownership transfer.
 *   3. No rewind buffer and no joypad callback: joypad state is applied
 *      directly with emulator_set_joypad_buttons, as the prototype's locally
 *      patched wrapper did.
 */
#include <stdlib.h>
#include <string.h>

#include "emulator.h"

/*
 * emulator.c defines these but no binjgb header declares them: at revision
 * c60e138 they exist only for the Emscripten export list, which relied on an
 * implicit declaration. Declared here so the shim compiles warning-clean.
 */
u8 emulator_read_mem(Emulator* e, u16 addr);
u8* emulator_get_wram_ptr(Emulator* e);

/* Bit order of the button mask, matching the prototype's BUTTON_BITS in
 * src/protocol.ts: up, down, left, right, a, b, start, select. */
#define FLY_GB_UP     (1u << 0)
#define FLY_GB_DOWN   (1u << 1)
#define FLY_GB_LEFT   (1u << 2)
#define FLY_GB_RIGHT  (1u << 3)
#define FLY_GB_A      (1u << 4)
#define FLY_GB_B      (1u << 5)
#define FLY_GB_START  (1u << 6)
#define FLY_GB_SELECT (1u << 7)

typedef struct FlyGb {
  Emulator* e;
  JoypadButtons buttons;
} FlyGb;

size_t fly_gb_state_size(void) { return s_emulator_state_size; }

size_t fly_gb_frame_buffer_size(void) { return sizeof(FrameBuffer); }

FlyGb* fly_gb_new(const void* rom_data, size_t rom_size, u32 audio_frequency,
                  u32 audio_frames) {
  if (rom_data == NULL || rom_size == 0) return NULL;

  /* binjgb requires a multiple of MINIMUM_ROM_SIZE (32 KiB); the prototype
   * zero-padded up to the next multiple before handing the bytes over. */
  size_t padded = (rom_size + (MINIMUM_ROM_SIZE - 1)) & ~(MINIMUM_ROM_SIZE - 1);
  if (padded > MAXIMUM_ROM_SIZE) return NULL;

  FlyGb* gb = calloc(1, sizeof(FlyGb));
  if (gb == NULL) return NULL;
  /* Zero-padded so the header checks and bank arithmetic see a full bank, as
   * the prototype's `(len + 0x7fff) & ~0x7fff` padding did. */
  u8* rom = calloc(1, padded);
  if (rom == NULL) {
    free(gb);
    return NULL;
  }
  memcpy(rom, rom_data, rom_size);

  EmulatorInit init;
  memset(&init, 0, sizeof(init));
  init.rom.data = rom;
  init.rom.size = padded;
  init.audio_frequency = (int)audio_frequency;
  init.audio_frames = (int)audio_frames;
  /* Same fixed seed and colour curve the prototype passed to
   * _emulator_new_simple(rom, size, 48000, 4096, 2). */
  init.random_seed = 0xcabba6e5;
  init.cgb_color_curve = CGB_COLOR_CURVE_GAMBATTE;

  gb->e = emulator_new(&init);
  if (gb->e == NULL) {
    /* emulator_new's error path runs emulator_delete, which frees the ROM
     * through file_data_delete. The only way it can fail before taking
     * ownership is set_rom_file_data, and the size checks above already
     * guarantee that call succeeds, so freeing `rom` here would double free. */
    free(gb);
    return NULL;
  }
  return gb;
}

void fly_gb_delete(FlyGb* gb) {
  if (gb == NULL) return;
  /* Frees the ROM copy too; see fly_gb_new. */
  emulator_delete(gb->e);
  free(gb);
}

u64 fly_gb_get_ticks(FlyGb* gb) { return (u64)emulator_get_ticks(gb->e); }

u32 fly_gb_run_until(FlyGb* gb, u64 until_ticks) {
  return (u32)emulator_run_until(gb->e, (Ticks)until_ticks);
}

const void* fly_gb_frame_buffer_ptr(FlyGb* gb) {
  return (const void*)*emulator_get_frame_buffer(gb->e);
}

void fly_gb_set_buttons(FlyGb* gb, u32 mask) {
  gb->buttons.up = (mask & FLY_GB_UP) ? TRUE : FALSE;
  gb->buttons.down = (mask & FLY_GB_DOWN) ? TRUE : FALSE;
  gb->buttons.left = (mask & FLY_GB_LEFT) ? TRUE : FALSE;
  gb->buttons.right = (mask & FLY_GB_RIGHT) ? TRUE : FALSE;
  gb->buttons.A = (mask & FLY_GB_A) ? TRUE : FALSE;
  gb->buttons.B = (mask & FLY_GB_B) ? TRUE : FALSE;
  gb->buttons.start = (mask & FLY_GB_START) ? TRUE : FALSE;
  gb->buttons.select = (mask & FLY_GB_SELECT) ? TRUE : FALSE;
  emulator_set_joypad_buttons(gb->e, &gb->buttons);
}

u8 fly_gb_read_mem(FlyGb* gb, u16 address) {
  return emulator_read_mem(gb->e, address);
}

u32 fly_gb_audio_frequency(FlyGb* gb) {
  return emulator_get_audio_buffer(gb->e)->frequency;
}

/* Number of bytes currently queued: interleaved unsigned 8-bit stereo frames,
 * so this is 2 * audio_buffer_get_frames(). */
size_t fly_gb_audio_size(FlyGb* gb) {
  AudioBuffer* buffer = emulator_get_audio_buffer(gb->e);
  return (size_t)(buffer->position - buffer->data);
}

const void* fly_gb_audio_ptr(FlyGb* gb) {
  return (const void*)emulator_get_audio_buffer(gb->e)->data;
}

/*
 * Rewind the write cursor so the next emulator_run_until starts a fresh block.
 * emulator_run_until only does this itself when the previous event carried
 * EMULATOR_EVENT_AUDIO_BUFFER_FULL, so a caller that wants per-frame audio has
 * to drain explicitly. This is exactly the assignment run_until performs.
 */
void fly_gb_audio_reset(FlyGb* gb) {
  AudioBuffer* buffer = emulator_get_audio_buffer(gb->e);
  buffer->position = buffer->data;
}

/* Byte-for-byte what binjgb's own emulator_write_state emits: a copy of
 * EmulatorState. `out` must be fly_gb_state_size() bytes. */
int fly_gb_write_state(FlyGb* gb, void* out, size_t size) {
  FileData file_data;
  if (size != s_emulator_state_size) return -1;
  file_data.data = (u8*)out;
  file_data.size = size;
  return emulator_write_state(gb->e, &file_data) == OK ? 0 : -1;
}

int fly_gb_read_state(FlyGb* gb, const void* in, size_t size) {
  FileData file_data;
  if (size != s_emulator_state_size) return -1;
  /* emulator_read_state takes a const FileData* and does not write through
   * file_data.data, so casting away const here is sound. */
  file_data.data = (u8*)in;
  file_data.size = size;
  return emulator_read_state(gb->e, &file_data) == OK ? 0 : -1;
}
