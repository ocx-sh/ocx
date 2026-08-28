# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`ocx package push --annotation` writes OCI annotations on the image index.

`org.opencontainers.image.source` is the only mechanism by which a registry
links a published package back to its source repository, so these tests pin
the wire effect end-to-end against a live registry: the annotation reaches
the index of the primary tag and of every cascade tag, and a push without the
flag leaves the index annotation-free.
"""
from __future__ import annotations

from pathlib import Path

from src.helpers import make_package
from src.registry import fetch_manifest_from_registry
from src.runner import OcxRunner

SOURCE = "org.opencontainers.image.source"
REVISION = "org.opencontainers.image.revision"
SOURCE_URL = "https://github.com/ocx-sh/ocx"


def index_annotations(ocx: OcxRunner, repo: str, tag: str) -> dict[str, str]:
    manifest = fetch_manifest_from_registry(ocx.registry, repo, tag)
    return manifest.get("annotations") or {}


def test_annotation_lands_on_the_primary_tag_and_every_cascade_tag(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    make_package(
        ocx,
        unique_repo,
        "1.2.3",
        tmp_path,
        cascade=True,
        extra_push_args=["--annotation", f"{SOURCE}={SOURCE_URL}", "--annotation", f"{REVISION}=deadbeef"],
    )

    # A cascading push must not leave a rolling alias with weaker provenance
    # than the version tag it points at.
    for tag in ("1.2.3", "1.2", "1", "latest"):
        annotations = index_annotations(ocx, unique_repo, tag)
        assert annotations.get(SOURCE) == SOURCE_URL, f"missing source annotation on {tag}: {annotations}"
        assert annotations.get(REVISION) == "deadbeef", f"missing revision annotation on {tag}: {annotations}"


def test_push_without_the_flag_writes_no_index_annotations(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The absent-value case: no flag, no annotations, no behaviour change."""
    make_package(ocx, unique_repo, "1.0.0", tmp_path, cascade=False)

    assert index_annotations(ocx, unique_repo, "1.0.0") == {}


def test_a_later_plain_push_does_not_unlink_the_source_repository(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """Merging into an existing index preserves annotations it already holds,
    so a pipeline that forgets the flag on one release does not silently
    unlink the package from its repository."""
    make_package(
        ocx,
        unique_repo,
        "1.0.0",
        tmp_path,
        cascade=True,
        extra_push_args=["--annotation", f"{SOURCE}={SOURCE_URL}"],
    )
    make_package(ocx, unique_repo, "1.0.1", tmp_path, cascade=True)

    # `latest` was re-pointed by the second, annotation-less push.
    assert index_annotations(ocx, unique_repo, "latest").get(SOURCE) == SOURCE_URL
