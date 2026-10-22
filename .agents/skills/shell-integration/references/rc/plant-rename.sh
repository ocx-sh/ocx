#!/bin/sh
# PLANT: temp file in the parent directory (mode 0600), full rewrite, rename onto the path
# as given. This replaces a symlinked rc with a regular file and downgrades 0644 to 0600.
# usage: plant-rename.sh apply FILE BLOCKFILE
set -eu
rc=$2
blk=$3
tmp=$(mktemp "$(dirname "$rc")/.tmp.XXXXXX")
{
    cat "$rc"
    echo '# >>> demo >>>'
    cat "$blk"
    echo '# <<< demo <<<'
} >"$tmp"
mv -f "$tmp" "$rc"
