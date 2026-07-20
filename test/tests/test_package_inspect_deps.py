# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package inspect --deps``.

Covers the metadata-only dependency closure walker
(`adr_inspect_metadata_closure.md`): the flat digest-keyed `closure` array,
the aggregated `interface_surface` object, offline-after-warm (goals 4+5),
the offline fail-closed contract for an un-warmed dependency, image-index
platform selection, and diamond dedup (JSON single-node + plain ``(*)``
marker).

Exit codes per quality-rust-exit_codes.md:
    0  = Success
    81 = PolicyBlocked (offline + a closure-reachable blob absent locally)
"""
from __future__ import annotations

from pathlib import Path

from src import OcxRunner
from src.helpers import make_package, make_package_with_entrypoints
from src.registry import fetch_platform_manifest_digest
from src.runner import PackageInfo


def _dep_entry(ocx: OcxRunner, pkg: PackageInfo, *, visibility: str) -> dict:
    """Build a digest-pinned dependency descriptor for `make_package(dependencies=...)`."""
    digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    return {"identifier": f"{pkg.fq}@{digest}", "visibility": visibility}


def _node(closure: list[dict], repo: str) -> dict:
    """Find the one closure node whose identifier names `repo`."""
    matches = [n for n in closure if f"/{repo}:" in n["identifier"] or f"/{repo}@" in n["identifier"]]
    assert len(matches) == 1, f"expected exactly one node for {repo!r}, got {matches}"
    return matches[0]


def test_inspect_deps_closure_and_interface_surface(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Closure lists every node (public + sealed); interface_surface admits
    only the root and the public dep, and reports declared edge visibility.
    """
    dep_public = make_package(
        ocx, f"{unique_repo}_pub", "1.0.0", tmp_path, binaries=["pub-bin"]
    )
    dep_sealed = make_package(
        ocx, f"{unique_repo}_sealed", "1.0.0", tmp_path, binaries=["sealed-bin"]
    )
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        binaries=[],  # declared-empty root claim keeps interface_binaries_complete True
        dependencies=[
            _dep_entry(ocx, dep_public, visibility="public"),
            _dep_entry(ocx, dep_sealed, visibility="sealed"),
        ],
    )

    data = ocx.json("package", "inspect", "--deps", root.short)[root.short]

    closure = data["closure"]
    assert len(closure) == 3, closure

    root_node = _node(closure, unique_repo)
    assert root_node["root"] is True
    assert "effective_visibility" not in root_node, "root must carry no effective_visibility key"

    pub_node = _node(closure, f"{unique_repo}_pub")
    assert pub_node["effective_visibility"] == "public"
    assert "root" not in pub_node
    assert pub_node["binaries"] == ["pub-bin"]

    sealed_node = _node(closure, f"{unique_repo}_sealed")
    assert sealed_node["effective_visibility"] == "sealed"
    assert sealed_node["binaries"] == ["sealed-bin"], "closure lists the sealed dep's own claim"

    edges = {e["identifier"].split("@")[0]: e for e in root_node["dependencies"]}
    assert edges[dep_public.fq]["visibility"] == "public"
    assert edges[dep_public.fq]["name"] == f"{unique_repo}_pub"
    assert edges[dep_sealed.fq]["visibility"] == "sealed"
    assert edges[dep_sealed.fq]["name"] == f"{unique_repo}_sealed"

    surface = data["interface_surface"]
    binary_names = {(b["name"], b["package"]) for b in surface["binaries"]}
    assert ("pub-bin", pub_node["identifier"]) in binary_names
    assert not any(name == "sealed-bin" for name, _ in binary_names), (
        "sealed dep must be excluded from the interface_surface aggregate"
    )
    assert surface["binaries_complete"] is True
    assert surface["conflicts"] == {"entrypoints": [], "repositories": []}


def test_inspect_deps_offline_after_warm_matches_online(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Goals 4+5 acceptance: a warm ``--deps`` walk persists every closure
    blob to local CAS, so a subsequent ``--offline`` walk yields an
    identical closure + interface_surface purely from cache.
    """
    dep = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path)
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
    )

    online = ocx.json("package", "inspect", "--deps", root.short)[root.short]

    offline_result = ocx.run("--offline", "package", "inspect", "--deps", root.short, format="json")
    assert offline_result.returncode == 0, offline_result.stderr
    import json as _json

    offline = _json.loads(offline_result.stdout)[root.short]

    assert offline["closure"] == online["closure"]
    assert offline["interface_surface"] == online["interface_surface"]


def test_inspect_deps_offline_unwarmed_dep_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A dependency never walked (config blob absent locally) fails closed
    under ``--offline`` even though the root's own metadata is cached.
    """
    dep = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path)
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
    )

    # Warm the root's own manifest+config blob (not the dep's — plain
    # `--resolve` never walks declared dependencies).
    ocx.json("package", "inspect", "--resolve", root.short)

    result = ocx.run(
        "--offline", "package", "inspect", "--deps", root.short,
        format=None, check=False,
    )

    assert result.returncode == 81, (
        f"expected 81 (PolicyBlocked), got {result.returncode}\nstderr: {result.stderr}"
    )


def test_inspect_deps_image_index_root_selects_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--deps`` on an image-index reference (no ``-p``) platform-selects
    and yields the Resolved body (``platform`` + ``resolution``) with the
    closure attached — never the metadata-less Candidates body.
    """
    dep = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path)
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
    )

    data = ocx.json("package", "inspect", "--deps", root.short)[root.short]

    assert "candidates" not in data
    assert "platform" in data
    assert "resolution" in data
    assert len(data["closure"]) == 2


def test_inspect_deps_diamond_dedups_to_one_node(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Diamond (app -> {left, right} -> leaf): the shared leaf appears once
    in the JSON closure and renders with a ``(*)`` marker on its repeat
    visit in the plain tree.
    """
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    left = make_package(
        ocx, f"{unique_repo}_left", "1.0.0", tmp_path,
        dependencies=[_dep_entry(ocx, leaf, visibility="public")],
    )
    right = make_package(
        ocx, f"{unique_repo}_right", "1.0.0", tmp_path,
        dependencies=[_dep_entry(ocx, leaf, visibility="public")],
    )
    app = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        dependencies=[
            _dep_entry(ocx, left, visibility="public"),
            _dep_entry(ocx, right, visibility="public"),
        ],
    )

    data = ocx.json("package", "inspect", "--deps", app.short)[app.short]
    closure = data["closure"]
    assert len(closure) == 4, closure  # leaf, left, right, app — leaf deduped
    leaf_nodes = [n for n in closure if f"/{unique_repo}_leaf:" in n["identifier"]]
    assert len(leaf_nodes) == 1

    plain = ocx.plain("package", "inspect", "--deps", app.short)
    assert "(*)" in plain.stdout, plain.stdout


def test_inspect_deps_undeclared_binaries_makes_aggregate_incomplete(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """An interface-admitted dep with NO ``binaries`` key (undeclared, not
    scanned) makes ``interface_surface.binaries_complete`` False — "couldn't
    determine != determined zero".
    """
    dep = make_package(
        ocx, f"{unique_repo}_dep", "1.0.0", tmp_path,
        no_bin_scan=True,  # keep the field genuinely absent, not auto-filled
    )
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        binaries=[],
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
    )

    data = ocx.json("package", "inspect", "--deps", root.short)[root.short]

    dep_node = _node(data["closure"], f"{unique_repo}_dep")
    assert "binaries" not in dep_node, "undeclared binaries must be an absent key, not []"
    assert data["interface_surface"]["binaries_complete"] is False


def test_inspect_deps_entrypoints_admitted_to_interface_surface(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A public dep's declared ``entrypoints`` show up as the node's own
    entrypoint list AND get ``{name, package}`` attribution in
    ``interface_surface.entrypoints``; entrypoints never affect
    ``binaries_complete``.
    """
    dep = make_package_with_entrypoints(
        ocx, f"{unique_repo}_ep", tmp_path, entrypoints=["ep-tool"], tag="1.0.0",
        # Public PATH so Auto bin-scan declares `binaries` (keeps this test
        # isolated from the separate undeclared-binaries case above).
        env=[{
            "key": "PATH", "type": "path", "required": True,
            "value": "${installPath}/bin", "visibility": "public",
        }],
    )
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        binaries=[],
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
    )

    data = ocx.json("package", "inspect", "--deps", root.short)[root.short]

    dep_node = _node(data["closure"], f"{unique_repo}_ep")
    assert dep_node["entrypoints"] == ["ep-tool"]

    surface = data["interface_surface"]
    assert {"name": "ep-tool", "package": dep_node["identifier"]} in surface["entrypoints"]
    assert surface["binaries_complete"] is True
