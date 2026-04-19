"""Acceptance tests for ${deps.NAME.installPath} env var interpolation."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from src.helpers import make_package
from src.registry import fetch_manifest_digest
from src.runner import OcxRunner, PackageInfo


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _dep_entry(ocx: OcxRunner, pkg: PackageInfo, *, alias: str | None = None) -> dict:
    """Build a dependency descriptor from a published PackageInfo."""
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    entry: dict = {"identifier": f"{pkg.fq}@{digest}"}
    if alias is not None:
        entry["alias"] = alias
    return entry


def _push_dep_and_app(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    *,
    dep_alias: str | None = None,
    env_token: str,
) -> tuple[PackageInfo, PackageInfo]:
    """Push a leaf dep and an app package whose env var uses an interpolation token."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = make_package(ocx, leaf_repo, "1.0.0", tmp_path, new=True)
    dep = _dep_entry(ocx, leaf, alias=dep_alias)

    app = make_package(
        ocx,
        app_repo,
        "1.0.0",
        tmp_path,
        new=True,
        env=[{"key": "DEP_PATH", "type": "constant", "value": env_token}],
        dependencies=[dep],
    )
    return leaf, app


# ---------------------------------------------------------------------------
# Runtime resolution tests
# ---------------------------------------------------------------------------


def test_dep_install_path_resolves_to_content_dir(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """${deps.NAME.installPath} resolves to the dep's content directory after install."""
    leaf_repo = f"{unique_repo}_leaf"
    leaf, app = _push_dep_and_app(
        ocx,
        unique_repo,
        tmp_path,
        env_token=f"${{deps.{leaf_repo}.installPath}}",
    )

    ocx.plain("install", "--select", app.short)

    env_result = ocx.json("env", app.short)
    dep_path_entry = next((e for e in env_result if e["key"] == "DEP_PATH"), None)
    assert dep_path_entry is not None, f"DEP_PATH missing in env: {[e['key'] for e in env_result]}"

    resolved = dep_path_entry["value"]
    # Token must be expanded — no literal ${deps. remaining
    assert "${deps." not in resolved, f"token not expanded: {resolved!r}"
    # Resolved path must exist on disk
    assert Path(resolved).exists(), f"resolved path does not exist: {resolved!r}"
    # Must point into the packages/ CAS tree
    assert "packages" in resolved, f"expected CAS packages path, got: {resolved!r}"


def test_dep_install_path_with_alias(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """${deps.ALIAS.installPath} resolves when the dep declares an alias."""
    leaf, app = _push_dep_and_app(
        ocx,
        unique_repo,
        tmp_path,
        dep_alias="my-dep",
        env_token="${deps.my-dep.installPath}",
    )

    ocx.plain("install", "--select", app.short)

    env_result = ocx.json("env", app.short)
    dep_path_entry = next((e for e in env_result if e["key"] == "DEP_PATH"), None)
    assert dep_path_entry is not None, f"DEP_PATH missing in env: {[e['key'] for e in env_result]}"

    resolved = dep_path_entry["value"]
    assert "${deps." not in resolved, f"alias token not expanded: {resolved!r}"
    assert Path(resolved).exists(), f"resolved alias path does not exist: {resolved!r}"


def test_dep_install_path_mixed_with_install_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """${installPath} and ${deps.NAME.installPath} can coexist in the same value."""
    leaf_repo = f"{unique_repo}_leaf"
    leaf, app = _push_dep_and_app(
        ocx,
        unique_repo,
        tmp_path,
        env_token=f"${{installPath}}:${{deps.{leaf_repo}.installPath}}/bin",
    )

    ocx.plain("install", "--select", app.short)

    env_result = ocx.json("env", app.short)
    dep_path_entry = next((e for e in env_result if e["key"] == "DEP_PATH"), None)
    assert dep_path_entry is not None, f"DEP_PATH missing in env: {[e['key'] for e in env_result]}"

    resolved = dep_path_entry["value"]
    assert "${deps." not in resolved, f"dep token not expanded: {resolved!r}"
    assert "${installPath}" not in resolved, f"installPath token not expanded: {resolved!r}"
    # Should be a colon-separated pair of paths
    assert ":" in resolved, f"expected two paths separated by ':', got: {resolved!r}"


# ---------------------------------------------------------------------------
# Publish-time validation tests
# ---------------------------------------------------------------------------


def test_package_push_rejects_undeclared_dep_ref(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """package push fails when env var references a dep not in the dependencies list."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = make_package(ocx, leaf_repo, "1.0.0", tmp_path, new=True)
    dep = _dep_entry(ocx, leaf)

    # The dep is declared as `leaf_repo`, but the env var references `nonexistent`
    with pytest.raises(AssertionError) as exc_info:
        make_package(
            ocx,
            app_repo,
            "1.0.0",
            tmp_path,
            new=True,
            env=[{"key": "X", "type": "constant", "value": "${deps.nonexistent.installPath}"}],
            dependencies=[dep],
        )

    assert "nonexistent" in str(exc_info.value).lower() or "nonexistent" in str(exc_info.value), (
        f"expected 'nonexistent' in error: {exc_info.value}"
    )


def test_file_uri_mode_validates_metadata_via_validmetadata(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """``ocx exec file://<package-root>`` must run the on-disk metadata
    through ``ValidMetadata::try_from`` so an undeclared ``${deps.X.installPath}``
    token fails fast at consumption time.

    The setup mutates the metadata.json of an installed package to inject an
    undeclared dep token. The file:// resolution path centralizes
    ``ValidMetadata::try_from`` against on-disk metadata, so the input must
    surface a ``DataError`` (exit 65) naming the undeclared dep.
    """
    pkg = published_package
    ocx.plain("install", pkg.short)

    install_dir = ocx.json("find", pkg.short)
    content_path_str = install_dir.get(pkg.short) if isinstance(install_dir, dict) else None
    assert content_path_str, "find must return a content path for the installed package"
    content_path = Path(content_path_str)
    pkg_root = content_path.parent
    metadata_path = pkg_root / "metadata.json"
    assert metadata_path.exists(), f"metadata.json must exist at {metadata_path}"

    # Inject an undeclared ${deps.NAME.installPath} reference. The dep is NOT
    # declared in `dependencies`, so a ValidMetadata round-trip must reject it.
    poisoned = {
        "type": "bundle",
        "version": 1,
        "env": [
            {
                "key": "BROKEN",
                "type": "constant",
                "value": "${deps.undeclared_dep_xyz.installPath}",
            }
        ],
    }
    metadata_path.write_text(json.dumps(poisoned))

    result = ocx.run(
        "exec", f"file://{pkg_root}", "--", "echo", "hi", check=False,
    )
    assert result.returncode == 65, (
        f"file:// URI with metadata that fails ValidMetadata must exit 65 (DataError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    assert "undeclared_dep_xyz" in result.stderr, (
        f"error must name the undeclared dep 'undeclared_dep_xyz'; "
        f"stderr={result.stderr.strip()}"
    )


def test_package_push_rejects_unsupported_field(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """package push fails when env var references an unsupported dep field."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = make_package(ocx, leaf_repo, "1.0.0", tmp_path, new=True)
    dep = _dep_entry(ocx, leaf)

    with pytest.raises(AssertionError) as exc_info:
        make_package(
            ocx,
            app_repo,
            "1.0.0",
            tmp_path,
            new=True,
            env=[{"key": "X", "type": "constant", "value": f"${{deps.{leaf_repo}.version}}"}],
            dependencies=[dep],
        )

    error_text = str(exc_info.value)
    assert "version" in error_text, f"expected 'version' in error: {error_text}"
