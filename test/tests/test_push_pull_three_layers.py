"""Three-layer push/pull round-trip acceptance test.

Ensures multi-layer support scales past the two-layer fixtures in
`test_multi_layer.py`: three distinct archives, pushed in one command,
pulled by `ocx install`, then assembled into a package whose `content/`
is the disjoint union of all three layers.
"""

from __future__ import annotations

import json
import stat
import urllib.request
from pathlib import Path

from src import OcxRunner, assert_dir_exists, current_platform

_INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"
_MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"


def _fetch_manifest(registry: str, repo: str, ref: str) -> dict:
    url = f"http://{registry}/v2/{repo}/manifests/{ref}"
    accept = f"{_INDEX_MEDIA_TYPE}, {_MANIFEST_MEDIA_TYPE}"
    req = urllib.request.Request(url, headers={"Accept": accept})
    resp = urllib.request.urlopen(req, timeout=5)
    return json.loads(resp.read())


def _make_layer_content(tmp_path: Path, name: str, files: dict[str, str]) -> Path:
    layer_dir = tmp_path / f"layer-{name}"
    for rel_path, content in files.items():
        p = layer_dir / rel_path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
        if rel_path.startswith("bin/"):
            p.chmod(p.stat().st_mode | stat.S_IEXEC)
    return layer_dir


def _bundle_layer(ocx: OcxRunner, layer_dir: Path, tmp_path: Path, *, ext: str = "tar.gz") -> Path:
    bundle = tmp_path / f"{layer_dir.name}.{ext}"
    metadata_path = tmp_path / f"{layer_dir.name}-meta.json"
    metadata_path.write_text(json.dumps({
        "type": "bundle", "version": 1, "env": [
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        ],
    }))
    ocx.plain("package", "create", "-m", str(metadata_path), "-o", str(bundle), str(layer_dir))
    return bundle


def test_push_pull_three_layers(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Push three distinct archive layers, install, verify content/ is the union.

    Layers are intentionally non-overlapping — the assemble walker
    rejects overlap, so a valid round-trip needs disjoint paths. Each
    layer contributes one file under a different top-level directory:
    `lib/`, `bin/`, `share/`.
    """
    layer_a = _make_layer_content(tmp_path, "a", {"lib/liba.so": "liba-content"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho hello\n"})
    layer_c = _make_layer_content(tmp_path, "c", {"share/doc/NOTES.md": "# Notes\n"})

    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)
    bundle_c = _bundle_layer(ocx, layer_c, tmp_path)

    meta = tmp_path / "meta.json"
    meta.write_text(json.dumps({
        "type": "bundle", "version": 1, "platform": current_platform(), "env": [
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        ],
    }))

    short = f"{unique_repo}:1.0.0"
    fq = f"{ocx.registry}/{short}"
    plat = current_platform()
    ocx.plain(
        "package", "push", "-p", plat, "-m", str(meta), "-n", "-i", fq,
        str(bundle_a), str(bundle_b), str(bundle_c),
    )
    ocx.plain("index", "update", short)

    result = ocx.json("package", "install", short)
    content = Path(result[short]["path"]) / "content"

    assert_dir_exists(content)
    assert (content / "lib" / "liba.so").exists(), "Layer A file missing"
    assert (content / "lib" / "liba.so").read_text() == "liba-content"
    assert (content / "bin" / "tool").exists(), "Layer B file missing"
    assert (content / "share" / "doc" / "NOTES.md").exists(), "Layer C file missing"
    assert (content / "share" / "doc" / "NOTES.md").read_text() == "# Notes\n"


def test_default_push_emits_no_layer_annotations(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A5 (BC2, acceptance): a default push (no per-layer layout) produces a
    manifest whose every layer descriptor carries no annotations.

    This is the acceptance-level companion to the manifest_builder.rs
    byte+digest golden — it proves the wire artifact, not just the builder,
    stays annotation-free on the default path. GREEN today; guards BC2.
    """
    layer_a = _make_layer_content(tmp_path, "a", {"lib/liba.so": "liba"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho hi\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    meta = tmp_path / "meta.json"
    meta.write_text(json.dumps({
        "type": "bundle", "version": 1, "platform": current_platform(), "env": [
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        ],
    }))
    ocx.plain(
        "package", "push", "-p", current_platform(), "-m", str(meta), "-n",
        "-i", f"{ocx.registry}/{unique_repo}:1.0.0", str(bundle_a), str(bundle_b),
    )

    index = _fetch_manifest(ocx.registry, unique_repo, "1.0.0")
    image = _fetch_manifest(ocx.registry, unique_repo, index["manifests"][0]["digest"])
    for descriptor in image["layers"]:
        assert not descriptor.get("annotations"), (
            f"default push must emit no layer annotations, got {descriptor.get('annotations')!r}"
        )


def test_cascade_layout_annotations_identical_across_tags(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A7 (W6): cascade-push a layout-bearing package (3.28.1 -> 3.28, 3);
    every resulting tag's manifest carries byte-identical per-layer
    `sh.ocx.layer.*` annotations.

    The cascade builds the layer-bearing manifest once and only merges
    platform pointers into each tag's outer index, so the per-layer
    annotations must be identical across every tag. Fails until the layer-ref
    layout grammar + push wiring land (the layout arg would otherwise be
    treated as a nonexistent file path).
    """
    layer_a = _make_layer_content(tmp_path, "a", {"topdir/lib/liba.so": "liba"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho hi\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    meta = tmp_path / "meta.json"
    meta.write_text(json.dumps({
        "type": "bundle", "version": 1, "platform": current_platform(), "env": [
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        ],
    }))
    ocx.plain(
        "package", "push", "-p", current_platform(), "-m", str(meta), "-n", "--cascade",
        "-i", f"{ocx.registry}/{unique_repo}:3.28.1",
        f"{bundle_a}:strip=1,prefix=share",
        str(bundle_b),
    )
    ocx.plain("index", "update", unique_repo)
    tags = ocx.json("index", "list", unique_repo)[unique_repo]
    for expected in ("3.28.1", "3.28", "3"):
        assert expected in tags, f"cascade must produce tag {expected}, got {tags}"

    layout_annotations = []
    for tag in ("3.28.1", "3.28", "3"):
        index = _fetch_manifest(ocx.registry, unique_repo, tag)
        image = _fetch_manifest(ocx.registry, unique_repo, index["manifests"][0]["digest"])
        annotated = [d["annotations"] for d in image["layers"] if d.get("annotations")]
        assert annotated, f"tag {tag} lost the per-layer layout annotations"
        layer_annotations = annotated[0]
        assert "sh.ocx.layer.strip-components" in layer_annotations, f"tag {tag} missing strip annotation"
        assert "sh.ocx.layer.prefix" in layer_annotations, f"tag {tag} missing prefix annotation"
        layout_annotations.append(layer_annotations)

    first = layout_annotations[0]
    for other in layout_annotations[1:]:
        assert other == first, "per-layer annotations must be identical across cascade tags (W6)"
