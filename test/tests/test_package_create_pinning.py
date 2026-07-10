"""Acceptance tests for `ocx package create` dependency pin resolution.

Create is the compiler of the create-resolves / push-gates split
(adr_dependency_manifest_pinning.md): tag-only dependencies in the metadata
sidecar are resolved to platform MANIFEST digests (never the GC-prone image
index digest) and the sidecar is rewritten canonically with the pins plus the
package's target-platform set.
"""

from __future__ import annotations

import json
from pathlib import Path

from src.helpers import make_package
from src.registry import (
    fetch_manifest_digest,
    fetch_manifest_from_registry,
    fetch_platform_manifest_digest,
)
from src.runner import OcxRunner, current_platform

EXIT_USAGE = 64  # UsageError — unpinned deps without --platform
EXIT_DATA_ERR = 65  # DataError — empty platform intersection etc.
EXIT_NOT_FOUND = 79  # NotFound — dependency tag absent
EXIT_POLICY = 81  # PolicyBlocked — --offline + unpinned + local miss


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_app(tmp_path: Path, name: str, deps: list[dict]) -> tuple[Path, Path]:
    """Write an app content dir + metadata sidecar with `deps`."""
    pkg_dir = tmp_path / f"app-{name}"
    (pkg_dir / "bin").mkdir(parents=True)
    (pkg_dir / "bin" / "app").write_text("#!/bin/sh\necho app\n")
    metadata = tmp_path / f"app-{name}-metadata.json"
    metadata.write_text(
        json.dumps({"type": "bundle", "version": 1, "dependencies": deps})
    )
    return pkg_dir, metadata


def _create(
    ocx: OcxRunner,
    pkg_dir: Path,
    metadata: Path,
    out: Path,
    *args: str,
    root_flags: tuple[str, ...] = (),
    check: bool = True,
):
    return ocx.plain(
        *root_flags,
        "package",
        "create",
        "-m",
        str(metadata),
        "-o",
        str(out),
        *args,
        str(pkg_dir),
        check=check,
    )


def _sidecar(out: Path) -> dict:
    """Read the rewritten metadata sidecar next to the output bundle."""
    sidecar = out.parent / (out.name.replace(".tar.xz", "") + "-metadata.json")
    assert sidecar.exists(), f"expected rewritten sidecar at {sidecar}"
    return json.loads(sidecar.read_text())


def _child_manifest_digest(ocx: OcxRunner, repo: str, tag: str) -> str:
    """The single platform manifest digest inside the tag's image index."""
    index = fetch_manifest_from_registry(ocx.registry, repo, tag)
    manifests = index["manifests"]
    assert len(manifests) == 1, f"expected single-child index, got {manifests}"
    return manifests[0]["digest"]


# ---------------------------------------------------------------------------
# Tag-only deps resolve to manifest digests
# ---------------------------------------------------------------------------


def test_create_pins_tag_to_manifest_digest(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A tag-only dep is pinned to the platform MANIFEST digest — never the
    image index digest (which registry GC collects on the next push)."""
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    pkg_dir, metadata = _write_app(tmp_path, "pin", [{"identifier": leaf.fq}])
    out = tmp_path / "app-pin.tar.xz"

    _create(ocx, pkg_dir, metadata, out, "-p", current_platform())

    sidecar = _sidecar(out)
    dep = sidecar["dependencies"][0]
    identifier = dep["identifier"]
    assert "@sha256:" in identifier, f"dep must be digest-pinned: {identifier}"

    pinned_digest = identifier.split("@", 1)[1]
    index_digest = fetch_manifest_digest(ocx.registry, leaf.repo, leaf.tag)
    manifest_digest = _child_manifest_digest(ocx, leaf.repo, leaf.tag)
    assert pinned_digest == manifest_digest, "must pin the platform manifest digest"
    assert pinned_digest != index_digest, "must NOT pin the image index digest"

    # The declared platform is embedded as the package target set.
    assert sidecar["platforms"] == [current_platform()]


def test_create_no_compatible_platform_lists_available(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A dependency published only for a libc-tagged leaf cannot satisfy a
    bare platform request — NoCompatiblePlatform lists the available leaf so
    the caller knows which libc feature to add (fail-closed: a plain platform
    cannot run a glibc-only leaf)."""
    leaf = make_package(
        ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path, platform="linux/amd64+libc.glibc"
    )
    pkg_dir, metadata = _write_app(tmp_path, "nocompat", [{"identifier": leaf.fq}])
    out = tmp_path / "app-nocompat.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "-p", "linux/amd64", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "available:" in result.stderr, result.stderr
    assert "linux/amd64+libc.glibc" in result.stderr, result.stderr


def test_create_ambiguous_platform_lists_candidates(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A dependency published for both libc families is ambiguous when the
    declared platform itself advertises both — AmbiguousPlatform lists every
    compatible candidate rather than picking one arbitrarily."""
    dep_repo = f"{unique_repo}_leaf"
    make_package(
        ocx, dep_repo, "1.0.0", tmp_path, platform="linux/amd64+libc.glibc", cascade=False
    )
    leaf = make_package(
        ocx, dep_repo, "1.0.0", tmp_path.joinpath("second"),
        platform="linux/amd64+libc.musl", cascade=False, new=False,
    )
    pkg_dir, metadata = _write_app(tmp_path, "ambiguous", [{"identifier": leaf.fq}])
    out = tmp_path / "app-ambiguous.tar.xz"

    result = _create(
        ocx, pkg_dir, metadata, out, "-p", "linux/amd64+libc.glibc+libc.musl", check=False
    )

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "candidates:" in result.stderr, result.stderr
    assert "linux/amd64+libc.glibc" in result.stderr, result.stderr
    assert "linux/amd64+libc.musl" in result.stderr, result.stderr


def test_create_any_with_agnostic_dep_single_pin(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`-p any` + a platform-agnostic dep collapses to a plain single pin."""
    leaf = make_package(ocx, f"{unique_repo}_anyleaf", "1.0.0", tmp_path, platform="any")
    pkg_dir, metadata = _write_app(tmp_path, "anyany", [{"identifier": leaf.fq}])
    out = tmp_path / "app-anyany.tar.xz"

    _create(ocx, pkg_dir, metadata, out, "-p", "any")

    sidecar = _sidecar(out)
    dep = sidecar["dependencies"][0]
    assert "@sha256:" in dep["identifier"]
    assert "platforms" not in dep, "agnostic dep must not carry a pin map"
    assert sidecar["platforms"] == ["any"]


def test_create_any_with_specific_dep_builds_map(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`-p any` + a platform-specific dep produces a per-dep pin map and a
    derived (specific) target set."""
    plat = current_platform()
    leaf = make_package(ocx, f"{unique_repo}_spec", "1.0.0", tmp_path, platform=plat)
    pkg_dir, metadata = _write_app(tmp_path, "anyspec", [{"identifier": leaf.fq}])
    out = tmp_path / "app-anyspec.tar.xz"

    _create(ocx, pkg_dir, metadata, out, "-p", "any")

    sidecar = _sidecar(out)
    dep = sidecar["dependencies"][0]
    assert "@sha256:" not in dep["identifier"], "map-bearing dep keeps a digest-less identifier"
    manifest_digest = _child_manifest_digest(ocx, leaf.repo, leaf.tag)
    assert dep["platforms"] == {plat: manifest_digest}
    assert sidecar["platforms"] == [plat]


def test_create_any_platform_prepinned_multiplatform_map_intersects(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """H1 regression: a dependency ALREADY pinned as a multi-platform map (no
    ``any`` key, and no OTHER dependency to seed the coverage union via fresh
    resolution) must still contribute its own specific platforms to the
    package target-set intersection. Before the fix this raised
    ``EmptyPlatformIntersection`` (65) even though the map covers two
    platforms — a pass-through pinned map's own platforms never entered the
    intersection computation, which only tracked freshly-resolved deps."""
    dep_repo = f"{unique_repo}_dep"
    make_package(ocx, dep_repo, "1.0.0", tmp_path, platform="linux/amd64", cascade=False)
    make_package(
        ocx, dep_repo, "1.0.0", tmp_path.joinpath("second"),
        platform="darwin/arm64", cascade=False, new=False,
    )
    linux_digest = fetch_platform_manifest_digest(ocx.registry, dep_repo, "1.0.0", platform="linux/amd64")
    darwin_digest = fetch_platform_manifest_digest(ocx.registry, dep_repo, "1.0.0", platform="darwin/arm64")

    pkg_dir, metadata = _write_app(tmp_path, "h1map", [{
        "identifier": f"{ocx.registry}/{dep_repo}:1.0.0",
        "platforms": {"linux/amd64": linux_digest, "darwin/arm64": darwin_digest},
    }])
    out = tmp_path / "app-h1map.tar.xz"

    _create(ocx, pkg_dir, metadata, out, "-p", "any", root_flags=("--offline",))

    sidecar = _sidecar(out)
    assert set(sidecar["platforms"]) == {"linux/amd64", "darwin/arm64"}


def test_create_empty_intersection_fails(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Deps with disjoint platform coverage leave no publishable platform."""
    linux = make_package(ocx, f"{unique_repo}_linux", "1.0.0", tmp_path, platform="linux/amd64")
    mac = make_package(ocx, f"{unique_repo}_mac", "1.0.0", tmp_path, platform="darwin/arm64")
    pkg_dir, metadata = _write_app(
        tmp_path, "disjoint", [{"identifier": linux.fq}, {"identifier": mac.fq}]
    )
    out = tmp_path / "app-disjoint.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "-p", "any", check=False)
    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "no platform is covered by every dependency" in result.stderr, result.stderr


# ---------------------------------------------------------------------------
# Flag / policy interactions
# ---------------------------------------------------------------------------


def test_create_unpinned_without_platform_is_usage_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    pkg_dir, metadata = _write_app(tmp_path, "noplat", [{"identifier": leaf.fq}])
    out = tmp_path / "app-noplat.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, check=False)
    assert result.returncode == EXIT_USAGE, result.stderr
    assert "--platform" in result.stderr, "error must hint at --platform"


def test_create_prepinned_offline_succeeds(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Fully pinned metadata needs no network — `--offline` create passes and
    rewrites the sidecar canonically."""
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    manifest_digest = _child_manifest_digest(ocx, leaf.repo, leaf.tag)
    pkg_dir, metadata = _write_app(
        tmp_path, "offline", [{"identifier": f"{leaf.fq}@{manifest_digest}"}]
    )
    out = tmp_path / "app-offline.tar.xz"

    _create(ocx, pkg_dir, metadata, out, root_flags=("--offline",))

    dep = _sidecar(out)["dependencies"][0]
    assert dep["identifier"].endswith(f"@{manifest_digest}")


def test_create_concrete_platform_passes_through_pinned_dep(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """A dep pre-pinned to its manifest digest needs no index consultation
    when ``--platform`` names a CONCRETE platform — not just when
    ``--platform`` is omitted entirely (see the sibling offline test above).
    The identical digest survives the canonical sidecar rewrite untouched."""
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    manifest_digest = _child_manifest_digest(ocx, leaf.repo, leaf.tag)
    pkg_dir, metadata = _write_app(
        tmp_path, "concretepinned", [{"identifier": f"{leaf.fq}@{manifest_digest}"}]
    )
    out = tmp_path / "app-concretepinned.tar.xz"

    _create(
        ocx, pkg_dir, metadata, out, "-p", current_platform(),
        root_flags=("--offline",),
    )

    sidecar = _sidecar(out)
    dep = sidecar["dependencies"][0]
    assert dep["identifier"].endswith(f"@{manifest_digest}"), "pinned digest must survive untouched"
    assert sidecar["platforms"] == [current_platform()]


def test_create_offline_unpinned_is_policy_blocked(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`--offline` + unpinned dep + local index miss = policy block (81)."""
    # index=False keeps the dep out of the local index, so the offline
    # resolve is a genuine local miss.
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path, index=False)
    pkg_dir, metadata = _write_app(tmp_path, "offmiss", [{"identifier": leaf.fq}])
    out = tmp_path / "app-offmiss.tar.xz"

    result = _create(
        ocx, pkg_dir, metadata, out, "-p", current_platform(),
        root_flags=("--offline",), check=False,
    )
    assert result.returncode == EXIT_POLICY, result.stderr


def test_create_dep_tag_not_found(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    pkg_dir, metadata = _write_app(
        tmp_path, "missing", [{"identifier": f"{ocx.registry}/{unique_repo}_ghost:9.9.9"}]
    )
    out = tmp_path / "app-missing.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "-p", current_platform(), check=False)
    assert result.returncode == EXIT_NOT_FOUND, result.stderr
