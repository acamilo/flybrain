#!/usr/bin/env bash
# infra/provision.sh ENVFILE [--from-step N] [--release RELEASE_TARBALL]
#
# Runs 01..07 for one env file, resumable. docs/design/infra.md section 2.
# Idempotent: running this twice in a row should converge nothing on the
# second run — verify.sh's "no second-run restarts" check depends on the
# per-run log this script pushes into the container at
# /var/lib/fly/provision-log/<timestamp>.log.
#
# --from-step N: resume from step N (1-7) instead of starting at 01. Useful
# after fixing a mid-run failure without re-running everything.
# --release RELEASE_TARBALL: forwarded to 05-deploy.sh; omit to converge
# everything except the flysim/stage/bridge release artifact (the normal
# case until those three packages exist and are built).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

ENVFILE=""
FROM_STEP=1
RELEASE_TARBALL=""

while [ $# -gt 0 ]; do
    case "$1" in
        --from-step)
            FROM_STEP="$2"
            shift 2
            ;;
        --release)
            RELEASE_TARBALL="$2"
            shift 2
            ;;
        -h|--help)
            echo "usage: $0 ENVFILE [--from-step N] [--release TARBALL]"
            exit 0
            ;;
        *)
            if [ -z "$ENVFILE" ]; then
                ENVFILE="$1"
                shift
            else
                die "unexpected argument: $1"
            fi
            ;;
    esac
done

[ -n "$ENVFILE" ] || die "usage: $0 ENVFILE [--from-step N] [--release TARBALL]"
load_env "$ENVFILE"
require_pve_host
need pct

log "provision: env=$ENVFILE ctid=$CTID hostname=$HOSTNAME from-step=$FROM_STEP"

RUN_LOG="$(mktemp "/tmp/fly-provision-run.XXXXXX")"
trap 'rm -f "$RUN_LOG"' EXIT

run_step() {
    local n="$1" script="$2"
    shift 2
    if [ "$n" -lt "$FROM_STEP" ]; then
        log "provision: skipping step $n ($script), before --from-step $FROM_STEP"
        return 0
    fi
    log "provision: === step $n: $script ==="
    # `|| true` keeps `set -e`/pipefail from exiting mid-pipe so the
    # friendlier die() message below actually runs; PIPESTATUS still
    # reflects the real exit code of the step script.
    "$SCRIPT_DIR/$script" "$ENVFILE" "$@" 2>&1 | tee -a "$RUN_LOG" || true
    if [ "${PIPESTATUS[0]}" -ne 0 ]; then
        die "step $n ($script) failed — re-run with: $0 $ENVFILE --from-step $n"
    fi
}

run_step 1 01-create-ct.sh
run_step 2 02-base.sh
run_step 3 03-node.sh
run_step 4 04-mediamtx.sh
if [ -n "$RELEASE_TARBALL" ]; then
    run_step 5 05-deploy.sh "$RELEASE_TARBALL"
else
    run_step 5 05-deploy.sh
fi
run_step 6 06-secrets.sh
run_step 7 07-enable.sh

# Push this run's log into the container for verify.sh's "no second-run
# restarts" check. Logs, not idempotent-converged: always push, timestamped.
ts="$(date -u +%Y%m%dT%H%M%SZ)"
ct_exec "$CTID" -- mkdir -p /var/lib/fly/provision-log
ct_push_file "$CTID" "$RUN_LOG" "/var/lib/fly/provision-log/${ts}.log" 0644

log "provision: done. Run 'infra/verify.sh $ENVFILE' next."
