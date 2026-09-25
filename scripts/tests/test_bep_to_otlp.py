"""`bep_to_otlp.py`'s proofs, as pytest.

Each `prove_*` function is its own red/green pair (see the module docstring's
own sections for what each covers); this file only calls them. `prove_no_redirect`
is called both directly and from inside `prove_live_transport` (its own return
value folds it in) — that duplication is harmless, both callers hit real
loopback servers this proof spins up and tears down itself.
"""

from __future__ import annotations

from pathlib import Path

import bep_to_otlp as guard
import pytest

NO_ARG_PROOFS = [
    guard.prove_failed_build,
    guard.prove_cache_state,
    guard.prove_no_secret,
    guard.prove_schema,
    guard.prove_provenance,
    guard.prove_bep_not_persisted,
    guard.prove_counts,
    guard.prove_no_redirect,
]

SCRATCH_PROOFS = [
    guard.prove_dropped_span,
    guard.prove_reader_floor,
    guard.prove_aspect_completion,
    guard.prove_two_invocations,
    guard.prove_silent_exit,
    guard.prove_live_transport,
]


@pytest.mark.parametrize("prove", NO_ARG_PROOFS, ids=lambda f: f.__name__)
def test_proof(prove) -> None:
    assert prove() > 0


@pytest.mark.parametrize("prove", SCRATCH_PROOFS, ids=lambda f: f.__name__)
def test_scratch_proof(prove, tmp_path: Path) -> None:
    assert prove(tmp_path) > 0
