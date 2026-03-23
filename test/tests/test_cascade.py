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


# ── Variant cascade tests ─────────────────────────────────────


def test_variant_cascade_full_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Variant-prefixed tags cascade within their own variant track."""
    make_package(
        ocx, unique_repo, "debug-1.2.3", tmp_path,
        platform="linux/amd64", new=True,
    )

    # Rolling tags debug-1.2, debug-1, debug should all exist with correct platform.
    for tag in ["debug-1.2", "debug-1", "debug"]:
        manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, tag)
        platforms = index_platforms(manifest)
        assert "linux/amd64" in platforms, (
            f"linux/amd64 missing from {tag} index: {platforms}"
        )
        assert len(manifest["manifests"]) == 1, (
            f"Expected 1 entry in {tag}, got {len(manifest['manifests'])}"
        )

    # The variant cascade must NOT create default rolling tags.
    for tag in ["1.2", "1", "latest"]:
        try:
            fetch_manifest_from_registry(ocx.registry, unique_repo, tag)
            raise AssertionError(f"Tag '{tag}' should not exist after variant-only push")
        except Exception:
            pass  # Expected: tag does not exist


def test_variant_newer_cross_variant_no_overwrite(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Pushing a newer version in a different variant must not overwrite the first variant's tags."""
    # Push debug-1.2.3 — creates debug-1.2, debug-1, debug
    make_package(
        ocx, unique_repo, "debug-1.2.3", tmp_path / "debug",
        platform="linux/amd64", new=True,
    )
    debug1_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1"),
        "linux/amd64",
    )
    debug_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug"),
        "linux/amd64",
    )

    # Push pgo-2.0.0 — NEWER version number, different variant.
    # Must create pgo-2.0, pgo-2, pgo — must NOT touch debug-1, debug.
    make_package(
        ocx, unique_repo, "pgo-2.0.0", tmp_path / "pgo",
        platform="linux/amd64", new=False,
    )

    # Verify pgo cascade was created correctly
    for tag in ["pgo-2.0", "pgo-2", "pgo"]:
        manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, tag)
        platforms = index_platforms(manifest)
        assert "linux/amd64" in platforms, f"pgo tag '{tag}' missing amd64: {platforms}"

    # Verify debug tags are completely untouched
    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1"),
        "linux/amd64",
    ) == debug1_digest, "debug-1 was overwritten by pgo push"

    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug"),
        "linux/amd64",
    ) == debug_digest, "debug was overwritten by pgo push"


def test_variant_same_version_different_variant(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Two variants with identical version numbers must maintain independent cascades."""
    # Push debug-1.2.3
    make_package(
        ocx, unique_repo, "debug-1.2.3", tmp_path / "debug",
        platform="linux/amd64", new=True,
    )
    debug1_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1"),
        "linux/amd64",
    )

    # Push pgo-1.2.3 — SAME version number, different variant.
    make_package(
        ocx, unique_repo, "pgo-1.2.3", tmp_path / "pgo",
        platform="linux/amd64", new=False,
    )
    pgo1_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "pgo-1"),
        "linux/amd64",
    )

    # Both variant tracks must have their own independent rolling tags
    assert debug1_digest != pgo1_digest, (
        "debug-1 and pgo-1 point to same digest — variants are not isolated"
    )

    # debug tags untouched
    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1"),
        "linux/amd64",
    ) == debug1_digest, "debug-1 was overwritten by pgo-1.2.3 push"

    # Both variant terminals exist independently
    manifest_debug = fetch_manifest_from_registry(ocx.registry, unique_repo, "debug")
    manifest_pgo = fetch_manifest_from_registry(ocx.registry, unique_repo, "pgo")
    assert "linux/amd64" in index_platforms(manifest_debug)
    assert "linux/amd64" in index_platforms(manifest_pgo)


def test_default_same_version_as_variant(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Default (no variant) and variant with identical version numbers don't interfere."""
    # Push debug-1.2.3
    make_package(
        ocx, unique_repo, "debug-1.2.3", tmp_path / "debug",
        platform="linux/amd64", new=True,
    )
    debug1_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1"),
        "linux/amd64",
    )
    debug_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug"),
        "linux/amd64",
    )

    # Push 1.2.3 (no variant, SAME version number)
    make_package(
        ocx, unique_repo, "1.2.3", tmp_path / "default",
        platform="linux/amd64", new=False,
    )

    # Default cascade tags must exist
    for tag in ["1.2", "1", "latest"]:
        manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, tag)
        assert "linux/amd64" in index_platforms(manifest), (
            f"Default tag '{tag}' missing amd64"
        )

    # debug tags must be completely untouched — different digests, same version
    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1"),
        "linux/amd64",
    ) == debug1_digest, "debug-1 overwritten by default 1.2.3"

    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug"),
        "linux/amd64",
    ) == debug_digest, "debug overwritten by default 1.2.3"

    # Default "1" and "debug-1" must point to different digests
    default1_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "1"),
        "linux/amd64",
    )
    assert default1_digest != debug1_digest, (
        "Default '1' and 'debug-1' point to same digest — tracks not isolated"
    )


def test_newer_default_no_variant_overwrite(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Pushing a newer default version must not overwrite variant rolling tags."""
    # Push debug-1.0.0
    make_package(
        ocx, unique_repo, "debug-1.0.0", tmp_path / "debug",
        platform="linux/amd64", new=True,
    )
    debug_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug"),
        "linux/amd64",
    )

    # Push 2.0.0 (default, NEWER version number) — must not touch debug
    make_package(
        ocx, unique_repo, "2.0.0", tmp_path / "default",
        platform="linux/amd64", new=False,
    )

    # Default tags exist
    for tag in ["2.0", "2", "latest"]:
        manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, tag)
        assert "linux/amd64" in index_platforms(manifest), (
            f"Default tag '{tag}' missing amd64"
        )

    # debug must be untouched
    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug"),
        "linux/amd64",
    ) == debug_digest, "debug overwritten by newer default push"

    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1"),
        "linux/amd64",
    ) is not None, "debug-1 disappeared after default push"


def test_newer_variant_no_default_overwrite(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Pushing a newer variant version must not overwrite default rolling tags."""
    # Push 1.0.0 (default)
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "default",
        platform="linux/amd64", new=True,
    )
    latest_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "latest"),
        "linux/amd64",
    )
    default1_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "1"),
        "linux/amd64",
    )

    # Push debug-3.0.0 (variant, MUCH newer version number) — must not touch latest or 1
    make_package(
        ocx, unique_repo, "debug-3.0.0", tmp_path / "debug",
        platform="linux/amd64", new=False,
    )

    # Variant cascade created
    for tag in ["debug-3.0", "debug-3", "debug"]:
        manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, tag)
        assert "linux/amd64" in index_platforms(manifest), (
            f"Variant tag '{tag}' missing amd64"
        )

    # Default tags untouched
    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "latest"),
        "linux/amd64",
    ) == latest_digest, "latest overwritten by variant push"

    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "1"),
        "linux/amd64",
    ) == default1_digest, "'1' overwritten by variant push"


def test_variant_same_variant_blocking(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Older version within the same variant is blocked by newer same-variant version."""
    # Push debug-1.2.3
    make_package(
        ocx, unique_repo, "debug-1.2.3", tmp_path / "newer",
        platform="linux/amd64", new=True,
    )
    debug1_digest = _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1"),
        "linux/amd64",
    )

    # Push debug-1.1.0 — should be blocked at debug-1 because debug-1.2.3 exists
    make_package(
        ocx, unique_repo, "debug-1.1.0", tmp_path / "older",
        platform="linux/amd64", new=False,
    )

    # debug-1 should still point to debug-1.2.3
    assert _platform_digest(
        fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1"),
        "linux/amd64",
    ) == debug1_digest, "debug-1 overwritten by older debug push"

    # debug-1.1 should still cascade (within its own patch range)
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "debug-1.1")
    assert "linux/amd64" in index_platforms(manifest), "debug-1.1 missing after push"


def test_variant_preserves_platforms(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Variant cascade preserves platform entries from previous pushes."""
    make_package(
        ocx, unique_repo, "debug-1.2.3", tmp_path / "amd64",
        platform="linux/amd64", new=True,
    )
    make_package(
        ocx, unique_repo, "debug-1.2.3", tmp_path / "arm64",
        platform="linux/arm64", new=False,
    )

    for tag in ["debug-1.2", "debug-1", "debug"]:
        manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, tag)
        platforms = index_platforms(manifest)
        assert "linux/amd64" in platforms, f"amd64 missing from {tag}: {platforms}"
        assert "linux/arm64" in platforms, f"arm64 missing from {tag}: {platforms}"


def test_three_variants_independent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Three variants plus default all maintain independent cascade chains."""
    # Push default 1.0.0
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "default",
        platform="linux/amd64", new=True,
    )
    # Push debug-1.5.0
    make_package(
        ocx, unique_repo, "debug-1.5.0", tmp_path / "debug",
        platform="linux/amd64", new=False,
    )
    # Push pgo-2.0.0
    make_package(
        ocx, unique_repo, "pgo-2.0.0", tmp_path / "pgo",
        platform="linux/amd64", new=False,
    )
    # Push slim-3.0.0
    make_package(
        ocx, unique_repo, "slim-3.0.0", tmp_path / "slim",
        platform="linux/amd64", new=False,
    )

    # Collect all terminal digests — each must be unique
    digests = {}
    for tag in ["latest", "debug", "pgo", "slim"]:
        digests[tag] = _platform_digest(
            fetch_manifest_from_registry(ocx.registry, unique_repo, tag),
            "linux/amd64",
        )

    unique_digests = set(digests.values())
    assert len(unique_digests) == 4, (
        f"Expected 4 unique terminal digests, got {len(unique_digests)}: {digests}"
    )

    # Each variant's major rolling tag also has its own unique digest
    major_digests = {}
    for tag in ["1", "debug-1", "pgo-2", "slim-3"]:
        major_digests[tag] = _platform_digest(
            fetch_manifest_from_registry(ocx.registry, unique_repo, tag),
            "linux/amd64",
        )

    unique_major = set(major_digests.values())
    assert len(unique_major) == 4, (
        f"Expected 4 unique major digests, got {len(unique_major)}: {major_digests}"
    )
