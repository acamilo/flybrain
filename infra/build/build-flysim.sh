#!/usr/bin/env bash
# infra/build/build-flysim.sh [OUT_PATH]
#
# Run INSIDE the throwaway build container (the throwaway build container `fly-build`, Debian 13,
# 16 cores, onboot=0 — docs/design/infra.md section 2), never on the WSL
# box and never on the host directly. Building against a newer glibc than
# Debian 13 trixie's and running on trixie fails at load; the build box
# being the same template on the same host removes that class of problem
# entirely.
#
# The sharp edge this script exists to get right every time:
#   RUSTFLAGS="-C target-cpu=haswell"   correct — the host's E5-2660 v3 is Haswell
#   RUSTFLAGS="-C target-cpu=native"    WRONG off-host — a different build
#                                       machine's native ISA (e.g. Zen3) can
#                                       emit instructions Haswell does not
#                                       have; illegal instruction at run time.
#
# Also rejects musl static linking (x86_64-unknown-linux-musl): musl's
# allocator is materially slower under the per-millisecond neuron sweep's
# allocation pattern, and real-time factor is the whole ballgame. Ships a
# dynamically linked glibc binary built on the matching template instead.
#
# FLY_CARGO_FEATURES — opt-in cargo features for the flysim bin, comma or
# space separated. DEFAULT EMPTY, which is the plain CPU-only release build
# every tagged release so far was made with. The only feature today is
# `cuda`, the bit-exact LIF tick on the GPU:
#
#   FLY_CARGO_FEATURES=cuda infra/build/build-flysim.sh ./flysim
#
# Three things make that safe to compile into a release binary, all measured
# (infra/docs/lif-cuda-spike.md, infra/docs/cuda-on-dev.md):
#
#   * it needs no CUDA toolkit to build — the PTX is committed and embedded
#     with include_str!, and the driver JITs it at module load;
#   * `cudarc` is built with `dynamic-loading`, so `libcuda` is dlopen'd on
#     first use. `ldd` on the binary lists no CUDA library, and a run
#     without FLY_LIF_CUDA=1 never opens the driver — the same binary is
#     therefore deployable to a GPU-less container;
#   * the GPU tick is bit-exact with the CPU one, so `--print-compatibility`
#     is byte-identical to a CPU-only build of the same tree and a live
#     checkpoint restores onto either.
#
# Compiling the feature in does NOT turn it on. The backend attaches only
# when `FLY_LIF_CUDA=1` reaches the process, which on a container means
# `FLY_LIF_CUDA=1` in the env file so 05-deploy.sh writes it into
# /etc/fly/fly.env (off by default there too).
set -euo pipefail

log() { echo "build-flysim: $*" >&2; }
die() { echo "build-flysim: FATAL: $*" >&2; exit 1; }

: "${CARGO_TARGET:=x86_64-unknown-linux-gnu}"
: "${FLY_CARGO_FEATURES:=}"
OUT_PATH="${1:-./flysim}"

command -v cargo >/dev/null 2>&1 || die "cargo not found — this must run inside fly-build (the build container), which has the Rust toolchain, not on the host or the WSL box"

if [ "$CARGO_TARGET" != "x86_64-unknown-linux-gnu" ]; then
    die "refusing target '$CARGO_TARGET': must be x86_64-unknown-linux-gnu (glibc), never a *-musl target"
fi

crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../services/flysim" 2>/dev/null && pwd || true)"
if [ -z "$crate_dir" ]; then
    die "services/flysim does not exist yet in this checkout (it is built by workstream W1, docs/stream-mvp-plan.md) — nothing to build"
fi

# Empty FLY_CARGO_FEATURES must produce the plain command, not `--features ''`
# (cargo accepts that, but it would put an empty-string feature in the build
# plan's fingerprint and make a default build look different from every
# release built before this knob existed). An array, so the flag and its
# value survive as two argv entries whatever the feature list contains.
features_args=()
if [ -n "$FLY_CARGO_FEATURES" ]; then
    features_args=(--features "$FLY_CARGO_FEATURES")
    log "opt-in cargo features: $FLY_CARGO_FEATURES (default is none — see the FLY_CARGO_FEATURES note in this script's header)"
fi

log "building in $crate_dir for target-cpu=haswell (the host is E5-2660 v3, Haswell, AVX2, no AVX-512)"
(
    cd "$crate_dir"
    RUSTFLAGS="-C target-cpu=haswell" cargo build --release --target "$CARGO_TARGET" --bin flysim "${features_args[@]}"
    # fly-edge (FLY_FEED_VIA=bus, docs/design/flybus.md): the feed WebSocket
    # served from flysim's feed bus. Small, and no cargo features of its own;
    # built every time so a release can switch a container onto the bus
    # without a rebuild. It lands next to OUT_PATH, where package-release.sh
    # looks for it.
    RUSTFLAGS="-C target-cpu=haswell" cargo build --release --target "$CARGO_TARGET" --bin fly-edge
)

built="${crate_dir}/target/${CARGO_TARGET}/release/flysim"
[ -x "$built" ] || die "expected binary not found after build: $built"

# Sanity: confirm it is dynamically linked glibc, not a static/musl binary.
if command -v file >/dev/null 2>&1; then
    file "$built" | grep -q 'dynamically linked' || die "$built does not look dynamically linked — check for an accidental musl/static target"
fi

# Sanity: no CUDA library may appear in the link. The `cuda` feature is only
# deployable because `cudarc`'s `dynamic-loading` dlopen's libcuda on first
# use — a DT_NEEDED on libcuda.so.1 instead would make the binary refuse to
# start on any container without the userspace driver, including the release container
# before gate 2 of the v0.2 plan and the fly-build container itself. Checked
# unconditionally, because the way this breaks is someone turning
# `dynamic-loading` off in Cargo.toml, not someone setting the env var.
if command -v ldd >/dev/null 2>&1; then
    if ldd "$built" 2>/dev/null | grep -qi 'libcuda\|libnvrtc\|libcudart'; then
        die "$built links a CUDA library dynamically — cudarc must keep its 'dynamic-loading' feature so libcuda is dlopen'd, or this binary cannot start on a GPU-less container:
$(ldd "$built" | grep -i 'libcuda\|libnvrtc\|libcudart')"
    fi
fi

cp "$built" "$OUT_PATH"
chmod 0755 "$OUT_PATH"
log "built $OUT_PATH ($(du -h "$OUT_PATH" | cut -f1))"
edge_built="${crate_dir}/target/${CARGO_TARGET}/release/fly-edge"
[ -x "$edge_built" ] || die "expected binary not found after build: $edge_built"
edge_out="$(dirname "$OUT_PATH")/fly-edge"
cp "$edge_built" "$edge_out"
chmod 0755 "$edge_out"
log "built $edge_out ($(du -h "$edge_out" | cut -f1))"
log "next: infra/build/package-release.sh VERSION $OUT_PATH <stage-dir> <bridge-dir> <out-dir>"
