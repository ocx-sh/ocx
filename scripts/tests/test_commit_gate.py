"""`commit_gate.py`'s self-test cases, as pytest — see the module docstring.

Every case drives real git through a real shell, armed with `prek`; needs
`prek` on PATH, as `task scripts:self-test` and `task git:hooks` give it.
"""

from __future__ import annotations

import tempfile
from pathlib import Path

import commit_gate as gate
import pytest

with tempfile.TemporaryDirectory() as _tmp:
    _NAMES = [name for name, _ in gate._self_test_cases(Path(_tmp))]


@pytest.mark.parametrize("name", _NAMES, ids=_NAMES)
def test_case(name: str, tmp_path: Path) -> None:
    cases = dict(gate._self_test_cases(tmp_path))
    problem = cases[name]()
    assert problem is None, problem
