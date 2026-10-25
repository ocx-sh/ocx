#!/bin/sh
# bats-lint.sh -- vacuous and disabled test forms in .bats files (SH-TEST-02 to 07, 10, 23, 24).
# Run from the repo root. Output is the violation, one line per finding: "Ln file:line: text" (L5 lists files).
#   L1 bare ! statement          L2 || true after an assertion   L3 [[ ]] or (( )) followed by another statement
#   L4 skip without a reason     L5 run flag, no version directive in the file or a loaded helper
#   L6 run whose result is never read (advisory on assertion-library suites)
#   L7 inexact failure status    L8 commented-out test           L9 skip as the first statement of a test
# Exit: 0 clean; 1 a finding; 64 usage; 69 a required tool is missing; 74 the scan failed;
#       78 not a usable git work tree; 79 no .bats files.
# shellcheck disable=SC2016  # single quotes are deliberate: the $ is for awk
set -u
[ "$#" -eq 0 ] || {
    echo "usage: bats-lint.sh (no arguments)" >&2
    exit 64
}
for t in git grep xargs awk sed mktemp dirname; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "bats-lint: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "bats-lint: not a usable git work tree" >&2
    exit 78
}
list=$(mktemp) || exit 74
out=$(mktemp) || exit 74
err=$(mktemp) || exit 74
hl=$(mktemp) || exit 74
trap 'rm -f "$list" "$out" "$err" "$hl"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
git ls-files -z --cached --others --exclude-standard -- '*.bats' >"$list" || {
    echo "bats-lint: git ls-files failed" >&2
    exit 74
}
[ -s "$list" ] || {
    echo "bats-lint: no .bats files" >&2
    exit 79
}
rc=0
verdict() { # status, label: any stderr, or a status above 123, means the scan is not trustworthy
    if [ -s "$err" ]; then
        cat "$err" >&2
        exit 74
    fi
    [ "$1" -le 123 ] || exit 74
    if [ -s "$out" ]; then
        sed "s/^/$2 /" "$out"
        rc=1
    fi
}
scan() { # label, grep arguments
    label=$1
    shift
    xargs -0 -r grep -s -H "$@" -- <"$list" >"$out" 2>"$err"
    verdict $? "$label"
}
awkscan() { # label, awk program
    prog=$2
    xargs -0 -r awk "$prog" <"$list" >"$out" 2>"$err"
    verdict $? "$1"
}
scan L1 -n -E -e '^[[:space:]]*![[:space:]]'
# L2, L3, L6 and L9 read statements, so they use awk. L2 blanks quoted strings first.
awkscan L2 '
  {
    s = $0
    gsub(/"([^"\\]|\\.)*"/, "Q", s)
    gsub(/'\''[^'\'']*'\''/, "Q", s)
    if (s ~ /^[[:space:]]*(\[\[?[[:space:]]|test[[:space:]]|e?grep[[:space:]]|diff[[:space:]]|cmp[[:space:]]|assert_[a-z_]+|refute_[a-z_]+|assert[[:space:]])[^|]*\|\|[[:space:]]*(true|:)[[:space:]]*($|[;&|)])/) print FILENAME ":" FNR ": " $0
  }'
awkscan L3 '
  FNR == 1 { cand = ""; intest = 0 }
  /^[[:space:]]*@test[[:space:]]/ { intest = 1; cand = ""; next }
  /^}/ { intest = 0; cand = ""; next }
  !intest || /^[[:space:]]*$/ || /^[[:space:]]*#/ { next }
  {
    if (cand != "") { print cand; cand = "" }
    if ($0 ~ /^[[:space:]]*(\[\[.*\]\]|\(\(.*\)\))[[:space:]]*$/) cand = FILENAME ":" FNR ": " $0
  }'
scan L4 -n -E -e '(^|[;&|])[[:space:]]*skip[[:space:]]*($|[;&|)])'
# L5: a file using a run flag (bats 1.5.0 and later) names the directive itself or in a loaded helper
xargs -0 -r grep -s -l -E -e '^[[:space:]]*run[[:space:]]+(-[0-9!]|--(separate-stderr|keep-empty-lines|output))' -- <"$list" >"$out" 2>"$err"
st=$?
if [ -s "$err" ] || [ "$st" -gt 123 ]; then
    cat "$err" >&2
    exit 74
fi
while IFS= read -r f; do
    grep -q -e bats_require_minimum_version -- "$f" && continue
    d=$(dirname -- "$f")
    sed -n 's/^[[:space:]]*load[[:space:]]\{1,\}["'"'"']\{0,1\}\([A-Za-z0-9_./-]\{1,\}\).*/\1/p' -- "$f" >"$hl"
    ok=0
    while IFS= read -r h; do
        for c in "$d/$h" "$d/$h.bash"; do
            [ -f "$c" ] && grep -q -e bats_require_minimum_version -- "$c" && ok=1
        done
    done <"$hl"
    [ "$ok" -eq 1 ] || {
        echo "L5 $f"
        rc=1
    }
done <"$out"
awkscan L6 '
  function flush() { if (open != "") print open; open = "" }
  FNR == 1 { flush() }
  /^[[:space:]]*@test[[:space:]]/ || /^}/ { flush(); next }
  /^[[:space:]]*run[[:space:]]/ {
    flush()
    if ($0 ~ /^[[:space:]]*run[[:space:]]+(--[a-z-]+[[:space:]]+)*(-[0-9]+|!)[[:space:]]/) next
    open = FILENAME ":" FNR ": " $0; next
  }
  /\$status|\$\{status\}|\$output|\$\{output|\$lines|\$\{lines|\$stderr|\$\{stderr|assert_[a-z_]+|refute_[a-z_]+/ { open = "" }
  END { flush() }'
scan L7 -n -E -e '\$\{?status\}?"?[[:space:]]+(-ne|-gt|!=)[[:space:]]+"?0' -e '^[[:space:]]*assert_failure[[:space:]]*($|[;&|])'
scan L8 -n -E -e '^[[:space:]]*#[[:space:]]*@test[[:space:]]+["'"'"'].*\{[[:space:]]*$'
awkscan L9 '
  FNR == 1 { first = 0 }
  /^[[:space:]]*@test[[:space:]]/ { first = 1; next }
  /^[[:space:]]*$/ || /^[[:space:]]*#/ { next }
  /^[[:space:]]*skip([[:space:]]|$)/ { if (first) print FILENAME ":" FNR ": " $0 }
  { first = 0 }'
exit $rc
