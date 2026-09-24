"""`bazel_pin_check.py`'s two gates (C-003 pin drift, C-026 rust toolchain
twin) and its `--check` entry point, each shown red and green on fixtures
built here."""

from __future__ import annotations

from pathlib import Path

import bazel_pin_check as pin_check
import pytest

PROOFS = [
    pin_check.prove_pin_gate,
    pin_check.prove_toolchain_gate,
    pin_check.prove_entry_point,
]


@pytest.mark.parametrize("prove", PROOFS, ids=lambda f: f.__name__)
def test_proof(prove, tmp_path: Path) -> None:
    assert prove(tmp_path) > 0
