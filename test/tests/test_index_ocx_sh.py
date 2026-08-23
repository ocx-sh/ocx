# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the `index.ocx.sh` client (`adr_index_indirection.md`
Decision F): two-hop resolve (root -> sha256-verified dispatch object ->
physical manifest), catalog sync (F2), status surfacing (F3), and the
`[registries]`/`[mirrors]` config surfaces (F5) against a local static-file
HTTP fixture that encodes the frozen ● wire shapes.

Ground truth for the wire shapes: `IndexRoot`, `RootTag`, `CatalogIndex` in
`crates/ocx_lib/src/oci/index/wire.rs` (`IndexFormatConfig`/`CatalogSyncOutcome`
in `crates/ocx_lib/src/oci/index/ocx_index.rs`). The dispatch object a root's
`content` names is a real OCI image index, stored verbatim
(`adr_oci_index_only_dispatch.md` D1).

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
import socket
import subprocess
import tomllib
import urllib.error
import urllib.request
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import static_index
from src.assertions import assert_not_exists, assert_symlink_exists
from src.helpers import make_package
from src.registry import clone_manifest_chain, fetch_manifest_raw, fetch_platform_manifest_digest
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

    Also trusts `ocx.registry`'s bare host (the SSRF guard's `trusted_hosts`
    escape hatch, X2) — every fixture here resolves physical manifests against
    the loopback `registry:2` test instance, which the default-on read-path
    SSRF guard (`oci/ssrf.rs`, ocx#218) otherwise refuses.
    """
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    registry_host = ocx.registry.split(":", 1)[0]
    config_path.write_text(
        f'[registries."{namespace}"]\nindex = "{server.base_url}"\ntrusted_hosts = ["{registry_host}"]\n'
    )
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
    """`ocx index update` two-hop resolves through the fixture (root ->
    sha256-verified dispatch object -> physical manifest from the local
    registry), and the resulting local index resolves the same package fully
    offline afterwards.

    Also covers item #8 ([registries] override authority): every root,
    dispatch-object and config request in this flow lands on the fixture,
    never the default `https://index.ocx.sh`.
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

    # Every hop landed on the fixture (item #8) — root, dispatch object, and
    # the config.json probe all resolved through the configured override.
    requested_paths = [record.path for record in index_server.requests]
    assert any(path.endswith("/config.json") for path in requested_paths)
    assert any(path.endswith(f"/p/{repository}.json") for path in requested_paths)
    index_hex = entry.index_digest.split(":", 1)[1]
    assert any(f"/o/sha256/{index_hex}.json" in path for path in requested_paths)

    # Self-contained afterwards: the root document + the dispatch object,
    # under the `ocx.sh` source subtree (A2 — dots preserved by slugify).
    assert _root_document_path(index_dir, repository).is_file()
    index_object = _dispatch_object_path(index_dir, repository, index_hex)
    assert index_object.is_file()
    assert hashlib.sha256(index_object.read_bytes()).hexdigest() == index_hex

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


def test_two_hop_resolve_licensed_by_config_registries_entry_alone(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """The plain-HTTP index gate accepts the config half of the union on its
    own: ``[registries."<index host>"] insecure = true``, with
    ``OCX_INSECURE_REGISTRIES`` licensing only the physical registry and never
    the index host. Every other plain-HTTP-index case in this file
    (``configure_index_source``) drives the gate through the env var alone —
    this is the config half, exercised end to end through a real two-hop
    resolve.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    registry_host = ocx.registry.split(":", 1)[0]
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f'[registries."ocx.sh"]\nindex = "{index_server.base_url}"\ntrusted_hosts = ["{registry_host}"]\n'
        f'[registries."{index_server.host}"]\ninsecure = true\n'
    )
    # Deliberately NOT the index host: only the physical registry goes
    # through the env half, so the index host's allowance can only have come
    # from the config entry above.
    ocx.env["OCX_INSECURE_REGISTRIES"] = ocx.registry

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

    requested_paths = [record.path for record in index_server.requests]
    assert any(path.endswith("/config.json") for path in requested_paths), (
        "the config-licensed index host must have been reached at all"
    )
    assert _root_document_path(index_dir, repository).is_file()


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
    index_hex = hashlib.sha256(
        static_index.index_bytes(leaf_digest, os=os_name, architecture=arch_name)
    ).hexdigest()

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", logical_id)

    # Every hop landed on the fixture — proof the `corp.example` namespace
    # routed through its own OcxIndex, never plain-OCI registry tags.
    requested_paths = [record.path for record in index_server.requests]
    assert any(path.endswith("/config.json") for path in requested_paths)
    assert any(path.endswith(f"/p/{repository}.json") for path in requested_paths)
    assert any(f"/o/sha256/{index_hex}.json" in path for path in requested_paths)

    # Snapshotted under the `corp.example` source subtree (A2).
    assert _root_document_path(index_dir, repository, namespace).is_file()
    assert _dispatch_object_path(index_dir, repository, index_hex, namespace).is_file()

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
    via the fixture (root -> dispatch object -> physical manifest) and the LAYER blobs are
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
    `recover_absent_dispatch`'s digest-mismatch branch returned `Ok(None)` and left
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
# 2 — dispatch-object tamper: hard DataError, nothing persisted
# ---------------------------------------------------------------------------


def test_dispatch_object_tamper_is_hard_dataerror_and_persists_nothing(
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
    index_hex = entry.index_digest.split(":", 1)[1]
    index_path = index_server.root / "p" / repository / "o" / "sha256" / f"{index_hex}.json"
    index_path.write_bytes(b'{"platforms":[]}TAMPERED-BYTES-DO-NOT-HASH')

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
    # tampered fetch fails BEFORE anything is written — no dispatch
    # object, no root document, under the whole index home.
    assert not list(index_dir.rglob("*.json")), (
        "a tampered dispatch-object fetch must persist nothing at all"
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
    # before the dispatch-object fetch commits) — nothing lands on disk.
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
# 6 — catalog sync: unconditional GET + moved-only digest diff
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
    # root's `repository` pointer changes — the dispatch/tag digests are
    # untouched (the platform-manifest digest is the same content, so the
    # dispatch object's own digest is unchanged too).
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
    # A BARE named update is the sanctioned point to take a migration: naming
    # the package with no tag is what adopts its package-level fields, routing
    # pointer included. A tagged update would move the tag's pin and leave the
    # pointer alone, which is the whole reason the two scopes differ.
    ocx.plain("--index", str(index_dir), "index", "update", f"ocx.sh/{repository}")

    # The migrated root re-verifies and re-stores under the SAME logical id,
    # naming the NEW physical repository. The dispatch object (keyed by content
    # digest, never a leaf manifest — A3) is unchanged since the underlying
    # platform content did not move.
    index_hex = entry.index_digest.split(":", 1)[1]
    index_object = _dispatch_object_path(index_dir, repository, index_hex)
    assert index_object.is_file()
    assert hashlib.sha256(index_object.read_bytes()).hexdigest() == index_hex

    root_doc = json.loads(_root_document_path(index_dir, repository).read_text())
    assert root_doc["repository"] == f"oci://{ocx.registry}/{repo_after}", (
        "the re-persisted root must name the NEW physical repository"
    )

    # The pre-migration committed lock still resolves — fully offline. Listing
    # platforms needs no network: the dispatch object already carries the
    # resolved `platform -> digest` map (A3).
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
    root, dispatch-object and config request lands on the fixture, none on the un-mirrored
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
    """No `[registries."plain.example"].index` field configured -> that
    namespace resolves as plain OCI even though the default index base is
    (index-role) mirror-routed here to an unreachable endpoint, for a fast
    deterministic failure IF it were ever contacted.

    `build_index_sources` gates `OcxIndex` construction purely on field
    presence — an index outage can never hard-block a plain-OCI namespace
    (arch-verify terra-on-D ruling, folded into E as mandatory item 1). The
    namespace under test is a foreign one, not `ocx.sh`: the compiled-in base
    tier makes `ocx.sh` itself index-bearing, so it is no longer an example of
    an unconfigured namespace.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        "[mirrors]\n"
        f'"plain.example" = "http://{ocx.registry}"\n'
        '"index.ocx.sh" = { index = "http://127.0.0.1:1" }\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},127.0.0.1:1"

    fq = f"plain.example/{pkg.repo}:1.0.0"
    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "update", fq)
    assert result.returncode == 0, (
        'absent registries."plain.example".index must resolve it as plain OCI, '
        f"never touching the dead default-index mirror target: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# NEW — the compiled-in base tier ships `[registries."ocx.sh"] index`
# ---------------------------------------------------------------------------


def test_builtin_base_tier_makes_ocx_sh_index_bearing(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """Nothing in the config declares an `index` for `ocx.sh`, yet the
    namespace resolves through the ocx-index protocol: the compiled-in base
    tier supplies `https://index.ocx.sh`.

    The index-role `[mirrors]` entry for `index.ocx.sh` is what proves it — a
    plain-OCI `ocx.sh` would never dial that host, so the fixture would record
    no root request. The `[registries."ocx.sh"]` entry present here declares
    only `trusted_hosts` (the loopback test registry the root points at); the
    built-in `index` survives it because the table merges field-wise.

    Runs hermetically (`OCX_NO_CONFIG=1` plus the fixture config as an explicit
    `OCX_CONFIG` tier, which survives it) so no discovered tier on the host can
    supply the `index` this test claims the binary ships. Without that, a
    developer whose `/etc/ocx/config.toml` or `~/.config/ocx/config.toml`
    already names `index.ocx.sh` — the config the pre-change docs told users to
    write — would see this pass with the compiled-in tier deleted. It doubles
    as the proof that the compiled-in tier is NOT gated on `OCX_NO_CONFIG`.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    registry_host = ocx.registry.split(":", 1)[0]
    config_path = tmp_path / "hermetic-config.toml"
    config_path.write_text(
        f'[registries."ocx.sh"]\ntrusted_hosts = ["{registry_host}"]\n'
        "\n[mirrors]\n"
        f'"index.ocx.sh" = {{ index = "{index_server.base_url}" }}\n'
    )
    ocx.env["OCX_NO_CONFIG"] = "1"
    ocx.env["OCX_CONFIG"] = str(config_path)
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host}"

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

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)
    assert result.returncode == 0, (
        f"the compiled-in ocx.sh index must resolve: {result.stderr}"
    )
    assert any(
        record.path.endswith(f"/p/{repository}.json")
        for record in index_server.requests
    ), "the index fixture must have served the root document"


def test_mirror_entry_for_ocx_sh_suppresses_the_builtin_index(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A `[mirrors]."ocx.sh"` entry suppresses the compiled-in index for that
    namespace, which then resolves as a plain OCI registry through the mirror.

    The firewalled-site scenario: an operator pins `ocx.sh` at their own
    artifact server. `[mirrors]` is keyed by traffic host and applied to the
    PHYSICAL identifier an index mints, so it does not follow the namespace
    through the index protocol — without the suppression this resolve would
    start dialling `index.ocx.sh`, a host the operator never allow-listed.

    Same dead-endpoint trick as
    `test_absent_index_field_never_touches_a_dead_index_endpoint`: the
    index-role mirror aims `index.ocx.sh` at an unreachable port, so if the
    compiled-in index were still built the resolve would dial `127.0.0.1:1`
    and fail. No `index` is declared for `ocx.sh` anywhere here — the
    suppression is the only thing under test. (`index = ""`, the explicit
    off-switch, is covered at the unit layer: `ConfigLoader`'s
    `empty_user_index_disables_the_builtin_ocx_sh_index` for the config value
    and `build_index_sources_skips_an_empty_index_value` for the filter that
    acts on it.)
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
        'a [mirrors]."ocx.sh" entry must suppress the compiled-in index and '
        f"resolve as plain OCI, never touching the dead index endpoint: {result.stderr}"
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
    """Guards `ocx index update pkg:tag` (tag-scoped form): the update merges
    the named tag into the local root and touches nothing else. A sibling tag
    (`2.0`) already snapshotted must survive with the digest it was pinned to —
    the local index is authored, so a narrower update never drops what a wider
    one recorded.

    Seeded by a BARE update first: with no prior copy there is no sibling to
    preserve, and a tagged first-sight lands only the tag it names.
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
    # bytes directly (same technique as `test_dispatch_object_tamper_...` above)
    # — the published root now carries two tags for one repository.
    root_path = index_server.root / "p" / f"{repository}.json"
    root_doc = json.loads(root_path.read_text())
    root_doc["tags"]["2.0"] = dict(root_doc["tags"]["1.0"])
    root_path.write_bytes(
        json.dumps(root_doc, sort_keys=True, separators=(",", ":")).encode()
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    # Snapshot the whole package first, so both tags are committed locally.
    ocx.plain("--index", str(index_dir), "index", "update", f"ocx.sh/{repository}")
    seeded = json.loads(_root_document_path(index_dir, repository).read_text())
    assert sorted(seeded["tags"]) == ["1.0", "2.0"], "precondition: both tags snapshotted"

    # Tag-scoped update names ONLY "1.0".
    ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)

    # Direct check: the sibling pin is exactly as it was.
    persisted_root = json.loads(_root_document_path(index_dir, repository).read_text())
    assert persisted_root["tags"].get("2.0") == seeded["tags"]["2.0"], (
        "a tag-scoped update must leave the sibling '2.0' pin untouched"
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
#      root, so it must fetch EVERY distinct dispatch object that root
#      references, not just the named tag's.
#      (`LocalIndex::refresh_published`, A2/A3/F1.)
# ---------------------------------------------------------------------------


def test_tag_scoped_update_fetches_every_distinct_sibling_dispatch_object(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """The invariant behind the write: every tag the local root pins has its
    dispatch object present in `o/`, or that tag cannot resolve offline
    (`adr_index_indirection.md` A2/A3/F1).

    A tagged update lands one pin, so it fetches one object — a sibling it does
    not pin is not its business. A BARE update lands every remote tag, so it
    must fetch every DISTINCT object those tags reference; tag `2.0` points at
    a distinct digest here precisely so a fetch narrowed to `1.0`'s would leave
    it dangling.
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

    # A DISTINCT sibling dispatch object: tag `2.0` points at a different
    # platform digest, so its image index hashes to a different digest than
    # `1.0`'s. Those bytes are self-serving for an offline
    # `index list --platforms` (which never fetches the leaf), so a
    # fabricated-but-valid leaf digest is enough.
    sibling_leaf = "sha256:" + hashlib.sha256(b"b2-distinct-sibling-leaf").hexdigest()
    sibling_index_bytes = static_index.index_bytes(
        sibling_leaf, os=os_name, architecture=arch_name
    )
    sibling_index_hex = hashlib.sha256(sibling_index_bytes).hexdigest()
    assert sibling_index_hex != entry.index_digest.split(":", 1)[1], (
        "test precondition: the sibling object must differ from the named tag's"
    )
    sibling_index_path = (
        index_server.root / "p" / repository / "o" / "sha256" / f"{sibling_index_hex}.json"
    )
    sibling_index_path.write_bytes(sibling_index_bytes)

    # Patch the published root to add tag `2.0` -> the distinct object (same
    # direct-bytes technique the sibling-preservation test uses).
    root_path = index_server.root / "p" / f"{repository}.json"
    root_doc = json.loads(root_path.read_text())
    root_doc["tags"]["2.0"] = {
        "content": f"sha256:{sibling_index_hex}",
        "observed": "2026-01-01T00:00:00Z",
    }
    root_path.write_bytes(
        json.dumps(root_doc, sort_keys=True, separators=(",", ":")).encode()
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    # Tag-scoped update names ONLY "1.0" — it pins one tag, so it fetches one
    # object and leaves the sibling entirely alone.
    ocx.plain("--index", str(index_dir), "index", "update", entry.logical_id)
    sibling_local = _dispatch_object_path(index_dir, repository, sibling_index_hex)
    assert not sibling_local.exists(), (
        "a tag-scoped update pins only the tag it names, so it must not fetch a "
        "sibling's dispatch object either"
    )
    assert "2.0" not in json.loads(_root_document_path(index_dir, repository).read_text())["tags"]

    # The BARE update lands both pins, so both objects must travel with them.
    ocx.plain("--index", str(index_dir), "index", "update", f"ocx.sh/{repository}")
    assert sibling_local.is_file(), (
        "a bare update pins every remote tag, so it must fetch every DISTINCT "
        "dispatch object those tags reference — sibling '2.0' is missing"
    )
    assert hashlib.sha256(sibling_local.read_bytes()).hexdigest() == sibling_index_hex

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
    digest_id = f"ocx.sh/{repository}@{entry.index_digest}"
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


# ===========================================================================
# G6/G7 — role cross-contamination refused at CLI wiring level
#
# The invariant (3-agent audit, 2026-07-19): a mirror endpoint appears ONLY at
# the network seam — `Client::transport_reference`/`transport_registry` for
# the registry role, `OcxIndex::resolve_base_url` for the index role.
# `ocx_lib::resolve_mirror_map` splits a `[mirrors."<host>"]` entry by role
# BEFORE either seam ever sees it: `resolved_mirrors.registry` feeds the OCI
# client's `MirrorMap` (`Context::try_init`), `resolved_mirrors.index` feeds
# `Context::build_index_sources`. The three tests below pin that split at the
# CLI wiring level, not just the unit layer `parse_mirror_value` already
# covers.
# ===========================================================================

# Accept header covering both single-platform manifests and image indexes —
# registry:2 answers a HEAD with 404 if the stored manifest's media type is
# not in the Accept set, so an OCX package (an OCI image index at the tag) is
# only visible via HEAD when the index media type is advertised. Local to
# this file (DAMP) — mirrors `test_oci_registry_mirror.py::head_manifest`.
_MANIFEST_ACCEPT_HEADERS = ", ".join(
    [
        "application/vnd.oci.image.index.v1+json",
        "application/vnd.oci.image.manifest.v1+json",
        "application/vnd.docker.distribution.manifest.list.v2+json",
        "application/vnd.docker.distribution.manifest.v2+json",
    ]
)


def _head_manifest(registry: str, repo: str, tag: str) -> int:
    """Returns the HTTP status for `HEAD /v2/<repo>/manifests/<tag>` — 404
    when absent. Used to prove a package's manifest never reached a given
    registry (G7 — the physical fetch must go through the registry-role
    mirror instead).
    """
    url = f"http://{registry}/v2/{repo}/manifests/{tag}"
    req = urllib.request.Request(
        url, method="HEAD", headers={"Accept": _MANIFEST_ACCEPT_HEADERS}
    )
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status
    except urllib.error.HTTPError as exc:
        return exc.code


# ---------------------------------------------------------------------------
# G6 case A — an index-role-only mirror entry for the CANONICAL REGISTRY host
#             never leaks into OCI registry-role routing (no [registries]
#             table at all, so no namespace is index-bearing either).
# ---------------------------------------------------------------------------


def test_index_role_only_mirror_for_registry_host_never_affects_plain_install(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """G6 case A: an INDEX-role-only `[mirrors."<host>"]` entry keyed by the
    registry's OWN host — with no `[registries]` table at all — must never
    influence a plain `ocx package install` against that same host.

    Traces: mirror-invariant audit 2026-07-19, gap G6. If the index-role value
    leaked into `MirrorMap` (the OCI client's registry-role rewrite table fed
    by `resolved_mirrors.registry` only), the install would either chase the
    dead `.invalid` endpoint (connection failure) or be refused by the plain-
    HTTP gate (that host is never added to `OCX_INSECURE_REGISTRIES`).
    Success here is only possible because `resolve_mirror_map` splits the
    entry by role before the registry-role table is built.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    dead_host = "no-such-index.invalid"
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f'[mirrors."{ocx.registry}"]\nindex = "https://{dead_host}"\n'
    )

    # No [registries] table -> `build_index_sources` yields zero sources
    # regardless; this identifier resolves as plain OCI either way.
    ocx.plain("package", "install", pkg.fq)

    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
        / "1.0.0"
    )
    assert_symlink_exists(candidate)


# ---------------------------------------------------------------------------
# G6 case B — a registry-role-only mirror entry for the INDEX fixture's host
#             never redirects index (root/dispatch/config) traffic.
# ---------------------------------------------------------------------------


def test_registry_role_only_mirror_for_index_host_never_redirects_index_traffic(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """G6 case B: `[registries."ocx.sh"].index` names the fixture, and a
    REGISTRY-role-only `[mirrors."<fixture-host>"]` entry is ALSO declared for
    that identical host — the registry-role value must never rewrite the
    index base URL.

    Traces: mirror-invariant audit 2026-07-19, gap G6.
    `OcxIndex::resolve_base_url` only ever consults the index-role split
    (`mirrors_index`, fed by `resolved_mirrors.index`); a registry-role entry
    keyed by the same host is invisible to it. The registry-role value points
    at a dead `.invalid` host: if it leaked into `resolve_base_url`'s override
    lookup, every root, dispatch-object and config request would be sent there instead and
    `ocx index update` would fail outright.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f'[registries."ocx.sh"]\nindex = "{index_server.base_url}"\n\n'
        f'[mirrors."{index_server.host}"]\nregistry = "https://no-such-registry.invalid"\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host}"

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

    requested_paths = [record.path for record in index_server.requests]
    assert requested_paths, "the fixture must have received the two-hop traffic"
    assert any(path.endswith("/config.json") for path in requested_paths)
    assert any(path.endswith(f"/p/{repository}.json") for path in requested_paths)
    index_hex = entry.index_digest.split(":", 1)[1]
    assert any(f"/o/sha256/{index_hex}.json" in path for path in requested_paths)


# ---------------------------------------------------------------------------
# G7 — both roles composed in ONE install: the index-role mirror serves the
#      root/dispatch traffic while the registry-role mirror carries the physical
#      manifest/layer fetch, neither clobbering the other's seam.
# ---------------------------------------------------------------------------


def test_registry_and_index_role_mirrors_compose_in_one_install(
    ocx: OcxRunner,
    mirror_registry: str,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """G7: an index-role mirror (base -> dead host -> fixture override) and a
    registry-role mirror (canonical registry host -> `mirror_registry`)
    composed in ONE `ocx package install`, each covering its own seam without
    clobbering the other.

    Traces: mirror-invariant audit 2026-07-19, gap G7. The published root's
    `repository` names the CANONICAL registry host, but the package bytes
    live ONLY on `mirror_registry`. A successful install, with the manifest
    fetch proven absent from the canonical registry, is only possible if the
    registry-role mirror rewrote the physical fetch while the fixture (never
    the dead `.invalid` base) served every root, dispatch-object and config
    request via the index-role mirror — the two seams compose without either
    clobbering the other.
    """
    mirror_ocx = OcxRunner(ocx.binary, ocx.ocx_home, mirror_registry)
    pkg = make_package(
        mirror_ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False
    )
    leaf_digest = fetch_platform_manifest_digest(mirror_registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    assert _head_manifest(ocx.registry, pkg.repo, pkg.tag) == 404, (
        "the canonical registry must be empty before the test proves the "
        "registry-role mirror carried the physical fetch"
    )

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

    dead_index_host = "no-such-index.invalid"
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    registry_host = ocx.registry.split(":", 1)[0]
    config_path.write_text(
        f'[registries."ocx.sh"]\nindex = "https://{dead_index_host}"\ntrusted_hosts = ["{registry_host}"]\n\n'
        f'[mirrors."{dead_index_host}"]\nindex = "{index_server.base_url}"\n\n'
        f'[mirrors."{ocx.registry}"]\nregistry = "http://{mirror_registry}"\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = (
        f"{ocx.registry},{mirror_registry},{index_server.host}"
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "package", "install", entry.logical_id)

    # (2) every root, dispatch-object and config request landed on the fixture
    # — the index-role mirror substituted it for the dead base, replace
    # semantics.
    requested_paths = [record.path for record in index_server.requests]
    assert requested_paths, "the fixture must have received the two-hop traffic"
    assert any(path.endswith("/config.json") for path in requested_paths)
    assert any(path.endswith(f"/p/{repository}.json") for path in requested_paths)

    # (3) the canonical registry never served the manifest — the physical
    # fetch went through the registry-role mirror to `mirror_registry` instead.
    assert _head_manifest(ocx.registry, pkg.repo, pkg.tag) == 404, (
        "the canonical registry must STILL be empty after the install — the "
        "registry-role mirror must have carried the manifest/layer fetch"
    )

    # (4) the candidate symlink lands under the LOGICAL namespace slug; no
    # mirror-host slug appears anywhere under symlinks/.
    symlinks_root = Path(ocx.env["OCX_HOME"]) / "symlinks"
    candidate = (
        symlinks_root / registry_dir("ocx.sh") / repository / "candidates" / "1.0.0"
    )
    assert_symlink_exists(candidate)
    mirror_slug = registry_dir(mirror_registry)
    assert_not_exists(symlinks_root / mirror_slug)


# ---------------------------------------------------------------------------
# G8 — a registry-role mirror keyed on an UNREACHABLE physical host carries the
#      index-indirected fetch: the rewrite happens before the physical host is
#      ever dialed. Committed as a PAIR (with / without the mirror line) so the
#      rewrite stays load-bearing.
# ---------------------------------------------------------------------------


@pytest.fixture()
def dead_endpoint() -> Iterator[str]:
    """A `127.0.0.1:<port>` authority that resolves but refuses connections.

    The socket stays BOUND (never listening) for the whole test: a bound
    socket both answers `ECONNREFUSED` and reserves the port, so no sibling
    xdist worker's fixture server can claim it mid-test and turn the refusal
    into an accept. Releasing it first — as an earlier revision did — left
    exactly that window, and a green install would then no longer prove the
    rewrite happened.

    A loopback IP literal rather than the `no-such-*.invalid` names used
    elsewhere in this file: `OcxIndex::physical_identifier`
    (`crates/ocx_lib/src/oci/index/ocx_index.rs`) runs
    `oci::ssrf::resolve_and_validate` on the physical host BEFORE the mirror
    seam in `Client::transport_reference`, so a `.invalid` name would die in
    DNS (`SsrfError::Resolution` -> exit 69) and never reach the seam under
    test. `127.0.0.1` resolves with no DNS at all and is admitted by an
    explicit `trusted_hosts` entry, which leaves the TCP connection as the
    only thing that can still stop the physical fetch.
    """
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
        # Observed, never assumed: the address must actually refuse.
        with pytest.raises(OSError):
            socket.create_connection(("127.0.0.1", port), timeout=1)
        yield f"127.0.0.1:{port}"


def _arrange_dead_physical_host(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    dead_host: str,
    *,
    with_registry_mirror: bool,
) -> tuple[static_index.PackageEntry, Path, Path]:
    """Publishes onto the live registry, serves an index root whose
    `repository` pointer names a dead loopback endpoint, and writes the config.

    `with_registry_mirror` is the ONLY difference between the two tests below:
    the single `[mirrors."<dead-host>"] registry` line. Everything else —
    package bytes, dispatch-object digests, `trusted_hosts`, insecure-host
    allowance, index home — is byte-identical, so the pair isolates the
    rewrite and nothing else.
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
        physical_repository=f"oci://{dead_host}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )

    registry_host = ocx.registry.split(":", 1)[0]
    # `trusted_hosts` carries BOTH the canonical registry's bare host (the
    # substitute the mirror points at) and `127.0.0.1` (the dead physical host
    # the SSRF pre-flight validates) — the guard runs on the pointer as
    # written, before any rewrite, so an untrusted loopback pointer would be
    # refused with exit 78 rather than reaching the mirror map.
    config = (
        f'[registries."ocx.sh"]\n'
        f'index = "{index_server.base_url}"\n'
        f'trusted_hosts = ["{registry_host}", "127.0.0.1"]\n'
    )
    if with_registry_mirror:
        config += f'\n[mirrors."{dead_host}"]\nregistry = "http://{ocx.registry}"\n'
    (Path(ocx.env["OCX_HOME"]) / "config.toml").write_text(config)
    # The dead host is deliberately NOT listed: the plain-HTTP gate only ever
    # inspects a mirror's TARGET host, and the dead endpoint is never dialed
    # on the mirrored path.
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host}"

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir("ocx.sh")
        / repository
        / "candidates"
        / "1.0.0"
    )
    return entry, index_dir, candidate


def test_registry_role_mirror_rewrites_an_unreachable_physical_host(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    dead_endpoint: str,
) -> None:
    """G8 positive half: `ocx package install` of an index-indirected logical
    identifier succeeds even though the root's `repository` pointer names a
    host that refuses every connection — because a `[mirrors."<dead-host>"]`
    registry-role entry rewrites it to the live registry first.

    G7 above already composes both mirror roles in one install; what it does
    not pin is that the physical host is never dialed at all. Here the pointer
    address is *unreachable* (`dead_endpoint` proves it refuses), so the
    rewrite in `Client::transport_reference` (`crates/ocx_lib/src/oci/client.rs`)
    must happen before the connect — a reachable-but-empty registry, G7's
    arrangement, cannot distinguish the two orders.
    """
    repository = f"{unique_repo}/pkg"
    entry, index_dir, candidate = _arrange_dead_physical_host(
        ocx,
        unique_repo,
        tmp_path,
        index_server,
        dead_endpoint,
        with_registry_mirror=True,
    )

    ocx.plain("--index", str(index_dir), "package", "install", entry.logical_id)

    # The install really went through the index indirection (not a plain OCI
    # resolve): the fixture served the config probe and the root document.
    requested_paths = [record.path for record in index_server.requests]
    assert requested_paths, "the fixture must have received the two-hop traffic"
    assert any(path.endswith("/config.json") for path in requested_paths)
    assert any(path.endswith(f"/p/{repository}.json") for path in requested_paths)

    # The physical bytes arrived — via the mirror, since the pointer's own host
    # is dead — and the candidate lands under the LOGICAL namespace slug.
    assert_symlink_exists(candidate)


def test_index_indirected_install_fails_without_the_registry_role_mirror(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    dead_endpoint: str,
) -> None:
    """G8 negative twin: the identical arrangement minus the one
    `[mirrors."<dead-host>"] registry` line fails, and fails *at the dead
    physical endpoint*.

    This is what makes the positive half above falsifiable: without it, a
    green install could equally be explained by the package bytes already
    sitting in the local stores from the `make_package` push. The pair only
    passes together if the mirror rewrite is the thing carrying the fetch.
    """
    entry, index_dir, candidate = _arrange_dead_physical_host(
        ocx,
        unique_repo,
        tmp_path,
        index_server,
        dead_endpoint,
        with_registry_mirror=False,
    )

    result = ocx.plain(
        "--index", str(index_dir), "package", "install", entry.logical_id, check=False
    )

    # 75 = EX_TEMPFAIL: a refused connection is classified as a transient
    # registry fault, the same lane as any other physical-registry outage.
    assert result.returncode == 75, (
        f"expected EX_TEMPFAIL (75) from the unreachable physical registry, "
        f"got rc={result.returncode}\nstderr: {result.stderr.strip()}"
    )
    assert dead_endpoint in result.stderr, (
        "the failure must name the dead physical endpoint — otherwise it proves "
        f"only that something went wrong\nstderr: {result.stderr.strip()}"
    )
    assert_not_exists(candidate)


# ===========================================================================
# adr_oci_index_only_dispatch.md — the stored object IS the registry's image
# index, so three things become true that could not be before.
# ===========================================================================


# ---------------------------------------------------------------------------
# A13 — the pinned-leaf path: resolved, never refused, and writes nothing
# ---------------------------------------------------------------------------


def test_pinned_leaf_digest_resolves_and_leaves_the_dispatch_store_untouched(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """A digest-pinned leaf platform manifest — the shape a committed
    `ocx.lock` stores — resolves, and the `o/` directory is byte-identical
    before and after.

    This pins `LocalIndex::persist_dispatch`'s EXISTING gate and adds no
    refusal to it. D2's scope boundary is tag entries in a root document; a
    digest pin is not a tag entry, so `Manifest::Image` is *required* here,
    not rejected. A fail-closed reflex that made `persist_dispatch` refuse a
    bare manifest would break every locked project on the machine — which is
    exactly why the negative half (nothing written) and the positive half (it
    resolves at all) are asserted together.
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
    ocx.plain("--index", str(index_dir), "package", "install", entry.logical_id)

    objects_before = {
        path.relative_to(index_dir).as_posix(): path.read_bytes()
        for path in index_dir.rglob("o/*/*.json")
    }
    assert objects_before, "precondition: the tag resolve must have stored a dispatch object"

    # The lock's own identity: `<logical>@<platform-leaf-digest>` (locks store
    # the platform-leaf digest directly, never an index digest — an index
    # digest is rewritten on every platform push).
    pinned = f"ocx.sh/{repository}@{leaf_digest}"
    result = ocx.plain("--index", str(index_dir), "package", "exec", pinned, "--", "hello", check=False)
    assert result.returncode == 0, (
        f"a pinned leaf digest must resolve, never be refused: rc={result.returncode}\n{result.stderr}"
    )
    assert pkg.marker in result.stdout

    objects_after = {
        path.relative_to(index_dir).as_posix(): path.read_bytes()
        for path in index_dir.rglob("o/*/*.json")
    }
    assert objects_after == objects_before, (
        "a pinned leaf manifest is never copied into the dispatch-object store"
    )


# ---------------------------------------------------------------------------
# A14 — a superseded tag still resolves from the local snapshot
# ---------------------------------------------------------------------------


def test_superseded_tag_still_resolves_from_the_local_dispatch_object(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """The case the snapshot exists for (ADR Context point 3): the publisher
    re-points the tag at a different image index, so the index the snapshot
    named is superseded upstream. The local copy keeps resolving it, and does
    so without asking the fixture again.

    The assertion that carries the test is the request log: after the
    supersession the resolve must land ZERO new requests on the fixture. A
    plain "it still resolves" would also be satisfied by a client that
    re-fetched and silently followed the moved tag — the opposite of a
    snapshot.
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
    snapshot = _dispatch_object_path(index_dir, repository, entry.index_digest.split(":", 1)[1])
    snapshot_bytes = snapshot.read_bytes()

    # Supersede: the publisher re-points `1.0.0` at a DIFFERENT image index and
    # the old one is gone from the served tree entirely — the shape a garbage
    # collected registry leaves behind.
    superseded_leaf = "sha256:" + hashlib.sha256(b"a14-superseding-leaf").hexdigest()
    for stale in (index_server.root / "p" / repository / "o" / "sha256").iterdir():
        stale.unlink()
    static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=superseded_leaf,
        os=os_name,
        architecture=arch_name,
    )
    requests_before = len(index_server.requests)

    result = ocx.plain(
        "--index", str(index_dir), "index", "list", entry.logical_id, "--platforms"
    )
    assert pkg.platform in result.stdout

    assert len(index_server.requests) == requests_before, (
        "a committed snapshot resolves from `o/`; it must not re-ask the source, "
        f"saw {[r.path for r in index_server.requests[requests_before:]]}"
    )
    assert snapshot.read_bytes() == snapshot_bytes, (
        "the committed dispatch object must be untouched by an upstream supersession"
    )


# ---------------------------------------------------------------------------
# A10 — an absent dispatch object recovers from the machine-global blob store
# ---------------------------------------------------------------------------


@pytest.mark.xfail(
    strict=True,
    reason=(
        "Not implemented: a Published source's AbsentDispatch recovery never reaches the "
        "physical registry — `ChainedIndex::walk_chain` asks the published source, which "
        "serves `p/<repo>/o/sha256/<hex>.json` and 404s. Two statements in the tree "
        "contradict each other: `DispatchResolution::AbsentDispatch` documents recovery as "
        "'the machine-global blob store first, then the physical registry', and "
        "adr_oci_index_only_dispatch.md's 'Published absent-dispatch recovers offline' "
        "bullet assumes the install staged the image index into $OCX_HOME/blobs — measured, "
        "an index.ocx.sh-resolved install stages the leaf manifest and the config blob only "
        "(the behaviour test_index_selfcontained.py item 8 pins deliberately). Strict, so "
        "this flips to a failure the day the capability lands rather than rotting."
    ),
)
def test_published_absent_dispatch_recovers_from_the_physical_registry_and_self_heals(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """A published root whose dispatch object is missing EVERYWHERE but the
    physical registry still resolves, and the recovered bytes heal `o/` back.

    This is the case `adr_oci_index_only_dispatch.md` calls impossible before
    it. The root's `content` used to name a document ocx derived for itself:
    no registry could serve that digest, so an incomplete CAS was terminal —
    `GET /v2/<repo>/manifests/sha256:<minted-hex>` is a 404 by construction.
    Now `content` IS the digest the registry served the image index under, so
    an `AbsentDispatch` is recoverable by fetching `content` by digest
    (`ChainedIndex` → `LocalIndex::persist_dispatch` → `stage_dispatch_bytes`).

    Deleting the object from the FIXTURE as well as from the local index is
    what makes the assertion about the physical registry: leave it served and
    the recovery is just an ordinary re-fetch from the source, which proves
    nothing about `content` being registry-addressable.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)

    configure_index_source(ocx, index_server)
    static_index.write_config(index_server.root)
    repository = f"{unique_repo}/pkg"

    # Seeded by hand rather than through `static_index.write_package`: this
    # test's whole premise is that `content` names a digest the PHYSICAL
    # REGISTRY can serve. `write_package` fabricates an image index (an
    # explicit placeholder `size`, sorted keys) that no registry ever stored,
    # so the recovery fetch would 404 by construction and the strict xfail
    # could never flip when the capability lands.
    served_bytes, served_digest = fetch_manifest_raw(ocx.registry, pkg.repo, "1.0.0")
    index_hex = served_digest.split(":", 1)[1]
    root = {
        "repository": f"oci://{ocx.registry}/{pkg.repo}",
        "tags": {"1.0.0": {"content": served_digest, "observed": "2026-01-01T00:00:00Z"}},
    }
    root_bytes = json.dumps(root, sort_keys=True, separators=(",", ":")).encode()
    root_path = index_server.root / "p" / f"{repository}.json"
    root_path.parent.mkdir(parents=True, exist_ok=True)
    root_path.write_bytes(root_bytes)
    served_object = index_server.root / "p" / repository / "o" / "sha256" / f"{index_hex}.json"
    served_object.parent.mkdir(parents=True, exist_ok=True)
    served_object.write_bytes(served_bytes)
    logical_id = f"ocx.sh/{repository}:1.0.0"
    static_index.write_catalog(
        index_server.root, {repository: "sha256:" + hashlib.sha256(root_bytes).hexdigest()}
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", logical_id)

    dispatch = _dispatch_object_path(index_dir, repository, index_hex)
    expected_bytes = dispatch.read_bytes()

    # The root keeps naming `content`; the object is gone from the snapshot AND
    # from the published site. Only the physical registry still has the bytes,
    # addressable by exactly that digest.
    dispatch.unlink()
    for served in (index_server.root / "p" / repository / "o" / "sha256").iterdir():
        served.unlink()
    assert not dispatch.exists()

    result = ocx.plain(
        "--index", str(index_dir), "index", "list", logical_id, "--platforms"
    )
    assert pkg.platform in result.stdout, (
        f"content must be recoverable by digest from the physical registry:\n{result.stderr}"
    )
    assert dispatch.is_file(), "the recovered bytes must heal the object back into `o/`"
    assert dispatch.read_bytes() == expected_bytes, (
        "the healed object must be byte-identical — it is the registry's own image index"
    )


# ---------------------------------------------------------------------------
# Jurisdiction — the INDEX declares what a MISS means; the root always decides.
#
# `config.json`'s `name_segments` is the index operator's own published
# statement about its name grammar. `index.ocx.sh` serves 2, restating its root
# schema's `^ocx\.sh/<ns>/<pkg>$`. For a name of another shape the client still
# asks for the root: a served root keeps the source authoritative (so a wrong
# declaration can bypass nothing), and only a genuine 404 falls through to the
# registry. Absence of the field means "serves every name" — today's behaviour
# verbatim, including the terminal stop.
# ---------------------------------------------------------------------------


def test_flat_name_falls_through_to_the_registry_when_the_index_declares_a_namespace_segment(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """The measured bug: with an index configured for the namespace, a flat
    identifier resolved only when NO index was configured. The index now
    declares `name_segments: 2`, so the one root request for the flat name
    404s and the install falls through to the plain-OCI registry.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    assert "/" not in unique_repo, "the fixture repository must be flat for this test"

    configure_index_source(ocx, index_server, namespace=ocx.registry)
    static_index.write_config(index_server.root, name_segments=2)

    ocx.plain("package", "install", pkg.fq)

    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
        / "1.0.0"
    )
    assert_symlink_exists(candidate)
    probes = [
        record.path
        for record in index_server.requests
        if record.path.startswith(f"/p/{unique_repo}")
    ]
    assert probes == [f"/p/{unique_repo}.json"], (
        "a declined name costs exactly one memoized root probe that 404s, and is "
        f"never dereferenced further: {[record.path for record in index_server.requests]}"
    )


def test_namespaced_name_still_fails_closed_when_the_index_has_no_root(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """Fail-closed survives for every name the index CAN express: a namespaced
    identifier with no root is a terminal stop, never a fall-through the
    registry could shadow. This is the property the yank gate rests on.
    """
    # Published to the registry under a NAMESPACED repository the index declares
    # it can hold — but the index has no root for it.
    namespaced_repo = f"{unique_repo}/tool"
    pkg = make_package(ocx, namespaced_repo, "1.0.0", tmp_path)

    configure_index_source(ocx, index_server, namespace=ocx.registry)
    static_index.write_config(index_server.root, name_segments=2)

    refused = ocx.plain("package", "install", pkg.fq, check=False)
    assert refused.returncode != 0, (
        "an expressible name absent from the index must not resolve through the "
        f"registry behind the index's back:\n{refused.stdout}\n{refused.stderr}"
    )
    assert any(
        record.path == f"/p/{namespaced_repo}.json" for record in index_server.requests
    ), (
        "the index must have been asked — it is authoritative for this name: "
        f"{[record.path for record in index_server.requests]}"
    )


def test_yank_gate_holds_for_an_index_that_declares_no_grammar(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """A private index that declares no `name_segments` keeps full authority
    over every name in its namespace — including a FLAT one — so its yank
    refusal is never bypassed by the plain-OCI catch-all.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    assert "/" not in unique_repo, "the fixture repository must be flat for this test"
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server, namespace=ocx.registry)
    static_index.write_config(index_server.root)  # no name_segments declared
    static_index.write_package(
        index_server.root,
        repository=unique_repo,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
        yanked=True,
    )

    refused = ocx.plain("package", "install", pkg.fq, check=False)
    assert refused.returncode == 65, (
        f"expected DataError(65), got rc={refused.returncode}\n{refused.stderr}"
    )
    assert "yanked" in refused.stderr
    candidate = (
        Path(ocx.env["OCX_HOME"])
        / "symlinks"
        / registry_dir(ocx.registry)
        / unique_repo
        / "candidates"
        / "1.0.0"
    )
    assert_not_exists(candidate)


def test_index_update_reroutes_a_flat_name_to_the_registry(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """`ocx index update` on a declined name refreshes against the registry
    instead of dying in the index source's derived-refresh path.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    assert "/" not in unique_repo, "the fixture repository must be flat for this test"

    configure_index_source(ocx, index_server, namespace=ocx.registry)
    static_index.write_config(index_server.root, name_segments=2)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", pkg.fq)

    assert _root_document_path(
        index_dir, unique_repo, namespace=registry_dir(ocx.registry)
    ).is_file(), (
        "the registry-derived root must be written for a name the index declined"
    )
    probes = [
        record.path
        for record in index_server.requests
        if record.path.startswith(f"/p/{unique_repo}")
    ]
    assert probes == [f"/p/{unique_repo}.json"], (
        "the declined name costs the one jurisdiction probe and is never refreshed "
        f"from the index: {[record.path for record in index_server.requests]}"
    )
