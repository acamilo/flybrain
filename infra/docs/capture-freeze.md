# The capture freeze: one new picture a second, perfect audio, 0 dup / 0 drop

Measured 2026-09-16 on the release box **the release container** and reproduced six times on the dev box
**the dev container**. ffmpeg 7.1.5 (Debian trixie) on both. Fixed by an ordering gate, and watched by a
probe on the encoder output; the exact mechanism inside ffmpeg is **not** established and nothing
below claims one.

## 1. The symptom

`flycast` (ffmpeg: `x11grab` on `:99` at 30 fps plus the pulse `stream.monitor`, an
`fps=30:round=near` filter, `-fps_mode:v cfr`, tee'd to MediaMTX RTMP and to 10-minute mpegts
segments) produces a video stream in which **only about one frame per second is a new picture and
the other 29 are repeats**, while:

- the **audio is perfect** the whole time;
- ffmpeg's own progress counters report **0 dup, 0 drop, 30 fps** — check 3 of `fly-watchdog`
  (`frame=` advancing in `/run/fly/flycast.progress`) is satisfied;
- an independent `x11grab` of the same display shows **the display updating normally** — the page,
  Chromium and Xvfb are all fine;
- the freeze lasts **for the life of the ffmpeg process**. It does not recover.

On the release container it was present from the **06:35:52 UTC container boot** until a manual
`systemctl restart flycast` at **10:26 UTC** — **3 h 50 min of frozen broadcast**. Confirmed after
the fact by decoding the recorded segments: **174 to 177 identical consecutive frames out of 180**
in the first six seconds of every segment from 06:35 to 10:25, against **0 to 4 out of 180** in
the segments recorded between the 06:06 restart and the 06:35 boot, and again after the 10:26
restart.

In the frozen state, `strace` on the x11grab thread shows about **4 X round trips per second**
instead of 30, and **no time blocked** in them.

## 2. The trigger, from the dev box

Ten experiments on the dev container. The number is **identical consecutive frames out of 180** in the local
HLS output, measured 40 s after the start.

| Experiment | Identical / 180 | |
| --- | --- | --- |
| restart xvfb (so flystage and flycast co-start via `Requires=`) | **171** | frozen |
| restart flycast alone with Chromium already up and steady | 6 | fine |
| flystage stopped, restart flycast, start flystage 8 s later | 0 | fine |
| flystage stopped, restart xvfb+flycast together, start flystage 10 s later | 5 | fine |
| restart flystage and flycast at the same instant, old Xvfb | **171** | frozen |
| Chromium up, an extra silent audio client connects to the pulse sink 1 s after flycast start | 22 | mostly fine |
| flystage stopped (sink IDLE), restart flycast, a silent client connects 1 s later, flystage started 5 s after that | **167** | frozen |
| hammering the X server with `xsetroot` during flycast's first 4 s, no audio change | 4 | fine |
| a permanent silent client keeps the sink RUNNING, flystage stopped, restart flycast, start flystage 1 s later | **166** | frozen |
| same permanent silent client, restart flystage and flycast at the same instant | 12 | fine |

**Conclusion.** A **new PulseAudio client** (Chromium's audio stream at boot, or any client)
attaching to the null sink during roughly the **first one to three seconds** of ffmpeg's start —
its stream probing window — leaves the **video** path permanently starved. A client attaching at
the same instant, or 8 s later, does not. The X server is not involved: hammering it in the same
window changes nothing.

Two things this rules out, both worth stating because both were the obvious first guesses:

- **It is not the sink going idle.** A sink keep-alive does **not** fix it: the two experiments
  with a permanent silent client holding the sink RUNNING still froze when the page's client
  attached 1 s in.
- **It is not encoder CPU starvation.** It reproduces on the NVENC dev box, where the encoder is
  off-CPU, and it survives with sim/page/encoder on separate cores.

What is **not** established: what inside ffmpeg goes wrong. Do not turn "its stream probing
window" into a mechanism claim. `-analyzeduration` and `-probesize` are demuxer probing controls
(`-analyzeduration` "specify how many microseconds are analyzed to probe the input", default 0,
i.e. the format decides; `-probesize` a byte cap, default 5 MB) and ffmpeg's own documentation of
them says nothing about a device input's first seconds — so the ffmpeg command is deliberately
left **untouched** by this fix. Tuning them would be a guess dressed as a fix, on the one process
whose flags are the broadcast.

## 3. What changed

Nothing in the ffmpeg command line. The fix is ordering plus a watchdog.

### 3.1 `units/flycast.service`: order after the page, and wait for it

- `After=xvfb.service pulse.service mediamtx.service **flystage.service**`. Ordering only;
  **`flystage.service` is deliberately NOT in `Requires=`** — a dead or restarting page must not
  be able to take the encoder, the recording and the broadcast down with it, which is this repo's
  "the page is display only so the fly survives browser and encoder restarts" rule.
- `ExecStartPre=/opt/fly/bin/wait-for-stage 120`, after the existing `wait-for-x :99 30`.
- `TimeoutStartSec=180`. Both waits are inside the start job and 30 + 120 s exceeds systemd's
  default `TimeoutStartSec=90s`, which would have killed the unit mid-wait with `Restart=always`
  spinning it. The same latent bug was in `flystage.service` (30 + 120 behind its `/healthz`
  wait); fixed there too, though it was never observed — flysim answers well inside 90 s on
  the release container.

### 3.2 `bin/wait-for-stage`: what "ready" means

`wait-for-stage [TIMEOUT]` returns 0 only when **all three** of these hold, and have held
continuously for a **5 s settle** period:

1. **The X display answers** — `xdpyinfo` on `:99`, the same probe `wait-for-x` uses. Its root
   geometry is reused by check 2.
2. **A Chromium kiosk window is mapped on `:99`** — `xwininfo -root -tree`, taking the windows
   whose `WM_CLASS` is `("chromium" "Chromium")`, or, if the profile renamed itself (e.g. under
   `vglrun`), any child window whose geometry matches the root. A candidate counts only if
   `xwininfo -id <id> -stats` says `Map State: IsViewable` and it covers at least half the root
   area. There is no window manager on `:99`, so there is no `_NET_*` state to ask instead.
3. **The pulse sink `stream` has at least one sink-input** — `pactl list short sink-inputs` as the
   `fly` user with `PULSE_SERVER=unix:/run/fly/pulse/native` and `XDG_RUNTIME_DIR=/run/fly` (the
   env `pulse.service` sets and `flycast.service` already passes), matched against the sink index
   `pactl list short sinks` gives for the name `stream`. This is the condition that matters: it
   means **the page's audio client is already attached**, so ffmpeg's first seconds see a static
   client set rather than a connect.

If any check flips back to not-ready, the settle period starts over — a client connecting or
reconnecting resets the clock instead of letting ffmpeg start into a moving target. Settle timing
is whole-second, so the held period can be up to a second short of 5 s.

`xwininfo` is from `x11-utils` and `pactl` from `pulseaudio-utils`, **both already in
`02-base.sh`'s package list**; nothing new is installed for this, and `xdotool` (which is not in
that list) is not used.

**After `TIMEOUT` (120 s) it logs a WARNING and still exits 0**, so flycast starts anyway: a
black-but-running stream beats no stream, and a page that never comes up is `flystage`'s failure
to report. The only nonzero exits are usage errors. `wait-for-stage --once` evaluates the three
checks once, prints which one failed, and exits 1 — that is the hand-run diagnostic.

### 3.3 `bin/fly-watchdog` check 9: the self-healing probe

The ordering gate closes the window that was reproduced; it is not proof that no path into the
freeze remains, and the freeze is invisible to every other check (audio fine, `frame=` advancing,
page healthy). So the watchdog now measures the **encoder output**:

```sh
ffmpeg -sseof -4 -i <newest segment> -t 3 \
  -vf "tblend=all_mode=difference,blackframe=amount=99.5:threshold=8" -an -f null -
```

and counts the `Parsed_blackframe` report lines (each carries `] frame:`; the `last_keyframe`
field on those same lines is not what is counted). With `tblend`'s difference output, a frame that
reads as black **is** a frame identical to the one before it.

- **Every 5 minutes**, not every 60 s pass; the newest `.ts` under `/srv/fly/media/rec`, and only
  when `flycast.service` is active and that segment is being written (mtime under 120 s old) —
  otherwise a deliberately stopped encoder would look frozen.
- Run under `nice -n 10` and **`taskset -c` on flycast's own `AllowedCPUs`** (read from
  `/etc/systemd/system/flycast.service.d/cpuset.conf`, which `05-deploy.sh` generates from
  `CPUSET`/`RAYON_THREADS`/`ENCODER_CORES`); unpinned when no partition is configured. The probe
  must never land on the sim's cores. A `timeout 60` guard keeps a wedged probe from holding the
  pass open.
- **Two consecutive probes at 80 or more identical frames out of ~90** restarts `flycast` once,
  logs it at `err`, and **never restarts again for this reason inside 30 minutes**. One bad probe
  logs and waits: a legitimately still page could read high once.
- A probe that decodes fewer than 60 frames is logged as **inconclusive** and not compared. An
  absolute threshold under-counts on a short decode, and under-counting can only ever hide a
  freeze, never invent one — the safe direction for something that restarts a live broadcast.
- Metrics, in the watchdog's existing node_exporter textfile:
  `fly_capture_freeze_restarts_total` (counter) and `fly_capture_identical_frames` (gauge, the
  last probe's value, `-1` before the first probe).

The restart it issues goes through the ordering gate like any other flycast start, so the
self-heal cannot itself land in the freeze window: the page is already up, so `wait-for-stage`
should return after its 5 s settle, and the gap is under ten seconds.

Local calibration of the same command on fixture segments (`infra/tests/lint.sh` section 3e):
a static colour reads **81** identical frames, a mandelbrot reads **0**. The live numbers are the
same shape: 174-177/180 frozen against 0-4/180 healthy.

### 3.4 `05-deploy.sh`: stop the deploy hurting the sim

Unrelated to the freeze, owed from the same morning. Every in-container step runs through
`pct exec`, which lands on the container's **whole** cpuset, so the release extraction, the
MANIFEST `sha256sum -c` pass and `chown -R` competed with flysim: `fly_lag_seconds` went from 3 s
to 5 s during the live v0.1.1 deploy. Those three steps now run under `taskset -c <page cpus>`,
derived from the same `lib/common.sh` `cpuset_partition` call (the same
`CPUSET`/`RAYON_THREADS`/`ENCODER_CORES` values) that generates the cpuset drop-ins, so the two
can never disagree about which cpus belong to the sim. It is a **no-op** when no partition is
configured, or when the container's conf does not match `CPUSET` — the same guard the drop-ins
use. The page group is the target because it has the most slack; the encoder group has to stay
clear or the deploy shows up as dropped frames on the broadcast.

The script header now also says, in as many words: **never re-run `05-deploy.sh` on a live
release box as an idempotency check.** It is idempotent, and that is not the point — every run
does real work on the cores the sim is holding real time on. Verify a deploy by reading
`/opt/fly/current`, the MANIFEST and the units.

## 4. The measurement lesson

**The "identical frames" check must be run on the ENCODER OUTPUT** — the local HLS, or the newest
segment under `/srv/fly/media/rec`, which is the same bytes MediaMTX and Twitch get. **Never on an
independent `x11grab` of the display.** The display was fine the whole time; a second grabber of
`:99` reports a healthy, updating picture while the broadcast is frozen, so it reports PASS on
exactly the failure it is supposed to catch.

This is not hypothetical. The **3/57 measurement** recorded in `docs/stream-mvp-plan.md`
(2026-09-16 06:36 UTC, "unchanged captured frames 3/57 (was 73/115)", after the release container was widened to
ten node-1 cores) was taken with that wrong instrument, and it is why a frozen broadcast read as
fixed at 06:36 and then ran frozen until 10:26. A correction note is filed in
`infra/docs/p0-measurements.md` and against that plan entry.

The same number in its earlier form — "63% of captured frames came back unchanged across two
`fly-watchdog` passes", quoted in `infra/README.md` step 4, `infra/docs/runbook.md`'s "CPU
partition (cpuset)" and `lib/common.sh`'s `cpuset_partition` header as the finding that justifies
giving flycast its own cores — is the `73/115` half of that same pair, from that same instrument.
It has **not** been re-measured, so treat the three-way cpuset split as unproven-by-that-number
rather than wrong: the split is cheap, it is live on the release container and the dev container, and nothing here is a
reason to re-plumb it. It is a reason not to cite 63% as evidence for anything until someone
re-runs it against the encoder output.

## 5. What is not verified

- `wait-for-stage`'s readiness path and check 9's live behaviour **cannot be exercised without a
  container**: they need Xvfb, a Chromium kiosk, a pulse sink with a real client, and a running
  flycast. Everything testable on the operator box is in `infra/tests/lint.sh` section 3e (the
  unit's ordering/timeout directives, the argument handling, the timeout contract, and the whole
  of check 9 against ffmpeg-made fixture segments with a fake `systemctl`).
- The mechanism inside ffmpeg. The fix avoids the window; it does not explain it.
- Whether the ordering gate closes **every** path into the freeze. That is what check 9 and
  `fly_capture_identical_frames` are for. If the counter ever climbs on a container where
  `wait-for-stage` reported ready, this document's conclusion is incomplete and the next
  experiment is the pulse client set during ffmpeg's first three seconds
  (`pactl subscribe` alongside a flycast start is the cheap way to log it).
