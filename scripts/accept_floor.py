#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Floor and ceilings over a directory of acceptance JUnit reports.

    python3 scripts/accept_floor.py --junit target/bazel/accept     # bazel:test:accept (unsharded)
    python3 scripts/accept_floor.py --junit target/accept/junit     # test:accept:floor, merged shards

Reads every `*.xml` under `--junit` recursively, sums testcases, skips and
xfails, and fails on any failure or error, on fewer cases than
`test/SUITE_FLOOR`, and on more skips / xfails than `test/SKIP_CEILING` /
`test/XFAIL_CEILING`. A directory with no report is red: a reader that read
nothing is not a clean suite.
The conformance root floor (`conformance_floor.py --junit`) is a separate call
the tasks make over the same directory.

A shard holds a subset, so the floor is evaluated over the merged directory,
never per shard. `scripts/tests/test_accept_floor.py` shows it red and green.
"""

from __future__ import annotations

import argparse
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]


def run(junit: Path, limits_dir: Path, expect_shards: int = 0) -> int:
    reports = sorted(junit.rglob("*.xml"))
    if not reports:
        print(f"accept floor: no *.xml report under {junit} — nothing was read", file=sys.stderr)
        return 1

    # Extra reports beyond the expected names are fine (the conformance file, `all.xml`).
    names = {r.name for r in reports}
    missing = [f"shard-{i}-of-{expect_shards}.xml" for i in range(1, expect_shards + 1)]
    missing = [m for m in missing if m not in names]
    if missing:
        print(f"accept floor: missing shard report(s) under {junit}: {', '.join(missing)}", file=sys.stderr)
        return 1

    def limit(name: str) -> int:
        return int((limits_dir / name).read_text(encoding="utf-8").strip())

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

    problems = []
    if bad:
        problems.append(f"{bad} failed or errored case(s)")
    if cases < limit("SUITE_FLOOR"):
        problems.append(f"{cases} cases ran, test/SUITE_FLOOR is {limit('SUITE_FLOOR')} — the suite shrank")
    if skipped > limit("SKIP_CEILING"):
        problems.append(f"{skipped} skipped exceeds test/SKIP_CEILING ({limit('SKIP_CEILING')})")
    if xfailed > limit("XFAIL_CEILING"):
        problems.append(f"{xfailed} xfailed exceeds test/XFAIL_CEILING ({limit('XFAIL_CEILING')})")
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
        "--expect-shards", type=int, default=0, metavar="N", help="require shard-1-of-N.xml .. shard-N-of-N.xml"
    )
    args = parser.parse_args(argv)
    return run(args.junit, args.limits_dir, args.expect_shards)


if __name__ == "__main__":
    raise SystemExit(main())
