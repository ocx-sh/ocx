#!/bin/sh
# core-weaken: SH-CORE-01 as a script. Prints each line a change ADDS that turns a check green by weakening it, and each changed line 1 or Kind header. It reads the change, not the tree.
# Usage: from the repo root, sh scripts/core-weaken.sh BASE [HEAD]. BASE is the merge target (origin/main). Without HEAD the work tree is compared, so run git add -N . first to include new files. Needs git 2.30 or newer.
# Output is the violation: every printed line needs a one-line reason in the commit, or is removed. A removed matrix leg, an added shell: bash and a shebang change with the Kind header left alone are reading items this script cannot see.
# Exit: 0 nothing weakened; 1 a finding; 64 no BASE argument; 69 a required tool is missing; 74 git diff or awk failed; 78 not a usable git work tree; 79 BASE does not resolve to a commit.
# Kind: gate (output is the violation), so `set -u` and no `-e`. Measured 2026-10-11 on dash 0.5.12, BusyBox ash 1.37, bash 3.2.57 --posix, gawk 5.3.2, mawk 1.3.4 and BusyBox awk.
set -u
[ "$#" -ge 1 ] || {
    echo "core-weaken: usage: core-weaken.sh BASE [HEAD]" >&2
    exit 64
}
for t in git awk mktemp; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "core-weaken: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "core-weaken: not a usable git work tree (not a repository, or dubious ownership)" >&2
    exit 78
}
git rev-parse --verify --quiet "$1^{commit}" >/dev/null || {
    echo "core-weaken: BASE does not resolve to a commit: $1" >&2
    exit 79
}
diff=''
trap 'rm -f "$diff"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
diff=$(mktemp) || exit 74
# Markdown is excluded: rules and skills quote the forms they forbid.
if [ "$#" -ge 2 ]; then
    git -c core.quotepath=off diff -U0 --no-color --merge-base "$1" "$2" -- . ':(exclude)*.md' >"$diff" || exit 74
else
    git -c core.quotepath=off diff -U0 --no-color --merge-base "$1" -- . ':(exclude)*.md' >"$diff" || exit 74
fi
found=$(
    awk '
    /^[+][+][+] / { f = substr($0, 7); sub(/\t.*$/, "", f); next }
    /^@@ / { fresh = ($2 == "-0,0"); first = ($2 ~ /^-1(,|$)/ && $3 ~ /^[+]1(,|$)/); next }
    /^[-+]#!/ { if (first && !fresh) print f ": shebang line changed: " $0; next }
    /^[-+]#[ \t]*Kind:/ { if (!fresh) print f ": Kind header changed: " $0; next }
    /^[+]/ {
        l = substr($0, 2)
        if (l ~ /shellcheck[ \t]+disable=[A-Za-z0-9,-]*[ \t]*$/) print f ": directive with no reason: " l
        else if (l ~ /[|][|][ \t]*(true|:)[ \t]*$/) print f ": true-fallback with no reason: " l
        else if (l ~ /^[ \t]*set[ \t]+[+]([a-zA-Z]*e|o[ \t]+errexit)([ \t]|$)/) print f ": errexit turned off: " l
        else if (l ~ /shellcheck[^|;&]*( -S|--severity)/ || l ~ /SHELLCHECK_OPTS.*(-S|severity)/) print f ": severity floor: " l
        else if (f ~ /(^|\/)[.]?shellcheckrc$/ && l ~ /^[ \t]*(severity|shell|disable)=/) print f ": rc key that hides findings: " l
    }' "$diff"
) || {
    echo "core-weaken: awk failed while reading the diff" >&2
    exit 74
}
[ -z "$found" ] || {
    printf '%s\n' "$found"
    exit 1
}
exit 0
