#!/usr/bin/env bash
# infra/verify.sh ENVFILE
#
# Asserts desired state, exit 1 on drift. docs/design/infra.md section 2:
# "the plan includes an explicit verify.sh that asserts desired state and
# exits non-zero, which is the piece people usually skip."
#
# Run on the host, as root, after provision.sh. Every check prints PASS/FAIL/
# SKIP and the script's own exit code is 0 only if nothing failed (SKIP
# does not fail the run — it means the check could not be evaluated yet,
# e.g. before the stream key exists).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -eq 1 ] || die "usage: $0 ENVFILE"
load_env "$1"
require_pve_host
need pct

FAILED=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; FAILED=1; }
skip() { echo "SKIP: $*"; }

echo "=== verify.sh: $1 (CT $CTID, $HOSTNAME) ==="

# ---------------------------------------------------------------------------
# 0. Is a release installed at all?
#
# A freshly provisioned container has no release artifact (services/flysim,
# apps/stage and services/bridge are packaged separately and installed by
# 05-deploy.sh's first argument), so flysim/flystage/flycast cannot run and
# 07-enable.sh has deliberately not been run: `fly.target` is not enabled and
# nothing is started. That is a legitimate, expected state — infra/README.md's
# quick start and 05-deploy.sh's own header both describe it — and it used to
# make verify.sh report 17 FAILs on a container that was exactly right,
# which trains people to ignore the output. So: /opt/fly/current is the
# discriminator (it is the symlink 05-deploy.sh flips when it installs a
# release), and in its absence the unit checks below assert that the unit
# FILES are converged rather than that the services are running.
# ---------------------------------------------------------------------------
RELEASE_PRESENT=1
if ! ct_exec "$CTID" -- test -e /opt/fly/current 2>/dev/null; then
    RELEASE_PRESENT=0
fi

# ---------------------------------------------------------------------------
# 1. Unit states
# ---------------------------------------------------------------------------
check_unit_active() {
    local unit="$1"
    if ct_exec "$CTID" -- systemctl is-active --quiet "$unit"; then
        pass "unit active: $unit"
    else
        fail "unit NOT active: $unit ($(ct_exec "$CTID" -- systemctl is-active "$unit" 2>&1 || true))"
    fi
}
check_unit_enabled() {
    local unit="$1"
    if ct_exec "$CTID" -- systemctl is-enabled --quiet "$unit"; then
        pass "unit enabled: $unit"
    else
        fail "unit NOT enabled: $unit"
    fi
}

check_unit_file() {
    local unit="$1"
    if ct_exec "$CTID" -- test -f "/etc/systemd/system/$unit"; then
        pass "unit file installed: $unit"
    else
        fail "unit file MISSING from /etc/systemd/system in CT $CTID: $unit (run 05-deploy.sh)"
    fi
}

APP_UNITS="xvfb.service pulse.service mediamtx.service flysim.service flystage-web.service flystage.service flycast.service"
APP_TIMERS="fly-recap.timer fly-retention.timer fly-watchdog.timer"

if [ "$RELEASE_PRESENT" -eq 0 ]; then
    skip "unit states: CT $CTID has no /opt/fly/current — pre-release container, nothing deployed and" \
         "fly.target deliberately not enabled (07-enable.sh not run). Checking the unit FILES instead."
    for u in $APP_UNITS $APP_TIMERS flypush.service flypush-restart.timer flybridge.service fly-recap.service fly-retention.service fly-watchdog.service fly.target; do
        check_unit_file "$u"
    done
else
    for u in $APP_UNITS; do
        check_unit_active "$u"
        check_unit_enabled "$u"
    done
    for t in $APP_TIMERS; do
        check_unit_active "$t"
        check_unit_enabled "$t"
    done
fi

if [ "$PUSH_TARGET" = twitch ]; then
    check_unit_active flypush.service
    check_unit_enabled flypush.service
    check_unit_active flypush-restart.timer
    check_unit_enabled flypush-restart.timer
else
    if ct_exec "$CTID" -- systemctl is-active --quiet flypush.service 2>/dev/null; then
        fail "flypush.service is active but PUSH_TARGET=local"
    else
        pass "flypush.service correctly inactive (PUSH_TARGET=local)"
    fi
fi

# flybridge: Wants=, not Requires= — a failed/missing flybridge must never
# fail verify.sh (docs/design/infra.md: "chat must not be able to stop the
# sim"). Report its state without affecting FAILED.
if ct_exec "$CTID" -- systemctl is-active --quiet flybridge.service 2>/dev/null; then
    pass "flybridge.service active (optional; not required to pass)"
else
    skip "flybridge.service not active (expected before P3 / before services/bridge is built)"
fi

# ---------------------------------------------------------------------------
# 2. No second-run restarts: the latest provision-log run must show zero
#    converge_file pushes.
# ---------------------------------------------------------------------------
log_count="$(ct_exec "$CTID" -- sh -c 'ls /var/lib/fly/provision-log 2>/dev/null | wc -l' | tr -d ' \r')"
if [ "${log_count:-0}" -lt 1 ]; then
    skip "no second-run restarts: no provision-log entries yet, run provision.sh first"
elif [ "${log_count:-0}" -lt 2 ]; then
    skip "no second-run restarts: only one provision run recorded, run provision.sh again to test idempotency"
else
    latest="$(ct_exec "$CTID" -- sh -c 'ls /var/lib/fly/provision-log | sort | tail -1')"
    pushes="$(ct_exec "$CTID" -- sh -c "grep -c 'converge_file: pushed' /var/lib/fly/provision-log/$latest 2>/dev/null || true")"
    pushes="${pushes:-0}"
    if [ "$pushes" -eq 0 ]; then
        pass "no second-run restarts: latest run ($latest) converged nothing"
    else
        fail "no second-run restarts: latest run ($latest) made $pushes change(s) — provisioning is not idempotent"
    fi
fi

# ---------------------------------------------------------------------------
# 3. Pulse sink not SUSPENDED
# ---------------------------------------------------------------------------
# pct exec already runs as root in the container; runuser drops to fly
# for the pulse socket without needing a sudoers entry (unlike
# fly-watchdog, which runs AS fly and does need one — see
# config/fly-sudoers).
sink_state="$(ct_exec "$CTID" -- runuser -u fly -- env XDG_RUNTIME_DIR=/run/fly PULSE_RUNTIME_PATH=/run/fly/pulse \
    pactl list short sinks 2>/dev/null | awk '$2 == "stream" {print $NF}' || true)"
case "$sink_state" in
    RUNNING|IDLE)
        pass "pulse sink 'stream' is ${sink_state} (not SUSPENDED)"
        ;;
    SUSPENDED)
        fail "pulse sink 'stream' is SUSPENDED — check for an accidental module-suspend-on-idle load"
        ;;
    *)
        skip "pulse sink 'stream' state unknown (pulse.service may not be up yet): '${sink_state}'"
        ;;
esac

# ---------------------------------------------------------------------------
# 4. Clock skew < 1s (the host must be NTP-synced; a container cannot set its
#    own clock, so this really checks the host, from the host).
# ---------------------------------------------------------------------------
if command -v chronyc >/dev/null 2>&1; then
    skew="$(chronyc tracking 2>/dev/null | awk -F': ' '/System time/ {print $2}' | awk '{print $1}')"
    if [ -n "${skew:-}" ] && awk -v s="$skew" 'BEGIN{exit !(s < 1 && s > -1)}'; then
        pass "clock skew ${skew}s < 1s (chronyc)"
    else
        fail "clock skew ${skew:-unknown}s >= 1s or unreadable — VERIFY chrony on the host (docs/design/infra.md section 8)"
    fi
elif command -v timedatectl >/dev/null 2>&1; then
    synced="$(timedatectl show -p NTPSynchronized --value 2>/dev/null || true)"
    if [ "$synced" = yes ]; then
        pass "NTP synchronized (timedatectl); exact skew not available without chrony"
    else
        fail "NOT NTP synchronized (timedatectl) — VERIFY chrony/timesyncd on the host"
    fi
else
    skip "no chronyc or timedatectl found to check clock skew"
fi

# ---------------------------------------------------------------------------
# 5. Key prefix absent from journal and the repo
# ---------------------------------------------------------------------------
: "${SSH_TARGET:=root@<host-ip>}"
key_prefix=""
if [ "$PUSH_TARGET" = twitch ] && command -v pass >/dev/null 2>&1; then
    key_prefix="$(pass show "$PASS_KEY" 2>/dev/null | head -c8 || true)"
elif [ "$PUSH_TARGET" = twitch ]; then
    key_prefix="$(ssh "$SSH_TARGET" true 2>/dev/null && pass show "$PASS_KEY" 2>/dev/null | head -c8 || true)"
fi

if [ -z "$key_prefix" ]; then
    skip "key-prefix leak check: PUSH_TARGET=local or 'pass' unavailable here, nothing to check against"
else
    if ct_exec "$CTID" -- sh -c "journalctl --no-pager 2>/dev/null | grep -qF -- '$key_prefix'"; then
        fail "key prefix found in CT $CTID's journal — ROTATE THE KEY NOW (see docs/runbook.md)"
    else
        pass "key prefix not found in CT $CTID's journal"
    fi
    repo_root_dir="$(repo_root)"
    if grep -RqF -- "$key_prefix" "$repo_root_dir" --exclude-dir=.git 2>/dev/null; then
        fail "key prefix found in the flybrain repo — ROTATE THE KEY NOW (see docs/runbook.md)"
    else
        pass "key prefix not found in the flybrain repo"
    fi
fi

# ---------------------------------------------------------------------------
# 6. ZFS quotas — NOT set by any script in this repo (quota-setting is a
#    deliberate host-side-only, by-hand step; see infra/README.md's
#    "host-side steps" section and docs/runbook.md). This check therefore
#    does not run `zfs` on the host at all: it checks what the quota
#    LOOKS LIKE from inside the container, which is the only vantage
#    point available once the dataset itself is out of scope for
#    automation. A ZFS quota on a subvol makes `df` inside the container
#    report the quota as the mountpoint's total size, so a /srv/fly/media
#    total far above MEDIA_MP_GB means no quota is set (or it is set
#    much too large to matter) — the actual failure mode this check
#    exists to catch.
# ---------------------------------------------------------------------------
# The dataset name cannot be guessed from the mountpoint index: PVE numbers
# volumes per storage pool, so the bulk-array volume for a container whose
# rootfs and mp0 live on local-zfs is subvol-<ctid>-disk-0 on the bulk array,
# not -disk-1 (measured on the dev container, P0 spike run 2 — the old hint here named a
# dataset that does not exist). And PVE sets refquota, not quota, so
# `zfs get quota` answers "none" on a container that is correctly limited.
echo "--- to check or set the media quota by hand on the host (never scripted, see infra/README.md) ---"
echo "  pct config ${CTID} | grep '^mp1:'    # names the dataset; do not assume -disk-1"
echo "  zfs list -r -o name,refquota,quota,used <bulk-pool> | grep subvol-${CTID}"
echo "  zfs set refquota=${MEDIA_MP_GB}G recordsize=1M <bulk-pool>/subvol-${CTID}-disk-N"
echo "---"

media_total_bytes="$(ct_exec "$CTID" -- df --output=size -B1 /srv/fly/media 2>/dev/null | tail -n1 | tr -d ' ')"
expected_bytes=$(( MEDIA_MP_GB * 1000 * 1000 * 1000 ))
# Generous band: ZFS quota accounting and df's view of it are not exactly
# the requested GB (metadata overhead, GiB vs GB), so anything within 2x
# the requested size counts as "a quota that matches", and anything at or
# above the pool's own scale (multi-TiB) is clearly "no quota set".
if [ -n "${media_total_bytes:-}" ] && [ "$media_total_bytes" -gt 0 ] && [ "$media_total_bytes" -le $(( expected_bytes * 2 )) ]; then
    pass "/srv/fly/media total size as seen from inside the CT (${media_total_bytes} bytes) is consistent with a ~${MEDIA_MP_GB}G quota"
else
    fail "/srv/fly/media total size as seen from inside the CT is ${media_total_bytes:-unknown} bytes, not consistent with" \
         "the expected ~${MEDIA_MP_GB}G quota — this is the single most important safety measure in the plan" \
         "(docs/design/infra.md section 1); set it by hand with the command printed above"
fi

# ---------------------------------------------------------------------------
# 7. /dev/shm sizing (docs/design/infra.md section 1)
# ---------------------------------------------------------------------------
shm_size="$(ct_exec "$CTID" -- df --output=size -B1 /dev/shm 2>/dev/null | tail -n1 | tr -d ' ')"
if [ -n "${shm_size:-}" ] && [ "$shm_size" -ge 900000000 ]; then
    pass "/dev/shm sized at $(( shm_size / 1000000 ))MB (>=~1G)"
else
    fail "/dev/shm is only ${shm_size:-unknown} bytes, expected ~1G — check config/dev-shm-override.conf took effect"
fi

# ---------------------------------------------------------------------------
# 7b. The cpuset actually took. PVE writes its own cpuset too, and whether
#     the raw lxc.cgroup2.cpuset.cpus key wins on PVE 9.2 was UNVERIFIED at
#     design time — this is the check that settles it, per gpu.md section 1
#     ("check with taskset -pc 1 inside the CT").
#
#     Checked whenever CPUSET is set, NOT only when GPU=1 (2026-09-16, the release container
#     provisioning): it used to live inside the GPU section, so a CPU-only
#     container with a CPUSET — the release box — was never checked at all,
#     even though the measurement that justifies the pinning (P0 run 2
#     measurement 4) was taken on the x264 encoder. An unpinned or
#     PVE-overridden cpuset is silent: the container runs, the sim just
#     cannot hold real time.
# ---------------------------------------------------------------------------
expand_cpulist() {
    # "0-3,8" -> "0,1,2,3,8", sorted numerically. Stride syntax ("0-8:2") is
    # not expanded; a mismatch there is reported as a mismatch, with both
    # strings printed, rather than guessed at.
    printf '%s\n' "$1" | tr ',' '\n' | while IFS= read -r part; do
        case "$part" in
            '') ;;
            *-*) seq "${part%%-*}" "${part##*-}" 2>/dev/null || printf '%s\n' "$part" ;;
            *) printf '%s\n' "$part" ;;
        esac
    done | sort -n | tr '\n' ',' | sed 's/,$//'
}
if [ -z "${CPUSET:-}" ]; then
    skip "cpuset check: CPUSET is not set in $1"
else
    ct_affinity="$(ct_exec "$CTID" -- taskset -pc 1 2>/dev/null | sed 's/.*: *//' | tr -d ' \r' || true)"
    if [ -z "$ct_affinity" ]; then
        fail "could not read 'taskset -pc 1' inside CT $CTID"
    elif [ "$(expand_cpulist "$ct_affinity")" = "$(expand_cpulist "$CPUSET")" ]; then
        pass "pid 1 in CT $CTID is pinned to $ct_affinity == CPUSET"
    else
        fail "pid 1 in CT $CTID is pinned to '$ct_affinity', but CPUSET in $1 is '$CPUSET'." \
             "PVE's own automatic cpuset may be winning over the raw lxc.cgroup2.cpuset.cpus key;" \
             "check the fly-cpuset block in /etc/pve/lxc/${CTID}.conf and restart the container"
    fi
    # cpuset.mems too: the neuron sweep is memory-bandwidth-bound, so pages
    # from the other socket undo the point of pinning (01-create-ct.sh writes
    # both keys; only one of them is visible through taskset).
    conf_mems="$(awk '$1 == "lxc.cgroup2.cpuset.mems:" { print $2; exit }' "/etc/pve/lxc/${CTID}.conf" 2>/dev/null || true)"
    if [ -z "$conf_mems" ]; then
        fail "no lxc.cgroup2.cpuset.mems in /etc/pve/lxc/${CTID}.conf — CPUSET is pinned to one socket with memory unpinned; run 01-create-ct.sh"
    elif [ "$conf_mems" = "${CPUMEMS:-0}" ]; then
        pass "cpuset.mems in /etc/pve/lxc/${CTID}.conf is $conf_mems == CPUMEMS"
    else
        fail "cpuset.mems in /etc/pve/lxc/${CTID}.conf is '$conf_mems', but CPUMEMS in $1 is '${CPUMEMS:-0}'"
    fi
fi

# ---------------------------------------------------------------------------
# 8. GPU (docs/design/gpu.md section 8). Skipped cleanly when GPU=0.
#
# These checks exist because every failure mode in the GPU design is
# SILENT. A stale uvm major, a bind whose source did not exist at container
# start, a host driver that moved out from under the guest userspace, a
# cpuset that PVE quietly overrode — none of them announce themselves, and
# three of the four leave a container that boots, runs, and encodes on the
# x264 fallback at twice the CPU while looking perfectly healthy.
# ---------------------------------------------------------------------------
if [ "${GPU:-0}" != 1 ]; then
    skip "GPU checks: GPU is not 1 in $1 (nothing to check)"
else
    echo "--- GPU (docs/design/gpu.md) ---"

    # 8a. Every /dev/nvidia* inside the CT is a CHARACTER device.
    #
    # THE check. `bind,optional,create=file` turns a bind whose source did
    # not exist at container start into an empty REGULAR FILE, which is the
    # exact tell the operator's own GPU-driver notes record. Everything
    # else downstream — nvidia-smi, NVENC — then fails in a way that looks
    # like a driver problem and is not.
    nv_nodes="$(ct_exec "$CTID" -- sh -c 'ls -1 /dev/nvidia* 2>/dev/null' | tr -d '\r' || true)"
    if [ -z "$nv_nodes" ]; then
        fail "no /dev/nvidia* nodes inside CT $CTID at all — the passthrough block never applied. Check: grep -A10 'BEGIN fly-nvidia' /etc/pve/lxc/${CTID}.conf, then restart the container (lxc.* keys are read only at start)"
    else
        nv_bad="$(ct_exec "$CTID" -- sh -c 'for f in /dev/nvidia*; do [ -c "$f" ] || [ -d "$f" ] || echo "$f"; done' | tr -d '\r' || true)"
        if [ -z "$nv_bad" ]; then
            pass "every /dev/nvidia* inside CT $CTID is a character device or directory ($(echo "$nv_nodes" | tr '\n' ' '))"
        else
            fail "these /dev/nvidia* entries inside CT $CTID are NOT character devices (an empty regular file means the bind's source was missing at container start — passthrough is broken): $(echo "$nv_bad" | tr '\n' ' ')"
        fi
    fi

    # 8b + 8c. nvidia-smi works, reports $NVIDIA_VERSION, and agrees with
    # The host. The lockstep check is the one that catches a host driver or
    # kernel upgrade before it catches the stream (docs/runbook.md "GPU
    # driver version lockstep").
    ct_driver="$(ct_exec "$CTID" -- sh -c 'nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1' 2>/dev/null | tr -d ' \r' || true)"
    if [ -z "$ct_driver" ]; then
        fail "nvidia-smi inside CT $CTID produced no version (check: pct exec $CTID -- nvidia-smi; 'Failed to initialize NVML: Driver/library version mismatch' means the host module moved and 02-base.sh must be re-run)"
    elif [ "$ct_driver" = "${NVIDIA_VERSION:-}" ]; then
        pass "nvidia-smi in CT $CTID reports driver $ct_driver == NVIDIA_VERSION"
    else
        fail "nvidia-smi in CT $CTID reports driver '$ct_driver', but NVIDIA_VERSION in $1 is '${NVIDIA_VERSION:-unset}'"
    fi

    host_driver=""
    if command -v nvidia-smi >/dev/null 2>&1; then
        host_driver="$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1 | tr -d ' \r' || true)"
    fi
    if [ -z "$host_driver" ]; then
        skip "driver lockstep: the host's own nvidia-smi produced no version (is this really the host?)"
    elif [ -z "$ct_driver" ]; then
        skip "driver lockstep: no container-side version to compare against"
    elif [ "$host_driver" = "$ct_driver" ]; then
        pass "driver lockstep: host $host_driver == CT $CTID $ct_driver"
    else
        fail "driver lockstep BROKEN: host nvidia-smi says $host_driver, CT $CTID says $ct_driver." \
             "The guest's libcuda/libnvidia-encode will refuse to talk to a mismatched kernel module and flycast will" \
             "run on the x264 fallback at ~2 cores. Re-run 02-base.sh for this CT (docs/runbook.md 'GPU driver version lockstep')"
    fi

    # 8d. The majors written into the conf still match /proc/devices. This
    # is the dynamic-uvm-major check, and the reason
    # fly-nvidia-majors.service exists at all.
    ct_conf="/etc/pve/lxc/${CTID}.conf"
    if [ ! -f "$ct_conf" ]; then
        fail "no $ct_conf to check majors against"
    else
        majors_ok=1
        for devname in nvidia nvidia-uvm nvidia-caps; do
            live="$(awk -v n="$devname" '$2 == n { print $1; exit }' /proc/devices || true)"
            if [ -z "$live" ]; then
                fail "major for '$devname' is not in /proc/devices on the host — the driver is not loaded"
                majors_ok=0
                continue
            fi
            if grep -qE "^lxc\.cgroup2\.devices\.allow:[[:space:]]*c[[:space:]]+${live}:\*[[:space:]]+rwm[[:space:]]*$" "$ct_conf"; then
                continue
            fi
            fail "$ct_conf has no 'lxc.cgroup2.devices.allow: c ${live}:* rwm' line for '$devname' (live major ${live})." \
                 "Run /usr/local/sbin/fly-nvidia-majors.sh $CTID and restart the container"
            majors_ok=0
        done
        [ "$majors_ok" -eq 1 ] && pass "all three NVIDIA majors in $ct_conf match /proc/devices right now"
    fi

    # 8e. The host unit that keeps the majors fresh across reboots. This
    # runs from the host, which verify.sh always does (require_pve_host).
    if ! command -v systemctl >/dev/null 2>&1; then
        skip "fly-nvidia-majors.service: no systemctl here"
    else
        if systemctl is-enabled --quiet fly-nvidia-majors.service 2>/dev/null; then
            pass "host unit enabled: fly-nvidia-majors.service"
        else
            fail "host unit NOT enabled: fly-nvidia-majors.service — the dynamic uvm/caps majors will go stale at the next reboot, SILENTLY. Install it per infra/host/README.md"
        fi
        majors_state="$(systemctl is-active fly-nvidia-majors.service 2>/dev/null || true)"
        if [ "$majors_state" = active ]; then
            pass "host unit active (oneshot, RemainAfterExit): fly-nvidia-majors.service"
        else
            fail "host unit fly-nvidia-majors.service is '$majors_state', expected 'active' (active (exited) for a RemainAfterExit oneshot). Check: systemctl status fly-nvidia-majors.service"
        fi
    fi

    # 8f. FLY_ENCODER matches the backend actually in use, i.e. the
    # fallback has not silently fired.
    enc_want="${FLY_ENCODER:-}"
    enc_live="$(ct_exec "$CTID" -- sh -c \
        "awk '/^fly_encoder_backend\\{/ && \$NF == 1 { if (match(\$0, /backend=\"[^\"]+\"/)) { print substr(\$0, RSTART + 9, RLENGTH - 10); exit } }' /var/lib/node_exporter/textfile/fly_encoder.prom 2>/dev/null" \
        2>/dev/null | tr -d ' \r' || true)"
    if [ -z "$enc_want" ]; then
        skip "encoder backend: FLY_ENCODER is not set in $1"
    elif [ -z "$enc_live" ]; then
        skip "encoder backend: no fly_encoder_backend metric in CT $CTID yet (flycast has not started since bin/flycast-launch was deployed)"
    elif [ "$enc_live" = "$enc_want" ]; then
        pass "encoder backend in use ($enc_live) matches FLY_ENCODER"
    else
        fail "encoder backend in use is '$enc_live' but FLY_ENCODER in $1 is '$enc_want' — the automatic fallback fired." \
             "The stream is up but costing ~2 cores. Usual causes: VRAM pressure from the GPU workload in the neighbouring container, or a driver lockstep break above." \
             "Check: pct exec $CTID -- journalctl -u flycast -n 50, and nvidia-smi --query-gpu=memory.used,utilization.encoder --format=csv"
    fi

    # 8g. VirtualGL, only when CHROMIUM_PROFILE=vgl asked for it
    # (docs/design/gpu.md section 3 (d), infra/docs/virtualgl-spike.md).
    #
    # Two checks, and between them they catch the whole silent-failure surface
    # this configuration has: no vglrun means flystage exits 3 and there is no
    # page at all, and an egl0 that is not NVIDIA means the fly renders on
    # llvmpipe — a page that looks right and costs several cores. Whether
    # Chromium really got the hardware context is a CDP question and stays out
    # of here; infra/docs/virtualgl-spike.md has the one-liner for it.
    if [ "${CHROMIUM_PROFILE:-default}" != vgl ]; then
        skip "VirtualGL checks: CHROMIUM_PROFILE is not 'vgl' in $1"
    else
        if ct_exec "$CTID" -- test -x /usr/bin/vglrun; then
            # `dpkg-query -W virtualgl | cut -f2`, NOT -f='${Version}': `pct
            # exec` interposes a shell of its own, so a `${...}` inside single
            # quotes still gets expanded — to nothing — before dpkg-query sees
            # it, and dpkg-query then fails on an empty format string with the
            # error sent to /dev/null. Measured: the same line read empty here
            # while 02-base.sh's escaped `\${Version}` spelling read 3.1.5.
            vgl_ver="$(ct_exec "$CTID" -- sh -c 'dpkg-query -W virtualgl 2>/dev/null | cut -f2' 2>/dev/null | tr -d ' \r' || true)"
            pass "VirtualGL installed in CT $CTID (${vgl_ver:-version unknown}), CHROMIUM_PROFILE=vgl can start"
        else
            fail "CHROMIUM_PROFILE=vgl but /usr/bin/vglrun is missing in CT $CTID — bin/flystage-launch exits 3 and there is NO page on the stream." \
                 "Set VIRTUALGL_VERSION and VIRTUALGL_DEB_SHA256 in $1 and re-run infra/02-base.sh $1"
        fi

        vgl_dev="$(ct_exec "$CTID" -- sh -c \
            "/opt/VirtualGL/bin/eglinfo ${VGL_DEVICE:-egl0} -B 2>/dev/null | awk -F': ' '/^OpenGL renderer string/{print \$2; exit}'" \
            2>/dev/null | tr -d '\r' || true)"
        case "$vgl_dev" in
            *NVIDIA*|*Quadro*)
                pass "VirtualGL EGL device ${VGL_DEVICE:-egl0} is the GPU: $vgl_dev" ;;
            "")
                fail "VirtualGL cannot read EGL device ${VGL_DEVICE:-egl0} in CT $CTID at all. The kiosk will have no GL." \
                     "Check: pct exec $CTID -- /opt/VirtualGL/bin/eglinfo -e" ;;
            *)
                fail "VirtualGL EGL device ${VGL_DEVICE:-egl0} in CT $CTID renders on '$vgl_dev', NOT the Quadro — the fly would run on software GL at a cost of several cores, on a page that looks correct." \
                     "The devices renumbered. Check: pct exec $CTID -- /opt/VirtualGL/bin/eglinfo -e, then set VGL_DEVICE in $1 to the NVIDIA one" ;;
        esac
    fi
fi

echo "=== verify.sh: $([ "$FAILED" -eq 0 ] && echo ALL CHECKS PASSED || echo FAILURES ABOVE) ==="
exit "$FAILED"
