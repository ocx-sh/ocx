from pathlib import Path

from src import OcxRunner, PackageInfo, assert_dir_exists, assert_not_exists


def test_clean_removes_unreferenced_objects(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx uninstall <pkg>; ocx clean"""
    pkg = published_package
    result = ocx.json("install", pkg.short)
    candidate = Path(result[pkg.short]["path"])
    content = candidate.resolve()
    assert_dir_exists(content)

    ocx.plain("uninstall", pkg.short)
    assert_dir_exists(content)

    ocx.plain("clean")
    assert_not_exists(content)


def test_clean_removes_stale_temp_directories(ocx: OcxRunner):
    """ocx clean should remove stale temp dir + sibling .lock file."""
    temp_dir = Path(ocx.env["OCX_HOME"]) / "temp"
    stale = temp_dir / "stale_abcdef1234567890abcdef1234567890"
    lock_file = stale.with_suffix(".lock")
    stale.mkdir(parents=True)
    lock_file.touch()
    (stale / "leftover.tar.gz").write_bytes(b"stale data")

    ocx.plain("clean")
    assert_not_exists(stale)
    assert_not_exists(lock_file)


def test_clean_removes_orphan_lock_file(ocx: OcxRunner):
    """ocx clean should remove a .lock file with no corresponding directory."""
    temp_dir = Path(ocx.env["OCX_HOME"]) / "temp"
    temp_dir.mkdir(parents=True, exist_ok=True)
    lock_file = temp_dir / "orphan_abcdef1234567890abcdef12345678.lock"
    lock_file.touch()

    ocx.plain("clean")
    assert_not_exists(lock_file)


def test_clean_removes_orphan_temp_directory(ocx: OcxRunner):
    """ocx clean should remove a temp directory with no .lock file."""
    temp_dir = Path(ocx.env["OCX_HOME"]) / "temp"
    orphan = temp_dir / "orphan_abcdef1234567890abcdef12345678"
    orphan.mkdir(parents=True)
    (orphan / "leftover.tar.gz").write_bytes(b"stale data")

    ocx.plain("clean")
    assert_not_exists(orphan)


def test_clean_preserves_referenced_objects(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx clean"""
    pkg = published_package
    result = ocx.json("install", pkg.short)
    content = Path(result[pkg.short]["path"]).resolve()

    ocx.plain("clean")
    assert_dir_exists(content)


def _registry_slug_from_packages_tree(packages_root: Path, manifest_path: Path) -> str:
    """Recover the registry slug directory name from the absolute path of an
    installed package's manifest.json.

    Layout: {ocx_home}/packages/{registry_slug}/{algo}/{2hex}/{30hex}/manifest.json
    """
    rel = manifest_path.relative_to(packages_root)
    return rel.parts[0]


def test_clean_preserves_config_blob_of_installed_package(
    ocx: OcxRunner, ocx_home: Path, published_package: PackageInfo
):
    """Regression: ocx clean must not delete an installed package's metadata
    config blob.

    Before the architectural fix, ResolvedChain.chain listed only manifest
    blobs (image-index + image-manifest), so ReferenceManager::link_blobs did
    not create a refs/blobs/{config_digest} symlink. The garbage collector
    treated the config blob as orphan and deleted it on the next `ocx clean`,
    which broke `ocx --offline install` rehydration from the local CAS.
    """
    import json

    from src.assertions import assert_path_exists

    pkg = published_package

    # Install the package; this creates packages/.../manifest.json from which
    # we can recover the config blob digest.
    ocx.plain("install", "--select", pkg.short)

    # Walk packages/ to find the installed package's manifest.json.
    packages_root = ocx_home / "packages"
    manifest_paths = list(packages_root.rglob("manifest.json"))
    assert manifest_paths, f"expected at least one manifest.json under {packages_root}"
    manifest = json.loads(manifest_paths[0].read_text())
    config_digest = manifest["config"]["digest"]  # e.g. "sha256:abcd..."

    algo, hex_digest = config_digest.split(":", 1)
    # Same sharding as BlobStore::path: {algorithm}/{hex[0..2]}/{hex[2..32]}/data
    registry_slug = _registry_slug_from_packages_tree(packages_root, manifest_paths[0])
    blob_data = (
        ocx_home
        / "blobs"
        / registry_slug
        / algo
        / hex_digest[:2]
        / hex_digest[2:32]
        / "data"
    )
    # Sanity: the blob is on disk after install.
    assert blob_data.exists(), f"expected config blob on disk after install at {blob_data}"

    # Run clean — should be a no-op for reachable objects.
    ocx.plain("clean")

    # Regression assertion.
    assert_path_exists(
        blob_data,
        (
            f"ocx clean deleted the config blob at {blob_data} while the package "
            "is installed; refs/blobs/ edge is missing for manifest.config.digest"
        ),
    )
