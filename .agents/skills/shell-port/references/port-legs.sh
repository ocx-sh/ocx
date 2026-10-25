#!/bin/sh
# Kind: repo tool. Dialect: POSIX sh. Floor: POSIX.1-2017. Tested: dash 0.5.12-2, busybox ash 1.37, bash 3.2 --posix.
# port-legs.sh FAMILY SCRIPT [ARG...]
# Runs SCRIPT once per leg of a port matrix in throwaway containers, prints each
# leg's output and exit status, then counts the distinct results.
#   FAMILY bash  bash 3.2 4.0 4.2 4.4 5.0 5.1 5.2 5.3 (VERSIONS overrides the list)
#   FAMILY sh    dash (debian:12), busybox ash (alpine:3.22), bash 3.2 --posix, bash 5.3
# The last leg is the newest bash. For FAMILY sh it is the oracle only, never evidence.
# PORT_SAVE=FILE   store the newest leg's result in FILE (run it on the unported script)
# PORT_ORACLE=FILE compare the newest leg's result with a stored one
# Exit: 0 one distinct result and the oracle (if given) matches, 1 results differ,
# 69 docker or an image is unavailable, 74 cannot create a temp dir, 78 bad usage.
set -u

fail() {
    printf 'port-legs: %s\n' "$1" >&2
    exit "$2"
}

[ "$#" -ge 2 ] || fail 'usage: port-legs.sh bash|sh SCRIPT [ARG...]' 78
family=$1
script=$2
shift 2
case $family in
    bash | sh) ;;
    *) fail "unknown family: $family" 78 ;;
esac
[ -f "$script" ] || fail "no such file: $script" 78
command -v docker >/dev/null 2>&1 || fail 'docker is not installed' 69
dir=$(cd "$(dirname "$script")" && pwd) || fail "cannot enter the script directory" 78
name=$(basename "$script")
out=$(mktemp -d "${TMPDIR:-/tmp}/port-legs.XXXXXX") || fail 'cannot create a temp dir' 74
trap 'rm -rf "$out"' EXIT
trap 'exit 143' TERM INT HUP

n=0
leg() { # label image interpreter... script args...
    label=$1
    image=$2
    shift 2
    docker image inspect "$image" >/dev/null 2>&1 || docker pull --quiet "$image" >/dev/null || fail "cannot pull $image" 69
    n=$((n + 1))
    {
        printf '== %s\n' "$label"
        docker run --rm -v "$dir:/w:ro" -w /w "$image" "$@" </dev/null 2>&1
        printf -- '-> exit %s\n' "$?"
    } >"$out/$n"
}

case $family in
    bash)
        for v in ${VERSIONS:-3.2 4.0 4.2 4.4 5.0 5.1 5.2 5.3}; do
            leg "bash $v" "bash:$v" bash "./$name" "$@"
        done
        ;;
    sh)
        leg 'dash (debian:12)' debian:12 dash "./$name" "$@"
        leg 'busybox ash (alpine:3.22)' alpine:3.22 sh "./$name" "$@"
        leg 'bash 3.2 --posix' bash:3.2 bash --posix "./$name" "$@"
        leg 'bash 5.3 (oracle)' bash:5.3 bash "./$name" "$@"
        ;;
esac

i=1
while [ "$i" -le "$n" ]; do
    cat "$out/$i"
    # the first line is the label, so only the result is compared
    tail -n +2 "$out/$i" >"$out/r$i"
    i=$((i + 1))
done

i=1
while [ "$i" -le "$n" ]; do
    cksum <"$out/r$i"
    i=$((i + 1))
done >"$out/sums"
distinct=$(LC_ALL=C sort -u "$out/sums" | awk 'END { print NR }')
printf 'distinct: %s of %s legs\n' "$distinct" "$n"
status=0
[ "$distinct" -eq 1 ] || status=1

if [ -n "${PORT_SAVE:-}" ]; then
    cp "$out/r$n" "$PORT_SAVE" || fail "cannot write $PORT_SAVE" 74
fi
if [ -n "${PORT_ORACLE:-}" ]; then
    if cmp -s "$out/r$n" "$PORT_ORACLE"; then
        printf 'oracle: match\n'
    else
        printf 'oracle: DIFFER\n'
        status=1
    fi
fi
exit "$status"
