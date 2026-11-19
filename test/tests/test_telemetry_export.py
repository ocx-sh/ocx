# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for usage-telemetry arming and the records one command sends.

Specs: system_design_otel_telemetry.md § 3.1, § 3.2, § 4. Every "sends nothing" test pairs with a control on
the same fixture that arms the run and must then see exactly one request, so a
capture server that never listened cannot make the negative half pass.
"""

from __future__ import annotations

import os
import re
import stat
import subprocess
import sys
from pathlib import Path

import pytest

from src.helpers import make_package
from src.otlp_fixture import LOGS_PATH, OtlpCollector
from src.runner import OcxRunner, PackageInfo
from src.telemetry_helpers import armed

pytestmark = pytest.mark.command("install", "package_pull", "uninstall")

UUID_V4 = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
)
COMMAND_EVENT = "sh.ocx.command"
PACKAGE_EVENT = "sh.ocx.package"


def _telemetry_dir(ocx: OcxRunner) -> Path:
    return ocx.ocx_home / "state" / "telemetry"


def _ambient_otel(collector: OtlpCollector) -> dict[str, str]:
    return {
        "OTEL_EXPORTER_OTLP_ENDPOINT": collector.endpoint,
        "OTEL_EXPORTER_OTLP_HEADERS": "x-ambient=a1",
        "OTEL_RESOURCE_ATTRIBUTES": "team=ambient",
        "OTEL_SERVICE_NAME": "ambient-service",
    }


def _events(export: dict, event_name: str) -> list[dict]:
    return [r for r in export["records"] if r["event_name"] == event_name]


def _install(
    ocx: OcxRunner, pkg: PackageInfo, env: dict[str, str], **kwargs
) -> subprocess.CompletedProcess[str]:
    return ocx.run("package", "install", pkg.short, env_overrides=env, **kwargs)


def test_unarmed_sends_nothing_with_ambient_otel(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """OTEL_* alone never sends, writes no state and logs no payload; the switch flips it."""
    ambient = _ambient_otel(otlp_collector)

    unarmed = _install(ocx, published_package, ambient, log_level="debug")
    assert otlp_collector.requests == [], (
        "ambient OTEL_* variables must not arm telemetry"
    )
    assert not _telemetry_dir(ocx).exists()
    assert "telemetry payload" not in unarmed.stderr

    # Control: the same fixture, now armed, must reach the capture server.
    _install(ocx, published_package, {**ambient, "OCX_TELEMETRY": "1"})
    assert len(otlp_collector.requests) == 1, (
        "armed control saw no request: the capture server is not wired"
    )
    assert len(otlp_collector.log_posts) == 1


@pytest.mark.parametrize(
    "variable", ["OTEL_EXPORTER_OTLP_ENDPOINT", "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"]
)
def test_otel_endpoint_alone_never_arms(
    ocx: OcxRunner,
    published_package: PackageInfo,
    otlp_collector: OtlpCollector,
    variable: str,
) -> None:
    """neither the generic nor the logs-specific endpoint arms; the switch does."""
    value = (
        otlp_collector.url(LOGS_PATH)
        if variable.endswith("LOGS_ENDPOINT")
        else otlp_collector.endpoint
    )
    env = {variable: value}

    _install(ocx, published_package, env)
    assert otlp_collector.requests == []

    _install(ocx, published_package, {**env, "OCX_TELEMETRY": "1"})
    assert len(otlp_collector.log_posts) == 1, "control: switch plus endpoint must send"


def test_switch_without_endpoint_sends_nothing(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """switch on with no destination sends nothing, writes no id, says why."""
    result = _install(ocx, published_package, {"OCX_TELEMETRY": "1"}, log_level="debug")
    assert otlp_collector.requests == []
    assert not _telemetry_dir(ocx).exists()
    assert "telemetry: no destination" in result.stderr

    control = _install(
        ocx,
        published_package,
        {"OCX_TELEMETRY": "1", "OTEL_EXPORTER_OTLP_ENDPOINT": otlp_collector.endpoint},
        log_level="debug",
    )
    assert len(otlp_collector.log_posts) == 1, "control: adding the endpoint must send"
    assert "telemetry: no destination" not in control.stderr


def test_config_arms_alone(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """`[telemetry] enabled = true` plus an endpoint in the user config sends with no env switch."""
    (ocx.ocx_home / "config.toml").write_text(
        f'[telemetry]\nenabled = true\nendpoint = "{otlp_collector.endpoint}"\n',
        encoding="utf-8",
    )
    _install(ocx, published_package, {})
    assert len(otlp_collector.log_posts) == 1
    assert len(_events(otlp_collector.exports[0], COMMAND_EVENT)) == 1


def test_one_post_per_armed_command(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """three send-set commands produce three POSTs, one matching command record each."""
    env = {"OCX_TELEMETRY": "1", "OTEL_EXPORTER_OTLP_ENDPOINT": otlp_collector.endpoint}
    pkg = published_package
    ocx.run("package", "install", pkg.short, env_overrides=env)
    ocx.run("package", "pull", pkg.short, env_overrides=env)
    ocx.run("package", "uninstall", pkg.short, env_overrides=env)

    assert len(otlp_collector.requests) == 3
    assert len(otlp_collector.requests_to(LOGS_PATH)) == 3
    names = []
    for export in otlp_collector.exports:
        commands = _events(export, COMMAND_EVENT)
        assert len(commands) == 1
        names.append(commands[0]["attributes"]["sh.ocx.command.name"])
    assert names == ["package install", "package pull", "package uninstall"]


def test_one_post_carries_command_and_package_records(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    otlp_collector: OtlpCollector,
) -> None:
    """installing two packages sends one request with one command and 2+ package records."""
    first = make_package(ocx, f"{unique_repo}_a", "1.0.0", tmp_path)
    second = make_package(ocx, f"{unique_repo}_b", "1.0.0", tmp_path)
    env = {"OCX_TELEMETRY": "1", "OTEL_EXPORTER_OTLP_ENDPOINT": otlp_collector.endpoint}

    ocx.run("package", "install", first.short, second.short, env_overrides=env)

    assert len(otlp_collector.log_posts) == 1
    export = otlp_collector.exports[0]
    assert len(_events(export, COMMAND_EVENT)) == 1
    assert len(_events(export, PACKAGE_EVENT)) >= 2


# --- installation id -------------------------------------------------


def test_installation_id_is_created_and_stable(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """the first armed run writes a v4 id that is also on the wire; later runs reuse it."""
    id_file = _telemetry_dir(ocx) / "installation-id"
    assert not id_file.exists()

    _install(ocx, published_package, armed(otlp_collector))
    assert id_file.is_file()
    stored = id_file.read_text(encoding="utf-8").strip()
    assert UUID_V4.match(stored), f"not a lowercase v4 UUID: {stored!r}"
    if sys.platform != "win32":
        assert stat.S_IMODE(id_file.stat().st_mode) == 0o600
    first = otlp_collector.exports[0]["resource"]["sh.ocx.installation.id"]
    assert first == stored

    _install(ocx, published_package, armed(otlp_collector))
    assert len(otlp_collector.exports) == 2
    assert otlp_collector.exports[1]["resource"]["sh.ocx.installation.id"] == stored
    assert id_file.read_text(encoding="utf-8").strip() == stored


def test_installation_id_not_written_when_unarmed_or_without_destination(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """no destination, no state."""
    _install(
        ocx, published_package, {"OTEL_EXPORTER_OTLP_ENDPOINT": otlp_collector.endpoint}
    )
    assert not _telemetry_dir(ocx).exists(), "unarmed run wrote telemetry state"

    _install(ocx, published_package, {"OCX_TELEMETRY": "1"})
    assert not _telemetry_dir(ocx).exists(), (
        "armed run without destination wrote telemetry state"
    )
    assert otlp_collector.requests == []

    _install(ocx, published_package, armed(otlp_collector))
    assert (_telemetry_dir(ocx) / "installation-id").is_file(), (
        "control: armed run with destination writes the id"
    )


def test_garbage_installation_id_is_omitted_and_left_untouched(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """an unusable id file is never repaired; the attribute is absent and exit stays 0."""
    id_file = _telemetry_dir(ocx) / "installation-id"
    id_file.parent.mkdir(parents=True)
    garbage = "not-a-uuid\n"
    id_file.write_text(garbage, encoding="utf-8")

    result = _install(ocx, published_package, armed(otlp_collector))
    assert result.returncode == 0
    assert len(otlp_collector.exports) == 1
    assert "sh.ocx.installation.id" not in otlp_collector.exports[0]["resource"]
    assert id_file.read_text(encoding="utf-8") == garbage


@pytest.mark.skipif(
    sys.platform == "win32" or (hasattr(os, "geteuid") and os.geteuid() == 0),
    reason="POSIX permission bits do not bind root and do not exist on Windows",
)
def test_read_only_state_dir_omits_the_id_and_keeps_exit_zero(
    ocx: OcxRunner, published_package: PackageInfo, otlp_collector: OtlpCollector
) -> None:
    """a read-only `state` directory costs the attribute, never the command."""
    state = ocx.ocx_home / "state"
    state.mkdir(exist_ok=True)
    state.chmod(0o555)
    try:
        # Control for the premise: the directory really refuses a write.
        with pytest.raises(PermissionError):
            (state / "probe").write_text("x", encoding="utf-8")

        result = _install(ocx, published_package, armed(otlp_collector))
    finally:
        state.chmod(0o755)
    assert result.returncode == 0
    assert len(otlp_collector.exports) == 1
    assert "sh.ocx.installation.id" not in otlp_collector.exports[0]["resource"]
