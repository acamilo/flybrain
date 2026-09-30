#!/usr/bin/env bash
# tools/shadow-rehearsal.sh -- SHADOW-01's offline rehearsal: the real `flysim` service and the
# real `fly-shadow` side by side on one machine, exactly as the release container runs them, from
# one FLYSIM01 checkpoint, with sugar posted to the control API and service restarts.
#
#   FLY_ROM=<cartridge> tools/shadow-rehearsal.sh --checkpoint FILE [options]
#
# Options:
#   --checkpoint FILE       the live checkpoint the fly starts from (required)
#   --brain-minutes N       stop the live service after N brain minutes (default 30)
#   --restart-at LIST       restart flysim at these brain minutes, comma separated (default: none)
#   --sugar-every S         POST /stimulate about every S wall seconds (default 20; 0 = never)
#   --speed X               flysim loop.speed (default 0 = unthrottled; 1 = real time)
#   --live-threads N        flysim RAYON_NUM_THREADS (default 3, the release value)
#   --mode M --threads N    the shadow's execution mode and agent threads (in-process, 2)
#   --ledgers N             FLY_TRACE_LEDGERS (default 1: every boundary)
#   --out DIR               where everything goes (default: a new temporary directory)
#   --bin DIR               flysim, fly-shadow (and fly-session for --mode process); default
#                           $CARGO_TARGET_DIR/release or services/flysim/target/release
#   --port N                first of three local ports (default 17400)
#   --guard S               the shadow's --lag-guard-margin against the live metrics (0.05, as
#                           on the container; 0 disables)
#
# Environment: FLY_ROM (required); FLYSIM_PATHS_DATASET (default data/fafb-v783);
# FLY_ACCEPT_ADAPTERS passes through. Nothing here touches a host: every path is under --out.
#
# Result: <out>/shadow/verdict.json (and divergence.json on a divergence) and <out>/summary.txt:
# live and shadow CPU seconds per wall second, live frames per wall second, the verdict.
set -euo pipefail

die() { echo "shadow-rehearsal: $*" >&2; exit 2; }
repo="$(cd "$(dirname "$0")/.." && pwd)"
checkpoint=""; brain_minutes=30; restart_at=""; sugar_every=20; speed=0; live_threads=3
mode=in-process; threads=2; ledgers=1; out=""; bin=""; port=17400; guard=0.05
while [ $# -gt 0 ]; do
    case "$1" in
        --checkpoint) checkpoint="$2"; shift 2 ;;
        --brain-minutes) brain_minutes="$2"; shift 2 ;;
        --restart-at) restart_at="$2"; shift 2 ;;
        --sugar-every) sugar_every="$2"; shift 2 ;;
        --speed) speed="$2"; shift 2 ;;
        --live-threads) live_threads="$2"; shift 2 ;;
        --mode) mode="$2"; shift 2 ;;
        --threads) threads="$2"; shift 2 ;;
        --ledgers) ledgers="$2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        --bin) bin="$2"; shift 2 ;;
        --port) port="$2"; shift 2 ;;
        --guard) guard="$2"; shift 2 ;;
        *) die "unknown option $1" ;;
    esac
done
[ -n "$checkpoint" ] && [ -f "$checkpoint" ] || die "--checkpoint FILE is required"
: "${FLY_ROM:?FLY_ROM must name the cartridge}"
[ -n "$bin" ] || bin="${CARGO_TARGET_DIR:-$repo/services/flysim/target}/release"
for b in flysim fly-shadow; do [ -x "$bin/$b" ] || die "no $bin/$b (cargo build --release --bins)"; done
dataset="${FLYSIM_PATHS_DATASET:-$repo/data/fafb-v783}"
[ -n "$out" ] || out="$(mktemp -d "${TMPDIR:-/tmp}/shadow-rehearsal.XXXXXX")"
live="$out/live"; shadow_dir="$out/shadow"
mkdir -p "$live/hot" "$live/durable" "$live/trace" "$shadow_dir"
cp "$checkpoint" "$live/durable/1.checkpoint"
printf '{"generation":1,"latest":1,"previous":null,"archives":{}}\n' > "$live/durable/manifest.json"

# The live service's configuration; the shadow reads the same (Config::load, as on the container).
export FLYSIM_PATHS_ROM="$FLY_ROM" FLYSIM_PATHS_DATASET="$dataset" FLYSIM_LOOP_GAME=pokemon-red
export FLY_STATE="$live/durable" FLY_STATE_HOT="$live/hot" FLY_MACRO_MODE=macros
export FLY_FEED_BIND="127.0.0.1:$port" FLY_CONTROL_BIND="127.0.0.1:$((port + 1))"
export FLY_METRICS_ADDR="127.0.0.1:$((port + 2))" FLY_CHAT_ENABLED=false
export FLYSIM_LOOP_SPEED="$speed"
control="http://127.0.0.1:$((port + 1))"

cpu_seconds() { # utime+stime of a pid and its children that have been reaped, in seconds
    awk -v hz="$(getconf CLK_TCK)" '{ print ($14 + $15 + $16 + $17) / hz }' "/proc/$1/stat" 2>/dev/null || echo 0
}
status_ms() { # brain ms of a running (not booting) service, else empty
    curl -fsS --max-time 2 "$control/status" 2>/dev/null | python3 -c '
import json, sys
v = json.load(sys.stdin)
if v.get("status") == "running" and v.get("brainMs"):
    print(int(v["brainMs"]))' 2>/dev/null || true; }

live_pid=""; live_n=0; live_cpu=0
start_live() {
    live_n=$((live_n + 1))
    RAYON_NUM_THREADS="$live_threads" FLY_TRACE_DIR="$live/trace" FLY_TRACE_LEDGERS="$ledgers" \
        "$bin/flysim" > "$live/flysim-$live_n.log" 2>&1 &
    live_pid=$!
    for _ in $(seq 1 600); do
        [ -n "$(status_ms)" ] && return 0
        kill -0 "$live_pid" 2>/dev/null || { tail -20 "$live/flysim-$live_n.log" >&2; die "flysim exited"; }
        sleep 0.5
    done
    die "flysim did not come up"
}
stop_live() {
    local c; c=$(cpu_seconds "$live_pid"); live_cpu=$(awk -v a="$live_cpu" -v b="$c" 'BEGIN{print a+b}')
    kill -TERM "$live_pid"; wait "$live_pid" || true
}

wall0=$(date +%s.%N)
"$bin/fly-shadow" run --out "$shadow_dir" --trace-dir "$live/trace" --all-files --keep-traces --keep-spool \
    --mode "$mode" --threads "$threads" --lag-guard-margin "$guard" \
    --required-brain-seconds "$((brain_minutes * 60))" > "$out/shadow.log" 2>&1 &
shadow_pid=$!
# flysim starts no trace without the shadow's heartbeat.
for _ in $(seq 1 120); do [ -f "$live/trace/consumer" ] && break; sleep 0.5; done
[ -f "$live/trace/consumer" ] || die "fly-shadow wrote no heartbeat"
start_live
echo "shadow-rehearsal: $out (flysim $live_pid, fly-shadow $shadow_pid)"

brain_done=0; seg_start="$(status_ms)"; seg_last="$seg_start"; next_sugar=$(date +%s); sugar_posted=0
IFS=',' read -r -a restarts <<< "$restart_at"
restart_i=0
while :; do
    sleep 1
    ms="$(status_ms)"; [ -n "$ms" ] && seg_last="$ms"
    elapsed=$(( brain_done + seg_last - seg_start ))
    kill -0 "$shadow_pid" 2>/dev/null || { echo "shadow-rehearsal: fly-shadow exited early" >&2; break; }
    if [ "$sugar_every" != 0 ] && [ "$(date +%s)" -ge "$next_sugar" ]; then
        duration=$(( 200 + RANDOM % 800 ))
        code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 -X POST "$control/stimulate" \
            -H 'content-type: application/json' \
            -d "{\"durationMs\":$duration,\"by\":\"rehearsal\",\"source\":\"operator\"}" || true)
        case "$code" in 2*) sugar_posted=$((sugar_posted + 1)) ;; esac
        next_sugar=$(( $(date +%s) + sugar_every / 2 + RANDOM % (sugar_every + 1) ))
    fi
    if [ "$restart_i" -lt "${#restarts[@]}" ] && [ -n "${restarts[$restart_i]}" ] \
        && [ "$elapsed" -ge $(( ${restarts[$restart_i]} * 60000 )) ]; then
        echo "shadow-rehearsal: restarting flysim at brain $((elapsed / 1000)) s"
        stop_live; brain_done=$elapsed
        start_live; seg_start="$(status_ms)"; seg_last="$seg_start"
        restart_i=$((restart_i + 1))
    fi
    [ "$elapsed" -ge $(( brain_minutes * 60000 )) ] && break
done
stop_live
live_wall=$(awk -v a="$wall0" -v b="$(date +%s.%N)" 'BEGIN{print b-a}')
total=$(cat "$live"/trace/trace-*.jsonl | grep -c '"behaviour"' || true)
echo "shadow-rehearsal: live stopped after $((elapsed / 1000)) brain s, $total transitions traced; waiting for the shadow"
while kill -0 "$shadow_pid" 2>/dev/null; do
    compared=$(python3 -c 'import json,sys; v=json.load(open(sys.argv[1])); print(v["compared"]["transitions"] + sum(s["transitionsCompared"] for s in v["skipped"]))' "$shadow_dir/verdict.json" 2>/dev/null || echo 0)
    [ "$compared" -ge "$total" ] && break
    sleep 2
done
shadow_cpu=$(cpu_seconds "$shadow_pid")
shadow_wall=$(awk -v a="$wall0" -v b="$(date +%s.%N)" 'BEGIN{print b-a}')
kill -TERM "$shadow_pid" 2>/dev/null || true
rc=0; wait "$shadow_pid" || rc=$?
{
    echo "checkpoint: $(basename "$checkpoint")"
    echo "live: $live_n process(es), $total transitions, $((elapsed / 1000)) brain s, ${live_wall%.*} wall s, sugar posted $sugar_posted"
    awk -v c="$live_cpu" -v w="$live_wall" -v t="$total" 'BEGIN{printf "live: %.2f cores, %.1f frames per wall second\n", c/w, t/w}'
    awk -v c="$shadow_cpu" -v w="$shadow_wall" -v t="$total" 'BEGIN{printf "shadow (%s, %s threads): %.2f cores over %.0f wall s, %.1f frames per wall second\n", "'"$mode"'", "'"$threads"'", c/w, w, t/w}'
    echo "shadow exit: $rc"
    python3 - "$shadow_dir/verdict.json" <<'PY'
import json, sys
v = json.load(open(sys.argv[1]))
c = v["compared"]
print(f"verdict: {v['status']} ({v['reason']})")
print(f"compared: {c['transitions']} transitions, {c['brainSeconds']} brain s, {c['segments']} segments, "
      f"{c['sugar']} sugar, {c['rewards']} rewards {c['rewardKinds']}, {c['macroEvents']} macro events, "
      f"{c['slotSaves']} slot saves, {c['rollbacks']} rollbacks, {c['ledgerChecks']} ledger checks, "
      f"checkpoints {c['checkpoints']}")
print(f"skipped: {v['skipped']}")
print(f"cost: {v['cost']}")
if v["firstDivergence"]:
    print(f"FIRST DIVERGENCE: {json.dumps(v['firstDivergence'])}")
PY
} | tee "$out/summary.txt"
