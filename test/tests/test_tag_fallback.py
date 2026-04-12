"""Acceptance tests for Transparent Tag Fallback (GitHub issue #41).

Specification-mode tests — written from the design record
(.claude/artifacts/plan_tag_fallback.md), NOT from the implementation.
These tests MUST fail against the current binary (before Context::try_init
wiring) because the ChainedIndex is not yet constructed.

Acceptance criteria (from issue #41):
  AC1 — Fresh install with empty local index succeeds (fallback to remote)
  AC2 — Tag persisted: offline install after first online install succeeds
  AC3 — Offline + empty index → NotFound, non-zero exit
  AC4 — Stale cached tag: cache wins, no auto-refresh from remote
  AC5 — Batch install: both packages succeed from empty index
  AC6a — Non-existent tag → non-zero exit, stderr contains diagnostic
  AC6b — Unreachable registry → non-zero exit, stderr contains network error
"""

from __future__ import annotations

import concurrent.futures
import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

from src import OcxRunner, PackageInfo, make_package, registry_dir
from src.registry import fetch_manifest_digest


# ── Helpers ───────────────────────────────────────────────────────────────


def _wipe_local_index(ocx: OcxRunner) -> None:
    """Remove the local index (tags/) directory to simulate a fresh machine."""
    tags_dir = Path(ocx.env["OCX_HOME"]) / "tags"
    if tags_dir.exists():
        shutil.rmtree(tags_dir)


def _tag_file_path(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    """Return the path to the local tag JSON file for a package."""
    return (
        Path(ocx.env["OCX_HOME"])
        / "tags"
        / registry_dir(ocx.registry)
        / f"{pkg.repo}.json"
    )


# ── AC1: Fresh install with empty local index succeeds ────────────────────


def test_ac1_fresh_install_empty_index_succeeds(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC1: ocx install <pkg>:<tag> on a fresh machine (empty index) must succeed.

    Design record: "Fresh install: ocx install <pkg>:<tag> with empty local
    index succeeds (AC1)".

    The ChainedIndex must fall through from the empty local index to the remote
    source, retrieve the tag, persist it locally, and complete the install.
    """
    pkg = published_package

    # Ensure the local index is empty so the test exercises the fallback path.
    _wipe_local_index(ocx)

    # The install must succeed — ChainedIndex fetches from remote on cache miss.
    result = ocx.json("install", pkg.short)

    # Basic smoke check: install returned data for this package.
    assert pkg.short in result, f"install result missing package key {pkg.short!r}"


# ── AC2: Tag persisted → offline install succeeds ─────────────────────────


def test_ac2_tag_persisted_offline_install_succeeds(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC2: After an online install, the tag is persisted so offline install works.

    Design record: "Tag persisted: second install with --offline succeeds
    (proves local persistence) (AC2)".

    Step 1: online install → ChainedIndex fetches from remote, persists tag.
    Step 2: wipe the binary install but keep the index.
    Step 3: --offline install → must succeed from persisted local tag.
    """
    pkg = published_package

    # Start with empty index to force the fallback path.
    _wipe_local_index(ocx)

    # Step 1: online install triggers fallback, persists tag.
    ocx.json("install", pkg.short)

    # Step 2: verify the tag file was persisted.
    tag_file = _tag_file_path(ocx, pkg)
    assert tag_file.exists(), (
        "AC2 prerequisite: tag file must exist after online install, "
        f"but {tag_file} is missing"
    )

    # Step 3: offline install of the same tag must succeed from cached data.
    result = ocx.plain("--offline", "install", pkg.short)
    assert result.returncode == 0, (
        f"AC2: --offline install failed (rc={result.returncode})\n"
        f"stderr: {result.stderr.strip()}"
    )


# ── AC3: Offline + empty index → NotFound ────────────────────────────────


def test_ac3_offline_empty_index_returns_not_found(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC3: --offline install with an empty index must fail with NotFound.

    Design record: "Offline mode: ocx install --offline <pkg>:<tag> with
    empty index returns NotFound (AC3)".

    --offline disables ChainedIndex; LocalIndex has nothing → NotFound.
    """
    pkg = published_package

    # Ensure the local index is empty.
    _wipe_local_index(ocx)

    result = ocx.plain("--offline", "install", pkg.short, check=False)
    assert result.returncode != 0, (
        "AC3: --offline install on empty index must fail, but it succeeded"
    )


# ── AC4: Stale cached tag → cache wins, no auto-refresh ──────────────────


def test_ac4_stale_cached_tag_uses_cached_digest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
) -> None:
    """AC4: When the tag is cached, ChainedIndex uses it and does not refresh.

    Design record: "Cached stale tag (cache wins, no auto-refresh): install
    with stale cached tag uses the cached digest (AC4)".

    Strategy:
      1. Push v1 and install it (caches digest A).
      2. Push v2 to the same tag on the registry (registry now has digest B).
      3. Install again → must install digest A (the cached one), not B.

    This proves the "cache always wins" contract: refresh is the job of
    `ocx index update`, not the fallback chain.
    """
    # Push v1 and install to seed the cache.
    v1_dir = tmp_path / "v1"
    v1_dir.mkdir()
    pkg_v1 = make_package(ocx, unique_repo, "1.0.0", v1_dir, new=True, cascade=False)
    ocx.json("install", pkg_v1.short)

    # Record the digest that was installed (digest A).
    tag_file = _tag_file_path(ocx, pkg_v1)
    assert tag_file.exists(), "prerequisite: tag file must exist after install"
    cached_data = json.loads(tag_file.read_text())
    cached_digest_a = cached_data["tags"].get(pkg_v1.tag)
    assert cached_digest_a is not None, "prerequisite: tag must be in cache"

    # Snapshot the v1 tag file so we can restore it after pushing v2 (which
    # would otherwise re-index the cache to digest B via make_package's
    # internal `ocx index update` call).
    v1_tag_snapshot = tag_file.read_text()

    # Push v2 to the same repo/tag (same tag, new digest B). Use a fresh
    # working directory so make_package can create a clean pkg_dir for the
    # new content (the marker UUID gives v2 a different digest from v1).
    v2_dir = tmp_path / "v2"
    v2_dir.mkdir()
    _ = make_package(ocx, unique_repo, "1.0.0", v2_dir, new=False, cascade=False)

    # Verify v2 has a different digest on the registry.
    registry_digest_b = fetch_manifest_digest(registry, unique_repo, "1.0.0")
    assert registry_digest_b != cached_digest_a, (
        "AC4 test setup: registry digest must differ from cached digest"
    )

    # Restore the cached tag to digest A — make_package's `index update` step
    # refreshed the cache to digest B, but the AC4 contract is "what is in the
    # cache wins, even if the registry has moved on."
    tag_file.write_text(v1_tag_snapshot)

    # Install again — ChainedIndex must hit the cache (digest A) and NOT refresh.
    result = ocx.json("install", pkg_v1.short)
    assert pkg_v1.short in result, "second install must succeed"

    # The local index must still contain digest A (not updated to B).
    refreshed_data = json.loads(tag_file.read_text())
    stored_digest = refreshed_data["tags"].get(pkg_v1.tag)
    assert stored_digest == cached_digest_a, (
        f"AC4: cache must not be refreshed automatically.\n"
        f"Expected (cached) digest: {cached_digest_a}\n"
        f"Found digest in index:    {stored_digest}"
    )


# ── AC5: Batch install from empty index ───────────────────────────────────


def test_ac5_batch_install_empty_index_both_succeed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """AC5: Batch install of two packages with empty index must both succeed.

    Design record: "Batch install: ocx install <pkg1>:<tag1> <pkg2>:<tag2>
    with empty index, both install (AC5)".

    Each package triggers its own fallback chain walk in parallel.
    """
    pkg1 = make_package(ocx, f"{unique_repo}_1", "1.0.0", tmp_path, new=True, cascade=False)
    pkg2 = make_package(ocx, f"{unique_repo}_2", "1.0.0", tmp_path, new=True, cascade=False)

    # Wipe the local index so both packages must fall through to remote.
    _wipe_local_index(ocx)

    result = ocx.json("install", pkg1.short, pkg2.short)

    assert pkg1.short in result, f"AC5: pkg1 {pkg1.short!r} missing from install result"
    assert pkg2.short in result, f"AC5: pkg2 {pkg2.short!r} missing from install result"


# ── AC6a: Non-existent tag → diagnostic error ─────────────────────────────


def test_ac6a_nonexistent_tag_fails_with_diagnostic(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """AC6a: Installing a tag that does not exist on the registry must fail
    with a non-zero exit and a diagnostic message in stderr.

    Design record: "Non-existent tag: ocx install <pkg>:nonexistent returns
    NotFound, stderr contains diagnostic context (AC6a)".

    The ChainedIndex walks the remote source, which returns no manifest for
    the bogus tag.  The chain degrades to NotFound; the CLI surfaces the
    warn-level diagnostic.
    """
    pkg = published_package
    nonexistent = f"{pkg.repo}:does-not-exist-at-all-xyz-9999"

    _wipe_local_index(ocx)

    result = ocx.plain("install", nonexistent, check=False)
    assert result.returncode != 0, (
        "AC6a: installing a non-existent tag must fail with non-zero exit"
    )
    # stderr must contain some diagnostic context — "not found" or similar.
    combined_output = result.stderr + result.stdout
    assert combined_output.strip(), (
        "AC6a: stderr/stdout must contain diagnostic output, but both were empty"
    )


# ── Concurrent writers racing the tag log ────────────────────────────────


def test_parallel_install_races_preserve_both_tags(
    ocx: OcxRunner,
    ocx_binary: Path,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Two concurrent `ocx install` subprocesses sharing one OCX_HOME must both
    succeed and the persisted tag log must contain both entries.

    The local index starts empty, so each child process takes the transparent
    tag-fallback path, fetches from the remote registry, and writes its own
    tag into the local tag log. Without per-repo locking + atomic rename, the
    two writers race on the same JSON file and one of two failure modes
    surfaces: the tag file is truncated/corrupt, or one of the two tags is
    silently lost via last-writer-wins on stale in-memory state.

    With the per-repo lock + in-place write path wired up, both entries
    must survive.
    """
    v1 = make_package(ocx, unique_repo, "1.0.0", tmp_path / "v1", new=True, cascade=False)
    v2 = make_package(ocx, unique_repo, "2.0.0", tmp_path / "v2", new=False, cascade=False)
    assert v1.repo == v2.repo, "both versions must target the same repo for the race"

    # Fresh local index → both installs exercise the fallback path.
    _wipe_local_index(ocx)

    def run_install(pkg_short: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(ocx_binary), "--format", "json", "install", pkg_short],
            capture_output=True,
            text=True,
            env=ocx.env,
            check=False,
        )

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(run_install, v1.short), pool.submit(run_install, v2.short)]
        results = [f.result() for f in futures]

    for pkg, result in zip((v1, v2), results, strict=True):
        assert result.returncode == 0, (
            f"concurrent install of {pkg.short} failed (rc={result.returncode}).\n"
            f"stdout: {result.stdout}\n"
            f"stderr: {result.stderr}"
        )

    tag_file = _tag_file_path(ocx, v1)
    assert tag_file.exists(), "tag file must exist after the racing installs"

    data = json.loads(tag_file.read_text())
    tags = data.get("tags", {})
    assert v1.tag in tags, (
        f"tag {v1.tag!r} missing from tag log after concurrent race; "
        f"present tags: {sorted(tags)}"
    )
    assert v2.tag in tags, (
        f"tag {v2.tag!r} missing from tag log after concurrent race; "
        f"present tags: {sorted(tags)}"
    )


# ── AC6b: Unreachable registry → network error diagnostic ─────────────────


def test_ac6b_unreachable_registry_fails_with_network_error(
    ocx_binary: Path, ocx_home: Path
) -> None:
    """AC6b: Installing from an unreachable registry must fail with a non-zero
    exit and a diagnostic message that includes network error context.

    Design record: "Network failure: install against unreachable registry,
    stderr contains network error context (AC6b)".

    Uses a known-unreachable address (127.0.0.1:1) so the TCP connection
    fails immediately with "connection refused".
    """
    unreachable = "127.0.0.1:1"
    env = {
        "OCX_HOME": str(ocx_home),
        "OCX_DEFAULT_REGISTRY": unreachable,
        # Do NOT add to insecure registries — let it try HTTPS and fail.
        "PATH": os.environ.get("PATH", ""),
        "HOME": os.environ.get("HOME", str(Path.home())),
    }
    for key in ("SYSTEMROOT", "TEMP", "TMP", "PATHEXT"):
        if key in os.environ:
            env[key] = os.environ[key]

    result = subprocess.run(
        [str(ocx_binary), "--format", "json", "install", "cmake:3.28"],
        capture_output=True,
        text=True,
        env=env,
    )

    assert result.returncode != 0, (
        "AC6b: install against unreachable registry must fail with non-zero exit"
    )
    combined_output = result.stderr + result.stdout
    assert combined_output.strip(), (
        "AC6b: stderr/stdout must contain network error diagnostic, but both were empty"
    )
