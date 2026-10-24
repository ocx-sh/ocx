# Replacement forms

You loaded this file because the baseline or the denylist named a construct and you need the form that runs on the whole matrix. Every form in the first two tables printed the same output on bash 3.2, 4.0, 4.4 and 5.3 (measured 2026-10-11, bash 3.2.57 in the `bash:3.2` image, so Alpine userland). Any macOS or BSD userland claim stays `unverified: read only`.

Contents: [Bash 4 and 5 constructs to the 3.2 floor](#bash-4-and-5-constructs-to-the-32-floor) ·
[The keyed lookup helper](#the-keyed-lookup-helper) ·
[Bash constructs to POSIX sh](#bash-constructs-to-posix-sh) ·
[What bash 3.2.57 prints](#what-bash-3257-prints)

## Bash 4 and 5 constructs to the 3.2 floor

Fix one class per hunk. Parse-time constructs first, because the whole file dies at the line (rule SH-DIAL-06, SH-DIAL-27).

| Construct (first bash that has it) | Form that runs on 3.2 | Note |
|---|---|---|
| `\|&` (4.0), `&>>` (4.0) | `2>&1 \|` and `>> file 2>&1` | parse error on 3.2, the script exits 2 at that line |
| `;;&` and `;&` (4.0) | repeat the arm, or separate `if` tests | parse error on 3.2 |
| `[[ -v x ]]` (4.2) | `[ -n "${x+set}" ]` | set but empty counts as set, same as `-v` |
| unparenthesised pattern in `$(case ...)` (4.0) | write the pattern as `(a)` | parse error on 3.2 |
| `coproc` (4.0) | a temp file or `mkfifo` under a `mktemp -d` directory with an EXIT trap | parse error on 3.2 |
| `declare -A`, `local -A`, `m[$k]=v` (4.0) | two indexed arrays, see the lookup helper below | 3.2 prints an error and builds an indexed array, so the run continues with wrong data |
| `mapfile -t a < <(cmd)`, `readarray` (4.0) | `s=$(cmd) \|\| fail "..."` then a `while IFS= read -r l` loop that appends with `a+=("$l")` fed by a here-document | the capture gives `-e` a status to see (SH-ERR-10). Guard an empty `s` with `[ -n "$s" ]`, or the loop appends one empty element. Trailing blank lines are lost |
| `${x,,}`, `${x^^}`, `declare -l`, `declare -u` (4.0) | `$(printf '%s' "$x" \| tr '[:upper:]' '[:lower:]')` and the reverse | first letter only: `cut -c1` and `cut -c2-` around the same `tr` |
| `{01..03}`, `{1..10..2}` (4.0) | a `while [ "$i" -le 3 ]` loop with `printf '%02d'` | silent on 3.2, `echo {01..03}` prints `1 2 3` |
| `read -t 0.5` (4.0) | `read -t 1` | 3.2 takes whole seconds only |
| `shopt -s globstar`, `lastpipe`, `inherit_errexit` | drop it, or probe: `shopt -s inherit_errexit 2>/dev/null \|\| :` | a bare unknown `shopt -s` ends a `set -e` script on 3.2 (SH-DIAL-28). Never `BASH_COMPAT` |
| `${a[-1]}` (4.2), `${s:0:-2}` (4.2) | `${a[${#a[@]}-1]}`, `${s%??}` or `${s:0:${#s}-2}` | `a: bad array subscript` on 3.2 |
| `printf '%(%s)T'` (4.2), `EPOCHSECONDS`, `EPOCHREALTIME` (5.0) | `date +%s` | whole seconds. Sub-second `%N` is GNU only (SH-USER-01), so name the lost precision in the receipt. `EPOCHREALTIME` is silently unset on 3.2 |
| `declare -g` (4.2) | a plain assignment in the function, with no `local` | `declare: -g: invalid option` on 3.2 |
| `local -n`, `declare -n` (4.3) | read with `${!name}` and write with `printf -v "$name" '%s' "$v"` | `local: -n: invalid option` on 3.2 |
| `wait -n` (4.3) | collect pids and `wait "$pid"` for each, reading each status | |
| `${x@Q}` (4.4) | `printf '%q' "$x"` | the quoting style differs, so use it for bash re-input only |
| `${ cmd; }` and `${\| cmd; }` (5.3) | `$(cmd)` | `bad substitution` on every older bash |
| `$BASHPID`, `BASH_ARGV0`, `SRANDOM` | `$$` where the main shell is meant, otherwise no 3.2 form | silently unset on 3.2 |

Three rows that are not a missing construct but break a ported script on 3.2 or 4.x:

| Construct | Form | Note |
|---|---|---|
| `"${a[@]}"` under `set -u` on an empty array | `${a[@]+"${a[@]}"}` | `a[@]: unbound variable` on 3.2, 4.0 and 4.2 (SH-ERR-12). Write `"$@"`, never `"${@}"` |
| `local x=$(cmd)` | `local x` then `x=$(cmd)` on the next line | the builtin's status replaces the substitution's (SH-ERR-01) |
| `(( i++ ))` as a statement under `-e` | `i=$((i + 1))` | aborts on 4.2 to 5.3 and continues on 3.2 and 4.0 (SH-ERR-14) |

## The keyed lookup helper

The replacement for an associative array. It ran on 3.2 under `set -euo pipefail` with empty arrays.

```bash
keys=(alpha beta)
vals=(1 2)
lookup() { # key -> prints the value, returns 1 when absent
    local i
    for i in ${keys[@]+"${!keys[@]}"}; do
        if [[ ${keys[i]} == "$1" ]]; then
            printf '%s\n' "${vals[i]}"
            return 0
        fi
    done
    return 1
}
```

Append with `keys+=("$k")` and `vals+=("$v")`. An absent slot stays empty with `vals[i]=`. Call `lookup key || fail "..."` so `-e` has a status to see.

## Bash constructs to POSIX sh

For a `#!/bin/sh` file. ShellCheck with `-s sh` names each of these at default severity (0.11.0, measured 2026-10-11). Never flip the shebang to bash or change `Kind:` to make the code legal (SH-CORE-01).

| ShellCheck code | Construct | POSIX form |
|---|---|---|
| SC3030, SC3054 | arrays | positional parameters (`set --`, `shift`, `"$@"`, function-local) or a newline-separated string read with `while IFS= read -r` |
| SC3010 | `[[ ]]` | `[ ]` with every operand quoted, and `case` for pattern tests |
| SC3014 | `==` inside `[ ]` | `=` |
| SC3011 | here-string `<<<` | a here-document, or `printf '%s\n' "$x" \|` into the command. A pipeline body runs in a subshell, so its assignments are lost |
| SC3001 | process substitution | a temp file under `mktemp -d` with an EXIT trap, or a pipe |
| SC3003 | `$'..'` | `printf` (SH-DIAL-09) |
| SC3045 | `read -d` | `find ... -exec cmd {} +`, or `find ... -print0 \| xargs -0 -r cmd` (SH-DIAL-10) |
| SC3037 | `echo -n`, `echo -e` | `printf '%s\n'` (SH-DIAL-12) |
| SC2113 | `function f { }` | `f() { }` |
| SC3009 | brace expansion `{a,b}`, `{1..3}` | an explicit list or a `while` counter. dash and busybox ash keep the literal text, so the loop body runs once |
| SC3028 | `$RANDOM`, `$HOSTNAME`, `$OSTYPE` | `uname -s`, `hostname`. For random numbers, `awk` `srand` seeded from `$$` |
| SC3060 | `${x//a/b}` | `sed`, or `case` with `${x%%pat*}` and `${x#*pat}` splits |
| SC3057 | `${x:1:2}` | `printf '%s' "$x" \| cut -c2-3` |
| SC3046 | `source` | `.` |
| SC3043 | `local` | the reasoned directive and the probe of SH-DIAL-17, never a bare use |
| SC3040 | `set -o pipefail` | the one-line probe of SH-DIAL-07, never bare and never followed by `\|\| true` |

Run the result on dash 0.5.12-2, busybox ash 1.37 and bash 3.2 `--posix` (rule SH-DIAL-04). A twin file using the `case`, `[ ]`, `printf`, `sed`, `cut`, counter-loop and `f()` forms printed identical output on all three and on bash 5.3 (measured 2026-10-11). A second twin with the rest of the rows (positional parameters for arrays, here-document for `<<<`, a temp file for process substitution, `printf` for `$'..'` and `echo -n`, `find -print0 | xargs -0` for `read -d`, a counter for braces, `cut` and `sed` for substring and replace, `awk` `srand`, `.` for `source`, the pipefail probe) printed identical output on the same four legs, and its bash plant (`${x:1:2}`, `${x//c/Z}`, arrays, `<<<`, braces, `$'..'`) died on dash and busybox ash with `Syntax error: "(" unexpected` at the array line (measured 2026-10-11).

## What bash 3.2.57 prints

Read the first stderr line of the failing leg against this list. Texts copied from a run on 2026-10-11, bash 3.2.57. The exit status is 2 for a parse error and whatever the next command gives otherwise.

| Construct | First stderr line | Class |
|---|---|---|
| `&>>` | ``syntax error near unexpected token `>'`` | parse, file dies |
| `\|&` and `;;&` | ``syntax error near unexpected token `&'`` | parse, file dies |
| `case` in `$( )` | ``syntax error near unexpected token `;;'`` | parse, file dies |
| `[[ -v HOME ]]` | `conditional binary operator expected` | parse, file dies |
| `[ -v HOME ]` | `[: -v: unary operator expected` | run time, test is false |
| `declare -A` | `declare: -A: invalid option` | loud, then continues with an indexed array |
| `local -A`, `local -n` | `local: -A: invalid option` | loud, then continues |
| `mapfile`, `readarray` | `mapfile: command not found` | loud, then continues with an empty array |
| `${x,,}`, `${x^^}` | `${x,,}: bad substitution` | loud, the command fails |
| `${x@Q}` | `${x@Q}: bad substitution` | loud, the command fails |
| `${a[-1]}` | `a: bad array subscript` | loud, empty value |
| `${s:0:-2}` | `-2: substring expression < 0` | loud, empty value |
| `read -t 0.5` | `read: 0.5: invalid timeout specification` | loud, empty variable |
| `printf '%(%s)T'` | ``printf: `(': invalid format character`` | loud, no output |
| `declare -g` | `declare: -g: invalid option` | loud, variable stays unset |
| `shopt -s globstar` | `shopt: globstar: invalid shell option name` | loud, ends a `set -e` script |
| empty array under `set -u` | `a[@]: unbound variable` | run time, the script stops |
| `EPOCHREALTIME`, `BASHPID` | none | silent, the variable is empty |
| `{01..03}` | none, prints `1 2 3` | silent, wrong value |
