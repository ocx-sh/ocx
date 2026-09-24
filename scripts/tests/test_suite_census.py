"""`suite_census.py`: the four census definitions and the ratchet gate, shown
red and green on suites built under `tmp_path` (see the script's docstring for
what each proof covers)."""

from __future__ import annotations

from collections import Counter
from pathlib import Path

import pytest
import suite_census as census_mod

TEST_A_SOURCE = (
    "import pytest\n"
    "\n"
    "@pytest.mark.parametrize('n', [1, 2])\n"
    "def test_one(n):\n"
    "    assert n\n"
    "\n"
    "@pytest.mark.skipif(True, reason='x')\n"
    "def test_two():\n"
    "    pass\n"
    "\n"
    "@pytest.mark.xfail\n"
    "async def test_three():\n"
    "    pass\n"
    "\n"
    "class TestGroup:\n"
    "    def test_method(self):\n"
    "        pass\n"
    "\n"
    "def helper():\n"
    "    pytest.skip('runtime')\n"
)

EXPECTED_COUNTS = Counter({"tests": 4, "skipped": 1, "xfailed": 1, "parametrized": 1})


@pytest.fixture
def suite(tmp_path: Path) -> Path:
    (tmp_path / "tests").mkdir()
    (tmp_path / "tests" / "test_a.py").write_text(TEST_A_SOURCE, encoding="utf-8")
    return tmp_path


@pytest.fixture
def real_suite(suite: Path) -> tuple[Path, dict[str, int]]:
    """`suite` padded past `MIN_FILES`, with its matching pinned census."""
    counts, _ = census_mod.census(suite)
    pinned = dict(counts)
    for index in range(census_mod.MIN_FILES):
        (suite / "tests" / f"test_pad_{index}.py").write_text(
            "def test_pad():\n    pass\n", encoding="utf-8"
        )
    pinned["tests"] += census_mod.MIN_FILES
    return suite, pinned


def test_census_counts_the_four_definitions(suite: Path) -> None:
    counts, files = census_mod.census(suite)
    assert counts == EXPECTED_COUNTS
    assert len(files) == 1


def test_dot_directories_are_excluded(suite: Path) -> None:
    # THE EXCLUSION: `test/.venv` holds more `.py` files than the suite does, so
    # a walk that took the whole tree would count uv's virtualenv and move
    # whenever a dependency did.
    before_counts, before_files = census_mod.census(suite)
    (suite / ".venv").mkdir()
    (suite / ".venv" / "test_vendored.py").write_text(
        "def test_from_a_dependency():\n    pass\n", encoding="utf-8"
    )
    after_counts, after_files = census_mod.census(suite)
    assert after_counts == before_counts
    assert len(after_files) == len(before_files)


def test_check_reds_under_min_files(suite: Path) -> None:
    counts, _ = census_mod.census(suite)
    assert census_mod.check(suite, dict(counts)) == 1


def test_check_passes_a_matching_real_sized_suite(
    real_suite: tuple[Path, dict[str, int]],
) -> None:
    suite, pinned = real_suite
    assert census_mod.check(suite, pinned) == 0


def test_check_passes_when_a_test_is_added(real_suite: tuple[Path, dict[str, int]]) -> None:
    # A test ADDED is ordinary work: `tests` is a floor, so a rise must pass it.
    suite, pinned = real_suite
    (suite / "tests" / "test_smuggled.py").write_text(
        "def test_added_behaviour():\n    assert True\n", encoding="utf-8"
    )
    assert census_mod.check(suite, pinned) == 0


def test_check_reds_when_a_test_is_removed(real_suite: tuple[Path, dict[str, int]]) -> None:
    # `tests` is a floor: the suite may never shrink.
    suite, pinned = real_suite
    (suite / "tests" / "test_pad_0.py").unlink()
    assert census_mod.check(suite, pinned) == 1


def test_check_passes_again_once_restored(real_suite: tuple[Path, dict[str, int]]) -> None:
    suite, pinned = real_suite
    (suite / "tests" / "test_pad_0.py").unlink()
    (suite / "tests" / "test_pad_0.py").write_text("def test_pad():\n    pass\n", encoding="utf-8")
    assert census_mod.check(suite, pinned) == 0


def test_check_reds_on_a_newly_skipped_test(real_suite: tuple[Path, dict[str, int]]) -> None:
    # The ceiling catches what the floor cannot: a test still there and no
    # longer running.
    suite, pinned = real_suite
    (suite / "tests" / "test_smuggled.py").write_text(
        "import pytest\n\n@pytest.mark.skip\ndef test_added_behaviour():\n    assert True\n",
        encoding="utf-8",
    )
    assert census_mod.check(suite, pinned) == 1
