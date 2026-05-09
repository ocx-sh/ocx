"""Discover and execute every scenario script under `test/scenarios/`.

One pytest case per `.sh` file. The harness reads the `# scenario:` header,
instantiates the registered `Scenario` subclass (or a bare `Scenario` when
no header is present), runs `setup()`, then executes the script body.

A script returning non-zero fails its case. Use `[[ … ]] || exit 1` inside
the script for behavioural assertions — this file makes no other claims.
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

from src.helpers import PROJECT_ROOT
from src.runner import OcxRunner
from src.scenarios import Scenario, discover_scripts

# Shell scenarios target Linux + macOS. Windows behaviour is covered by
# the pytest acceptance suite (see .claude/rules/subsystem-tests.md
# "Platform Split").
pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="Shell scenarios target Linux/macOS; Windows behaviour covered by the pytest suite.",
)

SCENARIOS_DIR = PROJECT_ROOT / "test" / "scenarios"


def pytest_generate_tests(metafunc: pytest.Metafunc) -> None:
    if "scenario_script" in metafunc.fixturenames:
        scripts = discover_scripts(SCENARIOS_DIR)
        ids = [str(p.relative_to(SCENARIOS_DIR)) for p in scripts]
        metafunc.parametrize("scenario_script", scripts, ids=ids)


def test_scenario_script(
    scenario_script: Path,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    scenario = Scenario.auto_load(scenario_script, ocx, tmp_path)
    scenario.run_file(scenario_script)
