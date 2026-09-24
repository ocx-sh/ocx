"""The gate scripts' proofs, as pytest.

Each gate script under `scripts/` is stdlib-only and importable; its tests live
here as `test_<script>.py` and call the script's own proof functions. pytest is
the only entry point: discovery replaces the hand-kept list, `-n auto` the
sequential run, and `test_every_gate_script_has_tests` the completeness check.

The session is budgeted like the lint tier: a clean run above
`OCX_TOOLING_BUDGET_SECONDS` (default 20) wall clock turns red, so a proof that
starts re-deriving what one pass already computed cannot slow the loop unseen.
"""

from __future__ import annotations

import os
import shutil
import sys
import time
from pathlib import Path

import pytest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

BUDGET_ENV = "OCX_TOOLING_BUDGET_SECONDS"
DEFAULT_BUDGET_SECONDS = 20.0
_CLOCK = pytest.StashKey[tuple[float, float]]()


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
    budget = float(os.environ.get(BUDGET_ENV, DEFAULT_BUDGET_SECONDS))
    session.config.stash[_CLOCK] = (time.monotonic(), budget)


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    if hasattr(session.config, "workerinput"):
        # An xdist worker: only the controller's clock spans the whole run.
        return
    clock = session.config.stash.get(_CLOCK, None)
    if clock is None:
        return
    started, budget = clock
    elapsed = time.monotonic() - started
    over = elapsed > budget
    line = f"gate tooling: {elapsed:.1f} s wall clock, budget {budget:g} s ({BUDGET_ENV})"
    reporter = session.config.pluginmanager.get_plugin("terminalreporter")
    if reporter is not None:
        reporter.write_sep("-", line + (" — OVER BUDGET" if over else ""), red=over, bold=over)
    if over and session.exitstatus == pytest.ExitCode.OK:
        session.exitstatus = pytest.ExitCode.TESTS_FAILED
