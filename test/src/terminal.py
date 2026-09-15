# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Run a shell script on a pseudo-terminal and capture what reached it.

The assertion these helpers exist for is on **bytes written to a controlling
terminal** — a progress bar, or its absence. Nothing captured through a pipe
can stand in for that: with stderr piped, ocx detects no terminal and renders
nothing, so a pipe-based test is green whether or not the bar would have been
drawn.
"""

from __future__ import annotations

from pathlib import Path

import pexpect
import pytest

requires_pty = pytest.mark.skipif(
    not hasattr(pexpect, "spawn"),
    reason="the assertion is on bytes written to a controlling terminal; pexpect imports "
    "here but defines no spawn, so there is no pty to attach the script to",
)


def run_on_a_terminal(
    script: Path, cwd: Path, env: dict[str, str], timeout: int = 300
) -> tuple[int, str]:
    """Run `bash --norc <script>` attached to a pty, returning `(status, terminal)`.

    `pexpect` puts the child in its own session with the pty as its
    **controlling terminal**, which is the state a developer's shell is in.
    Whatever the script leaves on the terminal — every stream it does not
    redirect into a file, plus anything written to `/dev/tty` directly — is
    what the returned string holds. A script that redirects both standard
    streams therefore isolates the controlling-terminal channel; one that
    leaves stderr attached observes what a TTY on fd 2 gets.

    A wide window keeps the terminal from truncating a rendered line before the
    token the caller greps for; 24x80 would clip a long pinned identifier.

    `TERM` is defaulted when the caller's env carries none: `console::is_dumb`
    treats an unset `TERM` as a dumb terminal and indicatif then hides its
    stderr target outright, so a runner env without it (the suite's
    `OcxRunner.env`) would make every "nothing was drawn" assertion vacuous.
    """
    env = {"TERM": "xterm-256color", **env}
    child = pexpect.spawn(
        "bash",
        ["--norc", str(script)],
        cwd=str(cwd),
        env=env,
        timeout=timeout,
        encoding="utf-8",
        codec_errors="replace",
        dimensions=(40, 200),
    )
    child.expect(pexpect.EOF)
    terminal = child.before or ""
    child.close()
    return child.exitstatus, terminal
