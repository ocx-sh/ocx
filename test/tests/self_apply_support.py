# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Helpers for the background self-apply acceptance cases in `test_self_update.py`.

Kept out of that module because the acceptance diff guard admits new test
functions there but no new helpers. Nothing here imports a test module, so the
cases stay independent of one another.
"""

from __future__ import annotations

import re
import shlex
from pathlib import Path
from uuid import uuid4

from src.runner import OcxRunner
from src.terminal import run_on_a_terminal


def plain(terminal: str) -> str:
    """A terminal capture with its ANSI escape sequences removed."""
    return re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", terminal)


def apply_lines(terminal: str) -> list[str]:
    """The ocx-authored apply lines on a terminal capture.

    Only the two apply wordings match: the delegating stand-in sends the real
    setup's own output to the terminal too, and none of that is an apply line.
    """
    return [
        line
        for line in plain(terminal).splitlines()
        if re.search(r"ocx \S+ installed; |Automatic ocx update to ", line)
    ]


def apply_env(
    ocx: OcxRunner, repo: str, home: Path, extra_env: dict[str, str]
) -> dict[str, str]:
    """A full child environment for a background-apply run.

    Interval 0 keeps the throttle from being the reason for silence; `HOME` is
    test-owned because the hand-off child runs a real `ocx self setup`.
    """
    return {
        **ocx.env,
        "__OCX_TESTING_SELF_IMAGE": f"{ocx.registry}/{repo}",
        "HOME": str(home),
        "OCX_SELF_UPDATE": "apply",
        "OCX_UPDATE_CHECK_INTERVAL": "0",
        **extra_env,
    }


def run_on_pty(
    ocx: OcxRunner, tmp_path: Path, env: dict[str, str], args: tuple[str, ...]
) -> tuple[int, str, bytes]:
    """Run `ocx <args>` with stderr on a pty and stdout in a file.

    Returns `(exit status, terminal capture, stdout bytes)`. `env` is the whole
    child environment, so a `CI` on the host cannot reach it.
    """
    stem = uuid4().hex[:8]
    stdout_file = tmp_path / f"stdout-{stem}.bin"
    script = tmp_path / f"run-{stem}.sh"
    script.write_text(
        f"{shlex.join([str(ocx.binary), *args])} >{shlex.join([str(stdout_file)])}\n"
    )
    status, terminal = run_on_a_terminal(script, cwd=tmp_path, env=env)
    return status, terminal, stdout_file.read_bytes()


def current_is_new_stand_in(current: Path) -> bool:
    """Whether the `current` symlink resolves to the 0.0.2 stand-in package."""
    body = (current.resolve() / "content" / "bin" / "ocx").read_text()
    return "Stand-in ocx 0.0.2" in body


def assert_not_applied(current: Path, before: Path, receipt: Path, output: str) -> None:
    """Nothing was installed: `current` unmoved, no hand-off child, no apply line."""
    assert current.resolve() == before, f"`current` must not have moved off {before}"
    assert not receipt.exists(), "the hand-off child must never have started"
    assert apply_lines(output) == [], f"no apply line may be printed; got:\n{output}"


def write_trust_policy(ocx: OcxRunner, scope: str, identity: str, issuer: str) -> None:
    """Write an operator `[[trust.policy]]` into `$OCX_HOME/config.toml`."""
    (ocx.ocx_home / "config.toml").write_text(
        f'[[trust.policy]]\nscope = "{scope}"\n'
        f'signers = [{{ kind = "keyless", identity = "{identity}", oidc_issuer = "{issuer}" }}]\n'
    )
