#!/bin/sh
# PLANT for CRLF: green on LF files, symlinks and modes, but awk rewrites the user lines in text mode.
# usage: rc-edit.sh apply|remove FILE BLOCKFILE
# Contract: one begin/end marker pair; replace in place when content differs; repair a missing
# trailing newline before appending; resolve a symlinked rc and write the target, not the link;
# keep the mode (cp -p of the target, then mv the copy back); exit 0 and write nothing when unchanged.
set -eu
BEGIN='# >>> demo managed block >>>'
END='# <<< demo managed block <<<'
cmd=${1:?}
rc=${2:?}
blk=${3:-/dev/null}

# resolve a symlink chain without readlink -f (absent on macOS < 12.3, read only)
resolve() {
    p=$1
    n=0
    while [ -L "$p" ]; do
        n=$((n + 1))
        [ "$n" -le 40 ] || {
            echo "rc-edit: symlink loop: $1" >&2
            exit 70
        }
        l=$(readlink "$p")
        case $l in /*) p=$l ;; *) p=$(dirname "$p")/$l ;; esac
    done
    printf '%s\n' "$p"
}
target=$(resolve "$rc")
[ -e "$target" ] || : >"$target"
dir=$(dirname "$target")
tmp=$(mktemp "$dir/.rc-edit.XXXXXX")
trap 'rm -f "$tmp"' EXIT HUP INT TERM
cp -p "$target" "$tmp" # same mode and owner as the original (POSIX cp -p)

# drop any existing block (exact marker lines), keeping every other byte
awk -v b="$BEGIN" -v e="$END" '
  $0 == b { skip = 1; next }
  skip && $0 == e { skip = 0; next }
  !skip { print }' "$target" >"$tmp.body"
trap 'rm -f "$tmp" "$tmp.body"' EXIT HUP INT TERM

case $cmd in
    apply)
        # awk adds a newline to a final unterminated line; that IS the newline repair
        {
            cat "$tmp.body"
            printf '%s\n' "$BEGIN"
            cat "$blk"
            printf '%s\n' "$END"
        } >"$tmp"
        ;;
    remove)
        cat "$tmp.body" >"$tmp"
        ;;
    *)
        echo "usage: rc-edit.sh apply|remove FILE BLOCKFILE" >&2
        exit 64
        ;;
esac
if cmp -s "$tmp" "$target"; then exit 0; fi # unchanged: no write, no mtime churn
mv -f "$tmp" "$target"
