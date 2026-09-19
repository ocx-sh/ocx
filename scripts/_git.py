# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The one `git` wrapper the gate scripts share (scoped_gate.py, test_diff_guard.py).

A failing git command is a loud SystemExit naming the command, never an empty
string a caller could read as "nothing changed". Output is decoded as UTF-8
explicitly: `text=True` alone rides the locale (PY-PROC-01).
"""

from __future__ import annotations

import subprocess
from pathlib import Path


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    if result.returncode != 0:
        raise SystemExit(f"git {' '.join(args)} failed (rc={result.returncode}):\n{result.stderr}")
    return result.stdout
