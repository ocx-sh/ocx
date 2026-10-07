# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the `OCX_*` environment contract.

* A hardening switch set to a non-boolean refuses with exit 78 and names the
  variable on stderr, never echoing the value.
* A variable renamed inside a deprecation window still works under its old
  name and warns once on stderr; stdout stays the command's own output.

The one file that keeps old spellings on purpose: the positive control of
`test/lint/test_deprecated_spellings.py`.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

from src.runner import OcxRunner

pytestmark = pytest.mark.command("about", "shell_state", "self_group/activate")

HARDENING = ["OCX_FROZEN", "OCX_OFFLINE", "OCX_NO_CONSENT", "OCX_NO_VERIFY"]

# Not a secret: a value a typo could produce, which must never reach the output.
INVALID_VALUE = "perhaps-later"

OLD = "OCX_NO_COMPLETIONS"
NEW = "OCX_NO_COMPLETION"
NOTICE = f"{OLD} is renamed to {NEW}"

OLD_LOG = "OCX_LOG"
NEW_LOG = "OCX_LOG_LEVEL"
LOG_NOTICE = f"{OLD_LOG} is renamed to {NEW_LOG}"

DEBUG_LINE = re.compile(r"^\S+\s+DEBUG\s", re.MULTILINE)


def _run(
    ocx: OcxRunner, argv: list[str], env: dict[str, str] | None = None, format: str | None = None
) -> subprocess.CompletedProcess[str]:
    return ocx.run(*argv, format=format, check=False, env_overrides=env)


def _activate(ocx: OcxRunner, env: dict[str, str] | None = None) -> str:
    """`ocx self activate` for bash in a session declared interactive, so completions load by default."""
    result = _run(ocx, ["self", "activate", "--shell=bash", "--interactive"], env)
    assert result.returncode == 0, f"self activate failed (rc={result.returncode})\nstderr: {result.stderr}"
    return result.stdout


@pytest.mark.parametrize("key", HARDENING)
def test_an_invalid_hardening_value_exits_78_naming_the_key(ocx: OcxRunner, key: str) -> None:
    result = _run(ocx, ["about"], {key: INVALID_VALUE})

    assert result.returncode == 78, (
        f"an invalid {key} must refuse with exit 78, got {result.returncode}\nstderr: {result.stderr}"
    )
    assert key in result.stderr, f"stderr must name {key}; got:\n{result.stderr}"
    assert INVALID_VALUE not in result.stderr + result.stdout, "the invalid value was echoed"


@pytest.mark.skipif(sys.platform == "win32", reason="the bash completion block is POSIX activation output")
def test_the_renamed_completion_switch_works_under_both_names(ocx: OcxRunner) -> None:
    assert "complete -F" in _activate(ocx), "control: an interactive bash activation must load completions"
    assert "complete -F" not in _activate(ocx, {NEW: "1"}), f"{NEW}=1 must suppress completions"
    assert "complete -F" not in _activate(ocx, {OLD: "1"}), f"{OLD}=1 must still suppress completions"


def test_the_old_completion_switch_warns_once_on_stderr_only(ocx: OcxRunner) -> None:
    result = _run(ocx, ["about"], {OLD: "1"}, format="json")

    assert result.returncode == 0, f"about failed (rc={result.returncode})\nstderr: {result.stderr}"
    json.loads(result.stdout)
    assert OLD not in result.stdout, "the rename notice reached stdout"
    assert result.stderr.count(NOTICE) == 1, f"expected one notice naming both spellings; stderr:\n{result.stderr}"


def test_the_new_completion_switch_warns_nothing(ocx: OcxRunner) -> None:
    result = _run(ocx, ["about"], {NEW: "1"}, format="json")

    assert result.returncode == 0, f"about failed (rc={result.returncode})\nstderr: {result.stderr}"
    assert "is renamed to" not in result.stderr, f"the current spelling warned; stderr:\n{result.stderr}"


@pytest.mark.parametrize("key", [NEW_LOG, OLD_LOG])
def test_the_log_level_variable_sets_the_level_under_both_names(ocx: OcxRunner, key: str) -> None:
    quiet = _run(ocx, ["about"])
    assert not DEBUG_LINE.search(quiet.stderr), f"control: the default level rendered DEBUG; stderr:\n{quiet.stderr}"

    loud = _run(ocx, ["about"], {key: "debug"})

    assert loud.returncode == 0, f"about failed (rc={loud.returncode})\nstderr: {loud.stderr}"
    assert DEBUG_LINE.search(loud.stderr), f"{key}=debug rendered no DEBUG line; stderr:\n{loud.stderr}"


def test_the_old_log_level_variable_warns_once_on_stderr_only(ocx: OcxRunner) -> None:
    result = _run(ocx, ["about"], {OLD_LOG: "warn"}, format="json")

    assert result.returncode == 0, f"about failed (rc={result.returncode})\nstderr: {result.stderr}"
    json.loads(result.stdout)
    assert OLD_LOG not in result.stdout, "the rename notice reached stdout"
    assert result.stderr.count(LOG_NOTICE) == 1, f"expected one notice naming both spellings; stderr:\n{result.stderr}"


def test_the_new_log_level_variable_warns_nothing(ocx: OcxRunner) -> None:
    result = _run(ocx, ["about"], {NEW_LOG: "warn"}, format="json")

    assert result.returncode == 0, f"about failed (rc={result.returncode})\nstderr: {result.stderr}"
    assert "is renamed to" not in result.stderr, f"the current spelling warned; stderr:\n{result.stderr}"


def test_a_relative_home_is_reported_absolute(ocx: OcxRunner, tmp_path: Path) -> None:
    """The reports type these paths as absolute, so a relative `OCX_HOME` is resolved, not echoed."""
    env = {**ocx.env, "OCX_HOME": "relative-home"}
    expected = tmp_path / "relative-home"

    def report(*argv: str) -> dict[str, object]:
        # In `tmp_path`, so the relative home resolves somewhere disposable.
        result = subprocess.run(
            [str(ocx.binary), "--format", "json", *argv],
            capture_output=True,
            text=True,
            env=env,
            cwd=tmp_path,
            stdin=subprocess.DEVNULL,
            check=False,
        )
        assert result.returncode == 0, f"{argv} failed (rc={result.returncode})\nstderr: {result.stderr}"
        return json.loads(result.stdout)

    assert Path(str(report("about")["home"])) == expected
    assert Path(str(report("--global", "shell", "state")["toolchain_home"])) == expected / "toolchain"
