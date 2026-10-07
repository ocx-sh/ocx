#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Distinct-root floor over the conformance hook's JUnit properties.

    python3 scripts/conformance_floor.py --junit test/results/junit.xml     # task test
    python3 scripts/conformance_floor.py --junit target/bazel/accept        # bazel:test:accept

`test/src/conformance.py` records, on each acceptance case, a
`conformance_root` property per schema root a `--format json` stdout validated
against and a `conformance_finding` property per document that did not. This
reads every `*.xml` under `--junit` (one file or a directory of per-module
reports), prints the findings grouped by root, and fails when the distinct
roots validated fall below `test/CONFORMANCE_FLOOR`. A finding fails here too,
so a test that swallowed the hook's own failure cannot hide it.

A run with fewer cases than `test/SUITE_FLOOR` is a subset (`-k`, one module,
the scoped tier), whose root count says nothing about the suite, so the floor
is reported as not checked. The Bazel lane refuses a subset before this runs.

Stdlib only; `scripts/tests/test_conformance_floor.py` shows it red and green.
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import json
import os
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
ROOT_PROPERTY = "conformance_root"
FINDING_PROPERTY = "conformance_finding"


@dataclasses.dataclass
class Census:
    cases: int
    roots: set[str]
    findings: list[dict[str, object]]


def read(junit: Path) -> Census:
    reports = sorted(junit.rglob("*.xml")) if junit.is_dir() else [junit] if junit.is_file() else []
    census = Census(0, set(), [])
    for report in reports:
        for case in ET.parse(report).getroot().iter("testcase"):
            census.cases += 1
            for prop in case.iter("property"):
                value = prop.get("value", "")
                if prop.get("name") == ROOT_PROPERTY:
                    census.roots.add(value)
                elif prop.get("name") == FINDING_PROPERTY:
                    census.findings.append(json.loads(value))
    return census


def _shown(path: Path) -> str:
    return os.path.relpath(path.resolve(), REPO_ROOT)


def _report_findings(findings: list[dict[str, object]]) -> None:
    grouped: dict[str, collections.Counter[str]] = collections.defaultdict(collections.Counter)
    for finding in findings:
        roots = finding.get("roots") or []
        key = "|".join(map(str, roots)) if roots else f"({finding.get('command')})"
        grouped[key][str(finding.get("rule"))] += 1
    print(f"conformance: {len(findings)} findings", file=sys.stderr)
    for key in sorted(grouped):
        for rule, count in grouped[key].most_common():
            print(f"  {key}: {count} × {rule}", file=sys.stderr)


def run(junit: Path, floor_file: Path, suite_floor_file: Path) -> int:
    census = read(junit)
    if census.cases == 0:
        print(f"conformance floor: no JUnit testcase under {junit} — nothing was read", file=sys.stderr)
        return 1
    if census.findings:
        # The hook fails the capturing test itself; this catches a test that swallowed that failure.
        _report_findings(census.findings)
    verdict = 1 if census.findings else 0
    suite_floor = int(suite_floor_file.read_text(encoding="utf-8").strip())
    if census.cases < suite_floor:
        print(
            f"conformance floor: partial run: {census.cases} cases, {_shown(suite_floor_file)} is "
            f"{suite_floor} — floor not checked ({len(census.roots)} distinct roots validated)"
        )
        return verdict
    floor = int(floor_file.read_text(encoding="utf-8").strip())
    if len(census.roots) < floor:
        failing = {str(r) for f in census.findings for r in f.get("roots") or []} - census.roots
        print(
            f"conformance floor: {len(census.roots)} distinct roots validated, {_shown(floor_file)} is "
            f"{floor} — a root stopped validating: {sorted(census.roots)}; failing only: {sorted(failing)}",
            file=sys.stderr,
        )
        return 1
    print(f"conformance floor: {len(census.roots)} distinct roots validated (floor {floor})")
    return verdict


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--junit", type=Path, required=True, help="a JUnit report, or a directory of them")
    parser.add_argument("--floor-file", type=Path, default=REPO_ROOT / "test" / "CONFORMANCE_FLOOR")
    parser.add_argument("--suite-floor-file", type=Path, default=REPO_ROOT / "test" / "SUITE_FLOOR")
    args = parser.parse_args()
    return run(args.junit, args.floor_file, args.suite_floor_file)


if __name__ == "__main__":
    raise SystemExit(main())
