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
        # the privacy guard lives in tools/ because the pre-push hook and
        # tag-release run it too.
        find "$INFRA_DIR/../tools" -maxdepth 1 -name '*.sh' -type f
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
    cmd="$(echo "$line" | sed -E 's/^Exec(Start|StartPre|StartPost)=//' | sed -E 's/^[-+@!:]+//' | awk '{print $1}')"
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
# ---------------------------------------------------------------------------
# 3b3. The shadow run (SHADOW-01, infra/units/flyshadow.service).
#
# Report-only and off by default. What would break it is statically visible: the unit ending up
# in a target or 07-enable's list; being bound to flysim (every flysim restart would take the
# shadow with it, and it must follow restarts, not die of them); restarting after a divergence
# (the verdict must stay); losing its idle scheduling (it must never take a live cycle); or
# landing on flysim's cpuset.
# ---------------------------------------------------------------------------
echo "--- flyshadow.service: off by default, report-only, idle, never bound to flysim ---"
SHADOW_UNIT="$INFRA_DIR/units/flyshadow.service"
if [ ! -f "$SHADOW_UNIT" ]; then
    fail "units/flyshadow.service is missing"
else
    grep -qE '^ExecStart=/opt/fly/current/fly-shadow run$' "$SHADOW_UNIT" \
        && pass "flyshadow.service runs the release's fly-shadow" \
        || fail "flyshadow.service ExecStart must be /opt/fly/current/fly-shadow run"
    grep -qE '^ConditionPathExists=/opt/fly/current/fly-shadow$' "$SHADOW_UNIT" \
        && pass "flyshadow.service stays inactive on a release without fly-shadow" \
        || fail "flyshadow.service needs ConditionPathExists=/opt/fly/current/fly-shadow"
    if grep -qE '^(Requires|BindsTo|PartOf|Requisite)=.*flysim' "$SHADOW_UNIT"; then
        fail "flyshadow.service must not be bound to flysim.service: it follows flysim's restarts"
    else
        pass "flyshadow.service outlives flysim restarts (not bound to flysim.service)"
    fi
    grep -qE '^RestartPreventExitStatus=3$' "$SHADOW_UNIT" \
        && pass "flyshadow.service stays stopped after a divergence (exit 3)" \
        || fail "flyshadow.service needs RestartPreventExitStatus=3 so a diverged verdict stays"
    grep -qE '^CPUSchedulingPolicy=idle$' "$SHADOW_UNIT" \
        && pass "flyshadow.service runs SCHED_IDLE" \
        || fail "flyshadow.service must be CPUSchedulingPolicy=idle: it may only use idle CPU"
    grep -qE '^\[Install\]' "$SHADOW_UNIT" \
        && fail "flyshadow.service has an [Install] section; it is started by fly-shadow-run only" \
        || pass "flyshadow.service has no [Install] section (never enabled)"
fi
if target_pulls "$INFRA_DIR/units/fly.target" | grep -qx 'flyshadow.service'; then
    fail "fly.target pulls flyshadow.service in; it must stay off until fly-shadow-run starts it"
else
    pass "fly.target does not pull flyshadow.service in"
fi
if grep -E '^(ALWAYS_ON_UNITS|APP_UNITS)=' "$INFRA_DIR/07-enable.sh" "$INFRA_DIR/verify.sh" | grep -q 'flyshadow'; then
    fail "07-enable.sh or verify.sh lists flyshadow.service as always-on"
else
    pass "07-enable.sh and verify.sh leave flyshadow.service alone"
fi
for u in flyshadow-guard.timer flyshadow-guard.service; do
    if [ ! -f "$INFRA_DIR/units/$u" ]; then
        fail "units/$u is missing"
    elif grep -qE '^\[Install\]' "$INFRA_DIR/units/$u"; then
        fail "$u has an [Install] section; fly-shadow-run starts and stops it"
    else
        pass "$u is never enabled (started by fly-shadow-run only)"
    fi
done
# The guard's rule, driven for real (python3 is on every container this repo provisions) over
# RTF traces: synthetic ones shaped on the release CT's own numbers (tests/shadow_guard_traces.py)
# and ones recorded from a real flysim with and without a shadow (tests/fixtures/shadow-guard/).
# A fly that was already below real time must never trip it; a genuine degradation must.
if command -v python3 >/dev/null 2>&1; then
    g_tmp="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-guard.XXXXXX")"
    guard_trace() { # name, dir with baseline.jsonl and checks.jsonl, expect (pass|trip)
        local out rc=0
        out="$("$INFRA_DIR/bin/fly-shadow-run" simulate "$2/baseline.jsonl" "$2/checks.jsonl")" || rc=$?
        if { [ "$3" = pass ] && [ "$rc" = 0 ]; } || { [ "$3" = trip ] && [ "$rc" = 1 ]; }; then
            pass "fly-shadow-run guard, $1: ${out%% (baseline*}"
        else
            fail "fly-shadow-run guard, $1: expected $3, got exit $rc: $out"
        fi
    }
    while read -r name expect; do
        guard_trace "$name" "$g_tmp/$name" "$expect"
    done < <(python3 "$INFRA_DIR/tests/shadow_guard_traces.py" "$g_tmp")
    for d in "$INFRA_DIR"/tests/fixtures/shadow-guard/*/; do
        [ -f "$d/expect" ] || continue
        guard_trace "recorded $(basename "$d")" "$d" "$(cat "$d/expect")"
    done
    # The guard's lifecycle and `check`'s preconditions (review round 3, G1-a), with a fake systemctl
    # (`show` answers FAKE_SHADOW_STATE; the timer is active when FAKE_TIMER_ACTIVE=yes), a fake `id`
    # (root) and a metrics URL nobody listens on (a live guard then reports "not judged" and stays).
    gl_bin="$g_tmp/lifecycle-bin"; gl_dir="$g_tmp/lifecycle-shadow"
    mkdir -p "$gl_bin" "$gl_dir"
    cat > "$gl_bin/systemctl" <<'GLSYSTEMCTL'
#!/bin/sh
echo "$*" >> "$FAKE_SYSTEMCTL_LOG"
case "$1" in
    show) printf 'ActiveState=%s\nSubState=x\n' "$FAKE_SHADOW_STATE" ;;
    is-active)
        case "$3" in
            flyshadow.service) [ "$FAKE_SHADOW_STATE" = active ]; exit $? ;;
            flyshadow-guard.timer) [ "$FAKE_TIMER_ACTIVE" = yes ]; exit $? ;;
            *) exit 3 ;;
        esac ;;
esac
exit 0
GLSYSTEMCTL
    cat > "$gl_bin/id" <<'GLID'
#!/bin/sh
[ "$1" = -u ] && { echo 0; exit 0; }
exec /usr/bin/id "$@"
GLID
    chmod +x "$gl_bin/systemctl" "$gl_bin/id"
    gl_run() { # args to fly-shadow-run; env FAKE_SHADOW_STATE, FAKE_TIMER_ACTIVE from the caller
        : > "$g_tmp/systemctl.log"
        PATH="$gl_bin:$PATH" FAKE_SYSTEMCTL_LOG="$g_tmp/systemctl.log" FLY_SHADOW_DIR="$gl_dir" \
            FLY_METRICS_URL=http://127.0.0.1:1 FLY_SHADOW_BIN="$g_tmp/none" \
            "$INFRA_DIR/bin/fly-shadow-run" "$@" 2>&1
    }
    printf '{"rtfMean":0.66,"rtfSd":0.01,"lagRate":0.3,"samples":60,"margin":0.05}\n' > "$gl_dir/baseline.json"
    printf '{}\n' > "$gl_dir/guard-state.json"
    for st in activating deactivating; do
        FAKE_SHADOW_STATE=$st FAKE_TIMER_ACTIVE=yes gl_run guard > "$g_tmp/out" || true
        if grep -q 'stop flyshadow-guard.timer' "$g_tmp/systemctl.log"; then
            fail "fly-shadow-run guard stopped its timer while flyshadow was $st (auto-restart): $(cat "$g_tmp/out")"
        else
            pass "fly-shadow-run guard keeps running while flyshadow is $st (crash-restart delay)"
        fi
    done
    for st in inactive failed; do
        FAKE_SHADOW_STATE=$st FAKE_TIMER_ACTIVE=yes gl_run guard > "$g_tmp/out" || true
        if grep -q 'stop flyshadow-guard.timer' "$g_tmp/systemctl.log"; then
            pass "fly-shadow-run guard stops its timer when flyshadow is $st"
        else
            fail "fly-shadow-run guard kept running with flyshadow $st: $(cat "$g_tmp/out")"
        fi
    done
    gl_check() { # name, expected (refused|passes-gates), then the env
        local out rc=0
        out="$(gl_run check)" || rc=$?
        case "$2:$rc" in
            refused:1) pass "fly-shadow-run check, $1: ${out%%(*}" ;;
            passes-gates:2) pass "fly-shadow-run check, $1: past the guard preconditions" ;;
            *) fail "fly-shadow-run check, $1: expected $2, got exit $rc: $out" ;;
        esac
    }
    FAKE_SHADOW_STATE=active FAKE_TIMER_ACTIVE=yes gl_check "guarded run" passes-gates
    FAKE_SHADOW_STATE=active FAKE_TIMER_ACTIVE=no gl_check "guard timer not active" refused
    FAKE_SHADOW_STATE=activating FAKE_TIMER_ACTIVE=yes gl_check "shadow restarting" refused
    mv "$gl_dir/baseline.json" "$gl_dir/baseline.json.off"
    FAKE_SHADOW_STATE=active FAKE_TIMER_ACTIVE=yes gl_check "no baseline.json" refused
    mv "$gl_dir/baseline.json.off" "$gl_dir/baseline.json"
    echo '{}' > "$gl_dir/guard-tripped.json"
    FAKE_SHADOW_STATE=active FAKE_TIMER_ACTIVE=yes gl_check "guard tripped" refused
    rm -rf "$g_tmp"
else
    fail "python3 is needed to test fly-shadow-run's guard rule"
fi
# N2: `start` refuses, starting nothing, unless the cpuset drop-in exists and is disjoint from flysim's.
cs_tmp="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-cpuset.XXXXXX")"
mkdir -p "$cs_tmp/bin" "$cs_tmp/shadow"
cat > "$cs_tmp/bin/systemctl" <<'CSSTUB'
#!/bin/sh
echo "$*" >> "$CS_DIR/systemctl.log"
case "$1" in
    is-active) exit 3 ;;
    show) case "$5" in
              flyshadow.service) echo "$CS_SHADOW_CPUS" ;;
              flysim.service) echo "$CS_SIM_CPUS" ;;
          esac ;;
esac
exit 0
CSSTUB
printf '#!/bin/sh\n[ "$1" = -u ] && { echo 0; exit 0; }\nexec /usr/bin/id "$@"\n' > "$cs_tmp/bin/id"
printf '#!/bin/sh\nexit 0\n' > "$cs_tmp/fly-shadow"
printf '#!/bin/sh\necho "stub install reached"\nexit 1\n' > "$cs_tmp/bin/install"
chmod +x "$cs_tmp/bin/install" "$cs_tmp/bin/systemctl" "$cs_tmp/bin/id" "$cs_tmp/fly-shadow"
cs_start() { # name, expect (refuse|proceed), shadow cpus, sim cpus, drop-in (yes|no)
    local rc=0 out
    : > "$cs_tmp/systemctl.log"
    rm -f "$cs_tmp/dropin.conf"
    [ "$5" = yes ] && : > "$cs_tmp/dropin.conf"
    out="$(PATH="$cs_tmp/bin:$PATH" CS_DIR="$cs_tmp" CS_SHADOW_CPUS="$3" CS_SIM_CPUS="$4" \
        FLY_SHADOW_DIR="$cs_tmp/shadow" FLY_SHADOW_BIN="$cs_tmp/fly-shadow" \
        FLY_SHADOW_CPUSET_DROPIN="$cs_tmp/dropin.conf" FLY_SHADOW_BASELINE_SECONDS=1 \
        FLY_SHADOW_REMOTE_ENV="$cs_tmp/no-remote.env" \
        FLY_METRICS_URL=http://127.0.0.1:1 "$INFRA_DIR/bin/fly-shadow-run" start --local 2>&1)" || rc=$?
    case "$2" in
        refuse)
            if [ "$rc" -ne 0 ] && ! grep -qE '^(start|restart|daemon-reload)' "$cs_tmp/systemctl.log" \
                && [ ! -d "$cs_tmp/shadow/trace" ] && echo "$out" | grep -q 'nothing started'; then
                pass "fly-shadow-run start refuses, $1"
            else
                fail "fly-shadow-run start must refuse and start nothing, $1 (rc=$rc): $out"
            fi ;;
        proceed)
            # past the cpuset check it goes on to install -d (stubbed to stop there)
            if echo "$out" | grep -q 'stub install reached'; then
                pass "fly-shadow-run start proceeds, $1"
            else
                fail "fly-shadow-run start must pass the cpuset check, $1 (rc=$rc): $out"
            fi ;;
    esac
}
if command -v python3 >/dev/null 2>&1; then
    cs_start "no cpuset drop-in" refuse "0-7" "1,3,5,7" no
    cs_start "AllowedCPUs overlapping flysim's" refuse "0 2 3" "1 3 5 7" yes
    cs_start "AllowedCPUs empty" refuse "" "1 3 5 7" yes
    cs_start "AllowedCPUs disjoint (ranges)" proceed "0 2 4 6" "1 3 5 7" yes
    cs_start "AllowedCPUs disjoint (range syntax)" proceed "0-2" "3-7" yes
else
    fail "python3 is needed to test fly-shadow-run's cpuset check"
fi
# SHADOW-02: the shadow runs on a build box. `start` without the operator's remote file refuses
# (unless --local); with one whose key is missing it refuses too, starting nothing either way.
rs_start() { # name, remote env file content (or "none")
    local rc=0 out
    : > "$cs_tmp/systemctl.log"
    rm -f "$cs_tmp/remote.env"
    [ "$2" = none ] || printf '%s\n' "$2" > "$cs_tmp/remote.env"
    out="$(PATH="$cs_tmp/bin:$PATH" CS_DIR="$cs_tmp" CS_SHADOW_CPUS="0 2" CS_SIM_CPUS="1 3" \
        FLY_SHADOW_DIR="$cs_tmp/shadow" FLY_SHADOW_BIN="$cs_tmp/fly-shadow" \
        FLY_SHADOW_CPUSET_DROPIN="$cs_tmp/dropin.conf" FLY_SHADOW_REMOTE_ENV="$cs_tmp/remote.env" \
        FLY_SHADOW_REMOTE_DROPIN_DIR="$cs_tmp/flyshadow.d" \
        "$INFRA_DIR/bin/fly-shadow-run" start 2>&1)" || rc=$?
    if [ "$rc" -ne 0 ] && ! grep -qE '^(start|restart|daemon-reload)' "$cs_tmp/systemctl.log" \
        && [ ! -e "$cs_tmp/flyshadow.d/remote.conf" ]; then
        pass "fly-shadow-run start refuses, $1: ${out#fly-shadow-run: }"
    else
        fail "fly-shadow-run start must refuse and start nothing, $1 (rc=$rc): $out"
    fi
}
: > "$cs_tmp/dropin.conf"
rs_start "no remote file (the shadow runs on a build box; --local to override)" none
rs_start "a remote file without its key" "FLY_SHADOW_REMOTE=user@box
FLY_SHADOW_REMOTE_KEY=$cs_tmp/missing-key
FLY_SHADOW_REMOTE_KNOWN_HOSTS=$cs_tmp/missing-known"
# BUS-01: only the legacy loop writes the trace, so `start` refuses while flysim.service runs the
# session runtime; `--bus` needs the release's fly-session worker beside fly-shadow.
bs_start() { # name, expect (refuse|proceed), runtime drop-in (yes|no), worker (yes|no), args...
    local name="$1" expect="$2" rt="$3" worker="$4" rc=0 out
    shift 4
    : > "$cs_tmp/systemctl.log"
    rm -f "$cs_tmp/10-runtime.conf" "$cs_tmp/fly-session"
    [ "$rt" = yes ] && printf '[Service]\nExecStart=/opt/fly/current/flysim-session\n' > "$cs_tmp/10-runtime.conf"
    [ "$worker" = yes ] && { printf '#!/bin/sh\nexit 0\n' > "$cs_tmp/fly-session"; chmod +x "$cs_tmp/fly-session"; }
    out="$(PATH="$cs_tmp/bin:$PATH" CS_DIR="$cs_tmp" CS_SHADOW_CPUS="0 2" CS_SIM_CPUS="1 3" \
        FLY_SHADOW_DIR="$cs_tmp/shadow" FLY_SHADOW_BIN="$cs_tmp/fly-shadow" \
        FLY_SHADOW_CPUSET_DROPIN="$cs_tmp/dropin.conf" FLY_SHADOW_REMOTE_ENV="$cs_tmp/no-remote.env" \
        FLY_SHADOW_REMOTE_DROPIN_DIR="$cs_tmp/flyshadow.d" FLY_RUNTIME_DROPIN="$cs_tmp/10-runtime.conf" \
        FLY_SHADOW_BASELINE_SECONDS=30 FLY_METRICS_URL=http://127.0.0.1:1 \
        "$INFRA_DIR/bin/fly-shadow-run" start "$@" 2>&1)" || rc=$?
    case "$expect" in
        refuse)
            if [ "$rc" -ne 0 ] && ! grep -qE '^(start|restart|daemon-reload)' "$cs_tmp/systemctl.log" \
                && [ ! -e "$cs_tmp/flyshadow.d/mode.conf" ]; then
                pass "fly-shadow-run start refuses, $name: ${out#fly-shadow-run: }"
            else
                fail "fly-shadow-run start must refuse and start nothing, $name (rc=$rc): $out"
            fi ;;
        proceed)
            if echo "$out" | grep -q 'stub install reached'; then
                pass "fly-shadow-run start proceeds, $name"
            else
                fail "fly-shadow-run start must proceed, $name (rc=$rc): $out"
            fi ;;
    esac
}
bs_start "the live fly on the session runtime (no trace to follow)" refuse yes yes --local
bs_start "--bus without fly-session in the release" refuse no no --local --bus
bs_start "--bus with fly-session, live on legacy" proceed no yes --local --bus
rm -rf "$cs_tmp"
# BUS-01: `check --bus` asks fly-shadow for a verdict that ran the bus topology.
if command -v python3 >/dev/null 2>&1; then
    cb_tmp="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-checkbus.XXXXXX")"
    mkdir -p "$cb_tmp/bin" "$cb_tmp/shadow"
    printf '#!/bin/sh\ncase "$1" in is-active) exit 0 ;; esac\nexit 0\n' > "$cb_tmp/bin/systemctl"
    printf '#!/bin/sh\necho "$*" > "%s/args"\n' "$cb_tmp" > "$cb_tmp/fly-shadow"
    chmod +x "$cb_tmp/bin/systemctl" "$cb_tmp/fly-shadow"
    printf '{"rtfMean":1,"rtfSd":0,"lagRate":0,"samples":60,"margin":0.05}\n' > "$cb_tmp/shadow/baseline.json"
    printf '{"status":"pass"}\n' > "$cb_tmp/shadow/verdict.json"
    PATH="$cb_tmp/bin:$PATH" FLY_SHADOW_DIR="$cb_tmp/shadow" FLY_SHADOW_BIN="$cb_tmp/fly-shadow" \
        FLY_SHADOW_REMOTE_DROPIN_DIR="$cb_tmp/none.d" FLY_ENV_FILE="$cb_tmp/none.env" \
        "$INFRA_DIR/bin/fly-shadow-run" check --bus >/dev/null 2>&1 || true
    if grep -q -- '--binary flysim-session --require-arm bus/process$' "$cb_tmp/args" 2>/dev/null; then
        pass "fly-shadow-run check --bus requires a verdict that ran bus/process (fly-shadow check --require-arm)"
    else
        fail "fly-shadow-run check --bus must pass --require-arm bus/process to fly-shadow check ($(cat "$cb_tmp/args" 2>/dev/null))"
    fi
    rm -rf "$cb_tmp"
fi
# SHADOW-02: `check` in remote mode also needs the relay healthy now (relay.json under a minute
# old), of this run (the drop-in's id, the verdict's too), without coverageLost (review B1: flysim
# stopped its trace for want of a consumer) and with a live trace written in the last 90 s.
if command -v python3 >/dev/null 2>&1; then
    rc_tmp="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-relay.XXXXXX")"
    mkdir -p "$rc_tmp/bin" "$rc_tmp/shadow" "$rc_tmp/flyshadow.d"
    printf '#!/bin/sh\ncase "$1" in is-active) exit 0 ;; esac\nexit 0\n' > "$rc_tmp/bin/systemctl"
    chmod +x "$rc_tmp/bin/systemctl"
    printf '[Service]\nEnvironment=FLY_SHADOW_RUN_ID=1790000000000\n' > "$rc_tmp/flyshadow.d/remote.conf"
    printf '{"rtfMean":1,"rtfSd":0,"lagRate":0,"samples":60,"margin":0.05}\n' > "$rc_tmp/shadow/baseline.json"
    rc_check() { # name, expect (refused|passes-gates), relay.json (or none), verdict.json (default: this run's), [local]
        local rc=0 out dropin_dir="$rc_tmp/flyshadow.d"
        rm -f "$rc_tmp/shadow/relay.json"
        [ "$3" = none ] || printf '%s\n' "$3" > "$rc_tmp/shadow/relay.json"
        printf '%s\n' "${4:-{\"runId\":\"1790000000000\",\"status\":\"running\",\"window\":{\"lastStopTrace\":null\}\}}" > "$rc_tmp/shadow/verdict.json"
        [ "${5:-}" = local ] && dropin_dir="$rc_tmp/none.d"
        out="$(PATH="$rc_tmp/bin:$PATH" FLY_SHADOW_DIR="$rc_tmp/shadow" FLY_SHADOW_BIN="$rc_tmp/none" \
            FLY_SHADOW_REMOTE_DROPIN_DIR="$dropin_dir" "$INFRA_DIR/bin/fly-shadow-run" check 2>&1)" || rc=$?
        case "$2:$rc" in
            refused:1) pass "fly-shadow-run check (remote), $1: ${out#cutover refused: }" ;;
            passes-gates:2) pass "fly-shadow-run check (remote), $1: past the relay precondition" ;;
            *) fail "fly-shadow-run check (remote), $1: expected $2, got exit $rc: $out" ;;
        esac
    }
    now="$(date -u +%Y-%m-%dT%H:%M:%S.000Z)"
    old="$(date -u -d '-5 min' +%Y-%m-%dT%H:%M:%S.000Z)"
    good="\"updatedAt\":\"$now\",\"runId\":\"1790000000000\",\"healthy\":true,\"coverageLost\":null,\"traceAgeSeconds\":12"
    rc_check "no relay.json" refused none
    rc_check "relay not healthy" refused "{\"updatedAt\":\"$now\",\"runId\":\"1790000000000\",\"healthy\":false,\"connected\":true,\"remoteAlive\":false,\"remoteWhy\":\"the shadow has no heartbeat\"}"
    rc_check "relay.json 5 minutes old" refused "{\"updatedAt\":\"$old\",\"runId\":\"1790000000000\",\"healthy\":true,\"traceAgeSeconds\":1}"
    rc_check "relay healthy, run, trace current" passes-gates "{$good}"
    # B1: healthy and caught up, but the live trace ended in a no-consumer stop the shadow has not reached.
    rc_check "coverage lost while the relay reports healthy (B1)" refused "{\"updatedAt\":\"$now\",\"runId\":\"1790000000000\",\"healthy\":true,\"coverageLost\":{\"trace\":\"trace-1790000005000-4242.jsonl\",\"window\":\"w1\"},\"traceAgeSeconds\":12}"
    rc_check "live trace not written for 5 minutes" refused "{\"updatedAt\":\"$now\",\"runId\":\"1790000000000\",\"healthy\":true,\"coverageLost\":null,\"traceAgeSeconds\":300}"
    rc_check "no live trace file" refused "{\"updatedAt\":\"$now\",\"runId\":\"1790000000000\",\"healthy\":true,\"coverageLost\":null,\"traceAgeSeconds\":null}"
    rc_check "relay.json of another run" refused "{\"updatedAt\":\"$now\",\"runId\":\"1789999999999\",\"healthy\":true,\"coverageLost\":null,\"traceAgeSeconds\":12}"
    # B2: a verdict that does not carry its window (an older shadow) may count time before a gap.
    rc_check "a verdict without a window (B2)" refused "{$good}" "{\"runId\":\"1790000000000\",\"status\":\"pass\"}"
    rc_check "a verdict with a window, after a stop (B2)" passes-gates "{$good}" "{\"runId\":\"1790000000000\",\"status\":\"pass\",\"window\":{\"lastStopTrace\":\"trace-1790000005000-4242.jsonl\"}}"
    rc_check "verdict of another run" refused "{$good}" "{\"runId\":\"1789999999999\",\"status\":\"pass\"}"
    rc_check "a local run's verdict (no run id) in remote mode" refused "{$good}" "{\"runId\":null,\"status\":\"pass\"}"
    rc_check "a remote verdict without a remote drop-in" refused "{$good}" "{\"runId\":\"1790000000000\",\"status\":\"pass\"}" local
    rc_check "a local verdict and no drop-in" passes-gates none "{\"runId\":null,\"status\":\"pass\"}" local
    rm -rf "$rc_tmp"
fi
# SHADOW-02: the build box's units live outside infra/units (05-deploy converges every unit there
# onto the release container) and keep the remote shadow's semantics.
BOX_UNIT="$INFRA_DIR/box/flyshadow-remote.service"
if ls "$INFRA_DIR"/units/flyshadow-remote* >/dev/null 2>&1; then
    fail "the build box's flyshadow-remote units must not be in infra/units (05-deploy would install them on the release container)"
elif [ ! -f "$BOX_UNIT" ] || [ ! -f "$INFRA_DIR/box/flyshadow-remote.path" ]; then
    fail "infra/box/flyshadow-remote.service and .path are missing"
else
    grep -qE '^ExecStart=/opt/fly/current/fly-shadow run --run-id-file /srv/fly-shadow-remote/run-id --lag-guard-margin 0( |$)' "$BOX_UNIT" \
        && pass "flyshadow-remote.service runs the release's fly-shadow on the mirror, for the relay's run" \
        || fail "flyshadow-remote.service must run /opt/fly/current/fly-shadow run --run-id-file /srv/fly-shadow-remote/run-id --lag-guard-margin 0"
    { grep -qE '^Restart=always$' "$BOX_UNIT" && grep -qE '^RestartPreventExitStatus=3$' "$BOX_UNIT"; } \
        && pass "flyshadow-remote.service restarts for a new run (exit 4) and stays stopped after a divergence (exit 3)" \
        || fail "flyshadow-remote.service needs Restart=always and RestartPreventExitStatus=3"
    grep -qE '^CPUSchedulingPolicy=idle' "$BOX_UNIT" \
        && fail "flyshadow-remote.service must not be idle-scheduled: it has its own cores" \
        || pass "flyshadow-remote.service runs at normal priority on its own cores"
    grep -qE '^PathChanged=/srv/fly-shadow-remote/run-id$' "$INFRA_DIR/box/flyshadow-remote.path" \
        && pass "flyshadow-remote.path starts the shadow on a new run id" \
        || fail "flyshadow-remote.path must watch /srv/fly-shadow-remote/run-id"
fi
SETUP="$INFRA_DIR/box/fly-shadow-remote-setup"
bash -n "$SETUP" && pass "fly-shadow-remote-setup parses" || fail "fly-shadow-remote-setup does not parse"
# Review N3: the key file is outside anything flyshadow owns; no symlink is followed as root; --from is required.
grep -qE '^HOME_DIR=/srv/fly-shadow-remote-home$' "$SETUP" \
    && grep -qF 'install -d -o root -g root -m 0755 "$HOME_DIR/.ssh"' "$SETUP" \
    && grep -qF 'authorized_keys.new' "$SETUP" && ! grep -qF '"$ROOT/.ssh/authorized_keys' "$SETUP" \
    && pass "fly-shadow-remote-setup keeps authorized_keys in a root-owned home, not in the flyshadow-owned mirror" \
    || fail "fly-shadow-remote-setup must put .ssh in a root-owned home (HOME_DIR), not in the mirror"
if grep -nE '^[[:space:]]*(install -d|chown|chmod)[^#]*"\$ROOT' "$SETUP" | grep -v -- 'chown -h' | grep -q .; then
    fail "fly-shadow-remote-setup must not install -d / chown / chmod under the flyshadow-owned mirror (it follows symlinks as root)"
else
    pass "fly-shadow-remote-setup never chowns or chmods under the mirror without -h, nor installs into it"
fi
grep -qF '[ -L "$ROOT/$d" ] && die' "$SETUP" \
    && pass "fly-shadow-remote-setup refuses a symlink in the mirror on a re-run" \
    || fail "fly-shadow-remote-setup must refuse a symlink under the mirror"
grep -qF '[ -n "$from" ] || die "--from is required' "$SETUP" \
    && pass "fly-shadow-remote-setup requires --from" \
    || fail "fly-shadow-remote-setup must require --from"
grep -qF 'restrict,command=\"/opt/fly/current/fly-shadow ingest --root $ROOT\"' "$INFRA_DIR/box/fly-shadow-remote-setup" \
    && pass "fly-shadow-remote-setup limits the relay key to the ingest (restrict, forced command)" \
    || fail "fly-shadow-remote-setup must write restrict,command=\"/opt/fly/current/fly-shadow ingest --root \$ROOT\""
grep -qF 'echo "flyshadow.service ${page},${enc}"' "$INFRA_DIR/lib/common.sh" \
    && pass "cpuset_dropin_plan keeps flyshadow.service off flysim's CPUs (page + encoder; checked disjoint in section 3c2)" \
    || fail "cpuset_dropin_plan must give flyshadow.service the page and encoder CPUs, never flysim's"
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
# (the cpuset drop-ins for flyedge.service and flysim-session.service are checked from the plan, section 3c2)
if grep -qE '^Environment=FLY_FEED_VIA' "$INFRA_DIR/units/flysim.service"; then
    fail "flysim.service pins FLY_FEED_VIA; it belongs to fly.env so a box can be switched by deploy"
else
    pass "flysim.service leaves FLY_FEED_VIA to fly.env"
fi

# ---------------------------------------------------------------------------
# 3b3. The session runtime (SERVE-01): flysim-session, its standalone unit and
# the fly-runtime switch CUT-01 calls.
#
# flysim.service stays the fly's one unit name; `fly-runtime session` points it at
# flysim-session with a drop-in, `fly-runtime legacy` removes it. What would break
# that: the standalone unit drifting from flysim.service (limits, env, ports),
# becoming enable-able or pulled in beside flysim, the drop-in naming another
# binary or session dir, the release not shipping the binary, the deploy
# forgetting it. fly-runtime itself is driven for real against stubs.
# ---------------------------------------------------------------------------
echo "--- the session runtime: flysim-session.service and fly-runtime ---"
SESSION_UNIT="$INFRA_DIR/units/flysim-session.service"
if [ ! -f "$SESSION_UNIT" ]; then
    fail "units/flysim-session.service is missing"
else
    grep -qE '^ExecStart=/opt/fly/current/flysim-session$' "$SESSION_UNIT" \
        && pass "flysim-session.service runs the release's flysim-session" \
        || fail "flysim-session.service ExecStart must be /opt/fly/current/flysim-session"
    grep -qE '^Conflicts=flysim\.service$' "$SESSION_UNIT" \
        && pass "flysim-session.service Conflicts=flysim.service (same ports, same stores)" \
        || fail "flysim-session.service must Conflict with flysim.service"
    if grep -qE '^\[Install\]' "$SESSION_UNIT"; then
        fail "flysim-session.service has an [Install] section; it must not be enable-able beside flysim"
    else
        pass "flysim-session.service cannot be enabled (no [Install])"
    fi
    # The [Service] section, comments and blank lines dropped: flysim.service's, line for
    # line, but for ExecStart and the one variable of its own.
    service_lines() {
        awk '/^\[/{sec=$0; next} sec=="[Service]" && !/^[[:space:]]*(#|$)/' "$1" \
            | grep -vE '^(ExecStart=|Environment=FLY_SESSION_DIR=|Environment=MALLOC_ARENA_MAX=)' || true
    }
    if [ "$(service_lines "$INFRA_DIR/units/flysim.service")" = "$(service_lines "$SESSION_UNIT")" ]; then
        pass "flysim-session.service [Service] is flysim.service's but for ExecStart and FLY_SESSION_DIR"
    else
        fail "flysim-session.service [Service] drifted from flysim.service: $(diff <(service_lines "$INFRA_DIR/units/flysim.service") <(service_lines "$SESSION_UNIT") | tr '\n' ' ')"
    fi
    grep -qx 'Environment=MALLOC_ARENA_MAX=2' "$SESSION_UNIT" \
        && ! grep -q 'MALLOC_ARENA_MAX' "$INFRA_DIR/units/flysim.service" \
        && pass "flysim-session.service sets MALLOC_ARENA_MAX=2; legacy flysim.service is untouched" \
        || fail "flysim-session.service must set MALLOC_ARENA_MAX=2 (and flysim.service must not)"
    grep -qE '^Environment=FLY_SESSION_DIR=/run/fly/session$' "$SESSION_UNIT" \
        && pass "flysim-session.service keeps its session dir on /run/fly/session" \
        || fail "flysim-session.service must set FLY_SESSION_DIR=/run/fly/session"
fi
if target_pulls "$INFRA_DIR/units/fly.target" | grep -qx 'flysim-session.service'; then
    fail "fly.target pulls flysim-session.service in beside flysim.service"
else
    pass "fly.target does not pull flysim-session.service in"
fi
if grep -E '^(ALWAYS_ON_UNITS|APP_UNITS)=' "$INFRA_DIR/07-enable.sh" "$INFRA_DIR/verify.sh" | grep -q 'flysim-session'; then
    fail "07-enable.sh or verify.sh lists flysim-session.service"
else
    pass "07-enable.sh and verify.sh leave flysim-session.service alone"
fi
grep -qE '^d /run/fly/session +0700 fly +fly' "$INFRA_DIR/config/fly-tmpfiles.conf" \
    && pass "tmpfiles creates /run/fly/session 0700 fly" \
    || fail "config/fly-tmpfiles.conf must create /run/fly/session 0700 fly fly"
grep -qE 'for name in .*\bfly-runtime\b.*; do$' "$INFRA_DIR/05-deploy.sh" \
    && pass "05-deploy.sh installs fly-runtime" \
    || fail "05-deploy.sh must converge bin/fly-runtime to /opt/fly/bin"
grep -qF 'flysim.service.d/10-runtime.conf' "$INFRA_DIR/05-deploy.sh" \
    && grep -qF '"${release_path}/flysim-session"' "$INFRA_DIR/05-deploy.sh" \
    && pass "05-deploy.sh refuses a release without flysim-session while the session runtime runs" \
    || fail "05-deploy.sh must refuse a release without flysim-session while 10-runtime.conf is present"
grep -qE -- '--bin flysim-session --bin fly-shadow --bin fly-session' "$INFRA_DIR/build/build-flysim.sh" \
    && grep -qE 'for extra in flysim-session fly-shadow fly-session' "$INFRA_DIR/build/package-release.sh" \
    && pass "build-flysim.sh builds flysim-session and package-release.sh ships it (with fly-shadow)" \
    || fail "build-flysim.sh must build flysim-session (and fly-session) and package-release.sh ship them"

rt_dir="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-runtime.XXXXXX")"
mkdir -p "$rt_dir/bin" "$rt_dir/release" "$rt_dir/systemd"
cat > "$rt_dir/bin/systemctl" <<'RTSTUB'
#!/usr/bin/env bash
echo "$*" >> "$RT_DIR/systemctl.log"
if [ "$1" = is-active ]; then
    case "$3" in flyshadow.service|flyshadow-guard.timer) [ "${RT_SHADOW_ACTIVE:-0}" = 1 ]; exit $? ;; esac
fi
RTSTUB
cat > "$rt_dir/release/fly-shadow-run" <<'RTSTUB'
#!/usr/bin/env bash
echo "$*" >> "$RT_DIR/shadow-run.log"
RTSTUB
cat > "$rt_dir/bin/id" <<'RTSTUB'
#!/usr/bin/env bash
echo 0
RTSTUB
cat > "$rt_dir/bin/logger" <<'RTSTUB'
#!/usr/bin/env bash
true
RTSTUB
cat > "$rt_dir/bin/curl" <<'RTSTUB'
#!/usr/bin/env bash
# /healthz answers per RT_HEALTHY; /status reports a frame that advances per call.
url="${*: -1}"
[ "${RT_HEALTHY:-1}" = 1 ] || exit 22
case "$url" in
    */status) n=$(( $(cat "$RT_DIR/frame" 2>/dev/null || echo 100) + 1 )); echo "$n" > "$RT_DIR/frame"
              echo "{\"status\":\"running\",\"frame\":$n}" ;;
esac
RTSTUB
for b in flysim flysim-session; do
    printf '#!/usr/bin/env bash\necho "${RT_COMPAT_%s:-same}"\n' "$(echo "$b" | tr 'a-z-' 'A-Z_')" > "$rt_dir/release/$b"
done
chmod +x "$rt_dir"/bin/* "$rt_dir"/release/*
echo 'FLY_GAME=pokemon-red' > "$rt_dir/fly.env"
fly_runtime() {
    env RT_DIR="$rt_dir" PATH="$rt_dir/bin:$PATH" FLY_RELEASE_DIR="$rt_dir/release" \
        FLY_ENV_FILE="$rt_dir/fly.env" FLY_SYSTEMD_DIR="$rt_dir/systemd" \
        FLY_RUNTIME_LOG="$rt_dir/runtime.log" FLY_RUNTIME_HEALTH_TIMEOUT=4 \
        FLY_RUNTIME_FELLBACK="$rt_dir/run/runtime-fellback.json" FLY_RUNTIME_LOCK="$rt_dir/run/runtime.lock" \
        FLY_SHADOW_RUN_BIN="$rt_dir/release/fly-shadow-run" \
        FLY_PROBATION_DIR="$rt_dir/probation" FLY_PROBATION_RESULT="$rt_dir/run/runtime-probation.json" \
        "$@" bash "$INFRA_DIR/bin/fly-runtime" "${RT_ARGS[@]}" >/dev/null 2>&1
}
rt_dropin="$rt_dir/systemd/flysim.service.d/10-runtime.conf"
RT_ARGS=(status)
[ "$(env RT_DIR="$rt_dir" PATH="$rt_dir/bin:$PATH" FLY_SYSTEMD_DIR="$rt_dir/systemd" bash "$INFRA_DIR/bin/fly-runtime" status 2>/dev/null | head -n1)" = legacy ] \
    && pass "fly-runtime status: legacy with no drop-in" \
    || fail "fly-runtime status must say legacy with no drop-in"
RT_ARGS=(session)
if fly_runtime && [ -f "$rt_dropin" ] \
    && grep -qx "ExecStart=$rt_dir/release/flysim-session" "$rt_dropin" \
    && grep -qx 'ExecStart=' "$rt_dropin" \
    && grep -qx 'Environment=FLY_SESSION_DIR=/run/fly/session' "$rt_dropin" \
    && grep -qx 'restart --no-block flysim.service' "$rt_dir/systemctl.log" \
    && [ "$(env RT_DIR="$rt_dir" PATH="$rt_dir/bin:$PATH" FLY_SYSTEMD_DIR="$rt_dir/systemd" bash "$INFRA_DIR/bin/fly-runtime" status 2>/dev/null | head -n1)" = session ]; then
    pass "fly-runtime session: the drop-in names flysim-session and /run/fly/session, flysim.service restarted, healthy"
else
    fail "fly-runtime session: drop-in/restart/health wrong ($(cat "$rt_dropin" 2>/dev/null | tr '\n' ' '))"
fi
# N1: a running shadow and guard are stopped by `session`, without restarting flysim; none running: untouched.
[ ! -e "$rt_dir/shadow-run.log" ] \
    && pass "fly-runtime session leaves fly-shadow-run alone when no shadow is running" \
    || fail "fly-runtime session called fly-shadow-run with no shadow running"
RT_ARGS=(legacy); fly_runtime || true
RT_ARGS=(session)
rm -f "$rt_dir/shadow-run.log"
if fly_runtime RT_SHADOW_ACTIVE=1 && [ "$(cat "$rt_dir/shadow-run.log" 2>/dev/null)" = stop ]; then
    pass "fly-runtime session stops the running shadow and guard (fly-shadow-run stop, no --restart-flysim)"
else
    fail "fly-runtime session must run exactly 'fly-shadow-run stop' when a shadow is running ($(cat "$rt_dir/shadow-run.log" 2>/dev/null))"
fi
rm -f "$rt_dir/shadow-run.log"
RT_ARGS=(session --no-restart)
fly_runtime RT_SHADOW_ACTIVE=1 || true
[ ! -e "$rt_dir/shadow-run.log" ] \
    && pass "fly-runtime session --no-restart (deploy refresh) does not touch the shadow" \
    || fail "fly-runtime session --no-restart must not stop the shadow"
RT_ARGS=(legacy)
if fly_runtime && [ ! -f "$rt_dropin" ]; then
    pass "fly-runtime legacy: the drop-in is gone, flysim.service restarted"
else
    fail "fly-runtime legacy must remove the drop-in and succeed"
fi
# BUS-01: the bus transport. `session-bus` (= `session --bus`) adds FLY_SESSION_TRANSPORT=bus to
# the drop-in and needs the release's fly-session worker; a flag-less `session` (05-deploy's
# refresh) keeps the transport; `session --local` is the way back; status says which.
rt_status() { env RT_DIR="$rt_dir" PATH="$rt_dir/bin:$PATH" FLY_SYSTEMD_DIR="$rt_dir/systemd" bash "$INFRA_DIR/bin/fly-runtime" status 2>/dev/null; }
RT_ARGS=(session-bus)
if ! fly_runtime && [ ! -f "$rt_dropin" ]; then
    pass "fly-runtime session-bus refuses a release without the fly-session worker"
else
    fail "fly-runtime session-bus must refuse without \$FLY_RELEASE_DIR/fly-session and write nothing"
fi
printf '#!/usr/bin/env bash\nexit 0\n' > "$rt_dir/release/fly-session"
chmod +x "$rt_dir/release/fly-session"
if fly_runtime && grep -qx 'Environment=FLY_SESSION_TRANSPORT=bus' "$rt_dropin" \
    && grep -qx "ExecStart=$rt_dir/release/flysim-session" "$rt_dropin" \
    && grep -qx 'OnFailure=fly-runtime-fallback.service' "$rt_dropin" \
    && [ "$(rt_status | head -n1)" = session ] \
    && rt_status | grep -qx '  transport: bus' \
    && tail -n1 "$rt_dir/runtime.log" | grep -q 'session-bus (was legacy)'; then
    pass "fly-runtime session-bus: the drop-in adds FLY_SESSION_TRANSPORT=bus (fallback and probation unchanged); status: session, transport bus"
else
    fail "fly-runtime session-bus: drop-in/status/log wrong ($(tr '\n' ' ' < "$rt_dropin" 2>/dev/null); $(tail -n1 "$rt_dir/runtime.log"))"
fi
RT_ARGS=(session --no-restart)
if fly_runtime && grep -qx 'Environment=FLY_SESSION_TRANSPORT=bus' "$rt_dropin"; then
    pass "fly-runtime session --no-restart (05-deploy's refresh) keeps the bus transport"
else
    fail "fly-runtime session --no-restart must keep the drop-in's transport"
fi
RT_ARGS=(session --local)
if fly_runtime && [ -f "$rt_dropin" ] && ! grep -q 'FLY_SESSION_TRANSPORT' "$rt_dropin" \
    && rt_status | grep -qx '  transport: local' \
    && tail -n1 "$rt_dir/runtime.log" | grep -q 'session (was session-bus)'; then
    pass "fly-runtime session --local takes the session off the bus (status: transport local)"
else
    fail "fly-runtime session --local must drop FLY_SESSION_TRANSPORT from the drop-in"
fi
RT_ARGS=(legacy --bus)
if ! fly_runtime && [ -f "$rt_dropin" ]; then
    pass "fly-runtime legacy --bus is refused (usage)"
else
    fail "fly-runtime legacy --bus must be refused"
fi
rm -f "$rt_dir/release/fly-session"
RT_ARGS=(legacy); fly_runtime || fail "fly-runtime legacy after the bus tests failed"
RT_ARGS=(session)
if ! fly_runtime RT_COMPAT_FLYSIM_SESSION=other && [ ! -f "$rt_dropin" ]; then
    pass "fly-runtime session refuses when the two compatibility strings differ"
else
    fail "fly-runtime session must refuse differing compatibility strings and write nothing"
fi
if ! fly_runtime RT_HEALTHY=0 && [ ! -f "$rt_dropin" ] && grep -q 'automatic fallback' "$rt_dir/runtime.log"; then
    pass "fly-runtime session falls back to legacy by itself when the session runtime is not healthy"
else
    fail "fly-runtime session must fall back to legacy (drop-in removed, logged) when unhealthy"
fi
# --- the persistent fallback (C1), the signal traps, the deploy policy (C2) ---
FALLBACK_UNIT="$INFRA_DIR/units/fly-runtime-fallback.service"
rt_fellback="$rt_dir/run/runtime-fellback.json"
rt_ncalls() { grep -c "$1" "$rt_dir/systemctl.log" || true; }
RT_ARGS=(session)
fly_runtime || fail "fly-runtime session (healthy) failed before the fallback tests"
if grep -qx 'OnFailure=fly-runtime-fallback.service' "$rt_dropin" \
    && grep -qx 'StartLimitBurst=3' "$rt_dropin" \
    && grep -qx 'StartLimitIntervalSec=600' "$rt_dropin" \
    && grep -qx 'RestartMode=direct' "$rt_dropin" \
    && grep -qx 'Environment=MALLOC_ARENA_MAX=2' "$rt_dropin" \
    && [ -f "$FALLBACK_UNIT" ] \
    && grep -qE '^ExecStart=/opt/fly/bin/fly-runtime fallback$' "$FALLBACK_UNIT" \
    && grep -qx 'Type=oneshot' "$FALLBACK_UNIT" \
    && ! grep -q '^\[Install\]' "$FALLBACK_UNIT"; then
    pass "session drop-in: OnFailure= fallback unit, 3 starts in 600 s, RestartMode=direct (else OnFailure= fires on every crash), MALLOC_ARENA_MAX=2; the unit runs 'fly-runtime fallback'"
else
    fail "session drop-in/fallback unit wrong ($(tr '\n' ' ' < "$rt_dropin"))"
fi
# Drive it the way systemd would: 3 failed starts of flysim.service reach the burst, systemd
# then starts the OnFailure= unit, i.e. runs its ExecStart (path mapped to the script under test).
sim_onfailure() {
    local burst unit cmd
    burst="$(sed -n 's/^StartLimitBurst=//p' "$rt_dropin")"
    unit="$(sed -n 's/^OnFailure=//p' "$rt_dropin")"
    [ "$unit" = fly-runtime-fallback.service ] || return 1
    cmd="$(sed -n 's/^ExecStart=//p' "$FALLBACK_UNIT")"
    cmd="${cmd/\/opt\/fly\/bin\/fly-runtime/bash $INFRA_DIR/bin/fly-runtime}"
    local failures=0
    while [ "$failures" -lt "$burst" ]; do failures=$((failures + 1)); done
    [ "$failures" -eq 3 ] || return 1
    # shellcheck disable=SC2086
    env RT_DIR="$rt_dir" PATH="$rt_dir/bin:$PATH" FLY_RELEASE_DIR="$rt_dir/release" \
        FLY_ENV_FILE="$rt_dir/fly.env" FLY_SYSTEMD_DIR="$rt_dir/systemd" \
        FLY_RUNTIME_LOG="$rt_dir/runtime.log" FLY_RUNTIME_FELLBACK="$rt_fellback" \
        FLY_RUNTIME_LOCK="$rt_dir/run/runtime.lock" $cmd >/dev/null 2>&1
}
restarts_before="$(rt_ncalls 'restart --no-block flysim.service')"
if sim_onfailure && [ ! -f "$rt_dropin" ] \
    && [ "$(rt_ncalls 'restart --no-block flysim.service')" -eq $((restarts_before + 1)) ] \
    && grep -q '"from":"session","to":"legacy"' "$rt_fellback" \
    && grep -q '"reason":"flysim.service failed on the session runtime' "$rt_fellback" \
    && grep -q 'automatic fallback' "$rt_dir/runtime.log" \
    && [ "$(env RT_DIR="$rt_dir" PATH="$rt_dir/bin:$PATH" FLY_SYSTEMD_DIR="$rt_dir/systemd" bash "$INFRA_DIR/bin/fly-runtime" status 2>/dev/null | head -n1)" = legacy ]; then
    pass "session fails 3x: OnFailure fallback removes the drop-in, starts legacy, writes the reason file and the journal line"
else
    fail "OnFailure fallback: drop-in/restart/reason file wrong ($(cat "$rt_fellback" 2>/dev/null))"
fi
# Idempotent: a second run (a stale OnFailure, a race with the script's own fallback) does nothing.
restarts_before="$(rt_ncalls 'restart --no-block flysim.service')"
cp "$rt_fellback" "$rt_dir/fellback.first"
RT_ARGS=(fallback)
if fly_runtime && [ "$(rt_ncalls 'restart --no-block flysim.service')" -eq "$restarts_before" ] \
    && cmp -s "$rt_fellback" "$rt_dir/fellback.first"; then
    pass "fly-runtime fallback is idempotent once legacy is selected"
else
    fail "a second fly-runtime fallback must be a no-op"
fi
# Never neither: no executable legacy binary -> keep the session drop-in.
RT_ARGS=(session)
fly_runtime || fail "could not re-select the session runtime for the no-legacy test"
mv "$rt_dir/release/flysim" "$rt_dir/flysim.away"
RT_ARGS=(fallback)
if ! fly_runtime && [ -f "$rt_dropin" ]; then
    pass "fly-runtime fallback keeps the session drop-in when there is no legacy binary to fall back to"
else
    fail "fly-runtime fallback removed the drop-in with no legacy binary"
fi
mv "$rt_dir/flysim.away" "$rt_dir/release/flysim"
# A successful `session` clears the reason file.
RT_ARGS=(fallback)
fly_runtime || true
[ -f "$rt_fellback" ] || fail "test setup: the fallback left no reason file"
RT_ARGS=(session)
if fly_runtime && [ ! -f "$rt_fellback" ]; then
    pass "fly-runtime session clears the previous fallback reason"
else
    fail "fly-runtime session must remove ${rt_fellback##*/} on success"
fi
# A deploy keeps the drop-in (C2): 05-deploy never removes it, refreshes it in place without a
# restart, and documents the policy; the refresh keeps the drop-in and restarts nothing.
if grep -n '10-runtime.conf' "$INFRA_DIR/05-deploy.sh" | grep -Eq '\brm\b'; then
    fail "05-deploy.sh removes 10-runtime.conf; a deploy must keep the chosen runtime"
elif grep -qF 'Session stays, trust gates' "$INFRA_DIR/05-deploy.sh" \
    && grep -qF 'fly-runtime session --no-restart' "$INFRA_DIR/05-deploy.sh" \
    && grep -qF 'Session stays, trust gates' "$INFRA_DIR/docs/runbook.md"; then
    pass "05-deploy.sh keeps the drop-in, refreshes it without a restart, and states the policy (runbook too)"
else
    fail "05-deploy.sh / runbook.md must state 'Session stays, trust gates' and refresh via fly-runtime session --no-restart"
fi
restarts_before="$(rt_ncalls '^restart ')"
printf 'stale\n' > "$rt_dropin"
RT_ARGS=(session --no-restart)
if fly_runtime && [ -f "$rt_dropin" ] \
    && grep -qx 'OnFailure=fly-runtime-fallback.service' "$rt_dropin" \
    && [ "$(rt_ncalls '^restart ')" -eq "$restarts_before" ]; then
    pass "a deploy's refresh (session --no-restart) keeps the drop-in, updates it, restarts nothing"
else
    fail "fly-runtime session --no-restart must rewrite the drop-in without restarting"
fi
# Signals: an interrupted switch rolls back to the previous state.
rt_interrupt() {  # $1 = signal: run `session` on an unhealthy runtime, signal it mid-wait, print its status
    local sig="$1" pid rc=0 n=0 base
    base="$(rt_ncalls '^restart --no-block flysim.service')"
    env RT_DIR="$rt_dir" PATH="$rt_dir/bin:$PATH" FLY_RELEASE_DIR="$rt_dir/release" \
        FLY_ENV_FILE="$rt_dir/fly.env" FLY_SYSTEMD_DIR="$rt_dir/systemd" \
        FLY_RUNTIME_LOG="$rt_dir/runtime.log" FLY_RUNTIME_HEALTH_TIMEOUT=60 RT_HEALTHY=0 \
        FLY_RUNTIME_FELLBACK="$rt_fellback" FLY_RUNTIME_LOCK="$rt_dir/run/runtime.lock" \
        python3 -c 'import os, signal, sys
signal.signal(signal.SIGINT, signal.SIG_DFL)  # a background job of a non-interactive shell starts with INT ignored
os.execvp(sys.argv[1], sys.argv[1:])' bash "$INFRA_DIR/bin/fly-runtime" session >/dev/null 2>&1 &
    pid=$!
    while [ "$(rt_ncalls '^restart --no-block flysim.service')" -le "$base" ] && [ "$n" -lt 100 ]; do
        n=$((n + 1)); sleep 0.1
    done
    if [ -n "${2:-}" ]; then "$2" >/dev/null 2>&1 || true; fi
    kill "-$sig" "$pid" 2>/dev/null || true
    wait "$pid" || rc=$?
    echo "$rc"
}
rm -f "$rt_dropin"
for sig_case in TERM:143 INT:130 HUP:129; do
    sig="${sig_case%%:*}"; want_rc="${sig_case##*:}"
    restarts_before="$(rt_ncalls '^restart ')"
    got_rc="$(rt_interrupt "$sig")"
    if [ "$got_rc" = "$want_rc" ] && [ ! -f "$rt_dropin" ] \
        && [ "$(rt_ncalls '^restart ')" -gt $((restarts_before + 1)) ] \
        && grep -q 'rolled back' "$rt_dir/runtime.log"; then
        pass "fly-runtime session interrupted by SIG$sig: drop-in removed, flysim.service restarted on the previous (legacy) state"
    else
        fail "fly-runtime session interrupted by SIG$sig: rc=$got_rc (want $want_rc), drop-in present=$([ -f "$rt_dropin" ] && echo yes || echo no)"
    fi
done
mkdir -p "$(dirname "$rt_dropin")"
printf '# previous-session-marker\n[Service]\nExecStart=\nExecStart=/previous\n' > "$rt_dropin"
cp "$rt_dropin" "$rt_dir/dropin.before"
got_rc="$(rt_interrupt TERM)"
if [ "$got_rc" = 143 ] && cmp -s "$rt_dropin" "$rt_dir/dropin.before" \
    && ! compgen -G "$rt_dropin.prev.*" >/dev/null; then
    pass "fly-runtime session interrupted while already on session: the earlier drop-in is restored, no temp files left"
else
    fail "an interrupted re-run of session must restore the earlier drop-in (rc=$got_rc)"
fi
# M2, deterministic interleaving: a re-run of `session` over an older drop-in is waiting for
# health, an OnFailure= fallback completes, THEN the switch is interrupted. The rollback must
# not put the older drop-in back (it has no OnFailure=, and the stream would be stranded on the
# session runtime).
# shellcheck disable=SC2317  # called through rt_interrupt
rt_fallback_now() {
    env RT_DIR="$rt_dir" PATH="$rt_dir/bin:$PATH" FLY_RELEASE_DIR="$rt_dir/release" \
        FLY_SYSTEMD_DIR="$rt_dir/systemd" FLY_RUNTIME_LOG="$rt_dir/runtime.log" \
        FLY_RUNTIME_FELLBACK="$rt_fellback" FLY_RUNTIME_LOCK="$rt_dir/run/runtime.lock" \
        bash "$INFRA_DIR/bin/fly-runtime" fallback "lint interleaving"
}
mkdir -p "$(dirname "$rt_dropin")"
printf '# older-session-marker\n[Service]\nExecStart=\nExecStart=/previous\n' > "$rt_dropin"
got_rc="$(rt_interrupt TERM rt_fallback_now)"
if [ "$got_rc" = 143 ] && [ ! -f "$rt_dropin" ] && [ -f "$rt_fellback" ] \
    && grep -q 'a fallback completed meanwhile' "$rt_dir/runtime.log"; then
    pass "rollback after a completed fallback leaves legacy selected (does not restore the older drop-in)"
else
    fail "rollback must not restore an older drop-in after a completed fallback (rc=$got_rc, drop-in present=$([ -f "$rt_dropin" ] && echo yes || echo no))"
fi
# ... and the two share ONE lock: a held lock stops `session` from touching anything.
rm -f "$rt_dropin" "$rt_fellback"
(
    exec 8>>"$rt_dir/run/runtime.lock"
    flock 8
    RT_ARGS=(session)
    if ! fly_runtime FLY_RUNTIME_LOCK_WAIT=1 && [ ! -f "$rt_dropin" ]; then
        touch "$rt_dir/lock-respected"
    fi
)
if [ -f "$rt_dir/lock-respected" ]; then
    pass "fly-runtime session waits on the same lock as fallback and does not switch without it"
else
    fail "fly-runtime session must take the shared ${FLY_RUNTIME_LOCK:-runtime.lock} lock"
fi
rm -f "$rt_dropin"
mv "$rt_dir/release/flysim-session" "$rt_dir/flysim-session.away"
if ! fly_runtime && [ ! -f "$rt_dropin" ]; then
    pass "fly-runtime session refuses a release without flysim-session"
else
    fail "fly-runtime session must refuse a release without flysim-session"
fi
mv "$rt_dir/flysim-session.away" "$rt_dir/release/flysim-session"

# CUT-01's speed probation (fly-runtime-probation), driven end to end with the real fly-runtime
# against the stubs above: `session` starts it, `tick --sample-json` feeds one recorded sample a
# minute (the sample times are synthetic, so 30 "minutes" take a second), and the outcome is read
# from the drop-in, the fallback reason file, the result file and the stubbed systemctl.
echo "--- the speed probation ---"
PROBATION="$INFRA_DIR/bin/fly-runtime-probation"
pr_state="$rt_dir/probation/state.json"
pr_result="$rt_dir/run/runtime-probation.json"
pr_fellback="$rt_dir/run/runtime-fellback.json"
pr_env() {
    env RT_DIR="$rt_dir" PATH="$rt_dir/bin:$PATH" FLY_RELEASE_DIR="$rt_dir/release" \
        FLY_ENV_FILE="$rt_dir/fly.env" FLY_SYSTEMD_DIR="$rt_dir/systemd" \
        FLY_RUNTIME_LOG="$rt_dir/runtime.log" FLY_RUNTIME_HEALTH_TIMEOUT=4 \
        FLY_RUNTIME_FELLBACK="$pr_fellback" FLY_RUNTIME_LOCK="$rt_dir/run/runtime.lock" \
        FLY_PROBATION_DIR="$rt_dir/probation" FLY_PROBATION_RESULT="$pr_result" "$@"
}
pr_reset() {
    rm -rf "$rt_dir/probation" "$pr_result" "$pr_fellback" "$rt_dir/systemd/flysim.service.d"
    : > "$rt_dir/systemctl.log"
}
pr_traces() { # case, n, t0 -> JSON samples, one a minute: {t,status,rtMean,lag,uptime}
    python3 - "$1" "$2" "$3" <<'PRPY'
import json, sys
case, n, t0 = sys.argv[1], int(sys.argv[2]), float(sys.argv[3])
lag, up = 2.0, 3600.0
for i in range(1, n + 1):
    rtf, status = 1.0 + (0.008 if i % 2 else -0.008), "running"
    if case == "slow":                       # a steady 0.85x
        rtf = 0.85 + (0.01 if i % 2 else -0.01); lag += 0.15 * 60
    elif case == "stall" and i in (12, 13):  # two stalled minutes (a GC, a disk hiccup): 18 s of lag
        rtf = 0.1; lag += 18 if i == 12 else 0
    elif case == "lag":                      # at real time on average, but 5 s more behind each minute
        lag += 5
    elif case == "warmup":                   # a slow restore: the first 5 samples are before the warm-up
        up = 30.0 + 60 * i if i <= 5 else 3600.0 + 60 * i
        if i <= 5:
            rtf = 0.2
    elif case == "restart" and i == 10:      # flysim restarted (uptime goes backwards), then a warm-up again
        up, lag = 20.0, 2.0
    if case in ("slow", "stall", "lag", "healthy"):
        up = 3600.0 + 60 * i
    elif case == "restart":
        up = up + 60 if i != 10 else up
    print(json.dumps({"t": t0 + 60 * i, "status": status, "rtMean": rtf, "lag": lag, "uptime": up}))
PRPY
}
pr_ticks() { # case, n: feed n samples to tick; leaves the last exit code in pr_rc
    local t0 line
    t0="$(date +%s)"
    pr_rc=0
    while IFS= read -r line; do
        pr_env bash "$PROBATION" tick --sample-json "$line" >/dev/null 2>&1 || pr_rc=$?
        [ -f "$pr_state" ] || break
    done < <(pr_traces "$1" "$2" "$t0")
}
pr_session() { pr_reset; rm -f "$rt_dir/systemd/flysim.service.d/10-runtime.conf"; RT_ARGS=(session); fly_runtime; }
pr_case() { # name, case, samples, expect: pass|fallback-speed|fallback-lag|no-verdict
    pr_session || { fail "probation $1: fly-runtime session failed"; return; }
    if [ ! -f "$pr_state" ] || ! grep -qx 'enable --now fly-runtime-probation.timer' "$rt_dir/systemctl.log"; then
        fail "probation $1: fly-runtime session did not start the probation (state file, timer)"
        return
    fi
    pr_ticks "$2" "$3"
    case "$4" in
        pass)
            if [ -f "$pr_result" ] && grep -q '"passed": true' "$pr_result" && [ -f "$rt_dropin" ] \
                && [ ! -f "$pr_fellback" ] && [ ! -f "$pr_state" ] \
                && grep -qx 'disable --now fly-runtime-probation.timer' "$rt_dir/systemctl.log"; then
                pass "probation $1: passes, writes runtime-probation.json, stops its timer, stays on session"
            else
                fail "probation $1: expected a pass with the session drop-in kept (result: $(cat "$pr_result" 2>/dev/null))"
            fi ;;
        fallback-*)
            if [ ! -f "$rt_dropin" ] && [ ! -f "$pr_result" ] && [ ! -f "$pr_state" ] \
                && grep -q '"reason":"speed: ' "$pr_fellback" 2>/dev/null \
                && grep -q "${4#fallback-}" "$pr_fellback" \
                && grep -q 'legacy (automatic fallback: speed: ' "$rt_dir/runtime.log" \
                && grep -q '"passed": false' "$rt_dir/probation/last.json" \
                && grep -qx 'disable --now fly-runtime-probation.timer' "$rt_dir/systemctl.log"; then
                pass "probation $1: falls back to legacy with reason speed (reason file, journal line, timer off)"
            else
                fail "probation $1: expected a speed fallback ($(cat "$pr_fellback" 2>/dev/null), drop-in $([ -f "$rt_dropin" ] && echo present || echo gone))"
            fi ;;
        no-verdict)
            if [ -f "$rt_dropin" ] && [ -f "$pr_state" ] && [ ! -f "$pr_fellback" ] && [ ! -f "$pr_result" ]; then
                pass "probation $1: no verdict yet, still on probation, no fallback"
            else
                fail "probation $1: expected it to be running undecided"
            fi ;;
    esac
}
pr_case "healthy session at 1.0x" healthy 32 pass
pr_case "a session at 0.85x, sustained" slow 40 fallback-speed
pr_case "a transient stall (2 stalled minutes, 18 s of lag)" stall 32 pass
pr_case "lag growth at real time (5 s a minute)" lag 40 fallback-lag
pr_case "a slow restore before the warm-up ends is not judged" warmup 36 pass
pr_case "a flysim restart inside the window starts it again" restart 50 pass
pr_case "a healthy session before 30 minutes" healthy 20 no-verdict
# A probation that has not finished is still there, with its state on disk, after a "reboot": a
# new process reads state.json and goes on (the timer stays enabled; nothing is held in memory).
pr_session && pr_ticks healthy 12
if [ -f "$pr_state" ] && [ "$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["history"]))' "$pr_state")" -ge 8 ]; then
    pass "probation state is persisted on disk between ticks (a reboot resumes it)"
else
    fail "probation state was not persisted"
fi
# fly-runtime legacy cancels a probation in progress.
RT_ARGS=(legacy)
if fly_runtime && [ ! -f "$pr_state" ] && [ ! -f "$rt_dropin" ] \
    && grep -qx 'disable --now fly-runtime-probation.timer' "$rt_dir/systemctl.log"; then
    pass "fly-runtime legacy cancels the probation (state gone, timer disabled)"
else
    fail "fly-runtime legacy must cancel the probation"
fi
# A tick with the legacy runtime selected ends a leftover probation and judges nothing.
pr_session && rm -f "$rt_dropin"
pr_env bash "$PROBATION" tick --sample-json '{"t":1,"status":"running","rtMean":0.1,"lag":1,"uptime":9999}' >/dev/null 2>&1 || true
if [ ! -f "$pr_state" ] && [ ! -f "$pr_fellback" ]; then
    pass "probation tick with the legacy runtime selected cancels itself, no fallback"
else
    fail "probation tick must end when the legacy runtime is selected"
fi
# A switch without a restart (05-deploy's refresh) or with --no-probation starts none.
pr_reset
RT_ARGS=(session --no-restart); fly_runtime
if [ ! -f "$pr_state" ]; then pass "fly-runtime session --no-restart starts no probation"; else fail "session --no-restart must not start a probation"; fi
pr_reset
rm -f "$rt_dropin"
RT_ARGS=(session --no-probation); fly_runtime
if [ ! -f "$pr_state" ] && [ -f "$rt_dropin" ]; then pass "fly-runtime session --no-probation starts no probation"; else fail "session --no-probation must not start a probation"; fi
# No probation helper, no cutover: an unguarded switch falls back.
pr_reset
rm -f "$rt_dropin"
RT_ARGS=(session)
if ! fly_runtime FLY_PROBATION_BIN="$rt_dir/none" && [ ! -f "$rt_dropin" ] && grep -q 'probation could not be started' "$pr_fellback"; then
    pass "fly-runtime session falls back when the probation cannot be started"
else
    fail "fly-runtime session must not leave an unguarded session runtime"
fi
# The rule over recorded traces, the way fly-shadow-run simulate does it.
pr_sim="$rt_dir/sim.jsonl"
pr_traces slow 40 1790000000 > "$pr_sim"
pr_out="$(bash "$PROBATION" simulate "$pr_sim" 2>&1)" && pr_rc=0 || pr_rc=$?
if [ "$pr_rc" = 1 ]; then pass "probation simulate: ${pr_out%%:*}, a steady 0.85x: ${pr_out#*: }"; else fail "probation simulate: slow trace: rc $pr_rc: $pr_out"; fi
pr_traces healthy 35 1790000000 > "$pr_sim"
pr_out="$(bash "$PROBATION" simulate "$pr_sim" 2>&1)" && pr_rc=0 || pr_rc=$?
if [ "$pr_rc" = 0 ]; then pass "probation simulate: healthy trace passes"; else fail "probation simulate: healthy trace: rc $pr_rc: $pr_out"; fi
# The unit files.
for u in fly-runtime-probation.service fly-runtime-probation.timer; do
    [ -f "$INFRA_DIR/units/$u" ] || fail "units/$u is missing"
done
if grep -qx 'ExecStart=/opt/fly/bin/fly-runtime-probation tick' "$INFRA_DIR/units/fly-runtime-probation.service" \
    && grep -qx 'OnUnitActiveSec=60s' "$INFRA_DIR/units/fly-runtime-probation.timer" \
    && grep -qx 'OnBootSec=90s' "$INFRA_DIR/units/fly-runtime-probation.timer" \
    && grep -qx 'WantedBy=timers.target' "$INFRA_DIR/units/fly-runtime-probation.timer" \
    && ! grep -q 'fly-runtime-probation' "$INFRA_DIR/units/fly.target" "$INFRA_DIR/07-enable.sh"; then
    pass "fly-runtime-probation units: a 60 s timer, enabled only by fly-runtime-probation start (boot resumes it)"
else
    fail "fly-runtime-probation units are wrong"
fi
if grep -qE 'for name in .*\bfly-runtime-probation\b.*; do$' "$INFRA_DIR/05-deploy.sh"; then
    pass "05-deploy.sh installs fly-runtime-probation"
else
    fail "05-deploy.sh must converge bin/fly-runtime-probation to /opt/fly/bin"
fi
rm -rf "$rt_dir"

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
# 3c2. The sim's CPUs are the sim's alone (PERF-02 review N1): the drop-in plan, the units, the
# deploy wiring and bin/fly-cpu-confine. A sustained CPU-bound process on a pinned sweep worker's
# CPU collapses the session runtime to 0.14x real time, so every slice that is not flysim.slice
# must be kept off its CPUs, and the session must not pin where that is not so.
# ---------------------------------------------------------------------------
echo "--- the sim's cpus are exclusive (flysim.slice, fly-cpu-confine) ---"
plan_bin="$(mktemp "${TMPDIR:-/tmp}/fly-lint-plan.XXXXXX")"
cat > "$plan_bin" <<PLANEOF
#!/usr/bin/env bash
set -euo pipefail
. "$INFRA_DIR/lib/common.sh"
"\$@"
PLANEOF
chmod +x "$plan_bin"
check_plan() {
    local label="$1" cpuset="$2" rayon="$3" encoder="$4" plan sim other u cpus bad=""
    plan="$("$plan_bin" cpuset_dropin_plan "$cpuset" "$rayon" "$encoder" 2>&1)" || { fail "cpuset_dropin_plan: $label: died: $plan"; return 0; }
    sim="$(awk '$1 == "flysim.slice" {print $2}' <<< "$plan")"
    other="$(awk '$1 == "FLY_OTHER_CPUS=" {print $2}' <<< "$plan")"
    [ "$sim" = "$(awk '$1 == "FLY_SIM_CPUS=" {print $2}' <<< "$plan")" ] || bad="$bad SIM-list-differs-from-slice"
    for u in flysim.service flysim-session.service; do
        [ "$(awk -v u="$u" '$1 == u {print $2}' <<< "$plan")" = "$sim" ] || bad="$bad $u-not-on-sim-cpus"
    done
    # every other unit, and the confinement list, is disjoint from the sim's cpus
    while read -r u cpus; do
        case "$u" in flysim.slice|flysim.service|flysim-session.service|FLY_SIM_CPUS=) continue ;; esac
        if [ -n "$(comm -12 <(tr ',' '\n' <<< "$sim" | sort) <(tr ',' '\n' <<< "$cpus" | sort))" ]; then
            bad="$bad $u-overlaps-sim"
        fi
    done <<< "$plan"
    # sim + other is exactly CPUSET
    [ "$(tr ',' '\n' <<< "$sim,$other" | sort -n | tr '\n' ' ')" = "$(tr ',' '\n' <<< "$cpuset" | sort -n | tr '\n' ' ')" ] || bad="$bad sim+other-is-not-CPUSET"
    for u in xvfb flystage flystage-web pulse mediamtx flyedge flycast flyshadow; do
        grep -q "^${u}.service " <<< "$plan" || bad="$bad $u-missing"
    done
    if [ -z "$bad" ]; then
        pass "cpuset_dropin_plan: $label: sim=$sim, nothing else can reach it, sim+other = CPUSET"
    else
        fail "cpuset_dropin_plan: $label:$bad"
    fi
}
check_plan "release shape (sixteen cpus, four sim, four encoder)" "1,3,5,7,9,11,13,15,29,31,33,35,17,19,37,39" 4 4
check_plan "ten cpus, ENCODER_CORES=3" "1,3,5,7,9,11,13,15,17,19" 4 3
check_plan "eight cpus, default encoder" "1,3,5,7,9,11,13,15" 4 ""
check_plan "dev shape (RAYON_THREADS=2)" "0,2,4,6,8,10,12,14" 2 ""
# the generated drop-ins: a slice gets [Slice], a service [Service], both AllowedCPUs=
if [ "$("$plan_bin" cpuset_dropin_text flysim.slice 1,3,5,7 lint-env | grep -v '^#')" = "$(printf '[Slice]\nAllowedCPUs=1,3,5,7')" ] \
    && [ "$("$plan_bin" cpuset_dropin_text xvfb.service 9,11 lint-env | grep -v '^#')" = "$(printf '[Service]\nAllowedCPUs=9,11')" ]; then
    pass "cpuset_dropin_text: [Slice] for flysim.slice, [Service] for a unit, AllowedCPUs= the list"
else
    fail "cpuset_dropin_text: wrong drop-in text"
fi
rm -f "$plan_bin"

# units
for u in flysim flysim-session; do
    if grep -qE '^Slice=flysim\.slice$' "$INFRA_DIR/units/$u.service" \
        && grep -qE '^ExecStartPre=-\+/opt/fly/bin/fly-cpu-confine apply$' "$INFRA_DIR/units/$u.service"; then
        pass "$u.service runs in flysim.slice and applies the confinement before it starts"
    else
        fail "$u.service must have Slice=flysim.slice and ExecStartPre=-+/opt/fly/bin/fly-cpu-confine apply"
    fi
done
if [ -f "$INFRA_DIR/units/flysim.slice" ] && grep -q '^\[Slice\]' "$INFRA_DIR/units/flysim.slice"; then
    pass "units/flysim.slice exists (no dash in the name: a-b.slice would nest under a.slice)"
else
    fail "units/flysim.slice is missing"
fi
if grep -rE '^Slice=' "$INFRA_DIR/units" | grep -vE '/flysim(-session)?\.service:Slice=flysim\.slice$'; then
    fail "a unit other than flysim/flysim-session sets Slice= (flysim.slice is the sim's alone)"
else
    pass "only flysim.service and flysim-session.service set Slice="
fi
# systemd nests a-b.slice under a.slice, and a child's cpuset is bounded by its parent's: the sim's
# slice (found as /fly.slice/fly-sim.slice/... in the N1 proof when it was named with a dash) must be
# top-level, beside system.slice and user.slice
for sl in "$INFRA_DIR"/units/*.slice; do
    case "$(basename "$sl" .slice)" in
        *-*) fail "$(basename "$sl"): a dash in a slice name nests it under the prefix slice" ;;
        *) pass "$(basename "$sl"): top-level slice (no dash in the name)" ;;
    esac
done
for u in fly-recap fly-retention; do
    grep -qE '^CPUSchedulingPolicy=idle$' "$INFRA_DIR/units/$u.service" \
        && pass "$u.service runs at SCHED_IDLE" || fail "$u.service must set CPUSchedulingPolicy=idle (N1: batch work never competes)"
done

# deploy wiring
grep -qF 'cpuset_dropin_plan "$CPUSET"' "$INFRA_DIR/05-deploy.sh" \
    && grep -qF '/etc/fly/cpuset.env' "$INFRA_DIR/05-deploy.sh" \
    && grep -qF '"$INFRA_DIR"/units/*.slice' "$INFRA_DIR/05-deploy.sh" \
    && grep -qE 'for name in .*\bfly-cpu-confine\b.*; do$' "$INFRA_DIR/05-deploy.sh" \
    && pass "05-deploy.sh converges the plan, cpuset.env, the slice unit and fly-cpu-confine" \
    || fail "05-deploy.sh must use cpuset_dropin_plan, write /etc/fly/cpuset.env, push units/*.slice and install fly-cpu-confine"
grep -qE 'FLY_SESSION_PIN=0' "$INFRA_DIR/05-deploy.sh" && grep -qE 'PARTITION_ACTIVE=1' "$INFRA_DIR/05-deploy.sh" \
    && pass "05-deploy.sh turns session pinning off (FLY_SESSION_PIN=0) when no partition is in force" \
    || fail "05-deploy.sh must write FLY_SESSION_PIN=0 to fly.env unless the cpuset partition is in force"

# bin/fly-cpu-confine against a stub systemctl and a fake /proc
cc="$INFRA_DIR/bin/fly-cpu-confine"
cc_dir="$(mktemp -d "${TMPDIR:-/tmp}/fly-lint-confine.XXXXXX")"
mkdir -p "$cc_dir/bin" "$cc_dir/proc" "$cc_dir/cg/.lxc"
echo 0-15 > "$cc_dir/cg/.lxc/cpuset.cpus"
cat > "$cc_dir/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >> "$STUB_LOG"
if [ "$1" = show ]; then
    unit="${*: -1}"
    case "$*" in
        *ControlGroup*) var="CG_${unit%.service}"; var="${var//-/_}"; echo "${!var:-}" ;;
    esac
fi
exit 0
STUB
cat > "$cc_dir/bin/timeout" <<'STUB'
#!/usr/bin/env bash
shift; exec "$@"
STUB
chmod +x "$cc_dir/bin/systemctl" "$cc_dir/bin/timeout"
printf 'FLY_SIM_CPUS=1,3,5,7\nFLY_OTHER_CPUS=9,11,13-15\n' > "$cc_dir/cpuset.env"
cc_run() {
    PATH="$cc_dir/bin:$PATH" CPUSET_ENV="${CPUSET_ENV:-$cc_dir/cpuset.env}" PROC_ROOT="$cc_dir/proc" CGROUP_ROOT="${CGROUP_ROOT:-$cc_dir/cg}" STUB_LOG="$cc_dir/log" "$@"
}
: > "$cc_dir/log"
out="$(cc_run "$cc" apply 2>&1)" && rc=0 || rc=$?
if [ "$rc" -eq 0 ] && [ "$(grep -c 'set-property --runtime .* AllowedCPUs=9,11,13-15' "$cc_dir/log")" = 3 ] \
    && grep -q 'set-property --runtime system.slice' "$cc_dir/log" && grep -q 'set-property --runtime user.slice' "$cc_dir/log" \
    && grep -q 'set-property --runtime init.scope' "$cc_dir/log"; then
    pass "fly-cpu-confine apply confines system.slice, user.slice and init.scope to the non-sim cpus"
else
    fail "fly-cpu-confine apply (rc=$rc): $out / $(cat "$cc_dir/log")"
fi
[ "$(cat "$cc_dir/cg/.lxc/cpuset.cpus")" = "9,11,13-15" ] \
    && pass "fly-cpu-confine apply also confines /.lxc (where pct exec lands) to the non-sim cpus" \
    || fail "fly-cpu-confine apply must write the non-sim cpus into /.lxc/cpuset.cpus (got $(cat "$cc_dir/cg/.lxc/cpuset.cpus"))"
out="$(cc_run "$cc" status 2>&1)"
grep -q '^/.lxc cpuset.cpus=9,11,13-15$' <<< "$out" \
    && pass "fly-cpu-confine status shows /.lxc" \
    || fail "fly-cpu-confine status must show /.lxc: $out"
# unwritable /.lxc: warn, still exit 0, slices still confined; absent /.lxc: nothing created
: > "$cc_dir/log"
mkdir -p "$cc_dir/cg2/.lxc"
mkdir "$cc_dir/cg2/.lxc/cpuset.cpus"   # a directory: the write fails, like a read-only cgroup file
out="$(CGROUP_ROOT="$cc_dir/cg2" cc_run "$cc" apply 2>&1)" && rc=0 || rc=$?
if [ "$rc" -eq 0 ] && grep -q 'WARNING: cannot write' <<< "$out" && [ "$(grep -c set-property "$cc_dir/log")" = 3 ]; then
    pass "fly-cpu-confine apply warns, never fails, when /.lxc cannot be written (slices still confined)"
else
    fail "fly-cpu-confine apply with an unwritable /.lxc (rc=$rc): $out"
fi
mkdir -p "$cc_dir/cg3"
CGROUP_ROOT="$cc_dir/cg3" cc_run "$cc" apply >/dev/null 2>&1
[ -z "$(ls -A "$cc_dir/cg3")" ] \
    && pass "fly-cpu-confine apply does not create /.lxc (lxc owns it)" \
    || fail "fly-cpu-confine apply must not create cgroups under the root: $(ls -A "$cc_dir/cg3")"
: > "$cc_dir/log"
out="$(CG_flysim=/system.slice/flysim.service cc_run "$cc" apply 2>&1)" && rc=0 || rc=$?
if [ "$rc" -eq 3 ] && ! grep -q set-property "$cc_dir/log"; then
    pass "fly-cpu-confine apply refuses (exit 3, changes nothing) while flysim still runs outside flysim.slice"
else
    fail "fly-cpu-confine apply must refuse a sim outside flysim.slice (rc=$rc): $out"
fi
: > "$cc_dir/log"
out="$(CG_flysim=/flysim.slice/flysim.service cc_run "$cc" apply 2>&1)" && rc=0 || rc=$?
[ "$rc" -eq 0 ] && grep -q set-property "$cc_dir/log" \
    && pass "fly-cpu-confine apply proceeds once flysim runs in flysim.slice" \
    || fail "fly-cpu-confine apply should proceed for a sim in flysim.slice (rc=$rc): $out"
: > "$cc_dir/log"
out="$(CPUSET_ENV="$cc_dir/none" cc_run "$cc" apply 2>&1)" && rc=0 || rc=$?
if [ "$rc" -eq 0 ] && ! grep -q set-property "$cc_dir/log"; then
    pass "fly-cpu-confine apply is a no-op without /etc/fly/cpuset.env (no partition, no confinement)"
else
    fail "fly-cpu-confine apply without cpuset.env must change nothing (rc=$rc): $out $(cat "$cc_dir/log")"
fi
: > "$cc_dir/log"
echo 0-15 > "$cc_dir/cg/cpuset.cpus.effective"
CGROUP_ROOT="$cc_dir/cg" cc_run "$cc" release >/dev/null 2>&1
[ "$(grep -c 'set-property --runtime .* AllowedCPUs=0-15$' "$cc_dir/log")" = 3 ] \
    && [ "$(cat "$cc_dir/cg/.lxc/cpuset.cpus")" = "0-15" ] \
    && pass "fly-cpu-confine release sets all three and /.lxc back to the container's whole cpuset (an empty AllowedCPUs= leaves the old mask in place)" \
    || fail "fly-cpu-confine release: $(cat "$cc_dir/log") / lxc=$(cat "$cc_dir/cg/.lxc/cpuset.cpus")"
# check: a fake /proc with a sim process in the slice, a clean neighbour, a kernel thread, then a hog
mkproc() { # pid comm cgroup allowed
    mkdir -p "$cc_dir/proc/$1"
    printf 'Name:\t%s\nCpus_allowed_list:\t%s\n' "$2" "$4" > "$cc_dir/proc/$1/status"
    echo "$2" > "$cc_dir/proc/$1/comm"
    echo "0::$3" > "$cc_dir/proc/$1/cgroup"
    ln -sfn /bin/true "$cc_dir/proc/$1/exe"
}
mkproc 10 flysim /flysim.slice/flysim.service 1,3,5,7
mkproc 11 ffmpeg /system.slice/fly-recap.service 9,11,13-15
mkproc 12 kthreadd /init.scope 0-15
rm -f "$cc_dir/proc/12/exe"
if cc_run "$cc" check >/dev/null 2>&1; then
    pass "fly-cpu-confine check: clean when only flysim.slice can reach the sim's cpus (kernel threads ignored)"
else
    fail "fly-cpu-confine check flagged a clean /proc: $(cc_run "$cc" check 2>&1)"
fi
mkproc 13 hog /system.slice/hog.service 0-15
out="$(cc_run "$cc" check 2>&1)" && rc=0 || rc=$?
if [ "$rc" -eq 1 ] && grep -q 'process 13 (hog).*sim cpu 1,3,5,7' <<< "$out"; then
    pass "fly-cpu-confine check names a process that may run on a sim cpu"
else
    fail "fly-cpu-confine check must flag the hog (rc=$rc): $out"
fi
# the check's own process chain (a pct exec shell) is noted, not failed; a stranger in /.lxc still fails
rm -rf "$cc_dir/proc/13"
mkproc 20 pctexec /.lxc 0-15
mkproc 21 bash /.lxc 0-15
mkproc 22 fly-cpu-confine /.lxc 0-15
printf 'Name:\tx\nPPid:\t21\nCpus_allowed_list:\t0-15\n' > "$cc_dir/proc/22/status"
printf 'Name:\tx\nPPid:\t20\nCpus_allowed_list:\t0-15\n' > "$cc_dir/proc/21/status"
printf 'Name:\tx\nPPid:\t1\nCpus_allowed_list:\t0-15\n' > "$cc_dir/proc/20/status"
out="$(SELF_PID=22 cc_run "$cc" check 2>&1)" && rc=0 || rc=$?
if [ "$rc" -eq 0 ] && grep -q 'own process chain' <<< "$out" && ! grep -q '^process' <<< "$out"; then
    pass "fly-cpu-confine check exits 0 when the only process on a sim cpu is its own pct exec chain"
else
    fail "fly-cpu-confine check must not count its own process or ancestors (rc=$rc): $out"
fi
mkproc 23 tar /.lxc 0-15
out="$(SELF_PID=22 cc_run "$cc" check 2>&1)" && rc=0 || rc=$?
if [ "$rc" -eq 1 ] && grep -q '^process 23 (tar)' <<< "$out" && ! grep -q '^process 2[012] ' <<< "$out"; then
    pass "fly-cpu-confine check still flags a different process in /.lxc"
else
    fail "fly-cpu-confine check must flag a stranger in /.lxc (rc=$rc): $out"
fi
rm -rf "$cc_dir"

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

    # (8) Row 67: the Pewter Gym, one trainer lost on repeat. MOVE 2 six turns in
    # seven, a whiteout, the walk back and the guide's YES/NO -- six names,
    # every macro done, and one stray tile reward in the window, so neither the
    # sequence, dominance nor unrewarded rule fires. No battle is won: two probes
    # flag as unwon-battles; the same window with one wild win in it does not.
    lp_reset
    lp_cycle 25 "GO OBJECTIVE" "TALK" "NO" "NEXT" "NEXT" "NEXT" "MOVE 2" "NEXT" \
        "MOVE 2" "NEXT" "MOVE 3" "NEXT" | lp_outcomes "$lp_fixture/events.jsonl" "done"
    printf '{"id":999997,"wallMs":1758000999997,"brainMs":100000,"kind":"reward","label":"AREA 54: 24 UNIQUE LOCATIONS","value":0.05,"rewardKind":"explore"}\n' \
        >> "$lp_fixture/events.jsonl"
    lp_status "$lp_fixture/status.json" 2311
    lp_pass
    lp_pass
    lp_first="$(lp_metric fly_loop_suspected)"
    lp_pass
    if [ "$lp_first" = "0" ] && [ "$(lp_metric fly_loop_suspected)" = "1" ] \
       && [ "$(lp_metric fly_loop_rewards)" = "1" ] \
       && [ "$(lp_metric fly_loop_fights)" = "75" ] \
       && [ "$(lp_metric fly_loop_wins)" = "0" ] \
       && grep -q 'loop suspected (unwon-battles): 75 moves chosen in battle and no battle won' "$lp_fixture/journal.log"; then
        pass "check 10: battles lost on repeat, a stray tile reward in the window, no win over two probes flag as unwon-battles"
    else
        fail "check 10: the row-67 log gave first=${lp_first} then suspected=$(lp_metric fly_loop_suspected) rewards=$(lp_metric fly_loop_rewards) fights=$(lp_metric fly_loop_fights) wins=$(lp_metric fly_loop_wins), journal: $(cat "$lp_fixture/journal.log")"
    fi
    if lp_report="$(jq -e -r '[.reason, (.window.fights|tostring), (.window.wins|tostring), .action] | join(" ")' "$lp_fixture/run/loop.json" 2>/dev/null)" \
       && [ "$lp_report" = "unwon-battles 75 0 none" ]; then
        pass "check 10: loop.json carries the moves chosen and the battles won in the window"
    else
        fail "check 10: loop.json read back as '${lp_report:-UNREADABLE}' — expected 'unwon-battles 75 0 none'"
    fi
    lp_reset
    printf '{"id":999998,"wallMs":1758000999998,"brainMs":150000,"kind":"reward","label":"BEAT PEWTER GYM TRAINER 1","value":0.5,"rewardKind":"trainer"}\n' \
        >> "$lp_fixture/events.jsonl"
    lp_pass
    lp_pass
    lp_pass
    if [ "$(lp_metric fly_loop_suspected)" = "0" ] && [ "$(lp_metric fly_loop_wins)" = "1" ]; then
        pass "check 10: the same battles with one of them won do not flag"
    else
        fail "check 10: a won battle gave suspected=$(lp_metric fly_loop_suspected) wins=$(lp_metric fly_loop_wins)"
    fi

    # (9) Row 70: the report carries the window's lasting progress for the recovery ladder --
    # a milestone event, and rewards of kind pokedex/area/trainer/badge -- and the species
    # alone. The flag itself does not read them: the same battles still flag with a species
    # owned in the window, as long as nothing was won.
    lp_reset
    lp_cycle 25 "GO OBJECTIVE" "TALK" "NO" "NEXT" "NEXT" "NEXT" "MOVE 2" "NEXT" \
        "MOVE 2" "NEXT" "MOVE 3" "NEXT" | lp_outcomes "$lp_fixture/events.jsonl" "done"
    printf '%s\n' \
        '{"id":999991,"wallMs":1758000999991,"brainMs":100000,"kind":"reward","label":"OWNED #41","value":0.5,"rewardKind":"pokedex"}' \
        '{"id":999992,"wallMs":1758000999992,"brainMs":100001,"kind":"reward","label":"AREA 59","value":0.2,"rewardKind":"area"}' \
        '{"id":999993,"wallMs":1758000999993,"brainMs":100002,"kind":"milestone","label":"Reached MT. MOON","value":12}' \
        >> "$lp_fixture/events.jsonl"
    lp_status "$lp_fixture/status.json" 2311
    lp_pass
    lp_pass
    lp_pass
    if lp_report="$(jq -e -r '[.reason, (.window.rewards|tostring), (.window.progress.lasting|tostring), (.window.progress.species|tostring)] | join(" ")' "$lp_fixture/run/loop.json" 2>/dev/null)" \
       && [ "$lp_report" = "unwon-battles 2 3 1" ]; then
        pass "check 10: loop.json carries the window's lasting progress (a rung, a species, a map) for the ladder, and the flag does not read it"
    else
        fail "check 10: loop.json read back as '${lp_report:-UNREADABLE}' — expected 'unwon-battles 2 3 1'"
    fi
    # ... and names each once (review r3): a reset's replay pays the same rewards again, and only
    # the names let the ladder tell them from new ones.
    lp_expected='pokedex:OWNED #41|area:AREA 59|milestone:Reached MT. MOON'
    if lp_report="$(jq -e -r '.window.progress.keys | join("|")' "$lp_fixture/run/loop.json" 2>/dev/null)" \
       && [ "$lp_report" = "$lp_expected" ]; then
        pass "check 10: loop.json names the window's lasting progress (kind:label) so the ladder can tell a replayed reward from a new one"
    else
        fail "check 10: window.progress.keys read back as '${lp_report:-UNREADABLE}' — expected '${lp_expected}'"
    fi

    # The ethos, asserted rather than reviewed: over every case above, check 10
    # restarted nothing. It reports; a human or a review agent decides.
    if [ ! -s "$lp_fixture/systemctl.log" ]; then
        pass "check 10: never acts — no unit was restarted across any of the cases"
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
# The scan is tools/pii-scan.sh. It carries only GENERIC shapes (addresses,
# container ids, host paths, e-mail, credential shapes); the proper nouns are
# loaded at run time from the operator's private pattern list
# ($FLY_PII_PATTERNS), so the guard is not itself a list of what it refuses.
# Here a missing private list is a NOTE (public CI has none, by design);
# infra/build/tag-release.sh and the pre-push hook (tools/install-hooks.sh)
# fail closed instead.
#
# Legitimate mentions are listed in infra/tests/de-pii-allow.txt, one
# `path pattern-name  # reason` per line. Add to it only with a reason; the
# right fix is almost always to redact instead.
# ---------------------------------------------------------------------------
echo "--- de-PII guard ---"

REPO_ROOT="$(cd "$INFRA_DIR/.." && pwd)"
DEPII_ALLOW="$INFRA_DIR/tests/de-pii-allow.txt"

DEPII_OUT="$("$REPO_ROOT/tools/pii-scan.sh" --allow "$DEPII_ALLOW" --tree 2>&1)" && DEPII_RC=0 || DEPII_RC=$?
DEPII_HITS=0
while IFS= read -r line; do
    case "$line" in
        "pii-scan: NOTE:"*) echo "$line" ;;
        "pii-scan: clean"*|"pii-scan: "*" finding(s) "*|"") ;;
        "pii-scan: "*) fail "de-PII: ${line#pii-scan: }"; DEPII_HITS=$((DEPII_HITS + 1)) ;;
    esac
done <<<"$DEPII_OUT"

if [ "$DEPII_RC" -eq 0 ] && [ "$DEPII_HITS" -eq 0 ]; then
    pass "de-PII guard: $(printf '%s\n' "$DEPII_OUT" | grep -o 'clean.*' | tail -1)"
elif [ "$DEPII_HITS" -gt 0 ]; then
    echo "       ^ redact these (rules in the operator's infra repo), or, if the" >&2
    echo "         mention is genuinely legitimate, add it to infra/tests/de-pii-allow.txt with a reason." >&2
else
    fail "de-PII guard: tools/pii-scan.sh exited ${DEPII_RC}: ${DEPII_OUT}"
fi

if "$INFRA_DIR/tests/pii-scan-test.sh" >/dev/null 2>&1; then
    pass "pii-scan tests: tree, range, commit message, value needles, allowlist, fail-closed, pre-push hook"
else
    fail "pii-scan tests failed; run infra/tests/pii-scan-test.sh"
fi

echo "--- loop recovery tests ---"
if python3 -m unittest discover -s "$REPO_ROOT/infra/tests" -p 'test_loop_recover.py' >/dev/null 2>&1; then
    pass "loop recovery: ladder, budget, reboot-safe state, model delay and fallback, splash notice"
else
    fail "loop recovery tests failed; run python3 -m unittest discover -s infra/tests -p test_loop_recover.py -v"
fi

echo "--- fly.env is read, never sourced ---"
# fly.env is systemd EnvironmentFile syntax: GAME_TITLE=Pokemon Red is valid there and a shell
# error when sourced (the v0.7.3 cutover's check died on it). Scripts parse KEY=VALUE lines.
sourced="$(grep -n -E '^[[:space:]]*(\.|source)[[:space:]]+.*(fly\.env|FLY_ENV_FILE)' "$REPO_ROOT"/infra/bin/* 2>/dev/null || true)"
if [ -z "$sourced" ]; then
    pass "no infra/bin script sources fly.env"
else
    fail "fly.env sourced as shell (parse KEY=VALUE lines instead): $sourced"
fi

echo "==="
if [ "$FAILED" -eq 0 ]; then
    echo "lint.sh: ALL CHECKS PASSED"
else
    echo "lint.sh: FAILURES ABOVE"
fi
exit "$FAILED"
