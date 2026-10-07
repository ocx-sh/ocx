#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Contract-site floor for `task satellite:contract`, and the pytest plugin feeding it.

    python3 scripts/satellite_contract.py --report <junit.xml> [--floor 6]
    PYTHONPATH=scripts pytest -p satellite_contract -m ocx_contract_site --junitxml=<junit.xml>

As a plugin it records each `ocx_contract_site("<site>")` test's site id as a
JUnit property, so the report says which site a testcase covers. As a floor it
reds unless no testcase failed, errored or skipped, and the passed ones cover
at least FLOOR distinct site ids. A missing report is red, never zero.

Stdlib only; `scripts/tests/test_satellite_contract.py` shows it red and green.
"""

from __future__ import annotations

import argparse
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

MARKER = "ocx_contract_site"
FLOOR = 6
NOT_PASSED = ("failure", "error", "skipped")


# Plugin hooks, unannotated: the floor runs under a bare python3 with no pytest to import.
def pytest_configure(config) -> None:
    config.addinivalue_line("markers", f"{MARKER}(site): the ocx spawn site this test pins")


def pytest_collection_modifyitems(items) -> None:
    for item in items:
        marker = item.get_closest_marker(MARKER)
        if marker is not None and marker.args:
            item.user_properties.append((MARKER, str(marker.args[0])))


def check(report: Path, floor: int = FLOOR) -> int:
    if not report.is_file():
        print(f"satellite:contract: no JUnit report at {report} — red, not zero", file=sys.stderr)
        return 1
    try:
        cases = list(ET.parse(report).getroot().iter("testcase"))
    except ET.ParseError as error:
        print(f"satellite:contract: unreadable JUnit report {report}: {error}", file=sys.stderr)
        return 1
    not_passed = [c for c in cases if any(c.find(tag) is not None for tag in NOT_PASSED)]
    sites = {
        p.get("value")
        for c in cases
        if c not in not_passed
        for p in c.iter("property")
        if p.get("name") == MARKER
    }
    print(
        f"satellite:contract: {len(sites)} distinct {MARKER} sites passed across {len(cases)} testcases "
        f"(floor {floor}): {', '.join(sorted(sites)) or '-'}"
    )
    if not_passed:
        names = ", ".join(f"{c.get('classname')}::{c.get('name')}" for c in not_passed)
        print(f"satellite:contract: failed, errored or skipped: {names}", file=sys.stderr)
        return 1
    if len(sites) < floor:
        print(f"satellite:contract: below the floor of {floor} distinct passed {MARKER} sites", file=sys.stderr)
        return 1
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--floor", type=int, default=FLOOR)
    args = parser.parse_args()
    return check(args.report, args.floor)


if __name__ == "__main__":
    sys.exit(main())
