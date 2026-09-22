#!/usr/bin/env bash
# infra/05-deploy.sh ENVFILE RELEASE_TARBALL
#
# NEVER RE-RUN THIS ON A LIVE RELEASE BOX AS AN IDEMPOTENCY CHECK. It is
# idempotent, and that is not the point: every run does real work inside the
# container — a release extraction, a MANIFEST sha256 pass, a chown -R, a
# possible daemon-reload — on the same cores the sim is trying to hold real
# time on. The 2026-09-16 v0.1.1 deploy on the release container pushed `fly_lag_seconds`
# from 3 s to 5 s for the length of the run, with the stream live. The heavy
# steps are pinned to the page cpus now (see cpu_pin below), which reduces
# that, it does not remove it. Verify a deploy by reading
# /opt/fly/current, the MANIFEST, and the units — not by deploying again.
#
# Installs one release (flysim binary + stage/ + bridge/, built by
# infra/build/build-flysim.sh and infra/build/package-release.sh) under
# /opt/fly/releases/<version>/ with /opt/fly/current as the symlink, plus
# every systemd unit, the app-level config files, and the bin/ helpers.
# docs/design/infra.md section 2 ("Releases go to
# /opt/fly/releases/<version>/ ... a rollback is one `ln -sfn` plus a
# restart. sha256 of every artifact recorded in .../MANIFEST").
#
# RELEASE_TARBALL is optional: this infra pass ships ahead of
# services/flysim, apps/stage and services/bridge (none exist in this repo
# yet), so running 05-deploy.sh with no tarball still converges every unit
# and config file, and only skips the release-artifact step. That is the
# expected shape for provisioning the spike CT before flysim exists.
#
# ROLE (from the env file, infra/lib/common.sh's load_env, default "dev"):
# PRERELEASE_UNITS=1 (env var, with no RELEASE_TARBALL) is the one documented
# way to converge units/config/bin onto a ROLE=release container from an
# untagged tree, and it works only until that container's first tagged deploy
# — see the ROLE gate in section 0 below, and infra/README.md's "Provisioning
# a release container before the first release".
#
# "release" refuses to run at all unless the source tree this script itself
# is running from (infra/lib/common.sh's repo_root) is exactly at a clean
# annotated tag matching ^v[0-9]+\.[0-9]+\.[0-9]+$ — see require_release_tag
# in lib/common.sh, infra/build/tag-release.sh, and docs/stream-mvp-plan.md
# "Release container" (the operator, 2026-09-16: "the release container runs TAGGED
# commits only"). "dev" (the historical behaviour) deploys any commit.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -ge 1 ] || die "usage: $0 ENVFILE [RELEASE_TARBALL]"
load_env "$1"
RELEASE_TARBALL="${2:-}"

# The label this script writes into the files it GENERATES (/etc/fly/fly.env,
# /etc/fly/flypush.env, the four cpuset drop-ins). Deliberately derived from
# the env file's basename rather than "$1" as typed.
#
# "$1" is a path, and the same env file is reached by different spellings
# depending on where the operator ran this from: `infra/05-deploy.sh
# <release-env>` from inside infra/ (infra/README.md's own examples) vs
# `infra/05-deploy.sh the release env file` from a repo root (the shape a
# checkout-at-a-tag deploy naturally takes). converge_file compares sha256, so
# embedding the raw argument made those two spellings look like a CONTENT
# change: every one of these files was re-pushed, NEED_RELOAD tripped a
# daemon-reload, and the cpuset drop-ins logged "applies at the next RESTART of
# that unit" — which on a release container invites an operator to restart
# flysim/flycast, i.e. drop the stream, for a no-op. Measured on the release container's first
# tagged deploy (v0.1.0, 2026-09-16): four drop-ins plus fly.env re-pushed
# purely because the path was spelled `infra/env/...` rather than `env/...`.
ENV_LABEL="env/$(basename "$1")"

# ---------------------------------------------------------------------------
# 0. ROLE gate. Checked first — before require_pve_host/need pct below, and
#    before anything else touches the container — because it only needs
#    git, not the host or pct: a ROLE=release env with an untagged or dirty
#    source tree refuses the WHOLE deploy right here, not just the
#    release-artifact step further down. Running it first (rather than
#    after the host check) is also what lets infra/tests/lint.sh exercise
#    this exact refusal by invoking this script directly against a
#    throwaway git repo, with no the host/pct in the picture at all.
# ---------------------------------------------------------------------------
#    PRERELEASE_UNITS=1 is the one documented exception, and it is narrow on
#    purpose (2026-09-16, the release container provisioning): a brand-new release container
#    has to be provisioned BEFORE any release exists — infra/README.md's own
#    quick start says so ("without --release, provision.sh still converges
#    every unit, config file, and bin/ helper, which is the expected state
#    until services/flysim, apps/stage and services/bridge are built") — but
#    provision.sh runs this step unconditionally, so a ROLE=release env could
#    not be provisioned at all: step 5 refused and took the run down with it.
#    The exception therefore requires ALL THREE of: no RELEASE_TARBALL (so no
#    release artifact is installed and `current` is never flipped),
#    PRERELEASE_UNITS=1 typed by a human, and a container that has never had a
#    release deployed to it (checked against /opt/fly/current further down,
#    once pct is available). That last condition is what keeps this from
#    becoming a way to push an untagged unit file onto a LIVE release box:
#    once the first tagged deploy has happened, this path refuses forever.
# ---------------------------------------------------------------------------
RELEASE_TAG=""
PRERELEASE=0
if [ "$ROLE" = release ]; then
    if [ -z "$RELEASE_TARBALL" ] && [ "${PRERELEASE_UNITS:-0}" = 1 ]; then
        PRERELEASE=1
        log "05-deploy: ROLE=release + PRERELEASE_UNITS=1 and no release tarball — converging units," \
            "config and bin/ helpers ONLY, from a tree that is NOT required to be tagged." \
            "No release artifact is installed and /opt/fly/current is not touched." \
            "This is allowed only until the first tagged deploy (checked below)."
    else
        RELEASE_TAG="$(require_release_tag "$(repo_root)")"
        log "05-deploy: ROLE=release — source tree clean at tag ${RELEASE_TAG}, deploying to ${HOSTNAME} (CT ${CTID})"
    fi
else
    log "05-deploy: ROLE=${ROLE} — any commit deployable (dev behaviour), deploying to ${HOSTNAME} (CT ${CTID})"
fi

require_pve_host
need pct

# CHROMIUM_PROFILE is validated here, not left to the launcher: a typo or a
# `vgl` on a container that never had VirtualGL installed would only show up as
# flystage refusing to start, i.e. a black stream, minutes after the deploy
# reported success.
case "${CHROMIUM_PROFILE:-default}" in
    default|gpu) ;;
    vgl)
        [ "${GPU:-0}" = 1 ] || die "05-deploy: CHROMIUM_PROFILE=vgl needs GPU=1 in $1 — VirtualGL has no card to render on otherwise (docs/design/gpu.md section 3 option (d))."
        [ -n "${VIRTUALGL_VERSION:-}" ] || die "05-deploy: CHROMIUM_PROFILE=vgl needs VIRTUALGL_VERSION in $1, so 02-base.sh installs VirtualGL. Without vglrun, bin/flystage-launch refuses to start rather than broadcast a GL-less page."
        ct_exec "$CTID" -- test -x /usr/bin/vglrun || die "05-deploy: CHROMIUM_PROFILE=vgl but /usr/bin/vglrun is not in CT ${CTID}. Run infra/02-base.sh $1 first."
        ;;
    *)
        die "05-deploy: CHROMIUM_PROFILE in $1 must be 'default', 'gpu' or 'vgl', got '${CHROMIUM_PROFILE}'"
        ;;
esac

# The third condition of the PRERELEASE_UNITS exception above: this container
# must never have had a release deployed to it. /opt/fly/current is the
# symlink 05-deploy.sh flips, so its existence means a tagged release is (or
# was) installed here, and from that point on units come only from a tagged
# tree — no "just this one file" edits onto the live stream.
if [ "$PRERELEASE" -eq 1 ] && ct_exec "$CTID" -- test -e /opt/fly/current; then
    die "05-deploy: PRERELEASE_UNITS=1 refused: CT ${CTID} already has /opt/fly/current, i.e. a release has been deployed here. From the first tagged deploy onwards, ROLE=release accepts units and config only from a clean annotated tag — tag the tree (infra/build/tag-release.sh vX.Y.Z), package it, and deploy that."
fi

INFRA_DIR="$(infra_root)"
NEED_RELOAD=0

# ---------------------------------------------------------------------------
# 0b. CPU pinning for the heavy in-container steps.
#
# Everything this script runs in the container goes through `pct exec`, which
# lands on the container's WHOLE cpuset — including flysim's whole physical
# cores. Tar extraction, the MANIFEST sha256 pass and `chown -R` over a
# release tree (dataset included) are the three expensive ones, and on
# The release container's v0.1.1 deploy (2026-09-16, stream live) they took
# `fly_lag_seconds` from 3 s to 5 s for the length of the run.
#
# So pin them to the PAGE cpus — the same middle group the cpuset drop-ins in
# section 3b give xvfb/flystage/flystage-web/pulse/mediamtx, derived from the
# same lib/common.sh `cpuset_partition` call with the same
# CPUSET/RAYON_THREADS/ENCODER_CORES values, so the two can never disagree
# about which cpus are the sim's. The page group is the right target: it has
# the most slack (Chromium is not real-time the way the sim loop is), and the
# encoder group must stay clear or the deploy shows up as dropped frames on
# the broadcast.
#
# No CPUSET, or a conf that does not match it: cpu_pin is a no-op and the
# steps run exactly as they did before, unpinned. Same guard shape (and the
# same reason) as section 3b's.
#
# RAYON_THREADS_EFFECTIVE/ENCODER_CORES_EFFECTIVE are defined here rather
# than next to the fly.env writer further down because this runs before it;
# fly.env still gets these exact values.
# ---------------------------------------------------------------------------
RAYON_THREADS_EFFECTIVE="${RAYON_THREADS:-3}"
ENCODER_CORES_EFFECTIVE="${ENCODER_CORES:-2}"
DEPLOY_CPUS=""
if [ -n "${CPUSET:-}" ]; then
    deploy_conf_cpus="$(awk '$1 == "lxc.cgroup2.cpuset.cpus:" { print $2; exit }' \
        "/etc/pve/lxc/${CTID}.conf" 2>/dev/null || true)"
    if [ "$deploy_conf_cpus" = "$CPUSET" ]; then
        read -r _deploy_sim_cpus DEPLOY_CPUS _deploy_encoder_cpus \
            <<< "$(cpuset_partition "$CPUSET" "$RAYON_THREADS_EFFECTIVE" "$ENCODER_CORES_EFFECTIVE")"
        log "05-deploy: heavy in-container steps pinned to the page cpus (${DEPLOY_CPUS}) so extraction/verification cannot compete with flysim"
    else
        log "05-deploy: NOT pinning the heavy in-container steps — the fly-cpuset block in" \
            "/etc/pve/lxc/${CTID}.conf says '${deploy_conf_cpus:-nothing}', not CPUSET ('$CPUSET')." \
            "They will run on the container's whole cpuset, i.e. on flysim's cores too."
    fi
else
    log "05-deploy: CPUSET unset — heavy in-container steps run unpinned (no partition configured)"
fi

# Whether the only difference between two compatibility strings is the adapter
# segment, and FLY_ACCEPT_ADAPTERS names the adapter the live checkpoints carry.
#
# The bash half of flybrain_gb::compatibility::decide, which is what flysim
# itself applies at restore. Both have to agree: a gate that let a deploy
# through and a flysim that then refused every checkpoint would be the black
# stream this whole section exists to prevent. The string is
# {kernel}/{adapter}/{fingerprint}/{plasticity}/binjgb:{rev}/pokered:{commit}/statefmt:{id},
# so the adapter is segment 1 and nothing else may move.
adapter_migration_accepted() {
    local live="$1" new="$2" accepted="$3"
    local -a live_parts new_parts
    IFS='/' read -r -a live_parts <<< "$live"
    IFS='/' read -r -a new_parts <<< "$new"
    [ "${#live_parts[@]}" -eq "${#new_parts[@]}" ] || return 1
    local i differing=0 index=-1
    for ((i = 0; i < ${#live_parts[@]}; i++)); do
        if [ "${live_parts[$i]}" != "${new_parts[$i]}" ]; then
            differing=$((differing + 1))
            index=$i
        fi
    done
    [ "$differing" -eq 1 ] && [ "$index" -eq 1 ] || return 1
    local entry
    for entry in ${accepted//,/ }; do
        [ "$entry" = "${live_parts[1]}" ] && return 0
    done
    return 1
}

# cpu_pin CMD [ARGS...] — run CMD inside the container on the page cpus.
# Falls through to a plain ct_exec when no partition is configured, so this is
# a no-op on an unpartitioned container rather than a new failure mode (a
# taskset with an empty -c list exits 1 and would fail the deploy).
cpu_pin() {
    if [ -n "$DEPLOY_CPUS" ]; then
        ct_exec "$CTID" -- taskset -c "$DEPLOY_CPUS" "$@"
    else
        ct_exec "$CTID" -- "$@"
    fi
}

# ---------------------------------------------------------------------------
# 1. Release artifact (optional until services/flysim + apps/stage +
#    services/bridge exist).
# ---------------------------------------------------------------------------
if [ -n "$RELEASE_TARBALL" ]; then
    [ -f "$RELEASE_TARBALL" ] || die "release tarball not found: $RELEASE_TARBALL"
    base="$(basename "$RELEASE_TARBALL")"
    version="${base#flybrain-}"
    version="${version%.tar.gz}"
    [[ "$version" =~ ^[A-Za-z0-9._-]+$ ]] || die "could not parse a version out of tarball name: $base"

    if [ "$ROLE" = release ]; then
        # The release directory is named after the tag — enforced here by
        # requiring the tarball's own VERSION (infra/build/package-release.sh's
        # first argument) to equal the tag this source tree is checked out
        # at, rather than trusting whoever named the tarball.
        [ "$version" = "$RELEASE_TAG" ] || die "05-deploy: ROLE=release refuses to deploy: tarball version '${version}' does not match the source tree's tag '${RELEASE_TAG}' — build it with: infra/build/package-release.sh ${RELEASE_TAG} <flysim-bin> <stage-dir> <bridge-dir> <out-dir>"
    fi

    release_path="/opt/fly/releases/${version}"
    if ct_exec "$CTID" -- test -f "${release_path}/MANIFEST"; then
        log "05-deploy: release ${version} already installed at ${release_path}, skipping extraction"
    else
        log "05-deploy: installing release ${version}"
        remote_tmp="/tmp/$(basename "$RELEASE_TARBALL")"
        ct_push_file "$CTID" "$RELEASE_TARBALL" "$remote_tmp" 0644
        ct_exec "$CTID" -- mkdir -p "$release_path"
        # The three heavy steps (section 0b): extraction, verification, chown.
        cpu_pin tar -xzf "$remote_tmp" -C "$release_path"
        ct_exec "$CTID" -- rm -f "$remote_tmp"

        log "05-deploy: verifying MANIFEST sha256s post-extraction"
        cpu_pin sh -c "cd '$release_path' && sha256sum -c MANIFEST --quiet" \
            || die "MANIFEST verification failed for release ${version}; NOT flipping 'current'"

        ct_exec "$CTID" -- chmod 0755 "${release_path}/flysim"
        cpu_pin chown -R fly:fly "$release_path"
    fi

    log "05-deploy: overlaying infra-owned stage/serve.{mjs,sh} (docs/design/infra.md's 'own tiny static server' clarification)"
    converge_file "$CTID" "$INFRA_DIR/config/serve.mjs" "${release_path}/stage/serve.mjs" 0644 fly:fly >/dev/null
    converge_file "$CTID" "$INFRA_DIR/config/serve.sh"  "${release_path}/stage/serve.sh"  0755 fly:fly >/dev/null

    # -----------------------------------------------------------------------
    # The checkpoint compatibility gate.
    #
    # flysim refuses a checkpoint whose compatibility string differs from its
    # own, and when EVERY candidate is refused it refuses to start rather than
    # run fresh over them ("Move /srv/fly/state aside by hand to reset
    # deliberately"). That is the right behaviour and a black stream if the
    # first time anyone finds out is at deploy: on 2026-09-16 a release that
    # bumped the Pokemon ladder from `pokered-unique8-v3` to `-v4` put the live
    # demo down for five minutes, with systemd's start limiter parking flysim
    # after five attempts and flystage timing out behind it.
    #
    # So ask the NEW binary, with the NEW release's dataset, what string it
    # would write, and compare it with what the durable state actually holds,
    # BEFORE the symlink moves. Cost: one dataset load, a second or two.
    #
    # FLY_RESET_STATE=1 is the deliberate override: it archives the durable
    # checkpoints (kept, never deleted) and clears the tmpfs hot ring — the hot
    # checkpoints and the on-screen chat ring's sidecar — so the new build warms
    # up fresh. Everything learned so far is thrown away, which is why it is not
    # the default.
    #
    # FLY_ACCEPT_ADAPTERS is the *other* override, and the opposite one: it keeps
    # the run. It names adapter version strings whose checkpoints the new build
    # may migrate — e.g. FLY_ACCEPT_ADAPTERS=pokered-unique8-v5 for the deploy
    # that adds the catch reward. It only applies when the adapter segment is the
    # ONLY difference between the two strings and the new build's adapter says it
    # can read that one; a dataset, kernel, emulator or state-format change is
    # still a refusal, because none of those has a migration. The same variable is
    # written into /etc/fly/fly.env below, so flysim applies the same rule at
    # restore that this gate applied at deploy.
    # -----------------------------------------------------------------------
    state_dir="${FLY_STATE_DIR:-/srv/fly/state}"
    hot_dir="${FLY_STATE_HOT_DIR:-/run/fly/state}"
    if ct_exec "$CTID" -- test -x "${release_path}/flysim" \
        && ct_exec "$CTID" -- test -f "${state_dir}/manifest.json"; then
        # The store's manifest.json is only an index; the compatibility string lives in each
        # checkpoint's own envelope, so the NEW binary is asked to decode the newest one (it can
        # read an older build's envelope — that is the whole point of the version field).
        live_compat="$(ct_exec "$CTID" -- "${release_path}/flysim" \
            --print-state-compatibility "$state_dir" 2>/dev/null | tr -d '\r' || true)"
        new_compat="$(ct_exec "$CTID" -- env \
            "FLY_GAME=${GAME}" \
            "FLY_DATASET=${release_path}/data/fafb-v783" \
            "FLY_ROM=/srv/fly/rom/${ROM_SHA256:-none}.gb" \
            "FLY_ROM_SHA256=${ROM_SHA256:-}" \
            "${release_path}/flysim" --print-compatibility 2>/dev/null | tr -d '\r' || true)"

        if [ -z "$new_compat" ]; then
            log "05-deploy: WARNING could not read the new build's compatibility string" \
                "(no --print-compatibility in this release?) — skipping the state gate. Check by hand:" \
                "pct exec $CTID -- ${release_path}/flysim --print-compatibility"
        elif [ -z "$live_compat" ]; then
            log "05-deploy: no decodable checkpoint in ${state_dir} — nothing to compare, continuing"
        elif [ "$new_compat" = "$live_compat" ]; then
            log "05-deploy: checkpoint compatibility matches the live state, the new build will restore it"
        elif [ -n "${FLY_ACCEPT_ADAPTERS:-}" ] \
            && adapter_migration_accepted "$live_compat" "$new_compat" "$FLY_ACCEPT_ADAPTERS"; then
            log "05-deploy: FLY_ACCEPT_ADAPTERS=${FLY_ACCEPT_ADAPTERS} — the adapter version is the only difference, and it is named; the run is KEPT and migrated"
            log "05-deploy:   live: $live_compat"
            log "05-deploy:   new:  $new_compat"
        elif [ "${FLY_RESET_STATE:-0}" = 1 ]; then
            archive="${state_dir}.$(date -u +%Y%m%d%H%M%S)"
            log "05-deploy: FLY_RESET_STATE=1 — compatibility CHANGED, archiving the durable state to ${archive} and clearing the hot ring"
            log "05-deploy:   live: $live_compat"
            log "05-deploy:   new:  $new_compat"
            # state_dir is its own mountpoint, so the directory itself cannot be
            # renamed; its contents move instead.
            ct_exec "$CTID" -- sh -c "mkdir -p '$archive' && mv '${state_dir}'/*.checkpoint '${state_dir}/manifest.json' '$archive'/ 2>/dev/null; chown -R fly:fly '$archive'"
            ct_exec "$CTID" -- sh -c "rm -f '${hot_dir}'/*.checkpoint '${hot_dir}/manifest.json' '${hot_dir}/chat-ring.json' 2>/dev/null; true"
        else
            die "05-deploy: REFUSING to deploy release ${version}: its checkpoint compatibility string does not match the live state in ${state_dir}, so flysim would refuse every checkpoint there and then refuse to start at all — a black stream.
  live state: ${live_compat}
  new build:  ${new_compat}
The difference is usually an adapter/ladder or dataset version bump. Three ways forward:
  * deploy a build whose string matches (check out the commit the running release was built from), or
  * if the ADAPTER VERSION is the only segment that differs and the new build documents a
    migration from the old one, re-run with FLY_ACCEPT_ADAPTERS set to the adapter id in the live
    string (e.g. FLY_ACCEPT_ADAPTERS=pokered-unique8-v5). The run is kept; flysim applies the same
    rule at restore. See docs/design/flysim.md, \"Restoring across an adapter version\", or
  * accept losing everything the brain has learned and re-run with FLY_RESET_STATE=1, which
    archives ${state_dir}'s checkpoints to ${state_dir}.<timestamp> (kept, not deleted) and
    clears ${hot_dir} so the new build warms up fresh.
The 'current' symlink has NOT been moved; the running release is untouched."
        fi
    fi

    log "05-deploy: flipping /opt/fly/current -> ${release_path} atomically"
    ct_exec "$CTID" -- sh -c "ln -sfn '$release_path' /opt/fly/current.tmp && mv -T /opt/fly/current.tmp /opt/fly/current"
else
    log "05-deploy: no RELEASE_TARBALL given, skipping the release-artifact step (units/config still converge below)"
fi

# ---------------------------------------------------------------------------
# 2. systemd units
# ---------------------------------------------------------------------------
log "05-deploy: converging systemd units"
for f in "$INFRA_DIR"/units/*.service "$INFRA_DIR"/units/*.timer "$INFRA_DIR"/units/fly.target; do
    [ -f "$f" ] || continue
    name="$(basename "$f")"
    changed="$(converge_file "$CTID" "$f" "/etc/systemd/system/${name}" 0644 root:root)"
    [ "$changed" = changed ] && NEED_RELOAD=1
done

# ---------------------------------------------------------------------------
# 3. app config
# ---------------------------------------------------------------------------
log "05-deploy: converging app config (pulse.pa, chromium-flags, chromium-flags.gpu)"
ct_exec "$CTID" -- mkdir -p /etc/fly
converge_file "$CTID" "$INFRA_DIR/config/pulse.pa" /etc/fly/pulse.pa 0644 root:root >/dev/null
converge_file "$CTID" "$INFRA_DIR/config/chromium-flags" /etc/fly/chromium-flags 0644 root:root >/dev/null
# The gpu and vgl profiles ship unconditionally but are only SELECTED by
# FLY_CHROMIUM_PROFILE (below). `gpu` is spike-only and measured insufficient
# on its own; `vgl` is the VirtualGL EGL configuration that passed on the dev container
# and needs VirtualGL in the container. See those files' headers,
# docs/design/gpu.md section 3 and infra/docs/virtualgl-spike.md.
converge_file "$CTID" "$INFRA_DIR/config/chromium-flags.gpu" /etc/fly/chromium-flags.gpu 0644 root:root >/dev/null
converge_file "$CTID" "$INFRA_DIR/config/chromium-flags.vgl" /etc/fly/chromium-flags.vgl 0644 root:root >/dev/null

# The chat deny list is the one config file this script does NOT converge: it is
# operator-owned (docs/control-api.md, [chat] deny_list), edited live on shift
# and re-read by flysim on SIGHUP, so overwriting it on every deploy would
# silently un-ban whatever someone added an hour ago. Installed once, then left
# alone. `git show` of infra/config/chat-deny.txt is always the pristine copy.
log "05-deploy: chat deny list (installed once, never overwritten)"
if ct_exec "$CTID" -- test -f "${CHAT_DENY_LIST:-/srv/fly/chat-deny.txt}"; then
    log "05-deploy: ${CHAT_DENY_LIST:-/srv/fly/chat-deny.txt} already exists, leaving the operator's copy alone"
else
    ct_exec "$CTID" -- mkdir -p /srv/fly
    ct_push_file "$CTID" "$INFRA_DIR/config/chat-deny.txt" "${CHAT_DENY_LIST:-/srv/fly/chat-deny.txt}" 0644
    ct_exec "$CTID" -- chown fly:fly "${CHAT_DENY_LIST:-/srv/fly/chat-deny.txt}"
    log "05-deploy: installed the empty starter deny list"
fi

log "05-deploy: non-secret env files"
# RAYON_THREADS_EFFECTIVE / ENCODER_CORES_EFFECTIVE are the single source of
# truth for flysim's Rayon pool size and flycast's core count: written into
# /etc/fly/fly.env below (RAYON_NUM_THREADS) AND handed to cpuset_partition
# for the AllowedCPUs= split in section 3b, so the thread/core counts can
# never drift apart (see cpuset_partition's own header comment). They are
# assigned in section 0b, which needs them earlier than this for the
# deploy-time cpu pinning; nothing between here and there changes them.
tmp_fly_env="$(mktemp)"
tmp_flypush_env="$(mktemp)"
trap 'rm -f "$tmp_fly_env" "$tmp_flypush_env"' EXIT
{
    echo "# generated by infra/05-deploy.sh from ${ENV_LABEL} — do not hand-edit, edit the env file source"
    echo "FLY_GAME=${GAME}"
    echo "FLY_REWARD_ADAPTER=${REWARD_ADAPTER:-}"
    echo "FLY_DECODER_PRESET=${DECODER_PRESET:-}"
    echo "FLY_ROM_SHA256=${ROM_SHA256:-}"
    # The connectome. Nothing set this before, so flysim fell back to its own
    # default (paths.dataset = /srv/fly/data/fafb-v783) — a path no script in
    # this repo ever creates, so the service could not start at all. The
    # dataset ships inside the release (package-release.sh stages it as
    # data/fafb-v783), which is also what makes a rollback carry the matching
    # dataset with it. Found on the P0 spike.
    echo "FLY_DATASET=/opt/fly/current/data/fafb-v783"
    if [ -n "${ROM_SHA256:-}" ]; then
        echo "FLY_ROM=/srv/fly/rom/${ROM_SHA256}.gb"
    else
        echo "# FLY_ROM unset: ROM_SHA256 is empty in $1 (VERIFY — see the env file comments)"
    fi
    # GPU / encoder / pinning (docs/design/gpu.md section 8). These are the
    # values flycast.service, flystage.service and flysim.service read
    # through EnvironmentFile=/etc/fly/fly.env. Defaults here are the
    # pre-GPU behaviour, so a GPU=0 container is byte-identical in intent to
    # what it ran before: x264, the default Chromium profile.
    echo "FLY_ENCODER=${FLY_ENCODER:-x264}"
    echo "FLY_NVENC_PRESET=${FLY_NVENC_PRESET:-p4}"
    # x264 preset, read by bin/flycast-launch's encoder_x264(). veryfast is
    # the wire default; the release container (2026-09-16, release box, x264 at 6000k)
    # measured it contended and moved to superfast — see
    # <release-env>'s X264_PRESET comment and docs/runbook.md "CPU
    # partition (cpuset)". Unused while FLY_ENCODER=nvenc.
    echo "FLY_X264_PRESET=${X264_PRESET:-veryfast}"
    # default | gpu | vgl (bin/flystage-launch). `vgl` additionally needs
    # VIRTUALGL_VERSION set so 02-base.sh installed VirtualGL; the launcher
    # refuses to start without vglrun rather than broadcasting a GL-less page.
    echo "FLY_CHROMIUM_PROFILE=${CHROMIUM_PROFILE:-default}"
    # Only written for the profile that reads it, so a default-profile fly.env
    # does not grow a line about a renderer it never uses. egl0 is the first
    # EGL device VirtualGL enumerates, which on the host is the Quadro; VGL_DEVICE
    # exists because a second GPU or a new Mesa device could renumber them
    # (verify.sh check 8g is what catches that).
    if [ "${CHROMIUM_PROFILE:-default}" = vgl ]; then
        echo "VGL_DEVICE=${VGL_DEVICE:-egl0}"
    fi
    # systemd expands %specifiers in Environment=, not ${VARS}, so
    # RAYON_NUM_THREADS cannot be forwarded from an env file by
    # flysim.service itself — it is written here instead. 4 was the
    # measured knee against the p0 spike's idle-container numbers (p0
    # measurement 2: 1.886x at 4 threads, 1.399x at 8); 2026-09-15's P0
    # spike run 2, under the whole stack on a real (non-whole-core)
    # automatic cpuset, found 3 beats 4 (1.753x vs 1.570x) because the
    # sweep wants whole physical cores, not socket locality — see
    # infra/docs/p0-measurements.md, "the host the dev container run 2". Default is now 3.
    echo "RAYON_NUM_THREADS=${RAYON_THREADS_EFFECTIVE}"
    # The LIF tick on the GPU (infra/docs/lif-cuda-spike.md,
    # infra/docs/cuda-on-dev.md). Written for every container, defaulting to
    # 0, for the same reason FLY_ENCODER does: a GPU=0 container's fly.env
    # then says out loud that the backend is off, instead of leaving an
    # operator to work out whether the variable's absence means off or
    # "depends on the build".
    #
    # 1 only attaches the backend if the binary was built with the `cuda`
    # feature (FLY_CARGO_FEATURES=cuda infra/build/build-flysim.sh); a
    # feature-less binary ignores it silently, so the deploy record's check is
    # flysim's own startup log, not this line. Bit-exact with the CPU kernel,
    # so FLY_ENCODER-style flipping between backends across restarts keeps the
    # checkpoint: the compatibility string this script gates on does not move.
    echo "FLY_LIF_CUDA=${FLY_LIF_CUDA:-0}"
    # On-screen chat (docs/control-api.md [chat]). FLY_CHAT_ENABLED=0 is the
    # kill switch: POST /chat answers 403 and the feed header omits `chat`, so
    # the CHAT panel blanks without touching flybridge or the page. flysim reads
    # both of these from this file through EnvironmentFile=.
    echo "FLY_CHAT_ENABLED=${FLY_CHAT_ENABLED:-1}"
    echo "FLY_CHAT_DENY_LIST=${CHAT_DENY_LIST:-/srv/fly/chat-deny.txt}"
    # The macro buttons (docs/control-api.md [macros], docs/design/macros.md
    # section 12). "raw" is the default and is unchanged behaviour; "macros"
    # gives every macro type its own population and its own readout channel,
    # with the scene deciding which of them are on the pad. Written
    # unconditionally so that a box running either says so in one grep, and so
    # that flipping the dev box back is an env edit rather than a build. flysim
    # refuses anything but "raw" for a game with no macros, reads the retired
    # "palette"/"plan" as "macros" with a warning, and refuses an unrecognised
    # value outright.
    echo "FLY_MACRO_MODE=${FLY_MACRO_MODE:-raw}"
    # How long a macro leaves a target alone after a walk to it aborted
    # (macros.md section 12.1, the Viridian stall). Only written when it is set,
    # because the default lives in the crate and a box that has not tuned it
    # should say nothing rather than pin the default into its env file.
    if [[ -n "${FLY_MACRO_BLOCKED_MINUTES:-}" ]]; then
        echo "FLY_MACRO_BLOCKED_MINUTES=${FLY_MACRO_BLOCKED_MINUTES}"
    fi
    # Adapter versions whose checkpoints this build may migrate
    # (flybrain_gb::compatibility, docs/design/flysim.md "Restoring across an
    # adapter version"). Only written when it is set, because the safe state is
    # absent: an empty or missing variable migrates nothing, which is what every
    # deploy before 2026-09-22 did. It stays in fly.env for as long as the
    # operator leaves it on the deploy command line, so removing the opt-in is
    # one deploy without it.
    if [[ -n "${FLY_ACCEPT_ADAPTERS:-}" ]]; then
        echo "FLY_ACCEPT_ADAPTERS=${FLY_ACCEPT_ADAPTERS}"
    fi
    # flybridge (services/bridge/src/config.ts). Nothing wrote these before, so
    # flybridge.service had no EnvironmentFile= at all and the service refused to
    # start with "CHANNEL is required / BOT_USER is required / GAME_TITLE is
    # required" the first time the release actually carried a built bridge —
    # found bringing the release container up on 2026-09-16. None of them is a secret (the app
    # id/secret arrive as a systemd credential, the OAuth tokens as a file mode
    # 0600 in /var/lib/flybridge), so they belong in this generated file next to
    # everything else the units read.
    #
    # BOT_USER defaults to the channel itself: one account can hold both roles,
    # and the first live channel does (see the comment on buildAuthProvider in
    # services/bridge/src/auth.ts). SIM_CONTROL_URL matches flysim's own control
    # API bind; TOKENS_FILE matches flybridge.service's ConditionPathExists, so
    # moving one means moving the other.
    echo "CHANNEL=${TWITCH_CHANNEL:-}"
    echo "BOT_USER=${TWITCH_BOT_USER:-${TWITCH_CHANNEL:-}}"
    echo "GAME_TITLE=${GAME_TITLE:-}"
    echo "SIM_CONTROL_URL=${SIM_CONTROL_URL:-http://127.0.0.1:7401}"
    echo "TOKENS_FILE=${TOKENS_FILE:-/var/lib/flybridge/tokens.json}"
    echo "REDEMPTION_STATE_FILE=${REDEMPTION_STATE_FILE:-/var/lib/flybridge/redemption-state.json}"
    echo "NOTICE_STATE_FILE=${NOTICE_STATE_FILE:-/var/lib/flybridge/notice-state.json}"
    # How long the channel.chat.message EventSub subscription may stay
    # unconfirmed before the bridge logs one FATAL line and exits 75 so systemd
    # restarts it with a fresh websocket transport
    # (services/bridge/src/subscription-health.ts; the 2026-09-16 hour of dead
    # chat is in infra/docs/runbook.md under "chat dead, bridge active"). The
    # bridge defaults to these same numbers if the variables are absent, so an
    # older /etc/fly/fly.env still behaves correctly.
    echo "EVENTSUB_GRACE_MS=${EVENTSUB_GRACE_MS:-60000}"
    # Minimum gap between two startup/recovery notices in chat, across
    # restarts, so a restart loop cannot spam the channel.
    echo "NOTICE_MIN_INTERVAL_MS=${NOTICE_MIN_INTERVAL_MS:-600000}"
    # Channel Points no longer need Affiliate, so redemptions can be on from the
    # first day of a channel; Predictions still do, which is why that one stays
    # off and its scope stays unrequested.
    echo "FEATURE_REDEMPTIONS=${FEATURE_REDEMPTIONS:-0}"
    echo "FEATURE_PREDICTIONS=${FEATURE_PREDICTIONS:-0}"
    echo "FEATURE_ONSCREEN_CHAT=${FEATURE_ONSCREEN_CHAT:-1}"
    # Quiet mode (services/bridge/src/config.ts, the operator 2026-09-17: "make bot
    # less chatty. speaks only when spoken to. doesn't greet."). 1 = no startup
    # or recovery notice, no explainer rotation, no follow/raid thanks; the five
    # commands still answer and viewer chat still reaches the on-screen CHAT
    # panel. Follows and raids stay subscribed and still increment
    # flybridge_follows_total / flybridge_raids_total. Default 0, which is the
    # behaviour every release before this one shipped.
    echo "FEATURE_QUIET=${FEATURE_QUIET:-0}"
    # Explainer rotation period. 0 is "off" on its own, without FEATURE_QUIET's
    # other three effects; unset means the bridge's own 20-minute default.
    if [[ -n "${EXPLAINER_INTERVAL_MS:-}" ]]; then
        echo "EXPLAINER_INTERVAL_MS=${EXPLAINER_INTERVAL_MS}"
    fi
} > "$tmp_fly_env"
{
    echo "# generated by infra/05-deploy.sh from ${ENV_LABEL} — do not hand-edit, edit the env file source"
    echo "# no non-secret flypush overrides today; the wrapper's defaults are correct"
} > "$tmp_flypush_env"
converge_file "$CTID" "$tmp_fly_env" /etc/fly/fly.env 0644 root:root >/dev/null
converge_file "$CTID" "$tmp_flypush_env" /etc/fly/flypush.env 0644 root:root >/dev/null

# ---------------------------------------------------------------------------
# 3b. The cpuset partition drop-ins (infra/README.md step 4).
#
# P0 spike run 2 measured this as the difference between a container that
# holds real time and one that does not: with everything sharing the
# container's cpuset, flysim burned 4.09 cores, `fly_lag_seconds` grew
# without bound (31 s eight minutes in) and x11grab lost ~4 frames a second
# in both directions. Partitioned — flysim on its own whole physical cores,
# everything else on the rest — flysim uses 1.90 cores and lag holds at 0.
# Nothing in infra/ installed these, so every provisioned container was one
# hand-typed step away from being quietly broken; the P0 run wrote them
# with `printf | pct exec tee` and they lived only on that container.
#
# Derived from CPUSET via lib/common.sh's cpuset_partition, not hardcoded,
# into THREE groups: flysim gets the FIRST RAYON_THREADS cpus (so the
# thread count and the core count cannot drift apart, which is the failure
# mode flysim.service's header warns about — RAYON_THREADS_EFFECTIVE above
# is the same value written into /etc/fly/fly.env as RAYON_NUM_THREADS);
# flycast gets the LAST ENCODER_CORES_EFFECTIVE cpus of what is left; and
# xvfb/flystage/flystage-web/pulse/mediamtx share whatever remains in
# between. flycast is split off from the rest of the page/capture group
# because the release container (2026-09-16) measured Chromium's compositor starving when
# it shared cores with the x264 encoder — 63% of captured frames unchanged
# across two watchdog passes, against 2% on the NVENC dev box. All seven
# units get the drop-in — until 2026-09-16 only flysim/xvfb/flystage/flycast
# did (two groups, not three), leaving flystage-web/pulse/mediamtx on the
# container's full, unpartitioned cpuset.
#
# Guarded on the conf, deliberately. AllowedCPUs values outside the
# container's own cpuset leave `cpuset.cpus.effective` empty and the units
# unstartable, so these are only written once /etc/pve/lxc/$CTID.conf
# actually carries CPUSET (i.e. 01-create-ct.sh has run).
# They take effect at the next restart of each unit, not at daemon-reload.
#
# Gated on CPUSET alone, NOT on GPU (2026-09-16, the release container provisioning): these
# drop-ins used to require GPU=1 as well, which silently skipped the partition
# on every CPU-only container — the release box included, where the whole
# measurement that justifies the partition was taken on the x264 encoder.
#
# The lookup is whole-file and NOT scoped to the `# BEGIN fly-cpuset` block:
# PVE re-emits the conf on every lifecycle operation with all comments
# hoisted to the top and all raw `lxc.*` keys moved to the end, so after one
# `pct stop`/`pct start` the block is empty and its content lives elsewhere
# in the file (measured on PVE 9.2.10, 2026-09-16 — see
# lib/common.sh's converge_conf_block header). A block-scoped read finds
# nothing and silently skips the partition that keeps the sim in real time.
# ---------------------------------------------------------------------------
if [ -n "${CPUSET:-}" ]; then
    conf_cpus="$(awk '$1 == "lxc.cgroup2.cpuset.cpus:" { print $2; exit }' \
        "/etc/pve/lxc/${CTID}.conf" 2>/dev/null || true)"

    if [ "$conf_cpus" != "$CPUSET" ]; then
        log "05-deploy: NOT writing the cpuset drop-ins — the fly-cpuset block in" \
            "/etc/pve/lxc/${CTID}.conf says '${conf_cpus:-nothing}', not CPUSET ('$CPUSET')." \
            "Run 01-create-ct.sh first; an AllowedCPUs= outside the container's own cpuset" \
            "leaves cpuset.cpus.effective empty and the unit unstartable."
    else
        read -r sim_cpus page_cpus encoder_cpus <<< "$(cpuset_partition "$CPUSET" "$RAYON_THREADS_EFFECTIVE" "$ENCODER_CORES_EFFECTIVE")"
        log "05-deploy: cpuset partition — flysim=$sim_cpus, xvfb/flystage/flystage-web/pulse/mediamtx=$page_cpus, flycast=$encoder_cpus"
        tmp_dropin="$(mktemp)"
        for u in flysim xvfb flystage flystage-web flycast pulse mediamtx; do
            case "$u" in
                flysim)  cpus="$sim_cpus" ;;
                flycast) cpus="$encoder_cpus" ;;
                *)       cpus="$page_cpus" ;;
            esac
            {
                echo "# generated by infra/05-deploy.sh from CPUSET in ${ENV_LABEL} — do not hand-edit."
                echo "# infra/README.md step 4 and infra/docs/p0-measurements.md run 2 measurement 4."
                echo "[Service]"
                echo "AllowedCPUs=${cpus}"
            } > "$tmp_dropin"
            ct_exec "$CTID" -- mkdir -p "/etc/systemd/system/${u}.service.d"
            changed="$(converge_file "$CTID" "$tmp_dropin" "/etc/systemd/system/${u}.service.d/cpuset.conf" 0644 root:root)"
            if [ "$changed" = changed ]; then
                NEED_RELOAD=1
                log "05-deploy: ${u}.service.d/cpuset.conf changed — it applies at the next RESTART of that unit, not at daemon-reload"
            fi
        done
        rm -f "$tmp_dropin"
    fi
else
    log "05-deploy: CPUSET unset — no cpuset partition drop-ins (see infra/README.md step 4)"
fi

# ---------------------------------------------------------------------------
# 4. bin/ helpers (fly-backup-stage is host-side only, deliberately not
#    pushed into the container — see its own header comment).
# ---------------------------------------------------------------------------
log "05-deploy: converging bin/ helpers to /opt/fly/bin"
ct_exec "$CTID" -- mkdir -p /opt/fly/bin
for name in fly-watchdog fly-recap fly-retention fly-reset-to-milestone flypush flystage-launch flycast-launch wait-for-x wait-for-stage wait-for-health; do
    converge_file "$CTID" "$INFRA_DIR/bin/$name" "/opt/fly/bin/$name" 0755 root:root >/dev/null
done

# ---------------------------------------------------------------------------
# 5. reload if anything unit-shaped changed
# ---------------------------------------------------------------------------
if [ "$NEED_RELOAD" -eq 1 ]; then
    log "05-deploy: unit files changed, daemon-reload"
    ct_exec "$CTID" -- systemctl daemon-reload
fi

# claim-log-style line (infra/README.md's "claim the container ID" section
# shows the convention this echoes: "DATE: claiming the release container ... for
# flybrain/infra."). This does not write to $AGENT_CLAIM_LOG itself —
# that file is host state, appended to by hand per that same section — but
# printing the line in this exact shape is what makes it copy-pasteable
# into it, tag included, for a ROLE=release deploy.
log "05-deploy: $(date -u +%Y-%m-%d): deployed ${HOSTNAME} (CT ${CTID}) role=${ROLE}$([ "$PRERELEASE" -eq 1 ] && echo ' (prerelease units only, untagged tree)') tag=${RELEASE_TAG:-none} release=${version:-units-only}"
log "05-deploy: done"
