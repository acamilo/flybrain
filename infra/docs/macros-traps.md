# Macro traps: the Viridian loop, and the audit beside it

Reproduced and fixed 2026-09-17 on the WSL development box, from the release box's own checkpoint
(`.local/checkpoints/release-viridian-loop.checkpoint`, never committed). `docs/design/macros.md`
section 12 is the contract throughout: **knowledge lives inside macros, never in the choice**, and
every fix below is a change to what a macro's *target choice* considers, or to which buttons a
scene deals — never to a ranking, a prior or a fallback action.

The decoder, the reward catalog, the adapter version and the compatibility string are untouched.
`flysim --print-compatibility` is byte-identical to v0.3.1:

```
lif-1ms-f64-v2/pokered-unique8-v5/75ba5d35…/fly-kc-mbon-rstdp-v2/binjgb:c60e138d…/pokered:0cd19d3b…/statefmt:199616-x86_64-unknown-linux-gnu
```

## What was live

Rank 8 (VIRIDIAN CITY, next VIRIDIAN FOREST), 2 h 17 min on the rung, ratchet rollbacks 3/3 spent,
stall meter 7:14 over a 2:00 window. The fly stood by a building door next to a girl, and the event
log repeated every ~3 brain seconds:

```
GO NPC start, GO NPC done (150 ms), GO OUT start, GO OUT done (170 ms),
NEXT start, NEXT done, GO FRONTIER start, GO FRONTIER done (420 ms)
```

The pad showed only GO ROUTE and GO FRONTIER; GO OBJECTIVE was gone.

## The mechanism

Six facts, in the order they compound. The frame counts are brain milliseconds at the emulator's
16.74 ms per frame, so 150 ms is nine frames and 420 ms is twenty-five.

1. **A walk across a town is longer than the frame cap.** `FRAME_CAP` is 600 frames (10 s) and a
   tile costs `TILE_FRAMES + STEP_GAP` = 30 frames at worst, so a walk from the south of Viridian
   to its north edge cannot finish inside one macro. Every `GO ROUTE` aimed at the Route 2
   connection ended `Timeout`.
2. **A `Timeout` wrote the blocked ledger.** Section 12.1 triggers the ten-brain-minute exclusion on
   "aborts `Blocked` or `Timeout`", so the connection the fly was half way to was excluded as
   *unreachable* — which is not what a frame cap measures. (It is now three strikes for a walk that
   ends nearer its goal, and immediate exclusion for one that does not: see row 1 and row 23 for why
   the first, simpler reading of this did not survive its own measurement.)
3. **That is why GO OBJECTIVE left the pad.** Not the rollback, and not "already reached": rung 9's
   place is `VIRIDIAN_FOREST`, `geography::next_hop` makes the first hop `ROUTE_2`, and the only
   exit taking it is the north edge — the exit the ledger had just excluded. `objective_goals`
   filters the blocked ones, came back empty, and `precondition(GoObjective)` is
   `!objective_goals(..).is_empty()`. The two macros aim at one `TargetKey::Exit`, so poisoning it
   for one poisoned it for both.
4. **GO ROUTE then fell through to its last-resort tier.** Tier 1 (a destination this run has not
   stood on) was empty once the connection was excluded and the town's interiors had been entered;
   tier 2 (toward the objective) was empty for the same reason; tier 3 was "all of them, nearest",
   and nearest is the house door the fly was standing at. Section 9.2 fixed *which* exits count as
   visited and left that fallback standing.
5. **GO OUT bounced straight back out.** Inside, `exit_goals` keeps a doormat underfoot — "its press
   is the point" — and the interior's front door is written `LAST_MAP`, which `Exit::destination`
   could not resolve because `macros::geography` has no row for an ordinary Viridian house. An
   unnameable destination counted as *unvisited*, so `GO OUT` was permanently first-tier fresh,
   aiming at the tile under the fly's feet: `Arrival::Leave` presses DOWN, the map changes, and the
   macro is `Done` in ten frames. The same bounce runs on a staircase, which is what the local
   reproduction showed: `GO WARP` up, `GO OUT` straight back down and out, once per hold.
6. **Nothing in the cycle recorded anything.** The macros all ended `Done`, so no blocked entry was
   written; and `GO NPC`'s `Done` came from the *scene-change* rule (`completed = false`) as the
   warp fired under it, which is exactly the path that does **not** write the reached ledger — so
   12.1's "a reached person is retired" never fired for the girl by the door. `GO FRONTIER`'s
   arrival press faced new ground it could not stand on, which the contract calls `Done` and which
   leaves the tile a frontier for ever, because the exploration ledger only records ground somebody
   stood on. Four macros, ten to twenty-five frames each, nothing learned, once per hold.

**Why GO OUT appeared on an outdoor map: it did not.** `path::exits` classifies every warp of an
outdoor map as `Way::Route`, so `GO OUT`'s precondition is false outdoors and it can only ever be
dealt indoors. The log interleaved two maps: the cycle crossed a warp twice every three seconds, so
`GO OUT` (and, in the local reproduction, `GO WARP`) came from the indoor half of the same three
seconds and the pad the operator read — GO ROUTE, GO FRONTIER — from the outdoor half.

**The ratchet could not help.** `Ratchet::budget_spent` is checked before either trigger, so at 3/3
attempts on one best rank no recovery fires again until the rank *improves*, which a loop cannot do.
The stall meter keeps measuring and `observe` keeps returning false — and separately, `GO FRONTIER`
netting one new tile every few minutes bumps `coverage`, which resets `last_progress` and disarms
the stall trigger on its own. The macro layer has to break its own loops.

## The audit

Every macro and every scene binding, one row per way the fly can be left cycling or stranded.
"Test" names the fake-game test in
`services/flysim/crates/flybrain-gb/src/pokemon_red/macros/tests.rs` unless it says otherwise.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 1 | `Timeout` excludes the target of a walk that was making progress | a walk longer than 600 frames; takes `GO OBJECTIVE` off the pad with `GO ROUTE` | `a_timeout_costs_a_closing_walk_a_strike_and_a_stalled_one_its_target` | **fixed**: a `Timeout` that ended *nearer* a goal costs one of `TIMEOUT_STRIKES` (three, section 4's number at the macro's scale) and the next hold resumes the walk; one that ended no nearer excludes the target at once |
| 2 | `GO ROUTE`'s last-resort fallback re-picks the nearest door for ever | every route leads somewhere visited and none is toward the objective | `go_route_prefers_a_door_whose_interior_this_run_has_not_seen` | **fixed**: no tier 3 for `Way::Route`; the button leaves the pad. `GO OUT` keeps it, because a room has to be leavable |
| 2b | the same bounce on the stairs: up, straight back down, up again | a floor whose staircase leads somewhere the run has been | `a_way_out_is_visited_only_when_the_map_on_the_other_side_is`, `a_front_door_nobody_can_name_is_not_somewhere_new` | **fixed**: no tier 3 for `Way::Passage` either — except on a map with no other way out (Red's bedroom, an upper floor), where dropping it would strand the fly |
| 3 | a front door nobody can name reads as somewhere new | any building with no row in `macros::geography` — most houses | `a_front_door_nobody_can_name_is_not_somewhere_new` | **fixed**: an unresolvable `Way::Exit` counts as visited; the only way onto an interior map is through its own front door |
| 4 | `GO NPC` / `GO ITEM` completes without moving | the fly already stands beside the target and faces it | `a_thing_the_fly_is_already_facing_is_talks_and_not_a_walks` | **fixed**: `untalked()` drops the tile-ahead target; `TALK`'s precondition is exactly that state, so the press is what the pad offers |
| 5 | `GO FRONTIER` completes without moving, for ever | the arrival press cannot enter the new ground (a ledge, a tile-pair rule, a person) | `a_frontier_tile_whose_new_ground_cannot_be_stood_on_is_excluded` | **fixed**: `Walk::stalled` writes the blocked ledger on an otherwise-`Done` arrival |
| 6 | a `no route` refusal records nothing and repeats once per hold | a bound walking macro whose goals are all unreachable (the precondition is the cheap question) | `a_no_route_refusal_records_what_it_could_not_reach` | **fixed**: `script` reports every goal key it could not reach, `start` writes them, and the driver drains after *every* start, refusal included |
| 7 | own turn with an empty pad | trainer battle, no move with PP, no healthy reserve, no potion: `ATTACK`/`SWITCH`/`ITEM`/`RUN` all drop | `no_playable_scene_deals_an_empty_pad` | **fixed**: `NEXT` closes the own-turn row, and `ITEM` joins it as section 12 lists it |
| 8 | forced switch with an empty pad | `healthiest_other()` is `None` in a menu the game will not let the fly cancel | `no_playable_scene_deals_an_empty_pad` | **fixed**: `SWITCH, NEXT` |
| 9 | `Unknown` offers one A press and A cannot leave | the Pokédex, the trainer card, OPTION — all land in `Unknown` (`docs/design/macros-wram.md`) | `a_forced_switch_plans_one_entry_and_the_other_scenes_plan_their_one_move` | **fixed**: `NEXT, BACK`; B is what leaves all three |
| 10 | a yes/no box can only ever be answered YES | every prompt in the game; `YES`/`NO` were in no plan at all | same | **fixed**: `Dialog` deals `NEXT, YES, NO`. See row 21 for what is still approximate about it |
| 11 | the start menu cannot be closed by anything but B, and `CONFIRM` is on no pad | `Menu` planned `CLOSE` alone | same | **fixed**: `CLOSE, CONFIRM, BACK`, which is section 12's own set |
| 12 | eight overworld buttons into six rows cut `GO FRONTIER` | a room with a person, an object, stairs and a known objective | `the_overworld_plan_never_truncates_the_frontier_away` | **fixed**: the tail survives; the middle gives way |
| 13 | the warp bounce itself: `GO OUT` from the mat it walked in over | standing on an interior doormat | covered by rows 2 and 3 (`trap_hunt` before/after) | **left**: it does move the fly to another map, and the contract keeps a doormat underfoot; the loop is broken on the way *in* instead |
| 14 | the name-entry / nickname screen | catching a Pokémon, then A = YES on "give a nickname?" | — | **left, contract call**: it reads `Unknown` and needs START or the "ED" cell; `NEXT`/`BACK` cannot leave. Putting `MENU` (START) on the `Unknown` pad is a section 12 change, not a macro one |
| 15 | `YES`/`NO` are dealt for every text box, not only an open choice | any plain dialog | row 10's test | **left, no observable**: pokered has no "the game wants a button" flag (`macros-wram.md`), and A and B both advance a plain box, so nothing is lost |
| 16 | the mart cannot buy | `MacroState::shop_stock` defaults empty over live WRAM | `the_shop_row_needs_money_and_stock` (existing) | **left**: the seam's own narrowing; the two buttons are now planned and bind the day the stock list is read |
| 17 | the PC can only be left | section 12's PC set is `LEAVE` | — | **left, contract**: nothing in the macro vocabulary deposits or withdraws |
| 18 | `MENU` is on no pad at all | the overworld row is full at six | — | **left**: no room at `SLOTS = 6`; raising it is a feed (`game.palette[].slot`) and stage change |
| 19 | a cursor macro waits 180 frames and reports `Blocked` | `ATTACK` or `RUN` started in the last frames of `own_turn`: `listing()` is `None` for most of a battle | — | **left**: one wasted hold, no loop — the turn moves on, and the macro carries no target to poison |
| 20 | a rollback writes neither ledger | the ratchet fires mid-walk | existing `driver.rs` tests | **left, contract** (12.1: "a rollback's cancellation writes neither — that is the loop's doing, not the map's") |
| 21 | the session ledgers do not survive a checkpoint | a restart re-offers every target once | — | **left, contract** (12.1: "a restored run offers every target once more") |
| 22 | the ratchet stops helping at 3/3 attempts on a rung | `budget_spent()` precedes both triggers; and one new tile resets the stall window | `ratchet.rs` tests | **left, contract**: "deliberately conservative and can leave an unproductive run unrecovered". It is why the macro layer must break its own loops, and why `trap_hunt` exists |
| 23 | A* oscillation into `Walkable::Unknown` | a walk that keeps moving but re-plans back and forth over the screen-buffer edge | `a_timeout_costs_a_closing_walk_a_strike_and_a_stalled_one_its_target` | **fixed by the strike count**, which is why it is strikes and not "did the player move": that first reading traded the Viridian loop for this one, twelve timeouts every two brain minutes over two tiles, measured in the first after-run |
| 24 | a walk that can only get *closer* to an unreachable goal carries no target | `GO FRONTIER`, whose goals are one key each, so "the key every goal shares" is `None` and the search returns its closest-approach route | `a_walk_that_can_only_get_closer_still_names_the_target_it_set_out_for` | **fixed**: the nearest goal is what the heuristic drove the walk toward, so it is what the walk set out for — without it the timeout recorded nothing and the same walk was available again on the next hold |
| 25 | the mart cannot buy (row 16, reopened) | `MacroState::shop_stock` defaulted empty over live WRAM | `the_four_purchases_are_bound_by_the_counters_own_stock` | **fixed**: `wItemList` is the open counter's own list and an item's place in it is its cursor index (`docs/design/macros-wram.md` section 7). Viridian stocks no Potion, so `BUY POTION` is correctly off the pad in the first mart the fly walks into |
| 26 | a mart clerk and a nurse cannot be reached at all | every one of the four tiles around either of them is wall or floor behind the desk | `a_counter_is_the_only_way_to_reach_a_clerk_or_a_nurse` | **fixed**: `wTilesetTalkingOverTiles` and `IsSpriteOrSignInFrontOfPlayer`'s counter branch — the tile *two* away, facing in, is a place to talk from. Without it `GO SHOP` refuses `no route` once per hold for ever |
| 27 | the errand loops: the fly walks into the mart, out, and in again | an errand keyed on anything but "this run has been inside" | `the_errand_is_outstanding_once_per_area_per_run` | **fixed**: `areaVisited(kind, area)` is written on *arrival* and never cleared, so the errand is paid once per area per run. A purchase marks nothing |
| 28 | `RUN` is bound for every wild battle, so the fly flees fights it is winning ("we run away a lot") | any wild battle at all | `run_is_off_the_pad_in_a_wild_battle_the_fly_is_not_losing` | **fixed**: section 13.1's `losing` — under a third of HP with no healthier reserve, or no PP anywhere with nothing to switch to. A fled battle pays nothing and teaches nothing |
| 29 | the bag opened from a battle's ITEM entry has no cursor to read | choosing ITEM in a battle | `item_reaches_the_potion_by_reading_the_bags_cursor` | **fixed**: `BattleMenu::Bag` (`macros-wram.md` 7.1). `ITEM` used to open the bag, wait out `CURSOR_WAIT` pressing nothing, and report `Blocked` — every potion the fly ever chose ended that way |
| 30b | the pad has a button that cannot move the fly anywhere ("the macro buttons disappear and everything just hangs there") | every route visited, every person and object excluded, the near frontier covered, no objective hop | `a_map_with_every_ledger_against_it_still_offers_a_way_out`, `the_last_resort_prefers_the_exit_toward_the_objective` | **fixed**: `ways`' last resort offers the visited exit toward the objective (ignoring the blocked window), and `frontier_aims` falls back to the nearest unstood tile of the whole map. Reached only when *nothing else* is on the pad, so row 2's loop does not come back |
| 31 | an empty pad in a playable scene is indistinguishable from a hang | any of the causes listed in section 13.1 | `the_empty_pad_clock_runs_only_while_a_playable_scene_binds_nothing`, `a_running_macro_is_not_an_empty_pad` | **measured, not acted on**: `game.padEmptyMs` and `fly_pad_empty_seconds`. Nothing presses for the fly, so waiting is the doctrine working; what was missing was being able to tell it from a stall |
| 32b | rows 12 and 18 are retired | — | `the_overworld_plan_never_truncates_the_frontier_or_the_menu_away` | **retired by section 14**: a slot per macro type, so nothing is truncated off a pad and `MENU` is on every overworld |
| 33b | `TALK` cannot reach over a counter | standing at a mart's or a centre's counter facing the person behind it | `a_counter_is_the_only_way_to_reach_a_clerk_or_a_nurse` | **fixed**: `facing_target` looks two tiles ahead over a counter. It looked one, found the counter tile, answered nothing -- so `GO SHOP` could walk the fly to a counter and the press that opens it was not on the pad there. The two macros between them could reach a mart and not buy in it |
| 34b | the errand is spent on the building's own doormat | arriving in a mart or a centre, where `exit_goals` keeps a mat underfoot (row 13) and any walk that presses DOWN warps out | `go_shop_enters_the_mart_once_and_walks_to_its_counter` (ROM-gated) | **fixed**: while an amenity's counter is unfaced, nothing on its pad leaves the building (`palette::counter_pending`) -- the same rule row 29 gave the rung's own target. The measured culprit was `GO OBJECTIVE`, not `GO OUT`: the rung's place is on another map, so the objective's goal was the mat the fly was standing on. The errand is paid once per area per run, so that bounce spent the one visit and nothing could bring the fly back |
| 35 | the screen-buffer tile read disagrees with itself on a map smaller than the screen | any interior smaller than ten tiles by nine -- most houses, both amenities | `a_counter_is_the_only_way_to_reach_a_clerk_or_a_nurse` (the map-id half) | **worked around, not fixed**: the same tile of the Viridian mart reads walkable-and-counter from one of the fly's tiles and wall-and-not-counter from another two tiles away, because an 8x8 map cannot centre under a ten-by-nine view and the player is no longer at the buffer's fixed point. The counter *reach* is taken from the map id instead. The predicate itself is unchanged and is still the walk's pricing, which is why the purchase below is not ROM-proven. **This is the next thing to measure** |
| 37 | **the private-property tile**: a text box that reopens every frame the fly stands on one tile, for most of a run | standing on (19, 9) of Viridian City without the Pokédex — `ViridianCityCheckGotPokedexScript` prints "This is private property!" and walks the player down, *every frame* — and that tile is one of the four `approach` offers around the sleeping old man at (18, 9) | `a_tile_the_cartridge_pushes_the_fly_off_is_not_a_tile_to_walk_to`, `a_scripted_push_back_records_the_tile_it_happened_on` | **fixed**: a session ledger keyed on the **tile**, with no window, that every aim *and the route* filter on. Measured 53,266 of 54,377 text-box frames in twenty brain minutes on that one tile, 991 answered `YES`. The blocked ledger could not close it — it is keyed on the *target*, so it excluded a villager for ten brain minutes, said nothing about the ground, and reopened |
| 37a | the same tile, still crossed | excluding it as a *goal* only | the route half of the same test | **fixed**: `route_avoiding` treats it as impassable. The goal filter alone took (19, 9) from 53,266 text-box frames to 12,919, because the A* went on routing *across* it toward somewhere else and the script fires on any frame the fly stands there. With the route filter it is 1,538 |
| 36 | a counter faced is lost to the next hold | `GO SHOP` ends at the counter looking at the clerk; a hold is 800 ms and whichever macro wins the next one turns the fly away | — | **left, bounded**: `TALK` is on the pad there, and the `reached` window brings `GO SHOP` back in ten brain minutes, so it is one wasted approach per window rather than a loop. It is why the purchase is unit-proven and not ROM-proven, and closing it would mean a macro that presses A at what it walked to -- which is `TALK`'s and the fly's (section 3, 2026-09-16) |

Fifteen fixed, eight left — six of those eight because the contract says so or there is nothing in
WRAM to ask (14, 15, 17, 20, 21, 22), and two (13, 19) because they are bounded and the fix would be
bigger than the trap. Rows 1, 2b, 23 and 24 were each found by running the hunt after the previous
fix: three of the five loops in this file only became visible once the one in front of them was
gone.

**2026-09-17, sections 13 and 14.** Rows 25 to 32b above. Row 16 is reopened and closed (the mart
can buy), row 19's cursor wait is closed for the one list it actually bit on (row 29), and rows 12
and 18 are retired by the slot-per-type pad. Rows 25 to 29 came out of the scene audit asked
for — "check to make sure each scene has the full complement of options" — and rows 30b and 31 out
of "sometimes the macro buttons disappear and everything just hangs there", which is the first trap
in this file that is *correct behaviour* the audience cannot tell from a fault. It is measured
rather than acted on, because nothing presses for the fly.

## The trap hunt

`services/flysim/crates/flysim/examples/trap_hunt.rs` runs macros mode from a checkpoint for N
brain minutes over the sim loop's own frame order and flags every two-brain-minute window (one
window every 15 brain seconds, so a loop is caught wherever it starts) in which either

- the fly stood on fewer than **4** distinct `(map, tile)`s, or
- one macro sequence repeated more than **10** times, at any period up to 8.

A window with no macro in it is not flagged: silence waits, and that is the doctrine working.

```sh
FLY_ROM=".../Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_TRAP_CHECKPOINT=.local/checkpoints/release-viridian-loop.checkpoint \
  FLY_TRAP_MINUTES=20 cargo run --release -p flysim --example trap_hunt
```

20 brain minutes from the Viridian checkpoint, seed 20260917, 4 sweep threads, same connectome and
same cartridge in both runs.

### Before (main at v0.3.1, `d53f278`)

| measure | value |
| --- | ---: |
| distinct (map, tile) over 20 brain minutes | 152 |
| macros started | 1374 |
| done / blocked / timeout / refused | 1365 / 4 / 5 / 4 |
| windows flagged | **66** of 73 |
| windows under 4 distinct tiles | **29** |
| worst window | 2 tiles, 150 macros |
| longest repeat | `GO FRONTIER, GO WARP, GO OUT, NEXT` **x37** |

The first and last flagged windows of the run, as the example printed them:

```
| at (brain min) | tiles | macros | repeated sequence |
| 0.25 | 105 | 72 | `GO FRONTIER, GO WARP, GO OUT, NEXT` x11 |
| 1.75 | 5 | 149 | `NEXT, GO FRONTIER, GO WARP, GO OUT` x37 |
| 8.00 | 5 | 149 | `GO OUT, NEXT, GO FRONTIER, GO WARP` x36 |
| 12.50 | 2 | 149 | `NEXT, NEXT, NEXT, NEXT, GO ITEM, NEXT, NEXT` x11 |
| 18.00 | 2 | 149 | — |
```

Thirty-seven of the flagged windows are one rotation or another of that four-macro cycle, and the
run ends stuck: from 11 brain minutes on, every window holds **two** tiles and about 149 macros, with
a second cycle of `NEXT … GO ITEM … NEXT` inside it. No recovery fired — the ratchet was at 3/3 on
rung 8 when the checkpoint was taken, so `budget_spent` refuses before either trigger.

### After (`fix/macros-traps`)

| measure | value |
| --- | ---: |
| distinct (map, tile) over 20 brain minutes | **194** |
| macros started | 291 |
| done / blocked / timeout / refused | 186 / 5 / 99 / 1 |
| windows flagged | 51 of 73 |
| windows under 4 distinct tiles | **1** |
| worst window | 2 tiles, 12 macros |
| longest repeat | `GO FRONTIER` x19 |

The same slice of the after-run:

```
| at (brain min) | tiles | macros | repeated sequence |
| 1.50 | 93 | 28 | `GO FRONTIER` x12 |
| 2.75 | 19 | 19 | `GO FRONTIER` x19 |
| 11.00 | 44 | 93 | `NEXT, GO FRONTIER, GO WARP, GO OUT` x12 |
| 13.00 | 64 | 72 | `GO FRONTIER, GO WARP, GO OUT, NEXT` x12 |
| 18.00 | 5 | 12 | `GO FRONTIER` x12 |
```

The hard cycle is gone: no window repeats a *sequence* of the four macros more than 12 times (it was
37), and only one window in the run holds fewer than four tiles (it was 29). A fifth more ground is
covered on a fifth of the macros — 291 against 1374 — which is the shape of the fix: the walks now
run to the frame cap and resume instead of finishing in ten frames and starting again.

Two residuals, both bounded and both left, named here rather than papered over:

- **41 windows are flagged for one macro repeating**: `GO FRONTIER` x11–x19, each one spending the
  full 600-frame cap. Viridian City is wider than ten seconds of walking, so a frontier tile across
  it takes several macros to reach; the windows that carry these repeats cover 5 to 93 tiles, so the
  fly is exploring rather than stuck. `TIMEOUT_STRIKES` is what bounds it: three caps on one target
  and the target is excluded for the ten-minute window.
- **10 windows still show the building cycle**, at x11–x12 rather than x37, and over 27 to 64 tiles
  rather than 2 to 5. The fly walks into a building, does something and comes out; it is a
  three-repeat-per-minute pattern with real ground under it, not the ten-frame bounce.

The sequence rule at period 1 therefore has a known false-positive shape — a legitimately repeated
explorer — and the tile rule is the one that separates the two. A future tightening is to flag a
repeat only when the window's tile count is also low; the thresholds here are the ones the operator asked
for and they are left as specified.

## Gates

- `cargo test --workspace` with `FLY_ROM` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on main at `d53f278` on this box — it asserts a 30 Hz publish rate and a
  debug build here reaches 10.2–10.4 Hz. Pre-existing, unrelated to the macro layer (that test runs
  `macros.mode = raw`).
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: ALL CHECKS PASSED.
- No TypeScript touched, so npm was not run.

## 2026-09-17 later: every walk in Viridian times out, and none completes

Reproduced and fixed the same day on the WSL development box, from the release box's own checkpoint
(`.local/checkpoints/release-viridian-timeouts.checkpoint`, never committed) — a second stall on the
same rung, four hours after the loop above was fixed and v0.3.2 deployed. `docs/design/macros.md`
section 12.3 is the contract for what changed.

### What was live

Rank 8 (VIRIDIAN CITY), 02:04 to 07:00 UTC, five hours with no rung and no completed macro. The
last 40 KB of the event log held nothing but starts and timeouts:

```
116 GO ITEM start      117 GO ITEM timeout
 36 GO ROUTE start      36 GO ROUTE timeout
 12 GO OBJECTIVE start  12 GO OBJECTIVE timeout
```

No `done`, no `blocked`, no `refused`, and nothing else at all. The gaps between bursts are the ten
brain minutes of the blocked ledger: three caps on one target excluded it, and with every candidate
excluded the pad was empty until the window expired.

### The mechanism

Measured rather than reasoned, from the checkpoint over the sim loop's own frame order, with the
per-macro rows `examples/trap_hunt.rs` now prints:

| macro | outcome | n | mean frames | mean tiles | mean net | mean reach |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| GO ITEM | timeout | 36 | 600 | 2.0 | 0.0 | 1.0 |
| GO OBJECTIVE | timeout | 2 | 600 | 2.0 | 0.0 | 1.0 |
| GO ROUTE | timeout | 6 | 600 | 2.0 | 0.0 | 1.0 |

Forty-four walks, forty-four timeouts, **every one of them over exactly two tiles with a net
displacement of zero and a maximum reach of one tile**. So the cap was not short of the map: the fly
never got more than one tile from where each walk began. Three facts compound.

1. **The closest-approach answer walks toward what it cannot reach.** With no goal reachable,
   `path::route` returns the route to the tile that gets nearest one, so the walk always had
   somewhere to go — and `GO ITEM`'s goals are the four tiles around one object, which in Viridian
   are behind the gym's fence, a ledge or a building for most of the city.
2. **The A\* re-planned after every tile.** The walkable predicate's window is the ten-by-nine
   screen buffer and it moves with the player, so the search's map changed under the walk's feet
   every step. The walk stepped to the closest-approach tile, the re-plan from the new tile answered
   with the tile it had just left, and the pair traded the fly back and forth until the cap. This is
   row 23 of the audit above, which three strikes *bounded* and did not fix.
3. **A frame cap that had been one tile nearer once read as progress.** `best_distance <
   start_distance` is true after a single step toward the goal, so 12.2's rule charged a strike
   rather than excluding anything. Three strikes, ten brain minutes of exclusion, and the same walk
   again — 165 times.

The 600-frame cap is a real second bound behind all this, and it is fixed too: Viridian City is
about twenty tiles by eighteen, a tile costs `TILE_FRAMES + STEP_GAP` = 30 frames at worst, and the
walk from the checkpoint's tile to the Route 2 connection is thirty-odd tiles. No amount of not
oscillating would have finished it inside ten seconds.

### What changed

All of it inside the macros; the decoder, the reward catalog, the adapter version and the
compatibility string are untouched (`flysim --print-compatibility` byte-identical to v0.3.2, below).
Section 12.3 has the contract; the audit table gains two rows.

| # | trap | trigger | test | fix |
| ---: | --- | --- | --- | --- |
| 25 | a closest-approach walk oscillates over the walkable window's edge for the whole cap, and the cap reads as progress | any goal walled off from where the fly stands: an item behind the gym fence, a north-edge tile behind trees | `a_goal_walled_off_from_the_fly_is_excluded_without_a_press`, `the_walk_commits_to_its_route_across_the_walkable_windows_edge` | **fixed**: a goal the full-map search cannot reach is excluded at `start` — nothing pressed, every goal key to the blocked ledger — and the route is planned once and followed while the player is on it, re-planned only when a step is refused, the plan runs out, or the player is not where the plan left it |
| 26 | a walk longer than ten seconds can never finish, and starts again from the fly's tile every hold | a town wider than `FRAME_CAP` frames of walking, which is every town | `a_walk_longer_than_the_old_cap_finishes_because_it_keeps_making_progress`, `the_walk_budget_is_the_plan_plus_a_floor_and_stops_at_a_minute`, `a_walk_the_ceiling_cuts_short_resumes_from_where_it_stopped` | **fixed**: the budget is 24 frames per planned tile plus `FRAME_CAP` as a floor, capped at 60 s of brain time; a walk still closing on a goal is not out of frames; the remaining route is kept per `(map, TargetKey)` and the next start carries it on from the tile it was suspended at |

Two smaller ones from the same reading, both in row 25's tests:

- **A refused step is a wall in the direction it refuses.** The survey method
  (`docs/design/macros-wram.md`) is what settles how the cartridge encodes these: `HandleLedges`
  matches a triple of facing, the tile stood on and the tile in front against `LedgeTiles` and hops
  the player two tiles, and the ledge tile is absent from every tileset's passable list — so
  `state::walkable` already calls a ledge `No` and the search treats it as a wall in both
  directions, declining the hop rather than getting it wrong. `TilePairCollisionsLand` and
  `...Water` are the ones nothing in the collision list can predict: they refuse a step *between*
  two tiles that are each passable. Those are measured — a step that spends its whole window with
  the player where it started becomes a directed wall for the rest of that walk, and the re-plan
  goes round. A person in the way has the same shape and costs the walk one detour.
- **A rollback drops a suspended route**, because a route is a list of directions from one tile and
  a rollback moves the fly off it (12.1: "a rollback's cancellation writes neither — that is the
  loop's doing, not the map's").

### The trap hunt, before and after

Same invocation as the section above, 20 brain minutes, seed 20260917, 4 sweep threads, same
connectome and same cartridge in both runs, from `release-viridian-timeouts.checkpoint`:

```sh
FLY_ROM=".../Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_TRAP_CHECKPOINT=.local/checkpoints/release-viridian-timeouts.checkpoint \
  FLY_TRAP_MINUTES=20 cargo run --release -p flysim --example trap_hunt
```

| measure | before (main, v0.3.2 `db393d9`) | after (`fix/macros-walks`) |
| --- | ---: | ---: |
| distinct (map, tile) over 20 brain minutes | 2 | **142** |
| macros started | 44 | 1,144 |
| done | **0** | **1,129** |
| blocked / timeout / refused | 0 / 44 / 0 | 0 / 14 / 1 |
| windows flagged | 39 of 73 | 50 of 73 |
| windows under 4 distinct tiles | 39 | 50 |
| worst window | 2 tiles, 12 macros | 2 tiles, 150 macros |
| walks that spent their whole cap | 44 of 44 | 14 of 1,144 |
| mean net displacement of a timed-out walk | 0.0 tiles | 6.4 tiles (`GO ITEM`) |

The stall is gone: **the walks complete.** A fly that covered two tiles in twenty brain minutes now
covers 142, on 1,129 completed macros where there were none, and the fourteen walks that still spend
their budget are long ones — 9 to 14 tiles of ground each, 6 to 8 tiles of net displacement — which
resume on the next hold instead of excluding what they were aimed at.

#### And the trap behind it: the scene reads `dialog` and stays there

The hunt's window count went *up*, and that is the honest reading of it rather than a regression in
the fix. From 5.75 brain minutes to the end of the run every window holds two tiles and about 149
macros, and the macros in them are `NEXT` (607) and `NO` (327) and nothing else — one A press and
one B press, for fourteen brain minutes. Every overworld walk in the run, and all 142 tiles of
ground, happen in the first 5.75 minutes. The scene tally says the same thing across the whole run:

| scene | frames |
| --- | ---: |
| dialog | 41,012 |
| overworld | 26,197 |
| unknown | 4,464 |

So the next trap is one of two things and this run does not settle which: a box whose text an A
press and a B press genuinely cannot advance (the shape of rows 14 and 15 above — no WRAM observable
says what a box is waiting for, and `MENU` (START) is on no pad at all), or a `dialog` reading that
is wrong, off map tiles that look like a box. **Settled the same day in the section below: it is the
first, and the box is a cartridge gate.**

> **Correction to a first reading of this, kept because it was written down.** The run ends in scene
> `dialog` with the adapter's mode reading `OVERWORLD`, and that was recorded here as the detector
> and the adapter disagreeing. They do not: `PokemonRedReward::mode` is `OVERWORLD` for every frame
> that is not a battle, a boot, a transition or the Safari — it is not a dialogue gate at all. The
> adapter's dialogue gate is `safe_for_snapshot`, which does read `wFontLoaded`, and it agreed with
> the detector throughout.

The ROM test measures the same wall from the other end: macros mode reaches the mart's door in 3.2
brain minutes on 27 macros and then never leaves Viridian City in **seven brain hours** — 1,500,000
frames, 30,946 macros, 30,921 of them `done`, ending at tile (19, 9) with 7,535 `NEXT`s, 7,515
`YES`es and 7,515 `NO`s spent on it. `GO OBJECTIVE` toward Route 2 is behind that wall, which is why
the test asserts the mart door and the completion of walks rather than Route 2.

Named here rather than worked around, and left for its own run: it is a scene-detection or a pad
question, not a walk one, and the walks are what this section fixed. That run is the section below.

### What this exposed, and did not fix

Row 29's proof was `EVENT_OAK_GOT_PARCEL` at 27,167 frames on 504 macros from this checkpoint. It
held for exactly one trajectory. The battle sub-states above change what the fly does in Route 1's
grass on the way to the errand — it chooses a move now instead of pressing A at the text — and the
rotation's phase inside Oak's lab moved with it: from the same checkpoint the fly reaches the lab,
presses `TALK` **ninety-four** times, and does not deliver in **900,000 frames** (16 brain hours),
bouncing 0↔40 with `GO OUT` running 753 times.

`GO OUT` running at all in the room the rung is in means row 29's suppression had lifted, and it
lifts when `objective_targets` empties — which the **talked ledger** does, because it filters the
objective's own target. So a fly that has talked to everyone in the lab once, without the delivery
firing, stops being held in the room. That is a gap in row 29 rather than in this fix, and the fix
here only changed which trajectory finds it.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 31 | the talked ledger retires the objective's own target, so the room stops holding the fly | a rung whose conversation did not fire the first time: the wrong person talked to, or a script with a precondition | — | **left, named and measured**: `objective_targets` filters `talked`, and 12.5 argued the *reached* ledger should not apply to the objective for exactly this reason. The same argument applies to `talked` and was not made: a rung's conversation is not "had" until the rung is earned. It needs its own run, because "talked and the rung still unearned" is also how a fly would grind an unearnable rung for ever |

The ROM test for row 29 is therefore narrowed to what the macros can be held to — the parcel carried
and undelivered, the lab reached by way of Pallet Town and never through the shut road, and `TALK`
taken in the room the rung is in — with the delivery measured rather than asserted. A game-blind
rotation does not aim, and one A press at one particular person is aiming; that claim belongs to a
run with a brain in it and a reward for the rung, which is the same thing
`macros_mode_leaves_the_house_and_reaches_oaks_lab` has recorded about the starter since 2026-09-16.

### Gates

- `cargo test --workspace` with `FLY_ROM` and `FLY_TRAP_CHECKPOINT` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on main at `db393d9` on this box: a debug build publishes at 9.46 Hz
  against the test's 30 Hz contract and the ladder is not loaded when `/status` is first read.
  Pre-existing and unrelated to the macro layer (that test runs `macros.mode = raw`). The macro
  suites are green: 278 in `flybrain-gb`'s lib and both ROM tests in `flysim`'s `rom_macros_mode`
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: ALL CHECKS PASSED.
- `flysim --print-compatibility`: the same 648 bytes as the string recorded at the top of this
  file, every component identical. The decoder, the reward catalog and the adapter version are
  untouched by this branch.
- No TypeScript touched, so npm was not run.

## 2026-09-17 later still: the dialog residual is a real box, and the corner test was thin

The section above left the residual as one of two things. Measured on the WSL development box from
the same checkpoint, with the real connectome driving macros mode, it is the first of them — and the
investigation found the second as a latent hazard rather than the cause, so both are recorded here.

### What the bytes say

`examples/trap_hunt.rs` now counts, once per frame, how the dialog branch's two halves agree:
`wFontLoaded`'s bit 0 (the WRAM flag `DisplayTextIDInit` sets and `CloseTextDisplay` clears) against
the drawn `TextBoxBorder` at screen (0, 12)-(19, 17), read both as the four corners the detector used
and as the whole figure. Twelve brain minutes, 43,004 frames:

| the dialog branch's two halves | frames |
| --- | ---: |
| font set, corners drawn, whole border drawn | 18,775 |
| font set, corners drawn, **border not** (a false `dialog`) | **0** |
| font set, corners not drawn (menu or unknown) | 0 |
| corners drawn, **font clear** (harmless: overworld) | **315** |

**No frame in the run was misclassified.** Every one of the 18,775 `dialog` frames was a fully drawn
text box, and the scene-run tally says the same from the other side: the longest unbroken `dialog`
run is **248 frames**, four seconds, against 6,245 frames for `overworld`. The fly was not stuck in
a box; it was going in and out of one.

### What it is: a scripted gate at (19, 9)

The periodic trace (`FLY_TRAP_TRACE_SECONDS`) puts the fly at **map 1, tile (19, 9)** from 6.00 brain
minutes to the end of the run, with:

```
trace 6.00 min  scene=dialog player=Player { map: 1, x: 19, y: 9, facing: Up } ahead=None
    font=0x01 textbox=0x01 joy=0 sim=0 flags5=0x00 move=0x00 corners=(0x79,0x7b,0x7d,0x7e)
trace 7.00 min  scene=unknown player=Player { map: 1, x: 19, y: 10, facing: Down } ahead=None
    font=0x00 textbox=0x01 joy=0 sim=0 flags5=0x80 move=0x00 corners=(0x40,0x39,0x50,0x23)
trace 7.67 min  scene=dialog  player=Player { map: 1, x: 19, y: 9, facing: Down } ahead=None
    font=0x01 textbox=0x01 joy=0 sim=1 flags5=0x80 move=0x00 corners=(0x79,0x7b,0x7d,0x7e)
```

Three things in that, and together they name the box:

- **`ahead=None`.** Nothing the macros can see is on the tile in front — no object sprite, no sign.
  So the box is not one the fly walked up to and talked to; it fires on the *step*.
- **`sim=1` and `flags5=0x80`.** `wSimulatedJoypadStatesIndex` is non-zero and `wStatusFlags5` bit 7
  is set: the cartridge is driving the player with its own joypad states. That is a script moving
  the fly, not the fly moving.
- **The fly oscillates between (19, 9) and (19, 10), facing Up then Down.** It steps north, the
  script fires, a box opens, the script walks it back south, the box closes, and the objective sends
  it north again.

That is Viridian City's blocking NPC on the road north — the gate that stands between the fly and
`ROUTE_2` until the parcel errand is done. `NEXT` and `NO` *do* advance the box; the 248-frame runs
are the proof. What the fly cannot do is pass, and rung 9's place is on the other side, so
`GO OBJECTIVE` walks it back into the gate every hold. The `Unknown` frames in the middle are the
scripted push-back, which is the detector working: a frame where the buttons do not reach the player
is not the overworld.

### What was fixed: the drawn half of the test

The cause is not a detector bug, but the investigation measured one anyway, and it is the reason the
question was worth asking: **315 frames of 43,004 had all four corner positions holding the frame's
own tile ids with no text box anywhere** (400 frames of 60,000 on the stub-driven ROM test). The
frame tiles `$79`, `$7b`, `$7d` and `$7e` are ordinary ground in the overworld tilesets — the same
range `TilePairCollisionsLand` names `$76` and `$78` in — so the four-corner test fires on the map
several times a minute, and the only thing between that and a wrong scene was `wFontLoaded`.

So the WRAM flag stays the gate, and the drawn half now reads the whole `TextBoxBorder` — four
corners, both horizontal runs, both vertical runs, 76 bytes rather than 4 — in `state::text_box` and
in `state::start_menu`, which had the same reading. A map cannot draw that by accident.

**No N-frame persistence rule was added**, and that is a measurement rather than a preference: with
the border read whole there were zero disagreeing frames to debounce, and a persistence rule costs
correctness in the other direction — a real box would read as the previous scene for N frames, which
is exactly long enough for a macro to start against the wrong palette.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 27 | four map tiles are read as a text box | the overworld tilesets use the frame's tile ids as ground; 315 frames of 43,004 drew all four corners with no box | `four_map_tiles_that_look_like_a_box_are_not_a_dialog`, `the_start_menus_box_is_read_the_same_way`, `the_map_is_never_read_as_a_text_box_from_the_stalled_checkpoint` (ROM) | **fixed**: `wFontLoaded` stays the gate and the drawn test reads the whole `TextBoxBorder`; `start_menu` likewise |
| 28 | the objective walks the fly into a scripted gate once per hold | rung 9's place is past Viridian's blocking NPC; the script opens a box and pushes the fly back, so the walk never fails and nothing is recorded | — | **left, named**: it is a target-ledger question, not a detector one. A walk whose arrival is answered by `wSimulatedJoypadStatesIndex` and a scripted push-back has been refused by the cartridge and should enter the blocked ledger; that is a macros change with its own measurement |

### The trap hunt, after the detector change

Same invocation, 20 brain minutes, seed 20260917, 4 sweep threads, from
`release-viridian-timeouts.checkpoint`. The detector change is meant to be a no-op on this run — every
frame it reclassifies is one no run has produced — and it is:

| measure | before the detector change | after |
| --- | ---: | ---: |
| distinct (map, tile) over 20 brain minutes | 142 | 142 |
| macros started | 1,144 | 1,144 |
| done / blocked / timeout / refused | 1,129 / 0 / 14 / 1 | 1,129 / 0 / 14 / 1 |
| windows flagged | 50 of 73 | 50 of 73 |
| frames read as `dialog` | 41,012 | 41,012 |
| longest unbroken `dialog` run | 248 | 248 |

Identical, measure for measure, which is what a no-op looks like when it is written down. The
branch's own counters over the same 71,673 frames: **41,012** frames of a real, fully drawn box,
**0** the corner test would have called `dialog` without a border behind it, and **682** frames —
about one per cent of the run — where the map drew all four corners with the font flag clear. The
rate is why the test was worth hardening; the zero is why nothing about this run changed.

The residual itself is untouched and expected to be: it is trap 28, the scripted gate, and it is a
macros change rather than a detector one.

### Gates

- `cargo test --workspace` with `FLY_ROM` and `FLY_TRAP_CHECKPOINT` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on main on this box (a debug build publishes at 9.46 Hz against the
  test's 30 Hz contract). Pre-existing and unrelated; that test runs `macros.mode = raw`. The scene
  and macro suites are green: 280 in `flybrain-gb`'s lib and all three ROM tests in `flysim`'s
  `rom_macros_mode`
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: ALL CHECKS PASSED.
- `flysim --print-compatibility`: the same 648 bytes, every component identical. The scene detector
  is not in the compatibility string and no checkpoint carries a scene.
- No TypeScript touched, so npm was not run.

## 2026-09-17, row 28: the gate at (18, 9), and the rung under the rank

Row 28 was left named: "the objective walks the fly into a scripted gate once per hold". Settled
the same day from the same checkpoint, on the WSL development box. `docs/design/macros.md` section
12.4 is the contract.

### Which script, and what unlocks it

`examples/scene_probe.rs` gained a `FLY_PROBE_CATCH=script` mode that drives macros mode until
`wSimulatedJoypadStatesIndex` is non-zero on the fly's map and then dumps everything. It caught it
at frame 27,053:

```
The cartridge took the joypad at frame 27053 (7.55 brain minutes).
The frame before, the player was on map 0x01 at (18, 10).

- scene: `Dialog`   text box: open=true waiting=true
- font=0x01 textbox=0x01 joy=0 sim=1 flags5=0x80 corners=(0x79,0x7b,0x7d,0x7e)
- player: Player { map: 1, x: 18, y: 10, facing: Down }
- sprites the macros can see:
    slot 4 picture 0x0d at (17, 9) facing Down
    slot 5 picture 0x48 at (18, 9) facing Down  (an object, not a person)
```

- The fly stepped **north out of (18, 10)**, into (18, 9), where a **still sprite** sits — picture
  `0x48`, past `FIRST_STILL_SPRITE`, so `Npc::person` calls it an object and `GO NPC` never offered
  it. A person stands beside it at (17, 9).
- The box's own tiles decode through pokered's charmap (`$80`-`$99` = A-Z, `$a0`-`$b9` = a-z, `$7f`
  = space, `$e7` = `!`) to **"This is private property!"**.
- `sim=1` and `flags5=0x80`: the cartridge is walking the player with joypad states of its own. It
  ends with the fly back on (18, 10) facing Down. 240 frames later, with nothing pressed, the box is
  still up — it waits for a press, and `NEXT` or `NO` does advance it.

**What unlocks it is in the save, not in the road.** The probe's event dump:

```
- rank 8 (VIRIDIAN CITY), badges 0
- 4 named events set
  - EVENT_FOLLOWED_OAK_INTO_LAB
  - EVENT_OAK_ASKED_TO_CHOOSE_MON
  - EVENT_GOT_STARTER
  - EVENT_GOT_OAKS_PARCEL
```

`EVENT_GOT_OAKS_PARCEL` is set and **`EVENT_OAK_GOT_PARCEL` is not**: the parcel is carried and
undelivered. `RUNGS` makes rung 6 `Flag("EVENT_OAK_GOT_PARCEL")` and rung 7
`Flag("EVENT_GOT_POKEDEX")`, so **rungs 6 and 7 were never earned** — and rung 7 in particular was
not, which is the question asked of this save. The rank reads 8 anyway, because `rank_from` takes
the *maximum* over satisfied rungs and rung 8 is `Visited(VIRIDIAN_CITY)`, a map the fly walked to.
`GameAdapter::objective` was `rank + 1`, so it answered rung 9 — `VIRIDIAN_FOREST`, whose first hop
is `ROUTE_2`, north, through the gate. The fly was being sent at a road the cartridge keeps shut
until Oak has his parcel, once per hold, for eight hours.

### What changed

All of it read-only on the adapter's side and inside the macros on the other; no payouts, no
version strings, no checkpoint format.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 28 | the objective walks the fly into a scripted gate once per hold | a rung earned out of order carries the rank past the rungs skipped under it, and `rank + 1` names the wrong one | `a_rung_earned_out_of_order_does_not_skip_the_ones_under_it`, `the_objective_is_the_errand_and_macros_mode_walks_to_it` (ROM) | **fixed**: the objective is the lowest rung *not* satisfied. The catalog already knew rung 6's place; the arithmetic was picking the wrong rung out of it |
| 28a | a conversation the cartridge ends by pushing the fly back is marked talked | any blocking script: the gate, a guard, an NPC that walks you off | `a_conversation_the_game_ends_by_moving_the_fly_is_not_talked_to`, `a_conversation_that_walks_the_fly_off_its_tile_is_not_talked_to` | **fixed**: the talked entry is pending until the box closes with the fly on the tile it pressed from and nothing driving it |
| 28b | a fly that answers `NO` retires what it said no to | every yes/no box, the catching tutorial included | `a_fly_that_answers_no_has_not_talked_to_anything` | **fixed**: a finished `NO` drops the pending entry; `YES` and `NO` both stay on the pad |
| 28c | a macro the cartridge answers by moving the fly records nothing | the gate, and every scripted push-back | `a_walk_the_cartridge_pushes_back_excludes_what_it_was_walking_to` | **fixed**: a scene change with `MacroState::scripted` excludes the target for the window; an ordinary scene change still records nothing |
| 28d | a person walked to and not talked to is retired for the session | `GO NPC` completes on arrival, and the fly is free never to press A | `a_reached_person_is_skipped_for_the_window` | **fixed**: reached is a window, like blocked. Oak is the case that found it — the fly stood in front of him, chose something else, and could never be offered him again |
| 29 | `GO OBJECTIVE` walks onto the objective map's doormat and `GO OUT` walks straight back out | the objective is an interior whose door leads to a town this run has stood on: tier 3 is the only tier that offers it, and it does | — | **left, named and measured**: it is trap 13 with an engine behind it. Dropping tier 3 on the objective's own map breaks the cycle and **replaces it with a stall in the room** — measured: the fly stayed in Oak's lab for 600,000 frames and delivered nothing. Both readings are dead ends; the next one has to keep the room leavable *and* give the fly a reason to stay |

### The trap hunt, before and after

Same invocation, 20 brain minutes, seed 20260917, 4 sweep threads, from
`release-viridian-timeouts.checkpoint`:

| measure | before (main, v0.3.4) | after (`fix/viridian-gate`) |
| --- | ---: | ---: |
| distinct (map, tile) | 142 | 137 |
| macros started | 1,144 | 1,335 |
| done / blocked / timeout / refused | 1,129 / 0 / 14 / 1 | 1,332 / 1 / 2 / 0 |
| windows flagged | 50 of 73 | **35** of 73 |
| frames in a text box | **41,012** | **0** |
| longest unbroken `dialog` run | 248 | — |
| frames in the overworld | 26,197 | 51,542 |
| frames in a battle | 0 | 1,720 (a wild battle at 1.03 min) |

**The gate loop is gone.** The fly spends no frames at all in a text box over twenty brain minutes,
where it spent 41,012 of 71,673 before; it leaves Viridian City southward, walks Route 1 and gets
into a wild battle — the first battle any run from this checkpoint has had. The ROM test measures
the same thing deterministically: Oak's lab, where the errand is, in **1.4 brain minutes on 55
macros**, by way of Route 1 and Pallet Town, and never through the shut road.

Two residuals, both named above and neither papered over:

- **Row 29, the lab-door bounce.** From 12.5 brain minutes the run holds
  `GO OBJECTIVE, GO FRONTIER, GO OUT, NEXT` at x11 to x28 over five tiles. It is trap 13, which was
  left as bounded, with an engine behind it now that the objective names the room. The fix attempted
  and rolled back is recorded in the row: it stalls the fly in the room instead.
- **The delivery itself is not something a game-blind rotation does.** Over 600,000 frames — eleven
  brain hours — in and around the lab the stub readout never pressed A at Oak. That is the same
  finding `macros_mode_leaves_the_house_and_reaches_oaks_lab` records for the starter: "a
  game-blind stub does not press `TALK` in front of a Pokéball on purpose". So **Route 2 is not
  asserted by any test on this branch**, and the claim that macros mode passes the gate belongs to a
  run with a brain in it and a reward for the rung. What is asserted is that the fly is now sent at
  the errand rather than at the wall.

`unknown` also rose, from 4,464 frames to 18,411, with a longest run of 135 frames: the fly is
crossing warps and taking scripted frames because it is moving between maps rather than standing in
one text box. It is named here as a thing to watch rather than a finding.

### Gates

- `cargo test --workspace` with `FLY_ROM` and `FLY_TRAP_CHECKPOINT` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on main on this box (a debug build publishes at 9.46 Hz against the
  test's 30 Hz contract). Pre-existing and unrelated; that test runs `macros.mode = raw`. Everything
  else is green: 285 in `flybrain-gb`'s lib, all 8 of its ROM tests, and all 4 in `flysim`'s
  `rom_macros_mode`
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: ALL CHECKS PASSED.
- `flysim --print-compatibility`: the same 648 bytes, every component identical. The rank ladder,
  the reward catalog and the checkpoint format are untouched; `objective` is a read accessor.
- No TypeScript touched, so npm was not run.

## 2026-09-17, row 29: the lab door, and a rung that is a conversation

Row 29 was named and measured in the section above and left for its own run. It went live the same
morning — `GO OBJECTIVE done, GO FRONTIER done, GO OUT done, NEXT done` every three brain seconds at
Oak's lab door, from about 09:00 UTC — so this is that run. `docs/design/macros.md` section 12.5 is
the contract.

### The mechanism, in one line

Section 12.4 aimed the objective at the right rung; the rung's *place* was still only a map, and
`GO OBJECTIVE`'s arrival on a map is that map's own door. So the fly walked to the lab's doormat,
the map changed, the walk was `Done`, and `GO OUT`'s last-resort tier offered the same door back.
Nothing in the room was the objective, so nothing held the fly in it.

### What changed

| # | trap | trigger | test | fix |
| ---: | --- | --- | --- | --- |
| 29 | `GO OBJECTIVE` arrives on the objective map's doormat and `GO OUT` walks straight back out | a rung earned by a conversation whose place is only a map: the parcel, the Pokédex, the starter, every badge | `an_objective_that_names_a_person_is_walked_to_and_faced`, `the_ways_out_leave_the_pad_while_the_objectives_own_thing_is_here`, `macros_mode_delivers_the_parcel_from_the_stalled_checkpoint` (ROM) | **fixed**: `MapPlace::target` names a `PlaceKind` (`Person`/`Object`), `GO OBJECTIVE` arrives beside it facing it — `GO NPC`'s own arrival, tile-ahead excluded so `TALK` takes over — and the ways out have no candidates while that target is on this map, untalked and unexcluded |
| 29a | the room becomes a trap when the objective's thing cannot be reached | a target walled off, or none | the same tests | **fixed by the ledger**: `GO OBJECTIVE`'s `no route` refusal writes the blocked keys, the objective's list empties, and the ways out are candidates again. This is what the earlier rollback lacked |
| 29b | the header names a rung nothing is working toward | any rung earned out of order | `the_next_rung_label_comes_from_the_adapter_ladder_and_holds_at_the_top` | **fixed**: `milestone.next` is `GameAdapter::next_rung`, the lowest rung not earned (`docs/feed-protocol.md`, 2026-09-17) |

### The proof, on the cartridge

`macros_mode_delivers_the_parcel_from_the_stalled_checkpoint`, from
`release-viridian-timeouts.checkpoint` with the game-blind rotating stub:

| stage | frames | brain minutes |
| --- | ---: | ---: |
| Oak's lab (south, by Route 1 and Pallet Town) | 5,150 | 1.4 |
| `EVENT_OAK_GOT_PARCEL` — the parcel delivered | 27,167 | 9.0 |
| `EVENT_GOT_POKEDEX` — rung 7, the same script | 0 | 9.0 |

504 macros for the whole errand. **This is the first rung that is a conversation any harness in this
repository has earned**, and the sibling tests' own note — "a game-blind stub does not press `TALK`
in front of a Pokéball on purpose" — is why: the stub still does not aim, but the macro now walks it
to the one thing that matters and hands it `TALK` with nothing else on the pad to walk away with.

### The trap hunt, before and after

Same invocation, 20 brain minutes, seed 20260917, 4 sweep threads, from the same checkpoint:

| measure | before (main, v0.3.5) | after (`fix/objective-target`) |
| --- | ---: | ---: |
| distinct (map, tile) | 137 | 136 |
| macros started | 1,335 | 977 |
| done / blocked / timeout / refused | 1,332 / 1 / 2 / 0 | 973 / 1 / 2 / 0 |
| **windows flagged** | 35 of 73 | **6** of 73 |
| `GO OUT` starts | 321 | **2** |
| `NEXT` starts | 533 | 41 |
| frames in the overworld | 51,542 | **69,458** of 71,673 |
| frames reading `unknown` | 18,411 | **355** |
| frames in a text box | 0 | 140 |
| longest unbroken overworld run | 6,245 | **28,851** (8 brain minutes) |

The bounce is gone: `GO OUT` ran twice in twenty brain minutes where it ran 321 times, and the six
windows still flagged are all in the first 1.25 minutes and all for `NEXT` repeating at period one
over 40 to 109 tiles — the known false-positive shape of a legitimately busy explorer, which the
section above already records. The `unknown` frames that came with crossing warps every three
seconds went with them, from 18,411 to 355.

`GO OBJECTIVE` now walks: 492 starts, all `done`, 71 frames and 1.7 tiles of net displacement each,
against 267 starts of 26 frames and **0.0** net before. That is the difference between arriving at a
door and walking to somebody.

### Gates

- `cargo test --workspace` with `FLY_ROM` and `FLY_TRAP_CHECKPOINT` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on main on this box (a debug build publishes at 9.46 Hz against the
  test's 30 Hz contract). Pre-existing and unrelated; that test runs `macros.mode = raw`. 23 suites
  green, including 287 in `flybrain-gb`'s lib, all 8 of its ROM tests and all 5 in `flysim`'s
  `rom_macros_mode`
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: ALL CHECKS PASSED.
- `flysim --print-compatibility`: the same 648 bytes, every component identical. `MapPlace` and
  `milestone.next` are neither in the compatibility string nor in a checkpoint.
- No TypeScript touched, so npm was not run.

## 2026-09-17, row 30: the move list is the fly's turn

Live at 14:56 UTC, rank 9, Viridian Forest (map 51), an hour and forty-one minutes on the rung: a
wild Kakuna L6, Bulbasaur at 8/28, the FIGHT move list open with TACKLE at 0 of 40 PP, and the event
log `NEXT start` / `NEXT done` every **268 brain milliseconds** for over an hour.
`docs/design/macros.md` section 12.6 is the contract.

### The mechanism

`Battle::own_turn` was `matches!(menu, BattleMenu::Main { .. })` — the top-level menu alone. So a
frame with the move list open was neither the fly's turn nor a forced switch, which is the
between-turns branch, whose pad is the single `NEXT` added by the v0.2.4 deadlock fix. `NEXT` is one
A press. A is "confirm the cursor's entry". The cursor sat on TACKLE, TACKLE had no PP, the game
printed its refusal, the box closed, the list was still there, and the next hold pressed A again.

Everything else about it was already right: `best_move` has skipped 0-PP moves since it was written,
and `ATTACK`'s script already goes straight to the move when the list is open. The button was simply
not on the pad.

| # | trap | trigger | test | fix |
| ---: | --- | --- | --- | --- |
| 30 | the move list open reads as "between turns", and `NEXT` presses A on a move with no PP | any turn where the list is open and the cursor's move is out of PP | `the_move_list_open_is_the_flys_turn`, `each_battle_menu_deals_its_own_pad`, `macros_mode_chooses_a_move_from_an_open_list_on_the_cartridge` (ROM) | **fixed**: any battle menu accepting input is `own_turn`; the pad is the sub-state's (`ATTACK`/`BACK` over the move list, `SWITCH`/`BACK` over the party list) and `NEXT` is on none of them |
| 30a | a turn with every move out of PP has nothing on the pad | the last Pokémon, out of PP, where the game wants Struggle | `attack_confirms_anyway_when_every_move_is_out_of_pp` | **fixed**: with the list open `ATTACK` confirms the cursor's own slot, which is how Struggle happens |
| 30b | a battle's opening frames read as an open move list | `MoveSelectionMenu`'s coordinates before the engine fills `wBattleMon*` | `macros_mode_chooses_a_move_from_an_open_list_on_the_cartridge` waits for a *settled* list | **fixed**: a list whose cursor the seam cannot place is not accepting input — measured on the cartridge as `Moves { cursor: None, count: 2 }` with `own: None` |
| 30c | the battle bag has no pad of its own | choosing ITEM in a battle | — | **left, no observable**: `BattleMenu` has no item variant and the bag reports no cursor through the seam, so an open battle bag reads as between turns and `NEXT` advances it. `ITEM` needs a potion and a hurt Pokémon to be offered at all, so the bag is rarely opened; a pad for it needs a verified reading first |

### The proof, on the cartridge

`macros_mode_chooses_a_move_from_an_open_list_on_the_cartridge`, from
`release-viridian-timeouts.checkpoint`: the fly walks into a wild battle of its own accord at **0.4
brain minutes** (Route 1's grass is on the way to the errand), raw A presses advance the opening text
until the move list is *settled*, and then

- the frame reads `Scene::Battle { own_turn: true }`, where it used to read `own_turn: false`;
- the pad is `ATTACK` and `BACK`, and **not** `NEXT`;
- `move_list_choice` names slot 0 with **28 PP**, `ATTACK` confirms it, and the list closes — the
  turn proceeds, with the eight `NEXT`s afterwards being the turn's own text, which is the
  between-turns pad doing its job.

The 0-PP case itself is pinned synthetically, because a fake can set a PP counter to zero and a
cartridge cannot be talked into it inside a test: `attack_over_an_open_move_list_chooses_a_move_that_has_pp`
puts TACKLE at 0 PP beside two moves that have some and asserts the cursor lands on a move with PP,
and `attack_confirms_anyway_when_every_move_is_out_of_pp` empties all three.

### Gates

- `cargo test --workspace` with `FLY_ROM` and `FLY_TRAP_CHECKPOINT` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on main on this box (a debug build publishes at 9.46 Hz against the
  test's 30 Hz contract). Pre-existing and unrelated; that test runs `macros.mode = raw`. 24 suites
  green, including 290 in `flybrain-gb`'s lib, all 8 of its ROM tests and all 7 in `flysim`'s
  `rom_macros_mode`
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: ALL CHECKS PASSED.
- `flysim --print-compatibility`: the same 648 bytes, every component identical. Scene detection is
  in neither the compatibility string nor a checkpoint.
- No TypeScript touched, so npm was not run.

## 2026-09-17, rows 32 and 33: a doormat the ledger cannot record, and a road that does not exist

Live at 17:32 UTC, four hours and eighteen minutes on rung 9 (VIRIDIAN FOREST, next PEWTER CITY),
with the fly on **map 50** — `VIRIDIAN_FOREST_SOUTH_GATE`, the ten-by-eight gate house between
Route 2 and the forest. The pad was **`GO FRONTIER` and nothing else**; `GO OBJECTIVE`, `GO ROUTE`
and `GO OUT` were absent; the last 40 KB of the event log was 175 `GO FRONTIER start`/`done` pairs,
each `done` about 420 brain milliseconds after its start — one tile — and a new one every 800 ms.
`uniqueLocations` read 1181. `docs/design/macros.md` section 12.7 is the contract.

Reproduced on the WSL development box from the release box's own checkpoint
(`.local/checkpoints/release-rank9-20260917T1732.checkpoint`, never committed). The reproduction is
faithful to the minute: from 5.25 to 12.00 brain minutes every window of the hunt holds **two**
tiles and about 149 macros, and the repeated sequence is `GO FRONTIER` **x150**.

### What the bytes say

`examples/scene_probe.rs` gained a `pad` dump — every candidate list the overworld pad rests on,
for the map a checkpoint is standing on — and it prints the whole trap in one screen. The ground,
`.` walkable, `#` wall, `?` off the screen buffer, `v` recorded in the exploration ledger, `@` the
fly:

```
y 3  ?- .v .v .v .v .v #- .v #- #-
y 4  ?- .v .v .v .v .v .v .v .- #-
y 5  ?- .v .v .v .v .v #- .v #- #-
y 6  ?- .v .v .v .v .v #- .v #- #-
y 7  ?- .v .v .v .v .- .-@.v .v .v #-
```

Three tiles of that map are walkable and have never been recorded, and the fly is **standing on
one of them**: (4, 7) and (5, 7) are the gate's two southern doormats, and (8, 4) is the tile the
gate's guard is standing on. So `path::frontier` answered four aims every hold, for ever:

```
- `path::frontier`: [((3,7), Right), ((4,6), Down), ((5,7), Left), ((7,4), Right)]
- the pad: ["GO OBJECTIVE", "GO OUT", "GO NPC", "GO FRONTIER"]   (session ledgers empty at t=0)
- `next_hop(0x32, 0x02)` = Some(13), neighbours [13, 51]
- Warp(0) at (4, 0) way Passage into Some(51) -> map_visited true
- Warp(1) at (5, 0) way Passage into Some(51) -> map_visited true
- Warp(2) at (4, 7) press Down way Exit into None -> destination Some(13), map_visited true
- Warp(3) at (5, 7) press Down way Exit into None -> destination Some(13), map_visited true
- `ways(Exit)` = [Warp(2), Warp(3)]   `ways(Passage)` = []   `ways(Route)` = []
```

Two mechanisms, and the second is the one that shut the pad.

1. **A doormat is walkable, standable ground the reward ledger can never record.**
   `MacroState::tile_visited` was `GameAdapter::tile_visited` alone, which is the `exploration`
   *payout*'s ledger: `sample` inserts a coordinate only on a frame its gate accepts, and that gate
   rejects `wMovementFlags & 0xc7` — bits 0, 1 and 2 are `BIT_STANDING_ON_DOOR`, `BIT_EXITING_DOOR`
   and `BIT_STANDING_ON_WARP`. The checkpoint's own frame reads `move=0x04`, standing on a warp. So
   **every warp tile in the game was a permanent frontier**, in every town and every house; the gate
   house is where it became the whole pad, because its south wall *is* two doormats and the walk to
   one is a single tile. `GO FRONTIER`'s 627 starts in the before-run average 26 frames and one tile
   of net displacement each, which is the live log's 420 ms exactly.
2. **`ROUTE_2` is one map id whose ground is in two halves.** `next_hop(0x32, PEWTER_CITY)` answered
   `ROUTE_2` — the *south* door, the one the fly had walked in by — because Route 2's north edge
   touches Pewter and the graph had one node for the whole map. Physically the halves are separated
   by a belt of trees that needs CUT and the only way between them is the forest. Everything else
   follows from that one wrong hop:
   - `objective_goals` aimed `GO OBJECTIVE` back out of the south door, so the fly ping-ponged
     gate → Route 2 → gate (the before-run's trace: map 50 at 0 min, map 13 at 1–5 min, map 50 from
     6 min on) until the blocked ledger excluded both doormat warps, after which the macro had no
     candidate and left the pad;
   - `ways(Way::Exit)`'s three tiers all emptied for the same reason: the destination (Route 2) is
     visited, so nothing is fresh; tier 2 asks the same wrong hop and the *excluded* doors are gone
     from the list; so `GO OUT` left the pad too;
   - `ways(Way::Passage)` — **the north doors into the forest, which are the way to Pewter** — has
     no tier 3 on a map that has a `Way::Exit`, tier 1 is empty because the forest is visited, and
     tier 2 wanted a destination of 13. So `GO WARP` was never on the pad at all.
   - `GO ROUTE` is indoors-absent by construction (`plan::overworld` deals `GO OUT` indoors), which
     is why the operator saw three buttons missing and not four.

### What changed

All of it inside the macros and the harness. The decoder, the reward catalog, the adapter version
and the compatibility string are untouched (`--print-compatibility` byte-identical, below).

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 32 | a warp tile is a frontier no ledger can retire | any doormat; the whole pad where the doormats are the only unrecorded ground | `the_tile_the_fly_stands_on_is_visited_although_the_run_ledger_records_nothing`, `macros_mode_leaves_the_forests_south_gate_and_reaches_its_north_one` (ROM) | **fixed**: the reward gate stays as it is and the macro layer keeps its own `StoodLedger` — session state beside `talked`/`blocked`/`reached`, written once per frame from the tile the fly stands on, never checkpointed, OR'd with the adapter's. A scripted frame records nothing, because the coordinates and the loaded map header are then from different frames |
| 32a | a tile a sprite stands on is a frontier for as long as the sprite stands there | a villager in the open, an item ball on a floor tile: the gate's guard at (8, 4) | `a_tile_a_sprite_is_standing_on_is_not_new_ground` | **fixed**: `path::frontier` drops it as the ground to stand on and as the new ground beyond. Row 5 bounded this at one walk per window; dropping it is what lets the frontier *exhaust*, which is what "the button leaves the pad" needs |
| 33 | the map graph collapses a map whose ground is in two pieces, and answers a road that does not exist | `ROUTE_2`, whose halves are joined only through Viridian Forest; it takes `GO OBJECTIVE`, `GO OUT` and `GO WARP` off the pad together | `route_2s_halves_are_told_apart_by_the_row_the_fly_is_standing_on`, `the_way_to_pewter_from_the_forests_south_gate_is_north_through_the_forest`, `a_split_maps_pieces_divide_its_neighbours_between_them` | **fixed**: `geography::Region` is a map *and a piece of it*; `next_hop` is asked from the piece the fly is standing in; one `SPLIT` row carries the two pieces' neighbour lists and the two doorway rows they are told apart by (11 and 43, surveyed from the cartridge) |
| 33a | tier 2 of `ways` gives up when the graph knows no route, even when a door names the objective's map outright | a map with no row in the table whose every destination is visited: no tier has a candidate and the button leaves the pad | `a_door_into_the_objectives_own_map_is_toward_it_with_no_route_in_the_graph` | **fixed**: the answer `objective_goals` already had, now in `toward_objective` as well |
| 33b | maps 46, 48 and 49 are still not on the graph | a route through Diglett's Cave, the Route 2 trade house or the Route 2 gate | — | **left, the table's standing rule**: a building no rung place needs a route through is not on the graph, and 49's two doors are both warps of `ROUTE_2` itself, so it is a shortcut within one map rather than a way between two. A route the table does not carry is not offered; nothing is guessed |
| 34 | a battle where every move is out of PP has no way to reach Struggle | `best_move` is `None`, so `ATTACK` leaves the *main* battle menu, so FIGHT never opens and the move list row 30a confirms Struggle from is never reached | `attack_is_on_the_pad_with_no_pp_anywhere_and_unbound_with_no_moves_at_all`, `a_turn_with_nothing_to_attack_switch_or_flee_with_still_has_a_button`, `macros_mode_ends_a_turn_with_no_move_left_on_the_cartridge` (ROM) | **fixed** in the section below: the precondition is "is there a move list to open", and the script confirms FIGHT and stops, because `CheckPlayerHasUsableMoves` answers it without opening the list — measured on the cartridge |

### Row 34, measured: the turn with nothing left to attack with

`examples/scene_probe.rs` gained `FLY_PROBE_CATCH=noattack`, which drives macros mode until the
fly's own turn has had `ATTACK` off the pad for a run of frames and then dumps the battle. From
this checkpoint it caught it at frame 355,143 — **99.1 brain minutes** — on map 51 at tile (4, 30),
which is the same tile the 1.2-million-frame ROM run ends on:

```
- scene: `Battle { own_turn: true, forced_switch: false }`
- the pad: ["macro_back"]
- battle: Wild, own_turn, menu Party { cursor: 0 }
- enemy: species 84 level 5, 18/18
- the Pokemon that is out: slot 0 species 0x99 level 12, 11/34, Healthy
  - move 0: Move { id: 33, pp: 0 }      (TACKLE)
  - move 1: Move { id: 45, pp: 0 }      (GROWL)
  - move 2: Move { id: 73, pp: 0 }      (LEECH SEED)
  - move 3: None
- party of 1
- bag: Poke Ball x1, Potion x1
```

**A pad of one button, in a battle.** Every move is at 0 PP, so `best_move` answers `None` and
`ATTACK`'s precondition over the top-level menu fails; the party is one Pokémon, so `SWITCH` fails;
`RUN` is on the pad and the cartridge refuses it (60 `RUN` starts, all `blocked`); `NEXT` is one A
press on whatever the cursor holds, which opens the **party list**, whose pad is `SWITCH` and
`BACK` and whose `SWITCH` is unbound — so the only button left is `BACK`, which closes it again.
`ITEM`, `BACK`, `NEXT`, once per hold, which is exactly the three names the ROM run's counts
showed still moving.

Row 30a already says "with the list open `ATTACK` confirms the cursor's own slot, because Struggle
is the cartridge's answer there" — and the list is only ever reached *through* FIGHT, which is the
button this state takes off the pad. That is row 34, and it is left for its own run rather than
patched here: the fix is one precondition and the measurement it needs is a checkpoint in this
state, which this one is not.

### The survey: Route 2's own warp table

`FLY_PROBE_MAP=13 cargo run --release -p flysim --example scene_probe`, sixty frames after the
fly stood on it, because `wCurMap` changes several frames before the header it names is loaded:

```
map size: MapSize { width: 20, height: 72 }
connections: north: true, south: true, east: false, west: false
warps: (12, 9)->46  (3, 11)->47  (15, 19)->48  (16, 35)->49  (15, 39)->49  (3, 43)->50
```

The two forest gates are rows **11** and **43**, and those are the doorways `SPLIT` tells the two
halves apart by: a tile belongs to the piece whose doorway row it is nearer. The number comes from
the warp table rather than from a claim about where the trees are, and the only place the answer
could be wrong is a tile in the impassable belt between them — ground the fly cannot stand on.

### The trap hunt, before and after

20 brain minutes, seed 20260917, 4 sweep threads, same connectome and same cartridge in both runs:

```sh
FLY_ROM=".../Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_TRAP_CHECKPOINT=.local/checkpoints/release-rank9-20260917T1732.checkpoint \
  FLY_TRAP_MINUTES=20 cargo run --release -p flysim --example trap_hunt
```

| measure | before (main, v0.3.9 `a844d56`) | after (`fix/loop-20260917T1732`) |
| --- | ---: | ---: |
| distinct (map, tile) over 20 brain minutes | 78 | **193** |
| macros started | 864 | 349 |
| done / blocked / timeout / refused | 860 / 3 / 0 / 0 | 322 / 17 / 10 / 1 |
| windows flagged | 54 of 73 | 49 of 73 |
| **windows under 4 distinct tiles** | **16** | **0** |
| worst window | **2 tiles, 150 macros** | 17 tiles, 76 macros |
| longest repeat | **`GO FRONTIER` x150** | `NEXT` x13 |
| `GO FRONTIER` starts / mean frames / mean net tiles | 627 / 26 / **1.0** | 11 / 64 / 0.8 (max net 8) |
| `GO WARP` starts / mean frames / max net tiles | 6 / 9 / **0** | 28 / 624 / **29** |
| frames in a battle | 2,489 | 13,913 |
| frames in a text box | 3,411 | 0 |

**The loop is gone, and so is its shape.** No window in the after-run holds fewer than four
distinct tiles, where sixteen of them held two; the longest repeated sequence is `NEXT` at period
one, which is battle text and the known false-positive shape this file has recorded since the first
section; and `GO FRONTIER` runs eleven times in twenty brain minutes instead of 627, covering up to
eight tiles a time instead of one. `GO WARP` is what replaces it: 28 starts averaging 624 frames
and reaching up to 29 tiles of net displacement, which is the fly walking north across Viridian
Forest. The battle frames are the honest cost of that — the forest is wild grass and trainers, and
2,489 frames of battle became 13,913.

The fly leaves the gate house **northward, into Viridian Forest, in 155 frames on one macro** (the
ROM test below), where four hours of stream never left it at all.

### The proof, on the cartridge

`macros_mode_leaves_the_forests_south_gate_northward`, from this checkpoint with the game-blind
rotating stub:

| what | measured |
| --- | ---: |
| frames to leave the gate house | **155** (2.6 brain seconds) |
| macros to leave it | **1** |
| the first map out of it | **`VIRIDIAN_FOREST` (51)** — northward, where it used to be Route 2 |
| smallest overworld pad the gate house ever dealt, over 33.5 brain minutes | **3** buttons |
| macros over the whole run | 1,037: 958 done, 60 blocked, 18 timeout, 2 refused |

The direction is the claim. Before this branch the first map out of the gate was Route 2, southward,
and the fly came straight back; the pad was one button for four hours. The pad dump above and the
one after the fix differ in exactly three lines:

```
-  `next_hop(0x32, 0x02)` = Some(13)          `ways(Passage)` = []
+  `next_hop(Region { map: 50, part: 0 }, 0x02)` = Some(51)   `ways(Passage)` = [Warp(0), Warp(1)]
-  objective_goals = [Aim { tile: (4, 7), .. }, Aim { tile: (5, 7), .. }]     # the south doormats
+  objective_goals = [Aim { tile: (4, 0), .. }, Aim { tile: (5, 0), .. }]     # the north doors
-  the pad: ["GO OBJECTIVE", "GO OUT", "GO NPC", "GO FRONTIER"]
+  the pad: ["GO OBJECTIVE", "GO OUT", "GO NPC", "GO WARP", "GO FRONTIER"]
```

**Route 2's north half is not asserted, and that is a measurement rather than a trim.** 1,200,000
frames (5.6 brain hours) from this checkpoint cover six maps and 20,595 macros — into the forest,
back out, down to Route 2, a blackout to Pallet Town and the walk back north — and never reach map
47. From about 600,000 frames on the only macros that start are `ITEM`, `BACK` and `NEXT`: `ATTACK`
does not start once more and every overworld count freezes. That is row 34 in the table above, and
it is its own run.

### Gates

- `cargo test --workspace` with `FLY_ROM`, `FLY_TRAP_CHECKPOINT` and `FLY_GATE_CHECKPOINT` set:
  green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on main on this box (a debug build publishes well under the test's 30 Hz
  contract). Pre-existing and unrelated; that test runs `macros.mode = raw`.
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: ALL CHECKS PASSED.
- `flysim --print-compatibility`: **byte-identical to main**, 648 bytes, diffed rather than eyeballed
  (`flysim` built from `a844d56` and from this branch, same ROM and same dataset). The exploration
  ledger's payout gate, the reward catalog and the checkpoint format are untouched; the stood ledger
  is session state in the executor layer and reaches no checkpoint.
- No TypeScript touched, so npm was not run.

## 2026-09-17, row 34: the turn with nothing left to attack with

Row 34 was named and measured in the section above and left for its own run. This is that run.
`docs/design/macros.md` section 12.8 is the contract.

### The mechanism, in one line

Section 12.6 made `ATTACK` confirm the cursor's own slot over an open move list "because Struggle
is the cartridge's answer there" — and the move list is only ever reached through FIGHT, whose
button's precondition was `best_move(..).is_some()`. So the one turn with nothing left to attack
with took `ATTACK` off the top-level pad and the way to Struggle with it.

### What the cartridge does with FIGHT and no PP, measured

`examples/scene_probe.rs` gained `FLY_PROBE_CATCH=nopp` — drive macros mode until the fly's own
turn has a Pokémon out whose every move is at 0 PP — and then, with raw presses, backs out to the
top-level menu, puts its cursor on FIGHT, confirms it and watches what happens. Caught at frame
181,616 (**50.7 brain minutes**) from `release-rank9-20260917T1732.checkpoint`:

```
- battle: Trainer, own_turn, menu Moves { cursor: Some(1), count: 3 }
- the Pokemon that is out: slot 0 species 0x99 level 12, 6/34, Healthy
  - move 0: Move { id: 33, pp: 0 }   move 1: Move { id: 45, pp: 0 }   move 2: Move { id: 73, pp: 0 }
- party of 1
- after backing out with B: Main { cursor: 0 }
- menus after confirming FIGHT, in order: ["Some(None)"]
- pulses with the move list open: 0
```

**The move list never opens.** `CheckPlayerHasUsableMoves` answers FIGHT itself — "has no moves
left!", Struggle, and `own_turn` goes false on the next frame — so the macro's whole job from the
top-level menu is to confirm FIGHT. A second cursor step would press A at text.

Two things the same survey settled, both recorded rather than assumed:

- **the list *can* be open with nothing having PP** (it was, on the frame this caught), so section
  12.6's rule stays: `ATTACK` confirms the cursor's slot there, earns "There's no PP left for this
  move!" and the list again, and `BACK` beside it leads to the menu where FIGHT works. One wasted
  press, not a loop.
- **the first `nopp` catch was a false positive and is kept here because it was run**: the
  condition was `best_move(..).is_none()`, which is also true on a battle's opening frames before
  the engine copies the active Pokémon into `wBattleMon*` (row 30b). It caught frame 17,142 with
  full PP and wrote a checkpoint of it. The condition is now the state itself — a Pokémon that *is*
  out, with every move it has at 0 PP.

### What changed

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 34 | `ATTACK` leaves the top-level pad on the one turn with no PP, so FIGHT never opens and Struggle is unreachable | any Pokémon whose every move is out of PP; with a party of one and a `RUN` the cartridge refuses it is the whole turn | `attack_is_on_the_pad_with_no_pp_anywhere_and_unbound_with_no_moves_at_all`, `a_turn_with_nothing_to_attack_switch_or_flee_with_still_has_a_button`, `macros_mode_ends_a_turn_with_no_move_left_on_the_cartridge` (ROM) | **fixed**: `can_fight` replaces `best_move(..).is_some()` over the top-level menu — the question is whether there is a move list to open — and the script confirms FIGHT and stops |
| 34a | the party list with a one-Pokémon party deals `BACK` alone | `ITEM` opens it to choose who to heal, and its one entry is the Pokémon already out | `a_turn_with_nothing_to_attack_switch_or_flee_with_still_has_a_button` pins it as `BACK` | **left, by construction**: there is nothing to choose, so backing out is the press. A second button there would be a macro that completes without moving, which section 12.2 calls a trap. What changed is where `BACK` leads — a menu with `ATTACK` on it |

### Before and after, on the same 1.5 million frames

`FLY_PROBE_CATCH=noattack` is row 34's own symptom detector: the fly's own turn with `ATTACK` off
the pad for 600 consecutive frames. Same checkpoint, same stub, same budget:

| measure | before (main, v0.3.10 `36e6b8c`) | after (`fix/loop-row34`) |
| --- | ---: | ---: |
| first own turn with no `ATTACK` on the pad | frame **355,143** (99.1 brain minutes) | **none in 1,500,000 frames** |
| the pad it dealt there | `["macro_back"]` | — |

And the ROM test, from the checkpoint `FLY_PROBE_SAVE` wrote of the caught turn:

| what | measured |
| --- | ---: |
| `ATTACK` on the pad of the fly's own turn | yes, on the first frame |
| the macro starts and runs on the cartridge | 1 start, `done` |
| smallest pad the fly's own turn ever dealt | **2** buttons |
| the battle ends | **1,535 frames** (0.4 brain minutes) on 33 macros |

Which way the battle ends is not asserted: with 6 of 34 HP against a trainer the measured run
faints and blacks out, and that is the game rather than the macro layer.

### The trap hunt, as a regression

Row 34's state is fifty brain minutes past the gate-house checkpoint, so twenty brain minutes of
hunting cannot reach it and the 1.5-million-frame sweep above is this row's own measurement. The
hunt is run anyway, for what it says about everything else. Same invocation, 20 brain minutes, seed
20260917, 4 sweep threads:

| measure | v0.3.10 `36e6b8c` | after (`fix/loop-row34`) |
| --- | ---: | ---: |
| distinct (map, tile) over 20 brain minutes | 193 | **380** |
| macros started | 349 | 643 |
| done / blocked / timeout / refused | 322 / 17 / 10 / 1 | 554 / 75 / 13 / 2 |
| windows flagged | 49 of 73 | 59 of 73 |
| windows under 4 distinct tiles | 0 | 6 |
| frames in a battle | 13,913 | **36,501** |
| longest unbroken battle | 845 | **11,130** (3.1 brain minutes) |
| `ATTACK` starts | 0 | 87 (13 done, **74 blocked**) |

**Twice the ground, and the reason the other numbers moved is that the fly now fights.** `ATTACK`
was on no pad at all in the v0.3.10 run from this checkpoint; it runs 87 times here, the battle
frames go from 13,913 to 36,501, and the six windows under four tiles are all inside one
eleven-thousand-frame trainer battle beginning at 16.89 brain minutes — one tile, sixty-odd macros,
**no repeated sequence at any period**. A three-minute battle is a window the tile rule cannot tell
from a stall, which is the same known false-positive shape this file records for `NEXT` at period
one, in the other direction.

Two residuals, named rather than papered over:

- **74 of the 87 `ATTACK` starts end `Blocked`**, at a mean of 204 frames and no ground covered.
  That is row 19 of the audit — "a cursor macro waits 180 frames and reports `Blocked`: `ATTACK` or
  `RUN` started in the last frames of `own_turn`, because `listing()` is `None` for most of a
  battle" — which was left as "one wasted hold, no loop". It is the same trap at a much higher
  rate, because `ATTACK` is now bound on every turn rather than only on turns with PP. It carries
  no target, so it poisons no ledger, and the run's own numbers say it costs coverage nothing; a
  cheaper wait is a tuning change with its own measurement.
- **the tile rule and a long battle.** Six windows of one tile with no repeat is a fly standing
  still in a trainer battle. The hunt's thresholds are the operator's and are left as specified.

### Gates

- `cargo test --workspace` with `FLY_ROM`, `FLY_TRAP_CHECKPOINT`, `FLY_GATE_CHECKPOINT` and
  `FLY_NOPP_CHECKPOINT` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on main on this box. Pre-existing and unrelated; that test runs
  `macros.mode = raw`.
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: ALL CHECKS PASSED.
- `flysim --print-compatibility`: byte-identical to main, 648 bytes, diffed rather than eyeballed.
  A precondition and a script are in neither the compatibility string nor a checkpoint.
- No TypeScript touched, so npm was not run.

## Sections 13 and 14: the trap hunt, before and after

Twenty brain minutes, seed 20260917, 4 sweep threads, same connectome and same cartridge, from the
Viridian timeouts checkpoint the release container wrote. **Both arms are driven by the hunt's
`FLY_TRAP_STUB=1` rotation over the same thirty-two channel names**, and that is not a detail:
`tools/build_flywire.py` re-deals every `macro_<type>` population whenever a type is added — 206
neurons thirty-one ways is not twenty-two ways plus nine — so a run driven by the *brain* cannot
separate "the macros got worse" from "the populations moved". The stub ticks the brain and replaces
only the readout, so the two arms differ in the macro code and in nothing else.

```sh
FLY_ROM=".../Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_TRAP_CHECKPOINT=.local/checkpoints/<the Viridian timeouts checkpoint> \
  FLY_TRAP_MINUTES=20 FLY_TRAP_STUB=1 cargo run --release -p flysim --example trap_hunt
```

| measure | before (main) | after (`feat/macros-shops`) |
| --- | ---: | ---: |
| distinct (map, tile) over 20 brain minutes | 166 | **229** |
| macros started | 82 | 95 |
| done | 79 | 80 |
| blocked / timeout / refused | 2 / 1 / 0 | 0 / 15 / 0 |
| windows flagged | 4 of 73 | **0 of 73** |
| frames in `overworld` | 59,425 | 61,582 |
| longest unbroken `overworld` run | 36,957 | 18,438 |
| frames in `dialog` | 2,203 | 8,114 |

**Both of the hunt's criteria improve**: more ground (166 → 229 tiles) and fewer flagged windows
(4 → 0). `docs/loop-review.md` asks for exactly those two.

Three things about it that are not improvements and are recorded rather than buried.

- **Timeouts went 1 → 15.** Fifteen of ninety-five macros spent their budget. None of them is a
  loop — no window flagged and the tile count went *up* — so they are long walks: the errands cross
  a city, and a walk that ends nearer its goal than it began costs a strike and resumes on the next
  hold rather than excluding anything (row 1). It is the shape row 1 designed for, and it is worth
  watching rather than celebrating.
- **Dialog frames went 2,203 → 8,114**, and 4,731 of the after-run's are one box at (20, 29) of
  Viridian City — beside `bg_event 21, 29`, a Trainer Tips sign the fly read and advanced slowly.
  The before arm spent 9,475 frames in a battle instead. The two runs simply go different places.
- **The trap in row 37 is one this branch *exposed* rather than introduced.** (19, 9) is the
  cartridge's behaviour and `approach` has always offered the four tiles around a person; what put
  the fly in that corner of Viridian is the mart and centre errands. Neither arm of the earlier
  brain-driven pair stood on it. Latent, then exposed, then closed.

### What the earlier brain-driven pair said, and why it is not the gate

Recorded because it was run and because it is what found row 37. Same checkpoint, no stub:

| measure | before (main, brain) | after (branch, brain, before row 37) |
| --- | ---: | ---: |
| distinct (map, tile) | 196 | 109 |
| macros started | 698 | 1,403 |
| windows flagged | 31 of 73 | 63 of 73 |
| frames in `dialog` | 24,411 | 49,286 |

Half the ground and twice the flagged windows — and the location tables the hunt gained for this
work said why in one line: **53,266 of 54,377 text-box frames on tile (19, 9) of Viridian City**,
991 of them answered `YES`. That is row 37. The population re-deal is why this pair cannot be the
gate; the stub pair above is.

The three readings of (19, 9), in the order they were measured:

| | text-box frames on (19, 9) |
| --- | ---: |
| before row 37 | 53,266 |
| the tile excluded as a **goal** | 12,919 |
| and as a **wall to the route** | **1,538** |

The middle row is why the fix has two halves: the A* went on routing *across* the tile toward
somewhere else, and the script fires on any frame the fly stands there.

`padEmptyMs` is **not** measured by this run: `examples/trap_hunt.rs` does not read the feed header,
so the only thing said about it here is that it exists and is exported. Whether a playable scene
ever deals nothing on the release box is the watchdog's gauge to answer, over a run longer than
twenty brain minutes.

## 2026-09-22, rows 38 and 39: `BACK` between turns, and a ball at what the party already has

Rank 9 (VIRIDIAN FOREST, next PEWTER CITY), **sixty-nine hours** on the rung, ratchet attempts 3 of
3 spent, the fly on Route 2 and in the forest and mostly in wild battles. Since the last restart the
macro starts were `BACK` 135, `THROW BALL` 28, `MOVE 2` 10, `RUN` 5 and `GO ROUTE` 5, and the event
log repeated:

```
RUN blocked, BACK start, BACK done
```

`docs/design/macros.md` section 12.9 is the contract this closed against. Both traps are section
12.2's one rule — *a macro that completes without moving because its precondition is already
satisfied where the fly stands* — and the reproduction is the hunt from the release container's own
checkpoint, twenty brain minutes, the real brain as the readout.

### The mechanism

Three facts, and the first one is the whole of it.

1. **`BACK` was half of the pad the fly spends a wild battle looking at, and it could not change
   anything.** The top-level battle menu never dealt it: `scene_set`'s own-turn arm is
   `MOVE 1..4, SWITCH, ITEM, THROW BALL, RUN, NEXT` and B is not a fifth answer to a four-entry
   menu. What dealt it was the **between-turns** row, `Scene::Battle { own_turn: false, .. }`, which
   section 13.1 gave `NEXT, BACK` for the sake of the battle bag — the bag reads as nobody's turn
   (12.6), so it lands on that arm — and which is *also* every frame of battle text, every
   animation and every turn resolving. On those frames nothing is open, so `BACK`'s single B press
   changes nothing the `NEXT` beside it does not: measured at **mean 16 frames, 1.0 tiles, net 0**,
   163 starts between turns plus 12 more on a move list whose cursor the seam cannot place, out of
   959 macros in twenty brain minutes. Half a pad of two, once per hold, while the turn did not
   move.
2. **`RUN blocked` was the script, not the cartridge.** Not "can't escape": `RUN` is one cursor
   navigation, and a cursor macro whose list is not accepting input **waits** rather than pressing
   blind (section 4), for `CURSOR_WAIT` = 180 frames before it reports `Blocked`. Measured: ten
   `RUN` starts, ten `Blocked`, **mean 184 frames**, every one of them started on the top-level
   menu. So a `RUN` that won a hold as the menu closed spent three brain seconds pressing nothing
   and gave up. That is **row 19**, bounded and left, and the only reason it was legible in the
   event log is the `BACK` above filling the frames on either side of it. `RUN`'s precondition is
   unchanged (13.1: only a wild battle the fly is losing).
3. **`THROW BALL` had no reading of what it was throwing at.** Its three facts were a wild battle,
   a ball in the bag and room in the party, and none of them is "this is not one I already have".
   Viridian Forest holds five species; a throw at one the party carries spends a ball for nothing,
   and a catch opens the nickname screen, which reads `Unknown`, needs the START the pad has no
   button for, and is the one screen neither `NEXT` nor `BACK` leaves (**row 14**). So the trap is
   paid for twice.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 38 | `BACK` on a battle pad with no list open completes without changing anything, once per hold | every frame of battle text, every animation, every turn resolving — which is most of a wild battle | `back_is_on_a_battle_pad_only_where_a_list_is_open`, `a_battle_frame_that_is_not_the_players_turn_binds_next_to_advance_its_text`, `the_battles_turns_advance_from_the_rung_nine_forest_checkpoint` (ROM-gated) | **fixed**: the between-turns row is the sub-state's, like the own turn's — `NEXT, BACK` with the bag open, `NEXT` alone otherwise. The move list and the party list keep `BACK`, because backing out of a list is one of exactly two answers to one, and the top-level menu never had it |
| 39 | `THROW BALL` throws at a species the party already holds | any wild battle after the first catch of that species | `throw_ball_refuses_a_species_the_party_already_holds`, and the ROM-gated run above asserts it never happens on the cartridge | **fixed**: a fourth fact in the precondition. The **party** is the caught set — the cartridge's own lifetime record, in the same internal species numbering the enemy is read in. `wPokedexOwned` is *not* asked: that bitset is by Pokédex number and the table converting an internal index into one lives in a ROM bank this crate cannot read, so it would be an unverified number (`docs/design/ladder.md`). It costs nothing measurable, because the button is already off the pad while the party is full and nothing in the vocabulary deposits into a box (row 17). An enemy the seam cannot place leaves the button where it was |
| 40 | the ratchet cannot help on a rung sixty-nine hours old | `budget_spent()` precedes both triggers and three attempts on rank 9 were spent | `ratchet.rs`'s own tests ("three attempts per rank") | **left, contract**, and this is row 22 measured again: no recovery can fire until the rank *improves*. What carries the run is the road, and the road is checked rather than assumed — the objective is the lowest **unearned** rung (12.4), which is rung 10's `PEWTER_CITY`; `geography::next_hop` from the forest answers `VIRIDIAN_FOREST_NORTH_GATE` (47), then Route 2's north piece, then Pewter (2), asserted hop by hop since 12.7; and the ROM-gated run reads `GO OBJECTIVE` on the overworld pad of maps 13, 50 and 51. **The run does not reach map 47** in 67 brain minutes of the game-blind rotation, so that is reported and not asserted |
| 41 | the nurse's box answered `YES` four hundred and seventy-four times | measured in the **before** arm only: from 11.5 brain minutes the fly stood on one tile of a Pokémon Center and the windows read `YES` x21 to the end of the run | — | **named, not worked**: 23,548 of the before arm's 71,673 frames were a text box on map `0x29` at (3, 3), 474 of them answered `YES`, and the run ended there. The after arm never enters it — `dialog` frames go **23,548 → 0** — so it is neither reproduced nor fixed by this branch, and it is *the next thing to measure*. Rows 1, 2b, 23 and 24 were each found this way: a loop behind the loop in front of it |

### The trap hunt, before and after

Twenty brain minutes, seed 20260917, 4 sweep threads, same connectome and same cartridge, from the
release container's rung-9 checkpoint. **Driven by the brain rather than by `FLY_TRAP_STUB`**, which
is legitimate here and was not for sections 13 and 14: this branch adds no macro type, so
`tools/build_flywire.py` re-deals nothing, the thirty-one `macro_<type>` populations are the same
function in both arms, and `--print-compatibility` is byte-identical at 648 bytes. The two arms
differ in the macro code and in nothing else.

```sh
FLY_ROM=".../pokemon-red.gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_TRAP_CHECKPOINT=.local/checkpoints/<the rung-9 forest checkpoint> \
  FLY_TRAP_MINUTES=20 cargo run --release -p flysim --example trap_hunt
```

| measure | before (v0.4.1) | after (`fix/loop-20260922T0254`) |
| --- | ---: | ---: |
| distinct (map, tile) over 20 brain minutes | 216 | **286** |
| windows flagged | 70 of 73 | **61 of 73** |
| windows under 4 distinct tiles | 21 | **5** |
| macros started | 959 | 830 |
| done / blocked / timeout | 873 / 80 / 5 | 726 / 97 / 6 |
| frames in `battle` | 27,525 | 48,756 |
| frames in `dialog` | 23,548 | **0** |
| `MOVE n` starts | 36 | **68** |
| `BACK` starts with **no list open** | **175** | **0** |
| `THROW BALL` starts | 47 | 46 |

**Both of `docs/loop-review.md`'s two criteria improve**: a third more ground (216 → 286 tiles) and
nine fewer flagged windows, with the windows that hold fewer than four tiles down from 21 to 5. The
pad tables the hunt gained for this work are the direct reading:

| battle sub-state | pad before | pad after |
| --- | --- | --- |
| main menu | `move_1..4 next run throw_ball` | unchanged |
| move list | `back move_1..4` | unchanged |
| move list, no cursor | `back next` | **`next`** |
| party list | `back` | unchanged |
| between turns | `back next` | **`next`** |

Four things about it that are not improvements, recorded rather than buried.

- **`BACK` is still 359 starts of 830**, all of them over an open list: 306 on the move list and 46
  on the party list, which with a party of one is `BACK` alone by design (row 34 — "what changed is
  where `BACK` leads: a menu with `ATTACK` on it"). The trap was the *listless* `BACK`, and that is
  the row that went to zero.
- **A `NEXT, BACK` two-cycle closes the run**, x75 over the last four brain minutes inside one
  battle: the move list opens before its cursor can be placed, `NEXT` advances, the cursor appears,
  `BACK` closes the list, and round again. It is bounded — the battle is real, `MOVE n` wins holds
  in it, and the turn does move — and it replaces a `BACK` x13 cycle in the same windows of the
  before arm. Worth watching; the honest fix is a move list whose cursor the seam can place on its
  opening frames, which is a WRAM reading and not a pad change.
- **A window spent in a battle is flagged by the tile rule whatever happens in it.** A battle does
  not move the fly, so `tiles = 1` is what a long fight looks like from outside, and the after arm
  spends 48,756 of 71,673 frames in one. That is the known false-positive shape of the *tile* rule,
  the mirror of the sequence rule's legitimately-repeating explorer, and it is why the pad and
  sub-state tables above are what this row is proved on.
- **The `THROW BALL` precondition is inert in this run and is still right.** The save carries a
  party of **one** — the starter — so no forest species is ever in it and the new fact never fires;
  the 46 starts are all `Blocked` at mean 67 frames, which is row 19's cursor wait again. What the
  ROM-gated test holds is the invariant (`threw_at_a_held_species` is never set), not a drop in the
  count. The live 28 starts were the same shape.

### Gates

- `cargo test --workspace` with `FLY_ROM` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails **identically on v0.4.1** on this box with the same assertion (`/status` reports
  `total: 1` with the service still on `BOOT` 54 s after start, where the test wants the 38-rung
  ladder). A debug build of the service does not finish booting inside the test's window here.
  Pre-existing and unrelated to the macro layer; measured on both sides rather than assumed.
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: all checks passed, de-PII guard included.
- `--print-compatibility`: byte-identical to v0.4.1, 648 bytes, decoder / reward catalog / adapter
  version / roles untouched.

## 2026-09-22, section 15: the walkable window becomes the whole map

The operator: "the frontier and warp macros need to be map aware: A* over walkable tiles."
`docs/design/macros.md` section 15 is the contract, `docs/design/macros-wram.md` section 9 the
bytes, and this is the measurement. Nothing about the *choice* moves: the scene deals the same
buttons and what changed is what a chosen walk knows about the ground.

**Row 35 has no source any more.** That row — "the screen-buffer tile read disagrees with itself on
a map smaller than the screen", worked around by taking a counter's reach from the map id — was a
fact about reading tiles out of a view that cannot centre on an 8x8 map. The grid reads the map's
own block data, so it answers the same way from every tile of every map. The counter rule is left
exactly as it is: this branch changes the ground, not the amenity, and a workaround that is no
longer needed is not the same thing as one that was wrong.

### The trap hunt, before and after

20 brain minutes, the hunt's own seed, 4 sweep threads, the same connectome, the same cartridge and
the same rung-9 checkpoint in both runs. "Before" is `main` at v0.4.2, which is the branch's own
merge base, so the two rows differ by this work and nothing else:

```sh
FLY_ROM=".../Pokemon Red (U) [S][BF].gb" FLY_MACRO_BRAIN=data/fafb-v783 \
  FLY_TRAP_CHECKPOINT=.local/checkpoints/release-rank9-20260922T0254.checkpoint \
  FLY_TRAP_MINUTES=20 FLY_TRAP_MODE=macros cargo run --release -p flysim --example trap_hunt
```

| measure | before (`main`, v0.4.2) | after (this branch) |
| --- | ---: | ---: |
| distinct (map, tile) over 20 brain minutes | 286 | **489** |
| median distinct tiles in a flagged window | 16 | **55** |
| most tiles in any one window | 94 | **180** |
| macros started | 830 | 912 |
| done / blocked / timeout / refused | 726 / 97 / 6 / 0 | 820 / 90 / **1** / 8 |
| windows flagged | 61 of 73 | 70 of 73 |
| windows under 4 distinct tiles | 5 | 14 |
| frames in a battle (longest run) | 48,756 (9,549) | 50,413 (13,411) |
| frames in the overworld | 22,149 | 18,972 |
| `GO FRONTIER` done / timeout / mean net / max net tiles | 4 / 1 / 0.5 / 1 | **64** / 0 / 1.6 / **12** |
| `GO WARP` done / timeout / mean frames / max net | 14 / 2 / 403 / 30 | 4 / **0** / 751 / 26 |
| `GO ROUTE` done / timeout / mean frames / max net | 9 / 2 / 483 / 22 | 11 / 1 / 410 / 8 |
| `GO OBJECTIVE` done / timeout / max net | 3 / 1 / 8 | 5 / **0** / **42** |
| wall seconds on the development box | 1,390 | 932 |

**The ground is the measure and the ground moved.** 286 distinct tiles became 489; the median
flagged window holds 55 of them instead of 16 and the widest holds 180 instead of 94. `GO FRONTIER`
went from four walks worth a net tile each to sixty-four worth up to twelve, `GO OBJECTIVE`'s best
walk from eight net tiles to forty-two, and the only walk that spent its cap is one `GO ROUTE` that
gained forty-two tiles while doing it. Nothing timed out that used to arrive.

**The cost, named rather than buried: the flag count went the wrong way**, 61 windows to 70, and
the windows holding fewer than four tiles went 5 to 14. Every one of those is a battle. The fly
covers more ground, so it walks into more grass and more trainers: battle frames 48,756 to 50,413,
the longest single battle 9,549 frames to 13,411, overworld frames 22,149 down to 18,972. A
two-minute window spent inside one battle is a window with one tile in it, and the worst window of
the after-run is 83 macros on one tile with **no repeated sequence at all** — which is a battle,
not a loop. This file has recorded battle text as check 10's known false-positive shape since its
first section, and the `NEXT` x12 to x14 runs that flag 43 of the 70 windows are exactly it.

So the two halves of the usual reading disagree here for the first time, and this is the honest
statement of it: **more ground, more battle, more flags.** What the flag counts cannot show and the
tile counts can is that the fly is walking across maps instead of round the tile it is standing on.

### Gates

`cargo test --workspace`, `cargo clippy --all-targets` and `infra/tests/lint.sh` on the development
box. `flysim --print-compatibility` is byte-identical across the change: 648 bytes,
`0d9bfde7…707fa`, so the live checkpoint carries over.
## 2026-09-22, rows 42 and 43: the pair that undid itself

Thirty-five minutes after v0.4.2 deployed, the release watchdog flagged the same rung. Rank 9
(VIRIDIAN FOREST, next PEWTER CITY), seventy-one hours on it, and since the restart the macro
starts were `NEXT` **1264**, `BACK` **1241**, `THROW BALL` 5 and `GO WARP` 3, with the event log
alternating

```
NEXT start, NEXT done, BACK start, BACK done
```

every hold on map 51. It is the residual the previous review named and left -- "a `NEXT, BACK`
2-cycle x75 closes the run inside one battle... worth watching" -- at full scale, and the reason
that review's rule did not catch it is that **each of the two buttons was legitimate where it
stood**. `docs/design/macros.md` section 12.10 is the contract this closed against.

### The mechanism, measured

Twenty brain minutes from the release container's own rung-9 checkpoint, real cartridge, real
brain as the readout. The whole run -- **71,673 frames, all of them `battle`** -- was one battle,
on one tile, with 73 of 73 windows flagged and every window reading `NEXT, BACK` x74 or x75. The
two rows of the hunt's own sub-state table are the mechanism:

| macro start | battle sub-state | n |
| --- | --- | ---: |
| `BACK` | move list | **739** |
| `NEXT` | **main menu** | **739** |
| `NEXT` | move list, no cursor | 8 |
| `BACK` | party list | 1 |
| `MOVE 1` | main menu | 1 |
| `THROW BALL` | main menu | 1 |

1. **`NEXT` on the top-level battle menu is not the press that advances text.** `NEXT`'s script is
   one press of A. Between turns that A advances the text box, which is the button's whole
   purpose. On the top-level menu the same A press **confirms whatever the cursor sits on**, and
   the cursor sits on FIGHT -- so `NEXT` *opened the move list*. It was put there as row 7's
   backstop for a turn where every other button drops out, and it is the wrong button for that
   job twice over: it does not end a turn, and what it does instead is reopen a list.
2. **`BACK` on the move list closed it again**, which is 12.9's contract and correct: backing out
   of a list is one of exactly two answers to one. So neither rule was broken and the loop was a
   *pair*: main menu -> `NEXT` -> move list -> `BACK` -> main menu, 739 times each, at 2.4 macros
   per brain second, with the turn never resolving and the battle never ending. Section 12.2's
   rule restated at the pad: **no pair of buttons on any battle pad may undo each other with
   nothing else changing.**
3. **The bag was the last frame in the game with a cursor accepting input and no own turn.**
   `Battle::own_turn` answered `false` for it (12.6 had no observable for the bag at all; 13.1 gave
   it a cursor), so it landed on the between-turns row -- whose `NEXT` on an open bag is the A
   press that *uses* whatever the cursor holds. Not what the live loop was made of, and the same
   defect: an A press dealt where A does not advance text.

### What changed, all of it inside the macros

- **`NEXT` is off every own-turn pad**: the top-level menu, the move list, the party list and the
  bag. It stays alone on the between-turns row and stays as row 8's backstop on the forced switch,
  which is the one arm with a cursor up and no `BACK` at all, because it cannot be cancelled.
- **`MOVE 1` is the top-level menu's backstop instead**, and it is bound there whatever the seam
  makes of `wBattleMon*`: the question over that menu is 12.8's "is there a move list to open",
  FIGHT always opens, and `MOVE 1`'s script over that menu is "confirm FIGHT and stop", which reads
  no move at all. A battler the seam cannot read is not a reason to take the turn's one ending
  button away. Over an **open list** the per-slot PP rule is unchanged (row 30a, row 34).
- **The bag is the fly's turn** (`state::battle`), and its pad is the bag's own three answers:
  `ITEM` (use what the cursor is on), `THROW BALL`, `BACK`. `CONFIRM` is gone from it -- the same
  blind A press under another name -- and both scripts already navigated this list by reading its
  cursor (`macros-wram.md` 7.1). The invariant is now whole: **a battle frame with a cursor
  accepting input is the fly's turn**, the forced switch excepted because it has a pad of its own.
- **`wListMenuID` was checked for staleness rather than assumed sound**: it is zeroed by
  `DisplayTextIDInit` at the start of every text display (`macros-wram.md`), so a battle's text
  frames cannot inherit an `ITEMLISTMENU` from a bag the fly closed, and the bag reading stands.
- **The harness asks the scene the pad was dealt for, not `wIsInBattle`.** The `$ff` frame a lost
  battle passes through reads `Unknown`, whose pad is `NEXT, BACK` by contract (row 9); the first
  version of the new assertion accused that row of a battle rule it is not under.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 42 | two buttons on a battle pad undo each other with nothing else changing: `NEXT` on the top-level menu opens the move list, `BACK` on the move list closes it | every wild battle, every turn -- 739 starts each in twenty brain minutes, one battle, 73 of 73 windows flagged | `no_battle_pad_holds_both_next_and_back`, `the_battle_row_is_the_move_buttons_switch_item_and_never_next`, `each_battle_menu_deals_its_own_pad`, `the_battles_turns_advance_from_the_rung_nine_forest_checkpoint` (ROM-gated: it **fails on v0.4.2** with `NEXT` dealt on `battle/main`) | **fixed**: `NEXT` is off every pad with a cursor accepting input, and `MOVE 1` is the top-level menu's backstop, bound there whatever the battler reads as. `NEXT` keeps the between-turns row and the forced switch |
| 43 | the battle bag reads as nobody's turn, so its pad is the between-turns `NEXT` -- an A press that *uses* what the cursor holds | choosing ITEM in a battle; rare, because `ITEM` needs a potion and a hurt Pokemon | `a_battle_frame_with_a_cursor_accepting_input_is_the_flys_turn`, `the_battle_bag_is_the_flys_turn_and_deals_its_own_two_uses`, `back_is_on_a_battle_pad_only_where_a_list_is_open` | **fixed**: `own_turn` is true for the bag, and its pad is `ITEM` / `THROW BALL` / `BACK`. This is row 30c closed -- it was left in 12.6 for want of an observable, given a cursor in 13.1, and given the right *turn* here |

### The ROM-gated run, before and after

Same test, same checkpoint, sixty-seven brain minutes of the game-blind rotation, `v0.4.2`
(`592c264`) against this branch.

| measure | before (v0.4.2) | after |
| --- | --- | --- |
| verdict | **FAILED**: "`NEXT` was on the pad while a battle menu was accepting input" | **passed** |
| `NEXT` dealt on | `battle/between-turns`, **`battle/main`**, `battle/moves-unplaceable` | `battle/between-turns`, `battle/moves-unplaceable` |
| `BACK` dealt on | `battle/moves`, `battle/party` | `battle/bag`, `battle/moves`, `battle/party` |
| maps visited | **[51]** -- never left the forest | 51, 0, 12, 1, 41, 44, 42, 13, 50 (nine) |
| battles entered / ended | 14 / 15 | 26 / 25 |
| worst battle, in macros | not measurable: the run never left the fly's own turn | **275** |
| longest `NEXT`/`BACK` alternation | (the assertion fires first) | **1** |
| `MOVE 1..4` starts | 96 | **718** |
| `NEXT` / `BACK` starts | 1235 / 1315 of 3944 | 526 / 313 of 2090 |
| own-turn frames | 20,914 | 143,180 |
| `GO OBJECTIVE` on an overworld pad | map 51 only | nine maps |

### The trap hunt, before and after

Twenty brain minutes, seed 20260917, 4 sweep threads, the same connectome and the same cartridge,
from the release container's rung-9 checkpoint, **driven by the brain** rather than by
`FLY_TRAP_STUB`: this branch adds no macro type, so the thirty-one `macro_<type>` populations are
the same function in both arms and `--print-compatibility` is byte-identical at 648 bytes. The two
arms differ in the macro code and in nothing else.

| measure | before (v0.4.2) | after |
| --- | ---: | ---: |
| distinct (map, tile) | **1** | **260** |
| windows flagged | 73/73 | **68/73** |
| windows under 4 tiles | **73** | **2** |
| longest repeated sequence in a window | **`NEXT, BACK` x75** | `NEXT` x20 |
| macros started | 1489 | 797 |
| frames in `battle` | **71,673** (one battle, the whole run) | 50,600 |
| frames in `overworld` | **0** | **20,394** |
| `NEXT` starts on the main menu | **739** | **0** |
| `BACK` starts on the move list | 739 | 263 |
| `MOVE n` starts | **1** | **113** |
| `THROW BALL` starts | 1 | 63 |
| `RUN` starts | 0 | 7 |

The before arm never left the battle it resumed in and never left the tile it stood on. The after
arm fought fifty thousand frames of battle *and* walked twenty thousand frames of overworld across
260 tiles.

Raw reports: `hunt-before-20260922T0459.md` and `hunt-after-20260922T0459.md` in the coordination
state's `runs/` directory.

### Residuals, named rather than worked around

- **`NEXT` on a move list whose cursor the seam cannot place is now the largest source of it** --
  142 of the after arm's 248 `NEXT` starts, over 8,687 frames. That frame is *correctly* not the
  fly's turn (row 30b: `MoveSelectionMenu`'s coordinates appear before the engine has copied the
  active Pokemon into `wBattleMon*`), so `NEXT` there is the between-turns press and it advances.
  It is not a 2-cycle -- the ROM-gated run measures the longest `NEXT`/`BACK` alternation at
  **1** -- but it is the same unplaceable cursor 12.6 named, and the honest fix is still a WRAM
  reading rather than a pad change.
- **`BACK` is 326 of 797 starts**, all of them over an open list: 263 on the move list and 63 on a
  party list. Row 34's contract, and where it leads is a menu with `MOVE 1` on it.
- **The 68 windows still flagged are the sequence rule's dominance arm, not a cycle.** They read
  `NEXT` x11 to x20 or `BACK` x11 to x14 over windows holding **5 to 86** distinct tiles, where
  every window of the before arm read `NEXT, BACK` x74 on **one** tile. Only two windows of the
  after arm are under four tiles, against seventy-three before. A window spent in a long battle is
  one tile by construction, which is the known false-positive shape of the tile rule and why this
  row is proved on the pad and sub-state tables as well.
- **Row 41 is not reproduced here and is not closed.** The after arm's text boxes are 140 frames
  at (6, 30) of map 0x33 and smaller counts on seven other tiles, with no Pokemon Center in the
  run at all; the nurse's `YES` loop needs its own checkpoint to measure.
- **The road north is still reported rather than asserted.** The ROM-gated run leaves the forest
  and reaches maps 50, 13, 12, 1, 41, 44, 42 and 0 with `GO OBJECTIVE` on the overworld pad of all
  nine, but it does not reach map 47 -- the forest's north gate -- in sixty-seven brain minutes of
  the game-blind rotation. Row 40's note stands.

### Gates

- `cargo test --workspace` with `FLY_ROM` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on v0.4.2 on this box with the same assertion (a debug build of the
  service does not finish booting inside the test's window here). Pre-existing and unrelated to
  the macro layer.
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: all checks passed, de-PII guard included.
- `--print-compatibility`: **648 bytes, sha256 `0d9bfde7...707fa`** -- byte-identical to v0.4.1 and
  v0.4.2. Decoder, reward catalog, adapter version and roles untouched.

## 2026-09-22, rows 44 to 47: the screen the pad opens and closes again, and a menu read row-major

Thirty-one minutes after v0.4.3 deployed, the release watchdog flagged the next rung. Rank 10
(PEWTER CITY, next the BOULDER BADGE), the fly inside a Pewter building, and since the restart the
macro starts were `MENU` **82**, `BACK` **82** and `GO FRONTIER` 8, with the event log alternating

```
MENU start, MENU done, BACK start, BACK done
```

on map `0x35`. It is section 12.10's pair again -- two buttons that undo each other with nothing
else changing -- with the difference that the two are on **different scenes**, so no per-pad rule
could see it: `MENU` is on the overworld and `BACK` is on the start menu that `MENU` opens.
`docs/design/macros.md` section 12.11 is the contract this closed against.

### Which building, and why its pad was two buttons

Reproduced from the release container's own checkpoint with the real cartridge,
`examples/scene_probe.rs`:

```
- rank 10 (PEWTER CITY), badges 0, unique tiles 2132
- player: Player { map: 53, x: 3, y: 3, facing: Left }
- objective: Objective { map: 54, target: Some(Person) }
- map size: MapSize { width: 14, height: 8 }
- warps: [Warp { x: 7, y: 7, destination_warp: 4, destination_map: 52 }]
- signs: [Sign { x: 11, y: 2 }, Sign { x: 2, y: 5 }]
- people and objects: slot 1 picture 0x04 at (2, 7), slot 2 picture 0x25 at (0, 5),
  slot 3 picture 0x20 at (7, 5)
- the pad: ["GO WARP", "GO ITEM", "GO NPC", "GO FRONTIER", "MENU"]
- the map grid: 14x8 walkable 81 reachable 81 unstood 4 unknown 0
- `next_hop(Region { map: 53, part: 0 }, 0x36)` = None, neighbours []
- `ways(Exit)` = [], `ways(Passage)` = [Warp(0)], `ways(Route)` = []
- `objective_goals` = [], `objective_targets` = []
```

Map `0x35` is **the upper floor of the Pewter museum** -- `0x34` is its ground floor, `0x36` the
gym, `0x38` the mart, `0x3a` the centre -- fourteen blocks by eight, one staircase down at (7, 7),
two exhibit signs and three people. Every candidate list on it empties, which is why the live pad
was `MENU` and an occasional `GO FRONTIER`:

| button | why it was not there |
| --- | --- |
| `GO OBJECTIVE` | the objective is right -- map `0x36` with a **person** on it, which is rung 11's gym leader -- but `geography` has no row for the museum, so `next_hop` from map 53 answers `None` with no neighbours, and `objective_goals` is empty |
| `GO OUT` | the museum's upper floor has **no exit-class warp at all**: its one way out is a staircase, which is a `Passage` |
| `GO WARP` | on the pad at a fresh restore, and off it live: `ways(Passage)`' tier 3 is `unexcluded_exits`, which the blocked window empties for ten brain minutes after a refused walk |
| `GO ITEM`, `GO NPC` | the two signs and three exhibits are **reached**, and a reached target is retired for the session (12.1) |
| `TALK` | the fly at (3, 3) facing Left has nothing in front of it |
| `GO SHOP`, `GO HEAL` | `geography::area_of` has no row for the museum, so the area's errands cannot be aimed at from inside it |
| `GO FRONTIER` | on the pad while any of the four unstood tiles is outside the blocked window, which is the 8 starts in thirty minutes |

### The survey that named the second trap

`THROW BALL` was **63 starts and 63 `blocked`** in v0.4.3's own after-arm, mean sixty-nine frames,
and the first reading of that -- the bag had not finished drawing when the step read the cursor --
is true and is not the whole of it. Instrumenting *where* a macro reports `blocked` from the rung-9
checkpoint answered `battle/party` and `battle/between-turns`, never `battle/bag`, and a frame dump
of the menu bytes across one `THROW BALL` says why:

```
DUMP 21  topy=14 topx=9  cur=1 max=1 watch=0x11  sub=main     (the cursor walked DOWN)
DUMP 35  topy=14 topx=15 cur=1 max=1 watch=0x21  sub=main     (then RIGHT)
DUMP 52  topy=14 topx=15 cur=0 max=1 watch=0x21  sub=main     (then UP: the step's target)
DUMP 55  topy=14 topx=15 cur=2 max=1 watch=0x21  sub=main     (A: the game adds 2 for the column)
DUMP 60  topy=1  topx=0  cur=0 max=0 watch=0x03  sub=party  list=0x02
```

The A press at the right column's first row opens the **party list**. Red's battle menu is two
*columns* -- the screen reads `FIGHT PKMN` over `ITEM RUN` -- and `wCurrentMenuItem` is the row
inside the column the cursor is in, with two added for the right column on selection. So the
game's order is **FIGHT, ITEM, PKMN, RUN**, and `macros::cartridge::battle_entry` had `PKMN` 1 and
`ITEM` 2: the row-major reading of the picture. Every macro that meant to open the bag opened the
party list and every macro that meant to open the party list opened the bag, for as long as the
four constants have existed. The fake's own two-by-two moved row-major too, which is why no unit
test could have caught it.

### What changed, all of it inside the macros

- **`MENU` is on no pad.** Not narrowed: nothing in the vocabulary uses the start menu except as a
  scene to leave, so there is nothing behind the button. It stays a type, a population, a tag and a
  script -- thirty-one channels, the roles and `--print-compatibility` do not move -- and the start
  menu is still the fly's to open with the raw START button.
- **`ways` gains a room's last resort**, the one `GO ROUTE` has had outdoors since 13.1: with
  nothing else on this map worth walking to (`palette::stranded`), the exits of that kind come back
  ignoring the blocked window, the one toward the objective preferred.
- **A cursor step waits for the list it was built for.** A `Listing` says which list it is and a
  step says which list its target indexes into; a step whose list is not up waits rather than
  pressing at the list it has already answered, and takes its order and budget from that list on
  the first frame it accepts input.
- **`battle_entry`'s `ITEM` and `PKMN` swap**, and the fake's geometry becomes column-major.
- **`BACK` on the move list only where the battler reads**, else `MOVE 1` alone.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 44 | `MENU` opens the start menu and that scene's `BACK` closes it again: a pair split across two scenes, and on a map where every other list has emptied it is the whole pad | thirty brain minutes on map `0x35`, `MENU` 82 starts and `BACK` 82; on v0.4.3 `MENU` is dealt on every overworld map | `menu_is_on_no_scenes_pad`, `the_overworld_plan_never_truncates_the_frontier_away`, `the_fly_leaves_the_pewter_building_from_the_rung_ten_checkpoint` (ROM-gated: it **fails on v0.4.3** with `MENU` on the pad of maps 2, 52, 53, 54, 55, 57, 58 and 147 starts) | **fixed**: `MENU` is off every scene's set. A macro whose precondition holds wherever the fly stands and whose effect a neighbouring scene undoes is section 12.2's trap, and the start menu has nothing in it for the fly |
| 45 | the overworld's never-empty guarantee *was* `MENU`, so taking it off could strand a room -- and the museum's one way out is a passage the blocked window rests | the same thirty minutes: `ways(Passage)` empty, `ways(Exit)` empty because there is no exit-class warp at all | `a_room_whose_only_way_out_the_ledger_rests_still_offers_it`, `an_overworld_pad_is_never_one_button_that_undoes_itself`, `no_playable_scene_and_no_sub_state_deals_an_empty_pad` | **fixed**: `ways`' last resort covers a room as well as a route. A map with **no way out at all** is a genuinely empty pad, is asserted as such, and is reported by `game.padEmptyMs` -- no map in Red is that shape |
| 46 | Red's battle menu is two columns, so `battle_entry`'s row-major `PKMN` 1 / `ITEM` 2 sent `ITEM` and `THROW BALL` to the party list and `SWITCH` to the bag | every `THROW BALL`, `ITEM` and `SWITCH` ever started: 63 starts and 63 `blocked` in v0.4.3's after-arm, `SWITCH` 15 of them | `the_battle_menus_two_columns_put_item_under_fight_and_pkmn_beside_it`, `throw_ball_waits_for_the_bag_rather_than_reading_the_menu_it_came_from`, `the_battles_turns_advance_from_the_rung_nine_forest_checkpoint` (ROM-gated, now asserting `THROW BALL` never blocks) | **fixed**: the order is FIGHT, ITEM, PKMN, RUN, surveyed byte by byte. ROM-gated from the rung-9 checkpoint: `THROW BALL` 15 starts and 0 blocked, `SWITCH` 17 and 0, against 6 of 6 and 15 before |
| 47 | a move list whose battler the seam cannot read binds no `MOVE n`, so its pad is `BACK` alone -- which closes the list `MOVE 1` underneath had just opened | `BACK` was 263 of 797 macro starts in v0.4.3's after-arm, every one over an open move list | `the_move_list_deals_back_only_where_the_moves_can_be_read` | **fixed**: `BACK` is dealt on the move list only while `wBattleMon*` reads; otherwise `MOVE 1` alone, whose script confirms where the cursor stands, which is the press that ends a turn |

### The ROM-gated runs, before and after

From the rung-10 checkpoint, thirty-three brain minutes of the game-blind rotation, v0.4.3
(`928d66b`) against this branch:

| measure | before (v0.4.3) | after |
| --- | ---: | ---: |
| maps whose overworld pad dealt `MENU` | **7** (2, 52, 53, 54, 55, 57, 58) | **0** |
| `MENU` starts | **147** | **0** |
| overworld pads that were empty | 0 | 0 |
| frame the fly left map `0x35` | (it left, then came back) | **182** |

From the rung-9 forest checkpoint, sixty-seven brain minutes, the same two arms:

| measure | before (v0.4.3) | after |
| --- | ---: | ---: |
| `THROW BALL` starts / blocked | 6 / **6** | 15 / **0** |
| `SWITCH` starts / blocked | 17 / **15** | 17 / **0** |
| `RUN` blocked | 6 | 0 |
| battles entered / ended | 8 / 8 | 4 / 3 (one still running at the budget) |
| worst battle, in macros | 275 | 450 |

The worst battle grows because a fly whose `SWITCH` and `ITEM` reach their own lists spends turns
switching and healing instead of only attacking. The claim the assertion carries is unchanged and
still holds: a battle **ends**, on a bounded number of macros.

### The trap hunt, before and after -- and why it says nothing about this trap

Twenty brain minutes, seed 20260917, 4 sweep threads, the same connectome and the same cartridge,
from the release container's own rung-10 checkpoint, driven by the brain.

| measure | before (v0.4.3) | after |
| --- | ---: | ---: |
| distinct (map, tile) | 175 | 175 |
| windows flagged | 73/73 | 73/73 |
| macros started | 1413 | 1413 |
| `MENU` starts | **0** | **0** |
| `THROW BALL` blocked | 0 (never started) | 0 (never started) |
| frames in `dialog` | 63,668 | 63,668 |

**The two arms are identical, and that is the honest result rather than a null one.** The reached
and blocked ledgers are session state that a restore clears (12.1), so a restored fly is not in the
state the loop needed: it walks out of the museum's upper floor in one `GO WARP` and never presses
`MENU` at all, so removing a channel the decoder never picked changes nothing downstream. The trap
hunt cannot reproduce a ledger-built loop from a checkpoint, and this is the first review where
that has mattered; the proof for rows 44 and 45 is the pad rules and the ROM-gated run above.

What the hunt *does* reproduce, from this checkpoint, is **row 41** -- the Pokémon Center nurse's
box -- at full scale: 62,804 of 71,673 frames are one text box on **one tile** of map `0x3a` at
(3, 3), with `YES` **1,278** of 1,295 macro starts and the run ending there. That is the next trap
and it now has a checkpoint of its own.

### Residuals, named rather than worked around

- **Row 41 is reproduced and is the next brief.** 1,295 of 1,413 macro starts in both arms are
  `YES` at the nurse's counter, on one tile, for the last eighteen brain minutes of the run.
- **`MOVE n` reports `blocked` 890 times in 1,431 macros** in the forest ROM run, every one of them
  with the move list drawn and its cursor *placeable* but not accepting input: no press moves the
  cursor and the step spends its budget (`reason=budget target=2 max=3 kind=BattleMoves here=1`,
  535 of them). That is row 30b's unplaceable cursor inverted, it is unchanged from v0.4.3 (747
  blocked in 1,260), and the honest fix is a WRAM reading rather than a pad change.
- **The museum is not in `geography`**, which is why `GO OBJECTIVE`, `GO SHOP` and `GO HEAL` are
  all off the pad inside it. Adding the row would give the fly the road to the gym from indoors;
  this branch did not, because the map graph is a data change with its own survey (12.7) and the
  way out now stands on its own.
- **A map with no way out at all deals an empty pad.** No map in Red is that shape; it is asserted
  and reported rather than covered.

### Gates

- `cargo test --workspace` with `FLY_ROM` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on v0.4.3 on this box (a debug build of the service does not finish
  booting inside the test's window here). Pre-existing and unrelated to the macro layer.
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: all checks passed, de-PII guard included.
- `--print-compatibility`: **648 bytes, sha256 `0d9bfde7...707fa`** -- byte-identical to v0.4.1,
  v0.4.2 and v0.4.3. Decoder, reward catalog, adapter version and roles untouched.

## 2026-09-22, row 41 worked: the nurse's box is a ring, and `YES` and `NEXT` are one press

Named in the rung-9 review and again by 12.11 as the next trap, and flagged live by the watchdog
(v0.4.4, macros mode) within the hour: rank 10 (PEWTER CITY), the fly in the **Pewter Pokémon
Center**, and since the 09:39 restart the macro starts were `YES` **2,142**, `TALK` 107,
`GO FRONTIER` 26, `BACK` 24, with the event log's tail

```
YES start, YES done, YES start, YES done, ...
```

for ever. `docs/design/macros.md` section 12.12 is the design; this is the reproduction, the
survey, and the before/after.

### Where the fly was standing

`examples/scene_probe.rs` from the live checkpoint:

- map `0x3a`, **14x8**, the player at **(3, 3) facing up**, two warps at (3, 7) and (4, 7) out to
  Pewter City (map 2);
- two sprites: picture `0x29` (`SPRITE_NURSE`) at **(3, 1)** and picture `0x38` at (1, 3) -- so the
  nurse is **two tiles away**, behind her counter, which is the reach
  `IsSpriteOrSignInFrontOfPlayer` doubles over a counter tile;
- the scene reads `Dialog` (`font=0x01`, the full-width border drawn, `textbox=0x01`) and the pad is
  **`NEXT`, `YES`, `NO`**;
- the objective is map `0x36` -- the Pewter gym, rung 11's BOULDER BADGE -- and `next_hop` from here
  answers map 2, so the road out is known;
- every overworld candidate list is empty (`ways(Exit)` `[]` because both warps are `exit_visited`,
  `objective_goals` `[]`, `untalked_people` `[]`, `frontier_aims` `[]`), which is why `TALK` and the
  dialog were the pad.

### The survey: the conversation, one raw A pulse at a time

`FLY_PROBE_CATCH=nurse` leaves the box with B and then pulses A, printing `scene`, the text box's
two halves, the two-option menu's cursor bytes, the top-right corner's frame tiles and both lines of
decoded text for every state the conversation passes through. **The party first**, because the
loop's premise is that it is already full:

```
- slot 0 species 0x09 level 24 hp 70/70 status Healthy
- `party_needs_rest` = false, `party_rested` = true
```

Then the ring, elided to the states that matter (`A#n` is the pulse):

```
at the checkpoint  Dialog  cursor=(8,12,0,1,0x03)  corner=empty  | POKeMON back to | perfect health!  v
A#0    Dialog  cursor=(8,12,1,1,0x03)  corner=empty                       |                  |
A#4    Dialog  cursor=(8,12,1,1,0x03)  corner=empty   | Welcome to our     | POKeMON CENTER!  v
A#9    Dialog  cursor=(8,12,1,1,0x03)  corner=empty   | We heal your       | POKeMON back to  v
A#12   Dialog  cursor=(8,12,1,1,0x03)  corner=empty   | POKeMON back to    | perfect health!  v
A#13   Dialog  cursor=(8,12,0,1,0x03)  corner=BOX     | POKeMON back to    | perfect health!
A#15   Dialog  cursor=(8,12,0,1,0x03)  corner=empty   | OK. We'll need     | your POKeMON.
A#33   Dialog  cursor=(8,12,0,1,0x03)  corner=empty   | Thank you!         | Your POKeMON are v
A#38   Dialog  cursor=(8,12,0,1,0x03)  corner=empty   | Your POKeMON are   | fighting fit!    v
A#42   Dialog  cursor=(8,12,0,1,0x03)  corner=empty   | We hope to see     | you again!
A#45   Overworld  open=false  waiting=false
A#46   Dialog  ... the whole thing again, and again, and again
```

Five things that settles:

1. **The cycle is forty-six A presses and the box is a *choice* on exactly one of them** (A#13,
   then A#59, A#105, A#151...). The other forty-five are plain text, where `NEXT` and `YES` are the
   same A press with two channel names and `NO`'s B advances a plain box too.
2. **The box open at the checkpoint is the closing line, not the prompt.** So the first of the
   brief's three candidate readings is out: the fly was not sitting on a YES/NO box re-offering
   itself; it was walking a ring of text.
3. **`HEAL` is not in the loop at all.** Its precondition reads the live party and
   `party_needs_rest` answers `false`, so the button was off the pad throughout -- and `HEAL`'s
   middle step is a *read* of the party (`Step::Rested`), not a timer, so it cannot spin on a party
   that is already full either. The third candidate reading is out too.
4. **The box closes for a single frame and the next A press reopens it**, because the fly is still
   standing at (3, 3) facing the nurse over the counter. That is the ring's own door, and `TALK` is
   what opens it from the overworld side.
5. **The two-option menu's cursor bytes are stale on all forty-six frames**
   (`wTopMenuItemY` 8, `wTopMenuItemX` 12, `wMaxMenuItem` 1, `wMenuWatchedKeys` `$03`) while the
   box itself is drawn on one. So "is a choice open" needs the **drawn border** beside them, at
   (11, 6)-(19, 11) -- which is the same construction `text_box().waiting` already makes for the
   dialogue box. `docs/design/macros-wram.md`'s table carries the reading and its limit.

### Why `TALK` fired 107 times and retired nothing

`TALK`'s precondition is `palette::facing_untalked`, which reaches **over a counter** because the
cartridge does. Its talked-ledger entry came from `executor::talk_target`, which looked **one tile
ahead** -- at the counter tile, which holds nothing. So the macro was bound by one reading and
recorded by a shorter one, and the ledger never learned that the nurse had been talked to: the
button came back every hold for ever. A precondition and the ledger that answers it have to be the
same question, and that was the second half of the trap.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 41 | the nurse's conversation is a ring of forty-six A presses that ends where it began, and the dialog pad deals two names for the A press that walks it | standing at a Pokémon Center's counter with a party that is already full -- which is every visit after a heal, and the state a `GO HEAL` errand leaves the fly in | `talk_is_off_the_pad_at_a_nurse_the_party_has_no_use_for`, `the_nurses_prompt_offers_only_the_answer_that_changes_something`, `a_completed_heal_writes_the_nurse_into_the_talked_ledger`, `a_declined_heal_writes_the_nurse_into_the_talked_ledger`, `the_fly_leaves_the_pokemon_center_from_the_rung_ten_checkpoint` (ROM-gated) | **fixed**: `TALK` is off the pad at a nurse the party has no use for; her prompt deals only the answer that changes something; `NEXT` is off any readable YES/NO pad, because an A press there *is* `YES`; and a completed heal or a declined prompt retires her |
| 48 | `TALK` is bound by a reach that goes over a counter and recorded by one that does not, so a counter person is never retired | any mart clerk or centre nurse, since the counter reach was added | `a_completed_heal_writes_the_nurse_into_the_talked_ledger` (the ledger entry is the assertion) | **fixed**: the ledger entry comes from `palette::facing_target`, which is `TALK`'s own precondition |
| 49 | a YES/NO answer that brings the same prompt straight back | any readable two-option box the answer does not settle | `a_yes_no_box_that_reopens_unchanged_takes_that_answer_off_the_pad`, `a_prompt_that_does_not_come_back_excludes_nothing` | **fixed**: `TargetKey::Answer { at, yes }` in the blocked ledger, same ten-minute window as a walk's target, armed for one hold after the answer. The exclusion narrows a pad and never empties one |
| 50 | `MOVE n` reports `blocked` with the move list drawn and its cursor placeable but not accepting input | every battle | `a_move_list_is_the_box_on_screen_and_not_the_cursor_bytes_it_left_behind`, and the `MOVE n` blocked share in `the_battles_turns_advance_from_the_rung_nine_forest_checkpoint` (ROM) | **fixed** (2026-09-22, `docs/design/macros.md` 12.18), and the half of the row that was wrong is where the fix is: the list was **not** drawn. `MoveSelectionMenu`'s cursor bytes are never cleared and `SelectMenuItem` decrements `wCurrentMenuItem` back into the one-based range on its way out, so every frame of a turn's text and animation read as an open list with a placeable cursor. A menu is up while its **box** is on screen -- surveyed by pressing at every battle frame with a rollback pulse, honoured on 264 frames of 3,102 by the cursor bytes alone and on **231 of 231** by the bytes and the box |

### The ROM-gated run, from the live checkpoint

`FLY_CENTER_CHECKPOINT`, macros mode, the stub rotation, 120,000 frames (33.5 brain minutes):

| measure | value |
| --- | --- |
| the checkpoint's map | `0x3a`, party 70/70 and healthy |
| left map `0x3a` on | **frame 326** |
| macros spent in the centre | **6**: `GO OBJECTIVE` 1, `NEXT` 2, `NO` 1, `YES` 2 |
| route | `0x3a` -> Pewter City (2) -> **the Pewter gym (0x36)**, which is the objective |
| `YES` starts in the centre | **2**, against 1,423 in the before hunt from the same room |
| `NEXT` on a pad with a readable prompt open | **0** |
| `TALK` on a pad at a rested nurse | **0** |
| dialog frames / of those, a readable prompt | 19,409 / **63** |

The two `YES` presses are the checkpoint's own half-finished conversation: it resumes inside a plain
text box, whose pad is `NEXT`, `YES`, `NO` by contract, and two A presses closed the boxes that were
left. Then the prompt came up, the party was full, the pad was `NO` alone, and the fly was out. The
`MOVE n` blocked counts in the table above are the gym's battles and are row 50, unchanged.

### The trap hunt, before and after

Twenty brain minutes, seed 20260917, 4 sweep threads, the same connectome and the same cartridge,
from the release container's own rung-10 Pokémon Center checkpoint, **driven by the brain**.

| measure | before (v0.4.4) | after |
| --- | ---: | ---: |
| distinct (map, tile) | **1** | **437** |
| windows flagged | 73/73 | **54/73** |
| macros started | 1494 | 1165 |
| `YES` starts | **1423** | 108 |
| `YES` presses in a text box on map `0x3a` | **1424** | **4** |
| `TALK` starts | 68 | 11 |
| `GO HEAL` starts | 2 | 0 |
| frames in `dialog` | **69,469** | 5,492 |
| frames in `overworld` | 2,204 | **44,817** |
| frames in `battle` | 0 | 10,518 |
| worst single text box | 69,469 frames, map `0x3a` (3, 3) | 2,318 frames, map `0x02` (16, 17) |
| the map at the end | none -- a text box over it | Pewter City, `40x36`, 879 walkable |

**The before arm is row 41 whole**: one tile for twenty brain minutes, every window flagged, and
`YES x21` in every one of them. The after arm leaves the centre in the first window, walks Pewter
City, finds the gym and fights in it -- `YES` on map `0x3a` goes **1424 → 4**, and the four are the
checkpoint's own half-finished conversation plus the one `NO` that answered the prompt.

### Residuals, named rather than worked around

- **54 of 73 windows still flag, and they are a different trap on ground the before arm never
  reached.** In Pewter City and the gym the sequences are `GO FRONTIER` runs of up to 112, and
  `GO OBJECTIVE, BACK, GO FRONTIER, GO FRONTIER` and `GO OUT, BACK, GO FRONTIER, GO FRONTIER` at
  x37, with `BACK` pressed 189 times in a text box on map `0x02`. A window with 150 distinct tiles
  in it flags on the *repeat* rule and not the tile rule, which is the detector working: the fly is
  covering ground and still cycling four macros. That is the next brief, and it is a loop behind
  the loop in front of it exactly as rows 1, 2b, 23, 24 and 41 were.
- **`MOVE n` still reports `blocked` with the move list drawn and its cursor placeable but not
  accepting input** (row 50): 24 of 27 `MOVE 2..4` in the hunt, 222 of 224 `MOVE 4` in the ROM run,
  mean 223-238 frames, which is the cursor step spending its whole wait. Unchanged from v0.4.3 and
  v0.4.4 and needing a WRAM reading rather than a pad change.
- **`yes_no_prompt` reads one box and says so.** Red places a two-option menu where the script
  asking for it says; the nurse's is at (11, 6)-(19, 11) and surveyed, and a prompt drawn elsewhere
  reads `false` and keeps the pad it had. The reopen exclusion therefore only fires on a prompt this
  crate can read, which is the honest half of a general rule.
- **The declined-prompt talked entry is 12.4 inverted for one person.** It is justified by the pad:
  `NO` is only ever offered at her prompt when the party is already full. A future macro that
  declined a heal for some other reason would want that revisited.

### Gates

- `cargo test --workspace` with `FLY_ROM` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which fails identically on v0.4.4 on this box (a debug build of the service does not finish
  booting inside the test's window here). Pre-existing and unrelated to the macro layer.
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: all checks passed, de-PII guard included.
- `--print-compatibility`: **648 bytes, sha256 `0d9bfde7...707fa`** -- byte-identical to v0.4.1
  through v0.4.4. Decoder, reward catalog, adapter version and roles untouched.

## 2026-09-22, rows 51 to 53: the road the museum had no row for, and a `BACK` with no box under it

Flagged live by the watchdog within the hour of v0.4.5 going out: rank 10 (PEWTER CITY) for five
and a half hours, the objective rung 11's BOULDER BADGE whose place is the gym leader in map
`0x36`, and since the 11:30 restart the macro starts were `GO FRONTIER` **1,235**, `BACK` **678**,
`GO OBJECTIVE` 267, `GO OUT` 242, `YES` 169. Between 11:46 and 12:17 the ratchet fired **two**
"Stuck: rolled back to PEWTER CITY" recoveries, attempts 0 -> 2. The fly was on map 52, the Pewter
museum's ground floor. This is the residual the row-41 review named: `BACK` 189 times "in a text
box" on map `0x02`, surrounded by `GO OBJECTIVE` and `GO FRONTIER`, with `GO FRONTIER` runs of 112.

`docs/design/macros.md` sections 12.13 to 12.16 are the design; this is the reproduction, the
surveys and the before/after.

### Where the fly was standing

`examples/scene_probe.rs` from the live checkpoint:

- map `0x34` (52), **20x8**, the player at **(10, 7)** facing up -- standing on the museum's own
  front doormat, one of four `LAST_MAP` warps on the south wall;
- the scene reads **`Unknown`**, not `Dialog`: `font=0x00` and **no text box at all**
  (`corners=(0x10,0x10,0x10,0x10)`), with `flags5=0x24` -- the cartridge holding the joypad. The
  pad is **`NEXT`, `BACK`**;
- the objective is map `0x36` and `next_hop(Region { map: 52 }, 0x36)` answers **`None`**, because
  `neighbours(52)` is **empty**: the museum has no row on the map graph. So
  `objective_goals` is `[]`, `GO OBJECTIVE` is off the pad, and so are `GO SHOP` and `GO HEAL`;
- the map grid decodes: **98 walkable tiles, 62 reachable from the door, 39 never stood on** --
  and `frontier_aims` offers **40** of them, almost all behind the admission desk on the east side.

### The three mechanisms, and none of them is the text box

1. **`BACK` was never in a text box.** It is on no overworld pad and on no dialog pad, so every one
   of the 678 was dealt by `Scene::Unknown` -- which is two states under one name. A screen this
   crate cannot name (the Pokedex, the trainer card, OPTION) is one, and the hunt's own attribution
   counts `unknown` frames as "a text box" (`dialog_map`), which is what made the residual read the
   way it did. The other is a frame of the **overworld** with the cartridge driving: `detect`'s
   overworld branch needs `controllable`, and a warp in flight, a scripted push-back or a guide
   walking the fly through a door all fail it. `NEXT` and `BACK` there are an A and a B pressed into
   somebody else's script.
2. **The museum is not on the map graph**, which the row-41 review named as a residual and left:
   "adding the row would give the fly the road to the gym from indoors". Both rows come from the
   cartridge's own warp table rather than from counting: Pewter City names `0x34` at (14, 7) and at
   (19, 5) -- the museum's two doors -- and `0x34` names `0x35` at (7, 7), the staircase.
3. **The museum's frontier is unreachable and the blocked ledger is a window.** A `GO FRONTIER`
   that can reach none of its goals refuses `no route` and writes all forty tiles to the ledger;
   ten brain minutes later they are candidates again and it refuses again, once per hold.

And the ratchet's two rollbacks were the ratchet working to contract: its stall window is restarted
by *exploration*, and two museum floors and a town the run had already covered earn none. Entering
a map for the first time does count -- a new map is a map's worth of tiles nobody has stood on --
which is the half of the 2026-09-17 rule that already held and is now pinned by a test of its own.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 51 | `Scene::Unknown` deals `NEXT` and `BACK` on a frame of the overworld the cartridge is driving, where an A and a B press are presses into a script | every warp, every scripted push-back, every guide that walks the fly somewhere -- `BACK` 678 starts in 47 live minutes, 189 of them on map `0x02` | `an_unknown_frame_with_no_box_on_it_deals_nothing`, `an_unknown_scene_is_dialog_with_advance_only`, `the_fly_reaches_the_pewter_gym_from_the_rung_ten_checkpoint` (ROM-gated: `BACK` in a box 0, unknown pads with no box 0) | **fixed**: the pad is dealt on whether a box is drawn. A scripted overworld frame is an empty pad the fly waits out, and it is the one empty pad that ends itself -- the cartridge gives the buttons back within a few frames |
| 52 | a building with no row on the map graph has no neighbours, so from inside it there is no road to the objective and no errand either | the whole rung-10 stall: `GO OBJECTIVE`, `GO SHOP` and `GO HEAL` all off the pad on maps `0x34` and `0x35` | `the_museums_two_floors_know_the_road_to_the_gym`, `the_hop_count_is_the_road_measured_rather_than_named`, the ROM run below | **fixed**: two rows, both surveyed from the cartridge's warp table. The upper floor has no *area* -- no front door of its own, like a bedroom -- so it offers no errand, and its road out is the staircase |
| 53 | a frontier the route search cannot reach is excluded by a ten brain minute window, so it comes back every ten minutes for ever | `GO FRONTIER` 1,235 starts in 47 minutes over two museum floors and a covered town; 39 unstood tiles on map `0x34`, almost all behind the admission desk | `a_frontier_no_walk_can_reach_takes_go_frontier_off_the_pad_and_keeps_it_off`, `a_frontier_mark_is_the_stood_ledgers_to_clear`, `standing_on_new_ground_is_what_clears_a_maps_frontier_mark` | **fixed**: the refusal is remembered per map with no window, beside the pushed-tile ledger of row 37, and cleared by the fly standing on ground of that map it had not stood on before -- the only event that can change which tiles it can reach |

### The ROM-gated run, from the live checkpoint

`FLY_PEWTER_CHECKPOINT`, macros mode, the stub rotation, 200,000 frames (55.8 brain minutes):

| measure | value |
| --- | --- |
| the checkpoint's map | `0x34`, the museum's ground floor, on its doormat |
| reached **map `0x36`**, the gym's interior | **frame 2,423, on 15 macros** |
| `TALK` on the pad inside the gym | yes -- and 15 `TALK` starts on map `0x36` |
| `GO OBJECTIVE` bound on the gym's own pad | yes |
| `BACK` on a `dialog` or `unknown` frame | **0** (189 in the before hunt, on map `0x02` alone) |
| pads dealt on an `unknown` frame with nothing drawn | **0** |
| `GO FRONTIER` starts on the museum's two floors | **0** (1,235 live over those floors and the town) |
| `GO FRONTIER` starts overall | 1,247 -- 649 of them in Pewter City itself |
| route | `0x34` -> Pewter City -> the gym, then the town's shops and houses |
| the rank at the end | 10 (PEWTER CITY), **0 badges**: the road is open, the badge is not won |

The fly is out of the museum and into the gym in the first minute, and then spends the run in
Pewter City's buildings: the town's errands are session state and the 11:30 restart re-armed both,
so `GO OBJECTIVE` aims at the mart and the centre before the leader (section 13, and `BUY
ANTIDOTE` 3, `BUY BALL` 2, `HEAL` 1 in the same run say it was paid).

### The trap hunt, before and after — and it does not improve

Twenty brain minutes, seed 20260917, 4 sweep threads, the same connectome and the same cartridge,
from the release container's own rung-10 Pewter checkpoint, **driven by the brain**.

| measure | before (v0.4.5) | after |
| --- | ---: | ---: |
| distinct (map, tile) | **193** | 175 |
| windows flagged | **59/73** | 69/73 |
| macros started | 1283 | 1056 |
| `BACK` starts | **334** | 133 |
| `BACK` in a text box on map `0x02` | **291** | **0** |
| `BACK` on any text box | 295 | **0** |
| `GO FRONTIER` starts | 531 | 242 |
| `GO HEAL` starts | 0 | **204** |
| frames in `overworld` | 43,797 | 30,861 |
| frames in `unknown` | 16,595 | 13,990 |
| frames in `battle` | 6,948 | **20,894** |
| recoveries | 0 | 0 |
| the repeated sequence | `BACK, GO FRONTIER, GO FRONTIER, GO OBJECTIVE` x16 and `GO OUT, BACK, GO FRONTIER, GO FRONTIER` x37 | `GO FRONTIER, GO HEAL, GO ROUTE` x42 |

**The before arm is the live loop whole**: the two four-macro cycles it flags are the two the
watchdog's own log named, `BACK` is 291 of them in a text box on map `0x02`, and the fly ends the
run having covered 193 tiles of a 879-tile town.

**The after arm does not improve on either of the hunt's two measures, and this says so.** `BACK`
in a box goes 295 -> 0, which is row 51 closed; the museum is left in the first window and never
returned to, which is rows 52 and 53. What replaces the old cycle is a *new* one on the same five
tiles -- `GO FRONTIER, GO HEAL, GO ROUTE` x42 from brain minute 1.0 to 8.5 -- and after that the
fly covers ground (up to 103 tiles in a window) and spends the last four brain minutes inside one
battle, which the hunt's tile rule flags as hard as it flags a loop. Battle frames go **6,948 ->
20,894**, and section 15 of `docs/design/macros.md` measured the same trade the same way: a fly
that covers more ground walks into more grass, and the hunt cannot tell a long fight from a stall.

| # | trap | trigger | test | fix, or why it is left |
| ---: | --- | --- | --- | --- |
| 54 | `GO FRONTIER`, `GO HEAL` and `GO ROUTE` cycle on five tiles: three walks that each end where they began | rung 10: brain minutes 1.0 to 8.5, `GO HEAL` **204** starts at a mean net of 0.0 tiles and a mean reach of 0.0, `GO ROUTE` 211 at a net of 0.2. Rung 11, the same cycle one town on: **14 distinct tiles in six brain minutes**, 17 of 17 windows flagged, `GO FRONTIER` 122 / `GO HEAL` 129 / `GO ROUTE` 126, **every one `done` at a mean net of 0.0**, printed as `GO ROUTE, GO FRONTIER, GO HEAL` x34 to x42 | `an_errand_does_not_settle_on_the_doormat_it_is_standing_on`, `an_errand_is_paid_by_a_building_this_run_has_already_been_inside`, `a_frame_mid_step_is_read_from_the_tile_the_screen_is_centred_on`, `a_decode_the_screen_disagrees_with_is_refused_mid_step_too`, `the_tile_a_step_is_landing_on_is_ground_the_run_has_covered`, `an_edge_the_table_cannot_name_stops_being_somewhere_new_once_it_is_stood_on`, and both ROM runs below | **fixed, and both readings were right.** Four facts, all measured: the coordinates change at the **end** of a step, so the grid was refused on every moving frame and every walk was planned over the ten-by-nine window; the tile a step is landing on was unrecorded for fifteen frames of every sixteen, so the fly's own next tile was a frontier it arrived at without moving; an errand's aim at a door the fly was standing on settled where it stood, and a completed errand walk writes the reached ledger, so the button came back every hold; and the errand ledger is session state, so a restore re-armed a town the run had already shopped and healed in. `docs/design/macros.md` section 12.17 |
| 54b | an **edge** the geography table has no row for is "somewhere new" for ever | Route 3: the cartridge reports its connections as **north and west** (`wCurMapConnections`; `warps: []`), the table carries west and **east**, so the seven walkable tiles of its north edge answered "leads somewhere this run has not stood on" on every hold, with `GO OBJECTIVE` off the pad beside them because nothing on that map leads to the objective | `an_edge_the_table_cannot_name_stops_being_somewhere_new_once_it_is_stood_on` | **fixed, narrowly.** A warp's destination is a byte the cartridge publishes, so `None` there is the `LAST_MAP` case row 2 already handles; an edge's comes only from `geography::connected`, so `None` there means the table cannot name it and never will. The only record left is the adapter's boundary ledger, and an edge the run has already stood on is not somewhere new. **The table row itself is not guessed at**: which map is north of Route 3 is a survey nobody has run, and it is a residual below |

### Residuals, named rather than worked around

- ~~**The whole-map grid is refused while the fly is moving.**~~ **Worked, 2026-09-22 (row 54).**
  The guess was the wrong way round: `FLY_PROBE_CATCH=step` in `examples/scene_probe.rs` found the
  coordinates change at the **end** of a sixteen-frame step, so it is the screen that is one tile
  ahead of `wYCoord` and not `wYCoord` ahead of the screen. The reader now measures which tile the
  screen is centred on. `docs/design/macros-wram.md` section 9 has the frame-by-frame trace and the
  candidates; `$cfc5` tracks a step exactly and is recorded there **unused**, because
  `gen_symbols.py` refuses a hand-written address and the checkout `resolve_wram.py` reads is not
  on this box.
- ~~**The town's errands are session state**~~, so every restart re-armed them. **Worked,
  2026-09-22 (row 54):** the adapter's lifetime `map_visited` is asked beside the session ledger,
  and a building this run has already been inside pays the errand whichever one remembers it.
- **`MOVE n` still reports `blocked`** with the move list drawn and its cursor placeable but not
  accepting input (row 50). It is now **the largest thing in the way**: after row 54 the fly wins
  the Boulder Badge and then spends 30,809 frames in one battle on the rung-10 arm and 13,251 on
  the rung-11 arm, and the hunt flags every window of both because its tile rule cannot tell a long
  battle from a stall. Unchanged since v0.4.3.
- **Which map is north of Route 3.** The cartridge says that edge is connected and the geography
  table has no row for it (row 54b). Naming it is a survey -- walk the fly off that edge with real
  presses and read `wCurMap` back, the method of `docs/design/macros-wram.md` -- and nothing here
  guesses at it. Until then that edge is walked once and then falls out of the first tier.

## Row 54: the two arms, and the ROM runs (2026-09-22, v0.4.6)

Two checkpoints, because the loop was found twice: the rung-10 one the previous review left it in,
and the rung-11 one the stream fell into forty minutes after the badge was won. Same seed, same
ground, `main` at `cb9a88c` against this branch.

**The rung-10 checkpoint, twenty brain minutes.**

| measure | before (`main`, v0.4.6) | after |
| --- | ---: | ---: |
| rung reached | 10 | **11 (BOULDER BADGE at 10.78 brain minutes)** |
| distinct (map, tile) | **175** | 165 |
| windows flagged | **69 / 73** | 73 / 73 |
| macros started | 1,056 | 1,211 |
| `GO HEAL` starts | **204**, every one `done` at a net of 0.0 | **0** |
| `GO FRONTIER` starts | 242 | 34 |
| `GO ROUTE` starts | 211, mean net 0.2 | 7, mean net 6.1, max 30 |
| the repeated sequence | `GO FRONTIER, GO HEAL, GO ROUTE` | no walk cycle at all |
| frames in `battle` | 20,894 | **58,687** (longest run 30,809) |
| wall clock | 7,515 s | **3,086 s** |

**The cycle is gone and the fly wins the badge, and the hunt still flags every window.** Both are
true and both are reported. Eighty-two per cent of the after arm is inside battles and the longest
single battle is 30,809 frames, so the tile rule -- fewer than four distinct tiles in two brain
minutes -- flags a fly that is fighting exactly as hard as a fly that is stuck. That is section
15's own measurement in `docs/design/macros.md`, and the second branch running into it.
**The ethos check's "fewer flagged windows, more distinct tiles" does not hold on this arm**, and
the merge is Fable's call. The wall clock is the grid fix seen from outside: `main` re-decodes the
whole map on most frames because the cross-check refuses them, and this branch serves the cache.

**The rung-11 checkpoint, six brain minutes.** This is the arm the fix is about, on ground with no
gym leader in it.

| measure | before (`main`, v0.4.6) | after |
| --- | ---: | ---: |
| distinct (map, tile) | **14** | **88** |
| windows flagged | 17 / 17 | 17 / 17 |
| macros started | 378, every one `done` | 314 |
| `GO HEAL` starts | **129**, every one at a net of 0.0 | **0** |
| `GO FRONTIER` starts | 122, net 0.0 | 9 |
| `GO ROUTE` starts | 126, net 0.0 | **4, mean net 4.8, mean reach 19.5** |
| the repeated sequence | `GO ROUTE, GO FRONTIER, GO HEAL` x34 to x42 | `NEXT` and `BACK` in a battle's move list |
| the fly leaves Pewter City | **never** | **at 0.85 brain minutes**, and it ends the run on Route 3 |
| frames in `battle` | 0 | 15,619 (longest run 13,251, from minute 1.09) |

**Six times the ground, and the same seventeen flagged windows.** No walk completes at a net of
zero tiles any more, and from brain minute 1.09 the fly is inside a single 13,251-frame battle,
which the tile rule flags exactly as hard as the cycle it replaced. What makes that battle last is
row 50.

**ROM-gated, from both checkpoints** (`services/flysim/crates/flysim/tests/rom_macros_mode.rs`,
skipped cleanly without `FLY_ROM` and the checkpoint):

- `the_fly_reaches_the_pewter_gym_from_the_rung_ten_checkpoint` -- the gym's own interior on frame
  **3,163** on **25 macros**, `BACK` in a box **0**, unknown pads with no box **0**, `GO FRONTIER`
  on the museum's two floors **0**, and the new claim: **the longest chain of walks that completed
  at a net of zero tiles is 1**, against a bound of three.
- `the_fly_leaves_pewter_from_the_rung_eleven_checkpoint` -- route `[2, 56, 2, 14, 2, 14]` over
  55.8 brain minutes: out of the town, into the mart **once**, and on to Route 3. `GO HEAL` **0**
  starts, `GO SHOP` **1**, `GO ROUTE` **3**, and the longest chain of walks that completed at a net
  of zero tiles is **2** (`GO FRONTIER`, `GO NPC`) against the same bound of three.

### Gates

- `cargo test --workspace` with `FLY_ROM` set: green except
  `flysim::integration::the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed`,
  which is the known debug-build boot failure on this box -- the service took 49.2 s to its first
  healthy `/healthz` and `/status` still read the booting header (`milestone.total` 1, the
  placeholder `simloop.rs` uses before the adapter exists). Pre-existing and unrelated.
- `cargo clippy --all-targets`: clean.
- `infra/tests/lint.sh`: all checks passed, de-PII guard included.
- `--print-compatibility`: **648 bytes, sha256 `0d9bfde7...707fa`** -- byte-identical to v0.4.1
  through v0.4.5. Decoder, reward catalog, adapter version and roles untouched.
