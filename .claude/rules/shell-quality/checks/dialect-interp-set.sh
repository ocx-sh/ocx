#!/bin/sh
# Kind: repo tool. Dialect: POSIX sh. Floor: POSIX.1-2017. Tested: dash 0.5.12-2, busybox ash 1.37, bash 3.2 --posix.
# Runs a #!/bin/sh script under the three interpreters that break sh code (SH-DIAL-04) and judges it:
# dash 0.5.12-2 (debian:12), busybox ash 1.37 (alpine:3.22) and bash 3.2 --posix (bash:3.2). Each must exit 0
# with the same stdout. Evidence from bash 5.x --posix, zsh sh mode or the host /bin/sh never counts.
# Usage: sh dialect-interp-set.sh SCRIPT [ARG...]
# Exit: 0 all three agree and exit 0; 1 one FAIL line per divergence on stdout; 69 docker is missing or an image
# could not start; 78 bad usage or no such file.
set -u
script=${1-}
[ -f "$script" ] || {
    echo "usage: sh dialect-interp-set.sh SCRIPT [ARG...]" >&2
    exit 78
}
for t in docker mktemp dirname basename cmp; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "dialect-interp-set: '$t' not on PATH" >&2
        exit 69
    }
done
shift
dir=$(cd "$(dirname "$script")" && pwd) || exit 78
name=$(basename "$script")
tmp=$(mktemp -d "${TMPDIR:-/tmp}/dialect-interp.XXXXXX") || exit 74
trap 'rm -rf "$tmp"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# run LABEL IMAGE INTERPRETER ARG... : stdout to $tmp/LABEL.out, stderr to LABEL.err, status to LABEL.rc
run() {
    label=$1
    image=$2
    interp=$3
    shift 3
    # shellcheck disable=SC2086 # interp is a fixed word list such as "bash --posix"
    docker run --rm -v "$dir:/w:ro" -w /w "$image" $interp "./$name" ${1+"$@"} >"$tmp/$label.out" 2>"$tmp/$label.err"
    echo $? >"$tmp/$label.rc"
}
run dash debian:12 dash ${1+"$@"}
run ash alpine:3.22 sh ${1+"$@"}
run bash32 bash:3.2 "bash --posix" ${1+"$@"}
for l in dash ash bash32; do
    read -r rc <"$tmp/$l.rc"
    if [ "$rc" -ge 125 ] && [ "$rc" -le 127 ]; then
        cat "$tmp/$l.err" >&2
        echo "dialect-interp-set: docker could not run the $l image" >&2
        exit 69
    fi
done
bad=0
for l in dash ash bash32; do
    read -r rc <"$tmp/$l.rc"
    if [ "$rc" -ne 0 ]; then
        echo "FAIL: $name under $l exits $rc: $(head -n 1 "$tmp/$l.err")"
        bad=1
    fi
done
for l in ash bash32; do
    if ! cmp -s "$tmp/dash.out" "$tmp/$l.out"; then
        echo "FAIL: $name prints different stdout under dash and $l"
        bad=1
    fi
done
exit "$bad"
