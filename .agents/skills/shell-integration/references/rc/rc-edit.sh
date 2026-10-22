#!/bin/sh
# Reference editor: byte-preserving marker-block editor, safe for CRLF files and for Git Bash, where awk, sed and grep read in text mode (the CR before LF is dropped on input, and awk and sed write LF only).
# Never rewrites user bytes through awk or sed: marker lines are located with grep -n -x -F (a CR-tolerant pattern pair), and the body is cut with head and tail, which copy bytes.
# The block is written with the file's own line ending. POSIX sh + grep/head/tail/cut/od/cp/mv/cmp/mktemp.
set -eu
BEGIN='# >>> demo managed block >>>'
END='# <<< demo managed block <<<'
cmd=${1:?}
rc=${2:?}
blk=${3:-/dev/null}
cr=$(printf '\r')
[ -e "$rc" ] || : >"$rc"
dir=$(dirname "$rc")
tmp=$(mktemp "$dir/.rc-edit.XXXXXX")
trap 'rm -f "$tmp" "$tmp.a" "$tmp.b"' EXIT HUP INT TERM
cp "$rc" "$tmp.a"
crp=''
[ "$(head -n 1 "$rc" | tail -c 2 | od -An -tx1 | tr -d ' \n')" = 0d0a ] && crp=$cr
# drop every block, one at a time, keeping every other byte
while :; do
    b=$(grep -n -x -F -e "$BEGIN" -e "$BEGIN$cr" "$tmp.a" | head -n 1 | cut -d: -f1 || true)
    [ -n "$b" ] || break
    e=$(grep -n -x -F -e "$END" -e "$END$cr" "$tmp.a" | cut -d: -f1 | while read -r n; do [ "$n" -gt "$b" ] && {
        echo "$n"
        break
    }; done || true)
    if [ -z "$e" ]; then e=$(wc -l <"$tmp.a" | tr -d ' '); fi
    {
        head -n $((b - 1)) "$tmp.a"
        tail -n +$((e + 1)) "$tmp.a"
    } >"$tmp.b"
    mv -f "$tmp.b" "$tmp.a"
done
# a missing final newline is repaired with the file's own line ending
if [ -s "$tmp.a" ] && [ "$(tail -c 1 "$tmp.a" | od -An -tx1 | tr -d ' \n')" != 0a ]; then printf '%s\n' "$crp" >>"$tmp.a"; fi
case $cmd in
    apply)
        {
            printf '%s%s\n' "$BEGIN" "$crp"
            while IFS= read -r l || [ -n "$l" ]; do printf '%s%s\n' "${l%"$cr"}" "$crp"; done <"$blk"
            printf '%s%s\n' "$END" "$crp"
        } >"$tmp.b"
        cat "$tmp.a" "$tmp.b" >"$tmp"
        ;;
    remove) cat "$tmp.a" >"$tmp" ;;
    *)
        echo "usage: rc-edit.sh apply|remove FILE BLOCKFILE" >&2
        exit 64
        ;;
esac
if cmp -s "$tmp" "$rc"; then exit 0; fi
cat "$tmp" >"$rc"
