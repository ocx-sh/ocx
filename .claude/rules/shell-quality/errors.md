---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Shell Errors, Traps and Exit Codes
summary: The SH-ERR family. The error model by script kind, where set -e silently stops working, set -u and empty arrays, pipefail and SIGPIPE, traps and cleanup, signal statuses and re-raise, exit-code tables, die placement, empty-glob contracts and glob shopts, RETURN traps
---

# Shell Errors, Traps and Exit Codes

Rows bind to bash 3.2.57 to 5.3, dash 0.5.12, busybox ash 1.35 and 1.37, mksh R59c, ksh93u+m 1.0.4 and 1.0.10, yash 2.52
and 2.61, posh 0.14.1, zsh 5.0.8 to 5.9, Git Bash 5.2.15 and ShellCheck 0.11.0, all measured 2026-10-11. macOS and BSD
behaviour is `unverified: read only`.

Owns which error model a script of each kind uses, the contexts where `-e` is off, `set -u` and empty arrays, early-exit
readers under `pipefail`, trap handlers and cleanup, signal statuses and re-raise, exit-code tables, where `die` may run,
and what a glob that matches nothing must do. Not owned here: enabling `pipefail` in `#!/bin/sh` and the bash floor
marker are `SH-DIAL`. Quoting and the zsh and fish halves of the unmatched-glob rule are `SH-EXP`, `SH-ZSH` and `SH-FISH`.
The `.shellcheckrc` keys, the gate script's exit table and the verification dialect are `SH-GATE`. A workflow `run:` step's
shell is `SH-CI`. The installer exit map 0 to 7 is `SH-INST`. Other shells' error models are `SH-NU`, `SH-ELV` and
`PS-ERR`. Kinds: TOOL (repo or dev script), CI (step helper), INST (installer or bootstrap), LIB (sourced library), RC (rc
snippet), EMIT (emitted code), GATE (verifier script whose output is the violation).

Contents: [Group 1: ShellCheck Default Checks](#group-1-shellcheck-default-checks) · [Group 2: ShellCheck Optional Check](#group-2-shellcheck-optional-check) ·
[Group 3: A Grep Decides](#group-3-a-grep-decides) · [Group 4: A Behavioural Run Decides](#group-4-a-behavioural-run-decides) ·
[Group 5: Reading Heuristics](#group-5-reading-heuristics) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defect fixed: `files` listed `*.zsh` files into the ShellCheck cells (`esc1`) and a dangling tracked file broke discovery. The `e` cells printed real sites, for example `trap build_failed ERR` at `bin/ruby-build:1701` (`e26`) and unreset `RETURN` traps (`e34`).

## Group 1: ShellCheck Default Checks

Cells `esc1` and `esc2` print one `file:line:col` finding per hit, and empty stdout and stderr is the pass. `esc1` runs on
every shell file, `esc2` only on `#!/bin/sh` files because the SC3xxx codes mean "undefined in POSIX sh". `--norc` keeps a
repo rc from changing the code list, and a per-site `# shellcheck disable=` with a reason still waives a hit. Watched red
on the plant and empty on the twin (measured 2026-10-11).

```sh
for t in git xargs grep awk shfmt shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard -- . ':(exclude,glob)**/*.zsh' | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
shfiles() { files | xargs -0 -r awk 'FNR == 1 && /^#![ ]*\/(usr\/)?bin\/(env )?sh([ \r]|$)/ { print FILENAME }' | tr '\n' '\0'; }
# esc1
files | xargs -0 -r shellcheck --norc -f gcc -i SC2064,SC2155,SC2181,SC2015 --
# esc2
shfiles | xargs -0 -r shellcheck --norc -s sh -f gcc -i SC3003,SC3040,SC3044,SC3047,SC3048,SC3049 --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ERR-01 | Never write `local`, `export`, `declare`, `typeset` or `readonly` with `x=$(cmd)` in one statement: declare, assign on the next line, then export or lock. | The builtin's status (0) replaces the substitution's, under `-e` too: `local x=$(false)` returns 0 on 11 of 11 POSIX interpreters, bash 3.2 to 5.3 and zsh 5.0.8 to 5.9, and the split form aborts on all. ksh93u+m has no `local`. **Scope:** every bash script and any `#!/bin/sh` script that uses `local`, all kinds. | Cell `esc1` (SC2155). `local -r x=$(cmd)` is not reported by SC2155, only by the optional SC2312, so read for it. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-02 | Single-quote a trap action or call a function from it. Name conditions `EXIT INT TERM HUP`: uppercase, no `SIG`, no lowercase. Use no `ERR`, `DEBUG` or `RETURN` in `#!/bin/sh`. | dash and posh reject `SIGINT`, posh and zsh in sh mode reject `int`, dash, yash and posh reject `ERR`. A rejected name prints a warning and the script runs untrapped. A double-quoted action expands at definition time. **Scope:** names bind `#!/bin/sh` (INST, RC, EMIT), SC2064 binds all. A value that must be frozen takes a reasoned `# shellcheck disable=SC2064`. | Cell `esc1` (SC2064), cell `esc2` (SC3047, SC3048, SC3049). Watched red (measured 2026-10-11). | MUST |
| SH-ERR-05 | Use `if A; then B; else C; fi`, not `A && B \|\| C`, whenever B can fail. | C runs when A is true and B fails (BashPitfalls 22). SC2015 misses the two commonest C's, `\|\| echo …` and `\|\| exit 1`. **Scope:** all kinds. | Cell `esc1` (SC2015), plus reading for the gap. Red for `\|\| rm file`, partial by design (measured 2026-10-11). | SHOULD |
| SH-ERR-32 | Use no `shopt` or other bash-only builtin in a `#!/bin/sh` script. | dash, both busybox ashes, mksh, both ksh93u+m, yash, posh and zsh in sh mode print "not found" and go on (9 of 11 POSIX interpreters, bash `--posix` accepts it), and under `set -e` the script dies with 127. **Scope:** every `#!/bin/sh` (INST, RC, EMIT). `SH-DIAL` owns the wider SC3xxx bashisms. | Cell `esc2` (SC3044, default-on in the `sh` dialect). Watched red (measured 2026-10-11). | MUST |

Wrong and right for SH-ERR-01:

```sh
local tmp=$(mktemp -d)   # wrong: mktemp failing is hidden, status 0
local tmp; tmp=$(mktemp -d)   # right: -e or || die sees the status
```

## Group 2: ShellCheck Optional Check

Cell `esc3` enables `check-set-e-suppressed`, which reports SC2310 (a function called in a condition, so `-e` is off in
it) and SC2311 (a function called inside `$( )`). It fires only in a file that sets `-e`. Both codes are info level, so a
`-S warning` severity floor silently disables them. Watched red on the plant and empty on the twin (measured 2026-10-11).

```sh
for t in git xargs grep awk shfmt shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard -- . ':(exclude,glob)**/*.zsh' | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# esc3
files | xargs -0 -r shellcheck --norc -o check-set-e-suppressed -f gcc -i SC2310,SC2311 --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ERR-06 | Do not rely on `-e` inside a function that is or may be called from `if`, `while`, `until`, `!` or the non-last side of `&&` and `\|\|` (this includes `f \|\| true`). End every fallible command in such a function with `\|\| return` (or `\|\| die`), or call the function as a plain statement. | Errexit is off in the callee on every shell measured, and POSIX.1-2024 says so. `f() { false; echo still; }; if f; then :; fi` prints `still` on 11 POSIX interpreters, 8 bash versions, Git Bash 5.2 and zsh 5.0.8 to 5.9. SC2310 is a call-site detector: it stays red after the callee is repaired, so green proves only that the call left the condition or carries a directive, and callee correctness is a reading. **Scope:** TOOL, CI, INST, LIB on all POSIX-family shells, bash and zsh. Exceptions: zsh 5.3.1 to 5.9 exits on `! f` (5.0.8 on `! true`), posh ignores `-e` on the last item of an AND-OR list. | Cell `esc3` (SC2310). Watched red (measured 2026-10-11). | MUST |
| SH-ERR-07 | Keep `enable=check-set-e-suppressed` in `.shellcheckrc` (the entry itself is mandatory, also SH-GATE-02) and answer each SC2310 and SC2311 hit by classification: call the function outside the condition (`set +e; f; rc=$?; set -e`), or put a per-site directive with its reason on the line before the call (`# shellcheck disable=SC2310  # predicate: what`), never a file-scope disable. A callee with two or more fallible steps gets `\|\| return` on each step and the directive. Do not enable `check-extra-masked-returns` (SC2312) repo-wide, new-code directories may opt in under SH-ERR-33. | A census of 199 shell files found 62 distinct SC2310 sites in 9 files: 59 benign (one-command predicates, explicit `\|\| return`, pure-shell bodies), 2 defects and 1 latent. The call site never goes green, so enabling the check repo-wide costs one permanent line per site, and ShellCheck 0.11.0 has no baseline option. SC2312 gave 191 sites for 2 strict defects that cell `e10` finds without noise. **Scope:** bash and sh files that set `-e`, ShellCheck 0.11.0. | Cell `esc3`. The classification is a reading. | SHOULD |
| SH-ERR-08 | In bash, call a multi-step function inside `$( )` as `x=$(set -e; f)`, or after a bare `shopt -s inherit_errexit` in a script whose header pins bash 4.4 (`# bash-floor: 4.4` and a guard that exits 69). Never assume `$( )` aborts on an inner failure in default-mode bash or busybox ash. At a call site that carries `\|\| die` or `\|\| return`, the callee's own steps carry `\|\| return`. | Default-mode bash 3.2 to 5.3 and busybox ash 1.35 and 1.37 do not inherit `-e` into `$( )`, while dash, mksh, ksh93u+m, yash, posh, zsh and bash `--posix` do. `x=$(f)` continues on 8 of 8 bash versions, `x=$(set -e; f)` aborts on 8 of 8, a bare `shopt -s inherit_errexit` kills bash 3.2 to 4.2 at the `shopt` line, and `x=$(set -e; f) \|\| echo …` prints `f-continued` on 4.0 to 5.3 because the `\|\|` context wins. `inherit_errexit` does not repair `local x=$(…)`. **Scope:** TOOL, CI on bash, every version, and busybox `sh`. | Cell `esc3` (SC2311) is valid only for `set -e` and `set -eu` headers and silent for `set -eo pipefail` and `set -euo pipefail`, so the check of record is `b08` on a probe that calls the function inside `$( )` and must exit 1. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-33 | A directory of new scripts may opt into SC2312 through a nested `.shellcheckrc` that repeats every root key. Never write a nested rc holding only the SC2312 key. | ShellCheck reads only the nearest rc and never merges, so a one-key nested file silently drops SC2310 for that directory. **Scope:** repos with new-code directories, ShellCheck 0.11.0. | Reading: run `shellcheck -f gcc` from inside the directory on a probe that holds both shapes and read the codes. SC2312 without SC2310 means the rc lost a key. Watched red (measured 2026-10-11). | CONSIDER |

Wrong and right for SH-ERR-08:

```bash
x=$(f)   # wrong on bash and busybox ash: a failing step inside f does not stop the script
x=$(set -e; f)   # right: aborts on 8 of 8 bash versions
```

## Group 3: A Grep Decides

Each cell prints `file:line:text` per hit (or a file name for the `-l` and `-L` cells) and empty stdout and stderr is the
pass. Cells `e12`, `e15`, `e21`, `e25b`, `e25c`, `e26` and `e30` list sites to read, and for them the pass is every hit fixed or
waived. `# tell-ok: REASON` on the same line waives a hit in a cell that filters it, and a bare token with no reason waives
nothing. `LIBSPEC` is the quoted pathspec naming every file that is only sourced (cell `e11`), and a pathspec that matches
nothing scans nothing. Each cell was watched red on a planted file and silent on its compliant twin, with identical output
under GNU grep and busybox 1.37 grep (measured 2026-10-11).

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
shfiles() { files | xargs -0 -r awk 'FNR == 1 && /^#![ ]*\/(usr\/)?bin\/(env )?sh([ \r]|$)/ { print FILENAME }' | tr '\n' '\0'; }
LIBSPEC=':(glob)**/lib/**/*.sh'
# e03
files | xargs -0 -r grep -H -n -E -e '^[[:space:]]*(export[[:space:]]+)?IFS=[$]'"'"'[^'"'"']*'"'"'[[:space:]]*(;|#|$)' --
# e04
files | xargs -0 -r grep -H -n -E -e '[$][(][^)]*[)][^$=]*[$][?]' -- | grep -v -E -e '[|][|][[:space:]]*(return|exit)[[:space:]]+[$][?]'
# e09
{ files; git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml'; } | xargs -0 -r grep -l -E -e 'pipefail' -e 'shell: bash' -- | tr '\n' '\0' | xargs -0 -r grep -H -n -E -e '[|] *grep( +-[a-zA-Z]+)* +-[a-zA-Z]*[qm]' -e '[|] *rg( +-[a-zA-Z]+)* +-[a-zA-Z]*[qm]' -e '[|] *head' -e '[|] *grep( +-[-a-zA-Z=0-9]+)* +--quiet' -e '[|] *grep( +-[-a-zA-Z=0-9]+)* +--silent' -e '[|] *grep( +-[-a-zA-Z=0-9]+)* +--max-count' -- | grep -v -e 'tell-ok: *[^ ]'
# e10
files | xargs -0 -r grep -H -n -E -e '^eval[[:space:]]+"[$][(]' -e '[[:space:];&]eval[[:space:]]+"[$][(]' -e 'cd[[:space:]]+"[$][(]git ' -e 'cd[[:space:]]+"[$][(]realpath ' -e 'cd[[:space:]]+"[$][(]mktemp ' -e '^[[:space:]]*exec[[:space:]].*"[$][(]' -- | grep -v -e 'tell-ok: *[^ ]'
# e11
git ls-files -z --cached --others --exclude-standard -- "$LIBSPEC" | xargs -0 -r grep -H -n -E -e '^[[:space:]]*set[[:space:]]+[-+][a-zA-Z]*[eu]' -e '^[[:space:]]*set[[:space:]]+[-+]o[[:space:]]+(errexit|nounset|pipefail)' --
# e12
files | xargs -0 -r grep -l -E -e 'set -[a-z]*u' -e 'set -o nounset' -- | tr '\n' '\0' | xargs -0 -r grep -H -n -E -e '[^+]"[$][{][A-Za-z_][A-Za-z_0-9]*\[@\][}]"' -e '^"[$][{][A-Za-z_][A-Za-z_0-9]*\[@\][}]"' --
# e14
files | xargs -0 -r grep -H -n -E -e '^[[:space:]]*\(\([[:space:]]*[A-Za-z_][A-Za-z_0-9]*(\+\+|--)[[:space:]]*\)\)[[:space:]]*(#.*)?$' -e '^[[:space:]]*let[[:space:]]+[A-Za-z_][A-Za-z_0-9]*(\+\+|--)([[:space:]]|$)' --
# e15
files | xargs -0 -r grep -l -e mktemp -- | tr '\n' '\0' | xargs -0 -r grep -L -e trap -e teardown --
# e16a
files | xargs -0 -r grep -H -n -E -e "trap[[:space:]]+([A-Za-z_][A-Za-z_0-9]*|'[^']*'|\"([^\"\\\\]|\\\\.)*\")[[:space:]]+(EXIT|0)[[:space:]]+.*(INT|TERM|HUP)" -e "trap[[:space:]]+([A-Za-z_][A-Za-z_0-9]*|'[^']*'|\"([^\"\\\\]|\\\\.)*\")[[:space:]]+.*(INT|TERM|HUP)[[:space:]]+.*(EXIT|0)([[:space:]]|\$)" -- | grep -v -e 'tell-ok: *[^ ]'
# e16b
files | xargs -0 -r grep -H -n -E -e "trap[[:space:]]+'([^']*[;[:space:]])?exit[[:space:]]*;?'" -e "trap[[:space:]]+\"([^\"]*[;[:space:]])?exit[[:space:]]*;?\"" --
# e17
files | xargs -0 -r grep -H -n -E -e '[$][(][^)]*[[:space:];|&(]die[[:space:]]' --
# e18
files | xargs -0 -r grep -H -n -E -e '(^|[^A-Za-z_])exit[[:space:]]+(12[3458]|2[0-9][0-9])([^0-9]|$)' --
# e19
files | xargs -0 -r grep -H -n -E -e '-eq[[:space:]]+"?(129|1[3-5][0-9])"?([[:space:]]|\]|;|$)' -e '[[:space:]](==|=)[[:space:]]+"?(129|1[3-5][0-9])"?([[:space:]]|\]|;|$)' -e '^[[:space:]]*(129|1[3-5][0-9])\)' -- | grep -v -e 'tell-ok: *[^ ]'
# e21
shfiles | xargs -0 -r grep -H -n -E -e '^[[:space:]]*shift[[:space:]]*(;|$)' --
# e25a
files | xargs -0 -r grep -H -n -E -e 'exit[[:space:]]+(66|70)([^0-9]|$)' -- | grep -v -e 'tell-ok: *[^ ]'
# e25b
files | xargs -0 -r grep -l -E -e 'exit[[:space:]]+([2-9]|[1-9][0-9]+)([^0-9]|$)' -- | tr '\n' '\0' | xargs -0 -r grep -L -e '^# Exit' --
# e25c
shfiles | xargs -0 -r grep -H -n -E -e 'err .* ([89]|[1-9][0-9]+)$' --
# e26
files | xargs -0 -r grep -H -n -e 'trap .* ERR' -- | grep -v -E -e 'trap[[:space:]]+-[[:space:]]' -e 'trap[[:space:]]+-p' -e ':[0-9]+:[[:space:]]*#'
# e29
files | xargs -0 -r grep -H -n -E -e '^shopt[[:space:]]+-s[[:space:]].*nullglob' -e '^shopt[[:space:]]+-s[[:space:]].*failglob' --
# e30
files | xargs -0 -r grep -H -n -E -e '^[[:space:]]+shopt[[:space:]]+-s[[:space:]].*nullglob' -e '^[[:space:]]+shopt[[:space:]]+-s[[:space:]].*failglob' --
# e34
files | xargs -0 -r grep -l -E -e 'trap[[:space:]]+[^-].*[[:space:]]RETURN' -- | tr '\n' '\0' | xargs -0 -r grep -L -E -e 'trap[[:space:]]+(--[[:space:]]+)?-[[:space:]]+(.*[[:space:]])?RETURN' -e 'tell-ok: *[^ ]' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ERR-03 | Put no `IFS=$'…'` line in a prologue, and none at all in a `#!/bin/sh` file. | The line has no measured benefit. It changes `read -r k v` (`k=[alpha beta] v=[]`), `"$*"`, `"${a[*]}"` and unquoted splitting on bash 3.2 to 5.3, and on dash 0.5.12, yash 2.52 and posh it assigns the four bytes `$ \ n t`. The popular "unofficial strict mode" line is wrong on this point. **Scope:** all kinds. | Cell `e03` for bash files, cell `esc2` (SC3003) for `#!/bin/sh` files. A scoped `IFS=$'\n' read …` is not a hit. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-04 | Read `$?` first: `rc=0` then `cmd \|\| rc=$?`, or `if ! cmd; then`. Never `cmd` then `if [ $? -ne 0 ]`, and never `echo "$(f) $?"`. | A command substitution in the same command runs first and replaces `$?`, and ShellCheck is silent on that shape (upstream issue 3522, open). **Scope:** all kinds. `\|\| return $?` is the correct propagation and is not a hit. | Cell `esc1` (SC2181) for the first shape, cell `e04` for the second. Watched red (measured 2026-10-11). | SHOULD |
| SH-ERR-09 | Under `pipefail` (explicit, or a workflow step with `shell: bash`), never pipe into an early-exit reader: `grep -q`, `grep -m`, `grep --quiet`, `grep --max-count`, ripgrep in quiet mode, `head`, `sed q`, `awk … exit`. Use `grep -q pat file`, capture first, or let the reader drain (`grep pat >/dev/null`). | The writer dies of SIGPIPE: status 141 (bash, zsh, busybox, mksh, dash 0.5.12), 397 (yash 2.52), 13 or 1 (ksh93u+m), 23 for `curl`. An `echo "$v"` of 10 KiB or more into `grep -q` failed 200 of 200 on bash 3.2 to 5.3 (8 KiB: 0 of 200), and a one-line producer never trips. A negated `! gh … \| grep -q …` answers "settled" when `gh` fails. SC2337 is not in ShellCheck 0.11.0. **Scope:** TOOL, CI on any shell with pipefail. A bare GitHub `run:` is `bash -e {0}` with no pipefail, so only `shell: bash` steps count. | Cell `e09` lists grep, rg and head readers, so judge each hit by producer size. `sed q` and `awk … exit` are reading. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-10 | In installer, CI and dev scripts, never `eval "$(cmd)"`, `cd "$(git …)"` (or `realpath`, `mktemp`) or `exec prog "$(cmd)"`. Capture first (`code=$(cmd)`, `top=$(git rev-parse --show-toplevel)`) so `-e` or `\|\| die` sees the status, then `eval "$code"`, `cd "$top"` or `exec prog "$arg"`. | The substitution is an argument, so `eval ""`, `cd ""` and `exec prog ""` succeed when the producer fails. These were the only strict SC2312 defects in the census of SH-ERR-07. **Scope:** non-interactive TOOL, CI, INST. The one-line `eval "$(tool init sh)"` documented for a user's interactive rc is exempt and carries `# tell-ok: reason`. The latent `"KEY=$(cmd)"` argument shape is not covered, read for it. | Cell `e10`. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-11 | A file that is only sourced sets no shell options (`set -e`, `-u`, `-o …`). One sourced into an interactive shell also never calls `exit`, installs an `EXIT` trap only when none exists (`[ -z "$(trap -p EXIT)" ]`, bash arm only), and probes the caller's flags instead of setting them (`[ "${-#*e}" != "$-" ]`). A library sourced only by scripts you own may define an exiting `die`. Shell options are shared state, and so is `shopt` state (SH-ERR-29). | A library's `set -e; set -u` flips the caller on 11 of 11 POSIX interpreters, bash 3.2 to 5.3 and zsh 5.0.8 to 5.9, and the caller then exits at its next non-zero status. Emitted hooks follow the rule and trap `EXIT` only when it is free. **Scope:** LIB, RC, EMIT. | Cell `e11`, which sees only the paths `LIBSPEC` names, so a file whose header says "sourced" in another directory is read by hand. Runtime: `bp` on a caller that sources `LIB` and exits 1 if its flags changed. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-12 | Under `set -u`, expand a possibly-empty array as `${a[@]+"${a[@]}"}` or behind `[ "${#a[@]}" -gt 0 ]`, unless the header pins bash 4.4. On a 3.2 floor write `"$@"`, never `"${@}"` or `${*}`. Empty `"$@"` needs no guard anywhere except posh 0.14.1, where every forwarding site is `${1+"$@"}`. | `a=(); echo "${a[@]}"` fails with `a[@]: unbound variable` on bash 3.2, 4.0 and 4.2 and passes from 4.4, and the guarded idiom passes on all eight. `"${@}"` and `${*}` with no arguments fail on 3.2 only. A glob-filled array `a=(./*.x)` can be empty and needs the same guard. posh aborts `@: parameter not set` at `parse_args "$@"`. **Scope:** TOOL, CI, LIB that may run on macOS `/bin/bash` 3.2.57 or macOS runners. | Cell `e12` lists candidates with a precision of about 13 percent, so decide emptiness by reading and run the path on a bash 3.2 binary with `b08`. No grep sees emptiness at run time. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-14 | Under `-e`, never use `(( i++ ))`, `(( i-- ))` or `let i++` as a statement: write `i=$((i + 1))` or `(( … )) \|\| true`. Do not rely on a bare `(( ))`, `[[ ]]` or `( … )` statement to stop a script on a 3.2 or 4.0 floor. | `(( i++ ))` from 0 returns 1: bash 4.2 to 5.3 abort and bash 3.2 and 4.0 continue, mksh and zsh abort and ksh93u+m continues. The 3.2 and 4.0 non-abort of `(( ))`, `[[ ]]` and `( false )` is silent divergence. **Scope:** TOOL, CI on bash with `-e`. | Cell `e14`, and `b08` on the script across bash versions. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-15 | A script that creates temp state (`mktemp`, a lock, a staged file) binds one status-preserving `EXIT` handler before creating it: `tmp=` first, then `cleanup() { [ -z "$tmp" ] \|\| rm -rf "$tmp"; }`, then `on_exit() { rc=$?; trap - EXIT; cleanup; exit "$rc"; }`, then `trap on_exit EXIT`, then `tmp=$(mktemp -d)`. Cleanup is safe when the resource is unset and does local work only. | Otherwise every failure leaks the directory. `rc=$?; … exit "$rc"` is mandatory because the last command of an `EXIT` trap overrides the status on mksh. **Scope:** all kinds. | Cell `e15` lists files that call `mktemp` with no `trap` and no bats `teardown`, and any `trap` word passes it, so it is reading-grade. Runtime: `b16` with `--clean-dir`. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-16 | `INT`, `TERM` and `HUP` get their own handlers that end the shell with an explicit status (`trap 'exit 129' HUP`, `trap 'exit 130' INT`, `trap 'exit 143' TERM`), or re-raise (SH-ERR-22). Never bind one returning cleanup function to `EXIT` plus signals, and never end a trap with a bare `exit`. | `trap cleanup EXIT INT TERM HUP` then SIGTERM prints `CLEANUP`, `SURVIVED`, `CLEANUP` and exits 0 on 11 of 11 interpreters and on Git Bash 5.2.15, and the twin exits 143 on 11 of 11. Under `-e` the next failing command or `\|\| err 3` hides it (Ctrl-C becomes status 3). A bare `exit` returns the pre-trap `$?` only on dash, ash, mksh, ksh93u+m 1.0.10, bash 5.3 and yash 2.61, while bash up to 5.2, yash 2.52, ksh93u+m 1.0.4, posh and zsh return the last trap command's status. `EXIT` does not fire on INT or TERM in dash, busybox ash, yash, posh or zsh, and mksh runs it then exits 0. A native Windows parent (PowerShell 5.1, `cmd.exe`, node) sees an explicit `exit 143` as 143 but an untrapped or re-raised death as `signal<<8` (TERM 3840, INT 512, HUP 256, KILL 2304), so only the explicit exit keeps the familiar number for a native consumer. **Scope:** every script that traps, INST, TOOL, CI. A GitHub-hosted windows-2025 runner was not run. | Cells `e16a` and `e16b`, and `b16` for the behaviour. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-17 | Call `die` only in statement position of the main shell (`v=$(f) \|\| die "msg"`, `[ -r "$f" ] \|\| die …`), never inside `$( )`, `( )` or a non-last pipeline stage. | `exit` there leaves only the subshell: `v=$(test -r f \|\| die "no file")` prints the message and the script continues with status 0 on every shell. `exit` in the last stage does leave the script on ksh93u+m, native zsh and bash with `lastpipe` (4.2+), so depend on neither. `local v=$(die)` also masks (SH-ERR-01). **Scope:** all kinds. | Cell `e17`, and `bp` on a probe (the bad form continues with status 0, the good one exits 1). Watched red (measured 2026-10-11). | MUST |
| SH-ERR-18 | Never `exit` 123, 124, 125, 128 or 200 to 255. Use 126 and 127 only with their shell meaning (found but not executable, not found) in a launcher shim. | `xargs` owns 123 to 127 (123 child exited 1 to 125, 124 child exited 255, 125 signalled), `timeout` owns 124 to 127 and 137, shells own 126, 127 and 128+N. `exit 300` is 44 and `exit -1` is 2 on 11 of 11. A 123 that is `xargs`' own status passed through is not an `exit 123`. **Scope:** all kinds. | Cell `e18`. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-19 | Never test for signal death with a literal or hard-code a signal number above 15. Use `[ "$rc" -gt 128 ]` and decode with the running shell's own `kill -l "$rc"`, and a parent accepts both `-N` (killed by signal) and `128+N`. Where the status may come from a native Windows intermediary (PowerShell, cmd, node reporting a Git Bash process) it can be `N*256`, and a bash script decodes both: `if [ "$rc" -ge 256 ] && [ $((rc % 256)) -eq 0 ] && [ $((rc / 256)) -le 64 ]; then kill -l $((rc / 256)); elif [ "$rc" -gt 128 ] && [ "$rc" -le 192 ]; then kill -l "$rc"; fi`. ksh93u+m and yash keep plain `kill -l "$rc"`. The one allowed literal is GNU and uutils `timeout`'s 124 (expired) and 137 (after `-k`), gated on the timeout flavour because busybox `timeout` returns 143. | A child killed by TERM is 143 in dash, busybox ash, mksh, posh, bash and zsh, 271 in ksh93u+m and 399 in yash (PIPE: 141, 269, 397). On MSYS the `kill -l` numbers above 15 differ from Linux (`kill -l 138` is BUS on Git Bash and USR1 on Linux), and only HUP INT QUIT ABRT KILL PIPE ALRM TERM agree. `-gt 128` is true for 256, 512, 3840 and 2304, but `kill -l` rejects them. **Scope:** scripts that must also run on ksh93u+m, yash or Git Bash. | Cell `e19`. Watched red (measured 2026-10-11). | SHOULD |
| SH-ERR-25 | State the exit table in an `# Exit:` header and use the table of the script's kind. A shell CLI that mirrors a Rust tool reuses that tool's table (default: 0, 1, 64, 65, 69, 74, 75, 77 to 82, new codes from 83, usage 64). An installer uses 0 to 7 with usage 2. A new gate or verifier script uses 0 clean, 1 findings, 69 tool missing, 78 config missing, 79 no input (also SH-GATE-08), plus 64 and 65 as needed. New files do not use sysexits 66 or 70 (the 16 sites in the shipped Swift gate scripts are frozen). Anything else: 0 and 1. A `# Exit:` header is required once a script uses any other code. | Three tables exist, and a status above 128 means "signal" only in the shell that reported it (SH-ERR-19). sysexits(3) calls its interface deprecated and discouraged, so it is a vocabulary and not a standard. Pinned decision (adopter may override). **Scope:** all kinds. | Cell `e25a` (every hit outside the frozen sites is a violation), cell `e25b` lists scripts with non-0/1 exits and no header, cell `e25c` lists installer codes above 7. Watched red (measured 2026-10-11). | SHOULD |
| SH-ERR-26 | Use no `trap … ERR` in `#!/bin/sh`. In bash use it only for diagnostics (`set -Eeuo pipefail; trap 'echo "line $LINENO: exit $?" >&2' ERR`), never for cleanup or control flow. | dash, yash and posh reject `ERR` (warning, no trap). Bash's ERR skips `if` and `while` conditions, `\|\|`, `&&`, `!` and `local x=$(false)` (and `( false )`, `(( ))`, `[[ ]]` on 3.2), is not inherited by functions without `-E` and fires twice with `-E`. zsh calls it `ZERR`. **Scope:** TOOL, CI, INST on sh and bash. | Cell `esc2` (SC3047), cell `e26` lists candidates. Watched red (measured 2026-10-11). | SHOULD |
| SH-ERR-29 | Put no `shopt -s nullglob` or `shopt -s failglob` at the top level of a script, in a prologue, or in a sourced library. A top-level region that needs one becomes a subshell. | Both change every glob in the process. Under `nullglob` `ls ./*.x` lists the cwd, `grep p *.x` reads stdin and `find *.x -name '*.dat' -delete` searches `.` and deletes `sub/keep.dat` (busybox `find` measured, GNU `find` documents the same default, unrun). `failglob` is not fail-closed: without `set -e` it skips only the failing command (or the whole `if`, `{ }` group or `&&`/`\|\|` list) and exits 0, and it fires before an `[ -e "$f" ] \|\| continue` guard can run. With both set, `failglob` wins. Identical on bash 3.2, 4.0, 4.2, 4.4 and 5.0 to 5.3. `nullglob` with `set -u` also makes empty arrays routine (SH-ERR-12). **Scope:** TOOL, CI, LIB, RC on bash. direnv's `stdlib.sh` sets it at top level in a library every `.envrc` sources. | Cell `e29` sees column 0 only, and cell `e30` lists the indented hits that SH-ERR-30 judges. No ShellCheck 0.11.0 check flags either (only SC2250, style). Watched red (measured 2026-10-11). | MUST |
| SH-ERR-34 | A bash function that sets `trap … RETURN` ends the action with `trap - RETURN`, for example `f() { trap 'rm -rf "$tmp"; trap - RETURN' RETURN; tmp=$(mktemp -d); }`. A deliberate `set -T` suite waives with `# tell-ok: reason`. | A `RETURN` trap set in a function is shell-global. It survives the function, fires again when any later `.` or `source` finishes, and for every function under `set -T`, and a single-quoted action re-evaluates its variables at the later firing. Behaviour identical on 8 bash versions. pi-hole's `gravity.sh` sets one with `rm -f "${recoverySQL}"` and never resets it, and dokku's archive functions carry 44 sets and 0 resets. **Scope:** TOOL, CI, LIB on bash. | Cell `e34` lists files that set a `RETURN` trap and never reset it or waive. The file-level waiver clause is this cell's translation of the rule's waiver. Watched red (measured 2026-10-11). Confirmed 2026-10-11 on 25 fresh trees: 2 files on 2 trees, both real (`Library/Homebrew/utils/shfmt.sh:367` in Homebrew/brew sets `trap '...' RETURN` and never resets it, `run-tests.sh:4` in jenv). | MUST |

Wrong and right for SH-ERR-16:

```sh
trap cleanup EXIT INT TERM HUP   # wrong: SIGTERM runs cleanup, then the script carries on and exits 0
on_exit() { rc=$?; trap - EXIT; cleanup; exit "$rc"; }
trap on_exit EXIT; trap 'exit 129' HUP; trap 'exit 130' INT; trap 'exit 143' TERM   # right
```

## Group 4: A Behavioural Run Decides

Three scripts under `checks/` do what a grep cannot. Copy them to `scripts/`. Each prints one line per violation, prints
nothing on a pass, and exits 0 clean, 1 violation, 64 usage, 69 no interpreter found, 79 target not found (`errors-run.sh`
also 74 for a temp directory failure). `errors-run.sh` runs a script under every interpreter it finds (dash, bash, ksh,
mksh, yash, posh, busybox ash), or under the commands named after it, each in a new empty directory, and compares the exit
status. `errors-signal.py` needs Python 3.10 and judges how a script ends when it receives HUP, INT or TERM: in `exit`
mode the status must be 128+N, in `raise` mode the script must die of the signal. `errors-leak.sh` needs bash and fails
when a library leaves `nullglob` or `failglob` on. `PROBE` is a small script that runs the construct under test, `WANT` is
the status its correct form returns, `SCRIPT` is the script under test, `CLEAN` is the temp directory it creates, and
`LIB` and `FUNC` name a library and one of its functions. All three scripts were watched red on a plant and silent on a
twin (measured 2026-10-11), and `errors-run.sh` passes dash 0.5.12, busybox ash 1.35 and 1.37, mksh, ksh93u+m, yash,
posh and bash 3.2 `--posix`.

```sh
for t in git xargs grep awk shfmt python3; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# b08: the status of a construct under bash, name more bash binaries after it to cover 3.2
sh scripts/errors-run.sh "$WANT" "$PROBE" bash
# bp: the status of a construct on every POSIX interpreter found, or on the commands named after PROBE
sh scripts/errors-run.sh "$WANT" "$PROBE"
# b20: the failing path exits WANT and writes nothing to stdout
sh scripts/errors-run.sh -q "$WANT" "$SCRIPT"
# b31: the script, run in an empty directory, must exit 79
sh scripts/errors-run.sh 79 "$SCRIPT"
# b16: the exit form, the exit form with a leftover-directory check, then the re-raise form, each repeated for HUP and INT
python3 -I scripts/errors-signal.py exit TERM "$SCRIPT" sh bash
python3 -I scripts/errors-signal.py --clean-dir "$CLEAN" exit TERM "$SCRIPT" sh bash
python3 -I scripts/errors-signal.py raise TERM "$SCRIPT" sh bash
# b30: a library function must leave no glob shopt on
bash scripts/errors-leak.sh "$LIB" "$FUNC"
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ERR-20 | Diagnostics, warnings, progress and `die` messages go to stderr, and stdout carries only the script's product. Define `die` once: `die() { rc=${2:-1}; printf '%s: error: %s\n' "${0##*/}" "$1" >&2; exit "$rc"; }`. | `v=$(tool)` and pipes capture stdout, so a `die` inside `$( )` that writes to stdout lands in the variable. **Scope:** all kinds. | `b20` on the failing path. Watched red (bad: `stdout=[error: oops]`, good: `[]`, measured 2026-10-11). | MUST |
| SH-ERR-21 | Guard `shift`: `if [ "$#" -gt 0 ]; then shift; fi`, or shift only after reading `$1` under `-u`. `shift \|\| true` is not a guard. | With nothing to shift, `shift` is a special-builtin error: dash exits 2 and ksh93u+m exits 1 even with `\|\| true`, while bash, busybox, mksh, yash, posh and zsh print a diagnostic and continue (ShellCheck issue 3444, open). **Scope:** `#!/bin/sh` scripts that parse arguments (INST, EMIT). | `bp` with `WANT` 0 on a probe that shifts with no arguments (the plain and `\|\| true` forms exit 2 on dash and 1 on ksh93u+m, the guarded form 0 everywhere). Cell `e21` lists bare `shift` statements in `#!/bin/sh` files as candidates. Watched red (measured 2026-10-11). | SHOULD |
| SH-ERR-22 | A shell CLI whose parent reads WIFSIGNALED re-raises after cleanup from a function, not inline and not with a hard-coded `exit 130`, and the function resets every trap it installed before it kills: `on_signal() { trap - EXIT HUP INT TERM; cleanup; kill -s "$1" "$$"; }` with `trap on_exit EXIT` and `trap 'on_signal HUP' HUP`, `trap 'on_signal INT' INT`, `trap 'on_signal TERM' TERM`. | The function pattern dies of HUP, INT and TERM on 6 of 6 (dash 0.5.12, busybox ash 1.35 and 1.37, bash 3.2 and 5.3, ksh93u+m 1.0.10). The inline string form exits 1 on ksh93u+m 1.0.10 for HUP and TERM (INT dies of 2), not a signal death. The reset is load-bearing: without it the handler re-enters itself until SIGSEGV on dash (27,565 runs), busybox ash (8,880) and bash 5.3 (5,691), and bash 3.2 ends with `exit 143` after two runs. HUP is not honoured on mksh and posh (129) or zsh (1), so treat only INT and TERM as "die of the signal", and a bash parent cannot tell `exit 130` from SIGINT death. On Git Bash a bash parent sees 143, 130, 129 and a native parent 3840, 512, 256, so use SH-ERR-16's explicit exit where a native consumer matches 128+N. **Scope:** shell CLIs only, not INST or TOOL. | `b16` in `raise` mode for HUP, INT and TERM, plus the reading "the handler resets the traps, then kills with `$1`". Watched red (measured 2026-10-11). | SHOULD |
| SH-ERR-30 | Contain any glob shopt in a subshell (`( shopt -s nullglob; … )`, `f() ( … )`) or a save and restore pair (`shopt -q nullglob && had=1`, later `[ "$had" -eq 1 ] \|\| shopt -u nullglob`). `local -` is not containment. | A function leaves `nullglob` on for the caller on bash 3.2 to 5.3 (8 of 8). `local -` (bash 4.4+) saves the single-letter `set` options only, not `shopt` options, and is an error on 3.2 to 4.2. A subshell function and a save and restore function leave it off (8 of 8). fzf's `completion.bash` contains it inside `<( … )` with the reason in a comment. **Scope:** every bash library or function that globs with a shopt, in LIB, TOOL, CI. | `b30` exits 1 and prints `LEAK: nullglob is on after FUNC`. Discovery is cell `e30`. Watched red (measured 2026-10-11). | MUST |
| SH-ERR-31 | A script that globs for required input gives "nothing matched" its own exit status through an explicit test, never a shopt: `set -- ./*.x` then `if [ ! -e "$1" ] && [ ! -L "$1" ]; then echo "no input files" >&2; exit 79; fi`. Optional input uses `for f in ./*.x; do [ -e "$f" ] \|\| [ -L "$f" ] \|\| continue; …`. The array form is `a=(./*.x)` with the same test on `"${a[0]}"`, never `${#a[@]}` as the emptiness test. A loop over a directory the script itself ships (non-empty by construction) is exempt, and a loop over user data is not. Status 79 is the `NotFound` code of a Rust CLI table and the gate "no input" row. An installer or other kind uses the nearest code of its own `# Exit:` table, never 0 and never the accidental 1. | Default globbing feeds the literal to the next command (accidental failure for `cat` and `rm`, silent success for `rm -f` and `echo`), `nullglob` gives a silent exit 0, `failglob` gives exit 0 plus a message without `set -e`, and `[ -e ]` alone skips a dangling symlink silently (bash 5.3, dash). `${#a[@]}` is 1 for an unmatched pattern. In a corpus of 220 glob-loop candidates 98 were unguarded, 59 guarded and 63 ran under `nullglob`, and only 1 of the 59 guards included the `-L` form. **Scope:** all kinds that glob. | `b31` runs the script in an empty directory on each interpreter and requires 79, with a reading for loops over user data. Watched red (measured 2026-10-11). | MUST |

Wrong and right for SH-ERR-31:

```sh
for f in ./*.x; do process "$f"; done   # wrong: with no match, process runs once on the literal ./*.x
set -- ./*.x; if [ ! -e "$1" ] && [ ! -L "$1" ]; then echo "no input files" >&2; exit 79; fi   # right
```

## Group 5: Reading Heuristics

No tool decides these rows. Each names the reading and the mechanical halves that sit in the groups above.

SH-ERR-23 states the prologue and contract for each kind. Write the one for the script's kind verbatim. This is a pinned
decision (adopter may override).

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ERR-23 | Write the prologue and contract of the script's kind verbatim from the table below. | `-e` is off in the contexts that matter (SH-ERR-06, SH-ERR-08), so the explicit check is the contract. The bootstrap exemplars drop `-e` on purpose: rustup (`set -u` plus `ensure` and `need_cmd`), Homebrew's installer (`set -u` plus `abort`), git's `t/` (`&&` chains and `--chain-lint`) and nvm (sourced). Other installers split about evenly (deno, docker and k3s use `set -e`), so neither form is wrong and the explicit check is what is mandatory. **Scope:** the six kinds below. | Reading, whether a call is fallible is semantic. The mechanical halves are SH-ERR-06, SH-ERR-08, SH-ERR-11, SH-ERR-12 and SH-ERR-29. | MUST |
| SH-ERR-24 | A verifier script whose output is the violation uses `set -u` and no `-e`, and judges each tool by its exit status: grep 0 match, 1 none, 2 or more error, xargs 123, git 128. A canary proves the scan ran. | `grep -q nomatch file` is a normal outcome and aborts a `-e` script. **Scope:** GATE. | Reading: a `scan()`-style wrapper that fails on `rc -gt 1`. | SHOULD |
| SH-ERR-27 | In zsh-targeted code keep `trap … EXIT` out of function bodies, and scope strictness with `setopt local_options err_return`. | Native zsh fires a function-scoped `EXIT` trap at function return, every other shell at shell exit. `err_return` restores on function exit (zsh 5.0.8 to 5.9). **Scope:** zsh scripts and bash code sourced into zsh. `SH-ZSH` owns the zsh family. | Reading. Measured, no grep. | CONSIDER |
| SH-ERR-28 | When a trap must act promptly, wait interruptibly (`cmd & wait $!`) instead of a bare foreground `sleep` or `curl`, and keep cleanup free of network I/O. | A trap runs after the current foreground command finishes on every shell except ksh93, and `wait $!` returns promptly with status above 128. Git Bash 5.2.15 matches Linux bash (`wait` rc 143 on TERM, and a background child ignores SIGINT so `wait` after INT returns 0). GitHub cancel sends SIGINT, then SIGTERM after 7500 ms, then kills the process tree after a further 2500 ms (docs read 2026-10-11), so an `INT` handler has about 7.5 s and a `TERM` handler about 2.5 s. **Scope:** long-running CI scripts. | Reading. Measured, no grep. | CONSIDER |

| Kind | Prologue | Contract |
|---|---|---|
| Bash TOOL or CI script | `#!/usr/bin/env bash` then `set -euo pipefail`, no `IFS` line | `-e` is a net. Explicit `\|\| die` or `\|\| return` at fallible calls, `local x; x=$(…)`, no early-exit reader (SH-ERR-09), no glob `shopt` (SH-ERR-29), and `shopt -s inherit_errexit` only when the header pins bash 4.4 |
| `#!/bin/sh` script | `#!/bin/sh` then `set -eu` | As above. `pipefail` only through the `SH-DIAL` probe, `shift` guarded, no `IFS=$'…'`, no `shopt` (SH-ERR-32) |
| INST (installer, bootstrap) | `#!/bin/sh` then `set -u` (`set -eu` allowed) | Explicit `cmd \|\| err "msg" N` at every fallible call, `main "$@"` last, and the code is not correct only because `-e` fires |
| LIB, RC, EMIT | none, no `exit`, no shared-signal traps, no glob `shopt` | `return` a status, probe the caller's flags (SH-ERR-11), and survive the caller's `set -eu` (an emitted activation snippet had 0 failures on bash 3.2.57 to 5.3.20 and zsh 5.0.8 to 5.9, plain and under `set -eu`) |
| GATE (output is the violation) | `#!/usr/bin/env bash` then `set -u` | No `-e`. See SH-ERR-24, exit table SH-ERR-25 |
| CI `run:` block | none to add on Linux (`bash -e {0}`), `shell: bash` adds `-eo pipefail` | `SH-CI` owns the semantics, and only SH-ERR-09 applies to the body |

## What Agents Get Wrong Here

Ranked by how likely a coding agent emits the form unprompted. Each row has one mechanical check.
1. **`local x=$(cmd)`.** The status is the builtin's, so a failing `cmd` is invisible even under `-e`. `SH-ERR-01`, cell `esc1`.
2. **`trap cleanup EXIT INT TERM HUP`.** A signal runs the cleanup and the script exits 0 on 11 of 11 interpreters. `SH-ERR-16`, cells `e16a` and `e16b`.
3. **Trusting `set -e` inside `if f` and `f \|\| …` and inside `$( )`.** The callee runs with `-e` off, and `x=$(f)` continues on default bash. `SH-ERR-06`, `SH-ERR-08`, cell `esc3`.
4. **`(( i++ ))` under `-e`.** From 0 it returns 1 and kills the script on bash 4.2 to 5.3 but not on 3.2. `SH-ERR-14`, cell `e14`.
5. **`"${args[@]}"` on a possibly-empty array under `set -u`.** It aborts on bash 3.2, 4.0 and 4.2. `SH-ERR-12`, cell `e12`.
6. **A `for f in ./*.x` loop with no emptiness test, or `shopt -s nullglob` in a prologue.** The literal reaches the next command, or `ls` lists the cwd. `SH-ERR-31`, `SH-ERR-29`, cells `e29` and `b31`.
7. **`cmd \| grep -q pat` under `pipefail`.** The writer dies of SIGPIPE and the pipeline reports 141. `SH-ERR-09`, cell `e09`.
8. **`die` inside `$( )` or a subshell.** `exit` leaves only the subshell, so the script goes on. `SH-ERR-17`, cell `e17`.
9. **`set -e` in a sourced library.** It flips the caller's options. `SH-ERR-11`, cell `e11`.
10. **`eval "$(cmd)"`, `cd "$(git …)"` and `IFS=$'\n\t'`.** The first three succeed on an empty argument and the last changes `read` and `"$*"`. `SH-ERR-10`, `SH-ERR-03`, cells `e10` and `e03`.
