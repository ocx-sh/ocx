# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""An ``ocx.toml`` cannot configure telemetry: a committed project file must not choose where usage data goes.

Armed by the environment to collector A, a project whose ``ocx.toml`` carries a ``[telemetry]``
section aimed at collector B is refused (exit 78); A still receives the failed command's record
and B receives nothing. Without the environment switch nothing is exported anywhere.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any

import pytest

from src.otlp_fixture import OtlpCollector, serve_collector
from src.runner import OcxRunner

pytestmark = pytest.mark.command("package_pull", "pull")

COMMAND = "sh.ocx.command"
EXIT_CONFIG = 78
SLUG = "telemetry_section_in_project"


def _project_with_telemetry(tmp_path: Path, endpoint: str) -> Path:
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\n\n[telemetry]\nenabled = true\nendpoint = "{endpoint}"\n'
    )
    return project


def _pull(
    ocx: OcxRunner, project: Path, env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return ocx.run("--project", str(project), "pull", check=False, env_overlay=env)


def _command_record(export: dict[str, Any]) -> dict[str, Any]:
    (record,) = [
        r["attributes"] for r in export["records"] if r["event_name"] == COMMAND
    ]
    return record


def test_telemetry_section_in_project_is_refused_and_reported_only_to_the_armed_collector(
    ocx: OcxRunner, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    with serve_collector("ok") as collector_b:
        project = _project_with_telemetry(tmp_path, collector_b.endpoint)

        result = _pull(
            ocx,
            project,
            {
                "OCX_TELEMETRY": "1",
                "OTEL_EXPORTER_OTLP_ENDPOINT": otlp_collector.endpoint,
            },
        )

        assert result.returncode == EXIT_CONFIG, result.stderr
        assert json.loads(result.stdout)["error"]["detail"] == SLUG
        assert len(otlp_collector.exports) == 1, otlp_collector.exports
        record = _command_record(otlp_collector.exports[0])
        assert record["process.exit.code"] == EXIT_CONFIG
        assert record["error.type"] == SLUG
        assert collector_b.exports == []


def test_telemetry_section_in_project_exports_nothing_when_not_armed(
    ocx: OcxRunner, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    # Positive control: the same project without the `[telemetry]` table, armed by
    # the environment, exports. The zero below therefore proves the table was the
    # cause, not an export path that never runs.
    plain = tmp_path / "plain"
    plain.mkdir()
    (plain / "ocx.toml").write_text("[tools]\n")
    ocx.run("--project", str(plain), "lock")
    control = _pull(
        ocx,
        plain,
        {"OCX_TELEMETRY": "1", "OTEL_EXPORTER_OTLP_ENDPOINT": otlp_collector.endpoint},
    )
    assert control.returncode == 0, control.stderr
    assert len(otlp_collector.exports) == 1, "control: the armed run must export"

    with serve_collector("ok") as collector_b:
        project = _project_with_telemetry(tmp_path, collector_b.endpoint)

        result = _pull(ocx, project)

        assert result.returncode == EXIT_CONFIG, result.stderr
        assert json.loads(result.stdout)["error"]["detail"] == SLUG
        assert len(otlp_collector.exports) == 1, "only the control's export"
        assert collector_b.exports == []
