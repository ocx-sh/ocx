#!/bin/sh
# checks/ps-enc-dash.sh: PS-ENC-11, an en or em dash used as a parameter prefix in a BOM-less PowerShell file.
# Usage: sh ps-enc-dash.sh [PATHSPEC...]   (run from the repository root; sits beside ps-enc-bytes.sh and runs its dash mode)
# Exit: as ps-enc-bytes.sh (0 clean, 1 a finding, 69, 74, 78, 79).
exec sh "$(dirname "$0")/ps-enc-bytes.sh" dash "$@"
