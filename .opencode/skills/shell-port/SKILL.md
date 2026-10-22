---
name: shell-port
description: 'Procedure for porting a shell script to a target matrix: bash 3.2 through 5.3 (the macOS system bash), or POSIX sh on dash, busybox ash and bash --posix. Runs a baseline on every leg, the bash 4 denylist, fixes by class, reruns until the output is byte-identical, and ends in a receipt, or pins the floor with a guard instead. Use when someone says "make this work on macOS", "port this script to bash 3.2", "works on Linux but not on my Mac", "bad substitution", "declare: -A: invalid option", "mapfile: command not found", "unbound variable" on an empty array, "needs bash 4", "support the system bash", "make this POSIX sh", "run on Alpine", "run on busybox", "run on dash", "convert this bash script to sh", or "Illegal option -o pipefail". Not for a failure whose cause is unknown (shell-diagnose), shell generated for other shells (shell-integration), an install.sh and install.ps1 pair (installer-script), or PowerShell.'
license: Apache-2.0
metadata:
  summary: Ports a shell script to a bash 3.2 to 5.3 or dash, ash and bash --posix matrix with a baseline, the bash 4 denylist and a byte-identical acceptance, or pins the floor with a guard
  keywords: shell,bash,port,portability,macos,bash-3.2,posix,sh,dash,busybox,alpine,floor,mapfile,associative-array,matrix
---

# shell-port

A shell script has to run somewhere it does not run today: the macOS system
bash (3.2.57), an Alpine or Debian `/bin/sh`, or every bash from 3.2 to 5.3.
This skill carries the procedure only. The standards live in the
`shell-quality` rule, and this skill cites them by ID. It reads `dialect.md`
(SH-DIAL) and `userland.md` (SH-USER) from that rule's depth directory.

Every command and every form was run on **bash 3.2.57 to 5.3, dash 0.5.12-2,
busybox ash 1.37 and ShellCheck 0.11.0 on 2026-10-11** (measured 2026-10-11).
macOS itself, Apple's build of bash and BSD userland are `unverified: read
only`, and the receipt says so. The bash images carry Alpine userland, so a
port that touches `date`, `sed` or `readlink` also needs the SH-USER pass of
step 6. A script that must also run under Git Bash or PowerShell needs the
Windows runs the `shell-quality` CI family describes. This skill does not cover
them.
The whole procedure was replayed on 2026-10-11 on a script from a third-party tree
(the ble.sh `string-split-lines.sh` benchmark, behind a stub): the baseline read
`distinct: 2 of 8 legs` with `mapfile: command not found` on bash 3.2 only, the
replacement form of step 4 gave `distinct: 1 of 8 legs` and `oracle: match`.

Contents: [Step 1: floor and target](#step-1-read-the-floor-and-choose-the-target) ·
[Step 2: baseline](#step-2-baseline-before-any-edit) ·
[Step 3: static pass](#step-3-static-pass) ·
[Step 4: fix by class](#step-4-fix-by-class) ·
[Step 5: acceptance run](#step-5-acceptance-run) ·
[Step 6: second static pass](#step-6-second-static-pass) ·
[Pin instead](#pin-instead-of-port) ·
[Stop condition](#stop-condition) ·
[Never edit the check](#never-edit-the-check) ·
[The receipt](#the-receipt) ·
[Rules carried](#rules-this-skill-carries) ·
[MUST rows](#must-rows-this-procedure-surfaces) ·
[Other findings](#findings-that-do-not-block) ·
[What agents get wrong](#what-agents-get-wrong)

| Read | When |
|---|---|
| [references/port-legs.sh](references/port-legs.sh) | Steps 2 and 5, and the pin check: runs the script on every leg of the matrix and counts distinct results |
| [references/replacements.md](references/replacements.md) | Step 4: the 3.2 form of each bash 4 and 5 construct, the POSIX form of each bashism, and the text 3.2 prints for each |

## Before you start

The matrix runs the script inside throwaway containers with its directory
mounted read-only. It needs Docker and pulls `bash:3.2` to `bash:5.3`,
`debian:12` and `alpine:3.22` the first time. Without Docker, say so and stop:
a green ShellCheck is not a substitute (SH-CORE-08), and a `bash 5` run on the
author's machine proves nothing about 3.2.

Run only the script's no-side-effect path (`--help`, a dry-run flag, a fixed
fixture directory). A script that deletes, pushes or installs gets a driver
that calls its functions on a scratch tree. The matrix sees only the path that
executes, which is why step 3 exists.

The output must be deterministic. A clock, a PID or a temp name in the output
makes every leg differ. Fix the arguments or wrap the script in a driver that
prints deterministic facts, and run that driver on every leg. Never filter the
difference away after the fact.

## Step 1: read the floor and choose the target

Read the header before editing (SH-CORE-04). The shebang, `# Kind:` and
`# bash-floor:` sit in lines 1 to 8 (SH-DIAL-01, SH-DIAL-03).

```sh
# rename SCRIPT to the operator-named script, LEGS to this skill's references/ copy
SCRIPT=scripts/announce.sh
LEGS=.claude/skills/shell-port/references/port-legs.sh
ls -- "$LEGS"
head -n 8 -- "$SCRIPT"
```

| Header says | Target | Matrix family |
|---|---|---|
| `#!/usr/bin/env bash` and no `bash-floor` | bash 3.2.57, the unpinned floor (SH-DIAL-06) | `bash`: 3.2, 4.0, 4.2, 4.4, 5.0 to 5.3 |
| `#!/bin/sh`, or `Kind:` installer, RC or sh library | POSIX.1-2017 plus probes (SH-DIAL-04) | `sh`: dash, busybox ash, bash 3.2 `--posix`, and bash 5.3 as the oracle |
| `# bash-floor: X.Y` | the named floor | read [Pin instead](#pin-instead-of-port) |
| no header | the kind is a decision. Write the header first | ask which shells will run it |

No header means the floor is undeclared. Write `# Kind:` and the floor line
before the first fix (SH-DIAL-03) and report that the file lacked it.

**Port or pin (SH-CORE-09).** Port when the script is reachable from macOS, a
`macos-*` runner, a Taskfile or a workflow step. Pin when it is not: the file
runs only in an image that ships bash 5, and porting costs readability for no
reader. Say which in the receipt and why. When in doubt, ask who runs it.

## Step 2: baseline before any edit

The baseline is the red. Run it before touching the file and paste it
(SH-CORE-08, SH-CORE-02).

```sh
ORACLE=$(mktemp "${TMPDIR:-/tmp}/oracle.XXXXXX")
PORT_SAVE="$ORACLE" sh "$LEGS" bash "$SCRIPT"
```

For an sh target, replace `bash` with `sh`. The script prints one block per
leg and ends with `distinct: N of M legs`. Exit 1 here is the red and not an
error. `PORT_SAVE` keeps the newest bash's output as the oracle.

Read each block's first stderr line against the table in
`references/replacements.md`. A leg fails in one of four ways, and only the
first two are visible to an exit status:

1. Parse error, exit 2 at that line. A function holding the construct is never
   defined.
2. A loud error, then the script continues with empty or wrong data
   (`declare: -A: invalid option`, `mapfile: command not found`).
3. A run-time stop (`a[@]: unbound variable` on 3.2, 4.0 and 4.2).
4. Silent drift with exit 0. A construct that is only unset or reinterpreted
   (`EPOCHREALTIME` empty, `{01..03}` printing `1 2 3`) looks like success.

A baseline with `distinct: 1` means the executed path never reaches the
construct. Do not stop there. Run a path that does, or rely on step 3.

On a Mac, also run `/bin/bash ./script` by hand and paste it. It is the real
system bash. Label it `unverified: read only` if no Mac is available.

## Step 3: static pass

ShellCheck has no bash-version model. It reports 0 of the bash 4 constructs on
a file that dies on 3.2, so the denylist is the static gate for an unpinned
bash script and the matrix is the run gate.

1. Run the SH-DIAL-06 cell (d06) from `dialect.md` over the file, including its
   PREFLIGHT line and its `files()` helper. Output is the violation. Judge by
   output and keep stderr visible (SH-GATE-26), because the last `xargs`
   returns 123 on a clean tree.
2. Run ShellCheck at default severity, with the dialect of the target.

```sh
# bash target
shellcheck -- "$SCRIPT"
# sh target: SC3xxx names each bashism
shellcheck -s sh -- "$SCRIPT"
```

For an sh target, also run the SH-DIAL-07, -09, -10, -11 and -12 cells from
`dialect.md`. They are `shfiles()` cells and catch what ShellCheck prints only
with `-s sh`.

A file carrying `# bash-floor: X.Y` is skipped by the denylist and gated by
SH-DIAL-25 instead, so a marker added to dodge the denylist is the first thing
to check in the diff.

Paste both outputs. The denylist hits are the work list for step 4.

## Step 4: fix by class

One class per hunk, in this order: parse-time constructs (the file dies),
run-time loud errors, silent drift, then userland. Take each replacement from
`references/replacements.md`. Never raise the floor or add `shell: bash` to
make a failure disappear.

| Class | Typical hits | Rule |
|---|---|---|
| Parse | `\|&`, `&>>`, `;;&`, `[[ -v ]]`, `coproc`, unparenthesised `case` in `$( )` | SH-DIAL-06, SH-DIAL-27 |
| Loud | `declare -A`, `mapfile`, `${x,,}`, `${a[-1]}`, `local -n`, `wait -n` | SH-DIAL-06 |
| Run-time stop | empty array under `set -u` | SH-ERR-12 |
| Silent | `EPOCHREALTIME`, `{01..03}`, `${y/b/[&]}` | SH-DIAL-06, SH-DIAL-28 |
| Status masking | `local x=$(cmd)`, `(( i++ ))` under `-e` | SH-ERR-01, SH-ERR-14 |
| Userland | `sed -i`, `readlink -f`, `date -d`, `grep -P`, `xargs` without `-r` | SH-USER-01 to SH-USER-03, SH-USER-08 |
| sh dialect | arrays, `[[ ]]`, `$'..'`, `read -d`, `echo -n`, bare `pipefail`, bare `local` | SH-DIAL-07, SH-DIAL-09, SH-DIAL-10, SH-DIAL-12, SH-DIAL-17 |

Rules while fixing:

- Replace, do not guard. A `BASH_VERSINFO` test around a construct is allowed
  only for constructs that fail at run time, never for parse-time ones
  (SH-DIAL-27). If the construct has no 3.2 form, pin instead.
- ShellCheck's SC2207 says "prefer mapfile". The answer on a 3.2 floor is the
  read loop.
- Keep the behaviour. Where the 3.2 form loses precision (whole seconds for
  `EPOCHREALTIME`), name the loss in the receipt.
- Do not fix unrelated defects on the way. Report them with file and line. A
  port pass that finds `IFS=$'\n\t'` in a prologue reports it (SH-ERR-03).

## Step 5: acceptance run

Run the same matrix with the oracle.

```sh
PORT_ORACLE="$ORACLE" sh "$LEGS" bash "$SCRIPT"
```

The port is accepted only when the script exits 0 with both
`distinct: 1 of M legs` and `oracle: match`. That is: the output, stderr and
exit status are byte-identical on every leg and equal the unported script's
output on the newest bash (SH-CORE-08). Exit 0 on every version is not
acceptance, because 4.4 exited 0 with `timer=0` before the fix in the worked
example. `oracle: DIFFER` with one distinct block means the port changed the
behaviour. Fix the port or record the change as an intentional, named gap.

For an sh target the last leg is bash 5.3, which is the oracle and never
evidence (SH-DIAL-04). The other three legs decide.

If the unported script already differed from its own newest-bash behaviour on
purpose, say so in the receipt, with the line that makes it differ.

## Step 6: second static pass

Re-run step 3 and require all of the following.

- The denylist prints nothing.
- ShellCheck at default severity prints nothing new.
- No new `# shellcheck disable=` without a same-line reason (SH-GATE-05).
- No new GNU-only or BSD-only form. Run the SH-CORE-07 tell table cells from
  the `shell-quality` index over the file. Every hit is fixed or carries a
  same-line `# userland-ok: reason` or `# tell-ok: reason`.
- `xargs` carries `-r` everywhere (SH-USER-08).

Before reporting, run SH-CORE-01's diff cell on the change. Output is a
weakening to justify or remove.

## Pin instead of port

Use when step 1 chose pin, or when a construct has no 3.2 form (SH-DIAL-25,
SH-DIAL-27).

1. Write `# bash-floor: X.Y` in lines 2 to 8 and the guard from SH-DIAL-25 as
   the first executable statement. It exits 69 with empty stdout and one line
   on stderr. A scalar test for a major floor, a `case ${BASH_VERSION-}` guard
   for a minor floor. Array indexing never appears in a guard.
2. A sourced library has no shebang and ends its refusal as SH-DIAL-26 says.
   Source it from a caller on `bash:3.2` and check the caller survives.
3. Run the matrix with `VERSIONS` set to the span around the floor.

```sh
VERSIONS="3.2 4.0 4.2 4.4 5.0 5.3" sh "$LEGS" bash "$SCRIPT"
```

The pass is read from the blocks. Every leg below the floor shows empty stdout,
a `needs bash` line on stderr and `-> exit 69`. Every leg at or above the floor
shows the script's normal output, identical across those legs. The guard line
names the version it found, so `distinct` counts one result per refusing
version and is not the pass, and the script's own exit is 1. A leg below the
floor that shows anything else (an error from a construct before the guard, or
exit 0) is the red. The same run on the unguarded file shows it.

A script that pins and is also invoked from a Taskfile or workflow on macOS is
a defect: it fails with 69 where it used to run. Port it, or fix the caller to
use a newer bash.

## Stop condition

Stop when all four hold.

1. The **baseline was pasted** from before the first edit, with its
   `distinct:` count.
2. The **acceptance run was pasted**: `distinct: 1`, `oracle: match` and exit 0,
   or for a pin the refusal pattern of the section above.
3. Both **static passes were pasted** and the second is empty.
4. Nothing outside the port changed.

## Never edit the check

Making the matrix or the denylist go green by changing the measurement is the
failure this skill exists to prevent (SH-CORE-01).

| Edit | Why it is a violation | Rule |
|---|---|---|
| Dropping a version from `VERSIONS`, or running only the 5.x legs | The failing leg is the evidence | SH-CORE-01, SH-CORE-08 |
| Adding `# bash-floor:` with no guard, flipping `#!/bin/sh` to bash, or adding `shell: bash` to a step | The denylist skips marked files and the script now dies on 3.2 unobserved | SH-CORE-01, SH-DIAL-25 |
| `\|\| true`, `set +e` or removing `set -u` to clear an error | The failure is masked and the data is still wrong | SH-CORE-01, SH-ERR-12 |
| `# shellcheck disable=` with no same-line reason, or `2>/dev/null` on a probe | The detector is silenced | SH-CORE-01, SH-GATE-05 |
| Filtering the legs' output with `sed` or `head` before comparing | Hides the drift being measured | SH-CORE-08 |
| Comparing against the old bash's output instead of the newest bash's pre-port output | The oracle becomes the bug | SH-CORE-08 |
| Testing a `#!/bin/sh` port with `bash --posix` from bash 5.x, `zsh --emulate sh` or the author's `/bin/sh` | Those accept arrays, `[[ ]]` and `$'..'` that dash rejects | SH-DIAL-04 |

If the only fix available is one of these, record a gap with the leg and the
construct. Never apply it silently.

## The receipt

Every run ends with this, successful or not. Paste real output in fields 3, 4,
5 and 7.

```text
Receipt (shell-port)
1. Script        PATH   Kind: ...   Floor: ...   Port or pin: ... and why
2. Target matrix legs run (image tags), and any leg not run
3. Baseline      distinct: N of M, the first stderr line of each failing band
4. Static pass 1 denylist hits, ShellCheck result, sh cells (before)
5. Changes       one line per class: construct -> replacement, with line numbers
6. Lost          precision or behaviour the 3.2 forms cannot match
7. Acceptance    distinct: 1 of M, oracle: match, exit 0   (or the pin refusal blocks)
8. Static pass 2 denylist empty, ShellCheck, tell table, diff cell
9. Rule IDs      every ID relied on
10. Not verified macOS /bin/bash (Apple build), BSD userland, callers outside the matrix
```

Field 10 states `unverified: read only` for every macOS or BSD claim and never
omits the line.

## Rules this skill carries

`shell-quality` indexes no row for these two. The port procedure owns them, and
the rest of this file cites them by ID.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-CORE-08 | Port in this order: baseline run of the unported script on the target matrix (bash 3.2, 4.0, 4.2, 4.4, 5.0 to 5.3 and, for `sh`, dash and busybox), the SH-DIAL-06 denylist, fix by class, rerun the matrix, rerun the denylist. Accept the port only when the output is byte-identical on every version and equal to the unported script's output on the newest bash. A green ShellCheck is not evidence. | ShellCheck 0.11.0 exits 0 before and after the port. On bash 3.2 the bash-4 constructs fail four ways: fatal (`bad substitution`), non-fatal with rc 0 (`declare -A` leaves an indexed array), command-not-found that the script continues past (`mapfile`), and silent drift (`EPOCHREALTIME` unset, so `timer=0`). Before the port the fixture exits 2 on 3.2, 1 on 4.0 and 4.2 (`extra[@]: unbound variable`) and 0 with a wrong value on 4.4; after, 8 blocks, 1 distinct. Scope: any script that must run on bash below 4.4. macOS `/bin/bash` 3.2.57 is unverified: read only, and the `bash:3.2` image stands in. | Compare the per-version blocks of the matrix output: one distinct block equal to the pre-port newest-bash block is the pass (step 5 reads `distinct: 1 of M legs` and `oracle: match`). Judge the denylist by output, not status (SH-DIAL-06, SH-GATE-09). Watched red (measured 2026-10-11): before the port the fixture exits 2, 1, 1, 0 (wrong value), 0, 0, 0, 0 on the eight versions; after, exit 0 on all eight with one distinct block, equal to the pre-port 5.3 block. | MUST |
| SH-CORE-09 | Port a script only when it is reachable from macOS, a `macos-*` runner, a Taskfile or a workflow; pin the others with `# bash-floor: X.Y` and a `BASH_VERSINFO` guard as the first executable statement (SH-DIAL-25, SH-DIAL-27). Say which in the receipt. | Porting costs readability for no reader. The fleet has 5 bash-4+ authored files among 166 authored bash files, 1 invoked from a taskfile, and 0 `BASH_VERSION` guards. Scope: bash scripts with a bash-4+ construct. | Reading heuristic: header lines 2 to 8 carry the floor line or the file is on the port list; the mechanical halves are SH-DIAL-25 and SH-DIAL-27. Not watched red: a reading heuristic. | SHOULD |

## MUST rows this procedure surfaces

Restated as findings so a port run without the rule loaded still reports them
with the right ID. The rule text and full verification live in `shell-quality`.

| # | Finding | Rule |
|---|---|---|
| 1 | The floor in the header was not read before editing | SH-CORE-04 |
| 2 | The port started with no baseline run on the target matrix, or the baseline was not pasted | SH-CORE-08 |
| 3 | The port was accepted on a green ShellCheck, on exit 0 alone, or with output that differs between legs or from the pre-port newest bash | SH-CORE-08 |
| 4 | A check was made to pass by weakening it | SH-CORE-01 |
| 5 | A new or changed check was relied on before a plant and a twin were run | SH-CORE-02 |
| 6 | An unpinned bash file matches the 3.2 denylist | SH-DIAL-06 |
| 7 | A raised floor has no `# bash-floor:` header or no first-statement guard that exits 69 | SH-DIAL-25 |
| 8 | A sourced library with a raised floor ends its refusal with a bare `exit` | SH-DIAL-26 |
| 9 | A parse-time construct sits in the untaken branch of a version test | SH-DIAL-27 |
| 10 | A `#!/bin/sh` script was not run on dash, busybox ash and bash 3.2 `--posix`, or the evidence is from bash 5.x | SH-DIAL-04 |
| 11 | `set -o pipefail` appears bare, or after `\|\| true`, in `#!/bin/sh` code | SH-DIAL-07 |
| 12 | `#!/bin/sh` code contains `$'..'`, `read -d`, `echo -n` or `echo -e`, or a bare `local` | SH-DIAL-09, SH-DIAL-10, SH-DIAL-12, SH-DIAL-17 |
| 13 | An empty-able array is expanded unguarded under `set -u` on a floor below 4.4 | SH-ERR-12 |
| 14 | The port introduced `local x=$(cmd)` | SH-ERR-01 |
| 15 | The port introduced `(( i++ ))` as a statement under `-e` | SH-ERR-14 |
| 16 | The port introduced a GNU-only or BSD-only form, `sed -i` with no suffix, or `readlink -f` | SH-USER-01, SH-USER-02, SH-USER-03 |
| 17 | An `xargs` has no `-r` | SH-USER-08 |
| 18 | A new `# shellcheck disable=` has no same-line reason | SH-GATE-05 |

## Findings that do not block

Not in the table above. Report them with their severity and never present them
as blockers.

| # | Finding | Severity | Rule |
|---|---|---|---|
| 1 | The receipt does not say whether the script was ported or pinned and why | SHOULD | SH-CORE-09 |
| 2 | A version claim in the receipt names no tool version, image tag or date | SHOULD | SH-CORE-03 |
| 3 | A `${v/pat/rep}` with `&` or data in the replacement has no `patsub_replacement` guard | SHOULD | SH-DIAL-28 |
| 4 | `#!/bin/bash` where the header does not say system bash 3.2 | SHOULD | SH-DIAL-05 |
| 5 | `-a` or `-o` inside `[ ]` | SHOULD | SH-DIAL-11 |
| 6 | `which` instead of `command -v` | SHOULD | SH-USER-04 |
| 7 | A temp path built by hand instead of `mktemp -d "${TMPDIR:-/tmp}/name.XXXXXX"` | SHOULD | SH-USER-11 |
| 8 | A tell-table hit with no same-line waiver reason | SHOULD | SH-CORE-07 |
| 9 | A userland waiver with no reason | SHOULD | SH-USER-16 |
| 10 | Git Bash behaviour of `ln -s` or `chmod` relied on | CONSIDER | SH-USER-15 |

## What agents get wrong

Ranked by how often a port goes wrong, each with the one check that catches it.

| # | Mistake | Check |
|---|---|---|
| 1 | Takes a green ShellCheck as proof that bash 3.2 works | Step 2 baseline: `distinct` above 1 on a file ShellCheck passes |
| 2 | Accepts "exit 0 on every version" | Step 5: the `distinct:` line and `oracle: match`. Bash 4.4 exits 0 with a wrong value |
| 3 | Guards `declare -A` with an `if` and keeps it, so 3.2 builds an indexed array and exits 0 | Step 5 oracle on the 3.2 leg |
| 4 | Swaps `mapfile` for `readarray`, which needs the same bash 4.0, or for `read -a`, which splits words | Step 3 denylist hit on `readarray`, and the oracle |
| 5 | Tests `#!/bin/sh` with `bash --posix` from 5.x or the author's `/bin/sh` and calls it POSIX | Step 5: dash and ash legs of the `sh` family |
| 6 | Raises the floor in the header with no guard | Pin check: the 3.2 leg must show `-> exit 69` and empty stdout |
| 7 | Ports to "macOS" with `date -d`, `sed -i`, `readlink -f` or `grep -P` | Step 6 tell table cells and the SH-USER cells |
| 8 | Clears `unbound variable` with `set +u` or by dropping `-u` | Step 6 diff cell (SH-CORE-01) |
| 9 | Writes `set -o pipefail \|\| true` in sh | SH-DIAL-07 cell, and the dash leg aborting with exit 2 |
| 10 | Puts a parse-time construct in the untaken branch of a version test and calls it guarded | The 3.2 leg exits 2 with a syntax error before any guard runs |
| 11 | Claims macOS works from a Linux container run | Receipt field 10 reads `unverified: read only` |
| 12 | Trusts a clean matrix when the construct sits in a branch the run never executes | Step 3 denylist over the whole file, not the matrix alone |
