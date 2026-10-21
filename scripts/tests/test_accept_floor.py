"""`accept_floor.py`'s proofs, on synthetic JUnit shards and synthetic limits."""

from __future__ import annotations

from pathlib import Path

import accept_floor as floor
import pytest


def _limits(tmp_path: Path, suite: int = 8, skip: int = 1, xfail: int = 1) -> Path:
    d = tmp_path / "limits"
    d.mkdir(exist_ok=True)
    for name, value in (("SUITE_FLOOR", suite), ("SKIP_CEILING", skip), ("XFAIL_CEILING", xfail)):
        (d / name).write_text(f"{value}\n", encoding="utf-8")
    return d


def _shard(tmp_path: Path, name: str, passed: int, extra: str = "") -> None:
    cases = "".join(f'<testcase classname="t" name="p{i}"/>' for i in range(passed))
    out = tmp_path / "junit" / f"{name}.xml"
    out.parent.mkdir(exist_ok=True)
    out.write_text(f"<testsuites><testsuite>{cases}{extra}</testsuite></testsuites>", encoding="utf-8")


def _four_shards(tmp_path: Path) -> None:
    for i in range(1, 5):
        _shard(tmp_path, f"shard-{i}-of-4", 2)


def _run(tmp_path: Path, **limits: int) -> int:
    return floor.run(tmp_path / "junit", _limits(tmp_path, **limits))


def test_union_of_four_shards_is_green(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    _four_shards(tmp_path)
    assert _run(tmp_path) == 0
    assert "8 cases (floor 8)" in capsys.readouterr().out


def test_dropping_a_shard_reds_the_suite_floor(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    _four_shards(tmp_path)
    (tmp_path / "junit" / "shard-4-of-4.xml").unlink()
    assert _run(tmp_path) == 1
    assert "6 cases ran, test/SUITE_FLOOR is 8" in capsys.readouterr().err


def test_one_failure_reds(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    _four_shards(tmp_path)
    _shard(tmp_path, "shard-4-of-4", 2, '<testcase name="bad"><failure message="x"/></testcase>')
    assert _run(tmp_path) == 1
    assert "1 failed or errored" in capsys.readouterr().err


def test_error_reds(tmp_path: Path) -> None:
    _four_shards(tmp_path)
    _shard(tmp_path, "shard-4-of-4", 2, '<testcase name="bad"><error message="x"/></testcase>')
    assert _run(tmp_path) == 1


def test_skip_over_ceiling_reds_but_xfail_is_counted_apart(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    _four_shards(tmp_path)
    skips = '<testcase name="s1"><skipped/></testcase><testcase name="s2"><skipped/></testcase>'
    _shard(tmp_path, "shard-4-of-4", 2, skips)
    assert _run(tmp_path) == 1
    assert "2 skipped exceeds test/SKIP_CEILING (1)" in capsys.readouterr().err
    xf = '<testcase name="x1"><skipped type="pytest.xfail"/></testcase>'
    _shard(tmp_path, "shard-4-of-4", 2, xf)
    assert _run(tmp_path) == 0


def test_empty_dir_reds_the_reader_floor(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    (tmp_path / "junit").mkdir()
    assert _run(tmp_path) == 1
    assert "nothing was read" in capsys.readouterr().err


def test_expect_shards_reds_naming_the_missing_report(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    _four_shards(tmp_path)
    (tmp_path / "junit" / "shard-3-of-4.xml").unlink()
    _shard(tmp_path, "shard-9-of-9", 2)  # enough cases for the floor: only the name check can red
    assert floor.run(tmp_path / "junit", _limits(tmp_path, suite=6), 4) == 1
    assert "shard-3-of-4.xml" in capsys.readouterr().err


def test_expect_shards_green_when_all_present_extras_tolerated(tmp_path: Path) -> None:
    _four_shards(tmp_path)
    _shard(tmp_path, "extra", 0)
    assert floor.run(tmp_path / "junit", _limits(tmp_path), 4) == 0
