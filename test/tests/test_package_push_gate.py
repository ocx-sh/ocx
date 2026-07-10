"""Acceptance tests for the `ocx package push` dependency gate + fan-out.

Push is a pure gate plus mechanical multi-platform materialization
(adr_dependency_manifest_pinning.md): it rejects unpinned or index-pinned
dependencies (create is the compiler) and fans out one published manifest per
platform in the sidecar's embedded target set.
"""

from __future__ import annotations

import io
import json
import tarfile
from pathlib import Path

from src.helpers import make_package
from src.registry import (
    fetch_blob,
    fetch_manifest_from_registry,
    fetch_platform_manifest_digest,
    push_raw_package,
)
from src.runner import OcxRunner, current_platform

EXIT_USAGE = 64
EXIT_DATA_ERR = 65
EXIT_NOT_FOUND = 79


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _fetch_manifest_by_digest(registry: str, repo: str, digest: str) -> dict:
    """GET a child image manifest by digest (manifests endpoint, not blobs)."""
    import urllib.request

    req = urllib.request.Request(
        f"http://{registry}/v2/{repo}/manifests/{digest}",
        headers={"Accept": "application/vnd.oci.image.manifest.v1+json"},
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read().decode())


def _bundle(ocx: OcxRunner, tmp_path: Path, name: str) -> Path:
    """Create a metadata-less bundle to feed `push -m` with hand-made metadata."""
    pkg_dir = tmp_path / f"content-{name}"
    (pkg_dir / "bin").mkdir(parents=True)
    (pkg_dir / "bin" / "app").write_text("#!/bin/sh\necho app\n")
    out = tmp_path / f"{name}.tar.xz"
    ocx.plain("package", "create", "-o", str(out), str(pkg_dir))
    return out

def _write_metadata(tmp_path: Path, name: str, obj: dict) -> Path:
    path = tmp_path / f"{name}-metadata.json"
    path.write_text(json.dumps(obj))
    return path


def _created_app(
    ocx: OcxRunner, tmp_path: Path, name: str, deps: list[dict], platform: str
) -> Path:
    """Run `ocx package create -p` so the OUTPUT sidecar carries resolved pins
    plus the embedded target set; push infers that sidecar from the bundle."""
    pkg_dir = tmp_path / f"content-{name}"
    (pkg_dir / "bin").mkdir(parents=True)
    (pkg_dir / "bin" / "app").write_text("#!/bin/sh\necho app\n")
    metadata = _write_metadata(tmp_path, f"authored-{name}", {
        "type": "bundle",
        "version": 1,
        "dependencies": deps,
    })
    out = tmp_path / f"{name}.tar.xz"
    ocx.plain(
        "package", "create", "-m", str(metadata), "-o", str(out), "-p", platform, str(pkg_dir)
    )
    return out


def _push(ocx: OcxRunner, fq: str, bundle: Path, *args: str, check: bool = True):
    return ocx.run("package", "push", "-n", *args, "-i", fq, str(bundle), check=check)


def _two_platform_leaf(ocx: OcxRunner, repo: str, tmp_path: Path):
    """Publish one dep tag carrying BOTH linux/amd64 and darwin/arm64 children."""
    make_package(ocx, repo, "1.0.0", tmp_path, platform="linux/amd64", cascade=False)
    return make_package(
        ocx, repo, "1.0.0", tmp_path.joinpath("second"), platform="darwin/arm64",
        cascade=False, new=False,
    )


# ---------------------------------------------------------------------------
# Gate: rejections
# ---------------------------------------------------------------------------


def test_push_rejects_unpinned_dep(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    bundle = _bundle(ocx, tmp_path, "unpinned")
    metadata = _write_metadata(tmp_path, "unpinned", {
        "type": "bundle", "version": 1,
        "dependencies": [{"identifier": leaf.fq}],
    })

    result = _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), "-p", current_platform(), check=False,
    )
    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "ocx package create" in result.stderr, "error must point at create"


def test_push_rejects_index_pinned_dep(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """The exact hazard this feature kills: pinning the tag's index digest."""
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    index = fetch_manifest_from_registry(ocx.registry, leaf.repo, leaf.tag)
    assert "index" in index.get("mediaType", ""), "leaf tag must be an image index"
    from src.registry import fetch_manifest_digest  # index digest, deliberately

    index_digest = fetch_manifest_digest(ocx.registry, leaf.repo, leaf.tag)
    bundle = _bundle(ocx, tmp_path, "indexpin")
    metadata = _write_metadata(tmp_path, "indexpin", {
        "type": "bundle", "version": 1,
        "dependencies": [{"identifier": f"{leaf.fq}@{index_digest}"}],
    })

    result = _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), "-p", current_platform(), check=False,
    )
    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "INDEX" in result.stderr, "error must explain the index-pin hazard"


def test_push_rejects_missing_dep_manifest(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    ghost_digest = "sha256:" + "c" * 64
    bundle = _bundle(ocx, tmp_path, "ghost")
    metadata = _write_metadata(tmp_path, "ghost", {
        "type": "bundle", "version": 1,
        "dependencies": [{"identifier": f"{ocx.registry}/{unique_repo}_ghost:1.0.0@{ghost_digest}"}],
    })

    result = _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), "-p", current_platform(), check=False,
    )
    assert result.returncode == EXIT_NOT_FOUND, result.stderr


def test_push_accepts_manifest_pinned_dep(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    manifest_digest = fetch_platform_manifest_digest(ocx.registry, leaf.repo, leaf.tag)
    bundle = _bundle(ocx, tmp_path, "pinned")
    metadata = _write_metadata(tmp_path, "pinned", {
        "type": "bundle", "version": 1,
        "dependencies": [{"identifier": f"{leaf.fq}@{manifest_digest}"}],
    })

    _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), "-p", current_platform(),
    )


# ---------------------------------------------------------------------------
# Platform-set decision (D1)
# ---------------------------------------------------------------------------


def test_push_legacy_sidecar_requires_platform(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A sidecar without an embedded target set keeps requiring -p."""
    bundle = _bundle(ocx, tmp_path, "legacy")
    metadata = _write_metadata(tmp_path, "legacy", {"type": "bundle", "version": 1})

    result = _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), check=False,
    )
    assert result.returncode == EXIT_USAGE, result.stderr

    _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), "-p", current_platform(),
    )


def test_push_platform_must_be_in_embedded_set(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Embedded set wins; -p is a member-filter and must be a member."""
    bundle = _created_app(ocx, tmp_path, "memberfilter", [], "any")

    result = _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-p", "linux/amd64", check=False,
    )
    assert result.returncode == EXIT_USAGE, result.stderr

    _push(ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle, "-p", "any")


def test_push_platform_narrows_multi_member_set(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`-p` genuinely NARROWS a multi-member embedded target set to the named
    platform alone — it must not fan out to every platform in the set. The
    single-member fixture above (`["any"]`) cannot distinguish "narrow" from
    "fan out everything"; this one embeds two platforms so the assertion is
    meaningful."""
    dep_repo = f"{unique_repo}_dep"
    _two_platform_leaf(ocx, dep_repo, tmp_path)
    dep_fq = f"{ocx.registry}/{dep_repo}:1.0.0"

    bundle = _created_app(ocx, tmp_path, "narrow", [{"identifier": dep_fq}], "any")
    app_fq = f"{ocx.registry}/{unique_repo}_app:1.0.0"
    _push(ocx, app_fq, bundle, "-p", "linux/amd64")

    index = fetch_manifest_from_registry(ocx.registry, f"{unique_repo}_app", "1.0.0")
    children = index["manifests"]
    assert len(children) == 1, f"-p must narrow to a single platform, not fan out: {children}"
    plat = children[0]["platform"]
    assert f"{plat['os']}/{plat['architecture']}" == "linux/amd64"


# ---------------------------------------------------------------------------
# Fan-out
# ---------------------------------------------------------------------------


def test_push_fanout_materializes_single_pins_per_platform(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`create -p any` + platform-specific dep -> push fans out one manifest
    per covered platform; each child config blob carries a SINGLE manifest
    pin for its platform and no sidecar-only fields."""
    dep_repo = f"{unique_repo}_dep"
    _two_platform_leaf(ocx, dep_repo, tmp_path)
    dep_fq = f"{ocx.registry}/{dep_repo}:1.0.0"

    bundle = _created_app(
        ocx, tmp_path, "fanout", [{"identifier": dep_fq}], "any"
    )
    app_fq = f"{ocx.registry}/{unique_repo}_app:1.0.0"
    _push(ocx, app_fq, bundle)

    index = fetch_manifest_from_registry(ocx.registry, f"{unique_repo}_app", "1.0.0")
    children = index["manifests"]
    platforms = {
        f"{c['platform']['os']}/{c['platform']['architecture']}" for c in children
    }
    assert platforms == {"linux/amd64", "darwin/arm64"}

    expected = {
        "linux/amd64": fetch_platform_manifest_digest(
            ocx.registry, dep_repo, "1.0.0", platform="linux/amd64"
        ),
        "darwin/arm64": fetch_platform_manifest_digest(
            ocx.registry, dep_repo, "1.0.0", platform="darwin/arm64"
        ),
    }
    for child in children:
        plat = f"{child['platform']['os']}/{child['platform']['architecture']}"
        manifest = _fetch_manifest_by_digest(ocx.registry, f"{unique_repo}_app", child["digest"])
        config = json.loads(
            fetch_blob(
                ocx.registry, f"{unique_repo}_app", manifest["config"]["digest"]
            ).decode()
        )
        dep = config["dependencies"][0]
        assert dep["identifier"].endswith(f"@{expected[plat]}"), (
            f"{plat} child must pin that platform's manifest digest: {dep}"
        )
        assert "platforms" not in dep, "published dep must carry no pin map"
        assert "platforms" not in config, "published metadata must carry no target set"


def test_push_fanout_cascade_updates_rolling_tags(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    dep_repo = f"{unique_repo}_dep"
    _two_platform_leaf(ocx, dep_repo, tmp_path)
    bundle = _created_app(
        ocx, tmp_path, "cascade",
        [{"identifier": f"{ocx.registry}/{dep_repo}:1.0.0"}], "any",
    )
    app_repo = f"{unique_repo}_app"
    report = ocx.json(
        "package", "push", "-n", "--cascade",
        "-i", f"{ocx.registry}/{app_repo}:1.2.3", str(bundle),
    )
    # Ordered union (adr_dependency_manifest_pinning.md): both platforms cascade
    # to the same tag set with no blockers, so the written order matches the
    # cascade algebra's most-specific -> least-specific walk (patch's parent
    # levels, then `latest`) rather than being incidentally order-blind.
    assert report["cascade_tags_written"] == ["1.2", "1", "latest"]

    for tag in ("1.2.3", "1.2", "1", "latest"):
        index = fetch_manifest_from_registry(ocx.registry, app_repo, tag)
        platforms = {
            f"{c['platform']['os']}/{c['platform']['architecture']}"
            for c in index["manifests"]
        }
        assert platforms == {"linux/amd64", "darwin/arm64"}, f"tag {tag}: {platforms}"


def test_push_repush_is_idempotent(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    dep_repo = f"{unique_repo}_dep"
    _two_platform_leaf(ocx, dep_repo, tmp_path)
    bundle = _created_app(
        ocx, tmp_path, "idem",
        [{"identifier": f"{ocx.registry}/{dep_repo}:1.0.0"}], "any",
    )
    app_fq = f"{ocx.registry}/{unique_repo}_app:1.0.0"

    first = ocx.json("package", "push", "-n", "-i", app_fq, str(bundle))
    second = ocx.json("package", "push", "-n", "-i", app_fq, str(bundle))
    assert first["manifest_digest"] == second["manifest_digest"]


# ---------------------------------------------------------------------------
# End-to-end + read-path backward compat
# ---------------------------------------------------------------------------


def test_end_to_end_create_push_install(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Author tag-only -> create resolves -> push publishes -> install composes."""
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    bundle = _created_app(
        ocx, tmp_path, "e2e", [{"identifier": leaf.fq}], current_platform()
    )
    app_repo = f"{unique_repo}_app"
    _push(ocx, f"{ocx.registry}/{app_repo}:1.0.0", bundle)
    ocx.plain("index", "update", f"{app_repo}:1.0.0")

    ocx.json("package", "install", "--select", f"{app_repo}:1.0.0")

    packages_root = Path(ocx.ocx_home) / "packages"
    content_dirs = [p for p in packages_root.rglob("content") if p.is_dir()]
    assert len(content_dirs) == 2, "app + dep must both be materialized"


def test_install_still_resolves_legacy_index_pinned_package(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """Already-published packages with index-pinned deps keep installing:
    the gate is publish-time only, the read path is untouched."""
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    from src.registry import fetch_manifest_digest

    index_digest = fetch_manifest_digest(ocx.registry, leaf.repo, leaf.tag)

    # Raw-HTTP publish an app whose dep pins the leaf's INDEX digest — the
    # shape the gate now rejects, mirroring pre-gate published packages.
    layer_buffer = io.BytesIO()
    with tarfile.open(fileobj=layer_buffer, mode="w:xz") as tar:
        body = b"#!/bin/sh\necho legacy\n"
        info = tarfile.TarInfo(name="bin/legacy")
        info.size = len(body)
        info.mode = 0o755
        tar.addfile(info, io.BytesIO(body))
    metadata = {
        "type": "bundle",
        "version": 1,
        "dependencies": [{"identifier": f"{leaf.fq}@{index_digest}"}],
    }
    os_name, arch = current_platform().split("/")
    app_repo = f"{unique_repo}_legacyapp"
    push_raw_package(
        ocx.registry, app_repo, "1.0.0", metadata, layer_buffer.getvalue(),
        platform=(os_name, arch),
    )
    ocx.plain("index", "update", f"{app_repo}:1.0.0")

    ocx.json("package", "install", "--select", f"{app_repo}:1.0.0")

    packages_root = Path(ocx.ocx_home) / "packages"
    content_dirs = [p for p in packages_root.rglob("content") if p.is_dir()]
    assert len(content_dirs) == 2, "legacy index-pinned dep must still resolve"
