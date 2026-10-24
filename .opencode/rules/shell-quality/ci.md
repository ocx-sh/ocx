---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Shell in CI
summary: The SH-CI family. Which shell a GitHub Actions run step gets, untrusted expressions in run bodies, runner files, per-OS runner traps, Taskfile cmds under mvdan/sh, GitLab CI script blocks and the CI gate
---

# Shell in CI

Rows bind to GitHub Actions runner source read 2026-10-11, actionlint 1.7.12, zizmor 1.30.1, ShellCheck 0.11.0,
Task 3.45.4 to 3.54.0 (mvdan/sh 3.14.1 in 3.54.0), GitLab Runner source on main read 2026-10-11, bash 3.2.57 to 5.3,
dash 0.5.12, busybox 1.37, Windows PowerShell 5.1.26100.9444 and pwsh 7.4.7 and 7.6.6 (7.4 end of support
2026-11-10). No real GitHub or GitLab runner was probed: claims that rest on runner source say so. macOS and BSD
claims are `unverified: read only`.

Owns the shell inside a workflow `run:` step or composite action (Linux, macOS, Windows, `container:` jobs), the files
the runner reads back (`$GITHUB_ENV`, `$GITHUB_OUTPUT`, `$GITHUB_PATH`), the shell of a Taskfile `cmds` item and of a
GitLab `script:` block, and the CI gate for template injection. Not owned here: the ShellCheck and shfmt invocation,
actionlint wrapping and the verification dialect are `SH-GATE`. `#!/bin/sh` and bash-floor rules for script files are
`SH-DIAL`. Pipefail and SIGPIPE semantics outside CI are `SH-ERR`. The generic injection program is `SH-SEC`.
PowerShell language rules are `PS-ERR` and `PS-ENC`, copied here only where a workflow edit needs them (`SH-CI-11`,
`SH-CI-12`).

Contents: [Check Scripts](#check-scripts) · [Group 1: Step Shell and Pipefail](#group-1-step-shell-and-pipefail) ·
[Group 2: Untrusted Expressions and the Gate](#group-2-untrusted-expressions-and-the-gate) ·
[Group 3: Runner Files](#group-3-runner-files) · [Group 4: Per-OS and Per-Version Traps](#group-4-per-os-and-per-version-traps) ·
[Group 5: Taskfile cmds](#group-5-taskfile-cmds) · [Group 6: GitLab CI and One Grammar Trap](#group-6-gitlab-ci-and-one-grammar-trap) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defects fixed: the `wf` pathspec printed a git warning on the 8 trees without `.github/workflows` (now `workflow[s]`), `ci-task-pipefail.sh` flagged Go-template pipes, and `ci-pwsh-run-lint.py` crashed on a `strategy:` that is a string. actionlint 1.7.12 and zizmor 1.30.1 ran on the 17 trees with workflows; their output is judged by reading.

## Check Scripts

Copy `checks/` to `scripts/` as the index says and run from the repository root. Every script lists its files from git,
prints `file:line: finding`, and exits 0 clean, 1 finding, 64 bad mode, 69 tool missing, 78 not a git work tree. The Python scripts run as
`python3 -I` and need PyYAML in that interpreter's own site-packages, because `-I` ignores the user site.

| Script | Modes | Rows |
|---|---|---|
| `ci-steps.sh` | pipe, noshell, composite, long, ps51 | the step walker of Group 1 and `SH-CI-11` |
| `ci-taint.sh` | inputs, envctx, outputs, matrix | the `${{ }}` greps of Group 2 |
| `ci-job-shell-lint.py`, `ci-pwsh-run-lint.py` | none | container and macOS steps, pwsh native guards |
| `ci-task-pipefail.sh` | pipeline, shopt | Taskfile pipefail scope |
| `ci-extract-lint.sh` | gitlab, gitlab-bash, taskfile | ShellCheck over YAML-embedded bodies, with `ci-gl-extract.py` and `ci-tf-extract.py` |
| `ci-win-task-lint.py` | none, `--all` | Taskfile commands absent from a plain Windows PATH |
| `ci-orand.sh` | none | the `A \|\| B && C` grammar trap |

## Group 1: Step Shell and Pipefail

Cells SH-CI-01, 02, 06 and 22 are `ci-steps.sh` modes. The other cells print sites, and a pass is an empty output.
`wf` lists workflow files top-level only (`SH-CI-31`).

```sh
for t in git xargs grep awk actionlint; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
wf() { git ls-files -z --cached --others --exclude-standard -- ':(glob).github/workflow[s]/*.yml' ':(glob).github/workflow[s]/*.yaml'; }
# SH-CI-01, SH-CI-02, SH-CI-06, SH-CI-22
sh scripts/ci-steps.sh pipe
sh scripts/ci-steps.sh noshell
sh scripts/ci-steps.sh composite
sh scripts/ci-steps.sh long
# SH-CI-03: each step run by sh, then actionlint at default severity on the same files
wf | xargs -0 -r grep -H -n -E -e '^[[:space:]]*(-[[:space:]]+)?shell:[[:space:]]*sh[[:space:]]*$' --
wf | xargs -0 -r actionlint -oneline --
# SH-CI-05: a path-qualified shell is the violation, the second line lists every template to read for -o pipefail
wf | xargs -0 -r grep -H -n -E -e 'shell:[[:space:]]*/' --
wf | xargs -0 -r grep -H -n -E -e 'shell:[^#]*[{]0[}]' --
# SH-CI-16: an early-exit reader after a pipe
wf | xargs -0 -r grep -H -n -E -e ' [|] +(grep -[A-Za-z]*q|head)([^A-Za-z]|$)' --
# SH-CI-31: recursive enumeration of workflow files in scripts and tasks
git ls-files -z --cached --others --exclude-standard -- '*.sh' '*.bash' '*.yml' '*.yaml' | xargs -0 -r grep -H -n -E -e 'grep -[A-Za-z]*[rR][^|;&]*[.]github/workflows' -e 'find[[:space:]]+[.]github/workflows' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CI-01 | Give a `run:` step that contains a pipeline `shell: bash`, inherit `defaults.run.shell: bash`, or start its body with `set -o pipefail`. | An unspecified shell is `bash -e {0}` with no pipefail, so `a \| b` hides a failing `a`: the default-shell step printed REACHED and exited 0, the `shell: bash` twin exited 1. `shell: bash` is `bash --noprofile --norc -e -o pipefail {0}`. Scope: bash on Linux and macOS runners, all versions. Windows defaults to pwsh and is not covered. | Cell SH-CI-01 (`ci-steps.sh pipe`). A file with any `defaults.run.shell` is skipped whole, and a pipeline in a default-pwsh Windows step is a false positive. Watched red (measured 2026-10-11). | MUST |
| SH-CI-02 | Declare the shell once: `defaults.run.shell: bash` at workflow level, at job level when the job has Windows legs, and an explicit `shell: pwsh` on PowerShell steps. | With no declaration the same `run:` is `bash -e` on Linux and macOS, pwsh on Windows and `sh -e` in a container, and defaults do not reach composite steps (`SH-CI-06`). Homebrew sets `defaults.run.shell: bash -xeuo pipefail {0}`. 482 fleet steps would be flagged, so this is advisory. Scope: all runners. | Cell SH-CI-02 (`ci-steps.sh noshell`). Watched red (measured 2026-10-11). | SHOULD |
| SH-CI-03 | Write a step body in the dialect of its shell. Under `shell: sh` use no bash syntax and enable pipefail only with the one-line probe `(set -o pipefail) 2>/dev/null && set -o pipefail`, preceded by `# shellcheck disable=SC3040 # probed in a subshell first: dash 0.5.12-2 and posh abort on an unknown set -o`. A step that depends on pipefail uses `shell: bash`. | `sh` is dash on the Ubuntu runners. dash 0.5.12-6ubuntu5 (ubuntu-24.04) exits 2 on bare `set -o pipefail`, 0.5.12-12ubuntu3 (ubuntu-26.04) accepts it, and the probe leaves pipefail off on dash 0.5.12-6ubuntu5 and posh 0.14.1 (`false \| true` then REACHED, exit 0). (also SH-DIAL-07, SH-DIAL-08) Scope: `shell: sh`, until the `ubuntu-latest` move to 26.04 completes 2026-11-19. | Cell SH-CI-03: the grep lists each `sh` step (a matrix value named `shell: sh` is a false positive), actionlint reports SC3010, SC3030 and SC3040 under the `sh` dialect. Watched red (measured 2026-10-11). | MUST |
| SH-CI-05 | Write a custom shell template as a bare interpreter name, `{0}` and for bash `-o pipefail`: `bash -euo pipefail {0}`. Never a path (`/bin/bash -eo pipefail {0}`), never without `{0}`. Known-good shims are `msys2 {0}` and `bash -euo pipefail {0}`. | actionlint 1.7.12 skips ShellCheck on every custom-shell body and silently on a path-qualified one, the runner throws when `{0}` is missing, and a template replaces `--noprofile --norc -e -o pipefail`, so they are gone unless written. `msys2 {0}` is `bash.exe -leo pipefail`. `-u` is the one thing `shell: bash` lacks. Scope: bash templates, all runners. | Cell SH-CI-05: the first line is the violation, the second lists templates to read for `-o pipefail`. Watched red (measured 2026-10-11). | MUST |
| SH-CI-06 | Give every `run:` step of a composite `action.yml` its own `shell:` (default `bash`) and take `inputs.*` through `env:`. | Composite run steps require `shell`, job and workflow defaults are skipped inside composites, and actionlint does not lint `action.yml` (it exits 1 with a missing `jobs` error). Scope: all runners. | Cell SH-CI-06 (`ci-steps.sh composite`) for the shell, cell SH-CI-07 for the inputs. Watched red (measured 2026-10-11). | MUST |
| SH-CI-16 | When a step gains pipefail (`SH-CI-01`, `SH-CI-02`), rewrite `producer \| grep -q` and `producer \| head -n` as capture-then-test. | The early-exit reader closes the pipe, the producer gets SIGPIPE and the pipeline status is 141 on bash 3.2 to 5.3 under pipefail, so `if producer \| grep -q X; then exit 1; fi` silently stops firing. ShellCheck SC2337 covers it only on unreleased master. (also SH-ERR-09) Scope: bash 3.2 to 5.3 with pipefail. | Cell SH-CI-16. Watched red (measured 2026-10-11). | SHOULD |
| SH-CI-22 | Move a `run:` block into a script file at 10 or more lines or on any function, trap, `case`, `mapfile` or `declare -A`, and keep 1 to 9 lines of glue inline. Pinned decision (adopter may override): the 10-line threshold. | ShellCheck and shfmt see script files, local reproduction needs one, and `run:` has a 21,000-character ceiling. Scope: all workflows. | Cell SH-CI-22 (`ci-steps.sh long`). A multi-command loop is a reading step. Watched red (measured 2026-10-11). | SHOULD |
| SH-CI-31 | Enumerate workflow files top-level only, with the `wf` pathspec form (`:(glob).github/workflow[s]/*.yml` and `*.yaml`), and give actionlint that file list. Never `grep -r`, or `find` without `-maxdepth 1`, over `.github/workflows`. | GitHub reads only the top level, but a recursive walk reads data YAML under it: a 5,000-line prompt-log file gave actionlint 6 syntax errors and extra hits for the grep checks. The bracket `[s]` keeps the literal directory out of the pathspec: git 2.43 to 2.54 prints `warning: could not open directory '.github/workflows/'` on a repo without that directory, and a cell must end with empty stderr (SH-GATE-26). (also SH-GATE-21) Scope: all repos with workflows. | Cell SH-CI-31 lists recursive enumeration to fix. Watched red and green (confirmed 2026-10-11 on 25 fresh trees, 24 of 24 workflow files listed on Homebrew/brew, empty stderr on the 8 trees with no workflow directory). | SHOULD |

```yaml
# wrong: no shell:, so bash -e {0} without pipefail, and a failing terraform exits 0
- run: terraform plan | tee plan.log
# right
- shell: bash
  run: terraform plan | tee plan.log
```

## Group 2: Untrusted Expressions and the Gate

Cells SH-CI-07, 08, 23 and 24 are `ci-taint.sh` modes over workflow files and every `action.yml`. A `key: value` line
other than `run:` is skipped, which is how the safe `env:` form passes. Cells SH-CI-23 and 24 skip a file whose header
says it is generated (fix the generator).

```sh
for t in git xargs awk zizmor; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# SH-CI-07, SH-CI-08, SH-CI-23, SH-CI-24
sh scripts/ci-taint.sh inputs
sh scripts/ci-taint.sh envctx
sh scripts/ci-taint.sh outputs
sh scripts/ci-taint.sh matrix
# SH-CI-21: presence of the injection gate, the warning-floor override, the tool itself
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' | xargs -0 -r awk '/zizmor/ { f = 1 } END { if (!f) print "SH-CI-21: no zizmor in any YAML file" }'
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.sh' | xargs -0 -r grep -H -n -F -e 'severity=warning' --
zizmor --min-severity medium .
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CI-07 | Never put an attacker-reachable `${{ }}` expression in a `run:` or `script:` body: set it under `env:` and read `"${VAR}"` (bash) or `$env:VAR` (pwsh). The set is `github.event.*.{title,body,message,name,full_name,email,ref,label,default_branch}` (so `github.event.pull_request.head.repo.full_name`), `github.event.pages.*`, `github.event.client_payload.*`, `github.head_ref`, `github.ref_name`, `inputs.*`, `github.event.inputs.*`, every `steps.*.outputs.*` and `needs.*.outputs.*` (`SH-CI-23`) and every dynamic `matrix.*` (`SH-CI-24`). Multi-line `prepare:` and `run:` inputs of VM actions count. Beyond the set route every value through `env:`, but literal-matrix, sha and runner values are not findings. | The expansion happens before the script exists, so the value is code: canary files were created for `github.event.issue.title` and `inputs.release_tag` (bash and pwsh) and not for the `env:` twins. actionlint 1.7.12 flags only the `github.event.*` group and zizmor 1.30.1 flags `inputs.*` at `--min-severity high`, so this cell is the complete check. Scope: every shell, every runner, composite `action.yml` too. | Cell SH-CI-07 (`ci-taint.sh inputs`). A one-line `script: "${{ inputs.x }}"` is out of reach by construction (zizmor is the second check), and continuation lines of a non-shell action's `with:` block match and are read in context. Watched red (measured 2026-10-11). | MUST |
| SH-CI-08 | Never write `${{ env.X }}` in a run body: write `${X}` (bash) or `$env:X` (pwsh). | `${{ env.X }}` is still a template expansion, so a value placed under `env:` is not sanitised by being read through the `env` context (a canary was created). actionlint is silent and zizmor reports it only at `--min-severity low` and below. Scope: every shell, every runner. | Cell SH-CI-08 (`ci-taint.sh envctx`). A hit inside a `with:` block of a non-shell action is not a violation, and a hit whose variable is defined only by literals or `secrets.*` is a style finding, so read the `env:` definition. Watched red (measured 2026-10-11). | MUST |
| SH-CI-23 | Never write `${{ steps.ID.outputs.NAME }}` or `${{ needs.JOB.outputs.NAME }}` in a `run:` or `script:` body: set it under `env:`. Authored files only, a generated file is fixed in its generator. | An output is a string pasted verbatim, so the taint of its source survives the hop, even a first hop written the safe way (`env:` in job a, then `needs.a.outputs.title` in job b: the payload ran). actionlint is silent and zizmor lists outputs only at informational, so the gate's medium floor never shows them. The rule is stated on the syntax because an agent cannot decide cross-job taint and the fix is one `env:` line. Scope: every shell, every runner, composite too. | Cell SH-CI-23 (`ci-taint.sh outputs`). Same one-line `script:` blind spot as `SH-CI-07`. Watched red (measured 2026-10-11). | MUST |
| SH-CI-24 | When a matrix is built from an expression (`fromJSON(needs.*.outputs.*)`, `steps.*`, `github.event.*` or `inputs.*`, as the whole `matrix:`, a dimension value or a list item), never write `${{ matrix.DIM }}` or `${{ matrix.DIM.FIELD }}` in a run body: pass it through `env:`. Literal matrices are exempt. | The dimension's elements are output strings. zizmor 1.30.1 flags the bare `matrix.item` but not the dotted `matrix.item.version`, which executed. A literal `item: [alpha, beta]` is constants. On each zizmor release re-run the dotted plant and narrow this row to a Limits note if it is fixed. Scope: all runners, authored files. | Cell SH-CI-24 (`ci-taint.sh matrix`) lists every `matrix.*` run-body line of a dynamic-matrix file, static dimensions included, and the reader keeps the dynamic one. Watched red (measured 2026-10-11). | MUST |
| SH-CI-21 | Run a template-injection check in the workflow gate: zizmor in its default `regular` persona at `--min-severity medium` (add `--offline` where no token exists), plus cells SH-CI-08, SH-CI-23 and SH-CI-24, and actionlint at default severity, never `-S` and never `SHELLCHECK_OPTS` with `--severity=warning`. Pinned decision (adopter may override): persona and floor. | actionlint misses `inputs.*`, `${{ env.X }}`, outputs, matrix values, pwsh bodies and composite actions, and zizmor at medium covers `inputs.*` but misses the env context, shows outputs only at informational and misses dotted matrix access at every level. Pedantic and auditor give 140 findings against 18 at low, every added line a constant or a smell. The warning floor hides SC2086. (also SH-GATE-01, SH-GATE-19) Scope: every repo with `.github/workflows`. The medium floor is attainable only on SHA-pinned actions. | Cell SH-CI-21: the presence awk prints when no YAML file names zizmor, the grep lists the floor override, and the zizmor run exits 11 to 14 on a finding. The component cells are watched red, tool runs measured with actionlint 1.7.12 and zizmor 1.30.1 (2026-10-11). | MUST |

```yaml
# wrong: the title is code before the shell starts
- run: echo "${{ github.event.issue.title }}"
# right: the shell reads an environment variable
- env:
    TITLE: ${{ github.event.issue.title }}
  run: echo "${TITLE}"
```

## Group 3: Runner Files

```sh
for t in git xargs grep awk; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
wf() { git ls-files -z --cached --others --exclude-standard -- ':(glob).github/workflow[s]/*.yml' ':(glob).github/workflow[s]/*.yaml'; }
# SH-CI-09
wf | xargs -0 -r grep -H -n -E -e '::(set-output|set-env|add-path|save-state)( |::)' --
# SH-CI-10: every fixed delimiter, read the value's source
wf | xargs -0 -r grep -H -n -E -e 'echo .[A-Za-z_][A-Za-z0-9_]*<<[A-Za-z0-9_-]+' --
# SH-CI-11: shell: powershell steps, then .ps1 files that write a runner file
sh scripts/ci-steps.sh ps51
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -i -E -e '(Add|Set)-Content[^|]*GITHUB_(PATH|ENV|OUTPUT|STATE)' -e '>>[[:space:]]*[$]env:GITHUB_(PATH|ENV|OUTPUT|STATE)' -e 'Out-File[^|]*GITHUB_(PATH|ENV|OUTPUT|STATE)' -- | grep -v -e '-Encoding utf8 -Append'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CI-09 | Do not emit `::set-output`, `::set-env`, `::add-path` or `::save-state`: write to `$GITHUB_OUTPUT`, `$GITHUB_ENV`, `$GITHUB_PATH` and the state file. | `::set-env` and `::add-path` throw unless `ACTIONS_ALLOW_UNSECURE_COMMANDS` is set, and `::set-output` and `::save-state` are deprecated behind a server flag with no published removal date. Training data still emits all four. Scope: all shells and runners. | Cell SH-CI-09. Watched red (measured 2026-10-11). | MUST |
| SH-CI-10 | Write a multiline value to `$GITHUB_ENV` or `$GITHUB_OUTPUT` with a random delimiter, end the value with a newline before the closing delimiter, and never put a value that may contain a newline into a one-line `name=value`. Arbitrary data goes to a file. | The runner ends the block at the first line equal to the delimiter, so a fixed `EOF` with attacker text sets extra variables (a parser port set `INJECTED=pwned`). Scope: bash, sh and pwsh steps, all runners. | Cell SH-CI-10 lists every fixed delimiter. A fixed delimiter around constant text is not a finding, so read the value's source. Watched red (measured 2026-10-11). | MUST |
| SH-CI-11 | Code that can run under Windows PowerShell 5.1 writes `$GITHUB_ENV`, `$GITHUB_PATH`, `$GITHUB_OUTPUT` and `$GITHUB_STATE` with exactly one of two writers: `[IO.File]::AppendAllText($env:GITHUB_ENV, "NAME=value" + [Environment]::NewLine, (New-Object System.Text.UTF8Encoding $false))` (the default, no BOM) or `"NAME=value" \| Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append`. Never `>>`, `Out-File` without `-Encoding utf8`, `-Encoding unicode` or `Add-Content`, and never mix a safe writer with a banned one on one file. | On 5.1 `>>` writes UTF-16LE with a BOM. The runner decodes a lone `>>`, but `>>` mixed with `Out-File -Append` leaves garbage lines, and `Add-Content` writes the ANSI code page, so `C:\Users\Jürgen\bin` reads back with U+FFFD. `Out-File -Encoding utf8` writes its BOM only into an empty file. 7 of 21 measured sequences corrupt, all with a banned writer. pwsh 7.x writes UTF-8 without BOM for every writer and `>>` is safe there. (also PS-ENC-10) Scope: Windows PowerShell 5.1 (`shell: powershell`, or a `.ps1` started by `powershell.exe`). | Cell SH-CI-11 (`ci-steps.sh ps51` for steps, the grep for `.ps1` and `.psm1` files, hits are banned writers). Watched red (measured 2026-10-11). | MUST |

```yaml
# wrong: fixed delimiter, a line "EOF" inside the body ends the value early
- run: |
    { echo "notes<<EOF"; echo "${BODY}"; echo "EOF"; } >> "$GITHUB_OUTPUT"
# right: random delimiter
- run: |
    delim="ghadelim_$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
    { echo "notes<<${delim}"; echo "${BODY}"; echo "${delim}"; } >> "$GITHUB_OUTPUT"
```

## Group 4: Per-OS and Per-Version Traps

```sh
for t in git xargs grep awk python3; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
python3 -I -c 'import yaml' || echo "PREFLIGHT: PyYAML missing for python3 -I"
wf() { git ls-files -z --cached --others --exclude-standard -- ':(glob).github/workflow[s]/*.yml' ':(glob).github/workflow[s]/*.yaml'; }
# SH-CI-04, SH-CI-13
python3 -I scripts/ci-job-shell-lint.py
# SH-CI-12
python3 -I scripts/ci-pwsh-run-lint.py
# SH-CI-14: keep hits inside a bash step
wf | xargs -0 -r grep -H -n -E -e '(^|[ ;(])cmd /[cCkK] ' --
# SH-CI-15: exposed jobs, then GNU sniffing
wf | xargs -0 -r grep -H -n -F -e 'ubuntu-latest' --
git ls-files -z --cached --others --exclude-standard -- '*.sh' '*.bash' '*.yml' '*.yaml' | xargs -0 -r grep -H -n -F -e '--version | grep' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CI-04 | In a job with `container:` set `shell: bash` on every step whose body uses bash syntax (the image must then contain bash), or write the body in POSIX sh. | The runner picks `sh -e {0}` inside the image when no shell is given. On debian:12 an `if [[ ... ]]` prints `[[: not found` and skips the branch while the step passes (the failing test sits in an `if`, so errexit never trips), and alpine's busybox ash accepts `[[`, so the same body differs per image. Scope: GitHub-hosted and self-hosted Linux `container:` jobs, Debian and Ubuntu images (dash). | Cell SH-CI-04 (`ci-job-shell-lint.py`, one line per step). A regex heuristic: a bash construct hidden behind `eval` is missed. Watched red (measured 2026-10-11). | MUST |
| SH-CI-12 | In a `shell: pwsh` or `shell: powershell` `run:` step, and in any composite-action step, with two or more native commands, test `$LASTEXITCODE` after each native call and `exit` or `throw` on non-zero, or on pwsh 7.x set `$PSNativeCommandUseErrorActionPreference = $true` first, scoping it to `$false` inside `& { }` for tools whose non-zero code is information (robocopy). A custom `shell: pwsh -File {0}` removes the runner's prelude and epilogue, so that script sets Stop and exits itself. | The runner prepends `$ErrorActionPreference = 'stop'` and appends a `$LASTEXITCODE` check only for the last command, so a failing build followed by a passing test is step exit 0 on 5.1, 7.4.7 and 7.6.6. The preference variable is a no-op on 5.1. (also PS-ERR-03) Scope: Windows and Linux runners, the preference alternative is pwsh 7.6 only. | Cell SH-CI-12 (`ci-pwsh-run-lint.py`, one line per unguarded step, default-shell Windows steps included). Watched red (measured 2026-10-11). Confirmed 2026-10-11 on 25 fresh trees: 17 steps on 3 trees (wailsapp/wails, for example `build-and-test.yml` job `test_templates` step 6, two natives and no guard). A workflow whose `strategy:` is a whole expression (`${{ fromJSON(...) }}`, docker-library/official-images) crashed the script with `AttributeError`, and a non-string `run:` would have too. Both are fixed and the plant is red. | MUST |
| SH-CI-13 | Write a `run:` body on a `macos-*` runner (bash default or `shell: bash`) in bash 3.2.57 syntax: no `declare -A` or `declare -g`, `mapfile`, `readarray`, `${x,,}`, `${x^^}`, `${x^}`, `${x,}`, `${x@Q}`, `[[ -v X ]]`, `\|&`, `;;&`, `&>>`, `wait -n`, `$EPOCHREALTIME`, `local -n`, `shopt -s globstar`, `coproc`, `printf '%(..)T'`. Alternatively install a newer bash and call it by absolute path. | macOS runners ship bash 3.2.57 and no newer bash on PATH, and Linux CI cannot see the failure: 20 constructs fail on a bash 3.2 binary and an associative array there is silently wrong with exit 0. BSD userland on the runner: unverified: read only. (also SH-DIAL-06, SH-DIAL-25) Scope: `macos-*` runners with bash 3.2.57. | Cell SH-CI-13 (`ci-job-shell-lint.py`), the macos test covers `runs-on` and `strategy` text so a matrix leg counts. Watched red (measured 2026-10-11). | MUST |
| SH-CI-14 | In `shell: bash` steps on `windows-*` runners call Windows tools as `cmd //c` (conversion on) or `MSYS_NO_PATHCONV=1 cmd /c` (conversion off), never both, pass native tools `cygpath -w` paths, and do not rely on Git's Unix tools from pwsh or cmd steps beyond what the image's PATH shows. | Git Bash rewrites Unix-looking arguments: `cmd /c` loses its flag and `/tmp/x` and `--prefix=/usr/local` are converted. With `MSYS_NO_PATHCONV=1` the `//c` form prints the cmd banner. Hosted images append Git's `mingw64\bin` and `usr\bin` after System32 (source read, unverified on a runner), so `grep`, `sed`, `uname` resolve and `find`, `sort`, `timeout`, `tar` stay the Windows programs. Scope: measured on Git Bash 5.2.15, the runner has 5.3.15. | Cell SH-CI-14, keep hits inside a bash step. Watched red (measured 2026-10-11). | SHOULD |
| SH-CI-15 | Until the `ubuntu-latest` move to 26.04 completes (rollout 2026-10-19 to 2026-11-19, rule expires 2026-11-19), make a shell-heavy job correct on ubuntu-24.04 and ubuntu-26.04: pin `ubuntu-24.04` or add a 26.04 leg, and detect features, never `--version \| grep GNU`. | 24.04 has bash 5.2.21, dash 0.5.12-6ubuntu5 (no pipefail) and ShellCheck 0.9.0, 26.04 has bash 5.3.9, dash 0.5.12-12ubuntu3 (pipefail) and ShellCheck 0.11.0. The plain `ubuntu:26.04` image ships uutils coreutils 0.10.0 (GNU not detected), whether the runner image does is unverified. Scope: `ubuntu-latest`, remove after the window and re-check the image READMEs. | Cell SH-CI-15 lists exposed jobs and GNU sniffing, a listing and not a red check. | SHOULD |

## Group 5: Taskfile cmds

Task runs `cmds` and `status:` in mvdan/sh with errexit on and pipefail off, each `cmds` item in its own interpreter
(task 3.45.4 to 3.54.0). `tf` lists Taskfiles outside `testdata/`.

```sh
for t in git xargs grep awk python3 shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
python3 -I -c 'import yaml' || echo "PREFLIGHT: PyYAML missing for python3 -I"
tf() { git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Taskfile*.yml' ':(glob)**/taskfile.yml' ':(glob)**/*.taskfile.yml' ':(exclude,glob)**/testdata/**'; }
# SH-CI-17: shopt lists, then pipefail as its own item
sh scripts/ci-task-pipefail.sh shopt
tf | xargs -0 -r grep -H -n -E -e '^[[:space:]]*-[[:space:]]+set[[:space:]]+-[A-Za-z]*o[[:space:]]+pipefail[[:space:]]*$' --
# SH-CI-32
sh scripts/ci-task-pipefail.sh pipeline
# SH-CI-18
tf | xargs -0 -r grep -H -n -E -e 'printf[[:space:]]+-v' -e PIPESTATUS -e BASH_VERSION -e BASH_SOURCE --
tf | xargs -0 -r grep -H -n -E -e '[A-Za-z_]+\[[A-Za-z0-9_]*-[A-Za-z0-9_-]*\]' --
# SH-CI-19
sh scripts/ci-extract-lint.sh taskfile
# SH-CI-20, SH-CI-25
python3 -I scripts/ci-win-task-lint.py
python3 -I scripts/ci-win-task-lint.py --all
# SH-CI-26, SH-CI-27
tf | xargs -0 -r grep -H -n -E -e '[{][{][^}]*(FILE_PATH_SEPARATOR|PATH_LIST_SEPARATOR|joinEnv|fromSlash)' --
tf | xargs -0 -r grep -H -n -E -e 'platforms:[^#]*!' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CI-17 | In Taskfile `cmds` enable pipefail with `set: [pipefail]` (Taskfile, task or cmd level) or `set -o pipefail;` inside the same cmd item. Never as a separate cmd item, never as `shopt: [pipefail]`. Repeat it inline inside `status:` commands. | Each cmd item is its own interpreter, so `- set -o pipefail` as its own item does not carry over (the next item survived with rc 0). `shopt: [pipefail]` panics (`index out of range [-1]`) on 3.45.4 and errors on 3.51.1 and later, and `status:` ignores `set:` (go-task/task#2469, open). Scope: task 3.45.4 to 3.54.0. | Cell SH-CI-17 (`ci-task-pipefail.sh shopt` plus the grep for the separate item). Watched red (measured 2026-10-11). | MUST |
| SH-CI-32 | A Taskfile `cmds`, `status` or `sh:` item that contains a pipeline runs with pipefail in scope: `set: [pipefail]` at Taskfile, task or cmd level, or `set -o pipefail;` inside the item. `SH-CI-17` says how, this row says when. | Task runs errexit on and pipefail off, so `cargo test 2>&1 \| tee out.log` hides a failing producer. Pipefail does not help inside `$(...)` in a test (`[ $(git status --porcelain=2 \| wc -l) = 0 ]`). Scope: task 3.45.4 to 3.54.0. | Cell SH-CI-32 (`ci-task-pipefail.sh pipeline`). A file-level approximation: a file naming pipefail anywhere is skipped, so read the files it skips. A Go-template pipe inside `{{ }}`, on one line or several, is not a shell pipeline and is not flagged. Watched red and green (confirmed 2026-10-11 on 25 fresh trees): 58 hits on 2 trees (57 in wailsapp/wails, 1 in outline-server), each read and a real pipeline in a `cmds` or `sh:` item. The first run also flagged a multi-line `{{printf ... \| get (dict ...)}}` on outline-server, which was the cell defect fixed here. | SHOULD |
| SH-CI-18 | In Taskfile `cmds` use no `printf -v`, `PIPESTATUS`, `BASH_VERSION`, `BASH_SOURCE`, and no `-` in an associative-array subscript in a compound assignment (use a `case`). | mvdan/sh returns silently wrong values for the first four (`printf -v` writes `-vprintf-v=[]`, `PIPESTATUS` and `BASH_VERSION` are unset, rc 0) and panics on a dashed associative subscript, even quoted in a compound assignment. Plain keys, arrays, `[[`, `declare -A`, `mapfile` and `read -p` work. Scope: task 3.45.4 to 3.54.0. Re-run on each task bump, no upstream fix as of 3.54.0. | Cell SH-CI-18, two greps. Known false positives: regex character classes, and a function task never runs. Watched red (measured 2026-10-11). | MUST |
| SH-CI-19 | Lint Taskfile `cmds` bodies through the extractor into ShellCheck (bash dialect, default severity, excluding SC1091, SC2050, SC2194, SC2154, SC2157, SC2043 and SC2234), because the shell gate sees only `*.sh`. A reasoned `# shellcheck disable=` inside a body is allowed only for SC1091 (`# path is computed at runtime from VAR; checked as FILE in its own run`) and SC2016 (`# single quotes are deliberate: the $ is for the target shell`). | Nothing else reads YAML-embedded task bodies. The extractor covers `cmds`, `status`, `defer`, `preconditions[].sh` and `vars.*.sh`. ShellCheck cannot see the three mvdan/sh divergences of `SH-CI-18`, so this complements that rule. (also SH-GATE-07) Scope: task files with bash-style `cmds`. | Cell SH-CI-19 (`ci-extract-lint.sh taskfile`). Watched red (measured 2026-10-11). | SHOULD |
| SH-CI-20 | A Taskfile task that a `task NAME` call in a Windows-runner step whose shell is not bash (pwsh, cmd, the Windows default) reaches uses only mvdan/sh builtins, the Task 3.45.3+ builtin coreutils (`base64 cat chmod cp find gzip ls mkdir mktemp mv rm shasum tar touch xargs`), tools on that host's PATH, or carries `platforms:` per cmd or task. On a plain Windows PATH never `uname sed grep awk head tail wc cut tr date env which sha256sum dirname basename tee sh`. Promote to MUST for a self-hosted Windows host whose PATH lacks Git's Unix directories. | Task resolves every command that is not a builtin from the PATH it inherited: from a cmd.exe parent 38 of 68 probed names are absent (`"uname": executable file not found`, rc 201). The builtin `find` and `ls` are u-root, not GNU, and `sort` and `timeout` are Windows programs. A GitHub-hosted image installs Git with `PathOption=CmdTools`, which appends `mingw64\bin` and `usr\bin`, so the same Taskfile passes there (source read, unverified on a runner), hence SHOULD. Scope: Task 3.45.3 to 3.54.0, Windows 10.0.26200 measured. | Cell SH-CI-20 (`ci-win-task-lint.py`). On a hosted runner a hit is advisory for the GNU names and real for `find`, `sort`, `timeout` and `tar`. Watched red (measured 2026-10-11). | SHOULD |
| SH-CI-25 | A Taskfile that a Windows developer may run gates commands absent from a plain Windows PATH with `platforms:`, or calls them through a script file run by an explicit shell. | `SH-CI-20` sees only tasks a CI step reaches, and a developer on cmd.exe or pwsh without Git's Unix directories hits the same `exit status 127`. Task's embedded coreutils shrink the absent set with each release and are not GNU. Scope: as `SH-CI-20`, re-run the 68-name probe on each task bump. | Cell SH-CI-25 (`ci-win-task-lint.py --all`), a list, each hit is read. Watched red (measured 2026-10-11). | SHOULD |
| SH-CI-26 | In Taskfile `cmds` do not interpolate `{{.FILE_PATH_SEPARATOR}}`, `{{.PATH_LIST_SEPARATOR}}`, `{{joinEnv ...}}` or `{{fromSlash ...}}` unquoted, and never write a literal backslash path: use `{{.ROOT_DIR}}` or `{{toSlash ...}}` (forward slashes) or single-quote the value. | Task pastes the value into the command text and mvdan/sh then parses it, so the backslash is an escape and `;` a separator: on Windows `echo SEP=[{{.FILE_PATH_SEPARATOR}}]` prints `SEP=[]` and `{{fromSlash "a/b/c"}}` prints `abc`. Linux renders `/` and `:`, so the trap is Windows-only. Scope: task 3.51.1+ for the variables. | Cell SH-CI-26 lists sites, each needs a quote check. Watched red (measured 2026-10-11). | SHOULD |
| SH-CI-27 | List the platforms that should run in `platforms:`, GOOS or GOOS/GOARCH values only. Never a negation (`!windows`). | `platforms: ['!windows']` makes Task refuse to load the whole Taskfile (rc 102, `invalid platform "!windows"`, every task fails), and a valid but wrong value skips silently with exit 0, a `deps:` entry included. Scope: task 3.54.0, other versions not run. | Cell SH-CI-27. Watched red (measured 2026-10-11). | MUST |

```yaml
# wrong: its own interpreter, pipefail is gone before the next item
cmds:
  - set -o pipefail
  - cargo test 2>&1 | tee out.log
# right
set: [pipefail]
cmds:
  - cargo test 2>&1 | tee out.log
```

## Group 6: GitLab CI and One Grammar Trap

The GitLab runner writes errexit on and pipefail on where the shell has it, then evals every entry in a subshell, so
GitHub's missing-pipefail trap does not exist here. The shell is the first of `/usr/local/bin/bash`, `/usr/bin/bash`,
`/bin/bash`, then sh variants. Variables are exported into the environment and never substituted into the script text,
so the shell is the only injector. All of it is emulated from runner source and run on real shells (alpine:3.22,
busybox:1.37, debian:12, dash 0.5.12), and no real runner ran.

```sh
for t in git xargs grep awk python3 shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
python3 -I -c 'import yaml' || echo "PREFLIGHT: PyYAML missing for python3 -I"
gl() { git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.gitlab-ci.yml' ':(glob)**/*.gitlab-ci.yml' ':(glob)**/.gitlab-ci/**/*.yml' ':(glob)**/.gitlab/ci/**/*.yml'; }
# SH-CI-28, SH-CI-29: sh dialect, then bash for a pinned bash image
sh scripts/ci-extract-lint.sh gitlab
sh scripts/ci-extract-lint.sh gitlab-bash
# SH-CI-29: eval and sh -c text, then the git checkout list
gl | xargs -0 -r grep -H -n -E -e 'eval[[:space:]]+[^#]*[$][{]?(CI_|GITLAB_USER)' -e '(sh|bash) -c[[:space:]]+"[^"]*[$][{]?(CI_|GITLAB_USER)' --
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.sh' | xargs -0 -r grep -H -n -E -e 'git checkout[[:space:]]+--[[:space:]]+[^[:space:]]*[$]' --
# SH-CI-33: job keys, tags and extends (flow or list form, any depth under .gitlab-ci/ or .gitlab/ci/) that start a word with win, to lint under the PowerShell rules. A lead: read each hit (wine is Linux, so is a job merely named Windows)
gl | xargs -0 -r grep -H -n -i -E -e '(tags|extends):.*\<win' -e '^([^[:space:]#-][^#:]*)?\<win[^#:]*:[[:space:]]*(&[A-Za-z0-9_]+)?[[:space:]]*(#.*)?$' -e '^[[:space:]]+-[[:space:]]+[^[:space:]#]*\<win[^[:space:]#]*[[:space:]]*(#.*)?$' -e '!reference.*\<win' --
# SH-CI-30
sh scripts/ci-orand.sh
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CI-28 | Write a GitLab `script:`, `before_script:` and `after_script:` body on a Linux runner in the dialect of the image's shell: POSIX sh unless the pinned `image:` ships bash (`node:*-alpine`, `alpine` and `busybox` are ash). A leading `set -euo pipefail` is harmless but hides the real question, which shell the image has. | The same trap as `SH-CI-04` with the default reversed: a `[[` or an array passes on a Debian image and fails or misbehaves on alpine. Pipefail is on in ash 1.35 and 1.37 and in dash 0.5.12-12 on Debian 13, and absent on sh-only images whose shell lacks it (dash 0.5.12 on Debian 12: `false \| true` then REACHED, rc 0). Scope: GitLab Runner docker executor, main as of 2026-10-11. | Cell SH-CI-28 (`ci-extract-lint.sh gitlab`, ShellCheck at default severity, `gitlab-bash` for a pinned bash image). The extractor blanks `{{placeholder}}` tokens, which can leave SC2215 on `{{run}} --version` lines. A failing extractor is a failure. Watched red (measured 2026-10-11). | MUST |
| SH-CI-29 | In GitLab scripts double-quote every `$CI_*`, `${CI_*}` and `$GITLAB_USER_*` expansion, never place one inside `eval`, `sh -c "..."` or `bash -c "..."` text (pass it as a positional: `sh -c 'echo "$1"' -- "${CI_COMMIT_TITLE}"`), and put `--` before a value used as a path operand (`rm -- "${path}"`). For a git ref write `git checkout "${CI_COMMIT_REF_NAME}" --` or `git switch -- "${CI_COMMIT_REF_NAME}"`, never `git checkout -- "${ref}"`. | The runner exports variables as environment, so only the shell can inject. `CI_COMMIT_MESSAGE`, `CI_COMMIT_TITLE`, `CI_COMMIT_REF_NAME`, `CI_MERGE_REQUEST_TITLE` and `GITLAB_USER_*` carry author text (a reading, GitLab lists no per-variable warning). `git checkout -- feat` reads `feat` as a pathspec and fails (git 2.54.0), while git itself rejects a branch or tag name starting with `-`, so the option risk is for titles, messages and paths. (also SH-SEC-08) Scope: GitLab CI, a fork MR's pipeline runs in the source project (a trust-boundary note). | Cell SH-CI-29: the extractor run reports SC2086 and SC2070, the first grep reports `eval` and `sh -c` text, the second lists `git checkout --` with an expansion (is the operand a path or a ref?). Watched red (measured 2026-10-11). | MUST |
| SH-CI-33 | Lint a GitLab job whose `tags` or `extends` name a Windows runner under the PowerShell rules, never with ShellCheck. `ci-gl-extract.py` skips such jobs. | A GitLab job on a Windows runner runs PowerShell (or cmd), and ShellCheck under `-s sh` reports parse errors on it (SC1066, SC2283, SC1036). Scope: all GitLab repos with Windows runners. The PowerShell half is watched (below). The extractor reads a job as Windows only by a whole word: its effective tags (its own, else its `extends` chain in the same file, plus `parallel.matrix` values when a tag is a variable) hold `windows` or `win` (`win10`, `saas-windows-medium-amd64`), or its image names `windows`, `servercore` or `nanoserver`; with no tags known it reads the job name and `extends`. `wine`, `darwin` and `winding` are not Windows. | Cell SH-CI-33 lists the jobs to lint (a lead: wine and a Windows-named Linux job are hits to discard), and cell SH-CI-28 shows the extractor skipping them. Watched red (measured 2026-10-11) for the skip. PowerShell half watched 2026-10-11: the script bodies of the Windows jobs in wireshark (5 jobs), mesa (5) and gimp (4) were extracted with their `extends` chain and run through `gates-parse-gate.ps1` on Windows PowerShell 5.1.26100.9444 (14 files, 0 errors) and `Invoke-PSGate.ps1` on pwsh 7.6.6 (15 real findings: `cd` alias, `Write-Host`, `Invoke-Expression`, WMI cmdlets). The cell was rewritten after the old one found no Windows job in mesa (list-form `extends:` and `tags:`, and `.gitlab-ci/container/windows/` two levels deep, were invisible); it lists the jobs on the 3 trees and nothing on a Linux-only plant, on GNU grep and busybox. Skip test re-run 2026-10-11 on the three trees: the skipped Windows jobs are unchanged and the wireshark `.Windows MinGW-w64 Package` (tags `saas-linux-small-amd64`) and release-cli `binaries darwin/amd64` and `darwin/arm64` jobs are now extracted and linted; on a plant, `wine-tests`, `darwin-build`, `winding-road` and a Linux-tagged job named Windows are extracted and `win10`, `servercore`, template-tag and `WindowsBuild` jobs are skipped (the old substring test also skipped the four Linux jobs and missed the `servercore` image). File discovery now reaches `.gitlab/ci/`, any `*.gitlab-ci.yml` and nested `.gitlab-ci/` directories. | MUST |
| SH-CI-30 | Never write an ungrouped `A \|\| B && C` (or `A \|\| B && C \|\| D`). Write the grouping you mean: `A \|\| { B && C; }` (A, else B then C), `{ A \|\| B; } && C` (A or B, then C), or `if ...; then ...; elif ...; fi`. | `&&` and `\|\|` have equal precedence and associate left, so it parses as `(A \|\| B) && C`: `apk add ... \|\| apt-get update && apt-get install` runs `apt-get install` on alpine and dies with rc 127. ShellCheck 0.11.0 has no code for it, also with `-o all`. Expert code writes the left-associative form on purpose, so the rule asks for the grouping and not a different meaning. Scope: every shell, grammar and not dialect. | Cell SH-CI-30 (`ci-orand.sh`, a reading aid: a hit is a line to read). Skipped: lines with GitHub expressions, `if:` and `runs-on:` keys, `\|\|` inside `[[ ]]` and generated files. A `\|\|` inside a quoted string or heredoc matches. Watched red (measured 2026-10-11). | MUST |

```yaml
script:
  # wrong: apt-get install runs after a successful apk add and dies with rc 127
  - apk add --no-cache curl || apt-get update && apt-get install -y curl
  # right
  - apk add --no-cache curl || { apt-get update && apt-get install -y curl; }
```

## What Agents Get Wrong Here

Ranked by how often each bites, with the check that catches it.

| Rank | Failure | Check |
|---|---|---|
| 1 | Writes `cmd1 \| cmd2` in a `run:` with no `shell:` and assumes pipefail (`SH-CI-01`). | Cell SH-CI-01 |
| 2 | Interpolates `${{ github.event.* }}` or `${{ inputs.* }}` into `run:`, or believes `${{ env.X }}` is the safe form (`SH-CI-07`, `SH-CI-08`). | Cells SH-CI-07, SH-CI-08, zizmor |
| 3 | Moves the title into `env:` in job A, then writes `${{ needs.a.outputs.title }}` or `${{ matrix.item.field }}` in a later run body (`SH-CI-23`, `SH-CI-24`). | Cells SH-CI-23, SH-CI-24 |
| 4 | Writes bash (`[[`, arrays, `pipefail`) in a `container:` job that runs under `sh -e`, or in a GitLab job on an alpine image (`SH-CI-04`, `SH-CI-28`). | Cells SH-CI-04, SH-CI-28 |
| 5 | Puts bare `set -o pipefail` under `shell: sh` (`SH-CI-03`). | Cell SH-CI-03 |
| 6 | Emits 2020-era `::set-output` and `::set-env` (`SH-CI-09`), or a fixed `EOF` delimiter (`SH-CI-10`). | Cells SH-CI-09, SH-CI-10 |
| 7 | Enables pipefail and leaves `\| grep -q` or `\| head` in the same step (`SH-CI-16`). | Cell SH-CI-16 |
| 8 | Writes Taskfile `cmds` as if they ran under bash: `printf -v`, `PIPESTATUS`, `shopt: [pipefail]`, `set -o pipefail` as its own item, a bare pipeline with no pipefail (`SH-CI-17`, `SH-CI-18`, `SH-CI-32`). | Cells SH-CI-17, SH-CI-18, SH-CI-32 |
| 9 | Writes `>>`, `Add-Content` or a mixed pair into `$GITHUB_ENV` under `shell: powershell` (`SH-CI-11`). | Cell SH-CI-11 |
| 10 | Runs several native commands in a pwsh block and trusts `$ErrorActionPreference` (`SH-CI-12`). | Cell SH-CI-12 |
| 11 | Puts bash 4 code (`declare -A`, `mapfile`, `${x,,}`) in a `macos-*` job (`SH-CI-13`). | Cell SH-CI-13 |
| 12 | Writes `A \|\| B && C` as "try A, else B then C" (`SH-CI-30`). | Cell SH-CI-30 |
| 13 | Reads a clean zizmor run at medium as "no injection", or reaches for `--persona=auditor` to be thorough and gets 140 findings against 18 (`SH-CI-21`). | The three grep cells, persona and floor stay |
| 14 | Writes `uname`, `sed`, `grep` or `head` in a Taskfile as if Task shipped a POSIX userland, or `platforms: ['!windows']` (`SH-CI-20`, `SH-CI-27`). | Cells SH-CI-20, SH-CI-27 |
| 15 | Uses `$CI_COMMIT_MESSAGE` unquoted or inside `eval` and `sh -c "..."`, or writes `git checkout -- "$ref"` as the safe form (`SH-CI-29`). | Cell SH-CI-29 |
| 16 | Sniffs GNU with `--version \| grep GNU`, which breaks on uutils in the 26.04 image (`SH-CI-15`). | Cell SH-CI-15 |
| 17 | Hallucinates `shell: dash`, `shell: zsh` or `shell: fish`, or assumes `export` or `cd` persists between steps. | actionlint at default severity, a reading step |
