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
