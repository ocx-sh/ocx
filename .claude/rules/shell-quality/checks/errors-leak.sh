#!/usr/bin/env bash
# errors-leak.sh: SH-ERR-30, fail when a bash library leaves nullglob or failglob on for its caller.
# Usage: bash errors-leak.sh LIB FUNC [ARG...]
#   Sources LIB, calls FUNC in the current shell, then reports every glob shopt that is on.
#   A shopt set while LIB is sourced counts too (SH-ERR-29).
# Exit: 0 no glob shopt left on, 1 a leak, 64 usage or FUNC not defined, 79 LIB not found.
# Watched red and green on bash 3.2.57, 4.4 and 5.3 (measured 2026-10-11).
set -u

if [ "$#" -lt 2 ]; then
    echo 'usage: errors-leak.sh LIB FUNC [ARG...]' >&2
    exit 64
fi
lib=$1
func=$2
shift 2
if [ ! -f "$lib" ]; then
    echo "errors-leak: no such file: $lib" >&2
    exit 79
fi
# shellcheck disable=SC1090  # the library under test is a runtime path
. "$lib"
if ! declare -F "$func" >/dev/null; then
    echo "errors-leak: $func is not defined by $lib" >&2
    exit 64
fi
"$func" "$@" >/dev/null 2>&1 || :

leak=0
for opt in nullglob failglob; do
    if shopt -q "$opt"; then
        echo "LEAK: $opt is on after $func"
        leak=1
    fi
done
exit "$leak"
