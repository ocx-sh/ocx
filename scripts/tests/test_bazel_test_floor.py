"""`bazel_test_floor.py`'s proofs, as pytest — the floor shown red on a
deleted target, on three tests deleted inside a surviving target, on a
stopped reader, on a mute log and on an empty stream, and green on the live
universe; the skip ceiling shown green with the `rust_doc_test` targets' `ignore`
fences left out and red on a non-doctest target over it (C-034); `--junit` shown red on a mangled per-case parser (one target and
all of them), on an unreadable log and on a target Bazel failed while its
cases passed, and green at exactly the recorded case count with a failure
named and its panic attached."""

from __future__ import annotations

from pathlib import Path

import bazel_test_floor as floor
import pytest


@pytest.fixture(scope="module")
def rows() -> list[floor.Row]:
    rows_ = floor.read_rows(floor.TEST_TARGET_MAP)
    assert rows_, f"{floor.TEST_TARGET_MAP} has no `[[target]]` rows — the fixture would be empty"
    return rows_


def test_counts(rows: list[floor.Row]) -> None:
    assert floor.prove_counts(rows) > 0


def test_synthesised_xml_is_not_the_count() -> None:
    assert floor.prove_synthesised_xml_is_not_the_count() > 0


def test_floor(rows: list[floor.Row], tmp_path: Path) -> None:
    assert floor.prove_floor(tmp_path, rows) > 0


def test_ceiling(rows: list[floor.Row], tmp_path: Path) -> None:
    assert floor.prove_ceiling(tmp_path, rows) > 0


def test_junit(rows: list[floor.Row], tmp_path: Path) -> None:
    assert floor.prove_junit(tmp_path, rows) > 0
