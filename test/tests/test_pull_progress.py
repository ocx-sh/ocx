# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Regression test for symmetric progress output from ``ocx pull --group``.

Bug: ``ocx pull -g <group>`` produced no progress output even in a TTY,
while ``ocx package pull <id>`` (single-package path) showed spinners.

Root cause: ``pull_all`` had a ``packages.len() == 1`` fast path that
instrumented its future with a ``spinner_span``; the multi-package JoinSet
path instrumented only per-task spans with no outer parent span. Because no
outer span existed for the N-package case, no overall progress indicator ever
fired — only the per-task child spans were created, and their connection to
``tracing-indicatif`` was masked by the missing parent context.

Fix: remove the single-package fast path; unify into the JoinSet dispatch;
add a ``tracing::info!`` event inside the outer ``info_span!("Pulling", ...)``
parent span so the batch is visible in log output regardless of TTY.

Test strategy
-------------
The ``IndicatifLayer`` only renders to stderr when stderr is a TTY
(``ProgressMode::detect()``).  Under pytest, ``capture_output=True`` pipes
stderr, so the indicatif layer is never registered.  Instead we assert on
the ``tracing::info!`` event emitted at the start of the outer span when
``RUST_LOG=info`` (or ``OCX_LOG=info``) is set.

The event is intentionally added as part of the fix; before the fix this
file MUST NOT appear here at all, making the test fail on pre-fix binaries.
After the fix the event appears unconditionally once per ``pull_all`` call
with N >= 1 packages.
"""
from __future__ import annotations

import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Helpers (DAMP — co-located with the tests that need them)
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0


def _ocx_cmd(ocx: OcxRunner, *args: str) -> list[str]:
    return [str(ocx.binary), *args]


def _run_pull(
    ocx: OcxRunner,
    cwd: Path,
    *extra: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx pull`` from ``cwd`` and capture stderr."""
    cmd = _ocx_cmd(ocx, "pull", *extra)
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
    )


def _run_lock(
    ocx: OcxRunner,
    cwd: Path,
) -> subprocess.CompletedProcess[str]:
    cmd = _ocx_cmd(ocx, "lock")
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _published_tool(
    ocx: OcxRunner, tmp_path: Path, label: str
) -> tuple[str, str]:
    short = uuid4().hex[:8]
    repo = f"t_{short}_progress_{label}"
    tag = "1.0.0"
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False)
    return repo, tag


def _write_ocx_toml(project_dir: Path, body: str) -> None:
    (project_dir / "ocx.toml").write_text(body)


# ---------------------------------------------------------------------------
# Phase 3 regression test
#
# The test asserts that ``ocx pull -g <group>`` emits a progress-related log
# event in stderr when ``RUST_LOG=info`` is set.  The event is produced by
# the ``tracing::info!`` call added inside the outer ``info_span!("Pulling")``
# in ``pull_all`` as part of the fix.
#
# BEFORE the fix: no outer span → no info event → assertion fails.
# AFTER the fix:  outer span with info event → assertion passes.
# ---------------------------------------------------------------------------


def test_pull_group_emits_progress_event_in_stderr(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx pull -g ci`` with RUST_LOG=info must emit a 'Pulling' log event.

    This is the regression test for the symmetric-progress bug.  The
    ``IndicatifLayer`` only renders to a TTY; under pytest's pipe, progress
    bars are suppressed.  Instead we assert on the ``tracing::info!`` event
    emitted inside the outer ``info_span!("Pulling", ...)`` that the fix adds
    to ``pull_all``.

    The event must appear in stderr when ``RUST_LOG=info`` is active.  Its
    presence proves that the outer span is created and entered for the N-package
    group path, giving ``tracing-indicatif`` the hook it needs to render a
    spinner in a real TTY.

    BEFORE fix: no outer span, no info event, test FAILS.
    AFTER  fix: outer span with info event, test PASSES.
    """
    repo_a, tag_a = _published_tool(ocx, tmp_path, "a")
    repo_b, tag_b = _published_tool(ocx, tmp_path, "b")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[group.ci]
tool_a = "{ocx.registry}/{repo_a}:{tag_a}"
tool_b = "{ocx.registry}/{repo_b}:{tag_b}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, (
        f"ocx lock failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    )

    result = _run_pull(
        ocx,
        project,
        "--group",
        "ci",
        extra_env={"RUST_LOG": "info"},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull -g ci failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    # The outer span's info event must appear in stderr.
    # The exact message is "pulling N package(s)" — emitted by
    # ``tracing::info!(count = N, "pulling")`` inside the outer span in
    # ``pull_all``.  We match on the lowercase "pulling" substring so the
    # assertion is resilient to minor wording changes while still being
    # discriminating enough to prove the outer span fired.
    assert "pulling" in result.stderr.lower(), (
        "expected a 'Pulling'/'pulling' progress event in stderr from the "
        "outer pull_all span, but none found.\n"
        f"stderr:\n{result.stderr!r}\n"
        "This is the regression indicator: the outer info_span!(\"Pulling\") "
        "must be created for the multi-package group path so tracing-indicatif "
        "can render a spinner in a real TTY."
    )


def test_pull_single_package_also_emits_progress_event(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Single-tool group also emits the 'Pulling' progress event.

    Symmetric with ``test_pull_group_emits_progress_event_in_stderr``:
    after the fix the single-package fast path is removed and the same outer
    span covers N=1 identically to N>1.  Both cases must produce the event.
    """
    repo, tag = _published_tool(ocx, tmp_path, "single")

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[group.ci]
only_tool = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock = _run_lock(ocx, project)
    assert lock.returncode == EXIT_SUCCESS, (
        f"ocx lock failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    )

    result = _run_pull(
        ocx,
        project,
        "--group",
        "ci",
        extra_env={"RUST_LOG": "info"},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx pull -g ci (single tool) failed: rc={result.returncode}\n"
        f"stderr:\n{result.stderr}"
    )

    assert "pulling" in result.stderr.lower(), (
        "expected a 'Pulling'/'pulling' progress event in stderr even for a "
        "single-tool group after the fast path is removed.\n"
        f"stderr:\n{result.stderr!r}"
    )
