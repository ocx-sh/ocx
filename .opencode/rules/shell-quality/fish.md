---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Fish Scripts and Emitted Config
summary: The SH-FISH family. Gating a fish file on the floor and the ceiling, the 3.6 to 4.9 construct ledger, bash habits the fish parser accepts, control flow without errexit, empty-string semantics, exit helpers, signal cleanup, destructive paths, and the content of emitted conf.d config
---

# Fish Scripts and Emitted Config

Rows bind to fish 3.6.0, 4.0.2, 4.7.0 and 4.9.3 on Linux (measured 2026-10-11), with ShellCheck 0.11.0 and shfmt 3.13.1
named only where a row says they never touch a fish file. Pinned decision (adopter may override): the supported range
is 3.6.0 (Debian 12, LTS until 2028-06-30) to 4.9.3 (released 2026-09-08), and the floor becomes 4.0.2 after
2028-06-30. Debian 13, Alpine 3.22 and 3.23 and Fedora 43 ship 4.0.2. macOS, BSD and Windows behaviour is `unverified: read only`.

Owns fish source an agent writes or emits: functions, scripts, completion files and `conf.d` snippets. That covers
the parse gate, the version range, bash habits, control flow without errexit, empty strings, exit helpers, cleanup,
destructive paths and the content of emitted config. Not owned here: where an emitter writes a file, hook
registration and the value quoter are `SH-EMIT`. File discovery, the verification dialect and the shared exit map
are `SH-GATE`. The errexit contract of POSIX shells is `SH-ERR`. Bash and sh quoting is `SH-EXP`. zsh, nu and
elvish are `SH-ZSH`, `SH-NU` and `SH-ELV`. Kinds: INST installer, CI a step that runs fish from a bash step, DEV tooling
and pasted blocks, RC interactive config, EMIT code a tool writes for fish. A hit from a cell is a site to read. A
same-line `# tell-ok: REASON` waives it.

Contents: [Group 1: A Parse or Tool Exit Decides](#group-1-a-parse-or-tool-exit-decides) · [Group 2: The Construct Ledger](#group-2-the-construct-ledger) · [Group 3: An Awk Scan Decides](#group-3-an-awk-scan-decides) · [Group 4: Empty Strings](#group-4-empty-strings) · [Group 5: Exit, Cleanup and Destructive Paths](#group-5-exit-cleanup-and-destructive-paths) · [Group 6: Emitted Config](#group-6-emitted-config) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defect fixed: `fish-scan.sh` took a backtick inside a quoted string for command substitution (348 false hits on Homebrew's `brew.fish`, 2 on oh-my-fish) and now strips quoted text first. The remaining hits were read.

## Group 1: A Parse or Tool Exit Decides

Run f1 and f2 with each gating interpreter first on PATH (CI legs for 3.6.0, 4.0.2 and 4.9.3). A parse error exits 127 and prints on stderr, so silence is the pass. Both fish and `fish_indent` accept `--`, and a file named `-x.fish` needs it.

```sh
for t in git xargs grep awk fish; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
fishfiles() { git ls-files -z --cached --others --exclude-standard -- '*.fish'; }
# f1
fishfiles | xargs -0 -r -n 1 fish --no-config --no-execute --
# f2
fishfiles | xargs -0 -r fish_indent --check --
# f3
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.sh' | xargs -0 -r grep -H -n -E -e 'shellcheck.*\.fish' -e 'shfmt.*\.fish' --
# f4
fishfiles | xargs -0 -r grep -H -n -E -e '^#!.*env +-S' -- | grep -v -e 'tell-ok: *[^ ]'
```

Watched red (measured 2026-10-11): each cell prints on a planted file and is silent on its compliant twin, on GNU grep 3.11 and
BusyBox 1.37. f1 and f2 give the same plant and twin result on fish 3.6.0, 4.0.2, 4.7.0 and 4.9.3. f2 lists unformatted files and an unterminated
`begin`, but it exits 0 on `echo (`, so it never replaces f1. f2 and f4 are new cells, and f1 gained `--`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-FISH-01 | Gate every `.fish` file, and every fish script an emitter writes, with `fish --no-config --no-execute -- FILE` on the floor (3.6.0) and the newest release (4.9.3). Exit 127 fails. Parse a file, never a `-c` string. Use only released syntax. | `fish -c 'echo ('` exits 0 on 3.6.0 and 127 on 4.0.2 and newer, while `--no-execute FILE` exits 127 on all four. Master-only syntax (`$(cmd)` in command position, `command --`, `builtin --`) parses on fish master and fails on 4.9.3, so upstream's own HEAD files fail a downstream gate. **Scope:** CI, DEV, EMIT. fish 3.6.0 to 4.9.3. | f1, and f5 for the master-only patterns | MUST |
| SH-FISH-03 | Never hand a `.fish` file to ShellCheck or shfmt. Fish finds and gates its own files with the f1 list (also SH-GATE-09). | ShellCheck exits 1 with SC1071 or SC2148 on any fish file, `shfmt -l FILE` fails with `must be followed by then`, and `shfmt -f` and `shfmt -l DIR` skip `.fish` silently with exit 0, so shell-file discovery never reaches fish. **Scope:** CI. ShellCheck 0.11.0, shfmt 3.13.1. | f3 | MUST |
| SH-FISH-16 | Run `fish_indent --check -- FILE` from the same release as the gating interpreter, run littlecheck tests (`#RUN: %fish --no-config %s`, `# CHECK:`) on 3.6.0 and the newest release, and keep the suite clean under `fish --debug=deprecated-test` on 4.0.2 and newer. Run a script that must not read user config as `fish --no-config script.fish`. Never use `#!/usr/bin/env -S fish --no-config` for an Alpine or busybox target. | `fish script.fish` and `env fish` load `conf.d` and `config.fish`. busybox env 1.37 has no `-S` (`env -S 'echo a b'` exits 1 on Alpine 3.22). **Scope:** DEV, CI. fish 3.6.0 to 4.9.3. | f2 and f4. The littlecheck legs and the `--no-config` call sites are a reading step | SHOULD |

## Group 2: The Construct Ledger

One grep lists every construct in the ledger below. The matrix leg is the same test file run on 3.6.0, 4.0.2 and 4.9.3.

```sh
for t in git xargs grep awk; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
fishfiles() { git ls-files -z --cached --others --exclude-standard -- '*.fish'; }
# f5
fishfiles | xargs -0 -r grep -H -n -E -e '%self' -e 'bind -k' -e 'on-job-exit \$fish_pid' -e 'string sub -s 1 -l 1.*FISH_VERSION' -e '^[[:space:]]*\{[[:space:]]' -e '[|]&' -e 'string pad .*-C' -e 'status language' -e 'max-matches' -e 'string repeat [0-9]' -e 'path basename .*-E' -e '^[[:space:]]*\$\(' -e 'command -- ' -e 'builtin -- ' -e '(^|[;[:space:]])status=[0-9]+ ' -- | grep -v -e 'tell-ok: *[^ ]'
```

Watched red (measured 2026-10-11): the 13-line ledger plant prints 13 lines and the twin prints none, on GNU grep 3.11 and BusyBox 1.37. Each ledger
row differs on at least one pair of the four versions (30 probe cases). The `?` glob has no pattern.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-FISH-04 | Write scripts and emitted config in constructs that are valid and equal on 3.6.0 and 4.9.3. Run the file in CI on 3.6.0, 4.0.2 and 4.9.3 and require equal stdout and exit status. Raise the floor to 4.0.2 after 2028-06-30. | Each ledger row fails or changes meaning on some version pair. Debian 12 ships 3.6.0 and the other distros ship 4.0.2. **Scope:** INST, DEV, EMIT, CI. Pinned decision (adopter may override). | f5, then the matrix leg on all three versions | MUST |
| SH-FISH-05 | Use no construct that changes meaning or fails across the range. No `?` glob (it globs on 3.6.0 only), no `%self`, no `bind -k` (errors from 4.1), no `NAME=value` on the special `status` (ignored before 4.9.0, an error from it). No 4.1+ syntax (`{ a; b }`, `\|&`, `string pad -C`, `status language` from 4.3) and no 4.0+ option (`string match --max-matches`, `string repeat` without `-n`, `path basename -E`) while the floor is 3.6.0. | Five constructs change meaning silently (`?` globs, `for` locals persisting between iterations, the `\|\|` right side, `string repeat 3 ab` returning 1, `status=123`) and five fail loudly on the older fish while `--no-execute` stays silent. **Scope:** INST, DEV, EMIT. fish 3.6.0 to 4.9.3. | f5, then the matrix leg on 3.6.0 and 4.0.2. The `?` glob is a reading step: an unquoted `?` in a word used as a path pattern | MUST |
| SH-FISH-12 | Detect a version numerically and a feature by probe. Use `string match -r '^(\d+)\.(\d+)' -- $version` with a numeric compare, and `status test-feature NAME` (0 on, 1 off, 2 unknown). Never use the first character of the version. | `string sub -s 1 -l 1 $FISH_VERSION` answers "old" for fish 10 because `10.0.1` reads as 1. **Scope:** INST, EMIT, RC. | f5 for the first-character idiom. The numeric compare is a reading step | MUST |

## Group 3: An Awk Scan Decides

`checks/fish-scan.sh` (copy it to `scripts/`) lists the `.fish` files from git and prints `FILE:LINE: [tag] text`. Tags are `bash-habit`, `or-lhs`
and `or-and-chain`. It exits 0 clean, 1 on a finding, 69 for a missing tool, 74 when the scan fails, 78 outside a git work tree and 79 when no
`.fish` file exists.

```sh
for t in git xargs grep awk; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
sh scripts/fish-scan.sh
# (GNU grep) the PCRE original of the or-lhs tag, for comparison only: fishfiles | xargs -0 -r grep -H -n -P -e '^\s*(?!(?:if|and|or|not|while|command|test|set|string|status|functions|contains|count|builtin|type|math|path|echo|return|true|false|read|begin|_)\b)[a-z][a-z0-9_.-]*\s[^|#]*\|\|' --
```

Watched red (measured 2026-10-11): the plant prints 12 lines, the twin prints none, a same-line `tell-ok:` waives, and a file named `-x.fish` is found.
It gives the same result on dash 0.5.12, BusyBox ash 1.37, bash 3.2 `--posix` and bash 5.2 `--posix`, with gawk 5.3.2, mawk 1.3.4 and BusyBox awk. Over 1323 fish
files its tags select the same lines as the grep forms they replace (`bash-habit` 1354 of 1354, `or-lhs` 56 of 56, `or-and-chain` the one file the `-Pz` form found).

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-FISH-02 | Write no bash habit the fish parser accepts. No `set -euo pipefail`, no `set -e` or `set -u` (they mean erase), no `[[ ]]`, no backticks, no `local` or `declare`, and no `\n` or `\t` inside a double-quoted `echo` string (use `printf 'a\nb\n'`). | `set -euo pipefail` is a run-time error (`set: -euo: unknown option`, status 2) and the script keeps running. `echo "a\nb"` prints the literal `a\nb` on all four versions. **Scope:** INST, DEV, EMIT. | fish-scan.sh, tag `bash-habit` | MUST |
| SH-FISH-06 | Read the status on the next line and branch explicitly, because fish has no errexit. Use `set -l rc $status` right after the command, `$pipestatus` for a pipeline, `if` and `if not`, and explicit `exit` or `return`. Never emulate `set -e`. Never exit 123, 124 or 125 (also SH-ERR-18). | A pipeline's status is its last stage and a bare `false` mid-script does nothing, so the script goes on after a failure. **Scope:** INST, DEV. | fish-scan.sh, tag `bash-habit`, for the emulation. Status captured on the next line is a reading step | MUST |
| SH-FISH-07 | Put no unknown command (not a builtin, a function or a `command -q` probe) on the left of `\|\|` while the floor is below 4.7.0, and no `or` line directly followed by an `and` line. Probe a dependency with `command -q NAME; or ...` or `if not command -q NAME`, and replace `test -e F` then `or echo` then `and return 1` with `if not`. | Only an unknown-command left side skips the right side: `nosuch-cmd \|\| echo RHS` skips it on 3.6.0 and 4.0.2 and runs it on 4.7.0 and 4.9.3, while `nosuch; or echo` runs on all four. The fixing release lies in 4.1 to 4.6 and is unmeasured. The `or` then `and` chain returns 1 when the test succeeded. `&&` is not banned. **Scope:** INST, DEV, EMIT. fish 3.6.0 to 4.6.x. | fish-scan.sh, tags `or-lhs` and `or-and-chain`. A known function on the left is a false positive (waive it). `a; or b; and c` on one line needs a read | MUST |

## Group 4: Empty Strings

```sh
for t in git xargs grep awk; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
fishfiles() { git ls-files -z --cached --others --exclude-standard -- '*.fish'; }
# f6
fishfiles | xargs -0 -r grep -H -n -E -e 'set -q [A-Za-z_]+; *and +(echo|set|string|printf)[^;]*\$[A-Za-z_]' -e 'test -[nz] \$[A-Za-z_]' -e '\[ -[nz] \$[A-Za-z_]' -- | grep -v -e 'tell-ok: *[^ ]'
```

Watched red (measured 2026-10-11): the plant prints 3 lines and the twin none, on GNU grep 3.11 and BusyBox 1.37. With `OCX_HOME=''` the wrong form yields an empty path and the right form the default, on 3.6.0, 4.0.2, 4.7.0 and 4.9.3.

```fish
set -l wrong (set -q OCX_HOME; and echo $OCX_HOME; or echo $HOME/.ocx)    # OCX_HOME='' gives an empty path
set -l right $HOME/.ocx
set -q OCX_HOME; and test -n "$OCX_HOME"; and set right $OCX_HOME
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-FISH-09 | Treat an empty string as a set variable. Guard with `test -n "$x"` and quote every value given to `test`. Write `set -q X; and test -n "$X"` for "set and non-empty". Never write `test -n $x` or a one-argument `test foo`. | `set -q VAR` is true for `VAR=''`, so a sourced `/env.fish` appears when `OCX_HOME` is empty, and `test -n $empty` is true on all four versions. POSIX `${V:-default}` treats empty as unset, so a port from sh is wrong by default. The one-argument `test` is being removed. **Scope:** INST, EMIT, RC. fish 3.6.0 to 4.9.3. | f6, and the suite under `--debug=deprecated-test` on 4.0.2 and newer prints no `test: called with one argument` (the category does not exist on 3.6.0). `set -q _flag_NAME` after `argparse` is a benign hit | MUST |

## Group 5: Exit, Cleanup and Destructive Paths

```sh
for t in git xargs grep awk; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
fishfiles() { git ls-files -z --cached --others --exclude-standard -- '*.fish'; }
# f7
fishfiles | xargs -0 -r grep -H -n -E -e 'if not [a-z_]*(die|fatal)' -e 'if [a-z_]*(die|fatal)' -e 'while [a-z_]*(die|fatal)' -- | grep -v -e 'tell-ok: *[^ ]'
# f8
fishfiles | xargs -0 -r grep -H -l -e 'mktemp' -- | tr '\n' '\0' | xargs -0 -r grep -H -L -e 'fish_exit' --
fishfiles | xargs -0 -r grep -H -l -e 'mktemp' -- | tr '\n' '\0' | xargs -0 -r grep -H -L -e '--on-signal' --
# f9
fishfiles | xargs -0 -r grep -H -n -E -e 'set -l [A-Za-z_]* \(mktemp' -e 'rm -[a-z]* "\$[A-Za-z_]*"/' -e 'rm -[a-z]* \$[A-Za-z_]*/\*' -- | grep -v -e 'tell-ok: *[^ ]'
```

Watched red (measured 2026-10-11): f8 prints files, so judge it by the listed files and never by exit status. f7 prints 3 lines, each f8 grep lists 3 files, f9 prints 4 lines, and the twins print nothing, on GNU grep 3.11 and BusyBox 1.37. The `fatal` alternative in f7 and the two-grep split of f8 are
rewrites of one older grep that listed only files with neither `fish_exit` nor a signal handler. Behaviour, driven with a signal sent to a waiting script: the plain version leaks its temp
directory on exit 3, TERM, INT, HUP and KILL on all four versions, and the handler version below leaks only on KILL (HUP gives status 0 before 4.7.0 and 129 from it).

```fish
# wrong: a local variable, no handler, one rm at the end
set -l tmp (mktemp -d)
command rm -rf $tmp
# right: a global variable and handlers, repeated for INT and HUP
set -g tmp (mktemp -d)
function __cleanup --on-event fish_exit
    command rm -rf -- $tmp
end
function __on_term --on-signal TERM
    command rm -rf -- $tmp
    functions -e __on_term
    kill -s TERM $fish_pid
end
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-FISH-08 | Call a function that runs `exit` in statement position only: a bare line, or the right side of `; or` or `; and`. Never use it as an `if` or `while` condition, after `not`, as a pipeline stage or inside `test (...)`. A helper that a caller branches on returns a status and the caller exits. Name exit helpers `die`, `__die*` or `fatal` so f7 sees them (also SH-ERR-17). | In those positions inside a function the shell stops with status 0 (7 of 16 placements, all four versions). **Scope:** INST, DEV. | f7. A helper with another name needs a read | MUST |
| SH-FISH-10 | Register cleanup before creating a temp file or directory. `--on-event fish_exit` removes it, and `--on-signal INT`, `TERM` and `HUP` handlers each `rm`, remove themselves with `functions -e`, then re-raise with `kill -s SIG $fish_pid`. The variable is `set -g`. Promise no status for a lone INT or for HUP before 4.7.0. KILL always leaks. | `fish_exit` does not run when a signal kills the shell, a handler that does not exit leaves the script running, `exit 143` inside a handler never gives the parent 143, and a `set -l` variable is invisible to the handlers. **Scope:** INST, EMIT. fish 3.6.0 to 4.9.3. | f8 (both greps), then the behaviour run above | MUST |
| SH-FISH-11 | Validate a path variable before a destructive command and pass it as one argument after `--`. Write `test -n "$tmp"; and test -d "$tmp"; or exit 1`, then `command rm -rf -- $tmp`. Never write `rm -rf "$tmp"/*` or `rm -rf $tmp/*` with a variable that can be empty. | `"$tmp"/*` with `tmp=''` is `/*`. An unquoted `$tmp` is safe only for an empty list, not an empty string. **Scope:** INST, DEV. | f9 | MUST |

## Group 6: Emitted Config

```sh
for t in git xargs grep awk; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
fishfiles() { git ls-files -z --cached --others --exclude-standard -- '*.fish'; }
# f10
fishfiles | xargs -0 -r grep -H -n -E -e 'set -[a-zA-Z]*U' -e 'set --universal' -e 'set -[a-zA-Z]*a[a-zA-Z]* PATH' -e 'set -[gx]* PATH \$PATH' -e 'eval "function' -e "eval 'function" -e '__fish_initialized' -e 'fish_frozen_' -e 'SETUVAR' -e 'fish_add_path [^-]' -- | grep -v -e 'tell-ok: *[^ ]'
```

Watched red (measured 2026-10-11): the plant prints 9 lines and the twin none, on GNU grep 3.11 and BusyBox 1.37. f10 scans `.fish` files, so point its patterns at the template files of an
emitter written in another language. Probe for SH-FISH-15: `fish --no-config -c` with an `--on-event fish_prompt` function that prints a marker, followed by `cd /`, prints no marker on all four versions.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-FISH-13 | Emit one `conf.d/TOOL.fish`, rewritten whole, idempotent under re-source and repeated starts, persisting nothing. Use fixed-name handler functions and `fish_add_path --path`. Write no `set -U` or `set -Ua`, no `set -gxa PATH` and no `eval`-generated handler names. Never write theme or key-binding variables, `fish_variables`, `__fish_initialized` or `fish_frozen_*`. | Sourced twice, `set -gxa PATH` gives two copies and a counter-named handler fires twice. A bare `fish_add_path` writes a universal `fish_user_paths` into `fish_variables` on all four versions. 4.3+ moves theme variables into `conf.d/fish_frozen_theme.fish` and 4.8+ no longer creates `__fish_initialized`. **Scope:** EMIT, RC. The content is MUST, and the file placement stays SHOULD (also SH-EMIT-17). | f10 | MUST |
| SH-FISH-14 | Quote a value for fish with the SH-EMIT-02 form. Escape `\` first, then `'`, inside single quotes, or use `string escape --style=script` when the code runs in fish. Never use the POSIX `'\''` quoter. | Fish single quotes treat only `\\` and `\'` as escapes. The POSIX quoter loses 3 of 8 hostile values on every version, and a trailing `\` makes the file unparsable. **Scope:** EMIT. fish 3.6.0 to 4.9.3. | The SH-EMIT-02 hostile-value round trip, sourced by fish on 3.6.0, 4.0.2, 4.7.0 and 4.9.3 (also SH-EMIT-02) | MUST |
| SH-FISH-15 | Register a hook as a fixed-name function with `--on-event fish_prompt` (also SH-EMIT-26), and assume it never fires in a script. Code that must run in scripts uses `--on-variable PWD` or `fish_exit`. | `fish_prompt` and `fish_postexec` never fire in a script or `fish -c`, while `PWD`, `fish_exit` and custom events do. **Scope:** EMIT, RC. fish 3.6.0 to 4.9.3. | The probe above, and a reading step for code that depends on the hook | SHOULD |

## What Agents Get Wrong Here

| Rank | Mistake | Check |
|---|---|---|
| 1 | Bash habits written into a fish file: `set -euo pipefail`, `[[ ]]`, backticks, `local`, `echo "a\nb"` | fish-scan.sh (SH-FISH-02, SH-FISH-06) |
| 2 | Trusting a blind gate: `fish -c 'broken'` on 3.6.0 exits 0, and ShellCheck or shfmt is pointed at a fish file | f1, f3 (SH-FISH-01, SH-FISH-03) |
| 3 | An exit helper used as a condition, or bash's `trap` copied into fish with a `set -l` temp variable | f7, f8 (SH-FISH-08, SH-FISH-10) |
| 4 | Newest-syntax drift: `$(cmd)` in command position, `{ a; b }`, `\|&`, `string repeat` without `-n` | f5 (SH-FISH-05, SH-FISH-01) |
| 5 | POSIX empty-variable semantics: `set -q X; and use $X`, `test -n $x`, `rm -rf "$tmp"/*` | f6, f9 (SH-FISH-09, SH-FISH-11) |
| 6 | `\|\|` after an unknown command, and `or` then `and` chains | fish-scan.sh (SH-FISH-07) |
| 7 | Emitted config that persists or duplicates: `set -U`, a bare `fish_add_path`, `set -gxa PATH` | f10 (SH-FISH-13) |
| 8 | A version compare on the first character of `$FISH_VERSION` | f5 (SH-FISH-12) |
| 9 | The POSIX quoter reused for a fish value | The round trip (SH-FISH-14) |
