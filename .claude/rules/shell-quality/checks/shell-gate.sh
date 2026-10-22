#!/usr/bin/env bash
# shell-gate: the gate of record. Copy to scripts/shell-gate.sh and run from the repo root. Any non-zero exit fails the build.
# Never silence a failure (no true-fallback, no skip branch); never pass a severity flag; style lives in .editorconfig and .shellcheckrc.
# Exit: 0 clean; 1 a finding; 69 a required tool is missing; 74 git, shfmt or awk failed while reading files; 78 .shellcheckrc or .editorconfig is missing, or this is not a usable git work tree; 79 no shell files found.
# Kind: gate (output is the violation), so `set -u` and no `-e`: every fallible call below carries its own `|| exit` or `|| fail=1`.
# Floor: bash 3.2.57 (arrays, `<<<`, `${a[@]/#/./}` only). Measured 2026-10-11 on bash 3.2.57 to 5.3, Git Bash 5.2.15, ShellCheck 0.9.0 to 0.11.0, shfmt 3.12.0 to 3.14.1.
set -u -o pipefail

for tool in git xargs grep awk cut tr shellcheck shfmt; do # vd-ok: xargs here is a tool name, not an invocation
    command -v "$tool" >/dev/null 2>&1 || {
        echo "shell-gate: '$tool' not on PATH" >&2
        exit 69
    }
done

# 0. The work tree must be usable (not a repository, or git refuses it: safe.directory) and the profile must live in it:
#    ShellCheck falls back to ~/.shellcheckrc, so a missing file means CI and laptop disagree.
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "shell-gate: not a usable git work tree (not a repository, or git refuses it: safe.directory)" >&2
    exit 78
}
for cfg in .shellcheckrc .editorconfig; do
    [ -f "$cfg" ] || {
        echo "shell-gate: missing $cfg at the repo root" >&2
        exit 78
    }
done

# 1. Discover: tracked or new-and-unignored files that shfmt recognises by extension or shebang.
#    Plain -f only: on shfmt 3.13.1 the zero-argument form stops filtering explicit file arguments.
#    Exclusions are git pathspecs: ... --exclude-standard -- . ':(exclude)vendor' | ...
#    A tracked file that is gone or a dangling symlink is dropped first: shfmt -f stats every operand and dies on it.
# shellcheck disable=SC2016 # single quotes are deliberate: the $f is for the inner sh, not this shell
list=$(git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f --) || {
    echo "shell-gate: could not list the shell files (git or shfmt failed)" >&2
    exit 74
}
[ -n "$list" ] || {
    echo "shell-gate: no shell files found" >&2
    exit 79
}
all=() sc=() zs=() fail=0
while IFS= read -r f; do
    all+=("$f")
    IFS= read -r line1 <"$f" || line1=''
    case "$f $line1" in
        *.zsh\ * | *\ '#!'*zsh*) zs+=("$f") ;; # zsh is not a ShellCheck dialect and shfmt's zsh support is advisory
        *) sc+=("$f") ;;
    esac
done <<<"$list"

# 2. Format and lint at default severity.
if [ "${#sc[@]}" -gt 0 ]; then
    shfmt -d --apply-ignore -- "${sc[@]}" || fail=1
    shellcheck -- "${sc[@]}" || fail=1
fi
[ "${#zs[@]}" -eq 0 ] || shfmt -d --apply-ignore -- "${zs[@]}" >&2 || echo "shell-gate: advisory only: shfmt would reformat a zsh file" >&2

# 3. Hygiene: each check prints the offending file; empty output means pass.
#    CR and BOM are found with awk in binary mode: MSYS grep (Git Bash) reads in text mode and never sees the CR.
#    The ./ prefix keeps awk from reading a file named NAME=VALUE as an assignment.
hyg=$(LC_ALL=C awk -v BINMODE=3 '/\r/ && !cr[FILENAME]++ { print FILENAME ": CR byte (CRLF)" } FNR == 1 && /^\357\273\277/ { print FILENAME ": UTF-8 BOM" }' "${all[@]/#/./}") || {
    echo "shell-gate: awk failed while reading the shell files" >&2
    exit 74
}
[ -z "$hyg" ] || {
    printf '%s\n' "$hyg" >&2
    fail=1
}
for f in "${all[@]}"; do
    IFS= read -r line1 <"$f" || line1=''
    mode=$(git ls-files -s -- "$f" | cut -c1-6) || {
        echo "shell-gate: git ls-files failed for $f" >&2
        exit 74
    }
    case "$mode:$line1" in
        100644:'#!'*)
            echo "shell-gate: shebang but mode 100644: $f" >&2
            fail=1
            ;;
        100755:'#!'*) ;;
        100755:*)
            echo "shell-gate: mode 100755 but no shebang: $f" >&2
            fail=1
            ;;
    esac
    shebang=${line1#'#!'}
    shebang=${shebang#"${shebang%%[! ]*}"}
    case "$line1:$shebang" in '#!'*:'/bin/sh' | '#!'*:'/bin/sh '* | '#!'*:'/usr/bin/env sh' | '#!'*:'/usr/bin/env sh '*)
        LC_ALL=C grep -n -e '[^ -~[:space:]]' -- "$f" && {
            echo "shell-gate: non-ASCII in sh script $f" >&2
            fail=1
        }
        ;;
    esac
done

# 4. Parse-only gate for the families ShellCheck cannot read, only when the repo has files of that family.
#    A missing interpreter then fails the gate (69). PowerShell belongs to the powershell-quality gate.
# shellcheck disable=SC2016 # the single quotes are deliberate: $env is for nu, not for this shell
parse_family() { # INTERPRETER PATHSPEC COMMAND...: the file name is appended as the last argument
    local tool=$1 spec=$2 files f
    shift 2
    files=$(git ls-files -z --cached --others --exclude-standard -- "$spec" | tr '\0' '\n') || {
        echo "shell-gate: git ls-files failed for $spec" >&2
        exit 74
    }
    [ -n "$files" ] || return 0
    command -v "$tool" >/dev/null 2>&1 || {
        echo "shell-gate: '$tool' not on PATH but the repo has $spec files" >&2
        exit 69
    }
    while IFS= read -r f; do
        case $tool in
            nu) GATE_FILE=$f nu --no-config-file -c 'if not (nu-check $env.GATE_FILE) { exit 1 }' >/dev/null ;;
            *) "$@" "$f" >/dev/null ;;
        esac || {
            echo "shell-gate: $tool cannot parse $f" >&2
            fail=1
        }
    done <<<"$files"
}
parse_family zsh '*.zsh' zsh -fn --
parse_family fish '*.fish' fish --no-execute --
parse_family elvish '*.elv' elvish -compileonly --
parse_family nu '*.nu' nu
exit "$fail"
