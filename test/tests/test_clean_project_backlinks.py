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
4. ``test_custom_named_project_registered_and_protected_from_clean``
5. ``test_force_does_not_collect_actively_installed_packages``
6. ``test_clean_io_failure_is_non_fatal``

``test_clean_io_failure_is_non_fatal`` is **relocated here from the Rust unit
suite** (plan W1-P3 unit test ``collect_project_roots_io_failure_is_non_fatal``,
F4): ``collect_project_roots`` lives behind the private
``package_manager::tasks`` module and is not re-exported (only
``CleanResult``/``CleanedObject`` are), so it is unreachable from a
crate-internal Rust test. The deliberate exit-78 elimination (no JSON parse
surface ⇒ a broken ledger is pruned, never a fatal ``ConfigError``) is
therefore pinned at the acceptance level instead.
"""
from __future__ import annotations

import json
import os
import re as _re_clean
import stat
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

    # Every held entry must reference proj-a's project directory. Under the
    # symlink ledger ``held_by`` carries the project dir (the ledger key),
    # not the lock path.
    proj_a_dir = str(proj_a)
    for entry in held_entries:
        held_by: list[str] = entry["held_by"]
        assert any(
            proj_a_dir in str(p) for p in held_by
        ), (
            f"held entry missing reference to proj-a project dir.\n"
            f"entry: {entry}\nexpected path containing: {proj_a_dir}"
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
    project B must lazily prune the stale ledger symlink and include the
    previously-held package in the would-collect set.

    ADR contract (``adr_project_gc_symlink_ledger.md``, flat symlink store):
    - ``<target>/ocx.lock`` absent → the ``$OCX_HOME/projects/<hash>`` symlink
      is pruned silently at DEBUG (no JSON document, no schema, no sentinel —
      the symlink's resolvability *is* the liveness signal).

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

    # The ledger must have been pruned: no symlink under
    # ``$OCX_HOME/projects/`` may still resolve into proj-a. The flat symlink
    # store has no JSON document — the absence (or non-resolution) of the
    # per-project symlink *is* the pruned state.
    ocx_home = Path(ocx.env["OCX_HOME"])
    projects_dir = ocx_home / "projects"
    proj_a_resolved = proj_a.resolve()
    if projects_dir.exists():
        for link in projects_dir.iterdir():
            if link.name.startswith(".tmp-"):
                continue  # in-flight staging entry — not a ledger root
            try:
                target = link.resolve()
            except OSError:
                continue  # dangling link mid-prune — acceptable
            assert target != proj_a_resolved, (
                f"proj-a's ledger symlink must be lazily pruned after "
                f"ocx.lock deletion; still resolves: {link} -> {target}"
            )


# ---------------------------------------------------------------------------
# 4. --force does not collect actively installed packages
# ---------------------------------------------------------------------------


def test_custom_named_project_registered_and_protected_from_clean(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A project entered via ``--project=<custom>.toml`` must register its
    canonical config path AND survive the lazy-prune walker.

    Codex High-4 contract: previously the registry hardcoded
    ``<lock_parent>/ocx.toml`` for prune rule 3; a project that named
    its config something else (e.g. ``workspace.toml``) was registered
    successfully but silently pruned on the next ``ocx clean`` →
    orphaned ``ocx.lock`` → packages still pinned by that lock got GC'd.

    Repro: bootstrap a project at ``custom-proj/workspace.toml`` via
    ``--project=workspace.toml``, run ``ocx lock``, then ``ocx clean
    --dry-run`` from a sibling project. The custom-named lock's package
    must NOT appear in the would-collect set.
    """
    proj_a = tmp_path / "custom-proj"
    proj_a.mkdir(parents=True, exist_ok=True)
    proj_b = tmp_path / "proj-b"
    proj_b.mkdir(parents=True, exist_ok=True)

    repo, tag = _published_tool(ocx, tmp_path, "custom_proj")

    # Stage the custom-named config file at workspace.toml (NOT ocx.toml).
    (proj_a / "workspace.toml").write_text(
        f"""\
[tools]
the_tool = "{ocx.registry}/{repo}:{tag}"
""",
        encoding="utf-8",
    )

    # `ocx --project workspace.toml lock` from proj_a registers the lock
    # under the custom config-file path. `--project` is a global flag
    # (lives on Cli, not the subcommand) so it must precede `lock`.
    lock_result = _run(ocx, proj_a, "--project", "workspace.toml", "lock")
    assert lock_result.returncode == EXIT_SUCCESS, (
        f"ocx --project workspace.toml lock failed: rc={lock_result.returncode}\n"
        f"stderr:\n{lock_result.stderr}"
    )
    pull_result = _run(ocx, proj_a, "--project", "workspace.toml", "pull")
    assert pull_result.returncode == EXIT_SUCCESS, (
        f"ocx --project workspace.toml pull failed: rc={pull_result.returncode}\n"
        f"stderr:\n{pull_result.stderr}"
    )

    # `ocx clean --dry-run` from proj_b: the custom-named project's
    # package must be HELD (registry honours the recorded config path)
    # — none of its objects may appear in the free set. Without the
    # Codex-High-4 fix, the lazy-prune walker would have pruned the
    # custom-named entry on this very call (no sibling ocx.toml at the
    # historical hardcoded path), and the package would then be free.
    # Deliberate verification (plan W1-P3, F3): this test asserts ONLY
    # ``free_entries == 0`` and never inspects ``held_by`` shape/contents, so
    # the symlink-ledger migration (held_by now keys on the project dir, not
    # the lock path) does NOT require any change here. Recorded as a conscious
    # no-op, not an oversight.
    entries = _run_clean_json(ocx, proj_b)
    object_entries = [e for e in entries if e.get("kind") == "object"]
    free_entries = [e for e in object_entries if not e.get("held_by")]
    assert len(free_entries) == 0, (
        "custom-named project's package must remain held by its lock; "
        f"got free entries: {free_entries}"
    )


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
    install_result = ocx.run("package", "install", short)
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


# ---------------------------------------------------------------------------
# 6. Broken ledger state is non-fatal — deliberate exit-78 elimination (F4)
# ---------------------------------------------------------------------------


def test_clean_io_failure_is_non_fatal(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A broken/garbage ``$OCX_HOME/projects/`` ledger state must NOT make
    ``ocx clean`` fail — it proceeds (rc=0), pruning what it can.

    Relocated from the Rust unit suite (plan W1-P3
    ``collect_project_roots_io_failure_is_non_fatal``, F4):
    ``collect_project_roots`` is not reachable from a crate-internal Rust
    test (private ``package_manager::tasks`` module, not re-exported), so the
    contract is pinned here instead.

    ADR ``adr_project_gc_symlink_ledger.md`` §Risks "Corrupt-state failure
    mode removed, not relocated": the superseded JSON model exited ``ocx
    clean`` with ``ConfigError`` (exit 78) on a corrupt ``projects.json``.
    The flat symlink store has **no parse surface** — a bad entry is simply a
    dangling/garbage directory entry that is pruned. This test asserts the
    deliberate *elimination*: a clobbered ``projects/`` (a non-symlink file
    where a hash symlink is expected) does not yield exit 78 and does not
    abort the clean.

    Failure signals (stub mode): ``ocx lock`` in ``_setup_project_a`` panics
    at ``ProjectRegistry::register`` → ``unimplemented!()``; if that somehow
    succeeded, ``ocx clean`` panics at ``collect_project_roots`` →
    ``live_projects`` → ``unimplemented!()``.
    """
    proj_a = tmp_path / "proj-a"
    proj_b = tmp_path / "proj-b"
    proj_b.mkdir(parents=True, exist_ok=True)

    _setup_project_a(ocx, tmp_path, proj_a)

    # Clobber the ledger: drop a garbage *regular file* into the projects/
    # store where only hash symlinks belong. There is no JSON to corrupt;
    # this is the closest analogue of a broken ledger entry.
    ocx_home = Path(ocx.env["OCX_HOME"])
    projects_dir = ocx_home / "projects"
    projects_dir.mkdir(parents=True, exist_ok=True)
    (projects_dir / "deadbeefdeadbeef").write_text(
        "not a symlink — garbage ledger entry", encoding="utf-8"
    )

    # ``ocx clean --dry-run`` from project B must still succeed (rc=0), NOT
    # exit 78 (ConfigError). The exact would-collect set is unconstrained
    # here — the contract under test is "non-fatal", i.e. clean proceeds.
    result = _run(ocx, proj_b, "--format", "json", "clean", "--dry-run")
    assert result.returncode == EXIT_SUCCESS, (
        f"broken ledger state must be non-fatal (no exit 78); "
        f"got rc={result.returncode}\nstderr:\n{result.stderr}\n"
        f"stdout:\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# 7. A2/A1 fail-closed: a transiently-unreachable holder project's package
#    must SURVIVE clean (no fail-open to an empty root set)
# ---------------------------------------------------------------------------


def test_transiently_unreachable_holder_survives_clean(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Plan §"Living Design — Review-Fix Amendments" A1 + A2 (fail-closed).

    Project B pins a package and is registered in the symlink ledger. Its
    ledger entry is then made *transiently* unreachable by ``chmod 000`` on an
    intermediate directory component of B's path (the project dir is NOT
    deleted — distinct from ``test_lazy_prune_after_lockfile_deletion``, which
    deletes ``ocx.lock``, and from ``test_clean_io_failure_is_non_fatal``,
    which drops a garbage regular file).

    Probing B's ledger link now fails with ``EACCES`` (a non-``NotFound``
    transient error) → ``ProbeResult::Unknown``. Per A1, B must remain a GC
    root for this run (fail-closed); per A2 a registry enumeration failure
    must NOT degrade to an empty root set. Either way, B's pinned package
    MUST survive ``ocx clean`` run from project A.

    Pre-A1/A2 behaviour (the bug this pins): the Unknown entry is dropped
    from the live-root set this run (and/or the enumeration error fails open
    to ``Vec::new()``), so B's package appears collectable — the assertion
    below fails, exposing the silent-data-loss regression.

    Skips gracefully when running as root (root ignores DAC mode bits, so the
    unreachable condition cannot be constructed).
    """
    proj_a = tmp_path / "proj-a"
    proj_a.mkdir(parents=True, exist_ok=True)

    # Project B lives behind an intermediate `gate/` directory so we can make
    # *its path* (not the ledger store) untraversable without touching A.
    gate = tmp_path / "gate"
    proj_b = gate / "proj-b"
    proj_b.mkdir(parents=True, exist_ok=True)

    # B pins a package (lock → register in the symlink ledger, pull → blobs).
    repo, tag = _setup_project_a(ocx, tmp_path, proj_b)

    ocx_home = Path(ocx.env["OCX_HOME"])
    count_before = _packages_present_count(ocx_home, ocx.registry)
    assert count_before >= 1, (
        f"expected B's package present after pull; got {count_before}"
    )

    # Make the intermediate `gate/` component untraversable. The ledger
    # symlink ($OCX_HOME/projects/<hash> -> .../gate/proj-b) still exists
    # (lstat unaffected), but canonicalising it now fails with EACCES — a
    # transient (NOT NotFound) error → ProbeResult::Unknown.
    original_mode = stat.S_IMODE(gate.stat().st_mode)
    os.chmod(gate, 0o000)
    try:
        # Root ignores DAC bits — the precondition cannot be built. Detect by
        # probing and skip without failing.
        try:
            os.listdir(proj_b)
            running_as_root = True
        except PermissionError:
            running_as_root = False
        if running_as_root:
            import pytest

            pytest.skip("running as root: cannot construct unreachable path")

        result = _run(ocx, proj_a, "--format", "json", "clean", "--dry-run")
    finally:
        os.chmod(gate, original_mode)

    # Clean must not crash the way an unhandled panic / fatal error would.
    assert result.returncode == EXIT_SUCCESS, (
        f"clean with a transiently-unreachable holder must remain non-fatal "
        f"for the run itself; rc={result.returncode}\n"
        f"stderr:\n{result.stderr}\nstdout:\n{result.stdout}"
    )

    entries: list[dict] = json.loads(result.stdout)
    object_entries = [e for e in entries if e.get("kind") == "object"]
    free_entries = [e for e in object_entries if not e.get("held_by")]

    # The core fail-closed assertion: B's pinned package must NOT be in the
    # collectable (free) set. A transient EACCES on a *live* holder project
    # must never let GC collect its pinned packages.
    assert len(free_entries) == 0, (
        "fail-closed (A1/A2): a transiently-unreachable holder project's "
        "pinned package must survive clean — it must NOT appear as a "
        f"collectable (free) object. Got free entries: {free_entries}"
    )


# ---------------------------------------------------------------------------
# 8. A3 fail-closed: a Live-probed holder whose ocx.lock is itself
#    independently unreadable ⇒ LockLoad::Indeterminate ⇒ RetainAll, exit 0
# ---------------------------------------------------------------------------


def test_live_holder_with_unreadable_lock_survives_clean(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Plan §"Living Design — Review-Fix Amendments" A3 (record):
    ``LockLoad::Indeterminate ⇒ CollectedRoots::RetainAll`` exit 0.

    This isolates the ``LockLoad::Indeterminate``-via-**readable-ledger** path
    from the A1-``ProbeResult::Unknown`` path exercised by
    ``test_transiently_unreachable_holder_survives_clean``.

    Project B is registered and its ledger entry probes cleanly to
    ``ProbeResult::Live`` (the ``$OCX_HOME/projects/<hash>`` symlink resolves
    and ``<dir>/ocx.lock`` exists). The root set is therefore *known*. We then
    make ``<B>/ocx.lock`` **itself** unreadable via ``chmod 0o000`` on the
    **lock file** — NOT an intermediate directory (distinct from
    ``test_transiently_unreachable_holder_survives_clean``) and NOT a garbage
    regular file dropped into the store (distinct from
    ``test_clean_io_failure_is_non_fatal``).

    ``ProjectLock::from_path`` then fails with a transient non-``NotFound``
    I/O error (``EACCES``) ⇒ ``Ok(None)`` is NOT produced ⇒
    ``LockLoad::Indeterminate``. Per A3 the GC fails closed
    proportionally: ``CollectedRoots::RetainAll`` ⇒ ``ocx clean`` collects
    NOTHING this run (sweeps only stale temps), exits 0 (WARN logged; next
    clean re-probes). B's pinned package therefore MUST survive — it must not
    appear as a collectable (free) object.

    Pre-A3 behaviour (the bug this pins): an unreadable lock on a known-Live
    root degrades the root's pinned digests to "drop the root", so B's package
    appears collectable — the assertion below fails, exposing the
    silent-data-loss regression at the lock-load layer (distinct from the
    enumeration/probe layer A1/A2 covers).

    Skips gracefully when running as root (root ignores DAC mode bits, so the
    unreadable-lock condition cannot be constructed).
    """
    proj_a = tmp_path / "proj-a"
    proj_a.mkdir(parents=True, exist_ok=True)
    proj_b = tmp_path / "proj-b"
    proj_b.mkdir(parents=True, exist_ok=True)

    # B pins a package (lock → register in the symlink ledger, pull → blobs).
    # B's ledger entry + dir + ocx.lock are all fine here, so the per-entry
    # liveness probe is ProbeResult::Live (root set is KNOWN — the A1/A2
    # enumeration/probe layer is NOT what this test exercises).
    _setup_project_a(ocx, tmp_path, proj_b)

    ocx_home = Path(ocx.env["OCX_HOME"])
    count_before = _packages_present_count(ocx_home, ocx.registry)
    assert count_before >= 1, (
        f"expected B's package present after pull; got {count_before}"
    )

    lock_b = proj_b / "ocx.lock"
    assert lock_b.is_file(), "precondition: B's ocx.lock must exist and be a file"
    original_mode = stat.S_IMODE(lock_b.stat().st_mode)

    # Make the LOCK FILE itself unreadable (not an intermediate dir). The
    # ledger symlink still resolves and <dir>/ocx.lock still *exists*
    # (try_exists → true) so probe_live_target returns Live; only
    # ProjectLock::from_path's read of the file content fails with EACCES.
    os.chmod(lock_b, 0o000)
    try:
        # Root ignores DAC bits — the precondition cannot be built. Detect by
        # probing a read and skip without failing.
        try:
            lock_b.read_bytes()
            running_as_root = True
        except PermissionError:
            running_as_root = False
        if running_as_root:
            import pytest

            pytest.skip("running as root: cannot make ocx.lock unreadable")

        result = _run(ocx, proj_a, "--format", "json", "clean", "--dry-run")
    finally:
        os.chmod(lock_b, original_mode)

    # A3: proportional fail-closed is exit 0 (RetainAll), NOT an abort.
    assert result.returncode == EXIT_SUCCESS, (
        f"A3: an unreadable ocx.lock on a Live-probed holder must yield "
        f"RetainAll + exit 0 (proportional fail-closed), not an abort; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}\n"
        f"stdout:\n{result.stdout}"
    )

    entries: list[dict] = json.loads(result.stdout)
    object_entries = [e for e in entries if e.get("kind") == "object"]
    free_entries = [e for e in object_entries if not e.get("held_by")]

    # The core A3 fail-closed assertion: RetainAll means NOTHING is
    # collectable this run, so B's pinned package must NOT be free.
    assert len(free_entries) == 0, (
        "fail-closed (A3): a Live-probed holder whose ocx.lock is itself "
        "transiently unreadable must trigger RetainAll — B's pinned package "
        "must survive clean and must NOT appear as a collectable (free) "
        f"object. Got free entries: {free_entries}"
    )


# ---------------------------------------------------------------------------
# 9. Bug-R3: an Absent (deleted-ocx.lock) project ordered before a survivor
#    must NOT panic ``ocx clean`` (buckets[index] out-of-bounds regression)
# ---------------------------------------------------------------------------


def test_clean_survives_departed_project_before_survivor(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Plan §"Living Design — Review-Fix Amendments" Bug-R3 regression.

    ``collect_project_roots`` loads every registered project's ``ocx.lock`` in
    parallel, then sizes a ``buckets`` vector to ``loaded.len()`` (survivors
    only). The pre-fix code keyed ``buckets[index]`` by the *original*
    ``entries`` enumerate index, which spans every registered project
    **including** the ones that became ``LockLoad::Absent`` (a deleted /
    raced-away ``ocx.lock``). When an ``Absent`` entry precedes a survivor,
    the survivor's original index is ``>= loaded.len()`` ⇒ ``buckets[index]``
    panics out of bounds, crashing ``ocx clean``. The fix re-keys ``buckets``
    on each survivor's *dense* position in ``loaded`` (assigned via
    ``loaded.iter().enumerate()``), so the OOB is structurally impossible
    regardless of how/when an ``Absent`` entry arises.

    This test pins the **user-visible invariant** the fix guarantees: a
    multi-project ``$OCX_HOME`` where one project's ``ocx.lock`` was deleted
    (the common departed-project case) and another survives — ``ocx clean``
    (no ``--force``) run from an unrelated third directory must exit 0 (never
    panic / abort) and retain the survivor's pinned package.

    Black-box determinism note: the flat-symlink ledger's ``live_projects``
    probe self-prunes a project whose ``<dir>/ocx.lock`` is absent *before*
    the load step (``probe_live_target`` → ``Dead`` → the ledger link is
    removed and the project is excluded from ``project_dirs``), so a
    plainly-deleted lock is filtered by the registry layer and the residual
    ``LockLoad::Absent`` arm is only reachable via the narrow
    probe-vs-``ProjectLock::from_path`` TOCTOU race — not deterministically
    forceable from a black-box acceptance test. The unit-level OOB precondition
    is therefore pinned by the dense-keying contract documented on
    ``LoadedLock`` in ``crates/ocx_lib/src/package_manager/tasks/clean.rs``;
    this test pins the externally-observable correctness contract (no crash,
    survivor retained) the regression would otherwise break for every
    multi-project user.
    """
    proj_a = tmp_path / "proj-a"
    proj_a.mkdir(parents=True, exist_ok=True)
    proj_b = tmp_path / "proj-b"
    proj_b.mkdir(parents=True, exist_ok=True)
    proj_c = tmp_path / "proj-c"
    proj_c.mkdir(parents=True, exist_ok=True)

    # One published package pinned by both A and B. Each gets its own
    # ``ocx.toml`` + ``ocx lock`` (→ ledger ``register``) + ``ocx pull`` (→
    # blobs in the object store). They pin the *same* package digest, so
    # whichever project survives is the sole holder of that one package.
    repo, tag = _published_tool(ocx, tmp_path, "shared")
    tool_ref = f"{ocx.registry}/{repo}:{tag}"
    for proj in (proj_a, proj_b):
        _write_ocx_toml(proj, f'[tools]\nthe_tool = "{tool_ref}"\n')
        lock_res = _run(ocx, proj, "lock")
        assert lock_res.returncode == EXIT_SUCCESS, (
            f"ocx lock failed for {proj}: rc={lock_res.returncode}\n"
            f"stderr:\n{lock_res.stderr}"
        )
        pull_res = _run(ocx, proj, "pull")
        assert pull_res.returncode == EXIT_SUCCESS, (
            f"ocx pull failed for {proj}: rc={pull_res.returncode}\n"
            f"stderr:\n{pull_res.stderr}"
        )

    ocx_home = Path(ocx.env["OCX_HOME"])
    projects_dir = ocx_home / "projects"

    # Map each ledger symlink to the project dir it resolves to, then pick the
    # project whose link sorts FIRST (collect_project_roots enumerates sorted
    # by link name). Deleting its lock makes the Absent entry deterministically
    # occupy enumerate index 0, so a survivor's original index always exceeds
    # `loaded.len()` ⇒ the pre-fix `buckets[index]` panics out of bounds
    # regardless of the hash values.
    proj_a_resolved = proj_a.resolve()
    proj_b_resolved = proj_b.resolve()
    ledger: list[tuple[str, Path]] = []
    for link in sorted(projects_dir.iterdir(), key=lambda p: p.name):
        if link.name.startswith(".tmp-"):
            continue
        ledger.append((link.name, link.resolve()))
    assert {t for _, t in ledger} == {proj_a_resolved, proj_b_resolved}, (
        f"both projects must be registered in the ledger; got {ledger}"
    )

    first_target = ledger[0][1]
    absent_proj = proj_a if first_target == proj_a_resolved else proj_b
    survivor_proj = proj_b if absent_proj is proj_a else proj_a

    # Delete the first-sorting project's ocx.lock → it becomes
    # LockLoad::Absent at enumerate index 0 (ProjectLock::from_path maps
    # NotFound → Ok(None) → the project is dropped from `loaded` but its
    # index slot is the one the pre-fix `buckets[index]` keyed on).
    absent_lock = absent_proj / "ocx.lock"
    absent_lock.unlink()
    assert not absent_lock.exists(), "absent project's ocx.lock must be deleted"
    assert (survivor_proj / "ocx.lock").exists(), "survivor's ocx.lock must remain"

    # Confirm the shared package is still in the object store before the GC.
    count_before = _packages_present_count(ocx_home, ocx.registry)
    assert count_before >= 1, (
        f"expected shared package present after pull; got {count_before}"
    )

    # ``ocx clean --dry-run`` (NO --force) from a third directory. The pre-fix
    # code panics here with ``index out of bounds`` because the Absent entry
    # is deterministically at enumerate index 0 while the survivor's original
    # index exceeds `loaded.len()`; _run_clean_json asserts rc=0 so a panic
    # surfaces as a clear failure with the captured stderr.
    entries = _run_clean_json(ocx, proj_c)

    # The survivor is a live GC root (its ledger symlink resolves and its
    # ocx.lock exists), so the shared package must be retained — no free
    # object entries.
    object_entries = [e for e in entries if e.get("kind") == "object"]
    free_entries = [e for e in object_entries if not e.get("held_by")]
    assert len(free_entries) == 0, (
        "Bug-R3: with the first-sorting project's ocx.lock deleted (Absent at "
        "enumerate index 0) and the other surviving, ocx clean must not panic "
        "and the survivor's pinned package must remain held — got free "
        f"entries: {free_entries}"
    )


# ---------------------------------------------------------------------------
# V2 lock shape: GC roots leaf digests from [tool.platforms] (ADR §clean.rs)
# ---------------------------------------------------------------------------

_LEAF_RE_CLEAN = _re_clean.compile(r'"[^"]+"\s*=\s*"sha256:([0-9a-f]{64})"')


def _leaf_digests_in_lock(lock_text: str) -> list[str]:
    """Return all per-platform leaf digest hex values from a V2 lock."""
    return _LEAF_RE_CLEAN.findall(lock_text)


def test_clean_v2_lock_roots_leaf_digests_from_platforms_table(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx clean`` on a V2 lock roots digests from ``[tool.platforms]``, not
    from a ``pinned`` index digest.

    ADR §GC: "V2: root leaf digests straight from the lock (presence-gated);
    the index-blob-read path is retained only for V1."

    Scenario: lock a project (V2 shape), confirm the lock carries a
    ``[tool.platforms]`` table with at least one leaf digest, run
    ``ocx clean --dry-run --format json``, and assert the package is held
    (not free) — proving GC roots the V2 leaf, not a missing index digest.
    """
    proj_a = tmp_path / "proj-a-v2gc"
    proj_b = tmp_path / "proj-b-v2gc"
    proj_b.mkdir(parents=True, exist_ok=True)

    repo, tag = _setup_project_a(ocx, tmp_path, proj_a)

    lock_text = (proj_a / "ocx.lock").read_text()
    assert "lock_version = 2" in lock_text, (
        "V2 lock shape required for this test; got:\n" + lock_text[:300]
    )
    assert "[tool.platforms]" in lock_text, (
        "V2 lock must carry a [tool.platforms] table; got:\n" + lock_text[:300]
    )
    leaf_digests = _leaf_digests_in_lock(lock_text)
    assert leaf_digests, (
        "V2 lock must record at least one leaf digest; got:\n" + lock_text[:300]
    )
    # The lock must NOT carry a legacy `pinned` line.
    assert "pinned =" not in lock_text, (
        "V2 lock must not carry a `pinned` index-digest line"
    )

    # GC from proj_b: proj_a's leaf-pinned package must NOT be free.
    entries = _run_clean_json(ocx, proj_b)
    object_entries = [e for e in entries if e.get("kind") == "object"]
    free_entries = [e for e in object_entries if not e.get("held_by")]
    assert len(free_entries) == 0, (
        "V2 GC must root the package via the per-platform leaf digest from "
        "[tool.platforms]; package must NOT appear as collectable.\n"
        f"Free entries: {free_entries}"
    )


def test_clean_v1_lock_roots_via_legacy_index_path(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A committed V1 lock (``lock_version = 1`` with ``pinned`` index digest)
    still roots GC correctly via the legacy index-blob-read path — no forced
    upgrade and no read-path mutation.

    ADR §clean.rs: "V1: retain the index-blob-read walk in
    ``resolve_to_package_digests`` (presence-gated)."

    Scenario: hand-author a V1 ``ocx.lock`` whose ``pinned`` digest matches the
    live index (obtained by running a real ``ocx lock`` first, capturing the
    digest, then re-writing the lock file in V1 format).  ``ocx pull`` is then
    run against the V1 lock (legacy install path) to populate the object store.
    ``ocx clean --dry-run`` from a sibling project must show the package as held.
    """
    proj_a = tmp_path / "proj-a-v1gc"
    proj_b = tmp_path / "proj-b-v1gc"
    proj_b.mkdir(parents=True, exist_ok=True)

    repo, tag = _published_tool(ocx, tmp_path, "v1gc")
    proj_a.mkdir(parents=True, exist_ok=True)
    _write_ocx_toml(
        proj_a,
        f"""\
[tools]
the_tool = "{ocx.registry}/{repo}:{tag}"
""",
    )

    # Run `ocx lock` to get a real V2 lock (we need the real digest values).
    lock_result = _run(ocx, proj_a, "lock", "--no-pull")
    assert lock_result.returncode == EXIT_SUCCESS, (
        f"ocx lock failed for V1 GC test setup: rc={lock_result.returncode}\n"
        f"stderr:\n{lock_result.stderr}"
    )

    v2_lock_text = (proj_a / "ocx.lock").read_text()
    # Extract bare repository and one leaf digest from the V2 lock so we can
    # synthesise the V1 `pinned` field.
    repo_match = _re_clean.search(r'repository\s*=\s*"([^"]+)"', v2_lock_text)
    leaf_match = _LEAF_RE_CLEAN.search(v2_lock_text)
    assert repo_match and leaf_match, (
        "V2 lock must carry repository + leaf for V1-synthesis;\n" + v2_lock_text[:300]
    )
    bare_repo = repo_match.group(1)
    leaf_hex = leaf_match.group(1)

    # Preserve the real declaration_hash so the lock passes stale-check.
    decl_hash_match = _re_clean.search(
        r'declaration_hash\s*=\s*"(sha256:[0-9a-f]{64})"', v2_lock_text
    )
    assert decl_hash_match, "declaration_hash missing in V2 lock"
    decl_hash = decl_hash_match.group(1)

    # Overwrite with a V1 lock using the real `pinned` identifier built from
    # the V2 bare repo + leaf digest (this is how a consumer-written V1 lock
    # that was never upgraded looks).
    (proj_a / "ocx.lock").write_text(
        f"""\
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "{decl_hash}"
generated_by = "ocx 0.3.0"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "the_tool"
group = "default"
pinned = "{bare_repo}@sha256:{leaf_hex}"
"""
    )

    # ``ocx pull`` against the V1 lock must succeed (legacy index-digest path).
    pull_result = _run(ocx, proj_a, "pull")
    assert pull_result.returncode == EXIT_SUCCESS, (
        f"ocx pull on V1 lock must succeed via legacy path; "
        f"rc={pull_result.returncode}\nstderr:\n{pull_result.stderr}"
    )

    # GC from proj_b: the V1-locked package must still be held.
    entries = _run_clean_json(ocx, proj_b)
    object_entries = [e for e in entries if e.get("kind") == "object"]
    free_entries = [e for e in object_entries if not e.get("held_by")]
    assert len(free_entries) == 0, (
        "V1 GC path must root the package via the legacy index-blob walk; "
        "package must NOT appear as collectable.\n"
        f"Free entries: {free_entries}"
    )
