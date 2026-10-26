#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Run the `workspace_structure` testcases that shell out to `cargo`, from the Bazel-built test binary.

    task rust:test:workspace-cargo
    python3 scripts/workspace_cargo_cases.py [--cache-rc ARG]

`NEEDS_CARGO` (`scripts/test_bundle.py`) read `cargo metadata` and `cargo tree` over the whole workspace,
which a Bazel test cannot run: no cargo, no network, no submodules in its sandbox. They are the only
`--skip`ped testcases that run outside Bazel. The binary is the one `bazel:test:unit` already built, run
from the checkout, so nothing compiles here; only cargo's own metadata fetch does.

Reds when the binary is not found, when a case fails, and when the run passed any number of testcases
other than the case count: a renamed case runs zero and libtest still exits 0. Stdlib only.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

from test_bundle import NEEDS_CARGO, ROOT, bazel

LABEL = "//crates/ocx_test_support:workspace_structure"
PASSED = re.compile(r"^test result: ok\. (?P<passed>\d+) passed; 0 failed;", re.MULTILINE)


def binary(cache_rc: str) -> Path:
    bazel("build", LABEL, rc=cache_rc)
    found = bazel("cquery", LABEL, "--output=files", rc=cache_rc, capture=True).split()
    if not found:
        sys.exit(f"{LABEL} produced no file")
    return ROOT / found[-1]


def verdict(returncode: int, stdout: str, cases: int) -> str | None:
    """Why the run is red, or None."""
    passed = PASSED.search(stdout)
    if returncode != 0:
        return f"the test binary exited {returncode}"
    if passed is None or int(passed["passed"]) != cases:
        return f"{passed['passed'] if passed else 0} testcase(s) passed for {cases} named — a name matched nothing"
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cache-rc", default="", help="the task's CACHE_RC: Bazel's first startup option, or empty")
    args = parser.parse_args()
    cases = sorted(NEEDS_CARGO)
    # The cwd is the checkout: `CARGO_MANIFEST_DIR` is baked in relative (`crates/ocx_test_support`).
    ran = subprocess.run([binary(args.cache_rc), "--exact", *cases], cwd=ROOT, text=True, capture_output=True, check=False)
    sys.stdout.write(ran.stdout)
    sys.stderr.write(ran.stderr)
    problem = verdict(ran.returncode, ran.stdout, len(cases))
    if problem:
        print(f"workspace cargo cases: {problem}", file=sys.stderr)
        return 1
    print(f"workspace cargo cases: {len(cases)} testcases passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
