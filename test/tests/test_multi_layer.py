"""Multi-layer package push and pull acceptance tests.

These tests exercise the full lifecycle of multi-layer packages:
push with multiple --layer flags, install with multi-layer assembly,
layer reuse via digest references, and error paths.
"""

from __future__ import annotations

import json
import stat
import urllib.request
from pathlib import Path

from src import OcxRunner, assert_dir_exists, current_platform


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_layer_content(
    tmp_path: Path,
    name: str,
    files: dict[str, str],
) -> Path:
    """Create a directory with the given file tree, ready for bundling."""
    layer_dir = tmp_path / f"layer-{name}"
    for rel_path, content in files.items():
        p = layer_dir / rel_path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
        if rel_path.startswith("bin/"):
            p.chmod(p.stat().st_mode | stat.S_IEXEC)
    return layer_dir


def _bundle_layer(
    ocx: OcxRunner,
    layer_dir: Path,
    tmp_path: Path,
    *,
    ext: str = "tar.gz",
) -> Path:
    """Bundle a layer directory into an archive.

    `ext` controls the output format — `package create` picks compression
    from the output filename extension, so "tar.gz"/"tgz" → gzip,
    "tar.xz"/"txz" → xz.
    """
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


def _push_multi_layer(
    ocx: OcxRunner,
    repo: str,
    tag: str,
    layers: list[str],
    tmp_path: Path,
    *,
    new: bool = True,
    cascade: bool = False,
    metadata_path: Path | None = None,
) -> str:
    """Push a multi-layer package. Layers are positional args after the identifier.

    Returns the fully-qualified identifier.
    """
    fq = f"{ocx.registry}/{repo}:{tag}"
    if metadata_path is None:
        metadata_path = tmp_path / f"meta-{repo}-{tag}.json"
        metadata_path.write_text(json.dumps({
            "type": "bundle",
            "version": 1,
            "env": [
                {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
            ],
        }))

    plat = current_platform()
    args = ["package", "push", "-p", plat, "-m", str(metadata_path)]
    if new:
        args.append("-n")
    if cascade:
        args.append("--cascade")
    args.append(fq)
    args.extend(layers)
    ocx.plain(*args)
    return fq


# ---------------------------------------------------------------------------
# Push tests
# ---------------------------------------------------------------------------


def test_push_multi_layer_files(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Push a 2-layer package from archive files, verify it succeeds."""
    layer_a = _make_layer_content(tmp_path, "a", {"lib/liba.so": "liba"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho hello\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    fq = _push_multi_layer(ocx, unique_repo, "1.0.0", [str(bundle_a), str(bundle_b)], tmp_path)
    # If we get here without error, the push succeeded.
    assert fq.endswith(f"{unique_repo}:1.0.0")


def test_push_single_layer(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A single positional layer is accepted (no flag needed)."""
    layer = _make_layer_content(tmp_path, "only", {"bin/tool": "#!/bin/sh\necho ok\n"})
    bundle = _bundle_layer(ocx, layer, tmp_path)

    _push_multi_layer(ocx, unique_repo, "1.0.0", [str(bundle)], tmp_path)


def test_push_zero_layers_succeeds_with_metadata(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Push with zero layers but explicit `--metadata` is a valid
    OCI config-only artifact (e.g. referrer-only / description-only
    manifests). Verify it round-trips via the registry manifest API."""
    meta = tmp_path / "meta.json"
    meta.write_text(json.dumps({
        "type": "bundle", "version": 1,
        "env": [{"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"}],
    }))
    plat = current_platform()
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    ocx.plain(
        "package", "push", "-p", plat, "-m", str(meta), "-n", fq,
    )

    # Walk the index → per-platform manifest and confirm `layers: []`.
    index = _fetch_manifest(ocx.registry, unique_repo, "1.0.0")
    first_manifest_digest = index["manifests"][0]["digest"]
    image_manifest = _fetch_manifest(ocx.registry, unique_repo, first_manifest_digest)
    assert image_manifest["layers"] == [], (
        f"expected empty layers array for config-only artifact, got: {image_manifest['layers']!r}"
    )


def test_push_zero_layers_without_metadata_fails(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Push with zero layers and no `--metadata` is rejected before any
    network I/O — the CLI cannot infer a sibling metadata path without
    a file layer to key off."""
    plat = current_platform()
    fq = f"{ocx.registry}/{unique_repo}:1.0.0"
    result = ocx.run(
        "package", "push", "-p", plat, "-n", fq,
        check=False, format=None,
    )
    assert result.returncode != 0
    combined = (result.stderr + result.stdout).lower()
    assert "--metadata" in combined or "metadata is required" in combined, (
        f"expected a metadata-required error, got:\n{result.stderr}\n{result.stdout}"
    )


# ---------------------------------------------------------------------------
# Round-trip tests (push + install)
# ---------------------------------------------------------------------------


def test_round_trip_zero_layers(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Push a config-only package (zero layers + metadata), install it,
    verify the install succeeds and produces an empty content/ directory."""
    meta = tmp_path / "meta.json"
    meta.write_text(json.dumps({
        "type": "bundle", "version": 1,
        "env": [{"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"}],
    }))
    plat = current_platform()
    short = f"{unique_repo}:1.0.0"
    fq = f"{ocx.registry}/{short}"
    ocx.plain("package", "push", "-p", plat, "-m", str(meta), "-n", fq)
    ocx.plain("index", "update", short)

    result = ocx.json("install", short)
    content = Path(result[short]["path"]) / "content"

    assert_dir_exists(content)
    assert list(content.iterdir()) == [], (
        f"expected empty content/ for zero-layer package, got: {list(content.iterdir())!r}"
    )


def test_round_trip_multi_layer(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Push a 2-layer package, install it, verify all files from both layers present."""
    layer_a = _make_layer_content(tmp_path, "a", {"lib/liba.so": "liba-content"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho hello\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    short = f"{unique_repo}:1.0.0"
    _push_multi_layer(ocx, unique_repo, "1.0.0", [str(bundle_a), str(bundle_b)], tmp_path)
    ocx.plain("index", "update", short)
    result = ocx.json("install", short)
    content = Path(result[short]["path"]) / "content"

    assert_dir_exists(content)
    assert (content / "lib" / "liba.so").exists(), "File from layer A missing"
    assert (content / "lib" / "liba.so").read_text() == "liba-content"
    assert (content / "bin" / "tool").exists(), "File from layer B missing"


def test_round_trip_shared_directory(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Push a 2-layer package where layers share a directory (bin/), install, verify merged."""
    layer_a = _make_layer_content(tmp_path, "a", {"bin/tool_a": "#!/bin/sh\necho a\n"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool_b": "#!/bin/sh\necho b\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    short = f"{unique_repo}:1.0.0"
    _push_multi_layer(ocx, unique_repo, "1.0.0", [str(bundle_a), str(bundle_b)], tmp_path)
    ocx.plain("index", "update", short)
    result = ocx.json("install", short)
    content = Path(result[short]["path"]) / "content"

    assert (content / "bin" / "tool_a").exists(), "File from layer A missing in shared dir"
    assert (content / "bin" / "tool_b").exists(), "File from layer B missing in shared dir"


def test_round_trip_layer_overlap_fails(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Push a 2-layer package with overlapping files, install fails with overlap error."""
    layer_a = _make_layer_content(tmp_path, "a", {"bin/conflict": "#!/bin/sh\necho a\n"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/conflict": "#!/bin/sh\necho b\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    short = f"{unique_repo}:1.0.0"
    _push_multi_layer(ocx, unique_repo, "1.0.0", [str(bundle_a), str(bundle_b)], tmp_path)
    ocx.plain("index", "update", short)

    result = ocx.run("install", short, check=False)
    assert result.returncode != 0
    assert "overlap" in result.stderr.lower() or "conflict" in result.stderr.lower()


def _fetch_manifest(registry: str, repo: str, ref: str) -> dict:
    url = f"http://{registry}/v2/{repo}/manifests/{ref}"
    accept = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json"
    req = urllib.request.Request(url, headers={"Accept": accept})
    resp = urllib.request.urlopen(req, timeout=5)
    return json.loads(resp.read())


def _fetch_layer_digest(registry: str, repo: str, tag: str) -> str:
    """Walk `repo:tag` (image index → per-platform image manifest) and
    return the first layer descriptor's digest."""
    index = _fetch_manifest(registry, repo, tag)
    first_manifest_digest = index["manifests"][0]["digest"]
    image_manifest = _fetch_manifest(registry, repo, first_manifest_digest)
    return image_manifest["layers"][0]["digest"]


def test_push_digest_layer_reuse(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Push layer A (tar.gz) as v1, then push v2 with A by digest + new layer B."""
    layer_a = _make_layer_content(tmp_path, "a", {"lib/shared.so": "shared-lib"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/new_tool": "#!/bin/sh\necho v2\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    _push_multi_layer(ocx, unique_repo, "1.0.0", [str(bundle_a)], tmp_path)
    ocx.plain("index", "update", f"{unique_repo}:1.0.0")

    layer_a_digest = _fetch_layer_digest(ocx.registry, unique_repo, "1.0.0")

    # The new CLI syntax requires the caller to declare the media type
    # of the reused layer alongside its digest. `.tar.gz` maps to
    # application/vnd.oci.image.layer.v1.tar+gzip via the existing
    # filename extension table.
    _push_multi_layer(
        ocx, unique_repo, "2.0.0",
        [f"{layer_a_digest}.tar.gz", str(bundle_b)],
        tmp_path, new=False,
    )
    ocx.plain("index", "update", f"{unique_repo}:2.0.0")

    result = ocx.json("install", f"{unique_repo}:2.0.0")
    content = Path(result[f"{unique_repo}:2.0.0"]["path"]) / "content"
    assert (content / "lib" / "shared.so").exists(), "Digest-referenced layer A missing"
    assert (content / "bin" / "new_tool").exists(), "File layer B missing"


def test_push_digest_layer_reuse_tar_xz(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Regression for the fabricated-media-type bug: a `.tar.xz` layer
    reused by digest must round-trip through push + install without
    the publisher silently declaring it as `tar+gzip`.

    On the buggy code the manifest for v2 would carry
    `application/vnd.oci.image.layer.v1.tar+gzip` regardless of the
    real archive format, and `ocx install` would then fail at pull
    time because `compression::CompressionAlgorithm::from_media_type`
    returns `Gzip` while the bytes on disk are xz-compressed.
    """
    layer_a = _make_layer_content(tmp_path, "a", {"lib/liba.so": "xz-shared"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho hi\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path, ext="tar.xz")
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path, ext="tar.gz")

    _push_multi_layer(ocx, unique_repo, "1.0.0", [str(bundle_a)], tmp_path)
    ocx.plain("index", "update", f"{unique_repo}:1.0.0")

    layer_a_digest = _fetch_layer_digest(ocx.registry, unique_repo, "1.0.0")

    _push_multi_layer(
        ocx, unique_repo, "2.0.0",
        [f"{layer_a_digest}.tar.xz", str(bundle_b)],
        tmp_path, new=False,
    )
    ocx.plain("index", "update", f"{unique_repo}:2.0.0")

    # Walk the pushed v2 manifest and assert the reused layer descriptor
    # carries the correct media type. Catches the regression at the
    # manifest layer rather than waiting for install-time extraction to
    # blow up on a mismatched compression algorithm.
    index = _fetch_manifest(ocx.registry, unique_repo, "2.0.0")
    image_manifest = _fetch_manifest(
        ocx.registry, unique_repo, index["manifests"][0]["digest"]
    )
    reused = next(
        layer for layer in image_manifest["layers"] if layer["digest"] == layer_a_digest
    )
    assert reused["mediaType"] == "application/vnd.oci.image.layer.v1.tar+xz", (
        f"reused layer A should declare tar+xz, got {reused['mediaType']}"
    )

    result = ocx.json("install", f"{unique_repo}:2.0.0")
    content = Path(result[f"{unique_repo}:2.0.0"]["path"]) / "content"
    assert (content / "lib" / "liba.so").exists(), "xz layer A not extracted on consumer"
    assert (content / "lib" / "liba.so").read_text() == "xz-shared"
    assert (content / "bin" / "tool").exists(), "File layer B missing"


def test_push_bare_digest_is_rejected(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A bare `sha256:<hex>` with no `.<ext>` suffix must be refused at
    the CLI layer — OCX refuses to guess the media type of a reused
    layer, because a wrong guess silently breaks consumers."""
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho ok\n"})
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    (tmp_path / "meta.json").write_text(json.dumps({
        "type": "bundle", "version": 1,
        "env": [{"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"}],
    }))
    bare_digest = "sha256:" + "0" * 64
    result = ocx.run(
        "package", "push", "-p", current_platform(),
        "-m", str(tmp_path / "meta.json"),
        "-n",
        f"{ocx.registry}/{unique_repo}:1.0.0",
        bare_digest,
        str(bundle_b),
        check=False, format=None,
    )
    assert result.returncode != 0
    combined = (result.stderr + result.stdout).lower()
    assert "bare layer digest" in combined or "extension suffix" in combined, (
        f"expected a bare-digest error mentioning the required suffix, got:\n{result.stderr}\n{result.stdout}"
    )


def test_push_digest_layer_not_found(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A well-formed `sha256:<hex>.tar.gz` pointing at a blob the
    registry doesn't actually have fails with a clear error."""
    fake_digest = "sha256:" + "0" * 64
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho ok\n"})
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    (tmp_path / "meta.json").write_text(json.dumps({
        "type": "bundle", "version": 1,
        "env": [{"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"}],
    }))
    result = ocx.run(
        "package", "push", "-p", current_platform(),
        "-m", str(tmp_path / "meta.json"),
        "-n",
        f"{ocx.registry}/{unique_repo}:1.0.0",
        f"{fake_digest}.tar.gz",
        str(bundle_b),
        check=False, format=None,
    )
    assert result.returncode != 0
    assert "blob not found" in result.stderr.lower()


def test_push_digest_only_without_metadata_fails(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """When every layer is a digest reference, `--metadata` is mandatory —
    there is no file layer to sniff a sibling metadata path from. The
    guard must fire before any network I/O."""
    # Stage a real layer so we have a valid digest on the registry to
    # reference. Without this, the push could fail for the wrong reason
    # (BlobNotFound) and hide the guard we're trying to cover.
    layer_a = _make_layer_content(tmp_path, "a", {"lib/a.so": "a"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    _push_multi_layer(ocx, unique_repo, "1.0.0", [str(bundle_a)], tmp_path)
    ocx.plain("index", "update", f"{unique_repo}:1.0.0")
    layer_a_digest = _fetch_layer_digest(ocx.registry, unique_repo, "1.0.0")

    result = ocx.run(
        "package", "push", "-p", current_platform(),
        "-n",
        f"{ocx.registry}/{unique_repo}:2.0.0",
        f"{layer_a_digest}.tar.gz",
        check=False, format=None,
    )
    assert result.returncode != 0
    combined = (result.stderr + result.stdout).lower()
    assert "--metadata" in combined or "metadata is required" in combined, (
        f"expected a metadata-required error, got:\n{result.stderr}\n{result.stdout}"
    )


def test_cascade_multi_layer(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Push a 2-layer package with --cascade, verify rolling tags exist."""
    layer_a = _make_layer_content(tmp_path, "a", {"lib/liba.so": "liba"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho hi\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    _push_multi_layer(
        ocx, unique_repo, "1.2.3",
        [str(bundle_a), str(bundle_b)],
        tmp_path, cascade=True,
    )
    # Index all tags so we can verify cascade
    ocx.plain("index", "update", unique_repo)
    result = ocx.json("index", "list", unique_repo)
    tags = result[unique_repo]
    assert "1.2.3" in tags
    assert "1.2" in tags
    assert "1" in tags
