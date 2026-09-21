#!/usr/bin/env bash
# infra/bin/dev-stage-deploy.sh CTID
#
# Redeploys ONLY apps/stage onto an already-provisioned dev container (ROLE=dev,
# e.g. The dev container fly-spike) — the "stage-only" flow described in
# docs/stream-mvp-plan.md ("every stage merge is redeployed to the dev container,
# The dev container, stage-only, flystage restart") and infra/README.md's dev/release split.
# Unlike infra/05-deploy.sh, this never touches flysim, bridge, units, app config,
# or the cpuset partition, and it never restarts flysim or flycast: the sim's
# checkpoint state and the encoder stay live across a stage-only redeploy.
#
# Runs on the OPERATOR BOX (WSL, not the host): builds apps/stage locally from this
# worktree's current HEAD (any commit, clean or not — the ROLE=dev behaviour),
# then does everything else over `ssh $PVE_HOST` + `pct`, exactly the way the
# first stage-only redeploy was done by hand (flybrain infra/docs/runbook.md's
# claim-log entry for the dev container, 2026-09-16 ~00:30-00:35 ET):
#
#   1. new release directory /opt/fly/releases/<sha>-<timestamp>/
#   2. flysim/bridge/data SYMLINKED to whatever /opt/fly/current's own
#      flysim/bridge/data already resolve to (readlink -f) — not rebuilt, not
#      copied, not restarted
#   3. the fresh apps/stage build plus infra/config/serve.mjs + serve.sh as
#      that release's stage/
#   4. atomic flip of /opt/fly/current (ln -sfn + mv -T)
#   5. `systemctl restart flystage-web` then `flystage`, in that order, timed
#   6. a CDP check against the running page's own websocket target
#      (window.__stage — presence and, when it reports hotRows, whether the
#      sprite pass' row span clears $SPRITE_SPAN_MIN of the grid), sampled
#      $SAMPLES times over $SAMPLE_INTERVAL seconds
#   7. an ffprobe check, run from THIS box, that the container's own MediaMTX
#      HLS output is still playing
#
# Claims and releases the container in the host's agent claim log itself, when the
# operator has told it where that is: set AGENT_CLAIM_LOG to the path on the host
# (CLAUDE.md, "one agent at a time, claim before you touch it, release when done").
# Leave it unset and the claim is SKIPPED rather than written somewhere invented —
# no path to the operator's host is baked into this repo. No other guest is ever
# named or touched, and this script refuses a CTID that has no /opt/fly/current (i.e. a
# container that has never had a full release deployed to it — run
# infra/05-deploy.sh first).
#
# Every scratch file this script creates, in HOST_STAGE_DIR on the host and inside the
# container's /tmp, is removed before it exits — including on failure, via the
# EXIT trap below.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INFRA_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

log() { printf '[%s] dev-stage-deploy: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" >&2; }
die() { printf '[%s] dev-stage-deploy: FATAL: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" >&2; exit 1; }
need() {
    local cmd missing=()
    for cmd in "$@"; do
        command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
    done
    [ "${#missing[@]}" -eq 0 ] || die "missing required command(s): ${missing[*]}"
}

usage() { echo "usage: $0 CTID" >&2; exit 2; }
[ $# -eq 1 ] || usage
CTID="$1"
[[ "$CTID" =~ ^[0-9]+$ ]] || die "CTID must be numeric, got '$CTID'"

: "${PVE_HOST:?set PVE_HOST to the ssh target of the Proxmox host}"
: "${HOST_STAGE_DIR:=/root}"
# Where to claim/release on the host. Unset means "skip the claim" — see the header.
: "${AGENT_CLAIM_LOG:=}"
: "${STAGE_NODE:=/opt/node-v22.14.0/bin/node}"
: "${STAGE_PORT:=7402}"
: "${CDP_PORT:=9222}"
: "${HLS_PORT:=8888}"
: "${WEBRTC_PORT:=8889}"
: "${STREAM_PATH:=live/fly}"
: "${SPRITE_SPAN_MIN:=0.40}"
: "${SAMPLES:=5}"
: "${SAMPLE_INTERVAL:=2}"

need git npm tar scp ssh ffprobe awk sed date mktemp node

# ---------------------------------------------------------------------------
# 1. Build apps/stage from this worktree's current HEAD.
# ---------------------------------------------------------------------------
cd "$REPO_ROOT"
SHA="$(git rev-parse --short=7 HEAD)"
if [ -n "$(git status --porcelain)" ]; then
    log "WARNING: $REPO_ROOT is dirty — deploying its current working-tree content anyway (ROLE=dev deploys any commit, clean or not; infra/README.md's dev/release split)"
fi
log "building apps/stage from ${SHA}"
npm ci
npm run build -w apps/stage

DIST_DIR="$REPO_ROOT/apps/stage/dist"
[ -f "$DIST_DIR/index.html" ] || die "build did not produce $DIST_DIR/index.html"

WORK_DIR="$(mktemp -d)"
REMOTE_TAG="dev-stage-deploy-${CTID}-${SHA}-$$"
TARBALL_NAME="${REMOTE_TAG}.tar.gz"
CDPJS_NAME="${REMOTE_TAG}-cdp-check.mjs"
cleanup() {
    rm -rf "$WORK_DIR"
    # Best-effort: if the remote orchestration died before its own cleanup
    # ran, do not leave scratch behind on the host or in the container either.
    ssh "$PVE_HOST" "rm -f '${HOST_STAGE_DIR}/${TARBALL_NAME}' '${HOST_STAGE_DIR}/${CDPJS_NAME}'" 2>/dev/null || true
    ssh "$PVE_HOST" "pct exec ${CTID} -- rm -f '/tmp/${TARBALL_NAME}' '/tmp/${CDPJS_NAME}'" 2>/dev/null || true
}
trap cleanup EXIT

PAYLOAD_DIR="$WORK_DIR/stage"
mkdir -p "$PAYLOAD_DIR"
cp -a "$DIST_DIR"/. "$PAYLOAD_DIR"/
cp "$INFRA_DIR/config/serve.mjs" "$PAYLOAD_DIR/serve.mjs"
cp "$INFRA_DIR/config/serve.sh" "$PAYLOAD_DIR/serve.sh"
chmod 0644 "$PAYLOAD_DIR/serve.mjs"
chmod 0755 "$PAYLOAD_DIR/serve.sh"
tar -czf "$WORK_DIR/$TARBALL_NAME" -C "$PAYLOAD_DIR" .

# ---------------------------------------------------------------------------
# 2. The CDP helper. Written fresh each run (never committed — this script is
#    the single source of truth), scp'd up alongside the tarball, pct pushed
#    into the container, and removed from both places before exit.
# ---------------------------------------------------------------------------
cat > "$WORK_DIR/$CDPJS_NAME" <<'CDPJS'
// dev-stage-deploy.sh's CDP helper. Usage: node cdp-check.mjs <url-substring>
// Finds the Chromium page target whose URL contains <url-substring>, asks it
// (over its own devtools websocket) whether window.__stage exists and what
// window.__stage.brainmap() reports, and prints one line of JSON to stdout.
const needle = process.argv[2] || '127.0.0.1:7402';

const res = await fetch('http://127.0.0.1:9222/json');
const targets = await res.json();
const target = targets.find((t) => t.type === 'page' && t.url && t.url.includes(needle));
if (!target) {
  console.error('NO_STAGE_PAGE');
  process.exit(1);
}

const expr = 'JSON.stringify({hasStage: !!window.__stage, hasBrainmap: !!(window.__stage && window.__stage.brainmap), stats: (window.__stage && window.__stage.brainmap) ? window.__stage.brainmap() : null, title: document.title, url: location.href})';

const ws = new WebSocket(target.webSocketDebuggerUrl);
ws.addEventListener('open', () => {
  ws.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression: expr, returnByValue: true, awaitPromise: true } }));
});
ws.addEventListener('message', (ev) => {
  const msg = JSON.parse(ev.data.toString());
  if (msg.id === 1) {
    console.log(msg.result.result.value);
    process.exit(0);
  }
});
ws.addEventListener('error', (e) => { console.error('WS_ERROR', e.message || e); process.exit(1); });
setTimeout(() => { console.error('TIMEOUT'); process.exit(1); }, 8000);
CDPJS

log "pushing build + CDP helper to ${PVE_HOST}:${HOST_STAGE_DIR}/"
scp -q "$WORK_DIR/$TARBALL_NAME" "$WORK_DIR/$CDPJS_NAME" "${PVE_HOST}:${HOST_STAGE_DIR}/"

# ---------------------------------------------------------------------------
# 3. Everything else happens on the host: claim, deploy, flip, restart, verify,
#    release, all in one ssh round trip so the container is never left half
#    converged between steps.
# ---------------------------------------------------------------------------
if [ -n "$AGENT_CLAIM_LOG" ]; then
    log "claiming CT ${CTID} in ${AGENT_CLAIM_LOG} on ${PVE_HOST}"
    CLAIM_LINE="$(date -u '+%Y-%m-%d %H:%M UTC') - dev-stage-deploy.sh: claiming CT ${CTID}, STAGE PAGE ONLY, redeploying apps/stage from ${SHA}. flysim/bridge/data untouched and unrestarted; only flystage-web then flystage restart. No other guest touched."
    printf '%s\n' "$CLAIM_LINE" | ssh "$PVE_HOST" "cat >> '$AGENT_CLAIM_LOG'"
else
    log "AGENT_CLAIM_LOG unset — SKIPPING the claim. Set it to the claim log's path on the host."
fi

set +e
REMOTE_OUT="$(ssh "$PVE_HOST" bash -s -- \
    "$CTID" "$SHA" "$TARBALL_NAME" "$CDPJS_NAME" "$STAGE_NODE" "$STAGE_PORT" "$CDP_PORT" "$SAMPLES" "$SAMPLE_INTERVAL" \
    "$HOST_STAGE_DIR" "$AGENT_CLAIM_LOG" \
    <<'REMOTE'
set -euo pipefail
CTID="$1"; SHA="$2"; TARBALL_NAME="$3"; CDPJS_NAME="$4"
NODE_BIN="$5"; STAGE_PORT="$6"; CDP_PORT="$7"; SAMPLES="$8"; SAMPLE_INTERVAL="$9"
HOST_STAGE_DIR="${10}"; AGENT_CLAIM_LOG="${11}"

log() { printf '[%s] dev-stage-deploy(the host): %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" >&2; }
die() { printf '[%s] dev-stage-deploy(the host): FATAL: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" >&2; exit 1; }

command -v pct >/dev/null 2>&1 || die "pct not found -- this must run on the host"
[ -e /etc/pve/local ] || die "this must run on the host (root), not in a container"
pct config "$CTID" >/dev/null 2>&1 || die "CT $CTID does not exist"
pct exec "$CTID" -- test -e /opt/fly/current \
    || die "CT $CTID has no /opt/fly/current -- this tool only redeploys the stage page onto a container that already has a full release. Run infra/05-deploy.sh first."

flysim_target="$(pct exec "$CTID" -- readlink -f /opt/fly/current/flysim)"
bridge_target="$(pct exec "$CTID" -- readlink -f /opt/fly/current/bridge)"
data_target="$(pct exec "$CTID" -- readlink -f /opt/fly/current/data)"
[ -n "$flysim_target" ] && [ -n "$bridge_target" ] && [ -n "$data_target" ] \
    || die "could not resolve flysim/bridge/data under /opt/fly/current"

REL="${SHA}-$(date -u +%Y%m%dT%H%M%SZ)"
RELDIR="/opt/fly/releases/${REL}"
log "new release ${RELDIR} -- flysim=${flysim_target} bridge=${bridge_target} data=${data_target}"

pct exec "$CTID" -- mkdir -p "${RELDIR}/stage"
pct push "$CTID" "${HOST_STAGE_DIR}/${TARBALL_NAME}" "/tmp/${TARBALL_NAME}" --perms 0644
pct exec "$CTID" -- tar -xzf "/tmp/${TARBALL_NAME}" -C "${RELDIR}/stage"
pct exec "$CTID" -- rm -f "/tmp/${TARBALL_NAME}"

pct exec "$CTID" -- ln -s "$flysim_target" "${RELDIR}/flysim"
pct exec "$CTID" -- ln -s "$bridge_target" "${RELDIR}/bridge"
pct exec "$CTID" -- ln -s "$data_target" "${RELDIR}/data"
pct exec "$CTID" -- chown -R fly:fly "$RELDIR"

pct exec "$CTID" -- sh -c "ln -sfn '$RELDIR' /opt/fly/current.tmp && mv -T /opt/fly/current.tmp /opt/fly/current"
echo "RELEASE=${REL}"
echo "FLIP_UTC=$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)"

t0="$(date -u +%s.%N)"
pct exec "$CTID" -- systemctl restart flystage-web
t1="$(date -u +%s.%N)"
pct exec "$CTID" -- systemctl restart flystage
t2="$(date -u +%s.%N)"
gap_web="$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')"
gap_total="$(awk -v a="$t0" -v c="$t2" 'BEGIN{printf "%.3f", c-a}')"
echo "RESTART_GAP_SECONDS=${gap_total}"
log "flystage-web restart ${gap_web}s, total gap ${gap_total}s -- flysim and flycast were never touched"

log "waiting for flystage's Chromium kiosk to publish a CDP target on :${CDP_PORT}"
pct push "$CTID" "${HOST_STAGE_DIR}/${CDPJS_NAME}" "/tmp/${CDPJS_NAME}" --perms 0644
found=0
for _ in $(seq 1 30); do
    if pct exec "$CTID" -- curl -fs "http://127.0.0.1:${CDP_PORT}/json" 2>/dev/null | grep -q "127.0.0.1:${STAGE_PORT}"; then
        found=1
        break
    fi
    sleep 1
done
[ "$found" -eq 1 ] || log "WARNING: no CDP page target for :${STAGE_PORT} appeared within 30s -- sampling anyway"

i=1
while [ "$i" -le "$SAMPLES" ]; do
    result="$(pct exec "$CTID" -- "$NODE_BIN" "/tmp/${CDPJS_NAME}" "127.0.0.1:${STAGE_PORT}" 2>&1 || true)"
    echo "STAGE_CHECK=${result}"
    i=$((i + 1))
    [ "$i" -le "$SAMPLES" ] && sleep "$SAMPLE_INTERVAL"
done
pct exec "$CTID" -- rm -f "/tmp/${CDPJS_NAME}"
rm -f "${HOST_STAGE_DIR}/${TARBALL_NAME}" "${HOST_STAGE_DIR}/${CDPJS_NAME}"

echo "CT_IP=$(pct exec "$CTID" -- hostname -I | awk '{print $1}')"

if [ -n "$AGENT_CLAIM_LOG" ]; then
    printf '%s\n' "$(date -u '+%Y-%m-%d %H:%M UTC') - dev-stage-deploy.sh: CT ${CTID} stage page redeployed, release ${REL}. flysim/bridge/data untouched, only flystage-web+flystage restarted. Host released." >> "$AGENT_CLAIM_LOG"
fi
log "done, host released"
REMOTE
)"
REMOTE_STATUS=$?
set -e
# The remote script's own log()/die() lines went to its stderr, which ssh
# already streamed live to this script's stderr above -- REMOTE_OUT here is
# only its stdout (the RELEASE=/STAGE_CHECK=/etc. lines this script parses).
[ "$REMOTE_STATUS" -eq 0 ] || die "remote deploy failed (exit ${REMOTE_STATUS}) -- see the log lines above; nothing here retries automatically"

RELEASE="$(printf '%s\n' "$REMOTE_OUT" | sed -n 's/^RELEASE=//p' | tail -1)"
RESTART_GAP="$(printf '%s\n' "$REMOTE_OUT" | sed -n 's/^RESTART_GAP_SECONDS=//p' | tail -1)"
CT_IP="$(printf '%s\n' "$REMOTE_OUT" | sed -n 's/^CT_IP=//p' | tail -1)"
[ -n "$RELEASE" ] && [ -n "$CT_IP" ] || die "could not parse the remote deploy's own output -- see the log above"

# ---------------------------------------------------------------------------
# 4. Verify the fix under test: window.__stage.brainmap() exists, and across
#    $SAMPLES live samples its hotRows.span (fraction of the grid) clears
#    $SPRITE_SPAN_MIN at least once.
# ---------------------------------------------------------------------------
VERDICT_SCRIPT="$WORK_DIR/verdict.cjs"
cat > "$VERDICT_SCRIPT" <<'VERDICTJS'
const threshold = Number(process.argv[2]);
const lines = require('fs').readFileSync(0, 'utf8').split('\n').filter(Boolean);
const parsed = lines.map((l) => { try { return JSON.parse(l); } catch { return null; } });
const hasBrainmap = parsed.some((p) => p && p.hasBrainmap);
const spans = parsed.filter((p) => p && p.stats && p.stats.hotRows).map((p) => p.stats.hotRows.span);
const maxSpan = spans.length ? Math.max(...spans) : null;
console.log(JSON.stringify({ hasBrainmap, samples: parsed.length, spans, maxSpan }));
process.exit(hasBrainmap && maxSpan !== null && maxSpan > threshold ? 0 : 1);
VERDICTJS

printf '%s\n' "$REMOTE_OUT" | sed -n 's/^STAGE_CHECK=//p' > "$WORK_DIR/stage-checks.jsonl"
set +e
VERDICT_JSON="$(node "$VERDICT_SCRIPT" "$SPRITE_SPAN_MIN" < "$WORK_DIR/stage-checks.jsonl")"
VERDICT_STATUS=$?
set -e
log "brainmap() verdict: $VERDICT_JSON"

# ---------------------------------------------------------------------------
# 5. Verify the stream is still live, from this box.
# ---------------------------------------------------------------------------
HLS_URL="http://${CT_IP}:${HLS_PORT}/${STREAM_PATH}/index.m3u8"
WEBRTC_URL="http://${CT_IP}:${WEBRTC_PORT}/${STREAM_PATH}"
log "ffprobe ${HLS_URL}"
FFPROBE_OUT="$(timeout 20 ffprobe -v error -show_entries stream=codec_name,codec_type -of default=noprint_wrappers=1 "$HLS_URL" 2>&1)" || die "ffprobe against ${HLS_URL} failed: ${FFPROBE_OUT}"
echo "$FFPROBE_OUT" | grep -q 'codec_type=video' || die "ffprobe against ${HLS_URL} reported no video stream"
echo "$FFPROBE_OUT" | grep -q 'codec_type=audio' || die "ffprobe against ${HLS_URL} reported no audio stream"

# ---------------------------------------------------------------------------
# 6. Report.
# ---------------------------------------------------------------------------
echo "=================================================================="
echo "release:        ${RELEASE}"
echo "restart gap:     ${RESTART_GAP}s (flystage-web then flystage; flysim and flycast untouched)"
echo "brainmap() verdict: ${VERDICT_JSON}"
echo "stage page URL (in-container): http://127.0.0.1:${STAGE_PORT}/?mode=live"
echo "HLS URL:    ${HLS_URL}"
echo "WebRTC URL: ${WEBRTC_URL}"
echo "=================================================================="

[ "$VERDICT_STATUS" -eq 0 ] || die "brainmap() did not clear ${SPRITE_SPAN_MIN} row-span across ${SAMPLES} samples -- see the verdict above"
