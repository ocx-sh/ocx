"""Acceptance tests for the `ocx package push` dependency gate.

Push is a pure gate (adr_dependency_manifest_pinning.md): it rejects unpinned
or index-pinned dependencies (create is the compiler).

Per `adr_platform_model_unification.md` D5, a bundle targets exactly one
platform per `create`/`push` invocation — there is no bundle-level embedded
target *set* and no per-push multi-platform fan-out (the sidecar's `platforms`
field, and the fan-out it drove, are deleted). The single-platform `push`
CLI surface (default `--platform`, cascade-tag interaction) is WP-E's
implementation scope (`adr_platform_model_unification.md` "Resolved" note
#2) — this file keeps only the platform-set-independent dependency gate
coverage; the multi-platform narrowing/fan-out tests that exercised the
deleted coverage-intersection machinery are removed with it.
"""

from __future__ import annotations

import io
import json
import tarfile
from pathlib import Path

from src.helpers import make_package, resolved_metadata_path, resolved_receipt_path
from src.registry import (
    fetch_manifest_from_registry,
    fetch_platform_manifest_digest,
    index_platforms,
    push_raw_package,
)
from src.runner import OcxRunner, current_platform

EXIT_USAGE = 64
EXIT_DATA_ERR = 65
EXIT_NOT_FOUND = 79


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


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
    for `platform`; push infers that sidecar from the bundle."""
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


# ---------------------------------------------------------------------------
# Gate: rejections
# ---------------------------------------------------------------------------


def test_push_rejects_digestless_dep(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """The published metadata parser refuses a digest-less dependency, and the
    error points at `ocx package create`.

    This never reaches the dependency gate: published `Dependency.identifier`
    is a `PinnedIdentifier`, so a tag-only entry dies at deserialize. That is
    the contract under test — `create` is the only thing that resolves a tag
    to a digest, so push has nothing to fall back on.

    The gate itself is covered by `test_push_rejects_index_pinned_dep` and
    `test_push_rejects_missing_dep_manifest`."""
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
    assert leaf.fq in result.stderr, (
        "error must name the offending dependency — the confusable is the "
        "receipt-or-platform gate, which exits 64 naming only --platform, so "
        "the two assertions above alone would not tell the two apart"
    )


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
# Single-platform contract (D4/D5) — `--platform` defaults to `current()`,
# takes one value, no fan-out. Push publishes exactly the manifest for the
# platform it ran under; publishing more than one platform under a tag is
# multiple pushes (existing cascade/index-merge mechanics, unaffected by
# this ADR), not one push fanning out from an embedded set.
# ---------------------------------------------------------------------------


def test_push_repush_is_idempotent(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    bundle = _created_app(
        ocx, tmp_path, "idem", [{"identifier": leaf.fq}], current_platform(),
    )
    app_fq = f"{ocx.registry}/{unique_repo}_app:1.0.0"

    first = ocx.json("package", "push", "-n", "-i", app_fq, str(bundle))
    second = ocx.json("package", "push", "-n", "-i", app_fq, str(bundle))
    assert first["manifest_digest"] == second["manifest_digest"]


def test_push_concrete_platform_flag_round_trip(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`--platform` on push (not just the default) succeeds end-to-end for a
    concrete platform, matching what `ocx package create --platform` resolved."""
    plat = current_platform()
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    bundle = _created_app(ocx, tmp_path, "concrete", [{"identifier": leaf.fq}], plat)
    app_fq = f"{ocx.registry}/{unique_repo}_app:1.0.0"

    _push(ocx, app_fq, bundle, "-p", plat)


def test_push_defaults_to_created_platform(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`push` with no `--platform` publishes under the platform the build
    receipt `ocx package create` wrote beside the bundle, not the host
    platform — the two must stay bound even when the flag is never repeated
    on push."""
    plat = current_platform()
    bundle = _created_app(ocx, tmp_path, "defaultplat", [], plat)
    app_repo = f"{unique_repo}_app"
    app_fq = f"{ocx.registry}/{app_repo}:1.0.0"

    _push(ocx, app_fq, bundle)

    manifest = fetch_manifest_from_registry(ocx.registry, app_repo, "1.0.0")
    assert plat in index_platforms(manifest), (
        f"published index must carry the create-recorded platform {plat!r}, got {manifest}"
    )


def test_push_explicit_platform_overrides_receipt_with_warning(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """An explicit `--platform` that disagrees with the platform the build
    receipt recorded is ACCEPTED — explicit wins — but push prints a warning
    on stderr naming both platform strings."""
    bundle = _created_app(ocx, tmp_path, "mismatch", [], "linux/amd64")
    app_fq = f"{ocx.registry}/{unique_repo}_app:1.0.0"

    result = _push(ocx, app_fq, bundle, "-p", "darwin/arm64", check=False)
    assert result.returncode == 0, result.stderr
    assert "linux/amd64" in result.stderr, result.stderr
    assert "darwin/arm64" in result.stderr, result.stderr


def test_push_any_target_with_any_offered_dep_succeeds(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """D5: an `any`-targeted bundle depending only on `any`-offered
    dependencies creates and pushes successfully end to end; the recorded
    `any` platform round-trips to the published index entry."""
    leaf = make_package(ocx, f"{unique_repo}_anyleaf", "1.0.0", tmp_path, platform="any")
    bundle = _created_app(ocx, tmp_path, "anyok", [{"identifier": leaf.fq}], "any")
    app_repo = f"{unique_repo}_app"
    app_fq = f"{ocx.registry}/{app_repo}:1.0.0"

    _push(ocx, app_fq, bundle, "-p", "any")

    manifest = fetch_manifest_from_registry(ocx.registry, app_repo, "1.0.0")
    assert "any/any" in index_platforms(manifest), (
        f"an any-targeted push must publish the OCI any/any platform entry, got {manifest}"
    )


def test_push_any_target_rejects_forged_any_pin(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """D5 any-provenance check — the single any-provenance guard now that the
    structural digest-pin prohibition is gone (a bare `@digest` under an
    `any` target is legitimate iff the dependency's own image index
    advertises that digest as `any`): a leaf published under a concrete
    platform only pins its digest bare on the identifier, no `platforms` map
    involved. Push must fetch the dependency's own image index and reject
    the pin as not `any`-advertised, not just check the leaf exists."""
    leaf = make_package(ocx, f"{unique_repo}_concreteleaf", "1.0.0", tmp_path)
    leaf_manifest_digest = fetch_platform_manifest_digest(ocx.registry, leaf.repo, leaf.tag)
    bundle = _bundle(ocx, tmp_path, "forgedany")
    metadata = _write_metadata(tmp_path, "forgedany", {
        "type": "bundle", "version": 1,
        "dependencies": [{"identifier": f"{leaf.fq}@{leaf_manifest_digest}"}],
    })

    result = _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), "-p", "any", check=False,
    )
    assert result.returncode == EXIT_DATA_ERR, result.stderr
    assert leaf.repo in result.stderr, result.stderr
    assert "direct digest pin" not in result.stderr, (
        "a bare digest under `any` is a legitimate pin shape now (the "
        "structural DirectDigestPinInAnyTarget check is deleted) — this must "
        "be the any-provenance rejection (AnyPinNotAdvertisedAsAny), not the "
        "old structural one"
    )


def test_push_repeated_platform_flag_rejected(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """Passing `--platform` twice is a clap usage error (64), not a
    fan-out request — push targets exactly one platform per invocation."""
    leaf = make_package(ocx, f"{unique_repo}_leaf", "1.0.0", tmp_path)
    bundle = _bundle(ocx, tmp_path, "repeatedplat")
    manifest_digest = fetch_platform_manifest_digest(ocx.registry, leaf.repo, leaf.tag)
    metadata = _write_metadata(tmp_path, "repeatedplat", {
        "type": "bundle", "version": 1,
        "dependencies": [{"identifier": f"{leaf.fq}@{manifest_digest}"}],
    })

    result = _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), "-p", "linux/amd64", "-p", "darwin/arm64", check=False,
    )
    assert result.returncode == EXIT_USAGE, result.stderr


# ---------------------------------------------------------------------------
# Build receipt resolution (4-row table: receipt x explicit --platform)
# ---------------------------------------------------------------------------


def test_create_writes_receipt_beside_bundle(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """`ocx package create -m -p` writes a build receipt sidecar beside the
    bundle recording the resolved platform, and the metadata sidecar itself
    carries no `platform` key (published wire shape)."""
    plat = current_platform()
    bundle = _created_app(ocx, tmp_path, "receipt", [], plat)

    receipt_path = resolved_receipt_path(bundle)
    assert receipt_path.exists(), f"expected build receipt at {receipt_path}"
    receipt = json.loads(receipt_path.read_text())
    assert receipt == {"version": 1, "platform": plat}

    sidecar = json.loads(resolved_metadata_path(bundle).read_text())
    assert "platform" not in sidecar, sidecar


def test_push_without_receipt_or_platform_is_usage_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """No build receipt beside the bundle and no explicit `--platform`: push
    has nothing that determines the OCI platform slot — usage error (64),
    naming `--platform`."""
    bundle = _bundle(ocx, tmp_path, "noreceiptnoplat")
    metadata = _write_metadata(tmp_path, "noreceiptnoplat", {"type": "bundle", "version": 1})

    result = _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), check=False,
    )
    assert result.returncode == EXIT_USAGE, result.stderr
    assert "--platform" in result.stderr, result.stderr


def test_push_without_receipt_with_platform_proceeds_with_notice(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """No build receipt beside the bundle, but an explicit `--platform` is
    enough to proceed — push publishes, with an informational notice on
    stderr that the receipt-backed validation was skipped."""
    bundle = _bundle(ocx, tmp_path, "noreceiptplat")
    metadata = _write_metadata(tmp_path, "noreceiptplat", {"type": "bundle", "version": 1})

    result = _push(
        ocx, f"{ocx.registry}/{unique_repo}_app:1.0.0", bundle,
        "-m", str(metadata), "-p", current_platform(), check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "receipt" in result.stderr.lower(), (
        f"expected a notice naming the skipped receipt-backed validation:\n{result.stderr}"
    )


def test_package_test_without_receipt_or_platform_is_usage_error(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
):
    """`ocx package test` shares push's receipt-or-platform contract: a
    metadata-only artifact (zero layers, so no bundle to anchor a receipt to)
    with no explicit `--platform` is a usage error (64), naming
    `--platform`."""
    metadata = _write_metadata(tmp_path, "packagetestnoreceipt", {"type": "bundle", "version": 1})

    result = ocx.run(
        "package", "test",
        "-m", str(metadata),
        "-i", f"{ocx.registry}/{unique_repo}_app:1.0.0",
        "--", "true",
        check=False,
    )
    assert result.returncode == EXIT_USAGE, result.stderr
    assert "--platform" in result.stderr, result.stderr


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
