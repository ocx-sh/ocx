# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx add`` (Unit 7 — specification mode).

Tests encode the contract for the ``ocx add`` command before the
implementation lands. Every test is expected to FAIL against the current
stub (``unimplemented!("Unit 7 — feat(cli): ocx add")``).

Spec source: plan ``auto-findings-md-eventual-fox.md`` Unit 7 §2.
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path
from uuid import uuid4

from src.assertions import assert_symlink_exists, assert_not_exists
from src.helpers import make_package
from src.runner import OcxRunner, registry_dir


EXIT_SUCCESS = 0
# BindingAlreadyExists → UsageError (64) per error.rs ClassifyExitCode
EXIT_USAGE_ERROR = 64


def _run_cmd(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), *args]
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _candidate_path(ocx: OcxRunner, repo: str, tag: str) -> Path:
    return (
        Path(ocx.ocx_home)
        / "symlinks"
        / registry_dir(ocx.registry)
        / repo
        / "candidates"
        / tag
    )


def test_add_appends_to_tools_table(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add <pkg>`` appends to ``[tools]``, updates ``ocx.lock``, and
    installs the package (candidate symlink present).

    Spec: Unit 7 §2 bullet 1.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_tools"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    assert repo in toml_content, (
        f"ocx.toml must contain a binding for {repo!r} after ocx add; got:\n{toml_content}"
    )

    assert (project_dir / "ocx.lock").exists(), "ocx.lock must exist after ocx add"

    candidate = _candidate_path(ocx, repo, "1.0.0")
    assert_symlink_exists(candidate)


def test_add_to_named_group_via_flag(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add --group ci <pkg>`` (flag before positional per OCX convention)
    places the binding under ``[group.ci.tools]``-equivalent, updates lock,
    and installs.

    Spec: Unit 7 §2 bullet 2.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_ci"
    pkg = make_package(ocx, repo, "2.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    result = _run_cmd(ocx, project_dir, "add", "--group", "ci", pkg.fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add --group ci failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    # The ci group must appear in the TOML.
    assert "ci" in toml_content, (
        f"ocx.toml must contain a [group.ci] section after --group ci; got:\n{toml_content}"
    )
    assert repo in toml_content, (
        f"ocx.toml must contain the binding for {repo!r}; got:\n{toml_content}"
    )

    assert (project_dir / "ocx.lock").exists(), "ocx.lock must exist after ocx add --group"

    candidate = _candidate_path(ocx, repo, "2.0.0")
    assert_symlink_exists(candidate)


def test_add_rejects_existing_binding(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add`` when the binding name already exists exits with UsageError
    (64) and leaves ``ocx.toml`` unchanged.

    Spec: Unit 7 §2 bullet 3. Error variant: ``BindingAlreadyExists`` →
    ``UsageError`` (64).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_dup"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo, "2.0.0", tmp_path, new=False, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    original_toml = f'[tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n'
    _write_ocx_toml(project_dir, original_toml)

    result = _run_cmd(ocx, project_dir, "add", f"{ocx.registry}/{repo}:2.0.0")
    assert result.returncode == EXIT_USAGE_ERROR, (
        f"ocx add duplicate should exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )

    assert (project_dir / "ocx.toml").read_text() == original_toml, (
        "ocx.toml must be unchanged when ocx add rejects a duplicate binding"
    )


def test_add_with_bare_identifier_defaults_to_latest(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add <registry>/<repo>`` (no tag) writes a binding with ``:latest``
    tag (or equivalent default) into ``ocx.toml``.

    Spec: Unit 7 §2 bullet 4. Bare-identifier-default-to-latest semantics
    from Unit 3 commit 7b8d7f2a and ``config.rs::parse_tool_map``.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_add_bare"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=True)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(project_dir, "[tools]\n")

    # Bare identifier: no tag component.
    bare_id = f"{ocx.registry}/{repo}"
    result = _run_cmd(ocx, project_dir, "add", bare_id)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add bare identifier failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    toml_content = (project_dir / "ocx.toml").read_text()
    assert repo in toml_content, (
        f"ocx.toml must contain a binding for {repo!r}; got:\n{toml_content}"
    )
    # The stored value should resolve to :latest.
    assert "latest" in toml_content, (
        f"bare identifier add must write ':latest' tag into ocx.toml; got:\n{toml_content}"
    )


def test_add_atomic_full_lockfile_rewrite(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """After ``ocx add new:1.0``, ``ocx.lock`` contains entries for all
    pre-existing tools plus the newly added tool — it is a full rewrite, not
    a partial patch.

    Spec: Unit 7 §2 bullet 5 (research §3 + §6.3 — atomic full rewrite).
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_atomic_a"
    repo_b = f"t_{short}_atomic_b"
    repo_new = f"t_{short}_atomic_new"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_new, "1.0.0", tmp_path, new=True, cascade=False)

    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    _write_ocx_toml(
        project_dir,
        f'[tools]\n'
        f'{repo_a} = "{ocx.registry}/{repo_a}:1.0.0"\n'
        f'{repo_b} = "{ocx.registry}/{repo_b}:1.0.0"\n',
    )

    # First lock: establish baseline with two tools.
    lock_result = subprocess.run(
        [str(ocx.binary), "lock"],
        cwd=project_dir,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_result.returncode == EXIT_SUCCESS, (
        f"baseline ocx lock failed: {lock_result.stderr}"
    )

    # Now add a new tool.
    result = _run_cmd(ocx, project_dir, "add", f"{ocx.registry}/{repo_new}:1.0.0")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx add new tool failed: rc={result.returncode}, stderr={result.stderr!r}"
    )

    lock_text = (project_dir / "ocx.lock").read_text()
    # All three tools must appear in the lock.
    for repo in (repo_a, repo_b, repo_new):
        assert repo in lock_text, (
            f"ocx.lock must contain entry for {repo!r} after atomic rewrite; "
            f"lock content:\n{lock_text}"
        )


def test_add_fails_without_ocx_toml(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add`` in a directory with no ``ocx.toml`` must exit with UsageError
    (64) and must NOT create an ``ocx.toml``.

    Spec: Unit 7 security fix — no-project guard. The command must surface a
    clear "no ocx.toml found" message and refuse to scaffold one implicitly.
    """
    project_dir = tmp_path / "empty_proj"
    project_dir.mkdir()

    result = _run_cmd(ocx, project_dir, "add", f"{ocx.registry}/foo:1.0")
    assert result.returncode == EXIT_USAGE_ERROR, (
        f"ocx add without ocx.toml should exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert "ocx.toml" in result.stderr.lower() or "ocx.toml" in result.stdout.lower(), (
        "error output must mention 'ocx.toml' when no project file is found; "
        f"stderr={result.stderr!r}, stdout={result.stdout!r}"
    )
    assert_not_exists(project_dir / "ocx.toml")


def test_add_rejects_path_traversal_group_name(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx add --group '../../etc' <pkg>`` must exit with UsageError (64) and
    leave ``ocx.toml`` unchanged.

    Spec: Unit 7 security fix #2 — group name validation. Smoke-test that the
    ``InvalidGroupName`` variant is wired and that path-traversal attempts are
    rejected before any filesystem mutation.
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir()
    original_toml = "[tools]\n"
    _write_ocx_toml(project_dir, original_toml)

    result = _run_cmd(ocx, project_dir, "add", "--group", "../../etc", f"{ocx.registry}/foo:1.0")
    assert result.returncode == EXIT_USAGE_ERROR, (
        f"ocx add with path-traversal group should exit {EXIT_USAGE_ERROR}; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert (project_dir / "ocx.toml").read_text() == original_toml, (
        "ocx.toml must be unchanged when add rejects an invalid group name"
    )
