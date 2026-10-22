# readback-zsh.zsh MODE FILE CMD : install FILE as _CMD in a throwaway dir, register it the way MODE says, then read
# $_comps[CMD] back from the completion system. Exit 1 when it is empty.
#   autoload              dir on fpath BEFORE compinit
#   fpath-late            dir on fpath AFTER compinit
#   nofpath               dir on no fpath at all
#   source-after          FILE sourced after compinit (compdef exists)
#   source-before         FILE sourced before compinit (compdef does not exist yet)
#   guard-only            guard prefix `(( ! $+functions[compdef] )) && compinit -C`, then FILE, no later compinit
#   guard-then-compinit   the same prefix and FILE, then the user's own later compinit
#   compdef-then-compinit FILE sourced after compinit, then compinit runs again
# Run it once per zsh version (5.0.8 to 5.9).
emulate -L zsh
mode=$1
file=${2:A}
cmd=$3
[[ -n $mode && -f $file && -n $cmd ]] || {
    print -u2 'usage: readback-zsh.zsh MODE FILE CMD'
    return 64
}
T=$(mktemp -d "${TMPDIR:-/tmp}/rbz.XXXXXX")
mkdir -p $T/fn
cp $file $T/fn/_$cmd
export ZDOTDIR=$T HOME=$T
autoload -Uz compinit
out=
case $mode in
autoload)
    fpath=($T/fn $fpath)
    compinit -u -d $T/zcompdump
    ;;
fpath-late)
    compinit -u -d $T/zcompdump
    fpath=($T/fn $fpath)
    ;;
nofpath) compinit -u -d $T/zcompdump ;;
source-after)
    compinit -u -d $T/zcompdump
    out=$(source $file 2>&1 >/dev/null)
    source $file 2>/dev/null
    ;;
source-before)
    out=$(source $file 2>&1 >/dev/null)
    compinit -u -d $T/zcompdump
    ;;
guard-only | guard-then-compinit)
    if ((! $+functions[compdef])); then autoload -Uz compinit && compinit -C -d $T/zd1; fi
    source $file 2>/dev/null
    [[ $mode == guard-then-compinit ]] && compinit -u -d $T/zcompdump
    ;;
compdef-then-compinit)
    compinit -u -d $T/zcompdump
    source $file 2>/dev/null
    compinit -u -d $T/zcompdump2
    ;;
*)
    print -u2 "unknown mode $mode"
    rm -rf $T
    return 64
    ;;
esac
print -r -- "mode=$mode zsh=$ZSH_VERSION comps[$cmd]=[${_comps[$cmd]}] err=[${out//$'\n'/ }]"
rm -rf $T
[[ -n ${_comps[$cmd]} ]]
