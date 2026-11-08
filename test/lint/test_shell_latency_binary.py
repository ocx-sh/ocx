# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The `shell-latency` task measures an optimized binary.

The per-prompt latency budgets (a few milliseconds above the bare-exec floor) are set for the
release build. The Bazel `fastbuild` `ocx` that `test/bin/ocx` holds runs
three times slower and reds every budget at any load, so the task builds and
runs its own `-c opt` binary instead.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

import pytest
import yaml  # a suite dependency (test/pyproject.toml)

TASKFILE = Path(__file__).resolve().parents[1] / "taskfile.yml"
OPT_FLAG = re.compile(r"(?:^|\s)(?:-c|--compilation_mode(?:=|\s+))\s*opt(?:\s|$)")
BAZEL_VERB = re.compile(r"\bbazel\b.*?\b(?P<verb>build|cquery)\b")
DEFAULT_VAR = re.compile(r"default\s+\.(?P<name>\w+)")


def _commands(task: dict[str, Any]) -> list[str]:
    out: list[str] = []
    for cmd in task.get("cmds", []):
        if isinstance(cmd, str):
            out.append(cmd)
        elif isinstance(cmd, dict) and "cmd" in cmd:
            out.append(cmd["cmd"])
    return out


def unoptimized_reasons(document: dict[str, Any]) -> list[str]:
    """Why the latency task would run a non-`-c opt` binary; empty when it would not."""
    tasks = document["tasks"]
    latency = tasks["shell-latency"]
    chosen = DEFAULT_VAR.search(latency["vars"]["LATENCY_OCX_BINARY"])
    if chosen is None:
        return ["LATENCY_OCX_BINARY has no `default .<VAR>` fallback to inspect"]
    default_var = chosen.group("name")

    builders = [
        str(cmd["task"]).lstrip(":")
        for cmd in latency.get("cmds", [])
        if isinstance(cmd, dict) and "task" in cmd and str(cmd["task"]).lstrip(":").startswith(".build")
    ]
    if len(builders) != 1:
        return [f"expected exactly one `.build*` task call in shell-latency, found {builders}"]
    builder = tasks[builders[0]]

    reasons: list[str] = []
    # The fastbuild publish target is whatever `.build-binaries` writes; a latency task that
    # falls back to it measures the wrong binary even when it also builds an optimized one.
    fastbuild = set(re.findall(r"publish\s+//\S+\s+'\{\{\.(\w+)\}\}'", "\n".join(_commands(tasks[".build-binaries"]))))
    if default_var in fastbuild:
        reasons.append(f"LATENCY_OCX_BINARY defaults to {default_var}, which `.build-binaries` publishes from `fastbuild`")
    body = "\n".join(_commands(builder))
    if f"{{{{.{default_var}}}}}" not in body:
        reasons.append(f"{builders[0]} never writes {default_var}, the binary the latency task runs")
    bazel_lines = [line for line in body.splitlines() if BAZEL_VERB.search(line)]
    if not bazel_lines:
        reasons.append(f"{builders[0]} runs no bazel build or cquery")
    reasons.extend(f"{builders[0]}: `{line.strip()}` has no `-c opt`" for line in bazel_lines if not OPT_FLAG.search(line))
    return reasons


@pytest.fixture(scope="module")
def document() -> dict[str, Any]:
    return yaml.safe_load(TASKFILE.read_text(encoding="utf-8"))


def test_the_latency_task_runs_an_optimized_binary(document: dict[str, Any]) -> None:
    assert unoptimized_reasons(document) == []


def _taskfile(default: str, build: str) -> dict[str, Any]:
    return {
        "tasks": {
            ".build-binaries": {
                "cmds": [
                    {"cmd": "publish //crates/ocx_cli:ocx '{{.OCX_BINARY}}'"},
                ]
            },
            ".build-opt": {"cmds": [build]},
            "shell-latency": {
                "vars": {"LATENCY_OCX_BINARY": "{{.LATENCY_OCX_BINARY | default ." + default + "}}"},
                "cmds": [{"task": ".build-opt"}],
            },
        }
    }


BUILD_OPT = "ocx exec bazel -- bazel build -c opt //crates/ocx_cli:ocx && cp x '{{.OPT_OCX_BINARY}}'"
BUILD_FAST = "ocx exec bazel -- bazel build //crates/ocx_cli:ocx && cp x '{{.OPT_OCX_BINARY}}'"


def test_a_fastbuild_fallback_is_detected() -> None:
    reasons = unoptimized_reasons(_taskfile("OCX_BINARY", BUILD_OPT))
    assert any("fastbuild" in reason for reason in reasons)


def test_a_build_without_the_opt_flag_is_detected() -> None:
    reasons = unoptimized_reasons(_taskfile("OPT_OCX_BINARY", BUILD_FAST))
    assert any("no `-c opt`" in reason for reason in reasons)


def test_an_optimized_build_of_the_default_is_accepted() -> None:
    assert unoptimized_reasons(_taskfile("OPT_OCX_BINARY", BUILD_OPT)) == []
