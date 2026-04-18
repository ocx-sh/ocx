"""Three-layer push/pull round-trip acceptance test.

Ensures multi-layer support scales past the two-layer fixtures in
`test_multi_layer.py`: three distinct archives, pushed in one command,
pulled by `ocx install`, then assembled into a package whose `content/`
is the disjoint union of all three layers.
"""

from __future__ import annotations

import json
import stat
from pathlib import Path

from src import OcxRunner, assert_dir_exists, current_platform


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
        "type": "bundle",
        "version": 1,
        "env": [
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
        "type": "bundle",
        "version": 1,
        "env": [
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        ],
    }))

    short = f"{unique_repo}:1.0.0"
    fq = f"{ocx.registry}/{short}"
    plat = current_platform()
    ocx.plain(
        "package", "push", "-p", plat, "-m", str(meta), "-n", fq,
        str(bundle_a), str(bundle_b), str(bundle_c),
    )
    ocx.plain("index", "update", short)

    result = ocx.json("install", short)
    content = Path(result[short]["path"])

    assert_dir_exists(content)
    assert (content / "lib" / "liba.so").exists(), "Layer A file missing"
    assert (content / "lib" / "liba.so").read_text() == "liba-content"
    assert (content / "bin" / "tool").exists(), "Layer B file missing"
    assert (content / "share" / "doc" / "NOTES.md").exists(), "Layer C file missing"
    assert (content / "share" / "doc" / "NOTES.md").read_text() == "# Notes\n"
