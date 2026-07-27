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

import re
import sys
from collections.abc import Iterable
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


SHARED_SLOT_GROUPS: dict[str, str] = {
    # `ocx patch sync` with no `--platform` also probes the registry-wide
    # reserved `global` patch-descriptor repository — the same shared slot
    # `tests/test_patches.py` serializes itself against.
    "patches__consumer.sh": "patch_global_slot",
    # Fixed, non-`unique_repo` identifier `corp/ocx-config` (the on-screen
    # example in the docs).
    "user-guide__managed-config-rollout.sh": "managed_config_corp_slot",
    "user-guide__managed-config-publish.sh": "managed_config_corp_slot",
    # Fixed identifier `mytool` — the on-screen name on the authoring pages.
    # Six publisher scripts share it; without a group, two xdist workers push
    # manifests into one repo and the loser sees DIGEST_INVALID /
    # MANIFEST_BLOB_UNKNOWN.
    "package-cascade.sh": "mytool_repo_slot",
    "package-describe.sh": "mytool_repo_slot",
    "package-layer-reuse.sh": "mytool_repo_slot",
    "package-multi-platform.sh": "mytool_repo_slot",
    "package-push.sh": "mytool_repo_slot",
    "package-test.sh": "mytool_repo_slot",
}
"""Doc scripts that touch a registry resource no per-worker prefix isolates.

A script that only *consumes* provisioned packages is isolated for free: it
reaches them through `$PKG_*` / `$REPO_*`, whose values carry the SP7
`t_<8hex>_` prefix. A script that *publishes* under a name spelled out in the
docs bypasses that entirely, so every script pushing to the same fixed name
must share one xdist group and run serially.

Hand-maintained, and `test_fixed_repo_publishers_are_slot_grouped` keeps it
honest for the class it can see. Renaming these repos is not the fix — the
identifier is the artifact the docs page renders.
"""

# `ocx package push -i <ident>` / `ocx config push -i <ident>`. A `$`-prefixed
# value is a provisioned, per-worker-unique repo; anything else is literal.
_PUBLISH_IDENTIFIER = re.compile(r"ocx (?:package|config) push\b[^\n]*?\s(?:-i|--identifier)[= ]\s*(\S+)")


def fixed_repo_publishers(scripts: Iterable[Path]) -> set[str]:
    """Names of scripts that publish under a literal (non-`$`) identifier."""
    return {
        script.name
        for script in scripts
        if any(not ident.startswith("$") for ident in _PUBLISH_IDENTIFIER.findall(script.read_text()))
    }


def test_fixed_repo_publishers_are_slot_grouped() -> None:
    """Every script publishing to a fixed identifier must carry an xdist group.

    The map above is hand-maintained, and six `mytool` publishers sat outside
    it long enough to read as flake rather than as a missing entry. This is the
    check that makes script number seven red instead of intermittent.

    Ceiling: it sees the `push -i <literal>` class only. A script racing a
    reserved name it never names as an identifier — `patches__consumer.sh` and
    the `global` patch-descriptor slot — is invisible here and stays a
    hand-added entry. Detecting *that* would mean knowing which commands touch
    which registry-wide singletons, which is not readable off the script.
    """
    ungrouped = sorted(fixed_repo_publishers(discover_doc_scripts(DOC_SCRIPTS_DIR)) - set(SHARED_SLOT_GROUPS))
    assert not ungrouped, (
        f"doc scripts publish to a fixed identifier but carry no xdist group: {ungrouped}. "
        f"Add each to SHARED_SLOT_GROUPS, sharing one group per identifier, or they will "
        f"race another worker pushing the same repo."
    )


def pytest_generate_tests(metafunc: pytest.Metafunc) -> None:
    """Parametrize ``test_doc_script`` over every ``.sh`` file under ``DOC_SCRIPTS_DIR``.

    Empty or missing root ⇒ zero parameters ⇒ zero cases, no error.
    Case IDs are paths relative to ``DOC_SCRIPTS_DIR`` (parity with
    ``test_scenarios_smoke.py``). Scripts named in ``SHARED_SLOT_GROUPS`` are
    marked into their group so they never run concurrently.
    """
    if "script" in metafunc.fixturenames:
        scripts = discover_doc_scripts(DOC_SCRIPTS_DIR)
        ids = [str(p.relative_to(DOC_SCRIPTS_DIR)) for p in scripts]
        params = [
            pytest.param(script, marks=pytest.mark.xdist_group(SHARED_SLOT_GROUPS[script.name]))
            if script.name in SHARED_SLOT_GROUPS
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
