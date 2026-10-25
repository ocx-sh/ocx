# Symptom text per interpreter and version

You are here because a pasted message does not match the route table in `SKILL.md`, or because the launcher that printed it is not the one the table names. Every row below was copied from a captured run on 2026-10-11 and carries the interpreter and its version. A variant with different text has its own row. Match the paste against the Text column, then return to the route table for the cause and the rule.

The script name and line number a launcher prefixes (`SCRIPT: line 7:`, `SCRIPT[7]:`, `SCRIPT:7:`) are left out below. They differ by launcher and carry no routing signal.

Contents: [Mode 100644](#mode-100644-on-a-file-with-a-shebang) · [CR in the shebang](#cr-after-the-shebang) · [CR in the body](#cr-in-the-body) · [Pipefail](#pipefail-in-binsh) · [Bash 3.2 constructs](#bash-32-constructs) · [Empty array under set -u](#empty-array-under-set--u) · [zsh](#zsh-unmatched-glob) · [PowerShell 5.1](#windows-powershell-51)

## Mode 100644 on a file with a shebang

The caller ran `DIR/f.sh` directly. Exit 126 on all 23 interpreter legs (dash 0.5.12, busybox ash 1.35 and 1.37, mksh R59c, ksh93u+m 2023 and 1.0.10, yash 2.52, posh 0.14.1, bash 3.2 to 5.3 and bash 3.2 and 5.2 `--posix`, zsh 5.0.8 to 5.9 and zsh 5.9 `--emulate sh`), captured 2026-10-11.

| Exit | Text | Launcher |
|---|---|---|
| 126 | `DIR/f.sh: Permission denied` | dash 0.5.12, busybox ash 1.35 and 1.37, bash 3.2 to 5.3 including `--posix` |
| 126 | `DIR/f.sh: can't execute: Permission denied` | mksh R59c |
| 126 | `DIR/f.sh: cannot execute [Permission denied]` | ksh93u+m 2023 and 1.0.10 |
| 126 | ``yash: cannot execute command `DIR/f.sh': Permission denied`` | yash 2.52 |
| 126 | `DIR/f.sh: cannot execute - Permission denied` | posh 0.14.1 |
| 126 | `permission denied: DIR/f.sh` | zsh 5.0.8, 5.3.1, 5.8, 5.9 and 5.9 `--emulate sh` (lower case, the path last) |

## CR after the shebang

A script whose first line is `#!/bin/sh` followed by a carriage return. The kernel looks for the interpreter `/bin/sh\r`, so a file that exists reports not found. The text is the launcher's, not the script's. Captured 2026-10-11.

| Exit | Text | Launcher |
|---|---|---|
| 127 | `crA.sh: not found` | dash 0.5.12, busybox ash 1.35 and 1.37, ksh93u+m 2023 and 1.0.10 |
| 126 | `crA.sh: /bin/sh^M: bad interpreter: No such file or directory` | bash 3.2, 4.0, 4.2, 4.4, 5.0, 5.1 and 5.3, and bash 3.2 `--posix` |
| 127 | `crA.sh: cannot execute: required file not found` | bash 5.2 and 5.2 `--posix` |
| 127 | `crA.sh: bad interpreter: /bin/sh^M: no such file or directory` | zsh 5.0.8, 5.3.1, 5.8, 5.9 and 5.9 `--emulate sh` |
| 126 | `crA.sh: No such file or directory` | mksh R59c |
| 127 | ``yash: cannot execute command `crA.sh': No such file or directory`` | yash 2.52 |
| 1 | `crA.sh: No such file or directory` | posh 0.14.1 |

The bash 5.2 row is the outlier. Bash 5.3 returned to the specific `bad interpreter` message (bash CHANGES, 5.3-alpha). `ubuntu-latest` moves to 26.04 by 2026-11-19 (SH-CI-15), so the text a CI log shows changes in that window.

## CR in the body

The shebang is clean and the CR sits in the body. The text depends on what `/bin/sh` is inside the image, not on the launcher. Captured 2026-10-11.

| Exit | Text | Where the CR sits | Image whose `/bin/sh` runs the script |
|---|---|---|---|
| 2 | `set: Illegal option -` | after `set -eu` | the Debian images (dash 0.5.12), which every launcher leg in the captured run used |
| 2 | `set: line 2: illegal option -` | after `set -eu` | alpine:3.22 (busybox ash 1.37) and the `bash:X` images, where `/bin/sh` is busybox |
| 2 | `Syntax error: word unexpected (expecting "do")` | after `do` | the Debian images (dash 0.5.12) |
| 2 | `syntax error: unexpected word (expecting "do")` with a `line 2:` prefix | after `do` | alpine:3.22 and the `bash:X` images |

## Pipefail in /bin/sh

Captured 2026-10-11 by `docker run` of the image, `dpkg-query -W -f` for the dash package.

| Exit | Text | Interpreter and package |
|---|---|---|
| 2 | `set: Illegal option -o pipefail` | dash 0.5.12-2 (Debian 12), 0.5.11+git20210903+057cd650a4ed-3build1 (Ubuntu 22.04), 0.5.12-6ubuntu5 (Ubuntu 24.04) |
| 1 | `set: pipefail: bad option` | posh 0.14.1 |
| 0 | no message | dash 0.5.12-12 (Debian 13), 0.5.12-12ubuntu3 (Ubuntu 26.04), busybox ash 1.35 and 1.37, mksh R59c, ksh93u+m 2023 and 1.0.10, yash 2.52, bash 3.2 and 5.2 `--posix`, zsh 5.9 `--emulate sh` |
| 2 | `set: Illegal option -o pipefail` after `set -o pipefail \|\| true` | dash 0.5.12-2 |
| 1 | `set: pipefail: bad option` after `set -o pipefail \|\| true` | posh 0.14.1 |

Dash gained `pipefail` in Debian `0.5.12-7` (2024-05-17). POSIX.1-2024 requires the option. Debian 12 and Ubuntu 24.04 keep rejecting it, so the probe the rule asks for stays needed while those images exist.

## Bash 3.2 constructs

Each row is one construct run on bash 3.2, 4.0, 4.2, 4.4 and 5.0 to 5.3 on 2026-10-11. The Text column is bash 3.2's. The last column says where the construct starts to work. Four failure shapes occur: fatal, non-fatal with exit 0, command not found that the script continues past, and silent drift with no message.

| Construct | Exit | Text on bash 3.2 | Works from |
|---|---|---|---|
| `declare -A m` then `m[a]=1` | 0 | `declare: -A: invalid option` and a `declare: usage:` line, then `m` is an indexed array | 4.0 |
| `mapfile -t a < FILE` | 0 | `mapfile: command not found`, the script goes on | 4.0 |
| `${x,,}` and `${x^^}` | 1 | `${x,,}: bad substitution` | 4.0 |
| `echo hi &>> FILE` | 2 | ``syntax error near unexpected token `>'`` | 4.0 |
| `case a in a) ;;& *) ;; esac` | 2 | ``syntax error near unexpected token `&'`` | 4.0 |
| `wait -n` | 0 | `wait: -n: invalid option` and a `wait: usage:` line | 4.4 (also fails on 4.0 and 4.2) |
| `printf '%(%s)T\n' -1` | 0 | ``printf: `(': invalid format character`` | 4.2 (also fails on 4.0) |
| `read -t 0.1 v` | 1 | `read: 0.1: invalid timeout specification` | 4.0 |
| `${x[-1]}` | 0 | `x: bad array subscript`, empty result | 4.2 (also fails on 4.0) |
| `[[ -v HOME ]]` | 2 | `conditional binary operator expected` and ``syntax error near `HOME'`` | 4.2 (also fails on 4.0) |
| `$EPOCHREALTIME` | 0 | no message, the variable is unset | 5.0 (unset on 4.4 too, so a timer reads 0) |

`local -A` fails the same way as `declare -A`, with `local: -A: invalid option`. `{1..3}` expands on every version and `{1..$i}` stays literal on every version.

## Empty array under set -u

An empty array expanded as `"${args[@]}"` under `set -u`. Captured 2026-10-11.

| Exit | Text | Bash |
|---|---|---|
| 1 | `line 5: args[@]: unbound variable` | 3.2, 4.0, 4.2 |
| 0 | prints `[]` | 4.4, 5.0, 5.1, 5.2, 5.3 |

The fix is in the 4.4 CHANGES file. The guarded expansion SH-ERR-12 names returns an empty argument list on all eight versions.

## zsh unmatched glob

`echo ./*.nomatch` run as `zsh -f -c` on zsh 5.0.8, 5.3.1, 5.8 and 5.9, captured 2026-10-11.

| Exit | Text |
|---|---|
| 1 | `zsh:1: no matches found: ./*.nomatch` |

A command with an unmatched glob operand is skipped with status 1. `echo`, a `for` word list and any function body abort the whole script. Bash passes the pattern through literally, which is why the same block passes there.

## Windows PowerShell 5.1

Windows PowerShell 5.1.26100 on the Windows host, captured 2026-10-11. PowerShell 7.4.7 and 7.6.6 parse and run the same files.

| Exit | Text | Cause |
|---|---|---|
| 1 | `The token '&&' is not a valid statement separator in this version.` (`InvalidEndOfLine`) | chain operator, 7 and later |
| 1 | `The token '\|\|' is not a valid statement separator in this version.` (`UnexpectedToken`) | chain operator, 7 and later |
| 1 | `Unexpected token '??' in expression or statement.` and `Unexpected token '?' in expression or statement.` | null-coalescing and ternary, 7 and later |
| 0 | no message, `len=5` and code points `0063 0061 0066 00C3 00A9` for the word cafe with an accent | a BOM-less UTF-8 file read as ANSI |

The same BOM-less file prints `len=4` and `00E9` on 7.4.7 and 7.6.6. The same file with a BOM prints `len=4` on all three engines.
