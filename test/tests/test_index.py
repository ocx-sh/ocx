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


def test_index_update_tag_scoped(
    ocx: OcxRunner, tmp_path: Path
):
    """ocx index update repo:tag updates only that tag, not all tags."""
    from src.helpers import make_package

    short_id = uuid4().hex[:8]
    repo = f"t_{short_id}_tag_scoped"
    fq = f"{ocx.registry}/{repo}"

    # Publish two versions but do NOT index them (make_package calls index update,
    # so we use a separate index to avoid polluting the default one).
    custom_index = tmp_path / "scoped_index"
    custom_index.mkdir()

    # Publish v1.0 and v2.0 to the registry.
    make_package(ocx, repo, "1.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo, "2.0", tmp_path, new=False, cascade=False)

    # Wipe the tag store so we start fresh.
    import shutil
    ocx_home = Path(ocx.env["OCX_HOME"])
    tags_dir = ocx_home / "tags"
    if tags_dir.exists():
        shutil.rmtree(tags_dir)

    # Update only tag 1.0 — should NOT fetch 2.0.
    ocx.plain("index", "update", f"{fq}:1.0")
    result = ocx.plain("index", "list", fq)
    assert "1.0" in result.stdout
    assert "2.0" not in result.stdout

    # Now update tag 2.0 — should have both.
    ocx.plain("index", "update", f"{fq}:2.0")
    result = ocx.plain("index", "list", fq)
    assert "1.0" in result.stdout
    assert "2.0" in result.stdout

    # Wipe tag store again and update bare (no tag) — should get both.
    if tags_dir.exists():
        shutil.rmtree(tags_dir)
    ocx.plain("index", "update", fq)
    result = ocx.plain("index", "list", fq)
    assert "1.0" in result.stdout
    assert "2.0" in result.stdout


def test_remote_index_list_does_not_write_local_tags(
    ocx: OcxRunner, published_package: PackageInfo
):
    """`--remote index list` is a pure query — must not mutate $OCX_HOME/tags/.

    Locks the M3 contract (Phase 11): query callers pass `IndexOperation::Query`
    so `ChainedIndex::fetch_manifest` never walks the source chain on miss
    even in Remote mode. Filesystem-state assertion catches both the
    `walk_chain` path and any future regression that writes tags through a
    different path (e.g. `Index::fetch_candidates`).
    """
    pkg = published_package
    # Populate the local tag store via install — this is the legitimate
    # writer; we want to assert the query commands below leave the result
    # byte-identical.
    ocx.json("install", pkg.short)

    tags_root = Path(ocx.env["OCX_HOME"]) / "tags"
    before = sorted(
        (str(p.relative_to(tags_root)), p.read_bytes())
        for p in tags_root.rglob("*.json")
    )
    assert before, "preconditions: install must populate the local tag store"

    # Pure-query commands under --remote — both flag forms covered.
    ocx.plain("--remote", "index", "list", pkg.short)
    ocx.plain("--remote", "index", "list", "--platforms", pkg.short)

    after = sorted(
        (str(p.relative_to(tags_root)), p.read_bytes())
        for p in tags_root.rglob("*.json")
    )
    assert before == after, (
        "Pure --remote query must not mutate $OCX_HOME/tags/. "
        f"Before: {[name for name, _ in before]}, after: {[name for name, _ in after]}"
    )


def test_index_list_rejects_digest_bearing_identifier(
    ocx: OcxRunner, published_package: PackageInfo
):
    """`ocx index list <pkg>@<digest>` is a usage error.

    `index list` enumerates tags; a digest narrows nothing. The error
    message must point users to `package info` as the alternative.
    Tag-only identifiers (`<pkg>:<tag>`) stay supported — they filter the
    returned list.
    """
    pkg = published_package
    # Resolve the digest for the published tag via a remote query.
    json_out = ocx.json("--remote", "index", "list", "--platforms", pkg.short)
    # Use a synthetic but well-formed digest string so we don't depend on
    # `package info` shape — only the rejection path matters here.
    fake_digest = "sha256:" + ("a" * 64)
    digest_id = f"{pkg.short}@{fake_digest}"

    result = ocx.plain("index", "list", digest_id, check=False)
    assert result.returncode != 0, "digest-bearing identifier must exit non-zero"
    assert "does not accept digest-pinned identifiers" in result.stderr
    assert "package info" in result.stderr

    # Tag-only path still works (no regression).
    success = ocx.plain("index", "update", pkg.short)
    assert success.returncode == 0
    assert json_out is not None  # silence ruff unused warning


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
