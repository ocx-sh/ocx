"""Acceptance tests for cross-repository blob mount reuse (`:from=<repo>`).

A layer pushed once to a source repository can be referenced from other
repositories in the same registry via `<ref>:from=<source-repo>` — the
registry mounts the existing blob instead of re-uploading it. These tests
exercise the mount-hit path end to end against the live `registry:2` fixture:
the layer's digest already exists in the source repository by the time the
second push runs, so the registry's cross-repo mount succeeds (HTTP 201) —
the "declined mount falls back to upload" path is covered at the unit level
(`oci::client::tests::mount_reuse` in `ocx_lib`), not here.
"""
from __future__ import annotations

import json
from pathlib import Path

from src import OcxRunner, assert_dir_exists, current_platform


def _make_shared_layer(tmp_path: Path, content: str) -> Path:
    layer_dir = tmp_path / "shared-layer"
    (layer_dir / "share").mkdir(parents=True)
    (layer_dir / "share" / "NOTES.md").write_text(content)
    return layer_dir


def _bundle(ocx: OcxRunner, layer_dir: Path, tmp_path: Path, name: str) -> Path:
    bundle = tmp_path / f"{name}.tar.gz"
    metadata_path = tmp_path / f"{name}-meta.json"
    metadata_path.write_text(json.dumps({
        "type": "bundle", "version": 1, "env": [
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        ],
    }))
    ocx.plain("package", "create", "-m", str(metadata_path), "-o", str(bundle), str(layer_dir))
    return bundle


def test_package_push_mount_cross_repository_reuse(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """A layer pushed once can be mounted (not re-uploaded) into two other
    repositories via `:from=<source-repo>`, and the mounted content installs
    intact.
    """
    source_repo = f"{unique_repo}-pip-test-pkg"
    app_a_repo = f"{unique_repo}-app-a"
    app_b_repo = f"{unique_repo}-app-b"
    plat = current_platform()

    layer_dir = _make_shared_layer(tmp_path, "shared layer content\n")
    bundle = _bundle(ocx, layer_dir, tmp_path, "shared-layer")

    # (1) Push the layer archive as its own package — establishes the blob in
    # the source repository so later mounts have something to reuse.
    ocx.plain(
        "package", "push", "-p", plat, "-n",
        "-i", f"{ocx.registry}/{source_repo}:1.0.0",
        str(bundle),
    )

    # (2) Push app-a referencing the SAME layer file suffixed `:from=<source_repo>`.
    # The registry must mount the blob already present in source_repo rather
    # than re-uploading it.
    report_a = ocx.json(
        "package", "push", "-p", plat, "-n",
        "-i", f"{ocx.registry}/{app_a_repo}:1.0.0",
        f"{bundle}:from={source_repo}",
    )
    assert report_a["layers"]["mounted"] == 1, f"expected a mount hit, got {report_a['layers']}"
    assert report_a["layers"]["uploaded"] == 0, f"a mount hit must not also upload, got {report_a['layers']}"

    # (3) Push app-b the same way — cross-repo reuse works from a second,
    # independent target repository too (not just the original app-a mount).
    report_b = ocx.json(
        "package", "push", "-p", plat, "-n",
        "-i", f"{ocx.registry}/{app_b_repo}:1.0.0",
        f"{bundle}:from={source_repo}",
    )
    assert report_b["layers"]["mounted"] == 1, f"expected a mount hit, got {report_b['layers']}"
    assert report_b["layers"]["uploaded"] == 0, f"a mount hit must not also upload, got {report_b['layers']}"

    # (4) Install app-b and verify the mounted layer's content is intact.
    ocx.plain("index", "update", f"{app_b_repo}:1.0.0")
    result = ocx.json("package", "install", f"{app_b_repo}:1.0.0")
    content = Path(result[f"{app_b_repo}:1.0.0"]["path"]) / "content"

    assert_dir_exists(content)
    notes = content / "share" / "NOTES.md"
    assert notes.exists(), "mounted layer content missing after install"
    assert notes.read_text() == "shared layer content\n"
