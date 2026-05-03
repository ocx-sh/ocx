# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx init`` (Unit 7 — specification mode).

Tests encode the contract for the ``ocx init`` command before the
implementation lands. Every test is expected to FAIL against the current
stub (``unimplemented!("Unit 7 — feat(cli): ocx init")``).

Spec source: plan ``auto-findings-md-eventual-fox.md`` Unit 7 §1.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

from src.runner import OcxRunner


# Exit codes per quality-rust-exit_codes.md / error.rs ClassifyExitCode:
# ConfigAlreadyExists → UsageError = 64
EXIT_SUCCESS = 0
EXIT_USAGE_ERROR = 64


def _run_init(ocx: OcxRunner, cwd: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), "init", *extra]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def test_init_creates_minimal_ocx_toml(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init`` in an empty directory creates ``ocx.toml`` with a ``[tools]``
    table and exits 0.

    Spec: Unit 7 §1 bullet 1.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    result = _run_init(ocx, project_dir)

    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init should exit 0; rc={result.returncode}, stderr={result.stderr!r}"
    )
    toml_path = project_dir / "ocx.toml"
    assert toml_path.exists(), "ocx.toml must be created by ocx init"
    content = toml_path.read_text()
    assert "[tools]" in content, (
        f"ocx.toml must contain a [tools] table; got:\n{content}"
    )


def test_init_idempotent_error_when_file_exists(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init`` in a directory that already has ``ocx.toml`` exits with
    UsageError (64) and does NOT overwrite the existing file.

    Spec: Unit 7 §1 bullet 2. Error variant: ``ConfigAlreadyExists`` →
    ``UsageError`` (64) per ``error.rs::ClassifyExitCode``.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    toml_path = project_dir / "ocx.toml"
    original_content = "# sentinel — must not be overwritten\n[tools]\n"
    toml_path.write_text(original_content)

    result = _run_init(ocx, project_dir)

    assert result.returncode == EXIT_USAGE_ERROR, (
        f"ocx init on existing ocx.toml must exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    # File must not be overwritten.
    assert toml_path.read_text() == original_content, (
        "ocx init must not overwrite an existing ocx.toml"
    )


def test_init_minimal_content_matches_research(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx init`` writes non-interactive minimal content: a registry
    declaration and an empty ``[tools]`` table, at most 10 non-blank lines.

    Spec: Unit 7 §1 bullet 3. Design reference:
    ``.claude/artifacts/research_cli_package_manager_conventions.md`` §6.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()

    result = _run_init(ocx, project_dir)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    content = (project_dir / "ocx.toml").read_text()
    non_blank_lines = [ln for ln in content.splitlines() if ln.strip()]
    assert len(non_blank_lines) <= 10, (
        f"ocx init output should be minimal (≤10 non-blank lines); "
        f"got {len(non_blank_lines)}:\n{content}"
    )
    # Must have [tools] so `ocx add` can append entries.
    assert "[tools]" in content, "ocx.toml must contain [tools] table"
    # Must not contain interactive prompts or questionnaire artifacts.
    lower = content.lower()
    for questionnaire_token in ("enter", "please", "y/n", "yes/no", "(y)", "(n)"):
        assert questionnaire_token not in lower, (
            f"ocx init must be non-interactive; found {questionnaire_token!r} in output:\n{content}"
        )
