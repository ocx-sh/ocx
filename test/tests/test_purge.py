"""Acceptance tests for uninstall --purge with dependency-aware scoped GC."""

from pathlib import Path

from src import OcxRunner, PackageInfo, assert_dir_exists, assert_not_exists
from src.helpers import make_package
from src.registry import fetch_manifest_digest


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
    return Path(ocx.ocx_home) / "objects"


def _count_object_dirs(ocx: OcxRunner) -> int:
    objects_root = _objects_dir(ocx)
    if not objects_root.exists():
        return 0
    return sum(1 for p in objects_root.rglob("content") if p.is_dir())


def _find_content_path(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    result = ocx.json("find", pkg.short)
    return Path(result[pkg.short]).resolve()


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
