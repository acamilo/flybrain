# ROM policy

**There is no game in this repository, and there never has been.**

This project simulates a fruit-fly brain and lets it press buttons on a Game Boy emulator. To watch
it play a commercial game you need that game's cartridge image, and you have to supply it yourself.
Nothing here will help you find one.

## What that means in practice

- **No ROM is committed.** `.gitignore` excludes `*.gb`, `*.gbc`, `*.rom`, `*.sav` and `*.state`, so
  a cartridge image or a save cannot be added by accident.
- **No ROM is copied into the working tree**, not even temporarily, and not into a fixture, a test
  resource or a checkpoint. The emulator reads the file from a path outside the repo and nothing
  writes it back in.
- **No ROM is linked.** You will not find a download link, a torrent, a "known-good dump" name, a
  mirror or a hint about where to look, in the code, the docs, the commit history or the issues.
  Requests for one will be closed.
- **No ROM appears on stream.** The game's video is on screen because the fly is playing it; the
  file is not offered, served or made downloadable from the broadcast, and the stream page exposes
  no path to it.
- **No game assets are vendored.** No sprites, tiles, palettes, music, text or disassembly source.

What the repository does contain, none of which is game content:

- **One SHA-256 digest** of the supported cartridge. A hash is a fingerprint, not the file: it lets
  the service refuse to run against something other than the build the reward rules were written
  for. Semantic rewards are enabled for exactly that one digest; any other cartridge boots and
  plays, pays nothing, and says `UNSUPPORTED ROM . SEMANTIC REWARDS OFF` on screen.
- **Audited RAM addresses and symbol names**, generated from the public pret/pokered disassembly at
  a pinned commit, in `services/flysim/crates/flybrain-gb/src/pokemon_red/symbols.rs`. These are
  numbers and names that describe where the running game keeps its progress flags. The disassembly
  checkout they came from lives outside this repository.
- **The emulator core**: [binjgb](https://github.com/binji/binjgb), MIT-licensed, vendored as seven
  unmodified upstream C files under `services/flysim/vendor/binjgb/` with its licence and a
  provenance note.

## How the release box gets a ROM

Not from git. The deployment takes two values from an environment file that lives outside this
repository, on the machine doing the deploy:

- a **path** to the cartridge, which the service opens read-only, and
- a **SHA-256 pin** for that file.

The service hashes the file it opened and compares it against the pin. A mismatch is a startup
failure (`FLY_ROM_SHA256 does not match the cartridge on disk`), not a warning: it will not quietly
play the wrong build. The file is staged into the container as `/srv/fly/rom/<sha256>.gb`, mode
`0400`, owned by the service user, and it stays there — never on the hypervisor, never in a backup
of this repository, never in a log or a journal line.

If the pin is empty, the deploy writes no ROM path at all and the service runs without one.

## Tests that need a cartridge

They are gated on an environment variable and **skip cleanly when it is unset** — they do not fail,
and they do not need to be excluded from a test run. The variables are `FLY_ROM` for the Pokémon Red
adapter and `FLY_ROM_PLATFORMER` for the Super Mario Land one. With neither set, each such test
prints `skipped: FLY_ROM is not set` and returns; the ROM-driven example binaries print instructions
instead of running.

```sh
cd services/flysim && cargo test --workspace        # ROM-gated tests skip, everything else runs
FLY_ROM=/path/to/your/cartridge.gb cargo test --workspace   # ROM-gated tests run too
```

So `cargo test --workspace` is a complete, honest green run without a cartridge anywhere in sight.
That is the default, and it is what CI-equivalent checks are expected to do.

## Running everything else with no ROM at all

Almost the whole system is exercisable without a game. The emulator is the only part that needs one.

**The fake sim.** A stand-in for the real service that speaks the same feed protocol and control API
over the same ports, so the page, the bridge and any client can be developed and tested against it.
No brain, no emulator, no cartridge.

```sh
npx tsx packages/feed/src/fake/server.ts --scenario running   # serves :7400 feed, :7401 control
```

**Recorded fixtures.** Four `.flyfeed` recordings ship in `apps/stage/public/fixtures/`. The stage
page replays them frame by frame, which is how its layout, motion, tabs and screenshot baselines are
developed and reviewed.

```sh
cd apps/stage && npm run dev
# then open ?mode=player&fixture=steady   (also cold-open, big-moment, macros)
# ?tab=senses|connectome|ladder   ?fly=webgl|paper|off
```

**The TypeScript oracle tests.** `packages/brain` is the reference implementation of the neural
core, and its suite includes bit-exact comparisons against verbatim copies of the original prototype
modules, plus a run on the real connectome dataset — which *is* in the repository, under
`data/fafb-v783`. No cartridge involved.

```sh
npm ci && npm test && npm run typecheck
```

**The Rust golden tests.** The Rust port is checked against golden files generated from the
TypeScript oracle (`services/flysim/golden/*.flygold`, produced by `packages/brain/tools/golden.ts`).
They cover the LIF kernel, the maths, plasticity, the decoder, the agent loop, checkpoint restore,
the version strings and the platformer preset, and they assert 0-ulp agreement. They are part of
`cargo test --workspace` and none of them needs a ROM.

**The infra suite and the stage end-to-end suite.**

```sh
bash infra/tests/lint.sh                        # shell scripts and systemd units
cd apps/stage && npm run test:e2e && npm run mockups   # Playwright baselines and the review PNGs
```

If you want to see the fly actually play a commercial game, supply your own legally obtained
cartridge image and point `FLY_ROM` at it. That is the whole extent of the help available here.
