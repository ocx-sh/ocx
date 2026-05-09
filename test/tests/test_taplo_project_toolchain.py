# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Phase 10 specification tests for taplo's auto-completion wiring on
``ocx.toml``.

The repo-root ``taplo.toml`` is supposed to map ``**/ocx.toml`` to
``https://ocx.sh/schemas/project/v1.json``, which gives editors with a
taplo plugin (helix, vscode, neovim) auto-completion + validation for
project-toolchain configs. This test catches schema URL drift in the
config by running ``taplo check`` (the canonical CLI command) against:

1. a valid ``ocx.toml`` fixture (must exit 0);
2. a malformed ``ocx.toml`` with a non-string ``[tools]`` value (must
   surface a type-mismatch — schema is fail-loud).

Plan reference: ``.claude/state/plans/plan_project_toolchain.md`` lines
859–873 (Phase 10 deliverable 2 — taplo auto-completion).

Operational notes
-----------------
``taplo`` is not currently pinned through the OCX index in this
worktree. The tests detect availability and ``pytest.skip`` when the
binary is absent, mirroring the pattern in
``test/tests/test_assembly.py`` (skip on missing host tools). To run
locally, pin via::

    ocx index update taplo
    ocx install --select taplo

The test file lives in ``test/tests/`` rather than as a Rust unit test
because the contract is the on-disk ``taplo.toml``, not any internal
type — and taplo itself is the consumer that proves the config wires
through correctly.
"""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parents[2]
TAPLO_CONFIG = PROJECT_ROOT / "taplo.toml"


def _taplo_path() -> str | None:
    """Return the path to a taplo binary if one is on PATH, else None.

    Future improvement: also probe ``$OCX_HOME/symlinks`` for a pinned
    taplo install, but that requires the OCX runner — not a fit here
    where the tests are pure file/subprocess plumbing.
    """
    return shutil.which("taplo")


@pytest.fixture(scope="module")
def taplo_binary() -> str:
    """Skip the entire module if taplo is unavailable."""
    path = _taplo_path()
    if path is None:
        pytest.skip(
            "taplo not available — pin via "
            "`ocx index update taplo && ocx install --select taplo` "
            "to enable this test"
        )
    return path


def test_taplo_config_targets_project_schema_url() -> None:
    """Pure file check: the taplo.toml rule for ``**/ocx.toml`` must
    point to the canonical project schema URL. Runs even when taplo
    itself is missing — guards against schema URL drift.
    """
    assert TAPLO_CONFIG.exists(), f"taplo.toml missing at {TAPLO_CONFIG}"
    text = TAPLO_CONFIG.read_text(encoding="utf-8")

    # The rule must exist as a literal include + url pair. Substring
    # checks are intentional — exact format depends on toml authoring
    # style and we don't want to lock to a single layout.
    assert '"**/ocx.toml"' in text, (
        "taplo.toml must declare an include rule for `**/ocx.toml` "
        "(plan Phase 10 deliverable 2)"
    )
    assert "https://ocx.sh/schemas/project/v1.json" in text, (
        "taplo.toml must point `**/ocx.toml` at the canonical project "
        "schema URL `https://ocx.sh/schemas/project/v1.json`"
    )


def test_taplo_check_accepts_valid_ocx_toml(
    taplo_binary: str, tmp_path: Path
) -> None:
    """taplo must validate a well-formed ``ocx.toml`` against its schema.

    The fixture has a registry-qualified identifier; bare-tag forms are
    rejected by the project loader and would also fail schema validation
    once the schema's `Identifier` regex is tightened (deferred).
    """
    fixture = tmp_path / "ocx.toml"
    fixture.write_text(
        "[tools]\n"
        'cmake = "ocx.sh/cmake:3.28"\n'
        'ripgrep = "ocx.sh/ripgrep:14"\n',
        encoding="utf-8",
    )

    # `taplo check` discovers the workspace's taplo.toml by walking up
    # from the file under check; we run with cwd = PROJECT_ROOT so the
    # project taplo.toml is found.
    result = subprocess.run(
        [taplo_binary, "check", str(fixture)],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, (
        f"taplo check rejected a valid ocx.toml fixture (exit "
        f"{result.returncode}):\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}\n"
        f"fixture content:\n{fixture.read_text()}"
    )


def test_taplo_check_rejects_integer_tool_value(
    taplo_binary: str, tmp_path: Path
) -> None:
    """Negative case: ``[tools] cmake = 3`` is a type-mismatch (integer
    instead of string). taplo must report it. Catches a schema-fail-loud
    regression — a permissive schema would silently accept the bad
    value.
    """
    fixture = tmp_path / "ocx.toml"
    fixture.write_text("[tools]\ncmake = 3\n", encoding="utf-8")

    result = subprocess.run(
        [taplo_binary, "check", str(fixture)],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0, (
        "taplo check accepted an integer-valued tool entry — schema "
        "must enforce string typing on `[tools]` values\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
