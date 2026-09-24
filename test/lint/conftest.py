"""Admission for the lint tier (plan_test_speed_tiers.md C-008, ADR C-LINT).

`test/lint/` holds the structural sweeps: tests that read the tree (sources,
docs, the acceptance modules themselves) and judge it. They sit outside the
Bazel acceptance graph on purpose — a sweep's input is every sibling module,
so a cached verdict over it is stale the moment any sibling changes — and run
uncached through `task test:lint:structure` instead. That is only cheap and
only honest while nothing here reaches the binary under test, a registry or
the task runner, so this file makes each of those routes an error rather than
a convention:

* a test whose fixture closure names a registry- or binary-backed fixture
  errors at setup (`FORBIDDEN_FIXTURES`; the closure, so a helper fixture that
  requests `registry` counts too), and setting one up by any other road —
  `request.getfixturevalue`, a session-scoped fixture — raises there;
* `subprocess.Popen` is replaced from `pytest_configure` to
  `pytest_unconfigure`, so an argv whose executable resolves under
  `test/bin/`, or is `task`, raises before anything spawns — at import time,
  from a fixture of any scope, and through `from subprocess import Popen`
  alike. `git` and `sys.executable` stay open, which is what a sweep needs;
* the session is budgeted: above `OCX_LINT_BUDGET_SECONDS` (default 30) wall
  clock it fails, whatever the tests said (S-015).

`test_lint_tier_guards.py` shows each route red and each control green by
running this very module as a plugin of a `pytester` session.
"""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
import time
from pathlib import Path

import pytest

pytest_plugins = ["pytester"]

FORBIDDEN_FIXTURES = frozenset(
    {
        "ocx",
        "ocx_binary",
        "registry",
        "mirror_registry",
        "legacy_registry",
        "target_registry",
        "published_package",
        "sigstore_stack",
    }
)
TEST_BIN = (Path(__file__).resolve().parents[1] / "bin").resolve()
FORBIDDEN_PROGRAMS = frozenset({"task", "task.exe"})
BUDGET_ENV = "OCX_LINT_BUDGET_SECONDS"
DEFAULT_BUDGET_SECONDS = 30.0

# (start, budget) per session. The budget is read at the start, not the end:
# a `pytester` run inside a lint test sets it with `monkeypatch`, and the value
# the session began under is the one it is judged by.
_CLOCK = pytest.StashKey[tuple[float, float]]()


class LintTierViolation(Exception):
    """A lint test reached a route the tier admits none of."""


_REAL_POPEN = subprocess.Popen
# What each configure replaced, restored by its unconfigure. A stack, not one
# slot: `test_lint_tier_guards.py` runs this module as a plugin of an inner
# in-process session, whose unconfigure must hand back the outer session's
# guard rather than the real class.
_REPLACED: list[type] = []


def _program_route(program: str, *, path_env: str | None, cwd: str | os.PathLike[str] | None) -> str | None:
    """Why `program` is refused, or None.

    Judged both as spelled (made absolute against `cwd`) and resolved, so a
    symlink under `test/bin/` pointing elsewhere is still under `test/bin/`.
    A missing file is still refused by path: a fresh checkout has no
    `test/bin/ocx`, and the spawn must red whether or not it was built.
    """
    if Path(program).name in FORBIDDEN_PROGRAMS:
        return f"task: {program}"
    found = shutil.which(program, path=path_env) if os.sep not in program else None
    candidate = Path(found or program)
    if not candidate.is_absolute() and cwd is not None:
        candidate = Path(cwd) / candidate
    for seen in (candidate.absolute(), candidate.resolve()):
        if seen.is_relative_to(TEST_BIN):
            return f"binary under test/bin: {program}"
        if seen.name in FORBIDDEN_PROGRAMS:
            return f"task: {program} -> {seen}"
    return None


def forbidden_route(args, *, env=None, cwd=None, shell=False, executable=None) -> str | None:
    """The route a `Popen(args, ...)` would take that the lint tier refuses, or None.

    Under `shell=True` every word of the command line is judged, not only the
    first: `cd x && task y` spawns `task` too. That refuses a harmless
    `echo task` as well, which is the direction to be wrong in here.
    """
    if isinstance(args, (str, bytes, os.PathLike)):
        words = [os.fsdecode(args)]
    else:
        words = [os.fsdecode(word) for word in args]
    if shell and words:
        words = shlex.split(words[0]) + words[1:]
    elif words:
        words = words[:1]
    if executable is not None:
        words.append(os.fsdecode(executable))
    path_env = (env if env is not None else os.environ).get("PATH")
    for word in words:
        if route := _program_route(word, path_env=path_env, cwd=cwd):
            return route
    return None


def pytest_sessionstart(session: pytest.Session) -> None:
    # The root conftest's own `pytest_sessionstart` would otherwise
    # `docker compose up` a registry for a tier that may never reach one.
    # Registered later, this hook runs first.
    os.environ.setdefault("OCX_TESTS_NO_REGISTRY", "1")
    budget = float(os.environ.get(BUDGET_ENV, DEFAULT_BUDGET_SECONDS))
    session.config.stash[_CLOCK] = (time.monotonic(), budget)


def pytest_runtest_setup(item: pytest.Item) -> None:
    banned = sorted(FORBIDDEN_FIXTURES.intersection(getattr(item, "fixturenames", ())))
    if banned:
        raise LintTierViolation(f"lint tier admits no fixture: {', '.join(banned)} ({item.nodeid})")


class _Guarded(_REAL_POPEN):
    """`subprocess.Popen`, refusing the binary under test and the task runner.

    `subprocess.run`, `check_output`, `call` and asyncio's subprocess
    transport all construct `subprocess.Popen` by module attribute, so
    replacing that attribute covers them. A subclass rather than a function,
    so `isinstance(p, subprocess.Popen)` still holds for what is admitted.
    """

    def __init__(self, args, *rest, **kwargs):
        route = forbidden_route(
            args,
            env=kwargs.get("env"),
            cwd=kwargs.get("cwd"),
            shell=kwargs.get("shell", False),
            executable=kwargs.get("executable"),
        )
        if route is not None:
            raise LintTierViolation(f"lint tier admits no {route}")
        super().__init__(args, *rest, **kwargs)


def pytest_configure(config: pytest.Config) -> None:
    # Before collection, so a module importing `Popen` by name, a spawn at
    # import time and a session-scoped fixture all meet the guard.
    _REPLACED.append(subprocess.Popen)
    subprocess.Popen = _Guarded


def pytest_unconfigure(config: pytest.Config) -> None:
    if _REPLACED:
        subprocess.Popen = _REPLACED.pop()


def pytest_fixture_setup(fixturedef: pytest.FixtureDef, request: pytest.FixtureRequest) -> None:
    # The closure check above cannot see `request.getfixturevalue(...)`; the
    # set-up itself can, whatever scope or road reached it.
    if fixturedef.argname in FORBIDDEN_FIXTURES:
        raise LintTierViolation(f"lint tier admits no fixture: {fixturedef.argname} ({request.node.nodeid})")


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    clock = session.config.stash.get(_CLOCK, None)
    if clock is None:
        return
    started, budget = clock
    elapsed = time.monotonic() - started
    over = elapsed > budget
    line = f"lint tier: {elapsed:.1f} s wall clock, budget {budget:g} s ({BUDGET_ENV})"
    reporter = session.config.pluginmanager.get_plugin("terminalreporter")
    if reporter is not None:
        reporter.write_sep("-", line + (" — OVER BUDGET" if over else ""), red=over, bold=over)
    # Only a clean session is turned red: an interrupted or errored run keeps
    # the status that says what actually went wrong.
    if over and session.exitstatus == pytest.ExitCode.OK:
        session.exitstatus = pytest.ExitCode.TESTS_FAILED
