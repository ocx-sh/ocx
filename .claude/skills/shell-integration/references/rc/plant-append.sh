#!/bin/sh
# PLANT: blind append with no guard and no newline repair, and no remove.
# usage: plant-append.sh apply|remove FILE BLOCKFILE
set -eu
cmd=$1
rc=$2
blk=$3
case $cmd in
    apply) {
        echo '# >>> demo >>>'
        cat "$blk"
        echo '# <<< demo <<<'
    } >>"$rc" ;;
    remove)
        echo "plant has no undo" >&2
        exit 1
        ;;
esac
