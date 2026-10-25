#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The job matrix of `.github/workflows/cross.yml`, as JSON for `fromJSON`.

    python3 scripts/cross_matrix.py --all "<every supported triple>" [--only "<triple> ..."]

`--all` is `task cross:targets`; `--only` narrows the run to some of them (the basic tier builds two), and a
triple outside `--all` is refused, so a typo cannot shrink a run to nothing and still read green. Each leg
carries the flags `cross.yml` steps read: `bundle` (a unit-test bundle for a native runner) and `test-bin` (the
acceptance binaries for one). Exit 1 is a refused request.
"""

from __future__ import annotations

import argparse
import json
import sys

# The legs a native runner (macOS arm64, Windows x64) downloads from; Linux x64 gets the acceptance binaries
# for the Linux acceptance run's parity with them.
BUNDLE = frozenset({"aarch64-apple-darwin", "x86_64-pc-windows-gnullvm"})
TEST_BIN = BUNDLE | {"x86_64-unknown-linux-gnu"}


def matrix(every: list[str], only: list[str]) -> dict[str, list[dict[str, object]]]:
    stray = sorted(set(only) - set(every))
    if stray:
        raise ValueError(f"unsupported target(s) {stray}; see: task cross:targets")
    missing = sorted((BUNDLE | TEST_BIN) - set(every))
    if missing:
        raise ValueError(f"{missing} carry a bundle or test-bin flag but are not supported targets")
    chosen = [t for t in every if t in only] if only else every
    return {
        "include": [
            {"target": t, **({"bundle": True} if t in BUNDLE else {}), **({"test-bin": True} if t in TEST_BIN else {})}
            for t in chosen
        ]
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--all", required=True, help="every supported triple, whitespace separated")
    parser.add_argument("--only", default="", help="the triples to build, whitespace separated; empty means all")
    args = parser.parse_args(argv)
    try:
        print(json.dumps(matrix(args.all.split(), args.only.split()), separators=(",", ":")))
    except ValueError as err:
        print(f"cross_matrix: {err}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
