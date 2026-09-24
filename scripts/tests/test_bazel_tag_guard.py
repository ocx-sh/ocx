"""`bazel_tag_guard.py`: C-011's clauses, each shown red and green on the live
graph's own query output (see the module docstring for what each proof covers)."""

from __future__ import annotations

from pathlib import Path

import bazel_tag_guard as guard
import pytest

# Two xdist groups, so two workers share the proofs. Each worker pays one live
# capture and one cold derivation of the test tree (about 4 s): one worker for
# all thirteen ran 14 s serially, and thirteen workers ran 15.7 s because every
# one of them paid the cold derivation. Two is the measured optimum.
_A = pytest.mark.xdist_group("bazel_tag_guard_a")
_B = pytest.mark.xdist_group("bazel_tag_guard_b")


@pytest.fixture(scope="module")
def live(bazel: str, tmp_path_factory: pytest.TempPathFactory) -> list[dict]:
    """The live graph, queried once for every proof below."""
    _, records = guard.capture_live(bazel, tmp_path_factory.mktemp("live"))
    return records


GRAPH_PROOFS = [
    pytest.param(guard.prove_no_sandbox, marks=_A, id="prove_no_sandbox"),
    pytest.param(guard.prove_build_action, marks=_B, id="prove_build_action"),
    pytest.param(guard.prove_rust_binary, marks=_B, id="prove_rust_binary"),
    pytest.param(guard.prove_acceptance, marks=_B, id="prove_acceptance"),
    pytest.param(guard.prove_suppressing_tags, marks=_A, id="prove_suppressing_tags"),
    pytest.param(guard.prove_suite_inputs, marks=_B, id="prove_suite_inputs"),
    pytest.param(guard.prove_sweep, marks=_A, id="prove_sweep"),
    pytest.param(guard.prove_uncached, marks=_A, id="prove_uncached"),
    pytest.param(guard.prove_slots, marks=_B, id="prove_slots"),
    pytest.param(guard.prove_hand_reads, marks=_A, id="prove_hand_reads"),
]


@pytest.mark.parametrize("prove", GRAPH_PROOFS)
def test_graph_proof(prove, live: list[dict], tmp_path: Path) -> None:
    assert prove(tmp_path, live) > 0


@_B
def test_floor(live: list[dict], tmp_path: Path, bazel: str) -> None:
    assert guard.prove_floor(tmp_path, live, bazel) > 0


def test_counts() -> None:
    assert guard.prove_counts() > 0


def test_predicate() -> None:
    assert guard.prove_predicate() > 0
