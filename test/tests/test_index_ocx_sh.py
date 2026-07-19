# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the `index.ocx.sh` client (`adr_index_indirection.md`
Decision F): two-hop resolve (root -> sha256-verified obs -> physical
manifest), catalog sync (F2), status surfacing (F3), and the `[registries]`/
`[mirrors]` config surfaces (F5) against a local static-file HTTP fixture that
encodes the frozen ● wire shapes.

Ground truth for the wire shapes: `IndexRoot`, `RootTag`, `Observation`,
`ObservationPlatform`, `IndexFormatConfig`, `CatalogIndex` in
`crates/ocx_lib/src/oci/index/wire.rs` (`IndexFormatConfig`/`CatalogSyncOutcome`
in `crates/ocx_lib/src/oci/index/ocx_index.rs`).

The `[registries."ocx.sh"] index = "<url>"` config-writing mechanism mirrors
`test_oci_registry_mirror.py::write_home_config`; the fixture server's
readiness wait mirrors `test/conftest.py::start_registry`.

Commands route through `ocx index update` (never `ocx package install`) for
every namespace-scoped assertion: `IndexUpdate::execute` dispatches an
`ocx.sh`-registered identifier through a single bare `Index::from_source`
(`crates/ocx_cli/src/command/index_update.rs`), never the `ChainedIndex`
fallback to the real registry `OciIndex` that `default_index()` would
build — so an index-side failure (tamper, bad format version, yank refusal)
can never spill into a live network call against the production
`ocx.sh`/`index.ocx.sh` hosts.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import tomllib
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import static_index
from src.assertions import assert_symlink_exists
from src.helpers import make_package
from src.registry import clone_manifest_chain, fetch_platform_manifest_digest
from src.runner import OcxRunner, registry_dir


# ---------------------------------------------------------------------------
# Fixture: a local `index.ocx.sh`-shaped HTTP server, one per test
# ---------------------------------------------------------------------------


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


def configure_index_source(
    ocx: OcxRunner, server: static_index.StaticIndexServer, namespace: str = "ocx.sh"
) -> None:
    """Points `[registries."<namespace>"] index` at the fixture and lists its
    host as insecure. `index` field PRESENCE is the sole protocol-kind marker,
    per NAMESPACE (`adr_index_indirection.md` F5a) — an entry without it
    resolves as plain OCI, no probing. `namespace` defaults to `ocx.sh` but any
    configured namespace resolves through its own index source.
    """
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(f'[registries."{namespace}"]\nindex = "{server.base_url}"\n')
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{server.host}"


def _source_dir(index_dir: Path, namespace: str = "ocx.sh") -> Path:
    """The `<namespace>` source subtree under a redirected index home (A2).

    The subtree slug preserves dots (`to_relaxed_slug`), so `ocx.sh` and
    `corp.example` map to same-named directories.
    """
    return index_dir / namespace


def _root_document_path(
    index_dir: Path, repository: str, namespace: str = "ocx.sh"
) -> Path:
    return _source_dir(index_dir, namespace) / "p" / f"{repository}.json"


def _dispatch_object_path(
    index_dir: Path, repository: str, hex_digest: str, namespace: str = "ocx.sh"
) -> Path:
    return (
        _source_dir(index_dir, namespace)
        / "p"
        / repository
        / "o"
        / "sha256"
        / f"{hex_digest}.json"
    )


def _leaf_blob_data_file(ocx_home: Path, leaf_digest: str) -> Path:
    """The machine-global blob-store `data` file for a leaf platform manifest.

    Layout is `blobs/<registry_slug>/sha256/<hex[0:2]>/<hex[2:32]>/data`; the
    registry slug is the physical push registry, so glob across it rather than
    hardcode the slug.
    """
    hex_digest = leaf_digest.split(":", 1)[1]
    matches = list(
        (ocx_home / "blobs").glob(f"*/sha256/{hex_digest[:2]}/{hex_digest[2:32]}/data")
    )
    assert len(matches) == 1, f"expected exactly one leaf blob data file, found {matches}"
    return matches[0]


# ---------------------------------------------------------------------------
# Lock hygiene — a published-source catalog sync leaves no `*.lock` sidecar
# ---------------------------------------------------------------------------


def test_no_lock_litter_in_index_home_after_catalog_sync(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """A published-source `ocx index update` — which takes the catalog
    transaction lock — leaves zero `*.lock` files inside the index home.

    Regression for the stale `c/index.json.lock` catalog sidecar: the lock is
    now machine-global under `$OCX_HOME/locks`, keyed on the per-source
    directory's file identity, never written into the (possibly committed or
    read-only) index tree.
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

    index_home = tmp_path / "index_home"
    index_home.mkdir()
    ocx.plain("--index", str(index_home), "index", "update", entry.logical_id)

    litter = list(index_home.rglob("*.lock"))
    assert not litter, f"the published-source index home must carry no lock sidecars, found: {litter}"


# ---------------------------------------------------------------------------
# 1 (+8) — two-hop resolve end-to-end, offline self-containment,
#          [registries] override authority
# ---------------------------------------------------------------------------


def test_two_hop_resolve_snapshots_offline_and_hits_only_the_fixture(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """`ocx index update` two-hop resolves through the fixture (root -> obs,
    sha256-verified -> physical manifest from the local registry), and the
    resulting local index resolves the same package fully offline afterwards.

    Also covers item #8 ([registries] override authority): every root/obs/
    config request in this flow lands on the fixture, never the default
    `https://index.ocx.sh`.
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

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)

    # Every hop landed on the fixture (item #8) — root, obs, and the
    # config.json probe all resolved through the configured override.
    requested_paths = [record.path for record in index_server.requests]
    assert any(path.endswith("/config.json") for path in requested_paths)
    assert any(path.endswith(f"/p/{repository}.json") for path in requested_paths)
    obs_hex = entry.obs_digest.split(":", 1)[1]
    assert any(f"/o/sha256/{obs_hex}.json" in path for path in requested_paths)

    # Self-contained afterwards: the root document + the obs dispatch object,
    # under the `ocx.sh` source subtree (A2 — dots preserved by slugify).
    assert _root_document_path(index_dir, repository).is_file()
    obs_object = _dispatch_object_path(index_dir, repository, obs_hex)
    assert obs_object.is_file()
    assert hashlib.sha256(obs_object.read_bytes()).hexdigest() == obs_hex

    # Offline re-resolve: zero network, resolves through the local index
    # alone (mirrors test_index_selfcontained.py's self-containment check).
    clean_home = tmp_path / "clean_home"
    clean_home.mkdir()
    offline_runner = OcxRunner(ocx.binary, clean_home, ocx.registry)
    result = offline_runner.plain(
        "--offline",
        "--index",
        str(index_dir),
        "index",
        "list",
        entry.logical_id,
        "--platforms",
    )
    assert pkg.platform in result.stdout
    assert not (clean_home / "blobs").exists()


def test_two_hop_resolve_under_a_non_ocx_sh_namespace(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """Index-kind selection is per NAMESPACE (`adr_index_indirection.md` F5a):
    a `[registries."<other-ns>"] index` entry resolves through its OWN
    `index.ocx.sh` source, not just `ocx.sh`.

    Regression for the bug where per-namespace index selection was hard-coded
    to `ocx.sh` and any other configured index-bearing namespace was silently
    routed as plain OCI. Here the logical namespace is `corp.example` (distinct
    from the physical registry the root points at); the two-hop resolve must
    land every request on the fixture and snapshot under the `corp.example`
    source subtree. Were it routed as plain OCI, the fixture would see nothing
    and the offline re-resolve would find no platforms.
    """
    namespace = "corp.example"
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server, namespace=namespace)
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
    # The logical id carries the NON-ocx.sh namespace (static_index hardcodes
    # `ocx.sh/...` in its own `logical_id`, so build it explicitly here).
    logical_id = f"{namespace}/{repository}:1.0.0"
    obs_hex = hashlib.sha256(
        static_index.observation_bytes(leaf_digest, os=os_name, architecture=arch_name)
    ).hexdigest()

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", logical_id)

    # Every hop landed on the fixture — proof the `corp.example` namespace
    # routed through its own OcxIndex, never plain-OCI registry tags.
    requested_paths = [record.path for record in index_server.requests]
    assert any(path.endswith("/config.json") for path in requested_paths)
    assert any(path.endswith(f"/p/{repository}.json") for path in requested_paths)
    assert any(f"/o/sha256/{obs_hex}.json" in path for path in requested_paths)

    # Snapshotted under the `corp.example` source subtree (A2).
    assert _root_document_path(index_dir, repository, namespace).is_file()
    assert _dispatch_object_path(index_dir, repository, obs_hex, namespace).is_file()

    # Offline re-resolve through the local index alone — zero network.
    clean_home = tmp_path / "clean_home"
    clean_home.mkdir()
    offline_runner = OcxRunner(ocx.binary, clean_home, ocx.registry)
    result = offline_runner.plain(
        "--offline",
        "--index",
        str(index_dir),
        "index",
        "list",
        logical_id,
        "--platforms",
    )
    assert pkg.platform in result.stdout
    assert not (clean_home / "blobs").exists()


def test_package_install_pulls_layers_from_physical_registry_and_execs_offline(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """A full `ocx package install` through the live chain: the manifest resolves
    via the fixture (root -> obs -> physical manifest) and the LAYER blobs are
    pulled from the physical registry the root's `repository` points at — never
    from the logical `ocx.sh` host, which has no `/v2` surface.

    The logical repository (`<repo>/pkg`) is deliberately distinct from the
    physical push repo (`<repo>`): storage keys on logical identity, transport
    on the physical pointer (C2). If the layer download used the logical
    identifier the pull would fail with "blob unknown to registry"; a
    successful install with a runnable binary is the proof the physical rewrite
    reaches the content-fetch site, not only the manifest resolve.
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

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()

    # GAP 1 proof: resolves the manifest via the fixture, then pulls the layer
    # blobs from the physical registry (`ocx.registry`). A pre-fix build fetched
    # blobs from `https://ocx.sh/v2/...` here and failed the pull.
    ocx.plain("--index", str(index_dir), "package", "install", entry.logical_id)

    # The binary is materialized on disk under the logical `ocx.sh` source key,
    # not under the physical push repo.
    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir("ocx.sh")
        / repository
        / "candidates"
        / "1.0.0"
    )
    assert_symlink_exists(candidate)

    # Offline re-exec: the installed binary runs with zero network, resolving
    # through the pinned candidate + the local index alone.
    result = ocx.plain(
        "--offline",
        "--index",
        str(index_dir),
        "package",
        "exec",
        entry.logical_id,
        "--",
        "hello",
    )
    assert pkg.marker in result.stdout


def test_corrupt_leaf_manifest_blob_self_heals_online_then_resolves_offline(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """A present-but-corrupt leaf platform-manifest blob in `$OCX_HOME/blobs`
    must never be trusted (CWE-345): an online resolve removes it and re-fetches,
    and a subsequent offline resolve then succeeds — corrupt bytes are never
    loaded or linked.

    Regression for two defects in the index chain's blob recovery:
    `recover_absent_leaf`'s digest-mismatch branch returned `Ok(None)` and left
    the corrupt blob in place (so `write_blob`'s check-first fast path re-accepted
    it forever, and every later offline resolve reloaded tampered bytes), and the
    install-staging shortcut short-circuited on blob-path EXISTENCE alone. The
    corruption is NON-EMPTY on purpose: a zero-byte artifact would be overwritten
    by `write_blob`'s fast path anyway; a non-empty mismatch is the case the
    remove-before-refetch guards.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    leaf_hex = leaf_digest.split(":", 1)[1]
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

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()

    # Fresh online install materializes the leaf manifest into $OCX_HOME/blobs.
    ocx.plain("--index", str(index_dir), "package", "install", entry.logical_id)

    ocx_home = Path(ocx.env["OCX_HOME"])
    leaf_blob = _leaf_blob_data_file(ocx_home, leaf_digest)
    honest_bytes = leaf_blob.read_bytes()
    assert honest_bytes, "the leaf manifest blob must be present and non-empty after install"

    # Tamper: non-empty bytes that do not hash to the leaf digest.
    corrupt = honest_bytes + b"CORRUPT-DOES-NOT-HASH"
    leaf_blob.write_bytes(corrupt)
    assert hashlib.sha256(corrupt).hexdigest() != leaf_hex

    # Online resolve must heal: the corrupt blob is removed and re-fetched.
    ocx.plain("--index", str(index_dir), "package", "install", entry.logical_id)
    healed = _leaf_blob_data_file(ocx_home, leaf_digest).read_bytes()
    assert hashlib.sha256(healed).hexdigest() == leaf_hex, (
        "the corrupt leaf blob must be re-fetched to matching content, never left corrupt"
    )

    # Offline exec resolves through the healed blob with zero network.
    result = ocx.plain(
        "--offline",
        "--index",
        str(index_dir),
        "package",
        "exec",
        entry.logical_id,
        "--",
        "hello",
    )
    assert pkg.marker in result.stdout


# ---------------------------------------------------------------------------
# 2 — obs tamper: hard DataError, nothing persisted
# ---------------------------------------------------------------------------


def test_observation_tamper_is_hard_dataerror_and_persists_nothing(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
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

    # Tamper: the root still points at the honest digest, but the bytes
    # served at that same URL no longer hash to it.
    obs_hex = entry.obs_digest.split(":", 1)[1]
    obs_path = index_server.root / "p" / repository / "o" / "sha256" / f"{obs_hex}.json"
    obs_path.write_bytes(b'{"platforms":[]}TAMPERED-BYTES-DO-NOT-HASH')

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain(
        "--index", str(index_dir), "index", "update", entry.logical_id, check=False
    )

    assert result.returncode == 65, (
        f"expected DataError(65), got rc={result.returncode}\n{result.stderr}"
    )
    assert "digest mismatch" in result.stderr

    # F1 write order (dispatch object -> root -> catalog entry) means a
    # tampered obs fetch fails BEFORE anything is written — no dispatch
    # object, no root document, under the whole index home.
    assert not list(index_dir.rglob("*.json")), (
        "a tampered obs fetch must persist nothing at all"
    )


# ---------------------------------------------------------------------------
# 3 — format_version=2: fail-closed for this namespace; other packages unaffected
# ---------------------------------------------------------------------------


def test_unsupported_format_version_fails_closed_registry_only_unaffected(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root, format_version=2)

    ocx_id = f"ocx.sh/{unique_repo}/pkg:1.0.0"
    result = ocx.plain("index", "update", ocx_id, check=False)
    assert result.returncode == 65, (
        f"expected DataError(65), got rc={result.returncode}\n{result.stderr}"
    )
    assert "format_version" in result.stderr

    # A registry-only package never reaches the ocx.sh namespace guard —
    # the broken index config must not leak into unrelated resolves.
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    ocx.plain("index", "update", pkg.short)


# ---------------------------------------------------------------------------
# 4 — yanked tag: refused without opt-in; OCX_ALLOW_YANKED allows;
#     digest-pinned resolve bypasses the check entirely
# ---------------------------------------------------------------------------


def test_yanked_tag_refused_optin_allows_digest_pin_bypasses(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
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
        yanked=True,
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()

    # (a) a tag resolve is refused without the opt-in. The refusal fires
    # before any dispatch object or root is persisted (surface_status runs
    # before the obs fetch commits) — nothing lands on disk.
    refused = ocx.plain(
        "--index", str(index_dir), "index", "update", entry.logical_id, check=False
    )
    assert refused.returncode == 65, (
        f"expected DataError(65), got rc={refused.returncode}\n{refused.stderr}"
    )
    assert "yanked" in refused.stderr
    assert not list(index_dir.rglob("*.json")), (
        "a refused yanked resolve must persist nothing"
    )

    # (b) OCX_ALLOW_YANKED=1 allows the same resolve.
    allowed = ocx.run(
        "--index",
        str(index_dir),
        "index",
        "update",
        entry.logical_id,
        format=None,
        env_overrides={"OCX_ALLOW_YANKED": "1"},
    )
    assert allowed.returncode == 0
    assert _root_document_path(index_dir, repository).is_file(), (
        "the opt-in must let the resolve commit"
    )

    # (c) a digest-pinned resolve of the same content passes without the
    # opt-in — a yank is a tag-lane publisher signal, never checked on an
    # immutable digest pin.
    digest_id = f"ocx.sh/{repository}@{leaf_digest}"
    ocx.plain("package", "inspect", digest_id, "--resolve")


# ---------------------------------------------------------------------------
# 5 — deprecated status: resolve succeeds with a stderr warning
# ---------------------------------------------------------------------------


def test_deprecated_status_resolves_with_stderr_warning(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    message = "use the successor package instead"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
        status="deprecated",
        deprecated_message=message,
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)
    assert message in result.stderr
    assert _root_document_path(index_dir, repository).is_file(), (
        "a deprecated (non-yanked) resolve must still commit"
    )


# ---------------------------------------------------------------------------
# 6 — catalog sync: conditional GET (304) + moved-only re-snapshot
# ---------------------------------------------------------------------------


def test_catalog_sync_conditional_get_and_moved_diff(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """Catalog sync: conditional GET (304) + moved-only re-snapshot.

    Updated for the corrected B3 listing-row contract (F2): a package that is
    merely NEW to the local catalog is recorded as a listing row and NOT
    auto-materialized by the piggyback — only a package with an existing local
    root whose remote digest moved is re-snapshotted. The first-sync assertion
    below therefore now requires pkgB to be a listing row (root absent), the
    opposite of the pre-fix behavior where `diff_moved` treated every
    absent-from-previous entry as "moved" and re-snapshotted the whole catalog.
    See `test_piggyback_catalog_sync_snapshots_only_the_named_package` for the
    single-package proof of the same contract.
    """
    pkg_a = make_package(
        ocx, f"{unique_repo}a", "1.0.0", tmp_path, new=True, index=False
    )
    pkg_b = make_package(
        ocx, f"{unique_repo}b", "1.0.0", tmp_path, new=True, index=False
    )
    leaf_a = fetch_platform_manifest_digest(ocx.registry, pkg_a.repo, pkg_a.tag)
    leaf_b = fetch_platform_manifest_digest(ocx.registry, pkg_b.repo, pkg_b.tag)
    os_a, arch_a = pkg_a.platform.split("/")
    os_b, arch_b = pkg_b.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repo_a = f"{unique_repo}/pkga"
    repo_b = f"{unique_repo}/pkgb"
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
    static_index.write_catalog(
        index_server.root, {repo_a: entry_a.root_digest, repo_b: entry_b.root_digest}
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()

    # First sync: only the NAMED package (pkgA) is materialized. pkgB is new to
    # the local catalog, so the piggyback records it as a LISTING ROW in
    # c/index.json without fetching its root — materialized only when first
    # `update`d (F2, corrected contract).
    ocx.plain("--index", str(index_dir), "index", "update", entry_a.logical_id)
    assert _root_document_path(index_dir, repo_a).is_file()
    assert not _root_document_path(index_dir, repo_b).is_file(), (
        "the first catalog sync must NOT re-snapshot pkgB — a package new to the "
        "local catalog is a listing row (F2), materialized only when first updated"
    )
    assert any(
        record.path.endswith("/c/index.json") for record in index_server.requests
    )

    # Second sync: catalog content is unchanged -> conditional GET answers
    # 304, and neither package is touched by the piggyback.
    checkpoint = len(index_server.requests)
    ocx.plain("--index", str(index_dir), "index", "update", entry_a.logical_id)
    since = index_server.requests[checkpoint:]
    catalog_requests = [
        record for record in since if record.path.endswith("/c/index.json")
    ]
    assert catalog_requests, "the second run must still send the conditional GET"
    assert catalog_requests[-1].status == 304, "an unchanged catalog must answer 304"
    assert not any(repo_b in record.path for record in since), (
        "pkgB must not be re-fetched by the piggyback when its catalog entry did not move"
    )

    # Third sync: only pkgB's root changes (its catalog digest moves) -> the
    # piggyback re-snapshots pkgB only. Name pkgB directly this time so a hit
    # on pkgA's URLs can only come from the (absent) piggyback re-snapshot.
    entry_b2 = static_index.write_package(
        index_server.root,
        repository=repo_b,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg_b.repo}",
        platform_digest=leaf_b,
        os=os_b,
        architecture=arch_b,
        status="deprecated",
        deprecated_message="refreshed",
    )
    static_index.write_catalog(
        index_server.root, {repo_a: entry_a.root_digest, repo_b: entry_b2.root_digest}
    )
    checkpoint = len(index_server.requests)
    ocx.plain("--index", str(index_dir), "index", "update", entry_b.logical_id)
    since = index_server.requests[checkpoint:]
    assert any(repo_b in record.path for record in since), (
        "pkgB (moved) must be re-fetched"
    )
    assert not any(repo_a in record.path for record in since), (
        "pkgA (unchanged in the catalog) must not be re-fetched by the piggyback"
    )


# ---------------------------------------------------------------------------
# 7 — migration-resolve: repository pointer moves, logical id + committed
#     lock survive
# ---------------------------------------------------------------------------


def test_repository_migration_preserves_logical_id_and_committed_lock(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """A `repository` pointer migrating to a new physical host does not
    change the resolved leaf digest, and a local index committed before the
    migration keeps resolving fully offline afterwards (validation #7).
    """
    repo_before = f"{unique_repo}a"
    repo_after = f"{unique_repo}b"
    pkg = make_package(ocx, repo_before, "1.0.0", tmp_path, new=True, index=False)
    expected_leaf_digest = fetch_platform_manifest_digest(
        ocx.registry, pkg.repo, pkg.tag
    )
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    logical_id = f"ocx.sh/{repository}:1.0.0"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{repo_before}",
        platform_digest=expected_leaf_digest,
        os=os_name,
        architecture=arch_name,
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", logical_id)

    # A committed lock pins the pre-migration leaf digest.
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f'[tools]\ntool = "{logical_id}"\n')
    lock_result = subprocess.run(
        [str(ocx.binary), "--index", str(index_dir), "lock", "--no-pull"],
        cwd=project,
        capture_output=True,
        text=True,
        env=ocx.env,
    )
    assert lock_result.returncode == 0, f"ocx lock failed: {lock_result.stderr}"
    lock_data = tomllib.loads((project / "ocx.lock").read_text())
    tool = next(t for t in lock_data["tool"] if t["name"] == "tool")
    digest_before = tool["platforms"].get("any") or next(
        iter(tool["platforms"].values())
    )
    assert digest_before == expected_leaf_digest

    # Migrate: byte-identical content moves to a new physical repo; only the
    # root's `repository` pointer changes — the obs/tag digests are untouched
    # (the platform-manifest digest is the same content, so the obs object's
    # own digest is unchanged too).
    clone_manifest_chain(ocx.registry, repo_before, repo_after, "1.0.0")
    static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{repo_after}",
        platform_digest=expected_leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    # A live re-fetch (the root is volatile, only refreshed by `index update`)
    # re-verifies + re-persists the root against the new physical host.
    ocx.plain("--index", str(index_dir), "index", "update", logical_id)

    # The migrated root re-verifies and re-stores under the SAME logical id,
    # naming the NEW physical repository. The dispatch object (the obs
    # object, keyed by content digest, never a leaf manifest — A3) is
    # unchanged since the underlying platform content did not move.
    obs_hex = entry.obs_digest.split(":", 1)[1]
    obs_object = _dispatch_object_path(index_dir, repository, obs_hex)
    assert obs_object.is_file()
    assert hashlib.sha256(obs_object.read_bytes()).hexdigest() == obs_hex

    root_doc = json.loads(_root_document_path(index_dir, repository).read_text())
    assert root_doc["repository"] == f"oci://{ocx.registry}/{repo_after}", (
        "the re-persisted root must name the NEW physical repository"
    )

    # The pre-migration committed lock still resolves — fully offline. Listing
    # platforms needs no network: the obs object already carries the resolved
    # `platform -> digest` map (A3).
    clean_home = tmp_path / "clean_home"
    clean_home.mkdir()
    offline_runner = OcxRunner(ocx.binary, clean_home, ocx.registry)
    result = offline_runner.plain(
        "--offline",
        "--index",
        str(index_dir),
        "index",
        "list",
        logical_id,
        "--platforms",
    )
    assert pkg.platform in result.stdout


# ---------------------------------------------------------------------------
# NEW (F5) — index-role [mirrors] override end-to-end
# ---------------------------------------------------------------------------


def test_index_role_mirror_override_routes_every_request_to_the_override(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """`[mirrors."<host>"] index` overrides the index-role traffic for the
    `[registries."ocx.sh"] index` base's OWN traffic host — replace
    semantics, no fallback (`adr_index_indirection.md` F5c, UX scenario 4).

    The base is pointed at a syntactically valid but never-resolvable
    hostname (`.invalid` TLD, RFC 2606); a direct hit would fail DNS
    resolution immediately. Success here is only possible because the
    override substitutes the fixture BEFORE any network call — every
    root/obs/config request lands on the fixture, none on the un-mirrored
    base host.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

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

    dead_host = "no-such-index.invalid"
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f'[registries."ocx.sh"]\nindex = "https://{dead_host}"\n\n'
        f'[mirrors."{dead_host}"]\nindex = "{index_server.base_url}"\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host}"

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)
    assert result.returncode == 0, (
        f"resolve through the index-role mirror override must succeed: {result.stderr}"
    )

    requested_paths = [record.path for record in index_server.requests]
    assert any(path.endswith("/config.json") for path in requested_paths)
    assert any(path.endswith(f"/p/{repository}.json") for path in requested_paths)
    assert _root_document_path(index_dir, repository).is_file()


# ---------------------------------------------------------------------------
# NEW (F5) — OCX_MIRRORS union acceptance: string form + object form
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "mirror_value_factory",
    [
        pytest.param(lambda base_url: base_url, id="string-form-both-roles"),
        pytest.param(
            lambda base_url: {"index": base_url}, id="object-form-index-role-only"
        ),
    ],
)
def test_ocx_mirrors_env_union_forms_override_index_base(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    mirror_value_factory,
) -> None:
    """`OCX_MIRRORS` (forwarded-env union, F5b) accepts both a bare-string
    value (both traffic roles) and a `{index: ...}` object (index role only)
    for the SAME override — parsed through the identical shared branch a
    `[mirrors]` TOML entry uses (`parse_mirror_value`).
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

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

    dead_host = "no-such-index.invalid"
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(f'[registries."ocx.sh"]\nindex = "https://{dead_host}"\n')
    ocx.env["OCX_MIRRORS"] = json.dumps(
        {dead_host: mirror_value_factory(index_server.base_url)}
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host}"

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)
    assert result.returncode == 0, (
        f"OCX_MIRRORS override must resolve through the fixture: {result.stderr}"
    )
    assert _root_document_path(index_dir, repository).is_file()
    assert any(
        record.path.endswith(f"/p/{repository}.json")
        for record in index_server.requests
    )


# ---------------------------------------------------------------------------
# NEW (D terra-on-D regression) — absent [registries] index field never
# constructs OcxIndex, so a dead default index endpoint is never touched
# ---------------------------------------------------------------------------


def test_absent_index_field_never_touches_a_dead_index_endpoint(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """No `[registries."ocx.sh"].index` field configured -> `ocx.sh` resolves
    as plain OCI even though the default index base is (index-role)
    mirror-routed here to an unreachable endpoint, for a fast deterministic
    failure IF it were ever contacted.

    `build_index_source` gates `OcxIndex` construction purely on field
    presence (`registries_index_field_present`) — an index outage can never
    hard-block a plain-OCI-configured namespace (arch-verify terra-on-D
    ruling, folded into E as mandatory item 1).
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        "[mirrors]\n"
        f'"ocx.sh" = "http://{ocx.registry}"\n'
        '"index.ocx.sh" = { index = "http://127.0.0.1:1" }\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},127.0.0.1:1"

    fq = f"ocx.sh/{pkg.repo}:1.0.0"
    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "update", fq)
    assert result.returncode == 0, (
        'absent registries."ocx.sh".index must resolve ocx.sh as plain OCI, '
        f"never touching the dead default-index mirror target: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# NEW (regression guard) — tag-scoped `index update` persists the FULL
# published root; a sibling tag not named on the command line must survive
# ---------------------------------------------------------------------------


def test_tag_scoped_update_preserves_sibling_tag_in_persisted_root(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """Guards `ocx index update pkg:tag` (tag-scoped form): the two-hop
    resolve fetches and persists the FULL published root document (every tag
    the publisher wrote), never a root narrowed to just the named tag. A
    sibling tag (`2.0`) absent from the invocation (`pkg:1.0`) must stay
    resolvable from the persisted local index afterwards — regression guard
    against a future "narrow the root fetch to the named tag only" change
    silently dropping sibling tags.
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
        tag="1.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    # `write_package` only writes the ONE tag it is given. Add a sibling tag
    # (`2.0`) to the SAME published root document by patching the fixture
    # bytes directly (same technique as `test_observation_tamper_...` above)
    # — the published root now carries two tags for one repository.
    root_path = index_server.root / "p" / f"{repository}.json"
    root_doc = json.loads(root_path.read_text())
    root_doc["tags"]["2.0"] = dict(root_doc["tags"]["1.0"])
    root_path.write_bytes(
        json.dumps(root_doc, sort_keys=True, separators=(",", ":")).encode()
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    # Tag-scoped update names ONLY "1.0".
    ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)

    # Direct check: the persisted root document still carries both tags.
    persisted_root = json.loads(_root_document_path(index_dir, repository).read_text())
    assert "2.0" in persisted_root["tags"], (
        "a tag-scoped update must persist the full published root — the "
        "sibling '2.0' tag must not be dropped"
    )

    # Behavioral check: "2.0" — never named on the command line — resolves
    # fully offline through the persisted local index alone (mirrors how the
    # two-hop-resolve tests above assert resolvability).
    sibling_id = f"ocx.sh/{repository}:2.0"
    clean_home = tmp_path / "clean_home"
    clean_home.mkdir()
    offline_runner = OcxRunner(ocx.binary, clean_home, ocx.registry)
    result = offline_runner.plain(
        "--offline",
        "--index",
        str(index_dir),
        "index",
        "list",
        sibling_id,
        "--platforms",
    )
    assert pkg.platform in result.stdout


# ===========================================================================
# WP-CORE review-fix regression specs (plan_review_fix_index_indirection)
#
# The three tests below are RED against the current binary — they encode the
# ADR-correct behavior for three confirmed Block findings and MUST fail until
# the fix lands. Each is anchored on `adr_index_indirection.md`.
# ===========================================================================


# ---------------------------------------------------------------------------
# B2 — snapshot completeness: a tag-scoped update persists the full published
#      root, so it must fetch EVERY distinct obs that root references, not just
#      the named tag's. (`LocalIndex::refresh_published`, A2/A3/F1.)
# ---------------------------------------------------------------------------


def test_tag_scoped_update_fetches_every_distinct_sibling_obs(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """B2 (Codex-high): `ocx index update pkg:1.0` persists the FULL published
    root document (every tag the publisher wrote), so it must also fetch every
    DISTINCT observation object those sibling tags reference — otherwise the
    committed root points a sibling tag at an obs object absent from `o/`, and
    that sibling cannot resolve offline (`adr_index_indirection.md` A2/A3/F1).

    Distinct from the existing
    `test_tag_scoped_update_preserves_sibling_tag_in_persisted_root`: that one
    makes both tags share ONE obs digest, so persisting the named tag's obs
    incidentally covers the sibling and the gap stays hidden. Here tag `2.0`
    points at a DISTINCT obs digest, so only fetching `1.0`'s obs leaves `2.0`
    dangling — the actual bug `refresh_published` carries (it fans obs persists
    over `identifier.tag()` only, then writes the whole root).
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
        tag="1.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )

    # A DISTINCT sibling obs: tag `2.0` points at a different platform digest,
    # so its observation object hashes to a different digest than `1.0`'s. The
    # obs bytes are self-serving for an offline `index list --platforms` (which
    # never fetches the leaf), so a fabricated-but-valid leaf digest is enough.
    sibling_leaf = "sha256:" + hashlib.sha256(b"b2-distinct-sibling-leaf").hexdigest()
    sibling_obs_bytes = static_index.observation_bytes(
        sibling_leaf, os=os_name, architecture=arch_name
    )
    sibling_obs_hex = hashlib.sha256(sibling_obs_bytes).hexdigest()
    assert sibling_obs_hex != entry.obs_digest.split(":", 1)[1], (
        "test precondition: the sibling obs must differ from the named tag's obs"
    )
    sibling_obs_path = (
        index_server.root / "p" / repository / "o" / "sha256" / f"{sibling_obs_hex}.json"
    )
    sibling_obs_path.write_bytes(sibling_obs_bytes)

    # Patch the published root to add tag `2.0` -> the distinct obs (same
    # direct-bytes technique the sibling-preservation test uses).
    root_path = index_server.root / "p" / f"{repository}.json"
    root_doc = json.loads(root_path.read_text())
    root_doc["tags"]["2.0"] = {
        "content": f"sha256:{sibling_obs_hex}",
        "observed": "2026-01-01T00:00:00Z",
    }
    root_path.write_bytes(
        json.dumps(root_doc, sort_keys=True, separators=(",", ":")).encode()
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    # Tag-scoped update names ONLY "1.0".
    ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)

    # Direct check: the sibling tag's DISTINCT obs object must be present in the
    # local dispatch-object CAS. Fetching only the named tag's obs (the bug)
    # leaves it absent even though the persisted root references it.
    sibling_obs_local = _dispatch_object_path(index_dir, repository, sibling_obs_hex)
    assert sibling_obs_local.is_file(), (
        "a tag-scoped update must fetch every DISTINCT obs the persisted root "
        "references, not only the named tag's — sibling '2.0' obs is missing"
    )
    assert hashlib.sha256(sibling_obs_local.read_bytes()).hexdigest() == sibling_obs_hex

    # Behavioral check: sibling '2.0' — never named — resolves fully offline.
    sibling_id = f"ocx.sh/{repository}:2.0"
    clean_home = tmp_path / "clean_home"
    clean_home.mkdir()
    offline_runner = OcxRunner(ocx.binary, clean_home, ocx.registry)
    result = offline_runner.plain(
        "--offline", "--index", str(index_dir), "index", "list", sibling_id, "--platforms"
    )
    assert pkg.platform in result.stdout


# ---------------------------------------------------------------------------
# B3 — catalog diff semantics: the piggyback catalog sync after a single-package
#      update materializes ONLY the named package; siblings that are merely NEW
#      in the remote catalog are listing rows. (`diff_moved`, F2.)
# ---------------------------------------------------------------------------


def test_piggyback_catalog_sync_snapshots_only_the_named_package(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """B3 (Codex-medium + perf): the first piggyback catalog sync after a
    single-package `ocx index update` must materialize ONLY the named package's
    root + obs on disk. Sibling packages that are merely NEW in the remote
    catalog (absent from the local catalog) are LISTING ROWS — recorded in
    `c/index.json`, materialized only when first `update`d
    (`adr_index_indirection.md` F2: "for packages in the remote catalog but not
    yet snapshotted locally, records nothing to verify against yet — they are
    listing rows, materialized only when first updated").

    `diff_moved` currently treats every catalog entry absent-from-`previous` as
    "moved", so the first piggyback re-snapshots EVERY remote package — a
    fetch storm scaling with catalog size, and an F2 violation.

    NOTE (conflict to resolve during the fix): the existing
    `test_catalog_sync_conditional_get_and_moved_diff` asserts the OPPOSITE for
    the first sync ("the first catalog sync must re-snapshot pkgB too — it is
    new against an empty previous catalog"). That assertion encodes the buggy
    behavior and must be updated when `diff_moved` is corrected. This test and
    that one cannot both pass.
    """
    packages = {
        suffix: make_package(
            ocx, f"{unique_repo}{suffix}", "1.0.0", tmp_path, new=True, index=False
        )
        for suffix in ("a", "b", "c")
    }

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)

    entries: dict[str, static_index.PackageEntry] = {}
    catalog: dict[str, str] = {}
    for suffix, pkg in packages.items():
        leaf = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
        os_name, arch_name = pkg.platform.split("/")
        repository = f"{unique_repo}/pkg{suffix}"
        entries[suffix] = static_index.write_package(
            index_server.root,
            repository=repository,
            tag="1.0.0",
            physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
            platform_digest=leaf,
            os=os_name,
            architecture=arch_name,
        )
        catalog[repository] = entries[suffix].root_digest
    static_index.write_catalog(index_server.root, catalog)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    # Update exactly ONE package (a). Its piggyback catalog sync sees b and c as
    # new remote entries against an empty local catalog.
    ocx.plain("--index", str(index_dir), "index", "update", entries["a"].logical_id)

    repo_a = f"{unique_repo}/pkga"
    repo_b = f"{unique_repo}/pkgb"
    repo_c = f"{unique_repo}/pkgc"

    # The named package IS materialized.
    assert _root_document_path(index_dir, repo_a).is_file()

    # The sibling packages are listing rows ONLY — no root document on disk.
    assert not _root_document_path(index_dir, repo_b).is_file(), (
        "a one-package update must NOT materialize sibling pkgB's root — it is "
        "a listing row until first updated (F2); diff_moved re-snapshots it today"
    )
    assert not _root_document_path(index_dir, repo_c).is_file(), (
        "a one-package update must NOT materialize sibling pkgC's root (F2)"
    )

    # ...but the synced catalog (offline listing source) still lists ALL three.
    catalog_path = _source_dir(index_dir) / "c" / "index.json"
    local_catalog = json.loads(catalog_path.read_text())
    assert set(local_catalog) == {repo_a, repo_b, repo_c}, (
        "the synced catalog must list every remote package as a row, even the "
        "unmaterialized siblings"
    )


# ---------------------------------------------------------------------------
# B1 — offline yank / deprecation surfacing: `resolve_dispatch` reading the
#      committed local root must surface the human-governed lane (F3), which it
#      currently ignores. Anchored on ADR F3 + Validation item V13.
# ---------------------------------------------------------------------------


def test_offline_yanked_tag_resolve_refused_from_committed_root(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """B1 / V13: a yank surfaced OFFLINE from the committed root doc. Per
    `adr_index_indirection.md` Validation item: "A tag resolving to a `yanked`
    entry warns and refuses (absent explicit opt-in) — surfaced **offline** from
    the committed root doc; a digest-pinned resolve of the same content still
    succeeds." F3 pins the refusal to `DataError`.

    The existing `test_yanked_tag_refused_optin_allows_digest_pin_bypasses`
    only exercises the ONLINE `index update` path (the `OcxIndex` remote's
    status surfacing). This pins the OFFLINE path — `LocalIndex::resolve_dispatch`
    reading the committed local root — which currently ignores the `yanked`
    marker entirely, so an offline resolve of a yanked tag succeeds silently.
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
        yanked=True,
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    # Commit the yanked root locally with the opt-in (a plain update is refused
    # before it commits — see the existing online yank test).
    ocx.run(
        "--index",
        str(index_dir),
        "index",
        "update",
        entry.logical_id,
        format=None,
        env_overrides={"OCX_ALLOW_YANKED": "1"},
    )
    assert _root_document_path(index_dir, repository).is_file()

    # (a) OFFLINE tag resolve refuses, surfacing the yank from the committed
    # root — no network, no opt-in.
    refused = ocx.plain(
        "--offline",
        "--index",
        str(index_dir),
        "index",
        "list",
        entry.logical_id,
        "--platforms",
        check=False,
    )
    assert refused.returncode == 65, (
        "an offline yanked tag resolve must refuse (DataError 65) surfacing the "
        f"committed root's yank, got rc={refused.returncode}\n{refused.stderr}"
    )
    assert "yanked" in refused.stderr

    # (b) OFFLINE digest-pinned resolve of the same content succeeds — a yank is
    # a tag-lane signal, never checked on an immutable digest pin.
    digest_id = f"ocx.sh/{repository}@{entry.obs_digest}"
    allowed = ocx.plain(
        "--offline",
        "--index",
        str(index_dir),
        "index",
        "list",
        digest_id,
        "--platforms",
        check=False,
    )
    assert allowed.returncode == 0, (
        "an offline digest-pinned resolve must bypass the yank check: "
        f"rc={allowed.returncode}\n{allowed.stderr}"
    )
    assert pkg.platform in allowed.stdout

    # (c) OFFLINE tag resolve with the OCX_ALLOW_YANKED opt-in succeeds, but
    # still surfaces the yank warning on stderr (warn-but-allow, ADR F3).
    optin = ocx.plain(
        "--offline",
        "--index",
        str(index_dir),
        "index",
        "list",
        entry.logical_id,
        "--platforms",
        check=False,
        env_overrides={"OCX_ALLOW_YANKED": "1"},
    )
    assert optin.returncode == 0, (
        "an offline yanked tag resolve with OCX_ALLOW_YANKED=1 must succeed: "
        f"rc={optin.returncode}\n{optin.stderr}"
    )
    assert "yanked" in optin.stderr, (
        "the opt-in must still warn about the yank on stderr, not silently allow it"
    )
    assert pkg.platform in optin.stdout


def test_offline_deprecated_tag_resolve_warns_from_committed_root(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """B1 / F3: a deprecation surfaced OFFLINE from the committed root doc. Per
    `adr_index_indirection.md` F3: "status: deprecated + deprecated_message |
    Warn on resolve; surface the message in ocx package info". An offline resolve
    of a deprecated tag must warn (stderr) from the committed root and still
    succeed.

    The existing `test_deprecated_status_resolves_with_stderr_warning` only
    exercises the ONLINE `index update` path; this pins the OFFLINE resolve —
    `LocalIndex::resolve_dispatch` — which currently drops the warning.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"
    message = "use the successor package instead"
    entry = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
        status="deprecated",
        deprecated_message=message,
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    # Commit the deprecated root (a deprecation is warned, never refused).
    ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)
    assert _root_document_path(index_dir, repository).is_file()

    # OFFLINE resolve still surfaces the deprecation message from the committed
    # root, with zero network.
    result = ocx.plain(
        "--offline",
        "--index",
        str(index_dir),
        "index",
        "list",
        entry.logical_id,
        "--platforms",
    )
    assert message in result.stderr, (
        "an offline resolve of a deprecated tag must surface the deprecated "
        f"message from the committed root doc (F3); stderr was:\n{result.stderr}"
    )
