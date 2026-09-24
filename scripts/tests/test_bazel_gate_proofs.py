"""`bazel_gate_proofs.py`: S-002/S-003/S-009/S-010/S-012 plus C-029 and the
floor arithmetic, each shown red and green on fixtures the script itself owns
(see the module docstring for what each proof covers)."""

from __future__ import annotations

from pathlib import Path

import bazel_gate_proofs as proofs
import pytest

SCRATCH_PROOFS = [
    proofs.prove_pin,
    proofs.prove_a1,
    proofs.prove_c029,
]


@pytest.mark.parametrize("prove", SCRATCH_PROOFS, ids=lambda f: f.__name__)
def test_scratch_proof(prove, tmp_path: Path) -> None:
    assert prove(tmp_path) > 0


def test_drift() -> None:
    assert proofs.prove_drift() > 0


def test_tags() -> None:
    assert proofs.prove_tags() > 0


def test_counts() -> None:
    assert proofs.prove_counts() > 0
