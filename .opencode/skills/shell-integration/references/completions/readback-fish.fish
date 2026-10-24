# readback-fish.fish RELPATH FILE CMD WANT [stub] : install FILE (a generated fish completion script) at $HOME/RELPATH inside
# a throwaway HOME and XDG tree, then ask the completion engine in a fresh `fish -c` for "CMD " from an empty directory.
# With a fifth argument `stub` an executable CMD is put on PATH first (fish before 4.7 loads completions only for a command
# that resolves). Exit 1 when the candidate WANT (a subcommand name) is not offered. Run it once per fish version.
set -l rel $argv[1]
set -l file $argv[2]
set -l cmd $argv[3]
set -l want $argv[4]
set -l stub $argv[5]
test (count $argv) -ge 4; or begin
    echo 'usage: fish readback-fish.fish RELPATH FILE CMD WANT [stub]' >&2
    exit 64
end
set -l src (path resolve $file)
set -l h (mktemp -d)
set -gx HOME $h
set -gx XDG_CONFIG_HOME $h/.config
set -gx XDG_DATA_HOME $h/.local/share
set -e XDG_DATA_DIRS
mkdir -p (dirname $h/$rel) $h/w $h/bin
cp $src $h/$rel
if test "$stub" = stub
    printf '#!/bin/sh\n' >$h/bin/$cmd
    chmod +x $h/bin/$cmd
    set -gx PATH $h/bin $PATH
end
cd $h/w
set -l n (fish -c "complete -C '$cmd ' | string match -r '^$want\\t' | count")
set -l ver (string replace 'fish, version ' '' (fish --version))
echo "fish=$ver rel=$rel stub=$stub found-$want=$n"
cd /
rm -rf $h
test "$n" -gt 0
