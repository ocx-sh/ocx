"""pytest fixtures for bench smoke validation only.

This conftest.py provides SMOKE-VALIDATION FIXTURES ONLY — it does NOT run
the full benchmark matrix from pytest. The full benchmark is driven by
`task bench` (standalone script via uv run python bench/harness.py).

Pytest smoke tests in test/tests/test_bench_smoke.py use these fixtures to:
  - Verify the scenario matrix has exactly 14 rows.
  - Verify compare_against_baseline is a pure function (no I/O).
  - Verify ScenarioResult and CompareReport dataclasses have expected fields.
  - Skip full harness tests unless BENCH_SMOKE_LIVE=1 env var is set.

No Docker, no toxiproxy, no hyperfine required for the smoke tests.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

# Bootstrap sys.path so bench.* imports work when pytest is invoked from test/.
_TEST_DIR = Path(__file__).resolve().parent.parent
if str(_TEST_DIR) not in sys.path:
    sys.path.insert(0, str(_TEST_DIR))


@pytest.fixture(scope="session")
def bench_scenarios():
    """Return the SCENARIOS list from bench.scenarios."""
    from bench.scenarios import SCENARIOS  # noqa: PLC0415

    return SCENARIOS


@pytest.fixture(scope="session")
def bench_compare_fn():
    """Return the compare_against_baseline pure function."""
    from bench.compare import compare_against_baseline  # noqa: PLC0415

    return compare_against_baseline


@pytest.fixture()
def sample_baseline_json() -> dict:
    """A minimal hyperfine-compatible baseline JSON for unit testing compare.py."""
    return {
        "results": [
            {
                "command": "ocx_install_10mbps",
                "mean": 45.0,
                "stddev": 1.2,
                "median": 45.0,
                "min": 43.0,
                "max": 47.0,
                "times": [45.0, 44.0, 46.0],
                "exit_codes": [0, 0, 0],
            },
            {
                "command": "ocx_install_100mbps",
                "mean": 12.0,
                "stddev": 0.5,
                "median": 12.0,
                "min": 11.5,
                "max": 12.5,
                "times": [12.0, 12.0, 12.0],
                "exit_codes": [0, 0, 0],
            },
        ]
    }


@pytest.fixture()
def sample_current_json_pass() -> dict:
    """Current results that pass the 0.85x threshold (improvement)."""
    return {
        "results": [
            {
                "command": "ocx_install_10mbps",
                "mean": 29.0,  # 29/45 = 0.644x — improvement
                "stddev": 0.8,
                "median": 29.0,
                "min": 28.0,
                "max": 30.0,
                "times": [29.0, 29.0, 29.0],
                "exit_codes": [0, 0, 0],
            },
            {
                "command": "ocx_install_100mbps",
                "mean": 9.0,  # 9/12 = 0.75x — improvement
                "stddev": 0.3,
                "median": 9.0,
                "min": 8.5,
                "max": 9.5,
                "times": [9.0, 9.0, 9.0],
                "exit_codes": [0, 0, 0],
            },
        ]
    }


@pytest.fixture()
def sample_current_json_fail() -> dict:
    """Current results that FAIL the 0.85x threshold (regression)."""
    return {
        "results": [
            {
                "command": "ocx_install_10mbps",
                "mean": 60.0,  # 60/45 = 1.333x — regression (> 1/0.85 = 1.176)
                "stddev": 1.0,
                "median": 60.0,
                "min": 59.0,
                "max": 61.0,
                "times": [60.0, 60.0, 60.0],
                "exit_codes": [0, 0, 0],
            },
            {
                "command": "ocx_install_100mbps",
                "mean": 9.0,  # 9/12 = 0.75x — pass
                "stddev": 0.3,
                "median": 9.0,
                "min": 8.5,
                "max": 9.5,
                "times": [9.0, 9.0, 9.0],
                "exit_codes": [0, 0, 0],
            },
        ]
    }


@pytest.fixture()
def skip_unless_live():
    """Skip the test unless BENCH_SMOKE_LIVE=1 is set.

    Used to gate tests that require Docker + toxiproxy + hyperfine.
    """
    if os.environ.get("BENCH_SMOKE_LIVE") != "1":
        pytest.skip("BENCH_SMOKE_LIVE=1 not set; skipping live bench fixture test")
