#!/usr/bin/env bash
# infra/02-base.sh ENVFILE
#
# Base OS convergence inside the container: packages, the fly user,
# /etc/tmpfiles.d, /dev/shm sizing, journald caps, font AA. Idempotent:
# apt-get install -y and mkdir -p converge naturally, everything else goes
# through converge_file. See docs/design/infra.md section 2.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -eq 1 ] || die "usage: $0 ENVFILE"
load_env "$1"
require_pve_host
need pct

PACKAGES="xvfb x11-utils x11-xserver-utils xauth chromium pulseaudio pulseaudio-utils \
ffmpeg fonts-dejavu-core fonts-liberation2 fonts-noto-core fonts-noto-color-emoji \
fontconfig prometheus-node-exporter curl ca-certificates jq rsync sysstat procps zstd sudo"
# Nothing was added for the capture-freeze fix (2026-09-16): bin/wait-for-stage
# needs `xdpyinfo` and `xwininfo` (x11-utils) and `pactl`
# (pulseaudio-utils), and check 9 of fly-watchdog needs `ffmpeg` — all three
# packages are already above. `xdotool` was the other candidate for the window
# probe and is deliberately NOT installed: xwininfo does the job and the image
# stays as it is. Do not prune x11-utils or pulseaudio-utils; flycast's
# ExecStartPre depends on them (infra/docs/capture-freeze.md section 3.2).
#
# sudo is not in docs/design/infra.md's package list, added because
# fly-watchdog runs as User=fly and needs a narrow NOPASSWD surface to
# restart units and reboot (config/fly-sudoers below) — see the deviation
# note in the final report.

# GPU containers additionally need Debian's libglvnd dispatch stack, and it
# has to be in place BEFORE the NVIDIA installer runs. The neighbouring GPU container's own
# /var/log/nvidia-installer.log records what happens otherwise:
# "Missing libraries: libEGL.so.1 ... Will not install libglvnd libraries",
# leaving libEGL_nvidia.so.0 with no dispatch library in front of it and no
# working EGL at all (docs/design/gpu.md section 2). NVENC itself does not
# need these, but the spike's option-(d) measurement does, and half an EGL
# stack is a worse thing to debug later than six packages now.
if [ "${GPU:-0}" = 1 ]; then
    PACKAGES="$PACKAGES libglvnd0 libgl1 libglx0 libegl1 libgles2 libvulkan1"
fi

log "02-base: apt-get update + install (naturally idempotent)"
ct_exec "$CTID" -- env DEBIAN_FRONTEND=noninteractive apt-get update -q
# shellcheck disable=SC2086
ct_exec "$CTID" -- env DEBIAN_FRONTEND=noninteractive apt-get install -y -q $PACKAGES

# No sshd, ever, by design (docs/design/infra.md section 0: "the fly
# containers get no sshd, all ops go through pct exec").
#
# The Debian 13 standard LXC template SHIPS openssh-server (confirmed on the
# P0 spike: 1:10.0p1-7+deb13u4, and pct create even generates its host keys),
# so this used to `die` on every freshly created container and provisioning
# could never get past step 2. Remove it instead of refusing to continue, then
# assert it is gone — the design's requirement is that no sshd runs here, not
# that a human deletes it by hand first.
if ct_exec "$CTID" -- sh -c 'dpkg -l openssh-server 2>/dev/null | grep -q ^ii'; then
    log "02-base: openssh-server present (Debian template ships it) — disabling and purging"
    ct_exec "$CTID" -- sh -c 'systemctl disable --now ssh.socket ssh.service 2>/dev/null || true'
    ct_exec "$CTID" -- env DEBIAN_FRONTEND=noninteractive apt-get purge -y -q openssh-server
fi
if ct_exec "$CTID" -- sh -c 'dpkg -l openssh-server 2>/dev/null | grep -q ^ii'; then
    die "openssh-server is still installed in CT $CTID after purge; infra design requires no sshd, all ops via pct exec"
fi

log "02-base: apt-mark hold chromium ffmpeg (unattended upgrades must not swap them under a live stream)"
ct_exec "$CTID" -- apt-mark hold chromium ffmpeg
# The NVIDIA userspace installed below is deliberately NOT apt-managed: the
# .run installer drops its libraries outside dpkg's world entirely, so
# unattended-upgrades cannot see them and cannot move them. Here that is a
# feature, not an oversight — those libraries must stay in lockstep with the
# host's hand-patched kernel module, and the only thing allowed to change
# them is a deliberate re-run of this script after the host driver moves
# (docs/runbook.md "GPU driver version lockstep"). The flip side is that no
# security update will ever reach them either; that is the accepted trade.

# ---------------------------------------------------------------------------
# NVIDIA userspace driver (docs/design/gpu.md section 2).
#
# Kernel modules stay on the host: --no-kernel-modules (PLURAL — this is the
# exact flag the neighbouring GPU container's own /var/log/nvidia-installer.log records, and
# --no-kernel-module is a different, older spelling). The STOCK .run is
# correct, not the -custom.run: only the kernel modules were patched for
# the 7.x kernel, the userspace payload is unmodified, and the neighbouring GPU container's copy is
# byte-size identical to the stock file.
#
# Idempotent: skipped entirely when nvidia-smi in the container already
# reports $NVIDIA_VERSION, which is what makes a second provision.sh run
# converge nothing (verify.sh's "no second-run restarts" check).
# ---------------------------------------------------------------------------
if [ "${GPU:-0}" = 1 ]; then
    need_var NVIDIA_VERSION
    NV_RUN_HOST="${HOST_STAGE_DIR:-/root}/NVIDIA-Linux-x86_64-${NVIDIA_VERSION}.run"
    NV_RUN_CT="/root/nvidia.run"

    ct_driver="$(ct_exec "$CTID" -- sh -c \
        'command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1' \
        2>/dev/null | tr -d ' \r' || true)"

    if [ "$ct_driver" = "$NVIDIA_VERSION" ]; then
        log "02-base: NVIDIA userspace already at $NVIDIA_VERSION in CT $CTID, skipping the installer"
    else
        [ -f "$NV_RUN_HOST" ] || die "02-base: GPU=1 but $NV_RUN_HOST is not on this host. Do NOT substitute the -custom.run (that one only differs in its kernel modules, which we do not install) — fetch the stock installer for $NVIDIA_VERSION first. See docs/design/gpu.md section 2."
        log "02-base: installing the NVIDIA userspace $NVIDIA_VERSION into CT $CTID (container reports: '${ct_driver:-none}')"
        ct_push_file "$CTID" "$NV_RUN_HOST" "$NV_RUN_CT" 0755
        ct_exec "$CTID" -- sh "$NV_RUN_CT" --no-kernel-modules --silent --no-x-check \
            || die "02-base: the NVIDIA installer failed in CT $CTID; read /var/log/nvidia-installer.log inside the container (pct exec $CTID -- tail -50 /var/log/nvidia-installer.log)"
        ct_exec "$CTID" -- rm -f "$NV_RUN_CT"

        ct_driver="$(ct_exec "$CTID" -- sh -c \
            'nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1' \
            2>/dev/null | tr -d ' \r' || true)"
        if [ "$ct_driver" != "$NVIDIA_VERSION" ]; then
            # Not fatal on purpose: the userspace install can succeed while
            # nvidia-smi still fails, because nvidia-smi needs the device
            # nodes and the cgroup allow lines too. That is a passthrough
            # problem, not an install problem, and verify.sh's GPU section
            # is where it gets diagnosed properly.
            log "02-base: WARNING nvidia-smi in CT $CTID reports '${ct_driver:-nothing}', expected $NVIDIA_VERSION." \
                "If it says 'Driver/library version mismatch' the host module moved; if it cannot find a device," \
                "the passthrough block or the majors are wrong. Run infra/verify.sh $1 for the full picture."
        else
            log "02-base: NVIDIA userspace $NVIDIA_VERSION installed and nvidia-smi agrees"
        fi
    fi
else
    log "02-base: GPU=0 (or unset) — skipping the NVIDIA userspace install"
fi

# ---------------------------------------------------------------------------
# VirtualGL (docs/design/gpu.md section 3 option (d), and
# infra/docs/virtualgl-spike.md for the run that proved it).
#
# Only needed by CHROMIUM_PROFILE=vgl, which wraps the kiosk in
# `vglrun -d egl0` to get hardware WebGL out of the Quadro while keeping Xvfb
# and x11grab exactly as they are. Gated on GPU=1 AND a non-empty
# VIRTUALGL_VERSION, so a GPU container that only wants NVENC installs nothing
# extra and a release env that never sets the variable is unaffected.
#
# apt has no virtualgl in Debian, so this is an upstream .deb. It is NOT
# apt-managed and gets no security updates, the same accepted trade as the
# NVIDIA userspace above. VIRTUALGL_DEB_SHA256 is mandatory when
# VIRTUALGL_VERSION is set: the .deb is fetched by hand onto the host (upstream
# publishes no checksum file, only a GPG signature over the deb's own
# _gpgorigin member), so the env file's hash is the only thing standing between
# "the file I verified" and "the file that got installed".
#
# mesa-utils comes with it for glxinfo. eglinfo comes from VirtualGL's own
# /opt/VirtualGL/bin — do NOT rely on Debian's: mesa-utils-bin's eglinfo
# segfaults on this container, and it cannot enumerate EGL devices anyway,
# which is the one thing this configuration needs to check.
# ---------------------------------------------------------------------------
if [ "${GPU:-0}" = 1 ] && [ -n "${VIRTUALGL_VERSION:-}" ]; then
    need_var VIRTUALGL_DEB_SHA256
    VGL_DEB_HOST="${HOST_STAGE_DIR:-/root}/virtualgl_${VIRTUALGL_VERSION}_amd64.deb"
    VGL_DEB_CT="/root/virtualgl.deb"

    ct_vgl="$(ct_exec "$CTID" -- sh -c \
        'dpkg-query -W -f="\${Version}" virtualgl 2>/dev/null' 2>/dev/null | tr -d ' \r' || true)"

    # dpkg's version is "3.1.5-20260814" for VIRTUALGL_VERSION=3.1.5, so match
    # on the leading upstream version rather than requiring the build date.
    case "$ct_vgl" in
        "$VIRTUALGL_VERSION"|"$VIRTUALGL_VERSION"-*)
            log "02-base: VirtualGL $ct_vgl already installed in CT $CTID, skipping" ;;
        *)
            [ -f "$VGL_DEB_HOST" ] || die "02-base: VIRTUALGL_VERSION=$VIRTUALGL_VERSION but $VGL_DEB_HOST is not on this host. Fetch it from https://github.com/VirtualGL/virtualgl/releases/download/${VIRTUALGL_VERSION}/virtualgl_${VIRTUALGL_VERSION}_amd64.deb and check its sha256 against VIRTUALGL_DEB_SHA256 in $1 (and, better, its GPG signature — see infra/docs/virtualgl-spike.md)."
            host_sha="$(sha256sum "$VGL_DEB_HOST" | cut -d' ' -f1)"
            [ "$host_sha" = "$VIRTUALGL_DEB_SHA256" ] || die "02-base: $VGL_DEB_HOST has sha256 $host_sha but VIRTUALGL_DEB_SHA256 in $1 is $VIRTUALGL_DEB_SHA256. Refusing to install an unexpected package."
            log "02-base: installing VirtualGL $VIRTUALGL_VERSION into CT $CTID (container reports: '${ct_vgl:-none}')"
            ct_push_file "$CTID" "$VGL_DEB_HOST" "$VGL_DEB_CT" 0644
            ct_sha="$(ct_exec "$CTID" -- sha256sum "$VGL_DEB_CT" 2>/dev/null | cut -d' ' -f1 | tr -d ' \r')"
            [ "$ct_sha" = "$VIRTUALGL_DEB_SHA256" ] || die "02-base: the .deb landed in CT $CTID with sha256 $ct_sha, expected $VIRTUALGL_DEB_SHA256 — the push corrupted it."
            ct_exec "$CTID" -- env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
                mesa-utils "$VGL_DEB_CT" \
                || die "02-base: installing VirtualGL in CT $CTID failed"
            ct_exec "$CTID" -- rm -f "$VGL_DEB_CT"
            ;;
    esac

    # The one check worth making here rather than in verify.sh, because it is
    # the difference between "VirtualGL is installed" and "VirtualGL can see
    # the card": egl0 must be the NVIDIA device. Not fatal — a fresh container
    # is provisioned before the GPU is necessarily usable, and verify.sh is
    # where the GPU picture gets diagnosed properly.
    if ct_exec "$CTID" -- sh -c '/opt/VirtualGL/bin/eglinfo egl0 -B 2>/dev/null | grep -qi nvidia'; then
        log "02-base: VirtualGL sees the NVIDIA card as EGL device egl0"
    else
        log "02-base: WARNING VirtualGL's egl0 in CT $CTID is not NVIDIA (or eglinfo failed)." \
            "CHROMIUM_PROFILE=vgl will render on llvmpipe or not at all." \
            "Check: pct exec $CTID -- /opt/VirtualGL/bin/eglinfo -e, then ... eglinfo egl0 -B"
    fi
elif [ -n "${VIRTUALGL_VERSION:-}" ]; then
    log "02-base: VIRTUALGL_VERSION is set but GPU is not 1 — skipping VirtualGL (it would have no card to render on)"
fi

log "02-base: fly system user"
if ! ct_exec "$CTID" -- id fly >/dev/null 2>&1; then
    ct_exec "$CTID" -- adduser --system --group --home /var/lib/fly --shell /usr/sbin/nologin fly
else
    log "02-base: fly user already exists"
fi

log "02-base: /etc/tmpfiles.d/fly.conf"
INFRA_DIR="$(infra_root)"
converge_file "$CTID" "$INFRA_DIR/config/fly-tmpfiles.conf" /etc/tmpfiles.d/fly.conf 0644 root:root >/dev/null
# --create is idempotent (mkdir -p semantics per entry), so it is safe to
# run every time regardless of whether the file itself just changed.
ct_exec "$CTID" -- systemd-tmpfiles --create /etc/tmpfiles.d/fly.conf

log "02-base: /dev/shm sized to 1G via a dev-shm.mount drop-in (CONFIRMED INERT in an unprivileged LXC — see below)"
# RESOLVED on the P0 spike (2026-09-15), and the answer is the pessimistic
# branch: in an unprivileged Debian 13 LXC on PVE 9 there is no
# dev-shm.mount unit at all (`systemctl list-unit-files | grep dev-shm` is
# empty), because LXC mounts /dev/shm itself before systemd starts. So this
# drop-in is INERT — it is left in place only so the intent is visible and
# so the day a template does own the unit it starts working.
#
# Two further measured facts:
#   * /dev/shm comes up as a 94.4G tmpfs (half the host's RAM), not the
#     "small tmpfs" the design expected, so Chromium's renderer is in no
#     danger of running out of space there. The residual risk is the
#     reverse: a runaway could charge 94G of page cache against the
#     container's 8G memory limit and be OOM-killed instead of getting
#     ENOSPC.
#   * It cannot be fixed from inside the guest either. The mount carries
#     `uid=100000,gid=100000` from the unprivileged id-map, and
#     `mount -o remount,size=1G /dev/shm` as root in the guest fails with
#     "fsconfig() failed: tmpfs: Invalid uid '100000'" because that uid is
#     not representable inside the container's user namespace.
# The only real fix is a host-side lxc.mount.entry in
# /etc/pve/lxc/<ctid>.conf, which is an the operator by-hand step (see
# infra/README.md and docs/runbook.md) — nothing reachable through pct exec
# can do it.
ct_exec "$CTID" -- mkdir -p /etc/systemd/system/dev-shm.mount.d
SHM_CHANGED="$(converge_file "$CTID" "$INFRA_DIR/config/dev-shm-override.conf" /etc/systemd/system/dev-shm.mount.d/override.conf 0644 root:root)"
if [ "$SHM_CHANGED" = changed ]; then
    ct_exec "$CTID" -- systemctl daemon-reload
    ct_exec "$CTID" -- systemctl restart dev-shm.mount 2>/dev/null \
        || log "02-base: dev-shm.mount not restartable (expected: the unit does not exist in an unprivileged LXC)"
fi
# Report the size that is actually in effect either way, so a run's log says
# what /dev/shm is rather than what it was asked to be.
log "02-base: /dev/shm in effect: $(ct_exec "$CTID" -- sh -c 'findmnt -no SIZE /dev/shm' 2>/dev/null || echo unknown)"

log "02-base: masking node_exporter's ipmitool collector (no IPMI in an LXC)"
# Debian's prometheus-node-exporter ships helper timers for hardware that a
# container does not have. Only the ipmitool one is enabled by default, and it
# fails every 60 s forever: "Failed to start
# prometheus-node-exporter-ipmitool-sensor.service". The journal is the
# watchdog's only notification channel, so a guaranteed once-a-minute error in
# it is not cosmetic. Measured on the P0 spike run 2. The others
# (smartmon, nvme, mellanox-hca-temp) ship disabled and are left alone.
ct_exec "$CTID" -- systemctl mask prometheus-node-exporter-ipmitool-sensor.timer \
    prometheus-node-exporter-ipmitool-sensor.service
ct_exec "$CTID" -- systemctl stop prometheus-node-exporter-ipmitool-sensor.timer 2>/dev/null || true

log "02-base: node_exporter textfile collector (config/node-exporter-default)"
# docs/design/infra.md section 5 item 1. Without the flag the textfile
# collector is off and every .prom this repo writes
# (/var/lib/node_exporter/textfile/, from fly-watchdog, fly-retention and
# fly-backup-stage) is scraped by nobody. Found on the P0 spike run 2.
NODE_EXPORTER_CHANGED="$(converge_file "$CTID" "$INFRA_DIR/config/node-exporter-default" /etc/default/prometheus-node-exporter 0644 root:root)"
if [ "$NODE_EXPORTER_CHANGED" = changed ]; then
    ct_exec "$CTID" -- systemctl restart prometheus-node-exporter
fi

log "02-base: journald caps (config/journald.conf)"
ct_exec "$CTID" -- mkdir -p /etc/systemd/journald.conf.d
JOURNALD_CHANGED="$(converge_file "$CTID" "$INFRA_DIR/config/journald.conf" /etc/systemd/journald.conf.d/fly.conf 0644 root:root)"
if [ "$JOURNALD_CHANGED" = changed ]; then
    ct_exec "$CTID" -- systemctl restart systemd-journald
fi

log "02-base: grayscale antialiasing (config/fonts-local.conf), matches flystage's --disable-lcd-text"
FONTS_CHANGED="$(converge_file "$CTID" "$INFRA_DIR/config/fonts-local.conf" /etc/fonts/local.conf 0644 root:root)"
if [ "$FONTS_CHANGED" = changed ]; then
    ct_exec "$CTID" -- fc-cache -f || true
fi

log "02-base: fly-watchdog sudoers (config/fly-sudoers), validated before install"
TMP_SUDOERS="/tmp/fly-sudoers.$$"
ct_push_file "$CTID" "$INFRA_DIR/config/fly-sudoers" "$TMP_SUDOERS" 0440
ct_exec "$CTID" -- visudo -c -f "$TMP_SUDOERS" || die "config/fly-sudoers failed visudo -c, refusing to install it"
ct_exec "$CTID" -- install -o root -g root -m 0440 "$TMP_SUDOERS" /etc/sudoers.d/fly-watchdog
ct_exec "$CTID" -- rm -f "$TMP_SUDOERS"

log "02-base: done"
