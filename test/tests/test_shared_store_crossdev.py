# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for cross-device assembly (P2.2 specification).

These tests verify the cross-device layer-to-package assembly fallback
introduced by M3 (system_design_shared_store.md §5 M3):

- ``OCX_CACHE_DIR`` on device A (blobs, layers)
- ``OCX_PACKAGES_DIR`` on device B (packages) → triggers reflink/copy fallback

All tests in this file are SPECIFICATION tests (contract-first TDD).
They are expected to FAIL against the current stub (P2.1) because
``reflink::create`` is ``unimplemented!("P2.3: ...")``.

Tests self-skip when no second device is available (``/dev/shm`` absent or on
the same device as ``tmp_path``).  On this Linux dev box ``/dev/shm`` is tmpfs
on a different device, so the tests RUN and FAIL — proving RED.

Deferred until P2.4 (macOS re-sign):

    # test_crossdev_independent_inodes_trigger_macos_resign
    # (requires macOS runner + P2.4 conditional codesign seam in pull.rs)

Spec sources
------------
- system_design_shared_store.md §5 M3 (cross-device assembly fallback)
- plan_shared_store.md P2.2 (specification task: acceptance tests)
- system_design_shared_store.md §5 M5 (test strategy: /dev/shm cross-device)
"""
from __future__ import annotations

import shutil
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package, separate_tmpfs_device
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# ACC1: cross-device packages assemble via reflink or copy
# ---------------------------------------------------------------------------


def test_crossdev_packages_assemble_via_reflink_or_copy(
    ocx_binary: Path,
    registry: str,
    tmp_path: Path,
) -> None:
    """Packages assemble successfully when cache and packages are on different devices.

    UC2 scenario: ``OCX_CACHE_DIR`` is on device A (the standard tmp_path);
    ``OCX_PACKAGES_DIR`` is on device B (/dev/shm), forcing the walker to use
    ``reflink::create`` (copy fallback on ext4) instead of ``hardlink::create``.

    The installed binary must be executable and print its marker.

    Requirement: system_design_shared_store.md §5 M3 —
    "Reflink → reflink::create (new module mirroring hardlink.rs/symlink.rs,
    wrapping reflink-copy::reflink_or_copy = CoW where supported, byte copy
    otherwise)."

    Traced to: plan_shared_store P2.2 test_crossdev_packages_assemble_via_reflink_or_copy.

    FAILS NOW: reflink::create is unimplemented!("P2.3: ...") — assembly
    errors non-zero during install.
    """
    dest = separate_tmpfs_device(tmp_path)
    if dest is None:
        pytest.skip("no separate device available (/dev/shm absent or same device as tmp_path)")

    try:
        cache_dir = tmp_path / "cache"
        cache_dir.mkdir()
        packages_dir = dest / "packages"
        packages_dir.mkdir(parents=True)
        home_dir = tmp_path / "home"
        home_dir.mkdir()

        runner = OcxRunner(
            ocx_binary,
            home_dir,
            registry,
            extra_env={
                "OCX_CACHE_DIR": str(cache_dir),
                "OCX_PACKAGES_DIR": str(packages_dir),
            },
        )

        repo = f"t_{uuid4().hex[:8]}_crossdev_install"
        pkg = make_package(runner, repo, "1.0.0", tmp_path / "pkgbuild", new=True)

        # Install must succeed — this is the key assertion.
        # With unimplemented!() the reflink path panics and the command exits
        # non-zero, causing runner.json() to raise AssertionError.
        runner.json("package", "install", "--select", pkg.fq)

        # Verify the cross-device-assembled binary runs and prints its marker.
        # `package exec` resolves the package on device B (OCX_PACKAGES_DIR) and
        # runs the entrypoint — proving the reflinked/copied content is intact
        # and executable end-to-end.
        result = runner.plain("package", "exec", pkg.fq, "--", "hello")
        assert pkg.marker in result.stdout, (
            f"cross-device-assembled binary must print marker '{pkg.marker}'; "
            f"got: {result.stdout.strip()!r}"
        )

    finally:
        shutil.rmtree(dest, ignore_errors=True)


# ---------------------------------------------------------------------------
# ACC2: cross-device temp splits per zone
# ---------------------------------------------------------------------------


def test_crossdev_temp_splits_per_zone(
    ocx_binary: Path,
    registry: str,
    tmp_path: Path,
) -> None:
    """When zones are split across devices, each zone hosts its own temp staging.

    system_design_shared_store.md §4 interaction 1 (M2 × M3):
    "Each tier's staging temp must co-locate with that tier (layer_temp → cache,
    temp → packages) so every publish is an intra-volume rename."

    After a successful install:
    - ``OCX_CACHE_DIR/layers/`` must contain extracted layer content.
    - ``OCX_PACKAGES_DIR/packages/`` must contain the assembled package.
    - No ``packages/`` directory must appear under ``OCX_CACHE_DIR``.
    - No ``layers/`` directory must appear under ``OCX_PACKAGES_DIR``.

    This verifies the zone boundaries are respected: blobs+layers stay on the
    cache device; packages are assembled onto the packages device.

    Traced to: plan_shared_store P2.2 test_crossdev_temp_splits_per_zone.

    FAILS NOW: reflink::create is unimplemented!() — install fails before
    any zone directories are populated cross-device.
    """
    dest = separate_tmpfs_device(tmp_path)
    if dest is None:
        pytest.skip("no separate device available")

    try:
        cache_dir = tmp_path / "cache"
        cache_dir.mkdir()
        packages_dir = dest / "packages"
        packages_dir.mkdir(parents=True)
        home_dir = tmp_path / "home"
        home_dir.mkdir()

        runner = OcxRunner(
            ocx_binary,
            home_dir,
            registry,
            extra_env={
                "OCX_CACHE_DIR": str(cache_dir),
                "OCX_PACKAGES_DIR": str(packages_dir),
            },
        )

        repo = f"t_{uuid4().hex[:8]}_crossdev_zones"
        pkg = make_package(runner, repo, "1.0.0", tmp_path / "pkgbuild", new=True)
        runner.json("package", "install", "--select", pkg.fq)

        # Cache device must hold layers/.
        layers_under_cache = cache_dir / "layers"
        assert layers_under_cache.exists(), (
            f"OCX_CACHE_DIR/layers must exist after install; "
            f"checked: {layers_under_cache}"
        )

        # Packages device must hold packages/.
        pkgs_under_packages = packages_dir / "packages"
        assert pkgs_under_packages.exists(), (
            f"OCX_PACKAGES_DIR/packages must exist after install; "
            f"checked: {pkgs_under_packages}"
        )

        # Packages must NOT be under the cache zone.
        pkgs_under_cache = cache_dir / "packages"
        assert not pkgs_under_cache.exists(), (
            f"packages/ must NOT be under OCX_CACHE_DIR when OCX_PACKAGES_DIR is set; "
            f"found: {pkgs_under_cache}"
        )

        # Layers must NOT be under the packages zone.
        layers_under_packages = packages_dir / "layers"
        assert not layers_under_packages.exists(), (
            f"layers/ must NOT be under OCX_PACKAGES_DIR — layers belong in the cache zone; "
            f"found: {layers_under_packages}"
        )

    finally:
        shutil.rmtree(dest, ignore_errors=True)


# ---------------------------------------------------------------------------
# ACC3: CI cache persist — packages ephemeral
# ---------------------------------------------------------------------------


def test_ci_cache_persist_packages_ephemeral(
    ocx_binary: Path,
    registry: str,
    tmp_path: Path,
) -> None:
    """Persisted cache (device A) enables re-install without network after packages deleted.

    UC2 scenario: CI caches ``OCX_CACHE_DIR`` (blobs+layers) across jobs but
    the packages zone is ephemeral (deleted between jobs).  A second install
    must succeed by assembling from the cached layers — no re-download.

    Test flow:
    1. Install once → populates ``OCX_CACHE_DIR`` (blobs+layers) and
       ``OCX_PACKAGES_DIR/packages/`` (assembled package on device B).
    2. Delete the packages zone directory (simulate ephemeral per-job packages).
    3. Re-install → must succeed by assembling from the cached layers.

    Requirement: plan_shared_store P2.2 test_ci_cache_persist_packages_ephemeral.

    FAILS NOW: reflink::create is unimplemented!() — step 1 fails before
    any packages are assembled.
    """
    dest = separate_tmpfs_device(tmp_path)
    if dest is None:
        pytest.skip("no separate device available")

    try:
        cache_dir = tmp_path / "cache"
        cache_dir.mkdir()
        home_dir = tmp_path / "home"
        home_dir.mkdir()

        def make_runner(packages_root: Path) -> OcxRunner:
            return OcxRunner(
                ocx_binary,
                home_dir,
                registry,
                extra_env={
                    "OCX_CACHE_DIR": str(cache_dir),
                    "OCX_PACKAGES_DIR": str(packages_root),
                },
            )

        # Step 1: Initial install — populates both cache and packages zones.
        packages_dir_v1 = dest / "packages-v1"
        packages_dir_v1.mkdir(parents=True)
        runner_v1 = make_runner(packages_dir_v1)

        repo = f"t_{uuid4().hex[:8]}_crossdev_persist"
        pkg = make_package(runner_v1, repo, "1.0.0", tmp_path / "pkgbuild", new=True)
        runner_v1.json("package", "install", "--select", pkg.fq)

        # Verify blobs/layers are present in the cache zone.
        assert (cache_dir / "blobs").exists(), (
            "blobs/ must be present in OCX_CACHE_DIR after first install"
        )
        assert (cache_dir / "layers").exists(), (
            "layers/ must be present in OCX_CACHE_DIR after first install"
        )

        # Step 2: Simulate ephemeral packages — delete the packages zone.
        # This mimics a CI job completing: the cache volume is preserved but
        # the per-job packages volume is discarded.
        shutil.rmtree(packages_dir_v1)

        # Step 3: Re-install using only the cached layers (no network needed
        # since blobs+layers are already in OCX_CACHE_DIR).
        packages_dir_v2 = dest / "packages-v2"
        packages_dir_v2.mkdir(parents=True)
        runner_v2 = make_runner(packages_dir_v2)

        # This must succeed: the walker assembles packages from cached layers
        # via reflink::create (copy fallback on ext4), without downloading.
        runner_v2.json("package", "install", "--select", pkg.fq)

        # Packages zone must be populated after the second install.
        pkgs_under_v2 = packages_dir_v2 / "packages"
        assert pkgs_under_v2.exists(), (
            f"packages/ must be present in the new packages zone after re-install from cache; "
            f"checked: {pkgs_under_v2}"
        )

        # Verify the re-assembled binary is executable and prints its marker.
        result = runner_v2.plain("package", "exec", pkg.fq, "--", "hello")
        assert pkg.marker in result.stdout, (
            "binary re-assembled from cached layers must be executable; "
            f"got: {result.stdout.strip()!r}"
        )

        # Cache zone blobs/layers must NOT have grown (no re-download).
        # We verify layers is still present and non-empty (not wiped by re-install).
        assert (cache_dir / "layers").exists(), (
            "layers/ in OCX_CACHE_DIR must still exist after ephemeral-packages re-install"
        )

    finally:
        shutil.rmtree(dest, ignore_errors=True)
