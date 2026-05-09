# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for project-tier ``ocx run`` env composition.

Spec sources:
- ``plan_cli_run_layering.md`` — group composition order; default-group
  PATH precedes inherited PATH.
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Exit codes
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_in(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), *args]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=env)


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _published_tool(
    ocx: OcxRunner,
    tmp_path: Path,
    label: str,
    home_value: str,
) -> tuple[str, str, str]:
    """Publish a package whose env exposes ``{LABEL}_HOME=<home_value>`` (public).

    The ``home_value`` is interpolated at install time via ``${installPath}``
    when it contains ``$``; the test passes a literal string so the env
    composition surface receives a stable assertable value.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_hooks_{label}"
    tag = "1.0.0"
    home_key = label.upper() + "_HOME"
    env = [
        {
            "key": "PATH",
            "type": "path",
            "required": True,
            "value": "${installPath}/bin",
            "visibility": "public",
        },
        {
            "key": home_key,
            "type": "constant",
            "value": home_value,
            "visibility": "public",
        },
    ]
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False, env=env)
    return repo, tag, home_key


# ---------------------------------------------------------------------------
# 1. Group env precedence (later group overrides earlier)
# ---------------------------------------------------------------------------


def test_run_later_group_env_overrides_earlier(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx run -g groupA -g groupB`` — later group's env wins on key collision.

    Two named groups each publish a package whose env entry uses the
    same key (``OVERRIDE_KEY``). Per ``plan_cli_run_layering.md``, the
    group passed last on the command line wins.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_grpa"
    repo_b = f"t_{short}_grpb"
    tag = "1.0.0"

    # Both packages export OVERRIDE_KEY with distinct values.
    env_a = [
        {
            "key": "OVERRIDE_KEY",
            "type": "constant",
            "value": "from_groupA",
            "visibility": "public",
        },
    ]
    env_b = [
        {
            "key": "OVERRIDE_KEY",
            "type": "constant",
            "value": "from_groupB",
            "visibility": "public",
        },
    ]
    make_package(ocx, repo_a, tag, tmp_path, new=True, cascade=False, env=env_a)
    make_package(ocx, repo_b, tag, tmp_path, new=True, cascade=False, env=env_b)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]

[group.groupA]
{repo_a} = "{ocx.registry}/{repo_a}:{tag}"

[group.groupB]
{repo_b} = "{ocx.registry}/{repo_b}:{tag}"
""",
    )

    lock_result = _run_in(ocx, project, "lock")
    assert lock_result.returncode == EXIT_SUCCESS, lock_result.stderr

    # `env` reflects the composed environment; the last group on argv wins.
    result = _run_in(ocx, project, "run", "-g", "groupA", "-g", "groupB", "--", "env")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -g groupA -g groupB failed: stderr={result.stderr!r}"
    )

    # The env dump must contain OVERRIDE_KEY=from_groupB, not from_groupA.
    assert "OVERRIDE_KEY=from_groupB" in result.stdout, (
        f"later group must win on env-key collision; "
        f"expected OVERRIDE_KEY=from_groupB; stdout excerpt:\n{result.stdout[:800]}"
    )
    assert "OVERRIDE_KEY=from_groupA" not in result.stdout, (
        f"earlier group's value must be overridden, not retained; "
        f"stdout excerpt:\n{result.stdout[:800]}"
    )


# ---------------------------------------------------------------------------
# 2. Default-group PATH precedes inherited PATH
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    os.name == "nt",
    reason="Inherited PATH semantics + the test sentinel use POSIX path separator",
)
def test_run_default_path_precedes_inherited(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Default-group tools' bin dirs prepend to ``$PATH`` per layering rule.

    Plan_cli_run_layering.md §composition: project tools' bin directories
    must appear *before* inherited ``$PATH`` entries in the child env, so
    a project-installed binary shadows a same-named system binary.

    The check: insert a sentinel directory containing a fake ``hello``
    into the inherited PATH; project's ``hello`` must still resolve
    first.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_pathprec"
    tag = "1.0.0"
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""",
    )

    lock_result = _run_in(ocx, project, "lock")
    assert lock_result.returncode == EXIT_SUCCESS, lock_result.stderr

    # Sentinel: a directory containing a `hello` script that prints
    # "from_inherited". If the project bin precedes inherited PATH,
    # `hello` resolves to the project's binary, NOT this sentinel.
    sentinel_dir = tmp_path / "sentinel"
    sentinel_dir.mkdir()
    sentinel_hello = sentinel_dir / "hello"
    sentinel_hello.write_text('#!/bin/sh\necho "from_inherited"\n')
    sentinel_hello.chmod(0o755)

    inherited_path = f"{sentinel_dir}:{os.environ.get('PATH', '')}"

    # `ocx run -- env` dumps PATH; we assert the project's bin dir
    # appears before the sentinel dir in the composed PATH.
    result = _run_in(
        ocx,
        project,
        "run",
        "--",
        "env",
        extra_env={"PATH": inherited_path},
    )
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx run -- env failed: stderr={result.stderr!r}"
    )

    # Find PATH= line in the env dump.
    path_lines = [
        line for line in result.stdout.splitlines() if line.startswith("PATH=")
    ]
    assert path_lines, f"PATH must appear in composed env dump; stdout:\n{result.stdout[:800]}"
    composed_path = path_lines[0].removeprefix("PATH=")
    sentinel_idx = composed_path.find(str(sentinel_dir))
    # The project's bin dir lives under OCX_HOME's symlinks tree; use the
    # OCX_HOME root as a coarse marker for "project tool dir".
    ocx_home_idx = composed_path.find(str(ocx.ocx_home))

    assert ocx_home_idx >= 0, (
        f"project tool bin dir (under OCX_HOME) must appear in composed PATH; "
        f"PATH={composed_path!r}"
    )
    if sentinel_idx >= 0:
        assert ocx_home_idx < sentinel_idx, (
            f"project bin dir must precede inherited PATH entry; "
            f"OCX_HOME idx={ocx_home_idx}, sentinel idx={sentinel_idx}; "
            f"PATH={composed_path!r}"
        )
