from __future__ import annotations

# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Helpers for evaluating shell export lines in a subprocess safely.

The fragile pattern::

    script = f'eval "{env_export}"\n{body}'
    subprocess.run(["bash", "--norc", "-c", script], ...)

breaks when ``env_export`` contains double-quotes, dollar signs, exclamation marks
(bash history expansion), or backslashes.  The safe alternative is to write the
export lines to a temp file and source the file with the POSIX dot-operator::

    . /tmp/tmpXXXXXX

This module provides ``run_after_sourcing`` as the single entry point so all
three affected test files share the same implementation.
"""

import subprocess
import tempfile
from pathlib import Path


def run_after_sourcing(
    env_export: str,
    body: str,
    cwd: Path,
    env: dict[str, str],
    shell: str = "bash",
    shell_flags: str = "--norc",
) -> "subprocess.CompletedProcess[str]":
    """Run ``body`` in a non-interactive ``<shell>`` that sources ``env_export``
    first, using a temp file to avoid quoting pitfalls.

    Args:
        env_export: Shell export lines (output of ``ocx env --shell=sh/bash``).
                    Written verbatim to a temp file and sourced with ``.``.
        body:       Shell script fragment to run after the env is applied.
                    Must be valid shell; the exit code of the last statement
                    determines ``CompletedProcess.returncode``.
        cwd:        Working directory for the subprocess.
        env:        Environment dict (typically ``dict(ocx.env)``).
        shell:      Shell binary name or path.  Default: ``"bash"``.
        shell_flags: Flags for the shell (space-separated string).
                     Default: ``"--norc"`` (non-interactive bash; remove or
                     replace for other shells).

    Returns:
        Completed subprocess with ``stdout``, ``stderr``, ``returncode``.

    Example::

        result = run_after_sourcing(
            env_result.stdout,
            "command -v mytool && mytool",
            cwd=tmp_path,
            env=dict(ocx.env),
        )
        assert result.returncode == 0
    """
    with tempfile.NamedTemporaryFile(
        mode="w",
        suffix=".sh",
        prefix="ocx_test_env_",
        delete=False,
    ) as f:
        f.write(env_export)
        env_file = f.name

    # Compose the script: source the env file, then run the body.
    # Using POSIX dot-operator (.) instead of ``eval`` avoids ALL quoting
    # issues: the export lines are interpreted as-is by the shell parser,
    # so paths with spaces, $, ", !, and \ are all handled correctly.
    script = f'. "{env_file}"\n{body}\n'
    flags = shell_flags.split() if shell_flags else []
    try:
        return subprocess.run(
            [shell, *flags, "-c", script],
            cwd=cwd,
            capture_output=True,
            text=True,
            env=env,
        )
    finally:
        import os
        try:
            os.unlink(env_file)
        except OSError:
            pass
