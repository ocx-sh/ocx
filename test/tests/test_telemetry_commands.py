# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the commands that never send telemetry.

Specs: system_design_otel_telemetry.md § 6 (the never-send command set). Every case runs the command armed with a capture endpoint, then
runs `ocx package install` on the same fixture and must see exactly one
request, so a capture server that never listened cannot make the negative half
pass.
"""

from __future__ import annotations

import json
import subprocess
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

import pytest

from src.helpers import make_package
from src.otlp_fixture import OtlpCollector
from src.runner import OcxRunner, PackageInfo

pytestmark = pytest.mark.command("install", "exec", "status")

EXIT_USAGE = 64


@dataclass
class World:
    """A published package, a locked and pulled project that uses it, and a config candidate."""

    pkg: PackageInfo
    project: Path
    candidate: Path


def _in(
    ocx: OcxRunner, cwd: Path, env: dict[str, str], *args: str
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **env},
        stdin=subprocess.DEVNULL,
        timeout=120,
        check=False,
    )


@pytest.fixture()
def world(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> World:
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)
    project = tmp_path / "project"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\nhello = "{pkg.fq}"\n', encoding="utf-8"
    )
    for step in ("lock", "pull"):
        done = _in(ocx, project, {}, step)
        assert done.returncode == 0, f"ocx {step}: {done.stderr}"
    candidate = tmp_path / "candidate.toml"
    candidate.write_text('[registry]\ndefault = "example.test"\n', encoding="utf-8")
    return World(pkg, project, candidate)


# Each row maps the fixture to an argv and the one exit code the command must end with;
# `--version` is not a flag of this CLI (`ocx version` is the command), so it ends as a usage error.
COMMANDS: list[tuple[str, Callable[[World], list[str]], int]] = [
    ("exec", lambda w: ["exec", "--", "hello"], 0),
    ("package-exec", lambda w: ["package", "exec", w.pkg.short, "--", "hello"], 0),
    (
        "package-exec-rm",
        lambda w: ["package", "exec", "--rm", w.pkg.short, "--", "hello"],
        0,
    ),
    ("shell-state", lambda w: ["shell", "state"], 0),
    ("direnv-export", lambda w: ["direnv", "export"], 0),
    ("help", lambda w: ["--help"], 0),
    ("package-help", lambda w: ["package", "--help"], 0),
    ("version-flag", lambda w: ["--version"], EXIT_USAGE),
    ("version", lambda w: ["version"], 0),
    ("usage-error", lambda w: ["package", "install", "--no-such-flag"], EXIT_USAGE),
    ("self-activate", lambda w: ["self", "activate", "--shell=bash"], 0),
    ("shell-completion", lambda w: ["shell", "completion", "--shell", "bash"], 0),
    ("env-shell", lambda w: ["env", "--shell=bash"], 0),
    ("package-env-shell", lambda w: ["package", "env", "--shell=bash", w.pkg.short], 0),
    ("status", lambda w: ["status"], 0),
    ("config-test", lambda w: ["config", "test", str(w.candidate)], 0),
]


@pytest.mark.parametrize(
    ("build", "expected"),
    [pytest.param(b, e, id=name) for name, b, e in COMMANDS],
)
def test_never_send_commands_send_nothing(
    ocx: OcxRunner,
    world: World,
    otlp_collector: OtlpCollector,
    build: Callable[[World], list[str]],
    expected: int,
) -> None:
    """an armed run of a never-send command sends nothing and writes no telemetry state."""
    armed = {
        "OCX_TELEMETRY": "1",
        "OTEL_EXPORTER_OTLP_ENDPOINT": otlp_collector.endpoint,
    }

    result = _in(ocx, world.project, armed, *build(world))

    assert result.returncode == expected, result.stderr
    assert otlp_collector.requests == []
    assert not (ocx.ocx_home / "state" / "telemetry").exists()

    control = _in(ocx, world.project, armed, "package", "install", world.pkg.short)
    assert control.returncode == 0, control.stderr
    assert len(otlp_collector.log_posts) == 1, (
        "control: an install on the same fixture must send"
    )
    assert (ocx.ocx_home / "state" / "telemetry").is_dir()


def test_a_failure_after_arming_still_exports_the_failed_command(
    ocx: OcxRunner, world: World, otlp_collector: OtlpCollector
) -> None:
    """`OCX_FROZEN` with `OCX_REMOTE` is refused late in start-up, after the export is armed."""
    env = {
        "OCX_TELEMETRY": "1",
        "OTEL_EXPORTER_OTLP_ENDPOINT": otlp_collector.endpoint,
        "OCX_FROZEN": "1",
        "OCX_REMOTE": "1",
    }

    result = _in(
        ocx, world.project, env, "--format", "json", "package", "install", world.pkg.short
    )

    assert result.returncode == EXIT_USAGE, result.stderr
    slug = json.loads(result.stdout)["error"]["detail"]
    assert len(otlp_collector.requests) == 1, otlp_collector.requests
    (record,) = [
        r["attributes"]
        for r in otlp_collector.exports[0]["records"]
        if r["event_name"] == "sh.ocx.command"
    ]
    assert record["process.exit.code"] == EXIT_USAGE
    assert record["error.type"] == slug
