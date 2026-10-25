#!/bin/sh
# errors-run.sh: run a script under each interpreter, in an empty directory, and compare its exit status (SH-ERR family).
# Usage: sh errors-run.sh [-q] WANT SCRIPT [INTERP...]
#   WANT    the exit status every interpreter must return
#   SCRIPT  the script under test (a probe that runs the construct, or the script itself)
#   INTERP  an interpreter command, quoted when it carries a flag, for example "busybox ash"
#   -q      also require an empty stdout, so diagnostics are shown to stay on stderr
# Without INTERP the POSIX shells found on PATH run: dash, bash, ksh, mksh, yash, posh and busybox ash.
# A mismatch prints one line, a pass prints nothing. Each run starts in a new empty directory, so a glob that
# matches nothing is the real case.
# Exit: 0 every interpreter matched, 1 a mismatch, 64 usage, 69 no interpreter found, 74 temp directory failed,
#       79 SCRIPT not found.
# Needs POSIX sh and mktemp. Watched red and green on dash, busybox ash 1.37 and bash 3.2 --posix (measured 2026-10-11).
set -u

quiet=0
if [ "${1-}" = -q ]; then
    quiet=1
    shift
fi
if [ "$#" -lt 2 ]; then
    echo 'usage: errors-run.sh [-q] WANT SCRIPT [INTERP...]' >&2
    exit 64
fi
want=$1
script=$2
shift 2
case $want in
    '' | *[!0-9]*)
        echo 'errors-run: WANT must be a number' >&2
        exit 64
        ;;
esac
if [ ! -f "$script" ]; then
    echo "errors-run: no such script: $script" >&2
    exit 79
fi
case $script in
    /*) ;;
    *) script=$PWD/$script ;;
esac

tmp=
# shellcheck disable=SC2329  # reached only by name through trap
on_exit() {
    rc=$?
    trap - EXIT
    [ -z "$tmp" ] || rm -rf "$tmp"
    exit "$rc"
}
trap on_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
tmp=$(mktemp -d) || exit 74
mkdir "$tmp/work" || exit 74

if [ "$#" -eq 0 ]; then
    for name in dash bash ksh mksh yash posh; do
        if command -v "$name" >/dev/null 2>&1; then set -- ${1+"$@"} "$name"; fi
    done
    if command -v busybox >/dev/null 2>&1; then set -- ${1+"$@"} 'busybox ash'; fi
fi

ran=0
bad=0
for interp in ${1+"$@"}; do
    first=${interp%% *}
    command -v "$first" >/dev/null 2>&1 || continue
    ran=$((ran + 1))
    # shellcheck disable=SC2086  # split: an interpreter entry may carry a flag
    (cd "$tmp/work" && $interp "$script") >"$tmp/out" 2>"$tmp/err"
    got=$?
    if [ "$got" -ne "$want" ]; then
        echo "errors-run: $interp exit $got, wanted $want"
        bad=1
    fi
    if [ "$quiet" -eq 1 ] && [ -s "$tmp/out" ]; then
        echo "errors-run: $interp wrote to stdout"
        bad=1
    fi
done
if [ "$ran" -eq 0 ]; then
    echo 'errors-run: no interpreter found' >&2
    exit 69
fi
exit "$bad"
