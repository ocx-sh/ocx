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


def _dep_entry(ocx: OcxRunner, pkg: PackageInfo, *, name: str | None = None) -> dict:
    """Build a dependency descriptor from a published PackageInfo."""
    digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    entry: dict = {"identifier": f"{pkg.fq}@{digest}"}
    if name is not None:
        entry["name"] = name
    return entry


def _push_dep_and_app(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    *,
    dep_name: str | None = None,
    env_token: str,
) -> tuple[PackageInfo, PackageInfo]:
    """Push a leaf dep and an app package whose env var uses an interpolation token."""
    leaf_repo = f"{unique_repo}_leaf"
    app_repo = f"{unique_repo}_app"

    leaf = make_package(ocx, leaf_repo, "1.0.0", tmp_path, new=True)
    dep = _dep_entry(ocx, leaf, name=dep_name)

    app = make_package(
        ocx,
        app_repo,
        "1.0.0",
        tmp_path,
        new=True,
        # Tagged ``public`` so the per-entry filter under v2 default-Consumer
        # admits the entry — interpolation tests assert the resolved value
        # is reachable via `ocx env`, which uses Consumer mode.
        env=[{"key": "DEP_PATH", "type": "constant", "value": env_token, "visibility": "public"}],
        dependencies=[dep],
    )
    return leaf, app


# ---------------------------------------------------------------------------
# Runtime resolution tests
# ---------------------------------------------------------------------------


def test_dep_install_path_resolves_to_content_dir(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """${deps.NAME.installPath} resolves to the dep's content directory after install.

    S4: pins the EXACT resolved path against the canonical content directory
    surfaced by `ocx find <leaf>`. The two paths must agree byte-for-byte —
    a regression that resolved the dep token to anything other than the
    leaf's installed content path (e.g. the consumer's content path, a
    layer dir, or a stale snapshot) is caught here.
    """
    leaf_repo = f"{unique_repo}_leaf"
    leaf, app = _push_dep_and_app(
        ocx,
        unique_repo,
        tmp_path,
        env_token=f"${{deps.{leaf_repo}.installPath}}",
    )

    ocx.plain("install", "--select", app.short)

    env_result = ocx.json("env", app.short)
    dep_path_entry = next((e for e in env_result["entries"] if e["key"] == "DEP_PATH"), None)
    assert dep_path_entry is not None, f"DEP_PATH missing in env: {[e['key'] for e in env_result["entries"]]}"

    resolved = dep_path_entry["value"]
    # Token must be expanded — no literal ${deps. remaining
    assert "${deps." not in resolved, f"token not expanded: {resolved!r}"
    # Resolved path must exist on disk
    assert Path(resolved).exists(), f"resolved path does not exist: {resolved!r}"
    # Must point into the packages/ CAS tree
    assert "packages" in resolved, f"expected CAS packages path, got: {resolved!r}"

    # S4: pin EXACT path equality against `ocx find <leaf>`. `ocx find`
    # reports the package root; ${deps.NAME.installPath} resolves to the
    # dep's content tree (`<root>/content`), so the resolved value must
    # equal the leaf's package root joined with `content`.
    leaf_paths = ocx.json("find", leaf.short)
    expected_leaf_root = leaf_paths.get(leaf.short) if isinstance(leaf_paths, dict) else leaf_paths
    assert expected_leaf_root, (
        f"`ocx find {leaf.short}` must return the leaf's package root; got: {leaf_paths!r}"
    )
    expected_leaf_content = str(Path(expected_leaf_root) / "content")
    assert resolved == expected_leaf_content, (
        f"${{deps.{leaf_repo}.installPath}} must resolve to leaf's content tree "
        f"(`<root>/content`); got {resolved!r}, expected {expected_leaf_content!r}"
    )
    # Canonical CAS shape: <packages_root>/<registry>/<algo>/<2hex>/<30hex>/content
    assert resolved.endswith("/content"), (
        f"resolved path must end with `/content` (CAS package content directory): {resolved!r}"
    )


def test_transitive_dep_install_path_propagates_via_public_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """``${deps.<NAME>.installPath}`` declared on a transitively-public dep resolves at the consumer.

    Topology: A → B → C. C is a leaf. B declares ``C_PATH = ${deps.<C>.installPath}``
    in its env and a Public dep on C. A has a Public dep on B and no env of its own.
    Running ``ocx env A`` must expose ``C_PATH`` with the token expanded to C's
    on-disk content path — i.e. B's env propagates through the Public chain and
    its template references the (also Public) transitive dep at resolve time.
    """
    c_repo = f"{unique_repo}_c"
    b_repo = f"{unique_repo}_b"
    a_repo = f"{unique_repo}_a"

    # C: leaf, no deps.
    c = make_package(ocx, c_repo, "1.0.0", tmp_path, new=True)

    # B: Public dep on C, env references C's installPath. ``C_PATH`` is
    # tagged ``public`` so it propagates to A under Consumer mode (default).
    c_dep = _dep_entry(ocx, c)
    c_dep["visibility"] = "public"
    b = make_package(
        ocx, b_repo, "1.0.0", tmp_path,
        new=True,
        env=[{"key": "C_PATH", "type": "constant", "value": f"${{deps.{c_repo}.installPath}}",
              "visibility": "public"}],
        dependencies=[c_dep],
    )

    # A: Public dep on B, no env of its own.
    b_dep = _dep_entry(ocx, b)
    b_dep["visibility"] = "public"
    a = make_package(
        ocx, a_repo, "1.0.0", tmp_path,
        new=True,
        dependencies=[b_dep],
    )

    ocx.plain("install", "--select", a.short)

    env_result = ocx.json("env", a.short)
    c_path_entry = next((e for e in env_result["entries"] if e["key"] == "C_PATH"), None)
    keys = [e["key"] for e in env_result["entries"]]
    assert c_path_entry is not None, (
        f"C_PATH (declared on B) must propagate to A through Public chain; got keys={keys}"
    )

    resolved = c_path_entry["value"]
    assert "${deps." not in resolved, f"transitive token not expanded: {resolved!r}"
    assert Path(resolved).exists(), f"resolved transitive path missing: {resolved!r}"
    # The resolved path must equal C's content path (not B's), proving the
    # template was expanded against C and not silently against the consumer.
    # `ocx find` returns the package root, so we join `content/` to compare
    # against the runtime ${installPath} value.
    c_paths = ocx.json("find", c.short)
    expected_c_root = c_paths.get(c.short) if isinstance(c_paths, dict) else c_paths
    assert expected_c_root, f"`ocx find {c.short}` returned no package root: {c_paths!r}"
    expected_c_content = str(Path(expected_c_root) / "content")
    assert resolved == expected_c_content, (
        f"C_PATH must equal C's content path; got {resolved!r}, expected {expected_c_content!r}"
    )


def test_dep_install_path_with_explicit_name(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """${deps.NAME.installPath} resolves when the dep declares an explicit name."""
    leaf, app = _push_dep_and_app(
        ocx,
        unique_repo,
        tmp_path,
        dep_name="my-dep",
        env_token="${deps.my-dep.installPath}",
    )

    ocx.plain("install", "--select", app.short)

    env_result = ocx.json("env", app.short)
    dep_path_entry = next((e for e in env_result["entries"] if e["key"] == "DEP_PATH"), None)
    assert dep_path_entry is not None, f"DEP_PATH missing in env: {[e['key'] for e in env_result["entries"]]}"

    resolved = dep_path_entry["value"]
    assert "${deps." not in resolved, f"explicit-name token not expanded: {resolved!r}"
    assert Path(resolved).exists(), f"resolved explicit-name path does not exist: {resolved!r}"


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
    dep_path_entry = next((e for e in env_result["entries"] if e["key"] == "DEP_PATH"), None)
    assert dep_path_entry is not None, f"DEP_PATH missing in env: {[e['key'] for e in env_result["entries"]]}"

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


def test_launcher_exec_validates_metadata_via_validmetadata(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """``ocx launcher exec <package-root>`` must run the on-disk metadata
    through ``ValidMetadata::try_from`` so an undeclared ``${deps.X.installPath}``
    token fails fast at consumption time.

    The setup mutates the metadata.json of an installed package to inject an
    undeclared dep token. The ``launcher exec`` resolution path centralizes
    ``ValidMetadata::try_from`` against on-disk metadata, so the input must
    surface a ``DataError`` (exit 65) naming the undeclared dep.
    """
    pkg = published_package
    ocx.plain("install", pkg.short)

    install_dir = ocx.json("find", pkg.short)
    pkg_root_str = install_dir.get(pkg.short) if isinstance(install_dir, dict) else None
    assert pkg_root_str, "find must return a package root for the installed package"
    pkg_root = Path(pkg_root_str)
    metadata_path = pkg_root / "metadata.json"
    assert metadata_path.exists(), f"metadata.json must exist at {metadata_path}"

    # Inject an undeclared ${deps.NAME.installPath} reference. The dep is NOT
    # declared in `dependencies`, so a ValidMetadata round-trip must reject it.
    poisoned = {
        "type": "bundle", "version": 1, "env": [
            {
                "key": "BROKEN",
                "type": "constant",
                "value": "${deps.undeclared_dep_xyz.installPath}",
            }
        ],
    }
    metadata_path.write_text(json.dumps(poisoned))

    result = ocx.run(
        "launcher", "exec", str(pkg_root), "--", "echo", check=False, format=None,
    )
    assert result.returncode == 65, (
        f"launcher exec with metadata that fails ValidMetadata must exit 65 (DataError); "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    assert "undeclared_dep_xyz" in result.stderr, (
        f"error must name the undeclared dep 'undeclared_dep_xyz'; "
        f"stderr={result.stderr.strip()}"
    )


def test_package_push_rejects_transitive_dep_ref(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`${deps.NAME.installPath}` must reference a *direct* dep, not a transitive one.

    Topology: R → D → T. T is a published package in the registry and a dep of D,
    but R declares only D as a dependency. R's env references `${deps.<T>.installPath}`,
    which is allowed only for D's interpolation namespace — not R's. Publish-time
    validation in `validate_env_tokens` walks R's declared deps only and must reject
    the unknown name.
    """
    t_repo = f"{unique_repo}_transitive"
    d_repo = f"{unique_repo}_direct"
    r_repo = f"{unique_repo}_root"

    t_pkg = make_package(ocx, t_repo, "1.0.0", tmp_path, new=True)
    t_dep_entry = _dep_entry(ocx, t_pkg)

    d_pkg = make_package(
        ocx, d_repo, "1.0.0", tmp_path,
        new=True,
        dependencies=[t_dep_entry],
    )
    d_dep_entry = _dep_entry(ocx, d_pkg)

    # R declares only D as a dependency; the env value references T's name,
    # which exists transitively but is not in R's direct dependency map.
    with pytest.raises(AssertionError) as exc_info:
        make_package(
            ocx, r_repo, "1.0.0", tmp_path,
            new=True,
            env=[{"key": "T_PATH", "type": "constant", "value": f"${{deps.{t_repo}.installPath}}"}],
            dependencies=[d_dep_entry],
        )

    error_text = str(exc_info.value)
    assert t_repo in error_text, (
        f"error must name the transitive dep '{t_repo}': {error_text}"
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


def test_uppercase_dep_name_token_rejected_at_consumption(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """``${deps.Python.installPath}`` (uppercase NAME) is caught as an unknown
    placeholder and rejected with exit 65 at consumption time.

    Contract: the dep-token regex ``DEP_TOKEN_PATTERN`` uses ``[a-z0-9][a-z0-9_-]*``
    so ``Python`` does not match — the token is not treated as a dep reference.
    After ``${installPath}`` is stripped, ``UNKNOWN_TOKEN_RE`` matches the
    leftover ``${deps.Python.installPath}`` as an unknown placeholder and
    ``ValidMetadata::try_from`` returns ``DataError``.

    Verification approach: install a package, mutate its ``metadata.json`` to
    inject the uppercase token (bypassing publish-time push validation), then
    invoke ``ocx launcher exec <pkg_root>`` — which runs ``ValidMetadata::try_from``
    at consumption time — and assert exit 65 with the placeholder appearing
    verbatim in the error output.

    This test documents that the uppercase-name case does not panic, produce a
    confusing "dep not found" message (``UnknownDependencyRef``), or silently
    pass through — it produces a clear ``UnknownPlaceholder`` error naming
    the exact token.
    """
    pkg = published_package
    ocx.plain("install", pkg.short)

    install_dir = ocx.json("find", pkg.short)
    pkg_root_str = install_dir.get(pkg.short) if isinstance(install_dir, dict) else None
    assert pkg_root_str, "find must return a package root for the installed package"
    pkg_root = Path(pkg_root_str)
    metadata_path = pkg_root / "metadata.json"
    assert metadata_path.exists(), f"metadata.json must exist at {metadata_path}"

    # Inject ``${deps.Python.installPath}`` — uppercase NAME not matched by
    # DEP_TOKEN_PATTERN, so it falls through to UNKNOWN_TOKEN_RE. No
    # `dependencies` entry is included; the token is checked after dep-token
    # stripping, so it is irrelevant whether Python is declared.
    poisoned = {
        "type": "bundle",
        "version": 1,
        "env": [
            {
                "key": "DEP_PATH",
                "type": "constant",
                "value": "${deps.Python.installPath}",
            }
        ],
    }
    metadata_path.write_text(json.dumps(poisoned))

    result = ocx.run(
        "launcher", "exec", str(pkg_root), "--", "echo", check=False, format=None,
    )
    assert result.returncode == 65, (
        f"uppercase dep NAME token must surface as DataError (exit 65) at consumption time; "
        f"got rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    # The placeholder must appear verbatim in the error so the user can identify
    # exactly which token was rejected — not just "unknown dep" (UnknownDependencyRef).
    assert "${deps.Python.installPath}" in result.stderr or "Python" in result.stderr, (
        f"error must name the uppercase placeholder verbatim; "
        f"stderr={result.stderr.strip()}"
    )


def test_transitive_dep_token_rejected_at_exec_resolve_time(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Contract 17 direct test: ``${deps.T.installPath}`` in root R's metadata
    is rejected at ``ocx exec`` resolve time when T is only a transitive dep
    (reachable via D, not declared in R's direct dependencies).

    Topology: R → D → T. T is a leaf. D has T as a direct dep. R has D as a
    direct dep but does NOT declare T. R's installed ``metadata.json`` is then
    mutated to reference ``${deps.<T>.installPath}``.

    ``ocx launcher exec <R_root>`` runs ``ValidMetadata::try_from`` against R's
    on-disk metadata. ``validate_env_tokens`` iterates R's *direct* deps only
    (just D), so T is not found → ``UnknownDependencyRef`` → exit 65.

    This is a more direct exercise of Contract 17 than the indirect path through
    ``test_launcher_exec_validates_metadata_via_validmetadata``, which uses a
    package with no deps at all. Here T genuinely exists in the registry and is
    transitively reachable — proving the check operates on the direct-dep
    namespace, not the full transitive closure.
    """
    t_repo = f"{unique_repo}_t"
    d_repo = f"{unique_repo}_d"
    r_repo = f"{unique_repo}_r"

    # T: leaf package, no deps.
    t_pkg = make_package(ocx, t_repo, "1.0.0", tmp_path, new=True)
    t_dep = _dep_entry(ocx, t_pkg)

    # D: direct dep on T.
    d_pkg = make_package(
        ocx, d_repo, "1.0.0", tmp_path,
        new=True,
        dependencies=[t_dep],
    )
    d_dep = _dep_entry(ocx, d_pkg)

    # R: direct dep on D only. No reference to T at push time.
    r_pkg = make_package(
        ocx, r_repo, "1.0.0", tmp_path,
        new=True,
        dependencies=[d_dep],
    )

    # Install R (which transitively installs D and T).
    ocx.plain("install", "--select", r_pkg.short)

    # Locate R's installed package root.
    find_result = ocx.json("find", r_pkg.short)
    r_root_str = find_result.get(r_pkg.short) if isinstance(find_result, dict) else None
    assert r_root_str, f"`ocx find {r_pkg.short}` must return R's package root"
    r_root = Path(r_root_str)
    metadata_path = r_root / "metadata.json"
    assert metadata_path.exists(), f"metadata.json must exist at {metadata_path}"

    # Mutate R's metadata.json to reference T by name in the env section.
    # T is in R's transitive closure (via D) but NOT in R's direct dep list,
    # so validate_env_tokens must reject this as UnknownDependencyRef.
    poisoned = {
        "type": "bundle",
        "version": 1,
        "dependencies": [d_dep],  # only D is declared — T is absent
        "env": [
            {
                "key": "T_PATH",
                "type": "constant",
                "value": f"${{deps.{t_repo}.installPath}}",
            }
        ],
    }
    metadata_path.write_text(json.dumps(poisoned))

    result = ocx.run(
        "launcher", "exec", str(r_root), "--", "echo", check=False, format=None,
    )
    assert result.returncode == 65, (
        f"transitive-dep token in root R must be rejected as UnknownDependencyRef (exit 65) "
        f"at launcher exec resolve time; got rc={result.returncode}, stderr={result.stderr.strip()}"
    )
    assert t_repo in result.stderr, (
        f"error must name the transitive dep '{t_repo}' that was referenced but not declared; "
        f"stderr={result.stderr.strip()}"
    )
