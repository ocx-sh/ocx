#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
# Run each named testcase of one libtest binary in a process of its own, as nextest does, all at once.
#   run_each_case.sh <test-binary> <case>...
# A case that is renamed or deleted runs zero tests and libtest still exits 0, so each case must
# report exactly one pass. Every case's output is passed through whole, in argument order: the
# unit-test floor reads its `test result:` lines.
set -eu

[ "$#" -ge 2 ] || {
    echo "usage: run_each_case.sh <test-binary> <case>..." >&2
    exit 2
}
binary=$1
shift

out=$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}/each.XXXXXX")
n=0
for case in "$@"; do
    n=$((n + 1))
    (
        code=0
        "$binary" --exact "$case" >"$out/$n.log" 2>&1 || code=$?
        echo "$code" >"$out/$n.code"
    ) &
done
wait

status=0
n=0
for case in "$@"; do
    n=$((n + 1))
    cat "$out/$n.log"
    if [ "$(cat "$out/$n.code")" -ne 0 ] || ! grep -q '^test result: ok\. 1 passed; 0 failed;' "$out/$n.log"; then
        echo "run_each_case: $case did not run exactly one passing test" >&2
        status=1
    fi
done
exit "$status"
