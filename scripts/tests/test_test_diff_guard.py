"""`test_diff_guard.py`'s proofs, as pytest.

`SELF_TEST_CASES` is the shape table: each `Case` builds its own throwaway git
repository (via `_run_case`) and is judged red or green against `expect_red`.
The probe functions are the control group beside it — see each one's own
docstring for what a stuck-at-one-answer reader would let through unnoticed.
Nothing is restored with `git checkout --`; every repository lives under
pytest's own `tmp_path` and is thrown away with it.
"""

from __future__ import annotations

from pathlib import Path

import pytest
import test_diff_guard as guard


@pytest.mark.parametrize("case", guard.SELF_TEST_CASES, ids=lambda c: c.name)
def test_case(case: guard.Case, tmp_path: Path) -> None:
    red, problems = guard._run_case(case, tmp_path)
    assert red == case.expect_red, "\n".join(problems) or "no problems reported"


def test_floor_probes(tmp_path: Path) -> None:
    for name, ok, detail in guard._floor_probes(tmp_path):
        assert ok, f"{name}: {detail}"


def test_collect_probes(tmp_path: Path) -> None:
    for name, ok, detail in guard._collect_probes(tmp_path):
        assert ok, f"{name}: {detail}"


def test_live_lint_conftest() -> None:
    name, ok, detail = guard._live_lint_conftest_probe()
    assert ok, f"{name}: {detail}"
