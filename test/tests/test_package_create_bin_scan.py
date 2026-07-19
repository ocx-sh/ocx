# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package create`'s interface-binaries auto-scan.

Encodes `adr_declared_binaries_metadata.md` §2 (tri-state mode table) + §2.1
(output-sidecar persistence) + the UX Scenarios / Error Taxonomy tables.
`create` is the compiler step (mirrors `test_package_create_pinning.py` for
dependency pins): a `-m` input's `binaries` field is filled or verified
against the on-disk content tree and the *output* sidecar (never `-m`) is
rewritten canonically.

Scan uses a Unix exec-bit convention vs. a Windows extension allowlist
(ADR §2 step 3) — this module drives raw exec-bit fixtures throughout, so it
is skipped on Windows; the extension-allowlist convention is covered by
`bin_scan.rs`'s Rust unit tests instead.
"""

from __future__ import annotations

import json
import os
import stat
import sys
from pathlib import Path

import pytest

from src.helpers import make_package, resolved_metadata_path
from src.runner import OcxRunner, current_platform

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="scan drives raw Unix exec-bit fixtures; the Windows extension-allowlist "
    "convention is covered by bin_scan.rs unit tests",
)

EXIT_SUCCESS = 0
EXIT_USAGE_ERR = 64  # UsageError — validate_bin_scan: --bin-scan given without --metadata
EXIT_DATA_ERR = 65  # DataError — BinScanError::{UndeclaredBinary,DeclaredNotExecutable}, BinaryError parse failures
EXIT_IO_ERR = 74  # IoError — crate::Error::InternalFile (BinScanError::Scan) on an unreadable scan-target dir


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_pkg(tmp_path: Path, name: str, bin_files: dict[str, bool]) -> Path:
    """Write a content dir with `bin/<filename>` for each entry.

    ``bin_files`` maps filename to whether that file gets the executable bit
    — the exec-bit knob the scan algorithm keys off of.
    """
    pkg_dir = tmp_path / f"pkg-{name}"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)
    for filename, executable in bin_files.items():
        f = bin_dir / filename
        f.write_text("#!/bin/sh\necho hi\n")
        if executable:
            f.chmod(f.stat().st_mode | stat.S_IEXEC)
    return pkg_dir


def _write_metadata(
    tmp_path: Path,
    name: str,
    *,
    binaries: list[str] | None = None,
    path_visibility: str | None = "public",
) -> Path:
    """Write a `-m` input sidecar with a single PATH var + optional `binaries`.

    ``binaries=None`` (default) omits the field entirely (undeclared);
    any list — including ``[]`` — declares it explicitly. The two are
    distinct wire states (ADR §1). ``path_visibility=None`` omits the
    `visibility` key so the var defaults to `private` (excluded from the
    interface-surface scan scope, per ADR §2 step 1).
    """
    path_var: dict = {
        "key": "PATH",
        "type": "path",
        "required": True,
        "value": "${installPath}/bin",
    }
    if path_visibility is not None:
        path_var["visibility"] = path_visibility
    metadata_obj: dict = {"type": "bundle", "version": 1, "env": [path_var]}
    if binaries is not None:
        metadata_obj["binaries"] = binaries
    metadata_path = tmp_path / f"metadata-{name}.json"
    metadata_path.write_text(json.dumps(metadata_obj))
    return metadata_path


def _create(
    ocx: OcxRunner,
    pkg_dir: Path,
    metadata: Path,
    out: Path,
    *args: str,
    check: bool = True,
):
    return ocx.plain(
        "package",
        "create",
        "-m",
        str(metadata),
        "-o",
        str(out),
        "-p",
        current_platform(),
        *args,
        str(pkg_dir),
        check=check,
    )


def _sidecar(out: Path) -> dict:
    return json.loads(resolved_metadata_path(out).read_text())


def _make_unreadable_or_skip(target: Path) -> None:
    """Chmod `target` to 0o000 and probe that it actually became unreadable.

    Mirrors the root-probe guard in `bin_scan.rs`'s
    `scan_propagates_permission_denied_reading_target_dir` unit test: root
    (or any process with `CAP_DAC_OVERRIDE`) ignores DAC permission bits, so
    the unreadable condition cannot be constructed there — skip rather than
    false-failing.
    """
    target.chmod(0o000)
    try:
        os.listdir(target)
    except PermissionError:
        return
    target.chmod(0o755)
    pytest.skip("running with elevated privileges that bypass DAC permission bits (e.g. root)")


# ---------------------------------------------------------------------------
# Auto mode (neither flag) — field absent
# ---------------------------------------------------------------------------


def test_auto_absent_fills_sidecar_leaves_input_untouched(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Auto + absent: scan fills the output sidecar; `-m` input never rewritten."""
    pkg_dir = _write_pkg(tmp_path, "auto", {"alpha": True, "beta": True})
    metadata = _write_metadata(tmp_path, "auto")
    original_bytes = metadata.read_bytes()
    out = tmp_path / "bundle-auto.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert metadata.read_bytes() == original_bytes, "create must never rewrite the -m input"
    assert _sidecar(out).get("binaries") == ["alpha", "beta"], _sidecar(out)


def test_auto_absent_excludes_non_interface_path_var_from_scan(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Scan scope is interface-surface only (ADR §2 step 1): a PATH var
    without `visibility.has_interface()` contributes zero candidates even
    though the directory it names holds executables."""
    pkg_dir = _write_pkg(tmp_path, "privatepath", {"hello": True})
    metadata = _write_metadata(tmp_path, "privatepath", path_visibility=None)
    out = tmp_path / "bundle-privatepath.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _sidecar(out).get("binaries") == [], _sidecar(out)


def test_auto_declared_field_present_skips_scan_verbatim(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Auto + a declared field: scan is NOT run at all — a name absent from
    disk (`ghost`) and a real on-disk executable not in the list (`hello`)
    both pass through unaffected, unlike `--bin-scan` Verify."""
    pkg_dir = _write_pkg(tmp_path, "autodecl", {"hello": True})
    metadata = _write_metadata(tmp_path, "autodecl", binaries=["ghost"])
    out = tmp_path / "bundle-autodecl.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _sidecar(out).get("binaries") == ["ghost"], (
        f"Auto mode with a declared field must never scan or verify; got {_sidecar(out)}"
    )


def test_auto_declared_empty_array_stays_empty(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`Some([])` is a present field, distinct from an absent one (ADR §1
    None-vs-Some-empty wire distinction) — Auto passes it through unchanged
    even though the content tree holds a real executable."""
    pkg_dir = _write_pkg(tmp_path, "autoempty", {"hello": True})
    metadata = _write_metadata(tmp_path, "autoempty", binaries=[])
    out = tmp_path / "bundle-autoempty.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _sidecar(out).get("binaries") == [], _sidecar(out)


# ---------------------------------------------------------------------------
# --bin-scan (Verify mode)
# ---------------------------------------------------------------------------


def test_bin_scan_flag_absent_field_behaves_like_auto(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`--bin-scan` + absent field: fills the sidecar exactly like Auto —
    verification needs a declaration to verify against."""
    pkg_dir = _write_pkg(tmp_path, "verifyabsent", {"gamma": True})
    metadata = _write_metadata(tmp_path, "verifyabsent")
    out = tmp_path / "bundle-verifyabsent.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "--bin-scan")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _sidecar(out).get("binaries") == ["gamma"], _sidecar(out)


def test_bin_scan_flag_absent_field_empty_bin_dir_yields_empty_array(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`--bin-scan` + absent field + no executables: fills `binaries: []`,
    not an error and not a missing key."""
    pkg_dir = _write_pkg(tmp_path, "verifyempty", {})
    metadata = _write_metadata(tmp_path, "verifyempty")
    out = tmp_path / "bundle-verifyempty.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "--bin-scan")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _sidecar(out).get("binaries") == [], _sidecar(out)


def test_bin_scan_declared_present_and_executable_passes_through(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`--bin-scan` + declared name present + executable: succeeds, declared
    list passes through verbatim."""
    pkg_dir = _write_pkg(tmp_path, "declok", {"cmake": True})
    metadata = _write_metadata(tmp_path, "declok", binaries=["cmake"])
    out = tmp_path / "bundle-declok.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "--bin-scan")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _sidecar(out).get("binaries") == ["cmake"], _sidecar(out)


def test_bin_scan_declared_not_executable_exits_65(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`--bin-scan` + declared name present but NOT executable →
    `BinScanError::DeclaredNotExecutable`, exit 65."""
    pkg_dir = _write_pkg(tmp_path, "declnoexec", {"cmake": False})
    metadata = _write_metadata(tmp_path, "declnoexec", binaries=["cmake"])
    out = tmp_path / "bundle-declnoexec.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "--bin-scan", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "cmake" in result.stderr, result.stderr
    assert "not executable" in result.stderr, result.stderr


def test_bin_scan_declared_missing_from_disk_is_legal(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`--bin-scan` + declared name absent from disk: legal (no error) — may
    be platform-conditional or dependency-sourced; declared list survives."""
    pkg_dir = _write_pkg(tmp_path, "declmissing", {})
    metadata = _write_metadata(tmp_path, "declmissing", binaries=["cmake"])
    out = tmp_path / "bundle-declmissing.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "--bin-scan")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _sidecar(out).get("binaries") == ["cmake"], _sidecar(out)


def test_bin_scan_scanned_name_undeclared_exits_65(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`--bin-scan`: a scanned executable absent from the declared list →
    `BinScanError::UndeclaredBinary`, exit 65 (one-directional diff)."""
    pkg_dir = _write_pkg(tmp_path, "undeclared", {"cmake": True, "ninja": True})
    metadata = _write_metadata(tmp_path, "undeclared", binaries=["cmake"])
    out = tmp_path / "bundle-undeclared.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "--bin-scan", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "ninja" in result.stderr, result.stderr
    assert "not declared" in result.stderr, result.stderr


# ---------------------------------------------------------------------------
# --no-bin-scan (Off mode)
# ---------------------------------------------------------------------------


def test_no_bin_scan_absent_field_stays_absent(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`--no-bin-scan` + absent field: no scan runs; field stays undeclared
    (the `binaries` key is entirely absent from the sidecar)."""
    pkg_dir = _write_pkg(tmp_path, "offabsent", {"hello": True})
    metadata = _write_metadata(tmp_path, "offabsent")
    out = tmp_path / "bundle-offabsent.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "--no-bin-scan")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert "binaries" not in _sidecar(out), _sidecar(out)


def test_no_bin_scan_declared_passes_through_verbatim(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`--no-bin-scan` + declared field: passes through untouched even when
    it disagrees with the content tree (a declared name missing on disk, a
    real executable left undeclared) — Off mode never inspects the tree."""
    pkg_dir = _write_pkg(tmp_path, "offdecl", {"hello": True})
    metadata = _write_metadata(tmp_path, "offdecl", binaries=["ghost"])
    out = tmp_path / "bundle-offdecl.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "--no-bin-scan")

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _sidecar(out).get("binaries") == ["ghost"], _sidecar(out)


# ---------------------------------------------------------------------------
# --bin-scan without --metadata: usage error (Cluster 2, arch-Warn)
# ---------------------------------------------------------------------------


def test_bin_scan_without_metadata_exits_usage_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`--bin-scan` given without `-m/--metadata` has nothing to verify —
    `validate_bin_scan` rejects it as a usage error (exit 64) rather than the
    prior silent no-op that exited 0 without scanning anything."""
    pkg_dir = _write_pkg(tmp_path, "noverifymeta", {"hello": True})
    out = tmp_path / "bundle-noverifymeta.tar.xz"

    result = ocx.plain(
        "package",
        "create",
        "--bin-scan",
        "-o",
        str(out),
        "-p",
        current_platform(),
        str(pkg_dir),
        check=False,
    )

    assert result.returncode == EXIT_USAGE_ERR, result.stderr
    assert "--bin-scan" in result.stderr and "--metadata" in result.stderr, result.stderr
    assert not out.exists(), "create must not write an output bundle on a usage error"


# ---------------------------------------------------------------------------
# Unreadable scan-target directory: fail-closed I/O (max-tier review Cluster 1)
# ---------------------------------------------------------------------------


def test_auto_unreadable_bin_dir_fails_closed_not_silent_empty_array(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Auto + absent field + a scan target that cannot be read: the I/O
    failure must propagate (nonzero exit), never collapse to a silently
    baked `binaries: []` — that would be a positive "publisher asserts
    zero" claim despite the tree never actually being read."""
    pkg_dir = _write_pkg(tmp_path, "unreadable-auto", {"hello": True})
    bin_dir = pkg_dir / "bin"
    _make_unreadable_or_skip(bin_dir)
    metadata = _write_metadata(tmp_path, "unreadable-auto")
    out = tmp_path / "bundle-unreadable-auto.tar.xz"

    try:
        result = _create(ocx, pkg_dir, metadata, out, check=False)
    finally:
        bin_dir.chmod(0o755)

    assert result.returncode == EXIT_IO_ERR, result.stderr
    assert not out.exists(), "create must not write an output bundle on scan failure"
    assert not resolved_metadata_path(out).exists(), (
        "create must not write a resolved sidecar on scan failure"
    )


def test_bin_scan_unreadable_bin_dir_fails_closed(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`--bin-scan` + a scan target that cannot be read: the I/O failure
    propagates too — Verify mode must not pass green on a scan that never
    actually read the content tree."""
    pkg_dir = _write_pkg(tmp_path, "unreadable-verify", {"hello": True})
    bin_dir = pkg_dir / "bin"
    _make_unreadable_or_skip(bin_dir)
    metadata = _write_metadata(tmp_path, "unreadable-verify")
    out = tmp_path / "bundle-unreadable-verify.tar.xz"

    try:
        result = _create(ocx, pkg_dir, metadata, out, "--bin-scan", check=False)
    finally:
        bin_dir.chmod(0o755)

    assert result.returncode == EXIT_IO_ERR, result.stderr
    assert not out.exists(), "create must not write an output bundle on scan failure"
    assert not resolved_metadata_path(out).exists(), (
        "create must not write a resolved sidecar on scan failure"
    )


# ---------------------------------------------------------------------------
# Missing (nonexistent) scan-target directory — legal, distinct from unreadable
# ---------------------------------------------------------------------------


def test_auto_absent_field_missing_bin_dir_yields_empty_array(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Auto + absent field + the declared PATH target directory does not
    exist on disk at all (not merely empty): a missing directory is legal
    (ADR §2 step 3 — existence probed before walk) and fills `binaries: []`,
    distinct from the unreadable-directory fail-closed path above."""
    pkg_dir = tmp_path / "pkg-missingdir"
    pkg_dir.mkdir()
    metadata = _write_metadata(tmp_path, "missingdir")
    out = tmp_path / "bundle-missingdir.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out)

    assert result.returncode == EXIT_SUCCESS, result.stderr
    assert _sidecar(out).get("binaries") == [], _sidecar(out)


# ---------------------------------------------------------------------------
# Hand-authored validation (mode-independent — fails at metadata parse)
# ---------------------------------------------------------------------------


def test_case_fold_collision_in_hand_authored_binaries_exits_65(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Two declared names colliding only by case (`Cmake`/`cmake`) reject at
    metadata parse — `BinaryError::CaseFoldCollision`, exit 65 — regardless
    of `--bin-scan` mode (construction-time validation, ADR §5 Decision B)."""
    pkg_dir = _write_pkg(tmp_path, "casefold", {"cmake": True})
    metadata = _write_metadata(tmp_path, "casefold", binaries=["Cmake", "cmake"])
    out = tmp_path / "bundle-casefold.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "collide" in result.stderr.lower(), result.stderr


# ---------------------------------------------------------------------------
# Push -> install -> inspect round trip: the claim reaches the config blob
# ---------------------------------------------------------------------------


def test_binaries_claim_round_trips_through_push_install_inspect(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Auto-mode create-time scan fill survives push: the config blob the
    registry serves for the platform manifest carries the claim."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, bins=["hello", "world"])

    child = ocx.json("package", "inspect", pkg.short)[pkg.short]["candidates"][0]["digest"]
    digest_ref = f"{unique_repo}@{child}"
    data = ocx.json("package", "inspect", digest_ref)[digest_ref]

    assert sorted(data["metadata"].get("binaries", [])) == ["hello", "world"], data["metadata"]


def test_declared_binaries_round_trip_through_push_install_inspect(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A hand-authored claim absent from disk survives create -> push
    untouched (Auto mode never scans/verifies a present field) — the
    published config blob carries it byte-for-byte."""
    pkg = make_package(
        ocx, unique_repo, "1.0.0", tmp_path, bins=["hello"], binaries=["ghost-tool"]
    )

    child = ocx.json("package", "inspect", pkg.short)[pkg.short]["candidates"][0]["digest"]
    digest_ref = f"{unique_repo}@{child}"
    data = ocx.json("package", "inspect", digest_ref)[digest_ref]

    assert data["metadata"].get("binaries") == ["ghost-tool"], data["metadata"]
