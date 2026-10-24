#!/bin/bash
# readback-bash.sh FILE CMD : source FILE (a generated bash completion script), then read the registration back from
# the live engine. (1) `complete -p CMD` must hold a spec, (2) the registered function, called the way readline calls
# it, must yield at least one candidate for "CMD <TAB>". Exit 1 when the spec is empty, sourcing printed an error,
# or the function yields nothing. Run it once per bash version (3.2 to 5.3).
f=$1
cmd=$2
[ -n "$f" ] && [ -n "$cmd" ] || {
    echo "usage: readback-bash.sh FILE CMD" >&2
    exit 64
}
e=$(mktemp "${TMPDIR:-/tmp}/rb.XXXXXX")
# shellcheck disable=SC1090
. "$f" 2>"$e" >/dev/null
spec=$(complete -p "$cmd" 2>/dev/null)
err=$(cat "$e")
rm -f "$e"
fn=$(printf '%s' "$spec" | sed -n 's/.* -F \([^ ]*\) .*/\1/p')
n=0
if [ -n "$fn" ]; then
    COMP_WORDS=("$cmd" "")
    COMP_CWORD=1
    COMP_LINE="$cmd "
    COMP_POINT=${#COMP_LINE}
    COMPREPLY=()
    "$fn" "$cmd" "" "$cmd" 2>/dev/null
    n=${#COMPREPLY[@]}
fi
echo "file=$f bash=${BASH_VERSION%%(*} fn=[$fn] candidates=$n err=[$err]"
[ -n "$spec" ] && [ -z "$err" ] && [ "$n" -gt 0 ]
