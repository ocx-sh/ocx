"""`bazel_hermeticity_proofs.py`'s comparators, each shown red and green on
fixtures built here (the module docstring covers what each proof measures)."""

from __future__ import annotations

from pathlib import Path

import bazel_hermeticity_proofs as hermeticity
import pytest

SCRATCH_PROOFS = [
    hermeticity.prove_reader,
    hermeticity.prove_declared,
    hermeticity.prove_ambient,
    hermeticity.prove_tool_pin,
    hermeticity.prove_c029,
]


@pytest.mark.parametrize("prove", SCRATCH_PROOFS, ids=lambda f: f.__name__)
def test_scratch_proof(prove, tmp_path: Path) -> None:
    assert prove(tmp_path) > 0


def test_sandbox_waiver() -> None:
    assert hermeticity.prove_sandbox_waiver() > 0


def test_counts() -> None:
    assert hermeticity.prove_counts() > 0
