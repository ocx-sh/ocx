"""`windows_local.sh`'s refusals, which need no Windows host: each is decided before any build.

The run itself (stage, zot, `task test:accept:native` through cmd.exe) is proven on the
Windows host by `task test:windows-local`, not here.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "windows_local.sh"


def _run(*args: str, **env: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(SCRIPT), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
        env={**os.environ, **env},
    )


@pytest.mark.parametrize(
    "args",
    [
        [],
        ["--mode", "bogus"],
        ["--mode"],
        ["--mode", "accept", "--nope"],
        ["--mode", "unit", "--exe", "x.exe"],
        ["--mode", "unit", "--shard-count", "2", "--shard-index", "0"],
        ["--mode", "unit", "--", "one", "two"],
    ],
    ids=["no-mode", "bad-mode", "mode-without-value", "unknown-flag", "unit-exe", "unit-shard", "unit-two-filters"],
)
def test_a_malformed_call_exits_64(args: list[str]) -> None:
    done = _run(*args)
    assert done.returncode == 64, done.stderr
    assert "windows_local:" in done.stderr


@pytest.mark.parametrize(
    "args",
    [
        ["--shard-count", "4", "--shard-index", "4"],
        ["--shard-count", "0", "--shard-index", "0"],
        ["--shard-count", "4"],
        ["--shard-index", "1"],
        ["--shard-count", "four", "--shard-index", "0"],
    ],
    ids=["index-eq-count", "count-0", "count-only", "index-only", "count-nan"],
)
def test_a_shard_the_selector_refuses_exits_2(args: list[str]) -> None:
    done = _run("--mode", "accept", *args)
    assert done.returncode == 2, done.stderr
    assert "accept shard:" in done.stderr


def test_a_missing_exe_exits_66(tmp_path: Path) -> None:
    done = _run("--mode", "accept", "--exe", str(tmp_path / "ocx.exe"))
    assert done.returncode == 66, done.stderr


def test_without_a_cmd_exe_it_exits_69_before_building_anything(tmp_path: Path) -> None:
    done = _run("--mode", "accept", WINDOWS_LOCAL_CMD_EXE=str(tmp_path / "no-cmd.exe"))
    assert done.returncode == 69, done.stderr
    assert "WSL" in done.stderr
