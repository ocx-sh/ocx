#!/bin/sh
# ci-steps.sh MODE: walk the steps of GitHub Actions YAML and print one line per violating step.
# Modes: pipe (SH-CI-01), noshell (SH-CI-02), composite (SH-CI-06), long (SH-CI-22), ps51 (SH-CI-11).
# Files come from git: top-level workflow files, plus action.yml files for composite and ps51.
# Exit: 0 clean, 1 finding, 64 bad mode, 69 tool missing, 78 not a git work tree.
set -u
mode=${1:-}
case $mode in
    pipe | noshell | long) spec=wf ;;
    composite) spec=act ;;
    ps51) spec=both ;;
    *)
        echo "usage: ci-steps.sh pipe|noshell|composite|long|ps51" >&2
        exit 64
        ;;
esac
for t in git xargs awk; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "ci-steps: $t missing" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "ci-steps: not a git work tree" >&2
    exit 78
}
wf=':(glob).github/workflow[s]/*.yml
:(glob).github/workflow[s]/*.yaml'
act=':(glob)**/action.yml
:(glob)**/action.yaml'
case $spec in
    wf) paths=$wf ;;
    act) paths=$act ;;
    both) paths=$wf'
'$act ;;
esac
list() {
    printf '%s\n' "$paths" | tr '\n' '\0' | xargs -0 git ls-files -z --cached --others --exclude-standard --
}
# shellcheck disable=SC2016 # single quotes are deliberate: the $ is for awk, not the shell
out=$(list | xargs -0 -r awk -v mode="$mode" -v sq="'" '
function ind(s) { match(s, /^ */); return RLENGTH }
function report(n, msg) { held[++nheld] = cur ":" n ": " msg }
function body(l,   t) {
    nbody++
    t = tolower(l)
    if (l ~ /^[ \t]*#/) return
    if (l ~ /[^|] [|] [^|]/) pipeline = 1
    if (l ~ /pipefail/) pipefail = 1
    if (t ~ />>[ ]*[$]env:github_(env|path|output|state)/ || t ~ /(add|set)-content[^|]*github_(env|path|output|state)/) psbad = 1
    if (t ~ /out-file/ && t ~ /github_(env|path|output|state)/ && (t !~ /-encoding utf8/ || t !~ /-append/)) psbad = 1
    if (l ~ /^[ \t]*(trap|mapfile)[ \t]/ || l ~ /^[ \t]*declare -A/ || l ~ /^[ \t]*case .* in[ \t]*$/ || l ~ /^[ \t]*(function[ \t]+)?[A-Za-z_][A-Za-z0-9_]*[ \t]*\(\)/) structure = 1
}
function key(t, n,   v) {
    if (t ~ /^run:/) {
        hasrun = 1; runline = n; inrun = 1
        v = substr(t, 5); sub(/^[ \t]+/, "", v)
        if (v != "" && v !~ /^[|>][-+0-9]*[ \t]*(#.*)?$/) body(v)
    } else if (t ~ /^shell:/) {
        hasshell = 1
        v = substr(t, 7); sub(/^[ \t]+/, "", v); sub(/[ \t]+#.*$/, "", v); gsub("[\"" sq "]", "", v)
        shellval = v
    }
}
function endstep() {
    if (hasrun) {
        eff = hasshell ? shellval : defshell
        if (mode == "pipe" && !hasshell && pipeline && !pipefail) report(runline, "SH-CI-01 pipeline in a step with no shell: and no pipefail")
        if ((mode == "noshell" || mode == "composite") && !hasshell) report(runline, (mode == "composite" ? "SH-CI-06" : "SH-CI-02") " run step with no shell:")
        if (mode == "long" && (nbody >= 10 || structure)) report(runline, "SH-CI-22 run block of " nbody " lines" (structure ? " with a function, trap, case, mapfile or declare -A" : "") ": move it to a script file")
        if (mode == "ps51" && psbad && eff ~ /^powershell/) report(runline, "SH-CI-11 5.1 step writes a runner file with a banned writer")
    }
    instep = 0; inrun = 0
}
function newstep(n) { instep = 1; hasrun = hasshell = pipeline = pipefail = psbad = structure = 0; nbody = 0; shellval = ""; runline = n }
function finish(   i) {
    if (instep) endstep()
    if (!(hasdef && (mode == "pipe" || mode == "noshell"))) for (i = 1; i <= nheld; i++) print held[i]
    nheld = 0
}
FNR == 1 { if (NR > 1) finish(); cur = FILENAME; instep = inrun = hasdef = 0; defshell = ""; p1 = p2 = "" }
{
    line = $0; sub(/\r$/, "", line)
    if (line ~ /^[ \t]*$/) next
    if (line ~ /^[ \t]*#/ && !inrun) next
    n = ind(line)
    if (line ~ /^[ ]*shell:/ && p1 ~ /^[ ]*run:[ ]*$/ && p2 ~ /^[ ]*defaults:[ ]*$/) {
        hasdef = 1; defshell = line; sub(/^[ ]*shell:[ ]*/, "", defshell)
    }
    p2 = p1; p1 = line
    if (instep && n <= sd) endstep()
    if (inrun && n <= kd) inrun = 0
    if (inrun) { body(line); next }
    if (!instep && line ~ /^[ ]*-([ ]|$)/) {
        sd = n; match(line, /^[ ]*-[ ]*/); kd = RLENGTH
        if (kd == n + 1) kd = n + 2
        newstep(NR)
        key(substr(line, RLENGTH + 1), FNR)
        next
    }
    if (instep && n == kd) key(substr(line, kd + 1), FNR)
}
END { finish() }
')
[ -z "$out" ] && exit 0
printf '%s\n' "$out"
exit 1
