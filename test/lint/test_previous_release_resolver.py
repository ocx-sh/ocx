# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`src.previous_release.resolve()` finds the previous-release ocx or fails.

A test that needs the previous release must not turn into a skip when CI lacks
one: a skipped nesting proof and a passing one print the same green. So an
unresolvable binary is a failure, unless the leg says it has none
(`OCX_TEST_NO_PREVIOUS_OCX=1`), and then the skip names why.

The toolchain lookup is injected, so this tier never spawns `ocx`.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
from pathlib import Path

import pytest

from src import previous_release as pr


def _executable(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\n", encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def _nothing() -> Path | None:
    return None


def test_unset_everything_with_no_opt_out_fails() -> None:
    with pytest.raises(pytest.fail.Exception, match="OCX_TEST_PREVIOUS_OCX"):
        pr.resolve({}, toolchain=_nothing)


def test_the_env_path_wins_over_the_toolchain(tmp_path: Path) -> None:
    configured = _executable(tmp_path / "env" / "ocx")
    other = _executable(tmp_path / "toolchain" / "ocx")
    assert pr.resolve({pr.PATH_ENV: str(configured)}, toolchain=lambda: other) == configured


def test_the_toolchain_path_is_used_when_the_env_is_unset(tmp_path: Path) -> None:
    found = _executable(tmp_path / "toolchain" / "ocx")
    assert pr.resolve({}, toolchain=lambda: found) == found


def test_an_env_path_that_is_not_executable_fails_naming_it(tmp_path: Path) -> None:
    missing = tmp_path / "nowhere" / "ocx"
    with pytest.raises(pytest.fail.Exception, match="nowhere"):
        pr.resolve({pr.PATH_ENV: str(missing)}, toolchain=_nothing)


def test_the_opt_out_skips_and_names_the_reason() -> None:
    with pytest.raises(pytest.skip.Exception, match="OCX_TEST_NO_PREVIOUS_OCX=1"):
        pr.resolve({pr.OPT_OUT_ENV: "1"}, toolchain=_nothing)


def test_the_opt_out_does_not_hide_a_resolvable_binary(tmp_path: Path) -> None:
    found = _executable(tmp_path / "toolchain" / "ocx")
    assert pr.resolve({pr.OPT_OUT_ENV: "1"}, toolchain=lambda: found) == found


@pytest.mark.parametrize("value", ["", "0", "true"])
def test_only_the_value_one_opts_out(value: str) -> None:
    with pytest.raises(pytest.fail.Exception):
        pr.resolve({pr.OPT_OUT_ENV: value}, toolchain=_nothing)


def test_the_resolver_defaults_to_the_process_environment(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    configured = _executable(tmp_path / "ocx")
    monkeypatch.setenv(pr.PATH_ENV, str(configured))
    assert pr.resolve(toolchain=_nothing) == configured
    assert os.environ[pr.PATH_ENV] == str(configured)


def _fake_run(monkeypatch: pytest.MonkeyPatch, *, returncode: int = 0, stdout: str = "", stderr: str = "") -> dict:
    seen: dict = {}

    def run(argv: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
        seen.update(kwargs)
        return subprocess.CompletedProcess(argv, returncode, stdout, stderr)

    monkeypatch.setattr(pr.subprocess, "run", run)
    return seen


def test_a_lookup_failure_surfaces_the_stderr_tail(monkeypatch: pytest.MonkeyPatch) -> None:
    _fake_run(monkeypatch, returncode=1, stderr="noise\n" * 50 + "registry unreachable: sentinel-cause")
    with pytest.raises(pytest.fail.Exception, match="sentinel-cause"):
        pr.resolve({})


def test_a_non_json_report_is_reported_as_a_failure_naming_the_output(monkeypatch: pytest.MonkeyPatch) -> None:
    _fake_run(monkeypatch, stdout="warning: sentinel-banner\nnot json")
    with pytest.raises(pytest.fail.Exception, match="sentinel-banner"):
        pr.resolve({})


def test_the_lookup_has_a_timeout(monkeypatch: pytest.MonkeyPatch) -> None:
    seen = _fake_run(monkeypatch, returncode=1)
    with pytest.raises(pytest.fail.Exception):
        pr.resolve({})
    assert seen.get("timeout")


def test_a_lookup_timeout_is_reported_as_a_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    def run(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        raise subprocess.TimeoutExpired(argv, 1)

    monkeypatch.setattr(pr.subprocess, "run", run)
    with pytest.raises(pytest.fail.Exception, match="timed out"):
        pr.resolve({})


def test_the_current_env_report_schema_is_read(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    found = _executable(tmp_path / "bin" / "ocx")
    report = {"items": [{"kind": "path", "value": str(found.parent)}]}
    _fake_run(monkeypatch, stdout=json.dumps(report))
    assert pr.resolve({}) == found


def test_the_retired_env_report_schema_is_not_read(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    found = _executable(tmp_path / "bin" / "ocx")
    report = {"entries": [{"type": "path", "value": str(found.parent)}]}
    _fake_run(monkeypatch, stdout=json.dumps(report))
    with pytest.raises(pytest.fail.Exception):
        pr.resolve({})
