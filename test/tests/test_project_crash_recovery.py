# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for project-tier mutation crash-recovery.

Cluster A (mutation transactionality) — Codex H2 fix:
half-committed `ocx add` state must be recoverable without manual cleanup.

Spec sources:
- ``crates/ocx_project/src/mutation.rs`` — ``MutationGuard::commit``
  ordering contract: lock first, manifest second, with rollback on
  manifest failure.
- plan_review_fixes_project_toolchain.md Phase 1 step 4 (atomic two-file
  write contract).

These tests are SPECIFICATION mode. The implementation must add
fault-injection env-var hooks (``OCX_TEST_FAULT``) so the kill / fail
points can be exercised deterministically; until those exist, the
relevant tests are ``@pytest.mark.xfail``.

Implementation hooks needed (flagged for the Implement phase):
- ``OCX_TEST_FAULT=after_lock_write`` — abort after ``ocx.lock`` is
  renamed but before ``ocx.toml`` is rewritten.
- ``OCX_TEST_FAULT=before_lock_rename`` — abort after the lock tempfile
  is staged but before the rename.
- ``OCX_TEST_FAULT=registry_unavailable`` — short-circuit the resolver
  to return ``Unavailable`` without contacting the registry.
"""
from __future__ import annotations

import os
import signal
import subprocess
import time
import tomllib
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
EXIT_DATA = 65
EXIT_UNAVAILABLE = 69


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_in(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    cmd = [str(ocx.binary), *args]
    env = dict(ocx.env)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=env, check=False)


def _spawn_in(
    ocx: OcxRunner,
    cwd: Path,
    *args: str,
    extra_env: dict[str, str] | None = None,
) -> subprocess.Popen[str]:
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
# 1. SIGKILL mid-add — next mutator recovers
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    os.name == "nt",
    reason="SIGKILL not available on Windows; equivalent path is TerminateProcess",
)
def test_kill_mid_add_leaves_recoverable_state(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """SIGKILL during ``ocx add`` between lock-write and manifest-write must
    leave both files in a state recoverable with a single ``ocx lock``.

    Recovery contract (whole-file model, spec §3 / flow #5): the SIGKILL
    leaves ``ocx.lock`` carrying a declaration_hash for a config the
    (unchanged) ``ocx.toml`` does not match — exactly the drift shape. The
    next mutator (``ocx add``) refuses to carry forward against the stale lock
    and fails closed with exit 65, naming ``ocx lock`` as the remedy. A single
    ``ocx lock`` reconciles the whole file, after which ``ocx add`` proceeds —
    no manual file removal required.
    """
    short = uuid4().hex[:8]
    repo_a = f"t_{short}_kill_a"
    repo_b = f"t_{short}_kill_b"
    make_package(ocx, repo_a, "1.0.0", tmp_path, cascade=False)
    make_package(ocx, repo_b, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    fq_a = f"{ocx.registry}/{repo_a}:1.0.0"
    fq_b = f"{ocx.registry}/{repo_b}:1.0.0"

    # Spawn `ocx add` with a fault hook that pauses between lock-write
    # and manifest-write. Kill it mid-flight.
    proc = _spawn_in(
        ocx,
        project,
        "add",
        fq_a,
        extra_env={"OCX_TEST_FAULT": "pause_before_manifest_write"},
    )
    # Give the process time to write ocx.lock and reach the pause.
    time.sleep(2.0)
    proc.send_signal(signal.SIGKILL)
    proc.wait(timeout=10)

    # The next mutator fails closed against the orphaned (stale) lock and
    # names `ocx lock` as the remedy — never a silent self-heal re-resolve.
    stale = _run_in(ocx, project, "add", fq_b)
    assert stale.returncode == EXIT_DATA, (
        f"recovery `ocx add` against an orphaned lock must fail closed (exit "
        f"{EXIT_DATA}); rc={stale.returncode}, stderr={stale.stderr!r}"
    )
    assert "ocx lock" in (stale.stderr + stale.stdout).lower(), (
        f"the fail-closed error must direct the user to `ocx lock`; stderr={stale.stderr!r}"
    )

    # A single `ocx lock` reconciles the whole file — no manual cleanup.
    relock = _run_in(ocx, project, "lock")
    assert relock.returncode == EXIT_SUCCESS, (
        f"`ocx lock` must reconcile the orphaned lock; rc={relock.returncode}, "
        f"stderr={relock.stderr!r}"
    )

    # Now the recovery add proceeds.
    result = _run_in(ocx, project, "add", fq_b)
    assert result.returncode == EXIT_SUCCESS, (
        f"recovery `ocx add` must succeed after `ocx lock`; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    toml_text = (project / "ocx.toml").read_text()
    assert repo_b in toml_text, (
        f"recovery add must land its binding in ocx.toml; got:\n{toml_text}"
    )


# ---------------------------------------------------------------------------
# 1b. SIGKILL mid-add leaves ocx.toml whole
# ---------------------------------------------------------------------------


@pytest.mark.skipif(
    os.name == "nt",
    reason="SIGKILL not available on Windows; equivalent path is TerminateProcess",
)
def test_kill_mid_add_leaves_ocx_toml_intact(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A SIGKILL during ``ocx add`` leaves ``ocx.toml`` byte-identical and
    parseable — never truncated, never half-written.

    ``ocx.toml`` is published by atomic rename, so the manifest a killed
    process leaves behind is whole: either the document that was there before
    or the replacement, and here the kill lands before the publish, so it is
    the former. The comments, the schema directive and the existing binding
    all survive, and no temp file is left in the project.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_kill_intact"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    body = (
        "#:schema https://ocx.sh/schemas/project/v1.json\n"
        "# hand-written, must survive a kill\n"
        "\n"
        "[tools]\n"
    )
    toml_path = _write_ocx_toml(project, body)
    before = toml_path.read_bytes()

    proc = _spawn_in(
        ocx,
        project,
        "add",
        f"{ocx.registry}/{repo}:1.0.0",
        extra_env={"OCX_TEST_FAULT": "pause_before_manifest_write"},
    )
    time.sleep(2.0)
    proc.send_signal(signal.SIGKILL)
    proc.wait(timeout=10)

    after = toml_path.read_bytes()
    assert after == before, (
        f"a killed mutation must leave ocx.toml byte-identical; before={before!r}, after={after!r}"
    )
    tomllib.loads(after.decode())

    strays = sorted(
        path.name
        for path in project.iterdir()
        if path.is_file() and path.name not in {"ocx.toml", "ocx.lock"}
    )
    assert not strays, f"a killed mutation must leave no staged temp file behind; found {strays}"


# ---------------------------------------------------------------------------
# 2. Fault-injected partial write — rollback restores predecessor lock
# ---------------------------------------------------------------------------


def test_partial_lock_write_rolled_back(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A simulated failure between ``ocx.lock`` write and ``ocx.toml`` write
    must roll back the lock to the predecessor (or remove it if there
    was none).

    After the rollback, ``ocx add`` must produce a consistent
    (manifest, lock) pair on retry — no corruption, no manual cleanup.
    """
    short = uuid4().hex[:8]
    repo = f"t_{short}_partial_write"
    make_package(ocx, repo, "1.0.0", tmp_path, cascade=False)

    project = tmp_path / "proj"
    project.mkdir()
    _write_ocx_toml(project, "[tools]\n")

    fq = f"{ocx.registry}/{repo}:1.0.0"

    # First attempt: fault-injected after lock write, before manifest write.
    failed = _run_in(
        ocx,
        project,
        "add",
        fq,
        extra_env={"OCX_TEST_FAULT": "after_lock_write"},
    )
    assert failed.returncode != EXIT_SUCCESS, (
        f"fault-injected add must fail; got rc={failed.returncode}, "
        f"stderr={failed.stderr!r}"
    )

    # ocx.toml must be unchanged (no orphaned binding).
    toml_text_after_fault = (project / "ocx.toml").read_text()
    assert repo not in toml_text_after_fault, (
        f"manifest must not contain orphaned binding after rollback; "
        f"got:\n{toml_text_after_fault}"
    )

    # Retry without the fault hook: must succeed and produce a consistent pair.
    result = _run_in(ocx, project, "add", fq)
    assert result.returncode == EXIT_SUCCESS, (
        f"retry add must succeed after rollback; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )
    final_toml = (project / "ocx.toml").read_text()
    assert repo in final_toml, (
        f"retry must land the binding in ocx.toml; got:\n{final_toml}"
    )
    assert (project / "ocx.lock").exists(), "ocx.lock must exist after successful retry"


# ---------------------------------------------------------------------------
# 3. Resolver failure rolls back manifest stage
# ---------------------------------------------------------------------------


def test_resolver_failure_rolls_back_manifest(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """When the resolver fails on the new binding's tag-resolve, ``ocx add``
    must NOT mutate ``ocx.toml`` or ``ocx.lock``.

    Drive the failure by pointing ``ocx add`` at an identifier whose tag
    cannot be resolved against the test registry (registry returns a
    ``ManifestNotFound`` for the unknown tag → ``NotFound`` 79; or a
    transient registry error → ``Unavailable`` 69).

    The exact exit code depends on the failure mode the test registry
    surfaces; the contract is that *some* non-zero exit is observed and
    that ``ocx.toml`` / ``ocx.lock`` are byte-identical to their
    pre-attempt state.
    """
    project = tmp_path / "proj"
    project.mkdir()
    original_toml = "[tools]\n"
    _write_ocx_toml(project, original_toml)
    original_bytes = (project / "ocx.toml").read_bytes()
    lock_existed_before = (project / "ocx.lock").exists()

    # Identifier that cannot resolve: the repo was never published in the
    # test registry, so the manifest endpoint returns 404.
    nonexistent = f"{ocx.registry}/never_published_{uuid4().hex[:8]}:1.0.0"

    result = _run_in(ocx, project, "add", nonexistent)
    assert result.returncode != EXIT_SUCCESS, (
        f"add must fail when the new binding cannot resolve; "
        f"rc={result.returncode}, stderr={result.stderr!r}"
    )

    # ocx.toml must be byte-identical to pre-attempt state.
    after_bytes = (project / "ocx.toml").read_bytes()
    assert after_bytes == original_bytes, (
        f"ocx.toml must be byte-identical after resolver-failure rollback; "
        f"before={original_bytes!r}, after={after_bytes!r}"
    )
    # ocx.lock must not have been created if it didn't exist before.
    if not lock_existed_before:
        assert not (project / "ocx.lock").exists(), (
            "ocx.lock must not be created when the resolver fails on a fresh project"
        )
