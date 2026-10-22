#!/bin/bash
# readback-elvish.sh MODE FILE CMD -- ELVISH-COMMAND... : interactive elvish in a pty with a throwaway config.
#   rc-eval       rc.elv = eval (slurp < FILE)
#   rc-eval-try   rc.elv = try { eval (slurp < FILE) } catch e { }
#   lib-only      FILE placed at lib/CMD.elv, rc.elv empty (nothing autoloads lib/)
#   noop          rc.elv empty, nothing installed (control)
# Reads back whether $edit:completion:arg-completer has CMD and how many candidates it returns.
# Exit 1 unless the completer is registered AND returns candidates. Run it once per elvish version.
mode=$1
file=$2
cmd=$3
[ "$4" = "--" ] && [ -n "$5" ] || {
    echo "usage: readback-elvish.sh MODE FILE CMD -- ELVISH-COMMAND..." >&2
    exit 64
}
shift 4
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$(dirname "$file")" && pwd)/$(basename "$file")
H=$(mktemp -d "${TMPDIR:-/tmp}/rbe.XXXXXX")
trap 'rm -rf "$H"' EXIT
mkdir -p "$H/cfg/elvish/lib" "$H/data" "$H/state"
case $mode in
    rc-eval) echo "eval (slurp < $src)" >"$H/cfg/elvish/rc.elv" ;;
    rc-eval-try) echo "try { eval (slurp < $src) } catch e { }" >"$H/cfg/elvish/rc.elv" ;;
    lib-only)
        cp "$src" "$H/cfg/elvish/lib/$cmd.elv"
        : >"$H/cfg/elvish/rc.elv"
        ;;
    noop) : >"$H/cfg/elvish/rc.elv" ;;
    *) exit 64 ;;
esac
cat >"$H/script" <<S
echo 'STATE registered='(if (has-key \$edit:completion:arg-completer $cmd) { put yes } else { put no })
echo 'STATE candidates='(try { count [(\$edit:completion:arg-completer[$cmd] $cmd '')] } catch e { put 0 })
S
out=$(env HOME="$H" TERM=xterm XDG_CONFIG_HOME="$H/cfg" XDG_DATA_HOME="$H/data" XDG_STATE_HOME="$H/state" python3 -I "$here/ptydrive.py" "$H/script" -- "$@" 2>&1)
printf '%s\n' "$out" | grep -a -o -e 'STATE [a-z]*=[a-z0-9]*' | sort -u | tr '\n' ' '
echo "mode=$mode"
printf '%s\n' "$out" | grep -a -q -e 'STATE registered=yes' && ! printf '%s\n' "$out" | grep -a -q -e 'STATE candidates=0'
