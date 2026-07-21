# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""A read-only ``ocx package inspect`` must never grow the permanent local index.

Contract (owner, 2026-07-20): ``OCX_INDEX`` is permanent (outside ``ocx clean``
GC); the blob store is the GC-able content cache. Only ``ocx index update`` /
pins may populate the index. A read-only ``inspect`` therefore resolves
content-addressed (index -> blobs -> source) and warms the blob cache, but
writes NOTHING into the index — no dispatch object (``o/<algo>/<hex>.json``),
no root document (``p/<ns>/<pkg>.json``).

The package is published+indexed into the fixture home, then inspected from a
*separate, empty* home so the index write under test is not masked by
``make_package``'s own ``index update``.
"""
from __future__ import annotations

from pathlib import Path

from src import OcxRunner
from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest
from src.runner import PackageInfo


def _dep_entry(ocx: OcxRunner, pkg: PackageInfo, *, visibility: str) -> dict:
    digest = fetch_platform_manifest_digest(ocx.registry, pkg.repo, pkg.tag)
    return {"identifier": f"{pkg.fq}@{digest}", "visibility": visibility}


def _index_objects(home: Path) -> list[str]:
    """Root documents + dispatch objects the index home holds, home-relative."""
    index_dir = home / "index"
    if not index_dir.exists():
        return []
    return sorted(str(p.relative_to(index_dir)) for p in index_dir.rglob("*.json"))


def _blob_objects(home: Path) -> list[Path]:
    blobs_dir = home / "blobs"
    if not blobs_dir.exists():
        return []
    return list(blobs_dir.rglob("data"))


def test_inspect_does_not_grow_local_index(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """Neither plain ``inspect`` nor ``inspect --closure`` writes the index; both
    warm the blob cache instead."""
    dep = make_package(ocx, f"{unique_repo}_dep", "1.0.0", tmp_path)
    root = make_package(
        ocx, unique_repo, "1.0.0", tmp_path,
        dependencies=[_dep_entry(ocx, dep, visibility="public")],
    )

    # A pristine home whose index starts empty — the write under test is not
    # masked by `make_package`'s own `index update` (which targeted `ocx`'s home).
    fresh_home = tmp_path / "reader_home"
    overrides = {"OCX_HOME": str(fresh_home)}

    ocx.run("package", "inspect", root.short, format="json", env_overrides=overrides)
    assert _index_objects(fresh_home) == [], (
        "plain read-only inspect must not write any object into the permanent index"
    )

    ocx.run("package", "inspect", "--closure", root.short, format="json", env_overrides=overrides)
    assert _index_objects(fresh_home) == [], (
        "inspect --closure must not write any object into the permanent index"
    )

    # Content still resolved — it went to the GC-able blob cache, not the index.
    assert _blob_objects(fresh_home), "inspect must warm the blob cache with the fetched content"
