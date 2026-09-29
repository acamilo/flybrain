#!/usr/bin/env bash
# infra/build/tag-release.sh vX.Y.Z
#
# Creates an annotated git tag for a flybrain release, after verifying
# `main` is clean and every test suite is green. Run on the operator box
# (or wherever the full toolchain — node, cargo, shellcheck — is
# available), not on the host: this script never touches the host, pct, or any
# container. It does not push the tag; that is a deliberate manual step.
#
# docs/stream-mvp-plan.md, "Release container" (the operator, 2026-09-16): "the
# release container runs TAGGED commits only" — tags on main, `vX.Y.Z`-
# style. This is the one place such a tag gets created, so the same
# ^v[0-9]+\.[0-9]+\.[0-9]+$ pattern infra/05-deploy.sh's ROLE=release gate
# checks (infra/lib/common.sh's require_release_tag) is enforced here too,
# before the tag exists at all. See docs/runbook.md's "Cutting a release"
# section for the rest of the flow (package, deploy to the release container, verify,
# rollback).
set -euo pipefail

log() { echo "tag-release: $*" >&2; }
die() { echo "tag-release: FATAL: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"; }

[ $# -eq 1 ] || die "usage: $0 vX.Y.Z"
TAG="$1"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "TAG must match ^v[0-9]+\.[0-9]+\.[0-9]+\$, got: $TAG"

for c in git npm cargo; do need "$c"; done

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_DIR"

branch="$(git rev-parse --abbrev-ref HEAD)"
[ "$branch" = main ] || die "refusing to tag: currently on branch '$branch', not 'main' — checkout main first"

[ -z "$(git status --porcelain)" ] || die "refusing to tag: working tree is not clean (git status --porcelain is non-empty)"

if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
    die "refusing to tag: ${TAG} already exists (git tag -l ${TAG})"
fi
git fetch --tags >/dev/null 2>&1 \
    && log "fetched tags from the remote" \
    || log "WARNING: git fetch --tags failed (no remote configured, or offline) — checking only against local tag state"
if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
    die "refusing to tag: ${TAG} already exists on the remote (git tag -l ${TAG})"
fi

log "running npm test"
npm test

log "running npm run typecheck"
npm run typecheck

flysim_dir="${REPO_DIR}/services/flysim"
if [ -d "$flysim_dir" ]; then
    # --release, not debug (Makefile's `rust:` target and the reason it gives): the flysim
    # integration test that asserts the feed's 30 Hz contract only reaches ~9.5-10.4 Hz in a debug
    # build, so a debug run would fail or flake that gate.
    # The ROM suites (tests/rom_*.rs) return early and count as PASSED when FLY_ROM or their
    # checkpoint variable is unset, so a gate run without them proves nothing about the macros.
    # v0.6.4's gate ran that way. Refuse without the ROM; name every checkpoint suite skipped.
    if [ -z "${FLY_ROM:-}" ] || [ ! -r "${FLY_ROM}" ]; then
        die "FLY_ROM is unset or unreadable: the ROM test suites would pass without running. Export FLY_ROM and the FLY_*_CHECKPOINT variables (the operator's rom env) first."
    fi
    skipped=""
    while IFS= read -r var; do
        [ -n "${!var:-}" ] || skipped="${skipped} ${var}"
    done < <(grep -rhoE 'var_os\("FLY_[A-Z0-9_]*_CHECKPOINT"\)' "$flysim_dir"/crates/*/tests/rom_*.rs \
                 | sed -E 's/.*"(FLY_[A-Z0-9_]*)".*/\1/' | sort -u)
    [ -z "$skipped" ] || log "WARNING: ROM checkpoint suites that will pass WITHOUT running (unset):${skipped}"
    log "running cargo test --workspace --release in services/flysim (FLY_ROM set)"
    (cd "$flysim_dir" && cargo test --workspace --release)
else
    log "WARNING: services/flysim does not exist in this checkout — skipping cargo test --workspace"
fi

log "running infra/tests/lint.sh"
"${REPO_DIR}/infra/tests/lint.sh"

commit="$(git rev-parse HEAD)"
log "all suites green at $(git rev-parse --short HEAD) on main — creating annotated tag ${TAG}"
git tag -a "$TAG" -m "flybrain release ${TAG}"

log "created annotated tag ${TAG} -> ${commit}. NOT pushed."
log "next: git push origin ${TAG} (when ready), then infra/build/build-flysim.sh, infra/build/package-release.sh ${TAG} <flysim-bin> <stage-dir> <bridge-dir> <out-dir>, infra/05-deploy.sh <release-env> <tarball> — see docs/runbook.md 'Cutting a release'"
