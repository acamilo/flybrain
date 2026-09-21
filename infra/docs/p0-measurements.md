# P0 spike measurements — method

> **The record lives in the operator's infra repo** (`services/flybrain/p0-measurements.md`), verbatim and dated.
> This file keeps only the method: what to measure, how, and the go/no-go bar.

The ten measurements are specified in `docs/design/infra.md` section 4 ("Phase 0 spike
checklist") and are run **by hand** on the dev container against its own env file — P0 is
manual instrumentation, not a script. Each row carries its go/no-go threshold alongside it
in the design doc so nobody has to flip back mid-run.

Method, in the order the runs happen:

1. Brain-only sweep: `cargo run --release` against `data/fafb-v783`, realtime factor at
   1, 2, 3, 4 and 8 Rayon threads. **The knee is the answer, not the maximum** — past it,
   extra threads are memory-bandwidth saturation and real time gets worse.
2. Whole-stack run with a real cpuset: whole physical cores on one socket, SMT siblings
   excluded, partitioned between sim / page / encoder by `lib/common.sh`'s
   `cpuset_partition`. The result that matters is that flysim must own **whole** physical
   cores: a thread landing on an SMT sibling costs about a fifth of real time, which is
   more than socket locality is worth.
3. Chromium and ffmpeg CPU under `pidstat` over a fixed window, all processes summed.
4. Capture and encode at the real geometry and bitrate. Resolution-dependent rows must be
   labelled with the resolution they were shot at; the current decision is native
   1920x1080 at 6000 kbps CBR (`docs/streaming-plan.md` section 3's dated note), and an
   older 720p row does not describe what goes out.
5. A/V sync and segment continuity from the container's own MediaMTX, not from the encoder.
6. Checkpoint write cost and SSD write volume over an hour (`docs/design/infra.md`
   section 8).

Two instrument mistakes worth not repeating, both recorded in the moved file: a second
`x11grab` of the display is **not** a measurement of the encoder's output (the "frames
unchanged" pair taken that way has never been re-run and must not be cited as evidence),
and `fly_lag_seconds` only ever grows, so it needs a sample either side of a change, not
one after it.
