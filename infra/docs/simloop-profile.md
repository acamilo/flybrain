# Where a game frame goes on the sim thread — method

> **The record lives in the operator's infra repo** (`services/flybrain/simloop-profile.md`), verbatim and dated.
> This file keeps only the method and what it found.

The question came from a release-box finding: flysim's main thread at 100% CPU with the
pool workers at about 70% and the realtime factor oscillating below 1, against a much
higher figure for the brain-only soak at the same thread count. Which of the sim thread's
**serial** phases is the difference in?

**Answer: two of them.** `plasticity.observe` is a large fixed slice of every frame and
does not move with the thread count, and the hot checkpoint's envelope encoding lands
entirely in whichever frame carries it — one frame in a few hundred blowing through the
16.74 ms budget, twice a stream-minute. Everything else on the sim thread is small: the
emulator frame, the retina, the decoder, the rewards and the whole snapshot publish
together are a fraction of one of those two.

**The premise that spike propagation is serial is wrong** — it has been sharded by target
range since `feat/flysim-perf`.

Method: instrument each serial phase of the sim loop with its own timer, run at several
thread counts, and compare per-phase means and the worst frame. A phase that does not move
with the thread count is serial work; a phase that only appears in some frames needs its
worst case reported, not its mean.
