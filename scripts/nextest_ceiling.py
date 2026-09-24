#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Skipped-test ceiling over a nextest run's Summary line.

    task rust:test:ceiling
    python3 scripts/nextest_ceiling.py --log target/nextest/run.log

Reads the Summary line of the run `rust:test:unit` tee'd to
target/nextest/run.log and fails when it reports more skipped tests than
crates/NEXTEST_SKIP_CEILING. `rust:test:floor` deletes that log before the run,
so a Summary line read here can only be this bracket's.

The skipped count is read only from a line `GRAMMAR` matches end to end — an
absent token is 0 there and nowhere else. The grammar is nextest 0.9.144's
`displayer/progress.rs::write_summary_str` (nextest-runner): every token but
`passed` is conditional. That version always prints `, N skipped`; the optional
group is for the day it stops. The tool is unpinned, so a Summary reshape reds
this gate loudly, by design — as does anything else, a cancelled `--stress` run
included.

Stdlib only; `scripts/tests/test_nextest_ceiling.py` shows it red and green.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
CEILING = REPO_ROOT / "crates" / "NEXTEST_SKIP_CEILING"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
GRAMMAR = re.compile(
    r"^ *Summary \[ *[0-9]+\.[0-9]+s\] [0-9]+(/[0-9]+)? tests? run:"
    r" [0-9]+ passed( \([0-9]+ (slow|flaky|leaky)(, [0-9]+ (slow|flaky|leaky))*\))?"
    r"(, [0-9]+ failed( \([0-9]+ due to being leaky\))?)?(, [0-9]+ exec failed)?"
    r"(, [0-9]+ timed out)?(, (?P<skipped>[0-9]+) skipped)?$"
)


def run(log: Path) -> int:
    # A missing log is the same verdict as a log with no Summary: nothing ran.
    text = log.read_text(encoding="utf-8", errors="replace") if log.is_file() else ""
    summaries = [line for line in ANSI.sub("", text).splitlines() if "Summary [" in line]
    if not summaries:
        print(
            f"nextest ceiling: no Summary line in {log} — rust:test:unit did not run in this bracket; "
            "run task rust:test:floor then task rust:test:unit first",
            file=sys.stderr,
        )
        return 1
    summary = summaries[-1]
    match = GRAMMAR.search(summary)
    if match is None:
        print(f"nextest ceiling: Summary line is not nextest's grammar (a nextest bump?): {summary}", file=sys.stderr)
        return 1
    skipped = int(match["skipped"] or 0)
    ceiling = int(CEILING.read_text(encoding="utf-8").strip())
    if skipped > ceiling:
        print(
            f"nextest ceiling: {skipped} skipped exceeds crates/NEXTEST_SKIP_CEILING ({ceiling}): {summary}",
            file=sys.stderr,
        )
        return 1
    print(f"nextest ceiling: {skipped} skipped (ceiling {ceiling})")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--log", type=Path, required=True, help="a nextest run log (target/nextest/run.log)")
    args = parser.parse_args()
    return run(args.log)


if __name__ == "__main__":
    raise SystemExit(main())
