# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""An upstream OpenTelemetry collector accepts what an armed ocx exports.

The other telemetry modules read a capture server that decodes whatever arrives. This one
sends to the pinned core `otelcol` (`src.otelcol`) and reads back what its OTLP/HTTP
receiver handed to the `file` exporter. A missing collector fails the test, never skips it,
unless the leg declares `OCX_TEST_NO_OTELCOL=1`.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from src.otelcol import run_collector
from src.otlp_fixture import flatten
from src.runner import OcxRunner, PackageInfo

pytestmark = pytest.mark.command("install")

COMMAND = "sh.ocx.command"
PACKAGE = "sh.ocx.package"


def test_real_collector_receives_the_command_record(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """an armed `package install` reaches the collector's pipeline with its command and package records."""
    with run_collector(tmp_path) as collector:
        ocx.run(
            "package",
            "install",
            published_package.short,
            env_overrides={
                "OCX_TELEMETRY": "1",
                "OTEL_EXPORTER_OTLP_ENDPOINT": collector.endpoint,
            },
        )
        exports = [flatten(request) for request in collector.read_exports()]

    records = [record for export in exports for record in export["records"]]
    names = [record["event_name"] for record in records]
    commands = [r["attributes"] for r in records if r["event_name"] == COMMAND]
    packages = [r["attributes"] for r in records if r["event_name"] == PACKAGE]
    assert len(commands) == 1, f"expected one {COMMAND} record, got event names {names}"
    assert commands[0]["sh.ocx.command.name"] == "package install", commands[0]
    assert commands[0]["process.exit.code"] == 0, commands[0]
    assert len(packages) == 1, f"expected one {PACKAGE} record, got event names {names}"
    assert packages[0]["sh.ocx.package.action"] == "installed", packages[0]
