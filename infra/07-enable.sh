#!/usr/bin/env bash
# infra/07-enable.sh ENVFILE
#
# Enable and start the full unit set in order, with health gates, plus the
# four timers. docs/design/infra.md section 3: fly.target is the single
# verb ("the runbook has a single verb and ordering lives in one place");
# this script enables every unit so fly.target's own Requires=/Wants=
# graph does the ordering, then verifies the health-critical ones actually
# came up rather than trusting `systemctl start` alone (Type=exec units in
# particular give no real readiness signal).
#
# flypush is enabled only when PUSH_TARGET=twitch (docs/design/infra.md
# section 4: "local: 07-enable.sh leaves flypush disabled"). flybridge is
# always enabled (Wants=, not Requires=, in fly.target — "chat must not be
# able to stop the sim"); if services/bridge is not built yet its unit
# will fail to start, which is expected and logged, not fatal, matching
# that same Wants semantics.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -eq 1 ] || die "usage: $0 ENVFILE"
load_env "$1"
require_pve_host
need pct

ALWAYS_ON_UNITS="xvfb.service pulse.service mediamtx.service flysim.service flystage-web.service flystage.service flycast.service flybridge.service"
ALWAYS_ON_TIMERS="fly-recap.timer fly-retention.timer fly-watchdog.timer"

log "07-enable: enabling app units (not yet starting)"
for u in $ALWAYS_ON_UNITS; do
    ct_exec "$CTID" -- systemctl enable "$u"
done

log "07-enable: enabling AND starting always-on timers"
# `enable` alone was not enough. A timer is WantedBy=timers.target, and
# timers.target is only reached at boot, so on a freshly provisioned
# container all three timers sat `enabled` but `inactive` until somebody
# rebooted it — measured on the P0 spike run 2, where `systemctl
# list-timers fly-watchdog.timer` listed nothing after a full provision run
# and a healthy stack. That means no watchdog (the entire self-healing
# story), no recap, and no media retention pruning on a container that is
# supposed to run for weeks without a reboot. `--now` starts them too.
for t in $ALWAYS_ON_TIMERS; do
    ct_exec "$CTID" -- systemctl enable --now "$t"
done
for t in $ALWAYS_ON_TIMERS; do
    if [ "$(ct_exec "$CTID" -- systemctl is-active "$t" || true)" != active ]; then
        die "$t did not become active after 'systemctl enable --now'; check: pct exec $CTID -- systemctl status $t"
    fi
done

if [ "$PUSH_TARGET" = twitch ]; then
    log "07-enable: PUSH_TARGET=twitch — enabling flypush.service and flypush-restart.timer"
    # test -s, not test -f: a failed 06-secrets.sh run used to be able to
    # leave a credential envelope wrapping an EMPTY secret behind (see that
    # script's own note, and the release container's provisioning run on 2026-09-16), and
    # `test -f` accepted it — flypush would then connect to Twitch with no
    # key and fail in a way that looks like a Twitch problem.
    if ! ct_exec "$CTID" -- test -s /etc/fly/creds/twitch-key.cred; then
        die "PUSH_TARGET=twitch but /etc/fly/creds/twitch-key.cred is missing or empty — run 06-secrets.sh first (FORCE_SECRETS=1 if PUSH_TARGET was local when it last ran), and check it says 'twitch-key installed'"
    fi
    ct_exec "$CTID" -- systemctl enable flypush.service
    # --now for the same reason as the always-on timers above: enable alone
    # leaves it inactive until the next boot.
    ct_exec "$CTID" -- systemctl enable --now flypush-restart.timer
else
    log "07-enable: PUSH_TARGET=local — leaving flypush.service and flypush-restart.timer disabled"
    ct_exec "$CTID" -- systemctl disable flypush.service 2>/dev/null || true
    ct_exec "$CTID" -- systemctl disable flypush-restart.timer 2>/dev/null || true
fi

log "07-enable: enabling fly.target"
ct_exec "$CTID" -- systemctl enable fly.target

log "07-enable: systemctl start fly.target"
ct_exec "$CTID" -- systemctl start fly.target

log "07-enable: health gate — flysim /healthz"
if ! ct_exec "$CTID" -- /opt/fly/bin/wait-for-health http://127.0.0.1:7401/healthz 60; then
    die "flysim did not become healthy within 60s; check: pct exec $CTID -- journalctl -u flysim -n 100"
fi

log "07-enable: health gate — flystage-web"
if ! ct_exec "$CTID" -- /opt/fly/bin/wait-for-health http://127.0.0.1:7402/ 30; then
    log "07-enable: WARNING flystage-web did not answer within 30s; check:" \
        "pct exec $CTID -- journalctl -u flystage-web -n 100 (non-fatal: stage/ may not be built yet)"
fi

log "07-enable: health gate — mediamtx API"
if ! ct_exec "$CTID" -- /opt/fly/bin/wait-for-health http://127.0.0.1:9997/v3/config/global/get 30; then
    die "mediamtx API did not answer within 30s; check: pct exec $CTID -- journalctl -u mediamtx -n 100"
fi

log "07-enable: unit summary"
ct_exec "$CTID" -- systemctl --no-pager --plain list-units 'xvfb.service' 'pulse.service' 'mediamtx.service' \
    'flysim.service' 'flystage-web.service' 'flystage.service' 'flycast.service' 'flypush.service' 'flybridge.service' || true

log "07-enable: done. flybridge and (if PUSH_TARGET=local) flypush are expected to be" \
    "absent/inactive at this point in the rollout — see docs/design/infra.md section 7" \
    "for the phase gates."
