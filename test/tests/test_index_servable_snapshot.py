# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the servable index snapshot
(`adr_servable_index_snapshot.md`, `design_spec_servable_index_snapshot.md`).

The change this file covers makes a local index snapshot directly servable:
`ocx index update` writes `config.json` beside the tree it authors (C-023), an
**absent** `config.json` reads as format version 1 at every reader (C-005),
`ocx index sync` snapshots a whole source in one command
(C-012/C-013/C-014/C-024), `ocx index regenerate` re-derives a drifted
`c/index.json` from the roots on disk (C-007/C-008/C-010/C-021), and a
`file://` base serves an index with no HTTP server at all
(C-015..C-020).

Scenario coverage (`design_spec` §7), one test per scenario unless noted:

| Scenario | Test |
|---|---|
| S-001 | `test_the_air_gap_pipeline_resolves_the_same_platform_manifest_digest` |
| S-002 + S-016 | `test_a_config_less_tree_resolves_and_a_version_two_tree_is_refused` |
| S-003 | `test_a_file_url_base_resolves_a_tag_with_no_server_at_all` |
| S-004 | `test_a_fleet_snapshot_lands_every_root_and_bounds_its_fan_out` |
| S-005 | `test_frozen_refuses_the_catalog_snapshot_and_permits_regenerate` |
| S-006 | `test_an_unsupported_wire_version_is_refused_identically_on_every_reader` |
| S-007 | `test_an_unreadable_index_document_is_refused_not_read_as_absent` |
| S-008 | `test_a_repeat_update_is_byte_and_mtime_stable` |
| S-009 | `test_regenerate_repairs_a_catalog_without_touching_content` |
| S-010 | `test_a_tree_written_by_an_older_ocx_gains_a_config_on_its_next_update` |
| S-011 | `test_a_bad_index_scheme_fails_at_context_init_in_the_config_class` |
| S-012 | `test_a_source_that_refuses_enumeration_stops_the_snapshot` |
| S-012 | `test_an_absent_catalog_document_is_not_a_served_empty_one` |
| S-012 | `test_a_refresh_failure_leaves_the_packages_that_did_refresh_pinned` |
| S-020 | `test_one_unreachable_registry_does_not_cost_the_others_their_snapshot` |
| S-014 | `test_a_yank_marker_rides_the_snapshot_and_the_served_copy_honours_it` |
| S-015 | `test_a_hand_written_config_json_is_never_rewritten` |
| S-018 | `test_a_dry_run_previews_the_catalog_and_writes_nothing` |
| S-018 | `test_the_dry_run_grammar_refusals_and_offline` |
| S-019 | `test_only_the_update_path_creates_config_json` |

A **second, unrelated** `S-` series also lands in this file: the sync-performance
scenarios of `adr_index_sync_performance.md` §8, which reuse the same numbers
with different meanings. Those tests live in the last section and every one of
them says `sync-perf S-0NN` in its docstring; a bare `S-0NN` is always the
snapshot spec's.

| sync-perf scenario | Test |
|---|---|
| S-001 | `test_an_unchanged_resync_fetches_no_dispatch_object` |
| S-002 | `test_a_refused_dispatch_object_costs_its_own_tag_and_no_other` |
| S-003 | `test_a_transient_unavailable_is_absorbed_without_an_operator_warning` |
| S-005 | `test_a_slow_but_progressing_root_outlives_the_retired_total_deadline` |
| S-007 | `test_a_corrupt_local_dispatch_object_is_repaired_without_a_warning` |
| S-009 | `test_a_dry_run_previews_the_catalog_and_writes_nothing` (request-count half) |
| C-027 | `test_reading_semantics_are_unchanged_by_the_sync_rework` |

**Two scenario halves are not runnable here and are not claimed anywhere
below.** S-002's first half ("a tree built by a *pre-change* ocx resolves as
not-found") and S-010's first half ("a tree carrying `config.json` is read by a
binary predating this change") both need a second, older `ocx` binary; the
harness builds exactly one. What each test does assert is the half this branch
can be held to: the post-change behaviour, against a tree in the pre-change
shape (no `config.json`).

Fixture note: no physical manifest is fetched by a published-source
`ocx index update`. `LocalIndex::persist_dispatch` reads the dispatch object
off the index source and stops there, so every fixture below that does not run
`ocx package install` fabricates its platform digest and never touches the
registry. Only S-001 and S-003 push a real package, because only they install
one.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import sys
import time
from collections import Counter
from collections.abc import Iterator, Sequence
from pathlib import Path

import pytest

from src import OcxRunner, static_index
from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest

NAMESPACE = "ocx.sh"

# The shared refresh loop fans out over at most `INDEX_REFRESH_CONCURRENCY`
# packages (8) and each package's own tag refresh over at most
# `TAG_REFRESH_CONCURRENCY` (64), so C-024 states a ceiling of 512 in-flight
# index requests however large the catalog is — and, since `index sync` flattens
# every registry into that one loop, however many registries are named.
STATED_INFLIGHT_CEILING = 512


# ---------------------------------------------------------------------------
# Fixtures + helpers
# ---------------------------------------------------------------------------


@pytest.fixture()
def index_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    """A local `index.ocx.sh`-shaped HTTP fixture, one per test — mirrors
    `test_index_ocx_sh.py::index_server` (module-scoped fixtures do not cross
    file boundaries).
    """
    root = tmp_path / "origin_site"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


@pytest.fixture()
def air_gap_server(tmp_path: Path) -> Iterator[static_index.StaticIndexServer]:
    """A second static host — the one an operator copies a snapshot onto."""
    root = tmp_path / "air_gap_site"
    root.mkdir()
    with static_index.running(root) as server:
        yield server


def configure_index_source(
    ocx: OcxRunner,
    base_url: str,
    *,
    namespace: str = NAMESPACE,
    insecure_host: str | None = None,
    extra: str = "",
) -> None:
    """Points `[registries."<namespace>"] index` at `base_url`.

    `index` field PRESENCE is the sole protocol-kind marker, per namespace
    (`adr_index_indirection.md` F5a). `trusted_hosts` carries the SSRF guard's
    escape hatch for the loopback `registry:2` instance every root here points
    at (X2, ocx#218). `insecure_host` is the fixture server's `host:port` for a
    plain-HTTP base; a `file://` base needs none.
    """
    registry_host = ocx.registry.split(":", 1)[0]
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f'[registries."{namespace}"]\n'
        f'index = "{base_url}"\n'
        f'trusted_hosts = ["{registry_host}"]\n'
        f"{extra}"
    )
    if insecure_host is not None:
        ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{insecure_host}"


def publish(
    root: Path,
    repositories: Sequence[str],
    *,
    registry: str,
    tag: str = "1.0.0",
    config: bool = True,
) -> dict[str, static_index.PackageEntry]:
    """Writes one root document + dispatch object per repository, plus the
    catalog that lists them, and (unless `config=False`) `config.json`.

    Mirrors `test_index_determinism.py::publish_site`. The platform digest is
    derived from the repository name so the fixture is deterministic and needs
    no registry — see this module's docstring for why that is sound.
    """
    entries = {
        repository: static_index.write_package(
            root,
            repository=repository,
            tag=tag,
            physical_repository=f"oci://{registry}/{repository}",
            platform_digest="sha256:" + hashlib.sha256(repository.encode()).hexdigest(),
        )
        for repository in repositories
    }
    static_index.write_catalog(root, {name: entry.root_digest for name, entry in entries.items()})
    if config:
        static_index.write_config(root)
    return entries


def snapshot(directory: Path) -> dict[str, tuple[bytes, int]]:
    """Every file under `directory`, mapped to its bytes and mtime.

    mtime as well as bytes, because several claims here are about writes that
    do not happen: a rewrite that lands identical bytes still churns a served
    tree's cache validators, and it is exactly what a write-always hook looks
    like from outside.

    Every fixture this walks is built from regular files. C-008 records that
    `regenerate` enumerates neither a symlinked root nor a symlinked directory
    under `p/`, so a fixture staged with links would quietly certify a layout
    the contract excludes.
    """
    return {
        str(path.relative_to(directory)): (path.read_bytes(), path.stat().st_mtime_ns)
        for path in directory.rglob("*")
        if path.is_file()
    }


def error_messages(stderr: str) -> list[str]:
    """The `ERROR`-level lines of an ocx run, with the timestamp/level prefix
    stripped — what an operator reads, comparable across invocations.
    """
    return [line.split(" ERROR ", 1)[1] for line in stderr.splitlines() if " ERROR " in line]


def warning_messages(stderr: str) -> list[str]:
    """The `WARN`-level lines, same treatment as `error_messages`.

    Two sync-perf scenarios turn on a state being absorbed *silently*, and
    silence is only a claim against the level that would otherwise carry the
    noise — hence a reader for that level rather than a substring search over
    the whole stream.
    """
    return [line.split(" WARN ", 1)[1] for line in stderr.splitlines() if " WARN " in line]


def dispatch_object_requests(server: static_index.StaticIndexServer) -> list[str]:
    """Every `p/<repository>/o/sha256/<hex>.json` the fixture has served as a
    **body GET**.

    Method-filtered on purpose. "Zero dispatch-object fetches" is a claim about
    bodies transferred, so a counter that lumped a HEAD in with a GET could not
    tell a skipped object from a probed one. The fixture records both methods
    (`RequestRecord.method`), so the two are separable here and a HEAD still
    shows up in any assertion made over the whole request log.
    """
    return [record.path for record in server.requests if record.method == "GET" and "/o/sha256/" in record.path]


def root_document_requests(server: static_index.StaticIndexServer) -> list[str]:
    """Every `p/<repository>.json` the fixture has served as a body GET — root
    documents only, not the dispatch objects under the same `p/` prefix.
    """
    return [
        record.path
        for record in server.requests
        if record.method == "GET" and record.path.startswith("/p/") and "/o/sha256/" not in record.path
    ]


def dispatch_object_path(repository: str, content_digest: str) -> str:
    """The served path of one tag's dispatch object, from the digest its root
    pins. Derived rather than globbed: a glob picks an arbitrary sibling, and
    every assertion below is about one specific object.
    """
    return f"/p/{repository}/o/sha256/{content_digest.split(':', 1)[1]}.json"


def leaf_blob_data_file(ocx_home: Path, leaf_digest: str) -> Path:
    """The machine-global blob-store `data` file for a leaf platform manifest.

    Layout is `blobs/<registry_slug>/sha256/<hex[0:2]>/<hex[2:32]>/data`;
    mirrors `test_index_ocx_sh.py::_leaf_blob_data_file`.
    """
    hex_digest = leaf_digest.split(":", 1)[1]
    matches = list((ocx_home / "blobs").glob(f"*/sha256/{hex_digest[:2]}/{hex_digest[2:32]}/data"))
    assert len(matches) == 1, f"expected exactly one leaf blob data file, found {matches}"
    return matches[0]


def skip_unless_permissions_bite() -> None:
    """`chmod 000` is not a refusal for `root`, and Windows ignores the mode
    bits entirely — the two cases the EACCES rows are `#[cfg(unix)]` for.
    """
    if sys.platform == "win32":
        pytest.skip("POSIX mode bits do not gate reads on Windows")
    if os.geteuid() == 0:
        pytest.skip("running as root: `chmod 000` does not deny a read")


# ---------------------------------------------------------------------------
# S-001 — the air-gap pipeline, end to end
# ---------------------------------------------------------------------------


def test_the_air_gap_pipeline_resolves_the_same_platform_manifest_digest(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    air_gap_server: static_index.StaticIndexServer,
) -> None:
    """S-001. A connected machine snapshots a source with
    `ocx index sync`; the resulting `$OCX_HOME/index/<ns>/`
    subtree is copied onto a static host verbatim; a clean machine pointed at
    that host installs the package and lands on the **same platform-manifest
    digest** the connected machine pinned.

    The copy is what the snapshot is for, so two properties of it are asserted
    rather than assumed:

    1. It carries `config.json` at the source root — the file whose absence was
       the defect this whole change exists to close (C-023).
    2. Re-snapshotting the served copy reproduces it byte for byte. S-001
       words its parity claim as "byte-identical to `wget --mirror` of the
       served tree", which is circular under its own arrangement — the served
       tree *is* the copy — and this is the non-circular half, the version of
       `adr_oci_index_only_dispatch.md:761-762` with teeth: a snapshot whose
       serialization differed from its source's would be a fixed point of
       nothing, and every round of copying would rewrite the tree.

    Completeness of the copy needs no assertion of its own: the install above
    resolves through it, so any file the snapshot failed to write fails that
    install. A "no 404 in the access log" check was tried here and removed —
    every way to make the copy incomplete either fails the install or trips
    assertion 1, so it could not be the assertion that goes red.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
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

    # ── connected machine: snapshot the whole source ───────────────────────
    connected_index = tmp_path / "connected_index"
    connected_index.mkdir()
    ocx.plain("--index", str(connected_index), "index", "sync", NAMESPACE)

    subtree = connected_index / NAMESPACE
    assert (subtree / "config.json").is_file(), (
        "the snapshot must carry config.json at its source root — its absence is the defect "
        "that made a served copy report every package as not-found"
    )

    # ── copy it onto a static host, verbatim ───────────────────────────────
    shutil.copytree(subtree, air_gap_server.root, dirs_exist_ok=True)

    # ── clean machine, pointed only at the copy ────────────────────────────
    air_gap_home = tmp_path / "air_gap_home"
    air_gap_home.mkdir()
    air_gapped = OcxRunner(ocx.binary, air_gap_home, ocx.registry)
    # The scenario's arrangement: the copied index answers the version choice
    # and `[mirrors]` points artifact traffic at the corp registry, so the
    # logical host is never dialed — it names a namespace, not a server.
    configure_index_source(
        air_gapped,
        air_gap_server.base_url,
        insecure_host=air_gap_server.host,
        extra=f'\n[mirrors."{NAMESPACE}"]\nregistry = "http://{ocx.registry}"\n',
    )
    air_gap_index = tmp_path / "air_gap_index"
    air_gap_index.mkdir()

    air_gapped.plain("--index", str(air_gap_index), "package", "install", entry.logical_id)

    assert leaf_blob_data_file(air_gap_home, leaf_digest).is_file(), (
        "the air-gapped install must resolve to the very platform manifest the connected "
        f"machine pinned ({leaf_digest})"
    )

    # ── (2) re-snapshotting the copy reproduces it byte for byte ───────────
    second_home = tmp_path / "second_home"
    second_home.mkdir()
    second = OcxRunner(ocx.binary, second_home, ocx.registry)
    configure_index_source(second, air_gap_server.base_url, insecure_host=air_gap_server.host)
    second_index = tmp_path / "second_index"
    second_index.mkdir()
    second.plain("--index", str(second_index), "index", "sync", NAMESPACE)

    copied = {name: body for name, (body, _) in snapshot(air_gap_server.root).items()}
    resnapshotted = {name: body for name, (body, _) in snapshot(second_index / NAMESPACE).items()}
    assert resnapshotted == copied, "a snapshot of a served snapshot must reproduce it byte for byte"


# ---------------------------------------------------------------------------
# S-002 + S-016 — a tree with no `config.json` resolves; `format_version: 2`
#                 does not
# ---------------------------------------------------------------------------


def test_a_config_less_tree_resolves_and_a_version_two_tree_is_refused(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
) -> None:
    """S-002 (second half) + S-016. A served tree carrying **no**
    `config.json` — the shape every pre-change `ocx index update` produced —
    resolves every package it holds, and the access log shows the `config.json`
    404 followed by the `p/…` GET that the pre-change client never issued.

    Paired with the negative S-016 demands: the same tree, once it declares
    `format_version: 2`, exits **65**. Absence reads as version 1; it does not
    read as "skip the gate".

    S-002's first half needs a *pre-change* binary to demonstrate the silent
    not-found it fixed, and the harness builds one binary. What is asserted
    instead is the fact that half rested on: the root document is on the
    server's disk the whole time, so a not-found here could only ever have been
    the client's doing.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repository = f"{unique_repo}/pkg"
    entries = publish(index_server.root, [repository], registry=ocx.registry, config=False)

    assert (index_server.root / "p" / f"{repository}.json").is_file(), (
        "precondition: the root document is on the server's disk throughout"
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "update", entries[repository].logical_id, check=False)
    assert result.returncode == 0, (
        f"a tree with no config.json must resolve (absent means version 1), got rc={result.returncode}\n"
        f"{result.stderr}"
    )
    assert (index_dir / NAMESPACE / "p" / f"{repository}.json").is_file()

    served = [(record.path, record.status) for record in index_server.requests]
    config_miss = next(index for index, (path, status) in enumerate(served) if path == "/config.json" and status == 404)
    root_get = next(
        index
        for index, (path, status) in enumerate(served)
        if path == f"/p/{repository}.json" and status == 200
    )
    assert config_miss < root_get, (
        "the client must ask for config.json, take the 404 as version 1, and go on to fetch the "
        f"root document; the access log shows {served}"
    )

    # The negative half: the same tree, now declaring a version this ocx does
    # not implement.
    static_index.write_config(index_server.root, format_version=2)
    refused_index = tmp_path / "refused_index"
    refused_index.mkdir()
    refused = ocx.plain(
        "--index", str(refused_index), "index", "update", entries[repository].logical_id, check=False
    )
    assert refused.returncode == 65, (
        f"a served format_version 2 must be a hard DataError, got rc={refused.returncode}\n{refused.stderr}"
    )
    assert "format_version" in refused.stderr


# ---------------------------------------------------------------------------
# S-003 — serving with no server at all
# ---------------------------------------------------------------------------


def test_a_file_url_base_resolves_a_tag_with_no_server_at_all(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-003. `[registries."corp.example"] index = "file:///srv/…"` resolves the
    tag through the index and fetches the leaf from the physical registry the
    root points at. No HTTP server, no TLS, no `--index` redirection of the
    source itself.

    The namespace carries a dot because `Identifier::parse` reads a first
    segment with no `.` or `:` as part of the repository path, not as a
    registry — a flat namespace would silently resolve against the default
    registry instead.
    """
    namespace = "corp.example"
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, index=False)
    leaf_digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    os_name, arch_name = pkg.platform.split("/")

    site = tmp_path / "srv" / "ocx-index" / "corp"
    site.mkdir(parents=True)
    static_index.write_config(site)
    repository = f"{unique_repo}/tool"
    entry = static_index.write_package(
        site,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{pkg.repo}",
        platform_digest=leaf_digest,
        os=os_name,
        architecture=arch_name,
    )
    static_index.write_catalog(site, {repository: entry.root_digest})

    # The scenario's own arrangement: the index answers the version choice and
    # `[mirrors]` points artifact traffic at the corp registry, so the logical
    # host is never dialed — it names a namespace, not a server.
    configure_index_source(
        ocx,
        f"file://{site}",
        namespace=namespace,
        extra=f'\n[mirrors."{namespace}"]\nregistry = "http://{ocx.registry}"\n',
    )

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    logical = f"{namespace}/{repository}:1.0.0"
    ocx.plain("--index", str(index_dir), "package", "install", logical)

    assert leaf_blob_data_file(Path(ocx.env["OCX_HOME"]), leaf_digest).is_file(), (
        "the tag must resolve through the file:// index and the leaf must come from the registry"
    )
    result = ocx.plain("--offline", "--index", str(index_dir), "package", "exec", logical, "--", "hello")
    assert pkg.marker in result.stdout


# ---------------------------------------------------------------------------
# S-004 — fleet snapshot, and the bound on its fan-out (C-024)
# ---------------------------------------------------------------------------


def test_a_fleet_snapshot_lands_every_root_and_bounds_its_fan_out(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    air_gap_server: static_index.StaticIndexServer,
) -> None:
    """S-004 + C-024's behavioural half. `ocx index sync` over
    a 42-package catalog lands 42 roots and 42 dispatch objects plus exactly
    one `config.json`, at the source root and nowhere else.

    The peak number of requests the fixture is serving at once is the
    measurement C-024 asks for, and both of its magic numbers are measured
    rather than reasoned: a static file is served far faster than the client
    can fan out, so the hold is what makes overlap observable at all, and the
    hold has to be long enough that a widened loop actually shows. At 20 ms the
    fixture saw a peak of 7 with the real bound and 15 with the loop's constant
    raised to 200 — too close to assert on. At 200 ms it saw 8 and 26, which is
    the separation this test is written to.

    The second half measures the same ceiling over **two** registries, and that
    is the half C-024's multi-registry claim actually rests on. The structural
    guards beside it are name denylists over an open vocabulary: a reviewer
    defeated them end to end with `tokio::join!` over two `refresh_packages`
    calls — 1024 in flight, every guard green, because a one-line helper keeps
    the call-site count at one and `tokio::join!` matches no needle. A summed
    peak cannot be evaded by spelling, which is why the assertion lives here.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repositories = [f"{unique_repo}/pkg{number:02d}" for number in range(42)]
    publish(index_server.root, repositories, registry=ocx.registry)
    index_server.hold_seconds = 0.2

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE)

    subtree = index_dir / NAMESPACE
    assert sorted(p.name for p in (subtree / "p" / unique_repo).glob("*.json")) == sorted(
        f"pkg{number:02d}.json" for number in range(42)
    ), "every package the catalog listed must land a root document"
    assert len(list(subtree.rglob("o/sha256/*.json"))) == 42, "each root's dispatch object lands with it"
    assert [str(p.relative_to(index_dir)) for p in index_dir.rglob("config.json")] == [
        f"{NAMESPACE}/config.json"
    ], "exactly one config.json, at the source root"

    assert index_server.peak_in_flight <= STATED_INFLIGHT_CEILING, (
        f"C-024 states a ceiling of {STATED_INFLIGHT_CEILING} in-flight index requests; "
        f"the fixture saw {index_server.peak_in_flight}"
    )
    # The falsifiable bound, and the one this test is really for: 512 is so far
    # above anything a 42-package catalog can reach that asserting it alone
    # would pass with the bound deleted. The loop admits 8 packages at once and
    # each of these roots carries a single tag, so the nested per-tag fan-out is
    # width 1 and the steady state is 8 — twice that leaves room for scheduling
    # while still failing the 26 a 200-wide loop was measured at.
    assert index_server.peak_in_flight <= 16, (
        "the per-package fan-out must stay bounded rather than opening one request per catalog "
        f"entry; the fixture saw {index_server.peak_in_flight} concurrent requests over 42 packages"
    )

    # ── the same ceiling, over two registries in one invocation ──────────────
    #
    # C-024's multi-registry claim: the packages every named registry lists are
    # flattened into ONE bounded loop, so the ceiling is a property of the run
    # and not of the argument count. A per-registry fan-out doubles it while
    # leaving every source-text guard green, so it has to be measured.
    second = "corp.example"
    registry_host = ocx.registry.split(":", 1)[0]
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f'[registries."{NAMESPACE}"]\n'
        f'index = "{index_server.base_url}"\n'
        f'trusted_hosts = ["{registry_host}"]\n'
        f'[registries."{second}"]\n'
        f'index = "{air_gap_server.base_url}"\n'
        f'trusted_hosts = ["{registry_host}"]\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host},{air_gap_server.host}"

    # The second source adds 21 packages beside the 42 this one already holds,
    # so the pair run covers 63. The count does not matter to the bound — what
    # matters is that both sources are wide enough to saturate the budget on
    # their own, so exceeding it can only come from the two overlapping.
    publish(air_gap_server.root, [f"{unique_repo}/far{number:02d}" for number in range(21)], registry=ocx.registry)
    air_gap_server.hold_seconds = 0.2
    # ONE high-water mark across both origins. Two independently measured peaks
    # cannot be added: they need not have occurred at the same instant, so a run
    # correctly holding 8 slots in total shows 8 and 9 and sums to 17. Measured
    # that way this assertion fails on conforming code — which is how the shared
    # counter came to exist.
    index_server.share_in_flight_with(air_gap_server)
    index_server.peak_in_flight = 0

    pair_dir = tmp_path / "pair_dir"
    pair_dir.mkdir()
    ocx.plain("--index", str(pair_dir), "index", "sync", NAMESPACE, second)

    # Non-vacuity first: a run that never reached the second source, or that
    # never overlapped anything, would trivially satisfy the bound below.
    assert (pair_dir / NAMESPACE / "p" / f"{unique_repo}" / "pkg00.json").is_file()
    assert (pair_dir / second / "p" / f"{unique_repo}" / "far00.json").is_file()
    assert air_gap_server.requests, "precondition: the second registry was actually fetched from"
    assert index_server.peak_in_flight > 1, "precondition: some concurrency was observable at all"

    # 8 slots shared across both registries, so the steady state over the pair is
    # the same ~8 one registry reaches. A per-registry loop reaches 8 per source
    # — 16 concurrently — so 12 separates the two with room on both sides rather
    # than sitting on the boundary.
    assert index_server.peak_in_flight <= 12, (
        "naming two registries must not double the in-flight ceiling: the fan-out is over the "
        f"flattened package set, not per registry. Peak across both origins was "
        f"{index_server.peak_in_flight}"
    )


def test_two_registries_overlap_rather_than_draining_one_at_a_time(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    air_gap_server: static_index.StaticIndexServer,
) -> None:
    """S-004's other half — the LOWER bound on the same fan-out.

    The ceiling above says two registries must not exceed one registry's budget.
    It cannot say the budget is actually shared: replacing the flattened set with
    a per-registry loop that awaits each registry in turn passes every assertion
    in this suite and every structural guard, because with 21 packages a
    registry a sequential drain still saturates all 8 slots on each in turn.

    Four packages per registry is what separates the two shapes. Eight slots
    over eight packages means every request is in flight at once if the set is
    flattened, and at most four if it is drained one registry at a time. No
    denylist can see this — a `for` loop over `refresh_packages` names no
    combinator — so it is measured or it is not asserted at all.
    """
    second = "corp.example"
    registry_host = ocx.registry.split(":", 1)[0]
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f'[registries."{NAMESPACE}"]\n'
        f'index = "{index_server.base_url}"\n'
        f'trusted_hosts = ["{registry_host}"]\n'
        f'[registries."{second}"]\n'
        f'index = "{air_gap_server.base_url}"\n'
        f'trusted_hosts = ["{registry_host}"]\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host},{air_gap_server.host}"

    publish(index_server.root, [f"{unique_repo}/near{number}" for number in range(4)], registry=ocx.registry)
    publish(air_gap_server.root, [f"{unique_repo}/far{number}" for number in range(4)], registry=ocx.registry)
    index_server.hold_seconds = 0.2
    air_gap_server.hold_seconds = 0.2
    index_server.share_in_flight_with(air_gap_server)
    index_server.peak_in_flight = 0

    pair_dir = tmp_path / "overlap_dir"
    pair_dir.mkdir()
    ocx.plain("--index", str(pair_dir), "index", "sync", NAMESPACE, second)

    assert (pair_dir / NAMESPACE / "p" / unique_repo / "near0.json").is_file()
    assert (pair_dir / second / "p" / unique_repo / "far0.json").is_file()
    # 6 rather than 8: the roots are fetched before the tags, so the very first
    # and very last moments of the run are narrower than the steady state.
    assert index_server.peak_in_flight >= 6, (
        "the fan-out is over the FLATTENED set, so eight packages across two registries overlap "
        "across both origins rather than being drained one registry at a time. Peak across both "
        f"origins was {index_server.peak_in_flight}"
    )


# ---------------------------------------------------------------------------
# S-005 — policy asymmetry, asserted together
# ---------------------------------------------------------------------------


def test_frozen_refuses_the_catalog_snapshot_and_permits_regenerate(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-005 (and C-021). `--frozen index sync` exits **81**
    with the index home byte-identical afterwards and no request reaching the
    source — the gate precedes source construction. `--frozen index regenerate`
    exits **0** and rewrites the catalog.

    Both halves in one test on purpose: separately they read as two unrelated
    facts, together they document the asymmetry as intentional. `regenerate`
    moves no pin and consults no source, so a freeze has nothing to freeze.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repository = f"{unique_repo}/pkg"
    publish(index_server.root, [repository], registry=ocx.registry)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE)

    before = snapshot(index_dir)
    requests_before = len(index_server.requests)
    frozen = ocx.plain(
        "--frozen", "--index", str(index_dir), "index", "sync", NAMESPACE, check=False
    )
    assert frozen.returncode == 81, (
        f"a frozen catalog snapshot must be PolicyBlocked(81), got rc={frozen.returncode}\n{frozen.stderr}"
    )
    assert snapshot(index_dir) == before, "a refused run must leave the index home byte- and mtime-identical"
    assert len(index_server.requests) == requests_before, (
        "the frozen gate precedes source construction, so nothing may be fetched"
    )

    # `--dry-run` under the same freeze. Documented in three places as still
    # refused and, until this, tested nowhere: the dry run writes nothing, so the
    # tempting reading is that a freeze has nothing to object to. It does — the
    # gate is ahead of enumeration, so a frozen run does not even contact the
    # source, and it must not print a listing that looks like a successful
    # preview.
    frozen_preview = ocx.plain(
        "--frozen", "--index", str(index_dir), "index", "sync", NAMESPACE, "--dry-run", check=False
    )
    assert frozen_preview.returncode == 81, (
        f"--frozen must refuse the preview too, got rc={frozen_preview.returncode}\n{frozen_preview.stderr}"
    )
    assert repository not in frozen_preview.stdout, (
        f"a refused run prints no listing: {frozen_preview.stdout!r}"
    )
    assert len(index_server.requests) == requests_before, (
        "the frozen gate precedes enumeration, so a frozen dry run fetches nothing either"
    )

    # `regenerate` under the same freeze, and it must actually rewrite: seed a
    # catalog entry naming a root that is not on disk, which is the one drift
    # only this verb repairs.
    catalog_path = index_dir / NAMESPACE / "c" / "index.json"
    drifted = static_index.read_catalog(catalog_path)
    drifted[f"{unique_repo}/ghost"] = "sha256:" + "00" * 32
    static_index.write_catalog(catalog_path.parent.parent, drifted)

    regenerated = ocx.json("--frozen", "--index", str(index_dir), "index", "regenerate", NAMESPACE)
    assert regenerated[0]["removed"] == [f"{unique_repo}/ghost"], (
        f"--frozen must permit regenerate and it must drop the ghost entry, got {regenerated}"
    )
    assert f"{unique_repo}/ghost" not in static_index.read_catalog(catalog_path)


# ---------------------------------------------------------------------------
# S-006 — one version rule, five readers, one message
# ---------------------------------------------------------------------------


def test_an_unsupported_wire_version_is_refused_identically_on_every_reader(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-006. A subtree declaring `{"format_version": 2}` exits **65** with the
    **same** message read as `$OCX_HOME/index/<src>/`, via `--index`, from a
    remote base, and via a `file://` base — plus a fifth variant carrying an
    unmodelled sibling key, asserting no field of the config participates in
    the diagnostic.

    Same code and same message on all five is what proves no reader has a rule
    of its own. The remote reader is exercised over the fixture's plain-HTTP
    base rather than `https://`: the scheme is a transport concern the version
    gate never sees, and the harness has no TLS endpoint.
    """
    repository = f"{unique_repo}/pkg"
    logical = f"{NAMESPACE}/{repository}"

    # A complete, resolvable subtree in the pre-change shape, then a v2 config
    # dropped on top of each copy of it.
    site = tmp_path / "site"
    site.mkdir()
    publish(site, [repository], registry=ocx.registry, config=False)

    default_home_subtree = Path(ocx.env["OCX_HOME"]) / "index" / NAMESPACE
    default_home_subtree.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(site, default_home_subtree)
    redirected = tmp_path / "redirected_index"
    shutil.copytree(site, redirected / NAMESPACE)
    shutil.copytree(site, index_server.root, dirs_exist_ok=True)
    file_site = tmp_path / "file_site"
    shutil.copytree(site, file_site)
    sibling_site = tmp_path / "sibling_site"
    shutil.copytree(site, sibling_site)

    for tree in (default_home_subtree, redirected / NAMESPACE, index_server.root, file_site):
        static_index.write_config(tree, format_version=2)
    (sibling_site / "config.json").write_text(json.dumps({"format_version": 2, "published_by": "someone else"}))

    fresh_index = tmp_path / "fresh_index"
    fresh_index.mkdir()
    runs = {}

    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    runs["default index home"] = ocx.plain("--offline", "index", "list", logical, check=False)
    runs["--index"] = ocx.plain("--offline", "--index", str(redirected), "index", "list", logical, check=False)
    runs["remote base"] = ocx.plain("--index", str(fresh_index), "index", "update", logical, check=False)

    configure_index_source(ocx, f"file://{file_site}")
    runs["file:// base"] = ocx.plain("--index", str(fresh_index), "index", "update", logical, check=False)

    configure_index_source(ocx, f"file://{sibling_site}")
    runs["file:// base with an unmodelled sibling key"] = ocx.plain(
        "--index", str(fresh_index), "index", "update", logical, check=False
    )

    for reader, result in runs.items():
        assert result.returncode == 65, (
            f"{reader}: an unsupported wire version must be DataError(65), got rc={result.returncode}\n"
            f"{result.stderr}"
        )

    messages = {reader: error_messages(result.stderr)[-1] for reader, result in runs.items()}
    assert len(set(messages.values())) == 1, (
        f"every reader must refuse an unsupported version with the same message, got {messages}"
    )
    assert "format_version 2" in next(iter(messages.values()))


# ---------------------------------------------------------------------------
# S-007 — unreadable is not absent
# ---------------------------------------------------------------------------


def test_an_unreadable_index_document_is_refused_not_read_as_absent(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-007. A `file://` tree whose `config.json` cannot be read fails
    **69** naming the path — it must not exit 0, must not report not-found,
    and must not be promoted to a v1 index by the absent-means-v1 rule. The
    same holds for an unreadable `p/<ns>/<pkg>.json`.

    Two departures from S-007 as written, both stated so neither is a silent
    reinterpretation:

    * The exit code is **69** (`Unavailable`), not the 75 the scenario text
      names. C-015's table maps every non-ENOENT I/O failure of this transport
      to `IndexHttpFailed`, and C-015's own test note records that 75
      (`TempFail`) is not returned anywhere on this path.
    * The command is `ocx index update`, not `ocx index list`. `index list`
      reads the local index only — for a package the local home does not hold
      it warns and exits 0 without contacting the source at all, so it cannot
      surface a transport failure whatever the transport does.
    """
    skip_unless_permissions_bite()

    site = tmp_path / "site"
    site.mkdir()
    repository = f"{unique_repo}/pkg"
    publish(site, [repository], registry=ocx.registry)
    configure_index_source(ocx, f"file://{site}")
    logical = f"{NAMESPACE}/{repository}"

    for unreadable in (site / "config.json", site / "p" / f"{repository}.json"):
        index_dir = tmp_path / f"index_{unreadable.name}"
        index_dir.mkdir()
        unreadable.chmod(0o000)
        try:
            result = ocx.plain("--index", str(index_dir), "index", "update", logical, check=False)
        finally:
            unreadable.chmod(0o644)

        assert result.returncode == 69, (
            f"an unreadable {unreadable.name} must be Unavailable(69) — never absence, never success — "
            f"got rc={result.returncode}\n{result.stderr}"
        )
        assert str(unreadable) in result.stderr, "the diagnostic must name the path that could not be read"
        assert not list(index_dir.rglob("*.json")), "a refused read must persist nothing"


# ---------------------------------------------------------------------------
# S-008 — a repeat update is byte- and mtime-stable
# ---------------------------------------------------------------------------


def test_a_repeat_update_is_byte_and_mtime_stable(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-008. Running `ocx index update` twice against an unchanged source
    leaves every file under the source subtree — `config.json` included —
    with unchanged bytes **and** unchanged mtime.

    mtime and not merely bytes: a rewrite that happens to produce the same
    bytes still churns a served tree's cache validators and a version-controlled
    checkout's working copy, and it is what a write-always `config.json` hook
    would look like from outside.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repository = f"{unique_repo}/pkg"
    entries = publish(index_server.root, [repository], registry=ocx.registry)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", entries[repository].logical_id)
    assert (index_dir / NAMESPACE / "config.json").is_file(), "precondition: the first update wrote config.json"

    before = snapshot(index_dir)
    ocx.plain("--index", str(index_dir), "index", "update", entries[repository].logical_id)
    after = snapshot(index_dir)

    assert after == before, (
        "a second update against an unchanged source must rewrite nothing; changed entries: "
        f"{sorted(name for name in set(before) | set(after) if before.get(name) != after.get(name))}"
    )


# ---------------------------------------------------------------------------
# S-009 — `regenerate` repairs a catalog without touching content
# ---------------------------------------------------------------------------


def test_regenerate_repairs_a_catalog_without_touching_content(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-009 (C-007/C-008). A catalog carrying a fabricated entry for a root
    that is not on disk and a wrong digest for one that is: `regenerate`
    reports `removed` and `corrected` for exactly those, and afterwards every
    `p/**` file, every `o/**` object and `config.json` are byte-identical.

    The dropped entry is the drift nothing else repairs — `write_root` only
    upserts and the read path's recovery only adds — and it is why this verb
    replaces the map wholesale instead of merging into it.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repositories = [f"{unique_repo}/first", f"{unique_repo}/second"]
    publish(index_server.root, repositories, registry=ocx.registry)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE)

    subtree = index_dir / NAMESPACE
    catalog_path = subtree / "c" / "index.json"
    catalog = static_index.read_catalog(catalog_path)
    catalog[f"{unique_repo}/ghost"] = "sha256:" + "00" * 32
    catalog[repositories[0]] = "sha256:" + "11" * 32
    static_index.write_catalog(subtree, catalog)

    # Everything except the catalog document itself, which this verb exists to
    # rewrite. `c/index.json.etag` is excluded for the same reason: `commit`
    # unconditionally clears a stale one before its no-op early return, so a
    # first run against a tree written by an older ocx drops that one file.
    def content(directory: Path) -> dict[str, tuple[bytes, int]]:
        return {name: value for name, value in snapshot(directory).items() if not name.startswith("c/")}

    before = content(subtree)
    outcome = ocx.json("--index", str(index_dir), "index", "regenerate", NAMESPACE)

    assert outcome[0]["removed"] == [f"{unique_repo}/ghost"], f"expected the ghost entry dropped, got {outcome}"
    assert outcome[0]["corrected"] == [repositories[0]], f"expected the wrong digest corrected, got {outcome}"
    assert outcome[0]["added"] == []
    assert outcome[0]["roots"] == 2

    assert content(subtree) == before, (
        "regenerate writes exactly one path — c/index.json — and must leave every root, every "
        "dispatch object and config.json byte- and mtime-identical"
    )
    repaired = static_index.read_catalog(catalog_path)
    assert set(repaired) == set(repositories)
    for repository in repositories:
        root_bytes = (subtree / "p" / f"{repository}.json").read_bytes()
        assert repaired[repository] == "sha256:" + hashlib.sha256(root_bytes).hexdigest()

    # Idempotence: a second run over the repaired tree changes nothing at all.
    steady = snapshot(subtree)
    second = ocx.json("--index", str(index_dir), "index", "regenerate", NAMESPACE)
    assert (second[0]["added"], second[0]["corrected"], second[0]["removed"]) == ([], [], [])
    assert snapshot(subtree) == steady, "a clean regenerate must not even touch mtimes"


# ---------------------------------------------------------------------------
# S-010 — a tree from an older ocx gains a config on its next update
# ---------------------------------------------------------------------------


def test_a_tree_written_by_an_older_ocx_gains_a_config_on_its_next_update(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-010 (second half) + C-023. An index home in the shape every pre-change
    `ocx index update` left behind — roots and a catalog, no `config.json` —
    gains one on its next update, containing exactly `{"format_version": 1}`
    and nothing else.

    The exact bytes matter twice over. `name_segments` is an operator
    declaration OCX cannot derive from a tree, so writing a guess would narrow
    a served copy's name grammar behind the operator's back; and C-025 fixes
    the encoding (two-space indent, one trailing newline) so an OCX-authored
    config and a renderer-authored one differ in content only, never in form.

    S-010's first half — an older binary ignoring a `config.json` it does not
    model — needs that older binary and is not asserted here.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repository = f"{unique_repo}/pkg"
    entries = publish(index_server.root, [repository], registry=ocx.registry)

    index_dir = tmp_path / "index_dir"
    legacy_subtree = index_dir / NAMESPACE
    legacy_subtree.mkdir(parents=True)
    publish(legacy_subtree, [repository], registry=ocx.registry, config=False)
    assert not (legacy_subtree / "config.json").exists(), "precondition: the pre-change shape has no config.json"

    ocx.plain("--index", str(index_dir), "index", "update", entries[repository].logical_id)

    assert (legacy_subtree / "config.json").read_bytes() == b'{\n  "format_version": 1\n}\n', (
        "the update path writes exactly the two-space-indented, newline-terminated "
        '`{"format_version": 1}` — no name_segments, no producer-specific field'
    )


# ---------------------------------------------------------------------------
# S-011 — a bad scheme fails at the right moment, in the right class
# ---------------------------------------------------------------------------


def test_a_bad_index_scheme_fails_at_context_init_in_the_config_class(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """S-011 (C-018). `[registries."<ns>"] index = "ftp://x"` makes every
    online ocx command exit **78** at context initialisation naming the
    namespace — not 75 at the first index fetch, and not a silent fall-through
    to plain OCI.

    Asserted across three unrelated commands, one of which never touches the
    broken namespace at all: a scheme refused only where it is used would still
    be a scheme accepted by the gate.

    Scope, stated because "every ocx command" overreaches: `--offline` builds
    no index sources at all, so the gate is not reached there. That is the
    documented offline posture and not a hole in the closed scheme set — an
    offline run performs no index traffic to misroute.
    """
    namespace = "broken.example"
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(f'[registries."{namespace}"]\nindex = "ftp://x"\n')

    for command in (
        ["index", "list", f"{namespace}/{unique_repo}/pkg"],
        ["index", "catalog", namespace],
        ["package", "install", f"{unique_repo}/unrelated:1.0.0"],
    ):
        result = ocx.plain(*command, check=False)
        assert result.returncode == 78, (
            f"`ocx {' '.join(command)}`: a scheme outside the closed set is a ConfigError(78) at "
            f"context init, got rc={result.returncode}\n{result.stderr}"
        )
        assert namespace in result.stderr, "the diagnostic must name the misconfigured namespace"
        assert "ftp://x" in result.stderr

    offline = ocx.plain("--offline", "index", "list", f"{namespace}/{unique_repo}/pkg", check=False)
    assert offline.returncode == 0, (
        "an offline run builds no index sources, so it never reaches the gate; recording it here "
        f"keeps the 'every command' claim honest, got rc={offline.returncode}\n{offline.stderr}"
    )


# ---------------------------------------------------------------------------
# S-012 — a source that refuses enumeration, and the absent-vs-empty line
# ---------------------------------------------------------------------------


def test_a_source_that_refuses_enumeration_stops_the_snapshot(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-012 (C-013's authoritative-stop rule). A source whose catalog endpoint
    refuses — 401, the shape an authenticated registry produces — makes
    `ocx index sync` exit non-zero with that source's error on
    stderr. It does not report success over zero packages, and it does not fall
    through to the registry's own repository listing.

    Silent empty success is this plan's recurring failure shape; the structural
    guard that stood in for this test until now is scoped to the enumeration
    function's own source text while the swallow it forbids lives one call
    below it, so only a behavioural test can see the defect at all.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repository = f"{unique_repo}/pkg"
    publish(index_server.root, [repository], registry=ocx.registry)
    index_server.refusals["/c/index.json"] = 401

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE, check=False)

    assert result.returncode == 69, (
        "a refused listing must surface as the source's own Unavailable(69), never as a clean "
        f"snapshot of nothing, got rc={result.returncode}\n{result.stderr}"
    )
    assert "/c/index.json" in result.stderr, "the diagnostic must name the endpoint that refused"
    assert not list(index_dir.rglob("*.json")), "nothing may be written for a snapshot that never enumerated"
    assert any(record.status == 401 for record in index_server.requests), (
        "precondition: the fixture actually refused the catalog request"
    )


def test_an_absent_catalog_document_is_not_a_served_empty_one(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-012 / C-013's added row. A published source that serves **no**
    `c/index.json` is a source that cannot answer the question, and
    `index sync` exits **69**. A source that serves a catalog listing
    **zero** packages has answered, and it exits **0**.

    Collapsing the two is what made the mutating shape exit 0 having written
    nothing and printed nothing, reachable from a wrong path component in the
    base URL or a CDN 404 on the catalog path.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    publish(index_server.root, [f"{unique_repo}/pkg"], registry=ocx.registry)
    (index_server.root / "c" / "index.json").unlink()

    absent_index = tmp_path / "absent_index"
    absent_index.mkdir()
    absent = ocx.plain("--index", str(absent_index), "index", "sync", NAMESPACE, check=False)
    assert absent.returncode == 69, (
        f"an absent catalog document is an authoritative stop, got rc={absent.returncode}\n{absent.stderr}"
    )
    assert "/c/index.json" in absent.stderr, (
        "the operator asked to snapshot a mirror and must be told which document is missing, not "
        f"answered by silence: {absent.stderr}"
    )
    assert not list(absent_index.rglob("*.json"))

    static_index.write_catalog(index_server.root, {})
    empty_index = tmp_path / "empty_index"
    empty_index.mkdir()
    empty = ocx.plain("--index", str(empty_index), "index", "sync", NAMESPACE, check=False)
    assert empty.returncode == 0, (
        f"a served catalog listing zero packages is a clean enumeration, got rc={empty.returncode}\n{empty.stderr}"
    )
    assert f"'{NAMESPACE}' listed no packages" in empty.stderr, (
        "exit 0 having refreshed nothing is the one clean way to snapshot nothing, and it must "
        f"still say so: an operator who mistyped a registry sees only silence otherwise\n{empty.stderr}"
    )


def test_a_poisoned_catalog_key_is_refused_before_anything_is_written(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """C-013 / CWE-22 / CWE-150. A catalog key is authored by whoever runs the
    index, which under `ocx index sync` is by definition someone else. Each key
    becomes an identifier's `repository` verbatim, so it must pass the same
    grammar an argv identifier passes — and the whole enumeration is refused
    (65), fail-closed, rather than the poisoned key being dropped and the rest
    snapshotted, which would mirror a tampered catalog minus the evidence.

    The second key is the one that made this a review finding: validating by
    parsing and discarding the result validates a *decomposition* — the tag is
    split off before the character-class guard runs — while the raw key is what
    gets used. It reached an INFO-level log line and a request URL intact.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repository = f"{unique_repo}/pkg"
    publish(index_server.root, [repository], registry=ocx.registry)
    catalog_path = index_server.root / "c" / "index.json"
    clean = static_index.read_catalog(catalog_path)

    # The U+202E RIGHT-TO-LEFT OVERRIDE below is the fixture, which is why the
    # three PLE2502 directives in this test suppress rather than fix: it asserts
    # ocx rejects a catalog key carrying one and never echoes it back into
    # stderr. Stripping the character would delete the subject of the test.
    for poisoned in (f"../../{unique_repo}/victim", f"{unique_repo}/pkg:‮gnp.exe"):  # noqa: PLE2502
        static_index.write_catalog(index_server.root, {**clean, poisoned: "sha256:" + "00" * 32})
        index_dir = tmp_path / f"index_{abs(hash(poisoned))}"
        index_dir.mkdir()
        result = ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE, check=False)

        assert result.returncode == 65, (
            f"a malformed catalog key is a DataError(65), got rc={result.returncode} for "
            f"{poisoned!r}\n{result.stderr}"
        )
        assert not list(index_dir.rglob("*.json")), (
            f"a refused enumeration writes nothing at all, not even the well-formed keys "
            f"beside {poisoned!r}"
        )
        assert "‮" not in result.stderr, (  # noqa: PLE2502
            "the key is echoed back to the operator and must be neutralized on the way out: "
            f"{result.stderr!r}"
        )
        assert not (index_dir.parent / "victim").exists(), "no path may be created outside the index home"

        # Again at debug level. Nothing in this command speaks at the default
        # level before the refusal, so the assertion above is satisfied by
        # silence — a raw key rendered at debug would pass it. An operator who
        # raised the level to diagnose a suspicious catalog is exactly the one
        # reading those lines, and the key reaches the same terminal.
        verbose_dir = tmp_path / f"verbose_{abs(hash(poisoned))}"
        verbose_dir.mkdir()
        verbose = ocx.plain(
            "--log-level", "debug", "--index", str(verbose_dir), "index", "sync", NAMESPACE, check=False
        )
        assert verbose.returncode == 65
        assert "‮" not in verbose.stderr, (  # noqa: PLE2502
            f"a raised verbosity must not be a way around the neutralization: {verbose.stderr!r}"
        )
        assert len(verbose.stderr) > len(result.stderr), (
            "control: -vv actually produced more output, so the assertion above is not passing "
            "on an empty stream"
        )

    # And the same catalog without the poisoned key still snapshots, so the
    # refusal is the key's and not the fixture's.
    static_index.write_catalog(index_server.root, clean)
    clean_dir = tmp_path / "index_clean"
    clean_dir.mkdir()
    ocx.plain("--index", str(clean_dir), "index", "sync", NAMESPACE)
    assert (clean_dir / NAMESPACE / "p" / f"{repository}.json").is_file()


def test_a_refresh_failure_leaves_the_packages_that_did_refresh_pinned(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-012's second sentence. A catalog snapshot in which one package fails to
    refresh exits non-zero, and the packages that did refresh keep their pins:
    the batch aggregates failures rather than rolling the successes back.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    good = [f"{unique_repo}/first", f"{unique_repo}/second"]
    broken = f"{unique_repo}/broken"
    publish(index_server.root, [*good, broken], registry=ocx.registry)
    (index_server.root / "p" / f"{broken}.json").write_text("{ this is not a root document")

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE, check=False)

    assert result.returncode == 65, (
        f"a malformed root document is a DataError(65) for the batch, got rc={result.returncode}\n{result.stderr}"
    )
    subtree = index_dir / NAMESPACE
    for repository in good:
        assert (subtree / "p" / f"{repository}.json").is_file(), (
            f"{repository} refreshed before the failure and must keep its pin"
        )
    assert not (subtree / "p" / f"{broken}.json").exists()


# ---------------------------------------------------------------------------
# S-020 — several registries are one run, and one failure does not void it
# ---------------------------------------------------------------------------


def test_one_unreachable_registry_does_not_cost_the_others_their_snapshot(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    air_gap_server: static_index.StaticIndexServer,
) -> None:
    """S-020. `ocx index sync` takes several registries, enumerates every one of
    them before refusing any, and still fails afterwards.

    The batch rule is the whole reason the verb takes a list: an early return on
    the first failed enumeration makes a five-registry run as fragile as its
    worst source, and an operator whose overnight mirror job stops at the first
    flaky host has no partial result to show for it. Asserted in BOTH argument
    orders, because a run that only works when the healthy registry comes first
    is passing by accident of the loop, not by contract.

    The failing half is an absent `c/index.json` (69) rather than a refusal, so
    this also pins that the authoritative stop still applies per registry —
    continuing past a failure is not the same as reading it as an empty set.

    Three more shapes follow the loop, each one a rule that shipped with nothing
    holding it: the two failure KINDS meeting (C-012's precedence row), two
    failing registries at once (which is what makes the per-registry log line the
    only report for one of them), and the preview path over the same.
    """
    second = "corp.example"
    registry_host = ocx.registry.split(":", 1)[0]
    config_path = Path(ocx.env["OCX_HOME"]) / "config.toml"
    config_path.write_text(
        f'[registries."{NAMESPACE}"]\n'
        f'index = "{index_server.base_url}"\n'
        f'trusted_hosts = ["{registry_host}"]\n'
        f'[registries."{second}"]\n'
        f'index = "{air_gap_server.base_url}"\n'
        f'trusted_hosts = ["{registry_host}"]\n'
    )
    ocx.env["OCX_INSECURE_REGISTRIES"] = f"{ocx.registry},{index_server.host},{air_gap_server.host}"

    healthy = f"{unique_repo}/pkg"
    publish(index_server.root, [healthy], registry=ocx.registry)
    publish(air_gap_server.root, [f"{unique_repo}/other"], registry=ocx.registry)
    (air_gap_server.root / "c" / "index.json").unlink()

    for order in ([NAMESPACE, second], [second, NAMESPACE]):
        index_dir = tmp_path / f"index_{'_'.join(order).replace('.', '_')}"
        index_dir.mkdir()
        result = ocx.plain("--index", str(index_dir), "index", "sync", *order, check=False)

        assert result.returncode == 69, (
            f"the unreachable registry must still decide the exit whatever its position, got "
            f"rc={result.returncode} for {order}\n{result.stderr}"
        )
        assert (index_dir / NAMESPACE / "p" / f"{healthy}.json").is_file(), (
            f"the healthy registry's snapshot must survive its neighbour's failure, order={order}"
        )
        assert any(
            f"Failed to enumerate the catalog for '{second}'" in message
            for message in error_messages(result.stderr)
        ), (
            f"the per-registry failure line must name the failing registry, order={order}: "
            f"{result.stderr}"
        )

    # C-012's precedence row: an enumeration failure outranks a refresh failure
    # **whatever their argument positions**. The refresh-failing registry goes
    # FIRST in argv, so input-order alone would pick its 65; the rule says 69.
    # Nothing exercised this — S-020 ran one healthy registry and one that failed
    # to enumerate, so the two kinds never met, and `.or()`'s operands can be
    # swapped without a single test noticing.
    (index_server.root / "p" / f"{healthy}.json").write_text("{ this is not a root document")
    mixed_dir = tmp_path / "index_mixed_kinds"
    mixed_dir.mkdir()
    mixed = ocx.plain("--index", str(mixed_dir), "index", "sync", NAMESPACE, second, check=False)
    assert mixed.returncode == 69, (
        f"an enumeration failure (69) outranks a refresh failure (65) even when the refresh "
        f"failure comes first in argv, got rc={mixed.returncode}\n{mixed.stderr}"
    )

    # Both registries broken, which is what actually pins the continue-on-failure
    # reporting: only the lowest-index failure becomes the process error and
    # reaches `main.rs`'s boundary, so the OTHER one appears on stderr through
    # that per-registry line and nowhere else. With a single failing registry the
    # assertion above is also satisfiable by the boundary alone.
    (index_server.root / "c" / "index.json").unlink()
    both_dir = tmp_path / "index_both_broken"
    both_dir.mkdir()
    both = ocx.plain("--index", str(both_dir), "index", "sync", NAMESPACE, second, check=False)
    assert both.returncode == 69, (
        f"two unreachable registries must still exit 69, got rc={both.returncode}\n{both.stderr}"
    )
    reported = error_messages(both.stderr)
    for registry in (NAMESPACE, second):
        assert any(f"Failed to enumerate the catalog for '{registry}'" in message for message in reported), (
            f"every failing registry is reported, not just the one that becomes the process error; "
            f"{registry} missing from {reported}"
        )

    # The dry run refuses the same way, and prints no partial listing: half an
    # answer presented as the answer is the empty-set success C-013 forbids,
    # one registry at a time. Both catalogs are gone by now, so this also covers
    # the all-failed shape on the preview path.
    preview_index = tmp_path / "preview_index"
    preview_index.mkdir()
    preview = ocx.plain(
        "--index", str(preview_index), "index", "sync", NAMESPACE, second, "--dry-run", check=False
    )
    assert preview.returncode == 69, (
        f"a dry run over a registry that cannot be enumerated must fail, got rc={preview.returncode}"
    )
    assert healthy not in preview.stdout, (
        f"no partial preview may be printed when one registry could not be enumerated: {preview.stdout!r}"
    )


def test_a_registry_named_twice_is_one_snapshot_on_both_paths(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """C-012's duplicate-registry row. `ocx index sync <R> <R>` is the same run as
    `ocx index sync <R>`: the list is deduplicated at argv, so the catalog is
    fetched once, the packages are refreshed once, and the dry run prints one
    row per registry rather than one per mention.

    A shell loop or a generated argv makes a repeat trivially reachable, and the
    dedup used to sit on the flattened package set — which made the wet path
    idempotent while the preview still showed the registry twice. Deduplicating
    the argv is what keeps the two paths describing the same run, so both are
    asserted here.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repository = f"{unique_repo}/pkg"
    publish(index_server.root, [repository], registry=ocx.registry)

    index_server.requests.clear()
    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE, NAMESPACE)

    assert (index_dir / NAMESPACE / "p" / f"{repository}.json").is_file()
    catalog_fetches = [record for record in index_server.requests if record.path.endswith("/c/index.json")]
    assert len(catalog_fetches) == 1, (
        f"a registry named twice must be enumerated once, not once per mention: {catalog_fetches}"
    )

    preview = ocx.run("--index", str(index_dir), "index", "sync", NAMESPACE, NAMESPACE, "--dry-run")
    assert json.loads(preview.stdout) == [{"registry": NAMESPACE, "packages": [repository]}], (
        f"the preview describes the run that would happen, so a repeat is one row: {preview.stdout}"
    )


# ---------------------------------------------------------------------------
# S-014 — yank markers ride the snapshot for free
# ---------------------------------------------------------------------------


def test_a_yank_marker_rides_the_snapshot_and_the_served_copy_honours_it(
    ocx: OcxRunner,
    unique_repo: str,
    tmp_path: Path,
    index_server: static_index.StaticIndexServer,
    air_gap_server: static_index.StaticIndexServer,
) -> None:
    """S-014. A source root marking `tags["1.2.3"].yanked` is snapshotted and
    served; a client resolving `pkg:1.2.3` against the served copy is refused
    unless `OCX_ALLOW_YANKED` is set — the identical verdict it gets against
    the origin. No yank-specific code exists in this change; the one shared
    gate answers on both trees.

    One thing S-014 leaves unsaid and this fixture has to face: the yank gate
    fires on the *snapshotting* machine too, so mirroring a yanked tag requires
    the mirror operator's own `OCX_ALLOW_YANKED=1`. That is the gate working as
    specified rather than a special case — a snapshot is a resolve — but a
    fixture written from the scenario text alone snapshots nothing and then
    asserts nothing about the copy.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repository = f"{unique_repo}/pkg"

    # One root, two tags: a clean one and a yanked one. `write_package` emits a
    # single-tag root, so the second tag is merged into it here.
    clean = static_index.write_package(
        index_server.root,
        repository=repository,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{repository}",
        platform_digest="sha256:" + hashlib.sha256(b"clean").hexdigest(),
    )
    yanked_body = static_index.index_bytes("sha256:" + hashlib.sha256(b"yanked").hexdigest())
    yanked_hex = hashlib.sha256(yanked_body).hexdigest()
    yanked_object = index_server.root / "p" / repository / "o" / "sha256" / f"{yanked_hex}.json"
    yanked_object.parent.mkdir(parents=True, exist_ok=True)
    yanked_object.write_bytes(yanked_body)
    root_path = index_server.root / "p" / f"{repository}.json"
    root = json.loads(root_path.read_text())
    root["tags"]["1.2.3"] = {
        "content": f"sha256:{yanked_hex}",
        "observed": "2026-01-01T00:00:00Z",
        "yanked": {"reason": "critical security issue", "at": "2026-02-01T00:00:00Z"},
    }
    root_bytes = json.dumps(root, sort_keys=True, separators=(",", ":")).encode()
    root_path.write_bytes(root_bytes)
    static_index.write_config(index_server.root)
    static_index.write_catalog(
        index_server.root, {repository: "sha256:" + hashlib.sha256(root_bytes).hexdigest()}
    )

    # The origin's verdict, for the comparison the scenario is about.
    origin_index = tmp_path / "origin_index"
    origin_index.mkdir()
    origin_refused = ocx.plain(
        "--index", str(origin_index), "index", "update", f"{NAMESPACE}/{repository}:1.2.3", check=False
    )
    assert origin_refused.returncode == 65

    # Snapshot with the mirror operator's opt-in, then serve the copy.
    snapshot_index = tmp_path / "snapshot_index"
    snapshot_index.mkdir()
    ocx.plain(
        "--index", str(snapshot_index), "index", "sync", NAMESPACE,
        env_overrides={"OCX_ALLOW_YANKED": "1"},
    )
    shutil.copytree(snapshot_index / NAMESPACE, air_gap_server.root, dirs_exist_ok=True)
    assert json.loads((air_gap_server.root / "p" / f"{repository}.json").read_text())["tags"]["1.2.3"]["yanked"], (
        "the yank marker must ride the snapshot verbatim — it is a field of the root document, "
        "not something the snapshot re-derives"
    )

    client_home = tmp_path / "client_home"
    client_home.mkdir()
    client = OcxRunner(ocx.binary, client_home, ocx.registry)
    configure_index_source(client, air_gap_server.base_url, insecure_host=air_gap_server.host)
    client_index = tmp_path / "client_index"
    client_index.mkdir()

    refused = client.plain(
        "--index", str(client_index), "index", "update", f"{NAMESPACE}/{repository}:1.2.3", check=False
    )
    assert refused.returncode == origin_refused.returncode == 65, (
        "the served copy must refuse a yanked tag exactly as the origin does, got "
        f"rc={refused.returncode}\n{refused.stderr}"
    )
    assert "yanked" in refused.stderr

    allowed = client.plain(
        "--index", str(client_index), "index", "update", f"{NAMESPACE}/{repository}:1.2.3",
        env_overrides={"OCX_ALLOW_YANKED": "1"}, check=False,
    )
    assert allowed.returncode == 0, f"the opt-in must allow it, got rc={allowed.returncode}\n{allowed.stderr}"

    clean_resolve = client.plain(
        "--index", str(client_index), "index", "update", clean.logical_id, check=False
    )
    assert clean_resolve.returncode == 0, (
        f"a yank is a per-tag signal; {clean.logical_id} is unaffected, got rc={clean_resolve.returncode}\n"
        f"{clean_resolve.stderr}"
    )


# ---------------------------------------------------------------------------
# S-015 — an existing `config.json` is never rewritten
# ---------------------------------------------------------------------------


def test_a_hand_written_config_json_is_never_rewritten(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-015 (C-023's write-if-absent rule, C-008's no-config rule). A subtree
    seeded with a hand-written `config.json` — an unmodelled key, a
    `name_segments` OCX would never choose (it chooses none), and four-space
    indentation OCX's canonical emitter would never produce — is byte-identical
    after `ocx index update` and after `ocx index regenerate`.

    This is what makes a verbatim `rsync` of a hosted tree survive contact with
    a local update, and what keeps `regenerate` safe to point at a foreign repo.
    The indentation is the load-bearing part of the fixture: a hook that
    re-emitted the file through the canonical serializer while preserving every
    field would still fail this, which a field-wise comparison would miss.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repository = f"{unique_repo}/pkg"
    entries = publish(index_server.root, [repository], registry=ocx.registry)

    index_dir = tmp_path / "index_dir"
    subtree = index_dir / NAMESPACE
    subtree.mkdir(parents=True)
    seeded = json.dumps(
        {"format_version": 1, "name_segments": 2, "published_by": "the operator, by hand"}, indent=4
    ).encode()
    (subtree / "config.json").write_bytes(seeded)

    ocx.plain("--index", str(index_dir), "index", "update", entries[repository].logical_id)
    assert (subtree / "config.json").read_bytes() == seeded, "an existing config.json is never updated"

    ocx.json("--index", str(index_dir), "index", "regenerate", NAMESPACE)
    assert (subtree / "config.json").read_bytes() == seeded, "and regenerate never writes one at all"


# ---------------------------------------------------------------------------
# S-018 — previewing a catalog mirror before committing to it
# ---------------------------------------------------------------------------


def test_a_dry_run_previews_the_catalog_and_writes_nothing(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-018 (C-027). `index sync --dry-run` lists every package the source
    catalog holds, sorted, on stdout and exits **0** — with the index home
    untouched (no new file, no mtime change on an existing one, no
    `config.json` created) and **`sync_patches` never run**.

    The patch-descriptor piggyback is asserted because an "index home
    untouched" check structurally cannot see it: it runs after aggregation,
    does its own network I/O and writes *outside* the index home. It is
    observed through the `warn!` inside `sync_patches` itself
    (`package_manager/tasks/patch_sync.rs`, "patch sync: global descriptor
    check failed"), so the needle proves the call ran rather than that the
    caller reached its call site. The same invocation without `--dry-run` is
    the positive control — a needle with no control is a needle that can
    silently stop matching.

    The `[patches]` tier points at a port nothing listens on, so the piggyback
    fails fast and touches no shared registry state; a failed sync logs just as
    loudly as a successful one, and it is the *running* that is under test.
    """
    configure_index_source(
        ocx,
        index_server.base_url,
        insecure_host=index_server.host,
        extra='\n[patches]\nregistry = "localhost:1"\nrequired = false\n',
    )
    repositories = [f"{unique_repo}/zeta", f"{unique_repo}/alpha"]
    entries = publish(index_server.root, repositories, registry=ocx.registry)

    # A non-empty home, so "no mtime change on existing ones" is a real claim
    # rather than a statement about an empty directory.
    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "update", entries[repositories[0]].logical_id)
    before = snapshot(index_dir)

    del index_server.requests[:]
    preview = ocx.run("--index", str(index_dir), "index", "sync", NAMESPACE, "--dry-run")
    assert preview.returncode == 0
    assert json.loads(preview.stdout) == [{"registry": NAMESPACE, "packages": sorted(repositories)}], (
        f"the preview must list every package the source catalog holds, sorted: {preview.stdout}"
    )
    assert snapshot(index_dir) == before, "a dry run opens nothing under the index home for write"
    # sync-perf S-009's other half, and the one an "index home untouched" check
    # cannot see: enumeration runs and the refresh loop does not. A dry run that
    # fetched every root would still write nothing, so only the request log
    # separates "previewed" from "did the work and discarded it".
    assert sorted(record.path for record in index_server.requests) == ["/c/index.json", "/config.json"], (
        "a dry run costs exactly the enumeration — the source's format declaration and its catalog: "
        f"{[record.path for record in index_server.requests]}"
    )
    assert "patch sync" not in preview.stderr, (
        "the dry run must return before the patch-descriptor piggyback, which writes outside the "
        f"index home: {preview.stderr}"
    )

    # Positive control for both needles: the same invocation, wet.
    wet = ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE)
    assert "patch sync" in wet.stderr, (
        "control: the piggyback does run for a real refresh, so its absence above is the dry run's "
        f"early return and not a needle that stopped matching: {wet.stderr}"
    )
    written = snapshot(index_dir)
    assert written != before, "control: the same invocation without --dry-run does write"
    for repository in repositories:
        assert f"{NAMESPACE}/p/{repository}.json" in written, (
            "the run without --dry-run writes exactly the previewed set"
        )


def test_the_dry_run_grammar_refusals_and_offline(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-018's refusals (C-012's Grammar row and C-027's policy row).

    `--dry-run` belongs to `index sync` alone, so naming it on `index update` is
    **64** whatever follows it — the flag is not part of that verb's grammar at
    all now, which is a stronger refusal than the `conflicts_with` pair it
    replaced. `index sync --dry-run` with no registry is **64** for the same
    reason `index sync` alone is: a preview of nothing is not an answer. And
    `--offline index sync --dry-run` is **81**, because a dry run writes nothing
    but still contacts the source.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    publish(index_server.root, [f"{unique_repo}/pkg"], registry=ocx.registry)
    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()

    for argv, why in (
        (["index", "update", "--dry-run", "cmake"], "--dry-run is not a flag of `index update`"),
        (["index", "update", "--from-catalog", NAMESPACE], "--from-catalog went with it"),
        (["index", "sync", "--dry-run"], "a dry run still has to name a registry"),
        (["index", "update"], "`index update` still requires a package"),
    ):
        refused = ocx.plain("--index", str(index_dir), *argv, check=False)
        assert refused.returncode == 64, (
            f"{why}: expected a usage error, got rc={refused.returncode}\n{refused.stderr}"
        )

    offline = ocx.plain(
        "--offline", "--index", str(index_dir), "index", "sync", NAMESPACE, "--dry-run",
        check=False,
    )
    assert offline.returncode == 81, (
        f"--offline refuses a dry run: enumeration is source contact, got rc={offline.returncode}\n{offline.stderr}"
    )


# ---------------------------------------------------------------------------
# S-019 — only the update path creates `config.json`
# ---------------------------------------------------------------------------


def test_only_the_update_path_creates_config_json(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """S-019 (C-022's containment claim, made executable). Against a
    served/foreign tree that has roots and a catalog but **no** `config.json`,
    the file is still absent after each of `ocx index regenerate`,
    `ocx index list`, `ocx index catalog`, and a resolve that triggers the
    `read_root` catalog self-heal. Only `ocx index update` creates it.

    Scoped to `config.json` and not to the whole index home, per C-022's
    correction: a self-heal changes the catalog map by definition, so
    `c/index.json` **is** rewritten on that step and a whole-home byte
    comparison would fail on the one path the scenario most needs to cover.
    No write-through resolve appears here either — `ChainedIndex` calls
    `commit_published_root` from a grow-on-resolve, which legitimately creates
    the file.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repositories = [f"{unique_repo}/first", f"{unique_repo}/second"]
    entries = publish(index_server.root, repositories, registry=ocx.registry)

    # The foreign tree: the same shape the site serves, dropped into an index
    # home by hand, with no config.json anywhere.
    index_dir = tmp_path / "index_dir"
    subtree = index_dir / NAMESPACE
    subtree.mkdir(parents=True)
    publish(subtree, repositories, registry=ocx.registry, config=False)
    config_path = subtree / "config.json"
    assert not config_path.exists(), "precondition: the foreign tree declares nothing"

    ocx.json("--index", str(index_dir), "index", "regenerate", NAMESPACE)
    assert not config_path.exists(), "regenerate must never inject metadata into a tree under repair"

    ocx.plain("--index", str(index_dir), "index", "list", entries[repositories[0]].logical_id)
    assert not config_path.exists(), "`index list` is a read"

    ocx.json("--index", str(index_dir), "index", "catalog", NAMESPACE)
    assert not config_path.exists(), "`index catalog` is a read"

    # The `read_root` self-heal: delete one catalog entry while its root stays
    # on disk, then read it. The entry is re-derived from the on-disk bytes —
    # a write to `c/index.json`, through the very `CatalogTransaction::commit`
    # the config hook had to be kept out of.
    catalog = static_index.read_catalog(subtree / "c" / "index.json")
    del catalog[repositories[0]]
    static_index.write_catalog(subtree, catalog)
    ocx.plain("--index", str(index_dir), "index", "list", entries[repositories[0]].logical_id)
    assert repositories[0] in static_index.read_catalog(subtree / "c" / "index.json"), (
        "precondition: the self-heal actually fired, so the assertion below is about that path"
    )
    assert not config_path.exists(), (
        "a resolve must not create config.json in a tree OCX may not own — this is why C-023's hook "
        "sits in commit_published_root and not in the shared CatalogTransaction::commit"
    )

    ocx.plain("--index", str(index_dir), "index", "update", entries[repositories[0]].logical_id)
    assert config_path.is_file(), "the update path, and only it, declares the tree an OCX index"


# ---------------------------------------------------------------------------
# `adr_index_sync_performance.md` §8 — the sync-perf scenarios
#
# A SECOND `S-` series, colliding with this file's own by number and meaning.
# Every ID below is written `sync-perf S-0NN` and belongs to
# `adr_index_sync_performance.md`; a bare `S-0NN` anywhere above is the
# snapshot spec's.
#
# What every one of these is really asserting is a REQUEST COUNT. Each scenario
# succeeded before the change too — the pre-change sync fetched every dispatch
# object it already held, and still exited 0. Outcome assertions therefore
# cannot see the work, so they are the control here and the counts are the
# claim.
# ---------------------------------------------------------------------------


def test_an_unchanged_resync_fetches_no_dispatch_object(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """sync-perf S-001 (A3). Re-syncing a source whose every package the local
    copy already holds, unchanged, costs **one** `config.json`, **one**
    `c/index.json`, **one root GET per package** and **zero** dispatch-object
    GETs — and rewrites nothing under the index home.

    The zero is the whole scenario. Before the gate, the second run refetched
    all D dispatch objects, hashed them, and wrote back bytes identical to the
    ones already on disk; it exited 0 either way, so nothing about the outcome
    distinguishes the two. The cold sync above the assertion is the control
    that keeps the zero from being a fixture that serves no objects at all.

    The byte-and-mtime comparison carries the other half of the scenario's
    promise: `CatalogTransaction::commit` writes nothing when the merged map
    equals what it read, and `merge_root` returns `None` when nothing in scope
    changed, so a re-sync leaves a served tree's cache validators untouched.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repositories = [f"{unique_repo}/pkg{number}" for number in range(4)]
    entries = publish(index_server.root, repositories, registry=ocx.registry)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE)

    assert sorted(dispatch_object_requests(index_server)) == sorted(
        dispatch_object_path(repository, entries[repository].index_digest) for repository in repositories
    ), (
        "control: the FIRST sync fetches every dispatch object, so the zero asserted below is a "
        "property of the second run and not of a fixture that serves none"
    )

    before = snapshot(index_dir)
    del index_server.requests[:]
    result = ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE, check=False)

    assert result.returncode == 0, f"an unchanged re-sync succeeds: rc={result.returncode}\n{result.stderr}"
    assert dispatch_object_requests(index_server) == [], (
        "every dispatch object the catalog's roots pin is already on disk and hash-verified, so a "
        f"re-sync must fetch none of them; the fixture served {dispatch_object_requests(index_server)}"
    )
    # Over METHOD and path together, so the claim cannot be evaded by a request
    # shape the count does not look at: a HEAD against a dispatch object is a
    # fetch this scenario forbids just as much as a GET is, and it would land in
    # this multiset as a `("HEAD", ...)` key with no counterpart on the right.
    assert Counter((record.method, record.path) for record in index_server.requests) == Counter(
        [
            ("GET", "/config.json"),
            ("GET", "/c/index.json"),
            *(("GET", f"/p/{repository}.json") for repository in repositories),
        ]
    ), (
        "the whole cost of an unchanged re-sync is one config.json, one catalog and one root per "
        f"package: {sorted(Counter((r.method, r.path) for r in index_server.requests).items())}"
    )
    assert snapshot(index_dir) == before, (
        "an unchanged re-sync rewrites nothing — the tree stays byte- and mtime-identical"
    )


def test_a_refused_dispatch_object_costs_its_own_tag_and_no_other(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """sync-perf S-002 (A7). One tag's dispatch object is unretrievable: every
    other tag of that package is still pinned, the failing tag is not, the run
    exits non-zero, and the other packages in the catalog are untouched.

    **The refusal is a 403 and not the 404 the scenario text names, and that is
    a correction rather than a convenience.** On the published wire an absent
    `o/` object is not an error at all — it is how a single-platform (leaf) tag
    looks, since a leaf's `content` is its own manifest digest and no leaf is
    ever copied into `o/` (A3/B2). Measured against this branch, a 404'd
    dispatch object exits **0** and pins the tag. So "404" cannot be the
    fixture for a failing tag; only a refusal the client may not read as
    absence can be, which is exactly the distinction
    `StaticIndexServer.refusals` exists to model. 403 additionally sits outside
    the retryable set, so the count below is 1 and not 3 — the failure is
    reached once, deliberately, rather than through the ladder.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    partial = f"{unique_repo}/partial"
    untouched = f"{unique_repo}/untouched"
    entry = static_index.write_package(
        index_server.root,
        repository=partial,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{partial}",
        platform_digest="sha256:" + hashlib.sha256(partial.encode()).hexdigest(),
        extra_tags=["2.0.0", "3.0.0"],
    )
    sibling = static_index.write_package(
        index_server.root,
        repository=untouched,
        tag="1.0.0",
        physical_repository=f"oci://{ocx.registry}/{untouched}",
        platform_digest="sha256:" + hashlib.sha256(untouched.encode()).hexdigest(),
    )
    static_index.write_catalog(
        index_server.root, {partial: entry.root_digest, untouched: sibling.root_digest}
    )
    static_index.write_config(index_server.root)

    refused = dispatch_object_path(partial, entry.tag_digests["2.0.0"])
    index_server.refusals[refused] = 403

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE, check=False)

    assert result.returncode == 69, (
        f"an unretrievable dispatch object fails the run: rc={result.returncode}\n{result.stderr}"
    )
    subtree = index_dir / NAMESPACE
    partial_root = subtree / "p" / f"{partial}.json"
    assert partial_root.is_file(), (
        "the package's root is committed at all: one tag's failure must not roll back the tags that "
        "succeeded, and rolling them back looks exactly like never writing the root"
    )
    assert sorted(json.loads(partial_root.read_text())["tags"]) == ["1.0.0", "3.0.0"], (
        "the tags whose objects landed keep their pins and the failing tag gets none — a sibling's "
        "failure must not throw away the work the others completed, and a tag may never be pinned "
        "without the object its pin resolves through"
    )
    for tag in ("1.0.0", "3.0.0"):
        held = subtree / "p" / partial / "o" / "sha256" / f"{entry.tag_digests[tag].split(':', 1)[1]}.json"
        assert held.is_file(), f"tag {tag} is pinned, so its dispatch object must be on disk"
    withheld = subtree / "p" / partial / "o" / "sha256" / f"{entry.tag_digests['2.0.0'].split(':', 1)[1]}.json"
    assert not withheld.exists(), "the refused object is not on disk, which is why its tag is not pinned"

    assert (subtree / "p" / f"{untouched}.json").is_file(), "the other package refreshes on its own terms"
    assert (
        subtree / "p" / untouched / "o" / "sha256" / f"{sibling.index_digest.split(':', 1)[1]}.json"
    ).is_file(), "and lands its dispatch object — one package's failure is not the batch's"
    assert sorted(static_index.read_catalog(subtree / "c" / "index.json")) == sorted([partial, untouched]), (
        "both packages hold a catalog entry: the partial commit narrows what is pinned, never what "
        "the catalog lists"
    )

    assert dispatch_object_requests(index_server).count(refused) == 1, (
        "403 is outside the retryable set, so the refused object is asked for exactly once"
    )


def test_a_transient_unavailable_is_absorbed_without_an_operator_warning(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """sync-perf S-003 (A5). A handful of `503`s mid-sync are retried and the
    run exits **0**, with no operator-facing diagnostic — a retried transient is
    a common benign state, and one warning per retry across a wide fan-out is
    noise rather than signal.

    Three assertions, and each one needs the other two. The exit code alone
    passes on a build with no ladder at all if nothing ever fails; the served
    log is what proves the fixture really did refuse and really was re-asked
    (`503` then `200` on the same path); and the silence is only meaningful
    against a control showing the retry *is* recorded — at `debug`. Without
    that last one, "no warning" is indistinguishable from a retry that never
    happened.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repositories = [f"{unique_repo}/pkg{number}" for number in range(3)]
    entries = publish(index_server.root, repositories, registry=ocx.registry)
    flaky = {
        "/c/index.json",
        f"/p/{repositories[0]}.json",
        dispatch_object_path(repositories[1], entries[repositories[1]].index_digest),
    }
    for path in flaky:
        index_server.transient_refusals[path] = [503]

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    result = ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE, check=False)

    assert result.returncode == 0, (
        f"a transient 503 is retried, not surfaced: rc={result.returncode}\n{result.stderr}"
    )
    served = Counter((record.path, record.status) for record in index_server.requests)
    for path in sorted(flaky):
        assert served[(path, 503)] == 1 and served[(path, 200)] == 1, (
            f"{path} must have been refused once and then served: {sorted(served.items())}"
        )
    for repository in repositories:
        assert (index_dir / NAMESPACE / "p" / f"{repository}.json").is_file(), (
            f"{repository} landed despite the transient failures"
        )
    assert warning_messages(result.stderr) == [], (
        f"a retried transient must not reach the operator: {result.stderr}"
    )
    assert error_messages(result.stderr) == [], f"nor as an error: {result.stderr}"

    # Control for the silence: the same run at `debug` records every retry. A
    # needle asserted absent needs a run where it is present, or "no warning"
    # and "no retry" look the same from here.
    for path in flaky:
        index_server.transient_refusals[path] = [503]
    verbose_dir = tmp_path / "verbose_dir"
    verbose_dir.mkdir()
    verbose = ocx.plain(
        "--log-level", "debug", "--index", str(verbose_dir), "index", "sync", NAMESPACE, check=False
    )
    assert verbose.returncode == 0
    assert "retrying index request" in verbose.stderr, (
        "control: the retry IS recorded, at debug — so the quiet run above is a level decision and "
        f"not a ladder that never ran: {verbose.stderr}"
    )
    assert warning_messages(verbose.stderr) == [], (
        "and raising the level is not a way for it to become a warning"
    )


def test_a_slow_but_progressing_root_outlives_the_retired_total_deadline(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """sync-perf S-005 (A6). A root document that takes longer to arrive than
    the **retired 60 s hard total deadline**, but never stalls past the idle
    bound, now succeeds.

    This test costs its wall clock — a bit over a minute — and there is no way
    around that at this tier: the two bounds are compile-time constants and the
    only injection seam (`ReqwestIndexTransport::with_hardening`) is private to
    the crate, so a fixture in Python has to out-wait the real deadline to say
    anything at all. The unit tier asserts the same semantics in milliseconds;
    what only this tier can show is that the shipped binary carries them.

    The pacing is what makes it a *slow* link rather than a stalled one: the
    body arrives in `SLOW_ROOT_CHUNKS` pieces with `SLOW_ROOT_GAP_SECONDS`
    between them, every gap comfortably inside the 30 s idle bound and every
    piece real progress. The measured elapsed time is asserted, not assumed —
    a fixture that quietly served the body at once would otherwise make the
    scenario pass without ever crossing the deadline it is about.
    """
    slow_chunks = 5
    slow_gap_seconds = 16.0
    retired_total_deadline_seconds = 60

    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repositories = [f"{unique_repo}/slow", f"{unique_repo}/brisk"]
    publish(index_server.root, repositories, registry=ocx.registry)
    index_server.slow_bodies[f"/p/{repositories[0]}.json"] = (slow_chunks, slow_gap_seconds)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    began = time.monotonic()
    result = ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE, check=False)
    elapsed = time.monotonic() - began

    assert elapsed > retired_total_deadline_seconds, (
        "precondition: the fixture has to hold the transfer open longer than the deadline this "
        f"scenario is about, or the success below proves nothing. Elapsed {elapsed:.1f}s"
    )
    assert result.returncode == 0, (
        f"a slow-but-progressing link must not be aborted at a fixed total deadline: "
        f"rc={result.returncode} after {elapsed:.1f}s\n{result.stderr}"
    )
    for repository in repositories:
        assert (index_dir / NAMESPACE / "p" / f"{repository}.json").is_file(), (
            f"{repository} landed — the slow root as much as the brisk one"
        )
    assert root_document_requests(index_server).count(f"/p/{repositories[0]}.json") == 1, (
        "the slow root arrives on its first attempt: a timeout followed by a retry would also end "
        "in success, and would be a different (and worse) behaviour than the one under test"
    )


def test_a_corrupt_local_dispatch_object_is_repaired_without_a_warning(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """sync-perf S-007 (A3's `Err` arm). A truncated `o/` object is refetched
    and repaired on the next sync; every other object is still skipped; the
    exit code is unaffected and no warning is emitted.

    The gate is `read_dispatch_object`, never `Path::exists()`, and this is the
    difference between the two made observable: an existence check reads a
    truncated file as "already held", strands the corruption forever — nothing
    else repairs it — and leaves the run green while the pin no longer
    resolves. Here the corrupt object costs exactly one refetch and its
    siblings cost none, which is a shape neither an existence check nor an
    unconditional refetch can produce.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repositories = [f"{unique_repo}/pkg{number}" for number in range(3)]
    entries = publish(index_server.root, repositories, registry=ocx.registry)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE)

    damaged = repositories[1]
    content_hex = entries[damaged].index_digest.split(":", 1)[1]
    object_path = index_dir / NAMESPACE / "p" / damaged / "o" / "sha256" / f"{content_hex}.json"
    intact = object_path.read_bytes()
    object_path.write_bytes(intact[: len(intact) // 2])
    assert hashlib.sha256(object_path.read_bytes()).hexdigest() != content_hex, (
        "precondition: the file on disk no longer hashes to the digest its root pins"
    )

    del index_server.requests[:]
    result = ocx.plain("--index", str(index_dir), "index", "sync", NAMESPACE, check=False)

    assert result.returncode == 0, (
        f"a self-heal is not a failure: rc={result.returncode}\n{result.stderr}"
    )
    assert dispatch_object_requests(index_server) == [
        dispatch_object_path(damaged, entries[damaged].index_digest)
    ], (
        "exactly one object is refetched — the corrupt one. Its intact siblings are skipped, and "
        f"the corrupt file is not: {dispatch_object_requests(index_server)}"
    )
    assert object_path.read_bytes() == intact, "the refetch overwrote the truncated file with the real bytes"
    assert warning_messages(result.stderr) == [], (
        f"a repaired object is a common benign state: debug and self-heal, never a WARN: {result.stderr}"
    )
    assert error_messages(result.stderr) == [], f"nor an error: {result.stderr}"


def test_reading_semantics_are_unchanged_by_the_sync_rework(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, index_server: static_index.StaticIndexServer
) -> None:
    """C-027 — the non-regression guard the whole initiative is held to: the
    index is central, and **existing read behaviour does not change**. Three
    claims, asserted end to end against a synced home.

    (a) *Local tags are read first.* A resolve the local copy can answer
    contacts no source — proved twice over, because either half alone is weak:
    the request log shows nothing was asked of a source that was up and
    answering, and the same resolve repeated against a **dead** source still
    succeeds, which a lazily-cached-but-still-dialling implementation could
    not do.

    (b) *Offline capability.* `--offline index sync` refuses ahead of any
    network contact (81, zero requests), and every resolve the local copy can
    answer still succeeds under the same flag.

    (c) *`index update`'s remote default.* Unchanged grammar — the verb takes
    package identifiers and no options at all — and unchanged source selection:
    a bare `index update` goes to the configured index source and pins from it.
    """
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    repositories = [f"{unique_repo}/pkg{number}" for number in range(3)]
    entries = publish(index_server.root, repositories, registry=ocx.registry)
    resolved, offline_read, later = (entries[repository] for repository in repositories)

    index_dir = tmp_path / "index_dir"
    index_dir.mkdir()
    # `later` is deliberately excluded from this sync, so (c) has a package the
    # local copy cannot answer for and the remote fetch is observable.
    ocx.plain("--index", str(index_dir), "index", "update", resolved.logical_id, offline_read.logical_id)

    # ── (a) local first ──────────────────────────────────────────────────────
    del index_server.requests[:]
    listed = ocx.plain(
        "--index", str(index_dir), "index", "list", resolved.logical_id, "--platforms", check=False
    )
    assert listed.returncode == 0, f"a locally-held tag resolves: {listed.stderr}"
    assert "linux/amd64" in listed.stdout, (
        f"precondition: the resolve really produced the tag's platform, so the silence below is a "
        f"resolve that happened locally rather than one that did nothing: {listed.stdout}"
    )
    assert index_server.requests == [], (
        "a resolve answerable from the local copy must contact no source; the fixture was asked for "
        f"{[record.path for record in index_server.requests]}"
    )

    # The same resolve with the source pointed at a port nothing listens on. A
    # request log can only say the source was not asked; this says it could not
    # have been.
    configure_index_source(ocx, "http://127.0.0.1:1", insecure_host="127.0.0.1:1")
    dead = ocx.plain(
        "--index", str(index_dir), "index", "list", resolved.logical_id, "--platforms", check=False
    )
    assert dead.returncode == 0 and "linux/amd64" in dead.stdout, (
        f"the local copy answers with no reachable source at all: rc={dead.returncode}\n{dead.stderr}"
    )

    # ── (b) offline ──────────────────────────────────────────────────────────
    configure_index_source(ocx, index_server.base_url, insecure_host=index_server.host)
    del index_server.requests[:]
    refused = ocx.plain("--offline", "--index", str(index_dir), "index", "sync", NAMESPACE, check=False)
    assert refused.returncode == 81, (
        f"--offline refuses the whole command: rc={refused.returncode}\n{refused.stderr}"
    )
    assert index_server.requests == [], (
        "and refuses BEFORE any network contact; the fixture was asked for "
        f"{[record.path for record in index_server.requests]}"
    )
    offline_listed = ocx.plain(
        "--offline", "--index", str(index_dir), "index", "list", offline_read.logical_id, "--platforms",
        check=False,
    )
    assert offline_listed.returncode == 0 and "linux/amd64" in offline_listed.stdout, (
        f"every locally-answerable resolve still succeeds offline: {offline_listed.stderr}"
    )

    # ── (c) `index update`'s grammar and default source ──────────────────────
    options = ocx.plain("index", "update", "-h").stdout.split("Options:", 1)[1]
    assert sorted({word for word in options.split() if word.startswith("--")}) == ["--help"], (
        f"`index update` takes package identifiers and no options; its help now lists: {options}"
    )

    del index_server.requests[:]
    updated = ocx.plain("--index", str(index_dir), "index", "update", later.logical_id, check=False)
    assert updated.returncode == 0, f"a bare `index update` succeeds: {updated.stderr}"
    assert root_document_requests(index_server) == [f"/p/{later.repository}.json"], (
        "with no flags, `index update` goes to the configured index source for the package it names "
        f"and no other: {[record.path for record in index_server.requests]}"
    )
    assert (index_dir / NAMESPACE / "p" / f"{later.repository}.json").is_file(), (
        "and pins it from there"
    )
