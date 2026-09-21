# binjgb (vendored)

- Upstream: https://github.com/binji/binjgb
- Revision: `c60e138da5a795ebb55e56b11b7e90024e41112c`
- License: MIT, see `LICENSE`.
- Copied from the `fly-plays-pokemon` prototype's checkout of that revision
  (`.tools/binjgb`), which is the same revision pinned in the prototype's
  compatibility string.

## What is here

Only the sources the `binjgb` emulator core needs. `CMakeLists.txt` at that
revision builds the Emscripten target from

    src/memory.c src/common.c src/emulator.c src/joypad.c src/rewind.c
    src/emscripten/wrapper.c

Of those, `joypad.c` and `rewind.c` are needed only by `wrapper.c`'s rewind and
joypad-replay features; `emulator.c` itself references neither. `flybrain-gb`
does not use rewind, so the vendored set is:

    src/emulator.c  src/emulator.h
    src/common.c    src/common.h
    src/memory.c    src/memory.h
    src/builtin-palettes.def   (#include'd by emulator.c)

Nothing here is modified from upstream. No host, SDL, OpenGL, ImGui, debugger,
tester, options or Emscripten source is vendored.

## What replaces `src/emscripten/wrapper.c`

`crates/flybrain-gb/csrc/shim.c` in this repository. It mirrors only the
wrapper functions the prototype's `src/emulator/binjgb.ts` actually called,
with no Emscripten dependency, no static emulator singleton, and no rewind or
joypad buffer. The prototype's wrapper was itself locally patched to apply
joypad state directly rather than through an accumulating rewind buffer; the
shim does the same by calling `emulator_set_joypad_buttons` and installing no
joypad callback.

`emulator_read_mem` and `emulator_get_wram_ptr` are defined in `emulator.c` but
declared in no header at this revision: the Emscripten build reached them
through its export list and an implicit declaration. The shim declares them
itself so it compiles warning-clean.

## Build defines

None. `CMakeLists.txt` declares `RGBDS_LIVE` and `GBSTUDIO` as options that
default to `OFF`, and the `js`/`wasm` Makefile targets that produced the
prototype's `binjgb.js` pass neither. The build therefore compiles the same
configuration the prototype ran, which means:

- `emulator_set_breakpoint`, `emulator_clear_breakpoints`,
  `emulator_get_banked_PC` and `emulator_render_vram` are not compiled in;
- `EMULATOR_EVENT_BREAKPOINT` can never be raised. `Emulator::run_frame` still
  checks for it, as the prototype did.

`build.rs` defines no `NDEBUG` of its own. The `cc` crate defines it for
profiles with debug assertions off, so `assert(buffer->position <=
buffer->end)` in `write_audio_frame` stays live in debug builds, where a
misconfigured `audio_frames` would otherwise corrupt memory quietly.
`-fno-strict-aliasing` is set for binjgb's type-punning paths.
`-Wno-unused-parameter`, `-Wno-unused-function`,
`-Wno-unused-variable` and `-Wno-implicit-fallthrough` mirror the warning flags
`CMakeLists.txt` sets for non-MSVC compilers.
