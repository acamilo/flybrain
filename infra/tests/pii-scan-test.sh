#!/usr/bin/env bash
# infra/tests/pii-scan-test.sh — tests for tools/pii-scan.sh and tools/install-hooks.sh.
#
# Everything runs in throwaway repos under a temp dir, against a throwaway private pattern
# list. The fake identifying values are assembled at run time from pieces, so no complete
# fake value (and no real one) appears in this file for the tree scan to trip over.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

FAILS=0
ok()   { echo "ok   - $*"; }
bad()  { echo "FAIL - $*"; FAILS=$((FAILS + 1)); }
# expect RC DESC -- CMD...: run CMD, capture output in $OUT, check the exit code
# the tree scan covers TRACKED files (git grep), like the guard it replaced
track() { git -C "$W" add -A; }
expect() {
    local want="$1" desc="$2"; shift 3
    local rc=0
    OUT="$("$@" 2>&1)" || rc=$?
    if [ "$rc" -eq "$want" ]; then ok "$desc"; else bad "$desc (exit $rc, wanted $want)"; printf '%s\n' "$OUT" | sed 's/^/      /'; fi
}
contains()     { case "$OUT" in *"$1"*) ok "$2" ;; *) bad "$2 (missing '$1')"; printf '%s\n' "$OUT" | sed 's/^/      /' ;; esac; }
not_contains() { case "$OUT" in *"$1"*) bad "$2 (output leaks '$1')" ;; *) ok "$2" ;; esac; }

# --- fake private values, built from pieces -------------------------------------------
HOST="zorb""lequin"                                  # a "host name"
PERSON="Quen""tavius"                                # a "person"
SECRET="fk""Q7pL2xV9""mZ4rT8wY""1nB6"                # a "router key"
LAN="192.168"".77.21"
MAIL="someone""@""example.org"

mkdir -p "$T/private"
printf 'FAKE_ROUTER_KEY=%s\nSHORT=abc\n' "$SECRET" > "$T/private/router.env"
cat > "$T/private/patterns.txt" <<EOF
# test list
house-host   -i  ${HOST}
operator-name -  \\b${PERSON}\\b
@values $T/private/router.env
EOF
export FLY_PII_PATTERNS="$T/private/patterns.txt"

# --- a throwaway repo with the scanner in it ------------------------------------------
W="$T/work"
git init -q -b main "$W"
git -C "$W" config user.name lint
git -C "$W" config user.email lint@example.invalid
mkdir -p "$W/tools" "$W/infra/tests" "$W/src"
cp "$REPO/tools/pii-scan.sh" "$REPO/tools/install-hooks.sh" "$W/tools/"
printf '# allowlist\n' > "$W/infra/tests/de-pii-allow.txt"
echo "hello" > "$W/src/a.txt"
git -C "$W" add -A && git -C "$W" commit -qm "init"
SCAN="$W/tools/pii-scan.sh"

expect 0 "clean tree passes" -- "$SCAN" --tree
contains "private list loaded" "private list is reported loaded"
expect 0 "--list prints names" -- "$SCAN" --list
contains "house-host" "--list includes private names"
not_contains "$HOST" "--list never prints a private regex"

# generic pattern
echo "feed at ws://${LAN}:7400" > "$W/src/b.txt"
track
expect 1 "LAN address in the tree fails" -- "$SCAN" --tree
contains "lan-address: src/b.txt:1:" "finding names the pattern and the location"
not_contains "$LAN" "the address itself is not echoed"
git -C "$W" rm -qf src/b.txt

# private noun, case-insensitive
echo "ssh root@$(echo "$HOST" | tr a-z A-Z).example" > "$W/src/c.txt"
track
expect 1 "private host name fails" -- "$SCAN" --tree
contains "house-host: src/c.txt:1:" "private name reported by its pattern name"
not_contains "$HOST" "private host name is not echoed"
git -C "$W" rm -qf src/c.txt

# case-sensitive private pattern: lowercase fixture passes, capitalised fails
echo "user: $(echo "$PERSON" | tr A-Z a-z)" > "$W/src/d.txt"
track
expect 0 "case-sensitive pattern ignores the lowercase fixture" -- "$SCAN" --tree
echo "by ${PERSON}" > "$W/src/d.txt"
track
expect 1 "case-sensitive pattern catches the capitalised name" -- "$SCAN" --tree
git -C "$W" rm -qf src/d.txt

# literal secret value from @values
echo "KEY=${SECRET}" > "$W/src/e.env"
track
expect 1 "a literal secret value fails" -- "$SCAN" --tree
contains "secret-value:FAKE_ROUTER_KEY: src/e.env:1:" "value finding names the env KEY"
not_contains "${SECRET:0:6}" "secret value is fully redacted"
git -C "$W" rm -qf src/e.env

# credential shape
echo "export STREAM_KEY=\"live_123456""7_abcdefghijABCDEFGHIJ123\"" > "$W/src/f.sh"
track
expect 1 "stream-key shape fails" -- "$SCAN" --tree
contains "secret-twitch" "stream key reported as secret-twitch"
git -C "$W" rm -qf src/f.sh

# allowlist
echo "contact ${MAIL}" > "$W/src/g.txt"
track
expect 1 "e-mail address fails" -- "$SCAN" --tree
printf 'src/g.txt email-address  # upstream licence contact\n' >> "$W/infra/tests/de-pii-allow.txt"
expect 0 "allowlisted path/name pair passes" -- "$SCAN" --tree
printf 'src/ house-host  # a different name does not excuse\n' >> "$W/infra/tests/de-pii-allow.txt"
echo "${LAN}" > "$W/src/g2.txt"
track
expect 1 "an allow entry excuses only its own pattern name" -- "$SCAN" --tree
git -C "$W" rm -qf src/g.txt src/g2.txt
printf '# allowlist\n' > "$W/infra/tests/de-pii-allow.txt"

# fail closed / open
expect 2 "--require-private without the list exits 2" -- env FLY_PII_PATTERNS="$T/nope" "$SCAN" --require-private --tree
contains "private pattern list not found" "missing list is named"
expect 0 "without --require-private a missing list is a NOTE" -- env FLY_PII_PATTERNS="$T/nope" "$SCAN" --tree
contains "NOTE: no private pattern list" "NOTE printed"
printf 'broken-line\n' > "$T/private/broken.txt"
expect 2 "a malformed private list is an error, not a silent pass" -- env FLY_PII_PATTERNS="$T/private/broken.txt" "$SCAN" --tree

# range: added then removed is still published history
base="$(git -C "$W" rev-parse HEAD)"
echo "on ${HOST} today" > "$W/src/h.txt"; git -C "$W" add -A; git -C "$W" commit -qm "add h"
git -C "$W" rm -q src/h.txt; git -C "$W" commit -qm "remove h"
expect 0 "tree is clean after the removal" -- "$SCAN" --tree
expect 1 "range still finds the line a commit added" -- "$SCAN" --range "${base}..HEAD"
contains "house-host:" "range finding names the pattern"
contains "src/h.txt:1" "range finding names path and line"
git -C "$W" commit -q --allow-empty -m "deploy to ${HOST}"
expect 1 "range scans commit messages" -- "$SCAN" --range "HEAD~1..HEAD"
contains "<commit-message>:1" "message finding is marked"
git -C "$W" reset -q --hard "$base"
git -C "$W" status --porcelain | grep -q . && bad "work repo not clean before the hook tests" || true

# --- pre-push hook ---------------------------------------------------------------------
git init -q --bare "$T/pub.git"
git init -q --bare "$T/priv.git"
git -C "$W" remote add github "$T/pub.git"
git -C "$W" remote add origin "$T/priv.git"
expect 0 "install-hooks installs" -- "$W/tools/install-hooks.sh"
[ -x "$W/.git/hooks/pre-push" ] && ok "hook is executable" || bad "hook missing"
expect 0 "install-hooks is idempotent" -- "$W/tools/install-hooks.sh"
expect 0 "clean push to github is accepted" -- git -C "$W" push -q github main
echo "at ${LAN}" > "$W/src/i.txt"; git -C "$W" add -A; git -C "$W" commit -qm "i"
expect 1 "push with PII to github is refused" -- git -C "$W" push -q github main
contains "refusing to publish" "hook explains the refusal"
expect 0 "push to a private remote is not scanned" -- git -C "$W" push -q origin main
git -C "$W" rm -q src/i.txt; git -C "$W" commit -qm "rm i"
expect 1 "removing it in a later commit does not unblock the push" -- git -C "$W" push -q github main
git -C "$W" reset -q --hard github/main
git -C "$W" tag -a v9.9.9 -m "release for ${HOST}"
expect 1 "annotated tag message is scanned" -- git -C "$W" push -q github v9.9.9
contains "<tag-message>" "tag finding is marked"
git -C "$W" tag -d v9.9.9 >/dev/null
git -C "$W" tag -a v9.9.8 -m "release"
expect 0 "clean annotated tag is accepted" -- git -C "$W" push -q github v9.9.8
git -C "$W" commit -q --allow-empty -m "clean"
expect 1 "push is refused when the private list is missing" -- env FLY_PII_PATTERNS="$T/nope" git -C "$W" push -q github main
git -C "$W" checkout -q -b old "$base"
rm -f "$W/tools/pii-scan.sh"
echo "x ${HOST}" > "$W/src/j.txt"; git -C "$W" add -A; git -C "$W" commit -qm "j"
expect 1 "a branch without tools/pii-scan.sh is scanned by the hook's own copy" -- git -C "$W" push -q github old
printf 'exit 0\n' > "$T/other-hook"
git -C "$W" checkout -q main
cp "$T/other-hook" "$W/.git/hooks/pre-push"
expect 1 "install-hooks refuses to clobber a foreign hook" -- "$W/tools/install-hooks.sh"

# --- review-pii-guard N2/N3 (2026-09-29) ----------------------------------------------
printf 'broken - (unclosed\n' > "$T/private/bad-patterns.txt"
expect 2 "an invalid private regex refuses to scan (never 'clean')" -- env FLY_PII_PATTERNS="$T/private/bad-patterns.txt" "$SCAN" --tree
printf '++ %s\n' "$LAN" > "$W/src/plusplus.txt"
git -C "$W" add -A && git -C "$W" commit -qm "a line that starts with ++"
expect 1 "an added line starting '++ ' is still scanned" -- "$SCAN" --range HEAD~1..HEAD
git -C "$W" rm -qf src/plusplus.txt && git -C "$W" commit -qm "drop it"

echo "==="
if [ "$FAILS" -eq 0 ]; then echo "pii-scan-test: ALL PASSED"; else echo "pii-scan-test: ${FAILS} FAILED"; exit 1; fi
