#!/bin/sh
# Kind: repo tool. Dialect: POSIX sh. Floor: POSIX.1-2017. Tested: dash 0.5.12, busybox ash 1.37, bash 3.2 --posix.
# Lists every line with a non-ASCII or control byte in the files git lists under the pathspecs (SH-EMIT-40, SH-EMIT-08).
# Tab and CR are allowed, so a CRLF file of printable ASCII is clean. The pathspecs name emitted or generated text
# that git can see, and an ignored build directory lists nothing, so that case exits 79 and never reads as clean.
# Usage: sh emit-ascii.sh PATHSPEC...
# Exit: 0 clean; 1 one FILE:LINE line per offending line on stdout; 69 git, xargs, awk or mktemp missing;
# 74 git or awk failed; 78 bad usage or not a git work tree; 79 no file matched.
set -u
[ "$#" -gt 0 ] || {
    echo "usage: sh emit-ascii.sh PATHSPEC..." >&2
    exit 78
}
for t in git xargs awk mktemp; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "emit-ascii: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "emit-ascii: not a git work tree" >&2
    exit 78
}
tmp=$(mktemp "${TMPDIR:-/tmp}/emit-ascii.XXXXXX") || exit 74
trap 'rm -f "$tmp"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
git ls-files -z --cached --others --exclude-standard -- "$@" >"$tmp" || exit 74
[ -s "$tmp" ] || {
    echo "emit-ascii: no file matched" >&2
    exit 79
}
LC_ALL=C
export LC_ALL
out=$(xargs -0 -r awk '/[^ -~\t\r]/ { print FILENAME ":" FNR ": non-ASCII or control byte" }' <"$tmp") || exit 74
[ -z "$out" ] && exit 0
printf '%s\n' "$out"
exit 1
