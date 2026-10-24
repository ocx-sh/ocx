# shellcheck shell=bash
__x_hook() { echo "FIRED:ours"; }
PROMPT_COMMAND=__x_hook
