#!/usr/bin/env bash
# Put the scorecard on an older release's tree, so a release that predates the tool can be scored.
#
#   tools/scorecard/overlay.sh <scorecard-ref> <target-tree>
#
# Copies the fly-scorecard crate, the checkpoint set and the docs from <scorecard-ref> of this
# repository into <target-tree> (a checkout of the release to score), adds the crate to the
# workspace, and applies the one accessor the session runtime needs
# (`PokeredTask::with_reader`, read-only). Nothing else in the release changes, so the numbers
# are the release's own. A no-op on a tree that already has the tool.
set -euo pipefail

ref="${1:?usage: overlay.sh <scorecard-ref> <target-tree>}"
target="${2:?usage: overlay.sh <scorecard-ref> <target-tree>}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
ws="$target/services/flysim/Cargo.toml"
task="$target/services/flysim/crates/fly-legacy-session/src/task.rs"

[ -f "$ws" ] && [ -f "$task" ] || { echo "overlay: $target is not a flybrain tree with the session runtime" >&2; exit 1; }

if [ ! -d "$target/services/flysim/crates/fly-scorecard" ]; then
    git -C "$repo" archive "$ref" services/flysim/crates/fly-scorecard tools/scorecard docs/scorecard.md \
        | tar -x -C "$target"
fi
if ! grep -q '"crates/fly-scorecard"' "$ws"; then
    sed -i 's#^    "crates/flysim-store",#    "crates/flysim-store",\n    "crates/fly-scorecard",#' "$ws"
    grep -q '"crates/fly-scorecard"' "$ws" || { echo "overlay: could not add the crate to the workspace" >&2; exit 1; }
fi
if ! grep -q 'pub fn with_reader' "$task"; then
    (cd "$target" && git apply --recount "$here/with-reader.patch" 2>/dev/null) \
        || (cd "$target" && patch -p1 --forward < "$here/with-reader.patch") \
        || { echo "overlay: with-reader.patch does not apply to this tree" >&2; exit 1; }
fi
echo "overlay: scorecard from $ref on $target"
