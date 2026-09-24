"""`scoped_gate.py`'s self-test cases, as pytest — see the module docstring."""

from __future__ import annotations

import tempfile
from pathlib import Path

import pytest
import scoped_gate as gate

with tempfile.TemporaryDirectory() as _tmp:
    _NAMES = [name for name, _ in gate._self_test_cases(Path(_tmp))]


@pytest.mark.parametrize("name", _NAMES, ids=_NAMES)
def test_case(name: str, tmp_path: Path) -> None:
    cases = dict(gate._self_test_cases(tmp_path))
    problem = cases[name]()
    assert problem is None, problem
