---
paths:
  - "**/*.sh"
  - "**/*.bash"
  - "**/*.bats"
  - "**/*.zsh"
  - "**/*.ksh"
  - "**/*.fish"
  - "**/*.nu"
  - "**/*.elv"
  - "**/.shellcheckrc"
  - "**/.github/workflows/*.yml"
  - "**/.github/workflows/*.yaml"
  - "**/action.yml"
  - "**/action.yaml"
  - "**/Taskfile.yml"
  - "**/taskfile.yml"
  - "**/*.taskfile.yml"
  - "**/.gitlab-ci.yml"
  - "**/.gitlab-ci/*.yml"
summary: The shell quality index, holding the gate, the floor-first rule, the script kinds, the SH-CORE rules, the tell cells, and where the depth lives
keywords: shell,sh,posix,bash,zsh,ksh,fish,nushell,elvish,shellcheck,shfmt,bats,github-actions,taskfile,gitlab-ci,installer
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# Shell Quality

Traps, not tutorials. Every line names a mistake generated shell makes by default.
The shells and their manuals are already in the model, and a repository's layout is discoverable by reading it, so neither is in this file.

Contents: [The Gate](#the-gate) · [Read the Floor First](#read-the-floor-first) · [Script Kinds](#script-kinds) · [Rules This File Owns](#rules-this-file-owns) ·
[Tell Cells](#tell-cells) · [Waiver Tokens](#waiver-tokens) · [Where the Depth Is](#where-the-depth-is) · [Severity](#severity) · [Siblings](#siblings)

## The Gate

Run it after every change that touches a file this rule covers, from the repository root, and stop at the first non-zero exit.
SH-GATE-08 owns the script and its depth. Copy `checks/shell-gate.sh` from this rule's `shell-quality/checks/` directory to
`scripts/shell-gate.sh` once, copy any other script a depth file names as `checks/NAME` to `scripts/NAME` the same way, and put the
`.shellcheckrc` and `.editorconfig` templates of `gates.md` at the repository root, because the script exits 78 without them.
A script is judged by its exit status. An inline cell is judged by its output: empty stdout and empty stderr is the pass, and a
`PREFLIGHT:` line means stop and fix the host (SH-GATE-25, SH-GATE-26).

```sh
for t in git xargs grep awk shfmt shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
bash scripts/shell-gate.sh    # SH-GATE-08. Exit 0 clean, 1 a finding, 69 a tool is missing, 74 listing or reading failed, 78 no profile file or no work tree, 79 no shell files
```

The script runs ShellCheck at default severity and `shfmt -d` over the git-fed file list, the hygiene checks (CR, BOM, file mode, ASCII in `#!/bin/sh`)
and a parse-only check for zsh, fish, elvish and nu. It does not read `*.ps1` (the `powershell-quality` gate does) or lint workflow `run:` bodies (actionlint does, SH-GATE-19).

## Read the Floor First

**Before editing or diagnosing a shell file, read its header and run the identity block with the interpreter that failed (SH-CORE-04).**
The block is POSIX sh. Save it as a file and run it as `dash FILE` or `bash FILE`, so the output names the shell that failed.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CORE-04 | Read the floor first. Before editing or diagnosing a shell file, read its header (lines 1 to 8: the shebang, `# Kind:`, `# bash-floor:`, SH-DIAL-03, SH-DIAL-25) and run the identity block below with the interpreter that failed, not the one you are typing in. Paste the output in the receipt. | The same file behaves differently on shells that share a name: `set -o pipefail` aborts dash 0.5.12-2 and Ubuntu 24.04's 0.5.12-6ubuntu5 and is accepted by 0.5.12-12 (Debian 13, Ubuntu 26.04). Reading the header first stops a bash 4 edit on a floor-3.2 file. **Scope:** all kinds. On macOS `/bin/sh` is bash 3.2 in POSIX mode (unverified: read only), so the dash line prints nothing there. | The block below, its output read against the header. The header grep of SH-DIAL-03 is the mechanical half, and reading the header is a reading heuristic. Watched red (measured 2026-10-11): the probe line differs between dash 0.5.12-2 and -6ubuntu5 (rejected) and -12 (accepted). | MUST |

```sh
printf 'shell=%s\n' "${BASH_VERSION:+bash $BASH_VERSION}${ZSH_VERSION:+zsh $ZSH_VERSION}${KSH_VERSION:+ksh $KSH_VERSION}"
ls -l /bin/sh
# shellcheck disable=SC2016 # dpkg-query expands ${Version} itself, the single quotes are deliberate
command -v dpkg-query >/dev/null 2>&1 && dpkg-query -W -f='dash pkg ${Version}\n' dash
uname -sm
# shellcheck disable=SC3040 # probed in a subshell first: dash 0.5.12-2 and posh abort on an unknown set -o
if (set -o pipefail) 2>/dev/null; then echo "probe: pipefail accepted"; else echo "probe: pipefail rejected"; fi
```

## Script Kinds

Pinned decision (adopter may override): the dialect follows the kind. SH-DIAL-03 and SH-DIAL-05 own the header and the shebang, SH-ERR-23 owns
the prologue and contract. State kind and floor in lines 2 to 8 (`# Kind: ...`, and `# bash-floor: X.Y` for a pinned bash script).

| Kind | Dialect, shebang and floor | Prologue and contract |
|---|---|---|
| INST (installer, anything piped to sh), RC (rc or login snippet), EMIT for an sh target | sh: `#!/bin/sh` for an executed script, POSIX.1-2017 plus probed `pipefail` and `local`. A sourced snippet (RC or emitted) has no shebang | an executed script: `set -u` or `set -eu` plus an explicit check at every fallible call, and `main "$@"` last in an installer. A sourced snippet: no options, no `exit` (SH-ERR-23) |
| TOOL (repository tooling), CI (a script a workflow runs) | `#!/usr/bin/env bash`, floor bash 3.2.57 | `set -euo pipefail`, an explicit `\|\| die` or `\|\| return` at every fallible call |
| LIB (sourced) | no shebang, `# shellcheck shell=sh` or `shell=bash` on line 1, the callers' floor | no options, no `exit`, `return` a status |
| GATE (output is the violation) | the dialect of its kind, exit map 0, 1, 69, 74, 78, 79 | `set -u` and no `-e` |

## Rules This File Owns

The SH-CORE family is defined here and nowhere else. The numbers it skips are procedures that live in the skills and rules about the catalog itself,
retired as rows and not reused. Every other family is defined in a depth file. The shipped script is `checks/core-weaken.sh`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CORE-01 | Never make a check pass by weakening it, fix the file. The forms are a `# shellcheck disable=` with no same-line reason, `\|\| true` or `\|\| :` on a command whose failure mattered, `set +e`, a severity floor (`-S`, `--severity`, `SHELLCHECK_OPTS`, an rc `severity=`), an rc `shell=` or `disable=` key, a changed shebang or `Kind:` header to dodge SC3xxx, `shell: bash` added to hide a dialect error, a removed matrix leg, and `2>/dev/null` on a probe. | SH-GATE-01, SH-GATE-02 and SH-GATE-05 each catch one form in the committed tree and none sees what a change introduces. `set -o pipefail \|\| true` is also a wrong fix, since dash 0.5.12-2 and posh abort on the unknown option before the `\|\| true` runs. **Scope:** all kinds, all shells. | `checks/core-weaken.sh BASE`, run on the change: output is the violation, and each printed line needs a one-line reason in the commit or is removed. A removed matrix leg, an added `shell: bash` and a shebang change with the `Kind:` header left alone are reading items against the diff. Watched red (measured 2026-10-11): 13 lines on a planted change, 0 on its twin. | MUST |
| SH-CORE-02 | Watch every new or changed check go red on a plant and green on its twin before relying on it, in a fixture directory outside the repository. The plant carries the hazards that make a check lie: an extensionless shebang file, a file name with a space and, for a cell run on busybox, the host's own binaries. Paste both outputs in the receipt. | Three empty-output shapes stay silent without scanning: a clean tree and a broken pattern both exit 123 through the batch, `2>/dev/null` hides `Invalid regular expression`, and on busybox 1.37 an old-style recursive include search exits 2 with usage text but prints nothing behind `2>/dev/null \| cat`. An extension pathspec misses `bin/tool` and a space in a name breaks a newline-fed list. **Scope:** all kinds. A cell is checked on the host that will run it (SH-GATE-33). | Reading heuristic: the receipt holds a plant path and its red output. The silent-green shapes are mechanical: SH-GATE-23, SH-GATE-25 and SH-GATE-26. | MUST |
| SH-CORE-03 | State the pass condition, the tool version and the host with every verification: "ShellCheck 0.11.0, default severity, exit 0" or "dash 0.5.12-2 (Debian 12): `set -o pipefail` aborts with status 2". Name dash and bash by package or image tag, never "old dash". | Behaviour is a property of the package: Debian 12 (0.5.12-2) and Ubuntu 24.04 (0.5.12-6ubuntu5) reject `pipefail` and Debian 13 and Ubuntu 26.04 (0.5.12-12) accept it, so a statement without the version is false on half the images. **Scope:** all kinds. Date version-specific claims. | Reading heuristic: every verification names its pass condition and every interpreter claim carries a version. The identity block of SH-CORE-04 prints the version. | SHOULD |
| SH-CORE-05 | Route a failure on three facts: the exit code, the first stderr line and the interpreter identity of SH-CORE-04. For status 126 or 127 on a file that exists, test in this order: committed mode, CR byte or BOM, shebang path. Never read 127 as "interpreter missing" without those three. | 126, 127, 2 and 1 each have four or more causes and the text differs by launcher. Mode 100644 gave status 126 on 23 of 23 interpreter legs with wording that varies (`Permission denied`, `cannot execute [Permission denied]`, `can't execute`), and a CR in the shebang gives 126 or 127 with four different texts. The order is by cost and by frequency, and committed mode was the commonest cause in the repositories audited. **Scope:** all kinds on Linux, WSL and Git Bash. The cell reads the index because the work-tree mode bit is unreliable on Git Bash. | The mode cell below (output is the file name). The CR half is SH-GATE-15, and a mode is fixed with `git update-index --chmod=+x FILE` (SH-GATE-17). Routing on the three facts is a reading heuristic. Watched red (measured 2026-10-11): the plant lists `run.sh` and a file with a space in its name, the twin lists nothing, and a sourced library at 100644 without a shebang is not listed. | MUST |
| SH-CORE-16 | Run a third-party installer or init command under `env -i` with a throwaway `HOME` and explicit `ZDOTDIR`, `XDG_*` and `PATH`. Never run it in a harness with the real environment. | A harness that ran `broot --install` with an inherited `ZDOTDIR` appended a line to the real `~/.zshrc`, in a run that believed it was read-only. **Scope:** any test, harness or CI step that runs a tool which edits an rc file. | The listing cell below: each printed file calls an installer option and never names `env -i`, so each is a site to read. Watched red (measured 2026-10-11): the plant lists, the twin is silent. Confirmed 2026-10-11 on 25 fresh trees: 5 sites on 3 trees, each read (1 real call, `brew bundle --check --install` in Homebrew/brew, and 4 option texts such as `--installed` in completions and `cmake --install-prefix`). The behavioural twin (the adapter through `env -i` left `~/.zshrc` byte-identical) was watched earlier. | MUST |

```sh
for t in git xargs grep awk tr shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
# SH-CORE-05 mode cell: a committed mode of 100644 on a file whose line 1 is a shebang
git ls-files -s -- . | awk -F'\t' '$1 ~ /^100644 / { f = $2; if ((getline l < f) > 0 && l ~ /^#![ ]*[/]/) print f; close(f) }'
# SH-CORE-16 listing cell: a file that calls an installer option and never names env -i is a site to read
files | xargs -0 -r grep -H -l -e '--install' -- | tr '\n' '\0' | xargs -0 -r grep -H -L -e 'env -i' --
```

```sh
BASE=origin/main    # the branch this change merges into, rename it. git add -N makes a new file count as part of the change
git add -N . && sh scripts/core-weaken.sh "$BASE"    # SH-CORE-01. Output is the violation. Exit 1 a finding, 64 no BASE, 69 a tool is missing, 74 git failed, 78 no work tree, 79 BASE unresolved
```

## Tell Cells

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CORE-07 | Run the six tell cells over authored scripts: fix every hit or end its line with `# tell-ok: REASON` (T1 to T4 also accept `# userland-ok: REASON`, SH-USER-16). T5 passes silently for the literal `eval "$(trusted-tool ...)"`, and an `eval "$var"` hit needs `# tell-ok: captured from TOOL, status checked` (the capture form of SH-EMIT-14). | ShellCheck 0.11.0 at default severity passes all six under `#!/usr/bin/env bash`: on a ten-file plant it reported the tell itself once (SC3037 for `echo -n` in an sh script), gave four files an unrelated code and was silent on five. `source <(...)` leaves the variable empty with no error on bash 3.2.57 (also SH-EMIT-13). **Scope:** bash scripts that may run on macOS or Alpine, and sh scripts. In a Linux-only pinned script a T2 or T3 hit is fine with a reason, and `eval "$(tool activate)"` is the intended form in documentation scripts. BSD `sed -i ''` and macOS `readlink -f`: unverified: read only. The MUST for each tell lives in its owner rule, so this row only finds. | The cells below, PREFLIGHT first. Output is the violation, nothing under a cell is the pass, and T1 runs on sh files only. Watched red (measured 2026-10-11): all six print on a planted tree, including an extensionless `bin/tool` that an extension pathspec misses, and the twin is silent. | SHOULD |

```sh
for t in git xargs grep awk shfmt; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
shfiles() { files | xargs -0 -r awk 'FNR == 1 && /^#![ ]*\/(usr\/)?bin\/(env )?sh([ \r]|$)/ { print FILENAME }' | tr '\n' '\0'; }
# T1 echo -n, -e or -E, sh files only. Owners SH-DIAL-12 (sh) and SH-EXP-09 (data)
shfiles | xargs -0 -r grep -H -n -e 'echo -[neE]' -- | grep -v -e 'tell-ok: *[^ ]' -e 'userland-ok: *[^ ]'
# T2 readlink -f, readlink -m, realpath -m. Owner SH-USER-03
files | xargs -0 -r grep -H -n -e 'readlink -f' -e 'readlink -m' -e 'realpath -m' -- | grep -v -e 'tell-ok: *[^ ]' -e 'userland-ok: *[^ ]'
# T3 sed -i with no suffix, sed --in-place. Owner SH-USER-02
files | xargs -0 -r grep -H -n -e 'sed -i[[:space:]]' -e 'sed --in-place' -- | grep -v -e 'tell-ok: *[^ ]' -e 'userland-ok: *[^ ]'
# T4 a literal /tmp/. Owner SH-USER-11
files | xargs -0 -r grep -H -n -e '[ "=]/tmp/' -- | grep -v -e 'tell-ok: *[^ ]' -e 'userland-ok: *[^ ]'
# T5 eval on a non-literal operand. Owners SH-EXP-13 and SH-EMIT-14
files | xargs -0 -r grep -H -n -w -e eval -- | grep -v -e 'eval "[$](' -e 'tell-ok: *[^ ]'
# T6 source <( and . <(. Owner SH-EMIT-13
files | xargs -0 -r grep -H -n -e 'source <(' -e '\. <(' -- | grep -v -e 'tell-ok: *[^ ]'
```

ShellCheck owns the rest: `function f`, `[[`, `$'..'`, arrays and an unguarded `pipefail` in an sh file are SC2113, SC3010, SC3003, SC3030 and SC3040
(SH-DIAL-02, SH-DIAL-07, SH-DIAL-08, SH-DIAL-09), and `which` is SC2230 through the `deprecate-which` key of the profile (SH-USER-04).

## Waiver Tokens

Four tokens exist. A waiver ends the line it excuses and carries a reason. A bare token or an empty reason is still printed by the filter
`grep -v -e 'TOKEN: *[^ ]'` and counts as a defect.

| Token | Owner | Waives |
|---|---|---|
| `vd-ok: REASON` | SH-GATE-21 to SH-GATE-29 | a catalog line that must show a banned form, so a self-check cell skips it |
| `userland-ok: REASON` | SH-USER-16 | a GNU-only or BSD-only command form, and the tells T1 to T4 |
| `sec-ok: REASON` | SH-SEC | a hit of a SH-SEC cell (a bare-name source, a PATH edit, a lock test, `sudo -E`) |
| `tell-ok: REASON` | SH-CORE-07 | a hit of a tell cell, and of a listing cell in SH-ERR, SH-EXP and SH-FISH |

## Where the Depth Is

Read the file for the work you are about to do, not for the topic it is filed under. One level deep: these files do not point at each other,
so a task that spans two families names both. The rule loads on the globs above and on nothing else. A script with no extension (`bin/tool`),
`.bashrc`, a Makefile recipe or a Dockerfile `RUN` loads nothing, so route them from this table by what the code does.

| Doing… | Read |
|---|---|
| Choosing `#!/bin/sh` or bash, declaring a bash floor or a `# Kind:` header, enabling pipefail in sh, using a POSIX.1-2024 feature, `local` in sh, a shebang argument, or testing an sh script on dash and busybox | [shell-quality/dialect.md](shell-quality/dialect.md) |
| Calling sed, grep, find, mktemp, readlink, date, stat, timeout, sha256sum, curl or wget so it runs on GNU, BSD and macOS, busybox and uutils | [shell-quality/userland.md](shell-quality/userland.md) |
| Choosing `set -e`, `set -u` or pipefail for a script's kind, writing a trap or cleanup, picking an exit code, handling a signal, an empty array under `set -u`, a glob loop or `nullglob`, or error propagation through a subshell, `$( )` or a pipe | [shell-quality/errors.md](shell-quality/errors.md) |
| Quoting a variable, building an argument list, looping over files or lines, choosing echo or printf, a here-doc, arithmetic on input, a test operator, brace or glob expansion, `eval`, option parsing, or passing a value through ssh or `sh -c` | [shell-quality/expansion.md](shell-quality/expansion.md) |
| Putting a token, password or credential on a command line, in git or cargo auth or under `set -x`, sourcing a file, extending PATH, calling sudo, taking a lock, passing data to ssh, git, rm or tar, or taking a variable name from data | [shell-quality/security.md](shell-quality/security.md) |
| Setting up or changing ShellCheck, shfmt, `.shellcheckrc`, `.editorconfig`, the gate script, the parse gate of a shell family, line endings or file modes, or writing a verification cell | [shell-quality/gates.md](shell-quality/gates.md) |
| Writing or running bats tests, isolating a test, setting a skip budget, counting tests, or testing across bash and sh versions | [shell-quality/testing.md](shell-quality/testing.md) |
| Editing a workflow `run:` step, a composite action, a Taskfile `cmds` item or a GitLab `script:` block: the default shell, an untrusted `${{ }}`, pipefail scope, runner files, Windows and macOS runners | [shell-quality/ci.md](shell-quality/ci.md) |
| Enabling pipefail or writing a bashism in a `run:` step, a Taskfile `cmds` item or a script that a `shell: sh` step runs | [shell-quality/dialect.md](shell-quality/dialect.md) and [shell-quality/ci.md](shell-quality/ci.md) |
| Writing or sourcing zsh code: emulation, word splitting, arrays, options, startup files, a zsh floor, or a pasted block that globs | [shell-quality/zsh.md](shell-quality/zsh.md) |
| Writing a fish function, completion or `conf.d` snippet, or fish code a tool generates for fish 3.6 to 4.9 | [shell-quality/fish.md](shell-quality/fish.md) |
| Writing a nushell script, module or env, config or autoload snippet, or nu code a tool generates across nu releases | [shell-quality/nushell.md](shell-quality/nushell.md) |
| Writing an elvish module or `rc.elv` snippet, or elvish code a tool generates | [shell-quality/elvish.md](shell-quality/elvish.md) |
| Generating shell code for other shells from Rust, Go or Python: quoting a value per target, editing an rc or profile file, registering a hook or completion, a Windows cmd or PowerShell target | [shell-quality/emit.md](shell-quality/emit.md) |
| Writing or reviewing an `install.sh`: the truncation guard, integrity check, TLS and wget flags, platform detection, prompts, privilege, PATH edits, the exit table or retry | [shell-quality/installers.md](shell-quality/installers.md) |
| Writing the sh of a curl-piped installer, or the sh an emitter prints | [shell-quality/installers.md](shell-quality/installers.md) or [shell-quality/emit.md](shell-quality/emit.md), plus [shell-quality/dialect.md](shell-quality/dialect.md) |
| Writing a `.ps1`, `.psm1` or `.psd1` file or a Pester test | `powershell-quality` (sibling rule: PS-COMPAT, PS-ERR, PS-NATIVE, PS-ENC, PS-GATE, PS-SEC) |
| Writing a header comment or a comment block in a script | `code-docs` (sibling rule) |
| Emitting code for N shells, writing an installer pair, porting to a target matrix, or diagnosing a shell failure from its symptom | the `shell-integration`, `installer-script`, `shell-port` and `shell-diagnose` skills |

## Severity

MUST = Block: fix before it lands. SHOULD = Warn: fix, or state why not in the commit body. CONSIDER = Suggest: never blocks, never re-raised after a decline.
Every MUST row in a depth file names the script kind it binds, so the rows that block a single edit stay few.

A row with `provisional: true` on its Scope line has a watched check and waits for a confirmation round on fresh trees. The 2026-10-11 round used 25 (amtoine/nu-git-manager, bpkg/bpkg, docker-library/official-images, exercism/bash, franciscolourenco/done, gitlab-development-kit, Homebrew/brew, href/elvish-gitstatus, jenv/jenv, jethrokuan/z, mathiasbynens/dotfiles, oh-my-fish/oh-my-fish, ohmybash/oh-my-bash, openssh/openssh-portable, outline-server, rbenv/ruby-build, sorin-ionescu/prezto, thoughtbot/rcm, tj/n, tmux-plugins/tpm, twpayne/chezmoi, wailsapp/wails, yadm-dev/yadm, zdharma-continuum/zinit, zzamboni/elvish-completions) and cleared every row.
The pinned decisions encode an agreed choice rather than a derivable fact, and each is a default an adopter may override once,
in the repository's shared config and never per file: the script-kind table above, the floors (unpinned bash 3.2.57, sh at POSIX.1-2017 plus probes,
zsh 5.8 authored and 5.0.8 emitted, fish 3.6.0 to 4.9.3, nu `# nu-range: FLOOR..CEILING`, elvish 0.18.0 to 0.21.0), the exit maps (installers 0 to 7,
gate scripts 0, 1, 69, 74, 78, 79), the tools (ShellCheck 0.11.0, shfmt 3.13.0 or newer, bats 1.14.0, actionlint and zizmor at their default and medium floors) and a zero skip budget.
Overriding one is a recorded decision, and re-arguing one in a pull request is not a review comment.

## Siblings

- **`powershell-quality`**: `*.ps1`, `*.psm1` and `*.psd1`, with its own gate wrapper, and the Pester rules live in its `gates.md`.
  A PowerShell `run:` step is checked by the SH-CI-11 and SH-CI-12 copies in `ci.md`, because a workflow edit loads this rule and not that one.
- **`code-docs`** and the `code-docs-cleanup` skill: comments and header blocks in every language, including the `# Kind:` and `# Exit:` headers this rule asks for.
- **`shell-integration`, `installer-script`, `shell-port` and `shell-diagnose`**: procedures that cite these IDs by number and never restate them.
  The `shell-essentials` bundle ships all of them with this rule, `powershell-quality`, `code-docs` and `code-docs-cleanup`.

**Dates.** Version claims in this rule set are dated 2026-10-11 and were measured on ShellCheck 0.11.0, shfmt 3.13.1, bash 3.2.57 to 5.3, dash 0.5.12, busybox 1.37, zsh 5.0.8 to 5.9, fish 3.6.0 to 4.9.3,
nu 0.105.1 to 0.116.0 and elvish 0.21.0. macOS and BSD rows are unverified: read only. Dated end points: pwsh 7.4 ends support 2026-11-10, `ubuntu-latest` moves to 26.04 by 2026-11-19 (SH-CI-15), and the fish 3.6.0 floor holds until 2028-06-30.
