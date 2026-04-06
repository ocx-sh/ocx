"""Tests for ``ocx package pull``."""
from pathlib import Path

from src import OcxRunner, PackageInfo, assert_dir_exists, registry_dir


def test_package_pull_populates_object_store(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """ocx package pull <pkg> downloads to the object store."""
    result = ocx.json("package", "pull", published_package.short)

    content = Path(result[published_package.short])
    assert_dir_exists(content)
    assert "packages" in str(content), f"Expected package store path, got: {content}"


def test_package_pull_does_not_create_candidate_symlink(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """ocx package pull <pkg> must NOT create a candidate symlink."""
    pkg = published_package
    ocx.plain("package", "pull", pkg.short)

    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "candidates"
        / pkg.tag
    )
    assert not candidate.exists(), f"Candidate symlink should not exist: {candidate}"


def test_package_pull_does_not_create_current_symlink(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """ocx package pull <pkg> must NOT create a current symlink."""
    pkg = published_package
    ocx.plain("package", "pull", pkg.short)

    current = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / pkg.repo
        / "current"
    )
    assert not current.exists(), f"Current symlink should not exist: {current}"


def test_package_pull_is_idempotent(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """Pulling the same package twice succeeds and returns the same path."""
    result1 = ocx.json("package", "pull", published_package.short)
    result2 = ocx.json("package", "pull", published_package.short)

    assert result1[published_package.short] == result2[published_package.short]
