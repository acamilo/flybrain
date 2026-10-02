# The behavioural scorecard

A repeatable, multi-seed "good play" regression harness. The unit tests say the macros do what
they were written to; the trap hunt says a fly stuck in one place got out. Neither says whether a
release plays *better or worse* than the one before it across the places the fly actually is. The
scorecard does: a fixed set of checkpoints, K seeds each, T brain minutes each, on the real
connectome with the macro layer, reduced to one JSON per release and compared release against
release by a test over the paired runs, never by a single run.

It measures and presses nothing. It restores a checkpoint, lets the brain play, and counts. The
ethos check (`docs/loop-review.md`) is untouched: the harness is outside the fly.

```sh
export FLY_ROM=/path/to/the-cartridge      # read, never copied
cargo build --release -p fly-scorecard     # from services/flysim

fly-scorecard list                                   # the set, resolved, each checkpoint's rung
fly-scorecard suite --label v0.7.5 --out card.json   # the set x 3 seeds x 10 brain minutes
fly-scorecard summary card.json                      # one screen of markdown
fly-scorecard compare old.json new.json              # exit 1 when a metric regressed
fly-scorecard run-one --checkpoint X --seed 1        # one run, JSON out (--out FILE)
```

`--runtime session` (the default) runs the session composition the release cut over to
(`fly-legacy-session`, in-process, unpaced); `--runtime legacy` runs the stream's own frame
(`flysim::frame::LegacyFrame`). Both hand the same per-frame observation to the same tally, so a
card from one runtime is comparable with a card from the other. From the same checkpoint and seed
the two give identical reports apart from wall time (`tests/rom_scorecard.rs` holds it), which makes a scorecard
run on both a second piece of shadow evidence. `--mode raw` runs without the macro layer.

## The set

`tools/scorecard/checkpoints.json` names each checkpoint by the environment variable that holds
its path (the ROM tests' own, `rom-env.sh`), never by a path; `--extra id=path` adds more (the
newest live checkpoint, a trap's). The checkpoints are not committed (`.local/` is untracked).
`list` prints the rung each one stands on, from its reward ledger. The set has two or three
states at each of rungs 8, 9, 10, 11, 12 and 15; rungs 13 and 14 have no checkpoint yet and join
the set when the scout makes them (a new entry is a line in the JSON). A checkpoint missing from
the environment is named on stderr and, with `--strict`, an error.

## Seeds

A checkpoint restores a deterministic brain, so two runs from it would be the same run. A seed
therefore chooses how many frames the brain's readout is ignored before the measurement starts
(60 to 1,800 frames, a splitmix of the seed; seed 0 is none). The brain still ticks and its noise
generator still advances, the game sees no button, and from the first measured frame the readout
is the brain's own: the run sits on a different point of its own trajectory. The idle frames are
not measured. The same seed twice is the same run.

## What a run reports

One `RunReport` per (checkpoint, seed). Every number is computed from per-frame observations by
`tally.rs`, which has no emulator in it and is unit-tested on synthetic frames.

| group | what |
| --- | --- |
| rungs | start, end, gained, the brain minute of each climb; reported per brain hour |
| places | the adapter's exploration count, start and end |
| empty pad | frames with the macro layer on, nothing running and no button dealt; seconds, share, longest stretch |
| watchdog | `fly-watchdog` check 10, rule for rule (`watchdog.rs`): probes, suspected, reasons, **confirmed** (two suspected probes in a row) and **ladder events** (one per run of two or more: a trap the recovery ladder would act on) |
| hunt | the trap hunt's windows (two brain minutes, fewer than four tiles or a sequence repeated more than ten times) |
| ratchet | rollbacks, split by game over and stall |
| macros | starts, done, blocked, timeout, refused, by macro |
| funnel | GO SHOP, BUY BALL, THROW BALL, catch: counts at each stage, balls gained and spent, species gained, and how far down the funnel the run got |
| heals | GO HEAL and the nurse's HEAL |
| whiteouts | the whole party fainted (a rising edge) |
| battles | started, ended, wild and trainer, won (a battle or trainer payout), lost (the party fainted in it), caught, other |
| rewards, scenes | counts and totals by adapter kind; frames by scene |

The watchdog's rules are the shell's: the same thresholds, the same order, the same two-probe
memory for the zero-progress, unrewarded and unwon-battle rules. Two things differ, both about
time: a run is short, so the probe cadence is a parameter (default every 120 brain seconds,
first at 240; the stream's is five wall minutes) and the window is the last ten brain minutes or
the whole run when it is shorter; and the first probe compares the exploration count with the
run's starting count, which the live check lacks after a boot.

## Compare

`compare A.json B.json` pairs runs by (checkpoint, seed). Each metric (`compare.rs`, `METRICS`)
has a direction (more is better, less is better, or info), an absolute tolerance and a relative
one. For each metric the pairs give one difference, signed so that positive means B is worse. B
**fails** a metric when it is worse by more than the tolerance **and** a one-sided sign-flip
permutation test over the pairs gives p below alpha (default 0.05). It reports **better** by the
same two tests, and **pass** otherwise: a change inside the tolerance is noise however consistent,
and one bad run among eighteen is not a regression. The test is exact up to 20 pairs and a
fixed-seed Monte-Carlo beyond, so the same two cards always give the same verdicts. The
comparison also names its caveats (different lengths, different runtimes, fewer than eight
pairs, unpaired runs). Exit status 1 is a failed metric.

The metrics: rungs per hour, places per hour, empty pad %, suspected probes %, ladder events per
hour, hunt windows flagged %, rollbacks per hour, blocked macro %, buy ball, throws and catches
per hour, battles won and lost per hour, whiteouts per hour. Heals per hour and macro starts per
minute are reported, never judged: healing more can follow losing more.

## Cost and size

A run's brain minute costs about 75 CPU seconds however it is split, and the result does not
depend on the thread count (the same seed gives an identical report at one thread and at four).
The split matters for wall time, because a run's sweep pool spins at its barriers: on a quiet
ten-core box two jobs of four threads are fine, but on a box shared with compiles and tests one
job of four threads ran five times slower per brain minute than one-thread jobs did. So the
default is one thread per run and eight runs at a time. The default suite, twelve checkpoints by
three seeds by ten brain minutes, is 360 brain minutes: about three quarters of an hour of ten
quiet cores, about twice that when the box is busy. More seeds tighten the test; longer runs let
rungs climb. Three seeds is the smallest suite whose pair count (36) can reach p < 0.001.
`--resume` keeps the runs an interrupted suite had finished (`<out>.runs.jsonl`).

## What it does not do

- It does not rebuild session ledgers a restore starts empty (the trap hunt's
  `FLY_TRAP_SEED_*`); a trap that needs them is that tool's.
- Ten brain minutes rarely climb two rungs. Rungs per hour is a rate over many runs, not a
  per-run prediction; a release that is faster by a rung every few hours shows only across the
  suite.
- It judges play against the previous release, not against a notion of good. A regression is
  "worse than last time"; the absolute numbers say how good last time was.

## Releases and nightly

`tools/scorecard/overlay.sh <ref> <tree>` puts the tool on the tree of an older release (the crate,
the set, the docs, and the one read-only accessor `PokeredTask::with_reader`), so a release that
predates it can be scored and its numbers are its own. The operator's nightly runner builds the
latest release tag on a build box, runs the suite, writes the card and a one-screen summary and
compares with the previous release's card.

`tools/scorecard/baseline-v0.7.5.json` is the reference card of v0.7.5 (adapter v8).
