# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx upgrade`` (plan Phase 5).

Tests trace one-to-one to the four spec bullets in plan Phase 5:

1. ``ocx upgrade cmake``               — only the ``cmake`` entry changes
2. ``ocx upgrade`` (no args)           — full re-resolution, equivalent to ``ocx lock``
3. ``ocx upgrade nonexistent``         — ``NotFound`` (79), no lock written
4. ``ocx upgrade --group ci``          — only ci-group tools change

Specification mode (contract-first TDD)
---------------------------------------
All tests run against the current Phase 5 stub. Both the CLI command
(``command/upgrade.rs``) and the library helper
(``project/resolve.rs::resolve_lock_partial``) call ``unimplemented!()``.
Every test is therefore expected to FAIL against the stub — the
contract they encode is the Phase 5 implementation target.
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path
from uuid import uuid4

from src.assertions import assert_not_exists
from src.helpers import make_package
from src.runner import OcxRunner, registry_dir


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
        _ocx_cmd(ocx, "upgrade", *extra),
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
# 1. ``ocx upgrade <name>`` — only the named entry changes
# ---------------------------------------------------------------------------


def test_update_named_tool_rewrites_only_that_entry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade a`` after swapping ``a``'s tag → only ``a``'s pinned
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

    # Swap 'a' tag in ocx.toml then run `ocx upgrade a`.
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
        f"ocx upgrade a failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    after_text = _read_lock_text(project)
    after_a = _pinned_for(after_text, "a")
    after_b = _pinned_for(after_text, "b")
    assert after_a is not None and after_b is not None

    assert after_a != initial_a, "'a' digest must change after `ocx upgrade a`"
    assert after_b == initial_b, "'b' digest must be unchanged when not selected"


# ---------------------------------------------------------------------------
# 2. ``ocx upgrade`` (no args) — full re-resolution, equivalent to ``ocx lock``
# ---------------------------------------------------------------------------


def test_update_no_args_full_resolution_equivalent_to_lock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade`` with no args on an unchanged ``ocx.toml`` → produces
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

    # `ocx upgrade` (no args) re-resolves everything against the same
    # ocx.toml — every digest must match the initial lock and the
    # declaration_hash must be unchanged.
    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_text = _read_lock_text(project)
    after_pinned = sorted(_PINNED_RE.findall(after_text))
    after_hash = _declaration_hash(after_text)

    assert after_pinned == initial_pinned, (
        "no-args `ocx upgrade` must keep every tool's pinned digest equal to `ocx lock`"
    )
    assert after_hash == initial_hash, (
        "declaration_hash must be unchanged when ocx.toml has not changed"
    )


# ---------------------------------------------------------------------------
# 3. ``ocx upgrade <unknown>`` — exit 79, no lock changes
# ---------------------------------------------------------------------------


def test_update_unknown_binding_exits_79_no_lock_change(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade nonexistent`` when ``nonexistent`` is not declared in
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
# 4. ``ocx upgrade --group ci`` — only ci-group tools change
# ---------------------------------------------------------------------------


def test_update_check_succeeds_on_current(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --check`` exits 0 without writing when the candidate
    lock matches the predecessor.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_upd_check_ok"
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
    before = (project / "ocx.lock").read_bytes()

    result = _run_update(ocx, project, "--check")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --check must exit 0 on a current lock; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after = (project / "ocx.lock").read_bytes()
    assert before == after, "ocx upgrade --check must NOT rewrite ocx.lock"


def test_update_check_exits_65_on_subset_drift(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --check`` exits 65 (DataError) without writing when
    an advisory tag has moved upstream — even though ``ocx.toml`` is
    byte-identical to the lock's recorded ``declaration_hash``.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_upd_check_drift"

    make_package(ocx, repo, "1.0.0", tmp_path, new=True, cascade=True)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
mover = "{ocx.registry}/{repo}:latest"
""",
    )
    initial = _run_lock(ocx, project)
    assert initial.returncode == EXIT_SUCCESS, initial.stderr
    before_lock = (project / "ocx.lock").read_bytes()

    # Publish 2.0.0 with cascade=True — "latest" now points at the new digest.
    make_package(ocx, repo, "2.0.0", tmp_path, new=False, cascade=True)
    refresh = subprocess.run(
        _ocx_cmd(ocx, "index", "update", f"{ocx.registry}/{repo}"),
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert refresh.returncode == EXIT_SUCCESS, refresh.stderr

    result = _run_update(ocx, project, "--check")
    assert result.returncode == 65, (
        f"ocx upgrade --check must exit 65 when an advisory tag moved; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    after_lock = (project / "ocx.lock").read_bytes()
    assert before_lock == after_lock, (
        "ocx upgrade --check must NOT rewrite ocx.lock when refusing"
    )


def test_update_group_filter_only_changes_named_group(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --group ci`` after swapping a ci-group tool's tag →
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
        f"ocx upgrade --group ci failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    after_text = _read_lock_text(project)
    after_def = _pinned_for(after_text, "defaulttool")
    after_ci = _pinned_for(after_text, "citool")
    assert after_def is not None and after_ci is not None

    assert after_ci != initial_ci, (
        "citool digest must change after `ocx upgrade --group ci`"
    )
    assert after_def == initial_def, (
        "defaulttool digest must be unchanged when not selected by --group"
    )


# ---------------------------------------------------------------------------
# Eager materialization — Phase-5 contracts
# ---------------------------------------------------------------------------


def _candidate_path(ocx: OcxRunner, repo: str, tag: str) -> Path:
    """Return the expected candidate-symlink path for ``repo:tag``."""
    return (
        Path(ocx.ocx_home)
        / "symlinks"
        / registry_dir(ocx.registry)
        / repo
        / "candidates"
        / tag
    )


def _packages_present_count(ocx: OcxRunner) -> int:
    """Count ``content/`` directories under
    ``$OCX_HOME/packages/{registry_dir}/`` — eager-vs-lazy observable for
    toolchain mutators (``project_context.rs::materialize_lock`` warms via
    ``pull_all``, never creates symlinks).
    """
    base = Path(ocx.ocx_home) / "packages" / registry_dir(ocx.registry)
    if not base.exists():
        return 0
    return sum(1 for p in base.rglob("content") if p.is_dir())


def _two_tag_project(
    ocx: OcxRunner, tmp_path: Path
) -> tuple[Path, str, str, str]:
    """Publish a tool with two distinct tags and return ``(project_dir, repo, tag_v1, tag_v2)``.

    The project's ``ocx.toml`` is initially locked to ``tag_v1``; callers that
    want to exercise upgrade behaviour swap the toml to ``tag_v2`` and re-run
    ``ocx lock`` / ``ocx upgrade``.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_upg_eager"
    tag_v1 = "1.0.0"
    tag_v2 = "2.0.0"
    make_package(ocx, repo, tag_v1, tmp_path, new=True, cascade=False)
    make_package(ocx, repo, tag_v2, tmp_path, new=False, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v1}"
""",
    )
    # Write the initial lock so upgrade has a predecessor.  Use --no-pull
    # so the setup step leaves the object store cold; otherwise tag_v1
    # would already be present and `_packages_present_count >= 1` would
    # fire trivially, hiding whether the eager-default upgrade actually
    # materialised tag_v2 (the cold-store baseline is the eager-vs-lazy
    # observable now that candidate symlinks are no longer created by
    # toolchain mutators).
    initial = _run_lock(ocx, project, "--no-pull")
    assert initial.returncode == EXIT_SUCCESS, initial.stderr

    return project, repo, tag_v1, tag_v2


def test_upgrade_eager_default_materializes_new_digest(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """REGRESSION GUARD: ``ocx upgrade`` (no flags) after bumping the toml to
    a new tag writes the lock AND pre-warms the object store with the new
    tag, but creates **no** candidate symlink.

    Plan Phase-5 Step 3.3 contract: default is eager. The candidate-absent
    half locks in the no-symlink mutator invariant
    (``project_context.rs::materialize_lock`` → ``pull_all``).
    """
    project, repo, tag_v1, tag_v2 = _two_tag_project(ocx, tmp_path)

    # Bump the toml to tag_v2 then upgrade.
    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v2}"
""",
    )
    result = _run_update(ocx, project)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    assert _packages_present_count(ocx) >= 1, (
        "eager ocx upgrade must pre-warm the new digest into the object store"
    )
    candidate = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate)


def test_upgrade_no_pull_skips_install(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --no-pull`` writes the lock and leaves the object store
    cold. No candidate symlink under either eager or lazy paths anymore;
    cold-store is the only eager-vs-lazy observable.

    Plan Phase-5 Step 3.3 contract.
    """
    project, repo, tag_v1, tag_v2 = _two_tag_project(ocx, tmp_path)

    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v2}"
""",
    )
    result = _run_update(ocx, project, "--no-pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --no-pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written even with --no-pull"

    assert _packages_present_count(ocx) == 0, (
        "ocx upgrade --no-pull must leave the object store cold"
    )
    candidate = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate)


def test_upgrade_pull_then_no_pull_last_wins(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --pull --no-pull`` → ``--no-pull`` wins (POSIX last-wins);
    lock must advance to the new digest but candidate_v2 must NOT exist.

    Plan Phase-5 Step 3.3 last-wins contract for ``ocx upgrade``.
    """
    project, repo, tag_v1, tag_v2 = _two_tag_project(ocx, tmp_path)

    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v2}"
""",
    )
    result = _run_update(ocx, project, "--pull", "--no-pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --pull --no-pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written"

    assert _packages_present_count(ocx) == 0, (
        "--no-pull must win: object store stays cold"
    )
    candidate_v2 = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate_v2)


def test_upgrade_no_pull_then_pull_last_wins(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --no-pull --pull`` → ``--pull`` wins (POSIX last-wins);
    lock advances and object store warms with the new digest; candidate
    symlink absent.

    Plan Phase-5 Step 3.3 last-wins contract for ``ocx upgrade``.
    """
    project, repo, tag_v1, tag_v2 = _two_tag_project(ocx, tmp_path)

    _write_ocx_toml(
        project,
        f"""\
[tools]
tool = "{ocx.registry}/{repo}:{tag_v2}"
""",
    )
    result = _run_update(ocx, project, "--no-pull", "--pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --no-pull --pull failed: rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert (project / "ocx.lock").is_file(), "ocx.lock must be written"

    assert _packages_present_count(ocx) >= 1, (
        "--pull must win: object store warms with the new digest"
    )
    candidate_v2 = _candidate_path(ocx, repo, tag_v2)
    assert_not_exists(candidate_v2)


def test_upgrade_check_unaffected_by_pull_flags(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx upgrade --check`` is a pure dry-run and must NOT create any
    candidate symlink regardless of ``--pull`` / ``--no-pull`` flags.

    Plan Phase-5 Step 3.3: the ``--check`` early-return path in
    ``upgrade.rs:175`` (before the materialize call) guarantees this.
    The test is a regression guard for the separation of verify vs mutate.
    """
    project, repo, tag_v1, _tag_v2 = _two_tag_project(ocx, tmp_path)

    # Candidate symlinks are never created by toolchain mutators (lock /
    # upgrade / add) under the no-symlink mutator model. The probe path
    # is kept here as a regression anchor for the verify-vs-mutate split.
    candidate_v1 = _candidate_path(ocx, repo, tag_v1)
    assert_not_exists(candidate_v1)

    lock_bytes_before = (project / "ocx.lock").read_bytes()

    # Run with both --check and --pull to confirm --check wins.
    result = _run_update(ocx, project, "--check", "--pull")
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx upgrade --check --pull must exit 0 on a current lock; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )

    lock_bytes_after = (project / "ocx.lock").read_bytes()
    assert lock_bytes_before == lock_bytes_after, (
        "ocx upgrade --check must NOT rewrite ocx.lock"
    )
    # --check must not materialize anything even when --pull is present.
    assert_not_exists(candidate_v1)
