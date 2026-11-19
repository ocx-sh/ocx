# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`src.otelcol.resolve()` finds the OpenTelemetry collector or fails.

A telemetry export test must not turn into a skip when CI lacks the collector: a
skipped export proof and a passing one print the same green. So an unresolvable
binary is a failure, unless the leg says it has none (`OCX_TEST_NO_OTELCOL=1`),
and then the skip names why. The toolchain lookup is faked, so this tier never
spawns `ocx`.
"""

from __future__ import annotations

import json
import stat
import subprocess
from pathlib import Path

import pytest

from src import otelcol, previous_release


def _executable(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\n", encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def _nothing() -> Path | None:
    return None


def _fake_env_report(monkeypatch: pytest.MonkeyPatch, report: dict) -> None:
    def run(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(argv, 0, json.dumps(report), "")

    monkeypatch.setattr(previous_release.subprocess, "run", run)


def test_a_missing_tool_fails_naming_the_tool_the_pin_and_the_override() -> None:
    with pytest.raises(pytest.fail.Exception) as excinfo:
        otelcol.resolve({}, toolchain=_nothing)
    message = str(excinfo.value)
    assert "otelcol" in message
    assert "[group.test.tools]" in message
    assert "OCX_TEST_OTELCOL" in message


def test_a_toolchain_without_the_binary_fails(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _fake_env_report(monkeypatch, {"items": [{"kind": "path", "value": str(tmp_path)}]})
    with pytest.raises(pytest.fail.Exception, match="OCX_TEST_OTELCOL"):
        otelcol.resolve({})


def test_the_toolchain_binary_is_used_when_the_env_is_unset(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    found = _executable(tmp_path / "bin" / otelcol._NAME)
    _fake_env_report(
        monkeypatch, {"items": [{"kind": "path", "value": str(found.parent)}]}
    )
    assert otelcol.resolve({}) == found


def test_a_failed_toolchain_lookup_is_reported_in_the_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def run(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(
            argv, 1, "", "registry unreachable: sentinel-cause"
        )

    monkeypatch.setattr(previous_release.subprocess, "run", run)
    with pytest.raises(pytest.fail.Exception, match="sentinel-cause"):
        otelcol.resolve({})


def test_the_env_path_wins_over_the_toolchain(tmp_path: Path) -> None:
    configured = _executable(tmp_path / "env" / "otelcol")
    other = _executable(tmp_path / "toolchain" / "otelcol")
    assert (
        otelcol.resolve({otelcol.PATH_ENV: str(configured)}, toolchain=lambda: other)
        == configured
    )


def test_an_env_path_that_is_not_executable_fails_naming_it(tmp_path: Path) -> None:
    missing = tmp_path / "nowhere" / "otelcol"
    with pytest.raises(pytest.fail.Exception, match="nowhere"):
        otelcol.resolve({otelcol.PATH_ENV: str(missing)}, toolchain=_nothing)


def test_the_opt_out_skips_and_names_the_variable() -> None:
    with pytest.raises(pytest.skip.Exception, match="OCX_TEST_NO_OTELCOL=1"):
        otelcol.resolve({otelcol.OPT_OUT_ENV: "1"}, toolchain=_nothing)


def test_the_opt_out_does_not_hide_a_resolvable_binary(tmp_path: Path) -> None:
    found = _executable(tmp_path / "toolchain" / "otelcol")
    assert otelcol.resolve({otelcol.OPT_OUT_ENV: "1"}, toolchain=lambda: found) == found


@pytest.mark.parametrize("value", ["", "0", "true"])
def test_only_the_value_one_opts_out(value: str) -> None:
    with pytest.raises(pytest.fail.Exception):
        otelcol.resolve({otelcol.OPT_OUT_ENV: value}, toolchain=_nothing)
