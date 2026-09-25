"""`lint_ratchet.py`: the ratchet's per-clause proofs, each shown red and
green on synthetic and real-workspace fixtures (see the script's module
docstring for what each proof covers)."""

from __future__ import annotations

from pathlib import Path

import lint_ratchet as ratchet
import pytest

NO_ARG_PROOFS = [
    ratchet.prove_compare,
    ratchet.prove_by_file,
    ratchet.prove_raised,
    ratchet.prove_workspace_members,
    ratchet.prove_label_member,
]

TMP_PATH_PROOFS = [
    ratchet.prove_bep_update_gate,
    ratchet.prove_bep_fail_closed,
    ratchet.prove_bep_gate_semantics,
    ratchet.prove_bep_dark_member,
    ratchet.prove_bep_target_coverage,
    ratchet.prove_unread_lints,
    ratchet.prove_allow_codes,
    ratchet.prove_rustdoc_allow_codes,
]


@pytest.mark.parametrize("prove", NO_ARG_PROOFS, ids=lambda f: f.__name__)
def test_proof(prove) -> None:
    assert prove() > 0


@pytest.mark.parametrize("prove", TMP_PATH_PROOFS, ids=lambda f: f.__name__)
def test_proof_over_tmp_path(prove, tmp_path: Path) -> None:
    assert prove(tmp_path) > 0
