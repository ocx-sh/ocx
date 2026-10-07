#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Run, under cargo nextest, every testcase a `rust_test` target `--skip`s.

    task rust:test:bazel-skipped
    python3 scripts/bazel_skipped_tests.py [--print]

`bazel:test:unit` is the only unit-test run in `task verify`, and its
`rust_test` targets `--skip` testcases that walk the `crates/*/src` of crates
they do not depend on, or need a process of their own. Those cases would run
nowhere on Linux. The skip list is read from the `--skip=` args of every
`crates/*/BUILD.bazel` `rust_test` — never copied — and mapped to its cargo
package and test binary: `crate = ":x"` is the package's lib, `srcs =
["tests/x.rs"]` the integration test `x`.

Reds when no skip parses, when a skip names no listed testcase, or when nextest
ran fewer testcases than skips parsed. Stdlib only;
`scripts/tests/test_bazel_skipped_tests.py` shows it red and green.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

from nextest_ceiling import ANSI

REPO_ROOT = Path(__file__).resolve().parents[1]
SUMMARY = re.compile(r"Summary \[ *[0-9.]+s\] (?P<run>[0-9]+)(?:/[0-9]+)? tests? run:")
RUST_TEST = re.compile(r"^rust_test\(\n(?P<body>.*?)^\)$", re.MULTILINE | re.DOTALL)
SKIP = re.compile(r'^\s*"--skip=(?P<name>[^"]+)",', re.MULTILINE)
LIB_CRATE = re.compile(r'^\s*crate = ":[^"]+",', re.MULTILINE)
TEST_SRC = re.compile(r'"tests/(?P<bin>[^"/]+)\.rs"')


@dataclass(frozen=True)
class Skip:
    package: str
    binary: str | None  # None = the package's lib
    name: str

    def filterset(self) -> str:
        scope = f"binary(={self.binary})" if self.binary else "kind(lib)"
        return f"(package(={self.package}) & {scope} & test(={self.name}))"


def parse(root: Path) -> list[Skip]:
    """Every skip of every `rust_test` under `root/crates`, deduplicated."""
    skips: dict[Skip, None] = {}
    for build in sorted((root / "crates").glob("*/BUILD.bazel")):
        manifest = tomllib.loads((build.parent / "Cargo.toml").read_text(encoding="utf-8"))
        package = manifest["package"]["name"]
        text = "\n".join(line for line in build.read_text(encoding="utf-8").splitlines() if not line.lstrip().startswith("#"))
        for target in RUST_TEST.finditer(text):
            body = target["body"]
            names = SKIP.findall(body)
            if not names:
                continue
            integration = TEST_SRC.search(body)
            if integration is None and LIB_CRATE.search(body) is None:
                raise SystemExit(f"{build}: a rust_test with --skip names neither `crate = \":x\"` nor `srcs = [\"tests/x.rs\"]`")
            for name in names:
                skips[Skip(package, integration["bin"] if integration else None, name)] = None
    return list(skips)


def nextest(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["cargo", "nextest", *args], cwd=REPO_ROOT, text=True, capture_output=True, check=False)


def run(skips: list[Skip]) -> int:
    if not skips:
        print("bazel skipped tests: parsed 0 --skip args from crates/*/BUILD.bazel — the reader found nothing", file=sys.stderr)
        return 1
    packages = sorted({s.package for s in skips})
    select = [arg for p in packages for arg in ("-p", p)] + ["--lib"]
    select += [arg for b in sorted({s.binary for s in skips if s.binary}) for arg in ("--test", b)]
    common = ["--locked", *select, "-E", " | ".join(s.filterset() for s in skips)]

    listing = nextest("list", *common, "--message-format", "json")
    if listing.returncode != 0:
        sys.stderr.write(listing.stderr)
        return listing.returncode
    listed = {
        (suite["package-name"], None if suite["kind"] == "lib" else suite["binary-name"], case)
        for suite in json.loads(listing.stdout)["rust-suites"].values()
        for case in suite["testcases"]
    }
    missing = [s for s in skips if (s.package, s.binary, s.name) not in listed]
    if missing:
        for s in missing:
            print(f"bazel skipped tests: --skip={s.name} ({s.package}) names no testcase", file=sys.stderr)
        return 1

    # Streamed, not captured: the run is minutes long and its failures are the product.
    ran = subprocess.Popen(
        ["cargo", "nextest", "run", "--no-tests=fail", "--no-fail-fast", *common],
        cwd=REPO_ROOT, text=True, stderr=subprocess.PIPE, stdout=subprocess.PIPE,
    )
    count = 0
    assert ran.stderr is not None
    for line in ran.stderr:
        sys.stderr.write(line)
        found = SUMMARY.search(ANSI.sub("", line))
        count = int(found["run"]) if found else count
    ran.wait()
    if ran.returncode != 0:
        return ran.returncode
    if count < len(skips):
        print(f"bazel skipped tests: nextest ran {count} testcases for {len(skips)} parsed skips", file=sys.stderr)
        return 1
    print(f"bazel skipped tests: {count} testcases ran for {len(skips)} parsed skips, all passed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--print", action="store_true", help="print the parsed skips and exit")
    args = parser.parse_args()
    skips = parse(REPO_ROOT)
    if args.print:
        for s in skips:
            print(f"{s.package}\t{s.binary or 'lib'}\t{s.name}")
        return 0 if skips else 1
    return run(skips)


if __name__ == "__main__":
    raise SystemExit(main())
