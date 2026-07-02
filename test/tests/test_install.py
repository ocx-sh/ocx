from pathlib import Path

from src import (
    OcxRunner,
    PackageInfo,
    assert_dir_exists,
    assert_not_exists,
    assert_symlink_exists,
    current_platform,
    make_package,
    registry_dir,
)

_AMD64 = "linux/amd64"
_ARM64 = "linux/arm64"


def test_install_creates_candidate_symlink(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg>"""
    pkg = published_package
    ocx.json("package", "install", pkg.short)

    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
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
    result = ocx.json("package", "install", pkg.short)
    content = Path(result[pkg.short]["path"])
    assert_dir_exists(content)


def test_install_select_creates_current_symlink(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install -s <pkg>"""
    pkg = published_package
    ocx.json("package", "install", "-s", pkg.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "current"
    )
    assert_symlink_exists(current)


def test_install_select_reports_current_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install -s <pkg> reports the current path, not the candidate.

    --select is the explicit opt-in to move the current pointer, so the
    reported path must surface `current`, the alias the user just set.
    """
    pkg = published_package
    result = ocx.json("package", "install", "-s", pkg.short)

    reported = Path(result[pkg.short]["path"])
    assert reported.name == "current"


def test_install_without_select_reports_candidate_path(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg> reports the tag-pinned candidate path."""
    pkg = published_package
    result = ocx.json("package", "install", pkg.short)

    reported = Path(result[pkg.short]["path"])
    assert reported.name == pkg.tag
    assert reported.parent.name == "candidates"


def test_install_cleans_temp_directory(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx install <pkg> should not leave temp directories behind."""
    pkg = published_package
    ocx.json("package", "install", pkg.short)

    temp_dir = Path(ocx.env["OCX_HOME"]) / "temp"
    if temp_dir.exists():
        leftover = list(temp_dir.iterdir())
        assert leftover == [], f"temp directory not cleaned up: {leftover}"


def test_install_foreign_platform_writes_no_candidate(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Regression (issue #179): a foreign-platform install populates the object
    store but must NOT write the host's `candidates/{tag}` slot, and reports a
    null path.

    The fixture ships linux/amd64 + linux/arm64, so `amd64` is foreign to every
    non-amd64 host and `arm64` is foreign to an amd64 host — the assertion never
    depends on which arch runs it.
    """
    make_package(ocx, unique_repo, "3.28.0", tmp_path / "amd64", platform=_AMD64, new=True)
    make_package(ocx, unique_repo, "3.28.0", tmp_path / "arm64", platform=_ARM64, new=False)

    foreign = _AMD64 if current_platform() != _AMD64 else _ARM64
    ref = f"{ocx.registry}/{unique_repo}:3.28"

    installs = ocx.json("package", "install", f"--platform={foreign}", ref)
    entry = next(iter(installs.values()))
    assert entry["path"] is None, f"foreign install must report no host symlink: {installs}"

    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
        / "3.28"
    )
    assert_not_exists(candidate)


def test_install_without_select_preserves_current(
    ocx: OcxRunner, published_two_versions: tuple[PackageInfo, PackageInfo]
):
    """ocx install -s <v1>; ocx install <v2>"""
    v1, v2 = published_two_versions

    # Install v1 with select
    result_v1 = ocx.json("package", "install", "-s", v1.short)
    content_v1 = Path(result_v1[v1.short]["path"])

    # Install v2 without select
    ocx.json("package", "install", v2.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / v1.repo
        / "current"
    )
    assert current.resolve() == content_v1.resolve()
