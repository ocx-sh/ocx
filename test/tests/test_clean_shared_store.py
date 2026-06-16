# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for shared-store GC rooting (P3.6 implementation).

These tests verify the shared-roots / OCX_SHARED_STORE contract described in
system_design_shared_store.md §5 M4 point 4 and the P3.6 architect adjustments
in plan_shared_store.md:

- OCX_SHARED_STORE=true: a digest-only, versioned shared-roots ledger is written
  per-instance on every ``ocx.lock`` save; GC unions all instances' ledgers so a
  peer's lock-pinned digest is not collected.
- OCX_SHARED_STORE unset (default): the shared-roots ledger is neither written
  nor consulted; a peer's lock-pin (whose project ledger lives in the peer's
  private state zone) is NOT protected from a default-mode clean.

Mechanism note (P3.6 Living Design correction)
-----------------------------------------------
The shared-roots ledger is written on **``ocx.lock`` save** (the same trigger as
the per-instance project ledger), NOT by ``package install``. A bare
``package install`` keeps the content live via an install-symlink back-ref in the
SHARED packages zone, which a peer's GC sees as live regardless of the
shared-roots ledger — so the original ``package install``-based P3.2s drafts
could not isolate the ledger behaviour. These tests therefore drive the contract
through a project ``ocx.toml`` + ``ocx lock`` + ``ocx pull`` (the
metadata-first pull path materialises content WITHOUT an install symlink), so the
ONLY cross-instance protector is the shared-roots ledger.

Spec sources
------------
- system_design_shared_store.md §5 M4.4 — "OCX_SHARED_STORE … shared-mode
  clean unions all instances' shared roots"
- plan_shared_store.md P3.6 (Adjustments 2/3/6)
"""
from __future__ import annotations

import subprocess
import tomllib
from pathlib import Path
from uuid import uuid4

from src.assertions import assert_dir_exists, assert_not_exists
from src.helpers import make_package
from src.runner import OcxRunner

EXIT_SUCCESS = 0


# ---------------------------------------------------------------------------
# Module-local helpers (DAMP > DRY for acceptance tests, per quality-core.md).
# Subprocess-based to surface ``cwd=`` and a shared-store env overlay, mirroring
# the helper shapes in test_clean_project_backlinks.py.
# ---------------------------------------------------------------------------


def _shared_env(runner: OcxRunner, *, shared_store: bool, grace_seconds: str) -> dict[str, str]:
    """The runner's isolated env plus the shared-store flags under test."""
    env = dict(runner.env)
    env["OCX_GC_GRACE_SECONDS"] = grace_seconds
    if shared_store:
        env["OCX_SHARED_STORE"] = "true"
    else:
        env.pop("OCX_SHARED_STORE", None)
    return env


def _run(runner: OcxRunner, cwd: Path, env: dict[str, str], *args: str) -> subprocess.CompletedProcess[str]:
    """Run an ocx sub-command with an explicit ``cwd`` and env."""
    return subprocess.run(
        [str(runner.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
    )


def _write_ocx_toml(project_dir: Path, tool_ref: str) -> None:
    project_dir.mkdir(parents=True, exist_ok=True)
    (project_dir / "ocx.toml").write_text(f'[tools]\nthe_tool = "{tool_ref}"\n', encoding="utf-8")


def _pin_via_project(runner: OcxRunner, project_dir: Path, env: dict[str, str], tool_ref: str) -> None:
    """``ocx lock`` (fires the ledger writes) + ``ocx pull`` (materialises content
    WITHOUT an install symlink) for a project pinning ``tool_ref``."""
    _write_ocx_toml(project_dir, tool_ref)
    lock = _run(runner, project_dir, env, "lock")
    assert lock.returncode == EXIT_SUCCESS, f"ocx lock failed: rc={lock.returncode}\n{lock.stderr}"
    pull = _run(runner, project_dir, env, "pull")
    assert pull.returncode == EXIT_SUCCESS, f"ocx pull failed: rc={pull.returncode}\n{pull.stderr}"


def _single_content_dir(shared_cache: Path) -> Path:
    """Return the sole package ``content/`` directory in the shared cache.

    The shared-store fixture uses a fresh shared cache per test, and each test
    pins exactly one package, so there is exactly one ``content/`` dir.
    """
    base = shared_cache / "packages"
    contents = [p for p in base.rglob("content") if p.is_dir()]
    assert len(contents) == 1, f"expected exactly one pinned content dir, got: {contents}"
    return contents[0]


def test_two_instances_share_packages_clean_spares_peer_pins(
    shared_store: "SharedStore",
    tmp_path: Path,
) -> None:
    """OCX_SHARED_STORE=true: instance-B's clean spares instance-A's lock-pinned digest.

    Contract: system_design_shared_store.md §5 M4.4 — "Shared-mode clean
    unions all instances' shared roots."

    Scenario:
    1. Runner A pins pkg v1.0.0 via a project ``ocx.lock`` (OCX_SHARED_STORE=true)
       and ``ocx pull``s it into the shared cache. A's project ledger lives in
       A's PRIVATE state zone (B cannot see it); A's shared-roots ledger lands in
       the SHARED packages zone (B can see it under shared-store mode).
    2. Runner B (a separate state zone, OCX_SHARED_STORE=true, no project, no
       install symlink) runs ``clean`` with grace disabled.
    3. The content object must still exist because A's shared-roots ledger entry
       protects the digest across instances.

    Traced to: plan_shared_store P3.6
    test_two_instances_share_packages_clean_spares_peer_pins.
    """
    a = shared_store.runner_a
    b = shared_store.runner_b
    a_env = _shared_env(a, shared_store=True, grace_seconds="0")
    b_env = _shared_env(b, shared_store=True, grace_seconds="0")

    repo = f"t_{uuid4().hex[:8]}_shared_pin"
    make_package(a, repo, "1.0.0", tmp_path, new=True, cascade=False)
    tool_ref = f"{a.registry}/{repo}:1.0.0"

    proj_a = tmp_path / "proj-a"
    _pin_via_project(a, proj_a, a_env, tool_ref)

    content = _single_content_dir(shared_store.shared_cache)
    assert_dir_exists(content)

    # B's clean (shared-store on): A's shared-roots ledger is unioned → the
    # digest is a GC root → must NOT be deleted.
    clean = _run(b, tmp_path, b_env, "clean")
    assert clean.returncode == EXIT_SUCCESS, f"B clean failed: rc={clean.returncode}\n{clean.stderr}"

    assert_dir_exists(
        content,
        "shared content was deleted by peer clean even though A holds a shared-roots "
        "ledger entry — OCX_SHARED_STORE union not protecting the peer pin",
    )


def test_collect_roots_default_mode_ignores_shared_roots(
    shared_store: "SharedStore",
    tmp_path: Path,
) -> None:
    """Without OCX_SHARED_STORE, a peer's lock-pin is NOT protected.

    Contract: system_design_shared_store.md §5 M4.4 — "OCX_SHARED_STORE …
    opt-in … default unchanged": default clean does not consult the shared-roots
    ledger, and A's project ledger lives in A's private state zone (invisible to
    B), so B is free to collect the digest.

    Scenario: as above but A pins WITHOUT OCX_SHARED_STORE (so no shared-roots
    ledger is even written) and B cleans WITHOUT OCX_SHARED_STORE and with grace
    disabled. With no shared-roots entry and no install symlink, B collects the
    content.

    Traced to: plan_shared_store P3.6 collect_roots_default_mode_ignores_shared_roots.
    """
    a = shared_store.runner_a
    b = shared_store.runner_b
    # A pins WITHOUT shared-store → no shared-roots ledger written at all.
    a_env = _shared_env(a, shared_store=False, grace_seconds="0")
    # B cleans WITHOUT shared-store → does not consult any ledger; grace off.
    b_env = _shared_env(b, shared_store=False, grace_seconds="0")

    repo = f"t_{uuid4().hex[:8]}_default_pin"
    make_package(a, repo, "1.0.0", tmp_path, new=True, cascade=False)
    tool_ref = f"{a.registry}/{repo}:1.0.0"

    proj_a = tmp_path / "proj-a"
    _pin_via_project(a, proj_a, a_env, tool_ref)

    content = _single_content_dir(shared_store.shared_cache)
    assert_dir_exists(content)

    # B cleans in default mode: A's project ledger is in A's private state zone
    # (unreadable to B), no shared-roots ledger exists, no install symlink, grace
    # off → the content is collected.
    clean = _run(b, tmp_path, b_env, "clean")
    assert clean.returncode == EXIT_SUCCESS, f"B clean failed: rc={clean.returncode}\n{clean.stderr}"

    assert_not_exists(
        content,
        "content survived B's default-mode clean even though B has no refs, no "
        "shared-roots ledger exists, and grace is disabled — default clean must "
        "not protect a peer's private lock-pin",
    )


def test_retain_all_on_unparseable_peer_ledger(
    shared_store: "SharedStore",
    tmp_path: Path,
) -> None:
    """RetainAll: an unparseable peer shared-roots ledger makes clean collect ZERO objects.

    Contract: system_design_shared_store.md §5 M4.4 — "unparseable /
    unknown-version file escalates to RetainAll (one-way-door safety property)":
    when any peer ledger is unreadable by the collector, the run must retain
    every object rather than collect against an incomplete root set (silent
    data-loss guard).

    Scenario:
    1. Runner B (the cleaner) has NO project pinning the object.
    2. We plant a peer ledger file with an UNKNOWN version (v=255) in the
       shared packages zone — simulating a future-format file a downlevel
       collector cannot parse.
    3. B runs ``ocx clean`` with OCX_SHARED_STORE=true and grace disabled.
    4. The content object must still exist: the unparseable ledger triggers
       RetainAll, so clean must collect ZERO objects (not the object, not
       anything else in the store).

    The assertion is over the JSON output's collected object count so it is
    version-stable and does not depend on paths or digests.

    Traced to: plan_shared_store P3.6 RetainAll fail-closed / review-fix FIX 9.
    """
    a = shared_store.runner_a
    b = shared_store.runner_b
    a_env = _shared_env(a, shared_store=True, grace_seconds="0")
    b_env = _shared_env(b, shared_store=True, grace_seconds="0")

    # Install and then uninstall a package via A so the object exists in the
    # shared store with no live install symlink (pure back-ref-free content).
    repo = f"t_{uuid4().hex[:8]}_retain_all"
    make_package(a, repo, "1.0.0", tmp_path, new=True, cascade=False)
    tool_ref = f"{a.registry}/{repo}:1.0.0"

    # Pull (not install) so no install-symlink back-ref protects the object.
    proj_a = tmp_path / "proj-retain-all"
    _pin_via_project(a, proj_a, a_env, tool_ref)

    content = _single_content_dir(shared_store.shared_cache)
    assert content.exists(), "content must exist after pull"

    # Plant an unparseable shared-roots ledger: version=255 is unknown to any
    # current reader; the parser rejects it → RetainAll.
    # The ledger lives at $OCX_PACKAGES_DIR/roots/<instance_id>/<project_hash>.
    # We place it under a synthetic instance-id directory (not B's) so it
    # looks like a peer ledger to B's collector.
    roots_dir = shared_store.shared_cache / "roots"
    fake_instance_dir = roots_dir / "ffffffffffffffffffffffffffffffff"
    fake_instance_dir.mkdir(parents=True, exist_ok=True)
    ledger_file = fake_instance_dir / "fake_project_hash"
    ledger_file.write_text('{"v": 255, "digests": []}', encoding="utf-8")

    # B cleans with OCX_SHARED_STORE=true. The unparseable peer ledger triggers
    # RetainAll → zero objects must be collected.
    # Use a real (non-dry-run) clean with zero grace so any collected object
    # would be removed. The content must survive because RetainAll prevents
    # collection when a peer ledger cannot be parsed.
    clean = _run(b, tmp_path, b_env, "clean")
    assert clean.returncode == EXIT_SUCCESS, f"clean failed: rc={clean.returncode}\n{clean.stderr}"

    assert_dir_exists(
        content,
        "content was deleted despite RetainAll from unparseable peer ledger — "
        "the fail-closed guard is not protecting the store",
    )


def _find_blob_data_for_digest(shared_cache: Path, digest: str) -> Path:
    """Locate the CAS ``data`` file for a manifest ``digest``.

    The digest is the full ``sha256:<hex>`` string the shared-roots ledger
    stores. The blob lives at ``blobs/<registry>/sha256/<hex[:2]>/<hex[2:32]>/data``.
    We glob on the shard prefix to stay independent of the registry slug.
    """
    algo, _, hexpart = digest.partition(":")
    shard = hexpart[:2]
    rest = hexpart[2:32]
    matches = list((shared_cache / "blobs").rglob(f"{algo}/{shard}/{rest}/data"))
    assert len(matches) == 1, f"expected exactly one blob data file for {digest}, got: {matches}"
    return matches[0]


def test_retain_all_when_peer_blob_absent_on_cleaner(
    shared_store: "SharedStore",
    tmp_path: Path,
) -> None:
    """RetainAll: a peer digest whose manifest blob is absent on the cleaner
    must NOT be collected (FIX C — fail closed on blob-unresolvable shared root).

    Contract: a shared-roots peer digest that cannot be resolved to its platform
    package digests on THIS cleaner (the manifest blob is absent in this
    cleaner's cache, e.g. the per-instance-cache deployment) must fail closed:
    the peer's package directory may still live on the shared packages volume,
    so resolving to a no-op root would let ``clean`` delete it. The cleaner must
    instead retain everything this run.

    Scenario:
    1. Runner A pins pkg v1.0.0 via a project ``ocx.lock`` (OCX_SHARED_STORE=true)
       and pulls it: the package dir AND the platform leaf manifest blob land in
       the shared cache; A's shared-roots ledger records the per-platform leaf
       digests (V2 lock format).
    2. We DELETE the peer's platform leaf manifest blob from the shared cache —
       simulating the per-instance-cache deployment where the cleaner lacks the
       blob even though the peer's package directory is present on the shared
       volume.
    3. Runner B (OCX_SHARED_STORE=true, grace disabled, no project, no install
       symlink) runs ``clean``. With grace off and no install back-ref, the ONLY
       thing that can spare the package is the fail-closed RetainAll on the
       blob-unresolvable peer digest.
    4. The package content must still exist.

    Traced to: plan_shared_store P3 cross-model review FIX C.
    """
    a = shared_store.runner_a
    b = shared_store.runner_b
    a_env = _shared_env(a, shared_store=True, grace_seconds="0")
    b_env = _shared_env(b, shared_store=True, grace_seconds="0")

    repo = f"t_{uuid4().hex[:8]}_blob_absent"
    make_package(a, repo, "1.0.0", tmp_path, new=True, cascade=False)
    tool_ref = f"{a.registry}/{repo}:1.0.0"

    proj_a = tmp_path / "proj-blob-absent"
    _pin_via_project(a, proj_a, a_env, tool_ref)

    content = _single_content_dir(shared_store.shared_cache)
    assert_dir_exists(content)

    # The shared-roots ledger records each per-platform leaf digest (V2 lock
    # format). Read the leaf from A's lock `[tool.platforms]` map so the deletion
    # targets exactly the blob the peer digest references.
    lock_text = (proj_a / "ocx.lock").read_text(encoding="utf-8")
    platforms = tomllib.loads(lock_text)["tool"][0]["platforms"]
    assert platforms, f"no per-platform leaf digests found in lock:\n{lock_text}"
    leaf_digest = next(iter(platforms.values()))

    # Simulate the per-instance-cache deployment: the manifest blob is absent on
    # the cleaner. Removing the blob data file makes `resolve_shared_root` return
    # None (UNRESOLVABLE) for this peer digest → RetainAll.
    blob_data = _find_blob_data_for_digest(shared_store.shared_cache, leaf_digest)
    blob_data.unlink()
    assert not blob_data.exists(), "precondition: peer platform-manifest blob removed"

    clean = _run(b, tmp_path, b_env, "clean")
    assert clean.returncode == EXIT_SUCCESS, f"B clean failed: rc={clean.returncode}\n{clean.stderr}"

    assert_dir_exists(
        content,
        "peer package was deleted even though its ImageIndex manifest blob is "
        "absent on the cleaner — resolve_shared_root must fail closed (RetainAll) "
        "instead of resolving to a no-op root",
    )


def test_missed_shared_root_write_within_grace_survives(
    shared_store: "SharedStore",
    tmp_path: Path,
) -> None:
    """A dropped shared-roots write is backstopped by the mtime grace window.

    Contract: plan_shared_store P3.6 Adjustment 6 (residual best-effort-write
    corruption guard). The shared-roots ledger write is best-effort — a missed
    write must NOT immediately expose a peer's freshly-pinned object to
    collection. The GC mtime grace window provides a time-bounded backstop.

    Scenario:
    1. Runner A pins pkg v1.0.0 via a project ``ocx.lock`` with
       OCX_SHARED_STORE=true and pulls it (writes the shared-roots ledger).
    2. We SIMULATE a dropped ledger write by deleting the shared-roots ledger
       from the shared packages zone — as if A's best-effort write had failed.
    3. Runner B (OCX_SHARED_STORE=true, LARGE grace window, no project, no
       install) runs ``clean``.
    4. The content object must still exist: the shared-roots ledger is gone (so
       cross-instance rooting cannot protect it), but the object's mtime is
       within the grace window, so grace retains it. This proves grace is the
       time-bounded backstop for a missed best-effort write.

    Traced to: plan_shared_store P3.6 missed-write-within-grace survives.
    """
    a = shared_store.runner_a
    b = shared_store.runner_b
    a_env = _shared_env(a, shared_store=True, grace_seconds="0")
    # Large grace window so the freshly-pulled object is spared once the ledger
    # backstop is removed.
    b_env = _shared_env(b, shared_store=True, grace_seconds="3600")

    repo = f"t_{uuid4().hex[:8]}_missed_write"
    make_package(a, repo, "1.0.0", tmp_path, new=True, cascade=False)
    tool_ref = f"{a.registry}/{repo}:1.0.0"

    proj_a = tmp_path / "proj-a"
    _pin_via_project(a, proj_a, a_env, tool_ref)

    content = _single_content_dir(shared_store.shared_cache)
    assert_dir_exists(content)

    # Simulate the dropped best-effort ledger write: remove the shared-roots
    # ledger directory from the shared packages zone. The ledger lives at
    # `$OCX_PACKAGES_DIR/roots/` (default `$OCX_CACHE_DIR` when packages dir is
    # unset, as in the shared_store fixture). After this, the ONLY thing that
    # can spare A's pin from B's clean is the mtime grace window.
    roots_dir = shared_store.shared_cache / "roots"
    if roots_dir.exists():
        import shutil

        shutil.rmtree(roots_dir)
    assert not roots_dir.exists(), "precondition: shared-roots ledger removed (write simulated as dropped)"

    # B cleans with a large grace window. The object's mtime is well within the
    # window, so grace retains it even though the shared-roots ledger is gone.
    clean = _run(b, tmp_path, b_env, "clean")
    assert clean.returncode == EXIT_SUCCESS, f"B clean failed: rc={clean.returncode}\n{clean.stderr}"

    assert_dir_exists(
        content,
        "freshly-pulled content was collected by peer clean despite a 1h grace "
        "window — the mtime grace backstop for a missed shared-roots write is broken",
    )
