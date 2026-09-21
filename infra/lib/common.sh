#!/usr/bin/env bash
# infra/lib/common.sh — shared helpers for the flybrain infra scripts.
#
# Sourced, never executed directly:
#   # shellcheck source=lib/common.sh
#   . "$(dirname "${BASH_SOURCE[0]}")/lib/common.sh"
#
# Every script that sources this must itself have `set -euo pipefail` at the
# top; this file assumes that is already in effect and does not set it again
# (sourcing scripts get to decide, but all of ours do).

# ---------------------------------------------------------------------------
# logging
# ---------------------------------------------------------------------------

log() {
    printf '[%s] %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" >&2
}

die() {
    printf '[%s] FATAL: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" >&2
    exit 1
}

# ---------------------------------------------------------------------------
# preconditions
# ---------------------------------------------------------------------------

# need CMD [CMD...] — die if any of the named commands is not on PATH.
need() {
    local cmd missing=()
    for cmd in "$@"; do
        command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
    done
    if [ "${#missing[@]}" -gt 0 ]; then
        die "missing required command(s): ${missing[*]}"
    fi
}

# need_var NAME [NAME...] — die if any of the named shell variables is unset
# or empty. Use after sourcing an env file.
need_var() {
    local name
    for name in "$@"; do
        if [ -z "${!name:-}" ]; then
            die "required variable '$name' is not set (check the env file)"
        fi
    done
}

# load_env FILE — source an env file and validate the common fields
# every env file must carry. Individual step scripts may call need_var for
# additional fields they specifically need (e.g. TWITCH_CHANNEL).
#
# ROLE defaults to "dev" when an env file does not set it, rather than
# being required, so env files written before the dev/release split keep
# behaving exactly as they always did. Only "dev" and "release" are valid —
# see infra/env/example.env's own comments and infra/docs/runbook.md's
# "Cutting a release" section for what each one changes (05-deploy.sh's
# tag/clean gate, 01-create-ct.sh's --onboot).
# The real env files are NOT in this repo (infra/env/README.md): this repo is
# public and a real env file names the host, the container id, the LAN address,
# the channel and the `pass` entry. `infra/env/example.env` is the template.
# So the argument is a PATH, anywhere on the filesystem, and nothing here
# resolves it against infra/env/. FLY_ENV_DIR is a typing convenience: when the
# argument is not itself a readable file, it is tried as a bare name under
# $FLY_ENV_DIR (e.g. FLY_ENV_DIR=/etc/fly/env infra/verify.sh <release-env>).
load_env() {
    local envfile="$1" tried="$1"
    if [ ! -f "$envfile" ] && [ -n "${FLY_ENV_DIR:-}" ]; then
        tried="$envfile and ${FLY_ENV_DIR%/}/$(basename "$envfile")"
        envfile="${FLY_ENV_DIR%/}/$(basename "$envfile")"
    fi
    [ -f "$envfile" ] || die "env file not found: $tried — the real env files live outside this repo, see infra/env/README.md"
    # shellcheck disable=SC1090
    . "$envfile"
    need_var CTID HOSTNAME IP GAME PUSH_TARGET
    ROLE="${ROLE:-dev}"
    case "$ROLE" in
        dev|release) ;;
        *) die "ROLE must be 'dev' or 'release', got '$ROLE' (check $envfile)" ;;
    esac
}

# require_release_tag REPO_DIR — for ROLE=release deploys: assert that the
# git worktree at REPO_DIR is exactly at a clean annotated tag matching
# ^v[0-9]+\.[0-9]+\.[0-9]+$ (docs/stream-mvp-plan.md, "Release container",
# The operator 2026-09-16: "the release container runs TAGGED commits only"). Prints
# the tag on stdout and returns 0 on success; dies with the refusal message
# on failure. Kept here, rather than inlined in 05-deploy.sh, so
# infra/tests/lint.sh can exercise it directly against a throwaway git repo
# without needing the host, pct, or a real CTID (the same reason
# host/fly-nvidia-majors.sh's rewrite logic is tested by calling the script
# directly instead of through provision.sh).
require_release_tag() {
    local repo_dir="$1" tag git_err
    need git
    # Distinguish "git cannot use this directory at all" from "this commit is
    # not tagged", because the refusal below swallows git's stderr and both
    # would otherwise be reported as an untagged tree.
    #
    # The case that actually happened, on the release container's first tagged deploy
    # (v0.1.0, 2026-09-16): a release deploy runs from a checkout staged on
    # The host, and unpacking one there as root leaves it owned by the uid from
    # the archive, so every git command fails with `fatal: detected dubious
    # ownership in repository at ...` (safe.directory). The operator then gets
    # told the tree "is not exactly at an annotated tag" — which is false, and
    # sends them looking for a tagging problem instead of running one chown.
    # Surface git's own message and say what to do about it.
    if ! git_err="$(git -C "$repo_dir" rev-parse --git-dir 2>&1 >/dev/null)"; then
        die "05-deploy: ROLE=release refuses to deploy: git cannot read a repository at $repo_dir — git said: ${git_err}. This is NOT a tagging problem. If it is a dubious-ownership refusal, the checkout is owned by another uid (unpacking a source tarball as root on the host does this): 'chown -R root:root $repo_dir' and re-run."
    fi
    if ! tag="$(git -C "$repo_dir" describe --exact-match --tags 2>/dev/null)"; then
        die "05-deploy: ROLE=release refuses to deploy: source tree at $repo_dir is not exactly at an annotated tag (git describe --exact-match --tags failed)"
    fi
    if ! [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        die "05-deploy: ROLE=release refuses to deploy: tag '$tag' at $repo_dir does not match ^v[0-9]+\.[0-9]+\.[0-9]+\$"
    fi
    if [ -n "$(git -C "$repo_dir" status --porcelain)" ]; then
        die "05-deploy: ROLE=release refuses to deploy: source tree at $repo_dir is not clean (git status --porcelain is non-empty)"
    fi
    echo "$tag"
}

# cpuset_partition CPUSET RAYON_THREADS [ENCODER_CORES] — split a
# comma-separated cpuset (whole physical cores, `01-create-ct.sh`'s
# `lxc.cgroup2.cpuset.cpus`) into THREE groups:
#
#   1. the FIRST RAYON_THREADS cpus, for flysim;
#   2. the LAST ENCODER_CORES cpus of what remains (default 2), for flycast
#      alone;
#   3. whatever is left in between, for the page/capture side (xvfb,
#      flystage, flystage-web, pulse, mediamtx).
#
# flycast gets its own group rather than sharing with the page group
# because the release container measured Chromium's compositor starving when it shared
# cores with the x264 encoder: 63% of captured frames came back unchanged
# across two watchdog passes, against 2% on the NVENC dev box, where the
# encoder is off-CPU entirely. See infra/README.md step 4 and
# docs/design/infra.md section 2.
#
# Echoes "SIM_CPUS PAGE_CPUS ENCODER_CPUS" on stdout, space-separated (each
# itself a comma-joined cpu list); dies if RAYON_THREADS leaves nothing
# over, or if what's left cannot cover ENCODER_CORES plus at least one page
# cpu. Kept here, not inlined in 05-deploy.sh, so infra/tests/lint.sh can
# exercise the split directly (same reason as require_release_tag above).
#
# 2026-09-16 live hotfix: CPUSET=1,3,5,7,9,11,13,15,17,19 (all ten of node
# 1's whole physical cores — "extra cores", widened same day from eight
# once eight was measured unable to hold x264 + page + sim at once),
# RAYON_THREADS=4, ENCODER_CORES=3 gives flysim=1,3,5,7, page=9,11,13,
# flycast=15,17,19 — the split the operator set on the release container by hand before this
# function existed. See infra/docs/runbook.md "CPU partition (cpuset)".
# (An earlier, eight-cpu version of this same live hotfix — CPUSET=
# 1,3,5,7,9,11,13,15, ENCODER_CORES=2 (the default) — gave flysim=1,3,5,7,
# page=9,11, flycast=13,15; infra/tests/lint.sh checks both shapes.)
cpuset_partition() {
    local cpuset="$1" rayon_threads="$2" encoder_cores="${3:-2}"
    local sim_cpus remainder remainder_count page_count page_cpus encoder_cpus

    sim_cpus="$(echo "$cpuset" | cut -d, -f1-"$rayon_threads")"
    remainder="$(echo "$cpuset" | cut -d, -f"$(( rayon_threads + 1 ))"-)"
    if [ -z "$remainder" ] || [ "$remainder" = "$cpuset" ]; then
        die "cpuset_partition: CPUSET ('$cpuset') has no cpus left over after RAYON_THREADS=${rayon_threads} for flysim. Widen CPUSET (Chromium alone needs 1.4 cores at 1080p and ffmpeg 1.2 on the x264 fallback path) or lower RAYON_THREADS."
    fi

    remainder_count="$(echo "$remainder" | awk -F, '{print NF}')"
    page_count=$(( remainder_count - encoder_cores ))
    if [ "$page_count" -lt 1 ]; then
        die "cpuset_partition: CPUSET ('$cpuset') leaves only $remainder_count cpu(s) after RAYON_THREADS=${rayon_threads} for flysim — not enough for ENCODER_CORES=${encoder_cores} (flycast) plus at least one page/xvfb/pulse/mediamtx cpu. Widen CPUSET or lower RAYON_THREADS/ENCODER_CORES."
    fi

    page_cpus="$(echo "$remainder" | cut -d, -f1-"$page_count")"
    encoder_cpus="$(echo "$remainder" | cut -d, -f"$(( page_count + 1 ))"-)"
    echo "$sim_cpus $page_cpus $encoder_cpus"
}

# ---------------------------------------------------------------------------
# operator-box guard
# ---------------------------------------------------------------------------

# on_pve_host — true when this script is actually running ON the Proxmox host
# (as opposed to being sourced/read for review on the operator box or in a
# worktree). Nothing in this repo is allowed to auto-detect this and then
# silently run pct commands: every script that would touch the host must
# still be invoked explicitly by a human on the host. This helper exists only
# so scripts can print a clear error instead of a confusing pct failure.
on_pve_host() {
    [ -e /etc/pve/local ] && command -v pct >/dev/null 2>&1
}

require_pve_host() {
    on_pve_host || die "this script must be run on the host (as root), where 'pct' and /etc/pve exist. It must never be run from the flybrain worktree or CI."
}

# ---------------------------------------------------------------------------
# pct wrappers — every one of these is a thin, logged wrapper. Nothing in
# this file calls pct except through these, so 'grep -n "pct " lib/common.sh'
# is a complete audit of the surface.
# ---------------------------------------------------------------------------

# ct_exists CTID — true if the container is already configured.
ct_exists() {
    local ctid="$1"
    pct config "$ctid" >/dev/null 2>&1
}

# ct_exec CTID -- CMD [ARGS...] — run a command inside the container.
# Never pass secrets as arguments here; pipe them on stdin instead
# (see 06-secrets.sh).
ct_exec() {
    local ctid="$1"
    shift
    [ "${1:-}" = "--" ] && shift
    pct exec "$ctid" -- "$@"
}

# ct_push_file CTID SRC DST [MODE] — copy one file into the container.
ct_push_file() {
    local ctid="$1" src="$2" dst="$3" mode="${4:-0644}"
    pct push "$ctid" "$src" "$dst" --perms "$mode"
}

# ct_pull_file CTID SRC DST — copy one file out of the container (used only
# by the host-side backup script, never by provisioning).
ct_pull_file() {
    local ctid="$1" src="$2" dst="$3"
    pct pull "$ctid" "$src" "$dst"
}

# ---------------------------------------------------------------------------
# idempotent file convergence
# ---------------------------------------------------------------------------

# sha256_local FILE — sha256 of a local file, hex only.
sha256_local() {
    sha256sum "$1" | awk '{print $1}'
}

# sha256_remote CTID PATH — sha256 of a file inside the container, or the
# literal string "MISSING" if it does not exist there yet.
sha256_remote() {
    local ctid="$1" path="$2"
    ct_exec "$ctid" -- sh -c "test -f '$path' && sha256sum '$path' | cut -d' ' -f1 || echo MISSING"
}

# converge_file CTID SRC DST [MODE] [OWNER]
#
# Push SRC to DST inside container CTID only if the content differs
# (compared by sha256). Sets MODE (default 0644) and OWNER (default
# root:root) on every push. Prints "changed" or "unchanged" to stdout so
# callers can decide whether a daemon-reload/restart is needed:
#
#   if [ "$(converge_file "$CTID" ./units/xvfb.service /etc/systemd/system/xvfb.service)" = changed ]; then
#       NEED_RELOAD=1
#   fi
converge_file() {
    local ctid="$1" src="$2" dst="$3" mode="${4:-0644}" owner="${5:-root:root}"
    [ -f "$src" ] || die "converge_file: local source missing: $src"

    local want have
    want="$(sha256_local "$src")"
    have="$(sha256_remote "$ctid" "$dst")"

    if [ "$want" = "$have" ]; then
        # Still converge mode/owner even when content matches, cheaply and
        # idempotently — chmod/chown are no-ops when already correct.
        ct_exec "$ctid" -- chmod "$mode" "$dst" 2>/dev/null || true
        ct_exec "$ctid" -- chown "$owner" "$dst" 2>/dev/null || true
        echo unchanged
        return 0
    fi

    local dstdir
    dstdir="$(dirname "$dst")"
    ct_exec "$ctid" -- mkdir -p "$dstdir"
    ct_push_file "$ctid" "$src" "$dst" "$mode"
    ct_exec "$ctid" -- chown "$owner" "$dst"
    log "converge_file: pushed $src -> ctid=$ctid:$dst"
    echo changed
}

# converge_dir CTID MODE OWNER PATH — ensure a directory exists inside the
# container with the given mode/owner (mkdir -p is naturally idempotent).
converge_dir() {
    local ctid="$1" mode="$2" owner="$3" path="$4"
    ct_exec "$ctid" -- mkdir -p "$path"
    ct_exec "$ctid" -- chmod "$mode" "$path"
    ct_exec "$ctid" -- chown "$owner" "$path"
}

# ---------------------------------------------------------------------------
# host-side container conf convergence (/etc/pve/lxc/<id>.conf)
# ---------------------------------------------------------------------------

# converge_conf_block CONF NAME CONTENT_FILE [GENERATOR] [OWNED_KEY...]
#
# Converge the `lxc.*` lines of a `# BEGIN <NAME>` .. `# END <NAME>` block in
# a PVE container config. Prints "changed" or "unchanged", like
# converge_file, so the caller can decide whether the container needs
# restarting (lxc.* keys are read only at container start).
#
# It converges LINES, not bytes, and that is the whole point. Three things
# this is careful about, each of which is a bug that has actually happened
# on the operator's hosts:
#
#   * /etc/pve is pmxcfs, a FUSE filesystem over the cluster config DB. The
#     new content is built in $TMPDIR and then `cat >`-ed THROUGH the
#     existing inode. Never `mv` a /tmp file across that boundary, and never
#     write when nothing changed (every write is a cluster transaction).
#   * A PVE conf can carry `[snapshotname]` sections. Appending blindly
#     would land lxc.* keys inside a snapshot section, where they are
#     ignored - silently. The block is therefore re-emitted just before the
#     first `^[` line, or at EOF when there is none.
#   * **PVE REWRITES THIS FILE ON EVERY LIFECYCLE OPERATION, AND IT DOES NOT
#     PRESERVE LAYOUT.** Measured on PVE 9.2.10 (the host, 2026-09-16): one
#     `pct stop` / `pct start` of the dev container re-emitted <ctid>.conf with every PVE
#     key sorted alphabetically, every COMMENT hoisted to the top of the
#     file, and every raw `lxc.*` key moved to the end. The sentinel
#     comments and the lines they were supposed to bracket end up in
#     different halves of the file: the block reads as EMPTY and its content
#     reads as unowned strays. A byte-comparing converge therefore reports
#     "changed" forever, rewrites pmxcfs on every run, and - because
#     01-create-ct.sh restarts the container when the block changed -
#     restarts a live container on every provisioning run, which for these
#     containers means taking the stream down. Worse, a
#     comment-bracket-only rewrite leaves the hoisted-out lines behind as
#     duplicates, and after a driver change those duplicates are STALE
#     device majors that nothing removes: exactly the silent breaker the
#     fly-nvidia block exists to prevent.
#
# So ownership is by LINE, not by position:
#
#   * every line of CONTENT_FILE is owned (that is the desired set);
#   * plus every line whose key prefix is one of the OWNED_KEY arguments,
#     which is how a key whose VALUE changed (cpuset.cpus after a
#     re-partition, a dynamic device major after a reboot) gets removed
#     instead of accumulating a stale twin.
#
# "unchanged" means: every desired line appears exactly once somewhere in
# the file, and no owned-but-undesired line appears at all. Position and
# comments are deliberately not part of that comparison, because PVE owns
# those and will move them again. The sentinel comments are still written -
# they are the only in-file signpost for a human - but nothing depends on
# where they end up, and after the next `pct start` they will be at the top
# of the file with the block empty. That is expected, not a fault.
#
# infra/host/fly-nvidia-majors.sh deliberately carries its own copy of this
# logic instead of sourcing this file: it is installed standalone on the host
# as /usr/local/sbin/fly-nvidia-majors.sh, with no infra/ checkout next to
# it. Keep the two in step.
converge_conf_block() {
    local conf="$1" name="$2" content_file="$3" generator="${4:-infra/01-create-ct.sh}"
    shift 3
    [ $# -gt 0 ] && shift            # drop GENERATOR if it was given
    local owned_keys="$*"
    [ -f "$conf" ] || die "converge_conf_block: no such container conf: $conf"
    [ -f "$content_file" ] || die "converge_conf_block: no such content file: $content_file"

    local begin="# BEGIN $name"
    local end="# END $name"
    local blk new
    blk="$(mktemp "${TMPDIR:-/tmp}/fly-conf-blk.XXXXXX")"
    new="$(mktemp "${TMPDIR:-/tmp}/fly-conf-new.XXXXXX")"

    {
        echo "$begin (generated by $generator, do not edit by hand)"
        cat "$content_file"
        echo "$end"
    } > "$blk"

    # Pass 1: is the file already semantically right? Then write nothing.
    if awk -v keysline="$owned_keys" -v desiredfile="$content_file" '
        function is_owned(line,   n, i, k, ks) {
            if (line in desired) return 1
            n = split(keysline, ks, " ")
            for (i = 1; i <= n; i++) {
                k = ks[i]
                if (k != "" && index(line, k) == 1) return 1
            }
            return 0
        }
        BEGIN {
            while ((getline line < desiredfile) > 0) if (line != "") desired[line] = 1
            close(desiredfile)
        }
        { if (is_owned($0)) seen[$0]++ }
        END {
            for (d in desired) if (seen[d] != 1) exit 1
            for (s in seen)    if (!(s in desired)) exit 1
            exit 0
        }
    ' "$conf"; then
        rm -f "$blk" "$new"
        echo unchanged
        return 0
    fi

    # Pass 2: rewrite. Drop every owned line and both sentinels wherever they
    # sit, then re-emit the block before the first section header or at EOF.
    awk -v b="$begin" -v e="$end" -v blockfile="$blk" \
        -v keysline="$owned_keys" -v desiredfile="$content_file" '
        function is_owned(line,   n, i, k, ks) {
            if (line in desired) return 1
            n = split(keysline, ks, " ")
            for (i = 1; i <= n; i++) {
                k = ks[i]
                if (k != "" && index(line, k) == 1) return 1
            }
            return 0
        }
        function emit(  line) {
            while ((getline line < blockfile) > 0) print line
            close(blockfile)
        }
        BEGIN {
            while ((getline line < desiredfile) > 0) if (line != "") desired[line] = 1
            close(desiredfile)
        }
        index($0, b) == 1 { next }
        index($0, e) == 1 { next }
        is_owned($0)      { next }
        /^\[/ && !done    { emit(); done = 1 }
        { print }
        END { if (!done) emit() }
    ' "$conf" > "$new"

    if cmp -s "$new" "$conf"; then
        rm -f "$blk" "$new"
        echo unchanged
        return 0
    fi

    cat "$new" > "$conf"
    rm -f "$blk" "$new"
    log "converge_conf_block: rewrote the '$name' block in $conf"
    echo changed
}

# ---------------------------------------------------------------------------
# misc
# ---------------------------------------------------------------------------

# repo_root — absolute path to infra/'s parent (~/flybrain), independent of
# cwd, so every script can be run from anywhere.
repo_root() {
    (cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
}

infra_root() {
    (cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
}
