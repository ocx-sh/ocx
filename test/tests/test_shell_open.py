# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for bare ``ocx shell``: the user's shell inside the project environment."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = pytest.mark.command("shell")


@pytest.mark.smoke
def test_bare_shell_runs_the_shell_in_the_project_environment(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx shell`` execs ``$SHELL`` with the project tools on ``PATH`` and the exec marker set."""
    repo = f"t_{uuid4().hex[:8]}_shell_open"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n'
    )
    lock = subprocess.run(
        [str(ocx.binary), "lock"], cwd=project, capture_output=True, text=True,
        env=dict(ocx.env), check=False,
    )
    assert lock.returncode == 0, lock.stderr

    # The shell is the test's own Python reading its script from stdin, so the case runs on Windows too;
    # `shutil.which` honours `PATHEXT` there.
    script = (
        "import os, shutil\n"
        "print('SCOPE=' + os.environ.get('__OCX_EXEC_SCOPE', ''))\n"
        "print('HELLO=' + (shutil.which('hello') or ''))\n"
    )
    env = dict(ocx.env)
    env["SHELL"] = sys.executable
    result = subprocess.run(
        [str(ocx.binary), "shell"], cwd=project, input=script, capture_output=True,
        text=True, env=env, check=False,
    )
    assert result.returncode == 0, result.stderr
    values = dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)
    assert values.get("SCOPE") and Path(values["SCOPE"]).samefile(project), result.stdout
    assert values.get("HELLO") and Path(values["HELLO"]).is_absolute(), f"hello is not on PATH: {result.stdout!r}"


@pytest.mark.skipif(sys.platform == "win32", reason="the stub shell is a POSIX script")
def test_bare_shell_with_a_group_puts_the_group_tool_on_path(ocx: OcxRunner, tmp_path: Path) -> None:
    """``ocx shell -g <group>`` composes the named group, whose tool the default set lacks."""
    repo = f"t_{uuid4().hex[:8]}_shell_group"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[group.extra.tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n')
    lock = subprocess.run(
        [str(ocx.binary), "lock"], cwd=project, capture_output=True, text=True, env=dict(ocx.env), check=False
    )
    assert lock.returncode == 0, lock.stderr

    stub = tmp_path / "stub-shell"
    stub.write_text('#!/bin/sh\necho "HELLO=$(command -v hello)"\n')
    stub.chmod(0o755)
    env = dict(ocx.env)
    env["SHELL"] = str(stub)

    plain = subprocess.run([str(ocx.binary), "shell"], cwd=project, capture_output=True, text=True, env=env, check=False)
    assert plain.returncode == 0, plain.stderr
    assert plain.stdout.strip() == "HELLO=", f"the default set must not carry the group tool: {plain.stdout!r}"
    grouped = subprocess.run(
        [str(ocx.binary), "shell", "-g", "extra"], cwd=project, capture_output=True, text=True, env=env, check=False
    )
    assert grouped.returncode == 0, grouped.stderr
    assert "HELLO=/" in grouped.stdout, f"the group tool is not on PATH: {grouped.stdout!r}"


def _locked_project(ocx: OcxRunner, tmp_path: Path, suffix: str) -> Path:
    repo = f"t_{uuid4().hex[:8]}_{suffix}"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\n{repo} = "{ocx.registry}/{repo}:1.0.0"\n')
    lock = subprocess.run(
        [str(ocx.binary), "lock"], cwd=project, capture_output=True, text=True, env=dict(ocx.env), check=False
    )
    assert lock.returncode == 0, lock.stderr
    return project


@pytest.mark.skipif(sys.platform == "win32", reason="the stub shell is a POSIX script")
def test_bare_shell_exits_with_the_shells_exit_code(ocx: OcxRunner, tmp_path: Path) -> None:
    """``exit 7`` inside the opened shell is ``ocx shell``'s own exit status."""
    project = _locked_project(ocx, tmp_path, "shell_exit")
    stub = tmp_path / "stub-shell"
    stub.write_text("#!/bin/sh\nexit 7\n")
    stub.chmod(0o755)
    env = dict(ocx.env)
    env["SHELL"] = str(stub)

    result = subprocess.run(
        [str(ocx.binary), "shell"], cwd=project, capture_output=True, text=True, env=env, check=False
    )
    assert result.returncode == 7, (result.returncode, result.stderr)


@pytest.mark.skipif(sys.platform == "win32", reason="the stub shell is a POSIX script")
def test_bare_shell_inside_an_exec_scope_of_the_same_project_warns_once(ocx: OcxRunner, tmp_path: Path) -> None:
    """A second ``ocx shell`` for the project an enclosing ``ocx exec`` owns names ``--stack``."""
    project = _locked_project(ocx, tmp_path, "shell_nested")
    stub = tmp_path / "stub-shell"
    stub.write_text("#!/bin/sh\nexit 0\n")
    stub.chmod(0o755)
    env = dict(ocx.env)
    env["SHELL"] = str(stub)

    result = subprocess.run(
        [str(ocx.binary), "exec", "--", str(ocx.binary), "shell"],
        cwd=project, capture_output=True, text=True, env=env, check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stderr.count("already inside an `ocx exec` or `ocx shell` environment") == 1, result.stderr
    assert "--stack" in result.stderr
