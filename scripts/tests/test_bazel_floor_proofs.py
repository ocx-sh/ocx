"""`bazel_floor_proofs.py`'s proofs, as pytest — C-012 (the floor reads no
run), C-013 (the ceiling from the listing, and a cache hit that is not a
skip), C-013a (target parity) and S-011 each shown red and green."""

from __future__ import annotations

from pathlib import Path

import bazel_floor_proofs as floor
import pytest

NOARG_PROOFS = [
    floor.prove_floor_recipe,
    floor.prove_floor_count,
    floor.prove_ceiling,
    floor.prove_parity,
    floor.prove_counts,
]


@pytest.mark.parametrize("prove", NOARG_PROOFS, ids=lambda f: f.__name__)
def test_proof(prove) -> None:
    assert prove() > 0


def test_cache_neutrality(tmp_path: Path) -> None:
    assert floor.prove_cache_neutrality(tmp_path) > 0
