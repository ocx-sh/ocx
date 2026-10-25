---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Nushell Scripts
summary: The SH-NU family. The nu range header and two-ended gate, fall-through of removed commands to external binaries, the safe command intersection and names born after the floor, bash syntax in nu, version-selected files, try and cleanup semantics, the 65536-byte hang, autoload placement and signal limits
---

# Nushell Scripts

Rows bind to nu 0.105.1 (2025-06-10) through 0.116.1 (2026-10-04), 11 breaking minors and no 1.0, measured 2026-10-11 on
Linux x86_64 with GNU grep 3.11 and busybox 1.37 for the cells. Windows facts are nu 0.116.1. macOS, BSD and nu 0.117 or
later are `unverified: read only`. Re-run the gate on each new minor.

Owns nu files, modules, env, config and autoload snippets and nu code that other tools generate. Kinds: INST installer, EMIT
emitted code, DEV hand-written repo script, CI workflow step, RC startup snippet. Not owned here: hook and completion
registration and rc locations of emitted code are `SH-EMIT`, wiring the gate into a gate script or workflow is `SH-GATE`
and `SH-CI`, installer exit codes are `SH-INST`, elvish is `SH-ELV`. The cells assume `checks/` was copied to `scripts/`.

Contents: [Group 1: The Gate](#group-1-the-gate-decides) · [Group 2: A Grep](#group-2-a-grep-decides) · [Group 3: A Probe](#group-3-a-probe-decides) ·
[Group 4: A Reading](#group-4-a-reading-heuristic-decides) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defect fixed: `nu-bashisms` listed every `export def` (142 of 425 lines on nu-git-manager) through `-e 'export '`; it now matches `export NAME=` only. The backtick hits left (283) are doc comments and strings, the false positives the cell already names.

## Group 1: The Gate Decides

A non-zero exit or any output is the finding. Run `nu-gate` once per pinned binary, `NU` set to the floor nu and then
the ceiling nu. Exclude a module tree from the list with `':(exclude)DIR'` and gate it through `nu-gate-module`.

```sh
for t in git xargs grep nu; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# cell nu-range: files with no declared range
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -L -F -e '# nu-range:' --
# cell nu-gate: a file named with a leading dash breaks nu 0.105.1, which has no end-of-flags marker
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r "$NU" --no-config-file scripts/nu-gate.nu --strict
# cell nu-gate-module: wrap.nu holds the one line "use DIR/", relative to wrap.nu, for each module tree
"$NU" --no-config-file scripts/nu-gate.nu --strict wrap.nu
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-NU-01 | Declare `# nu-range: FLOOR..CEILING` in the file header (floor 0.105.1) and gate with the floor binary and the ceiling binary. Scope: INST, EMIT, DEV, nu 0.105.1 to 0.116.1. | 11 breaking minors in 16 months. nu_scripts has no range: 69 of 1,545 files differ across 0.105.1..0.116.0 and 35 fail on both. nupm declares none in 25 of 25 files. | Cell `nu-range` (listing, every file fixed), then `nu-gate` per binary. | MUST |
| SH-NU-02 | Gate every nu file with `nu-gate.nu --strict`, never bare `--ide-check` or `nu-check`. Gate a module tree through a wrapper holding `use DIR/`, not file by file. `--allow` takes one comma-separated string. Scope: INST, EMIT, DEV, CI. | `--ide-check` always exits 0 and, like `nu-check`, sees no deprecation (`str upcase`, `get -i`) and no removed command (it parses as an external). Standalone module files flag every module command as external: nupm 13 of 25 red, 0 wrapped on 0.116.1. A list flag is nuon on 0.110.0 and 0.111.0 and a type mismatch on 0.114.1 and 0.116.1. | Cells `nu-gate` and `nu-gate-module`. | MUST |
| SH-NU-03 | Call externals with `^`. A bareword that is neither a builtin nor a custom command is a defect, and `--allow` lists the barewords that are intended. Scope: INST, EMIT, DEV. | A removed command falls through to whatever is on PATH: the removed `size` ran GNU binutils `size` on a string, exit 0 and a wrong answer, on 0.105.1 and 0.116.1 with an `a.out` in the cwd. | Cell `nu-gate`: `--strict` prints `unknown command treated as external`. The default mode is green, which is the blindness. | MUST |

Watched red and green (measured 2026-10-11, nu 0.105.1 and 0.116.1): `nu-gate` exits 1 on `let-env`, the removed `size` and
`unfold` files and 0 on a safe twin and on `nu-gate.nu` itself. A `str upcase` file exits 0 on 0.105.1 and 1 on 0.116.1.
`nu-gate-module` exits 1 on a `let-env` module and 0 on a clean one. `nu-range` lists the planted file, not the twin.

## Group 2: A Grep Decides

Output is the violation. A hit inside a string literal (` && ` and `fetch ` in text) is a false positive, judged by reading.

```sh
for t in git xargs grep tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# cell nu-denylist: names broken on the whole range or changed inside it
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -F -e 'let-env' -e 'def-env' -e 'extern-wrapped' -e 'date format' -e 'unfold ' -e 'str collect' -e 'str find-replace' -e 'register ' -e 'with-env [' -e ' && ' -e 'do -s' -e 'do -p' -e 'fetch ' -e '$nu.home-path' -e '$nu.temp-path' -e 'str upcase' -e 'str downcase' -e 'into value' -e 'get -i' -e 'get --ignore-errors' -e 'select -i' -e 'reject -i' -e 'filter {' -e 'random dice' -e 'def run ' -e '| size' --
# cell nu-floor: names that do not exist on 0.105.1, each hit must sit in a file whose nu-range floor is newer
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -F -e '--optional' -e 'str uppercase' -e 'str lowercase' -e 'detect type' -e '$nu.home-dir' -e '$nu.temp-dir' -e ' finally ' --
# cell nu-floor-headerless: the same names in a file with no header
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -l -F -e '--optional' -e 'str uppercase' -e 'str lowercase' -e 'detect type' -e '$nu.home-dir' -e '$nu.temp-dir' -e ' finally ' -- | tr '\n' '\0' | xargs -0 -r grep -H -L -F -e '# nu-range:' --
# cell nu-bashisms: bash habits
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -F -e ' && ' -e ' || ' -e '$(' -e '$?' -e '<<' -e '2>&1' -e '`' -e '; do' -e '; then' -e '$HOME' -e 'echo -n' --
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -E -e 'export[[:space:]]+[A-Za-z_][A-Za-z_0-9]*=' --
# cell nu-guard: a runtime branch around source
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -E -e 'if .*version.*\{[[:space:]]*source' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-NU-04 | Write the safe intersection in shared and emitted nu. Never write, broken on the whole range: `let-env`, `def-env`, `extern-wrapped`, `date format`, `fetch`, `unfold`, `str collect`, `str find-replace`, `register`, `with-env [..]`, `&&`, `do -s`. Changed inside the window: `$nu.home-path` and `$nu.temp-path` (hard error from 0.110.0), `str upcase` and `str downcase` (warning from 0.114.0), `get -i` (warning from 0.106.1), `into value` (0.108.0), `def run` (a builtin from 0.114.0, an error on 0.115 and 0.116), `random dice` (gone by 0.114.1). Use instead: `$env.X =`, `def --env`, `format date`, `http get`, `generate`, `str join`, separate statements for `&&`, a `?` cell path with `default`. Name nothing `run` or after a parser keyword and prefix helpers with the tool name. Scope: INST, EMIT, DEV, nu 0.100.0 to 0.116.1. | The safe forms print byte-identical output on 0.100.0 to 0.116.1. The old forms are correct code that rots: the same text is fine on the floor and a warning or error on the ceiling. | Cell `nu-denylist` and `nu-gate` (`name_is_keyword` on 0.116.1). | MUST |
| SH-NU-05 | A file that must run on the floor uses no name born after it: `--optional` (0.106.0), `detect type` (0.108.0), `$nu.home-dir` and `$nu.temp-dir` (0.110.0), `finally` (0.111.0), `str uppercase` and `str lowercase` (0.114.0). Read a `catch` record through `$e.msg` only (`$e.json` is gone from 0.114.0). With no common spelling, select the file by `SH-NU-06` or raise the floor. Scope: INST, EMIT, DEV. | virtualenv `activate.nu` fails on 0.105.1 (`get --optional`, 1 error) and uv's template fails with 3 errors on 0.105.1 and 2 on 0.116.0. nupm uses `$nu.temp-dir` at `dirs.nu:11,65,71`. | Cells `nu-floor` and `nu-floor-headerless`. `$e.json` was measured, not grepped. | MUST |
| SH-NU-06 | Select a version-specific file with a `const` and `source`, never a runtime `if`. Floor 0.102.0 (0.100.0 and 0.101.0 fail with `not_a_const_command`). Scope: EMIT, INST. | The parser reads both files of a runtime branch, so one unknown flag in the file for the other version fails the whole script. zoxide raises its floor instead. | Cell `nu-guard`, and run the entry script on the floor and the ceiling. | MUST |
| SH-NU-08 | Write no bash syntax in nu: `&&`, the double pipe, `$?`, `$(..)`, `export X=`, `<<`, `2>&1`, backticks, `$HOME`, `echo -n`. Scope: INST, EMIT, DEV. | 9 of 13 habits are parse errors and 2 are silent: a heredoc runs `cat` and a backtick prints the word. | Cell `nu-bashisms` (false positives inside strings) and `nu-gate` for the heredoc. | MUST |

```nu
# wrong: the parser reads both files, so the one for the other version breaks the parse
if (version | get minor) >= 114 { source new.nu } else { source old.nu }
# right: only the chosen file is ever opened
const f = (if (version | get minor) >= 114 { "new.nu" } else { "old.nu" })
source $f
```

Watched red and green (measured 2026-10-11): `nu-denylist` 20 lines on a stale tree and 0 on the safe twin, `nu-floor`
6 and 0, `nu-bashisms` 12 and 0 (the `export` alternative moved to an `-E` grep on `export NAME=` after the round, still red on a plant and silent on `export def`), `nu-guard` 1 and 0, identical on GNU grep 3.11 and busybox 1.37. The runtime-`if` pair
exits 1 on 0.105.1 (`extra_positional`) and on 0.116.1, the `const` pair prints its result on both.

## Group 3: A Probe Decides

A probe runs the nu build. Exit 1 of `nu-hang` means the hazard is live on that build, so run it per pinned binary.

```sh
for t in git xargs grep nu; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# cell nu-hang: checks/nushell-hang.sh, NU names one binary
sh scripts/nushell-hang.sh
# cell nu-exit: listing of exit calls, confirm that the caller owns no temp dir
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -F -e 'exit ' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-NU-09 | Clean up by capture, clean, then `exit` with the original status. Helpers raise `error make` and only the entry point calls `exit`. Portable on 0.100.0 to 0.116.1: a `try` block that ends in `0`, a `catch` block that ends in `1`, `rm`, then `exit $code`. Scope: INST. | A failing step aborts the script, so a trailing `rm -rf` never runs, and a helper `exit` skips the caller's cleanup. A real installer leaks its `mktemp -d` directory on three error exits this way. | Cell `nu-exit`, then run the failing path and test that the directory is gone. | MUST |
| SH-NU-11 | Never leave an external in tail position of `try`, pipe it through `complete` inside the block. Scope: INST, DEV, nu 0.112.1 to 0.116.1 (0.105.1, 0.110.0 and 0.111.0 finish). | With more than 65536 bytes of output the process hangs forever. No upstream issue was found. A real installer documents the `complete` form. | Cell `nu-hang` per binary, red on 0.112.1 and 0.116.1. Grep `nu-bare-try` is a reading heuristic that matches plant and twin alike. | MUST |

```nu
# wrong: the helper's error aborts main, the directory stays
def main [] { let d = (mktemp -d); check; rm -rf $d }
# right: capture, clean, then exit with the status
def main [] { let d = (mktemp -d); let code = (try { check; 0 } catch {|e| print -e $e.msg; 1 }); rm -rf $d; if $code != 0 { exit $code } }
```

Watched red and green (measured 2026-10-11): `nu-hang` exits 1 on 0.112.1 and 0.116.1 and 0 on 0.105.1 and 0.111.0, the
twin is silent on all, identical under dash, busybox ash 1.37 and bash 3.2 `--posix`. The wrong pair leaves its directory
and the right pair removes it and exits 1, on 0.105.1 and 0.116.1. `nu-exit` is a listing.

## Group 4: A Reading Heuristic Decides

A hit is a site to read. A pass is every hit fixed or justified in the diff.

```sh
for t in git xargs grep; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# cell nu-bare-try: a try block that swallows a command failure
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -E -e 'try[[:space:]]*\{[[:space:]]*\^' -e 'try[[:space:]]*\{[^}]*\}[[:space:]]*$' --
# cell nu-listflag: a list-typed flag on a script entry point
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -E -e '--[a-z][a-z-]*:[[:space:]]*list<' --
# cell nu-startup: scripts that lean on login.nu or autoload
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -F -e 'login.nu' -e 'autoload' --
# cell nu-signal: cleanup claimed to be signal-safe
git ls-files -z --cached --others --exclude-standard -- '*.nu' | xargs -0 -r grep -H -n -i -e 'sigterm' -e 'sigint' -e 'sighup' -e 'signal-safe' --
# cell nu-autoload-dir: ask nu, in the environment the session will have
nu --no-config-file -c '$nu.vendor-autoload-dirs | last'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-NU-07 | Put emitted startup code in `$nu.data-dir/vendor/autoload/TOOL.nu` and read the directory from nu itself (cell `nu-autoload-dir`), never from `~/.local/share`. It loads for the REPL only: `nu script.nu`, `nu -c` and `nu -l -c` load no autoload directory, so a script never depends on autoload or `login.nu`. The order of `env_change` and `pre_prompt` hooks flips at 0.107.0 and `PROMPT_*` is not inherited from 0.103.0. Scope: RC, EMIT, nu 0.102.0 to 0.116.1 (also SH-EMIT-17, SH-EMIT-27). | Measured order on 0.105.1 and 0.116.1: `env.nu`, `config.nu`, `login.nu`, vendor autoload, user autoload, although the nu book lists `login.nu` last. On Windows (0.116.1) `$nu.data-dir` is `%AppData%\nushell`, so a file under `$HOME\.local\share\nushell` is never read. | Cells `nu-autoload-dir` and `nu-startup`. The order is a measurement with no plant and twin pair. | SHOULD |
| SH-NU-10 | Use `try { } finally { }` only when the declared floor is 0.116.0 or newer. Scope: INST. | A parse error below 0.111.0, `finally` ran twice on 0.111.0, and 0.114.1 continued past a failed external (exit 7). | Cell `nu-floor` lists ` finally `, compare with the `nu-range` floor. A measurement, not a red and green gate. | SHOULD |
| SH-NU-12 | Do not use a bare `try` to ignore an error you must act on. `try { ^cmd }` exits 0 on a failed command, so use a `try` with a `catch` that sets the exit code, or run the command through `complete` and read `.exit_code`. Scope: INST, DEV. | `try { ^sh -c "exit 7" }` exits 0 on 0.105.1 and 0.116.1. | Cell `nu-bare-try`, a reading heuristic. Measured, no grep was run on a plant. | MUST |
| SH-NU-13 | Make options and defaults explicit. Pass a list option as one comma-separated string and `split row ","` inside. Spell result-changing defaults: `find` is case-sensitive from 0.107.0, `each` over `null` passes `null` from 0.107.0, `*` skips dotfiles in `cp`, `mv` and `du` from 0.110.0. Re-test custom completers on 0.116.0 (read only, not run). Scope: DEV, EMIT. | `--allow '[ocx,git]'` is nuon on 0.110.0 and 0.111.0 and a type mismatch on 0.114.1 and 0.116.1. The default changes are silent and invisible to every gate. | Cell `nu-listflag`. A string flag is exit 0 on all four versions and a list flag exit 1 on 0.114.1 and 0.116.1. The defaults are a heuristic. | SHOULD |
| SH-NU-14 | Claim no signal-safe cleanup. nu has no `trap` and `finally` is skipped on SIGTERM and SIGHUP on 0.116.1. SIGINT to the child's process group runs it on nu only. Keep temp files under a per-tool state directory that the next run sweeps. Scope: INST, EMIT (also SH-ELV-06). | Single runs on nu 0.116.1. | Cell `nu-signal`, a heuristic. Measured, single runs. | SHOULD |

```nu
# wrong: exits 0 when the command fails
try { ^sh -c "exit 7" }
# right: read the status
let r = (^sh -c "exit 7" | complete)
if $r.exit_code != 0 { error make {msg: $"failed: ($r.exit_code)"} }
```

Watched (measured 2026-10-11): `nu-listflag`, `nu-startup` and `nu-signal` print 1 line on the plant and 0 on the twin,
`nu-bare-try` prints 1 on the plant and also on the `complete` twin, so a hit is read, not trusted. The wrong pair exits 0
and the right pair exits 1, on 0.105.1 and 0.116.1.

## What Agents Get Wrong Here

| Rank | Mistake | Rule | Check |
|---|---|---|---|
| 1 | Trusts a green `nu --ide-check` or `nu-check` | SH-NU-02 | `nu-gate` with `--strict` |
| 2 | Writes bash syntax: `&&`, `$(..)`, `export`, `2>&1`, heredocs | SH-NU-08 | `nu-bashisms` |
| 3 | Emits stale commands from older nu: `let-env`, `def-env`, `str collect` | SH-NU-04 | `nu-denylist` |
| 4 | Leaves a removed command as a bareword that runs a PATH binary | SH-NU-03 | `nu-gate --strict` |
| 5 | Wraps a command in a bare `try` and calls the result handled | SH-NU-12 | `nu-bare-try` |
| 6 | Ends a `try` on an external, which hangs past 65536 bytes | SH-NU-11 | `nu-hang` |
| 7 | Calls `exit` from a helper and skips the caller's cleanup | SH-NU-09 | `nu-exit` |
| 8 | Branches on `version` at runtime around `source` | SH-NU-06 | `nu-guard` |
| 9 | Uses the newest spelling in a file that must run on the floor | SH-NU-05 | `nu-floor` |
| 10 | Declares no range and gates one binary | SH-NU-01 | `nu-range` |
| 11 | Passes a list flag, or builds the autoload path by hand | SH-NU-13, SH-NU-07 | `nu-listflag`, `nu-autoload-dir` |
