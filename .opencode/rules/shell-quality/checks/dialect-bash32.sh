#!/bin/sh
# Kind: repo tool. Dialect: POSIX sh. Floor: POSIX.1-2017. Tested: dash 0.5.12-2, busybox ash 1.37, bash 3.2 --posix.
# Runs a bash script on the bash 3.2 image and judges the result (SH-DIAL-25, SH-DIAL-26, SH-DIAL-27).
# Usage: sh dialect-bash32.sh MODE SCRIPT [ARG...]
#   refuse  executed, the script exits 69, prints nothing on stdout and a line on stderr (a guarded bash 4+ script)
#   run     executed with ARG (a no-side-effect path such as --help), the script exits 0 (an unmarked script)
#   lib     sourced by a throwaway caller, the library returns 69 and the caller survives (a guarded library)
# Exit: 0 verdict met; 1 verdict failed, one FAIL line on stdout; 69 docker is missing or could not start the
# image; 78 bad usage or no such file. Needs docker and the bash:3.2 image.
set -u
mode=${1-}
script=${2-}
case $mode in
    refuse | run | lib) ;;
    *)
        echo "usage: sh dialect-bash32.sh refuse|run|lib SCRIPT [ARG...]" >&2
        exit 78
        ;;
esac
[ -f "$script" ] || {
    echo "dialect-bash32: no such file: $script" >&2
    exit 78
}
for t in docker mktemp dirname basename cmp wc head tr; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "dialect-bash32: '$t' not on PATH" >&2
        exit 69
    }
done
shift 2
dir=$(cd "$(dirname "$script")" && pwd) || exit 78
name=$(basename "$script")
tmp=$(mktemp -d "${TMPDIR:-/tmp}/dialect-b32.XXXXXX") || exit 74
trap 'rm -rf "$tmp"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "$mode" = lib ]; then
    docker run --rm -v "$dir:/w:ro" -w /w bash:3.2 bash -c '. "./$1"; printf "source-rc=%s\n" "$?"; echo caller-alive' caller "$name" >"$tmp/out" 2>"$tmp/err"
else
    docker run --rm -v "$dir:/w:ro" -w /w bash:3.2 bash "./$name" ${1+"$@"} >"$tmp/out" 2>"$tmp/err"
fi
rc=$?
if [ "$rc" -eq 125 ] || [ "$rc" -eq 126 ] || [ "$rc" -eq 127 ]; then
    cat "$tmp/err" >&2
    echo "dialect-bash32: docker could not run the bash:3.2 image" >&2
    exit 69
fi
case $mode in
    refuse)
        [ "$rc" -eq 69 ] && [ ! -s "$tmp/out" ] && [ -s "$tmp/err" ] && exit 0
        echo "FAIL: $name on bash 3.2 exits $rc, stdout $(wc -c <"$tmp/out") bytes, stderr $(wc -c <"$tmp/err") bytes (want 69, 0, more than 0)"
        ;;
    run)
        [ "$rc" -eq 0 ] && exit 0
        echo "FAIL: $name on bash 3.2 exits $rc (want 0): $(head -n 1 "$tmp/err")"
        ;;
    lib)
        printf 'source-rc=69\ncaller-alive\n' >"$tmp/want"
        cmp -s "$tmp/out" "$tmp/want" && exit 0
        echo "FAIL: sourcing $name on bash 3.2 gave: $(tr '\n' ' ' <"$tmp/out")(want source-rc=69 and caller-alive)"
        ;;
esac
exit 1
