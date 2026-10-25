#!/bin/bash
# lazy-dir.sh RELPATH FILE CMD -- BASH-COMMAND... : install FILE at $HOME/RELPATH in a throwaway HOME, start an
# interactive bash that loads bash-completion the way a distro skeleton does, and press TAB TAB after "CMD ".
# BASH-COMMAND is the bash to run, for example `bash` or `docker run --rm -it -v "$PWD:$PWD" -e HOME -e TERM IMAGE bash`
# (the image needs the bash-completion package, and the current directory must be mounted at the same path).
# Prints the matching lines. Exit 1 when the subcommand `sub` is not offered, which means the file was never read.
rel=$1
file=$2
cmd=$3
[ "$4" = "--" ] && [ -n "$5" ] || {
    echo "usage: lazy-dir.sh RELPATH FILE CMD -- BASH-COMMAND..." >&2
    exit 64
}
shift 4
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$(dirname "$file")" && pwd)/$(basename "$file")
H=$(mktemp -d "$PWD/home.XXXXXX")
trap 'rm -rf "$H"' EXIT
mkdir -p "$(dirname "$H/$rel")"
cp "$src" "$H/$rel"
printf '%s\n' '. /usr/share/bash-completion/bash_completion' >"$H/.bashrc"
printf '%s \t\techo "--- END"\n' "$cmd" >"$H/script"
out=$(HOME=$H TERM=xterm python3 -I "$here/ptydrive.py" "$H/script" -- "$@" --rcfile "$H/.bashrc" -i 2>&1)
printf '%s\n' "$out" | grep -a -e 'sub' -e 'END' | head -3 | cut -c1-160
printf '%s\n' "$out" | grep -a -q -e 'sub'
