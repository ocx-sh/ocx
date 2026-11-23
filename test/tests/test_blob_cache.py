# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the blob cache and the blob-directory settings.

`--blob-cache-dir` keeps every verified blob a pull fetched from a registry;
`--blob-dir` is read-only and searched before the network. Each cache or blob
directory here is produced by a pull (a cache has the same layout as a blob
directory) unless the scenario needs an export, which only the export-backed
tests drive.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
from pathlib import Path
from typing import Any

import pytest

from src import registry_guard as registry_guard_mod
from src.export_dir import blob_data_files, flip_byte, run_together
from src.helpers import make_package
from src.registry import fetch_platform_manifest_digest
from src.runner import OcxRunner, PackageInfo

pytestmark = pytest.mark.command(
    "package_pull", "pull", "exec", "install", "package_export"
)


def _digest_ref(pkg: PackageInfo, registry: str) -> str:
    """The leaf platform manifest of `pkg` as a digest-pinned reference (no tag resolution)."""
    digest = fetch_platform_manifest_digest(registry, pkg.repo, pkg.tag)
    return f"{pkg.repo}@{digest}"


def _recorded_digest(data: Path) -> str:
    return (data.parent / "digest").read_text(encoding="utf-8").strip()


def _assert_every_blob_verifies(cache: Path) -> int:
    """Every `data` file hashes to the digest its sibling `digest` file names; returns how many."""
    files = blob_data_files(cache)
    assert files, f"{cache} holds no blob at all"
    for data in files:
        algorithm, hex_digest = _recorded_digest(data).split(":", 1)
        actual = hashlib.new(algorithm, data.read_bytes()).hexdigest()
        assert actual == hex_digest, f"{data} does not hash to its recorded digest"
    return len(files)


def _stray_files(cache: Path) -> list[Path]:
    """Files under `cache` that are neither a blob's `data` nor its `digest`."""
    return sorted(
        p
        for p in cache.rglob("*")
        if p.is_file() and p.name not in {"data", "digest", "ocx-export.json"}
    )


def _pull(runner: OcxRunner, *args: str) -> subprocess.CompletedProcess[str]:
    """Run `ocx --format json <args>` and hand back the result whatever its exit code."""
    return runner.run(*args, check=False)


def _package_root(result: subprocess.CompletedProcess[str], ref: str) -> Path:
    return Path(json.loads(result.stdout)["paths"][ref])


def _project(runner: OcxRunner, tmp_path: Path, pkg: PackageInfo) -> Path:
    """A locked project whose one tool is `pkg`."""
    project = tmp_path / "proj"
    project.mkdir()
    (project / "ocx.toml").write_text(
        f'[tools]\ntool = "{runner.registry}/{pkg.short}"\n', encoding="utf-8"
    )
    locked = subprocess.run(
        [str(runner.binary), "lock", "--no-pull"],
        cwd=project,
        capture_output=True,
        text=True,
        env=runner.env,
        check=False,
    )
    assert locked.returncode == 0, locked.stderr
    return project


def _run_in(
    runner: OcxRunner, cwd: Path, *args: str
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(runner.binary), *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=runner.env,
        check=False,
    )


def _homes(
    ocx_binary: Path, registry: str, tmp_path: Path, count: int
) -> list[OcxRunner]:
    homes = []
    for index in range(count):
        home = tmp_path / f"home{index}"
        home.mkdir()
        homes.append(OcxRunner(ocx_binary, home, registry))
    return homes


def test_concurrent_pulls_share_cache(
    ocx: OcxRunner,
    ocx_binary: Path,
    registry: str,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Eight processes fill one empty cache at once; none fails and none leaves a bad blob."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, layers=3, size_mb=3)
    cache = tmp_path / "cache"
    cache.mkdir()
    runners = _homes(ocx_binary, registry, tmp_path, 8)

    results = run_together(
        [
            lambda r=r: _pull(
                r, "--blob-cache-dir", str(cache), "package", "pull", pkg.short
            )
            for r in runners
        ]
    )

    failures = [r.stderr for r in results if r.returncode != 0]
    assert not failures, f"{len(failures)} of 8 concurrent pulls failed: {failures[0]}"
    _assert_every_blob_verifies(cache)
    assert _stray_files(cache) == [], (
        "the cache holds files that are neither data nor digest"
    )


def test_warm_cache_zero_registry_requests(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: registry_guard_mod.RegistryGuard,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """A lock-pinned pull from a warm cache succeeds on a fresh home without one registry request."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path, pkg)
    cache = tmp_path / "cache"

    warm = _run_in(ocx, project, "--blob-cache-dir", str(cache), "pull")
    assert warm.returncode == 0, warm.stderr
    # Manifest, config and layer: the cache holds metadata as well as layers.
    assert len(blob_data_files(cache)) >= 3

    registry_guard.deny(second_home)
    registry_guard.reset()
    cold_home = _run_in(second_home, project, "--blob-cache-dir", str(cache), "pull")

    assert cold_home.returncode == 0, cold_home.stderr
    assert registry_guard.count == 0, registry_guard.requests
    registry_guard.allow(second_home)
    present = second_home.run(
        "--offline",
        "package",
        "which",
        f"{ocx.registry}/{_digest_ref(pkg, ocx.registry)}",
        check=False,
    )
    assert present.returncode == 0, (
        "the pull reported success but the package is not in the home"
    )


def test_missing_blob_dir_flag_is_79_env_is_skipped(
    ocx: OcxRunner, published_package: PackageInfo, tmp_path: Path
) -> None:
    """A flag naming a missing directory refuses the command; the same path in the env is skipped with a warning."""
    pkg = published_package
    missing = tmp_path / "does-not-exist"

    flagged = _pull(ocx, "--blob-dir", str(missing), "package", "pull", pkg.short)
    assert flagged.returncode == 79, flagged.stderr
    assert json.loads(flagged.stdout)["error"]["detail"] == "blob_dir_not_found"

    via_env = ocx.run(
        "package",
        "pull",
        pkg.short,
        check=False,
        env_overlay={"OCX_BLOB_DIR": str(missing)},
    )
    assert via_env.returncode == 0, via_env.stderr
    assert str(missing) in via_env.stderr, "no warning names the skipped directory"
    assert _package_root(via_env, pkg.short).is_dir()


def test_cache_flipped_byte_is_refetched(
    ocx: OcxRunner,
    second_home: OcxRunner,
    published_package: PackageInfo,
    tmp_path: Path,
) -> None:
    """A cache entry that no longer matches its digest is warned about once, removed and fetched again."""
    pkg = published_package
    ref = _digest_ref(pkg, ocx.registry)
    cache = tmp_path / "cache"
    assert (
        _pull(ocx, "--blob-cache-dir", str(cache), "package", "pull", ref).returncode
        == 0
    )
    layer = max(blob_data_files(cache), key=lambda p: p.stat().st_size)
    damaged_digest = _recorded_digest(layer)
    flip_byte(layer)

    result = _pull(second_home, "--blob-cache-dir", str(cache), "package", "pull", ref)

    assert result.returncode == 0, result.stderr
    warnings = [
        line
        for line in result.stderr.splitlines()
        if "discarding cached blob" in line and damaged_digest in line
    ]
    assert len(warnings) == 1, result.stderr
    assert _package_root(result, ref).is_dir()
    _assert_every_blob_verifies(cache)


def test_blob_dir_hit_is_not_copied_to_cache(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: registry_guard_mod.RegistryGuard,
    published_package: PackageInfo,
    tmp_path: Path,
) -> None:
    """A blob answered by a blob directory is not written into the cache as well."""
    pkg = published_package
    ref = _digest_ref(pkg, ocx.registry)
    blob_dir = tmp_path / "dir"
    cache = tmp_path / "cache"
    assert (
        _pull(ocx, "--blob-cache-dir", str(blob_dir), "package", "pull", ref).returncode
        == 0
    )

    registry_guard.deny(second_home)
    registry_guard.reset()
    result = _pull(
        second_home,
        "--blob-dir",
        str(blob_dir),
        "--blob-cache-dir",
        str(cache),
        "package",
        "pull",
        ref,
    )

    assert result.returncode == 0, result.stderr
    assert registry_guard.count == 0, registry_guard.requests
    assert blob_data_files(cache) == [], (
        "a blob-directory hit was copied into the cache"
    )


def test_planted_cache_blob_records_no_origin(
    ocx: OcxRunner,
    ocx_binary: Path,
    second_home: OcxRunner,
    registry_guard: registry_guard_mod.RegistryGuard,
    published_package: PackageInfo,
    tmp_path: Path,
) -> None:
    """Content that came from the cache proves no registry served it, so no pull origin is recorded."""
    pkg = published_package
    ref = _digest_ref(pkg, ocx.registry)
    cache = tmp_path / "cache"

    # The publishing home already holds the manifest, so a pull there is a home hit; a fresh home pulls from the registry.
    cold = _pull(second_home, "--blob-cache-dir", str(cache), "package", "pull", ref)
    assert cold.returncode == 0, cold.stderr
    # Control: a registry pull does record where the bytes came from.
    assert list((_package_root(cold, ref) / "refs" / "origins").glob("*")), (
        "a registry pull recorded no origin, so the empty result below proves nothing"
    )

    (third_home,) = _homes(ocx_binary, ocx.registry, tmp_path, 1)
    registry_guard.deny(third_home)
    registry_guard.reset()
    warm = _pull(third_home, "--blob-cache-dir", str(cache), "package", "pull", ref)

    assert warm.returncode == 0, warm.stderr
    assert registry_guard.count == 0, registry_guard.requests
    origins = _package_root(warm, ref) / "refs" / "origins"
    assert not origins.exists() or list(origins.iterdir()) == []


def test_project_tier_blobs_section_is_stripped(
    ocx: OcxRunner,
    published_package: PackageInfo,
    tmp_path: Path,
) -> None:
    """A repository cannot redirect where a machine keeps blobs: a project `[blobs]` never reaches the cache."""
    pkg = published_package
    project = _project(ocx, tmp_path, pkg)
    redirected = tmp_path / "redirected"

    # Control: the same project pull does fill a cache named the legitimate way.
    legit = tmp_path / "legit"
    assert _run_in(ocx, project, "--blob-cache-dir", str(legit), "pull").returncode == 0
    assert blob_data_files(legit), (
        "control: --blob-cache-dir wrote nothing, so a stripped [blobs] proves nothing"
    )

    (project / "ocx.toml").write_text(
        f'[tools]\ntool = "{ocx.registry}/{pkg.short}"\n\n[blobs]\ncache_dir = "{redirected}"\n',
        encoding="utf-8",
    )
    result = _run_in(ocx, project, "pull")

    assert not redirected.exists(), "the project file's [blobs] cache_dir was honoured"
    assert "blobs" in result.stderr, (
        "the project's [blobs] section was neither warned about nor refused"
    )


def test_blob_dir_flag_reaches_child_ocx(
    ocx: OcxRunner,
    second_home: OcxRunner,
    ocx_binary: Path,
    published_package: PackageInfo,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """`--blob-dir` is inherited by an `ocx` started from inside `package exec`."""
    pkg = published_package
    (tmp_path / "other-src").mkdir()
    other = make_package(
        ocx, f"{unique_repo}_other", "1.0.0", tmp_path / "other-src", bins=["other"]
    )
    other_ref = _digest_ref(other, ocx.registry)
    blob_dir = tmp_path / "dir"
    assert (
        _pull(
            second_home, "--blob-cache-dir", str(blob_dir), "package", "pull", other_ref
        ).returncode
        == 0
    )
    assert _pull(ocx, "package", "pull", pkg.short).returncode == 0

    result = _pull(
        ocx,
        "--blob-dir",
        str(blob_dir),
        "package",
        "exec",
        pkg.short,
        "--",
        str(ocx_binary),
        "--offline",
        "package",
        "pull",
        other_ref,
    )

    assert result.returncode == 0, result.stderr
    stored = ocx.run("--offline", "package", "which", other_ref, check=False)
    assert stored.returncode == 0, (
        "the child reported success but never stored the package"
    )


def test_blob_dir_alone_never_resolves_a_tag(
    ocx: OcxRunner,
    second_home: OcxRunner,
    published_package: PackageInfo,
    tmp_path: Path,
) -> None:
    """A blob directory holds content by digest only; an offline home cannot turn a tag into one with it."""
    pkg = published_package
    export = tmp_path / "export"
    exported = ocx.run("package", "export", pkg.short, "-o", str(export), check=False)
    assert exported.returncode == 0, exported.stderr

    result = _pull(
        second_home,
        "--offline",
        "--blob-dir",
        str(export / "blobs"),
        "package",
        "install",
        pkg.short,
    )

    assert result.returncode == 81, result.stderr
    detail: Any = json.loads(result.stdout)["error"]["detail"]
    assert detail == "index_resolution_blocked"


def test_guard_counts_a_cold_lock_pinned_pull(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: registry_guard_mod.RegistryGuard,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Red control: with no warm cache the same pull needs the registry, so the guard counts and the pull fails."""
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path)
    project = _project(ocx, tmp_path, pkg)

    registry_guard.deny(second_home)
    registry_guard.reset()
    result = _run_in(second_home, project, "pull")

    assert result.returncode != 0, "a cold pull behind a denied registry succeeded"
    assert registry_guard.count > 0, "the guard saw no request, so a zero elsewhere proves nothing"


def test_guard_counts_an_install_an_import_cannot_serve(
    ocx: OcxRunner,
    second_home: OcxRunner,
    registry_guard: registry_guard_mod.RegistryGuard,
    unique_repo: str,
    tmp_path: Path,
) -> None:
    """Red control: after an import, a package the export never held needs the registry and the guard counts it."""
    (tmp_path / "a").mkdir()
    (tmp_path / "b").mkdir()
    held = make_package(ocx, f"{unique_repo}_a", "1.0.0", tmp_path / "a")
    absent = make_package(ocx, f"{unique_repo}_b", "1.0.0", tmp_path / "b")
    out = tmp_path / "export"
    ocx.json("package", "export", "-o", str(out), held.fq)

    registry_guard.deny(second_home)
    registry_guard.reset()
    imported = second_home.run("package", "import", str(out), check=False)
    assert imported.returncode == 0, imported.stderr
    assert registry_guard.count == 0, registry_guard.requests

    result = second_home.run("package", "install", absent.short, check=False)

    assert result.returncode != 0
    assert registry_guard.count > 0, "the guard saw no request, so a zero elsewhere proves nothing"
