---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.shell-quality-depth-on-demand"
title: Shell Testing
summary: The SH-TEST family for bats. Pinning bats, the vacuous test forms bats leaves green, the census that turns a green run red, skip budgets, the canary, hand-listed runners and the bash and sh interpreter matrix
---

# Shell Testing

Rows bind to bats-core 1.14.0 with bats-support 0.3.0 and bats-assert 2.2.4, bash 3.2.57 to 5.3, ShellCheck 0.11.0 and
shfmt 3.13.1 (measured 2026-10-11). macOS and BSD behaviour is `unverified: read only`. The bats 1.14.0 pin and the zero
skip budget are pinned decisions (adopter may override).

Owns how a bats suite is pinned, written, censused and matrixed, for every script that runs as a subprocess. The Pester
half of the family (numbers 18 to 22) is authored with the PowerShell set. Not owned here: lint and format of `.bats`
files and the gate wiring are `SH-GATE`, `local v=$(cmd)` in a test body is `SH-ERR-01`, trap and signal handler forms
are `SH-ERR`, the interpreter list and bash floors are `SH-DIAL`, `shell:` flags and pipefail of the CI step that runs
the suite are `SH-CI`, hostile-input tests of emitted code are `SH-EMIT`, the installer hatch contract is `SH-INST`, and
the 5.1 parse and smoke run are `PS-COMPAT` and `PS-GATE`. Test code obeys the dialect floor of the script it tests.

Contents: [Group 1: A Version Assertion](#group-1-a-version-assertion) ·
[Group 2: bats-lint.sh and ShellCheck](#group-2-bats-lintsh-and-shellcheck) ·
[Group 3: Reading Decides](#group-3-reading-decides) ·
[Group 4: The Census, the Canary and the Runner List](#group-4-the-census-the-canary-and-the-runner-list) ·
[Group 5: A Matrix Against the Subject](#group-5-a-matrix-against-the-subject) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Confirmed 2026-10-11 (confirmation round): the cells and check scripts of this file were replayed on 25 fresh trees under dash 0.5.12-2 and busybox ash 1.37.0 (identical output on 4,144 paired runs), GNU grep 3.12, git 2.54.0 (2.43.0 in a container), ShellCheck 0.11.0 and shfmt 3.13.1, and the hits were read on a sample. Cell defect fixed: `bats-runner-list.sh` flagged every file of a runner that names a directory (`bats tests`); it now counts the directory. The scripts `bats-lint.sh`, `bats-runner-list.sh` and the census ran on the four fresh trees with `.bats` files.

Copy `checks/*.sh` to `scripts/` (the index says so). Each script prints the violation and exits 0 clean, 1 a finding,
64 usage, 69 a tool is missing, 74 the scan failed, 78 no git work tree, 79 nothing in scope. Inline cells print the
violation and empty output is the pass. Any `PREFLIGHT:` line means stop and fix the host.

## Group 1: A Version Assertion

Set `BATS` to the vendored executable and run the cell in the CI step that runs the suite.

```sh
for t in git xargs grep awk bash; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
BATS=external/bats-core/bin/bats
bash "$BATS" --version | grep -F -x -v -e 'Bats 1.14.0'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-TEST-01 | Vendor bats-core 1.14.0, bats-support 0.3.0 and bats-assert 2.2.4 by tag (a submodule, or `git clone --branch v1.14.0`) and assert in CI that `bats --version` equals the pin. Never `apt install bats`, never the `bats` on PATH. | Distro bats lags and each old one is vacuous in its own way: Ubuntu 22.04 ships 1.2.1, where `run ! true` and `run -1 true` run the commands `!` and `-1` and pass. Debian 12 has 1.8.2, Ubuntu 24.04 1.10.0, Debian 13 1.11.1. Below 1.14.0 an empty suite exits 0 (`1..0`) and a function's own `set -e` is ignored on bash 4.0 and later. Scope: every bats suite, any script kind. bats needs bash 3.2 or later. | Cell above, printing the wrong version line (a missing bats prints on stderr). Watched red on 1.13.0, silent on 1.14.0 (measured 2026-10-11). | MUST |

## Group 2: bats-lint.sh and ShellCheck

`bats-lint.sh` takes no arguments, lists `*.bats` through git and prints `Ln file:line: text`. Its checks are L1 bare `!`,
L2 `|| true` after an assertion, L3 `[[ ]]` or `(( ))` followed by another statement, L4 `skip` with no reason, L5 a run
flag with no version directive, L6 a `run` never read, L7 an inexact failure status, L8 a commented-out test and L9 a
`skip` first in its test. ShellCheck must also parse the `.bats` files (also SH-GATE-09, SH-GATE-11).

```sh
for t in git xargs grep awk sed shellcheck; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
sh scripts/bats-lint.sh
git ls-files -z --cached --others --exclude-standard -- '*.bats' | xargs -0 -r shellcheck -f gcc -i SC2314 --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-TEST-02 | A `.bats` file that uses `run -N`, `run !` or `run --separate-stderr` names `bats_require_minimum_version 1.5.0` itself or in a `load`ed helper. Place it first in the file (that half is SHOULD). | Below 1.5.0 the flags are command names and both tests are `ok` on 1.2.1. With the directive 1.2.1 and 1.6.1 fail loudly. From 1.11.1 a missing directive only prints BW02 on stderr and exits 0, which the census turns red. A presence grep flagged 329 of 329 files in a nine-repo bats corpus and none had the hazard. Scope: every `.bats` file. | Script L5. Watched red on a plant, silent on a helper twin (measured 2026-10-11). | MUST |
| SH-TEST-03 | Never a bare `! cmd` statement in a test body: write `run ! cmd`, `run -N cmd` or `! cmd \|\| false`. | bash does not trip errexit for a negated command and only the last statement's status counts, so a mid-test `! grep -q x file` asserts nothing and is green on every bats and bash pair. Scope: every `.bats` file. A last-statement `!` works and is a false positive to read. | Script L1 and the SC2314 line of the block (error in 0.11.0). Watched red, twin silent (measured 2026-10-11). | MUST |
| SH-TEST-04 | In a suite that runs on bash below 4.1 (any 3.2 floor), end a `[[ ]]` or `(( ))` assertion with `\|\| false`, or use `[ ]` or an assertion helper. | bash 3.2 and 4.0 do not abort on a failing `[[` or `((` mid-function and 4.1 and later do. On bats 1.14.0 the plants stay green on 3.2 and 4.0 and go red on 4.1 to 5.3. macOS `/bin/bash` is 3.2.57. Scope: assertion statements only. A condition or a last statement is fine, and a suite pinned to 4.1 or later is exempt. | Script L3, the review priority list (105 listed in the nine-repo corpus, 10 mutation-proven vacuous). Run the suite on `bash:3.2` and `bash:5.3` for the proof. Watched red (measured 2026-10-11). | MUST |
| SH-TEST-05 | No `\|\| true` or `\|\| :` on an assertion or on the command under test. Cleanup in `setup` or `teardown` may use it with a same-line reason. | It turns the statement into a no-op and is green on every pair. The reason clause keeps the legal cleanup case. Scope: every `.bats` file. | Script L2, which reads assertion-shaped statements only (the plain `\|\| true` grep had 200 of 200 false positives). The command under test is a reading step. Watched red, twin silent (measured 2026-10-11). | MUST |
| SH-TEST-06 | Every `run` is followed, before the next `run` or the end of the test, by its status or output assertion: `run -N`, `run !`, `[ "$status" -eq N ]`, `assert_success`, `assert_failure N`, `assert_output` or `refute_output`. | `run` always succeeds, so a `run` whose result is never read asserts nothing. Scope: every `.bats` file. A `run` inside a helper whose callers check `$status` is a reading exemption. | Script L6, advisory on assertion-library suites (bats-assert is 204 by construction). Watched red, twin silent (measured 2026-10-11). | MUST |
| SH-TEST-07 | An expected failure pins the exact code: `run -N`, `[ "$status" -eq N ]` or `assert_failure N`. Never `-ne 0`, `-gt 0`, `!= 0` or a bare `assert_failure`. | 126 and 127 satisfy them, so a tool that is not installed passes the test and bats only warns (BW01, stderr, exit 0). Scope: every `.bats` file. A negative control paired with a passing twin may keep `-ne 0` with a stated reason. | Script L7. Watched red, twin silent (measured 2026-10-11). | SHOULD |
| SH-TEST-23 | No commented-out test: delete it, or `skip` it with a reason. | A commented-out `# @test` lowers the static count and the TAP plan together, so the census stays green. 39 in 3 of the nine corpus repos. Scope: every `.bats` file. | Script L8. Watched red, twin silent (measured 2026-10-11). Confirmed 2026-10-11: red on a commented-out `# @test` plant, silent on the twin, and 0 hits on 129 `.bats` files in 4 fresh trees (exercism/bash, rbenv/ruby-build, tj/n, jenv). | SHOULD |
| SH-TEST-24 | No `skip` as the first statement of a test. The conditional form `command -v x \|\| skip "reason"` is the legal one, and the `CENSUS_SKIP_OK` reason regex never whitelists `huh`. | An unconditional first-statement `skip` leaves the body dead and the census only reports it when the budget is empty or the reason is not whitelisted. Scope: every `.bats` file. | Script L9, and L4 for a `skip` with no reason. Watched red, conditional twin silent (measured 2026-10-11). Confirmed 2026-10-11: L9 red on a first-statement `skip` plant, silent on the conditional twin and on the 4 fresh trees; L4 lists 1,649 reasonless `[[ $BATS_RUN_SKIPPED == "true" ]] \|\| skip` lines in exercism/bash, each a real hit. | SHOULD |

```bats
bats_require_minimum_version 1.5.0
# wrong: green when the assertion fails (the [[ ]] only on bash 3.2 and 4.0)
@test "tool refuses" {
    ! grep -q old file
    [[ -n "$out" ]]
    run ./tool --bad
}
# right
@test "tool refuses" {
    run ! grep -q old file
    [[ -n "$out" ]] || false
    run -2 ./tool --bad
}
```

## Group 3: Reading Decides

No check is mechanical here. For each `&&`, `for`, `while` and `teardown`, ask whether a failing assertion there changes
the test's exit status.

```sh
for t in git xargs grep awk; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
git ls-files -z --cached --others --exclude-standard -- '*.bats' | xargs -0 -r grep -L -e 'unset __' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-TEST-08 | Make every assertion unconditional: not on the left of `&&`, not inside a loop over a list that can be empty (count the iterations and assert the count is above 0), not in the middle of `teardown` (end it with the assertion or `\|\| return 1` after each check). | `[ a = b ] && echo x`, an empty loop and a failing command mid-`teardown` are green on every bats and bash pair. ShellCheck sees none of them and no grep separates them from legal code. The bats guide says failing commands in `teardown` do not fail the test. Scope: every `.bats` file. | Reading heuristic, not watched red as a check (the vacuity itself was). | SHOULD |
| SH-TEST-16 | In-process unit tests of a POSIX library use ShellSpec pinned to a commit and run with `--fail-no-examples` under the dash, busybox ash and bash `--posix` set. Do this only when a subprocess test cannot reach the logic. | It is the only framework that runs inside dash, ash, yash and posh and it exits 0 on an empty suite by default. A failing example exits 101. Its last release is 0.28.1 (2021-01-11), so pin a commit, never latest. Scope: POSIX libraries sourced by others. | `shellspec --fail-no-examples --shell dash spec/` exits 101 on a plant, 0 on its twin and 101 on an empty suite only with the flag. Re-watched 2026-10-11 on ShellSpec 0.29.0-dev (f2d13f9): the plant exits 101 under dash 0.5.12-2, busybox ash 1.37.0 and bash `--posix`, the twin 0, and the empty suite 0 without the flag and 101 with it. | CONSIDER |
| SH-TEST-17 | Installer tests are hermetic: a dunder-prefixed hatch variable read in one function replaces the download, a localhost HTTPS fixture server with a vendored self-signed certificate replaces the release host, a stub binary records its argv, and each test's `setup` unsets every hatch variable. | The tests then run offline, assert the exact hand-off and cannot be steered by the developer's environment. TLS stays on because installers enforce `--proto =https`. A hatch that accepts a URL or runs a script is an injection path in a `curl \| sh` installer (also SH-INST-19). Scope: installer and bootstrap tests. | The listing in the block above names suites that never unset a hatch, a hit is a site to read. Reading heuristic, not watched red. | SHOULD |

## Group 4: The Census, the Canary and the Runner List

bats 1.14.0 exits 0 on a missing `-r` (nested files dropped), on any `skip`, on `run` calls that saw 126 or 127, on a
missing directive and on a short run. Run the CI step in this shape, keeping bats's own status and failing on either.

```sh
rc=0
bash "$BATS" --tap -r tests >tap.txt 2>bats.err || rc=$?
cat tap.txt bats.err
CENSUS_SKIP_OK='GNU wget not installed|timeout[(]1[)] not available' sh scripts/bats-census.sh tests tap.txt bats.err || rc=1
exit "$rc"
```

The listing, the canary and the runner list are cells.

```sh
for t in git xargs grep awk sed bash; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
git ls-files -z --cached --others --exclude-standard -- '*.bats' | xargs -0 -r grep -H -n -E -e '(^|[;&|])[[:space:]]*skip([[:space:]]|$)' --
[ -f tests-canary/failing.bats ] || echo "canary: tests-canary/failing.bats is missing"
bash "$BATS" tests-canary/failing.bats >/dev/null 2>&1 && echo "canary: the planted failing suite exited 0"
sh scripts/bats-runner-list.sh tests Makefile
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-TEST-09 | After every bats run, run the census over the test directory, the TAP file and the stderr file, and run bats with `-r` on the directory (or list the files in one place that the census checks). The census is red on a static `@test` count that differs from the TAP plan, a plan that differs from the result lines or repeats a test number, any `not ok`, any skip outside the leg's budget, any `# bats warning` line, any `BWnn:` or `bats-gather-tests` line on stderr, and a file that calls `bats_test_function` with no pinned count. The CI step fails on bats's own status as well. | Each of these is exit 0 in bats. A short run without GNU parallel printed `1..2` with 0 executed and the first census exited 0. A generator whose input vanished shrinks the plan and the static count together. Fixture `.bats` files that are test inputs go in `CENSUS_EXCLUDE`. Scope: every bats leg. `bats --count -r DIR` cannot replace the static count (it runs top-level code, descends into submodules and counts fixtures), so use `--count` on single files only. | Script `bats-census.sh` in the CI step above. A dynamic file needs `BATS` and a pin line `N path` in `TESTS_DIR/.bats-expected`. Without a TAP plan use `testing-census-junit.sh` over a junit report. Watched red on 7 plants and green on 5 twins on dash, busybox ash, bash 5.3 and bash 3.2 `--posix` (measured 2026-10-11). | MUST |
| SH-TEST-10 | Do not write `skip` for an absent interpreter or tool that the suite exists to exercise: the CI leg provisions it and the test fails when it is missing. `skip` is for a platform fact (OS, libc, a GNU-only flag) with its reason in the message, and the CI step's `CENSUS_SKIP_OK` regex for that leg names exactly those reasons. The default budget is zero. | A skipped test is `ok` and exit 0 on every bats version, so a leg that loses fish, nu or elvish goes green. In one repo 26 of 31 skip sites were an absent shell. Scope: CI legs of every bats suite. Local runs may skip with an install hint. | The `skip` listing in the block above (a hit is a site to read) and the census with the leg's budget. Watched red: a skip outside the budget exits 1, inside it 0 (measured 2026-10-11). | MUST |
| SH-TEST-11 | Keep one canary: a planted always-failing `.bats` outside the censused directory that CI runs through the same wrapper step and requires to exit non-zero. | It proves that the runner, the `shell:` flags, a trailing `\| tee` and `continue-on-error` can still go red. `bats failing.bats \| tee` under `bash -e {0}` exits 0 and under `bash -eo pipefail` exits 1. Scope: every CI job that runs a bats suite. | The canary cell above and the Canary step of the SH-TEST-12 recipe. Watched red when the canary passes, silent when it fails, on bats 1.14.0 and bash 3.2 and 5.3 (measured 2026-10-11). | SHOULD |
| SH-TEST-25 | Every `.bats` under the test tree is run by the gate, or listed in the runner's explicit exclude list. | A hand-listed runner (Go, make) runs a subset: one runner names 23 of 32 files, 9 are in no list and one listed entry is commented out. A directory census then describes tests CI never runs. Scope: repos whose runner names files. Owner default: an exclude list, not run-all. | Script `bats-runner-list.sh DIR RUNNER [EXCLUDE]` over `git ls-files` with a `:(glob)` pathspec. Watched red on an unlisted and a commented-out entry, green on the twin (measured 2026-10-11). Confirmed 2026-10-11: red on both plants and green on the twin, and clean on tj/n (`test/bin/run-all-tests` runs `bats tests`) and rbenv/ruby-build (`script/test` runs `bats test`). The first version flagged all 14 files of tj/n because the runner names the directory, not the files: that was the cell defect fixed here, and a plant with a subdirectory outside the named one stays red. | SHOULD |

## Group 5: A Matrix Against the Subject

Run the recipe commands on both images against a correct suite. The macOS leg is `unverified: read only`.

```yaml
jobs:
  bats-matrix:
    name: bats on bash ${{ matrix.bash }}
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        bash: ['3.2', '5.3']
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          submodules: recursive
      - name: Assert the pinned bats
        shell: bash
        run: '[ "$(docker run --rm -v "$PWD:/w" -w /w "bash:${{ matrix.bash }}" bash external/bats-core/bin/bats --version)" = "Bats 1.14.0" ]'
      - name: Suite and census in the bash image
        shell: bash
        run: |
          rc=0
          docker run --rm -v "$PWD:/w" -w /w "bash:${{ matrix.bash }}" bash external/bats-core/bin/bats --tap -r tests > tap.txt 2> bats.err || rc=$?
          cat tap.txt bats.err
          sh scripts/bats-census.sh tests tap.txt bats.err || rc=1
          exit "$rc"
      - name: Canary, a planted failing suite must be red through the same wrapper
        shell: bash
        run: '! docker run --rm -v "$PWD:/w" -w /w "bash:${{ matrix.bash }}" bash external/bats-core/bin/bats tests-canary/failing.bats'
  bats-macos: # unverified: read only
    runs-on: macos-26
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          submodules: recursive
      - name: Suite and census under the stock bash 3.2
        shell: bash
        run: |
          rc=0
          /bin/bash external/bats-core/bin/bats --tap -r tests > tap.txt 2> bats.err || rc=$?
          cat tap.txt bats.err
          CENSUS_SKIP_OK='only on Linux' sh scripts/bats-census.sh tests tap.txt bats.err || rc=1
          exit "$rc"
```

The subject-under-test patterns for the next two rows, run once per interpreter with `SUT_SH` and `SUT_SCRIPT` set. The
signal subject creates the file named by `STATE`.

```bats
bats_require_minimum_version 1.5.0
@test "script under ${SUT_SH:?set SUT_SH}" {
    read -r -a sut <<<"$SUT_SH"
    run -0 "${sut[@]}" "${SUT_SCRIPT:?set SUT_SCRIPT}" x
    [ "$output" = "hit" ]
}
@test "TERM stops the script with 143 and removes its temp state under ${SUT_SH:?set SUT_SH}" {
    read -r -a sut <<<"$SUT_SH"
    state=$BATS_TEST_TMPDIR/state
    STATE=$state "${sut[@]}" "${SUT_SCRIPT:?set SUT_SCRIPT}" 3>&- &
    pid=$!
    for _ in 1 2 3 4 5 6 7 8; do [ -e "$state" ] && break; sleep 0.25; done
    [ -e "$state" ]
    kill -TERM "$pid"
    (sleep 4; kill -KILL "$pid") 2>/dev/null 3>&- &
    watchdog=$!
    st=0
    wait "$pid" || st=$?
    kill "$watchdog" 2>/dev/null || true # cleanup: the watchdog may have exited
    [ "$st" -eq 143 ]
    [ ! -e "$state" ]
}
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-TEST-12 | Run the suite for a bash script on the script's floor image (`bash:3.2` unless its header pins higher) and on `bash:5.3`, plus one macOS runner leg when the script claims macOS. Capture the bats status and exit with it, then run the census. Add `bash:4.1` only for a rule with a 4.1 threshold, and a GNU-userland variant when the subject needs GNU tools. | The oldest and newest bash disagree on 3 of 13 vacuous forms. The `bash:<ver>` images are Alpine and prove bash-version behaviour only, never macOS userland, so BSD claims need the macOS leg. Scope: bash scripts that may run on macOS or on a 3.2 floor. A bash-4-only script on a pinned CI image needs only the latest. | The recipe above. Linux commands watched on `bash:3.2` and `bash:5.3` (measured 2026-10-11). The macOS leg was not run, unverified: read only. | MUST |
| SH-TEST-13 | Run a `#!/bin/sh` script under test under every interpreter of the POSIX set by passing the interpreter as data (`read -r -a sut <<<"$SUT_SH"`, then `run -0 "${sut[@]}" "$SCRIPT"`). The set is dash 0.5.12, busybox ash 1.37 and bash 3.2 `--posix`, plus posh when strict POSIX.1-2017 is claimed (also SH-DIAL-04). A distro-container matrix is equivalent when each `/bin/sh` is a member and the leg's name records which. | A `[[ ]]` under `#!/bin/sh` runs on busybox ash, mksh, ksh93u+m, yash and bash and is rejected only by dash and posh, so busybox plus bash misses it. A quoted `"$SUT_SH"` exits 127 for `busybox ash` (two words). Scope: `#!/bin/sh` scripts. macOS `/bin/sh` is bash 3.2 in POSIX mode, unverified: read only. | The first test of the block above, once per interpreter. Watched: the `[[ ]]` plant is red on dash and green on busybox ash and bash, the twin green on all (measured 2026-10-11). | MUST |
| SH-TEST-14 | A script that installs a trap has one signal test in its suite: start it in the background, send TERM, require that it stops within 4 seconds with status 143 and that its temp state is gone. Run it through the loop of SH-TEST-13 on Linux legs. | The static forms are greps and only a signal proves the handler ends the shell. `trap cleanup EXIT INT TERM` with no `exit` survives the signal and exits 0 on 11 of 11 interpreters. Scope: installers, CLIs and dev tooling that create temp state. Not Git Bash, where signal statuses differ. macOS unverified: read only. | The second test of the block above. Watched: a handler without `exit` is red (status 0) and the `trap 'exit 143' TERM` twin green on dash, busybox ash and bash (measured 2026-10-11). | SHOULD |
| SH-TEST-15 | A function sourced into a test and called under `run` that needs errexit sets `set -e` itself, or the suite passes `--errexit` (bats 1.14.0 and later). Do not assume either from an older bats run. | bats up to 1.13.0 ignores a function's own `set -e` on bash 4.0 and later and honours it on 3.2. 1.14.0 honours it on every bash but still does not stop plain commands under `run` without `--errexit`. A suite green on bash 5 with bats 1.13 can turn red on 1.14.0. Scope: suites that source a library and call functions under `run`. A `run script.sh` has its own `set -e`. | Run the plant on the pinned bats: 1.13.0 exits 1 on bash 3.2 and 0 on 4.0 to 5.3, 1.14.0 exits 1 on all. The plant is a function with `set -e; false; echo reached` called by `run -0 f`, where 0 is vacuous. Watched on bash 3.2, 4.0, 4.2, 4.4, 5.0, 5.2 and 5.3 (measured 2026-10-11). | SHOULD |

## What Agents Get Wrong Here

Ranked by how often a coding agent emits it unprompted times how long it stays green.

1. **Trusting a green bats exit.** A missing `-r`, a `skip`, a BW01 or a short run all exit 0. Run the census. `SH-TEST-09`, `SH-TEST-10`.
2. **`apt install bats` or the PATH bats.** Every distro version is vacuous differently and none is 1.14.0. `SH-TEST-01`, `SH-TEST-02`.
3. **A bare `! cmd`, a mid-test `[[ ]]` or an assertion with `|| true`.** Green on a failing assertion. `SH-TEST-03`, `SH-TEST-04`, `SH-TEST-05`.
4. **A `run` whose status is never read, or `-ne 0` and a bare `assert_failure`.** A missing tool passes as a failure. `SH-TEST-06`, `SH-TEST-07`.
5. **`skip` for an absent shell, a first-statement `skip` and a commented-out test.** The leg goes green without the tool. `SH-TEST-10`, `SH-TEST-23`, `SH-TEST-24`.
6. **Testing a `#!/bin/sh` script only under bash.** `[[ ]]` passes on the host and fails on dash. `SH-TEST-13`.
7. **One bash image or only the latest.** The 3.2 and 5.3 results disagree on three forms. `SH-TEST-12`, `SH-TEST-04`.
8. **A hand-listed runner that drifts from the tree.** New files never run. `SH-TEST-25`.
9. **A trap with no signal test.** The handler runs and the script keeps going. `SH-TEST-14`.
10. **No canary.** A trailing `| tee` or `continue-on-error` hides every red. `SH-TEST-11`.
