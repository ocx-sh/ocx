from pathlib import Path

from src import OcxRunner, PackageInfo, assert_dir_exists, assert_symlink_exists, registry_dir


def test_install_creates_candidate_symlink(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>"""
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


def test_install_creates_content_directory(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>"""
    pkg = published_package
    result = ocx.json("install", pkg.short)
    content = Path(result[pkg.short]["path"])
    assert_dir_exists(content)


def test_install_select_creates_current_symlink(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install -s <pkg>"""
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


def test_install_cleans_temp_directory(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg> should not leave temp directories behind."""
    pkg = published_package
    ocx.json("install", pkg.short)

    temp_dir = Path(ocx.env["OCX_HOME"]) / "temp"
    if temp_dir.exists():
        leftover = list(temp_dir.iterdir())
        assert leftover == [], f"temp directory not cleaned up: {leftover}"


def test_install_without_select_preserves_current(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """ocx install -s <v1>; ocx install <v2>"""
    v1, v2 = published_two_versions

    # Install v1 with select
    result_v1 = ocx.json("install", "-s", v1.short)
    content_v1 = Path(result_v1[v1.short]["path"])

    # Install v2 without select
    ocx.json("install", v2.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / v1.repo
        / "current"
    )
    assert current.resolve() == content_v1.resolve()
