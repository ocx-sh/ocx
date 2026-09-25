"""`lint_ratchet.py`: the ratchet's per-clause proofs, each shown red and
green on synthetic and real-workspace fixtures (see the script's module
docstring for what each proof covers)."""

from __future__ import annotations

from pathlib import Path

import lint_ratchet as ratchet
import pytest

NO_ARG_PROOFS = [
    ratchet.prove_count_codes_dedup_and_censuses,
    ratchet.prove_count_codes_requires_a_real_stream,
    ratchet.prove_is_member,
    ratchet.prove_compare,
    ratchet.prove_by_file,
    ratchet.prove_raised,
    ratchet.prove_workspace_members,
    ratchet.prove_end_to_end_fixture_sanity,
    ratchet.prove_finished_census_three_states,
    ratchet.prove_label_member,
]

TMP_PATH_PROOFS = [
    ratchet.prove_run_reds_a_truncated_stream,
    ratchet.prove_run_reds_a_summary_only_stream,
    ratchet.prove_run_reds_a_failed_build,
    ratchet.prove_run_reds_a_dark_member,
    ratchet.prove_run_update_gate,
    ratchet.prove_bep_fail_closed,
    ratchet.prove_bep_gate_semantics,
    ratchet.prove_bep_dark_member,
    ratchet.prove_bep_target_coverage,
    ratchet.prove_unread_lints,
    ratchet.prove_allow_codes,
]


@pytest.mark.parametrize("prove", NO_ARG_PROOFS, ids=lambda f: f.__name__)
def test_proof(prove) -> None:
    assert prove() > 0


@pytest.mark.parametrize("prove", TMP_PATH_PROOFS, ids=lambda f: f.__name__)
def test_proof_over_tmp_path(prove, tmp_path: Path) -> None:
    assert prove(tmp_path) > 0
