# shellcheck shell=bash
# PLANT: always an array, ours appended at the end
__x_hook() { echo "FIRED:ours"; }
PROMPT_COMMAND+=(__x_hook)
