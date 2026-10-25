#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Clear a previous acceptance run still holding this checkout's test binary.

    evict_stale_run.py <binary>

A run that was interrupted (a stopped `task verify`, a closed terminal, a
crashed orchestrator) leaves pytest workers alive. They keep executing
``<binary>`` -- which makes the rebuild fail with ETXTBSY -- and they keep
using the registry containers and the basetemp, so the next run reds for
reasons that have nothing to do with the tree. The next run should not need a
human with a kill command.

Processes are identified by ``/proc/<pid>/exe``, never by matching a command
line: a pattern broad enough to find them also matches the shell that is
looking, and the obvious filter for that suppresses the real hits too. Their
parents (the pytest controller, `uv`, `task`) go by process group, and SIGCONT
comes first because a *stopped* process never acts on a pending SIGTERM.
"""

from __future__ import annotations

import os
import signal
import sys
import time
from pathlib import Path


def holders(binary: Path) -> dict[int, int]:
    """``{pid: process group}`` for every live process executing ``binary``."""
    found: dict[int, int] = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        try:
            if (entry / "exe").resolve() != binary:
                continue
            found[pid] = os.getpgid(pid)
        except (OSError, PermissionError):
            continue
    return found


def evict(binary: Path, *, grace: float = 10.0) -> int:
    if not Path("/proc").is_dir():  # non-Linux: nothing to read, nothing claimed
        return 0
    binary = binary.resolve()
    initial = holders(binary)
    if not initial:
        return 0
    mine = os.getpgid(0)
    groups = {pgid for pgid in initial.values() if pgid != mine}
    print(
        f"evicting a previous acceptance run: {len(initial)} process(es) still executing "
        f"{binary} in {len(groups)} process group(s) — {sorted(initial)[:8]}",
        file=sys.stderr,
    )
    for sig in (signal.SIGCONT, signal.SIGTERM):
        for pgid in groups:
            try:
                os.killpg(pgid, sig)
            except OSError:
                pass
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        if not holders(binary):
            return 0
        time.sleep(0.25)
    for pgid in groups:
        try:
            os.killpg(pgid, signal.SIGKILL)
        except OSError:
            pass
    time.sleep(0.5)
    left = holders(binary)
    if left:
        print(f"evict_stale_run: {sorted(left)} survived SIGKILL", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: evict_stale_run.py <binary>")
    raise SystemExit(evict(Path(sys.argv[1])))
