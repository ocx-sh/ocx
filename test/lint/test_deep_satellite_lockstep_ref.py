# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The deep tier's ocx-mirror jobs build the lockstep branch, not only the mirror's default.

An ecosystem break is landed with its consumer upgraded in the same change series, on a
mirror branch named like this repository's branch. A satellite job that always checks out
the mirror's default branch builds the pre-break consumer against the post-break crates,
so every lockstep pull request reads red until the mirror merges first.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

import pytest
import yaml  # a suite dependency (test/pyproject.toml)

WORKFLOW = Path(__file__).resolve().parents[2] / ".github" / "workflows" / "deep-repo.yml"
MIRROR = "ocx-sh/ocx-mirror"
STEP_OUTPUT_REF = re.compile(r"^\$\{\{\s*steps\.(?P<id>[A-Za-z0-9_-]+)\.outputs\.ref\s*\}\}$")


def _mirror_jobs(workflow: dict[str, Any]) -> dict[str, list[dict[str, Any]]]:
    return {
        name: job["steps"]
        for name, job in workflow["jobs"].items()
        if any((s.get("with") or {}).get("repository") == MIRROR for s in job.get("steps", []))
    }


def _violations(workflow: dict[str, Any]) -> list[str]:
    found: list[str] = []
    for name, steps in _mirror_jobs(workflow).items():
        checkout = next(i for i, s in enumerate(steps) if (s.get("with") or {}).get("repository") == MIRROR)
        ref = STEP_OUTPUT_REF.match(str(steps[checkout]["with"].get("ref", "")))
        if not ref:
            found.append(f"{name}: the ocx-mirror checkout takes no `ref: ${{{{ steps.<id>.outputs.ref }}}}`")
            continue
        resolvers = [s for s in steps[:checkout] if s.get("id") == ref.group("id")]
        if not resolvers:
            found.append(f"{name}: no step `{ref.group('id')}` before the ocx-mirror checkout resolves its ref")
            continue
        resolver = resolvers[0]
        branch = str((resolver.get("env") or {}).get("BRANCH", ""))
        if "github.head_ref" not in branch or "github.ref_name" not in branch:
            found.append(f"{name}: the resolver's BRANCH is not `github.head_ref || github.ref_name`")
        if "git ls-remote" not in str(resolver.get("run", "")) or "${{" in str(resolver.get("run", "")):
            found.append(f"{name}: the resolver must probe with `git ls-remote` and read the branch from env only")
    return found


@pytest.fixture(scope="module")
def workflow() -> dict[str, Any]:
    return yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))


def test_both_mirror_jobs_are_found(workflow: dict[str, Any]) -> None:
    assert sorted(_mirror_jobs(workflow)) == ["satellite-contract", "satellite-verify"]


def test_mirror_checkouts_resolve_the_lockstep_branch(workflow: dict[str, Any]) -> None:
    assert _violations(workflow) == []


def test_a_default_branch_checkout_is_detected() -> None:
    """The checker is red on the shape the jobs had: a mirror checkout with no ref."""
    pinned_default = {
        "jobs": {"satellite-verify": {"steps": [{"uses": "actions/checkout", "with": {"repository": MIRROR}}]}}
    }
    assert _violations(pinned_default) == [
        "satellite-verify: the ocx-mirror checkout takes no `ref: ${{ steps.<id>.outputs.ref }}`"
    ]
