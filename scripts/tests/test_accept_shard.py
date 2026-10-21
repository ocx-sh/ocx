"""`accept_shard.py`'s proofs: the slices partition the suite, and bad input is red."""

from __future__ import annotations

from pathlib import Path

import accept_shard as shard
import pytest


def test_four_shards_partition_the_modules_exactly() -> None:
    stems = [f"test_{c}" for c in "abcdefghij"]
    parts = [shard.select(stems, f"{i}/4") for i in range(1, 5)]
    assert sorted(x for part in parts for x in part) == stems
    assert parts[0] == ["test_a", "test_e", "test_i"]


def test_the_live_tree_is_read_and_partitioned() -> None:
    stems = shard.modules()
    assert len(stems) > 100, "reader floor: the tests/ glob stopped matching"
    union = [x for i in range(1, 8) for x in shard.select(stems, f"{i}/7")]
    assert sorted(union) == stems


@pytest.mark.parametrize("spec", ["0/4", "5/4", "x", "1/", "1/2/3"])
def test_bad_spec_is_red(spec: str, capsys: pytest.CaptureFixture[str]) -> None:
    assert shard.main(["--shard", spec]) == 1
    assert "accept shard:" in capsys.readouterr().err


def test_empty_shard_is_red(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    (tmp_path / "test" / "tests").mkdir(parents=True)
    (tmp_path / "test" / "tests" / "test_only.py").write_text("", encoding="utf-8")
    assert shard.main(["--root", str(tmp_path), "--shard", "2/2"]) == 1
    assert "selects none" in capsys.readouterr().err


def test_formats(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    (tmp_path / "test" / "tests").mkdir(parents=True)
    (tmp_path / "test" / "tests" / "test_only.py").write_text("", encoding="utf-8")
    assert shard.main(["--root", str(tmp_path), "--format", "labels"]) == 0
    assert capsys.readouterr().out == "//test:test_only\n"
    assert shard.main(["--root", str(tmp_path), "--format", "paths"]) == 0
    assert capsys.readouterr().out == "tests/test_only.py\n"
