#!/bin/sh
# gates-scan: SH-GATE-31 reference. SH-GATE-01 (no severity flag) as a script instead of a table cell. Copy and replace the pathspecs and patterns. Shell content needs the shfmt list of SH-GATE-22 instead of an extension pathspec.
# Run from the repo root. Output is the violation.
# Exit: 0 clean; 1 a finding; 69 a required tool is missing; 74 the scan itself failed (a tool reported an error); 78 not a usable git work tree; 79 no files in scope.
# The patterns spell their own hyphens and first letters as bracket classes so the scan does not report itself.
# Shape: the list goes to a file first so git's own status is judged and not hidden behind a pipe, then a null-separated batch of grep -H -n -s.
# Kind: gate (output is the violation), so `set -u` and no `-e`. Measured 2026-10-11 on dash 0.5.12, BusyBox ash 1.37, bash 3.2.57 --posix, GNU grep 3.11 and BusyBox grep.
set -u
for t in git grep xargs mktemp; do # vd-ok: xargs here is a tool name, not an invocation
    command -v "$t" >/dev/null 2>&1 || {
        echo "gates-scan: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "gates-scan: not a usable git work tree (not a repository, or dubious ownership)" >&2
    exit 78
}
list=$(mktemp) || exit 74
out=$(mktemp) || exit 74
err=$(mktemp) || exit 74
trap 'rm -f "$list" "$out" "$err"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.mk' ':(glob)**/Makefile' >"$list" || {
    echo "gates-scan: git ls-files failed" >&2
    exit 74
}
[ -s "$list" ] || {
    echo "gates-scan: no files in scope" >&2
    exit 79
}
xargs -0 -r grep -H -n -s -e 'shellcheck.* [-]S' -e 'shellcheck.*[-][-]severity' -e '[S]HELLCHECK_OPTS.*[-]S' -e '[S]HELLCHECK_OPTS.*severity' -- <"$list" >"$out" 2>"$err"
rc=$?
if [ -s "$err" ]; then
    cat "$err" >&2
    exit 74 # any stderr text from the scan: it is not trustworthy
fi
[ "$rc" -le 123 ] || exit 74 # 124 to 127: the batch could not run grep, or grep exited 255 or was killed
if [ -s "$out" ]; then
    cat "$out"
    exit 1
fi
exit 0
