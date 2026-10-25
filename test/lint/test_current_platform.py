"""`current_platform()` is the binary under test's host platform, never CPython's.

On Windows `platform.machine()` asks WMI and, on a timeout, falls back to `PROCESSOR_ARCHITECTURE` and caches
that for the process. The x64 interpreter emulated on a windows-11-arm runner then answers `AMD64` in one xdist
worker, which publishes `windows/amd64` fixtures the arm64 `ocx` never selects. Each test makes Python lie that
way and stubs the one `ocx about` call, so no binary is needed.
"""

import platform
from collections.abc import Iterator

import pytest

from src import runner


@pytest.fixture(autouse=True)
def _fresh_cache() -> Iterator[None]:
    runner.current_platform.cache_clear()
    yield
    runner.current_platform.cache_clear()


@pytest.fixture(autouse=True)
def _python_reports_emulated_x64(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Windows")
    monkeypatch.setattr(platform, "machine", lambda: "AMD64")


def _binary_reports(monkeypatch: pytest.MonkeyPatch, os_name: str, arch: str) -> list[tuple[str, ...]]:
    """Stands in for `ocx`: answers `about` with this host and returns the argv tails it was asked."""
    asked: list[tuple[str, ...]] = []

    def about(self: runner.OcxRunner, *args: str, **kwargs: object) -> dict:
        asked.append(args)
        return {"platforms": [{"os": os_name, "architecture": arch, "os.features": []}]}

    monkeypatch.setattr(runner.OcxRunner, "json", about)
    return asked


@pytest.mark.parametrize(
    ("os_name", "arch"),
    [("windows", "arm64"), ("linux", "amd64"), ("darwin", "arm64")],
)
def test_the_host_is_what_the_binary_reports(monkeypatch: pytest.MonkeyPatch, os_name: str, arch: str):
    _binary_reports(monkeypatch, os_name, arch)

    assert runner.current_platform() == f"{os_name}/{arch}"


def test_the_binary_is_asked_once_per_process(monkeypatch: pytest.MonkeyPatch):
    asked = _binary_reports(monkeypatch, "windows", "arm64")

    assert [runner.current_platform(), runner.current_platform()] == ["windows/arm64"] * 2
    assert asked == [("--offline", "about")]


def test_a_binary_that_cannot_answer_is_an_error_not_a_guess(monkeypatch: pytest.MonkeyPatch):
    def broken(self: runner.OcxRunner, *args: str, **kwargs: object) -> dict:
        raise AssertionError("ocx about failed")

    monkeypatch.setattr(runner.OcxRunner, "json", broken)

    with pytest.raises(AssertionError, match="ocx about failed"):
        runner.current_platform()
