#!/usr/bin/env bash
# infra/06-secrets.sh ENVFILE
#
# Reads secrets from `pass` on the OPERATOR box and pipes them straight
# into `pct exec CTID -- systemd-creds encrypt --name=... - DEST`. The
# secret never touches a file on the operator box or on the host, and never
# appears in an argv or in this script's own output — it travels on stdin
# only, and this script never runs `set -x`.
# docs/design/infra.md section 3 ("Secrets: exact paths and modes") and
# section 4's flip-to-Twitch command, which is the exact shape this script
# generalises:
#
#   pass twitch/<channel>-key | ssh root@<host-ip> \
#     'pct exec <release-ctid> -- systemd-creds encrypt --name=twitch-key - /etc/fly/creds/twitch-key.cred'
#
# `pass` lives on the operator box only (docs/design/infra.md section 3's
# secrets table: "source of truth ... WSL box only"), while `pct` only
# exists on the host. So: if this script is itself running on the host, it calls
# pct directly; otherwise it relays the single stdin pipe over ssh to
# SSH_TARGET (default root@<host-ip>), exactly as the design's own
# example does. Either way, `pass show` is the only place the secret is
# ever materialised, and it goes straight into a pipe.
#
# Deviation, documented: infra.md's secrets table lists a single
# `/etc/fly/creds/twitch-app.cred` for "bridge app creds", while
# docs/design/stage-bridge.md section B1 has flybridge read
# TWITCH_CLIENT_ID and TWITCH_CLIENT_SECRET as two separate
# LoadCredentialEncrypted= values sourced from
# twitch/fly-pokemon-client-{id,secret}. This script follows infra.md (the
# doc this task implements) and writes ONE twitch-app.cred containing both
# values as two lines ("<client_id>\n<client_secret>\n"); flybridge's own
# implementation (out of scope here, not yet written) will need to parse
# that shape, or the two docs need reconciling before P3. See the final
# report's deviations list.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPT_DIR/lib/common.sh"

[ $# -eq 1 ] || die "usage: $0 ENVFILE"
load_env "$1"
# `need pass` deliberately does NOT live here. A PUSH_TARGET=local container
# never reads a secret (the branch below skips the Twitch key outright), and
# provision.sh runs this step unconditionally — so requiring `pass` up front
# made step 6 die with "missing required command(s): pass" when provisioning
# on the host, where `pass` does not exist, and took the whole
# `provision.sh <dev-env>` run down with it before 07-enable could start
# anything. Found on the P0 spike, run 2. Each branch that actually reads from
# `pass` requires it itself.

: "${SSH_TARGET:=root@<host-ip>}"

if on_pve_host; then
    need pct
    remote_creds_encrypt() {
        # $1 = credential name, $2 = dest path inside the container; secret on stdin
        pct exec "$CTID" -- systemd-creds encrypt --name="$1" - "$2"
    }
    remote_ct_exec() { ct_exec "$CTID" -- "$@"; }
else
    need ssh
    log "06-secrets: not running on the host; relaying through ssh $SSH_TARGET, same shape as" \
        "docs/design/infra.md section 4's flip-to-Twitch example"
    remote_creds_encrypt() {
        # shellcheck disable=SC2029 # CTID/name/path are ours, not attacker input
        ssh "$SSH_TARGET" "pct exec $CTID -- systemd-creds encrypt --name=$1 - $2"
    }
    remote_ct_exec() {
        # shellcheck disable=SC2029
        ssh "$SSH_TARGET" "pct exec $CTID -- $*"
    }
fi

if [ "$PUSH_TARGET" = local ] && [ -z "${FORCE_SECRETS:-}" ]; then
    log "06-secrets: PUSH_TARGET=local and FORCE_SECRETS not set — skipping the Twitch key."
    log "06-secrets: local test mode does not need it (flypush stays disabled). Set FORCE_SECRETS=1 to install it anyway."
else
    need pass
    need_var PASS_KEY
    log "06-secrets: installing twitch-key from pass:${PASS_KEY}"

    # Read the secret BEFORE touching the container, and refuse on an empty
    # read. `pass show | remote_creds_encrypt` looks tidy but fails in the
    # worst possible order: if `pass` cannot decrypt (the GPG key is
    # passphrase-protected and nothing can prompt — pinentry in a
    # non-interactive shell dies with "Inappropriate ioctl for device", which
    # `pass` reports as "decryption failed: No secret key"), systemd-creds has
    # ALREADY encrypted an empty stdin on the other end of the pipe, and
    # `set -o pipefail` then kills this script before the chmod. What is left
    # behind on the release container was a 130-byte credential envelope wrapping nothing, at
    # mode 0644, which `07-enable.sh`'s `test -f` gate would have accepted as
    # a stream key — flypush would have gone to Twitch with an empty key.
    # Measured on the release container's provisioning run, 2026-09-16.
    #
    # The value lives in a shell variable and is written with `printf`, a
    # builtin: it never appears in an argv, in a file, or in this script's
    # output.
    secret="$(pass show "$PASS_KEY")" \
        || die "06-secrets: 'pass show ${PASS_KEY}' failed on this box, nothing written to CT ${CTID}. If it says \"decryption failed: No secret key\", the GPG key needs a passphrase and there is no terminal to ask on — re-run this from an interactive shell (or unlock the key first: 'pass show ${PASS_KEY} >/dev/null'). NEVER hand-write a key into the container instead."
    [ -n "$secret" ] \
        || die "06-secrets: 'pass show ${PASS_KEY}' returned EMPTY, nothing written to CT ${CTID}. Check the pass entry; an empty credential would be accepted by systemd-creds and would fail at Twitch instead."

    remote_ct_exec mkdir -p /etc/fly/creds
    remote_ct_exec chmod 0700 /etc/fly/creds
    printf '%s\n' "$secret" | remote_creds_encrypt twitch-key /etc/fly/creds/twitch-key.cred
    unset secret
    remote_ct_exec chmod 0400 /etc/fly/creds/twitch-key.cred
    remote_ct_exec chown root:root /etc/fly/creds/twitch-key.cred
    # And assert the far end actually has something. A zero-length or missing
    # file here means the relay dropped it silently.
    remote_ct_exec test -s /etc/fly/creds/twitch-key.cred \
        || die "06-secrets: /etc/fly/creds/twitch-key.cred is missing or empty inside CT ${CTID} after the install — the credential did not survive the relay; do NOT enable flypush"
    log "06-secrets: twitch-key installed (content never touched this script's stdout/argv)"
fi

# Bridge app creds: optional at this phase (flybridge is P3, per
# docs/stream-mvp-plan.md's rollout order) — install only if both pass
# entries already exist, so running this before the Twitch app is
# registered is a clean no-op rather than a hard failure.
BRIDGE_ID_KEY="${BRIDGE_ID_KEY:-twitch/helix-client-id}"
BRIDGE_SECRET_KEY="${BRIDGE_SECRET_KEY:-twitch/helix-client-secret}"
if command -v pass >/dev/null 2>&1 \
    && pass show "$BRIDGE_ID_KEY" >/dev/null 2>&1 \
    && pass show "$BRIDGE_SECRET_KEY" >/dev/null 2>&1; then
    log "06-secrets: installing twitch-app from pass:${BRIDGE_ID_KEY} + pass:${BRIDGE_SECRET_KEY}"
    # Same read-then-write order, and the same refusal, as the stream key
    # above: never let systemd-creds encrypt half a credential (or none).
    app_id="$(pass show "$BRIDGE_ID_KEY")" || die "06-secrets: 'pass show ${BRIDGE_ID_KEY}' failed after its own availability check; nothing written"
    app_secret="$(pass show "$BRIDGE_SECRET_KEY")" || die "06-secrets: 'pass show ${BRIDGE_SECRET_KEY}' failed after its own availability check; nothing written"
    if [ -z "$app_id" ] || [ -z "$app_secret" ]; then
        die "06-secrets: one of ${BRIDGE_ID_KEY} / ${BRIDGE_SECRET_KEY} is EMPTY; refusing to write a half credential to CT ${CTID}"
    fi
    printf '%s\n%s\n' "$app_id" "$app_secret" | remote_creds_encrypt twitch-app /etc/fly/creds/twitch-app.cred
    unset app_id app_secret
    remote_ct_exec chmod 0400 /etc/fly/creds/twitch-app.cred
    remote_ct_exec chown root:root /etc/fly/creds/twitch-app.cred
    remote_ct_exec test -s /etc/fly/creds/twitch-app.cred \
        || die "06-secrets: /etc/fly/creds/twitch-app.cred is missing or empty inside CT ${CTID} after the install"
    log "06-secrets: twitch-app installed"
else
    log "06-secrets: ${BRIDGE_ID_KEY} / ${BRIDGE_SECRET_KEY} not available, skipping twitch-app (expected before P3)." \
        "Three different reasons look identical here and only the last one is a problem: there is no 'pass' on this" \
        "box; the entries have not been created yet (the case on 2026-09-16 — only twitch/<channel>-key exists);" \
        "or 'pass' is installed and the entries exist but the GPG key cannot be unlocked without a terminal, in" \
        "which case EVERY entry reads as unavailable. Check by hand with: pass ls twitch"
fi

log "06-secrets: done. Be honest about what this buys: no TPM in an unprivileged LXC, so" \
    "systemd-creds falls back to /var/lib/systemd/credential.secret inside the container." \
    "Root in the container or on the host can still read it. The value is that the key is not" \
    "in the repo, the journal, any unit file, or /proc/*/environ for any process except the" \
    "one unit that declares LoadCredentialEncrypted=. See docs/runbook.md for rotation."
