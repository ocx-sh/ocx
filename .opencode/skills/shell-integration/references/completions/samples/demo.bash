# Twin: a complete bash completion script for the command `demo`.
_demo() {
    local cur=${COMP_WORDS[COMP_CWORD]} w
    COMPREPLY=()
    for w in sub help; do
        case $w in "$cur"*) COMPREPLY+=("$w") ;; esac
    done
}
complete -F _demo demo
