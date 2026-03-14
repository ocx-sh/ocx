"""Tests for `ocx package describe` — push description data to __ocx.desc tag."""

from __future__ import annotations

from pathlib import Path
from uuid import uuid4

import pytest
import requests

from src import OcxRunner, fetch_manifest_from_registry
from src.registry import make_client


@pytest.fixture()
def unique_repo(request: pytest.FixtureRequest) -> str:
    short_id = uuid4().hex[:8]
    name = request.node.name.lower()[:40]
    return f"t_{short_id}_{name}"


def test_describe_readme_only(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Push a README-only description and verify the tag exists."""
    readme = tmp_path / "README.md"
    readme.write_text("# My Tool\n\nA great tool for great things.\n")

    fq = f"{ocx.registry}/{unique_repo}"
    ocx.plain("package", "describe", "--readme", str(readme), fq)

    # Verify __ocx.desc tag exists via registry API.
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "__ocx.desc")
    assert manifest["mediaType"] == "application/vnd.oci.image.manifest.v1+json"
    assert manifest.get("artifactType") == "application/vnd.sh.ocx.description.v1"

    # Should have exactly 1 layer (README).
    layers = manifest["layers"]
    assert len(layers) == 1
    assert layers[0]["mediaType"] == "application/markdown"


def test_describe_readme_and_logo(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Push a README + PNG logo and verify both layers exist."""
    readme = tmp_path / "README.md"
    readme.write_text("# Tool with Logo\n")

    logo = tmp_path / "logo.png"
    # Minimal valid PNG (1x1 transparent pixel).
    logo.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06"
        b"\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\nIDATx"
        b"\x9cc\x00\x01\x00\x00\x05\x00\x01\r\n\xb4\x00\x00\x00\x00IEND\xaeB`\x82"
    )

    fq = f"{ocx.registry}/{unique_repo}"
    ocx.plain("package", "describe", "--readme", str(readme), "--logo", str(logo), fq)

    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "__ocx.desc")
    layers = manifest["layers"]
    assert len(layers) == 2

    media_types = {layer["mediaType"] for layer in layers}
    assert "application/markdown" in media_types
    assert "image/png" in media_types


def test_describe_idempotent_overwrite(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Pushing a description twice should succeed (overwrite)."""
    readme = tmp_path / "README.md"

    readme.write_text("# Version 1\n")
    fq = f"{ocx.registry}/{unique_repo}"
    ocx.plain("package", "describe", "--readme", str(readme), fq)

    readme.write_text("# Version 2\n")
    ocx.plain("package", "describe", "--readme", str(readme), fq)

    # Should still have a valid manifest.
    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "__ocx.desc")
    assert manifest.get("artifactType") == "application/vnd.sh.ocx.description.v1"


def test_describe_svg_logo(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Push a README + SVG logo and verify media type."""
    readme = tmp_path / "README.md"
    readme.write_text("# SVG Logo Tool\n")

    logo = tmp_path / "logo.svg"
    logo.write_text('<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>')

    fq = f"{ocx.registry}/{unique_repo}"
    ocx.plain("package", "describe", "--readme", str(readme), "--logo", str(logo), fq)

    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "__ocx.desc")
    layers = manifest["layers"]
    media_types = {layer["mediaType"] for layer in layers}
    assert "image/svg+xml" in media_types


def test_describe_with_annotations(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Push a description with title, description, and keywords annotations."""
    readme = tmp_path / "README.md"
    readme.write_text("# Annotated Tool\n")

    fq = f"{ocx.registry}/{unique_repo}"
    ocx.plain(
        "package", "describe",
        "--readme", str(readme),
        "--title", "My Tool",
        "--description", "A fantastic build tool",
        "--keywords", "build,c++,cmake",
        fq,
    )

    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "__ocx.desc")
    annotations = manifest.get("annotations", {})
    assert annotations["org.opencontainers.image.title"] == "My Tool"
    assert annotations["org.opencontainers.image.description"] == "A fantastic build tool"
    assert annotations["sh.ocx.keywords"] == "build,c++,cmake"


def test_describe_merge_preserves_existing(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Updating only annotations preserves the existing README and logo."""
    readme = tmp_path / "README.md"
    readme.write_text("# Original README\n")

    logo = tmp_path / "logo.svg"
    logo.write_text('<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>')

    fq = f"{ocx.registry}/{unique_repo}"

    # First push: README + logo + title.
    ocx.plain(
        "package", "describe",
        "--readme", str(readme),
        "--logo", str(logo),
        "--title", "Original Title",
        fq,
    )

    # Second push: only update keywords — README, logo, and title should be preserved.
    ocx.plain("package", "describe", "--keywords", "new,keywords", fq)

    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "__ocx.desc")

    # Layers preserved (README + logo).
    layers = manifest["layers"]
    assert len(layers) == 2
    media_types = {layer["mediaType"] for layer in layers}
    assert "application/markdown" in media_types
    assert "image/svg+xml" in media_types

    # Both old and new annotations present.
    annotations = manifest.get("annotations", {})
    assert annotations["org.opencontainers.image.title"] == "Original Title"
    assert annotations["sh.ocx.keywords"] == "new,keywords"


def _fetch_blob(registry: str, repo: str, digest: str) -> bytes:
    """Fetch a raw blob from the registry by digest."""
    resp = requests.get(f"http://{registry}/v2/{repo}/blobs/{digest}")
    resp.raise_for_status()
    return resp.content


def test_describe_frontmatter_extraction(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Frontmatter is extracted into annotations and stripped from the pushed README."""
    readme = tmp_path / "README.md"
    readme.write_text(
        "---\n"
        "title: My Tool\n"
        "description: A fantastic build tool\n"
        "keywords: build,cpp,cmake\n"
        "---\n"
        "# My Tool\n\n"
        "Some content.\n"
    )

    fq = f"{ocx.registry}/{unique_repo}"
    ocx.plain("package", "describe", "--readme", str(readme), fq)

    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "__ocx.desc")

    # Annotations should contain the frontmatter values.
    annotations = manifest.get("annotations", {})
    assert annotations["org.opencontainers.image.title"] == "My Tool"
    assert annotations["org.opencontainers.image.description"] == "A fantastic build tool"
    assert annotations["sh.ocx.keywords"] == "build,cpp,cmake"

    # The pushed README body should NOT contain frontmatter.
    readme_layer = next(l for l in manifest["layers"] if l["mediaType"] == "application/markdown")
    blob = _fetch_blob(ocx.registry, unique_repo, readme_layer["digest"])
    body = blob.decode("utf-8")
    assert not body.startswith("---"), f"README body should not contain frontmatter, got: {body[:100]}"
    assert body.startswith("# My Tool")


def test_describe_cli_flags_override_frontmatter(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """CLI flags take precedence over frontmatter values."""
    readme = tmp_path / "README.md"
    readme.write_text(
        "---\n"
        "title: FromFrontmatter\n"
        "description: Frontmatter description\n"
        "keywords: frontmatter,keywords\n"
        "---\n"
        "# Content\n"
    )

    fq = f"{ocx.registry}/{unique_repo}"
    ocx.plain(
        "package", "describe",
        "--readme", str(readme),
        "--title", "FromFlag",
        "--keywords", "flag,keywords",
        fq,
    )

    manifest = fetch_manifest_from_registry(ocx.registry, unique_repo, "__ocx.desc")
    annotations = manifest.get("annotations", {})

    # CLI flags win over frontmatter.
    assert annotations["org.opencontainers.image.title"] == "FromFlag"
    assert annotations["sh.ocx.keywords"] == "flag,keywords"

    # Description was only in frontmatter (no CLI flag), so frontmatter value is used.
    assert annotations["org.opencontainers.image.description"] == "Frontmatter description"
