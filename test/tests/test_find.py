from pathlib import Path

from src import OcxRunner, PackageInfo, registry_dir


def test_find_returns_content_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx find <pkg>"""
    pkg = published_package
    install_result = ocx.json("install", pkg.short)
    candidate = Path(install_result[pkg.short]["path"])

    find_result = ocx.json("find", pkg.short)
    # find (default) returns the object-store content path;
    # the candidate symlink from install resolves to the same location.
    assert Path(find_result[pkg.short]) == candidate.resolve()


def test_find_candidate_returns_candidate_symlink(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>; ocx find --candidate <pkg>"""
    pkg = published_package
    ocx.json("install", pkg.short)

    find_result = ocx.json("find", "--candidate", pkg.short)
    candidate = Path(find_result[pkg.short])

    expected = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "candidates"
        / pkg.tag
    )
    assert candidate == expected


def test_find_current_returns_current_symlink(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install -s <pkg>; ocx find --current <pkg>"""
    pkg = published_package
    ocx.json("install", "-s", pkg.short)

    find_result = ocx.json("find", "--current", pkg.short)
    current = Path(find_result[pkg.short])

    expected = (
        Path(ocx.env["OCX_HOME"])
        / "installs"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "current"
    )
    assert current == expected


def test_find_fails_when_not_installed(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx find <pkg>  (not installed, expects failure)"""
    pkg = published_package
    result = ocx.plain("find", pkg.short, check=False)
    assert result.returncode != 0
