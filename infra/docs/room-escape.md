# Room escape: choosing the direction hold

Measured 2026-09-16 for `docs/design/room-escape.md` section 1, which asked for a hold value
between 600 and 1000 ms chosen by measurement rather than by argument.

**Superseded in part by the v0.1.1 section below**, which reversed the `fatigueGain` half of this
result and lowered the hysteresis: the random walker used here cannot build an incumbency, so it
cannot see what a low fatigue gain does to a real network. The 800 ms hold survived.

**Chosen: `holdMs` = `decisionMs` = 800, `fatigueGain` = 0.04.** Baked into
`packages/brain/src/readout/presets/gameboy.ts` and its Rust twin
`flybrain-core::decoder::gameboy`, noted in `docs/readout.md`.

## What was run

`services/flysim/crates/flysim/examples/room_escape.rs`, on the pinned Pokémon Red cartridge
(SHA-256 `0e85…219f`), unthrottled, 15 worker threads on the WSL development box. The real
`PopulationDecoder` with the real Game Boy preset, the real `PokemonRedReward` adapter, and seeded
uniform random rates in place of a network — the `random_walker.rs` pattern, which is how the
readout gets measured without 139,255 neurons of confound.

Two starting rooms, both reached by the harness itself rather than from an archive:

- **Red's bedroom, map 38 (`REDS_HOUSE_2F`).** Booted from a cold cartridge: Start and A on a slow
  alternation through the intro and the naming screens, then B until the adapter reports a stable,
  unscripted, dialogue-free overworld sample. One warp, the stairs at (7, 1); the fly spawns at
  (3, 6) with (3, 5) blocked.
- **The ground floor, map 37 (`REDS_HOUSE_1F`).** One run walked down the stairs and its state was
  cached. Captured 15 brain seconds *after* arrival, not on arrival: the first safe sample after a
  warp is standing on the staircase, and "leave the map" from the staircase is one step back up.
  Three warps — the two front-door tiles at (2, 7) and (3, 7), and the stairs at (7, 1) — and a
  table between them.

Each cell is 60 seeded runs of 10 brain minutes, run to the full budget rather than stopped at the
first map change, so the coverage column is uncensored.

## The design's twenty seeds

Twenty is what the design asked for. It is not enough to choose on: **every candidate leaves both
rooms in every one of twenty runs**, so the leave fraction saturates at 20/20 everywhere, and the
median time to leave moved by more between neighbouring hold values than it did across the whole
range (bedroom, fatigue 0.04: 38.1 s at 400, 26.8 s at 600, 29.2 s at 800, 43.7 s at 1000 — not
monotone in either direction). The tables below are therefore 60 seeds, of which those twenty are
the first.

## From Red's bedroom, map 38

60 seeded runs per row.

| hold = decision | fatigue gain | left the map | median time to leave | mean time to leave | median tiles | mean tiles | median maps |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 400 ms | 0.04 | 60/60 | 34.3 s | 52.4 s | 173.0 | 168.9 | 4.0 |
| 400 ms | 0.08 | 60/60 | 37.5 s | 56.6 s | 179.0 | 173.0 | 3.0 |
| 600 ms | 0.04 | 60/60 | 31.3 s | 39.6 s | 174.5 | 173.2 | 4.0 |
| 600 ms | 0.08 | 60/60 | 28.7 s | 36.0 s | 174.0 | 178.9 | 4.0 |
| **800 ms** | **0.04** | **60/60** | **22.6 s** | **27.0 s** | **188.0** | **181.6** | **4.0** |
| 800 ms | 0.08 | 60/60 | 23.8 s | 31.6 s | 172.5 | 171.1 | 4.0 |
| 1000 ms | 0.04 | 60/60 | 30.2 s | 35.9 s | 174.0 | 173.8 | 4.0 |
| 1000 ms | 0.08 | 60/60 | 24.0 s | 31.0 s | 182.0 | 174.3 | 4.0 |

Maps reached across all 480 runs in this table: Pallet Town (0) in 470, the ground floor (37) in
480, the bedroom again (38) in 384, Oak's Lab (40) in 323 and Blue's house (39) in 73. No run
reached route 1 (12) in either table: the game blocks the north exit from Pallet Town until the
player has a starter, so a walker with no plan circles the four maps it can open.

## From the ground floor, map 37

60 seeded runs per row.

| hold = decision | fatigue gain | left the map | median time to leave | mean time to leave | median tiles | mean tiles | median maps |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 400 ms | 0.04 | 60/60 | 17.3 s | 24.6 s | 158.0 | 151.6 | 3.0 |
| 400 ms | 0.08 | 60/60 | 16.2 s | 19.6 s | 159.5 | 152.7 | 3.0 |
| 600 ms | 0.04 | 60/60 | 15.0 s | 21.2 s | 166.0 | 162.5 | 3.0 |
| 600 ms | 0.08 | 60/60 | 19.4 s | 24.0 s | 148.0 | 152.7 | 3.0 |
| **800 ms** | **0.04** | **60/60** | **20.0 s** | **28.5 s** | **171.0** | **164.7** | **3.0** |
| 800 ms | 0.08 | 60/60 | 18.9 s | 25.8 s | 162.5 | 163.8 | 3.0 |
| 1000 ms | 0.04 | 60/60 | 24.1 s | 28.1 s | 165.0 | 162.6 | 3.0 |
| 1000 ms | 0.08 | 60/60 | 19.6 s | 25.9 s | 178.0 | 171.7 | 4.0 |

Maps reached across all 480 runs in this table: Pallet Town (0) in 480, the ground floor again (37)
in 369, the bedroom (38) in 275, Oak's Lab (40) in 350 and Blue's house (39) in 91.

## Confirmation with the real brain

`FLY_ESCAPE_BRAIN=data/fafb-v783`: `NeuralAgent` on the FlyWire connectome, 139,255 neurons, four
sweep threads, warm-up included, the adapter's payouts reinforcing as they do on the stream, from
the same bedroom state. Five runs each, and the runs differ only in warm-up length — the network
and binjgb are both deterministic, so five runs from one state with one warm-up are one run
reported five times. (That was the first attempt: all five left the bedroom at the same
millisecond.)

| hold / fatigue | run 1 | run 2 | run 3 | run 4 | run 5 | left | median | mean |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 400 ms / 0.08 (before) | 38.3 s | 22.7 s | 129.1 s | 52.5 s | 294.2 s | 5/5 | 52.5 s | 107.4 s |
| **800 ms / 0.04 (chosen)** | 18.6 s | 28.6 s | 68.3 s | 30.3 s | 68.8 s | **5/5** | **30.3 s** | **42.9 s** |

The brain is slower than the random walker at both settings and moves the same way between them:
the median more than halves and the mean falls by 60%. So the choice is not a random-walker
artefact. Every run left through the stairs to map 37; none of the ten went anywhere else first.

## Why 800 and 0.04 (the 0.04 half was wrong)

- **Leave fraction cannot decide it.** Every candidate escapes both rooms in every run at 60 seeds,
  which is worth recording as a result in itself: the design's premise, that the fly cannot get out
  of a room, is not reproducible by a random walker in these two rooms at *any* of the four hold
  values. What the old setting cost was time, not success.
- **800 ms is the best row in the bedroom on every column.** Fastest median (22.6 s), fastest mean
  (27.0 s), most ground covered (188 median tiles). The bedroom is the tight room — 8x8 tiles with
  furniture and one exit — and it is the case the design was written about.
- **1000 ms is worse, and consistently.** Slower than 800 in both rooms at both fatigue gains. A
  commitment long enough to cross the whole room means running into a wall and staying there until
  the next decision.
- **0.04 beats 0.08 at 800 ms**, on the bedroom mean (27.0 s against 31.6 s) and on coverage in
  both rooms. Elsewhere in the table the two fatigue gains are inside the seed-to-seed spread, so
  this is a weak result on its own; it agrees with the mechanism the design gives, which is that a
  lower gain is what lets a committed direction survive a re-decision, and it is the value the
  design proposed. **This was the wrong call, and the weakness of the evidence was the warning.**
  A uniform random source re-rolls a fresh direction at every decision, so it never builds the long
  incumbency the gain governs; on a real network at 0.04 one direction won 4.1 consecutive holds,
  twelve tiles of commitment in an eight-tile room. See the v0.1.1 section.
- **The ground floor costs a few seconds.** 800/0.04 is 20.0 s against 15.0 s for the best row
  (600/0.04), inside the spread of that room's eight rows (15.0 to 24.1 s) and paid back in
  coverage (171 against 166 median tiles). The ground floor has three warps including two adjacent
  front-door tiles, so it is easy at every setting and is the weaker of the two signals.

## Honesty

This measures the readout. A random walker is not a brain, and a configuration that leaves a room
faster is not learning anything — the whole reason the walker is the instrument here is that it
removes the brain from the loop so the readout can be seen on its own. The five-run brain
confirmation is five runs, on one cartridge, from one room; it says the number is not an artefact
of the random source, and it is not a benchmark of the fly.

## Reproducing

```sh
FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
FLY_ESCAPE_SEEDS=60 FLY_ESCAPE_JOBS=15 \
  cargo run --release -p flysim --example room_escape
```

About 17 minutes of wall time for the 960 runs. Then, for the confirmation rows:

```sh
FLY_ROM=... FLY_ESCAPE_SEEDS=1 FLY_ESCAPE_HOLDS=800 FLY_ESCAPE_FATIGUES=0.04 \
FLY_ESCAPE_BRAIN=data/fafb-v783 FLY_ESCAPE_HOLD=800 FLY_ESCAPE_FATIGUE=0.04 \
  cargo run --release -p flysim --example room_escape
```

binjgb prints the cartridge header to stdout on every boot, so the report is easier to read
through `grep -v`. The ROM is read from the path in `FLY_ROM` and is never copied into the
repository.

# v0.1.1: why the live fly stayed on the ground floor

Measured 2026-09-16 for `docs/design/room-escape.md` section 3, after the release box (the release container,
`twitch.tv/<twitch-channel>`, v0.1.0) spent forty minutes on rung 2.

**Chosen: `hysteresis` 1.15 -> 1.05, `fatigueGain` 0.04 -> 0.08, and the blocked-direction cooldown
on at `blockedFatigue` 0.35 / `blockedMs` 800.** `holdMs` and `decisionMs` stay at 800. Baked into
`packages/brain/src/readout/presets/gameboy.ts` and its Rust twin
`flybrain-core::decoder::gameboy`, dated in `docs/readout.md`.

## The state this was measured from

Generation 525 of the release run, `pct pull`ed read-only off the release container and never written back:
SHA-256 `a899997a36ab1706…fd3d49`, 2,667,148 bytes, brain clock 2,589,850 ms (43.2 minutes),
emulator frame 154,537, rank 2 since 95,740 ms, ratchet best 2 with **0 recoveries and 0 attempts**,
`tileCounts` 37:32 and 38:39, all eight `boundary` payouts spent, lifetime reward 3.20.

The fly was standing at map 37 (7, 1) — *on* the staircase tile, which only warps on the step onto
it — holding `up` into the wall above it, with `stable` at 248 samples.

Its four direction scores at that instant:

| channel | rate | baseline | score | fatigue | adjusted |
| --- | ---: | ---: | ---: | ---: | ---: |
| up | 50.744 | 50.661 | 1.0016 | 0.1446 | 0.8751 |
| down | 69.265 | 71.920 | 0.9636 | 0.0000 | 0.9636 |
| left | 62.733 | 59.897 | 1.0466 | 0.0512 | 0.9956 |
| right | 65.500 | 66.633 | 0.9833 | 0.0939 | 0.8988 |

The spread from best to worst is 8.6%, well under the 15% a challenger needed to beat an incumbent:
**no direction could ever win on score alone.** Only fatigue could move the winner, and at gain 0.04
that took a run of four wins — so the readout was a slow four-step rotation through a fixed
preference order, and `down`, the only direction that opens the front door, was last in it.

### Where the preference order comes from

`calibrate()` runs once, at warm-up, and stores each role's rate as its baseline. At that instant
every score is exactly 1 by construction and the four directions are exactly tied. The whole spread
above is **drift of the four `command_*` roles since that calibration**, 43 brain minutes earlier,
frozen into the score. It is not a claim about what the fly wants now; it is the sign of each role's
slow drift, and it holds its shape for as long as the run does. That is why habituation was the only
thing that could reorder the directions, and why `fatigueGain` and `hysteresis` — two numbers
section 1 treated as secondary — turned out to decide whether the fly could leave a room.

## The room

`survey` in the harness walks map 37 with real presses on throwaway emulators. 48 tiles are
reachable and exactly six presses leave the map:

| from | press | to |
| --- | --- | --- |
| (6, 1) | right | 38, the bedroom |
| (7, 2) | up | 38, the bedroom |
| (2, 6) | down | 0, Pallet Town |
| (3, 6) | down | 0, Pallet Town |
| (2, 7) | down | 0, Pallet Town |
| (3, 7) | down | 0, Pallet Town |

The live ledger held 32 of the 48; the 16 it had never stood on were (1,4), (1,5), (1,6), (2,1),
(2,4), (2,5), (2,6), (2,7), (3,6), (3,7), (4,6), (5,5), (5,6), (6,4), (6,5) and (6,6) — the southern
interior, and with it three of the four tiles the front door needs. So "71 unique locations (the
whole floor)" was wrong in two ways: 71 is both floors, and neither floor was covered.

The two doormats are the exception that proves the mechanism. `boundary:37:7:2:on` and
`boundary:37:7:3:on` were both paid, 0.10 each, at 355 s — the fly *did* stand on both mats. It
walked onto them sideways along row 7, which does nothing, and the next direction it committed to
was not `down`. (They are missing from the tile ledger because the main sample gate rejects
`wMovementFlags & 0xc7`, which includes `BIT_STANDING_ON_DOOR`; the `boundary` rule keeps its own
gate at `0xc0` precisely so the on-exit half can fire. Both behaviours are correct and unchanged.)

## How the runs were made independent

Fifteen runs of ten brain minutes per candidate, unthrottled, the real `NeuralAgent` on
`data/fafb-v783` (139,255 neurons), the real `PokemonRedReward` with the checkpoint's own ledger
imported, and the real `Ratchet` with the checkpoint's own budget — one thread each, fifteen at a
time, on the 16-core WSL development box, about nine minutes of wall time per candidate.

A restore has no warm-up length to vary, because `simloop.rs` deliberately skips the warm-up for a
checkpoint that already carries a settled network and a calibrated readout. So the runs differ only
in `LifState::rng`, the xorshift state behind the per-tick noise kicks, displaced by the harness's
own seed list. Everything else — membranes, refractory counters, learned gains, eligibility traces,
the brain clock, the decoder's fatigue, holds and baseline, the emulator, the reward ledger — is the
live fly's to the byte. Fifteen noise streams from one state is the same kind of variation fifteen
warm-ups are, and on a restored run it is the only arbitrary quantity left.

Runs stop when the fly leaves the *house*. Leaving the map is not leaving the house: two of the six
exits are the staircase, and the bedroom is a room the fly has also exhausted.

## The baseline: what v0.1.0 does from here

Fifteen runs, ten brain minutes each, at `holdMs` 800 / `fatigueGain` 0.04 / `hysteresis` 1.15.
Columns after the first four are the diagnosis: the share of decisions each direction won, the mean
length of a same-direction run in decisions, the share of decisions where the fatigue-adjusted
argmax lost to the incumbent under hysteresis, the share of holds over which the position never
changed at all, the longest single motionless stretch, and the decisions taken while standing on a
tile one press from leaving (over how many such decisions there were).

| run | left the map | out of the house | new tiles | decisions | up | down | left | right | mean run | hysteresis | blocked holds | longest still | exit decisions | recoveries | reward |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 28.7 s | no | 0 | 747 | 29% | 17% | 34% | 20% | 4.15 | 72% | 481 (64%) | 23.5 s | 13/25 | 0 | 0.00 |
| 2 | 20.0 s | no | 0 | 747 | 31% | 15% | 33% | 21% | 4.22 | 73% | 460 (62%) | 21.7 s | 14/41 | 0 | 0.00 |
| 3 | 6.0 s | no | 6 | 747 | 30% | 18% | 33% | 20% | 4.10 | 72% | 475 (64%) | 23.6 s | 7/20 | 0 | 0.00 |
| 4 | 6.5 s | no | 0 | 747 | 30% | 14% | 33% | 24% | 4.17 | 73% | 466 (62%) | 8.2 s | 14/14 | 0 | 0.00 |
| 5 | 22.7 s | no | 0 | 747 | 29% | 17% | 32% | 22% | 4.08 | 71% | 462 (62%) | 11.6 s | 10/14 | 0 | 0.00 |
| 6 | 33.5 s | no | 8 | 747 | 30% | 16% | 32% | 22% | 4.13 | 72% | 474 (63%) | 34.5 s | 11/28 | 0 | 0.05 |
| 7 | 13.0 s | no | 0 | 747 | 31% | 16% | 33% | 21% | 4.08 | 72% | 473 (63%) | 27.1 s | 10/27 | 0 | 0.00 |
| 8 | 62.4 s | no | 0 | 747 | 29% | 16% | 32% | 22% | 4.20 | 73% | 472 (63%) | 7.2 s | 8/21 | 0 | 0.00 |
| 9 | 48.1 s | no | 0 | 747 | 29% | 16% | 34% | 22% | 4.10 | 73% | 470 (63%) | 26.5 s | 5/21 | 0 | 0.00 |
| 10 | 13.3 s | no | 4 | 747 | 30% | 15% | 32% | 23% | 4.10 | 73% | 442 (59%) | 11.7 s | 11/39 | 0 | 0.00 |
| 11 | 46.5 s | no | 0 | 747 | 30% | 15% | 32% | 23% | 4.17 | 73% | 483 (65%) | 31.3 s | 9/17 | 0 | 0.00 |
| 12 | 11.8 s | no | 0 | 747 | 30% | 16% | 33% | 21% | 4.17 | 72% | 460 (62%) | 8.0 s | 13/30 | 0 | 0.00 |
| 13 | 11.7 s | no | 0 | 747 | 30% | 15% | 33% | 21% | 4.17 | 74% | 469 (63%) | 9.6 s | 10/27 | 0 | 0.00 |
| 14 | 172.7 s | no | 4 | 747 | 31% | 14% | 32% | 23% | 4.20 | 73% | 452 (61%) | 17.2 s | 9/27 | 0 | 0.00 |
| 15 | 6.9 s | 573.5 s | 5 | 714 | 30% | 16% | 32% | 23% | 4.13 | 73% | 432 (61%) | 9.1 s | 12/23 | 0 | 0.00 |

**1/15 out of the house, 0/15 inside five minutes.** Every run left the *map* within three minutes
and, except for run 15, every one of those exits was the staircase. Ten of the fifteen discovered not
one new tile in ten minutes, and fourteen of the fifteen earned no reward at all. Frames spent on an
exit tile, summed over the fifteen runs: the two staircase tiles 5,524 and 8,710, the two doormats
1,441 and 1,427, and the two tiles above the doormats **68 and 55** — 0.02% of the run.

**59 to 65% of holds moved the player not one tile**, for up to 34.5 seconds at a stretch.

**The mean same-direction run was 4.1 decisions.** At `holdMs` 800 that is 3.3 seconds of committed
travel, and one overworld step is about 268 ms, so a run commits to about **12 tiles** of straight
walking. Red's ground floor is 8 tiles wide. This is the arithmetic section 1 got wrong: it doubled
the hold from 400 to 800 ms, which was right, and halved `fatigueGain` from 0.08 to 0.04 at the same
time, which doubled the run length *in holds* as well — so the committed distance went from about 6
tiles to about 12, and every run began ending in a wall. The random walker could not see it, because
a uniform random source re-rolls a fresh direction at every decision and so builds no incumbency.

**71 to 74% of decisions were settled by hysteresis, not by score**, which is the 8.6% spread
against the 15% margin, measured rather than argued.

## The candidates

Fifteen runs of ten brain minutes each, all from generation 525, all measured the same way.

| candidate | `holdMs` | `fatigueGain` | `hysteresis` | `blockedFatigue` | out of the house | inside 5 min | median | mean run | hysteresis share | blocked holds |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| v0.1.0 | 800 | 0.04 | 1.15 | off | 1/15 | 0/15 | 573.5 s | 4.1 | 71-74% | 59-65% |
| fatigue only | 800 | 0.08 | 1.15 | off | 11/15 | 4/15 | 314.6 s | 2.5 | 57-61% | 25-45% |
| fatigue + cooldown | 800 | 0.08 | 1.15 | 0.35 | 7/15 | 4/15 | 282.7 s | 2.0 | 44-50% | 30-39% |
| no commitment bonus | 800 | 0.08 | 1.00 | off | 8/15 | 3/15 | 330.2 s | 1.0 | 0% | 31-54% |
| shorter hold | 600 | 0.08 | 1.05 | off | 8/15 | 4/15 | 276.7 s | 1.2 | 12-19% | 41-55% |
| hysteresis only | 800 | 0.08 | 1.05 | off | 13/15 | 6/15 | 312.3 s | 1.2 | 12-20% | 30-62% |
| **chosen** | **800** | **0.08** | **1.05** | **0.35** | **14/15** | **13/15** | **130.3 s** | **1.2** | **11-21%** | **27-57%** |

The blocked-direction cooldown alone, at v0.1.0's fatigue and hysteresis (800 / 0.04 / 1.15 / 0.35),
left the house in 0 of 5 runs and is not in the table for that reason.

Three things are worth reading off it.

- **The hysteresis is the lock, and 1.05 is the right amount to unlock.** 1.00, no commitment bonus
  at all, is *worse* than 1.05 on every column (8/15 against 13/15): the hold is a commitment device
  and dropping the last of the margin wastes it. The mean run at 1.00 is exactly 1.0 — the incumbent
  never once survived a decision.
- **The shorter hold is not the answer.** 600 ms at the same fatigue and hysteresis gets 8/15. The
  three-tile commitment an 800 ms hold buys is worth keeping; section 1 was right about that.
- **The cooldown's sign depends on the hysteresis.** At 1.15 it makes things worse (7/15 against
  11/15 without it) because a fly pushed off walls faster orbits the perimeter faster, and the
  staircase is on the perimeter. At 1.05 the fly turns inward instead and the cooldown stops each
  crossing being spent on a wall. Time on the two staircase tiles over the fifteen runs: 16,268
  frames at 1.15 with the cooldown, 3,642 at 1.05 with it. Time on the two tiles above the doormats:
  55 and 68 frames under v0.1.0, 482 and 367 under the chosen config.

The chosen config also gets the fly paid again — 0.05 to 0.10 per run against v0.1.0's 0.00 — and
back to discovering 5 to 19 new tiles per run against 0.

## From a fresh boot

Five runs each, real brain, cold warm-up, the same harness without `FLY_ESCAPE_CHECKPOINT`.

| room | out of the house | inside 5 min | median | left the starting map |
| --- | ---: | ---: | ---: | ---: |
| ground floor (map 37) | 5/5 | 4/5 | 53.2 s | 5/5, median 16.6 s |
| bedroom (map 38) | 5/5 | 4/5 | 83.8 s | 5/5, median 20.3 s |

The bedroom row is the check against section 1's confirmation table, which measured leaving the
bedroom and nothing else: 5/5 at a median 30.3 s under v0.1.0, 5/5 at a median 20.3 s here. No
regression, and the fly now also leaves the building. A fresh fly earns 1.75 to 2.30 of reward in
these runs, against the live fly's 0.00, because a cold ledger still has the operator's payouts in it.

## The random walker, for scale

Twenty seeded walker runs per cell, both rooms, ten brain minutes, no brain. The walker cannot
exercise the blocked-direction cooldown — nothing tells it whether it moved — so this covers the
hold, the fatigue gain and the hysteresis only, and it is the bedroom-regression check section 1's
tables were for.

| room | `fatigueGain` | `hysteresis` | left the map | median | out of the house | median |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| bedroom | 0.04 | 1.15 | 20/20 | 29.2 s | 20/20 | 162.1 s |
| bedroom | 0.08 | 1.05 | 20/20 | 36.0 s | 20/20 | 122.2 s |
| ground floor | 0.04 | 1.15 | 20/20 | 18.1 s | 20/20 | 24.8 s |
| ground floor | 0.08 | 1.05 | 20/20 | 12.4 s | 20/20 | 25.8 s |

Two readings. The bedroom does not regress: still 20/20, the median time to leave it moves by 7
seconds inside a table whose v0.1.0 spread was 22.6 to 43.7 seconds, and the time to get *outside*
from the bedroom improves by a quarter. And the scale is worth stating plainly: a uniform random
walker leaves the ground floor for Pallet Town in a median 25 seconds at either setting. The fly's
best measured median is 130 seconds. The readout is no longer the thing stopping it, but it is still
five times slower than choosing directions at random, and nothing here claims otherwise.

## Reproducing

The checkpoint is not in the repository and never will be. Pull one read-only off the release box —
read `manifest.json` first for the generation to ask for:

```sh
ssh the host 'pct exec <release-ctid> -- cat /srv/fly/state/manifest.json'
ssh the host 'pct pull <release-ctid> /srv/fly/state/<latest>.checkpoint /tmp/fly.checkpoint'
scp the host:/tmp/fly.checkpoint ./
ssh the host 'rm -f /tmp/fly.checkpoint'
```

Then, from `services/flysim`:

```sh
FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
FLY_ESCAPE_CHECKPOINT=./fly.checkpoint FLY_ESCAPE_BRAIN=../../data/fafb-v783 \
FLY_ESCAPE_BRAIN_RUNS=15 FLY_ESCAPE_MINUTES=10 \
FLY_ESCAPE_BRAIN_THREADS=1 FLY_ESCAPE_BRAIN_PARALLEL=15 \
FLY_ESCAPE_CANDIDATES=800/0.08/0.35/1.05 \
  cargo run --release -p flysim --example room_escape
```

About nine minutes of wall time per candidate on a 16-core box. `FLY_ESCAPE_CANDIDATES` takes names
from the harness's own list or bare `hold/fatigue/blocked[/hysteresis]` tuples, so a follow-up point
needs no recompile. The fresh-boot half is the same binary without `FLY_ESCAPE_CHECKPOINT`:

```sh
FLY_ROM=... FLY_ESCAPE_SKIP_SWEEP=1 FLY_ESCAPE_BRAIN=../../data/fafb-v783 \
FLY_ESCAPE_BRAIN_ROOM=1F FLY_ESCAPE_BRAIN_RUNS=5 FLY_ESCAPE_MINUTES=10 \
FLY_ESCAPE_BRAIN_THREADS=15 FLY_ESCAPE_CANDIDATES=800/0.08/0.35/1.05 \
  cargo run --release -p flysim --example room_escape
```

which boots the cartridge, walks one random-walker run down the stairs, caches that state in
`room-escape-1f.state` (gitignored) and runs the brain from it. The walker table is the same binary
with neither variable and `FLY_ESCAPE_HYSTERESES` set. binjgb prints the cartridge header on every
boot, so read the report through `grep -v`.

## Honesty

This measures the readout, from one state, on one cartridge, with one connectome. Fifteen noise
streams from a single checkpoint are fifteen samples of one initial condition, not fifteen
independent flies, and a configuration that leaves a house faster has not learned anything — the
whole reason to measure from the live state was to see the readout as the stream actually runs it,
after the drift and the spent ledger a fresh boot does not have. The one thing here that is evidence
about the fly is the last row of the walker table, and it says the fly is still slower than chance.
