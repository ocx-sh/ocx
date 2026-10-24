#!/bin/sh
# elv-gate.sh: SH-ELV-02 and SH-ELV-03, the elvish gate of record. Compile check, deprecation check, removed-name check.
# Usage: sh scripts/elv-gate.sh [--tolerate-edit] [FILE...]    (run from the repo root; ELV=/path/to/elvish overrides PATH)
#   No FILE: every *.elv that git lists (tracked or untracked, not ignored). A path that holds a newline is unsupported.
#   --tolerate-edit: ignore compile errors that are ONLY "$edit:..." (an rc file is interactive, -compileonly has no $edit:).
#                    Give it rc files only: it blinds the gate to a mistyped $edit: name.
# Exit: 0 clean; 1 a finding; 69 elvish or another tool is missing, or elvish is older than 0.20.1 (no -json before it);
#       74 git or elvish failed in a way that is not a finding; 78 listing needed and this is not a usable git work tree;
#       79 no elvish file to check.
# Findings print to stdout as FILE: reason. Pass is empty stdout and empty stderr.
# Measured on elvish 0.20.1 and 0.21.0, Linux x86_64 (2026-10-11).
set -u
elv=${ELV:-elvish}
tol=0
[ "${1-}" = "--tolerate-edit" ] && {
    tol=1
    shift
}
for t in "$elv" grep tr mktemp; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "elv-gate: '$t' not on PATH" >&2
        exit 69
    }
done
ver=$("$elv" -version 2>/dev/null </dev/null)
case $ver in
    0.20.0* | 0.1[0-9].* | 0.[0-9].* | "")
        echo "elv-gate: elvish 0.20.1 or newer is required (found '$ver')" >&2
        exit 69
        ;;
esac
tmp=$(mktemp -d) || exit 74
trap 'rm -rf "$tmp"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
if [ $# -gt 0 ]; then
    printf '%s\n' "$@" >"$tmp/names"
else
    command -v git >/dev/null 2>&1 || {
        echo "elv-gate: 'git' not on PATH" >&2
        exit 69
    }
    git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
        echo "elv-gate: not a usable git work tree (not a repository, or git refuses it)" >&2
        exit 78
    }
    git ls-files -z --cached --others --exclude-standard -- '*.elv' >"$tmp/list" || exit 74
    tr '\0' '\n' <"$tmp/list" >"$tmp/names"
fi
[ -s "$tmp/names" ] || {
    echo "elv-gate: no elvish file to check" >&2
    exit 79
}
# hit ARGS: grep into the hits file. True on a match, false on none, exit 74 when grep itself failed.
hit() {
    grep "$@" >"$tmp/hits"
    case $? in
        0) return 0 ;;
        1) return 1 ;;
    esac
    echo "elv-gate: grep failed" >&2
    exit 74
}
bad=0
while IFS= read -r f; do
    [ -f "$f" ] && [ -r "$f" ] || {
        echo "elv-gate: cannot read $f" >&2
        exit 74
    }
    out=$("$elv" -compileonly -json -- "$f" 2>"$tmp/err" </dev/null)
    case $? in
        0 | 2) ;;
        *)
            echo "elv-gate: elvish failed on $f" >&2
            exit 74
            ;;
    esac
    printf '%s\n' "$out" | grep -o -e '"message":"[^"]*"' >"$tmp/msgs"
    if [ $tol = 1 ]; then
        grep -v -e 'cannot find variable [$]edit:' -e 'variable [$]edit:.* not found' "$tmp/msgs" >"$tmp/msgs2"
        mv "$tmp/msgs2" "$tmp/msgs"
    fi
    if [ -s "$tmp/msgs" ]; then
        printf '%s: compile: %s\n' "$f" "$(tr '\n' ' ' <"$tmp/msgs")"
        bad=1
    fi
    if grep -q -i -e 'deprecat' "$tmp/err"; then
        printf '%s: deprecated form (elvish stderr)\n' "$f"
        bad=1
    fi
    if hit -n -w -e 'eawk' -e 'float64' -e 'except' -- "$f"; then
        printf '%s: removed name (eawk, float64 or except)\n' "$f"
        head -n 3 "$tmp/hits"
        bad=1
    fi
    if hit -n -E -e '(^|[[:space:]({;|])[A-Za-z_][A-Za-z_0-9]*=[^[:space:]=]+[[:space:]]+[{(A-Za-z]' -e '^[[:space:]]*[/~.][^[:space:]]*[[:space:]]*$' -- "$f"; then
        printf '%s: legacy NAME=value command or implicit cd (both gone or deprecated in 0.21.0)\n' "$f"
        head -n 3 "$tmp/hits"
        bad=1
    fi
done <"$tmp/names"
exit $bad
