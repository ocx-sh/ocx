from pathlib import Path

from src import OcxRunner, PackageInfo, assert_dir_exists, assert_not_exists


def test_purge_removes_object_directory(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx uninstall --purge <pkg>"""
    pkg = published_package
    result = ocx.json("install", pkg.short)
    content = Path(result[pkg.short]["content"])
    assert_dir_exists(content)

    ocx.plain("uninstall", "--purge", pkg.short)
    assert_not_exists(content)
