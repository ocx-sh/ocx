# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for per-group binding uniqueness and in-place project lock.

Two design contracts exercised:

1. **Per-group uniqueness**: the same binding name may coexist in the default
   ``[tools]`` table AND in named ``[group.*]`` tables. ``ocx add`` rejects
   duplicates only within the *same* group. ``ocx remove`` gains ``--group``
   to disambiguate when the same name appears in multiple groups.

2. **In-place project lock**: ``.ocx-lock`` sentinel file is deleted; ``ocx.toml``
   itself is the project mutex anchor. No ``.ocx-lock`` file must appear on disk
   after any project-mutating command.
"""
from __future__ import annotations

import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Exit code constants — align with crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64      # UsageError: BindingAlreadyExists, BindingAmbiguous, no ocx.toml


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_cmd(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run an ocx subcommand from ``cwd`` and capture all output."""
    cmd = [str(ocx.binary), *args]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _init_project(ocx: OcxRunner, project_dir: Path) -> None:
    """Create a minimal ``ocx.toml`` in ``project_dir`` via ``ocx init``."""
    result = _run_cmd(ocx, project_dir, "init")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx init failed: rc={result.returncode}, stderr={result.stderr!r}"
    )


def _add_tool(
    ocx: OcxRunner,
    project_dir: Path,
    fq: str,
    group: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx add [--group <group>] <fq>`` from ``project_dir``."""
    args = ["add"]
    if group is not None:
        args += ["--group", group]
    args.append(fq)
    return _run_cmd(ocx, project_dir, *args)


def _remove_tool(
    ocx: OcxRunner,
    project_dir: Path,
    name: str,
    group: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ``ocx remove [--group <group>] <name>`` from ``project_dir``."""
    args = ["remove"]
    if group is not None:
        args += ["--group", group]
    args.append(name)
    return _run_cmd(ocx, project_dir, *args)


def _read_toml(project_dir: Path) -> str:
    return (project_dir / "ocx.toml").read_text()


# ---------------------------------------------------------------------------
# 1. Same binding name in default [tools] AND named [group.ci] succeeds
# ---------------------------------------------------------------------------


def test_add_same_name_default_and_named_group_succeeds(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add cmake:1.0.0`` followed by ``ocx add --group ci cmake:1.0.0``
    must both succeed and leave ``cmake`` in both ``[tools]`` and
    ``[group.ci]``.

    Per-group uniqueness contract: the same binding name may coexist in the
    default ``[tools]`` table and in any named ``[group.*]`` table.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_grp_both"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    binding = repo  # repo basename = binding key

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _init_project(ocx, project_dir)

    # Add to default group.
    r1 = _add_tool(ocx, project_dir, pkg.fq)
    assert r1.returncode == EXIT_SUCCESS, (
        f"ocx add to default group failed: rc={r1.returncode}, stderr={r1.stderr!r}"
    )

    # Add same name to named group [group.ci].
    r2 = _add_tool(ocx, project_dir, pkg.fq, group="ci")
    assert r2.returncode == EXIT_SUCCESS, (
        f"ocx add --group ci same name failed: rc={r2.returncode}, stderr={r2.stderr!r}"
    )

    toml = _read_toml(project_dir)
    # Binding must appear in the default [tools] section.
    assert binding in toml, (
        f"binding {binding!r} must be in ocx.toml after add to default; got:\n{toml}"
    )
    # The ci group section must also carry the binding.
    assert "ci" in toml, (
        f"[group.ci] section must appear in ocx.toml after --group ci add; got:\n{toml}"
    )
    # Both occurrences must be present: at least 2 lines with the binding key.
    assert toml.count(binding) >= 2, (
        f"binding {binding!r} must appear in both [tools] and [group.ci]; "
        f"got {toml.count(binding)} occurrences:\n{toml}"
    )


# ---------------------------------------------------------------------------
# 2. Same binding name in the same group is rejected
# ---------------------------------------------------------------------------


def test_add_same_name_same_group_rejected(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Second ``ocx add cmake:1.0.0`` targeting the *same* default group
    must exit 64 (UsageError) and mention the group label ``default`` in
    stderr.

    Error variant: ``BindingAlreadyExists`` → ``UsageError`` (64).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_grp_dup"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _init_project(ocx, project_dir)

    # First add must succeed.
    r1 = _add_tool(ocx, project_dir, pkg.fq)
    assert r1.returncode == EXIT_SUCCESS, (
        f"first ocx add failed: rc={r1.returncode}, stderr={r1.stderr!r}"
    )

    original_toml = _read_toml(project_dir)

    # Second add to the same (default) group must fail.
    r2 = _add_tool(ocx, project_dir, pkg.fq)
    assert r2.returncode == EXIT_USAGE, (
        f"duplicate add to default group should exit {EXIT_USAGE}; "
        f"rc={r2.returncode}, stderr={r2.stderr!r}"
    )
    # Error message must mention "already exists" and the group label.
    combined = (r2.stderr + r2.stdout).lower()
    assert "already" in combined, (
        f"stderr must mention 'already' for BindingAlreadyExists; stderr:\n{r2.stderr}"
    )
    assert "default" in combined, (
        f"stderr must name the 'default' group; stderr:\n{r2.stderr}"
    )
    # ocx.toml must be unchanged.
    assert _read_toml(project_dir) == original_toml, (
        "ocx.toml must be unchanged after a rejected duplicate add"
    )


# ---------------------------------------------------------------------------
# 3. --group targets only the named group on remove
# ---------------------------------------------------------------------------


def test_remove_with_group_targets_named_group(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove cmake --group ci`` removes the binding from ``[group.ci]``
    but leaves ``cmake`` intact in the default ``[tools]`` table.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_grp_tgt"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    binding = repo

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _init_project(ocx, project_dir)

    # Add to both default and ci group.
    r1 = _add_tool(ocx, project_dir, pkg.fq)
    assert r1.returncode == EXIT_SUCCESS, r1.stderr
    r2 = _add_tool(ocx, project_dir, pkg.fq, group="ci")
    assert r2.returncode == EXIT_SUCCESS, r2.stderr

    # Remove from ci only.
    r3 = _remove_tool(ocx, project_dir, binding, group="ci")
    assert r3.returncode == EXIT_SUCCESS, (
        f"ocx remove --group ci failed: rc={r3.returncode}, stderr={r3.stderr!r}"
    )

    toml = _read_toml(project_dir)

    # The binding must still exist in the default group (at least one occurrence).
    assert binding in toml, (
        f"binding {binding!r} must remain in [tools] after --group ci remove; "
        f"got:\n{toml}"
    )

    # After remove, there should be at most one occurrence of the binding key
    # (the one in [tools]); the ci entry must be gone.
    # We check that the ci group no longer holds the binding by verifying that
    # no line of the form `{binding} = "..."` appears in a [group.ci] context.
    # Simplest proxy: the toml has binding appearing once, not twice.
    assert toml.count(f"{binding} =") == 1, (
        f"after --group ci remove, {binding!r} must appear exactly once "
        f"(in [tools] only); got {toml.count(f'{binding} =')} occurrences:\n{toml}"
    )


# ---------------------------------------------------------------------------
# 4. remove without --group when ambiguous errors
# ---------------------------------------------------------------------------


def test_remove_without_group_when_ambiguous_errors(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove cmake`` (no ``--group``) when ``cmake`` exists in both
    default and ``[group.ci]`` must exit 64 (UsageError) and mention
    ``ambiguous`` plus both group names (``default``, ``ci``) in stderr.

    Error variant: ``BindingAmbiguous`` → ``UsageError`` (64).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_grp_amb"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    binding = repo

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _init_project(ocx, project_dir)

    r1 = _add_tool(ocx, project_dir, pkg.fq)
    assert r1.returncode == EXIT_SUCCESS, r1.stderr
    r2 = _add_tool(ocx, project_dir, pkg.fq, group="ci")
    assert r2.returncode == EXIT_SUCCESS, r2.stderr

    original_toml = _read_toml(project_dir)

    # Remove without --group on an ambiguous binding must fail.
    r3 = _remove_tool(ocx, project_dir, binding)
    assert r3.returncode == EXIT_USAGE, (
        f"ambiguous remove without --group should exit {EXIT_USAGE}; "
        f"rc={r3.returncode}, stderr={r3.stderr!r}"
    )

    combined = (r3.stderr + r3.stdout).lower()
    # The error message uses "multiple groups" / "disambiguate" to convey
    # ambiguity — accept either wording so the test is not brittle to
    # exact phrasing while still verifying the semantic content.
    assert "multiple groups" in combined or "ambiguous" in combined, (
        f"stderr must indicate the binding exists in multiple groups; stderr:\n{r3.stderr}"
    )
    assert "default" in combined, (
        f"stderr must name the 'default' group; stderr:\n{r3.stderr}"
    )
    assert "ci" in combined, (
        f"stderr must name the 'ci' group; stderr:\n{r3.stderr}"
    )

    # ocx.toml must be unchanged.
    assert _read_toml(project_dir) == original_toml, (
        "ocx.toml must be unchanged after an ambiguous remove error"
    )


# ---------------------------------------------------------------------------
# 5. remove without --group when unique succeeds
# ---------------------------------------------------------------------------


def test_remove_without_group_when_unique_succeeds(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove cmake`` (no ``--group``) when ``cmake`` exists only in
    ``[group.ci]`` succeeds and removes it from that group.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_grp_uniq"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    binding = repo

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _init_project(ocx, project_dir)

    # Add to ci group only (not to default).
    r1 = _add_tool(ocx, project_dir, pkg.fq, group="ci")
    assert r1.returncode == EXIT_SUCCESS, r1.stderr

    # Remove without --group; exactly one group contains it → should succeed.
    r2 = _remove_tool(ocx, project_dir, binding)
    assert r2.returncode == EXIT_SUCCESS, (
        f"remove without --group on unique binding failed: "
        f"rc={r2.returncode}, stderr={r2.stderr!r}"
    )

    toml = _read_toml(project_dir)
    assert f"{binding} =" not in toml, (
        f"binding {binding!r} must be gone from ocx.toml after remove; got:\n{toml}"
    )


# ---------------------------------------------------------------------------
# 6 + 7. No .ocx-lock sentinel after add / lock
# ---------------------------------------------------------------------------


def test_no_ocx_lock_sentinel_after_add(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """After ``ocx init`` + ``ocx add``, ``.ocx-lock`` must NOT exist in the
    project directory.

    The in-place lock scheme uses an exclusive ``flock(2)`` on ``ocx.toml``
    itself; no sentinel file should be written.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_nosentinel_add"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _init_project(ocx, project_dir)

    r = _add_tool(ocx, project_dir, pkg.fq)
    assert r.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={r.returncode}, stderr={r.stderr!r}"
    )

    sentinel = project_dir / ".ocx-lock"
    assert not sentinel.exists(), (
        ".ocx-lock sentinel must NOT exist after ocx add; "
        "the in-place lock is held only during the mutation and released immediately"
    )


def test_no_ocx_lock_sentinel_after_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """After ``ocx init`` + ``ocx add`` + ``ocx lock``, ``.ocx-lock`` must
    NOT exist in the project directory.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_nosentinel_lock"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _init_project(ocx, project_dir)

    r_add = _add_tool(ocx, project_dir, pkg.fq)
    assert r_add.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={r_add.returncode}, stderr={r_add.stderr!r}"
    )

    r_lock = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert r_lock.returncode == EXIT_SUCCESS, (
        f"ocx lock failed: rc={r_lock.returncode}, stderr={r_lock.stderr!r}"
    )

    sentinel = project_dir / ".ocx-lock"
    assert not sentinel.exists(), (
        ".ocx-lock sentinel must NOT exist after ocx lock; "
        "the in-place lock is held only during the resolve cycle and released immediately"
    )
