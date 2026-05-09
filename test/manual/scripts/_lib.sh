# Shared helpers for manual-testing scripts. Sourced from bootstrap.sh,
# teardown.sh, and any future scripts that wrap `ocx`. Provides:
#   - ANSI color palette (gated on $stdout being a tty)
#   - `ocx` wrapper that echoes each invocation before running the binary
#   - `ocx_step` / `ocx_warn` / `ocx_done` log helpers
#
# Callers must set `OCX_BIN` to an absolute path before invoking `ocx`,
# otherwise the wrapper falls back to a `PATH` lookup.

# shellcheck shell=bash

# ANSI colors when stdout is a tty.
if [[ -t 1 ]]; then
    OCX_C_STEP=$'\033[1;36m'   # bold cyan — section headers
    OCX_C_CMD=$'\033[0;90m'    # bright-black — echoed commands
    OCX_C_WARN=$'\033[1;33m'   # bold yellow
    OCX_C_DONE=$'\033[1;32m'   # bold green
    OCX_C_RESET=$'\033[0m'
else
    OCX_C_STEP=""
    OCX_C_CMD=""
    OCX_C_WARN=""
    OCX_C_DONE=""
    OCX_C_RESET=""
fi

# Quote a single argv element for human-readable echo.
ocx_quote() {
    case "$1" in
        '' | *[[:space:]\'\"\\\$\`\&\|\;\<\>\(\)\#\*\?]*) printf "'%s'" "${1//\'/\'\\\'\'}" ;;
        *) printf '%s' "$1" ;;
    esac
}

# Print `$ ocx <args>` (in $OCX_C_CMD) to stderr, then run the real binary.
ocx() {
    local out="" arg
    for arg in "$@"; do
        out+=" $(ocx_quote "$arg")"
    done
    printf '%s$ ocx%s%s\n' "$OCX_C_CMD" "$out" "$OCX_C_RESET" >&2
    "${OCX_BIN:-ocx}" "$@"
}

# Section header.
ocx_step() {
    printf '%s→ %s%s\n' "$OCX_C_STEP" "$*" "$OCX_C_RESET"
}

# Echo `$ cd <dir>` then chdir. Use inside a subshell so the cwd change
# does not leak back to the caller.
ocx_cd() {
    printf '%s$ cd %s%s\n' "$OCX_C_CMD" "$(ocx_quote "$1")" "$OCX_C_RESET" >&2
    cd "$1" || return
}

# Warning to stderr.
ocx_warn() {
    printf '%swarning: %s%s\n' "$OCX_C_WARN" "$*" "$OCX_C_RESET" >&2
}

# Success summary.
ocx_done() {
    printf '%s%s%s\n' "$OCX_C_DONE" "$*" "$OCX_C_RESET"
}
