"""Acceptance tests for `ocx package create` dependency pin resolution.

Create is the compiler of the create-resolves / push-gates split
(adr_dependency_manifest_pinning.md): tag-only dependencies in the metadata
sidecar are resolved to platform MANIFEST digests (never the GC-prone image
index digest) and the sidecar is rewritten canonically with the pins.

A bundle targets exactly one platform per `create` invocation
(adr_platform_model_unification.md D5) — there is no bundle-level
target-platform *set* in the sidecar. A concrete `--platform P` resolve pins
each dependency's identifier directly (`@digest`); an `--platform any`
resolve requires every dependency to offer an `any`-typed manifest and
records the pin as a single `"any"`-keyed entry in the dependency's
`platforms` map (never a bare digest — a leaf carries no platform
descriptor, so only a map key can record a verified `any`-ness).
"""

from __future__ import annotations

import json
from pathlib import Path

from src.helpers import make_package
from src.registry import fetch_manifest_digest, fetch_manifest_from_registry
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
        ocx, pkg_dir, metadata, out, "-p", "linux/amd64+libc.glibc,libc.musl", check=False
    )

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "candidates:" in result.stderr, result.stderr
    assert "linux/amd64+libc.glibc" in result.stderr, result.stderr
    assert "linux/amd64+libc.musl" in result.stderr, result.stderr


def test_create_any_with_agnostic_dep_pins_into_map(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`-p any` + a platform-agnostic dep records the pin as a single
    `"any"`-keyed map entry — never a bare digest (D5: a leaf carries no
    platform descriptor, so only a map key can record a verified `any`-ness)."""
    leaf = make_package(ocx, f"{unique_repo}_anyleaf", "1.0.0", tmp_path, platform="any")
    pkg_dir, metadata = _write_app(tmp_path, "anyany", [{"identifier": leaf.fq}])
    out = tmp_path / "app-anyany.tar.xz"

    _create(ocx, pkg_dir, metadata, out, "-p", "any")

    sidecar = _sidecar(out)
    dep = sidecar["dependencies"][0]
    assert "@sha256:" not in dep["identifier"], "an any-targeted pin must not be a bare digest"
    manifest_digest = _child_manifest_digest(ocx, leaf.repo, leaf.tag)
    assert dep["platforms"] == {"any": manifest_digest}


def test_create_any_with_specific_only_dep_fails(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """D5: a dependency offering only a Specific leaf (no `any` manifest)
    fails an `any`-targeted create — an any-targeted package performs no
    platform-specific resolution, so it cannot pick a platform-specific
    dependency leaf."""
    plat = current_platform()
    leaf = make_package(ocx, f"{unique_repo}_spec", "1.0.0", tmp_path, platform=plat)
    pkg_dir, metadata = _write_app(tmp_path, "anyspec", [{"identifier": leaf.fq}])
    out = tmp_path / "app-anyspec.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "-p", "any", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "has no leaf compatible with platform 'any'" in result.stderr, result.stderr


def test_create_any_with_direct_digest_pin_fails(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """D5: an `any`-targeted create rejects a dependency that already carries
    a direct digest pin — a leaf manifest carries no platform descriptor, so
    a bare `@digest` pin cannot be verified to be `any`-offered."""
    leaf = make_package(ocx, f"{unique_repo}_anyleaf", "1.0.0", tmp_path, platform="any")
    manifest_digest = _child_manifest_digest(ocx, leaf.repo, leaf.tag)
    pkg_dir, metadata = _write_app(
        tmp_path, "anydigest", [{"identifier": f"{leaf.fq}@{manifest_digest}"}]
    )
    out = tmp_path / "app-anydigest.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "-p", "any", check=False)

    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "direct digest pin" in result.stderr, result.stderr


def test_create_empty_intersection_fails(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """A dependency offering only Specific leaves (no shared `any` manifest)
    leaves no publishable platform for an `any`-targeted create."""
    linux = make_package(ocx, f"{unique_repo}_linux", "1.0.0", tmp_path, platform="linux/amd64")
    pkg_dir, metadata = _write_app(tmp_path, "disjoint", [{"identifier": linux.fq}])
    out = tmp_path / "app-disjoint.tar.xz"

    result = _create(ocx, pkg_dir, metadata, out, "-p", "any", check=False)
    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert "has no leaf compatible with platform 'any'" in result.stderr, result.stderr


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
