#!/usr/bin/env bash
# infra/build/package-release.sh VERSION FLYSIM_BIN STAGE_DIR BRIDGE_DIR OUT_DIR
#
# Assembles a release tarball from the three built artifacts (flysim
# binary, apps/stage's built static page, services/bridge's node app +
# node_modules) plus a MANIFEST of every file's sha256. Run on the
# operator box or the fly-build CT, wherever the three artifacts already
# exist — this script does not build anything itself: see build-flysim.sh
# for the Rust build, build-bridge.sh for BRIDGE_DIR, and `npm run build -w
# @flybrain/stage` for STAGE_DIR.
#
# Output: OUT_DIR/flybrain-<version>.tar.gz, laid out as
#   flysim               (the binary, mode 0755)
#   fly-edge              (the feed-bus edge, mode 0755, when build-flysim.sh
#                          left one beside FLYSIM_BIN; flyedge.service stays
#                          inactive on a release without it)
#   stage/...             (apps/stage's build output)
#   bridge/...             (services/bridge + node_modules)
#   data/fafb-v783/...     (the connectome, from the repo; FLY_DATASET points here)
#   MANIFEST               (a few '#'-prefixed provenance comments, git_tag=
#                           and git_commit= among them, then sha256
#                           relative/path checksum lines, one per file,
#                           sorted)
#
# The '#' comment lines are deliberate: GNU coreutils' `sha256sum -c` skips
# blank and '#'-prefixed lines rather than treating them as malformed, so
# 05-deploy.sh's post-extraction `sha256sum -c MANIFEST --quiet` verifies
# cleanly with the provenance header still in the file (checked by hand: a
# MANIFEST with such a header produces no warning and exits 0).
#
# git_tag is the exact annotated tag (`git describe --exact-match --tags`)
# when this script is run from a tree checked out at one — the normal case
# for a ROLE=release build per infra/build/tag-release.sh and
# docs/stream-mvp-plan.md's "release container runs TAGGED commits only" —
# and otherwise the best `git describe --tags --always` can do (a dev
# build's short sha, typically), so a release deploy's tag-match check in
# 05-deploy.sh always has something to compare VERSION against.
#
# 05-deploy.sh pushes this tarball into the container, extracts it under
# /opt/fly/releases/<version>/, and re-verifies MANIFEST after extraction
# before flipping the `current` symlink.
set -euo pipefail

usage() { echo "usage: $0 VERSION FLYSIM_BIN STAGE_DIR BRIDGE_DIR OUT_DIR" >&2; exit 2; }
[ $# -eq 5 ] || usage

VERSION="$1"
FLYSIM_BIN="$2"
STAGE_DIR="$3"
BRIDGE_DIR="$4"
OUT_DIR="$5"

log() { echo "package-release: $*" >&2; }
die() { echo "package-release: FATAL: $*" >&2; exit 1; }

[ -x "$FLYSIM_BIN" ] || die "flysim binary not found or not executable: $FLYSIM_BIN"
[ -d "$STAGE_DIR" ] || die "stage build dir not found: $STAGE_DIR"
[ -d "$BRIDGE_DIR" ] || die "bridge dir not found: $BRIDGE_DIR"
# infra/units/flybridge.service's ExecStart and its ConditionPathExists are both
# on bridge/index.js. A BRIDGE_DIR without one packages a release whose chat
# bridge can never start — and the condition makes that failure SILENT: the unit
# stays cleanly inactive, verify.sh SKIPs it as "expected before P3", and the
# only symptom is a live channel with no bot in it. That is exactly what v0.1.0
# shipped on 2026-09-16. Build the directory with infra/build/build-bridge.sh,
# which produces this shape by construction.
[ -f "${BRIDGE_DIR}/index.js" ] || die "no index.js in ${BRIDGE_DIR} — flybridge.service execs bridge/index.js and will refuse to start without it (ConditionPathExists), silently. Build it with infra/build/build-bridge.sh"
[[ "$VERSION" =~ ^[A-Za-z0-9._-]+$ ]] || die "VERSION must match [A-Za-z0-9._-]+, got: $VERSION"

mkdir -p "$OUT_DIR"
work="$(mktemp -d "${TMPDIR:-/tmp}/package-release.XXXXXX")"
trap 'rm -rf "$work"' EXIT

release_dir="${work}/release"
mkdir -p "$release_dir"

cp "$FLYSIM_BIN" "${release_dir}/flysim"
chmod 0755 "${release_dir}/flysim"
EDGE_BIN="$(dirname "$FLYSIM_BIN")/fly-edge"
if [ -x "$EDGE_BIN" ]; then
    cp "$EDGE_BIN" "${release_dir}/fly-edge"
    chmod 0755 "${release_dir}/fly-edge"
else
    log "no fly-edge beside $FLYSIM_BIN; packaging without it (FLY_FEED_VIA=bus unavailable in this release)"
fi
cp -a "$STAGE_DIR" "${release_dir}/stage"
cp -a "$BRIDGE_DIR" "${release_dir}/bridge"

# The connectome travels with the release. flysim cannot start without it and
# nothing else in infra/ ever put it on the container, so before the P0 spike
# every provisioned container had a flysim that exited looking for
# /srv/fly/data/fafb-v783. Shipping it here (rather than as a separate
# provisioning step) also means a rollback to an older release gets that
# release's dataset, and the MANIFEST below covers its every file.
DATASET_SRC="${DATASET_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/data/fafb-v783}"
[ -d "$DATASET_SRC" ] || die "dataset dir not found: $DATASET_SRC (override with DATASET_SRC=)"
[ -f "${DATASET_SRC}/meta.json" ] || die "no meta.json in $DATASET_SRC — not a dataset directory"
mkdir -p "${release_dir}/data"
cp -a "$DATASET_SRC" "${release_dir}/data/"
log "staged dataset $(basename "$DATASET_SRC") ($(du -sh "$DATASET_SRC" | cut -f1))"

# Provenance: which commit/tag this release was built from. REPO_DIR is
# the flybrain checkout this script itself lives in (infra/build/../..),
# not necessarily the same tree 05-deploy.sh runs from — recorded here so
# a MANIFEST always says where its bits came from even when it does not.
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
GIT_TAG=unknown
GIT_COMMIT=unknown
if git -C "$REPO_DIR" rev-parse --git-dir >/dev/null 2>&1; then
    GIT_COMMIT="$(git -C "$REPO_DIR" rev-parse HEAD)"
    GIT_TAG="$(git -C "$REPO_DIR" describe --exact-match --tags 2>/dev/null || true)"
    if [ -z "$GIT_TAG" ]; then
        GIT_TAG="$(git -C "$REPO_DIR" describe --tags --always 2>/dev/null || echo unknown)"
    fi
else
    log "WARNING: $REPO_DIR is not a git checkout — MANIFEST will record git_tag=unknown git_commit=unknown"
fi
log "provenance: git_tag=${GIT_TAG} git_commit=${GIT_COMMIT}"

log "computing MANIFEST"
{
    echo "# flybrain release MANIFEST"
    echo "# version=${VERSION}"
    echo "# git_tag=${GIT_TAG}"
    echo "# git_commit=${GIT_COMMIT}"
    (
        cd "$release_dir"
        find . -type f ! -name MANIFEST -print0 \
            | sort -z \
            | xargs -0 sha256sum \
            | sed 's#\./##'
    )
} > "${release_dir}/MANIFEST"

tarball="${OUT_DIR}/flybrain-${VERSION}.tar.gz"
log "writing ${tarball}"
tar -C "$release_dir" -czf "$tarball" .

log "sha256 of the tarball itself: $(sha256sum "$tarball" | awk '{print $1}')"
log "done: ${tarball}"
