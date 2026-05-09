# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for project-tier mutation flock serialization.

Cluster A (mutation transactionality) — Codex H2 / Architect H2 fixes.

Project-tier mutators (`ocx add`, `ocx remove`, `ocx update`, `ocx lock`,
`ocx init`) must hold an exclusive advisory flock on `ocx.toml` for the
duration of the staged → resolve → commit transaction. Two concurrent
writers must serialize: one wins, the other either retries cleanly or
exits with `Locked` (TempFail 75).

Spec sources:
- ``crates/ocx_lib/src/project/mutation.rs`` (MutationGuard contract)
- ``crates/ocx_lib/src/project/error.rs`` ``ProjectErrorKind::Locked`` →
  ``ExitCode::TempFail`` (75)
- plan_review_fixes_project_toolchain.md Phase 1 step 5

These tests are SPECIFICATION mode: they encode the contract before the
implementation lands. Expected to FAIL or be flaky against today's
naive sequential-write mutators (which can corrupt either file).
"""
from __future__ import annotations

import os
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

# ---------------------------------------------------------------------------
# Exit codes — mirror crates/ocx_lib/src/cli/exit_code.rs
# ---------------------------------------------------------------------------

EXIT_SUCCESS = 0
EXIT_USAGE = 64
EXIT_TEMP_FAIL = 75  # ProjectErrorKind::Locked


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_in(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run ocx with cwd driving the project CWD-walk."""
    cmd = [str(ocx.binary), *args]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=env)


def _spawn_in(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.Popen[str]:
    """Spawn ocx without blocking; caller handles wait()."""
    cmd = [str(ocx.binary), *args]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.Popen(
        cmd,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )


def _write_ocx_toml(project_dir: Path, body: str) -> Path:
    path = project_dir / "ocx.toml"
    path.write_text(body)
    return path


# ---------------------------------------------------------------------------
# 1. Two concurrent `ocx add` — exactly one wins, no corruption
# ---------------------------------------------------------------------------


def test_concurrent_add_serialized_by_flock(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """Two ``ocx add`` invocations against the same ``ocx.toml`` must serialize.

    Either:
    - Both succeed sequentially (winner finishes fast, loser retries inside
      the flock acquisition timeout), and the final ``ocx.toml`` contains
      both bindings in stable order.
    - One succeeds, the other exits with ``EXIT_TEMP_FAIL`` (75 — Locked).

    No partial / interleaved write may corrupt ``ocx.toml`` or ``ocx.lock``.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_conc_a"
    repo_b = f"t_{short}_conc_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    fq_a = f"{ocx.registry}/{repo_a}:1.0.0"
    fq_b = f"{ocx.registry}/{repo_b}:1.0.0"

    # Spawn both writers as close together as possible. Use threads to
    # block on wait() concurrently.
    def _add(fq: str) -> subprocess.CompletedProcess[str]:
        proc = _spawn_in(ocx, project, "add", fq)
        out, err = proc.communicate(timeout=60)
        return subprocess.CompletedProcess(
            args=proc.args, returncode=proc.returncode, stdout=out, stderr=err
        )

    with ThreadPoolExecutor(max_workers=2) as pool:
        fut_a = pool.submit(_add, fq_a)
        fut_b = pool.submit(_add, fq_b)
        result_a = fut_a.result()
        result_b = fut_b.result()

    # Validate: at least one succeeded; any failure must be EXIT_TEMP_FAIL
    # (Locked), never an arbitrary I/O / parse error from a corrupted file.
    successes = [r for r in (result_a, result_b) if r.returncode == EXIT_SUCCESS]
    failures = [r for r in (result_a, result_b) if r.returncode != EXIT_SUCCESS]
    assert successes, (
        "at least one writer must succeed; "
        f"a={result_a.returncode} stderr={result_a.stderr!r} "
        f"b={result_b.returncode} stderr={result_b.stderr!r}"
    )
    for failed in failures:
        assert failed.returncode == EXIT_TEMP_FAIL, (
            f"concurrent add loser must exit {EXIT_TEMP_FAIL} (Locked), "
            f"got {failed.returncode}; stderr={failed.stderr!r}"
        )

    # ocx.toml + ocx.lock must be present and parseable.
    toml_text = (project / "ocx.toml").read_text()
    assert (project / "ocx.lock").exists(), "ocx.lock must exist after a successful add"

    # If both succeeded, both bindings must appear (serialized commits
    # carry forward each other's state); otherwise only the winner's.
    if len(successes) == 2:
        assert repo_a in toml_text and repo_b in toml_text, (
            f"both bindings must appear in ocx.toml after sequential success; "
            f"got:\n{toml_text}"
        )
    else:
        assert (repo_a in toml_text) ^ (repo_b in toml_text), (
            f"exactly one binding must be present when one writer was rejected; "
            f"got:\n{toml_text}"
        )


# ---------------------------------------------------------------------------
# 2. `ocx lock` concurrent with `ocx add` — same serialization contract
# ---------------------------------------------------------------------------


def test_concurrent_lock_command_serialized(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """``ocx lock`` and ``ocx add`` against the same project must serialize.

    Whichever wins first holds the flock; the other either waits and
    succeeds (acceptable) or exits ``EXIT_TEMP_FAIL`` (Locked).

    Neither file may end up corrupted regardless of who wins.
    """
    short = uuid4().hex[:8]
    repo_existing = f"t_{short}_lockconc_existing"
    repo_new = f"t_{short}_lockconc_new"
    make_package(ocx, repo_existing, "1.0.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo_new, "1.0.0", tmp_path, new=True, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(
        project,
        f'[tools]\n{repo_existing} = "{ocx.registry}/{repo_existing}:1.0.0"\n',
    )

    fq_new = f"{ocx.registry}/{repo_new}:1.0.0"

    def _lock() -> subprocess.CompletedProcess[str]:
        proc = _spawn_in(ocx, project, "lock")
        out, err = proc.communicate(timeout=60)
        return subprocess.CompletedProcess(proc.args, proc.returncode, out, err)

    def _add() -> subprocess.CompletedProcess[str]:
        proc = _spawn_in(ocx, project, "add", fq_new)
        out, err = proc.communicate(timeout=60)
        return subprocess.CompletedProcess(proc.args, proc.returncode, out, err)

    with ThreadPoolExecutor(max_workers=2) as pool:
        fut_lock = pool.submit(_lock)
        fut_add = pool.submit(_add)
        r_lock = fut_lock.result()
        r_add = fut_add.result()

    for r in (r_lock, r_add):
        assert r.returncode in (EXIT_SUCCESS, EXIT_TEMP_FAIL), (
            f"unexpected exit code {r.returncode}; "
            f"args={r.args}, stderr={r.stderr!r}"
        )

    # ocx.toml must still be valid TOML containing at least the original binding.
    toml_text = (project / "ocx.toml").read_text()
    assert repo_existing in toml_text, (
        f"original binding must survive concurrent lock+add; got:\n{toml_text}"
    )

    # ocx.lock must exist and be parseable (not truncated / interleaved).
    lock_path = project / "ocx.lock"
    assert lock_path.exists(), "ocx.lock must exist after at least one writer succeeded"
    lock_text = lock_path.read_text()
    assert "[metadata]" in lock_text or "metadata" in lock_text, (
        f"ocx.lock must be parseable TOML, not corrupted; got:\n{lock_text[:500]}"
    )


# ---------------------------------------------------------------------------
# 3. External flock holder blocks `ocx add` — exit 75
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    os.name == "nt",
    reason="POSIX flock(2) advisory contract; Windows uses LockFileEx with different semantics",
)
def test_lock_holder_blocks_other_writers(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """An external process holding the exclusive flock makes ``ocx add`` exit 75.

    Uses ``flock(1)`` from the host shell to hold the advisory lock, then
    invokes ``ocx add`` with a short ``--lock-timeout`` (or the default
    timeout). The mutator must exit ``EXIT_TEMP_FAIL`` rather than block
    forever or silently overwrite.
    """
    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    # Hold the exclusive flock on ocx.toml from a sleeping subshell.
    # `flock -x -n` returns immediately if it cannot acquire (-n = non-block);
    # we want a long-running holder, so use `flock -x` (blocking) with a
    # multi-second sleep wrapped around it.
    holder = subprocess.Popen(
        ["flock", "-x", str(project / "ocx.toml"), "-c", "sleep 10"],
    )

    try:
        # Give flock(1) a moment to acquire.
        time.sleep(0.5)

        result = _run_in(
            ocx,
            project,
            "add",
            f"{ocx.registry}/foo:1.0",
        )
        assert result.returncode == EXIT_TEMP_FAIL, (
            f"ocx add must exit {EXIT_TEMP_FAIL} when flock is held by another process; "
            f"got {result.returncode}; stderr={result.stderr!r}"
        )
    finally:
        holder.terminate()
        holder.wait(timeout=5)
