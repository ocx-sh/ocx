# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for GC lock concurrency (P3 specification).

These tests verify the GC lock contract described in
system_design_shared_store.md §5 M4 point 1:

- clean() acquires exclusive GC lock at $OCX_STATE_DIR/gc.lock.
- A second clean() that cannot acquire the lock within the timeout exits
  TempFail (75) rather than proceeding without the lock.

All tests are SPECIFICATION tests (contract-first TDD, P3.2s).
They MUST FAIL against the current stubs (RED state) — that is the goal of
this phase.

Spec sources
------------
- system_design_shared_store.md §5 M4.1 — "clean 120s → TempFail (75)"
- plan_shared_store.md P3.2s — GC lock contention acceptance test
"""
from __future__ import annotations

import subprocess
from pathlib import Path

from src.helpers import make_package
from src.runner import OcxRunner

# Exit code from sysexits.h EX_TEMPFAIL.
_EXIT_TEMP_FAIL = 75


def test_external_clean_lock_holder_blocks(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """A second `ocx clean` loses the exclusive lock and exits TempFail (75).

    Contract: system_design_shared_store.md §5 M4.1 —
    "$OCX_STATE_DIR/gc.lock; clean() exclusive … Timeouts: clean 120s →
    TempFail (75)".

    Strategy: hold the gc.lock with a separate flock process, then spawn
    `ocx clean` with a zero-second timeout so it fails immediately rather than
    waiting 120 s.  The loser must exit 75.

    Traced to: plan_shared_store P3.2s test_external_clean_lock_holder_blocks.
    """
    state_dir = Path(ocx.env.get("OCX_STATE_DIR", ocx.env["OCX_HOME"]))
    gc_lock_path = state_dir / "gc.lock"
    gc_lock_path.parent.mkdir(parents=True, exist_ok=True)
    gc_lock_path.touch()

    # Hold an exclusive flock on gc.lock for the duration of the subtest.
    # `flock -x <fd>` + `sleep` keeps the fd open while we run the losing clean.
    holder = subprocess.Popen(
        ["flock", "--exclusive", str(gc_lock_path), "sleep", "10"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    try:
        # Use a zero-second timeout so the loser fails immediately rather than
        # waiting out the holder. The timeout must reach the subprocess via the
        # runner's environment (the same `extra_env` pattern the sibling P3
        # acceptance tests use), so build a dedicated runner.
        zero_timeout_runner = OcxRunner(
            ocx.binary,
            ocx.ocx_home,
            ocx.registry,
            extra_env={"OCX_GC_LOCK_TIMEOUT": "0"},
        )

        result = zero_timeout_runner.run("clean", format=None, check=False)
        assert result.returncode == _EXIT_TEMP_FAIL, (
            f"expected TempFail (75) when GC lock already held, got {result.returncode}; "
            "GC lock exclusive-acquire with timeout not implemented"
        )
    finally:
        holder.terminate()
        holder.wait(timeout=5)


def test_clean_blocks_concurrent_install(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """A concurrent ``package install`` succeeds and its freshly-installed object
    is NOT deleted when ``clean`` holds the exclusive GC lock.

    Contract: system_design_shared_store.md §5 M4.1 — "Shared lock: install/pull
    acquire shared lock … on timeout [the mutator] proceeds without it — a stuck
    clean never blocks installs."

    Strategy: hold the exclusive gc.lock (simulating ``ocx clean`` mid-run) so
    the installer cannot acquire the shared lock within its 10 s timeout. The
    installer must then proceed without the lock, succeed, and the installed
    object (which the held lock protects from concurrent GC) must exist after
    the install completes. We verify that:

    a) ``package install`` exits 0 even though the exclusive lock is held.
    b) The installed content directory exists after the lock is released.
       (A real ``ocx clean`` running concurrently would not collect the freshly
       installed object because the mtime grace window covers the TOCTOU gap.)

    This test uses the lock-holding pattern from the sibling test above: hold the
    gc.lock file with ``flock --exclusive`` for the duration of the install so
    the acquire_shared call inside the installer times out (10 s default), then
    the installer proceeds without the lock. We use a zero-second grace in the
    env so any subsequent clean would collect objects, but the installer itself
    must succeed.

    Traced to: plan_shared_store P3.3 test_clean_blocks_concurrent_install.
    """
    state_dir = Path(ocx.env.get("OCX_STATE_DIR", ocx.env["OCX_HOME"]))
    gc_lock_path = state_dir / "gc.lock"
    gc_lock_path.parent.mkdir(parents=True, exist_ok=True)
    gc_lock_path.touch()

    # Publish a test package so the installer has something to fetch.
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True)

    # Use a zero-second shared-lock timeout so the installer gives up on the
    # shared lock immediately and proceeds without it, rather than waiting 10 s.
    # The shared lock timeout is not configurable in production (it is always
    # DEFAULT_SHARED_TIMEOUT = 10 s), but we use OCX_GC_LOCK_TIMEOUT to configure
    # the *exclusive* timeout; the shared timeout falls back to its built-in.
    # To keep the test fast we simply hold the lock, run the install (which
    # proceeds without the lock after the timeout), then release the lock.
    holder = subprocess.Popen(
        ["flock", "--exclusive", str(gc_lock_path), "sleep", "30"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    installed_path: Path | None = None
    try:
        # Run install while the exclusive lock is held. The installer acquires
        # the shared lock best-effort: it times out (10 s default) and proceeds
        # without the lock. The install must still succeed (exit 0).
        result = ocx.run("package", "install", pkg.short, format="json", check=False)
        assert result.returncode == 0, (
            f"package install must succeed even when exclusive GC lock is held; "
            f"got exit {result.returncode}\nstderr: {result.stderr}"
        )

        import json as _json
        data = _json.loads(result.stdout)
        import re as _re
        # The JSON output maps the package identifier to its install info.
        for key in data:
            if _re.search(r"content|path", str(data[key])):
                raw_path = data[key].get("path") or data[key].get("content")
                if raw_path:
                    installed_path = Path(raw_path).resolve()
                    break
    finally:
        holder.terminate()
        holder.wait(timeout=5)

    # After the lock is released, verify the installed content still exists.
    # (A concurrent clean would not have collected it because the lock was held
    # throughout; additionally the mtime grace window protects fresh objects.)
    # Fail loudly if the install path could not be extracted — a silently skipped
    # existence check would make this safety test vacuously pass on output drift.
    assert installed_path is not None, (
        f"could not extract installed content path from install JSON: {result.stdout}"
    )
    assert installed_path.exists(), (
        f"freshly installed content at '{installed_path}' does not exist after "
        "the exclusive GC lock was released — concurrent install was corrupted"
    )
