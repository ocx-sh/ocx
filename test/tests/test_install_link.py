"""`ocx package install --link <PATH>` and the `--link` read side of `which`, `env` and `exec`."""

from pathlib import Path

import pytest

from src import (
    OcxRunner,
    PackageInfo,
    assert_dir_exists,
    assert_not_exists,
    assert_symlink_exists,
)
from src.helpers import make_package

pytestmark = pytest.mark.command("install")

EXIT_USAGE = 64
EXIT_DATA = 65


def test_install_link_creates_link_and_which_reads_it(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """ocx package install --link <path> <pkg>; ocx package which --link <path> <pkg>"""
    pkg = published_package
    link = tmp_path / "tools" / "pkg"

    result = ocx.json("package", "install", "--link", str(link), pkg.short)
    assert Path(result[pkg.short]["path"]) == link
    assert_symlink_exists(link)
    assert (link / "content").is_dir()

    which = ocx.json("package", "which", "--link", str(link), pkg.short)
    assert Path(which[pkg.short]["path"]) == link


def test_env_link_roots_values_in_the_link(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """ocx package env --link <path> <pkg> roots `<REPO>_HOME` in the link, not the store."""
    pkg = published_package
    link = tmp_path / "pkg"
    ocx.plain("package", "install", "--link", str(link), pkg.short)

    home_key = pkg.repo.upper().replace("-", "_") + "_HOME"
    env_result = ocx.json("package", "env", "--link", str(link), pkg.short)
    home_entry = next(e for e in env_result["entries"] if e["key"] == home_key)
    assert home_entry["value"].startswith(str(link))


def test_exec_link_runs_the_linked_binary(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """ocx package exec --link <path> <pkg> -- hello"""
    pkg = published_package
    link = tmp_path / "pkg"
    ocx.plain("package", "install", "--link", str(link), pkg.short)

    result = ocx.plain("package", "exec", "--link", str(link), pkg.short, "--", "hello")
    assert result.stdout.strip() == pkg.marker


def test_link_keeps_package_from_clean_until_removed(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """install --link; uninstall; clean keeps the package; rm the link; clean collects it."""
    pkg = published_package
    link = tmp_path / "pkg"
    ocx.plain("package", "install", "--link", str(link), pkg.short)
    package_root = link.resolve()

    ocx.plain("package", "uninstall", pkg.short)
    ocx.plain("clean")
    assert_dir_exists(package_root)

    link.unlink()
    ocx.plain("clean")
    assert_not_exists(package_root)


def test_install_link_refuses_an_occupied_path(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """A path holding a user file is refused (exit 65) and left untouched."""
    pkg = published_package
    link = tmp_path / "pkg"
    link.write_text("user data")

    result = ocx.plain("package", "install", "--link", str(link), pkg.short, check=False)
    assert result.returncode == EXIT_DATA, result.stderr
    assert link.read_text() == "user data"


def test_link_takes_exactly_one_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """--link names one path, so two packages are a usage error on install and on a read."""
    a = make_package(ocx, f"{unique_repo}-a", "1.0.0", tmp_path)
    b = make_package(ocx, f"{unique_repo}-b", "1.0.0", tmp_path)
    link = tmp_path / "pkg"

    install = ocx.plain("package", "install", "--link", str(link), a.short, b.short, check=False)
    assert install.returncode == EXIT_USAGE, install.stderr
    assert not link.exists()

    which = ocx.plain("package", "which", "--link", str(link), a.short, b.short, check=False)
    assert which.returncode == EXIT_USAGE, which.stderr
