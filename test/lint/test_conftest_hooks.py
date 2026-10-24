# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`test/conftest.py`'s session hooks, and `src.helpers.rglob_no_links`, driven through `pytester`.

The release-bytes skip runs the real conftest as a plugin of an in-process inner session.
The per-test deadline kills its process, so it runs a copy of the conftest in a subprocess.
`_wants_zot` is a pure function of the config and is called directly. None of this starts a
registry: the inner sessions set `OCX_TESTS_NO_REGISTRY`.
"""

from __future__ import annotations

import inspect
import os
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from src import zot
from src.helpers import rglob_no_links

TEST_ROOT = Path(__file__).resolve().parents[1]
ROOT_CONFTEST = TEST_ROOT / "conftest.py"

MARKED = """
import pytest

@pytest.mark.requires_testing_build
def test_seam():
    pass

def test_plain():
    pass
"""


@pytest.fixture
def root_conftest(request: pytest.FixtureRequest):
    plugin = request.config.pluginmanager.get_plugin(str(ROOT_CONFTEST))
    assert plugin is not None, f"root conftest not registered under {ROOT_CONFTEST}"
    return plugin


def _inner(
    pytester: pytest.Pytester, plugin, monkeypatch: pytest.MonkeyPatch, source: str
) -> pytest.RunResult:
    monkeypatch.setenv("OCX_TESTS_NO_REGISTRY", "1")
    pytester.makeini(
        "[pytest]\nmarkers =\n    requires_testing_build: a release-bytes seam\n"
    )
    pytester.makepyfile(source)
    return pytester.runpytest_inprocess(
        "-p", "no:cacheprovider", "-rs", plugins=[plugin]
    )


def test_release_bytes_skip_a_marked_test_by_name_and_run_the_rest(
    pytester: pytest.Pytester, root_conftest, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("OCX_TEST_RELEASE_BINARY", "1")
    result = _inner(pytester, root_conftest, monkeypatch, MARKED)
    result.assert_outcomes(passed=1, skipped=1)
    result.stdout.fnmatch_lines(["SKIPPED*release bytes:*"])


@pytest.mark.parametrize("value", [None, "0", ""])
def test_a_marked_test_runs_unless_release_bytes_are_declared(
    pytester: pytest.Pytester,
    root_conftest,
    monkeypatch: pytest.MonkeyPatch,
    value: str | None,
) -> None:
    if value is None:
        monkeypatch.delenv("OCX_TEST_RELEASE_BINARY", raising=False)
    else:
        monkeypatch.setenv("OCX_TEST_RELEASE_BINARY", value)
    result = _inner(pytester, root_conftest, monkeypatch, MARKED)
    result.assert_outcomes(passed=2)


def _config(args, invoked_from=TEST_ROOT, **flags) -> SimpleNamespace:
    config = SimpleNamespace(
        args=args,
        invocation_params=SimpleNamespace(dir=invoked_from),
        getoption=lambda name: flags.get(name, False),
    )
    if flags.pop("worker", False):  # xdist workers carry `workerinput`
        config.workerinput = {}
    return config


@pytest.mark.parametrize(
    ("args", "invoked_from", "expected"),
    [
        (["tests"], TEST_ROOT, True),
        (["tests/test_install.py::test_x"], TEST_ROOT, True),
        ([], TEST_ROOT, True),
        (["lint"], TEST_ROOT, False),
        (["lint/test_zot.py::test_x"], TEST_ROOT, False),
        (["test_zot.py"], TEST_ROOT / "lint", False),
        (["lint", "tests"], TEST_ROOT, True),
        (["lintx"], TEST_ROOT, True),
        (["/"], TEST_ROOT, True),
    ],
)
def test_the_session_starts_zot_unless_every_target_is_the_lint_tier(
    root_conftest, monkeypatch: pytest.MonkeyPatch, args, invoked_from, expected: bool
) -> None:
    monkeypatch.delenv("OCX_TESTS_NO_REGISTRY", raising=False)
    assert root_conftest._wants_zot(_config(args, invoked_from)) is expected


@pytest.mark.parametrize(
    "opt_out",
    [
        "worker",
        "no_registry",
        "collectonly",
        "help",
        "version",
        "showfixtures",
        "markers",
    ],
)
def test_the_session_does_not_start_zot_when_it_opts_out_or_runs_no_test(
    root_conftest, monkeypatch: pytest.MonkeyPatch, opt_out: str
) -> None:
    monkeypatch.delenv("OCX_TESTS_NO_REGISTRY", raising=False)
    if opt_out == "no_registry":
        monkeypatch.setenv("OCX_TESTS_NO_REGISTRY", "1")
        config = _config(["tests"])
    else:
        config = _config(["tests"], **{opt_out: True})
    assert root_conftest._wants_zot(config) is False


@pytest.mark.parametrize(
    "flag", ["--collect-only", "--help", "--version", "--fixtures", "--markers"]
)
def test_the_no_test_flags_are_read_under_pytests_own_option_names(
    pytester: pytest.Pytester,
    root_conftest,
    monkeypatch: pytest.MonkeyPatch,
    flag: str,
) -> None:
    """A fake config answers any name; only the real parser knows `--fixtures` is `showfixtures`."""
    monkeypatch.delenv("OCX_TESTS_NO_REGISTRY", raising=False)
    assert (
        root_conftest._wants_zot(pytester.parseconfig("-p", "no:cacheprovider")) is True
    ), "control: without a no-test flag the same real config starts zot"
    config = pytester.parseconfig("-p", "no:cacheprovider", flag)
    assert root_conftest._wants_zot(config) is False


def _configure(
    pytester: pytest.Pytester,
    root_conftest,
    monkeypatch: pytest.MonkeyPatch,
    start,
) -> pytest.Config:
    """A real config that wants zot, with `zot.start` replaced by `start`."""
    monkeypatch.delenv("OCX_TESTS_NO_REGISTRY", raising=False)
    monkeypatch.setattr(root_conftest.zot, "start", start)
    return pytester.parseconfig("-p", "no:cacheprovider")


def test_a_zot_that_fails_to_start_aborts_the_session_as_a_usage_error_naming_the_cause(
    pytester: pytest.Pytester, root_conftest, monkeypatch: pytest.MonkeyPatch
) -> None:
    cause = (
        "zot `registry` did not answer GET /v2/ with 200 within 60 s; log: /x/zot.log"
    )

    def start(*args, **kwargs):
        raise zot.ZotError(cause)

    config = _configure(pytester, root_conftest, monkeypatch, start)
    with pytest.raises(pytest.UsageError) as failure:
        root_conftest.pytest_configure(config)
    assert str(failure.value) == cause
    assert root_conftest._ZOT not in config.stash
    # Nothing was started, so unconfigure has nothing to stop.
    root_conftest.pytest_unconfigure(config)


def test_a_started_stack_is_exported_stashed_and_stopped_at_unconfigure(
    pytester: pytest.Pytester, root_conftest, monkeypatch: pytest.MonkeyPatch
) -> None:
    events: list = []

    class Stack:
        def env(self) -> dict[str, str]:
            return {"OCX_TEST_REGISTRY_PORT": "4242"}

        def stop(self) -> None:
            events.append("stop")

    stack = Stack()

    def start(*args, **kwargs):
        events.append((args, kwargs))
        return stack

    config = _configure(pytester, root_conftest, monkeypatch, start)
    monkeypatch.setenv("OCX_TEST_REGISTRY_PORT", "0")  # restored at teardown
    root_conftest.pytest_configure(config)
    assert os.environ["OCX_TEST_REGISTRY_PORT"] == "4242"
    assert config.stash[root_conftest._ZOT] is stack
    assert events == [((), {})], "the session passes no timeout: the default governs"
    root_conftest.pytest_unconfigure(config)
    assert events[-1] == "stop"


def test_the_wait_the_session_gives_zot_is_sixty_seconds() -> None:
    """The session calls `zot.start()` bare (test above), so this default is the wait."""
    assert zot.READY_TIMEOUT_SECONDS == 60.0
    assert (
        inspect.signature(zot.start).parameters["timeout"].default
        == zot.READY_TIMEOUT_SECONDS
    )


def _hanging_run(
    pytester: pytest.Pytester,
    monkeypatch: pytest.MonkeyPatch,
    deadline: str,
    sleep: float,
    after_tests: str = "",
) -> tuple[pytest.RunResult, float]:
    """The conftest copy, in its own interpreter: the deadline ends the process."""
    pytester.makeconftest(ROOT_CONFTEST.read_text(encoding="utf-8") + after_tests)
    pytester.makepyfile(f"import time\n\ndef test_hangs():\n    time.sleep({sleep})\n")
    monkeypatch.setenv("PYTHONPATH", str(TEST_ROOT))  # the copy imports `src`
    monkeypatch.setenv("OCX_TESTS_NO_REGISTRY", "1")
    monkeypatch.setenv("OCX_TEST_DEADLINE_SECONDS", deadline)
    started = time.monotonic()
    result = pytester.runpytest_subprocess("-p", "no:cacheprovider", timeout=90)
    return result, time.monotonic() - started


def test_a_test_past_its_deadline_dumps_its_stack_and_kills_the_process(
    pytester: pytest.Pytester, monkeypatch: pytest.MonkeyPatch
) -> None:
    result, elapsed = _hanging_run(pytester, monkeypatch, deadline="1", sleep=25)
    (log,) = pytester.path.glob("runpytest-[0-9]*/deadline-*.log")
    assert "test_hangs" in log.read_text(encoding="utf-8"), (
        "the dump must name the stuck test"
    )
    assert result.ret != 0
    assert elapsed < 15, (
        f"the process ran {elapsed:.0f} s: the deadline did not kill it"
    )


def test_a_finished_test_leaves_no_timer_to_kill_the_session_after_it(
    pytester: pytest.Pytester, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The session outlives its last test (teardown, zot stop) and must not meet that test's clock."""
    slow_exit = "\n\ndef pytest_unconfigure(config):\n    time.sleep(3)\n"
    result, _ = _hanging_run(
        pytester,
        monkeypatch,
        deadline="1",
        sleep=0.1,
        after_tests="\nimport time" + slow_exit,
    )
    result.assert_outcomes(passed=1)
    assert result.ret == 0
    (log,) = pytester.path.glob("runpytest-[0-9]*/deadline-*.log")
    assert os.path.getsize(log) == 0


def test_rglob_no_links_yields_a_link_but_never_descends_one(tmp_path: Path) -> None:
    (tmp_path / "real").mkdir()
    (tmp_path / "real" / "f.txt").write_text("x", encoding="utf-8")
    try:
        (tmp_path / "alias").symlink_to("real", target_is_directory=True)
        (tmp_path / "real" / "loop").symlink_to("..", target_is_directory=True)
    except OSError as error:
        pytest.skip(f"this host cannot create symlinks: {error}")
    assert list(rglob_no_links(tmp_path, "f.txt")) == [tmp_path / "real" / "f.txt"]
    assert list(rglob_no_links(tmp_path, "alias")) == [tmp_path / "alias"]
    assert list(rglob_no_links(tmp_path, "loop")) == [tmp_path / "real" / "loop"]


def test_rglob_no_links_on_a_missing_root_yields_nothing(tmp_path: Path) -> None:
    assert list(rglob_no_links(tmp_path / "absent", "*")) == []
