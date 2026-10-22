#!/bin/sh
# check-rc-edit.sh IMPL
# Apply-twice harness for an rc-block editor. IMPL is a POSIX sh script called as
#   IMPL apply|remove RCFILE BLOCKFILE
# Scenarios: a file with and without a final newline, an update that must replace
# the block, remove, a file re-saved with CRLF between two runs, a file born CRLF,
# the user's own bytes, a symlinked rc, and the mode of a 0644 rc.
# Output lines starting FAIL are the violations. Exit 0 = compliant, 1 = violation.
# RC_INPROC=1 runs IMPL sourced inside the interpreter that runs this script,
# so the same checker covers dash, busybox ash, bash --posix and zsh in sh mode.
set -u
IMPL=${1:?usage: check-rc-edit.sh IMPL}
# shellcheck disable=SC1090 # IMPL is a runtime path
run_impl() {
    if [ -n "${RC_INPROC-}" ]; then (
        set -- "$@"
        . "$IMPL"
    ); else sh "$IMPL" "$@"; fi
}
W=$(mktemp -d "${TMPDIR:-/tmp}/rc-check.XXXXXX") || exit 70
trap 'rm -rf "$W"' EXIT HUP INT TERM
printf 'export A=1\n' >"$W/blk"
printf 'export A=2\n' >"$W/blk2"
bad=0
fail() {
    echo "FAIL[$1]: $2"
    bad=1
}
begins() { grep -c -e '# >>>' "$1"; }
bytes() { od -An -v -tx1 "$1" | tr -s ' ' '\n' | grep -c -e "^$2\$" || true; }

for variant in nl nonl; do
    rc=$W/bashrc.$variant
    if [ $variant = nl ]; then printf 'alias ll=ls\n' >"$rc"; else printf 'alias ll=ls' >"$rc"; fi
    run_impl apply "$rc" "$W/blk" || fail $variant "apply exited non-zero"
    cp "$rc" "$W/once.$variant"
    run_impl apply "$rc" "$W/blk" || fail $variant "second apply exited non-zero"
    n=$(begins "$rc")
    [ "$n" -eq 1 ] || fail $variant "$n begin markers after two applies (want 1)"
    cmp -s "$rc" "$W/once.$variant" || fail $variant "second apply changed the file"
    grep -q -x -e 'alias ll=ls' "$rc" || fail $variant "user line 'alias ll=ls' was joined or lost"
    run_impl apply "$rc" "$W/blk2" >/dev/null 2>&1
    c=$(grep -c -e '^export A=' "$rc")
    [ "$c" -eq 1 ] || fail $variant "$c 'export A=' lines after an update (want 1)"
    run_impl remove "$rc" "$W/blk2" >/dev/null 2>&1 || fail $variant "remove unsupported or failed"
    [ "$(begins "$rc")" -eq 0 ] || fail $variant "marker survives remove"
    grep -q -x -e 'alias ll=ls' "$rc" || fail $variant "user line lost by remove"
done

# CRLF: re-saved between two runs
printf 'alias ll=ls\n' >"$W/rc1"
run_impl apply "$W/rc1" "$W/blk"
sed 's/$/\r/' "$W/rc1" >"$W/rc1.crlf" && mv "$W/rc1.crlf" "$W/rc1"
run_impl apply "$W/rc1" "$W/blk"
n=$(begins "$W/rc1")
[ "$n" -eq 1 ] || fail lf-then-crlf "$n managed blocks after the second apply (want 1)"
# CRLF: born CRLF, applied twice, then removed
printf 'alias ll=ls\r\n' >"$W/rc2"
run_impl apply "$W/rc2" "$W/blk"
run_impl apply "$W/rc2" "$W/blk"
n=$(begins "$W/rc2")
[ "$n" -eq 1 ] || fail born-crlf "$n managed blocks after two applies (want 1)"
run_impl remove "$W/rc2" "$W/blk"
[ "$(begins "$W/rc2")" -eq 0 ] || fail born-crlf "marker survives remove"
grep -q -e 'alias ll=ls' "$W/rc2" || fail born-crlf "user line lost by remove"
# CRLF: the block follows the file's line ending. Bytes are counted with od, never
# with grep, sed or awk, which read in text mode under Git Bash and cannot see the CR.
printf 'alias ll=ls\r\n' >"$W/rc4"
run_impl apply "$W/rc4" "$W/blk"
ncr=$(bytes "$W/rc4" 0d)
nlf=$(bytes "$W/rc4" 0a)
[ "$ncr" -eq "$nlf" ] || fail born-crlf "$ncr CR bytes against $nlf LF bytes (mixed line endings)"
# every byte the user wrote survives the edit
printf 'alias ll=ls\r\nalias la=ls\r\n' >"$W/rc5"
cp "$W/rc5" "$W/rc5.orig"
run_impl apply "$W/rc5" "$W/blk"
n5=$(wc -c <"$W/rc5.orig" | tr -d ' ')
cmp -s -n "$n5" "$W/rc5.orig" "$W/rc5" || fail byte-prefix "the user's first $n5 bytes changed (line endings rewritten)"

# symlinked rc (the stow and chezmoi shape) and mode
mkdir "$W/h" "$W/dotfiles"
printf 'alias ll=ls\n' >"$W/dotfiles/bashrc"
chmod 644 "$W/dotfiles/bashrc"
ln -s ../dotfiles/bashrc "$W/h/.bashrc"
symok=0
[ -L "$W/h/.bashrc" ] && symok=1
# shellcheck disable=SC2012 # no portable stat for the mode string
mode() { ls -ld "$1" | cut -c1-10; }
before=$(mode "$W/dotfiles/bashrc")
run_impl apply "$W/h/.bashrc" "$W/blk"
if [ "$symok" -eq 0 ]; then
    # Git Bash without the symlink privilege copies the file or refuses: the scenario cannot be built
    fail symlink "INVALID: ln -s makes no symlink on this host (Git Bash needs MSYS=winsymlinks:nativestrict and the privilege), the symlink scenario did not run"
else
    [ -L "$W/h/.bashrc" ] || fail symlink "the symlinked rc is no longer a symlink"
    [ "$(readlink "$W/h/.bashrc")" = ../dotfiles/bashrc ] || fail symlink "link target changed"
    grep -q -e '^export A=1' "$W/dotfiles/bashrc" || fail symlink "block missing from the dotfiles target"
fi
after=$(mode "$W/dotfiles/bashrc")
[ "$before" = "$after" ] || fail mode "dotfiles target mode $before became $after"
printf 'alias ll=ls\n' >"$W/plain"
chmod 644 "$W/plain"
run_impl apply "$W/plain" "$W/blk"
m=$(mode "$W/plain")
[ "$m" = "-rw-r--r--" ] || fail mode "-rw-r--r-- became $m"

[ $bad -eq 0 ] && echo "OK: idempotent, newline-safe, CRLF-safe, replaceable, removable, symlink and mode kept"
exit $bad
