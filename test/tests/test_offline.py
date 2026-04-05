from pathlib import Path

from src import OcxRunner, PackageInfo
from src.helpers import make_package
from src.registry import fetch_manifest_digest


def test_find_fails_offline_when_not_installed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx --offline find <pkg>  (not installed, expects failure)"""
    pkg = published_package
    result = ocx.plain("--offline", "find", pkg.short, check=False)
    assert result.returncode != 0


def test_find_succeeds_offline_when_installed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx --offline find <pkg>"""
    pkg = published_package
    ocx.plain("install", pkg.short)

    result = ocx.plain("--offline", "find", pkg.short)
    assert result.returncode == 0


def test_exec_works_offline_when_installed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx --offline exec <pkg> -- hello"""
    pkg = published_package
    ocx.plain("install", pkg.short)

    result = ocx.plain("--offline", "exec", pkg.short, "--", "hello")
    assert result.stdout.strip() == pkg.marker


# ---------------------------------------------------------------------------
# Helpers for dependency tests
# ---------------------------------------------------------------------------


def _push_leaf(ocx: OcxRunner, repo: str, tmp_path: Path, **kwargs) -> PackageInfo:
    return make_package(ocx, repo, "1.0.0", tmp_path, new=True, **kwargs)


def _dep_entry(ocx: OcxRunner, pkg: PackageInfo, *, visibility: str | None = None) -> dict:
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    entry: dict = {"identifier": f"{pkg.fq}@{digest}"}
    if visibility is not None:
        entry["visibility"] = visibility
    return entry


def _push_with_deps(
    ocx: OcxRunner, repo: str, tag: str, tmp_path: Path, deps: list[dict], **kwargs,
) -> PackageInfo:
    return make_package(ocx, repo, tag, tmp_path, new=True, dependencies=deps, **kwargs)


# ---------------------------------------------------------------------------
# Tests: Offline mode with transitive dependencies
# ---------------------------------------------------------------------------


def test_env_offline_includes_transitive_dep_vars(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Install A->B->C (all exported) online, then offline env on A includes C's env vars."""
    c = _push_leaf(ocx, f"{unique_repo}_c", tmp_path)
    b = _push_with_deps(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[_dep_entry(ocx, c, visibility="public")])
    a = _push_with_deps(ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[_dep_entry(ocx, b, visibility="public")])

    ocx.json("install", "--select", a.short)

    # Go offline — env should still resolve transitive dep vars from filesystem.
    env_result = ocx.run("--offline", "env", a.short)
    assert env_result.returncode == 0

    c_home_key = f"{unique_repo}_c".upper().replace("-", "_") + "_HOME"
    assert c_home_key in env_result.stdout, (
        f"expected transitive dep env var {c_home_key!r} in offline env output"
    )


def test_exec_offline_with_transitive_deps(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Install A->B (exported) online, then offline exec on A sees B's env vars."""
    leaf = _push_leaf(ocx, f"{unique_repo}_leaf", tmp_path)
    app = _push_with_deps(
        ocx, f"{unique_repo}_app", "1.0.0", tmp_path, deps=[_dep_entry(ocx, leaf, visibility="public")]
    )

    ocx.json("install", "--select", app.short)

    # Offline exec — dependency env vars should be in the subprocess environment.
    result = ocx.plain("--offline", "exec", app.short, "--", "env")
    assert result.returncode == 0

    leaf_home_key = f"{unique_repo}_leaf".upper().replace("-", "_") + "_HOME"
    assert leaf_home_key in result.stdout, (
        f"expected dep env var {leaf_home_key!r} in offline exec output"
    )


def test_deps_offline_shows_transitive_tree(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Install A->B->C online, then offline deps shows full transitive tree."""
    c = _push_leaf(ocx, f"{unique_repo}_c", tmp_path)
    b = _push_with_deps(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[_dep_entry(ocx, c)])
    a = _push_with_deps(ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[_dep_entry(ocx, b)])

    ocx.json("install", "--select", a.short)

    # Offline deps — graph is built from filesystem, not index.
    result = ocx.run("--offline", "deps", a.short)
    assert result.returncode == 0

    # Verify all three packages appear in output.
    assert f"{unique_repo}_a" in result.stdout
    assert f"{unique_repo}_b" in result.stdout
    assert f"{unique_repo}_c" in result.stdout


def test_deps_flat_offline_shows_topological_order(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Install A->B->C online, then offline deps --flat shows C, B, A order."""
    c = _push_leaf(ocx, f"{unique_repo}_c", tmp_path)
    b = _push_with_deps(ocx, f"{unique_repo}_b", "1.0.0", tmp_path, deps=[_dep_entry(ocx, c)])
    a = _push_with_deps(ocx, f"{unique_repo}_a", "1.0.0", tmp_path, deps=[_dep_entry(ocx, b)])

    ocx.json("install", "--select", a.short)

    result = ocx.run("--offline", "deps", "--flat", a.short)
    assert result.returncode == 0

    # Parse JSON — entries should be in topological order: C, B, A.
    import json
    data = json.loads(result.stdout)
    ids = [e["identifier"] for e in data["entries"]]
    assert len(ids) == 3
    # C (leaf) must come before B, B before A.
    c_idx = next(i for i, x in enumerate(ids) if f"{unique_repo}_c" in x)
    b_idx = next(i for i, x in enumerate(ids) if f"{unique_repo}_b" in x)
    a_idx = next(i for i, x in enumerate(ids) if f"{unique_repo}_a" in x)
    assert c_idx < b_idx < a_idx, f"expected C < B < A order, got indices {c_idx}, {b_idx}, {a_idx}"
