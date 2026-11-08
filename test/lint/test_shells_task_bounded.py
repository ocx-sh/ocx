# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The `shells` task bounds the zoo container's scratch space.

Without a bound, `tmp_path` and every per-test home land in the container's
overlay layer on the host disk, so a runaway writer fills the host instead of
failing the run. The container gets a size-limited tmpfs, and pytest's base
temp, `TMPDIR` and `HOME` all live inside it, so ENOSPC names the bound.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest
import yaml  # a suite dependency (test/pyproject.toml)

TASKFILE = Path(__file__).resolve().parents[1] / "taskfile.yml"
TMPFS = re.compile(r"--tmpfs\s+(?P<mount>/\S+?):(?:\S*,)?size=(?P<size>\d+[kmg])\b", re.IGNORECASE)
TMPFS_OPTIONS = re.compile(r"--tmpfs\s+/\S+?:(?P<options>\S+)")


@pytest.fixture(scope="module")
def docker_run() -> str:
    tasks = yaml.safe_load(TASKFILE.read_text(encoding="utf-8"))["tasks"]
    runs = [c for c in tasks["shells"]["cmds"] if isinstance(c, str) and "docker run" in c]
    assert len(runs) == 1, f"expected exactly one `docker run` in the shells task, found {len(runs)}"
    return " ".join(runs[0].split())


def _mount(docker_run: str) -> str:
    found = TMPFS.search(docker_run)
    assert found, "the shells container has no size-bounded `--tmpfs <dir>:size=<N>g`: scratch lands in the host overlay"
    return found.group("mount").rstrip("/")


def test_scratch_is_a_size_bounded_tmpfs(docker_run: str) -> None:
    assert _mount(docker_run)


def test_scratch_tmpfs_allows_executing_the_seeded_binary(docker_run: str) -> None:
    found = TMPFS_OPTIONS.search(docker_run)
    assert found, "the shells container has no `--tmpfs <dir>:<options>`"
    assert "exec" in found.group("options").split(","), (
        "docker mounts a tmpfs noexec by default, so the seeded ocx candidate cannot run from the scratch: "
        f"every shim then activates nothing (options: {found.group('options')})"
    )


def test_a_noexec_default_is_detected() -> None:
    """The options matcher is red on the size-only shape the task had."""
    size_only = "docker run --rm --tmpfs /tmp:size=10g -e TMPDIR=/tmp img"
    found = TMPFS_OPTIONS.search(size_only)
    assert found is not None
    assert "exec" not in found.group("options").split(",")


def test_pytest_basetemp_is_inside_the_bound(docker_run: str) -> None:
    mount = _mount(docker_run)
    found = re.search(r"--basetemp[= ](\S+)", docker_run)
    assert found, "pytest in the zoo has no --basetemp: tmp_path falls outside the bounded tmpfs"
    assert found.group(1).startswith(mount + "/") or found.group(1) == mount


@pytest.mark.parametrize("variable", ["TMPDIR", "HOME"])
def test_environment_roots_are_inside_the_bound(docker_run: str, variable: str) -> None:
    mount = _mount(docker_run)
    found = re.search(rf"-e\s+{variable}=(\S+)", docker_run)
    assert found, f"{variable} is not set for the zoo container: scratch from it leaves the bounded tmpfs"
    assert found.group(1).startswith(mount + "/") or found.group(1) == mount


def test_a_missing_bound_is_detected() -> None:
    """The matcher is red on the unbounded shape the task had."""
    unbounded = "docker run --rm -v a:/b:ro -e OCX_ACTIVATION_BINARY=/work/ocx img python3 -m pytest"
    assert TMPFS.search(unbounded) is None
