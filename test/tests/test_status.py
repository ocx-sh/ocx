# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx status``.

``status`` reports what ``ocx.toml`` and ``ocx.lock`` say, with no resolution:
no registry, no platform selection, no package metadata. Its whole point is the
states that make every other project-tier command refuse to run — an absent
lock (78 elsewhere), a drifted one (65 elsewhere), an unparseable one — so each
of those is asserted to exit 0 here and carry the condition in the payload.

Exit codes per quality-rust-exit_codes.md:
    0  = Success (including "no lock", "stale lock", "unreadable lock")
    64 = UsageError (no ocx.toml in scope; a selector was passed)
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = pytest.mark.command("status")

EXIT_SUCCESS = 0
EXIT_USAGE = 64


def _run(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args], cwd=cwd, capture_output=True, text=True, env=ocx.env, check=False
    )


def _status(ocx: OcxRunner, cwd: Path) -> dict:
    result = _run(ocx, cwd, "--format", "json", "status")
    assert result.returncode == EXIT_SUCCESS, (
        f"status must exit 0, got {result.returncode}\nstderr: {result.stderr}"
    )
    return json.loads(result.stdout)


def _project(ocx: OcxRunner, tmp_path: Path) -> Path:
    project = tmp_path / "project"
    project.mkdir()
    result = _run(ocx, project, "init")
    assert result.returncode == EXIT_SUCCESS, result.stderr
    return project


def test_status_reports_drift_instead_of_refusing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A drifted lock exits 0 with ``current: false`` — and names WHICH binding
    drifted, which the hash alone cannot.

    Every other project-tier command exits 65 here. Status is the one that has
    to answer.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path)
    assert _run(ocx, project, "add", "--no-pull", pkg.short).returncode == EXIT_SUCCESS

    # Declare a second binding without re-locking.
    config = project / "ocx.toml"
    config.write_text(
        config.read_text() + f'undeclared-in-lock = "{ocx.registry}/{unique_repo}:1.0.0"\n'
    )

    # A command that enforces the staleness gate refuses outright...
    stale = _run(ocx, project, "pull")
    assert stale.returncode == 65, (
        f"expected the staleness gate to fire for a sibling command, got {stale.returncode}"
    )

    # ...while status answers.
    data = _status(ocx, project)
    assert data["lock"]["current"] is False
    assert data["lock"]["declaration_hash"] != data["lock"]["declaration_hash_expected"]

    tools = data["groups"]["default"]["tools"]
    assert "platforms" not in tools["undeclared-in-lock"], (
        "the binding added since the last lock is the one without platforms"
    )
    assert "platforms" in tools[unique_repo], "the already-locked sibling keeps its pins"
