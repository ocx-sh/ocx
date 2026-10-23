#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The one selector that splits the acceptance suite into shards.

    python3 scripts/accept_shard.py --shard 3/40 --format labels   # bazel:test:accept SHARD=3/40
    python3 scripts/accept_shard.py --shard 3/40 --format paths    # test:accept:native SHARD=3/40

Modules are `test/tests/test_*.py`, sorted by stem; shard `i/n` (1-based) takes
`modules[i-1::n]`. Both execution paths (the Bazel targets and the native pytest
run) call this, so a module lands in the same shard on either. Without `--shard`
every module is printed. An empty selection is an error: a shard past the module
count would otherwise run nothing and report green.

Stdlib only; `scripts/tests/test_accept_shard.py` shows it red and green.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]


def modules(root: Path = REPO_ROOT) -> list[str]:
    """Sorted stems of every acceptance module under `root/test/tests`."""
    return sorted(p.stem for p in (root / "test" / "tests").glob("test_*.py"))


def parse_shard(spec: str) -> tuple[int, int]:
    try:
        index, count = (int(part) for part in spec.split("/"))
    except ValueError:
        raise ValueError(f"shard {spec!r} is not <i>/<n>") from None
    if not 1 <= index <= count:
        raise ValueError(f"shard {spec!r}: need 1 <= i <= n")
    return index, count


def select(stems: list[str], shard: str | None) -> list[str]:
    if shard is None:
        return list(stems)
    index, count = parse_shard(shard)
    return stems[index - 1 :: count]


FORMATS = {
    "stems": lambda stem: stem,
    "paths": lambda stem: f"tests/{stem}.py",
    "labels": lambda stem: f"//test:{stem}",
}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--shard", help="i/n, 1-based; omitted = every module")
    parser.add_argument("--format", choices=sorted(FORMATS), default="stems")
    parser.add_argument("--root", type=Path, default=REPO_ROOT)
    args = parser.parse_args(argv)
    stems = modules(args.root)
    if not stems:
        print(f"accept shard: no acceptance module under {args.root / 'test' / 'tests'}", file=sys.stderr)
        return 1
    try:
        chosen = select(stems, args.shard)
    except ValueError as err:
        print(f"accept shard: {err}", file=sys.stderr)
        return 1
    if not chosen:
        print(f"accept shard: shard {args.shard} selects none of the {len(stems)} modules", file=sys.stderr)
        return 1
    print("\n".join(FORMATS[args.format](stem) for stem in chosen))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
