#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The one selector that splits the acceptance suite into shards.

    python3 scripts/accept_shard.py --shard-count 40 --shard-index 3 --format labels   # bazel:test:accept
    python3 scripts/accept_shard.py --shard-count 40 --shard-index 3 --format paths    # test:accept:native

Modules are `test/tests/test_*.py`, sorted by stem; shard `I` of `N` (0-based) takes
`modules[I::N]`. Both execution paths (the Bazel targets and the native pytest
run) call this, so a module lands in the same shard on either. Without a shard
every module is printed. Flags win; with none, Bazel's test-sharding variables
`TEST_TOTAL_SHARDS` and `TEST_SHARD_INDEX` are read, and `TEST_SHARD_STATUS_FILE`
is touched when set (the protocol's "this runner shards" signal).

Exit 2 is a bad request: a count below 1, an index outside `0..N-1`, a non-integer,
or one half of a pair. Exit 1 is an empty selection: a shard past the module count
would otherwise run nothing and report green.

Stdlib only; `scripts/tests/test_accept_shard.py` shows it red and green.
"""

from __future__ import annotations

import argparse
import os
import sys
from collections.abc import Mapping
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

# Bazel's test-sharding names (bazel.build/reference/test-encyclopedia).
ENV_COUNT, ENV_INDEX, ENV_STATUS_FILE = "TEST_TOTAL_SHARDS", "TEST_SHARD_INDEX", "TEST_SHARD_STATUS_FILE"


class BadShard(ValueError):
    """A shard request that cannot be honoured; the caller exits 2."""


def modules(root: Path = REPO_ROOT) -> list[str]:
    """Sorted stems of every acceptance module under `root/test/tests`."""
    return sorted(p.stem for p in (root / "test" / "tests").glob("test_*.py"))


def resolve(count: str | None, index: str | None, env: Mapping[str, str]) -> tuple[int, int] | None:
    """`(count, index)`, or `None` when no slice was asked for.

    The flags are one source and the env pair another, never mixed: a flag given
    alone is not completed from the environment. An empty value counts as unset.
    """
    names = ("--shard-count", "--shard-index")
    if count is None and index is None:
        count, index = env.get(ENV_COUNT) or None, env.get(ENV_INDEX) or None
        names = (ENV_COUNT, ENV_INDEX)
    if count is None and index is None:
        return None
    if count is None or index is None:
        raise BadShard(f"{names[0]} and {names[1]} are given together or not at all")
    try:
        n, i = int(count), int(index)
    except ValueError:
        raise BadShard(f"{names[0]}={count!r} and {names[1]}={index!r} must be integers") from None
    if n < 1:
        raise BadShard(f"{names[0]}={n}: need at least 1 shard")
    if not 0 <= i < n:
        raise BadShard(f"{names[1]}={i}: need 0 <= index < {n}")
    return n, i


def select(stems: list[str], count: int, index: int) -> list[str]:
    return stems[index::count]


FORMATS = {
    "stems": lambda stem: stem,
    "paths": lambda stem: f"tests/{stem}.py",
    "labels": lambda stem: f"//test:{stem}",
}


def main(argv: list[str] | None = None, env: Mapping[str, str] | None = None) -> int:
    env = os.environ if env is None else env
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--shard-count", help="N, the number of shards; with --shard-index")
    parser.add_argument("--shard-index", help="I in 0..N-1; omitted = every module")
    parser.add_argument("--format", choices=sorted(FORMATS), default="stems")
    parser.add_argument("--root", type=Path, default=REPO_ROOT)
    args = parser.parse_args(argv)

    status_file = env.get(ENV_STATUS_FILE)
    if status_file:
        try:
            Path(status_file).touch()
        except OSError as err:
            print(f"accept shard: cannot touch {ENV_STATUS_FILE}: {err}", file=sys.stderr)
            return 1
    try:
        shard = resolve(args.shard_count, args.shard_index, env)
    except BadShard as err:
        print(f"accept shard: {err}", file=sys.stderr)
        return 2

    stems = modules(args.root)
    if not stems:
        print(f"accept shard: no acceptance module under {args.root / 'test' / 'tests'}", file=sys.stderr)
        return 1
    chosen = stems if shard is None else select(stems, *shard)
    if not chosen:
        print(f"accept shard: shard {shard[1]} of {shard[0]} selects none of the {len(stems)} modules", file=sys.stderr)
        return 1
    print("\n".join(FORMATS[args.format](stem) for stem in chosen))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
