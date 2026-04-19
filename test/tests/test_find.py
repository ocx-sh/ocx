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
    # find (default) returns the package-store content path; the candidate
    # symlink points at the package root, so traversing into `/content`
    # lands at the same content tree.
    find_path = Path(find_result[pkg.short])
    assert find_path == candidate.resolve() / "content"


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
        / "symlinks"
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
        / "symlinks"
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


def test_find_returns_package_path_not_layer(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx find returns a path inside packages/, never inside layers/.

    With hardlink-based assembly (Plan 8c), packages/{P}/content/ is a real
    directory.  The path returned by `ocx find` must therefore point under
    the packages/ subtree of OCX_HOME.

    This guards against regressions to the previous symlink-based assembly
    where `ocx find` could return a path that, when resolved, pointed into
    layers/.
    """
    pkg = published_package
    ocx.json("install", pkg.short)

    find_result = ocx.json("find", pkg.short)
    find_path = Path(find_result[pkg.short])

    # `ocx find` (without `--candidate`/`--current`) returns the raw
    # package-store content path directly — it is built from OCX_HOME by
    # path join, with no symlinks of its own.  `is_relative_to` is a lexical
    # prefix check, so we compare against OCX_HOME-derived paths without any
    # canonicalization: both sides inherit the same (possibly non-canonical)
    # OCX_HOME prefix.
    ocx_home = Path(ocx.env["OCX_HOME"])
    packages_root = ocx_home / "packages"
    layers_root = ocx_home / "layers"

    # Must be inside the packages/ tree
    assert find_path.is_relative_to(packages_root), (
        f"`ocx find` returned {find_path}, which is outside packages/ "
        f"({packages_root}).  Expected the package content/ directory."
    )

    # Must NOT be inside the layers/ tree (regression guard for symlink assembly)
    assert not find_path.is_relative_to(layers_root), (
        f"`ocx find` returned {find_path}, which is inside layers/ "
        f"({layers_root}).  With hardlink assembly, package content/ must be "
        f"a real directory under packages/, not a symlink into layers/."
    )
