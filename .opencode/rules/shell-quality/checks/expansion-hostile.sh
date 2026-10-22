#!/bin/sh
# expansion-hostile.sh: SH-EXP-19 and SH-EXP-23, the hostile-value run. Runs a command once per hostile value
# and judges the final state of its working directory, never its exit status.
# Usage: sh expansion-hostile.sh COMMAND [ARG...]     (COMMAND is on PATH or an absolute path)
#   Each value reaches COMMAND three ways, in a fresh empty directory: as the last argument, as one stdin line
#   and as HOSTILE in the environment. COMMAND must write nothing into its working directory on bad input.
# Exit: 0 no file appeared, 1 a file appeared (value, channel and names are printed), 69 mktemp missing,
#       78 no COMMAND given.
set -u
command -v mktemp >/dev/null 2>&1 || {
    echo "expansion-hostile: mktemp is missing" >&2
    exit 69
}
[ "$#" -gt 0 ] || {
    echo "usage: expansion-hostile.sh COMMAND [ARG...]" >&2
    exit 78
}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/expansion-hostile.XXXXXX") || exit 69
trap 'rm -rf -- "${scratch:?}"' EXIT
trap 'exit 1' HUP INT TERM
nl='
'
found=0
for n in 1 2 3 4 5 6 7; do
    # shellcheck disable=SC2016 # the hostile values are literal text
    case $n in
        1) v='a b' ;;
        2) v='-dash' ;;
        3) v='a[$(touch PWNED)]' ;;
        4) v='$(touch PWNED)' ;;
        5) v="it's" ;;
        6) v="a${nl}b" ;;
        7) v='*' ;;
    esac
    for channel in arg stdin env; do
        d=$scratch/$n-$channel
        mkdir "$d" || exit 69
        case $channel in
            arg) (cd "$d" && "$@" "$v") </dev/null >/dev/null 2>&1 ;;
            stdin) printf '%s\n' "$v" | (cd "$d" && "$@") >/dev/null 2>&1 ;;
            env) (cd "$d" && HOSTILE=$v "$@") </dev/null >/dev/null 2>&1 ;;
        esac
        if ! rmdir "$d" 2>/dev/null; then
            found=1
            printf 'HOSTILE: value [%s] via %s left:\n' "$v" "$channel"
            find "$d" -print
        fi
    done
done
exit "$found"
