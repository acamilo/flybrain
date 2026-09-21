# flybrain-gb

The Game Boy half of `flysim`: the binjgb emulator core linked natively, plus
the game adapters that turn WRAM into reward and progress. It knows nothing
about neurons, and does not depend on `flybrain-core`.

```text
ROM bytes -> Emulator::run_frame -> RGBA frame + PCM + WRAM
                                      |
                           GameAdapter::sample -> RewardEvent
                                      |
                           Ratchet::observe -> recover_game
```

| Module | What it is |
| --- | --- |
| `emulator` | Safe wrapper over binjgb: frames, framebuffer, buttons, WRAM, audio, save states, ROM hash |
| `adapter` | `GameAdapter`, `RewardEvent`, `ProgressSnapshot`, `MemoryReader`, `adapter_for` |
| `pokemon_red` | The `pokered-unique8-v5` reward adapter, its catalog and its generated symbol table |
| `platformer` | The `sml-progress-v1` Super Mario Land adapter, its catalog and its RAM map |
| `ratchet` | The progress ratchet, generic over the adapter's rank and its `RecoveryPolicy` |
| `recovery` | Rolling the game back to the ratchet's best safe snapshot |
| `compatibility` | The checkpoint compatibility string |

One `flysim` binary serves both demos, so everything game-specific sits behind
`GameAdapter` and nothing above this crate hardcodes a milestone ladder:
`ProgressSnapshot` carries `rank`, `rank_max` and the adapter's own
`rank_label`. The same rule covers the things the two games disagree about:
`decoder_preset` picks the readout, `boot` says when Start should be permissive,
and `game_over` plus `recovery_policy` give the ratchet its triggers and budgets.
Each of those has a default that is exactly the Pokémon behaviour, so adding a
game does not change an existing one. The one bound that is *not* a per-game
setting is the ratchet's rank ceiling: `Ratchet::import` takes the running
adapter's `rank_ladder().len()`, 38 for Pokémon and 16 for the platformer, so a
checkpoint can never carry a rank the live ladder has no label for.

Two ROM pins, two mechanisms. Pokémon Red's is the constant
`pokemon_red::SUPPORTED_ROM`; the platformer's arrives from configuration through
`adapter_for_with_rom_pin`, because `docs/design/platformer.md` §8.5 records only
the SHA-1 of the revision its RAM map describes. An adapter with no usable pin
reports `SEMANTIC REWARDS OFF` and pays nothing, which is also what a wrong
cartridge does.

## What is vendored

`services/flysim/vendor/binjgb` holds the sources of binjgb's own `binjgb`
library target at revision `c60e138da5a795ebb55e56b11b7e90024e41112c`
(<https://github.com/binji/binjgb>, MIT). `PROVENANCE.md` there records exactly
which files were taken and why, and they are unmodified upstream:

    emulator.c/h  common.c/h  memory.c/h  joypad.c/h  builtin-palettes.def

No host, SDL, OpenGL, ImGui, debugger, tester, options or Emscripten source is
vendored. `rewind.c` is not either: only binjgb's Emscripten wrapper needed it,
and nothing here rewinds. `build.rs` compiles `emulator.c`, `common.c`,
`memory.c` and `joypad.c` with the `cc` crate and no `-D` defines, which is the
configuration the prototype's `binjgb.js` was built in (`RGBDS_LIVE` and
`GBSTUDIO` default to `OFF` and the `js`/`wasm` Makefile targets pass neither).
One consequence: `EMULATOR_EVENT_BREAKPOINT` can never fire, because
`emulator_set_breakpoint` is not compiled in. `run_frame` still checks for it,
as the prototype did.

`csrc/shim.c` replaces binjgb's `src/emscripten/wrapper.c`. It mirrors only the
entry points the prototype's `src/emulator/binjgb.ts` called, and differs from
`wrapper.c` deliberately:

- every function takes an explicit handle, where `wrapper.c` keeps a file-static
  `Emulator*` and static joypad and rewind globals, so `Emulator` is `Send`;
- joypad state is applied directly with `emulator_set_joypad_buttons` and no
  joypad callback is installed, matching the prototype's own patch to
  `wrapper.c`;
- the handle hands binjgb a heap copy of the ROM and never frees it.
  `set_rom_file_data` stores the `FileData` by value and `emulator_delete` frees
  it through `file_data_delete`, so freeing it in the shim too is a double free.
  The prototype leaked its WASM-heap ROM allocation, which hid this.

FFI declarations in `src/ffi.rs` are hand-written; there is no `bindgen`
dependency. `emulator_read_mem` and `emulator_get_wram_ptr` are declared in the
shim, because at this revision `emulator.c` defines them and no header declares
them.

### Known quirk

binjgb's `init_emulator` calls `log_cart_info`, which unconditionally prints the
cartridge title, type and header checksum to stdout on every `Emulator::new`.
Silencing it would mean patching a vendored source, so `flysim` should redirect
its own stdout if the noise matters.

## Regenerating the symbol table

`src/pokemon_red/symbols.rs` is generated. The prototype owns the extraction
from the pret/pokered disassembly and is read-only to us, so
`services/flysim/tools/gen_symbols.py` re-emits the prototype's own generated
`src/reward/symbols.ts` as Rust rather than re-deriving anything:

```sh
uv run python3 services/flysim/tools/gen_symbols.py
```

(`uv run python3`, not plain `python3`, which is blocked on this box. Pass
`--prototype <path>` for a checkout somewhere other than `~/fly-plays-pokemon`.)

That yields the same 24 addresses, 368 selected event flags, 16 story
milestones and pokered commit `0cd19d3b877b7dc66d12c7050bed9a7f38154d4b`.
`EVENTS` is emitted as an **ordered slice, not a map**: `pokemon-red.ts`
iterates `Object.entries(EVENTS)`, so that order decides which of several
simultaneous payouts is emitted first, which is visible in the recent-event
ticker. Commit the script and its output together.

## Audio format

binjgb's native buffer is **unsigned 8-bit, 2 channels, interleaved (left,
right), at the configured frequency** — `AudioBuffer` in `emulator.h` and
`write_audio_frame` in `emulator.c`. `Emulator::new` takes that frequency;
`DEFAULT_AUDIO_FREQUENCY` is 48,000 Hz, matching Web Audio, which is 803.6
stereo frames (1,607.2 interleaved samples) per Game Boy frame.

**The samples are unipolar: silence is 0, not mid-scale.** binjgb's mixer sums
each channel's unsigned `0..15` sample, scales by the master volume and divides
by the channel count, so nothing offsets the result to the middle of the range.
Measured over a 600-frame Pokémon Red boot: raw range `0..=44`, mean 1.2.

`take_audio()` therefore uses binjgb's own host conversion, `sample / 255`
(`AUDIO_CONVERT_SAMPLE_FROM_U8` in `host.c`), giving interleaved stereo `f32`
in `[0, 1]` with silence at exactly `0.0` and no clipping. `take_audio_u8()`
returns the bytes untouched. `take_audio_bipolar()` rescales to fill `[-1, 1]`
by treating mid-scale as zero, which is louder but sits at -1.0 during silence
and needs a high-pass filter downstream. The residual DC while the APU plays is
binjgb's mixer, not the conversion.

`docs/feed-protocol.md` in this repository currently says 44,100 Hz; the flysim
design and the stage page want 48,000. The rate is a constructor parameter, so
whichever the feed settles on is one argument, not a code change.

### Draining

`emulator_run_until` resets the audio write cursor only when the *previous*
event carried `EMULATOR_EVENT_AUDIO_BUFFER_FULL`, so a caller that wants
per-frame audio has to drain explicitly or silently lose blocks. The prototype
never drained at all. `run_frame` drains on `AUDIO_BUFFER_FULL` and again at the
frame boundary, and accumulates into one buffer that `take_audio*` empties, so
audio is continuous across frames. The ROM test asserts the sample count matches
the emulated tick count to within 0.1%.

Note that a `run_frame` during the boot interval, where the LCD is off, consumes
several frame periods (the 120-attempt retry loop), so it produces several
frames' worth of audio. Expected sample counts must come from the tick delta,
not from the number of `run_frame` calls.

## State format

`export_state` returns exactly what binjgb's `emulator_write_state` emits, which
is what the prototype's checkpoints embedded. Three caveats:

1. **It is a `memcpy` of `EmulatorState`, so the bytes are ABI dependent.**
   Measured: 199,616 bytes on `x86_64-unknown-linux-gnu`, against 199,608 in the
   prototype's Emscripten build. A prototype milestone save therefore does
   **not** import here, and the probe test in `tests/rom.rs` confirms it:
   `Game Boy state size mismatch: 199608 != 199616`. `compatibility.rs` appends
   a `statefmt:<size>-<target triple>` segment to the prototype's compatibility
   string so this is an honest mismatch instead of a silent misparse. There is
   no migration; the fly starts its native run from a cold boot.
2. **The framebuffer is not part of `EmulatorState`.** After `import_state` the
   previous image is still on screen until the next `run_frame`. That is why
   `ratchet::Snapshot` carries the framebuffer alongside the state, and why the
   prototype's checkpoint had a separate `framebuffer` chunk.
3. **The audio resampler's phase is not part of it either.**
   `AudioBuffer.freq_counter` and `.divisor` live outside `EmulatorState`, so
   audio after a restore is not bit-identical to the same run without one. The
   framebuffer is; the ROM test asserts that and only checks the audio block
   length.

## Tests

```sh
cargo test                       # unit tests; ROM tests skip
cargo clippy --all-targets
FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
  FLY_CHECKPOINT="$HOME/fly-plays-pokemon/local/saves/milestone-3.checkpoint" \
  cargo test --release --test rom -- --nocapture
```

The reward and ratchet unit tests are ports of the prototype's
`tests/unit/reward.test.ts` and `tests/unit/ratchet.test.ts`, using the same
synthetic WRAM traces; `FakeMemory` implements the same read interface the
emulator's frame cache does.

The ROM tests are gated on `FLY_ROM` and skip cleanly without it. The cartridge
never enters this repository: the repo `.gitignore` excludes `*.gb`, `*.gbc`,
`*.rom`, `*.sav` and `*.state`. `FLY_CHECKPOINT` is optional and only drives the
save-state probe above.

Measured on the development laptop (WSL2, release build): **about 5,650
emulator frames per second, 95x real time at 59.7275 fps**, with audio drained
every frame and no neural work. The boot walkthrough test reaches `OVERWORLD`
in Red's bedroom at about frame 3,835 by alternating Start and A, paying exactly
one reward, `ADVENTURE STARTED`.
