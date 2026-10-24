#!/bin/sh
# fish-scan.sh: SH-FISH-02, SH-FISH-06 (emulated errexit) and SH-FISH-07 (the two awk halves). Run from the repo root.
# Output is the violation as FILE:LINE: [tag] text. Tags: bash-habit, or-lhs, or-and-chain.
# Exit: 0 clean, 1 finding, 69 a required tool is missing, 74 the scan itself failed, 78 not a usable git work tree,
#       79 no .fish file in scope.
# A hit is a site to read. A line with a same-line "# tell-ok: REASON" is skipped.
# or-lhs lists a "||" whose left side starts with a word that is not a fish builtin in the allow list below. A known
# function on the left is a false positive. fish 3.6.0 and 4.0.2 skip the right side of "unknown-command || rhs",
# 4.7.0 and 4.9.3 run it (measured 2026-10-11).
set -u
for t in git awk xargs mktemp; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "fish-scan: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "fish-scan: not a usable git work tree" >&2
    exit 78
}
list=$(mktemp) || exit 74
out=$(mktemp) || exit 74
err=$(mktemp) || exit 74
prog=$(mktemp) || exit 74
trap 'rm -f "$list" "$out" "$err" "$prog"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
git ls-files -z --cached --others --exclude-standard -- '*.fish' >"$list" || {
    echo "fish-scan: git ls-files failed" >&2
    exit 74
}
[ -s "$list" ] || {
    echo "fish-scan: no .fish file in scope" >&2
    exit 79
}
cat >"$prog" <<'AWK'
function hit(tag) { printf "%s:%d: [%s] %s\n", FILENAME, FNR, tag, $0; bad = 1 }
# unq: drop the text inside quotes (single or double), where a backtick is a literal character in fish
function unq(s,   o, i, c, q, n) {
    o = ""; q = ""; n = length(s)
    for (i = 1; i <= n; i++) {
        c = substr(s, i, 1)
        if (q != "") { if (c == "\\") i++; else if (c == q) q = ""; continue }
        if (c == "\\") { i++; continue }
        if (c == "\"" || c == "\047") { q = c; continue }
        o = o c
    }
    return o
}
FNR == 1 { prev = "" }
{
    line = $0
    if (line !~ /tell-ok: *[^ ]/) {
        if (line ~ /set -[a-z]* pipefail/ || line ~ /^[ \t]*set -[euo]+[ \t]*(;|$)/ ||
            line ~ /^[^#]*\[\[ .* \]\]/ || unq(line) ~ /^[^#]*\140[^\140]*\140/ ||
            line ~ /^[ \t]*(local|declare)[ \t]/ || line ~ /echo "[^"]*\\[nt]/)
            hit("bash-habit")
        if (prev ~ /^[ \t]*or / && line ~ /^[ \t]*and /)
            hit("or-and-chain")
        s = line
        sub(/^[ \t]+/, "", s)
        if (match(s, /^[a-z][a-z0-9_.-]*[ \t]/)) {
            word = substr(s, 1, RLENGTH - 1)
            rest = substr(s, RLENGTH + 1)
            head = word
            sub(/[^A-Za-z0-9_].*$/, "", head)
            if (head !~ /^(if|and|or|not|while|command|test|set|string|status|functions|contains|count|builtin|type|math|path|echo|return|true|false|read|begin|_)$/) {
                n = match(rest, /[|#]/)
                if (n > 0 && substr(rest, n, 2) == "||")
                    hit("or-lhs")
            }
        }
    }
    prev = line
}
END { exit bad }
AWK
xargs -0 -r awk -f "$prog" -- <"$list" >"$out" 2>"$err"
rc=$?
if [ -s "$err" ]; then
    cat "$err" >&2
    exit 74
fi
[ "$rc" -le 123 ] || exit 74
if [ -s "$out" ]; then
    cat "$out"
    exit 1
fi
exit 0
