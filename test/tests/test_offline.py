import subprocess
from pathlib import Path

import pytest

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
    b = _push_with_deps(
        ocx, f"{unique_repo}_b", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, c, visibility="public")],
    )
    a = _push_with_deps(
        ocx, f"{unique_repo}_a", "1.0.0", tmp_path,
        deps=[_dep_entry(ocx, b, visibility="public")],
    )

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


# ---------------------------------------------------------------------------
# Tests: Exit codes for offline mode (Phase 3 specification tests)
# NOTE: These tests assert exit code 81 (OfflineBlocked) which will only pass
# after Phase 4 implements classify_error dispatch in main.rs.
# ---------------------------------------------------------------------------


@pytest.mark.xfail(
    strict=True,
    reason="resolve path returns NotFound (79) before the offline check fires; "
    "routing `--offline + cache-miss` through OfflineMode requires a resolve-layer "
    "change that distinguishes 'never cached' from 'not in index'",
)
def test_exit_code_on_offline_blocks_fetch(ocx: OcxRunner) -> None:
    """--offline install of non-cached package → exit 81 (OfflineBlocked).

    Plan Test 3.2.6: ocx_lib::Error::OfflineMode → OfflineBlocked (81).
    The package has never been installed so no cached version exists — offline
    mode must block the fetch and exit with code 81 (distinct from Unavailable=69
    which signals a network fault rather than a deliberate policy block).
    """
    result = subprocess.run(
        [str(ocx.binary), "--offline", "install", "nonexistent_spec_test_pkg:0"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert result.returncode == 81, (
        f"offline fetch should exit with code 81 (OfflineBlocked), got {result.returncode}; "
        f"stderr={result.stderr!r}"
    )


def test_exit_code_on_auth_failure(ocx: OcxRunner) -> None:
    """Auth failure simulation deferred — registry fixture does not support 401 responses.

    Plan Test 3.2.7: AuthError → exit 80. The test registry (registry:2) cannot
    inject 401 responses without a custom auth plugin. Skipped until a fixture
    supporting auth failure simulation is available.
    """
    pytest.skip("auth failure simulation deferred; registry:2 fixture cannot inject 401 responses")
