# Readout

`PopulationDecoder` (`packages/brain/src/readout/decoder.ts`) turns per-role population rates into
a set of active output channel names. Nothing about the downstream device enters the module:
channels are named by the caller and the only input is the `role -> rate` record the network
produces. Device specifics live in presets under `src/readout/presets/`.

## Score

Every decision uses one normalized score:

```
score(channel) = (rate[role] + 1) / (baseline[role] + 1)
```

`baseline` is recorded once by `calibrate(rates)`, which stores the current rate of every
channel's role. The `+1` on both sides bounds the ratio when rates are near zero. A score of 1
means "at rest", above 1 means "above its calibration rate". Source: `readout/decoder.ts`,
`decode()`; original wording in `fly-plays-pokemon/docs/ratchet-sprint.md`, "Controller".

An uncalibrated decoder returns an empty array from `decode()` and changes no state.

## Exclusive group

Up to two optional `ExclusiveGroup`s, each with its own channels, winner, fatigue and decision
clock: `exclusive`, the device's directions, and `macros`, the macro buttons (see "Macro group"
below). At most one of a group's channels is active at a time, and that group's winner is
re-decided only when `nowMs >= nextDecision`. On a decision:

0. (Before the decision, on every `decode`: a blocked-direction report raises that channel's
   fatigue. See below.)
1. Divide each group score by `1 + fatigue[channel]`.
2. Take the argmax over the candidates — every channel of the group, except in the macro group,
   where the caller names them. Ties keep the earlier channel, because only a strictly greater
   score displaces the running best, and channel order is the insertion order of `channels`.
3. If there is a current winner, it is still a candidate, and `score[best] < score[current] *
   hysteresis`, keep the current winner. At `hysteresis = 1.05` a challenger must lead by 5% to
   take over; at 1.00 there is no commitment bonus and the argmax always wins.
4. Clear every group channel's hold, then set the winner's hold to `nowMs + holdMs`.
5. Add `fatigueGain` to the winner's fatigue, capped at 1; multiply every other channel's fatigue
   by `fatigueDecay`.
6. Set `nextDecision = nowMs + decisionMs`.

Fatigue is bounded habituation: it stops a small persistent rate bias from holding one channel
forever. It is motor history, not reward learning and not a navigation policy.

The cap matters and is worth stating plainly: fatigue lives in `[0, 1]`, so habituation can discount
an incumbent's score by at most a factor of two. A channel whose raw score leads the field by more
than `2 x hysteresis` is a permanent winner however long it holds, and a field whose scores are all
within `hysteresis` of each other -- which is what a settled network on the live stream actually
produces -- can only ever be reordered *by* fatigue, never by score. `fatigueGain` is therefore not
a tie-breaker; it is the rotation period.

## Blocked-direction cooldown

`decode(rates, nowMs, boot, blocked, bound)` takes one input that is not a rate: the name of an
exclusive channel the caller has observed producing no effect. (`bound` is the macro group's, and
is described under "Macro group".) Before anything else in a `decode`, a report
sets that channel's fatigue to `blockedFatigue`:

```
fatigue[blocked] = min(1, max(blockedFatigue, fatigue[blocked]))
```

`max`, not `+=`: the caller reports the same wall on every frame of a hold, and one wall is one
penalty however many frames observe it. A report never lowers a fatigue the channel has already
earned, and it is bounded by the same cap as `fatigueGain`.

That is the whole rule. It adds no state — fatigue is already in `DecoderState`, so the decoder
version and the compatibility string are untouched — and it does nothing at all when
`blockedFatigue` is 0, when `blocked` is null, or when `blocked` names a channel outside the group
(a pulse channel, or a typo). `decode` without the argument is byte-identical to the three-argument
`decode` that came before it. A blocked report names a channel, so it reaches whichever group holds
that name: the macro group has the same field and the same rule, for the caller that one day
reports a macro that moved nothing.

**Why fatigue and not a lockout.** Habituation is the mechanism the group already has for "this
channel has had its turn", and the whole point is that a blocked direction should lose the *next*
decision rather than be forbidden. A wall the fly is facing may stop being a wall — it is standing
on a warp tile, a sprite moved, it turned a corner — and a channel whose fatigue decays back over
the following few decisions recovers by itself. A hard lockout would need a new timestamp in the
checkpoint and a rule for when to lift it.

**`blockedMs` is the caller's number, not this module's.** The decoder has no clock of its own
beyond `nowMs` and no idea what "movement" means, so it cannot decide when a direction has failed.
It publishes the window in the preset, next to the hold it is measured against, and the caller
applies it. In `flysim` that caller is the sim loop: it keeps the adapter's last reported position
(`GameAdapter::location`, the area *and* the tile) and the channel the group is holding, and
reports that channel once
*both* have been unchanged for `blockedMs`. Restarting the window on a new winner is what stops one
stuck hold blaming the direction that follows it, which has not had a hold to move in yet. A
`location` of `null` -- a battle, a script, a map transition -- is no information rather than
"still",
so the rule cannot fire while the fly has no control anyway.

**What it is not.** It says "that button did nothing". It does not say where the fly is, where it
should go, what the room looks like, or which button to press instead; the winner is still the
argmax of the same normalized scores over the same rates. It is the same kind of fact as the frame
the retina already sees, arriving through a much narrower channel, and it is disclosed on the
honesty panel with the rest of the readout.

**Where the location comes from on the session framework (2026-09-23).** When the live fly runs
on the session framework (the operator's port decision of 2026-09-23), the sim loop that owned
the position is split: the task reads the location, the agent owns the decoder. The location
then reaches the decoder as a **declared** field of its decision context,
`gameboy-readout-context-v1 {boot, bound, location}`
([legacy Game Boy composition](design/session-framework/legacy-gameboy-v1.md) section 5). The rule is
unchanged: the location only restarts the blocked window, `null` is still no information, the
held channel and the window stay the readout's own state, and the blocked channel is still
computed here, never handed in. The profile allowlists the field, and the task cannot put
anything else in it.

## Macro group (2026-09-16)

`DecoderConfig.macros` is a second `ExclusiveGroup` with the same fields and the same decision rules
as the direction group above, decided after it on its own clock. Its channels are the macro types of
`docs/design/macros.md` sections 12, 13 and 14: **31** of them, one per type, and a channel's name
*is* its rate role (`macro_go_item`, `macro_move_1`, …), because the population is the button and a
second name for it would be a second thing to keep in step. There are no pulses in the group.

Twenty-two when section 11 shipped. Section 13 added `GO SHOP`, `GO HEAL`, `BUY ANTIDOTE`,
`BUY REPEL` and `HEAL`; section 14 replaced `ATTACK` with `MOVE 1`..`MOVE 4`, added `THROW BALL`
and renamed `GO STAIRS` to `GO WARP`. Nothing about the *rules* above changed with either.

The one thing that differs is who may win. `decode(rates, nowMs, boot, blocked, bound)` takes the
channels the caller has bound — the scene's own buttons — and the argmax runs over those alone:

- an unbound channel is **masked out of the decision entirely**, not penalised. That is the
  opposite end from the blocked-direction cooldown, which habituates a channel that can still win;
  an unbound channel is not on the pad at all, so it cannot.
- a masked channel's fatigue still relaxes by `fatigueDecay`, exactly as a loser's does. A type the
  scene took away is resting, not accumulating.
- the winner is therefore always one of the bound set, and the incumbent's commitment bonus applies
  only while it is still bound: a macro the scene has taken away cannot hold its own seat.
- `bound` of `null` lets the whole group compete, which is what the direction group always does.
  An **empty** `bound` is a scene that binds nothing: no winner, no hold, no fatigue, and
  `nextDecision` does not move, so the frame a button appears on is a frame that can press it.

`macroChannelNames` is the group's channels in config order and `macroWinner` is its current
winner, which is the macro the game layer starts. In raw mode there is no group to consult: the
preset is built with no macro roles, which is also every non-Pokémon adapter, so the decoder has
no macro channels, no macro state and nothing to mask.

The 31 populations are a dataset artifact, not a decoder one: `tools/build_flywire.py` shares the
96 mushroom body output neurons and the 110 brain motor neurons — 206 neurons, six or seven each —
round-robin over the types in the contract's own order, and emits them into
`data/fafb-v783/circuit-roles.json` with the checksums in `tools/artifact-checksums.txt`. Neuron
ids, edges, kernel and every pre-existing *anatomical* role are byte-identical — but 206 neurons
dealt thirty-one ways is not twenty-two ways plus nine, so **every macro population is re-dealt
whenever a type is added**, exactly as it was when section 11's amendment went from six slot
channels to twenty-two type channels. Rates are restored by role name and a new or renamed role
starts at zero, so a checkpoint written before the change loads; what a run loses across it is the
mushroom body's learned preference for particular macros. The macro roles are
deliberately outside the dataset fingerprint — `fingerprintDataset` hashes the metadata with the
`macro_*` roles removed, in both twins —
so `flysim --print-compatibility` is unchanged and a checkpoint written before the populations
existed still loads, with their rates starting at zero.

## Pulse channels

Any number of independent `PulseChannel` entries, evaluated every `decode()` call in config order.
A channel fires when `score > threshold` and `nowMs >= nextAllowed`. Firing sets
`heldUntil = nowMs + holdMs` and `nextAllowed = nowMs + cooldownMs`.

- `boot`: an alternate `{ cooldownMs, threshold }` used while `decode(rates, nowMs, boot)` is
  called with `boot` true, which is the default. `holdMs` is never overridden. A channel without a
  `boot` variant behaves identically in and out of boot.
- `throttleGroup`: when a channel with a throttle group fires, every channel in that group gets
  the same `nextAllowed`, so the group shares one cooldown.

## Active channels

```
active = channels where nowMs < heldUntil[channel]
```

returned as direction channels first in config order, then pulses in config order, then the macro
group's channels. `channelNames` exposes that same order. The macro channels go last so that every
index and every record order a consumer already reads is untouched by their arrival, and they are
not joypad names, so `toButtonMask` ignores them the way it ignores any unknown name.

`clearHolds(nowMs)` drops every hold, forgets both groups' winners, zeroes all fatigue including the
macro group's, sets both `nextDecision` clocks to `nowMs` so the next `decode()` re-decides
immediately, and locks every channel out until `nowMs + clearLockoutMs`. Use it when the environment
changes under the readout, for example after a state restore, so a stale hold cannot leak into the
new context.

## Game Boy preset

`gameboyDecoderConfig(macroRoles)` (`src/readout/presets/gameboy.ts`) is the only device-specific
module in the library. The D-pad is the exclusive group; A and B are fast pulses; Start and Select
are rare pulses sharing one throttle, with a permissive boot variant so title screens and new-game
menus still work; `macroRoles`, the `macro_<type>` roles of the running adapter, becomes the macro
group, and an empty list (the default) builds no macro group at all. Every value except the
direction hold, the decision period, the fatigue gain, the hysteresis and the blocked-direction
cooldown is the prototype's fixed motor decoder unchanged.

Exclusive group, `decisionMs` 800, `holdMs` 800, `hysteresis` 1.05, `fatigueGain` 0.08,
`fatigueDecay` 0.8, `blockedFatigue` 0.35, `blockedMs` 800. `clearLockoutMs` is 480.

Macro group (2026-09-16): the same seven numbers to the digit, over one channel per `macro_<type>`
role. They are the direction group's because a macro is a button, and because the measurement that
chose them (below) measured how long a commitment should last, not what it was to. No macro channel
has a bit: the game layer runs the winning macro, and the macro's own presses reach the emulator
through the same button register the fly's raw buttons use.

| Channel | Kind | Role | Bit | Hold | Cooldown | Threshold | Boot | Throttle |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `up` | exclusive | `command_0` | 0x01 | 800 ms | | | | |
| `down` | exclusive | `command_1` | 0x02 | 800 ms | | | | |
| `left` | exclusive | `command_2` | 0x04 | 800 ms | | | | |
| `right` | exclusive | `command_3` | 0x08 | 800 ms | | | | |
| `a` | pulse | `command_4` | 0x10 | 85 ms | 480 ms | 1 | | |
| `b` | pulse | `command_5` | 0x20 | 85 ms | 480 ms | 1 | | |
| `start` | pulse | `command_6` | 0x40 | 55 ms | 30000 ms | 1.35 | 2500 ms, 1 | `system` |
| `select` | pulse | `command_7` | 0x80 | 55 ms | 30000 ms | 1.35 | 2500 ms, 1 | `system` |

The bit column is `GAMEBOY_BUTTON_BITS`, the standard joypad mask. `toButtonMask(active)` packs
active channel names into that mask, ignoring unknown names; `fromButtonMask(mask)` unpacks in
`bits` key order. `GAMEBOY_BUTTONS` is the eight names as a const tuple.

In the prototype, boot mode was the reward adapter's active-game gate, so title and new-game
operation kept the permissive Start/Select threshold while live play used the strict one
(`fly-plays-pokemon/docs/ratchet-sprint.md`, "Controller").

### Changed 2026-09-16: the direction hold, from 400 ms to 800 ms

`decisionMs` and `holdMs` went from 400 to 800 and `fatigueGain` from 0.08 to 0.04.
`docs/design/room-escape.md` section 1 asked for it and `infra/docs/room-escape.md` holds the
measurement that chose the value: one overworld step in Pokémon Red is about 270 ms, so a 400 ms
commitment was one to two steps and the walker jittered in place instead of crossing a room. Over
sixty seeded random-walker runs of ten brain minutes each, a fly escapes Red's bedroom in a mean
27.0 brain seconds at 800/0.04 against 52.4 at 400/0.08, and the real brain leaves it in all five
of five confirmation runs.

Nothing about the decoder itself changed, and readout state is not part of the neural checkpoint,
so `DecoderState`'s version 4, the kernel version strings and the compatibility string are all
untouched: an existing checkpoint loads and simply decodes on the new timings. What did have to
move is the golden `agent` scenario (`services/flysim/golden/agent.flygold`), whose per-frame
button masks are the preset's own output, and the two `MotorDecoder` oracles in
`packages/brain/tests`, which now spell the prototype's 400 ms and 0.08 out for themselves — the
oracle proves the decoder's logic, and the preset's numbers are pinned by their own tests.

### Changed 2026-09-16 (v0.1.1): hysteresis to 1.05, fatigue gain back to 0.08, the cooldown on

`hysteresis` went from 1.15 to 1.05, `fatigueGain` from 0.04 back to the prototype's 0.08, and the
blocked-direction cooldown was switched on at `blockedFatigue` 0.35, `blockedMs` 800. `decisionMs`
and `holdMs` stayed at 800. `docs/design/room-escape.md` section 3 asked for it and
`infra/docs/room-escape.md` holds the measurement that chose the values: this time not a random
walker from a cold boot but the live release fly, run forward from its own generation-525
checkpoint, because the bug the change fixes is invisible to both a fresh boot and a walker.

Out of Red's house within ten brain minutes, fifteen runs per row from that checkpoint:

| `holdMs` | `fatigueGain` | `hysteresis` | `blockedFatigue` | out of the house | inside 5 min | median |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 800 | 0.04 | 1.15 | off | 1/15 | 0/15 | 573.5 s |
| 800 | 0.08 | 1.15 | off | 11/15 | 4/15 | 314.6 s |
| 800 | 0.08 | 1.15 | 0.35 | 7/15 | 4/15 | 282.7 s |
| 800 | 0.08 | 1.00 | off | 8/15 | 3/15 | 330.2 s |
| 600 | 0.08 | 1.05 | off | 8/15 | 4/15 | 276.7 s |
| 800 | 0.08 | 1.05 | off | 13/15 | 6/15 | 312.3 s |
| **800** | **0.08** | **1.05** | **0.35** | **14/15** | **13/15** | **130.3 s** |

Three mechanisms, and they only make sense together.

- **The hysteresis was the lock.** On a settled network the four direction scores sit inside a 9%
  spread, so a challenger can never lead by 15% and the winner can only ever be changed by fatigue.
  1.05 is a margin the spread can actually overcome. 1.00, no commitment bonus at all, is worse than
  either (8/15): the hold is a commitment device and removing the last of the bonus wastes it.
- **The fatigue gain sets the length of a run of holds, not of one hold.** At 0.04 one direction won
  4.1 consecutive holds — 3.3 s, about twelve tiles at 268 ms a step, in a room eight tiles wide —
  and 62 to 64% of holds moved the fly nowhere at all. 0.08 halves that to 2.5 holds.
- **The cooldown only pays off once the hysteresis is low.** At 1.15 it made things worse, because a
  fly pushed off walls faster orbits the perimeter faster and Red's staircase is on the perimeter.
  At 1.05 the fly turns inward instead, and the cooldown stops each crossing being spent on a wall:
  time on the two staircase tiles falls from 16,268 frames to 3,642 over the fifteen runs.

Confirmed on a fresh boot with the real brain, five runs each: 5/5 out of the house from the ground
floor (median 53.2 s, 4/5 inside five minutes) and 5/5 from the bedroom (median 83.8 s, 4/5 inside
five minutes), leaving the bedroom itself in a median 20.3 s against v0.1.0's 30.3 s. The random
walker, which cannot exercise the cooldown, still leaves both rooms 20/20 at these timings and
reaches the outside from the bedroom in a median 122.2 s against 162.1 s at v0.1.0's values.

Nothing about the decoder's state changed, and readout state is not part of the neural checkpoint,
so `DecoderState`'s version 4, the kernel version strings and the compatibility string are all
untouched: the live fly's checkpoint loads and simply decodes on the new numbers. What did have to
move is the golden `agent` scenario (`services/flysim/golden/agent.flygold`), whose per-frame button
masks are the preset's own output, and both `legacyGameboyConfig` twins, which now spell out the
prototype's 1.15 and switch the cooldown off — the `MotorDecoder` oracles prove the decoder's logic
against the prototype's numbers, and those are no longer the preset's.
## Platformer preset

`platformerDecoderConfig()` (`src/readout/presets/platformer.ts`, and its Rust twin
`flybrain-core::decoder::platformer`) is the same eight channels, bits and roles with different
timings. Every value is argued from the Super Mario Land disassembly in
`docs/design/platformer.md` §4; the summary is that a side-scroller needs commitment without gaps,
a real jump, a held B, and no pausing.

Exclusive group, `decisionMs` 250, `holdMs` 250, `hysteresis` 1.25, `fatigueGain` 0.04,
`fatigueDecay` 0.85. `clearLockoutMs` is 300.

| Channel | Kind | Role | Bit | Hold | Cooldown | Threshold | Boot | Throttle |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `right` | exclusive | `command_3` | 0x08 | 250 ms | | | | |
| `left` | exclusive | `command_2` | 0x04 | 250 ms | | | | |
| `down` | exclusive | `command_1` | 0x02 | 250 ms | | | | |
| `up` | exclusive | `command_0` | 0x01 | 250 ms | | | | |
| `a` | pulse | `command_4` | 0x10 | 300 ms | 420 ms | 1.10 | | |
| `b` | pulse | `command_5` | 0x20 | 600 ms | 200 ms | 1.05 | | |
| `start` | pulse | `command_6` | 0x40 | 55 ms | 600000 ms | 2.0 | 2500 ms, 1 | `system` |
| `select` | pulse | `command_7` | 0x80 | 55 ms | 600000 ms | 2.0 | 2500 ms, 1 | `system` |

Three consequences worth stating plainly, because they are not visible in the numbers:

- `holdMs === decisionMs` means a winning direction never gaps: the re-decision lands exactly as
  the hold expires. Momentum in Super Mario Land decays whenever no direction is held, and momentum
  is what clears gaps.
- `b`'s cooldown is *shorter* than its hold, so a channel whose score stays above threshold refires
  the instant the hold expires. That is a sustained hold built out of a pulse channel, with no
  decoder change, and it releases within 600 ms of the score dropping.
- Both system channels share one throttle group, so Start and Select can never be held in the same
  frame. Super Mario Land soft resets when A, B, Select and Start are held together, so the reset
  combination is structurally unreachable rather than merely unlikely.

`right` is first in `channels`, so the tie-break above favours rightward travel. That is a fixed
prior, disclosed on the honesty panel rather than hidden.

The two presets share `GAMEBOY_BUTTONS`, `GAMEBOY_BUTTON_BITS`, `toButtonMask` and
`fromButtonMask`; `presets/platformer.ts` re-exports them. The adapter picks a preset by returning
a `DecoderPresetId` (`flybrain-gb`), so the sim loop never names a game.

## State and legacy import

`exportState(): DecoderState` writes `version: 4`, `calibrated`, `baseline`, `heldUntil`,
`nextAllowed`, `nextDecision`, `current` and `fatigue`, plus the macro group's `macroNextDecision`,
`macroCurrent` and `macroFatigue` (2026-09-16).

The version stays **4**. Those three fields are additive and optional on import: a checkpoint
written before the macro group existed carries none of them, and the group then starts rested — no
winner, every fatigue zero, its clock at 0 — which is the same rule the network's new rates follow.
Macro channels are likewise exempt from "every configured channel is present in `heldUntil` and
`nextAllowed`", for the same reason and no other: a present macro field is validated exactly as its
direction twin is.

`importState()` accepts version 4 and the prototype's `MotorDecoder` checkpoints, versions
`undefined`, 2 and 3, whose channel names match the configuration:

| Version | Mapping |
| --- | --- |
| 4 | fields as written |
| 3 | `nextDirectionDecision` to `nextDecision`, `direction` to `current`, `fatigue` required |
| 2 and undefined | same renames, and fatigue starts at zero because these predate habituation |

Any other version number is rejected with `Invalid decoder version`. Every field is validated
before anything is written, so a rejected checkpoint leaves the decoder untouched:
`calibrated` must be a boolean, `nextDecision` finite, every value in `baseline`, `heldUntil` and
`nextAllowed` finite, every direction and pulse channel present in `heldUntil` and `nextAllowed`,
every fatigue value finite in `[0, 1]` for versions 3 and 4, and `current` either null or a channel
of the exclusive group. `macroNextDecision`, when present, must be finite; every present
`macroFatigue` value finite in `[0, 1]`; `macroCurrent` either null or a channel of the macro
group.

### Channels added after a checkpoint was written (2026-09-17)

A checkpoint carries `baseline` for the roles the decoder had **when it was written**. A group added
since — the macro channels are the case this was found on — has no entry, and an absent baseline
reads as zero, so the score

```
(rate + 1) / (baseline + 1)
```

becomes `rate + 1`: the channel competes on its **raw rate** against channels normalized to about
1. Population rates are not comparable that way. Live on 2026-09-17, forty-eight minutes in Oak's
lab with `TALK` never chosen once, the macro populations read

| role | rate |
| --- | ---: |
| `macro_next` | 145 Hz |
| `macro_npc` | 107 Hz |
| `macro_objective` | 65 Hz |
| `macro_frontier` | 54 Hz |
| `macro_talk` | 41 Hz |
| `macro_run` | 27 Hz |

`macro_talk` at 41 cannot beat `macro_frontier` at 54 in its one facing window, whatever the fly
does. Nothing was wrong with the populations: the decoder had never been told what rest looks like
for them, because `calibrate()` runs once at warm-up and the live state was restored from a
checkpoint that predates the roles.

**The rule.** On import, the roles the checkpoint carried no baseline for are recorded as pending.
The **first `decode` after the restore** calibrates them from its own rates, before any score is
computed, so every such channel scores exactly `1.0` on that decision — at rest, which is the
honest reading of a channel nobody has measured — and from the next decision on the group decides
on merit against a baseline of its own.

- **One sample, not a window.** That is what warm-up does (`NeuralAgent.warmup`: settle for
  `warmupMs`, then `calibrate` on one snapshot of the settled rates), so the restore rule is the
  same rule at the same fidelity, applied to the channels warm-up never saw.
- **It is transient.** `pendingBaseline` is not part of `DecoderState`; it is derived at import and
  consumed by the next decode, so the schema version stays **4** and no checkpoint changes.
- **`calibrate()` clears it**, because a full calibration is a better answer than a pending one.
- **The caveat, named:** a channel that happens to be firing above rest at that first decode gets a
  high baseline and is under-rated afterwards. Warm-up avoids this by settling first, which a
  restore cannot do — the network is mid-run. A population rate is already an average over a
  window, so one sample is a reasonable estimate of rest; if this ever bites, the fix is to
  re-calibrate rather than to widen the window, because a window would leave the channels competing
  on raw rate while it filled.
- **Visible next time:** `GET /status` reports `decoder` — `calibrated`, `pending`, and one row per
  channel with its role, baseline and last score. Diagnostic and additive; nothing reads it back.

Pinned by `a_restored_group_with_no_baselines_scores_every_channel_at_rest_then_on_merit` in both
twins and by the `restore` golden, which compares the scores of every channel at every step of a
restored run.

## The readout is fixed

The decoder calibrates once and then applies fixed ratios, timings and thresholds. No channel gain
adapts, no task state enters scoring, and nothing in this module learns. Habituation is bounded
motor history, not reward learning. The macro group changes none of that: it scores `macro_<type>`
rates by the same formula on the same timings, and the only thing that can make one macro win more
often than another is the reward rule acting on the mushroom body upstream of it ([rewards and
learning](rewards-learning.md)). Learning lives in the network, on the plastic edges described in
[plasticity](plasticity.md). Source: `readout/decoder.ts` module comment, and
`fly-plays-pokemon/docs/ratchet-sprint.md`, "Controller".

## Display approximation (flystage)

`apps/stage`'s NAMED CIRCUITS panel draws a faint tick at each pulse channel's decision threshold
(A/B at 1, Start/Select at 1.35 after boot, from the table above), computed by inverting this
page's own score formula: `rate = threshold * (baseline + 1) - 1`. The page cannot use the real
`baseline` this module calibrates with — `FeedHeader` (`docs/feed-protocol.md`) never carries it —
so it substitutes a running median of the role's rate as a stand-in "resting level". This is a
display-only approximation: it does not feed back into `decode()` or any other part of this
module, and it can disagree with the real calibrated baseline (most visibly right after a restore,
before the median has had time to settle). See `apps/stage/src/lib/circuit-scale.ts`
(`RunningMedian`, `thresholdRateHz`) for the implementation.

Bar *scale* is a separate, unrelated approximation: each bar's fill is a fraction of its own
adaptive running-max reference (`CircuitScale` in the same module), not a fraction of this
module's `baseline`. A bar reading "60% full" says nothing about where that role sits relative to
its decoder score — only the threshold tick does that, and only for the two groups listed above.
