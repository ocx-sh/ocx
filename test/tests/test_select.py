from pathlib import Path

from src import OcxRunner, PackageInfo, assert_not_exists, assert_symlink_exists, registry_dir


def test_select_switches_current_symlink(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """ocx install -s <v1>; ocx install <v2>; ocx select <v2>"""
    v1, v2 = published_two_versions
    ocx.json("install", "-s", v1.short)
    ocx.json("install", v2.short)

    install_v2 = ocx.json("install", v2.short)
    content_v2 = Path(install_v2[v2.short]["path"])

    ocx.plain("select", v2.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / v1.repo
        / "current"
    )
    assert current.resolve() == content_v2.resolve()


def test_deselect_removes_current_symlink(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install -s <pkg>; ocx deselect <pkg>"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "current"
    )
    assert_symlink_exists(current)

    ocx.plain("deselect", pkg.short)
    assert_not_exists(current)


def test_reselect_after_deselect(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install -s <pkg>; ocx deselect <pkg>; ocx select <pkg>"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)
    ocx.plain("deselect", pkg.short)
    ocx.plain("select", pkg.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "current"
    )
    assert_symlink_exists(current)
