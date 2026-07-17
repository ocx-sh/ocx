"""Per-layer layout (strip + prefix) acceptance tests.

Covers the Part 2 feature: optional per-layer `strip` + output `prefix`
carried in each manifest layer descriptor's `annotations`
(`sh.ocx.layer.*`), driven from the CLI layer-ref grammar
`<ref>:strip=N,prefix=P`.

Contract-first: these fail until the grammar + read-boundary resolution +
the layout-aware assembler land (plan Implement — Part 2). Rows cite the
plan Test Matrix tag (A#).
"""

from __future__ import annotations

import hashlib
import json
import stat
import urllib.error
import urllib.request
from pathlib import Path

from src import OcxRunner, assert_dir_exists, assert_not_exists, current_platform

_INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"
_MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"


def _make_layer_content(tmp_path: Path, name: str, files: dict[str, str]) -> Path:
    layer_dir = tmp_path / f"layer-{name}"
    for rel_path, content in files.items():
        p = layer_dir / rel_path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
        if rel_path.startswith("bin/") or "/bin/" in rel_path:
            p.chmod(p.stat().st_mode | stat.S_IEXEC)
    return layer_dir


def _bundle_layer(ocx: OcxRunner, layer_dir: Path, tmp_path: Path) -> Path:
    bundle = tmp_path / f"{layer_dir.name}.tar.gz"
    metadata_path = tmp_path / f"{layer_dir.name}-meta.json"
    metadata_path.write_text(json.dumps({
        "type": "bundle", "version": 1, "env": [
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        ],
    }))
    ocx.plain("package", "create", "-m", str(metadata_path), "-o", str(bundle), str(layer_dir))
    return bundle


def _write_meta(tmp_path: Path) -> Path:
    meta = tmp_path / "meta.json"
    meta.write_text(json.dumps({
        "type": "bundle", "version": 1, "platform": current_platform(), "env": [
            {"key": "PATH", "type": "path", "required": True, "value": "${installPath}/bin"},
        ],
    }))
    return meta


def _fetch_manifest(registry: str, repo: str, ref: str) -> dict:
    url = f"http://{registry}/v2/{repo}/manifests/{ref}"
    accept = f"{_INDEX_MEDIA_TYPE}, {_MANIFEST_MEDIA_TYPE}"
    req = urllib.request.Request(url, headers={"Accept": accept})
    resp = urllib.request.urlopen(req, timeout=5)
    return json.loads(resp.read())


def _put_manifest(registry: str, repo: str, reference: str, body: bytes, content_type: str) -> str:
    """PUT a raw manifest/index body under `reference` (tag or digest).

    Returns the stored digest (from the Docker-Content-Digest header, else
    computed from the body).
    """
    url = f"http://{registry}/v2/{repo}/manifests/{reference}"
    req = urllib.request.Request(url, data=body, method="PUT", headers={"Content-Type": content_type})
    resp = urllib.request.urlopen(req, timeout=5)
    digest = resp.headers.get("Docker-Content-Digest")
    return digest or f"sha256:{hashlib.sha256(body).hexdigest()}"


# ---------------------------------------------------------------------------
# A4 — end-to-end strip + prefix via the CLI grammar
# ---------------------------------------------------------------------------


def test_layer_ref_strip_and_prefix_round_trip(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A4 (D1/D5): push `layerA:strip=1,prefix=share layerB`, install, and
    verify layer A lands stripped under `content/share/...` while layer B
    lands at the content root — with no overlap.
    """
    # Layer A ships under a `topdir/` that strip=1 removes; prefix=share then
    # relocates the remainder under `share/`.
    layer_a = _make_layer_content(tmp_path, "a", {"topdir/lib/liba.so": "liba-content"})
    layer_b = _make_layer_content(tmp_path, "b", {"bin/tool": "#!/bin/sh\necho hello\n"})
    bundle_a = _bundle_layer(ocx, layer_a, tmp_path)
    bundle_b = _bundle_layer(ocx, layer_b, tmp_path)

    meta = _write_meta(tmp_path)
    short = f"{unique_repo}:1.0.0"
    fq = f"{ocx.registry}/{short}"
    ocx.plain(
        "package", "push", "-p", current_platform(), "-m", str(meta), "-n", "-i", fq,
        f"{bundle_a}:strip=1,prefix=share",
        str(bundle_b),
    )
    ocx.plain("index", "update", short)

    result = ocx.json("package", "install", short)
    content = Path(result[short]["path"]) / "content"

    assert_dir_exists(content)
    # Layer A: strip=1 dropped `topdir/`, prefix=share relocated the rest.
    assert (content / "share" / "lib" / "liba.so").exists(), "Layer A must land under share/ stripped"
    assert (content / "share" / "lib" / "liba.so").read_text() == "liba-content"
    assert_not_exists(content / "topdir")
    assert_not_exists(content / "lib" / "liba.so")
    # Layer B: no layout, lands at the content root.
    assert (content / "bin" / "tool").exists(), "Layer B must land at the content root"


# ---------------------------------------------------------------------------
# B4 — the local path (`ocx package test`) also wires layer annotations
# ---------------------------------------------------------------------------


def test_local_path_layer_layout_is_applied(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """B4 (local push-wiring): `ocx package test` materializes a layout-bearing
    `LayerRef::File` through `pull_local::stage_layers`, which must populate the
    layer descriptor's `annotations`. If the local wiring drops annotations, the
    read boundary resolves strip=0 / prefix="" and the content is NOT relocated
    — so this test fails, catching a dropped-annotation regression on the local
    path (distinct from the registry push path exercised by A4/A5).
    """
    layer = _make_layer_content(tmp_path, "a", {"topdir/lib/liba.so": "liba-content"})
    bundle = _bundle_layer(ocx, layer, tmp_path)
    meta = _write_meta(tmp_path)

    # --output must share a filesystem with $OCX_HOME/layers (same tmpfs), so
    # place it under OCX_HOME.
    output_dir = Path(ocx.ocx_home) / "layout-local-out" / unique_repo
    output_dir.mkdir(parents=True, exist_ok=True)

    result = ocx.plain(
        "package", "test",
        "-p", current_platform(),
        "-m", str(meta),
        "-o", str(output_dir),
        "-i", f"{unique_repo}:1.0.0",
        f"{bundle}:strip=1,prefix=share",
        "--",
        "sh", "-c", "true",
        check=False,
    )
    assert result.returncode == 0, (
        f"local layout materialization must succeed, got {result.returncode}\n"
        f"{result.stderr}\n{result.stdout}"
    )

    content = output_dir / "content"
    assert_dir_exists(content)
    # strip=1 dropped `topdir/`; prefix=share relocated the remainder. A dropped
    # annotation would leave the file at `topdir/lib/liba.so`.
    assert (content / "share" / "lib" / "liba.so").exists(), (
        "local path must apply the layer annotation (strip + prefix)"
    )
    assert (content / "share" / "lib" / "liba.so").read_text() == "liba-content"
    assert_not_exists(content / "topdir")
    assert_not_exists(content / "lib" / "liba.so")


# ---------------------------------------------------------------------------
# A6 — read boundary rejects a hostile prefix annotation
# ---------------------------------------------------------------------------


def test_pull_rejects_escaping_prefix_annotation(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A6 (security · D10): a manifest whose layer annotation encodes an
    escaping `prefix` is rejected at pull time with a clear error, and no
    file is written outside the package tree. Registries are third-party
    writable, so the read boundary must re-validate the annotation.
    """
    # 1. Push a normal single-layer package.
    layer = _make_layer_content(tmp_path, "a", {"bin/tool": "#!/bin/sh\necho hi\n"})
    bundle = _bundle_layer(ocx, layer, tmp_path)
    meta = _write_meta(tmp_path)
    ocx.plain(
        "package", "push", "-p", current_platform(), "-m", str(meta), "-n",
        "-i", f"{ocx.registry}/{unique_repo}:1.0.0", str(bundle),
    )

    # 2. Fetch the image index -> per-platform image manifest.
    index = _fetch_manifest(ocx.registry, unique_repo, "1.0.0")
    image_digest = index["manifests"][0]["digest"]
    image = _fetch_manifest(ocx.registry, unique_repo, image_digest)

    # 3. Inject an escaping prefix annotation onto the layer descriptor. The
    #    layer blob digest is unchanged, so the registry still accepts the
    #    mutated manifest (all referenced blobs already exist).
    image["layers"][0].setdefault("annotations", {})["sh.ocx.layer.prefix"] = "../../evil"

    # 4. PUT the mutated image manifest by its (new) digest.
    image_body = json.dumps(image).encode()
    new_image_digest = _put_manifest(
        ocx.registry, unique_repo,
        f"sha256:{hashlib.sha256(image_body).hexdigest()}",
        image_body, _MANIFEST_MEDIA_TYPE,
    )

    # 5. Rebuild the index to point at the mutated manifest, PUT under a new tag.
    index["manifests"][0]["digest"] = new_image_digest
    index["manifests"][0]["size"] = len(image_body)
    index_body = json.dumps(index).encode()
    _put_manifest(ocx.registry, unique_repo, "2.0.0", index_body, _INDEX_MEDIA_TYPE)

    # 6. Pull it: the escaping prefix must be rejected, nothing written out.
    ocx.plain("index", "update", f"{unique_repo}:2.0.0")
    result = ocx.run("package", "install", f"{unique_repo}:2.0.0", check=False)
    assert result.returncode == 65, (
        "an escaping prefix annotation must be rejected with DataError (65), got "
        f"{result.returncode}\n{result.stderr}\n{result.stdout}"
    )
    combined = (result.stderr + result.stdout).lower()
    assert "prefix" in combined or "invalid" in combined or "escape" in combined, (
        f"expected a clear prefix/layout error, got:\n{result.stderr}\n{result.stdout}"
    )
    # Defence-in-depth: no directory named `evil` may be written anywhere under
    # OCX_HOME (a botched implementation would create the escaped path).
    escaped = list(Path(ocx.ocx_home).rglob("evil"))
    assert not escaped, f"escaping prefix wrote outside the package tree: {escaped}"
