#!/bin/sh
# testing-census-junit.sh TESTS_DIR REPORT_XML -- census over a bats junit report, for a leg that cannot keep a TAP plan.
# Make the report with "bats --report-formatter junit --output DIR" beside the stdout formatter, or "--formatter junit".
# Output is the violation. Red when: the report's tests= sum differs from "bats --count" summed over git ls-files;
# any failures or errors; skipped above CENSUS_MAX_SKIPPED (default 0).
# env BATS: the pinned bats executable (required).
# Exit: 0 clean; 1 a finding; 64 usage; 69 a required tool is missing; 74 the scan failed;
#       78 not a usable git work tree; 79 no .bats files under TESTS_DIR.
set -u
[ "$#" -eq 2 ] || {
    echo "usage: testing-census-junit.sh TESTS_DIR REPORT_XML" >&2
    exit 64
}
dir=$1
xml=$2
: "${BATS:?set BATS to the pinned bats executable}"
for t in git grep xargs awk sed tr; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "testing-census-junit: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "testing-census-junit: not a usable git work tree" >&2
    exit 78
}
[ -r "$xml" ] || {
    echo "testing-census-junit: cannot read $xml" >&2
    exit 74
}
files=$(git ls-files -z --cached --others --exclude-standard -- "$dir/*.bats" | tr '\0' '\n')
[ -n "$files" ] || {
    echo "testing-census-junit: no .bats files under $dir" >&2
    exit 79
}
disk=$(git ls-files -z --cached --others --exclude-standard -- "$dir/*.bats" | xargs -0 -r -n 1 "$BATS" --count | awk '{ n += $1 } END { print n + 0 }')
# one <testsuite ...> start tag per file
sums=$(grep -o -e '<testsuite [^>]*>' "$xml" | sed 's/.*tests="\([0-9]*\)".*failures="\([0-9]*\)".*errors="\([0-9]*\)".*skipped="\([0-9]*\)".*/\1 \2 \3 \4/' | awk '{ t += $1; f += $2; e += $3; s += $4 } END { print t + 0, f + 0, e + 0, s + 0 }')
# shellcheck disable=SC2086  # four integers, word splitting is the point
set -- $sums
rc=0
[ "$disk" -gt 0 ] && [ "$1" -eq "$disk" ] || {
    echo "census: $disk tests on disk (bats --count) but the report has tests=$1"
    rc=1
}
[ "$2" -eq 0 ] && [ "$3" -eq 0 ] || {
    echo "census: failures=$2 errors=$3"
    rc=1
}
[ "$4" -le "${CENSUS_MAX_SKIPPED:-0}" ] || {
    echo "census: skipped=$4 over budget ${CENSUS_MAX_SKIPPED:-0}"
    rc=1
}
exit $rc
