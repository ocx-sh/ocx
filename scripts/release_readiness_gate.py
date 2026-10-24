#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Refuse a release whose exact commit has no green readiness run.

    python3 scripts/release_readiness_gate.py --sha <40-hex commit>

Asks GitHub (through `gh`) for a successful `release-readiness.yml` run whose head is exactly `--sha`
(`task release:tag` on HEAD and `readiness-gate.yml` before `host` both call it); a run on an
ancestor or sibling commit is no evidence. Every non-zero exit refuses the release: 1 no green run,
2 `--sha` is not a full 40-hex commit, 69 `gh` missing or unauthenticated, 74 any other `gh` failure
or unexpected JSON. Exit 0 prints the run URL and run-name, so a `BYPASS` run is named in the log.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys

WORKFLOW = "release-readiness.yml"
FULL_SHA = re.compile(r"[0-9a-f]{40}")
# What `gh` says on stderr when it has no usable credential; its exit code (1 or 4) is shared with
# every other failure, so the words decide.
UNAUTHENTICATED = re.compile(r"gh auth login|GH_TOKEN|GITHUB_TOKEN|authentication|HTTP 401", re.IGNORECASE)


def run(sha: str) -> int:
    if shutil.which("gh") is None:
        print("readiness gate: `gh` is not installed", file=sys.stderr)
        return 69
    done = subprocess.run(
        [
            "gh",
            "run",
            "list",
            "--workflow",
            WORKFLOW,
            "--commit",
            sha,
            "--status",
            "success",
            "--limit",
            "100",
            "--json",
            "url,displayTitle,headSha,conclusion",
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    if done.returncode != 0:
        print(f"readiness gate: gh run list failed ({done.returncode}): {done.stderr.strip()}", file=sys.stderr)
        return 69 if UNAUTHENTICATED.search(done.stderr) else 74
    try:
        runs = json.loads(done.stdout)
        # Asked for by status, but re-checked here: the exit status below must not rest on a flag's meaning.
        green = [r for r in runs if r["headSha"] == sha and r["conclusion"] == "success"]
    except (ValueError, TypeError, KeyError) as error:
        print(f"readiness gate: unexpected `gh run list` output ({error!r}): {done.stdout[:200]!r}", file=sys.stderr)
        return 74
    if not green:
        print(f"readiness gate: no successful {WORKFLOW} run for {sha}", file=sys.stderr)
        print(f"dispatch it on that commit: gh workflow run {WORKFLOW} --ref <branch holding {sha[:12]}>", file=sys.stderr)
        return 1
    found = green[0]
    print(f"readiness gate: {found['url']}")
    print(f"readiness gate: run-name '{found['displayTitle']}'")
    if "BYPASS" in found["displayTitle"]:
        print("readiness gate: this run is a BYPASS - the test jobs were skipped, the reason is in the run-name")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--sha", required=True, help="the full 40-hex commit the release is cut from")
    args = parser.parse_args(argv)
    if not FULL_SHA.fullmatch(args.sha):
        parser.error(f"--sha must be a full 40-hex commit, got {args.sha!r}")
    return run(args.sha)


if __name__ == "__main__":
    sys.exit(main())
