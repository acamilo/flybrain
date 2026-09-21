> Design document produced 2026-09-15 evening by a planning agent after a read-only probe of the host. Host changes listed here need the operator's approval before they run.

# GPU design: Quadro RTX 4000 on the host for the flybrain stream containers

Research and design only. Nothing on the host was changed. Every host fact below was read
live on 2026-09-15 ~19:00 EDT from `the host` with read-only commands; anything not read live
is marked UNVERIFIED.

## 0. What is actually on the host (all verified live)

| Fact | Value |
|---|---|
| card | `03:00.0 TU104GL [Quadro RTX 4000]`, Dell subsystem `1028:12a0`, `driver in use: nvidia` |
| VRAM / driver | 8192 MiB, 580.76.05, CUDA 13.0, persistence **Disabled**, compute mode **Default** |
| GPU NUMA node | `/sys/bus/pci/devices/0000:03:00.0/numa_node` = **0** |
| sibling functions | `03:00.1` HD Audio (`snd_hda_intel`), `03:00.2` USB xHCI, `03:00.3` UCSI (`nvidia-gpu`). None are wanted by any container. |
| loaded modules | `nvidia`, `nvidia_uvm`, `nvidia_modeset`. **No `nvidia_drm`** |
| built modules | `/lib/modules/7.0.14-11-pve/updates/dkms/` has only `nvidia.ko`, `nvidia-modeset.ko`, `nvidia-uvm.ko`. `modinfo nvidia-drm` = not found |
| `/dev/dri/card0` | the **Matrox G200eR2 BMC**, proven by `/dev/dri/by-path/pci-0000:08:00.0-card -> ../card0`. The NVIDIA card has no DRI node at all, because nvidia-drm was never built |
| ICDs | EGL `/usr/share/glvnd/egl_vendor.d/10_nvidia.json`; Vulkan `/etc/vulkan/icd.d/nvidia_icd.json` (note: `/etc`, not `/usr/share`) |
| encode libs present | `libnvidia-encode.so.1`, `libnvcuvid.so.1`, `libnvidia-fbc.so.1`, `libcuda.so.1`, `libEGL_nvidia.so.0`, `libnvidia-egl-xlib/xcb.so.1` |
| host swap | **0**. `CORES/SWAP_MB=2048` in the env file therefore buys nothing; per-unit `MemoryMax` is the only real cap |
| CPU topology | node 0 = even CPUs `0,2,...,38`; node 1 = odd. SMT sibling of cpu N is cpu N+20. 10 physical cores per socket. **Node 0 is the GPU-local socket** |
| installers on host | `<host-stage>/NVIDIA-Linux-x86_64-580.76.05.run` (stock, 393,683,997 B) and `<host-stage>/NVIDIA-Linux-x86_64-580.76.05-custom.run` (hand-patched, 637,970,662 B) plus the two kernel patches |

**The card is already in production use.** the neighbouring GPU container `ml` (16 cores, 120 GB) runs the GPU workload in the neighbouring container
(a Python inference server on its own port, 96 MiB resident at
idle) on this GPU, via passthrough lines already in `/etc/pve/lxc/<neighbour-ctid>.conf`. This is the
single most important new constraint and it is not in any flybrain doc yet.

### Majors read from `/proc/devices` today

```
195 nvidia            <- the frontend; there is no separate "nvidia-frontend" line on this kernel
195 nvidia-modeset
195 nvidiactl
236 nvidia-caps
511 nvidia-uvm        <- DYNAMIC, changes every boot
234 nvidia-nvswitch   235 nvidia-nvlink   (not needed, no NVLink)
```

Node minors: `nvidia0` 195:0, `nvidiactl` 195:255, `nvidia-modeset` 195:254,
`nvidia-uvm` 511:0, `nvidia-uvm-tools` 511:1, `nvidia-caps/nvidia-cap1` 236:1 (mode 0600),
`nvidia-cap2` 236:2 (mode 0444). All `nvidia*` nodes are 0666, so the unprivileged `fly`
user can open them with no group membership. `nvidia-cap1` is MIG configuration and will
be unreadable inside an unprivileged CT (host root owns it 0600); that is harmless.

### A finding that contradicts the operator's own notes, and should be recorded

`/etc/pve/lxc/<neighbour-ctid>.conf` allows `c 195:*`, `c 509:*`, `c 234:*`. The real uvm major is
**511** and the real caps major is **236**, so two of its three allow lines are stale, and
`234` is `nvidia-nvswitch`. The neighbouring GPU container nevertheless has `/dev/nvidia-uvm` (511:0) **open**:
`/proc/9694/fd` holds three fds on `/dev/nvidia-uvm` plus `nvidia-caps/nvidia-cap2`.
`/usr/share/lxc/config/common.conf` line 21 does set `lxc.cgroup2.devices.deny = a`, so a
default-deny policy is configured, yet access to a non-allowed major succeeds. Conclusion:
on this host (PVE 9.2.10, liblxc 7.0.0, kernel 7.0.14-11-pve) the cgroup2 eBPF device
filter is **not being enforced** for these containers. Cause not established (UNVERIFIED);
`bpftool` is not installed so the attached programs could not be listed.

Do not lean on that. Write the correct majors anyway, because (a) a PVE upgrade could start
enforcing, and (b) the failure mode the memory warns about is silent. But also note the
practical consequence: on the host the *bind mount* is the load-bearing part, and the real
silent breaker is a bind whose source did not exist at container start, which
`optional,create=file` turns into an empty regular file. That is exactly the tell the
the operator's own GPU-driver notes record ("every other `nvidia*` entry being a regular file
means passthrough is broken").

---

## 1. LXC device passthrough for the release container / the platformer container / the dev container

Block to append to `/etc/pve/lxc/<id>.conf` (paths after the first field are relative, no
leading slash, matching the neighbouring GPU container):

```
# BEGIN fly-nvidia (generated by fly-nvidia-majors.service, do not edit by hand)
lxc.cgroup2.devices.allow: c 195:* rwm
lxc.cgroup2.devices.allow: c 511:* rwm
lxc.cgroup2.devices.allow: c 236:* rwm
lxc.mount.entry: /dev/nvidia0 dev/nvidia0 none bind,optional,create=file
lxc.mount.entry: /dev/nvidiactl dev/nvidiactl none bind,optional,create=file
lxc.mount.entry: /dev/nvidia-uvm dev/nvidia-uvm none bind,optional,create=file
lxc.mount.entry: /dev/nvidia-uvm-tools dev/nvidia-uvm-tools none bind,optional,create=file
lxc.mount.entry: /dev/nvidia-caps dev/nvidia-caps none bind,optional,create=dir
# END fly-nvidia
```

- `195` covers `nvidia0`, `nvidiactl` and `nvidia-modeset` in one line, and is
  compile-time static in the driver (`NV_MAJOR_DEVICE_NUMBER`), so it never drifts.
- `511` (uvm) and `236` (caps) are dynamically allocated and **must be regenerated at every
  boot**, see below.
- `/dev/nvidia-modeset` is deliberately **not** bind-mounted for the NVENC-only design: it
  is needed only for modesetting, EGL display and an in-container Xorg. If option (b) or
  (d) in section 3 is ever taken, add
  `lxc.mount.entry: /dev/nvidia-modeset dev/nvidia-modeset none bind,optional,create=file`.
  The memory's note that it appears as a zero-byte regular file inside another container on the host and is
  harmless applies only because nothing there uses it.

  > **2026-09-16, VirtualGL spike on the dev container: option (d) does NOT need the modeset node,
  > and this bullet is wrong for it.** Measured with the dev container's passthrough block exactly as
  > written above — no modeset bind in `/etc/pve/lxc/<ctid>.conf` at all — VirtualGL's EGL back
  > end enumerated the card and rendered on it: `/opt/VirtualGL/bin/eglinfo -e` lists `egl0`
  > and `egl1`, and `eglinfo egl0 -B` reports `NVIDIA Corporation` /
  > `Quadro RTX 4000/PCIe/SSE2` / `4.6.0 NVIDIA 580.76.05`. The distinction this bullet
  > collapses is the EGL **display** (`EGL_DEFAULT_DISPLAY`, an X or KMS surface — that one
  > does want modeset) versus the EGL **device** platform (`EGL_EXT_platform_device`,
  > headless, off a `/dev/nvidia0` handle), and option (d) is the second. So (d) needs **no
  > host conf change and no container restart**. Option (b), an in-container Xorg, is
  > untested and presumably still does — but (b) is rejected for the `/dev/tty0` reason
  > anyway. See `infra/docs/virtualgl-spike.md`.
- `nvidia-caps` is mounted as a **directory** (`create=dir`), not per-node, which is what
  the neighbouring GPU container does and what works.
- Keep `optional`. Without it a missing node blocks container start, so a post-kernel-update
  driver failure takes the whole stream down instead of degrading to the x264 path. The
  price is that a broken bind is silent, which `verify.sh` must catch (section 8).

### The dynamic `nvidia-uvm` major

What the operator's hosts already have, verified: the host runs `nvidia-devnodes.service`
(`Before=pve-guests.service`) which executes only
`nvidia-smi; nvidia-modprobe -u -c=0; nvidia-modprobe -m`. It materialises the nodes but
**does not rewrite majors**, which is why the neighbouring GPU container's numbers are stale. The LLM host has the other
half, `pve-nvidia-majors.service`, which per those notes "materialises
`/dev/nvidia*` and rewrites the majors into `/etc/pve/lxc/<neighbour-ctid>.conf`" and really fired when
uvm moved 509 -> 511 on 2026-08-29.

There is no module option that pins the uvm major (`nvidia-uvm` registers with
`register_chrdev(0, ...)`; no `major=` parameter exists, UNVERIFIED but consistent with
every observation here), so the rewrite service is the fix. Port the LLM host's unit to the host and
widen it to the fly CTs:

```
# /etc/systemd/system/fly-nvidia-majors.service
[Unit]
Description=Refresh NVIDIA device majors in LXC guest configs
Wants=nvidia-devnodes.service
After=nvidia-devnodes.service
Before=pve-guests.service
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/sbin/fly-nvidia-majors.sh 122 150 151 199
[Install]
WantedBy=multi-user.target
```

`/usr/local/sbin/fly-nvidia-majors.sh` (sketch): `nvidia-modprobe -u -c 0 -m`; read
majors with `awk -v n=nvidia-uvm '$2==n{print $1;exit}' /proc/devices`; refuse to write if
any major is empty; for each CT id, delete the `# BEGIN fly-nvidia`..`# END fly-nvidia`
block, append a freshly generated one, and write it back only if it differs (`/etc/pve` is
pmxcfs, so build in `/tmp` and `cat >` into place, never `mv` across the filesystem, and
avoid needless cluster writes). Including `122` in the list fixes the neighbouring GPU container's stale lines as a
side effect, which is a small, welcome cleanup but is a change to a production container
and needs the operator's nod.

> **2026-09-16: the sentinel block cannot be trusted to bracket anything, and this design
> note is wrong as written.** PVE rewrites `/etc/pve/lxc/<id>.conf` on every lifecycle
> operation and does not preserve layout: one `pct stop` / `pct start` of the dev container re-emitted
> the conf with every PVE key sorted alphabetically, every **comment hoisted to the top of
> the file**, and every raw `lxc.*` key moved to the end (PVE 9.2.10, measured). The
> `# BEGIN`/`# END` pair ends up adjacent and empty at the top, with the eight lines it used
> to bracket at the bottom outside any block.
>
> A "delete the block, append a fresh one" converge therefore (a) reports `changed` on every
> single run, which rewrites pmxcfs needlessly and makes `01-create-ct.sh` restart a live
> container — i.e. take the stream down — on every provisioning run, and (b) leaves the
> hoisted-out lines behind as duplicates that carry the OLD dynamic majors, which is exactly
> the stale-major hole this unit exists to close.
>
> Both `fly-nvidia-majors.sh` and `lib/common.sh`'s `converge_conf_block` now converge
> **lines, not bytes**: a line is owned if it is one of the lines they generate, or (for the
> cpuset block) if its key prefix is one they own, or (for the nvidia block) if it is one of
> the five `/dev/nvidia*` bind targets they write, or an `allow: c N:* rwm` whose major is
> theirs, is registered to an `nvidia*` device, or is registered to nothing at all while
> sitting in a kernel dynamic char-major range (234-254, 384-511). "unchanged" means every
> desired line is present exactly once and no owned-but-undesired line remains, position and
> comments ignored. `infra/tests/lint.sh` asserts the normalized-conf case.
>
> One thing that ownership rule deliberately does **not** clean up, and it is worth knowing:
> The neighbouring GPU container's stale `lxc.cgroup2.devices.allow: c 509:* rwm` — written when `nvidia-uvm` was 509
> — now resolves to **`509 mei`**, the Intel Management Engine interface, on this host's
> `/proc/devices`. The rule keeps any major that resolves to a non-NVIDIA device, so the
> script reports it and leaves it. It is latent rather than live (nothing binds `/dev/mei0`
> into the neighbouring GPU container, so the node does not exist there), but it is the predicted "dead allow line for
> a major some future driver gets dynamically" and it should be deleted by hand from
> `/etc/pve/lxc/<neighbour-ctid>.conf` by whoever owns the neighbouring GPU container.

### AppArmor and nesting

**No `lxc.apparmor.profile` line is needed.** Verified: the neighbouring GPU container is `unprivileged: 1`,
`features: nesting=1`, has no apparmor stanza at all, and runs CUDA fine. Both fly CTs
already need `nesting=1` for Chromium's namespace sandbox (p0 measurement 1 passed:
`unshare --user --pid true` exits 0 as `fly`), and nesting plus NVIDIA passthrough
coexisting is proven on this host by the neighbouring GPU container.

Do not set `unconfined`. The security trade-off, stated plainly: an unprivileged CT with
`nesting=1` is already a relaxed AppArmor profile (`lxc-container-default-with-nesting`)
that permits nested user and mount namespaces; adding the NVIDIA character devices hands
the guest an ioctl surface into a host kernel module that historically has had
privilege-escalation CVEs, and that surface is now shared with the neighbouring GPU container. That is an accepted,
already-taken risk on this host. Going `unconfined` on top would additionally drop the
mount and `/proc` restrictions that are the remaining wall, on the one host that runs
another service's production workload. Not worth it, and not required.

### CPU pinning (fixes the `fly_lag_seconds` finding)

The p0 run got PVE's automatic cpuset `1,3,9,19,23,27,30,35`: seven physical cores, one
SMT pair, and split across both NUMA nodes. Since the neuron sweep is
memory-bandwidth-bound, pin to whole physical cores on node 0, which is also the GPU-local
socket:

```
# The release container
lxc.cgroup2.cpuset.cpus: 0,2,4,6,20,22,24,26
lxc.cgroup2.cpuset.mems: 0
# The platformer container
lxc.cgroup2.cpuset.cpus: 8,10,12,14,28,30,32,34
lxc.cgroup2.cpuset.mems: 0
# The dev container (spike; use the release container's set, the release container will not exist yet)
```

That is 4 physical cores plus their SMT siblings each, 8 logical CPUs, matching
`CORES=8`, leaving node 0 cpus `16,18,36,38` and all of node 1 for the host, the neighbouring GPU container and
the neighbouring production service. Keep `cores: 8` in the PVE config for lxcfs `cpuinfo` masking; the raw
`lxc.cgroup2.cpuset.cpus` line is applied after PVE's own and wins (UNVERIFIED on PVE 9.2,
check with `taskset -pc 1` inside the CT). Set `RAYON_NUM_THREADS=4` in
`flysim.service`, since the measured knee was 4 threads at 1.886x and 8 threads was
*worse* (1.399x).

> **2026-09-16, GPU run on the dev container — this set is superseded, and the raw key is confirmed.**
> Two corrections, both measured:
>
> 1. **The raw key wins.** `taskset -pc 1` inside the dev container returns exactly the configured
>    cpus and cgroup2's `cpuset.cpus.effective` agrees, on PVE 9.2.10. No longer UNVERIFIED.
> 2. **Eight WHOLE cores, not four cores plus their siblings.** The sets above are 8 logical
>    CPUs over only 4 physical cores, and they cannot be partitioned the way run 2 proved is
>    necessary: flysim needs three whole cores (1.753x, against 1.375x the moment a thread
>    lands on an SMT sibling), which leaves Chromium's 1.42 core and ffmpeg sharing one whole
>    core plus the siblings of flysim's own. the env file now carry
>    `CPUSET=0,2,4,6,8,10,12,14` (the release container/spike, node 0, GPU-local) and
>    `CPUSET=1,3,5,7,9,11,13,15` plus `CPUMEMS=1` (the platformer container, node 1 — node 0 has only ten
>    physical cores, so two containers of eight cannot both live there). That is
>    `infra/README.md` step 4's shape, and `05-deploy.sh` now generates the in-guest
>    `AllowedCPUs=` partition from it instead of leaving it to a hand-typed step.

> **2026-09-15, P0 spike run 2:** re-measured under the whole stack, on the automatic
> (non-whole-core) cpuset PVE actually hands out rather than a hand-picked whole-core set
> like this section's. 3 threads on 3 physical cores of one socket beat 4 threads spread
> across that cpuset's 6 physical cores plus 2 SMT siblings (1.753x vs 1.570x) — the
> mechanism is that the sweep wants whole physical cores, and *which socket* they sit on
> matters less than *how many are whole*. The default is now 3, not 4
> (`infra/env/example.env`, `infra/05-deploy.sh`); see `infra/docs/p0-measurements.md`, "the host
> The dev container run 2", section 1.

---

## 2. Userspace driver inside the CT

**A copy already exists on the host and the operator has already done this exact install.**
`/proc/<pid>/root/var/log/nvidia-installer.log`, read through procfs for a process in the
neighbouring GPU container, records the command
verbatim:

```
./nvidia-installer --no-kernel-modules --silent --no-x-check
```

Note the flag is `--no-kernel-modules` (plural), not `--no-kernel-module`. Use the **stock**
`<host-stage>/NVIDIA-Linux-x86_64-580.76.05.run`, not `-custom.run`: only the kernel modules were
patched, the userspace payload is unmodified, and the neighbouring GPU container's `/root/nv.run` is byte-size
identical to the stock file.

Recipe for `02-base.sh` (idempotent, skips if `nvidia-smi` already reports 580.76.05):

```sh
# host side, once per CT
pct push "$CTID" <host-stage>/NVIDIA-Linux-x86_64-580.76.05.run /root/nvidia.run -perms 0755
# in CT, before the installer: give libglvnd the dispatch libs so EGL is usable
apt-get install -y libglvnd0 libgl1 libglx0 libegl1 libgles2 libvulkan1
sh /root/nvidia.run --no-kernel-modules --silent --no-x-check
```

The `libegl1` line matters: the neighbouring GPU container's install log shows
`Missing libraries: libEGL.so.1 ... Will not install libglvnd libraries`, so the neighbouring GPU container has
`libEGL_nvidia.so.0` with no dispatch library in front of it and cannot actually do EGL.
Install Debian's libglvnd stack first (or pass `--install-libglvnd`).

What the installer leaves that we care about: `libnvidia-encode.so.1` (NVENC, dlopened by
ffmpeg's ffnvcodec), `libcuda.so.1`, `libnvidia-ml.so.1`, `nvidia-smi`,
`libEGL_nvidia.so.0` plus `/usr/share/glvnd/egl_vendor.d/10_nvidia.json`,
`libGLX_nvidia.so.0`, `libnvidia-egl-{xlib,xcb}.so.1`,
`/etc/vulkan/icd.d/nvidia_icd.json`, `libnvcuvid.so.1`, `libnvidia-fbc.so.1`. No CUDA
toolkit is needed; NVENC needs only the driver libraries.

Verification inside the CT, in order:

```sh
ls -l /dev/nvidia*            # every entry must be a CHARACTER device (c...), not a file
nvidia-smi                    # must print "Driver Version: 580.76.05", GPU 0 Quadro RTX 4000
ffmpeg -hide_banner -encoders | grep -E 'h264_nvenc|hevc_nvenc'
ffmpeg -hide_banner -f lavfi -i testsrc2=s=1920x1080:r=30 -t 3 -c:v h264_nvenc -f null -   # real session test
apt-get install -y mesa-utils-extra vulkan-tools   # only if section 3 option b/d is taken
eglinfo -B ; vulkaninfo --summary | grep -i 'deviceName\|driverName'
```

`ffmpeg -encoders` only proves the encoder was compiled in; the `testsrc2` run is the one
that proves the device, the library and the cgroup all line up.

---

## 3. Rendering path for Chromium

### (a) Xvfb plus NVIDIA EGL: does not work, and this is not a tuning problem

NVIDIA's GL and EGL X11 paths require an X server running the NVIDIA X driver. Xvfb is a
software X server with no DRI3 and no NVIDIA driver, so `glxinfo` against it reports
"GLX extension missing" or falls back to Mesa, and `eglInitialize` on that display has no
matching device. This is precisely why VirtualGL exists: its documentation states the 3D X
server "has to be a real X server attached to the GPU. It can be headless, but it can't be
virtual", while the 2D X server may be Xvfb
([virtualgl.org](https://virtualgl.org/vgldoc/2_0/), [ArchWiki](https://wiki.archlinux.org/title/VirtualGL),
[NVIDIA forum: Nvidia driver breaks Xvfb](https://forums.developer.nvidia.com/t/nvidia-driver-breaks-xvfb-on-rhel-6/42529)).
So `--use-gl=angle --use-angle=gl-egl` on `DISPLAY=:99` will silently fall back to
SwiftShader, and `--use-angle=vulkan` fails the same way because the NVIDIA Vulkan ICD has
no xlib presentation support on a non-NVIDIA X server. Rejected.

### (b) Real Xorg with the nvidia driver inside the CT: blocked by the VT, on purpose

`nvidia-xconfig --allow-empty-initial-configuration` plus a 1920x1080 virtual screen is the
right config, and x11grab would keep working unchanged. The blocker is that Xorg calls
`xf86OpenConsole` and needs `/dev/tty0` plus a free VT; in a container it dies with
"parse_vt_settings: Cannot open /dev/tty0"
([NVIDIA/nvidia-docker#1557](https://github.com/NVIDIA/nvidia-docker/issues/1557),
[x11docker#5](https://github.com/mviereck/x11docker/issues/5),
[VirtualGL#98](https://github.com/VirtualGL/virtualgl/issues/98)). Passing it would need
`lxc.cgroup2.devices.allow: c 4:* rwm` plus a bind of the host's `/dev/tty0`, which gives an
unprivileged, nesting-enabled guest write access to the **host console** (console
spoofing, and `c 4:*` also covers the host serial consoles). On the host that runs the neighbouring production service
production, no. Also note the nvidia X driver would be installed by the same `.run`, and
`--no-x-check` was used in the neighbouring GPU container precisely to skip that question. Rejected for prod;
allowed in the spike CT only if a measurement genuinely needs a baseline number.

### (c) `--headless=new` with GPU plus CDP screencast

This is the configuration that demonstrably works headless with NVIDIA, because the GPU
process uses the EGL/Vulkan device platform and never needs an X surface. The published
flag set is
`--headless=new --use-angle=vulkan --enable-features=Vulkan --disable-vulkan-surface --enable-unsafe-webgpu`
([jasonmayes/headless-chrome-nvidia-t4-gpu-support](https://github.com/jasonmayes/headless-chrome-nvidia-t4-gpu-support),
linked from [Chromium's own docs](https://chromium.googlesource.com/chromium/src/+/main/docs/gpu/server-side-headless-linux-chrome-with-gpus.md)),
with `--use-gl=angle --use-angle=gl-egl --use-cmd-decoder=passthrough` as the EGL variant
([Chromium docs](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/docs/gpu/using-gpu-hardware-in-headless-chrome.md)).
But the capture side is unchanged from the reason it was rejected: `Page.startScreencast`
JPEG-encodes every frame on the CPU inside the browser, then ffmpeg decodes it again, and
the audio path loses its shared X/pulse clock. The GPU does not help the part that was
expensive. Still rejected.

### (d) VirtualGL 3.x EGL back end, as the escape hatch

VirtualGL 3.0+ added an EGL back end that needs **no 3D X server** at all; it renders on
the GPU through the EGL device platform and blits the result into the 2D X server, which
may be Xvfb. That is the one arrangement that gets real GPU WebGL **and** keeps
`-f x11grab` and the existing A/V timing. Cost: `vglrun -d egl0 chromium ...`, GLX
interposition against a Chromium that now prefers EGL (VirtualGL's EGL-in-app interposition
maturity for Chromium is UNVERIFIED), a readback and blit of 1920x1080 per frame, and a new
failure surface interacting with the renderer sandbox. Do not put this in the MVP.

> **2026-09-16: measured on the dev container, and it PASSES. This is no longer unproven.** Full run in
> `infra/docs/virtualgl-spike.md`; shipped as `CHROMIUM_PROFILE=vgl`
> (`infra/config/chromium-flags.vgl`, `infra/bin/flystage-launch`), on the spike container
> only. Against the paper-fly baseline on the same box, same release, same stream:
>
> | | paper fly | VirtualGL |
> |---|---|---|
> | `__stage.fly()` | `paper` (requested `webgl`) | **`webgl`**, 35-37 fps |
> | unmasked WebGL renderer | no context at all | **ANGLE (NVIDIA Corporation, Quadro RTX 4000/PCIe/SSE2, OpenGL ES 3.2)** |
> | `chrome://gpu` webgl / 2d_canvas | `disabled_off` / `disabled_software` | **`enabled` / `enabled`** |
> | Chromium CPU, 180 s, all processes | 1.379 core | **1.057 core** |
> | page rAF | 59.91 fps | 59.99 fps |
>
> Four corrections to the paragraphs above, all measured:
>
> 1. **No modeset node, no host conf change, no restart** — see section 1's dated note.
> 2. **The interposition worked first try; the sandbox did not.** VirtualGL faked GLX for a
>    Chromium running `--use-gl=angle --use-angle=gl-egl` with no trouble. What failed is
>    that VirtualGL opens its own second X11 connection inside the process it is loaded
>    into, which the **GPU-process sandbox** forbids: `[VGL] ERROR: in VirtualWin-- 77:
>    Could not clone X display connection`, then `GPU process exited unexpectedly:
>    exit_code=256` about twice a second until Chromium reports `(gl=disabled,angle=none)`.
>    `--disable-gpu-sandbox` is therefore mandatory and is the only difference between
>    `chromium-flags.gpu` and `chromium-flags.vgl`. The **renderer** sandbox — the one this
>    paragraph worried about, and the one that contains the page — is untouched.
> 3. **The 1920x1080 readback and blit is cheap, and the GPU pays for the 2D canvas, not the
>    fly.** The fly's own draw got *more* expensive per call (0.152 ms → 0.45-0.65 ms), and
>    the saving came from the page renderer dropping 0.913 → 0.629 core because
>    `--enable-gpu-rasterization --canvas-oop-rasterization --enable-zero-copy` moved the
>    Game Boy screen, the bars and the connectome onto the card. The GPU process absorbed
>    all of that plus the blit for *less* than it had been spending on software compositing
>    (0.449 → 0.411). The "measurement that decides it" below therefore asked the wrong
>    question: the GPU is not worth it for the fly, it is worth it for everything else.
> 4. **x11grab really is untouched.** `ffprobe` on the LAN HLS read h264 1920x1080 30/1 plus
>    aac 48 k before and after; `flycast`, `flysim` and `mediamtx` were never restarted.
>
> Still not a recommendation for the release containers. Reasons, in order: the longest
> observation is ~25 minutes and a GL context on a card shared with the GPU workload in the neighbouring container is
> exactly the kind of thing that fails at hour six; nothing has run a the GPU workload in the neighbouring container job while the
> kiosk held a context; and `--disable-gpu-sandbox` on a 24/7 public stream is the operator's call,
> not a spike's. `the release env file` and `<platformer-env>` leave
> `CHROMIUM_PROFILE` unset.

### Recommendation

> **2026-09-16: still the right call for the release containers, but for one reason fewer.**
> "(d) is unproven here" is no longer true — see the dated note in (d). What stands is the
> rest: a GL context on a card shared with an interactive the GPU workload in the neighbouring container is an unsoaked black-screen
> risk for a 24/7 stream, and (d) needs `--disable-gpu-sandbox`. The dev container runs it;
> The release container/151 do not.

**Keep Xvfb and a CPU Chromium for the MVP. Use the GPU only for NVENC.** Reasons: the
only Chromium GPU path compatible with x11grab is (d) and it is unproven here; the measured
Chromium cost is already small (whole-frame paint p50 2.5 ms, p95 6.1 ms, rAF 59.02 fps);
the only WebGL element in the design is an 800x220 strip with about 3,000 triangles
(`docs/design/fly-avatar.md`), which the avatar doc already says falls back to a hand
projected 2D "paper fly" if SwiftShader costs more than 0.5 core; and putting a Chromium
GPU process on a card shared with an interactive the GPU workload in the neighbouring container adds a black-screen failure mode
for a 24/7 stream in exchange for very little.

Concrete flag change to `infra/config/chromium-flags`: **none for the GPU variant at MVP.**
Keep `--disable-gpu --disable-software-rasterizer` as-is for the 2D-canvas build. Ship a
second file, `config/chromium-flags.gpu`, selected by `FLY_CHROMIUM_PROFILE=gpu`, which
drops those two lines and adds, for the option (d) spike only:

```
--use-gl=angle
--use-angle=gl-egl
--use-cmd-decoder=passthrough
--ignore-gpu-blocklist
--enable-gpu-rasterization
--canvas-oop-rasterization
--enable-zero-copy
```

The measurement that decides it, taken on the dev container in this order:
1. `--disable-gpu` build, three.js fly strip loaded, SwiftShader: `pidstat -u 5 360`
   summed over all chromium processes, plus the page's own fly-render ms/frame. If the fly
   costs under 0.5 core at 30 fps and total Chromium stays under 1.0 core, **stop here, the
   GPU is not needed for rendering** and the paper fallback is not needed either.
2. Only if (1) fails: run (d) and require `chrome://gpu` (dumped over CDP, not eyeballed)
   to read "Hardware accelerated" for both *WebGL* and *Canvas*, `GL_RENDERER` containing
   "Quadro RTX 4000", the fly strip holding 30 fps, and total Chromium CPU lower than
   in (1) by at least 0.3 core. Anything less and the added fragility is not paid for.

---

## 4. Capture and encode with NVENC

Debian's ffmpeg has nvenc enabled. Verified indirectly but strongly: the libavcodec inside
The neighbouring GPU container contains `h264_nvenc`, `hevc_nvenc`, `NVIDIA NVENC H.264 encoder`,
`NvEncodeAPICreateInstance` and `h264_cuvid` in its string table. That build is
libavcodec.so.59 (ffmpeg 5.1 era), not trixie's 7:7.1.5-0+deb13u1, so the Debian 13 case is
**UNVERIFIED until one command runs in the spike CT**:
`ffmpeg -hide_banner -encoders | grep nvenc`. Debian has not changed that build flag in
years, so a static build (BtbN or johnvansickle) should not be needed; keep it as the
documented fallback only, because a static ffmpeg would break the `apt-mark hold` and
unattended-upgrade story that `02-base.sh` deliberately set up.

Proposed `flycast.service` encoder block for `FLY_ENCODER=nvenc`, replacing the libx264
block, and replacing the trailing `-r 30`:

```
-filter_complex "[0:v]fps=30:round=near,format=nv12[v];[1:a]aresample=async=1:min_hard_comp=0.100:first_pts=0[a]"
-map "[v]" -map "[a]"
-c:v h264_nvenc -preset p4 -tune hq -profile:v high -level 4.1
-rc cbr -b:v 6000k -maxrate 6000k -bufsize 12000k
-g 60 -keyint_min 60 -no-scenecut 1 -rc-lookahead 15
-bf 2 -b_ref_mode middle -spatial-aq 1 -temporal-aq 1
-fps_mode:v cfr
-c:a aac -b:a 160k -ar 48000 -ac 2
```

Notes and the flag names to confirm (`ffmpeg -h encoder=h264_nvenc` on the spike CT, since
these are the SDK-10 preset names and some have underscore aliases):
- `p1..p7` presets plus `-tune hq|ll|ull|lossless` are the current NVENC preset model; the
  old `slow/medium/fast` names are deprecated aliases
  ([NVENC Preset Migration Guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-preset-migration-guide/index.html)).
  Start at `p4`, and because NVENC is nearly free here, **try `p6`/`p7`** and keep whichever
  passes the quality check. That is the real win: quality at fixed bitrate, not CPU.
- `-rc cbr` sets NV_ENC_PARAMS_RC_CBR; ffmpeg enables filler-data insertion for it, which
  is what `-x264-params nal-hrd=cbr:filler=1` was doing on the x264 path (UNVERIFIED for
  7.1, check `-h encoder=h264_nvenc` for a `cbr`/filler option and confirm the FLV output
  is HRD-conformant).
- `-spatial-aq`/`-temporal-aq` and `-no-scenecut` may need underscores in 7.1.
- `-sc_threshold 0` is an x264-only option; `-no-scenecut 1` is its NVENC equivalent and is
  only meaningful with lookahead enabled.
- Turing supports B-frames as reference; `-b_ref_mode middle` is a free quality gain but
  adds ~66 ms of encoder delay, which is irrelevant for Twitch. Drop it if RTMP timestamps
  misbehave.
- `-profile:v high -level 4.1` unchanged. 8-bit yuv420p/nv12 only; NVENC H.264 has no
  4:2:0 10-bit.

**`-hwaccel cuda` is the wrong tool here**: there is no decode step to accelerate, x11grab
delivers raw BGRA. Do the BGRA to NV12 conversion on the CPU first (`format=nv12`, roughly
0.2 to 0.3 core with swscale on Haswell, UNVERIFIED) and measure. If it shows up,
`hwupload_cuda,scale_cuda=format=nv12` moves it onto the GPU at the cost of a 249 MB/s
BGRA PCIe upload; h264_nvenc already uploads system-memory frames internally, so this only
relocates the colour conversion. Measure before adding it.

**NvFBC: skip, and for a harder reason than "needs a custom ffmpeg".** NvFBC captures the
framebuffer of an X screen driven by the NVIDIA X driver (or a KMS framebuffer). There is
no such X screen here, and no `nvidia-drm` module for the KMS path, so NvFBC has nothing to
capture even though `libnvidia-fbc.so.1` is installed and the Quadro is a qualifying SKU.
x11grab's cost is a memcpy of 8.3 MB per frame, 249 MB/s at 1080p30, which is noise.
Do confirm XShm is actually in use (p0 measurement 9); if MIT-SHM were missing, every frame
would be a full `XGetImage` round trip through the X socket, which would plausibly explain
the dup/drop finding.

**Two demos, two NVENC sessions, one card: fine.** Quadro-class parts are not subject to
the consumer concurrent-session cap; GeForce is limited (2, then 3, then 5, now 8 sessions)
while Quadro/professional SKUs are unrestricted
([NVIDIA support matrix](https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new),
[VideoCardz on the 8-session change](https://videocardz.com/newz/nvdia-geforce-gpus-now-support-up-to-8-concurrent-nvenc-encoding-sessions),
[Tom's Hardware](https://www.tomshardware.com/news/nvidia-increases-concurrent-nvenc-sessions-on-consumer-gpus)).
TU104 has a single NVENC engine, and two 1080p30 H.264 sessions is a small fraction of it
(exact headroom UNVERIFIED; read `nvidia-smi --query-gpu=utilization.encoder` during the
spike).

---

## 5. Sharing one card between two containers, and with the neighbouring GPU container

- Both CTs get the identical device block from section 1. Nothing is partitioned; the
  driver multiplexes.
- **VRAM is the contended resource, and the neighbouring GPU container is the threat, not the second demo.**
  Budget: 2 NVENC sessions at roughly 150 to 250 MB each (UNVERIFIED, measure), plus about
  100 MB of driver context per process, so the whole stream side should sit under 1 GB of
  the 8 GB. the GPU workload in the neighbouring container runs video models on this card and can plausibly take 6 to 7 GB. If
  the GPU workload in the neighbouring container is mid-generation when `flycast` restarts, `NvEncOpenEncodeSessionEx` can fail
  for lack of memory, and `flycast` will crash-loop with a healthy-looking card.
- Mitigations, in order of value: (1) make `FLY_ENCODER` fall back automatically, so
  `flycast` retries once with `libx264` if the nvenc session cannot be opened, and exports
  `fly_encoder_backend{backend="nvenc|x264"}` so the panel shows the degradation;
  (2) export `fly_gpu_memory_used_bytes` and `fly_gpu_encoder_util` from a host-side or
  in-CT `nvidia-smi --query-gpu=...` textfile collector, matching the metrics container's existing `gpu_*`
  collectors; (3) agree a VRAM ceiling for the GPU workload in the neighbouring container with whoever owns the neighbouring GPU container.
- Chromium's GPU process does not contend at all in the recommended design, because there
  isn't one. If option (d) is ever taken, add roughly 300 to 600 MB per container.
- **Compute mode: leave it `Default`. Confirmed no change needed, and actively do not set
  `nvidia-smi -c EXCLUSIVE_PROCESS`**, which would let whichever container got there first
  lock the card and would break the neighbouring GPU container. `nvidia-smi -pm 1` (persistence) is a separate,
  optional host hygiene item; with a 24/7 client attached it changes little, and it is a
  host change that needs the operator.

> **2026-09-16: the budget above is measured now, and there is a THIRD stream-side consumer it
> does not account for.** Read live on the dev container during the 3 h CUDA soak in
> `infra/docs/cuda-on-dev.md`, from `nvidia-smi --query-compute-apps` and `pmon`, with the neighbouring GPU container's
> the GPU workload in the neighbouring container context resident throughout:
>
> | process | container | type | GPU memory |
> |---|---|---|---|
> | `flysim`, the **CUDA LIF backend** | the dev container | compute | **160 MiB** (57 MiB of buffers + ~103 MiB of context) |
> | `ffmpeg`, flycast's NVENC session | the dev container | compute | **238 MiB** |
> | Chromium's GPU process under VirtualGL | the dev container | graphics | **139 MiB** |
> | the GPU workload in the neighbouring container | the neighbouring GPU container | compute | 96 MiB (idle, no generation running) |
> | | | whole card | **633–674 MiB of 8,192** over three hours |
>
> Three corrections to the bullets above:
>
> 1. **One NVENC session is 238 MiB** — the top of this section's "150 to 250 MB each
> (UNVERIFIED, measure)" band. That estimate was right; it is no longer unverified, for one
> session at 1920x1080 / 6000 kbps / p4.
> 2. **Chromium's GPU process is 139 MiB, not "roughly 300 to 600 MB per container."** That
> estimate for option (d) was high by a factor of two to four. VirtualGL blitting into Xvfb
> does not cost what a full compositing GPU process costs.
> 3. **The LIF backend is a stream-side consumer this section predates, and it changes the
> arithmetic for two containers.** Per container the stream side is now 160 + 238 + 139 =
> **537 MiB**, so one container fits comfortably under the 1 GB ceiling this section sets and
> **two would be about 1.07 GB, over it.** The ceiling is not a hard limit — 8 GB less
> the GPU workload in the neighbouring container's 6-to-7 GB worst case is the real constraint — but if the release container and the platformer container both run
> CUDA + NVENC + VirtualGL, the stream side is over 1 GB and this section should be re-read
> before anyone adds the second demo.
>
> Stability, which is what the soak was for: **flysim's own device memory did not move by one
> MiB across nineteen ten-minute samples** (160 MiB every time), and the whole-card figure
> wobbled 5.5 % with no trend. The drift `infra/docs/virtualgl-soak.md` records is Chromium's,
> not the backend's. Mitigation (2) in the list above — exporting `fly_gpu_memory_used_bytes`
> from an `nvidia-smi` textfile collector — is still not built, and this soak had to sample the
> host by hand because of it.

---

## 6. Future: the LIF kernel on CUDA

Feasible and probably a large win, but it cannot be the same kernel. The sweep is 139,255
neurons and 2,700,513 edges in CSR, stepped every simulated millisecond, and the CPU
version is memory-bandwidth-bound (measured: 1.886x real time at 4 threads, and *worse* at
8, which is the signature of bandwidth saturation). A 2.7M-nonzero SpMV-plus-threshold step
is tiny for a Turing card with 416 GB/s of bandwidth, so the arithmetic suggests one to two
orders of magnitude of headroom, with the per-step kernel-launch overhead (about 5 to 10 us
per launch, so 5 to 10 ms per simulated second at 1 kHz) becoming the floor unless steps are
batched or CUDA graphs are used. The blocker is the project's bit-exactness contract:
`lif-1ms-f64-v2` and `fly-kc-mbon-rstdp-v2` are pinned for checkpoint compatibility, and
the TS and Rust cores agree at 0 ulp. A GPU reduction over a neuron's incoming edges has a
non-deterministic summation order across warps and across launch configurations, so
float64 results will differ in the last bits and then diverge through the threshold
nonlinearity. Do not try to make the GPU kernel bit-exact (it would require sorted,
fixed-order sequential reductions per neuron and would throw away most of the speedup).
Ship it as a separate, explicitly non-bit-exact backend with its own version string, e.g.
`lif-1ms-f64-cuda-v1`, refuse to load a checkpoint across backends, and keep the CPU kernel
as the oracle that the equivalence tests in `packages/brain/tests/legacy/` run against.
Not for the MVP, and worth noting that the 24/7 stream does not need the speed: it needs
exactly 1.0x, which the CPU already delivers with 1.8x of headroom.

> **2026-09-16: spiked on the dev container, and this section's central claim is WRONG.** The tick **is**
> bit-exact on the GPU: 10,000 ticks on `data/fafb-v783` with membrane, refractory, the spike list,
> the RNG, the rates and the whole plasticity state compared after every tick, zero divergences,
> and the existing golden suite passes through the backend unchanged. The kernel version string
> stays `lif-1ms-f64-v2` and a live checkpoint restores across backends with no migration. Full
> record in `infra/docs/lif-cuda-spike.md`; branch `spike/lif-cuda`, not merged.
>
> Where the reasoning above goes wrong: "a GPU reduction over a neuron's incoming edges has a
> non-deterministic summation order" is true of a *reduction*, and propagation does not need to be
> one. Each target's membrane depends on nothing but its own previous value and its own sequence of
> addends, so one thread per target applying them in ascending base-edge order — which is exactly
> the order the sequential kernel applies them in — reproduces the arithmetic operation for
> operation, with no atomics on a float and no cross-thread reduction anywhere. And it does **not**
> "throw away most of the speedup": the paragraph assumed sorted fixed-order reductions would be
> the expensive part, when in fact steps 1-4 cost 36 us of a 326 us tick and the expensive part is
> the edge traffic, which is a data-structure question independent of determinism.
>
> Two predictions here did hold. Launch overhead is real but small (seven-to-twelve kernels per
> tick, and batching 32 ticks per call buys 12 %). And the stream does not need the speed: the
> measured factor is 1.89x raw and 1.53x in the agent loop, against the CPU kernel's uncontended
> 1.886x at four threads — so this is **parity in absolute terms**. What it is not parity on is
> cores: the GPU path reached 1.53x using 1.02 host cores. The one-to-two-orders-of-magnitude
> headroom the first paragraph predicted from bandwidth arithmetic did **not** materialise, because
> a tick only touches 3.6 % of the edges and the cost is latency and per-target serialisation, not
> bandwidth.

---

## 7. Revised measurement list for the GPU spike (the dev container recreated)

Recreate the dev container from `<dev-env>` with `local-zfs` rootfs (SSD-pool headroom was freed), the
section 1 NVIDIA block, and the release container's cpuset. Order matters; stop and fix rather than pushing
past a failure.

**New, GPU:**
1. `ls -l /dev/nvidia*` inside the CT: every entry a character device. Then `nvidia-smi`
   prints 580.76.05 and Quadro RTX 4000. Then reboot the host once and re-check, to prove the
   majors service actually fires (this is the check the operator's own notes says is the silent one).
2. `ffmpeg -encoders | grep nvenc`, then the `testsrc2 -> h264_nvenc -f null -` session test.
3. flycast on nvenc: `pidstat -u 5 360` for ffmpeg over 30 minutes. Target: under 0.4 core
   total, against the x264-veryfast baseline of an expected 1.5 to 2.5 cores. Record
   `nvidia-smi --query-gpu=utilization.encoder,memory.used` every 10 s alongside.
4. Encoded quality: capture 60 s of the live page to a near-lossless reference
   (`-c:v ffv1 -level 3`), then encode the same source with x264 veryfast 6000k, nvenc p4,
   nvenc p6. Compute SSIM and VMAF against the reference, and eyeball the 1 px UI borders,
   the 5x game panel and the small right-rail type, which is where NVENC's weaker psy
   modelling shows. Pick the preset from this, not from CPU alone.
5. `chrome://gpu` dumped over CDP with the default (`--disable-gpu`) flags, recorded as the
   baseline, plus the fly-strip SwiftShader measurement (old measurement k, never run):
   fly ms/frame and total Chromium CPU at 30 fps. This is the decision input for section 3.
6. Two-session check: run a second ffmpeg nvenc session at 1080p30 in the same CT (standing
   in for the platformer container) and confirm both hold 30 fps with no session-limit error.

**Carried over, still unrun:**
7. Chromium CPU over 30 min (old 3), total per-container load via
   `systemd-cgtop -1 --order=cpu` (old 5), now with the pinned cpuset.
8. Audio null sink exercised for real: fire sugar SFX at the live page (the p0 run never
   did), then A/V sync drift at minute 1 versus minute 55 from a 60-minute recording
   (old 6). Target under 100 ms/h.
9. Checkpoint envelope size and commit latency on `local-zfs` (old 7). This still decides
   the 5 s versus 300 s cadence question.
10. Restore drill under systemd: `kill -9` flysim, confirm restart, page reconnect,
    checkpoint continuity. Never run under systemd.
11. 1 h soak minimum, ideally 4 h (old 8): RSS flat, `visibilityState` visible throughout,
    rAF cadence stable, `/dev/shm` not exhausted, journal within cap, Xvfb never
    restarted, pulse sink never SUSPENDED, and now also GPU memory flat and encoder
    session never re-opened.
12. `ffmpeg -protocols | grep rtmps`, x11grab confirmed using XShm (old 9); VLC on the LAN
    against the MediaMTX HLS URL (old 10).

**The two open issues from the last run:**
13. **ffmpeg dup/drop about 4 each per second.** Hypothesis: x11grab's own grab clock
    jitters against the output CFR grid, so nearly every frame lands slightly off-slot and
    ffmpeg pays with a matched dup and drop. Fix to try, in order: (i) move the rate
    decision into the filter graph with `fps=30:round=near` and replace the trailing
    `-r 30` with `-fps_mode:v cfr` (`-vsync` is the deprecated spelling), so dup/drop only
    happens on a real gap rather than on sub-frame jitter; (ii) confirm XShm; (iii) if it
    persists, grab at `-framerate 60` and decimate to 30, which converts dups into drops
    only, at more CPU. Accept under 0.2 dup/s and under 0.2 drop/s sustained over 30
    minutes. Also re-check with nvenc, since the x264 encoder's own pacing was in the loop.
14. **`fly_lag_seconds` 1.005 under systemd.** Change all three inputs at once and
    re-measure for 10 minutes at speed 1.0: the section 1 cpuset plus `cpuset.mems: 0`,
    `RAYON_NUM_THREADS=3` (2026-09-15: dropped further than this section's original 4 —
    see the section 1 note and `infra/docs/p0-measurements.md` "the host the dev container run 2")
    instead of 6, and keep `Nice=-5 CPUWeight=400` on flysim while
    lowering `flycast` from `CPUWeight=100` (it will barely need CPU on nvenc). Pass is
    realtime factor 1.0 with `fly_lag_seconds` under 0.05 held for 10 minutes with the
    whole stack running and no build tools competing.

---

## 8. Provisioning and runbook changes

| File | Change |
|---|---|
| `infra/01-create-ct.sh` | after `pct create`, converge the `# BEGIN fly-nvidia` block and the two cpuset lines into `/etc/pve/lxc/$CTID.conf`. Prefer appending the block directly (with sentinels, read-modify-write, only on difference) over `pct set`, because `pct set -lxc.*` is not a supported key and the sentinel block is what `fly-nvidia-majors.sh` regenerates. Gate on a new `GPU=1` env var so the env file stays the switch. Then `pct stop`/`pct start` if the block changed, since `lxc.*` only applies at container start |
| `infra/env/example.env` | add `GPU=1`, `FLY_ENCODER=nvenc`, `NVIDIA_VERSION=580.76.05`, `CPUSET=0,2,4,6,20,22,24,26` (151 gets its own), `RAYON_THREADS=3` (2026-09-15: dropped from this table's original 4, see section 1's dated note) |
| `infra/02-base.sh` | add `libglvnd0 libgl1 libglx0 libegl1 libgles2 libvulkan1` to `PACKAGES`; new idempotent step that `pct push`es the stock `.run` and installs with `--no-kernel-modules --silent --no-x-check`, skipped when `nvidia-smi` already reports `$NVIDIA_VERSION`; extend `apt-mark hold` reasoning to note that the NVIDIA userspace is **not** apt-managed and so is invisible to unattended upgrades, which is a feature here |
| `infra/units/flycast.service` | `Environment=FLY_ENCODER=%i`-style is wrong for a non-templated unit; instead read `FLY_ENCODER` from `/etc/fly/fly.env` via `EnvironmentFile=` and move `ExecStart` into a new `infra/bin/flycast-launch` that assembles the encoder block. This mirrors the existing `flystage-launch` pattern and keeps the two flag sets reviewable. Add the automatic one-shot fallback to `x264` and the `fly_encoder_backend` metric |
| `infra/config/chromium-flags` | unchanged for MVP. Add `config/chromium-flags.gpu` (section 3) selected by `FLY_CHROMIUM_PROFILE`, marked in its header as spike-only and unproven |
| `infra/units/flysim.service` | `RAYON_NUM_THREADS=3` (2026-09-15: dropped from 4, see section 1's dated note) |
| `infra/verify.sh` | new checks: every `/dev/nvidia*` inside the CT is a character device (not a regular file); `nvidia-smi` exits 0 and its version string equals `$NVIDIA_VERSION`; the CT's userspace version equals the host's `nvidia-smi` version (lockstep); the majors in `/etc/pve/lxc/$CTID.conf` match `/proc/devices` right now; `taskset -pc 1` inside the CT equals `$CPUSET`; `fly-nvidia-majors.service` is enabled and `active (exited)`; `FLY_ENCODER` matches the backend actually in use |
| host, by hand (record in `infra/README.md` "host-side steps") | install `fly-nvidia-majors.service` plus its script, enable it, and note that it also repairs the neighbouring GPU container's stale lines |
| `infra/docs/runbook.md` | new section "GPU driver version lockstep" |

### Runbook: driver version lockstep

The host kernel modules and the in-container userspace must be the **same version string**.
The host's are a hand-patched 580.76.05 built by DKMS against 7.0.14-11-pve, with the patch
repo at the operator's own kernel-patch repo. What breaks and in what order after a
host driver or kernel upgrade:

1. The guest's `libcuda`/`libnvidia-encode` refuses to talk to a mismatched kernel module.
   `nvidia-smi` inside the CT prints "Failed to initialize NVML: Driver/library version
   mismatch", and `flycast` fails to open an NVENC session. With the fallback in place the
   stream continues on libx264 at about 2 cores; without it, `flycast` crash-loops and the
   stream goes black.
2. A kernel update rebuilds via DKMS, but per those same notes the stock sources do not
   build against 7.x, so fresh patches may be needed. Always check `nvidia-smi` on the host
   **and** in every guest after a kernel update.
3. A driver *upgrade* is a three-step, ordered operation: patch and install the new kernel
   module on the host, reboot (so `fly-nvidia-majors.service` rewrites majors for the new
   module load), then re-run `02-base.sh` for each CT to install the matching userspace,
   then `verify.sh`. Do the spike CT first. Never upgrade the host driver without a window
   in which the streams may be on the x264 path.
4. Also add to the existing "the host reboot order" section: `nvidia-devnodes.service` and the
   new `fly-nvidia-majors.service` must both complete before `pve-guests.service`, and the
   fly CTs' `startup order=4,up=60` is what keeps them behind it.

---

## Risks

1. **Shared card with the GPU workload in the neighbouring container** is the top risk: VRAM exhaustion there causes
   NVENC session-open failures here, and it is an interactive workload nobody is going to
   coordinate with at 3 a.m. Mitigated by the x264 fallback and a VRAM metric, not
   eliminated.
2. **Driver version lockstep** is a new, permanent operational coupling between the host's
   hand-patched driver and two 24/7 containers. This is the risk that will actually bite,
   probably at a kernel update.
3. **The dynamic uvm major** is the documented silent breaker. The new observation that the
   cgroup2 device filter appears unenforced on this host means the current the neighbouring GPU container setup
   survives it by luck; if a PVE upgrade starts enforcing, every GPU container breaks at
   once, silently. The majors service plus the `verify.sh` char-device check is the answer.
4. **Debian 13 nvenc availability** is unverified by one command. Low risk, cheap to check.
5. **NVENC quality at 6000 kbps for a text-and-1px-border UI** could be visibly worse than
   x264 veryfast. Mitigated by testing p4/p6/p7 and by the fact that nvenc frees enough CPU
   that a slower preset is affordable. If it loses on quality, the answer is not "go back to
   x264 veryfast", it is "x264 at `faster` or `medium`, which the freed cores now allow".
6. **No GPU for Chromium** means the 3D fly's fate still rests on the SwiftShader
   measurement, which has never been taken. The paper-fly fallback already exists in the
   design, so this is a scope risk, not a schedule risk.
7. **Host swap is 0**, so `SWAP_MB=2048` is fiction and the OOM story is entirely
   `MemoryMax` plus the watchdog. Unrelated to the GPU but it invalidates a stated
   assumption in `docs/design/infra.md` section 1 and should be corrected there.
8. **One agent owns a host at a time.** Check `$AGENT_CLAIM_LOG` and `who` on the host
   before the spike, per the operator's rule, and note that the neighbouring GPU container belongs to the LLM/the GPU workload in the neighbouring container
   work.

## Go / no-go for the GPU spike

**GO**, with a narrow scope. The recommendation is not "put the GPU in the browser", it is
"put the GPU in the encoder", and that is a small, reversible, already-proven-on-this-host
change: identical passthrough lines to a container that has been running CUDA since August,
the same `.run` installed the same way with a command already in the neighbouring GPU container's own install log,
and an `ffmpeg -c:v h264_nvenc` swap behind one env var with an automatic fallback to the
code path that is running today. The expected payoff is large and specific: roughly 1.5 to
2.5 cores returned per container, which converts a 6-of-8-cores budget into comfortable
headroom on a 2015 Haswell box, and makes a better-quality encode affordable either way.

Blocking conditions to resolve first, all cheap: confirm `nvenc` in Debian 13's ffmpeg;
install `fly-nvidia-majors.service` and prove it survives one reboot; and get agreement on
sharing the card with the neighbouring GPU container. If measurement 3 (ffmpeg under 0.4 core) or measurement 4
(quality no worse than x264 veryfast at the same bitrate) fails, the correct outcome is
**no-go on nvenc and no change at all**, not a broader GPU project, since the CPU path
already met its only hard gate at 1.818x real time.

### Critical Files for Implementation
- infra/01-create-ct.sh
- infra/02-base.sh
- infra/units/flycast.service
- infra/verify.sh
- docs/design/infra.md

Sources: [NVIDIA Video Encode and Decode GPU Support Matrix](https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new), [VideoCardz: 8 concurrent NVENC sessions](https://videocardz.com/newz/nvdia-geforce-gpus-now-support-up-to-8-concurrent-nvenc-encoding-sessions), [Tom's Hardware: Nvidia lifts encoding limits](https://www.tomshardware.com/news/nvidia-increases-concurrent-nvenc-sessions-on-consumer-gpus), [NVENC Preset Migration Guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-preset-migration-guide/index.html), [VirtualGL User's Guide](https://virtualgl.org/vgldoc/2_0/), [ArchWiki VirtualGL](https://wiki.archlinux.org/title/VirtualGL), [NVIDIA forum: driver breaks Xvfb](https://forums.developer.nvidia.com/t/nvidia-driver-breaks-xvfb-on-rhel-6/42529), [Chromium: headless Linux with GPUs](https://chromium.googlesource.com/chromium/src/+/main/docs/gpu/server-side-headless-linux-chrome-with-gpus.md), [Chromium: using GPU hardware in headless Chrome](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/docs/gpu/using-gpu-hardware-in-headless-chrome.md), [headless-chrome-nvidia-t4-gpu-support](https://github.com/jasonmayes/headless-chrome-nvidia-t4-gpu-support), [nvidia-docker#1557 /dev/tty0](https://github.com/NVIDIA/nvidia-docker/issues/1557), [x11docker#5 xf86OpenConsole](https://github.com/mviereck/x11docker/issues/5), [VirtualGL#98 X server in docker](https://github.com/VirtualGL/virtualgl/issues/98)