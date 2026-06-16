# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the GC audit log (P3 specification).

These tests verify the audit-log contract described in
system_design_shared_store.md §5 M4 point 5:

- clean() appends JSONL records to $OCX_STATE_DIR/gc-log.jsonl.
- Each deleted object emits a record with action="Deleted", digest, instance_id, run_id.
- dry-run logs action="WouldDelete" without deleting the object.
- OCX_GC_LOG=off disables the log entirely (no file created, no error).

All tests are SPECIFICATION tests (contract-first TDD, P3.2s).
They MUST FAIL against the current stubs (RED state) — that is the goal of
this phase.

Spec sources
------------
- system_design_shared_store.md §5 M4.5 — audit log schema + dry-run behaviour
- plan_shared_store.md P3.2s — audit log acceptance tests
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from src.assertions import assert_dir_exists, assert_not_exists
from src.helpers import make_package
from src.runner import OcxRunner


def _audit_log_path(ocx: OcxRunner) -> Path:
    """Return the expected audit log path for this runner."""
    state_dir = Path(ocx.env.get("OCX_STATE_DIR", ocx.env["OCX_HOME"]))
    return state_dir / "gc-log.jsonl"


def test_clean_writes_audit_log(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """clean() appends a Deleted record with digest, instance_id, run_id.

    Contract: system_design_shared_store.md §5 M4.5 — "Append-only JSONL
    $OCX_STATE_DIR/gc-log.jsonl; … one-generation rotation at
    OCX_GC_LOG_MAX_BYTES (10 MiB); best-effort (WARN, never fatal)".

    Schema contract: each record is a JSON object on one line containing at
    minimum action="Deleted", a non-empty digest, a non-empty instance_id,
    and a non-empty run_id.

    Traced to: plan_shared_store P3.2s test_clean_writes_audit_log.
    """
    # Use zero-second grace so the object is collectible immediately.
    zero_grace_runner = OcxRunner(
        ocx.binary,
        ocx.ocx_home,
        ocx.registry,
        extra_env={"OCX_GC_GRACE_SECONDS": "0"},
    )

    pkg = make_package(zero_grace_runner, unique_repo, "1.0.0", tmp_path, new=True)
    result = zero_grace_runner.json("package", "install", pkg.short)
    content = Path(result[pkg.short]["path"]).resolve()

    zero_grace_runner.plain("package", "uninstall", pkg.short)

    log_path = _audit_log_path(zero_grace_runner)
    assert not log_path.exists() or log_path.stat().st_size == 0, (
        "audit log should be absent or empty before clean"
    )

    zero_grace_runner.plain("clean")

    # Object should now be deleted.
    assert_not_exists(content)

    assert log_path.exists(), (
        "audit log not created after clean — GC audit log not implemented"
    )

    lines = [l for l in log_path.read_text().splitlines() if l.strip()]
    assert lines, "audit log is empty after clean collected an object"

    records = [json.loads(line) for line in lines]
    deleted = [r for r in records if r.get("action") == "Deleted"]
    assert deleted, (
        f"no 'Deleted' record in audit log; records found: {records!r} — "
        "GC audit log schema not implemented"
    )

    record = deleted[0]
    assert record.get("digest"), (
        f"Deleted record missing non-empty digest: {record!r}"
    )
    assert record.get("instance_id"), (
        f"Deleted record missing non-empty instance_id: {record!r}"
    )
    assert record.get("run_id"), (
        f"Deleted record missing non-empty run_id: {record!r}"
    )


def test_clean_temp_sweep_writes_audit_log_temp_entry(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """clean() emits an audit-log record with object_kind="Temp" and digest=null
    for each stale temp directory it removes.

    Contract: system_design_shared_store.md §5 M4.5 — "AuditLog records each
    deleted object; temp sweep entries carry object_kind='Temp', digest=null".

    Strategy: plant a fake stale temp directory directly in the temp store
    (OCX_HOME/temp/) with an mtime far in the past so it is older than any
    grace window, then run ``ocx clean`` with zero grace seconds. The sweep
    must remove the temp dir and emit a record with action="Deleted",
    object_kind="Temp", digest=null (JSON null).

    Traced to: plan_shared_store P3.6 FIX 6 — AuditLog threaded into
    sweep_temp_store.
    """
    import os
    import time

    zero_grace_runner = OcxRunner(
        ocx.binary,
        ocx.ocx_home,
        ocx.registry,
        extra_env={"OCX_GC_GRACE_SECONDS": "0"},
    )

    # Plant a stale temp directory inside OCX_HOME/temp/ so clean's sweep picks
    # it up. The name must match the temp-dir pattern expected by sweep
    # (any directory directly under temp/ is eligible).
    temp_store = Path(zero_grace_runner.ocx_home) / "temp"
    temp_store.mkdir(parents=True, exist_ok=True)
    stale_dir = temp_store / f"stale-{unique_repo}"
    stale_dir.mkdir()
    (stale_dir / "content").write_bytes(b"stale content")

    # Push mtime far into the past (beyond any grace window) so zero-grace
    # clean immediately considers it stale.
    past = time.time() - 7200  # 2 hours ago
    os.utime(stale_dir, (past, past))

    log_path = _audit_log_path(zero_grace_runner)

    zero_grace_runner.plain("clean")

    # Stale temp dir must be gone after clean.
    assert not stale_dir.exists(), (
        f"stale temp dir '{stale_dir}' not removed by clean — "
        "temp sweep not implemented or grace not applied"
    )

    assert log_path.exists(), (
        "audit log not created after clean removed a stale temp dir — "
        "AuditLog not threaded into sweep_temp_store"
    )

    lines = [line for line in log_path.read_text().splitlines() if line.strip()]
    records = [json.loads(line) for line in lines]
    temp_records = [
        r
        for r in records
        if r.get("object_kind") == "Temp"
    ]
    assert temp_records, (
        f"no audit-log record with object_kind='Temp' found after stale temp sweep; "
        f"records: {records!r} — "
        "AuditLog.record_delete not called for temp objects (FIX 6)"
    )

    temp_record = temp_records[0]
    assert temp_record.get("action") in ("Deleted", "WouldDelete"), (
        f"Temp audit record has unexpected action: {temp_record!r}"
    )
    assert "digest" in temp_record and temp_record["digest"] is None, (
        f"Temp audit record must carry digest=null (JSON null), got: {temp_record!r}"
    )


def test_clean_dry_run_logs_intent_without_deleting(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """clean --dry-run emits WouldDelete records without actually deleting.

    Contract: system_design_shared_store.md §5 M4.5 — "dry-run logs
    WouldDelete".

    The object must still be present after dry-run, and the audit log must
    contain a WouldDelete record (not Deleted).

    Traced to: plan_shared_store P3.2s test_clean_dry_run_logs_intent_without_deleting.
    """
    zero_grace_runner = OcxRunner(
        ocx.binary,
        ocx.ocx_home,
        ocx.registry,
        extra_env={"OCX_GC_GRACE_SECONDS": "0"},
    )

    pkg = make_package(zero_grace_runner, unique_repo, "1.0.0", tmp_path, new=True)
    result = zero_grace_runner.json("package", "install", pkg.short)
    content = Path(result[pkg.short]["path"]).resolve()

    zero_grace_runner.plain("package", "uninstall", pkg.short)

    # dry-run must not delete the object.
    zero_grace_runner.plain("clean", "--dry-run")

    assert_dir_exists(
        content,
        "dry-run clean deleted the object — --dry-run flag not honored",
    )

    log_path = _audit_log_path(zero_grace_runner)
    assert log_path.exists(), (
        "audit log not created after dry-run clean — GC audit log not implemented"
    )

    lines = [l for l in log_path.read_text().splitlines() if l.strip()]
    records = [json.loads(line) for line in lines]
    would_delete = [r for r in records if r.get("action") == "WouldDelete"]
    assert would_delete, (
        f"no 'WouldDelete' record in audit log after dry-run; "
        f"records found: {records!r} — "
        "dry-run audit log record not implemented"
    )


def test_clean_audit_log_disabled_when_off(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """OCX_GC_LOG=off suppresses the audit log entirely.

    Contract: system_design_shared_store.md §5 M4.5 — "OCX_GC_LOG=off".

    With OCX_GC_LOG=off no gc-log.jsonl should be created.  clean must still
    succeed (log suppression is never fatal).

    Traced to: plan_shared_store P3.2s test_clean_audit_log_disabled_when_off.
    """
    off_runner = OcxRunner(
        ocx.binary,
        ocx.ocx_home,
        ocx.registry,
        extra_env={"OCX_GC_GRACE_SECONDS": "0", "OCX_GC_LOG": "off"},
    )

    pkg = make_package(off_runner, unique_repo, "1.0.0", tmp_path, new=True)
    off_runner.json("package", "install", pkg.short)
    off_runner.plain("package", "uninstall", pkg.short)

    off_runner.plain("clean")  # must not error

    log_path = _audit_log_path(off_runner)
    assert not log_path.exists(), (
        f"audit log created even though OCX_GC_LOG=off — "
        "log-disable flag not implemented"
    )
