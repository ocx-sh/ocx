# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for network-FS posture during GC (P3 specification).

These tests verify the network-FS posture contract described in
system_design_shared_store.md §5 M4 point 6:

- OCX_NETWORK_FS=refuse → Error::NetworkFsRefused → ExitCode::PolicyBlocked (81).
- OCX_NETWORK_FS=warn (default) → proceed with a warning, no error.
- The __OCX_TESTING_FORCE_FS_KIND testing seam triggers the posture check
  without requiring a real network filesystem.

All tests are SPECIFICATION tests (contract-first TDD, P3.2s).
They MUST FAIL against the current stubs (RED state) — that is the goal of
this phase.

Spec sources
------------
- system_design_shared_store.md §5 M4.6 — "OCX_NETWORK_FS ∈ {warn, refuse,
  allow}; refuse → Error::NetworkFsRefused → ExitCode::PolicyBlocked (81)"
- plan_shared_store.md P3.2s — network-FS acceptance tests
"""
from __future__ import annotations

from pathlib import Path

from src.runner import OcxRunner

# PolicyBlocked exit code (reused for network-FS refuse per design record).
_EXIT_POLICY_BLOCKED = 81


def test_refuse_blocks_clean_on_forced_nfs(ocx: OcxRunner) -> None:
    """OCX_NETWORK_FS=refuse + __OCX_TESTING_FORCE_FS_KIND=nfs → exit 81.

    Contract: system_design_shared_store.md §5 M4.6 — "refuse →
    Error::NetworkFsRefused → ExitCode::PolicyBlocked (81)".

    The __OCX_TESTING_FORCE_FS_KIND=nfs seam forces the fs-kind detection to
    report NFS without a real NFS mount, making this deterministic in CI.

    Traced to: plan_shared_store P3.2s test_refuse_blocks_clean_on_forced_nfs.
    """
    refuse_runner = OcxRunner(
        ocx.binary,
        ocx.ocx_home,
        ocx.registry,
        extra_env={
            "OCX_NETWORK_FS": "refuse",
            "__OCX_TESTING_FORCE_FS_KIND": "nfs",
        },
    )

    result = refuse_runner.run("clean", format=None, check=False)
    assert result.returncode == _EXIT_POLICY_BLOCKED, (
        f"expected PolicyBlocked (81) when OCX_NETWORK_FS=refuse and FS kind is NFS, "
        f"got {result.returncode} — "
        "NetworkFsRefused error + PolicyBlocked exit code not implemented"
    )


def test_warn_proceeds_on_forced_nfs(ocx: OcxRunner) -> None:
    """OCX_NETWORK_FS=warn (default) + forced NFS → clean succeeds (exit 0).

    Contract: system_design_shared_store.md §5 M4.6 — "'warn' default …
    proceed" (contrast with 'refuse').

    With the warn posture, detecting a network filesystem should emit a warning
    but must NOT abort the operation.

    Traced to: plan_shared_store P3.2s test_warn_proceeds_on_forced_nfs.
    """
    warn_runner = OcxRunner(
        ocx.binary,
        ocx.ocx_home,
        ocx.registry,
        extra_env={
            "OCX_NETWORK_FS": "warn",
            "__OCX_TESTING_FORCE_FS_KIND": "nfs",
        },
    )

    # clean on an empty store — must exit 0 even on "NFS".
    result = warn_runner.run("clean", format=None, check=False)
    assert result.returncode == 0, (
        f"expected success (0) when OCX_NETWORK_FS=warn, "
        f"got {result.returncode} — "
        "warn posture must not abort clean; network-FS posture not implemented"
    )
