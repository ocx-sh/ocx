"""`bazel_doctor.py`'s checks, each shown red and green on fixtures built here
(the module docstring covers what each check judges)."""

from __future__ import annotations

import tempfile
from pathlib import Path

import bazel_doctor as doctor
import pytest
from bazel_gate_proofs import REPO_ROOT

SCRATCH_PROOFS = [
    doctor.prove_host_block,
    doctor.prove_probe,
    doctor.prove_secret,
    doctor.prove_bep_modes,
]


@pytest.fixture
def repo_scratch() -> Path:
    """A scratch dir under this repo's own `.tmp/`, never under `/tmp` — a
    tmpfs on this host, and the very thing `prove_host_block` asserts a doctor
    fixture is NOT under. pytest's own `tmp_path` resolves under `/tmp` here,
    so it cannot stand in for this one."""
    root = REPO_ROOT / ".tmp"
    root.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=root) as directory:
        yield Path(directory)


@pytest.mark.parametrize("prove", SCRATCH_PROOFS, ids=lambda f: f.__name__)
def test_scratch_proof(prove, repo_scratch: Path) -> None:
    assert prove(repo_scratch) > 0


def test_rc_reading() -> None:
    assert doctor.prove_rc_reading() > 0


def test_pure_checks() -> None:
    assert doctor.prove_pure_checks() > 0
