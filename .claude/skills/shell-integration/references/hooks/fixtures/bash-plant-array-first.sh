# shellcheck shell=bash
# PLANT: array form appends at index 1, which bash 3.2 to 5.0 never run; no bash 5.1 guard
__x_hook() {
    local s=$?
    echo "FIRED:ours"
    return "$s"
}
case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
    *__x_hook*) : ;;
    'declare -a'*) PROMPT_COMMAND+=(__x_hook) ;;
    *)
        __pc="${PROMPT_COMMAND-}"
        while :; do case "$__pc" in *";" | *" " | *"	") __pc="${__pc%?}" ;; *) break ;; esac done
        # shellcheck disable=SC2178 # the string form is the deliberate branch
        PROMPT_COMMAND="${__pc:+$__pc;}__x_hook"
        unset __pc
        ;;
esac
