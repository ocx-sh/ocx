#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Select the tags `release.yml` prunes, from a package's tag list on stdin.

    scripts/release_prune_select.py dev --days N [--now ISO] < tags
    scripts/release_prune_select.py candidates --days N [--now ISO] < tags
    scripts/release_prune_select.py canary --keep N < tags
    scripts/release_prune_select.py pr --days N --open FILE [--now ISO] < tags

`dev` selects build tags `<X.Y.Z>-dev_<UTC>` whose build time is at least N days old, except the newest
build of each `<X.Y.Z>` (the one the rolling tag carries); the rolling `<X.Y.Z>-dev` tag is never selected. `candidates` selects every candidate tag of `<X.Y.Z>`
(`<X.Y.Z>-rc<N>_<UTC>` builds and rolling `<X.Y.Z>-rc<N>` tags) once the final `<X.Y.Z>_<UTC>` build is
at least N days old; before the final exists or while it is younger, nothing of that version is selected. One tag per line on
stdout, and nothing at all when none is selected.
`canary` selects every `<X.Y.Z>-canary_<UTC>` build except the N newest across all versions. `pr` selects `<X.Y.Z>-pr<N>_<UTC>` builds of a pull request that is not listed in FILE (open
pull request numbers, one per line; an empty file means none is open) and builds at least N days old of one that is, plus the rolling `<X.Y.Z>-pr<N>` tag of a closed pull request that has no build. For both, once every build of
`<X.Y.Z>-canary` or `<X.Y.Z>-pr<N>` is selected its rolling tag is selected with them, when the listing has it. A tag that fits no shape is ignored, so a registry holding other tags selects nothing from them.

--now must carry a UTC offset or is read as UTC.

Exit codes: 0 selected (possibly nothing), 64 a bad argument.
"""

from __future__ import annotations

import argparse
import re
import sys
from collections.abc import Iterable
from datetime import UTC, datetime, timedelta

STAMP = "%Y%m%d%H%M%S"
DEV_BUILD = re.compile(r"^([0-9]+\.[0-9]+\.[0-9]+)-dev_([0-9]{14})$")
RELEASE_BUILD = re.compile(r"^([0-9]+\.[0-9]+\.[0-9]+)_([0-9]{14})$")
CANARY_BUILD = re.compile(r"^([0-9]+\.[0-9]+\.[0-9]+)-canary_([0-9]{14})$")
PR_BUILD = re.compile(r"^([0-9]+\.[0-9]+\.[0-9]+)-pr([1-9][0-9]*)_([0-9]{14})$")
PR_ROLLING = re.compile(r"^([0-9]+\.[0-9]+\.[0-9]+)-pr([1-9][0-9]*)$")
CANDIDATE = re.compile(r"^([0-9]+\.[0-9]+\.[0-9]+)(-rc[1-9][0-9]*)(?:_([0-9]{14}))?$")


def _built_at(stamp: str) -> datetime | None:
    try:
        return datetime.strptime(stamp, STAMP).replace(tzinfo=UTC)
    except ValueError:
        return None


def select_dev(tags: Iterable[str], cutoff: datetime) -> list[str]:
    builds: dict[str, list[tuple[datetime, str]]] = {}
    for tag in tags:
        match = DEV_BUILD.match(tag)
        built = _built_at(match.group(2)) if match else None
        if match and built is not None:
            builds.setdefault(match.group(1), []).append((built, tag))
    chosen = []
    for line in builds.values():
        # The newest build of a line shares its digest with the rolling `X.Y.Z-dev` tag, so prune
        # refuses it (prune_shared_version) however old it is.
        line.remove(max(line))
        chosen.extend(tag for built, tag in line if built <= cutoff)
    return sorted(chosen)


def select_candidates(tags: Iterable[str], cutoff: datetime) -> list[str]:
    tags = list(tags)
    final: set[str] = set()
    for tag in tags:
        match = RELEASE_BUILD.match(tag)
        built = _built_at(match.group(2)) if match else None
        if match and built is not None and built <= cutoff:
            final.add(match.group(1))
    # The final is the newest build of its line, so every candidate of that version goes together;
    # selecting the rolling tag with its builds avoids prune's shared-version refusal.
    chosen = []
    for tag in tags:
        match = CANDIDATE.match(tag)
        if match and match.group(1) in final:
            chosen.append(tag)
    return sorted(chosen)


def _with_rolling(
    tags: list[str], families: dict[str, list[str]], chosen: set[str]
) -> list[str]:
    # A rolling tag shares its digest with the family's newest build; prune refuses that build
    # unless the rolling tag is in the same delete set.
    for rolling, builds in families.items():
        if rolling in tags and all(build in chosen for build in builds):
            chosen.add(rolling)
    return sorted(chosen)


def select_canary(tags: Iterable[str], keep: int) -> list[str]:
    tags = list(tags)
    builds: list[tuple[datetime, str]] = []
    families: dict[str, list[str]] = {}
    for tag in tags:
        match = CANARY_BUILD.match(tag)
        built = _built_at(match.group(2)) if match else None
        if match and built is not None:
            builds.append((built, tag))
            families.setdefault(f"{match.group(1)}-canary", []).append(tag)
    chosen = {tag for _, tag in sorted(builds, reverse=True)[keep:]}
    return _with_rolling(tags, families, chosen)


def select_pr(tags: Iterable[str], cutoff: datetime, open_prs: set[int]) -> list[str]:
    tags = list(tags)
    families: dict[str, list[str]] = {}
    chosen: set[str] = set()
    for tag in tags:
        match = PR_BUILD.match(tag)
        built = _built_at(match.group(3)) if match else None
        if match and built is not None:
            families.setdefault(f"{match.group(1)}-pr{match.group(2)}", []).append(tag)
            if int(match.group(2)) not in open_prs or built <= cutoff:
                chosen.add(tag)
    for tag in tags:
        match = PR_ROLLING.match(tag)
        if match and int(match.group(2)) not in open_prs and tag not in families:
            chosen.add(tag)
    return _with_rolling(tags, families, chosen)


def _read_open(path: str) -> set[int] | None:
    try:
        with open(path, encoding="utf-8") as handle:
            lines = [line.strip() for line in handle if line.strip()]
    except (OSError, UnicodeDecodeError):
        return None
    if not all(re.fullmatch(r"[1-9][0-9]*", line) for line in lines):
        return None
    return {int(line) for line in lines}


def _usage(message: str) -> int:
    print(f"release_prune_select: {message}", file=sys.stderr)
    return 64


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("kind", choices=("dev", "candidates", "canary", "pr"))
    parser.add_argument("--days", type=int, help="age in days (dev, candidates, pr)")
    parser.add_argument("--keep", type=int, help="builds to keep (canary)")
    parser.add_argument("--open", help="file of open pull request numbers (pr)")
    parser.add_argument(
        "--now",
        help="time to measure from, ISO 8601; an offset-less value is read as UTC; defaults to the clock",
    )
    args = parser.parse_args(argv)
    if args.kind != "canary":
        if args.days is None:
            parser.error("--days is required")
        if args.days < 0:
            return _usage("--days must be >= 0")
    open_prs: set[int] = set()
    if args.kind == "canary" and (args.keep is None or args.keep < 1):
        return _usage("--keep is required for canary and must be >= 1")
    if args.kind == "pr":
        if args.open is None:
            return _usage("--open is required for pr")
        read = _read_open(args.open)
        if read is None:
            return _usage(f"--open '{args.open}' is not a readable list of pull request numbers")
        open_prs = read
    now = datetime.now(UTC)
    if args.now:
        try:
            parsed = datetime.fromisoformat(args.now)
        except ValueError:
            print(
                f"release_prune_select: --now '{args.now}' is not an ISO 8601 time",
                file=sys.stderr,
            )
            return 64
        now = (
            parsed.replace(tzinfo=UTC)
            if parsed.tzinfo is None
            else parsed.astimezone(UTC)
        )
    cutoff = now - timedelta(days=args.days or 0)
    tags = [line.strip() for line in sys.stdin if line.strip()]
    if args.kind == "dev":
        chosen = select_dev(tags, cutoff)
    elif args.kind == "candidates":
        chosen = select_candidates(tags, cutoff)
    elif args.kind == "canary":
        chosen = select_canary(tags, args.keep)
    else:
        chosen = select_pr(tags, cutoff, open_prs)
    sys.stdout.write("".join(f"{tag}\n" for tag in chosen))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
