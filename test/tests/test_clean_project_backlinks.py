# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the Unit 6 project-registry backlink guard in ``ocx clean``.

These tests encode the acceptance contract from ``adr_clean_project_backlinks.md``
"Testing Surface" section and the plan verification block
(``auto-findings-md-eventual-fox.md`` lines 185–186):

  "Unit 6: package referenced only by another project's ``ocx.lock`` survives
   ``ocx clean``; ``--force`` deletes it; report cites project path."

Specification mode (contract-first TDD)
---------------------------------------
The ``ProjectRegistry::register`` stub at
``crates/ocx_lib/src/project/registry.rs`` returns ``unimplemented!()``.
Every test below is therefore expected to FAIL until the Unit 6 implementation
phase:

1. Tests that trigger ``ocx lock`` or ``ocx pull`` will panic inside the binary
   because ``ProjectLock::save`` will call ``register`` after the rename, hitting
   the ``unimplemented!()`` panic.
2. Even if registration somehow silently swallowed the panic, ``ocx clean``
   would fail because ``collect_project_roots`` (also ``unimplemented!()``)
   panics when invoked.
3. The ``--force`` flag is not yet wired: ``clean.rs::Clean`` does not yet
   accept a ``--force`` argument, so tests that pass ``--force`` will fail with
   a usage error (rc=64) rather than the expected rc=0.

Test inventory (per ADR "Testing Surface" + plan Verification block):

1. ``test_package_held_by_other_project_survives_clean``
2. ``test_force_flag_bypasses_registry``
3. ``test_lazy_prune_after_lockfile_deletion``
4. ``test_force_does_not_collect_actively_installed_packages``
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path
from uuid import uuid4

from src.helpers import make_package
from src.runner import OcxRunner, registry_dir

# ---------------------------------------------------------------------------
# Exit code constants — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64


# ---------------------------------------------------------------------------
# Module-local helpers
# ---------------------------------------------------------------------------
# Kept co-located with the tests (DAMP > DRY for acceptance tests, per
# ``quality-core.md``). Mirrors the helper shapes in ``test_project_pull.py``
# and ``test_clean.py``: subprocess-based to expose the ``cwd=`` argument that
# ``OcxRunner.run`` does not surface.


def _ocx_cmd(ocx: OcxRunner, *args: str) -> list[str]:
    """Build an argv list for ``ocx`` with the runner's isolated environment."""
    return [str(ocx.binary), *args]


def _run(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run an ocx sub-command with ``cwd`` set (needed for CWD-walk logic)."""
    cmd = _ocx_cmd(ocx, *args)
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    """Write an ``ocx.toml`` into ``project_dir`` and return its path."""
    path = project_dir / "ocx.toml"
    path.write_text(body, encoding="utf-8")
    return path


def _published_tool(ocx: OcxRunner, tmp_path: Path, label: str) -> tuple[str, str]:
    """Publish a unique test package and return ``(repo, tag)``."""
    short = uuid4().hex[:8]
    repo = f"t_{short}_bkl_{label}"
    tag = "1.0.0"
    make_package(ocx, repo, tag, tmp_path, new=True, cascade=False)
    return repo, tag


def _packages_present_count(ocx_home: Path, registry: str) -> int:
    """Count distinct ``content/`` directories in the object store.

    Each pulled (or installed) package contributes exactly one ``content/``
    directory under ``packages/{registry_dir}/``.  Counting them lets us assert
    "exactly N packages present" without knowing the content-addressed path.
    """
    base = ocx_home / "packages" / registry_dir(registry)
    if not base.exists():
        return 0
    return sum(1 for p in base.rglob("content") if p.is_dir())


def _setup_project_a(
    ocx: OcxRunner,
    tmp_path: Path,
    proj_a: Path,
) -> tuple[str, str]:
    """Bootstrap project A: publish a package, write ``ocx.toml``, run
    ``ocx lock`` to populate ``ocx.lock`` (which triggers registry
    ``register``), then ``ocx pull`` to download blobs into the object store.

    Returns ``(repo, tag)`` for the package.
    """
    repo, tag = _published_tool(ocx, tmp_path, "proj_a")
    proj_a.mkdir(parents=True, exist_ok=True)

    _write_ocx_toml(
        proj_a,
        f"""\
[tools]
the_tool = "{ocx.registry}/{repo}:{tag}"
""",
    )

    # ``ocx lock`` resolves the digest and writes ``ocx.lock``.  The
    # ``ProjectLock::save`` tail calls ``ProjectRegistry::register`` —
    # this is the call that triggers the ``unimplemented!()`` panic in
    # stub mode (expected test failure signal #1).
    lock_result = _run(ocx, proj_a, "lock")
    assert lock_result.returncode == EXIT_SUCCESS, (
        f"ocx lock failed in project A setup: rc={lock_result.returncode}\n"
        f"stderr:\n{lock_result.stderr}"
    )

    # ``ocx pull`` downloads blobs into the object store without creating
    # install symlinks (metadata-first pull path).  This populates the
    # packages/ tree that the GC will later attempt to collect.
    pull_result = _run(ocx, proj_a, "pull")
    assert pull_result.returncode == EXIT_SUCCESS, (
        f"ocx pull failed in project A setup: rc={pull_result.returncode}\n"
        f"stderr:\n{pull_result.stderr}"
    )

    return repo, tag


def _run_clean_json(
    ocx: OcxRunner,
    cwd: Path,
    *extra_flags: str,
) -> list[dict]:
    """Run ``ocx clean --dry-run --format json`` and return parsed entries.

    ``cwd`` drives which project is "active" for the GC invocation.
    """
    result = _run(ocx, cwd, "--format", "json", "clean", "--dry-run", *extra_flags)
    assert result.returncode == EXIT_SUCCESS, (
        f"ocx clean --dry-run failed: rc={result.returncode}\n"
        f"stderr:\n{result.stderr}\nstdout:\n{result.stdout}"
    )
    entries: list[dict] = json.loads(result.stdout)
    return entries


# ---------------------------------------------------------------------------
# 1. Package held by another project survives clean
# ---------------------------------------------------------------------------


def test_package_held_by_other_project_survives_clean(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A package pinned only by project A must NOT appear in would-collect set
    when ``ocx clean --dry-run`` is run with project B as the active directory.

    ADR contract: "package installed via project A's ``ocx.lock``, then
    ``ocx clean`` run with project B active → package retained, dry-run
    preview names project A's path."

    Failure signals (stub mode):
    - ``ocx lock`` (in ``_setup_project_a``) panics at
      ``ProjectLock::save`` → ``ProjectRegistry::register`` →
      ``unimplemented!()``.
    - If that somehow succeeds, ``ocx clean`` panics at
      ``collect_project_roots`` → ``unimplemented!()``.
    """
    proj_a = tmp_path / "proj-a"
    proj_b = tmp_path / "proj-b"
    proj_b.mkdir(parents=True, exist_ok=True)

    _setup_project_a(ocx, tmp_path, proj_a)

    # Confirm the package is present in the object store.
    ocx_home = Path(ocx.env["OCX_HOME"])
    count_before = _packages_present_count(ocx_home, ocx.registry)
    assert count_before >= 1, (
        f"expected at least one package in object store after pull; "
        f"got {count_before}"
    )

    # Run ``ocx clean --dry-run --format json`` from project B.
    entries = _run_clean_json(ocx, proj_b)

    # The package from project A must NOT be in the would-collect set
    # (i.e., its held_by list must be non-empty).
    object_entries = [e for e in entries if e.get("kind") == "object"]
    held_entries = [e for e in object_entries if e.get("held_by")]
    free_entries = [e for e in object_entries if not e.get("held_by")]

    # All downloaded objects must be held by project A — none are free.
    assert len(free_entries) == 0, (
        f"expected 0 unclaimed object entries; got {len(free_entries)}.\n"
        f"Free entries: {free_entries}"
    )
    assert len(held_entries) >= 1, (
        "expected at least one held entry pointing to proj-a's lock"
    )

    # Every held entry must reference proj-a's ocx.lock.
    lock_a = str(proj_a / "ocx.lock")
    for entry in held_entries:
        held_by: list[str] = entry["held_by"]
        assert any(
            lock_a in str(p) for p in held_by
        ), (
            f"held entry missing reference to proj-a/ocx.lock.\n"
            f"entry: {entry}\nexpected path containing: {lock_a}"
        )


# ---------------------------------------------------------------------------
# 2. --force bypasses the registry
# ---------------------------------------------------------------------------


def test_force_flag_bypasses_registry(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """With ``--force``, project A's package must appear in the would-collect
    set and its ``held_by`` array must be empty.

    ADR contract: "``--force``: ignore the registry entirely" means project
    roots are not consulted, so nothing can hold a package.

    Failure signals (stub mode):
    - ``ocx lock`` panics at ``register`` → ``unimplemented!()``.
    - Even if registration silent, ``ocx clean --force --dry-run`` fails
      because ``--force`` is not yet a recognised flag (rc=64, UsageError).
    """
    proj_a = tmp_path / "proj-a"
    proj_b = tmp_path / "proj-b"
    proj_b.mkdir(parents=True, exist_ok=True)

    _setup_project_a(ocx, tmp_path, proj_a)

    # Run ``ocx clean --force --dry-run --format json`` from project B.
    entries = _run_clean_json(ocx, proj_b, "--force")

    object_entries = [e for e in entries if e.get("kind") == "object"]

    # With --force, ALL objects should be collectable (held_by must be empty
    # for every entry — the registry is bypassed).
    held_entries = [e for e in object_entries if e.get("held_by")]
    assert len(held_entries) == 0, (
        f"--force must make held_by empty for all entries; "
        f"found {len(held_entries)} entries still held: {held_entries}"
    )

    # The package that was held in test 1 must now appear as collectable.
    assert len(object_entries) >= 1, (
        "expected at least one object entry in --force dry-run output"
    )


# ---------------------------------------------------------------------------
# 3. Lazy prune after lockfile deletion
# ---------------------------------------------------------------------------


def test_lazy_prune_after_lockfile_deletion(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """After deleting project A's ``ocx.lock``, running ``ocx clean`` from
    project B must lazily prune the registry entry and include the previously-
    held package in the would-collect set.

    ADR contract (Lazy Pruning Rules):
    - Rule 1: "``ocx_lock_path`` does not exist on disk → drop entry".
    - The on-disk ``projects.json`` must be rewritten (entry removed).

    Failure signals (stub mode): same as tests 1 and 2.
    """
    proj_a = tmp_path / "proj-a"
    proj_b = tmp_path / "proj-b"
    proj_b.mkdir(parents=True, exist_ok=True)

    _setup_project_a(ocx, tmp_path, proj_a)

    # Delete project A's ocx.lock — the registry entry becomes stale.
    lock_a = proj_a / "ocx.lock"
    lock_a.unlink()
    assert not lock_a.exists(), "lock_a must be deleted before running clean"

    # Run ``ocx clean --dry-run --format json`` from project B.
    entries = _run_clean_json(ocx, proj_b)

    # The package must now be in the would-collect set (held_by empty).
    object_entries = [e for e in entries if e.get("kind") == "object"]
    held_entries = [e for e in object_entries if e.get("held_by")]
    assert len(held_entries) == 0, (
        f"after deleting ocx.lock, package must be collectable; "
        f"still held: {held_entries}"
    )

    # The registry must have been pruned: projects.json must not list proj-a.
    ocx_home = Path(ocx.env["OCX_HOME"])
    registry_file = ocx_home / "projects.json"
    assert registry_file.exists(), "projects.json must still exist after prune"
    doc = json.loads(registry_file.read_text(encoding="utf-8"))
    entries_in_registry: list[dict] = doc.get("entries", [])
    proj_a_lock = str(lock_a)
    still_registered = [
        e for e in entries_in_registry
        if e.get("ocx_lock_path") == proj_a_lock
    ]
    assert len(still_registered) == 0, (
        f"proj-a's entry must be lazily pruned from registry after "
        f"ocx.lock deletion; still present: {still_registered}"
    )


# ---------------------------------------------------------------------------
# 4. --force does not collect actively installed packages
# ---------------------------------------------------------------------------


def test_force_does_not_collect_actively_installed_packages(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Even with ``--force``, a package that is actively installed (has live
    ``refs/symlinks/`` entries) must NOT be collected.

    ADR contract: "``--force`` bypasses registry only; live install symlinks
    and profile roots are always honoured even with ``--force``."

    Failure signals (stub mode):
    - ``ocx install`` succeeds (no registry involvement in install path).
    - ``ocx clean --force --dry-run`` fails because ``--force`` is not yet
      a recognised CLI flag (rc=64, UsageError).
    """
    proj_b = tmp_path / "proj-b"
    proj_b.mkdir(parents=True, exist_ok=True)

    # Publish a package and install it (creates install symlinks → GC root).
    repo, tag = _published_tool(ocx, tmp_path, "installed")
    short = f"{ocx.registry}/{repo}:{tag}"
    install_result = ocx.run("install", short)
    assert install_result.returncode == EXIT_SUCCESS, (
        f"ocx install failed: rc={install_result.returncode}\n"
        f"stderr:\n{install_result.stderr}"
    )

    # Confirm the package is present in the object store.
    ocx_home = Path(ocx.env["OCX_HOME"])
    count = _packages_present_count(ocx_home, ocx.registry)
    assert count >= 1, f"expected package in store after install; got {count}"

    # Run ``ocx clean --force --dry-run --format json`` from project B.
    entries = _run_clean_json(ocx, proj_b, "--force")

    # The installed package must NOT appear in the would-collect set.
    # Install symlinks override ``--force`` — they are always a GC root.
    object_entries = [e for e in entries if e.get("kind") == "object"]

    # With install symlinks present the package must either:
    #   a) not appear in the output at all (not a candidate), OR
    #   b) appear with ``dry_run: true`` but a non-empty ``held_by`` list
    #      (held by install-symlink root, independent of registry).
    #
    # Per ADR: "Live install symlinks … are always honoured even with
    # ``--force``."  The exact output shape depends on implementation
    # detail; we assert no collectable (free) object entries here.
    free_entries = [e for e in object_entries if not e.get("held_by")]
    assert len(free_entries) == 0, (
        f"installed package must not be collected even with --force; "
        f"free entries: {free_entries}"
    )
