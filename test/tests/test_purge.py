"""Acceptance tests for uninstall --purge with dependency-aware scoped GC."""

import os
import sys
from pathlib import Path

import pytest

from src import OcxRunner, PackageInfo, assert_dir_exists, assert_not_exists
from src.helpers import make_package
from src.registry import fetch_manifest_digest
from tests.test_assembly import _make_two_packages_sharing_layer


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _push_leaf(ocx: OcxRunner, repo: str, tmp_path: Path, **kwargs) -> PackageInfo:
    return make_package(ocx, repo, "1.0.0", tmp_path, new=True, **kwargs)


def _dep_entry(ocx: OcxRunner, pkg: PackageInfo) -> dict:
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    return {"identifier": f"{pkg.fq}@{digest}"}


def _push_with_deps(
    ocx: OcxRunner, repo: str, tag: str, tmp_path: Path, deps: list[dict], **kwargs
) -> PackageInfo:
    return make_package(ocx, repo, tag, tmp_path, dependencies=deps, **kwargs)


def _objects_dir(ocx: OcxRunner) -> Path:
    return Path(ocx.ocx_home) / "packages"


def _count_object_dirs(ocx: OcxRunner) -> int:
    objects_root = _objects_dir(ocx)
    if not objects_root.exists():
        return 0
    return sum(1 for p in objects_root.rglob("content") if p.is_dir())


# ---------------------------------------------------------------------------
# Basic purge
# ---------------------------------------------------------------------------


def test_purge_removes_object_directory(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx uninstall --purge <pkg>"""
    pkg = published_package
    result = ocx.json("install", pkg.short)
    candidate = Path(result[pkg.short]["path"])
    content = candidate.resolve()
    assert_dir_exists(content)

    ocx.plain("uninstall", "--purge", pkg.short)
    assert_not_exists(content)


# ---------------------------------------------------------------------------
# Purge cascades to transitive dependencies
# ---------------------------------------------------------------------------


def test_purge_cascades_to_transitive_deps(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B: purging A also removes orphaned B."""
    leaf = _push_leaf(ocx, f"{unique_repo}_leaf", tmp_path)
    app = _push_with_deps(
        ocx, f"{unique_repo}_app", "1.0.0", tmp_path, deps=[_dep_entry(ocx, leaf)]
    )
    ocx.json("install", "--select", app.short)
    assert _count_object_dirs(ocx) == 2

    ocx.plain("uninstall", "--purge", "-d", app.short)
    assert _count_object_dirs(ocx) == 0, "both app and leaf should be purged"


def test_purge_cascades_through_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B->C: purging A removes the entire chain."""
    c = _push_leaf(ocx, f"{unique_repo}_c", tmp_path)
    b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[_dep_entry(ocx, c)]
    )
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[_dep_entry(ocx, b)]
    )
    ocx.json("install", "--select", a.short)
    assert _count_object_dirs(ocx) == 3

    ocx.plain("uninstall", "--purge", "-d", a.short)
    assert _count_object_dirs(ocx) == 0, "entire A->B->C chain should be purged"


# ---------------------------------------------------------------------------
# Purge does NOT affect unrelated objects
# ---------------------------------------------------------------------------


def test_purge_does_not_collect_unrelated_orphan(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Purging A should not collect an unrelated orphaned object X."""
    # Install two independent packages
    pkg_a = _push_leaf(ocx, f"{unique_repo}_a", tmp_path)
    pkg_x = _push_leaf(ocx, f"{unique_repo}_x", tmp_path)

    ocx.json("install", "--select", pkg_a.short)
    ocx.json("install", "--select", pkg_x.short)
    assert _count_object_dirs(ocx) == 2

    # Make X an orphan by removing its symlinks (but not purging)
    ocx.plain("uninstall", "-d", pkg_x.short)

    # Now purge A — X should NOT be collected
    ocx.plain("uninstall", "--purge", "-d", pkg_a.short)
    assert _count_object_dirs(ocx) == 1, "orphaned X should survive A's purge"


def test_purge_does_not_collect_unrelated_orphan_with_deps(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Purging A->B should not collect unrelated orphaned X->Y."""
    leaf_b = _push_leaf(ocx, f"{unique_repo}_b", tmp_path)
    pkg_a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[_dep_entry(ocx, leaf_b)]
    )
    leaf_y = _push_leaf(ocx, f"{unique_repo}_y", tmp_path)
    pkg_x = _push_with_deps(
        ocx, f"{unique_repo}_x", "1.0.0", tmp_path, deps=[_dep_entry(ocx, leaf_y)]
    )

    ocx.json("install", "--select", pkg_a.short)
    ocx.json("install", "--select", pkg_x.short)
    assert _count_object_dirs(ocx) == 4

    # Make X an orphan
    ocx.plain("uninstall", "-d", pkg_x.short)

    # Purge A — only A and B should be removed, X and Y survive
    ocx.plain("uninstall", "--purge", "-d", pkg_a.short)
    assert _count_object_dirs(ocx) == 2, "orphaned X and Y should survive A's purge"


# ---------------------------------------------------------------------------
# Shared dependency edge cases
# ---------------------------------------------------------------------------


def test_purge_preserves_shared_dep_when_one_pkg_uninstalled(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->C and B->C: purging A preserves C because B still needs it."""
    leaf_c = _push_leaf(ocx, f"{unique_repo}_c", tmp_path)
    dep_c = _dep_entry(ocx, leaf_c)

    pkg_a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[dep_c]
    )
    pkg_b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[dep_c]
    )

    ocx.json("install", "--select", pkg_a.short)
    ocx.json("install", "--select", pkg_b.short)
    assert _count_object_dirs(ocx) == 3  # A, B, C

    # Purge A — C should survive because B still depends on it
    ocx.plain("uninstall", "--purge", "-d", pkg_a.short)
    assert _count_object_dirs(ocx) == 2, "B and C should survive"


def test_purge_collects_shared_dep_when_both_pkgs_uninstalled(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->C and B->C: purging both A and B also collects C."""
    leaf_c = _push_leaf(ocx, f"{unique_repo}_c", tmp_path)
    dep_c = _dep_entry(ocx, leaf_c)

    pkg_a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[dep_c]
    )
    pkg_b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[dep_c]
    )

    ocx.json("install", "--select", pkg_a.short)
    ocx.json("install", "--select", pkg_b.short)
    assert _count_object_dirs(ocx) == 3  # A, B, C

    # Purge both in one command — C should now be collected
    ocx.plain("uninstall", "--purge", "-d", pkg_a.short, pkg_b.short)
    assert _count_object_dirs(ocx) == 0, "all three should be purged"


def test_purge_collects_shared_dep_sequential_uninstall(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->C and B->C: purge A (C survives), then purge B (C now collected)."""
    leaf_c = _push_leaf(ocx, f"{unique_repo}_c", tmp_path)
    dep_c = _dep_entry(ocx, leaf_c)

    pkg_a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[dep_c]
    )
    pkg_b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[dep_c]
    )

    ocx.json("install", "--select", pkg_a.short)
    ocx.json("install", "--select", pkg_b.short)
    assert _count_object_dirs(ocx) == 3

    # Purge A — C survives
    ocx.plain("uninstall", "--purge", "-d", pkg_a.short)
    assert _count_object_dirs(ocx) == 2

    # Purge B — C now collected
    ocx.plain("uninstall", "--purge", "-d", pkg_b.short)
    assert _count_object_dirs(ocx) == 0


# ---------------------------------------------------------------------------
# Diamond dependency with purge
# ---------------------------------------------------------------------------


def test_purge_diamond_shared_leaf_survives_partial(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->{B,C}, B->D, C->D: purge A collects everything (D shared but only reachable from A)."""
    d = _push_leaf(ocx, f"{unique_repo}_d", tmp_path)
    dep_d = _dep_entry(ocx, d)

    b = _push_with_deps(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[dep_d])
    c = _push_with_deps(ocx, f"{unique_repo}_c", "1.0.0", tmp_path, deps=[dep_d])

    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b), _dep_entry(ocx, c)]
    )

    ocx.json("install", "--select", a.short)
    assert _count_object_dirs(ocx) == 4  # A, B, C, D

    ocx.plain("uninstall", "--purge", "-d", a.short)
    assert _count_object_dirs(ocx) == 0, "entire diamond should be purged"


# ---------------------------------------------------------------------------
# Purge protects objects still needed as dependencies
# ---------------------------------------------------------------------------


def test_purge_protects_dep_still_reachable_from_other_root(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """R->A->B: purging A's candidate preserves A and B because R still depends on A."""
    b = _push_leaf(ocx, f"{unique_repo}_b", tmp_path)
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[_dep_entry(ocx, b)]
    )
    r = _push_with_deps(
        ocx, f"{unique_repo}_r", "1.0.0", tmp_path, deps=[_dep_entry(ocx, a)]
    )

    ocx.json("install", "--select", a.short)
    ocx.json("install", "--select", r.short)
    assert _count_object_dirs(ocx) == 3  # R, A, B

    # Uninstall A with purge — A is still reachable as a dependency of R
    ocx.plain("uninstall", "--purge", "-d", a.short)
    assert _count_object_dirs(ocx) == 3, "R still depends on A, nothing should be purged"


# ---------------------------------------------------------------------------
# Shared-layer hardlink survival (Sub-plan 6 / I5 purge variant)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform == "win32", reason="inode comparison not meaningful on Windows")
def test_purge_preserves_shared_layer_inodes(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Purging package A leaves package B's hardlinked files intact with the same inode.

    Two packages reference the same OCI layer (pushed from identical binary content
    but with distinct per-repo metadata so they are separate packages).

    After installing both:
    - A file in package B's content/ shares an inode with the same file in A's
      content/ (both are hardlinks to the same underlying layer inode).

    After purging package A:
    - Package A's content directory is gone.
    - Package B's file still exists and is still readable.
    - The inode of B's file is unchanged (the hardlink kept the inode alive).
    """
    short_a, short_b, shared_file_rel = _make_two_packages_sharing_layer(
        ocx, tmp_path, unique_repo
    )

    # Install both packages
    ocx.json("install", "--select", short_a)
    ocx.json("install", "--select", short_b)

    result_a = ocx.json("find", short_a)
    result_b = ocx.json("find", short_b)
    root_a = Path(result_a[short_a])
    root_b = Path(result_b[short_b])
    file_a = root_a / "content" / shared_file_rel
    file_b = root_b / "content" / shared_file_rel

    assert file_a.exists(), f"Package A file not found before purge: {file_a}"
    assert file_b.exists(), f"Package B file not found before purge: {file_b}"

    # Both files must hardlink to the same underlying inode before the purge.
    inode_a_before = os.stat(str(file_a)).st_ino
    inode_b_before = os.stat(str(file_b)).st_ino
    assert inode_a_before == inode_b_before, (
        f"Shared-layer hardlink missing before purge: "
        f"A inode={inode_a_before}, B inode={inode_b_before}"
    )

    # Purge package A (removes its candidate symlink and content directory)
    ocx.plain("uninstall", "--purge", "-d", short_a)

    # Behaviour-centric assertions: A's package root must disappear, B's must
    # survive with its hardlink intact (same inode as before the purge).
    assert not root_a.exists(), (
        f"A's package root must be gone after purge: {root_a}"
    )
    assert root_b.exists(), (
        f"B's package root must survive A's purge: {root_b}"
    )
    assert file_b.exists(), f"Package B file vanished after purging A: {file_b}"

    inode_b_after = os.stat(str(file_b)).st_ino
    assert inode_b_after == inode_b_before, (
        f"B's hardlinked file must retain its inode "
        f"(before={inode_b_before}, after={inode_b_after})"
    )

    # Verify the content is still correct
    content_bytes = file_b.read_text()
    assert "shared-layer-binary" in content_bytes, (
        f"Package B's file content corrupted after purging A: {content_bytes!r}"
    )
