from pathlib import Path
from uuid import uuid4

from src import OcxRunner, PackageInfo
from src.runner import registry_dir


def test_index_update_succeeds(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx index update <pkg>"""
    pkg = published_package
    result = ocx.plain("index", "update", pkg.short)
    assert result.returncode == 0


def test_index_update_partial_failure_exits_nonzero_and_stable(
    ocx: OcxRunner, published_package: PackageInfo
):
    """One unresolvable package among several fails the whole batch (nonzero, stable).

    Regression test: `ocx index update` used to always return exit 0 even
    when a tag failed to refresh (the per-package refresh error was logged
    but never surfaced as a batch failure). The command now propagates the
    input-order-first failure, and the exit code is stable across repeated
    runs (not completion-order dependent).
    """
    pkg = published_package
    missing = f"t_{uuid4().hex[:8]}_index_update_missing:9.9.9"

    first = ocx.run("index", "update", pkg.short, missing, format=None, check=False)
    second = ocx.run("index", "update", pkg.short, missing, format=None, check=False)

    assert first.returncode != 0, f"expected nonzero exit, stderr: {first.stderr}"
    assert first.returncode == second.returncode, (
        "exit code must be stable across repeated runs, "
        f"got {first.returncode} then {second.returncode}"
    )


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
    """--index redirects the WHOLE local index collection to an arbitrary path.

    `--index` redirects the collection home (`adr_index_indirection.md`
    Decision A1/A2): every configured source gets its own subtree
    `<home>/<slug(source)>/{c/,p/}` holding the hosted wire grammar verbatim
    (root documents + dispatch-object CAS). The registry is a DERIVED source
    here (a plain OCI registry, not `index.ocx.sh`), so it carries no
    `config.json`/`c/index.json` — its catalog is the directory enumeration
    of `p/` (A2).
    """
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

    # On-disk layout (A2): `<home>/<slug(source)>/p/<repo>.json` (root doc) +
    # `<home>/<slug(source)>/p/<repo>/o/<algo>/<hex>.json` (dispatch-object
    # CAS) — a per-source subtree, not a flat `p/{registry}/{repo}/tags.json`.
    source_dir = custom_index / registry_dir(ocx.registry)
    root_doc = source_dir / "p" / f"{pkg.repo}.json"
    assert root_doc.is_file(), "expected the root document under the redirected index root"
    objects = list((source_dir / "p" / pkg.repo / "o" / "sha256").glob("*.json"))
    assert objects, "expected verbatim dispatch objects in the o/sha256/ CAS"

    # Reading from a different empty path: tag is absent, proving --index is respected.
    result = ocx.plain("--index", str(empty_index), "index", "list", pkg.repo, check=False)
    assert pkg.tag not in result.stdout


def test_ocx_index_env_var_reads_from_custom_path(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """OCX_INDEX env var redirects the whole local index collection to an arbitrary path."""
    pkg = published_package
    custom_index = tmp_path / "custom_index"
    custom_index.mkdir()

    ocx.env["OCX_INDEX"] = str(custom_index)
    try:
        ocx.plain("index", "update", pkg.short)

        result = ocx.plain("index", "list", pkg.repo)
        assert pkg.tag in result.stdout

        # OCX_INDEX redirects the dispatch-object CAS too, not just the root doc.
        source_dir = custom_index / registry_dir(ocx.registry)
        assert list((source_dir / "p" / pkg.repo / "o" / "sha256").glob("*.json")), (
            "expected verbatim dispatch objects under OCX_INDEX-redirected root"
        )
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

    # Publish v1.0 and v2.0 to the registry. `make_package(cascade=False)`
    # indexes only the tagged identifier it just pushed (`index_target =
    # short` in `helpers.make_package`), so each call incrementally adds its
    # own tag to the default source's root document — both tags are already
    # present at this point, which is why the wipe below is load-bearing for
    # the "update only 1.0 must not fetch 2.0" assertion.
    make_package(ocx, repo, "1.0", tmp_path, new=True, cascade=False)
    make_package(ocx, repo, "2.0", tmp_path, new=False, cascade=False)

    # Wipe the default index home so we start fresh (`adr_index_indirection.md`
    # A1: `$OCX_HOME/index`, not the deleted `$OCX_HOME/tags` or
    # `$OCX_HOME/state/registry-index`).
    import shutil
    ocx_home = Path(ocx.env["OCX_HOME"])
    index_home = ocx_home / "index"
    if index_home.exists():
        shutil.rmtree(index_home)

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

    # Wipe the index home again and update bare (no tag) — should get both.
    if index_home.exists():
        shutil.rmtree(index_home)
    ocx.plain("index", "update", fq)
    result = ocx.plain("index", "list", fq)
    assert "1.0" in result.stdout
    assert "2.0" in result.stdout


def test_remote_index_list_does_not_write_local_tags(
    ocx: OcxRunner, published_package: PackageInfo
):
    """`--remote index list` is a pure query — must not mutate the local index.

    Locks the M3 contract (Phase 11): query callers pass `IndexOperation::Query`
    so `ChainedIndex::fetch_manifest` never walks the source chain on miss
    even in Remote mode. Filesystem-state assertion catches both the
    `walk_chain` path and any future regression that writes tags through a
    different path (e.g. `Index::fetch_candidates`).

    The writable surface under test is the whole default index home
    (`$OCX_HOME/index/`, `adr_index_indirection.md` Decision A1) — root
    documents AND the `o/<algo>/<hex>.json` dispatch-object CAS — not the
    deleted `$OCX_HOME/tags/` or `$OCX_HOME/state/registry-index/`.
    """
    pkg = published_package
    # Populate the local index via install — this is the legitimate writer;
    # we want to assert the query commands below leave the result
    # byte-identical.
    ocx.json("package", "install", pkg.short)

    index_home = Path(ocx.env["OCX_HOME"]) / "index"
    before = sorted(
        (str(p.relative_to(index_home)), p.read_bytes())
        for p in index_home.rglob("*")
        if p.is_file()
    )
    assert before, "preconditions: install must populate the local index"

    # Pure-query commands under --remote — both flag forms covered.
    ocx.plain("--remote", "index", "list", pkg.short)
    ocx.plain("--remote", "index", "list", "--platforms", pkg.short)

    after = sorted(
        (str(p.relative_to(index_home)), p.read_bytes())
        for p in index_home.rglob("*")
        if p.is_file()
    )
    assert before == after, (
        "Pure --remote query must not mutate the local index home. "
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


def test_index_list_platforms_accepts_digest_offline(
    ocx: OcxRunner, published_package: PackageInfo
):
    """`ocx index list <repo>@<digest> --platforms` resolves the platform set
    for a digest-pinned identifier fully offline (the accepted B1 scope
    extension). Default mode (no --platforms) still rejects digest-bearing
    identifiers — see `test_index_list_rejects_digest_bearing_identifier`.
    """
    from src.registry import fetch_manifest_digest

    pkg = published_package
    # The tag's own digest is the image-index manifest (one child per pushed
    # platform) — `Platform::from_manifest` only reports a real platform list
    # for an image index, not a flat leaf image manifest (which reports `any`).
    index_digest = fetch_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    # Install populates both the local index (root + dispatch object) and the
    # blob cache, so the offline digest lookup below has no network dependency.
    ocx.json("package", "install", pkg.short)

    digest_id = f"{pkg.repo}@{index_digest}"

    offline = ocx.plain("--offline", "index", "list", digest_id, "--platforms", check=False)
    assert offline.returncode == 0, (
        f"offline digest+--platforms resolve must succeed: rc={offline.returncode}\n{offline.stderr}"
    )
    assert pkg.platform in offline.stdout
    # The digest branch never calls `list_tags` — it must never emit the
    # ordinary tag-lookup "not found in the index" warning.
    assert "not found in the index" not in offline.stderr, (
        f"digest+--platforms must not emit a spurious not-found warning: {offline.stderr}"
    )

    # Default mode (no --platforms) rejects the same digest-bearing identifier.
    rejected = ocx.plain("index", "list", digest_id, check=False)
    assert rejected.returncode != 0, "digest-bearing identifier must exit non-zero without --platforms"
    assert "does not accept digest-pinned identifiers" in rejected.stderr


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
