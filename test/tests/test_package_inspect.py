# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package inspect``.

Read-only, ref-shape adaptive:
  * default + image-index ref → platform ``candidates`` (no metadata).
  * default + single-manifest / ``@digest`` ref → ``metadata`` + ``layers``
    (no chain).
  * ``--resolve`` → platform-selected ``metadata`` + ``layers`` +
    ``resolution`` chain (layers live at the top level, not inside the chain).

``-p/--platform`` applies only with ``--resolve``. Honors global
``--offline`` / ``--format``.

Exit codes per quality-rust-exit_codes.md:
  0  = Success
  79 = NotFound (unknown tag)
  81 = PolicyBlocked (offline + config blob absent locally)
"""
from __future__ import annotations

import json
import shutil
import urllib.request
from pathlib import Path

from src import OcxRunner
from src.helpers import make_package
from src.runner import registry_dir


def test_inspect_default_lists_index_candidates(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Default inspect of an image-index tag lists platform candidates only."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    data = ocx.json("package", "inspect", pkg.short)

    assert "metadata" not in data, "candidate listing must not load metadata"
    assert "resolution" not in data
    assert "layers" not in data, "an index candidate listing selects no manifest"
    candidates = data["candidates"]
    assert len(candidates) >= 1
    c = candidates[0]
    assert c["digest"].startswith("sha256:")
    assert c["platform"]
    assert c["media_type"]
    assert isinstance(c["size"], int)


def test_inspect_digest_manifest_shows_metadata(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A ``<repo>@<digest>`` ref pointing at a child manifest shows metadata."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    child = ocx.json("package", "inspect", pkg.short)["candidates"][0]["digest"]

    data = ocx.json("package", "inspect", f"{unique_repo}@{child}")

    assert "candidates" not in data
    assert "resolution" not in data
    assert data["pinned_digest"] == child
    assert data["metadata"]["version"] == 1
    env_keys = {v["key"] for v in data["metadata"]["env"]}
    assert "PATH" in env_keys, f"expected PATH env var, got {env_keys}"
    # Default-mode manifest inspect surfaces the manifest's layers directly,
    # no --resolve required.
    assert len(data["layers"]) >= 1, "default manifest inspect must show layers"
    layer = data["layers"][0]
    assert layer["digest"].startswith("sha256:")
    assert layer["media_type"]
    assert isinstance(layer["size"], int)


def test_inspect_resolve_adds_metadata_and_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--resolve`` platform-selects and adds the resolution chain."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    data = ocx.json("package", "inspect", "--resolve", pkg.short)

    assert data["metadata"]["version"] == 1
    resolution = data["resolution"]
    assert resolution["pinned"]
    assert "layers" not in resolution, "layers moved out of the resolution chain"
    chain = resolution["chain"]
    assert len(chain) >= 2, chain
    # Every chain entry is a descriptor object, not a bare digest string:
    # digest + role + media_type + raw integer size (machine surface keeps
    # the integer; the plain tree humanises it).
    for entry in chain:
        assert entry["digest"].startswith("sha256:"), entry
        assert entry["role"] in {"index", "manifest", "config"}, entry
        assert entry["media_type"], entry
        assert isinstance(entry["size"], int), entry
    roles = [e["role"] for e in chain]
    assert roles[-1] == "config", roles
    assert "manifest" in roles, roles
    # Layers render alongside metadata (same surface as default mode), not
    # inside the resolution chain.
    assert len(data["layers"]) >= 1
    layer = data["layers"][0]
    assert layer["digest"].startswith("sha256:")
    assert layer["media_type"]
    assert isinstance(layer["size"], int)


def test_inspect_resolve_platform_selects_child(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Multi-platform tag: default lists both; ``--resolve -p`` picks one."""
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "amd64",
        platform="linux/amd64", new=True, cascade=False,
    )
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "arm64",
        platform="linux/arm64", new=False, cascade=False,
    )
    short = f"{unique_repo}:1.0.0"

    listed = {c["platform"] for c in ocx.json("package", "inspect", short)["candidates"]}
    assert {"linux/amd64", "linux/arm64"} <= listed, listed

    amd = ocx.json("package", "inspect", "--resolve", "-p", "linux/amd64", short)
    arm = ocx.json("package", "inspect", "--resolve", "-p", "linux/arm64", short)
    assert amd["pinned_digest"] != arm["pinned_digest"]


def test_inspect_unknown_tag_exits_79(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """An unknown tag resolves to NotFound (exit 79)."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path)

    result = ocx.run(
        "package", "inspect", f"{unique_repo}:9.9.9", format=None, check=False
    )

    assert result.returncode == 79, (
        f"expected 79, got {result.returncode}\nstderr: {result.stderr}"
    )


def test_inspect_resolve_offline_missing_config_blob_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--resolve --offline`` with the config blob absent is PolicyBlocked (81).

    ``make_package`` runs ``index update`` which persists the manifest chain
    but not the OCX config blob, so an offline resolve selects the platform
    manifest locally yet cannot load metadata.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    result = ocx.run(
        "--offline", "package", "inspect", "--resolve", pkg.short,
        format=None, check=False,
    )

    assert result.returncode == 81, (
        f"expected 81, got {result.returncode}\nstderr: {result.stderr}"
    )


# ---------------------------------------------------------------------------
# Specification tests for documented gaps in the `package inspect` design
# record (`subsystem-cli-commands.md` "package inspect" gotcha checklist).
# Each test traces to one gap (F1..F5). They are written before the
# `resolve_top_manifest` implementation lands and currently fail (the stub
# panics with `unimplemented!()` in every default-mode path, F2/F3/F5, and
# F1 covers a genuinely uncovered ref shape).
# ---------------------------------------------------------------------------


def _registry_get(url: str, accept: str) -> tuple[bytes, str]:
    """GET a manifest, returning ``(body, content_type)``."""
    req = urllib.request.Request(url, headers={"Accept": accept})
    with urllib.request.urlopen(req, timeout=5) as resp:
        return resp.read(), resp.headers.get("Content-Type", "")


def _registry_put(url: str, body: bytes, content_type: str) -> None:
    """PUT a manifest body under ``url`` with the given media type."""
    req = urllib.request.Request(
        url, data=body, method="PUT",
        headers={"Content-Type": content_type},
    )
    with urllib.request.urlopen(req, timeout=5):
        return


def test_inspect_default_platform_flag_ignored_without_resolve(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """F2 (Block gap): ``-p`` is ignored in default mode.

    A multi-platform tag (image index, two platforms) inspected without
    ``--resolve`` must return the FULL candidate set regardless of ``-p`` —
    byte-identical to the same invocation without ``-p``. Proves the
    platform flag does not narrow the candidate listing in default mode.
    """
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "amd64",
        platform="linux/amd64", new=True, cascade=False,
    )
    make_package(
        ocx, unique_repo, "1.0.0", tmp_path / "arm64",
        platform="linux/arm64", new=False, cascade=False,
    )
    short = f"{unique_repo}:1.0.0"

    without_flag = ocx.run("package", "inspect", short).stdout
    with_flag = ocx.run(
        "package", "inspect", "-p", "linux/amd64", short
    ).stdout

    assert with_flag == without_flag, (
        "-p must be ignored in default mode (byte-identical output expected)\n"
        f"without -p: {without_flag!r}\nwith -p: {with_flag!r}"
    )
    listed = {c["platform"] for c in json.loads(with_flag)["candidates"]}
    assert {"linux/amd64", "linux/arm64"} <= listed, (
        f"default mode must list ALL platforms even with -p, got {listed}"
    )


def test_inspect_default_flat_tag_shows_metadata_no_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """F1 (Warn gap): a flat (non-index) image-manifest *tag*.

    The existing suite only covers the flat shape via ``@digest``. This
    covers the flat-*tag* entry shape: a tag pointing directly at a single
    image manifest (no image index) must yield the ``metadata`` shape with
    NO ``candidates`` and NO ``resolution`` keys.

    ``ocx package push`` always wraps in an image index, so the flat-tag
    manifest is materialized by retagging a child manifest digest directly
    on the registry, then re-indexing that tag.
    """
    make_package(ocx, unique_repo, "1.0.0", tmp_path, new=True, cascade=False)
    child = ocx.json(
        "package", "inspect", f"{unique_repo}:1.0.0"
    )["candidates"][0]["digest"]

    # Retag the child image manifest under a new tag so the tag points
    # directly at a bare image manifest (not an image index).
    img_mt = "application/vnd.oci.image.manifest.v1+json"
    base = f"http://{ocx.registry}/v2/{unique_repo}/manifests"
    body, _ = _registry_get(f"{base}/{child}", img_mt)
    _registry_put(f"{base}/flat", body, img_mt)
    ocx.plain("index", "update", f"{unique_repo}:flat")

    data = ocx.json("package", "inspect", f"{unique_repo}:flat")

    assert "candidates" not in data, "flat-tag manifest must not list candidates"
    assert "resolution" not in data, "default mode must not add a resolution chain"
    assert data["metadata"]["version"] == 1
    assert len(data["layers"]) >= 1, "flat-tag manifest inspect must show layers"
    env_keys = {v["key"] for v in data["metadata"]["env"]}
    assert "PATH" in env_keys, f"expected PATH env var, got {env_keys}"


def test_inspect_default_offline_missing_manifest_blob_exits_81(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """F3 (Warn gap): default-mode offline with the manifest blob absent.

    Distinct from ``test_inspect_resolve_offline_missing_config_blob_exits_81``
    (which is ``--resolve`` + config-blob miss). Here the tag→digest pointer
    is pinned locally (``index update`` persists it) but the manifest blob
    itself is removed before going offline, so a *default-mode* inspect
    cannot fetch the manifest and must exit 81 (PolicyBlocked).
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    # `index update` persisted the tag→digest pointer under tags/ and the
    # manifest chain under blobs/. Drop the blob CAS so the digest stays
    # pinned locally but the manifest blob is gone — default-mode offline
    # inspect must report PolicyBlocked rather than NotFound.
    # Note: the pin step does not persist the manifest blob under blobs/ —
    # the digest stays pinned via tags/ while the manifest blob is absent,
    # which is exactly the state this test needs. Remove blobs/ defensively
    # if a future change starts persisting it.
    blobs_root = Path(ocx.env["OCX_HOME"]) / "blobs"
    if blobs_root.exists():
        shutil.rmtree(blobs_root)
    tags_root = Path(ocx.env["OCX_HOME"]) / "tags"
    assert tags_root.exists(), "tag store must remain so the digest stays pinned"

    result = ocx.run(
        "--offline", "package", "inspect", pkg.short,
        format=None, check=False,
    )

    assert result.returncode == 81, (
        f"expected 81 (PolicyBlocked), got {result.returncode}\n"
        f"stderr: {result.stderr}"
    )


def test_inspect_default_has_no_install_side_effects(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """F5 (Block gap, reframed): inspect performs no install/symlink mutation.

    After a default-mode inspect, no install or symlink side effects may
    exist for the inspected repo: no candidate symlink and no assembled
    package directory. The local index / blob CAS MAY be populated on a
    cache miss (default-mode inspect uses ``IndexOperation::Resolve`` and is
    permitted to warm the index) — only install/symlink mutation is forbidden.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    ocx.json("package", "inspect", pkg.short)

    home = Path(ocx.env["OCX_HOME"])
    reg = registry_dir(ocx.registry)

    repo_symlink_root = home / "symlinks" / reg / unique_repo
    assert not repo_symlink_root.exists(), (
        f"inspect must not create symlinks for the repo: {repo_symlink_root}"
    )

    candidates_root = home / "symlinks" / reg / unique_repo / "candidates"
    assert not candidates_root.exists(), (
        f"inspect must not create a candidate symlink: {candidates_root}"
    )

    packages_root = home / "packages"
    if packages_root.exists():
        for entry in packages_root.rglob("metadata.json"):
            assert unique_repo not in entry.read_text(), (
                f"inspect must not assemble a package for {unique_repo}: {entry}"
            )

