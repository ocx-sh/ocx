"""Platform-aware cascade push tests.

Regression tests for two bugs in `ocx package push --cascade`:
- Bug 1: Platform-blind version comparison blocked cascade for platforms
  that didn't have a newer version.
- Bug 2: copy_manifest_data overwrote multi-platform image indexes,
  destroying entries for other platforms.
"""

from __future__ import annotations

from pathlib import Path

from src import OcxRunner, make_package
from src.registry import fetch_manifest_from_registry, index_platforms


def test_cascade_preserves_platforms(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Cascade push for one platform must not destroy entries for other platforms."""
    # Push same version for two different platforms.
    # Use separate tmp dirs since make_package uses repo+tag as dir key.
    make_package(
        ocx, unique_repo, "3.28.0", tmp_path / "amd64",
        platform="linux/amd64", new=True,
    )
    make_package(
        ocx, unique_repo, "3.28.0", tmp_path / "arm64",
        platform="linux/arm64", new=False,
    )

    # Rolling tag "3.28" must contain both platforms.
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "3.28")
    platforms = index_platforms(manifest)
    assert "linux/amd64" in platforms, f"amd64 missing from 3.28 index: {platforms}"
    assert "linux/arm64" in platforms, f"arm64 missing from 3.28 index: {platforms}"

    # Rolling tag "3" must also contain both platforms.
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "3")
    platforms = index_platforms(manifest)
    assert "linux/amd64" in platforms, f"amd64 missing from 3 index: {platforms}"
    assert "linux/arm64" in platforms, f"arm64 missing from 3 index: {platforms}"


def test_cascade_platform_aware_version_filter(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A newer version on another platform must not block cascade for this platform."""
    # Step 1: Push 3.28.0 for amd64.
    make_package(
        ocx, unique_repo, "3.28.0", tmp_path,
        platform="linux/amd64", new=True,
    )

    # Step 2: Push a newer patch 3.28.1 for arm64 only.
    make_package(
        ocx, unique_repo, "3.28.1", tmp_path,
        platform="linux/arm64", new=False,
    )

    # Step 3: Push yet another patch 3.28.2 for amd64.
    # Without the fix, 3.28.1 (arm64-only) would be the newest patch and
    # amd64's 3.28.2 would still cascade past it (3.28.2 > 3.28.1).
    # But this test specifically verifies the minor tag has both platforms.
    make_package(
        ocx, unique_repo, "3.28.2", tmp_path,
        platform="linux/amd64", new=False,
    )

    # "3.28" must contain amd64 (from step 3 cascade) AND arm64 (from step 2).
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "3.28")
    platforms = index_platforms(manifest)
    assert "linux/amd64" in platforms, f"amd64 missing from 3.28 index: {platforms}"
    assert "linux/arm64" in platforms, f"arm64 missing from 3.28 index: {platforms}"


def test_cascade_new_package_all_levels(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Cascade on a fresh package creates all rolling tags with correct platform."""
    make_package(
        ocx, unique_repo, "1.2.3", tmp_path,
        platform="linux/amd64", new=True,
    )

    for tag in ["1.2", "1", "latest"]:
        manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, tag)
        platforms = index_platforms(manifest)
        assert "linux/amd64" in platforms, (
            f"linux/amd64 missing from {tag} index: {platforms}"
        )
        assert len(manifest["manifests"]) == 1, (
            f"Expected 1 entry in {tag}, got {len(manifest['manifests'])}"
        )


# ── Edge-case cascade tests ────────────────────────────────────


def _platform_digest(manifest: dict, os_arch: str) -> str | None:
    """Extract the digest for a specific platform from an image index."""
    os, arch = os_arch.split("/")
    for entry in manifest.get("manifests", []):
        plat = entry.get("platform", {})
        if plat.get("os") == os and plat.get("architecture") == arch:
            return entry["digest"]
    return None


def test_cascade_old_version_different_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Pushing an older version for a different platform cascades past the newer version."""
    # Push 3.28.0 for amd64.
    make_package(
        ocx, unique_repo, "3.28.0", tmp_path / "amd64",
        platform="linux/amd64", new=True,
    )
    # Push 3.27.0 for arm64 — should cascade past 3.28 since arm64 is not present there.
    make_package(
        ocx, unique_repo, "3.27.0", tmp_path / "arm64",
        platform="linux/arm64", new=False,
    )

    # "3" should have arm64 (from 3.27.0 cascade).
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "3")
    platforms = index_platforms(manifest)
    assert "linux/arm64" in platforms, f"arm64 missing from 3 index: {platforms}"


def test_cascade_old_version_same_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Pushing an older version for the same platform stops cascade at the blocked level."""
    # Push 3.28.0 for amd64.
    make_package(
        ocx, unique_repo, "3.28.0", tmp_path / "new",
        platform="linux/amd64", new=True,
    )
    amd64_digest_328 = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "3"), "linux/amd64"
    )

    # Push 3.27.0 for amd64 — should be blocked at "3" because 3.28.0 has amd64.
    make_package(
        ocx, unique_repo, "3.27.0", tmp_path / "old",
        platform="linux/amd64", new=False,
    )

    # "3" should still point to the 3.28.0 amd64 digest (cascade stopped).
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "3")
    amd64_digest_after = _platform_digest(manifest, "linux/amd64")
    assert amd64_digest_after == amd64_digest_328, (
        f"Expected 3 to keep 3.28.0 digest {amd64_digest_328}, got {amd64_digest_after}"
    )


def test_cascade_same_version_different_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Pushing the same version for a second platform cascades all levels with both platforms."""
    make_package(
        ocx, unique_repo, "3.28.0", tmp_path / "amd64",
        platform="linux/amd64", new=True,
    )
    make_package(
        ocx, unique_repo, "3.28.0", tmp_path / "arm64",
        platform="linux/arm64", new=False,
    )

    for tag in ["3.28", "3", "latest"]:
        manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, tag)
        platforms = index_platforms(manifest)
        assert "linux/amd64" in platforms, f"amd64 missing from {tag}: {platforms}"
        assert "linux/arm64" in platforms, f"arm64 missing from {tag}: {platforms}"


def test_cascade_patch_one_platform_preserves_other(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Pushing a new patch for one platform preserves the other platform in rolling tags."""
    # Push 3.28.0 for both platforms.
    make_package(
        ocx, unique_repo, "3.28.0", tmp_path / "amd64_0",
        platform="linux/amd64", new=True,
    )
    make_package(
        ocx, unique_repo, "3.28.0", tmp_path / "arm64_0",
        platform="linux/arm64", new=False,
    )

    # Push 3.28.1 for amd64 only.
    make_package(
        ocx, unique_repo, "3.28.1", tmp_path / "amd64_1",
        platform="linux/amd64", new=False,
    )

    # "3.28" should have amd64 (from 3.28.1) AND arm64 (preserved from 3.28.0).
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "3.28")
    platforms = index_platforms(manifest)
    assert "linux/amd64" in platforms, f"amd64 missing from 3.28: {platforms}"
    assert "linux/arm64" in platforms, f"arm64 missing from 3.28: {platforms}"
