# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""``OCX_CONFIG`` as a list of config files, joined with the platform path separator.

A registry host named by ``[registry] default`` is the observable scalar: a failed
``package install`` of a bare name prints the registry it resolved, so the host that appears
is the file that won. Entries load in order (a later file wins), ``--config`` loads after the
list, empty entries are skipped, at most 32 are allowed and each must exist.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from src.otlp_fixture import OtlpCollector
from src.runner import OcxRunner

pytestmark = pytest.mark.command("install", "exec")

SEP = os.pathsep
EXIT_CONFIG = 78
EXIT_NOT_FOUND = 79
BARE = "nonexistent_config_list_pkg:0"


def _config(tmp_path: Path, name: str, host: str) -> Path:
    path = tmp_path / f"{name}.toml"
    path.write_text(f'[registry]\ndefault = "{host}"\n')
    return path


def _run(
    ocx: OcxRunner,
    *args: str,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    child_env = {k: v for k, v in ocx.env.items() if k != "OCX_DEFAULT_REGISTRY"}
    child_env.update(env or {})
    return subprocess.run(
        [str(ocx.binary), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=child_env,
        stdin=subprocess.DEVNULL,
        check=False,
        timeout=120,
    )


def _output(result: subprocess.CompletedProcess[str]) -> str:
    return result.stdout + result.stderr


def _install(
    ocx: OcxRunner, *flags: str, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return _run(ocx, *flags, "package", "install", BARE, env=env)


def test_later_entry_of_the_list_wins(ocx: OcxRunner, tmp_path: Path) -> None:
    first = _config(tmp_path, "one", "first-config.example")
    second = _config(tmp_path, "two", "second-config.example")

    control = _install(ocx, env={"OCX_CONFIG": str(first)})
    assert "first-config.example" in _output(control), _output(control)

    result = _install(ocx, env={"OCX_CONFIG": f"{first}{SEP}{second}"})

    assert "second-config.example" in _output(result), _output(result)
    assert "first-config.example" not in _output(result), _output(result)


def test_no_config_still_loads_the_list_and_ignores_the_user_tier(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    (ocx.ocx_home / "config.toml").write_text(
        '[registry]\ndefault = "home-tier.example"\n'
    )
    first = _config(tmp_path, "one", "first-config.example")
    second = _config(tmp_path, "two", "second-config.example")

    control = _install(ocx)
    assert "home-tier.example" in _output(control), _output(control)

    result = _install(
        ocx, env={"OCX_NO_CONFIG": "1", "OCX_CONFIG": f"{first}{SEP}{second}"}
    )

    assert "second-config.example" in _output(result), _output(result)
    assert "home-tier.example" not in _output(result), _output(result)


def test_list_entry_that_does_not_exist_exits_79_naming_it(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    first = _config(tmp_path, "one", "first-config.example")
    missing = tmp_path / "missing.toml"

    result = ocx.run(
        "package",
        "install",
        BARE,
        check=False,
        env_overlay={"OCX_CONFIG": f"{first}{SEP}{missing}"},
    )

    assert result.returncode == EXIT_NOT_FOUND, result.stderr
    error = json.loads(result.stdout)["error"]
    assert error["detail"] == "config_file_not_found"
    assert str(missing) in error["message"], error


def test_empty_entries_of_the_list_are_skipped(ocx: OcxRunner, tmp_path: Path) -> None:
    first = _config(tmp_path, "one", "first-config.example")

    result = _install(ocx, env={"OCX_CONFIG": f"{SEP}{first}{SEP}{SEP}"})

    assert "first-config.example" in _output(result), _output(result)
    assert result.returncode != EXIT_CONFIG, _output(result)


def test_list_past_the_cap_exits_78_config_list_too_long(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    files = [_config(tmp_path, f"c{i}", f"host{i}.example") for i in range(33)]

    at_cap = _install(ocx, env={"OCX_CONFIG": SEP.join(map(str, files[:32]))})
    assert "host31.example" in _output(at_cap), (
        f"control: 32 entries load; {_output(at_cap)}"
    )

    result = ocx.run(
        "package",
        "install",
        BARE,
        check=False,
        env_overlay={"OCX_CONFIG": SEP.join(map(str, files))},
    )

    assert result.returncode == EXIT_CONFIG, result.stderr
    assert json.loads(result.stdout)["error"]["detail"] == "config_list_too_long"


def test_config_flag_loads_after_the_list(ocx: OcxRunner, tmp_path: Path) -> None:
    first = _config(tmp_path, "one", "first-config.example")
    second = _config(tmp_path, "two", "second-config.example")
    third = _config(tmp_path, "three", "third-config.example")

    result = _install(
        ocx, "--config", str(third), env={"OCX_CONFIG": f"{first}{SEP}{second}"}
    )

    assert "third-config.example" in _output(result), _output(result)
    assert "second-config.example" not in _output(result), _output(result)


def test_telemetry_only_file_appended_to_a_single_config_keeps_both_effects(
    ocx: OcxRunner, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    user = _config(tmp_path, "user", "user-config.example")
    telemetry = tmp_path / "telemetry.toml"
    telemetry.write_text(
        f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n'
    )

    _install(ocx, env={"OCX_CONFIG": str(user)})
    assert otlp_collector.exports == [], "control: the user file alone arms nothing"

    result = _install(ocx, env={"OCX_CONFIG": f"{user}{SEP}{telemetry}"})

    assert "user-config.example" in _output(result), _output(result)
    assert len(otlp_collector.exports) == 1, otlp_collector.exports


def test_config_flag_path_containing_the_separator_is_refused_when_composing_a_child(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text("[tools]\n")
    ocx.run("--project", str(project), "lock", "--no-pull")
    awkward = tmp_path / f"awk{SEP}ward.toml"
    awkward.write_text('[registry]\ndefault = "awkward-config.example"\n')

    control = ocx.run(
        "--config",
        str(_config(tmp_path, "plain", "plain-config.example")),
        "--project",
        str(project),
        "exec",
        "--",
        str(ocx.binary),
        "about",
        check=False,
    )
    assert control.returncode == 0, _output(control)

    result = ocx.run(
        "--config",
        str(awkward),
        "--project",
        str(project),
        "exec",
        "--",
        str(ocx.binary),
        "about",
        check=False,
    )

    assert result.returncode == EXIT_CONFIG, _output(result)
    assert (
        json.loads(result.stdout)["error"]["detail"] == "config_path_contains_separator"
    )
