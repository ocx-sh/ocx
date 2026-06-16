# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for shared-store GC grace period (P3 specification).

These tests verify the mtime-grace contract described in
system_design_shared_store.md §5 M4 point 3:

- Objects younger than OCX_GC_GRACE_SECONDS are spared even when unreferenced.
- The grace window is the primary TOCTOU defense for the cross-instance case.

All tests are SPECIFICATION tests (contract-first TDD, P3.2s).
They MUST FAIL against the current stubs (RED state) — that is the goal of
this phase.

Spec sources
------------
- system_design_shared_store.md §5 M4 — "In delete_objects, skip entry-dir
  mtime younger than OCX_GC_GRACE_SECONDS (default 600)"
- plan_shared_store.md P3.2s — grace predicate acceptance tests
"""
from __future__ import annotations

from pathlib import Path

from src.assertions import assert_dir_exists, assert_not_exists
from src.helpers import make_package
from src.runner import OcxRunner


def test_clean_spares_freshly_pulled_object(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Freshly installed+uninstalled package survives clean within the grace window.

    Contract: system_design_shared_store.md §5 M4.3 —
    "skip entry-dir mtime younger than OCX_GC_GRACE_SECONDS (default 600)".

    A package installed and immediately uninstalled has an mtime at most a few
    seconds old.  With the default 600 s grace window the object must NOT be
    collected by the immediately following clean.

    Traced to: plan_shared_store P3.2s test_clean_spares_freshly_pulled_object.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)
    result = ocx.json("package", "install", pkg.short)
    content = Path(result[pkg.short]["path"]).resolve()

    assert_dir_exists(content)

    ocx.plain("package", "uninstall", pkg.short)

    # Default grace = 600 s.  Object just created — must survive.
    ocx.plain("clean")

    assert_dir_exists(
        content,
        "freshly uninstalled object was GC'd within the grace window — "
        "OCX_GC_GRACE_SECONDS grace predicate not implemented",
    )


def test_clean_collects_object_after_grace_expired(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """An unreferenced object older than grace IS collected.

    Contract: system_design_shared_store.md §5 M4.3 — grace applies only when
    mtime is *younger* than the threshold; older objects are fair game.

    Uses OCX_GC_GRACE_SECONDS=0 to disable grace entirely (same contract: 0
    disables the window, so any unreferenced object is collectible immediately).

    Traced to: plan_shared_store P3.2s test_clean_collects_object_after_grace_expired.
    """
    # Build a runner with zero-second grace so we don't need to wait.
    zero_grace_runner = OcxRunner(
        ocx.binary,
        ocx.ocx_home,
        ocx.registry,
        extra_env={"OCX_GC_GRACE_SECONDS": "0"},
    )

    pkg = make_package(zero_grace_runner, unique_repo, "1.0.0", tmp_path, new=True)
    result = zero_grace_runner.json("package", "install", pkg.short)
    content = Path(result[pkg.short]["path"]).resolve()

    assert_dir_exists(content)

    zero_grace_runner.plain("package", "uninstall", pkg.short)

    # Grace = 0 means all unreferenced objects are immediately collectible.
    zero_grace_runner.plain("clean")

    assert_not_exists(
        content,
        "unreferenced object survived clean even with OCX_GC_GRACE_SECONDS=0 — "
        "grace predicate not correctly disabling with 0",
    )
