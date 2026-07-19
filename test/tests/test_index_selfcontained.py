"""Self-containment + verifiability acceptance tests for the local index
collection (`adr_index_indirection.md` Decisions A2/A3/A4/B1/B2/F1/F2).

Covers:

1. A shipped copy resolves a *version choice* (tag -> digest + platform)
   offline, with no network and no `$OCX_HOME/blobs` — locks resolution
   isolate from content install (ADR DR1).
2. Dispatch-only writes: a multi-platform tag persists exactly one dispatch
   object (the image index itself, byte-identical to the registry); a
   single-platform tag (a bare Image manifest, e.g. a canonical
   `sha256.<hex>` tag) persists zero.
3. Every persisted `o/<algo>/<hex>.json` object satisfies
   `sha256(bytes) == <hex>` (the `.json`-suffixed filename strips to the
   hex digest).
4. An absent dispatch object self-heals on the next online `index update`.
5. Copy-a-mirror parity: a raw filesystem copy of a published fixture's
   served tree resolves offline, and the store-written subtree for the
   same package is byte-identical to the fixture's own tree.
6. A deleted `c/index.json` catalog entry (root document untouched)
   self-heals by re-derivation on the next local read — no error.
7. A byte-tampered dispatch object fails an offline read with `DataError`.
8. GC retention (B2): an OCI-derived install keeps its parent image-index
   blob alive through `ocx clean`; an index.ocx.sh-resolved install never
   fetches an image-index blob at all, and nothing wrong is collected.
9. Catalog concurrency: two parallel `ocx index update` runs for distinct
   packages of one source both leave their catalog entry intact.
10. Catalog concurrency (sync x update): a direct `ocx index update`
    races another process's piggyback catalog sync that independently
    re-snapshots the SAME moved package — both land coherently.
11. Catalog concurrency (sync x sync): two concurrent piggyback catalog
    syncs both discover the same brand-new package and race to snapshot
    it — every catalog entry stays self-consistent and the `.etag`
    sidecar stays coherent.
12. Root copy-first (F1): a Default-mode resolve of an already-snapshotted
    package never re-fetches or rewrites the root document; only an
    explicit `ocx index update` bumps `observed`.
13. F1 crash-orphan recovery: an obs object written but root/catalog never
    committed (steps 2-3 pending) self-heals cleanly on the next online
    `ocx index update`, reusing rather than duplicating the orphan.
14. F1 crash-orphan recovery: a root re-written but catalog entry left
    stale (step 3 skipped) self-heals on the next local read by
    re-deriving the entry from the on-disk root — a digest disagreement,
    not merely an absence.

`--index`/`OCX_INDEX`/default (`$OCX_HOME/index`) precedence and layout are
covered in `test_index.py`; this file is the dedicated self-containment +
byte-fidelity + catalog-concurrency suite the ADR calls out as its own
regression surface for #215.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import tomllib
import urllib.error
import urllib.request
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import OcxRunner, PackageInfo, static_index
from src.helpers import make_package
from src.registry import fetch_manifest_digest, fetch_platform_manifest_digest
from src.runner import registry_dir

# ---------------------------------------------------------------------------
# Fixture: a local `index.ocx.sh`-shaped HTTP server, one per test — mirrors
# `test_index_ocx_sh.py::index_server` (module-scoped fixtures do not cross
# file boundaries).
# ---------------------------------------------------------------------------


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def configure_index_source(ocx: OcxRunner, server: static_index.StaticIndexServer) -> None:
    """Points `[registries."ocx.sh"] index` at the fixture and lists its host
    as insecure. Mirrors `test_index_ocx_sh.py::configure_index_source`.
    """
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(f'[registries."ocx.sh"]\nindex = "{server.base_url}"\n')
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{server.host}"


def _fetch_raw_manifest(registry: str, repo: str, ref: str) -> bytes:
    """Fetch the exact bytes the registry serves for `repo:ref`."""
    url = f"http://{registry}/v2/{repo}/manifests/{ref}"
    for media_type in (
        "application/vnd.oci.image.index.v1+json",
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.v2+json",
        "application/vnd.docker.distribution.manifest.list.v2+json",
    ):
        request = urllib.request.Request(url, headers={"Accept": media_type})
        try:
            with urllib.request.urlopen(request, timeout=5) as response:
                return response.read()
        except urllib.error.HTTPError:
            continue
    raise RuntimeError(f"could not fetch manifest bytes for {registry}/{repo}@{ref}")


# ---------------------------------------------------------------------------
# Lock hygiene — no `*.lock` sidecar ever lands inside the index home
# ---------------------------------------------------------------------------


def test_no_lock_litter_in_index_home_after_resolve_and_update(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A resolve and an `ocx index update` against a redirected index home leave
    zero `*.lock` files inside it. Cross-process locks are machine-global under
    `$OCX_HOME/locks`, never sidecars in the (possibly user-committed or
    read-only) index tree.

    Regression for the stale `<repo>.json.lock` sidecar the derived-root commit
    used to leave behind — the exact litter `ocx --index=<dir> package exec`
    dropped on every resolve.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)

    index_home = tmp_path / "index_home"
    index_home.mkdir()

    # A Default-mode resolve commits the derived root document (the old root
    # lock sidecar site), then an explicit refresh re-commits it.
    ocx.plain("--index", str(index_home), "package", "exec", pkg.short, "--", "hello")
    ocx.plain("--index", str(index_home), "index", "update", pkg.short)

    litter = list(index_home.rglob("*.lock"))
    assert not litter, f"the index home must carry no lock sidecars, found: {litter}"


# ---------------------------------------------------------------------------
# #2 — dispatch-only writes: multi-platform = 1 object, single-platform = 0
# ---------------------------------------------------------------------------


def test_dispatch_object_count_multi_platform_one_single_platform_zero(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`ocx index update` writes exactly one dispatch object (the verbatim
    image index, byte-identical to the registry) for a multi-platform tag,
    and zero for a single-platform tag — the on-demand-fetchable leaf is
    never copied into the local index (A3).

    Every OCX-published tag is an image index by construction
    (`Client::push_manifest_and_merge_tags`), so the "single-platform tag"
    case is realized by the registry-side-deletion-safety-net canonical
    `sha256.<hex>` tag (`Client::push_canonical_tag`), which points DIRECTLY
    at the bare platform manifest — a real, product-reachable single-platform
    tag shape, not a synthetic one.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)

    # Multi-platform (ordinary) tag: exactly one dispatch object, the
    # verbatim image index.
    index_dir = tmp_path / "index_dir_multi"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", pkg.short)

    index_bytes = _fetch_raw_manifest(ocx.registry, pkg.repo, pkg.tag)
    index_hex = hashlib.sha256(index_bytes).hexdigest()
    objects = list(index_dir.rglob("o/sha256/*.json"))
    assert len(objects) == 1, f"expected exactly one dispatch object, got {objects}"
    assert objects[0].name == f"{index_hex}.json"
    assert objects[0].read_bytes() == index_bytes, (
        "the persisted dispatch object must be byte-identical to the registry's own image index"
    )

    # Single-platform (canonical `sha256.<hex>`) tag: zero dispatch objects.
    algo, leaf_hex = leaf_digest.split(":", 1)
    canonical_tag = f"{algo}.{leaf_hex}"
    index_dir2 = tmp_path / "index_dir_single"
    index_dir2.mkdir()
    ocx.plain("--index", str(index_dir2), "index", "update", f"{pkg.repo}:{canonical_tag}")

    assert not list(index_dir2.rglob("o/*/*.json")), (
        "a single-platform (bare Image manifest) tag must persist zero dispatch objects"
    )
    root_doc = json.loads(next(index_dir2.rglob(f"{pkg.repo}.json")).read_text())
    assert root_doc["tags"][canonical_tag]["content"] == leaf_digest, (
        "a single-platform tag's content must be the leaf manifest digest itself"
    )


# ---------------------------------------------------------------------------
# #3 — write-side digest self-consistency
# ---------------------------------------------------------------------------


def test_dispatch_objects_hash_to_their_own_filename(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
):
    """Every persisted `o/<algo>/<hex>.json` file satisfies
    `sha256(bytes) == <hex>` once the `.json` suffix is stripped.

    `SnapshotStore::write_dispatch_object` recomputes and verifies the
    digest before the write commits (A3/A4); this is the acceptance-level
    positive control that the recompute-and-verify contract holds
    end-to-end through the real `ocx index update` command.
    """
    pkg = published_package
    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", pkg.short)

    objects = list(index_dir.rglob("o/*/*.json"))
    assert objects, "index update must persist a verbatim dispatch object"
    for obj in objects:
        digest = hashlib.sha256(obj.read_bytes()).hexdigest()
        assert obj.name == f"{digest}.json", f"{obj} does not hash to its own filename"


# ---------------------------------------------------------------------------
# #1 — self-containment: version choice resolves offline, zero network,
#      zero blobs (resolution vs install split, ADR DR1)
# ---------------------------------------------------------------------------


def test_shipped_copy_resolves_version_choice_offline_with_no_network_and_no_blobs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A shipped `.ocx/index/` copy resolves an `ocx.lock`-pinned tool's
    *version choice* (tag -> platform-manifest leaf digest) on a clean
    machine with zero network and zero `$OCX_HOME/blobs` (#215).

    `--no-pull` skips eager materialization so the assertion isolates
    manifest-chain RESOLUTION (what the dispatch object + `select_best` are
    for) from binary CONTENT installation, a distinct machine-global
    concern outside this ADR's self-containment claim (content fetch needs
    network — B2, ADR Consequences "Cold-store content fetch needs
    network").
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)

    # Simulates the project-committed `.ocx/index/`.
    committed_index = tmp_path / "committed_index"
    committed_index.mkdir()
    ocx.plain("--index", str(committed_index), "index", "update", pkg.short)
    assert list(committed_index.rglob(f"{pkg.repo}.json")), "precondition: index update must populate the root document"
    assert list(committed_index.rglob("o/*/*.json")), "precondition: index update must populate the dispatch-object CAS"

    # Ground truth, fetched independently from the still-running registry.
    expected_leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\nhello = "{pkg.fq}"\n')

    # A brand-new, never-touched $OCX_HOME simulates the clean machine.
    clean_home = tmp_path / "clean_home"
    clean_home.mkdir()
    env = dict(ocx.env)
    env["OCX_HOME"] = str(clean_home)

    lock_result = subprocess.run(
        [str(ocx.binary), "--offline", "--index", str(committed_index), "lock", "--no-pull"],
        cwd=project,
        capture_output=True,
        text=True,
        env=env,
    )
    assert lock_result.returncode == 0, (
        "offline lock against a self-contained shipped copy must succeed: "
        f"rc={lock_result.returncode}\nstdout={lock_result.stdout}\nstderr={lock_result.stderr}"
    )

    lock_data = tomllib.loads((project / "ocx.lock").read_text())
    tool = next(t for t in lock_data["tool"] if t["name"] == "hello")
    leaf_digest = tool["platforms"].get("any") or next(iter(tool["platforms"].values()))
    assert leaf_digest == expected_leaf_digest, (
        "offline-resolved platform leaf digest must match the registry's own digest: "
        f"got {leaf_digest}, expected {expected_leaf_digest}"
    )

    assert not (clean_home / "blobs").exists(), (
        "offline lock resolution must not require (or create) $OCX_HOME/blobs"
    )

    # Second, independent consumer of the same shipped copy: a pure index
    # query against a fresh clean home reaches the same platform.
    query_home = tmp_path / "query_home"
    query_home.mkdir()
    query_runner = OcxRunner(ocx.binary, query_home, ocx.registry)
    query_result = query_runner.plain(
        "--offline", "--index", str(committed_index), "index", "list", pkg.short, "--platforms"
    )
    assert pkg.platform in query_result.stdout, (
        f"offline `index list --platforms` must resolve {pkg.platform} from the "
        f"shipped copy alone; got: {query_result.stdout!r}"
    )
    assert not (query_home / "blobs").exists()


# ---------------------------------------------------------------------------
# #4 — absence self-heals on the next online update
# ---------------------------------------------------------------------------


def test_absent_dispatch_object_self_heals_on_next_online_update(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Deleting a dispatch object from an otherwise-populated local index
    self-heals on the next online `ocx index update`: the object is
    re-fetched, re-verified, and re-materialized at the same path.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", pkg.short)

    objects = list(index_dir.rglob("o/*/*.json"))
    assert len(objects) == 1
    dispatch_object = objects[0]
    original_bytes = dispatch_object.read_bytes()
    dispatch_object.unlink()
    assert not dispatch_object.exists(), "precondition: the dispatch object must be gone"

    # Online re-fetch: re-materializes the object at the same path.
    ocx.plain("--index", str(index_dir), "index", "update", pkg.short)
    assert dispatch_object.is_file(), "the dispatch object must be re-materialized on the next online update"
    assert dispatch_object.read_bytes() == original_bytes
    assert hashlib.sha256(dispatch_object.read_bytes()).hexdigest() == dispatch_object.stem


# ---------------------------------------------------------------------------
# #5 — copy-a-mirror parity
# ---------------------------------------------------------------------------


def test_copy_a_mirror_parity_offline_resolve_and_byte_identical_subtree(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """A raw filesystem copy of a published fixture's served tree (`wget
    --mirror` equivalent) resolves offline identically, and OCX's own
    `ocx index update`-written subtree for the same package is
    byte-identical to the fixture's own tree (A2, "copy-a-mirror" — a
    published index copy is a verbatim site copy that verifies against
    itself).
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )

    # 1. Raw filesystem copy ("wget --mirror") of the fixture's served tree
    #    into a fresh index home under the `ocx.sh` source key — no `ocx
    #    index update` involved at all.
    import shutil

    copy_home = tmp_path / "copy_home"
    copy_home.mkdir()
    shutil.copytree(index_server.root, copy_home / "ocx.sh")

    clean_home = tmp_path / "clean_home"
    clean_home.mkdir()
    copy_runner = OcxRunner(ocx.binary, clean_home, ocx.registry)
    result = copy_runner.plain(
        "--offline", "--index", str(copy_home), "index", "list", f"ocx.sh/{repository}:1.0.0", "--platforms"
    )
    assert pkg.platform in result.stdout, "a raw copy of the fixture's served tree must resolve offline identically"

    # 2. OCX's own written subtree for the same package equals the fixture's
    #    own tree, byte-for-byte, for the same package's root + dispatch
    #    object.
    written_home = tmp_path / "written_home"
    written_home.mkdir()
    ocx.plain("--index", str(written_home), "index", "update", f"ocx.sh/{repository}:1.0.0")

    fixture_root_bytes = (index_server.root / "p" / f"{repository}.json").read_bytes()
    written_root_bytes = (written_home / "ocx.sh" / "p" / f"{repository}.json").read_bytes()
    assert written_root_bytes == fixture_root_bytes, "the written root document must byte-equal the fixture's own"

    fixture_obs_dir = index_server.root / "p" / repository / "o" / "sha256"
    written_obs_dir = written_home / "ocx.sh" / "p" / repository / "o" / "sha256"
    fixture_obs_files = sorted(p.name for p in fixture_obs_dir.iterdir())
    written_obs_files = sorted(p.name for p in written_obs_dir.iterdir())
    assert fixture_obs_files == written_obs_files
    for name in fixture_obs_files:
        assert (fixture_obs_dir / name).read_bytes() == (written_obs_dir / name).read_bytes(), (
            f"dispatch object {name} must byte-equal the fixture's own copy"
        )


# ---------------------------------------------------------------------------
# #6 — catalog-entry-delete self-heals on next local read
# ---------------------------------------------------------------------------


def test_catalog_entry_delete_self_heals_on_next_local_read(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """Deleting one package's `c/index.json` catalog entry (the root
    document itself untouched) self-heals by re-derivation on the next
    LOCAL read — no error, the entry is restored from the on-disk root
    bytes (F1 read-path recovery, `SnapshotStore::read_root`).

    Not run under `--offline`: `ChainedIndex::kind_for` provenance-detects
    a namespace as `Published` only through a CONFIGURED source in
    `self.sources`, which is empty by construction under `--offline`
    (`Context::try_init` builds no `remote_client`, so `build_index_source`
    returns `None`). An absent kind falls back to `SourceKind::Derived`,
    which reads the same root-document bytes but skips the catalog
    cross-check/self-heal entirely (`kind_for`'s own doc comment). The read
    below still touches no network — it is a pure local-cache hit — the
    fixture just needs to stay configured so kind detection resolves
    `Published`.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    static_index.write_catalog(index_server.root, {repository: entry.root_digest})

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)

    catalog_path = index_dir / "ocx.sh" / "c" / "index.json"
    catalog = json.loads(catalog_path.read_text())
    assert repository in catalog, "precondition: the catalog must carry the package's entry"
    del catalog[repository]
    catalog_path.write_text(json.dumps(catalog))

    # A pure local read (tag listing — no fetch needed, everything is
    # already cached) must succeed without error and restore the deleted
    # entry by re-deriving it from the on-disk root bytes.
    result = ocx.plain("--index", str(index_dir), "index", "list", entry.logical_id)
    assert result.returncode == 0
    assert "1.0.0" in result.stdout

    restored = json.loads(catalog_path.read_text())
    assert restored.get(repository) == entry.root_digest, (
        "the deleted catalog entry must be restored (re-derived) by the read path"
    )


# ---------------------------------------------------------------------------
# #7 — read path: a tampered object must never silently load
# ---------------------------------------------------------------------------


def test_tampered_dispatch_object_fails_offline_read_with_dataerror(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", pkg.short)

    objects = list(index_dir.rglob("o/*/*.json"))
    assert len(objects) == 1
    dispatch_object = objects[0]
    original = dispatch_object.read_bytes()
    dispatch_object.write_bytes(b"TAMPERED" * 4)
    try:
        clean_home = tmp_path / "clean_home"
        clean_home.mkdir()
        clean_runner = OcxRunner(ocx.binary, clean_home, ocx.registry)
        result = clean_runner.plain(
            "--offline", "--index", str(index_dir), "index", "list", pkg.short, "--platforms", check=False
        )
        assert result.returncode == 65, (
            "a byte-tampered dispatch object must fail the read with DataError (65), "
            f"got rc={result.returncode}\nstdout={result.stdout!r}\nstderr={result.stderr!r}"
        )
    finally:
        dispatch_object.write_bytes(original)


# ---------------------------------------------------------------------------
# #8 — GC retention (B2)
# ---------------------------------------------------------------------------


def test_gc_retention_oci_derived_keeps_index_blob_index_resolved_adds_no_edge(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """B2 GC-retention regression: an OCI-derived (plain-registry) install
    keeps its parent image-index blob alive through `ocx clean`
    (`add_index_retention_edges`, unchanged); an index.ocx.sh-resolved
    install never fetches an image-index blob at all (Decision C bypasses
    the hop) — nothing wrong is collected, and the package still execs
    after `clean`.
    """
    # --- OCI-derived leg: the index blob is retained -----------------------
    derived_home = tmp_path / "derived_home"
    derived_home.mkdir()
    derived = OcxRunner(ocx.binary, derived_home, ocx.registry)
    pkg = make_package(derived, unique_repo, "1.0.0", tmp_path, new=True)
    derived.json("package", "install", pkg.short)

    index_digest = fetch_manifest_digest(derived.registry, pkg.repo, pkg.tag)
    algo, hex_digest = index_digest.split(":", 1)
    index_blob = (
        derived_home / "blobs" / registry_dir(derived.registry) / algo / hex_digest[:2] / hex_digest[2:32] / "data"
    )
    assert index_blob.is_file(), "precondition: install must cache the outer image-index blob"

    derived.plain("clean")
    assert index_blob.is_file(), "add_index_retention_edges must keep a live leaf's parent index alive after clean"

    # --- index.ocx.sh-resolved leg: no image-index blob ever exists --------
    unique_repo2 = f"{unique_repo}b"
    pkg2 = make_package(ocx, unique_repo2, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg2.repo, pkg2.tag)
    os_name, arch_name = pkg2.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo2}/pkg"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg2.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    index_dir = tmp_path / "index_dir_gc"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "package", "install", entry.logical_id)

    ocx_sh_blobs = Path(ocx.env["OCX_HOME"]) / "blobs" / registry_dir("ocx.sh")
    for data_file in ocx_sh_blobs.rglob("data"):
        try:
            media_type = json.loads(data_file.read_bytes()).get("mediaType", "")
        except (json.JSONDecodeError, UnicodeDecodeError):
            continue
        assert "image.index" not in media_type, (
            f"an index.ocx.sh-resolved install must never fetch an image-index blob: {data_file}"
        )

    ocx.plain("--index", str(index_dir), "clean")

    # Nothing wrong was collected — the package still execs (online) after clean.
    result = ocx.plain("--index", str(index_dir), "package", "exec", entry.logical_id, "--", "hello")
    assert pkg2.marker in result.stdout


# ---------------------------------------------------------------------------
# #9 — catalog concurrency: two parallel `index update` runs, distinct
#      packages of the SAME source, both entries survive
# ---------------------------------------------------------------------------


def test_catalog_concurrent_updates_of_distinct_packages_both_entries_survive(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """Two `ocx index update` invocations racing against ONE source's
    `c/index.json` — each for a distinct package — must both leave their
    catalog entry intact. The single catalog-transaction contract
    (`SnapshotStore::begin_catalog_transaction`, source-scoped lock,
    re-read-then-reconcile before commit) prevents the classic lost-update:
    a wholesale replace from a stale pre-lock read would silently drop
    whichever writer committed second.
    """
    pkg_a = make_package(ocx, f"{unique_repo}a", "1.0.0", tmp_path, new=True, index=False)
    pkg_b = make_package(ocx, f"{unique_repo}b", "1.0.0", tmp_path, new=True, index=False)
    leaf_a = fetch_platform_manifest_digest(ocx.registry, pkg_a.repo, pkg_a.tag)
    leaf_b = fetch_platform_manifest_digest(ocx.registry, pkg_b.repo, pkg_b.tag)
    os_a, arch_a = pkg_a.platform.split("/")
    os_b, arch_b = pkg_b.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repo_a = f"{unique_repo}/pkga"
    repo_b = f"{unique_repo}/pkgb"
    static_index.write_package(
        index_server.root,
        repository=repo_a,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg_a.repo}",
        platform_digest=leaf_a,
        os=os_a,
        architecture=arch_a,
    )
    static_index.write_package(
        index_server.root,
        repository=repo_b,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg_b.repo}",
        platform_digest=leaf_b,
        os=os_b,
        architecture=arch_b,
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    logical_a = f"ocx.sh/{repo_a}:1.0.0"
    logical_b = f"ocx.sh/{repo_b}:1.0.0"

    proc_a = subprocess.Popen(
        [str(ocx.binary), "--index", str(index_dir), "index", "update", logical_a],
        env=ocx.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    proc_b = subprocess.Popen(
        [str(ocx.binary), "--index", str(index_dir), "index", "update", logical_b],
        env=ocx.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    out_a, err_a = proc_a.communicate(timeout=30)
    out_b, err_b = proc_b.communicate(timeout=30)
    assert proc_a.returncode == 0, f"concurrent update A failed: {err_a}"
    assert proc_b.returncode == 0, f"concurrent update B failed: {err_b}"

    catalog_path = index_dir / "ocx.sh" / "c" / "index.json"
    catalog = json.loads(catalog_path.read_text())
    assert repo_a in catalog, f"catalog must retain repo_a's entry after concurrent updates: {catalog}"
    assert repo_b in catalog, f"catalog must retain repo_b's entry after concurrent updates: {catalog}"

    # Both root documents must also be present — the per-package upsert
    # itself must not have been lost.
    assert (index_dir / "ocx.sh" / "p" / f"{repo_a}.json").is_file()
    assert (index_dir / "ocx.sh" / "p" / f"{repo_b}.json").is_file()


# ---------------------------------------------------------------------------
# #10 — catalog concurrency: a direct update races the OTHER process's
#       piggyback catalog sync re-snapshotting the SAME moved package
# ---------------------------------------------------------------------------


def test_catalog_sync_race_direct_update_vs_piggyback_resnapshot_same_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """A direct `ocx index update <pkgA>` races ANOTHER process's piggyback
    catalog sync that independently re-snapshots the SAME moved package as a
    side effect of refreshing `pkgB`. Both write paths must land pkgA's root
    + catalog entry coherently — this is exactly the sol-gate finding the
    single catalog-transaction contract closes: a sync's read-then-replace
    path racing a per-package upsert must never lose either write.
    """
    pkg_a = make_package(ocx, f"{unique_repo}a", "1.0.0", tmp_path, new=True, index=False)
    pkg_b = make_package(ocx, f"{unique_repo}b", "1.0.0", tmp_path, new=True, index=False)
    leaf_a = fetch_platform_manifest_digest(ocx.registry, pkg_a.repo, pkg_a.tag)
    leaf_b = fetch_platform_manifest_digest(ocx.registry, pkg_b.repo, pkg_b.tag)
    os_a, arch_a = pkg_a.platform.split("/")
    os_b, arch_b = pkg_b.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repo_a = f"{unique_repo}/pkga"
    repo_b = f"{unique_repo}/pkgb"
    entry_a1 = static_index.write_package(
        index_server.root,
        repository=repo_a,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg_a.repo}",
        platform_digest=leaf_a,
        os=os_a,
        architecture=arch_a,
    )
    entry_b = static_index.write_package(
        index_server.root,
        repository=repo_b,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg_b.repo}",
        platform_digest=leaf_b,
        os=os_b,
        architecture=arch_b,
    )
    static_index.write_catalog(index_server.root, {repo_a: entry_a1.root_digest, repo_b: entry_b.root_digest})

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    logical_a = f"ocx.sh/{repo_a}:1.0.0"
    logical_b = f"ocx.sh/{repo_b}:1.0.0"

    # First sync establishes both packages + the catalog + etag baseline.
    ocx.plain("--index", str(index_dir), "index", "update", logical_a)

    # Move pkgA's root (new status -> new root digest) and update the served
    # catalog so BOTH the direct update and pkgB's piggyback see it as moved.
    entry_a2 = static_index.write_package(
        index_server.root,
        repository=repo_a,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg_a.repo}",
        platform_digest=leaf_a,
        os=os_a,
        architecture=arch_a,
        status="deprecated",
        deprecated_message="moved",
    )
    static_index.write_catalog(index_server.root, {repo_a: entry_a2.root_digest, repo_b: entry_b.root_digest})

    proc_direct = subprocess.Popen(
        [str(ocx.binary), "--index", str(index_dir), "index", "update", logical_a],
        env=ocx.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    proc_piggyback = subprocess.Popen(
        [str(ocx.binary), "--index", str(index_dir), "index", "update", logical_b],
        env=ocx.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    _, err_direct = proc_direct.communicate(timeout=30)
    _, err_piggyback = proc_piggyback.communicate(timeout=30)
    assert proc_direct.returncode == 0, f"direct update failed: {err_direct}"
    assert proc_piggyback.returncode == 0, f"piggyback update failed: {err_piggyback}"

    # Internal consistency: pkgA's persisted catalog entry must equal
    # sha256(actual on-disk root bytes) — never a torn/stale straddle.
    root_a_path = index_dir / "ocx.sh" / "p" / f"{repo_a}.json"
    catalog_path = index_dir / "ocx.sh" / "c" / "index.json"
    root_a_bytes = root_a_path.read_bytes()
    expected_entry = f"sha256:{hashlib.sha256(root_a_bytes).hexdigest()}"
    catalog = json.loads(catalog_path.read_text())
    assert catalog.get(repo_a) == expected_entry, (
        "pkgA's catalog entry must match its actual on-disk root bytes after the race, "
        f"got {catalog.get(repo_a)!r}, expected {expected_entry!r}"
    )
    assert catalog.get(repo_b) is not None, "pkgB's own entry must survive the race"

    # The root itself must reflect the MOVED (deprecated) content, not the
    # stale pre-race version — proves the direct update's own write was not
    # silently lost either.
    root_a = json.loads(root_a_bytes)
    assert root_a.get("status") == "deprecated", "pkgA's root must reflect the moved (post-race) content"


# ---------------------------------------------------------------------------
# #11 — catalog concurrency: two concurrent piggyback syncs both discover
#       the same brand-new package and race to record it as a listing row
# ---------------------------------------------------------------------------


def test_catalog_sync_race_two_concurrent_syncs_keep_catalog_and_etag_coherent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """Two `ocx index update` calls for DIFFERENT already-known packages
    both piggyback a whole-catalog sync at the same time; both also
    discover a brand-new THIRD package in the served catalog and race to
    record it. Under the F2 listing-row contract the brand-new package is
    recorded as a LISTING ROW (its served catalog digest) WITHOUT being
    materialized — a package new to the local catalog is fetched only when
    first `update`d. The materialized packages' catalog entries must match
    their own on-disk root bytes — no corrupted/torn write from the
    concurrent reconcile-commits — and the `.etag` sidecar stays coherent
    (a follow-up sync against the unchanged fixture answers 304).
    """
    pkg_a = make_package(ocx, f"{unique_repo}a", "1.0.0", tmp_path, new=True, index=False)
    pkg_b = make_package(ocx, f"{unique_repo}b", "1.0.0", tmp_path, new=True, index=False)
    pkg_c = make_package(ocx, f"{unique_repo}c", "1.0.0", tmp_path, new=True, index=False)
    leaf_a = fetch_platform_manifest_digest(ocx.registry, pkg_a.repo, pkg_a.tag)
    leaf_b = fetch_platform_manifest_digest(ocx.registry, pkg_b.repo, pkg_b.tag)
    leaf_c = fetch_platform_manifest_digest(ocx.registry, pkg_c.repo, pkg_c.tag)
    os_a, arch_a = pkg_a.platform.split("/")
    os_b, arch_b = pkg_b.platform.split("/")
    os_c, arch_c = pkg_c.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repo_a = f"{unique_repo}/pkga"
    repo_b = f"{unique_repo}/pkgb"
    repo_c = f"{unique_repo}/pkgc"
    entry_a = static_index.write_package(
        index_server.root,
        repository=repo_a,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg_a.repo}",
        platform_digest=leaf_a,
        os=os_a,
        architecture=arch_a,
    )
    entry_b = static_index.write_package(
        index_server.root,
        repository=repo_b,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg_b.repo}",
        platform_digest=leaf_b,
        os=os_b,
        architecture=arch_b,
    )
    static_index.write_catalog(index_server.root, {repo_a: entry_a.root_digest, repo_b: entry_b.root_digest})

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    logical_a = f"ocx.sh/{repo_a}:1.0.0"
    logical_b = f"ocx.sh/{repo_b}:1.0.0"

    # Establish pkgA + pkgB locally (baseline catalog + etag).
    ocx.plain("--index", str(index_dir), "index", "update", logical_a)
    ocx.plain("--index", str(index_dir), "index", "update", logical_b)

    # A brand-new pkgC appears in the served catalog — neither concurrent
    # call names it directly, so only the piggyback sync can discover it.
    entry_c = static_index.write_package(
        index_server.root,
        repository=repo_c,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg_c.repo}",
        platform_digest=leaf_c,
        os=os_c,
        architecture=arch_c,
    )
    static_index.write_catalog(
        index_server.root,
        {repo_a: entry_a.root_digest, repo_b: entry_b.root_digest, repo_c: entry_c.root_digest},
    )

    proc_a = subprocess.Popen(
        [str(ocx.binary), "--index", str(index_dir), "index", "update", logical_a],
        env=ocx.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    proc_b = subprocess.Popen(
        [str(ocx.binary), "--index", str(index_dir), "index", "update", logical_b],
        env=ocx.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    _, err_a = proc_a.communicate(timeout=30)
    _, err_b = proc_b.communicate(timeout=30)
    assert proc_a.returncode == 0, f"concurrent sync A failed: {err_a}"
    assert proc_b.returncode == 0, f"concurrent sync B failed: {err_b}"

    catalog_path = index_dir / "ocx.sh" / "c" / "index.json"
    etag_path = index_dir / "ocx.sh" / "c" / "index.json.etag"
    assert etag_path.is_file() and etag_path.read_text().strip(), (
        "the .etag sidecar must survive the concurrent reconcile-commits, non-empty"
    )

    catalog = json.loads(catalog_path.read_text())

    # pkgA and pkgB were materialized by the named `index update` calls: each
    # catalog entry must match its OWN on-disk root bytes (no torn write from the
    # concurrent reconcile-commits).
    for repository in (repo_a, repo_b):
        assert repository in catalog, f"{repository} must survive the concurrent sync race: {catalog}"
        root_bytes = (index_dir / "ocx.sh" / "p" / f"{repository}.json").read_bytes()
        expected_entry = f"sha256:{hashlib.sha256(root_bytes).hexdigest()}"
        assert catalog[repository] == expected_entry, (
            f"{repository}'s catalog entry must match its own on-disk root bytes "
            f"after the concurrent sync race, got {catalog[repository]!r}, expected {expected_entry!r}"
        )

    # pkgC was never named on the command line and had no local root before the
    # race — the piggyback records it as a LISTING ROW (its served catalog
    # digest) WITHOUT materializing its root (F2: materialized only when first
    # `update`d). The race must still land that listing row coherently.
    assert catalog.get(repo_c) == entry_c.root_digest, (
        f"pkgC must be recorded as a listing row carrying its served catalog digest, got {catalog.get(repo_c)!r}"
    )
    assert not (index_dir / "ocx.sh" / "p" / f"{repo_c}.json").is_file(), (
        "pkgC must NOT be materialized by the piggyback — it is a listing row until first updated (F2)"
    )

    # Coherence proof: a follow-up sync against the UNCHANGED fixture must
    # answer 304 — the locally stored etag genuinely matches the served
    # catalog's current state, not a torn write from the race.
    checkpoint = len(index_server.requests)
    ocx.plain("--index", str(index_dir), "index", "update", logical_a)
    since = index_server.requests[checkpoint:]
    catalog_requests = [record for record in since if record.path.endswith("/c/index.json")]
    assert catalog_requests, "the follow-up run must still send the conditional GET"
    assert catalog_requests[-1].status == 304, (
        "the etag persisted by the concurrent race must validate against the unchanged "
        f"fixture (304), got status {catalog_requests[-1].status}"
    )


# ---------------------------------------------------------------------------
# #12 — root copy-first (F1): Default-mode resolve never re-fetches an
#       already-snapshotted root; only `ocx index update` bumps `observed`
# ---------------------------------------------------------------------------


def test_default_mode_resolve_never_refetches_root_observed_bumps_only_on_update(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Root copy-first (F1): a Default-mode resolve of an ALREADY-snapshotted
    package (`ocx package install`) never re-fetches or rewrites the root
    document — the on-disk bytes (including `observed`) stay byte-identical.
    Only an explicit `ocx index update` bumps `observed`.

    Uses a plain OCI (derived) source: `ChainedIndex::fetch_manifest`'s
    tag-addressed path answers straight from a `Dispatch` cache hit without
    ever reaching `commit_root_tag` — root-copy-first holds structurally
    for both source kinds, this is the simpler one to drive without an
    index.ocx.sh fixture.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", pkg.short)

    root_path = index_dir / registry_dir(ocx.registry) / "p" / f"{pkg.repo}.json"
    assert root_path.is_file(), "precondition: index update must persist the root document"
    before_bytes = root_path.read_bytes()
    before_observed = json.loads(before_bytes)["tags"][pkg.tag]["observed"]

    # A Default-mode resolve of the ALREADY-cached tag (a `package install`)
    # must not touch the root document at all.
    ocx.plain("--index", str(index_dir), "package", "install", pkg.short)
    after_install_bytes = root_path.read_bytes()
    assert after_install_bytes == before_bytes, (
        "a Default-mode resolve of an already-snapshotted package must not rewrite "
        "the root document — observed must stay exactly as it was"
    )

    # Only the explicit refresh path bumps `observed`.
    ocx.plain("--index", str(index_dir), "index", "update", pkg.short)
    after_update = json.loads(root_path.read_text())
    after_observed = after_update["tags"][pkg.tag]["observed"]
    assert after_observed != before_observed, (
        f"ocx index update must bump observed, but it stayed {before_observed!r}"
    )


def test_orphan_dispatch_object_without_root_self_heals_on_next_online_update(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """F1 crash-orphan recovery — dispatch object written, root/catalog
    never committed (a crash strictly between write-order steps 1 and 2).

    Hand-crafts the interrupted state directly on disk (bypassing the CLI
    entirely, so no real crash is needed): only the obs object under `o/`
    exists; the root document and its catalog entry were never written. F1:
    "an orphan left by an aborted write is harmless — nothing points at it
    yet." A pure local read against this state must not error or corrupt
    anything (the package is simply unknown locally — the root never
    landed); the next ONLINE `ocx index update` must complete cleanly and
    leave a fully consistent root + catalog, reusing the orphaned object
    rather than duplicating it.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    static_index.write_catalog(index_server.root, {repository: entry.root_digest})

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()

    # Hand-craft ONLY write-order step 1: the obs object exists under `o/`;
    # the root document (step 2) and catalog entry (step 3) were never
    # written.
    obs_hex = entry.obs_digest.split(":", 1)[1]
    obs_src = index_server.root / "p" / repository / "o" / "sha256" / f"{obs_hex}.json"
    obs_dst = index_dir / "ocx.sh" / "p" / repository / "o" / "sha256" / f"{obs_hex}.json"
    obs_dst.parent.mkdir(parents=True, exist_ok=True)
    orphan_bytes = obs_src.read_bytes()
    obs_dst.write_bytes(orphan_bytes)
    root_path = index_dir / "ocx.sh" / "p" / f"{repository}.json"
    catalog_path = index_dir / "ocx.sh" / "c" / "index.json"
    assert not root_path.exists(), "precondition: the root document must not exist yet"

    # A pure local read against the orphan-only state: no crash, no error —
    # the package is simply unknown locally (root never landed), a clean
    # miss.
    read_result = ocx.plain(
        "--offline", "--index", str(index_dir), "index", "list", entry.logical_id, check=False
    )
    assert read_result.returncode == 0, (
        f"a local read over an orphan-only state must not error, got rc={read_result.returncode}\n"
        f"stderr:\n{read_result.stderr}"
    )
    assert entry.tag not in read_result.stdout, "the tag cannot be known before the root ever lands"

    # The next ONLINE index operation — an explicit re-sync — must complete
    # cleanly and produce a fully consistent root + catalog.
    update_result = ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id, check=False)
    assert update_result.returncode == 0, (
        f"the next online update must self-heal cleanly, got rc={update_result.returncode}\n"
        f"stderr:\n{update_result.stderr}"
    )
    assert root_path.is_file(), "the root document must now be materialized"
    assert catalog_path.is_file(), "the catalog entry must now be materialized"

    root_bytes = root_path.read_bytes()
    expected_entry = f"sha256:{hashlib.sha256(root_bytes).hexdigest()}"
    catalog = json.loads(catalog_path.read_text())
    assert catalog.get(repository) == expected_entry, "the catalog entry must match the newly-written root's own bytes"

    # The orphan is reused, not duplicated: exactly one dispatch object
    # remains, byte-identical to the one hand-crafted before the update.
    objects = list((index_dir / "ocx.sh" / "p" / repository / "o" / "sha256").glob("*.json"))
    assert len(objects) == 1, f"the orphan must be reused, not duplicated: {objects}"
    assert objects[0].read_bytes() == orphan_bytes, "the reused dispatch object must be byte-identical to the orphan"


def test_root_updated_catalog_stale_self_heals_on_next_local_read(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """F1 crash-orphan recovery — root document re-written, catalog entry
    never re-committed (a crash strictly between write-order steps 2 and 3).

    After a clean initial sync, hand-crafts a NEW root document directly on
    disk (bypassing the CLI, as a re-tag / `repository`-migration re-sync
    would leave mid-write) while leaving the catalog entry pointing at the
    OLD root digest — a digest DISAGREEMENT, not an absence (the sharper
    case `test_catalog_entry_delete_self_heals_on_next_local_read` above
    does not cover). F1: "A read-path mismatch between a stored root and
    its catalog entry is ... an inconsistency ... it recovers
    deterministically: re-derive the entry from the root bytes actually on
    disk and rewrite the catalog to match." The next LOCAL read must
    self-heal without error, corruption, or crash — not just skip the write
    because the key happens to be present.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    static_index.write_catalog(index_server.root, {repository: entry.root_digest})

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)

    catalog_path = index_dir / "ocx.sh" / "c" / "index.json"
    root_path = index_dir / "ocx.sh" / "p" / f"{repository}.json"
    stale_entry = json.loads(catalog_path.read_text())[repository]
    assert stale_entry == entry.root_digest, "precondition: the catalog must match the initial root"

    # Hand-craft "root updated, catalog stale": overwrite the root document
    # directly (bypassing the CLI) with new content — the SAME tag
    # re-pointed at a different leaf digest, as a `repository` migration or
    # re-tag would — while leaving the catalog entry pointing at the OLD
    # root digest. No `ocx index update` runs between the two root
    # generations, so the catalog is never touched — exactly the "step 2
    # landed, step 3 pending" straddle.
    fake_leaf_digest = "sha256:" + ("b" * 64)
    static_index.write_package(
        index_dir / "ocx.sh",
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=fake_leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    new_root_bytes = root_path.read_bytes()
    expected_entry = f"sha256:{hashlib.sha256(new_root_bytes).hexdigest()}"
    assert expected_entry != stale_entry, "precondition: the new root must hash differently from the stale entry"
    assert json.loads(catalog_path.read_text())[repository] == stale_entry, (
        "precondition: hand-crafting the root alone must not touch the catalog"
    )

    # The next LOCAL read (deliberately NOT --offline: mirrors
    # test_catalog_entry_delete_self_heals_on_next_local_read's documented
    # pitfall — under --offline no remote_client is built, so
    # ChainedIndex::kind_for cannot detect this namespace as Published and
    # falls back to Derived, which skips the catalog cross-check/self-heal
    # entirely. This read is still a pure local-cache hit; no network round
    # trip occurs since everything needed is already on disk) must succeed
    # and self-heal the catalog entry to match the actual on-disk root.
    read_result = ocx.plain("--index", str(index_dir), "index", "list", entry.logical_id, check=False)
    assert read_result.returncode == 0, (
        f"a root/catalog digest disagreement must never be a hard error, got rc={read_result.returncode}\n"
        f"stderr:\n{read_result.stderr}"
    )
    assert "1.0.0" in read_result.stdout, "the read must reflect the actual on-disk root, not the stale catalog"

    healed = json.loads(catalog_path.read_text())[repository]
    assert healed == expected_entry, (
        "a stale-but-present catalog entry must self-heal to match the on-disk root, "
        f"got {healed!r}, expected {expected_entry!r}"
    )
