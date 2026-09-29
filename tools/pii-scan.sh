#!/usr/bin/env bash
# tools/pii-scan.sh — refuse identifying strings and secrets in what this public repo publishes.
#
#   tools/pii-scan.sh [options] --tree [REV]       tracked files at REV (default: the checkout)
#   tools/pii-scan.sh [options] --range REVS...    lines ADDED by those commits, plus their
#                                                  messages (git rev-list syntax: A..B, X --not Y)
#   tools/pii-scan.sh [options] --push REMOTE      pre-push mode: reads git's pre-push stdin
#                                                  ("<lref> <lsha> <rref> <rsha>" lines) and scans
#                                                  every commit the push would publish, their
#                                                  messages, annotated tag messages, and each tip
#   tools/pii-scan.sh --list                       pattern names in force (never the regexes)
#
# Options:
#   --patterns FILE     the PRIVATE pattern list (default $FLY_PII_PATTERNS, else
#                       ${XDG_CONFIG_HOME:-~/.config}/flybrain/pii-patterns.txt)
#   --require-private   exit 2 when that file is missing (tag-release and pre-push use this:
#                       fail closed). Without it a missing file is a NOTE and only the generic
#                       patterns below run (public CI has no private list, by design).
#   --allow FILE        allowlist (default infra/tests/de-pii-allow.txt)
#
# Exit: 0 clean, 1 findings, 2 usage or configuration error.
#
# Why two pattern sets. The generic shapes below (LAN addresses, container ids, host paths,
# e-mail addresses, credential shapes) contain no proper noun, so they can live here. The
# proper nouns — host names, the operator's names and handles, the channel, account ids,
# `pass` entry names, the forge — are exactly what must not be published, so they live in a
# file outside this repo and are loaded at run time. A guard that spelled them out (even with
# one character bracketed) would itself be the leak.
#
# Private file format, one per line (`#` comments):
#   <name> <flags> <extended regex>      flags: -i (ignore case) or - (case-sensitive)
#   @values <env-file>                   each KEY=VALUE value of 8+ chars in that file becomes a
#                                        literal needle, reported as secret-value:<KEY>, with
#                                        the matched text never printed
#
# Findings print as `pii-scan: <name>: <where>: <redacted match>`; the match is cut to its
# first three characters so a log of the scan does not republish what it found.
set -euo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# FLY_PII_REPO: the checkout to scan. The pre-push hook sets it, because its fallback copy of
# this script lives in .git/hooks, outside any work tree.
REPO_ROOT="${FLY_PII_REPO:-$(git -C "$SELF_DIR" rev-parse --show-toplevel 2>/dev/null || (cd "$SELF_DIR/.." && pwd))}"

usage() { sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }
die() { echo "pii-scan: $*" >&2; exit 2; }

PATTERNS_FILE="${FLY_PII_PATTERNS:-${XDG_CONFIG_HOME:-$HOME/.config}/flybrain/pii-patterns.txt}"
ALLOW_FILE="$REPO_ROOT/infra/tests/de-pii-allow.txt"
REQUIRE_PRIVATE=0
MODE=""
MODE_ARGS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --patterns) PATTERNS_FILE="${2:?}"; shift 2 ;;
        --allow) ALLOW_FILE="${2:?}"; shift 2 ;;
        --require-private) REQUIRE_PRIVATE=1; shift ;;
        --tree|--range|--push|--list) MODE="$1"; shift; MODE_ARGS=("$@"); break ;;
        -h|--help) usage ;;
        *) die "unknown argument: $1 (see --help)" ;;
    esac
done
[ -n "$MODE" ] || usage

TMP="$(mktemp -d)"
chmod 700 "$TMP"
trap 'rm -rf "$TMP"' EXIT

# --------------------------------------------------------------------------------------
# Patterns. NAMES/FLAGS/RES are parallel arrays; KIND is `re` or `values` (a -F -f file).
# The two path patterns keep one bracketed character only so this file does not match
# itself; they are not proper nouns.
# --------------------------------------------------------------------------------------
NAMES=(); FLAGS=(); RES=(); KIND=()
pat() { NAMES+=("$1"); FLAGS+=("$2"); RES+=("$3"); KIND+=(re); }

#   name                  flags  extended regex
pat lan-address           -i  '\b192\.168\.[0-9]{1,3}\.[0-9]{1,3}\b|\b2600:[0-9a-f]'
pat private-address       -   '\b10\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\b|\b172\.(1[6-9]|2[0-9]|3[01])\.[0-9]{1,3}\.[0-9]{1,3}\b'
pat tailnet-address       -   '\b100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]{1,3}\.[0-9]{1,3}\b'
pat private-dns           -i  '(@|://)[a-z0-9.-]+\.(internal|lan|home\.arpa|ts\.net)\b|\b[a-z0-9-]+\.ts\.net\b|\bheadscale\b'
pat container-id          -i  '\b(ct|vm)[ _-]?[12][0-9][0-9]\b|\bpct +[a-z-]+ +[0-9]{2,4}\b|\bqm +[a-z-]+ +[0-9]{2,4}\b|subvol-[12][0-9][0-9]-disk|/lxc/[12][0-9][0-9]\.conf'
pat host-root-path        -   '/ro[o]t/'
pat home-path             -   '/ho[m]e/[a-z]'
pat claim-log-name        -   'AGENTS[_]LOG'
pat mac-address           -   '\b([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}\b'
pat email-address         -   '[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}'
pat pass-entry-path       -   '\bpass +(show|insert|edit|generate) +[A-Za-z0-9._-]+/'
pat secret-private-key    -   '-----BEGIN [A-Z ]*PRIVATE KEY-----'
pat secret-token-prefix   -   '\bsk-(or-v1-|proj-|ant-)?[A-Za-z0-9_-]{20,}|\bgh[pousr]_[A-Za-z0-9]{30,}|\bgithub_pat_[A-Za-z0-9_]{30,}|\bAKIA[0-9A-Z]{16}\b|\bxox[baprs]-[A-Za-z0-9-]{10,}|\bglpat-[A-Za-z0-9_-]{20,}'
pat secret-twitch         -   'oauth:[a-z0-9]{20,}|\blive_[0-9]{6,}_[A-Za-z0-9]{20,}'
pat secret-client-id      -i  '(client[_-]?id|clientid)["'"'"' ]*[:=][ "'"'"']*[a-z0-9]{30}\b'
pat secret-bearer         -   '\b(Bearer|Basic) [A-Za-z0-9._~+/=-]{24,}'
pat secret-assignment     -   '(KEY|SECRET|TOKEN|PASSWORD|PASSWD)["'"'"']?[ ]*[:=][ ]*["'"'"']?[A-Za-z0-9][A-Za-z0-9_+/=-]{19,}'

PRIVATE_LOADED=0
if [ -r "$PATTERNS_FILE" ]; then
    PRIVATE_LOADED=1
    lineno=0
    while IFS= read -r raw || [ -n "$raw" ]; do
        lineno=$((lineno + 1))
        line="${raw#"${raw%%[![:space:]]*}"}"
        case "$line" in ''|'#'*) continue ;; esac
        if [ "${line%% *}" = "@values" ]; then
            vfile="${line#@values}"; vfile="${vfile#"${vfile%%[![:space:]]*}"}"
            vfile="${vfile/#\~/$HOME}"
            [ -r "$vfile" ] || die "$PATTERNS_FILE:$lineno: @values file is not readable: $vfile"
            while IFS= read -r kv || [ -n "$kv" ]; do
                case "$kv" in ''|'#'*) continue ;; esac
                kv="${kv#export }"
                case "$kv" in *=*) ;; *) continue ;; esac
                key="${kv%%=*}"; val="${kv#*=}"
                val="${val%\"}"; val="${val#\"}"; val="${val%\'}"; val="${val#\'}"
                [ "${#val}" -ge 8 ] || continue
                n=${#NAMES[@]}
                printf '%s\n' "$val" > "$TMP/needle.$n"
                NAMES+=("secret-value:${key}"); FLAGS+=(-); RES+=("$TMP/needle.$n"); KIND+=(values)
            done < "$vfile"
            continue
        fi
        read -r pname pflags pre <<<"$line"
        [ -n "${pre:-}" ] || die "$PATTERNS_FILE:$lineno: expected '<name> <flags> <regex>'"
        case "$pflags" in -i|-) ;; *) die "$PATTERNS_FILE:$lineno: flags must be -i or -, got '$pflags'" ;; esac
        pat "$pname" "$pflags" "$pre"
    done < "$PATTERNS_FILE"
elif [ "$REQUIRE_PRIVATE" -eq 1 ]; then
    die "private pattern list not found: ${PATTERNS_FILE} (set FLY_PII_PATTERNS). Refusing: without it the scan cannot see host names, people's names, the channel or pass entry names."
else
    echo "pii-scan: NOTE: no private pattern list (${PATTERNS_FILE}); generic patterns only" >&2
fi

if [ "$MODE" = "--list" ]; then
    printf '%s\n' "${NAMES[@]}"
    exit 0
fi

# --------------------------------------------------------------------------------------
# Allowlist: `<path> <pattern-name>  # reason`; path may end in / (prefix) or hold a glob.
# --------------------------------------------------------------------------------------
ALLOW_PATHS=(); ALLOW_NAMES=()
if [ -r "$ALLOW_FILE" ]; then
    while read -r a_path a_name _; do
        case "$a_path" in ''|'#'*) continue ;; esac
        ALLOW_PATHS+=("$a_path"); ALLOW_NAMES+=("$a_name")
    done < "$ALLOW_FILE"
fi
allowed() {
    local path="$1" name="$2" i a
    for i in "${!ALLOW_PATHS[@]}"; do
        a="${ALLOW_NAMES[$i]}"
        [ "$a" = "$name" ] || [ "$a" = '*' ] || continue
        a="${ALLOW_PATHS[$i]}"
        case "$a" in
            */) case "$path" in "$a"*) return 0 ;; esac ;;
            *\**) # shellcheck disable=SC2254
                  case "$path" in $a) return 0 ;; esac ;;
            *)    [ "$path" = "$a" ] && return 0 ;;
        esac
    done
    return 1
}

HITS=0
# report NAME PATH WHERE MATCH
report() {
    local name="$1" path="$2" where="$3" match="$4" shown
    allowed "$path" "$name" && return 0
    case "$name" in
        secret-*) shown="<redacted ${#match} chars>" ;;
        *) shown="${match:0:3}… (${#match} chars)" ;;
    esac
    echo "pii-scan: ${name}: ${where}: ${shown}"
    HITS=$((HITS + 1))
}

grep_flags() { # INDEX -> grep flags for regex pattern i (value needles use -F -f)
    local i="$1"
    # printf, not echo: `echo -E` is echo's own option and prints nothing.
    if [ "${FLAGS[$i]}" = -i ]; then printf '%s\n' "-i -E"; else printf '%s\n' "-E"; fi
}

# --------------------------------------------------------------------------------------
# --tree [REV]
# --------------------------------------------------------------------------------------
scan_tree() {
    local rev="${1:-}" i flags line path rest lno match is_git=0
    git -C "$REPO_ROOT" rev-parse --git-dir >/dev/null 2>&1 && is_git=1
    [ -z "$rev" ] || [ "$is_git" -eq 1 ] || die "--tree $rev needs a git checkout"
    for i in "${!NAMES[@]}"; do
        flags="$(grep_flags "$i")"
        while IFS= read -r line; do
            [ -n "$line" ] || continue
            [ -z "$rev" ] || line="${line#"$rev":}"
            path="${line%%:*}"; rest="${line#*:}"; lno="${rest%%:*}"; match="${rest#*:}"
            report "${NAMES[$i]}" "$path" "$path:$lno" "$match"
        done < <(
            if [ "${KIND[$i]}" = values ]; then set -- -F -f "${RES[$i]}"; else set -- $flags -e "${RES[$i]}"; fi
            if [ "$is_git" -eq 1 ]; then
                # shellcheck disable=SC2086
                git -C "$REPO_ROOT" grep -noI "$@" ${rev:+"$rev"} -- . ':!data' ':!package-lock.json' 2>/dev/null || true
            else
                grep -rnoI "$@" "$REPO_ROOT" --exclude-dir=data --exclude-dir=.git \
                    --exclude-dir=node_modules --exclude-dir=.local --exclude-dir=target \
                    --exclude=package-lock.json 2>/dev/null | sed "s|^${REPO_ROOT}/||" || true
            fi
        )
    done
}

# --------------------------------------------------------------------------------------
# --range REVS...: added lines (corpus + index "commit path line") and commit messages.
# --------------------------------------------------------------------------------------
scan_corpus() { # CORPUS INDEX
    local corpus="$1" index="$2" i flags
    [ -s "$corpus" ] || return 0
    for i in "${!NAMES[@]}"; do
        flags="$(grep_flags "$i")"
        while IFS=$'\t' read -r commit path lno match; do
            report "${NAMES[$i]}" "$path" "${commit:0:10} $path:$lno" "$match"
        done < <(
            # shellcheck disable=SC2086
            if [ "${KIND[$i]}" = values ]; then grep -noa -F -f "${RES[$i]}" "$corpus" || true
            else grep -noa $flags -e "${RES[$i]}" "$corpus" || true; fi \
            | awk -F'\t' 'NR==FNR { idx[NR] = $0; next }
                          { n = index($0, ":"); k = substr($0, 1, n - 1) + 0;
                            print idx[k] "\t" substr($0, n + 1) }' "$index" -
        )
    done
}

scan_range() {
    local corpus="$TMP/added" index="$TMP/added.idx"
    git -C "$REPO_ROOT" log -p --no-color --no-ext-diff --no-renames --unified=0 \
        --format='PIISCAN-COMMIT %H' "$@" -- . ':!data' ':!package-lock.json' \
    | awk -v corpus="$corpus" -v index_f="$index" '
        /^PIISCAN-COMMIT / { commit = $2; path = ""; next }
        /^\+\+\+ /         { path = substr($0, 5); sub(/^b\//, "", path); if (path == "/dev/null") path = ""; next }
        /^@@ /             { if (match($0, /\+[0-9]+/)) ln = substr($0, RSTART + 1, RLENGTH - 1) + 0; next }
        /^\+/              { if (path != "") { print substr($0, 2) > corpus; print commit "\t" path "\t" ln > index_f }; ln++; next }
    '
    touch "$corpus" "$index"
    scan_corpus "$corpus" "$index"

    local mcorpus="$TMP/msgs" mindex="$TMP/msgs.idx"
    git -C "$REPO_ROOT" log --no-color --format='PIISCAN-COMMIT %H%n%B' "$@" \
    | awk -v corpus="$mcorpus" -v index_f="$mindex" '
        /^PIISCAN-COMMIT / { commit = $2; ln = 0; next }
        { ln++; print $0 > corpus; print commit "\t<commit-message>\t" ln > index_f }
    '
    touch "$mcorpus" "$mindex"
    scan_corpus "$mcorpus" "$mindex"
}

scan_tag_message() { # TAG-OBJECT-SHA
    local corpus="$TMP/tagmsg" index="$TMP/tagmsg.idx"
    git -C "$REPO_ROOT" cat-file tag "$1" | sed '1,/^$/d' > "$corpus"
    awk -v t="$1" '{ print t "\t<tag-message>\t" NR }' "$corpus" > "$index"
    scan_corpus "$corpus" "$index"
}

# --------------------------------------------------------------------------------------
# --push REMOTE: git's pre-push protocol on stdin.
# --------------------------------------------------------------------------------------
ZERO=0000000000000000000000000000000000000000
scan_push() {
    local remote="$1" lref lsha rref rsha
    while read -r lref lsha rref rsha; do
        [ -n "${lsha:-}" ] || continue
        case "$lsha" in *[!0]*) ;; *) continue ;; esac   # branch/tag deletion publishes nothing
        echo "pii-scan: checking ${lref} -> ${remote} ${rref}" >&2
        if [ "$(git -C "$REPO_ROOT" cat-file -t "$lsha")" = tag ]; then
            scan_tag_message "$lsha"
        fi
        if [ "$rsha" != "$ZERO" ] && git -C "$REPO_ROOT" cat-file -e "${rsha}^{commit}" 2>/dev/null; then
            scan_range "${rsha}..${lsha}"
        else
            scan_range "$lsha" --not --remotes="$remote"
        fi
        scan_tree "$(git -C "$REPO_ROOT" rev-parse "${lsha}^{commit}")"
    done
}

case "$MODE" in
    --tree)  [ ${#MODE_ARGS[@]} -le 1 ] || die "--tree takes at most one REV"
             scan_tree "${MODE_ARGS[0]:-}" ;;
    --range) [ ${#MODE_ARGS[@]} -ge 1 ] || die "--range needs revisions"
             scan_range "${MODE_ARGS[@]}" ;;
    --push)  [ ${#MODE_ARGS[@]} -eq 1 ] || die "--push takes the remote name"
             scan_push "${MODE_ARGS[0]}" ;;
esac

if [ "$HITS" -gt 0 ]; then
    echo "pii-scan: ${HITS} finding(s) (private list: $([ "$PRIVATE_LOADED" -eq 1 ] && echo loaded || echo absent)). Redact them — real values belong in the operator's infra repo — or, if a mention is genuinely not identifying, add '<path> <name>  # reason' to infra/tests/de-pii-allow.txt." >&2
    exit 1
fi
echo "pii-scan: clean (${#NAMES[@]} patterns, private list $([ "$PRIVATE_LOADED" -eq 1 ] && echo loaded || echo absent))" >&2
