---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Shell Dialect and Floors
summary: The SH-DIAL family. Declaring the dialect and kind, shebang and extension policy, the bash 3.2.57 floor and its guard, the features that fail in #!/bin/sh (pipefail, local, dollar-quote, read -d, test -a, echo -n) and the interpreter set that tests sh code
---

# Shell Dialect and Floors

Rows bind to bash 3.2.57 to 5.3.20, dash 0.5.12-2, -6ubuntu5 and -12, busybox ash 1.37, mksh R59c, ksh93u+m 1.0.10,
yash 2.52, posh 0.14.1, ShellCheck 0.11.0 and shfmt 3.13.1, all measured 2026-10-11. macOS and BSD behaviour is
`unverified: read only`.

Owns which dialect a file declares and how it states kind and floor, shebang and extension policy, the bash 3.2.57
floor with the `bash-floor` marker and its guards, the features that fail in `#!/bin/sh` code, and the interpreter set
that tests it. Not owned here: GNU against BSD command flags, `xargs -r` and locale are `SH-USER`. Errexit, `set -u`
and traps are `SH-ERR`. Quoting, globbing and the `{1..3}` brace result are `SH-EXP`. The `.shellcheckrc`, the
file-list helper and the exit map are `SH-GATE`. Workflow `shell:` templates are `SH-CI`. Emitted code is `SH-EMIT`.
Kinds: INST (installer, anything piped to sh), CI (a workflow `run:` target), TOOL (repo tooling), LIB (sourced, no
shebang), RC (rc or login snippet), EMIT (emitted code). "sh kinds" means INST, RC, LIB with `shell=sh`, and EMIT for
an sh target.

Contents: [Dates and Defaults](#dates-and-defaults) · [Setup](#setup) · [A. ShellCheck and shfmt](#a-shellcheck-and-shfmt) ·
[B. Header and file shape](#b-header-and-file-shape) · [C. What sh code may contain](#c-what-sh-code-may-contain) ·
[D. The bash 3.2 floor](#d-the-bash-32-floor) · [E. Interpreter set and floor tables](#e-interpreter-set-and-floor-tables) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defects fixed: `files` listed `*.zsh` files into ShellCheck and `shfmt -d` (2,511 output lines on prezto, 232 fewer on zinit), and a dangling or deleted tracked file made `shfmt -f` fail (prezto, mathiasbynens/dotfiles). Both are fixed in the helper.

## Dates and Defaults

Pinned decisions (adopter may override): dialect follows kind, unpinned bash is 3.2.57, sh is POSIX.1-2017 plus probed
features, and a refused floor exits 69 (the status a missing prerequisite uses).

| Kind | Shebang | Floor | Header, anywhere in lines 2 to 8 |
|---|---|---|---|
| INST, any curl-piped target | `#!/bin/sh` | POSIX.1-2017 plus probed pipefail and local | `# Kind: installer. Dialect: POSIX sh. Floor: POSIX.1-2017 + probed (pipefail, local). Tested: dash 0.5.12-2, busybox ash 1.37, bash 3.2 --posix.` |
| TOOL, CI, unpinned | `#!/usr/bin/env bash` | bash 3.2.57 | `# Kind: repo tool. Floor: bash 3.2.` |
| TOOL, CI, pinned | `#!/usr/bin/env bash` | the named version, guarded | `# Kind: CI helper. Floor: bash 5.2 (pinned: ubuntu-24.04 runner).` and `# bash-floor: 5.2` |
| LIB, RC | none, line 1 `# shellcheck shell=sh` or `shell=bash` | the callers' floor | `# Kind: library. Floor: sourced by CALLERS.`, plus `# bash-floor: X.Y` when raised |
| EMIT | the target shell's own | the target's floor | sh targets take the INST row, the rest is `SH-EMIT` |

## Setup

Every block opens with the PREFLIGHT line, and any `PREFLIGHT:` line means stop and fix the host. Paste the two
functions first (a missing one prints `not found`, which fails closed) and add `-- . ':(exclude,glob)**/vendor/**'` to
the `git ls-files` call to prune vendored trees (also SH-GATE-09). `files` leaves out `*.zsh` because ShellCheck cannot parse zsh (350 findings on prezto, `SH-ZSH-12` gates zsh). `shfiles` selects `#!/bin/sh` files only and
tolerates `#! /bin/sh` and a CRLF shebang. Pass is empty stdout and stderr, except for cells marked listing, whose hits
are sites to read. Every cell here ran red on a plant and silent on a twin under GNU grep and busybox 1.37 (measured 2026-10-11).

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard -- . ':(exclude,glob)**/*.zsh' | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
shfiles() { files | xargs -0 -r awk 'FNR == 1 && /^#![ ]*\/(usr\/)?bin\/(env )?sh([ \r]|$)/ { print FILENAME }' | tr '\n' '\0'; }
```

## A. ShellCheck and shfmt

```sh
for t in git xargs grep awk shfmt shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files | xargs -0 -r shellcheck -f gcc --
files | xargs -0 -r shfmt -d -i 4 -ci --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-DIAL-02 | The body matches the declared dialect: run ShellCheck, which reads the dialect from the shebang (SC3xxx are on for `#!/bin/sh`), and `shfmt -d`. Never use `shfmt -d` as the dialect gate for a file without a shebang. | A `#!/bin/sh` file holding an array is SC3030 and SC3054 in ShellCheck and `tried parsing as posix` in shfmt, but shfmt 3.13.1 parses a shebang-less `.sh` as bash, so only ShellCheck objects. One 19-line bashism file raises 18 distinct SC3xxx codes. **Scope:** all kinds. Unreleased ShellCheck master accepts pipefail in SC3040 and removes SC3003, so a later pin silences warnings dash 0.5.12-2 still needs: keep the SH-DIAL-07 and SH-DIAL-09 greps beside the pinned version. | Block A, both lines. Function names and `function` fall under the same run (SC3033, SC2112, SC2113). Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-08 | Suppress SC3040 on the pipefail probe with a reason, placed above the one-line `&&` list or the `if`: `# shellcheck disable=SC3040 # probed in a subshell first: dash 0.5.12-2 and posh abort on an unknown set -o`. Pin ShellCheck until an sh floor with `$'..'` exists. | The probe line raises SC3040 twice on 0.11.0 (columns 9 and 41), and a directive inside the `if` leaves the probe line flagged. **Scope:** sh kinds. | Block A first line exits 0 on the prologue below, and `shellcheck --version` shows the pin (also SH-GATE-13). Watched red (measured 2026-10-11). | SHOULD |

## B. Header and file shape

Each cell prints offenders. Cell d22 lists markdown, and cells d05 and d21 list sites to read against the header.

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# d01 SH-DIAL-01
files | xargs -0 -r awk 'FNR == 1 { if (NR > 1 && !ok) print prev; prev = FILENAME; ok = ($0 ~ /^#!/) } /^# shellcheck shell=/ { ok = 1 } END { if (NR > 0 && !ok) print prev }'
# d03 SH-DIAL-03
files | xargs -0 -r awk 'FNR == 1 { if (NR > 1 && !ok) print prev; prev = FILENAME; ok = 0 } FNR >= 2 && FNR <= 8 && /^# (Kind: |bash-floor: )/ { ok = 1 } END { if (NR > 0 && !ok) print prev }'
# d05 SH-DIAL-05 (listing: read each hit against its header)
files | xargs -0 -r grep -H -n -e '^#!/bin/bash' --
# d18 SH-DIAL-18
files | xargs -0 -r grep -H -n -E -e '^#![[:space:]]*/[^[:space:]]+[[:space:]]+[^[:space:]]+[[:space:]]+[^[:space:]]' --
# d21 SH-DIAL-21 (listing: waive with a header line naming the kind)
files | xargs -0 -r awk 'FNR == 1 && /^#!/ { n = split(FILENAME, p, "/"); if (p[n] !~ /[.]/) print "extensionless shell file: " FILENAME }'
# d22 SH-DIAL-22
git ls-files -z --cached --others --exclude-standard -- '*.md' | xargs -0 -r grep -H -n -E -e 'macOS .?/bin/sh.? is dash' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-DIAL-01 | Every authored shell file declares its dialect: a shebang on line 1, or for a shebang-less sourced file `# shellcheck shell=sh` or `shell=bash`. | Without it shfmt guesses bash and ShellCheck raises SC2148 (error, exit 1). A heredoc line holding `#!/bin/sh` must not satisfy the check: a `grep -L '^#!'` form passed 8 void-packages files. **Scope:** all kinds. A mid-file `shell=` directive stays a pass. | Cell d01. Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-03 | State kind and floor in the header, anywhere in lines 2 to 8: `# Kind: ...`, or `# bash-floor: X.Y` for a pinned bash script (table in Dates and Defaults). Choose the dialect by kind: INST `#!/bin/sh`, TOOL and CI `#!/usr/bin/env bash` at 3.2.57, LIB no shebang. | `/bin/sh` exists on 21 of 21 images and bash on 18 of 21. `#!/usr/bin/env bash` and `#!/bin/bash` both exit 127 on Alpine and busybox, and `#!/bin/bash` exits 126 on nixos/nix. The runners macos-14, 15 and 26 print bash 3.2.57. **Scope:** all kinds, pinned decision (adopter may override). SPDX or a `shellcheck disable` line may precede the header. | Cell d03 checks presence only. Whether the stated kind is true is a reading heuristic. Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-05 | Write `#!/usr/bin/env bash` for bash. Write `#!/bin/bash` only when the header says `Floor: system bash 3.2`. Never `#!/bin/sh` for bash code. | `/bin/bash` is absent on nixos/nix (exit 126) and on Alpine and busybox:1.37 (127), and `env bash` fails only where bash is not installed. **Scope:** TOOL, CI. | Cell d05 lists the exceptions to read against the header, and ShellCheck SC2239 covers relative shebang paths. Watched red (measured 2026-10-11). | SHOULD |
| SH-DIAL-18 | Pass at most one argument after the interpreter in a shebang. Set options in the body (`set -eo pipefail`). Never `#!/usr/bin/env bash -e`, and never `env -S` in a shebang that can reach a container. | Linux hands everything after the interpreter over as one argument: `#!/bin/bash -e -o pipefail` gives `/bin/bash: - : invalid option` (exit 1) and `#!/usr/bin/env bash -e` gives `env: 'bash -e': No such file or directory` (127). `env -S` is absent from busybox 1.37 (exit 1) and coreutils 8.22 (125). **Scope:** every executable kind, Linux 6.18, coreutils 9.4, busybox 1.37. macOS splits arguments (unverified: read only). | Cell d18, and ShellCheck SC2096. Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-21 | Name authored scripts `.sh`. Use `.bash` only for sourced bash libraries. Use an extensionless shebang file only for an installed command or a tool-managed hook, and then its header names the kind. | No glob matches a shebang, so `.sh` alone selects no dialect (SH-DIAL-01 still applies) and `shfmt -f` is what lists extensionless files. **Scope:** all kinds. shfmt 3.13.1 has no `--detect` (3.14.0 adds it). | Cell d21 lists extensionless shebang files. Watched red (measured 2026-10-11). | SHOULD |
| SH-DIAL-22 | Never claim the macOS sh is dash. Write "bash 3.2 in POSIX mode by default, selectable to dash (unverified: read only)". | Apple documents bash 3.2.57 and a `/bin/sh` shim that reads `/private/var/select/sh`, and no Apple source says dash is the default. The sentence sits in six fleet rule files. **Scope:** rule and doc text. | Cell d22. Watched red (measured 2026-10-11). | SHOULD |

## C. What sh code may contain

Cells run on `shfiles` output only, so a `#!/bin/bash` file never trips them. Measured on 2026-10-11, `y` works and
`n` fails. The bash 3.2 column ran with `--posix` in the `bash:3.2` image. Debian 12 and Ubuntu 24.04 ship dash -2 and
-6ubuntu5, Debian 13 and Ubuntu 26.04 ship -12. Dash 0.5.12-7 (2024-05-17) backported pipefail and `dash --version`
does not exist, so a version string identifies nothing. Upstream dash 0.5.13.5 has `$'..'`, no package does.

| Feature | dash -2/-6 | dash -12 | ash 1.37 | mksh | ksh93u+m 1.0.10 | yash 2.52 | posh 0.14.1 | bash 3.2 `--posix` | Verdict |
|---|---|---|---|---|---|---|---|---|---|
| `set -o pipefail` | n (abort) | y | y | y | y | y | n (abort) | y | probe |
| `$'..'` | n (literal) | n | y | y | y | n (literal) | n (literal) | y | never |
| `read -d` | n | n | y | y | y | n | n | y | never |
| `test -nt -ot -ef` | y | y | y | y | y | y | n | y | use, not for posh |
| `test -a -o` | y | y | y | y | y | y | y | y | never |
| `local` | y | y | y | y | n (127) | y | y | y | allowed with directive |
| `echo -n` | y | y | y | y | y | n (prints `-n`) | y | y | never |
| `[[ ]]` | n | n | y | y | y | y | n | y | never |
| `{1..3}` | n (once) | n | n | n | y (three) | n | n | y | never (also SH-EXP-12) |

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# d07 SH-DIAL-07
shfiles | xargs -0 -r grep -H -n -E -e '^[[:space:]]*set [^#]*pipefail' --
# d09 SH-DIAL-09
shfiles | xargs -0 -r grep -H -n -E -e "^[\$]'" -e "[[:space:]=(;&][\$]'" --
# d10 SH-DIAL-10
shfiles | xargs -0 -r grep -H -n -E -e '(^|[[:space:];{(])read[[:space:]]+(-[a-zA-Z]+[[:space:]]+)*-[a-zA-Z]*d' --
# d11 SH-DIAL-11
shfiles | xargs -0 -r grep -H -n -E -e '\[[^]]*[[:space:]]-[ao][[:space:]]' --
# d12 SH-DIAL-12
shfiles | xargs -0 -r grep -H -n -E -e '(^|[[:space:];{(])echo[[:space:]]+-[neE]' --
# d17 SH-DIAL-17 (every shell file: a reason follows the code on the same line)
files | xargs -0 -r grep -H -n -E -e 'disable=SC3043[[:space:]]*$' -e 'disable=SC3040[[:space:]]*$' --
```

```sh
# wrong: a bare set -o exits dash 0.5.12-2 and posh before the next line, and bare local is exit 127 on ksh93u+m
set -o pipefail
f() { local x; x=1; }
# right: probe in a subshell, reasoned directives, and correctness never needs local scope
# shellcheck disable=SC3040 # probed in a subshell first: dash 0.5.12-2 and posh abort on an unknown set -o
(set -o pipefail) 2>/dev/null && set -o pipefail
# shellcheck disable=SC3043 # local verified at runtime by has_local(); ksh93 gets typeset
has_local() { (PATH='' local _has_local); }
has_local 2>/dev/null || alias local=typeset
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-DIAL-07 | In `#!/bin/sh` code enable pipefail only as `(set -o pipefail) 2>/dev/null && set -o pipefail`, on one line. Never bare, never `\|\| true`, never `2>/dev/null \|\| :`, never through `eval` or `command set`. Never let correctness depend on it. | An unknown `set -o` is a special-builtin error that exits dash 0.5.12-2 and -6ubuntu5 (status 2) and posh 0.14.1 (status 1) with no later output, and `command set` survives dash but not posh. The probe ran on 11 of 11 interpreters and leaves pipefail off on dash -2/-6 and posh, so a step that needs pipefail is a bash step. **Scope:** sh kinds, dash 0.5.12-2, posh 0.14.1 (also SH-CI-03). | Cell d07 (the probe is `(set ...)`, so the anchor skips it). Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-09 | Never write `$'..'` in `#!/bin/sh` or `shell=sh` code. Build the string with `printf`. | dash 0.5.12-2/-6ubuntu5/-12, yash 2.52 and posh 0.14.1 print the literal `$a\tb` and exit 0, a silent wrong value. A literal tab or newline through `printf` works, and command substitution strips trailing newlines. **Scope:** sh kinds (also SH-EMIT-12 for emitters). | Cell d09, and ShellCheck SC3003 on 0.11.0 (removed on master). Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-10 | Never use `read -d` in `#!/bin/sh` code. Use `find ... -exec cmd {} +` or `find ... -print0 \| xargs -0 -r cmd`. | `read -d` is `Illegal option -d` on every dash (upstream 0.5.13.5 too), invalid on yash and posh, and the failure is a stderr line and an empty variable, not an exit. POSIX's own `find -print0 \| while IFS= read -rd ""` example is therefore not dash-portable. **Scope:** sh kinds, dash 0.5.12, yash 2.52, posh 0.14.1. | Cell d10, and ShellCheck SC3045. Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-11 | Never write `-a` or `-o` inside `[ ]` or `test`. Use `[ p ] && [ q ]`. | POSIX.1-2024 removed them and ShellCheck SC2166 fires on 0.11.0, but they still work on 11 of 11 interpreters and Debian Policy 10.4 keeps them, so this is style plus forward safety, not a runtime failure. **Scope:** sh kinds. | Cell d11, and ShellCheck SC2166. Watched red (measured 2026-10-11). | SHOULD |
| SH-DIAL-12 | In `#!/bin/sh` code print with `printf '%s\n'`, never `echo -n`, `echo -e` or `echo -E`. | yash 2.52 prints `-n` and dash interprets `\t` and prints `-e`. macOS `/bin/sh` echo is unverified: read only. **Scope:** sh kinds, yash 2.52, dash 0.5.12. The data and non-sh half is `SH-EXP`. | Cell d12, and ShellCheck SC3037. Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-17 | Give `local` in `#!/bin/sh` code a reasoned directive. INST: `# shellcheck disable=SC3043 # local verified at runtime by has_local(); ksh93 gets typeset` with the probe `has_local() { (PATH='' local _has_local); }` and `has_local 2>/dev/null \|\| alias local=typeset`. Other sh kinds: `# shellcheck disable=SC3043 # local: dash, ash, bash, mksh, yash, posh, not ksh93 (see header)`. A bare `disable=SC3043` or `disable=SC3040` is a defect. | ksh93u+m 1.0.10 exits 127 on bare `local`. The probe keeps it running but makes the variable global, so code must not need local scope there. Austin Group bug 767 "Add built-in local" is Closed, Rejected, and Debian Policy 10.4 mandates `local` for `/bin/sh`. **Scope:** sh kinds, ksh93u+m 1.0.10 is the only shell measured without `local`. | Cell d17, and ShellCheck SC3043 stays red without the directive. Watched red (measured 2026-10-11). | MUST |

## D. The bash 3.2 floor

A violation looks like a successful run with missing data: `declare -A m` on 3.2 prints `declare: -A: invalid option`
and the script exits 0. Bash parses a whole compound command before running any of it. Classes on 3.2 (47 probes on
3.2 to 5.3, first working version in brackets):

| Class on 3.2 | Constructs |
|---|---|
| Parse error: exit 2 at that line, a function holding it is never defined | `\|&`, `&>>`, `coproc`, `;;&`, `;&` [4.0], `[[ -v x ]]` [4.2], an unparenthesised case pattern inside `$( )` [4.0] |
| Loud at run time, then empty data | `declare -A`, `mapfile`, `readarray`, `${x^^}`, `${x,,}`, `globstar`, `read -t 0.2` [4.0], `[ -v x ]`, `${a[-1]}`, `${s:0:-2}`, `printf '%(%Y)T'`, `declare -g`, `lastpipe` [4.2], `local -n`, `wait -n` [4.3], `${x@Q}`, `inherit_errexit` [4.4], `${x@U}` [5.1], `${ cmd; }`, `source -p`, `trap -P` [5.3] |
| Silent, no message | `{01..03}`, `{1..10..4}` [4.0], `EPOCHREALTIME`, `EPOCHSECONDS`, `BASH_ARGV0` [5.0], `SRANDOM` [5.1], `GLOBSORT` [5.3], `${y/b/[&]}` (behaviour change at 5.2) |

Scripts, run from the repo root (adopters copy `checks/` to `scripts/`). Each exits 0 pass, 1 verdict failed with the
finding printed, 69 a tool is missing, 78 bad usage or not a work tree:

- `checks/dialect-bash-floor.sh [PATHSPEC...]` prints every bash 4+ construct and every silent-drift shopt, `BASH_COMPAT`
  or `BASH_*` variable in a shell file with no `# bash-floor:` line (SH-DIAL-06, SH-DIAL-28). Also 74 scan failed, 79 empty.
- `checks/dialect-bash32.sh MODE SCRIPT` runs on the `bash:3.2` image. `refuse` requires exit 69, empty stdout and a
  stderr line (SH-DIAL-25), `lib` sources a library from a throwaway caller and requires status 69 with the caller
  alive (SH-DIAL-26), and `run SCRIPT --help` requires exit 0 (SH-DIAL-27).

```bash
# wrong: the guard sits in main() beside a parse-time [[ -v ]], so 3.2 dies at parse (exit 2) and never reaches it
main() {
    [ "${BASH_VERSINFO:-0}" -ge 5 ] || exit 69
    [[ -v HOME ]] && echo set
}
# right: the first executable statement, scalar test, outside any function (a minor floor uses case ${BASH_VERSION-})
[ "${BASH_VERSINFO:-0}" -ge 5 ] || {
    printf '%s\n' "$0: needs bash >= 5 (found ${BASH_VERSION:-non-bash})" >&2
    exit 69
}
```

Cell d25 lists the scripts that carry the marker, each of which needs a `refuse` run:

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# d25 SH-DIAL-25 (listing)
files | xargs -0 -r grep -H -n -e '^# bash-floor: ' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-DIAL-06 | Write every unpinned bash script to bash 3.2.57. The denylist prints nothing for any file without a `# bash-floor:` line, and a script that needs more follows SH-DIAL-25. Replace each hit with its 3.2 twin (a `while IFS= read -r l` loop for `mapfile`, `tr` for `${x,,}`, parenthesised `(a)` case patterns in `$( )`, `i=$((i + 1))`). When ShellCheck SC2207 says "Prefer mapfile", the answer is the read loop. | macOS `/bin/bash` and the runners macos-14, 15 and 26 are 3.2.57, and reachability cannot be decided statically. ShellCheck 0.11.0 flags 0 of 47 bash 4+ constructs, also with `--enable=all` (koalaman/shellcheck#2615 open since 2022-10-25), shfmt `-ln bash` accepts 46 and `bash -n` on 3.2 sees 6. **Scope:** TOOL, CI, LIB shell=bash, bash 3.2.57 to 5.3.20. Blind spots: a flag carried in a variable, a quoted word such as `mapfile `, and a `#` earlier on the line (`$#`, `${#a[@]}`). Add one `-e` per newly probed construct. | `checks/dialect-bash-floor.sh`. Flags 40 of 47 probes (the other seven are the safe `$( )` spelling, two `:+` probes and four owned by SH-ERR-12 and SH-EXP-15) and 0 on 30 twins. Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-25 | A bash script that needs more than 3.2 carries `# bash-floor: X.Y` in lines 2 to 8 and starts with a 3.2-parsable guard as its first executable statement, outside any function, `if` or `{ }`, that writes one stderr line and exits 69 with empty stdout. Major floor: the scalar `[ "${BASH_VERSINFO:-0}" -ge N ]`. Minor floor: `case ${BASH_VERSION-} in '' \| [0-3].* \| 4.[01].*) ... exit 69 ;; esac`. Never index `${BASH_VERSINFO[0]}` in a guard. | Without the guard 3.2 prints errors and continues with wrong data. The array form exits 2 on dash and both ash versions (3 of 11) and the scalar form refuses with 69 on 10 of 11. A re-exec into a newer bash before refusing is allowed. The marker `# bash-floor: 3.2 guarded` declares 4+ constructs only in untaken runtime branches (SH-DIAL-27). **Scope:** TOOL, CI, LIB shell=bash. | `checks/dialect-bash32.sh refuse SCRIPT` on every script cell d25 lists. Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-26 | A sourced library with a raised bash floor has no shebang, `# shellcheck shell=bash` on line 1, `# bash-floor: X.Y` on line 2, the `case ${BASH_VERSION-}` guard, and ends the refusal with `return 69 2>/dev/null \|\| exit 69`, never a bare `exit`. The `exit` fallback carries `# shellcheck disable=SC2317 # exit is the fallback when the file is executed, not sourced`. | A bare `exit` in a sourced file ends the user's interactive shell, and an unguarded 4.2 construct drops that function and every later definition on 3.2. ShellCheck 0.11.0 reports SC2317 (note) on all four spellings of the refusal, so the per-site directive is the only way to a clean lint (the gate allow table needs that row, also SH-GATE-07). **Scope:** LIB, RC, callers bash 3.2 to 5.3, dash and zsh. | `checks/dialect-bash32.sh lib LIBRARY`, and ShellCheck exits 0 on the library. Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-27 | Only constructs that fail at run time may sit in the untaken branch of a `BASH_VERSINFO` test: `declare -A`, `local -A`, `mapfile`, `${x,,}`, `${x@Q}`, `${ cmd; }`, `shopt -s ...`. Parse-time constructs never may: `[[ -v x ]]`, `\|&`, `&>>`, `coproc`, `;;&`, an unparenthesised case pattern in `$( )`. The CI job that enforces SH-DIAL-06 also runs a no-side-effect path of every unmarked script on the `bash:3.2` image and requires exit 0. | `if [ "${BASH_VERSINFO[0]:-0}" -ge 4 ]; then printf '%s\n' "${1,,}"; else ...; fi` prints `abc` on 3.2, 4.1, 4.2 and 5.3, and the same file with `[[ -v $1 ]]` in the untaken branch exits 2 on 3.2 and 4.1. `bash -n` is not the check: it sees 6 of 47 constructs and exits 2 on a guard-first script that refuses correctly. **Scope:** TOOL, CI, LIB shell=bash, bash:3.2 (3.2.57 with Alpine userland, so BSD userland is not covered). | `checks/dialect-bash32.sh run SCRIPT --help`. That the CI job names `bash:3.2` and the SH-DIAL-06 check is a reading heuristic. Watched red (measured 2026-10-11) for the run component. | MUST |
| SH-DIAL-28 | Configure away the silent bash drift. Before any `${v/pat/rep}` or `${v//pat/rep}` whose replacement contains `&` or comes from data, write `shopt -u patsub_replacement 2>/dev/null \|\| :` (or build the string with `%%` and `#` splits, or `sed`). Never set `BASH_COMPAT` or `shopt -s compatNN` to pin behaviour. Probe an option instead of assuming it: `shopt -s inherit_errexit 2>/dev/null \|\| :`. Never `shopt -s lastpipe` or `globstar` on a 3.2 floor. | `${y/b/[&]}` is `a[&]c` on 3.2 to 5.1 and `a[b]c` on 5.2 and 5.3 with no error, and `shopt -u patsub_replacement` first gives `a[&]c` on all five tested versions. `BASH_COMPAT=32` on 5.3 rejects none of `declare -A`, `${s,,}`, `mapfile`, `[[ -v ]]` and `${x@Q}`. A bare unknown `shopt -s` returns 1 and kills a `set -e` script on 3.2 to 4.3, and `lastpipe` leaves the variable empty on 3.2 to 4.1. **Scope:** TOOL, CI, bash 3.2 to 5.3. A distro may compile `patsub_replacement` differently (5.3 NEWS), so detect by behaviour if it matters. Errexit drift is `SH-ERR`, dot globs `SH-EXP`. | `checks/dialect-bash-floor.sh` covers bare shopts, `BASH_COMPAT` and a literal `&`. An `&` that arrives in a variable is invisible to grep, so read each such replacement for the `shopt -u` line above it. Watched red (measured 2026-10-11). | SHOULD |

## E. Interpreter set and floor tables

`checks/dialect-interp-set.sh SCRIPT [ARG...]` runs the script under dash 0.5.12-2 (`debian:12`), busybox ash 1.37
(`alpine:3.22`) and bash 3.2 `--posix` (`bash:3.2`) and requires exit 0 and identical stdout from all three, or exit 69
when docker or an image is unavailable. Add posh 0.14.1 when strict POSIX.1-2017 is claimed.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-DIAL-04 | Test every `#!/bin/sh` script on dash 0.5.12-2 (Debian 12, Ubuntu 24.04), busybox ash 1.37 (Alpine 3.22) and bash 3.2 `--posix`. Evidence from bash 5.2 `--posix`, `zsh --emulate sh` or the author's `/bin/sh` never counts. | The `/bin/sh` of 12 of 21 images accepts bashisms that dash rejects. bash 5.2 `--posix` accepts arrays, `${x,,}`, `<( )`, `[[ ]]` and `function` and exits 0 where dash and ash exit 2 and posh 1. **Scope:** sh kinds, macOS and FreeBSD `sh` unverified: read only. | `checks/dialect-interp-set.sh SCRIPT`, plus Block A for the dialect. Watched red (measured 2026-10-11). | MUST |
| SH-DIAL-23 | A table of shell or tool floors names the image or version and the date on every row. | A version string does not identify features: dash 0.5.12-2 and -12 differ on pipefail. The Section C matrix is the format. **Scope:** rule and doc text. | Reading heuristic: every row carries an image or version and a date. Not watched red. | SHOULD |

## What Agents Get Wrong Here

Ranked by how often a coding agent emits it unprompted times what it costs.

1. **`set -o pipefail` or `set -euo pipefail` in a `#!/bin/sh` file.** dash 0.5.12-2 and posh abort before the next line. `SH-DIAL-07`, `SH-DIAL-08`.
2. **bash 4+ in an unpinned bash script:** `mapfile`, `declare -A`, `${x,,}`, `[[ -v x ]]`, and `mapfile` from ShellCheck's SC2207 advice. 3.2 continues with empty data. `SH-DIAL-06`.
3. **A `#!/bin/sh` file full of bashisms** (`[[ ]]`, arrays, `function`, `$'..'`, `echo -e`, `read -d`, `[ a -a b ]`) that passes on the author's `/bin/sh` or bash `--posix`. `SH-DIAL-04`, `SH-DIAL-09` to `SH-DIAL-12`.
4. **Options in the shebang:** `#!/bin/bash -e`, `#!/usr/bin/env bash -eu`, `env -S`. `SH-DIAL-18`.
5. **A version guard that never runs:** inside `main()`, after a `declare -A`, with `${BASH_VERSINFO[0]}`, or with exit 1. `SH-DIAL-25`, `SH-DIAL-27`.
6. **Bare `local` or a bare `# shellcheck disable=SC3043` in sh.** ksh93u+m exits 127. `SH-DIAL-17`.
7. **`#!/bin/sh` on bash code, or `#!/bin/bash` where bash lives elsewhere** (nixos/nix, Alpine). `SH-DIAL-03`, `SH-DIAL-05`.
8. **Pinning behaviour with `BASH_COMPAT`, or `shopt -s lastpipe` and `globstar` on a 3.2 floor.** `SH-DIAL-28`.
9. **A bare `exit` in a sourced library's version guard.** It ends the user's shell. `SH-DIAL-26`.
10. **No dialect line:** a shebang-less `.sh` with no `shell=` directive, or no kind and floor header. `SH-DIAL-01`, `SH-DIAL-03`.
11. **Stating the macOS sh is dash, or trusting a dash version string for pipefail.** `SH-DIAL-22`, `SH-DIAL-07`.
