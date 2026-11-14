"""The gate scripts' proofs, as pytest.

Each stdlib-only gate script under `scripts/` has its tests here as
`test_<script>.py`, calling the script's own proof functions. Run `task scripts:self-test`
(a bare `uv run pytest` has no pytest): discovery replaces the hand-kept list and
`test_every_gate_script_has_tests` is the completeness check.

The session is budgeted in CPU seconds: a clean run whose controller plus xdist
workers spend more than `OCX_TOOLING_BUDGET_SECONDS` turns red. Wall time only
backs it up at 5x: a loaded host stretches the clock, not the cost.
"""

from __future__ import annotations

import os
import resource
import shutil
import sys
import time
from pathlib import Path

import pytest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

BUDGET_ENV = "OCX_TOOLING_BUDGET_SECONDS"
# Three `-n auto` runs spent 53.8, 52.3 and 52.1 CPU seconds (user+sys over
# all workers); 1.5x the largest, rounded up to 10.
DEFAULT_BUDGET_SECONDS = 90.0
# Hosted runners get twice that, for a cold Bazel server.
HOSTED_RUNNER_BUDGET_SECONDS = 180.0
WALL_BACKSTOP_FACTOR = 5
_CLOCK = pytest.StashKey[tuple[float, float, float]]()
_WORKER_CPU = pytest.StashKey[list[float]]()


def _cpu_seconds() -> float:
    """User plus system CPU seconds this process has spent. A seam for the budget tests."""
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return usage.ru_utime + usage.ru_stime


def _wall_seconds() -> float:
    """Monotonic wall clock. A seam for the budget tests."""
    return time.monotonic()


@pytest.fixture(scope="session")
def bazel() -> str:
    """The `bazel` on PATH — `ocx exec bazel --` puts it there. Absent is a red,
    never a skip: the proofs that need it read the live graph, and a skipped one
    is the vacuous green they exist to prevent."""
    found = shutil.which("bazel")
    if found is None:
        pytest.fail("no `bazel` on PATH — run through `task scripts:self-test` (it wraps `ocx exec bazel prek --`)")
    return found


def pytest_sessionstart(session: pytest.Session) -> None:
    hosted = os.environ.get("GITHUB_ACTIONS") == "true"
    default = HOSTED_RUNNER_BUDGET_SECONDS if hosted else DEFAULT_BUDGET_SECONDS
    budget = float(os.environ.get(BUDGET_ENV, default))
    session.config.stash[_CLOCK] = (_wall_seconds(), _cpu_seconds(), budget)
    session.config.stash[_WORKER_CPU] = []


@pytest.hookimpl(optionalhook=True)
def pytest_testnodedown(node, error) -> None:
    """Collect an xdist worker's CPU seconds (controller side)."""
    reported = getattr(node, "workeroutput", {}).get("cpu_seconds")
    sink = node.config.stash.get(_WORKER_CPU, None)
    if reported is not None and sink is not None:
        sink.append(float(reported))


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    clock = session.config.stash.get(_CLOCK, None)
    if clock is None:
        return
    started_wall, started_cpu, budget = clock
    own_cpu = _cpu_seconds() - started_cpu
    if hasattr(session.config, "workeroutput"):
        # An xdist worker: report to the controller, which judges the sum.
        session.config.workeroutput["cpu_seconds"] = own_cpu
        return
    cpu = own_cpu + sum(session.config.stash.get(_WORKER_CPU, []))
    wall = _wall_seconds() - started_wall
    backstop = budget * WALL_BACKSTOP_FACTOR
    over_cpu = cpu > budget
    over_wall = wall > backstop
    line = (
        f"gate tooling: {cpu:.1f} CPU s, budget {budget:g} s ({BUDGET_ENV}); "
        f"wall {wall:.1f} s, backstop {backstop:g} s"
    )
    if over_cpu:
        line += " — CPU OVER BUDGET"
    if over_wall:
        line += " — wall OVER BUDGET"
    reporter = session.config.pluginmanager.get_plugin("terminalreporter")
    if reporter is not None:
        reporter.write_sep("-", line, red=over_cpu or over_wall, bold=over_cpu or over_wall)
    if (over_cpu or over_wall) and session.exitstatus == pytest.ExitCode.OK:
        session.exitstatus = pytest.ExitCode.TESTS_FAILED
