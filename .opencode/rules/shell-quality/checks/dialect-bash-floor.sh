#!/bin/sh
# Kind: repo tool. Dialect: POSIX sh. Floor: POSIX.1-2017. Tested: dash 0.5.12-2, busybox ash 1.37, bash 3.2 --posix.
# Prints every bash 4+ construct (SH-DIAL-06) and every silent-drift shopt, BASH_COMPAT or BASH_* variable
# (SH-DIAL-28) in a shell file that has no "# bash-floor:" line. Run from the repo root.
# Usage: sh dialect-bash-floor.sh [PATHSPEC...]   default: the whole work tree; prune with ':(exclude,glob)**/vendor/**'
# Exit: 0 clean; 1 findings printed; 69 a tool is missing; 74 the scan failed; 78 not a usable git work tree;
# 79 no shell files in scope. Needs git, shfmt, grep, xargs, tr, mktemp.
set -u
for t in git grep xargs shfmt tr mktemp; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "dialect-bash-floor: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "dialect-bash-floor: not a usable git work tree" >&2
    exit 78
}
if [ $# -eq 0 ]; then
    set -- .
fi
tmp=$(mktemp -d "${TMPDIR:-/tmp}/dialect-floor.XXXXXX") || exit 74
trap 'rm -rf "$tmp"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

git ls-files -z --cached --others --exclude-standard -- "$@" >"$tmp/all" || {
    echo "dialect-bash-floor: git ls-files failed" >&2
    exit 74
}
# shellcheck disable=SC2016 # single quotes are deliberate: the $f is for the inner sh, not this shell
xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ <"$tmp/all" | xargs -0 -r shfmt -f -- | tr '\n' '\0' >"$tmp/shell"
[ -s "$tmp/shell" ] || {
    echo "dialect-bash-floor: no shell files in scope" >&2
    exit 79
}
xargs -0 -r grep -s -L -E -e '^# bash-floor: ' -- <"$tmp/shell" 2>"$tmp/err" | tr '\n' '\0' >"$tmp/unpinned"
if [ -s "$tmp/err" ]; then
    cat "$tmp/err" >&2
    exit 74
fi
[ -s "$tmp/unpinned" ] || exit 0

xargs -0 -r grep -s -H -n -E \
    -e '^[^#]*(declare|local|typeset|readonly)[[:space:]]+(-[a-zA-Z]+[[:space:]]+)*-[a-zA-Z]*A' \
    -e '^[^#]*(mapfile|readarray)[[:space:]]' \
    -e '^[^#]*\$\{[A-Za-z_0-9@*]+(\[[^]]*\])?[,^]' \
    -e '^[^#]*(declare|local)[[:space:]]+(-[a-zA-Z]+[[:space:]]+)*-[a-zA-Z]*[ul][[:space:]]' \
    -e '^[^#]*\[\[?[[:space:]]+(![[:space:]]+)?-[vR][[:space:]]' \
    -e '^[^#]*(\$\{|[[:space:];(])[A-Za-z_][A-Za-z_0-9]*\[-[0-9]+\][}=]' \
    -e '^[^#]*:[0-9]+:-[0-9]' \
    -e '^[^#]*printf[[:space:]]+[^;]*%\([^)]*\)T' \
    -e '^[^#]*declare[[:space:]]+(-[a-zA-Z]+[[:space:]]+)*-[a-zA-Z]*[ng][[:space:]]' \
    -e '^[^#]*(local|typeset)[[:space:]]+(-[a-zA-Z]+[[:space:]]+)*-[a-zA-Z]*n[[:space:]]' \
    -e '^[^#]*wait[[:space:]]+(-[a-zA-Z]+[[:space:]]+)*-[a-zA-Z]*n' \
    -e '^[^#]*\$\{[A-Za-z_0-9@*]+(\[[^]]*\])?@[QEPAaKkUuL]\}' \
    -e '^[^#]*shopt[[:space:]]+-[su][[:space:]]+[^#]*(inherit_errexit|lastpipe|globstar|globskipdots|patsub_replacement|localvar_inherit|varredir_close|compat[0-9]+)[[:space:]]*(;|$)' \
    -e '^([^#"'"'"']|"[^"]*"|'"'"'[^'"'"']*'"'"')*([|]&|&>>)' \
    -e '^[^#]*;;&' \
    -e '^[^#]*coproc[[:space:]]' \
    -e '^[^#]*;&[[:space:]]*$' \
    -e '^[^#]*(EPOCHREALTIME|EPOCHSECONDS|SRANDOM|BASH_ARGV0|BASH_MONOSECONDS|BASH_TRAPSIG|GLOBSORT|BASHPID|BASH_COMPAT)' \
    -e '^[^#]*\$\{[[:space:]|]' \
    -e '^[^#]*(source[[:space:]]+-p[[:space:]]|trap[[:space:]]+-P)' \
    -e '^[^#]*read[[:space:]]+(-[a-zA-Z]+[[:space:]]+)*-t[[:space:]]+[0-9]*\.[0-9]' \
    -e '^[^#]*\{[0-9]+\.\.[0-9]+\.\.[0-9]+\}' \
    -e '^[^#]*\{0[0-9]+\.\.' \
    -e '^[^#]*[^$]\{[A-Za-z_][A-Za-z_0-9]*\}[<>]' \
    -e '^[^#]*\$\([[:space:]]*case[[:space:]]+[^[:space:]]+[[:space:]]+in[[:space:]]+[^(]' \
    -e '^[^#]*\$\{[A-Za-z_][A-Za-z_0-9]*//?[^/}]+/[^}]*&' \
    -- <"$tmp/unpinned" >"$tmp/hits" 2>"$tmp/err"
rc=$?
if [ -s "$tmp/err" ]; then
    cat "$tmp/err" >&2
    exit 74
fi
[ "$rc" -le 123 ] || exit 74
grep -v -E -e ':[0-9]+:@test ' -e 'BASH_VERSINFO' -e 'bash_version' -e '\$\{(EPOCHREALTIME|EPOCHSECONDS|SRANDOM|BASH_ARGV0|BASHPID)[:]?[-+]' <"$tmp/hits" >"$tmp/final"
if [ -s "$tmp/final" ]; then
    cat "$tmp/final"
    exit 1
fi
exit 0
