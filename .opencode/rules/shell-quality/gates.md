---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Shell Gates
summary: The SH-GATE family. The ShellCheck and shfmt profile, suppression policy, the fail-closed gate script and its exit map, file discovery, parse-only checks per shell family, tool pins, file hygiene, embedded workflow shell, and the dialect every verification cell in the shell set is written in
---

# Shell Gates

Rows bind to ShellCheck 0.9.0 to 0.11.0, shfmt 3.12.0 to 3.14.1 (3.13.0 or newer for `.zsh`), bash 3.2.57 to 5.3, GNU grep 3.11 and 3.12,
BusyBox 1.37.0, uutils coreutils 0.10.0 and Git Bash 5.2.15 (measured 2026-10-11). macOS and BSD are `unverified: read only` on every cell.
Until 2026-11-19 `ubuntu-latest` may still resolve to 24.04 (ShellCheck 0.9.0, shfmt 3.8.0), then to 26.04 (0.11.0, 3.12.0).

Owns the lint and format profile files, the suppression policy, `checks/shell-gate.sh` and its exit map, file discovery, the parse-only check of
each shell family, tool pins, line endings, BOM, ASCII and file modes, actionlint for workflow shell, and the dialect of every verification
cell in the set. Not owned here: the portability and quoting findings ShellCheck reports are `SH-DIAL` and `SH-EXP`, error handling inside
scripts is `SH-ERR`, run-step semantics are `SH-CI`, the strict gates of zsh, fish, nu and elvish are `SH-ZSH`, `SH-FISH`, `SH-NU` and `SH-ELV`,
bats gates are `SH-TEST`, the PowerShell parse gate is `PS-GATE`, and the tell table is in the index (`SH-CORE`).

Contents: [Reading the Cells](#reading-the-cells) · [Templates](#templates) · [A. Profile and Suppression](#a-profile-and-suppression) ·
[B. The Gate Fails Closed](#b-the-gate-fails-closed) · [C. Discovery and Parse Gates](#c-discovery-and-parse-gates) ·
[D. Style and Pins](#d-style-and-pins) · [E. Hygiene](#e-hygiene) · [F. Embedded Shell](#f-embedded-shell) ·
[G. The Verification Dialect](#g-the-verification-dialect) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defects fixed: a dangling or deleted tracked file broke discovery in `files` and in `checks/shell-gate.sh` (exit 74 where the file is not a finding), and the gate script now drops such a file before shfmt. `shell-gate.sh` ran on 38 plants and twins, all as expected per its exit map (the 33 plants of the original round, run with `--`, plus a tracked-deleted file and two dangling symlinks).

## Reading the Cells

Output is the violation. A cell passes only with empty stdout and empty stderr (`SH-GATE-26`). Never read an exit status. A `# NN-x` comment
names the cell a table row cites. Each block opens with the PREFLIGHT line, then the helper it needs. A `PREFLIGHT:` line means stop and fix the host. `files` lists what `shfmt -f` lists: not a shebang file that carries another extension (`.aliases`, `.in`, `.tmpl`), and not a dash, ash or ksh shebang. Cell `i00-gap` in `installers.md` lists those files; read each hit.

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
```

Severity words: MUST needs a measured source and a cell watched red on a plant and green on its twin. Every cell below was, measured 2026-10-11, under
dash, bash, `zsh -f` and BusyBox ash on GNU grep 3.12, and under BusyBox 1.37 with git added and Git Bash 5.2.15. Cell 07-a is a listing and prints on both trees.

## Templates

Pinned decision (adopter may override the style, never the keys): copy these to the repo root, and copy `checks/shell-gate.sh` to `scripts/shell-gate.sh`.
The index carries the one call, `bash scripts/shell-gate.sh`.

```text
# .shellcheckrc: severity is a command-line concept, never an rc key. Only the nearest rc is read, a nested one must repeat every line.
external-sources=true         # follow `source` without -x
source-path=SCRIPTDIR         # resolve `. "$here/../lib/x.sh"` from the script's directory
enable=check-set-e-suppressed # SC2310 and SC2311: set -e silently off inside `if f`, `f &&`, `f ||`
enable=deprecate-which        # SC2230: `which` is not POSIX, use `command -v`
```

```ini
# .editorconfig: [[shell]] is what styles extensionless scripts (a four-space bin/tool is dirty without it on shfmt 3.12.0 to 3.14.1)
root = true

[*]
end_of_line = lf
insert_final_newline = true

[*.{sh,bash,bats,ksh}]
indent_style = space
indent_size = 4
switch_case_indent = true

[[shell]]
indent_style = space
indent_size = 4
switch_case_indent = true
```

`.gitattributes`: `* text=auto eol=lf`, plus `*.bat text eol=crlf` and `*.cmd text eol=crlf` where those exist.
`check-extra-masked-returns` (SC2312) is not enabled: 191 sites, 2 real defects, answered by `SH-ERR-07`.

## A. Profile and Suppression

Cells 01-a to 07-a, one block. They scan workflows, Makefiles, rc files and the discovered shell files.

```sh
for t in git xargs grep awk shfmt shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# 01-a
{ git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.mk' ':(glob)**/Makefile'; files; } | xargs -0 -r grep -H -n -e 'shellcheck.* -S' -e 'shellcheck.*--severity' -e 'SHELLCHECK_OPTS.*-S' -e 'SHELLCHECK_OPTS.*severity' --
# 02-a
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.shellcheckrc' ':(glob)**/shellcheckrc' | xargs -0 -r grep -H -n -e '^severity' -e '^shell=' -e '^disable=all' --
# 02-b
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.editorconfig' | xargs -0 -r grep -H -n -e 'shellcheck\.' --
# 02-c
for k in external-sources=true source-path=SCRIPTDIR enable=check-set-e-suppressed enable=deprecate-which; do grep -q -e "^$k" .shellcheckrc || echo "missing in .shellcheckrc: $k"; done
# 03-a
{ shellcheck --list-optional | sed -n -e 's/^name:[[:space:]]*/L /p'; sed -n -e 's/^enable=\([A-Za-z0-9-]*\).*/R \1/p' .shellcheckrc; } | awk '$1 == "L" { have[$2] = 1 } $1 == "R" && !($2 in have) { print "unknown optional check in .shellcheckrc: " $2 }'
# 04-a
git ls-files -z --cached --others --exclude-standard -- ':(glob)*/**/.shellcheckrc' ':(glob)*/**/shellcheckrc' | tr '\0' '\n'
# 05-a
{ git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml'; files; } | xargs -0 -r grep -H -n -E -e 'shellcheck[[:space:]]+disable=[A-Za-z0-9,-]*[[:space:]]*$' --
# 05-b
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.shellcheckrc' ':(glob)**/shellcheckrc' | xargs -0 -r grep -H -n -E -e '^disable=[A-Za-z0-9,-]*[[:space:]]*$' --
# 06-a
{ git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' ':(glob)**/.shellcheckrc' ':(glob)**/shellcheckrc'; files; } | xargs -0 -r grep -H -n -E -e 'disable=[^#]*SC(2155|2164|2162|2115|2068|2039|2230|3013)([^0-9]|$)' -e 'disable=[^#]*SC([0-9]{1,3}|[0-9]{5,})([^0-9]|$)' --
# 06-b
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.shellcheckrc' ':(glob)**/shellcheckrc' | xargs -0 -r grep -H -n -E -e 'SC(2086|2046)' --
# 06-c
{ git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml'; files; } | xargs -0 -r grep -H -n -E -e 'disable=[^#]*SC(2086|2046)([^0-9]|$)' -- | grep -v -i -E -e '#.*(split|glob)'
# 07-a (listing: each hit is a site to read against the table, pass is every hit fixed or reasoned)
files | xargs -0 -r grep -H -n -o -e 'disable=[A-Za-z0-9,-]*.*' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-GATE-01 | Run ShellCheck at default severity: no `-S`, no `--severity`, no `SHELLCHECK_OPTS` carrying a severity, in any task, workflow, Makefile or script. | `-S warning` hid 57 of 65 default findings on 197 authored files and every optional-check finding: SC2086 and SC2162 are info, SC2046 SC2155 SC2164 warnings, SC2068 an error (identical on 0.9.0 to 0.11.0). Scope: all script kinds, CI and embedded shell. | cell 01-a | MUST |
| SH-GATE-02 | The root `.shellcheckrc` is the four-key profile of Templates. It has no `severity=`, no `shell=`, no `disable=all`, and `.editorconfig` has no `shellcheck.` key. | `severity=` is not an rc key and is silently ignored, `shell=bash` overrides every shebang (SC3010 and SC3030 vanish in a `#!/bin/sh` file), and `.editorconfig` ShellCheck keys are ignored by 0.11.0. Scope: every repo with shell. Reasoned `disable=` lines for the `SH-GATE-07` codes may follow the four keys. | cells 02-a, 02-b, 02-c | MUST |
| SH-GATE-03 | Every `enable=` name exists in `shellcheck --list-optional` of the pinned version. `enable=all` is not a name and fails the same test. | An unknown optional check is accepted silently on 0.9.0, 0.10.0 and 0.11.0: the check never runs and the gate looks green. Scope: ShellCheck 0.9.0 or newer. | cell 03-a | MUST |
| SH-GATE-04 | There is no nested `.shellcheckrc` or `shellcheckrc`. If one is unavoidable it repeats all four profile keys. | ShellCheck reads only the first rc found walking up: a nested `disable=SC2034` file dropped `external-sources`, `source-path` and every `enable=` (SC1091 came back, SC2310 vanished). The pathspec means one directory deep or more, so the root profile is not listed. Scope: all. | cell 04-a | MUST |
| SH-GATE-05 | Every `# shellcheck disable=` directive and every `disable=` line in an rc carries a same-line `# reason`. A comment that only mentions the tool starts `# note: shellcheck ...`. | 1,185 exemplar directives, 148 justified. A comment beginning `# shellcheck ` followed by prose parses as a directive and breaks the file (SC1073, SC1072). Scope: all. A preceding-line reason is legal for ShellCheck and is not accepted here, because a grep can check only the same line. | cells 05-a, 05-b, and the gate run for the prose form | MUST |
| SH-GATE-06 | Never disable SC2155, SC2164, SC2162, SC2115 or SC2068. Never list SC2086 or SC2046 in an rc. A per-site SC2086 or SC2046 directive says "split" or "glob" in its reason. Never disable a retired code (SC2039, SC2230, SC3013) or a malformed one. | These are the bug classes the gate exists for, and ShellCheck never reports a stale or mistyped suppression: `disable=SC2039` is silent while the real SC3043 appears, `disable=SC20865` is ignored. Scope: all. | cells 06-a, 06-b, 06-c | MUST |
| SH-GATE-07 | Relax only what the table below allows, with a reason in the shown shape. | These are the relaxations maintained exemplars converge on (14 of 39 repos disable SC2016), narrowed to per-site. The probed-pipefail line still raises SC3040 on 0.9.0 to 0.11.0 and runs clean with the directive on 11 of 11 POSIX interpreters. Scope: all, and the SC3xxx rows bind `#!/bin/sh` only. Reading heuristic: whether a reason is true is semantic. | cell 07-a (listing) | SHOULD |

| Code | Allowed when | Reason shape |
|---|---|---|
| SC2317 (0.9.0, 0.10.0), SC2329 (0.11.0 or newer) | a function is reached only by name through a table, trap-less dispatch or a function argument | `# reached only through WHERE` |
| SC1091 | the sourced path is computed at runtime and `source-path` cannot resolve it | `# path is computed at runtime from VAR, checked as FILE in its own run` |
| SC2016 | a single-quoted `$` is meant literally (emitted code, test strings, `$env` for nu) | `# single quotes are deliberate: the $ is for the target shell` |
| SC3040 | probed `pipefail` in a `#!/bin/sh` script only | `# probed in a subshell first: dash 0.5.12-2 and posh abort on an unknown set -o` |
| SC3043 | `local` in `#!/bin/sh` when ksh93 is not a target | `# local is accepted by every sh target in the floor table` |
| SC2218 (`.bats` only) | never: bats evaluates the whole file before the first test, so SC2218 fired on 21 of 22 sites that ran fine | order each helper function above the first `@test`, never `disable=SC2218` |

```sh
# wrong: no reason, a forbidden code, and prose that parses as a directive
# shellcheck disable=SC2155
# shellcheck cannot parse zsh, so skip it
# right
# shellcheck disable=SC2086 # deliberate split of $FLAGS
# note: shellcheck cannot parse zsh, so the gate parses it with zsh -fn
```

## B. The Gate Fails Closed

The check is `checks/shell-gate.sh` itself, run with tools off PATH (the `bash` block), and cell 08-a for the silent-pass idioms.
Exit map (pinned, also `SH-GATE-31`): 0 clean, 1 a finding, 69 a tool or family interpreter is missing, 74 git, shfmt or awk failed while reading files,
78 `.shellcheckrc` or `.editorconfig` is missing or the directory is not a usable git work tree, 79 no shell file found. Kind: gate, `set -u` and no `-e`.

```bash
d=$(mktemp -d) && for t in bash git grep xargs awk cut tr env; do ln -s "$(command -v "$t")" "$d/$t"; done # vd-ok: xargs here is a tool name
PATH="$d" bash scripts/shell-gate.sh; echo "exit $?"
```

The first run prints 69. Linking in `shellcheck` and `shfmt` turns it into 0 (the green twin).

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# 08-a
{ git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.mk' ':(glob)**/Makefile'; files; } | xargs -0 -r grep -H -l -e shellcheck -e shfmt -e actionlint -- | tr '\n' '\0' | xargs -0 -r grep -H -n -e '|| true' -e 'not installed' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-GATE-08 | The gate exits 69 when `git`, `xargs`, `grep`, `awk`, `cut`, `tr`, `shellcheck`, `shfmt` or the interpreter of a family present is not on PATH, 78 when a profile file is missing or the tree is unusable, 74 when listing fails, 79 when no shell file is found, 1 on a finding. No gate command is followed by a true-fallback, and no branch prints "not installed, skipping" and continues. | A skipped gate is a passed gate: one fleet task printed `shellcheck not installed - skipping` and exited 0, and actionlint exits 0 without ShellCheck even with `-shellcheck=/nonexistent`. ShellCheck falls back to `~/.shellcheckrc`, so a missing repo rc means laptop and CI disagree. A tracked file deleted before the commit made `shfmt -f` fail and the first gate died with 123, a status in nobody's table. 66 is never claimed (`cli-contract`). Scope: all gates including the actionlint step. A Task `preconditions:` entry whose `msg:` says "skipping" fails the task with exit 201 (Task 3.53.1), so it is loud. vd-ok: the row names xargs as a tool. | the PATH-strip block, cell 08-a | MUST |

## C. Discovery and Parse Gates

Cell 09-a (config and shell files that hand ShellCheck or shfmt a glob). `SH-GATE-10` is a plant: a violation under the excluded directory is green
with the pathspec and red without it. `SH-GATE-11` is run on a broken file and its twin.

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# 09-a
{ git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.mk' ':(glob)**/Makefile'; files; } | xargs -0 -r grep -H -l -e shellcheck -e shfmt -- | tr '\n' '\0' | xargs -0 -r grep -H -n -E -e '\*\.(sh|bash|bats)' -e '(shellcheck|shfmt)[^|;&]*\*\*' -e 'find \. -name' -e 'shfmt[^|;&]* -f=0' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-GATE-09 | Discover with `git ls-files -z --cached --others --exclude-standard`, piped through a `[ -f ]` filter and then `xargs -0 -r shfmt -f --` (plain `-f`). The filter drops a tracked file that is gone and a dangling symlink, on which shfmt exits with a stat error and the whole list dies (`bin/subl` in mathiasbynens/dotfiles, `modules/prompt/functions/async` in prezto). Never a `*.sh` or `**` glob, `find . -name`, a hand-written list or `shfmt -f=0`. `.bats` and extensionless shebang files are in the list. Families without a shebang convention (fish, nu, elvish, zsh) are found by extension pathspec. shfmt's blocking set is that list minus zsh files. zsh files go to an advisory run over an explicit list, and `zsh -fn` is their gate. | On the discovery fixture `git ls-files '*.sh'` found 5 of 14 shell files, `shfmt -f .` walked gitignored `node_modules`, the git-fed command found 12 (13 on 3.14.1, which also reads `#!/bin/dash`). Two exemplars skip their own installer because `**/*.sh` without globstar means `*/*.sh`. On 3.13.1 exactly, `-f=0` echoes non-shell arguments such as `.editorconfig`. Scope: sh, bash, ksh, bats, zsh. A file with no extension and no shebang is unreachable by any tool. vd-ok: the row names the xargs form and the failing globs. | cell 09-a | MUST |
| SH-GATE-10 | Exclusions are git pathspecs on the discovery command (`-- . ':(exclude)vendor'`), and the gate passes `--apply-ignore` to shfmt. | shfmt's `ignore = true` is honoured when it walks a directory and skipped for explicit file arguments unless `--apply-ignore` is given. Scope: repos with vendored or fixture shell. | plant a violation under the excluded directory | SHOULD |
| SH-GATE-11 | Every shell family present has a parse-only check watched red on a broken file. The parser for sh, bash, ksh and bats is ShellCheck plus shfmt, never `bash -n`. The table lists the floor the gate script runs. | `bash -n` exits 0 on `arr=(1 2` for bash 3.2, 4.4 and 5.1 (1 on 5.2 and 5.3), so a script that cannot parse passes. `nu --ide-check` and bare `nu-check` exit 0 on a broken file, and fish exits 127, not 1. shfmt is a partial dialect checker (a shebang-less `.sh` with an array parses as bash), so the dialect check is `shellcheck -s sh` (`SH-DIAL`). Scope: each family only where the repo has files for it. | broken file and good twin per row | MUST |

| Family | Parse command, file last | Broken / good |
|---|---|---|
| sh, bash, ksh, bats | `shellcheck FILE` and `shfmt FILE` | 1 / 0 |
| POSIX sh, cheap second parse | `dash -n FILE` | 2 / 0 |
| zsh, from 5.3.1 (also SH-ZSH-12) | `zsh -fn FILE`, never `zsh -n` on 5.0.8 | 1 / 0 |
| fish 3.6.0 to 4.9.3 (also SH-FISH-01) | `fish --no-execute FILE`, any non-zero is a failure | 127 / 0 |
| nu 0.105.1 to 0.116.0 (also SH-NU-02) | the `nu-check` wrapper in the gate script, strict form `nu-gate.nu --strict` | 1 / 0 |
| elvish 0.21.0 (also SH-ELV-02) | `elvish -compileonly FILE` as the floor, `elv-gate.sh` for the strict form | 2 / 0 |
| PowerShell | `PS-GATE`: `Parser::ParseFile` plus a real 5.1 host parse | 1 / 0 |

## D. Style and Pins

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# 12-a
{ git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.mk' ':(glob)**/Makefile'; files; } | xargs -0 -r grep -H -n -E -e 'shfmt[^|;&#]* -(i|ci|bn|s|ln|sr|kp|fn|mn|p)([ =]|$)' -e 'shfmt[^|;&#]* --(indent|case-indent|binary-next-line|simplify|language-dialect|space-redirects|keep-padding|func-next-line|minify)' --
# 12-b
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.editorconfig' | xargs -0 -r grep -H -n -e '^case_indent' -e '^language_dialect' --
# 12-c
grep -q -e '^root *= *true' .editorconfig || echo "missing: root = true in .editorconfig"
# 13-a
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/ocx.toml' | xargs -0 -r -n 1 sh -c '[ -f "${1%ocx.toml}ocx.lock" ] || grep -H -n -E -e "(shellcheck|shfmt)[^[:space:]]*:(latest|[0-9]+)[^0-9.]" -- "$1"' sh
# 13-b
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/mise.toml' ':(glob)**/.mise.toml' ':(glob)**/.tool-versions' | xargs -0 -r grep -H -n -E -e '(shellcheck|shfmt)[[:space:]]*=?[[:space:]]*"?(latest|[0-9]+)"?$' --
# 13-c
{ git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' ':(glob)**/Makefile'; files; } | xargs -0 -r grep -H -n -E -e '(apt(-get)?|apk|brew) (-y )?install[^|;&#]*(shellcheck|shfmt)' -e 'webi\.sh/(shfmt|shellcheck)' --
```

```sh
# wrong: a style flag switches EditorConfig off for the whole run
shfmt -d -i 4 -ci "$f"
# right: the style lives in .editorconfig, the gate passes none
shfmt -d --apply-ignore -- "$f"
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-GATE-12 | The shfmt style lives in `.editorconfig` (`root = true`, the two sections of Templates, key spelling `switch_case_indent`) and the gate passes no shfmt parser or printer flag. | Any of `-i -ci -bn -s -ln -sr -kp -fn -mn -p` switches EditorConfig off for the whole run (measured on 3.12.0, 3.13.0, 3.14.1): `shfmt -d -i 4 a.sh` fails a four-space case-indented file that plain `shfmt -d` accepts. `case_indent` and `language_dialect` are ignored by 3.14.1, and `.bats` breaks under `-ln posix`. Agents write two-space unsupervised (97 of 129 scratch files). Scope: the program's repos. An adopter whose `.editorconfig` states another style is compliant. | cells 12-a, 12-b, 12-c | MUST |
| SH-GATE-13 | Pin ShellCheck and shfmt the same on laptops and CI: an exact version, or a floating tag only with a committed lockfile (for example `ocx.lock`). Do not install either with apt, apk, brew or webi, or take it from the runner image. shfmt is 3.13.0 or newer when the repo has `.zsh` files. | Findings differ by version: SC2329 exists from 0.11.0, SC3013 until 0.10.0, SC2002 is default-on until 0.10.0. bats-core pins shfmt 3.5.1 and 3.13.1 would reformat 48 of its files. Scope: any repo whose gate runs in CI. The floating tag plus lock is the fleet's practice. | cells 13-a, 13-b, 13-c | MUST |

## E. Hygiene

Cells 14-a, 15-a and 16-a. The mode checks (`SH-GATE-17`) are the mode loop of `checks/shell-gate.sh`, and 18 is a gate run.

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# 14-a
files | xargs -0 -r git check-attr eol -- | grep -v -e ': eol: lf$'
# 15-a
files | LC_ALL=C xargs -0 -r awk -v BINMODE=3 '/\r/ && !cr[FILENAME]++ { print FILENAME ":" FNR ": CR" } FNR == 1 && /^\357\273\277/ { print FILENAME ": BOM" }'
# 16-a
files | LC_ALL=C xargs -0 -r awk 'FNR == 1 { sh = ($0 ~ /^#![ ]*\/(usr\/)?bin\/(env )?sh([ \r]|$)/) } sh && /[^ -~\t\r]/ { print FILENAME ":" FNR ": " $0 }'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-GATE-14 | `.gitattributes` has `* text=auto eol=lf`, so no shell file is checked out with CRLF. | Git's default on Windows is CRLF. A CRLF body runs a prefix and dies (`syntax error: unexpected end of file`, exit 2 on bash 3.2 to 5.3), while Git Bash 5.2.15 runs the same file to completion with exit 0: it works locally and fails in the Linux container. Scope: repos that may be checked out on Windows, except `*.bat` and `*.cmd`. | cell 14-a | MUST |
| SH-GATE-15 | No shell file contains a CR byte or a UTF-8 BOM. | A CR after the shebang makes the kernel look for `/bin/sh\r` (exit 126 when exec'd from bash, 127 for `sh -c ./f`). A BOM makes line 1 `No such file or directory` while the rest runs and exits 0. Windows PowerShell 5.1 `-Encoding UTF8` writes a BOM. The cell uses awk in binary mode because Git Bash's grep reads text mode and never sees the CR, and `$'\r'` is the literal text `$\r` on dash. ShellCheck reports the same as SC1017 and SC1082. `rg` strips a BOM and prints nothing. Scope: all shell files, extensionless ones included. The gate script also catches a file named NAME=VALUE, which awk reads as an assignment. | cell 15-a, the gate run | MUST |
| SH-GATE-16 | A `#!/bin/sh` script is ASCII only, comments included. A `#!/bin/bash` script may carry UTF-8 comments. | yash 2.52 in a POSIX or C locale cannot read a script with any non-ASCII byte (exit 2, passes under `LC_ALL=C.UTF-8`), and a runtime em dash mojibakes on a C locale. 21 of 31 authored sh scripts had one. The awk tests line 1 only, so a bash script whose heredoc holds `#!/bin/sh` is not flagged, and `#! /bin/sh` and a CRLF shebang are caught. Scope: installer and bootstrap scripts are MUST, other `#!/bin/sh` scripts SHOULD. | cell 16-a, the gate run | MUST for installer and bootstrap, else SHOULD |
| SH-GATE-17 | The git mode matches line 1: a shebang means `100755`, none means `100644`. Fix with `git update-index --chmod=+x FILE`. | 93 of 186 fleet scripts with a shebang were committed `100644`, and agents create files with `cat >` and never `chmod +x`. The mode is read from the index, so it is stable on Windows checkouts. Scope: all. | the mode loop of `checks/shell-gate.sh` | SHOULD |
| SH-GATE-18 | A sourced library has a `.sh` or `.bash` extension, no shebang, `# shellcheck shell=bash` (or `sh`, `dash`) as line 1, and mode `100644`. | Without the directive ShellCheck raises SC2148, an error that survives `-S error`. A file with neither extension nor shebang is reachable by no tool. `shell=zsh` is rejected (SC1103). Scope: libraries sourced by scripts, the directive names the floor of the callers. | the gate run, `shellcheck FILE` clean | SHOULD |

## F. Embedded Shell

The check is two lines in the CI job (not a cell). It needs actionlint and ShellCheck on PATH. Watched red 2026-10-11 on actionlint 1.7.12 with ShellCheck 0.11.0 (SC2310 appears, a missing ShellCheck exits 69).

```bash
command -v shellcheck >/dev/null || exit 69
SHELLCHECK_OPTS='-o check-set-e-suppressed,deprecate-which' actionlint
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-GATE-19 | Lint workflow `run:` blocks with actionlint, passing the profile through `SHELLCHECK_OPTS` as shown, after the `command -v shellcheck` precheck. A step whose `shell:` is a path-qualified template is not linted, so set `shell: bash`. | actionlint ignores `.shellcheckrc`, runs ShellCheck only for `bash` and `sh` steps (never `pwsh`), exits 0 when ShellCheck is missing, and `shell: /bin/bash -eo pipefail {0}` silently skips it. `--severity=warning` drops SC2086 and SC2310 from a workflow that has both. Scope: GitHub workflows (composite `action.yml` is not linted by actionlint). Run-step semantics belong to `SH-CI`. | the two lines above on a bad workflow and a good twin | SHOULD |

SH-GATE-20, SH-GATE-27 and SH-GATE-32 are retired and not reused.

## G. The Verification Dialect

How every cell in the set is written. The self-checks scan the set's own files, so an adopter replaces the pathspecs of `cells` with the files that
hold its cells. A line that must show a banned form for explanation ends in `vd-ok: REASON`. `checks/gates-scan.sh` is the reference for a scan too big for one cell.

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
cells() { git ls-files -z --cached --others --exclude-standard -- 'rules/*shell-quality*' 'skills/shell-*' 'skills/installer-script*'; }
# 21-a
cells | xargs -0 -r grep -H -n -E -e '--include' -e '--exclude([^-]|-dir|-from|$)' -- | grep -v -e 'vd-ok: *[^ ]'
# 21-b
cells | xargs -0 -r grep -H -n -e 'grep -[a-zA-Z]*[rR]' -- | grep -v -e 'vd-ok: *[^ ]'
# 21-c
cells | xargs -0 -r grep -H -n -e '^[[:space:]]*rg ' -e '[^[:alnum:]_/.-]rg -' -- | grep -v -e 'vd-ok: *[^ ]'
# 22-a
cells | xargs -0 -r grep -H -n -E -e "git ls-files .*'[*][.](sh|bash|bats|ksh)'" -- | grep -v -e 'vd-ok: *[^ ]'
# 23-a
cells | xargs -0 -r grep -H -n -E -e 'grep( +-[a-zA-Z]+)* +-[a-zA-Z]*[PzZUI]' -e 'grep .*--(null|no-messages|color)' -- | grep -v -e '(GNU grep)' -e 'vd-ok: *[^ ]'
# 23-b
cells | xargs -0 -r grep -H -n -e "grep .*[[:space:]=(][$]'" -- | grep -v -e 'vd-ok: *[^ ]'
# 24-a
cells | xargs -0 -r grep -H -n -e 'git ls-files .*[[:space:]][*]' -- | grep -v -e 'vd-ok: *[^ ]'
# 26-a
cells | xargs -0 -r grep -H -n -e 'grep .*2>/dev/null' -- | grep -v -e 'vd-ok: *[^ ]'
# 28-a
cells | xargs -0 -r grep -H -n -e 'xargs' -- | grep -v -e 'xargs -0 -r' -e 'xargs -r' -e 'vd-ok: *[^ ]'
# 28-b
cells | xargs -0 -r grep -H -n -e 'xargs -0 -r grep' -- | grep -v -e 'xargs -0 -r grep -[a-zA-Z]*H' -e 'vd-ok: *[^ ]'
# 28-c
cells | xargs -0 -r grep -H -n -e 'xargs -0 -r grep' -- | grep -v -e 'xargs -0 -r grep .* -- ' -e 'xargs -0 -r grep .* --$' -e 'vd-ok: *[^ ]'
# 28-d
cells | xargs -0 -r grep -H -n -E -e 'grep .*-[a-zA-Z]*E.*[\\][|]' -- | grep -v -e 'vd-ok: *[^ ]'
# 29-a
cells | xargs -0 -r grep -H -n -e '^[[:space:]]*[|].*grep -' -e '^[[:space:]]*[|].*xargs ' -- | grep -v -e 'vd-ok: *[^ ]'
```

```sh
# wrong: recursive grep with an include option, dead on BusyBox, and its error hides behind the next pipe stage
grep -rn --include='*.yml' -e 'shellcheck -S' . # vd-ok: the wrong form, shown on purpose
# right: the list comes from git, grep gets -H -n, a trailing -- and every file as an operand
git ls-files -z --cached --others --exclude-standard -- '*.yml' | xargs -0 -r grep -H -n -e 'shellcheck -S' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-GATE-21 | An inline verification of non-shell files has exactly the git-fed shape in the right-hand snippet above. Never `grep -r`, `find` piped to xargs without `-print0`, or `rg`. Every cell in sections A to F is written to it. | The recursive form with an include option is an unrecognized option on BusyBox 1.37: 17 of 18 representative cells came back silent there, and 7 of 18 on Git Bash. The list from git is the list the gate uses, skips ignored trees and includes untracked files. `rg` is on none of the four stock images and strips a BOM. Scope: the shell set's cells. vd-ok: the row names the banned forms. | cells 21-a, 21-b, 21-c | MUST |
| SH-GATE-22 | A cell that searches the content of shell files lists them with the `files` helper, not an extension pathspec. A cell over workflows, Taskfiles, Makefiles and rc files uses a pathspec. A cell needing both joins the two lists in one brace group. | An extension pathspec is blind to extensionless scripts (`libexec/pyenv-rehash`, `bin/tool`). `files` without `shfmt` on PATH yields an empty list and a green, so the PREFLIGHT line names `shfmt`. Scope: cells over shell content. | cell 22-a | MUST |
| SH-GATE-23 | Inside a verification, grep uses only `-E -F -e -H -n -l -L -c -o -q -s -v -w -x -i --`. Banned: the recursive, include and exclude options, `-P -z -Z -U -I`, `--null`, `--no-messages`, `--color`, and `$'..'` quoting. A `-P` or `-z` form appears only as an extra labelled `(GNU grep)` beside the awk check that is run. | Each banned item is unrecognized on BusyBox 1.37 except `-I`, which it accepts and ignores. `-P` also needs a UTF-8 locale on Git Bash. `$'..'` is not POSIX: dash greps for the literal text. The set is the intersection of GNU, BusyBox and FreeBSD grep, not of POSIX. Scope: the shell set's cells. BSD and macOS: expected to work, unverified: read only. vd-ok: the row names the banned forms. | cells 23-a, 23-b | MUST |
| SH-GATE-24 | Pathspecs are quoted. A path match uses `'*.EXT'` (any depth) or `'DIR/*.EXT'`. An exact basename or a `Prefix*.ext` basename uses `':(glob)**/NAME'`. Exclusions use `':(exclude)DIR'` or `':(exclude,glob)**/DIR/**'`. Never an unquoted `*`, a bare `'Makefile'` (root only) or `'*Makefile'` (also matches `NotMakefile`). | Measured on four hosts: `'*.yml'` matches `.github/workflows/ci.yml` and `top.yml`, `':(glob)*.yml'` only the top level. An unquoted `*.yml` is expanded by the shell first and finds `top.yml` only, and zsh aborts the whole block on a non-match. Scope: every `git ls-files` pathspec. Run from a subdirectory, paths are relative to it. The bare-basename half is a reading heuristic. | cell 24-a | MUST |
| SH-GATE-25 | The first line of a block of inline cells is the PREFLIGHT line, with every tool the block's cells use in its list (`shfmt` for `files`, `awk` for awk cells, `shellcheck` for cell 03-a). | The git-fed shape moves a failure upstream and loses it behind the pipe: no git, no repository, `fatal: detected dubious ownership` (a root container on a bind mount), a missing `shfmt` or xargs all leave exit 0 with empty stdout. A script can refuse, an inline cell can only print. The remedy `safe.directory='*'` disables a security check, so the rule asks for the failure to be visible, not for the exception. Scope: every block of inline cells. vd-ok: the row names xargs as a tool. | the PREFLIGHT line on a host with one tool removed | MUST |
| SH-GATE-26 | A cell passes only when stdout is empty and stderr is empty. Do not judge by exit status through xargs, and never write `2>/dev/null` on a verification. A stderr line such as `unrecognized option`, `fatal:`, `not found`, `No such file`, `Invalid regular expression` or `binary file matches` is a harness failure, not a clean result. | xargs maps grep's no-match (1) and error (2) both to 123, `-L` through xargs is plant 123 and twin 0 on GNU but plant 0 and twin 123 on BusyBox, and five BusyBox idioms turned an exit 2 into "clean". GNU grep 3.8 or newer warns on a stray backslash, so silence is also a lint for sloppy escapes. Scope: all cells. The silence rule is a reading rule, the redirect half is mechanical. vd-ok: the row names xargs and the banned redirect. | cell 26-a, stderr visible | MUST |
| SH-GATE-28 | Write one `-e` per top-level alternative. An xargs-fed grep carries `-H`, `-n`, a trailing `--` and `xargs -0 -r`. Under `-E`, `\|` is a literal pipe and always wrong, while a grouped `(a\|b)` inside one pattern is alternation. | A one-file batch prints no file name without `-H`, a tracked file named `-x.sh` kills the cell (123) without `--`, and git C-quotes a non-ASCII name and xargs dies on `it's.sh` without the null separator. An alternation copied from a markdown cell as `\|` under `-E` matches the text `a\|b` and nothing else, so the check goes silently dead. Scope: all xargs-fed cells. vd-ok: the row names the xargs form and a dead pattern. | cells 28-a, 28-b, 28-c, 28-d | MUST |
| SH-GATE-29 | No verification command sits in a markdown table cell. The cell holds the cell ID and the command lives in a fenced block. | A `\|` in a cell must be escaped, and an agent reads the raw file: it gets a backslash-pipe, an invalid shell line and a dead ERE. The binding shape is itself a pipeline. 69 of 1,532 catalog rows that mention a search carried `\|`. Scope: the set's rules and skills. | cell 29-a | MUST |
| SH-GATE-30 | A check that needs lookaround, multi-line or step scope, a per-file first-line anchor beyond one awk one-liner, or more than two pipeline stages is a `checks/NAME.sh` script with awk doing the matching. The awk form is the one that is run. | python3 is absent from every stock image and awk is present on all of them. Structural checks became 8 to 12 line awk programs, red on the plant and green on the twin on five hosts, while their `-P` and `-z` originals die on BusyBox and Git Bash. Avoid `{n,}` intervals, `\b`, `nextfile` and gawk-only functions (gawk 5.0.0, mawk 1.3.4 and BusyBox awk were tested). Scope: cells that cannot be one inline ERE of about 400 characters. | the script on a plant and a twin, cell 23-a for stray `-P` | MUST |
| SH-GATE-31 | A `checks/` script that scans files writes the git list to a file by redirect and exits 74 if git fails, fails closed on an empty list (79), prechecks its tools (69) and the work tree (78), runs the xargs-fed grep with `-s`, maps any stderr text or status above 123 to 74, prints findings and exits 1, else 0. Exit map: 0, 1, 69, 74, 78, 79. | The inline PREFLIGHT only prints, a script can also refuse. The list goes to a file so git's own status counts, and `grep -s` silences exactly the "tracked but deleted" message that appears when an agent removes a file before committing. Scope: scripts that scan. `checks/shell-gate.sh` meets the intent through its own exit map because it hands the list to ShellCheck and shfmt. vd-ok: the row names xargs as a tool. | `checks/gates-scan.sh` on a plant, a twin and each fault | MUST for scripts |
| SH-GATE-33 | A supported verification host is one where the PREFLIGHT line prints nothing and the search, batching and awk tools behave as the portable set: GNU grep 3.11 or 3.12, BusyBox 1.37 with git installed, uutils coreutils 0.10.0 on ubuntu:26.04, Git Bash with git. Bare busybox and alpine images are not hosts until git (and bash, shfmt, shellcheck for the gate) are installed. | Stock alpine:3.22 and busybox:1.37 have no git, bash (alpine), python3, `rg`, shfmt or ShellCheck, and `checks/shell-gate.sh` is bash. uutils changed nothing (149 of 151 probe rows identical) and the search, find, sed and awk tools are not uutils there. Scope: runner and container choice. Not run: BSD and macOS, Git Bash 5.3.15, uutils findutils. | the container command below | SHOULD |
| SH-GATE-34 | `git grep` with `--untracked` may be used inside a script, for a single-line pattern with no leading slash, when an honest exit status (0 match, 1 none, 128 fatal) is wanted. | One process and no 123 status conflation, and `-P` works even on Alpine. Limits: on Git Bash MSYS rewrites POSIX-looking arguments so a `git grep` pattern such as `^#!/bin/sh` returns nothing with no error (`MSYS_NO_PATHCONV=1` or `[/]` fixes it), there is no per-file first-line anchor, and default `git grep` ignores untracked files. Scope: scripts, not inline cells. Reading heuristic. | reading heuristic | CONSIDER |

```bash
docker run --rm -v "${PWD}:${PWD}" -w "${PWD}" alpine:3.22 sh -c 'apk add --no-cache git >/dev/null && git config --global --add safe.directory "*" && sh CHECK'
```

`CHECK` is a file holding the cell. It prints nothing on a compliant repo, and the same cell on the bare image prints a `PREFLIGHT:` line.

## What Agents Get Wrong Here

Ranked by how often the failure appears in the measured fleet and corpus. Each has a mechanical check.

| # | Failure | Check |
|---|---|---|
| 1 | Adds `-S warning` or `--severity=warning` "to cut noise", copied from the repo's own gate | cell 01-a |
| 2 | Lints a hand-written list, `git ls-files '*.sh'` or `**/*.sh` (4 of 4 fleet gates, 10 `.bats` files unlinted) vd-ok: names the failing form | cell 09-a |
| 3 | Adds "if not installed, skip" or `\|\| true` so the gate "works everywhere" | cell 08-a, the PATH-strip block |
| 4 | Silences a finding with no reason, on SC2155 or SC2164, or keeps SC2039 (87.5% of 1,185 exemplar directives carry no reason) | cells 05-a, 06-a |
| 5 | Passes shfmt style flags "to match a house style", bypassing `.editorconfig` | cell 12-a |
| 6 | Trusts `bash -n`, `nu --ide-check` or bare `nu-check` exit codes, or expects fish to return 1 | the `SH-GATE-11` table |
| 7 | Writes an em dash in a `#!/bin/sh` comment (21 of 31 authored sh scripts) or creates a script with `cat >` and no `chmod +x` (93 of 186) | cell 16-a, the mode loop |
| 8 | Puts `severity=` or `shell=` in the rc, adds a nested rc "for tests", or mistypes an `enable=` name | cells 02-a, 03-a, 04-a |
| 9 | Installs ShellCheck or shfmt with apt, brew or webi, or trusts the runner image | cell 13-c |
| 10 | Writes a check as a recursive grep with an include option, or reaches for `-P`, `-z` or `rg` | cells 21-a, 21-b, 23-a |
| 11 | Judges a cell by exit status, adds `2>/dev/null`, or reads empty output as clean | cell 26-a, a visible stderr |
| 12 | Puts a command with `\|` in a table cell, or `\|` inside an `-E` pattern | cells 28-d, 29-a |
| 13 | Omits `-0` and `-r` from xargs, or `-H` and `--` from grep, or leaves a pathspec unquoted vd-ok: names xargs | cells 24-a, 28-a, 28-b, 28-c |
| 14 | Trusts the git-fed shape when git is missing, the directory is no repository or git refuses it | the PREFLIGHT line |
| 15 | Runs the cells in the owner's zsh without `zsh -f`, or writes `-v $T:ro` in a docker line (zsh reads `:r` as a modifier) | write `${T}:ro`, run a long block from an `sh` driver |
