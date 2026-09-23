#!/usr/bin/env bash
# infra/tests/lint.sh — run from anywhere; validates every script and unit
# file in infra/. Quality bar from the infra task: shellcheck-clean (or
# bash -n if shellcheck is not installed), every unit file parses
# (systemd-analyze verify if available), and a unit-file sanity grep
# (every ExecStart/ExecStartPre/ExecStartPost references an existing path
# in the tree or /usr/bin, /usr/local/bin, /usr/sbin, /bin, /sbin).
set -euo pipefail

INFRA_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAILED=0

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; FAILED=1; }

# ---------------------------------------------------------------------------
# 1. Shell scripts: shellcheck if present, else bash -n.
# ---------------------------------------------------------------------------
mapfile -t SHELL_SCRIPTS < <(
    {
        find "$INFRA_DIR" -maxdepth 1 -name '*.sh' -type f
        find "$INFRA_DIR/lib" -name '*.sh' -type f
        find "$INFRA_DIR/build" -name '*.sh' -type f
        find "$INFRA_DIR/tests" -name '*.sh' -type f
        # bin/ has no extensions, so this picks up flystage-launch,
        # flycast-launch, fly-watchdog and friends by shebang below.
        find "$INFRA_DIR/bin" -type f
        find "$INFRA_DIR/config" -name '*.sh' -type f
        # host/ runs on the host itself rather than in a container, and is
        # installed by hand — which is exactly why it needs linting here:
        # nothing else ever exercises it before it runs as root, at boot,
        # against /etc/pve.
        find "$INFRA_DIR/host" -name '*.sh' -type f
    } | sort -u
)

if command -v shellcheck >/dev/null 2>&1; then
    echo "--- shellcheck ---"
    for f in "${SHELL_SCRIPTS[@]}"; do
        # bin/ scripts have no extension; only lint files that are actually
        # shell (skip serve.mjs/serve.sh's mjs sibling and anything without
        # a bash/sh shebang).
        head -1 "$f" 2>/dev/null | grep -qE '^#!.*(bash|sh)' || continue
        # -P gives shellcheck an explicit search path for `# shellcheck
        # source=...` directives; older shellcheck (0.8.x, this repo's own
        # dev box) resolves those relative to $PWD instead of the file's
        # own directory, which would otherwise falsely fail SC1091 on
        # every script that sources lib/common.sh.
        # -S warning: fail on warning/error only. Every remaining
        # info-level finding at review time was one of: SC2153 (CTID,
        # PUSH_TARGET etc. genuinely come from a dynamically-sourced
        # an env file via load_env, not a typo), SC2029 (fly-backup-stage's
        # ssh calls deliberately expand $DEST/$base client-side — they are
        # our own trusted values, not remote input), or SC2015 (A && B || C
        # used deliberately as "run B if possible, never fail the caller"
        # in fly-watchdog/build-flysim.sh, not as if/else). style/info noise
        # is not the same bar as shellcheck-clean at the warning level.
        if shellcheck -x -P "$INFRA_DIR" -S warning "$f"; then
            pass "shellcheck: $f"
        else
            fail "shellcheck: $f"
        fi
    done
else
    echo "--- shellcheck not installed, falling back to bash -n ---"
    for f in "${SHELL_SCRIPTS[@]}"; do
        head -1 "$f" 2>/dev/null | grep -qE '^#!.*(bash|sh)' || continue
        if bash -n "$f" 2>&1; then
            pass "bash -n: $f"
        else
            fail "bash -n: $f"
        fi
    done
fi

# ---------------------------------------------------------------------------
# 2. Unit files: systemd-analyze verify if available, else a structural
#    grep (every unit has [Unit]/[Service] or [Timer], every Type= is
#    valid).
# ---------------------------------------------------------------------------
# host/*.service is a HOST unit (installed by hand on the host, not pushed to
# any container), but it is still a unit file and still has to parse.
mapfile -t UNIT_FILES < <(
    {
        find "$INFRA_DIR/units" -type f
        find "$INFRA_DIR/host" -name '*.service' -type f
    } | sort
)

if command -v systemd-analyze >/dev/null 2>&1; then
    echo "--- systemd-analyze verify ---"
    # systemd-analyze verify wants the units addressable by their real
    # names, and resolves ExecStart paths against the *host* filesystem
    # (it does not know about the container). It is still useful for unit
    # *syntax* validation, so we point it at a scratch dir under the unit's
    # own name and accept "unit not found"/missing-binary style complaints
    # about paths that only exist inside the CT (e.g. /opt/fly/...,
    # /usr/bin/chromium if this host has none) — those are reported
    # separately below, not as a lint failure here. A real parse error
    # (bad directive, bad section, bad Type=) is.
    for f in "${UNIT_FILES[@]}"; do
        name="$(basename "$f")"
        out="$(systemd-analyze verify "$f" 2>&1 || true)"
        # `verify` loads the whole host's real unit set for dependency
        # resolution context, so its stderr is full of noise about unit
        # files that have nothing to do with ours (this lint box's own
        # netplan/snapd units, etc). Keep only lines that actually
        # mention our unit, then drop the two classes of "expected" noise
        # within those: missing binaries and missing dependency units,
        # both of which only exist inside the container, never on the box
        # running this lint.
        mine="$(echo "$out" | grep -F "${name}:" || true)"
        # LoadCredentialEncrypted= needs systemd >= 250; this lint may run
        # on an older host (this repo's dev box is systemd 249) while the
        # real target (Debian 13 trixie) ships systemd 256+. An "Unknown
        # key name" warning for exactly that directive is a lint-host
        # limitation, not a unit-file bug — filtered accordingly.
        real_errors="$(echo "$mine" | grep -vE "(No such file or directory|is not executable|Failed to load environment files|Unit .* not found|Cannot add dependency|does not exist|Unknown key name 'LoadCredentialEncrypted')" || true)"
        if [ -z "$real_errors" ]; then
            pass "systemd-analyze verify: $name"
        else
            fail "systemd-analyze verify: $name"
            # sed, not a bash parameter expansion, because this needs a
            # per-line prefix over a multi-line string.
            # shellcheck disable=SC2001
            echo "$real_errors" | sed 's/^/    /'
        fi
    done
else
    echo "--- systemd-analyze not installed, skipping ---"
fi

# ---------------------------------------------------------------------------
# 3. ExecStart* path sanity: every ExecStart/ExecStartPre/ExecStartPost
#    directive's binary either exists in the tree (relative to /opt/fly,
#    i.e. checked against infra/bin or infra/config's serve.sh, since
#    those are what get deployed there) or is one of the standard system
#    paths.
# ---------------------------------------------------------------------------
echo "--- ExecStart* path sanity ---"
check_exec_path() {
    local unit="$1" line="$2"
    # Extract the first whitespace-separated token after the directive,
    # stripping a leading '-' (systemd's "failure is ok" marker) and any
    # /bin/sh -c '...' wrapper (checked as a shell built-in, always ok).
    local cmd
    cmd="$(echo "$line" | sed -E 's/^Exec(Start|StartPre|StartPost)=//' | sed -E 's/^-//' | awk '{print $1}')"
    [ -z "$cmd" ] && return 0

    case "$cmd" in
        /bin/sh|/usr/bin/env|/bin/bash)
            return 0 ;;  # wrapper; whatever it execs is checked by hand, not by this grep
        /usr/local/sbin/*)
            # Host-side scripts installed by hand from infra/host (see
            # infra/host/README.md). Unlike the other system paths this one
            # IS checkable: the file has to exist in this repo, or the
            # install instructions point at something that does not exist.
            local hostbase="${cmd#/usr/local/sbin/}"
            if [ -f "$INFRA_DIR/host/$hostbase" ]; then
                pass "$unit: $cmd -> infra/host/$hostbase exists"
            else
                fail "$unit: $cmd -> infra/host/$hostbase NOT FOUND"
            fi
            return 0 ;;
        /usr/bin/*|/usr/local/bin/*|/usr/sbin/*|/bin/*|/sbin/*)
            # Standard system path. We cannot assert it exists on THIS box
            # (chromium/ffmpeg/mediamtx/node are installed in the CT, not
            # here), so this class always passes the sanity grep — it is
            # a real path shape, not a typo'd /opt path.
            pass "$unit: $cmd (standard system path)"
            return 0 ;;
        /opt/fly/*)
            # Must correspond to something this repo actually deploys
            # there. /opt/fly/bin/X -> infra/bin/X. /opt/fly/current/... is
            # populated at deploy time from the release tarball or
            # infra/config/serve.{mjs,sh} and cannot be checked against a
            # static tree path, so it is reported informationally only.
            local rel="${cmd#/opt/fly/}"
            case "$rel" in
                bin/*)
                    local base="${rel#bin/}"
                    if [ -f "$INFRA_DIR/bin/$base" ]; then
                        pass "$unit: $cmd -> infra/bin/$base exists"
                    else
                        fail "$unit: $cmd -> infra/bin/$base NOT FOUND"
                    fi
                    ;;
                current/*)
                    echo "INFO: $unit: $cmd is populated at deploy time (release artifact or infra/config/serve.*), not statically checkable"
                    ;;
                *)
                    fail "$unit: $cmd does not match a known /opt/fly/{bin,current}/... shape"
                    ;;
            esac
            return 0 ;;
        *)
            fail "$unit: ExecStart* path '$cmd' is neither a standard system path nor under /opt/fly"
            return 0 ;;
    esac
}

for f in "${UNIT_FILES[@]}"; do
    name="$(basename "$f")"
    while IFS= read -r line; do
        check_exec_path "$name" "$line"
    done < <(grep -E '^Exec(Start|StartPre|StartPost)=' "$f" || true)
done

# ---------------------------------------------------------------------------
# 4. Behavioural smoke tests for the two pieces that assemble something
#    rather than just calling a binary. Still no the host, no pct, no
#    container: both are driven entirely through their own dry-run paths
#    and a temp directory.
# ---------------------------------------------------------------------------
# ---------------------------------------------------------------------------
# 3b. XML config files parse.
#
# config/fonts-local.conf is XML, and fontconfig rejects the WHOLE file on a
# well-formedness error — then falls back to its defaults with nothing but a
# warning on fc-cache's stderr. On the release container's provisioning run that is exactly
# what happened: a `--` inside the XML comment (the leading dashes of a
# Chromium flag name) made line 6 invalid, so the grayscale-antialiasing
# settings never applied and the stream would have had the colour fringing
# the file exists to prevent. Nothing noticed until someone read a scrollback.
# ---------------------------------------------------------------------------
echo "--- XML config well-formedness ---"
for f in "$INFRA_DIR"/config/*; do
    [ -f "$f" ] || continue
    head -1 "$f" | grep -q '^<?xml' || continue
    if command -v xmllint >/dev/null 2>&1; then
        if out="$(xmllint --noout "$f" 2>&1)"; then
            pass "xmllint: $f"
        else
            fail "xmllint: $f -- $out"
        fi
        continue
    fi
    # No xmllint on this box (nor on the host, checked 2026-09-16), and no
    # python3 either in the general case — so check the one error class that
    # has actually bitten, by hand: a double hyphen inside an XML comment.
    # It is illegal, it is easy to write (every long CLI flag starts with
    # one), and fontconfig's only complaint is a line on fc-cache's stderr.
    if awk '
        { line = $0 }
        {
            while (length(line)) {
                if (!incomment) {
                    i = index(line, "<!--")
                    if (i == 0) break
                    line = substr(line, i + 4)
                    incomment = 1
                } else {
                    j = index(line, "-->")
                    seg = (j ? substr(line, 1, j - 1) : line)
                    if (index(seg, "--")) {
                        printf "line %d: double hyphen inside an XML comment: %s\n", FNR, $0
                        bad = 1
                    }
                    if (j) { line = substr(line, j + 3); incomment = 0 } else break
                }
            }
        }
        END { exit bad ? 1 : 0 }
    ' "$f"; then
        pass "no double hyphen inside an XML comment: $f"
    else
        fail "XML comment in $f contains '--', which makes the file not well-formed; fontconfig will reject ALL of it with only an fc-cache warning"
    fi
done

echo "--- flycast-launch --print ---"
FLYCAST_LAUNCH="$INFRA_DIR/bin/flycast-launch"
check_print() {
    local backend="$1" want="$2" out
    if ! out="$(FLY_ENCODER="$backend" "$FLYCAST_LAUNCH" --print "$backend" 2>&1)"; then
        fail "flycast-launch --print $backend exited nonzero: $out"
        return 0
    fi
    case "$out" in
        *"$want"*) : ;;
        *) fail "flycast-launch --print $backend does not mention '$want'"; return 0 ;;
    esac
    # The gpu.md section 7 item 13 dup/drop fix has to be on BOTH encoders,
    # or the measurement is confounded by the encoder swap.
    case "$out" in
        *"fps=${FLY_FPS:-30}:round=near"*) : ;;
        *) fail "flycast-launch --print $backend is missing 'fps=30:round=near' (gpu.md section 7 item 13)"; return 0 ;;
    esac
    case "$out" in
        *"-fps_mode:v cfr"*) : ;;
        *) fail "flycast-launch --print $backend is missing '-fps_mode:v cfr'"; return 0 ;;
    esac
    # A trailing `-r 30` would reintroduce exactly the double rate
    # decision the fix removes.
    case "$out" in
        *" -r 30"*) fail "flycast-launch --print $backend still passes '-r 30' alongside -fps_mode:v cfr"; return 0 ;;
    esac
    pass "flycast-launch --print $backend: $want, fps filter, cfr, no stray -r"
}
check_print nvenc h264_nvenc
check_print x264 libx264
if out="$(FLY_ENCODER=vaapi "$FLYCAST_LAUNCH" --print 2>&1)"; then
    fail "flycast-launch accepted an unknown FLY_ENCODER ('vaapi') instead of failing: $out"
else
    pass "flycast-launch rejects an unknown FLY_ENCODER"
fi

echo "--- flystage-launch profile switch (default|gpu|vgl) ---"
# flystage-launch execs Chromium, so it cannot be run for real here. What is
# testable without a container is the profile switch's decisions: which flag
# file each profile picks, that an unknown profile is refused rather than
# silently defaulting, and that `vgl` refuses to start when vglrun is missing
# instead of broadcasting a page with no GL (docs/design/gpu.md section 3 (d),
# infra/docs/virtualgl-spike.md). CHROMIUM_FLAGS_FILE is pointed at a file that
# does not exist, so the script reaches its own "cannot read" exit 1 right
# after the switch and never reaches the exec.
STAGE_LAUNCH="$INFRA_DIR/bin/flystage-launch"
check_profile() {
    local profile="$1" want_file="$2" out
    out="$(env -u CHROMIUM_FLAGS_FILE FLY_CHROMIUM_PROFILE="$profile" "$STAGE_LAUNCH" 2>&1 || true)"
    case "$out" in
        *"cannot read $want_file"*) pass "flystage-launch profile '$profile' selects $want_file" ;;
        *) fail "flystage-launch profile '$profile' did not select $want_file; said: $out" ;;
    esac
}
check_profile default /etc/fly/chromium-flags
check_profile gpu /etc/fly/chromium-flags.gpu
if [ -x /usr/bin/vglrun ]; then
    check_profile vgl /etc/fly/chromium-flags.vgl
else
    # No VirtualGL on the box running the linter (the WSL dev box, normally),
    # which is the other half of the contract: refuse, do not fall back.
    out="$(FLY_CHROMIUM_PROFILE=vgl "$STAGE_LAUNCH" 2>&1 || true)"
    case "$out" in
        *"vglrun is missing"*) pass "flystage-launch profile 'vgl' refuses to start without vglrun" ;;
        *) fail "flystage-launch profile 'vgl' did not refuse a missing vglrun; said: $out" ;;
    esac
fi
out="$(FLY_CHROMIUM_PROFILE=swiftshader "$STAGE_LAUNCH" 2>&1 || true)"
case "$out" in
    *"must be 'default', 'gpu' or 'vgl'"*) pass "flystage-launch rejects an unknown FLY_CHROMIUM_PROFILE" ;;
    *) fail "flystage-launch accepted an unknown FLY_CHROMIUM_PROFILE instead of failing: $out" ;;
esac

echo "--- host/fly-nvidia-majors.sh sentinel-block rewrite ---"
MAJORS_SH="$INFRA_DIR/host/fly-nvidia-majors.sh"
lint_tmp="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint.XXXXXX")"
mkdir -p "$lint_tmp/lxc"
printf 'Character devices:\n195 nvidia\n195 nvidia-modeset\n195 nvidiactl\n236 nvidia-caps\n511 nvidia-uvm\n' > "$lint_tmp/devices"
# A conf with a [snapshot] section, because appending lxc.* keys after one
# would put them somewhere PVE ignores — silently.
printf 'arch: amd64\ncores: 8\nhostname: fly-lint\n\n[snap1]\narch: amd64\n' > "$lint_tmp/lxc/900.conf"
majors_env=(env "PROC_DEVICES=$lint_tmp/devices" "LXC_CONF_DIR=$lint_tmp/lxc")

run1="$("${majors_env[@]}" "$MAJORS_SH" 900 2>/dev/null || true)"
run2="$("${majors_env[@]}" "$MAJORS_SH" 900 2>/dev/null || true)"
if [ "$run1" = "900 changed" ] && [ "$run2" = "900 unchanged" ]; then
    pass "fly-nvidia-majors.sh: first run changed, second run unchanged (idempotent)"
else
    fail "fly-nvidia-majors.sh: expected 'changed' then 'unchanged', got '$run1' then '$run2'"
fi
if grep -q '^lxc.cgroup2.devices.allow: c 511:\* rwm$' "$lint_tmp/lxc/900.conf" \
   && grep -q '^lxc.cgroup2.devices.allow: c 236:\* rwm$' "$lint_tmp/lxc/900.conf" \
   && grep -q '^lxc.mount.entry: /dev/nvidia-caps dev/nvidia-caps none bind,optional,create=dir$' "$lint_tmp/lxc/900.conf"; then
    pass "fly-nvidia-majors.sh: wrote the live uvm/caps majors and the caps directory bind"
else
    fail "fly-nvidia-majors.sh: the generated block is missing the expected majors/binds"
fi
if [ "$(awk '/^\[snap1\]/{print NR; exit}' "$lint_tmp/lxc/900.conf")" -gt "$(awk '/^# END fly-nvidia$/{print NR; exit}' "$lint_tmp/lxc/900.conf")" ]; then
    pass "fly-nvidia-majors.sh: block sits BEFORE the [snap1] section"
else
    fail "fly-nvidia-majors.sh: block landed inside or after the [snap1] section, where PVE ignores lxc.* keys"
fi
# PVE rewrites the conf on every lifecycle operation: keys sorted, comments
# hoisted to the top, raw lxc.* keys moved to the end (measured on PVE
# 9.2.10, 2026-09-16). The sentinels then bracket nothing and the eight
# generated lines sit outside them. This must read as `unchanged` and write
# NOTHING — a byte-comparing converge reports `changed` forever, which
# rewrites pmxcfs on every run and makes 01-create-ct.sh restart a live
# container every time; and a comment-bracket-only rewrite leaves the
# hoisted-out lines behind as duplicates that carry stale majors after the
# next driver change.
normalized="$lint_tmp/lxc/901.conf"
{
    awk '/^#/ { print }' "$lint_tmp/lxc/900.conf"
    awk '!/^#/ && !/^lxc\./ && NF' "$lint_tmp/lxc/900.conf"
    awk '/^lxc\./ { print }' "$lint_tmp/lxc/900.conf"
} > "$normalized"
before="$(sha256sum < "$normalized")"
run3="$("${majors_env[@]}" "$MAJORS_SH" 901 2>/dev/null || true)"
after="$(sha256sum < "$normalized")"
if [ "$run3" = "901 unchanged" ] && [ "$before" = "$after" ]; then
    pass "fly-nvidia-majors.sh: a PVE-normalized conf (comments hoisted, lxc.* at the end) reads as unchanged and is not rewritten"
else
    fail "fly-nvidia-majors.sh: PVE-normalized conf gave '$run3' and $([ "$before" = "$after" ] && echo 'no rewrite' || echo 'a rewrite') — expected '901 unchanged' with no rewrite"
fi

# A stale DYNAMIC major left behind by an older driver load (509 — registered
# to nothing now, and inside the kernel's extended dynamic range) is ours and
# must be removed. A STATIC major (226, drm) is not ours and must survive even
# though this fixture's /proc/devices does not list it either: a container
# whose device module happens to be unloaded must not lose its allow line.
printf 'lxc.cgroup2.devices.allow: c 509:* rwm\nlxc.cgroup2.devices.allow: c 226:* rwm\n' >> "$normalized"
run4="$("${majors_env[@]}" "$MAJORS_SH" 901 2>/dev/null || true)"
if [ "$run4" = "901 changed" ] \
   && ! grep -q '^lxc.cgroup2.devices.allow: c 509:\* rwm$' "$normalized" \
   && grep -q '^lxc.cgroup2.devices.allow: c 226:\* rwm$' "$normalized"; then
    pass "fly-nvidia-majors.sh: removes a stale NVIDIA major (509) and keeps a foreign one (226, drm)"
else
    fail "fly-nvidia-majors.sh: stale-major cleanup wrong (run='$run4'); 509 should be gone, 226 should remain"
fi

# The one NVIDIA bind this script must never delete: the neighbouring GPU container's
# /dev/nvidia-modeset, which belongs to the LLM work, not to this unit.
printf 'lxc.mount.entry: /dev/nvidia-modeset dev/nvidia-modeset none bind,optional,create=file\n' >> "$normalized"
"${majors_env[@]}" "$MAJORS_SH" 901 >/dev/null 2>&1 || true
if grep -q '^lxc.mount.entry: /dev/nvidia-modeset ' "$normalized"; then
    pass "fly-nvidia-majors.sh: leaves a /dev/nvidia-modeset bind it did not write alone"
else
    fail "fly-nvidia-majors.sh: deleted the /dev/nvidia-modeset bind — that is another container's device"
fi

# The refusal that matters: no caps line in /proc/devices must mean no write.
printf 'Character devices:\n195 nvidia\n511 nvidia-uvm\n' > "$lint_tmp/devices"
if "${majors_env[@]}" "$MAJORS_SH" 900 >/dev/null 2>&1; then
    fail "fly-nvidia-majors.sh: wrote (or exited 0) with an EMPTY nvidia-caps major — it must refuse"
else
    pass "fly-nvidia-majors.sh: refuses to write when a major reads empty"
fi
rm -rf "$lint_tmp"

# ---------------------------------------------------------------------------
# 3b2. The feed bus edge (docs/design/flybus.md, "Feed over the bus").
#
# flyedge.service is off unless the operator switches a container to
# FLY_FEED_VIA=bus by hand, and when it is on it must follow flysim, which
# owns the router. What would break that is statically visible: the unit
# ending up in fly.target or 07-enable's list, losing its ordering on
# flysim, or the deploy no longer writing the default. Watchdog check 2's
# choice of /metrics is driven for real against a fixture fly.env.
# ---------------------------------------------------------------------------
echo "--- flyedge.service: off by default, after and bound to flysim ---"
EDGE_UNIT="$INFRA_DIR/units/flyedge.service"
if [ ! -f "$EDGE_UNIT" ]; then
    fail "units/flyedge.service is missing"
else
    grep -qE '^After=.*\bflysim\.service\b' "$EDGE_UNIT" \
        && pass "flyedge.service orders itself After=flysim.service" \
        || fail "flyedge.service must be After=flysim.service: flysim owns the feed router"
    grep -qE '^Requires=.*\bflysim\.service\b' "$EDGE_UNIT" \
        && pass "flyedge.service Requires=flysim.service" \
        || fail "flyedge.service must Require flysim.service, so a stop or restart of flysim takes the edge with it"
    grep -qE '^ExecStart=/opt/fly/current/fly-edge$' "$EDGE_UNIT" \
        && pass "flyedge.service runs the release's fly-edge" \
        || fail "flyedge.service ExecStart must be /opt/fly/current/fly-edge"
    grep -qE '^ConditionPathExists=/opt/fly/current/fly-edge$' "$EDGE_UNIT" \
        && pass "flyedge.service stays inactive on a release without fly-edge" \
        || fail "flyedge.service needs ConditionPathExists=/opt/fly/current/fly-edge (a release before it has none)"
    grep -qE '^Environment=FLY_EDGE_METRICS_ADDR=127\.0\.0\.1:' "$EDGE_UNIT" \
        && pass "flyedge.service keeps its metrics on loopback" \
        || fail "flyedge.service FLY_EDGE_METRICS_ADDR must be a 127.0.0.1 address"
fi
# Every unit a target's Wants=/Requires= names, with backslash continuations joined and
# comments dropped: fly.target spreads both lists over several physical lines, and the
# continuation line is exactly where a new unit would be added.
target_pulls() {
    awk '
        /^[[:space:]]*[#;]/ { next }
        {
            line = $0
            cont = sub(/\\[[:space:]]*$/, "", line)
            buf = buf line
            if (cont) next
            if (buf ~ /^[[:space:]]*(Wants|Requires)=/) { sub(/^[^=]*=/, "", buf); print buf }
            buf = ""
        }
    ' "$1" | tr -s ' \t' '\n' | grep -v '^$' || true
}
if target_pulls "$INFRA_DIR/units/fly.target" | grep -qx 'flyedge.service'; then
    fail "fly.target pulls flyedge.service in; it must stay off until the operator enables it"
else
    pass "fly.target does not pull flyedge.service in"
fi
# The parser itself: a unit named only on a continuation line must be found, a commented one
# must not, and the real fly.target must still yield flysim.service.
tp_fixture="$(mktemp "${TMPDIR:-/tmp}/fly-lint-target.XXXXXX")"
cat > "$tp_fixture" <<'TPTARGET'
[Unit]
Wants=network-online.target xvfb.service \
      flysim.service flyedge.service
# Requires=commented.service
Requires=xvfb.service \
         pulse.service
TPTARGET
tp_units="$(target_pulls "$tp_fixture")"
if printf '%s\n' "$tp_units" | grep -qx 'flyedge.service' \
    && printf '%s\n' "$tp_units" | grep -qx 'pulse.service' \
    && ! printf '%s\n' "$tp_units" | grep -qx 'commented.service' \
    && target_pulls "$INFRA_DIR/units/fly.target" | grep -qx 'flysim.service'; then
    pass "target_pulls reads continuation lines and skips comments (fixture + fly.target)"
else
    fail "target_pulls missed a continuation line or read a comment: $(echo "$tp_units" | tr '\n' ' ')"
fi
rm -f "$tp_fixture"
if grep -E '^(ALWAYS_ON_UNITS|APP_UNITS)=' "$INFRA_DIR/07-enable.sh" "$INFRA_DIR/verify.sh" | grep -q 'flyedge'; then
    fail "07-enable.sh or verify.sh lists flyedge.service as always-on"
else
    pass "07-enable.sh and verify.sh leave flyedge.service alone"
fi
if grep -qF 'FLY_FEED_VIA_EFFECTIVE="$(feed_via_normalize "${FLY_FEED_VIA:-}")"' "$INFRA_DIR/05-deploy.sh" \
    && grep -qF 'echo "FLY_FEED_VIA=${FLY_FEED_VIA_EFFECTIVE}"' "$INFRA_DIR/05-deploy.sh"; then
    pass "05-deploy.sh validates FLY_FEED_VIA and writes the normalized value"
else
    fail "05-deploy.sh must run FLY_FEED_VIA through feed_via_normalize and write FLY_FEED_VIA_EFFECTIVE"
fi
# shellcheck source=../lib/common.sh
fv_out="$(bash -c '. "$1/lib/common.sh"
    for v in "" direct DIRECT bus Bus BUS; do printf "%s=%s " "${v:-empty}" "$(feed_via_normalize "$v")"; done
    for v in buss "bus " direct,bus; do feed_via_normalize "$v" >/dev/null && printf "ACCEPTED:%s " "$v"; done; true' _ "$INFRA_DIR" 2>&1)"
if [ "$fv_out" = "empty=direct direct=direct DIRECT=direct bus=bus Bus=bus BUS=bus " ]; then
    pass "feed_via_normalize: direct|bus in any case, empty is direct, anything else refused"
else
    fail "feed_via_normalize: got '$fv_out'"
fi
if grep -qE '^[[:space:]]*for u in flysim .*\bflyedge\b.*; do$' "$INFRA_DIR/05-deploy.sh"; then
    pass "05-deploy.sh writes a cpuset drop-in for flyedge.service"
else
    fail "05-deploy.sh cpuset loop must include flyedge (the page's CPUs, never flysim's)"
fi
if grep -qE '^Environment=FLY_FEED_VIA' "$INFRA_DIR/units/flysim.service"; then
    fail "flysim.service pins FLY_FEED_VIA; it belongs to fly.env so a box can be switched by deploy"
else
    pass "flysim.service leaves FLY_FEED_VIA to fly.env"
fi

echo "--- fly-watchdog check 2: the feed counters follow FLY_FEED_VIA ---"
if ! tail -n1 "$INFRA_DIR/bin/fly-watchdog" | grep -qE '^main "\$@"$'; then
    fail "fly-watchdog: expected the last line to be 'main \"\$@\"' — the check-2 fixture strips it"
else
    fe_fixture="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-edge.XXXXXX")"
    sed '$d' "$INFRA_DIR/bin/fly-watchdog" > "$fe_fixture/wd.sh"
    feed_url_case() {
        local label="$1" env_line="$2" override="$3" want="$4" got
        printf '%s\n' "$env_line" > "$fe_fixture/fly.env"
        got="$(FLY_ENV_FILE="$fe_fixture/fly.env" FLY_FEED_METRICS_URL="$override" \
            FLY_METRICS_URL=http://sim FLY_EDGE_METRICS_URL=http://edge \
            WD_RUN_DIR="$fe_fixture/run" WD_STATE_DIR="$fe_fixture/state" \
            TEXTFILE_DIR="$fe_fixture/textfile" \
            bash -c "source '$fe_fixture/wd.sh'; feed_metrics_url" 2>&1 || true)"
        if [ "$got" = "$want" ]; then
            pass "check 2 feed metrics: $label -> $got"
        else
            fail "check 2 feed metrics: $label: got '$got', want '$want'"
        fi
    }
    feed_url_case "direct" "FLY_FEED_VIA=direct" "" "http://sim"
    feed_url_case "no FLY_FEED_VIA line (a fly.env before it)" "FLY_GAME=pokemon-red" "" "http://sim"
    feed_url_case "bus" "FLY_FEED_VIA=bus" "" "http://edge"
    feed_url_case "Bus (flysim lowercases)" "FLY_FEED_VIA=Bus" "" "http://edge"
    feed_url_case "BUS" "FLY_FEED_VIA=BUS" "" "http://edge"
    feed_url_case "quoted bus" 'FLY_FEED_VIA="bus"' "" "http://edge"
    feed_url_case "explicit override wins" "FLY_FEED_VIA=bus" "http://other" "http://other"
    rm -rf "$fe_fixture"
fi

# ---------------------------------------------------------------------------
# 3c. lib/common.sh cpuset_partition — the three-way cpuset split used by
# 05-deploy.sh section 3b (flysim / page-capture / flycast). Run as its own
# process (a tiny wrapper script), not sourced into this lint script,
# because cpuset_partition calls die() on a refusal and die() calls exit —
# sourcing it here would kill lint.sh itself on the refusal-path cases
# below, the same reason 05-deploy.sh's ROLE=release gate is exercised by
# invoking it directly rather than sourcing it (further down this file).
# ---------------------------------------------------------------------------
echo "--- lib/common.sh cpuset_partition (three-way cpu split) ---"
cpuset_partition_bin="$(mktemp "${TMPDIR:-/tmp}/fly-lint-cpuset.XXXXXX")"
cat > "$cpuset_partition_bin" <<EOF
#!/usr/bin/env bash
set -euo pipefail
. "$INFRA_DIR/lib/common.sh"
cpuset_partition "\$@"
EOF
chmod +x "$cpuset_partition_bin"

check_cpuset_partition() {
    local label="$1" cpuset="$2" rayon="$3" encoder="$4" want_sim="$5" want_page="$6" want_encoder="$7"
    local out rc sim page encoder_got
    if [ -n "$encoder" ]; then
        out="$("$cpuset_partition_bin" "$cpuset" "$rayon" "$encoder" 2>&1)" && rc=0 || rc=$?
    else
        out="$("$cpuset_partition_bin" "$cpuset" "$rayon" 2>&1)" && rc=0 || rc=$?
    fi
    if [ "$rc" -ne 0 ]; then
        fail "cpuset_partition: $label: expected success, died with: $out"
        return 0
    fi
    read -r sim page encoder_got <<< "$out"
    if [ "$sim" = "$want_sim" ] && [ "$page" = "$want_page" ] && [ "$encoder_got" = "$want_encoder" ]; then
        pass "cpuset_partition: $label: flysim=$sim page=$page flycast=$encoder_got"
    else
        fail "cpuset_partition: $label: got flysim=$sim page=$page flycast=$encoder_got, want flysim=$want_sim page=$want_page flycast=$want_encoder"
    fi
}
check_cpuset_partition_refuses() {
    local label="$1" cpuset="$2" rayon="$3" encoder="$4" want_snippet="$5"
    local out rc
    out="$("$cpuset_partition_bin" "$cpuset" "$rayon" "$encoder" 2>&1)" && rc=0 || rc=$?
    if [ "$rc" -eq 0 ]; then
        fail "cpuset_partition: $label: expected a refusal, got success: $out"
        return 0
    fi
    case "$out" in
        *"$want_snippet"*) pass "cpuset_partition: $label: refused as expected" ;;
        *) fail "cpuset_partition: $label: refused, but message did not mention '$want_snippet': $out" ;;
    esac
}

# The 2026-09-16 live hotfix (docs/runbook.md "CPU partition (cpuset)"):
# flysim gets the first 4, flycast the last 2, xvfb/flystage/flystage-web/
# pulse/mediamtx share the 2 left in between.
# The release container's current live values (<release-env>'s "extra cores" note,
# 2026-09-16): ten cpus, ENCODER_CORES=3.
check_cpuset_partition "the release container 2026-09-16 live (ten cpus, ENCODER_CORES=3)" "1,3,5,7,9,11,13,15,17,19" 4 3 "1,3,5,7" "9,11,13" "15,17,19"
# The earlier, eight-cpu version of that same live hotfix, before "extra
# cores" widened it — also exercises the ENCODER_CORES-omitted default (2).
check_cpuset_partition "the release container 2026-09-16 earlier hotfix (eight cpus, ENCODER_CORES default)" "1,3,5,7,9,11,13,15" 4 "" "1,3,5,7" "9,11" "13,15"
# The dev container's values once FLY_LIF_CUDA=1 (<dev-env>, 2026-09-16): the LIF
# tick is on the Quadro, so RAYON_THREADS drops to 2 and the cpu that frees
# up goes to the page group, which is the whole point of the backend
# (infra/docs/cuda-on-dev.md).
check_cpuset_partition "the dev container 2026-09-16 with the CUDA backend (eight cpus, RAYON_THREADS=2)" "0,2,4,6,8,10,12,14" 2 "" "0,2" "4,6,8,10" "12,14"
check_cpuset_partition_refuses "RAYON_THREADS consumes the whole cpuset" "1,3" 2 2 "no cpus left over"
check_cpuset_partition_refuses "not enough left for ENCODER_CORES plus a page cpu" "1,3,5,7" 2 2 "not enough for ENCODER_CORES"
rm -f "$cpuset_partition_bin"

# ---------------------------------------------------------------------------
# 3d. bin/fly-watchdog check 7 (process age guard) — flypush uptime above
# 24h forces a restart and records fly_watchdog_restarts_total{unit=
# "flypush",reason="age"}. Driven the same way as the fly-nvidia-majors.sh
# fixtures above: fake PROC/ENV inputs, here in the form of a fake
# `systemctl` (plus `sudo` and `systemd-cat` shims so restart_unit's real
# call shape — `sudo /usr/bin/systemctl restart ...` — and log_err/log_info
# work without a real systemd). fly-watchdog is sourced with its trailing
# `main "$@"` call stripped, so only check_process_age and
# write_textfile_metrics run — not the curl/jq-driven checks, which this
# lint box cannot satisfy.
# ---------------------------------------------------------------------------
echo "--- fly-watchdog check 7: process age guard ---"
WATCHDOG_SRC="$INFRA_DIR/bin/fly-watchdog"
if ! tail -n1 "$WATCHDOG_SRC" | grep -qE '^main "\$@"$'; then
    fail "fly-watchdog: expected the last line to be 'main \"\$@\"' — this lint fixture strips that line to source the file without running main; the assumption broke, refusing to test check 7"
else
    wd_fixture="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-wd.XXXXXX")"
    wd_fakebin="$wd_fixture/bin"
    mkdir -p "$wd_fakebin"

    cat > "$wd_fakebin/systemctl" <<'FAKESYSTEMCTL'
#!/usr/bin/env bash
# Fake systemctl for the fly-watchdog check-7 lint fixture.
case "$1" in
    is-active)
        [ "${FAKE_FLYPUSH_ACTIVE:-1}" = "1" ] && exit 0 || exit 3 ;;
    is-enabled)
        [ "${FAKE_FLYPUSH_ENABLED:-1}" = "1" ] && exit 0 || exit 1 ;;
    show)
        echo "${FAKE_ACTIVE_ENTER_TS:-}"; exit 0 ;;
    restart)
        echo "restart $2" >> "${FAKE_SYSTEMCTL_LOG:-/dev/null}"; exit 0 ;;
    *)
        exit 0 ;;
esac
FAKESYSTEMCTL
    cat > "$wd_fakebin/sudo" <<'FAKESUDO'
#!/usr/bin/env bash
# fly-watchdog's restart_unit calls `sudo /usr/bin/systemctl ...`; resolve
# that through our PATH-shimmed systemctl instead of the real one.
if [ "$1" = "/usr/bin/systemctl" ]; then
    shift
    exec systemctl "$@"
fi
exec "$@"
FAKESUDO
    cat > "$wd_fakebin/systemd-cat" <<'FAKECAT'
#!/usr/bin/env bash
cat >/dev/null
exit 0
FAKECAT
    chmod +x "$wd_fakebin/systemctl" "$wd_fakebin/sudo" "$wd_fakebin/systemd-cat"

    wd_no_main="$wd_fixture/fly-watchdog-no-main.sh"
    sed '$d' "$WATCHDOG_SRC" > "$wd_no_main"

    run_watchdog_age_case() {
        local label="$1" enabled="$2" active="$3" age_seconds="$4" expect_restart="$5" expect_metric="$6"
        local ts=""
        if [ -n "$age_seconds" ]; then
            ts="$(date -d "@$(( $(date +%s) - age_seconds ))")"
        fi
        local run_dir="$wd_fixture/run" textfile_dir="$wd_fixture/textfile"
        rm -rf "$run_dir" "$textfile_dir"
        mkdir -p "$run_dir" "$textfile_dir"
        : > "$wd_fixture/systemctl.log"

        PATH="$wd_fakebin:$PATH" \
        FAKE_FLYPUSH_ENABLED="$enabled" FAKE_FLYPUSH_ACTIVE="$active" \
        FAKE_ACTIVE_ENTER_TS="$ts" FAKE_SYSTEMCTL_LOG="$wd_fixture/systemctl.log" \
        WD_RUN_DIR="$run_dir" WD_STATE_DIR="$run_dir" TEXTFILE_DIR="$textfile_dir" \
        FLY_ENV_FILE="$wd_fixture/fly.env" \
        bash -c "source '$wd_no_main'; check_process_age; write_textfile_metrics" \
            >/dev/null 2>&1 || true

        local restarted=0
        grep -q "restart flypush.service" "$wd_fixture/systemctl.log" 2>/dev/null && restarted=1
        local metric
        metric="$(grep -F 'fly_watchdog_restarts_total{unit="flypush",reason="age"}' "$textfile_dir/fly_watchdog.prom" 2>/dev/null | awk '{print $2}')"
        metric="${metric:-MISSING}"

        if [ "$restarted" = "$expect_restart" ] && [ "$metric" = "$expect_metric" ]; then
            pass "fly-watchdog check 7: $label"
        else
            fail "fly-watchdog check 7: $label (restarted=$restarted want=$expect_restart, metric=$metric want=$expect_metric)"
        fi
    }

    run_watchdog_age_case "disabled flypush is skipped"                              0 1 90000 0 0
    run_watchdog_age_case "enabled but inactive is skipped"                          1 0 90000 0 0
    run_watchdog_age_case "enabled, active, under 24h: no restart"                   1 1 3600  0 0
    run_watchdog_age_case "enabled, active, over 24h: restarts and records reason=age" 1 1 90000 1 1

    rm -rf "$wd_fixture"
fi

# ---------------------------------------------------------------------------
# 3e. The capture-freeze pass (infra/docs/capture-freeze.md): the ordering
# gate in units/flycast.service, bin/wait-for-stage's decisions, and
# bin/fly-watchdog's check 9.
#
# What is testable without a container: the unit's ordering/timeout
# directives, wait-for-stage's argument handling and its "start anyway after
# the timeout" contract, and the whole of check 9 driven against ffmpeg-made
# fixture segments (a frozen one and a moving one) with a fake systemctl.
# What is NOT: the real readiness path, which needs Xvfb + Chromium + a pulse
# sink — that is exercised on the dev container, not here.
# ---------------------------------------------------------------------------
echo "--- flycast.service capture-freeze ordering gate ---"
FLYCAST_UNIT="$INFRA_DIR/units/flycast.service"
if grep -qE '^After=.*\bflystage\.service\b' "$FLYCAST_UNIT"; then
    pass "flycast.service orders itself After=flystage.service"
else
    fail "flycast.service is missing flystage.service in After= (infra/docs/capture-freeze.md: co-starting with the page is what freezes the capture)"
fi
if grep -qE '^Requires=.*\bflystage\.service\b' "$FLYCAST_UNIT"; then
    fail "flycast.service has flystage.service in Requires= — a dead page must not be able to take the encoder, the recording and the broadcast down with it"
else
    pass "flycast.service keeps flystage.service out of Requires="
fi
if grep -qE '^ExecStartPre=/opt/fly/bin/wait-for-stage [0-9]+$' "$FLYCAST_UNIT"; then
    pass "flycast.service runs wait-for-stage as an ExecStartPre"
else
    fail "flycast.service is missing 'ExecStartPre=/opt/fly/bin/wait-for-stage <timeout>'"
fi
# The two ExecStartPre waits (30 + 120) live inside the start job, so the
# default TimeoutStartSec=90s would kill the unit mid-wait.
for u in flycast flystage; do
    unit="$INFRA_DIR/units/${u}.service"
    pre_total=0
    while IFS= read -r secs; do
        pre_total=$(( pre_total + secs ))
    done < <(grep -oE '^ExecStartPre=/opt/fly/bin/wait-for-(x|stage|health) [^ ]*( [0-9]+)?$' "$unit" | grep -oE '[0-9]+$' || true)
    tss="$(grep -oE '^TimeoutStartSec=[0-9]+' "$unit" | head -n1 | cut -d= -f2 || true)"
    if [ "$pre_total" -le 90 ]; then
        pass "${u}.service: ExecStartPre waits total ${pre_total}s, inside systemd's default 90s"
    elif [ -n "$tss" ] && [ "$tss" -ge "$pre_total" ]; then
        pass "${u}.service: ExecStartPre waits total ${pre_total}s, covered by TimeoutStartSec=${tss}"
    else
        fail "${u}.service: ExecStartPre waits total ${pre_total}s but TimeoutStartSec is ${tss:-unset} (default 90s) — systemd would kill the start job mid-wait and Restart=always would spin it"
    fi
done

echo "--- wait-for-stage argument handling and timeout contract ---"
WAIT_FOR_STAGE="$INFRA_DIR/bin/wait-for-stage"
if out="$("$WAIT_FOR_STAGE" notanumber 2>&1)"; then
    fail "wait-for-stage accepted a non-numeric timeout instead of failing: $out"
else
    case "$out" in
        *"usage:"*) pass "wait-for-stage rejects a non-numeric timeout" ;;
        *) fail "wait-for-stage refused a non-numeric timeout without printing usage: $out" ;;
    esac
fi
# :99 may well exist on a dev box; :77 with nothing on it is the deterministic
# "not ready" case. --once must say so and exit 1.
if out="$(FLY_STAGE_DISPLAY=:77 "$WAIT_FOR_STAGE" --once 2>&1)"; then
    fail "wait-for-stage --once reported ready against a display that does not exist: $out"
else
    case "$out" in
        *"X display :77 does not answer"*) pass "wait-for-stage --once names the failing check (no X display)" ;;
        *) fail "wait-for-stage --once did not name the X check: $out" ;;
    esac
fi
# The contract that keeps a dark channel from being the failure mode: after
# the timeout it WARNS and still exits 0, so flycast starts.
if out="$(FLY_STAGE_DISPLAY=:77 "$WAIT_FOR_STAGE" 1 2>&1)"; then
    case "$out" in
        *"WARNING"*"starting the encoder ANYWAY"*) pass "wait-for-stage exits 0 with a warning after its timeout (a black-but-running stream beats no stream)" ;;
        *) fail "wait-for-stage timed out and exited 0 but did not log the warning: $out" ;;
    esac
else
    fail "wait-for-stage exited nonzero on timeout — that would make flycast's ExecStartPre fail and leave the channel dark: $out"
fi

echo "--- fly-watchdog check 9: capture-freeze probe ---"
if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "SKIP: fly-watchdog check 9 (no ffmpeg on this box to build the fixture segments)"
elif ! tail -n1 "$INFRA_DIR/bin/fly-watchdog" | grep -qE '^main "\$@"$'; then
    fail "fly-watchdog: expected the last line to be 'main \"\$@\"' — the check-9 fixture strips it to source the file without running main"
else
    fz_fixture="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-freeze.XXXXXX")"
    fz_bin="$fz_fixture/bin"
    mkdir -p "$fz_bin" "$fz_fixture/rec" "$fz_fixture/run" "$fz_fixture/textfile"

    cat > "$fz_bin/systemctl" <<'FZSYSTEMCTL'
#!/usr/bin/env bash
# Fake systemctl for the check-9 fixture: flycast is active, restarts are logged.
case "$1" in
    is-active)  [ "${FAKE_FLYCAST_ACTIVE:-1}" = "1" ] && exit 0 || exit 3 ;;
    is-enabled) exit 1 ;;
    restart)    echo "restart $2" >> "${FAKE_SYSTEMCTL_LOG:-/dev/null}"; exit 0 ;;
    *)          exit 0 ;;
esac
FZSYSTEMCTL
    cat > "$fz_bin/sudo" <<'FZSUDO'
#!/usr/bin/env bash
if [ "$1" = "/usr/bin/systemctl" ]; then
    shift
    exec systemctl "$@"
fi
exec "$@"
FZSUDO
    cat > "$fz_bin/systemd-cat" <<'FZCAT'
#!/usr/bin/env bash
cat >> "${FAKE_JOURNAL:-/dev/null}"
exit 0
FZCAT
    chmod +x "$fz_bin/systemctl" "$fz_bin/sudo" "$fz_bin/systemd-cat"

    # Two fixture "segments", built the way the real ones are muxed (mpegts,
    # h264, 30 fps): one frozen (a single colour, every frame identical to the
    # last) and one moving (mandelbrot, no two frames alike). 12 s each, which
    # is more than the probe's 4 s tail.
    ffmpeg -loglevel error -y -f lavfi -i "color=c=blue:size=320x240:rate=30:duration=12" \
        -c:v libx264 -preset ultrafast -pix_fmt yuv420p -f mpegts "$fz_fixture/frozen.ts" 2>/dev/null
    ffmpeg -loglevel error -y -f lavfi -i "mandelbrot=size=320x240:rate=30" -t 12 \
        -c:v libx264 -preset ultrafast -pix_fmt yuv420p -f mpegts "$fz_fixture/moving.ts" 2>/dev/null

    wd_no_main_fz="$fz_fixture/fly-watchdog-no-main.sh"
    sed '$d' "$INFRA_DIR/bin/fly-watchdog" > "$wd_no_main_fz"

    # run_freeze_probe SEGMENT [EXTRA_ENV...] — one check_capture_freeze pass
    # against a copy of SEGMENT as the newest segment in the rec dir, then
    # write_textfile_metrics. Echoes nothing; the caller reads the fixture.
    run_freeze_pass() {
        local seg="$1"
        cp "$fz_fixture/${seg}" "$fz_fixture/rec/20260916-000000.ts"
        PATH="$fz_bin:$PATH" \
        FAKE_SYSTEMCTL_LOG="$fz_fixture/systemctl.log" FAKE_JOURNAL="$fz_fixture/journal.log" \
        WD_RUN_DIR="$fz_fixture/run" WD_STATE_DIR="$fz_fixture/run" TEXTFILE_DIR="$fz_fixture/textfile" \
        FLY_REC_DIR="$fz_fixture/rec" FLY_ENV_FILE="$fz_fixture/fly.env" \
        FLYCAST_CPUSET_DROPIN="$fz_fixture/nonexistent-cpuset.conf" \
        WD_FREEZE_INTERVAL=0 \
        bash -c "source '$wd_no_main_fz'; check_capture_freeze; write_textfile_metrics" \
            >/dev/null 2>&1 || true
    }
    freeze_metric() {
        # Anchored: the .prom file carries a `# HELP <name> ...` line for each
        # metric, and an unanchored grep picks that up first.
        awk -v name="$1" '$1 == name {print $2; exit}' "$fz_fixture/textfile/fly_watchdog.prom" 2>/dev/null
    }
    restart_count() {
        grep -c "restart flycast.service" "$fz_fixture/systemctl.log" 2>/dev/null || true
    }

    : > "$fz_fixture/systemctl.log"

    # A moving picture must never trigger anything, and must publish a low
    # identical-frame gauge.
    run_freeze_pass moving.ts
    moving_identical="$(freeze_metric fly_capture_identical_frames)"
    if [ "$(restart_count)" = "0" ] && [ -n "$moving_identical" ] && [ "$moving_identical" -lt 80 ]; then
        pass "check 9: a moving segment reads ${moving_identical} identical frames and restarts nothing"
    else
        fail "check 9: a moving segment gave identical=${moving_identical:-MISSING} and $(restart_count) flycast restart(s) — expected a low count and none"
    fi

    # One frozen probe is a warning, not a restart: a legitimately still page
    # could read high once.
    run_freeze_pass frozen.ts
    frozen_identical="$(freeze_metric fly_capture_identical_frames)"
    if [ "$(restart_count)" = "0" ] && [ -n "$frozen_identical" ] && [ "$frozen_identical" -ge 80 ]; then
        pass "check 9: one frozen probe (${frozen_identical} identical) logs but does not restart"
    else
        fail "check 9: first frozen probe gave identical=${frozen_identical:-MISSING} and $(restart_count) restart(s) — expected >=80 identical and no restart yet"
    fi

    # The second consecutive frozen probe restarts flycast exactly once and
    # counts it.
    run_freeze_pass frozen.ts
    if [ "$(restart_count)" = "1" ] && [ "$(freeze_metric fly_capture_freeze_restarts_total)" = "1" ]; then
        pass "check 9: two consecutive frozen probes restart flycast once and export fly_capture_freeze_restarts_total 1"
    else
        fail "check 9: expected exactly one flycast restart and fly_capture_freeze_restarts_total=1, got $(restart_count) restart(s) and $(freeze_metric fly_capture_freeze_restarts_total)"
    fi

    # And the 30-minute cooldown holds: still frozen, no second restart.
    run_freeze_pass frozen.ts
    run_freeze_pass frozen.ts
    if [ "$(restart_count)" = "1" ]; then
        pass "check 9: the 30-minute cooldown blocks a second restart while the freeze persists"
    else
        fail "check 9: cooldown leaked — $(restart_count) flycast restarts, expected 1"
    fi

    # encoder_cpus is a pure parse of the drop-in 05-deploy.sh writes; the
    # probe runs unpinned when there is no partition (the no-op rule).
    # WD_RUN_DIR/WD_STATE_DIR have to be overridden even for a pure parse:
    # sourcing fly-watchdog runs its `mkdir -p` on the real /run/fly paths,
    # which this box cannot create, and `set -e` would abort the source before
    # any function is defined.
    printf '[Service]\nAllowedCPUs=15,17,19\n' > "$fz_fixture/cpuset.conf"
    fz_source_env=(env "WD_RUN_DIR=$fz_fixture/run" "WD_STATE_DIR=$fz_fixture/run" "TEXTFILE_DIR=$fz_fixture/textfile")
    cpus_out="$("${fz_source_env[@]}" "FLYCAST_CPUSET_DROPIN=$fz_fixture/cpuset.conf" \
        bash -c "source '$wd_no_main_fz'; encoder_cpus" 2>/dev/null || true)"
    cpus_none="$("${fz_source_env[@]}" "FLYCAST_CPUSET_DROPIN=$fz_fixture/nope.conf" \
        bash -c "source '$wd_no_main_fz'; encoder_cpus" 2>/dev/null || true)"
    if [ "$cpus_out" = "15,17,19" ] && [ -z "$cpus_none" ]; then
        pass "check 9: encoder_cpus reads AllowedCPUs from flycast's cpuset drop-in, empty (unpinned) without one"
    else
        fail "check 9: encoder_cpus gave '${cpus_out}' with a drop-in and '${cpus_none}' without — expected '15,17,19' and empty"
    fi

    rm -rf "$fz_fixture"
fi

# ---------------------------------------------------------------------------
# 3f. fly-watchdog check 10 (loop suspected), infra/docs/macros-traps.md.
#
# Fixture-driven like check 9 above, but nothing here needs ffmpeg: the inputs
# are an events.jsonl and a /status.json, both writable by hand, which is the
# whole reason this check was built on them. Four cases, and one standing
# assertion across all of them — the fake systemctl log stays EMPTY, because
# the ethos of this check is that it reports and never acts.
# ---------------------------------------------------------------------------
echo "--- fly-watchdog check 10: loop-suspected probe ---"
if ! command -v jq >/dev/null 2>&1; then
    echo "SKIP: fly-watchdog check 10 (no jq on this box)"
elif ! tail -n1 "$INFRA_DIR/bin/fly-watchdog" | grep -qE '^main "\$@"$'; then
    fail "fly-watchdog: expected the last line to be 'main \"\$@\"' — the check-10 fixture strips it to source the file without running main"
else
    lp_fixture="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-loop.XXXXXX")"
    lp_bin="$lp_fixture/bin"
    mkdir -p "$lp_bin" "$lp_fixture/run" "$lp_fixture/textfile"

    cat > "$lp_bin/systemctl" <<'LPSYSTEMCTL'
#!/usr/bin/env bash
# Fake systemctl for the check-10 fixture: flysim is active, and any restart
# at all is a test failure, so it is logged.
case "$1" in
    is-active)  exit 0 ;;
    is-enabled) exit 1 ;;
    restart)    echo "restart $2" >> "${FAKE_SYSTEMCTL_LOG:-/dev/null}"; exit 0 ;;
    *)          exit 0 ;;
esac
LPSYSTEMCTL
    cat > "$lp_bin/sudo" <<'LPSUDO'
#!/usr/bin/env bash
if [ "$1" = "/usr/bin/systemctl" ]; then
    shift
    exec systemctl "$@"
fi
exec "$@"
LPSUDO
    cat > "$lp_bin/systemd-cat" <<'LPCAT'
#!/usr/bin/env bash
cat >> "${FAKE_JOURNAL:-/dev/null}"
exit 0
LPCAT
    chmod +x "$lp_bin/systemctl" "$lp_bin/sudo" "$lp_bin/systemd-cat"

    wd_no_main_lp="$lp_fixture/fly-watchdog-no-main.sh"
    sed '$d' "$INFRA_DIR/bin/fly-watchdog" > "$wd_no_main_lp"

    # lp_cycle N NAME... — echoes the NAMEs repeated N times, one per line.
    lp_cycle() {
        local reps="$1"
        shift
        local i nm
        for (( i = 0; i < reps; i++ )); do
            for nm in "$@"; do
                echo "$nm"
            done
        done
    }
    # lp_events FILE — one `macro` start + done pair per name on stdin, 750
    # brain ms apart, in the real FeedEvent shape (docs/feed-protocol.md), with
    # a `reward` event and a torn final line mixed in: the probe must filter by
    # kind and survive reading a file its writer is still appending to.
    lp_events() {
        local out="$1" id=0 ms=0 nm
        {
            while IFS= read -r nm; do
                id=$(( id + 1 ))
                printf '{"id":%d,"wallMs":%d,"brainMs":%d,"kind":"macro","label":"%s start","value":1}\n' \
                    "$id" "$(( 1758000000000 + id ))" "$ms" "$nm"
                id=$(( id + 1 ))
                printf '{"id":%d,"wallMs":%d,"brainMs":%d,"kind":"macro","label":"%s done","value":1}\n' \
                    "$id" "$(( 1758000000000 + id ))" "$ms" "$nm"
                if [ $(( id % 40 )) -eq 0 ]; then
                    id=$(( id + 1 ))
                    printf '{"id":%d,"wallMs":%d,"brainMs":%d,"kind":"reward","label":"exploration","value":1,"rewardKind":"explore"}\n' \
                        "$id" "$(( 1758000000000 + id ))" "$ms"
                fi
                ms=$(( ms + 750 ))
            done
            printf '{"id":999999,"wallMs":1758'
        } > "$out"
    }
    # lp_outcomes FILE OUTCOME — like lp_events, but each name on stdin is one
    # decision that ended OUTCOME: `refused` writes the refusal alone (nothing
    # started, which is what a refused press is), anything else a start and
    # that outcome. Row 57's shape is `GO ROUTE refused` every 800 brain ms.
    lp_outcomes() {
        local out="$1" outcome="$2" id=0 ms=0 nm
        {
            while IFS= read -r nm; do
                if [ "$outcome" != "refused" ]; then
                    id=$(( id + 1 ))
                    printf '{"id":%d,"wallMs":%d,"brainMs":%d,"kind":"macro","label":"%s start","value":3}\n' \
                        "$id" "$(( 1758000000000 + id ))" "$ms" "$nm"
                fi
                id=$(( id + 1 ))
                printf '{"id":%d,"wallMs":%d,"brainMs":%d,"kind":"macro","label":"%s %s","value":3}\n' \
                    "$id" "$(( 1758000000000 + id ))" "$ms" "$nm" "$outcome"
                ms=$(( ms + 800 ))
            done
        } > "$out"
    }
    # lp_status FILE PLACES — the /status.json fields check 10 reads. `places`
    # is `game.uniqueLocations`; there is no `places` field in the contract.
    lp_status() {
        printf '{"game":{"mode":"OVERWORLD","map":1,"uniqueLocations":%s,"macroMode":"macros"},"milestone":{"rank":8,"label":"VIRIDIAN CITY","next":"VIRIDIAN FOREST","sinceSeconds":8234}}\n' \
            "$2" > "$1"
    }
    # lp_pass — one check_loop pass plus write_textfile_metrics, against the
    # fixture's own event log and status file (curl reads the latter over
    # file://, so no fake curl is needed).
    lp_pass() {
        PATH="$lp_bin:$PATH" \
        FAKE_SYSTEMCTL_LOG="$lp_fixture/systemctl.log" FAKE_JOURNAL="$lp_fixture/journal.log" \
        WD_RUN_DIR="$lp_fixture/run" WD_STATE_DIR="$lp_fixture/run" TEXTFILE_DIR="$lp_fixture/textfile" \
        FLY_EVENT_LOG="$lp_fixture/events.jsonl" FLY_STATUS_URL="file://$lp_fixture/status.json" \
        WD_LOOP_INTERVAL=0 \
        bash -c "source '$wd_no_main_lp'; check_loop; write_textfile_metrics" \
            >/dev/null 2>&1 || true
    }
    lp_metric() {
        awk -v name="$1" '$1 == name {print $2; exit}' "$lp_fixture/textfile/fly_watchdog.prom" 2>/dev/null
    }
    lp_reset() {
        rm -rf "$lp_fixture/run"
        mkdir -p "$lp_fixture/run"
        : > "$lp_fixture/journal.log"
    }
    : > "$lp_fixture/systemctl.log"

    # (1) The Viridian shape: a three-macro cycle repeating, no new ground.
    # Two passes, because "has the exploration count grown" needs a previous
    # probe to compare against — the first probe can never flag.
    lp_reset
    lp_cycle 40 "GO OUT" "NEXT" "GO FRONTIER" | lp_events "$lp_fixture/events.jsonl"
    lp_status "$lp_fixture/status.json" 152
    lp_pass
    if [ "$(lp_metric fly_loop_suspected)" = "0" ] && [ "$(lp_metric fly_places_delta)" = "-1" ]; then
        pass "check 10: the first probe reports fly_places_delta -1 and flags nothing (no previous count to compare)"
    else
        fail "check 10: first probe gave suspected=$(lp_metric fly_loop_suspected) places_delta=$(lp_metric fly_places_delta) — expected 0 and -1"
    fi
    lp_pass
    if [ "$(lp_metric fly_loop_suspected)" = "1" ] \
       && [ "$(lp_metric fly_loop_period)" = "3" ] \
       && [ "$(lp_metric fly_loop_repeats)" = "40" ] \
       && [ "$(lp_metric fly_loop_distinct_macros)" = "3" ] \
       && [ "$(lp_metric fly_places_delta)" = "0" ]; then
        pass "check 10: a looping event log flags (period 3 x40, 3 distinct macros, places delta 0)"
    else
        fail "check 10: a looping log gave suspected=$(lp_metric fly_loop_suspected) period=$(lp_metric fly_loop_period) repeats=$(lp_metric fly_loop_repeats) distinct=$(lp_metric fly_loop_distinct_macros) places_delta=$(lp_metric fly_places_delta) — expected 1/3/40/3/0"
    fi
    if grep -q 'loop suspected: \[GO OUT, NEXT, GO FRONTIER\] x40' "$lp_fixture/journal.log"; then
        pass "check 10: the journal line carries the repeating sequence and its repeat count"
    else
        fail "check 10: expected a 'loop suspected: [GO OUT, NEXT, GO FRONTIER] x40' journal line, got: $(cat "$lp_fixture/journal.log")"
    fi
    # The report a review agent picks up.
    if lp_report="$(jq -e -r '[(.suspected|tostring), (.sequence|join("|")), .reason, (.window.macroStarts|tostring), (.milestone.sinceSeconds|tostring), (.map|tostring), (.places.delta|tostring), .action] | join(" ")' "$lp_fixture/run/loop.json" 2>/dev/null)" \
       && [ "$lp_report" = "1 GO OUT|NEXT|GO FRONTIER sequence 120 8234 1 0 none" ]; then
        pass "check 10: /run/fly/wd/loop.json carries the sequence, window, map, milestone and action:none"
    else
        fail "check 10: loop.json read back as '${lp_report:-UNREADABLE}' — expected '1 GO OUT|NEXT|GO FRONTIER sequence 120 8234 1 0 none'"
    fi

    # (2) A progressing run: eight macro names, no short cycle, same standing
    # exploration count. The places rule alone must never flag.
    lp_reset
    lp_cycle 15 "GO ROUTE" "TALK" "GO OBJECTIVE" "NEXT" "GO ITEM" "GO WARP" "GO FRONTIER" "GO OUT" \
        | lp_events "$lp_fixture/events.jsonl"
    lp_status "$lp_fixture/status.json" 152
    lp_pass
    lp_pass
    if [ "$(lp_metric fly_loop_suspected)" = "0" ] && [ "$(lp_metric fly_loop_distinct_macros)" = "8" ]; then
        pass "check 10: a progressing event log (8 distinct macros) does not flag even with a flat exploration count"
    else
        fail "check 10: a progressing log flagged: suspected=$(lp_metric fly_loop_suspected) distinct=$(lp_metric fly_loop_distinct_macros) period=$(lp_metric fly_loop_period) repeats=$(lp_metric fly_loop_repeats)"
    fi

    # (3) The dominance rule: one macro at 95% of the window, with the tail
    # broken so the sequence rule cannot be what fires (period 0, repeats 0).
    lp_reset
    { for _ in 1 2 3 4 5; do lp_cycle 19 "GO FRONTIER"; echo "NEXT"; done; } \
        | lp_events "$lp_fixture/events.jsonl"
    lp_status "$lp_fixture/status.json" 152
    lp_pass
    lp_pass
    if [ "$(lp_metric fly_loop_suspected)" = "1" ] \
       && [ "$(lp_metric fly_loop_period)" = "0" ] \
       && grep -q 'loop suspected: \[GO FRONTIER\] is 95% of 100 macro starts' "$lp_fixture/journal.log"; then
        pass "check 10: one macro at 95% of the window flags on the dominance rule with nothing repeating"
    else
        fail "check 10: the 95% single-macro case gave suspected=$(lp_metric fly_loop_suspected) period=$(lp_metric fly_loop_period) and journal: $(cat "$lp_fixture/journal.log")"
    fi

    # (4) New ground clears it: same looping log, the exploration count moves.
    lp_reset
    lp_cycle 40 "GO OUT" "NEXT" "GO FRONTIER" | lp_events "$lp_fixture/events.jsonl"
    lp_status "$lp_fixture/status.json" 152
    lp_pass
    lp_pass
    lp_flagged="$(lp_metric fly_loop_suspected)"
    lp_status "$lp_fixture/status.json" 170
    : > "$lp_fixture/journal.log"
    lp_pass
    if [ "$lp_flagged" = "1" ] \
       && [ "$(lp_metric fly_loop_suspected)" = "0" ] \
       && [ "$(lp_metric fly_places_delta)" = "18" ] \
       && grep -q 'loop cleared:' "$lp_fixture/journal.log"; then
        pass "check 10: growth in the exploration count clears the flag and logs one 'loop cleared' line"
    else
        fail "check 10: expected the flag to clear on places growth, got flagged=${lp_flagged} then suspected=$(lp_metric fly_loop_suspected) places_delta=$(lp_metric fly_places_delta), journal: $(cat "$lp_fixture/journal.log")"
    fi

    # (5) Row 57: a pad of one button that refuses every hold. One start in the
    # window and one name, so the sequence and dominance rules over starts
    # alone never fired; the outcomes say it is a stall.
    lp_reset
    { lp_cycle 700 "GO ROUTE" | lp_outcomes "$lp_fixture/refused.jsonl" refused
      echo "GO ROUTE" | lp_outcomes "$lp_fixture/blocked.jsonl" blocked
      cat "$lp_fixture/refused.jsonl" "$lp_fixture/blocked.jsonl"; } > "$lp_fixture/events.jsonl"
    lp_status "$lp_fixture/status.json" 1846
    lp_pass
    lp_first="$(lp_metric fly_loop_suspected)"
    lp_pass
    if [ "$lp_first" = "0" ] \
       && [ "$(lp_metric fly_loop_suspected)" = "1" ] \
       && [ "$(lp_metric fly_loop_refused)" = "700" ] \
       && [ "$(lp_metric fly_loop_blocked)" = "1" ] \
       && [ "$(lp_metric fly_loop_done)" = "0" ] \
       && grep -q 'loop suspected (stalled): \[GO ROUTE\]' "$lp_fixture/journal.log"; then
        pass "check 10: a pad whose one button is refused every hold flags as stalled (700 refused, 1 blocked, 0 done)"
    else
        fail "check 10: the row-57 refusal log gave first=${lp_first} suspected=$(lp_metric fly_loop_suspected) refused=$(lp_metric fly_loop_refused) blocked=$(lp_metric fly_loop_blocked) done=$(lp_metric fly_loop_done), journal: $(cat "$lp_fixture/journal.log")"
    fi
    if lp_report="$(jq -e -r '[.reason, (.window.macroStarts|tostring), (.window.decisions|tostring), (.window.outcomes.refused|tostring), (.window.outcomes.done|tostring), .action] | join(" ")' "$lp_fixture/run/loop.json" 2>/dev/null)" \
       && [ "$lp_report" = "stalled 1 701 700 0 none" ]; then
        pass "check 10: loop.json carries the decisions and every outcome, not only the starts"
    else
        fail "check 10: loop.json read back as '${lp_report:-UNREADABLE}' — expected 'stalled 1 701 700 0 none'"
    fi

    # (6) Zero progress: a handful of decisions, every one blocked, too few for
    # the stall rule's floor. One probe of it is not enough; two in a row are.
    lp_reset
    lp_cycle 3 "GO OBJECTIVE" "GO FRONTIER" | lp_outcomes "$lp_fixture/events.jsonl" blocked
    lp_status "$lp_fixture/status.json" 1846
    lp_pass
    lp_pass
    lp_first="$(lp_metric fly_loop_suspected)"
    lp_pass
    if [ "$lp_first" = "0" ] && [ "$(lp_metric fly_loop_suspected)" = "1" ] \
       && grep -q 'loop suspected (zero-progress)' "$lp_fixture/journal.log"; then
        pass "check 10: decisions that complete nothing over two probes with no new ground flag as zero-progress"
    else
        fail "check 10: the zero-progress case gave first=${lp_first} then suspected=$(lp_metric fly_loop_suspected), journal: $(cat "$lp_fixture/journal.log")"
    fi

    # (7) Row 58: the gym door, in and out. GO OBJECTIVE / GO OUT diluted by
    # eight other names, every macro `done`, no reward event, no new ground --
    # neither the four-name sequence rule, dominance nor zero-progress fires.
    # Two probes of it flag; the same window with one reward in it does not.
    lp_reset
    lp_cycle 20 "GO OBJECTIVE" "GO OUT" "GO OUT" "GO ITEM" "GO OBJECTIVE" "GO OUT" \
        "GO FRONTIER" "YES" "NO" "GO ROUTE" "NEXT" "TALK" "GO SHOP" \
        | lp_outcomes "$lp_fixture/events.jsonl" "done"
    lp_status "$lp_fixture/status.json" 1892
    lp_pass
    lp_pass
    lp_first="$(lp_metric fly_loop_suspected)"
    lp_pass
    if [ "$lp_first" = "0" ] && [ "$(lp_metric fly_loop_suspected)" = "1" ] \
       && [ "$(lp_metric fly_loop_rewards)" = "0" ] \
       && [ "$(lp_metric fly_loop_distinct_macros)" = "10" ] \
       && grep -q 'loop suspected (unrewarded): 260 decisions and no reward event' "$lp_fixture/journal.log"; then
        pass "check 10: an undo pair diluted by eight other names, all done, no reward over two probes flags as unrewarded"
    else
        fail "check 10: the row-58 log gave first=${lp_first} then suspected=$(lp_metric fly_loop_suspected) rewards=$(lp_metric fly_loop_rewards) distinct=$(lp_metric fly_loop_distinct_macros), journal: $(cat "$lp_fixture/journal.log")"
    fi
    if lp_report="$(jq -e -r '[.reason, (.window.decisions|tostring), (.window.rewards|tostring), .action] | join(" ")' "$lp_fixture/run/loop.json" 2>/dev/null)" \
       && [ "$lp_report" = "unrewarded 260 0 none" ]; then
        pass "check 10: loop.json carries the reward events in the window"
    else
        fail "check 10: loop.json read back as '${lp_report:-UNREADABLE}' — expected 'unrewarded 260 0 none'"
    fi
    lp_reset
    { cat "$lp_fixture/events.jsonl"
      printf '{"id":999998,"wallMs":1758000999998,"brainMs":150000,"kind":"reward","label":"WILD KO 54:1:1","value":0.1,"rewardKind":"wildwin"}\n'; } \
        > "$lp_fixture/events-rewarded.jsonl"
    mv -f "$lp_fixture/events-rewarded.jsonl" "$lp_fixture/events.jsonl"
    lp_pass
    lp_pass
    lp_pass
    if [ "$(lp_metric fly_loop_suspected)" = "0" ] && [ "$(lp_metric fly_loop_rewards)" = "1" ]; then
        pass "check 10: the same busy window with one reward in it does not flag"
    else
        fail "check 10: a rewarded busy window gave suspected=$(lp_metric fly_loop_suspected) rewards=$(lp_metric fly_loop_rewards)"
    fi

    # The ethos, asserted rather than reviewed: over every case above, check 10
    # restarted nothing. It reports; a human or a review agent decides.
    if [ ! -s "$lp_fixture/systemctl.log" ]; then
        pass "check 10: never acts — no unit was restarted across any of the eight cases"
    else
        fail "check 10 ACTED, which it must never do: $(cat "$lp_fixture/systemctl.log")"
    fi
    if grep -nE 'restart_unit|record_fail|escalate|maybe_reboot' "$INFRA_DIR/bin/fly-watchdog" \
        | awk -F: -v start="$(grep -n '^check_loop()' "$INFRA_DIR/bin/fly-watchdog" | cut -d: -f1)" \
               -v end="$(grep -n '^main()' "$INFRA_DIR/bin/fly-watchdog" | cut -d: -f1)" \
               '$1 > start && $1 < end' | grep -q .; then
        fail "check 10: check_loop's body mentions restart_unit/record_fail/escalate/maybe_reboot — this check must only detect and report"
    else
        pass "check 10: check_loop's body calls no restart, failure-counter or escalation helper"
    fi

    rm -rf "$lp_fixture"
fi

echo "--- 05-deploy.sh ROLE=release tag gate (temporary git repo) ---"
if [ -e /etc/pve/local ]; then
    echo "SKIP: 05-deploy.sh ROLE=release tag gate test (this box looks like the host itself: the" \
         "'accepts a tagged tree' assertion below relies on require_pve_host failing here, which it would not)"
else
    # A throwaway copy of just 05-deploy.sh + lib/common.sh, in its own git
    # repo, so infra/lib/common.sh's repo_root() (BASH_SOURCE-relative, two
    # levels up from lib/) resolves to THIS temp repo rather than the real
    # flybrain checkout — no mutating this box's own tags/tree required.
    deploy_tmp="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-deploy.XXXXXX")"
    mkdir -p "$deploy_tmp/infra/lib"
    cp "$INFRA_DIR/05-deploy.sh" "$deploy_tmp/infra/05-deploy.sh"
    cp "$INFRA_DIR/lib/common.sh" "$deploy_tmp/infra/lib/common.sh"
    chmod +x "$deploy_tmp/infra/05-deploy.sh"
    echo "lint fixture for 05-deploy.sh's ROLE=release gate" > "$deploy_tmp/README"
    (
        cd "$deploy_tmp"
        git init -q
        git config user.email lint@example.invalid
        git config user.name "flybrain lint"
        git add -A
        git commit -q -m "initial"
    )

    # Deliberately OUTSIDE deploy_tmp: an env file living inside the git
    # repo would itself be an untracked file, tripping the "tree is not
    # clean" check even in the tagged/clean case this is meant to exercise.
    envfile="$(mktemp "${TMPDIR:-/tmp}/fly-lint-deploy-env.XXXXXX")"
    {
        echo "CTID=999"
        echo "HOSTNAME=fly-lint-test"
        echo "IP=dhcp"
        echo "GAME=pokemon-red"
        echo "PUSH_TARGET=local"
        echo "ROLE=release"
    } > "$envfile"

    if untagged_out="$("$deploy_tmp/infra/05-deploy.sh" "$envfile" 2>&1)"; then
        untagged_rc=0
    else
        untagged_rc=$?
    fi
    if [ "$untagged_rc" -ne 0 ] \
       && printf '%s' "$untagged_out" | grep -qF "ROLE=release refuses to deploy: source tree at $deploy_tmp is not exactly at an annotated tag"; then
        pass "05-deploy.sh: ROLE=release refuses an untagged tree"
    else
        fail "05-deploy.sh: expected a 'not exactly at an annotated tag' refusal for an untagged tree, got (rc=$untagged_rc): $untagged_out"
    fi

    # The narrow pre-release exception (2026-09-16): PRERELEASE_UNITS=1 with
    # no tarball converges units/config/bin from an UNTAGGED tree, because a
    # brand-new release container has to be provisioned before any release
    # exists. It must pass the tag gate and then stop at require_pve_host — this
    # box is never the host — and it must NOT reach the /opt/fly/current guard
    # (that one needs pct).
    if pre_out="$(PRERELEASE_UNITS=1 "$deploy_tmp/infra/05-deploy.sh" "$envfile" 2>&1)"; then
        pre_rc=0
    else
        pre_rc=$?
    fi
    if [ "$pre_rc" -ne 0 ] \
       && printf '%s' "$pre_out" | grep -qF "must be run on the host" \
       && ! printf '%s' "$pre_out" | grep -qF "refuses to deploy"; then
        pass "05-deploy.sh: PRERELEASE_UNITS=1 converges units from an untagged tree (passed the gate, stopped at require_pve_host)"
    else
        fail "05-deploy.sh: expected PRERELEASE_UNITS=1 to pass the tag gate on an untagged tree and stop at require_pve_host, got (rc=$pre_rc): $pre_out"
    fi

    # ...but it is units-only: asking for a release artifact with it set must
    # still hit the tag gate, or the exception would be a way to deploy an
    # untagged BUILD to the release container.
    if pre_tar_out="$(PRERELEASE_UNITS=1 "$deploy_tmp/infra/05-deploy.sh" "$envfile" /nonexistent/flybrain-v0.0.0.tar.gz 2>&1)"; then
        pre_tar_rc=0
    else
        pre_tar_rc=$?
    fi
    if [ "$pre_tar_rc" -ne 0 ] \
       && printf '%s' "$pre_tar_out" | grep -qF "ROLE=release refuses to deploy"; then
        pass "05-deploy.sh: PRERELEASE_UNITS=1 does NOT bypass the tag gate when a release tarball is given"
    else
        fail "05-deploy.sh: PRERELEASE_UNITS=1 with a tarball should still be refused on an untagged tree, got (rc=$pre_tar_rc): $pre_tar_out"
    fi

    ( cd "$deploy_tmp" && git tag -a v9.9.9 -m "lint fixture tag" )

    # This box is never the host, so the tagged/clean case is expected to pass
    # the ROLE gate and then die at require_pve_host instead — proving the
    # gate let it through rather than refusing it for a tag/clean reason.
    if tagged_out="$("$deploy_tmp/infra/05-deploy.sh" "$envfile" 2>&1)"; then
        tagged_rc=0
    else
        tagged_rc=$?
    fi
    if printf '%s' "$tagged_out" | grep -qF "ROLE=release refuses to deploy"; then
        fail "05-deploy.sh: refused a clean tree tagged v9.9.9: $tagged_out"
    elif [ "$tagged_rc" -ne 0 ] && printf '%s' "$tagged_out" | grep -qF "must be run on the host"; then
        pass "05-deploy.sh: ROLE=release accepts a clean tagged tree (passed the gate, stopped at the require_pve_host check instead)"
    else
        fail "05-deploy.sh: unexpected output for a clean tagged tree (rc=$tagged_rc): $tagged_out"
    fi

    # Tagged but dirty must still be refused — the gate checks both.
    echo "dirty" >> "$deploy_tmp/README"
    if dirty_out="$("$deploy_tmp/infra/05-deploy.sh" "$envfile" 2>&1)"; then
        dirty_rc=0
    else
        dirty_rc=$?
    fi
    if [ "$dirty_rc" -ne 0 ] \
       && printf '%s' "$dirty_out" | grep -qF "ROLE=release refuses to deploy: source tree at $deploy_tmp is not clean"; then
        pass "05-deploy.sh: ROLE=release refuses a dirty tree even when tagged"
    else
        fail "05-deploy.sh: expected a 'is not clean' refusal for a dirty tagged tree, got (rc=$dirty_rc): $dirty_out"
    fi

    rm -rf "$deploy_tmp" "$envfile"
fi


# ---------------------------------------------------------------------------
# 5. de-PII guard. This repo is PUBLIC (docs/stream-mvp-plan.md's "move PII to
# The infra repo" sprint, step 4). Nothing that identifies the operator's own
# network may land in it: no hostnames, LAN addresses, container ids, host
# paths, account ids, channel names, people's names, `pass` entry names or
# forge URLs. The rules, and the keep/move/redact verdict behind every file
# that was touched, live in the operator's infra repo.
#
# Two things about the patterns below are deliberate:
#
#   * Each proper noun is spelled with ONE bracketed character — `sp[i]cy`,
#     `chonk[e]rs`, `Al[e]x`. The regex still matches the real string exactly,
#     but the literal token does not appear in this file, so the guard does not
#     itself become the last copy of what it refuses. Do not "tidy" the
#     brackets away.
#   * `operator-name` is the only case-SENSITIVE pattern. Capitalised `Al[e]x` in
#     prose is the operator; lowercase quoted `"al[e]x"` is a chat-username
#     fixture all through the tests, which the sprint explicitly leaves alone.
#
# Legitimate mentions are listed in infra/tests/de-pii-allow.txt, one
# `path pattern-name  # reason` per line. Add to it only with a reason; the
# right fix is almost always to redact instead.
# ---------------------------------------------------------------------------
echo "--- de-PII guard ---"

REPO_ROOT="$(cd "$INFRA_DIR/.." && pwd)"
DEPII_ALLOW="$INFRA_DIR/tests/de-pii-allow.txt"

DEPII_NAMES=(); DEPII_FLAGS=(); DEPII_RES=()
depii() { DEPII_NAMES+=("$1"); DEPII_FLAGS+=("$2"); DEPII_RES+=("$3"); }

#      name               grep flags   regex (one bracketed char per proper noun)
depii host-proxmox        '-i'  'sp[i]cy'
depii host-backup         '-i'  'chonk[e]rs'
depii host-llm            '-i'  '\bsl[o]th\b'
depii operator-name       ''    '\bAl[e]x\b|al[e]x_camilo|al[e]x\.camilo|al[e]x-(copy|ui)-taste'
depii lan-address         '-i'  '192\.168\.[0-9]{1,3}\.[0-9]{1,3}|\b2600:[0-9a-f]'
depii container-id        '-i'  '\b(ct|vm)[ _-]?[12][0-9][0-9]\b|\bpct +[a-z-]+ +[0-9]{2,4}\b|subvol-[12][0-9][0-9]-disk|/lxc/[12][0-9][0-9]\.conf'
depii host-root-path      ''    '/ro[o]t/'
depii home-path           ''    '/ho[m]e/[a-z]'
depii twitch-account      '-i'  '15412[6]9693|aflyplayspok[e]mon'
depii claim-log-name      ''    'AGENTS[_]LOG'
depii forge-name          '-i'  'forg[e]jo|git[e]a'
depii infra-repo-name     '-i'  'trik[i]lli'
depii mac-address         ''    '\b([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}\b'
depii email-address       ''    '[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+\.[A-Za-z]{2,}'
depii pass-entry-path     ''    '\bpass +(show|insert) +[A-Za-z0-9._-]+/'

# depii_allowed PATH PATTERN_NAME — true when de-pii-allow.txt excuses this
# pair. A path entry may end in `/` (directory prefix) or `*` (glob).
depii_allowed() {
    local path="$1" name="$2" a_path a_name rest
    [ -f "$DEPII_ALLOW" ] || return 1
    while read -r a_path a_name rest; do
        case "$a_path" in ''|'#'*) continue ;; esac
        [ "$a_name" = "$name" ] || [ "$a_name" = '*' ] || continue
        case "$a_path" in
            */) case "$path" in "$a_path"*) return 0 ;; esac ;;
            *\**) # shellcheck disable=SC2254
                  case "$path" in $a_path) return 0 ;; esac ;;
            *)    [ "$path" = "$a_path" ] && return 0 ;;
        esac
    done < "$DEPII_ALLOW"
    return 1
}

# The tree, minus what the sprint deliberately excludes: the connectome data
# (neuron ids and column CSVs, no house strings), the npm lockfile, and
# .local/ (untracked anyway). Prefer `git grep` — it honours .gitignore and is
# an order of magnitude faster — and fall back to grep -r for a checkout that
# is not a git repo (a release tarball unpacked on the host).
depii_scan() {
    local flags="$1" re="$2"
    if git -C "$REPO_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
        # shellcheck disable=SC2086
        git -C "$REPO_ROOT" grep -nI $flags -E -e "$re" -- \
            . ':!data' ':!package-lock.json' 2>/dev/null || true
    else
        # shellcheck disable=SC2086
        grep -rnI $flags -E -e "$re" "$REPO_ROOT" \
            --exclude-dir=data --exclude-dir=.git --exclude-dir=node_modules \
            --exclude-dir=.local --exclude-dir=target --exclude=package-lock.json \
            2>/dev/null | sed "s|^${REPO_ROOT}/||" || true
    fi
}

DEPII_HITS=0
for i in "${!DEPII_NAMES[@]}"; do
    name="${DEPII_NAMES[$i]}"
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        hit_path="${line%%:*}"
        depii_allowed "$hit_path" "$name" && continue
        fail "de-PII: ${name}: ${line}"
        DEPII_HITS=$((DEPII_HITS + 1))
    done < <(depii_scan "${DEPII_FLAGS[$i]}" "${DEPII_RES[$i]}")
done

if [ "$DEPII_HITS" -eq 0 ]; then
    pass "de-PII guard: no identifying strings outside tests/de-pii-allow.txt (${#DEPII_NAMES[@]} patterns)"
else
    echo "       ^ redact these (rules in the operator's infra repo), or, if the" >&2
    echo "         mention is genuinely legitimate, add it to infra/tests/de-pii-allow.txt with a reason." >&2
fi

echo "==="
if [ "$FAILED" -eq 0 ]; then
    echo "lint.sh: ALL CHECKS PASSED"
else
    echo "lint.sh: FAILURES ABOVE"
fi
exit "$FAILED"
