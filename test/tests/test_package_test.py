# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package test``.

Tests exercise the full lifecycle: local package materialization, command
execution in the composed env, tempdir lifecycle (auto-clean vs ``--keep``),
``--output`` override, dependency auto-install, offline digest-layer fetch,
metadata validation, surface selection (``--self``), ``--clean`` env stripping,
and identifier validation.

Plan reference: plan_package_test.md §4 Phase 3 acceptance-test table (17 rows).

Exit codes per quality-rust-exit_codes.md:
  0  = Success
  1  = Failure (child propagation)
  64 = UsageError
  65 = DataError
  74 = IoError
  81 = OfflineBlocked

Child exit codes propagate verbatim (Unix passthrough).
"""
from __future__ import annotations

import glob
import json
import os
import re
import stat
import sys
from pathlib import Path
from uuid import uuid4

import pytest

from src import OcxRunner, current_platform
from src.helpers import make_package
from src.runner import PackageInfo


# ---------------------------------------------------------------------------
# Helpers (DAMP per quality-core.md — keep tests self-contained)
# ---------------------------------------------------------------------------

_PLATFORM = current_platform()


def _make_test_package(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    *,
    tag: str = "1.0.0",
    bins: list[str] | None = None,
    env: list[dict] | None = None,
    dependencies: list[dict] | None = None,
) -> tuple[Path, Path, PackageInfo]:
    """Create a local package layout (content dir + metadata file + bundle) for
    use with ``ocx package test``.

    Returns ``(bundle_path, metadata_path, package_info)`` where
    ``package_info`` is the pushed package record (needed for digest refs and
    dep descriptors). The bundle is also pushed to the registry so digest-layer
    tests can resolve layer blobs.
    """
    plat = _PLATFORM
    marker = f"marker-{uuid4().hex[:12]}"
    bin_names = bins or ["hello"]

    pkg_dir = tmp_path / f"pkg-{unique_repo}-{tag}"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)

    for name in bin_names:
        script = bin_dir / name
        if sys.platform == "win32":
            script = script.with_suffix(".bat")
            script.write_text(f"@echo {marker}\n")
        else:
            script.write_text(f"#!/bin/sh\necho {marker}\n")
            script.chmod(script.stat().st_mode | stat.S_IEXEC)

    home_key = unique_repo.upper().replace("-", "_") + "_HOME"
    metadata_env = env or [
        {
            "key": "PATH",
            "type": "path",
            "required": True,
            "value": "${installPath}/bin",
            "visibility": "public",
        },
        {
            "key": home_key,
            "type": "constant",
            "value": "${installPath}",
            "visibility": "public",
        },
    ]

    metadata_path = tmp_path / f"metadata-{unique_repo}-{tag}.json"
    metadata_obj: dict = {"type": "bundle", "version": 1, "env": metadata_env}
    if dependencies:
        metadata_obj["dependencies"] = dependencies
    metadata_path.write_text(json.dumps(metadata_obj))

    bundle = tmp_path / f"bundle-{unique_repo}-{tag}.tar.xz"
    ocx.plain("package", "create", "-m", str(metadata_path), "-o", str(bundle), str(pkg_dir))

    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    ocx.plain("package", "push", "-n", "-p", plat, "-m", str(metadata_path), "-i", fq, str(bundle))
    short = f"{unique_repo}:{tag}"
    ocx.plain("index", "update", short)

    pkg_info = PackageInfo(
        repo=unique_repo,
        tag=tag,
        short=short,
        fq=fq,
        content_dir=pkg_dir,
        marker=marker,
        platform=plat,
    )
    return bundle, metadata_path, pkg_info


def _ocx_home(ocx: OcxRunner) -> Path:
    """Return the OCX_HOME path configured on this runner."""
    return Path(ocx.env["OCX_HOME"])


def _temp_test_dir(ocx: OcxRunner) -> Path:
    """Return the default temp/test/ directory used by ``ocx package test``."""
    return _ocx_home(ocx) / "temp" / "test"


# ---------------------------------------------------------------------------
# Tests — plan §4 Phase 3 table (17 rows)
# ---------------------------------------------------------------------------


def test_runs_command_in_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """package test materializes the package and runs CMD in the composed env.

    The metadata declares a BIN_DIR (via PATH env entry). The command ``sh -c
    'echo $<REPO>_HOME'`` must print the materialized package root path.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)
    home_key = unique_repo.upper().replace("-", "_") + "_HOME"

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", f"echo ${home_key}",
        check=False,
    )

    assert result.returncode == 0, f"expected exit 0, got {result.returncode}\nstderr: {result.stderr}"
    # The env var must be set to the materialized package root.
    home_val = result.stdout.strip()
    assert home_val, f"{home_key} must be set in the composed env, got empty stdout"


def test_propagates_child_failure(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """package test forwards child exit code verbatim (Unix passthrough).

    A child that exits 7 must cause ``ocx package test`` to also exit 7.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", "exit 7",
        check=False,
    )

    assert result.returncode == 7, (
        f"expected child exit code 7 to propagate verbatim, got {result.returncode}"
    )


def test_cleans_temp_on_success(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Default temp dir is auto-deleted after successful child exit.

    The ``$OCX_HOME/temp/test/`` directory must be empty (or absent) after
    a successful ``package test`` run without ``--keep`` or ``--output``.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 0, f"expected exit 0, got {result.returncode}\nstderr: {result.stderr}"

    temp_dir = _temp_test_dir(ocx)
    if temp_dir.exists():
        leftover = [p for p in temp_dir.iterdir() if p.is_dir()]
        assert not leftover, (
            f"temp/test/ must be empty after successful run, found: {leftover}"
        )


def test_cleans_temp_on_child_failure(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Default temp dir is auto-deleted even when child fails (review F5).

    A bare invocation (no ``--keep``, no ``--output``) deletes the scratch dir
    on both success AND failure. ``--keep`` is the explicit opt-in for inspection
    on failure. Plan §3 Failure semantics.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", "exit 1",
        check=False,
    )

    assert result.returncode == 1, (
        f"expected child exit 1 to propagate, got {result.returncode}"
    )

    temp_dir = _temp_test_dir(ocx)
    if temp_dir.exists():
        leftover = [p for p in temp_dir.iterdir() if p.is_dir()]
        assert not leftover, (
            f"temp/test/ must be empty after child failure without --keep, found: {leftover}"
        )


def test_keep_preserves_temp_on_failure(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--keep`` preserves the temp dir when the child fails.

    The tempdir path must be printed to stderr and must exist on disk after exit.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "--keep",
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", "exit 1",
        check=False,
    )

    assert result.returncode == 1, (
        f"expected child exit 1 to propagate, got {result.returncode}"
    )

    # Path must be printed to stderr in the form "kept at <path>".
    assert "kept at " in result.stderr, (
        f"missing 'kept at ' in stderr: {result.stderr!r}"
    )

    kept_path_str = re.search(r"kept at (\S+)", result.stderr).group(1)  # type: ignore[union-attr]
    kept_path = Path(kept_path_str)
    assert kept_path.exists(), (
        f"--keep must preserve tempdir at {kept_path}, but it does not exist"
    )


def test_keep_preserves_temp_on_success(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--keep`` preserves the temp dir when the child succeeds.

    Even on exit 0, the directory must survive and its path must appear on
    stderr.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "--keep",
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 0, f"expected exit 0, got {result.returncode}\nstderr: {result.stderr}"

    # Path must be printed to stderr in the form "kept at <path>".
    assert "kept at " in result.stderr, (
        f"missing 'kept at ' in stderr: {result.stderr!r}"
    )

    kept_path_str = re.search(r"kept at (\S+)", result.stderr).group(1)  # type: ignore[union-attr]
    kept_path = Path(kept_path_str)
    assert kept_path.exists(), (
        f"--keep must preserve tempdir at {kept_path} on success, but it does not exist"
    )


def test_output_dir_honored_same_fs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--output DIR`` materializes the package into DIR and leaves it after exit.

    DIR must be on the same filesystem as ``$OCX_HOME/layers/`` (same tmpfs in
    tests). The directory survives after ``package test`` exits.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    # Place output inside OCX_HOME to guarantee same-fs constraint.
    output_dir = _ocx_home(ocx) / "test-output" / unique_repo
    output_dir.mkdir(parents=True, exist_ok=True)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-o", str(output_dir),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 0, (
        f"expected exit 0 with same-fs --output, got {result.returncode}\nstderr: {result.stderr}"
    )
    # The output directory must contain the materialized package.
    assert output_dir.exists(), f"--output DIR must survive after exit: {output_dir}"
    assert (output_dir / "content").is_dir() or any(output_dir.iterdir()), (
        f"output DIR must be non-empty after package test: {output_dir}"
    )


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="cross-filesystem --output detection requires Unix dev() comparison",
)
def test_output_dir_cross_fs_errors(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--output`` pointing to a different filesystem yields IoError (74).

    ``$OCX_HOME/layers/`` and ``/tmp`` may be on different filesystems.
    When they are, the hardlink assembly would fail (EXDEV) and ``package test``
    must detect this up-front and exit 74 with a clear message.

    If ``/tmp`` happens to be on the same filesystem as ``$OCX_HOME`` (e.g.
    in containerised CI where everything is on one rootfs), this test is
    marked as an expected skip condition.
    """
    import os

    ocx_home = _ocx_home(ocx)
    tmp_output = tmp_path / "cross-fs-output"
    tmp_output.mkdir(parents=True)

    # Check if /tmp and OCX_HOME are actually on different filesystems.
    ocx_home_dev = os.stat(ocx_home).st_dev
    tmp_dev = os.stat(tmp_output).st_dev

    if ocx_home_dev == tmp_dev:
        pytest.skip(
            f"OCX_HOME ({ocx_home}) and tmp_path ({tmp_output}) are on the same "
            f"filesystem (dev={ocx_home_dev}) — cross-fs scenario not reproducible"
        )

    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-o", str(tmp_output),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 74, (
        f"expected exit 74 (IoError) for cross-fs --output, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )
    assert "same filesystem" in result.stderr.lower() or "filesystem" in result.stderr.lower(), (
        "error message must mention filesystem requirement"
    )


def test_output_dir_nonempty_errors(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--output`` pointing at a non-empty existing directory yields UsageError (64).

    Plan review finding F2: prevent clobbering user data by rejecting non-empty
    output dirs up-front.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    # Pre-populate the output dir with a file to make it non-empty.
    output_dir = _ocx_home(ocx) / "nonempty-output" / unique_repo
    output_dir.mkdir(parents=True)
    (output_dir / "existing-file.txt").write_text("not empty\n")

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-o", str(output_dir),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 64, (
        f"expected exit 64 (UsageError) for non-empty --output, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_auto_installs_deps(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Dependencies declared in metadata are auto-installed into the object store.

    The test creates a leaf package (``lib``) and a root package whose metadata
    declares ``lib`` as a dependency. Running ``package test`` on the root
    package must auto-install ``lib`` into ``$OCX_HOME/packages/``.
    """
    from src.registry import fetch_platform_manifest_digest

    lib_repo = f"{unique_repo}-lib"
    lib_pkg = make_package(ocx, lib_repo, "1.0.0", tmp_path, new=True)

    lib_digest = fetch_platform_manifest_digest(ocx.registry, lib_repo, "1.0.0")
    dep_entry = {
        "identifier": f"{lib_pkg.fq}@{lib_digest}",
        "name": "mylib",
        "visibility": "public",
    }

    bundle, metadata_path, pkg_info = _make_test_package(
        ocx, unique_repo, tmp_path, dependencies=[dep_entry]
    )

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 0, (
        f"expected exit 0 with auto-installed deps, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )

    # The lib package must be present in the object store.
    packages_root = _ocx_home(ocx) / "packages"
    assert packages_root.exists(), "packages store must exist after dep install"
    # At least one package directory must have been created.
    assert any(packages_root.rglob("content")), (
        "no package content found in object store — dep was not installed"
    )


def test_pulls_digest_layer_on_demand(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A digest-only layer reference is auto-pulled from the registry on demand.

    The test pushes a bundle to the registry, fetches the layer digest from the
    pushed manifest, removes the layer from the local cache so the on-demand
    pull path must fire, then runs ``package test`` with the
    ``sha256:<hex>.tar.xz`` digest-layer ref. The layer must be fetched from
    the registry without error.

    This exercises ``LayerRef::Digest`` inside ``pull_local`` end-to-end.
    """
    import json
    import shutil
    import urllib.request
    from src.registry import fetch_manifest_from_registry

    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    # Fetch the OCI image manifest that was pushed for this package.
    # The manifest may be an image index; look for a platform manifest child.
    raw_manifest = fetch_manifest_from_registry(ocx.registry, pkg_info.repo, pkg_info.tag)

    # Resolve to an image manifest (not an image index).
    # Image index entries reference child manifests by digest. ORAS uses `:` as
    # separator so cannot fetch by digest that way; use the registry HTTP API directly.
    if "manifests" in raw_manifest:
        # image index: pick the first child that matches the current platform.
        for entry in raw_manifest["manifests"]:
            plat = entry.get("platform", {})
            plat_str = f"{plat.get('os', '')}/{plat.get('architecture', '')}"
            if plat_str == _PLATFORM or not raw_manifest.get("manifests"):
                child_digest = entry["digest"]
                # Fetch child manifest via the registry HTTP API using @digest syntax.
                child_url = (
                    f"http://{ocx.registry}/v2/{pkg_info.repo}/manifests/{child_digest}"
                )
                child_req = urllib.request.Request(
                    child_url,
                    headers={"Accept": "application/vnd.oci.image.manifest.v1+json"},
                )
                with urllib.request.urlopen(child_req, timeout=10) as resp:
                    raw_manifest = json.loads(resp.read())
                break

    layers = raw_manifest.get("layers", [])
    if not layers:
        pytest.skip("pushed manifest has no layers — cannot construct digest-only layer ref")

    layer = layers[0]
    layer_digest: str = layer["digest"]  # e.g. "sha256:abcdef..."
    layer_media_type: str = layer.get("mediaType", "")

    # Determine the file extension from the media type.
    if "xz" in layer_media_type:
        ext = "tar.xz"
    else:
        ext = "tar.gz"

    # Form the digest-layer reference as expected by LayerRef::Digest parser.
    digest_ref = f"{layer_digest}.{ext}"

    # Delete the local layer cache so the on-demand pull path must fire.
    # OCX stores layers at: $OCX_HOME/layers/{registry_slug}/{alg}/{2hex}/{30hex}/
    ocx_home = _ocx_home(ocx)
    layers_root = ocx_home / "layers"
    if layers_root.exists():
        # Remove all cached layers so the digest ref triggers a registry pull.
        shutil.rmtree(str(layers_root))

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.short,
        digest_ref,
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 0, (
        f"expected exit 0 with digest-only layer ref '{digest_ref}', "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


def test_offline_missing_digest_layer_blocks(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--offline`` + missing digest-only layer blob yields OfflineBlocked (81).

    Scenario:
    1. Push a package to the registry so a real layer digest is available.
    2. Strip the local layer and blob caches so the digest ref is not locally
       available.
    3. Re-run ``package test --offline`` with the ``sha256:<hex>.<ext>`` digest
       ref — the offline flag blocks all network calls, so the missing blob must
       cause exit 81 (OfflineBlocked).
    """
    import json
    import shutil
    import urllib.request
    from src.registry import fetch_manifest_from_registry

    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    # Fetch the OCI image manifest to find the real layer digest.
    raw_manifest = fetch_manifest_from_registry(ocx.registry, pkg_info.repo, pkg_info.tag)

    # Resolve image index → image manifest if needed.
    if "manifests" in raw_manifest:
        for entry in raw_manifest["manifests"]:
            plat = entry.get("platform", {})
            plat_str = f"{plat.get('os', '')}/{plat.get('architecture', '')}"
            if plat_str == _PLATFORM or not raw_manifest.get("manifests"):
                child_digest = entry["digest"]
                child_url = (
                    f"http://{ocx.registry}/v2/{pkg_info.repo}/manifests/{child_digest}"
                )
                child_req = urllib.request.Request(
                    child_url,
                    headers={"Accept": "application/vnd.oci.image.manifest.v1+json"},
                )
                with urllib.request.urlopen(child_req, timeout=10) as resp:
                    raw_manifest = json.loads(resp.read())
                break

    layers = raw_manifest.get("layers", [])
    if not layers:
        pytest.skip("pushed manifest has no layers — cannot construct digest-only layer ref")

    layer = layers[0]
    layer_digest: str = layer["digest"]
    layer_media_type: str = layer.get("mediaType", "")
    ext = "tar.xz" if "xz" in layer_media_type else "tar.gz"
    digest_ref = f"{layer_digest}.{ext}"

    # Strip ALL local caches so the digest ref is not available offline.
    ocx_home = _ocx_home(ocx)
    for sub in ("layers", "blobs", "packages"):
        cache_dir = ocx_home / sub
        if cache_dir.exists():
            shutil.rmtree(str(cache_dir))

    # Run package test with --offline and the digest-only layer ref.
    # The missing blob must block with OfflineBlocked (81).
    result = ocx.plain(
        "--offline",
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.short,
        digest_ref,
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 81, (
        f"expected exit 81 (OfflineBlocked) when digest layer is missing in offline mode, "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


def test_invalid_metadata_data_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Malformed metadata.json yields DataError (65).

    ``package test`` reads and validates the metadata file before materializing
    the package. A file that is not valid JSON (or fails schema validation) must
    cause an early exit with code 65.
    """
    bundle_dir = tmp_path / f"pkg-invalid-{unique_repo}"
    bin_dir = bundle_dir / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "hello").write_text("#!/bin/sh\necho hi\n")
    if sys.platform != "win32":
        (bin_dir / "hello").chmod(0o755)

    # Write a syntactically invalid metadata file.
    bad_metadata = tmp_path / "bad-metadata.json"
    bad_metadata.write_text("this is not valid JSON {{{")

    # Create a minimal bundle (content doesn't matter — metadata is checked first).
    bundle = tmp_path / "bundle-invalid.tar.xz"
    # Use a valid minimal metadata to create the bundle, then override the path.
    valid_meta = tmp_path / "valid-meta.json"
    valid_meta.write_text('{"type":"bundle","version":1}')
    ocx.plain("package", "create", "-m", str(valid_meta), "-o", str(bundle), str(bundle_dir))

    fq = f"{ocx.registry}/{unique_repo}-invalid:1.0.0"
    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(bad_metadata),
        "-i", fq,
        str(bundle),
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 65, (
        f"expected exit 65 (DataError) for invalid metadata, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_self_view_emits_private_surface(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--self`` selects the private env surface.

    The package metadata declares one public var and one private var.
    Without ``--self``, only the public var is visible.
    With ``--self``, both vars are visible.
    """
    private_key = "MY_PRIVATE_VAR"
    public_key = f"{unique_repo.upper().replace('-', '_')}_HOME"

    env_entries = [
        {
            "key": "PATH",
            "type": "path",
            "required": True,
            "value": "${installPath}/bin",
            "visibility": "public",
        },
        {
            "key": public_key,
            "type": "constant",
            "value": "${installPath}",
            "visibility": "public",
        },
        {
            "key": private_key,
            "type": "constant",
            "value": "secret-value",
            "visibility": "private",
        },
    ]

    bundle, metadata_path, pkg_info = _make_test_package(
        ocx, unique_repo, tmp_path, env=env_entries
    )

    # With --self: private var must be visible.
    result_self = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "--self",
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", f"echo ${private_key}",
        check=False,
    )

    assert result_self.returncode == 0, (
        f"expected exit 0 with --self, got {result_self.returncode}\nstderr: {result_self.stderr}"
    )
    assert "secret-value" in result_self.stdout, (
        f"--self must expose private var {private_key}=secret-value; "
        f"got stdout: {result_self.stdout!r}"
    )

    # Without --self (consumer/interface surface): private var must be absent.
    result_consumer = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", f'echo "${{{private_key}:-NOT_SET}}"',
        check=False,
    )

    assert result_consumer.returncode == 0, (
        f"expected exit 0 without --self, got {result_consumer.returncode}\nstderr: {result_consumer.stderr}"
    )
    assert "secret-value" not in result_consumer.stdout, (
        f"consumer surface must NOT expose private var {private_key}; "
        f"got stdout: {result_consumer.stdout!r}"
    )


def test_clean_strips_ambient_env(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--clean`` strips ambient parent env; only OCX vars + composed package vars reach child.

    The parent shell exports a sentinel variable (``OCX_TEST_AMBIENT_FOO``). With
    ``--clean``, the child must not see it. The composed package vars (PATH from
    metadata) must still be present so the child can run.
    """
    # Include /bin and /usr/bin so ``sh`` is reachable under --clean even when
    # only the package's PATH entries remain in the child env. The path type
    # resolver validates existence for ``required: true`` entries, so each
    # system directory is a separate ``required: false`` entry.
    clean_env = [
        {
            "key": "PATH",
            "type": "path",
            "required": True,
            "value": "${installPath}/bin",
            "visibility": "public",
        },
        {
            "key": "PATH",
            "type": "path",
            "required": False,
            "value": "/bin",
            "visibility": "public",
        },
        {
            "key": "PATH",
            "type": "path",
            "required": False,
            "value": "/usr/bin",
            "visibility": "public",
        },
    ]
    bundle, metadata_path, pkg_info = _make_test_package(
        ocx, unique_repo, tmp_path, env=clean_env
    )

    # Inject a sentinel into the runner's env (simulating an ambient var).
    sentinel_key = "OCX_TEST_AMBIENT_FOO"
    sentinel_val = f"ambient-{uuid4().hex[:8]}"
    ocx_with_ambient = OcxRunner(ocx.binary, ocx.ocx_home, ocx.registry)
    ocx_with_ambient.env[sentinel_key] = sentinel_val

    result = ocx_with_ambient.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "--clean",
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh", "-c", f'echo "${{{sentinel_key}:-ABSENT}}"',
        check=False,
    )

    assert result.returncode == 0, (
        f"expected exit 0 with --clean, got {result.returncode}\nstderr: {result.stderr}"
    )
    assert sentinel_val not in result.stdout, (
        f"--clean must strip ambient var {sentinel_key}={sentinel_val!r}; "
        f"got stdout: {result.stdout!r}"
    )
    # The sentinel must be absent (replaced by "ABSENT" sentinel).
    assert "ABSENT" in result.stdout, (
        f"ambient var {sentinel_key} must be absent under --clean; "
        f"got stdout: {result.stdout!r}"
    )


def test_rejects_digest_in_identifier(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``repo:tag@sha256:<hex>`` is rejected with UsageError (64).

    The plan §1 specifies: "Tag form (``repo:tag``) only; an explicit ``@digest``
    is rejected (the digest is computed locally during this command and supplying
    one would conflict)." Plan open question Q9 was deferred — confirmed here.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    # Construct a fully-qualified identifier with an explicit digest.
    fake_digest = "sha256:" + "ab" * 32
    fq_with_digest = f"{pkg_info.fq}@{fake_digest}"

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", fq_with_digest,
        str(bundle),
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 64, (
        f"expected exit 64 (UsageError) for @digest in identifier, got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_digest_only_no_metadata_usage_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Providing only digest layers and no ``--metadata`` flag yields UsageError (64).

    When all layers are digest references (no file layers) OCX cannot infer a
    metadata file because there is no file layer sibling to derive from. The
    ``--metadata`` flag is required. Omitting it must exit with code 64
    (``UsageError``) and NOT exit with the generic failure code 1.
    """
    # Push a package to the registry so we have a real tag to reference.
    _, _, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    # Provide a digest-only layer ref (any fake digest is fine; the usage
    # error fires before any layer resolution takes place).
    fake_digest_ref = f"sha256:{'ab' * 32}.tar.xz"

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        # No -m / --metadata flag
        "-i", pkg_info.short,
        fake_digest_ref,
        "--",
        "sh", "-c", "true",
        check=False,
    )

    assert result.returncode == 64, (
        f"expected exit 64 (UsageError) when --metadata is omitted with digest-only layers, "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )


def test_arg_parity_with_push(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``package test`` accepts the same ``-p``, ``-m``, ``IDENTIFIER LAYERS...`` shape as ``package push``.

    This test verifies *argument parsing only* — it uses a no-op command (``true``)
    and accepts any post-parse exit code. The key assertion is that clap does NOT
    reject the argument shape. This documents parity with ``package push`` as a
    compile-time-captured invariant.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    # Same argument shape as ``ocx package push -p <plat> -m <meta> <fq> <bundle>``.
    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.fq,
        str(bundle),
        "--",
        "true",
        check=False,
    )

    # Parsing must succeed: if clap rejects the argument shape, it exits 64
    # with a "Usage:" message. Any other exit code (0, child codes, 65, 74, …)
    # means clap accepted the shape and the pipeline ran. The assertion
    # intentionally allows non-zero post-parse exits (e.g., missing command on
    # PATH) — we only care that the argument *shape* matches package push.
    assert result.returncode != 64, (
        f"argument shape must parse correctly (parity with package push); "
        f"got exit 64\nstderr: {result.stderr}"
    )


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="bare relative --output path test requires Unix cwd semantics",
)
def test_output_dir_relative_path_works(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--output build`` (bare relative name, no slash) passes filesystem
    validation when the cwd is on the same filesystem as ``$OCX_HOME/layers/``.

    Regression test for Warn #2: ``validate_same_filesystem`` previously called
    ``tokio::fs::metadata("")`` (empty parent of a bare relative name) which
    fails with ENOENT before the directory is created.  The fix normalises an
    empty parent to ``"."``.

    The package metadata uses only ``constant`` env entries (no ``required``
    ``path`` entries) so that relative-path env resolution does not introduce
    a secondary error.
    """
    import subprocess

    # Build a package with a constant-only env (no required path checks) so
    # that relative-path env resolution does not trigger a secondary error.
    bundle, metadata_path, pkg_info = _make_test_package(
        ocx,
        unique_repo,
        tmp_path,
        env=[
            {
                "key": unique_repo.upper().replace("-", "_") + "_HOME",
                "type": "constant",
                "value": "${installPath}",
                "visibility": "public",
            }
        ],
    )

    # Use tmp_path as cwd so "build" resolves to tmp_path/build.
    # tmp_path is on the same filesystem as OCX_HOME (both under the test tmpfs).
    cmd = [
        str(ocx.binary),
        "package",
        "test",
        "-p",
        _PLATFORM,
        "-m",
        str(metadata_path),
        "-o",
        "build",  # bare relative name — the bug manifested here
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "sh",
        "-c",
        "true",
    ]
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env=ocx.env,
        cwd=str(tmp_path),  # cwd provides the "."-parent for "build"
    )

    assert result.returncode == 0, (
        f"expected exit 0 when --output is a bare relative name, "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    output_dir = tmp_path / "build"
    assert output_dir.exists(), f"--output build must create {output_dir}"
    assert any(output_dir.iterdir()), f"output directory must be non-empty: {output_dir}"


def test_output_dir_rejects_symlink_target(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--output DIR`` is rejected with UsageError (64) when the target path
    contains a symlink component.

    Plan §4 Phase 5 security requirement: "--output DIR canonicalized; refuse
    symlink-traversal targets." A symlink along the output path could redirect
    writes to attacker-controlled locations.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    # Create a real directory and a symlink pointing at it.
    real_dir = tmp_path / "real-output-dir"
    real_dir.mkdir()
    symlink_target = tmp_path / "symlink-to-real"
    symlink_target.symlink_to(real_dir)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-o", str(symlink_target),
        "-i", pkg_info.fq,
        str(bundle),
        "--",
        "true",
        check=False,
    )

    assert result.returncode == 64, (
        f"expected exit 64 (UsageError) when --output is a symlink, "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )
    assert "symlink" in result.stderr.lower(), (
        f"error message must mention 'symlink'; got stderr: {result.stderr!r}"
    )


def test_runs_package_entrypoint(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Running a binary that lives INSIDE the materialized package succeeds.

    This is the critical regression test for the pre-delete bug: the old
    implementation called ``drop(td_guard)`` (deleting the tempdir) BEFORE
    ``execvp``, which caused ENOENT when the binary lived inside the tempdir.
    The fix switches bare invocations to spawn+wait so the tempdir stays alive
    while the child runs and is only deleted after the child exits.

    The test uses a package-provided binary (``hello`` script in ``bin/``),
    NOT a system binary, so the binary path is inside the materialized package.
    Exit 0 and the marker string on stdout both prove that the binary ran
    successfully from within the materialized package directory.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "hello",  # the package entrypoint — NOT a system binary
        check=False,
    )

    assert result.returncode == 0, (
        f"expected exit 0 when running a package entrypoint inside the "
        f"materialized temp dir; got {result.returncode}\nstderr: {result.stderr}"
    )
    # The binary prints the unique marker — confirms it actually executed.
    assert pkg_info.marker in result.stdout, (
        f"expected marker '{pkg_info.marker}' in stdout from the package "
        f"entrypoint 'hello'; got stdout: {result.stdout!r}"
    )

    # Tempdir must be cleaned up after child exits.
    temp_dir = _temp_test_dir(ocx)
    if temp_dir.exists():
        leftover = [p for p in temp_dir.iterdir() if p.is_dir()]
        assert not leftover, (
            f"temp/test/ must be empty after successful entrypoint run, "
            f"found: {leftover}"
        )


def test_runs_entrypoint_with_keep(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--keep`` runs a package entrypoint successfully AND preserves the tempdir.

    Two invariants in one test:
    1. The binary (a package entrypoint inside the tempdir) executes and exits 0.
    2. The tempdir is NOT deleted after child exits.

    Without ``--keep``, the keep-path uses execvp directly (no pre-delete race).
    This test verifies the ``--keep`` path still produces correct output and
    leaves the directory on disk for post-failure inspection.
    """
    bundle, metadata_path, pkg_info = _make_test_package(ocx, unique_repo, tmp_path)

    result = ocx.plain(
        "package", "test",
        "-p", _PLATFORM,
        "-m", str(metadata_path),
        "--keep",
        "-i", pkg_info.short,
        str(bundle),
        "--",
        "hello",  # the package entrypoint — NOT a system binary
        check=False,
    )

    assert result.returncode == 0, (
        f"expected exit 0 when running a package entrypoint with --keep; "
        f"got {result.returncode}\nstderr: {result.stderr}"
    )

    # The binary's unique marker must appear in stdout.
    assert pkg_info.marker in result.stdout, (
        f"expected marker '{pkg_info.marker}' in stdout from the package "
        f"entrypoint 'hello' under --keep; got stdout: {result.stdout!r}"
    )

    # The kept path must be printed to stderr and the directory must exist.
    assert "kept at " in result.stderr, (
        f"missing 'kept at ' in stderr under --keep: {result.stderr!r}"
    )

    kept_match = re.search(r"kept at (\S+)", result.stderr)
    assert kept_match is not None, (
        f"could not parse 'kept at <path>' from stderr: {result.stderr!r}"
    )
    kept_path = Path(kept_match.group(1))
    assert kept_path.exists(), (
        f"--keep must preserve tempdir at {kept_path} after entrypoint exit, "
        f"but it does not exist"
    )
