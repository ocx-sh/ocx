"""The gate-tooling session budget is CPU seconds, backed by wall at 5x.

Each case runs an inner session through `pytester` with the REAL
`scripts/tests/conftest.py` as a plugin. The CPU and wall readings are
module seams, replaced per case: a loaded host stretches the wall clock
without costing the suite any more, and that must stay green, while spent CPU
(the controller's plus every xdist worker's) must red.
"""

from __future__ import annotations

from pathlib import Path

import pytest

pytest_plugins = ["pytester"]

CONFTEST = Path(__file__).parent / "conftest.py"

_PASSING = """
def test_it():
    pass
"""

_BURNER = """
import time

def test_{n}():
    end = time.process_time() + 1.5
    while time.process_time() < end:
        pass
"""


def _plugin(request: pytest.FixtureRequest):
    plugin = request.config.pluginmanager.get_plugin(str(CONFTEST))
    assert plugin is not None, f"tooling conftest not registered under {CONFTEST}"
    return plugin


def _run(
    pytester: pytest.Pytester,
    request: pytest.FixtureRequest,
    monkeypatch: pytest.MonkeyPatch,
    *,
    cpu: float,
    wall: float,
) -> pytest.RunResult:
    plugin = _plugin(request)
    monkeypatch.setenv("OCX_TOOLING_BUDGET_SECONDS", "30")
    for name, end in (("_cpu_seconds", cpu), ("_wall_seconds", wall)):
        calls = iter([0.0])
        monkeypatch.setattr(plugin, name, lambda calls=calls, end=end: next(calls, end))
    pytester.makepyfile(_PASSING)
    return pytester.runpytest_inprocess("-p", "no:cacheprovider", plugins=[plugin])


def test_wall_jump_with_low_cpu_stays_green(
    pytester: pytest.Pytester, request: pytest.FixtureRequest, monkeypatch: pytest.MonkeyPatch
) -> None:
    result = _run(pytester, request, monkeypatch, cpu=2.0, wall=100.0)
    result.assert_outcomes(passed=1)
    assert result.ret == pytest.ExitCode.OK
    result.stdout.fnmatch_lines(["*gate tooling:*CPU*budget*"])


def test_cpu_over_budget_fails(
    pytester: pytest.Pytester, request: pytest.FixtureRequest, monkeypatch: pytest.MonkeyPatch
) -> None:
    result = _run(pytester, request, monkeypatch, cpu=31.0, wall=31.0)
    result.assert_outcomes(passed=1)
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    result.stdout.fnmatch_lines(["*gate tooling:*CPU*OVER BUDGET*"])


def test_wall_over_five_times_budget_fails(
    pytester: pytest.Pytester, request: pytest.FixtureRequest, monkeypatch: pytest.MonkeyPatch
) -> None:
    result = _run(pytester, request, monkeypatch, cpu=1.0, wall=151.0)
    result.assert_outcomes(passed=1)
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    result.stdout.fnmatch_lines(["*gate tooling:*wall*OVER BUDGET*"])


def test_worker_cpu_is_summed(pytester: pytest.Pytester, monkeypatch: pytest.MonkeyPatch) -> None:
    """The controller idles while xdist workers burn, so only the worker sum can red this."""
    monkeypatch.setenv("OCX_TOOLING_BUDGET_SECONDS", "2")
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    pytester.makeconftest(CONFTEST.read_text(encoding="utf-8"))
    pytester.makepyfile(_BURNER.format(n="a") + _BURNER.format(n="b"))
    result = pytester.runpytest_subprocess("-p", "no:cacheprovider", "-n", "2")
    result.assert_outcomes(passed=2)
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    result.stdout.fnmatch_lines(["*gate tooling:*CPU*OVER BUDGET*"])
