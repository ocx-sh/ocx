# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests — `ocx launcher exec` internal subcommand.

These tests exercise the hidden `ocx launcher exec` subcommand that generated
entry-point launchers call at runtime.  The stable wire ABI is:

    ocx launcher exec '<pkg-root>' -- <argv0> [args...]

Scenarios covered:
- Success: valid installed package root, entrypoint binary resolves and runs.
- Missing pkg-root: non-existent path → exit 64 (UsageError).
- pkg-root outside $OCX_HOME/packages/: path escapes packages root → exit 64.
- Missing metadata.json: path inside packages/ but no metadata.json → exit 64.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

from src.helpers import make_package_with_entrypoints
from src.runner import OcxRunner


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _get_package_root(ocx: OcxRunner, pkg_short: str) -> Path:
    """Return the on-disk package root for an installed package."""
    result = ocx.json("find", pkg_short)
    path = result.get(pkg_short) if isinstance(result, dict) else None
    assert path is not None, f"ocx find must report a path for {pkg_short!r}"
    return Path(path)


# ---------------------------------------------------------------------------
# Success path
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher exec test")
def test_launcher_exec_success(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx launcher exec <pkg_root> -- hello` resolves and runs the entrypoint binary."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", pkg.short)

    pkg_root = _get_package_root(ocx, pkg.short)

    result = ocx.run("launcher", "exec", str(pkg_root), "--", "hello", format=None, check=False)
    assert result.returncode == 0, (
        f"launcher exec with valid pkg-root must succeed; "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert pkg.marker in result.stdout, (
        f"launcher exec must run the resolved binary — marker missing; "
        f"stdout={result.stdout!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason="Unix launcher exec test")
def test_launcher_exec_forwards_args(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """`ocx launcher exec <pkg_root> -- hello extra` forwards extra args to the binary."""
    pkg = make_package_with_entrypoints(
        ocx,
        unique_repo,
        tmp_path,
        entrypoints=[{"name": "hello", "target": "${installPath}/bin/hello"}],
        bins=["hello"],
    )
    ocx.plain("install", pkg.short)

    pkg_root = _get_package_root(ocx, pkg.short)
    extra_arg = "extra-from-launcher-exec"

    result = ocx.run("launcher", "exec", str(pkg_root), "--", "hello", extra_arg, format=None, check=False)
    assert result.returncode == 0, (
        f"launcher exec with extra args must succeed; "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert extra_arg in result.stdout, (
        f"launcher exec must forward extra args verbatim; stdout={result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Error paths
# ---------------------------------------------------------------------------


def test_launcher_exec_missing_pkg_root_exits_64(ocx: OcxRunner) -> None:
    """`ocx launcher exec /nonexistent/path -- hello` must exit 64 (UsageError)."""
    nonexistent = "/nonexistent/pkg-root-does-not-exist"
    result = ocx.run("launcher", "exec", nonexistent, "--", "hello", format=None, check=False)
    assert result.returncode == 64, (
        f"non-existent pkg-root must exit 64 (UsageError); "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )


def test_launcher_exec_relative_path_exits_64(ocx: OcxRunner) -> None:
    """`ocx launcher exec relative/path -- hello` must exit 64 (non-absolute)."""
    result = ocx.run("launcher", "exec", "relative/path", "--", "hello", format=None, check=False)
    assert result.returncode == 64, (
        f"relative pkg-root must exit 64 (UsageError); "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert "absolute" in result.stderr.lower(), (
        f"error must mention 'absolute'; stderr={result.stderr.strip()!r}"
    )


def test_launcher_exec_outside_packages_root_exits_64(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """`ocx launcher exec` with a path outside $OCX_HOME/packages/ must exit 64."""
    outside = tmp_path / "outside-packages-root"
    outside.mkdir()

    result = ocx.run("launcher", "exec", str(outside), "--", "hello", format=None, check=False)
    assert result.returncode == 64, (
        f"pkg-root outside OCX_HOME/packages/ must exit 64 (UsageError); "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )


def test_launcher_exec_missing_metadata_json_exits_64(ocx: OcxRunner) -> None:
    """`ocx launcher exec` on a path inside packages/ with no metadata.json must exit 64."""
    # Construct a fake path inside OCX_HOME/packages/; the directory exists
    # but contains no metadata.json — validate_launcher_pkg_root rejects it.
    fake = Path(str(ocx.ocx_home)) / "packages" / "fake.example.com" / "sha256" / "ab" / "fake"
    fake.mkdir(parents=True, exist_ok=True)

    result = ocx.run("launcher", "exec", str(fake), "--", "hello", format=None, check=False)
    assert result.returncode == 64, (
        f"pkg-root without metadata.json must exit 64 (UsageError); "
        f"rc={result.returncode}, stderr={result.stderr.strip()!r}"
    )
    assert "metadata.json" in result.stderr, (
        f"error must mention 'metadata.json'; stderr={result.stderr.strip()!r}"
    )
