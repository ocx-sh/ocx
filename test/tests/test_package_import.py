# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for `ocx package import` — installing from an export directory.

Every export here is produced by `ocx package export` itself; a test that needs a
damaged directory damages a real one. The importing home never reaches a registry:
its runner is routed into a refusal proxy (`registry_guard`), and a test that claims
"no network" asserts both that the import succeeded and that the proxy saw nothing.
"""

from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
from pathlib import Path
from typing import Any

import pytest

from src import (
    OcxRunner,
    assert_symlink_exists,
    current_platform,
    make_package,
    registry_dir,
)
from src.export_dir import (
    MARKER,
    blob_path,
    corrupt_index_root,
    delete_blob,
    flip_byte,
    read_marker,
    truncate_blob,
)
from src.registry import (
    fetch_manifest_digest,
    fetch_manifest_raw,
    fetch_platform_manifest_digest,
    push_referrer,
)
from src.registry_guard import RegistryGuard

pytestmark = pytest.mark.command(
    "package_import", "package_export", "install", "exec", "clean", "index_list"
)

_NO_BLOBS = ("index", "packages", "symlinks")
_TAG_STATUSES = {"added", "unchanged", "replaced", "conflict"}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _export(
    ocx: OcxRunner, out: Path, *identifiers: str, platforms: tuple[str, ...] = ()
) -> dict[str, Any]:
    args = ["package", "export", "-o", str(out)]
    for platform in platforms:
        args += ["-p", platform]
    return ocx.json(*args, *identifiers)


def _import(
    runner: OcxRunner,
    source: Path,
    *names: str,
    platforms: tuple[str, ...] = (),
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    args = ["package", "import"]
    for platform in platforms:
        args += ["-p", platform]
    return runner.run(*args, str(source), *names, check=check)


def _imported(result: subprocess.CompletedProcess[str]) -> list[dict[str, Any]]:
    assert result.returncode == 0, (
        f"import failed (rc={result.returncode})\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    return json.loads(result.stdout)["items"]


def _tag_rows(items: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    rows = {tag["tag"]: tag for item in items for tag in item.get("tags") or []}
    assert rows, f"the report lists no tag: {items}"
    return rows


def _platform_name(platform: Any) -> str:
    if isinstance(platform, str):
        return platform
    parts = [platform["os"], platform["architecture"]]
    if platform.get("variant"):
        parts.append(platform["variant"])
    return "/".join(parts)


def _platform_rows(items: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    return {
        _platform_name(row["platform"]): row
        for item in items
        for row in item["manifests"]
    }


def _leaf_digests(items: list[dict[str, Any]]) -> set[str]:
    """Every leaf digest the report lists, across packages and across tags of one platform."""
    return {row["digest"] for item in items for row in item["manifests"]}


def _error(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    return json.loads(result.stdout)["error"]


def _text(result: subprocess.CompletedProcess[str]) -> str:
    return result.stdout + result.stderr


def _home_tags(runner: OcxRunner, repo: str) -> set[str]:
    """The tags the home's own index pins for `repo`; empty when it knows no such package."""
    result = runner.run("--offline", "index", "list", repo, check=False)
    if result.returncode != 0:
        return set()
    return {
        tag
        for item in json.loads(result.stdout)["items"]
        for tag in item.get("tags") or []
    }


def _home_files(home: Path, tier: str) -> list[Path]:
    return sorted(path for path in (home / tier).rglob("*") if path.is_file())


def _home_state(
    home: Path, tiers: tuple[str, ...] = ("index", "blobs", "packages", "symlinks")
) -> list[str]:
    """Every file and link of the given tiers, so a refusal can prove it wrote nothing."""
    return sorted(
        str(path.relative_to(home))
        for tier in tiers
        for path in (home / tier).rglob("*")
        if not path.is_dir() and "locks" not in path.parts
    )


def _refused(
    second_home: OcxRunner,
    guard: RegistryGuard,
    source: Path,
    detail: str = "export_dir_invalid",
) -> subprocess.CompletedProcess[str]:
    """Import `source` into a guarded home, expect a 65 refusal, and assert the home gained nothing."""
    _guarded(second_home, guard)
    before = _home_state(second_home.ocx_home)
    result = _import(second_home, source, check=False)
    assert result.returncode == 65, _text(result)
    assert _error(result)["detail"] == detail
    assert _home_state(second_home.ocx_home) == before, (
        "a refused import left pins, blobs or packages behind"
    )
    assert guard.count == 0, guard.requests
    return result


def _other_platform() -> str:
    return "linux/arm64" if current_platform() != "linux/arm64" else "linux/amd64"


def _layer_digest(
    registry: str, repo: str, tag: str, platform: str | None = None
) -> str:
    """The digest of the one layer archive of `repo:tag`, read from its platform manifest."""
    leaf = fetch_platform_manifest_digest(
        registry, repo, tag, platform=platform or current_platform()
    )
    manifest = json.loads(fetch_manifest_raw(registry, repo, leaf)[0])
    layers = manifest["layers"]
    assert len(layers) == 1, f"the fixture publishes one layer, got {layers}"
    return layers[0]["digest"]


def _guarded(runner: OcxRunner, guard: RegistryGuard) -> OcxRunner:
    return guard.deny(runner)


def _publish_two_platforms(
    ocx: OcxRunner, repo: str, tag: str, tmp_path: Path
) -> tuple[str, str]:
    host, other = current_platform(), _other_platform()
    make_package(ocx, repo, tag, tmp_path / "host", platform=host)
    make_package(ocx, repo, tag, tmp_path / "other", platform=other)
    return host, other


def _mkdirs(tmp_path: Path, *names: str) -> None:
    for name in names:
        (tmp_path / name).mkdir()


# ---------------------------------------------------------------------------
# Pass A — pins and platforms
# ---------------------------------------------------------------------------


def test_import_then_offline_install_by_tag(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """Export, import into an empty home with the registry denied, then install by tag."""
    _mkdirs(tmp_path, "src")
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path / "src")
    out = tmp_path / "export"
    _export(ocx, out, pkg.fq)

    _guarded(second_home, registry_guard)
    _imported(_import(second_home, out))
    second_home.json("--offline", "package", "install", pkg.short)

    candidate = (
        second_home.ocx_home
        / "symlinks"
        / registry_dir(registry)
        / unique_repo
        / "candidates"
        / pkg.tag
    )
    assert_symlink_exists(candidate)
    assert registry_guard.count == 0, (
        f"the importing home dialed the registry: {registry_guard.requests}"
    )


def test_import_one_name_pins_only_it(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """`import DIR a` pins `a`'s tag and nothing of `b`."""
    _mkdirs(tmp_path, "a", "b")
    repo_a, repo_b = f"{unique_repo}_a", f"{unique_repo}_b"
    pkg_a = make_package(ocx, repo_a, "1.0.0", tmp_path / "a")
    pkg_b = make_package(ocx, repo_b, "1.0.0", tmp_path / "b")
    out = tmp_path / "export"
    _export(ocx, out, pkg_a.fq, pkg_b.fq)

    _guarded(second_home, registry_guard)
    items = _imported(_import(second_home, out, repo_a))

    assert [item["identifier"].rsplit("/", 1)[-1] for item in items] == [repo_a]
    assert _home_tags(second_home, repo_a) == {"1.0.0"}
    assert _home_tags(second_home, repo_b) == set()
    assert registry_guard.count == 0


def test_import_roots_nothing(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """An import holds nothing live: `ocx clean` removes the packages and their metadata blobs, the pins stay."""
    _mkdirs(tmp_path, "src")
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path / "src")
    out = tmp_path / "export"
    _export(ocx, out, pkg.fq)

    home = second_home.ocx_home
    _guarded(second_home, registry_guard)
    _imported(_import(second_home, out))
    assert not (home / "symlinks").exists() or not any(
        (home / "symlinks").rglob("candidates")
    ), "an import creates no candidate"
    assert _home_files(home, "packages"), (
        "precondition: the import materialized the package"
    )
    assert [p for p in _home_files(home, "blobs") if p.name == "data"], (
        "precondition: the import wrote metadata blobs"
    )

    second_home.plain("clean")

    assert _home_files(home, "packages") == []
    assert [p for p in _home_files(home, "blobs") if p.name == "data"] == []
    assert _home_tags(second_home, unique_repo) == {"1.0.0"}, (
        "clean never touches the index pins"
    )


def test_import_no_names_pins_every_tag(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """With no names, every tag the export lists is pinned, and only those."""
    _mkdirs(tmp_path, "one", "two")
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "one")
    make_package(ocx, unique_repo, "2.0.0", tmp_path / "two")
    out = tmp_path / "export"
    fq = f"{ocx.registry}/{unique_repo}"
    _export(ocx, out, f"{fq}:1.0.0", f"{fq}:2.0.0")

    _guarded(second_home, registry_guard)
    rows = _tag_rows(_imported(_import(second_home, out)))

    assert set(rows) == {"1.0.0", "2.0.0"}
    assert {row["status"] for row in rows.values()} == {"added"}
    assert _home_tags(second_home, unique_repo) == {"1.0.0", "2.0.0"}, (
        "neither `latest` nor a cascade tag is pinned"
    )
    assert registry_guard.count == 0


def test_import_platform_present_and_missing(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A platform the export holds imports; the host default it lacks fails naming it, and a later offline install names the leaf."""
    host, other = _publish_two_platforms(ocx, unique_repo, "1.0.0", tmp_path)
    pkg_short = f"{unique_repo}:1.0.0"
    out = tmp_path / "export"
    _export(ocx, out, f"{registry}/{pkg_short}", platforms=(other,))

    _guarded(second_home, registry_guard)
    items = _imported(_import(second_home, out, platforms=(other,)))
    assert set(_platform_rows(items)) == {other}

    missing = _import(second_home, out, check=False)
    assert missing.returncode == 79, _text(missing)
    assert _error(missing)["detail"] == "export_not_found"
    assert host in _text(missing), "the error names the platform the export lacks"

    host_leaf = fetch_platform_manifest_digest(
        registry, unique_repo, "1.0.0", platform=host
    )
    install = second_home.run(
        "--offline", "package", "install", "-p", host, pkg_short, check=False
    )
    assert install.returncode == 81, _text(install)
    assert _error(install)["detail"] == "offline_manifest_missing"
    assert host_leaf in _text(install), (
        "the offline error names the leaf manifest it could not read"
    )
    assert registry_guard.count == 0


def test_import_conflict_and_named_replace(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A differing pin is kept (and reported) without names; naming the package moves it."""
    _mkdirs(tmp_path, "v1", "v2", "other")
    fq = f"{registry}/{unique_repo}"
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "v1", bins=["hello"])
    old_digest = fetch_manifest_digest(registry, unique_repo, "1.0.0")
    second_home.plain("index", "update", f"{unique_repo}:1.0.0")

    make_package(ocx, unique_repo, "1.0.0", tmp_path / "v2", bins=["other"])
    make_package(ocx, unique_repo, "2.0.0", tmp_path / "other")
    new_digest = fetch_manifest_digest(registry, unique_repo, "1.0.0")
    new_leaf = fetch_platform_manifest_digest(
        registry, unique_repo, "1.0.0", platform=current_platform()
    )
    assert new_digest != old_digest, "precondition: the tag moved"
    out = tmp_path / "export"
    _export(ocx, out, f"{fq}:1.0.0", f"{fq}:2.0.0")

    _guarded(second_home, registry_guard)
    kept = _imported(_import(second_home, out))
    rows = _tag_rows(kept)
    assert rows["1.0.0"]["status"] == "conflict"
    assert rows["1.0.0"]["digest"] == new_digest
    assert rows["1.0.0"]["home_digest"] == old_digest
    assert rows["2.0.0"]["status"] == "added"
    assert new_leaf not in _leaf_digests(kept), (
        "a conflict materializes nothing"
    )

    moved = _imported(_import(second_home, out, unique_repo))
    rows = _tag_rows(moved)
    assert rows["1.0.0"]["status"] == "replaced"
    assert rows["1.0.0"]["home_digest"] == old_digest
    assert rows["1.0.0"]["digest"] == new_digest
    assert new_leaf in _leaf_digests(moved), (
        "the moved tag materializes"
    )
    assert registry_guard.count == 0


def test_first_sight_import_uses_identity_pointer(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A new root's `repository` is the identity pointer, whatever pointer the export's root carries."""
    _mkdirs(tmp_path, "src")
    repo = f"team/{unique_repo}"
    pkg = make_package(ocx, repo, "1.0.0", tmp_path / "src")
    out = tmp_path / "export"
    _export(ocx, out, pkg.fq)

    export_root = out / "index" / registry_dir(registry) / "p" / f"{repo}.json"
    document = json.loads(export_root.read_text(encoding="utf-8"))
    hostile = "oci://elsewhere.invalid/not/the/package"
    assert document["repository"] != hostile
    document["repository"] = hostile
    edited = json.dumps(document).encode("utf-8")
    export_root.write_bytes(edited)
    # The export's catalog entry hashes the root, so a consistent plant re-hashes it too.
    catalog_path = export_root.parents[2] / "c" / "index.json"
    catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
    catalog["packages"][repo] = f"sha256:{hashlib.sha256(edited).hexdigest()}"
    catalog_path.write_text(json.dumps(catalog), encoding="utf-8")

    _guarded(second_home, registry_guard)
    _imported(_import(second_home, out))

    source_dir = second_home.ocx_home / "index" / registry_dir(registry)
    home_root = json.loads(
        (source_dir / "p" / f"{repo}.json").read_text(encoding="utf-8")
    )
    assert home_root["repository"] == f"oci://{registry}/{repo}"
    assert not (source_dir / "config.json").exists(), (
        "an unconfigured registry is written as a derived source"
    )
    assert registry_guard.count == 0


def test_reexport_adds_platform(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """Re-exporting into the same directory adds a platform and keeps the first one's blobs."""
    host, other = _publish_two_platforms(ocx, unique_repo, "1.0.0", tmp_path)
    fq = f"{registry}/{unique_repo}:1.0.0"
    out = tmp_path / "export"
    _export(ocx, out, fq, platforms=(host,))

    _guarded(second_home, registry_guard)
    first = _imported(_import(second_home, out, platforms=(host,)))
    assert set(_platform_rows(first)) == {host}

    _export(ocx, out, fq, platforms=(other,))
    second = _imported(_import(second_home, out, platforms=(host, other)))

    assert _tag_rows(second)["1.0.0"]["status"] == "unchanged"
    platforms = _platform_rows(second)
    assert platforms[other]["status"] == "materialized"
    assert platforms[host]["status"] == "present"
    assert registry_guard.count == 0


def test_import_digest_identifier_writes_no_pin(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A digest identifier materializes content and leaves the home index alone."""
    _mkdirs(tmp_path, "src")
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path / "src")
    digest = fetch_manifest_digest(registry, unique_repo, pkg.tag)
    identifier = f"{registry}/{unique_repo}@{digest}"
    host = current_platform()
    out = tmp_path / "export"
    _export(ocx, out, identifier, platforms=(host,))

    home = second_home.ocx_home
    _guarded(second_home, registry_guard)
    items = _imported(_import(second_home, out, identifier, platforms=(host,)))

    assert all("tags" not in item for item in items), (
        f"a digest identifier reports no tags: {items}"
    )
    assert _platform_rows(items)[host]["status"] == "materialized"
    assert _home_files(home, "packages"), "the content is in the home"
    assert _home_tags(second_home, unique_repo) == set(), "no pin was written"
    assert not (
        home / "index" / registry_dir(registry) / "p" / f"{unique_repo}.json"
    ).exists()
    assert registry_guard.count == 0


def test_import_report_fields(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """The import report carries the documented fields and every status word, `home_digest` only where defined."""
    _mkdirs(tmp_path, "v1", "v2")
    fq = f"{registry}/{unique_repo}"
    make_package(ocx, unique_repo, "1.0.0", tmp_path / "v1")
    first_dir = tmp_path / "export-first"
    _export(ocx, first_dir, f"{fq}:1.0.0")

    _guarded(second_home, registry_guard)
    fresh = _import(second_home, first_dir)
    document = json.loads(fresh.stdout)
    assert document["schema_version"] == 1
    assert set(document) == {"schema_version", "items"}
    (item,) = document["items"]
    assert item["identifier"] == fq
    (tag,) = item["tags"]
    assert set(tag) == {"tag", "digest", "status"}, (
        "an added tag carries no `home_digest`"
    )
    assert tag["status"] == "added"
    assert tag["digest"].startswith("sha256:")
    (platform,) = item["manifests"]
    assert set(platform) == {"platform", "digest", "status"}
    assert platform["status"] == "materialized"

    again = _tag_rows(_imported(_import(second_home, first_dir)))
    assert again["1.0.0"]["status"] == "unchanged"
    assert "home_digest" not in again["1.0.0"]
    assert {
        r["status"]
        for r in _platform_rows(_imported(_import(second_home, first_dir))).values()
    } == {"present"}

    make_package(ocx, unique_repo, "1.0.0", tmp_path / "v2", bins=["other"])
    moved_dir = tmp_path / "export-moved"
    _export(ocx, moved_dir, f"{fq}:1.0.0")
    conflict = _tag_rows(_imported(_import(second_home, moved_dir)))["1.0.0"]
    assert conflict["status"] == "conflict"
    assert (
        conflict["home_digest"].startswith("sha256:")
        and conflict["home_digest"] != conflict["digest"]
    )
    replaced = _tag_rows(_imported(_import(second_home, moved_dir, unique_repo)))[
        "1.0.0"
    ]
    assert replaced["status"] == "replaced"
    assert replaced["home_digest"] == conflict["home_digest"]
    assert {
        t
        for t in (
            tag["status"],
            again["1.0.0"]["status"],
            conflict["status"],
            replaced["status"],
        )
    } <= _TAG_STATUSES


# ---------------------------------------------------------------------------
# Pass B — integrity and errors
# ---------------------------------------------------------------------------


def _exported_layer(
    ocx: OcxRunner,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
    repo: str | None = None,
) -> tuple[Path, str]:
    """Publish one package, export it, and return the export directory and its layer digest."""
    _mkdirs(tmp_path, "src")
    name = repo or unique_repo
    pkg = make_package(ocx, name, "1.0.0", tmp_path / "src")
    out = tmp_path / "export"
    _export(ocx, out, pkg.fq)
    return out, _layer_digest(registry, name, "1.0.0")


def test_import_refuses_changed_byte(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """One flipped byte in a layer archive is refused as a digest mismatch, never fetched around."""
    out, layer = _exported_layer(ocx, unique_repo, registry, tmp_path)
    layer_file = blob_path(out, registry_dir(registry), layer)
    assert layer_file.is_file(), "precondition: the export holds the layer archive"
    flip_byte(layer_file)

    _guarded(second_home, registry_guard)
    before = _home_state(second_home.ocx_home, _NO_BLOBS)
    result = _import(second_home, out, check=False)

    assert result.returncode == 65, _text(result)
    assert _error(result)["detail"] == "digest_mismatch"
    assert _home_state(second_home.ocx_home, _NO_BLOBS) == before, (
        "a refused import left a pin or a package behind"
    )
    assert registry_guard.count == 0


def test_import_then_exec_without_export(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """An imported package runs after the export is gone, and its layer archive was never kept in the home."""
    _mkdirs(tmp_path, "src")
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path / "src")
    layer = _layer_digest(registry, unique_repo, "1.0.0")
    out = tmp_path / "export"
    _export(ocx, out, pkg.fq)

    _guarded(second_home, registry_guard)
    _imported(_import(second_home, out))
    shutil.rmtree(out)

    run = second_home.plain("package", "exec", pkg.short, "--", "hello")
    assert pkg.marker in run.stdout

    kept = {p.parent.name for p in (second_home.ocx_home / "blobs").rglob("data")}
    assert layer.split(":", 1)[1][2:32] not in kept, (
        "the layer archive is not in the home's blob store"
    )
    assert registry_guard.count == 0


def test_import_without_marker_or_index_is_invalid(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A directory with no marker, or a marker but no `index/`, is `export_dir_invalid`."""
    bare = tmp_path / "bare"
    bare.mkdir()
    _guarded(second_home, registry_guard)
    no_marker = _import(second_home, bare, check=False)
    assert no_marker.returncode == 65, _text(no_marker)
    assert _error(no_marker)["detail"] == "export_dir_invalid"

    out, _ = _exported_layer(ocx, unique_repo, registry, tmp_path)
    shutil.rmtree(out / "index")
    no_index = _import(second_home, out, check=False)
    assert no_index.returncode == 65, _text(no_index)
    assert _error(no_index)["detail"] == "export_dir_invalid"
    assert "ocx import" in _text(no_index), "the error points at the root-level import"
    assert registry_guard.count == 0


def test_import_truncated_layer_is_65(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A short layer archive is corruption (65), not a retryable read failure (75)."""
    out, layer = _exported_layer(ocx, unique_repo, registry, tmp_path)
    layer_file = blob_path(out, registry_dir(registry), layer)
    assert layer_file.is_file(), "precondition: the export holds the layer archive"
    truncate_blob(layer_file)

    _guarded(second_home, registry_guard)
    before = _home_state(second_home.ocx_home, _NO_BLOBS)
    result = _import(second_home, out, check=False)

    assert result.returncode == 65, _text(result)
    assert _home_state(second_home.ocx_home, _NO_BLOBS) == before, (
        "a refused import left a pin or a package behind"
    )
    assert registry_guard.count == 0


def test_import_corrupt_root_is_invalid(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A package root that is not JSON fails as `export_dir_invalid` and says to re-run the export."""
    out, _ = _exported_layer(
        ocx, unique_repo, registry, tmp_path, repo=f"team/{unique_repo}"
    )
    corrupt_index_root(out)

    result = _refused(second_home, registry_guard, out)

    assert "re-run export" in _text(result).lower()


def test_import_missing_layer_is_export_not_found(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A layer archive absent from the export is `export_not_found`, naming the blob and the package."""
    out, layer = _exported_layer(ocx, unique_repo, registry, tmp_path)
    layer_file = blob_path(out, registry_dir(registry), layer)
    assert layer_file.is_file(), "precondition: the export holds the layer archive"
    delete_blob(layer_file)

    _guarded(second_home, registry_guard)
    before = _home_state(second_home.ocx_home, _NO_BLOBS)
    result = _import(second_home, out, check=False)

    assert result.returncode == 79, _text(result)
    assert _home_state(second_home.ocx_home, _NO_BLOBS) == before, (
        "a refused import left a pin or a package behind"
    )
    assert _error(result)["detail"] == "export_not_found"
    assert layer.split(":", 1)[1][:12] in _text(result), (
        "the error names the missing blob"
    )
    assert unique_repo in _text(result), "the error names the package"
    assert registry_guard.count == 0


# ---------------------------------------------------------------------------
# Hostile exports — an export directory is untrusted input
# ---------------------------------------------------------------------------


def test_import_refuses_symlinked_index_dir(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """An `index/<dir>` that is a link out of the export is refused, not followed."""
    out, _ = _exported_layer(ocx, unique_repo, registry, tmp_path)
    outside = tmp_path / "outside"
    outside.mkdir()
    slug = registry_dir(registry)
    (out / "index" / slug).rename(outside / slug)
    (out / "index" / slug).symlink_to(outside / slug, target_is_directory=True)

    result = _refused(second_home, registry_guard, out)

    assert "link" in _text(result), "the error says why the entry is refused"


def test_import_refuses_traversal_registry_name(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A marker naming the registry `..` (an index entry that climbs out of `index/`) is refused."""
    out, _ = _exported_layer(ocx, unique_repo, registry, tmp_path)
    marker = read_marker(out)
    marker["registries"][".."] = ".."
    (out / MARKER).write_text(json.dumps(marker), encoding="utf-8")

    _refused(second_home, registry_guard, out, "repository_escapes_index_home")


def test_import_refuses_marker_key_that_is_not_the_slug(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """A marker whose key is not the slug of its registry is refused before any index is read."""
    out, _ = _exported_layer(ocx, unique_repo, registry, tmp_path)
    marker = read_marker(out)
    marker["registries"] = {"not-the-slug": registry}
    (out / MARKER).write_text(json.dumps(marker), encoding="utf-8")

    result = _refused(second_home, registry_guard, out)

    assert "slug" in _text(result)


def test_import_junk_referrer_markers_do_not_hide_a_real_one(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: RegistryGuard,
    unique_repo: str,
    registry: str,
    tmp_path: Path,
) -> None:
    """More than 256 junk referrer markers beside one real referrer: the import succeeds and the real one lands."""
    _mkdirs(tmp_path, "src")
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path / "src")
    leaf = fetch_platform_manifest_digest(registry, unique_repo, "1.0.0")
    body, _ = fetch_manifest_raw(registry, unique_repo, leaf)
    referrer, _ = push_referrer(
        registry,
        unique_repo,
        leaf,
        len(body),
        artifact_type="application/vnd.ocx.test.export+json",
    )
    out = tmp_path / "export"
    _export(ocx, out, pkg.fq)
    slug = registry_dir(registry)
    markers = blob_path(out, slug, leaf).parent / "referrers"
    assert (markers / referrer.replace(":", "-")).is_file(), (
        "precondition: the export marks the real referrer"
    )
    for index in range(300):
        (markers / f"sha256-{index:064x}").write_bytes(b"")

    _guarded(second_home, registry_guard)
    _imported(_import(second_home, out))

    assert blob_path(second_home.ocx_home / "blobs", slug, referrer).is_file(), (
        "the real referrer was pushed out by junk marker names"
    )
    assert registry_guard.count == 0
