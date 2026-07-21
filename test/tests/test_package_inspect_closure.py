# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package inspect --closure``.

Covers the metadata-only dependency closure walker: the single top-level
``closure`` object (``closure.deps`` — transitive dependencies in
transitive-closure order, root excluded; ``closure.surface.{interface,
private}`` — the two symmetric admitted-projection summaries; and
``closure.conflicts``), offline-after-warm, the offline fail-closed contract
for an un-warmed dependency, image-index platform selection, and diamond
dedup (each shared dep listed exactly once, both in JSON ``closure.deps`` and
in the flat plain ``deps`` section — no ``(*)`` marker, no nesting).

Exit codes per quality-rust-exit_codes.md:
    0  = Success
    81 = PolicyBlocked (offline + a closure-reachable blob absent locally)
"""
from __future__ import annotations

import re
from pathlib import Path

from src import OcxRunner
from src.helpers import make_package, make_package_with_entrypoints
from src.registry import fetch_platform_manifest_digest
from src.runner import PackageInfo


def _dep_entry(ocx: OcxRunner, pkg: PackageInfo, *, visibility: str) -> dict:
    """Build a digest-pinned dependency descriptor for `make_package(dependencies=...)`."""
    digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    return {"identifier": f"{pkg.fq}@{digest}", "visibility": visibility}


def _node(deps: list[dict], repo: str) -> dict:
    """Find the one closure['deps'] node whose identifier names `repo`."""
    matches = [n for n in deps if f"/{repo}:" in n["identifier"] or f"/{repo}@" in n["identifier"]]
    assert len(matches) == 1, f"expected exactly one node for {repo!r}, got {matches}"
    return matches[0]


def _env_keys_for_repo(env: list[dict], repo: str) -> set[str]:
    """Env keys attributed to the package whose identifier names `repo`."""
    return {
        entry["key"]
        for entry in env
        if f"/{repo}:" in entry["package"] or f"/{repo}@" in entry["package"]
    }


def _tree_node_line_count(stdout: str, label: str) -> int:
    """Count plain-tree lines whose *label* (not just some substring of the
    line) is exactly `label`. Every dependency leaf also carries a digest
    annotation embedding its full identifier, which itself contains the
    repo's short name as a substring — a whole-output substring count would
    double-count a single leaf line. Anchor on the tree connector
    (``── ``) immediately preceding the label and the `` · `` annotation
    separator immediately following it, which only the label position
    satisfies.
    """
    pattern = re.compile(rf"── {re.escape(label)} ·")
    return len(pattern.findall(stdout))


def test_inspect_closure_closure_and_surface(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``closure.deps`` lists every transitive dependency (public + sealed)
    but never the root; both ``closure.surface.interface`` and
    ``closure.surface.private`` admit only the public dep — sealed has
    neither the interface nor the private axis set.
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

    data = ocx.json("package", "inspect", "--closure", root.short)[root.short]

    deps = data["closure"]["deps"]
    assert len(deps) == 2, deps
    assert not any(f"/{unique_repo}:" in d["identifier"] for d in deps), (
        "the root must never appear in closure.deps"
    )

    pub_node = _node(deps, f"{unique_repo}_pub")
    assert pub_node["effective_visibility"] == "public"
    assert pub_node["binaries"] == ["pub-bin"]

    sealed_node = _node(deps, f"{unique_repo}_sealed")
    assert sealed_node["effective_visibility"] == "sealed"
    assert sealed_node["binaries"] == ["sealed-bin"], "closure.deps lists the sealed dep's own claim"

    interface = data["closure"]["surface"]["interface"]
    binary_names = {(b["name"], b["package"]) for b in interface["binaries"]}
    assert ("pub-bin", pub_node["identifier"]) in binary_names
    assert not any(name == "sealed-bin" for name, _ in binary_names), (
        "sealed dep must be excluded from the interface surface (node admission gate)"
    )
    assert interface["binaries_complete"] is True
    assert data["closure"]["conflicts"] == {"entrypoints": [], "repositories": []}

    # The private surface's node admission gate is `has_private()` (true for
    # PUBLIC | PRIVATE). A public dep crosses both axes — admitted to
    # interface AND private. A sealed dep has neither axis set, so it is
    # excluded from private too, same as it is from interface.
    private = data["closure"]["surface"]["private"]
    private_binary_names = {(b["name"], b["package"]) for b in private["binaries"]}
    assert ("pub-bin", pub_node["identifier"]) in private_binary_names, (
        "a public dep crosses both axes — admitted to interface AND private"
    )
    assert not any(name == "sealed-bin" for name, _ in private_binary_names), (
        "sealed dep must be excluded from the private surface too (neither axis set)"
    )

    # The env summary lists the public env keys each admitted node exposes,
    # attributed to its package with the modifier kind. Sealed deps contribute
    # nothing to the interface surface — the same node-admission gate
    # binaries/entrypoints use.
    for entry in interface["env"]:
        assert entry["type"] in ("path", "constant"), entry
    assert "PATH" in _env_keys_for_repo(interface["env"], unique_repo), (
        "root's public PATH must be summarized on the interface surface"
    )
    assert "PATH" in _env_keys_for_repo(interface["env"], f"{unique_repo}_pub"), (
        "an interface-admitted dep's public env keys are summarized"
    )
    assert not _env_keys_for_repo(interface["env"], f"{unique_repo}_sealed"), (
        "a sealed dep's env vars must not reach the interface surface"
    )


def test_inspect_closure_offline_after_warm_matches_online(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Goals 4+5 acceptance: a warm ``--closure`` walk persists every closure
    blob to local CAS, so a subsequent ``--offline`` walk yields an identical
    ``closure`` object purely from cache.
    """
    dep = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path)
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
    )

    online = ocx.json("package", "inspect", "--closure", root.short)[root.short]

    offline_result = ocx.run("--offline", "package", "inspect", "--closure", root.short, format="json")
    assert offline_result.returncode == 0, offline_result.stderr
    import json as _json

    offline = _json.loads(offline_result.stdout)[root.short]

    assert offline["closure"] == online["closure"]


def test_inspect_closure_offline_unwarmed_dep_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A dependency never walked (config blob absent locally) fails closed
    under ``--offline`` even though the root's own metadata is cached.

    ``--resolve`` warms the root; a never-walked dep still fails closed.
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
        "--offline", "package", "inspect", "--closure", root.short,
        format=None, check=False,
    )

    assert result.returncode == 81, (
        f"expected 81 (PolicyBlocked), got {result.returncode}\nstderr: {result.stderr}"
    )


def test_inspect_closure_image_index_root_selects_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--closure`` on an image-index reference (no ``-p``) platform-selects
    and yields the Resolved body (``platform`` + ``resolution``) with the
    closure attached — never the metadata-less Candidates body.
    """
    dep = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path)
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
    )

    data = ocx.json("package", "inspect", "--closure", root.short)[root.short]

    assert "candidates" not in data
    assert "platform" in data
    assert "resolution" in data
    assert len(data["closure"]["deps"]) == 1, "one dependency — the root is excluded"


def test_inspect_closure_diamond_lists_shared_dep_once(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Diamond (app -> {left, right} -> leaf): the shared leaf appears exactly
    once in ``closure.deps`` (deduped), and the flat plain ``deps`` section
    carries no ``(*)`` marker — a flat list dedups by construction.
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

    data = ocx.json("package", "inspect", "--closure", app.short)[app.short]
    deps = data["closure"]["deps"]
    assert len(deps) == 3, deps  # leaf, left, right — app (root) excluded
    leaf_nodes = [n for n in deps if f"/{unique_repo}_leaf:" in n["identifier"]]
    assert len(leaf_nodes) == 1

    plain = ocx.plain("package", "inspect", "--closure", app.short)
    assert "(*)" not in plain.stdout, plain.stdout
    leaf_short_name = f"{unique_repo}_leaf"
    assert _tree_node_line_count(plain.stdout, leaf_short_name) == 1, (
        f"the shared leaf must render exactly once in the flat deps section:\n{plain.stdout}"
    )


def test_inspect_closure_undeclared_binaries_makes_aggregate_incomplete(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """An interface-admitted dep with NO ``binaries`` key (undeclared, not
    scanned) makes ``closure.surface.interface.binaries_complete`` False —
    "couldn't determine != determined zero".
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

    data = ocx.json("package", "inspect", "--closure", root.short)[root.short]

    dep_node = _node(data["closure"]["deps"], f"{unique_repo}_dep")
    assert "binaries" not in dep_node, "undeclared binaries must be an absent key, not []"
    assert data["closure"]["surface"]["interface"]["binaries_complete"] is False


def test_inspect_closure_declared_empty_binaries_keeps_aggregate_complete(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The tri-state's other arm: an interface-admitted dep with an explicit
    ``"binaries": []`` claim (publisher asserts ZERO binaries — distinct wire
    state from the absent key) renders the node key as ``[]`` and keeps
    ``binaries_complete`` True. Sibling of the undeclared case above.
    """
    dep = make_package(
        ocx, f"{unique_repo}_dep", "1.0.0", tmp_path,
        binaries=[],  # declared empty — asserted zero, not a gap
    )
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        binaries=[],
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
    )

    data = ocx.json("package", "inspect", "--closure", root.short)[root.short]

    dep_node = _node(data["closure"]["deps"], f"{unique_repo}_dep")
    assert dep_node["binaries"] == [], "declared-empty binaries must be a present [] key, not absent"
    assert data["closure"]["surface"]["interface"]["binaries_complete"] is True


def test_inspect_closure_entrypoints_admitted_to_surface(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A public dep's declared ``entrypoints`` show up as the dep's own
    entrypoint list AND get ``{name, package}`` attribution in
    ``closure.surface.interface.entrypoints``; entrypoints never affect
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

    data = ocx.json("package", "inspect", "--closure", root.short)[root.short]

    dep_node = _node(data["closure"]["deps"], f"{unique_repo}_ep")
    assert dep_node["entrypoints"] == ["ep-tool"]

    interface = data["closure"]["surface"]["interface"]
    assert {"name": "ep-tool", "package": dep_node["identifier"]} in interface["entrypoints"]
    assert interface["binaries_complete"] is True


def _attr_by_digest(items: list[dict]) -> list[tuple[str, str]]:
    """(name, digest) pairs for a surface's binaries/entrypoints attribution.

    Compares by digest rather than the full identifier string so a tag-bearing
    ``repo:tag@digest`` (as `env` reports) matches the same node however inspect
    spells it — the invariant is *which package*, not its string form.
    """

    def digest(ident: str | None) -> str:
        match = re.search(r"@(sha256:[0-9a-f]+)", ident or "")
        return match.group(1) if match else (ident or "")

    return sorted((item["name"], digest(item.get("package"))) for item in items)


def test_inspect_closure_surface_equals_env_and_env_self(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """`inspect --closure`'s two surfaces ARE the projections `ocx package env`
    (interface / consumer) and `ocx package env --self` (private / self)
    compose. They share ONE surface algebra (``composer::{dep_admitted,
    carrier_crosses}`` — env vars cross under their declared visibility,
    entry points under ``Entrypoints::IMPLICIT_VISIBILITY`` (interface),
    binaries claims under ``Binaries::IMPLICIT_VISIBILITY`` (public)), so the
    binaries/entrypoints attribution and env-key crossing must match exactly —
    inspect is metadata-only, but the resolution is the same code.

    Shape mirrors the ``deps-app`` manual fixture: ``app`` → ``mid`` (interface),
    ``app`` → ``leaf_a`` (public), ``app`` → ``leaf_b`` (private). ``leaf_b``
    additionally declares its OWN ``private`` env var — the case that would
    diverge if inspect re-derived the env rule (a dependency crosses only its
    interface side, on either surface).
    """
    pub_path = {"key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "public"}

    leaf_a = make_package(ocx, f"{unique_repo}_leafa", "1.0.0", tmp_path, bins=["leaf-a"], env=[pub_path])
    leaf_b = make_package(
        ocx, f"{unique_repo}_leafb", "1.0.0", tmp_path,
        bins=["leaf-b"],
        env=[
            pub_path,
            {"key": "LEAF_B_SECRET", "type": "constant", "value": "${installPath}/secret", "visibility": "private"},
        ],
    )
    mid = make_package(
        ocx, f"{unique_repo}_mid", "1.0.0", tmp_path,
        bins=["mid"], env=[pub_path],
        dependencies=[_dep_entry(ocx, leaf_a, visibility="interface")],
    )
    app = make_package_with_entrypoints(
        ocx, f"{unique_repo}_app", tmp_path,
        entrypoints=["app"], bins=["app"], tag="1.0.0",
        env=[pub_path, {"key": "APP_HOME", "type": "constant", "value": "${installPath}", "visibility": "public"}],
        dependencies=[
            _dep_entry(ocx, mid, visibility="interface"),
            _dep_entry(ocx, leaf_a, visibility="public"),
            _dep_entry(ocx, leaf_b, visibility="private"),
        ],
    )

    # `env` composes from installed packages; install pulls the whole closure.
    ocx.json("package", "install", "--select", app.short)

    surface = ocx.json("package", "inspect", "--closure", app.short)[app.short]["closure"]["surface"]
    env_consumer = ocx.json("package", "env", app.short)
    env_self = ocx.json("package", "env", "--self", app.short)

    # The invariant: each inspect surface equals the env the composer produces.
    assert _attr_by_digest(surface["interface"]["binaries"]) == _attr_by_digest(env_consumer["binaries"])
    assert _attr_by_digest(surface["interface"]["entrypoints"]) == _attr_by_digest(env_consumer["entrypoints"])
    assert _attr_by_digest(surface["private"]["binaries"]) == _attr_by_digest(env_self["binaries"])
    assert _attr_by_digest(surface["private"]["entrypoints"]) == _attr_by_digest(env_self["entrypoints"])

    # The root's own entry points are interface-only: `--self` bypasses the
    # package's own launchers and calls `bin/` directly, so the composer does
    # not put the root's `entrypoints/` on PATH there — and must not claim it.
    # The root's `bin/` DOES reach the self surface (its PATH var is `public`).
    priv_bins = {name for name, _ in _attr_by_digest(surface["private"]["binaries"])}
    priv_eps = {name for name, _ in _attr_by_digest(surface["private"]["entrypoints"])}
    iface_eps = {name for name, _ in _attr_by_digest(surface["interface"]["entrypoints"])}
    assert "app" in priv_bins, priv_bins
    assert "app" not in priv_eps, priv_eps
    assert "app" in iface_eps, iface_eps

    # Regression: `leaf_b`'s OWN private var crosses no edge, so it is absent
    # from the parent's private surface AND from `ocx env --self`.
    insp_private_keys = {entry["key"] for entry in surface["private"]["env"]}
    env_self_keys = {entry["key"] for entry in env_self["entries"]}
    assert "LEAF_B_SECRET" not in insp_private_keys, insp_private_keys
    assert "LEAF_B_SECRET" not in env_self_keys, env_self_keys
