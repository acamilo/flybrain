# Macro palette: the smoke run

Measured 2026-09-16 on the WSL development box for `docs/design/macros.md` section 7.

**This is not the gate.** Section 7 asks for 6 brain hours per arm from the same archived
checkpoint, three runs, with palette mode shipping as the default only if it reaches rung 6 or
higher in fewer brain hours in 3 of 3. This is **one** run of **one** brain hour per arm from a
**fresh boot**, because there is no live checkpoint on this box: the release box's durable state
has not been pulled here, and `FLY_MACRO_CHECKPOINT` was therefore unset. It establishes that the
wiring works and what it costs; it decides nothing.

## What was run

`services/flysim/crates/flysim/examples/palette_bench.rs` at `feat/macros-simloop`, on the pinned
Pokémon Red cartridge, unthrottled, both arms side by side with 4 sweep threads each.

```sh
FLY_ROM=".../Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_MACRO_HOURS=1 FLY_MACRO_DIGEST=1 FLY_MACRO_THREADS=4 \
  cargo run --release -p flysim --example palette_bench
```

Both arms are the sim loop's own frame order (`flysim::frame::LegacyFrame` since 2026-09-23;
before that `NeuralAgent::tick`'s, one frame behind the stream) over the real connectome (`data/fafb-v783`), the real
Game Boy readout preset with nothing overridden, the real Pokémon adapter paying the real reward
catalog, and the real ratchet on the adapter's own recovery policy. The only difference between
them is `flysim::macros::MacroLayer`, built from the configuration the way `Sim::boot` builds it,
so the raw arm has no layer rather than a disabled one.

The harness drove the cartridge intro with scripted presses and both arms started from that one
identical playable frame in Red's bedroom (map 38) with a freshly warmed-up brain. The fly's own
readout was not asked to sit through the naming screens, which is also why this run says nothing
about palette mode on the title screen — that path is raw by design and is covered by
`tests/integration.rs` instead.

## Both arms, 1 brain hour each

| measure | raw | palette |
| --- | ---: | ---: |
| brain hours | 1.00 | 1.00 |
| frames | 215,020 | 215,020 |
| wall seconds | 1,449 | 1,449 |
| realtime factor | 2.48x | 2.48x |
| rung reached | **5** (GOT A STARTER) | **4** (OAK'S LAB) |
| reward total | 9.200 | 6.650 |
| reward per brain hour | **9.200** | **6.650** |
| new locations | 141 | **244** |
| recoveries | 0 | 0 |
| frames with nothing pressed | 0 (0%) | 134,446 (63%) |
| frames a macro owned the buttons | — | 118,500 (55%) |
| macros started | 0 | 3,355 |
| trajectory digest | `a25dbaef740223f0` | `0358f5d151216668` |

Brain hours at each rung, from the shared starting frame:

| rung | label | raw | palette |
| ---: | --- | ---: | ---: |
| 0 | BOOT | 0.000 | 0.000 |
| 1 | BEDROOM | 0.000 | 0.000 |
| 2 | DOWNSTAIRS | 0.008 | **0.001** |
| 3 | PALLET TOWN | 0.148 | **0.013** |
| 4 | OAK'S LAB | **0.156** | 0.249 |
| 5 | GOT A STARTER | **0.185** | — |

Macro outcomes over the palette hour: **3,355 started, 3,334 done, 21 blocked, 0 timeout, 0
refused.** Payouts by kind — raw: boundary 11, exploration 16, map 3, milestone 5, species 4;
palette: boundary 32, exploration 29, map 4, milestone 2. Frames by scene in the palette arm:
overworld 66%, dialog 15%, menu 13%, unknown 6%.

## What this establishes

- **The wiring works end to end.** 3,355 macros ran in an hour and 99.4% of them finished on their
  own script rather than being cut off. Nothing timed out, and nothing was refused: the palette's
  preconditions and the executor's route search agreed on every one of 3,355 starts, which is the
  path that was most likely to be noisy in production.
- **It costs nothing measurable in throughput.** Both arms stepped 215,020 frames in the same
  1,449 s at 2.48x realtime, on the same box at the same time. Scene detection and
  `Palette::for_scene` run every frame in palette mode, and at this resolution they do not show up
  against a 139,255-neuron brain step. That is the number the release box cares about, since it has
  to hold 1.0x.
- **Palette mode leaves a room much faster and then stalls.** Out of the bedroom in 0.001 brain
  hours against 0.008, and out of the house into Pallet Town in 0.013 against 0.148 — an order of
  magnitude, and exactly what `GO EXIT` is for. It then took longer to reach Oak's lab (0.249
  against 0.156) and never got the starter inside the hour, while covering nearly twice as much
  ground (244 new locations against 141).
- **And it earned less.** 6.65 reward per brain hour against 9.20. The two arms earn from different
  rules: palette doubled the `boundary` and `exploration` payouts (32 and 29 against 11 and 16)
  and lost the `milestone` and `species` ones, which are the large values. Covering ground is not
  the same as climbing, and this hour is the clearest statement of that difference so far.

## What it does not establish

- Nothing about the gate. One run, one seed, one brain hour, a fresh boot rather than an archived
  checkpoint, and palette mode came out **behind** on the rung that matters. Section 7's bar is
  rung 6 in fewer brain hours in 3 of 3 runs; this is not evidence for or against that, because an
  hour from a fresh brain is the noisiest part of a run — a single early dialogue box the fly
  answers differently moves both arms by more than the gap between them.
- Nothing about the fly. It measures what a scene-appropriate action palette does to a run.
- Nothing about a long run. The reward attribution the design is actually for — "the interval
  between a macro decision and its payout is now one macro long" — is a learning effect, and an
  hour with 0 recoveries is not long enough to see one.

## Plan mode, one brain hour, 2026-09-16

The third arm (`docs/design/macros.md` section 9, `FLY_MACRO_ARMS=plan`), on the same box, the
same fresh boot and the **same seed** as smoke run 1, so the row below sits beside the two above.
One arm rather than three: the raw and palette numbers are the ones already recorded, and
re-running them would have measured the box's load rather than the mode. Re-run after section
9.1's `TALK` amendment; the pre-amendment run is the "what it does not establish" note below.

```sh
FLY_ROM=".../Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_MACRO_HOURS=1 FLY_MACRO_DIGEST=1 FLY_MACRO_THREADS=4 FLY_MACRO_SEED=20260916 \
  FLY_MACRO_ARMS=plan cargo run --release -p flysim --example palette_bench
```

| measure | raw | palette | **plan** |
| --- | ---: | ---: | ---: |
| brain hours | 1.00 | 1.00 | 1.00 |
| frames | 215,020 | 215,020 | 215,020 |
| wall seconds | 1,449 | 1,449 | 1,033 |
| realtime factor | 2.48x | 2.48x | 3.49x |
| rung reached | **5** (GOT A STARTER) | **4** (OAK'S LAB) | **4** (OAK'S LAB) |
| reward total | 9.200 | 6.650 | 4.650 |
| reward per brain hour | **9.200** | 6.650 | 4.650 |
| new locations | 141 | **244** | 63 |
| recoveries | 0 | 0 | 0 |
| frames with nothing pressed | 0 (0%) | 134,446 (63%) | 157,519 (73%) |
| frames a macro owned the buttons | — | 118,500 (55%) | 90,989 (42%) |
| macros started | 0 | 3,355 | 3,403 |
| trajectory digest | `a25dbaef740223f0` | `0358f5d151216668` | `61f77acca870656e` |

Brain hours at each rung, from the shared starting frame:

| rung | label | raw | palette | **plan** |
| ---: | --- | ---: | ---: | ---: |
| 0 | BOOT | 0.000 | 0.000 | 0.000 |
| 1 | BEDROOM | 0.000 | 0.000 | 0.000 |
| 2 | DOWNSTAIRS | 0.008 | 0.001 | **0.001** |
| 3 | PALLET TOWN | 0.148 | 0.013 | **0.003** |
| 4 | OAK'S LAB | 0.156 | 0.249 | **0.005** |
| 5 | GOT A STARTER | **0.185** | — | — |

Macro outcomes over the plan hour: **3,403 started, 3,401 done, 2 blocked, 0 timeout, 0 refused.**
Payouts by kind — boundary 22, exploration 6, map 4, milestone 2. Frames by scene: overworld 65%,
dialog 22%, unknown 12%. Silent frames by cause: decision cooldown 68%, macro pressing nothing
23%, readout silent 8%, plan skipped 647 frames, no bound slot 0, macro refused 0.

The wall-clock column is not comparable: the raw and palette arms ran side by side on this box and
the plan arm ran alone, which is the whole of the 2.48x against 3.49x. Nothing about the layer
changed the per-frame cost — 215,020 frames in an hour in all three arms, as before.

### What this establishes

- **The early ladder is a walk now, not a search.** Out of the bedroom in 0.001 brain hours, into
  Pallet Town in 0.003 against palette's 0.013 and raw's 0.148, and into Oak's lab in 0.005
  against 0.249 and 0.156 — fifty times faster than palette mode on the rung palette mode was
  slowest on. That is `GO OBJECTIVE` reading the rung catalog's places and the drive doing nothing
  cleverer than "next step".
- **`TALK` is in the plan and firing.** Dialog is 22% of the hour's frames, against none before the
  amendment, and `tests/rom_plan.rs` shows the same drive taking a starter from Oak's lab in 3.5
  brain minutes when the balls are on the table. Nothing refused and two blocked in 3,403.
- **And the hour still ends on rung 4, for a different reason than before.** The starter is gated
  behind Oak's own script: `PalletTownDefaultScript` triggers on `wYCoord == 1`, Oak intercepts the
  player at the north edge of the town and walks them into the lab, and until that has run there
  are no Pokéballs on the table to talk to. The plan pulls the fly *into the lab* — rung 4's place
  is the lab, and rung 5's is the same room — so it does not spend time at the north edge, and
  nothing in the plan is aimed at a script trigger. The ROM test drives that one stretch itself and
  says so. **This is the next thing to decide**, and it is a contract question like the last one:
  either the rung catalog carries a place for rung 5 that is the trigger rather than the room, or
  the plan needs an entry for "the map edge the story is behind".
- **It earns least of the three.** 4.650 reward per brain hour against palette's 6.650 and raw's
  9.200, on a quarter of the ground covered (63 new locations against 244 and 141). The plan walks
  to the places the ladder names and stops exploring, so the `boundary` and `exploration` payouts
  that carried palette mode's total do not arrive. Reaching rungs faster and earning less in the
  same hour is exactly the tension section 7's gate is about, and one hour of one seed does not
  settle it.

### What it does not establish

Nothing about the gate — one run, one seed, one brain hour, a fresh boot rather than the archived
checkpoint, and no raw or palette arm run beside it on the same box at the same time. Nothing about
the fly. Nothing about a long run: the reward attribution plan mode is for is a learning effect and
an hour with 0 recoveries cannot show one.

Two earlier plan hours were run and discarded, and both are worth knowing about rather than hiding:
the first (digest `c9da99631adf4071`) predates the section 9.1 demotion and bounced between Oak's
lab and Pallet Town once per hold; the second (`2c0e08fe5586d4bb`) predates the `TALK` amendment
and so could not press A at anything. Neither is evidence about anything except the two faults it
found.

## plan (biased), one brain hour, 2026-09-16

`docs/design/macros.md` section 10's blend, on the same box, the same fresh boot and the **same
seed** as smoke run 1 and the plan row above, so all four compare. One arm again, for the same
reason: the other numbers are already recorded and re-running them would measure the box's load.

```sh
FLY_ROM=".../Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_MACRO_HOURS=1 FLY_MACRO_DIGEST=1 FLY_MACRO_THREADS=4 FLY_MACRO_SEED=20260916 \
  FLY_MACRO_ARMS=plan cargo run --release -p flysim --example palette_bench
```

Weights as shipped (`SceneBias::default`): overworld 0.3, dialog 0.95, menu 0.9, battle 0.9,
battle-switch 0.95, shop 0.9, pc 0.9, unknown 0.5. `fly[slot]` is the spread across the bound
slots (section 10 as amended the same day; the run on the first reading of that sentence is the
note at the end of this section).

| measure | raw | palette | plan | **plan (biased)** |
| --- | ---: | ---: | ---: | ---: |
| brain hours | 1.00 | 1.00 | 1.00 | 1.00 |
| frames | 215,020 | 215,020 | 215,020 | 215,020 |
| wall seconds | 1,449 | 1,449 | 1,033 | 1,063 |
| realtime factor | 2.48x | 2.48x | 3.49x | 3.39x |
| rung reached | **5** (GOT A STARTER) | **4** | **4** | **4** (OAK'S LAB) |
| reward per brain hour | **9.200** | 6.650 | 4.650 | 4.450 |
| new locations | 141 | **244** | 63 | 59 |
| recoveries | 0 | 0 | 0 | 0 |
| frames with nothing pressed | 0 (0%) | 134,446 (63%) | 157,519 (73%) | 156,768 (73%) |
| frames a macro owned the buttons | — | 118,500 (55%) | 90,989 (42%) | 84,708 (39%) |
| macros started | 0 | 3,355 | 3,403 | 4,041 |
| trajectory digest | `a25dbaef740223f0` | `0358f5d151216668` | `61f77acca870656e` | `ca2b9c96fdad264f` |

Brain hours at each rung, from the shared starting frame:

| rung | label | raw | palette | plan | **plan (biased)** |
| ---: | --- | ---: | ---: | ---: | ---: |
| 0 | BOOT | 0.000 | 0.000 | 0.000 | 0.000 |
| 1 | BEDROOM | 0.000 | 0.000 | 0.000 | 0.000 |
| 2 | DOWNSTAIRS | 0.008 | 0.001 | 0.001 | **0.001** |
| 3 | PALLET TOWN | 0.148 | 0.013 | 0.003 | **0.003** |
| 4 | OAK'S LAB | 0.156 | 0.249 | **0.005** | 0.025 |
| 5 | GOT A STARTER | **0.185** | — | — | — |

Macro outcomes: **4,041 started, 3,600 done, 440 blocked, 0 timeout, 0 refused.** Payouts by kind
— boundary 20, exploration 5, map 4, milestone 2. Frames by scene: dialog 66%, overworld 34%.
Silent frames by cause: decision cooldown 50%, readout silent 31%, macro pressing nothing 19%, no
bound slot 0, macro refused 0.

**Macros chosen by rank: rank 0 3,482 (86%), rank 2 455 (11%), rank 4 93 (2%), rank 3 11 (0%).**
That is the row section 10's gate asks for, and it is a blend.

### What this establishes

- **The blend blends.** The fly took 14% of the decisions off the plan's head — 559 macros — and
  the plan kept the other 86%. Both halves are doing what the weights say: the overworld is a
  third of the hour's frames and `w = 0.3` there, dialog is two thirds and `w = 0.95`, so a
  rank-0-heavy overall split with the fly winning in the rooms is exactly the shape the operator's numbers
  describe. Rank 1 never won, which is the prior ladder's own doing: rank 1's prior is half rank
  0's, so a fly that leans on rank 1 has to beat 0.3 with 0.7 x 1.0 + 0.15 — it does, whenever it
  leans — and the reason the count is 0 rather than small is that the *policy* rarely offers a
  second entry the fly's leading channel points at. Worth watching over a longer run.
- **The early ladder holds.** Downstairs 0.001 and Pallet Town 0.003, the same as the cursor
  drive. Oak's lab took 0.025 brain hours against the cursor drive's 0.005: five times slower and
  still six times faster than raw mode's 0.156 and ten times faster than palette mode's 0.249.
  That is the cost of letting the fly overrule the map on a third of its decisions, and it is the
  trade section 10 is asking for rather than a regression.
- **440 blocked, against 2 on the cursor drive.** A blocked macro is one whose route stopped
  producing movement, and the fly interrupting a walk it did not choose is how that happens: the
  fly's slot wins a decision, its macro starts, and the *next* decision goes elsewhere. Nothing
  refused and nothing timed out in 4,041 starts, so the executor and the palette still agree on
  every start.
- **The fly is still the only thing that starts a macro.** 31% of the silent frames are "readout
  silent" — every bound row at or under the readout's activity floor, or the spread flat — so a
  third of the doing-nothing is the fly saying nothing, and the scene never acted alone in it.

### What it does not establish

Nothing about the gate: one run, one seed, one brain hour, a fresh boot rather than the archived
checkpoint, no other arm beside it on the same box at the same time, and the box was also running
cartridge tests during this hour, so the wall-clock and realtime columns are worth even less than
usual. Nothing about the fly. Nothing about a long run — and the reward attribution plan mode is
for is a learning effect, which an hour with 0 recoveries cannot show.

**The run that forced the amendment.** The same hour on the first reading of "the channel's
normalized score in [0, 1]" — each channel against its own baseline, `clamp(score - 1, 0, 1)` —
chose **rank 0 for all 4,399 of its macros** (digest `6abcbcbd815646fd`, 3.650 reward per brain
hour, 36 new locations, rungs at 0.001/0.003/0.005). The fly decided whether a macro started and
never which one, because the readout's score is a ratio around 1 whose directions sit inside a 9%
spread (`docs/readout.md`): every vote landed in 0..0.1 while beating rank 0 at `w = 0.3` needs
0.214. Same weights, same seed, same code otherwise — the whole difference between a blend and the
plan alone was the unit the fly's vote is measured in.

And one thing neither hour establishes: **the starter**. Section 9.1's `TALK` amendment relied on
section 9's "a plan whose head changes puts the drive back at the top", and section 10 has no
cursor to reset — `TALK` is rank 0, rank 0 is the UP channel, so the press that takes a Pokéball
needs the fly to lean on UP while facing one. The cartridge test
(`services/flysim/crates/flysim/tests/rom_plan.rs`) still reaches Oak's lab and its rung from the
bedroom on a game-blind stub readout, in 1.6 brain minutes on 10 macros, but no stub took a
starter under either reading of `fly[slot]`: every slot equal pressed A 615 times at one shelf, one
hot slot per burst wandered out of the lab 367 times, and a committing rotation did the same over
five times the frames (4,904 macros, `TALK` once, identical under both mappings). The starter leg
is therefore off that test, with the finding recorded in it. **This is the next thing to decide.**

## Smoke run 2: not yet measured

The two causes smoke run 1 pointed at are fixed on `feat/macros-lab` (2026-09-16) — slot 2 is
`GO ITEM` instead of the duplicate `LOOK`, and `GO EXIT` reads the adapter's `boundary` ledger
through `GameAdapter::exit_visited` — and the harness now attributes every silent frame to its
cause. **The re-run was deferred, so there is no smoke-2 table here yet.** Do not read the fixes as
measured: all that is established is that they are wired, unit-tested and confirmed on the
cartridge (`tests/rom_macros.rs`: `GO ITEM` walks to a starter ball in Oak's lab and `TALK` there
hands over a Charmander; `GO ROUTE` -- `GO EXIT` when that run was recorded -- in Pallet Town
leaves by an unvisited door rather than the one it came out of).

When it is run, it is one brain hour per arm from a fresh boot on the same seed as the record
above, so that the two tables compare:

```sh
FLY_ROM=".../Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_MACRO_HOURS=1 FLY_MACRO_DIGEST=1 FLY_MACRO_THREADS=4 FLY_MACRO_SEED=20260916 \
  cargo run --release -p flysim --example palette_bench
```

The palette arm now prints one extra table, which is the question smoke run 1 could not answer
about its own 63% of frames pressing nothing:

| silent frame | meaning |
| --- | --- |
| macro pressing nothing | a macro owned the buttons and its script held nothing: a settle, the gap between two pulses, a wait for a menu cursor, or the frame it finished on |
| readout silent | no channel was active, so the fly asked for nothing |
| no bound slot | a channel was active and its slot is unbound in this scene |
| decision cooldown | a channel was active and bound, but the last decision is less than one `holdMs` old |
| macro refused | a bound macro's precondition had lapsed, or it found no route |

"readout silent" means something slightly different in plan mode since section 10, because the
channels there are scores rather than a set: it is a readout with every bound row's channel at or
below the readout's own activity floor, i.e. a fly with nothing to say. The sixth cause, "plan
skipped", is gone with the B-skips rule section 10 replaced.

The split is what decides whether the palette is too small or never reached, and the two are
different fixes: the first is a binding change in `docs/design/macros.md` section 3, the second is
not. A trajectory digest that differs from smoke run 1's is expected and is not a harness fault —
the fixes change which buttons get pressed, which is the point.

## Running the real thing

1. Pull the release box's durable checkpoint read-only (`/srv/fly/state/<gen>.checkpoint`) onto the
   dev box. Do not run this against the release container's live state directory.
2. `FLY_MACRO_CHECKPOINT=/path/to/<gen>.checkpoint FLY_MACRO_HOURS=6` with three different
   `FLY_MACRO_SEED` values, one run per seed, both arms each.
3. Record all three tables here with the checkpoint's generation and brain hours, and compare the
   brain hours at rung 6 and above. `FLY_MACRO_DIGEST=1` on every run: two runs of one arm with one
   seed on one box must print the same digest, and a digest that moves between runs means something
   in the harness is not deterministic and the numbers cannot be compared.
4. Palette mode stays off (`FLY_MACRO_MODE=raw`, which is the default and what every env file
   carries) until that passes and the operator has seen the screen.

The one-hour smoke run above takes about 24 minutes of wall time for both arms together; six hours
per arm is about two and a half hours, so the three runs are most of a working day on this box.
