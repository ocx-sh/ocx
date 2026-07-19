"""Drift-gate acceptance tests for doc scripts.

One pytest case per ``.sh`` file discovered under ``test/doc_scripts/``.
Each case calls ``run_doc_script``, which parses the header, provisions the
declared state, executes the full script body through the Scenario harness,
and optionally diffs output against a golden file.

An empty (or missing) ``doc_scripts/`` directory produces zero cases without
error — the test module must import cleanly at all times (EX7).

Parity with ``test_scenarios_smoke.py``:
- ``pytestmark`` skips all cases on Windows (EX7).
- ``pytest_generate_tests`` drives parametrization via ``discover_doc_scripts``.
- Case IDs are paths relative to ``DOC_SCRIPTS_DIR``.

Design contract reference: design_spec_doc_command_scripts.md
§2 (EX1–EX9, GO1–GO3), §6 (DG1–DG3), §6b (NC1–NC3).
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

from src.doc_scripts import discover_doc_scripts, run_doc_script
from src.helpers import PROJECT_ROOT
from src.runner import OcxRunner

# Shell scenarios target Linux + macOS. Windows behaviour is covered by
# the pytest acceptance suite (see .claude/rules/subsystem-tests.md
# "Platform Split"). Parity with test_scenarios_smoke.py (EX7).
pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="Doc-script drift gate targets Linux/macOS; Windows behaviour covered by the pytest suite.",
)

DOC_SCRIPTS_DIR: Path = PROJECT_ROOT / "test" / "doc_scripts"


def pytest_generate_tests(metafunc: pytest.Metafunc) -> None:
    """Parametrize ``test_doc_script`` over every ``.sh`` file under ``DOC_SCRIPTS_DIR``.

    Empty or missing root ⇒ zero parameters ⇒ zero cases, no error.
    Case IDs are paths relative to ``DOC_SCRIPTS_DIR`` (parity with
    ``test_scenarios_smoke.py``).

    ``patches__consumer.sh`` runs ``ocx patch sync`` with no ``--platform``,
    which (per D4 of `adr_platform_model_unification.md`) also probes the
    registry-wide reserved ``global`` patch-descriptor repository — the same
    shared slot `tests/test_patches.py` serializes itself against via the
    ``patch_global_slot`` xdist group. Join that group here too so this case
    never races a concurrent global-descriptor publish from that module.

    ``user-guide__managed-config-{rollout,publish}.sh`` both push to the
    fixed, non-``unique_repo`` identifier ``corp/ocx-config`` (the on-screen
    example in the docs) — join a shared group so the two never run
    concurrently on different xdist workers and race the same registry repo.
    """
    _SHARED_SLOT_GROUPS = {
        "patches__consumer.sh": "patch_global_slot",
        "user-guide__managed-config-rollout.sh": "managed_config_corp_slot",
        "user-guide__managed-config-publish.sh": "managed_config_corp_slot",
    }
    if "script" in metafunc.fixturenames:
        scripts = discover_doc_scripts(DOC_SCRIPTS_DIR)
        ids = [str(p.relative_to(DOC_SCRIPTS_DIR)) for p in scripts]
        params = [
            pytest.param(script, marks=pytest.mark.xdist_group(_SHARED_SLOT_GROUPS[script.name]))
            if script.name in _SHARED_SLOT_GROUPS
            else script
            for script in scripts
        ]
        metafunc.parametrize("script", params, ids=ids)


def test_doc_script(
    script: Path,
    ocx: OcxRunner,
    tmp_path: Path,
) -> None:
    """Execute a single doc script through the drift-gate executor.

    Delegates entirely to ``run_doc_script``:

    - Parses the header (``DocScriptParseError`` on grammar violation).
    - Resolves the state (``ValueError`` on unknown/unqualified state — EX4).
    - Provisions registry state.
    - Runs the full script body; asserts exit 0 (EX2/EX3, DG1/DG2).
    - Diffs against golden output when ``# expect:`` is set (GO1–GO3).

    Args:
        script: Path to the ``.sh`` file (injected by ``pytest_generate_tests``).
        ocx: ``OcxRunner`` fixture (function-scoped, test-isolated).
        tmp_path: Pytest-provided per-test temp directory.
    """
    run_doc_script(script, ocx, tmp_path)
