# Room escape: straighter runs and boundary rewards

Decided 2026-09-16 (the operator): the fly struggles with rooms. Two levers, both honest under the doctrine
(buttons always the fly's; readout fixed and global; rewards are design):

## 1. Straighter runs (readout)

The Game Boy preset commits to a direction for 400 ms with hysteresis 1.15 and fatigue 0.08/0.8.
One overworld step is about 270 ms, so runs are one to two steps and the walker jitters. Raise the
direction hold and decision period together (candidates 600, 800, 1000 ms), keep hysteresis, and
lower fatigue gain so a run can persist (0.04). Choose by measurement, then bake the value into the
preset as `gameboyDecoderConfig()` (TS and Rust twins, golden regenerated). Decoder version note
and the compatibility string are unaffected (readout state is not in the neural checkpoint) but
the readout doc records the change and the date.

Measurement: the random-walker harness (`services/flysim/crates/flysim/examples/random_walker.rs`
pattern, on the Pokémon ROM) started from the archived bedroom snapshot and from the house 1F
snapshot, 20 seeded runs per hold value, 10 brain minutes each, unthrottled. Report median time to
leave the map and the fraction that leave at all. Then confirm with the real brain for the chosen
value (5 runs) so the number is not a random-walker artefact.

## 2. Boundary rewards (adapter)

Pay a small bonus the first time per map the fly stands on a tile adjacent to a warp (door, stairs,
map edge exit) and again the first time it stands on the warp tile itself. Warps come from the
current map's warp table in WRAM (`wNumberOfWarps`, `wWarpEntries` at the pinned pokered commit;
verify names and layout: y, x, destination warp id, destination map). Map-edge exits are the
connection tiles (`wMapConnections`); pay the same for the first step onto a route boundary tile.
Values: adjacent 0.05, on-warp 0.10, capped once per (map, warp) for the lifetime ledger so
oscillating cannot farm. Kind `boundary`, feed reward kind mapping `explore` (no new feed kind),
label "found an exit". Adapter version `pokered-unique8-v5`; old checkpoints refused as before,
and the deploy-time compatibility check archives state.

Doctrine note for the honesty panel and README: the reward catalog now includes exits; still no
button path, still no map knowledge in the readout.


## 3. v0.1.1: the ground floor, diagnosed from the live fly

Decided 2026-09-16 (the operator), after the release box (the release container, `twitch.tv/<twitch-channel>`, v0.1.0)
spent forty minutes on rung 2 with the whole of section 2's boundary reward already collected:
"rework reward and/or movement and push out 0.1.1".

Section 1's measurement was a random walker from a freshly booted cartridge. That instrument cannot
see this bug, and said so at the time: *every* candidate left both rooms in every one of 480 runs,
so the leave fraction saturated and only the time to leave could be compared. It also measured the
wrong thing, which nobody noticed: on the ground floor "left the map" is the staircase, which is one
step and lands in the other room of the same house.

The v0.1.1 measurement therefore starts from the live fly instead — generation 525 of the release
run, pulled read-only off the release container — through `FLY_ESCAPE_CHECKPOINT` in the same harness, and reports
leaving the *house*. That state carries what a fresh boot does not: 43 brain minutes of plasticity,
a calibrated baseline the network has since drifted away from, and a reward ledger with every payout
in the house already spent. `infra/docs/room-escape.md` holds the numbers.

### What the room actually is

`survey` in the harness walks map 37 with real button presses on throwaway emulators. Red's ground
floor has **48 reachable tiles**, and exactly **six presses leave it**:

| from | press | to |
| --- | --- | --- |
| (6, 1) | right | map 38, the bedroom |
| (7, 2) | up | map 38, the bedroom |
| (2, 6) | down | map 0, Pallet Town |
| (3, 6) | down | map 0, Pallet Town |
| (2, 7) | down | map 0, Pallet Town |
| (3, 7) | down | map 0, Pallet Town |

Two facts follow that the design did not have when section 2 was written, and both matter more than
anything in it:

- **Leaving the map is not leaving the house.** Two of the six exits are the staircase, which warps
  the instant it is stepped on and lands the fly in a room it has also exhausted.
- **The front door needs DOWN, and only DOWN.** The two doormats are on the bottom row and the two
  tiles above them are walkable, so four tiles are one press from outside; on every one of them that
  press is `down`. A sideways step onto a mat does nothing, which is why the live ledger holds
  `boundary:37:7:2:on` and `boundary:37:7:3:on` — paid 0.10 each, at 355 s — from a fly that never
  went outside.

### The four hypotheses, answered

- **(a) A persistent rate bias makes one direction win almost always, and fatigue 0.04 lets it
  stick.** Half right, and the half that is wrong matters. There *is* a persistent bias — `down`
  won 14 to 18% of decisions against `left`'s 32 to 34%, run after run — but no direction wins
  almost always, because the bias is small and fatigue does rotate the winner every four decisions.
  The bias is a standing preference order, not a lock. What made it fatal is that the direction at
  the bottom of the order is the only one that opens the door.
- **(b) The door needs DOWN from the tile above the mat while the fly's DOWN drive is weak.** Yes,
  and it is the proximate cause. Four tiles, one press, and that press is the least likely of the
  four. Over fifteen ten-minute runs the fly spent 123 frames of 537,000 — 0.02% of its life — on
  the two tiles above the mats.
- **(c) Hysteresis 1.15 plus long holds create wall-hugging loops.** Yes, and it is the biggest
  number in the report: nearly two of every three 800 ms holds moved the player not one tile, for up
  to 34 seconds at a stretch. The lock is arithmetic — the four direction scores sit inside a 9%
  spread and a challenger needs 15% — so the winner could only ever be changed by habituation. The
  fly orbited the walls, and the staircase is on that orbit while the front door is not.
- **(d) The stall rollback resets to the same room and wastes attempts.** Not what happened. The
  ratchet fired **zero** recoveries in 43 live brain minutes and zero in 150 measured ones, with 0
  of 3 attempts spent at rank 2. The stall window never matures: `Ratchet::observe` pushes
  `lastProgress` forward on any unsafe stretch longer than `unsafeResetMs` (1 s), and a fly walking
  and bumping produces those constantly. The objection is still true in principle — the archived
  snapshot for rank 2 *is* a ground-floor state — but it is not this bug and nothing here changes
  it. Recorded as a known limitation.

### The fix

Three numbers in the Game Boy preset, all readout, no reward rule and no button path:

| | v0.1.0 | v0.1.1 |
| --- | ---: | ---: |
| `holdMs` = `decisionMs` | 800 | 800 |
| `fatigueGain` | 0.04 | **0.08** |
| `hysteresis` | 1.15 | **1.05** |
| `blockedFatigue` / `blockedMs` | off | **0.35 / 800** |

Out of the house within ten brain minutes, fifteen runs each from generation 525: v0.1.0 got 1 out,
this gets 14, and 13 of those inside five minutes against 0. `infra/docs/room-escape.md` has the
candidate table, including the two that were tried and rejected (hysteresis 1.00, and hold 600) and
the interaction that decides the third number: the blocked-direction cooldown made things *worse* at
hysteresis 1.15 and is decisive at 1.05.

The blocked-direction cooldown is the one new mechanism, and it is deliberately the narrowest input
that fixes a wall bump: the sim loop tells the readout the name of a direction it has watched
produce no movement for a whole hold, and the readout habituates that channel. It says "that button
did nothing" — not where the fly is, not where the door is, not which button would have worked.
`docs/readout.md` specifies it and `docs/rewards-learning.md` discloses it on the honesty panel.

### What was not changed, and why

**The reward side.** Section 2's `boundary` rule pays once per exit per lifetime, and the design's
own v0.1.1 candidate was a small decaying bonus for each new visit to a warp-adjacent tile after
60 s. It was not needed and, on these numbers, would not have worked: the fly spent 0.02% of its
frames on a door-approach tile, so a repeat bonus would have paid at most once or twice in a
ten-minute run — too sparse for the eligibility traces to build anything from, and the runs that
most needed help never got near the door at all. The readout change fixes what was broken, and a
reward gradient is worth reconsidering now that the fly reliably reaches the tiles it could be paid
on. `REWARD_ADAPTER` stays at `pokered-unique8-v5` and `STATE_VERSION` at 4.

For the deploy: because readout state is not part of the neural checkpoint and the adapter version
is untouched, **the compatibility string does not move**. v0.1.1 restores the live fly's generation
525 and simply decodes on the new numbers. The run does not reset and the ladder does not
rebaseline.

**The direction-run cap.** The design listed "after N consecutive holds of the same direction, that
direction's fatigue jumps" as a candidate. It is already what `fatigueGain` does, one linear step at
a time instead of one jump, and unlike a counter it needs no new field in `DecoderState` and no
version bump. Raising the gain from 0.04 to 0.08 halves N, which is the same lever with none of the
cost.

**Recalibration.** `calibrate()` runs once, at warm-up, so the entire 9% spread between the four
direction scores is drift since then, frozen in. Re-tying them periodically would dissolve the
preference order at its root, but running once is what makes the readout *fixed*
(`docs/readout.md`, "The readout is fixed") — a doctrine commitment, not a tuning knob. Left for
The operator; `docs/limitations.md` records it.

## Verification

Synthetic WRAM traces: warp table parsing, adjacency, once-per-warp, edge exits, no payout inside
a map already baselined; ROM-gated: from the bedroom archive, the first payouts are boundary events
near the stairs. Random-walker report committed under `infra/docs/room-escape.md` with the chosen
hold value.

For section 3: the blocked-direction cooldown has unit tests on both decoder twins (one report
unseats an incumbent that habituation would have taken three more decisions to shift; repeated
reports inside one hold are one penalty; a report never lowers fatigue and is capped at 1; a report
is ignored when the rule is off or names a channel outside the group), and `GameAdapter::location`
has one on the adapter (the same tile in two rooms is two locations, which is what stops a warp
reading as standing still). The preset's own numbers are pinned separately, in
`flybrain-core/tests/decoder.rs` and `packages/brain/tests/readout.test.ts`, so a preset edit fails
a test. The live-state measurement itself is not a test: it needs a checkpoint that is not in the
repository and a quarter of an hour, and it is reproduced by hand from `infra/docs/room-escape.md`.
