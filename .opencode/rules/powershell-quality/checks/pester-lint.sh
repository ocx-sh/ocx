#!/bin/sh
# pester-lint.sh -- Pester pin and removed-in-6 forms (SH-TEST-18, SH-TEST-20). Run from the repository root with no arguments.
# P1  Pester installed or imported without -RequiredVersion, or Invoke-Pester called (outside comments) in a repo that carries no
#     Pester version pin anywhere (a -RequiredVersion on a Pester line, a -MaximumVersion 5.9, a "Pester = 'x.y.z'" or "PesterVersion = 'x.y.z'"
#     table entry, a Pester manifest entry with a Version key), all matched case-insensitively. A pin in a build file counts for the whole repo.
# P2  constructs removed in Pester 6.0.0 (Assert-MockCalled, Assert-VerifiableMock, Set-ItResult -Pending, It -Focus) in every
#     *.Tests.ps1, matched case-insensitively because Pester also runs *.tests.ps1.
# Output is the violation: "Px file:line: text". Exit: 0 clean; 1 a finding; 64 usage; 69 a required tool is missing;
#       74 the scan failed (a sparse checkout with unmaterialised files is one); 78 not a usable git work tree;
#       79 no PowerShell, YAML or Taskfile files.
set -u
[ "$#" -eq 0 ] || {
    echo "usage: pester-lint.sh (no arguments)" >&2
    exit 64
}
for t in git grep xargs awk sed mktemp head; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "pester-lint: '$t' not on PATH" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "pester-lint: not a usable git work tree" >&2
    exit 78
}
list=$(mktemp) || exit 74
plist=$(mktemp) || exit 74
tlist=$(mktemp) || exit 74
out=$(mktemp) || exit 74
err=$(mktemp) || exit 74
trap 'rm -f "$list" "$plist" "$tlist" "$out" "$out.v" "$err"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.ps1' '*.psm1' >"$list" || {
    echo "pester-lint: git ls-files failed" >&2
    exit 74
}
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.ps1' '*.psm1' '*.psd1' >"$plist" || {
    echo "pester-lint: git ls-files failed" >&2
    exit 74
}
git ls-files -z --cached --others --exclude-standard -- ':(icase)*.tests.ps1' >"$tlist" || {
    echo "pester-lint: git ls-files failed" >&2
    exit 74
}
[ -s "$list" ] || {
    echo "pester-lint: no files in scope" >&2
    exit 79
}
rc=0
verdict() {
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
# P1: install or import lines without a version pin
xargs -0 -r grep -s -H -n -i -E -e '(Install-Module|Import-Module)[[:space:]]+(-Name[[:space:]]+)?Pester' -- <"$list" >"$out" 2>"$err"
st=$?
grep -v -e '-RequiredVersion' "$out" >"$out.v"
mv "$out.v" "$out"
verdict "$st" P1
# P1: Invoke-Pester called in a repo with no pin evidence anywhere
xargs -0 -r grep -s -h -i -E -e 'Pester.*-RequiredVersion' -e '-RequiredVersion.*Pester' -e "Pester[[:space:]]*=[[:space:]]*['\"][0-9]+[.][0-9]+" -e "Pester[A-Za-z]*Version[[:space:]]*=[[:space:]]*['\"][0-9]+[.][0-9]+" -e "-MaximumVersion[[:space:]]+['\"]?5[.]9" -- <"$plist" >"$out" 2>"$err"
st=$?
[ -s "$err" ] && {
    cat "$err" >&2
    exit 74
}
[ "$st" -le 123 ] || exit 74
if [ -s "$out" ]; then
    : >"$out"
else
    # shellcheck disable=SC2016 # single quotes are deliberate: the $ is awk's
    xargs -0 -r awk '
      FNR == 1 { cnt = 0 }
      FILENAME ~ /[.]psd1$/ && /Pester[^A-Za-z]*=[[:space:]]*@/ { cnt = 4 }
      FILENAME ~ /[.]psd1$/ && cnt > 0 { if ($0 ~ /Version[[:space:]]*=/) { print "pin"; exit } cnt-- }' <"$plist" >"$out" 2>"$err"
    st=$?
    [ -s "$err" ] && {
        cat "$err" >&2
        exit 74
    }
    [ "$st" -le 123 ] || exit 74
    if [ -s "$out" ]; then
        : >"$out"
    else
        # shellcheck disable=SC2016 # single quotes are deliberate: the $ is awk's
        xargs -0 -r awk '
          FNR == 1 { incomment = 0 }
          /<#/ { incomment = 1 }
          incomment { if ($0 ~ /#>/) incomment = 0; next }
          $0 ~ /^[[:space:]]*#/ { next }
          tolower($0) ~ /(^|[;|&(=]|run:)[[:space:]]*(&[[:space:]]+)?invoke-pester([[:space:]]|$)/ { print FILENAME ":" FNR ": no Pester version pin in the repo: " $0 }' <"$list" >"$out" 2>"$err"
        verdict $? P1
    fi
fi
# P2: removed in Pester 6.0.0
xargs -0 -r grep -s -H -n -E -e 'Assert-MockCalled|Assert-VerifiableMock|Set-ItResult[[:space:]]+-Pending|(^|[[:space:]])It[[:space:]].*-Focus' -- <"$tlist" >"$out" 2>"$err"
verdict $? P2
exit $rc
