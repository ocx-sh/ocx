# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the shared OCX store (P1.3 specification).

These tests verify the zone-based store layout introduced by M2
(system_design_shared_store.md §5 M2):

- ``OCX_CACHE_DIR``  → blobs, layers (content zone, shareable)
- ``OCX_PACKAGES_DIR`` → packages (default = cache zone)
- ``OCX_STATE_DIR``  → symlinks, state, projects (per-instance, never shared)

All tests in this file are SPECIFICATION tests (contract-first TDD).
They are expected to FAIL against the current stub (feature absent)
because ``OCX_CACHE_DIR`` / ``OCX_STATE_DIR`` are not yet wired into
``FileStructure`` construction at the CLI context level.

Deferred tests requiring ``__OCX_TESTING_PUBLISH_PAUSE`` (added in P1.5):

    # TODO P1.5: test_concurrent_same_digest_publish_no_enoent
    # TODO P1.5: test_dest_override_no_destructive_race

Spec sources
------------
- system_design_shared_store.md §3 (zones), §5 M2 (layout resolver)
- plan_shared_store.md P1.3 (acceptance spec items)
- §5 M5 (test strategy: two-instance simulation via shared_store fixture)
"""
from __future__ import annotations

import json
from pathlib import Path
from uuid import uuid4

import pytest

from src.assertions import assert_symlink_exists, assert_not_exists
from src.helpers import make_package
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# UC1 zone contract tests
# ---------------------------------------------------------------------------


def test_separate_state_independent_pins(
    shared_store: "SharedStore",
    tmp_path: Path,
) -> None:
    """Two runners sharing a cache maintain independent install state.

    UC1 scenario: A and B share one content store (OCX_CACHE_DIR) but each
    has its own OCX_STATE_DIR.  A pins v1 and B pins v2 of the same package.
    Each runner's "current" must reflect its own pin, not the peer's.

    Requirement: system_design_shared_store.md §3 — "symlinks, state,
    projects ← state zone — per-instance — NEVER shared".

    Traced to: plan_shared_store P1.3 test_separate_state_independent_pins.
    """
    repo = f"t_{uuid4().hex[:8]}_shared_pins"

    # Both runners see the same registry so they push/pull the same blobs.
    runner_a = shared_store.runner_a
    runner_b = shared_store.runner_b

    # Publish both versions (A does the pushing; B will pull)
    v1 = make_package(runner_a, repo, "1.0.0", tmp_path, new=True)
    v2 = make_package(runner_a, repo, "2.0.0", tmp_path, new=False)

    # A pins v1, B pins v2.
    runner_a.json("package", "install", "--select", v1.fq)
    runner_b.json("package", "install", "--select", v2.fq)

    def _which_path(runner: OcxRunner, *args: str) -> Path:
        """Resolve `ocx package which` to the package-root path it reports."""
        result = runner.json("package", "which", *args)
        if isinstance(result, dict):
            # Single identifier per call here, so the mapping has exactly one
            # entry; take it by value rather than guessing the key (the key is
            # the resolved identifier string, which is not always `args[-1]`).
            assert len(result) == 1, f"expected a single `which` entry, got: {result!r}"
            raw = next(iter(result.values()))
        else:
            raw = result
        return Path(raw["path"] if isinstance(raw, dict) else raw).resolve()

    bare = f"{runner_a.registry}/{repo}"
    # Each runner's `current` symlink lives in its own per-instance OCX_STATE_DIR
    # and must resolve to its own pinned version's package root.
    current_a = _which_path(runner_a, "--current", bare)
    current_b = _which_path(runner_b, "--current", bare)

    # The candidate symlink for each pinned tag is the ground truth for that
    # version's content-addressed package root (distinct digests per version).
    candidate_v1 = _which_path(runner_a, "--candidate", v1.fq)
    candidate_v2 = _which_path(runner_b, "--candidate", v2.fq)

    # v1 and v2 have distinct content → distinct CAS package roots.
    assert candidate_v1 != candidate_v2, (
        f"v1 and v2 must resolve to distinct package roots; both = {candidate_v1}"
    )

    # A's current must point at v1's package root, B's at v2's — proving the
    # per-instance state zones hold independent selections over a shared cache.
    assert current_a == candidate_v1, (
        f"Runner A's current must resolve to v1.0.0's package root {candidate_v1}, "
        f"got: {current_a}"
    )
    assert current_b == candidate_v2, (
        f"Runner B's current must resolve to v2.0.0's package root {candidate_v2}, "
        f"got: {current_b}"
    )


def test_shared_cache_dedup_blobs_layers_packages(
    shared_store: "SharedStore",
    tmp_path: Path,
) -> None:
    """Content downloaded by A is reused by B without a second download.

    UC1 scenario: A installs a package.  B then installs the same version.
    The blobs, layers, and packages directories inside OCX_CACHE_DIR must
    contain each object exactly once — not duplicated per runner.

    Requirement: system_design_shared_store.md §3 — "content zone — one
    volume, shareable"; §1 UC1 "Download/extract/assemble once; dedup to
    1×content + N×state."

    Traced to: plan_shared_store P1.3 test_shared_cache_dedup_blobs_layers_packages.
    """
    repo = f"t_{uuid4().hex[:8]}_shared_dedup"
    runner_a = shared_store.runner_a
    runner_b = shared_store.runner_b
    shared_cache = shared_store.shared_cache

    pkg = make_package(runner_a, repo, "1.0.0", tmp_path, new=True)

    # A installs the package — content must land in the shared cache.
    runner_a.json("package", "install", "--select", pkg.fq)

    def _count_files(directory: Path) -> int:
        """Count regular files recursively, ignoring absent dirs."""
        if not directory.exists():
            return 0
        return sum(1 for _ in directory.rglob("*") if _.is_file())

    # After A installs, the shared cache must be populated with content.
    # If OCX_CACHE_DIR is not honoured (feature absent), these will be 0 and
    # the first assertion below will fire — confirming the test fails correctly.
    blobs_after_a = _count_files(shared_cache / "blobs")
    packages_after_a = _count_files(shared_cache / "packages")

    assert blobs_after_a > 0, (
        f"shared OCX_CACHE_DIR/blobs must be populated after A installs; "
        f"found {blobs_after_a} files — OCX_CACHE_DIR may not be honoured yet"
    )
    assert packages_after_a > 0, (
        f"shared OCX_CACHE_DIR/packages must be populated after A installs; "
        f"found {packages_after_a} files"
    )

    # Snapshot before B installs.
    blobs_before_b = _count_files(shared_cache / "blobs")
    layers_before_b = _count_files(shared_cache / "layers")
    packages_before_b = _count_files(shared_cache / "packages")

    # B installs the same version — content must already be present (shared);
    # file counts must not increase.
    runner_b.json("package", "install", "--select", pkg.fq)

    blobs_after_b = _count_files(shared_cache / "blobs")
    layers_after_b = _count_files(shared_cache / "layers")
    packages_after_b = _count_files(shared_cache / "packages")

    assert blobs_after_b == blobs_before_b, (
        f"Blobs must not be duplicated: {blobs_before_b} before, {blobs_after_b} after "
        f"B installed the same package"
    )
    assert layers_after_b == layers_before_b, (
        f"Layers must not be duplicated: {layers_before_b} before, {layers_after_b} after"
    )
    assert packages_after_b == packages_before_b, (
        f"Packages must not be duplicated: {packages_before_b} before, {packages_after_b} after"
    )


def test_state_dir_holds_symlinks_state_projects(
    shared_store: "SharedStore",
    tmp_path: Path,
) -> None:
    """Install symlinks land in the per-instance OCX_STATE_DIR, not shared cache.

    After A installs a package, the candidate symlink must reside under A's
    OCX_STATE_DIR / symlinks/, and must not appear anywhere inside the
    shared OCX_CACHE_DIR.  B's state directory must be unaffected.

    Requirement: system_design_shared_store.md §3 — "symlinks, state,
    projects ← per-instance — NEVER shared".

    Traced to: plan_shared_store P1.3 test_state_dir_holds_symlinks_state_projects.
    """
    repo = f"t_{uuid4().hex[:8]}_state_isolation"
    runner_a = shared_store.runner_a
    shared_cache = shared_store.shared_cache
    state_a = runner_a.env["OCX_STATE_DIR"]
    state_b = shared_store.runner_b.env["OCX_STATE_DIR"]

    pkg = make_package(runner_a, repo, "1.0.0", tmp_path, new=True)
    runner_a.json("package", "install", "--select", pkg.fq)

    # Symlinks must be under A's state dir.
    state_a_symlinks = Path(state_a) / "symlinks"
    assert state_a_symlinks.exists(), (
        f"OCX_STATE_DIR/symlinks must exist for A after install; "
        f"checked: {state_a_symlinks}"
    )

    # The shared cache must NOT contain symlinks/ — that is state-zone content.
    shared_symlinks = shared_cache / "symlinks"
    assert not shared_symlinks.exists(), (
        f"shared OCX_CACHE_DIR must NOT contain a symlinks/ directory; "
        f"found: {shared_symlinks}"
    )

    # B's state dir must be empty (untouched by A's install).
    state_b_symlinks = Path(state_b) / "symlinks"
    assert not state_b_symlinks.exists(), (
        f"B's OCX_STATE_DIR/symlinks must not exist — A's install must not "
        f"affect B's state; checked: {state_b_symlinks}"
    )


def test_default_packages_colocate_with_cache(
    ocx_binary: Path,
    registry: str,
    tmp_path: Path,
) -> None:
    """When OCX_PACKAGES_DIR is unset, packages/ lands under OCX_CACHE_DIR.

    System design §5 M2: "packages defaults to resolved cache; tags defaults
    under cache."  This test uses a single runner with only OCX_CACHE_DIR
    set (no OCX_PACKAGES_DIR) and verifies the packages store appears inside
    OCX_CACHE_DIR rather than OCX_HOME.

    Traced to: plan_shared_store P1.3 test_default_packages_colocate_with_cache.
    """
    cache_dir = tmp_path / "custom-cache"
    cache_dir.mkdir()
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    # Runner with only OCX_CACHE_DIR set — no OCX_PACKAGES_DIR.
    runner = OcxRunner(
        ocx_binary,
        home_dir,
        registry,
        extra_env={"OCX_CACHE_DIR": str(cache_dir)},
    )

    repo = f"t_{uuid4().hex[:8]}_pkg_colocate"
    pkg = make_package(runner, repo, "1.0.0", tmp_path / "pkg", new=True)
    runner.json("package", "install", "--select", pkg.fq)

    # packages/ must be under the cache dir, not OCX_HOME.
    packages_under_cache = cache_dir / "packages"
    packages_under_home = home_dir / "packages"

    assert packages_under_cache.exists(), (
        f"packages/ must be under OCX_CACHE_DIR ({cache_dir}) when "
        f"OCX_PACKAGES_DIR is unset; checked: {packages_under_cache}"
    )
    assert not packages_under_home.exists(), (
        f"packages/ must NOT be under OCX_HOME ({home_dir}) when "
        f"OCX_CACHE_DIR is set; found: {packages_under_home}"
    )
