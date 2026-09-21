# flybrain/infra

Provisioning, systemd units, and a runbook for the two 24/7 Twitch demo LXC containers
(`fly-pokemon` the release container, `fly-platformer` the platformer container) plus a throwaway measurement CT
(`fly-spike` the dev container), all on **the host**. Implements `docs/design/infra.md`; where
`docs/feed-protocol.md` or `docs/control-api.md` disagree with that design doc on a
port number or endpoint shape, this code follows the contract docs (feed
`ws://127.0.0.1:7400/feed`, control `http://127.0.0.1:7401`) — see each unit file's own
header comment for exactly where.

The GPU path (NVIDIA passthrough, the in-container driver, NVENC encoding, CPU pinning)
implements `docs/design/gpu.md` and is switched entirely by `GPU=1` in the env file. The
recommendation there is narrow on purpose: **the GPU is for the encoder, not for
Chromium.** Read it before touching anything under `host/`, the `lxc.*` blocks in
`01-create-ct.sh`, or `bin/flycast-launch`.

Read `docs/design/infra.md` in full before changing anything here, then
`infra/docs/runbook.md` for operations.

## Layout

```
provision.sh          runs 01..07 for one env file, resumable
verify.sh             asserts desired state, exit 1 on drift
lib/common.sh          log/die/need/converge_file/converge_conf_block/pct wrappers
host/*                 the two HOST-side files (fly-nvidia-majors.{sh,service}),
                         installed by hand on the host — see host/README.md
01-create-ct.sh .. 07-enable.sh
env/example.env         the template; real env files live outside this repo
units/*.service .timer .target
config/*                mediamtx.yml, pulse.pa, chromium-flags, chromium-flags.gpu,
                        chat-deny.txt (installed once to /srv/fly, then operator-owned)
                         (gpu: spike-only, insufficient on its own), chromium-flags.vgl
                         (VirtualGL EGL, measured passing, dev containers only —
                         infra/docs/virtualgl-spike.md), journald.conf, fonts-local.conf,
                         fly-tmpfiles.conf, dev-shm-override.conf, fly-sudoers,
                         serve.mjs + serve.sh (flystage-web's static server)
bin/*                   fly-watchdog, fly-recap, fly-retention, fly-backup-stage,
                         flypush, flystage-launch, flycast-launch, wait-for-x,
                         wait-for-stage,
                         wait-for-health
build/*                 build-flysim.sh (runs in the fly-build CT), package-release.sh
docs/runbook.md
docs/*.md               method + pointers; the records live in the infra repo
tests/lint.sh            bash -n / shellcheck + systemd-analyze verify + ExecStart* sanity
```

Every script under `infra/` takes the **path to an env file** as its first argument, is
`set -euo pipefail`, and is safe to re-run (`converge_file` in `lib/common.sh` compares
sha256 before pushing anything, and every `pct create`/`adduser`/`mkdir -p` call is
naturally idempotent or explicitly guarded).

**The real env files are not in this repo.** `infra/env/example.env` is the template;
the deployable ones live in the operator's infra repo and are passed by path from
outside the checkout (`infra/env/README.md`). Throughout this document
`<release-env>`, `<dev-env>` and `<platformer-env>` stand for those paths — e.g.
`/etc/fly/env/fly-pokemon.env`, or a bare name with `FLY_ENV_DIR` set. Other
angle-bracket values (`<ctid>`, `<host-stage>`, `<twitch-channel>`, …) are placeholders
for the same reason: this repo is public and must not name the operator's network.

**Nothing in this repo runs against the host by itself.** These scripts are meant to be
run by a human, on the host, deliberately, one step at a time if needed
(`--from-step N`). They never run from CI, from this worktree's own test harness, or
unattended.

## Quick start (once P0 has gone GO)

```sh
# On the host, as root:
infra/provision.sh <release-env>
infra/verify.sh <release-env>
```

Add `--release path/to/flybrain-<version>.tar.gz` to `provision.sh` once a release
exists (`infra/build/build-flysim.sh` + `infra/build/package-release.sh`); without it,
`provision.sh` still converges every unit, config file, and bin/ helper, which is the
expected state until `services/flysim`, `apps/stage`, and `services/bridge` are built.

### Provisioning a release container before the first release

`ROLE=release` (the release container) makes `05-deploy.sh` refuse any tree that is not exactly at a
clean annotated tag — which is also true of the units-and-config-only pass above, so a
brand-new release container could not be provisioned at all until a tag existed.
`PRERELEASE_UNITS=1` is the one documented way through, and it is narrow: no release
tarball (so nothing is installed under `/opt/fly/releases` and `/opt/fly/current` is
never flipped), the variable typed by a human, and a container that has never had a
release deployed to it. From the first tagged deploy onwards it refuses forever, so it
can never be used to slip an untagged unit file onto a live stream.

```sh
# on the host, provisioning the release container for the first time (no release exists yet):
PRERELEASE_UNITS=1 infra/provision.sh <release-env>
```

Do **not** run `07-enable.sh` (or `provision.sh` past step 6) on a release container with
no release: `fly.target` would start `flysim` against a `/opt/fly/current` that does not
exist. Run steps 1-6 and leave the target disabled until the first tagged deploy — that
is what `verify.sh` now expects to find on a pre-release container (it checks that the
unit *files* are converged and skips the active/enabled assertions).

## The dev/release split

`ROLE` in each env file (default `dev` when unset — see `infra/lib/common.sh`'s
`load_env`) picks which of two shapes a container gets (docs/stream-mvp-plan.md,
"Release container", the operator 2026-09-16):

- **`ROLE=dev`** — `<dev-env>` (the dev container `fly-spike`). Builds, measurements, deploy
  trials, the VirtualGL experiment happen here. GPU passthrough and NVENC stay on this
  box. `infra/05-deploy.sh` deploys any commit, clean or not, and names the release
  directory after a short sha + timestamp — the historical behaviour, unchanged.
- **`ROLE=release`** — `<release-env>` (the release container `fly-pokemon`), the actual stream.
  `GPU=0`, `FLY_ENCODER=x264` for the first release (no NVIDIA passthrough on this
  container at all; NVENC stays dev-only until the operator says otherwise). `05-deploy.sh`
  refuses to deploy unless the source tree it is itself run from is exactly at a clean
  annotated `vX.Y.Z` tag, names the release directory after that tag, and prints the
  tag in its final claim-log-style deploy line. Nothing experimental runs here; never
  develop or measure on this box.

See "Cutting a release" below for the tag-to-deploy flow, and
`infra/docs/runbook.md`'s section of the same name for the full command sequence
including rollback.

## Cutting a release

1. On a clean `main`: `infra/build/tag-release.sh vX.Y.Z`. It runs `npm test`, `npm run
   typecheck`, `cargo test --workspace` (in `services/flysim`), and
   `infra/tests/lint.sh` itself, and refuses to create the tag if any of them fail, the
   tree is dirty, or the current branch is not `main`. It does not push the tag.
2. Build and package from that tagged commit: `infra/build/build-flysim.sh`, then
   `infra/build/package-release.sh vX.Y.Z <flysim-bin> <stage-dir> <bridge-dir>
   <out-dir>`. The MANIFEST inside the tarball records `git_tag=vX.Y.Z` and
   `git_commit=<sha>` as `#`-prefixed lines ahead of the sha256 checksums.
3. Deploy, from a checkout that is ITSELF at that same clean tag: `infra/05-deploy.sh
   <release-env> /path/to/flybrain-vX.Y.Z.tar.gz`. `<release-env>`'s
   `ROLE=release` makes `05-deploy.sh` check the checkout it is running from (not the
   tarball) — `git describe --exact-match --tags` and `git status --porcelain` — and
   refuse the entire deploy if either fails.
4. `infra/verify.sh <release-env>`.
5. Rollback to the previous release: `infra/docs/runbook.md`'s "Roll back a release" —
   one `ln -sfn` of `/opt/fly/current` to the previous tag's
   `/opt/fly/releases/<tag>/` directory, plus a restart. Releases on this container are
   named after tags, so "the previous release" and "the previous tag" are the same
   directory.

Full command sequence, including the exact refusal message and what a passing run
prints, is in `infra/docs/runbook.md`'s own "Cutting a release" section.

## Host-side steps the operator runs by hand

These are **not** run by any script in this repo, on purpose — either because they are
one-time/rare, because they are safety-critical enough to want a human looking at the
number, or because they touch host state (`$AGENT_CLAIM_LOG`, the ZFS pool) that no
per-container script should be reaching outside its own container's boundary.

### 1. Claim the container ID

Before running `provision.sh` for the first time against a real CT ID, append a claim to
The host's agent claim log — the file `AGENT_CLAIM_LOG` names (the operator's rule; the
path is in the operator's infra repo, not here):

```
2026-0X-XX: claiming the release container (fly-pokemon) and the platformer container (fly-platformer) for flybrain/infra.
```

### 2. `pct create` (reference — `01-create-ct.sh` runs this for you)

`01-create-ct.sh` runs this exact command (guarded on `pct config` so a second run is a
no-op); it is reproduced here so the manual command and the scripted one never drift
apart silently. The one difference is the template argument: the script resolves the
newest `debian-13-standard_*_amd64.tar.zst` actually present in `pveam list local`
(override with `TEMPLATE=`) rather than hardcoding a patch level, because the patch level
moves. `13.6-1` below is what the host had on 2026-09-15.

```sh
pct create <release-ctid> local:vztmpl/debian-13-standard_13.6-1_amd64.tar.zst \
  --hostname fly-pokemon --ostype debian --unprivileged 1 \
  --cores 8 --memory 8192 --swap 0 \
  --features nesting=1 \
  --rootfs local-zfs:24 \
  --mp0 local-zfs:16,mp=/srv/fly/state \
  --mp1 bulk-array:600,mp=/srv/fly/media \
  --net0 name=eth0,bridge=vmbr0,ip=dhcp \
  --onboot 1 --startup order=4,up=60 \
  --description "fly demo: flysim/flystage/flybridge/flycast (flybrain/infra)"
```

`--swap 0`, not the design doc's 2048: the host's swap is **0** (read live on
2026-09-15, `docs/design/gpu.md` section 0), so a container swap allocation has no
backing store and buys nothing. Per-unit `MemoryMax` is the whole OOM story.

`01-create-ct.sh` additionally converges two `lxc.*` sentinel blocks into
`/etc/pve/lxc/<ctid>.conf` — the NVIDIA passthrough block when `GPU=1`, and the
`cpuset.cpus`/`cpuset.mems` pinning whenever `CPUSET` is set at all, which includes
CPU-only containers like the release box — and restarts the container if either changed.
Those are raw `lxc.*` keys, which `pct set` does not accept, so they are not part of the
`pct create` line above. See step 5 below and `docs/design/gpu.md` section 1.

Substitute `151`/`fly-platformer` for the second container. **VERIFY** the two reserved addresses
are free (the router's lease table + `arping`) before creating — see `docs/design/infra.md`
section 0's reserved holder table, and request the the router's DHCP reservation by MAC
after the container's first boot (the LAN convention is DHCP + a the router reservation, not
in-guest static — see that same section for why those two guests are documented hazards for
having done it the other way).

### 3. ZFS quota

`--mp1 bulk-array:600,mp=/srv/fly/media` in the `pct create` above already sizes that
subvolume, which Proxmox's ZFS storage plugin implements on the underlying dataset at
allocation time — as **`refquota`**, not `quota`, and the dataset is numbered **per
storage pool**, not per mountpoint index. Measured on the dev container (P0 spike run 2): the
`bulk-array` volume is `<bulk-pool>/subvol-<ctid>-disk-0` (it is the first volume this
guest has on *that* pool, even though it is `mp1`), while `local-zfs` holds
`rpool/data/subvol-<ctid>-disk-0` (rootfs) and `-disk-1` (`mp0`, `/srv/fly/state`).
`zfs get quota` on any of them returns `none`, which reads as "no limit" and is wrong.

So confirm it like this — find the dataset first, ask for `refquota`, and only then
decide whether anything needs setting:

```sh
# which dataset is /srv/fly/media, really:
pct config <release-ctid> | grep '^mp1:'                     # -> bulk-array:subvol-<ctid>-disk-N,mp=/srv/fly/media
zfs list -r -o name,refquota,quota,used,recordsize <bulk-pool> | grep subvol-<ctid>

# the limit PVE actually set (expect 600G here; `quota` will say none):
zfs get -H -o value refquota <bulk-pool>/subvol-<ctid>-disk-0

# only if refquota is missing or wrong, and with the exact dataset name from above:
zfs set refquota=600G recordsize=1M <bulk-pool>/subvol-<ctid>-disk-0
```

`refquota` caps what the guest itself can write, which is the runaway-recorder case this
guard exists for. It does **not** cover snapshots of that dataset, so if a snapshot
schedule is ever pointed at these subvolumes, add a `quota` on top of the `refquota`.

This is deliberately **not** run by any script — `docs/design/infra.md` section 1 calls
it "the single most important safety measure in this plan" (guests on the bulk array
include the neighbouring GPU container, the metrics container, another container on the host and another guest on the host; a runaway recorder with no quota takes the
monitoring stack and another project's NFS share down with it), and safety-critical
host-level ZFS changes get a human's eyes on the exact number, every time. `verify.sh`
checks the *result* of this (via `df` from inside the container, since it never runs
`zfs` on the host itself) but does not set it.

### 4. Pin the cpuset, and partition it between the units

`--cores 8` does **not** give the container eight cores. PVE turns it into an automatic
cpuset of eight host *threads*, and on the host (2 × E5-2660 v3, SMT on, two NUMA nodes) the
set it picks is neither whole-cored nor single-socket: the dev container got `3,6,13,15,20,23,26,36`
on the P0 spike's second run — six physical cores, two of which contributed both SMT
siblings, split three and three across the two sockets. The first run got a different
eight-thread set with seven physical cores. This is measured, not theoretical: with the
automatic set and the old `RAYON_NUM_THREADS=6`, flysim burned 4.1 of 8 cores, could not
hold real time (`fly_lag_seconds` at 31 s eight minutes in) and starved x11grab into
~4 duplicated and ~4 dropped frames a second.

**Both halves are scripted now** (2026-09-16), and neither half depends on `GPU`: the
pinning is the sim's business, not the encoder's, and the measurement that justifies it
was taken on x264. `01-create-ct.sh` writes the host-side
`lxc.cgroup2.cpuset.cpus`/`.mems` from `CPUSET`/`CPUMEMS` in the env file, and `05-deploy.sh`
generates the in-guest `AllowedCPUs=` drop-ins from that same `CPUSET`, via
`lib/common.sh`'s `cpuset_partition` — flysim gets the first `RAYON_THREADS` cpus, flycast
gets the last `ENCODER_CORES` (default 2) of what is left, and xvfb/flystage/flystage-web/
pulse/mediamtx share whatever is left over in between — so the thread count and the core
count cannot drift apart. flycast gets its own group, separate from the page/xvfb group,
because the release container measured Chromium's compositor starving when it shared cores with the x264
encoder (63% of captured frames unchanged, against 2% on the NVENC dev box). `05-deploy.sh`
refuses to write the drop-ins until the conf actually carries
`CPUSET`, because an `AllowedCPUs=` outside the container's own cpuset leaves
`cpuset.cpus.effective` empty and the unit unstartable.

`CPUSET` is now eight WHOLE physical cores on one socket (node 0 for the release container, the GPU-local
socket; node 1 for the platformer container), not the four-cores-plus-SMT-siblings set `docs/design/gpu.md`
section 1 originally specified: a four-core set cannot be partitioned so that flysim owns three
whole cores AND Chromium gets its 1.4, and P0 run 2 measured that sharing physical cores is
exactly what breaks real time.

**Temporarily, the release container is on node 1** (`CPUSET=1,3,5,7,9,11,13,15`, `CPUMEMS=1` — the
override block at the bottom of `<release-env>`, 2026-09-16). The dev container `fly-spike`,
the dev/demo box, is running on node 0's eight whole cores; two stream containers on the
same physical cores is the sharing the measurement above says breaks real time. The release container
moves back to node 0 when the dev container is retired — the env file's own comment carries the
procedure, and node 1 is the platformer container's documented home, so the two cannot both be pinned there.

The manual procedure below is kept for the diagnosis — what PVE picks, and how to see what it
really is:

```sh
# 1. See what PVE picked, and what it really is:
pct exec <release-ctid> -- lscpu -e                       # ONLINE=yes rows are the container's cpuset
for c in <those cpus>; do \
  printf 'cpu%s node=%s siblings=%s\n' "$c" \
    "$(cat /sys/devices/system/cpu/cpu$c/topology/physical_package_id)" \
    "$(cat /sys/devices/system/cpu/cpu$c/topology/thread_siblings_list)"; done

# 2. Replace it with whole cores on ONE socket (host-side, /etc/pve/lxc/<ctid>.conf):
#    eight distinct physical cores of node1, no SMT siblings, no cross-node hop.
echo 'lxc.cgroup2.cpuset.cpus: 1,3,5,7,9,11,13,15' >> /etc/pve/lxc/<ctid>.conf
pct stop <release-ctid> && pct start <release-ctid>

# 3. Partition it inside the guest so the sim, the browser and the encoder never
#    share a physical core (drop-ins, since the numbers are per host):
pct exec <release-ctid> -- mkdir -p /etc/systemd/system/flysim.service.d
printf '[Service]\nAllowedCPUs=1,3,5\n' | \
  pct exec <release-ctid> -- tee /etc/systemd/system/flysim.service.d/cpuset.conf
for u in xvfb flystage flycast; do \
  pct exec <release-ctid> -- mkdir -p /etc/systemd/system/$u.service.d; \
  printf '[Service]\nAllowedCPUs=7,9,11,13,15\n' | \
    pct exec <release-ctid> -- tee /etc/systemd/system/$u.service.d/cpuset.conf; done
pct exec <release-ctid> -- systemctl daemon-reload
pct exec <release-ctid> -- systemctl restart flysim.service flycast.service flystage.service
```

`RAYON_NUM_THREADS` in `units/flysim.service` must match the number of physical cores
flysim's own `AllowedCPUs` covers — it ships as 3 for that reason, with the measurements
in its header comment. `AllowedCPUs` does work through cgroup2 delegation in an
unprivileged LXC (`cpuset.cpus.effective` comes back exactly as asked); it is
deliberately not committed to the unit files because the numbers are per host and per
container.

### 5. Secrets

`06-secrets.sh <release-env>` reads `pass twitch/<channel>-key` on the operator
box and pipes it straight into `systemd-creds encrypt` inside the container — see that
script's own header for the exact relay shape (direct `pct exec` if run on the host
itself, `ssh` relay otherwise, mirroring `docs/design/infra.md` section 4's own
flip-to-Twitch example). Nothing here needs a manual step beyond having the `pass`
entries populated ahead of time.

### 5. `fly-nvidia-majors.service` (GPU, one time)

Required before any `GPU=1` container is provisioned. Full instructions and the reboot
ordering are in **`infra/host/README.md`**; the short form, on the host as root:

```sh
install -o root -g root -m 0755 infra/host/fly-nvidia-majors.sh /usr/local/sbin/fly-nvidia-majors.sh
install -o root -g root -m 0644 infra/host/fly-nvidia-majors.service /etc/systemd/system/fly-nvidia-majors.service
/usr/local/sbin/fly-nvidia-majors.sh --dry-run 122 150 151 199    # prints the diff, writes nothing
systemctl daemon-reload && systemctl enable --now fly-nvidia-majors.service
```

Why by hand and not in `provision.sh`: it writes `/etc/pve`, which nothing reachable
through `pct exec` can do, it has to be ordered against `pve-guests.service`, and its
default id list **includes the neighbouring GPU container** (`ml`, the GPU workload in the neighbouring container), whose passthrough majors are
currently stale. The operator approved touching the neighbouring GPU container on 2026-09-15 — the majors service is what
repairs it, and it is a production container, so that approval is the reason this is not
automated. Do not extend the list to other people's containers without the same
conversation.

The reason it exists at all: the `nvidia-uvm` and `nvidia-caps` majors are allocated
dynamically and move across boots (uvm went 509 → 511 on the LLM host on 2026-08-29), so a
major written once into a container config goes stale **silently** —
`docs/design/gpu.md` section 1. `verify.sh`'s GPU section asserts the unit is enabled
and active, and that the conf's majors still match `/proc/devices`.

Optional, not scripted, and a separate decision: `nvidia-smi -pm 1` (persistence mode).
With a 24/7 client attached it changes little. **Do not** set `nvidia-smi -c
EXCLUSIVE_PROCESS` — compute mode stays `Default` or the neighbouring GPU container breaks
(`docs/design/gpu.md` section 5).

The stock installer `<host-stage>/NVIDIA-Linux-x86_64-580.76.05.run` also has to be present on
The host for `02-base.sh` to push into each container. It was there on 2026-09-15. Not
the `-custom.run`: only its kernel modules were patched, and those stay on the host.

## The infra-repo update checklist

**Do not edit `the operator's infra repo` from this repo or this worktree.** `docs/design/infra.md`
section 7 ("Every phase, on completion") lists exactly what to add there, on `the operator's infra repo`'s
own `master` branch (`git pull --rebase` first, never force-push, markdown only, no
`.sh` artifacts — the operator's infra repo holds documentation, not scripts):

- New `the infra repo's flybrain page` describing this whole setup (ports, units, timers,
  secrets shape, backup shape) at the level of detail `the infra repo's panels page` and
  the operator's other services already use.
- `the host's notes`'s guest table gains rows for the release container `fly-pokemon` and the platformer container
  `fly-platformer`.
- `main.md`'s static reservations table gains the two reserved addresses (once VERIFYed free and
  reserved in the router).
- `the infra repo's panels page` gains the "fly live / rank / uptime" panel tile and the new
  `panel-bridge` source entry (`docs/design/infra.md` section 5), including the
  freshness budget (10s) in that doc's per-source table.

Do this **after** each rollout phase completes (P0 spike results, P1 provisioning
proven, P2 Twitch test channel, P3 public + bridge, P4 second demo), not once at the
end — `docs/design/infra.md` section 7 is explicit that this is a per-phase step.

## Testing this repo

```sh
infra/tests/lint.sh
```

Runs `shellcheck` (falls back to `bash -n` if not installed) on every script — including
`host/` and the extension-less `bin/` launchers — `systemd-analyze verify` (skipped if
not installed) on every unit file including `host/*.service`, and a structural check that
every `ExecStart*=` directive points at a standard system path, something this repo
deploys under `/opt/fly/bin` or `/opt/fly/current`, or a `/usr/local/sbin/` script that
exists in `host/`.

It also runs two behavioural smoke tests, both entirely local:

- `bin/flycast-launch --print nvenc` / `--print x264` assemble the real ffmpeg command
  and are checked for the right encoder, the `fps=30:round=near` filter and
  `-fps_mode:v cfr` on **both** backends, and the absence of a leftover `-r 30`.
- `host/fly-nvidia-majors.sh` is run against a fake `/proc/devices` and a temp conf dir:
  changed-then-unchanged, the live majors written, the block placed before a
  `[snapshot]` section, and a refusal to write when a major reads empty.

This never touches the host, `pct`, any container, or `/etc/pve` — it is static analysis
plus two dry runs against a temp directory.
