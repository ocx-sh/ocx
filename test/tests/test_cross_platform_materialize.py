# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `--platform` on toolchain-tier commands (issue #138).

Toolchain-tier commands (`add`, `lock`, `update`, `pull`, `env`) hardcoded the
host-native platform when warming the object store. These tests prove the new
`--platform` flag materializes a *foreign* platform's leaf instead — the V2 lock
already pins every shipped platform's leaf, so `--platform` only selects which
one to fetch (the lock stays host-agnostic).

The published fixture ships `linux/amd64` + `linux/arm64`. On every CI host
(linux/amd64, linux/arm64, darwin/*) at least one of those is a genuinely
foreign platform, so the assertions never depend on which arch runs them.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

from src.helpers import make_package
from src.runner import OcxRunner


EXIT_SUCCESS = 0
EXIT_USAGE = 64
# NoHostLeaf → ConfigError (78) per error.rs ClassifyExitCode.
EXIT_CONFIG = 78

# The published fixture ships these two; bind to the rolling minor tag, which
# cascade makes a multi-platform image index (see test_cascade.py).
AMD64 = "linux/amd64"
ARM64 = "linux/arm64"
MINOR_TAG = "3.28"
PUSH_VERSION = "3.28.0"


def _run(ocx: OcxRunner, cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=ocx.env,
    )


def _publish_multiplatform(ocx: OcxRunner, repo: str, tmp_path: Path) -> None:
    """Push the same version for amd64 and arm64 → a 2-platform image index."""
    make_package(ocx, repo, PUSH_VERSION, tmp_path / "amd64", platform=AMD64, new=True)
    make_package(ocx, repo, PUSH_VERSION, tmp_path / "arm64", platform=ARM64, new=False)


def _project_with_lock(ocx: OcxRunner, repo: str, tmp_path: Path, *, lock: bool = True) -> Path:
    """Create a project dir binding `repo` to its multi-platform minor tag.

    Uses `lock --no-pull` to write the lock without host-platform
    materialization (the fixture ships no leaf for a darwin host, so an eager
    host pull would fail on macOS runners — irrelevant to what we test here).
    """
    project_dir = tmp_path / "proj"
    project_dir.mkdir(exist_ok=True)
    (project_dir / "ocx.toml").write_text(f'[tools]\n{repo} = "{ocx.registry}/{repo}:{MINOR_TAG}"\n')
    if lock:
        result = _run(ocx, project_dir, "lock", "--no-pull")
        assert result.returncode == EXIT_SUCCESS, f"baseline lock failed: {result.stderr}"
    return project_dir


def _dry_run_status(ocx: OcxRunner, project_dir: Path, platform: str) -> str:
    """Return the single tool's dry-run status (`cached` / `would-fetch`)."""
    result = _run(ocx, project_dir, "--format", "json", "pull", f"--platform={platform}", "--dry-run")
    assert result.returncode == EXIT_SUCCESS, f"dry-run for {platform} failed: {result.stderr}"
    rows = json.loads(result.stdout)
    assert len(rows) == 1, f"expected one tool row, got: {rows}"
    return rows[0]["status"]


def test_pull_platform_materializes_foreign_leaf(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`ocx pull --platform=linux/arm64` warms the arm64 leaf and ONLY that leaf.

    End-to-end proof of cross-platform selection: after pulling arm64, arm64 is
    cached while the sibling amd64 leaf is still would-fetch — the flag drove the
    materialization, not the host.
    """
    _publish_multiplatform(ocx, unique_repo, tmp_path)
    project_dir = _project_with_lock(ocx, unique_repo, tmp_path)

    assert _dry_run_status(ocx, project_dir, ARM64) == "would-fetch", "arm64 must start uncached"

    pull = _run(ocx, project_dir, "pull", f"--platform={ARM64}")
    assert pull.returncode == EXIT_SUCCESS, f"cross-platform pull failed: {pull.stderr}"

    assert _dry_run_status(ocx, project_dir, ARM64) == "cached", "arm64 must be cached after pull"
    assert _dry_run_status(ocx, project_dir, AMD64) == "would-fetch", (
        "amd64 must stay uncached — only the requested arm64 leaf was materialized"
    )


def test_lock_multi_platform_warms_all_requested(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`ocx lock --platform=amd64 --platform=arm64` materializes both leaves."""
    _publish_multiplatform(ocx, unique_repo, tmp_path)
    project_dir = _project_with_lock(ocx, unique_repo, tmp_path)

    result = _run(ocx, project_dir, "lock", f"--platform={AMD64}", f"--platform={ARM64}")
    assert result.returncode == EXIT_SUCCESS, f"multi-platform lock failed: {result.stderr}"

    assert _dry_run_status(ocx, project_dir, AMD64) == "cached", "amd64 leaf must be warmed"
    assert _dry_run_status(ocx, project_dir, ARM64) == "cached", "arm64 leaf must be warmed"


def test_add_platform_materializes_foreign_leaf(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`ocx add <id> --platform=linux/arm64` writes the lock and warms arm64."""
    _publish_multiplatform(ocx, unique_repo, tmp_path)
    project_dir = tmp_path / "proj"
    project_dir.mkdir(exist_ok=True)
    (project_dir / "ocx.toml").write_text("[tools]\n")

    add = _run(ocx, project_dir, "add", f"{ocx.registry}/{unique_repo}:{MINOR_TAG}", f"--platform={ARM64}")
    assert add.returncode == EXIT_SUCCESS, f"add --platform failed: {add.stderr}"

    assert _dry_run_status(ocx, project_dir, ARM64) == "cached", "arm64 leaf must be warmed by add --platform"


def test_update_platform_materializes_foreign_leaf(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`ocx update --platform=linux/arm64` re-resolves a moved tag and warms
    ONLY the requested arm64 leaf of the freshly-resolved digest.

    #138 lists `update` in its `--platform` scope; unlike `add`/`lock` this
    is the whole-file bump verb that re-resolves every declared tag even when
    unchanged — publishing a second version and bumping the bound minor tag
    proves `--platform` drives materialization of the NEW lock, not a stale
    one carried over from setup.
    """
    _publish_multiplatform(ocx, unique_repo, tmp_path)
    project_dir = _project_with_lock(ocx, unique_repo, tmp_path)

    # Bump: publish a second version for both platforms. `cascade` (default
    # True in `make_package`) re-points the bound minor tag (3.28) at the
    # new digest, so `ocx update` has a moved tag to re-resolve.
    make_package(ocx, unique_repo, "3.28.1", tmp_path / "bump_amd64", platform=AMD64, new=False)
    make_package(ocx, unique_repo, "3.28.1", tmp_path / "bump_arm64", platform=ARM64, new=False)

    assert _dry_run_status(ocx, project_dir, ARM64) == "would-fetch", "arm64 must start uncached"

    update = _run(ocx, project_dir, "update", f"--platform={ARM64}")
    assert update.returncode == EXIT_SUCCESS, f"cross-platform update failed: {update.stderr}"

    assert _dry_run_status(ocx, project_dir, ARM64) == "cached", "arm64 must be cached after update"
    assert _dry_run_status(ocx, project_dir, AMD64) == "would-fetch", (
        "amd64 must stay uncached — only the requested arm64 leaf of the "
        "newly-resolved digest was materialized"
    )


def test_pull_platform_not_shipped_exits_78(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Requesting a platform the publisher does not ship fails loud (exit 78)."""
    _publish_multiplatform(ocx, unique_repo, tmp_path)  # linux only
    project_dir = _project_with_lock(ocx, unique_repo, tmp_path)

    result = _run(ocx, project_dir, "pull", "--platform=windows/amd64")
    assert result.returncode == EXIT_CONFIG, (
        f"unshipped --platform must exit {EXIT_CONFIG}; got {result.returncode}: {result.stderr}"
    )


def test_env_platform_single_target(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`ocx env --platform=linux/arm64 --shell=bash` composes the arm64 env."""
    _publish_multiplatform(ocx, unique_repo, tmp_path)
    project_dir = _project_with_lock(ocx, unique_repo, tmp_path)

    result = _run(ocx, project_dir, "env", f"--platform={ARM64}", "--shell=bash")
    assert result.returncode == EXIT_SUCCESS, f"env --platform failed: {result.stderr}"
    assert "export" in result.stdout, f"expected shell export lines, got: {result.stdout!r}"


def test_env_platform_rejects_multiple(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`ocx env` composes ONE env — more than one --platform is a usage error."""
    _publish_multiplatform(ocx, unique_repo, tmp_path)
    project_dir = _project_with_lock(ocx, unique_repo, tmp_path)

    result = _run(ocx, project_dir, "env", f"--platform={ARM64}", f"--platform={AMD64}", "--shell=bash")
    assert result.returncode == EXIT_USAGE, (
        f"multiple --platform for env must exit {EXIT_USAGE}; got {result.returncode}: {result.stderr}"
    )
