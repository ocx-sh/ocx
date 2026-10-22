---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Elvish Scripts and rc Snippets
summary: The SH-ELV family. The elvish 0.18.0 to 0.21.0 range, the compile plus deprecation plus denylist gate, removed forms, probing commands newer than the floor, and cleanup that survives a failing step
---

# Elvish Scripts and rc Snippets

Rows bind to elvish 0.18.0 to 0.21.0 (0.21.0, released 2024-08-14, is the newest release on 2026-10-11), measured on Linux x86_64 on 2026-10-11, with ShellCheck 0.11.0 and shfmt 3.13.1 on `checks/elv-gate.sh`. Windows and macOS behaviour, including the Windows `rc.elv` path, is `unverified: read only`.

Owns elvish source (`*.elv`): the version range, the gate, removed and deprecated forms, probing a command newer than the floor, and cleanup of temporary state. Not owned here: hook registration, idempotent rc edits and quoting of emitted strings (`SH-EMIT`), the per-family parse table and the cell dialect (`SH-GATE`), nushell churn, which shares the cleanup and signal limits (`SH-NU`), and installer exit codes (`SH-INST`). INST, EMIT, RC and CI are the script kinds of the index.

Contents: [Group 1: The Gate Decides](#group-1-the-gate-decides) · [Group 2: A Run on Both Ends Decides](#group-2-a-run-on-both-ends-decides) · [Group 3: A Listing or a Failing-Step Run Decides](#group-3-a-listing-or-a-failing-step-run-decides) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. No cell defect. The elvish cells ran on the two fresh elvish trees (zzamboni/elvish-completions, href/elvish-gitstatus) with elvish 0.21.0, and on the other 23 as negative controls.

## Group 1: The Gate Decides

Check `elv-gate`: `sh scripts/elv-gate.sh [FILE...]` for modules, scripts and generated files, and `sh scripts/elv-gate.sh --tolerate-edit FILE...` for rc files only (copy it from `checks/`). No FILE lists every `*.elv` git knows. `ELV` pins the binary. Exit 0 clean, 1 a finding, 69 elvish missing or older than 0.20.1, 74 elvish or git failed, 78 not a git work tree, 79 no file. Cells `E2` and `E2b` run the same denylists tree-wide without elvish installed. A hit is a site to fix or reword.

```sh
for t in git xargs grep tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
# E2: removed names
git ls-files -z --cached --others --exclude-standard -- '*.elv' | xargs -0 -r grep -H -n -w -e 'eawk' -e 'float64' -e 'except' --
# E2b: legacy NAME=value command, and a bare path line (implicit cd)
git ls-files -z --cached --others --exclude-standard -- '*.elv' | xargs -0 -r grep -H -n -E -e '(^|[[:space:]({;|])[A-Za-z_][A-Za-z_0-9]*=[^[:space:]=]+[[:space:]]+[{(A-Za-z]' -e '^[[:space:]]*[/~.][^[:space:]]*[[:space:]]*$' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ELV-02 | Gate every elvish file with compile, deprecation and denylist, never `-compileonly` alone, and tolerate `$edit:` compile messages in rc files only. In a shared script wrap `edit:` in `eval` (also SH-EMIT-28). Scope: INST, EMIT, RC, CI, gate needs 0.20.1 or newer. | Every removed command (`eawk`, `float64`, implicit `cd`, `a=b cmd`) compiles and then fails at run time with `executable file not found`. `-compileonly` rejects `$edit:` as `variable $edit:before-readline not found` though the line works in a live session (a pty run fired the hook once). On 0.18.0 `-json` prints `null` and a real typo passes. | `elv-gate` | MUST |
| SH-ELV-03 | Do not emit a removed form. Write `tmp E:X = 1` inside a `fn` for `X=1 cmd`, `catch` for `except`, `num` or `inexact-num` for `float64`, `re:awk` for `eawk`, `cd /path` for a bare `/path`. `!=` takes two arguments, no lone `try`, no `try` with `else` and no `catch`, `tmp` only inside a function, `with` only when the floor is 0.21.0. Scope: INST, EMIT, RC. `a=b cmd` and `eawk` and `!=` with three arguments gone in 0.21.0, `except` and `float64` gone in 0.20.0, implicit `cd` deprecated in 0.21.0. | A top-level `tmp` is `tmp may only be used inside a function`. `echo (num 1)` prints `1` on 0.18.0, 0.20.1 and 0.21.0, so `num` holds across the range. | `elv-gate`, `E2`, `E2b` | MUST |

```elvish
fn f { X=1 sh -c 'echo $X' }   # wrong: compiles on 0.21.0, then runs a program named "X=1"
fn f {
  tmp E:X = 1                  # right on 0.18.0 to 0.21.0
  sh -c 'echo $X'
}
```

Watched: `elv-gate` red on 0.21.0 (consolidation, 2026-10-11): `-compileonly` alone exits 0 on the `eawk` plant and the gate exits 1, the `except` plant is 2 and 1, the `catch` twin 0 and 0, a valid `$edit:` file is 2 and 1 and 0 with `--tolerate-edit`, and a typo beside it stays 1. The POSIX rewrite, with the `NAME=value` and implicit-cd greps added, is watched red (measured 2026-10-11) on 0.20.1 and 0.21.0 under dash, busybox ash 1.37 and bash 3.2 `--posix`: 5 plants exit 1, 4 twins exit 0, and each fault exits 69, 74, 78 or 79. `E2` plant 3 lines, twin 0. `E2b` watched red (measured 2026-10-11), plant 2 lines, twin 0, GNU grep and busybox 1.37 with git.

## Group 2: A Run on Both Ends Decides

Check `E3a`: `ELV_FLOOR` and `ELV_CEIL` name the oldest and newest elvish builds the file claims, `SUBJECT` the file. Fetch both from the elvish release archives. An rc file runs in an interactive session on each end. Pass is empty output. Watched red (consolidation, 2026-10-11): the unguarded `keep-if` plant exits 2 on 0.18.0 and 0.20.1 and passes on 0.21.0. `E3a` watched red (measured 2026-10-11): the plant prints one line for the floor build, the `resolve`-probed twin nothing.

```sh
for e in "$ELV_FLOOR" "$ELV_CEIL"; do "$e" "$SUBJECT" >/dev/null || echo "E3a: $e exit $?"; done
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ELV-01 | Target elvish 0.18.0 to 0.21.0 and run the file on both ends of the range. Scope: INST, EMIT, RC, pinned decision (adopter may override with a narrower range). | The compile check cannot see an unknown command, so a 0.21.0-only command passes on 0.18.0 and dies there with `exec: "keep-if": executable file not found`, exit 2. | `E3a` | MUST |

## Group 3: A Listing or a Failing-Step Run Decides

Checks `E3b` (a command newer than 0.20.1 in a file that never calls `resolve`), `E3c` (a temp dir in a file with no `defer`, `finally` or `catch`) and `E3d` (a signal claim). Each hit is a site to read. For SH-ELV-05 also force the failing step and list the temp dir.

```sh
for t in git xargs grep tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
# E3b: with, keep-if or os:mkdir-all, unprobed
git ls-files -z --cached --others --exclude-standard -- '*.elv' | xargs -0 -r grep -l -E -e '(^|[({;|])[[:space:]]*(with|keep-if)[[:space:]]' -e 'os:mkdir-all' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'resolve' --
# E3c: a temp dir and no cleanup construct
git ls-files -z --cached --others --exclude-standard -- '*.elv' | xargs -0 -r grep -l -e 'mktemp' -e 'temp-dir' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'defer' -e 'finally' -e 'catch' --
# E3d: a signal-safety claim
git ls-files -z --cached --others --exclude-standard -- '*.elv' | xargs -0 -r grep -H -n -i -e 'signal' -e 'sigterm' -e 'sighup' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-ELV-04 | Probe a command newer than the floor with `resolve`, never a parsed version: `fn has-cmd {\|n\| not (str:has-prefix (resolve $n) '(external')}`, then `if (has-cmd keep-if)`. Scope: EMIT, INST, floor below 0.21.0. | A new command in a dead branch compiles on 0.18.0 to 0.21.0, so no file split is needed (no release in the range added syntax). | `E3a`, `E3b` | MUST |
| SH-ELV-05 | Clean up with `defer` inside the function that owns the resource, or `try { } finally { }`, and re-exit with `$e[reason][exit-status]`. Never `exit` from a helper while a temp dir is live. Scope: INST, 0.18.0 to 0.21.0. | `defer` is `defer must be called from within a closure` at top level and does not run on `exit`, and elvish has no `trap`, so a helper's `exit` leaks the directory and an `rm` after the failing step never runs. | `E3c` | MUST |
| SH-ELV-06 | Claim no signal-safe cleanup. Keep temp files under a per-tool state directory that the next run sweeps. Scope: INST, EMIT, elvish 0.21.0, single runs (also SH-NU-14, SH-EMIT-28). | `try { sleep 30 } finally {..}` exits on SIGTERM and SIGHUP without running `finally`, and SIGINT leaves elvish alive and `finally` unrun. | `E3d` | SHOULD |

```elvish
sh -c 'exit 7'
rm -rf $d                      # wrong: never runs, the script exits 2

var code = 0                   # right: clean up, then exit with the external's own status (7)
try { sh -c 'exit 7' } catch e {
  set code = 1
  if (has-key $e[reason] exit-status) { set code = $e[reason][exit-status] }
}
rm -rf $d
exit $code
```

Watched: `SH-ELV-04` and `SH-ELV-05` red on 0.18.0 and 0.21.0 (consolidation, 2026-10-11): the plant leaves the temp dir, the `defer` and `finally` twins clean it, the `catch` twin exits 7, a top-level `tmp` exits 2. `E3b`, `E3c` and `E3d` watched red (measured 2026-10-11), plant lists the file, twin prints nothing, GNU grep and busybox 1.37 with git. `SH-ELV-06` is a heuristic, measured on single runs.

## What Agents Get Wrong Here

| Rank | Mistake | Rule | Check |
|---|---|---|---|
| 1 | Treat `elvish -compileonly` as the gate, green on `eawk`, `float64`, `a=b cmd` and implicit `cd` | SH-ELV-02 | `elv-gate` |
| 2 | Write `X=1 cmd` or `except` from shell habit and older docs | SH-ELV-03 | `E2`, `E2b` |
| 3 | Use `with`, `keep-if` or `os:mkdir-all` unguarded under a 0.18.0 floor | SH-ELV-04, SH-ELV-01 | `E3b`, `E3a` |
| 4 | Clean up with a trailing `rm`, a top-level `defer` or an `exit` in a helper | SH-ELV-05 | `E3c` |
| 5 | Run `--tolerate-edit` on every file, which hides a mistyped `$edit:` name | SH-ELV-02 | `elv-gate` |
| 6 | Promise that `defer` or `finally` runs on SIGTERM | SH-ELV-06 | `E3d` |
