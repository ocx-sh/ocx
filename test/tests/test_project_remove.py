# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx remove`` (Unit 7 — specification mode).

Tests encode the contract for the ``ocx remove`` command before the
implementation lands. Every test is expected to FAIL against the current
stub (``unimplemented!("Unit 7 — feat(cli): ocx remove")``).

Spec source: plan ``auto-findings-md-eventual-fox.md`` Unit 7 §3.
"""
from __future__ import annotations

import subprocess
from pathlib import Path
from uuid import uuid4

from src.assertions import assert_not_exists, assert_symlink_exists
from src.helpers import make_package
from src.runner import OcxRunner, registry_dir


EXIT_SUCCESS = 0
# BindingNotFound → NotFound (79) per error.rs ClassifyExitCode
EXIT_NOT_FOUND = 79


def _run_cmd(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), *args]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> None:
    (project_dir / "ocx.toml").write_text(body)


def _candidate_path(ocx: OcxRunner, repo: str, tag: str) -> Path:
    return (
        Path(ocx.ocx_home)
        / "symlinks"
        / registry_dir(ocx.registry)
        / repo
        / "candidates"
        / tag
    )


def _setup_project_with_tool(
    ocx: OcxRunner, tmp_path: Path, repo: str, tag: str
) -> tuple[Path, Path]:
    """Create project dir + ocx.toml + lock + install for one tool.

    Returns (project_dir, candidate_symlink_path).
    """
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir(exist_ok=True)
    _write_ocx_toml(
        project_dir,
        f'[tools]\n{repo} = "{ocx.registry}/{repo}:{tag}"\n',
    )
    # Establish lock + install baseline.
    lock_r = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock_r.stderr}"
    install_r = subprocess.run(
        [str(ocx.binary), "install", f"{ocx.registry}/{repo}:{tag}"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert install_r.returncode == EXIT_SUCCESS, f"baseline install failed: {install_r.stderr}"
    return project_dir, _candidate_path(ocx, repo, tag)


def test_remove_drops_binding_and_uninstalls(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove <name>`` removes the binding from ``ocx.toml``, rewrites
    ``ocx.lock``, and uninstalls the package (candidate symlink absent).

    Spec: Unit 7 §3 bullet 1.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_rem_drop"
    project_dir, candidate = _setup_project_with_tool(ocx, tmp_path, repo, "1.0.0")

    assert_symlink_exists(candidate)  # sanity: installed before remove

    result = _run_cmd(ocx, project_dir, "remove", repo)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx remove failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    assert repo not in toml_content, (
        f"binding for {repo!r} must be gone from ocx.toml after remove; got:\n{toml_content}"
    )

    lock_text = (project_dir / "ocx.lock").read_text()
    assert repo not in lock_text, (
        f"lock entry for {repo!r} must be removed from ocx.lock; got:\n{lock_text}"
    )

    assert_not_exists(candidate)


def test_remove_rejects_unknown_binding(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove nonexistent`` exits with NotFound (79) when the binding
    is not declared in any group. ``ocx.toml`` and ``ocx.lock`` stay unchanged.

    Spec: Unit 7 §3 bullet 2. Error variant: ``BindingNotFound`` →
    ``NotFound`` (79).
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    original_toml = "[tools]\n"
    _write_ocx_toml(project_dir, original_toml)

    result = _run_cmd(ocx, project_dir, "remove", "nonexistent")
    assert result.returncode == EXIT_NOT_FOUND, (
        f"ocx remove nonexistent should exit {EXIT_NOT_FOUND}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert (project_dir / "ocx.toml").read_text() == original_toml, (
        "ocx.toml must be unchanged when remove target not found"
    )
    # Lock file must also remain unchanged (or absent if never created).
    lock_path = project_dir / "ocx.lock"
    if lock_path.exists():
        assert "nonexistent" not in lock_path.read_text()


def test_remove_from_named_group(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx remove <name>`` finds a binding in a named group (``[group.ci]``)
    and removes it — no explicit ``--group`` flag required.

    Spec: Unit 7 §3 bullet 3. ``mutate::remove_binding`` searches both
    default and named groups.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_rem_grp"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(
        project_dir,
        f'[tools]\n\n[group.ci]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n',
    )

    # Lock the group so ocx.lock reflects the ci-group entry.
    lock_r = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_r.returncode == EXIT_SUCCESS, f"baseline lock failed: {lock_r.stderr}"

    result = _run_cmd(ocx, project_dir, "remove", repo)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx remove from named group failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    # The binding value line must be gone; the [group.ci] header may remain empty.
    assert f'{repo} = ' not in toml_content, (
        f"binding for {repo!r} must be removed from [group.ci]; got:\n{toml_content}"
    )
