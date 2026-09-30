#!/usr/bin/env bash
# tools/install-hooks.sh [--force]
#
# Installs the flybrain pre-push hook into this clone's hooks directory (shared by every
# worktree). On a push to a PUBLIC remote — the remote named `github`, any name listed in
# $FLY_PII_PUBLIC_REMOTES, or any github.com URL — the hook runs tools/pii-scan.sh --push
# over every commit the push would publish (added lines and messages), annotated tag
# messages, and each pushed tip's tree. It fails closed: without the operator's private
# pattern list ($FLY_PII_PATTERNS, default ~/.config/flybrain/pii-patterns.txt) the push is
# refused. Pushes to other remotes are not scanned.
#
# A copy of the scanner is installed next to the hook so branches that predate
# tools/pii-scan.sh are still scanned; re-run this script after the scanner changes.
# `git push --no-verify` skips the hook, as for any git hook; do not.
set -euo pipefail

FORCE=0
[ "${1:-}" != --force ] || FORCE=1

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
hooks_dir="$(git -C "$SELF_DIR" rev-parse --path-format=absolute --git-path hooks)"
mkdir -p "$hooks_dir"
hook="$hooks_dir/pre-push"
marker="# flybrain pii-guard pre-push hook"

if [ -e "$hook" ] && ! grep -qF "$marker" "$hook" && [ "$FORCE" -ne 1 ]; then
    echo "install-hooks: $hook exists and is not ours; merge it by hand or re-run with --force" >&2
    exit 1
fi

install -m 0755 "$SELF_DIR/pii-scan.sh" "$hooks_dir/flybrain-pii-scan.sh"
cat > "$hook" <<'HOOK'
#!/usr/bin/env bash
# flybrain pii-guard pre-push hook (installed by tools/install-hooks.sh; re-run it to update)
set -euo pipefail
remote="$1" url="${2:-}"
public=0
for r in github ${FLY_PII_PUBLIC_REMOTES:-}; do [ "$remote" = "$r" ] && public=1; done
case "$url" in *github.com[:/]*) public=1 ;; esac
[ "$public" -eq 1 ] || exit 0
top="$(git rev-parse --show-toplevel)"
scan="$top/tools/pii-scan.sh"
[ -x "$scan" ] || scan="$(dirname "${BASH_SOURCE[0]}")/flybrain-pii-scan.sh"
allow="$top/infra/tests/de-pii-allow.txt"
if ! FLY_PII_REPO="$top" "$scan" --require-private --allow "$allow" --push "$remote"; then
    echo "pre-push: refusing to publish to '${remote}': fix the findings above (or restore the private pattern list)." >&2
    exit 1
fi
HOOK
chmod 0755 "$hook"
echo "install-hooks: installed $hook (public remotes: github ${FLY_PII_PUBLIC_REMOTES:-}, and github.com URLs)"
