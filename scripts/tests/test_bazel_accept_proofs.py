"""`bazel_accept_proofs.py`'s proofs — S-015, S-004, C-029, the live table
floors and the runner's host locks — each shown red and green, as pytest."""

from __future__ import annotations

from pathlib import Path

import bazel_accept_proofs as proofs
import pytest

# Proofs that build their own fixtures under a scratch directory.
SCRATCH_PROOFS = [
    proofs.prove_caching_on,
    proofs.prove_binary_digest,
    proofs.prove_selection,
    proofs.prove_c029,
    proofs.prove_runner_locks,
]

# Proofs that read only already-checked-in repo state.
NOARG_PROOFS = [
    proofs.prove_live_table,
    proofs.prove_entry_points,
    proofs.prove_uncached_list,
    proofs.prove_counts,
]


@pytest.mark.parametrize("prove", SCRATCH_PROOFS, ids=lambda f: f.__name__)
def test_scratch_proof(prove, tmp_path: Path) -> None:
    assert prove(tmp_path) > 0


@pytest.mark.parametrize("prove", NOARG_PROOFS, ids=lambda f: f.__name__)
def test_proof(prove) -> None:
    assert prove() > 0
