# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""A pinned ``ocx shell`` whose project directory vanishes tells the user once and keeps its layer.

Drives a real interactive bash on a real pty (``shell_matrix.pty_session``) that reads a
``~/.bashrc`` with the line ``ocx self setup`` would install. No registry: the project is
``[env]``-only and ocx runs ``--offline``.
"""

from __future__ import annotations

import re
import shutil
import sys
from pathlib import Path

import pytest

import shell_matrix as matrix

pytestmark = [
    pytest.mark.skipif(
        sys.platform == "win32",
        reason="the case drives a POSIX-family shell on a pty.",
    ),
    pytest.mark.command("shell"),
]

_OCX = matrix.ocx_binary()
_BASH = shutil.which("bash")

pytestmark.append(
    pytest.mark.skipif(
        _OCX is None, reason="no ocx binary (set OCX_COMMAND, or build test/bin/ocx)."
    )
)
pytestmark.append(
    pytest.mark.skipif(
        _BASH is None,
        reason="bash is needed as the interactive shell (shutil.which returned None).",
    )
)

_TIMEOUT = 90
_CANDIDATE_REL = (
    Path("symlinks") / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"
)
_VANISHED = re.compile(
    r"^ocx: pinned project .* no longer has an ocx\.toml; keeping its layer",
    re.MULTILINE,
)


def _snap(label: str, name: str) -> str:
    return f'printf "%s\\n" "@@{label}.{name}@@${{{name}-{matrix.ABSENT}}}"'


def test_pinned_shell_prints_one_notice_when_its_project_vanishes_and_keeps_the_layer(
    tmp_path: Path,
) -> None:
    """Delete the pinned project, prompt twice -- one notice, the layer stays."""
    assert _OCX is not None and _BASH is not None
    root = tmp_path.resolve()
    home = root / "home"
    home.mkdir()
    ocx_home = home / ".ocx"
    candidate = ocx_home / _CANDIDATE_REL
    candidate.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(_OCX, candidate)
    candidate.chmod(0o755)
    rc = f'eval "$({_OCX} --offline self activate --shell=bash --interactive --no-completion)"\n'
    (home / ".bashrc").write_text(rc, encoding="utf-8")
    (home / ".bash_profile").write_text('. "$HOME/.bashrc"\n', encoding="utf-8")
    project = root / "projects" / "p"
    matrix.write_project(project, 'P_VAR = "p-value"')
    outside = root / "projects" / "outside"
    outside.mkdir()

    env = matrix.clean_env(home, _BASH, ocx_home=ocx_home)
    env["SHELL"] = _BASH
    env["TERM"] = "dumb"
    env["PS1"] = ""
    locked = matrix.run_lock(_OCX, project, env)
    assert locked.returncode == 0, (
        f"ocx lock must succeed for the fixture: {locked.stderr}"
    )

    lines = [
        _snap("a", "P_VAR"),
        f"cd '{outside}'",
        f"rm -rf '{project}'",
        "true",
        "true",
        _snap("b", "P_VAR"),
    ]
    output = matrix.pty_session(
        [str(_OCX), "--offline", "--project", str(project), "shell"],
        lines,
        cwd=project,
        env=env,
        timeout=_TIMEOUT,
    )
    print(f"pty transcript:\n{output}")
    found = matrix.probes(output)

    ctx = f"\npty transcript:\n{output}"
    assert found["a.P_VAR"] == "p-value", f"the shell opens pinned{ctx}"
    assert len(_VANISHED.findall(output)) == 1, (
        f"exactly one vanished-project notice across two prompts{ctx}"
    )
    assert found["b.P_VAR"] == "p-value", f"the layer is kept{ctx}"
