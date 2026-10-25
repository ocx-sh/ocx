#compdef demo
# Twin: works both as an autoloaded _demo on fpath and when sourced after compinit.
_demo() {
    local -a c
    c=(sub help)
    _describe 'command' c
}
if [ "$funcstack[1]" = "_demo" ]; then
    _demo "$@"
else
    compdef _demo demo
fi
