"""The local index copy is the package tier's lock.

Two properties, both about what does NOT move:

- Resolving the same `<pkg>:<tag>` twice gives the same digest, even when the
  registry moved the tag in between and `ocx clean` evicted the content.
- A pin moves only for a package the user names in `ocx index update <pkg>`, or
  opts in to with `--all`. Updating one package never disturbs another's root
  document — tag pins or `repository` routing pointer.

The second property is what the catalog piggyback used to break: it re-snapshotted
every already-materialized package whose remote root digest had moved, so an
`ocx index update cmake` silently re-pinned every other package on the machine.
"""

from __future__ import annotations

import hashlib
import json
from collections.abc import Iterator
from pathlib import Path

import pytest

from src import OcxRunner, static_index
from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest
from src.runner import registry_dir

NAMESPACE = "ocx.sh"


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


def configure_index_source(ocx: OcxRunner, server: static_index.StaticIndexServer) -> None:
    """Points `[registries."ocx.sh"] index` at the fixture and trusts both hosts.

    Mirrors `test_index_selfcontained.py::configure_index_source` — the physical
    manifests these roots point at live on the loopback `registry:2` instance,
    which the default-on SSRF guard otherwise refuses.
    """
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    registry_host = ocx.registry.split(":", 1)[0]
    config_path.write_text(
        f'[registries."{NAMESPACE}"]\nindex = "{server.base_url}"\ntrusted_hosts = ["{registry_host}"]\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{server.host}"


def local_root(ocx: OcxRunner, repository: str) -> Path:
    return ocx.ocx_home / "index" / registry_dir(NAMESPACE) / "p" / f"{repository}.json"


def publish_site(
    server: static_index.StaticIndexServer,
    repositories: dict[str, str],
    physical_repository: str,
) -> None:
    """(Re)writes every root plus the catalog that describes them.

    `repositories` maps each `<ns>/<pkg>` path to the platform digest its tag
    should resolve to — changing that digest is what moves the root, exactly as
    a re-publish does at the real site.
    """
    static_index.write_config(server.root)
    entries = {
        repository: static_index.write_package(
            server.root,
            repository=repository,
            tag="1.0",
            physical_repository=physical_repository,
            platform_digest=platform_digest,
        ).root_digest
        for repository, platform_digest in repositories.items()
    }
    static_index.write_catalog(server.root, entries)


def publish_multi_tag_root(
    server: static_index.StaticIndexServer,
    repository: str,
    physical_repository: str,
    tags: dict[str, str],
) -> None:
    """Writes one root carrying several tags, plus each tag's dispatch object.

    `static_index.write_package` emits a single-tag root; a copy that must
    outlive the remote dropping a tag needs more than one to begin with.
    """
    entries = {}
    for tag, platform_digest in tags.items():
        body = static_index.index_bytes(platform_digest)
        digest = hashlib.sha256(body).hexdigest()
        path = server.root / "p" / repository / "o" / "sha256" / f"{digest}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body)
        entries[tag] = {"content": f"sha256:{digest}", "observed": "2026-01-01T00:00:00Z"}

    root = {"repository": physical_repository, "tags": entries}
    root_bytes = json.dumps(root, sort_keys=True, separators=(",", ":")).encode()
    root_path = server.root / "p" / f"{repository}.json"
    root_path.parent.mkdir(parents=True, exist_ok=True)
    root_path.write_bytes(root_bytes)

    static_index.write_config(server.root)
    static_index.write_catalog(
        server.root, {repository: "sha256:" + hashlib.sha256(root_bytes).hexdigest()}
    )


def test_a_tag_the_remote_dropped_survives_in_the_local_copy(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """The local index is authored, not mirrored: it records what this machine
    snapshotted. A package the site stops listing a version for keeps resolving
    that version here — an update adds and moves pins, it never deletes one.

    Without that, a publisher retiring an old tag would silently break every
    machine still pinned to it, which is the opposite of what a lock is for.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path / "v2", new=False, index=False)
    old = fetch_platform_manifest_digest(ocx.registry, pkg.repo, "1.0.0")
    new = fetch_platform_manifest_digest(ocx.registry, pkg.repo, "2.0.0")
    configure_index_source(ocx, index_server)

    repository = f"{unique_repo}/tool"
    physical = f"oci://{ocx.registry}/{pkg.repo}"
    publish_multi_tag_root(index_server, repository, physical, {"1.0": old, "2.0": new})
    ocx.plain("index", "update", f"{NAMESPACE}/{repository}")

    before = json.loads(local_root(ocx, repository).read_text())
    assert sorted(before["tags"]) == ["1.0", "2.0"], "prerequisite: both tags snapshotted"

    # The site retires 1.0 — it now publishes only 2.0.
    publish_multi_tag_root(index_server, repository, physical, {"2.0": new})
    ocx.plain("index", "update", f"{NAMESPACE}/{repository}")

    after = json.loads(local_root(ocx, repository).read_text())
    assert sorted(after["tags"]) == ["1.0", "2.0"], (
        f"a retired tag must survive in the local copy, got {sorted(after['tags'])}"
    )
    assert after["tags"]["1.0"] == before["tags"]["1.0"], "and keep the digest it was pinned to"


def test_updating_one_package_leaves_every_other_root_untouched(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """`ocx index update <A>` moves A's pins and nobody else's, and reports the
    updates it did not take.

    Both packages have moved at the site. The old catalog piggyback would
    re-snapshot both — B's root, its tag pins and its `repository` pointer all
    silently replaced by a command that named only A.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    make_package(ocx, unique_repo, "2.0.0", tmp_path / "v2", new=False, index=False)
    first = fetch_platform_manifest_digest(ocx.registry, pkg.repo, "1.0.0")
    second = fetch_platform_manifest_digest(ocx.registry, pkg.repo, "2.0.0")
    assert first != second, "prerequisite: the two versions must be distinct manifests"
    configure_index_source(ocx, index_server)

    named, bystander = f"{unique_repo}/named", f"{unique_repo}/bystander"
    physical = f"oci://{ocx.registry}/{pkg.repo}"
    publish_site(index_server, {named: first, bystander: first}, physical)

    # Materialize both, so each holds a committed root worth protecting.
    ocx.plain("index", "update", f"{NAMESPACE}/{named}:1.0", f"{NAMESPACE}/{bystander}:1.0")
    committed = {
        repository: local_root(ocx, repository).read_bytes() for repository in (named, bystander)
    }
    assert all(committed.values()), "prerequisite: both packages must be materialized"

    # The site moves BOTH roots — each tag now resolves to the other manifest.
    publish_site(index_server, {named: second, bystander: second}, physical)

    ocx.plain("index", "update", f"{NAMESPACE}/{named}:1.0")

    assert local_root(ocx, named).read_bytes() != committed[named], (
        "the package the user named must take its update"
    )
    assert local_root(ocx, bystander).read_bytes() == committed[bystander], (
        "a package nobody named must keep its root document byte-for-byte"
    )

    # Taking it is the same command, named — there is no whole-index sync.
    ocx.plain("index", "update", f"{NAMESPACE}/{bystander}:1.0")
    assert local_root(ocx, bystander).read_bytes() != committed[bystander], (
        "naming the reported package must take the update the first run left"
    )


def test_a_flat_name_the_index_cannot_express_refreshes_via_the_registry(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
):
    """An index declaring `name_segments: 2` 404s a flat name's root, which is
    exactly how the client reads the declaration. The refresh must ask which
    source answers for the package rather than assuming the configured index
    does, or it hands the package to the one source guaranteed to fail it.

    The index is attached to the TEST registry's own namespace here, so the
    fallback has a real registry to reach — the shape a fleet has, an index in
    front of a registry that also serves the flat names directly.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    namespace = ocx.registry
    Path(ocx.env["OCX_HOME"], "config.toml").write_text(
        f'[registries."{namespace}"]\nindex = "{index_server.base_url}"\n'
        f'trusted_hosts = ["{namespace.split(":", 1)[0]}"]\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host}"
    static_index.write_config(index_server.root, name_segments=2)

    result = ocx.plain("index", "update", pkg.repo, check=False)
    assert result.returncode == 0, (
        f"a flat name the index declines must refresh via the registry, got {result.returncode}: {result.stderr}"
    )
    root = ocx.ocx_home / "index" / registry_dir(namespace) / "p" / f"{pkg.repo}.json"
    assert root.is_file(), "the registry fallback must have written a derived root"


def test_index_update_requires_a_package(ocx: OcxRunner):
    """There is no whole-index sync, so a bare invocation has nothing to mean —
    a usage error (64), never a silent guess at "everything"."""
    result = ocx.plain("index", "update", check=False)
    assert result.returncode == 64, (
        f"a bare `index update` must exit 64 (EX_USAGE), got {result.returncode}: {result.stderr}"
    )


def test_reinstalling_a_tag_resolves_to_the_first_install_digest(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A tag re-pushed under the same name does not change what an already-known
    tag resolves to, even after `ocx clean` evicted the content.

    The committed root pins tag -> digest; GC may evict blob CONTENT (refetched
    by digest, byte-stable) but never identity. `--remote` or
    `ocx index update <pkg>` is how a user asks for the new one.

    This is the end-to-end net for that pin stability. It does NOT exercise the
    digest-addressed recovery walk: a multi-platform tag's dispatch object lives
    in `o/`, which is outside GC, so `ocx clean` never makes this resolve reach a
    source at all. The recovery path is unit-covered where a bare-leaf pin can be
    constructed (`chain_refs_tests::absent_dispatch_resolve_*`).
    """
    def install_digest() -> str:
        """The digest `ocx package install` actually resolved the tag to.

        The report is keyed by the identifier as given, and each entry's
        `identifier` is the canonical resolved form — tag plus the digest it
        pinned to.
        """
        report = ocx.json("package", "install", pkg.short)
        identifier = report[pkg.short]["identifier"]
        _, _, digest = identifier.partition("@")
        assert digest.startswith("sha256:"), f"expected a pinned identifier, got {identifier!r}"
        return digest

    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    published = fetch_platform_manifest_digest(ocx.registry, pkg.repo, "1.0.0")
    first = install_digest()

    # Re-push the SAME tag: `make_package` bakes a fresh marker per call, so the
    # content — and therefore the manifest the tag now points at — differs.
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "republish", new=False, index=False)
    assert fetch_platform_manifest_digest(ocx.registry, pkg.repo, "1.0.0") != published, (
        "prerequisite: the re-push must have moved the tag at the registry"
    )

    ocx.plain("package", "uninstall", pkg.short)
    ocx.plain("clean", "--force")

    again = install_digest()
    assert again == first, (
        "a committed pin must survive a re-pushed tag and a GC sweep — "
        "the local index is the lock, and nobody asked to update it"
    )
