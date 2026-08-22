import json
from collections.abc import Iterator
from hashlib import sha256
from pathlib import Path
from uuid import uuid4

import pytest

from src import OcxRunner, PackageInfo, static_index
from src.registry import fetch_manifest_raw, fetch_platform_manifest_digest
from src.runner import registry_dir

IMAGE_INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"
IMAGE_MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


def test_index_update_succeeds(
    ocx: OcxRunner, published_package: PackageInfo
):
    """ocx index update <pkg>"""
    pkg = published_package
    result = ocx.plain("index", "update", pkg.short)
    assert result.returncode == 0


def _publish_bare_manifest_tag(registry: str, repo: str, source_tag: str, tag: str) -> str:
    """Publishes `tag` pointing DIRECTLY at `source_tag`'s leaf platform
    manifest, straight through the registry HTTP API.

    `ocx package push` never writes this shape under a version tag, and the
    canonical `sha256.<hex>` tag it does write is *also* reserved — so a tag
    that is a bare manifest and nothing else is the only way to exercise the
    bare-manifest rule (D2) in isolation from the reserved-name rule (D7).
    Returns the leaf manifest's digest.
    """
    import requests

    leaf_digest = fetch_platform_manifest_digest(registry, repo, source_tag)
    leaf_bytes, _ = fetch_manifest_raw(registry, repo, leaf_digest)
    requests.put(
        f"http://{registry}/v2/{repo}/manifests/{tag}",
        data=leaf_bytes,
        headers={"Content-Type": IMAGE_MANIFEST_MEDIA_TYPE},
        timeout=10,
    ).raise_for_status()
    return leaf_digest


def test_index_update_records_only_the_image_index_tag_and_stores_no_manifest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
):
    """Write path 1 of 2 — `ocx index update <repo>` (`refresh_derived`).

    Against a repository carrying one image-index tag and one bare-manifest
    tag, the local index records exactly ONE entry and stores exactly ONE
    dispatch object (`adr_oci_index_only_dispatch.md` D1/D2,
    `adr_index_indirection.md` A3).

    D2 is a root rule, not just a storage rule: a bare manifest writes nothing
    to `o/`, so recording its tag would leave the root pointing at an object
    that is not there — the tag-without-an-object absence D1 abolished. The
    old contract recorded the pointer and relied on the absence as an
    encoding; that is what this file now pins the other way.

    Two independent anchors keep it from passing vacuously: the bare-manifest
    tag must genuinely be on the registry, and the version tag must genuinely
    be recorded. The sweep over EVERY json file catches a manifest leaking in
    through any other path.
    """
    from src.helpers import make_package

    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False, index=False)
    leaf_digest = _publish_bare_manifest_tag(registry, pkg.repo, "1.0.0", "9.9.9")
    published = fetch_manifest_raw(registry, pkg.repo, "9.9.9")[0]
    assert json.loads(published)["mediaType"] == IMAGE_MANIFEST_MEDIA_TYPE, (
        "precondition: tag 9.9.9 must serve a bare image manifest"
    )

    ocx.plain("index", "update", pkg.repo)

    index_home = ocx.ocx_home / "index"
    documents = sorted(index_home.rglob("*.json"))
    assert documents, f"index update wrote nothing under {index_home}"

    objects = sorted(index_home.rglob("o/*/*.json"))
    assert len(objects) == 1, f"expected exactly one dispatch object, got {objects}"

    for path in documents:
        media_type = json.loads(path.read_text()).get("mediaType")
        assert media_type != IMAGE_MANIFEST_MEDIA_TYPE, (
            f"{path.relative_to(index_home)} is a leaf platform image manifest; "
            "the local index must only ever hold dispatch objects"
        )
    for path in objects:
        media_type = json.loads(path.read_text()).get("mediaType")
        assert media_type == IMAGE_INDEX_MEDIA_TYPE, (
            f"{path.relative_to(index_home)} has mediaType {media_type!r}, "
            f"expected {IMAGE_INDEX_MEDIA_TYPE}"
        )

    root_document = index_home / registry_dir(registry) / "p" / f"{pkg.repo}.json"
    tags = json.loads(root_document.read_text())["tags"]
    assert list(tags) == ["1.0.0"], (
        f"only the image-index tag may become a root entry; got {sorted(tags)}"
    )
    algorithm, hex_digest = leaf_digest.split(":", 1)
    manifest_object = root_document.with_suffix("") / "o" / algorithm / f"{hex_digest}.json"
    assert not manifest_object.exists(), (
        f"{manifest_object.relative_to(index_home)} exists; a leaf platform manifest is "
        "never copied into the local index"
    )


def test_index_update_of_a_bare_manifest_tag_alone_is_no_indexable_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
):
    """Every candidate tag excluded is `NotFound` (79) with its OWN message —
    not `DataError` (65) and not an unclassified exit 1.

    Nothing here is malformed: the tag resolved, the artifact exists, it is
    simply not a version pointer. 79 is the absent-resource category, and the
    message ("no indexable tag") is what separates "every published tag was
    excluded" from "package absent" — the two share an exit code, so the
    message is the only thing that disambiguates them.
    """
    from src.helpers import make_package

    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False, index=False)
    _publish_bare_manifest_tag(registry, pkg.repo, "1.0.0", "9.9.9")

    result = ocx.plain("index", "update", f"{pkg.repo}:9.9.9", check=False)

    assert result.returncode == 79, (
        f"expected NotFound (79), got rc={result.returncode}\n{result.stderr}"
    )
    assert "no indexable tag" in result.stderr, result.stderr
    assert not list((ocx.ocx_home / "index").rglob("o/*/*.json")), (
        "a refused refresh must persist no dispatch object"
    )


def test_bare_manifest_tag_never_becomes_a_root_entry_through_the_resolve_path(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, registry: str
):
    """Write path 2 of 2 — the resolve-path root-growth branch
    (`ChainedIndex::walk_chain`, the `SourceKind::Derived` arm).

    `ocx index update` is not the only writer: an install resolves a tag on
    the fly and grows the local root from it. That branch applies the SAME
    gate (`local_index::records_root_tag`), and it has to — neither write path
    consults `list_tags`, so the listing filters cannot catch a bad entry
    downstream; a committed violation would be *hidden* rather than absent.

    Non-vacuity: the install must succeed (so the grow branch was genuinely
    reached) and a subsequent install of the version tag must genuinely record
    an entry (so "no entry" is not just "the root was never written").
    """
    from src.helpers import make_package

    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False, index=False)
    _publish_bare_manifest_tag(registry, pkg.repo, "1.0.0", "9.9.9")

    ocx.json("package", "install", f"{pkg.repo}:9.9.9")

    root_document = ocx.ocx_home / "index" / registry_dir(registry) / "p" / f"{pkg.repo}.json"
    recorded = json.loads(root_document.read_text())["tags"] if root_document.is_file() else {}
    assert "9.9.9" not in recorded, (
        f"a tag resolving to a bare manifest must never become a root entry; got {sorted(recorded)}"
    )

    ocx.json("package", "install", f"{pkg.repo}:1.0.0")
    grown = json.loads(root_document.read_text())["tags"]
    assert list(grown) == ["1.0.0"], (
        f"the resolve path must grow the root for the image-index tag only; got {sorted(grown)}"
    )


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


def test_index_catalog_tags_local_mode_empty_listing_succeeds(
    ocx: OcxRunner,
):
    """`ocx index catalog --tags` (no `--remote`, nothing indexed yet) is a
    legitimate empty listing, not a failure — must exit 0.

    Distinguishes "no tags exist locally" (success) from "a fetch failed"
    (see `test_index_catalog_tags_remote_fetch_failure_exits_nonzero`).
    """
    result = ocx.plain("index", "catalog", "--tags")
    assert result.returncode == 0


def test_index_catalog_tags_remote_fetch_failure_exits_nonzero(
    ocx: OcxRunner,
    index_server: static_index.StaticIndexServer,
):
    """`ocx --remote index catalog --tags` must not report SUCCESS with an
    empty or partial catalog when a per-repository tag fetch fails.

    Regression test: the per-repository `list_tags` failure inside the
    `--tags` fan-out used to be logged and swallowed — the command still
    printed a catalog and returned exit 0, so a script consuming
    `ocx --format json index catalog --tags` could not tell "no tags exist"
    from "the fetch failed". Here the catalog listing succeeds (the fixture's
    `c/index.json` lists a real repository name) but that repository's root
    document is malformed, so `list_tags` errors for it; the command must
    now propagate that failure as a nonzero exit.

    The registry namespace is a loopback address with nothing listening
    (`127.0.0.1:1`, the project's established "fast deterministic failure"
    fixture host) so a source-chain fallback attempt fails instantly with no
    real network dependency, rather than risking a slow/flaky DNS lookup.
    """
    namespace = "127.0.0.1:1"
    static_index.write_config(index_server.root)
    static_index.write_catalog(index_server.root, {"broken": f"sha256:{'0' * 64}"})
    broken_root = index_server.root / "p" / "broken.json"
    broken_root.parent.mkdir(parents=True, exist_ok=True)
    broken_root.write_bytes(b"not valid json")

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(f'[registries."{namespace}"]\nindex = "{index_server.base_url}"\n')
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host}"

    result = ocx.plain("--remote", "index", "catalog", "--tags", namespace, check=False)
    assert result.returncode != 0, (
        f"total remote tag-fetch failure must not read as success: {result.stdout}"
    )


# `CATALOG_TAG_CONCURRENCY` in `crates/ocx_cli/src/command/index_catalog.rs`.
# Duplicated rather than imported because the constant is Rust and this suite is
# not; the assertion below names it so a reader can find the pair, and the
# fixture is sized so that a change to either side shows up as a failure rather
# than as slack.
CATALOG_TAG_CONCURRENCY = 16
CATALOG_REPOSITORY_COUNT = 100


def test_index_catalog_tags_bounds_its_in_flight_listings(
    ocx: OcxRunner,
    index_server: static_index.StaticIndexServer,
):
    """`ocx --remote index catalog --tags` fans out over a whole registry's
    repository list, and that fan-out is bounded.

    Every number here is measured, not reasoned. A static file is served far
    faster than the client can fan out, so with no hold the peak overlap is 1
    however wide the loop is — the hold is what makes concurrency observable at
    all, and it has to be long enough that a widened loop actually shows. The
    calibration this borrows (`design_spec_servable_index_snapshot.md`, for the
    sibling fan-out) found 20 ms far too short and 200 ms adequate there. Here
    200 ms was NOT adequate: with the bound raised to 512 the fixture peaked at
    25-35 against a threshold of 32, so the check passed on unbounded code some
    of the time. The limiter is this fixture's own accept path, not the client,
    and a longer hold is what moves it: at 500 ms the same widened build peaked
    at 37, 48 and 55 over three runs while the real bound stayed at 16.

    Ten runs of the real bound gave 16 nine times and 17 once. The permit cap
    is hard at 16, so the 17th is the catalog listing's own handler still
    winding down as the first tasks start — hence a threshold above the
    constant rather than equal to it. It is the module's constant it is derived
    from, NOT the `index` family's stated 512: 100 repositories cannot reach
    512, so asserting that would pass with the semaphore deleted.
    """
    namespace = "127.0.0.1:1"
    repositories = [f"bounded/pkg{number:03d}" for number in range(CATALOG_REPOSITORY_COUNT)]
    for repository in repositories:
        static_index.write_package(
            index_server.root,
            repository=repository,
            tag="1.0.0",
            physical_repository=f"oci://{ocx.registry}/{repository}",
            platform_digest="sha256:" + sha256(repository.encode()).hexdigest(),
        )
    static_index.write_config(index_server.root)
    static_index.write_catalog(
        index_server.root,
        {
            repository: "sha256:" + sha256(repository.encode()).hexdigest()
            for repository in repositories
        },
    )

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(f'[registries."{namespace}"]\nindex = "{index_server.base_url}"\n')
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host}"

    index_server.hold_seconds = 0.5
    catalog = ocx.json("--remote", "index", "catalog", "--tags", namespace)

    # Non-vacuity first, three ways: the run listed every repository, each
    # listing actually resolved its tags off the fixture, and the requests
    # overlapped at all. Without these a run that fanned out over nothing
    # satisfies the bound trivially — a green indistinguishable from the test
    # never having run.
    listed = catalog["repositories"]
    assert len(listed) == CATALOG_REPOSITORY_COUNT, (
        f"precondition: every repository must have been listed, got {len(listed)}"
    )
    assert all(tags == ["1.0.0"] for tags in listed.values()), (
        "precondition: each listing resolved its tags rather than reporting an empty set"
    )
    assert index_server.peak_in_flight > 1, "precondition: some concurrency was observable at all"

    # 24: above the 17 the real bound was measured at, and comfortably below the
    # 37 that was the LOWEST peak a widened build produced. Sitting on 16 exactly
    # would red on the catalog listing's trailing handler; doubling to 32 lands
    # inside the widened build's own range and passes on unbounded code.
    assert index_server.peak_in_flight <= CATALOG_TAG_CONCURRENCY + 8, (
        f"`index catalog --tags` must hold roughly CATALOG_TAG_CONCURRENCY "
        f"({CATALOG_TAG_CONCURRENCY}) tag listings in flight rather than one per repository; "
        f"the fixture saw {index_server.peak_in_flight} over {CATALOG_REPOSITORY_COUNT} repositories"
    )


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
