# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Unit tests for `conftest._wait_for_reachable` — the retry loop that gives
`target_registry` the same cold-start grace `mirror_registry` already had.

Pure and Docker-free: `is_reachable` and `sleep` are both injected, so this
proves the polling shape (attempt count, backoff between tries, no sleep
after a final attempt) on a fully controlled input rather than against a real
registry's boot time, which the acceptance suite already covers end to end.
It also holds the Docker skip contract: `OCX_TEST_NO_DOCKER=1` skips, nothing infers it.
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

from src import helpers


def _load_root_conftest():
    """Loads `test/conftest.py` by path.

    A bare `import conftest` is ambiguous here: `test/tests/conftest.py`
    (function-scoped fixtures) and `test/conftest.py` (session-scoped,
    where `_wait_for_reachable` lives) share the module name, and pytest's
    "prepend" import mode puts `test/tests/` on `sys.path` ahead of the
    `pythonpath` entries — the same by-path convention `_load_fake_forge_module`
    uses one directory over, for the same reason. Registered under a
    dedicated name in `sys.modules` before execution because the module
    defines a `@dataclass` (`MockHelper`), and `dataclasses` resolves the
    owning module by name from `sys.modules` while the class body runs.
    """
    module_path = Path(__file__).parent.parent / "conftest.py"
    spec = importlib.util.spec_from_file_location("root_conftest", module_path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


conftest = _load_root_conftest()


def test_wait_for_reachable_returns_true_immediately_when_already_up() -> None:
    calls = 0

    def is_reachable() -> bool:
        nonlocal calls
        calls += 1
        return True

    sleeps: list[float] = []
    result = conftest._wait_for_reachable(is_reachable, attempts=10, sleep=sleeps.append)

    assert result is True
    assert calls == 1
    assert sleeps == []


def test_wait_for_reachable_retries_until_the_service_comes_up() -> None:
    # Reachable only on the third probe — a late-binding port, the exact
    # shape the target-registry cold start produces.
    outcomes = iter([False, False, True])

    def is_reachable() -> bool:
        return next(outcomes)

    sleeps: list[float] = []
    result = conftest._wait_for_reachable(is_reachable, attempts=10, delay_seconds=0.5, sleep=sleeps.append)

    assert result is True
    assert sleeps == [0.5, 0.5], "one sleep between each failed probe and the next, none after success"


def test_wait_for_reachable_gives_up_after_exhausting_every_attempt() -> None:
    calls = 0

    def is_reachable() -> bool:
        nonlocal calls
        calls += 1
        return False

    sleeps: list[float] = []
    result = conftest._wait_for_reachable(is_reachable, attempts=3, delay_seconds=0.1, sleep=sleeps.append)

    assert result is False
    assert calls == 3, "every attempt is spent before giving up"
    assert sleeps == [0.1, 0.1], "no sleep follows the final, still-failing attempt"


NO_DOCKER_REASON = "no docker:"


def test_no_docker_flag_skips_the_mirror_fixtures_even_when_a_registry_answers(monkeypatch) -> None:
    monkeypatch.delenv("OCX_TESTS_NO_REGISTRY", raising=False)
    monkeypatch.setenv("OCX_TEST_NO_DOCKER", "1")
    monkeypatch.setattr(conftest, "registry_is_reachable", lambda _address: True)

    with pytest.raises(pytest.skip.Exception) as skipped:
        conftest._mirror_address("localhost:5001", "mirror_registry")

    assert str(skipped.value).startswith(NO_DOCKER_REASON)


def test_no_docker_flag_skips_the_sigstore_stack(monkeypatch) -> None:
    monkeypatch.setenv("OCX_TEST_NO_DOCKER", "1")

    with pytest.raises(pytest.skip.Exception) as skipped:
        helpers.start_sigstore_stack()

    assert str(skipped.value).startswith(NO_DOCKER_REASON)


@pytest.mark.parametrize("value", [None, "", "0", "true"])
def test_nothing_but_the_flag_set_to_one_skips(monkeypatch, value: str | None) -> None:
    if value is None:
        monkeypatch.delenv("OCX_TEST_NO_DOCKER", raising=False)
    else:
        monkeypatch.setenv("OCX_TEST_NO_DOCKER", value)

    helpers.skip_without_docker("anything")


def test_a_missing_docker_cli_fails_when_the_job_declares_nothing(monkeypatch) -> None:
    monkeypatch.delenv("OCX_TESTS_NO_REGISTRY", raising=False)
    monkeypatch.delenv("OCX_TEST_NO_DOCKER", raising=False)
    monkeypatch.setattr(conftest, "registry_is_reachable", lambda _address: False)
    monkeypatch.setattr(conftest.shutil, "which", lambda _name: None)

    with pytest.raises(pytest.fail.Exception, match="OCX_TEST_NO_DOCKER=1"):
        conftest._mirror_address("localhost:5001", "mirror_registry")
