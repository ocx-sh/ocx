from pathlib import Path
from uuid import uuid4

from src import OcxRunner, PackageInfo


def test_index_update_succeeds(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx index update <pkg>"""
    pkg = published_package
    result = ocx.plain("index", "update", pkg.short)
    assert result.returncode == 0


def test_index_list_shows_tag(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx index update <pkg>; ocx index list <repo>"""
    pkg = published_package
    ocx.plain("index", "update", pkg.short)

    result = ocx.plain("index", "list", pkg.repo)
    assert pkg.tag in result.stdout


def test_index_catalog_shows_repo(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx index update <pkg>; ocx index catalog"""
    pkg = published_package
    ocx.plain("index", "update", pkg.short)

    result = ocx.plain("index", "catalog")
    assert pkg.repo in result.stdout


def test_index_flag_reads_from_custom_path(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """--index redirects index reads and writes to an arbitrary path."""
    pkg = published_package
    custom_index = tmp_path / "custom_index"
    custom_index.mkdir()
    empty_index = tmp_path / "empty_index"
    empty_index.mkdir()

    # Update into the custom path.
    ocx.plain("--index", str(custom_index), "index", "update", pkg.short)

    # Reading from the custom path: tag is visible.
    result = ocx.plain("--index", str(custom_index), "index", "list", pkg.repo)
    assert pkg.tag in result.stdout

    # Reading from a different empty path: tag is absent, proving --index is respected.
    result = ocx.plain("--index", str(empty_index), "index", "list", pkg.repo, check=False)
    assert pkg.tag not in result.stdout


def test_ocx_index_env_var_reads_from_custom_path(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """OCX_INDEX env var redirects index reads and writes to an arbitrary path."""
    pkg = published_package
    custom_index = tmp_path / "custom_index"
    custom_index.mkdir()

    ocx.env["OCX_INDEX"] = str(custom_index)
    try:
        ocx.plain("index", "update", pkg.short)

        result = ocx.plain("index", "list", pkg.repo)
        assert pkg.tag in result.stdout
    finally:
        del ocx.env["OCX_INDEX"]


def test_index_flag_takes_precedence_over_ocx_index_env(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """--index flag wins over OCX_INDEX when both are set."""
    pkg = published_package
    custom_index = tmp_path / "custom_index"
    custom_index.mkdir()
    empty_index = tmp_path / "empty_index"
    empty_index.mkdir()

    # Populate the custom index.
    ocx.plain("--index", str(custom_index), "index", "update", pkg.short)

    # OCX_INDEX points to the empty dir, but --index points to the populated one.
    ocx.env["OCX_INDEX"] = str(empty_index)
    try:
        result = ocx.plain("--index", str(custom_index), "index", "list", pkg.repo)
        assert pkg.tag in result.stdout
    finally:
        del ocx.env["OCX_INDEX"]


def test_index_list_excludes_internal_tags(
    ocx: OcxRunner, tmp_path: Path
):
    """Internal __ocx.* tags must never appear in index list output."""
    short_id = uuid4().hex[:8]
    repo = f"t_{short_id}_internal_tag_filter"
    fq = f"{ocx.registry}/{repo}"

    # Push a real package so the repo has a normal tag.
    from src.helpers import make_package
    make_package(ocx, repo, "1.0.0", tmp_path, new=True)

    # Push a description, creating the __ocx.desc tag on the registry.
    readme = tmp_path / "README.md"
    readme.write_text("# Test\n")
    ocx.plain("package", "describe", "--readme", str(readme), fq)

    # Remote index: __ocx.desc must not appear.
    result = ocx.plain("--remote", "index", "list", fq)
    assert "__ocx" not in result.stdout
    assert "1.0.0" in result.stdout

    # Local index after update: __ocx.desc must not appear.
    ocx.plain("index", "update", fq)
    result = ocx.plain("index", "list", fq)
    assert "__ocx" not in result.stdout
    assert "1.0.0" in result.stdout
