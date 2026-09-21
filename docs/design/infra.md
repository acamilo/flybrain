> Design document produced 2026-09-15 by a planning agent. Binding contracts are ../feed-protocol.md and ../control-api.md; where this document differs, the contracts win.

# Two 24/7 Twitch demo containers on the host: infrastructure and rollout

Research only. Nothing on the host was touched, no files written. Every address, CT ID and
free-space claim below is marked VERIFY where it rests on the infra repo's docs rather than on
a live `pct list`.

## 0. What the operator's existing infrastructure constrains

- The host = PVE 9.2.10, `<host-ip>`, 2x Xeon E5-2660 v3 (20C/40T, AVX2, no AVX-512),
  188 GB RAM. An SSD mirror backs `local-zfs` with something over 150 GB free, and a wide raidz2
  array (tens of TiB usable) is registered with pvesm for `images,rootdir`. The exact
  devices, sizes and free space are in the operator's infra repo; treat any figure here
  as indicative.
- ZFS ARC is pinned at 6 GB in `/etc/modprobe.d/zfs-arc.conf`. Do not raise it.
- The LAN convention is DHCP plus a router-side reservation keyed on MAC, not in-guest static.
  The two guests that ignore that convention (two unrelated guests) are both
  documented as hazards, and one of them caused a live address collision with `metrics`.
  Follow the convention.
- The reserved range is already densely populated — around a dozen holders, plus the
  hosts themselves, the management interfaces and the switch. Read the reservation table
  in the operator's infra repo before asking for an address, and ask for one by MAC.
- The other containers on the host have no direct SSH; the neighbours' own deploy scripts scp to the host and then `pct push`.
  Mirror that: the fly containers get no sshd, all ops go through `pct exec`.
- Provisioning on these hosts has always been shell (`deploy-prod.sh`, `deploy.sh`, a
  hand-installed backup script, a burn-in directory, a migration directory). There is no
  Ansible anywhere in the operator's repos, and the only `pct create` in
  those docs is one mention. The closest template is a neighbouring container running a
  comparable self-hosted web service:
  Debian 13 unprivileged, 4c/6G/1G swap, 40G on `bulk-array`, `onboot=1`,
  `features: nesting=1,keyctl=1`.
- There is no alerting path at all. `the infra repo's panels page` "Known gaps" says
  Alertmanager and Grafana are unbuilt and postfix on the metrics container is loopback-only. Plan for
  "the panel tile is the alarm", and record the gap.

## 1. Container spec

Two containers, identical except for an env file.

| | fly-pokemon | fly-platformer |
|---|---|---|
| CT ID | 150 (VERIFY `pct list`) | 151 (VERIFY) |
| hostname | `fly-pokemon` | `fly-platformer` |
| IP | `<release-ct-ip>` (VERIFY free: the router's lease table + `arping`) | `<platformer-ct-ip>` (VERIFY) |
| Twitch | channel 1 | channel 2 |

Hostname note: `fly-platformer` rather than `fly-mario`, because the ROM choice is still
open between Super Mario Land and Kirby's Dream Land (`docs/streaming-plan.md` section 6)
and the container name should survive that decision. `streaming-plan.md` and the proposed
`pass` entries already say `fly-platformer`.

```
pct create <release-ctid> local:vztmpl/debian-13-standard_13.6-1_amd64.tar.zst \
  --hostname fly-pokemon --ostype debian --unprivileged 1 \
  --cores 8 --memory 8192 --swap 2048 \
  --features nesting=1 \
  --rootfs local-zfs:24 \
  --mp0 local-zfs:16,mp=/srv/fly/state \
  --mp1 bulk-array:600,mp=/srv/fly/media \
  --net0 name=eth0,bridge=vmbr0,ip=dhcp \
  --onboot 1 --startup order=4,up=60 \
  --description "fly demo: flysim/flystage/flybridge/flycast (flybrain/infra)"
```

Why `nesting=1`: modern Chromium has no setuid sandbox, it uses the namespace sandbox,
which means the zygote calls `clone(CLONE_NEWUSER|CLONE_NEWPID|CLONE_NEWNS)`. An
unprivileged LXC runs under the PVE `generated` AppArmor profile, which blocks nested
user and mount namespace creation; without nesting the zygote dies with
"Failed to move to new namespace: Operation not permitted" and Chromium never paints.
`nesting=1` switches to the nesting AppArmor profile and permits it. One-line precheck
inside the CT as the `fly` user: `unshare --user --pid true` must exit 0. Fallback if it
cannot be enabled: `--no-sandbox`, acceptable in principle because the page is local
content from `127.0.0.1` with no navigation surface, but it removes the renderer's
defence in depth for a process that runs for weeks, so treat it as a temporary workaround
and not the design. `keyctl=1` is not needed (no Docker here); add only if something
complains.

Cores and memory: 8/8192 as decided. Two containers take 16 of 40 threads and 16 GB of
the 60 GB available, which leaves the box at well under half. Swap 2048 so Chromium's
cold pages page out instead of triggering the kernel OOM killer (VERIFY the host actually
has host swap; if not, drop swap to 0 and rely on the per-unit `MemoryMax` below).

> **2026-09-15, VERIFIED, and the answer is no: the host's swap is 0.** Read live from
> The host during the GPU probe (`docs/design/gpu.md` section 0). A container swap
> allocation has no backing store on a host with no swap, so `--swap 2048` bought exactly
> nothing — the paragraph above is wrong on its own terms and its parenthetical resolves
> to "drop swap to 0". the env files now set `SWAP_MB=0`, and the per-unit `MemoryMax`
> values plus `fly-watchdog` are the entire OOM story: a Chromium leak hits
> `MemoryMax=3G` on `flystage`, gets killed there, and `flysim` (`4G`, `Nice=-5`) keeps
> playing. Also affects section 8's risk list, which assumed the swap cushion existed.
> The `cpuset` pinning added by the GPU work (`docs/design/gpu.md` section 1) changes the
> "16 of 40 threads" arithmetic too: the two containers now take named whole physical
> cores on NUMA node 0 rather than whatever PVE picked.

Per-unit memory caps matter more than the container total: `MemoryMax=4G` on flysim,
`3G` on flystage, `512M` on flycast. A Chromium leak then kills flystage, the watchdog
restarts it, and flysim keeps playing. Without caps the kernel picks the biggest RSS,
which is usually Chromium but not always.

`/dev/shm` in an LXC defaults to a small tmpfs and Chromium's renderer will fall over on
it. Fix it properly with a tmpfs mount unit sizing `/dev/shm` at 1 GB rather than using
`--disable-dev-shm-usage`, which just moves the same traffic onto the rootfs.

### Storage sizing

Rolling recording at 720p30, 3000 kbps video plus 160 kbps AAC:

- 3160 kbit/s = 395 kB/s = 1.42 GB/h
- 34.1 GB/day, 239 GB for 7 days, about 244 GB with MPEG-TS overhead
- plus 90 days of daily recaps (5 min at ~6 Mbps, ~225 MB each) = ~20 GB
- plus a checkpoint archive on the array

`mp1 = bulk-array:600` mounted `/srv/fly/media` covers 7 days with 2x headroom. Set on
the dataset (host side):

```
zfs set quota=600G recordsize=1M <bulk-pool>/subvol-<ctid>-disk-1
```

The quota is the single most important safety measure in this plan. Guests on
the bulk array include the neighbouring GPU container, the metrics container, another container on the host and another guest on the host; a runaway recorder with no quota
takes the monitoring stack and another project's NFS share down with it. `recordsize=1M`
suits 10-minute sequential video files. Keep `compression=lz4` (its early abort makes
incompressible video nearly free) rather than turning compression off.

`mp0 = local-zfs:16` mounted `/srv/fly/state` holds checkpoints, the event log,
`status.json` and the segment index. It is on the SSD mirror because checkpoint commits
are fsync-latency-sensitive, and it is a separate dataset so it can be snapshotted and
backed up on its own and carries its own quota.

**2026-09-15 note:** the resolution/bitrate decision above changed
(`docs/streaming-plan.md` section 3's dated note) — the broadcast canvas is now native
1920x1080 and the Twitch target is 1080p30 at 6000 kbps CBR, not the 720p30/3000 kbps
this section's sizing assumed. Recomputed at 6160 kbit/s (6000 video + 160 audio): 770
kB/s, 2.77 GB/h, about 66.5 GB/day, **about 465 GB for 7 days** — roughly 1.9x the 244 GB
figure above. The 600G `mp1` quota still clears that with headroom, but not the same 2x
margin the original sizing intended; **recommend the bulk-array quota become 900G** to
restore comparable headroom (the 90-day recap and checkpoint-archive figures above are
unaffected — recaps and checkpoints don't scale with live-stream resolution). Left as a
note rather than rewriting the paragraph above, which records the original decision.

### A finding that changes the checkpoint design

`docs/server-sessions.md` in the prototype commits a checkpoint every 5 s with fsync plus
atomic rename. Estimate the envelope: 139,255 neurons of state (~3 MB), the plastic
KC to MBON weight and eligibility arrays (~1 to 4 MB), the binjgb save state (~0.2 MB),
so 5 to 15 MB per generation. At 10 MB every 5 s that is 173 GB/day of writes to
the SSD pool. A 512G consumer SATA mirror is rated around 200 TBW, which is under a year
of life, and that mirror also hosts another service's production workload.

Do not do that. Recommended instead:

1. Hot state ring in tmpfs at `/run/fly/state` (size-capped, 64 MB), written as often as
   the sim likes. This is what the watchdog reads for freshness.
2. Durable checkpoint to `/srv/fly/state` every 300 s, plus one on every milestone
   rank-up and one on clean shutdown.
3. Result: roughly 3 GB/day of SSD writes, and a crash costs at most 5 minutes of play,
   which is irrelevant for a 24/7 stream.

P0 must measure the real envelope size before this is finalised. If it turns out to be
under 1 MB the 5 s cadence is fine and this becomes a non-issue.

## 2. Provisioning: shell, under `~/flybrain/infra/`

Shell, not Ansible. Reasons: the operator has never used Ansible and the host has no control
node or inventory; `pct` is not idempotent in a way Ansible's lxc modules improve on, so
the useful idempotency is check-then-act either way; the whole thing is roughly 400 lines;
and adding a new dependency to the one host that runs another service's production workload is a poor trade.
The cost of shell is no drift detection, so the plan includes an explicit `verify.sh`
that asserts desired state and exits non-zero, which is the piece people usually skip.

```
~/flybrain/infra/
  README.md
  provision.sh                 # runs 01..07 for one env file, resumable
  verify.sh                    # asserts desired state, exit 1 on drift
  lib/common.sh                # log/die/on_pve_host/ct_exec/ct_push/converge_file/need
  01-create-ct.sh              # pct create + pct set, guarded on `pct config`
  02-base.sh                   # apt, /dev/shm, journald caps, fly user, dirs, tmpfiles
  03-node.sh                   # pinned node 22 tarball from nodejs.org + sha256
  04-mediamtx.sh               # pinned release tarball + sha256 + /etc/mediamtx.yml
  05-deploy.sh                 # flysim binary, flystage bundle, flybridge, units
  06-secrets.sh                # pass -> systemd-creds, root 0600
  07-enable.sh                 # enable/start in order, health gates
  <release-env>  <platformer-env>  <dev-env>
  units/*.service  units/*.timer  units/fly.target
  config/mediamtx.yml  config/pulse.pa  config/chromium-flags  config/journald.conf
  bin/fly-watchdog  bin/fly-recap  bin/fly-backup-stage  bin/fly-retention
  bin/flypush  bin/wait-for-health  bin/wait-for-x  bin/wait-for-stage
  docs/runbook.md
```

Idempotency rules: `set -euo pipefail` everywhere; `pct config $CTID >/dev/null 2>&1 ||
pct create ...`; `apt-get install -y` is naturally converging; config files are shipped
with `converge_file` which compares sha256, pushes only on difference, and signals
whether a `daemon-reload` plus restart is needed. Running `provision.sh` twice in a row
must produce no restarts the second time, and that is a test in `verify.sh`.

Packages: `xvfb x11-utils x11-xserver-utils xauth chromium pulseaudio pulseaudio-utils
ffmpeg fonts-dejavu-core fonts-liberation2 fonts-noto-core fonts-noto-color-emoji
fontconfig prometheus-node-exporter curl ca-certificates jq rsync sysstat procps zstd`.
No sshd. `apt-mark hold chromium ffmpeg` so an unattended security upgrade cannot swap
the two components most likely to break the stream; upgrade them deliberately on the
spike CT first.

PulseAudio, not pipewire-pulse. The null-sink plus monitor pattern is exactly what
PulseAudio does natively, it configures from a single `--file=` script, and it needs no
dbus session or wireplumber. pipewire in a container with no logind session is the more
fragile of the two, for no gain here.

Node 22 as a pinned tarball from nodejs.org into `/opt/node-v22.x`, verified by sha256,
symlinked into `/usr/local/bin`. Hermetic, no third-party apt repo, matches the
the operator's posture of pinning artifacts. Debian 13 ships Node 20, so the distro package
is not an option; NodeSource works but adds a repo whose trixie support is another thing
to monitor.

Rust never lands in prod. Build `flysim` on a dedicated throwaway build container,
The throwaway build container `fly-build` (Debian 13, 16 cores, `onboot=0`, destroyable), not on the WSL box.
Reason: glibc. Building against a newer glibc than Debian 13 trixie's and running on
trixie fails at load, and the build box being the same template on the same host removes
that class of problem entirely. Also, and this is the sharp edge:

```
RUSTFLAGS="-C target-cpu=haswell"     # correct: E5-2660 v3 is Haswell
RUSTFLAGS="-C target-cpu=native"      # WRONG off-host: Zen3 emits instructions
                                      # Haswell does not have, illegal instruction at run time
```

Reject `x86_64-unknown-linux-musl` static linking: musl's allocator is materially slower
under the allocation pattern of a per-millisecond neuron sweep, and real-time factor is
the whole ballgame. Ship a dynamically linked glibc binary built on the matching template.

Releases go to `/opt/fly/releases/<version>/` with `/opt/fly/current` as the symlink, so
a rollback is one `ln -sfn` plus a restart. sha256 of every artifact recorded in
`/opt/fly/releases/<version>/MANIFEST`.

`fly` user: `adduser --system --group --home /var/lib/fly --shell /usr/sbin/nologin fly`.
Directories via `/etc/tmpfiles.d/fly.conf`:

```
d /run/fly            0750 fly  fly  -
d /run/fly/pulse      0750 fly  fly  -
d /run/fly/state      0750 fly  fly  -
d /run/fly/wd         0750 fly  fly  -
d /var/lib/fly        0750 fly  fly  -
d /var/lib/fly/chrome 0700 fly  fly  -
d /srv/fly/state      0750 fly  fly  -
d /srv/fly/media/rec  0750 fly  fly  -
d /srv/fly/media/highlights 0750 fly fly -
d /etc/fly            0755 root root -
d /etc/fly/creds      0700 root root -
```

## 3. systemd units

One `fly.target` declares the whole set so the runbook has a single verb and ordering
lives in one place. All units `After=network-online.target`, all app units `User=fly`.

```
fly.target
  xvfb.service        Xvfb :99
  pulse.service       null sink "stream"
  mediamtx.service    local RTMP/HLS/WebRTC ingest
  flysim.service      Type=notify, WatchdogSec=30
   -> flystage.service   waits on flysim /health
       -> flycast.service   encode once, tee to mediamtx + segments
           -> flypush.service  copy-only remux to Twitch (disabled in local mode)
  flybridge.service   Wants, never Requires (chat must not be able to stop the sim)
```

**flycast's ordering is load-bearing, and `After=` only (2026-09-16).** The arrow from
flystage to flycast above used to be a diagram convention — flycast's `After=`/`Requires=`
named `xvfb` and `pulse` but not `flystage`, so an `xvfb` restart co-started the page and the
encoder. That race is the capture freeze: a new PulseAudio client attaching to the null sink
during roughly the first one to three seconds of flycast's ffmpeg leaves its x11grab leg
permanently starved (about one new picture a second, 29 repeats, audio perfect, 0 dup / 0 drop,
`frame=` advancing at 30 fps), and Chromium's audio stream at page load is exactly such a
client. Measured 3 h 50 min of frozen broadcast on the release container and reproduced six times on the dev container;
`infra/docs/capture-freeze.md` has the experiments.

So `flycast.service` now carries `After=xvfb.service pulse.service mediamtx.service
flystage.service` plus `ExecStartPre=/opt/fly/bin/wait-for-stage 120` (X answers, a Chromium
kiosk window mapped on `:99`, the `stream` sink has a sink-input, all three held for 5 s; after
120 s it warns and starts anyway) and `TimeoutStartSec=180`, since two ExecStartPre waits
totalling 150 s do not fit systemd's default 90 s start timeout. `flystage.service` stays out of
flycast's `Requires=` deliberately: ordering is what the fix needs, and a `Requires=` would let a
dead or restarting page take the encoder, the recording and the broadcast down with it — the
opposite of "the browser is display only; the sim is a service" in section 7's decisions.

### The one structural change to the plan in `piped-noodling-valley.md`

Split `flycast` into two units. `flycast` encodes once and tees only to local sinks
(MediaMTX and the segment recorder). `flypush` is a pure `-c copy` remux from
`rtmp://127.0.0.1:1935/live/fly` to Twitch. Reasons:

1. A Twitch outage or backpressure can no longer stall the encoder or the local
   recording. With a single unit, a blocked RTMP write blocks the muxer loop for every
   tee leg (`-use_fifo 1` mitigates it, does not eliminate it).
2. The 23 h restart then bounces a copy-only process, so there is no re-encode hiccup,
   the local recording is continuous across the seam, and the recap never sees a gap.
3. "Flip to Twitch" becomes "enable one unit", with nothing else restarting.
4. Cost is about 0.05 core.

The single-unit variant stays documented in the runbook as the simpler fallback.

### xvfb.service

```
ExecStart=/usr/bin/Xvfb :99 -screen 0 1280x720x24 -nolisten tcp -noreset -dpi 96 \
          +extension RANDR -extension GLX
Restart=always  RestartSec=2
```

Depth 24 so the framebuffer is plain RGB, which converts cleanly to `yuv420p`. Keep
MIT-SHM (on by default): `x11grab` uses XShm for fast frame reads and losing it costs
real CPU. `-extension GLX` is deliberate: decision 3 in the plan puts the brain map on a
2D canvas precisely to avoid SwiftShader WebGL, so nothing needs server-side GLX, and
removing it stops Chromium probing a software GLX stack it should not use. If WebGL ever
comes back, the answer is `--use-gl=angle --use-angle=swiftshader
--enable-unsafe-swiftshader` plus re-enabling GLX, and a fresh CPU measurement.
P0 tests both settings.

### pulse.service

```
User=fly
Environment=XDG_RUNTIME_DIR=/run/fly PULSE_RUNTIME_PATH=/run/fly/pulse
ExecStart=/usr/bin/pulseaudio -n --file=/etc/fly/pulse.pa --exit-idle-time=-1 \
          --disallow-exit --log-target=journal
Restart=always  RestartSec=2
```

No `--system` mode (upstream discourages it and it disables per-user modules) and no
logind or `enable-linger` dependency. A plain system unit running as `fly` with an
explicit `PULSE_RUNTIME_PATH` under `/run/fly` sidesteps the whole "container has no
session" problem: the socket is at a fixed path both Chromium and ffmpeg can be pointed
at. `/etc/fly/pulse.pa`:

```
load-module module-null-sink sink_name=stream rate=48000 channels=2 \
    sink_properties=device.description=stream
set-default-sink stream
load-module module-native-protocol-unix socket=/run/fly/pulse/native
```

`-n` means no default script, so `module-udev-detect`, `module-console-kit`,
`module-systemd-login` and bluetooth are never loaded, all of which fail noisily in a
container. Critically, `module-suspend-on-idle` is NOT loaded: if the null sink suspends
while the page is silent, ffmpeg's pulse input stalls and you get audio gaps or a hard
desync at the next sound. Verify with `pactl list short sinks` showing `stream` as
`RUNNING` while Chromium plays, and `IDLE` never becoming `SUSPENDED`.
48 kHz throughout, matching AAC output, so nothing resamples.

### flysim.service

```
Type=notify  NotifyAccess=main  WatchdogSec=30
Environment=FLY_GAME=${GAME} FLY_ROM=/srv/fly/rom/${ROM_SHA256}.gb
Environment=FLY_STATE_HOT=/run/fly/state FLY_STATE=/srv/fly/state
Environment=FLY_CONTROL_ADDR=127.0.0.1:7380 FLY_METRICS_ADDR=0.0.0.0:9101
Environment=RAYON_NUM_THREADS=6
Restart=always  RestartSec=2
StartLimitIntervalSec=300  StartLimitBurst=5
Nice=-5  CPUWeight=400  MemoryMax=4G
```

`sd_notify` is worth the small effort: it is one `sendmsg` to `$NOTIFY_SOCKET`, no crate
needed. The important detail is that `WATCHDOG=1` must be sent from inside the simulation
loop, not from a helper thread, because then a stalled sweep actually trips the watchdog.
`WatchdogSec=30` against a 30 Hz loop is generous enough to survive a checkpoint fsync.
If the Rust side is not ready at P1, fall back to `Type=exec` and let the external
watchdog cover it on checkpoint freshness; add `Type=notify` before P2.

Two listeners, one process, and the separation is deliberate: the control API
(`/stimulate`, `/reward`, `/pause`, `/checkpoint`, and explicitly no button endpoint)
binds `127.0.0.1` only, while a read-only `/metrics` plus `/status.json` binds the CT
address on 9101 so Prometheus on the metrics container can reach it. No mutating route is served on the
LAN listener. `RAYON_NUM_THREADS=6` leaves headroom for the encoder; P0 finds the knee.

### flystage.service

```
After=xvfb.service pulse.service flysim.service
Requires=xvfb.service pulse.service
Environment=DISPLAY=:99 PULSE_SERVER=unix:/run/fly/pulse/native
ExecStartPre=/opt/fly/bin/wait-for-x :99 30
ExecStartPre=/opt/fly/bin/wait-for-health http://127.0.0.1:7380/health 120
ExecStart=/usr/bin/chromium $(cat /etc/fly/chromium-flags | tr '\n' ' ') http://127.0.0.1:7380/stage
Restart=always  RestartSec=5  MemoryMax=3G
```

Flags, with the reason each one is there:

| Flag | Why |
|---|---|
| `--kiosk --window-position=0,0 --window-size=1280,720` | one window filling the root, no browser chrome in frame |
| `--user-data-dir=/var/lib/fly/chrome` | writable profile outside a nologin home, survives restarts |
| `--no-first-run --no-default-browser-check --disable-search-engine-choice-screen` | no first-run UI on stream |
| `--noerrdialogs --disable-session-crashed-bubble --disable-infobars --hide-scrollbars` | nothing modal can appear over the broadcast |
| `--autoplay-policy=no-user-gesture-required` | page-played audio has no click to wait for |
| `--disable-background-timer-throttling` | Chromium throttles background timers to ~1 Hz |
| `--disable-backgrounding-occluded-windows` | an occluded window must not be treated as background |
| `--disable-renderer-backgrounding` | keep renderer priority |
| `--disable-ipc-flooding-protection` | a 30 Hz feed plus canvas draws exceeds the default 10/s/frame cap |
| `--disable-gpu --disable-software-rasterizer` | 2D canvas only, Skia CPU raster, no SwiftShader cost |
| `--force-device-scale-factor=1 --force-color-profile=srgb` | deterministic pixels into x264 |
| `--disable-lcd-text` | subpixel antialiasing becomes colour fringing after 4:2:0 subsampling |
| `--password-store=basic --use-mock-keychain` | no gnome-keyring in the container |
| `--disable-features=Translate,MediaRouter,OptimizationHints,CalculateNativeWinOcclusion` | fewer background subsystems, and occlusion calculation is meaningless on Xvfb |
| `--remote-debugging-port=9222` (bind 127.0.0.1) | needed for the P0 measurements and the CDP fallback watchdog |

Rejected, with reasons, because the brief asked: `--headless` (see `streaming-plan.md`
section 2b, screencast has no rate guarantee and burns cores on JPEG round-trips);
`--disable-gpu-vsync` and `--disable-frame-rate-limit` (they uncap paint rate, which
burns cores for nothing when ffmpeg owns the output clock at 30 fps, and make CPU
unpredictable); `--enable-unsafe-swiftshader` (only if WebGL returns); `--mute-audio`
(the whole point is page-played audio); `--disable-dev-shm-usage` (fix `/dev/shm` instead).
Also set `/etc/fonts/local.conf` to grayscale antialiasing to match `--disable-lcd-text`.

### flycast.service (encode once, local sinks only)

The unit as shipped also carries `After=... flystage.service`,
`ExecStartPre=/opt/fly/bin/wait-for-stage 120` and `TimeoutStartSec=180` — the capture-freeze
ordering gate described above. The ffmpeg command below is unchanged by that fix (beyond the
1080p/6000k and `fps=30:round=near` / `-fps_mode:v cfr` notes already recorded elsewhere):
`-analyzeduration`/`-probesize` were considered and rejected, because ffmpeg's documentation of
those two demuxer-probing options does not support an argument about a device input's first
seconds, and the mechanism is not established (`infra/docs/capture-freeze.md` section 2).

```
ExecStartPre=/opt/fly/bin/wait-for-x :99 30
ExecStartPre=/opt/fly/bin/wait-for-stage 120
ExecStart=/usr/bin/ffmpeg -nostdin -loglevel warning -nostats \
  -thread_queue_size 1024 -f x11grab -draw_mouse 0 -framerate 30 -video_size 1280x720 -i :99.0+0,0 \
  -thread_queue_size 1024 -f pulse -name flycast -sample_rate 48000 -channels 2 -i stream.monitor \
  -filter_complex "[0:v]format=yuv420p[v];[1:a]aresample=async=1:min_hard_comp=0.100:first_pts=0[a]" \
  -map "[v]" -map "[a]" \
  -c:v libx264 -preset veryfast -profile:v high -level 4.1 \
  -b:v 3000k -minrate 3000k -maxrate 3000k -bufsize 6000k \
  -g 60 -keyint_min 60 -sc_threshold 0 -r 30 -bf 2 -x264-params "nal-hrd=cbr:filler=1" \
  -c:a aac -b:a 160k -ar 48000 -ac 2 \
  -progress /run/fly/flycast.progress -stats_period 5 \
  -f tee -use_fifo 1 \
  -fifo_options "queue_size=120:drop_pkts_on_overflow=1:attempt_recovery=1:recovery_wait_time=1" \
  "[f=flv:onfail=ignore]rtmp://127.0.0.1:1935/live/fly|[f=segment:segment_format=mpegts:segment_time=600:strftime=1:reset_timestamps=1:segment_list=/srv/fly/state/segments.csv:segment_list_type=csv:segment_list_flags=+live:segment_list_size=0]/srv/fly/media/rec/%Y%m%d-%H%M%S.ts"
ExecStartPost=/bin/sh -c 'date -u +%%s > /srv/fly/state/flycast-start'
Restart=always  RestartSec=5  CPUWeight=100  MemoryMax=512M
```

Decisions inside that command worth stating:

- No `-tune zerolatency`, despite the question in the brief. It disables lookahead and
  B-frames and therefore needs more bitrate for the same quality. We are not latency
  bound (a few seconds of glass-to-glass is fine for chat interaction), and 3000 kbps on
  a text-heavy 720p frame needs the efficiency. `-tune stillimage` is also rejected: it
  raises deblocking and smears 1 px UI edges. No tune, `-bf 2`, and `nal-hrd=cbr:filler=1`
  for true CBR, which Twitch prefers.
- MPEG-TS segments, not MP4. A `.ts` file killed mid-write is still playable and
  concat-friendly; a truncated MP4 has no moov atom and is garbage. This matters because
  every ffmpeg restart truncates the open segment.
- `-use_fifo 1` on the tee muxer is the specific fix for one slow leg blocking the others.
- `segment_list` as CSV gives the recap script an exact segment-to-time index instead of
  parsing filenames and hoping. `flycast-start` records the wallclock epoch of stream
  start so offsets are unambiguous.
- `aresample=async=1` is the long-run A/V drift fix. x11grab and pulse are two
  independent clocks; over hours the pulse leg drifts and the fix is to let the resampler
  stretch rather than to accumulate.

### flypush.service

```
After=mediamtx.service flycast.service
LoadCredentialEncrypted=twitch-key:/etc/fly/creds/twitch-key.cred
EnvironmentFile=/etc/fly/flypush.env
ExecStart=/opt/fly/bin/flypush
Restart=always  RestartSec=15
StartLimitIntervalSec=0
```

`/opt/fly/bin/flypush` reads `$CREDENTIALS_DIRECTORY/twitch-key`, builds
`rtmps://ingest.global-contribute.live-video.net/app/$KEY`, and execs:

```
exec ffmpeg -nostdin -loglevel warning -nostats -rw_timeout 5000000 \
  -i rtmp://127.0.0.1:1935/live/fly -c copy -f flv \
  -progress /run/fly/flypush.progress "$URL"
```

P0 must confirm `ffmpeg -protocols | grep rtmps` on the Debian 13 build (native TLS RTMP,
not librtmp). `StartLimitIntervalSec=0` because a multi-hour Twitch outage must not
permanently defeat the restart logic.

### Timers

| Unit | Schedule | Does |
|---|---|---|
| `flypush-restart.timer` | `OnUnitActiveSec=23h`, `RandomizedDelaySec=30m`, `AccuracySec=1m` | `systemctl restart flypush`, the 48 h guard |
| `fly-recap.timer` | `OnCalendar=*-*-* 04:20`, `Persistent=true`, `RandomizedDelaySec=10m` | cut yesterday's highlights |
| `fly-retention.timer` | `OnUnitActiveSec=1h` | prune segments > 7 d, highlights > 90 d, checkpoints per policy |
| `fly-watchdog.timer` | `OnBootSec=2min`, `OnUnitActiveSec=60s`, `AccuracySec=5s` | the health loop below |
| `fly-backup.timer` | on the host, not in the CT, 02:50 daily | stage and rsync to the backup host |

`RandomizedDelaySec=30m` on the restart timer keeps the two channels from reconnecting at
the same instant. Retention runs hourly rather than daily on purpose: a runaway recorder
must not get 24 hours of rope.

`fly-recap` reads `/srv/fly/state/events.jsonl` for yesterday (boundary computed in
`America/New_York` because that is the audience's yesterday, while every filename and
event timestamp is UTC, which is a real source of off-by-one bugs), selects milestone
rank-ups, badges, first-visit areas, deaths and sugar redemptions, maps each timestamp to
`(segment file, offset)` via `segments.csv` plus `flycast-start`, cuts each clip with
`-ss/-t -c copy` (2 s GOP means 2 s granularity, which is fine), concatenates with
`-f concat`, and re-encodes only the final few minutes. `Nice=19`,
`IOSchedulingClass=idle`, `CPUWeight=20` so it can never starve the live stream.

### fly-watchdog, 60 s

Checks in order, each with its own remediation, restarting only the failed unit:

1. flysim: `/health` returns 200 and hot-state mtime age < 30 s. Else restart flysim.
2. flystage: flysim's `frames_sent_total` and `feed_clients` from `/status.json`. Clients
   zero, or the counter flat across two passes, means the page is dead or frozen even
   though Chromium is alive. Restart flystage. This is the check that catches the failure
   mode a process-liveness check cannot see.
3. flycast: `frame=` in `/run/fly/flycast.progress` advancing. Flat for two passes,
   restart flycast.
4. mediamtx: `GET 127.0.0.1:9997/v3/paths/get/live/fly` shows `ready: true` and
   `bytesReceived` advancing. Else restart mediamtx, then flycast.
5. flypush: `frame=` in its own progress file advancing. Twitch-side liveness is checked
   centrally, not here, so two containers do not both poll Helix.
6. Disk guard: `/srv/fly/media` above 85% triggers an immediate prune; above 95% drops
   the recording leg while keeping the stream up, and raises the alarm metric.
7. Process age guard: flypush uptime above 24 h forces a restart even if the timer
   misfired. This is the local belt for the 48 h cap. Skipped when flypush is not
   enabled or not active. Restarts here count separately from the checks above, as
   `fly_watchdog_restarts_total{unit="flypush",reason="age"}`, so a dashboard can tell
   a scheduled age-guard bounce from a real progress-stall restart (check 5).

Escalation: per-unit consecutive-failure counters in `/run/fly/wd/<unit>.fails` (tmpfs,
so a reboot clears them). Three consecutive failures of the same unit inside 10 minutes
restarts the dependent chain; five triggers `systemctl reboot` from inside the container,
gated by `/var/lib/fly/wd/last-reboot` so it cannot reboot more than once an hour, with a
hard stop after three reboots in six hours. Past that it stops trying and only alarms: a
reboot loop on a host that also runs another service's production workload is worse than a dead demo.

Notification: there is no alerting path, so (a) journal at `err` with the stable
prefix `fly-watchdog:`, (b) a Prometheus textfile
`/var/lib/node_exporter/textfile/fly_watchdog.prom` carrying
`fly_watchdog_restarts_total{unit=...}` and `fly_watchdog_escalations_total`, which puts
it on the wall panels. Building a real alert path is listed as a known gap, not smuggled
into this project.

### Secrets: exact paths and modes

| What | Where | Mode |
|---|---|---|
| source of truth | `pass twitch/<channel>-key`, `twitch/<platformer-channel>-key`, `twitch/helix-client-id`, `twitch/helix-client-secret`, `twitch/fly-pokemon-bot-token` | WSL box only |
| stream key, in CT | `/etc/fly/creds/twitch-key.cred` | `root:root 0400`, dir `0700` |
| bridge app creds, in CT | `/etc/fly/creds/twitch-app.cred` | `root:root 0400` |
| bridge refresh tokens | `/var/lib/fly/bridge/tokens.json` | `fly:fly 0600` |
| non-secret env | `/etc/fly/flypush.env`, `/etc/fly/fly.env` | `root:root 0644` |
| Helix poller creds | `/etc/fly-twitch/helix.env` on the metrics container | `root:root 0600` |

`06-secrets.sh` reads from `pass` on the WSL box and pipes straight into
`pct exec <release-ctid> -- systemd-creds encrypt --name=twitch-key - /etc/fly/creds/twitch-key.cred`.
The secret never touches a file on the WSL box or on the host, and never appears in an
argv (it arrives on stdin).

Be honest about what `systemd-creds` buys in an unprivileged LXC: there is no TPM, so it
falls back to `/var/lib/systemd/credential.secret`, which lives in the same container.
Root in the container, or root on the host, can read it either way. The real value is that
the key is not in the repo, not in the journal, not in any unit file, not in
`/proc/*/environ` for any process except the one unit that declares
`LoadCredentialEncrypted=`, and is scoped to that unit's lifetime.

The residual exposure is `/proc/<pid>/cmdline`: ffmpeg takes the RTMP URL as an argument,
and there is no way around that. Containment is that the container has one non-root
service user and no sshd. `verify.sh` greps the journal and the whole repo for the key's
first 8 characters and fails if it finds them. The wrapper never runs `set -x`.
Rotation is in the runbook and takes under a minute.

Never rsync secrets off-box: `fly-backup-stage` explicitly excludes
`/etc/fly/creds`, `/var/lib/fly/bridge`, and `/srv/fly/rom`.

### Backups, mirroring the neighbouring service's nightly job

Same shape as `the backup script of another service on the host`: a root cron/timer on **the host** (not in the
container), so the backup host credentials stay on the hypervisor and never enter a guest, and
The host root's pubkey is already authorized for the backup host's backup account. 02:50 daily, staggered
after the neighbouring production service's 02:30.

`<host-stage>/fly-backup/backup.sh` per container:

1. `pct exec <release-ctid> -- cat /srv/fly/state/manifest.json` and note `latest`/`previous`.
2. `pct pull` those generation files, then `pct pull` the manifest **last**. A concurrent
   commit can only add a newer generation, so the snapshot is a valid earlier point in
   time and the server never has to stop. (This is the ordering
   `docs/streaming-plan.md` section 4 argues for.)
3. `pct pull` `events.jsonl` and yesterday's highlight.
4. md5 change detection, then dated copy to
   `<backup-user>@<backup-host>:<backup-path>/fly-pokemon/`.
5. Write `backup_*` textfile metrics (last success epoch, bytes) with `host` and `role`
   labels, matching the existing collector convention.

Do not back up the rolling recordings: 240 GB/week of material whose only durable value
is the highlights. Retain 14 daily and 8 weekly checkpoint sets on the backup host; keep
highlights indefinitely (they are small). Log to `<host-stage>/fly-backup/backup.log` and expect
"unchanged, skip" lines, exactly like the the neighbouring production service job.

## 4. Local test mode

the env file carries `PUSH_TARGET=local|twitch`. In `local`, `07-enable.sh` leaves
`flypush` disabled and everything else runs identically, so the only difference between
the test rig and production is one unit.

MediaMTX config, with the security split that matters on a flat /16 that has a public
NPM edge:

```
rtmpAddress: 127.0.0.1:1935     # publish and pull are loopback only
hls: yes                        # LAN preview lives here
hlsAddress: :8888
hlsVariant: mpegts              # VLC-friendly; fMP4 low-latency HLS is flaky in VLC
hlsAlwaysRemux: yes             # HLS is ready with no reader, so probes need no fake viewer
webrtc: yes
webrtcAddress: :8889
webrtcLocalUDPAddress: :8189
api: yes
apiAddress: 127.0.0.1:9997
metrics: yes
metricsAddress: 127.0.0.1:9998
paths:
  live/fly: { source: publisher }
```

From the LAN:

- VLC: `http://<release-ct-ip>:8888/live/fly/index.m3u8`
- browser: `http://<release-ct-ip>:8889/live/fly`
- RTMP stays loopback, so no `rtmp://<release-ct-ip>` URL exists by design

Explicitly: do not create an NPM proxy host for 8888 or 8889. If LAN-open HLS is not
acceptable, add MediaMTX `authInternalUsers` with a read-only password.

Flip to Twitch:

```
pass twitch/<channel>-key | ssh root@<host-ip> \
  'pct exec <release-ctid> -- systemd-creds encrypt --name=twitch-key - /etc/fly/creds/twitch-key.cred'
pct exec <release-ctid> -- systemctl enable --now flypush.service
```

Flip back is `systemctl disable --now flypush`. Neither direction restarts the sim, the
page, the encoder or the recording. That is the whole reason for the split.

### Phase 0 spike checklist (throwaway the dev container `fly-spike`, `onboot=0`)

Measure, in this order, and write every number into `infra/docs/p0-measurements.md`:

1. `unshare --user --pid true` as `fly` exits 0 (nesting works, no `--no-sandbox` needed).
2. flysim real-time factor on the host cores: 30 min runs at `RAYON_NUM_THREADS` of
   1, 2, 4, 6, 8, feed off, reading `fly_sim_realtime_factor`. Record the knee.
   Calibration: Node reached 0.92x on a 5800X3D, so Node on Haswell projects to 0.4-0.5x.
   Rust with rayon at 6 threads should clear 1.0x. Below 0.5x is a design problem, not a
   tuning problem.
3. Chromium CPU with the 2D canvas map: `pidstat -u 5 360` filtered to chromium,
   30 min. Target under 1.0 core total across all chromium processes.
4. ffmpeg CPU at veryfast 720p30, then faster, then ultrafast. Target under 1.5 core at
   veryfast. Judge quality at 3000 kbps by eyeballing 1 px UI borders and the 4x game
   panel, not by a number.
5. Total per-container load: `systemd-cgtop -1 --order=cpu` on the container slice while
   everything runs. Target 6 of 8 cores steady state.
6. A/V sync from a recorded file: record 60 minutes, then compare a reward tone against
   its on-screen flash frame by frame at minute 1 and minute 55. The drift is what
   matters, not the offset. Target under 100 ms per hour; the fix is
   `aresample=async=1`, which is already in the command.
7. Checkpoint envelope size and commit latency on `local-zfs` (this decides the cadence
   question in section 1).
8. 4 h unattended: RSS flat, `document.visibilityState === "visible"` throughout over
   CDP, rAF cadence stable, `/dev/shm` not exhausted, journal within its cap, Xvfb never
   restarted, pulse sink never `SUSPENDED`.
9. `ffmpeg -protocols | grep rtmps`, and `ffmpeg -f x11grab` using XShm.
10. VLC on the LAN plays the MediaMTX HLS output with production flags.

Go/no-go to P1: all of 1 through 10 pass, real-time factor at or above 0.8x, total load
at or below 6 cores, drift under 100 ms/h, RSS flat over 4 h.

## 5. Monitoring

Existing: the metrics container `metrics` at `<metrics-ct-ip>` runs Prometheus (`:9090`) and node_exporter
(`:9100`), with custom textfile collectors (`gpu_*`, `service_up`, `guest_*`, `disk_*`,
`backup_*`, `remote_pool_*`) that all carry `host` and `role` labels. `panel-bridge`
(stdlib Python on `127.0.0.1:9099`) polls Prometheus once and fans label-stripped frames
to the wall panels over SSE, with per-source `[age, budget]` freshness. There is no
alerting path.

Proposal, fitting that architecture rather than adding a parallel one:

1. `prometheus-node-exporter` in each fly CT with
   `--collector.textfile.directory=/var/lib/node_exporter/textfile`, listening on the CT
   address `:9100`. Two new scrape targets on the metrics container with
   `labels: {host: fly-pokemon, role: stream}`.
   VERIFY the exact `prometheus.yml` path and textfile directory on the metrics container; the infra repo's
   docs name the collectors but not the paths.
2. flysim's own `/metrics` on the read-only listener `:9101`. Series:
   `fly_sim_realtime_factor`, `fly_sim_steps_total`, `fly_emulator_frames_total`,
   `fly_frames_sent_total`, `fly_feed_clients`, `fly_ratchet_rank`, `fly_badges`,
   `fly_reward_total`, `fly_stuck_seconds`, `fly_checkpoint_age_seconds`,
   `fly_checkpoint_generation`, `fly_uptime_seconds`, `fly_sugar_redemptions_total`.
   Plus from the watchdog textfile: `fly_encoder_fps`,
   `fly_encoder_dropped_frames_total`, `fly_push_up`, `fly_media_use_ratio`,
   `fly_watchdog_restarts_total{unit}`.
3. A status JSON at `http://<ct-ip>:9101/status.json`, written every 5 s, shaped for the
   panels rather than for Prometheus:
   `{demo, live, rank, rank_name, badges, uptime_s, realtime_factor, last_event,
   checkpoint_age_s, encoder_fps, twitch_live, viewers, ts}`.
   `panel-bridge` polls it once and adds a `fly` section to the SSE frame, so the tablets
   keep issuing zero extra requests and inherit the existing freshness machinery. Give it
   its own freshness budget (10 s) in the per-source table. wall-panels then gets a
   "fly live / rank / uptime" tile, and the honesty rules already in that project apply:
   if the source is stale, grey it out rather than showing a confident stale rank.
4. Twitch Helix liveness from exactly one place: `fly-twitch-liveness.timer` on **the metrics container**,
   every 120 s. One app access token via client credentials, one request
   `GET /helix/streams?user_login=<ch1>&user_login=<ch2>`, writing
   `fly_twitch_live{channel=}`, `fly_twitch_viewers{channel=}`,
   `fly_twitch_started_at{channel=}` to the textfile collector. The metrics container already owns
   "is it up" semantics and already has the collector directory. The app limit is 800
   req/min, so 120 s is free. An empty `data` array means offline
   (`streaming-plan.md` marks that unverified; confirm by hand against a live and an
   offline channel in P2).
5. The two interesting derived conditions, both visible on the panel:
   `fly_push_up == 1 and fly_twitch_live == 0` for two consecutive polls means the local
   side thinks it is streaming and Twitch disagrees, which is the failure you cannot see
   from inside the container. And `time() - fly_twitch_started_at > 40h` means the 48 h
   cap is approaching despite the 23 h timer, which is the remote brace for the local
   process-age guard.

## 6. The second demo

What differs: the ROM, the reward adapter, the decoder preset, the channel, and the env
file. Nothing else.

```
# <platformer-env>
CTID=<platformer-ctid>  HOSTNAME=fly-platformer  IP=<platformer-ct-ip>
GAME=super-mario-land            # or kirbys-dream-land after the RAM-search spike
ROM_SHA256=<sha256>
REWARD_ADAPTER=fly-sml-rstdp-v1
DECODER_PRESET=platformer        # sustained holds, retuned cooldowns
TWITCH_CHANNEL=<ch2>
PASS_KEY=twitch/<platformer-channel>-key
PUSH_TARGET=local
```

One binary, not two. `flysim` compiles both adapters and selects by `FLY_GAME`, with the
game id folded into the compatibility string so a Pokemon checkpoint is rejected by a
platformer run and the reverse. Reasons: one artifact to build, sign and ship; one golden
test suite; and the shared-image claim is then literally true rather than aspirational.

The one real code difference to flag, because it is easy to under-scope: the motor
decoder is calibrated at startup and tuned to Pokemon's menu-driven pacing. A platformer
needs sustained button holds, so `DECODER_PRESET=platformer` is a reviewed change with
its own tests, not a config tweak. `streaming-plan.md` section 7 phase 2 step 4 already
says this.

ROM handling: staged to `/srv/fly/rom/<sha256>.gb`, mode `0400 fly:fly`, never in the
repo (`.gitignore` already blocks `*.gb`), never in the backup rsync, never on screen,
never linked.

## 7. Rollout, with go/no-go gates

**P0, spike (the dev container, throwaway).** The ten measurements in section 4. Nothing shared,
nothing live, no real checkpoint directory.
Go: all ten pass at the stated thresholds. No-go paths: real-time factor under 0.5x sends
us back to the sim design; Chromium over 2 cores sends the brain map to a smaller canvas;
nesting failing sends us to `--no-sandbox` with that risk written down.

**P1, fly-pokemon provisioned from the script, local only, 48 h.** Claim the release container in
The host's `$AGENT_CLAIM_LOG` first (the operator's rule, in `the host's own agent notes`). Run
`provision.sh <release-env>` from a clean checkout, then `verify.sh`. Stream to
MediaMTX for 48 h with `flypush` disabled. Restore drills, all of them, with the observed
recovery time recorded:

1. `kill -9` flysim. Expect: systemd restarts it, it restores from the latest checkpoint,
   the encoder and recording never drop, the page reconnects. Measure the visible gap.
2. `pct reboot <release-ctid>`. Expect: every unit returns in order, HLS is back within N seconds.
   Record N.
3. Corrupt the latest checkpoint (`dd` one byte into it). Expect: flysim detects the bad
   checksum and falls back to `previous`. **If the checkpoint envelope has no checksum,
   that is a code gap to close before P1 completes**, because this drill is the one that
   protects against a silent bad-state loop.
4. Fill `/srv/fly/media` to 96%. Expect: retention prunes, the stream survives, the
   metric fires.
5. `pct set <release-ctid> -net0 ...,link_down=1` for 10 minutes. Expect: flycast and the recording
   are untouched, flypush retries, recovery is automatic.
6. Restore from the backup host into the dev container and boot flysim on it. Assert rank and badges match.

Also in P1: nightly backup wired and proven once; retention proven over a full 7-day
cycle; the recap timer produces a watchable highlight three days running.
Go: 48 h with no manual intervention, all six drills pass, provision script run twice
with no second-run restarts, `verify.sh` green.

**P2, Twitch test channel, 7 days.** Enable flypush against a throwaway test channel.
Verify the 23 h restart leaves the sim and the recording running and produces one tidy
VOD per day; verify whether the VOD splits and what the reconnect grace window actually
is (both marked unverified in `streaming-plan.md`); verify the Helix poller flips offline
when flypush stops and back when it returns; verify the discrepancy condition in section
5 item 5 by deliberately breaking it. Grep the journal and the repo for the key prefix.
Go: 7 unattended days, at least 6 clean daily restarts, offline alert fires correctly in
a deliberate test, no key found anywhere it should not be.

**P3, public channel plus flybridge.** Rotate to the real channel's key. Enable
flybridge for read-only chat first, then the rate-capped sugar path, with the cap enforced
on the flysim side as the real limit and on the bridge side only for UX. Moderation policy
in writing before any chat text is rendered into the frame.
Go: overlay plus bridge adds under 0.3 core, cap enforcement proven by attacking it,
moderation policy exists.

**P4, fly-platformer.** RAM-search spike, adapter and catalog, decoder preset,
compatibility string bump with a cross-rejection test, then
`provision.sh <platformer-env>`. Both containers live.
Go: the host load average stays under 0.7x its thread count with both live, no farming loop
reachable in a 1 h soak, and both pools trending flat on usage.

**Every phase, on completion.** Append to the host's `$AGENT_CLAIM_LOG`, and update
The operator's infra repo (`master`, `git pull --rebase` first, never force-push, markdown only, no `.sh`
artifacts): new `the infra repo's flybrain page`; `the host's notes` guest table gains the release container/151;
`main.md` static reservations table gains the two reserved addresses; `the infra repo's panels page` gains
the fly tile and the new panel-bridge source. The scripts themselves live in
`~/flybrain/infra/`, pushed to the operator's own git remote
(API-create the repo first, push-to-create is disabled).

## 8. Risks

- **Chromium in an unprivileged LXC.** Namespace sandbox needs nesting; without it the
  zygote aborts. Gated by the `unshare` precheck in P0. `--no-sandbox` works but is a
  real reduction in defence for a process running for weeks, so it is a fallback with a
  written justification, not a default.
- **PulseAudio with no session.** No logind, no dbus session, no udev. Mitigated by
  `-n --file=` with only two modules loaded and a fixed socket path. The specific trap is
  `module-suspend-on-idle`: if it loads, the null sink suspends during silence and
  ffmpeg's pulse input stalls. Verify the sink never reaches `SUSPENDED` over a 4 h run.
- **x11grab tearing.** Xvfb has no vblank, so a capture can read a frame mid-paint and
  show a horizontal seam. The content is largely static panels with a nearest-neighbour
  game view, so the exposure is low, and XShm capture plus Chromium's double-buffered
  compositor keeps it rare. Accept it, confirm by eye on a recorded file in P0, and do
  not chase it with `--disable-gpu-vsync`, which makes it worse and costs cores.
- **Disk growth.** 240 GB per container per week if retention fails, on a pool that also
  carries the neighbouring GPU container, the metrics container, another container on the host and another guest on the host. Three independent brakes: the ZFS `quota=600G`
  (which is the one that actually protects the neighbours), the hourly retention timer,
  and the watchdog's 85%/95% guard. The quota is not optional.
- **SSD write endurance from the checkpoint cadence.** Section 1 covers the arithmetic.
  A 5 s full-checkpoint cadence would put roughly 173 GB/day onto the mirror that hosts
  another service's production workload. Fix with the tmpfs hot ring plus a 300 s durable cadence, and confirm
  the envelope size in P0 before committing to numbers.
- **48 h cap edge cases.** The 23 h timer resets on every crash-restart, and each restart
  ends the broadcast, so the cap itself is safe from that direction. The genuine edge case
  is the opposite: Twitch counts per broadcast, not per process, so a silent RTMP
  re-establish inside one ffmpeg lifetime can leave a broadcast older than the process
  thinks. Hence both guards: local process age over 24 h forces a restart, and remote
  `started_at` over 40 h alarms and forces one.
- **Secrets.** The unavoidable residue is the RTMP URL in `/proc/<pid>/cmdline`.
  Containment: no sshd, one service user, `verify.sh` greps the journal and repo for the
  key prefix, secrets excluded from the backup, rotation documented and under a minute.
  `systemd-creds` in an LXC has no TPM, so be honest in the docs about what it does and
  does not protect against.
- **Haswell single-thread performance**, the pre-existing risk. P0 measurement 2 is the
  gate; there is no software fix downstream of it.
- **Host time sync.** A container cannot set its own clock, so every recap boundary and
  event timestamp depends on the host being NTP-synced. VERIFY chrony or timesyncd on the host,
  and have `verify.sh` assert clock skew under 1 s.
- **Unattended upgrades swapping Chromium or ffmpeg under a live stream.** `apt-mark hold`
  both, canary them on the spike CT, apply deliberately.
- **No alerting path at all.** Everything above degrades to a panel tile and a
  journal line. Record it as a known gap rather than pretending the watchdog notifies
  anyone.

## Critical files to create

- `infra/provision.sh` and `infra/lib/common.sh`
- `the release env file` (plus `<platformer-env>`, `<dev-env>`)
- `infra/units/` (`xvfb`, `pulse`, `mediamtx`, `flysim`, `flystage`,
  `flycast`, `flypush`, `flybridge`, `fly.target`, five timers)
- `infra/bin/fly-watchdog` and `infra/bin/fly-recap`
- `infra/docs/runbook.md` (start/stop, local-to-Twitch flip, rotate
  stream key, rotate bot token, restore checkpoint locally and from the backup host, roll back a
  release via the `current` symlink, disk full, panel tile meanings, the host reboot order,
  and "do not touch the neighbouring production container")
- `the operator's infra repo/the infra repo's flybrain page` (plus the three existing-doc edits in §7)
```

### Critical Files for Implementation
- infra/provision.sh (with `lib/common.sh`, `verify.sh`, the env file)
- infra/units/flycast.service (and `flypush.service`, `flysim.service`, `flystage.service`, `fly.target`)
- infra/bin/fly-watchdog (and `fly-recap`, `fly-retention`, `flypush`)
- infra/docs/runbook.md
- The operator's infra repo/the infra repo's flybrain page (plus `the host's notes`, `main.md`, `the infra repo's panels page` edits)