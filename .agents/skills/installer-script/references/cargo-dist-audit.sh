#!/bin/sh
# references/cargo-dist-audit.sh INSTALLER_SH [INSTALLER_PS1 [DIST_MANIFEST_JSON]]
# The static rows of the generated-installer checklist (cargo-dist 0.33.0 template, 2026-10-11), run on the RENDERED file.
# One line per row: "row N OK|FAIL|READ text". Rows 3 and 5 also need the behavioural kit (installer-kit.py tamper and trap).
# Exit map: 0 no FAIL, 1 a FAIL row, 69 a required tool is missing, 78 an operand is not a file.
set -u
for t in grep awk tail head wc; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "cargo-dist-audit: '$t' not on PATH" >&2
        exit 69
    }
done
[ $# -ge 1 ] && [ -f "$1" ] || {
    echo "usage: cargo-dist-audit.sh INSTALLER_SH [INSTALLER_PS1 [DIST_MANIFEST_JSON]]" >&2
    exit 78
}
sh_file=$1
bad=0
row() {
    printf 'row %s %s %s\n' "$1" "$2" "$3"
    [ "$2" = FAIL ] && bad=1
}

# 1 and 9: every curl call on every hop carries --proto (wget has no such flag: SH-INST-06's https assertion covers it)
miss=$(grep -n -E -e '^[[:space:]]*curl[[:space:]]' -- "$sh_file" | grep -v -e '--proto' | awk -F: '{printf "%s%s", sep, $1; sep=","}')
if [ -n "$miss" ]; then row 1 FAIL "curl call(s) without --proto at line(s) $miss"; else row 1 OK "every curl call carries --proto"; fi

# 2: digests embedded in the file (class pinned)
n=$(grep -c -E -e '[0-9a-f]{64}' -- "$sh_file")
if [ "$n" -gt 0 ]; then row 2 OK "$n line(s) carry a 64-hex digest (integrity class: pinned)"; else row 2 READ "no embedded digest: name the class (sidecar, manifest, none)"; fi

# 3: a skip message means the check fails open when the hash tool is missing (behavioural proof: installer-kit.py tamper)
skip=$(grep -n -E -e 'skipping .*checksum' -- "$sh_file" | awk -F: '{printf "%s%s", sep, $1; sep=","}')
if [ -n "$skip" ]; then row 3 FAIL "checksum verification is skipped when a tool is missing, at line(s) $skip"; else row 3 OK "no skip message (still run installer-kit.py tamper)"; fi

# 4: Rosetta test whose '||' binds looser than the pipe
ros=$(grep -n -E -e 'sysctl[^|]*\|\|[^|)]*\|' -- "$sh_file" | awk -F: '{printf "%s%s", sep, $1; sep=","}')
if [ -n "$ros" ]; then row 4 FAIL "sysctl test tests an exit status, not a value, at line(s) $ros"; else row 4 OK "no sysctl || pipe precedence"; fi

# 5: signals (behavioural proof: installer-kit.py trap)
if [ "$(grep -c -E -e '^[[:space:]]*trap[[:space:]]' -- "$sh_file")" -eq 0 ]; then row 5 FAIL "no trap: a signal leaks the temp directory"; else row 5 OK "a trap exists (still run installer-kit.py trap)"; fi

# 6: the last non-comment line is one guarded call
lastl=$(grep -v -E -e '^[[:space:]]*(#|$)' -- "$sh_file" | tail -n 1)
case $lastl in
    *'|| exit'* | 'main '* | 'main') row 6 OK "last statement: $lastl" ;;
    *) row 6 FAIL "last statement is not a guarded call: $lastl" ;;
esac

# 7: the rendered file parses on dash and BusyBox ash
parse7() {
    command -v "$1" >/dev/null 2>&1 || {
        row 7 FAIL "$* is not installed, so the parse was not checked"
        return
    }
    if LC_ALL=C "$@" -n "$sh_file" >/dev/null 2>&1; then row 7 OK "$* -n parses the file"; else row 7 FAIL "$* -n rejects the file"; fi
}
parse7 dash
parse7 busybox ash

# 8: the PowerShell installer hashes before it extracts
if [ -n "${2:-}" ]; then
    [ -f "$2" ] || {
        echo "cargo-dist-audit: not a file: $2" >&2
        exit 78
    }
    if [ "$(grep -c -E -e 'Get-FileHash|ComputeHash' -- "$2")" -eq 0 ]; then row 8 FAIL "no Get-FileHash or ComputeHash in $2"; else row 8 OK "a hash call exists (check its order with the PowerShell steps)"; fi
else
    row 8 READ "no PowerShell installer given"
fi

# 10: the generator version is pinned
if [ -n "${3:-}" ] && [ -f "${3:-}" ]; then
    v=$(grep -E -e '"dist_version"' -- "$3" | head -n 1)
    if [ -n "$v" ]; then row 10 OK "$v"; else row 10 FAIL "no dist_version in $3"; fi
else
    row 10 READ "no manifest given: pin the generator version in the release workflow"
fi
exit "$bad"
