#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`--workspace_status_command` of the release build (`task cross:build:release`).

`build.rs`'s provenance as `STABLE_OCX_<VAR> <value>` lines, one per compile-time variable
`build_info.rs` reads; `provenance.bzl` turns them into a rustc env file. A value this script
cannot produce is not printed and the binary omits the field. Every key is STABLE_: Bazel
re-runs an action on a stable-status change, never on a volatile one.
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from collections.abc import Mapping
from datetime import UTC, datetime

PASS_THROUGH = (
    "__OCX_BUILD_VERSION",
    "__OCX_BUILD_CHANNEL",
    "GITHUB_SERVER_URL",
    "GITHUB_REPOSITORY",
    "GITHUB_RUN_ID",
    "GITHUB_WORKFLOW",
    "GITHUB_REF",
    "GITHUB_SHA",
)
HEADS = "refs/heads/"


def iso8601(ns: int) -> str:
    """vergen's `Iso8601::DEFAULT` rendering of a UTC instant, nanoseconds included."""
    seconds, nanos = divmod(ns, 1_000_000_000)
    return f"{datetime.fromtimestamp(seconds, UTC):%Y-%m-%dT%H:%M:%S}.{nanos:09d}Z"


def git(*args: str) -> str | None:
    result = subprocess.run(["git", *args], capture_output=True, text=True, check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def branch(env: Mapping[str, str]) -> str | None:
    """`GITHUB_REF_NAME` for a branch ref (a tag or pull-request ref names none), else the checked-out branch."""
    if env.get("GITHUB_REF", "").startswith(HEADS):
        return env.get("GITHUB_REF_NAME") or None
    name = git("rev-parse", "--abbrev-ref", "HEAD")
    return None if name in (None, "HEAD") else name


def provenance() -> dict[str, str | None]:
    values: dict[str, str | None] = {}
    sha = git("rev-parse", "HEAD")
    if sha:
        values["VERGEN_GIT_SHA"] = sha
        values["VERGEN_GIT_DESCRIBE"] = git("describe", "--tags", "--dirty")
        status = git("status", "--porcelain", "--untracked-files=no")
        values["VERGEN_GIT_DIRTY"] = None if status is None else str(bool(status)).lower()
        committed = git("log", "-1", "--format=%ct")
        values["VERGEN_GIT_COMMIT_TIMESTAMP"] = committed and iso8601(int(committed) * 1_000_000_000)
    values["__OCX_BUILD_BRANCH"] = branch(os.environ)
    values["VERGEN_BUILD_TIMESTAMP"] = iso8601(time.time_ns())
    for name in PASS_THROUGH:
        values[name] = os.environ.get(name)
    return values


def main() -> int:
    for name, value in provenance().items():
        # A newline would end the status line early, and a value in a rustc env
        # file would inject another variable: build.rs's guard, same response.
        if value and "\n" not in value and "\r" not in value:
            print(f"STABLE_OCX_{name} {value}")
        elif value:
            print(
                f"workspace_status.py: skipping {name}: value contains a newline",
                file=sys.stderr,
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
