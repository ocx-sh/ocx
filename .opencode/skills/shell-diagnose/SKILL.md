---
name: shell-diagnose
description: 'Symptom-routed diagnosis of a shell script, CI step or PowerShell installer that is already failing, routing a failure on exit code, first stderr line and interpreter identity to the one measurement that names its cause. Covers exit 126 or 127 on a file that exists, a CR byte in a shebang, a pipefail abort on dash, bash 3.2 constructs, exit 141, 143, 269 or 397, an empty array under set -u, zsh unmatched globs, PowerShell 5.1 parse errors, exit under irm piped to iex, nushell removed commands, a cmd /c hang in Git Bash, and busybox wget flags. Use when someone reports "Permission denied", "bad interpreter", "required file not found", "Illegal option -o pipefail", "[[: not found", "bad substitution", "unbound variable", "no matches found", "The token ''&&'' is not a valid statement separator", "works locally but fails in CI" or an exit status above 128. Not for writing portable shell (shell-quality), a bash 3.2 port (shell-port), shell integration (shell-integration) or an installer (installer-script).'
license: Apache-2.0
metadata:
  summary: Routes a failing shell script, CI step or PowerShell installer by exit code, first stderr line and interpreter identity to the one measurement that names its cause, consent-gated, captured 2026-10-11 on dash 0.5.12, busybox 1.37, bash 3.2 to 5.3, zsh 5.0.8 to 5.9, nu 0.105.1 and 0.116.0, PowerShell 5.1, 7.4.7 and 7.6.6
  keywords: shell,diagnose,debugging,exit-code,126,127,bad-interpreter,crlf,pipefail,dash,bash,zsh,sigpipe,signal,powershell,nushell,git-bash,busybox,wget
---

# shell-diagnose

A shell script, CI step or PowerShell installer is already failing. This skill
routes the symptom to the measurement that names the cause. It carries the
procedure only. The standards live in the `shell-quality` and
`powershell-quality` rules, and this skill cites them by ID.

Every error text and status was captured on **2026-10-11** on dash 0.5.12,
busybox 1.37, bash 3.2 to 5.3, zsh 5.0.8 to 5.9, nu 0.105.1 and 0.116.0, Windows
PowerShell 5.1.26100, and PowerShell 7.4.7 and 7.6.6 (7.4 reaches end of
support 2026-11-10). macOS and BSD behaviour is `unverified: read only` and the
rows that touch it say so. Depth is in `references/`, read only when a step
points to it. The cells `mode`, `cr`, `pipefail-or` and `non-ascii` were run on
2026-10-11 over twelve third-party trees (plant red, twin green, hits read).

Contents: [Consent first](#consent-first) · [Stop condition](#stop-condition) ·
[Never edit the check](#never-edit-the-check) · [Step 0: interpreter identity](#step-0-interpreter-identity) ·
[Route by three facts](#route-by-three-facts) · [A. Exit 126, 127 or 2 on a file that exists](#a-exit-126-127-or-2-on-a-file-that-exists) ·
[B. Dialect](#b-dialect-pipefail-and-double-brackets) · [C. Bash 3.2](#c-bash-32-and-empty-arrays) ·
[D. Statuses above 128](#d-statuses-above-128) · [E. zsh](#e-zsh) ·
[F. PowerShell](#f-powershell) · [G. Nushell](#g-nushell) · [H. Hosts, CI and installers](#h-hosts-ci-and-installers) ·
[The receipt](#the-receipt) · [MUST rows](#must-rows-this-procedure-surfaces) ·
[What agents get wrong](#what-agents-get-wrong)

| Read | When |
|---|---|
| [references/symptom-text.md](references/symptom-text.md) | A pasted message that differs from the table: every captured variant per launcher and version, the 126 wording on 23 interpreters, the bash 3.2 construct texts |
| [references/signal-statuses.md](references/signal-statuses.md) | Any exit above 128, or a fix that compares a signal status to a literal |
| [references/nu-churn.md](references/nu-churn.md) | A nushell error id or warning, and what `nu-check` can and cannot see |
| [references/windows-and-ci.md](references/windows-and-ci.md) | `exit` under `irm \| iex`, a Git Bash hang, a Taskfile panic, a container-job step, BusyBox wget |

## Consent first

Touch only a process the operator has named as theirs to touch. Absent
consent, print the command and its cost from this table and stop.

| Action | Cost to the target |
|---|---|
| `kill -TERM` or `kill -KILL` on a named process | Ends it, and `KILL` destroys the evidence |
| Running the failing script or installer again | It can write to `HOME`, rc files and `/usr/local`. Run it in a throwaway container, or under `env -i` with a throwaway `HOME` (SH-CORE-16) |
| `docker run` of a version image | Pulls the image. The mount is read-only, so a script that writes to its directory must be copied first |

Anything run on a fixture, a copy or a throwaway container needs no consent.
Prefer those.

## Stop condition

Stop when all four hold, and do not keep measuring after.

1. The **root cause is named as a mechanism**. "The kernel looks for the
   interpreter `/bin/sh` followed by a CR byte" is a cause. "A bad shebang" is a
   category.
2. The **measurement that proved it is pasted**: the command, the exit status and
   the verbatim first stderr line.
3. Either the **fix was watched to clear the symptom** (the same measurement,
   before and after, both pasted), or the cause is a **named gap** with the
   interpreter version and the surface that would settle it.
4. Nothing outside the cause was changed to reach step 3.

## Never edit the check

Making the symptom disappear by changing the measurement is the failure this
skill exists to prevent. Each of these is a violation, not a fix.

| Edit | Why it is a violation | Rule |
|---|---|---|
| `\|\| true` after `set -o pipefail` | Dash 0.5.12-2 and posh abort on the option error before `\|\| true` runs | SH-DIAL-07 |
| `set +e`, `2>/dev/null` on a probe, or `\|\| true` on a command whose failure mattered | The failure is still there. Only the report is gone | SH-CORE-01 |
| A `# shellcheck disable=` with no same-line reason | The next reader cannot tell a verified exception from a silenced bug | SH-GATE-05 |
| Flipping `#!/bin/sh` to bash and leaving the `Kind:` header alone | The header and the shebang now disagree, and the dialect error moves | SH-DIAL-03 |
| `shell: bash` added to a step to hide a dialect error | The step passes on the runner and fails where bash is absent | SH-CORE-01 |
| A file-wide `setopt nonomatch` to clear a zsh `no matches found` | It changes every glob in the file | SH-ZSH-05 |
| `chcp 65001` or a console font change for garbled PowerShell 5.1 text | The file was decoded as ANSI before the console saw a byte | PS-ENC-01 |
| A `timeout` around a hanging `cmd /c` | The hang is now a failure. The cause is the path conversion | SH-CORE-01 |

If the only fix available is one of these, record it as a gap under stop
condition 3 and never apply it silently. Before reporting a fix, run the
diff-time check SH-CORE-01 names on the change. Its output is the violation. A
removed matrix leg, an added `shell: bash` and a flipped shebang are reading
items against the diff, so read the fix diff for them.

## Step 0: interpreter identity

Run before the first hypothesis, with the interpreter that failed and not the
one you are typing in. For a CI failure that means the job's image
(`docker run --rm IMAGE sh -c '...'`), because the same file behaves differently
on shells that share a name.

```sh
printf 'shell=%s\n' "${BASH_VERSION:+bash $BASH_VERSION}${ZSH_VERSION:+zsh $ZSH_VERSION}${KSH_VERSION:+ksh $KSH_VERSION}"
ls -l /bin/sh
# shellcheck disable=SC2016 # the dpkg-query format string, not a shell expansion
command -v dpkg-query >/dev/null 2>&1 && dpkg-query -W -f='dash pkg ${Version}\n' dash
uname -sm
# shellcheck disable=SC3040 # probed in a subshell first, the option may be rejected
if (set -o pipefail) 2>/dev/null; then echo "probe: pipefail accepted"; else echo "probe: pipefail rejected"; fi
```

Paste the output in the receipt and read it against the file's header, lines 1 to
8 (shebang, `# Kind:`, `# bash-floor:`, SH-CORE-04).

- `shell=` is empty on dash by design. The block never claims bash.
- The dash package decides pipefail, not the name. `0.5.12-2` (Debian 12),
  `0.5.11+git20210903+057cd650a4ed-3build1` (Ubuntu 22.04) and `0.5.12-6ubuntu5`
  (Ubuntu 24.04) reject it. `0.5.12-12` (Debian 13) and `0.5.12-12ubuntu3`
  (Ubuntu 26.04) accept it.
- On macOS `/bin/sh` is bash 3.2 in POSIX mode, so the dash line prints nothing
  (`unverified: read only`).
- Never branch on a `--version` banner (SH-USER-05). The probe line is the
  behaviour check.

## Route by three facts

Never route on the exit code alone (SH-CORE-05). 126, 127, 2 and 1 each have
four or more causes, and the text differs by launcher. Collect the exit status,
the first non-empty stderr line and the Step 0 identity. For a CI step the
status and line come from the job log.

```sh
./script.sh > stdout.txt 2> stderr.txt; echo "rc=$?"; head -n 3 stderr.txt
```

Rename the script. To reproduce on the interpreter that failed, use its image:
`bash:3.2` to `bash:5.3`, `debian:12` (dash 0.5.12-2), `debian:13`,
`ubuntu:24.04`, `alpine:3.22` (busybox ash 1.37), `zshusers/zsh:5.0.8` to
`zshusers/zsh:5.9`.

```sh
docker run --rm -v "$PWD":/w:ro -w /w bash:3.2 bash ./script.sh; echo "rc=$?"
```

| Status and shape | Go to |
|---|---|
| 126, 127, or 2 with `Illegal option -` or a `do` syntax error, on a file that exists | [A](#a-exit-126-127-or-2-on-a-file-that-exists) |
| `Illegal option -o pipefail`, `pipefail: bad option`, `[[: not found` | [B](#b-dialect-pipefail-and-double-brackets) |
| `declare: -A`, `bad substitution`, `mapfile: command not found`, `unbound variable`, `timer=0` | [C](#c-bash-32-and-empty-arrays) |
| 141, 143, 269, 271, 397, 399 | [D](#d-statuses-above-128) |
| `no matches found:` from zsh | [E](#e-zsh) |
| PowerShell: `not a valid statement separator`, garbled text, a session that closes | [F](#f-powershell) |
| A `nu::` error id | [G](#g-nushell) |
| A Git Bash hang, a go-task panic, a container step, a wget usage dump | [H](#h-hosts-ci-and-installers) |

## A. Exit 126, 127 or 2 on a file that exists

Test in this order, because each step costs less than the next and the first
two cause most of the cases. Do not read 127 as "interpreter missing" before all
three.

1. **Committed mode.** A file with a shebang at git mode 100644 gives 126 on all
   23 interpreters tried (captured 2026-10-11), with the wording varying by
   launcher. A fleet audit found 93 of 186 authored scripts at that mode.
2. **CR byte.** One command rules it in or out.
3. **Shebang path.** Read line 1, then list the interpreter in the image that
   failed.

Cells `mode` and `cr`. Output is the candidate list, and empty output is the pass.
Run from the repository root. Read each path: a data file in the `cr` list
(`.svg`, `.toml`) is not the failing script, and a sourced library that keeps a
shebang line shows in the `mode` list.

```sh
for t in git awk; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
git ls-files -s -- . | awk -F'\t' '$1 ~ /^100644 / { f = $2; if ((getline l < f) > 0 && l ~ /^#![ ]*[/]/) print f; close(f) }'
git ls-files --eol -- . | awk '$1 == "i/crlf" || $2 == "w/crlf" || $1 == "i/mixed" || $2 == "w/mixed"'
```

The `mode` cell reads the index, so it gives the same answer on WSL, Git Bash and
CI. A sourced library at 100644 with no shebang is correctly not listed. An
untracked file is not listed either, so read its mode with `ls -l -- "$F"`. For
the shebang step, set `F` to the failing script:

```sh
F=path/to/script.sh
head -n 1 -- "$F"
ls -l /bin/bash
```

| Exit | First line contains | Where | Cause and fix | Rule | Sev |
|---|---|---|---|---|---|
| 126 | `Permission denied` | all 23 interpreters, wording varies | Mode 100644 with a shebang. `git update-index --chmod=+x -- FILE` | SH-GATE-17 | SHOULD |
| 126 | `/bin/sh^M: bad interpreter: No such file or directory` | bash 3.2 to 5.1 and 5.3 as launcher | CR after the shebang. The kernel looks for the interpreter `/bin/sh` plus CR | SH-GATE-15 | MUST |
| 127 | `crA.sh: not found` | dash 0.5.12, busybox ash 1.35 and 1.37, ksh93u+m | same | SH-GATE-15 | MUST |
| 127 | `cannot execute: required file not found` | bash 5.2 as launcher (5.3 returned to `bad interpreter`) | same | SH-GATE-15 | MUST |
| 127 | `bad interpreter: /bin/sh^M: no such file or directory` | zsh 5.0.8 to 5.9 | same | SH-GATE-15 | MUST |
| 2 | `set: Illegal option -` (image `/bin/sh` is dash) or `set: line 2: illegal option -` (busybox ash 1.37) | CR after `set -eu` | The option string is `-eu` plus CR | SH-GATE-15 | MUST |
| 2 | `Syntax error: word unexpected (expecting "do")` | CR after `do` | `do` plus CR is a word, not the keyword | SH-GATE-15 | MUST |

mksh, yash and posh print `No such file or directory` with exit 126, 127 and 1.
Every variant is in [references/symptom-text.md](references/symptom-text.md).

Fix a CR by stripping it into a new file and moving that over the old one, then
make git keep it out. `sed -i 's/\r$//'` is not portable, because BSD sed needs a
suffix argument and does not read `\r`.

```sh
F=path/to/script.sh
tr -d '\r' < "$F" > "$F.new" && mv "$F.new" "$F"
```

Then put `* text=auto eol=lf` in `.gitattributes` (SH-GATE-14), and rerun the
`cr` cell on the fixed tree.

## B. Dialect, pipefail and double brackets

| Exit | First line contains | Interpreter | Cause and fix | Rule | Sev |
|---|---|---|---|---|---|
| 2 | `set: Illegal option -o pipefail` | dash 0.5.12-2, 0.5.12-6ubuntu5, 0.5.11+git20210903+057cd650a4ed-3build1 | `/bin/sh` is dash without pipefail, and an option error in a special builtin ends a non-interactive shell | SH-DIAL-07 | MUST |
| 1 | `set: pipefail: bad option` | posh 0.14.1 | same cause, different text and status | SH-DIAL-07 | MUST |
| 0 | no message | dash 0.5.12-12, 0.5.12-12ubuntu3, busybox ash, mksh, ksh93u+m, yash | Accepted. Dash got the option in Debian `0.5.12-7` (2024-05-17) | SH-DIAL-04 | MUST |
| 2 | same text after `set -o pipefail \|\| true` | dash 0.5.12-2 | `\|\| true` cannot catch a special-builtin error | SH-DIAL-07 | MUST |
| 0 | `[[: not found`, then the step goes on | dash in a `container:` job (`sh -e {0}`), debian:12 and ubuntu:24.04 | `[[` is a bash keyword, the condition is false, the step stays green | SH-CI-04 | MUST |
| 0 | no message, the `[[` branch runs | busybox ash 1.37 (alpine:3.22) | Ash accepts `[[`, so the step passes here and fails on another image | SH-CI-04 | MUST |

The answer flips with the dash package, so do not call dash "pipefail-less"
as a fact. `ubuntu-latest` moves to 26.04 by 2026-11-19 (SH-CI-15, SHOULD), and a
`shell: sh` step changes behaviour in that window. ShellCheck 0.11.0 finds it
statically with `shellcheck -s sh FILE` (SC3040 for the option, SC3010 for
`[[`). SH-DIAL-02 makes the body match the declared dialect.

Cell `pipefail-or`: shell files that follow pipefail with `||`. Output is the
violation.

```sh
for t in git xargs grep shfmt tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
files | xargs -0 -r grep -H -n -E -e 'pipefail.*[|][|]' --
```

For the container step, run the body the way the runner does and read the first
line:

```sh
docker run --rm -v "$PWD":/w:ro -w /w debian:12 sh -e ./step.sh; echo "rc=$?"
```

## C. Bash 3.2 and empty arrays

A green ShellCheck is no evidence here: it exits 0 before and after a bash 3.2
fix. The bash-4 constructs fail four ways on 3.2: fatal (`bad substitution`),
non-fatal with exit 0 (`declare -A` leaves an indexed array), command not found
that the script continues past (`mapfile`), and silent drift (`EPOCHREALTIME`
unset, so `timer=0`). A run that exits 0 proves nothing.

| Exit | First line contains | Bash | Cause and fix | Rule | Sev |
|---|---|---|---|---|---|
| 0 | `declare: -A: invalid option` | 3.2 | No associative arrays. Parallel indexed arrays or a `case` | SH-DIAL-06 | MUST |
| 0 | `mapfile: command not found` | 3.2 | No `mapfile` before 4.0. A `while IFS= read -r` loop | SH-DIAL-06 | MUST |
| 1 | `bad substitution` on `${x,,}` or `${x^^}` | 3.2 | Case modification is 4.0 | SH-DIAL-06 | MUST |
| 2 | `syntax error near unexpected token` for `&>>` or `;;&`, `conditional binary operator expected` for `[[ -v x ]]` | 3.2 (`[[ -v` also 4.0) | Parse errors, the whole file fails | SH-DIAL-06 | MUST |
| 1 | `args[@]: unbound variable` | 3.2, 4.0, 4.2 under `set -u` | An empty array expansion is unbound before 4.4 | SH-ERR-12 | MUST |
| 0 | `timer=0`, no message | 4.4 and older | `EPOCHREALTIME` is unset until 5.0 | SH-DIAL-06 | MUST |

The other construct texts (`wait -n`, `printf '%(%s)T'`, `read -t 0.1`, `${x[-1]}`)
and the version each starts to work are in
[references/symptom-text.md](references/symptom-text.md).

Reproduce on the floor, then on the newest bash, and compare the two outputs
byte for byte:

```sh
docker run --rm -v "$PWD":/w:ro -w /w bash:3.2 bash ./script.sh; echo "rc=$?"
docker run --rm -v "$PWD":/w:ro -w /w bash:5.3 bash ./script.sh; echo "rc=$?"
```

The denylist is SH-DIAL-06 and it is judged by output, because the last
`xargs` returns 123 on a clean tree. Whether to port or to pin the floor is
SH-DIAL-25 and the port itself is the `shell-port` skill. macOS `/bin/bash` is
3.2.57 (`unverified: read only`, the `bash:3.2` image stands in).

## D. Statuses above 128

141 is SIGPIPE, 143 is SIGTERM, and the numbers differ by shell: 269 and 271 on
ksh93u+m, 397 and 399 on yash. Test `-gt 128` and decode with `kill -l`, never a
literal (SH-ERR-19, SHOULD). The full table is in
[references/signal-statuses.md](references/signal-statuses.md).

```sh
rc=141   # the status the parent saw
if [ "$rc" -gt 128 ]; then kill -l "$rc"; fi
```

| Status | First line contains | Cause and fix | Rule | Sev |
|---|---|---|---|---|
| 141, 269, 397 | none | SIGPIPE. The producer lost its reader, usually `producer \| grep -q` or `\| head` under pipefail. Capture the output, then test it | SH-ERR-09 | MUST |
| 143, 271, 399 | `Terminated` or none | SIGTERM. A handler that exits with an explicit status | SH-ERR-16 | MUST |

## E. zsh

`no matches found: PATTERN` on zsh 5.0.8 to 5.9: an unmatched glob is an error
there, and bash passes it through literally. A command operand is skipped with
status 1, but `echo`, a `for` word list and any function body abort the script.

```sh
zsh -f -c 'echo ./*.nomatch'; echo "rc=$?"
```

Fix by quoting the glob that is an operand of another tool, or by `(N)` on an
optional glob, never by a file-wide `setopt nonomatch` (SH-ZSH-05, MUST). A
pasted command block must quote its globs (SH-ZSH-06, MUST).

## F. PowerShell

| Exit | First line contains | Where | Cause and fix | Rule | Sev |
|---|---|---|---|---|---|
| 1 | `The token '&&' is not a valid statement separator in this version.` | Windows PowerShell 5.1.26100 | Chain operators are 7 and later. `if ($?) { ... }` | PS-COMPAT-02 | MUST |
| 1 | `Unexpected token '??' in expression or statement.` | 5.1 | `??` and the ternary are 7 and later | PS-COMPAT-02 | MUST |
| 0 | none, `len=5` for a word with one accented letter | 5.1 reading a BOM-less UTF-8 `.ps1` | 5.1 decodes the file as ANSI. 7.4.7 and 7.6.6 give `len=4` | PS-ENC-01 | MUST |
| child 3 | caller session ends, no `caller: still alive` | 5.1, 7.4.7, 7.6.6 | `exit` under `irm \| iex` ends the caller's host | PS-SEC-09 | MUST |

The parse is the cheapest red. It fails on the whole file, so nothing ran.
Run it with `powershell.exe`, because 7.x parses the same file clean and prints
nothing. Pass is empty output.

```powershell
$tokens = $null
$errors = $null
$path = (Resolve-Path -LiteralPath 'install.ps1').Path
[void][System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
$errors | ForEach-Object { '{0}:{1} {2}' -f $_.Extent.File, $_.Extent.StartLineNumber, $_.Message }
```

The gate that keeps this from recurring is PS-GATE-04. Mojibake is silent, so
the check is on bytes and not a run. Cell `non-ascii` is a listing: a hit is a
file to read, and a BOM at byte 0 is the fix (PS-ENC-01), not the defect.

```sh
for t in git xargs grep; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' '*.psd1' | LC_ALL=C xargs -0 -r grep -H -n -e '[^[:print:][:space:]]' --
```

For `exit` under `iex` the listing, the mechanism and the detection line that
throws under StrictMode are in
[references/windows-and-ci.md](references/windows-and-ci.md).

## G. Nushell

Nushell has no 1.0, and removed or renamed commands are the usual cause. Read
the error id, not the English: `let-env` fails as `assignment_requires_variable`
and never says it was removed.

| Exit | First line contains | Cause and fix | Rule | Sev |
|---|---|---|---|---|
| 1 | `nu::parser::shell_andand` or `shell_oror` | No `&&` or `\|\|`. Use `;` or `and`, `try` or `or` | SH-NU-08 | MUST |
| 1 | `nu::shell::removed_command`, `nu::parser::extra_positional`, `unknown_flag`, `assignment_requires_variable` | A removed or renamed command. The error names the replacement or the new argument shape | SH-NU-04 | MUST |
| 0 | `nu::parser::deprecated` (0.116.0 only) | A deprecation warning. 0.105.1 is silent | SH-NU-04 | MUST |
| 1 | `nu::shell::external_command` | A removed name fell through to an external binary | SH-NU-03 | MUST |

`nu-check --debug` catches parse errors only. It returned `true` with exit 0 for
a removed command, a deprecation and a fall-through, so exit 0 is not a clean
file. Gate with SH-NU-02, and repair by the error id from
[references/nu-churn.md](references/nu-churn.md), never by guess.

```sh
nu --no-config-file --commands 'nu-check --debug script.nu'
```

## H. Hosts, CI and installers

| Exit | First line contains | Where | Cause and fix | Rule | Sev |
|---|---|---|---|---|---|
| 124 | the `Microsoft Windows [Version` banner and no output | Git Bash 5.2.15 on Windows | `cmd /c` loses its `/c` to path conversion. Write `cmd //c` | SH-CI-14 | SHOULD |
| 2 | `panic: interface conversion: syntax.ArithmExpr` | go-task 3.45.4, 3.51.1, 3.53.1, 3.54.0 | An unquoted hyphenated key in an associative array. Quote the key | SH-CI-18 | MUST |
| 0 | `[[: not found` in a `container:` job | dash images | See B | SH-CI-04 | MUST |
| 1 | `wget: unrecognized option: secure-protocol=TLSv1_2` and a usage dump | BusyBox wget 1.37.0 (alpine:3.22) | GNU flags with no probe | SH-USER-10 | MUST for installers |

The reproductions, the mechanisms and the call-site listings are in
[references/windows-and-ci.md](references/windows-and-ci.md). The wget row also
falls under SH-INST-22 (watched 2026-10-11 on BusyBox v1.37.0, alpine:3.22: exit 1 with the flags, DNS failure without).

## The receipt

Every run ends with this, success or not. Write six blocks in order.

1. **Symptom**, one line, in the reporter's words.
2. **Interpreter identity**, Step 0's output pasted, with the image tag.
3. **Root cause**, the mechanism, with the interpreter version it holds on.
4. **Evidence**, the command and its verbatim output: the status and the first
   stderr line. Two readings when a fix was applied.
5. **Fix, or gap**: the change and the rule ID it satisfies, or the named gap
   with the surface that would settle it.
6. **Rule IDs relied on**, every one. A diagnosis citing none either found
   something new, and says so, or skipped the standard that already covered it.

## MUST rows this procedure surfaces

Restated as findings so a diagnosis run without the rules loaded still reports
them with the right ID. Rule text and full verification live in the
`shell-quality` and `powershell-quality` rules.

| # | Finding | Rule |
|---|---|---|
| 1 | A failure was routed on its exit code alone, or a 126 or 127 on an existing file was read as "interpreter missing" | SH-CORE-05 |
| 2 | The interpreter identity was not collected before a fix | SH-CORE-04 |
| 3 | A fix stopped the symptom by changing the measurement | SH-CORE-01 |
| 4 | A new check was relied on before a plant and a twin were run | SH-CORE-02 |
| 5 | A `# shellcheck disable=` carries no same-line reason | SH-GATE-05 |
| 6 | A shell file contains a CR byte or a BOM | SH-GATE-15 |
| 7 | `.gitattributes` does not keep shell files LF | SH-GATE-14 |
| 8 | `#!/bin/sh` code enables pipefail other than by the one-line probe | SH-DIAL-07 |
| 9 | The header kind and the shebang disagree | SH-DIAL-03 |
| 10 | A bash script uses a construct above 3.2.57 with no `# bash-floor:` line | SH-DIAL-06 |
| 11 | A possibly-empty array is expanded unguarded under `set -u` on a floor below 4.4 | SH-ERR-12 |
| 12 | A pipeline under pipefail ends in an early-exit reader | SH-ERR-09 |
| 13 | A container-job step uses bash syntax with no `shell: bash` | SH-CI-04 |
| 14 | A Taskfile body reads a hyphenated associative key unquoted | SH-CI-18 |
| 15 | A glob has no no-match policy in zsh | SH-ZSH-05 |
| 16 | A 5.1-bound `.ps1` contains 7-only syntax | PS-COMPAT-02 |
| 17 | A 5.1-bound `.ps1` has a non-ASCII byte with no BOM | PS-ENC-01 |
| 18 | `exit` runs in a script delivered by `irm \| iex` | PS-SEC-09 |
| 19 | A nu file is not gated by the strict gate | SH-NU-02 |
| 20 | An installer passes GNU wget flags with no probe | SH-INST-22 |

## What agents get wrong

Ranked by measured count, then by how many planted fixtures each reddened. Rows
with no fleet count say so.

| # | Mistake | Count and source | Mechanical check |
|---|---|---|---|
| 1 | Reads 127 or 126 on a file that exists as "interpreter missing" and installs one | 93 of 186 authored scripts at mode 100644 in a fleet audit, 23 of 23 interpreters give 126 | The `mode` cell, then the `cr` cell (SH-CORE-05) |
| 2 | Clears an `Illegal option -o pipefail` with `\|\| true` or a bash shebang | no count, `\|\| true` fails on dash 0.5.12-2 and posh | The `pipefail-or` cell, and `shellcheck -s sh` (SH-DIAL-07) |
| 3 | Calls dash "pipefail-less" as a fact | no count, the answer flips by package | Step 0's `probe:` line |
| 4 | Takes a green ShellCheck as proof of a bash 3.2 fix | no count, exit 0 before and after the fix | Run on `bash:3.2` and compare the output with `bash:5.3` |
| 5 | Tests a signal death against a literal such as 141 | no count, misses on 3 of 11 shells | `-gt 128` and `kill -l` (SH-ERR-19) |
| 6 | Fixes CRLF with `sed -i`, or fixes the file and not `.gitattributes` | no count | The `cr` cell on the fixed tree (SH-GATE-14) |
| 7 | Clears a zsh `no matches found` with a global `setopt nonomatch` | no count, 6 of 6 sampled catalog blocks abort or skip on zsh | `zsh -f -c` on the block in an empty directory (SH-ZSH-05) |
| 8 | Writes `&&`, `??` or a ternary in a 5.1-bound `.ps1` because 7.x accepted it | no count | The 5.1 parse block (PS-GATE-04) |
| 9 | Reads garbled 5.1 text as a console font problem | no count, 5.1 gives `len=5` for one accented letter | The `non-ascii` cell (PS-ENC-01) |
| 10 | Calls `exit` under `irm \| iex`, or copies a detection line that throws under StrictMode | no count, reproduced on 5.1, 7.4.7, 7.6.6 | The `exit` listing in `references/windows-and-ci.md` (PS-SEC-09) |
| 11 | Repairs a nu error by guess, or trusts `nu-check` exit 0 | no count, `nu-check` returned `true` on 4 of 12 failing or deprecated lines | The error id and the SH-NU-02 gate |
| 12 | Wraps a hanging `cmd /c` in a timeout | no count | The `cmd /c` listing in `references/windows-and-ci.md` (SH-CI-14) |
| 13 | Adds `shell: bash` to a container step without checking the image | no count | `docker run --rm IMAGE sh -c 'command -v bash'` (SH-CI-04) |
| 14 | Pastes a paraphrase of the error instead of the first line | no count | The receipt's block 4 holds the verbatim line |
| 15 | Weakens the check to reach green | no fleet count, 7 of 7 forms red in the diff-time check | The SH-CORE-01 diff check |
