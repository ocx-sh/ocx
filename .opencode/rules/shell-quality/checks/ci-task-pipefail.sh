#!/bin/sh
# ci-task-pipefail.sh MODE: Taskfile checks for pipefail.
# pipeline (SH-CI-32): a pipeline in a file that never names pipefail. shopt (SH-CI-17): shopt: lists pipefail.
# Files come from git: Taskfile*.yml, taskfile.yml and *.taskfile.yml outside testdata/. A Go-template pipe inside {{ }} is not a shell pipeline, including a template that spans lines.
# Exit: 0 clean, 1 finding, 64 bad mode, 69 tool missing, 78 not a git work tree.
set -u
mode=${1:-}
case $mode in
    pipeline | shopt) ;;
    *)
        echo "usage: ci-task-pipefail.sh pipeline|shopt" >&2
        exit 64
        ;;
esac
for t in git xargs awk; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "ci-task-pipefail: $t missing" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "ci-task-pipefail: not a git work tree" >&2
    exit 78
}
# shellcheck disable=SC2016 # single quotes are deliberate: the $ is for awk, not the shell
out=$(git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Taskfile*.yml' ':(glob)**/taskfile.yml' ':(glob)**/*.taskfile.yml' ':(exclude,glob)**/testdata/**' | xargs -0 -r awk -v mode="$mode" '
function finish(   i) {
    if (mode == "pipeline" && named) nheld = 0
    for (i = 1; i <= nheld; i++) print held[i]
    nheld = 0
}
FNR == 1 { if (NR > 1) finish(); cur = FILENAME; named = inlist = intpl = 0 }
{
    line = $0; sub(/\r$/, "", line)
    if (line ~ /pipefail/) named = 1
    if (mode == "pipeline") {
        t = ""
        rest = line
        while (rest != "") {
            if (intpl) {
                i = index(rest, "}}")
                if (!i) break
                rest = substr(rest, i + 2); intpl = 0
            } else {
                i = index(rest, "{{")
                if (!i) { t = t rest; break }
                t = t substr(rest, 1, i - 1); rest = substr(rest, i + 2); intpl = 1
            }
        }
        gsub(/[|][|]/, "OR", t)
        if (t ~ / [|] [a-z]/) held[++nheld] = cur ":" FNR ": SH-CI-32 pipeline in a file that never names pipefail: " line
        next
    }
    if (line ~ /^[ ]*shopt:[ ]*\[[^]]*pipefail/) { held[++nheld] = cur ":" FNR ": SH-CI-17 shopt cannot enable pipefail: use set:"; next }
    if (line ~ /^[ ]*shopt:[ ]*$/) { inlist = 1; at = FNR; next }
    if (inlist && line ~ /^[ ]*-[ ]*[A-Za-z]+[ ]*$/) {
        if (line ~ /pipefail/) held[++nheld] = cur ":" at ": SH-CI-17 shopt cannot enable pipefail: use set:"
        next
    }
    inlist = 0
}
END { finish() }
')
[ -z "$out" ] && exit 0
printf '%s\n' "$out"
exit 1
