# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx update`` (plan Phase 5).

Tests trace one-to-one to the four spec bullets in plan Phase 5:

1. ``ocx update cmake``               — only the ``cmake`` entry changes
2. ``ocx update`` (no args)           — full re-resolution, equivalent to ``ocx lock``
3. ``ocx update nonexistent``         — ``NotFound`` (79), no lock written
4. ``ocx update --group ci``          — only ci-group tools change

Specification mode (contract-first TDD)
---------------------------------------
All tests run against the current Phase 5 stub. Both the CLI command
(``command/update.rs``) and the library helper
(``project/resolve.rs::resolve_lock_partial``) call ``unimplemented!()``.
Every test is therefore expected to FAIL against the stub — the
contract they encode is the Phase 5 implementation target.
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner


EXIT_SUCCESS = 0
EXIT_NOT_FOUND = 79


def _ocx_cmd(ocx: OcxRunner, *args: str) -> list[str]:
    return [str(ocx.binary), *args]


def _run_lock(ocx: OcxRunner, cwd: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        _ocx_cmd(ocx, "lock", *extra),
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _run_update(ocx: OcxRunner, cwd: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        _ocx_cmd(ocx, "update", *extra),
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


def _read_lock_text(project_dir: Path) -> str:
    return (project_dir / "ocx.lock").read_text()


_PINNED_RE = re.compile(r'pinned\s*=\s*"([^"@]+)@sha256:([0-9a-f]{64})"')


def _pinned_for(lock_text: str, name: str) -> str | None:
    """Return the pinned digest hex for the ``[[tool]]`` entry whose
    ``name`` field equals ``name``; ``None`` when no entry matches.
    """
    pat = re.compile(
        rf'\[\[tool\]\]\s*\nname\s*=\s*"{re.escape(name)}"\s*\n'
        rf'group\s*=\s*"[^"]+"\s*\n'
        rf'pinned\s*=\s*"[^"@]+@sha256:([0-9a-f]{{64}})"',
        re.MULTILINE,
    )
    m = pat.search(lock_text)
    return m.group(1) if m else None


def _declaration_hash(lock_text: str) -> str:
    m = re.search(r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', lock_text)
    assert m is not None, "declaration_hash missing"
    return m.group(1)


def _generated_at(lock_text: str) -> str:
    m = re.search(r'generated_at\s*=\s*"([^"]+)"', lock_text)
    assert m is not None, "generated_at missing"
    return m.group(1)


# ---------------------------------------------------------------------------
# 1. ``ocx update <name>`` — only the named entry changes
# ---------------------------------------------------------------------------


def test_update_named_tool_rewrites_only_that_entry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update a`` after swapping ``a``'s tag → only ``a``'s pinned
    digest changes; ``b``'s entry stays byte-identical to the prior lock.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_upd_a"
    repo_b = f"t_{short}_upd_b"

    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_a, "2.0.0", tmp_path, new=False, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:1.0.0"
b = "{ocx.registry}/{repo_b}:1.0.0"
""",
    )

    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    initial_text = _read_lock_text(project)
    initial_a = _pinned_for(initial_text, "a")
    initial_b = _pinned_for(initial_text, "b")
    assert initial_a is not None and initial_b is not None

    # Swap 'a' tag in ocx.toml then run `ocx update a`.
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:2.0.0"
b = "{ocx.registry}/{repo_b}:1.0.0"
""",
    )

    result = _run_update(ocx, project, "a")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update a failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    after_text = _read_lock_text(project)
    after_a = _pinned_for(after_text, "a")
    after_b = _pinned_for(after_text, "b")
    assert after_a is not None and after_b is not None

    assert after_a != initial_a, "'a' digest must change after `ocx update a`"
    assert after_b == initial_b, "'b' digest must be unchanged when not selected"


# ---------------------------------------------------------------------------
# 2. ``ocx update`` (no args) — full re-resolution, equivalent to ``ocx lock``
# ---------------------------------------------------------------------------


def test_update_no_args_full_resolution_equivalent_to_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update`` with no args on an unchanged ``ocx.toml`` → produces
    a byte-identical lock to a fresh ``ocx lock`` run from scratch.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_full_a"
    repo_b = f"t_{short}_full_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "2.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
a = "{ocx.registry}/{repo_a}:1.0.0"
b = "{ocx.registry}/{repo_b}:2.0.0"
""",
    )

    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    initial_text = _read_lock_text(project)
    initial_pinned = sorted(_PINNED_RE.findall(initial_text))
    initial_hash = _declaration_hash(initial_text)

    # `ocx update` (no args) re-resolves everything against the same
    # ocx.toml — every digest must match the initial lock and the
    # declaration_hash must be unchanged.
    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_text = _read_lock_text(project)
    after_pinned = sorted(_PINNED_RE.findall(after_text))
    after_hash = _declaration_hash(after_text)

    assert after_pinned == initial_pinned, (
        "no-args `ocx update` must keep every tool's pinned digest equal to `ocx lock`"
    )
    assert after_hash == initial_hash, (
        "declaration_hash must be unchanged when ocx.toml has not changed"
    )


# ---------------------------------------------------------------------------
# 3. ``ocx update <unknown>`` — exit 79, no lock changes
# ---------------------------------------------------------------------------


def test_update_unknown_binding_exits_79_no_lock_change(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update nonexistent`` when ``nonexistent`` is not declared in
    ``ocx.toml`` → exit 79 (NotFound); the existing ``ocx.lock`` is left
    untouched (byte-identical).
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_unknown"
    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:1.0.0"
""",
    )

    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    before_bytes = (project / "ocx.lock").read_bytes()

    result = _run_update(ocx, project, "nonexistent")
    assert result.returncode == EXIT_NOT_FOUND, (
        f"expected exit {EXIT_NOT_FOUND}; got {result.returncode}\nstderr:\n{result.stderr}"
    )

    after_bytes = (project / "ocx.lock").read_bytes()
    assert after_bytes == before_bytes, (
        "ocx.lock must NOT be rewritten when an unknown binding is passed"
    )


# ---------------------------------------------------------------------------
# 4. ``ocx update --group ci`` — only ci-group tools change
# ---------------------------------------------------------------------------


def test_update_group_filter_only_changes_named_group(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx update --group ci`` after swapping a ci-group tool's tag →
    that ci-group entry's digest changes; the default-group entry stays
    byte-identical.
    """
    short = uuid4().hex[:8]
    repo_def = f"t_{short}_grp_def"
    repo_ci = f"t_{short}_grp_ci"

    make_package(ocx, repo_def, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_ci, "1.0.0", tmp_path, new=True, cascade=False)
    # Second tag for the ci-group tool so we have a different digest to
    # update to.
    make_package(ocx, repo_ci, "2.0.0", tmp_path, new=False, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
defaulttool = "{ocx.registry}/{repo_def}:1.0.0"

[group.ci]
citool = "{ocx.registry}/{repo_ci}:1.0.0"
""",
    )

    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    initial_text = _read_lock_text(project)
    initial_def = _pinned_for(initial_text, "defaulttool")
    initial_ci = _pinned_for(initial_text, "citool")
    assert initial_def is not None and initial_ci is not None

    # Swap citool's tag to 2.0.0 then update only the ci group.
    _write_ocx_toml(
        project,
        f"""\
[tools]
defaulttool = "{ocx.registry}/{repo_def}:1.0.0"

[group.ci]
citool = "{ocx.registry}/{repo_ci}:2.0.0"
""",
    )

    result = _run_update(ocx, project, "--group", "ci")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx update --group ci failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    after_text = _read_lock_text(project)
    after_def = _pinned_for(after_text, "defaulttool")
    after_ci = _pinned_for(after_text, "citool")
    assert after_def is not None and after_ci is not None

    assert after_ci != initial_ci, (
        "citool digest must change after `ocx update --group ci`"
    )
    assert after_def == initial_def, (
        "defaulttool digest must be unchanged when not selected by --group"
    )
