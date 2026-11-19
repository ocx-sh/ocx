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

import os
import subprocess
import sys
import tempfile
from pathlib import Path

from src.shell_matrix import quote, shell_which


def run_after_sourcing(
    env_export: str,
    body: str,
    cwd: Path,
    env: dict[str, str],
    shell: str = "bash",
    shell_flags: str = "--norc",
) -> subprocess.CompletedProcess[str]:
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
    # Forward slashes: a slash-free name makes `.` search PATH before the file itself.
    sourced = Path(env_file).as_posix() if sys.platform == "win32" else env_file
    script = f'. "{sourced}"\n{body}\n'
    flags = shell_flags.split() if shell_flags else []
    if sys.platform == "win32" and not Path(shell).is_absolute():
        shell = shell_which(shell, env.get("PATH")) or shell
    try:
        return subprocess.run(
            [shell, *flags, "-c", script],
            cwd=cwd,
            capture_output=True,
            text=True,
            env=env, check=False,
        )
    finally:
        try:
            os.unlink(env_file)
        except OSError:
            pass


# Bash helpers for asserting where a directory sits on ``$PATH`` as bash reports it.
# ``_u`` turns a native path into the form bash holds (``cygpath -u`` under Git Bash,
# the identity elsewhere); ``_phys`` resolves symlinks; ``_has_dir`` is true when a
# ``$PATH`` segment is the same directory. The conversion runs inside the session so
# no Python mirror of it can agree with the code under test by construction.
PATH_HELPERS = r"""
_u() { if command -v cygpath >/dev/null 2>&1; then cygpath -u "$1"; else printf '%s' "$1"; fi; }
_phys() { (cd "$1" 2>/dev/null && pwd -P); }
_has_dir() {
    local want seg
    want="$(_phys "$(_u "$1")")"
    [ -n "$want" ] || return 1
    local IFS=:
    for seg in $PATH; do
        [ -n "$seg" ] && [ "$(_phys "$seg")" = "$want" ] && return 0
    done
    return 1
}
"""


def require_dirs_on_path(dirs: list[str]) -> str:
    """Script lines that exit 1 naming the first of ``dirs`` missing from ``$PATH``."""
    return "\n".join(
        f"_has_dir {quote("bash", d)} || {{ echo \"NOT_ON_PATH: \"{quote("bash", d)} >&2; exit 1; }}" for d in dirs
    )


def git_bash_env(env: dict[str, str], bash: str) -> dict[str, str]:
    """``env`` with Git for Windows' ``usr/bin`` (``cygpath``, coreutils) on ``PATH``; unchanged elsewhere."""
    if sys.platform != "win32":
        return env
    root = Path(bash).resolve().parent
    candidates = [root, root.parent / "usr" / "bin", root / "usr" / "bin"]
    extra = [str(c) for c in candidates if (c / "cygpath.exe").is_file()]
    return {**env, "PATH": os.pathsep.join([*extra, env.get("PATH", "")])}
