"""`accept_shard.py`'s proofs: the slices partition the suite, and bad input is red."""

from __future__ import annotations

from pathlib import Path

import accept_shard as shard
import pytest

SHARD_ENV = ("TEST_TOTAL_SHARDS", "TEST_SHARD_INDEX", "TEST_SHARD_STATUS_FILE")


@pytest.fixture(autouse=True)
def _clean_shard_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """The host's own sharding variables must never reach a case."""
    for name in SHARD_ENV:
        monkeypatch.delenv(name, raising=False)


@pytest.fixture
def one_module(tmp_path: Path) -> Path:
    (tmp_path / "test" / "tests").mkdir(parents=True)
    (tmp_path / "test" / "tests" / "test_only.py").write_text("", encoding="utf-8")
    return tmp_path


@pytest.fixture
def ten_modules(tmp_path: Path) -> Path:
    (tmp_path / "test" / "tests").mkdir(parents=True)
    for letter in "abcdefghij":
        (tmp_path / "test" / "tests" / f"test_{letter}.py").write_text("", encoding="utf-8")
    return tmp_path


def test_four_shards_partition_the_modules_exactly() -> None:
    stems = [f"test_{c}" for c in "abcdefghij"]
    parts = [shard.select(stems, 4, i) for i in range(4)]
    assert sorted(x for part in parts for x in part) == stems
    assert parts[0] == ["test_a", "test_e", "test_i"]
    assert parts[3] == ["test_d", "test_h"]


def test_the_live_tree_is_read_and_partitioned() -> None:
    stems = shard.modules()
    assert len(stems) > 100, "reader floor: the tests/ glob stopped matching"
    union = [x for i in range(7) for x in shard.select(stems, 7, i)]
    assert sorted(union) == stems


def test_shard_zero_of_n_is_the_first_slice(ten_modules: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert shard.main(["--root", str(ten_modules), "--shard-count", "4", "--shard-index", "0"]) == 0
    assert capsys.readouterr().out.split() == ["test_a", "test_e", "test_i"]


def test_the_last_shard_is_index_n_minus_one(ten_modules: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert shard.main(["--root", str(ten_modules), "--shard-count", "4", "--shard-index", "3"]) == 0
    assert capsys.readouterr().out.split() == ["test_d", "test_h"]


def test_no_shard_asked_prints_every_module(ten_modules: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert shard.main(["--root", str(ten_modules)]) == 0
    assert len(capsys.readouterr().out.split()) == 10


@pytest.mark.parametrize(
    "argv",
    [
        ["--shard-count", "0", "--shard-index", "0"],
        ["--shard-count", "-1", "--shard-index", "0"],
        ["--shard-count", "4", "--shard-index", "-1"],
        ["--shard-count", "4", "--shard-index", "4"],
        ["--shard-count", "4", "--shard-index", "9"],
        ["--shard-count", "4"],
        ["--shard-index", "1"],
        ["--shard-count", "x", "--shard-index", "0"],
        ["--shard-count", "4", "--shard-index", "1/4"],
    ],
    ids=["n0", "n-neg", "i-neg", "i-eq-n", "i-past-n", "count-only", "index-only", "count-nan", "index-nan"],
)
def test_bad_flags_exit_two(argv: list[str], capsys: pytest.CaptureFixture[str]) -> None:
    assert shard.main(argv) == 2
    assert "accept shard:" in capsys.readouterr().err


@pytest.mark.parametrize(
    "env",
    [
        {"TEST_TOTAL_SHARDS": "0", "TEST_SHARD_INDEX": "0"},
        {"TEST_TOTAL_SHARDS": "4", "TEST_SHARD_INDEX": "-1"},
        {"TEST_TOTAL_SHARDS": "4", "TEST_SHARD_INDEX": "4"},
        {"TEST_TOTAL_SHARDS": "4"},
        {"TEST_SHARD_INDEX": "1"},
        {"TEST_TOTAL_SHARDS": "four", "TEST_SHARD_INDEX": "1"},
        {"TEST_TOTAL_SHARDS": "4", "TEST_SHARD_INDEX": ""},
    ],
    ids=["n0", "i-neg", "i-eq-n", "count-only", "index-only", "count-nan", "index-empty"],
)
def test_bad_env_exits_two(
    env: dict[str, str], monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    for name, value in env.items():
        monkeypatch.setenv(name, value)
    assert shard.main([]) == 2
    assert "accept shard:" in capsys.readouterr().err


def test_the_env_pair_selects_the_slice(
    ten_modules: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("TEST_TOTAL_SHARDS", "4")
    monkeypatch.setenv("TEST_SHARD_INDEX", "1")
    assert shard.main(["--root", str(ten_modules)]) == 0
    assert capsys.readouterr().out.split() == ["test_b", "test_f", "test_j"]


def test_flags_win_over_the_env(
    ten_modules: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("TEST_TOTAL_SHARDS", "2")
    monkeypatch.setenv("TEST_SHARD_INDEX", "1")
    assert shard.main(["--root", str(ten_modules), "--shard-count", "4", "--shard-index", "0"]) == 0
    assert capsys.readouterr().out.split() == ["test_a", "test_e", "test_i"]


def test_one_flag_is_not_completed_from_the_env(
    ten_modules: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("TEST_TOTAL_SHARDS", "4")
    monkeypatch.setenv("TEST_SHARD_INDEX", "1")
    assert shard.main(["--root", str(ten_modules), "--shard-count", "4"]) == 2
    assert "accept shard:" in capsys.readouterr().err


def test_the_status_file_is_touched_when_the_env_names_one(
    ten_modules: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    status = tmp_path / "status" / "shard-status"
    status.parent.mkdir()
    monkeypatch.setenv("TEST_SHARD_STATUS_FILE", str(status))
    assert not status.exists()
    assert shard.main(["--root", str(ten_modules), "--shard-count", "4", "--shard-index", "2"]) == 0
    assert status.is_file()


def test_the_status_file_is_left_alone_when_no_env_names_one(ten_modules: Path, tmp_path: Path) -> None:
    assert shard.main(["--root", str(ten_modules), "--shard-count", "4", "--shard-index", "2"]) == 0
    assert list(tmp_path.glob("**/shard-status")) == []


def test_empty_shard_is_red(one_module: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert shard.main(["--root", str(one_module), "--shard-count", "2", "--shard-index", "1"]) == 1
    assert "selects none" in capsys.readouterr().err


def test_no_module_at_all_is_red(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    (tmp_path / "test" / "tests").mkdir(parents=True)
    assert shard.main(["--root", str(tmp_path)]) == 1
    assert "no acceptance module" in capsys.readouterr().err


def test_the_one_based_i_over_n_flag_is_gone() -> None:
    """The old `--shard i/n` form is deleted outright, not aliased."""
    with pytest.raises(SystemExit) as exit_info:
        shard.main(["--shard", "1/4"])
    assert exit_info.value.code == 2


def test_formats(one_module: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert shard.main(["--root", str(one_module), "--format", "labels"]) == 0
    assert capsys.readouterr().out == "//test:test_only\n"
    assert shard.main(["--root", str(one_module), "--format", "paths"]) == 0
    assert capsys.readouterr().out == "tests/test_only.py\n"
    assert shard.main(["--root", str(one_module), "--format", "stems"]) == 0
    assert capsys.readouterr().out == "test_only\n"
