#!/bin/bash
# readback-nu.sh FILE KIND CMD -- NU-COMMAND... : interactive nu in a pty with a throwaway config. FILE is copied into
# the autoload directory. The user's config.nu first installs an external completer that answers `usercmd ` with
# `user-cand`. KIND = extern (read `scope externs` rows for CMD) or completer (ask the one completer slot about
# `usercmd` and about CMD). Exit 1 when the registration is absent: extern needs a row for CMD, completer needs the
# user's answer AND ours (`sub,help`) to survive. Run it once per nu version.
file=$1
kind=$2
cmd=$3
[ "$4" = "--" ] && [ -n "$5" ] || {
    echo "usage: readback-nu.sh FILE extern|completer CMD -- NU-COMMAND..." >&2
    exit 64
}
shift 4
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$(dirname "$file")" && pwd)/$(basename "$file")
W=$(mktemp -d "${TMPDIR:-/tmp}/rbn.XXXXXX")
trap 'rm -rf "$W"' EXIT
mkdir -p "$W/cfg/nushell/autoload"
cat >"$W/cfg/nushell/config.nu" <<'C'
$env.config.completions.external.enable = true
$env.config.completions.external.completer = {|spans| if $spans.0 == 'usercmd' { [user-cand] } }
C
cp "$src" "$W/cfg/nushell/autoload/x.nu"
case $kind in
    extern) printf '%s\n' "print (\"RES\" + \"ULT:externs=\" + (scope externs | where name == $cmd | length | into string))" >"$W/script" ;;
    completer)
        {
            printf '%s\n' "print (\"RES\" + \"ULT:user=\" + (do \$env.config.completions.external.completer [usercmd \"\"] | str join \",\"))"
            printf '%s\n' "print (\"RES\" + \"ULT:ours=\" + (do \$env.config.completions.external.completer [$cmd \"\"] | str join \",\"))"
        } >"$W/script"
        ;;
    *) exit 64 ;;
esac
out=$(env HOME="$W" TERM=xterm XDG_CONFIG_HOME="$W/cfg" XDG_DATA_HOME="$W/data" python3 -I "$here/ptydrive.py" "$W/script" -- "$@" 2>&1)
st=$(printf '%s\n' "$out" | grep -a -o -e 'RESULT:[a-z]*=[a-z,0-9-]*' | sort -u | tr '\n' ' ')
echo "file=$(basename "$file") kind=$kind $st"
case $kind in
    extern) printf '%s' "$st" | grep -q -e 'externs=[1-9]' ;;
    completer) printf '%s' "$st" | grep -q -e 'user=user-cand' && printf '%s' "$st" | grep -q -e 'ours=sub,help' ;;
esac
