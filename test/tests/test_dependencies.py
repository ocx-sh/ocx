"""Acceptance tests for package dependencies."""

from __future__ import annotations

from pathlib import Path

import pytest

from src.assertions import assert_not_exists, assert_symlink_exists
from src.helpers import make_package, make_package_with_entrypoints
from src.registry import fetch_manifest_digest
from src.runner import OcxRunner, PackageInfo, registry_dir


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _push_leaf(ocx: OcxRunner, repo: str, tmp_path: Path, **kwargs) -> PackageInfo:
    """Push a leaf package (no dependencies)."""
    return make_package(ocx, repo, "1.0.0", tmp_path, new=True, **kwargs)


def _push_with_deps(
    ocx: OcxRunner,
    repo: str,
    tag: str,
    tmp_path: Path,
    deps: list[dict],
    *,
    new: bool = True,
    env: list[dict] | None = None,
) -> PackageInfo:
    """Push a package with dependency metadata."""
    return make_package(
        ocx,
        repo,
        tag,
        tmp_path,
        new=new,
        env=env,
        dependencies=deps,
    )


def _dep_entry(ocx: OcxRunner, pkg: PackageInfo, *, visibility: str | None = None) -> dict:
    """Build a dependency descriptor from a published PackageInfo."""
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    entry: dict = {"identifier": f"{pkg.fq}@{digest}"}
    if visibility is not None:
        entry["visibility"] = visibility
    return entry


def _objects_dir(ocx: OcxRunner) -> Path:
    """Return the packages/ directory root."""
    return Path(ocx.ocx_home) / "packages"


def _count_object_dirs(ocx: OcxRunner) -> int:
    """Count the number of object directories in the store."""
    objects_root = _objects_dir(ocx)
    if not objects_root.exists():
        return 0
    count = 0
    for p in objects_root.rglob("content"):
        if p.is_dir():
            count += 1
    return count


# ---------------------------------------------------------------------------
# Tests: Install with dependencies
# ---------------------------------------------------------------------------


def test_install_with_one_dep_pulls_both(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Installing a package with one dependency pulls both to the object store."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = _push_leaf(ocx, leaf_repo, tmp_path)
    dep = _dep_entry(ocx, leaf)
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[dep])

    ocx.json("install", "--select", app.short)

    # Both objects should be in the store (app + leaf = 2 content dirs)
    assert _count_object_dirs(ocx) == 2


def test_install_deps_no_symlinks_for_deps(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Dependencies pulled transitively should NOT have install symlinks."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = _push_leaf(ocx, leaf_repo, tmp_path)
    dep = _dep_entry(ocx, leaf)
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[dep])

    ocx.json("install", "--select", app.short)

    # App should have candidate symlink
    reg_slug = registry_dir(ocx.registry)
    app_candidate = Path(ocx.ocx_home) / "symlinks" / reg_slug / app_repo / "candidates" / "1.0.0"
    assert_symlink_exists(app_candidate)

    # Leaf should NOT have candidate symlink (it was pulled as a dependency)
    leaf_candidate = Path(ocx.ocx_home) / "symlinks" / reg_slug / leaf_repo / "candidates" / "1.0.0"
    assert_not_exists(leaf_candidate)


# ---------------------------------------------------------------------------
# Tests: GC with dependencies
# ---------------------------------------------------------------------------


def test_clean_does_not_collect_dependency(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A dependency object should not be collected while its dependent is installed."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = _push_leaf(ocx, leaf_repo, tmp_path)
    dep = _dep_entry(ocx, leaf)
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[dep])

    ocx.json("install", "--select", app.short)

    # Run clean — should not remove anything (all objects are referenced)
    ocx.json("clean")

    # Both objects should still be present
    assert _count_object_dirs(ocx) == 2


def test_clean_collects_chain_after_uninstall(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """After uninstalling the top-level, both it and its dependency should be collectible."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = _push_leaf(ocx, leaf_repo, tmp_path)
    dep = _dep_entry(ocx, leaf)
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[dep])

    ocx.json("install", "--select", app.short)
    assert _count_object_dirs(ocx) == 2

    ocx.plain("uninstall", "--purge", "-d", app.short)
    ocx.json("clean")

    # After clean, both objects should be gone
    assert _count_object_dirs(ocx) == 0


# ---------------------------------------------------------------------------
# Tests: Transitive chain
# ---------------------------------------------------------------------------


def test_transitive_chain_pulls_all_three(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Installing A (-> B -> C) pulls all three objects to the store."""
    c_repo = f"{unique_repo}_c"
    b_repo = f"{unique_repo}_b"
    a_repo = f"{unique_repo}_a"

    c = _push_leaf(ocx, c_repo, tmp_path)
    b = _push_with_deps(ocx, b_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, c)])
    a = _push_with_deps(ocx, a_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, b)])

    ocx.json("install", "--select", a.short)

    assert _count_object_dirs(ocx) == 3


# ---------------------------------------------------------------------------
# Tests: Env vars from dependencies
# ---------------------------------------------------------------------------


def test_env_includes_dependency_vars(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """ocx env on an app should include env vars declared by its exported dependency."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = _push_leaf(ocx, leaf_repo, tmp_path)
    dep = _dep_entry(ocx, leaf, visibility="public")
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[dep])

    ocx.json("install", "--select", app.short)

    env_result = ocx.json("env", app.short)
    assert env_result is not None

    # The leaf sets a {REPO_UPPER}_HOME constant. Check that key is present.
    # env_result["entries"] is a list of {"key": "...", "value": "...", "type": "..."}
    leaf_home_key = leaf_repo.upper().replace("-", "_") + "_HOME"
    env_keys = [e["key"] for e in env_result["entries"]]
    assert leaf_home_key in env_keys, (
        f"expected {leaf_home_key!r} from leaf dep in env output; got keys: {env_keys}"
    )


# ---------------------------------------------------------------------------
# Tests: Diamond deduplication
# ---------------------------------------------------------------------------


def test_diamond_dep_deduplicates(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A -> B,C -> D should result in exactly 4 objects (D not duplicated)."""
    d_repo = f"{unique_repo}_d"
    b_repo = f"{unique_repo}_b"
    c_repo = f"{unique_repo}_c"
    a_repo = f"{unique_repo}_a"

    d = _push_leaf(ocx, d_repo, tmp_path)
    b = _push_with_deps(ocx, b_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, d)])
    c = _push_with_deps(ocx, c_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, d)])
    a = _push_with_deps(ocx, a_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, b), _dep_entry(ocx, c)])

    ocx.json("install", "--select", a.short)

    assert _count_object_dirs(ocx) == 4


# ---------------------------------------------------------------------------
# Tests: Missing dependency error
# ---------------------------------------------------------------------------


def test_install_with_missing_dep_reports_error(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Installing a package whose dependency doesn't exist should fail with a clear error."""
    app_repo = f"{unique_repo}_app"

    # Build a dep entry referencing a non-existent package (fabricated digest)
    fake_fq = f"{ocx.registry}/{unique_repo}_ghost:1.0.0"
    fake_digest = "sha256:" + "a" * 64
    bad_dep = {"identifier": f"{fake_fq}@{fake_digest}"}
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[bad_dep])

    result = ocx.run("install", app.short, check=False)
    assert result.returncode != 0, "expected non-zero exit for missing dependency"

    stderr = result.stderr.lower()
    assert "not found" in stderr or "missing" in stderr or "error" in stderr, (
        f"expected error message mentioning missing dep; stderr: {result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Tests: Backward compatibility
# ---------------------------------------------------------------------------


def test_package_without_deps_works(published_package: PackageInfo, ocx: OcxRunner):
    """Packages without dependencies should continue to work unchanged."""
    result = ocx.json("install", "--select", published_package.short)
    assert result is not None

    find_result = ocx.json("find", published_package.short)
    assert find_result is not None


# ---------------------------------------------------------------------------
# Helpers (additional)
# ---------------------------------------------------------------------------


def _find_content_path(ocx: OcxRunner, pkg: PackageInfo) -> Path:
    """Return the content path for an installed package."""
    result = ocx.json("find", pkg.short)
    return Path(result[pkg.short])


def _setup_leaf_and_app(ocx, unique_repo, tmp_path):
    """Common setup: push leaf + app with one dep (non-exported), install app."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"
    leaf = _push_leaf(ocx, leaf_repo, tmp_path)
    dep = _dep_entry(ocx, leaf)
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[dep])
    ocx.json("install", "--select", app.short)
    return leaf, app


def _setup_leaf_and_app_public(ocx, unique_repo, tmp_path):
    """Common setup: push leaf + app with one public dep, install app."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"
    leaf = _push_leaf(ocx, leaf_repo, tmp_path)
    dep = _dep_entry(ocx, leaf, visibility="public")
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[dep])
    ocx.json("install", "--select", app.short)
    return leaf, app


def _setup_chain(ocx, unique_repo, tmp_path):
    """Common setup: push C -> B -> A chain, install A."""
    c_repo = f"{unique_repo}_c"
    b_repo = f"{unique_repo}_b"
    a_repo = f"{unique_repo}_a"
    c = _push_leaf(ocx, c_repo, tmp_path)
    b = _push_with_deps(ocx, b_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, c)])
    a = _push_with_deps(ocx, a_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, b)])
    ocx.json("install", "--select", a.short)
    return c, b, a


def _setup_diamond(ocx, unique_repo, tmp_path):
    """Common setup: push D, B->D, C->D, A->{B,C}, install A."""
    d_repo = f"{unique_repo}_d"
    b_repo = f"{unique_repo}_b"
    c_repo = f"{unique_repo}_c"
    a_repo = f"{unique_repo}_a"
    d = _push_leaf(ocx, d_repo, tmp_path)
    b = _push_with_deps(ocx, b_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, d)])
    c = _push_with_deps(ocx, c_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, d)])
    a = _push_with_deps(
        ocx, a_repo, "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b), _dep_entry(ocx, c)],
    )
    ocx.json("install", "--select", a.short)
    return d, b, c, a


def _setup_diamond_public(ocx, unique_repo, tmp_path):
    """Common setup: push D, B->D, C->D, A->{B,C}, all public, install A."""
    d_repo = f"{unique_repo}_d"
    b_repo = f"{unique_repo}_b"
    c_repo = f"{unique_repo}_c"
    a_repo = f"{unique_repo}_a"
    d = _push_leaf(ocx, d_repo, tmp_path)
    b = _push_with_deps(ocx, b_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, d, visibility="public")])
    c = _push_with_deps(ocx, c_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, d, visibility="public")])
    a = _push_with_deps(
        ocx, a_repo, "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="public"), _dep_entry(ocx, c, visibility="public")],
    )
    ocx.json("install", "--select", a.short)
    return d, b, c, a


# ---------------------------------------------------------------------------
# Tests: ocx deps — tree view
# ---------------------------------------------------------------------------


def test_deps_tree_shows_hierarchy(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Default deps output (JSON) has root with nested dependency."""
    leaf, app = _setup_leaf_and_app(ocx, unique_repo, tmp_path)

    result = ocx.json("deps", app.short)
    roots = result["roots"]
    assert len(roots) == 1

    root_node = roots[0]
    assert app.fq in root_node["identifier"] or app.repo in root_node["identifier"]
    assert len(root_node["dependencies"]) == 1
    dep_node = root_node["dependencies"][0]
    assert leaf.repo in dep_node["identifier"]


def test_deps_tree_leaf_has_empty_deps(published_package: PackageInfo, ocx: OcxRunner):
    """Package with no deps shows single node with empty dependencies."""
    ocx.json("install", "--select", published_package.short)
    result = ocx.json("deps", published_package.short)
    roots = result["roots"]
    assert len(roots) == 1
    assert roots[0].get("dependencies", []) == []


def test_deps_tree_transitive_chain(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A->B->C: tree nests correctly to depth 2."""
    c, b, a = _setup_chain(ocx, unique_repo, tmp_path)

    result = ocx.json("deps", a.short)
    root = result["roots"][0]
    assert a.repo in root["identifier"]

    # root -> B
    assert len(root["dependencies"]) == 1
    b_node = root["dependencies"][0]
    assert b.repo in b_node["identifier"]

    # B -> C
    assert len(b_node["dependencies"]) == 1
    c_node = b_node["dependencies"][0]
    assert c.repo in c_node["identifier"]
    assert c_node.get("dependencies", []) == []


def test_deps_tree_diamond_marks_repeated(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A->{B,C}->D: second D occurrence has repeated: true."""
    d, b, c, a = _setup_diamond(ocx, unique_repo, tmp_path)

    result = ocx.json("deps", a.short)
    root = result["roots"][0]

    # Collect all D nodes across the tree
    d_nodes = []
    for child in root["dependencies"]:
        for grandchild in child.get("dependencies", []):
            if d.repo in grandchild["identifier"]:
                d_nodes.append(grandchild)

    assert len(d_nodes) == 2, f"expected D to appear twice in diamond, got {len(d_nodes)}"
    repeated_flags = [n.get("repeated") for n in d_nodes]
    assert True in repeated_flags, "expected one D node to be marked as repeated"


# ---------------------------------------------------------------------------
# Tests: ocx deps — flat view
# ---------------------------------------------------------------------------


def test_deps_flat_evaluation_order(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """--flat JSON shows leaf before root (dependency-first)."""
    leaf, app = _setup_leaf_and_app(ocx, unique_repo, tmp_path)

    result = ocx.json("deps", "--flat", app.short)
    entries = result["entries"]
    identifiers = [e["identifier"] for e in entries]

    leaf_idx = next(i for i, ident in enumerate(identifiers) if leaf.repo in ident)
    app_idx = next(i for i, ident in enumerate(identifiers) if app.repo in ident)
    assert leaf_idx < app_idx, f"leaf ({leaf_idx}) should come before app ({app_idx})"


def test_deps_flat_transitive_chain(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A->B->C --flat order is C, B, A."""
    c, b, a = _setup_chain(ocx, unique_repo, tmp_path)

    result = ocx.json("deps", "--flat", a.short)
    identifiers = [e["identifier"] for e in result["entries"]]

    c_idx = next(i for i, ident in enumerate(identifiers) if c.repo in ident)
    b_idx = next(i for i, ident in enumerate(identifiers) if b.repo in ident)
    a_idx = next(i for i, ident in enumerate(identifiers) if a.repo in ident)
    assert c_idx < b_idx < a_idx, f"expected C < B < A, got C={c_idx} B={b_idx} A={a_idx}"


# ---------------------------------------------------------------------------
# Tests: ocx deps — why view
# ---------------------------------------------------------------------------


def test_deps_why_traces_path(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """--why leaf returns a path containing root and leaf."""
    leaf, app = _setup_leaf_and_app(ocx, unique_repo, tmp_path)

    result = ocx.json("deps", "--why", leaf.fq, app.short)
    paths = result["paths"]
    assert len(paths) >= 1, "expected at least one path"
    # Each path is a list of identifier strings
    path_str = " ".join(paths[0])
    assert app.repo in path_str
    assert leaf.repo in path_str


def test_deps_why_diamond_multiple_paths(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A->{B,C}->D --why D returns 2 paths."""
    d, b, c, a = _setup_diamond(ocx, unique_repo, tmp_path)

    result = ocx.json("deps", "--why", d.fq, a.short)
    paths = result["paths"]
    assert len(paths) == 2, f"expected 2 paths through diamond, got {len(paths)}"


def test_deps_why_not_found_returns_error(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """--why nonexistent returns non-zero exit with error message."""
    leaf, app = _setup_leaf_and_app(ocx, unique_repo, tmp_path)

    fake_fq = f"{ocx.registry}/{unique_repo}_nonexistent:1.0.0"
    result = ocx.run("deps", "--why", fake_fq, app.short, check=False)
    assert result.returncode != 0
    assert "is not a dependency of" in result.stdout


# ---------------------------------------------------------------------------
# Tests: ocx deps — error cases
# ---------------------------------------------------------------------------


def test_deps_not_installed_fails(ocx: OcxRunner, unique_repo: str):
    """deps on uninstalled package returns non-zero exit."""
    result = ocx.run("deps", f"{unique_repo}_missing:1.0.0", check=False)
    assert result.returncode != 0


# ---------------------------------------------------------------------------
# Tests: Filesystem side effects — dependency refs
# ---------------------------------------------------------------------------


def test_dependency_forward_refs_created(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """After install, the dependent's deps/ dir contains a forward-ref symlink."""
    leaf, app = _setup_leaf_and_app(ocx, unique_repo, tmp_path)

    app_content = _find_content_path(ocx, app)
    app_obj_dir = app_content.parent
    deps_dir = app_obj_dir / "refs" / "deps"
    assert deps_dir.exists(), "refs/deps/ directory should exist on dependent object"

    dep_entries = list(deps_dir.iterdir())
    assert len(dep_entries) >= 1, f"expected at least 1 forward-ref in refs/deps/, got {len(dep_entries)}"
    # At least one should be a symlink
    assert any(e.is_symlink() for e in dep_entries), "expected symlink entries in refs/deps/"


def test_reinstall_restores_dependency_refs(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Uninstall+purge then reinstall recreates dependency refs."""
    leaf, app = _setup_leaf_and_app(ocx, unique_repo, tmp_path)

    # Purge everything
    ocx.plain("uninstall", "--purge", "-d", app.short)
    ocx.json("clean")
    assert _count_object_dirs(ocx) == 0

    # Reinstall
    ocx.json("install", "--select", app.short)
    assert _count_object_dirs(ocx) == 2

    # Verify forward refs restored on app
    app_content = _find_content_path(ocx, app)
    deps_dir = app_content.parent / "refs" / "deps"
    assert deps_dir.exists(), "refs/deps/ should be restored after reinstall"
    assert any(e.is_symlink() for e in deps_dir.iterdir()), "forward-ref symlinks should be restored"


# ---------------------------------------------------------------------------
# Tests: Clean/GC edge cases with dependencies
# ---------------------------------------------------------------------------


def test_clean_dry_run_reports_without_removing(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """clean --dry-run after uninstall lists collectible objects without removing."""
    leaf, app = _setup_leaf_and_app(ocx, unique_repo, tmp_path)
    # Uninstall without --purge to leave orphaned objects for clean to find.
    ocx.plain("uninstall", "-d", app.short)

    before_count = _count_object_dirs(ocx)
    result = ocx.json("clean", "--dry-run")
    after_count = _count_object_dirs(ocx)

    assert after_count == before_count, "dry-run should not remove any objects"
    assert len(result) >= 1, f"dry-run should report collectible objects; got {result}"


def test_clean_preserves_shared_dependency(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A->D, B->D; uninstall+purge A; clean does NOT remove D (B still refs it)."""
    d_repo = f"{unique_repo}_d"
    a_repo = f"{unique_repo}_a"
    b_repo = f"{unique_repo}_b"

    d = _push_leaf(ocx, d_repo, tmp_path)
    d_dep = _dep_entry(ocx, d)
    a = _push_with_deps(ocx, a_repo, "1.0.0", tmp_path, deps=[d_dep])
    b = _push_with_deps(ocx, b_repo, "1.0.0", tmp_path, deps=[d_dep])

    ocx.json("install", "--select", a.short)
    ocx.json("install", "--select", b.short)
    assert _count_object_dirs(ocx) == 3  # A, B, D

    # Uninstall+purge only A
    ocx.plain("uninstall", "--purge", "-d", a.short)
    ocx.json("clean")

    # D should still be present (B depends on it), A should be gone
    assert _count_object_dirs(ocx) == 2  # B + D remain


# ---------------------------------------------------------------------------
# Tests: Exec/env integration with dependencies
# ---------------------------------------------------------------------------


def test_exec_with_deps_includes_dep_env(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """exec app -- env output includes env var from exported leaf dependency."""
    leaf, app = _setup_leaf_and_app_public(ocx, unique_repo, tmp_path)

    result = ocx.plain("exec", app.short, "--", "env")
    leaf_home_key = leaf.repo.upper().replace("-", "_") + "_HOME"
    assert leaf_home_key in result.stdout, (
        f"expected {leaf_home_key!r} in exec env output"
    )


def test_env_dependency_order_deps_first(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """env JSON output shows exported leaf dep vars before dependent vars."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = _push_leaf(ocx, leaf_repo, tmp_path)
    dep = _dep_entry(ocx, leaf, visibility="public")
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[dep])
    ocx.json("install", "--select", app.short)

    env_result = ocx.json("env", app.short)
    keys = [e["key"] for e in env_result["entries"]]

    leaf_home_key = leaf_repo.upper().replace("-", "_") + "_HOME"
    app_home_key = app_repo.upper().replace("-", "_") + "_HOME"

    assert leaf_home_key in keys, f"missing leaf key {leaf_home_key} in {keys}"
    assert app_home_key in keys, f"missing app key {app_home_key} in {keys}"

    leaf_idx = keys.index(leaf_home_key)
    app_idx = keys.index(app_home_key)
    assert leaf_idx < app_idx, (
        f"leaf env var ({leaf_idx}) should come before app env var ({app_idx})"
    )


# ---------------------------------------------------------------------------
# Tests: ocx deps — depth flag (W10)
# ---------------------------------------------------------------------------


def test_deps_tree_depth_limits_nesting(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """--depth 1 shows direct deps (B) but not transitive deps (C) for A->B->C."""
    c, b, a = _setup_chain(ocx, unique_repo, tmp_path)

    result = ocx.json("deps", "--depth", "1", a.short)
    root = result["roots"][0]

    # Root should show B as a direct dependency
    assert len(root["dependencies"]) == 1, (
        f"expected root to have 1 dependency at depth=1, got {len(root['dependencies'])}"
    )
    b_node = root["dependencies"][0]
    assert b.repo in b_node["identifier"], (
        f"expected B in first dep identifier; got {b_node['identifier']!r}"
    )

    # B's dependencies list should be empty — C is cut off by depth limit
    b_deps = b_node.get("dependencies", [])
    assert b_deps == [], (
        f"expected B's dependencies to be empty at depth=1, but got {b_deps}"
    )


# ---------------------------------------------------------------------------
# Tests: ocx deps --flat conflict detection (W11)
# ---------------------------------------------------------------------------


def test_deps_flat_conflicting_digests_reports_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """--flat with two roots depending on exported different digests of the same repo warns."""
    d_repo = f"{unique_repo}_d"
    a_repo = f"{unique_repo}_a"
    b_repo = f"{unique_repo}_b"

    # Push two different versions of D to the same repo
    d_v1 = make_package(ocx, d_repo, "1.0.0", tmp_path, new=True)
    d_v2 = make_package(ocx, d_repo, "2.0.0", tmp_path, new=False)

    # A depends on D v1 (exported), B depends on D v2 (exported) — conflicting digests
    a = _push_with_deps(ocx, a_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, d_v1, visibility="public")])
    b = _push_with_deps(ocx, b_repo, "1.0.0", tmp_path, deps=[_dep_entry(ocx, d_v2, visibility="public")])

    ocx.json("install", "--select", a.short)
    ocx.json("install", "--select", b.short)

    result = ocx.run("deps", "--flat", a.short, b.short, check=False)
    assert result.returncode == 0, (
        f"conflicting digests should warn, not error; got rc={result.returncode}: {result.stderr!r}"
    )
    assert "conflicting" in result.stderr.lower(), (
        f"expected 'conflicting' warning in stderr; got: {result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# Tests: Transitive dependency edge cases
# ---------------------------------------------------------------------------


def test_shell_env_includes_transitive_dep_vars(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """shell env on A->B (exported) should include B's env vars as shell exports."""
    leaf = _push_leaf(ocx, f"{unique_repo}_leaf", tmp_path)
    app = _push_with_deps(
        ocx, f"{unique_repo}_app", "1.0.0", tmp_path, deps=[_dep_entry(ocx, leaf, visibility="public")]
    )
    ocx.json("install", "--select", app.short)

    result = ocx.plain("shell", "env", app.short)
    assert result.returncode == 0

    leaf_home_key = f"{unique_repo}_leaf".upper().replace("-", "_") + "_HOME"
    assert leaf_home_key in result.stdout, (
        f"expected transitive dep env var {leaf_home_key!r} in shell env output"
    )


def test_exec_diamond_transitive_dep_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """exec on A->{B,C}->D (all exported) sees D's env vars through the diamond."""
    d, b, c, a = _setup_diamond_public(ocx, unique_repo, tmp_path)

    result = ocx.plain("exec", a.short, "--", "env")
    assert result.returncode == 0

    d_home_key = f"{unique_repo}_d".upper().replace("-", "_") + "_HOME"
    assert d_home_key in result.stdout, (
        f"expected diamond transitive dep env var {d_home_key!r} in exec output"
    )


def test_transitive_ref_chain_integrity(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B->C: verify deps/ forward-refs at every level of the chain."""
    c, b, a = _setup_chain(ocx, unique_repo, tmp_path)

    a_content = _find_content_path(ocx, a)
    a_obj = a_content.parent

    # A's deps/ should have exactly 1 entry (pointing to B's content).
    a_deps = _list_dep_targets(a_obj)
    assert len(a_deps) == 1, f"expected 1 dep for A, got {len(a_deps)}"
    b_content = a_deps[0]
    b_obj = b_content.parent

    # B's deps/ should have exactly 1 entry (pointing to C's content).
    b_deps = _list_dep_targets(b_obj)
    assert len(b_deps) == 1, f"expected 1 dep for B, got {len(b_deps)}"
    c_content = b_deps[0]
    c_obj = c_content.parent

    # C should be a leaf — no deps/ entries.
    c_deps = _list_dep_targets(c_obj)
    assert len(c_deps) == 0, f"expected 0 deps for C (leaf), got {len(c_deps)}"


def test_deep_conflict_at_depth_two(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B->D v1 and A->C->D v2 (all exported): conflict warned at transitive depth 2."""
    d_repo = f"{unique_repo}_d"
    d_v1 = make_package(ocx, d_repo, "1.0.0", tmp_path, new=True)
    d_v2 = make_package(ocx, d_repo, "2.0.0", tmp_path, new=False)

    b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[_dep_entry(ocx, d_v1, visibility="public")]
    )
    c = _push_with_deps(
        ocx, f"{unique_repo}_c", "1.0.0", tmp_path, deps=[_dep_entry(ocx, d_v2, visibility="public")]
    )
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="public"), _dep_entry(ocx, c, visibility="public")],
    )

    ocx.json("install", "--select", a.short)

    result = ocx.run("deps", "--flat", a.short, check=False)
    assert result.returncode == 0, (
        f"conflicting digests should warn, not error; got rc={result.returncode}: {result.stderr!r}"
    )
    assert "conflicting" in result.stderr.lower(), (
        f"expected 'conflicting' warning in stderr; got: {result.stderr!r}"
    )


def test_clean_cascades_transitive_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Uninstall A from A->B->C chain, clean removes all three."""
    c, b, a = _setup_chain(ocx, unique_repo, tmp_path)
    assert _count_object_dirs(ocx) == 3

    ocx.json("uninstall", "--purge", "-d", a.short)
    ocx.json("clean")

    assert _count_object_dirs(ocx) == 0, (
        "expected all transitive deps removed after uninstall+clean"
    )


def test_clean_preserves_shared_transitive_dep(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B->D and C->D: uninstall A, clean should preserve D (still needed by C)."""
    d = _push_leaf(ocx, f"{unique_repo}_d", tmp_path)
    b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[_dep_entry(ocx, d)]
    )
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[_dep_entry(ocx, b)]
    )
    c = _push_with_deps(
        ocx, f"{unique_repo}_c", "1.0.0", tmp_path, deps=[_dep_entry(ocx, d)]
    )

    ocx.json("install", "--select", a.short)
    ocx.json("install", "--select", c.short)
    assert _count_object_dirs(ocx) == 4  # A, B, C, D

    ocx.json("uninstall", "--purge", "-d", a.short)
    ocx.json("clean")

    # D and C should survive; A and B should be cleaned.
    assert _count_object_dirs(ocx) == 2, (
        "expected D preserved (needed by C) and B removed"
    )


def test_deps_multi_root_shared_transitive(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B->D and C->B->D: deps on both roots shows B and D once each."""
    d = _push_leaf(ocx, f"{unique_repo}_d", tmp_path)
    b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[_dep_entry(ocx, d)]
    )
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[_dep_entry(ocx, b)]
    )
    c = _push_with_deps(
        ocx, f"{unique_repo}_c", "1.0.0", tmp_path, deps=[_dep_entry(ocx, b)]
    )

    ocx.json("install", "--select", a.short)
    ocx.json("install", "--select", c.short)

    result = ocx.json("deps", "--flat", a.short, c.short)
    ids = [e["identifier"] for e in result["entries"]]

    # D and B should appear exactly once each (deduped across roots).
    d_count = sum(1 for x in ids if f"{unique_repo}_d" in x)
    b_count = sum(1 for x in ids if f"{unique_repo}_b" in x)
    assert d_count == 1, f"expected D once in flat output, got {d_count}"
    assert b_count == 1, f"expected B once in flat output, got {b_count}"


# ---------------------------------------------------------------------------
# Tests: GC with transitive dependencies — object store verification
# ---------------------------------------------------------------------------


def test_clean_protects_transitive_chain_while_installed(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B->C: clean while A is installed preserves all three objects."""
    c, b, a = _setup_chain(ocx, unique_repo, tmp_path)
    assert _count_object_dirs(ocx) == 3

    ocx.json("clean")

    assert _count_object_dirs(ocx) == 3, (
        "clean should not remove any objects while root is installed"
    )


def test_clean_diamond_cascade_removes_all(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->{B,C}->D: uninstall A, clean removes all four objects."""
    d, b, c, a = _setup_diamond(ocx, unique_repo, tmp_path)
    assert _count_object_dirs(ocx) == 4

    ocx.plain("uninstall", "--purge", "-d", a.short)
    ocx.json("clean")

    assert _count_object_dirs(ocx) == 0, (
        "expected all 4 diamond objects removed after uninstall+clean"
    )


def test_clean_dry_run_transitive_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B->C: uninstall A, dry-run reports all three as collectible."""
    c, b, a = _setup_chain(ocx, unique_repo, tmp_path)

    # Uninstall without --purge to leave all three objects as orphans.
    ocx.plain("uninstall", "-d", a.short)

    before = _count_object_dirs(ocx)
    result = ocx.json("clean", "--dry-run")
    after = _count_object_dirs(ocx)

    assert after == before, "dry-run must not remove objects"
    # Post-#35 blobs are first-class GC participants, so the dry-run reports
    # entries across all three CAS tiers. Split by path-prefix and assert
    # per-tier counts so the test stays robust to changes in the per-package
    # blob shape (single-platform vs image-index).
    pkg_paths = [e for e in result if "/packages/" in e["path"]]
    layer_paths = [e for e in result if "/layers/" in e["path"]]
    blob_paths = [e for e in result if "/blobs/" in e["path"]]
    assert len(pkg_paths) == 3, f"expected 3 collectible package entries; got {len(pkg_paths)}"
    assert len(layer_paths) == 3, f"expected 3 collectible layer entries; got {len(layer_paths)}"
    assert len(blob_paths) >= 3, (
        f"expected at least 3 collectible blob entries (one manifest per package); got {len(blob_paths)}"
    )


def test_clean_partial_diamond_preserves_shared_leaf(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->{B,C}->D and E->D: uninstall A, clean removes A+B+C but preserves D (E refs it)."""
    d = _push_leaf(ocx, f"{unique_repo}_d", tmp_path)
    d_dep = _dep_entry(ocx, d)

    b = _push_with_deps(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[d_dep])
    c = _push_with_deps(ocx, f"{unique_repo}_c", "1.0.0", tmp_path, deps=[d_dep])
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b), _dep_entry(ocx, c)],
    )
    e = _push_with_deps(ocx, f"{unique_repo}_e", "1.0.0", tmp_path, deps=[d_dep])

    ocx.json("install", "--select", a.short)
    ocx.json("install", "--select", e.short)
    assert _count_object_dirs(ocx) == 5  # A, B, C, D, E

    ocx.plain("uninstall", "--purge", "-d", a.short)
    ocx.json("clean")

    # D survives (E still depends on it); A, B, C removed; E stays.
    assert _count_object_dirs(ocx) == 2, (
        "expected D+E preserved, A+B+C removed"
    )


def test_reinstall_after_clean_restores_transitive_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B->C: uninstall+clean wipes store; reinstall restores all three."""
    c, b, a = _setup_chain(ocx, unique_repo, tmp_path)
    assert _count_object_dirs(ocx) == 3

    ocx.plain("uninstall", "--purge", "-d", a.short)
    ocx.json("clean")
    assert _count_object_dirs(ocx) == 0

    # Reinstall — should pull the full transitive chain again.
    ocx.json("install", "--select", a.short)
    assert _count_object_dirs(ocx) == 3, (
        "reinstall should restore all three objects in the chain"
    )


def test_object_store_counts_at_each_step(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Track object count through install → clean → purge → clean for A->B->C."""
    c, b, a = _setup_chain(ocx, unique_repo, tmp_path)

    # After install: 3 objects (A, B, C)
    assert _count_object_dirs(ocx) == 3, "after install"

    # Clean while installed: nothing collected (all referenced)
    ocx.json("clean")
    assert _count_object_dirs(ocx) == 3, "after clean (all still referenced)"

    # Uninstall without purge: candidate symlink removed, objects + refs stay
    ocx.json("uninstall", a.short)
    assert _count_object_dirs(ocx) == 3, "after uninstall without purge"

    # Clean after non-purge uninstall: refs still intact, nothing collected
    ocx.json("clean")
    assert _count_object_dirs(ocx) == 3, "after clean (refs still intact)"

    # Reinstall, then purge+clean: full cascade
    ocx.json("install", "--select", a.short)
    ocx.plain("uninstall", "--purge", "-d", a.short)
    ocx.json("clean")
    assert _count_object_dirs(ocx) == 0, "after purge + clean"


# ---------------------------------------------------------------------------
# Tests: Symlink-resolved root deduplication with dependencies
# ---------------------------------------------------------------------------


def test_env_candidate_deduplicates_root_that_is_also_dependency(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """env --candidate app lib deduplicates lib when it's both a root and a dep of app.

    Regression: when lib is resolved via --candidate (symlink, no digest in
    identifier), it must be deduplicated against the same lib discovered as
    a transitive dependency of app (resolved with real digest). The identifier
    persisted in resolve.json enables this deduplication.
    """
    lib_repo = f"{unique_repo}_lib"
    app_repo = f"{unique_repo}_app"

    lib = _push_leaf(ocx, lib_repo, tmp_path)
    dep = _dep_entry(ocx, lib, visibility="public")
    app = _push_with_deps(ocx, app_repo, "1.0.0", tmp_path, deps=[dep])

    # Install both — app gets candidate symlink, lib gets candidate symlink.
    ocx.json("install", app.short)
    ocx.json("install", lib.short)

    # Request env for both via --candidate. lib is both a root (via symlink)
    # and an exported transitive dependency of app (via real digest).
    env_result = ocx.json("env", "--candidate", app.short, lib.short)

    lib_home_key = lib_repo.upper().replace("-", "_") + "_HOME"
    occurrences = [e for e in env_result["entries"] if e["key"] == lib_home_key]
    assert len(occurrences) == 1, (
        f"expected {lib_home_key!r} exactly once, got {len(occurrences)} times"
    )


# ---------------------------------------------------------------------------
# Tests: Export control
# ---------------------------------------------------------------------------


def test_sealed_suppresses_dep_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A depends on B (export: false): ocx env A must NOT contain B_HOME."""
    b = _push_leaf(ocx, f"{unique_repo}_b", tmp_path)
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="sealed")],
    )
    ocx.json("install", "--select", a.short)

    env_result = ocx.json("env", a.short)
    b_home_key = f"{unique_repo}_b".upper().replace("-", "_") + "_HOME"
    env_keys = [e["key"] for e in env_result["entries"]]
    assert b_home_key not in env_keys, (
        f"non-exported dep key {b_home_key!r} should NOT appear in env; got keys: {env_keys}"
    )


def test_public_includes_dep_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A depends on B (export: true): ocx env A MUST contain B_HOME."""
    b = _push_leaf(ocx, f"{unique_repo}_b", tmp_path)
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="public")],
    )
    ocx.json("install", "--select", a.short)

    env_result = ocx.json("env", a.short)
    b_home_key = f"{unique_repo}_b".upper().replace("-", "_") + "_HOME"
    env_keys = [e["key"] for e in env_result["entries"]]
    assert b_home_key in env_keys, (
        f"exported dep key {b_home_key!r} MUST appear in env; got keys: {env_keys}"
    )


def test_sealed_conflicting_deps_coexist(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A depends on D v1 (non-exported), B depends on D v2 (non-exported): env A B succeeds."""
    d_repo = f"{unique_repo}_d"
    d_v1 = make_package(ocx, d_repo, "1.0.0", tmp_path, new=True)
    d_v2 = make_package(ocx, d_repo, "2.0.0", tmp_path, new=False)

    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, d_v1, visibility="sealed")],
    )
    b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, d_v2, visibility="sealed")],
    )

    ocx.json("install", "--select", a.short)
    ocx.json("install", "--select", b.short)

    # Should succeed — non-exported conflicting deps don't trigger errors.
    result = ocx.run("env", a.short, b.short, check=False)
    assert result.returncode == 0, (
        f"non-exported conflicting deps should not error; stderr: {result.stderr!r}"
    )


def test_public_conflicting_deps_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A depends on D v1 (exported), B depends on D v2 (exported): env A B warns."""
    d_repo = f"{unique_repo}_d"
    d_v1 = make_package(ocx, d_repo, "1.0.0", tmp_path, new=True)
    d_v2 = make_package(ocx, d_repo, "2.0.0", tmp_path, new=False)

    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, d_v1, visibility="public")],
    )
    b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, d_v2, visibility="public")],
    )

    ocx.json("install", "--select", a.short)
    ocx.json("install", "--select", b.short)

    result = ocx.run("env", a.short, b.short, check=False)
    assert result.returncode == 0, (
        f"conflicting digests should warn, not error; got rc={result.returncode}: {result.stderr!r}"
    )
    assert "conflicting" in result.stderr.lower(), (
        f"expected 'conflicting' warning in stderr; got: {result.stderr!r}"
    )


def test_transitive_public_propagates(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B(export:true)->C(export:true): ocx env A contains C's env."""
    c = _push_leaf(ocx, f"{unique_repo}_c", tmp_path)
    b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, c, visibility="public")],
    )
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="public")],
    )
    ocx.json("install", "--select", a.short)

    env_result = ocx.json("env", a.short)
    c_home_key = f"{unique_repo}_c".upper().replace("-", "_") + "_HOME"
    env_keys = [e["key"] for e in env_result["entries"]]
    assert c_home_key in env_keys, (
        f"transitively exported dep key {c_home_key!r} MUST appear in env; got keys: {env_keys}"
    )


def test_sealed_blocks_transitive_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A->B(export:false)->C(export:true): ocx env A does NOT contain C's env."""
    c = _push_leaf(ocx, f"{unique_repo}_c", tmp_path)
    b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, c, visibility="public")],
    )
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="sealed")],
    )
    ocx.json("install", "--select", a.short)

    env_result = ocx.json("env", a.short)
    c_home_key = f"{unique_repo}_c".upper().replace("-", "_") + "_HOME"
    env_keys = [e["key"] for e in env_result["entries"]]
    assert c_home_key not in env_keys, (
        f"blocked transitive dep key {c_home_key!r} should NOT appear in env; got keys: {env_keys}"
    )


def test_gc_protects_sealed_dep(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A depends on B (export: false). Clean does not collect B while A is installed."""
    b = _push_leaf(ocx, f"{unique_repo}_b", tmp_path)
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="sealed")],
    )
    ocx.json("install", "--select", a.short)
    assert _count_object_dirs(ocx) == 2  # A + B

    ocx.json("clean")

    assert _count_object_dirs(ocx) == 2, (
        "non-exported dep B should be protected from GC while A is installed"
    )


# ---------------------------------------------------------------------------
# Tests: Visibility levels (private, interface)
# ---------------------------------------------------------------------------


def test_private_includes_dep_env_for_direct_target(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path,
):
    """A depends on B (visibility: private): ocx env A MUST contain B_HOME.

    When A is the direct exec/env target, its private deps are self-visible
    and should contribute to the environment.
    """
    b_home_key = f"{unique_repo}_B_HOME".upper().replace("-", "_")
    b = _push_leaf(ocx, f"{unique_repo}_b", tmp_path, env=[
        {"key": b_home_key, "type": "constant", "value": "${installPath}"}
    ])
    _push_with_deps(
        ocx, f"{unique_repo}_app", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="private")],
    )
    ocx.json("install", "--select", f"{unique_repo}_app:1.0.0")

    env_result = ocx.json("env", f"{unique_repo}_app:1.0.0")
    env_keys = [e["key"] for e in env_result["entries"]]
    assert b_home_key in env_keys, (
        f"private dep key {b_home_key!r} MUST appear in env for direct target; got keys: {env_keys}"
    )


def test_private_suppresses_dep_env_for_consumer(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path,
):
    """Root→(public)→A→(private)→B: ocx env Root must NOT contain B_HOME.

    B is private to A — when Root consumes A publicly, B's env should not
    leak to Root because Private doesn't export (propagation: Sealed).
    """
    b_home_key = f"{unique_repo}_B_HOME".upper().replace("-", "_")
    b = _push_leaf(ocx, f"{unique_repo}_b", tmp_path, env=[
        {"key": b_home_key, "type": "constant", "value": "${installPath}"}
    ])
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="private")],
    )
    _push_with_deps(
        ocx, f"{unique_repo}_root", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, a, visibility="public")],
    )
    ocx.json("install", "--select", f"{unique_repo}_root:1.0.0")

    env_result = ocx.json("env", f"{unique_repo}_root:1.0.0")
    env_keys = [e["key"] for e in env_result["entries"]]
    assert b_home_key not in env_keys, (
        f"private transitive dep key {b_home_key!r} should NOT appear for consumer; got keys: {env_keys}"
    )


def test_interface_includes_dep_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path,
):
    """A depends on B (visibility: interface): ocx env A MUST contain B_HOME.

    Interface behaves like public for consumer env resolution. This test locks
    in that behavior.
    """
    b_home_key = f"{unique_repo}_B_HOME".upper().replace("-", "_")
    b = _push_leaf(ocx, f"{unique_repo}_b", tmp_path, env=[
        {"key": b_home_key, "type": "constant", "value": "${installPath}"}
    ])
    _push_with_deps(
        ocx, f"{unique_repo}_app", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="interface")],
    )
    ocx.json("install", "--select", f"{unique_repo}_app:1.0.0")

    env_result = ocx.json("env", f"{unique_repo}_app:1.0.0")
    env_keys = [e["key"] for e in env_result["entries"]]
    assert b_home_key in env_keys, (
        f"interface dep key {b_home_key!r} MUST appear in env; got keys: {env_keys}"
    )


def test_deps_flat_shows_visibility_column(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path,
):
    """--flat JSON shows visibility field with correct values."""
    leaf = _push_leaf(ocx, f"{unique_repo}_leaf", tmp_path)
    _push_with_deps(
        ocx, f"{unique_repo}_app", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, leaf, visibility="public")],
    )
    ocx.run("install", "--select", f"{unique_repo}_app:1.0.0")

    result = ocx.json("deps", "--flat", f"{unique_repo}_app:1.0.0")
    entries = result["entries"]
    leaf_entry = next(e for e in entries if f"{unique_repo}_leaf" in e["identifier"])
    app_entry = next(e for e in entries if f"{unique_repo}_app" in e["identifier"])

    assert leaf_entry["visibility"] == "public", (
        f"public dep should show 'public', got {leaf_entry['visibility']!r}"
    )
    assert app_entry["visibility"] == "public", (
        f"root package should show 'public', got {app_entry['visibility']!r}"
    )


# ---------------------------------------------------------------------------
# Tests: Diamond deps/ forward-ref symlinks
# ---------------------------------------------------------------------------


def test_diamond_intermediate_deps_forward_refs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path,
):
    """A->{B,C}->D: both B and C should have deps/ symlinks pointing to D."""
    d, b, c, a = _setup_diamond(ocx, unique_repo, tmp_path)

    reg_slug = registry_dir(ocx.registry)
    b_obj = _find_object_dir(ocx, reg_slug, f"{unique_repo}_b")
    c_obj = _find_object_dir(ocx, reg_slug, f"{unique_repo}_c")

    b_deps = _list_dep_targets(b_obj)
    c_deps = _list_dep_targets(c_obj)

    assert len(b_deps) >= 1, f"B should have deps/ symlink to D, got {b_deps}"
    assert len(c_deps) >= 1, f"C should have deps/ symlink to D, got {c_deps}"


# ---------------------------------------------------------------------------
# Helpers for ref-chain inspection
# ---------------------------------------------------------------------------


def _find_object_dir(ocx: OcxRunner, reg_slug: str, repo: str) -> Path:
    """Find the single package directory for a given repo in the package store.

    Package dirs are sharded by registry + digest only (no repo in the path),
    so ``find`` is the authoritative way to resolve a repo name to a content
    path. For transitive deps (not installed directly and thus without an
    install symlink) we fall back to the bare repo name so ``ocx find``
    resolves via the local index.
    """
    find_result = ocx.json("find", repo)
    key = next(iter(find_result))
    content_path = Path(find_result[key]).resolve()
    assert content_path.name == "content", f"unexpected find target: {content_path}"
    return content_path.parent


def _list_dep_targets(obj_dir: Path) -> list[Path]:
    """Read all refs/deps/ symlinks to their target content directories."""
    deps_dir = obj_dir / "refs" / "deps"
    if not deps_dir.exists():
        return []
    return sorted(entry.resolve() for entry in deps_dir.iterdir() if entry.is_symlink())


# ---------------------------------------------------------------------------
# Tests: Entrypoint visibility in dependency env
# ---------------------------------------------------------------------------


def test_public_dep_entrypoints_appear_in_consumer_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A's env must include B's entrypoints/ in PATH when B is a public dependency.

    The visible-package pipeline emits a synthetic `PATH ⊳ <B_pkg_root>/entrypoints`
    for every visible package that has non-empty entrypoints.  When A depends on B
    publicly, B is visible to A — so B's entrypoints/ must appear in `ocx env A`.
    """
    b_repo = f"{unique_repo}_b"
    a_repo = f"{unique_repo}_a"

    pkg_b = make_package_with_entrypoints(
        ocx,
        b_repo,
        tmp_path,
        entrypoints=[{"name": "cmake", "target": "${installPath}/bin/cmake"}],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="b",
    )

    dep_digest = fetch_manifest_digest(ocx.registry, b_repo, "1.0.0")
    dep_entry = {
        "identifier": f"{pkg_b.fq}@{dep_digest}",
        "visibility": "public",
    }
    pkg_a = make_package(ocx, a_repo, "1.0.0", tmp_path, dependencies=[dep_entry])
    ocx.plain("install", "--select", pkg_a.short)

    env_result = ocx.json("env", pkg_a.short)
    path_values = [e["value"] for e in env_result["entries"] if e["key"] == "PATH"]

    assert any("entrypoints" in v for v in path_values), (
        f"expected B's entrypoints/ in PATH for public dep; PATH values: {path_values}"
    )


def test_sealed_dep_entrypoints_excluded_from_consumer_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A's env must NOT include B's entrypoints/ in PATH when B is a sealed dependency.

    Sealed (non-exported) dependencies are not visible to A's consumers — the
    synthetic entrypoints/ PATH entry for B must not appear in `ocx env A`.
    """
    b_repo = f"{unique_repo}_b"
    a_repo = f"{unique_repo}_a"

    pkg_b = make_package_with_entrypoints(
        ocx,
        b_repo,
        tmp_path,
        entrypoints=[{"name": "cmake", "target": "${installPath}/bin/cmake"}],
        bins=["cmake"],
        tag="1.0.0",
        file_prefix="b",
    )

    dep_digest = fetch_manifest_digest(ocx.registry, b_repo, "1.0.0")
    dep_entry = {
        "identifier": f"{pkg_b.fq}@{dep_digest}",
        "visibility": "sealed",
    }
    pkg_a = make_package(ocx, a_repo, "1.0.0", tmp_path, dependencies=[dep_entry])
    ocx.plain("install", "--select", pkg_a.short)

    env_result = ocx.json("env", pkg_a.short)
    path_values = [e["value"] for e in env_result["entries"] if e["key"] == "PATH"]

    # B's entrypoints/ dir must not appear in PATH for A (sealed dep not exported).
    b_find = ocx.json("find", pkg_b.short)
    b_content_path = next(iter(b_find.values()))
    b_pkg_root = str(Path(b_content_path).parent)

    assert not any(v.startswith(b_pkg_root) and "entrypoints" in v for v in path_values), (
        f"sealed dep B's entrypoints/ must NOT appear in consumer PATH; PATH values: {path_values}"
    )
