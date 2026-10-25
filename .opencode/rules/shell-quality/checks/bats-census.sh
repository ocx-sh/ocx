#!/bin/sh
# bats-census.sh TESTS_DIR TAP_FILE [STDERR_FILE] -- proves a green bats run ran what is on disk. Output is the violation.
# Run from the repo root after the bats step, which must have used --tap and kept its own exit status.
# Red when: the test count on disk differs from the TAP plan; the plan differs from the result lines or a test number repeats;
#   any "not ok"; a skip outside CENSUS_SKIP_OK; a "# bats warning" line; a BWnn or bats-gather-tests line on stderr;
#   a file that calls bats_test_function has no pinned count in the expect file or its pin differs from bats --count.
# Disk count: the static @test count over git ls-files, except a file that calls bats_test_function, where "bats --count FILE" replaces it.
# env BATS: the pinned bats executable (needed only when a file registers tests dynamically).
# env CENSUS_SKIP_OK: ERE of skip reasons this leg allows (default none: every skip is a finding).
# env CENSUS_EXCLUDE: extra git pathspecs, space separated, for .bats files that are test inputs, e.g. ":(exclude)test/fixtures".
# env CENSUS_EXPECT: file of "N path" lines, one per dynamic file (default TESTS_DIR/.bats-expected).
# Exit: 0 clean; 1 a finding; 64 usage; 69 a required tool is missing; 74 the scan failed;
#       78 not a usable git work tree; 79 no .bats files under TESTS_DIR.
set -u
[ "$#" -ge 2 ] && [ "$#" -le 3 ] || {
    echo "usage: bats-census.sh TESTS_DIR TAP_FILE [STDERR_FILE]" >&2
    exit 64
}
dir=$1
tap=$2
errfile=${3:-}
for t in git grep xargs awk sed mktemp sort uniq wc head; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "bats-census: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "bats-census: not a usable git work tree" >&2
    exit 78
}
[ -r "$tap" ] || {
    echo "bats-census: cannot read $tap" >&2
    exit 74
}
list=$(mktemp) || exit 74
out=$(mktemp) || exit 74
dyn=$(mktemp) || exit 74
trap 'rm -f "$list" "$out" "$dyn"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
# shellcheck disable=SC2086  # pathspecs are single words
git ls-files -z --cached --others --exclude-standard -- "$dir/*.bats" ${CENSUS_EXCLUDE:-} >"$list" || {
    echo "bats-census: git ls-files failed" >&2
    exit 74
}
[ -s "$list" ] || {
    echo "bats-census: no .bats files under $dir" >&2
    exit 79
}
xargs -0 -r grep -s -H -c -e '^[[:blank:]]*@test[[:blank:]]' -- <"$list" >"$out" || [ "$?" -le 123 ] || exit 74
disk=$(awk -F: '{ n += $NF } END { print n + 0 }' "$out")
xargs -0 -r grep -s -l -e '^[[:blank:]]*bats_test_function' -- <"$list" >"$dyn"
rc=0
expect=${CENSUS_EXPECT:-$dir/.bats-expected}
while IFS= read -r f; do
    [ -n "$f" ] || continue
    : "${BATS:?set BATS to the pinned bats executable for files that call bats_test_function}"
    got=$("$BATS" --count "$f" 2>/dev/null) || {
        echo "census: '$BATS --count $f' failed (gather error?)"
        rc=1
        continue
    }
    mine=$(awk -F: -v f="$f" '$1 == f { print $NF }' "$out")
    disk=$((disk - ${mine:-0} + got))
    want=$(awk -v f="$f" '$2 == f { print $1 }' "$expect" 2>/dev/null)
    if [ -z "$want" ]; then
        echo "census: $f registers tests dynamically and has no pin in $expect"
        rc=1
    elif [ "$want" != "$got" ]; then
        echo "census: $f registers $got tests, pin says $want"
        rc=1
    fi
done <"$dyn"
plan=$(sed -n 's/^1\.\.\([0-9][0-9]*\)$/\1/p' "$tap" | head -n 1)
if [ "$disk" -eq 0 ] || [ "${plan:-x}" != "$disk" ]; then
    echo "census: $disk tests on disk but TAP plan is ${plan:-missing}"
    rc=1
fi
res=$(grep -c -e '^ok [0-9]' -e '^not ok [0-9]' "$tap")
dups=$(grep -o -e '^ok [0-9]*' -e '^not ok [0-9]*' "$tap" | awk '{ print $NF }' | sort | uniq -d | wc -l)
if [ "${plan:-x}" = "x" ] || [ "$res" != "$plan" ] || [ "$dups" -gt 0 ]; then
    echo "census: TAP plan ${plan:-missing}, $res result lines, $dups duplicated test numbers"
    rc=1
fi
grep -e '^# bats warning' "$tap" && rc=1
skips=$(grep -e '^ok [0-9]* .* # skip' "$tap")
if [ -n "$skips" ] && [ -n "${CENSUS_SKIP_OK:-}" ]; then
    skips=$(printf '%s\n' "$skips" | grep -v -E -e "# skip.*(${CENSUS_SKIP_OK})")
fi
if [ -n "$skips" ]; then
    printf '%s\n' "$skips"
    rc=1
fi
grep -e '^not ok ' "$tap" && rc=1
if [ -n "$errfile" ]; then
    # bats reports BW01 (run saw 126 or 127) and BW02 (flag without the version directive) on stderr and still exits 0
    grep -e '^BW[0-9][0-9]*:' "$errfile" && rc=1
    grep -e 'bats-gather-tests' "$errfile" && rc=1
fi
exit $rc
