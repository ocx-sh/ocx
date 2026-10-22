# shellcheck shell=bash
# TWIN: version-independent, idempotent, status-preserving PROMPT_COMMAND registration (bash 3.2-5.3).
__x_hook() {
    local s=$?
    echo "FIRED:ours"
    return "$s"
}
__x_install_prompt_hook() {
    local cur pc=" ${PROMPT_COMMAND[*]-} "
    pc=${pc//;/ }
    pc=${pc//$'\n'/ }
    case $pc in *" __x_hook "*) return 0 ;; esac
    if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
        # only element 0 runs on bash 3.2-5.0, every element on 5.1+: extend element 0 on both
        cur=${PROMPT_COMMAND[0]-}
        PROMPT_COMMAND[0]="${cur:+${cur%"${cur##*[![:space:];]}"};}__x_hook"
    else
        cur=${PROMPT_COMMAND-}
        # shellcheck disable=SC2178 # the string form is the deliberate branch
        PROMPT_COMMAND="${cur:+${cur%"${cur##*[![:space:];]}"};}__x_hook"
    fi
}
__x_install_prompt_hook
