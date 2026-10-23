#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Skip/xfail ceilings over a pytest run's terminal summary line.

    task test:.suite-ceilings
    python3 scripts/suite_ceilings.py --log <run.log> \
        --skip-ceiling-file test/SKIP_CEILING --xfail-ceiling-file test/XFAIL_CEILING

Fails when the last summary line of the log reports more `skipped` than the
skip ceiling file or more `xfailed` than the xfail ceiling file — a skip added
to dodge a red is caught here, not by the collected count. `test:lint:structure`
passes its own pair, test/LINT_SKIP_CEILING and test/LINT_XFAIL_CEILING.

The log is what the terminal saw, escape codes included when a caller exported
FORCE_COLOR, so they are stripped before the summary is looked for; and the `=`
bars are absent under `-q`, so the line is recognised by its counts alone — the
last such line is the summary. The count may open the line (`256 skipped in
1.00s`, the `-q` shape when every test skipped), so nothing is required before
it but a non-digit or the start of the line.

Stdlib only; `scripts/tests/test_suite_ceilings.py` shows it red and green.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
ANSI = re.compile(r"\x1b\[[0-9;]*m")
SUMMARY = re.compile(r"(passed|failed|skipped|xfailed|error).* in [0-9.]+s")


def summary_line(text: str) -> str | None:
    """The last line of `text` that reads as a pytest summary, escape codes stripped."""
    lines = [line for line in ANSI.sub("", text).splitlines() if SUMMARY.search(line)]
    return lines[-1] if lines else None


def count(summary: str, kind: str) -> int:
    """The last `<N> <kind>` in the summary; absent is 0."""
    found = re.findall(rf"(?<![0-9])([0-9]+) {kind}", summary)
    return int(found[-1]) if found else 0


def _shown(path: Path) -> str:
    return os.path.relpath(path.resolve(), REPO_ROOT)


def run(log: Path, skip_ceiling_file: Path, xfail_ceiling_file: Path) -> int:
    # A missing log is the same verdict as a log with no summary: nothing ran.
    text = log.read_text(encoding="utf-8", errors="replace") if log.is_file() else ""
    summary = summary_line(text)
    if summary is None:
        print(f"suite ceilings: no pytest summary line in {log}", file=sys.stderr)
        return 1
    skipped, xfailed = count(summary, "skipped"), count(summary, "xfailed")
    skip_ceiling = int(skip_ceiling_file.read_text(encoding="utf-8").strip())
    xfail_ceiling = int(xfail_ceiling_file.read_text(encoding="utf-8").strip())
    if skipped > skip_ceiling:
        print(
            f"suite ceilings: {skipped} skipped exceeds {_shown(skip_ceiling_file)} ({skip_ceiling}): {summary}",
            file=sys.stderr,
        )
        return 1
    if xfailed > xfail_ceiling:
        print(
            f"suite ceilings: {xfailed} xfailed exceeds {_shown(xfail_ceiling_file)} ({xfail_ceiling}): {summary}",
            file=sys.stderr,
        )
        return 1
    print(
        f"suite ceilings: {skipped} skipped (ceiling {skip_ceiling}), {xfailed} xfailed (ceiling {xfail_ceiling})"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--log", type=Path, required=True, help="the tee'd terminal output of a pytest run")
    parser.add_argument("--skip-ceiling-file", type=Path, default=REPO_ROOT / "test" / "SKIP_CEILING")
    parser.add_argument("--xfail-ceiling-file", type=Path, default=REPO_ROOT / "test" / "XFAIL_CEILING")
    args = parser.parse_args()
    return run(args.log, args.skip_ceiling_file, args.xfail_ceiling_file)


if __name__ == "__main__":
    raise SystemExit(main())
