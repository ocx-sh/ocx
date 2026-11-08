# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""A tool started through `ocx exec <tools> -- <tool>` is one of the tools it names.

`ocx exec bazel -- python3 …` puts only bazel on `PATH`, so `python3` resolves to
whatever the host ships (3.12 on ubuntu-latest). The repo's floor is 3.14, and a
script that parses repo sources (the Bazel tag guard reads every acceptance module)
then rejects 3.14-only syntax that ruff itself writes. Naming `python` among the
exec'd tools pins the interpreter. `uv` is the same case: the installed ocx
drops the hook-activated toolchain from `PATH`, so an unnamed `uv` does not
resolve at all.
"""

from __future__ import annotations

import re
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
TASKFILES = sorted([REPO / "taskfile.yml", *(REPO / "taskfiles").glob("*.yml")])
EXEC_TOOL = re.compile(
    r"\bocx exec (?P<tools>(?:[^\s-][^\s]*\s+)*)--\s+(?P<command>python3?|uv)\b"
)
#: The toolchain entry each command resolves from.
TOOL_OF = {"python": "python", "python3": "python", "uv": "uv"}


def _exec_tool_lines() -> list[tuple[str, int, str, str]]:
    hits = []
    for path in TASKFILES:
        for number, line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), start=1
        ):
            for found in EXEC_TOOL.finditer(line):
                hits.append(
                    (
                        path.relative_to(REPO).as_posix(),
                        number,
                        found.group("tools"),
                        found.group("command"),
                    )
                )
    return hits


def test_reader_finds_exec_tool_invocations() -> None:
    assert TASKFILES, "no taskfile found: the scan read nothing"
    assert _exec_tool_lines(), (
        "no `ocx exec … -- python3|uv` line found: the scan read nothing"
    )


def test_every_exec_tool_names_its_toolchain_entry() -> None:
    unpinned = [
        f"{path}:{number} ({command})"
        for path, number, tools, command in _exec_tool_lines()
        if TOOL_OF[command] not in tools.split()
    ]
    assert not unpinned, (
        f"`ocx exec` runs a tool it does not name (host or unresolvable binary): {unpinned}"
    )
