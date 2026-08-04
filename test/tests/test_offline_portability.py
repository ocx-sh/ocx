"""Store-portability acceptance suite: copying ONLY ``blobs/`` + ``layers/`` +
``index/`` out of a warmed ``OCX_HOME`` into a brand-new, empty home suffices
to run installs, ``package exec``, and project toolchain execution fully
offline (``OCX_OFFLINE=1``).

This complements ``test_offline.py`` (offline resolution semantics with
transitive deps) and ``test_pinned_offline.py`` (pinned-digest offline exec)
by proving the *storage* contract instead: ``packages/``, ``symlinks/``,
``temp/``, ``projects/``, ``state/`` are all either GC-derived assembly
caches or install-time bookkeeping — a fresh home reconstructs them locally
from the three portable stores without any network access. The mechanism is
the layer-cache fast path (``pull.rs::extract_layer_atomic``): a package
whose layers are already on disk re-assembles into ``packages/`` even when
the manager holds no OCI client at all.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import urllib.error
import urllib.request
from collections.abc import Iterator
from pathlib import Path
from uuid import uuid4

import pytest

from src import OcxRunner, assert_symlink_exists, make_package, registry_dir, static_index
from src.registry import fetch_platform_manifest_digest

# ---------------------------------------------------------------------------
# Store-copy helpers
# ---------------------------------------------------------------------------

_PORTABLE_DIRS = ("blobs", "layers", "index")


def _copy_store(warm_home: Path, fresh_home: Path, *, exclude: str | None = None) -> None:
    """Copy the portable store subdirectories from a warm home into a fresh one.

    Copies ``blobs/``, ``layers/``, ``index/`` (minus ``exclude`` when given).
    Deliberately never copies ``packages/``, ``symlinks/``, ``temp/``,
    ``projects/``, ``state/`` — those are exactly what the tests below prove
    unnecessary to carry across. ``symlinks=True`` mirrors the relocation
    pattern used by ``test_patches.py``'s warm-home copies: preserve internal
    symlinks verbatim rather than dereferencing them.
    """
    fresh_home.mkdir(parents=True, exist_ok=True)
    for name in _PORTABLE_DIRS:
        if name == exclude:
            continue
        src = warm_home / name
        if src.is_dir():
            shutil.copytree(src, fresh_home / name, symlinks=True)


def _fresh_runner(ocx: OcxRunner, home: Path) -> OcxRunner:
    """A second ``OcxRunner`` sharing the binary and registry but pointed at
    ``home`` — the "brand-new empty home" side of the portability contract.
    """
    return OcxRunner(ocx.binary, home, ocx.registry)


def _candidate_current_path(home: Path, registry: str, repo: str) -> Path:
    return home / "symlinks" / registry_dir(registry) / repo / "current"


# ---------------------------------------------------------------------------
# (a) `ocx package install` succeeds fully offline against a copied store
# ---------------------------------------------------------------------------


def test_offline_install_succeeds_from_copied_store(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Warm home installs a package; a fresh home built from only its
    ``blobs/`` + ``layers/`` + ``index/`` installs the same package fully
    offline (the package re-assembles locally — no candidate symlink was
    copied, so this also proves install is not relying on stale symlinks).
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    ocx.json("package", "install", "--select", pkg.short)

    fresh_home = tmp_path / "fresh_home"
    _copy_store(ocx.ocx_home, fresh_home)
    fresh = _fresh_runner(ocx, fresh_home)

    result = fresh.run(
        "package", "install", "--select", pkg.short,
        env_overrides={"OCX_OFFLINE": "1"},
    )
    assert result.returncode == 0, (
        f"offline install against a blobs+layers+index-only copy must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert_symlink_exists(
        _candidate_current_path(fresh_home, ocx.registry, unique_repo),
        "offline install must create the current symlink in the fresh home",
    )


# ---------------------------------------------------------------------------
# (b) `ocx package exec` runs the binary fully offline, no prior install
# ---------------------------------------------------------------------------


def test_offline_package_exec_runs_binary_from_copied_store(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``ocx package exec`` re-assembles on demand and runs the binary fully
    offline against a copied store — no install/symlink step in the fresh
    home first; ``exec`` auto-installs on miss.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    ocx.json("package", "install", "--select", pkg.short)  # warm blobs+layers in home A

    fresh_home = tmp_path / "fresh_home"
    _copy_store(ocx.ocx_home, fresh_home)
    fresh = _fresh_runner(ocx, fresh_home)

    result = fresh.plain(
        "package", "exec", pkg.short, "--", "hello",
        env_overrides={"OCX_OFFLINE": "1"},
    )
    assert result.returncode == 0, (
        f"offline package exec against a blobs+layers+index-only copy must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert pkg.marker in result.stdout, (
        f"expected marker {pkg.marker!r} in offline exec output; got: {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# (c) Project toolchain (`ocx run --`) works fully offline
# ---------------------------------------------------------------------------


def test_offline_project_toolchain_run_succeeds_from_copied_store(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    """A project's ``ocx run -- <bin>`` executes fully offline against a home
    whose store was reconstructed from ``blobs/`` + ``layers/`` + ``index/``
    only. ``ocx.lock`` resolution is index-free by design (locks store the
    platform-leaf digest directly), so this also proves the lock-driven path
    shares the same layer-cache re-assembly as tag-driven install/exec.
    """
    short_id = uuid4().hex[:8]
    repo = f"t_{short_id}_offline_toolchain"
    tag = "1.0.0"
    bin_name = "hello"
    pkg = make_package(ocx, repo, tag, tmp_path, new=True, cascade=False, bins=[bin_name])

    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(f"""\
[tools]
{repo} = "{ocx.registry}/{repo}:{tag}"
""")

    lock = subprocess.run(
        [str(ocx.binary), "lock"], cwd=project, capture_output=True, text=True, env=ocx.env
    )
    assert lock.returncode == 0, f"ocx lock failed: rc={lock.returncode}\nstderr:\n{lock.stderr}"
    pull = subprocess.run(
        [str(ocx.binary), "pull"], cwd=project, capture_output=True, text=True, env=ocx.env
    )
    assert pull.returncode == 0, f"ocx pull failed: rc={pull.returncode}\nstderr:\n{pull.stderr}"

    fresh_home = tmp_path / "fresh_home"
    _copy_store(ocx.ocx_home, fresh_home)
    fresh_env = {**ocx.env, "OCX_HOME": str(fresh_home), "OCX_OFFLINE": "1"}

    result = subprocess.run(
        [str(ocx.binary), "run", "--", bin_name],
        cwd=project,
        capture_output=True,
        text=True,
        env=fresh_env,
    )
    assert result.returncode == 0, (
        f"offline `ocx run` against a blobs+layers+index-only copy must succeed; "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert pkg.marker in result.stdout, (
        f"expected marker {pkg.marker!r} in offline run output; got: {result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# (d) Negative: a home missing one of the three stores fails cleanly offline
# ---------------------------------------------------------------------------


def test_offline_install_missing_index_exits_policy_blocked(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A home carrying ``blobs/`` + ``layers/`` but no ``index/`` cannot
    resolve the unpinned tag offline — exits ``PolicyBlocked`` (81), the same
    documented code as an entirely un-indexed package (see
    ``test_offline.py::test_exit_code_on_offline_blocks_fetch``).
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    ocx.json("package", "install", "--select", pkg.short)

    fresh_home = tmp_path / "fresh_home_no_index"
    _copy_store(ocx.ocx_home, fresh_home, exclude="index")
    fresh = _fresh_runner(ocx, fresh_home)

    result = fresh.run(
        "package", "install", "--select", pkg.short,
        check=False,
        env_overrides={"OCX_OFFLINE": "1"},
    )
    assert result.returncode == 81, (
        f"offline install with no local index/ must exit PolicyBlocked (81); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    # Pins `oci::index::error::Error::PolicyResolutionBlocked`
    # (crates/ocx_lib/src/oci/index/error.rs): "{policy} mode refused to
    # resolve unpinned reference '{identifier}'; ...". "unpinned reference" is
    # unique to this variant's message.
    assert "unpinned reference" in result.stderr.lower(), (
        f"stderr must describe the unresolved-tag policy block; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# (e) Subtree-copy parity: `ocx index update` output == a recursive mirror of
#     the served paths (ADR Validation bullet 11)
# ---------------------------------------------------------------------------


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    root = tmp_path / "static_index_root"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


def _subtree(root: Path) -> dict[str, bytes]:
    """Every file under `root`, keyed by POSIX-relative path — the comparable
    form of a recursive directory diff."""
    return {
        path.relative_to(root).as_posix(): path.read_bytes()
        for path in root.rglob("*")
        if path.is_file()
    }


def test_index_update_subtree_carries_every_file_a_recursive_mirror_would(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """The copy-paste property D1 exists for: an `ocx index update` subtree
    holds exactly the files a `wget --mirror` of the same subtree would — no
    file missing, and none of ocx's own added beside them.

    The ROOT DOCUMENT is authored, not mirrored: ocx merges tags into a root it
    owns and re-emits it through the canonical serializer, so its bytes are the
    site's content in the site's normal form, not the site's bytes. Everything
    below it — the dispatch objects — is registry content stored verbatim, and
    is compared byte-for-byte, which is what each object's own filename digest
    attests.

    Rendering the served tree rather than shelling out to `wget` keeps the
    assertion on the subtree instead of one tool's behaviour: the fixture
    server serves its root directory verbatim, so that directory IS the mirror,
    with no new tool dependency and nothing to run in CI.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    registry_host = ocx.registry.split(":", 1)[0]
    (ocx.ocx_home / "config.toml").write_text(
        f'[registries."ocx.sh"]\nindex = "{index_server.base_url}"\ntrusted_hosts = ["{registry_host}"]\n'
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
    static_index.write_catalog(index_server.root, {repository: entry.root_digest})

    index_home = tmp_path / "index_home"
    index_home.mkdir()
    ocx.plain("--index", str(index_home), "index", "update", entry.logical_id)

    # `p/<ns>` covers both halves of a package's published subtree: the root
    # document (`p/<ns>/<pkg>.json`) and the dispatch-object CAS beside it
    # (`p/<ns>/<pkg>/o/sha256/*.json`). Scoping to it excludes `config.json`
    # and `c/index.json`, which are site-level and not part of the claim.
    namespace = unique_repo
    mirrored = _subtree(index_server.root / "p" / namespace)
    written = _subtree(index_home / "ocx.sh" / "p" / namespace)

    assert mirrored, "the fixture must serve a package subtree, or this proves nothing"
    assert sorted(written) == sorted(mirrored), (
        f"the written subtree must have exactly the mirror's files;\n"
        f"only written: {sorted(set(written) - set(mirrored))}\n"
        f"only mirrored: {sorted(set(mirrored) - set(written))}"
    )
    root_relative = f"{Path(repository).name}.json"
    for relative, expected in mirrored.items():
        if relative == root_relative:
            # Authored, so compared on content rather than bytes.
            assert json.loads(written[relative]) == json.loads(expected), (
                "the authored root must carry the same tags and routing as the mirrored one"
            )
            continue
        assert written[relative] == expected, f"{relative} differs from the mirrored copy"


def test_offline_install_missing_blobs_exits_policy_blocked(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A home carrying ``layers/`` + ``index/`` but no ``blobs/`` resolves the
    tag locally (the index is present) but has no cached manifest content —
    exits ``PolicyBlocked`` (81) with a distinct "not in the local cache"
    message, never falling back to the network under ``--offline``.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    ocx.json("package", "install", "--select", pkg.short)

    fresh_home = tmp_path / "fresh_home_no_blobs"
    _copy_store(ocx.ocx_home, fresh_home, exclude="blobs")
    fresh = _fresh_runner(ocx, fresh_home)

    result = fresh.run(
        "package", "install", "--select", pkg.short,
        check=False,
        env_overrides={"OCX_OFFLINE": "1"},
    )
    assert result.returncode == 81, (
        f"offline install with no local blobs/ must exit PolicyBlocked (81); "
        f"rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    # Pins `PackageErrorKind::OfflineManifestMissing`
    # (crates/ocx_lib/src/package_manager/error.rs): "manifest {digest} is not
    # in the local cache; run `ocx install {identifier}` online to populate
    # it". "populate" is unique to this variant's message and distinct from
    # the missing-index PolicyResolutionBlocked message above.
    assert "populate" in result.stderr.lower(), (
        f"stderr must mention the missing local cache — distinct from the "
        f"missing-index unpinned-reference message; got:\n{result.stderr}"
    )


# ---------------------------------------------------------------------------
# (f) An INDEX-INDIRECTED package installs from a copied store with no index
#     configuration at all — the physical transport address comes from the
#     committed local root
# ---------------------------------------------------------------------------

# The index-bearing namespace this scenario publishes into. Deliberately NOT
# `ocx.sh`: that name is index-bearing from the compiled-in defaults tier
# (`config/loader.rs::builtin_defaults`), so a home with no config would dial
# the real `index.ocx.sh` and the test's outcome would depend on the internet.
# A namespace nobody configured gets no index source at all, which is exactly
# the "no index configuration" the fresh home must survive.
#
# It is a second spelling of the loopback test registry so that logical
# registry == physical registry. That equality is load-bearing: it is the
# "not a rewrite" carve-out in `ChainedIndex::guard_local_physical`, without
# which the read-path SSRF floor refuses a loopback physical target — and a
# home with no index configuration has no source to hold a `trusted_hosts`
# exemption, so there would be no way to allow it.
_INDIRECTED_NAMESPACE = "127.0.0.1:5000"


def _registry_status(registry: str, repo: str, reference: str) -> int:
    """The registry's HTTP status for `GET /v2/<repo>/manifests/<reference>`.

    404 for a repository that was never pushed — registry:2 answers
    `NAME_UNKNOWN` for every endpoint of an absent repository.
    """
    request = urllib.request.Request(
        f"http://{registry}/v2/{repo}/manifests/{reference}",
        headers={"Accept": "application/vnd.oci.image.index.v1+json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code


def test_indirected_install_from_copied_store_without_index_configuration(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """An **index-indirected** package — logical name and physical repository
    are different addresses — installs from a copied store on a machine that
    carries no index configuration whatsoever.

    The four tests above all use `make_package`, whose logical name IS its
    physical repository, so they cannot tell a working physical lookup from a
    missing one: with no rewrite to derive, the logical address is the right
    answer by accident. This one publishes the root at
    `127.0.0.1:5000/<ns>/absent` while the content lives at
    `127.0.0.1:5000/<real repo>`, so the physical address is the only address
    that works.

    Discriminating **by construction**, not by assertion: the logical
    repository is never pushed (asserted below), so `physical_reference`
    answering `None` — which `resolve_transport_pinned` turns back into the
    logical identifier — cannot succeed by accident. `layers/` is deliberately
    left behind by the copy for the same reason: the layer-cache fast path
    (`pull.rs::extract_layer_atomic` step 2) short-circuits before the
    transport address is ever used, so a store carrying its layers would
    install identically whether the physical lookup worked or not.

    Regression for `LocalIndex::physical_reference` + `ChainedIndex`'s
    local-root fallback: without them the only holder of the physical pointer
    is the index site, and a fresh home has no way to ask it.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    logical_repository = f"{unique_repo}/absent"
    logical_id = f"{_INDIRECTED_NAMESPACE}/{logical_repository}:1.0.0"

    # The warm home is the only one that knows the index site exists.
    (ocx.ocx_home / "config.toml").write_text(
        f'[registries."{_INDIRECTED_NAMESPACE}"]\n'
        f'index = "{index_server.base_url}"\n'
        f'trusted_hosts = ["127.0.0.1"]\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = (
        f"{ocx.registry},{_INDIRECTED_NAMESPACE},{index_server.host}"
    )

    static_index.write_config(index_server.root)
    entry = static_index.write_package(
        index_server.root,
        repository=logical_repository,
        tag="1.0.0",
        physical_repository=f"oci://{_INDIRECTED_NAMESPACE}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    static_index.write_catalog(index_server.root, {logical_repository: entry.root_digest})

    # The property the whole test rests on: the logical repository does not
    # exist on the registry, so the transport can only be the physical one.
    assert _registry_status(ocx.registry, logical_repository, "1.0.0") == 404, (
        f"the logical repository {logical_repository!r} must be absent from the "
        f"registry, or a fallback to the logical address could succeed by accident"
    )
    assert _registry_status(ocx.registry, pkg.repo, pkg.tag) == 200, (
        f"the physical repository {pkg.repo!r} must be present, or the install "
        f"below would fail for the wrong reason"
    )

    ocx.plain("package", "install", logical_id)

    # Carry the index and the manifest blobs across, but NOT the layers — see
    # the docstring: the layer cache would otherwise make the fetch, and hence
    # the physical address, unnecessary.
    fresh_home = tmp_path / "fresh_home_indirected"
    _copy_store(ocx.ocx_home, fresh_home, exclude="layers")
    assert not (fresh_home / "layers").exists(), "precondition: the fresh home must have no layers"
    copied_root = (
        fresh_home / "index" / registry_dir(_INDIRECTED_NAMESPACE) / "p" / f"{logical_repository}.json"
    )
    assert copied_root.is_file(), "precondition: the copied index must carry the committed root document"

    fresh = _fresh_runner(ocx, fresh_home)
    # Transport policy only — the loopback registry speaks plain HTTP. No
    # `config.toml` is written, so the fresh home has no index source, no
    # `trusted_hosts`, and no knowledge that `index_server` exists.
    fresh.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{_INDIRECTED_NAMESPACE}"
    assert not (fresh_home / "config.toml").exists(), "the fresh home must carry no configuration"
    served_before = len(index_server.requests)

    result = fresh.run("package", "install", "--select", logical_id, check=False)
    assert result.returncode == 0, (
        f"an indirected package must install from a copied store with no index "
        f"configuration; rc={result.returncode}\nstderr:\n{result.stderr}"
    )
    assert_symlink_exists(
        _candidate_current_path(fresh_home, _INDIRECTED_NAMESPACE, logical_repository),
        "the install must create the current symlink under the LOGICAL name",
    )
    assert (fresh_home / "layers").is_dir(), (
        "the install must have fetched layer content — without a fetch the "
        "physical address was never consulted and this proves nothing"
    )
    assert len(index_server.requests) == served_before, (
        "the fresh home must not have reached the index site at all; served: "
        f"{[r.path for r in index_server.requests[served_before:]]}"
    )
