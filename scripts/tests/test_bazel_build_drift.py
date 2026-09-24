"""`bazel_build_drift.py`'s proofs, as pytest — C-010 shown red and green on all
three failure modes (a dropped first-party edge, a dropped @crates// edge, an
unmapped label), on both directions, and on every reader floor. No subprocess
and no built graph: the fixtures are `sample_tree()`'s own text."""

from __future__ import annotations

from pathlib import Path

import bazel_build_drift as drift
import pytest


@pytest.fixture(scope="module")
def sample() -> tuple[str, dict]:
    return drift.sample_tree()


SAMPLE_PROOFS = [
    drift.prove_readers,
    drift.prove_drift_gate,
    drift.prove_floors,
]


@pytest.mark.parametrize("prove", SAMPLE_PROOFS, ids=lambda f: f.__name__)
def test_sample_proof(prove, sample: tuple[str, dict]) -> None:
    text, metadata = sample
    assert prove(text, metadata) > 0


def test_map_reader(tmp_path: Path) -> None:
    assert drift.prove_map_reader(tmp_path) > 0


def test_shipped_map() -> None:
    assert drift.prove_shipped_map() > 0
