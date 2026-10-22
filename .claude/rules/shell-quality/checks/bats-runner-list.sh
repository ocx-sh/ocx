#!/bin/sh
# bats-runner-list.sh DIR RUNNER_FILE [EXCLUDE_FILE] -- every .bats under DIR is run by a hand-listed runner or is excluded (SH-TEST-25).
# Output is the violation: "runner-list: PATH is not run by RUNNER_FILE and not excluded".
# A file counts as listed when the runner or the exclude file names its path, a path suffix or its basename in a
# non-comment line (a line starting with # or //), or names the directory that holds it (`bats tests` runs every
# file in tests, not those in a subdirectory). A word equal to the directory counts wherever it appears, so pass the file
# that invokes bats, not a workflow with a job named after the directory. Two files with one basename share one entry: list the paths.
# Exit: 0 clean; 1 a finding; 64 usage; 69 a required tool is missing; 74 the scan failed;
#       78 not a usable git work tree; 79 no .bats files under DIR.
set -u
[ "$#" -ge 2 ] && [ "$#" -le 3 ] || {
    echo "usage: bats-runner-list.sh DIR RUNNER_FILE [EXCLUDE_FILE]" >&2
    exit 64
}
dir=${1%/}
runner=$2
excl=${3:-}
for t in git grep awk tr mktemp; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "bats-runner-list: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "bats-runner-list: not a usable git work tree" >&2
    exit 78
}
for f in "$runner" ${excl:+"$excl"}; do
    [ -r "$f" ] || {
        echo "bats-runner-list: cannot read $f" >&2
        exit 74
    }
done
list=$(mktemp) || exit 74
tok=$(mktemp) || exit 74
dtok=$(mktemp) || exit 74
trap 'rm -f "$list" "$tok" "$dtok"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
git ls-files -z --cached --others --exclude-standard -- ":(glob)$dir/**/*.bats" | tr '\0' '\n' >"$list" || {
    echo "bats-runner-list: git ls-files failed" >&2
    exit 74
}
[ -s "$list" ] || {
    echo "bats-runner-list: no .bats files under $dir" >&2
    exit 79
}
for f in "$runner" ${excl:+"$excl"}; do
    awk '/^[[:space:]]*(#|\/\/)/ { next } { print }' "$f" | grep -o -E -e '[A-Za-z0-9_./-]+[.]bats' >>"$tok"
    awk '/^[[:space:]]*(#|\/\/)/ { next } { print }' "$f" | grep -o -E -e '[A-Za-z0-9_./-]+' >>"$dtok"
done
awk -v tokfile="$tok" -v dtokfile="$dtok" -v runner="$runner" '
  FILENAME == tokfile { sub(/^\.\//, ""); t[$0] = 1; next }
  FILENAME == dtokfile { sub(/^\.\//, ""); sub(/\/+$/, ""); if ($0 != "" && $0 !~ /[.]bats$/) d[$0] = 1; next }
  {
    n = split($0, p, "/")
    hit = ($0 in t) || (p[n] in t)
    dir = substr($0, 1, length($0) - length(p[n]) - 1)
    while (dir != "" && !hit) {
      if (dir in d) hit = 1
      i = index(dir, "/")
      dir = i ? substr(dir, i + 1) : ""
    }
    for (k in t) if (index(k, "/") && length($0) >= length(k) && substr($0, length($0) - length(k) + 1) == k) hit = 1
    if (!hit) { print "runner-list: " $0 " is not run by " runner " and not excluded"; bad = 1 }
  }
  END { exit bad }' "$tok" "$dtok" "$list"
