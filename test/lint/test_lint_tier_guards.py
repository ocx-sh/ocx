"""Red/green proof of the lint tier's admission (see adr_test_speed_tiers.md).

Each case runs an inner pytest session through `pytester` with the REAL
`test/lint/conftest.py` module passed as a plugin — never a copy, whose
`TEST_BIN` would be derived from the tmp dir. Why each red discriminates:

- forbidden fixture: the inner file defines the fixture itself, so without
  the admission hook the test passes; `errors=1` plus the `lint tier admits
  no fixture` line can only come from the conftest.
- `test/bin/ocx` / `task` spawn: without the wrapper the spawn either runs or
  raises FileNotFoundError; neither prints `lint tier admits no`. The
  `test/bin/ocx` case must red whether or not the binary was built, since
  the lint tier runs without a build.
- budget: the inner test itself passes, so only the session-level budget
  check can turn the exit code to TESTS_FAILED. The CPU and wall readings are
  module seams, replaced per case rather than burned.
"""

from __future__ import annotations

from pathlib import Path

import pytest

CONFTEST = Path(__file__).parent / "conftest.py"
REPO_ROOT = Path(__file__).resolve().parents[2]


def _run_isolated(pytester: pytest.Pytester, source: str) -> pytest.RunResult:
    """An inner session in its own interpreter, with a copy of the conftest.

    For the spawn-scope cases: an in-process inner session runs inside this
    session's guard, so it would pass whatever the copy under test did. A
    copy is fine here because these cases spawn `task`, whose refusal does
    not depend on `TEST_BIN`.
    """
    pytester.makeconftest(CONFTEST.read_text(encoding="utf-8"))
    pytester.makepyfile(source)
    return pytester.runpytest_subprocess("-p", "no:cacheprovider")


def _run_with_conftest(
    pytester: pytest.Pytester, request: pytest.FixtureRequest, source: str
) -> pytest.RunResult:
    plugin = request.config.pluginmanager.get_plugin(str(CONFTEST))
    assert plugin is not None, f"lint conftest not registered under {CONFTEST}"
    pytester.makepyfile(source)
    return pytester.runpytest_inprocess("-p", "no:cacheprovider", plugins=[plugin])


@pytest.mark.parametrize(
    "name",
    [
        "ocx",
        "ocx_binary",
        "registry",
        "mirror_registry",
        "legacy_registry",
        "target_registry",
        "published_package",
        "sigstore_stack",
    ],
)
def test_c008_s014_forbidden_fixture_errors_at_setup(
    pytester: pytest.Pytester, request: pytest.FixtureRequest, name: str
) -> None:
    """A lint test requesting a forbidden fixture errors at setup."""
    result = _run_with_conftest(
        pytester,
        request,
        f"""
        import pytest

        @pytest.fixture
        def {name}():
            return object()

        def test_it({name}):
            pass
        """,
    )
    result.assert_outcomes(errors=1)
    result.stdout.fnmatch_lines([f"*lint tier admits no fixture*{name}*"])


def test_c008_forbidden_fixture_reached_transitively_errors_at_setup(
    pytester: pytest.Pytester, request: pytest.FixtureRequest
) -> None:
    """The fixture closure counts — `wrapper` requesting `registry` reds."""
    result = _run_with_conftest(
        pytester,
        request,
        """
        import pytest

        @pytest.fixture
        def registry():
            return object()

        @pytest.fixture
        def wrapper(registry):
            return registry

        def test_it(wrapper):
            pass
        """,
    )
    result.assert_outcomes(errors=1)
    result.stdout.fnmatch_lines(["*lint tier admits no fixture*registry*"])


def test_c008_forbidden_fixture_reached_by_getfixturevalue_is_refused(
    pytester: pytest.Pytester, request: pytest.FixtureRequest
) -> None:
    """`request.getfixturevalue` bypasses the closure — the set-up itself refuses it."""
    result = _run_with_conftest(
        pytester,
        request,
        """
        import pytest

        @pytest.fixture
        def ocx_binary():
            return object()

        def test_it(request):
            request.getfixturevalue("ocx_binary")
        """,
    )
    result.assert_outcomes(failed=1)
    result.stdout.fnmatch_lines(["*lint tier admits no fixture: ocx_binary*"])


def test_c008_spawn_from_a_module_scoped_fixture_is_refused(pytester: pytest.Pytester) -> None:
    """The spawn guard spans the session, not only each test's own body."""
    result = _run_isolated(
        pytester,
        """
        import subprocess

        import pytest

        @pytest.fixture(scope="module")
        def built():
            subprocess.run(["task", "--version"])

        def test_it(built):
            pass
        """,
    )
    result.assert_outcomes(errors=1)
    result.stdout.fnmatch_lines(["*lint tier admits no task*"])


def test_c008_popen_imported_by_name_is_refused(pytester: pytest.Pytester) -> None:
    """`from subprocess import Popen` at import time gets the guard, not the real class."""
    result = _run_isolated(
        pytester,
        """
        from subprocess import Popen

        def test_it():
            Popen(["task", "--version"]).wait()
        """,
    )
    result.assert_outcomes(failed=1)
    result.stdout.fnmatch_lines(["*lint tier admits no task*"])


def test_c008_s014_test_bin_ocx_spawn_is_refused(
    pytester: pytest.Pytester, request: pytest.FixtureRequest
) -> None:
    """An argv whose executable resolves under test/bin raises."""
    plugin = request.config.pluginmanager.get_plugin(str(CONFTEST))
    assert plugin is not None
    ocx = str(plugin.TEST_BIN / "ocx")
    result = _run_with_conftest(
        pytester,
        request,
        f"""
        import subprocess

        def test_it():
            subprocess.run([{ocx!r}, "--version"])
        """,
    )
    result.assert_outcomes(failed=1)
    result.stdout.fnmatch_lines(["*lint tier admits no*"])


def test_c008_s014_symlink_into_test_bin_is_refused(
    pytester: pytest.Pytester, request: pytest.FixtureRequest
) -> None:
    """A link outside test/bin whose target is under it raises.

    Only the resolved leg of `_program_route` sees this one: as spelled, the
    link sits in the pytester tmp dir under a name that is not `task`. The
    link may dangle (no build), and `resolve()` still follows it.
    """
    plugin = request.config.pluginmanager.get_plugin(str(CONFTEST))
    assert plugin is not None
    link = pytester.path / "innocent-tool"
    link.symlink_to(plugin.TEST_BIN / "ocx")
    result = _run_with_conftest(
        pytester,
        request,
        f"""
        import subprocess

        def test_it():
            subprocess.run([{str(link)!r}, "--version"])
        """,
    )
    result.assert_outcomes(failed=1)
    result.stdout.fnmatch_lines(["*lint tier admits no binary under test/bin*"])


def test_c008_s014_task_spawn_is_refused(
    pytester: pytest.Pytester, request: pytest.FixtureRequest
) -> None:
    """Spawning `task` raises."""
    result = _run_with_conftest(
        pytester,
        request,
        """
        import subprocess

        def test_it():
            subprocess.run(["task", "--version"])
        """,
    )
    result.assert_outcomes(failed=1)
    result.stdout.fnmatch_lines(["*lint tier admits no task*"])


def test_c008_s014_git_ls_files_is_admitted(
    pytester: pytest.Pytester, request: pytest.FixtureRequest
) -> None:
    """Control: `git ls-files` stays allowed."""
    result = _run_with_conftest(
        pytester,
        request,
        f"""
        import subprocess

        def test_it():
            out = subprocess.run(["git", "ls-files"], cwd={str(REPO_ROOT)!r}, capture_output=True, check=True)
            assert out.stdout.strip()
        """,
    )
    result.assert_outcomes(passed=1)


def test_c008_sys_executable_is_admitted(
    pytester: pytest.Pytester, request: pytest.FixtureRequest
) -> None:
    """Control: `sys.executable` stays allowed."""
    result = _run_with_conftest(
        pytester,
        request,
        """
        import subprocess
        import sys

        def test_it():
            subprocess.run([sys.executable, "-c", "pass"], check=True)
        """,
    )
    result.assert_outcomes(passed=1)


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


def _readings(monkeypatch: pytest.MonkeyPatch, request: pytest.FixtureRequest, *, cpu: float, wall: float) -> None:
    """Pin the session's CPU and wall readings: zero at start, `cpu`/`wall` at the end."""
    plugin = request.config.pluginmanager.get_plugin(str(CONFTEST))
    assert plugin is not None, f"lint conftest not registered under {CONFTEST}"
    for name, end in (("_cpu_seconds", cpu), ("_wall_seconds", wall)):
        calls = iter([0.0])
        monkeypatch.setattr(plugin, name, lambda calls=calls, end=end: next(calls, end))


def test_s031_wall_jump_with_low_cpu_stays_green(
    pytester: pytest.Pytester,
    request: pytest.FixtureRequest,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A host so loaded the wall clock jumps 100 s, while the session spent 2 CPU s, is green."""
    monkeypatch.setenv("OCX_LINT_BUDGET_SECONDS", "30")
    _readings(monkeypatch, request, cpu=2.0, wall=100.0)
    result = _run_with_conftest(pytester, request, _PASSING)
    result.assert_outcomes(passed=1)
    assert result.ret == pytest.ExitCode.OK
    result.stdout.fnmatch_lines(["*lint tier:*CPU*budget*"])


def test_s031_cpu_over_budget_fails(
    pytester: pytest.Pytester,
    request: pytest.FixtureRequest,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """CPU seconds above the budget fail the session, though every test passed."""
    monkeypatch.setenv("OCX_LINT_BUDGET_SECONDS", "30")
    _readings(monkeypatch, request, cpu=31.0, wall=31.0)
    result = _run_with_conftest(pytester, request, _PASSING)
    result.assert_outcomes(passed=1)
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    result.stdout.fnmatch_lines(["*lint tier:*CPU*OVER BUDGET*"])


def test_s031_wall_over_five_times_budget_fails(
    pytester: pytest.Pytester,
    request: pytest.FixtureRequest,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The wall backstop is 5x the budget, so a hung run still reds on low CPU."""
    monkeypatch.setenv("OCX_LINT_BUDGET_SECONDS", "30")
    _readings(monkeypatch, request, cpu=1.0, wall=151.0)
    result = _run_with_conftest(pytester, request, _PASSING)
    result.assert_outcomes(passed=1)
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    result.stdout.fnmatch_lines(["*lint tier:*wall*OVER BUDGET*"])


def test_s031_worker_cpu_is_summed(pytester: pytest.Pytester, monkeypatch: pytest.MonkeyPatch) -> None:
    """The controller idles while xdist workers burn, so only the worker sum can red this."""
    monkeypatch.setenv("OCX_LINT_BUDGET_SECONDS", "2")
    pytester.makeconftest(CONFTEST.read_text(encoding="utf-8"))
    pytester.makepyfile(_BURNER.format(n="a") + _BURNER.format(n="b"))
    result = pytester.runpytest_subprocess("-p", "no:cacheprovider", "-n", "2")
    result.assert_outcomes(passed=2)
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    result.stdout.fnmatch_lines(["*lint tier:*CPU*OVER BUDGET*"])


def test_s031_cpu_left_by_an_earlier_session_is_not_summed(
    pytester: pytest.Pytester,
    request: pytest.FixtureRequest,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Worker seconds are per session: a stale total from an earlier run must not red this one."""
    monkeypatch.setenv("OCX_LINT_BUDGET_SECONDS", "30")
    plugin = request.config.pluginmanager.get_plugin(str(CONFTEST))
    assert plugin is not None, f"lint conftest not registered under {CONFTEST}"
    monkeypatch.setattr(plugin, "_WORKER_CPU_SECONDS", [1000.0])
    _readings(monkeypatch, request, cpu=1.0, wall=1.0)
    result = _run_with_conftest(pytester, request, _PASSING)
    result.assert_outcomes(passed=1)
    assert result.ret == pytest.ExitCode.OK
