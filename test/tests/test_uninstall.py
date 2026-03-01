from pathlib import Path

from src import OcxRunner, PackageInfo, assert_not_exists, assert_symlink_exists, registry_dir


def test_uninstall_removes_candidate_symlink(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx uninstall <pkg>"""
    pkg = published_package
    ocx.json("install", pkg.short)

    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "candidates"
        / pkg.tag
    )
    assert_symlink_exists(candidate)

    ocx.plain("uninstall", pkg.short)
    assert_not_exists(candidate)


def test_uninstall_preserves_current_symlink(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """ocx install -s <v1>; ocx install <v2>; ocx uninstall <v2>"""
    v1, v2 = published_two_versions
    ocx.json("install", "-s", v1.short)
    ocx.json("install", v2.short)

    ocx.plain("uninstall", v2.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / v1.repo
        / "current"
    )
    assert_symlink_exists(current)


def test_uninstall_deselect_removes_both(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install -s <pkg>; ocx uninstall -d <pkg>"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)

    ocx.plain("uninstall", "-d", pkg.short)

    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "candidates"
        / pkg.tag
    )
    current = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "current"
    )
    assert_not_exists(candidate)
    assert_not_exists(current)
