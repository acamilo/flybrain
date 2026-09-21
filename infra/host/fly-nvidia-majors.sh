#!/usr/bin/env bash
# infra/host/fly-nvidia-majors.sh — installed BY HAND on the host as
# /usr/local/sbin/fly-nvidia-majors.sh, run by fly-nvidia-majors.service
# before pve-guests.service on every boot. See infra/host/README.md for the
# install steps and docs/design/gpu.md section 1 for why it exists.
#
#   fly-nvidia-majors.sh [--dry-run] CTID [CTID...]
#
# What it does, per docs/design/gpu.md section 1:
#   1. `nvidia-modprobe -u -c 0 -m` so /dev/nvidia-uvm{,-tools} and
#      /dev/nvidia-caps/* actually exist (loading the modules is what
#      allocates their majors in the first place).
#   2. Reads the three majors it needs out of /proc/devices, live:
#        nvidia      -> 195, compile-time static (NV_MAJOR_DEVICE_NUMBER),
#                       covers nvidia0 / nvidiactl / nvidia-modeset
#        nvidia-uvm  -> DYNAMIC, changes every boot (509 -> 511 on the LLM host on
#                       2026-08-29; 511 on the host on 2026-09-15)
#        nvidia-caps -> DYNAMIC (236 on the host on 2026-09-15)
#   3. Rewrites the `# BEGIN fly-nvidia` .. `# END fly-nvidia` block in
#      /etc/pve/lxc/<id>.conf for each id given, and writes the file back
#      ONLY if the result differs.
#
# Three deliberate properties, each of which is a bug that has actually
# happened on the operator's hosts:
#
#   * It REFUSES to write anything if any major came back empty. A truncated
#     or malformed /proc/devices read must not turn into
#     `lxc.cgroup2.devices.allow: c :* rwm` in a production container's conf.
#   * It builds the new file in $TMPDIR (default /tmp) and then `cat >`s it
#     into place. /etc/pve is pmxcfs (a FUSE filesystem backed by the cluster
#     config DB); `mv` across filesystems into it is not reliable, and every
#     write is a cluster transaction, so needless writes are avoided too.
#   * It is idempotent, so it is safe on a timer, from a unit, or by hand
#     while debugging, and a second run reports `unchanged`.
#
# Output contract (this is consumed by infra/01-create-ct.sh): exactly one
# line per CTID on stdout, `<ctid> changed` or `<ctid> unchanged` (or
# `<ctid> missing` when there is no conf file for it). All logging goes to
# stderr.
#
# This script is HOST-SIDE. It is the one file in this repo that runs
# outside a container, does not take an env file, and is installed by hand
# rather than by provision.sh — /etc/pve is not reachable through pct exec,
# and a boot-ordered host unit is not something a per-container script
# should be installing behind the operator's back.
set -euo pipefail

BLOCK_BEGIN='# BEGIN fly-nvidia'
BLOCK_END='# END fly-nvidia'
DRY_RUN=0

# Two test hooks, both defaulting to the only values that matter in
# production. They exist so the sentinel-block rewrite can be exercised
# without a GPU and without touching /etc/pve (infra/tests/lint.sh does
# exactly that), and for `--dry-run` against a copied conf while debugging.
# Nothing in the repo sets them for a real run.
: "${PROC_DEVICES:=/proc/devices}"
: "${LXC_CONF_DIR:=/etc/pve/lxc}"

log() { printf 'fly-nvidia-majors: %s\n' "$*" >&2; }
die() { printf 'fly-nvidia-majors: FATAL: %s\n' "$*" >&2; exit 1; }

usage() {
    echo "usage: $0 [--dry-run] CTID [CTID...]" >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage ;;
        --) shift; break ;;
        -*) die "unknown option: $1" ;;
        *) break ;;
    esac
done

[ $# -ge 1 ] || usage

for id in "$@"; do
    case "$id" in
        ''|*[!0-9]*) die "not a container id: '$id'" ;;
    esac
done

# ---------------------------------------------------------------------------
# 1. Materialise the device nodes. Harmless and idempotent when they already
#    exist; `-u` is uvm, `-c 0` is the caps directory for GPU 0, `-m` is
#    nvidia-modeset. Matches the host's existing nvidia-devnodes.service, which
#    does exactly this and nothing else (which is why the neighbouring GPU container's majors went
#    stale: nothing rewrote them).
# ---------------------------------------------------------------------------
if command -v nvidia-modprobe >/dev/null 2>&1; then
    nvidia-modprobe -u -c 0 -m || log "WARNING: nvidia-modprobe -u -c 0 -m exited nonzero; continuing to read /proc/devices anyway"
else
    log "WARNING: nvidia-modprobe not found; relying on the modules already being loaded"
fi

# ---------------------------------------------------------------------------
# 2. Read the majors, live. `$2 == name` (not a regex match) so that
#    `nvidia` does not also match `nvidia-uvm`, `nvidia-modeset`,
#    `nvidia-nvswitch` or `nvidia-nvlink`, all of which appear in
#    /proc/devices on this host.
# ---------------------------------------------------------------------------
read_major() {
    awk -v n="$1" '$2 == n { print $1; exit }' "$PROC_DEVICES"
}

MAJ_NVIDIA="$(read_major nvidia || true)"
MAJ_UVM="$(read_major nvidia-uvm || true)"
MAJ_CAPS="$(read_major nvidia-caps || true)"

for pair in "nvidia:${MAJ_NVIDIA}" "nvidia-uvm:${MAJ_UVM}" "nvidia-caps:${MAJ_CAPS}"; do
    name="${pair%%:*}"
    val="${pair#*:}"
    case "$val" in
        ''|*[!0-9]*)
            die "refusing to write: major for '$name' read from /proc/devices is '${val}', not a number. Is the driver loaded? Check: grep nvidia $PROC_DEVICES"
            ;;
    esac
done

log "majors: nvidia=${MAJ_NVIDIA} nvidia-uvm=${MAJ_UVM} nvidia-caps=${MAJ_CAPS}"
if [ "$MAJ_NVIDIA" != 195 ]; then
    log "WARNING: nvidia major is ${MAJ_NVIDIA}, not the expected compile-time-static 195 — worth a look, but writing it as read"
fi

# ---------------------------------------------------------------------------
# 3. The block itself (docs/design/gpu.md section 1).
#
# /dev/nvidia-modeset is deliberately NOT bound: the NVENC-only design has
# no in-container Xorg and no EGL display, and binding it hands the guest
# modesetting ioctls for nothing. If gpu.md section 3 option (b) or (d) is
# ever taken, add:
#   lxc.mount.entry: /dev/nvidia-modeset dev/nvidia-modeset none bind,optional,create=file
#
# nvidia-caps is bound as a DIRECTORY (create=dir), which is what the neighbouring GPU container does
# and what is proven to work on this host.
#
# `optional` is load-bearing in both directions: without it a missing node
# blocks container start, so a post-kernel-update driver failure takes the
# whole stream down instead of degrading to the x264 path — but WITH it a
# broken bind is silent, and `optional,create=file` materialises an empty
# REGULAR FILE where a character device should be. That is the exact tell the
# operator's own GPU-driver notes record, and it is what infra/verify.sh's
# char-device check exists to catch.
# ---------------------------------------------------------------------------
render_block() {
    cat <<EOF
lxc.cgroup2.devices.allow: c ${MAJ_NVIDIA}:* rwm
lxc.cgroup2.devices.allow: c ${MAJ_UVM}:* rwm
lxc.cgroup2.devices.allow: c ${MAJ_CAPS}:* rwm
lxc.mount.entry: /dev/nvidia0 dev/nvidia0 none bind,optional,create=file
lxc.mount.entry: /dev/nvidiactl dev/nvidiactl none bind,optional,create=file
lxc.mount.entry: /dev/nvidia-uvm dev/nvidia-uvm none bind,optional,create=file
lxc.mount.entry: /dev/nvidia-uvm-tools dev/nvidia-uvm-tools none bind,optional,create=file
lxc.mount.entry: /dev/nvidia-caps dev/nvidia-caps none bind,optional,create=dir
EOF
}

# ---------------------------------------------------------------------------
# 4. Converge one conf file.
#
# Ownership is by LINE, not by position, and that is load-bearing. PVE
# REWRITES /etc/pve/lxc/<id>.conf on every lifecycle operation and does not
# preserve layout: measured on PVE 9.2.10 (the host, 2026-09-16), one
# `pct stop` / `pct start` of the dev container re-emitted the conf with every PVE key
# sorted alphabetically, every COMMENT hoisted to the top of the file, and
# every raw `lxc.*` key moved to the end. The `# BEGIN fly-nvidia` /
# `# END fly-nvidia` pair ends up adjacent and EMPTY at the top, with the
# eight lines it used to bracket sitting at the bottom outside any block.
#
# A converge that trusted those comments would then, on the next boot:
# delete the empty block, append a fresh one, and leave the hoisted-out
# lines in place as duplicates. Harmless on the day it happens, and a silent
# breaker the day after a driver change, because those duplicates carry the
# OLD dynamic majors and nothing ever removes them — which is precisely the
# failure this unit exists to prevent (it is how the neighbouring GPU container came to allow
# `c 509:*`).
#
# So a line is owned if it is one of the lines we generate, or if it is:
#
#   * `lxc.mount.entry: /dev/<one of OUR five targets> ...` — so a bind with
#     different options is replaced rather than duplicated. /dev/nvidia-modeset
#     is deliberately NOT in that list: the neighbouring GPU container binds it, we do not, and
#     deleting a device out of a production container's config is not this
#     script's job.
#   * `lxc.cgroup2.devices.allow: c N:* rwm` where N is one of the three
#     majors we are writing, or N is registered to a device whose name starts
#     with `nvidia` (e.g. `234`, nvidia-nvswitch, which nothing here wants),
#     or N is registered to NOTHING and sits in a kernel dynamic char-major
#     range (234-254, 384-511) — the signature of one of our own lines from an
#     earlier driver load, which is what the neighbouring GPU container's `c 509:*` is. A major that
#     resolves to a non-NVIDIA device (`226` for /dev/dri), or a static major
#     whose module simply is not loaded, is NOT ours and is left alone.
#
# Anything else is reported as a stray and never touched.
#
# "unchanged" therefore means: every desired line appears exactly once
# somewhere in the file and no owned-but-undesired line appears at all.
# Comments and position are not compared, because PVE owns them and will
# move them again on the next start. The sentinels are still written, as the
# only in-file signpost for a human; after the next `pct start` they will be
# at the top with nothing between them, and that is expected, not a fault.
# ---------------------------------------------------------------------------

# owned_majors CONF — the majors in CONF's `devices.allow: c N:* rwm` lines
# that this script considers its own, per the rules above. One per line.
owned_majors() {
    local conf="$1" maj name
    sed -n 's|^lxc\.cgroup2\.devices\.allow: c \([0-9]\{1,\}\):\* rwm$|\1|p' "$conf" \
        | sort -u \
        | while read -r maj; do
            [ -n "$maj" ] || continue
            case " ${MAJ_NVIDIA} ${MAJ_UVM} ${MAJ_CAPS} " in
                *" ${maj} "*) echo "$maj"; continue ;;
            esac
            name="$(awk -v m="$maj" '$1 == m { print $2; exit }' "$PROC_DEVICES")"
            if [ -n "$name" ]; then
                case "$name" in nvidia*) echo "$maj" ;; esac
            elif { [ "$maj" -ge 234 ] && [ "$maj" -le 254 ]; } \
              || { [ "$maj" -ge 384 ] && [ "$maj" -le 511 ]; }; then
                # Registered to nothing AND inside a kernel DYNAMIC char-major
                # range (234-254, then the extended 384-511). Only a
                # dynamically allocated major can go stale the way nvidia-uvm
                # and nvidia-caps do, so this is the signature of one of our
                # own lines from an earlier driver load — the neighbouring GPU container's `c 509:*` is
                # exactly this. A STATIC major that simply is not loaded right
                # now (226/drm on a host with no DRM, 116/alsa, ...) is not
                # ours and must survive: deleting an `allow` line for another
                # container's device because its module happened to be
                # unloaded would be a real regression, and infra/tests/lint.sh
                # asserts both halves of this.
                echo "$maj"
            fi
        done
}

converge_conf() {
    local ctid="$1"
    local conf="${LXC_CONF_DIR}/${ctid}.conf"

    if [ ! -f "$conf" ]; then
        log "no such container conf: $conf (CT ${ctid} not created yet?) — skipping"
        echo "${ctid} missing"
        return 0
    fi

    local blk new majs
    blk="$(mktemp "${TMPDIR:-/tmp}/fly-nvidia-blk.XXXXXX")"
    new="$(mktemp "${TMPDIR:-/tmp}/fly-nvidia-conf.XXXXXX")"

    {
        echo "$BLOCK_BEGIN (generated by fly-nvidia-majors.service, do not edit by hand)"
        render_block
        echo "$BLOCK_END"
    } > "$blk"

    majs="$(owned_majors "$conf" | tr '\n' ' ')"

    # The awk ownership test, shared by both passes below. Kept in one string
    # so the "is it already right?" check and the rewrite can never disagree.
    local own_prog='
        function is_owned(line,   n, i, t, ts, maj) {
            if (line in desired) return 1
            n = split("/dev/nvidia0 /dev/nvidiactl /dev/nvidia-uvm /dev/nvidia-uvm-tools /dev/nvidia-caps", ts, " ")
            for (i = 1; i <= n; i++) {
                t = "lxc.mount.entry: " ts[i] " "
                if (index(line, t) == 1) return 1
            }
            if (match(line, /^lxc\.cgroup2\.devices\.allow: c [0-9]+:\* rwm$/)) {
                maj = line
                sub(/^lxc\.cgroup2\.devices\.allow: c /, "", maj)
                sub(/:\* rwm$/, "", maj)
                if ((" " majs " ") ~ (" " maj " ")) return 1
            }
            return 0
        }
        BEGIN {
            while ((getline line < desiredfile) > 0) if (line != "") desired[line] = 1
            close(desiredfile)
        }
    '

    # Pass 1: already semantically correct? Then write nothing at all.
    if awk -v majs="$majs" -v desiredfile="$blk" "
        $own_prog
        { if (is_owned(\$0)) seen[\$0]++ }
        END {
            for (d in desired) if (d !~ /^# (BEGIN|END) /) if (seen[d] != 1) exit 1
            for (s in seen)    if (!(s in desired)) exit 1
            exit 0
        }
    " "$conf"; then
        report_strays "$conf" "$majs"
        rm -f "$blk" "$new"
        echo "${ctid} unchanged"
        return 0
    fi

    # Pass 2: rewrite. Drop every owned line and both sentinels wherever they
    # are, then re-emit the block before the first `[section]` or at EOF —
    # appending blindly would land lxc.* keys inside a snapshot section, where
    # PVE ignores them, silently.
    awk -v majs="$majs" -v desiredfile="$blk" -v b="$BLOCK_BEGIN" -v e="$BLOCK_END" -v blockfile="$blk" "
        $own_prog
        function emit(  line) {
            while ((getline line < blockfile) > 0) print line
            close(blockfile)
        }
        index(\$0, b) == 1 { next }
        index(\$0, e) == 1 { next }
        is_owned(\$0)      { next }
        /^\[/ && !done     { emit(); done = 1 }
        { print }
        END { if (!done) emit() }
    " "$conf" > "$new"

    report_strays "$conf" "$majs"

    if cmp -s "$new" "$conf"; then
        rm -f "$blk" "$new"
        echo "${ctid} unchanged"
        return 0
    fi

    if [ "$DRY_RUN" -eq 1 ]; then
        log "DRY RUN: would rewrite $conf; diff follows"
        diff -u "$conf" "$new" >&2 || true
    else
        # pmxcfs: write THROUGH the existing inode with `cat >`, never `mv` a
        # /tmp file across the filesystem boundary into /etc/pve.
        cat "$new" > "$conf"
        log "rewrote the fly-nvidia block in $conf"
    fi

    rm -f "$blk" "$new"
    echo "${ctid} changed"
}

# report_strays CONF OWNED_MAJORS — NVIDIA-shaped device lines this script
# does NOT own are reported, never silently deleted. The neighbouring GPU container's
# /dev/nvidia-modeset bind is the live example: it belongs to the LLM work,
# not to this unit.
report_strays() {
    local conf="$1" majs="$2" stray
    stray="$(awk -v majs="$majs" '
        function ours(line,   n, i, t, ts, maj) {
            n = split("/dev/nvidia0 /dev/nvidiactl /dev/nvidia-uvm /dev/nvidia-uvm-tools /dev/nvidia-caps", ts, " ")
            for (i = 1; i <= n; i++) {
                t = "lxc.mount.entry: " ts[i] " "
                if (index(line, t) == 1) return 1
            }
            if (match(line, /^lxc\.cgroup2\.devices\.allow: c [0-9]+:\* rwm$/)) {
                maj = line
                sub(/^lxc\.cgroup2\.devices\.allow: c /, "", maj)
                sub(/:\* rwm$/, "", maj)
                if ((" " majs " ") ~ (" " maj " ")) return 1
            }
            return 0
        }
        /^lxc\.mount\.entry:[[:space:]]*\/dev\/nvidia/ { if (!ours($0)) print; next }
        /^lxc\.cgroup2\.devices\.allow:/              { if (!ours($0)) print; next }
    ' "$conf" || true)"
    if [ -n "$stray" ]; then
        log "NOTE: $conf has NVIDIA/device lines this script does not own; they are left alone. Review and delete by hand if stale:"
        printf '%s\n' "$stray" | sed 's/^/    /' >&2
    fi
}

for id in "$@"; do
    converge_conf "$id"
done

log "done ($#: $*)"
