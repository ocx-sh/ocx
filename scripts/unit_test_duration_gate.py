#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""One-second budget over every Rust unit test, read off a nextest run log.

    task rust:test:duration
    python3 scripts/unit_test_duration_gate.py target/nextest/run.log
    python3 scripts/unit_test_duration_gate.py <log> --budget-seconds 0.5 --allowlist <file>

A unit test is a function call with its dependencies faked. One that takes a
second is waiting on something real — a lock timeout, a retry backoff, a
process start, a whole-tree scan — and the suite pays for it on every gate
run, on every runner, forever. This names them.

Reads the log `rust:test:unit` tee'd to `target/nextest/run.log`, the same file
`rust:test:ceiling` parses and inside the same bracket: `rust:test:floor`
deletes the log before the run, so a line read here can only be this run's.

**The reader floor is the point, not a detail.** A derived check that reads
nothing is indistinguishable from a clean tree: a `-- <filter>` subset run, a
log truncated by a cancelled gate, or a nextest that reshaped its result
grammar each leave this script with no over-budget test to report and a green
to print. So the number of timed result lines it actually parsed is compared
against `crates/NEXTEST_FLOOR` — the same floor `rust:test:floor` holds the
listed test count to — and anything short is a red that names both numbers.
Fail-closed in the same direction: a result kind this grammar does not know
is not counted, so a nextest that renames one starves the reader rather than
quietly exempting the tests it printed.

Grammar, from nextest 0.9.144 with and without `--profile ci`'s progress
counter:

    PASS [   0.003s] (5911/8215) ocx_project hash::tests::hash_accepts_tags
    PASS [   0.003s] ocx_project hash::tests::hash_accepts_tags
    SLOW [>120.000s] ocx_shell shell::tests::live_repeated_eval_is_byte_identical

The test id is everything after the optional `(N/M)` counter: `<binary id>
<test name>`, exactly as nextest prints it and exactly as the allowlist spells
it. A `SLOW` line and the `PASS` that follows it name the same test, so the
slowest duration per id wins; `FLAKY n/m` is read for the same reason a retry
must not be a way under the budget.

`crates/NEXTEST_SLOW_ALLOWLIST` carries one entry per line — an exact test id,
or an `fnmatch` pattern for a family slow for one structural reason
(`ocx_shell *live_*`). A pattern matching `CANARY` below is refused outright:
one fat-fingered `*` would otherwise disable the gate while the file still
read as a careful list. An entry is a
debt, not a licence — see that file's header. An entry that ran *under* budget,
or did not run at all, is NOT a failure: `live_powershell_*` needs `pwsh` on
PATH and a machine-dependent red is worse than a stale line, so those print as
`stale allowlist candidate: <id>` for a reader to act on.

**Hard where the measurement is controlled, advisory where it is not.** With
`CI` set the gate is a gate: an over-budget test no allowlist line covers exits
1. Without it the same findings print and the run exits 0, because on a shared
machine a fast test measures its neighbours — four tests taking 6 ms to 190 ms
quiet, and 12 ms to 160 ms on CI, were measured at 1.0 s to 1.2 s on a 32-core
box under load 27. Allowlisting those would file a false reason for tests that
are fast everywhere it matters, and a gate that reds on a busy laptop for code
nobody touched is a gate someone turns off.

That is not an off-switch: `verify-basic.yml`'s smoke job runs on every pull
request with `CI` set, so the hard mode runs on every change. Both modes print
which one they ran in, on every run, green or red. Only the *budget* verdict is
advisory — a log this reader cannot judge, and an unbounded allowlist pattern,
fail in both modes, being wiring faults rather than slow tests.

Stdlib only; `task rust:test:duration` is the caller, and
`scripts/tests/test_unit_test_duration_gate.py` holds the fixture logs that show
this script red on an over-budget test, red on a short log, and green.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from collections.abc import Iterable
from fnmatch import fnmatchcase
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
FLOOR = REPO_ROOT / "crates" / "NEXTEST_FLOOR"
ALLOWLIST = REPO_ROOT / "crates" / "NEXTEST_SLOW_ALLOWLIST"

#: The colour nextest writes when it is talking to a terminal, which `tee`
#: preserves — stripped as `rust:test:ceiling`'s `sed -E` does, plus the form a
#: log fetched with `gh run view --log` carries: that transport renders the
#: escape as the two literal characters `^[`, and the codes sit INSIDE the test
#: id (`<esc>ocx<esc> <esc>api::tests<esc>::<esc>name<esc>`), so a reader that
#: strips only one spelling parses ids nothing can ever match.
ANSI = re.compile(r"(?:\x1b|\^\[)\[[0-9;]*m")

#: `gh run view --log` prefixes every line `<job>\t<step>\t<ISO-8601 Z> `.
#: Stripped so a CI log can be read by hand with this gate; matched as its own
#: exact shape rather than by unanchoring the result pattern, which would let
#: a test's own captured output be read as a result line.
GH_LOG_PREFIX = re.compile(r"^[^\t]*\t[^\t]*\t\d{4}-\d{2}-\d{2}T[0-9:.]+Z ")

#: One timed result line. Every numeric field nextest prints is RIGHT-ALIGNED,
#: which is the bug this pattern was written around twice: the progress counter
#: is `(   1/8215)` for the first 999 tests and `(8215/8215)` for the last, so
#: `\(\d+/\d+\)` matches none of the padded ones. Because the counter group is
#: optional, that did not fail loudly — it silently glued `(   1/8215)` onto
#: the front of every early test id, which no allowlist entry can match. The
#: `>` is nextest's "still running" prefix on a `SLOW` line (`[>120.000s]`).
RESULT = re.compile(
    r"^ *(?:PASS|FAIL|SLOW|LEAK|TIMEOUT|TRY \d+ FAIL|FLAKY \d+/\d+)"
    r" +\[ *>? *(\d+\.\d+)s\] +(?:\( *\d+ */ *\d+ *\) +)?(\S.*)$"
)


def durations(lines: Iterable[str]) -> tuple[dict[str, float], int]:
    """Slowest duration per test id, and how many timed result lines were read.

    The line count is the reader floor's subject and is deliberately not the
    size of the mapping: a `SLOW` line and its eventual `PASS` are two lines
    for one test, and the floor's question is "did this log describe a whole
    run", which lines answer and unique ids under-answer."""
    slowest: dict[str, float] = {}
    read = 0
    for line in lines:
        match = RESULT.match(GH_LOG_PREFIX.sub("", ANSI.sub("", line)))
        if match is None:
            continue
        read += 1
        test = match.group(2).strip()
        slowest[test] = max(slowest.get(test, 0.0), float(match.group(1)))
    return slowest, read


#: An id belonging to no crate and naming no family. A pattern that matches it
#: is unbounded — `*`, `* *`, `?*` — and one unbounded line silently disables
#: the whole gate while leaving a file full of careful-looking entries. Every
#: legitimate pattern pins something real: a crate (`ocx_shell *live_*`) or a
#: name fragment (`*live_*`), neither of which this id carries.
CANARY = "zzz_no_such_crate zzz_no_such_test"


def allowlist(path: Path) -> list[str]:
    """Patterns the budget tolerates, in file order. `#` starts a comment
    anywhere on a line — no Rust test id can contain one, so this cannot eat
    part of an entry.

    A pattern is an `fnmatch` glob, which for an entry carrying no metacharacter
    is exactly equality — so one code path serves both spellings. The glob is
    what keeps a *family* from rotting into a dozen stale lines:
    `ocx_shell *live_*` names the shell tests that spawn a real interpreter,
    anchored on their naming convention, so an ordinary `ocx_shell` unit test
    that goes slow is still caught."""
    entries = [
        entry
        for line in path.read_text(encoding="utf-8").splitlines()
        if (entry := line.split("#", 1)[0].strip())
    ]
    unbounded = [entry for entry in entries if fnmatchcase(CANARY, entry)]
    if unbounded:
        raise SystemExit(
            f"nextest duration: {path} holds {len(unbounded)} unbounded pattern(s) "
            f"{unbounded} — each matches {CANARY!r}, an id belonging to no crate, so it "
            "allows every test in the workspace and this gate would pass over any duration "
            "at all. Pin the crate or a name fragment"
        )
    return entries


#: Where the measurement is controlled. GitHub Actions always sets `CI`, and
#: `verify-basic.yml`'s smoke job — every pull request, drafts included — is
#: therefore where this gate gates.
def _hard_mode() -> bool:
    """Whether an over-budget finding fails the run.

    A fact about *where* this is running, never a guess about what the machine
    was doing. A load-average check was the obvious alternative and is the
    wrong instrument: during a nextest run the box is saturated by design, so
    load at gate time describes the gate's own moment rather than the
    conditions each duration was measured under.

    Empty counts as unset, which is the only spelling that could be used as an
    off-switch — and it cannot be one, because the switch would have to be
    flipped in `verify-basic.yml`, where the step's presence is asserted by
    `.claude/tests/test_workflows.py`.
    """
    return bool(os.environ.get("CI"))


def _mode_line(hard: bool) -> str:
    """Printed on EVERY run, green or red. A reader must never have to infer
    whether the run they are looking at could have failed."""
    if hard:
        return (
            "nextest duration: hard mode (CI is set) — a test at or above the budget that no "
            "allowlist line covers fails this run"
        )
    return (
        "nextest duration: advisory mode (CI is unset) — findings are printed and cannot fail "
        "this run: on a shared machine a fast test measures its neighbours rather than itself "
        "(6x to 170x inflation measured at load 27 on a 32-core box, for tests that take 6 ms "
        "to 190 ms quiet and 12 ms to 160 ms on CI). The gate is the Linux CI leg of "
        "verify-basic.yml, which runs on every pull request. A log this reader cannot judge "
        "still fails here — that is a wiring fault, not a slow test"
    )


def run(log: Path, budget: float, allowlist_path: Path) -> int:
    hard = _hard_mode()
    # Flushed: stdout is block-buffered when piped and stderr is not, so
    # without this the mode line lands *after* the findings it introduces.
    print(_mode_line(hard), flush=True)
    floor = int(FLOOR.read_text(encoding="utf-8").strip())
    slowest, read = durations(log.read_text(encoding="utf-8", errors="replace").splitlines())
    if read < floor:
        print(
            f"nextest duration: {read} timed result lines read, floor is {floor} "
            f"(crates/NEXTEST_FLOOR) — {log} does not describe a whole-workspace run: a subset "
            "run (`-- <filter>`), a truncated or stale log, or a nextest whose result grammar this "
            "reader no longer matches. Run `task rust:test:floor` then `task rust:test:unit` "
            "first; a gate that read nothing is not a green",
            file=sys.stderr,
        )
        return 1
    allowed = allowlist(allowlist_path)
    over = sorted(((seconds, test) for test, seconds in slowest.items() if seconds >= budget), reverse=True)
    matched = {
        entry: [test for _, test in over if fnmatchcase(test, entry)] for entry in allowed
    }
    exercised = [entry for entry, tests in matched.items() if tests]
    # Not a failure, by design: `live_powershell_*` needs pwsh on PATH, so an
    # entry that matched nothing says something about this machine and nothing
    # about the tree. Printed so a reader can delete it once it is really gone.
    for entry in sorted(entry for entry, tests in matched.items() if not tests):
        print(f"stale allowlist candidate: {entry}")
    covered = {test for tests in matched.values() for test in tests}
    failures = [(seconds, test) for seconds, test in over if test not in covered]
    if failures:
        print(
            f"nextest duration: {len(failures)} test(s) at or above the {budget:.3f}s budget and "
            f"not in {allowlist_path.name}:",
            file=sys.stderr,
        )
        for seconds, test in failures:
            print(f"nextest duration:   {seconds:.3f}s {test}", file=sys.stderr)
        print(
            "nextest duration: a unit test that slow is waiting on something real — fake it, or "
            f"add the id to {allowlist_path} with the one line saying why it cannot be fast",
            file=sys.stderr,
        )
        if hard:
            return 1
        # Printed, never suppressed: an advisory run that quietly dropped its
        # findings would be the off-switch this mode must not become.
        print(
            f"nextest duration: advisory mode — {len(failures)} finding(s) above the budget did "
            "NOT fail this run; the Linux CI leg of verify-basic.yml decides, on every pull "
            "request. Re-run with CI=1 to see the verdict it will reach",
            file=sys.stderr,
        )
        return 0
    peak, test = max((seconds, test) for test, seconds in slowest.items())
    print(
        f"nextest duration: {read} tests read (floor {floor}), budget {budget:.3f}s, "
        f"slowest {test} at {peak:.3f}s, "
        f"{len(exercised)}/{len(allowed)} allowlist entries matched, covering {len(covered)} test(s)"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("log", type=Path, help="a nextest run log (target/nextest/run.log)")
    parser.add_argument(
        "--budget-seconds",
        type=float,
        default=1.0,
        help="fail any test at or above this many seconds (default: 1.0)",
    )
    parser.add_argument("--allowlist", type=Path, default=ALLOWLIST)
    # No `--floor`: the floor is the one number this gate must not be able to
    # argue with, and a flag that lowers it is the escape hatch it exists to close.
    args = parser.parse_args()
    return run(args.log, args.budget_seconds, args.allowlist)


if __name__ == "__main__":
    raise SystemExit(main())
