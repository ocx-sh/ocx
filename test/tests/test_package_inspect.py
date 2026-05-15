# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for ``ocx package inspect``.

Read-only, ref-shape adaptive:
  * default + image-index ref → platform ``candidates`` (no metadata).
  * default + single-manifest / ``@digest`` ref → ``metadata`` (no chain).
  * ``--resolve`` → platform-selected ``metadata`` + ``resolution`` chain.

``-p/--platform`` applies only with ``--resolve``. Honors global
``--offline`` / ``--format``.

Exit codes per quality-rust-exit_codes.md:
  0  = Success
  79 = NotFound (unknown tag)
  81 = OfflineBlocked (offline + config blob absent locally)
"""
from __future__ import annotations

from pathlib import Path

from src import OcxRunner
from src.helpers import make_package


def test_inspect_default_lists_index_candidates(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Default inspect of an image-index tag lists platform candidates only."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    data = ocx.json("package", "inspect", pkg.short)

    assert "metadata" not in data, "candidate listing must not load metadata"
    assert "resolution" not in data
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


def test_inspect_resolve_adds_metadata_and_chain(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """``--resolve`` platform-selects and adds the resolution chain."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)

    data = ocx.json("package", "inspect", "--resolve", pkg.short)

    assert data["metadata"]["version"] == 1
    resolution = data["resolution"]
    assert resolution["pinned"]
    assert len(resolution["chain"]) >= 2, resolution["chain"]
    assert len(resolution["layers"]) >= 1
    layer = resolution["layers"][0]
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
    """``--resolve --offline`` with the config blob absent is OfflineBlocked (81).

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
