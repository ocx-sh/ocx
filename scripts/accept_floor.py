#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Floor and ceilings over a directory of acceptance JUnit reports.

    python3 scripts/accept_floor.py --junit target/bazel/accept     # bazel:test:accept (unsharded)
    python3 scripts/accept_floor.py --junit target/accept/junit     # test:accept:floor, merged shards

Sums testcases, skips and xfails over every `*.xml` under `--junit`; fails on a failure or error, on
fewer cases than `SUITE_FLOOR`, on more skips / xfails than `SKIP_CEILING` /
`XFAIL_CEILING` (read from `--limits-dir`, default `test/`), and on either ceiling more than `CEILING_SLACK` above its measured count. No
report is red: reading nothing is not a clean suite. A shard holds a subset, so the floor is judged
over the merged directory. `conformance_floor.py --junit` is a separate call.
"""

from __future__ import annotations

import argparse
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
LIMIT_FILES = ("SUITE_FLOOR", "SKIP_CEILING", "XFAIL_CEILING")
# How far a skip / xfail ceiling may sit above the measured count: room for one flaky skip, not for a
# guessed bump that would let the next regressions in unseen.
CEILING_SLACK = 2


def run(junit: Path, limits_dir: Path, expect_shards: int = 0) -> int:
    reports = sorted(junit.rglob("*.xml"))
    if not reports:
        print(f"accept floor: no *.xml report under {junit} — nothing was read", file=sys.stderr)
        return 1

    # Extra reports beyond the expected names are fine (the conformance file, `all.xml`).
    names = {r.name for r in reports}
    missing = [f"shard-{i}-of-{expect_shards}.xml" for i in range(expect_shards)]
    missing = [m for m in missing if m not in names]
    if missing:
        print(f"accept floor: missing shard report(s) under {junit}: {', '.join(missing)}", file=sys.stderr)
        return 1

    def limit(name: str) -> int:
        return int((limits_dir / name).read_text(encoding="utf-8").strip())

    def where(name: str) -> str:
        # Name the file actually read: a profile under `test/limits/` carries its own ceilings.
        path = limits_dir / name
        try:
            return path.relative_to(REPO_ROOT).as_posix()
        except ValueError:
            return f"{limits_dir.name}/{name}"

    cases = skipped = xfailed = bad = 0
    for report in reports:
        for case in ET.parse(report).getroot().iter("testcase"):
            cases += 1
            if case.find("failure") is not None or case.find("error") is not None:
                bad += 1
            for mark in case.findall("skipped"):
                if mark.get("type") == "pytest.xfail":
                    xfailed += 1
                else:
                    skipped += 1

    # A missing limit file is red too, but the counts print first: a platform's first run has no
    # limit files and commits them from these numbers.
    absent = [limits_dir / name for name in LIMIT_FILES if not (limits_dir / name).is_file()]
    if absent:
        print(
            f"accept floor: no limit file {', '.join(str(a) for a in absent)} — counted {cases} cases, "
            f"{skipped} skipped, {xfailed} xfailed, {bad} failed or errored; commit the limits from these",
            file=sys.stderr,
        )
        return 1

    problems = []
    if bad:
        problems.append(f"{bad} failed or errored case(s)")
    if cases < limit("SUITE_FLOOR"):
        problems.append(f"{cases} cases ran, {where('SUITE_FLOOR')} is {limit('SUITE_FLOOR')} — the suite shrank")
    if skipped > limit("SKIP_CEILING"):
        problems.append(f"{skipped} skipped exceeds {where('SKIP_CEILING')} ({limit('SKIP_CEILING')})")
    if xfailed > limit("XFAIL_CEILING"):
        problems.append(f"{xfailed} xfailed exceeds {where('XFAIL_CEILING')} ({limit('XFAIL_CEILING')})")
    for name, measured in (("SKIP_CEILING", skipped), ("XFAIL_CEILING", xfailed)):
        if limit(name) > measured + CEILING_SLACK:
            problems.append(
                f"{where(name)} ({limit(name)}) sits above the {measured} measured by more than {CEILING_SLACK} "
                f"— lower it to {measured}"
            )
    if problems:
        print("accept floor: " + "; ".join(problems), file=sys.stderr)
        return 1
    print(
        f"accept floor: {len(reports)} report(s); {cases} cases (floor {limit('SUITE_FLOOR')}), "
        f"{skipped} skipped (ceiling {limit('SKIP_CEILING')}), {xfailed} xfailed (ceiling {limit('XFAIL_CEILING')})"
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--junit", type=Path, required=True)
    parser.add_argument("--limits-dir", type=Path, default=REPO_ROOT / "test")
    parser.add_argument(
        "--expect-shards", type=int, default=0, metavar="N", help="require shard-0-of-N.xml .. shard-<N-1>-of-N.xml"
    )
    args = parser.parse_args(argv)
    return run(args.junit, args.limits_dir, args.expect_shards)


if __name__ == "__main__":
    raise SystemExit(main())
